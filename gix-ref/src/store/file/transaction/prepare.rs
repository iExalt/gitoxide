use std::path::PathBuf;

use gix_error::{ClassificationMarker, ErrorExt, Message, Result, ResultExt, bail, message, not_found, validation};
use gix_object::bstr::{BStr, BString, ByteSlice};

use crate::{
    FullName, FullNameRef, Reference, Target, packed,
    packed::transaction::buffer_into_transaction,
    store_impl::{
        file,
        file::{
            Transaction, loose,
            transaction::{Edit, PackedRefs, notifier},
        },
    },
    transaction::{
        Change, ExistingRefs, LogChange, NameConflict, PreviousValue, RefEdit, RefEditsExt, RefLog, observe::Update,
        verify_available,
    },
};

/// The references of a files store that exist, loose or packed, for checking that names are available.
///
/// Parent directories are shared by many references, so what is known about them is kept for the transaction. It can't
/// change while it is being prepared, as nothing is created until it is committed.
struct Existing<'a> {
    store: &'a file::Store,
    packed: Option<&'a packed::Buffer>,
    known: std::cell::RefCell<std::collections::BTreeMap<BString, bool>>,
}

impl Existing<'_> {
    /// Look for the reference named `name` in loose and packed storage.
    fn find(&self, name: &BStr) -> Result<bool> {
        let full_name = FullNameRef::new_unchecked(name);
        match self.store.ref_contents(full_name) {
            Ok(Some(_)) => return Ok(true),
            Ok(None) => {}
            // A file is in the way of this name, which means the name isn't a reference of its own.
            Err(err) if err.kind() == std::io::ErrorKind::NotADirectory => return Ok(false),
            Err(err) => return Err(err.and_raise(message!("Could not look for reference '{name}'"))),
        }
        let (Some(packed), Some(packed_name)) = (self.packed, self.packed_name(name)) else {
            return Ok(false);
        };
        Ok(packed
            .try_find_full_name(FullNameRef::new_unchecked(packed_name.as_bstr()))?
            .is_some())
    }

    /// The name as it is stored in `packed-refs`, or `None` if such a name isn't stored there.
    fn packed_name(&self, name: &BStr) -> Option<BString> {
        let name = packed::find::transform_full_name_for_lookup(FullNameRef::new_unchecked(name))?;
        Some(match &self.store.namespace {
            Some(namespace) => namespace.to_owned().into_namespaced_name(name).0,
            None => name.as_bstr().to_owned(),
        })
    }
}

impl ExistingRefs for Existing<'_> {
    fn exists(&self, name: &BStr) -> Result<bool> {
        if let Some(exists) = self.known.borrow().get(name) {
            return Ok(*exists);
        }
        let exists = self.find(name)?;
        self.known.borrow_mut().insert(name.to_owned(), exists);
        Ok(exists)
    }

    fn any_below(&self, directory: &BStr) -> Result<Option<BString>> {
        let directory_name = FullNameRef::new_unchecked(directory[..directory.len() - 1].as_bstr());
        let (base, relative_path) = self.store.reference_path_with_base(directory_name)?;
        let directory_path = base.join(relative_path.as_ref());
        if directory_path.is_dir() {
            for entry in gix_fs::walkdir_sorted_new(&directory_path, usize::MAX, self.store.precompose_unicode) {
                let entry = entry.map_err(|err| {
                    err.into_io_error()
                        .expect("no symlink related errors")
                        .and_raise(message!("Could not look for references in '{directory}'"))
                })?;
                // Lock files belong to references that are being edited, and are accounted for by the edits.
                if !entry.file_type().is_ok_and(|ft| ft.is_file())
                    || entry.file_name().as_encoded_bytes().ends_with(b".lock")
                {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(&directory_path)
                    .expect("entries are located below their directory")
                    .to_owned();
                let mut name = directory_name.as_bstr().to_owned();
                name.push(b'/');
                name.extend_from_slice(&gix_path::to_unix_separators_on_windows(gix_path::into_bstr(relative)?));
                return Ok(Some(name));
            }
        }
        let Some(packed) = self.packed else {
            return Ok(None);
        };
        let Some(packed_directory) = self.packed_name(directory_name.as_bstr()) else {
            return Ok(None);
        };
        let mut prefix = packed_directory;
        prefix.push(b'/');
        match packed.iter_prefixed(prefix)?.next().transpose()? {
            Some(reference) => {
                let mut name = reference.name.as_bstr().to_owned();
                if let Some(namespace) = &self.store.namespace {
                    name = name[namespace.as_bstr().len()..].into();
                }
                Ok(Some(name))
            }
            None => Ok(None),
        }
    }
}

