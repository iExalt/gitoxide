//! Receiving into a shallow repository: what it advertises and what it takes from a client that has the whole history, which
//! is compared with what `git receive-pack` does. `make_shallow_receivers.sh` says what the repositories and the branches
//! of the client are.
//!
//! The only commits without parents are the entries of the `shallow` file. Anything below them is missing, and a client that
//! assumes that the receiver has it, because it can reach it from what the receiver advertised, is told so.
use std::{io::Write as _, path::Path};

use gix_testtools::TestResult;

use super::receive_pack::{
    Cmd, Hook, Push, World, add_alternate_of_client, assert_advertisement_same, compare_on, fork_receive,
};

const FORMATS: &[&str] = &["files", "reftable"];

fn world() -> TestResult<World> {
    World::from_fixture("make_shallow_receivers.sh")
}

/// Receive `push` with Git and with the fork, and compare what they say and leave behind. Git has to tell `verdict`, so that no
/// row can pass by being refused for another reason than the one it is about.
fn same(world: &World, label: &str, push: &Push, verdict: &str) -> TestResult {
    same_all(world, label, push, &[verdict])
}

fn same_all(world: &World, label: &str, push: &Push, verdicts: &[&str]) -> TestResult {
    for result in compare_on(world, label, push, FORMATS)? {
        let report = String::from_utf8_lossy(&result.report);
        for verdict in verdicts {
            assert!(
                report.contains(verdict),
                "{label}: Git is to tell {verdict:?}, but told: {report}"
            );
        }
    }
    Ok(())
}

/// A push to the receiver that has `main` and `extra` at `c4`, which is where it is shallow. Pack only what the client doesn't
/// know the receiver has by what it advertised.
fn to(remote: &'static str, commands: Vec<Cmd>, pack_of: Vec<&'static str>) -> Push {
    Push {
        remote: Some(remote),
        commands,
        pack_of: Some(pack_of),
        ..Default::default()
    }
}

fn with_config(mut push: Push, setup: &'static str) -> Push {
    push.setup.push(setup);
    push
}

/// The `shallow` file with an entry for a commit that the repository doesn't have, one that it does, and the latter twice.
fn shallow_file_with_odd_entries(remote: &Path, client: &Path) -> TestResult {
    let git_dir = Path::new(gix_testtools::git(remote, "rev-parse --absolute-git-dir")?.trim()).to_owned();
    let absent = gix_testtools::git(client, "rev-parse main~3")?;
    let present = gix_testtools::git(client, "rev-parse main")?;
    std::fs::write(
        git_dir.join("shallow"),
        format!("{}\n{}\n{}\n", present.trim(), absent.trim(), present.trim()),
    )?;
    Ok(())
}

fn shallow_file_and_alternate(remote: &Path, client: &Path) -> TestResult {
    add_alternate_of_client(remote, client)
}

#[test]
fn the_advertisement_lists_the_entries_of_the_shallow_file() -> TestResult {
    let world = world()?;
    let cases = [
        (
            "graft",
            Push {
                remote: Some("remote.git"),
                ..Default::default()
            },
        ),
        (
            "graft-and-commit-graph",
            Push {
                remote: Some("remote-graph.git"),
                ..Default::default()
            },
        ),
        (
            "odd-entries",
            Push {
                remote: Some("remote.git"),
                prepare: Some(shallow_file_with_odd_entries),
                ..Default::default()
            },
        ),
        (
            "alternate",
            Push {
                remote: Some("remote.git"),
                prepare: Some(shallow_file_and_alternate),
                ..Default::default()
            },
        ),
    ];
    for (label, case) in &cases {
        for format in FORMATS {
            assert_advertisement_same(&world, format, label, case)?;
        }
    }
    Ok(())
}

const OK_MAIN: &str = "ok refs/heads/main";
const MISSING: &str = "missing necessary objects";

