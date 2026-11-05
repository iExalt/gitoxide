//! Receive a pack into a quarantine and make it part of the object database only once it's accepted, the way
//! `git receive-pack` does, but without running Git.
//!
//! A [`Quarantine`] is a temporary object directory `objects/tmp_objdir-incoming-XXXXXX` that gets the objects of a pack
//! through [`Quarantine::receive_pack()`]. They can be read together with those of the repository through
//! [`Quarantine::store()`], or by programs which are given the [`Quarantine::environment()`]. Once they are accepted,
//! [`Quarantine::migrate()`] moves them into the object database of the repository. Dropping the quarantine removes
//! it, which doesn't happen if the process is killed, like with Git.
//!
//! ## Compatibility with Git
//!
//! The result is the same as that of Git 2.56: a pack with fewer entries than the unpack limit is exploded into loose
//! objects, which are only written if the repository doesn't have them, and which freshen the modification time of
//! the copy that exists. Any other pack is kept as pack, with a `.keep` file that keeps it from being collected
//! until the references that make its objects reachable are in place, and is completed if it is thin.
//!
//! Some details differ, which is observable only by looking at the file system while things are going on:
//!
//! * Loose objects are first written to a scratch pack in the quarantine, which is removed before they are written
//!   in the order of the pack.
//! * If `core.fsync` selects loose objects and `core.fsyncMethod` is `batch`, all loose objects of a pack take part in the
//!   batch, while Git leaves out the first one. The barrier that makes them durable is created in the quarantine just like
//!   in Git, and happens before they are migrated.
//! * The `.keep` file says `receive-pack <pid> on localhost` irrespective of the actual host, which Git gets from the system.
//! * `.rev` files aren't written.
//!
//! Configuration that this module doesn't support is refused by [`Options::from_config()`], so nothing is silently
//! ignored.
//!
//! ## Checking what was received
//!
//! Git can check the objects of a pack as `unpack-objects` or `index-pack` receive them, which is `receive.fsckObjects`.
//! This module doesn't know how to check objects, but it can have an [`Inspector`] look at the pack once it's complete
//! and before any of its objects can be found in the object database: before loose objects are written, packs are kept or
//! the quarantine is migrated. If the inspector refuses the pack, [`Quarantine::receive_pack()`] fails as `receive-pack`
//! does, with what the inspector said in [`Rejected::report`].
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    io::{self, Read},
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
};

use gix_error::{ErrorExt, Result, ResultExt, bail, message, unsupported, validation};
use gix_hash::{ObjectId, oid};
use gix_object::{Find, FindHeader};
use gix_parallel::OwnShared;

use crate::{Store, loose};

/// The configuration of a [`Quarantine`], which can be derived from the configuration of a repository with
/// [`from_config()`](Options::from_config()).
#[derive(Debug, Clone)]
pub struct Options {
    /// The hash of the repository.
    pub object_hash: gix_hash::Kind,
    /// What to flush, and how.
    pub fsync: gix_fs::fsync::Options,
    /// Packs with fewer entries than this are exploded into loose objects, while others are kept as pack.
    /// This is `100` by default, and `receive.unpackLimit` or else `transfer.unpackLimit` if set.
    /// A limit of `0` keeps every pack.
    pub unpack_limit: u32,
    /// Fail if the pack has more bytes than that, which is `receive.maxInputSize` and unlimited by default.
    pub max_input_size: Option<u64>,
    /// Refuse thin packs that are kept, which have to be completed with objects of the repository. Git has no configuration
    /// for this, only the hidden `--reject-thin-pack-for-testing` option of `receive-pack`.
    pub reject_thin: bool,
    /// Move files into place with `rename()` and without checking for collisions, which is `core.createObject=rename`.
    pub create_object_rename: bool,
    /// The maximum amount of threads to use for resolving a pack, or all cores if `None`.
    pub thread_limit: Option<usize>,
    /// The maximum size of a single allocation caused by user-controlled data, or no limit if `None`.
    pub alloc_limit_bytes: Option<usize>,
    /// The compression of loose objects, which is `core.looseCompression` and then `core.compression`.
    pub loose_compression: gix_zlib::Compression,
    /// Have this look at each pack that was received before it is accepted, which is what checking objects needs.
    pub inspector: Option<Inspector>,
}

/// What an [`Inspector`] says about a pack.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inspected {
    /// `false` to refuse the pack.
    pub accepted: bool,
    /// What to tell the sender, and what to log, like warnings about an accepted pack, or the reason for refusing it.
    /// It is lines of text as the programs of Git print them.
    pub report: Vec<u8>,
}

/// A function that looks at a pack that was received into a [`Quarantine`], and decides if it is acceptable.
///
/// It is called once per pack, before any of its objects can be seen by others, and fails with an error if it can't make
/// up its mind. That's reported as `receive-pack` does for a failure to run the program that checks objects.
#[derive(Clone)]
pub struct Inspector(Arc<InspectFn>);

