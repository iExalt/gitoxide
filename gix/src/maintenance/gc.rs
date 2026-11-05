//! Garbage collection like `git gc`, and the decisions that automatic maintenance makes about when it is due.
//!
//! [`Repository::gc()`] runs, in this order: `pack-refs --all --prune` and `reflog expire --all`, as the settings `gc.packRefs`
//! and `gc.reflogExpire*` allow, then repacks all reachable objects into one pack with unreachable ones going to a cruft pack
//! (`gc.cruftPacks`, on by default) or loose, and prunes what is unreachable and older than `gc.pruneExpire`, then removes
//! index files that have no pack. With [`Options::auto`] it does so only if there are too many loose objects (`gc.auto`) or packs
//! (`gc.autoPackLimit`), after the `pre-auto-gc` hook agreed, and then it packs only the loose objects unless there are too many
//! packs. It takes `gc.pid` like Git, so it never runs at the same time as another `git gc`, and `objects/maintenance.lock` like
//! `git maintenance run` does, so it doesn't run at the same time as that either.
//!
//! # Differences from Git
//!
//! * It never detaches, so what `gc.autoDetach` and `maintenance.autoDetach` ask for is left to the caller, who learns it from
//!   [`Repository::auto_maintenance()`]. A `gc.log` that a detached `git gc` left is honoured for automatic runs, but none is written.
//! * `gc.bigPackThreshold`, `--keep-largest-pack`, `--aggressive`, `gc.repackFilter` and `--expire-to` aren't supported, so a
//!   large pack is rewritten as well, which costs time and not data. Pruning worktrees, `rerere gc` and writing a commit-graph are
//!   not done, and [`Outcome::unsupported`] says so when there is something for them to do.
//! * Unreachable objects are loosened or put into a cruft pack by `repack`, which then removes what is expired, and `prune` runs
//!   after it, as in Git, for the loose ones and for temporary files.
//! * `gc.pruneExpire`, `gc.logExpiry` and the other dates are read as Git reads them, but a value that can't be understood is an error.
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use gix_error::{ErrorExt, Result, ResultExt, bail, message};

pub use super::lock::{Busy, Holder};
use super::{
    lock::{GcPid, MaintenanceLock},
    prune, reflog, repack,
};
use crate::Repository;

/// How long unreachable objects are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expire {
    /// Remove what is unreachable at once, which is `now`.
    Now,
    /// Never remove anything, which is `never`.
    Never,
    /// Remove what is not newer than this time.
    At(gix_date::Time),
}

/// Options for [`Repository::gc()`].
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct Options {
    /// If `true`, do nothing unless maintenance is due, like `git gc --auto`.
    pub auto: bool,
    /// If `true`, run even if another run seems to be going, like `git gc --force`.
    pub force: bool,
    /// How long unreachable objects are kept, instead of `gc.pruneExpire`.
    pub prune_expire: Option<Expire>,
}

/// Why `gc()` did nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Skipped {
    /// With `auto`, there aren't enough loose objects or packs, or `gc.auto` is `0`.
    NotDue,
    /// With `auto`, the `pre-auto-gc` hook said no.
    DeclinedByHook,
    /// With `auto`, a previous run that was detached left a `gc.log` that is younger than `gc.logExpiry`.
    LastRunFailed(PathBuf),
    /// With `auto`, another `gc` has `gc.pid` or holds `gc.pid.lock`, which is how Git keeps quiet about it.
    HeldBy(Busy),
    /// Another maintenance run has `objects/maintenance.lock`, which has no age limit and which `force` doesn't override, so
    /// one that a killed `git maintenance` left behind keeps this from running until it is removed.
    MaintenanceLockHeld(PathBuf),
}

