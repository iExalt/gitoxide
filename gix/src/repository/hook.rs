use std::path::PathBuf;

use gix_hook::Side;

use crate::{
    Repository,
    error::{ResultExt, bail, message},
};

/// Variables describing the location of a repository, which `git` only passes on if it was opened through them.
/// They'd otherwise point hooks to another repository, for instance when running from the hook of another one.
const LOCATION_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Hooks
impl Repository {
    /// Return the directory containing hook files: the value of `core.hooksPath` if set in a trusted configuration file,
    /// or else the `hooks` directory of the [common directory](Self::common_dir()), which linked worktrees share.
    ///
    /// A relative `core.hooksPath` is relative to the directory the hooks run in, which is the top-level directory of
    /// the worktree or the Git directory of a bare repository, and on the receiving [`Side`] it's the Git directory,
    /// see [`hook_for()`](Self::hook_for()). The path is made absolute using the captured
    /// [current directory](Self::current_dir()).
    ///
    /// The path can have a `:(optional)` prefix, which makes it missing if it can't be accessed. Note that Git
    /// ignores it only if the path doesn't exist, and tests a relative path against the directory the hook runs in
    /// whereas this tests it relative to the current directory of the process.
    ///
    /// Use it with [`gix_hook::find()`] and [`Side::Receive`] to locate a single hook file, which is what `proc-receive` needs as
    /// it's never defined by configuration.
    pub fn hooks_dir(&self, side: Side) -> crate::Result<PathBuf> {
        let configured = self
            .config_snapshot()
            .trusted_path("core.hooksPath")
            .or_raise(|| message("Failed to interpolate core.hooksPath"))?;
        Ok(match configured {
            Some(path) => self.current_dir().join(self.hook_dir(side)).join(path),
            None => self.current_dir().join(self.common_dir()).join("hooks"),
        })
    }

    /// Return the hooks to run for `event` such as `pre-push`, as a [`gix_hook::Prepare`] to configure further with arguments,
    /// standard input and environment, and to [run](gix_hook::Prepare::run()).
    ///
    /// Hooks are defined by trusted `hook.*` configuration and by the file named after `event` in
    /// [`hooks_dir()`](Self::hooks_dir()). It is an error to call this on a repository that isn't fully trusted, as
    /// Git wouldn't open it and hooks are programs.
    ///
    /// The hooks run like in `git`: in the top-level directory of the worktree, or in the Git directory for bare
    /// repositories and for receiving events like `pre-receive`, `update`, `post-receive`, `post-update` and
    /// `push-to-checkout`. `GIT_DIR` is set to the absolute path of the Git directory, but only in the cases `git` does so:
    /// if it isn't the `.git` directory of the worktree, like for bare repositories and linked worktrees, and for receiving events. Git sets it to a relative path in some of these,
    /// and sets `GIT_PREFIX` which is left to the caller, as are variables like `GIT_INDEX_FILE`.
    /// Unlike `git`, no exec-path is prepended to `PATH`.
    ///
    /// Use [`hook_advice()`](Self::hook_advice()) to get the advice for a hook that was ignored for not being executable.
    /// It shows the absolute path, where Git shows it relative to the directory the hook runs in.
    pub fn hook(&self, event: &str) -> crate::Result<gix_hook::Prepare> {
        self.hook_for(event, Side::of_event(event))
    }

    /// Like [`hook()`](Self::hook()), but run the hook on a specific `side`, which matters for events that happen locally as
    /// well as while receiving, like `reference-transaction`.
    ///
    /// Location variables like `GIT_WORK_TREE` or `GIT_INDEX_FILE` that this process has are not passed on, unless the
    /// caller sets them on the returned [`Prepare`](gix_hook::Prepare).
    pub fn hook_for(&self, event: &str, side: Side) -> crate::Result<gix_hook::Prepare> {
        if self.git_dir_trust() != gix_sec::Trust::Full {
            bail!(message!(
                "Refusing to run '{event}' hooks of a repository that isn't owned by the current user"
            ));
        }
        let settings = gix_hook::Settings::from_config(&self.config.resolved, &mut self.filter_config_section())
            .or_raise(|| message("Failed to read hook configuration"))?;
        let hooks = gix_hook::Hooks::resolve(event, &self.hooks_dir(side)?, Some(&settings));
        let mut prepare = gix_hook::Prepare::new(hooks).current_dir(self.current_dir().join(self.hook_dir(side)));
        for key in LOCATION_ENV {
            prepare = prepare.env_remove(*key);
        }
        if side == Side::Receive || !self.has_default_git_dir() {
            prepare = prepare.with_context(gix_command::Context {
                git_dir: Some(self.current_dir().join(self.git_dir())),
                ..Default::default()
            });
        }
        Ok(prepare)
    }

    /// Return the advice `git` shows when it ignores the hook file at `path` for not being executable, or `None` if
    /// `advice.ignoredHook` is turned off. Callers print it, and show it once per hook if they like.
    pub fn hook_advice(&self, path: &std::path::Path) -> Option<String> {
        let enabled = self.config_snapshot().boolean("advice.ignoredHook").unwrap_or(true);
        enabled.then(|| gix_hook::ignored_hook_hint(path))
    }

    /// Return `true` if the Git directory is the `.git` directory of the worktree, which is the only case in which Git
    /// finds it from the worktree and thus doesn't export `GIT_DIR` to hooks. Bare repositories, linked worktrees,
    /// gitfiles and `core.worktree` or environment setups all don't have it.
    fn has_default_git_dir(&self) -> bool {
        let Some(workdir) = self.workdir() else {
            return false;
        };
        let dot_git = self.current_dir().join(workdir).join(".git");
        match (
            std::fs::canonicalize(dot_git),
            std::fs::canonicalize(self.current_dir().join(self.git_dir())),
        ) {
            (Ok(dot_git), Ok(git_dir)) => dot_git == git_dir,
            _ => false,
        }
    }

    fn hook_dir(&self, side: Side) -> PathBuf {
        match self.workdir() {
            Some(workdir) if side == Side::Local => workdir.to_owned(),
            _ => self.git_dir().to_owned(),
        }
    }
}
