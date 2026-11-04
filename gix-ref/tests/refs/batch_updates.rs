//! Edits that fail on their own are rejected while the others go on, which is what Git's `update-ref --batch-updates` does,
//! and is the oracle for what is rejected, why, what an observer hears about, and what the references are afterwards.
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
};

use gix_lock::acquire::Fail;
use gix_ref::{
    FullName, Target,
    transaction::{
        PreviousValue, RefEdit,
        observe::{Event, Veto},
    },
    view::transaction::LockOptions,
};
use gix_testtools::TestResult;

use crate::{fixture_hash_kind, scripted_fixture_writable};

/// An edit in the terms of Git's `update-ref --stdin`, with `C1` and `C2` standing for the first two commits.
#[derive(Clone, Copy)]
enum Edit {
    /// `update <name> <new> <old>` with `old` having to exist and match.
    Update(&'static str, &'static str, &'static str),
    Create(&'static str, &'static str),
    /// Like `Update`, but of a symbolic reference, which is followed to the reference that it points to.
    UpdateThroughSymbolicRef(&'static str, &'static str, &'static str),
    Delete(&'static str, Option<&'static str>),
}

struct Repos {
    dir: gix_testtools::tempfile::TempDir,
    c1: gix_hash::ObjectId,
    c2: gix_hash::ObjectId,
}

fn copy_dir(from: &Path, to: &Path) -> TestResult {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

impl Repos {
    fn new() -> TestResult<Self> {
        let dir = scripted_fixture_writable("make_batch_updates_repos.sh")?;
        let rev = |spec: &str| -> TestResult<gix_hash::ObjectId> {
            Ok(gix_hash::ObjectId::from_hex(
                gix_testtools::git(dir.path().join("files"), &format!("rev-parse {spec}"))?
                    .trim()
                    .as_bytes(),
            )?)
        };
        Ok(Repos {
            c1: rev("main~1")?,
            c2: rev("main")?,
            dir,
        })
    }

    /// A copy of the repository for `backend`, which is where a scenario is run.
    fn copy(&self, backend: &str, label: &str) -> TestResult<PathBuf> {
        let dest = self.dir.path().join(format!("{backend}-{label}"));
        copy_dir(&self.dir.path().join(backend), &dest)?;
        Ok(dest)
    }

    fn zero(&self) -> String {
        gix_hash::ObjectId::null(fixture_hash_kind()).to_string()
    }

    fn ids(&self, text: &str) -> String {
        text.replace("C1", &self.c1.to_string())
            .replace("C2", &self.c2.to_string())
    }

    fn stdin_for_git(&self, edits: &[Edit]) -> String {
        let mut out = String::new();
        for edit in edits {
            out.push_str(&self.ids(&match edit {
                Edit::Update(name, new, old) => format!("update {name} {new} {old}\n"),
                Edit::UpdateThroughSymbolicRef(name, new, old) => format!("update {name} {new} {old}\n"),
                Edit::Create(name, new) => format!("create {name} {new}\n"),
                Edit::Delete(name, None) => format!("delete {name}\n"),
                Edit::Delete(name, Some(old)) => format!("delete {name} {old}\n"),
            }));
        }
        out
    }

    fn ref_edits(&self, edits: &[Edit]) -> Vec<RefEdit> {
        build_edits(edits, self.c1, self.c2)
    }
}

/// The edits for the fork, with `c1` and `c2` for the ids that `C1` and `C2` stand for.
fn build_edits(edits: &[Edit], c1: gix_hash::ObjectId, c2: gix_hash::ObjectId) -> Vec<RefEdit> {
    let name = |name: &str| FullName::try_from(name).expect("valid name");
    let id = |name: &str| match name {
        "C1" => c1,
        "C2" => c2,
        other => panic!("unknown commit {other}"),
    };
    edits
        .iter()
        .map(|edit| match edit {
            Edit::Update(n, new, old) => RefEdit::update(
                name(n),
                Target::Object(id(new)),
                PreviousValue::MustExistAndMatch(Target::Object(id(old))),
                "batch",
            ),
            Edit::UpdateThroughSymbolicRef(n, new, old) => {
                let mut edit = RefEdit::update(
                    name(n),
                    Target::Object(id(new)),
                    PreviousValue::MustExistAndMatch(Target::Object(id(old))),
                    "batch",
                );
                edit.deref = true;
                edit
            }
            Edit::Create(n, new) => {
                RefEdit::update(name(n), Target::Object(id(new)), PreviousValue::MustNotExist, "batch")
            }
            Edit::Delete(n, old) => RefEdit::delete(
                name(n),
                old.map_or(PreviousValue::Any, |old| {
                    PreviousValue::MustExistAndMatch(Target::Object(id(old)))
                }),
            ),
        })
        .collect()
}

/// What a scenario did, which is to be the same for Git and the fork.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    /// `(name, reason)` of every rejected edit, in the order of the edits.
    rejected: Vec<(String, String)>,
    /// The states that observers heard about, with the updates as `<old> <new> <name>`.
    events: Vec<(String, Vec<String>)>,
    /// All references with their ids, after the transaction.
    refs: Vec<String>,
    /// The ids in the reflog of `HEAD`, newest first, which follows the branch it points to.
    head_log: Vec<String>,
}

fn refs(repo: &Path) -> TestResult<Vec<String>> {
    Ok(
        gix_testtools::git(repo, "for-each-ref --format='%(refname) %(objectname)'")?
            .lines()
            .map(ToOwned::to_owned)
            .collect(),
    )
}

fn head_log(repo: &Path) -> TestResult<Vec<String>> {
    Ok(gix_testtools::git(repo, "reflog show HEAD --format=%H --")?
        .lines()
        .map(ToOwned::to_owned)
        .collect())
}

fn git_side(
    repos: &Repos,
    backend: &str,
    label: &str,
    edits: &[Edit],
    setup: impl Fn(&Path) -> TestResult,
) -> TestResult<Outcome> {
    let repo = repos.copy(backend, &format!("git-{label}"))?;
    setup(&repo)?;
    let log = repo.join("hook.log");
    let hook = repo.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().expect("parent"))?;
    std::fs::write(
        &hook,
        format!("#!/bin/sh\necho \"== $1\" >> '{0}'\ncat >> '{0}'\n", log.display()),
    )?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    }
    let mut child = gix_testtools::git_command(&repo)
        .args(["update-ref", "--stdin", "--batch-updates"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(repos.stdin_for_git(edits).as_bytes())?;
    let output = child.wait_with_output()?;
    assert!(output.status.success(), "{output:?}");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // `rejected <name> <new> <old> <reason>`
    let rejected: Vec<_> = all
        .lines()
        .filter_map(|line| line.strip_prefix("rejected "))
        .map(|line| {
            let mut parts = line.splitn(4, ' ');
            let name = parts.next().expect("name").to_owned();
            let reason = parts.nth(2).expect("reason").to_owned();
            (name, reason)
        })
        .collect();
    let mut events: Vec<(String, Vec<String>)> = Vec::new();
    for line in std::fs::read_to_string(&log).unwrap_or_default().lines() {
        if let Some(state) = line.strip_prefix("== ") {
            events.push((state.to_owned(), Vec::new()));
        } else {
            let (_, updates) = events.last_mut().expect("a state precedes updates");
            updates.push(line.to_owned());
        }
    }
    Ok(Outcome {
        rejected,
        events,
        refs: refs(&repo)?,
        head_log: head_log(&repo)?,
    })
}

/// The backends to compare with Git, of which `reftable` needs its feature.
#[cfg(feature = "reftable")]
const BACKENDS: [&str; 2] = ["files", "reftable"];
#[cfg(not(feature = "reftable"))]
const BACKENDS: [&str; 1] = ["files"];

fn open(repo: &Path, backend: &str) -> gix_ref::Store {
    let git_dir = repo.join(".git");
    match backend {
        "files" => gix_ref::file::Store::at(git_dir, fixture_hash_kind()).into(),
        #[cfg(feature = "reftable")]
        "reftable" => gix_ref::reftable::Store::at(git_dir, fixture_hash_kind()).into(),
        _ => unreachable!("known backends"),
    }
}

fn fork_side(
    repos: &Repos,
    backend: &str,
    label: &str,
    edits: &[Edit],
    setup: impl Fn(&Path) -> TestResult,
) -> TestResult<Outcome> {
    let repo = repos.copy(backend, &format!("fork-{label}"))?;
    setup(&repo)?;
    let store = open(&repo, backend);
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(String, Vec<String>)>::new()));
    let observer = {
        let events = events.clone();
        let zero = repos.zero();
        move |event: &Event<'_>| -> Result<(), Veto> {
            // Git's hook can't tell the transaction on `packed-refs` from the main one, so both are compared.
            {
                let updates = event
                    .updates
                    .iter()
                    .map(|update| {
                        let id = |target: &Option<Target>| {
                            target
                                .as_ref()
                                .and_then(|target| target.try_id())
                                .map_or_else(|| zero.clone(), |id| id.to_owned().to_string())
                        };
                        format!("{} {} {}", id(&update.previous), id(&update.new), update.name.as_bstr())
                    })
                    .collect();
                events.borrow_mut().push((event.state.as_str().to_owned(), updates));
            }
            Ok(())
        }
    };
    let prepared = store
        .as_ref()
        .transaction()
        .batch_updates()
        .observer(observer)
        .prepare(
            repos.ref_edits(edits),
            LockOptions::new(Fail::Immediately, Fail::Immediately),
        )?;
    let rejected: Vec<_> = prepared
        .rejected()
        .iter()
        .map(|rejected| {
            (
                rejected.edit.name.as_bstr().to_string(),
                rejected.reason.as_str().to_owned(),
            )
        })
        .collect();
    let committer = gix_actor::Signature {
        name: "Committer".into(),
        email: "committer@example.com".into(),
        time: gix_date::parse_header("1700100000 +0130").expect("valid"),
    };
    let outcome = prepared.commit(committer.to_ref(&mut gix_date::parse::TimeBuf::default()))?;
    assert_eq!(
        outcome.rejected.len(),
        rejected.len(),
        "the outcome has what was rejected, which isn't among the {} applied edits",
        outcome.edits.len()
    );
    let events = events.borrow().clone();
    Ok(Outcome {
        rejected,
        events,
        refs: refs(&repo)?,
        head_log: head_log(&repo)?,
    })
}

