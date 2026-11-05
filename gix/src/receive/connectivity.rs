//! Check that everything the new tips of a push need is there, which is what `git receive-pack` asks `git rev-list
//! --objects --stdin --not --exclude-hidden=receive --all --alternate-refs --quiet` for, but without a process.
//!
//! References are assumed to be complete, so that their history is where checking stops. The rules, which were settled by probing
//! Git 2.56 with broken repositories, are:
//!
//! * A commit that can be reached from a reference that isn't hidden is never looked at.
//! * Commits that can't must exist and parse, and so must their trees and all trees below them.
//! * Blobs only need to exist, and neither do the contents of gitlinks matter.
//! * What a parent of a new commit that isn't new itself has in its trees, is known to be there, which is not checked either.
//! * Tags, including those of tags, must exist and parse, and what they point to is checked as if it was a tip.
//! * A reference whose object is missing is an error for all pushes.
//!
//! Two differences to Git make the check stricter, and show only if objects behind references are missing already: it marks
//! the trees of the parents of new commits as known but not those of all references, and it doesn't treat the `HEAD` of
//! linked worktrees as a reference that ends the check. The references of alternates don't include their `HEAD`, as in Git.
//!
//! Hidden references (`receive.hideRefs` or `transfer.hideRefs`) are what the caller says they are, and `HEAD` is
//! never hidden. The references of alternate repositories count like those of the repository itself.
use std::collections::HashSet;

use gix_hash::ObjectId;
use gix_object::{Kind, bstr::BStr};

use crate::Repository;

/// The reason for which a push is not connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disconnected {
    /// The object that is missing or unreadable, if it could be determined.
    pub id: Option<ObjectId>,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for Disconnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Disconnected {}

fn disconnected(id: impl Into<Option<ObjectId>>, message: impl Into<String>) -> Disconnected {
    Disconnected {
        id: id.into(),
        message: message.into(),
    }
}

/// The commits whose history is assumed to be complete, which is where the check of a push stops.
#[derive(Debug, Clone, Default)]
pub struct Cutoffs {
    commits: Vec<ObjectId>,
}

impl Cutoffs {
    /// Collect the commits that `HEAD` and all references of `repo` lead to, except for the references that `is_hidden` says are hidden,
    /// and those of all alternates of `repo` that are repositories themselves.
    ///
    /// Fail if a reference can't be read or leads to an object that is missing, as Git does for all pushes in that case.
    pub fn collect(repo: &Repository, is_hidden: &dyn Fn(&BStr) -> bool) -> crate::Result<Self> {
        use gix_error::{ResultExt, message};
        let mut cutoffs = Cutoffs::default();
        cutoffs.add_references(repo, is_hidden, true)?;
        let alternates = gix_odb::alternate::resolve(repo.objects.store_ref().path().to_owned(), repo.current_dir())
            .or_raise(|| message("Could not resolve the alternates of the object database"))?;
        for alternate in alternates {
            // Only alternates that are the `objects` directory of a repository have references to learn about.
            let Some(git_dir) = alternate.parent().filter(|_| alternate.ends_with("objects")) else {
                continue;
            };
            let Ok(alternate_repo) = crate::open_opts(git_dir, crate::open::Options::isolated()) else {
                continue;
            };
            cutoffs.add_references(&alternate_repo, &|_| false, false)?;
        }
        cutoffs.commits.sort();
        cutoffs.commits.dedup();
        Ok(cutoffs)
    }

    /// Collect the commits that `HEAD` and all references of `repo` lead to, including the hidden ones and those of other namespaces,
    /// and not those of alternates, except for the references that `skip` says to leave out. These are the references that Git
    /// decides with whether a shallow update is needed.
    ///
    /// Like Git does there, skip what can't be read or isn't a commit.
    pub(super) fn of_all_references(repo: &Repository, skip: &dyn Fn(&BStr) -> bool) -> Self {
        let mut ids = Vec::new();
        if let Some(id) = repo.head().ok().and_then(|head| head.id()) {
            ids.push(id.detach());
        }
        if let Ok(platform) = repo.references()
            && let Ok(all) = platform.all()
        {
            ids.extend(
                all.flatten()
                    .filter(|reference| !skip(reference.name().as_bstr()))
                    .filter_map(|reference| reference.try_id().map(crate::Id::detach)),
            );
        }
        let mut cutoffs = Cutoffs::default();
        for id in ids {
            cutoffs.add_object(repo, id).ok();
        }
        cutoffs.commits.sort();
        cutoffs.commits.dedup();
        cutoffs
    }

