use gix_error::{Result, ResultExt};
use gix_path::RelativePath;

use crate::{Reference, file};

/// A platform to obtain iterators over references, created by [`StoreRef::iter()`](super::StoreRef::iter()) or
/// [`Snapshot::iter()`](super::Snapshot::iter()).
///
/// It holds what is shared between the iterators it creates, so that it has to be kept alive while they are used.
#[must_use = "Iterators should be obtained from this platform"]
pub struct Platform<'s> {
    inner: PlatformInner<'s>,
}

enum PlatformInner<'s> {
    File(file::iter::Platform<'s>),
}

impl<'s> Platform<'s> {
    pub(crate) fn new_file(platform: file::iter::Platform<'s>) -> Self {
        Platform {
            inner: PlatformInner::File(platform),
        }
    }

    /// Return an iterator over all references sorted by their name.
    pub fn all<'p>(&'p self) -> Result<Iter<'p, 's>> {
        match &self.inner {
            PlatformInner::File(platform) => platform.all().map(Iter::new_file).or_error(),
        }
    }

    /// Like [`all()`](Self::all()), but only return references that start with `prefix`, like `refs/heads/` or
    /// `refs/heads/feature-`.
    ///
    /// Note that a prefix without a trailing `/`, like `refs/heads/foo`, starts the traversal in the parent directory and
    /// lists everything that starts with `foo`, like `refs/heads/foo` and `refs/heads/foobar`.
    pub fn prefixed<'p>(&'p self, prefix: &RelativePath) -> Result<Iter<'p, 's>> {
        match &self.inner {
            PlatformInner::File(platform) => platform.prefixed(prefix).map(Iter::new_file).or_error(),
        }
    }

    /// Return an iterator over the pseudo references, like `HEAD` or `FETCH_HEAD`, or anything else suffixed with `HEAD`
    /// in the root of the `.git` directory, sorted by name.
    pub fn pseudo<'p>(&'p self) -> Result<Iter<'p, 's>> {
        match &self.inner {
            PlatformInner::File(platform) => platform.pseudo().map(Iter::new_file).or_error(),
        }
    }
}

/// An iterator over references sorted by their name, whose items can fail individually.
pub struct Iter<'p, 's> {
    inner: IterInner<'p, 's>,
}

enum IterInner<'p, 's> {
    File(file::iter::LooseThenPacked<'p, 's>),
}

impl<'p, 's> Iter<'p, 's> {
    fn new_file(iter: file::iter::LooseThenPacked<'p, 's>) -> Self {
        Iter {
            inner: IterInner::File(iter),
        }
    }
}

impl Iterator for Iter<'_, '_> {
    type Item = gix_error::Result<Reference>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            IterInner::File(iter) => iter.next(),
        }
    }
}
