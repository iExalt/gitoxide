use bstr::ByteSlice;
use gix_hash::Kind;
use gix_protocol::upload::{Capabilities, Command, Fetch, LsRefs, read_request};
use gix_testtools::TestResult;

use super::{FLUSH, hex_of, pkt, read_only, upload_pack};

fn parse(kind: Kind, bytes: &[u8]) -> gix_error::Result<Option<gix_protocol::upload::Request>> {
    let mut bytes = bytes;
    read_request(&mut bytes, &Capabilities::new(kind, "agent"))
}

fn request(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

const DELIM: &[u8] = b"0001";

#[test]
fn what_git_rejects_while_reading_a_request_is_rejected_with_its_message() -> TestResult {
    let other = |kind: Kind| if kind == Kind::Sha1 { "sha256" } else { "sha1" };
    for f in read_only()? {
        let dir = f.remote();
        let mut cases: Vec<(&str, Vec<u8>, String)> = vec![
            (
                "an unknown command",
                request(&[&pkt("command=ls-reps\n"), FLUSH]),
                "invalid command 'ls-reps'".into(),
            ),
            (
                "a command with a value",
                request(&[&pkt("command=ls-refs=1\n"), FLUSH]),
                "invalid command 'ls-refs=1'".into(),
            ),
            (
                "two commands",
                request(&[&pkt("command=ls-refs\n"), &pkt("command=fetch\n"), FLUSH]),
                "command 'fetch' requested after already requesting command 'ls-refs'".into(),
            ),
            (
                "an unknown capability",
                request(&[&pkt("command=ls-refs\n"), &pkt("bogus\n"), FLUSH]),
                "unknown capability 'bogus'".into(),
            ),
            (
                "a command as a capability",
                request(&[&pkt("command=ls-refs\n"), &pkt("fetch\n"), FLUSH]),
                "unknown capability 'fetch'".into(),
            ),
            (
                "capabilities without a command",
                request(&[&pkt("agent=x\n"), FLUSH]),
                "no command requested".into(),
            ),
            (
                "an argument section without a command",
                request(&[&pkt("agent=x\n"), DELIM, FLUSH]),
                "no command requested".into(),
            ),
            (
                "an object format of another hash",
                request(&[
                    &pkt("command=ls-refs\n"),
                    &pkt(format!("object-format={}\n", other(f.kind))),
                    FLUSH,
                ]),
                format!("mismatched object format: server {}; client {}", f.kind, other(f.kind)),
            ),
            (
                "an object format without a value",
                request(&[&pkt("command=ls-refs\n"), &pkt("object-format\n"), FLUSH]),
                "object-format capability requires an argument".into(),
            ),
            (
                "an unknown object format",
                request(&[&pkt("command=ls-refs\n"), &pkt("object-format=md5\n"), FLUSH]),
                "unknown object format 'md5'".into(),
            ),
            (
                "an unknown argument of ls-refs",
                request(&[
                    &pkt("command=ls-refs\n"),
                    &pkt(format!("object-format={}\n", f.kind)),
                    DELIM,
                    &pkt("bogus\n"),
                    FLUSH,
                ]),
                "unexpected line: 'bogus'".into(),
            ),
            (
                "a second delimiter in ls-refs",
                request(&[
                    &pkt("command=ls-refs\n"),
                    &pkt(format!("object-format={}\n", f.kind)),
                    DELIM,
                    DELIM,
                ]),
                "expected flush after ls-refs arguments".into(),
            ),
            (
                "an unknown argument of fetch",
                request(&[
                    &pkt("command=fetch\n"),
                    &pkt(format!("object-format={}\n", f.kind)),
                    DELIM,
                    &pkt("filter blob:none\n"),
                    FLUSH,
                ]),
                "unexpected line: 'filter blob:none'".into(),
            ),
            (
                "a want that isn't an id",
                request(&[
                    &pkt("command=fetch\n"),
                    &pkt(format!("object-format={}\n", f.kind)),
                    DELIM,
                    &pkt("want nothex\n"),
                    FLUSH,
                ]),
                "git upload-pack: protocol error, expected to get oid, not 'want nothex'".into(),
            ),
            (
                "an error packet",
                request(&[&pkt("command=ls-refs\n"), &pkt("ERR it broke"), FLUSH]),
                "remote error: it broke".into(),
            ),
        ];
        if f.kind != Kind::Sha1 {
            cases.push((
                "no object format for a repository that doesn't use sha1",
                request(&[&pkt("command=ls-refs\n"), FLUSH]),
                format!("mismatched object format: server {}; client sha1", f.kind),
            ));
        }
        for (what, bytes, message) in cases {
            let git = upload_pack(&dir, &[], &bytes)?;
            assert!(!git.success, "{}: Git rejects {what}", f.kind);
            assert!(
                git.stderr.contains(&format!("fatal: {message}")),
                "{}: Git's message for {what} is the one we use: {}",
                f.kind,
                git.stderr
            );
            let err = parse(f.kind, &bytes).expect_err(what);
            assert!(
                format!("{err}").contains(&message),
                "{}: {what} is rejected with the message of Git: {err}",
                f.kind
            );
            assert!(err.classify().is_validation(), "{what} is a protocol error");
        }
    }
    Ok(())
}

#[test]
fn a_client_that_has_nothing_to_ask_ends_the_session_without_a_request() -> TestResult {
    for f in read_only()? {
        for (what, bytes) in [("it closes the connection", &b""[..]), ("it sends a flush", FLUSH)] {
            assert!(
                upload_pack(&f.remote(), &[], bytes)?.success,
                "{}: Git ends the session without error when {what}",
                f.kind
            );
            assert_eq!(parse(f.kind, bytes)?, None, "{what}");
        }
        let truncated = pkt("command=ls-refs\n");
        let err = parse(f.kind, &truncated).expect_err("a request that stops in the middle");
        assert!(
            format!("{err}").contains("hung up"),
            "the end of the connection in a request is an error: {err}"
        );
        assert!(!upload_pack(&f.remote(), &[], &truncated)?.success);
    }
    Ok(())
}

#[test]
fn ls_refs_arguments_are_read() -> TestResult {
    let kind = Kind::Sha1;
    let all = request(&[
        &pkt("command=ls-refs\n"),
        &pkt("agent=git/2.56.0\n"),
        &pkt("server-option=a\n"),
        &pkt("object-format=sha1\n"),
        DELIM,
        &pkt("peel\n"),
        &pkt("symrefs\n"),
        &pkt("unborn\n"),
        &pkt("ref-prefix refs/heads/\n"),
        &pkt("ref-prefix HEAD\n"),
        FLUSH,
    ]);
    let parsed = parse(kind, &all)?.expect("a request");
    assert_eq!(
        parsed.agent.as_ref().map(|agent| agent.as_bstr()),
        Some(b"git/2.56.0".as_bstr())
    );
    assert_eq!(parsed.server_options, ["a"]);
    assert_eq!(
        parsed.command,
        Command::LsRefs(LsRefs {
            peel: true,
            symrefs: true,
            unborn: true,
            ref_prefixes: vec!["refs/heads/".into(), "HEAD".into()],
        })
    );

    let without_arguments = request(&[&pkt("command=ls-refs\n"), FLUSH]);
    assert_eq!(
        parse(kind, &without_arguments)?.expect("a request").command,
        Command::LsRefs(LsRefs::default()),
        "a flush instead of the delimiter is a request without arguments"
    );
    Ok(())
}

#[test]
fn fetch_arguments_are_read_and_what_is_not_served_is_kept_for_refusal() -> TestResult {
    let kind = Kind::Sha1;
    let (a, b, c) = (hex_of(kind, 'a'), hex_of(kind, 'b'), hex_of(kind, 'c'));
    let bytes = request(&[
        &pkt("command=fetch\n"),
        DELIM,
        &pkt(format!("want {a}\n")),
        &pkt(format!("want {b}\n")),
        &pkt(format!("want {a}\n")),
        &pkt(format!("have {c}\n")),
        &pkt("thin-pack\n"),
        &pkt("ofs-delta\n"),
        &pkt("no-progress\n"),
        &pkt("include-tag\n"),
        &pkt("done\n"),
        &pkt("wait-for-done\n"),
        &pkt(format!("shallow {c}\n")),
        &pkt("deepen 3\n"),
        &pkt("deepen-relative\n"),
        &pkt("deepen-since 100\n"),
        &pkt("deepen-not refs/heads/x\n"),
        FLUSH,
    ]);
    let Command::Fetch(fetch) = parse(kind, &bytes)?.expect("a request").command else {
        panic!("a fetch");
    };
    let id = |hex: &str| gix_hash::ObjectId::from_hex(hex.as_bytes()).expect("valid");
    assert_eq!(
        fetch,
        Fetch {
            wants: vec![id(&a), id(&b)],
            haves: vec![id(&c)],
            done: true,
            thin_pack: true,
            ofs_delta: true,
            no_progress: true,
            include_tag: true,
            wait_for_done: true,
            shallow_lines: vec![
                format!("shallow {c}").into(),
                "deepen 3".into(),
                "deepen-relative".into(),
                "deepen-since 100".into(),
                "deepen-not refs/heads/x".into(),
            ],
        },
        "duplicate wants are one, and everything that is for shallow clones is kept for the driver to refuse"
    );
    Ok(())
}

#[test]
fn a_session_with_several_requests_is_read_one_at_a_time() -> TestResult {
    let two = request(&[
        &pkt("command=ls-refs\n"),
        FLUSH,
        &pkt("command=fetch\n"),
        DELIM,
        &pkt("done\n"),
        FLUSH,
    ]);
    let mut reader = &two[..];
    let capabilities = Capabilities::new(Kind::Sha1, "agent");
    assert!(matches!(
        read_request(&mut reader, &capabilities)?.expect("the first").command,
        Command::LsRefs(_)
    ));
    assert!(matches!(
        read_request(&mut reader, &capabilities)?.expect("the second").command,
        Command::Fetch(_)
    ));
    assert_eq!(
        read_request(&mut reader, &capabilities)?,
        None,
        "nothing follows, so the session is over"
    );
    Ok(())
}
