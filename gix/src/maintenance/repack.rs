//! Putting objects into a pack and removing the packs it replaces, like `git repack -a -d` and `git repack -d` do.
//!
//! [`Repository::repack()`] with [`Scope::All`] writes one pack with all reachable objects of this repository that are
//! loose or in a pack that has no `.keep` file, and then removes the packs it replaces. [`Scope::Incremental`] only packs
//! the reachable loose objects, and doesn't remove packs. Packs and objects of alternates are never looked at.
//!
//! Objects aren't deltified: an object that is in a pack is copied as it is, with its delta if the base is in the new pack
//! as well, and a loose object is stored whole. A repository that is repacked after loose objects accumulated is thus
//! about as big as it was, while Git would find deltas between them.
//!
//! # What happens to what is unreachable
//!
//! Objects of removed packs that aren't reachable are [loosened](Unreachable::Loosen) by default, with the time of the pack they
//! were in, the way `git repack -A` does. This is what keeps objects that a writer needs and didn't reference yet, like those of a
//! received pack that hasn't had its references updated: they stay for [`prune()`](Repository::prune()) to decide
//! about, which looks at their age. [`Unreachable::Drop`] removes them with their pack like `git repack -a` does, and a writer who
//! makes such an object reachable after the walk found the roots loses it, so it must not be used while others write.
//!
//! # Order, and what an interruption leaves
//!
//! 1. The packs to replace are listed, before anything is looked at, so that a pack that arrives later is never removed.
//! 2. The roots are found, and with them the objects for the new pack.
//! 3. The new pack is written next to the others, under a name that doesn't count until the index has been moved into
//!    place, and with a `.keep` file that has the process, host and time, and marks it as one that no other `repack` may replace
//!    until this is done. The marker of a process that is gone, or of one that is over twelve hours old, is removed by the next run,
//!    while a run that finds another marker stops before it changes anything.
//! 4. Unreachable objects that are in a pack to be removed are written loose, with the time of the pack or of the
//!    object if the pack is a cruft pack, and what was written is flushed before anything is removed.
//! 5. A multi-pack-index is removed with what belongs to it, as it names packs that go away. Likewise a commit-graph
//!    that names an object that is dropped.
//! 6. The packs are removed, each one after checking that it still is the pack that was listed: it has the same size and
//!    time, which a writer who freshened it to keep its objects changes, and it has no `.keep` file. A pack that
//!    changed stays, and the next run deals with it.
//! 7. Loose objects that are in the new pack are removed, and the `.keep` of the new pack.
//!
//! The new pack contains all that was packed before anything is removed, so an interruption at any point leaves all objects in
//! place, possibly more than once and with a `.keep` file that the next run removes. The packs that were listed must not be changed by anyone else while this runs, which includes another
//! `repack`: it takes no lock, so callers have to make sure that there is one at a time.
//!
//! # Differences from Git
//!
//! * Objects in packs are never deltified, see above.
//! * There are no bitmaps, multi-pack-indices or reverse indices written.
//! * Loose copies of objects that a kept pack has stay, while Git's `prune-packed` removes them.
//! * Files of packs that have no index, which a crash in the middle of removing a pack leaves, are not cleaned up.
//! * Setting the time of loose objects needs write access on Windows, where this was not tried.
//! * Whether the process of an interrupted run is gone is only known on Linux, so elsewhere its marker blocks the next run until it is 12 hours old.
//! * What `.keep` files and `gc.bigPackThreshold` do to Git's choice of packs doesn't apply: there are two scopes only.
use std::{
    collections::BTreeSet,
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, UNIX_EPOCH},
};

use gix_error::{ErrorExt, Result, ResultExt, bail, message};
use gix_hash::ObjectId;
use gix_hashtable::{HashMap, HashSet};
use gix_object::Find as _;

use super::prune::{cruft_mtimes, mtime_of, remove_empty_fan_out_dirs, scan_loose};
use crate::{
    Repository,
    config::{cache::util::ApplyLeniency, tree::Pack as PackKeys},
};

/// The start of the content of the `.keep` file of a pack that this module is making, which is followed by the process, the
/// host and the time, and marks a file that was left by an interrupted run once the process is gone.
const KEEP_MARKER: &str = "gix repack in progress\n";

