use gix_error::ResultExt;
use gix_object::Exists;
use gix_ref::{
    Target, TargetRef,
    transaction::{Change, PreviousValue, RefEdit},
};

use crate::{
    Repository, Result,
    ext::ObjectIdExt,
    remote::{
        fetch,
        fetch::{
            RefLogMessage,
            refmap::Source,
            refs::update::{Mode, TypeChange},
        },
    },
};

///
pub mod update;

mod prune;

/// Information about the update of a single reference, corresponding the respective entry in [`RefMap::mappings`][crate::remote::fetch::RefMap::mappings].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// The way the update was performed.
    pub mode: Mode,
    ///  If not `None`, the update also affects the type of the reference. This also implies that `edit_index` is not None.
    pub type_change: Option<TypeChange>,
    /// The index to the edit that was created from the corresponding mapping, or `None` if there was no local ref.
    pub edit_index: Option<usize>,
}

impl From<Mode> for Update {
    fn from(mode: Mode) -> Self {
        Update {
            mode,
            type_change: None,
            edit_index: None,
        }
    }
}

/// Update all refs as derived from `refmap.mappings` and produce an `Outcome` informing about all applied changes in detail, with each
/// [`update`][Update] corresponding to the [`fetch::Mapping`] of at the same index.
/// If `dry_run` is true, ref transactions won't actually be applied, but are assumed to work without error so the underlying
/// `repo` is not actually changed. Also it won't perform an 'object exists' check as these are likely not to exist as the pack
/// wasn't fetched either.
/// `action` is the prefix used for reflog entries, and is typically "fetch".
///
/// It can be used to produce typical information that one is used to from `git fetch`.
///
/// We will reject updates only if…
///
/// * …fast-forward rules are violated
/// * …the local ref is currently checked out
/// * …existing refs would not become 'unborn', i.e. point to a reference that doesn't exist and won't be created due to ref-specs
///
/// With these safeguards in place, one can handle each naturally and implement mirrors or bare repos easily.
#[expect(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn update(
    repo: &Repository,
    message: RefLogMessage,
    mappings: &[fetch::refmap::Mapping],
    refspecs: &[gix_refspec::RefSpec],
    extra_refspecs: &[gix_refspec::RefSpec],
    fetch_tags: fetch::Tags,
    dry_run: fetch::DryRun,
    write_packed_refs: fetch::WritePackedRefs,
) -> Result<update::Outcome> {
    update_and_prune(
        repo,
        message,
        mappings,
        refspecs,
        extra_refspecs,
        fetch_tags,
        dry_run,
        write_packed_refs,
        Prune::No,
    )
}

/// Whether to delete the references of the destinations of the ref-specs that the remote no longer has.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Prune {
    /// Leave them alone.
    No,
    /// Delete them once all updates were applied successfully, or in a transaction of their own beforehand if they stand in the
    /// way of one of the updates, which can't create `refs/p/a/b` while `refs/p/a` exists.
    Yes,
}

