//! Damaged tables fail with errors and never panic, hang or are mistaken for tables that hold nothing.

use bstr::BString;
use gix_hash::Kind;
use gix_reftable::{
    BlockSource, Table,
    record::{LogRecord, LogValue, RefRecord, RefValue},
    writer::Options,
};

use crate::{Result, util::HASHES};

/// A table with several blocks of refs, an index and several compressed blocks of logs.
fn sample(kind: Kind) -> Result<Vec<u8>> {
    let id = |seed| gix_hash::ObjectId::try_from(vec![seed; kind.len_in_bytes()].as_slice()).expect("valid");
    let refs = (0..40u8)
        .map(|idx| RefRecord {
            refname: BString::from(format!("refs/heads/branch-{idx:02}")),
            update_index: 2,
            value: RefValue::Val1(id(idx)),
        })
        .collect();
    let logs = (0..40u8)
        .rev()
        .map(|idx| LogRecord {
            refname: "refs/heads/main".into(),
            update_index: 1 + u64::from(idx),
            value: LogValue::Update {
                old_hash: id(idx),
                new_hash: id(idx + 1),
                name: "committer".into(),
                email: "committer@example.com".into(),
                time: 1_577_123_507 + u64::from(idx),
                tz_offset: 0,
                message: format!("update number {idx}\n").into(),
            },
        })
        .collect();
    crate::util::write_table(
        kind,
        Options {
            block_size: 256,
            ..Default::default()
        },
        (1, 60),
        refs,
        logs,
    )
}

/// Read everything, so that all block, restart and record decoding runs.
fn read_all(bytes: Vec<u8>) -> gix_error::Result<(usize, usize)> {
    let table = Table::from_block_source("damaged".into(), BlockSource::from_bytes(bytes))?;
    let (mut refs, mut logs) = (0, 0);
    let mut iter = table.ref_iter()?;
    while iter.next_record()?.is_some() {
        refs += 1;
    }
    let mut iter = table.log_iter()?;
    while iter.next_record()?.is_some() {
        logs += 1;
    }
    Ok((refs, logs))
}

#[test]
fn flipping_any_single_byte_is_an_error_of_corruption_or_reads_on_without_panicking() -> Result {
    // The object id size is the only difference between the hashes, so one hash takes both kinds of damage
    // and the other a single one.
    for (hash, damages) in [("sha1", &[0xffu8, 0x80][..]), ("sha256", &[0xff][..])] {
        let kind: Kind = hash.parse()?;
        let original = sample(kind)?;
        assert_eq!(read_all(original.clone())?, (40, 40), "the sample is valid");
        let mut detected = 0;
        for pos in 0..original.len() {
            for damage in damages {
                let mut bytes = original.clone();
                bytes[pos] ^= damage;
                if let Err(err) = read_all(bytes) {
                    assert!(
                        err.is_corrupted() || err.is_unsupported(),
                        "{hash} byte {pos}: damage must be classified as such, but is {err:?}"
                    );
                    detected += 1;
                }
            }
        }
        assert!(
            detected > original.len() / 2,
            "{hash}: much of the damage is noticed, but not all as only the footer has a checksum: {detected}"
        );
    }
    Ok(())
}

#[test]
fn a_table_cut_off_anywhere_is_corrupted() -> Result {
    for hash in HASHES {
        let original = sample(hash.parse()?)?;
        for len in 0..original.len() {
            let err = read_all(original[..len].to_vec()).expect_err("the footer is missing or wrong");
            assert!(err.is_corrupted(), "{hash}, cut to {len} bytes: {err:?}");
        }
    }
    Ok(())
}

#[test]
fn unknown_versions_and_hash_formats_are_unsupported_not_corrupted() -> Result {
    let original = sample(Kind::Sha256)?;
    let footer_start = original.len() - 72;
    let patch = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = original.clone();
        f(&mut bytes);
        // The checksum covers all but its own four bytes, and the footer repeats the header.
        let header = bytes[..28].to_vec();
        bytes[footer_start..footer_start + 28].copy_from_slice(&header);
        let crc_at = bytes.len() - 4;
        let crc = crc32fast::hash(&bytes[footer_start..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_be_bytes());
        bytes
    };

    let err = read_all(patch(&|b| b[4] = 3)).expect_err("version 3 does not exist");
    assert!(err.is_unsupported(), "{err:?}");

    let err = read_all(patch(&|b| b[24..28].copy_from_slice(b"nope"))).expect_err("unknown hash format");
    assert!(err.is_corrupted(), "an unknown format id is invalid data: {err:?}");

    let err = read_all(patch(&|b| b[16..24].copy_from_slice(&0u64.to_be_bytes()))).map(|_| ());
    assert!(
        err.is_ok(),
        "a maximum update index which is lower than the minimum is not for the reader to check"
    );
    Ok(())
}
