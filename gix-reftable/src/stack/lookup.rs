//! Find a reference or its reflog without iterating over everything.

use bstr::BString;
use gix_error::Result;

use super::Snapshot;
use crate::{
    merged::MergedIter,
    record::{LogRecord, Record, RefRecord, log_key},
};

impl Snapshot {
    /// Return the reference called `name`, or `None` if there is none, including when it was deleted.
    pub fn find_ref(&self, name: &[u8]) -> Result<Option<RefRecord>> {
        let mut iter = self.merged().ref_iter()?;
        iter.seek_key(name)?;
        Ok(match iter.next_record()? {
            Some(Record::Ref(record)) if record.refname == name => Some(record),
            _ => None,
        })
    }

    /// Iterate the reflog of the reference `name`, which is also the reflog of a reference that doesn't exist any longer, starting
    /// with the newest entry.
    pub fn reflog(&self, name: &[u8]) -> Result<Reflog<'_>> {
        let mut iter = self.merged().log_iter()?;
        iter.seek_key(&log_key(name, u64::MAX))?;
        Ok(Reflog {
            iter,
            refname: name.into(),
        })
    }
}

/// The entries of one reference's reflog, newest first, as [`Snapshot::reflog()`] returns them.
pub struct Reflog<'a> {
    iter: MergedIter<'a>,
    refname: BString,
}

impl Iterator for Reflog<'_> {
    type Item = Result<LogRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.iter.next_record() {
            Ok(Some(Record::Log(record))) if record.refname == self.refname => Some(Ok(record)),
            Ok(_) => None,
            Err(err) => Some(Err(err)),
        }
    }
}
