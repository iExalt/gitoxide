//! Git's `pack-refs` assumes that whoever deletes or rewrites a loose reference keeps `packed-refs` locked until the loose
//! file is gone. Otherwise it packs the value that is about to be replaced or deleted, and the loose file that is
//! removed afterwards takes the newer state with it.
//!
//! Each test runs `git pack-refs --all --prune` in the middle of a transaction, at the state that is reported while the loose
//! reference still exists, and expects it to be turned away by the lock instead of changing the outcome. The Git
//! command doesn't wait for the lock.
#![cfg(unix)]
use std::{cell::Cell, path::Path, rc::Rc};

use gix_lock::acquire::Fail;
use gix_ref::{
    file::transaction::PackedRefs,
    transaction::{
        Change, LogChange, PreviousValue, RefEdit, RefLog,
        observe::{Event, Scope, State, Veto},
    },
};
use gix_testtools::TestResult;

use crate::file::transaction::prepare_and_commit::committer;

const NAME: &str = "refs/heads/topic";

/// A bare repository with two commits, `old` and `new`, and `NAME` pointing to `old` as a loose reference.
fn repo() -> TestResult<(
    gix_testtools::tempfile::TempDir,
    gix_ref::file::Store,
    gix_hash::ObjectId,
    gix_hash::ObjectId,
)> {
    let dir = gix_testtools::tempfile::TempDir::new()?;
    let kind = crate::fixture_hash_kind();
    gix_testtools::git(dir.path(), &format!("init -q --bare --object-format={kind}"))?;
    let tree = gix_testtools::git(dir.path(), "write-tree")?;
    let commit = |message: &str| -> TestResult<gix_hash::ObjectId> {
        let id = gix_testtools::git(dir.path(), &format!("commit-tree -m {message} {}", tree.trim()))?;
        Ok(gix_hash::ObjectId::from_hex(id.trim().as_bytes())?)
    };
    let (old, new) = (commit("old")?, commit("new")?);
    gix_testtools::git(dir.path(), &format!("update-ref {NAME} {old}"))?;
    let store = gix_ref::file::Store::at(dir.path().to_owned(), kind);
    Ok((dir, store, old, new))
}

/// Run `git pack-refs`, return `true` if it succeeded and thus wasn't stopped by the lock.
fn pack_refs(git_dir: &Path) -> TestResult<bool> {
    Ok(gix_testtools::run_git(
        git_dir,
        &["-c", "core.packedRefsTimeout=0", "pack-refs", "--all", "--prune"],
    )?
    .success())
}

fn resolved(store: &gix_ref::file::Store) -> TestResult<Option<gix_hash::ObjectId>> {
    Ok(store
        .try_find(NAME)?
        .and_then(|reference| reference.target.try_id().map(ToOwned::to_owned)))
}

#[test]
fn an_update_that_moves_a_reference_into_packed_refs_is_not_undone_by_packing_the_old_value() -> TestResult {
    let (dir, store, old, new) = repo()?;
    let git_dir = dir.path().to_owned();
    let pack_refs_ran = Rc::new(Cell::new(None));
    let seen = pack_refs_ran.clone();
    store
        .transaction()
        .packed_refs(PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(
            Box::new(crate::file::EmptyCommit),
        ))
        .observer(move |event: &Event<'_>| -> Result<(), Veto> {
            if (event.scope, event.state) == (Scope::PackedRefs, State::Committed) {
                seen.set(Some(pack_refs(&git_dir).expect("git can be run")));
            }
            Ok(())
        })
        .prepare(
            [RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: "update".into(),
                    },
                    expected: PreviousValue::MustExistAndMatch(gix_ref::Target::Object(old)),
                    new: gix_ref::Target::Object(new),
                },
                name: NAME.try_into()?,
                deref: false,
            }],
            Fail::Immediately,
            Fail::Immediately,
        )?
        .commit(committer().to_ref(&mut Default::default()))?;

    assert_eq!(
        resolved(&store)?,
        Some(new),
        "the new value is what remains once the loose reference is gone, and isn't replaced by the old one that was packed"
    );
    assert_eq!(
        pack_refs_ran.get(),
        Some(false),
        "Git can't pack while the new value is in packed-refs and the old one still is in the loose file"
    );
    Ok(())
}

#[test]
fn a_deleted_loose_reference_is_not_packed_again_before_it_is_gone() -> TestResult {
    let (dir, store, _old, _new) = repo()?;
    assert!(
        !dir.path().join("packed-refs").exists(),
        "the reference is only loose, so deleting it has nothing to do in packed-refs, which doesn't even exist"
    );
    let git_dir = dir.path().to_owned();
    let pack_refs_ran = Rc::new(Cell::new(None));
    let seen = pack_refs_ran.clone();
    store
        .transaction()
        .observer(move |event: &Event<'_>| -> Result<(), Veto> {
            if (event.scope, event.state) == (Scope::Transaction, State::Prepared) {
                seen.set(Some(pack_refs(&git_dir).expect("git can be run")));
            }
            Ok(())
        })
        .prepare(
            [RefEdit::delete(NAME.try_into()?, PreviousValue::Any)],
            Fail::Immediately,
            Fail::Immediately,
        )?
        .commit(None)?;

    assert_eq!(
        resolved(&store)?,
        None,
        "the reference stays deleted instead of coming back from packed-refs"
    );
    assert_eq!(
        pack_refs_ran.get(),
        Some(false),
        "packing is locked out while the reference is being deleted"
    );
    assert!(
        !dir.path().join("packed-refs").exists()
            || !std::fs::read_to_string(dir.path().join("packed-refs"))?.contains(NAME),
        "packed-refs doesn't have it either"
    );
    Ok(())
}
