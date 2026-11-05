//! What a client that is shallow asks of the receiver, which `git receive-pack` decides with `shallow.c`.
//!
//! The client sends the commits at its shallow boundary, whose parents only it is missing. A push that has history that leads
//! to one of them would make the receiver shallow there, which a receiver doesn't do unless it is told to, and the
//! commands that need it are rejected. Which commits are *needed* is what this module tells.
//!
//! * The lines of the client are sorted out before objects are received. Those that the receiver has already, and that aren't
//!   its own shallow commits, are the boundary that is "ours", and those it hasn't are "theirs", which are of interest if
//!   the pack brings them. A line that isn't a commit isn't of interest, as Git ignores blobs.
//! * A new tip needs the commits of the boundary that its history leads to, without going further than
//!   them, or beyond what any reference reaches, which includes `HEAD`, the references of all namespaces and hidden ones.
//!   Git paints the history from the tips and stops at the boundary, and then asks if what is reached is an ancestor of
//!   a reference, with only the shallow commits of the receiver as boundary. The walk from the tips returns what it reaches
//!   of the boundary without what references reach, and a second walk of what the receiver had already drops what a
//!   reference reaches through other commits of the boundary. What is new needs no second walk, as it can't be an
//!   ancestor of an old reference.
use std::collections::HashSet;

use gix_error::{ResultExt, message};
use gix_hash::{ObjectId, oid};
use gix_object::{Kind, WriteTo as _};

use crate::Repository;

/// The shallow commits that a client sent, sorted by whether the receiver had them before it received the objects.
pub(super) struct Boundary {
    ours: Vec<ObjectId>,
    theirs: Vec<ObjectId>,
    /// The shallow commits of the receiver, which are without parents like those of the client.
    own: HashSet<ObjectId>,
}

impl Boundary {
    /// Sort `lines`, the shallow commits of the client, with what the receiver `repo` has now, which is before it received
    /// a pack. A line that is an entry of the `shallow` file of `repo` already isn't new to it, but an entry that the receiver
    /// doesn't have as an object is, which makes it "theirs" when the pack brings it.
    pub(super) fn new(repo: &Repository, lines: &[ObjectId]) -> crate::Result<Self> {
        let own: HashSet<ObjectId> = repo
            .shallow_commits()
            .or_raise(|| message("Could not read the shallow file"))?
            .map_or_else(HashSet::new, |commits| commits.iter().copied().collect());
        let (mut ours, mut theirs) = (Vec::new(), Vec::new());
        let mut seen = HashSet::new();
        for &id in lines.iter().filter(|&&id| seen.insert(id)) {
            if !repo.has_object(id) {
                theirs.push(id);
            } else if !own.contains(&id) {
                ours.push(id);
            }
        }
        Ok(Boundary { ours, theirs, own })
    }

    /// Return what of the boundary is of interest after objects were received, which `repo` sees. What is "theirs" has to have
    /// arrived by now, and all of it has to be commits.
    pub(super) fn after_receiving(self, repo: &Repository) -> Candidates {
        let is_commit = |id: &ObjectId| repo.find_header(*id).is_ok_and(|header| header.kind() == Kind::Commit);
        let ours: HashSet<ObjectId> = self.ours.into_iter().filter(is_commit).collect();
        let mut ids: Vec<ObjectId> = ours
            .iter()
            .copied()
            .chain(self.theirs.into_iter().filter(is_commit))
            .collect();
        ids.sort();
        let mut roots = self.own.clone();
        roots.extend(ids.iter().copied());
        Candidates {
            ids,
            ours,
            own: self.own,
            roots,
        }
    }
}

/// The commits of the boundary of a client that history can need.
pub(super) struct Candidates {
    ids: Vec<ObjectId>,
    /// Those that the receiver had before the push.
    ours: HashSet<ObjectId>,
    /// The shallow commits of the receiver.
    own: HashSet<ObjectId>,
    /// Commits to treat as if they had no parents, which are the candidates and the shallow commits of the receiver.
    roots: HashSet<ObjectId>,
}

impl Candidates {
    pub(super) fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Return the candidates that the history of `tip` leads to without going through a reference, whose commits are `reachable`.
    /// `repo` sees all objects of the push.
    ///
    /// A tip that isn't a commit, as well as history that can't be walked, needs nothing, as what is wrong with it is told by
    /// the check for missing objects.
    pub(super) fn needed(&self, repo: &Repository, reachable: &[ObjectId], tip: ObjectId) -> Vec<ObjectId> {
        let Some(tip) = peel_to_commit(repo, tip) else {
            return Vec::new();
        };
        let objects = Roots {
            repo,
            roots: &self.roots,
        };
        let Ok(walk) = gix_traverse::commit::Simple::filtered([tip], objects, |_| true).hide(reachable.iter().copied())
        else {
            return Vec::new();
        };
        let mut needed = Vec::new();
        for info in walk {
            let Ok(info) = info else {
                return Vec::new();
            };
            if self.ids.binary_search(&info.id).is_ok() {
                needed.push(info.id);
            }
        }
        // What the receiver has might be reached by a reference through other commits of the boundary, which the walk above
        // doesn't pass, and which Git's test for that doesn't know of as it only applies the shallow commits of the receiver.
        let had: Vec<ObjectId> = needed.iter().copied().filter(|id| self.ours.contains(id)).collect();
        if !had.is_empty() {
            let objects = Roots { repo, roots: &self.own };
            let Ok(walk) = gix_traverse::commit::Simple::filtered(had.iter().copied(), objects, |_| true)
                .hide(reachable.iter().copied())
            else {
                return needed;
            };
            let mut not_reached = HashSet::new();
            for info in walk {
                // Like Git, which ignores what is missing there, what can't be walked is taken as not reached.
                let Ok(info) = info else {
                    return needed;
                };
                not_reached.insert(info.id);
            }
            needed.retain(|id| !self.ours.contains(id) || not_reached.contains(id));
        }
        needed
    }
}

fn peel_to_commit(repo: &Repository, mut id: ObjectId) -> Option<ObjectId> {
    loop {
        let object = repo.find_object(id).ok()?;
        match object.kind {
            Kind::Commit => return Some(id),
            Kind::Tag => id = object.try_into_tag().ok()?.target_id().ok()?.detach(),
            Kind::Tree | Kind::Blob => return None,
        }
    }
}

/// Finds objects like `repo` does, but makes the `roots` commits without parents.
struct Roots<'a> {
    repo: &'a Repository,
    roots: &'a HashSet<ObjectId>,
}

impl gix_object::Find for Roots<'_> {
    fn try_find<'a>(&self, id: &oid, buffer: &'a mut Vec<u8>) -> gix_error::Result<Option<gix_object::Data<'a>>> {
        if !self.roots.contains(id) {
            return self.repo.objects.try_find(id, buffer);
        }
        let Some(data) = self.repo.objects.try_find(id, buffer)? else {
            return Ok(None);
        };
        let (kind, object_hash) = (data.kind, data.object_hash);
        if kind.is_commit() {
            let mut commit = gix_object::CommitRef::from_bytes(data.data, object_hash)?;
            commit.parents.clear();
            let mut without_parents = Vec::new();
            commit
                .write_to(&mut without_parents)
                .or_raise(|| message("Could not write a commit without its parents"))?;
            drop(commit);
            *buffer = without_parents;
        }
        Ok(Some(gix_object::Data {
            kind,
            object_hash,
            data: buffer,
        }))
    }
}
