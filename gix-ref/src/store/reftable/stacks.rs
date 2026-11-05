//! The stacks of a repository, how references are routed to them, and the state that is shared between clones of a store.
use std::{collections::BTreeMap, path::Path};

use gix_error::{Result, ResultExt, message};
use gix_object::bstr::{BStr, BString, ByteSlice};
use gix_parallel::{Mutable, OwnShared};
use gix_reftable::{Stack, stack::Snapshot};

use crate::reftable::Store;

/// Identify one of the stacks of a repository.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StackId {
    /// The stack in the common directory, with the shared references and those of the main worktree.
    Main,
    /// The stack of the linked worktree this store is for.
    Worktree,
    /// The stack of the linked worktree with the given name.
    Other(BString),
}

/// A stack that is opened when it is needed first, and which is shared between clones of a store.
#[derive(Debug)]
pub(crate) struct Cell {
    dir: std::path::PathBuf,
    hash: gix_hash::Kind,
    stack: Mutable<Option<Stack>>,
}

impl Cell {
    /// The directory of the stack.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn new(dir: std::path::PathBuf, hash: gix_hash::Kind) -> Self {
        Cell {
            dir,
            hash,
            stack: Mutable::new(None),
        }
    }

    /// Return the tables of the stack, after looking for changes other writers made if `reload` is set. A stack that wasn't
    /// read yet is always read.
    pub fn snapshot(&self, reload: bool) -> Result<Snapshot> {
        let mut stack = gix_parallel::lock(&self.stack);
        match stack.as_mut() {
            Some(stack) => {
                if reload {
                    stack
                        .reload()
                        .or_raise(|| message("Could not reload the reftable stack").with("path", self.dir.as_path()))?;
                }
                Ok(stack.snapshot().clone())
            }
            None => {
                let opened = Stack::open(&self.dir, self.hash)
                    .or_raise(|| message("Could not read the reftable stack").with("path", self.dir.as_path()))?;
                let snapshot = opened.snapshot().clone();
                *stack = Some(opened);
                Ok(snapshot)
            }
        }
    }
}

impl Cell {
    /// Return `true` if the sizes of the tables of the stack violate the geometric sequence, so that `auto` compaction would
    /// merge some of them.
    pub fn compaction_required(&self) -> Result<bool> {
        let mut guard = gix_parallel::lock(&self.stack);
        match guard.as_mut() {
            Some(stack) => {
                stack
                    .reload()
                    .or_raise(|| message("Could not reload the reftable stack").with("path", self.dir.as_path()))?;
            }
            None => {
                *guard = Some(
                    Stack::open(&self.dir, self.hash)
                        .or_raise(|| message("Could not read the reftable stack").with("path", self.dir.as_path()))?,
                );
            }
        }
        Ok(guard
            .as_ref()
            .expect("the stack was opened above")
            .compaction_required())
    }

    /// Merge tables of the stack like `git pack-refs` does: the ones the geometric policy suggests if `auto` is set, all of them
    /// otherwise, and remove the tables that nothing lists anymore. Return the number of tables that were merged away.
    pub fn compact(&self, auto: bool) -> Result<usize> {
        let mut guard = gix_parallel::lock(&self.stack);
        match guard.as_mut() {
            Some(stack) => {
                stack
                    .reload()
                    .or_raise(|| message("Could not reload the reftable stack").with("path", self.dir.as_path()))?;
            }
            None => {
                *guard = Some(
                    Stack::open(&self.dir, self.hash)
                        .or_raise(|| message("Could not read the reftable stack").with("path", self.dir.as_path()))?,
                );
            }
        }
        let stack = guard.as_mut().expect("the stack was opened above");
        let compaction = if auto {
            stack.auto_compact()
        } else {
            stack.compact_all()
        }
        .or_raise(|| message("Could not compact the reftable stack").with("path", self.dir.as_path()))?;
        stack
            .clean()
            .or_raise(|| message("Could not remove unused reftables").with("path", self.dir.as_path()))?;
        Ok(compaction.map_or(0, |compaction| compaction.replaced.len()))
    }
}

/// All stacks a store may use.
#[derive(Debug)]
pub(crate) struct Stacks {
    main: OwnShared<Cell>,
    worktree: Option<OwnShared<Cell>>,
    others: Mutable<BTreeMap<BString, OwnShared<Cell>>>,
    common_dir: std::path::PathBuf,
    hash: gix_hash::Kind,
}

impl Stacks {
    pub fn new(git_dir: &Path, common_dir: Option<&Path>, hash: gix_hash::Kind) -> Self {
        let common = common_dir.unwrap_or(git_dir);
        Stacks {
            main: OwnShared::new(Cell::new(common.join("reftable"), hash)),
            worktree: common_dir.map(|_| OwnShared::new(Cell::new(git_dir.join("reftable"), hash))),
            others: Mutable::new(BTreeMap::new()),
            common_dir: common.to_owned(),
            hash,
        }
    }

    pub fn has_worktree_stack(&self) -> bool {
        self.worktree.is_some()
    }

    pub fn cell(&self, id: &StackId) -> Result<OwnShared<Cell>> {
        Ok(match id {
            StackId::Main => self.main.clone(),
            StackId::Worktree => self.worktree.clone().unwrap_or_else(|| self.main.clone()), // routing never asks for it
            StackId::Other(name) => {
                let mut others = gix_parallel::lock(&self.others);
                if let Some(cell) = others.get(name) {
                    return Ok(cell.clone());
                }
                let dir = self
                    .common_dir
                    .join("worktrees")
                    .join(gix_path::from_bstr(name.as_bstr())?)
                    .join("reftable");
                let cell = OwnShared::new(Cell::new(dir, self.hash));
                others.insert(name.clone(), cell.clone());
                cell
            }
        })
    }
}

