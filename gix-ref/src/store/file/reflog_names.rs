//! Listing the references that have a reflog.
use std::{collections::BTreeSet, path::Path};

use gix_error::{ErrorExt, Result, ResultExt, message};
use gix_object::bstr::{BString, ByteSlice};

use crate::{FullName, store_impl::file};

impl file::Store {
    /// Return the names of all references that have a reflog, even an empty one, in the order of their names.
    ///
    /// See [`StoreRef::reflog_names()`](crate::StoreRef::reflog_names()) for which reflogs these are.
    pub(crate) fn reflog_names_inner(&self) -> Result<Vec<FullName>> {
        self.assure_backend_supported()?;
        let common_dir = self.common_dir_resolved();
        let is_linked = self.common_dir().is_some();
        let mut names = BTreeSet::new();

        // The reflogs of the references that all worktrees share, and in the main worktree also its own.
        for name in names_below(&common_dir.join("logs"))? {
            let private = is_private(&name);
            if is_linked && private {
                continue; // these belong to the main worktree, which Git doesn't list from a linked one either.
            }
            names.insert(name);
        }
        if is_linked {
            for name in names_below(&self.git_dir().join("logs"))? {
                if is_private(&name) {
                    names.insert(name);
                }
            }
        } else {
            // The main worktree is the only one who sees the reflogs of the linked ones.
            let worktrees = common_dir.join("worktrees");
            let entries = match std::fs::read_dir(&worktrees) {
                Ok(entries) => Some(entries),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => {
                    return Err(err.and_raise(message("Could not list linked worktrees").with("path", worktrees)));
                }
            };
            for entry in entries.into_iter().flatten() {
                let entry = entry.or_raise(|| {
                    message("Could not read an entry of the linked worktrees").with("path", worktrees.as_path())
                })?;
                let id = gix_path::os_str_into_bstr(&entry.file_name())
                    .map_err(|_| message("A linked worktree has a name that isn't valid UTF-8"))?
                    .to_owned();
                for name in names_below(&entry.path().join("logs"))? {
                    if is_private(&name) {
                        let mut prefixed = BString::from("worktrees/");
                        prefixed.extend_from_slice(id.as_bytes());
                        prefixed.push(b'/');
                        prefixed.extend_from_slice(name.as_bstr());
                        if let Ok(name) = FullName::try_from(prefixed) {
                            names.insert(name);
                        }
                    }
                }
            }
        }
        Ok(names.into_iter().collect())
    }
}

/// Return `true` if `name` exists once per worktree, like `HEAD` or `refs/bisect/bad`.
fn is_private(name: &FullName) -> bool {
    name.as_ref()
        .category()
        .is_some_and(|category| category.is_worktree_private())
}

/// Return the names of the references whose reflogs are files below `logs`, which doesn't have to exist.
/// Files whose names aren't valid reference names are skipped, like Git does.
fn names_below(logs: &Path) -> Result<Vec<FullName>> {
    fn walk(dir: &Path, prefix: &[u8], out: &mut Vec<FullName>) -> Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.and_raise(message("Could not list reflogs").with("path", dir))),
        };
        for entry in entries {
            let entry = entry.or_raise(|| message("Could not read an entry of the reflogs").with("path", dir))?;
            let Ok(file_name) = gix_path::os_str_into_bstr(&entry.file_name()).map(ToOwned::to_owned) else {
                continue;
            };
            let mut name = BString::from(prefix);
            name.extend_from_slice(file_name.as_bytes());
            let file_type = entry
                .file_type()
                .or_raise(|| message("Could not determine the type of a reflog").with("path", entry.path()))?;
            if file_type.is_dir() {
                name.push(b'/');
                walk(&entry.path(), &name, out)?;
            } else if let Ok(name) = FullName::try_from(name) {
                out.push(name);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(logs, b"", &mut out)?;
    Ok(out)
}
