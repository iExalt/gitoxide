use std::fmt;

use gix_hash::ObjectId;
use gix_hashtable::hash_map::Entry;
use gix_object::Kind;

use crate::check::{Checker, Refusal};

/// A problem with an object that another object links to.
///
/// How to word it depends on the program whose message is to be reproduced: `unpack-objects` says `object of unexpected type`
/// for anything that is missing or has an unexpected kind, while `index-pack` says `did not receive expected object <oid>`
/// and `object <oid>: expected type <kind>, found <kind>` respectively, or `object type mismatch at <oid>` if both were received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// The object isn't part of what was received nor in the object database.
    Missing {
        /// The object that was linked to.
        id: ObjectId,
        /// The kind of object that is required.
        expected: Kind,
    },
    /// The object exists but isn't of the kind it is linked as.
    WrongType {
        /// The object that was linked to.
        id: ObjectId,
        /// The kind of object that is required.
        expected: Kind,
        /// The kind of object it is.
        found: Kind,
        /// If `true`, the object was among those passed to the checker, so both kinds are in what was received.
        received: bool,
    },
}

impl fmt::Display for LinkError {
    /// Format like `index-pack` does.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::Missing { id, .. } => write!(f, "did not receive expected object {id}"),
            LinkError::WrongType { id, received: true, .. } => write!(f, "object type mismatch at {id}"),
            LinkError::WrongType {
                id, expected, found, ..
            } => write!(f, "object {id}: expected type {expected}, found {found}"),
        }
    }
}

impl std::error::Error for LinkError {}

impl Checker {
    /// Note that `id`, an object of `kind`, was received, and verify that what links to it expected the same kind.
    pub(super) fn arrive(&mut self, id: &gix_hash::oid, kind: Kind) -> Result<(), Refusal> {
        if let Some(expected) = self.pending.remove(id)
            && expected != kind
        {
            return Err(Refusal::Link(LinkError::WrongType {
                id: id.to_owned(),
                expected,
                found: kind,
                received: true,
            }));
        }
        self.seen.insert(id.to_owned(), kind);
        Ok(())
    }

    /// Note that an object links to `target` which has to be of `expected` kind.
    pub(super) fn link(&mut self, target: ObjectId, expected: Kind) -> Result<(), Refusal> {
        if let Some(found) = self.seen.get(&target) {
            return if *found == expected {
                Ok(())
            } else {
                Err(Refusal::Link(LinkError::WrongType {
                    id: target,
                    expected,
                    found: *found,
                    received: true,
                }))
            };
        }
        match self.pending.entry(target) {
            Entry::Occupied(entry) if *entry.get() != expected => Err(Refusal::Link(LinkError::WrongType {
                id: target,
                expected,
                found: *entry.get(),
                received: false,
            })),
            Entry::Occupied(_) => Ok(()),
            Entry::Vacant(entry) => {
                entry.insert(expected);
                Ok(())
            }
        }
    }

    /// Look up everything that is linked to but wasn't received.
    pub(super) fn finish_links<F>(&mut self, find: &F) -> Result<(), LinkError>
    where
        F: gix_object::FindHeader + ?Sized,
    {
        for (id, expected) in std::mem::take(&mut self.pending) {
            match find.try_header(&id) {
                Ok(Some(header)) if header.kind == expected => {}
                Ok(Some(header)) => {
                    return Err(LinkError::WrongType {
                        id,
                        expected,
                        found: header.kind,
                        received: false,
                    });
                }
                Ok(None) | Err(_) => return Err(LinkError::Missing { id, expected }),
            }
        }
        Ok(())
    }
}
