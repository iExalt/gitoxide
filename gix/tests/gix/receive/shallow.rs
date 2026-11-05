//! Receiving into a shallow repository: what it advertises and what it takes from a client that has the whole history, which
//! is compared with what `git receive-pack` does. `make_shallow_receivers.sh` says what the repositories and the branches
//! of the client are.
//!
//! The only commits without parents are the entries of the `shallow` file. Anything below them is missing, and a client that
//! assumes that the receiver has it, because it can reach it from what the receiver advertised, is told so.
use std::path::Path;

use gix_testtools::TestResult;

use super::receive_pack::{Cmd, Push, World, add_alternate_of_client, assert_advertisement_same, compare_on};

const FORMATS: &[&str] = &["files", "reftable"];

fn world() -> TestResult<World> {
    World::from_fixture("make_shallow_receivers.sh")
}

/// Receive `push` with Git and with the fork, and compare what they say and leave behind. Git has to tell `verdict`, so that no
/// row can pass by being refused for another reason than the one it is about.
fn same(world: &World, label: &str, push: &Push, verdict: &str) -> TestResult {
    for result in compare_on(world, label, push, FORMATS)? {
        let report = String::from_utf8_lossy(&result.report);
        assert!(
            report.contains(verdict),
            "{label}: Git is to tell {verdict:?}, but told: {report}"
        );
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
