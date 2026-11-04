use bstr::BString;
use gix_hash::Kind;
use gix_protocol::{
    push::{
        Options, StatusFormat,
        report::{Status, Update},
    },
    receive::{Report, report::CommandReport, write_report},
};
use gix_testtools::TestResult;

use super::{FLUSH, Fixture, empty_pack, hex_of, pkt, receive_pack, split_at_flush, writable};

/// The bytes a client sends: `commands` as `(name, old, new)` hex ids, then `pack`.
fn request(kind: Kind, features: &str, commands: &[(&str, String, String)], pack: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, (name, old, new)) in commands.iter().enumerate() {
        out.extend(if index == 0 {
            pkt(format!("{old} {new} {name}\0{features} quiet object-format={kind}\n"))
        } else {
            pkt(format!("{old} {new} {name}\n"))
        });
    }
    out.extend(FLUSH);
    out.extend(pack);
    out
}

/// What a server said in `response`, decoded into a report, along with the packets of band 1 if `side_band` is set.
/// Other bands carry what the processes Git runs have to say, like why a pack is unusable.
fn decode_report(response: &[u8], side_band: bool, format: StatusFormat) -> (Report, Vec<u8>) {
    let mut report_bytes = Vec::new();
    let mut band_packets = Vec::new();
    if side_band {
        let mut rest = response;
        while rest != FLUSH {
            let len = usize::from_str_radix(std::str::from_utf8(&rest[..4]).expect("hex"), 16).expect("hex");
            if rest[4] == 1 {
                report_bytes.extend_from_slice(&rest[5..len]);
                band_packets.extend_from_slice(&rest[..len]);
            }
            rest = &rest[len..];
        }
    } else {
        report_bytes.extend_from_slice(response);
    }
    let (lines, rest) = split_at_flush(&report_bytes);
    assert!(rest.is_empty());
    let mut report = Report {
        unpack: None,
        commands: Vec::new(),
    };
    for line in lines {
        let line = String::from_utf8(line.to_vec()).expect("utf8");
        let line = line.strip_suffix('\n').expect("newline");
        if let Some(status) = line.strip_prefix("unpack ") {
            report.unpack = (status != "ok").then(|| status.into());
        } else if let Some(name) = line.strip_prefix("ok ") {
            let outcome = match report.commands.last_mut() {
                Some(CommandReport {
                    ref_name,
                    outcome: Ok(updates),
                }) if format == StatusFormat::V2 && ref_name == name => {
                    updates.push(Update::default());
                    continue;
                }
                _ => Ok(if format == StatusFormat::V2 {
                    vec![Update::default()]
                } else {
                    Vec::new()
                }),
            };
            report.commands.push(CommandReport {
                ref_name: name.into(),
                outcome,
            });
        } else if let Some(ng) = line.strip_prefix("ng ") {
            let (name, reason) = ng.split_once(' ').expect("a reason");
            report.commands.push(CommandReport {
                ref_name: name.into(),
                outcome: Err(reason.into()),
            });
        } else {
            panic!("unexpected line {line:?}, `option` lines need a proc-receive hook which isn't used here");
        }
    }
    (report, band_packets)
}

fn scenario(f: &Fixture) -> TestResult<Vec<(&'static str, String, String)>> {
    let (kind, remote) = (f.kind, f.remote());
    let one = f.rev(&remote, "refs/tags/victim")?.to_string();
    let two = f.rev(&remote, "main")?.to_string();
    let null = hex_of(kind, '0');
    Ok(vec![
        ("refs/heads/copy", null.clone(), two.clone()),
        ("refs/heads/main", two.clone(), null.clone()),
        ("refs/heads/bad..name", null.clone(), two.clone()),
        ("refs/heads/stale", one, two),
        ("refs/heads/missing", null.clone(), hex_of(kind, 'f')),
    ])
}

