//! Removing objects that nothing wants any more, like `git prune` and `git prune-packed` do.
//!
//! Only loose objects of this repository are removed, never objects in packs and never those of an alternate. A loose object stays
//! if it's reachable (see [`Repository::reachability_roots()`]) or recent. A recent object is one that's younger than the expiry
//! time, or that `gc.recentObjectsHook` names, and everything it leads to stays as well, however old that is. This includes the
//! objects of a pack that's younger than the expiry time, and those of a cruft pack that its `.mtimes` file says are younger, because an
//! object that a recent one needs may well be loose and old.
//!
//! Temporary files and directories that a writer left behind stay until they're older than the expiry time. These are `tmp_*` in
//! `objects/` and `objects/pack/` and `tmp_obj_*` in the directories of loose objects, which includes the quarantine directory of a
//! push that never finished. The `shallow` file loses the commits that aren't reachable any more.
//!
//! Nothing is removed from a repository with `extensions.preciousObjects`, or from one with promisor packs, where a missing object may
//! be fetched later.
//!
//! # Deviations from Git
//!
//! * `gc.recentObjectsHook` runs only in a repository that is fully trusted, and is an error otherwise.
//! * A commit-graph that has a commit this removes is removed first, so that no file names an object that is gone. Git leaves it.
use std::{
    io::BufRead,
    path::{Path, PathBuf},
};

use gix_error::{ErrorExt, Result, ResultExt, bail, message, unsupported};
use gix_hash::ObjectId;
use gix_hashtable::HashSet;
use gix_odb::Header as _;

use crate::{
    Repository,
    maintenance::reachable::{Origin, Reachable, Root},
};

/// Options for [`Repository::prune()`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Options {
    /// Objects and temporary files that are not newer than this can go, which is what `gc.pruneExpire` is for.
    ///
    /// If `None`, everything that isn't wanted goes at once, as with `git prune` without `--expire`. That includes the temporary
    /// files and directories of writers that are still busy, like the quarantine of a push that is being received, so a caller
    /// that runs while others write should always pass a time.
    pub expire: Option<gix_date::Time>,
    /// If `true`, find out what would be removed and report that, but change nothing.
    pub dry_run: bool,
}

/// What pruning did, or, with [`dry_run`](Options::dry_run), would do.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Outcome {
    /// The loose objects that were unreachable and old enough, which were removed.
    pub removed: Vec<ObjectId>,
    /// The number of loose objects that were removed because they are in a pack.
    pub removed_as_packed: usize,
    /// The number of temporary files and directories that were removed.
    pub temporary: usize,
    /// The number of commits that were removed from the shallow file.
    pub unshallowed: usize,
    /// If `true`, a commit-graph that named removed commits was removed.
    pub commit_graph_removed: bool,
    /// Things that couldn't be removed, with the reason, which are left for next time.
    pub failed: Vec<(PathBuf, gix_error::Error)>,
}

/// Options for [`Repository::prune_packed()`].
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct PackedOptions {
    /// If `true`, find out what would be removed and report that, but change nothing.
    pub dry_run: bool,
}

pub(super) struct Loose {
    pub(super) id: ObjectId,
    pub(super) path: PathBuf,
    pub(super) mtime: i64,
}

pub(super) fn mtime_of(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

/// Return `true` if the file at `path` is there and not newer than `expire`, according to the file system now.
fn is_still_old(path: &Path, expire: i64) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| mtime_of(&metadata) <= expire)
}

/// Everything that is in the directories of loose objects: what is an object, and what is a leftover of a writer.
pub(super) fn scan_loose(objects_dir: &Path, hash: gix_hash::Kind) -> Result<(Vec<Loose>, Vec<PathBuf>)> {
    let (mut loose, mut temporary) = (Vec::new(), Vec::new());
    for fan_out in 0..=255u8 {
        let dir = objects_dir.join(format!("{fan_out:02x}"));
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => bail!(err.and_raise(message!("Could not read {}", dir.display()))),
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("tmp_obj_") {
                temporary.push(entry.path());
                continue;
            }
            if name.len() != hash.len_in_hex() - 2 {
                continue;
            }
            let Ok(id) = ObjectId::from_hex(format!("{fan_out:02x}{name}").as_bytes()) else {
                continue;
            };
            let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            loose.push(Loose {
                id,
                path: entry.path(),
                mtime: mtime_of(&metadata),
            });
        }
    }
    Ok((loose, temporary))
}

