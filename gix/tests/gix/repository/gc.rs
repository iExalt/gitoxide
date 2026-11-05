//! Garbage collection and `maintenance run`, with `git gc` and `git maintenance run` as the oracle for what is done and when it is
//! due. See `make_gc_repo.sh` for the repository and what is special about each of its objects.
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use gix::maintenance::{
    gc::{self, Skipped},
    run::{self, Task, TaskOutcome},
};
use gix_hash::Kind;
use gix_testtools::{Creation, TestResult};

fn kinds() -> impl Iterator<Item = Kind> {
    [Kind::Sha1, Kind::Sha256]
        .into_iter()
        .filter(|kind| *kind == Kind::Sha1 || cfg!(feature = "sha256"))
}

fn formats() -> Vec<&'static str> {
    let mut formats = vec!["files"];
    if cfg!(feature = "reftable") {
        formats.push("reftable");
    }
    formats
}

fn fixture(kind: Kind, format: &str) -> TestResult<(gix_testtools::tempfile::TempDir, PathBuf)> {
    let dir = gix_testtools::scripted_fixture_writable_with_args(
        "make_gc_repo.sh",
        [kind.to_string(), format.to_owned()],
        Creation::Execute,
    )?;
    let repo_dir = dir.path().join("repo");
    Ok((dir, repo_dir))
}

/// Configuration as keys with values.
type Settings<'a> = &'a [(&'a str, &'a str)];

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

fn open(repo_dir: &Path) -> TestResult<gix::Repository> {
    Ok(gix::open_opts(repo_dir, crate::restricted())?)
}

/// What a repository has, in a form that can be compared with that of another one.
#[derive(Debug, PartialEq, Eq)]
struct State {
    /// All objects with their kinds, wherever they are.
    objects: BTreeSet<String>,
    /// The number of loose objects.
    loose: usize,
    /// The number of packs and of those with a `.mtimes` file, which are cruft packs.
    packs: (usize, usize),
    /// All references with what they point to.
    refs: Vec<String>,
    /// The number of entries of the reflog of `HEAD`.
    head_reflog: usize,
    /// The number of files that store references, which is lower after they were packed or their tables were merged.
    ref_files: usize,
}

fn state(repo_dir: &Path) -> TestResult<State> {
    let objects = gix_testtools::git(
        repo_dir,
        "cat-file --batch-all-objects --unordered --batch-check='%(objectname) %(objecttype)'",
    )?
    .lines()
    .map(ToOwned::to_owned)
    .collect();
    let objects_dir = repo_dir.join(".git/objects");
    let mut loose = 0;
    for fan_out in std::fs::read_dir(&objects_dir)? {
        let fan_out = fan_out?;
        if fan_out.file_name().len() == 2 && fan_out.path().is_dir() {
            loose += std::fs::read_dir(fan_out.path())?.count();
        }
    }
    let mut packs = (0, 0);
    for entry in std::fs::read_dir(objects_dir.join("pack"))? {
        match entry?.path().extension().and_then(|ext| ext.to_str()) {
            Some("pack") => packs.0 += 1,
            Some("mtimes") => packs.1 += 1,
            _ => {}
        }
    }
    let refs = gix_testtools::git(repo_dir, "for-each-ref --format='%(refname) %(objectname)'")?
        .lines()
        .map(ToOwned::to_owned)
        .collect();
    let head_reflog = gix_testtools::git(repo_dir, "reflog show HEAD")?.lines().count();
    let mut ref_files = 0;
    for dir in ["refs", "reftable"] {
        let mut dirs = vec![repo_dir.join(".git").join(dir)];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue; // the other reference format
            };
            for entry in entries {
                let entry = entry?;
                if entry.path().is_dir() {
                    dirs.push(entry.path());
                } else {
                    ref_files += 1;
                }
            }
        }
    }
    ref_files += usize::from(repo_dir.join(".git/packed-refs").exists());
    Ok(State {
        objects,
        loose,
        packs,
        refs,
        head_reflog,
        ref_files,
    })
}

