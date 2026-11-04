//! Tell an observer what happens to a transaction.
use crate::{
    Target,
    store_impl::file::transaction::Edit,
    transaction::{
        Change, LogChange, PreviousValue, RefEdit, RefLog,
        observe::{Event, Observer, Scope, State, Update, Veto, Vetoed},
    },
};

/// The updates that the edits stand for, which excludes those which only change reflogs.
///
/// `previous` has the value that the caller of each edit expected, as it's not preserved in the edits themselves.
pub(super) fn updates(edits: &[Edit], previous: &[Option<Target>]) -> Vec<Update> {
    updates_with_positions(edits, previous)
        .map(|(_, update)| update)
        .collect()
}

fn updates_with_positions<'a>(
    edits: &'a [Edit],
    previous: &'a [Option<Target>],
) -> impl Iterator<Item = (usize, Update)> + 'a {
    edits
        .iter()
        .zip(previous)
        .enumerate()
        .filter_map(|(position, (edit, previous))| {
            let (log_mode, new) = match &edit.update.change {
                Change::Delete { log, .. } => (*log, None),
                Change::Update {
                    log: LogChange { mode, .. },
                    new,
                    ..
                } => (*mode, Some(new.clone())),
            };
            (log_mode != RefLog::Only).then(|| {
                (
                    position,
                    Update {
                        name: edit.update.name.clone(),
                        previous: previous.clone(),
                        new,
                    },
                )
            })
        })
}

/// The deletions among `edits`, as the updates for the transaction on `packed-refs` that Git begins for them, with the
/// positions of the edits that make them. Edits that only change reflogs are left out.
pub(super) fn deletions(edits: &[Edit], previous: &[Option<Target>]) -> Vec<(usize, Update)> {
    updates_with_positions(edits, previous)
        .filter(|(_, update)| update.new.is_none())
        .map(|(position, update)| {
            (
                position,
                Update {
                    previous: None,
                    ..update
                },
            )
        })
        .collect()
}

/// The value that the caller of `edit` expected the reference to have, if any.
pub(super) fn expected(edit: &RefEdit) -> Option<Target> {
    match &edit.change {
        Change::Update { expected, .. } | Change::Delete { expected, .. } => match expected {
            PreviousValue::MustExistAndMatch(target) | PreviousValue::ExistingMustMatch(target) => Some(target.clone()),
            PreviousValue::Any | PreviousValue::MustExist | PreviousValue::MustNotExist => None,
        },
    }
}

#[derive(Default)]
enum Progress {
    /// Nothing was reported yet, or the transaction ended and there is nothing left to say.
    #[default]
    Idle,
    /// The transaction is being prepared, which is the time between `Preparing` and `Prepared`. Git opens a transaction on
    /// `packed-refs` when it locks the first deletion, and aborts it if anything fails afterwards, which is
    /// reported when the notifier is dropped.
    Locking {
        /// The deletions, with the position of the edits they are made by.
        deletions: Vec<(usize, Update)>,
        /// The position of the edit before which all deletions were locked.
        locked_before: usize,
    },
    /// The transaction was prepared, and neither committed nor aborted.
    Prepared {
        updates: Vec<Update>,
        /// The updates of the transaction on `packed-refs`, which is prepared and thus can be aborted or committed.
        packed: Option<Vec<Update>>,
    },
}

/// Tells the observer of a transaction what happens to it. If the transaction is prepared and gets dropped without being
/// committed, it is reported as aborted.
#[derive(Default)]
pub(in crate::store_impl::file) struct Notifier<'p> {
    observer: Option<Box<dyn Observer + 'p>>,
    progress: Progress,
    /// Updates of edits that were rejected, with their positions among all updates, which are part of what is reported.
    rejected: Vec<(usize, Update)>,
    /// Set once the observer was told that the transaction is being prepared, which happens once even if edits are
    /// rejected and prepared again.
    announced: bool,
}

impl<'p> Notifier<'p> {
    pub(in crate::store_impl::file) fn set(&mut self, observer: impl Observer + 'p) {
        self.observer = Some(Box::new(observer));
    }

    pub(super) fn set_rejected(&mut self, rejected: Vec<(usize, Update)>) {
        self.rejected = rejected;
    }

    pub(super) fn is_observing(&self) -> bool {
        self.observer.is_some()
    }

    fn notify(&mut self, scope: Scope, state: State, updates: &[Update]) -> Result<(), Vetoed> {
        let Some(observer) = self.observer.as_mut() else {
            return Ok(());
        };
        match observer.observe(&Event { state, scope, updates }) {
            Err(Veto) if matches!(state, State::Preparing | State::Prepared) => Err(Vetoed { state, scope }),
            Ok(()) | Err(Veto) => Ok(()),
        }
    }