impl Transaction<'_, '_> {
    /// Read the current value of a reference from loose storage, falling back to packed refs.
    ///
    /// Must be called while holding the lock for `name` so the subsequent CAS check
    /// compares against a value that cannot be changed concurrently.
    fn read_existing_ref(
        store: &file::Store,
        name: &FullNameRef,
        packed: Option<&packed::Buffer>,
    ) -> Result<Option<Reference>> {
        let loose = store
            .ref_contents(name)
            .or_raise(|| message("Could not read existing reference"))?
            // Like Git, don't treat a malformed loose reference as absent: edits would replace or delete it, and
            // decide against the packed reference that it hides.
            .map(|buf| {
                loose::Reference::try_from_path(name.to_owned(), &buf, store.object_hash)
                    .map(Reference::from)
                    .or_raise(|| message!("Unable to resolve reference '{name}': reference broken"))
            })
            .transpose()?;
        match (loose, packed) {
            (None, Some(packed)) => packed.try_find(name).map(|reference| reference.map(Into::into)),
            (reference, _) => Ok(reference),
        }
    }

    fn lock_ref_and_apply_change(
        store: &file::Store,
        lock_fail_mode: gix_lock::acquire::Fail,
        packed: Option<&packed::Buffer>,
        change: &mut Edit,
        direct_to_packed_refs: bool,
        names_in_transaction: &[BString],
        existing: &Existing<'_>,
    ) -> Result {
        use std::io::Write;
        assert!(
            change.lock.is_none(),
            "locks can only be acquired once and it's all or nothing"
        );

        // Reject Windows reserved device names before acquiring the lock.
        // The lock file itself (e.g. `CON.lock`) is also a device name,
        // so acquiring it would fail or open the device instead of
        // returning the configured validation error.
        store
            .check_windows_device_name(change.update.name.as_ref())
            .or_raise(|| message("Invalid reference filename"))?;

        let (reference_path, lock_boundary) = reference_path_and_lock_boundary(store, change.update.name.as_ref())?;

        let lock = match &mut change.update.change {
            Change::Delete { expected, .. } => {
                let lock = gix_lock::Marker::acquire_to_hold_resource(
                    &reference_path,
                    lock_fail_mode,
                    Some(lock_boundary.clone()),
                    0,
                )?;

                let existing_ref = Self::read_existing_ref(store, change.update.name.as_ref(), packed)?;

                match (&expected, &existing_ref) {
                    (PreviousValue::MustNotExist, _) => {
                        panic!("BUG: MustNotExist constraint makes no sense if references are to be deleted")
                    }
                    (PreviousValue::ExistingMustMatch(_) | PreviousValue::Any, None)
                    | (PreviousValue::MustExist | PreviousValue::Any, Some(_)) => {}
                    (PreviousValue::MustExist | PreviousValue::MustExistAndMatch(_), None) => {
                        bail!(not_found("The reference to delete must exist"));
                    }
                    (
                        PreviousValue::MustExistAndMatch(previous) | PreviousValue::ExistingMustMatch(previous),
                        Some(existing),
                    ) => {
                        let actual = existing.target.clone();
                        if *previous != actual {
                            let context = message!("Expected reference content {previous}");
                            return Err(ReferenceOutOfDate {
                                full_name: change.name(),
                                actual,
                            }
                            .and_raise(context));
                        }
                    }
                }

                // Keep the previous value for the caller and ourselves. Maybe they want to keep a log of sorts.
                if let Some(existing) = existing_ref {
                    *expected = PreviousValue::MustExistAndMatch(existing.target);
                }

                Some(lock)
            }
            Change::Update { expected, new, .. } => {
                let may_create = matches!(
                    expected,
                    PreviousValue::Any | PreviousValue::MustNotExist | PreviousValue::ExistingMustMatch(_)
                );
                let mut lock = match gix_lock::File::acquire_to_update_resource(
                    &reference_path,
                    lock_fail_mode,
                    Some(lock_boundary.clone()),
                    0,
                ) {
                    Ok(lock) => lock,
                    Err(err) => {
                        // A reference that is a file can't be a directory, which shows as a failure to lock.
                        // Only a conflict is a better explanation than the lock, and finding out may fail itself.
                        if may_create
                            && let Err(conflict) =
                                verify_available(change.update.name.as_bstr(), names_in_transaction, existing)
                            && conflict.downcast_any_ref::<NameConflict>().is_some()
                        {
                            return Err(conflict);
                        }
                        return Err(err);
                    }
                };

                let existing_ref = Self::read_existing_ref(store, change.update.name.as_ref(), packed)?;
                if existing_ref.is_none() && may_create {
                    verify_available(change.update.name.as_bstr(), names_in_transaction, existing)?;
                }

                match (&expected, &existing_ref) {
                    (PreviousValue::Any, _)
                    | (PreviousValue::MustExist, Some(_))
                    | (PreviousValue::MustNotExist | PreviousValue::ExistingMustMatch(_), None) => {}
                    (PreviousValue::MustExist, None) => {
                        bail!(not_found("The reference to update must exist"));
                    }
                    (PreviousValue::MustNotExist, Some(existing)) => {
                        // Even a reference that has the new value exists, and creating it is a duplicate write that
                        // the caller wants to know about, as Git does.
                        let context = message!("Expected the reference not to exist when writing {new}");
                        return Err(MustNotExist {
                            full_name: change.name(),
                            actual: existing.target.clone(),
                        }
                        .and_raise(context));
                    }
                    (
                        PreviousValue::MustExistAndMatch(previous) | PreviousValue::ExistingMustMatch(previous),
                        Some(existing),
                    ) => {
                        if *previous != existing.target {
                            let actual = existing.target.clone();
                            let context = message!("Expected reference content {previous}");
                            return Err(ReferenceOutOfDate {
                                full_name: change.name(),
                                actual,
                            }
                            .and_raise(context));
                        }
                    }

                    (PreviousValue::MustExistAndMatch(previous), None) => {
                        bail!("The reference must exist with content {previous}".not_found());
                    }
                }

                fn new_would_change_existing(new: &Target, existing: &Target) -> (bool, bool) {
                    match (new, existing) {
                        (Target::Object(new), Target::Object(old)) => (old != new, false),
                        (Target::Symbolic(new), Target::Symbolic(old)) => (old != new, true),
                        (Target::Object(_), _) => (true, false),
                        (Target::Symbolic(_), _) => (true, true),
                    }
                }

                let (is_effective, is_symbolic) = if let Some(existing) = existing_ref {
                    let (effective, is_symbolic) = new_would_change_existing(new, &existing.target);
                    *expected = PreviousValue::MustExistAndMatch(existing.target);
                    (effective, is_symbolic)
                } else {
                    (true, matches!(new, Target::Symbolic(_)))
                };

                let keep_lock_for_loose_source_delete = direct_to_packed_refs && matches!(new, Target::Object(_));
                if (is_effective && !direct_to_packed_refs) || is_symbolic {
                    lock.with_mut(|file| match new {
                        Target::Object(oid) => writeln!(file, "{oid}"),
                        Target::Symbolic(name) => writeln!(file, "ref: {}", name.0),
                    })
                    .or_raise(|| message("Could not write loose reference"))?;
                    if matches!(new, Target::Object(_)) {
                        // Git doesn't flush symbolic references, which it writes through a different path.
                        lock.with_mut(|file| store.fsync.flush(file, gix_fs::fsync::Components::REFERENCE))
                            .or_raise(|| message("Could not flush loose reference"))?;
                    }
                    Some(lock.close().or_raise(|| message("Could not close reference lock"))?)
                } else if keep_lock_for_loose_source_delete {
                    Some(lock.close().or_raise(|| message("Could not close reference lock"))?)
                } else {
                    None
                }
            }
        };
        change.lock = lock;
        Ok(())
    }
}

