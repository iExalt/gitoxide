use std::{path::PathBuf, sync::Arc};

use arc_swap::ArcSwap;

use crate::{Store, store::types::SlotMapIndex};

/// Options for use in [`Store::at_opts()`].
#[derive(Clone, Debug)]
pub struct Options {
    /// How to obtain a size for the slot map.
    pub slots: Slots,
    /// If false, no multi-pack indices will be used. If true, they will be used if their hash matches `object_hash`.
    pub use_multi_pack_index: bool,
    /// The maximum size of a single allocation caused by user-controlled on-disk pack data.
    ///
    /// If `None`, no additional limit is enforced.
    pub alloc_limit_bytes: Option<usize>,
    /// The current directory of the process at the time of instantiation.
    /// If unset, it will be retrieved using `gix_fs::current_dir(false)`.
    pub current_dir: Option<std::path::PathBuf>,
    /// The compression level to use when writing loose objects.
    ///
    /// Defaults to [`Compression::BEST_SPEED`](gix_zlib::Compression::BEST_SPEED), which is
    /// also what `git` uses unless configured otherwise with `core.looseCompression` or `core.compression`.
    pub loose_compression: gix_zlib::Compression,
    /// What to flush when writing loose objects, which is nothing by default like in Git, except for loose objects on Windows.
    pub fsync: gix_fs::fsync::Options,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            slots: Default::default(),
            use_multi_pack_index: true,
            alloc_limit_bytes: None,
            current_dir: None,
            loose_compression: gix_zlib::Compression::BEST_SPEED,
            fsync: Default::default(),
        }
    }
}

/// Configures how many pack indices the store can hold.
#[derive(Copy, Clone, Debug)]
pub enum Slots {
    /// The number of slots to use, that is the total number of indices we can hold at a time.
    /// Using this has the advantage of avoiding an initial directory listing of the repository, and is recommended
    /// on the server side where the repository setup is controlled.
    ///
    /// Note that this won't affect their packs, as each index can have one or more packs associated with it.
    Given(u16),
    /// Size the slot map from the indices on disk, and grow it when a refresh discovers more indices.
    AsNeededByDiskState {
        /// 1.0 means no safety, 1.1 means 10% more slots than needed
        multiplier: f32,
        /// The minimum number of slots to assume
        minimum: usize,
    },
}

impl Default for Slots {
    fn default() -> Self {
        Slots::AsNeededByDiskState {
            multiplier: 1.1,
            minimum: 32,
        }
    }
}

impl Store {
    /// Open the store at `objects_dir` (containing loose objects and `packs/`), which must only be a directory for
    /// the store to be created without any additional work being done.
    /// `slots` defines how many multi-pack-indices as well as indices we can know about at a time, which includes
    /// the allowance for all additional object databases coming in via `alternates` as well.
    /// Note that the `slots` isn't used for packs, these are included with their multi-index or index respectively.
    /// For example, In a repository with 250m objects and geometric packing one would expect 27 index/pack pairs,
    /// or a single multi-pack index.
    /// `object_hash` is the hash expected in packs and used for loose object iteration and object writing.
    /// `replacements` is an iterator over pairs of old and new object ids for replacement support.
    /// This means that when asking for object `X`, one will receive object `X-replaced` given an iterator like `Some((X, X-replaced))`.
    pub fn at_opts(
        objects_dir: PathBuf,
        object_hash: gix_hash::Kind,
        replacements: &mut dyn Iterator<Item = (gix_hash::ObjectId, gix_hash::ObjectId)>,
        Options {
            slots,
            use_multi_pack_index,
            alloc_limit_bytes,
            current_dir,
            loose_compression,
            fsync,
        }: Options,
    ) -> std::io::Result<Self> {
        let _span = gix_trace::detail!("gix_odb::Store::at()");
        let current_dir = current_dir.map_or_else(
            || {
                // It's only used for real-pathing alternate paths and there it just needs to be consistent (enough).
                gix_fs::current_dir(false)
            },
            Ok,
        )?;
        if !objects_dir.is_dir() {
            return Err(std::io::Error::other(format!(
                "\"{}\" wasn't a directory",
                objects_dir.display()
            )));
        }
        let slot_count = match slots {
            Slots::Given(n) => n as usize,
            Slots::AsNeededByDiskState { multiplier, minimum } => {
                let mut db_paths =
                    crate::alternate::resolve(objects_dir.clone(), &current_dir).map_err(super::error_to_io)?;
                db_paths.insert(0, objects_dir.clone());
                let num_slots =
                    Store::collect_indices_and_mtime_sorted_by_size(db_paths, None, None, alloc_limit_bytes)
                        .map_err(super::error_to_io)?
                        .len();

                let candidate = ((num_slots as f32 * multiplier) as usize).max(minimum);
                if candidate > crate::store::types::PackId::max_indices() {
                    // A chance for this to work without 10% extra allocation - this already
                    // is an insane amount of packs.
                    num_slots
                } else {
                    candidate
                }
            }
        };
        if slot_count > crate::store::types::PackId::max_indices() {
            return Err(std::io::Error::other(format!(
                "Cannot use more than 2^15-1 slots, got {slot_count}"
            )));
        }
        let mut replacements: Vec<_> = replacements.collect();
        replacements.sort_by_key(|a| a.0);

        Ok(Store {
            current_dir,
            write: Default::default(),
            replacements,
            path: objects_dir,
            slots,
            index: ArcSwap::from_pointee(SlotMapIndex {
                files: Arc::new((0..slot_count).map(|_| Arc::default()).collect()),
                ..Default::default()
            }),
            use_multi_pack_index,
            object_hash,
            alloc_limit_bytes,
            loose_compression,
            fsync,
            extra_alternates: Vec::new(),
            num_handles_stable: Default::default(),
            num_handles_unstable: Default::default(),
            num_disk_state_consolidation: Default::default(),
        })
    }

    /// Use the object directories in `alternates` in addition to those listed in `info/alternates` files, as if they were listed
    /// there, along with the alternates of these directories. They are searched after the object directory of this store, which
    /// remains the only one that is written to.
    ///
    /// This is how the object directory of a quarantine sees the objects of the repository it belongs to.
    /// Call it before the store is used, as objects that were looked up before aren't looked up again.
    pub fn with_alternates(mut self, alternates: impl IntoIterator<Item = PathBuf>) -> Self {
        self.extra_alternates = alternates.into_iter().collect();
        self
    }

    /// Return the object directories to read from, starting with `objects_dir`, followed by its alternates and the
    /// alternates of the directories passed to [`Self::with_alternates()`].
    pub(crate) fn object_directories(&self, objects_dir: &std::path::Path) -> gix_error::Result<Vec<PathBuf>> {
        let mut out = vec![objects_dir.to_owned()];
        out.extend(crate::alternate::resolve(objects_dir.to_owned(), &self.current_dir)?);
        for extra in &self.extra_alternates {
            for dir in
                std::iter::once(extra.clone()).chain(crate::alternate::resolve(extra.clone(), &self.current_dir)?)
            {
                if !out.contains(&dir) {
                    out.push(dir);
                }
            }
        }
        Ok(out)
    }
}
