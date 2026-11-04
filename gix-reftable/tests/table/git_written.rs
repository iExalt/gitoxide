//! Tables written by Git are read, and written again, by this crate.

use bstr::ByteSlice;
use gix_reftable::{MergedTable, record::Record};

use crate::{
    Result,
    util::{self, HASHES, git, git_ref_lines, git_written_tables, merged, ref_lines, reflog},
};

#[test]
fn references_of_stacked_and_compacted_tables_are_those_git_lists() -> Result {
    for object_format in HASHES {
        let Some(fixture) = git_written_tables(object_format)? else {
            return Ok(());
        };
        for repo in ["stacked", "compacted", "plain", "deep"] {
            let repo = fixture.join(repo);
            let lines = ref_lines(&merged(&repo)?)?;
            assert!(
                lines.len() > 1500,
                "{object_format} {repo:?}: the fixture has more refs than branches"
            );
            assert_eq!(
                lines,
                git_ref_lines(&repo)?,
                "{object_format} {repo:?}: all names, values, peeled values and symbolic targets are those of Git"
            );
        }
    }
    Ok(())
}

#[test]
fn non_utf8_reference_names_are_kept_verbatim() -> Result {
    let Some(fixture) = git_written_tables("sha1")? else {
        return Ok(());
    };
    let lines = ref_lines(&merged(&fixture.join("compacted"))?)?;
    assert!(
        lines.iter().any(|line| line.starts_with(b"refs/heads/caf\xe9\t")),
        "the name is not converted to UTF-8 and not rejected"
    );
    Ok(())
}

#[test]
fn tombstones_hide_references_of_older_tables_of_the_stack_only_while_the_stack_exists() -> Result {
    for object_format in HASHES {
        let Some(fixture) = git_written_tables(object_format)? else {
            return Ok(());
        };
        let stacked = fixture.join("stacked");
        let tables = util::open_tables(&stacked)?;
        assert!(tables.len() >= 3, "the fixture makes a stack of several tables");

        let unsuppressed = MergedTable::new(tables)?;
        let mut tombstones = Vec::new();
        let mut iter = unsuppressed.ref_iter()?;
        while let Some(record) = iter.next_record()? {
            if record.is_deletion() {
                let Record::Ref(record) = record else { unreachable!() };
                tombstones.push(record.refname);
            }
        }
        assert!(
            tombstones.contains(&"refs/heads/team01/sub1/feature-1".into()),
            "{object_format}: the newest tables carry the tombstone of a deleted branch: {tombstones:?}"
        );

        let visible = ref_lines(&merged(&stacked)?)?;
        assert!(
            !visible
                .iter()
                .any(|line| line.starts_with(b"refs/heads/team01/sub1/feature-1\t")),
            "{object_format}: the deleted branch is hidden by its tombstone"
        );

        let compacted = util::open_tables(&fixture.join("compacted"))?;
        assert_eq!(compacted.len(), 1, "a full compaction merges the stack");
        let remaining: Vec<_> = {
            let mut iter = compacted[0].ref_iter()?;
            let mut deletions = 0;
            while let Some(record) = iter.next_record()? {
                deletions += usize::from(record.is_deletion());
            }
            vec![deletions]
        };
        assert_eq!(
            remaining,
            [0],
            "a compaction which includes the bottom of the stack drops tombstones"
        );
    }
    Ok(())
}