/// What [`Repository::gc()`] did.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Outcome {
    /// Why nothing was done, if that's the case.
    pub skipped: Option<Skipped>,
    /// `true` if references were packed.
    pub packed_refs: bool,
    /// What expiring reflogs did, if it was done.
    pub reflogs: Option<reflog::Outcome>,
    /// What repacking did, unless it wasn't done.
    pub repack: Option<repack::Outcome>,
    /// What pruning did, unless it wasn't done.
    pub prune: Option<prune::Outcome>,
    /// The number of loose objects that were removed as they are in a pack, if the expiry is `never`.
    pub removed_as_packed: usize,
    /// The number of index files that had no pack and were removed.
    pub pack_garbage_removed: usize,
    /// With `auto`, `true` if there are still too many loose objects after repacking, which only `prune` can help with, as they are
    /// unreachable and young.
    pub too_many_unreachable_loose_objects: bool,
    /// The reason why objects weren't repacked and pruned, like a repository that has precious objects, which is not an error.
    pub objects_not_touched: Option<String>,
    /// What there was to do that this doesn't know how to do: `worktree-prune`, `rerere-gc` and `commit-graph` if there is something for
    /// them, and the settings `gc.bigPackThreshold`, `gc.maxCruftSize` and `gc.repackFilter` if they are set.
    pub unsupported: Vec<&'static str>,
}

/// Settings that decide what to do, read from the configuration like Git's `gc` does.
#[derive(Debug, Clone)]
pub(super) struct Settings {
    pub auto: i64,
    pub auto_pack_limit: i64,
    pub pack_refs: bool,
    pub prune_reflogs: bool,
    pub cruft_packs: bool,
    pub prune_expire: Expire,
    pub log_expire_seconds: i64,
    pub detach_auto: bool,
}

fn parse_expire(value: &str, key: &str, now: &gix_date::Zoned) -> Result<Expire> {
    Ok(match value {
        "never" | "false" => Expire::Never,
        "all" | "now" => Expire::Now,
        other => Expire::At(
            gix_date::parse(other, Some(now.clone())).or_raise(|| message!("Could not interpret {key} = {other}"))?,
        ),
    })
}

impl Settings {
    /// What to do with objects that are unreachable when everything is repacked, as `gc.pruneExpire` and `gc.cruftPacks` say.
    /// An expiry of `now` drops them whatever else is set, as Git checks that first.
    pub(super) fn unreachable(&self) -> repack::Unreachable {
        match (self.prune_expire, self.cruft_packs) {
            (Expire::Now, _) => repack::Unreachable::Drop,
            (Expire::Never, true) => repack::Unreachable::Cruft { expire: None },
            (Expire::At(time), true) => repack::Unreachable::Cruft { expire: Some(time) },
            (_, false) => repack::Unreachable::Loosen,
        }
    }

    pub(super) fn read(repo: &Repository, now: &gix_date::Zoned) -> Result<Self> {
        let config = repo.config_snapshot();
        let integer = |key: &str, default: i64| -> Result<i64> {
            Ok(config
                .try_integer(key)
                .or_raise(|| message!("The value of {key} isn't a number"))?
                .unwrap_or(default))
        };
        let boolean = |key: &str, default: bool| -> Result<bool> {
            Ok(config
                .try_boolean(key)
                .or_raise(|| message!("The value of {key} isn't a boolean"))?
                .unwrap_or(default))
        };
        let is_never = |key: &str| -> Result<bool> {
            Ok(match config.string(key) {
                Some(value) => {
                    let text = std::str::from_utf8(&value).or_raise(|| message!("The value of {key} isn't text"))?;
                    matches!(parse_expire(text, key, now)?, Expire::Never)
                }
                None => false,
            })
        };
        let pack_refs = match config.string("gc.packRefs") {
            Some(value) if value == "notbare" => !repo.is_bare(),
            Some(_) => boolean("gc.packRefs", true)?,
            None => true,
        };
        let expire = |key: &str, default: &str| -> Result<Expire> {
            let value = config.string(key).map(|value| value.to_string());
            parse_expire(value.as_deref().unwrap_or(default), key, now)
        };
        let log_expire = match expire("gc.logExpiry", "1.day.ago")? {
            Expire::At(time) => time.seconds,
            Expire::Never => 0,
            Expire::Now => i64::MAX,
        };
        Ok(Settings {
            auto: integer("gc.auto", 6700)?,
            auto_pack_limit: integer("gc.autoPackLimit", 50)?,
            pack_refs,
            prune_reflogs: !(is_never("gc.reflogExpire")? && is_never("gc.reflogExpireUnreachable")?),
            cruft_packs: boolean("gc.cruftPacks", true)?,
            prune_expire: expire("gc.pruneExpire", "2.weeks.ago")?,
            log_expire_seconds: log_expire,
            detach_auto: boolean("gc.autoDetach", true)?,
        })
    }
}

