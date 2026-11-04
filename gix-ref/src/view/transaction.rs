//! Backend-neutral transactions to edit references.
use gix_error::Result;
use gix_lock::acquire::Fail;

use crate::{
    file,
    transaction::{RefEdit, observe::Observer},
};

pub use crate::file::transaction::PackedRefs;

/// The errors that recoverable transaction failures are identified by, which every kind of storage raises for the
/// same failure so that callers can recover without knowing how references are stored.
///
/// [`ReferenceOutOfDate`] and [`MustNotExist`] identify a failed constraint of an edit, [`NameConflict`] a name that
/// would be the directory of another reference, and [`MissingCommitter`] a reflog entry that had to be written without
/// an identity. Find them by downcasting the error of
/// [`Transaction::prepare()`] or [`Prepared::commit()`].
///
/// [`Vetoed`] is what an [observer](Transaction::observer()) causes when it refuses a transaction while it is prepared.
/// A failed [`Prepared::commit()`] carries [`NothingApplied`] if it changed nothing, and [`Uncertain`] with the
/// progress of each edit if it may have changed something.
pub use crate::transaction::{Rejected, RejectionReason};
pub use crate::{
    file::{
        log::create_or_update::MissingCommitter,
        transaction::prepare::{MustNotExist, ReferenceOutOfDate},
    },
    transaction::{
        HeadLog, NameConflict,
        commit_failure::{NothingApplied, Uncertain},
        observe::Vetoed,
    },
};

/// Options for obtaining the locks a transaction needs, which are different for each kind of storage.
///
/// Create it with [`LockOptions::new()`] and adjust it with its methods, as more options are likely to be added.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct LockOptions {
    /// How to wait for the lock of each loose reference that is edited, for files.
    pub ref_files: Fail,
    /// How to wait for the `packed-refs` lock, for files.
    pub packed_refs: Fail,
}

impl LockOptions {
    /// Create options which fail as `ref_files` and `packed_refs` say.
    pub fn new(ref_files: Fail, packed_refs: Fail) -> Self {
        LockOptions { ref_files, packed_refs }
    }
}

/// A transaction that is about to be prepared with edits, created by [`StoreRef::transaction()`](super::StoreRef::transaction()).
#[must_use = "A transaction does nothing unless it is prepared and committed"]
pub struct Transaction<'s, 'p> {
    inner: Inner<'s, 'p>,
}

enum Inner<'s, 'p> {
    File(file::Transaction<'s, 'p>),
    #[cfg(feature = "reftable")]
    Reftable(crate::reftable::Transaction<'s, 'p>),
}

impl<'s, 'p> Transaction<'s, 'p> {
    pub(crate) fn new_file(transaction: file::Transaction<'s, 'p>) -> Self {
        Transaction {
            inner: Inner::File(transaction),
        }
    }

    #[cfg(feature = "reftable")]
    pub(crate) fn new_reftable(store: &'s crate::reftable::Store) -> Self {
        Transaction {
            inner: Inner::Reftable(crate::reftable::Transaction::new(store)),
        }
    }

    /// Configure the way `packed-refs` are handled during the transaction, which only has an effect on files.
    pub fn packed_refs(self, packed_refs: PackedRefs<'p>) -> Self {
        match self.inner {
            Inner::File(transaction) => Transaction::new_file(transaction.packed_refs(packed_refs)),
            #[cfg(feature = "reftable")]
            Inner::Reftable(transaction) => {
                // `packed-refs` don't exist in reftables, so there is nothing to configure.
                drop(packed_refs);
                Transaction {
                    inner: Inner::Reftable(transaction),
                }
            }
        }
    }

    /// Tell `observer` about the states of the transaction, and let it refuse to prepare it, like a
    /// `reference-transaction` hook does in Git. See [`observe`](crate::transaction::observe) for what is reported.
    ///
    /// Events with [`Scope::PackedRefs`](crate::transaction::observe::Scope::PackedRefs) only exist for files.
    pub fn observer(self, observer: impl Observer + 'p) -> Self {
        match self.inner {
            Inner::File(transaction) => Transaction::new_file(transaction.observer(observer)),
            #[cfg(feature = "reftable")]
            Inner::Reftable(transaction) => Transaction {
                inner: Inner::Reftable(transaction.observer(observer)),
            },
        }
    }

    /// Reject edits whose expectations aren't met, or whose names can't be used, one by one instead of failing the entire
    /// transaction, while the others go on, as `git update-ref --batch-updates` does.
    ///
    /// Edits are rejected if the reference isn't what they expect ([`RejectionReason::IncorrectOldValue`]), exists although it was to be
    /// created ([`RejectionReason::CreateExists`], which includes a lock that is held by somebody else), doesn't exist although it must
    /// ([`RejectionReason::NonexistentRef`]), or has a name that conflicts with another reference
    /// ([`RejectionReason::NameConflict`]). Anything else fails the transaction as a whole.
    ///
    /// [`Prepared::rejected()`] lists the rejected edits, which also appear in [`Outcome::rejected`]. They are not part of
    /// [`Outcome::edits`] and don't change the reflog of `HEAD`. Observers hear about them nonetheless, in the position they
    /// were requested in, as with Git's `reference-transaction` hook.
    ///
    /// An edit that fails only after all others were prepared, which happens if somebody else changes the reference in the meantime,
    /// fails the transaction.
    pub fn batch_updates(self) -> Self {
        match self.inner {
            Inner::File(transaction) => Transaction::new_file(transaction.batch_updates()),
            #[cfg(feature = "reftable")]
            Inner::Reftable(transaction) => Transaction {
                inner: Inner::Reftable(transaction.batch_updates()),
            },
        }
    }

