//! Publish changes by adding tables to a stack.

use std::{fs::File, io::BufWriter, path::PathBuf, time::Duration};

use gix_error::Result;
use gix_lock::acquire::Fail;

use super::{
    LIST_FILE_NAME, Snapshot, Stack,
    files::{self, Failure},
};
use crate::Writer;

/// The error of [`Addition::commit()`] if replacing `tables.list` failed, but it is not certain whether that happened
/// before the new list became visible.
///
/// The tables stay where they are, as another reader may use them already. Reload the stack to learn which of the two
/// stacks is in place, and look for the changes in it, before deciding whether to try again. Retrieve it from an error
/// with [`gix_error::Error::downcast_any_ref()`].
#[derive(Debug)]
pub struct UncertainAfterPublish {
    /// The names of the tables that were added.
    pub tables: Vec<String>,
    /// Whether the new list was found in place when looking after the failure, as opposed to not being able to look.
    pub list_confirmed: bool,
}

impl std::fmt::Display for UncertainAfterPublish {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "replacing tables.list failed, {}: the added tables {:?} are in use or not, which a reload will show",
            if self.list_confirmed {
                "though the new list is in place"
            } else {
                "and the list could not be checked afterwards"
            },
            self.tables
        )
    }
}

impl std::error::Error for UncertainAfterPublish {}

/// A stack, locked for adding tables to it. The lock is released when this is dropped or committed.
///
/// Tables that were added are removed again when the addition is dropped without having been committed.
#[derive(Debug)]
pub struct Addition<'a> {
    stack: &'a mut Stack,
    lock: Option<gix_lock::File>,
    next_update_index: u64,
    created: Vec<Created>,
    /// Whether the new list is, or may be, in place, so that the tables must be left alone.
    published: bool,
}

#[derive(Debug)]
struct Created {
    name: String,
    path: PathBuf,
}

impl Stack {
    /// Lock the stack to change it, waiting as [configured](super::Options::lock_timeout), and make the view of it
    /// [current](Addition::snapshot()).
    ///
    /// Fails with an error that [is retryable](gix_error::Error::is_retryable()) if the lock stays taken.
    /// Like Git, this replaces a stale view by reloading under the lock instead of failing, so anything that was
    /// concluded from an older view, like the expected old value of a reference, needs to be checked against the one
    /// of the addition.
    pub fn addition(&mut self) -> Result<Addition<'_>> {
        let lock = self.lock_list()?;
        // Nobody else can replace the list now, so this is what the new tables are added to.
        self.reload()?;
        let next_update_index = self.snapshot.next_update_index();
        Ok(Addition {
            stack: self,
            lock: Some(lock),
            next_update_index,
            created: Vec::new(),
            published: false,
        })
    }

    /// Take the lock of `tables.list`, waiting as long as the options say.
    pub(super) fn lock_list(&self) -> Result<gix_lock::File> {
        let fail = self
            .options
            .lock_timeout
            .map_or(Fail::AfterDurationWithBackoff(Duration::MAX), Fail::from);
        gix_lock::File::acquire_to_update_resource(
            self.dir.join(LIST_FILE_NAME),
            fail,
            None,
            self.options.shared_repository_permissions,
        )
    }
}

impl Addition<'_> {
    /// The stack as it is while it is locked, which is what the tables are added to.
    pub fn snapshot(&self) -> &Snapshot {
        self.stack.snapshot()
    }

    /// The update index the next table has to start at.
    pub fn next_update_index(&self) -> u64 {
        self.next_update_index
    }

    /// Write a table with the records that `write` adds to the writer it receives, whose limits are preset to
    /// [`next_update_index()`](Self::next_update_index()) for both ends, which `write` may change before it adds records.
    /// The table is flushed according to the [options](super::Options::fsync), and kept under its final name, but isn't
    /// visible to anyone else until the addition is [committed](Self::commit()).
    ///
    /// Return `false` without adding anything if `write` added no records, as tables without content aren't listed.
    pub fn add_table(&mut self, write: impl FnOnce(&mut Writer<BufWriter<File>>) -> Result<()>) -> Result<bool> {
        let next = self.next_update_index;
        let Some(written) = files::write_table(
            &self.stack.dir,
            self.stack.hash_kind,
            &self.stack.options,
            (next, next),
            next,
            write,
        )?
        else {
            return Ok(false);
        };
        let max = written.max;
        let (name, path) = written.persist(&self.stack.dir)?;
        self.created.push(Created { name, path });
        self.next_update_index = max + 1;
        Ok(true)
    }

    /// Publish the tables that were added by listing them in `tables.list`, and release the lock. Return `false` if there
    /// was nothing to publish.
    ///
    /// The stack is reloaded afterwards, which is best effort: if that fails the stack keeps its view of before, which
    /// the next [`reload()`](Stack::reload()) corrects. This isn't an [`UncertainAfterPublish`], as the outcome is known.
    ///
    /// If the new list couldn't be put in place, the tables are removed. But if it may be in place nonetheless, the
    /// tables stay and the error [is an `UncertainAfterPublish`](UncertainAfterPublish).
    pub fn commit(mut self) -> Result<bool> {
        if self.created.is_empty() {
            return Ok(false);
        }
        let lock = self
            .lock
            .take()
            .expect("the lock is only taken by commit() which consumes self");
        let created: Vec<_> = self.created.iter().map(|created| created.name.clone()).collect();
        let names: Vec<_> = self
            .stack
            .snapshot()
            .tables()
            .iter()
            .map(|table| table.name().to_owned())
            .chain(created.iter().cloned())
            .collect();

        let result = files::publish_list(
            &self.stack.dir,
            &self.stack.options,
            #[cfg(test)]
            self.stack.inject.take(),
            lock,
            &names,
            &created,
            |found| created.iter().all(|name| found.contains(name)),
        );
        match result {
            Ok(()) => {}
            Err(Failure::NotPublished(err)) => return Err(err),
            Err(Failure::Uncertain(err)) => {
                self.published = true;
                return Err(err);
            }
        }
        self.published = true;
        self.stack.reload().ok();
        Ok(true)
    }
}

