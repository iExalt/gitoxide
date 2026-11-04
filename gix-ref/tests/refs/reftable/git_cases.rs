//! A selection of the cases of Git's reftable tests that apply to a store of references, named after the test they come
//! from, with Git as the judge of what the store left behind.
//!
//! This file doesn't cover all of `t0610-reftable-basics.sh`, `t0613-reftable-write-options.sh` and
//! `t0614-reftable-fsck.sh`. Most of their cases drive commands this store doesn't have, like `pack-refs`, `branch --copy`,
//! `stash`, `rebase`, `notes` and `gc`. Where a case is about what happens to references, it is here, in `write.rs`,
//! `read.rs`, `reflog_edit.rs` or in `gix`'s tests, as listed below.
//!
//! Covered elsewhere:
//!
//! * `init: creates basic reftable structures`, `init: sha256 …` and the three `clone:` cases are in `gix`'s tests
//!   `the_new_repository_looks_like_the_one_git_creates` and `cloning_into_reftables_has_the_references_and_reflogs_git_makes`.
//! * `ref transaction: refuses to write ref causing F/D conflict`, `updating same ref multiple times fails`, `ref deletion`
//!   and `timeout acquiring tables.list lock` are in `write.rs`, as are the worktree cases that change one stack or both
//!   and `reflog: updates via HEAD update HEAD reflog`. `deleting ref with invalid name fails` can't be expressed, as a
//!   `FullName` is validated when it is made.
//! * `reflog: can store empty logs`, `expiry empties reflog` and `can be deleted` are in `reflog_edit.rs`.
//! * `basic: show-ref`, `for-each-ref can print symrefs` and the reading of reflogs are in `read.rs`.
//! * The four auto-compaction cases are met by `stacks_are_compacted_after_enough_transactions_and_git_keeps_reading_them`
//!   in `write.rs` and by the compaction tests of `gix-reftable`. `GIT_TEST_REFTABLE_AUTOCOMPACTION` isn't honoured, and
//!   a compaction that fails after the commit is ignored and never changes the outcome of the transaction.
//! * `block size exceeding maximum supported size` and `restart interval exceeding maximum supported interval` are in
//!   `gix`'s `ref_storage.rs`, with the other invalid settings.
//! * `no errors reported on a well formed repository` is `Fixture::git_agrees_that_nothing_is_wrong` throughout.
//! * `init: reinitializing` isn't covered here, as the store doesn't initialize repositories.
//!
//! Not covered, though they apply: `write options can be set up via onbranch condition` (the configuration is read after
//! `HEAD` was read through the store, which nothing tests), and `ref iterator: bogus names are flagged` and
//! `missing object IDs are not flagged`.
//!
//! Not applicable, as they test commands: the five `pack-refs` cases and their worktree variants, `branch` copy and move
//! and their directory/file cases, `reflog: can switch to previous branch`, `stash`, `rebase`, `cherry-pick`, `notes`,
//! `can check out unborn branch`, `show-ref fails with empty repository`, `commit and list refs`, `can write large
//! commit message`, `adds worktree with detached HEAD`, and `invalid symref gets reported` (`fsck` only).
//!
//! Known limits instead of cases: `can skip object ID verification` (objects are never verified), `writes are synced`
//! (flushing is tested with `strace` in `gix-ref`'s transaction tests), `peeled tags are stored` and `writes do not
//! persist peeled value for invalid tags` (tags are written as one value, so there is no peeled value to persist),
//! `object index gets written by default` and `can be disabled` (no object index is written), and `FETCH_HEAD` (Git keeps
//! it in a file, and the store refuses it).
use std::time::Duration;

use gix_ref::{
    Target,
    reftable::{StackOptions, Store},
    transaction::{Change, PreviousValue, RefEdit, RefLog},
    view::transaction::NameConflict,
};
use gix_testtools::TestResult;

use super::write::{Fixture, commit, create, name};
use crate::fixture_hash_kind;

/// The path of the newest table of the stack at `dir`.
fn newest_table(fixture: &Fixture, dir: &str) -> TestResult<std::path::PathBuf> {
    let tables = fixture.tables(dir)?;
    Ok(fixture
        .dir
        .path()
        .join(dir)
        .join(tables.last().expect("a stack has tables")))
}

