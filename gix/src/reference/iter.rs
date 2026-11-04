//!
#![allow(clippy::empty_docs)]

use crate::error::Error;

use crate::bstr::{BString, ByteSlice};
use gix_path::RelativePath;

use crate::Result;

/// A platform to create iterators over references.
#[must_use = "Iterators should be obtained from this iterator platform"]
pub struct Platform<'r> {
    pub(crate) platform: gix_ref::view::Platform<'r>,
    /// The owning repository.
    pub repo: &'r crate::Repository,
}

/// An iterator over references, with or without filter.
pub struct Iter<'packed, 'repo> {
    inner: gix_ref::view::Iter<'packed, 'repo>,
    peel_snapshot: Option<gix_ref::view::Snapshot<'repo>>,
    peel: bool,
    /// Only yield references that are this name or are in this directory, if set.
    under: Option<BString>,
    repo: &'repo crate::Repository,
}

impl<'packed, 'repo> Iter<'packed, 'repo> {
    fn new(repo: &'repo crate::Repository, platform: gix_ref::view::Iter<'packed, 'repo>) -> Self {
        Iter {
            inner: platform,
            peel_snapshot: None,
            peel: false,
            under: None,
            repo,
        }
    }
}

impl<'repo> Platform<'repo> {
    /// Return an iterator over all references in the repository, excluding
    /// pseudo references.
    ///
    /// Even broken or otherwise unparsable or inaccessible references are returned and have to be handled by the caller on a
    /// case by case basis.
    pub fn all(&self) -> Result<Iter<'_, 'repo>> {
        Ok(Iter::new(self.repo, self.platform.all()?))
    }

    /// Return an iterator over all references that match the given `prefix`.
    ///
    /// These are of the form `refs/heads/` or `refs/remotes/origin`, and must not contain relative paths components like `.` or `..`.
    pub fn prefixed<'a>(&self, prefix: impl TryInto<&'a RelativePath, Error = Error>) -> Result<Iter<'_, 'repo>> {
        let prefix = prefix.try_into()?;
        Ok(Iter::new(self.repo, self.platform.prefixed(prefix)?))
    }

    /// Return an iterator over the references that are `name` or are in the directory `name`, like the pattern of
    /// `git for-each-ref` does it: `refs/heads/m` matches neither `refs/heads/main` nor `refs/heads/mine`, but it matches
    /// `refs/heads/m/x`, and `refs/heads/main` matches that reference only. A `name` that ends with a slash matches
    /// references in that directory only.
    ///
    /// [`prefixed()`](Self::prefixed()) is different, as it matches the start of a name wherever that ends.
    /// Globs are not supported, and the same restrictions apply as there. As with `prefixed()`, references that
    /// are broken are yielded as errors, even though they may not be under `name`.
    pub fn under<'a>(&self, name: impl TryInto<&'a RelativePath, Error = Error>) -> Result<Iter<'_, 'repo>> {
        let name = name.try_into()?;
        let mut iter = Iter::new(self.repo, self.platform.prefixed(name)?);
        let bytes: &[u8] = name.as_ref();
        iter.under = Some(bytes.into());
        Ok(iter)
    }

    /// Return an iterator over all references that are tags.
    ///
    /// They are all prefixed with `refs/tags`.
    pub fn tags(&self) -> Result<Iter<'_, 'repo>> {
        let prefix = b"refs/tags/".try_into()?;
        Ok(Iter::new(self.repo, self.platform.prefixed(prefix)?))
    }

    // TODO: tests
    /// Return an iterator over all local branches.
    ///
    /// They are all prefixed with `refs/heads`.
    pub fn local_branches(&self) -> Result<Iter<'_, 'repo>> {
        let prefix = b"refs/heads/".try_into()?;
        Ok(Iter::new(self.repo, self.platform.prefixed(prefix)?))
    }

    // TODO: tests
    /// Return an iterator over all local pseudo references.
    pub fn pseudo(&self) -> Result<Iter<'_, 'repo>> {
        Ok(Iter::new(self.repo, self.platform.pseudo()?))
    }

    // TODO: tests
    /// Return an iterator over all remote branches.
    ///
    /// They are all prefixed with `refs/remotes`.
    pub fn remote_branches(&self) -> Result<Iter<'_, 'repo>> {
        let prefix = b"refs/remotes/".try_into()?;
        Ok(Iter::new(self.repo, self.platform.prefixed(prefix)?))
    }
}

impl Iter<'_, '_> {
    /// Automatically peel references before yielding them during iteration.
    ///
    /// This has the same effect as using `iter.map(|r| {r.peel_to_id(); r})`.
    ///
    /// # Note
    ///
    /// Doing this is necessary as the packed-refs buffer is already held by the iterator, disallowing the consumer of the iterator
    /// to peel the returned references themselves.
    pub fn peeled(mut self) -> Result<Self> {
        self.peel_snapshot = Some(self.repo.ref_store().snapshot()?);
        self.peel = true;
        Ok(self)
    }
}

impl<'r> Iterator for Iter<'_, 'r> {
    type Item = Result<crate::Reference<'r>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let res = self.inner.next()?;
            if let (Some(under), Ok(reference)) = (&self.under, &res) {
                let name = reference.name.as_bstr();
                let is_under = match under.strip_suffix(b"/") {
                    Some(_) => name.starts_with(under.as_bytes()),
                    None => name
                        .strip_prefix(under.as_bytes())
                        .is_some_and(|rest| rest.is_empty() || rest[0] == b'/'),
                };
                if !is_under {
                    continue;
                }
            }
            return Some(self.finish(res));
        }
    }
}

impl<'r> Iter<'_, 'r> {
    fn finish(&self, res: <gix_ref::view::Iter<'_, 'r> as Iterator>::Item) -> Result<crate::Reference<'r>> {
        let mut r = res?;
        if let Some(snapshot) = self.peel_snapshot.as_ref().filter(|_| self.peel) {
            snapshot.peel_to_id(&mut r, &self.repo.objects)?;
        }
        Ok(crate::Reference::from_ref(r, self.repo))
    }
}
