use gix_error::Class;
use gix_hash::Kind;
use gix_protocol::push::{
    Options, Report, StatusFormat,
    report::{Status, Unpack, Update},
};

use super::{classes, id, request};

fn lines(report: &str) -> Vec<&[u8]> {
    report.lines().map(str::as_bytes).collect()
}

fn v1(kind: Kind, atomic: bool) -> gix_protocol::push::Request {
    request(
        kind,
        &[("refs/heads/a", 1, 2), ("refs/heads/b", 0, 3), ("refs/tags/gone", 4, 0)],
        Options {
            atomic,
            ..Default::default()
        },
    )
}

fn v2(kind: Kind) -> gix_protocol::push::Request {
    request(
        kind,
        &[("refs/heads/a", 1, 2), ("refs/heads/b", 0, 3)],
        Options {
            status_format: StatusFormat::V2,
            ..Default::default()
        },
    )
}

fn statuses(report: &Report) -> Vec<(&str, &Status)> {
    report
        .commands
        .iter()
        .map(|c| (std::str::from_utf8(&c.ref_name).expect("utf8 in tests"), &c.status))
        .collect()
}

#[test]
fn v1_accepts_each_command_once_in_any_order_with_or_without_newlines() {
    for kind in Kind::all().iter().copied() {
        let request = v1(kind, false);
        let report = request
            .parse_report(lines(
                "unpack ok\nok refs/tags/gone\nng refs/heads/a pre-receive hook declined\nok refs/heads/b",
            ))
            .expect("a consistent report");
        assert_eq!(report.unpack, Unpack::Ok);
        assert_eq!(
            statuses(&report),
            [
                (
                    "refs/heads/a",
                    &Status::Rejected {
                        reason: "pre-receive hook declined".into()
                    }
                ),
                ("refs/heads/b", &Status::Accepted { updates: vec![] }),
                ("refs/tags/gone", &Status::Accepted { updates: vec![] }),
            ],
            "{kind}: results follow the order of the commands, and a rejection keeps its reason"
        );
        assert!(!report.is_success() && report.is_definite(), "{kind}");

        let with_newlines = request
            .parse_report([
                &b"unpack ok\n"[..],
                b"ok refs/heads/a\n",
                b"ok refs/heads/b\n",
                b"ng refs/tags/gone\n",
            ])
            .expect("packet lines usually end in newlines");
        assert_eq!(
            statuses(&with_newlines)[2].1,
            &Status::Rejected { reason: "".into() },
            "{kind}: a missing reason is empty"
        );
    }
}

#[test]
fn success_requires_a_successful_unpack_and_accepted_commands() {
    for kind in Kind::all().iter().copied() {
        let report = v1(kind, false)
            .parse_report(lines("unpack ok\nok refs/heads/a\nok refs/heads/b\nok refs/tags/gone"))
            .expect("consistent");
        assert!(report.is_success() && report.is_definite(), "{kind}");

        let report = v1(kind, false)
            .parse_report(lines(
                "unpack index-pack abnormal exit\nng refs/heads/a unpacker error\nng refs/heads/b unpacker error\nng refs/tags/gone unpacker error",
            ))
            .expect("consistent");
        assert_eq!(report.unpack, Unpack::Failed("index-pack abnormal exit".into()));
        assert!(!report.is_success(), "{kind}: a failed unpack is no success");
    }
}

#[test]
fn generic_commit_failures_are_uncertain_unless_atomic() {
    // Git overwrites the status of every command with this reason if committing one batch of updates failed,
    // even for deletions that were committed by the batch before.
    let failed = "unpack ok\nng refs/heads/a failed to update refs\nng refs/heads/b failed to update refs\nng refs/tags/gone failed to update refs";
    for kind in Kind::all().iter().copied() {
        let report = v1(kind, false).parse_report(lines(failed)).expect("consistent");
        assert!(
            report.commands.iter().all(|c| c.status
                == Status::Uncertain {
                    reason: "failed to update refs".into()
                }),
            "{kind}: not even the deletion's rejection can be trusted: {report:?}"
        );
        assert!(!report.is_definite() && !report.is_success());

        let report = v1(kind, false)
            .parse_report(lines(
                "unpack ok\nng refs/heads/a transaction failed to start\nng refs/heads/b transaction failed to start\nng refs/tags/gone transaction failed to start",
            ))
            .expect("consistent");
        assert!(
            !report.is_definite(),
            "{kind}: the same goes for a transaction that didn't start"
        );

        let report = v1(kind, true).parse_report(lines(failed)).expect("consistent");
        assert!(
            report.is_definite()
                && report
                    .commands
                    .iter()
                    .all(|c| matches!(&c.status, Status::Rejected { .. })),
            "{kind}: atomic transactions apply nothing on failure, so the rejection stands"
        );
    }
}

