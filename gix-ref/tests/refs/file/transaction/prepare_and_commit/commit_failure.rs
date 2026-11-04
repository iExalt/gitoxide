//! What a failing `commit()` tells about the state it leaves behind, with the state that Git leaves behind in the same
//! situation described in `docs/push-durability-evidence.md`. Failures are provoked with the file system: a directory
//! where a reflog file is expected, or a directory that was made read-only after the locks were taken.
use gix_lock::acquire::Fail;
use gix_ref::{
    file,
    transaction::{
        PreviousValue, RefEdit,
        commit_failure::{NothingApplied, Progress, Uncertain},
    },
};
use gix_testtools::TestResult;

use super::{committer, create_at, empty_store};
use crate::file::EmptyCommit;

fn name(full_name: &str) -> gix_ref::FullName {
    full_name.try_into().expect("valid name")
}

/// Return the progress that the error reports, after asserting that it carries exactly one marker, the right one.
fn uncertain_progress(err: &gix_error::Error) -> Vec<(String, Progress)> {
    assert!(
        err.downcast_any_ref::<NothingApplied>().is_none(),
        "a failure that may have changed something must not claim that it didn't: {err:?}"
    );
    err.downcast_any_ref::<Uncertain>()
        .expect("commit failures after the first change are marked as uncertain")
        .edits
        .iter()
        .map(|(name, progress)| (name.as_bstr().to_string(), *progress))
        .collect()
}

#[test]
fn a_missing_committer_fails_before_anything_is_written() -> TestResult {
    let (dir, store) = empty_store()?;
    let err = store
        .transaction()
        .prepare([create_at("refs/heads/a")], Fail::Immediately, Fail::Immediately)?
        .commit(None)
        .expect_err("a reflog entry needs a committer");
    assert!(
        err.downcast_any_ref::<Uncertain>().is_none(),
        "nothing happened, so it can't be uncertain"
    );
    assert!(
        err.downcast_any_ref::<NothingApplied>().is_some(),
        "the caller can tell that retrying is safe"
    );
    assert!(
        err.downcast_any_ref::<file::log::create_or_update::MissingCommitter>()
            .is_some(),
        "the specific cause remains identifiable"
    );
    assert!(
        store.try_find("refs/heads/a")?.is_none(),
        "the reference wasn't created"
    );
    assert!(
        !dir.path().join("logs").exists(),
        "no empty reflog file or directory was left behind, which is where the old code stopped"
    );
    Ok(())
}

#[test]
fn a_missing_committer_is_fine_if_no_reflog_entry_is_needed() -> TestResult {
    let (_dir, store) = empty_store()?;
    // Unlike branches, references elsewhere don't get a reflog unless asked for.
    let mut edit = create_at("refs/custom/a");
    if let gix_ref::transaction::Change::Update { log, .. } = &mut edit.change {
        log.force_create_reflog = false;
    }
    store
        .transaction()
        .prepare([edit], Fail::Immediately, Fail::Immediately)?
        .commit(None)?;
    assert!(store.try_find("refs/custom/a")?.is_some(), "the reference was created");
    Ok(())
}

/// Git, with a directory with content where the second reference's reflog goes, updates the first reference and not
/// the second, and fails. The remaining edit was never attempted.
#[test]
fn an_edit_failing_in_the_middle_reports_how_far_each_got() -> TestResult {
    let (dir, store) = empty_store()?;
    let blocked = dir.path().join("logs/refs/heads/b");
    std::fs::create_dir_all(&blocked)?;
    std::fs::write(
        blocked.join("content"),
        "keeps the directory from being removed as empty",
    )?;

    let err = store
        .transaction()
        .prepare(
            [
                create_at("refs/heads/a"),
                create_at("refs/heads/b"),
                create_at("refs/heads/c"),
            ],
            Fail::Immediately,
            Fail::Immediately,
        )?
        .commit(committer().to_ref(&mut Default::default()))
        .expect_err("the reflog can't be written");
    assert_eq!(
        uncertain_progress(&err),
        [
            ("refs/heads/a".to_owned(), Progress::Applied),
            ("refs/heads/b".to_owned(), Progress::InDoubt),
            ("refs/heads/c".to_owned(), Progress::NotAttempted),
        ]
    );
    assert!(
        store.try_find("refs/heads/a")?.is_some(),
        "the first edit stays applied, as in Git"
    );
    assert!(store.try_find("refs/heads/b")?.is_none());
    assert!(
        store.try_find("refs/heads/c")?.is_none(),
        "nothing is done for edits after the failing one"
    );
    Ok(())
}

