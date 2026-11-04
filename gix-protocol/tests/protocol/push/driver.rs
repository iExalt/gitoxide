//! The blocking driver over canned server output, to cover wire shapes real `receive-pack` doesn't produce on demand.
use std::io::{Cursor, Write};

use gix_error::{Class, ErrorExt};
use gix_hash::Kind;
use gix_protocol::push::{Options, Request, ServerCapabilities, push, report::Status};
use gix_transport::{
    Protocol, Service,
    client::git::{ConnectMode, blocking_io::Connection},
};

use super::{classes, commands};

fn pkt(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", data.len() + 4).into_bytes();
    out.extend_from_slice(data);
    out
}

fn band(band: u8, data: &[u8]) -> Vec<u8> {
    let mut payload = vec![band];
    payload.extend_from_slice(data);
    pkt(&payload)
}

/// What `receive-pack` sends before it reads commands: refs, with only the capabilities for an empty repository.
fn advertisement(kind: Kind, capabilities: &str) -> Vec<u8> {
    let mut out = pkt(format!(
        "{} capabilities^{{}}\0{capabilities} object-format={kind}\n",
        kind.null()
    )
    .as_bytes());
    out.extend_from_slice(b"0000");
    out
}

const CAPABILITIES: &str = "report-status delete-refs side-band-64k quiet atomic ofs-delta push-options";

/// The outcome of running `push()` against `response`, with everything the client wrote.
struct Run {
    result: gix_error::Result<gix_protocol::push::Report>,
    written: Vec<u8>,
    messages: Vec<(bool, Vec<u8>)>,
    pack_requested: bool,
}

fn run(
    kind: Kind,
    capabilities: &str,
    updates: &[(&str, u8, u8)],
    options: Options,
    response: &[u8],
    pack: impl FnOnce(&mut dyn Write) -> gix_error::Result,
) -> Run {
    let mut input = advertisement(kind, capabilities);
    input.extend_from_slice(response);
    let mut written = Vec::new();
    let mut messages = Vec::new();
    let mut pack_requested = false;
    let result = {
        let mut transport = Connection::new(
            Cursor::new(input),
            &mut written,
            Protocol::V1,
            "/repo.git",
            None::<(&str, _)>,
            ConnectMode::Process,
            false,
        );
        let handshake = gix_protocol::handshake(
            &mut transport,
            Service::ReceivePack,
            |_| Ok(None),
            Vec::new(),
            &mut gix_utils::progress::Discard,
        )
        .expect("canned handshake");
        let request = Request::new(
            commands(kind, updates),
            kind,
            &ServerCapabilities::from_capabilities(&handshake.capabilities),
            options,
        )
        .expect("a valid request");
        let mut on_message = |is_error: bool, text: &[u8]| messages.push((is_error, text.to_vec()));
        push(
            &mut transport,
            &request,
            |out| {
                pack_requested = true;
                pack(out)
            },
            Some(&mut on_message),
            false,
        )
    };
    Run {
        result,
        written,
        messages,
        pack_requested,
    }
}

fn report_lines(lines: &[&str]) -> Vec<u8> {
    let mut out: Vec<u8> = lines
        .iter()
        .flat_map(|line| pkt(format!("{line}\n").as_bytes()))
        .collect();
    out.extend_from_slice(b"0000");
    out
}

fn side_band_response(report: &[u8], extra_messages: &[&[u8]]) -> Vec<u8> {
    let mut out = band(1, report);
    for message in extra_messages {
        out.extend(band(2, message));
    }
    out.extend_from_slice(b"0000");
    out
}

#[test]
fn commands_options_and_pack_are_sent_and_the_side_band_report_is_decoded() {
    for kind in Kind::all().iter().copied() {
        let response = side_band_response(
            &report_lines(&["unpack ok", "ok refs/heads/a"]),
            &[b"post-receive says hi\n"],
        );
        let run = run(
            kind,
            CAPABILITIES,
            &[("refs/heads/a", 1, 2)],
            Options {
                push_options: vec!["topic=x".into()],
                ..Default::default()
            },
            &response,
            |out| out.write_all(b"PACK-bytes").map_err(gix_error::Error::from_error),
        );
        let report = run.result.expect("the canned report is valid");
        assert!(report.is_success(), "{kind}: {report:?}");
        let mut expected = commands_header(kind, &[("refs/heads/a", 1, 2)], true);
        expected.extend_from_slice(b"PACK-bytes");
        assert_eq!(
            run.written, expected,
            "{kind}: the header, the push options and the pack are written, and the pack verbatim"
        );
        assert_eq!(
            run.messages,
            [(false, b"post-receive says hi".to_vec())],
            "{kind}: messages that follow the report are delivered"
        );
    }
}

fn commands_header(kind: Kind, updates: &[(&str, u8, u8)], with_option: bool) -> Vec<u8> {
    let options = Options {
        push_options: if with_option {
            vec!["topic=x".into()]
        } else {
            Vec::new()
        },
        ..Default::default()
    };
    let capabilities = ServerCapabilities::from_capabilities(
        &gix_transport::client::Capabilities::from_bytes(format!("\0{CAPABILITIES} object-format={kind}").as_bytes())
            .expect("valid")
            .0,
    );
    Request::new(commands(kind, updates), kind, &capabilities, options)
        .expect("valid")
        .encode_header()
}

