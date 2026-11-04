//! File-level building blocks that adding and compacting tables share.

use std::{
    collections::hash_map::RandomState,
    fs::{File, OpenOptions},
    hash::{BuildHasher, Hasher},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
};

use gix_error::{ErrorExt, Result, message};
use gix_fs::fsync::Components;
use gix_hash::Kind;

use super::{LIST_FILE_NAME, Options, UncertainAfterPublish, list::TABLE_SUFFIX, read_if_exists};
use crate::{Writer, basics::misuse};

/// Failures to provoke when testing, as they can't be produced reliably otherwise.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum Inject {
    /// Fail replacing the list after it was replaced, as if the confirmation got lost.
    AfterRename,
    /// Fail replacing the list before it was replaced.
    BeforeRename,
    /// Fail replacing the list after it was replaced, and make it impossible to read the list afterwards.
    AfterRenameWithUnreadableList,
}

/// A file which is deleted unless it is [kept](Self::keep()).
#[derive(Debug)]
pub(super) struct Temporary {
    pub(super) path: PathBuf,
    keep: bool,
}

impl Temporary {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path, keep: false }
    }

    pub(super) fn keep(mut self) -> PathBuf {
        self.keep = true;
        std::mem::take(&mut self.path)
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.keep {
            std::fs::remove_file(&self.path).ok();
        }
    }
}

pub(super) fn random() -> u64 {
    RandomState::new().build_hasher().finish()
}

fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o666);
    options.open(path)
}

/// A table which was written completely and flushed, under a temporary name.
#[derive(Debug)]
pub(super) struct Written {
    temp: Temporary,
    pub(super) min: u64,
    pub(super) max: u64,
}

impl Written {
    /// Give the table its final name, which is made of its update indices and a random part, and return that name.
    pub(super) fn persist(self, dir: &Path) -> Result<(String, PathBuf)> {
        let name = format!(
            "0x{:012x}-0x{:012x}-{:08x}{TABLE_SUFFIX}",
            self.min,
            self.max,
            random() as u32
        );
        let path = dir.join(&name);
        std::fs::rename(&self.temp.path, &path)
            .map_err(|err| err.and_raise(message!("Could not move the table to {}", path.display())))?;
        self.temp.keep();
        Ok((name, path))
    }
}

/// Write a table into `dir` with the records `write` adds to the writer, whose limits start out as `limits`, and
/// require its lower limit to be `min_allowed` or higher.
///
/// Return `None` if there were no records, as such a table isn't useful, and remove the file again whenever there is no table.
pub(super) fn write_table(
    dir: &Path,
    hash_kind: Kind,
    options: &Options,
    limits: (u64, u64),
    min_allowed: u64,
    write: impl FnOnce(&mut Writer<BufWriter<File>>) -> Result<()>,
) -> Result<Option<Written>> {
    let temp_name = format!(
        "0x{:012x}-0x{:012x}-{:08x}.temp.{:06x}",
        limits.0,
        limits.1,
        random() as u32,
        random() & 0xff_ffff
    );
    let temp = Temporary::new(dir.join(temp_name));
    let file =
        create_new(&temp.path).map_err(|err| err.and_raise(message!("Could not create {}", temp.path.display())))?;

    let mut writer = Writer::new(BufWriter::new(file), hash_kind, options.write)?;
    writer.set_limits(limits.0, limits.1)?;
    write(&mut writer)?;
    if writer.is_empty() {
        return Ok(None);
    }
    let (min, max) = writer.limits();
    if min < min_allowed {
        return Err(misuse(
            "a table must start at the next update index of the stack or later",
        ));
    }
    let file = writer
        .finish()?
        .into_inner()
        .map_err(|err| err.into_error().and_raise(message("Could not write the table")))?;
    options
        .fsync
        .flush(&file, Components::REFERENCE)
        .map_err(|err| err.and_raise(message("Could not flush the table")))?;
    drop(file);
    gix_fs::set_shared_repository_permissions(&temp.path, options.shared_repository_permissions)
        .map_err(|err| err.and_raise(message("Could not set the permissions of the table")))?;
    Ok(Some(Written { temp, min, max }))
}

/// Why replacing the list failed.
pub(super) enum Failure {
    /// The old list is in place, so nothing was published.
    NotPublished(gix_error::Error),
    /// The new list is or may be in place.
    Uncertain(gix_error::Error),
}

/// Write `names` as the new list into `lock`, flush it, and replace `tables.list` with it.
///
/// If that fails, look at the list to learn whether it was replaced, with `is_published` telling from the names it
/// finds whether it was this list. Not knowing is an [`UncertainAfterPublish`] naming `tables`.
pub(super) fn publish_list(
    dir: &Path,
    options: &Options,
    #[cfg(test)] inject: Option<Inject>,
    mut lock: gix_lock::File,
    names: &[String],
    tables: &[String],
    is_published: impl FnOnce(&[String]) -> bool,
) -> std::result::Result<(), Failure> {
    let mut list = Vec::new();
    for name in names {
        list.extend_from_slice(name.as_bytes());
        list.push(b'\n');
    }
    let fsync = options.fsync;
    lock.with_mut(|file| {
        file.write_all(&list)?;
        fsync.flush(file, Components::REFERENCE).map(|_| ())
    })
    .map_err(|err| Failure::NotPublished(err.and_raise(message("Could not write the new tables.list"))))?;

    let result = {
        #[cfg(test)]
        {
            let after_rename = |lock: gix_lock::File, unreadable: bool| -> std::result::Result<(), _> {
                lock.commit().map_err(|err| (err.error, None))?;
                if unreadable {
                    let list = dir.join(LIST_FILE_NAME);
                    std::fs::remove_file(&list).map_err(|err| (err, None))?;
                    std::fs::create_dir(&list).map_err(|err| (err, None))?;
                }
                Err((io::Error::other("injected failure after replacing the list"), None))
            };
            match inject {
                Some(Inject::AfterRename) => after_rename(lock, false),
                Some(Inject::AfterRenameWithUnreadableList) => after_rename(lock, true),
                Some(Inject::BeforeRename) => {
                    Err((io::Error::other("injected failure before replacing the list"), None))
                }
                None => lock.commit().map(|_| ()).map_err(|err| (err.error, Some(err.instance))),
            }
        }
        #[cfg(not(test))]
        {
            lock.commit().map(|_| ()).map_err(|err| (err.error, Some(err.instance)))
        }
    };
    // Keep the lock, which a failed commit returns, until it is known what happened, so that nobody changes the list meanwhile.
    let Err((err, _lock)) = result else {
        return Ok(());
    };

    // A list that can't be read tells nothing, whereas a missing one or one without the new tables says that
    // nothing was published. The first must not lead to removing tables that might be in use.
    let list_confirmed = match read_if_exists(&dir.join(LIST_FILE_NAME)) {
        Ok(Some(content)) => super::list::parse(&content).ok().map(|found| is_published(&found)),
        Ok(None) => Some(false),
        Err(_) => None,
    };
    let cause = err.and_raise(message("Could not replace tables.list"));
    match list_confirmed {
        Some(false) => Err(Failure::NotPublished(cause)),
        confirmed => Err(Failure::Uncertain(cause.and_raise(UncertainAfterPublish {
            tables: tables.to_vec(),
            list_confirmed: confirmed.is_some(),
        }))),
    }
}
