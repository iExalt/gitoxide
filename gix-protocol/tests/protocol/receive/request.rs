use std::io::Read;

use gix_error::Class;
use gix_hash::{Kind, ObjectId};
use gix_protocol::receive::{Capabilities, RawCommand, Request, read_request, request::Parser};
use gix_testtools::TestResult;

use super::{FLUSH, capabilities, empty_pack, hex_of, pkt, read_only, receive_pack, writable};

fn caps(kind: Kind) -> Capabilities {
    Capabilities::new(kind, "git/2.56.0")
}

fn parse(kind: Kind, bytes: &[u8]) -> gix_error::Result<Request> {
    read_request(&mut &*bytes, &caps(kind))
}

fn id(kind: Kind, digit: char) -> ObjectId {
    ObjectId::from_hex(hex_of(kind, digit).as_bytes()).expect("valid hex")
}

/// `line` with a feature list, which is separated from it by a NUL byte.
fn with_features(line: &str, features: &str) -> Vec<u8> {
    pkt(format!("{line}\0{features}\n"))
}

fn command_line(kind: Kind, name: &str) -> String {
    format!("{} {} {name}", hex_of(kind, '0'), hex_of(kind, '1'))
}

#[test]
fn commands_features_and_shallow_boundary_are_parsed() -> TestResult {
    for kind in Kind::all().iter().copied() {
        let mut bytes = pkt(format!("shallow {}\n", hex_of(kind, '2')));
        bytes.extend(with_features(
            &command_line(kind, "refs/heads/a"),
            &format!("report-status report-status-v2 side-band-64k quiet atomic push-options object-format={kind} agent=x session-id=abc"),
        ));
        let mut caps = caps(kind);
        caps.push_options = true;
        let mut input = bytes;
        input.extend(pkt(format!(
            "{} {} refs/heads/b\n",
            hex_of(kind, '3'),
            hex_of(kind, '0')
        )));
        input.extend(FLUSH);
        input.extend(pkt("option one\n"));
        input.extend(pkt("option two"));
        input.extend(FLUSH);
        let mut reader = &input[..];
        let request = read_request(&mut reader, &caps)?;
        assert_eq!(
            request.commands,
            vec![
                RawCommand {
                    old_id: id(kind, '0'),
                    new_id: id(kind, '1'),
                    ref_name: "refs/heads/a".into()
                },
                RawCommand {
                    old_id: id(kind, '3'),
                    new_id: id(kind, '0'),
                    ref_name: "refs/heads/b".into()
                }
            ]
        );
        assert!(request.commands[1].is_delete());
        assert_eq!(request.shallow, vec![id(kind, '2')]);
        assert_eq!(
            request.push_options,
            ["option one", "option two"],
            "newlines are chomped like Git does"
        );
        let features = &request.features;
        assert!(
            features.report_status && features.report_status_v2 && features.side_band_64k && features.quiet,
            "{features:?}"
        );
        assert!(features.atomic && features.push_options, "offered, and asked for");
        assert_eq!(features.session_id.as_ref().map(|id| id.as_slice()), Some(&b"abc"[..]));
        assert_eq!(
            features.status_format(),
            Some(gix_protocol::push::StatusFormat::V2),
            "v2 wins like in Git"
        );
        assert!(reader.is_empty(), "everything was consumed");
    }
    Ok(())
}

#[test]
fn features_that_were_not_offered_are_ignored_and_names_are_not_validated() -> TestResult {
    for kind in Kind::all().iter().copied() {
        let object_format = format!("object-format={kind}");
        let mut input = with_features(
            &command_line(kind, "HEAD"),
            &format!("report-status atomic push-options {object_format} unknown"),
        );
        input.extend(pkt(command_line(kind, "")));
        input.extend(pkt(command_line(kind, "refs/heads/with space and \u{e4}")));
        input.extend(FLUSH);
        let mut not_offered = caps(kind);
        not_offered.atomic = false;
        let request = read_request(&mut &*input, &not_offered)?;
        assert!(
            !request.features.atomic && !request.features.push_options,
            "{kind}: `receive.advertiseAtomic` and `receive.advertisePushOptions` decide: {:?}",
            request.features
        );
        assert!(request.features.report_status && !request.features.report_status_v2);
        assert_eq!(
            request
                .commands
                .iter()
                .map(|c| c.ref_name.to_string())
                .collect::<Vec<_>>(),
            ["HEAD", "", "refs/heads/with space and \u{e4}"],
            "{kind}: the server answers for each command, which isn't possible if parsing refuses them"
        );
    }
    Ok(())
}