#[test]
fn what_is_built_on_the_shallow_commit_is_received() -> TestResult {
    let world = world()?;
    for remote in ["remote.git", "remote-graph.git"] {
        let label = |what: &str| format!("{remote}-{what}");
        same(
            &world,
            &label("fast-forward"),
            &to(remote, vec![Cmd("main", "ff", "refs/heads/main")], vec!["ff", "^main"]),
            OK_MAIN,
        )?;
        same(
            &world,
            &label("new-branch"),
            &to(remote, vec![Cmd("0", "ff", "refs/heads/new")], vec!["ff", "^main"]),
            "ok refs/heads/new",
        )?;
        // The whole history is no problem either, as everything it needs is brought along.
        same(
            &world,
            &label("whole-history"),
            &to(remote, vec![Cmd("0", "side", "refs/heads/whole")], vec!["side"]),
            "ok refs/heads/whole",
        )?;
        // No reference leads to the shallow commit, so the check has to walk to it and stop there instead of at a reference.
        let unreferenced = with_config(
            with_config(
                to(remote, vec![Cmd("0", "ff", "refs/heads/new")], vec!["ff", "^main"]),
                "update-ref -d refs/heads/main",
            ),
            "update-ref -d refs/heads/extra",
        );
        same(
            &world,
            &label("unreferenced-shallow-commit"),
            &unreferenced,
            "ok refs/heads/new",
        )?;
        same(
            &world,
            &label("delete"),
            &to(remote, vec![Cmd("main", "0", "refs/heads/extra")], vec![]),
            "ok refs/heads/extra",
        )?;
        let rewrite = to(remote, vec![Cmd("main", "orphan", "refs/heads/extra")], vec!["orphan"]);
        same(&world, &label("rewrite"), &rewrite, "ok refs/heads/extra")?;
        same(
            &world,
            &label("rewrite-denied"),
            &with_config(rewrite, "config receive.denyNonFastForwards true"),
            "non-fast-forward",
        )?;
        same(
            &world,
            &label("fast-forward-checked"),
            &with_config(
                to(remote, vec![Cmd("main", "ff", "refs/heads/main")], vec!["ff", "^main"]),
                "config receive.denyNonFastForwards true",
            ),
            OK_MAIN,
        )?;
    }
    Ok(())
}

#[test]
fn history_below_the_shallow_commit_is_missing_even_if_the_client_assumes_it_is_there() -> TestResult {
    let world = world()?;
    for remote in ["remote.git", "remote-graph.git"] {
        let label = |what: &str| format!("{remote}-{what}");
        // Both leave out what can be reached from `main`, which the receiver offers, but the receiver doesn't have `c2`.
        same(
            &world,
            &label("merge"),
            &to(
                remote,
                vec![Cmd("0", "below-merge", "refs/heads/below-merge")],
                vec!["below-merge", "^main"],
            ),
            MISSING,
        )?;
        same(
            &world,
            &label("branch"),
            &to(
                remote,
                vec![Cmd("0", "side", "refs/heads/below")],
                vec!["side", "^main"],
            ),
            MISSING,
        )?;
    }
    Ok(())
}

