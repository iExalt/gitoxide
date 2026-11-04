//! A stack of tables, which together hold the references and reflogs of a repository, as `tables.list` names them.
//!
//! The list is ordered from the oldest table to the newest, and the newest records win. Tables are immutable, and a
//! writer publishes its changes by replacing the list, so readers see either the stack before the change or the
//! one after it, but never something in between.
//!
//! ## Reading
//!
//! [`Stack::open()`] reads the list and the tables it names, and [`Stack::reload()`] picks up what other writers published since.
//! Tables that were read already are reused if the list still names them. A [`Snapshot`] is the stack as it was read, which
//! stays valid and cheap to share, no matter what is published afterwards.
//!
//! Where Git reads a stack whose list names a missing or unreadable table as if it had no references, which can make a
//! repository look empty, this crate returns an error. The only exception is a table that disappears because a writer
//! replaced the list while it was read, in which case the new list is read, like Git does. The same goes for a list that keeps
//! changing while it is read: Git gives up after a few seconds by keeping its previous view (or none at all when opening),
//! while this crate reports the missing table.

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use gix_error::{ErrorExt, Result};
use gix_hash::Kind;

use crate::{BlockSource, MergedTable, Table, basics::corrupt};

mod addition;
mod files;
mod list;
mod reload;

pub use addition::{Addition, UncertainAfterPublish};

/// The name of the file that lists the tables of a stack.
pub const LIST_FILE_NAME: &str = "tables.list";

/// Configure how a stack is changed.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Options {
    /// How long to wait for the lock of `tables.list` while somebody else holds it, which is `reftable.lockTimeout`
    /// and 100 milliseconds by default. `None` waits for as long as it takes.
    pub lock_timeout: Option<Duration>,
    /// Git's sharing policy, usually derived from `core.sharedRepository`, which is applied to the files that are created
    /// after the process umask. See [`gix_fs::set_shared_repository_permissions()`] for the encoding.
    pub shared_repository_permissions: i32,
    /// What to flush to stable storage, according to `core.fsync` and `core.fsyncMethod`. Tables and the list are
    /// flushed as references are, before they are made visible by renaming them.
    pub fsync: gix_fs::fsync::Options,
    /// The layout of the tables that are written.
    pub write: crate::writer::Options,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            lock_timeout: Some(Duration::from_millis(100)),
            shared_repository_permissions: 0,
            fsync: gix_fs::fsync::Options::default(),
            write: crate::writer::Options::default(),
        }
    }
}

/// A stack of tables in a directory, kept up to date with [`reload()`](Stack::reload()).
#[derive(Debug)]
pub struct Stack {
    dir: PathBuf,
    hash_kind: Kind,
    options: Options,
    snapshot: Snapshot,
    #[cfg(test)]
    inject: Option<files::Inject>,
}

/// The tables of a stack as they were when the stack was read.
///
/// A snapshot is cheap to clone and to share between threads, as the data of its tables is shared.
#[derive(Debug, Clone)]
pub struct Snapshot(Arc<SnapshotInner>);

#[derive(Debug)]
struct SnapshotInner {
    /// The content of `tables.list` the tables were read for.
    list: Vec<u8>,
    merged: MergedTable,
}

impl Snapshot {
    fn new(list: Vec<u8>, tables: Vec<Table>) -> Result<Self> {
        Ok(Self(Arc::new(SnapshotInner {
            list,
            merged: MergedTable::new(tables)?.with_deletions_suppressed(true),
        })))
    }

    /// The tables from the oldest to the newest.
    pub fn tables(&self) -> &[Table] {
        self.0.merged.tables()
    }

    /// All tables as a single table, in which a reference that was deleted doesn't show up.
    pub fn merged(&self) -> &MergedTable {
        &self.0.merged
    }

    /// All tables as a single table that reports deletions as tombstone records, as compaction needs them.
    pub fn merged_with_tombstones(&self) -> MergedTable {
        self.0.merged.clone().with_deletions_suppressed(false)
    }

