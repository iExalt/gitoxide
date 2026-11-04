use gix_error::{ErrorExt, Message, Result, ResultExt, message};
use gix_hash::ObjectId;

use crate::{
    Target,
    store_impl::{
        file,
        file::{
            Transaction,
            transaction::{Edit, Notifier, PackedRefs},
        },
    },
    transaction::{
        Change, LogChange, PreviousValue, RefEdit, RefLog,
        commit_failure::{NothingApplied, Progress, Uncertain},
        is_noop_update,
    },
};

impl Edit {
    /// Return the previous and the new value of the reference as the reflog entry for this edit is to see them,
    /// or `None` if the edit doesn't ask for a reflog entry or its value doesn't change.
    fn reflog_entry(&self) -> Option<(Option<ObjectId>, &ObjectId)> {
        let Change::Update { new, expected, .. } = &self.update.change else {
            return None;
        };
        let (previous, new_oid) = match new {
            Target::Symbolic(_) => {
                // Special HACK: no reflog for symref changes as there is no OID involved which the reflog needs.
                // Unless, the ref is new and we can obtain a peeled id
                // identified by the expectation of what could be there, as is the case when cloning.
                match expected {
                    PreviousValue::ExistingMustMatch(Target::Object(oid)) => (Some(ObjectId::null(oid.kind())), oid),
                    _ => return None,
                }
            }
            Target::Object(new_oid) => {
                let previous = match expected {
                    // Here, this means that the ref already existed, and that it will receive (even transitively)
                    // the given value
                    PreviousValue::MustExistAndMatch(Target::Object(oid)) => Some(oid.to_owned()),
                    _ => None,
                }
                .or(self.leaf_referent_previous_oid);
                (previous, new_oid)
            }
        };
        let current = previous.map(Target::Object);
        (!is_noop_update(&self.update, current.as_ref())).then_some((previous, new_oid))
    }

    /// Return `true` if the value of this edit becomes visible through a rewritten `packed-refs` file, as opposed to
    /// the rename of a lock file, which is the case only when the loose source reference is to be removed.
    fn is_applied_through_packed_refs(&self, delete_loose_refs: bool) -> bool {
        delete_loose_refs
            && matches!(
                self.update.change,
                Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        ..
                    },
                    new: Target::Object(_),
                    ..
                }
            )
    }
}

impl Transaction<'_, '_> {
    /// Make all [prepared][Transaction::prepare()] permanent and return the performed edits which represent the current
    /// state of the affected refs in the ref store in that instant. Please note that the obtained edits may have been
    /// adjusted to contain more dependent edits or additional information.
    /// `committer` is used in the reflog and only if the reflog is actually written, which is why it is optional.
    /// If it is `None` and a reflog entry has to be written, the transaction fails before it changes anything.
    ///
    /// # Failure
    ///
    /// Errors identify what they did to the repository. Each carries exactly one of these, which can be found with
    /// [`Error::downcast_any_ref()`](gix_error::Error::downcast_any_ref()):
    ///
    /// * [`NothingApplied`] if the transaction failed *before* it changed anything. That includes a missing committer,
    ///   which is also identifiable as [`file::log::create_or_update::MissingCommitter`](crate::file::log::create_or_update::MissingCommitter).
    /// * [`Uncertain`] if it failed after it started changing the repository. The transaction may have been performed
    ///   in part, and no attempt to roll back partial changes is made. It lists how far each edit got. Never treat
    ///   such a failure as one that has changed nothing.
    ///
    /// Failures of [`prepare()`](Transaction::prepare()) don't change references and don't carry either.
    ///
    /// In this stage, we perform the following operations:
    ///
    /// * update the ref log
    /// * move updated refs into place
    /// * delete reflogs and empty parent directories, ignoring failures to do so, as Git does
    /// * delete packed refs
    /// * delete their corresponding reference (if applicable)
    ///   along with empty parent directories
    ///
    /// Note that transactions will be prepared automatically as needed.
    /// Per-reference failures include [metadata](gix_error::Error::metadata()) `reference` (bytes), the affected name.
    ///
    /// Nothing is flushed here. Values of loose references and `packed-refs` are flushed while preparing, if
    /// [configured](file::Store::fsync), and reflogs aren't flushed at all, like in Git.
    pub fn commit<'a>(self, committer: impl Into<Option<gix_actor::SignatureRef<'a>>>) -> Result<Vec<RefEdit>> {
        self.commit_inner(committer.into())
    }

    /// Per-reference failures include [metadata](gix_error::Error::metadata()) `reference` (bytes), the affected name.
    fn commit_inner(self, committer: Option<gix_actor::SignatureRef<'_>>) -> Result<Vec<RefEdit>> {
        let Transaction {
            store,
            packed_transaction,
            updates,
            packed_refs,
            mut observer,
            head_log: _,
            batch_updates: _,
            skip_transaction_conflicts: _,
            dry_pass: _,
            rejected: _,
        } = self;
        let mut updates = updates.expect("BUG: must call prepare before commit");
        let delete_loose_refs = matches!(
            packed_refs,
            PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(_)
        );

        if let Err(err) = check_before_writing(store, &updates, committer) {
            // Let go of the locks before the observer is told that nothing happened, as Git does.
            drop(updates);
            drop(packed_transaction);
            return Err(err.and_raise(NothingApplied));
        }

        // Whatever happens from here on, `progress` is the one place that knows what became of each edit.
        let mut progress = vec![Progress::NotAttempted; updates.len()];
        match write(
            store,
            packed_transaction,
            &mut updates,
            delete_loose_refs,
            committer,
            &mut progress,
            &mut observer,
        ) {
            Ok(()) => {
                observer.committed();
                // The log entries of `HEAD` that were added aren't what the caller asked for.
                Ok(updates
                    .into_iter()
                    .filter(|edit| edit.head_log_of.is_none())
                    .map(|edit| edit.update)
                    .collect())
            }
            Err(err) => {
                // Git doesn't tell anybody after failing to commit, and neither do we.
                observer.disarm();
                Err(err.and_raise(Uncertain::new(
                    updates
                        .iter()
                        .zip(progress)
                        .map(|(edit, progress)| (edit.update.name.clone(), progress))
                        .collect(),
                )))
            }
        }
    }
}

