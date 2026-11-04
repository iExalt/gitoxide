//! Tell what a failed [`commit()`](crate::file::Transaction::commit()) did to the repository.
//!
//! Git's ref transactions, and those of the files backend of this crate, are all-or-nothing only up to the point where
//! the first reference is changed. Failing afterwards, for example when a disk error interrupts the renames of a
//! number of references, can leave some changed and others not, and a reflog entry for a change that never happened.
//! Reporting such a failure like one that changed nothing would invite blind retries and wrong conclusions, so errors
//! of `commit()` carry one of these markers, to be found with [`Error::downcast_any_ref()`](gix_error::Error::downcast_any_ref()).
use crate::FullName;

/// How far an edit got when committing a transaction failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Progress {
    /// Nothing was done for the edit, which makes it safe to retry it.
    NotAttempted,
    /// Something was done for the edit, or was about to be done when the failure happened, so the current state of the
    /// reference has to be looked at to know whether the edit took effect, and its reflog may be inconsistent.
    InDoubt,
    /// The edit has been performed completely.
    Applied,
}

/// Committing failed before anything was changed, for example because the committer needed for a reflog entry is
/// missing. The repository is as it was before, and the failure is not specific to the state it was left in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NothingApplied;

impl std::fmt::Display for NothingApplied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The transaction failed before it changed anything")
    }
}

impl std::error::Error for NothingApplied {}

/// Committing failed after it started to change the repository, so the transaction may have been applied in part,
/// but not necessarily. It is never rolled back.
///
/// Backends that commit all-or-nothing, like a reftable stack, may not be able to tell more than that.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Uncertain {
    /// How far each edit got in the order in which the edits were prepared, which includes edits that were added when
    /// preparing, like those for the targets of symbolic references. It may be empty if a backend can't tell.
    pub edits: Vec<(FullName, Progress)>,
}

impl Uncertain {
    pub(crate) fn new(edits: Vec<(FullName, Progress)>) -> Self {
        Uncertain { edits }
    }
}

impl std::fmt::Display for Uncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The transaction failed while committing, and may have been applied in part")
    }
}

impl std::error::Error for Uncertain {}
