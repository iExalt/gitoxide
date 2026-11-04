//! A reflog has one line per entry, so a message is written the way Git cleans it: without leading and trailing
//! whitespace, and with every run of whitespace, newlines included, as one space.
//!
//! Each message is written by Git and by the files store in repositories that start out the same, and the logs are
//! compared byte for byte, as the identity and the time are the same on both sides.
use std::{path::Path, process::Stdio};

use gix_hash::ObjectId;
use gix_lock::acquire::Fail;
use gix_ref::{
    Target,
    transaction::{PreviousValue, RefEdit},
};
use gix_testtools::TestResult;

/// A repository that Git initialised, and the id of a commit in it.
fn repo(root: &Path, name: &str) -> TestResult<(std::path::PathBuf, ObjectId)> {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo)?;
    gix_testtools::git(&repo, "init -q -b main")?;
    let tree = gix_testtools::git_command(&repo)
        .args(["hash-object", "-w", "-t", "tree", "--stdin"])
        .stdin(Stdio::null())
        .output()?;
    assert!(tree.status.success(), "writing the empty tree failed");
    let tree = String::from_utf8(tree.stdout)?.trim().to_owned();
    let mut cmd = gix_testtools::git_command(&repo);
    cmd.args(["commit-tree", &tree, "-m", "c"]);
    let out = identity(cmd).output()?;
    assert!(out.status.success(), "commit-tree failed");
    Ok((
        repo,
        ObjectId::from_hex(String::from_utf8(out.stdout)?.trim().as_bytes())?,
    ))
}

/// Make Git use the identity and time of [`committer()`](super::committer()).
fn identity(mut cmd: std::process::Command) -> std::process::Command {
    for who in ["AUTHOR", "COMMITTER"] {
        cmd.env(format!("GIT_{who}_NAME"), "committer")
            .env(format!("GIT_{who}_EMAIL"), "committer@example.com")
            .env(format!("GIT_{who}_DATE"), "@1234 +0800");
    }
    cmd
}

#[test]
fn messages_are_written_like_git_cleans_them() -> TestResult {
    let messages = [
        "plain",
        "  a \t b\n\nc  ",
        "tab\u{b}vertical\u{c}form\r\nfeed",
        "\n\nleading newlines",
        "trailing newlines\n\n",
        "   ",
        "no-break\u{a0}space and  é",
    ];
    for (idx, message) in messages.iter().enumerate() {
        let root = gix_testtools::tempfile::tempdir()?;
        let (git_repo, commit_id) = repo(root.path(), "git")?;
        let (fork_repo, _) = repo(root.path(), "fork")?;

        let mut git = gix_testtools::git_command(&git_repo);
        git.args(["update-ref", "-m", message, "refs/heads/side", &commit_id.to_string()]);
        let out = identity(git).output()?;
        assert!(
            out.status.success(),
            "case {idx}: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        gix_ref::file::Store::at(fork_repo.join(".git"), crate::fixture_hash_kind())
            .transaction()
            .prepare(
                [RefEdit::update(
                    "refs/heads/side".try_into()?,
                    Target::Object(commit_id),
                    PreviousValue::Any,
                    *message,
                )],
                Fail::Immediately,
                Fail::Immediately,
            )?
            .commit(super::committer().to_ref(&mut Default::default()))?;

        let log = |repo: &Path| std::fs::read(repo.join(".git/logs/refs/heads/side"));
        let (fork_log, git_log) = (log(&fork_repo)?, log(&git_repo)?);
        assert_eq!(
            fork_log,
            git_log,
            "case {idx}, message {message:?}: the log has what Git writes, and one line per entry: {:?}",
            String::from_utf8_lossy(&fork_log)
        );
    }
    Ok(())
}
