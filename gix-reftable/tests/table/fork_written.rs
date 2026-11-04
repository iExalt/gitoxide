//! Tables written by this crate are read by Git.

use std::path::Path;

use bstr::{BString, ByteSlice};
use gix_hash::ObjectId;
use gix_reftable::{
    Table,
    record::{LogRecord, LogValue, RefRecord, RefValue},
    writer::Options,
};

use crate::{
    Result,
    util::{self, HASHES, assert_healthy, git, git_ref_lines, reftable_dir, writable_git_written_tables},
};

/// Replace all tables of `repo` with `tables`, which are given as bytes and their update index limits, oldest first.
fn install(repo: &Path, tables: &[(Vec<u8>, (u64, u64))]) -> Result {
    let dir = reftable_dir(repo);
    for entry in std::fs::read_dir(&dir)? {
        std::fs::remove_file(entry?.path())?;
    }
    let mut list = String::new();
    for (idx, (bytes, (min, max))) in tables.iter().enumerate() {
        let name = format!("0x{min:012x}-0x{max:012x}-{idx:08x}.ref");
        std::fs::write(dir.join(&name), bytes)?;
        list.push_str(&name);
        list.push('\n');
    }
    std::fs::write(dir.join("tables.list"), list)?;
    Ok(())
}

fn reflog_of_main(repo: &Path) -> Result<Vec<u8>> {
    git(
        repo,
        &[
            "reflog",
            "show",
            "--date=raw",
            "--format=%H%x09%gn%x09%ge%x09%gd%x09%gs",
            "refs/heads/main",
        ],
    )
}

#[test]
fn git_reads_tables_of_all_layouts_and_compacts_them() -> Result {
    let layouts = [
        ("default layout, padded blocks", Options::default()),
        (
            "unpadded blocks",
            Options {
                unpadded: true,
                ..Default::default()
            },
        ),
        (
            "small blocks with a multi-level index",
            Options {
                block_size: 512,
                ..Default::default()
            },
        ),
        (
            "a restart point at every record",
            Options {
                restart_interval: 1,
                ..Default::default()
            },
        ),
        (
            "few restart points",
            Options {
                restart_interval: 200,
                ..Default::default()
            },
        ),
    ];
    for object_format in HASHES {
        let Some(dir) = writable_git_written_tables(object_format)? else {
            return Ok(());
        };
        let repo = dir.path().join("compacted");
        let expected_refs = git_ref_lines(&repo)?;
        let expected_log = reflog_of_main(&repo)?;
        let table = Table::open(reftable_dir(&repo).join(&util::table_names(&repo)?[0]))?;
        let limits = (table.min_update_index(), table.max_update_index());
        let kind = util::kind(object_format);

        for (what, options) in layouts {
            let what = format!("{object_format}, {what}");
            let (refs, logs, base) = util::records_and_options(&table)?;
            let options = Options {
                block_size: if options.block_size == 0 {
                    base.block_size
                } else {
                    options.block_size
                },
                ..options
            };
            install(
                &repo,
                &[(util::write_table(kind, options, limits, refs, logs)?, limits)],
            )?;

            assert_eq!(git_ref_lines(&repo)?, expected_refs, "{what}: Git lists the same refs");
            assert_eq!(
                reflog_of_main(&repo)?,
                expected_log,
                "{what}: Git shows the same reflog"
            );
            assert_healthy(&repo, &what);

            // Git adds a table to ours and then merges both.
            let main_id = git(&repo, &["rev-parse", "refs/heads/main"])?;
            git(
                &repo,
                &["update-ref", "refs/heads/written-by-git", main_id.trim().to_str()?],
            )?;
            git(&repo, &["pack-refs", "--all"])?;
            let mut after = git_ref_lines(&repo)?;
            assert_eq!(
                after
                    .iter()
                    .filter(|line| line.starts_with(b"refs/heads/written-by-git\t"))
                    .count(),
                1,
                "{what}: Git's own update survives"
            );
            after.retain(|line| !line.starts_with(b"refs/heads/written-by-git\t"));
            assert_eq!(
                after, expected_refs,
                "{what}: nothing of ours is lost when Git merges the tables"
            );
            assert_eq!(reflog_of_main(&repo)?, expected_log, "{what}: nor reflogs");
            assert_healthy(&repo, &format!("{what}, after Git compacted"));
        }
    }
    Ok(())
}