/// Fail without having written anything if a reflog entry is going to be needed without a committer to write it.
fn check_before_writing(
    store: &file::Store,
    updates: &[Edit],
    committer: Option<gix_actor::SignatureRef<'_>>,
) -> Result<()> {
    if committer.is_some() {
        return Ok(());
    }
    for change in updates {
        if let (Some(_), Change::Update { log, .. }) = (change.reflog_entry(), &change.update.change)
            && store.reflog_append_planned(change.update.name.as_ref(), log.force_create_reflog)?
        {
            return Err(crate::file::log::create_or_update::MissingCommitter
                .and_raise(Message::new("Could not update reflog").with("reference", change.update.name.as_bstr())));
        }
    }
    Ok(())
}

/// Change the repository according to `updates`, recording in `progress` how far each edit got.
///
/// `progress` stays [`NotAttempted`](Progress::NotAttempted) as long as nothing was done for an edit, becomes
/// [`InDoubt`](Progress::InDoubt) before anything is done that can't be told to have changed the repository if it
/// fails, and [`Applied`](Progress::Applied) when the edit is complete. Nothing is rolled back.
fn write(
    store: &file::Store,
    packed_transaction: Option<crate::packed::Transaction>,
    updates: &mut [Edit],
    delete_loose_refs: bool,
    committer: Option<gix_actor::SignatureRef<'_>>,
    progress: &mut [Progress],
    observer: &mut Notifier<'_>,
) -> Result<()> {
    // Perform updates first so live commits remain referenced
    for (idx, change) in updates.iter_mut().enumerate() {
        assert!(!change.update.deref, "Deref mode is turned into splits and turned off");
        let Change::Update { log, new, .. } = &change.update.change else {
            continue;
        };
        let update_ref = matches!(log.mode, RefLog::AndReference);
        let through_packed_refs = change.is_applied_through_packed_refs(delete_loose_refs);
        let lock = change.lock.take();
        // reflog first, then reference
        if let Some((previous, new_oid)) = change.reflog_entry() {
            progress[idx] = Progress::InDoubt;
            store
                .reflog_create_or_append(
                    change.update.name.as_ref(),
                    previous,
                    new_oid,
                    committer,
                    log.message.as_ref(),
                    log.force_create_reflog,
                )
                .or_raise(|| Message::new("Could not update reflog").with("reference", change.update.name.as_bstr()))?;
        }
        // Don't do anything else while keeping the lock after potentially updating the reflog.
        // We delay deletion of the reference and dropping the lock to after the packed-refs were
        // safely written.
        if delete_loose_refs && matches!(new, Target::Object(_)) {
            change.lock = lock;
            if !through_packed_refs {
                // Only the reflog is to be changed, which is done.
                progress[idx] = Progress::Applied;
            }
            continue;
        }
        if update_ref && let Some(lock) = lock {
            progress[idx] = Progress::InDoubt;
            if let Err(err) = lock.commit() {
                // TODO: when Kind::IsADirectory becomes stable, use that.
                let err = if err.instance.resource_path().is_dir() {
                    gix_fs::dir::remove::empty_depth_first(err.instance.resource_path())
                        .map_err(std::io::Error::other)
                        .and_then(|_| err.instance.commit().map_err(|err| err.error))
                        .err()
                } else {
                    Some(err.error)
                };

                if let Some(err) = err {
                    return Err(err.and_raise(
                        Message::new("Could not commit reference").with("reference", change.update.name.as_bstr()),
                    ));
                }
            }
        }
        progress[idx] = Progress::Applied;
    }

    for (idx, change) in updates.iter().enumerate() {
        let Change::Delete { log, .. } = &change.update.change else {
            continue;
        };
        // Reflog deletion happens first in case it fails a ref without log is less terrible than
        // a log without a reference.
        let (reflog_root, relative_name) = store.reflog_base_and_relative_path(change.update.name.as_ref())?;
        let reflog_path = reflog_root.join(relative_name);
        match std::fs::remove_file(&reflog_path) {
            Ok(()) => {
                // The log is gone while the reference remains, which is not what was asked for yet.
                progress[idx] = Progress::InDoubt;
                gix_fs::dir::remove::empty_upward_until_boundary(
                    reflog_path.parent().expect("never without parent"),
                    &reflog_root,
                )
                .ok();
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            // Git only warns about a reflog it cannot delete and deletes the reference anyway.
            // That is not an option if the reflog is all there is to delete.
            Err(err) if *log == RefLog::Only => {
                progress[idx] = Progress::InDoubt;
                return Err(err.and_raise(
                    Message::new("Could not delete reflog").with("reference", change.update.name.as_bstr()),
                ));
            }
            Err(_) => {}
        }
        if *log == RefLog::Only {
            progress[idx] = Progress::Applied;
        }
    }

    // Git keeps `packed-refs` locked until the loose references are deleted, so that nobody packs what is about to
    // be deleted, or has just been written to `packed-refs` and still exists as an older value in a loose file.
    let _packed_refs_lock;
    if let Some(t) = packed_transaction {
        // From here on, `packed-refs` may or may not have changed.
        for (idx, change) in updates.iter().enumerate() {
            if matches!(
                change.update.change,
                Change::Delete {
                    log: RefLog::AndReference,
                    ..
                }
            ) || change.is_applied_through_packed_refs(delete_loose_refs)
            {
                progress[idx] = Progress::InDoubt;
            }
        }
        _packed_refs_lock = t
            .commit_keeping_lock()
            .or_raise(|| message("Could not commit packed-ref transaction"))?;
        observer.packed_committed();
        // Always refresh ourselves right away to avoid races. We ignore errors as there may be many reasons this fails, and it's not
        // critical to be done here. In other words, the pack may be refreshed at a later time and then it might work.
        store.force_refresh_packed_buffer().ok();
    }

    for (idx, change) in updates.iter_mut().enumerate() {
        let take_lock_and_delete = match &change.update.change {
            Change::Update {
                log: LogChange { mode, .. },
                new,
                ..
            } => delete_loose_refs && *mode == RefLog::AndReference && matches!(new, Target::Object(_)),
            Change::Delete { log: mode, .. } => *mode == RefLog::AndReference,
        };
        if take_lock_and_delete {
            let lock = change.lock.take();
            let reference_path = store.reference_path(change.update.name.as_ref())?;
            // An unlink that fails, say with `EIO`, may still have taken effect.
            progress[idx] = Progress::InDoubt;
            if let Err(err) = std::fs::remove_file(reference_path)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                return Err(err.and_raise(
                    Message::new("Could not delete reference").with("reference", change.update.name.as_bstr()),
                ));
            }
            drop(lock);
            progress[idx] = Progress::Applied;
        }
    }
    Ok(())
}