type InspectFn = dyn Fn(&Inspection<'_>) -> Result<Inspected> + Send + Sync;

impl Inspector {
    /// Create an inspector from `inspect`.
    pub fn new(inspect: impl Fn(&Inspection<'_>) -> Result<Inspected> + Send + Sync + 'static) -> Self {
        Inspector(Arc::new(inspect))
    }
}

impl std::fmt::Debug for Inspector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Inspector(..)")
    }
}

/// The pack that was received, as seen by an [`Inspector`].
pub struct Inspection<'a> {
    bundle: &'a gix_pack::Bundle,
    thin_bases: &'a HashSet<ObjectId>,
    odb: crate::store::Handle<OwnShared<Store>>,
    kept: bool,
}

impl Inspection<'_> {
    /// `true` if the pack is kept as pack, and `false` if it will be exploded into loose objects.
    /// This is the difference between `index-pack` and `unpack-objects`.
    pub fn kept(&self) -> bool {
        self.kept
    }

    /// Call `each` with every object that was received, in the order of the pack, until it breaks.
    /// The kind and size of the object are known without more ado, while its data is only decoded if [`Object::data()`] is called,
    /// which saves much of the work if what is looked at are only some of the objects.
    ///
    /// The objects that were added to complete a thin pack aren't received, and are left out.
    pub fn objects(&self, each: &mut dyn FnMut(&mut Object<'_>) -> Result<ControlFlow<()>>) -> Result {
        let mut by_offset: Vec<_> = (0..self.bundle.index.num_objects())
            .map(|index| (self.bundle.index.pack_offset_at_index(index), index))
            .collect();
        by_offset.sort_unstable();
        let mut inflate = gix_zlib::Inflate::default();
        let mut buf = Vec::new();
        // Objects are decoded in the order of the pack, in which what a delta is based on is usually close by.
        let mut cache = gix_pack::cache::lru::StaticLinkedList::<64>::new(64 * 1024 * 1024);
        for (offset, index) in by_offset {
            let id = self.bundle.index.oid_at_index(index);
            if self.thin_bases.contains(id) {
                continue;
            }
            let header = pack_header(self.bundle, offset, &mut inflate)?;
            let mut object = Object {
                id,
                kind: header.kind,
                size: header.object_size,
                index,
                bundle: self.bundle,
                buf: &mut buf,
                inflate: &mut inflate,
                cache: &mut cache,
            };
            if each(&mut object)?.is_break() {
                break;
            }
        }
        Ok(())
    }

    /// Find objects in what was received and in the repository with the quarantine that has no more than that.
    pub fn find(&self) -> impl Find + FindHeader + '_ {
        PackAndRepository {
            bundle: self.bundle,
            odb: &self.odb,
            inflate: Default::default(),
        }
    }
}

/// An object of a received pack, which is only decoded if its [data](Object::data()) is asked for.
pub struct Object<'a> {
    id: &'a oid,
    kind: gix_object::Kind,
    size: u64,
    index: u32,
    bundle: &'a gix_pack::Bundle,
    buf: &'a mut Vec<u8>,
    inflate: &'a mut gix_zlib::Inflate,
    cache: &'a mut dyn gix_pack::cache::DecodeEntry,
}

impl Object<'_> {
    /// The id of the object.
    pub fn id(&self) -> &oid {
        self.id
    }

    /// The kind of the object.
    pub fn kind(&self) -> gix_object::Kind {
        self.kind
    }

    /// The size of the object in bytes, which is that of its [data](Self::data()).
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Decode the object, which can be expensive if it's a delta, and return its data.
    pub fn data(&mut self) -> Result<&[u8]> {
        let (object, _) = self
            .bundle
            .get_object_by_index(self.index, self.buf, self.inflate, self.cache)?;
        Ok(object.data)
    }
}

/// Decode the header of the object whose entry is at `offset` in the pack of `bundle`, which is cheap as only as much of a delta
/// is read as it takes to learn the size of the result, and the kind that its base has.
fn pack_header(
    bundle: &gix_pack::Bundle,
    offset: gix_pack::data::Offset,
    inflate: &mut gix_zlib::Inflate,
) -> Result<gix_pack::data::decode::header::Outcome> {
    let resolve = |base: &oid| -> Result<Option<gix_pack::data::decode::header::ResolvedBase>> {
        bundle
            .index
            .lookup(base)
            .map(|index| {
                bundle
                    .pack
                    .entry(bundle.index.pack_offset_at_index(index))
                    .map(gix_pack::data::decode::header::ResolvedBase::InPack)
            })
            .transpose()
    };
    bundle.pack.decode_header(bundle.pack.entry(offset)?, inflate, &resolve)
}

struct PackAndRepository<'a> {
    bundle: &'a gix_pack::Bundle,
    odb: &'a crate::store::Handle<OwnShared<Store>>,
    /// Reused by all lookups of headers.
    inflate: std::cell::RefCell<gix_zlib::Inflate>,
}

impl Find for PackAndRepository<'_> {
    fn try_find<'a>(&self, id: &oid, buffer: &'a mut Vec<u8>) -> Result<Option<gix_object::Data<'a>>> {
        let mut inflate = gix_zlib::Inflate::default();
        // The borrow checker can't see that `buffer` isn't used if nothing is found in the pack.
        if self.bundle.index.lookup(id).is_some() {
            return Ok(self
                .bundle
                .find(id, buffer, &mut inflate, &mut gix_pack::cache::Never)?
                .map(|(data, _)| data));
        }
        self.odb.try_find(id, buffer)
    }
}