    /// The commits that are the cutoffs.
    pub(super) fn commits(&self) -> &[ObjectId] {
        &self.commits
    }

    fn add_references(
        &mut self,
        repo: &Repository,
        is_hidden: &dyn Fn(&BStr) -> bool,
        include_head: bool,
    ) -> crate::Result<()> {
        use gix_error::{ResultExt, message};
        let mut ids = Vec::new();
        // Git lists the references of alternates, which doesn't include their `HEAD`.
        if include_head && let Some(id) = repo.head().or_raise(|| message("Could not read HEAD"))?.id() {
            ids.push(id.detach());
        }
        for reference in repo
            .references()
            .or_raise(|| message("Could not read the references"))?
            .all()
            .or_raise(|| message("Could not read the references"))?
        {
            let reference = reference.or_raise(|| message("Could not read a reference"))?;
            if is_hidden(reference.name().as_bstr()) {
                continue;
            }
            // Symbolic references lead to what other references have.
            if let Some(id) = reference.try_id() {
                ids.push(id.detach());
            }
        }
        for id in ids {
            self.add_object(repo, id)
                .or_raise(|| message!("Reference to {id} is broken"))?;
        }
        Ok(())
    }

    /// Peel `id` until it is a commit, and remember it. Objects that aren't commits don't cut anything off.
    fn add_object(&mut self, repo: &Repository, mut id: ObjectId) -> crate::Result<()> {
        use gix_error::{ResultExt, message};
        loop {
            let object = repo.find_object(id).or_raise(|| message!("Object {id} is missing"))?;
            match object.kind {
                Kind::Commit => {
                    self.commits.push(id);
                    return Ok(());
                }
                Kind::Tag => {
                    id = object
                        .try_into_tag()
                        .or_raise(|| message!("Tag {id} is invalid"))?
                        .target_id()
                        .or_raise(|| message!("Tag {id} is invalid"))?
                        .detach();
                }
                Kind::Tree | Kind::Blob => return Ok(()),
            }
        }
    }
}

/// Return `Ok(())` if everything that the history of `tips` needs, which are the new values of references, exists in `repo`, or
/// else what's missing. `repo` must see the objects that were received, and the objects that were there before.
///
/// `cutoffs` are what the references of the repository lead to.
pub fn verify(repo: &Repository, cutoffs: &Cutoffs, tips: &[ObjectId]) -> Result<(), Disconnected> {
    verify_with_roots(repo, cutoffs, tips, None)
}

