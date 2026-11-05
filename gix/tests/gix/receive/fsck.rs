//! A push is received by `git receive-pack` and by the native receiver with `receive.fsckObjects` and its relatives, and both
//! have to refuse what they refuse, and say the same about it and about what they accept with a warning.
//! The remotes are made by `make_fsck_remotes.sh`, which says what they contain.
use std::fmt::Write as _;

use gix_testtools::TestResult;

use super::receive_pack::{Cmd, Push, World, compare_on, push, with_setup};

const CHECK: &[&str] = &["config receive.fsckObjects true"];

fn world() -> TestResult<World> {
    World::from_fixture("make_fsck_remotes.sh")
}

/// A push that moves `main` forward to `branch`, which is one commit on top of it.
fn forward(branch: &'static str) -> Push {
    push(&[Cmd("main", branch, "refs/heads/main")])
}

/// Receive `push` with Git and with the native receiver, and require that they agree, and that the push was accepted or
/// refused as `accepted` says, which makes sure that the comparison is about something. Return what Git said to people.
fn same(world: &World, label: &str, push: &Push, accepted: bool) -> TestResult<String> {
    let results = compare_on(world, label, push, &["files"])?;
    let git = results.first().expect("one format");
    let report = String::from_utf8_lossy(&git.report);
    let refused = report.contains("ng refs/") || report.contains("unpack ") && !report.contains("unpack ok");
    assert_eq!(
        !refused, accepted,
        "{label}: Git's verdict is the one expected\n{report}\n{}",
        git.messages
    );
    Ok(git.messages.clone())
}

/// Branches with a commit on top of `main` that have a flaw that is an error, so they are refused when checking.
const REFUSED: &[&str] = &[
    "missing-committer",
    "bad-email",
    "bad-timezone",
    "zero-padded-date",
    "two-flaws",
    "zero-padded-mode",
    "unsorted",
    "duplicate",
    "dotgit",
    "dotgit-ntfs",
    "full-pathname",
    "dot-entry",
    "dotdot-entry",
    "symlink-gitmodules",
    "gitmodules-bad-url",
    "gitmodules-bad-name",
];

/// Branches that pass, and which of them have something to say, which is a warning.
const ACCEPTED: &[(&str, bool)] = &[
    ("ok", false),
    ("extra-header", false),
    ("gitmodules-ok", false),
    ("bad-mode-info", true),
];

#[test]
fn objects_are_checked_when_asked_to_and_not_unless() -> TestResult {
    let world = world()?;
    for branch in REFUSED {
        let messages = same(
            &world,
            &format!("{branch}-checked"),
            &with_setup(forward(branch), CHECK),
            false,
        )?;
        assert!(
            messages.contains("fatal: ") || messages.contains("error: "),
            "{branch}: people are told why\n{messages}"
        );
        let messages = same(&world, &format!("{branch}-unchecked"), &forward(branch), true)?;
        assert!(
            messages.is_empty(),
            "{branch}: nothing is said if nothing is checked: {messages}"
        );
    }
    for (branch, warns) in ACCEPTED {
        let messages = same(
            &world,
            &format!("{branch}-checked"),
            &with_setup(forward(branch), CHECK),
            true,
        )?;
        assert_eq!(
            messages.contains("warning: "),
            *warns,
            "{branch}: warnings are shown\n{messages}"
        );
    }
    Ok(())
}