    /// Report that nothing is locked yet. On a veto, the transaction is aborted.
    pub(super) fn preparing(&mut self, updates: &[Update]) -> Result<(), Vetoed> {
        if std::mem::replace(&mut self.announced, true) {
            return Ok(());
        }
        let updates = &crate::transaction::splice(updates, &self.rejected);
        self.notify(Scope::Transaction, State::Preparing, updates)
            .inspect_err(|_| {
                self.notify(Scope::Transaction, State::Aborted, updates).ok();
            })
    }

    /// Report that all references are locked. `packed` has the updates for the transaction on `packed-refs` if there
    /// is one, with `packed_is_needed` set if it has something to do, which is only then prepared and committed.
    ///
    /// On a veto, whatever was prepared is aborted.
    pub(super) fn prepared(
        &mut self,
        updates: Vec<Update>,
        packed: Option<Vec<Update>>,
        packed_is_needed: bool,
    ) -> Result<(), Vetoed> {
        if !self.is_observing() {
            return Ok(());
        }
        let updates = crate::transaction::splice(&updates, &self.rejected);
        self.progress = Progress::Idle;
        let packed = match packed {
            Some(packed_updates) if packed_is_needed => {
                for state in [State::Preparing, State::Prepared] {
                    if let Err(veto) = self.notify(Scope::PackedRefs, state, &packed_updates) {
                        // Git gives up on the spot, which leaves it without the outer state.
                        self.notify(Scope::PackedRefs, State::Aborted, &packed_updates).ok();
                        return Err(veto);
                    }
                }
                Some(packed_updates)
            }
            Some(packed_updates) => {
                // There is nothing to delete from `packed-refs`, and Git aborts its transaction.
                self.notify(Scope::PackedRefs, State::Aborted, &packed_updates).ok();
                None
            }
            None => None,
        };
        self.progress = Progress::Prepared { updates, packed };
        let Progress::Prepared { updates, .. } = &self.progress else {
            unreachable!("just set")
        };
        let updates = updates.clone();
        // On a veto, the transaction is aborted when it is dropped, after the locks are released.
        self.notify(Scope::Transaction, State::Prepared, &updates)
    }

    /// Start to track the `deletions` of the edits at the given positions, after nothing was locked yet.
    /// Failing to prepare is reported to the observer as the abort of the transaction on `packed-refs` that Git
    /// has begun by then, for those deletions that were locked as [reported](Self::locked_before()).
    pub(super) fn locking(&mut self, deletions: Vec<(usize, Update)>) {
        if self.is_observing() {
            self.progress = Progress::Locking {
                deletions,
                locked_before: 0,
            };
        }
    }

    /// Record that all edits before the one at `position` are locked, or `usize::MAX` for all of them.
    pub(super) fn locked_before(&mut self, position: usize) {
        if let Progress::Locking { locked_before, .. } = &mut self.progress {
            *locked_before = position;
        }
    }

    /// Report that the transaction on `packed-refs` was committed, which is in the middle of committing the transaction.
    pub(super) fn packed_committed(&mut self) {
        if let Progress::Prepared { packed, .. } = &mut self.progress
            && let Some(packed) = packed.take()
        {
            self.notify(Scope::PackedRefs, State::Committed, &packed).ok();
        }
    }

    /// Report that the transaction was committed.
    pub(super) fn committed(&mut self) {
        self.packed_committed();
        if let Progress::Prepared { updates, .. } = std::mem::take(&mut self.progress) {
            self.notify(Scope::Transaction, State::Committed, &updates).ok();
        }
    }

    /// Forget about the transaction without reporting anything more, as failing to commit does.
    pub(super) fn disarm(&mut self) {
        self.progress = Progress::Idle;
    }

    /// Report that the prepared transaction was abandoned.
    fn aborted(&mut self) {
        if let Progress::Prepared { updates, packed } = std::mem::take(&mut self.progress) {
            if let Some(packed) = packed {
                self.notify(Scope::PackedRefs, State::Aborted, &packed).ok();
            }
            self.notify(Scope::Transaction, State::Aborted, &updates).ok();
        }
    }
}

impl Drop for Notifier<'_> {
    fn drop(&mut self) {
        // Unless the transaction was prepared, only the transaction on `packed-refs` may have to be aborted.
        if matches!(self.progress, Progress::Locking { .. })
            && let Progress::Locking {
                deletions,
                locked_before,
            } = std::mem::take(&mut self.progress)
        {
            let begun: Vec<_> = deletions
                .into_iter()
                .filter(|(position, _)| *position < locked_before)
                .map(|(_, update)| update)
                .collect();
            if !begun.is_empty() {
                self.notify(Scope::PackedRefs, State::Aborted, &begun).ok();
            }
        }
        self.aborted();
    }
}
