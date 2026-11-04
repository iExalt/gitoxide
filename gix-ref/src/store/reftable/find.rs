//! Finding references, following symbolic ones and peeling them, and the pinned view that [`Snapshot`] provides.
use std::collections::{BTreeMap, BTreeSet};

use gix_error::{ErrorExt, Message, OptionExt, Result, ResultExt, bail, corruption, message, not_found};
use gix_hash::ObjectId;
use gix_object::bstr::{BStr, BString, ByteSlice};
use gix_parallel::Mutable;
use gix_reftable::{
    record::{RefRecord, RefValue},
    stack::Snapshot as Tables,
};

use super::stacks::StackId;
use crate::{
    FullName, FullNameRef, PartialNameRef, Reference, Target,
    file::find::NotFound,
    name::is_pseudo_ref,
    reftable::{Platform, Store},
};

/// Provides the state of the stacks that the lookups it performs use.
///
/// If it is `pinned`, every stack is read once and used from then on, which is what a [`Snapshot`] needs.
/// Otherwise, every lookup looks for changes in the stack it needs, like Git does for single references.
pub(crate) struct View<'s> {
    store: &'s Store,
    pinned: Option<Mutable<BTreeMap<StackId, Tables>>>,
}

impl<'s> View<'s> {
    pub fn fresh(store: &'s Store) -> Self {
        View { store, pinned: None }
    }

    /// Read the stacks which are certain to be needed, which are the main one and the one of the linked worktree.
    pub fn pinned(store: &'s Store) -> Result<Self> {
        let mut pinned = BTreeMap::new();
        pinned.insert(StackId::Main, store.stack_snapshot(&StackId::Main, true)?);
        if store.has_worktree_stack() {
            pinned.insert(StackId::Worktree, store.stack_snapshot(&StackId::Worktree, true)?);
        }
        Ok(View {
            store,
            pinned: Some(Mutable::new(pinned)),
        })
    }

