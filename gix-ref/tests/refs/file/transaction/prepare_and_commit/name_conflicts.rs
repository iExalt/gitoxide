//! References whose names conflict, as `refs/heads/a` and `refs/heads/a/b` do.
//!
//! Each case runs against Git first, which refuses the transaction while preparing it, and then against the files store,
//! which has to refuse it in the same way and with the same leftovers: nothing changed, and nothing that Git can't
//! read afterwards. Git's message is compared too, as it is what the typed error stands in for.
use std::{
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
    process::Stdio,
};

use gix_hash::ObjectId;
use gix_lock::acquire::Fail;
use gix_ref::{
    Target,
    transaction::{NameConflict, PreviousValue, RefEdit},
};
use gix_testtools::TestResult;

/// A repository that Git initialised, and the id of a commit in it that references can point to.
fn repo(root: &Path, name: &str) -> TestResult<(PathBuf, ObjectId)> {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo)?;
    gix_testtools::git(&repo, "init -q")?;
    // The object format follows the environment of the tests, so the empty tree has to be written to know its id.
    let tree = gix_testtools::git_command(&repo)
        .args(["hash-object", "-w", "-t", "tree", "--stdin"])
        .stdin(Stdio::null())
        .output()?;
    assert!(tree.status.success(), "writing the empty tree failed");
    let tree = String::from_utf8(tree.stdout)?;
    let mut cmd = gix_testtools::git_command(&repo);
    cmd.args(["commit-tree", tree.trim(), "-m", "c"]);
    for who in ["AUTHOR", "COMMITTER"] {
        cmd.env(format!("GIT_{who}_NAME"), "committer")
            .env(format!("GIT_{who}_EMAIL"), "committer@example.com")
            .env(format!("GIT_{who}_DATE"), "@1234 +0800");
    }
    let out = cmd.output()?;
    assert!(
        out.status.success(),
        "commit-tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = ObjectId::from_hex(String::from_utf8(out.stdout)?.trim().as_bytes())?;
    Ok((repo, id))
}

fn git_ok(repo: &Path, args: &[&str]) -> TestResult<String> {
    let out = gix_testtools::git_command(repo).args(args).output()?;
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?)
}

/// Prepare the existing references, as `(name, packed)`, and write `garbage` to the file of each name in it.
fn prepare_state(repo: &Path, id: ObjectId, existing: &[(&str, bool)], garbage: &[&str]) -> TestResult {
    for (name, _) in existing {
        git_ok(repo, &["update-ref", name, &id.to_string()])?;
    }
    if existing.iter().any(|(_, packed)| *packed) {
        git_ok(repo, &["pack-refs", "--all", "--prune"])?;
    }
    for name in garbage {
        let path = repo.join(".git").join(name);
        std::fs::create_dir_all(path.parent().expect("a file below refs/"))?;
        std::fs::write(path, "garbage\n")?;
    }
    Ok(())
}