/// Git deletes a reference whose reflog it can't delete and carries on, which the fork does as well now.
#[test]
fn a_reflog_that_cannot_be_deleted_does_not_prevent_deleting_the_reference() -> TestResult {
    let (dir, store) = empty_store()?;
    store
        .transaction()
        .prepare([create_at("refs/heads/a")], Fail::Immediately, Fail::Immediately)?
        .commit(committer().to_ref(&mut Default::default()))?;
    let reflog = dir.path().join("logs/refs/heads/a");
    std::fs::remove_file(&reflog)?;
    std::fs::create_dir_all(&reflog)?;
    std::fs::write(reflog.join("content"), "makes this a directory that is not empty")?;

    store
        .transaction()
        .prepare(
            [RefEdit::delete(name("refs/heads/a"), PreviousValue::Any)],
            Fail::Immediately,
            Fail::Immediately,
        )?
        .commit(None)?;
    assert!(store.try_find("refs/heads/a")?.is_none(), "the reference is gone");
    assert!(
        reflog.is_dir(),
        "the reflog that could not be deleted remains, an orphan like Git leaves"
    );
    Ok(())
}

/// Git applies nothing if the reflog of the first reference can't be written. The fork can't tell whether writing the
/// reflog changed anything, so it reports the edit as in doubt, which is the conservative side of a documented difference.
#[test]
fn the_first_edit_failing_leaves_the_others_not_attempted() -> TestResult {
    let (dir, store) = empty_store()?;
    let blocked = dir.path().join("logs/refs/heads/a");
    std::fs::create_dir_all(&blocked)?;
    std::fs::write(blocked.join("content"), "makes this a directory that is not empty")?;

    let err = store
        .transaction()
        .prepare(
            [create_at("refs/heads/a"), create_at("refs/heads/b")],
            Fail::Immediately,
            Fail::Immediately,
        )?
        .commit(committer().to_ref(&mut Default::default()))
        .expect_err("the reflog can't be written");
    assert_eq!(
        uncertain_progress(&err),
        [
            ("refs/heads/a".to_owned(), Progress::InDoubt),
            ("refs/heads/b".to_owned(), Progress::NotAttempted),
        ]
    );
    assert!(store.try_find("refs/heads/a")?.is_none() && store.try_find("refs/heads/b")?.is_none());
    Ok(())
}

/// Deleting nothing but the reflog fails if it can't be deleted, since that is all that was asked for.
#[test]
fn a_reflog_that_cannot_be_deleted_fails_if_only_the_reflog_is_to_be_deleted() -> TestResult {
    let (dir, store) = empty_store()?;
    store
        .transaction()
        .prepare([create_at("refs/heads/a")], Fail::Immediately, Fail::Immediately)?
        .commit(committer().to_ref(&mut Default::default()))?;
    let reflog = dir.path().join("logs/refs/heads/a");
    std::fs::remove_file(&reflog)?;
    std::fs::create_dir_all(&reflog)?;
    std::fs::write(reflog.join("content"), "makes this a directory that is not empty")?;

    let err = store
        .transaction()
        .prepare(
            [RefEdit {
                change: gix_ref::transaction::Change::Delete {
                    expected: PreviousValue::Any,
                    log: gix_ref::transaction::RefLog::Only,
                },
                name: name("refs/heads/a"),
                deref: false,
            }],
            Fail::Immediately,
            Fail::Immediately,
        )?
        .commit(None)
        .expect_err("the reflog is all there is to delete");
    assert_eq!(
        uncertain_progress(&err),
        [("refs/heads/a".to_owned(), Progress::InDoubt)]
    );
    assert!(
        store.try_find("refs/heads/a")?.is_some(),
        "the reference was not asked to be deleted"
    );
    Ok(())
}

#[cfg(unix)]
mod permissions {
    use std::{os::unix::fs::PermissionsExt, path::Path};

    use super::*;

    /// Make `dir` read-only for the lifetime of the guard, so that nothing can be created, renamed or removed in it.
    struct ReadOnly(std::path::PathBuf, std::fs::Permissions);