/// What a caller needs to know to start automatic maintenance after it changed the repository, which Git decides in the
/// same way for every command that does so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Auto {
    /// `false` if automatic maintenance is turned off with `maintenance.auto`, or with `gc.auto` being `0` or less if
    /// `maintenance.auto` isn't set.
    pub enabled: bool,
    /// `true` if it should run in the background, as `maintenance.autoDetach`, and without that `gc.autoDetach`, say, which is
    /// the case if neither is set.
    pub detach: bool,
}

/// Return `true` if there are more loose objects than `limit` says, which is estimated by those in one of the 256 directories,
/// and compared with the limit rounded up to the next multiple of 256, as Git does.
pub(super) fn too_many_loose_objects(objects_dir: &Path, hash: gix_hash::Kind, limit: i64) -> bool {
    if limit <= 0 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(objects_dir.join("17")) else {
        return false;
    };
    let count = entries
        .flatten()
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.len() == hash.len_in_hex() - 2
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
        })
        .count() as u64;
    let rounded = (limit as u64).div_ceil(256) * 256;
    count * 256 > rounded
}

fn has_entries(dir: PathBuf) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
}

/// Return `true` if there are linked worktrees, which `worktree-prune` may have to look at.
pub(super) fn worktrees_to_prune(repo: &Repository) -> bool {
    has_entries(repo.common_dir().join("worktrees"))
}

/// Return `true` if there are records of `rerere`, which `rerere gc` may have to remove.
pub(super) fn rerere_records(repo: &Repository) -> bool {
    has_entries(repo.common_dir().join("rr-cache"))
}

impl Repository {
    /// Return what automatic maintenance should do after a command changed the repository, like `git receive-pack` and `git commit`
    /// decide before they start `git maintenance run --auto`. Use [`run_maintenance()`](Self::run_maintenance()) with
    /// [`auto`](crate::maintenance::run::Options::auto) for it.
    pub fn auto_maintenance(&self) -> Result<Auto> {
        let config = self.config_snapshot();
        let enabled = match config
            .try_boolean("maintenance.auto")
            .or_raise(|| message("The value of maintenance.auto isn't a boolean"))?
        {
            Some(enabled) => enabled,
            None => match config
                .try_integer("gc.auto")
                .or_raise(|| message("The value of gc.auto isn't a number"))?
            {
                Some(threshold) => threshold > 0,
                None => true,
            },
        };
        let detach = match config
            .try_boolean("maintenance.autoDetach")
            .or_raise(|| message("The value of maintenance.autoDetach isn't a boolean"))?
        {
            Some(detach) => detach,
            None => config
                .try_boolean("gc.autoDetach")
                .or_raise(|| message("The value of gc.autoDetach isn't a boolean"))?
                .unwrap_or(true),
        };
        Ok(Auto { enabled, detach })
    }

    /// Return `true` if there are too many loose objects or packs for automatic maintenance, which is what `git gc --auto` and
    /// the `gc` task of `git maintenance run --auto` ask.
    pub(super) fn gc_is_due(&self, settings: &Settings) -> Result<bool> {
        if settings.auto <= 0 {
            return Ok(false);
        }
        let objects_dir = self.objects.store_ref().path().to_owned();
        if settings.auto_pack_limit > 0
            && i64::try_from(repack::count_unkept_packs(&objects_dir.join("pack"))?).unwrap_or(i64::MAX)
                > settings.auto_pack_limit
        {
            return Ok(true);
        }
        Ok(too_many_loose_objects(&objects_dir, self.object_hash(), settings.auto))
    }

    /// Run the `pre-auto-gc` hook and return `true` if maintenance may go on.
    pub(super) fn pre_auto_gc_allows(&self) -> Result<bool> {
        let outcome = self
            .hook("pre-auto-gc")
            .or_raise(|| message("Could not prepare the pre-auto-gc hook"))?
            .run()
            .or_raise(|| message("Could not run the pre-auto-gc hook"))?;
        Ok(outcome.is_success())
    }

