//! Merge tables of a stack so that it doesn't grow without bound, following Git's geometric policy.

use std::ops::Range;

use gix_error::{Class, ErrorExt, Result};
use gix_lock::{Marker, acquire::Fail};

use super::{
    LIST_FILE_NAME, Snapshot, Stack,
    files::{self, Failure},
    read_if_exists,
};
use crate::{MergedTable, Table, basics::misuse, record::Record};

/// The factor by which each table of a stack is supposed to be larger than the one above it, which is
/// `reftable.geometricFactor` and is used when the factor of the options is 0.
pub const DEFAULT_GEOMETRIC_FACTOR: u8 = 2;

/// Return the range of tables in a stack with the given table `sizes`, from the oldest to the newest, that should be merged to
/// make the sizes a geometric sequence in which each table is at least `factor` times as large as the next one, or `None`
/// if they already are one. A `factor` of 0 means [`DEFAULT_GEOMETRIC_FACTOR`].
///
/// The end of the range is that of the newest table that isn't smaller than its predecessor by the factor, and its start
/// that of the oldest table which would otherwise be smaller than what the tables merged with it add up to. This is the
/// algorithm of Git's `suggest_compaction_segment()`, for which sizes are those of [`Snapshot::compaction_sizes()`].
pub fn suggest_segment(sizes: &[u64], factor: u8) -> Option<Range<usize>> {
    let factor = u64::from(if factor == 0 { DEFAULT_GEOMETRIC_FACTOR } else { factor });
    if sizes.len() <= 1 {
        return None;
    }
    // Tables past the end are valid members of the sequence already, and what they add up to can't exceed the table at the end.
    let mut i = sizes.len() - 1;
    let mut end = 0;
    let mut bytes = 0u64;
    while i > 0 {
        if sizes[i - 1] < sizes[i].saturating_mul(factor) {
            end = i + 1;
            bytes = sizes[i];
            break;
        }
        i -= 1;
    }
    // Keep going after the first start was found, as tables before it may violate the sequence as well.
    let mut start = 0;
    while i > 0 {
        let current = bytes;
        bytes = bytes.saturating_add(sizes[i - 1]);
        if sizes[i - 1] < current.saturating_mul(factor) {
            start = i - 1;
        }
        i -= 1;
    }
    (end > start).then_some(start..end)
}

impl Snapshot {
    /// The sizes of the tables, oldest first, that the geometric policy compares: those of the file without its footer, and
    /// without the header, which is how Git sees them.
    pub fn compaction_sizes(&self) -> Vec<u64> {
        self.tables()
            .iter()
            .map(|table| {
                let header_len = if table.version() == 1 { 24 } else { 28 };
                table.data_size().saturating_sub(header_len - 1)
            })
            .collect()
    }
}

/// What a [compaction](Stack::compact_all()) did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Compaction {
    /// The names of the tables that were merged, oldest first, and which were removed afterwards if possible.
    pub replaced: Vec<String>,
    /// The name of the table that replaced them, or `None` if nothing was left to keep because deleted references and
    /// reflog entries cancelled out the others.
    pub new_table: Option<String>,
}

fn outdated() -> gix_error::Error {
    gix_error::message(
        "the stack was changed by somebody else since it was read, which the compaction needs to start from",
    )
    .with_class(Class::Conflict)
    .raise()
}

/// Failures to provoke while testing, at the time when nothing is locked.
#[cfg(test)]
pub(crate) type Hook = fn(&std::path::Path, gix_hash::Kind);

impl Stack {
    /// Return whether the sizes of the tables violate the geometric sequence that
    /// [`auto_compact()`](Self::auto_compact()) maintains.
    pub fn compaction_required(&self) -> bool {
        self.snapshot.tables().len() > 1
            && suggest_segment(&self.snapshot.compaction_sizes(), self.options.geometric_factor).is_some()
    }

    /// Merge all tables into one, which drops deleted references and reflog entries as nothing below them needs to be hidden.
    ///
    /// Return `None` if there are fewer than two tables. Fail with an error [classified as conflict](gix_error::Error::is_conflict())
    /// if the stack was changed since it was read, in which case [`reload()`](Self::reload()) and try again, and with a
    /// [retryable](gix_error::Error::is_retryable()) one if the lock of `tables.list` or of a table that is being compacted
    /// is held by somebody else.
    pub fn compact_all(&mut self) -> Result<Option<Compaction>> {
        let len = self.snapshot.tables().len();
        self.compact_range(0..len, false)
    }

