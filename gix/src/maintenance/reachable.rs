//! Finding the objects that something still refers to, the way Git's `prune` and `repack` do before they remove any.
//!
//! The starting points, called [roots](Root), are what Git's `mark_reachable_objects()` uses: all references, the `HEAD` of every
//! worktree, the objects of the index of every worktree, the objects that the reflogs of every worktree mention, and the commits
//! that an interrupted rebase has saved. [`Repository::reachable_objects()`] walks everything they lead to.
use std::path::PathBuf;

use gix_error::{Result, ResultExt, bail, message};
use gix_hash::ObjectId;
use gix_hashtable::HashSet;
use gix_object::{Find, bstr::BString};

use crate::Repository;

/// Why an object is a starting point of the search for reachable objects.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Origin {
    /// A reference like `refs/heads/main`, of the main worktree or of a linked one, which is named
    /// `worktrees/<id>/refs/bisect/bad` if it is private to a linked worktree that isn't the main one.
    Reference(BString),
    /// The `HEAD` of a worktree, which is `HEAD` for the main one and `worktrees/<id>/HEAD` for linked ones.
    Head(BString),
    /// One of the values in the reflog of the reference with the name that is held here, which is named like the one of
    /// [`Origin::Head`] for the `HEAD`s of linked worktrees.
    Reflog(BString),
    /// An entry or a cache-tree entry or an entry of the resolve-undo extension of the index of the worktree with the id held
    /// here, which is empty for the main worktree.
    Index(BString),
    /// The commit that a file of an interrupted rebase has saved.
    RebaseState(PathBuf),
    /// An object that is too new to be removed, or that somebody asked to keep, and what it leads to, which is kept for as
    /// long as it is the case even if the objects it leads to are older.
    Recent,
}

/// An object that is wanted, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// The object that is wanted.
    pub id: ObjectId,
    /// Why it is wanted.
    pub origin: Origin,
    /// The kind of `id` if it's known, which saves reading blobs that don't lead anywhere. Use `None` if it's not known.
    pub kind: Option<gix_object::Kind>,
}

impl Root {
    /// Return `true` if this root is allowed to point to objects that aren't there, and to lead to objects that aren't there.
    ///
    /// That's the case for what the reflogs and the index remember, which can mention objects that were removed on purpose or
    /// that never were in this repository, and for objects that are recent. Missing objects behind a reference, a `HEAD` and a saved rebase are an
    /// error, as they mean that the repository is damaged and what is reachable can't be told.
    pub fn is_optional(&self) -> bool {
        matches!(self.origin, Origin::Reflog(_) | Origin::Index(_) | Origin::Recent)
    }
}

/// The ids of all objects that were found to be reachable.
#[derive(Debug, Default, Clone)]
pub struct Reachable {
    ids: HashSet<ObjectId>,
}

impl Reachable {
    /// Return `true` if `id` is reachable.
    pub fn contains(&self, id: &gix_hash::oid) -> bool {
        self.ids.contains(id)
    }

    /// The amount of reachable objects.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Return `true` if no object is reachable.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Iterate over all reachable objects, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = &ObjectId> + '_ {
        self.ids.iter()
    }
}

/// Options for finding what is reachable.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// If `true`, the objects in the reflogs of all worktrees are roots as well, which is what Git's `prune` and `gc` do.
    ///
    /// Turning this off also drops everything that only a reflog remembers, like the older entries of `refs/stash`, so it
    /// is only safe if nothing but references and the index matter.
    ///
    /// Default `true`.
    pub reflogs: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { reflogs: true }
    }
}

