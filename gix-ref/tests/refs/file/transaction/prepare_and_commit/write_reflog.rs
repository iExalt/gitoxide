//! Which reflogs are created, and which ones are appended to, follows `core.logAllRefUpdates`, and Git is the reference.
//!
//! With `false`, Git doesn't create reflogs, but it keeps appending to the ones that exist. Even with the default, it
//! creates none for references that are private to a worktree.
use gix_lock::acquire::Fail;
use gix_ref::{store::WriteReflog, transaction::RefEdit};
use gix_testtools::TestResult;

use super::head_reflog::{git_update_ref, reflogs, repo, update};

fn fork(repo: &std::path::Path, mode: WriteReflog, edits: Vec<RefEdit>) -> Result<(), gix_error::Error> {
    let mut store = gix_ref::file::Store::at(repo.join(".git"), crate::fixture_hash_kind());
    store.write_reflog = mode;
    store
        .transaction()
        .prepare(edits, Fail::Immediately, Fail::Immediately)?
        .commit(super::committer().to_ref(&mut Default::default()))?;
    Ok(())
}

#[test]
fn without_logallrefupdates_existing_reflogs_are_appended_to_and_none_are_created() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (git_repo, [first, second]) = repo(root.path(), "git")?;
    let (fork_repo, _) = repo(root.path(), "fork")?;
    gix_testtools::git(&git_repo, "config core.logAllRefUpdates false")?;

    let out = git_update_ref(&git_repo, &["-m", "m", "refs/heads/main", &first.to_string()])?;
    assert!(out.status.success());
    fork(
        &fork_repo,
        WriteReflog::Existing,
        vec![update("refs/heads/main", first, false)],
    )?;
    assert_eq!(reflogs(&git_repo)?, [], "Git creates no reflog");
    assert_eq!(reflogs(&fork_repo)?, [], "and neither does the store");

    // Only the reflog of the branch exists, as Git would have left it with `true` before it was switched off.
    for repo in [&git_repo, &fork_repo] {
        std::fs::create_dir_all(repo.join(".git/logs/refs/heads"))?;
        std::fs::write(repo.join(".git/logs/refs/heads/main"), "")?;
    }
    let out = git_update_ref(&git_repo, &["-m", "m", "refs/heads/main", &second.to_string()])?;
    assert!(out.status.success());
    fork(
        &fork_repo,
        WriteReflog::Existing,
        vec![update("refs/heads/main", second, false)],
    )?;
    let logs = reflogs(&fork_repo)?;
    assert_eq!(
        logs.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
        ["refs/heads/main"],
        "the existing reflog is appended to, and HEAD gets none as it has none"
    );
    assert_eq!(logs, reflogs(&git_repo)?, "which is what Git writes, byte for byte");
    Ok(())
}

#[test]
fn references_that_are_private_to_a_worktree_get_no_reflog_unless_it_exists() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (git_repo, [first, _]) = repo(root.path(), "git")?;
    let (fork_repo, _) = repo(root.path(), "fork")?;

    let out = git_update_ref(&git_repo, &["-m", "m", "refs/worktree/private", &first.to_string()])?;
    assert!(out.status.success());
    fork(
        &fork_repo,
        WriteReflog::Normal,
        vec![update("refs/worktree/private", first, false)],
    )?;
    assert_eq!(reflogs(&git_repo)?, [], "Git creates no reflog for them");
    assert_eq!(reflogs(&fork_repo)?, [], "and neither does the store");
    Ok(())
}
