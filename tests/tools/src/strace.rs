//! Observe which files a command writes, flushes and moves, and in which order, using `strace` on Linux.
//!
//! This is the means to compare *the sequence of file system effects* of Git and of `gix` for the same operation,
//! including the flushes, which leave no trace in the files themselves. A trace shows what was asked of the
//! operating system. It proves nothing about what a device did with it, in particular not after power loss.
//!
//! Calls that several threads of a process make at the same time are split by `strace` into an unfinished and a resumed
//! line, and the parser drops both. Only use this to observe effects that one thread produces at a time.
//!
//! Use [`available()`] to skip tests where tracing is impossible, for example on other platforms or where `ptrace` is
//! forbidden. Set `GIX_TEST_REQUIRE_STRACE` to turn skipping into a failure on machines that must be able to trace.
use std::{
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    sync::OnceLock,
};

/// Syscalls that change files, plus the flushes of data.
const SYSCALLS: &str = "openat,open,creat,fsync,fdatasync,sync_file_range,rename,renameat,renameat2,link,linkat,unlink,unlinkat,rmdir,mkdir,mkdirat";

/// An effect on a path, as observed. Paths are relative to the root passed to [`trace()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A file was opened with write access, or created.
    OpenForWriting(String),
    /// A file was flushed with `fsync()` or `fdatasync()`, reaching the device if the platform honours it.
    Flush(String),
    /// A file was written out with `sync_file_range()`, which doesn't ask the device to flush.
    Writeout(String),
    /// A file or directory was renamed.
    Rename(String, String),
    /// A hard link was created.
    Link(String, String),
    /// A file was removed.
    Unlink(String),
    /// A directory was created or removed.
    Directory(String),
}

impl Event {
    /// A single line describing the event, with failed calls marked by the caller.
    pub fn render(&self) -> String {
        match self {
            Event::OpenForWriting(p) => format!("open-for-writing {p}"),
            Event::Flush(p) => format!("flush {p}"),
            Event::Writeout(p) => format!("writeout {p}"),
            Event::Rename(a, b) => format!("rename {a} -> {b}"),
            Event::Link(a, b) => format!("link {a} -> {b}"),
            Event::Unlink(p) => format!("unlink {p}"),
            Event::Directory(p) => format!("directory {p}"),
        }
    }
}

/// An event with the outcome of the system call, so that a failed attempt remains visible.
///
/// A call that was interrupted by a fatal signal never returns and is not `ok` either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// What was attempted.
    pub event: Event,
    /// `true` if the call returned without an error, which excludes a call that never returned.
    pub ok: bool,
    /// `true` if `strace` made the call fail with an injected error. A call ended by an injected signal never returns,
    /// so it's not marked as injected, but is the last observation of its process and not `ok`.
    pub injected: bool,
}

/// A fault to inject into the system calls named in it, using `strace`'s own mechanism.
#[derive(Debug, Clone)]
pub struct Inject {
    /// The names of the syscalls to inject into, like `rename`, `renameat` and `renameat2`. Calls are counted for each
    /// syscall and each process separately, so `when` refers to the *n*th call of the same syscall. Assert which call was hit
    /// if the processes under test may use different variants of a syscall.
    pub syscalls: &'static [&'static str],
    /// What to do, like `error=EIO` or `signal=KILL`.
    pub action: &'static str,
    /// The call, counting from 1, at which to inject the fault.
    pub when: u32,
}

/// The result of running a command under `strace`.
#[derive(Debug)]
pub struct Trace {
    /// The exit status of the command, which is that of the signal if it was killed.
    pub status: ExitStatus,
    /// The observed effects below the root, in the order in which they happened, across all processes.
    pub observations: Vec<Observation>,
}

impl Trace {
    /// One line per observation, with failed calls marked with `!`, which is suitable for comparing and snapshots.
    pub fn render(&self) -> Vec<String> {
        self.observations
            .iter()
            .map(|o| format!("{}{}", if o.ok { "" } else { "! " }, o.event.render()))
            .collect()
    }
}

/// Return `true` if `strace` can trace a process here.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        cfg!(target_os = "linux")
            && Command::new("strace")
                .args(["-qq", "-o", "/dev/null", "-e", "trace=none", "true"])
                .status()
                .is_ok_and(|s| s.success())
    })
}

/// Return `true` if tracing is possible, so that a test can proceed. Otherwise print why a test is skipped, or panic if
/// the environment variable `GIX_TEST_REQUIRE_STRACE` is set.
pub fn available_or_skip(test: &str) -> bool {
    if available() {
        return true;
    }
    assert!(
        std::env::var_os("GIX_TEST_REQUIRE_STRACE").is_none(),
        "{test}: strace is required by GIX_TEST_REQUIRE_STRACE but cannot trace here"
    );
    eprintln!("SKIPPED {test}: needs Linux with a working `strace`");
    false
}