/// A marker that no process has refreshed for this long is stale whichever process wrote it, as with `gc.pid`.
const STALE_MARKER_SECONDS: u64 = 12 * 60 * 60;

fn host_name() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|name| name.trim().to_owned())
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn marker_content() -> String {
    format!(
        "{KEEP_MARKER}pid {}\nhost {}\ntime {}\n",
        std::process::id(),
        host_name().unwrap_or_default(),
        now_seconds()
    )
}

/// Return `None` if `content` isn't a marker of this module, and otherwise whether the process that wrote it is gone, which is
/// only known on Linux for a process on the same host, or it's been too long.
fn marker_is_stale(content: &[u8]) -> Option<bool> {
    let content = std::str::from_utf8(content).ok()?.strip_prefix(KEEP_MARKER)?;
    let field = |name: &str| {
        content
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(' '))
    };
    let too_old = field("time")
        .and_then(|time| time.parse::<u64>().ok())
        .is_none_or(|time| now_seconds().saturating_sub(time) > STALE_MARKER_SECONDS);
    let process_is_gone = cfg!(target_os = "linux")
        && field("host").is_some_and(|host| Some(host) == host_name().as_deref())
        && field("pid")
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_some_and(|pid| !Path::new("/proc").join(pid.to_string()).exists());
    Some(too_old || process_is_gone)
}

/// What to put into the new pack, and what to do with the packs it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Scope {
    /// Pack all reachable objects that are loose or in a pack without `.keep` file into one pack, like `git repack -a`.
    #[default]
    All,
    /// Pack the reachable loose objects that are in no pack, like `git repack`, and leave all packs alone.
    Incremental,
}

/// What to do with the objects of the removed packs that aren't reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Unreachable {
    /// Write them as loose objects, with the time of the pack they were in, like `git repack -A`.
    #[default]
    Loosen,
    /// Remove them with their pack, like `git repack -a`, and lose what a writer makes reachable after the roots were
    /// found. Don't use this while others write.
    Drop,
}

/// Options for [`Repository::repack()`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// What to pack.
    pub scope: Scope,
    /// What to do with unreachable objects of packs that are removed.
    pub unreachable: Unreachable,
    /// If `true`, which is the default like `-d`, remove the packs that the new pack replaces and the loose objects it has.
    pub delete_redundant: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            scope: Scope::default(),
            unreachable: Unreachable::default(),
            delete_redundant: true,
        }
    }
}

/// A step that was completed, which [`Repository::repack_observed()`] reports and can end the operation after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Phase<'a> {
    /// The new pack at the given path is in place, and nothing was removed.
    PackWritten(&'a Path),
    /// Unreachable objects were written loose, and flushed. The number is how many there were.
    Loosened(usize),
    /// A multi-pack-index, or a commit-graph that names objects that are dropped, was removed.
    DerivedFilesRemoved,
    /// The pack at the path was removed.
    PackRemoved(&'a Path),
    /// Loose objects that the new pack has were removed. The number is how many there were.
    LooseRemoved(usize),
}

/// What [`Repository::repack()`] did.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Outcome {
    /// The new pack, if one was written. It is the path of the `.pack` file.
    pub pack: Option<PathBuf>,
    /// The number of objects in the new pack.
    pub packed: usize,
    /// The number of unreachable objects that were written loose, or whose time was raised to that of their pack.
    pub loosened: usize,
    /// The packs that were removed, as the paths of their `.pack` files.
    pub removed_packs: Vec<PathBuf>,
    /// The packs that stayed although the new pack has their reachable objects, because they changed or got a `.keep`
    /// file in the meantime.
    pub changed_packs: Vec<PathBuf>,
    /// The number of loose objects that were removed as the new pack has them.
    pub removed_loose: usize,
    /// Things that couldn't be removed, with the reason. They are left for the next run.
    pub failed: Vec<(PathBuf, gix_error::Error)>,
}

/// A pack as it was when the list of packs was made.
struct ListedPack {
    stem: PathBuf,
    name: String,
    kept: bool,
    mtime: i64,
    size: u64,
}

