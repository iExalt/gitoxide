//! A backend-neutral, borrowed view of a reference store.
//!
//! [`StoreRef`] is the way to use references without committing to the way they are stored, and with room for more
//! backends than the [`file::Store`]. All its reads, iterations, reflog access and transactions are
//! available on it, and a [`Snapshot`] provides a consistent view for operations that need one.
//!
//! The view is [`Copy`] and borrows the store it was created from, so it never gets out of sync with it.
use std::path::Path;

use gix_error::{Result, ResultExt, message};

use crate::{FullNameRef, Namespace, PartialNameRef, Reference, file, store::WriteReflog};

mod iter;
pub use iter::{Iter, Platform};

mod reflog;
pub use reflog::ReflogPlatform;

mod snapshot;
pub use snapshot::Snapshot;

pub mod transaction;
pub use transaction::Transaction;

/// The error that [`StoreRef::find()`], [`StoreRef::find_resolved()`] and the lookups of a [`Snapshot`] raise for a
/// reference that doesn't exist, which includes a missing referent of a symbolic reference.
///
/// Every kind of storage raises it, so that callers can recover from a missing reference, and learn its name, without
/// knowing how references are stored. It is also classified as [`NotFound`](gix_error::Class::NotFound).
pub use crate::file::find::NotFound;

/// The kind of storage that backs a [`StoreRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Kind {
    /// References are stored in loose files, and in `packed-refs`.
    Files,
    /// References and their reflogs are stored in stacks of reftables.
    Reftable,
}

/// A borrowed, backend-neutral view of a reference store, to find, iterate and edit references.
///
/// Create it from a store with `StoreRef::from(&store)`.
#[derive(Clone, Copy)]
pub struct StoreRef<'a> {
    backend: Backend<'a>,
}

#[derive(Clone, Copy)]
enum Backend<'a> {
    File(&'a file::Store),
    #[cfg(feature = "reftable")]
    Reftable(&'a crate::reftable::Store),
}

#[cfg(feature = "reftable")]
impl<'a> From<&'a crate::reftable::Store> for StoreRef<'a> {
    fn from(store: &'a crate::reftable::Store) -> Self {
        StoreRef {
            backend: Backend::Reftable(store),
        }
    }
}

impl<'a> From<&'a file::Store> for StoreRef<'a> {
    fn from(store: &'a file::Store) -> Self {
        StoreRef {
            backend: Backend::File(store),
        }
    }
}

impl std::fmt::Debug for StoreRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.backend {
            Backend::File(store) => f.debug_tuple("StoreRef").field(store).finish(),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => f.debug_tuple("StoreRef").field(store).finish(),
        }
    }
}

/// Access
impl<'a> StoreRef<'a> {
    /// Return the kind of storage this is a view of.
    pub fn kind(self) -> Kind {
        match self.backend {
            Backend::File(_) => Kind::Files,
            #[cfg(feature = "reftable")]
            Backend::Reftable(_) => Kind::Reftable,
        }
    }