/// The objects of the packs of this repository that are younger than `expire`, which takes a pack of the age of its file, or
/// the age of each object for a cruft pack.
fn recent_in_packs(objects_dir: &Path, hash: gix_hash::Kind, expire: i64) -> Result<Vec<ObjectId>> {
    let pack_dir = objects_dir.join("pack");
    let entries = match std::fs::read_dir(&pack_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => bail!(err.and_raise(message!("Could not read {}", pack_dir.display()))),
    };
    let mut out = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("idx") {
            continue;
        }
        let pack_path = path.with_extension("pack");
        let Ok(pack_metadata) = std::fs::metadata(&pack_path) else {
            continue;
        };
        let mtimes = std::fs::read(path.with_extension("mtimes")).ok();
        let pack_mtime = mtime_of(&pack_metadata);
        if mtimes.is_none() && pack_mtime <= expire {
            continue;
        }
        let index = gix_pack::index::File::at(&path, hash)
            .or_raise(|| message!("Could not read the pack index {}", path.display()))?;
        match mtimes {
            None => out.extend(index.iter().map(|entry| entry.oid)),
            Some(mtimes) => {
                let per_object = cruft_mtimes(&mtimes, hash, index.num_objects() as usize)
                    .or_raise(|| message!("Could not read the object mtimes of {}", path.display()))?;
                out.extend(
                    index
                        .iter()
                        .zip(per_object)
                        .filter(|(_, mtime)| i64::from(*mtime) > expire)
                        .map(|(entry, _)| entry.oid),
                );
            }
        }
    }
    Ok(out)
}

/// Read the times of the objects of a cruft pack, in the order of its index, from the `.mtimes` file `data`.
pub(super) fn cruft_mtimes(data: &[u8], hash: gix_hash::Kind, num_objects: usize) -> Result<Vec<u32>> {
    const HEADER: usize = 12;
    let id_len = hash.len_in_bytes();
    let expected_hash_version: u32 = if id_len == 20 { 1 } else { 2 };
    if data.len() < HEADER + num_objects * 4 + 2 * id_len || &data[..4] != b"MTME" {
        bail!(message("The file isn't a valid .mtimes file"));
    }
    let word = |at: usize| u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
    if word(4) != 1 || word(8) != expected_hash_version {
        bail!(message("The .mtimes file has an unknown version or hash"));
    }
    Ok((0..num_objects).map(|idx| word(HEADER + idx * 4)).collect())
}

/// Run the programs of `gc.recentObjectsHook` and return the ids they print.
fn recent_objects_from_hooks(repo: &Repository) -> Result<Vec<ObjectId>> {
    let config = repo.config_snapshot();
    let programs = config.plumbing().strings("gc.recentObjectsHook").unwrap_or_default();
    if programs.is_empty() {
        return Ok(Vec::new());
    }
    if repo.git_dir_trust() != gix_sec::Trust::Full {
        bail!(unsupported(
            "gc.recentObjectsHook is configured, but programs are only run in repositories that are fully trusted"
        ));
    }
    let mut out = Vec::new();
    for program in programs {
        let program = gix_path::from_byte_slice(&program)
            .or_raise(|| message("gc.recentObjectsHook isn't valid UTF-8"))?
            .to_owned();
        let mut child = gix_command::prepare(program.clone())
            .with_shell()
            .stdout(std::process::Stdio::piped())
            .spawn()
            .or_raise(|| message!("Could not run gc.recentObjectsHook {}", program.display()))?;
        let stdout = child.stdout.take().expect("stdout was configured to be piped");
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.or_raise(|| message("Could not read the output of gc.recentObjectsHook"))?;
            let id = ObjectId::from_hex(line.trim().as_bytes())
                .or_raise(|| message!("invalid extra cruft tip: '{line}'"))?;
            out.push(id);
        }
        let status = child
            .wait()
            .or_raise(|| message("Could not wait for gc.recentObjectsHook"))?;
        if !status.success() {
            bail!(message("unable to enumerate additional recent objects"));
        }
    }
    Ok(out)
}

/// Remove the directories for loose objects that are empty, which `rmdir` leaves alone if they are not.
pub(super) fn remove_empty_fan_out_dirs(objects_dir: &Path) {
    for fan_out in 0..=255u8 {
        std::fs::remove_dir(objects_dir.join(format!("{fan_out:02x}"))).ok();
    }
}

