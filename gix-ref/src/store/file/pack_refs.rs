use gix_error::{Result, ResultExt, bail, message};
use gix_lock::acquire::Fail;

use crate::{
    FullName, Target,
    pack_refs::{Options, Outcome},
    store_impl::file::{self, loose, transaction::prepare::reference_path_and_lock_boundary},
    transaction::{
        Change, LogChange, PreviousValue, RefEdit, RefLog,
        observe::{Event, Observer, Scope, State, Update, Veto, Vetoed},
    },
};

/// Tells an observer about the transactions Git would run, and what it says, like Git's `reference-transaction` hook.
#[derive(Default)]
pub(super) struct Hook<'a>(Option<&'a mut dyn Observer>);

impl Hook<'_> {
    /// Report `state` and fail if the observer refuses it, which only `Preparing` and `Prepared` allow. Then it's
    /// told that the transaction is aborted.
    fn notify(&mut self, state: State, updates: &[Update]) -> Result<()> {
        let Some(observer) = self.0.as_mut() else {
            return Ok(());
        };
        let event = |state| Event {
            state,
            scope: Scope::Transaction,
            updates,
        };
        match observer.observe(&event(state)) {
            Err(Veto) if matches!(state, State::Preparing | State::Prepared) => {
                observer.observe(&event(State::Aborted)).ok();
                bail!(Vetoed {
                    state,
                    scope: Scope::Transaction
                })
            }
            Ok(()) | Err(Veto) => Ok(()),
        }
    }
}

