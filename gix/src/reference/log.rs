//!
#![allow(clippy::empty_docs)]
use gix_object::commit::MessageRef;

use crate::{
    Reference,
    bstr::{BStr, BString, ByteVec},
};

impl Reference<'_> {
    /// Return a platform for obtaining iterators over reference logs.
    pub fn log_iter(&self) -> gix_ref::view::ReflogPlatform<'_, '_> {
        self.repo.ref_store().reflog(self.inner.name.as_ref())
    }

    /// Return true if a reflog is present for this reference.
    pub fn log_exists(&self) -> crate::Result<bool> {
        self.repo.ref_store().reflog_exists(self.inner.name.as_ref())
    }
}

/// Generate a message typical for git commit logs based on the given `operation`, commit `message` and `num_parents` of the commit.
pub fn message(operation: &str, message: &BStr, num_parents: usize) -> BString {
    let mut out = BString::from(operation);
    if let Some(commit_type) = commit_type_by_parents(num_parents) {
        out.push_str(b" (");
        out.extend_from_slice(commit_type.as_bytes());
        out.push_byte(b')');
    }
    out.push_str(b": ");
    out.extend_from_slice(&MessageRef::from_bytes(message).summary());
    out
}

pub(crate) fn commit_type_by_parents(count: usize) -> Option<&'static str> {
    Some(match count {
        0 => "initial",
        1 => return None,
        _two_or_more => "merge",
    })
}