impl ListedPack {
    fn with_extension(&self, extension: &str) -> PathBuf {
        let mut path = self.stem.clone().into_os_string();
        path.push(".");
        path.push(extension);
        path.into()
    }

    /// Return `true` if the pack is still the one that was listed.
    fn is_unchanged(&self) -> bool {
        std::fs::metadata(self.with_extension("pack"))
            .is_ok_and(|metadata| metadata.len() == self.size && mtime_of(&metadata) == self.mtime)
            && !self.with_extension("keep").exists()
    }

    /// The objects with their times, which are those of the pack, or of each object for a cruft pack.
    fn objects(&self, hash: gix_hash::Kind) -> Result<Vec<(ObjectId, i64)>> {
        let index_path = self.with_extension("idx");
        let index = gix_pack::index::File::at(&index_path, hash)
            .or_raise(|| message!("Could not read the pack index {}", index_path.display()))?;
        let times = match std::fs::read(self.with_extension("mtimes")) {
            Ok(data) => Some(
                cruft_mtimes(&data, hash, index.num_objects() as usize)
                    .or_raise(|| message!("Could not read the object times of {}", self.name))?,
            ),
            Err(_) => None,
        };
        Ok(index
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                (
                    entry.oid,
                    times.as_ref().map_or(self.mtime, |times| i64::from(times[idx])),
                )
            })
            .collect())
    }
}

/// List the packs of this repository, which are the ones with an index and a pack file.
fn local_packs(pack_dir: &Path) -> Result<Vec<ListedPack>> {
    let entries = match std::fs::read_dir(pack_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => bail!(err.and_raise(message!("Could not read {}", pack_dir.display()))),
    };
    let mut packs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("idx") {
            continue;
        }
        let stem = path.with_extension("");
        let Ok(metadata) = std::fs::metadata(stem.with_extension("pack")) else {
            continue;
        };
        packs.push(ListedPack {
            name: stem
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            kept: stem.with_extension("keep").exists(),
            mtime: mtime_of(&metadata),
            size: metadata.len(),
            stem,
        });
    }
    packs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packs)
}

/// Remove the `.keep` files that an interrupted run of this module left, and fail if there is one of a run that may still be going.
fn remove_stale_markers(pack_dir: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(pack_dir) else {
        return Ok(());
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.extension().and_then(|ext| ext.to_str()) != Some("keep") {
            continue;
        }
        match std::fs::read(&path).ok().as_deref().and_then(marker_is_stale) {
            Some(true) => {
                std::fs::remove_file(&path).ok();
            }
            Some(false) => bail!(message!(
                "Another repack seems to be running, as {} says, so this one wasn't started",
                path.display()
            )),
            None => {}
        }
    }
    Ok(())
}

/// Remove the multi-pack-index with its bitmap, reverse index and chain, as Git does when it removes packs. Return `true` if
/// anything was removed.
fn remove_multi_pack_index(pack_dir: &Path) -> Result<bool> {
    let mut removed = false;
    let entries = match std::fs::read_dir(pack_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => bail!(err.and_raise(message!("Could not read {}", pack_dir.display()))),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_file = name == "multi-pack-index"
            || (name.starts_with("multi-pack-index-") && (name.ends_with(".bitmap") || name.ends_with(".rev")));
        let result = if is_file {
            std::fs::remove_file(entry.path())
        } else if name == "multi-pack-index.d" {
            std::fs::remove_dir_all(entry.path())
        } else {
            continue;
        };
        match result {
            Ok(()) => removed = true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => bail!(err.and_raise(message!(
                "Could not remove {}, which names packs that are to be removed, so nothing was",
                entry.path().display()
            ))),
        }
    }
    Ok(removed)
}

/// Set the time at which the file at `path` was last modified.
fn set_mtime(path: &Path, seconds: i64) -> std::io::Result<()> {
    let time = UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).unwrap_or(0));
    // Only Windows needs to be allowed to write to change a time, while the files of objects don't allow it elsewhere.
    std::fs::OpenOptions::new()
        .read(true)
        .write(cfg!(windows))
        .open(path)?
        .set_modified(time)
}