/// Create each reference in `names` with `git update-ref --stdin` in one transaction, as `create` or as `delete` for
/// names that are prefixed with `-`. Return the exit code and the message of the failure.
fn git_transaction(repo: &Path, id: ObjectId, edits: &[&str]) -> TestResult<(Option<i32>, String)> {
    let mut input = String::from("start\n");
    for edit in edits {
        match edit.strip_prefix('-') {
            Some(name) => writeln!(input, "delete {name}")?,
            None => writeln!(input, "create {edit} {id}")?,
        }
    }
    input.push_str("prepare\ncommit\n");
    let mut child = gix_testtools::git_command(repo)
        .args(["update-ref", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().expect("piped").write_all(input.as_bytes())?;
    let out = child.wait_with_output()?;
    let stderr = String::from_utf8(out.stderr)?;
    Ok((out.status.code(), stderr.trim().to_owned()))
}

/// What the files store does for the same edits, like [`git_transaction()`] describes them.
fn fork_transaction(repo: &Path, id: ObjectId, edits: &[&str]) -> Result<(), gix_error::Error> {
    let store = gix_ref::file::Store::at(repo.join(".git"), crate::fixture_hash_kind());
    let edits = edits.iter().map(|edit| match edit.strip_prefix('-') {
        Some(name) => RefEdit::delete(name.try_into().expect("valid"), PreviousValue::Any),
        None => RefEdit::update(
            (*edit).try_into().expect("valid"),
            Target::Object(id),
            PreviousValue::MustNotExist,
            "",
        ),
    });
    let prepared = store
        .transaction()
        .prepare(edits, Fail::Immediately, Fail::Immediately)?;
    prepared.commit(Some(super::committer().to_ref(&mut Default::default())))?;
    Ok(())
}

/// List files below `refs/` and `logs/`, with their content, to see if anything changed. Directories that are left
/// empty aren't part of it as they don't change what references there are.
fn dump(repo: &Path) -> TestResult<Vec<String>> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root).expect("below root").display().to_string();
            if path.is_dir() {
                walk(&path, root, out)?;
            } else {
                out.push(format!("{relative}: {}", std::fs::read_to_string(&path)?.trim()));
            }
        }
        Ok(())
    }
    let git_dir = repo.join(".git");
    let mut out = Vec::new();
    for dir in ["refs", "logs"] {
        walk(&git_dir.join(dir), &git_dir, &mut out)?;
    }
    if let Ok(packed) = std::fs::read_to_string(git_dir.join("packed-refs")) {
        out.push(format!("packed-refs: {packed}"));
    }
    Ok(out)
}

struct Case {
    /// Existing references and whether they are packed.
    existing: &'static [(&'static str, bool)],
    /// Edits, with a leading `-` for a deletion.
    edits: &'static [&'static str],
    /// The names of the conflict, Git's message, and if the other name is edited by the transaction as well.
    full_name: &'static str,
    other: &'static str,
    in_transaction: bool,
}