    pub fn store(&self) -> &'s Store {
        self.store
    }

    fn snapshot(&self, id: &StackId) -> Result<Tables> {
        let Some(pinned) = &self.pinned else {
            return self.store.stack_snapshot(id, true);
        };
        if let Some(snapshot) = gix_parallel::lock(pinned).get(id) {
            return Ok(snapshot.clone());
        }
        let snapshot = self.store.stack_snapshot(id, true)?;
        // Another thread may have pinned the stack in the meantime, and all lookups have to use what was pinned first.
        Ok(gix_parallel::lock(pinned).entry(id.clone()).or_insert(snapshot).clone())
    }

    /// Return the pinned snapshots of the main stack and the stack of the linked worktree. Only for pinned views.
    pub fn main_and_worktree(&self) -> (Tables, Option<Tables>) {
        let pinned = gix_parallel::lock(self.pinned.as_ref().expect("only called for pinned views"));
        (
            pinned.get(&StackId::Main).expect("pinned on creation").clone(),
            pinned.get(&StackId::Worktree).cloned(),
        )
    }

    /// Read the reference `name`, which is not namespaced, or return `None` if it doesn't exist.
    pub fn read(&self, name: &FullNameRef) -> Result<Option<Reference>> {
        let namespaced;
        let lookup = match &self.store.namespace {
            Some(namespace) => {
                namespaced = namespace.clone().into_namespaced_name(name);
                namespaced.as_bstr()
            }
            None => name.as_bstr(),
        };
        let Some(mut reference) = self.read_raw(lookup)? else {
            return Ok(None);
        };
        if let Some(namespace) = &self.store.namespace {
            reference.strip_namespace(namespace);
        }
        Ok(Some(reference))
    }

    fn read_raw(&self, name: &BStr) -> Result<Option<Reference>> {
        self.store.assure_backend_supported()?;
        if is_special_head(name) {
            return self.store.read_special_head(name);
        }
        let (id, local_name) = self.store.route(name);
        if local_name.is_empty() {
            return Ok(None);
        }
        let tables = self.snapshot(&id)?;
        let record = tables
            .find_ref(local_name)
            .or_raise(|| message("Could not look up a reference in a reftable").with("reference", name))?;
        record
            .map(|record| reference_from_record(name, record.value))
            .transpose()
            .map(Option::flatten)
    }

    /// Find the reference by `partial` name using Git's rules to complete it, and let `accept` skip candidates.
    pub fn find(
        &self,
        partial: &PartialNameRef,
        mut accept: impl FnMut(&Reference) -> bool,
    ) -> Result<Option<Reference>> {
        let mut buf = BString::default();
        for consider_pseudo_ref in [true, false] {
            if !consider_pseudo_ref && !is_pseudo_ref(partial.as_bstr()) {
                break;
            }
            'try_directories: for inbetween in &["", "tags", "heads", "remotes"] {
                let full_name = partial.construct_full_name_ref(inbetween, &mut buf, consider_pseudo_ref);
                match self.read(full_name)? {
                    Some(reference) if accept(&reference) => return Ok(Some(reference)),
                    Some(_) => continue,
                    None => {
                        if consider_pseudo_ref && is_pseudo_ref(partial.as_bstr()) {
                            break 'try_directories;
                        }
                    }
                }
            }
        }
        if partial.as_bstr() == "HEAD" {
            return Ok(None);
        }
        let remote_head = partial
            .to_owned()
            .join("HEAD".into())
            .expect("HEAD is a valid name component");
        let full_name =
            AsRef::<PartialNameRef>::as_ref(&remote_head).construct_full_name_ref("remotes", &mut buf, true);
        Ok(self.read(full_name)?.filter(|reference| accept(reference)))
    }

    pub fn find_resolved(&self, partial: &PartialNameRef) -> Result<Option<Reference>> {
        let mut resolution_error = None;
        let found = self.find(partial, |reference| {
            match self.follow_to_object(&mut reference.clone()) {
                Ok(_) => true,
                Err(err) => {
                    resolution_error.get_or_insert(err);
                    false
                }
            }
        })?;
        match found {
            Some(reference) => Ok(Some(reference)),
            None => resolution_error.map_or(Ok(None), Err),
        }
    }

    pub fn follow(&self, reference: &Reference) -> Option<Result<Reference>> {
        match &reference.target {
            Target::Object(_) => None,
            Target::Symbolic(name) => Some(match self.read(name.as_ref()) {
                Ok(Some(next)) => Ok(next),
                Ok(None) => Err(NotFound {
                    name: name.as_bstr().to_owned(),
                }
                .raise()),
                Err(err) => Err(err),
            }),
        }
    }

    pub fn follow_to_object(&self, reference: &mut Reference) -> Result<ObjectId> {
        match reference.target {
            Target::Object(id) => Ok(id),
            Target::Symbolic(_) => {
                const MAX_REF_DEPTH: usize = 5;
                let mut seen = BTreeSet::new();
                let cursor = &mut *reference;
                while let Some(next) = self.follow(cursor) {
                    let next = next?;
                    if seen.contains(&next.name) {
                        bail!(corruption("Aborting symbolic reference cycle").with("reference", cursor.name.as_bstr()));
                    }
                    *cursor = next;
                    seen.insert(cursor.name.clone());
                    if seen.len() == MAX_REF_DEPTH {
                        bail!(Message::new("Symbolic reference depth limit exceeded").with("max_depth", MAX_REF_DEPTH));
                    }
                }
                Ok(reference
                    .target
                    .try_id()
                    .expect("a symbolic chain ends in an object")
                    .to_owned())
            }
        }
    }

    pub fn peel_to_id(&self, reference: &mut Reference, objects: &dyn gix_object::Find) -> Result<ObjectId> {
        if let Some(peeled) = reference.peeled {
            reference.target = Target::Object(peeled);
            return Ok(peeled);
        }
        let mut object_id = self.follow_to_object(reference)?;
        let mut buf = Vec::new();
        let peeled_id = loop {
            let gix_object::Data {
                kind,
                data,
                object_hash,
            } = objects
                .try_find(&object_id, &mut buf)
                .or_raise(|| {
                    Message::new("Could not peel reference to an object")
                        .with("object_id", object_id.to_string())
                        .with("reference", reference.name.as_bstr())
                })?
                .ok_or_raise(|| {
                    not_found("Could not peel reference to an object: object could not be found")
                        .with("object_id", object_id.to_string())
                        .with("reference", reference.name.as_bstr())
                })?;
            match kind {
                gix_object::Kind::Tag => {
                    object_id = gix_object::TagRefIter::from_bytes(data, object_hash)
                        .target_id()
                        .or_raise(|| {
                            message!(
                                "Could not decode tag {object_id} as referred to by {:?}",
                                reference.name.0
                            )
                            .corrupted()
                        })?;
                }
                _ => break object_id,
            }
        };
        reference.peeled = Some(peeled_id);
        reference.target = Target::Object(peeled_id);
        Ok(peeled_id)
    }
}