    /// Decide how the reflog of `HEAD` follows the branch it points at, which is [what Git does](HeadLog::Git) by default.
    ///
    pub fn head_log(self, head_log: HeadLog) -> Self {
        match self.inner {
            Inner::File(transaction) => Transaction::new_file(transaction.head_log(head_log)),
            #[cfg(feature = "reftable")]
            Inner::Reftable(transaction) => Transaction {
                inner: Inner::Reftable(transaction.head_log(head_log)),
            },
        }
    }

    /// Lock everything the `edits` need and verify what they expect, so that [committing](Prepared::commit()) is
    /// likely to succeed. Nothing is changed if this fails.
    ///
    /// Failed edits identify the requested name in [metadata](gix_error::Error::metadata()) `reference` (bytes).
    ///
    /// Like in Git, writing or deleting the branch that `HEAD` points at is also logged in the reflog of `HEAD`, which
    /// isn't among the edits that [`Prepared::commit()`] returns. See [`HeadLog`] for how to deviate from that.
    pub fn prepare(self, edits: impl IntoIterator<Item = RefEdit>, lock: LockOptions) -> Result<Prepared<'s, 'p>> {
        match self.inner {
            Inner::File(transaction) => transaction
                .prepare(edits, lock.ref_files, lock.packed_refs)
                .map(Prepared::new_file),
            #[cfg(feature = "reftable")]
            Inner::Reftable(transaction) => {
                // Reftables don't have separate locks for references and `packed-refs`: stacks wait as configured with
                // `reftable.lockTimeout`.
                let _ = lock;
                transaction.prepare(edits).map(Prepared::new_reftable)
            }
        }
    }
}

/// A transaction whose edits were verified and whose locks are held, which is rolled back if it is dropped.
#[must_use = "Dropping a prepared transaction rolls it back"]
pub struct Prepared<'s, 'p> {
    inner: PreparedInner<'s, 'p>,
}

enum PreparedInner<'s, 'p> {
    File(file::Transaction<'s, 'p>),
    #[cfg(feature = "reftable")]
    Reftable(crate::reftable::Prepared<'s, 'p>),
}

impl<'s, 'p> Prepared<'s, 'p> {
    fn new_file(transaction: file::Transaction<'s, 'p>) -> Self {
        Prepared {
            inner: PreparedInner::File(transaction),
        }
    }

    #[cfg(feature = "reftable")]
    fn new_reftable(transaction: crate::reftable::Prepared<'s, 'p>) -> Self {
        Prepared {
            inner: PreparedInner::Reftable(transaction),
        }
    }

    /// The edits that were rejected while preparing, which only happens with [`Transaction::batch_updates()`].
    pub fn rejected(&self) -> &[Rejected] {
        match &self.inner {
            PreparedInner::File(transaction) => transaction.rejected(),
            #[cfg(feature = "reftable")]
            PreparedInner::Reftable(transaction) => transaction.rejected(),
        }
    }

    /// Make all prepared edits visible, using `committer` for reflogs, and return what was done.
    ///
    /// A failure doesn't imply that nothing was changed: how much of a transaction was applied depends on the storage.
    pub fn commit<'a>(self, committer: impl Into<Option<gix_actor::SignatureRef<'a>>>) -> Result<Outcome> {
        match self.inner {
            PreparedInner::File(mut transaction) => {
                let rejected = transaction.take_rejected();
                transaction.commit(committer).map(|edits| Outcome { edits, rejected })
            }
            #[cfg(feature = "reftable")]
            PreparedInner::Reftable(mut transaction) => {
                let rejected = transaction.take_rejected();
                transaction
                    .commit(committer.into())
                    .map(|edits| Outcome { edits, rejected })
            }
        }
    }

    /// Release all locks and return the edits without changing anything.
    pub fn rollback(self) -> Vec<RefEdit> {
        match self.inner {
            PreparedInner::File(transaction) => transaction.rollback(),
            #[cfg(feature = "reftable")]
            PreparedInner::Reftable(transaction) => transaction.rollback(),
        }
    }
}

/// What a committed transaction did.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Outcome {
    /// The edits that were applied, with the previous value of references filled in where it was known.
    pub edits: Vec<RefEdit>,
    /// The edits that were rejected, which is only ever the case with [`Transaction::batch_updates()`].
    pub rejected: Vec<Rejected>,
}