impl Repository {
    /// Return all objects that are reachable, or that are recent, which includes what they lead to, where recent means newer than
    /// `expire` in seconds, or named by `gc.recentObjectsHook`.
    pub(super) fn reachable_with_recent(&self, objects_dir: &Path, loose: &[Loose], expire: i64) -> Result<Reachable> {
        let mut roots = self.reachability_roots(Default::default())?;
        let mut recent: Vec<ObjectId> = loose.iter().filter(|l| l.mtime > expire).map(|l| l.id).collect();
        recent.extend(recent_in_packs(objects_dir, self.object_hash(), expire)?);
        recent.extend(recent_objects_from_hooks(self)?);
        roots.extend(recent.into_iter().map(|id| Root {
            id,
            origin: Origin::Recent,
            kind: None,
        }));
        self.reachable_objects(roots)
    }

    pub(super) fn refuse_to_delete_objects(&self) -> Result<()> {
        if self
            .config_snapshot()
            .boolean("extensions.preciousObjects")
            .unwrap_or(false)
        {
            bail!(unsupported("cannot prune in a precious-objects repo"));
        }
        let pack_dir = self.objects.store_ref().path().join("pack");
        let is_promisor = std::fs::read_dir(pack_dir).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|entry| entry.path().extension().is_some_and(|ext| ext == "promisor"))
        });
        if is_promisor {
            bail!(unsupported(
                "the repository has promisor packs, which means that objects that are missing may be fetched later"
            ));
        }
        Ok(())
    }

    /// Remove the loose objects that nothing wants any more, the way `git prune` does, along with the temporary files that writers
    /// left, and loose objects that are in a pack. See the [module documentation](crate::maintenance::prune) for what is kept
    /// and what differs from Git.
    pub fn prune(&self, options: Options) -> Result<Outcome> {
        self.refuse_to_delete_objects()?;
        let expire = options.expire.map_or(i64::MAX, |time| time.seconds);
        let objects_dir = self.objects.store_ref().path().to_owned();
        let hash = self.object_hash();
        let mut outcome = Outcome::default();

        let (loose, leftovers) = scan_loose(&objects_dir, hash)?;
        let reachable = self.reachable_with_recent(&objects_dir, &loose, expire)?;

        let unreachable: Vec<&Loose> = loose
            .iter()
            .filter(|l| !reachable.contains(&l.id) && l.mtime <= expire)
            .collect();
        if !options.dry_run {
            // A cache that names a commit that is removed must not stay, and an interrupted run must not leave that behind.
            outcome.commit_graph_removed = self.remove_commit_graph_naming(unreachable.iter().map(|l| l.id))?;
        } else {
            outcome.commit_graph_removed = self.commit_graph_names(unreachable.iter().map(|l| l.id));
        }
        for object in unreachable {
            // What was seen before the walk isn't what is there now: a writer may have freshened the object since, and be about to
            // refer to it.
            if !is_still_old(&object.path, expire) {
                continue;
            }
            if !options.dry_run
                && let Err(err) = std::fs::remove_file(&object.path)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                outcome.failed.push((
                    object.path.clone(),
                    err.and_raise(message("Could not remove an object")),
                ));
                continue;
            }
            outcome.removed.push(object.id);
        }

        outcome.removed_as_packed = self.prune_packed_inner(options.dry_run, &mut outcome.failed)?;

        let mut temporary = leftovers;
        for dir in [objects_dir.clone(), objects_dir.join("pack")] {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                temporary.extend(
                    entries
                        .flatten()
                        .filter(|entry| entry.file_name().to_string_lossy().starts_with("tmp_"))
                        .map(|entry| entry.path()),
                );
            }
        }
        for path in temporary {
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if mtime_of(&metadata) > expire {
                continue;
            }
            if !options.dry_run {
                let result = if metadata.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                if let Err(err) = result {
                    outcome
                        .failed
                        .push((path, err.and_raise(message("Could not remove a temporary file"))));
                    continue;
                }
            }
            outcome.temporary += 1;
        }

        if !options.dry_run {
            remove_empty_fan_out_dirs(&objects_dir);
        }

        match self.prune_shallow(&reachable.iter().copied().collect(), options.dry_run) {
            Ok(count) => outcome.unshallowed = count,
            // What was removed already stays that way, and the next run can try again.
            Err(err) => outcome.failed.push((self.shallow_file().unwrap_or_default(), err)),
        }
        Ok(outcome)
    }

    /// Remove the loose objects that are in a pack as well, like `git prune-packed` does, and return how many there were.
    pub fn prune_packed(&self, options: PackedOptions) -> Result<usize> {
        self.refuse_to_delete_objects()?;
        let mut failed = Vec::new();
        self.prune_packed_inner(options.dry_run, &mut failed)
    }

    fn prune_packed_inner(&self, dry_run: bool, failed: &mut Vec<(PathBuf, gix_error::Error)>) -> Result<usize> {
        let objects_dir = self.objects.store_ref().path().to_owned();
        let (loose, _) = scan_loose(&objects_dir, self.object_hash())?;
        // A handle that hasn't loaded any pack index yet finds a loose object first, as it only looks for a pack once the
        // loose database doesn't have the object.
        // Replacements are not to be followed, as the object with the replaced id is not in a pack just because its replacement is.
        let mut repo = self.clone();
        repo.objects.ignore_replacements = true;
        repo.objects
            .packed_object_count()
            .or_raise(|| message("Could not load the pack indices"))?;
        let mut count = 0;
        for object in loose {
            // Packs are looked at before loose objects, so a loose one is found only if there isn't a pack that has it.
            let in_pack = matches!(
                repo.objects.try_header(&object.id),
                Ok(Some(gix_odb::find::Header::Packed(_)))
            );
            if !in_pack {
                continue;
            }
            if !dry_run && let Err(err) = std::fs::remove_file(&object.path) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    failed.push((
                        object.path,
                        err.and_raise(message("Could not remove an object that is in a pack")),
                    ));
                }
                continue;
            }
            count += 1;
        }
        if !dry_run {
            remove_empty_fan_out_dirs(&objects_dir);
        }
        Ok(count)
    }

    /// Return `true` if the commit-graph has any of `ids`.
    fn commit_graph_names(&self, ids: impl Iterator<Item = ObjectId>) -> bool {
        let info = self.objects.store_ref().path().join("info");
        let Ok(graph) = gix_commitgraph::at(&info, self.object_hash(), None) else {
            return false;
        };
        let mut ids = ids;
        ids.any(|id| graph.lookup(id).is_some())
    }

    /// Remove the commit-graph if it names any of `ids`, and return `true` if it was removed. It's an error if it can't be removed,
    /// as the objects must not be removed then.
    pub(super) fn remove_commit_graph_naming(&self, ids: impl Iterator<Item = ObjectId>) -> Result<bool> {
        if !self.commit_graph_names(ids) {
            return Ok(false);
        }
        let info = self.objects.store_ref().path().join("info");
        for (path, is_dir) in [(info.join("commit-graph"), false), (info.join("commit-graphs"), true)] {
            let result = if is_dir {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match result {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => bail!(err.and_raise(message!(
                    "Could not remove {}, which has commits that are to be removed, so nothing was",
                    path.display()
                ))),
            }
        }
        Ok(true)
    }

    /// Remove the commits from the shallow file that aren't reachable, and return how many there were.
    fn prune_shallow(&self, reachable: &HashSet<ObjectId>, dry_run: bool) -> Result<usize> {
        let Some(commits) = self.shallow_commits()? else {
            return Ok(0);
        };
        let gone: Vec<ObjectId> = commits.iter().copied().filter(|id| !reachable.contains(id)).collect();
        if gone.is_empty() || dry_run {
            return Ok(gone.len());
        }
        let file = self.shallow_file()?;
        let lock = gix_lock::File::acquire_to_update_resource(&file, gix_lock::acquire::Fail::Immediately, None, 0)
            .or_raise(|| message("Could not lock the shallow file"))?;
        let updates: Vec<_> = gone.iter().map(|id| gix_shallow::Update::Unshallow(*id)).collect();
        gix_shallow::write(lock, gix_shallow::read(&file)?, &updates)?;
        Ok(gone.len())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::is_still_old;

    #[test]
    fn an_object_that_a_writer_freshened_after_it_was_seen_is_not_old_any_more() -> gix_error::TestResult {
        let dir = gix_testtools::tempfile::tempdir()?;
        let path = dir.path().join("object");
        let file = std::fs::File::create(&path)?;
        let set_age = |seconds| file.set_modified(UNIX_EPOCH + Duration::from_secs(seconds));

        set_age(1_000)?;
        assert!(is_still_old(&path, 2_000), "it is not newer than the expiry time");
        set_age(2_000)?;
        assert!(
            is_still_old(&path, 2_000),
            "being as old as the expiry time is old enough, as in Git"
        );
        set_age(3_000)?;
        assert!(
            !is_still_old(&path, 2_000),
            "once it is newer than that it must stay, even though it was old when the walk began"
        );
        std::fs::remove_file(&path)?;
        assert!(
            !is_still_old(&path, 2_000),
            "an object that is gone isn't removed again"
        );
        Ok(())
    }
}