#[test]
fn feature_matching_is_like_gits_with_its_quirks() -> TestResult {
    let kind = Kind::Sha1;
    for (features, report_status, quiet) in [
        ("report-status quiet", true, true),
        ("xreport-status", false, false),
        ("report-status-v2x quiet=1", false, true),
        ("report-status=value\tquiet", true, true),
        ("report-status\x0bquiet", false, false),
        ("quiet\x0creport-status", false, false),
        ("report-statuss  quiet", false, true),
    ] {
        let mut input = with_features(&command_line(kind, "refs/heads/a"), features);
        input.extend(FLUSH);
        let features_parsed = parse(kind, &input)?.features;
        assert_eq!(
            (features_parsed.report_status, features_parsed.quiet),
            (report_status, quiet),
            "{features:?}"
        );
    }
    Ok(())
}

#[test]
fn ids_may_use_uppercase_hex_and_shallow_lines_ignore_trailing_bytes_and_features() -> TestResult {
    for kind in Kind::all().iter().copied() {
        let upper = hex_of(kind, 'A');
        let mut input = pkt(format!("{upper} {upper} refs/heads/a\n"));
        input.extend(pkt(format!(
            "shallow {}junk\0object-format=nonsense\n",
            hex_of(kind, 'b')
        )));
        input.extend(FLUSH);
        let request = parse(kind, &input)?;
        assert_eq!(
            request.commands[0].old_id,
            id(kind, 'a'),
            "{kind}: normalized to lowercase"
        );
        assert_eq!(request.shallow, vec![id(kind, 'b')], "{kind}");
    }
    Ok(())
}

#[test]
fn the_object_format_is_checked_on_every_line_with_features() -> TestResult {
    let kind = Kind::Sha1;
    // A second feature list without a format asks for SHA-1, which the repository uses.
    let mut input = with_features(&command_line(kind, "refs/heads/a"), "report-status");
    input.extend(with_features(&command_line(kind, "refs/heads/b"), "quiet"));
    input.extend(FLUSH);
    assert_eq!(parse(kind, &input)?.commands.len(), 2);

    #[cfg(feature = "sha256")]
    {
        let kind = Kind::Sha256;
        let mut input = with_features(&command_line(kind, "refs/heads/a"), "object-format=sha256");
        input.extend(with_features(&command_line(kind, "refs/heads/b"), "quiet"));
        input.extend(FLUSH);
        assert!(
            parse(kind, &input).is_err(),
            "a feature list without object format means SHA-1, even if an earlier one said otherwise"
        );
        let input = [pkt(command_line(kind, "refs/heads/a")), FLUSH.to_vec()].concat();
        assert!(
            parse(kind, &input).is_ok(),
            "lines without features aren't checked, so SHA-1 isn't assumed"
        );
    }
    Ok(())
}

#[test]
fn any_packet_that_isnt_data_ends_the_commands_and_the_pack_follows() -> TestResult {
    let kind = Kind::Sha1;
    for end in ["0000", "0001", "0002"] {
        let mut input = pkt(command_line(kind, "refs/heads/a"));
        input.extend(end.as_bytes());
        input.extend(b"PACK....");
        let mut reader = &input[..];
        let request = read_request(&mut reader, &caps(kind))?;
        assert_eq!(request.commands.len(), 1, "{end}");
        assert_eq!(
            reader, b"PACK....",
            "{end}: the reader is at the first byte of the pack"
        );
        assert!(request.expects_pack());
    }
    Ok(())
}

