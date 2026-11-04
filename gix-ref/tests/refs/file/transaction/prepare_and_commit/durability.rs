//! Compare what Git and the files store do to the file system for the same transaction, using `strace` on Linux:
//! which files are flushed, which are renamed, and what remains when the process dies or a call fails halfway.
//!
//! The fork's side runs in a re-executed copy of this test binary (`fork_child`), so that only it is traced.
//! Every test skips itself if `strace` can't trace here, unless `GIX_TEST_REQUIRE_STRACE` is set.
#![cfg(target_os = "linux")]
use std::{
    path::{Path, PathBuf},
    process::Command,
};

use gix_testtools::{
    TestResult,
    strace::{self, Inject, Trace},
};

const SCENARIO: &str = "GIX_REFS_STRACE_SCENARIO";
const REPO: &str = "GIX_REFS_STRACE_REPO";
const OUTCOME_FILE: &str = "fork-outcome.txt";
const FSYNC: &str = "GIX_REFS_STRACE_FSYNC";
const OID: &str = "GIX_REFS_STRACE_OID";
const NAMES: [&str; 3] = ["refs/heads/a", "refs/heads/b", "refs/heads/c"];

/// What the fork should do in the re-executed child. Doesn't do anything in a regular run.
#[test]
fn fork_child() -> TestResult {
    let (Some(scenario), Some(repo)) = (std::env::var_os(SCENARIO), std::env::var_os(REPO)) else {
        return Ok(());
    };
    let repo = PathBuf::from(repo);
    let mut store = gix_ref::file::Store::at(repo.join(".git"), crate::fixture_hash_kind());
    if let Some(components) = std::env::var_os(FSYNC) {
        store.fsync.components = gix_fs::fsync::Components::parse(components.to_string_lossy().as_ref().into()).0;
    }
    assert_eq!(scenario, "update-three");
    let committer = gix_actor::Signature {
        name: "committer".into(),
        email: "committer@example.com".into(),
        time: gix_date::parse_header("1234 +0800").expect("valid"),
    };
    let oid = gix_hash::ObjectId::from_hex(std::env::var(OID)?.as_bytes())?;
    let edits = NAMES.map(|name| {
        gix_ref::transaction::RefEdit::update_with_log(
            name.try_into().expect("valid"),
            oid,
            gix_ref::transaction::PreviousValue::MustNotExist,
            gix_ref::transaction::LogChange {
                mode: gix_ref::transaction::RefLog::AndReference,
                force_create_reflog: false,
                message: "m".into(),
            },
        )
    });
    let mut buf = Default::default();
    let outcome = store
        .transaction()
        .prepare(
            edits,
            gix_lock::acquire::Fail::Immediately,
            gix_lock::acquire::Fail::Immediately,
        )
        .and_then(|t| t.commit(committer.to_ref(&mut buf)));
    let text = match outcome {
        Ok(_) => "ok".to_owned(),
        Err(err) => match err.downcast_any_ref::<gix_ref::transaction::commit_failure::Uncertain>() {
            Some(uncertain) => format!("uncertain {:?}", uncertain.edits),
            None => format!("failed {err:?}"),
        },
    };
    std::fs::write(repo.join(OUTCOME_FILE), text)?;
    Ok(())
}

/// A repository that Git initialised, with a commit with the same id in each, which branches can point to.
fn repo(root: &Path, name: &str) -> TestResult<(PathBuf, String)> {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo)?;
    gix_testtools::git(&repo, "init -q")?;
    // The object format follows the environment of the tests, so the empty tree has to be written to know its id.
    let mut write_tree = gix_testtools::git_command(&repo);
    write_tree
        .args(["hash-object", "-w", "-t", "tree", "--stdin"])
        .stdin(std::process::Stdio::null());
    let tree = write_tree.output()?;
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
    Ok((repo, String::from_utf8(out.stdout)?.trim().to_owned()))
}

