use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// Where a [`Program`](crate::Program) looks for things that Git would take from its environment.
///
/// It is what makes credential helpers resolvable without running `git`: Git finds `git-credential-<name>` in its
/// exec-path and then in `PATH`, and the `store` helper locates its files in the home directory.
/// A program that has none uses the one [of the process](Environment::from_process()).
#[derive(Debug, Clone, Default)]
pub struct Environment {
    /// The user's home directory, which holds `~/.git-credentials`.
    pub home: Option<PathBuf>,
    /// The `XDG_CONFIG_HOME` directory, whose `git/credentials` file is the second default location of the `store` helper.
    /// Without it, `.config/git/credentials` within [`home`](Self::home) is used, as Git does.
    pub xdg_config_home: Option<PathBuf>,
    /// Directories to search for `git-credential-<name>` before the directories in [`path`](Self::path), typically what
    /// `GIT_EXEC_PATH` or `gitoxide.credentials.execPath` provide.
    ///
    /// Git's compiled-in exec-path isn't known here, as finding it requires running `git`. A helper that is found only there,
    /// like the `osxkeychain` helper of some installations, has to be made available by listing its directory here.
    pub exec_path: Vec<PathBuf>,
    /// The value of `PATH` to search for helpers, or `None` to use the one of the process.
    pub path: Option<OsString>,
}

impl Environment {
    /// Obtain the environment of the current process: its home directory, `XDG_CONFIG_HOME`, `GIT_EXEC_PATH` as [`exec_path`](Self::exec_path),
    /// and `PATH`, which is read when helpers are looked up.
    pub fn from_process() -> Self {
        let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        Environment {
            home: gix_path::env::home_dir(),
            xdg_config_home: non_empty("XDG_CONFIG_HOME").map(PathBuf::from),
            exec_path: non_empty("GIT_EXEC_PATH").map(PathBuf::from).into_iter().collect(),
            path: None,
        }
    }

    /// Find the executable of the credential helper `name` like `foo` for `git-credential-foo`, searching
    /// [`exec_path`](Self::exec_path) and then `PATH`, without running anything.
    ///
    /// Returns `None` if `name` isn't a plain name, or if no directory contains an executable file with the helper's file name.
    pub fn find_helper(&self, name: &str) -> Option<PathBuf> {
        if name.is_empty() || name.contains(std::path::is_separator) {
            return None;
        }
        let file_name = format!("git-credential-{name}{}", std::env::consts::EXE_SUFFIX);
        let path = match &self.path {
            Some(path) => path.clone(),
            None => std::env::var_os("PATH").unwrap_or_default(),
        };
        self.exec_path
            .iter()
            .cloned()
            .chain(std::env::split_paths(&path))
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join(&file_name))
            .find(|candidate| is_executable_file(candidate))
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}
