use gix_hash::ObjectId;
use gix_object::bstr::BString;

/// A parsed ref log line that can be changed
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Line {
    /// The previous object id. Can be a null-sha to indicate this is a line for a new ref.
    pub previous_oid: ObjectId,
    /// The new object id. Can be a null-sha to indicate this ref is being deleted.
    pub new_oid: ObjectId,
    /// The signature of the currently configured committer.
    pub signature: gix_actor::Signature,
    /// The message providing details about the operation performed in this log line.
    pub message: BString,
}

/// Removing lines from a reflog.
pub mod expire {
    /// Options to [expire the lines of a reflog](crate::StoreRef::reflog_expire()).
    #[derive(Debug, Clone, Copy, Default)]
    #[non_exhaustive]
    pub struct Options {
        /// If `true`, decide which lines go and report that, but change nothing. Locks are not taken either, so the
        /// outcome may be outdated by the time somebody else changes the reference.
        pub dry_run: bool,
        /// How to wait if the lock of the reference or its reflog is held by somebody else. It's ignored for storage that has
        /// its own rules for waiting, like `reftable`.
        pub lock: gix_lock::acquire::Fail,
    }

    /// What expiring the lines of a reflog did, or, with [`dry_run`](Options::dry_run), would do.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Outcome {
        /// The number of lines that were removed.
        pub removed: usize,
        /// The number of lines that remain.
        pub kept: usize,
    }
}