    /// The content of `tables.list` that this snapshot was read for.
    pub fn list(&self) -> &[u8] {
        &self.0.list
    }

    /// The update index a table that is added on top of this snapshot starts at, which is one more than that of
    /// the newest table, or 1 for an empty stack.
    pub fn next_update_index(&self) -> u64 {
        self.tables().last().map_or(1, |table| table.max_update_index() + 1)
    }
}

impl Stack {
    /// Read the stack that is stored in `dir`, which holds `tables.list` and the tables it names, for a repository
    /// whose object hash is `hash_kind`.
    ///
    /// A directory without `tables.list`, or none at all, is an empty stack. A table of another hash, or one that is
    /// listed but doesn't exist, is an error.
    pub fn open(dir: impl Into<PathBuf>, hash_kind: Kind) -> Result<Self> {
        Self::open_with_options(dir, hash_kind, Options::default())
    }

    /// Like [`open()`](Self::open()), but with `options` that apply when the stack is changed.
    pub fn open_with_options(dir: impl Into<PathBuf>, hash_kind: Kind, options: Options) -> Result<Self> {
        let dir = dir.into();
        let (list, tables) = reload::reload(&mut Disk::new(&dir, hash_kind), &[])?;
        Ok(Self {
            dir,
            hash_kind,
            options,
            snapshot: Snapshot::new(list, tables)?,
            #[cfg(test)]
            inject: None,
        })
    }

    /// Read the list and the tables again and return `true` if the stack changed since it was read last.
    ///
    /// Snapshots taken before aren't affected.
    pub fn reload(&mut self) -> Result<bool> {
        let (list, tables) = reload::reload(&mut Disk::new(&self.dir, self.hash_kind), self.snapshot.tables())?;
        let changed = list != self.snapshot.list();
        if changed {
            self.snapshot = Snapshot::new(list, tables)?;
        }
        Ok(changed)
    }

    /// The stack as it was read last.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Change how the stack is changed from now on.
    pub fn set_options(&mut self, options: Options) {
        self.options = options;
    }

    /// The directory of the stack.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The hash of the repository the stack belongs to.
    pub fn hash_kind(&self) -> Kind {
        self.hash_kind
    }
}

/// The real file system.
struct Disk<'a> {
    dir: &'a Path,
    hash_kind: Kind,
    started: Instant,
}

impl<'a> Disk<'a> {
    fn new(dir: &'a Path, hash_kind: Kind) -> Self {
        Self {
            dir,
            hash_kind,
            started: Instant::now(),
        }
    }
}

/// Read `path` and return `None` if it doesn't exist.
fn read_if_exists(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(data) => Ok(Some(data)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.and_raise(gix_error::message!("Could not read {}", path.display()))),
    }
}

impl reload::Backend for Disk<'_> {
    fn read_list(&mut self) -> Result<Vec<u8>> {
        Ok(read_if_exists(&self.dir.join(LIST_FILE_NAME))?.unwrap_or_default())
    }

    fn open_table(&mut self, name: &str) -> Result<Option<Table>> {
        let Some(data) = read_if_exists(&self.dir.join(name))? else {
            return Ok(None);
        };
        let table = Table::from_block_source(name.to_owned(), BlockSource::from_bytes(data))?;
        if table.hash_kind() != self.hash_kind {
            return Err(corrupt(
                "a table of the stack uses a different hash than the repository",
            ));
        }
        Ok(Some(table))
    }

    fn sleep(&mut self, duration: Duration) {
        // A little jitter keeps readers that retry at the same time from polling in lockstep.
        let jitter = RandomState::new().build_hasher().finish() % 25;
        std::thread::sleep(duration + duration * u32::try_from(jitter).unwrap_or(0) / 100);
    }

    fn elapsed(&mut self) -> Duration {
        self.started.elapsed()
    }
}
