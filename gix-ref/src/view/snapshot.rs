use gix_error::Result;
use gix_hash::ObjectId;

use crate::{
    PartialNameRef, Reference,
    file::{self, ReferenceExt},
    view::{Platform, StoreRef},
};
use gix_error::ResultExt;

/// A pinned view of a reference store, to perform multiple lookups, iterations and peeling operations that share the
/// state of what they are looking at.
///
/// Get it with [`StoreRef::snapshot()`]. What is pinned depends on the storage: files pin `packed-refs` only,
/// whereas loose references are read when asked for.
pub struct Snapshot<'a> {
    inner: Inner<'a>,
}

enum Inner<'a> {
    File {
        store: &'a file::Store,
        packed: Option<file::packed::SharedBufferSnapshot>,
    },
}

impl<'a> Snapshot<'a> {
    pub(crate) fn new_file(store: &'a file::Store, packed: Option<file::packed::SharedBufferSnapshot>) -> Self {
        Snapshot {
            inner: Inner::File { store, packed },
        }
    }
}

impl<'a> Snapshot<'a> {
    /// Return the store this is a snapshot of.
    pub fn store(&self) -> StoreRef<'a> {
        match &self.inner {
            Inner::File { store, .. } => StoreRef::from(*store),
        }
    }

    /// Find a single reference by `partial` name and return `None` if it doesn't exist, using the pinned state.
    pub fn try_find<'n, Name, E>(&self, partial: Name) -> Result<Option<Reference>>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        match &self.inner {
            Inner::File { store, packed } => store.try_find_packed(partial, packed.as_ref().map(|b| &***b)),
        }
    }

    /// Like [`try_find()`](Self::try_find()), but a missing reference is an error of class
    /// [`NotFound`](gix_error::Class::NotFound).
    pub fn find<'n, Name, E>(&self, partial: Name) -> Result<Reference>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        match &self.inner {
            Inner::File { store, packed } => store.find_packed(partial, packed.as_ref().map(|b| &***b)),
        }
    }

    /// Return a platform to obtain iterators over all references or a subset of them, using the pinned state.
    pub fn iter(&self) -> Platform<'a> {
        match &self.inner {
            Inner::File { store, packed } => Platform::new_file(store.platform_with_snapshot(packed.clone())),
        }
    }

    /// Follow all symbolic targets `reference` might point to and peel the underlying object to the end of the tag-chain,
    /// returning the first non-tag object the annotated tag points to, using `objects` to access them.
    ///
    /// Note that this mutates `reference` in place if it does not already point to a non-symbolic object.
    ///
    /// Object lookup failures include [metadata](gix_error::Error::metadata()) `object_id` (hex text) and `reference`
    /// (name bytes). Missing objects are classified as not found.
    pub fn peel_to_id(&self, reference: &mut Reference, objects: &dyn gix_object::Find) -> Result<ObjectId> {
        match &self.inner {
            Inner::File { store, packed } => {
                reference.peel_to_id_packed(store, objects, packed.as_ref().map(|b| &***b))
            }
        }
    }

    /// Follow all symbolic references `reference` points to while gracefully handling loops, altering it in place.
    ///
    /// Cycle failures include [metadata](gix_error::Error::metadata()) `path` (native path); depth-limit failures include
    /// `max_depth` (unsigned integer).
    pub fn follow_to_object(&self, reference: &mut Reference) -> Result<ObjectId> {
        match &self.inner {
            Inner::File { store, packed } => reference.follow_to_object_packed(store, packed.as_ref().map(|b| &***b)),
        }
    }

    /// Follow `reference` one level if it is symbolic and return the reference it refers to.
    ///
    /// Return `None` if it is not symbolic, hence the leaf of the chain.
    pub fn follow(&self, reference: &Reference) -> Option<Result<Reference>> {
        match &self.inner {
            Inner::File { store, packed } => reference.follow_packed(store, packed.as_ref().map(|b| &***b)),
        }
    }
}