#[test]
fn reports_are_the_bytes_git_writes() -> TestResult {
    for f in writable()? {
        let kind = f.kind;
        let remote = f.remote();
        let commands = scenario(&f)?;
        let empty = empty_pack(&remote)?;
        let garbage = [&b"PACK\0\0\0\x02\0\0\0\x01"[..], b"definitely not an object"].concat();
        for (pack_name, pack) in [
            ("a valid pack without objects", &empty),
            ("a pack that can't be unpacked", &garbage),
        ] {
            for (format, feature) in [
                (StatusFormat::V1, "report-status"),
                (StatusFormat::V2, "report-status-v2"),
            ] {
                for side_band in [false, true] {
                    let features = if side_band {
                        format!("{feature} side-band-64k")
                    } else {
                        feature.into()
                    };
                    let input = request(kind, &features, &commands, pack);
                    // Later runs see what earlier ones did, which only changes what Git has to say, not how it is encoded.
                    let git = receive_pack(
                        &remote,
                        &["receive.denyDeletes=true", "receive.autogc=false"],
                        None,
                        &input,
                    )?;
                    let (_advertisement, response) = split_at_flush(&git.stdout);
                    if side_band {
                        assert!(
                            response.ends_with(FLUSH),
                            "{kind}: with side bands, a flush packet ends the response"
                        );
                    }
                    let (report, band_packets) = decode_report(response, side_band, format);
                    assert!(
                        report.commands.iter().any(|command| command.outcome.is_err()),
                        "{kind} {pack_name}: the scenario has rejections, which Git words: {:?}\n{}",
                        report,
                        git.stderr
                    );
                    let mut ours = Vec::new();
                    write_report(&mut ours, &report, format, side_band)?;
                    let expected = if side_band { band_packets } else { response.to_vec() };
                    assert_eq!(
                        BString::from(ours),
                        BString::from(expected),
                        "{kind} {pack_name} {format:?} side-band={side_band}: our bytes are those of Git"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn v2_options_of_redirected_updates_are_understood_by_the_client() -> TestResult {
    let kind = Kind::Sha1;
    let id = |digit| gix_hash::ObjectId::from_hex(hex_of(kind, digit).as_bytes()).expect("valid");
    let report = Report {
        unpack: None,
        commands: vec![
            CommandReport {
                ref_name: "refs/heads/a".into(),
                outcome: Ok(vec![
                    Update {
                        ref_name: Some("refs/for/a".into()),
                        old_id: Some(id('1')),
                        new_id: Some(id('2')),
                        forced: true,
                    },
                    Update::default(),
                ]),
            },
            CommandReport {
                ref_name: "refs/heads/b".into(),
                outcome: Err("hook declined".into()),
            },
        ],
    };
    let bytes = report.encode(StatusFormat::V2)?;
    let (lines, rest) = split_at_flush(&bytes);
    assert!(rest.is_empty());
    let client = crate::push::request(
        kind,
        &[("refs/heads/a", 0, 1), ("refs/heads/b", 0, 2)],
        Options {
            status_format: StatusFormat::V2,
            ..Default::default()
        },
    );
    let parsed = client.parse_report(lines)?;
    assert_eq!(
        parsed.commands[0].status,
        Status::Accepted {
            updates: vec![
                Update {
                    ref_name: Some("refs/for/a".into()),
                    old_id: Some(id('1')),
                    new_id: Some(id('2')),
                    forced: true
                },
                Update::default()
            ]
        }
    );
    assert_eq!(
        parsed.commands[1].status,
        Status::Rejected {
            reason: "hook declined".into()
        }
    );
    // V1 doesn't carry them.
    let v1 = report.encode(StatusFormat::V1)?;
    assert!(
        !String::from_utf8_lossy(&v1).contains("option"),
        "report-status can't represent updates"
    );
    Ok(())
}

#[test]
fn an_unpack_failure_and_overlong_lines_are_reported_or_refused() -> TestResult {
    let report = Report {
        unpack: Some("unpack-objects abnormal exit".into()),
        commands: vec![CommandReport {
            ref_name: "refs/heads/a".into(),
            outcome: Err("unpacker error".into()),
        }],
    };
    assert_eq!(
        String::from_utf8(report.encode(StatusFormat::V1)?)?,
        "0028unpack unpack-objects abnormal exit\n0023ng refs/heads/a unpacker error\n0000"
    );
    let too_long = Report {
        unpack: None,
        commands: vec![CommandReport {
            ref_name: "r".repeat(65520).into(),
            outcome: Ok(Vec::new()),
        }],
    };
    assert!(
        too_long.encode(StatusFormat::V1).is_err(),
        "Git dies instead of writing a truncated line"
    );
    Ok(())
}