#[test]
fn packs_that_are_kept_and_packs_that_are_exploded_are_checked_alike() -> TestResult {
    let world = world()?;
    for limit in ["0", "1", "2", "100"] {
        let setup = |extra: &'static [&'static str]| -> Vec<&'static str> {
            let mut setup = vec!["config receive.fsckObjects true"];
            setup.push(match limit {
                "0" => "config receive.unpackLimit 0",
                "1" => "config receive.unpackLimit 1",
                "2" => "config transfer.unpackLimit 2",
                _ => "config receive.unpackLimit 100",
            });
            setup.extend(extra);
            setup
        };
        for branch in ["zero-padded-mode", "gitmodules-bad-url", "bad-email", "unsorted"] {
            let mut p = forward(branch);
            p.setup = setup(&[]);
            same(&world, &format!("{branch}-limit-{limit}"), &p, false)?;
        }
        for (branch, warns) in [("ok", false), ("bad-mode-info", true), ("gitmodules-ok", false)] {
            let mut p = forward(branch);
            p.setup = setup(&[]);
            let messages = same(&world, &format!("{branch}-limit-{limit}"), &p, true)?;
            assert_eq!(messages.contains("warning: "), warns, "{branch}/{limit}");
        }
    }
    Ok(())
}

#[test]
fn more_than_a_hundred_objects_are_kept_and_checked() -> TestResult {
    let world = world()?;
    same(&world, "many-ok", &with_setup(forward("many-ok"), CHECK), true)?;
    same(&world, "many-flawed", &with_setup(forward("many-flawed"), CHECK), false)?;
    same(
        &world,
        "many-unsorted",
        &with_setup(forward("many-unsorted"), CHECK),
        false,
    )?;
    same(&world, "many-flawed-unchecked", &forward("many-flawed"), true)?;
    Ok(())
}

#[test]
fn receive_decides_over_transfer_whatever_the_order() -> TestResult {
    let world = world()?;
    for (label, setup, accepted) in [
        ("receive", &["config receive.fsckObjects true"][..], false),
        ("transfer", &["config transfer.fsckObjects true"], false),
        (
            "receive-over-transfer-off",
            &["config transfer.fsckObjects false", "config receive.fsckObjects true"],
            false,
        ),
        (
            "transfer-on-receive-off",
            &["config transfer.fsckObjects true", "config receive.fsckObjects false"],
            true,
        ),
        (
            "receive-off-first",
            &["config receive.fsckObjects false", "config transfer.fsckObjects true"],
            true,
        ),
        ("fetch-is-not-for-receive", &["config fetch.fsckObjects true"], true),
        ("integer", &["config receive.fsckObjects 1"], false),
        ("word", &["config receive.fsckObjects yes"], false),
    ] {
        let mut p = forward("zero-padded-mode");
        p.setup = setup.to_vec();
        same(&world, label, &p, accepted)?;
    }
    Ok(())
}

#[test]
fn severities_are_configured_per_message_for_receive_only() -> TestResult {
    let world = world()?;
    for (label, branch, extra, accepted, warns) in [
        (
            "ignored",
            "zero-padded-mode",
            &["config receive.fsck.zeroPaddedFilemode ignore"][..],
            true,
            false,
        ),
        (
            "warned",
            "zero-padded-mode",
            &["config receive.fsck.zeroPaddedFilemode warn"],
            true,
            true,
        ),
        (
            "case-insensitive-name",
            "zero-padded-mode",
            &["config receive.fsck.ZEROPADDEDFILEMODE warn"],
            true,
            true,
        ),
        (
            "plain-fsck-is-for-git-fsck",
            "zero-padded-mode",
            &["config fsck.zeroPaddedFilemode ignore"],
            false,
            false,
        ),
        (
            "fetch-is-for-fetch",
            "zero-padded-mode",
            &["config fetch.fsck.zeroPaddedFilemode ignore"],
            false,
            false,
        ),
        (
            "unknown-id-is-skipped",
            "zero-padded-mode",
            &["config receive.fsck.nonsense error"],
            false,
            false,
        ),
        (
            "others-stay-errors",
            "unsorted",
            &["config receive.fsck.zeroPaddedFilemode ignore"],
            false,
            false,
        ),
        (
            "later-wins",
            "zero-padded-mode",
            &[
                "config receive.fsck.zeroPaddedFilemode error",
                "config --add receive.fsck.zeroPaddedFilemode warn",
            ],
            true,
            true,
        ),
        (
            "zero-padded-date-ignored",
            "zero-padded-date",
            &["config receive.fsck.zeroPaddedDate ignore"],
            true,
            false,
        ),
        (
            "bad-email-ignored",
            "bad-email",
            &["config receive.fsck.badEmail ignore"],
            true,
            false,
        ),
        (
            "bad-timezone-ignored",
            "bad-timezone",
            &["config receive.fsck.badTimezone ignore"],
            true,
            false,
        ),
        (
            "missing-committer-ignored",
            "missing-committer",
            &["config receive.fsck.missingCommitter ignore"],
            true,
            false,
        ),
        (
            "dotgit-ignored",
            "dotgit",
            &["config receive.fsck.hasDotgit ignore"],
            true,
            false,
        ),
        (
            "gitmodules-url-warned",
            "gitmodules-bad-url",
            &["config receive.fsck.gitmodulesUrl warn"],
            true,
            true,
        ),
        (
            "everything-warned-except-whats-asked",
            "zero-padded-mode",
            &[
                "config receive.fsck.treeNotSorted warn",
                "config receive.fsck.zeroPaddedFilemode ignore",
            ],
            true,
            false,
        ),
        (
            "promotion-stays-for-the-others",
            "full-pathname",
            &["config receive.fsck.zeroPaddedFilemode ignore"],
            false,
            false,
        ),
    ] {
        let mut setup: Vec<_> = CHECK.to_vec();
        setup.extend_from_slice(extra);
        let mut p = forward(branch);
        p.setup = setup;
        let messages = same(&world, label, &p, accepted)?;
        assert_eq!(messages.contains("warning: "), warns, "{label}: warnings\n{messages}");
    }
    Ok(())
}

