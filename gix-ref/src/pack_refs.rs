//! Moving loose references into `packed-refs` and pruning what became redundant, like `git pack-refs --all --prune`, and for
//! `reftable` merging tables, like `git pack-refs` does there.

/// Options to [pack references](crate::StoreRef::pack_refs()).
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Options {
    /// If `true`, do nothing unless there are enough loose references to make it worthwhile, or, for `reftable`, unless the
    /// tables should be merged according to their sizes, which is what `--auto` does.
    ///
    /// Default `false`.
    pub auto: bool,
    /// If `true`, delete the loose reference files whose value is in `packed-refs` now, as long as nobody changed them.
    /// Without this, references are only copied, which doesn't matter for `reftable`.
    ///
    /// Default `true`.
    pub prune: bool,
    /// How to wait for the lock of `packed-refs`. Loose references that are locked by somebody else are never waited for, but
    /// skipped. It's ignored for `reftable`, which has its own rules for waiting.
    pub lock: gix_lock::acquire::Fail,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            auto: false,
            prune: true,
            lock: gix_lock::acquire::Fail::Immediately,
        }
    }
}

/// What packing references did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Outcome {
    /// The number of loose references that were written to `packed-refs`, which includes those that were there already.
    pub packed: usize,
    /// The number of loose reference files that were deleted.
    pub pruned: usize,
    /// The number of loose references that stayed because somebody else held their lock or changed them meanwhile.
    ///
    /// They are still valid, and a later run will deal with them.
    pub skipped: usize,
    /// The number of tables that were merged into others, for `reftable`.
    pub merged_tables: usize,
}