#[test]
fn deleting_sends_no_pack() {
    for kind in Kind::all().iter().copied() {
        let response = side_band_response(&report_lines(&["unpack ok", "ok refs/heads/a"]), &[]);
        let run = run(
            kind,
            CAPABILITIES,
            &[("refs/heads/a", 1, 0)],
            Options::default(),
            &response,
            |_| panic!("deletions have no pack"),
        );
        assert!(run.result.expect("valid report").is_success(), "{kind}");
        assert!(!run.pack_requested);
        assert_eq!(
            run.written,
            commands_header(kind, &[("refs/heads/a", 1, 0)], false),
            "{kind}"
        );
    }
}

#[test]
fn report_lines_split_across_side_band_packets_are_decoded() {
    for kind in Kind::all().iter().copied() {
        let report = report_lines(&["unpack ok", "ok refs/heads/a"]);
        let (first, second) = report.split_at(report.len() / 2 + 3);
        let mut response = band(1, first);
        response.extend(band(1, second));
        response.extend_from_slice(b"0000");
        let run = run(
            kind,
            CAPABILITIES,
            &[("refs/heads/a", 1, 2)],
            Options::default(),
            &response,
            |_| Ok(()),
        );
        assert!(
            run.result.expect("a split report is decoded as a stream").is_success(),
            "{kind}"
        );
    }
}

#[test]
fn empty_keepalive_packets_around_and_within_the_report_are_skipped() {
    for kind in Kind::all().iter().copied() {
        let report = report_lines(&["unpack ok", "ok refs/heads/a"]);
        let (first, second) = report.split_at(report.len() / 2);
        let mut response = band(1, b"");
        response.extend(band(1, first));
        response.extend(band(1, b""));
        response.extend(band(1, second));
        response.extend(band(1, b""));
        response.extend_from_slice(b"0000");
        let run = run(
            kind,
            CAPABILITIES,
            &[("refs/heads/a", 1, 2)],
            Options::default(),
            &response,
            |_| Ok(()),
        );
        assert!(
            run.result.expect("empty band-1 packets carry no data").is_success(),
            "{kind}: receive-pack sends them as keepalive while it is busy"
        );
    }
}

#[test]
fn without_side_band_the_report_consists_of_plain_packet_lines() {
    for kind in Kind::all().iter().copied() {
        let response = report_lines(&["unpack ok", "ng refs/heads/a pre-receive hook declined"]);
        let run = run(
            kind,
            "report-status delete-refs ofs-delta",
            &[("refs/heads/a", 1, 2)],
            Options::default(),
            &response,
            |_| Ok(()),
        );
        let report = run.result.expect("plain report");
        assert!(
            matches!(&report.commands[0].status, Status::Rejected { reason } if reason == "pre-receive hook declined"),
            "{kind}: {report:?}"
        );
    }
}

#[test]
fn a_missing_or_incomplete_report_is_an_uncertain_outcome() {
    let kind = Kind::all()[0];
    let updates = [("refs/heads/a", 1, 2)];
    let truncated_in_packet = {
        let mut out = band(1, &report_lines(&["unpack ok", "ok refs/heads/a"]));
        out.truncate(out.len() - 6);
        out
    };
    let report_without_flush = {
        let mut out = band(1, b"000eunpack ok\n0013ok refs/heads/a\n");
        out.extend_from_slice(b"0000");
        out
    };
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "the server hangs up without a word",
            Vec::new(),
            "outcome of the push is unknown",
        ),
        (
            "a remote error and nothing else",
            [band(3, b"fatal: it broke\n"), b"0000".to_vec()].concat(),
            "it broke",
        ),
        ("a report cut in the middle of a packet", truncated_in_packet, "unknown"),
        (
            "a report without its flush packet",
            report_without_flush,
            "outcome of the push is unknown",
        ),
        (
            "a flush where data is expected",
            [band(1, b"0000"), b"0000".to_vec()].concat(),
            "report was empty",
        ),
    ];
    for (name, response, says) in cases {
        let run = run(kind, CAPABILITIES, &updates, Options::default(), &response, |_| Ok(()));
        let err = run.result.expect_err(name);
        assert_eq!(classes(&err), [Class::Corruption], "{name}: {err:#}");
        assert!(
            format!("{err:#}").contains(says),
            "{name}: should say '{says}', got {err:#}"
        );
    }
}

#[test]
fn a_failing_pack_producer_ends_the_push_with_its_own_error() {
    let kind = Kind::all()[0];
    let run = run(
        kind,
        CAPABILITIES,
        &[("refs/heads/a", 1, 2)],
        Options::default(),
        &[],
        |_| Err(gix_error::validation("cannot produce the pack").raise()),
    );
    let err = run.result.expect_err("the producer failed");
    assert_eq!(
        classes(&err),
        [Class::Validation],
        "the producer's classification survives: {err:#}"
    );
}
