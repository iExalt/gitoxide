//! Running the tasks of `git maintenance run`, with `--auto` to run only those that are due.
//!
//! [`Repository::run_maintenance()`] takes `objects/maintenance.lock` like Git, and runs the tasks in the order of Git's table,
//! first what Git does before it detaches (`pack-refs`, `reflog-expire` and the references and reflogs part of `gc`) and then the
//! rest. Without tasks in the options they are chosen like Git chooses them for a manual run: from `maintenance.strategy`, which
//! is `geometric` if unset, and then `maintenance.<task>.enabled`.
//!
//! With [`Options::auto`] each task runs if its condition holds, which the module documentation of [`gc`](mod@super::gc) and the
//! documentation of [`Task`] describe, and the `pre-auto-gc` hook is run once, before the first task that is due, and a hook that
//! fails makes all the tasks that follow not run.
//!
//! # Differences from Git
//!
//! * `prefetch`, `incremental-repack`, `commit-graph`, `worktree-prune` and `rerere-gc` are not implemented. They are reported as
//!   [unsupported](TaskOutcome::Unsupported), in an automatic run only if there is something that they would do, and they never
//!   fail the run.
//! * Nothing is detached, see [`Repository::auto_maintenance()`](crate::Repository::auto_maintenance()) for what a caller may do.
//! * The `gc` task doesn't run the `pre-auto-gc` hook a second time, as the `gc` that Git starts does.
use std::{path::PathBuf, sync::atomic::AtomicBool};

use gix_error::{ErrorExt, Result, ResultExt, bail, message};

use super::{
    gc::{self, Settings},
    lock::MaintenanceLock,
    repack,
};
use crate::Repository;

/// A task of `git maintenance run`, in the order in which Git runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Task {
    /// `prefetch`, which fetches from the remotes. Not supported.
    Prefetch,
    /// `loose-objects`, which removes loose objects that are in a pack and packs up to `maintenance.loose-objects.batchSize`
    /// others, `50000` by default. It is due if there are `maintenance.loose-objects.auto`, `100` by default, loose objects.
    LooseObjects,
    /// `incremental-repack`, which maintains a multi-pack-index. Not supported.
    IncrementalRepack,
    /// `geometric-repack`, which rolls up packs so that each has `maintenance.geometric-repack.splitFactor`, `2` by default,
    /// times as many objects as the next smaller one. It is due if that would roll up packs, or if there are
    /// `maintenance.geometric-repack.auto`, `6700` by default, loose objects.
    GeometricRepack,
    /// `gc`, which is [`Repository::gc()`] without the parts that `pack-refs` and `reflog-expire` do.
    Gc,
    /// `commit-graph`. Not supported.
    CommitGraph,
    /// `pack-refs`, which is `git pack-refs --all --prune`.
    PackRefs,
    /// `reflog-expire`, which is `git reflog expire --all`. It is due if `maintenance.reflog-expire.auto`, `100` by default,
    /// entries of the reflog of `HEAD` would expire.
    ReflogExpire,
    /// `worktree-prune`. Not supported.
    WorktreePrune,
    /// `rerere-gc`. Not supported.
    RerereGc,
}

impl Task {
    /// All tasks, in the order in which they run.
    pub const ALL: [Task; 10] = [
        Task::Prefetch,
        Task::LooseObjects,
        Task::IncrementalRepack,
        Task::GeometricRepack,
        Task::Gc,
        Task::CommitGraph,
        Task::PackRefs,
        Task::ReflogExpire,
        Task::WorktreePrune,
        Task::RerereGc,
    ];

    /// The name that Git uses for it.
    pub fn name(self) -> &'static str {
        match self {
            Task::Prefetch => "prefetch",
            Task::LooseObjects => "loose-objects",
            Task::IncrementalRepack => "incremental-repack",
            Task::GeometricRepack => "geometric-repack",
            Task::Gc => "gc",
            Task::CommitGraph => "commit-graph",
            Task::PackRefs => "pack-refs",
            Task::ReflogExpire => "reflog-expire",
            Task::WorktreePrune => "worktree-prune",
            Task::RerereGc => "rerere-gc",
        }
    }

    /// The task that Git calls `name`, ignoring case.
    pub fn from_name(name: &str) -> Option<Task> {
        Task::ALL
            .into_iter()
            .find(|task| task.name().eq_ignore_ascii_case(name))
    }

    /// The reason for a task to not be supported, if it isn't.
    fn unsupported(self) -> Option<&'static str> {
        match self {
            Task::Prefetch => Some("fetching from remotes isn't done"),
            Task::IncrementalRepack => Some("multi-pack-indices aren't written"),
            Task::CommitGraph => Some("commit-graphs aren't written"),
            Task::WorktreePrune => Some("worktrees aren't pruned"),
            Task::RerereGc => Some("rerere records aren't cleaned"),
            _ => None,
        }
    }
}

