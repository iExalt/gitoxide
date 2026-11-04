//! The reflog of `HEAD` records updates of the branch that `HEAD` points at, even though no edit names `HEAD`.
//!
//! Every case runs the same change with Git and with the files store in repositories that start out the same, and
//! compares the reflogs of `HEAD` and of the branch byte for byte, as the identity and the time are the same on both sides.
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use gix_hash::ObjectId;
use gix_lock::acquire::Fail;
use gix_ref::{
    Target,
    transaction::{HeadLog, PreviousValue, RefEdit},
};
use gix_testtools::TestResult;

/// A repository that Git initialised on `main`, and ids of two commits in it, one the child of the other.
fn repo(root: &Path, name: &str) -> TestResult<(PathBuf, [ObjectId; 2])> {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo)?;
    gix_testtools::git(&repo, "init -q -b main")?;
    let tree = gix_testtools::git_command(&repo)
        .args(["hash-object", "-w", "-t", "tree", "--stdin"])
        .stdin(Stdio::null())
        .output()?;
    assert!(tree.status.success(), "writing the empty tree failed");
    let tree = String::from_utf8(tree.stdout)?.trim().to_owned();
    let commit = |parent: Option<ObjectId>| -> TestResult<ObjectId> {
        let mut cmd = gix_testtools::git_command(&repo);
        cmd.args(["commit-tree", &tree, "-m", "c"]);
        if let Some(parent) = parent {
            cmd.args(["-p", &parent.to_string()]);
        }
        let out = identity(cmd).output()?;
        assert!(out.status.success(), "commit-tree failed");
        Ok(ObjectId::from_hex(String::from_utf8(out.stdout)?.trim().as_bytes())?)
    };
    let first = commit(None)?;
    let second = commit(Some(first))?;
    Ok((repo, [first, second]))
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

fn git_update_ref(repo: &Path, args: &[&str]) -> TestResult<std::process::Output> {
    let mut cmd = gix_testtools::git_command(repo);
    cmd.arg("update-ref").args(args);
    Ok(identity(cmd).output()?)
}

fn fork(repo: &Path, edits: impl IntoIterator<Item = RefEdit>) -> Result<(), gix_error::Error> {
    fork_with(repo, edits, HeadLog::Git)
}

fn fork_with(repo: &Path, edits: impl IntoIterator<Item = RefEdit>, head_log: HeadLog) -> Result<(), gix_error::Error> {
    let store = gix_ref::file::Store::at(repo.join(".git"), crate::fixture_hash_kind());
    store
        .transaction()
        .head_log(head_log)
        .prepare(edits, Fail::Immediately, Fail::Immediately)?
        .commit(super::committer().to_ref(&mut Default::default()))?;
    Ok(())
}

fn update(name: &str, to: ObjectId, deref: bool) -> RefEdit {
    RefEdit::update(
        name.try_into().expect("valid"),
        Target::Object(to),
        PreviousValue::Any,
        "m",
    )
    .with_deref(deref)
}

/// The reflogs that exist below `logs/`, with their content, as the reference that these edits can change.
fn reflogs(repo: &Path) -> TestResult<Vec<(String, String)>> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) -> std::io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out)?;
            } else {
                out.push((
                    path.strip_prefix(root).expect("below root").display().to_string(),
                    std::fs::read_to_string(&path)?,
                ));
            }
        }
        Ok(())
    }
    let logs = repo.join(".git/logs");
    let mut out = Vec::new();
    walk(&logs, &logs, &mut out)?;
    Ok(out)
}

/// Run `steps`, each of which is a Git command and the edits that do the same with the fork, in both repositories
/// and compare the reflogs after every step.
fn same_reflogs_as_git(steps: &[(&[&str], Vec<RefEdit>)]) -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (git_repo, _) = repo(root.path(), "git")?;
    let (fork_repo, _) = repo(root.path(), "fork")?;
    for (idx, (git_args, edits)) in steps.iter().enumerate() {
        let out = git_update_ref(&git_repo, git_args)?;
        assert!(
            out.status.success(),
            "step {idx}: git {git_args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        fork(&fork_repo, edits.clone())?;
        assert_eq!(
            reflogs(&fork_repo)?,
            reflogs(&git_repo)?,
            "step {idx} ({git_args:?}): the reflogs are those of Git, which includes the one of HEAD"
        );
    }
    Ok(())
}