    /// Merge the tables [that the geometric policy suggests](suggest_segment()), if any, and as many of them as aren't in the middle
    /// of being compacted by somebody else, as long as there are two of them. Call this after tables were added.
    ///
    /// The errors are those of [`compact_all()`](Self::compact_all()), none of which means the stack is damaged. In particular, a
    /// view that is stale always fails as a conflict, as this method doesn't reload by itself: reload and try again, or skip
    /// compacting for now, as the next call after the next change will catch up.
    pub fn auto_compact(&mut self) -> Result<Option<Compaction>> {
        match suggest_segment(&self.snapshot.compaction_sizes(), self.options.geometric_factor) {
            Some(range) => self.compact_range(range, true),
            None => Ok(None),
        }
    }

    /// Remove the tables in the directory of the stack that the list doesn't name, and which nobody can need any more:
    /// the ones that stopped being listed, for example as their compaction was interrupted, not the ones that a writer
    /// may be about to list. Those are told apart by their update index, which is beyond that of the stack for the latter.
    ///
    /// Files that aren't named like tables or can't be read as one are left alone, and so are temporary ones.
    /// The list is locked meanwhile, so that nobody lists a table while it is deleted.
    pub fn clean(&mut self) -> Result<()> {
        let dir = self.dir.clone();
        let addition = self.addition()?;
        let max = addition.snapshot().merged().max_update_index();
        let listed: Vec<_> = addition
            .snapshot()
            .tables()
            .iter()
            .map(|table| table.name().to_owned())
            .collect();
        let entries = std::fs::read_dir(&dir)
            .map_err(|err| err.and_raise(gix_error::message!("Could not read {}", dir.display())))?;
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else { continue };
            if !name.ends_with(super::list::TABLE_SUFFIX) || listed.iter().any(|listed| listed == name) {
                continue;
            }
            let path = entry.path();
            if Table::open(&path).is_ok_and(|table| table.max_update_index() <= max) {
                std::fs::remove_file(&path).ok();
            }
        }
        Ok(())
    }

    fn compact_range(&mut self, range: Range<usize>, best_effort: bool) -> Result<Option<Compaction>> {
        let tables = self.snapshot.tables();
        if range.end > tables.len() {
            return Err(misuse("the tables to compact must exist"));
        }
        if range.len() < 2 {
            return Ok(None);
        }
        let mut first = range.start;
        let end = range.end;

        // Holding the list lock, learn that the view is current and take the locks of the tables.
        let list_lock = self.lock_list()?;
        if read_if_exists(&self.dir.join(LIST_FILE_NAME))?.unwrap_or_default() != self.snapshot.list() {
            return Err(outdated());
        }
        // Newest first, so that somebody who adds a table and compacts it doesn't fail on a compaction that started earlier
        // and is still busy with tables that are older.
        let mut table_locks = Vec::new();
        for idx in (first..end).rev() {
            let path = self.dir.join(self.snapshot.tables()[idx].name());
            match Marker::acquire_to_hold_resource(
                path,
                Fail::Immediately,
                None,
                self.options.shared_repository_permissions,
            ) {
                Ok(lock) => table_locks.push(lock),
                Err(err) if best_effort && err.is_retryable() && table_locks.len() >= 2 => {
                    first = idx + 1;
                    break;
                }
                Err(err) => return Err(err),
            }
        }
        // Letting go of the list lets others add to the stack while the tables are merged.
        drop(list_lock);

        let compacted: Vec<Table> = self.snapshot.tables()[first..end].to_vec();
        let replaced: Vec<String> = compacted.iter().map(|table| table.name().to_owned()).collect();
        let limits = (
            compacted[0].min_update_index(),
            compacted[compacted.len() - 1].max_update_index(),
        );
        // Whether something is below the tables, which tells if deletions still have something to hide.
        let drop_deletions = first == 0;
        let merged = MergedTable::new(compacted)?;
        let mut options = self.options;
        options.write.exact_log_message = true;
        let written = files::write_table(&self.dir, self.hash_kind, &options, limits, 0, |writer| {
            for mut iter in [merged.ref_iter()?, merged.log_iter()?] {
                while let Some(record) = iter.next_record()? {
                    if drop_deletions && record.is_deletion() {
                        continue;
                    }
                    match record {
                        Record::Ref(record) => writer.add_ref(record)?,
                        Record::Log(record) => writer.add_log(record)?,
                        _ => {}
                    }
                }
            }
            Ok(())
        })?;

        #[cfg(test)]
        if let Some(hook) = self.hook.take() {
            hook(&self.dir, self.hash_kind);
        }

        // Put the result in the place of what it was made from, which might have to be found first if others added tables.
        let list_lock = self.lock_list()?;
        let current = read_if_exists(&self.dir.join(LIST_FILE_NAME))?.unwrap_or_default();
        let mut names: Vec<String> = super::list::parse(&current)?;
        let at = if current == self.snapshot.list() {
            first
        } else {
            let at = names
                .iter()
                .position(|name| *name == replaced[0])
                .ok_or_else(outdated)?;
            if names.get(at..at + replaced.len()) != Some(&replaced[..]) {
                return Err(outdated());
            }
            at
        };
        let new_table = match written {
            Some(written) => Some(written.persist(&self.dir)?),
            None => None,
        };
        let new_name = new_table.as_ref().map(|(name, _)| name.clone());
        names.splice(at..at + replaced.len(), new_name.iter().cloned());

        let result = files::publish_list(
            &self.dir,
            &self.options,
            #[cfg(test)]
            self.inject.take(),
            list_lock,
            &names,
            new_name.as_slice(),
            |found| match &new_name {
                Some(name) => found.contains(name),
                None => replaced.iter().all(|name| !found.contains(name)),
            },
        );
        match result {
            Ok(()) => {}
            Err(Failure::NotPublished(err)) => {
                if let Some((_, path)) = &new_table {
                    std::fs::remove_file(path).ok();
                }
                return Err(err);
            }
            Err(Failure::Uncertain(err)) => return Err(err),
        }

        self.reload().ok();
        // Others may still read the tables, in which case they stay until nobody uses them, as far as the system allows.
        for name in &replaced {
            std::fs::remove_file(self.dir.join(name)).ok();
        }
        drop(table_locks);
        Ok(Some(Compaction {
            replaced,
            new_table: new_name,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gix_hash::Kind;

    use super::suggest_segment;
    use crate::{
        Stack,
        record::{RefRecord, RefValue},
        stack::{UncertainAfterPublish, files::Inject},
    };

    fn id() -> gix_hash::ObjectId {
        Kind::Sha1.null()
    }

    fn add(stack: &mut Stack, name: &str, value: RefValue) {
        let mut addition = stack.addition().expect("lock");
        addition
            .add_table(|writer| {
                writer.add_ref(RefRecord {
                    refname: name.into(),
                    update_index: writer.limits().0,
                    value,
                })
            })
            .expect("table");
        addition.commit().expect("commit");
    }

    fn stack_of(names: &[&str]) -> (gix_testtools::tempfile::TempDir, Stack) {
        let dir = gix_testtools::tempfile::tempdir().expect("tempdir");
        let mut stack = Stack::open(dir.path(), Kind::Sha1).expect("empty stack");
        for name in names {
            add(&mut stack, name, RefValue::Val1(id()));
        }
        (dir, stack)
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .expect("readable")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn visible(stack: &Stack) -> Vec<String> {
        let mut iter = stack.snapshot().merged().ref_iter().expect("iter");
        let mut names = Vec::new();
        while let Some(crate::record::Record::Ref(record)) = iter.next_record().expect("record") {
            names.push(record.refname.to_string());
        }
        names
    }

    #[test]
    fn git_adding_a_table_while_the_fork_merges_loses_nothing_and_stays_readable() {
        let Some(fixture) = gix_testtools::scripted_fixture_read_only_with_args_with_git_version(
            "make_git_written_tables.sh",
            ["sha1", "1500"],
            |version| version >= (2, 45, 0),
        )
        .expect("fixture") else {
            return;
        };
        let tmp = gix_testtools::tempfile::tempdir().expect("tempdir");
        gix_testtools::copy_recursively_into_existing_dir(fixture.join("stacked"), tmp.path()).expect("copy");
        let dir = tmp.path().join(".git").join("reftable");
        let mut stack = Stack::open(&dir, Kind::Sha1).expect("open");
        let tables = stack.snapshot().tables().len();
        stack.hook = Some(|dir, _| {
            let repo = dir.parent().and_then(Path::parent).expect("a repository");
            let output = gix_testtools::git_command(repo)
                .args(["update-ref", "refs/heads/written-by-git-meanwhile", "refs/heads/main"])
                .output()
                .expect("git runs");
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        });

        let compaction = stack.compact_all().expect("merged").expect("tables");
        assert_eq!(compaction.replaced.len(), tables);
        assert!(
            stack
                .snapshot()
                .find_ref(b"refs/heads/written-by-git-meanwhile")
                .expect("lookup")
                .is_some(),
            "Git's reference survived"
        );
        assert!(stack.snapshot().find_ref(b"refs/heads/main").expect("lookup").is_some());
        let output = gix_testtools::git_command(tmp.path())
            .args(["refs", "verify", "--strict"])
            .output()
            .expect("git runs");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn a_table_that_somebody_adds_while_the_others_are_merged_stays_on_top() {
        let (dir, mut stack) = stack_of(&["refs/heads/a", "refs/heads/b", "refs/heads/c"]);
        stack.hook = Some(|dir, kind| {
            let mut other = Stack::open(dir, kind).expect("open");
            add(&mut other, "refs/heads/in-window", RefValue::Val1(id()));
        });

        let compaction = stack
            .compact_all()
            .expect("the list changed but not the tables")
            .expect("merged");
        assert_eq!(compaction.replaced.len(), 3);
        assert_eq!(stack.snapshot().tables().len(), 2, "the merged table and the new one");
        assert_eq!(
            visible(&stack),
            ["refs/heads/a", "refs/heads/b", "refs/heads/c", "refs/heads/in-window"],
            "nothing was lost"
        );
        assert_eq!(
            files(dir.path()).len(),
            3,
            "two tables and the list, no leftovers: {:?}",
            files(dir.path())
        );
    }

    #[test]
    fn tables_below_the_range_that_somebody_merged_meanwhile_move_the_range_but_not_the_result() {
        let (dir, mut stack) = stack_of(&[
            "refs/heads/a",
            "refs/heads/b",
            "refs/heads/c",
            "refs/heads/d",
            "refs/heads/e",
        ]);
        stack.hook = Some(|dir, kind| {
            let mut other = Stack::open(dir, kind).expect("open");
            other
                .compact_range(0..2, false)
                .expect("no conflict with the locked tables")
                .expect("merged");
        });

        let compaction = stack
            .compact_range(2..5, false)
            .expect("found by name")
            .expect("merged");
        assert_eq!(compaction.replaced.len(), 3);
        assert_eq!(
            stack.snapshot().tables().len(),
            2,
            "what the other compacted, and our result"
        );
        assert_eq!(visible(&stack).len(), 5);
        assert_eq!(files(dir.path()).len(), 3, "{:?}", files(dir.path()));
    }

    #[test]
    fn a_compaction_whose_tables_were_replaced_meanwhile_fails_without_losing_anything() {
        let (dir, mut stack) = stack_of(&["refs/heads/a", "refs/heads/b", "refs/heads/c"]);
        stack.hook = Some(|dir, _| {
            // Whoever changes the tables that are being merged ignores their locks, which isn't supposed to happen.
            let list = std::fs::read_to_string(dir.join("tables.list")).expect("list");
            let mut names: Vec<_> = list.lines().collect();
            names.swap(0, 1);
            std::fs::write(dir.join("tables.list"), names.join("\n") + "\n").expect("write");
        });
        let before = files(dir.path());
        let err = stack
            .compact_all()
            .expect_err("the order isn't the one that was merged");
        assert!(err.is_conflict(), "{err:?}");
        assert_eq!(
            files(dir.path()),
            before,
            "the merged table was removed again, and the locks are gone"
        );
    }

    #[test]
    fn references_and_reflogs_that_cancel_out_leave_no_table_at_all() {
        let (dir, mut stack) = stack_of(&["refs/heads/a"]);
        add(&mut stack, "refs/heads/a", RefValue::Deletion);
        let compaction = stack.compact_all().expect("merged").expect("two tables");
        assert_eq!(compaction.new_table, None);
        assert!(stack.snapshot().tables().is_empty());
        assert_eq!(files(dir.path()), ["tables.list"], "the empty list is all that is left");
    }

    #[test]
    fn a_failed_publication_removes_the_merged_table_and_keeps_the_old_ones() {
        let (dir, mut stack) = stack_of(&["refs/heads/a", "refs/heads/b"]);
        let before = files(dir.path());
        stack.inject = Some(Inject::BeforeRename);
        let err = stack.compact_all().expect_err("injected");
        assert!(err.downcast_any_ref::<UncertainAfterPublish>().is_none(), "{err:?}");
        assert_eq!(files(dir.path()), before, "nothing changed, no new table, no lock");
        assert_eq!(stack.snapshot().tables().len(), 2);
    }

    #[test]
    fn an_uncertain_publication_keeps_the_merged_table_and_the_old_ones() {
        let (dir, mut stack) = stack_of(&["refs/heads/a", "refs/heads/b"]);
        let old = files(dir.path());
        stack.inject = Some(Inject::AfterRename);
        let err = stack.compact_all().expect_err("injected");
        let uncertain = err
            .downcast_any_ref::<UncertainAfterPublish>()
            .unwrap_or_else(|| panic!("typed: {err:?}"));
        assert!(uncertain.list_confirmed);
        let new_table = &uncertain.tables[0];
        assert!(dir.path().join(new_table).is_file());
        for name in old
            .iter()
            .filter(|name| Path::new(name).extension().is_some_and(|ext| ext == "ref"))
        {
            assert!(
                dir.path().join(name).is_file(),
                "{name} may be read by somebody who has it open: {:?}",
                files(dir.path())
            );
        }
        stack.reload().expect("reload");
        assert_eq!(
            stack.snapshot().tables().len(),
            1,
            "the merged stack is what is in place"
        );
    }

    #[test]
    fn git_s_own_expectations() {
        assert_eq!(
            suggest_segment(&[512, 64, 17, 16, 9, 9, 9, 16, 2, 16], 2),
            Some(1..10),
            "u-reftable-stack.c: suggest_compaction_segment"
        );
        assert_eq!(
            suggest_segment(&[64, 32, 16, 8, 4, 2], 2),
            None,
            "u-reftable-stack.c: suggest_compaction_segment_nothing"
        );
    }

    #[test]
    fn examples_from_the_comments_of_the_algorithm() {
        assert_eq!(
            suggest_segment(&[64, 32, 16, 8, 4, 2, 1], 2),
            None,
            "a geometric sequence needs nothing"
        );
        assert_eq!(
            suggest_segment(&[64, 32, 16, 8, 4, 3, 1], 2),
            Some(0..6),
            "the end is exclusive, so the last table is left out, and everything before it is merged as sums grow past the larger tables"
        );
        assert_eq!(
            suggest_segment(&[128, 32, 16, 8, 4, 3, 1], 2),
            Some(1..6),
            "the start is the table of 32, as 128 is at least twice what the others add up to"
        );
    }

    #[test]
    fn trivial_stacks_and_the_default_factor() {
        assert_eq!(suggest_segment(&[], 2), None);
        assert_eq!(suggest_segment(&[100], 2), None);
        assert_eq!(
            suggest_segment(&[100, 60], 0),
            Some(0..2),
            "a factor of 0 is 2: 100 < 60 * 2"
        );
        assert_eq!(suggest_segment(&[120, 60], 0), None, "120 isn't smaller than 60 * 2");
        assert_eq!(
            suggest_segment(&[200, 60], 4),
            Some(0..2),
            "a larger factor merges more eagerly"
        );
        assert_eq!(
            suggest_segment(&[u64::MAX, u64::MAX], 2),
            None,
            "huge sizes don't overflow"
        );
    }

    /// Table sizes as Git 2.56 left them while a stack grew by one reference update at a time, with the tables that its
    /// automatic compaction started from and the range that it merged. The sizes are those of the files in bytes.
    #[test]
    fn the_steps_of_gits_own_compaction_are_reproduced() {
        // Each file size has the 68 bytes of the footer and 23 of the header removed, so that is 91 less.
        let sized = |files: &[u64]| -> Vec<u64> { files.iter().map(|size| size - 68 - 23).collect() };
        // Step 5 of the log: a table of 219 bytes came on top of one of 348, after which Git left both.
        assert_eq!(suggest_segment(&sized(&[348, 219]), 2), None);
        // Step 6: another one of 219 arrived, and Git merged everything into 415 bytes.
        assert_eq!(suggest_segment(&sized(&[348, 219, 219]), 2), Some(0..3));
        // Step 14: [617, 221, 221] became [617, 255]: only the last two were merged.
        assert_eq!(suggest_segment(&sized(&[617, 221, 221]), 2), Some(1..3));
        // Step 18: [717, 255, 221] became [717, 289].
        assert_eq!(suggest_segment(&sized(&[717, 255, 221]), 2), Some(1..3));
        // Step 25: [870, 357, 221] stays as it is.
        assert_eq!(suggest_segment(&sized(&[870, 357, 221]), 2), None);
        // Step 26: [870, 357, 221] is followed by 221 and merged completely: 1084 bytes.
        assert_eq!(suggest_segment(&sized(&[870, 357, 221, 221]), 2), Some(0..4));
    }
}