/// Write the ids of what is to be skipped to `skip.txt` in the remote, as the lines of a skip list.
fn skip_list(remote: &std::path::Path, client: &std::path::Path, what: &[&str]) -> TestResult {
    let mut text = String::from("# the objects to leave alone\n\n");
    for rev in what {
        let id = gix_testtools::git(client, &format!("rev-parse {rev}"))?;
        writeln!(text, "  {}\t# why", id.trim())?;
    }
    std::fs::write(remote.join("skip.txt"), text)?;
    Ok(())
}

#[test]
fn skipped_objects_are_not_reported_and_nothing_else_is() -> TestResult {
    let world = world()?;
    let setup = &[
        "config receive.fsckObjects true",
        "config receive.fsck.skipList skip.txt",
    ][..];
    let with = |branch: &'static str, prepare: fn(&std::path::Path, &std::path::Path) -> TestResult| {
        let mut p = forward(branch);
        p.setup = setup.to_vec();
        p.prepare = Some(prepare);
        p
    };
    same(
        &world,
        "skip-the-tree",
        &with("zero-padded-mode", |remote, client| {
            skip_list(remote, client, &["zero-padded-mode^{tree}"])
        }),
        true,
    )?;
    same(
        &world,
        "skip-the-commit",
        &with("zero-padded-date", |remote, client| {
            skip_list(remote, client, &["zero-padded-date"])
        }),
        true,
    )?;
    same(
        &world,
        "skip-the-wrong-object",
        &with("zero-padded-mode", |remote, client| {
            skip_list(remote, client, &["zero-padded-mode"])
        }),
        false,
    )?;
    same(
        &world,
        "skip-the-blob-of-gitmodules",
        &with("gitmodules-bad-url", |remote, client| {
            skip_list(remote, client, &["gitmodules-bad-url:.gitmodules"])
        }),
        true,
    )?;
    same(
        &world,
        "skip-what-is-no-problem",
        &with("unsorted", |remote, client| skip_list(remote, client, &["ok"])),
        false,
    )?;
    // Whether the skip list is read doesn't depend on the objects. Without checking it's ignored, even if it is missing.
    let mut p = forward("zero-padded-mode");
    p.setup = vec!["config receive.fsck.skipList does-not-exist.txt"];
    same(&world, "unread-skip-list-is-no-problem", &p, true)?;
    Ok(())
}