impl FindHeader for PackAndRepository<'_> {
    /// Only the header is read, so that looking at the objects that something links to, which is most of the objects
    /// that aren't new, doesn't mean to decode them.
    fn try_header(&self, id: &oid) -> Result<Option<gix_object::Header>> {
        if let Some(index) = self.bundle.index.lookup(id) {
            let offset = self.bundle.index.pack_offset_at_index(index);
            let header = pack_header(self.bundle, offset, &mut self.inflate.borrow_mut())?;
            return Ok(Some(gix_object::Header {
                kind: header.kind,
                size: header.object_size,
            }));
        }
        self.odb.try_header(id)
    }
}

impl Options {
    /// Options with the defaults of Git for a repository with `object_hash`.
    pub fn new(object_hash: gix_hash::Kind) -> Self {
        Options {
            object_hash,
            fsync: Default::default(),
            unpack_limit: 100,
            max_input_size: None,
            reject_thin: false,
            create_object_rename: false,
            thread_limit: None,
            alloc_limit_bytes: None,
            loose_compression: gix_zlib::Compression::BEST_SPEED,
            inspector: None,
        }
    }

    /// Derive options from the configuration `get`, which returns the last value for a key given as `section.key` or
    /// `section.subsection.key`, where section and key are matched case-insensitively. `fsync` is passed as is
    /// as it's parsed from more than one key.
    ///
    /// Fail if the configuration contains an invalid value, or a setting that isn't supported:
    ///
    /// * `core.sharedRepository` other than `false`, `umask` or `0`
    /// * `receive.fsckObjects` and `transfer.fsckObjects`, whichever takes effect, if `true`
    /// * `pack.indexVersion` other than `2`
    pub fn from_config(
        object_hash: gix_hash::Kind,
        fsync: gix_fs::fsync::Options,
        get: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let mut opts = Options::new(object_hash);
        opts.fsync = fsync;

        if let Some(value) = get("core.sharedRepository") {
            let value = value.to_ascii_lowercase();
            if !matches!(value.as_str(), "false" | "umask" | "0") {
                bail!(unsupported(format!(
                    "core.sharedRepository={value} isn't supported as objects wouldn't get the permissions it asks for"
                )));
            }
        }
        let fsck = ["receive.fsckObjects", "transfer.fsckObjects"]
            .iter()
            .find_map(|key| get(key).map(|value| parse_bool(key, &value)))
            .transpose()?
            .unwrap_or(false);
        if fsck {
            bail!(unsupported(
                "receive.fsckObjects and transfer.fsckObjects aren't supported, as received objects can't be checked yet"
            ));
        }
        if let Some(value) = get("pack.indexVersion").filter(|value| value.trim() != "2") {
            bail!(unsupported(format!(
                "pack.indexVersion={value} isn't supported, only version 2 indices are written"
            )));
        }
        if let Some(value) = get("core.createObject") {
            opts.create_object_rename = match value.to_ascii_lowercase().as_str() {
                "rename" => true,
                "link" => false,
                _ => bail!(validation(format!("invalid value for core.createObject: {value}"))),
            };
        }
        for key in ["transfer.unpackLimit", "receive.unpackLimit"] {
            // Git treats negative values as not set, which makes the other key or the default apply.
            if let Some(value) = get(key).filter(|value| !value.trim().starts_with('-')) {
                opts.unpack_limit = u32::try_from(parse_unsigned(key, &value)?)
                    .map_err(|_| validation(format!("{key}={value} is too large")).raise())?;
            }
        }
        if let Some(value) = get("receive.maxInputSize") {
            opts.max_input_size = Some(parse_unsigned("receive.maxInputSize", &value)?).filter(|max| *max != 0);
        }
        Ok(opts)
    }
}

fn parse_bool(key: &str, value: &str) -> Result<bool> {
    Ok(match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => true,
        "false" | "no" | "off" | "" => false,
        other => other
            .parse::<i64>()
            .map(|number| number != 0)
            .map_err(|_| validation(format!("bad boolean config value '{value}' for '{key}'")).raise())?,
    })
}

fn parse_unsigned(key: &str, value: &str) -> Result<u64> {
    let bad = || validation(format!("bad numeric config value '{value}' for '{key}'")).raise();
    let lower = value.trim().to_ascii_lowercase();
    let (digits, factor) = match lower.as_bytes().last() {
        Some(b'k') => (&lower[..lower.len() - 1], 1024),
        Some(b'm') => (&lower[..lower.len() - 1], 1024 * 1024),
        Some(b'g') => (&lower[..lower.len() - 1], 1024 * 1024 * 1024),
        _ => (lower.as_str(), 1),
    };
    let number = if let Some(hex) = digits.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else if digits.len() > 1 && digits.starts_with('0') {
        u64::from_str_radix(&digits[1..], 8)
    } else {
        digits.parse()
    }
    .map_err(|_| bad())?;
    number.checked_mul(factor).ok_or_else(bad)
}