#[test]
fn updating_the_branch_that_head_points_at_logs_to_head_as_well() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (_, [first, second]) = repo(root.path(), "ids")?;
    let (first, second) = (first.to_string(), second.to_string());
    // The ids are those of the repositories that `same_reflogs_as_git()` makes, as these are made the same way.
    same_reflogs_as_git(&[
        (
            &["-m", "m", "refs/heads/main", &first],
            vec![update("refs/heads/main", ObjectId::from_hex(first.as_bytes())?, false)],
        ),
        (
            &["-m", "m", "refs/heads/main", &second],
            vec![update("refs/heads/main", ObjectId::from_hex(second.as_bytes())?, false)],
        ),
    ])
}

#[test]
fn an_update_that_changes_nothing_is_logged_by_head_only() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (_, [first, _]) = repo(root.path(), "ids")?;
    let first_hex = first.to_string();
    let to_first = || update("refs/heads/main", first, false);
    same_reflogs_as_git(&[
        (&["-m", "m", "refs/heads/main", &first_hex], vec![to_first()]),
        (&["-m", "m", "refs/heads/main", &first_hex], vec![to_first()]),
        (
            &["-m", "m", "refs/heads/other", &first_hex],
            vec![update("refs/heads/other", first, false)],
        ),
        (
            &["-m", "m", "refs/heads/other", &first_hex],
            vec![update("refs/heads/other", first, false)],
        ),
    ])
}

#[test]
fn updating_through_head_logs_even_if_nothing_changes() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (_, [first, second]) = repo(root.path(), "ids")?;
    let (first_hex, second_hex) = (first.to_string(), second.to_string());
    same_reflogs_as_git(&[
        (&["-m", "m", "HEAD", &first_hex], vec![update("HEAD", first, true)]),
        (&["-m", "m", "HEAD", &second_hex], vec![update("HEAD", second, true)]),
        (&["-m", "m", "HEAD", &second_hex], vec![update("HEAD", second, true)]),
    ])
}

#[test]
fn deleting_the_branch_that_head_points_at_logs_the_deletion_to_head() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (_, [first, _]) = repo(root.path(), "ids")?;
    let first_hex = first.to_string();
    same_reflogs_as_git(&[
        (
            &["-m", "m", "refs/heads/main", &first_hex],
            vec![update("refs/heads/main", first, false)],
        ),
        (
            &["-d", "refs/heads/main"],
            vec![RefEdit::delete_with_log(
                "refs/heads/main".try_into()?,
                PreviousValue::Any,
                gix_ref::transaction::RefLog::AndReference,
            )],
        ),
    ])
}

fn verification(name: &str, value: Target) -> RefEdit {
    RefEdit::update(
        name.try_into().expect("valid"),
        value.clone(),
        PreviousValue::MustExistAndMatch(value),
        "m",
    )
}

#[test]
fn an_update_that_expects_what_it_writes_is_logged_in_head_only_like_in_git() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (_, [first, _]) = repo(root.path(), "ids")?;
    let first_hex = first.to_string();
    same_reflogs_as_git(&[
        (
            &["-m", "m", "refs/heads/main", &first_hex],
            vec![update("refs/heads/main", first, false)],
        ),
        (
            &["-m", "m", "refs/heads/main", &first_hex, &first_hex],
            vec![verification("refs/heads/main", Target::Object(first))],
        ),
    ])
}

