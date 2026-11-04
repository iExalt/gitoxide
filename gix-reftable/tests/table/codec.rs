//! The codecs, exercised with tables written in memory. The cases follow those of Git's `u-reftable-*` unit tests
//! (`readwrite`, `merged`, `record` and `block`), as far as they apply to the public interface.

use bstr::BString;
use gix_hash::{Kind, ObjectId};
use gix_reftable::{
    BlockSource, MergedTable, Table, Writer,
    record::{LogRecord, LogValue, Record, RefRecord, RefValue},
    writer::Options,
};

use crate::{Result, util::HASHES};

fn id(kind: Kind, seed: u8) -> ObjectId {
    ObjectId::try_from(vec![seed; kind.len_in_bytes()].as_slice()).expect("both hashes are enabled")
}

fn val1(kind: Kind, name: impl Into<BString>, update_index: u64, seed: u8) -> RefRecord {
    RefRecord {
        refname: name.into(),
        update_index,
        value: RefValue::Val1(id(kind, seed)),
    }
}

fn deletion(name: impl Into<BString>, update_index: u64) -> RefRecord {
    RefRecord {
        refname: name.into(),
        update_index,
        value: RefValue::Deletion,
    }
}

fn log(kind: Kind, name: &str, update_index: u64, message: &str) -> LogRecord {
    LogRecord {
        refname: name.into(),
        update_index,
        value: LogValue::Update {
            old_hash: id(kind, 1),
            new_hash: id(kind, 2),
            name: "committer".into(),
            email: "committer@example.com".into(),
            time: 1_577_123_507,
            tz_offset: 100,
            message: message.into(),
        },
    }
}

fn table(
    kind: Kind,
    options: Options,
    limits: (u64, u64),
    refs: Vec<RefRecord>,
    logs: Vec<LogRecord>,
) -> Result<Table> {
    let bytes = crate::util::write_table(kind, options, limits, refs, logs)?;
    Ok(Table::from_block_source(
        "in-memory".into(),
        BlockSource::from_bytes(bytes),
    )?)
}

fn refs_of(table: &Table) -> Result<Vec<RefRecord>> {
    let mut out = Vec::new();
    let mut iter = table.ref_iter()?;
    while let Some(Record::Ref(record)) = iter.next_record()? {
        out.push(record);
    }
    Ok(out)
}

fn logs_of(table: &Table) -> Result<Vec<LogRecord>> {
    let mut out = Vec::new();
    let mut iter = table.log_iter()?;
    while let Some(Record::Log(record)) = iter.next_record()? {
        out.push(record);
    }
    Ok(out)
}

fn names(refs: &[RefRecord]) -> Vec<String> {
    refs.iter().map(|r| r.refname.to_string()).collect()
}

#[test]
fn empty_table_consists_of_header_and_footer() -> Result {
    for (hash, version, size) in [("sha1", 1, 24 + 68), ("sha256", 2, 28 + 72)] {
        let kind: Kind = hash.parse()?;
        let mut writer = Writer::new(Vec::new(), kind, Options::default())?;
        assert!(writer.is_empty());
        writer.set_limits(1, 1)?;
        let bytes = writer.finish()?;
        assert_eq!(bytes.len(), size, "{hash}: nothing but the framing");

        let table = Table::from_block_source("empty".into(), BlockSource::from_bytes(bytes))?;
        assert_eq!((table.version(), table.hash_kind()), (version, kind));
        assert!(!table.has_refs() && !table.has_logs());
        assert!(table.ref_iter()?.next_record()?.is_none());
        assert!(table.log_iter()?.next_record()?.is_none());
    }
    Ok(())
}

#[test]
fn references_are_read_back_in_order_with_their_values_for_both_hashes() -> Result {
    for hash in HASHES {
        let kind: Kind = hash.parse()?;
        let mut refs: Vec<_> = (0..50u8)
            .map(|idx| val1(kind, format!("refs/heads/{idx:02}"), 2, idx))
            .collect();
        refs.push(RefRecord {
            refname: "refs/tags/v1".into(),
            update_index: 3,
            value: RefValue::Val2 {
                value: id(kind, 7),
                target_value: id(kind, 8),
            },
        });
        refs.push(RefRecord {
            refname: "refs/tags/v2".into(),
            update_index: 10,
            value: RefValue::Symref("refs/heads/00".into()),
        });
        let t = table(
            kind,
            Options {
                block_size: 128,
                ..Default::default()
            },
            (1, 10),
            refs.clone(),
            vec![],
        )?;
        assert_eq!(
            refs_of(&t)?,
            refs,
            "{hash}: values and absolute update indices are kept"
        );
        assert_eq!((t.min_update_index(), t.max_update_index()), (1, 10));
        assert!(t.has_refs() && !t.has_logs());
    }
    Ok(())
}