fn assert_same(repos: &Repos, label: &str, edits: &[Edit], setup: impl Fn(&Path) -> TestResult + Copy) -> TestResult {
    for backend in BACKENDS {
        let git = git_side(repos, backend, label, edits, setup)?;
        let fork = fork_side(repos, backend, label, edits, setup)?;
        let sorted = |rejected: &[(String, String)]| {
            let mut rejected = rejected.to_vec();
            rejected.sort();
            rejected
        };
        assert_eq!(
            sorted(&fork.rejected),
            sorted(&git.rejected),
            "{backend}/{label}: the same edits are rejected, for the same reasons, even though Git reports them in the order it found them"
        );
        assert_eq!(fork.refs, git.refs, "{backend}/{label}: the same references remain");
        if git.rejected.iter().any(|(name, _)| name == "refs/heads/main") {
            // Git logs the move of `HEAD` even though the update of the branch it points to was rejected, so that the reflog
            // claims a value that `HEAD` never had. That is a bug we don't copy: nothing is logged for edits that didn't happen.
            assert_eq!(
                git.head_log[1..],
                fork.head_log,
                "{backend}/{label}: Git adds a spurious entry to the reflog of HEAD that we don't"
            );
        } else {
            assert_eq!(
                fork.head_log, git.head_log,
                "{backend}/{label}: the reflog of HEAD follows the branch it points to when that is updated"
            );
        }
        assert_eq!(
            fork.events, git.events,
            "{backend}/{label}: observers hear about all edits, rejected or not, like Git's hook does"
        );
        assert!(
            !git.events.is_empty(),
            "{backend}/{label}: the hook was run, so the comparison isn't vacuous"
        );
    }
    Ok(())
}

