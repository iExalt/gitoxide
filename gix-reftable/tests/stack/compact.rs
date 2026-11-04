use std::path::Path;

use bstr::ByteSlice;
use gix_hash::{Kind, ObjectId};
use gix_reftable::{
    Stack,
    record::{RefRecord, RefValue},
    stack::Options,
};

use crate::{
    Result,
    util::{self, HASHES, assert_healthy, git, git_ref_lines, git_supports_reftable, kind, ref_lines, reftable_dir},
};

fn ref_record(name: &str, update_index: u64, id: ObjectId) -> RefRecord {
    RefRecord {
        refname: name.into(),
        update_index,
        value: RefValue::Val1(id),
    }
}

/// Add a table with the reference `name` to `stack`, without compacting it.
fn add_ref(stack: &mut Stack, name: &str, id: ObjectId) -> Result {
    let mut addition = stack.addition()?;
    addition.add_table(|writer| writer.add_ref(ref_record(name, writer.limits().0, id)))?;
    addition.commit()?;
    Ok(())
}

fn list_names(dir: &Path) -> Result<Vec<String>> {
    Ok(std::fs::read_to_string(dir.join("tables.list"))?
        .lines()
        .map(str::to_owned)
        .collect())
}

/// The files in `dir`, sorted, which includes locks and temporary files.
fn files(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

#[test]
fn compacting_a_stack_written_by_git_leaves_one_table_with_everything_that_git_reads() -> Result {
    for object_format in HASHES {
        let Some(repo) = util::writable_git_written_tables(object_format)? else {
            return Ok(());
        };
        let repo = repo.path().join("stacked");
        let dir = reftable_dir(&repo);
        let main_log = git(&repo, &["reflog", "show", "--date=raw", "refs/heads/main"])?;
        let mut stack = Stack::open(&dir, kind(object_format))?;
        let old_tables = list_names(&dir)?;
        assert!(old_tables.len() >= 3);

        let compaction = stack.compact_all()?.expect("several tables to merge");
        assert_eq!(compaction.replaced, old_tables, "every table was merged, oldest first");
        let new_table = compaction.new_table.expect("references remain");
        assert_eq!(
            list_names(&dir)?,
            std::slice::from_ref(&new_table),
            "the list names only the new table"
        );
        let mut expected = ["tables.list".to_owned(), new_table];
        expected.sort();
        assert_eq!(
            files(&dir)?,
            expected,
            "the old tables, locks and temporary files are gone"
        );

        let merged = stack.snapshot().merged_with_tombstones();
        let mut iter = merged.ref_iter()?;
        while let Some(record) = iter.next_record()? {
            assert!(
                !record.is_deletion(),
                "nothing below the table needs hiding, so tombstones are dropped"
            );
        }
        assert_eq!(ref_lines(stack.snapshot().merged())?, git_ref_lines(&repo)?);
        assert_eq!(
            git(&repo, &["reflog", "show", "--date=raw", "refs/heads/main"])?,
            main_log,
            "{object_format}: reflogs are carried over"
        );
        assert_healthy(&repo, &format!("{object_format} after compacting"));

        // Git continues on top of the compacted stack.
        git(&repo, &["update-ref", "refs/heads/after-compaction", "refs/heads/main"])?;
        assert!(stack.reload()?);
        assert_eq!(ref_lines(stack.snapshot().merged())?, git_ref_lines(&repo)?);
        assert_healthy(&repo, &format!("{object_format} after Git continued"));
        assert!(stack.compact_all()?.is_some() || stack.snapshot().tables().len() < 2);
    }
    Ok(())
}

#[test]
fn compaction_of_fewer_than_two_tables_does_nothing() -> Result {
    let dir = gix_testtools::tempfile::tempdir()?;
    let mut stack = Stack::open(dir.path(), Kind::Sha1)?;
    assert_eq!(stack.compact_all()?, None, "an empty stack");
    add_ref(&mut stack, "refs/heads/a", ObjectId::null(Kind::Sha1))?;
    assert_eq!(stack.compact_all()?, None, "a single table");
    assert!(!stack.compaction_required());
    Ok(())
}

#[test]
fn a_stale_view_cannot_be_compacted_and_nothing_changes() -> Result {
    let Some(repo) = util::writable_git_written_tables("sha1")? else {
        return Ok(());
    };
    let repo = repo.path().join("stacked");
    let dir = reftable_dir(&repo);
    let mut stack = Stack::open(&dir, Kind::Sha1)?;
    git(&repo, &["update-ref", "refs/heads/raced", "refs/heads/main"])?;
    let before = (files(&dir)?, std::fs::read(dir.join("tables.list"))?);

    let err = stack
        .compact_all()
        .expect_err("the tables to merge may not be the ones that exist");
    assert!(err.is_conflict(), "{err:?}");
    assert_eq!((files(&dir)?, std::fs::read(dir.join("tables.list"))?), before);

    stack.reload()?;
    assert!(stack.compact_all()?.is_some(), "with a current view it works");
    Ok(())
}

#[test]
fn a_table_locked_by_somebody_else_stops_a_compaction_unless_the_rest_is_worth_it() -> Result {
    let dir = gix_testtools::tempfile::tempdir()?;
    let id = ObjectId::null(Kind::Sha1);
    let mut stack = Stack::open(dir.path(), Kind::Sha1)?;
    for name in ["refs/heads/a", "refs/heads/b", "refs/heads/c"] {
        add_ref(&mut stack, name, id)?;
    }
    let names = list_names(dir.path())?;
    assert_eq!(names.len(), 3);
    assert_eq!(
        stack.snapshot().compaction_sizes().len(),
        3,
        "tables of the same size are to be merged by the policy"
    );
    assert!(stack.compaction_required());

    // Somebody else, such as a Git that compacts, holds the oldest table.
    let lock = dir.path().join(format!("{}.lock", names[0]));
    std::fs::write(&lock, "")?;
    let before = (files(dir.path())?, std::fs::read(dir.path().join("tables.list"))?);

    let err = stack.compact_all().expect_err("all tables were asked for");
    assert!(
        err.is_retryable(),
        "a lock that is held isn't an error with the stack: {err:?}"
    );
    assert_eq!(
        (files(dir.path())?, std::fs::read(dir.path().join("tables.list"))?),
        before,
        "nothing changed"
    );

    let compaction = stack
        .auto_compact()?
        .expect("the two newer tables are still worth merging");
    assert_eq!(compaction.replaced, names[1..], "only what could be locked");
    assert_eq!(stack.snapshot().tables().len(), 2);
    assert!(lock.is_file(), "the lock of somebody else is left alone");
    assert_eq!(ref_lines(stack.snapshot().merged())?.len(), 3, "nothing was lost");
    Ok(())
}

#[test]
fn a_lock_of_a_table_that_somebody_left_behind_doesnt_stop_additions() -> Result {
    let dir = gix_testtools::tempfile::tempdir()?;
    let id = ObjectId::null(Kind::Sha1);
    let mut stack = Stack::open(dir.path(), Kind::Sha1)?;
    add_ref(&mut stack, "refs/heads/a", id)?;
    let name = list_names(dir.path())?.remove(0);
    std::fs::write(dir.path().join(format!("{name}.lock")), "")?;

    add_ref(&mut stack, "refs/heads/b", id)?;
    assert_eq!(stack.snapshot().tables().len(), 2);
    let err = stack
        .compact_all()
        .expect_err("the left-behind lock keeps the table from being merged");
    assert!(err.is_retryable(), "{err:?}");
    Ok(())
}

/// The update indices of the tables in `dir`, oldest first, which is what a compaction is about.
fn ranges(dir: &Path) -> Result<Vec<String>> {
    Ok(list_names(dir)?
        .into_iter()
        .map(|name| name[..name.len() - "-00000000.ref".len()].to_owned())
        .collect())
}

/// Run Git in `repo` without it compacting what it writes.
fn git_without_auto_compaction(repo: &Path, args: &[&str]) -> Result {
    let mut cmd = gix_testtools::git_command(repo);
    let output = cmd
        .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
        .args(args)
        .output()?;
    assert!(output.status.success(), "{args:?}: {}", output.stderr.as_bstr());
    Ok(())
}

#[test]
fn the_tables_that_the_fork_merges_are_those_that_git_merges_for_the_same_stack_at_every_step() -> Result {
    const STEPS: u32 = 40;
    for object_format in HASHES {
        for factor in [2u8, 4] {
            let Some(base) = gix_testtools::scripted_fixture_read_only_with_args_with_git_version(
                "make_replay_base.sh",
                [object_format],
                git_supports_reftable,
            )?
            else {
                return Ok(());
            };
            let commit_id = std::fs::read_to_string(base.join("commit-id"))?.trim().to_owned();
            let tmp = gix_testtools::tempfile::tempdir()?;
            let repo = tmp.path().join("repo");
            std::fs::create_dir(&repo)?;
            gix_testtools::copy_recursively_into_existing_dir(base.join("base"), &repo)?;
            git(&repo, &["config", "reftable.geometricFactor", &factor.to_string()])?;
            let dir = reftable_dir(&repo);
            let options = {
                let mut options = Options::default();
                options.geometric_factor = factor;
                options
            };

            let mut compacted_by_git = 0;
            for step in 1..=STEPS {
                // Git adds a table to the stack that it compacted before, without compacting it yet.
                git_without_auto_compaction(
                    &repo,
                    &[
                        "update-ref",
                        "-m",
                        &format!("u{step}"),
                        &format!("refs/heads/g{step}"),
                        &commit_id,
                    ],
                )?;
                // The same tables, one copy for each, are compacted by Git and by this crate.
                let copy = tmp.path().join("copy");
                std::fs::remove_dir_all(&copy).ok();
                std::fs::create_dir(&copy)?;
                gix_testtools::copy_recursively_into_existing_dir(&dir, &copy)?;
                let before = ranges(&dir)?;

                let mut fork = Stack::open_with_options(&copy, kind(object_format), options)?;
                let compaction = fork.auto_compact()?;
                git(&repo, &["pack-refs", "--auto"])?;
                assert_eq!(
                    ranges(&copy)?,
                    ranges(&dir)?,
                    "{object_format} factor {factor} step {step}: from {before:?}, the fork leaves what Git leaves"
                );
                compacted_by_git += usize::from(before != ranges(&dir)?);
                assert_eq!(compaction.is_some(), before != ranges(&dir)?);
            }
            assert!(
                compacted_by_git > 10,
                "the replay exercises compactions: {compacted_by_git}"
            );
            let fork = Stack::open(&dir, kind(object_format))?;
            assert_eq!(ref_lines(fork.snapshot().merged())?, git_ref_lines(&repo)?);
        }
    }
    Ok(())
}

#[test]
fn merging_everything_gives_what_git_writes_for_pack_refs_all() -> Result {
    for object_format in HASHES {
        let Some(fixture) = util::git_written_tables(object_format)? else {
            return Ok(());
        };
        let tmp = gix_testtools::tempfile::tempdir()?;
        let (by_git, by_fork) = (tmp.path().join("git"), tmp.path().join("fork"));
        for target in [&by_git, &by_fork] {
            std::fs::create_dir(target)?;
            gix_testtools::copy_recursively_into_existing_dir(fixture.join("stacked-plain"), target)?;
        }
        git(&by_git, &["pack-refs", "--all"])?;
        let mut stack = Stack::open(reftable_dir(&by_fork), kind(object_format))?;
        assert!(stack.compact_all()?.is_some());

        let git_table = util::table_names(&by_git)?;
        let fork_table = util::table_names(&by_fork)?;
        assert_eq!((git_table.len(), fork_table.len()), (1, 1), "a single table each");
        let git_bytes = std::fs::read(reftable_dir(&by_git).join(&git_table[0]))?;
        let fork_bytes = std::fs::read(reftable_dir(&by_fork).join(&fork_table[0]))?;
        let log_offset = util::log_section_offset(&git_bytes);
        assert_eq!(
            util::log_section_offset(&fork_bytes),
            log_offset,
            "{object_format}: the log section starts at the same place"
        );
        assert!(
            fork_bytes[..log_offset] == git_bytes[..log_offset],
            "{object_format}: references, padding and indices are those of Git's compaction, first difference at {:?}",
            fork_bytes.iter().zip(&git_bytes).position(|(a, b)| a != b)
        );
        assert_eq!(
            fork_table[0][..fork_table[0].len() - "-00000000.ref".len()],
            git_table[0][..git_table[0].len() - "-00000000.ref".len()],
            "the table covers the same update indices"
        );
        let (fork_logs, git_logs) = (
            util::records_and_options(&gix_reftable::Table::open(reftable_dir(&by_fork).join(&fork_table[0]))?)?.1,
            util::records_and_options(&gix_reftable::Table::open(reftable_dir(&by_git).join(&git_table[0]))?)?.1,
        );
        assert_eq!(fork_logs, git_logs, "{object_format}: the same reflog entries");
    }
    Ok(())
}

#[test]
fn deleted_references_stay_hidden_when_a_locked_bottom_table_narrows_the_compaction() -> Result {
    let dir = gix_testtools::tempfile::tempdir()?;
    let id = ObjectId::null(Kind::Sha1);
    let mut stack = Stack::open(dir.path(), Kind::Sha1)?;
    add_ref(&mut stack, "refs/heads/x", id)?;
    {
        let mut addition = stack.addition()?;
        addition.add_table(|writer| {
            writer.add_ref(RefRecord {
                refname: "refs/heads/x".into(),
                update_index: writer.limits().0,
                value: RefValue::Deletion,
            })
        })?;
        addition.commit()?;
    }
    add_ref(&mut stack, "refs/heads/y", id)?;
    let names = list_names(dir.path())?;
    assert_eq!(names.len(), 3);
    assert!(stack.snapshot().find_ref(b"refs/heads/x")?.is_none(), "x is deleted");

    // The table that has `x` is held, so merging starts above it and the tombstone must go on hiding it.
    std::fs::write(dir.path().join(format!("{}.lock", names[0])), "")?;
    let compaction = stack.auto_compact()?.expect("the two newer tables are merged");
    assert_eq!(compaction.replaced, names[1..]);
    assert!(
        stack.snapshot().find_ref(b"refs/heads/x")?.is_none(),
        "x didn't come back"
    );
    let merged = stack.snapshot().merged_with_tombstones();
    let mut iter = merged.ref_iter()?;
    let mut tombstones = 0;
    while let Some(record) = iter.next_record()? {
        tombstones += usize::from(record.is_deletion());
    }
    assert_eq!(tombstones, 1, "the tombstone was kept, as something below it remains");
    Ok(())
}

#[test]
fn tables_nobody_lists_are_cleaned_up_unless_they_may_belong_to_a_writer_that_is_busy() -> Result {
    let dir = gix_testtools::tempfile::tempdir()?;
    let id = ObjectId::null(Kind::Sha1);
    let mut stack = Stack::open(dir.path(), Kind::Sha1)?;
    add_ref(&mut stack, "refs/heads/a", id)?;
    add_ref(&mut stack, "refs/heads/b", id)?;
    let listed = list_names(dir.path())?;
    let max = stack.snapshot().tables().last().expect("two tables").max_update_index();

    // A table which was left behind by a compaction that got interrupted, as its tables are part of the stack still.
    let table_bytes = std::fs::read(dir.path().join(&listed[0]))?;
    std::fs::write(
        dir.path().join("0x000000000001-0x000000000001-deadbeef.ref"),
        &table_bytes,
    )?;
    // One that a writer is adding right now: it isn't listed yet, and goes beyond what the stack knows.
    let newer = {
        let mut writer = gix_reftable::Writer::new(Vec::new(), Kind::Sha1, Default::default())?;
        writer.set_limits(max + 1, max + 1)?;
        writer.add_ref(ref_record("refs/heads/in-flight", max + 1, id))?;
        writer.finish()?
    };
    std::fs::write(dir.path().join("0x000000000009-0x000000000009-cafebabe.ref"), &newer)?;
    // What is no table, or can't be read, stays as well.
    std::fs::write(
        dir.path().join("0x000000000001-0x000000000001-00000000.temp.AbCdEf"),
        &table_bytes,
    )?;
    std::fs::write(dir.path().join("broken.ref"), b"no table")?;
    std::fs::write(dir.path().join("notes.txt"), b"mine")?;

    stack.clean()?;
    let remaining = files(dir.path())?;
    assert!(
        !remaining.contains(&"0x000000000001-0x000000000001-deadbeef.ref".to_owned()),
        "an unlisted table that the stack has superseded is removed: {remaining:?}"
    );
    for kept in [
        "0x000000000009-0x000000000009-cafebabe.ref",
        "0x000000000001-0x000000000001-00000000.temp.AbCdEf",
        "broken.ref",
        "notes.txt",
        "tables.list",
    ] {
        assert!(remaining.contains(&kept.to_owned()), "{kept} stays: {remaining:?}");
    }
    for name in &listed {
        assert!(remaining.contains(name), "listed tables are never removed");
    }
    assert!(
        !remaining.iter().any(|name| name.ends_with("tables.list.lock")),
        "the lock that cleaning needs is released"
    );
    Ok(())
}

#[test]
fn a_reference_and_its_reflog_are_found_without_iterating_everything() -> Result {
    let Some(repo) = util::writable_git_written_tables("sha1")? else {
        return Ok(());
    };
    let repo = repo.path().join("compacted");
    let stack = Stack::open(reftable_dir(&repo), Kind::Sha1)?;
    let snapshot = stack.snapshot();

    let main = snapshot.find_ref(b"refs/heads/main")?.expect("main exists");
    assert_eq!(main.refname, "refs/heads/main");
    let expected = git(&repo, &["rev-parse", "refs/heads/main"])?;
    assert!(matches!(main.value, RefValue::Val1(id) if id.to_string().as_bytes() == expected.trim()));
    assert!(snapshot.find_ref(b"refs/heads/does-not-exist")?.is_none());
    assert!(
        snapshot.find_ref(b"refs/heads/main2")?.is_none(),
        "only the exact name matches"
    );

    let log = snapshot
        .reflog(b"refs/heads/main")?
        .collect::<gix_error::Result<Vec<_>>>()?;
    assert_eq!(log.len(), 3, "main was created and advanced twice");
    assert!(
        log.windows(2).all(|pair| pair[0].update_index > pair[1].update_index),
        "newest first"
    );
    assert!(log.iter().all(|record| record.refname == "refs/heads/main"));
    assert_eq!(snapshot.reflog(b"refs/heads/does-not-exist")?.count(), 0);
    Ok(())
}
