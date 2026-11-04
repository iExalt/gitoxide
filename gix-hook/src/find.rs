use std::path::{Path, PathBuf};

/// The result of looking for a hook with [`find()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The hook exists and is executable.
    Executable(PathBuf),
    /// The hook exists but isn't executable, so `git` ignores it with [advice](ignored_hook_hint()).
    NotExecutable(PathBuf),
    /// There is no such hook.
    Missing,
}

/// Look for the hook `name` in `hooks_dir`, which is used as is, so a relative directory yields a relative path.
///
/// `name` must be a plain file name such as `pre-push`; anything with a path separator is [`Found::Missing`].
pub fn find(hooks_dir: &Path, name: &str) -> Found {
    if name.is_empty() || name.contains(['/', '\\']) {
        return Found::Missing;
    }
    let path = hooks_dir.join(name);
    match executable_state(&path) {
        State::Executable => Found::Executable(path),
        State::NotExecutable => Found::NotExecutable(path),
        State::Missing => Found::Missing,
    }
}

enum State {
    Executable,
    NotExecutable,
    Missing,
}

/// Like `git`, ask the operating system with `access(X_OK)`, so only a permission error means 'not executable'.
/// A directory passes, and fails when it is started.
#[cfg(unix)]
fn executable_state(path: &Path) -> State {
    use rustix::{fs::Access, io::Errno};
    match rustix::fs::access(path, Access::EXEC_OK) {
        Ok(()) => State::Executable,
        Err(Errno::ACCESS) => State::NotExecutable,
        Err(_) => State::Missing,
    }
}

/// Without execute permissions, every existing file is considered executable. `git` also tries a `.exe` suffix on Windows, which isn't supported.
#[cfg(not(unix))]
fn executable_state(path: &Path) -> State {
    if path.is_file() {
        State::Executable
    } else {
        State::Missing
    }
}

/// The advice `git` prints to standard error when it ignores the hook at `path` for not being executable.
///
/// Callers print it unless `advice.ignoredHook` is disabled.
pub fn ignored_hook_hint(path: &Path) -> String {
    format!(
        "hint: The '{}' hook was ignored because it's not set as executable.\n\
         hint: You can disable this warning with `git config set advice.ignoredHook false`.\n",
        path.display()
    )
}
