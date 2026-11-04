//! Removing reflogs and some of their lines, which is done by adding tombstones to a table of the stack that holds them.
//!
//! Like Git does, expiring removes the lines one by one and leaves a marker behind if no line remains, as the reflog
//! would otherwise stop to exist, which is what decides if references outside of the usual namespaces get one. Deleting
//! a reflog, on the other hand, removes it completely.
use gix_error::{Result, ResultExt, message};
use gix_hash::ObjectId;
use gix_object::bstr::{BStr, BString, ByteSlice};
use gix_reftable::{
    Stack,
    record::{LogRecord, LogValue},
    stack::{Addition, Snapshot},
};

use super::{StackOptions, Store, log::line_from_record, stacks::StackId, transaction};
use crate::{
    FullNameRef,
    log::{Line, expire},
};

/// What expiring the lines of a reflog removes.
#[derive(Debug, PartialEq, Eq)]
struct Expiry {
    /// The update index of each line that is removed, starting with the most recent one like the log does.
    removed: Vec<u64>,
    /// The number of lines that remain.
    kept: usize,
}

impl Expiry {
    /// Ask `keep` about each of `lines`, which come with their update index and start with the most recent one,
    /// from the oldest to the most recent, like Git does.
    fn new(
        lines: &[(u64, Line)],
        current: Option<&gix_hash::oid>,
        keep: &mut dyn FnMut(&Line, Option<&gix_hash::oid>) -> bool,
    ) -> Self {
        let mut removed = Vec::new();
        let mut kept = 0;
        for (update_index, line) in lines.iter().rev() {
            if keep(line, current) {
                kept += 1;
            } else {
                removed.push(*update_index);
            }
        }
        removed.reverse();
        Expiry { removed, kept }
    }

    /// Return `true` if a marker has to take the place of the lines that were removed, so that the reflog still exists.
    fn needs_marker(&self) -> bool {
        self.kept == 0 && !self.removed.is_empty()
    }

    fn outcome(&self) -> expire::Outcome {
        expire::Outcome {
            removed: self.removed.len(),
            kept: self.kept,
        }
    }
}

/// The reflog of a reference, as far as its stack is concerned.
struct Log {
    /// The name of the reference in its stack.
    local: BString,
    /// All records that aren't tombstones, starting with the most recent one.
    records: Vec<LogRecord>,
}

impl Store {
    /// Remove the reflog of `name` and return `true` if there was one.
    pub(crate) fn reflog_delete_inner(&self, name: &FullNameRef) -> Result<bool> {
        self.assure_backend_supported()?;
        let options = *self.stack_options()?;
        let Some((id, local)) = self.reflog_stack(name) else {
            return Ok(false);
        };
        let mut addition = self.lock(&id, &options)?;
        let log = read(addition.snapshot(), local)?;
        if log.records.is_empty() {
            return Ok(false);
        }
        let tombstones: Vec<_> = log.records.iter().map(|record| record.update_index).collect();
        write(&mut addition, log.local.as_bstr(), &tombstones, None)?;
        addition
            .commit()
            .or_raise(|| message("Could not delete a reflog").with("reference", name.as_bstr()))?;
        transaction::compact(self, &options, &id);
        Ok(true)
    }

    /// Remove the lines of the reflog of `name` for which `keep` returns `false`, passing the value `current` returns once
    /// the stack is locked.
    pub(crate) fn reflog_expire_inner(
        &self,
        name: &FullNameRef,
        options: expire::Options,
        current: &dyn Fn() -> Result<Option<ObjectId>>,
        keep: &mut dyn FnMut(&Line, Option<&gix_hash::oid>) -> bool,
    ) -> Result<expire::Outcome> {
        self.assure_backend_supported()?;
        let stack_options = *self.stack_options()?;
        let Some((id, local)) = self.reflog_stack(name) else {
            return Ok(expire::Outcome::default());
        };
        let error = || message("Could not expire a reflog").with("reference", name.as_bstr());
        let mut addition = if options.dry_run {
            None
        } else {
            Some(self.lock(&id, &stack_options)?)
        };
        let log = match &addition {
            Some(addition) => read(addition.snapshot(), local)?,
            None => read(&self.stack_snapshot(&id, true)?, local)?,
        };
        let lines: Vec<_> = log
            .records
            .iter()
            .filter_map(|record| line_from_record(record.clone()).map(|line| (record.update_index, line)))
            .collect();
        let expiry = Expiry::new(&lines, current()?.as_deref(), keep);
        if let (Some(addition), false) = (&mut addition, expiry.removed.is_empty()) {
            let marker = expiry.needs_marker().then(|| ObjectId::null(self.object_hash()));
            write(addition, log.local.as_bstr(), &expiry.removed, marker)?;
        }
        if let (Some(addition), false) = (addition, expiry.removed.is_empty()) {
            addition.commit().or_raise(error)?;
            transaction::compact(self, &stack_options, &id);
        }
        Ok(expiry.outcome())
    }