#[test]
fn edits_are_rejected_one_by_one_for_each_reason_like_git_does() -> TestResult {
    let repos = Repos::new()?;
    let edits = [
        // The reference is at C1, not at C2.
        Edit::Update("refs/heads/a", "C2", "C2"),
        // Fine.
        Edit::Update("refs/heads/b", "C2", "C1"),
        Edit::Create("refs/heads/c", "C2"),
        // Wrong old value for a deletion.
        Edit::Delete("refs/heads/d", Some("C2")),
        Edit::Create("refs/heads/x/y", "C2"),
        // `x` would be the directory of `x/y`, which comes first.
        Edit::Create("refs/heads/x", "C2"),
        // `deep` is the directory of `deep/er` which exists.
        Edit::Create("refs/heads/deep", "C2"),
        // Doesn't exist.
        Edit::Update("refs/heads/missing", "C2", "C1"),
        Edit::Delete("refs/heads/gone", Some("C1")),
        // Exists.
        Edit::Create("refs/heads/main", "C1"),
        // Fine, from packed refs where there are some.
        Edit::Delete("refs/heads/packed", None),
    ];
    assert_same(&repos, "reasons", &edits, |_| Ok(()))
}

#[test]
fn a_batch_with_nothing_wrong_is_applied_as_a_whole() -> TestResult {
    let repos = Repos::new()?;
    assert_same(
        &repos,
        "clean",
        &[
            Edit::Create("refs/heads/c", "C2"),
            Edit::Update("refs/heads/b", "C2", "C1"),
        ],
        |_| Ok(()),
    )
}