/// The pseudo references that Git keeps in files even if everything else is in reftables.
pub(crate) fn is_special_head(name: &BStr) -> bool {
    name == "FETCH_HEAD" || name == "MERGE_HEAD"
}

/// Turn the record for `name` into a reference, or return `None` if it is a tombstone.
pub(crate) fn reference_from_record(name: &BStr, value: RefValue) -> Result<Option<Reference>> {
    let (target, peeled) = match value {
        RefValue::Deletion => return Ok(None),
        RefValue::Val1(id) => (Target::Object(id), None),
        RefValue::Val2 { value, target_value } => (Target::Object(value), Some(target_value)),
        RefValue::Symref(target) => {
            gix_validate::reference::name(target.as_bstr()).or_raise(|| {
                corruption("A symbolic reference in a reftable has an invalid target")
                    .with("reference", name)
                    .with("target", target.clone())
            })?;
            (Target::Symbolic(FullName(target)), None)
        }
    };
    Ok(Some(Reference {
        name: FullName(name.to_owned()),
        target,
        peeled,
    }))
}

/// Convert a record into a reference whose name is checked, for use by iterators.
pub(crate) fn validated_reference(record: RefRecord) -> Result<Option<Reference>> {
    gix_validate::reference::name(record.refname.as_bstr()).or_raise(|| {
        corruption("A reference in a reftable has an invalid name").with("reference", record.refname.clone())
    })?;
    reference_from_record(record.refname.as_bstr(), record.value)
}

impl Store {
    /// Read `FETCH_HEAD` or `MERGE_HEAD` from the file Git keeps it in, like Git does for these pseudo references.
    fn read_special_head(&self, name: &BStr) -> Result<Option<Reference>> {
        self.assure_backend_supported()?;
        let path = self.git_dir().join(gix_path::from_bstr(name)?);
        let content = match std::fs::read(&path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err.and_raise(message("Could not read a reference").with("path", path)));
            }
        };
        let loose =
            crate::file::loose::Reference::try_from_path(FullName(name.to_owned()), &content, self.object_hash())
                .or_raise(|| message("Could not decode a reference").with("path", path))?;
        Ok(Some(loose.into()))
    }
}