    /// Pack references and expire reflogs as far as the settings ask for it, which is what Git's `gc` does before it detaches.
    pub(super) fn gc_foreground_tasks(&self, settings: &Settings, auto: bool, outcome: &mut Outcome) -> Result<()> {
        if settings.pack_refs {
            self.pack_all_refs(auto)
                .or_raise(|| message("Could not pack references, so garbage collection was stopped"))?;
            outcome.packed_refs = true;
        }
        if settings.prune_reflogs {
            outcome.reflogs = Some(
                self.expire_reflogs(reflog::Options::default())
                    .or_raise(|| message("Could not expire reflogs, so garbage collection was stopped"))?,
            );
        }
        Ok(())
    }

    /// Pack all references and remove the loose ones, like `git pack-refs --all --prune`, or `--auto` if `auto` is set.
    pub(super) fn pack_all_refs(&self, auto: bool) -> Result<()> {
        let mut options = gix_ref::pack_refs::Options::default();
        options.auto = auto;
        self.ref_store().pack_refs(&self.objects, options, None)?;
        Ok(())
    }

    /// Run garbage collection, as the [module documentation](crate::maintenance::gc) describes.
    pub fn gc(&self, options: Options, should_interrupt: &AtomicBool) -> Result<Outcome> {
        self.gc_inner(options, should_interrupt, false)
    }

