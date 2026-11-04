use bstr::ByteSlice;
use gix_error::Class;
use gix_hash::Kind;
use gix_protocol::push::{Options, Request, StatusFormat};

use super::{classes, commands, request, server, server_with};

fn pkt(line: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", line.len() + 4).into_bytes();
    out.extend_from_slice(line);
    out
}

const OLD: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";

#[test]
fn header_matches_the_bytes_git_sends_for_a_single_update() {
    let options = Options {
        agent: Some("git/2.56.0-gitoxide".into()),
        ..Default::default()
    };
    let request = Request::new(
        commands(Kind::Sha1, &[("refs/heads/main", 0x11, 0x22)]),
        Kind::Sha1,
        &server(Kind::Sha1, ""),
        options,
    )
    .expect("valid request");
    let line = format!(
        "{OLD} {NEW} refs/heads/main\0report-status side-band-64k object-format=sha1 agent=git/2.56.0-gitoxide"
    );
    let expected = format!("{:04x}{line}0000", line.len() + 4);
    assert_eq!(
        request.encode_header().as_bstr(),
        expected.as_bytes().as_bstr(),
        "one command line carrying the capabilities, then a flush, and no newline as Git sends it"
    );
    assert!(request.expects_pack() && request.uses_side_band() && request.ofs_delta_allowed());
}

#[test]
fn capabilities_follow_the_choices_and_are_only_announced_if_offered() {
    for kind in Kind::all().iter().copied() {
        let request = request(
            kind,
            &[("refs/heads/a", 0, 1), ("refs/heads/b", 1, 2)],
            Options {
                status_format: StatusFormat::V2,
                atomic: true,
                push_options: vec!["topic=x".into(), "ci.skip".into()],
                quiet: true,
                agent: Some("agent/1".into()),
            },
        );
        assert_eq!(
            request.capabilities(),
            format!("report-status-v2 side-band-64k quiet atomic push-options object-format={kind} agent=agent/1"),
            "{kind}: everything requested and offered is announced, in a stable order"
        );
        let lines = request.command_lines();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains(&0) && !lines[1].contains(&0),
            "{kind}: only the first command carries capabilities"
        );
        assert_eq!(
            request.push_option_lines(),
            ["topic=x", "ci.skip"],
            "{kind}: options are sent verbatim"
        );
        let mut expected = Vec::new();
        for line in &lines {
            expected.extend(pkt(line));
        }
        expected.extend(b"0000");
        expected.extend(pkt(b"topic=x"));
        expected.extend(pkt(b"ci.skip"));
        expected.extend(b"0000");
        assert_eq!(
            request.encode_header().as_bstr(),
            expected.as_bstr(),
            "{kind}: one flush ends the commands, another ends the push options"
        );
    }

    let kind = Kind::all()[0];
    let unquiet = Request::new(
        commands(kind, &[("refs/heads/a", 0, 1)]),
        kind,
        &server_with(&format!("report-status object-format={kind}")),
        Options {
            quiet: true,
            ..Default::default()
        },
    )
    .expect("valid request");
    assert_eq!(
        unquiet.capabilities(),
        format!("report-status object-format={kind}"),
        "quiet and side-band-64k are optional, so they are left out when the server doesn't offer them"
    );
    assert!(!unquiet.uses_side_band() && !unquiet.ofs_delta_allowed());
}

#[test]
fn a_pack_is_expected_unless_every_command_deletes() {
    for kind in Kind::all().iter().copied() {
        let deletes = request(
            kind,
            &[("refs/heads/a", 1, 0), ("refs/tags/b", 2, 0)],
            Options::default(),
        );
        assert!(!deletes.expects_pack(), "{kind}: only deletions need no pack");
        let mixed = request(
            kind,
            &[("refs/heads/a", 1, 0), ("refs/tags/b", 0, 2)],
            Options::default(),
        );
        assert!(
            mixed.expects_pack(),
            "{kind}: a single non-delete needs a pack, possibly without objects"
        );
    }
}