#[test]
fn rejecting_every_edit_leaves_everything_as_it_was() -> TestResult {
    let repos = Repos::new()?;
    assert_same(
        &repos,
        "all-rejected",
        &[
            Edit::Update("refs/heads/a", "C2", "C2"),
            Edit::Create("refs/heads/main", "C1"),
        ],
        |_| Ok(()),
    )
}

#[test]
fn a_reference_that_is_locked_by_somebody_else_is_rejected_on_its_own_like_git_does() -> TestResult {
    let repos = Repos::new()?;
    let lock_a = |repo: &Path| -> TestResult {
        std::fs::create_dir_all(repo.join(".git/refs/heads"))?;
        std::fs::write(repo.join(".git/refs/heads/a.lock"), "")?;
        Ok(())
    };
    let edits = [
        Edit::Update("refs/heads/a", "C2", "C1"),
        Edit::Update("refs/heads/b", "C2", "C1"),
    ];
    let (git, fork) = (
        git_side(&repos, "files", "locked", &edits, lock_a)?,
        fork_side(&repos, "files", "locked", &edits, lock_a)?,
    );
    assert_eq!(
        git.rejected,
        [("refs/heads/a".to_owned(), "reference already exists".to_owned())],
        "Git calls the lock of somebody else the existence of the reference"
    );
    assert_eq!(fork.rejected, git.rejected);
    assert_eq!(fork.refs, git.refs, "the other edit was applied");
    Ok(())
}

#[test]
fn a_rejected_edit_of_the_checked_out_branch_leaves_the_reflog_of_head_alone() -> TestResult {
    let repos = Repos::new()?;
    // `HEAD` points to `main`.
    assert_same(
        &repos,
        "head-rejected",
        &[
            Edit::Update("refs/heads/main", "C1", "C1"),
            Edit::Update("refs/heads/b", "C2", "C1"),
        ],
        |_| Ok(()),
    )?;
    assert_same(
        &repos,
        "head-applied",
        &[
            Edit::Update("refs/heads/main", "C1", "C2"),
            Edit::Update("refs/heads/b", "C2", "C2"),
        ],
        |_| Ok(()),
    )
}

#[test]
fn a_rejected_edit_doesnt_make_its_name_unavailable_for_others() -> TestResult {
    let repos = Repos::new()?;
    assert_same(
        &repos,
        "names-freed",
        &[
            // Doesn't exist, so it's rejected and never takes the name `zz/a`, which makes `zz` available for a reference.
            Edit::Update("refs/heads/zz/a", "C2", "C1"),
            Edit::Create("refs/heads/zz", "C2"),
        ],
        |_| Ok(()),
    )?;
    assert_same(
        &repos,
        "names-freed-reverse",
        &[
            Edit::Create("refs/heads/yy", "C2"),
            // Rejected for a reason of its own, and not for the conflict with `yy`.
            Edit::Update("refs/heads/yy/a", "C2", "C1"),
        ],
        |_| Ok(()),
    )
}

#[test]
fn edits_through_a_symbolic_reference_are_reported_where_git_reports_them_while_others_are_rejected() -> TestResult {
    let repos = Repos::new()?;
    // Git lists a rejected edit after the edits that precede it in what was asked for, and an edit through a symbolic reference
    // counts as the reference it leads to, which comes last. Observers hear about the same updates in the same order.
    // `HEAD` points to `main`, which is at C2, so this edit is split into one for `HEAD` and one for `main`.
    assert_same(
        &repos,
        "through-head",
        &[
            Edit::UpdateThroughSymbolicRef("HEAD", "C1", "C2"),
            Edit::Update("refs/heads/a", "C2", "C2"),
            Edit::Update("refs/heads/b", "C2", "C1"),
        ],
        |_| Ok(()),
    )?;
    // The branch behind `HEAD` doesn't match, which rejects the edit as a whole. Git names the reference it leads to, and
    // the fork the edit as it was asked for, which is what a caller can look for. Events are named in the same way, so only
    // what is rejected and what remains is compared.
    let edits = [
        Edit::Update("refs/heads/b", "C2", "C1"),
        Edit::UpdateThroughSymbolicRef("HEAD", "C1", "C1"),
        Edit::Update("refs/heads/a", "C2", "C2"),
    ];
    for backend in BACKENDS {
        let git = git_side(&repos, backend, "through-head-rejected", &edits, |_| Ok(()))?;
        let fork = fork_side(&repos, backend, "through-head-rejected", &edits, |_| Ok(()))?;
        let reason = "incorrect old value provided".to_owned();
        assert_eq!(
            git.rejected.iter().map(|(_, reason)| reason).collect::<Vec<_>>(),
            [&reason, &reason],
            "{backend}: Git rejects the edit through `HEAD` and the one of `a` for the same reason"
        );
        assert_eq!(
            fork.rejected,
            [
                ("HEAD".to_owned(), reason.clone()),
                ("refs/heads/a".to_owned(), reason.clone())
            ],
            "{backend}: the fork rejects them as well, naming the edit as it was asked for"
        );
        assert_eq!(fork.refs, git.refs, "{backend}: the same references remain");
    }
    Ok(())
}