/// A client that is shallow at the commits `shallow`, which are revisions of the client.
fn shallow_at(mut push: Push, shallow: &[&'static str]) -> Push {
    push.shallow = shallow.to_vec();
    push
}

/// The `shallow` file lists `c4`, which the repository doesn't have.
fn shallow_file_with_c4(remote: &Path, client: &Path) -> TestResult {
    let git_dir = Path::new(gix_testtools::git(remote, "rev-parse --absolute-git-dir")?.trim()).to_owned();
    let present = gix_testtools::git(client, "rev-parse main")?;
    std::fs::write(git_dir.join("shallow"), format!("{}\n", present.trim()))?;
    Ok(())
}

const NOT_ALLOWED: &str = "shallow update not allowed";

#[test]
fn a_client_that_is_shallow_is_received_unless_the_receiver_would_have_to_become_shallow() -> TestResult {
    let world = world()?;
    let on_main =
        |remote, new: &'static str, name: &'static str| to(remote, vec![Cmd("0", new, name)], vec!["ff", "^main"]);
    // The boundary is `c4`, which the receiver has, and it has a reference at it or later, so the history stays whole.
    same(
        &world,
        "update-of-the-boundary",
        &shallow_at(
            to(
                "full.git",
                vec![Cmd("main", "ff", "refs/heads/main")],
                vec!["ff", "^main"],
            ),
            &["main"],
        ),
        OK_MAIN,
    )?;
    same(
        &world,
        "new-branch",
        &shallow_at(on_main("full.git", "ff", "refs/heads/topic"), &["main"]),
        "ok refs/heads/topic",
    )?;
    same(
        &world,
        "reference-moved-on",
        &shallow_at(on_main("moved.git", "ff", "refs/heads/topic"), &["main"]),
        "ok refs/heads/topic",
    )?;
    // Hidden references and `HEAD` count, as they are what the receiver has.
    let hidden = with_config(
        with_config(
            with_config(
                shallow_at(on_main("full.git", "ff", "refs/heads/topic"), &["main"]),
                "update-ref refs/hidden/x main",
            ),
            "update-ref refs/heads/main main~2",
        ),
        "config receive.hideRefs refs/hidden",
    );
    same(&world, "reached-by-a-hidden-reference", &hidden, "ok refs/heads/topic")?;
    let detached = with_config(
        with_config(
            shallow_at(on_main("full.git", "ff", "refs/heads/topic"), &["main"]),
            "update-ref --no-deref HEAD main",
        ),
        "update-ref refs/heads/main main~2",
    );
    same(&world, "reached-by-head", &detached, "ok refs/heads/topic")?;
    // The receiver having the commit isn't enough if nothing reaches it, as then it would have to keep it alive.
    let unreachable = with_config(
        shallow_at(on_main("full.git", "ff", "refs/heads/topic"), &["main"]),
        "update-ref refs/heads/main main~2",
    );
    same(&world, "not-reached-by-anything", &unreachable, NOT_ALLOWED)?;

    // The receiver doesn't have it, which the pack brings, so that it would have to become shallow.
    let bringing_the_boundary = to(
        "empty.git",
        vec![Cmd("0", "ff", "refs/heads/main")],
        vec!["ff", "^main~1"],
    );
    same(
        &world,
        "new-boundary",
        &shallow_at(bringing_the_boundary.clone(), &["main"]),
        NOT_ALLOWED,
    )?;
    // Also when the receiver was told about it by its `shallow` file, as the commit wasn't there.
    same(
        &world,
        "boundary-of-the-file-that-arrives",
        &Push {
            prepare: Some(shallow_file_with_c4),
            ..shallow_at(bringing_the_boundary, &["main"])
        },
        NOT_ALLOWED,
    )?;
    // A boundary that a reference reaches only through another boundary is reached all the same, as it is by the real history.
    same(
        &world,
        "boundary-below-a-boundary",
        &shallow_at(
            to(
                "full.git",
                vec![Cmd("0", "side", "refs/heads/side2")],
                vec!["side", "^main"],
            ),
            &["main", "main~2"],
        ),
        "ok refs/heads/side2",
    )?;
    // The tip is the boundary itself.
    same(
        &world,
        "the-tip-is-the-boundary",
        &shallow_at(on_main("full.git", "ff", "refs/heads/topic"), &["ff"]),
        NOT_ALLOWED,
    )?;

    // Only the commands that need it are rejected, whether the push is atomic or not, as Git updates the others.
    let two = |atomic| Push {
        atomic,
        ..shallow_at(
            to(
                "full.git",
                vec![
                    Cmd("main", "ff", "refs/heads/main"),
                    Cmd("0", "side", "refs/heads/side2"),
                ],
                vec!["ff", "side", "^main"],
            ),
            &["main", "side"],
        )
    };
    same_all(
        &world,
        "one-of-two",
        &two(false),
        &[OK_MAIN, "ng refs/heads/side2 shallow update not allowed"],
    )?;
    same_all(
        &world,
        "one-of-two-atomic",
        &two(true),
        &[OK_MAIN, "ng refs/heads/side2 shallow update not allowed"],
    )?;
    Ok(())
}

#[test]
fn what_a_shallow_client_says_that_doesnt_matter_is_ignored() -> TestResult {
    let world = world()?;
    let topic = || on_c4("full.git");
    // Deleting needs nothing, even though the client is shallow.
    same(
        &world,
        "deletion",
        &shallow_at(
            to("remote.git", vec![Cmd("main", "0", "refs/heads/extra")], vec![]),
            &["main"],
        ),
        "ok refs/heads/extra",
    )?;
    // A blob, a commit that the receiver doesn't get, and the boundary that is the receiver's own.
    for (label, shallow) in [("blob", "main:file"), ("unknown-commit", "main~3")] {
        same(&world, label, &shallow_at(topic(), &[shallow]), "ok refs/heads/topic")?;
    }
    same(
        &world,
        "own-boundary",
        &shallow_at(
            to(
                "remote.git",
                vec![Cmd("main", "ff", "refs/heads/main")],
                vec!["ff", "^main"],
            ),
            &["main"],
        ),
        OK_MAIN,
    )?;
    // History that is below the receiver's boundary is missing, which is what the push is told for a client that isn't shallow.
    same(
        &world,
        "built-on-what-the-receiver-lacks",
        &shallow_at(
            to(
                "remote.git",
                vec![Cmd("0", "on-c3", "refs/heads/t1")],
                vec!["on-c3", "^main"],
            ),
            &["main~1"],
        ),
        MISSING,
    )
}

fn on_c4(remote: &'static str) -> Push {
    to(remote, vec![Cmd("0", "ff", "refs/heads/topic")], vec!["ff", "^main"])
}

const UPDATE: &str = "config receive.shallowUpdate true";

fn lines(file: Option<String>) -> Vec<String> {
    file.map(|text| text.lines().map(ToOwned::to_owned).collect())
        .unwrap_or_default()
}

#[test]
fn a_receiver_that_is_told_to_becomes_shallow_where_the_client_is() -> TestResult {
    let world = world()?;
    let to_update = |push: Push| with_config(push, UPDATE);
    // What the pack brings of the boundary is written to the `shallow` file, which compares as it is a file of the receiver.
    let bringing_the_boundary = to(
        "empty.git",
        vec![Cmd("0", "ff", "refs/heads/main")],
        vec!["ff", "^main~1"],
    );
    same(
        &world,
        "new-boundary",
        &to_update(shallow_at(bringing_the_boundary, &["main"])),
        OK_MAIN,
    )?;
    // Not what a reference reaches, or what is the boundary of the receiver already.
    same(
        &world,
        "nothing-to-take",
        &to_update(shallow_at(on_c4("moved.git"), &["main"])),
        "ok refs/heads/topic",
    )?;
    same(
        &world,
        "own-boundary",
        &to_update(shallow_at(
            to(
                "remote.git",
                vec![Cmd("main", "ff", "refs/heads/main")],
                vec!["ff", "^main"],
            ),
            &["main"],
        )),
        OK_MAIN,
    )?;
    // What is added to what the file has, with what was there first and the new commits after it.
    let beside_the_old = |atomic| Push {
        atomic,
        ..to_update(shallow_at(
            to(
                "remote.git",
                vec![Cmd("0", "side", "refs/heads/side2")],
                vec!["side", "^main"],
            ),
            &["main", "side"],
        ))
    };
    same(
        &world,
        "added-to-the-file",
        &beside_the_old(false),
        "ok refs/heads/side2",
    )?;
    // The command that needs it is the last one, as Git forgets about those that follow it.
    let last = |atomic| Push {
        atomic,
        ..to_update(shallow_at(
            to(
                "full.git",
                vec![
                    Cmd("main", "ff", "refs/heads/main"),
                    Cmd("0", "side", "refs/heads/side2"),
                ],
                vec!["ff", "side", "^main"],
            ),
            &["main", "side"],
        ))
    };
    same_all(
        &world,
        "needed-by-the-last",
        &last(false),
        &[OK_MAIN, "ok refs/heads/side2"],
    )?;
    same_all(
        &world,
        "needed-by-the-last-atomic",
        &last(true),
        &[OK_MAIN, "ok refs/heads/side2"],
    )?;
    // What reached the boundary through the reference that is deleted doesn't any more, which Git looks at after the deletion.
    let deleted = with_config(
        with_config(
            to_update(shallow_at(
                to(
                    "moved.git",
                    vec![Cmd("moved", "0", "refs/heads/keep"), Cmd("0", "ff", "refs/heads/topic")],
                    vec!["ff", "^main"],
                ),
                &["main"],
            )),
            "update-ref refs/heads/keep main",
        ),
        "update-ref refs/heads/main main~4",
    );
    same_all(
        &world,
        "reached-by-what-is-deleted",
        &deleted,
        &["ok refs/heads/keep", "ok refs/heads/topic"],
    )?;
    // Without the setting it is decided before the deletion, and nothing is needed as the reference is still there.
    let without = Push {
        setup: deleted.setup.iter().copied().filter(|setup| *setup != UPDATE).collect(),
        ..deleted.clone()
    };
    same_all(
        &world,
        "reached-by-what-is-deleted-without-the-setting",
        &without,
        &["ok refs/heads/keep", "ok refs/heads/topic"],
    )
}

#[test]
fn every_command_of_a_push_that_needs_the_boundary_is_applied_unlike_in_git() -> TestResult {
    // Git gives the check of the command that it applies a pointer to the command and not the command, which makes it
    // check the one after it, or none, and forget about those that follow: pushing two new branches where the first needs
    // the boundary ends with a report that has only the first. Each command is checked here, and each is told.
    let world = world()?;
    let format = "files";
    let push = with_config(
        shallow_at(
            to(
                "empty.git",
                vec![Cmd("0", "side", "refs/heads/side2"), Cmd("0", "on-c3", "refs/heads/t1")],
                vec!["side", "on-c3", "^main~3"],
            ),
            &["side", "main~1"],
        ),
        UPDATE,
    );
    let remote = world.remote(format, "two-needing-the-boundary", &push)?;
    let fork = fork_receive(&remote, &world.request(format, &push)?, true)?;
    let report = String::from_utf8_lossy(&fork.report);
    assert!(
        report.contains("ok refs/heads/side2") && report.contains("ok refs/heads/t1"),
        "both are updated and told: {report}"
    );
    assert_eq!(
        lines(std::fs::read_to_string(remote.join("shallow")).ok()),
        [world.id(format, "side")?, world.id(format, "main~1")?],
        "the file has what each command needed, in the order of the commands"
    );
    Ok(())
}

fn hooks_of_a_receive() -> Vec<Hook> {
    ["pre-receive", "update", "post-receive", "post-update"]
        .into_iter()
        .map(Hook::new)
        .collect()
}

#[test]
fn hooks_are_told_about_the_shallow_commits_of_the_client_in_a_file_when_the_receiver_is_to_update() -> TestResult {
    let world = world()?;
    let with_hooks = |mut push: Push| {
        push.hooks = hooks_of_a_receive();
        with_config(push, UPDATE)
    };
    // Whether it is needed or not, as long as something of what the client said is there.
    same(
        &world,
        "hooks-needed",
        &with_hooks(shallow_at(
            to(
                "empty.git",
                vec![Cmd("0", "ff", "refs/heads/main")],
                vec!["ff", "^main~1"],
            ),
            &["main"],
        )),
        OK_MAIN,
    )?;
    same(
        &world,
        "hooks-not-needed",
        &with_hooks(shallow_at(
            to(
                "full.git",
                vec![Cmd("main", "ff", "refs/heads/main")],
                vec!["ff", "^main"],
            ),
            &["main"],
        )),
        OK_MAIN,
    )?;
    // Nothing of it is there, so Git doesn't mean to update, and there is no file.
    same(
        &world,
        "hooks-unknown-line",
        &with_hooks(shallow_at(
            to(
                "full.git",
                vec![Cmd("main", "ff", "refs/heads/main")],
                vec!["ff", "^main"],
            ),
            &["main~3"],
        )),
        OK_MAIN,
    )?;
    // The file has what the receiver has, and then what the client said as it said it, whether it is there or not and twice.
    same(
        &world,
        "hooks-what-the-client-said",
        &with_hooks(shallow_at(
            to(
                "remote.git",
                vec![Cmd("0", "side", "refs/heads/side2")],
                vec!["side", "^main"],
            ),
            &["side", "main~3", "side"],
        )),
        "ok refs/heads/side2",
    )?;
    // The `update` hook decides before anything is written.
    let mut declined = to(
        "empty.git",
        vec![Cmd("0", "ff", "refs/heads/main")],
        vec!["ff", "^main~1"],
    );
    declined.hooks = vec![Hook::new("update").declining("refs/heads/main")];
    same(
        &world,
        "declined-by-the-hook",
        &with_config(shallow_at(declined, &["main"]), UPDATE),
        "ng refs/heads/main hook declined",
    )
}

fn lock_the_shallow_file(remote: &Path, _client: &Path) -> TestResult {
    let git_dir = Path::new(gix_testtools::git(remote, "rev-parse --absolute-git-dir")?.trim()).to_owned();
    std::fs::write(git_dir.join("shallow.lock"), "")?;
    Ok(())
}

#[test]
fn a_shallow_file_that_cannot_be_updated_fails_the_command_instead_of_the_receiver_unlike_git() -> TestResult {
    // Git dies if the file changed after it read it, or is locked, which ends the push without a report. The command that needs
    // the file is rejected here, and what it said is told.
    let world = world()?;
    let format = "files";
    let bringing_the_boundary = || {
        shallow_at(
            with_config(
                to(
                    "empty.git",
                    vec![Cmd("0", "ff", "refs/heads/main")],
                    vec!["ff", "^main~1"],
                ),
                UPDATE,
            ),
            &["main"],
        )
    };
    let locked = Push {
        prepare: Some(lock_the_shallow_file),
        ..bringing_the_boundary()
    };
    let changed = Push {
        hooks: vec![Hook::new("update").running("echo changed > ./shallow")],
        ..bringing_the_boundary()
    };
    for (label, push, reason) in [
        ("locked", locked, "Could not lock the shallow file"),
        ("changed", changed, "the shallow file has changed since it was read"),
    ] {
        let remote = world.remote(format, label, &push)?;
        let fork = fork_receive(&remote, &world.request(format, &push)?, true)?;
        let report = String::from_utf8_lossy(&fork.report);
        assert!(
            report.contains("ng refs/heads/main shallow error"),
            "{label}: the command is rejected: {report}"
        );
        assert!(
            fork.messages.contains(reason),
            "{label}: and it is told why: {}",
            fork.messages
        );
        assert!(
            !fork.refs.iter().any(|line| line.starts_with("refs/heads/main ")),
            "{label}: and nothing is updated"
        );
    }
    Ok(())
}

/// The receiver has the commit `c4` of the client, and not its tree, as if objects were lost.
fn only_the_commit_of_the_boundary(remote: &Path, client: &Path) -> TestResult {
    let commit = gix_testtools::git_command(client)
        .args(["cat-file", "commit", "main"])
        .output()?;
    assert!(commit.status.success(), "the commit can be read");
    let mut write = gix_testtools::git_command(remote)
        .args(["hash-object", "-w", "-t", "commit", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    write.stdin.take().expect("piped").write_all(&commit.stdout)?;
    assert!(write.wait()?.success(), "the commit can be written");
    Ok(())
}

#[test]
fn a_boundary_that_does_not_connect_fails_the_command_and_leaves_the_file_alone_unlike_git() -> TestResult {
    // Git accepts this push, as it checks the wrong command, and then has a repository with a commit that lacks its tree and
    // is a root.
    let world = world()?;
    let format = "files";
    let push = Push {
        prepare: Some(only_the_commit_of_the_boundary),
        ..shallow_at(
            with_config(
                to(
                    "empty.git",
                    vec![Cmd("0", "ff", "refs/heads/main")],
                    vec!["ff", "^main"],
                ),
                UPDATE,
            ),
            &["main"],
        )
    };
    let remote = world.remote(format, "boundary-without-tree", &push)?;
    let fork = fork_receive(&remote, &world.request(format, &push)?, true)?;
    let report = String::from_utf8_lossy(&fork.report);
    assert!(
        report.contains("ng refs/heads/main shallow error"),
        "the command is rejected: {report}"
    );
    assert!(!remote.join("shallow").exists(), "no commit became a boundary");
    assert!(!remote.join("shallow.lock").exists(), "and the lock was let go of");
    Ok(())
}
