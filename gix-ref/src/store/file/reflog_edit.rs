//! Removing reflogs and some of their lines.
use gix_error::{ErrorExt, Result, ResultExt, message};
use gix_hash::ObjectId;
use gix_object::bstr::ByteSlice;

use crate::{
    FullNameRef,
    log::{Line, expire},
    store_impl::{
        file,
        file::{log, transaction::prepare::reference_path_and_lock_boundary},
    },
};

impl file::Store {
    /// Remove the reflog of the reference `name` along with the directories that were only there to hold it, and return
    /// `true` if there was a reflog.
    pub(crate) fn reflog_delete_inner(&self, name: &FullNameRef) -> Result<bool> {
        let (root, relative_name) = self.reflog_base_and_relative_path(name)?;
        self.assure_backend_supported()?;
        let path = root.join(relative_name);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                gix_fs::dir::remove::empty_upward_until_boundary(path.parent().expect("never without parent"), &root)
                    .ok();
                Ok(true)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err.and_raise(message("Could not delete reflog").with("reference", name.as_bstr()))),
        }
    }

    /// Remove the lines of the reflog of `name` for which `keep` returns `false`, from the oldest to the most recent one,
    /// along with the current value of the reference, which `current` reads once the reference is locked.
    ///
    /// Like Git, this locks the reference as well as its reflog, so that nobody appends lines that would be lost, and leaves
    /// an empty reflog if nothing remains.
    pub(crate) fn reflog_expire_inner(
        &self,
        name: &FullNameRef,
        options: expire::Options,
        current: &dyn Fn() -> Result<Option<ObjectId>>,
        keep: &mut dyn FnMut(&Line, Option<&gix_hash::oid>) -> bool,
    ) -> Result<expire::Outcome> {
        let path = self.reflog_path(name)?;
        let error = || message("Could not expire reflog").with("reference", name.as_bstr());
        let ref_lock = if options.dry_run {
            None
        } else {
            let (reference_path, boundary) = reference_path_and_lock_boundary(self, name)?;
            Some(
                gix_lock::Marker::acquire_to_hold_resource(&reference_path, options.lock, Some(boundary), 0)
                    .or_raise(error)?,
            )
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(expire::Outcome::default()),
            Err(err) => return Err(err.and_raise(error().with("path", path))),
        };
        let current = current()?;
        let (kept_bytes, outcome) = {
            let mut lines = Vec::new();
            for (raw, parsed) in bytes.lines_with_terminator().zip(log::iter::forward(&bytes)) {
                lines.push((raw, Line::from(parsed.or_raise(error)?)));
            }
            let mut removed = vec![false; lines.len()];
            for idx in 0..lines.len() {
                removed[idx] = !keep(&lines[idx].1, current.as_deref());
            }
            let mut kept_bytes = Vec::with_capacity(bytes.len());
            for ((raw, _), removed) in lines.iter().zip(&removed) {
                if !removed {
                    kept_bytes.extend_from_slice(raw);
                }
            }
            let removed = removed.iter().filter(|removed| **removed).count();
            (
                kept_bytes,
                expire::Outcome {
                    removed,
                    kept: lines.len() - removed,
                },
            )
        };
        if options.dry_run || outcome.removed == 0 {
            return Ok(outcome);
        }
        let (root, _) = self.reflog_base_and_relative_path(name)?;
        let mut log_lock =
            gix_lock::File::acquire_to_update_resource(&path, options.lock, Some(root), 0).or_raise(error)?;
        {
            use std::io::Write;
            log_lock.with_mut(|file| file.write_all(&kept_bytes)).or_raise(error)?;
        }
        log_lock.commit().map_err(|err| err.error).or_raise(error)?;
        drop(ref_lock);
        Ok(outcome)
    }
}