#[test]
fn push_options_are_read_only_if_negotiated() -> TestResult {
    let kind = Kind::Sha1;
    let mut input = with_features(&command_line(kind, "refs/heads/a"), "report-status push-options");
    input.extend(FLUSH);
    input.extend(pkt("a=b\n"));
    input.extend(FLUSH);
    input.extend(b"PACK");
    let mut without = caps(kind);
    let mut reader = &input[..];
    let request = read_request(&mut reader, &without)?;
    assert!(request.push_options.is_empty() && !request.features.push_options);
    assert_eq!(
        reader.len(),
        8 + 4 + 4,
        "the options weren't offered, so they are part of the pack as far as we know"
    );

    without.push_options = true;
    let mut reader = &input[..];
    let request = read_request(&mut reader, &without)?;
    assert_eq!(request.push_options, ["a=b"]);
    assert_eq!(reader, b"PACK");
    Ok(())
}

#[test]
fn a_request_without_commands_is_not_an_error() -> TestResult {
    let request = parse(Kind::Sha1, FLUSH)?;
    assert!(request.commands.is_empty() && request.shallow.is_empty());
    Ok(())
}

#[test]
fn push_certificates_are_refused_as_unsupported() {
    let input = [
        pkt("push-cert\0report-status\n"),
        pkt("certificate version 0.1\n"),
        pkt("push-cert-end\n"),
    ]
    .concat();
    let err = parse(Kind::Sha1, &input).expect_err("not supported yet");
    assert!(
        err.classify().any(|class| class.class() == Class::Unsupported),
        "so that callers can tell apart from malformed requests: {err:?}"
    );
}

#[test]
fn lines_are_decoded_like_git_does() -> TestResult {
    let kind = Kind::Sha1;
    // The largest line Git accepts holds 65519 bytes, here a reference name making up for the rest of the line.
    let name = "r".repeat(65519 - (2 * kind.len_in_hex() + 2));
    let mut input = pkt(command_line(kind, &name));
    input.extend(FLUSH);
    assert_eq!(parse(kind, &input)?.commands[0].ref_name.len(), name.len());
    Ok(())
}

#[test]
fn the_parser_can_be_fed_line_by_line() -> TestResult {
    use gix_packetline::PacketLineRef::{Data, Flush};
    let kind = Kind::Sha1;
    let mut parser = Parser::new(&caps(kind));
    let line = command_line(kind, "refs/heads/a");
    assert_eq!(
        parser.feed(Data(line.as_bytes()))?,
        gix_protocol::receive::request::Step::More
    );
    assert_eq!(parser.feed(Flush)?, gix_protocol::receive::request::Step::Done);
    assert_eq!(parser.finish().commands.len(), 1);
    Ok(())
}

/// Requests that Git rejects as a protocol error and that we have to reject the same way, as `(name, request)`.
fn malformed(kind: Kind) -> Vec<(&'static str, Vec<u8>)> {
    let (zero, one) = (hex_of(kind, '0'), hex_of(kind, '1'));
    let other_format = if kind == Kind::Sha1 { "sha256" } else { "sha1" };
    let mut cases = vec![
        ("a line that is too short", pkt("abc\n")),
        ("an empty data packet", b"0004".to_vec()),
        ("no space after the old id", pkt(format!("{zero}{one} refs/heads/a\n"))),
        ("no ref after the new id", pkt(format!("{zero} {one}\n"))),
        (
            "an id that isn't hex",
            pkt(format!("{} {one} refs/heads/a\n", "z".repeat(kind.len_in_hex()))),
        ),
        (
            "ids of the wrong length",
            pkt(format!(
                "{} {} refs/heads/a\n",
                &zero[..zero.len() - 2],
                &one[..one.len() - 2]
            )),
        ),
        ("a shallow line without an id", pkt("shallow nothex\n")),
        ("an error packet", pkt("ERR boom")),
        ("an error packet with a newline", pkt("ERR boom\n")),
        ("an error packet that ends at a NUL byte", pkt("ERR a\0b\n")),
        (
            "an object format without a value",
            with_features(&format!("{zero} {one} refs/heads/a"), "object-format agent=x"),
        ),
        (
            "a bad command after a shallow line with trailing bytes",
            [pkt(format!("shallow {}junk\n", hex_of(kind, 'b'))), pkt("abc\n")].concat(),
        ),
        (
            "an object format that isn't the repository's",
            with_features(
                &format!("{zero} {one} refs/heads/a"),
                &format!("report-status object-format={other_format} agent=x"),
            ),
        ),
        ("a bad character in the length", b"zzzz".to_vec()),
        ("a length of 3", b"0003".to_vec()),
        ("a length beyond what Git buffers", b"fff4".to_vec()),
        ("end of input right away", Vec::new()),
        ("end of input in the length", b"00".to_vec()),
        ("end of input in a line", b"0010abc".to_vec()),
        (
            "end of input before the flush packet",
            pkt(format!("{zero} {one} refs/heads/a\n")),
        ),
    ];
    if kind != Kind::Sha1 {
        cases.push((
            "a second feature list that asks for SHA-1 by default",
            [
                with_features(&format!("{zero} {one} refs/heads/a"), "object-format=sha256"),
                with_features(&format!("{zero} {one} refs/heads/b"), "quiet"),
            ]
            .concat(),
        ));
    }
    cases
}