    /// Return the stack that holds the reflog of `name`, and the name the reference has in it, or `None` if `name` is
    /// not stored in any.
    fn reflog_stack(&self, name: &FullNameRef) -> Option<(StackId, BString)> {
        let namespaced;
        let lookup = match &self.namespace {
            Some(namespace) => {
                namespaced = namespace.clone().into_namespaced_name(name);
                namespaced.as_bstr()
            }
            None => name.as_bstr(),
        };
        let (id, local) = self.route(lookup);
        (!local.is_empty()).then(|| (id, local.to_owned()))
    }

    /// Lock the stack `id`, and pin what it holds once nobody else can change it.
    fn lock(&self, id: &StackId, options: &StackOptions) -> Result<Addition<'static>> {
        let cell = self.stacks.cell(id)?;
        let snapshot = cell.snapshot(false)?;
        let dir = cell.dir().to_owned();
        Stack::from_snapshot(&dir, self.object_hash(), *options, snapshot)
            .into_addition()
            .or_raise(|| message("Could not lock the reftable stack").with("path", dir))
    }
}

/// Read what the log of the reference `local` consists of.
fn read(snapshot: &Snapshot, local: BString) -> Result<Log> {
    let mut records = Vec::new();
    for record in snapshot
        .reflog(local.as_ref())
        .or_raise(|| message("Could not read a reflog").with("reference", local.clone()))?
    {
        let record = record?;
        if matches!(record.value, LogValue::Update { .. }) {
            records.push(record);
        }
    }
    Ok(Log { local, records })
}

/// Add a table that deletes the lines with the update indices `tombstones` from the log of `refname`, and, if there is a
/// `marker`, adds an entry that stands for the log's existence.
fn write(addition: &mut Addition<'static>, refname: &BStr, tombstones: &[u64], marker: Option<ObjectId>) -> Result<()> {
    addition.add_table(|writer| {
        let update_index = writer.limits().0;
        writer.set_limits(update_index, update_index)?;
        if let Some(null) = marker {
            writer.add_log(LogRecord {
                refname: refname.to_owned(),
                update_index,
                value: LogValue::Update {
                    old_hash: null,
                    new_hash: null,
                    name: BString::default(),
                    email: BString::default(),
                    time: 0,
                    tz_offset: 0,
                    message: BString::default(),
                },
            })?;
        }
        for update_index in tombstones {
            writer.add_log(LogRecord {
                refname: refname.to_owned(),
                update_index: *update_index,
                value: LogValue::Deletion,
            })?;
        }
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use gix_hash::ObjectId;

    use super::*;

    fn line(message: &str) -> Line {
        Line {
            previous_oid: ObjectId::null(gix_hash::Kind::Sha1),
            new_oid: ObjectId::null(gix_hash::Kind::Sha1),
            signature: gix_actor::Signature::default(),
            message: message.into(),
        }
    }

    #[test]
    fn the_oldest_line_is_asked_about_first_and_the_removed_ones_come_out_like_the_log() {
        let lines = [
            (5, line("e")),
            (4, line("d")),
            (3, line("c")),
            (2, line("b")),
            (1, line("a")),
        ];
        let mut asked = Vec::new();
        let expiry = Expiry::new(&lines, None, &mut |line, _| {
            asked.push(line.message.to_string());
            line.message != "b" && line.message != "d"
        });
        assert_eq!(
            asked,
            ["a", "b", "c", "d", "e"],
            "Git asks from the oldest to the most recent line"
        );
        assert_eq!(
            expiry,
            Expiry {
                removed: vec![4, 2],
                kept: 3
            },
            "tombstones are in the order of the log"
        );
        assert!(!expiry.needs_marker());
    }

    #[test]
    fn the_current_value_is_passed_to_every_decision() {
        let tip = ObjectId::from_hex(b"1111111111111111111111111111111111111111").expect("valid");
        let mut seen = Vec::new();
        Expiry::new(&[(2, line("b")), (1, line("a"))], Some(&tip), &mut |_, current| {
            seen.push(current.map(ToOwned::to_owned));
            true
        });
        assert_eq!(seen, [Some(tip), Some(tip)]);
    }

    #[test]
    fn a_marker_takes_the_place_of_what_was_removed_only_if_nothing_is_left() {
        let lines = [(2, line("b")), (1, line("a"))];
        let all = Expiry::new(&lines, None, &mut |_, _| false);
        assert!(all.needs_marker(), "without it the reflog would stop to exist");
        let some = Expiry::new(&lines, None, &mut |line, _| line.message == "b");
        assert!(!some.needs_marker());
        let none = Expiry::new(&lines, None, &mut |_, _| true);
        assert!(!none.needs_marker(), "nothing changes if nothing is removed");
        assert_eq!(none.outcome(), expire::Outcome { removed: 0, kept: 2 });
        let none = Expiry::new(&[], None, &mut |_, _| false);
        assert!(!none.needs_marker(), "a reflog with only a marker keeps it");
    }
}