impl Repository {
    /// Return all starting points for finding what is wanted in this repository, which is the main one and all linked worktrees,
    /// no matter which of them this handle is for.
    ///
    /// They are what Git's `mark_reachable_objects()` starts with, and more:
    ///
    /// * All references, except symbolic ones, whose targets are covered by the references they point to. A namespace of this
    ///   handle is ignored, as what the other namespaces refer to is wanted as well.
    /// * The `HEAD` of every worktree.
    /// * The references that are private to the linked worktrees, which Git only knows about through their reflogs.
    /// * The objects in the indices of all worktrees, including the trees remembered by the cache-tree extension and the
    ///   objects that the resolve-undo extension remembers, but not submodule commits. Git only takes regular files from the
    ///   latter, while symbolic links are kept here as well, which can only keep more.
    /// * The old and the new value of each reflog line of every worktree, if `options` say so.
    /// * The commits saved in `rebase-apply/` and `rebase-merge/` in each worktree.
    ///
    /// The roots are returned in the order of these steps, and are not deduplicated.
    ///
    /// Note that a reference that points to a missing object, and one that is symbolic and can't be resolved, is an error
    /// for the references that can't be missing, see [`Root::is_optional()`]. Dangling symbolic references are skipped, like Git does.
    pub fn reachability_roots(&self, options: Options) -> Result<Vec<Root>> {
        // A namespace limits what a handle sees, but everything that is referenced in any of them is wanted.
        let mut main = self.main_repo()?;
        main.clear_namespace();
        let mut roots = Vec::new();
        let mut worktrees = vec![(BString::default(), main.clone())];
        for proxy in main.worktrees()? {
            let id = proxy.id()?.to_owned();
            let mut worktree = proxy.into_repo_with_possibly_inaccessible_worktree()?;
            worktree.clear_namespace();
            worktrees.push((id, worktree));
        }

        // References: the shared ones from the main repository, and the private ones of each worktree that isn't the main one.
        let main_store = main.ref_store();
        let references = main_store.iter()?;
        for reference in references.all()? {
            let reference = reference?;
            if let Some(id) = reference.target.try_id() {
                roots.push(Root {
                    id: id.to_owned(),
                    origin: Origin::Reference(reference.name.as_bstr().to_owned()),
                    kind: None,
                });
            }
        }
        for (id, worktree) in &worktrees {
            let store = worktree.ref_store();
            if !id.is_empty() {
                let platform = store.iter()?;
                for prefix in ["refs/bisect/", "refs/rewritten/", "refs/worktree/"] {
                    let prefix = <&gix_path::RelativePath>::try_from(prefix).or_error()?;
                    for reference in platform.prefixed(prefix)? {
                        let reference = reference?;
                        if let Some(object_id) = reference.target.try_id() {
                            roots.push(Root {
                                id: object_id.to_owned(),
                                origin: Origin::Reference(prefixed_name(id, reference.name.as_bstr())),
                                kind: None,
                            });
                        }
                    }
                }
            }
            // `HEAD`, resolved if it is symbolic, as a branch that doesn't exist yet leaves nothing to keep.
            let head = match store.try_find_resolved("HEAD") {
                Ok(head) => head,
                Err(err) if err.is_not_found() => None,
                Err(err) => return Err(err),
            };
            if let Some(head) = head
                && let Some(object_id) = head.target.try_id()
            {
                roots.push(Root {
                    id: object_id.to_owned(),
                    origin: Origin::Head(prefixed_name(id, "HEAD".into())),
                    kind: None,
                });
            }
        }

        // The state of interrupted rebases, which is in the Git directory of each worktree.
        for (_, worktree) in &worktrees {
            for file in [
                "rebase-apply/autostash",
                "rebase-apply/orig-head",
                "rebase-merge/autostash",
                "rebase-merge/orig-head",
            ] {
                let path = worktree.git_dir().join(file);
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if let Ok(id) = ObjectId::from_hex(content.trim().as_bytes()) {
                    roots.push(Root {
                        id,
                        origin: Origin::RebaseState(path),
                        kind: None,
                    });
                }
            }
        }

        // The index of each worktree.
        for (id, worktree) in &worktrees {
            index_roots(id, worktree, &mut roots)?;
        }

        if options.reflogs {
            for name in main_store.reflog_names()? {
                let mut platform = main_store.reflog(name.as_ref());
                let Some(lines) = platform.all()? else {
                    continue;
                };
                for line in lines {
                    let line = line?;
                    for id in [line.previous_oid, line.new_oid] {
                        if !id.is_null() {
                            roots.push(Root {
                                id,
                                origin: Origin::Reflog(name.as_bstr().to_owned()),
                                kind: None,
                            });
                        }
                    }
                }
            }
        }
        Ok(roots)
    }

    /// Return all objects that are reachable from `roots`, which are those of [`reachability_roots()`](Self::reachability_roots())
    /// unless the caller adds to them, for example with objects that were written recently.
    ///
    /// Replacement objects are not used, as the objects that were replaced are the ones that are wanted. Commits at the shallow
    /// boundary don't have their parents followed, and entries of trees that are submodules are left alone.
    /// Missing objects are an error unless they are behind an [optional root](Root::is_optional()).
    pub fn reachable_objects(&self, roots: impl IntoIterator<Item = Root>) -> Result<Reachable> {
        let mut repo = self.clone();
        repo.objects.ignore_replacements = true;
        let shallow: HashSet<ObjectId> = repo
            .shallow_commits()?
            .map(|commits| commits.iter().copied().collect())
            .unwrap_or_default();
        let mut walk = Walk {
            repo: &repo,
            shallow,
            seen: HashSet::default(),
            buf: Vec::new(),
        };
        let (optional, required): (Vec<_>, Vec<_>) = roots.into_iter().partition(Root::is_optional);
        for root in required {
            walk.run(root.id, root.kind, false).or_raise(|| {
                message("Could not find everything that a root leads to").with("origin", format!("{:?}", root.origin))
            })?;
        }
        for root in optional {
            walk.run(root.id, root.kind, true)?;
        }
        Ok(Reachable { ids: walk.seen })
    }
}

