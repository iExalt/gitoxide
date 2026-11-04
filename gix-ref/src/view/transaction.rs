//! Backend-neutral transactions to edit references.
use gix_error::Result;
use gix_lock::acquire::Fail;

use crate::{file, transaction::RefEdit};

pub use crate::file::transaction::PackedRefs;

/// The errors that recoverable transaction failures are identified by, which every kind of storage raises for the
/// same failure so that callers can recover without knowing how references are stored.
///
/// [`ReferenceOutOfDate`] and [`MustNotExist`] identify a failed constraint of an edit, [`NameConflict`] a name that
/// would be the directory of another reference, and [`MissingCommitter`] a reflog entry that had to be written without
/// an identity. Find them by downcasting the error of
/// [`Transaction::prepare()`] or [`Prepared::commit()`].
pub use crate::{
    file::{
        log::create_or_update::MissingCommitter,
        transaction::prepare::{MustNotExist, ReferenceOutOfDate},
    },
    transaction::NameConflict,
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
}

impl<'s, 'p> Transaction<'s, 'p> {
    pub(crate) fn new_file(transaction: file::Transaction<'s, 'p>) -> Self {
        Transaction {
            inner: Inner::File(transaction),
        }
    }

    /// Configure the way `packed-refs` are handled during the transaction, which only has an effect on files.
    pub fn packed_refs(self, packed_refs: PackedRefs<'p>) -> Self {
        match self.inner {
            Inner::File(transaction) => Transaction::new_file(transaction.packed_refs(packed_refs)),
        }
    }

    /// Lock everything the `edits` need and verify what they expect, so that [committing](Prepared::commit()) is
    /// likely to succeed. Nothing is changed if this fails.
    ///
    /// Failed edits identify the requested name in [metadata](gix_error::Error::metadata()) `reference` (bytes).
    pub fn prepare(self, edits: impl IntoIterator<Item = RefEdit>, lock: LockOptions) -> Result<Prepared<'s, 'p>> {
        match self.inner {
            Inner::File(transaction) => transaction
                .prepare(edits, lock.ref_files, lock.packed_refs)
                .map(Prepared::new_file),
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
}

impl<'s, 'p> Prepared<'s, 'p> {
    fn new_file(transaction: file::Transaction<'s, 'p>) -> Self {
        Prepared {
            inner: PreparedInner::File(transaction),
        }
    }

    /// Make all prepared edits visible, using `committer` for reflogs, and return what was done.
    ///
    /// A failure doesn't imply that nothing was changed: how much of a transaction was applied depends on the storage.
    pub fn commit<'a>(self, committer: impl Into<Option<gix_actor::SignatureRef<'a>>>) -> Result<Outcome> {
        match self.inner {
            PreparedInner::File(transaction) => transaction.commit(committer).map(|edits| Outcome { edits }),
        }
    }

    /// Release all locks and return the edits without changing anything.
    pub fn rollback(self) -> Vec<RefEdit> {
        match self.inner {
            PreparedInner::File(transaction) => transaction.rollback(),
        }
    }
}

/// What a committed transaction did.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Outcome {
    /// The edits that were applied, with the previous value of references filled in where it was known.
    pub edits: Vec<RefEdit>,
}