    impl ReadOnly {
        fn new(dir: &Path) -> std::io::Result<Self> {
            let previous = std::fs::metadata(dir)?.permissions();
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555))?;
            Ok(ReadOnly(dir.to_owned(), previous))
        }
    }

    impl Drop for ReadOnly {
        fn drop(&mut self) {
            std::fs::set_permissions(&self.0, self.1.clone()).ok();
        }
    }

    /// Permissions don't restrict the superuser, which makes it impossible to provoke the failures here.
    fn is_superuser() -> bool {
        // SAFETY: `geteuid()` has no preconditions and no side effects.
        #[allow(unsafe_code)]
        unsafe {
            libc::geteuid() == 0
        }
    }

    /// The content of a `packed-refs` file with a single reference, whose value is the same in each hash kind.
    fn packed_refs() -> String {
        format!(
            "# pack-refs with: peeled fully-peeled sorted \n{} refs/heads/a\n",
            crate::hex_to_id("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")
        )
    }

    /// Git with a loose and a packed reference, and a directory made read-only after the locks were taken, removes the
    /// reference from `packed-refs` and its reflog, but not the loose file, so that the reference still resolves.
    #[test]
    fn deleting_a_loose_and_packed_reference_when_the_loose_one_cannot_be_removed() -> TestResult {
        if is_superuser() {
            eprintln!("SKIPPED: the superuser ignores permissions");
            return Ok(());
        }
        let (dir, store) = empty_store()?;
        store
            .transaction()
            .prepare([create_at("refs/heads/a")], Fail::Immediately, Fail::Immediately)?
            .commit(committer().to_ref(&mut Default::default()))?;
        std::fs::write(dir.path().join("packed-refs"), packed_refs())?;

        let transaction = store.transaction().prepare(
            [RefEdit::delete(name("refs/heads/a"), PreviousValue::Any)],
            Fail::Immediately,
            Fail::Immediately,
        )?;
        let err = {
            let _read_only = ReadOnly::new(&dir.path().join("refs/heads"))?;
            transaction
                .commit(None)
                .expect_err("the loose reference can't be removed")
        };
        assert_eq!(
            uncertain_progress(&err),
            [("refs/heads/a".to_owned(), Progress::InDoubt)],
            "the packed entry and the reflog are gone, but the value is still visible, so it's not known to be deleted"
        );
        assert_eq!(
            store.find("refs/heads/a")?.target.id().to_owned(),
            crate::hex_to_id("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
            "the loose reference still resolves"
        );
        assert!(
            !dir.path().join("logs/refs/heads/a").exists(),
            "the reflog was deleted before the reference, so that no reflog outlives its reference"
        );
        Ok(())
    }

    /// With the loose reference to be removed after the update, `packed-refs` carries the new value. If the git directory
    /// can't be written to anymore, `packed-refs.lock` can't replace `packed-refs`, but the reflog was appended already.
    #[test]
    fn updating_through_packed_refs_when_the_packed_refs_file_cannot_be_replaced() -> TestResult {
        if is_superuser() {
            eprintln!("SKIPPED: the superuser ignores permissions");
            return Ok(());
        }
        let (dir, store) = empty_store()?;
        // The reflog directory exists, so that only replacing `packed-refs` can fail.
        std::fs::create_dir_all(dir.path().join("logs/refs/heads"))?;
        let edit = create_at("refs/heads/a");
        let transaction = store
            .transaction()
            .packed_refs(
                file::transaction::PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(Box::new(
                    EmptyCommit,
                )),
            )
            .prepare([edit], Fail::Immediately, Fail::Immediately)?;
        let err = {
            let _read_only = ReadOnly::new(dir.path())?;
            transaction
                .commit(committer().to_ref(&mut Default::default()))
                .expect_err("packed-refs can't be replaced")
        };
        assert_eq!(
            uncertain_progress(&err),
            [("refs/heads/a".to_owned(), Progress::InDoubt)],
            "its reflog has an entry for a change that didn't happen, like in Git"
        );
        assert!(
            store.try_find("refs/heads/a")?.is_none(),
            "the reference was not created"
        );
        assert!(
            dir.path().join("logs/refs/heads/a").is_file(),
            "the reflog entry for the change that never happened remains"
        );
        Ok(())
    }

    /// Without `packed-refs` there is nothing but the unlink to say whether the reference is gone, and an unlink that
    /// fails may have worked nonetheless, so a blind retry isn't known to be safe.
    #[test]
    fn deleting_a_loose_reference_that_cannot_be_unlinked_is_in_doubt() -> TestResult {
        if is_superuser() {
            eprintln!("SKIPPED: the superuser ignores permissions");
            return Ok(());
        }
        let (dir, store) = empty_store()?;
        let mut create = create_at("refs/custom/a");
        if let gix_ref::transaction::Change::Update { log, .. } = &mut create.change {
            log.force_create_reflog = false;
        }
        store
            .transaction()
            .prepare([create], Fail::Immediately, Fail::Immediately)?
            .commit(None)?;

        let transaction = store.transaction().prepare(
            [RefEdit::delete(name("refs/custom/a"), PreviousValue::Any)],
            Fail::Immediately,
            Fail::Immediately,
        )?;
        let err = {
            let _read_only = ReadOnly::new(&dir.path().join("refs/custom"))?;
            transaction.commit(None).expect_err("the reference can't be unlinked")
        };
        assert_eq!(
            uncertain_progress(&err),
            [("refs/custom/a".to_owned(), Progress::InDoubt)],
            "neither a packed entry nor a reflog was there to tell anything"
        );
        assert!(store.try_find("refs/custom/a")?.is_some(), "the reference remains");
        Ok(())
    }
}