/// Run `command` with all its child processes under `strace` and return what it did to the files below `root`.
/// Relative paths seen in system calls resolve against `cwd`, which must be the working directory the command uses.
///
/// Names that Git and `gix` make up randomly, like `tmp_obj_Ab3dE9`, are normalized to end in `XXXXXX`.
/// Use `inject` to make a system call fail or kill the process, for example to simulate an I/O error.
/// If `stdin` is set, the command reads its standard input from that file.
pub fn trace(
    command: &Command,
    root: &Path,
    cwd: &Path,
    inject: Option<&Inject>,
    stdin: Option<&Path>,
) -> std::io::Result<Trace> {
    let out_dir = tempfile::tempdir()?;
    let out = out_dir.path().join("trace.txt");
    let mut traced = Command::new("strace");
    traced
        .args(["-f", "-y", "-qq", "-s", "0", "-e"])
        .arg(format!("trace={SYSCALLS}"));
    if let Some(inject) = inject {
        traced.arg("-e").arg(format!(
            "inject={}:{}:when={}",
            inject.syscalls.join(","),
            inject.action,
            inject.when
        ));
    }
    traced
        .arg("-o")
        .arg(&out)
        .arg("--")
        .arg(command.get_program())
        .args(command.get_args());
    if let Some(dir) = command.get_current_dir() {
        traced.current_dir(dir);
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => traced.env(key, value),
            None => traced.env_remove(key),
        };
    }
    let status = traced
        .stdin(match stdin {
            Some(path) => std::process::Stdio::from(std::fs::File::open(path)?),
            None => std::process::Stdio::null(),
        })
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    let raw = std::fs::read_to_string(&out)?;
    Ok(Trace {
        status,
        observations: raw.lines().filter_map(|line| parse(line, root, cwd)).collect(),
    })
}

fn parse(line: &str, root: &Path, cwd: &Path) -> Option<Observation> {
    let line = line.split_once(' ')?.1.trim_start();
    let (name, rest) = line.split_once('(')?;
    let (args, outcome) = rest.rsplit_once(") ")?;
    // `= ?` is a call that never returned, as it was killed by a signal.
    let outcome = outcome.trim_start();
    let ok = outcome
        .strip_prefix("= ")
        .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
    let injected = outcome.contains("(INJECTED)");
    let strings: Vec<&str> = quoted(args);
    // File descriptors are annotated with the path they refer to, as in `3</path/to/file>`.
    let fd_path = || {
        args.split_once('<')
            .and_then(|(_, p)| p.split_once('>'))
            .map(|(p, _)| p)
    };
    let rel = |p: &str| relative(p, root, cwd);
    let event = match name {
        "openat" | "open" | "creat" => {
            // Flags follow the quoted path.
            let flags = args.rsplit_once('"').map_or("", |(_, flags)| flags);
            let writes = name == "creat"
                || ["O_WRONLY", "O_RDWR", "O_CREAT", "O_APPEND", "O_TRUNC"]
                    .iter()
                    .any(|flag| flags.contains(flag));
            if !writes {
                return None;
            }
            Event::OpenForWriting(rel(strings.first()?)?)
        }
        "fsync" | "fdatasync" => Event::Flush(rel(fd_path()?)?),
        "sync_file_range" => Event::Writeout(rel(fd_path()?)?),
        "rename" | "renameat" | "renameat2" => Event::Rename(rel(strings.first()?)?, rel(strings.get(1)?)?),
        "link" | "linkat" => Event::Link(rel(strings.first()?)?, rel(strings.get(1)?)?),
        "unlink" | "unlinkat" => {
            // `unlinkat()` with `AT_REMOVEDIR` removes directories, which `rmdir()` does as well.
            if args.contains("AT_REMOVEDIR") {
                Event::Directory(rel(strings.first()?)?)
            } else {
                Event::Unlink(rel(strings.first()?)?)
            }
        }
        "rmdir" | "mkdir" | "mkdirat" => Event::Directory(rel(strings.first()?)?),
        _ => return None,
    };
    Some(Observation { event, ok, injected })
}

/// Extract the double-quoted strings of a system call's arguments, which are paths here.
fn quoted(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = args;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { break };
        out.push(&after[..end]);
        rest = &after[end + 1..];
    }
    out
}

/// Make `path` relative to `root` after resolving it against `cwd`, or return `None` if it is somewhere else.
fn relative(path: &str, root: &Path, cwd: &Path) -> Option<String> {
    let absolute: PathBuf = if Path::new(path).is_absolute() {
        path.into()
    } else {
        cwd.join(path)
    };
    // Resolve `.` components that Git leaves in place, like `./objects`.
    let cleaned: PathBuf = absolute.components().collect();
    let relative = cleaned.strip_prefix(root).ok()?;
    let normalized: Vec<String> = relative
        .components()
        .map(|c| normalize_component(&c.as_os_str().to_string_lossy()))
        .collect();
    Some(normalized.join("/"))
}

/// Replace the random six characters at the end of names like `tmp_obj_Ab3dE9`, `bulk_fsync_Ab3dE9` and
/// `tmp_objdir-bulk-fsync-Ab3dE9` or `0x01-0x01-ab12cd34.temp.Ab3dE9`.
fn normalize_component(name: &str) -> String {
    let random_suffix = name.starts_with("tmp_") || name.starts_with("bulk_fsync_") || name.contains(".temp.");
    if random_suffix && name.len() > 6 {
        let (head, tail) = name.split_at(name.len() - 6);
        let separator = head.chars().last();
        if tail.chars().all(|c| c.is_ascii_alphanumeric()) && matches!(separator, Some('_' | '-' | '.')) {
            return format!("{head}XXXXXX");
        }
    }
    name.to_owned()
}