/// Options for [`Repository::run_maintenance()`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Options {
    /// The tasks to run, in the order of [`Task::ALL`] and each once, or none to run those that the configuration asks for.
    pub tasks: Vec<Task>,
    /// If `true`, run only the tasks that are due, like `git maintenance run --auto`.
    pub auto: bool,
}

/// What happened to a task.
#[derive(Debug)]
#[non_exhaustive]
pub enum TaskOutcome {
    /// The task ran.
    Ran,
    /// With `auto`, the task wasn't due.
    NotDue,
    /// With `auto`, the `pre-auto-gc` hook said no.
    DeclinedByHook,
    /// The task is not implemented, and it would have had something to do if it was `auto`, with the reason.
    Unsupported(&'static str),
    /// The task failed, and the other tasks ran nonetheless, as in Git.
    Failed(gix_error::Error),
}

/// What [`Repository::run_maintenance()`] did.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Outcome {
    /// If set, another run, or a `git maintenance` that was killed, has `objects/maintenance.lock`, and nothing was done. The lock
    /// has no age limit, as in Git.
    pub locked_by: Option<PathBuf>,
    /// What happened to each task that was selected.
    pub tasks: Vec<(Task, TaskOutcome)>,
    /// What the `gc` task did, if it ran.
    pub gc: Option<gc::Outcome>,
}

impl Outcome {
    /// Return `true` if no task failed, which is when `git maintenance run` succeeds.
    pub fn is_success(&self) -> bool {
        !self
            .tasks
            .iter()
            .any(|(_, outcome)| matches!(outcome, TaskOutcome::Failed(_)))
    }
}

/// Return the tasks that a manual run does without `--task`.
fn configured_tasks(repo: &Repository) -> Result<Vec<Task>> {
    let config = repo.config_snapshot();
    let strategy = config
        .string("maintenance.strategy")
        .map(|value| value.to_string().to_ascii_lowercase());
    let by_strategy: &[Task] = match strategy.as_deref() {
        None | Some("geometric") => &[
            Task::CommitGraph,
            Task::GeometricRepack,
            Task::PackRefs,
            Task::RerereGc,
            Task::ReflogExpire,
            Task::WorktreePrune,
        ],
        Some("gc" | "incremental") => &[Task::Gc],
        Some(other) => bail!(message!("unknown maintenance strategy: '{other}'")),
    };
    let mut tasks = Vec::new();
    for task in Task::ALL {
        let key = format!("maintenance.{}.enabled", task.name());
        let enabled = config
            .try_boolean(key.as_str())
            .or_raise(|| message!("The value of {key} isn't a boolean"))?
            .unwrap_or_else(|| by_strategy.contains(&task));
        if enabled {
            tasks.push(task);
        }
    }
    Ok(tasks)
}

/// Return the limit that the key `maintenance.<task>.auto` says, with `0` for never and a negative number for always.
fn auto_limit(repo: &Repository, task: Task, default: i64) -> Result<i64> {
    let key = format!("maintenance.{}.auto", task.name());
    Ok(repo
        .config_snapshot()
        .try_integer(key.as_str())
        .or_raise(|| message!("The value of {key} isn't a number"))?
        .unwrap_or(default))
}

/// Return `true` if there are at least `limit` loose objects, which has to be positive.
fn loose_objects_at_least(repo: &Repository, limit: i64) -> bool {
    let objects_dir = repo.objects.store_ref().path();
    let hex_len = repo.object_hash().len_in_hex() - 2;
    let mut count = 0;
    for fan_out in 0..=255u8 {
        let Ok(entries) = std::fs::read_dir(objects_dir.join(format!("{fan_out:02x}"))) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|name| name.len() == hex_len && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                count += 1;
                if count >= limit {
                    return true;
                }
            }
        }
    }
    false
}