#[test]
fn conflicts_are_refused_while_preparing_like_git_does() -> TestResult {
    const CASES: &[(&str, Case)] = &[
        (
            "a loose reference where a directory is needed",
            Case {
                existing: &[("refs/heads/a", false)],
                edits: &["refs/heads/a/b"],
                full_name: "refs/heads/a/b",
                other: "refs/heads/a",
                in_transaction: false,
            },
        ),
        (
            "a packed reference where a directory is needed, which only the packed-refs file knows about",
            Case {
                existing: &[("refs/heads/a", true)],
                edits: &["refs/heads/a/b"],
                full_name: "refs/heads/a/b",
                other: "refs/heads/a",
                in_transaction: false,
            },
        ),
        (
            "a loose reference below the name",
            Case {
                existing: &[("refs/heads/a/b", false)],
                edits: &["refs/heads/a"],
                full_name: "refs/heads/a",
                other: "refs/heads/a/b",
                in_transaction: false,
            },
        ),
        (
            "a packed reference below the name",
            Case {
                existing: &[("refs/heads/a/b/c", true)],
                edits: &["refs/heads/a"],
                full_name: "refs/heads/a",
                other: "refs/heads/a/b/c",
                in_transaction: false,
            },
        ),
        (
            "two creations in one transaction",
            Case {
                existing: &[],
                edits: &["refs/heads/q", "refs/heads/q/r"],
                full_name: "refs/heads/q",
                other: "refs/heads/q/r",
                in_transaction: true,
            },
        ),
        (
            "a deletion of the file that makes way for a directory, which Git refuses just the same",
            Case {
                existing: &[("refs/heads/a", true)],
                edits: &["-refs/heads/a", "refs/heads/a/b"],
                full_name: "refs/heads/a/b",
                other: "refs/heads/a",
                in_transaction: false,
            },
        ),
    ];

    let root = gix_testtools::tempfile::tempdir()?;
    for (idx, (description, case)) in CASES.iter().enumerate() {
        let (git_repo, id) = repo(root.path(), &format!("git-{idx}"))?;
        let (fork_repo, _) = repo(root.path(), &format!("fork-{idx}"))?;
        for repo in [&git_repo, &fork_repo] {
            prepare_state(repo, id, case.existing, &[])?;
        }
        let before = dump(&fork_repo)?;
        assert_eq!(before, dump(&git_repo)?, "{description}: both start out the same");

        let (code, git_message) = git_transaction(&git_repo, id, case.edits)?;
        assert_eq!(code, Some(128), "{description}: Git refuses the transaction");

        let err = fork_transaction(&fork_repo, id, case.edits)
            .expect_err("the conflicting names have to fail the transaction while it is prepared");
        let conflict = err
            .downcast_any_ref::<NameConflict>()
            .unwrap_or_else(|| panic!("{description}: the failure is identifiable: {err:?}"));
        assert_eq!(
            (
                conflict.full_name.as_slice(),
                conflict.other.as_slice(),
                conflict.in_transaction
            ),
            (case.full_name.as_bytes(), case.other.as_bytes(), case.in_transaction),
            "{description}: the names of the conflict"
        );
        assert!(
            git_message.contains(&conflict.to_string()),
            "{description}: the message of the error is Git's: {conflict} vs {git_message}"
        );
        assert!(
            err.is_conflict(),
            "{description}: it is a conflict, not a failure to retry"
        );
        assert!(
            err.downcast_any_ref::<gix_ref::transaction::commit_failure::Uncertain>()
                .is_none(),
            "{description}: nothing was attempted, so it isn't uncertain"
        );
        assert_eq!(
            dump(&fork_repo)?,
            before,
            "{description}: refused while preparing, so no file, reflog or lock is left behind"
        );
        assert!(
            !fork_repo.join(".git/packed-refs.lock").exists(),
            "{description}: the lock of the packed-refs file is released"
        );
        assert_eq!(
            dump(&git_repo)?,
            before,
            "{description}: Git leaves nothing behind either"
        );
        git_ok(&fork_repo, &["for-each-ref"])?;
        git_ok(&fork_repo, &["fsck", "--no-dangling"])?;
    }
    Ok(())
}

#[test]
fn names_that_only_look_alike_can_be_created() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (repo, id) = repo(root.path(), "fork")?;
    prepare_state(&repo, id, &[("refs/heads/a-b", true), ("refs/heads/ab", false)], &[])?;
    fork_transaction(&repo, id, &["refs/heads/a", "refs/heads/a.b", "refs/tags/a/b"])?;
    let listed = git_ok(&repo, &["for-each-ref", "--format=%(refname)"])?;
    assert_eq!(
        listed.lines().collect::<Vec<_>>(),
        [
            "refs/heads/a",
            "refs/heads/a-b",
            "refs/heads/a.b",
            "refs/heads/ab",
            "refs/tags/a/b"
        ],
        "a shared prefix isn't a directory unless it ends at a slash"
    );
    git_ok(&repo, &["pack-refs", "--all"])?;
    Ok(())
}

#[test]
fn two_edits_of_one_name_are_invalid_like_in_git() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let (git_repo, id) = repo(root.path(), "git")?;
    let (fork_repo, _) = repo(root.path(), "fork")?;

    let edits = ["refs/heads/a", "refs/heads/a"];
    let (code, git_message) = git_transaction(&git_repo, id, &edits)?;
    assert_eq!(code, Some(128), "Git refuses to process a name twice: {git_message}");

    let err = fork_transaction(&fork_repo, id, &edits).expect_err("a name can only be edited once");
    assert!(
        err.is_validation(),
        "it is a mistake of the caller, which can't be fixed by trying again: {err:?}"
    );
    assert_eq!(dump(&fork_repo)?, dump(&git_repo)?, "nothing is left behind by either");
    Ok(())
}