/// Like [`update()`], but also deletes the local references that are stale if `prune` is [`Prune::Yes`].
///
/// A reference is stale if a ref-spec maps it to a source that the remote doesn't have, which is the case if none of the
/// `mappings` has it as destination. Only the destinations of `refspecs` are ever considered, and nothing is pruned if
/// any update was rejected, as the fetch isn't complete then.
/// The deletions are part of the transaction that applies the updates, so none of them happens if any update fails,
/// and each of them requires the reference to still have the value it had when it was found stale.
/// The exception are references that conflict with the name of an update, which are deleted first, and in a transaction of their own,
/// like Git deletes all stale references before it updates, as the update can't happen otherwise.
#[expect(clippy::too_many_arguments)]
pub(crate) fn update_and_prune(
    repo: &Repository,
    message: RefLogMessage,
    mappings: &[fetch::refmap::Mapping],
    refspecs: &[gix_refspec::RefSpec],
    extra_refspecs: &[gix_refspec::RefSpec],
    fetch_tags: fetch::Tags,
    dry_run: fetch::DryRun,
    write_packed_refs: fetch::WritePackedRefs,
    prune: Prune,
) -> Result<update::Outcome> {
    let _span = gix_trace::detail!("update_refs()", mappings = mappings.len());
    let mut edits = Vec::new();
    let mut updates = Vec::new();
    let mut edit_indices_to_validate = Vec::new();

    let mut checked_out_branches = repo.checked_out_branches()?;
    let implicit_tag_refspec = fetch_tags
        .to_refspec()
        .filter(|_| matches!(fetch_tags, crate::remote::fetch::Tags::Included));
    for (remote, local, spec, is_implicit_tag) in mappings.iter().filter_map(
        |fetch::refmap::Mapping {
             remote,
             local,
             spec_index,
         }| {
            spec_index.get(refspecs, extra_refspecs).map(|spec| {
                (
                    remote,
                    local,
                    spec,
                    implicit_tag_refspec.is_some_and(|tag_spec| spec.to_ref() == tag_spec),
                )
            })
        },
    ) {
        // `None` only if unborn.
        let remote_id = remote.as_id();
        if matches!(dry_run, fetch::DryRun::No)
            && !remote_id.is_none_or(|id| repo.objects.exists(id))
            && let Some(remote_id) = remote_id.filter(|id| !repo.objects.exists(id))
        {
            let update = if is_implicit_tag {
                Mode::ImplicitTagNotSentByRemote.into()
            } else {
                // Assure the ODB is not to blame for the missing object.
                repo.try_find_object(remote_id)?;
                Mode::RejectedSourceObjectNotFound { id: remote_id.into() }.into()
            };
            updates.push(update);
            continue;
        }
        let (mode, edit_index, type_change) = match local {
            Some(name) => {
                let (mode, reflog_message, name, previous_value) = match repo.try_find_reference(name)? {
                    Some(existing) => {
                        if let Some(wt_dirs) = checked_out_branches.get_mut(existing.name()) {
                            wt_dirs.sort();
                            wt_dirs.dedup();
                            let mode = Mode::RejectedCurrentlyCheckedOut {
                                worktree_dirs: wt_dirs.to_owned(),
                            };
                            updates.push(mode.into());
                            continue;
                        }

                        match existing
                            .try_id()
                            .map_or_else(|| existing.clone().peel_to_id(), Ok)
                            .or_raise(|| gix_error::message("Could not peel symbolic local reference to its ID"))
                            .map(crate::Id::detach)
                        {
                            Ok(local_id) => {
                                let remote_id = match remote_id {
                                    Some(id) => id,
                                    None => {
                                        // we don't allow to go back to unborn state if there is a local reference already present.
                                        // Note that we will be changing it to a symbolic reference just fine.
                                        updates.push(Mode::RejectedToReplaceWithUnborn.into());
                                        continue;
                                    }
                                };
                                let (mode, reflog_message) = if local_id == remote_id {
                                    (Mode::NoChangeNeeded, "no update will be performed")
                                } else if let Some(gix_ref::Category::Tag) = existing.name().category() {
                                    if spec.allow_non_fast_forward() {
                                        (Mode::Forced, "updating tag")
                                    } else {
                                        updates.push(Mode::RejectedTagUpdate.into());
                                        continue;
                                    }
                                } else {
                                    let mut force = spec.allow_non_fast_forward();
                                    let is_fast_forward = match dry_run {
                                        fetch::DryRun::No => {
                                            let local = repo.find_object(local_id)?;
                                            if local.kind == gix_object::Kind::Commit
                                                && repo.find_header(remote_id)?.kind() == gix_object::Kind::Commit
                                            {
                                                let local_commit_time = local.into_commit().committer()?.seconds();
                                                let mut ancestors =
                                                    remote_id.to_owned().ancestors(&repo.objects).sorting(
                                                        gix_traverse::commit::simple::Sorting::ByCommitTimeCutoff {
                                                            order: Default::default(),
                                                            seconds: local_commit_time,
                                                        },
                                                    )?;
                                                // Stop at either a matching ancestor or the first traversal error.
                                                ancestors
                                                    .find(|entry| {
                                                        entry.as_ref().map_or(true, |entry| entry.id == local_id)
                                                    })
                                                    .transpose()?
                                                    .is_some()
                                            } else {
                                                force = true;
                                                false
                                            }
                                        }
                                        fetch::DryRun::Yes => true,
                                    };
                                    if is_fast_forward {
                                        (
                                            Mode::FastForward,
                                            matches!(dry_run, fetch::DryRun::Yes)
                                                .then(|| "fast-forward (guessed in dry-run)")
                                                .unwrap_or("fast-forward"),
                                        )
                                    } else if force {
                                        (Mode::Forced, "forced-update")
                                    } else {
                                        updates.push(Mode::RejectedNonFastForward.into());
                                        continue;
                                    }
                                };
                                (
                                    mode,
                                    reflog_message,
                                    existing.name().to_owned(),
                                    PreviousValue::MustExistAndMatch(existing.target().into_owned()),
                                )
                            }
                            Err(err) if err.downcast_any_ref::<gix_ref::view::NotFound>().is_some() => {
                                // An unborn reference, always allow it to be changed to whatever the remote wants.
                                (
                                    if existing.target().try_name().map(gix_ref::FullNameRef::as_bstr)
                                        == remote.as_target()
                                    {
                                        Mode::NoChangeNeeded
                                    } else {
                                        Mode::Forced
                                    },
                                    "change unborn ref",
                                    existing.name().to_owned(),
                                    PreviousValue::MustExistAndMatch(existing.target().into_owned()),
                                )
                            }
                            Err(err) => return Err(err),
                        }
                    }
                    None => {
                        let name = gix_ref::FullName::try_from(name).or_raise(|| {
                            gix_error::message(
                                "A remote reference had a name that wasn't considered valid. \
                                 Corrupt remote repo or insufficient checks on remote?",
                            )
                        })?;
                        let reflog_msg = match name.category() {
                            Some(gix_ref::Category::Tag) => "storing tag",
                            Some(gix_ref::Category::LocalBranch) => "storing head",
                            _ => "storing ref",
                        };
                        (
                            Mode::New,
                            reflog_msg,
                            name,
                            PreviousValue::ExistingMustMatch(new_value_by_remote(remote)?),
                        )
                    }
                };

                let new = new_value_by_remote(remote)?;
                let type_change = match (&previous_value, &new) {
                    (
                        PreviousValue::ExistingMustMatch(Target::Object(_))
                        | PreviousValue::MustExistAndMatch(Target::Object(_)),
                        Target::Symbolic(_),
                    ) => Some(TypeChange::DirectToSymbolic),
                    (
                        PreviousValue::ExistingMustMatch(Target::Symbolic(_))
                        | PreviousValue::MustExistAndMatch(Target::Symbolic(_)),
                        Target::Object(_),
                    ) => Some(TypeChange::SymbolicToDirect),
                    _ => None,
                };
                // We are here because this edit should work and fast-forward rules are respected.
                // But for setting a symref-target, we have to be sure that the target already exists
                // or will exists. To be sure all rules are respected, we delay the check to when the
                // edit-list has been built.
                let edit_index = edits.len();
                if matches!(new, Target::Symbolic(_)) {
                    let anticipated_update_index = updates.len();
                    edit_indices_to_validate.push((anticipated_update_index, edit_index));
                }
                // We must not deref symrefs or we will overwrite their destination, which might be checked out
                // and we don't check for that case.
                let edit = RefEdit::update(name, new, previous_value, message.compose(reflog_message));
                edits.push(edit);
                (mode, Some(edit_index), type_change)
            }
            None => (Mode::NoChangeNeeded, None, None),
        };
        updates.push(Update {
            mode,
            type_change,
            edit_index,
        });
    }

    for (update_index, edit_index) in edit_indices_to_validate {
        let edit = &edits[edit_index];
        if update_needs_adjustment_as_edits_symbolic_target_is_missing(edit, repo, &edits) {
            let edit = &mut edits[edit_index];
            let update = &mut updates[update_index];

            update.mode = Mode::RejectedToReplaceWithUnborn;
            update.type_change = None;

            match edit.change {
                Change::Update {
                    ref expected,
                    ref mut new,
                    ref mut log,
                    ..
                } => match expected {
                    PreviousValue::MustExistAndMatch(existing) => {
                        *new = existing.clone();
                        log.message = "no-op".into();
                    }
                    _ => unreachable!("at this point it can only be one variant"),
                },
                Change::Delete { .. } => {
                    unreachable!("we don't do that here")
                }
            }
        }
    }

    let mut early_deletions = Vec::new();
    if prune == Prune::Yes
        && !updates.iter().any(|update| {
            matches!(
                update.mode,
                Mode::RejectedSourceObjectNotFound { .. }
                    | Mode::RejectedTagUpdate
                    | Mode::RejectedNonFastForward
                    | Mode::RejectedToReplaceWithUnborn
                    | Mode::RejectedCurrentlyCheckedOut { .. }
            )
        })
    {
        for stale in prune::stale_destinations(repo, refspecs, mappings)? {
            let in_the_way = edits
                .iter()
                .any(|edit| prune::conflicts(stale.name.as_bstr(), edit.name.as_bstr()));
            let deletion = stale.into_delete_edit();
            if in_the_way && matches!(dry_run, fetch::DryRun::No) {
                early_deletions.push(deletion);
            } else {
                edits.push(deletion);
            }
        }
    }

    let edits = match dry_run {
        fetch::DryRun::No => {
            let _span = gix_trace::detail!("apply", edits = edits.len());
            let (file_lock_fail, packed_refs_lock_fail) = repo.config.lock_timeout().or_raise(|| {
                gix_error::message("Failed to update references to their new position to match their remote locations")
            })?;
            let apply = |edits: Vec<gix_ref::transaction::RefEdit>| -> Result<Vec<gix_ref::transaction::RefEdit>> {
                const CONTEXT: &str =
                    "Failed to update references to their new position to match their remote locations";
                Ok(repo
                    .ref_store()
                    .transaction()
                    .packed_refs(match write_packed_refs {
                        fetch::WritePackedRefs::Only => {
                            gix_ref::view::transaction::PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(Box::new(&repo.objects))},
                        fetch::WritePackedRefs::Never => gix_ref::view::transaction::PackedRefs::DeletionsOnly
                    })
                    .prepare(
                        edits,
                        gix_ref::view::transaction::LockOptions::new(file_lock_fail, packed_refs_lock_fail),
                    )
                    .or_raise(|| gix_error::message(CONTEXT))?
                    .commit(
                        repo.committer()
                            .transpose()
                            .or_raise(|| gix_error::message(CONTEXT))?,
                    )
                    .or_raise(|| gix_error::message(CONTEXT))?
                    .edits)
            };
            let mut committed = if early_deletions.is_empty() {
                Vec::new()
            } else {
                apply(early_deletions)?
            };
            // The edits are in the order of the updates, with deletions at the end, so edit indices stay valid. Those
            // of references that were deleted early come last, which no update refers to.
            let mut applied = apply(edits)?;
            applied.append(&mut committed);
            applied
        }
        fetch::DryRun::Yes => edits,
    };

    Ok(update::Outcome { edits, updates })
}