#[test]
fn malformed_requests_fail_with_the_messages_of_git() -> TestResult {
    for f in read_only()? {
        for (name, bytes) in malformed(f.kind) {
            let git = receive_pack(&f.remote(), &[], None, &bytes)?;
            assert!(!git.success, "{} {name}: Git dies with a protocol error", f.kind);
            let err = parse(f.kind, &bytes).expect_err(name);
            let message = err.to_string();
            assert_eq!(
                git.stderr,
                format!("fatal: {message}\n"),
                "{} {name}: Git says exactly what we do",
                f.kind
            );
            assert!(
                err.classify().any(|class| class.class() == Class::Validation),
                "{} {name}: callers can tell malformed input from I/O failures: {err:?}",
                f.kind
            );
        }
    }
    Ok(())
}

#[test]
fn a_request_without_commands_makes_git_write_nothing_more() -> TestResult {
    for f in read_only()? {
        let git = receive_pack(&f.remote(), &[], None, FLUSH)?;
        assert!(git.success, "{}: {}", f.kind, git.stderr);
        let advertisement = receive_pack(&f.remote(), &[], None, b"")?.stdout;
        assert_eq!(
            git.stdout, advertisement,
            "{}: neither a report nor a trailing flush follow, as no side band was negotiated",
            f.kind
        );
        assert!(parse(f.kind, FLUSH)?.commands.is_empty());
    }
    Ok(())
}

#[test]
fn git_accepts_what_we_parse_that_isnt_a_valid_reference() -> TestResult {
    for f in writable()? {
        let kind = f.kind;
        let remote = f.remote();
        let two = f.rev(&remote, "main")?;
        let upper = two.to_string().to_uppercase();
        let mut input = with_features(
            &format!("{} {upper} refs/heads/upper", hex_of(kind, '0')),
            &format!("report-status quiet object-format={kind}"),
        );
        input.extend(pkt(format!("{} {} refs/heads/funny..name", hex_of(kind, '0'), two)));
        input.extend(FLUSH);
        let request = parse(kind, &input)?;
        assert_eq!(request.commands[0].new_id, two, "{kind}: parsed like Git does");
        input.extend(empty_pack(&remote)?);
        let git = receive_pack(&remote, &[], None, &input)?;
        let stdout = String::from_utf8_lossy(&git.stdout);
        assert!(
            stdout.contains("ok refs/heads/upper") && stdout.contains("ng refs/heads/funny..name funny refname"),
            "{kind}: Git takes an uppercase id and answers for a bad name per command: {stdout}\n{}",
            git.stderr
        );
    }
    Ok(())
}

#[test]
fn what_git_advertises_is_what_the_parser_expects() -> TestResult {
    // The capabilities of Git's advertisement decide what the parser accepts, which matters for atomic and push-options.
    for f in read_only()? {
        let config = ["receive.advertisePushOptions=true"];
        let advertisement = receive_pack(&f.remote(), &config, None, b"")?.stdout;
        let caps = capabilities(f.kind, &advertisement, &config);
        let mut input = with_features(
            &command_line(f.kind, "refs/heads/a"),
            &format!("atomic push-options object-format={}", f.kind),
        );
        input.extend(FLUSH);
        input.extend(FLUSH);
        let mut reader = &input[..];
        let request = read_request(&mut reader, &caps)?;
        assert!(request.features.atomic && request.features.push_options);
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest)?;
        assert!(rest.is_empty());
    }
    Ok(())
}