impl Drop for Addition<'_> {
    fn drop(&mut self) {
        if !self.published {
            for created in &self.created {
                std::fs::remove_file(&created.path).ok();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gix_hash::Kind;

    use super::UncertainAfterPublish;
    use crate::{
        Stack,
        record::{RefRecord, RefValue},
        stack::files::Inject,
    };

    fn publish_one(stack: &mut Stack) -> gix_error::Result<bool> {
        let mut addition = stack.addition()?;
        addition.add_table(|writer| {
            writer.add_ref(RefRecord {
                refname: "refs/heads/main".into(),
                update_index: writer.limits().0,
                value: RefValue::Val1(Kind::Sha1.null()),
            })
        })?;
        addition.commit()
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .expect("readable")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn failing_before_the_list_is_replaced_removes_the_tables_and_the_lock() {
        let dir = gix_testtools::tempfile::tempdir().expect("tempdir");
        let mut stack = Stack::open(dir.path(), Kind::Sha1).expect("empty stack");
        stack.inject = Some(Inject::BeforeRename);

        let err = publish_one(&mut stack).expect_err("injected");
        assert!(
            err.downcast_any_ref::<UncertainAfterPublish>().is_none(),
            "nothing was published: {err:?}"
        );
        assert!(
            names(dir.path()).is_empty(),
            "no table and no lock remain: {:?}",
            names(dir.path())
        );
        assert!(stack.snapshot().tables().is_empty());

        assert!(publish_one(&mut stack).expect("the next attempt works"));
    }

    #[test]
    fn failing_after_the_list_is_replaced_keeps_the_tables_and_says_that_the_outcome_is_uncertain() {
        let dir = gix_testtools::tempfile::tempdir().expect("tempdir");
        let mut stack = Stack::open(dir.path(), Kind::Sha1).expect("empty stack");
        stack.inject = Some(Inject::AfterRename);

        let err = publish_one(&mut stack).expect_err("injected");
        let uncertain = err
            .downcast_any_ref::<UncertainAfterPublish>()
            .unwrap_or_else(|| panic!("the error is the typed one: {err:?}"));
        assert!(uncertain.list_confirmed, "the new list was found in place");
        assert_eq!(uncertain.tables.len(), 1);
        assert!(
            dir.path().join(&uncertain.tables[0]).is_file(),
            "the table of a list that is in place must stay"
        );
        assert!(
            !names(dir.path())
                .iter()
                .any(|name| std::path::Path::new(name).extension().is_some_and(|ext| ext == "lock")),
            "the lock is released"
        );

        assert!(stack.reload().expect("reload"), "the stack shows what was published");
        assert_eq!(stack.snapshot().tables().len(), 1);
    }

    #[test]
    fn a_list_that_cannot_be_read_after_a_failure_leaves_the_outcome_uncertain_and_keeps_the_tables() {
        let dir = gix_testtools::tempfile::tempdir().expect("tempdir");
        let mut stack = Stack::open(dir.path(), Kind::Sha1).expect("empty stack");
        stack.inject = Some(Inject::AfterRenameWithUnreadableList);

        let err = publish_one(&mut stack).expect_err("injected");
        let uncertain = err
            .downcast_any_ref::<UncertainAfterPublish>()
            .unwrap_or_else(|| panic!("not being able to look is uncertain, not a clean failure: {err:?}"));
        assert!(!uncertain.list_confirmed, "the list couldn't be looked at");
        assert!(
            dir.path().join(&uncertain.tables[0]).is_file(),
            "tables that might be listed are never deleted"
        );
    }
}