impl Repository {
    /// Run the tasks of `git maintenance run`, as the [module documentation](crate::maintenance::run) describes.
    pub fn run_maintenance(&self, options: Options, should_interrupt: &AtomicBool) -> Result<Outcome> {
        let mut tasks = if options.tasks.is_empty() {
            configured_tasks(self)?
        } else {
            options.tasks.clone()
        };
        tasks.sort();
        tasks.dedup();

        let mut outcome = Outcome::default();
        let objects_dir = self.objects.store_ref().path().to_owned();
        let Some(_lock) = MaintenanceLock::acquire(&objects_dir)? else {
            outcome.locked_by = Some(objects_dir.join("maintenance.lock"));
            return Ok(outcome);
        };

        let now = gix_date::Zoned::now();
        let settings = Settings::read(self, &now)?;
        let mut state = State {
            repo: self,
            options: &options,
            settings,
            hook_allows: None,
            should_interrupt,
            gc_foreground: gc::Outcome::default(),
            gc_ran_foreground: false,
            results: tasks.iter().map(|task| (*task, None)).collect(),
        };
        for phase in [Phase::Foreground, Phase::Background] {
            for task in &tasks {
                state.run(*task, phase, &mut outcome)?;
            }
        }
        outcome.tasks = state
            .results
            .into_iter()
            .map(|(task, result)| (task, result.unwrap_or(TaskOutcome::NotDue)))
            .collect();
        Ok(outcome)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Foreground,
    Background,
}

struct State<'a> {
    repo: &'a Repository,
    options: &'a Options,
    settings: Settings,
    /// What the hook said, once it was asked.
    hook_allows: Option<bool>,
    should_interrupt: &'a AtomicBool,
    gc_foreground: gc::Outcome,
    gc_ran_foreground: bool,
    results: Vec<(Task, Option<TaskOutcome>)>,
}