#[test]
fn seeking_works_in_tables_with_no_index_a_single_index_and_several_index_levels() -> Result {
    let kind = Kind::Sha1;
    for (what, count, options) in [
        (
            "one unpadded block, no index",
            5,
            Options {
                unpadded: true,
                ..Default::default()
            },
        ),
        (
            "three padded blocks, still no index",
            20,
            Options {
                block_size: 128,
                ..Default::default()
            },
        ),
        (
            "many blocks, an index",
            300,
            Options {
                block_size: 256,
                ..Default::default()
            },
        ),
        (
            "tiny blocks, an index of indices",
            3000,
            Options {
                block_size: 128,
                ..Default::default()
            },
        ),
        (
            "unpadded many blocks",
            300,
            Options {
                block_size: 128,
                unpadded: true,
                ..Default::default()
            },
        ),
    ] {
        let refs: Vec<_> = (0..count)
            .map(|idx| val1(kind, format!("refs/heads/{idx:05}"), 1, 1))
            .collect();
        let t = table(kind, options, (1, 1), refs.clone(), vec![])?;
        let mut iter = t.ref_iter()?;
        for (idx, record) in refs.iter().enumerate() {
            iter.seek_key(&record.refname)?;
            assert_eq!(
                iter.next_record()?,
                Some(Record::Ref(record.clone())),
                "{what}: exact key {idx}"
            );
            let mut after = record.refname.to_vec();
            after.push(b'-'); // sorts between this and the next name, as '-' < '0'
            iter.seek_key(&after)?;
            assert_eq!(
                iter.next_record()?.map(|r| r.key()),
                refs.get(idx + 1).map(|r| r.refname.to_vec()),
                "{what}: key after {idx}"
            );
        }
        iter.seek_key(b"")?;
        assert_eq!(
            iter.next_record()?.map(|r| r.key()),
            Some(refs[0].refname.to_vec()),
            "{what}: before all"
        );
        iter.seek_key(b"refs/z")?;
        assert_eq!(iter.next_record()?, None, "{what}: after all");
        // An iterator that has been used keeps working when it is repositioned and then runs to the end.
        iter.seek_key(&refs[count / 2].refname)?;
        let mut rest = 0;
        while iter.next_record()?.is_some() {
            rest += 1;
        }
        assert_eq!(
            rest,
            count - count / 2,
            "{what}: iterating continues across block boundaries"
        );
    }
    Ok(())
}

#[test]
fn log_records_are_compressed_read_back_and_sorted_newest_first() -> Result {
    for hash in HASHES {
        let kind: Kind = hash.parse()?;
        let message = format!("{}\n", "a reasonably long and repetitive message ".repeat(8));
        let logs: Vec<_> = ["refs/heads/a", "refs/heads/b"]
            .into_iter()
            .flat_map(|name| (1..=30).rev().map(move |idx| (name, idx)))
            .map(|(name, idx)| log(kind, name, idx, &message))
            .collect();
        let t = table(
            kind,
            Options {
                block_size: 512,
                ..Default::default()
            },
            (1, 30),
            vec![],
            logs.clone(),
        )?;
        assert!(t.has_logs() && !t.has_refs());
        assert_eq!(
            logs_of(&t)?,
            logs,
            "{hash}: records come back newest first within a reference"
        );
        assert!(
            (t.file_size() as usize) < logs.len() * message.len() / 2,
            "{hash}: {} bytes for {} bytes of repetitive messages shows that blocks are deflated",
            t.file_size(),
            logs.len() * message.len()
        );

        let mut iter = t.log_iter()?;
        iter.seek_key(&Record::Log(log(kind, "refs/heads/b", 10, "")).key())?;
        let Some(Record::Log(found)) = iter.next_record()? else {
            panic!("a record exists")
        };
        assert_eq!(
            (found.refname.to_string(), found.update_index),
            ("refs/heads/b".into(), 10)
        );
    }
    Ok(())
}

