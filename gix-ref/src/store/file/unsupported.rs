//! Refusal of every reference operation on a repository whose reference storage this store cannot handle.
//!
//! A [`file::Store`] reads and writes loose references and `packed-refs`. Pointed at a repository that keeps its references
//! elsewhere, such as in `reftable`, it would report references that exist as missing and would write loose files that
//! Git never reads. A store can therefore be marked with the storage it cannot handle, after which every operation that
//! touches references fails with [`Class::Unsupported`](gix_error::Class::Unsupported).
use std::io;

use gix_error::{Result, bail};

use crate::file;

impl file::Store {
    /// Mark this store as unable to handle the reference storage backend named `backend`, like `reftable`, or reset the mark
    /// with `None`.
    ///
    /// Afterwards, every operation that finds, iterates, reads reflogs of, or edits references fails with an error of class
    /// [`Unsupported`](gix_error::Class::Unsupported), including all `try_find*()` methods which never return `Ok(None)` in
    /// that case. Methods that merely compute a path or inspect the store itself keep working.
    ///
    /// The mark is part of the store, so clones carry it. It is a safeguard against accidental misuse and not a security
    /// boundary, as everyone with mutable access to the store can reset it.
    pub fn set_unsupported_backend(&mut self, backend: Option<String>) {
        self.unsupported_backend = backend;
    }

    /// Return the name of the unsupported reference storage backend that this store refuses to operate on, if any.
    pub fn unsupported_backend(&self) -> Option<&str> {
        self.unsupported_backend.as_deref()
    }

    /// Fail with an error of class [`Unsupported`](gix_error::Class::Unsupported) if
    /// [`unsupported_backend()`](Self::unsupported_backend()) is set, which allows to refuse an operation before it has any side effect.
    pub fn assure_backend_supported(&self) -> Result<()> {
        match self.unsupported_backend.as_deref() {
            Some(backend) => bail!(gix_error::unsupported(refusal_message(backend))),
            None => Ok(()),
        }
    }

    /// Like [`assure_backend_supported()`](Self::assure_backend_supported()), but for functions returning I/O errors.
    pub(crate) fn assure_backend_supported_io(&self) -> io::Result<()> {
        match self.unsupported_backend.as_deref() {
            Some(backend) => Err(io::Error::new(io::ErrorKind::Unsupported, refusal_message(backend))),
            None => Ok(()),
        }
    }
}

fn refusal_message(backend: &str) -> String {
    format!("References are stored using the unsupported `{backend}` backend")
}