#[test]
fn legacy_problems_of_objects_the_repository_has_are_not_what_is_checked() -> TestResult {
    let world = world()?;
    let legacy = |commands: &[Cmd]| {
        let mut p = push(commands);
        p.remote = Some("remote-legacy.git");
        p.setup = CHECK.to_vec();
        p
    };
    // The pack is thin, so that it has an object of the repository as base for the tree that changes, which is not checked.
    let mut thin = legacy(&[Cmd("legacy-main", "legacy-next", "refs/heads/main")]);
    thin.thin = true;
    let messages = same(&world, "thin-pack-on-a-flawed-base", &thin, true)?;
    assert!(
        messages.is_empty(),
        "the base isn't checked, so nothing is said: {messages}"
    );

    let mut thin = legacy(&[Cmd("legacy-main", "legacy-next", "refs/heads/main")]);
    thin.thin = true;
    thin.setup.push("config receive.unpackLimit 0");
    same(&world, "thin-pack-on-a-flawed-base-kept", &thin, true)?;

    // What the repository has is checked if it is sent again, whether or not it is needed.
    let mut resend = legacy(&[Cmd("0", "legacy-resend", "refs/heads/resend")]);
    resend.pack_of = Some(vec!["legacy-resend"]);
    same(&world, "flawed-tree-sent-again", &resend, false)?;
    let mut resend = legacy(&[Cmd("0", "legacy-resend", "refs/heads/resend")]);
    resend.pack_of = Some(vec!["legacy-resend"]);
    resend.setup.push("config receive.unpackLimit 0");
    same(&world, "flawed-tree-sent-again-kept", &resend, false)?;
    Ok(())
}

#[test]
fn tags_are_checked_as_well() -> TestResult {
    let world = world()?;
    let tag = |name: &'static str| {
        let reference: &'static str = Box::leak(format!("refs/tags/{name}").into_boxed_str());
        let mut p = push(&[Cmd("0", reference, reference)]);
        p.setup = CHECK.to_vec();
        p
    };
    let messages = same(&world, "no-tagger", &tag("no-tagger"), true)?;
    assert!(
        messages.contains("warning: "),
        "a missing tagger is only a warning: {messages}"
    );
    let messages = same(&world, "bad-tag-name", &tag("bad-tag-name"), true)?;
    assert!(
        messages.contains("badTagName"),
        "a bad tag name is only a warning: {messages}"
    );
    Ok(())
}

#[test]
fn the_history_a_shallow_client_leaves_out_is_not_missing_to_the_check() -> TestResult {
    let world = world()?;
    let mut shallow = push(&[Cmd("0", "ok", "refs/heads/ok")]);
    shallow.remote = Some("remote-empty.git");
    shallow.shallow = vec!["ok"];
    shallow.setup = vec!["config receive.fsckObjects true", "config receive.shallowUpdate true"];
    same(&world, "shallow-client", &shallow, true)?;
    shallow.setup = vec![
        "config receive.fsckObjects true",
        "config receive.shallowUpdate true",
        "config receive.unpackLimit 0",
    ];
    same(&world, "shallow-client-kept", &shallow, true)?;
    Ok(())
}

#[test]
fn the_history_a_shallow_receiver_leaves_out_is_not_missing_to_the_check() -> TestResult {
    let world = world()?;
    // The pack has `main` without its parent, as it would for a client that is shallow, but nobody says so. What makes the
    // parent not be missing is the shallow file of the receiver.
    let mut push = push(&[Cmd("0", "main", "refs/heads/copy")]);
    push.remote = Some("remote-shallow.git");
    push.pack_of = Some(vec!["main", "^main~1"]);
    push.setup = vec!["config receive.fsckObjects true"];
    same(&world, "shallow-receiver", &push, true)?;
    push.setup = vec!["config receive.fsckObjects true", "config receive.unpackLimit 0"];
    same(&world, "shallow-receiver-kept", &push, true)?;
    Ok(())
}
