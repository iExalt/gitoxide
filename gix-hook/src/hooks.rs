use std::path::{Path, PathBuf};

use crate::{Found, Settings, find};

/// Where a hook runs: on the local side of an operation, or on the receiving side of a push.
///
/// Git runs receiving hooks, including `reference-transaction` while receiving, in the Git directory with `GIT_DIR` set,
/// whereas local hooks run at the top of the worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The hook runs in the top-level directory of the worktree, or the Git directory of a bare repository.
    Local,
    /// The hook runs in the Git directory, with `GIT_DIR` set.
    Receive,
}

impl Side {
    /// The side `event` runs on in Git, which is [`Side::Receive`] for events that only exist there:
    /// `pre-receive`, `update`, `proc-receive`, `post-receive`, `post-update` and `push-to-checkout`.
    /// Others are local, but can also happen while receiving, like `reference-transaction`, so pick the side explicitly then.
    pub fn of_event(event: &str) -> Self {
        match event {
            "pre-receive" | "update" | "proc-receive" | "post-receive" | "post-update" | "push-to-checkout" => {
                Side::Receive
            }
            _ => Side::Local,
        }
    }
}

/// The way a [`Hook`] is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A command line from `hook.<name>.command`, run by the shell if it needs one.
    Configured {
        /// The command line.
        command: String,
        /// Whether `hook.<name>.parallel` allows it to run in parallel with others.
        parallel: bool,
    },
    /// The executable hook file at `path`.
    File {
        /// The path of the hook file, as built from the hooks directory.
        path: PathBuf,
    },
}

/// One hook to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    /// The friendly-name of a configured hook, or the event name of a hook file.
    pub name: String,
    /// How to run it.
    pub kind: Kind,
}

/// The hooks to run for an event, in the order `git` runs them.
#[derive(Debug, Clone)]
pub struct Hooks {
    pub(crate) hooks: Vec<Hook>,
    pub(crate) jobs: usize,
    pub(crate) not_executable: Option<PathBuf>,
}

impl Hooks {
    /// Resolve the hooks for `event`: the enabled configured hooks from `settings`, then the hook file in `hooks_dir`.
    ///
    /// `hooks_dir` is used as is, so callers anchor a relative `core.hooksPath` the way `git` does, which is at the
    /// top of the worktree or at the Git directory of a bare repository.
    pub fn resolve(event: &str, hooks_dir: &Path, settings: Option<&Settings>) -> Self {
        let configured = settings.map(|settings| settings.configured(event)).unwrap_or_default();
        let mut jobs = settings.and_then(|settings| settings.jobs(event)).unwrap_or(1);
        if configured.iter().any(|(_, _, parallel, _)| !parallel) {
            jobs = 1; // like Git, even disabled hooks count
        }
        let mut hooks: Vec<_> = configured
            .into_iter()
            .filter_map(|(name, command, parallel, enabled)| {
                Some(Hook {
                    name: name.into(),
                    kind: Kind::Configured {
                        command: command.filter(|_| enabled)?.into(),
                        parallel,
                    },
                })
            })
            .collect();
        let mut not_executable = None;
        match find(hooks_dir, event) {
            Found::Executable(path) => hooks.push(Hook {
                name: event.into(),
                kind: Kind::File { path },
            }),
            Found::NotExecutable(path) => not_executable = Some(path),
            Found::Missing => {}
        }
        Hooks {
            hooks,
            jobs,
            not_executable,
        }
    }

    /// The number of hooks that run at once by default, which is one unless configured.
    pub fn jobs(&self) -> usize {
        self.jobs
    }

    /// The hooks that will run, in order.
    pub fn hooks(&self) -> &[Hook] {
        &self.hooks
    }

    /// The hook file that was skipped for not being executable, to be reported with [`ignored_hook_hint()`](crate::ignored_hook_hint()).
    pub fn not_executable(&self) -> Option<&Path> {
        self.not_executable.as_deref()
    }

    /// Return `true` if no hook will run.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}