/// Like [`verify()`], but with the commits in `roots`, if there are any, taken for what has no parents, in addition to those that
/// are the shallow commits of `repo`. That is what a shallow file that is about to be written makes of them.
pub(super) fn verify_with_roots(
    repo: &Repository,
    cutoffs: &Cutoffs,
    tips: &[ObjectId],
    roots: Option<&HashSet<ObjectId>>,
) -> Result<(), Disconnected> {
    let mut commit_tips = Vec::new();
    let mut tree_tips = Vec::new();
    for &tip in tips {
        let mut id = tip;
        loop {
            let object = repo
                .find_object(id)
                .map_err(|err| disconnected(id, format!("Object {id} is missing or unreadable: {err}")))?;
            match object.kind {
                Kind::Commit => {
                    commit_tips.push(id);
                    break;
                }
                Kind::Tree => {
                    tree_tips.push(id);
                    break;
                }
                Kind::Blob => break,
                Kind::Tag => {
                    id = object
                        .try_into_tag()
                        .map_err(|err| disconnected(id, format!("Tag {id} is invalid: {err}")))?
                        .target_id()
                        .map_err(|err| disconnected(id, format!("Tag {id} is invalid: {err}")))?
                        .detach();
                }
            }
        }
    }

    // What is new is what history is left after cutting off what references lead to.
    let mut new_commits = Vec::new();
    let mut new_ids = HashSet::new();
    if !commit_tips.is_empty() {
        let mut all_roots = roots.cloned().unwrap_or_default();
        all_roots.extend(
            repo.shallow_commits()
                .map_err(|err| disconnected(None, format!("Could not read the shallow file: {err}")))?
                .iter()
                .flat_map(|commits| commits.iter().copied()),
        );
        // The walk doesn't read what Git doesn't, so that flawed identities are a matter of checking objects only.
        let walk = super::shallow::walk_without_parents(repo, commit_tips, cutoffs.commits.iter().copied(), all_roots)
            .map_err(|err| disconnected(None, format!("Could not start to walk the history: {err}")))?
            .map(|info| info.map_err(|err| err.to_string()));
        for info in walk {
            let info = info.map_err(|err| disconnected(None, format!("History is incomplete: {err}")))?;
            new_ids.insert(info.id);
            new_commits.push((info.id, info.parent_ids.iter().copied().collect::<Vec<_>>()));
        }
    }

    // What the parents that aren't new have is there, and it is not checked for being there once more.
    // A parent that isn't new is a cutoff, which references reach and which is there, or it is behind a shallow commit and not
    // there, which is why a parent that can't be read is skipped here: nothing below it is walked either.
    let mut known = HashSet::<ObjectId>::new();
    let mut edges = HashSet::new();
    for (_, parents) in &new_commits {
        for parent in parents.iter().filter(|parent| !new_ids.contains(*parent)) {
            if edges.insert(*parent)
                && let Some(tree_id) = repo
                    .find_commit(*parent)
                    .ok()
                    .and_then(|commit| commit.tree_id().ok())
                    .map(crate::Id::detach)
            {
                mark_known(repo, tree_id, &mut known);
            }
        }
    }

    let mut checked = HashSet::<ObjectId>::new();
    for (commit_id, _) in &new_commits {
        let commit = repo.find_commit(*commit_id).map_err(|err| {
            disconnected(
                *commit_id,
                format!("Commit {commit_id} is missing or unreadable: {err}"),
            )
        })?;
        let tree_id = commit
            .tree_id()
            .map_err(|err| disconnected(*commit_id, format!("Commit {commit_id} is invalid: {err}")))?
            .detach();
        check_tree(repo, tree_id, &known, &mut checked)?;
    }
    for tree_id in tree_tips {
        check_tree(repo, tree_id, &known, &mut checked)?;
    }
    Ok(())
}

/// Remember `tree_id` and everything below it as known, ignoring what can't be read.
fn mark_known(repo: &Repository, tree_id: ObjectId, known: &mut HashSet<ObjectId>) {
    let mut stack = vec![tree_id];
    while let Some(tree_id) = stack.pop() {
        if !known.insert(tree_id) {
            continue;
        }
        let Ok(tree) = repo.find_tree(tree_id) else { continue };
        for entry in tree.iter().flatten() {
            let mode = entry.mode();
            if mode.is_commit() {
                continue;
            }
            if mode.is_tree() {
                stack.push(entry.oid().to_owned());
            } else {
                known.insert(entry.oid().to_owned());
            }
        }
    }
}

/// Check that the tree at `tree_id`, its trees and all their blobs are there, other than those that are `known`.
fn check_tree(
    repo: &Repository,
    tree_id: ObjectId,
    known: &HashSet<ObjectId>,
    checked: &mut HashSet<ObjectId>,
) -> Result<(), Disconnected> {
    let mut stack = vec![tree_id];
    while let Some(tree_id) = stack.pop() {
        if known.contains(&tree_id) || !checked.insert(tree_id) {
            continue;
        }
        let tree = repo
            .find_tree(tree_id)
            .map_err(|err| disconnected(tree_id, format!("Tree {tree_id} is missing or unreadable: {err}")))?;
        for entry in tree.iter() {
            let entry = entry.map_err(|err| disconnected(tree_id, format!("Tree {tree_id} is invalid: {err}")))?;
            let mode = entry.mode();
            let id = entry.oid().to_owned();
            if mode.is_commit() || known.contains(&id) {
                continue;
            }
            if mode.is_tree() {
                stack.push(id);
            } else if checked.insert(id) && !repo.has_object(id) {
                return Err(disconnected(id, format!("Blob {id} is missing")));
            }
        }
    }
    Ok(())
}