    /// Return the `.git` directory at which references are loaded.
    ///
    /// For linked worktrees, this is the private reference location, and [`common_dir()`](Self::common_dir()) is
    /// `Some(parent_git_dir)`.
    pub fn git_dir(self) -> &'a Path {
        match self.backend {
            Backend::File(store) => store.git_dir(),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.git_dir(),
        }
    }

    /// If this is a linked worktree, return the directory of the parent repository which holds shared references.
    pub fn common_dir(self) -> Option<&'a Path> {
        match self.backend {
            Backend::File(store) => store.common_dir(),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.common_dir(),
        }
    }

    /// Return the kind of hash that references point to.
    pub fn object_hash(self) -> gix_hash::Kind {
        match self.backend {
            Backend::File(store) => store.object_hash(),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.object_hash(),
        }
    }

    /// Return the namespace that limits all reads and writes, if any.
    pub fn namespace(self) -> Option<&'a Namespace> {
        match self.backend {
            Backend::File(store) => store.namespace.as_ref(),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.namespace.as_ref(),
        }
    }

    /// Return the way reflogs are written.
    pub fn write_reflog(self) -> WriteReflog {
        match self.backend {
            Backend::File(store) => store.write_reflog,
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.write_reflog,
        }
    }

    /// Return the name of the unsupported reference storage backend that all operations are refused for, if any.
    pub fn unsupported_backend(self) -> Option<&'a str> {
        match self.backend {
            Backend::File(store) => store.unsupported_backend(),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.unsupported_backend(),
        }
    }

    /// Fail with an error of class [`Unsupported`](gix_error::Class::Unsupported) if
    /// [`unsupported_backend()`](Self::unsupported_backend()) is set.
    pub fn assure_backend_supported(self) -> Result<()> {
        match self.backend {
            Backend::File(store) => store.assure_backend_supported(),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.assure_backend_supported(),
        }
    }

    /// Return `Some(true)` if this is a freshly initialized reference store without any observable changes, with `HEAD`
    /// still pointing to `default_ref`, or `None` if `HEAD` couldn't be read.
    pub fn is_pristine(self, default_ref: &FullNameRef) -> Option<bool> {
        match self.backend {
            Backend::File(store) => store.is_pristine(default_ref),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.is_pristine(default_ref),
        }
    }

    /// Return the files store this is a view of, or `None` for other kinds of storage.
    ///
    /// Use it only where files are what matters, as it does not work for other storage.
    pub fn as_file(self) -> Option<&'a file::Store> {
        match self.backend {
            Backend::File(store) => Some(store),
            #[cfg(feature = "reftable")]
            Backend::Reftable(_) => None,
        }
    }
}

/// Lookup
impl<'a> StoreRef<'a> {
    /// Find a single reference by `partial` name, which is required to be a valid reference name, and return `None` if
    /// it doesn't exist. A reference that can't be looked up, for instance as the storage is unsupported, is an error
    /// and never reported as missing.
    pub fn try_find<'n, Name, E>(self, partial: Name) -> Result<Option<Reference>>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        match self.backend {
            Backend::File(store) => store.try_find(partial),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.try_find(partial),
        }
    }

    /// Like [`try_find()`](Self::try_find()), but a missing reference is an error of class
    /// [`NotFound`](gix_error::Class::NotFound).
    pub fn find<'n, Name, E>(self, partial: Name) -> Result<Reference>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        match self.backend {
            Backend::File(store) => store.find(partial),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.find(partial),
        }
    }

    /// Find a reference whose symbolic targets can be resolved, skipping dangling or cyclic DWIM candidates.
    /// Return `None` if no reference exists.
    pub fn try_find_resolved<'n, Name, E>(self, partial: Name) -> Result<Option<Reference>>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        match self.backend {
            Backend::File(store) => store.try_find_resolved(partial),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.try_find_resolved(partial),
        }
    }

    /// Like [`try_find_resolved()`](Self::try_find_resolved()), but a missing reference is an error of class
    /// [`NotFound`](gix_error::Class::NotFound).
    pub fn find_resolved<'n, Name, E>(self, partial: Name) -> Result<Reference>
    where
        Name: TryInto<&'n PartialNameRef, Error = E>,
        std::result::Result<&'n PartialNameRef, E>: ResultExt<Success = &'n PartialNameRef>,
    {
        match self.backend {
            Backend::File(store) => store.find_resolved(partial),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.find_resolved(partial),
        }
    }

    /// Pin the current state of what is shared between the lookups and iterations of the returned [`Snapshot`].
    ///
    /// How much consistency this provides depends on the storage: files give no point-in-time view of loose references,
    /// which are read when they are asked for, but pin `packed-refs`.
    pub fn snapshot(self) -> Result<Snapshot<'a>> {
        match self.backend {
            Backend::File(store) => store
                .cached_packed_buffer()
                .map(|packed| Snapshot::new_file(store, packed))
                .or_raise(|| message("Could not obtain a snapshot of the references")),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.snapshot().map(Snapshot::new_reftable),
        }
    }

    /// Return a platform to obtain iterators over all references or a subset of them, sorted by name.
    pub fn iter(self) -> Result<Platform<'a>> {
        match self.backend {
            Backend::File(store) => store.iter().map(Platform::new_file),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.iter().map(Platform::new_reftable),
        }
    }

    /// Return a platform to read the reflog of the reference with the given fully qualified `name`.
    pub fn reflog<'n>(self, name: &'n FullNameRef) -> ReflogPlatform<'n, 'a> {
        ReflogPlatform::new(self, name)
    }

    /// Return the names of all references that have a reflog, even an empty one, in the order of their names, for example to
    /// expire them or to find the objects they keep alive.
    ///
    /// These are the reflogs Git's `reflog list` shows, as seen from the worktree this store is for:
    ///
    /// * The references all worktrees share, and the ones private to this worktree like its `HEAD`, which are
    ///   the main worktree's own if this is the main worktree.
    /// * From the main worktree only, those that are private to the linked worktrees, named `worktrees/<id>/HEAD`,
    ///   `worktrees/<id>/refs/bisect/bad` and so on. A linked worktree doesn't see the ones of the others, nor those of the main one.
    ///
    /// Namespaces are ignored. The names are read now, and a reflog that is created or removed meanwhile may or may not be among them.
    pub fn reflog_names(self) -> Result<Vec<crate::FullName>> {
        match self.backend {
            Backend::File(store) => store.reflog_names_inner(),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.reflog_names_inner(),
        }
    }

    /// Return `true` if a reflog exists for the reference with the given fully qualified `name`.
    ///
    /// This is the fastest possible way to learn about the existence of a reflog. It might still not be readable.
    pub fn reflog_exists<'n, Name, E>(self, name: Name) -> Result<bool>
    where
        Name: TryInto<&'n FullNameRef, Error = E>,
        std::result::Result<&'n FullNameRef, E>: ResultExt<Success = &'n FullNameRef>,
    {
        match self.backend {
            Backend::File(store) => store.reflog_exists(name),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.reflog_exists(name),
        }
    }
}