impl State<'_> {
    fn record(&mut self, task: Task, result: TaskOutcome) {
        let slot = &mut self
            .results
            .iter_mut()
            .find(|(candidate, _)| *candidate == task)
            .expect("only selected tasks run")
            .1;
        // A failure sticks, and otherwise a task that ran in one phase has run.
        *slot = Some(match (slot.take(), result) {
            (Some(failed @ TaskOutcome::Failed(_)), _) => failed,
            (_, result @ TaskOutcome::Failed(_)) => result,
            (Some(TaskOutcome::Ran), _) | (_, TaskOutcome::Ran) => TaskOutcome::Ran,
            (_, result) => result,
        });
    }

    /// Run `task` in `phase` if it has something to do in that phase and is due, which is checked in each phase as Git does.
    fn run(&mut self, task: Task, phase: Phase, outcome: &mut Outcome) -> Result<()> {
        let foreground = matches!(task, Task::Gc | Task::PackRefs | Task::ReflogExpire);
        let background = !matches!(task, Task::PackRefs | Task::ReflogExpire);
        if (phase == Phase::Foreground && !foreground) || (phase == Phase::Background && !background) {
            return Ok(());
        }
        if self.options.auto {
            if self.hook_allows == Some(false) {
                self.record(task, TaskOutcome::DeclinedByHook);
                return Ok(());
            }
            let due = match self.is_due(task) {
                Ok(due) => due,
                Err(err) => {
                    self.record(task, TaskOutcome::Failed(err));
                    return Ok(());
                }
            };
            if !due {
                // An unsupported task is reported only if it would have had something to do.
                self.record(task, TaskOutcome::NotDue);
                return Ok(());
            }
            if let Some(reason) = task.unsupported() {
                self.record(task, TaskOutcome::Unsupported(reason));
                return Ok(());
            }
            if self.hook_allows.is_none() {
                let allows = self.repo.pre_auto_gc_allows()?;
                self.hook_allows = Some(allows);
                if !allows {
                    self.record(task, TaskOutcome::DeclinedByHook);
                    return Ok(());
                }
            }
        } else if let Some(reason) = task.unsupported() {
            self.record(task, TaskOutcome::Unsupported(reason));
            return Ok(());
        }
        match self.execute(task, phase, outcome) {
            Ok(()) => self.record(task, TaskOutcome::Ran),
            Err(err) => self.record(task, TaskOutcome::Failed(err)),
        }
        Ok(())
    }

    fn is_due(&self, task: Task) -> Result<bool> {
        let repo = self.repo;
        Ok(match task {
            // They have no condition, so they never run in an automatic run, or can't be checked.
            Task::Prefetch => false,
            Task::IncrementalRepack => {
                let limit = auto_limit(repo, task, 10)?;
                limit < 0
                    || (limit > 0
                        && i64::try_from(repack::count_unkept_packs(
                            &repo.objects.store_ref().path().join("pack"),
                        )?)
                        .unwrap_or(i64::MAX)
                            >= limit)
            }
            Task::LooseObjects => {
                let limit = auto_limit(repo, task, 100)?;
                limit < 0 || (limit > 0 && loose_objects_at_least(repo, limit))
            }
            Task::GeometricRepack => {
                let limit = auto_limit(repo, task, 6700)?;
                if limit <= 0 {
                    return Ok(limit < 0);
                }
                let pack_dir = repo.objects.store_ref().path().join("pack");
                repack::count_packs_to_roll_up(&pack_dir, repo.object_hash(), self.geometric_factor()?)?.0 > 0
                    || loose_objects_at_least(repo, limit)
            }
            Task::Gc => repo.gc_is_due(&self.settings)?,
            Task::CommitGraph => repo.objects.store_ref().path().join("info/commit-graph").exists(),
            Task::PackRefs => repo.ref_store().pack_refs_required(&repo.objects)?,
            Task::ReflogExpire => repo.reflog_expiry_is_due(auto_limit(repo, task, 100)?)?,
            Task::WorktreePrune => gc::worktrees_to_prune(repo),
            Task::RerereGc => gc::rerere_records(repo),
        })
    }

    fn geometric_factor(&self) -> Result<u32> {
        let factor = self
            .repo
            .config_snapshot()
            .try_integer("maintenance.geometric-repack.splitFactor")
            .or_raise(|| message("The value of maintenance.geometric-repack.splitFactor isn't a number"))?
            .unwrap_or(2);
        u32::try_from(factor).or_raise(|| message("maintenance.geometric-repack.splitFactor must not be negative"))
    }

    fn execute(&mut self, task: Task, phase: Phase, outcome: &mut Outcome) -> Result<()> {
        let repo = self.repo;
        let auto = self.options.auto;
        match task {
            Task::PackRefs => repo.pack_all_refs(auto),
            Task::ReflogExpire => {
                let reflogs = repo.expire_reflogs(super::reflog::Options::default())?;
                if let Some((name, err)) = reflogs.failed.into_iter().next() {
                    bail!(err.and_raise(message!("Could not expire the reflog of {name}")));
                }
                Ok(())
            }
            Task::LooseObjects => {
                repo.prune_packed(super::prune::PackedOptions::default())?;
                let batch = repo
                    .config_snapshot()
                    .try_integer("maintenance.loose-objects.batchSize")
                    .or_raise(|| message("The value of maintenance.loose-objects.batchSize isn't a number"))?
                    .unwrap_or(50_000);
                let batch = usize::try_from(batch).unwrap_or(1);
                repo.pack_loose_objects(batch, self.should_interrupt)?;
                Ok(())
            }
            Task::GeometricRepack => {
                let factor = self.geometric_factor()?;
                let pack_dir = repo.objects.store_ref().path().join("pack");
                let (split, packs) = repack::count_packs_to_roll_up(&pack_dir, repo.object_hash(), factor)?;
                let options = if split < packs {
                    repack::Options {
                        scope: repack::Scope::Geometric { factor },
                        ..Default::default()
                    }
                } else {
                    // Everything would be rolled up, which Git does as a full repack that expires what is unreachable
                    // through a cruft pack, as it has no `prune` task to do so otherwise.
                    repack::Options {
                        scope: repack::Scope::All,
                        unreachable: self.settings.unreachable(),
                        ..Default::default()
                    }
                };
                repo.repack(options, self.should_interrupt)?;
                Ok(())
            }
            Task::Gc if phase == Phase::Foreground => {
                let mut gc_outcome = gc::Outcome::default();
                repo.gc_foreground_tasks(&self.settings, auto, &mut gc_outcome)?;
                self.gc_foreground = gc_outcome;
                self.gc_ran_foreground = true;
                Ok(())
            }
            Task::Gc => {
                let options = gc::Options {
                    auto,
                    ..Default::default()
                };
                let mut gc_outcome = repo.gc_inner(options, self.should_interrupt, true)?;
                if self.gc_ran_foreground {
                    gc_outcome.packed_refs = self.gc_foreground.packed_refs;
                    gc_outcome.reflogs = self.gc_foreground.reflogs.take();
                }
                outcome.gc = Some(gc_outcome);
                Ok(())
            }
            Task::Prefetch | Task::IncrementalRepack | Task::CommitGraph | Task::WorktreePrune | Task::RerereGc => {
                Ok(())
            }
        }
    }
}