#[test]
fn atomic_pushes_can_have_definite_partial_outcomes_if_commands_fail_before_the_transaction() {
    // Observed with Git 2.56: an atomic push deleting `refs/tags/gone` and updating a ref to objects the remote lacks.
    // Git rejects the second command up front, and commits the rest, so the tag was really deleted.
    for kind in Kind::all().iter().copied() {
        let report = v1(kind, true)
            .parse_report(lines(
                "unpack ok\nng refs/heads/a missing necessary objects\nng refs/heads/b deny updating a hidden ref\nok refs/tags/gone",
            ))
            .expect("this is a consistent and definite outcome");
        assert!(report.is_definite() && !report.is_success(), "{kind}");
        assert_eq!(
            statuses(&report)[2],
            ("refs/tags/gone", &Status::Accepted { updates: vec![] }),
            "{kind}: the deletion happened"
        );

        let report = v1(kind, true)
            .parse_report(lines(
                "unpack ok\nng refs/heads/a transaction failed to start\nng refs/heads/b transaction failed to start\nng refs/tags/gone transaction failed to start",
            ))
            .expect("consistent");
        assert!(
            report.is_definite(),
            "{kind}: nothing is committed if an atomic transaction doesn't start, so it's a definite rejection"
        );
    }
}

#[test]
fn v2_accepts_options_and_repeated_ok_lines() {
    for kind in Kind::all().iter().copied() {
        let (old, new) = (id(kind, 1), id(kind, 2));
        let report = v2(kind)
            .parse_report(lines(&format!(
                "unpack ok\nok refs/heads/a\noption refname refs/pull/1/head\noption old-oid {old}\noption new-oid {new}\noption forced-update\noption unknown-key some value\nok refs/heads/a\noption refname refs/pull/2/head\nok refs/heads/b"
            )))
            .expect("a consistent v2 report");
        assert_eq!(
            statuses(&report),
            [
                (
                    "refs/heads/a",
                    &Status::Accepted {
                        updates: vec![
                            Update {
                                ref_name: Some("refs/pull/1/head".into()),
                                old_id: Some(old),
                                new_id: Some(new),
                                forced: true
                            },
                            Update {
                                ref_name: Some("refs/pull/2/head".into()),
                                ..Default::default()
                            }
                        ]
                    }
                ),
                (
                    "refs/heads/b",
                    &Status::Accepted {
                        updates: vec![Update::default()]
                    }
                ),
            ],
            "{kind}: each ok starts an update, unknown options are ignored, and an ok without options is one default update"
        );
        assert!(report.is_success());
    }
}