impl Transaction<'_, '_> {
    /// Prepare for calling [`commit(…)`][Transaction::commit()] in a way that can be rolled back perfectly.
    ///
    /// If the operation succeeds, the transaction can be committed or dropped to cause a rollback automatically.
    /// Rollbacks happen automatically on failure and they tend to be perfect.
    /// This method is idempotent.
    ///
    /// Failed edits identify the requested name in [metadata](gix_error::Error::metadata()) `reference` (bytes).
    /// `referent` records the resolved name only when it differs from the requested name.
    /// [`ReferenceOutOfDate`] and [`MustNotExist`] retain the actual target observed while holding the lock.
    pub fn prepare(
        self,
        edits: impl IntoIterator<Item = RefEdit>,
        ref_files_lock_fail_mode: gix_lock::acquire::Fail,
        packed_refs_lock_fail_mode: gix_lock::acquire::Fail,
    ) -> Result<Self> {
        self.prepare_inner(
            &mut edits.into_iter(),
            ref_files_lock_fail_mode,
            packed_refs_lock_fail_mode,
        )
    }

    /// Failed edits include [metadata](gix_error::Error::metadata()) `reference` (requested name bytes) and `referent`
    /// (resolved name bytes).
    fn prepare_inner(
        mut self,
        edits: &mut dyn Iterator<Item = RefEdit>,
        ref_files_lock_fail_mode: gix_lock::acquire::Fail,
        packed_refs_lock_fail_mode: gix_lock::acquire::Fail,
    ) -> Result<Self> {
        assert!(self.updates.is_none(), "BUG: Must not call prepare(…) multiple times");
        self.store.assure_backend_supported()?;
        let store = self.store;
        let mut updates: Vec<_> = edits
            .map(|update| Edit {
                update,
                lock: None,
                parent_index: None,
                leaf_referent_previous_oid: None,
            })
            .collect();
        let duplicate = |name: BString| validation(format!("A reference named '{name}' has multiple edits")).raise();
        // Git rejects the same name twice before it tells anybody, but not a name that is also the target of a
        // symbolic reference, which it finds while locking.
        updates.assure_one_name_has_one_edit().map_err(duplicate)?;
        if self.observer.is_observing() {
            // The caller's view of the edits, before they are split along symbolic references.
            let previous: Vec<_> = updates.iter().map(|edit| notifier::expected(&edit.update)).collect();
            let requested = notifier::updates(&updates, &previous);
            if let Err(vetoed) = self.observer.preparing(&requested) {
                bail!(vetoed);
            }
            self.observer.locking(notifier::deletions(&updates, &previous));
        }
        updates
            .extend_with_splits_of_symbolic_refs(
                &mut |name| {
                    let symbolic_refs_are_never_packed = None;
                    store
                        .find_existing_inner(name, symbolic_refs_are_never_packed)
                        .map(|r| r.target)
                        .ok()
                },
                &mut |idx, update| Edit {
                    update,
                    lock: None,
                    parent_index: Some(idx),
                    leaf_referent_previous_oid: None,
                },
            )
            .or_raise(|| message("Could not preprocess reference edits"))?;
        if let Err(name) = updates.assure_one_name_has_one_edit() {
            // Git finds this when it splits the symbolic reference, which is when the deletions before it were locked.
            if let Some(split) = updates
                .iter()
                .find(|edit| edit.name() == name && edit.parent_index.is_some())
            {
                let mut position = split.parent_index;
                let mut first = position;
                while let Some(parent) = position {
                    first = Some(parent);
                    position = updates[parent].parent_index;
                }
                self.observer.locked_before(first.unwrap_or(0));
            }
            return Err(duplicate(name));
        }

        // What each caller expects is lost while locking, so remember it now.
        let previous: Vec<_> = if self.observer.is_observing() {
            updates.iter().map(|edit| notifier::expected(&edit.update)).collect()
        } else {
            Vec::new()
        };
        if self.observer.is_observing() {
            self.observer.locking(notifier::deletions(&updates, &previous));
            // The order of locks differs from Git, which locks `packed-refs` last: failing to prepare it is reported
            // as if all deletions were locked before.
            self.observer.locked_before(usize::MAX);
        }
        let mut maybe_updates_for_packed_refs = match self.packed_refs {
            PackedRefs::DeletionsAndNonSymbolicUpdates(_)
            | PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(_) => Some(0_usize),
            PackedRefs::DeletionsOnly => None,
        };
        if maybe_updates_for_packed_refs.is_some()
            || self.store.packed_refs_path().is_file()
            || self.store.packed_refs_lock_path().is_file()
        {
            let mut edits_for_packed_transaction = Vec::<RefEdit>::new();
            let mut needs_packed_refs_lookups = false;
            for edit in &updates {
                let log_mode = match edit.update.change {
                    Change::Update {
                        log: LogChange { mode, .. },
                        ..
                    } => mode,
                    Change::Delete { log, .. } => log,
                };
                if log_mode == RefLog::Only {
                    continue;
                }
                let name = match possibly_adjust_name_for_prefixes(edit.update.name.as_ref()) {
                    Some(n) => n,
                    None => continue,
                };
                if let Some(ref mut num_updates) = maybe_updates_for_packed_refs
                    && let Change::Update {
                        new: Target::Object(_), ..
                    } = edit.update.change
                {
                    edits_for_packed_transaction.push(RefEdit {
                        name,
                        ..edit.update.clone()
                    });
                    *num_updates += 1;
                    continue;
                }
                match edit.update.change {
                    Change::Update {
                        expected: PreviousValue::ExistingMustMatch(_) | PreviousValue::MustExistAndMatch(_),
                        ..
                    } => needs_packed_refs_lookups = true,
                    Change::Delete { .. } => {
                        edits_for_packed_transaction.push(RefEdit {
                            name,
                            ..edit.update.clone()
                        });
                    }
                    _ => {
                        needs_packed_refs_lookups = true;
                    }
                }
            }

            if !edits_for_packed_transaction.is_empty() || needs_packed_refs_lookups {
                // What follows means that we will only create a transaction if we have to access packed refs for looking
                // up current ref values, or that we definitely have a transaction if we need to make updates. Otherwise
                // we may have no transaction at all which isn't required if we had none and would only try making deletions.
                let packed_transaction: Option<_> =
                    if maybe_updates_for_packed_refs.unwrap_or(0) > 0 || self.store.packed_refs_lock_path().is_file() {
                        // We have to create a packed-ref even if it doesn't exist
                        self.store.packed_transaction(packed_refs_lock_fail_mode)?.into()
                    } else {
                        // A packed transaction is optional - we only have deletions that can't be made if
                        // no packed-ref file exists anyway
                        self.store
                            .assure_packed_refs_uptodate()?
                            .map(|p| {
                                buffer_into_transaction(
                                    p,
                                    packed_refs_lock_fail_mode,
                                    self.store.precompose_unicode,
                                    self.store.namespace.clone(),
                                    self.store.fsync,
                                )
                            })
                            .transpose()?
                    };
                if let Some(transaction) = packed_transaction {
                    self.packed_transaction = Some(match &mut self.packed_refs {
                        PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(f)
                        | PackedRefs::DeletionsAndNonSymbolicUpdates(f) => {
                            transaction.prepare(&mut edits_for_packed_transaction.into_iter(), &**f)?
                        }
                        PackedRefs::DeletionsOnly => transaction
                            .prepare(&mut edits_for_packed_transaction.into_iter(), &gix_object::find::Never)?,
                    });
                }
            }
        }

        self.observer.locked_before(0);
        let mut names_in_transaction: Vec<BString> = updates
            .iter()
            .map(|edit| edit.update.name.as_bstr().to_owned())
            .collect();
        names_in_transaction.sort();
        let existing = Existing {
            store,
            packed: self.packed_transaction.as_ref().and_then(packed::Transaction::buffer),
            known: Default::default(),
        };
        for cid in 0..updates.len() {
            let change = &mut updates[cid];
            if let Err(err) = Self::lock_ref_and_apply_change(
                self.store,
                ref_files_lock_fail_mode,
                self.packed_transaction.as_ref().and_then(packed::Transaction::buffer),
                change,
                matches!(
                    self.packed_refs,
                    PackedRefs::DeletionsAndNonSymbolicUpdatesRemoveLooseSourceReference(_)
                ),
                &names_in_transaction,
                &existing,
            ) {
                let referent = change.name();
                let mut ref_name = referent.clone();
                let mut cursor = change.parent_index;
                while let Some(parent_idx) = cursor {
                    let parent = &updates[parent_idx];
                    ref_name = parent.name();
                    cursor = parent.parent_index;
                }
                let differs = ref_name != referent;
                let mut context = Message::new("Could not prepare reference edit").with("reference", ref_name);
                if differs {
                    context = context.with("referent", referent);
                }
                return Err(err.and_raise(context));
            }
            self.observer.locked_before(cid + 1);

            // traverse parent chain from leaf/peeled ref and set the leaf previous oid accordingly
            // to help with their reflog entries
            if let (Some(crate::TargetRef::Object(oid)), Some(parent_idx)) =
                (change.update.change.previous_value(), change.parent_index)
            {
                let oid = oid.to_owned();
                let mut parent_idx_cursor = Some(parent_idx);
                while let Some(parent) = parent_idx_cursor.take().map(|idx| &mut updates[idx]) {
                    parent_idx_cursor = parent.parent_index;
                    parent.leaf_referent_previous_oid = Some(oid);
                }
            }
        }
        if self.observer.is_observing() {
            let observed = notifier::updates(&updates, &previous);
            // As in Git, deletions need a transaction on `packed-refs`, which has work to do if it has them.
            let packed_mode = !matches!(self.packed_refs, PackedRefs::DeletionsOnly);
            let packed_updates: Vec<_> = observed
                .iter()
                .filter(|update| update.new.is_none() || (packed_mode && matches!(update.new, Some(Target::Object(_)))))
                .map(|update| Update {
                    previous: None,
                    ..update.clone()
                })
                .collect();
            let buffer = self.packed_transaction.as_ref().and_then(packed::Transaction::buffer);
            let packed_is_needed = packed_updates.iter().any(|update| {
                update.new.is_some()
                    || buffer.is_some_and(|buffer| {
                        buffer
                            .try_find(update.name.as_ref())
                            .is_ok_and(|reference| reference.is_some())
                    })
            });
            let packed_updates = (!packed_updates.is_empty()).then_some(packed_updates);
            if let Err(vetoed) = self.observer.prepared(observed, packed_updates, packed_is_needed) {
                // The locks are released and the observer told about it when `self` is dropped.
                bail!(vetoed);
            }
        }
        self.updates = Some(updates);
        Ok(self)
    }

