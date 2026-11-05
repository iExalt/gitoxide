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
use std::{borrow::Cow, cell::RefCell, collections::HashSet, io::Write as _};

use gix_error::{ErrorExt as _, ResultExt, message};
use gix_hash::{ObjectId, oid};
use gix_object::{
    Kind, WriteTo as _,
    bstr::{BStr, BString, ByteSlice as _},
};

use super::{
    connectivity::{self, Cutoffs},
    policy::{Command, Messages},
};
use crate::Repository;

/// The shallow commits that a client sent, sorted by whether the receiver had them before it received the objects.
pub(super) struct Boundary {
    /// What the receiver has and doesn't have, in the order of the lines of the client.
    lines: Vec<(ObjectId, Had)>,
    /// The shallow commits of the receiver, which are without parents like those of the client.
    own: HashSet<ObjectId>,
    /// What the `shallow` file of the receiver was when it was read, which is to be what it is when it is updated.
    file: Option<Vec<u8>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Had {
    Ours,
    Theirs,
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
        let file = std::fs::read(
            repo.shallow_file()
                .or_raise(|| message("Could not find the shallow file"))?,
        )
        .ok();
        let mut sorted = Vec::new();
        // A line that is there twice is two entries, as in Git, which then write it twice.
        for &id in lines {
            if !repo.has_object(id) {
                sorted.push((id, Had::Theirs));
            } else if !own.contains(&id) {
                sorted.push((id, Had::Ours));
            }
        }
        Ok(Boundary {
            lines: sorted,
            own,
            file,
        })
    }

    /// Return what of the boundary is of interest after objects were received, which `repo` sees. What is "theirs" has to have
    /// arrived by now, and all of it has to be commits.
    pub(super) fn after_receiving(self, repo: &Repository) -> Candidates {
        let is_commit = |id: &ObjectId| repo.find_header(*id).is_ok_and(|header| header.kind() == Kind::Commit);
        // What Git keeps asking `shallow_update` for is if any of it is there, whatever it is.
        let present: Vec<_> = self
            .lines
            .into_iter()
            .filter(|(id, had)| *had == Had::Ours || repo.has_object(*id))
            .collect();
        let matters = !present.is_empty();
        let order: Vec<ObjectId> = present.iter().map(|(id, _)| *id).filter(is_commit).collect();
        let ours: HashSet<ObjectId> = present
            .iter()
            .filter(|(id, had)| *had == Had::Ours && order.contains(id))
            .map(|(id, _)| *id)
            .collect();
        let mut ids = order.clone();
        ids.sort();
        ids.dedup();
        let mut roots = self.own.clone();
        roots.extend(ids.iter().copied());
        Candidates {
            ids,
            order,
            ours,
            own: self.own,
            roots,
            matters,
            file: self.file,
        }
    }
}

/// The commits of the boundary of a client that history can need.
pub(super) struct Candidates {
    ids: Vec<ObjectId>,
    /// The same in the order of the client.
    order: Vec<ObjectId>,
    /// Whether any line of the client is an object that the receiver has, which is what makes Git take a push for one that
    /// needs shallow updates to be looked at, and tell hooks about it.
    matters: bool,
    file: Option<Vec<u8>>,
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

    /// Whether any of what the client said is there, even if it isn't a commit.
    pub(super) fn matters(&self) -> bool {
        self.matters
    }

    /// Return the candidates that the history of `tip` leads to without going through a reference, whose commits are `reachable`.
    /// `repo` sees all objects of the push.
    ///
    /// A tip that isn't a commit, as well as history that can't be walked, needs nothing, as what is wrong with it is told by
    /// the check for missing objects.
    pub(super) fn needed(&self, repo: &Repository, reachable: &[ObjectId], tip: ObjectId) -> Vec<ObjectId> {
        self.needed_with(repo, reachable, tip, &[])
    }

    /// Like [`Self::needed()`], with the commits of `registered` as shallow commits of the receiver as well, and in the order
    /// in which the client said them.
    pub(super) fn needed_with(
        &self,
        repo: &Repository,
        reachable: &[ObjectId],
        tip: ObjectId,
        registered: &[ObjectId],
    ) -> Vec<ObjectId> {
        let Some(tip) = peel_to_commit(repo, tip) else {
            return Vec::new();
        };
        let (roots, own) = if registered.is_empty() {
            (Cow::Borrowed(&self.roots), Cow::Borrowed(&self.own))
        } else {
            let mut roots = self.roots.clone();
            roots.extend(registered.iter().copied());
            let mut own = self.own.clone();
            own.extend(registered.iter().copied());
            (Cow::Owned(roots), Cow::Owned(own))
        };
        let objects = Roots { repo, roots };
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
            let objects = Roots { repo, roots: own };
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
        let needed: HashSet<ObjectId> = needed.into_iter().collect();
        self.order.iter().copied().filter(|id| needed.contains(id)).collect()
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
    roots: Cow<'a, HashSet<ObjectId>>,
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

/// Walk the history of `tips` without what `hidden` leads to, where the commits in `roots` have no parents.
pub(super) fn walk_without_parents(
    repo: &Repository,
    tips: Vec<ObjectId>,
    hidden: impl IntoIterator<Item = ObjectId>,
    roots: HashSet<ObjectId>,
) -> gix_error::Result<impl Iterator<Item = gix_error::Result<gix_traverse::commit::Info>> + '_> {
    let objects = Roots {
        repo,
        roots: Cow::Owned(roots),
    };
    gix_traverse::commit::Simple::filtered(tips, objects, |_| true).hide(hidden)
}

/// Takes the commits of the boundary of a client that references are to need into the `shallow` file as the commands that need them
/// are updated, which is what `receive.shallowUpdate` is for.
///
/// Git does that for a command by locking the file, writing what it has and what the command needs, and then checking that the
/// command is connected with that file, which it keeps if it is. It has a bug that makes it check another command or none, and drop
/// the commands that follow, which is not copied: each command is checked itself.
pub(super) struct Update<'a> {
    repo: &'a Repository,
    candidates: Candidates,
    /// The names of the commands that are to go through [`Self::apply()`].
    pending: HashSet<BString>,
    state: RefCell<State>,
}