/// What the file system sees of a batch, which has to be what Git does when nothing is rejected: each reference is locked,
/// written, flushed if configured and renamed once. Passes that look for what to reject are only allowed to
/// cost anything when something is rejected.
#[cfg(target_os = "linux")]
mod effects {
    use std::process::Command;

    use gix_testtools::strace::{self, Event, Trace};

    use super::*;

    const SCENARIO: &str = "GIX_REFS_BATCH_SCENARIO";
    const BACKEND: &str = "GIX_REFS_BATCH_BACKEND";
    const REPO: &str = "GIX_REFS_BATCH_REPO";
    const IDS: &str = "GIX_REFS_BATCH_IDS";

    fn scenario_edits(scenario: &str) -> Vec<Edit> {
        let mut edits = vec![
            Edit::Update("refs/heads/b", "C2", "C1"),
            Edit::Create("refs/heads/c", "C2"),
            Edit::Create("refs/heads/e", "C2"),
        ];
        match scenario {
            "clean" => {}
            // `a` is at C1.
            "one-rejected" => edits.insert(1, Edit::Update("refs/heads/a", "C2", "C2")),
            other => panic!("unknown scenario {other}"),
        }
        edits
    }

    /// What the fork does in the re-executed child, to be traced alone. Does nothing in a regular run.
    #[test]
    fn fork_child() -> TestResult {
        let (Some(scenario), Some(backend), Some(repo), Some(ids)) = (
            std::env::var_os(SCENARIO),
            std::env::var_os(BACKEND),
            std::env::var_os(REPO),
            std::env::var_os(IDS),
        ) else {
            return Ok(());
        };
        let (scenario, backend, repo) = (
            scenario.to_string_lossy().into_owned(),
            backend.to_string_lossy().into_owned(),
            PathBuf::from(repo),
        );
        let ids = ids.to_string_lossy().into_owned();
        let (c1, c2) = ids.split_once(' ').expect("two ids");
        let (c1, c2) = (
            gix_hash::ObjectId::from_hex(c1.as_bytes())?,
            gix_hash::ObjectId::from_hex(c2.as_bytes())?,
        );
        let store: gix_ref::Store = if backend == "files" {
            let mut store = gix_ref::file::Store::at(repo.join(".git"), fixture_hash_kind());
            store.fsync.components = gix_fs::fsync::Components::parse("reference".into()).0;
            store.into()
        } else {
            #[cfg(feature = "reftable")]
            {
                let mut store = open(&repo, &backend);
                let mut options = gix_ref::reftable::StackOptions::default();
                options.fsync.components = gix_fs::fsync::Components::parse("reference".into()).0;
                store.set_reftable_options(Ok(options));
                store
            }
            #[cfg(not(feature = "reftable"))]
            unreachable!("only backends that exist are compared")
        };
        let committer = gix_actor::Signature {
            name: "Committer".into(),
            email: "committer@example.com".into(),
            time: gix_date::parse_header("1700100000 +0130").expect("valid"),
        };
        let prepared = store.as_ref().transaction().batch_updates().prepare(
            build_edits(&scenario_edits(&scenario), c1, c2),
            LockOptions::new(Fail::Immediately, Fail::Immediately),
        )?;
        prepared.commit(committer.to_ref(&mut gix_date::parse::TimeBuf::default()))?;
        Ok(())
    }

