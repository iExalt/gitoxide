//! Reading reflogs.
use gix_error::{Result, ResultExt, message};
use gix_object::bstr::BString;
use gix_reftable::{
    record::{LogRecord, LogValue},
    stack::Snapshot as Tables,
};

use crate::{FullNameRef, log::Line, reftable::Store};

impl Store {
    /// Return the tables that hold the reflog of `name`, and the name the reference has in them.
    ///
    /// The stack is read again to see what others wrote since, as a store can live much longer than the Git process that
    /// Git's own choice of not reloading for iterating reflogs is made for.
    fn reflog_tables(&self, name: &FullNameRef) -> Result<(Tables, BString)> {
        let namespaced;
        let lookup = match &self.namespace {
            Some(namespace) => {
                namespaced = namespace.clone().into_namespaced_name(name);
                namespaced.as_bstr()
            }
            None => name.as_bstr(),
        };
        let (id, local_name) = self.route(lookup);
        let local_name = local_name.to_owned();
        Ok((self.stack_snapshot(&id, true)?, local_name))
    }

    /// Return the lines of the log of the reference `name`, or `None` if there is no log. They start with the most
    /// recent one if `rev` is set, and with the oldest one otherwise.
    pub(crate) fn reflog(&self, name: &FullNameRef, rev: bool) -> Result<Option<std::vec::IntoIter<Result<Line>>>> {
        let (tables, local_name) = self.reflog_tables(name)?;
        if local_name.is_empty() {
            return Ok(None);
        }
        let records = tables
            .reflog(&local_name)
            .or_raise(|| message("Could not read a reflog").with("reference", name.as_bstr()))?;
        let mut lines = Vec::new();
        let mut exists = false;
        for record in records {
            exists = true;
            match record {
                Ok(record) => lines.extend(line_from_record(record).map(Ok)),
                Err(err) => lines.push(Err(err)),
            }
        }
        if !exists {
            return Ok(None);
        }
        if !rev {
            lines.reverse();
        }
        Ok(Some(lines.into_iter()))
    }

    /// Return `true` if a reflog exists for `name`, even if it doesn't have lines, as Git creates one that way to
    /// mark that the log is wanted.
    pub(crate) fn reflog_exists<'a, Name, E>(&self, name: Name) -> Result<bool>
    where
        Name: TryInto<&'a FullNameRef, Error = E>,
        std::result::Result<&'a FullNameRef, E>: ResultExt<Success = &'a FullNameRef>,
    {
        let name = name
            .try_into()
            .or_raise(|| message("The reference name is not valid"))?;
        let (tables, local_name) = self.reflog_tables(name)?;
        if local_name.is_empty() {
            return Ok(false);
        }
        match tables
            .reflog(&local_name)
            .or_raise(|| message("Could not read a reflog").with("reference", name.as_bstr()))?
            .next()
        {
            Some(Err(err)) => Err(err),
            Some(Ok(_)) => Ok(true),
            None => Ok(false),
        }
    }
}

/// Convert a record into a line, or return `None` for tombstones and for the marker that a reflog exists.
fn line_from_record(record: LogRecord) -> Option<Line> {
    let LogValue::Update {
        old_hash,
        new_hash,
        name,
        email,
        time,
        tz_offset,
        mut message,
    } = record.value
    else {
        return None;
    };
    if old_hash.is_null() && new_hash.is_null() {
        return None;
    }
    if message.last() == Some(&b'\n') {
        message.pop();
    }
    let sign = if tz_offset < 0 { -1 } else { 1 };
    let hhmm = i32::from(tz_offset).abs();
    let offset = sign * ((hhmm / 100) * 3600 + (hhmm % 100) * 60);
    Some(Line {
        previous_oid: old_hash,
        new_oid: new_hash,
        signature: gix_actor::Signature {
            name,
            email,
            time: gix_actor::date::Time {
                seconds: i64::try_from(time).unwrap_or(i64::MAX),
                offset,
            },
        },
        message,
    })
}