fn is_root_ref_syntax(name: &[u8]) -> bool {
    name.iter().all(|b| b.is_ascii_uppercase() || *b == b'-' || *b == b'_')
}

fn is_per_worktree_ref(name: &[u8]) -> bool {
    [&b"refs/worktree/"[..], b"refs/bisect/", b"refs/rewritten/"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Return `true` if `name` is a reference that exists once per worktree, like `HEAD` or `refs/bisect/bad`.
pub(crate) fn is_current_worktree_ref(name: &[u8]) -> bool {
    is_root_ref_syntax(name) || is_per_worktree_ref(name)
}

impl Store {
    /// Return the stack that holds the reference `name` and the name it has there, like Git does:
    ///
    /// * `worktrees/<name>/<ref>` is `<ref>` in the stack of the linked worktree called `<name>`, if it is private to its
    ///   worktree, and `main-worktree/<ref>` is `<ref>` in the main stack under the same condition.
    /// * References private to a worktree are in its stack, which is the main one unless this is a linked worktree.
    /// * Everything else is in the main stack.
    pub(crate) fn route<'n>(&self, name: &'n BStr) -> (StackId, &'n BStr) {
        if let Some(rest) = name.strip_prefix(b"worktrees/") {
            match rest.find_byte(b'/') {
                None => return (StackId::Other(rest.into()), b"".as_bstr()),
                Some(pos) => {
                    let bare = &rest[pos + 1..];
                    if is_current_worktree_ref(bare) {
                        return (StackId::Other(rest[..pos].into()), bare.as_bstr());
                    }
                }
            }
        }
        if let Some(bare) = name.strip_prefix(b"main-worktree/")
            && is_current_worktree_ref(bare)
        {
            return (StackId::Main, bare.as_bstr());
        }
        if is_current_worktree_ref(name) && self.has_worktree_stack() {
            (StackId::Worktree, name)
        } else {
            (StackId::Main, name)
        }
    }

    pub(crate) fn has_worktree_stack(&self) -> bool {
        self.stacks.has_worktree_stack()
    }

    /// Merge the tables of the stack of this worktree, or of the main one if it has none of its own, which is what Git
    /// does to pack references.
    pub(crate) fn pack_refs_inner(&self, options: crate::pack_refs::Options) -> Result<crate::pack_refs::Outcome> {
        self.assure_backend_supported()?;
        let id = if self.stacks.has_worktree_stack() {
            StackId::Worktree
        } else {
            StackId::Main
        };
        Ok(crate::pack_refs::Outcome {
            merged_tables: self.stacks.cell(&id)?.compact(options.auto)?,
            ..Default::default()
        })
    }

    /// Return `true` if merging the tables of the stack of this worktree is due, which is what `--auto` decides.
    pub(crate) fn pack_refs_required_inner(&self) -> Result<bool> {
        self.assure_backend_supported()?;
        let id = if self.stacks.has_worktree_stack() {
            StackId::Worktree
        } else {
            StackId::Main
        };
        self.stacks.cell(&id)?.compaction_required()
    }

    pub(crate) fn stack_snapshot(&self, id: &StackId, reload: bool) -> Result<Snapshot> {
        self.assure_backend_supported()?;
        self.stacks.cell(id)?.snapshot(reload)
    }
}

#[cfg(test)]
mod tests {
    use gix_object::bstr::ByteSlice;

    use super::StackId;
    use crate::reftable::Store;

    fn route(linked: bool, name: &str) -> (StackId, String) {
        let store = if linked {
            Store::for_linked_worktree("wt".into(), "common".into(), gix_hash::Kind::Sha1)
        } else {
            Store::at("git".into(), gix_hash::Kind::Sha1)
        };
        let (id, bare) = store.route(name.as_bytes().as_bstr());
        (id, bare.to_string())
    }

    #[test]
    fn routing_follows_git() {
        use StackId::*;
        for (name, linked, expected) in [
            ("refs/heads/main", true, (Main, "refs/heads/main")),
            ("refs/tags/v1", false, (Main, "refs/tags/v1")),
            ("HEAD", true, (Worktree, "HEAD")),
            ("ORIG_HEAD", true, (Worktree, "ORIG_HEAD")),
            ("refs/bisect/bad", true, (Worktree, "refs/bisect/bad")),
            ("refs/worktree/x", true, (Worktree, "refs/worktree/x")),
            ("refs/rewritten/x", true, (Worktree, "refs/rewritten/x")),
            ("refs/bisect/bad", false, (Main, "refs/bisect/bad")),
            ("HEAD", false, (Main, "HEAD")),
            ("main-worktree/HEAD", true, (Main, "HEAD")),
            ("main-worktree/refs/bisect/bad", true, (Main, "refs/bisect/bad")),
            (
                "main-worktree/refs/heads/main",
                true,
                (Main, "main-worktree/refs/heads/main"),
            ),
            ("worktrees/w1/HEAD", true, (Other("w1".into()), "HEAD")),
            (
                "worktrees/w1/refs/bisect/bad",
                false,
                (Other("w1".into()), "refs/bisect/bad"),
            ),
            ("worktrees/w1/refs/heads/x", true, (Main, "worktrees/w1/refs/heads/x")),
            ("worktrees/w1", true, (Other("w1".into()), "")),
        ] {
            let (id, bare) = route(linked, name);
            assert_eq!((id, bare.as_str()), expected, "routing of {name} (linked: {linked})");
        }
    }
}
