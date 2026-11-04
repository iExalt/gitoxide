//! Read a stack from disk the way Git does, which includes coping with writers that replace tables while it reads.

use std::time::Duration;

use gix_error::{ErrorExt, Result};

use super::list;
use crate::Table;

/// Retries only start to be limited by time after this many attempts.
const MIN_TRIES: u32 = 3;
/// Give up reading a stack that keeps changing after this long, once [`MIN_TRIES`] were made.
const DEADLINE: Duration = Duration::from_secs(3);
/// The longest wait between two attempts.
const MAX_DELAY: Duration = Duration::from_millis(100);

/// What reloading needs from the outside world, so that the retry logic can be tested without racing real writers.
pub(crate) trait Backend {
    /// The content of `tables.list`, which is empty if there is no such file.
    fn read_list(&mut self) -> Result<Vec<u8>>;
    /// Open the table called `name`, or return `None` if its file doesn't exist.
    fn open_table(&mut self, name: &str) -> Result<Option<Table>>;
    fn sleep(&mut self, duration: Duration);
    /// The time since reloading started.
    fn elapsed(&mut self) -> Duration;
}

/// Read `tables.list` and open all the tables it lists, reusing those of `previous` that have the same name, as tables
/// are immutable.
///
/// Return the content of the list and its tables. A listed table that doesn't exist is the sign of a writer that
/// replaced the list since it was read, so the list is read again. Only if it hasn't changed, or keeps changing for
/// too long, the missing table is an error. Every other error fails immediately, as retrying doesn't make a
/// damaged table better.
pub(crate) fn reload(backend: &mut impl Backend, previous: &[Table]) -> Result<(Vec<u8>, Vec<Table>)> {
    let mut tries = 0u32;
    let mut delay = Duration::from_millis(1);
    loop {
        tries += 1;
        let content = backend.read_list()?;
        let names = list::parse(&content)?;
        let mut tables = Vec::with_capacity(names.len());
        let mut missing = None;
        for name in &names {
            if let Some(table) = previous.iter().find(|table| table.name() == name) {
                tables.push(table.clone());
            } else if let Some(table) = backend.open_table(name)? {
                tables.push(table);
            } else {
                missing = Some(name);
                break;
            }
        }
        let Some(missing) = missing else {
            return Ok((content, tables));
        };
        let not_found = |detail: &str| {
            gix_error::not_found(format!("the table {missing:?} of the stack is missing{detail}")).raise()
        };
        if backend.read_list()? == content {
            return Err(not_found(" though tables.list didn't change"));
        }
        if tries > MIN_TRIES && backend.elapsed() > DEADLINE {
            return Err(not_found(" and tables.list kept changing until reading gave up"));
        }
        backend.sleep(delay);
        delay = (delay + delay / 2 + Duration::from_millis(1)).min(MAX_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, time::Duration};

    use gix_error::Result;
    use gix_hash::Kind;

    use super::{Backend, reload};
    use crate::{
        BlockSource, Table, Writer,
        record::{RefRecord, RefValue},
        writer::Options,
    };

    fn table(name: &str, update_index: u64) -> Table {
        let mut writer = Writer::new(Vec::new(), Kind::Sha1, Options::default()).expect("valid options");
        writer
            .set_limits(update_index, update_index)
            .expect("limits are set first");
        writer
            .add_ref(RefRecord {
                refname: "refs/heads/main".into(),
                update_index,
                value: RefValue::Val1(Kind::Sha1.null()),
            })
            .expect("one record");
        let bytes = writer.finish().expect("finished");
        Table::from_block_source(name.into(), BlockSource::from_bytes(bytes)).expect("a valid table")
    }

    /// A scripted world: each read of the list returns the next entry, the last one repeatedly, and tables exist
    /// only while they are in `existing`.
    struct Script {
        lists: VecDeque<Vec<u8>>,
        existing: Vec<&'static str>,
        opened: Vec<String>,
        slept: Vec<Duration>,
        clock: Duration,
    }

    impl Script {
        fn new(lists: &[&str], existing: &[&'static str]) -> Self {
            Self {
                lists: lists.iter().map(|list| list.as_bytes().to_vec()).collect(),
                existing: existing.to_vec(),
                opened: Vec::new(),
                slept: Vec::new(),
                clock: Duration::ZERO,
            }
        }
    }

    impl Backend for Script {
        fn read_list(&mut self) -> Result<Vec<u8>> {
            Ok(if self.lists.len() > 1 {
                self.lists.pop_front().expect("checked")
            } else {
                self.lists.front().cloned().unwrap_or_default()
            })
        }

        fn open_table(&mut self, name: &str) -> Result<Option<Table>> {
            self.opened.push(name.to_owned());
            Ok(self.existing.contains(&name).then(|| table(name, 1)))
        }

        fn sleep(&mut self, duration: Duration) {
            self.slept.push(duration);
            self.clock += duration;
        }

        fn elapsed(&mut self) -> Duration {
            self.clock
        }
    }

    #[test]
    fn a_table_that_vanished_with_a_newer_list_is_retried_until_the_new_list_loads() {
        // First pass reads list one then finds `a.ref` gone, and the re-read shows list two, which loads.
        let mut script = Script::new(&["a.ref\n", "b.ref\n", "b.ref\n"], &["b.ref"]);
        let (content, tables) = reload(&mut script, &[]).expect("the second list loads");
        assert_eq!(content, b"b.ref\n");
        assert_eq!(tables.iter().map(Table::name).collect::<Vec<_>>(), ["b.ref"]);
        assert_eq!(script.slept.len(), 1, "one retry, after a pause");
    }

    #[test]
    fn a_table_that_is_missing_while_the_list_stays_the_same_fails_without_retrying() {
        let mut script = Script::new(&["a.ref\n"], &[]);
        let err = reload(&mut script, &[]).expect_err("the table is truly gone");
        assert!(err.is_not_found(), "{err:?}");
        assert!(script.slept.is_empty(), "the fork fails closed instead of waiting");
    }

    #[test]
    fn a_list_that_keeps_changing_is_given_up_on_after_the_deadline_and_a_few_tries() {
        // Every read shows a different list, and no table ever exists.
        let lists: Vec<String> = (0..10_000).map(|idx| format!("t{idx}.ref\n")).collect();
        let list_refs: Vec<&str> = lists.iter().map(String::as_str).collect();
        let mut script = Script::new(&list_refs, &[]);
        let err = reload(&mut script, &[]).expect_err("gives up");
        assert!(err.is_not_found(), "{err:?}");
        assert!(
            script.slept.len() > 3,
            "at least the minimum number of tries is made: {:?}",
            script.slept
        );
        assert!(
            script.clock > Duration::from_secs(3) && script.clock < Duration::from_secs(4),
            "stops soon after the deadline: {:?}",
            script.clock
        );
        assert!(
            script.slept.windows(2).all(|pair| pair[0] <= pair[1]),
            "pauses grow and stay capped: {:?}",
            script.slept
        );
        assert!(script.slept.iter().all(|pause| *pause <= Duration::from_millis(100)));
    }

    #[test]
    fn open_tables_are_reused_by_name_and_not_read_again() {
        let previous = [table("a.ref", 1)];
        let mut script = Script::new(&["a.ref\nb.ref\n"], &["b.ref"]);
        let (_, tables) = reload(&mut script, &previous).expect("loads");
        assert_eq!(tables.len(), 2);
        assert_eq!(script.opened, ["b.ref"], "only the new table is read");
    }
}