/// The reason a pack was rejected, which is what `receive-pack` reports after `unpack` for the same failure.
#[derive(Debug)]
pub struct Rejected {
    /// What to report, like `unpack-objects abnormal exit`.
    pub reason: String,
    /// What actually went wrong, if there is something to add to the reason.
    pub cause: Option<gix_error::Error>,
    /// What an [`Inspector`] said, if it refused the pack. `receive-pack` shows that before it reports the reason.
    pub report: Vec<u8>,
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)?;
        if let Some(cause) = &self.cause {
            write!(f, ": {cause}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Rejected {}

/// What [`Quarantine::receive_pack()`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// The amount of entries the header of the pack announced.
    pub entries: u32,
    /// `true` if the pack was kept as pack, and `false` if it was exploded into loose objects.
    pub kept: bool,
    /// What an [`Inspector`] said about the pack, like warnings, which `receive-pack` shows to the sender.
    pub report: Vec<u8>,
}

/// What [`Quarantine::migrate()`] did.
#[derive(Debug)]
#[must_use = "the .keep files must be released once the references which make the new objects reachable are in place"]
pub struct Migrated {
    /// The `.keep` files in the object database, which protect the new packs until they are released.
    pub keep_files: Vec<PathBuf>,
}

impl Migrated {
    /// Remove the `.keep` files, which should happen when the references that make the objects reachable are updated,
    /// or when it's clear they never will. Files that are gone already are fine.
    ///
    /// All files are attempted, and the first failure is returned. This also happens on a best-effort basis when `self` is dropped, so that no failure path leaves them behind.
    pub fn release(mut self) -> Result {
        let mut first_error = None;
        for path in std::mem::take(&mut self.keep_files) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    first_error.get_or_insert_with(|| {
                        err.and_raise(message!("Could not remove the keep file at \"{}\"", path.display()))
                    });
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for Migrated {
    fn drop(&mut self) {
        for path in std::mem::take(&mut self.keep_files) {
            std::fs::remove_file(path).ok();
        }
    }
}

/// A temporary object directory within the object database of a repository, see the [module documentation](self).
pub struct Quarantine {
    main: PathBuf,
    dir: tempfile::TempDir,
    options: Options,
    store: OwnShared<Store>,
}

impl Quarantine {
    /// Create a quarantine in the object directory `objects_dir` of a repository.
    pub fn new(objects_dir: impl Into<PathBuf>, options: Options) -> Result<Self> {
        let main = gix_path_absolute(objects_dir.into())?;
        let mut builder = tempfile::Builder::new();
        builder.prefix("tmp_objdir-incoming-").rand_bytes(6);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Like `mkdtemp()`, only the owner may see what hasn't been accepted yet.
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let dir = builder
            .tempdir_in(&main)
            .or_raise(|| message!("Could not create a quarantine in \"{}\"", main.display()))?;
        std::fs::create_dir(dir.path().join("pack")).or_raise(|| {
            message!(
                "Could not create the pack directory of the quarantine at \"{}\"",
                dir.path().display()
            )
        })?;
        let store = Store::at_opts(
            main.clone(),
            options.object_hash,
            &mut std::iter::empty(),
            crate::store::init::Options {
                alloc_limit_bytes: options.alloc_limit_bytes,
                ..Default::default()
            },
        )
        .or_raise(|| message!("Could not open the object database at \"{}\"", main.display()))?
        .with_alternates([dir.path().to_owned()]);
        Ok(Quarantine {
            main,
            dir,
            options,
            store: OwnShared::new(store),
        })
    }

    /// The absolute path to the quarantine, which is a directory in the object directory of the repository.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The object database that sees the objects of the repository as well as those of the quarantine.
    /// Objects can be written to the quarantine only with [`receive_pack()`](Self::receive_pack()).
    pub fn store(&self) -> OwnShared<Store> {
        self.store.clone()
    }

    /// The environment for programs that should see the quarantine instead of the object database, as Git sets it for hooks:
    /// `GIT_ALTERNATE_OBJECT_DIRECTORIES` is `inherited_alternates` if set, followed by the object directory of the repository,
    /// `GIT_OBJECT_DIRECTORY` and `GIT_QUARANTINE_PATH` are the quarantine. All paths are absolute.
    pub fn environment(&self, inherited_alternates: Option<&OsStr>) -> Vec<(OsString, OsString)> {
        let main = quote_alternate(self.main.as_os_str());
        let mut alternates = OsString::new();
        if let Some(inherited) = inherited_alternates {
            alternates.push(inherited);
            alternates.push(if cfg!(windows) { ";" } else { ":" });
        }
        alternates.push(main);
        vec![
            ("GIT_ALTERNATE_OBJECT_DIRECTORIES".into(), alternates),
            ("GIT_OBJECT_DIRECTORY".into(), self.dir.path().as_os_str().to_owned()),
            ("GIT_QUARANTINE_PATH".into(), self.dir.path().as_os_str().to_owned()),
        ]
    }

    /// Read a pack from `pack` into the quarantine. Like `git receive-pack`, a pack with fewer entries than
    /// [`Options::unpack_limit`] is exploded into loose objects, and a thin pack is completed unless
    /// it's [rejected](Options::reject_thin). Other packs are kept as packs.
    ///
    /// Return [`Rejected`] with the reason that `receive-pack` reports if the pack can't be received.
    /// The quarantine can't be used to receive another pack after that, so drop it.
    ///
    /// Reading stops at the end of the pack, but `pack` is read in chunks, so up to a few KiB after it can be consumed.
    /// That is fine if nothing follows the pack, as is the case when receiving a push.
    pub fn receive_pack(
        &mut self,
        pack: &mut dyn Read,
        interrupt: &AtomicBool,
    ) -> std::result::Result<Received, Rejected> {
        let header = read_pack_header(pack)?;
        let kept = header.entries >= self.options.unpack_limit;
        let abnormal = |cause: gix_error::Error| {
            let reason = if kept {
                "index-pack abnormal exit"
            } else {
                "unpack-objects abnormal exit"
            };
            match cause.downcast_any_ref::<Refused>() {
                Some(Refused(report)) => Rejected {
                    reason: reason.into(),
                    cause: None,
                    report: report.clone(),
                },
                None => Rejected {
                    reason: reason.into(),
                    cause: Some(cause),
                    report: Vec::new(),
                },
            }
        };
        let mut stream = io::BufReader::new(Limited {
            inner: io::Cursor::new(header.raw).chain(pack),
            consumed: 0,
            max: self.options.max_input_size,
        });
        let report = if kept {
            self.keep(&mut stream, interrupt)
        } else {
            self.unpack(&mut stream, interrupt)
        }
        .map_err(abnormal)?;
        Ok(Received {
            entries: header.entries,
            kept,
            report,
        })
    }

    fn pack_options(&self, fsync: gix_fs::fsync::Options) -> gix_pack::bundle::write::Options {
        gix_pack::bundle::write::Options {
            thread_limit: self.options.thread_limit,
            alloc_limit_bytes: self.options.alloc_limit_bytes,
            compression: self.options.loose_compression,
            fsync,
            ..Default::default()
        }
    }

    /// The base objects for completing thin packs, or `None` if they are to be rejected.
    ///
    /// Only packs that are kept can be rejected for being thin as it's `index-pack` that is told to not fix them.
    /// `unpack-objects` takes the bases from the repository whatever the setting.
    fn thin_lookup(&self, kept: bool) -> Option<RecordingFind> {
        (!(kept && self.options.reject_thin)).then(|| RecordingFind {
            odb: self.store.to_handle(),
            requested: Arc::default(),
        })
    }

    /// Let the inspector, if there is one, decide about the pack `bundle` whose objects that complete it are `thin_bases`,
    /// and return what it says, or fail with it if the pack is refused.
    fn inspect(&self, bundle: &gix_pack::Bundle, thin_bases: &HashSet<ObjectId>, kept: bool) -> Result<Vec<u8>> {
        let Some(inspector) = &self.options.inspector else {
            return Ok(Vec::new());
        };
        let inspected = (inspector.0)(&Inspection {
            bundle,
            thin_bases,
            odb: self.store.to_handle(),
            kept,
        })?;
        if inspected.accepted {
            Ok(inspected.report)
        } else {
            bail!(Refused(inspected.report))
        }
    }

    fn keep(&mut self, stream: &mut dyn io::BufRead, interrupt: &AtomicBool) -> Result<Vec<u8>> {
        let directory = self.dir.path().join("pack");
        let thin_lookup = self.thin_lookup(true);
        let requested = thin_lookup.as_ref().map(|lookup| lookup.requested.clone());
        let outcome = gix_pack::Bundle::write_to_directory(
            stream,
            Some(&directory),
            &mut gix_utils::progress::Discard,
            interrupt,
            thin_lookup,
            self.options.object_hash,
            self.pack_options(self.options.fsync),
        )?;
        let mut report = Vec::new();
        if self.options.inspector.is_some()
            && let Some(index_path) = &outcome.index_path
        {
            let thin_bases = requested.map_or_else(HashSet::new, |bases| {
                std::mem::take(&mut *bases.lock().expect("no panic while held"))
            });
            let bundle = gix_pack::Bundle::at(index_path, self.options.object_hash)?;
            report = self.inspect(&bundle, &thin_bases, true)?;
        }
        if let Some(keep_path) = outcome.keep_path {
            std::fs::write(
                &keep_path,
                format!("receive-pack {} on localhost\n", std::process::id()),
            )
            .or_raise(|| message!("Could not write the keep file at \"{}\"", keep_path.display()))?;
        }
        Ok(report)
    }

    fn unpack(&mut self, stream: &mut dyn io::BufRead, interrupt: &AtomicBool) -> Result<Vec<u8>> {
        let scratch = tempfile::Builder::new()
            .prefix("scratch-")
            .tempdir_in(self.dir.path())
            .or_raise(|| message("Could not create a scratch directory for unpacking"))?;
        let thin_lookup = self.thin_lookup(false);
        let requested = thin_lookup.as_ref().map(|lookup| lookup.requested.clone());
        // The scratch pack is not to be made durable, the loose objects are.
        let no_fsync = gix_fs::fsync::Options {
            components: gix_fs::fsync::Components::NONE,
            ..Default::default()
        };
        let outcome = gix_pack::Bundle::write_to_directory(
            stream,
            Some(scratch.path()),
            &mut gix_utils::progress::Discard,
            interrupt,
            thin_lookup,
            self.options.object_hash,
            self.pack_options(no_fsync),
        )?;
        let Some(index_path) = outcome.index_path else {
            return Ok(Vec::new());
        };
        // Objects that were added to complete the pack aren't part of it and are only read, never written or freshened.
        let thin_bases = requested.map(|bases| std::mem::take(&mut *bases.lock().expect("no panic while held")));
        let bundle = gix_pack::Bundle::at(&index_path, self.options.object_hash)?;
        let report = self.inspect(&bundle, thin_bases.as_ref().unwrap_or(&HashSet::new()), false)?;
        let mut by_offset: Vec<_> = (0..bundle.index.num_objects())
            .map(|index| (bundle.index.pack_offset_at_index(index), index))
            .collect();
        by_offset.sort_unstable();

        let batch = self.options.fsync.loose_object_batch();
        let loose = loose::Store::at_opts(
            self.dir.path(),
            self.options.object_hash,
            loose::Options {
                alloc_limit_bytes: self.options.alloc_limit_bytes,
                compression: self.options.loose_compression,
                fsync: if batch.is_some() {
                    // Write each object out and flush once at the end, which is what the batch is.
                    gix_fs::fsync::Options {
                        components: gix_fs::fsync::Components::LOOSE_OBJECT,
                        method: gix_fs::fsync::Method::WriteoutOnly,
                        fsync_object_files: false,
                    }
                } else {
                    self.options.fsync
                },
            },
        );
        let mut freshen = Freshen::new(self.directories()?, self.options.object_hash);
        let (mut buf, mut inflate) = (Vec::new(), gix_zlib::Inflate::default());
        let mut wrote_any = false;
        for (_, index) in by_offset {
            let id = bundle.index.oid_at_index(index).to_owned();
            if thin_bases.as_ref().is_some_and(|bases| bases.contains(&id)) || freshen.freshen(&id) {
                continue;
            }
            let (object, _) = bundle.get_object_by_index(index, &mut buf, &mut inflate, &mut gix_pack::cache::Never)?;
            gix_object::Write::write_buf_with_known_id(&loose, object.kind, object.data, id)
                .or_raise(|| message!("Could not write loose object {id}"))?;
            wrote_any = true;
        }
        if let Some(batch) = batch.filter(|_| wrote_any) {
            batch
                .barrier(self.dir.path())
                .or_raise(|| message("Could not flush the loose objects of the quarantine"))?;
        }
        Ok(report)
    }

    /// The object directories to find objects in, which is the quarantine first.
    fn directories(&self) -> Result<Vec<PathBuf>> {
        let mut out = vec![self.dir.path().to_owned()];
        out.extend(
            self.store
                .object_directories(&self.main)?
                .into_iter()
                .filter(|dir| dir != self.dir.path()),
        );
        Ok(out)
    }

    /// Move everything in the quarantine into the object database of the repository, and remove the quarantine.
    /// Everything is flushed before it is moved, so that the objects exist in full or not at all after power loss.
    ///
    /// Loose objects come first, and packs follow with their `.keep` file, the data, the reverse index and finally the
    /// index. Files that exist already aren't replaced, and it is an error if packs with the same name have a different content.
    /// All files are attempted even in the face of failures, and the first failure is returned afterwards, with the quarantine removed.
    pub fn migrate(self) -> Result<Migrated> {
        // Like Git, make everything that was written durable before it can be found by its name. This is a flush
        // of a scratch file in the quarantine, which is what orders it after the previous writes. It isn't optional
        // as Git doesn't make it depend on what `core.fsync` selects.
        self.options
            .fsync
            .barrier(self.dir.path())
            .or_raise(|| message("Could not flush the quarantine before moving it into the object database"))?;
        let mut keep_files = Vec::new();
        let mut first_error = None;
        migrate_dir(
            self.dir.path(),
            &self.main,
            self.options.create_object_rename,
            &mut keep_files,
            &mut first_error,
        );
        let removed = self.dir.close();
        match (first_error, removed) {
            (Some(err), _) => Err(err),
            (None, Err(err)) => Err(err.and_raise(message("Could not remove the quarantine after migrating it"))),
            (None, Ok(())) => Ok(Migrated { keep_files }),
        }
    }
}

fn gix_path_absolute(path: PathBuf) -> Result<PathBuf> {
    std::path::absolute(&path).or_raise(|| message!("Could not make \"{}\" absolute", path.display()))
}

/// Quote `path` for use in `GIT_ALTERNATE_OBJECT_DIRECTORIES` as Git does, which is only necessary if it contains the
/// separator or starts with a quote.
fn quote_alternate(path: &OsStr) -> OsString {
    let separator = if cfg!(windows) { b';' } else { b':' };
    let bytes = path.as_encoded_bytes();
    if bytes.first() != Some(&b'"') && !bytes.contains(&separator) {
        return path.to_owned();
    }
    let mut quoted = vec![b'"'];
    for &byte in bytes {
        match byte {
            b'"' | b'\\' => quoted.extend([b'\\', byte]),
            0x07 => quoted.extend(b"\\a"),
            0x08 => quoted.extend(b"\\b"),
            b'\t' => quoted.extend(b"\\t"),
            b'\n' => quoted.extend(b"\\n"),
            0x0b => quoted.extend(b"\\v"),
            0x0c => quoted.extend(b"\\f"),
            b'\r' => quoted.extend(b"\\r"),
            byte if !(0x20..0x7f).contains(&byte) => quoted.extend(format!("\\{byte:03o}").bytes()),
            byte => quoted.push(byte),
        }
    }
    quoted.push(b'"');
    // SAFETY: the bytes are those of `path`, with ASCII additions and replacements only.
    #[allow(unsafe_code)]
    unsafe {
        OsString::from_encoded_bytes_unchecked(quoted)
    }
}

struct PackHeader {
    raw: [u8; 12],
    entries: u32,
}

/// Read the header of a pack like `read_pack_header()` does, and fail with the same reasons.
fn read_pack_header(pack: &mut dyn Read) -> std::result::Result<PackHeader, Rejected> {
    let reject = |reason: &str| Rejected {
        reason: reason.into(),
        cause: None,
        report: Vec::new(),
    };
    let mut raw = [0u8; 12];
    let mut filled = 0;
    while filled < raw.len() {
        match pack.read(&mut raw[filled..]) {
            Ok(0) => return Err(reject("eof before pack header was fully read")),
            Ok(n) => filled += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                return Err(Rejected {
                    reason: "eof before pack header was fully read".into(),
                    cause: Some(err.and_raise(message("Could not read the pack header"))),
                    report: Vec::new(),
                });
            }
        }
    }
    if &raw[..4] != b"PACK" {
        return Err(reject("protocol error (pack signature mismatch detected)"));
    }
    if !matches!(u32::from_be_bytes(raw[4..8].try_into().expect("4 bytes")), 2 | 3) {
        return Err(reject("protocol error (pack version unsupported)"));
    }
    Ok(PackHeader {
        entries: u32::from_be_bytes(raw[8..].try_into().expect("4 bytes")),
        raw,
    })
}

/// Fail once more than `max` bytes were read, as `--max-input-size` does.
struct Limited<R> {
    inner: R,
    consumed: u64,
    max: Option<u64>,
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.consumed += read as u64;
        if self.max.is_some_and(|max| self.consumed > max) {
            return Err(io::Error::other("pack exceeds maximum allowed size"));
        }
        Ok(read)
    }
}

/// The error that makes `receive_pack()` return what the inspector refused with.
#[derive(Debug)]
struct Refused(Vec<u8>);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the received objects were refused")
    }
}