#[test]
fn git_honours_tombstones_and_updates_in_the_newer_table_of_a_stack_written_here() -> Result {
    for object_format in HASHES {
        let Some(dir) = writable_git_written_tables(object_format)? else {
            return Ok(());
        };
        let repo = dir.path().join("compacted");
        let table = Table::open(reftable_dir(&repo).join(&util::table_names(&repo)?[0]))?;
        let kind = util::kind(object_format);
        let (refs, logs, options) = util::records_and_options(&table)?;
        let base_limits = (table.min_update_index(), table.max_update_index());
        let next = base_limits.1 + 1;

        let main_id: ObjectId = refs
            .iter()
            .find_map(|record| match (record.refname.as_slice(), &record.value) {
                (b"refs/heads/main", RefValue::Val1(id)) => Some(*id),
                _ => None,
            })
            .expect("main is a direct reference");
        let deleted_branch = BString::from("refs/heads/team02/sub2/feature-2");
        let deleted_log_keys: Vec<_> = logs
            .iter()
            .filter(|log| log.refname == deleted_branch)
            .map(|log| log.update_index)
            .collect();
        assert!(
            !deleted_log_keys.is_empty(),
            "the branch has a reflog which needs tombstones, too"
        );

        let new_ref = |name: &str, value| RefRecord {
            refname: name.into(),
            update_index: next,
            value,
        };
        // Records are sorted by name: `HEAD` < `refs/heads/alias` < `refs/heads/fork-new` < `refs/heads/team02…` < `refs/tags/…`.
        let newer_refs = vec![
            new_ref("refs/heads/alias", RefValue::Symref("refs/heads/fork-new".into())),
            new_ref("refs/heads/fork-new", RefValue::Val1(main_id)),
            new_ref(&deleted_branch.to_str_lossy(), RefValue::Deletion),
            new_ref("refs/tags/v1", RefValue::Deletion),
        ];
        let mut newer_logs = vec![LogRecord {
            refname: "refs/heads/fork-new".into(),
            update_index: next,
            value: LogValue::Update {
                old_hash: kind.null(),
                new_hash: main_id,
                name: "Z\u{f6}e".into(),
                email: "zoe@example.com".into(),
                time: 1_700_000_000,
                tz_offset: -830,
                message: "branch: Created from main\n".into(),
            },
        }];
        newer_logs.extend(deleted_log_keys.iter().map(|update_index| LogRecord {
            refname: deleted_branch.clone(),
            update_index: *update_index,
            value: LogValue::Deletion,
        }));
        // Logs of one name are sorted by descending update index.
        newer_logs.sort_by(|a, b| a.refname.cmp(&b.refname).then(b.update_index.cmp(&a.update_index)));

        let expected_before = git_ref_lines(&repo)?;
        let base = util::write_table(kind, options, base_limits, refs, logs)?;
        let newer = util::write_table(kind, Options::default(), (next, next), newer_refs, newer_logs)?;
        install(&repo, &[(base, base_limits), (newer, (next, next))])?;

        let mut expected = expected_before;
        expected.retain(|line| {
            !line.starts_with(deleted_branch.as_slice())
                && !line.starts_with(b"refs/tags/v1\t")
                && !line.starts_with(b"refs/heads/alias\t")
        });
        expected.push("refs/heads/alias\tSYM\trefs/heads/fork-new".into());
        expected.push(format!("refs/heads/fork-new\tV1\t{main_id}").into());
        expected.sort();
        assert_eq!(
            git_ref_lines(&repo)?,
            expected,
            "{object_format}: Git applies tombstones and newer values of our second table"
        );
        let mut exists = gix_testtools::git_command(&repo);
        assert!(
            !exists
                .args(["reflog", "exists", &deleted_branch.to_str_lossy()])
                .status()?
                .success(),
            "{object_format}: log tombstones remove the whole reflog of the deleted branch"
        );
        assert_healthy(&repo, &format!("{object_format}, two tables written here"));

        git(&repo, &["pack-refs", "--all"])?;
        assert_eq!(
            git_ref_lines(&repo)?,
            expected,
            "{object_format}: and so does Git's compaction of them"
        );
        assert_eq!(util::table_names(&repo)?.len(), 1);
        assert_healthy(&repo, &format!("{object_format}, after Git compacted both"));
    }
    Ok(())
}