/// What `git push` sent to `receive-pack`, recorded by a program that stands in for it and tees its input to a file.
struct Recording {
    bytes: Vec<u8>,
}

/// Run `git push` in `client` with `options` and `refspecs` to `remote`, and record what it sent.
fn record_push(
    client: &std::path::Path,
    remote: &std::path::Path,
    options: &[&str],
    refspecs: &[&str],
) -> TestResult<Recording> {
    let dir = gix_testtools::tempfile::tempdir()?;
    let record = dir.path().join("request");
    let shim = dir.path().join("recording-receive-pack");
    std::fs::write(
        &shim,
        "#!/bin/sh\ntee \"$RECORD\" | git -c receive.advertisePushOptions=true receive-pack \"$@\"\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))?;
    }
    let output = gix_testtools::git_command(client)
        .env("RECORD", &record)
        .arg("push")
        .arg(format!("--receive-pack={}", shim.display()))
        .args(options)
        .arg(std::fs::canonicalize(remote)?)
        .args(refspecs)
        .output()?;
    // The server may refuse what is pushed, which doesn't matter as it is the request that is of interest.
    let bytes = std::fs::read(&record).map_err(|err| {
        format!(
            "git push didn't reach receive-pack: {err}\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    Ok(Recording { bytes })
}

#[test]
fn requests_of_git_push_are_parsed_and_leave_the_reader_at_the_pack() -> TestResult {
    for f in writable()? {
        let kind = f.kind;
        let (source, remote) = (f.root.join("source"), f.remote());
        let other = f.rev(&source, "other")?;
        let null = kind.null();
        let mut caps = caps(kind);
        caps.push_options = true;

        // Creating a reference sends objects.
        let recording = record_push(&source, &remote, &[], &["refs/heads/other"])?;
        let mut reader = &recording.bytes[..];
        let request = read_request(&mut reader, &caps)?;
        assert_eq!(
            request.commands,
            [RawCommand {
                old_id: null,
                new_id: other,
                ref_name: "refs/heads/other".into()
            }],
            "{kind}"
        );
        assert!(
            request.features.report_status_v2 && request.features.side_band_64k && !request.features.atomic,
            "{kind}: {:?}",
            request.features
        );
        assert!(
            request.expects_pack() && reader.starts_with(b"PACK"),
            "{kind}: the pack follows the request"
        );

        // Atomic pushes with options and a deletion.
        let recording = record_push(
            &source,
            &remote,
            &["--atomic", "-o", "one", "-o", "two words"],
            &["refs/heads/other:refs/heads/more", ":refs/tags/victim"],
        )?;
        let mut reader = &recording.bytes[..];
        let request = read_request(&mut reader, &caps)?;
        let victim = f.rev(&source, "victim")?;
        let mut commands = request
            .commands
            .iter()
            .map(|c| (c.ref_name.to_string(), c.old_id, c.new_id))
            .collect::<Vec<_>>();
        commands.sort();
        assert_eq!(
            commands,
            [
                ("refs/heads/more".to_string(), null, other),
                ("refs/tags/victim".to_string(), victim, null)
            ],
            "{kind}"
        );
        assert!(
            request.features.atomic && request.features.push_options,
            "{kind}: {:?}",
            request.features
        );
        assert_eq!(request.push_options, ["one", "two words"], "{kind}");
        assert!(
            reader.starts_with(b"PACK"),
            "{kind}: {:?}",
            &reader[..reader.len().min(16)]
        );

        // Deleting needs no pack.
        let recording = record_push(&source, &remote, &[], &[":refs/heads/main"])?;
        let mut reader = &recording.bytes[..];
        let request = read_request(&mut reader, &caps)?;
        assert!(
            !request.expects_pack() && reader.is_empty(),
            "{kind}: the request is all there is"
        );

        // Pushing from a shallow clone tells the server about its boundary.
        let client = f.root.join("shallow-client");
        let recording = record_push(&client, &remote, &[], &["HEAD:refs/heads/from-shallow"])?;
        let request = read_request(&mut &recording.bytes[..], &caps)?;
        assert_eq!(request.shallow, super::shallow(&client)?, "{kind}");
        assert!(!request.shallow.is_empty());
    }
    Ok(())
}
