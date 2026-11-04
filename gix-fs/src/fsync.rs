//! Flush written data to stable storage as configured by `core.fsync` and `core.fsyncMethod`, following Git v2.56.
//!
//! Git decides *what* to flush per kind of data ([`Components`]) and *how* ([`Method`]). It flushes each file once,
//! before it is made visible by a rename or link. It never flushes directories, and it does not flush reflogs at all.
//! The defaults keep loose objects and references out of the flushed set on most platforms.
//!
//! Flushing narrows the window of data loss on power failure. It is no proof of durability: Git allows disabling it,
//! and what a flush guarantees depends on the file system and the device.
use std::{fs::File, io, path::Path};

use bstr::{BStr, BString, ByteSlice};

/// The kinds of persistent data that `core.fsync` selects, as a set.
///
/// Use the associated constants and [`Components::parse()`]. The bits are an implementation detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Components(u8);

impl Components {
    /// No component.
    pub const NONE: Self = Self(0);
    /// Loose object files.
    pub const LOOSE_OBJECT: Self = Self(1 << 0);
    /// Pack data files.
    pub const PACK: Self = Self(1 << 1);
    /// Pack indices and reverse indices.
    pub const PACK_METADATA: Self = Self(1 << 2);
    /// Commit-graph files.
    pub const COMMIT_GRAPH: Self = Self(1 << 3);
    /// The index.
    pub const INDEX: Self = Self(1 << 4);
    /// Loose references and `packed-refs`, but not reflogs, which Git never flushes.
    pub const REFERENCE: Self = Self(1 << 5);
    /// [`LOOSE_OBJECT`](Self::LOOSE_OBJECT) and [`PACK`](Self::PACK).
    pub const OBJECTS: Self = Self(Self::LOOSE_OBJECT.0 | Self::PACK.0);
    /// [`PACK_METADATA`](Self::PACK_METADATA) and [`COMMIT_GRAPH`](Self::COMMIT_GRAPH).
    pub const DERIVED_METADATA: Self = Self(Self::PACK_METADATA.0 | Self::COMMIT_GRAPH.0);
    /// [`OBJECTS`](Self::OBJECTS) and [`REFERENCE`](Self::REFERENCE).
    pub const COMMITTED: Self = Self(Self::OBJECTS.0 | Self::REFERENCE.0);
    /// [`COMMITTED`](Self::COMMITTED) and [`INDEX`](Self::INDEX).
    pub const ADDED: Self = Self(Self::COMMITTED.0 | Self::INDEX.0);
    /// Every component.
    pub const ALL: Self = Self(Self::ADDED.0 | Self::DERIVED_METADATA.0);

    /// What Git flushes without configuration: packs and derived metadata, but neither loose objects nor references.
    /// On Windows, loose objects are flushed as well.
    pub const DEFAULT: Self = if cfg!(windows) {
        Self(Self::PACK.0 | Self::DERIVED_METADATA.0 | Self::LOOSE_OBJECT.0)
    } else {
        Self(Self::PACK.0 | Self::DERIVED_METADATA.0)
    };