    /// What was done to references, logs and tables, relative to `.git`, without probing.
    fn significant(trace: &Trace) -> Vec<String> {
        trace
            .observations
            .iter()
            .filter(|o| o.ok && !matches!(o.event, Event::Directory(_)))
            .map(|o| o.event.render().replace(".git/", ""))
            .filter(|line| line.contains("refs/") || line.contains("reftable/"))
            .collect()
    }

    /// How often references or stacks were locked and how often something was flushed.
    #[derive(Debug, PartialEq, Eq)]
    struct Counts {
        locks: usize,
        flushes: usize,
    }

    fn counts(lines: &[String]) -> Counts {
        // What follows the first publication of a stack is compaction, which is a courtesy whose timing differs from Git's.
        let publication = lines
            .iter()
            .position(|line| line.ends_with("reftable/tables.list.lock -> reftable/tables.list"));
        let lines = publication.map_or(lines, |end| &lines[..=end]);
        Counts {
            locks: lines
                .iter()
                .filter(|line| {
                    line.starts_with("open-for-writing")
                        && std::path::Path::new(line)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"))
                })
                .count(),
            flushes: lines.iter().filter(|line| line.starts_with("flush")).count(),
        }
    }

    fn run(repos: &Repos, backend: &str, scenario: &str) -> TestResult<Option<(Vec<String>, Vec<String>)>> {
        if !strace::available_or_skip(module_path!()) {
            return Ok(None);
        }
        let edits = scenario_edits(scenario);
        let git_repo = repos.copy(backend, &format!("trace-git-{scenario}"))?;
        let fork_repo = repos.copy(backend, &format!("trace-fork-{scenario}"))?;
        let commands = repos.dir.path().join(format!("commands-{backend}-{scenario}"));
        std::fs::write(&commands, repos.stdin_for_git(&edits))?;

        let mut git = gix_testtools::git_command(&git_repo);
        git.args(["-c", "core.fsync=reference", "update-ref", "--stdin", "--batch-updates"])
            .env("GIT_COMMITTER_NAME", "Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "@1700100000 +0130");
        let git_trace = strace::trace(&git, &git_repo, &git_repo, None, Some(&commands))?;

        let mut fork = Command::new(std::env::current_exe()?);
        let test = module_path!().split_once("::").expect("crate prefix").1;
        fork.args([
            "--exact",
            &format!("{test}::fork_child"),
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(&fork_repo)
        .env(SCENARIO, scenario)
        .env(BACKEND, backend)
        .env(REPO, &fork_repo)
        .env(IDS, format!("{} {}", repos.c1, repos.c2));
        let fork_trace = strace::trace(&fork, &fork_repo, &fork_repo, None, None)?;
        assert!(
            git_trace.status.success() && fork_trace.status.success(),
            "{backend}/{scenario}: both batches ran, or there is nothing to compare: {:?} {:?}",
            git_trace.status,
            fork_trace.status
        );
        Ok(Some((significant(&git_trace), significant(&fork_trace))))
    }

    #[test]
    fn a_batch_without_rejections_locks_and_flushes_each_reference_once_like_git() -> TestResult {
        let repos = Repos::new()?;
        for backend in BACKENDS {
            let Some((git, fork)) = run(&repos, backend, "clean")? else {
                return Ok(());
            };
            let git_counts = counts(&git);
            let fork_counts = counts(&fork);
            assert!(
                git_counts.locks > 0,
                "{backend}: Git locks, so that the comparison isn't vacuous: {git:#?}"
            );
            assert_eq!(
                fork_counts, git_counts,
                "{backend}: the lock windows and flushes of the fork are those of Git\nfork: {fork:#?}\ngit: {git:#?}"
            );
            if backend == "files" {
                assert_eq!(fork, git, "files: the effects come in the order of Git's");
            }
        }
        Ok(())
    }

    #[test]
    fn a_rejection_costs_extra_passes_but_changes_the_same_references_in_the_end() -> TestResult {
        let repos = Repos::new()?;
        for backend in BACKENDS {
            let Some((git, fork)) = run(&repos, backend, "one-rejected")? else {
                return Ok(());
            };
            let (git_counts, fork_counts) = (counts(&git), counts(&fork));
            // Git: 4 and 1 locks. The first attempt fails on the rejected edit, a dry pass finds what else fails, and the last one counts.
            let expected = if backend == "files" {
                Counts { locks: 10, flushes: 4 }
            } else {
                Counts { locks: 4, flushes: 2 }
            };
            assert_eq!(
                fork_counts, expected,
                "{backend}: finding what to reject takes more passes than the {git_counts:?} of Git, which is the documented difference, and which shouldn't grow"
            );
        }
        Ok(())
    }
}