fn assert_healthy(repo_dir: &Path, what: &str) -> TestResult {
    gix_testtools::git(repo_dir, "fsck --full --strict")?;
    let count = gix_testtools::git(repo_dir, "count-objects -v")?;
    assert!(
        count.lines().any(|line| line == "garbage: 0"),
        "{what}: no garbage files are left in the object directory: {count}"
    );
    Ok(())
}

fn write_hook(repo_dir: &Path, name: &str, exit_code: i32) -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;
    let hooks = repo_dir.join(".git/hooks");
    std::fs::create_dir_all(&hooks)?;
    let path = hooks.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nexit {exit_code}\n"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Run `git` with the settings as `-c` options, as the isolation of the test tools overrides what the repository configures.
fn git_with(repo_dir: &Path, settings: Settings, args: &str) -> TestResult<String> {
    let mut command = String::new();
    for (key, value) in settings {
        write!(command, "-c {key}={value} ")?;
    }
    command.push_str(args);
    Ok(gix_testtools::git(repo_dir, &command)?)
}

fn git_config(repo_dir: &Path, settings: Settings) -> TestResult {
    for (key, value) in settings {
        gix_testtools::git(repo_dir, &format!("config {key} {value}"))?;
    }
    Ok(())
}

#[test]
fn what_remains_after_gc_is_what_git_gc_keeps() -> TestResult {
    for kind in kinds() {
        for format in formats() {
            let (_git, git_dir) = fixture(kind, format)?;
            let (_native, native_dir) = fixture(kind, format)?;
            let before = state(&native_dir)?;
            gix_testtools::git(&git_dir, "gc -q")?;

            let outcome = open(&native_dir)?.gc(gc::Options::default(), &never())?;
            assert!(outcome.skipped.is_none(), "{kind} {format}: it ran: {outcome:?}");

            let (expected, actual) = (state(&git_dir)?, state(&native_dir)?);
            assert_ne!(
                before, actual,
                "{kind} {format}: something changed, or the fixture is wrong"
            );
            assert_eq!(
                actual, expected,
                "{kind} {format}: the same objects, packs, references and reflog as after `git gc`"
            );
            assert_healthy(&native_dir, &format!("{kind} {format}"))?;
            assert!(
                !native_dir.join(".git/gc.pid").exists() && !native_dir.join(".git/gc.pid.lock").exists(),
                "{kind} {format}: the locks are gone"
            );
        }
    }
    Ok(())
}