fn git_side(repo: &Path, commands: &Path, fsync: Option<&str>, inject: Option<&Inject>) -> TestResult<(Trace, String)> {
    let mut cmd = gix_testtools::git_command(repo);
    if let Some(fsync) = fsync {
        cmd.args(["-c", &format!("core.fsync={fsync}")]);
    }
    cmd.args(["update-ref", "-m", "m", "--stdin"])
        .env("GIT_COMMITTER_NAME", "committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "@1234 +0800");
    let trace = strace::trace(&cmd, repo, repo, inject, Some(commands))?;
    let state = dump(repo)?;
    Ok((trace, state))
}

fn fork_side(repo: &Path, oid: &str, fsync: Option<&str>, inject: Option<&Inject>) -> TestResult<(Trace, String)> {
    let mut cmd = Command::new(std::env::current_exe()?);
    let test = module_path!().split_once("::").expect("crate prefix").1;
    cmd.args([
        "--exact",
        &format!("{test}::fork_child"),
        "--nocapture",
        "--test-threads=1",
    ])
    .current_dir(repo)
    .env(SCENARIO, "update-three")
    .env(OID, oid)
    .env(REPO, repo);
    if let Some(fsync) = fsync {
        cmd.env(FSYNC, fsync);
    }
    let trace = strace::trace(&cmd, repo, repo, inject, None)?;
    let state = dump(repo)?;
    Ok((trace, state))
}

/// List files below `refs/` and `logs/` with their content, which is all that these transactions touch.
fn dump(repo: &Path) -> TestResult<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
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
                out.push(format!(
                    "{}: {}",
                    path.strip_prefix(root).expect("below root").display(),
                    std::fs::read_to_string(&path)?.trim_end()
                ));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    for sub in ["refs", "logs"] {
        walk(&repo.join(".git").join(sub), &repo.join(".git"), &mut out)?;
    }
    Ok(out.join("\n"))
}

/// Keep what happens to references and their logs, relative to `.git`, and leave out probing, which differs.
/// That's failed calls and creating directories, but also what was opened without being changed, as in Git.
fn significant(trace: &Trace) -> Vec<String> {
    trace
        .observations
        .iter()
        .filter(|o| o.ok && !matches!(o.event, strace::Event::Directory(_)))
        .map(|o| o.event.render().replace(".git/", ""))
        .filter(|line| line.contains("refs/"))
        .collect()
}

struct Setup {
    _root: gix_testtools::tempfile::TempDir,
    git_repo: PathBuf,
    fork_repo: PathBuf,
    commands: PathBuf,
    oid: String,
}

fn setup() -> TestResult<Option<Setup>> {
    if !strace::available_or_skip(module_path!()) {
        return Ok(None);
    }
    let root = gix_testtools::tempfile::tempdir()?;
    let (git_repo, oid) = repo(root.path(), "git")?;
    let (fork_repo, fork_oid) = repo(root.path(), "fork")?;
    assert_eq!(oid, fork_oid, "the same commit is created on both sides");
    let commands = root.path().join("commands");
    std::fs::write(&commands, NAMES.map(|name| format!("update {name} {oid}\n")).concat())?;
    Ok(Some(Setup {
        _root: root,
        git_repo,
        fork_repo,
        commands,
        oid,
    }))
}

#[test]
fn updating_three_references_flushes_and_renames_in_the_same_order_as_git() -> TestResult {
    let Some(s) = setup()? else { return Ok(()) };
    let (git, git_state) = git_side(&s.git_repo, &s.commands, Some("reference"), None)?;
    let (fork, fork_state) = fork_side(&s.fork_repo, &s.oid, Some("reference"), None)?;
    assert!(
        git.status.success() && fork.status.success(),
        "both transactions succeed"
    );
    let git_lines = significant(&git);
    assert!(
        git_lines.iter().any(|l| l.starts_with("flush refs/heads/a.lock")),
        "Git flushes each lock file: {git_lines:#?}"
    );
    assert_eq!(
        significant(&fork),
        git_lines,
        "all values are flushed while preparing, then each reference gets its reflog entry and its rename"
    );
    assert_eq!(fork_state, git_state, "same references and reflogs result");
    Ok(())
}

#[test]
fn nothing_is_flushed_by_default_like_in_git() -> TestResult {
    let Some(s) = setup()? else { return Ok(()) };
    let (git, _) = git_side(&s.git_repo, &s.commands, None, None)?;
    let (fork, _) = fork_side(&s.fork_repo, &s.oid, None, None)?;
    for (who, trace) in [("Git", &git), ("the fork", &fork)] {
        assert!(trace.status.success(), "{who} succeeds, or there is nothing to compare");
        assert!(
            significant(trace).iter().any(|l| l.starts_with("rename")),
            "{who} did rename the references, so the trace is not empty"
        );
        assert!(
            !significant(trace)
                .iter()
                .any(|l| l.starts_with("flush") || l.starts_with("writeout")),
            "{who} flushes no references without `core.fsync`"
        );
    }
    assert!(
        significant(&git)
            .iter()
            .any(|l| l == "rename refs/heads/a.lock -> refs/heads/a"),
        "paths are recognized, so that the absence of flushes means something"
    );
    assert_eq!(significant(&fork), significant(&git));
    Ok(())
}

