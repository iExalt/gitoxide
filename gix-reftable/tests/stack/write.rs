use std::{
    path::Path,
    time::{Duration, Instant},
};

use bstr::ByteSlice;
use gix_error::ErrorExt;
use gix_hash::{Kind, ObjectId};
use gix_reftable::{
    Stack,
    record::{LogRecord, LogValue, Record, RefRecord, RefValue},
    stack::Options,
};

use crate::{
    Result,
    util::{self, HASHES, assert_healthy, git, git_ref_lines, kind, ref_lines, reftable_dir},
};

fn id_of(repo: &Path, rev: &str) -> Result<ObjectId> {
    Ok(ObjectId::from_hex(git(repo, &["rev-parse", rev])?.trim())?)
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

fn ref_record(name: &str, update_index: u64, id: ObjectId) -> RefRecord {
    RefRecord {
        refname: name.into(),
        update_index,
        value: RefValue::Val1(id),
    }
}

#[test]
fn tables_added_on_top_of_a_stack_written_by_git_are_read_by_git_which_then_adds_and_compacts_its_own() -> Result {
    for object_format in HASHES {
        let Some(repo) = util::writable_git_written_tables(object_format)? else {
            return Ok(());
        };
        let repo = repo.path().join("stacked");
        let main = id_of(&repo, "refs/heads/main")?;
        let mut stack = Stack::open(reftable_dir(&repo), kind(object_format))?;
        let top_max = stack.snapshot().tables().last().expect("a stack").max_update_index();

        let mut addition = stack.addition()?;
        assert_eq!(addition.next_update_index(), top_max + 1);
        let added = addition.add_table(|writer| {
            let idx = writer.limits().0;
            writer.add_ref(ref_record("refs/heads/fork-added", idx, main))?;
            writer.add_ref(RefRecord {
                refname: "refs/heads/team01/sub1/feature-186".into(),
                update_index: idx,
                value: RefValue::Deletion,
            })?;
            writer.add_log(LogRecord {
                refname: "refs/heads/fork-added".into(),
                update_index: idx,
                value: LogValue::Update {
                    old_hash: ObjectId::null(kind(object_format)),
                    new_hash: main,
                    name: "Fork Writer".into(),
                    email: "fork@example.com".into(),
                    time: 1_700_000_000,
                    tz_offset: 0,
                    message: "branch: created by the fork".into(),
                },
            })
        })?;
        assert!(added, "the table has records");
        assert!(addition.commit()?, "something was published");

        let newest = stack.snapshot().tables().last().expect("the new table is the newest");
        assert_eq!(
            (newest.min_update_index(), newest.max_update_index()),
            (top_max + 1, top_max + 1),
            "{object_format}: the new table continues where the stack ended"
        );
        assert_eq!(
            ref_lines(stack.snapshot().merged())?,
            git_ref_lines(&repo)?,
            "{object_format}: Git sees the new reference and the tombstone the same way this crate does"
        );
        assert!(
            git_ref_lines(&repo)?
                .iter()
                .any(|line| line.starts_with(b"refs/heads/fork-added\t")),
            "the added reference is there"
        );
        assert!(
            !git_ref_lines(&repo)?
                .iter()
                .any(|line| line.starts_with(b"refs/heads/team01/sub1/feature-186\t")),
            "the tombstone hides the reference of an older table"
        );
        let reflog = git(&repo, &["reflog", "show", "--format=%gs", "refs/heads/fork-added"])?;
        assert_eq!(reflog.trim().as_bstr(), "branch: created by the fork");
        assert_healthy(&repo, &format!("{object_format} after the fork's addition"));

        // Git continues with the stack, merging tables that include ours, and the result still is what we read.
        git(
            &repo,
            &["update-ref", "refs/heads/written-by-git-later", "refs/heads/main"],
        )?;
        assert!(stack.reload()?, "Git published a table, and maybe compacted");
        assert_eq!(ref_lines(stack.snapshot().merged())?, git_ref_lines(&repo)?);
        git(&repo, &["pack-refs", "--all"])?;
        assert!(stack.reload()?, "compaction replaced the tables");
        assert_eq!(stack.snapshot().tables().len(), 1);
        assert_eq!(ref_lines(stack.snapshot().merged())?, git_ref_lines(&repo)?);
        assert_healthy(&repo, &format!("{object_format} after Git compacted the stack"));
    }
    Ok(())
}

#[test]
fn an_addition_sees_what_was_published_since_its_stack_was_read_and_leaves_the_decision_to_the_caller() -> Result {
    let Some(repo) = util::writable_git_written_tables("sha1")? else {
        return Ok(());
    };
    let repo = repo.path().join("stacked");
    let mut stack = Stack::open(reftable_dir(&repo), Kind::Sha1)?;
    let before = stack.snapshot().clone();
    git(&repo, &["update-ref", "refs/heads/raced", "refs/heads/main"])?;

    let addition = stack.addition()?;
    let lookup = |snapshot: &gix_reftable::stack::Snapshot| -> Result<bool> {
        let mut iter = snapshot.merged().ref_iter()?;
        iter.seek_key(b"refs/heads/raced")?;
        Ok(matches!(iter.next_record()?, Some(Record::Ref(record)) if record.refname == "refs/heads/raced"))
    };
    assert!(
        lookup(addition.snapshot())?,
        "the view under the lock is current, not what was read before"
    );
    assert!(
        !lookup(&before)?,
        "the older view lacks it, so a caller comparing against it finds the conflict"
    );
    assert_eq!(
        addition.next_update_index(),
        addition.snapshot().tables().last().expect("tables").max_update_index() + 1,
        "the next index follows the current stack"
    );
    Ok(())
}

#[test]
fn an_addition_that_adds_nothing_or_fails_leaves_the_directory_as_it_was() -> Result {
    let Some(repo) = util::writable_git_written_tables("sha256")? else {
        return Ok(());
    };
    let repo = repo.path().join("stacked");
    let dir = reftable_dir(&repo);
    let mut stack = Stack::open(&dir, Kind::Sha256)?;
    let before = files(&dir)?;
    let list_before = std::fs::read(dir.join("tables.list"))?;

    let mut addition = stack.addition()?;
    assert!(!addition.add_table(|_| Ok(()))?, "tables without records aren't added");
    assert!(
        !addition.commit()?,
        "nothing is published, and the list isn't rewritten"
    );

    let mut addition = stack.addition()?;
    let err = addition
        .add_table(|writer| {
            writer.add_ref(ref_record(
                "refs/heads/x",
                writer.limits().0,
                ObjectId::null(Kind::Sha256),
            ))?;
            Err(gix_error::message("the caller gives up").raise())
        })
        .expect_err("the callback failed");
    assert!(err.to_string().contains("gives up"), "{err:?}");
    let err = addition
        .add_table(|writer| {
            writer.set_limits(1, 1)?;
            writer.add_ref(ref_record("refs/heads/x", 1, ObjectId::null(Kind::Sha256)))
        })
        .expect_err("a table can't start below the next update index");
    assert!(err.is_validation(), "{err:?}");
    drop(addition);

    let mut kept = stack.addition()?;
    kept.add_table(|writer| {
        writer.add_ref(ref_record(
            "refs/heads/y",
            writer.limits().0,
            ObjectId::null(Kind::Sha256),
        ))
    })?;
    drop(kept);

    assert_eq!(
        files(&dir)?,
        before,
        "no tables, temporary files or locks are left behind"
    );
    assert_eq!(std::fs::read(dir.join("tables.list"))?, list_before);
    Ok(())
}

#[test]
fn the_lock_is_exclusive_and_waiting_for_it_is_bounded() -> Result {
    let dir = gix_testtools::tempfile::tempdir()?;
    let mut holder = Stack::open(dir.path(), Kind::Sha1)?;
    let mut waiter = Stack::open(dir.path(), Kind::Sha1)?;

    let held = holder.addition()?;
    let start = Instant::now();
    let err = waiter.addition().expect_err("the lock is taken");
    let waited = start.elapsed();
    assert!(
        err.is_retryable(),
        "contention is told apart from other failures: {err:?}"
    );
    assert!(
        waited >= Duration::from_millis(100) && waited < Duration::from_secs(5),
        "the default of 100ms is waited out: {waited:?}"
    );

    drop(held);
    waiter.addition().expect("the lock was released by the drop");
    Ok(())
}

#[cfg(unix)]
#[test]
fn files_get_the_permissions_the_sharing_policy_asks_for() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| -> Result<u32> { Ok(std::fs::metadata(path)?.permissions().mode() & 0o777) };

    let dir = gix_testtools::tempfile::tempdir()?;
    let id = ObjectId::null(Kind::Sha1);
    let publish = |stack: &mut Stack| -> Result<Vec<String>> {
        let mut addition = stack.addition()?;
        addition.add_table(|writer| writer.add_ref(ref_record("refs/heads/x", writer.limits().0, id)))?;
        addition.commit()?;
        Ok(stack
            .snapshot()
            .tables()
            .iter()
            .map(|table| table.name().to_owned())
            .collect())
    };

    // Without a policy, files get what the umask allows for new files, like the files that Git creates.
    let probe = dir.path().join("probe");
    std::fs::File::create(&probe)?;
    let default_mode = mode(&probe)?;
    let mut stack = Stack::open(dir.path(), Kind::Sha1)?;
    let names = publish(&mut stack)?;
    assert_eq!(
        mode(&dir.path().join(&names[0]))?,
        default_mode,
        "the table follows the umask"
    );
    assert_eq!(mode(&dir.path().join("tables.list"))?, default_mode, "so does the list");

    let mut options = Options::default();
    options.shared_repository_permissions = 0o660;
    stack.set_options(options);
    let names = publish(&mut stack)?;
    for file in [dir.path().join(&names[1]), dir.path().join("tables.list")] {
        assert_eq!(
            mode(&file)? & 0o660,
            0o660,
            "{file:?}: the group may read and write, as for a shared repository"
        );
    }
    Ok(())
}

#[test]
fn an_unbounded_wait_for_the_lock_ends_when_it_is_released() -> Result {
    let dir = gix_testtools::tempfile::tempdir()?;
    let mut holder = Stack::open(dir.path(), Kind::Sha1)?;
    let mut options = Options::default();
    options.lock_timeout = None;
    let mut waiter = Stack::open_with_options(dir.path(), Kind::Sha1, options)?;

    let held = holder.addition()?;
    let start = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            drop(held);
        });
        waiter.addition().map(drop)
    })?;
    assert!(
        start.elapsed() >= Duration::from_millis(300),
        "it did wait for the release"
    );
    Ok(())
}