#[test]
fn automatic_gc_is_due_when_git_says_so() -> TestResult {
    // with its decision, and what a limit of `1` means to Git for the loose objects: more than one in `objects/17`
    let configs: [(&str, Settings, bool); 5] = [
        ("defaults", &[], false),
        ("a low limit", &[("gc.auto", "1")], true),
        ("disabled with 0", &[("gc.auto", "0")], false),
        ("disabled with a negative limit", &[("gc.auto", "-1")], false),
        (
            "a limit that is too high but packs are limited",
            &[("gc.auto", "100000"), ("gc.autoPackLimit", "0")],
            false,
        ),
    ];
    for kind in kinds() {
        for (what, settings, due) in configs {
            let (_git, git_dir) = fixture(kind, "files")?;
            let (_native, native_dir) = fixture(kind, "files")?;
            for dir in [&git_dir, &native_dir] {
                git_config(dir, settings)?;
            }
            let before = state(&git_dir)?;
            git_with(&git_dir, settings, "gc --auto -q --no-detach")?;
            let changed_by_git = state(&git_dir)? != before;

            let mut options = gc::Options::default();
            options.auto = true;
            let outcome = open(&native_dir)?.gc(options, &never())?;
            let changed_natively = state(&native_dir)? != before;
            assert_eq!(
                changed_by_git, due,
                "{kind} {what}: Git agrees with the expectation of this test"
            );
            assert_eq!(
                changed_natively, changed_by_git,
                "{kind} {what}: native gc runs if Git's does: {outcome:?}"
            );
            if !due {
                assert_eq!(outcome.skipped, Some(Skipped::NotDue), "{kind} {what}: it says why");
            } else {
                assert_eq!(
                    state(&native_dir)?,
                    state(&git_dir)?,
                    "{kind} {what}: the same state as after `git gc --auto`"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn the_pre_auto_gc_hook_decides_and_a_failing_hook_stops_gc() -> TestResult {
    for kind in kinds() {
        for (exit_code, runs) in [(0, true), (1, false)] {
            let (_git, git_dir) = fixture(kind, "files")?;
            let (_native, native_dir) = fixture(kind, "files")?;
            git_config(&native_dir, &[("gc.auto", "1")])?;
            for dir in [&git_dir, &native_dir] {
                write_hook(dir, "pre-auto-gc", exit_code)?;
            }
            let before = state(&git_dir)?;
            git_with(&git_dir, &[("gc.auto", "1")], "gc --auto -q --no-detach")?;
            assert_eq!(state(&git_dir)? != before, runs, "{kind}: Git honours the hook");

            let mut options = gc::Options::default();
            options.auto = true;
            let outcome = open(&native_dir)?.gc(options, &never())?;
            assert_eq!(
                state(&native_dir)? != before,
                runs,
                "{kind}: so does native gc, with exit code {exit_code}"
            );
            assert_eq!(
                outcome.skipped,
                (!runs).then_some(Skipped::DeclinedByHook),
                "{kind}: and it says so"
            );
        }
    }
    Ok(())
}

#[test]
fn automatic_maintenance_is_wanted_as_git_decides_it() -> TestResult {
    // `prepare_auto_maintenance()` in `run-command.c`: `maintenance.auto` wins, else `gc.auto > 0`, and `maintenance.autoDetach`
    // wins over `gc.autoDetach`.
    let cases: [(Settings, bool, bool); 8] = [
        (&[], true, true),
        (&[("maintenance.auto", "false")], false, true),
        (&[("gc.auto", "0")], false, true),
        (&[("gc.auto", "-1")], false, true),
        (&[("gc.auto", "10")], true, true),
        (&[("maintenance.auto", "true"), ("gc.auto", "0")], true, true),
        (&[("gc.autoDetach", "false")], true, false),
        (
            &[("gc.autoDetach", "false"), ("maintenance.autoDetach", "true")],
            true,
            true,
        ),
    ];
    let (_dir, repo_dir) = fixture(Kind::Sha1, "files")?;
    for (settings, enabled, detach) in cases {
        git_config(&repo_dir, settings)?;
        let auto = open(&repo_dir)?.auto_maintenance()?;
        assert_eq!(
            (auto.enabled, auto.detach),
            (enabled, detach),
            "{settings:?}: whether automatic maintenance runs, and in the background"
        );
        for (key, _) in settings {
            gix_testtools::git(&repo_dir, &format!("config --unset {key}"))?;
        }
    }
    Ok(())
}

#[test]
fn gc_does_not_run_while_somebody_else_holds_a_lock() -> TestResult {
    let (_dir, repo_dir) = fixture(Kind::Sha1, "files")?;
    let git_dir = repo_dir.join(".git");
    let before = state(&repo_dir)?;
    git_config(&repo_dir, &[("gc.auto", "1")])?;
    let repo = open(&repo_dir)?;
    let mut auto = gc::Options::default();
    auto.auto = true;

    // a recent `gc.pid` of a process on some other machine, which Git respects, too
    std::fs::write(git_dir.join("gc.pid"), "4242 some-other-host")?;
    let outcome = repo.gc(auto, &never())?;
    assert!(
        matches!(&outcome.skipped, Some(Skipped::HeldBy(gc::Busy::Holder(holder))) if holder.pid == 4242),
        "automatic gc leaves it to the holder: {outcome:?}"
    );
    assert!(
        repo.gc(gc::Options::default(), &never()).is_err(),
        "an explicit gc fails and says who holds it"
    );
    assert_eq!(state(&repo_dir)?, before, "nothing changed");
    git_with(&repo_dir, &[("gc.auto", "1")], "gc --auto -q --no-detach")?;
    assert_eq!(state(&repo_dir)?, before, "Git leaves it as well");

    // `gc.pid.lock` is what a process that takes `gc.pid` holds, and Git gives up when it exists
    std::fs::write(git_dir.join("gc.pid.lock"), "")?;
    let outcome = repo.gc(auto, &never())?;
    assert!(
        matches!(&outcome.skipped, Some(Skipped::HeldBy(gc::Busy::LockFile(_)))),
        "{outcome:?}"
    );
    std::fs::remove_file(git_dir.join("gc.pid.lock"))?;

    // a `gc.pid` that is old enough to be stale doesn't count
    let stale = std::fs::File::options().write(true).open(git_dir.join("gc.pid"))?;
    stale.set_modified(std::time::SystemTime::now() - std::time::Duration::from_hours(13))?;
    drop(stale);
    let outcome = repo.gc(auto, &never())?;
    assert!(
        outcome.skipped.is_none(),
        "a run that is more than 12 hours old is stale: {outcome:?}"
    );
    assert!(
        !git_dir.join("gc.pid").exists(),
        "and its file is replaced and then removed"
    );

    // `force` goes over `gc.pid`, but never over `objects/maintenance.lock`, which has no age and no holder to ask
    let (_dir, repo_dir) = fixture(Kind::Sha1, "files")?;
    let git_dir = repo_dir.join(".git");
    let repo = open(&repo_dir)?;
    std::fs::write(git_dir.join("gc.pid"), "4242 some-other-host")?;
    let mut force = gc::Options::default();
    force.force = true;
    assert!(repo.gc(force, &never())?.skipped.is_none(), "force goes over a holder");
    std::fs::write(git_dir.join("objects/maintenance.lock"), "")?;
    let outcome = repo.gc(force, &never())?;
    assert!(
        matches!(&outcome.skipped, Some(Skipped::MaintenanceLockHeld(path)) if path.ends_with("maintenance.lock")),
        "{outcome:?}"
    );
    assert!(
        !git_dir.join("gc.pid").exists(),
        "a run that is skipped for the lock leaves no `gc.pid` behind"
    );
    Ok(())
}

#[test]
fn what_an_index_without_a_pack_left_is_removed_with_its_companions_and_nothing_else() -> TestResult {
    for kind in kinds() {
        let (_git, git_dir) = fixture(kind, "files")?;
        let (_native, native_dir) = fixture(kind, "files")?;
        for dir in [&git_dir, &native_dir] {
            let pack_dir = dir.join(".git/objects/pack");
            let idx = std::fs::read_dir(&pack_dir)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| path.extension().is_some_and(|ext| ext == "idx"))
                .expect("the fixture has a pack");
            // an index with a `.keep` file, whose pack is gone, which is garbage with everything that belongs to it
            std::fs::copy(&idx, pack_dir.join("pack-garbage.idx"))?;
            std::fs::write(pack_dir.join("pack-garbage.keep"), "")?;
            // a `.keep` file alone, which isn't garbage, and an index that is being removed by something that got killed
            std::fs::write(pack_dir.join("pack-lonely.keep"), "")?;
        }
        gix_testtools::git(&git_dir, "gc -q")?;
        let outcome = open(&native_dir)?.gc(gc::Options::default(), &never())?;
        assert_eq!(
            outcome.pack_garbage_removed, 2,
            "{kind}: the index and the `.keep` file of the name without a pack"
        );
        for dir in [&git_dir, &native_dir] {
            let pack_dir = dir.join(".git/objects/pack");
            assert!(
                !pack_dir.join("pack-garbage.idx").exists(),
                "{kind}: the index is removed"
            );
            assert!(
                !pack_dir.join("pack-garbage.keep").exists(),
                "{kind}: and what belongs to it"
            );
            assert!(
                pack_dir.join("pack-lonely.keep").exists(),
                "{kind}: a `.keep` file alone stays"
            );
        }
    }
    Ok(())
}

fn run_options(auto: bool, tasks: &[Task]) -> run::Options {
    let mut options = run::Options::default();
    options.auto = auto;
    options.tasks = tasks.to_vec();
    options
}

#[test]
fn tasks_that_are_asked_for_do_what_git_maintenance_does() -> TestResult {
    // each hash and each reference format is combined with each task, but not every hash with every format, which costs a fixture each
    for (kind, format) in kinds().zip(formats()) {
        for task in [
            Task::PackRefs,
            Task::ReflogExpire,
            Task::LooseObjects,
            Task::GeometricRepack,
            Task::Gc,
        ] {
            let (_git, git_dir) = fixture(kind, format)?;
            let (_native, native_dir) = fixture(kind, format)?;
            let before = state(&native_dir)?;
            gix_testtools::git(&git_dir, &format!("maintenance run --quiet --task={}", task.name()))?;

            let outcome = open(&native_dir)?.run_maintenance(run_options(false, &[task]), &never())?;
            assert!(outcome.is_success(), "{kind} {format} {task:?}: {outcome:?}");
            let (expected, actual) = (state(&git_dir)?, state(&native_dir)?);
            if !(format == "reftable" && task == Task::PackRefs) {
                // a few references are one table that needs no merging
                assert_ne!(before, actual, "{kind} {format} {task:?}: something changed");
            }
            assert_eq!(
                actual,
                expected,
                "{kind} {format} {task:?}: the same objects, packs, references and reflog as with `git maintenance run --task={}`",
                task.name()
            );
            assert_healthy(&native_dir, &format!("{kind} {format} {task:?}"))?;
        }
    }
    Ok(())
}

#[test]
fn what_is_due_in_an_automatic_run_is_what_git_finds_due() -> TestResult {
    // `maintenance.<task>.auto`, which tasks are due with it, and what they do
    let cases: [(&str, Settings, &[Task]); 3] = [
        ("nothing is due at the defaults", &[], &[]),
        (
            "reflogs expire when `HEAD` has enough entries to expire",
            &[("maintenance.reflog-expire.auto", "1")],
            &[Task::ReflogExpire],
        ),
        (
            "loose objects are due when there are enough of them",
            &[("maintenance.loose-objects.auto", "5")],
            &[Task::LooseObjects],
        ),
    ];
    let tasks = [Task::LooseObjects, Task::PackRefs, Task::ReflogExpire];
    for (what, settings, due) in cases {
        let (_git, git_dir) = fixture(Kind::Sha1, "files")?;
        let (_native, native_dir) = fixture(Kind::Sha1, "files")?;
        for dir in [&git_dir, &native_dir] {
            git_config(dir, settings)?;
        }
        let before = state(&git_dir)?;
        let task_args: Vec<_> = tasks.iter().map(|task| format!("--task={}", task.name())).collect();
        gix_testtools::git(
            &git_dir,
            &format!("maintenance run --auto --quiet {}", task_args.join(" ")),
        )?;
        let outcome = open(&native_dir)?.run_maintenance(run_options(true, &tasks), &never())?;
        let ran: Vec<_> = outcome
            .tasks
            .iter()
            .filter(|(_, result)| matches!(result, TaskOutcome::Ran))
            .map(|(task, _)| *task)
            .collect();
        assert_eq!(ran, due, "{what}: the tasks that ran, in {outcome:?}");
        assert_eq!(
            state(&native_dir)? != before,
            state(&git_dir)? != before,
            "{what}: something changes if it does with Git, which ran {outcome:?}"
        );
    }
    Ok(())
}

#[test]
fn the_tasks_come_from_the_strategy_and_what_is_not_implemented_is_reported() -> TestResult {
    let (_dir, repo_dir) = fixture(Kind::Sha1, "files")?;
    // an automatic run with limits that are never reached finds nothing due and so touches nothing
    git_config(
        &repo_dir,
        &[
            ("gc.auto", "100000"),
            ("maintenance.geometric-repack.auto", "100000"),
            ("maintenance.reflog-expire.auto", "100000"),
            ("maintenance.loose-objects.auto", "100000"),
        ],
    )?;
    let selected = |settings: &[(&str, &str)]| -> TestResult<Vec<Task>> {
        git_config(&repo_dir, settings)?;
        let outcome = open(&repo_dir)?.run_maintenance(run_options(true, &[]), &never())?;
        for (key, _) in settings {
            gix_testtools::git(&repo_dir, &format!("config --unset {key}"))?;
        }
        Ok(outcome.tasks.into_iter().map(|(task, _)| task).collect())
    };
    assert_eq!(
        selected(&[])?,
        [
            Task::GeometricRepack,
            Task::CommitGraph,
            Task::PackRefs,
            Task::ReflogExpire,
            Task::WorktreePrune,
            Task::RerereGc
        ],
        "the default is the geometric strategy, in the order in which Git runs the tasks"
    );
    assert_eq!(
        selected(&[("maintenance.strategy", "gc")])?,
        [Task::Gc],
        "the `gc` strategy is just `gc`"
    );
    assert_eq!(
        selected(&[
            ("maintenance.strategy", "gc"),
            ("maintenance.pack-refs.enabled", "true"),
            ("maintenance.gc.enabled", "false")
        ])?,
        [Task::PackRefs],
        "`enabled` adds and removes tasks of the strategy"
    );

    git_config(&repo_dir, &[("maintenance.strategy", "nonsense")])?;
    let repo = open(&repo_dir)?;
    assert!(
        repo.run_maintenance(run::Options::default(), &never()).is_err(),
        "an unknown strategy is an error, like in Git"
    );
    git_config(&repo_dir, &[("maintenance.strategy", "geometric")])?;
    let repo = open(&repo_dir)?;

    // explicitly asking for what isn't implemented is reported and doesn't fail
    let outcome = repo.run_maintenance(
        run_options(false, &[Task::Prefetch, Task::CommitGraph, Task::IncrementalRepack]),
        &never(),
    )?;
    assert!(outcome.is_success(), "{outcome:?}");
    assert!(
        outcome
            .tasks
            .iter()
            .all(|(_, result)| matches!(result, TaskOutcome::Unsupported(_))),
        "they are reported as unsupported: {outcome:?}"
    );
    Ok(())
}

#[test]
fn maintenance_does_not_run_while_its_lock_exists_like_with_git() -> TestResult {
    let (_git, git_dir) = fixture(Kind::Sha1, "files")?;
    let (_native, native_dir) = fixture(Kind::Sha1, "files")?;
    for dir in [&git_dir, &native_dir] {
        std::fs::write(dir.join(".git/objects/maintenance.lock"), "")?;
    }
    let before = state(&git_dir)?;
    gix_testtools::git(&git_dir, "maintenance run --quiet --task=gc")?;
    assert_eq!(state(&git_dir)?, before, "Git leaves things as they are");

    let outcome = open(&native_dir)?.run_maintenance(run_options(false, &[Task::Gc]), &never())?;
    assert!(outcome.locked_by.is_some() && outcome.tasks.is_empty(), "{outcome:?}");
    assert_eq!(state(&native_dir)?, before, "and so does native maintenance");
    assert!(
        native_dir.join(".git/objects/maintenance.lock").exists(),
        "a lock that isn't ours stays"
    );
    Ok(())
}

#[test]
fn what_gc_does_not_do_is_reported_if_there_was_something_to_do() -> TestResult {
    let (_dir, repo_dir) = fixture(Kind::Sha1, "files")?;
    let outcome = open(&repo_dir)?.gc(gc::Options::default(), &never())?;
    assert!(
        outcome.unsupported.is_empty(),
        "nothing is reported if there is nothing to do: {:?}",
        outcome.unsupported
    );

    let (_dir, repo_dir) = fixture(Kind::Sha1, "files")?;
    git_config(&repo_dir, &[("gc.bigPackThreshold", "1m"), ("gc.maxCruftSize", "1m")])?;
    std::fs::create_dir_all(repo_dir.join(".git/rr-cache/0123"))?;
    std::fs::create_dir_all(repo_dir.join(".git/worktrees/w1"))?;
    let outcome = open(&repo_dir)?.gc(gc::Options::default(), &never())?;
    assert_eq!(
        outcome.unsupported,
        ["gc.bigPackThreshold", "gc.maxCruftSize", "worktree-prune", "rerere-gc"],
        "the settings that are set and the tasks that have something to do"
    );
    Ok(())
}

#[test]
fn a_geometric_repack_of_all_packs_expires_what_is_unreachable_like_git_does() -> TestResult {
    for kind in kinds() {
        let (_git, git_dir) = fixture(kind, "files")?;
        let (_native, native_dir) = fixture(kind, "files")?;
        for dir in [&git_dir, &native_dir] {
            // One old pack with everything, even what only the reflog of `HEAD` mentions, and then another of a similar size, so
            // that all packs would be rolled up.
            gix_testtools::git(dir, "repack -a -d -q")?;
            let pack_dir = dir.join(".git/objects/pack");
            let first_pack_files: Vec<_> = std::fs::read_dir(&pack_dir)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<_, _>>()?;
            for n in 0..60 {
                std::fs::write(dir.join(format!("new{n}.txt")), format!("new {n}\n"))?;
            }
            gix_testtools::git(dir, "add -A")?;
            gix_testtools::git(dir, "commit -q -m new")?;
            gix_testtools::git(dir, "repack -d -q")?;
            // The first pack is from the year 2001, which is set last as writing objects that it has makes it newer.
            let old = std::time::UNIX_EPOCH + std::time::Duration::from_hours(978_307_200 / 3600);
            for path in first_pack_files {
                // the files of packs can't be written, which only Windows needs for this
                std::fs::File::options()
                    .read(true)
                    .write(cfg!(windows))
                    .open(path)?
                    .set_modified(old)?;
            }
            // what the reflog mentions is unreachable once its entries are gone
            gix_testtools::git(dir, "reflog expire --expire=now --all")?;
        }
        let before = state(&native_dir)?;
        gix_testtools::git(&git_dir, "maintenance run --quiet --task=geometric-repack")?;
        let outcome = open(&native_dir)?.run_maintenance(run_options(false, &[Task::GeometricRepack]), &never())?;
        assert!(outcome.is_success(), "{kind}: {outcome:?}");
        let (expected, actual) = (state(&git_dir)?, state(&native_dir)?);
        assert!(
            actual.objects.len() < before.objects.len(),
            "{kind}: what is unreachable and old is expired by the pack that rolls up everything"
        );
        assert_eq!(
            actual, expected,
            "{kind}: the same objects, packs and references as with `git maintenance run --task=geometric-repack`"
        );
        assert_healthy(&native_dir, &format!("{kind}"))?;
    }
    Ok(())
}

#[test]
fn a_reference_that_cannot_be_packed_does_not_keep_gc_from_reclaiming_objects() -> TestResult {
    let (_dir, repo_dir) = fixture(Kind::Sha1, "files")?;
    let before = state(&repo_dir)?;
    // somebody, like a push, holds the lock of `packed-refs`
    std::fs::write(repo_dir.join(".git/packed-refs.lock"), "")?;
    let result = open(&repo_dir)?.gc(gc::Options::default(), &never());
    assert!(result.is_err(), "the failure is reported: {result:?}");
    let after = state(&repo_dir)?;
    assert!(
        after.loose < before.loose && after.packs.0 > 0,
        "the loose objects were packed nonetheless, which Git does as well: {before:?} {after:?}"
    );
    assert!(
        !repo_dir.join(".git/gc.pid").exists(),
        "the lock of `gc` is released after the failure"
    );
    Ok(())
}

#[test]
fn a_repository_with_precious_objects_is_left_alone_without_an_error() -> TestResult {
    let (_dir, repo_dir) = fixture(Kind::Sha1, "files")?;
    git_config(
        &repo_dir,
        &[
            ("core.repositoryformatversion", "1"),
            ("extensions.preciousObjects", "true"),
        ],
    )?;
    let before = state(&repo_dir)?;
    let outcome = open(&repo_dir)?.gc(gc::Options::default(), &never())?;
    assert!(
        outcome.objects_not_touched.is_some() && outcome.repack.is_none() && outcome.prune.is_none(),
        "nothing is repacked or pruned, and it says why: {outcome:?}"
    );
    let after = state(&repo_dir)?;
    assert_eq!(
        (after.objects, after.loose, after.packs),
        (before.objects, before.loose, before.packs),
        "no object is removed or moved"
    );
    Ok(())
}