    /// Rollback all intermediate state and return the `RefEdits` as we know them thus far.
    ///
    /// Note that they have been altered compared to what was initially provided as they have
    /// been split and know about their current state on disk.
    ///
    /// # Note
    ///
    /// A rollback happens automatically as this instance is dropped as well.
    pub fn rollback(self) -> Vec<RefEdit> {
        self.updates
            .map(|updates| updates.into_iter().map(|u| u.update).collect())
            .unwrap_or_default()
    }
}

/// Resolve the reference path and the directory that empty-parent lock cleanup must preserve.
/// Keep the physical `refs` root for discovery; pseudorefs retain their containing directory.
///
/// For a resolved base of `/repo/.git`, relative paths produce these cleanup boundaries:
///
/// * `refs/heads/topic` -> `/repo/.git/refs`
/// * `refs/namespaces/team/refs/heads/topic` -> `/repo/.git/refs`
/// * `HEAD` -> `/repo/.git`
/// * `worktrees/other/HEAD` -> `/repo/.git/worktrees/other`
/// * `worktrees/other/refs/bisect/bad` -> `/repo/.git/worktrees/other/refs`
/// * `worktrees/refs/refs/bisect/bad` -> `/repo/.git/worktrees/refs/refs`
///
/// The store resolves namespaces and worktree aliases before selecting the boundary.
pub(in crate::store_impl::file) fn reference_path_and_lock_boundary(
    store: &file::Store,
    name: &FullNameRef,
) -> Result<(PathBuf, PathBuf)> {
    let (base, relative_path) = store.reference_path_with_base(name)?;
    let reference_path = base.join(relative_path.as_ref());
    let mut lock_boundary = base.into_owned();
    let worktree_prefix = relative_path.starts_with("worktrees");
    for (index, component) in relative_path
        .parent()
        .expect("reference paths have a parent")
        .components()
        .enumerate()
    {
        lock_boundary.push(component);
        if component.as_os_str() == "refs" && (!worktree_prefix || index >= 2) {
            break;
        }
    }
    Ok((reference_path, lock_boundary))
}

