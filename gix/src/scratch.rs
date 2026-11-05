//! Directories in which something is made before it is moved into place.

/// A directory in `parent` that is removed when it goes out of scope, in which something is made before it is moved into place.
/// A process that is killed leaves it, and it is named `tmp_pack_dir_*` so that `Repository::prune()` finds it, which removes it once
/// it is older than the expiry, like Git's `tmp_pack_*` files.
///
/// Its modification time is that of the last file that was made in it, so that what is still being written isn't old.
pub(crate) struct Scratch(std::path::PathBuf);

impl Scratch {
    pub(crate) fn new(parent: &std::path::Path) -> crate::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};

        use gix_error::{ResultExt, message};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos());
        let path = parent.join(format!(
            "tmp_pack_dir_{}_{nanos}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).or_raise(|| message!("Could not create {}", path.display()))?;
        Ok(Scratch(path))
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[cfg(test)]
mod scratch_tests {
    use super::Scratch;

    #[test]
    fn the_directory_is_named_for_pruning_and_goes_with_its_owner() {
        let tmp = gix_testtools::tempfile::tempdir().expect("temp dir");
        let scratch = Scratch::new(tmp.path()).expect("created");
        let path = scratch.path().to_owned();
        assert!(
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tmp_pack_dir_")),
            "pruning removes `tmp_*` of the pack directory that are old: {path:?}"
        );
        assert!(path.is_dir());
        drop(scratch);
        assert!(!path.exists(), "nothing is left when it isn't killed");
    }
}
