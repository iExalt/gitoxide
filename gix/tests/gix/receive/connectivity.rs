//! The check of what a push brings is compared with `git rev-list --objects --stdin --not --exclude-hidden=receive --all --alternate-refs`, which
//! is what `receive-pack` asks. The scenarios are made by `make_connectivity_repos.sh`, which says what they break.
use std::{io::Write, process::Stdio};

use gix::receive::connectivity::{Cutoffs, verify};
use gix_testtools::TestResult;

/// The scenarios and whether the new commit is accepted.
const SCENARIOS: &[(&str, bool)] = &[
    ("ok", true),
    ("blob_missing", false),
    ("subtree_missing", false),
    ("root_tree_missing", false),
    ("blob_damaged", true),
    ("tree_damaged", false),
    ("reused_blob_missing", true),
    ("reused_subtree_missing", true),
    ("parents_tree_missing", true),
    ("grandparent_tree_missing", true),
    ("grandparent_blob_missing", true),
    ("grandparent_missing", true),
    ("parent_missing", false),
    ("parent_not_referenced", true),
    ("reference_broken", false),
    ("hidden_complete", true),
    ("hidden_blob_missing", false),
    ("hidden_tree_missing", false),
    ("hidden_blob_missing_but_not_hidden", true),
    ("tip_is_referenced", true),
    ("tag_ok", true),
    ("tag_target_missing", false),
    ("tip_is_blob", true),
    ("tip_is_tree", true),
    ("commit_graph", true),
    ("commit_graph_blob_missing", false),
    ("commit_graph_parent_missing", true),
    ("alternate_user", true),
    ("alternate_user_new_blob_missing", false),
    ("alternate_detached_user", false),
];

fn git_accepts(dir: &std::path::Path, tip: &str, hidden: &[String]) -> TestResult<bool> {
    let mut cmd = gix_testtools::git_command(dir);
    for hidden in hidden {
        cmd.args(["-c", &format!("receive.hideRefs={hidden}")]);
    }
    let mut child = cmd
        .args([
            "rev-list",
            "--objects",
            "--stdin",
            "--not",
            "--exclude-hidden=receive",
            "--all",
            "--quiet",
            "--alternate-refs",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    writeln!(child.stdin.take().expect("piped"), "{tip}")?;
    Ok(child.wait()?.success())
}

fn fork_accepts(dir: &std::path::Path, tip: &str, hidden: &[String]) -> TestResult<bool> {
    let repo = gix::open_opts(dir, gix::open::Options::isolated())?;
    let is_hidden = |name: &gix::bstr::BStr| hidden.iter().any(|hidden| name == hidden.as_bytes());
    let Ok(cutoffs) = Cutoffs::collect(&repo, &is_hidden) else {
        return Ok(false);
    };
    let tip = gix::hash::ObjectId::from_hex(tip.as_bytes())?;
    Ok(verify(&repo, &cutoffs, &[tip]).is_ok())
}

#[test]
fn verdicts_are_those_of_rev_list() -> TestResult {
    let root = gix_testtools::scripted_fixture_read_only("make_connectivity_repos.sh")?;
    for (name, expected) in SCENARIOS {
        let dir = root.join(name);
        let tip = std::fs::read_to_string(dir.join(".git/tip"))?.trim().to_owned();
        let hidden: Vec<String> = std::fs::read_to_string(dir.join(".git/hide"))
            .unwrap_or_default()
            .lines()
            .map(ToOwned::to_owned)
            .collect();
        assert_eq!(
            git_accepts(&dir, &tip, &hidden)?,
            *expected,
            "{name}: Git's verdict is what the table says, or the scenario doesn't test what it should"
        );
        assert_eq!(
            fork_accepts(&dir, &tip, &hidden)?,
            *expected,
            "{name}: the verdict of the check is that of Git"
        );
    }
    Ok(())
}