impl std::error::Error for Refused {}

/// Look up the bases to complete a thin pack and remember which ones were asked for.
struct RecordingFind {
    odb: crate::store::Handle<OwnShared<Store>>,
    requested: Arc<Mutex<HashSet<ObjectId>>>,
}

impl Find for RecordingFind {
    fn try_find<'a>(&self, id: &oid, buffer: &'a mut Vec<u8>) -> Result<Option<gix_object::Data<'a>>> {
        let found = self.odb.try_find(id, buffer)?;
        if found.is_some() {
            self.requested
                .lock()
                .expect("no panic while held")
                .insert(id.to_owned());
        }
        Ok(found)
    }
}

/// Finds out if objects exist in any of the object directories, and refreshes their modification time if so, which
/// is what keeps garbage collection from removing them while they are about to become reachable again.
struct Freshen {
    object_hash: gix_hash::Kind,
    dirs: Vec<PathBuf>,
    /// Pack indices of each directory in `dirs` by position, opened on first use.
    packs: Vec<Option<Vec<(gix_pack::index::File, PathBuf)>>>,
}

impl Freshen {
    fn new(dirs: Vec<PathBuf>, object_hash: gix_hash::Kind) -> Self {
        Freshen {
            object_hash,
            packs: dirs.iter().map(|_| None).collect(),
            dirs,
        }
    }