fn prefixed_name(worktree: &BString, name: &gix_object::bstr::BStr) -> BString {
    if worktree.is_empty() {
        name.to_owned()
    } else {
        let mut out = BString::from("worktrees/");
        out.extend_from_slice(worktree);
        out.push(b'/');
        out.extend_from_slice(name);
        out
    }
}

fn index_roots(worktree_id: &BString, repo: &Repository, roots: &mut Vec<Root>) -> Result<()> {
    if !repo.index_path().is_file() {
        return Ok(());
    }
    let index = repo.open_index().or_raise(|| {
        message("Could not read the index to find the objects it needs").with("worktree", worktree_id.clone())
    })?;
    let origin = || Origin::Index(worktree_id.clone());
    for entry in index.entries() {
        if entry.mode.is_submodule() {
            continue;
        }
        roots.push(Root {
            id: entry.id,
            origin: origin(),
            // Sparse directories are trees, everything else that isn't a submodule is a blob.
            kind: Some(if entry.mode.is_sparse() {
                gix_object::Kind::Tree
            } else {
                gix_object::Kind::Blob
            }),
        });
    }
    if let Some(tree) = index.tree() {
        fn add(tree: &gix_index::extension::Tree, roots: &mut Vec<Root>, origin: &dyn Fn() -> Origin) {
            if tree.num_entries.is_some() {
                roots.push(Root {
                    id: tree.id,
                    origin: origin(),
                    kind: Some(gix_object::Kind::Tree),
                });
            }
            for child in &tree.children {
                add(child, roots, origin);
            }
        }
        add(tree, roots, &origin);
    }
    if let Some(paths) = index.resolve_undo() {
        for path in paths {
            for stage in path.stages().iter().flatten() {
                if stage.mode() & 0o170000 != 0o160000 {
                    roots.push(Root {
                        id: stage.id().to_owned(),
                        origin: origin(),
                        kind: Some(gix_object::Kind::Blob),
                    });
                }
            }
        }
    }
    Ok(())
}

struct Walk<'a> {
    repo: &'a Repository,
    shallow: HashSet<ObjectId>,
    seen: HashSet<ObjectId>,
    buf: Vec<u8>,
}

impl Walk<'_> {
    /// Mark `start` and everything it leads to as seen. If `tolerate_missing` is set, objects that aren't there are skipped.
    fn run(&mut self, start: ObjectId, start_kind: Option<gix_object::Kind>, tolerate_missing: bool) -> Result<()> {
        // The kind of an object is known if it comes from a tree, and it may be known for roots.
        let mut stack: Vec<(ObjectId, Option<gix_object::Kind>)> = vec![(start, start_kind)];
        while let Some((id, kind)) = stack.pop() {
            if self.seen.contains(&id) {
                continue;
            }
            if kind == Some(gix_object::Kind::Blob) {
                // Blobs don't lead anywhere, and Git doesn't check that they exist either.
                self.seen.insert(id);
                continue;
            }
            let data = match self.repo.objects.try_find(&id, &mut self.buf)? {
                Some(data) => data,
                None if tolerate_missing => continue,
                None => bail!(message("An object that is wanted is missing").with("object_id", id.to_string())),
            };
            self.seen.insert(id);
            let hash_kind = id.kind();
            match data.kind {
                gix_object::Kind::Blob => {}
                gix_object::Kind::Tag => {
                    let target = gix_object::TagRefIter::from_bytes(data.data, hash_kind).target_id()?;
                    stack.push((target, None));
                }
                gix_object::Kind::Commit => {
                    let mut iter = gix_object::CommitRefIter::from_bytes(data.data, hash_kind);
                    let tree = iter.tree_id()?;
                    stack.push((tree, Some(gix_object::Kind::Tree)));
                    if !self.shallow.contains(&id) {
                        stack.extend(iter.parent_ids().map(|parent| (parent, Some(gix_object::Kind::Commit))));
                    }
                }
                gix_object::Kind::Tree => {
                    for entry in gix_object::TreeRefIter::from_bytes(data.data, hash_kind) {
                        let entry = entry?;
                        let kind = match entry.mode.kind() {
                            gix_object::tree::EntryKind::Commit => continue,
                            gix_object::tree::EntryKind::Tree => gix_object::Kind::Tree,
                            _ => gix_object::Kind::Blob,
                        };
                        if !self.seen.contains(entry.oid) {
                            stack.push((entry.oid.to_owned(), Some(kind)));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