#[test]
fn t0610_ref_transaction_corrupted_tables_cause_failure() -> TestResult {
    let f = Fixture::new()?;
    let list_before = std::fs::read_to_string(f.dir.path().join("repo/.git/reftable/tables.list"))?;
    std::fs::write(newest_table(&f, "repo/.git/reftable")?, b"this is not a table")?;

    let store = f.main();
    assert!(
        gix_ref::Store::from(store.clone())
            .as_ref()
            .try_find("refs/heads/main")
            .is_err(),
        "reading fails instead of reporting that the reference doesn't exist"
    );
    commit(&store, vec![create("refs/heads/after-corruption", f.c1, "x")])
        .expect_err("a transaction doesn't build upon what it can't read");
    assert_eq!(
        std::fs::read_to_string(f.dir.path().join("repo/.git/reftable/tables.list"))?,
        list_before,
        "the failed transaction leaves the list as it was"
    );
    // Git 2.56 reads nothing from such a stack and crashes in `refs verify`, so what is compared is whether it finds a reference.
    assert!(
        f.git("repo", "rev-parse --verify -q refs/heads/main").is_err(),
        "Git can't find the reference either, so this is a corruption and not a limitation of ours"
    );
    Ok(())
}

#[test]
fn t0610_ref_transaction_corrupted_tables_list_causes_failure() -> TestResult {
    let f = Fixture::new()?;
    let list = f.dir.path().join("repo/.git/reftable/tables.list");
    let mut contents = std::fs::read_to_string(&list)?;
    contents.push_str("0x000000000099-0x000000000099-deadbeef.ref\n");
    std::fs::write(&list, &contents)?;

    let store = f.main();
    gix_ref::Store::from(store.clone())
        .as_ref()
        .try_find("refs/heads/main")
        .expect_err("a table that is listed but doesn't exist is an error and not an empty stack");
    commit(&store, vec![create("refs/heads/after-corruption", f.c1, "x")])
        .expect_err("nothing is written on top of a list that names a missing table");
    assert_eq!(std::fs::read_to_string(&list)?, contents, "the list is untouched");
    // Git 2.56 reads nothing from such a stack and crashes in `refs verify`, so what is compared is whether it finds a reference.
    assert!(
        f.git("repo", "rev-parse --verify -q refs/heads/main").is_err(),
        "Git can't find the reference either, so this is a corruption and not a limitation of ours"
    );
    Ok(())
}

#[test]
fn t0610_ref_transaction_empty_transaction_in_empty_repo() -> TestResult {
    let tmp = gix_testtools::tempfile::tempdir()?;
    gix_testtools::git(
        tmp.path(),
        &format!(
            "init -q -b main --ref-format=reftable --object-format={} .",
            fixture_hash_kind()
        ),
    )?;
    gix_testtools::git(tmp.path(), "commit -q --allow-empty -m A")?;
    let store = Store::at(tmp.path().join(".git"), fixture_hash_kind());
    // Like Git's case, which deletes the branch and then `HEAD` itself, no reference is left, and the stack
    // consists of tables with tombstones.
    let delete = |reference: &str| RefEdit {
        change: Change::Delete {
            expected: PreviousValue::Any,
            log: RefLog::AndReference,
        },
        name: name(reference),
        deref: false,
    };
    commit(&store, vec![delete("refs/heads/main")])?;
    commit(&store, vec![delete("HEAD")])?;
    assert_eq!(
        gix_testtools::git(tmp.path(), "for-each-ref")?,
        "",
        "there are no references to begin with"
    );
    commit(&store, Vec::new())?;
    assert_eq!(
        gix_testtools::git(tmp.path(), "for-each-ref")?,
        "",
        "an empty transaction succeeds and creates no reference"
    );
    gix_testtools::git(tmp.path(), "refs verify")?;
    Ok(())
}

#[test]
fn t0610_ref_transaction_retry_acquiring_tables_list_lock() -> TestResult {
    let f = Fixture::new()?;
    let lock = f.dir.path().join("repo/.git/reftable/tables.list.lock");
    std::fs::write(&lock, b"")?;
    let mut options = StackOptions::default();
    options.lock_timeout = Some(Duration::from_secs(20));
    let mut store = f.main();
    store.set_stack_options(Ok(options));

    let release = std::thread::spawn({
        let lock = lock.clone();
        move || {
            std::thread::sleep(Duration::from_millis(300));
            std::fs::remove_file(lock)
        }
    });
    commit(&store, vec![create("refs/heads/waited", f.c1, "x")])?;
    release.join().expect("the thread doesn't panic")?;
    assert_eq!(
        f.git("repo", "rev-parse refs/heads/waited")?.trim(),
        f.c1.to_string(),
        "the transaction waited for the lock to go away instead of failing"
    );
    Ok(())
}