/// Edit
impl<'a> StoreRef<'a> {
    /// Remove the reflog of the reference `name` completely, and return `true` if there was one.
    ///
    /// The reference itself isn't touched, and neither are the observers of transactions told about this.
    pub fn reflog_delete(self, name: &FullNameRef) -> Result<bool> {
        match self.backend {
            Backend::File(store) => store.reflog_delete_inner(name),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.reflog_delete_inner(name),
        }
    }

    /// Remove the lines of the reflog of the reference `name` for which `keep` returns `false`.
    ///
    /// `keep` is called for every line, from the oldest to the most recent one, with the value that the reference has once it
    /// is locked against other writers, resolved if it is symbolic, and `None` if it can't be resolved. A reflog that has
    /// no lines left still exists. Nothing happens if there is no reflog.
    ///
    /// Unlike Git's `reflog expire`, this doesn't rewrite the previous values of the lines that remain, and doesn't update the
    /// reference. Observers of transactions aren't told about this, as it isn't one.
    pub fn reflog_expire(
        self,
        name: &FullNameRef,
        options: crate::log::expire::Options,
        mut keep: impl FnMut(&crate::log::Line, Option<&gix_hash::oid>) -> bool,
    ) -> Result<crate::log::expire::Outcome> {
        let current = || -> Result<Option<gix_hash::ObjectId>> {
            Ok(self
                .try_find_resolved(name.as_partial_name())?
                .and_then(|reference| reference.target.try_id().map(ToOwned::to_owned)))
        };
        match self.backend {
            Backend::File(store) => store.reflog_expire_inner(name, options, &current, &mut keep),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => store.reflog_expire_inner(name, options, &current, &mut keep),
        }
    }

    /// Start a transaction to edit references.
    ///
    /// The transaction inherits the namespace of the store.
    pub fn transaction(self) -> Transaction<'a, 'a> {
        match self.backend {
            Backend::File(store) => Transaction::new_file(store.transaction()),
            #[cfg(feature = "reftable")]
            Backend::Reftable(store) => Transaction::new_reftable(store),
        }
    }
}