fn possibly_adjust_name_for_prefixes(name: &FullNameRef) -> Option<FullName> {
    match name.category_and_short_name() {
        Some((c, sn)) => {
            use crate::Category::*;
            let sn = FullNameRef::new_unchecked(sn);
            match c {
                Bisect | Rewritten | WorktreePrivate | LinkedPseudoRef { .. } | PseudoRef | MainPseudoRef => None,
                Tag | LocalBranch | RemoteBranch | Note => name.into(),
                MainRef | LinkedRef { .. } => sn
                    .category()
                    .is_some_and(|cat| !cat.is_worktree_private())
                    .then_some(sn),
            }
            .map(ToOwned::to_owned)
        }
        None => Some(name.to_owned()), // allow (uncategorized/very special) refs to be packed
    }
}

/// A reference changed since the caller obtained its expected value.
/// Retrying requires reading and reconciling the new value first.
#[derive(Debug)]
pub struct ReferenceOutOfDate {
    /// The reference whose target did not match the expected value.
    pub full_name: crate::bstr::BString,
    /// The target observed while holding the reference lock.
    pub actual: Target,
}

impl std::fmt::Display for ReferenceOutOfDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "The reference {:?} changed to {}", self.full_name, self.actual)
    }
}

impl std::error::Error for ReferenceOutOfDate {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(const { &ClassificationMarker::CONFLICT })
    }
}

/// A reference exists, no matter its target, although the edit required its absence.
#[derive(Debug)]
pub struct MustNotExist {
    /// The reference which unexpectedly exists.
    pub full_name: crate::bstr::BString,
    /// The target observed while holding the reference lock.
    pub actual: Target,
}

impl std::fmt::Display for MustNotExist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The reference {:?} already exists with content {}",
            self.full_name, self.actual
        )
    }
}

impl std::error::Error for MustNotExist {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(const { &ClassificationMarker::CONFLICT })
    }
}