#[test]
fn reflogs_of_main_and_head_match_git() -> Result {
    for object_format in HASHES {
        let Some(fixture) = git_written_tables(object_format)? else {
            return Ok(());
        };
        for repo in ["stacked", "compacted"] {
            let repo = fixture.join(repo);
            let merged = merged(&repo)?;
            for refname in ["HEAD", "refs/heads/main"] {
                let output = git(
                    &repo,
                    &[
                        "reflog",
                        "show",
                        "--date=raw",
                        "--format=%H%x09%gn%x09%ge%x09%gd%x09%gs",
                        refname,
                    ],
                )?;
                let expected: Vec<_> = output.lines().collect();
                let ours = reflog(&merged, refname)?;
                assert_eq!(
                    ours.len(),
                    expected.len(),
                    "{object_format} {refname}: same number of entries"
                );
                assert_eq!(ours.len(), 3, "main was created and then advanced twice");

                for ((_, entry), expected) in ours.iter().zip(expected) {
                    let fields: Vec<_> = expected.split_str("\t").collect();
                    assert_eq!(entry.id.to_string().as_bytes(), fields[0], "{refname}: new id");
                    assert_eq!(entry.name, fields[1], "{refname}: committer name is kept as bytes");
                    assert_eq!(entry.email, fields[2], "{refname}: committer email");
                    assert_eq!(
                        entry.message.trim_end_with(|c| c == '\n'),
                        fields[4],
                        "{refname}: message"
                    );
                    let selector = fields[3].to_str()?;
                    let (time, tz) = selector
                        .split_once("@{")
                        .and_then(|(_, rest)| rest.strip_suffix('}'))
                        .and_then(|raw| raw.split_once(' '))
                        .expect("raw date in the reflog selector");
                    // Like in commits, the offset is stored as `±hhmm`, which is not the same as minutes.
                    let tz_hhmm = tz.parse::<i16>()?;
                    assert_eq!(entry.time, time.parse::<u64>()?, "{refname}: time");
                    assert_eq!(entry.tz_offset, tz_hhmm, "{refname}: time zone offset");
                    assert_eq!(
                        entry.tz_offset,
                        if entry.id == ours[0].1.id { -830 } else { 0 },
                        "{refname}: as it was at the time"
                    );
                }

                // Each entry continues where the one before it ended, and the oldest one creates the ref.
                for pair in ours.windows(2) {
                    assert_eq!(
                        pair[0].0, pair[1].1.id,
                        "{refname}: old value is the previous new value"
                    );
                }
                assert!(
                    ours.last().expect("three").0.is_null(),
                    "{refname}: created from nothing"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn seeking_finds_every_reference_through_single_and_multi_level_indices() -> Result {
    for object_format in HASHES {
        let Some(fixture) = git_written_tables(object_format)? else {
            return Ok(());
        };
        for repo in ["plain", "deep"] {
            let repo = fixture.join(repo);
            let merged = merged(&repo)?;
            let names: Vec<_> = {
                let mut iter = merged.ref_iter()?;
                let mut names = Vec::new();
                while let Some(Record::Ref(record)) = iter.next_record()? {
                    names.push(record.refname);
                }
                names
            };
            assert!(names.len() > 1500);

            let mut iter = merged.ref_iter()?;
            for (idx, name) in names.iter().enumerate() {
                iter.seek_key(name)?;
                let Some(Record::Ref(found)) = iter.next_record()? else {
                    panic!("{object_format} {repo:?}: {name} must be found")
                };
                assert_eq!(&found.refname, name, "an exact key is found");

                // Just past a key, we get the key after it, which may be in another block.
                let mut after = name.to_vec();
                after.push(0);
                iter.seek_key(&after)?;
                let next = iter.next_record()?;
                match (names.get(idx + 1), next) {
                    (Some(expected), Some(Record::Ref(found))) => {
                        assert_eq!(&found.refname, expected, "seeking after {name}");
                    }
                    (None, None) => {}
                    (expected, found) => panic!("seeking after {name}: expected {expected:?}, got {found:?}"),
                }
            }
            iter.seek_key(b"")?;
            assert!(
                iter.next_record()?.is_some(),
                "seeking before everything finds the first record"
            );
            iter.seek_key(b"\xff")?;
            assert!(iter.next_record()?.is_none(), "seeking behind everything finds nothing");
        }
    }
    Ok(())
}

/// Where the log blocks start according to the footer of `table`, which is also where the blocks of references end.
fn log_section_offset(table: &[u8]) -> usize {
    let (header_len, footer_len) = if table[4] == 2 { (28, 72) } else { (24, 68) };
    let at = table.len() - footer_len + header_len + 3 * 8;
    usize::try_from(u64::from_be_bytes(table[at..at + 8].try_into().expect("8 bytes"))).expect("fits")
}

#[test]
fn rewriting_tables_written_by_git_reproduces_what_git_wrote() -> Result {
    for object_format in HASHES {
        let Some(fixture) = git_written_tables(object_format)? else {
            return Ok(());
        };
        let kind = util::kind(object_format);
        for repo in ["plain", "deep"] {
            let repo = fixture.join(repo);
            for name in util::table_names(&repo)? {
                let path = util::reftable_dir(&repo).join(&name);
                let original = std::fs::read(&path)?;
                let table = gix_reftable::Table::open(&path)?;
                let (refs, logs, options) = util::records_and_options(&table)?;
                assert!(!refs.is_empty() && !logs.is_empty(), "the table has both sections");
                let limits = (table.min_update_index(), table.max_update_index());
                let rewritten = util::write_table(kind, options, limits, refs, logs.clone())?;

                // Everything but the log blocks is a function of the records and the options alone.
                let log_offset = log_section_offset(&original);
                assert!(
                    rewritten.len() > log_offset && rewritten[..log_offset] == original[..log_offset],
                    "{object_format} {repo:?} {name}: reference blocks, padding and indices are those of Git, \
                     but the first difference is at {:?}",
                    rewritten.iter().zip(&original).position(|(a, b)| a != b)
                );
                // The deflated log blocks are the same when Git is linked against the zlib that `gix-zlib` imitates, which is
                // the case for the Git we test with, but not for every Git. Their content is the same regardless.
                let reread = gix_reftable::Table::from_block_source(
                    name.clone(),
                    gix_reftable::BlockSource::from_bytes(rewritten),
                )?;
                assert_eq!(
                    util::records_and_options(&reread)?.1,
                    logs,
                    "{object_format} {repo:?} {name}"
                );
            }
        }
    }
    Ok(())
}