/// The renames that did not complete, which must be the one of the second reference on both sides, or else the fault
/// went into a different call than intended and the states can't be compared meaningfully.
fn assert_fault_hit_the_second_rename(who: &str, trace: &Trace) {
    let incomplete: Vec<_> = trace
        .observations
        .iter()
        .filter(|o| !o.ok && matches!(o.event, strace::Event::Rename(..)))
        .map(|o| o.event.render().replace(".git/", ""))
        .collect();
    assert_eq!(
        incomplete,
        ["rename refs/heads/b.lock -> refs/heads/b"],
        "{who}: the injected fault is the only rename that didn't complete, and it's the second one"
    );
}

const RENAMES: &[&str] = &["rename", "renameat", "renameat2"];

/// Killed at the second of three renames, Git has updated the first reference, has the reflog entry for the second without
/// its value, and leaves the lock files of the second and third behind. The fork must leave the same state, which is
/// what a retry has to be able to cope with.
#[test]
fn killed_at_the_second_rename_leaves_the_state_git_leaves() -> TestResult {
    let Some(s) = setup()? else { return Ok(()) };
    let inject = Inject {
        syscalls: RENAMES,
        action: "signal=KILL",
        when: 2,
    };
    let (git, git_state) = git_side(&s.git_repo, &s.commands, Some("reference"), Some(&inject))?;
    let (fork, fork_state) = fork_side(&s.fork_repo, &s.oid, Some("reference"), Some(&inject))?;
    for (who, trace) in [("Git", &git), ("the fork", &fork)] {
        assert!(!trace.status.success(), "{who} was killed");
    }
    assert_fault_hit_the_second_rename("Git", &git);
    assert_fault_hit_the_second_rename("the fork", &fork);
    assert!(
        git_state.contains("refs/heads/b.lock") && !git_state.lines().any(|l| l.starts_with("refs/heads/b:")),
        "this is what Git leaves, a lock for the reference that wasn't updated:\n{git_state}"
    );
    assert_eq!(fork_state, git_state);
    Ok(())
}

/// With `EIO` at the second of three renames, the process lives to clean up its locks. Git has updated the first reference
/// and logged the second, which the fork reports with the progress of each edit.
#[test]
fn a_failing_second_rename_leaves_the_state_git_leaves_and_is_reported_as_uncertain() -> TestResult {
    let Some(s) = setup()? else { return Ok(()) };
    let inject = Inject {
        syscalls: RENAMES,
        action: "error=EIO",
        when: 2,
    };
    let (git, git_state) = git_side(&s.git_repo, &s.commands, Some("reference"), Some(&inject))?;
    let (fork, fork_state) = fork_side(&s.fork_repo, &s.oid, Some("reference"), Some(&inject))?;
    assert!(!git.status.success(), "Git fails");
    assert!(
        fork.status.success(),
        "the child reports the failure by its outcome file"
    );
    for (who, trace) in [("Git", &git), ("the fork", &fork)] {
        assert!(
            trace.observations.iter().any(|o| o.injected),
            "{who}: strace made a call fail, which is what is being tested"
        );
    }
    assert_fault_hit_the_second_rename("Git", &git);
    assert_fault_hit_the_second_rename("the fork", &fork);
    let mut fork_state = fork_state;
    let outcome = std::fs::read_to_string(s.fork_repo.join(OUTCOME_FILE))?;
    assert_eq!(
        outcome,
        r#"uncertain [(FullName("refs/heads/a"), Applied), (FullName("refs/heads/b"), InDoubt), (FullName("refs/heads/c"), NotAttempted)]"#,
        "the first edit is applied, the second one failed in the middle, the third one wasn't tried"
    );
    fork_state.push('\n');
    assert_eq!(
        fork_state.trim_end(),
        git_state.trim_end(),
        "the same references, reflogs and locks remain"
    );
    Ok(())
}