    /// Return `true` if the object exists and its file could be touched. Like Git, look for a pack with the object before
    /// looking for a loose object, directory by directory.
    fn freshen(&mut self, id: &oid) -> bool {
        for (dir, packs) in self.dirs.iter().zip(&mut self.packs) {
            let packs = packs.get_or_insert_with(|| open_packs(dir, self.object_hash));
            if let Some((_, pack_path)) = packs.iter().find(|(index, _)| index.lookup(id).is_some())
                && touch(pack_path)
            {
                return true;
            }
            if touch(&loose::hash_path(id, dir.clone())) {
                return true;
            }
        }
        false
    }
}

fn open_packs(dir: &Path, object_hash: gix_hash::Kind) -> Vec<(gix_pack::index::File, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir.join("pack")) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "idx").then_some(path)
        })
        .filter_map(|idx_path| {
            // Git recognizes cruft packs by their `.mtimes` file. Objects in them expire by what it says, so touching
            // the pack protects nothing, and Git writes the object again instead.
            if idx_path.with_extension("mtimes").is_file() {
                return None;
            }
            let index = gix_pack::index::File::at(&idx_path, object_hash).ok()?;
            Some((index, idx_path.with_extension("pack")))
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// Set the modification time of `path` to now, and return `false` if it doesn't exist or couldn't be changed.
fn touch(path: &Path) -> bool {
    let now = filetime::FileTime::now();
    path.is_file() && filetime::set_file_times(path, now, now).is_ok()
}

/// The order in which Git copies files out of a quarantine, which makes sure a pack can only be found by its index
/// when everything else is in place: loose objects first, then `.keep`, `.pack`, `.rev` and `.idx` in this order.
fn migration_priority(name: &str) -> u8 {
    if !name.starts_with("pack") {
        return 0;
    }
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("keep") => 1,
        Some("pack") => 2,
        Some("rev") => 3,
        Some("idx") => 4,
        _ => 5,
    }
}

fn migrate_dir(
    src: &Path,
    dst: &Path,
    rename: bool,
    keep_files: &mut Vec<PathBuf>,
    first_error: &mut Option<gix_error::Error>,
) {
    fn fail(first_error: &mut Option<gix_error::Error>, err: gix_error::Error) {
        first_error.get_or_insert(err);
    }
    let mut names: Vec<String> = match std::fs::read_dir(src) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.'))
            .collect(),
        Err(err) => {
            return fail(
                first_error,
                err.and_raise(message!(
                    "Could not read the quarantine directory \"{}\"",
                    src.display()
                )),
            );
        }
    };
    names.sort_by(|a, b| migration_priority(a).cmp(&migration_priority(b)).then_with(|| a.cmp(b)));
    for name in names {
        let (from, to) = (src.join(&name), dst.join(&name));
        if from.is_dir() {
            if let Err(err) = std::fs::create_dir(&to)
                && err.kind() != io::ErrorKind::AlreadyExists
            {
                fail(
                    first_error,
                    err.and_raise(message!("Could not create the directory \"{}\"", to.display())),
                );
                continue;
            }
            migrate_dir(&from, &to, rename, keep_files, first_error);
            continue;
        }
        let is_loose_shard_entry = src
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|dir| dir.len() == 2 && dir.bytes().all(|b| b.is_ascii_hexdigit()));
        match finalize(&from, &to, rename, is_loose_shard_entry) {
            Ok(()) => {
                if to.extension().is_some_and(|ext| ext == "keep") {
                    keep_files.push(to);
                }
            }
            Err(err) => fail(first_error, err),
        }
    }
}