impl file::Store {
    /// Write all loose references that can be packed into `packed-refs`, and delete the loose files if `options` say so, like
    /// `git pack-refs --all --prune` does, for all worktrees' shared references and regardless of the namespace.
    ///
    /// `objects` is used to see which references point to objects that exist, and to peel what they point to.
    pub(crate) fn pack_refs_inner(
        &self,
        objects: &dyn gix_object::Find,
        options: Options,
        observer: Option<&mut dyn Observer>,
    ) -> Result<Outcome> {
        let mut hook = Hook(observer);
        self.assure_backend_supported()?;
        let mut this;
        let store = if self.namespace.is_some() {
            this = self.clone();
            this.namespace = None;
            &this
        } else {
            self
        };

        if options.auto && !store.enough_loose_refs_to_pack(objects)? {
            return Ok(Outcome::default());
        }

        // The lock comes first, to read the loose references while nobody changes `packed-refs` or deletes loose references
        // that are in the middle of being deleted, like Git.
        let transaction = store.packed_transaction(options.lock)?;
        let candidates = store.packable_references(objects)?;
        let packed_updates: Vec<_> = candidates
            .iter()
            .map(|(name, id)| Update {
                name: name.clone(),
                previous: None,
                new: Some(Target::Object(*id)),
            })
            .collect();
        // Git tells its hook about this transaction even if it has nothing to do.
        hook.notify(State::Preparing, &packed_updates)?;
        let edits = candidates.iter().map(|(name, id)| RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: Default::default(),
                },
                expected: PreviousValue::Any,
                new: Target::Object(*id),
            },
            name: name.clone(),
            deref: false,
        });
        let mut outcome = Outcome {
            packed: candidates.len(),
            ..Default::default()
        };
        let prepared = transaction.prepare(&mut edits.into_iter(), objects)?;
        hook.notify(State::Prepared, &packed_updates)?;
        let lock = prepared.commit_keeping_lock()?;
        store.force_refresh_packed_buffer().ok();
        hook.notify(State::Committed, &packed_updates).ok();
        // Like Git, don't hold the lock while deleting, as the values are verified one by one.
        drop(lock);

        if options.prune {
            // Git prunes in the opposite order, which its hook can see.
            for (name, id) in candidates.iter().rev() {
                if store.prune_loose_reference(name, id, &mut hook)? {
                    outcome.pruned += 1;
                } else {
                    outcome.skipped += 1;
                }
            }
        }
        Ok(outcome)
    }

    /// Return the loose references that may be packed, which are the shared ones that point to objects that exist.
    fn packable_references(&self, objects: &dyn gix_object::Find) -> Result<Vec<(FullName, gix_hash::ObjectId)>> {
        let mut out = Vec::new();
        let mut buf = Vec::new();
        let prefix = <&gix_path::RelativePath>::try_from("refs/").map_err(|err| message!("{err}"))?;
        for reference in self
            .loose_iter_prefixed(prefix)
            .or_raise(|| message("Could not list loose references"))?
        {
            // Broken references are left alone, as are those that can't be told apart from them.
            let Ok(reference) = reference else { continue };
            let Target::Object(id) = reference.target else {
                continue;
            };
            if id.is_null()
                || reference
                    .name
                    .category()
                    .is_some_and(|category| category.is_worktree_private())
            {
                continue;
            }
            if objects.try_find(&id, &mut buf)?.is_none() {
                continue;
            }
            out.push((reference.name, id));
        }
        Ok(out)
    }

    /// Return `true` if there are enough loose references to be worth packing, which is the rule Git uses for `--auto`: more than
    /// five times the base-2 logarithm of what `packed-refs` has, at least sixteen, which are what Git estimates a hundred bytes
    /// per reference for.
    fn enough_loose_refs_to_pack(&self, objects: &dyn gix_object::Find) -> Result<bool> {
        let packed_size = std::fs::metadata(self.packed_refs_path()).map_or(0, |meta| meta.len());
        let limit = ((packed_size / 100).checked_ilog2().unwrap_or(0) as usize * 5).max(16);
        Ok(self.packable_references(objects)?.len() >= limit)
    }

    /// Delete the loose reference `name` if it still has the value `id` that was packed, and if nobody else holds it locked,
    /// and return `true` if it is gone. Reflogs and `packed-refs` are left alone.
    fn prune_loose_reference(&self, name: &FullName, id: &gix_hash::ObjectId, hook: &mut Hook<'_>) -> Result<bool> {
        let updates = [Update {
            name: name.clone(),
            previous: Some(Target::Object(*id)),
            new: None,
        }];
        // Like in Git, a reference that can't be locked or isn't what was packed is only ever reported as `preparing`.
        if hook.notify(State::Preparing, &updates).is_err() {
            return Ok(false);
        }
        let (path, mut boundary) = reference_path_and_lock_boundary(self, name.as_ref())?;
        // Git keeps the first directory below `refs/`, like `refs/tags`, even if it's empty now.
        if let Some(first) = path
            .strip_prefix(&boundary)
            .ok()
            .filter(|relative| relative.components().count() > 1)
            .and_then(|relative| relative.components().next())
        {
            boundary.push(first);
        }
        let Ok(lock) = gix_lock::Marker::acquire_to_hold_resource(&path, Fail::Immediately, Some(boundary), 0) else {
            return Ok(false);
        };
        let content = match std::fs::read(&path) {
            Ok(content) => content,
            Err(_) => return Ok(false),
        };
        match loose::Reference::try_from_path(name.clone(), &content, self.object_hash) {
            Ok(reference) if reference.target == Target::Object(*id) => {}
            _ => return Ok(false),
        }
        // Somebody may have deleted the reference after it was packed, and created it again with the same value. Only
        // a reference that is still in `packed-refs` is redundant. As changing it needs the lock that is held now, nothing
        // changes between this check and the deletion.
        let packed = self.open_packed_buffer()?;
        let is_packed = match &packed {
            Some(packed) => packed
                .try_find_full_name(name.as_ref())?
                .is_some_and(|packed| packed.target() == *id),
            None => false,
        };
        if !is_packed {
            return Ok(false);
        }
        if hook.notify(State::Prepared, &updates).is_err() {
            return Ok(false);
        }
        // Like Git, a reference that can't be deleted is reported as not pruned, instead of stopping the others.
        if std::fs::remove_file(&path).is_err() {
            return Ok(false);
        }
        // Dropping the lock removes it, and the directories that are empty now.
        drop(lock);
        hook.notify(State::Committed, &updates).ok();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::Hook;
    use crate::{FullName, file::Store};

    /// A repository with `main` at the second of two commits, which are returned.
    fn repo() -> gix_testtools::Result<(
        gix_testtools::tempfile::TempDir,
        Store,
        gix_hash::ObjectId,
        gix_hash::ObjectId,
    )> {
        let dir = gix_testtools::tempfile::TempDir::new()?;
        gix_testtools::git(dir.path(), "init -q -b main")?;
        let mut commits = Vec::new();
        for message in ["first", "second"] {
            gix_testtools::git(dir.path(), &format!("commit -q --allow-empty -m {message}"))?;
            commits.push(gix_hash::ObjectId::from_hex(
                gix_testtools::git(dir.path(), "rev-parse HEAD")?.trim().as_bytes(),
            )?);
        }
        let store = Store::at(dir.path().join(".git"), gix_testtools::object_hash());
        Ok((dir, store, commits[0], commits[1]))
    }

    #[test]
    fn a_reference_that_changed_after_it_was_packed_is_not_pruned() -> gix_testtools::Result {
        let (dir, store, first, second) = repo()?;
        let name = FullName::try_from("refs/heads/main")?;
        let path = store.git_dir().join("refs/heads/main");
        gix_testtools::git(dir.path(), "pack-refs --all --no-prune")?;

        assert!(
            !store.prune_loose_reference(&name, &first, &mut Hook::default())?,
            "the loose reference has moved on to a value that wasn't packed"
        );
        assert!(path.is_file(), "so it is kept, with its new value");

        assert!(
            store.prune_loose_reference(&name, &second, &mut Hook::default())?,
            "it has the packed value"
        );
        assert!(!path.exists(), "so it is gone");
        assert!(
            !store.prune_loose_reference(&name, &second, &mut Hook::default())?,
            "nothing is there to be deleted anymore"
        );
        assert!(
            !store.git_dir().join("refs/heads/main.lock").exists(),
            "no lock file remains in any of these cases"
        );
        Ok(())
    }

    #[test]
    fn a_reference_that_is_not_packed_is_kept_even_if_it_has_the_value_that_was_packed() -> gix_testtools::Result {
        let (dir, store, _first, second) = repo()?;
        let name = FullName::try_from("refs/heads/main")?;
        let path = store.git_dir().join("refs/heads/main");

        gix_testtools::git(dir.path(), "pack-refs --all --no-prune")?;
        assert!(path.is_file(), "the loose reference stays as nothing prunes it");
        gix_testtools::git(dir.path(), "update-ref -d refs/heads/main")?;
        gix_testtools::git(dir.path(), &format!("update-ref refs/heads/main {second}"))?;
        assert!(path.is_file(), "it was deleted and created again, and is only loose");

        assert!(
            !store.prune_loose_reference(&name, &second, &mut Hook::default())?,
            "packed-refs doesn't have it any more, so the loose file is all there is"
        );
        assert!(path.is_file(), "which is why it must stay");
        Ok(())
    }

    #[test]
    fn a_reference_that_is_locked_is_skipped() -> gix_testtools::Result {
        let (dir, store, _first, second) = repo()?;
        let name = FullName::try_from("refs/heads/main")?;
        gix_testtools::git(dir.path(), "pack-refs --all --no-prune")?;
        std::fs::write(store.git_dir().join("refs/heads/main.lock"), "")?;
        assert!(
            !store.prune_loose_reference(&name, &second, &mut Hook::default())?,
            "somebody else is changing it"
        );
        assert!(store.git_dir().join("refs/heads/main").is_file());
        Ok(())
    }
}
