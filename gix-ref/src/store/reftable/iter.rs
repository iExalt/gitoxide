//! Iteration over the references of one or two stacks.
use std::cmp::Ordering;

use gix_error::{Result, ResultExt, message};
use gix_object::bstr::{BString, ByteSlice};
use gix_path::RelativePath;
use gix_reftable::{
    merged::MergedIter,
    record::{Record, RefRecord},
    stack::Snapshot as Tables,
};

use super::{
    find::{View, is_special_head, validated_reference},
    stacks::is_current_worktree_ref,
};
use crate::{Reference, reftable::Store};

/// A platform to obtain iterators over references, which holds the state of the stacks they iterate.
///
/// The stacks are read when the platform is created, and then once more by nothing: all iterators see the same state.
#[must_use = "Iterators should be obtained from this platform"]
pub struct Platform<'s> {
    store: &'s Store,
    main: Tables,
    worktree: Option<Tables>,
}

impl<'s> Platform<'s> {
    pub(crate) fn new(store: &'s Store, main: Tables, worktree: Option<Tables>) -> Self {
        Platform { store, main, worktree }
    }

    /// Return an iterator over all references in `refs/` sorted by their name, with those private to the worktree
    /// coming from its stack if this is a linked worktree.
    pub fn all(&self) -> Result<Iter<'_>> {
        self.iter_prefix(b"refs/")
    }

    /// Like [`all()`](Self::all()), but only return references that start with `prefix`, like `refs/heads/` or
    /// `refs/heads/feature-`.
    pub fn prefixed<'p>(&'p self, prefix: &RelativePath) -> Result<Iter<'p>> {
        self.iter_prefix(prefix.as_ref())
    }

    fn iter_prefix<'p>(&'p self, prefix: &[u8]) -> Result<Iter<'p>> {
        let prefix: BString = match &self.store.namespace {
            Some(namespace) => {
                let mut namespaced = namespace.as_bstr().to_owned();
                namespaced.extend_from_slice(prefix);
                namespaced
            }
            None => prefix.into(),
        };
        let worktree = self
            .worktree
            .as_ref()
            .map(|tables| Source::new(tables, &prefix, false))
            .transpose()?;
        let main = Source::new(&self.main, &prefix, self.worktree.is_some())?;
        Ok(Iter {
            inner: Inner::Merged(Merged {
                store: self.store,
                worktree,
                main,
                prefix,
            }),
        })
    }

    /// Return an iterator over the pseudo references, like `HEAD` or `ORIG_HEAD`, or anything else suffixed with `HEAD`
    /// in the root, sorted by name. `FETCH_HEAD` and `MERGE_HEAD` are included if their files exist, as Git doesn't
    /// store them in reftables.
    pub fn pseudo(&self) -> Result<Iter<'_>> {
        let tables = self.worktree.as_ref().unwrap_or(&self.main);
        let mut found = Vec::new();
        let mut iter = tables
            .merged()
            .ref_iter()
            .or_raise(|| message("Could not iterate a reftable"))?;
        while let Some(record) = iter
            .next_record()
            .or_raise(|| message("Could not iterate a reftable"))?
        {
            let Record::Ref(record) = record else { continue };
            if record.refname.starts_with(b"refs/") {
                break;
            }
            let name = &record.refname;
            if !name.contains(&b'/')
                && name.ends_with(b"HEAD")
                && crate::name::is_pseudo_ref(name.as_bstr())
                && !is_special_head(name.as_bstr())
            {
                found.push(validated_reference(record));
            }
        }
        let view = View::fresh(self.store);
        for name in ["FETCH_HEAD", "MERGE_HEAD"] {
            let name = crate::FullName::try_from(name).expect("valid name");
            match view.read(name.as_ref()) {
                Ok(Some(reference)) => found.push(Ok(Some(reference))),
                Ok(None) => {}
                Err(err) => found.push(Err(err)),
            }
        }
        let mut items: Vec<Result<Reference>> = found.into_iter().filter_map(Result::transpose).collect();
        items.sort_by(|a, b| match (a, b) {
            (Ok(a), Ok(b)) => a.name.cmp(&b.name),
            (Err(_), Ok(_)) => Ordering::Less,
            (Ok(_), Err(_)) => Ordering::Greater,
            (Err(_), Err(_)) => Ordering::Equal,
        });
        Ok(Iter {
            inner: Inner::Collected(items.into_iter()),
        })
    }
}