/// Move `from` to `to` like `finalize_object_file()` of Git does: link it and remove the source, falling back to renaming,
/// and compare the content of an existing destination unless it's a loose object.
fn finalize(from: &Path, to: &Path, rename: bool, skip_collision_check: bool) -> Result {
    let mut retries = 0;
    loop {
        let mut existing = false;
        let mut failure = None;
        if rename {
            existing = to.exists();
        } else {
            match std::fs::hard_link(from, to) {
                Ok(()) => {
                    std::fs::remove_file(from).ok();
                    return Ok(());
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => existing = true,
                Err(err) => failure = Some(err),
            }
        }
        if !existing && (rename || failure.is_some()) {
            // Git falls back to renaming if linking isn't possible, which can't check for collisions.
            existing = to.exists();
            if !existing {
                match std::fs::rename(from, to) {
                    Ok(()) => return Ok(()),
                    Err(err) => failure = Some(err),
                }
            }
        }
        if !existing {
            std::fs::remove_file(from).ok();
            let err = failure.expect("set if neither linking nor renaming worked");
            return Err(err.and_raise(message!("unable to write file {}", to.display())));
        }
        if !skip_collision_check {
            match same_content(from, to) {
                Ok(Some(true)) => {}
                Ok(Some(false)) => {
                    bail!(validation(format!(
                        "files '{}' and '{}' differ in contents",
                        from.display(),
                        to.display()
                    )));
                }
                Ok(None) if retries < 5 => {
                    retries += 1;
                    continue;
                }
                Ok(None) => bail!(message!("unable to write repeatedly vanishing file {}", to.display())),
                Err(err) => {
                    return Err(err.and_raise(message!(
                        "Could not compare \"{}\" with \"{}\"",
                        from.display(),
                        to.display()
                    )));
                }
            }
        }
        std::fs::remove_file(from).ok();
        return Ok(());
    }
}

/// Compare the content of two files, or return `None` if `to` vanished.
fn same_content(from: &Path, to: &Path) -> io::Result<Option<bool>> {
    let to_file = match std::fs::File::open(to) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let from_file = std::fs::File::open(from)?;
    if from_file.metadata()?.len() != to_file.metadata()?.len() {
        return Ok(Some(false));
    }
    let (mut a, mut b) = (io::BufReader::new(from_file), io::BufReader::new(to_file));
    let (mut left, mut right) = ([0u8; 8192], [0u8; 8192]);
    loop {
        let read = a.read(&mut left)?;
        if read == 0 {
            return Ok(Some(true));
        }
        b.read_exact(&mut right[..read])?;
        if left[..read] != right[..read] {
            return Ok(Some(false));
        }
    }
}