#[test]
fn requests_that_the_server_cannot_honor_are_refused_before_anything_is_sent() {
    let kind = Kind::all()[0];
    let ok = || commands(kind, &[("refs/heads/a", 1, 2)]);
    let with_options = |options: Options| options;
    type Case = (&'static str, gix_error::Result<Request>, Class);
    let mut cases: Vec<Case> = vec![
        (
            "atomic without the capability",
            Request::new(
                ok(),
                kind,
                &server_with(&format!("report-status object-format={kind}")),
                with_options(Options {
                    atomic: true,
                    ..Default::default()
                }),
            ),
            Class::Unsupported,
        ),
        (
            "push options without the capability",
            Request::new(
                ok(),
                kind,
                &server(kind, ""),
                Options {
                    push_options: vec!["a".into()],
                    ..Default::default()
                },
            ),
            Class::Unsupported,
        ),
        (
            "delete without delete-refs",
            Request::new(
                commands(kind, &[("refs/heads/a", 1, 0)]),
                kind,
                &server_with(&format!("report-status object-format={kind}")),
                Options::default(),
            ),
            Class::Unsupported,
        ),
        (
            "no status reporting at all",
            Request::new(
                ok(),
                kind,
                &server_with(&format!("delete-refs object-format={kind}")),
                Options::default(),
            ),
            Class::Unsupported,
        ),
        (
            "v2 reports without the capability",
            Request::new(
                ok(),
                kind,
                &server_with(&format!("report-status object-format={kind}")),
                Options {
                    status_format: StatusFormat::V2,
                    ..Default::default()
                },
            ),
            Class::Unsupported,
        ),
        (
            "remote object format differs",
            Request::new(
                ok(),
                kind,
                &server_with("report-status object-format=sha512"),
                Options::default(),
            ),
            Class::Unsupported,
        ),
        (
            "remote object format differs, with the name spelled out",
            Request::new(
                commands(Kind::Sha1, &[("refs/heads/a", 1, 2)]),
                Kind::Sha1,
                &server_with("report-status object-format=sha256"),
                Options::default(),
            ),
            Class::Unsupported,
        ),
        (
            "no commands",
            Request::new(Vec::new(), kind, &server(kind, ""), Options::default()),
            Class::Validation,
        ),
        (
            "the same ref twice",
            Request::new(
                commands(kind, &[("refs/heads/a", 1, 2), ("refs/heads/a", 2, 3)]),
                kind,
                &server(kind, ""),
                Options::default(),
            ),
            Class::Validation,
        ),
        (
            "option with a newline",
            Request::new(
                ok(),
                kind,
                &server(kind, "push-options"),
                Options {
                    push_options: vec!["a\nb".into()],
                    ..Default::default()
                },
            ),
            Class::Validation,
        ),
        (
            "empty option",
            Request::new(
                ok(),
                kind,
                &server(kind, "push-options"),
                Options {
                    push_options: vec!["".into()],
                    ..Default::default()
                },
            ),
            Class::Validation,
        ),
        (
            "agent with a space",
            Request::new(
                ok(),
                kind,
                &server(kind, ""),
                Options {
                    agent: Some("a b".into()),
                    ..Default::default()
                },
            ),
            Class::Validation,
        ),
        (
            "line beyond the packet line limit",
            Request::new(
                commands(kind, &[(&format!("refs/heads/{}", "a".repeat(70_000)), 1, 2)]),
                kind,
                &server(kind, ""),
                Options::default(),
            ),
            Class::Validation,
        ),
    ];
    if let Some(other) = Kind::all().iter().copied().find(|other| *other != kind) {
        cases.push((
            "commands for another hash than the repository uses",
            Request::new(ok(), other, &server(other, ""), Options::default()),
            Class::Validation,
        ));
    }
    for (name, result, class) in cases {
        let err = result.expect_err(name);
        assert_eq!(classes(&err), [class], "{name}: {err:#}");
    }
}

#[test]
fn an_unannounced_object_format_is_limited_to_sha1() {
    for kind in Kind::all().iter().copied() {
        let result = Request::new(
            commands(kind, &[("refs/heads/a", 1, 2)]),
            kind,
            &server_with("report-status ofs-delta"),
            Options::default(),
        );
        if kind == Kind::Sha1 {
            let request = result.expect("old servers are sha1-only, which matches");
            assert_eq!(
                request.capabilities(),
                "report-status",
                "no object-format is announced to a server that doesn't know it"
            );
        } else {
            let err = result.expect_err("a server without object-format can't be sha256");
            assert_eq!(classes(&err), [Class::Unsupported], "{kind}: {err:#}");
        }
    }
}

#[test]
fn server_capabilities_are_extracted_from_an_advertisement() {
    let server = server_with(
        "report-status report-status-v2 delete-refs side-band-64k quiet atomic ofs-delta push-options session-id=abc push-cert=nonce object-format=sha256 agent=git/2.56.0 unknown-thing",
    );
    assert!(
        server.report_status
            && server.report_status_v2
            && server.delete_refs
            && server.side_band_64k
            && server.quiet
            && server.atomic
            && server.ofs_delta
            && server.push_options
            && server.session_id
            && server.push_cert,
        "{server:?}"
    );
    assert_eq!(
        server.object_format.as_ref().map(|v| v.as_bstr()),
        Some(b"sha256".as_bstr())
    );
    assert_eq!(
        server.agent.as_ref().map(|v| v.as_bstr()),
        Some(b"git/2.56.0".as_bstr())
    );

    let bare = server_with("report-status");
    assert!(
        !bare.atomic && !bare.delete_refs && bare.object_format.is_none() && bare.agent.is_none(),
        "everything not mentioned is not offered"
    );
}