#[test]
fn editing_head_and_its_branch_in_one_transaction_is_refused_like_in_git() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (git_repo, [first, _]) = repo(root.path(), "git")?;
    let (fork_repo, _) = repo(root.path(), "fork")?;
    git_update_ref(&git_repo, &["refs/heads/main", &first.to_string()])?;
    fork(&fork_repo, [update("refs/heads/main", first, false)])?;
    let before = reflogs(&fork_repo)?;

    let mut git = gix_testtools::git_command(&git_repo);
    git.args(["update-ref", "--stdin"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = identity(git).spawn()?;
    std::io::Write::write_all(
        child.stdin.as_mut().expect("piped"),
        format!("start\nupdate refs/heads/main {first}\nupdate HEAD {first}\nprepare\ncommit\n").as_bytes(),
    )?;
    let out = child.wait_with_output()?;
    let git_message = String::from_utf8(out.stderr)?;
    assert_eq!(out.status.code(), Some(128), "Git refuses: {git_message}");

    let err = fork(
        &fork_repo,
        [update("refs/heads/main", first, false), update("HEAD", first, false)],
    )
    .expect_err("HEAD would be changed twice");
    assert!(err.is_conflict(), "{err:?}");
    let reason = "multiple updates for 'HEAD' (including one via its referent 'refs/heads/main') are not allowed";
    assert!(
        git_message.contains(reason) && format!("{err:?}").contains(reason),
        "both give the same reason: {git_message} vs {err:?}"
    );
    assert_eq!(reflogs(&fork_repo)?, before, "nothing was logged");
    Ok(())
}

/// Callers that move a branch while they re-attach `HEAD` have no atomic alternative to what Git refuses, so they can
/// ask for `HEAD` to log only what its own edit asks for, instead of the entry that the change of the branch would add.
#[test]
fn the_caller_can_edit_head_and_its_branch_in_one_transaction() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (repo, [first, second]) = repo(root.path(), "fork")?;
    fork(&repo, [update("refs/heads/main", first, false)])?;
    let head_log_before = reflogs(&repo)?.into_iter().find(|(name, _)| name == "HEAD");

    fork_with(
        &repo,
        [
            update("refs/heads/main", second, false),
            RefEdit::update(
                "HEAD".try_into()?,
                Target::Symbolic("refs/heads/other".try_into()?),
                PreviousValue::Any,
                "m",
            ),
        ],
        HeadLog::CallerDecides,
    )?;
    let logs = reflogs(&repo)?;
    assert_eq!(
        logs.iter().find(|(name, _)| name == "HEAD"),
        head_log_before.as_ref(),
        "re-attaching HEAD writes no entry, and the change of the branch isn't one for HEAD any more"
    );
    let branch_log = &logs
        .iter()
        .find(|(name, _)| name == "refs/heads/main")
        .expect("the branch has a reflog")
        .1;
    assert_eq!(branch_log.lines().count(), 2, "the branch logs its own change");
    Ok(())
}

/// An edit that expects exactly the value it writes verifies a reference while other references change, as callers
/// use it to guard what they leave as it is. If the caller decides, it is no change of `HEAD` and writes no log entry.
#[test]
fn the_caller_can_guard_references_without_logging() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (repo, [first, second]) = repo(root.path(), "fork")?;
    fork(&repo, [update("refs/heads/main", first, false)])?;

    let before = reflogs(&repo)?;
    fork_with(
        &repo,
        [verification("refs/heads/main", Target::Object(first))],
        HeadLog::CallerDecides,
    )?;
    assert_eq!(
        reflogs(&repo)?,
        before,
        "guarding the branch logs nothing, also not in HEAD"
    );

    fork_with(
        &repo,
        [
            verification("HEAD", Target::Symbolic("refs/heads/main".try_into()?)),
            update("refs/heads/main", second, false),
        ],
        HeadLog::CallerDecides,
    )?;
    let after = reflogs(&repo)?;
    let head_log = &after
        .iter()
        .find(|(name, _)| name == "HEAD")
        .expect("HEAD has a reflog")
        .1;
    assert_eq!(
        head_log.lines().count(),
        2,
        "the branch change is logged once in HEAD, and the guard of HEAD isn't a second change: {head_log}"
    );
    Ok(())
}