#[test]
fn log_messages_are_a_single_line_unless_stored_exactly() -> Result {
    let kind = Kind::Sha1;
    let stored = |message: &str, exact| -> Result<Option<LogRecord>> {
        let options = Options {
            exact_log_message: exact,
            ..Default::default()
        };
        Ok(table(
            kind,
            options,
            (1, 1),
            vec![],
            vec![log(kind, "refs/heads/a", 1, message)],
        )
        .ok()
        .map(|t| logs_of(&t).expect("readable").remove(0)))
    };
    let message_of = |record: Option<LogRecord>| match record.map(|r| r.value) {
        Some(LogValue::Update { message, .. }) => Some(message.to_string()),
        _ => None,
    };
    assert_eq!(
        message_of(stored("one line", false)?),
        Some("one line\n".into()),
        "a newline is added"
    );
    assert_eq!(
        message_of(stored("one line\n\n\n", false)?),
        Some("one line\n".into()),
        "extra ones are removed"
    );
    assert_eq!(
        message_of(stored("", false)?),
        Some("\n".into()),
        "even an empty message becomes a line"
    );
    assert_eq!(
        message_of(stored("two\nlines", false)?),
        None,
        "more than one line is rejected"
    );
    assert_eq!(
        message_of(stored("two\nlines", true)?),
        Some("two\nlines".into()),
        "unless stored exactly"
    );
    Ok(())
}

#[test]
fn records_which_cannot_fit_a_block_are_rejected() {
    let kind = Kind::Sha1;
    let options = Options {
        block_size: 128,
        ..Default::default()
    };
    let big_ref = vec![val1(kind, "x".repeat(200), 1, 1)];
    assert!(
        crate::util::write_table(kind, options, (1, 1), big_ref, vec![]).is_err(),
        "a ref record larger than the block size can't be stored"
    );
    let big_log = vec![log(kind, "refs/heads/a", 1, &"m".repeat(2000))];
    assert!(
        crate::util::write_table(kind, options, (1, 1), vec![], big_log).is_err(),
        "a log record is limited by the block size before it is compressed"
    );
}

#[test]
fn writers_enforce_contracts_of_limits_order_and_names() -> Result {
    let kind = Kind::Sha1;
    let mut writer = Writer::new(Vec::new(), kind, Options::default())?;
    assert!(writer.set_limits(5, 4).is_err(), "min must not exceed max");
    writer.set_limits(1, 5)?;
    writer.add_ref(val1(kind, "b", 2, 1))?;
    assert!(
        writer.set_limits(1, 5).is_err(),
        "limits are fixed once records are added"
    );
    assert!(writer.add_ref(val1(kind, "a", 2, 1)).is_err(), "names must ascend");
    assert!(writer.add_ref(val1(kind, "b", 2, 1)).is_err(), "names must not repeat");
    assert!(writer.add_ref(val1(kind, "c", 6, 1)).is_err(), "update index above max");
    writer.add_log(log(kind, "z", 3, "x"))?;
    assert!(
        writer.add_ref(val1(kind, "zzz", 2, 1)).is_err(),
        "no references after logs"
    );
    assert!(
        writer.add_log(log(kind, "z", 3, "x")).is_err(),
        "log keys must not repeat"
    );
    assert!(
        writer.add_log(log(kind, "z", 4, "x")).is_err(),
        "newest first within a name"
    );
    let mut writer = Writer::new(Vec::new(), kind, Options::default())?;
    writer.set_limits(1, 1)?;
    assert!(
        writer.add_ref(val1(kind, "", 1, 1)).is_err(),
        "empty names are not keys"
    );
    Ok(())
}

#[test]
fn tables_with_references_and_logs_or_only_one_kind_declare_what_they_have() -> Result {
    let kind = Kind::Sha1;
    let refs = vec![val1(kind, "refs/heads/a", 1, 1)];
    let logs = vec![log(kind, "refs/heads/a", 1, "m")];
    for (refs, logs) in [
        (refs.clone(), logs.clone()),
        (refs.clone(), vec![]),
        (vec![], logs.clone()),
    ] {
        let t = table(kind, Options::default(), (1, 1), refs.clone(), logs.clone())?;
        assert_eq!((t.has_refs(), t.has_logs()), (!refs.is_empty(), !logs.is_empty()));
        assert_eq!(refs_of(&t)?, refs);
        assert_eq!(logs_of(&t)?.len(), logs.len());
    }
    Ok(())
}

fn merged_of(kind: Kind, tables: Vec<(Vec<RefRecord>, Vec<LogRecord>, u64)>) -> Result<MergedTable> {
    let tables = tables
        .into_iter()
        .map(|(refs, logs, idx)| table(kind, Options::default(), (1, idx), refs, logs))
        .collect::<Result<Vec<_>>>()?;
    Ok(MergedTable::new(tables)?)
}