/// Lookup
impl Store {
    /// Find a single reference by `partial` name, which is required to be a valid reference name, and return `None` if
    /// it doesn't exist.
    ///
    /// Names are completed like Git does, so `main` finds `refs/heads/main` unless there is a `main` or `refs/main`.
    /// References that are private to a worktree are looked up in the stack of that worktree, and so are those of other
    /// worktrees as `worktrees/<name>/HEAD`. `main-worktree/HEAD` is the one of the main worktree.
    /// The stack that holds the reference is read again, so that references that were changed by others since are seen.
    pub fn try_find<'n, Name, E>(&self, partial: Name) -> Result<Option<Reference>>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        let partial = partial
            .try_into()
            .or_raise(|| message("The ref name or path is not a valid ref name"))?;
        View::fresh(self).find(partial, |_| true)
    }

    /// Like [`try_find()`](Self::try_find()), but a missing reference is an error of class
    /// [`NotFound`](gix_error::Class::NotFound).
    pub fn find<'n, Name, E>(&self, partial: Name) -> Result<Reference>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        let partial = partial
            .try_into()
            .or_raise(|| message("The ref name or path is not a valid ref name"))?;
        self.try_find::<_, std::convert::Infallible>(partial)?
            .ok_or_else(|| not_found_error(partial))
    }

    /// Find a reference whose symbolic targets can be resolved, skipping dangling or cyclic candidates.
    /// Return `None` if no reference exists, and otherwise see [`file::Store::try_find_resolved()`](crate::file::Store::try_find_resolved()).
    pub fn try_find_resolved<'n, Name, E>(&self, partial: Name) -> Result<Option<Reference>>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        let partial = partial
            .try_into()
            .or_raise(|| message("The ref name or path is not a valid ref name"))?;
        View::fresh(self).find_resolved(partial)
    }

    /// Like [`try_find_resolved()`](Self::try_find_resolved()), but a missing reference is an error of class
    /// [`NotFound`](gix_error::Class::NotFound).
    pub fn find_resolved<'n, Name, E>(&self, partial: Name) -> Result<Reference>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        let partial = partial
            .try_into()
            .or_raise(|| message("The ref name or path is not a valid ref name"))?;
        self.try_find_resolved::<_, std::convert::Infallible>(partial)?
            .ok_or_else(|| not_found_error(partial))
    }

    /// Read the stacks of the repository as they are, and return a view which uses them from then on.
    pub fn snapshot(&self) -> Result<Snapshot<'_>> {
        Ok(Snapshot {
            view: View::pinned(self)?,
        })
    }
}

fn not_found_error(partial: &PartialNameRef) -> gix_error::Error {
    NotFound {
        name: partial.as_bstr().to_owned(),
    }
    .raise()
}

/// A view of the references that was pinned when it was created, to look up and iterate references that are consistent
/// with each other even though others change them.
///
/// Each stack is read once, when it is needed first, and the main stack and the one of a linked worktree when the
/// snapshot is created. Reflogs aren't part of it, they are read when asked for.
pub struct Snapshot<'s> {
    view: View<'s>,
}

impl<'s> Snapshot<'s> {
    /// Return the store this is a snapshot of.
    pub fn store(&self) -> &'s Store {
        self.view.store()
    }

    /// Find a single reference by `partial` name and return `None` if it doesn't exist.
    pub fn try_find<'n, Name, E>(&self, partial: Name) -> Result<Option<Reference>>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        let partial = partial
            .try_into()
            .or_raise(|| message("The ref name or path is not a valid ref name"))?;
        self.view.find(partial, |_| true)
    }

    /// Like [`try_find()`](Self::try_find()), but a missing reference is an error.
    pub fn find<'n, Name, E>(&self, partial: Name) -> Result<Reference>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        let partial = partial
            .try_into()
            .or_raise(|| message("The ref name or path is not a valid ref name"))?;
        self.try_find::<_, std::convert::Infallible>(partial)?
            .ok_or_else(|| not_found_error(partial))
    }

    /// Return a platform to iterate over the pinned references.
    pub fn platform(&self) -> Platform<'s> {
        let (main, worktree) = self.view.main_and_worktree();
        Platform::new(self.view.store(), main, worktree)
    }

    /// See [`Reference::peel_to_id()`](crate::file::ReferenceExt::peel_to_id()), but using the pinned stacks.
    pub fn peel_to_id(&self, reference: &mut Reference, objects: &dyn gix_object::Find) -> Result<ObjectId> {
        self.view.peel_to_id(reference, objects)
    }

    /// Follow all symbolic references `reference` points to, using the pinned stacks, altering it in place.
    pub fn follow_to_object(&self, reference: &mut Reference) -> Result<ObjectId> {
        self.view.follow_to_object(reference)
    }

    /// Follow `reference` one level, using the pinned stacks, or return `None` if it is not symbolic.
    pub fn follow(&self, reference: &Reference) -> Option<Result<Reference>> {
        self.view.follow(reference)
    }
}