/// Figure out if target of `edit` points to a reference that doesn't exist in `repo` and won't exist as it's not in any of `edits`.
/// If so, return true.
fn update_needs_adjustment_as_edits_symbolic_target_is_missing(
    edit: &RefEdit,
    repo: &Repository,
    edits: &[RefEdit],
) -> bool {
    match edit.change.new_value().expect("here we need a symlink") {
        TargetRef::Object(_) => unreachable!("BUG: we already know it's symbolic"),
        TargetRef::Symbolic(new_target_ref) => {
            match &edit.change {
                Change::Update { expected, .. } => match expected {
                    PreviousValue::MustExistAndMatch(current_target) => {
                        if let Target::Symbolic(current_target_name) = current_target {
                            if current_target_name.as_ref() == new_target_ref {
                                return false; // no-op are always fine
                            }
                            let current_is_unborn =
                                repo.ref_store().try_find(current_target_name).ok().flatten().is_none();
                            if current_is_unborn {
                                return false;
                            }
                        }
                    }
                    PreviousValue::ExistingMustMatch(_) => return false, // this means the ref doesn't exist locally, so we can create unborn refs anyway
                    _ => {
                        unreachable!("BUG: we don't do that here")
                    }
                },
                Change::Delete { .. } => {
                    unreachable!("we don't ever delete here")
                }
            }
            let target_ref_exists_locally = repo.ref_store().try_find(new_target_ref).ok().flatten().is_some();
            if target_ref_exists_locally {
                return false;
            }

            let target_ref_will_be_created = edits.iter().any(|edit| edit.name.as_ref() == new_target_ref);
            !target_ref_will_be_created
        }
    }
}

/// Convert the remote source into the value to write locally.
///
/// Born symbolic remote refs are written as direct refs to the advertised target object id.
/// Unborn remote refs remain symbolic as there is no object id to write.
fn new_value_by_remote(remote: &Source) -> Result<Target> {
    let remote_id = remote.as_id();
    Ok(
        if let Source::Ref(
            gix_protocol::handshake::Ref::Symbolic { target, .. } | gix_protocol::handshake::Ref::Unborn { target, .. },
        ) = &remote
        {
            match remote_id {
                Some(desired_id) => Target::Object(desired_id.to_owned()),
                // Unborn branches we create as such, with the location they point to on the remote which helps mirroring.
                None => Target::Symbolic(gix_ref::FullName::try_from(target).or_raise(|| {
                    gix_error::message(
                        "A remote reference had a name that wasn't considered valid. \
                         Corrupt remote repo or insufficient checks on remote?",
                    )
                })?),
            }
        } else {
            Target::Object(remote_id.expect("unborn case handled earlier").to_owned())
        },
    )
}

#[cfg(test)]
mod tests;