/// Flush the directory at `path` so that what was created or removed in it stays that way.
fn flush_directory(path: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(path) {
        dir.sync_all().ok();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn loose_path(objects_dir: &Path, id: &ObjectId) -> PathBuf {
    let hex = id.to_hex().to_string();
    objects_dir.join(&hex[..2]).join(&hex[2..])
}

/// A directory for what is made on the way to a pack, which is removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(pack_dir: &Path) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.subsec_nanos());
        // It starts with `tmp_` so that pruning removes what an interrupted run left, once it's old enough.
        let path = pack_dir.join(format!("tmp_repack_{}_{nanos}", std::process::id()));
        std::fs::create_dir(&path).or_raise(|| message!("Could not create {}", path.display()))?;
        Ok(Scratch(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A pack that was written.
struct NewPack {
    stem: PathBuf,
    /// `true` if this run marked the pack with a `.keep` file that it has to remove, which isn't the case if the pack was there
    /// already under the same name, or if somebody else's `.keep` file was there, which stays.
    marked: bool,
}

impl Repository {
    /// Put objects into a pack and remove the packs it replaces, as the [module documentation](crate::maintenance::repack)
    /// describes. It must not run at the same time as another `repack`.
    pub fn repack(&self, options: Options, should_interrupt: &AtomicBool) -> Result<Outcome> {
        self.repack_observed(options, should_interrupt, &mut |_| Ok(()))
    }

    /// Like [`repack()`](Self::repack()), but call `observer` after each step with the step that was completed. If the
    /// observer returns an error, the operation ends with it right there, leaving what was done, which is safe at any point.
    pub fn repack_observed(
        &self,
        options: Options,
        should_interrupt: &AtomicBool,
        observer: &mut dyn FnMut(Phase<'_>) -> Result<()>,
    ) -> Result<Outcome> {
        let interrupted = || {
            if should_interrupt.load(std::sync::atomic::Ordering::Relaxed) {
                bail!(gix_error::cancelled("The repack was interrupted"));
            }
            Ok::<_, gix_error::Error>(())
        };
        self.refuse_to_delete_objects()?;
        let objects_dir = self.objects.store_ref().path().to_owned();
        let pack_dir = objects_dir.join("pack");
        let hash = self.object_hash();
        let fsync = {
            let mut fsync = self.config.fsync_options()?;
            // What's written before something is removed has to survive, which is a stronger demand than the configuration has.
            fsync.components = fsync.components
                | gix_fs::fsync::Components::LOOSE_OBJECT
                | gix_fs::fsync::Components::PACK
                | gix_fs::fsync::Components::PACK_METADATA;
            fsync
        };
        let mut outcome = Outcome::default();

        remove_stale_markers(&pack_dir)?;
        // 1. The packs that may be replaced, before the roots are looked at.
        let packs = local_packs(&pack_dir)?;
        let (kept, replaceable): (Vec<_>, Vec<_>) = packs.into_iter().partition(|pack| pack.kept);
        let replaceable = if options.scope == Scope::All {
            replaceable
        } else {
            Vec::new()
        };

        // 2. What is wanted.
        let roots = self.reachability_roots(Default::default())?;
        let reachable = self.reachable_objects(roots)?;
        interrupted()?;

        // The objects that are in packs which stay.
        let mut in_stay: HashSet<ObjectId> = HashSet::default();
        for pack in &kept {
            in_stay.extend(pack.objects(hash)?.into_iter().map(|(id, _)| id));
        }
        let (loose, _temporary) = scan_loose(&objects_dir, hash)?;
        let mut candidates: BTreeSet<ObjectId> = BTreeSet::new();
        // The objects that the packs to be removed have, with the newest time of the packs that have them.
        let mut in_replaced: HashMap<ObjectId, i64> = HashMap::default();
        for pack in &replaceable {
            for (id, time) in pack.objects(hash)? {
                let newest = in_replaced.entry(id).or_insert(time);
                *newest = (*newest).max(time);
            }
        }
        match options.scope {
            Scope::All => {
                candidates.extend(
                    in_replaced
                        .keys()
                        .filter(|id| reachable.contains(id) && !in_stay.contains(*id))
                        .copied(),
                );
                candidates.extend(
                    loose
                        .iter()
                        .map(|object| object.id)
                        .filter(|id| reachable.contains(id) && !in_stay.contains(id)),
                );
            }
            Scope::Incremental => {
                // Objects of any pack are not loose ones that this is for.
                let mut in_any_pack = in_stay.clone();
                for pack in local_packs(&pack_dir)? {
                    in_any_pack.extend(pack.objects(hash)?.into_iter().map(|(id, _)| id));
                }
                candidates.extend(
                    loose
                        .iter()
                        .map(|object| object.id)
                        .filter(|id| reachable.contains(id) && !in_any_pack.contains(id)),
                );
            }
        }

        // 3. The new pack.
        let new_pack = if candidates.is_empty() {
            None
        } else {
            let ids: Vec<ObjectId> = candidates.iter().copied().collect();
            Some(self.write_new_pack(&ids, &pack_dir, &fsync, should_interrupt)?)
        };
        let new_pack_name = new_pack.as_ref().map(|pack| {
            pack.stem
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        if let Some(pack) = &new_pack {
            outcome.pack = Some(pack.stem.with_extension("pack"));
            outcome.packed = candidates.len();
            observer(Phase::PackWritten(&pack.stem.with_extension("pack")))?;
        }
        if !options.delete_redundant {
            self.finish_repack(&mut outcome, new_pack.as_ref(), &pack_dir)?;
            return Ok(outcome);
        }
        interrupted()?;

        // Objects of the new pack that are loose as well, which is determined from what is on disk and not from what
        // this handle has seen, as it may have packs mapped that are removed below.
        let in_new_pack: HashSet<ObjectId> = match &new_pack {
            Some(pack) => {
                let index_path = pack.stem.with_extension("idx");
                gix_pack::index::File::at(&index_path, hash)
                    .or_raise(|| message!("Could not read the pack index {}", index_path.display()))?
                    .iter()
                    .map(|entry| entry.oid)
                    .collect()
            }
            None => HashSet::default(),
        };

        // 4. What isn't reachable, and wouldn't survive the removal of its pack.
        let to_loosen: Vec<(ObjectId, i64)> = in_replaced
            .iter()
            .filter(|(id, _)| !in_new_pack.contains(*id) && !in_stay.contains(*id))
            .map(|(id, time)| (*id, *time))
            .collect();
        if options.unreachable == Unreachable::Loosen && !replaceable.is_empty() {
            let count = self.loosen(&objects_dir, &to_loosen, &fsync, should_interrupt)?;
            outcome.loosened = count;
            observer(Phase::Loosened(count))?;
        }
        interrupted()?;

        // 5. What names objects or packs that are to go.
        let removable: Vec<&ListedPack> = replaceable
            .iter()
            .filter(|pack| Some(&pack.name) != new_pack_name.as_ref())
            .collect();
        if !removable.is_empty() {
            let mut removed = remove_multi_pack_index(&pack_dir)?;
            if options.unreachable == Unreachable::Drop {
                removed |= self.remove_commit_graph_naming(to_loosen.iter().map(|(id, _)| *id))?;
            }
            if removed {
                observer(Phase::DerivedFilesRemoved)?;
            }
        }

        // 6. The packs.
        for pack in removable {
            if !pack.is_unchanged() {
                outcome.changed_packs.push(pack.with_extension("pack"));
                continue;
            }
            let mut failed = false;
            // The index first, so that nobody finds a pack that is on its way out.
            for extension in ["idx", "pack", "rev", "bitmap", "mtimes"] {
                let path = pack.with_extension(extension);
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        outcome.failed.push((
                            path,
                            err.and_raise(message("Could not remove a pack that was replaced")),
                        ));
                        failed = true;
                        break;
                    }
                }
            }
            if !failed {
                let path = pack.with_extension("pack");
                observer(Phase::PackRemoved(&path))?;
                outcome.removed_packs.push(path);
            }
        }
        flush_directory(&pack_dir);

        // 7. The loose objects that the new pack has.
        let mut removed_loose = 0;
        for object in &loose {
            if in_new_pack.contains(&object.id) && std::fs::remove_file(&object.path).is_ok() {
                removed_loose += 1;
            }
        }
        remove_empty_fan_out_dirs(&objects_dir);
        outcome.removed_loose = removed_loose;
        observer(Phase::LooseRemoved(removed_loose))?;
        self.finish_repack(&mut outcome, new_pack.as_ref(), &pack_dir)?;
        Ok(outcome)
    }

    /// Remove the `.keep` file of the new pack, as the work is done.
    fn finish_repack(&self, outcome: &mut Outcome, new_pack: Option<&NewPack>, pack_dir: &Path) -> Result<()> {
        if let Some(pack) = new_pack.filter(|pack| pack.marked) {
            let keep = pack.stem.with_extension("keep");
            if let Err(err) = std::fs::remove_file(&keep)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                outcome.failed.push((
                    keep,
                    err.and_raise(message("Could not remove the marker of the new pack")),
                ));
            }
            flush_directory(pack_dir);
        }
        Ok(())
    }

    /// Write a pack with `ids` into `pack_dir`, with a `.keep` file that the caller removes.
    fn write_new_pack(
        &self,
        ids: &[ObjectId],
        pack_dir: &Path,
        fsync: &gix_fs::fsync::Options,
        should_interrupt: &AtomicBool,
    ) -> Result<NewPack> {
        let hash = self.object_hash();
        let scratch = Scratch::new(pack_dir)?;
        let mut db = self
            .objects
            .clone()
            .into_arc()
            .or_raise(|| message("Could not prepare the object database for reading"))?
            .into_inner();
        db.prevent_pack_unload();
        db.ignore_replacements = true;
        let (counts, _) = gix_pack::data::output::count::objects_unthreaded(
            &db,
            &mut ids.iter().map(|id| Ok(*id)),
            &gix_utils::progress::Discard,
            should_interrupt,
            gix_pack::data::output::count::objects::ObjectExpansion::AsIs,
        )
        .or_raise(|| message("Could not find the objects that are to be packed, so nothing was repacked"))?;

        let stream_path = scratch.0.join("stream");
        {
            let mut stream = BufWriter::new(
                std::fs::File::create_new(&stream_path)
                    .or_raise(|| message!("Could not create {}", stream_path.display()))?,
            );
            gix_pack::data::output::missing::write_pack(
                db,
                counts,
                &mut stream,
                hash,
                gix_pack::data::output::missing::Options {
                    compression: self.pack_compression()?,
                    ..Default::default()
                },
            )
            .or_raise(|| message("Could not write the objects into a pack, so nothing was repacked"))?;
            stream
                .flush()
                .or_raise(|| message!("Could not write {}", stream_path.display()))?;
        }
        let mut reader = BufReader::new(
            std::fs::File::open(&stream_path).or_raise(|| message!("Could not read {}", stream_path.display()))?,
        );
        let bundle = gix_pack::Bundle::write_to_directory(
            &mut reader,
            Some(&scratch.0),
            &mut gix_utils::progress::Discard,
            should_interrupt,
            None::<gix_object::find::Never>,
            hash,
            gix_pack::bundle::write::Options {
                thread_limit: index_threads(self)?,
                index_version: pack_index_version(self)?,
                iteration_mode: gix_pack::data::input::Mode::Verify,
                alloc_limit_bytes: self.config.alloc_limit_bytes,
                compression: self.config.loose_compression,
                fsync: *fsync,
            },
        )
        .or_raise(|| message("Could not index the new pack, so nothing was repacked"))?;
        let (Some(data_path), Some(index_path)) = (bundle.data_path, bundle.index_path) else {
            bail!(message(
                "The new pack has no objects, which can't be as there are some to pack"
            ));
        };
        let file_name = data_path.file_name().expect("a pack has a file name").to_owned();
        let stem = pack_dir.join(&file_name).with_extension("");
        if stem.with_extension("idx").is_file() {
            // The same objects in the same order have the same name, which is that of a pack that is there.
            return Ok(NewPack { stem, marked: false });
        }
        // What a pack of the same name had that this one doesn't is wrong for it: a `.mtimes` file, for one, makes it a cruft pack.
        for extension in ["rev", "bitmap", "mtimes", "promisor"] {
            let path = stem.with_extension(extension);
            match std::fs::remove_file(&path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    return Err(err.and_raise(message!(
                        "Could not remove {}, which the new pack must not have",
                        path.display()
                    )));
                }
                _ => {}
            }
        }
        // It's marked before it can be found, so that a repack by somebody else leaves it alone while this runs, and so that this
        // knows it if it's left behind. A `.keep` file that is there already protects the pack for somebody else, say a receiver
        // that has yet to install the same pack and update its references, and stays as it is.
        let marked = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stem.with_extension("keep"))
        {
            Ok(mut file) => {
                std::io::Write::write_all(&mut file, marker_content().as_bytes())
                    .or_raise(|| message("Could not write the marker of the new pack"))?;
                true
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(err) => return Err(err.and_raise(message("Could not create the marker of the new pack"))),
        };
        std::fs::rename(&data_path, stem.with_extension("pack"))
            .or_raise(|| message("Could not move the new pack into place"))?;
        std::fs::rename(&index_path, stem.with_extension("idx"))
            .or_raise(|| message("Could not move the index of the new pack into place"))?;
        flush_directory(pack_dir);
        Ok(NewPack { stem, marked })
    }

    /// Write the objects, each one with its time, unless they are loose already, which raises the time if they are older.
    /// Return the number of objects that were written or touched, once all are on disk.
    fn loosen(
        &self,
        objects_dir: &Path,
        objects: &[(ObjectId, i64)],
        fsync: &gix_fs::fsync::Options,
        should_interrupt: &AtomicBool,
    ) -> Result<usize> {
        let store = gix_odb::loose::Store::at_opts(
            objects_dir,
            self.object_hash(),
            gix_odb::loose::Options {
                alloc_limit_bytes: self.config.alloc_limit_bytes,
                compression: self.config.loose_compression,
                fsync: *fsync,
            },
        );
        let mut repo = self.clone();
        repo.objects.ignore_replacements = true;
        let mut buf = Vec::new();
        let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let mut count = 0;
        for (id, time) in objects {
            if should_interrupt.load(std::sync::atomic::Ordering::Relaxed) {
                bail!(gix_error::cancelled("The repack was interrupted"));
            }
            let path = loose_path(objects_dir, id);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if mtime_of(&metadata) < *time {
                        set_mtime(&path, *time)
                            .or_raise(|| message!("Could not set the time of the object at {}", path.display()))?;
                        count += 1;
                    }
                    continue;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => bail!(err.and_raise(message!("Could not look at {}", path.display()))),
            }
            let Some(object) = repo
                .objects
                .try_find(id, &mut buf)
                .or_raise(|| message!("Could not read {id} to write it loose"))?
            else {
                bail!(message!(
                    "The object {id} is in a pack that is to be removed, but can't be read, so nothing was removed"
                ));
            };
            use gix_object::Write as _;
            store
                .write_buf(object.kind, object.data)
                .or_raise(|| message!("Could not write {id} loose"))?;
            set_mtime(&path, *time)
                .or_raise(|| message!("Could not set the time of the object at {}", path.display()))?;
            if let Some(dir) = path.parent() {
                dirs.insert(dir.to_owned());
            }
            count += 1;
        }
        for dir in dirs {
            flush_directory(&dir);
        }
        flush_directory(objects_dir);
        Ok(count)
    }
}

fn index_threads(repo: &Repository) -> Result<Option<usize>> {
    PackKeys::THREADS
        .try_into_usize(
            repo.config
                .resolved
                .integer_filter(PackKeys::THREADS, &mut repo.filter_config_section()),
        )
        .with_leniency(repo.options.lenient_config)
        .or_raise(|| message("The configured pack thread count is invalid"))
}

fn pack_index_version(repo: &Repository) -> Result<gix_pack::index::Version> {
    Ok(PackKeys::INDEX_VERSION
        .try_into_index_version(repo.config.resolved.integer(PackKeys::INDEX_VERSION))
        .with_leniency(repo.options.lenient_config)
        .or_raise(|| message("The configured pack index version is invalid"))?
        .unwrap_or(gix_pack::index::Version::V2))
}
