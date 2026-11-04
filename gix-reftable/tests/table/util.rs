use std::path::{Path, PathBuf};

use bstr::{BString, ByteSlice};
use gix_hash::{Kind, ObjectId};
use gix_reftable::{
    MergedTable, Table, Writer,
    record::{LogRecord, LogValue, Record, RefRecord, RefValue},
    writer::Options,
};

use crate::Result;

pub const BRANCHES: &str = "1500";
pub const HASHES: [&str; 2] = ["sha1", "sha256"];

/// The oldest Git with reftable support, which is also what the fixtures need.
pub fn git_supports_reftable(version: (u8, u8, u8)) -> bool {
    version >= (2, 45, 0)
}

/// The repositories of `make_git_written_tables.sh` for `object_format`, or `None` if Git is too old to make them.
pub fn git_written_tables(object_format: &str) -> Result<Option<PathBuf>> {
    gix_testtools::scripted_fixture_read_only_with_args_with_git_version(
        "make_git_written_tables.sh",
        [object_format, BRANCHES],
        git_supports_reftable,
    )
}

pub fn writable_git_written_tables(object_format: &str) -> Result<Option<gix_testtools::tempfile::TempDir>> {
    Ok(match git_written_tables(object_format)? {
        None => None,
        Some(read_only) => {
            let dir = gix_testtools::tempfile::tempdir()?;
            gix_testtools::copy_recursively_into_existing_dir(read_only, dir.path())?;
            Some(dir)
        }
    })
}

pub fn kind(object_format: &str) -> Kind {
    object_format.parse().expect("a valid object format")
}

pub fn reftable_dir(repo: &Path) -> PathBuf {
    repo.join(".git").join("reftable")
}

/// The names of the tables in `tables.list` order, oldest first.
pub fn table_names(repo: &Path) -> Result<Vec<String>> {
    Ok(std::fs::read_to_string(reftable_dir(repo).join("tables.list"))?
        .lines()
        .map(str::to_owned)
        .collect())
}

pub fn open_tables(repo: &Path) -> Result<Vec<Table>> {
    table_names(repo)?
        .into_iter()
        .map(|name| Ok(Table::open(reftable_dir(repo).join(name))?))
        .collect()
}

pub fn merged(repo: &Path) -> Result<MergedTable> {
    Ok(MergedTable::new(open_tables(repo)?)?.with_deletions_suppressed(true))
}

/// All reference records of a merged view in `refs/`, one line each in a form that is easy to compare.
pub fn ref_lines(merged: &MergedTable) -> Result<Vec<BString>> {
    let mut lines = Vec::new();
    let mut iter = merged.ref_iter()?;
    while let Some(record) = iter.next_record()? {
        let Record::Ref(RefRecord { refname, value, .. }) = record else {
            unreachable!("only references are iterated")
        };
        if !refname.starts_with(b"refs/") {
            continue;
        }
        let mut line = refname.to_vec();
        line.extend(
            match value {
                RefValue::Deletion => unreachable!("deletions are suppressed"),
                RefValue::Val1(id) => format!("\tV1\t{id}"),
                RefValue::Val2 { value, target_value } => format!("\tV2\t{value}\t{target_value}"),
                RefValue::Symref(target) => format!("\tSYM\t{target}"),
            }
            .into_bytes(),
        );
        lines.push(line.into());
    }
    Ok(lines)
}

/// The same lines, as Git reports them with `for-each-ref`.
pub fn git_ref_lines(repo: &Path) -> Result<Vec<BString>> {
    let output = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%09%(symref)%09%(objectname)%09%(*objectname)",
        ],
    )?;
    Ok(output
        .lines()
        .map(|line| {
            let fields: Vec<_> = line.split_str("\t").collect();
            let (name, symref, id, peeled) = (fields[0].as_bstr(), fields[1], fields[2], fields[3]);
            let mut line = name.to_vec();
            line.extend(
                if !symref.is_empty() {
                    format!("\tSYM\t{}", symref.as_bstr())
                } else if !peeled.is_empty() {
                    format!("\tV2\t{}\t{}", id.as_bstr(), peeled.as_bstr())
                } else {
                    format!("\tV1\t{}", id.as_bstr())
                }
                .into_bytes(),
            );
            line.into()
        })
        .collect())
}

/// Run Git in isolation and return its standard output, which unlike `gix_testtools::git()` may be no UTF-8.
pub fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = gix_testtools::git_command(repo);
    let output = cmd.args(args).output()?;
    assert!(output.status.success(), "{args:?} failed: {}", output.stderr.as_bstr());
    Ok(output.stdout)
}

pub struct Entry {
    pub id: ObjectId,
    pub name: BString,
    pub email: BString,
    pub message: BString,
    pub time: u64,
    pub tz_offset: i16,
}

/// The reflog of `refname` newest first, which is all records of the key in a merged view.
pub fn reflog(merged: &MergedTable, refname: &str) -> Result<Vec<(ObjectId, Entry)>> {
    let mut out = Vec::new();
    let mut iter = merged.log_iter()?;
    iter.seek_key(
        &gix_reftable::record::Record::Log(LogRecord {
            refname: refname.into(),
            update_index: u64::MAX,
            value: LogValue::Deletion,
        })
        .key(),
    )?;
    while let Some(Record::Log(log)) = iter.next_record()? {
        if log.refname != refname {
            break;
        }
        if let LogValue::Update {
            old_hash,
            new_hash,
            name,
            email,
            time,
            tz_offset,
            message,
        } = log.value
        {
            out.push((
                old_hash,
                Entry {
                    id: new_hash,
                    name,
                    email,
                    message,
                    time,
                    tz_offset,
                },
            ));
        }
    }
    Ok(out)
}

/// All records of `table`, including tombstones, and the options which reproduce its layout.
pub fn records_and_options(table: &Table) -> Result<(Vec<RefRecord>, Vec<LogRecord>, Options)> {
    let (mut refs, mut logs) = (Vec::new(), Vec::new());
    let mut iter = table.ref_iter()?;
    while let Some(Record::Ref(record)) = iter.next_record()? {
        refs.push(record);
    }
    let mut iter = table.log_iter()?;
    while let Some(Record::Log(record)) = iter.next_record()? {
        logs.push(record);
    }
    Ok((
        refs,
        logs,
        Options {
            block_size: table.block_size(),
            exact_log_message: true,
            ..Default::default()
        },
    ))
}

pub fn write_table(
    kind: Kind,
    options: Options,
    limits: (u64, u64),
    refs: Vec<RefRecord>,
    logs: Vec<LogRecord>,
) -> Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::new(), kind, options)?;
    writer.set_limits(limits.0, limits.1)?;
    for record in refs {
        writer.add_ref(record)?;
    }
    for record in logs {
        writer.add_log(record)?;
    }
    Ok(writer.finish()?)
}