#[test]
fn inconsistent_reports_are_errors_classified_as_corruption() {
    for kind in Kind::all().iter().copied() {
        let other_len = if kind.len_in_bytes() == 20 { 32 } else { 20 };
        let wrong_length_id = "a".repeat(other_len * 2);
        let good_id = id(kind, 5);
        // (what is wrong, the report, whether the push is atomic, what the error says)
        let v1_cases: Vec<(&str, String, bool, &str)> = vec![
            ("an empty report", String::new(), false, "was empty"),
            ("a report not starting with unpack", "ok refs/heads/a".into(), false, "expected 'unpack"),
            ("an empty unpack status", "unpack ".into(), false, "expected 'unpack"),
            (
                "a missing command",
                "unpack ok\nok refs/heads/a\nok refs/heads/b".into(),
                false,
                "didn't report the status of 'refs/tags/gone'",
            ),
            (
                "an unknown reference",
                "unpack ok\nok refs/heads/a\nok refs/heads/b\nok refs/tags/gone\nok refs/heads/other".into(),
                false,
                "not part of this push",
            ),
            (
                "a repeated ok in v1",
                "unpack ok\nok refs/heads/a\nok refs/heads/a\nok refs/heads/b\nok refs/tags/gone".into(),
                false,
                "reported before",
            ),
            (
                "ng after ok",
                "unpack ok\nok refs/heads/a\nng refs/heads/a late\nok refs/heads/b\nok refs/tags/gone".into(),
                false,
                "reported before",
            ),
            (
                "ok after ng",
                "unpack ok\nng refs/heads/a early\nok refs/heads/a\nok refs/heads/b\nok refs/tags/gone".into(),
                false,
                "rejected before",
            ),
            (
                "ng twice",
                "unpack ok\nng refs/heads/a x\nng refs/heads/a y\nok refs/heads/b\nok refs/tags/gone".into(),
                false,
                "reported before",
            ),
            (
                "text after ok",
                "unpack ok\nok refs/heads/a extra\nok refs/heads/b\nok refs/tags/gone".into(),
                false,
                "no further text",
            ),
            (
                "an unknown verb",
                "unpack ok\nmaybe refs/heads/a\nok refs/heads/b\nok refs/tags/gone".into(),
                false,
                "expected 'ok', 'ng' or 'option'",
            ),
            ("a line without a value", "unpack ok\nok".into(), false, "expected '<ok|ng|option> <value>'"),
            (
                "options in v1",
                "unpack ok\nok refs/heads/a\noption refname refs/x\nok refs/heads/b\nok refs/tags/gone".into(),
                false,
                "only valid with report-status-v2",
            ),
            (
                "ok despite a failed unpack",
                "unpack bad pack\nok refs/heads/a\nng refs/heads/b unpacker error\nng refs/tags/gone unpacker error".into(),
                false,
                "unpacking failed",
            ),
            (
                "a partial atomic outcome",
                "unpack ok\nok refs/heads/a\nng refs/heads/b atomic push failure\nng refs/tags/gone atomic push failure"
                    .into(),
                true,
                "failed atomic transaction",
            ),
        ];
        for (name, report, atomic, says) in v1_cases {
            let err = v1(kind, atomic).parse_report(lines(&report)).expect_err(name);
            assert_eq!(classes(&err), [Class::Corruption], "{kind} {name}: {err:#}");
            assert!(
                format!("{err:#}").contains(says),
                "{kind} {name}: should say '{says}', got {err:#}"
            );
        }

        let v2_cases = [
            (
                "option before any ok",
                "unpack ok\noption refname refs/x\nok refs/heads/a\nok refs/heads/b".to_string(),
                "without a preceding 'ok'",
            ),
            (
                "option after ng",
                "unpack ok\nng refs/heads/a no\noption refname refs/x\nok refs/heads/b".to_string(),
                "without a preceding 'ok'",
            ),
            (
                "option with an id of the wrong hash",
                format!("unpack ok\nok refs/heads/a\noption old-oid {wrong_length_id}\nok refs/heads/b"),
                "object id",
            ),
            (
                "option with a malformed id",
                "unpack ok\nok refs/heads/a\noption new-oid zzzz\nok refs/heads/b".to_string(),
                "hexadecimal object id",
            ),
            (
                "option without a value",
                "unpack ok\nok refs/heads/a\noption refname\nok refs/heads/b".to_string(),
                "expected a reference name",
            ),
            (
                "a repeated option within one update",
                format!(
                    "unpack ok\nok refs/heads/a\noption new-oid {good_id}\noption new-oid {good_id}\nok refs/heads/b"
                ),
                "more than once",
            ),
            (
                "a missing command",
                "unpack ok\nok refs/heads/a".to_string(),
                "didn't report the status of 'refs/heads/b'",
            ),
        ];
        for (name, report, says) in v2_cases {
            let err = v2(kind).parse_report(lines(&report)).expect_err(name);
            assert_eq!(classes(&err), [Class::Corruption], "{kind} {name}: {err:#}");
            assert!(
                format!("{err:#}").contains(says),
                "{kind} {name}: should say '{says}', got {err:#}"
            );
        }
    }
}
