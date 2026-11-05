//! The two files that keep maintenance runs of Git and of this crate from running at the same time: `gc.pid`, which `git gc`
//! takes, and `objects/maintenance.lock`, which `git maintenance run` takes.
use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use gix_error::{Result, ResultExt, message};

/// The name of this machine, as `gc.pid` records it.
pub(super) fn host_name() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|name| name.trim().to_owned())
}

/// Return `true` if the process `pid` on the host `host` is known not to exist, which is only the case for a process on this
/// machine on Linux. Elsewhere a process is assumed to be alive, which only delays what its lock protects.
pub(super) fn process_is_gone(host: Option<&str>, pid: u32) -> bool {
    cfg!(target_os = "linux")
        && host.is_some_and(|host| Some(host) == host_name().as_deref())
        && !Path::new("/proc").join(pid.to_string()).exists()
}

/// Who holds `gc.pid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The process that holds it.
    pub pid: u32,
    /// The machine it runs on.
    pub host: String,
}

/// Why `gc.pid` couldn't be taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Busy {
    /// `gc.pid` names a process that may still run.
    Holder(Holder),
    /// `gc.pid.lock` exists, which is what somebody holds while they take `gc.pid`, or what a killed process left behind.
    LockFile(PathBuf),
}

/// The `gc.pid` file of this process, which is removed when it is dropped if it still is ours, like Git's.
#[derive(Debug)]
pub(super) struct GcPid {
    path: PathBuf,
    content: String,
}

impl GcPid {
    /// Take `gc.pid` in `git_dir` like Git does, by creating `gc.pid.lock` exclusively, looking at `gc.pid` while holding it,
    /// writing `<pid> <host>` to the lock, and renaming it to `gc.pid`. What another process has written is respected unless it is
    /// more than 12 hours old, or that process is known to be gone, or `force` is set.
    /// Return why it's busy if somebody else has it, which is also the case if somebody holds `gc.pid.lock`, where Git gives up.
    pub(super) fn acquire(git_dir: &Path, force: bool) -> Result<std::result::Result<GcPid, Busy>> {
        let path = git_dir.join("gc.pid");
        let lock_path = git_dir.join("gc.pid.lock");
        let mut lock = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(Err(Busy::LockFile(lock_path))),
            Err(err) => gix_error::bail!(gix_error::ErrorExt::and_raise(
                err,
                message!("Could not create {}", lock_path.display())
            )),
        };
        let result = (|| -> Result<std::result::Result<GcPid, Busy>> {
            if !force
                && let Some(holder) = std::fs::metadata(&path)
                    .ok()
                    .filter(|metadata| {
                        metadata
                            .modified()
                            .ok()
                            .and_then(|time| time.elapsed().ok())
                            .is_none_or(|age| age.as_secs() <= 12 * 3600)
                    })
                    .and_then(|_| std::fs::read_to_string(&path).ok())
                    .and_then(|content| {
                        let (pid, host) = content.split_once(' ')?;
                        Some(Holder {
                            pid: pid.trim().parse().ok()?,
                            host: host.split_whitespace().next()?.to_owned(),
                        })
                    })
                    .filter(|holder| !process_is_gone(Some(&holder.host), holder.pid))
            {
                return Ok(Err(Busy::Holder(holder)));
            }
            let content = format!(
                "{} {}",
                std::process::id(),
                host_name().unwrap_or_else(|| "unknown".into())
            );
            lock.write_all(content.as_bytes())
                .or_raise(|| message!("Could not write {}", lock_path.display()))?;
            std::fs::rename(&lock_path, &path).or_raise(|| message!("Could not create {}", path.display()))?;
            Ok(Ok(GcPid { path, content }))
        })();
        if !matches!(result, Ok(Ok(_))) {
            std::fs::remove_file(&lock_path).ok();
        }
        result
    }
}

impl Drop for GcPid {
    fn drop(&mut self) {
        // Somebody who forced a run, or found this one to be stale, may have replaced it, which is theirs to remove.
        if std::fs::read_to_string(&self.path).is_ok_and(|content| content == self.content) {
            std::fs::remove_file(&self.path).ok();
        }
    }
}

/// The `objects/maintenance.lock` file, which stays while it is alive. Whoever finds it there lets maintenance be, and it has no
/// age limit, like Git's.
#[derive(Debug)]
pub(super) struct MaintenanceLock {
    _marker: gix_lock::Marker,
}

impl MaintenanceLock {
    /// Take the lock, or return `None` if it is held. Other failures, like a lack of permission, are errors.
    pub(super) fn acquire(objects_dir: &Path) -> Result<Option<Self>> {
        match gix_lock::Marker::acquire_to_hold_resource(
            objects_dir.join("maintenance"),
            gix_lock::acquire::Fail::Immediately,
            None,
            0,
        ) {
            Ok(marker) => Ok(Some(MaintenanceLock { _marker: marker })),
            Err(err) if err.dominant_class() == Some(gix_error::Class::Retryable) => Ok(None),
            Err(err) => Err(err),
        }
    }
}