    /// Return `true` if every component in `other` is part of `self`.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Return `true` if `self` has no component.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Interpret the value of `core.fsync` the way Git does and return the selected components along with the
    /// tokens that name no component, which Git ignores with a warning.
    ///
    /// The value is a list separated by `,`. Each entry is the name of a component or a group, an unambiguous prefix
    /// of it (so `pack` also selects `pack-metadata`), or either one negated with a leading `-`. Names are
    /// `loose-object`, `pack`, `pack-metadata`, `commit-graph`, `index`, `objects`, `reference`, `derived-metadata`,
    /// `committed`, `added` and `all`. `none` resets the defaults, and Git only recognizes it as the *last* entry.
    ///
    /// The selection starts from [`Components::DEFAULT`], so a value of `reference` adds to the defaults.
    /// Negated components are removed from the defaults only, hence `pack,-pack` still selects `pack`.
    /// A lone `-` ends the interpretation.
    ///
    /// Each configuration entry stands on its own: callers must pass the last value of `core.fsync`.
    pub fn parse(value: &BStr) -> (Self, Vec<BString>) {
        const NAMES: &[(&str, Components)] = &[
            ("loose-object", Components::LOOSE_OBJECT),
            ("pack", Components::PACK),
            ("pack-metadata", Components::PACK_METADATA),
            ("commit-graph", Components::COMMIT_GRAPH),
            ("index", Components::INDEX),
            ("objects", Components::OBJECTS),
            ("reference", Components::REFERENCE),
            ("derived-metadata", Components::DERIVED_METADATA),
            ("committed", Components::COMMITTED),
            ("added", Components::ADDED),
            ("all", Components::ALL),
        ];
        let (mut current, mut positive, mut negative) = (Self::DEFAULT.0, 0_u8, 0_u8);
        let mut ignored = Vec::new();
        let mut rest = Some(value.as_bytes());
        while let Some(remainder) = rest {
            let remainder = remainder.trim_start_with(|c| matches!(c, ',' | ' ' | '\t' | '\n' | '\r'));
            let (mut token, next) = match remainder.find_byte(b',') {
                Some(pos) => (&remainder[..pos], Some(&remainder[pos..])),
                None => (remainder, None),
            };
            if remainder == b"none" {
                current = 0;
                rest = next;
                continue;
            }
            let negated = token.first() == Some(&b'-');
            if negated {
                token = &token[1..];
            }
            if token.is_empty() {
                break;
            }
            let mut found = false;
            for (name, components) in NAMES {
                if name.as_bytes().starts_with(token) {
                    found = true;
                    if negated {
                        negative |= components.0;
                    } else {
                        positive |= components.0;
                    }
                }
            }
            if !found {
                ignored.push(token.into());
            }
            rest = next;
        }
        (Self((current & !negative) | positive), ignored)
    }
}

impl std::ops::BitOr for Components {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl Default for Components {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How `core.fsyncMethod` asks for data to be flushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// Ask the operating system to flush all the way to the device. On macOS this is `F_FULLFSYNC`.
    Fsync,
    /// Only force the data out of the page cache, without asking the device to flush its caches, where the platform
    /// can do so. On Linux this is `sync_file_range()`, and on macOS it is `fsync()`.
    /// Platforms that cannot, including Windows, flush fully instead, which is slower but never less durable.
    WriteoutOnly,
    /// Like [`Fsync`](Self::Fsync) for everything but loose objects that are written as part of a batch.
    /// For those, Git writes out each object and flushes once per batch, see [`Options::loose_object_batch()`].
    /// Without a batch, it behaves as [`Fsync`](Self::Fsync).
    Batch,
}

impl Method {
    /// Git's default: [`WriteoutOnly`](Self::WriteoutOnly) on macOS, [`Batch`](Self::Batch) on Windows and
    /// [`Fsync`](Self::Fsync) elsewhere.
    pub const DEFAULT: Self = if cfg!(target_vendor = "apple") {
        Self::WriteoutOnly
    } else if cfg!(windows) {
        Self::Batch
    } else {
        Self::Fsync
    };

    /// Interpret the value of `core.fsyncMethod`, or return `None` for an unknown value, which Git ignores with a warning.
    pub fn parse(value: &BStr) -> Option<Self> {
        Some(match value.as_bytes() {
            b"fsync" => Self::Fsync,
            b"writeout-only" => Self::WriteoutOnly,
            b"batch" => Self::Batch,
            _ => return None,
        })
    }
}

impl Default for Method {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The settings `core.fsync`, `core.fsyncMethod` and `core.fsyncObjectFiles` amount to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Options {
    /// The data to flush.
    pub components: Components,
    /// How to flush it.
    pub method: Method,
    /// The deprecated `core.fsyncObjectFiles`: flush every loose object, whatever `components` says.
    pub fsync_object_files: bool,
}

impl Options {
    /// Return `true` if data of the given single `component` is flushed.
    pub fn is_enabled(&self, component: Components) -> bool {
        self.components.contains(component) || (self.fsync_object_files && component == Components::LOOSE_OBJECT)
    }

    /// Flush `file`, which has been written completely, if `component` is configured to be flushed, using the
    /// configured method, where [`Method::Batch`] flushes fully. Return `Ok(false)` if nothing was done.
    pub fn flush(&self, file: &File, component: Components) -> io::Result<bool> {
        if !self.is_enabled(component) {
            return Ok(false);
        }
        flush_file(file, self.method)?;
        Ok(true)
    }

    /// Return the means to flush loose objects in a batch if the configuration asks for it, which requires
    /// [`Method::Batch`] and the [`Components::LOOSE_OBJECT`] component. The deprecated `core.fsyncObjectFiles`
    /// does not enable batching.
    ///
    /// If `None`, flush loose objects one by one with [`Options::flush()`].
    pub fn loose_object_batch(&self) -> Option<Batch> {
        (self.method == Method::Batch && self.components.contains(Components::LOOSE_OBJECT)).then_some(Batch(()))
    }