#[test]
fn newer_tables_shadow_older_ones_and_tombstones_can_be_hidden() -> Result {
    let kind = Kind::Sha1;
    let m = merged_of(
        kind,
        vec![
            (
                vec![val1(kind, "a", 1, 1), val1(kind, "b", 1, 1), val1(kind, "d", 1, 1)],
                vec![],
                1,
            ),
            (vec![val1(kind, "b", 2, 2), val1(kind, "c", 2, 2)], vec![], 2),
            (vec![deletion("a", 3), val1(kind, "d", 3, 3)], vec![], 3),
        ],
    )?;
    let collect = |m: &MergedTable| -> Result<Vec<(String, u64, bool)>> {
        let mut out = Vec::new();
        let mut iter = m.ref_iter()?;
        while let Some(Record::Ref(r)) = iter.next_record()? {
            out.push((
                r.refname.to_string(),
                r.update_index,
                matches!(r.value, RefValue::Deletion),
            ));
        }
        Ok(out)
    };
    assert_eq!(
        collect(&m)?,
        [
            ("a".into(), 3, true),
            ("b".into(), 2, false),
            ("c".into(), 2, false),
            ("d".into(), 3, false)
        ],
        "each name once, from the newest table that has it, tombstones included"
    );
    assert_eq!(
        names(&refs_of(&m.tables()[2])?),
        ["a", "d"],
        "the newest table stays what it was"
    );
    let hidden = m.with_deletions_suppressed(true);
    assert_eq!(
        collect(&hidden)?.into_iter().map(|(name, ..)| name).collect::<Vec<_>>(),
        ["b", "c", "d"],
        "a tombstone hides the older value of its name as well"
    );
    assert_eq!((hidden.min_update_index(), hidden.max_update_index()), (1, 3));
    Ok(())
}

#[test]
fn merged_logs_are_ordered_by_name_and_newest_first_and_tombstones_cancel_entries() -> Result {
    let kind = Kind::Sha1;
    let tombstone = LogRecord {
        refname: "a".into(),
        update_index: 2,
        value: LogValue::Deletion,
    };
    let m = merged_of(
        kind,
        vec![
            (
                vec![],
                vec![
                    log(kind, "a", 2, "a2"),
                    log(kind, "a", 1, "a1"),
                    log(kind, "b", 1, "b1"),
                ],
                2,
            ),
            (vec![], vec![tombstone, log(kind, "b", 3, "b3")], 3),
        ],
    )?
    .with_deletions_suppressed(true);
    let mut seen = Vec::new();
    let mut iter = m.log_iter()?;
    while let Some(Record::Log(l)) = iter.next_record()? {
        seen.push((l.refname.to_string(), l.update_index));
    }
    assert_eq!(
        seen,
        [("a".into(), 1), ("b".into(), 3), ("b".into(), 1)],
        "entry a@2 is cancelled by the newer tombstone with the same key"
    );
    Ok(())
}

#[test]
fn merged_iterators_can_be_repositioned_many_times() -> Result {
    let kind = Kind::Sha1;
    let m = merged_of(
        kind,
        vec![
            (
                vec![val1(kind, "a", 1, 1), val1(kind, "c", 1, 1), val1(kind, "e", 1, 1)],
                vec![],
                1,
            ),
            (
                vec![val1(kind, "b", 2, 2), val1(kind, "c", 2, 2), val1(kind, "f", 2, 2)],
                vec![],
                2,
            ),
        ],
    )?;
    let mut iter = m.ref_iter()?;
    for (seek, expected) in [("b", "b"), ("a", "a"), ("d", "e"), ("c", "c"), ("", "a"), ("e", "e")] {
        iter.seek_key(seek.as_bytes())?;
        assert_eq!(
            iter.next_record()?.map(|r| r.key()),
            Some(expected.as_bytes().to_vec()),
            "seeking {seek:?}"
        );
    }
    iter.seek_key(b"g")?;
    assert!(iter.next_record()?.is_none());
    Ok(())
}

#[test]
fn tables_of_different_hashes_cannot_be_merged() -> Result {
    let sha1 = table(
        Kind::Sha1,
        Options::default(),
        (1, 1),
        vec![val1(Kind::Sha1, "a", 1, 1)],
        vec![],
    )?;
    let sha256 = table(
        Kind::Sha256,
        Options::default(),
        (1, 1),
        vec![val1(Kind::Sha256, "a", 1, 1)],
        vec![],
    )?;
    assert!(MergedTable::new(vec![sha1, sha256]).is_err());
    Ok(())
}