/// What callers ask for if the branch may exist already, as fetching does, doesn't make the branch look like it
/// existed: the log of `HEAD` starts from the null id when the branch is created.
#[test]
fn creating_the_branch_with_a_lenient_expectation_starts_the_log_of_head_from_the_null_id() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (_, [first, _]) = repo(root.path(), "ids")?;
    let first_hex = first.to_string();
    same_reflogs_as_git(&[(
        &["-m", "m", "refs/heads/main", &first_hex],
        vec![RefEdit::update(
            "refs/heads/main".try_into()?,
            Target::Object(first),
            PreviousValue::ExistingMustMatch(Target::Object(first)),
            "m",
        )],
    )])
}

#[test]
fn a_branch_that_only_exists_in_packed_refs_starts_the_log_of_head_from_its_packed_value() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (git_repo, [first, second]) = repo(root.path(), "git")?;
    let (fork_repo, _) = repo(root.path(), "fork")?;
    git_update_ref(&git_repo, &["-m", "m", "refs/heads/main", &first.to_string()])?;
    fork(&fork_repo, [update("refs/heads/main", first, false)])?;
    for repo in [&git_repo, &fork_repo] {
        gix_testtools::git(repo, "pack-refs --all --prune")?;
        assert!(!repo.join(".git/refs/heads/main").exists(), "the loose branch is gone");
    }

    let out = git_update_ref(&git_repo, &["-m", "m", "refs/heads/main", &second.to_string()])?;
    assert!(out.status.success());
    fork(&fork_repo, [update("refs/heads/main", second, false)])?;
    assert_eq!(
        reflogs(&fork_repo)?,
        reflogs(&git_repo)?,
        "HEAD and the branch log what Git logs, starting from the packed value"
    );
    Ok(())
}

/// The entries of `HEAD` that are added aren't edits that the caller asked for, so observers, rollbacks and results
/// don't see them.
#[test]
fn the_log_entries_of_head_are_not_edits_of_the_transaction() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (repo, [first, _]) = repo(root.path(), "fork")?;
    let store = gix_ref::file::Store::at(repo.join(".git"), crate::fixture_hash_kind());
    let seen = std::cell::RefCell::new(Vec::<String>::new());
    let observer = |event: &gix_ref::transaction::observe::Event<'_>| {
        seen.borrow_mut()
            .extend(event.updates.iter().map(|update| update.name.as_bstr().to_string()));
        Ok(())
    };

    let rolled_back = store
        .transaction()
        .observer(observer)
        .prepare(
            [update("refs/heads/main", first, false)],
            Fail::Immediately,
            Fail::Immediately,
        )?
        .rollback();
    assert_eq!(rolled_back.len(), 1, "a rollback returns the edits that were asked for");
    assert!(reflogs(&repo)?.is_empty(), "and writes nothing");

    let committed = store
        .transaction()
        .observer(observer)
        .prepare(
            [update("refs/heads/main", first, false)],
            Fail::Immediately,
            Fail::Immediately,
        )?
        .commit(super::committer().to_ref(&mut Default::default()))?;
    assert_eq!(committed.len(), 1, "a commit returns them as well");
    assert!(
        seen.borrow().iter().all(|name| name == "refs/heads/main"),
        "observers aren't told about HEAD: {:?}",
        seen.borrow()
    );
    assert!(
        reflogs(&repo)?.iter().any(|(name, _)| name == "HEAD"),
        "while HEAD is logged"
    );
    Ok(())
}

/// A no-op update of the branch writes no log of the branch, but one of `HEAD`, so that it needs an identity, like in Git.
#[test]
fn the_log_of_head_needs_an_identity_even_if_the_branch_has_no_change() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (repo, [first, _]) = repo(root.path(), "fork")?;
    fork(&repo, [update("refs/heads/main", first, false)])?;
    let before = reflogs(&repo)?;

    let store = gix_ref::file::Store::at(repo.join(".git"), crate::fixture_hash_kind());
    let err = store
        .transaction()
        .prepare(
            [update("refs/heads/main", first, false)],
            Fail::Immediately,
            Fail::Immediately,
        )?
        .commit(None)
        .expect_err("HEAD has to log it, which needs a committer");
    assert!(
        err.downcast_any_ref::<gix_ref::file::log::create_or_update::MissingCommitter>()
            .is_some(),
        "{err:?}"
    );
    assert_eq!(reflogs(&repo)?, before, "nothing was written");
    Ok(())
}
