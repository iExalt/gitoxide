use std::path::Path;

use crate::{Namespace, StoreRef, file, store, store::WriteReflog};

mod init;

impl From<file::Store> for crate::Store {
    fn from(store: file::Store) -> Self {
        crate::Store {
            inner: store::State::Files { store },
        }
    }
}

#[cfg(feature = "reftable")]
impl From<crate::reftable::Store> for crate::Store {
    fn from(store: crate::reftable::Store) -> Self {
        crate::Store {
            inner: store::State::Reftable { store },
        }
    }
}

impl<'a> From<&'a crate::Store> for StoreRef<'a> {
    fn from(store: &'a crate::Store) -> Self {
        store.as_ref()
    }
}

/// Access
impl crate::Store {
    /// Return a borrowed view of this store to find, iterate and edit references.
    pub fn as_ref(&self) -> StoreRef<'_> {
        match &self.inner {
            store::State::Files { store } => StoreRef::from(store),
            #[cfg(feature = "reftable")]
            store::State::Reftable { store } => StoreRef::from(store),
        }
    }

    /// Return the files store that this is, or `None` for other kinds of storage.
    ///
    /// Use it only where files are what matters, as it does not work for other storage.
    pub fn as_file(&self) -> Option<&file::Store> {
        match &self.inner {
            store::State::Files { store } => Some(store),
            #[cfg(feature = "reftable")]
            store::State::Reftable { .. } => None,
        }
    }

    /// Return the reftable store that this is, or `None` for other kinds of storage.
    #[cfg(feature = "reftable")]
    pub fn as_reftable(&self) -> Option<&crate::reftable::Store> {
        match &self.inner {
            store::State::Files { .. } => None,
            store::State::Reftable { store } => Some(store),
        }
    }

    /// Like [`as_file()`](Self::as_file()), but mutable.
    pub fn as_file_mut(&mut self) -> Option<&mut file::Store> {
        match &mut self.inner {
            store::State::Files { store } => Some(store),
            #[cfg(feature = "reftable")]
            store::State::Reftable { .. } => None,
        }
    }

    /// Return the `.git` directory at which references are loaded, which is the private location of a linked worktree.
    pub fn git_dir(&self) -> &Path {
        self.as_ref().git_dir()
    }

    /// If this is a linked worktree, return the directory of the parent repository which holds shared references.
    pub fn common_dir(&self) -> Option<&Path> {
        self.as_ref().common_dir()
    }

    /// Return the kind of hash that references point to.
    pub fn object_hash(&self) -> gix_hash::Kind {
        self.as_ref().object_hash()
    }

    /// Return the namespace that limits all reads and writes, if any.
    pub fn namespace(&self) -> Option<&Namespace> {
        self.as_ref().namespace()
    }

    /// Return the way reflogs are written.
    pub fn write_reflog(&self) -> WriteReflog {
        self.as_ref().write_reflog()
    }

    /// Return the name of the unsupported reference storage backend that all operations are refused for, if any.
    pub fn unsupported_backend(&self) -> Option<&str> {
        self.as_ref().unsupported_backend()
    }
}

/// Relocation
impl crate::Store {
    /// Return a store for the same references at `git_dir` and `common_dir` instead, which is for example useful to
    /// replace relative paths with equivalent absolute ones.
    ///
    /// `common_dir` is the directory of the parent repository if this is a linked worktree, like
    /// [`common_dir()`](Self::common_dir()) returns it. All options, the namespace and a mark of unsupported storage are
    /// retained, and so is the threshold to memory-map `packed-refs`.
    pub fn with_locations(&self, git_dir: std::path::PathBuf, common_dir: Option<std::path::PathBuf>) -> crate::Store {
        match &self.inner {
            store::State::Files { store } => store.with_locations(git_dir, common_dir).into(),
            #[cfg(feature = "reftable")]
            store::State::Reftable { store } => store.with_locations(git_dir, common_dir).into(),
        }
    }
}

/// Mutation
impl crate::Store {
    /// Limit all reads and writes to `namespace`, or lift the limit with `None`, and return the previous namespace.
    pub fn set_namespace(&mut self, namespace: Option<Namespace>) -> Option<Namespace> {
        match &mut self.inner {
            store::State::Files { store } => std::mem::replace(&mut store.namespace, namespace),
            #[cfg(feature = "reftable")]
            store::State::Reftable { store } => std::mem::replace(&mut store.namespace, namespace),
        }
    }

    /// Change the way reflogs are written, and return the previous mode.
    pub fn set_write_reflog(&mut self, mode: WriteReflog) -> WriteReflog {
        match &mut self.inner {
            store::State::Files { store } => std::mem::replace(&mut store.write_reflog, mode),
            #[cfg(feature = "reftable")]
            store::State::Reftable { store } => std::mem::replace(&mut store.write_reflog, mode),
        }
    }

    /// Refuse every operation on references as the storage named `backend` is unsupported, or reset that with `None`.
    ///
    /// See [`file::Store::set_unsupported_backend()`] for details.
    pub fn set_unsupported_backend(&mut self, backend: Option<String>) {
        match &mut self.inner {
            store::State::Files { store } => store.set_unsupported_backend(backend),
            #[cfg(feature = "reftable")]
            store::State::Reftable { store } => store.set_unsupported_backend(backend),
        }
    }
}