    /// Run garbage collection. `from_maintenance` is `true` for the run that `maintenance run` does with the lock of
    /// `objects/maintenance.lock` held, which is what `git gc --no-detach --skip-foreground-tasks` is for Git: it leaves out
    /// that lock, the foreground tasks, the `pre-auto-gc` hook that was run already, and the check of `gc.log`.
    pub(super) fn gc_inner(
        &self,
        options: Options,
        should_interrupt: &AtomicBool,
        from_maintenance: bool,
    ) -> Result<Outcome> {
        let now = gix_date::Zoned::now();
        let mut settings = Settings::read(self, &now)?;
        if let Some(expire) = options.prune_expire {
            settings.prune_expire = expire;
        }
        let mut outcome = Outcome::default();
        let git_dir = self.common_dir().to_owned();
        let objects_dir = self.objects.store_ref().path().to_owned();

        if options.auto {
            if !self.gc_is_due(&settings)? {
                outcome.skipped = Some(Skipped::NotDue);
                return Ok(outcome);
            }
            if !from_maintenance {
                if !self.pre_auto_gc_allows()? {
                    outcome.skipped = Some(Skipped::DeclinedByHook);
                    return Ok(outcome);
                }
                if settings.detach_auto {
                    // What Git does for the run that would have been detached: don't try again what failed before.
                    let log = git_dir.join("gc.log");
                    if let Some(metadata) = std::fs::metadata(&log).ok().filter(|metadata| metadata.len() > 0) {
                        let modified = metadata
                            .modified()
                            .ok()
                            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX));
                        if modified >= settings.log_expire_seconds {
                            outcome.skipped = Some(Skipped::LastRunFailed(log));
                            return Ok(outcome);
                        }
                    }
                }
            }
        }

        let _pid = match GcPid::acquire(&git_dir, options.force)? {
            Ok(pid) => pid,
            Err(busy) if options.auto => {
                outcome.skipped = Some(Skipped::HeldBy(busy));
                return Ok(outcome);
            }
            Err(Busy::Holder(holder)) => bail!(message!(
                "gc is already running on machine '{}' pid {}, use force to run anyway",
                holder.host,
                holder.pid
            )),
            Err(Busy::LockFile(path)) => bail!(message!(
                "{} exists, which means that gc is starting, or was killed while it did. Remove it if no gc runs",
                path.display()
            )),
        };
        let _maintenance = if from_maintenance {
            None
        } else {
            match MaintenanceLock::acquire(&objects_dir)? {
                Some(lock) => Some(lock),
                None => {
                    outcome.skipped = Some(Skipped::MaintenanceLockHeld(objects_dir.join("maintenance.lock")));
                    return Ok(outcome);
                }
            }
        };

        // Git ignores what the foreground tasks say, but a failure is still reported, after the rest was done, so that a
        // packed-refs lock that somebody holds doesn't keep objects from being reclaimed.
        let foreground = if from_maintenance {
            Ok(())
        } else {
            self.gc_foreground_tasks(&settings, options.auto, &mut outcome)
        };
        self.gc_unsupported(&mut outcome);
        let objects = self.gc_objects(&settings, options, should_interrupt, &mut outcome);
        match (foreground, objects) {
            (Ok(()), objects) => objects?,
            (Err(foreground), Ok(())) => return Err(foreground),
            // both are reported, with the one that came first as context
            (Err(foreground), Err(objects)) => {
                return Err(objects.and_raise(message!(
                    "Packing references or expiring reflogs failed before: {foreground}"
                )));
            }
        }
        // Holding `gc.pid` keeps a detached `git gc`, which writes the file, from running, so this can't remove what it wrote.
        std::fs::remove_file(git_dir.join("gc.log")).ok();
        Ok(outcome)
    }

    /// Say what there is to do that isn't done, if there is any.
    fn gc_unsupported(&self, outcome: &mut Outcome) {
        // What only chooses which large packs are rewritten, and what to do with a filter, is not done, which costs time and not data.
        // `gc.maxCruftSize` makes Git keep what `now` as the expiry would drop, which is not done here.
        let config = self.config_snapshot();
        for key in ["gc.bigPackThreshold", "gc.maxCruftSize", "gc.repackFilter"] {
            if config.string(key).is_some() {
                outcome.unsupported.push(key);
            }
        }
        if worktrees_to_prune(self) {
            outcome.unsupported.push("worktree-prune");
        }
        if rerere_records(self) {
            outcome.unsupported.push("rerere-gc");
        }
        if self.objects.store_ref().path().join("info/commit-graph").exists()
            || self.objects.store_ref().path().join("info/commit-graphs").exists()
        {
            outcome.unsupported.push("commit-graph");
        }
    }

    /// Repack, prune, and remove what the packs left behind.
    fn gc_objects(
        &self,
        settings: &Settings,
        options: Options,
        should_interrupt: &AtomicBool,
        outcome: &mut Outcome,
    ) -> Result<()> {
        if let Err(err) = self.refuse_to_delete_objects() {
            // Git leaves repositories with precious objects, and those with promisor packs that it can't prune, as they are.
            outcome.objects_not_touched = Some(format!("{err}"));
            return Ok(());
        }
        let objects_dir = self.objects.store_ref().path().to_owned();
        let pack_dir = objects_dir.join("pack");
        let too_many_packs = settings.auto_pack_limit > 0
            && i64::try_from(repack::count_unkept_packs(&pack_dir)?).unwrap_or(i64::MAX) > settings.auto_pack_limit;

        let repack_options = repack::Options {
            scope: if options.auto && !too_many_packs {
                repack::Scope::Incremental
            } else {
                repack::Scope::All
            },
            unreachable: settings.unreachable(),
            ..Default::default()
        };
        outcome.repack = Some(
            self.repack(repack_options, should_interrupt)
                .or_raise(|| message("Could not repack the objects"))?,
        );

        // Pruning follows, with cruft packs as well, as it removes loose objects and what writers left behind.
        match settings.prune_expire {
            Expire::Never => {
                outcome.removed_as_packed = self.prune_packed(prune::PackedOptions::default())?;
            }
            Expire::Now => {
                outcome.prune = Some(self.prune(prune::Options {
                    expire: Some(gix_date::Time::now_utc()),
                    ..Default::default()
                })?);
            }
            Expire::At(time) => {
                outcome.prune = Some(self.prune(prune::Options {
                    expire: Some(time),
                    ..Default::default()
                })?);
            }
        }

        // What a killed run was removing has been finished by the repack, so what is left is garbage.
        outcome.pack_garbage_removed = repack::remove_pack_garbage(&pack_dir)?;
        outcome.too_many_unreachable_loose_objects =
            options.auto && too_many_loose_objects(&objects_dir, self.object_hash(), settings.auto);
        Ok(())
    }
}