struct State {
    /// What references reach, which is looked at once as Git does, when the first command is updated.
    cutoffs: Option<Cutoffs>,
    /// What was added to the file, in this push.
    registered: Vec<ObjectId>,
    /// What the file has, if it is there.
    file: Option<Vec<u8>>,
}

impl<'a> Update<'a> {
    pub(super) fn new(repo: &'a Repository, candidates: Candidates, pending: HashSet<BString>) -> Self {
        let file = candidates.file.clone();
        Update {
            repo,
            candidates,
            pending,
            state: RefCell::new(State {
                cutoffs: None,
                registered: Vec::new(),
                file,
            }),
        }
    }

    /// Whether `name` is a command that has to be applied before it is queued.
    pub(super) fn is_pending(&self, name: &BStr) -> bool {
        self.pending.contains(name)
    }

    /// Make the shallow file have what `command` needs, if it is connected then, and return `true` if that failed.
    pub(super) fn apply(&self, command: &Command, messages: &mut Messages) -> bool {
        if !self.is_pending(command.ref_name.as_bstr()) {
            return false;
        }
        match self.try_apply(command) {
            Ok(connected) => !connected,
            Err(reason) => {
                messages.error(reason);
                true
            }
        }
    }

    fn try_apply(&self, command: &Command) -> Result<bool, String> {
        let mut state = self.state.borrow_mut();
        let State {
            cutoffs,
            registered,
            file,
        } = &mut *state;
        let reached = cutoffs.get_or_insert_with(|| Cutoffs::of_all_references(self.repo, &|_| false));
        let extras: Vec<ObjectId> = self
            .candidates
            .needed_with(self.repo, reached.commits(), command.new_id, registered)
            .into_iter()
            .filter(|id| !registered.contains(id))
            .collect();

        let path = self
            .repo
            .shallow_file()
            .map_err(|err| format!("Could not find the shallow file: {err}"))?;
        let mut update = None;
        if !extras.is_empty() {
            // The lock comes first, as anything that writes the file does it that way, and what the file has is what is read under it.
            let lock = gix_lock::File::acquire_to_update_resource(&path, gix_lock::acquire::Fail::Immediately, None, 0)
                .map_err(|err| format!("Could not lock the shallow file: {err}"))?;
            let current = std::fs::read(&path).ok();
            // Git dies if the file has changed since it read it, as what is written is to be what the file had and more.
            if current != *file {
                return Err("the shallow file has changed since it was read".into());
            }
            let mut entries = current
                .iter()
                .flat_map(|bytes| bytes.lines())
                .map(ObjectId::from_hex)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| format!("Could not read the shallow file: {err}"))?;
            entries.sort();
            entries.dedup();
            let mut text = Vec::new();
            for id in entries.iter().chain(&extras) {
                writeln!(text, "{id}").map_err(|err| err.to_string())?;
            }
            update = Some((lock, text));
        }

        let mut roots = self.candidates.own.clone();
        roots.extend(registered.iter().copied());
        roots.extend(extras.iter().copied());
        let all_references = Cutoffs::of_all_references(self.repo, &|_| false);
        if connectivity::verify_with_roots(self.repo, &all_references, &[command.new_id], Some(&roots)).is_err() {
            // Dropping the lock leaves the file as it was.
            return Ok(false);
        }
        if let Some((mut lock, text)) = update {
            lock.with_mut(|file| file.write_all(&text))
                .map_err(|err| format!("Could not write the shallow file: {err}"))?;
            lock.commit()
                .map_err(|err| format!("Could not update the shallow file: {}", err.error))?;
            registered.extend(extras);
            *file = Some(text);
        }
        Ok(true)
    }
}

/// The file that hooks are told about as the `shallow` file, which has what the receiver has and what the client said, whatever
/// that is. It is removed when it isn't needed any more, which is after the hooks that follow the report ran.
#[derive(Debug)]
pub(in crate::receive) struct TemporaryFile(std::path::PathBuf);

impl TemporaryFile {
    pub(super) fn create(repo: &Repository, lines: &[ObjectId]) -> crate::Result<Self> {
        let mut text = Vec::new();
        let mut own = gix_shallow::read(
            &repo
                .shallow_file()
                .or_raise(|| message("Could not find the shallow file"))?,
        )
        .or_raise(|| message("Could not read the shallow file"))?
        .map_or_else(Vec::new, Vec::from);
        own.dedup();
        for id in own.iter().chain(lines) {
            writeln!(text, "{id}").or_raise(|| message("Could not prepare the shallow file for hooks"))?;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.subsec_nanos());
        for attempt in 0..100_u32 {
            let name = format!(
                "shallow_{:06x}",
                (nanos ^ std::process::id()).wrapping_add(attempt) & 0xff_ffff
            );
            let path = repo.git_dir().join(name);
            let mut open = std::fs::OpenOptions::new();
            open.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
            match open.open(&path) {
                Ok(mut file) => {
                    let temporary = TemporaryFile(path);
                    file.write_all(&text)
                        .or_raise(|| message("Could not write the shallow file for hooks"))?;
                    return Ok(temporary);
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err.and_raise(message("Could not create the shallow file for hooks"))),
            }
        }
        gix_error::bail!(message("Could not find a name for the shallow file for hooks"))
    }

    pub(super) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}