/// An iterator over references sorted by their name, whose items can fail individually.
pub struct Iter<'p> {
    inner: Inner<'p>,
}

enum Inner<'p> {
    Merged(Merged<'p>),
    Collected(std::vec::IntoIter<Result<Reference>>),
}

struct Merged<'p> {
    store: &'p Store,
    worktree: Option<Source<'p>>,
    main: Source<'p>,
    prefix: BString,
}

/// The records of one stack in the range of the prefix, that are looked at one at a time.
struct Source<'p> {
    iter: MergedIter<'p>,
    peeked: Option<RefRecord>,
    exhausted: bool,
    /// Skip references that are private to a worktree, as these come from the stack of the worktree, not this one.
    skip_worktree_refs: bool,
}

impl<'p> Source<'p> {
    fn new(tables: &'p Tables, prefix: &[u8], skip_worktree_refs: bool) -> Result<Self> {
        let mut iter = tables
            .merged()
            .ref_iter()
            .or_raise(|| message("Could not iterate a reftable"))?;
        iter.seek_key(prefix)
            .or_raise(|| message("Could not seek in a reftable"))?;
        Ok(Source {
            iter,
            peeked: None,
            exhausted: false,
            skip_worktree_refs,
        })
    }

    /// Make sure the next record in range is available as `peeked`, or the source is exhausted.
    fn fill(&mut self, prefix: &[u8]) -> Result<()> {
        while self.peeked.is_none() && !self.exhausted {
            match self.iter.next_record() {
                Err(err) => {
                    self.exhausted = true;
                    return Err(err).or_raise(|| message("Could not iterate a reftable"));
                }
                Ok(None) => self.exhausted = true,
                Ok(Some(Record::Ref(record))) => {
                    if !record.refname.starts_with(prefix) {
                        self.exhausted = true;
                    } else if !(self.skip_worktree_refs && is_current_worktree_ref(&record.refname)) {
                        self.peeked = Some(record);
                    }
                }
                Ok(Some(_)) => {}
            }
        }
        Ok(())
    }
}

impl Iterator for Iter<'_> {
    type Item = Result<Reference>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            Inner::Collected(items) => items.next(),
            Inner::Merged(merged) => merged.next(),
        }
    }
}

impl Merged<'_> {
    fn next(&mut self) -> Option<Result<Reference>> {
        loop {
            if let Some(worktree) = &mut self.worktree
                && let Err(err) = worktree.fill(&self.prefix)
            {
                return Some(Err(err));
            }
            if let Err(err) = self.main.fill(&self.prefix) {
                return Some(Err(err));
            }
            let ordering = match (
                self.worktree.as_ref().and_then(|s| s.peeked.as_ref()),
                self.main.peeked.as_ref(),
            ) {
                (None, None) => return None,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(worktree), Some(main)) => worktree.refname.cmp(&main.refname),
            };
            let record = match ordering {
                Ordering::Less => self.worktree.as_mut().and_then(|s| s.peeked.take()),
                Ordering::Greater => self.main.peeked.take(),
                Ordering::Equal => {
                    self.main.peeked.take();
                    self.worktree.as_mut().and_then(|s| s.peeked.take())
                }
            }
            .expect("a record was peeked");
            match validated_reference(record) {
                Ok(Some(mut reference)) => {
                    if let Some(namespace) = &self.store.namespace {
                        reference.strip_namespace(namespace);
                    }
                    return Some(Ok(reference));
                }
                Ok(None) => continue,
                Err(err) => return Some(Err(err)),
            }
        }
    }
}

impl Store {
    /// Read the stacks and return a platform to obtain iterators over the references in them.
    pub fn iter(&self) -> Result<Platform<'_>> {
        let view = View::pinned(self)?;
        let (main, worktree) = view.main_and_worktree();
        Ok(Platform::new(self, main, worktree))
    }
}