#[test]
fn t0610_ref_transaction_many_concurrent_writers() -> TestResult {
    let f = Fixture::new()?;
    let git_dir = f.dir.path().join("repo/.git");
    let (writers, per_writer) = (6, 5);
    std::thread::scope(|scope| -> TestResult {
        let handles: Vec<_> = (0..writers)
            .map(|writer| {
                let git_dir = git_dir.clone();
                let id = f.c1;
                scope.spawn(move || -> TestResult {
                    let mut options = StackOptions::default();
                    options.lock_timeout = Some(Duration::from_secs(60));
                    let mut store = Store::at(git_dir, fixture_hash_kind());
                    store.set_stack_options(Ok(options));
                    for n in 0..per_writer {
                        commit(
                            &store,
                            vec![create(&format!("refs/heads/writer-{writer}-{n}"), id, "x")],
                        )?;
                    }
                    Ok(())
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("writers don't panic")?;
        }
        Ok(())
    })?;
    assert_eq!(
        f.git("repo", "for-each-ref --format=%(refname) refs/heads/writer-*")?
            .lines()
            .count(),
        writers * per_writer,
        "every transaction of every writer is there, none was lost to a race"
    );
    f.git_agrees_that_nothing_is_wrong()?;
    Ok(())
}

#[test]
fn t0610_ref_transaction_symbolic_self_reference_is_deleted_without_dereferencing_only() -> TestResult {
    let f = Fixture::new()?;
    f.git("repo", "symbolic-ref refs/heads/self refs/heads/self")?;
    let delete = |deref: bool| RefEdit {
        change: Change::Delete {
            expected: PreviousValue::Any,
            log: RefLog::AndReference,
        },
        name: name("refs/heads/self"),
        deref,
    };
    commit(&f.main(), vec![delete(true)])
        .expect_err("dereferencing a reference that points to itself never ends, so it is an error");
    // Git hides a dangling symbolic reference from its listings, so what is stored is looked at without following it.
    let exists = || -> TestResult<bool> {
        Ok(gix_ref::Store::from(f.main())
            .as_ref()
            .try_find("refs/heads/self")?
            .is_some())
    };
    assert!(exists()?, "it is still there after the failure");
    commit(&f.main(), vec![delete(false)])?;
    assert!(!exists()?, "it is gone");
    f.git_agrees_that_nothing_is_wrong()?;
    Ok(())
}

#[test]
fn t0610_ref_transaction_creating_symbolic_ref_fails_with_a_directory_file_conflict() -> TestResult {
    let f = Fixture::new()?;
    let symbolic = |reference: &str, target: &str| {
        RefEdit::update(
            name(reference),
            Target::Symbolic(name(target)),
            PreviousValue::MustNotExist,
            "symref",
        )
    };
    let err = commit(&f.main(), vec![symbolic("refs/heads/main/below", "refs/heads/side")])
        .expect_err("main is a reference, so it can't be a directory");
    assert!(err.downcast_any_ref::<NameConflict>().is_some(), "{err:?}");
    assert!(
        f.git("repo", "symbolic-ref refs/heads/main/below").is_err(),
        "nothing was written"
    );
    commit(&f.main(), vec![symbolic("refs/heads/alias-of-side", "refs/heads/side")])?;
    assert_eq!(
        f.git("repo", "symbolic-ref refs/heads/alias-of-side")?.trim(),
        "refs/heads/side"
    );
    Ok(())
}

#[test]
fn t0610_reflog_can_delete_separate_reflog_entries() -> TestResult {
    let ours = Fixture::new()?;
    let git = Fixture::new()?;
    let before = ours.git("repo", "reflog show --format=%gs refs/heads/main --")?;
    let lines = before.lines().count();
    assert!(lines >= 2, "the fixture has something to delete from: {before}");
    git.git("repo", "reflog delete refs/heads/main@{1}")?;

    // `main@{1}` is the second most recent line, and the predicate is asked from the oldest to the most recent one.
    let mut asked = 0;
    let outcome = gix_ref::Store::from(ours.main()).as_ref().reflog_expire(
        name("refs/heads/main").as_ref(),
        Default::default(),
        |_, _| {
            asked += 1;
            asked != lines - 1
        },
    )?;
    assert_eq!((outcome.removed, outcome.kept), (1, lines - 1));
    assert_eq!(
        ours.git("repo", "reflog show --format=%gs refs/heads/main --")?,
        git.git("repo", "reflog show --format=%gs refs/heads/main --")?,
        "Git left the same lines when it deleted that one"
    );
    ours.git_agrees_that_nothing_is_wrong()?;
    Ok(())
}

#[test]
fn t0610_worktree_creating_a_per_worktree_reference_from_the_main_repository_updates_the_stack_of_the_worktree()
-> TestResult {
    let f = Fixture::new()?;
    commit(
        &f.main(),
        vec![create(
            "worktrees/w1/refs/worktree/from-main",
            f.c2,
            "from the main repository",
        )],
    )?;
    assert_eq!(
        f.git("w1", "rev-parse refs/worktree/from-main")?.trim(),
        f.c2.to_string(),
        "the worktree sees it as its own reference"
    );
    assert!(
        f.git("repo", "rev-parse --verify -q refs/worktree/from-main").is_err(),
        "and the main worktree doesn't"
    );
    assert_eq!(
        gix_ref::Store::from(f.worktree())
            .as_ref()
            .find("refs/worktree/from-main")?
            .target
            .try_id()
            .map(ToOwned::to_owned),
        Some(f.c2)
    );
    f.git_agrees_that_nothing_is_wrong()?;
    Ok(())
}

/// The block size in the header of a table, which is the 24 bits after the magic and the version.
fn block_size(table: &std::path::Path) -> TestResult<u32> {
    let header = std::fs::read(table)?;
    Ok(u32::from_be_bytes([0, header[5], header[6], header[7]]))
}

#[test]
fn t0613_default_write_options() -> TestResult {
    let f = Fixture::new()?;
    commit(&f.main(), vec![create("refs/heads/defaults", f.c1, "x")])?;
    assert_eq!(
        block_size(&newest_table(&f, "repo/.git/reftable")?)?,
        4096,
        "the default block size is the one Git has"
    );
    Ok(())
}

#[test]
fn t0613_disabled_reflog_writes_no_log_records() -> TestResult {
    let f = Fixture::new()?;
    let mut store = gix_ref::Store::from(f.main());
    store.set_write_reflog(gix_ref::store::WriteReflog::Disable);
    commit_with(&store, vec![create("refs/heads/unlogged", f.c1, "x")])?;
    assert!(
        f.git("repo", "reflog exists refs/heads/unlogged").is_err(),
        "no reflog was created although branches get one otherwise"
    );
    Ok(())
}

fn commit_with(store: &gix_ref::Store, edits: Vec<RefEdit>) -> gix_error::Result<gix_ref::view::transaction::Outcome> {
    store
        .as_ref()
        .transaction()
        .prepare(edits, super::write::lock())?
        .commit(super::write::committer().to_ref(&mut gix_date::parse::TimeBuf::default()))
}

#[test]
fn t0613_many_refs_and_small_blocks_result_in_multiple_blocks_that_git_reads() -> TestResult {
    for (block, restart_interval, refs) in [(0, 0, 300), (256, 1, 12)] {
        let f = Fixture::new()?;
        let mut options = StackOptions::default();
        options.write.block_size = block;
        if restart_interval != 0 {
            options.write.restart_interval = restart_interval;
        }
        let mut store = f.main();
        store.set_stack_options(Ok(options));
        let edits: Vec<_> = (0..refs)
            .map(|n| create(&format!("refs/heads/branch-{n}"), f.c1, ""))
            .collect();
        commit(&store, edits)?;
        let table = newest_table(&f, "repo/.git/reftable")?;
        let expected_block_size = if block == 0 { 4096 } else { block };
        assert_eq!(block_size(&table)?, expected_block_size, "the option reached the table");
        assert!(
            std::fs::metadata(&table)?.len() > u64::from(expected_block_size) * 2,
            "{refs} references don't fit into one block of {expected_block_size} bytes"
        );
        assert_eq!(
            f.git("repo", "for-each-ref --format=%(refname) refs/heads/branch-*")?
                .lines()
                .count(),
            refs,
            "Git finds every reference in the blocks and restarts that were written"
        );
        f.git_agrees_that_nothing_is_wrong()?;
    }
    Ok(())
}

#[test]
fn t0613_tiny_block_size_and_large_reflog_message_fail_the_transaction_and_change_nothing() -> TestResult {
    for (block, message) in [(50, "x".to_owned()), (100, "a".repeat(500))] {
        let f = Fixture::new()?;
        let list_before = std::fs::read_to_string(f.dir.path().join("repo/.git/reftable/tables.list"))?;
        let mut options = StackOptions::default();
        options.write.block_size = block;
        let mut store = f.main();
        store.set_stack_options(Ok(options));
        commit(&store, vec![create("refs/heads/logme", f.c1, &message)])
            .expect_err("an entry that doesn't fit into a block is an error, like in Git: `entry too large`");
        assert_eq!(
            std::fs::read_to_string(f.dir.path().join("repo/.git/reftable/tables.list"))?,
            list_before,
            "block size {block}: nothing was published"
        );
        assert!(f.git("repo", "rev-parse --verify -q refs/heads/logme").is_err());
        f.git_agrees_that_nothing_is_wrong()?;
    }
    Ok(())
}

#[test]
fn t0614_worktree_stacks_can_be_verified_after_both_stacks_were_written_by_the_store() -> TestResult {
    let f = Fixture::new()?;
    commit(
        &f.worktree(),
        vec![
            create("refs/heads/shared-by-w1", f.c1, "x"),
            create("refs/worktree/private-to-w1", f.c2, "y"),
        ],
    )?;
    f.git("repo", "refs verify")?;
    f.git("w1", "refs verify")?;
    f.git("repo", "fsck --no-dangling")?;
    Ok(())
}
