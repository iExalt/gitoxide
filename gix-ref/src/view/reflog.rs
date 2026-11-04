use gix_error::{Result, ResultExt, message};

use crate::{
    FullNameRef,
    log::Line,
    view::{Backend, StoreRef},
};

/// A platform to read the reflog of a reference, created by [`StoreRef::reflog()`].
///
/// It owns the buffer that read lines are stored in.
#[must_use = "Iterators should be obtained from this platform"]
pub struct ReflogPlatform<'n, 's> {
    store: StoreRef<'s>,
    name: &'n FullNameRef,
    buf: Vec<u8>,
}

impl<'n, 's> ReflogPlatform<'n, 's> {
    pub(crate) fn new(store: StoreRef<'s>, name: &'n FullNameRef) -> Self {
        ReflogPlatform {
            store,
            name,
            buf: Vec::new(),
        }
    }

    /// Return an iterator over all log lines from the most recent to the oldest, or `None` if there is no reflog.
    ///
    /// The iterator type is not promised to be `Send` or `Sync`, as that depends on the kind of storage.
    pub fn rev(&mut self) -> Result<Option<impl Iterator<Item = Result<Line>> + '_>> {
        match self.store.backend {
            Backend::File(store) => {
                self.buf.clear();
                self.buf.resize(1024 * 4, 0);
                let path = store.reflog_path(self.name)?;
                store
                    .reflog_iter_rev_inner(&path, &mut self.buf)
                    .or_raise(|| message("Could not read reflog").with("path", path))
            }
        }
    }

    /// Return an iterator over all log lines from the oldest to the most recent, or `None` if there is no reflog.
    ///
    /// Lines are owned like those of [`rev()`](Self::rev()) as other kinds of storage don't keep them as text, and the
    /// iterator type is not promised to be `Send` or `Sync`.
    pub fn all(&mut self) -> Result<Option<impl Iterator<Item = Result<Line>> + '_>> {
        match self.store.backend {
            Backend::File(store) => {
                let path = store.reflog_path(self.name)?;
                store
                    .reflog_iter_inner(&path, &mut self.buf)
                    .map(|iter| iter.map(|iter| iter.map(|line| line.map(Line::from))))
                    .or_raise(|| message("Could not read reflog").with("path", path))
            }
        }
    }
}