    /// Create, flush and remove a scratch file named `bulk_fsync_XXXXXX` in `dir`, using the configured method,
    /// whatever the selected components are.
    ///
    /// Flushing a file orders the writes to it before the flush, and Git relies on that to make all *previously written
    /// but not flushed* data in the same file system durable by the time the scratch file is. This is the barrier Git
    /// places before it moves quarantined objects into the object database, so use it before such a migration.
    pub fn barrier(&self, dir: &Path) -> io::Result<()> {
        let file = scratch::create(dir)?;
        flush_file(&file.0, self.method)
    }
}

/// The means to flush loose objects in a batch, as obtained from [`Options::loose_object_batch()`].
///
/// Write out each object with [`flush_object()`](Self::flush_object()) before it is made visible, and
/// [`barrier()`](Self::barrier()) once all of them are written, before they are moved to their final location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batch(());

impl Batch {
    /// Write out `file`, which has been written completely, without flushing the device if the platform can do that,
    /// and flush it fully otherwise.
    pub fn flush_object(&self, file: &File) -> io::Result<()> {
        flush_file(file, Method::WriteoutOnly)
    }

    /// Create, fully flush and remove a scratch file named `bulk_fsync_XXXXXX` in `dir`, which makes the objects
    /// that were written out before durable.
    pub fn barrier(&self, dir: &Path) -> io::Result<()> {
        let file = scratch::create(dir)?;
        flush_file(&file.0, Method::Fsync)
    }
}

/// Flush `file` regardless of any component selection, using `method`.
///
/// Interrupted flushes are retried. With [`Method::WriteoutOnly`], a platform that cannot write out without flushing
/// the device flushes it fully instead.
pub fn flush_file(file: &File, method: Method) -> io::Result<()> {
    if method == Method::WriteoutOnly && writeout(file).is_ok() {
        return Ok(());
    }
    hardware_flush(file)
}

mod scratch {
    use std::{fs::File, io, path::Path, path::PathBuf};

    /// A scratch file that is removed on drop.
    pub struct Scratch(pub File, PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_file(&self.1).ok();
        }
    }

    pub fn create(dir: &Path) -> io::Result<Scratch> {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
            ^ u64::from(std::process::id()).rotate_left(32);
        for _ in 0..100 {
            let mut name = String::from("bulk_fsync_");
            for _ in 0..6 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                name.push(ALPHABET[((seed >> 33) % ALPHABET.len() as u64) as usize] as char);
            }
            let path = dir.join(name);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            match options.open(&path) {
                Ok(file) => return Ok(Scratch(file, path)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not find an unused name for the scratch file",
        ))
    }
}

/// Write out the data of `file` without flushing the device, or fail if the platform cannot.
// The standard library offers no safe way to ask for a write-out that skips the device's caches.
#[allow(unsafe_code)]
fn writeout(file: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: the file descriptor is valid for the duration of the call as `file` is borrowed.
        retry_on_interrupt(|| unsafe { libc::fsync(std::os::fd::AsRawFd::as_raw_fd(file)) })
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // Wait for earlier write-out, start ours and wait for it. An offset and length of zero cover the entire file.
        // SAFETY: the file descriptor is valid for the duration of the call as `file` is borrowed.
        let res = unsafe {
            libc::sync_file_range(
                std::os::fd::AsRawFd::as_raw_fd(file),
                0,
                0,
                libc::SYNC_FILE_RANGE_WAIT_BEFORE | libc::SYNC_FILE_RANGE_WRITE | libc::SYNC_FILE_RANGE_WAIT_AFTER,
            )
        };
        if res < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    {
        let _ = file;
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Flush `file` and the caches of its device as far as the platform lets us.
///
/// The standard library's `sync_all()` does this on every platform, using `F_FULLFSYNC` on macOS and
/// `FlushFileBuffers()` on Windows.
fn hardware_flush(file: &File) -> io::Result<()> {
    loop {
        match file.sync_all() {
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            res => return res,
        }
    }
}

#[cfg(target_vendor = "apple")]
fn retry_on_interrupt(mut op: impl FnMut() -> libc::c_int) -> io::Result<()> {
    loop {
        if op() >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}
