//! Writing a single table, block by block, with the layout Git produces.

use std::io::Write;

use bstr::ByteSlice;
use gix_error::{Result, ResultExt, message};
use gix_hash::Kind;
use gix_zlib::{
    Compression, Status,
    stream::deflate::{Compress, FlushCompress},
};

use crate::{
    basics::{corrupt, misuse, put_be24, unsupported},
    block::{DEFAULT_BLOCK_SIZE, footer_size, header_size},
    record::{BlockType, IndexRecord, LogRecord, LogValue, Record, RefRecord, encode_key},
    table::{FORMAT_ID_SHA1, FORMAT_ID_SHA256},
};

/// The most restart points a block can have, as its count has 16 bits.
const MAX_RESTARTS: usize = (1 << 16) - 1;
const DEFAULT_RESTART_INTERVAL: usize = 16;
/// The block size field of the header has 24 bits.
const MAX_BLOCK_SIZE: u32 = (1 << 24) - 1;

/// Options which control the layout of a table.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// The size blocks are limited to and padded to, or 0 for the default of 4096 bytes.
    pub block_size: u32,
    /// After how many records a block restarts key compression, or 0 for the default of 16.
    pub restart_interval: u16,
    /// Don't pad blocks other than log blocks up to `block_size`.
    pub unpadded: bool,
    /// Store log messages as they are, instead of making them a single line that ends in a newline.
    pub exact_log_message: bool,
}

/// A block under construction, whose records are kept in file layout until it is finished.
struct BlockWriter {
    block_type: BlockType,
    /// The bytes of the block, preceded by room for the table header if this is the first block.
    buf: Vec<u8>,
    header_off: usize,
    block_size: usize,
    restart_interval: usize,
    entries: usize,
    restarts: Vec<u32>,
    last_key: Vec<u8>,
}

enum Added {
    Yes,
    /// The record doesn't fit what is left of the block.
    TooBig,
}

impl BlockWriter {
    fn new(block_type: BlockType, header_off: usize, block_size: usize, restart_interval: usize) -> Self {
        let mut buf = vec![0; header_off + 4];
        buf[header_off] = block_type as u8;
        Self {
            block_type,
            buf,
            header_off,
            block_size,
            restart_interval,
            entries: 0,
            restarts: Vec::new(),
            last_key: Vec::new(),
        }
    }

    fn add(&mut self, key: &[u8], val_type: u8, value: &[u8]) -> Added {
        let prev_key: &[u8] = if self.entries.is_multiple_of(self.restart_interval) {
            &[]
        } else {
            &self.last_key
        };
        let mut encoded = Vec::with_capacity(key.len() + value.len() + 4);
        let mut is_restart = encode_key(&mut encoded, prev_key, key, val_type);
        encoded.extend_from_slice(value);

        is_restart &= self.restarts.len() < MAX_RESTARTS;
        let restarts_after = self.restarts.len() + usize::from(is_restart);
        // What remains of the block must also take the restart table and its count.
        if 2 + 3 * restarts_after + encoded.len() > self.block_size.saturating_sub(self.buf.len()) {
            return Added::TooBig;
        }
        if is_restart {
            self.restarts.push(self.buf.len() as u32);
        }
        self.buf.extend_from_slice(&encoded);
        self.last_key.clear();
        self.last_key.extend_from_slice(key);
        self.entries += 1;
        Added::Yes
    }

    /// Add the restart table and finalize the size, then compress the records of a log block.
    fn finish(mut self, compress: &mut Compress) -> Result<(Vec<u8>, Vec<u8>)> {
        for restart in &self.restarts {
            put_be24(&mut self.buf, *restart);
        }
        self.buf.extend_from_slice(&(self.restarts.len() as u16).to_be_bytes());
        let size = u32::try_from(self.buf.len())
            .ok()
            .filter(|size| *size <= MAX_BLOCK_SIZE)
            .ok_or_else(|| misuse("block exceeds the 24-bit size field"))?;
        self.buf[self.header_off + 1..self.header_off + 4].copy_from_slice(&size.to_be_bytes()[1..]);

        if self.block_type == BlockType::Log {
            let skip = self.header_off + 4;
            let mut compressed = vec![0u8; self.buf.len() - skip + (self.buf.len() - skip) / 8 + 64];
            compress.reset();
            let status = compress
                .compress(&self.buf[skip..], &mut compressed, FlushCompress::Finish)
                .or_raise(|| message("could not compress a log block"))?;
            if status != Status::StreamEnd {
                return Err(corrupt("log block did not compress in one go"));
            }
            compressed.truncate(usize::try_from(compress.total_out()).unwrap_or(usize::MAX));
            self.buf.truncate(skip);
            self.buf.extend_from_slice(&compressed);
        }
        Ok((self.buf, self.last_key))
    }
}

/// Writes one table into a sink, record by record.
///
/// Records must be added in their sort order, with reference records before log records: refs ascending by name,
/// logs ascending by name and then descending by update index.
/// The output follows the layout of Git's own writer, including the padding of blocks and multi-level indices,
/// so that tables are interchangeable between both. It does not write the optional object index.
pub struct Writer<W: Write> {
    sink: W,
    hash_kind: Kind,
    block_size: usize,
    restart_interval: usize,
    unpadded: bool,
    exact_log_message: bool,
    min_update_index: u64,
    max_update_index: u64,
    /// How many bytes of the table are accounted for, which excludes padding that is still pending.
    next: u64,
    /// Zeroes which pad the previous block and are only written if another block follows, so that the table doesn't end in them.
    pending_padding: usize,
    /// The last key that was added, to enforce the order. It is only reset when a new section or index level starts.
    last_key: Vec<u8>,
    block: Option<BlockWriter>,
    /// One record for each block written since the section or index level started.
    index: Vec<IndexRecord>,
    ref_index_offset: u64,
    log_offset: u64,
    log_index_offset: u64,
    log_blocks: usize,
    compress: Compress,
}

impl<W: Write> Writer<W> {
    /// Create a writer for a table with object ids of `hash_kind` which writes to `sink`.
    ///
    /// Fails if the hash kind has no table format or `options` are out of range.
    pub fn new(sink: W, hash_kind: Kind, options: Options) -> Result<Self> {
        if hash_kind.len_in_bytes() != 20 && hash_kind.len_in_bytes() != 32 {
            return Err(unsupported("no table format exists for this hash"));
        }
        let block_size = if options.block_size == 0 {
            DEFAULT_BLOCK_SIZE
        } else {
            options.block_size as usize
        };
        if block_size > MAX_BLOCK_SIZE as usize {
            return Err(misuse("block size does not fit the 24-bit size field"));
        }
        let restart_interval = match options.restart_interval {
            0 => DEFAULT_RESTART_INTERVAL,
            interval => usize::from(interval),
        };
        Ok(Self {
            sink,
            hash_kind,
            block_size,
            restart_interval,
            unpadded: options.unpadded,
            exact_log_message: options.exact_log_message,
            min_update_index: 0,
            max_update_index: 0,
            next: 0,
            pending_padding: 0,
            last_key: Vec::new(),
            block: None,
            index: Vec::new(),
            ref_index_offset: 0,
            log_offset: 0,
            log_index_offset: 0,
            log_blocks: 0,
            compress: Compress::new(Compression::BEST),
        })
    }

    /// Set the range of update indices of the transactions whose records the table contains.
    ///
    /// This must be done before records are added, and defaults to 0 for both ends.
    pub fn set_limits(&mut self, min: u64, max: u64) -> Result<()> {
        if self.next != 0 || !self.last_key.is_empty() || self.block.as_ref().is_some_and(|b| b.entries > 0) {
            return Err(misuse("limits must be set before records are added"));
        }
        if min > max {
            return Err(misuse("the minimum update index must not exceed the maximum"));
        }
        self.min_update_index = min;
        self.max_update_index = max;
        Ok(())
    }

    /// Whether no record was added yet, in which case [`finish()`](Self::finish()) produces a table without any content.
    pub fn is_empty(&self) -> bool {
        self.next == 0 && self.block.as_ref().is_none_or(|block| block.entries == 0)
    }

    /// Add a reference record, whose `update_index` must be within the limits.
    pub fn add_ref(&mut self, record: RefRecord) -> Result<()> {
        if record.refname.is_empty() {
            return Err(misuse("references need a name"));
        }
        if record.update_index < self.min_update_index || record.update_index > self.max_update_index {
            return Err(misuse(
                "update index of a reference is outside of the limits of the table",
            ));
        }
        self.add_record(Record::Ref(record))
    }

    /// Add a log record. Unless it is a tombstone, its `update_index` must not exceed the maximum,
    /// but is allowed to be below the minimum as reflog entries can be replaced by newer ones with the same index.
    pub fn add_log(&mut self, mut record: LogRecord) -> Result<()> {
        if record.refname.is_empty() {
            return Err(misuse("log records need the name of a reference"));
        }
        if let LogValue::Update { message, .. } = &mut record.value {
            if record.update_index > self.max_update_index {
                return Err(misuse("update index of a log record is above the maximum of the table"));
            }
            if !self.exact_log_message {
                let trimmed = message.trim_end_with(|c| c == '\n').len();
                message.truncate(trimmed);
                if message.contains(&b'\n') {
                    return Err(misuse(
                        "log messages must be a single line unless they are stored exactly",
                    ));
                }
                message.push(b'\n');
            }
        }
        if self.block.as_ref().is_some_and(|b| b.block_type == BlockType::Ref) {
            self.finish_section()?;
        }
        // The padding of the references section is not needed as log blocks follow it right away.
        self.next -= self.pending_padding as u64;
        self.pending_padding = 0;
        self.add_record(Record::Log(record))
    }

    /// Finish the table by writing the last blocks, the indices and the footer, and return the sink.
    pub fn finish(mut self) -> Result<W> {
        if self.block.is_some() {
            self.finish_section()?;
        }
        self.pending_padding = 0;
        let empty_table = self.next == 0;
        let version = if self.hash_kind.len_in_bytes() == 32 { 2 } else { 1 };
        let header = self.header(version);
        if empty_table {
            self.padded_write(&header, 0)?;
        }
        let mut footer = header;
        for offset in [
            self.ref_index_offset,
            // The object index is not written: no offset, no object id length, no index.
            0,
            0,
            self.log_offset,
            self.log_index_offset,
        ] {
            footer.extend_from_slice(&offset.to_be_bytes());
        }
        let crc = crc32fast::hash(&footer);
        footer.extend_from_slice(&crc.to_be_bytes());
        debug_assert_eq!(footer.len(), footer_size(version));
        self.padded_write(&footer, 0)?;
        self.sink.flush().or_raise(|| message("could not flush the table"))?;
        Ok(self.sink)
    }

    fn header(&self, version: u8) -> Vec<u8> {
        let mut header = Vec::with_capacity(header_size(version));
        header.extend_from_slice(b"REFT");
        header.push(version);
        put_be24(&mut header, self.block_size as u32);
        header.extend_from_slice(&self.min_update_index.to_be_bytes());
        header.extend_from_slice(&self.max_update_index.to_be_bytes());
        if version == 2 {
            let id = if self.hash_kind.len_in_bytes() == 32 {
                FORMAT_ID_SHA256
            } else {
                FORMAT_ID_SHA1
            };
            header.extend_from_slice(&id.to_be_bytes());
        }
        header
    }

    fn start_block(&mut self, block_type: BlockType) {
        let header_off = if self.next == 0 {
            header_size(if self.hash_kind.len_in_bytes() == 32 { 2 } else { 1 })
        } else {
            0
        };
        self.block = Some(BlockWriter::new(
            block_type,
            header_off,
            self.block_size,
            self.restart_interval,
        ));
    }

    fn add_record(&mut self, record: Record) -> Result<()> {
        let block_type = record.block_type();
        let key = record.key();
        if self.last_key >= key {
            return Err(misuse(
                "records must be added in ascending key order without duplicates",
            ));
        }
        let value = record.encode_value(self.min_update_index)?;
        let val_type = record.val_type();
        if self.block.is_none() {
            self.start_block(block_type);
        }
        if self.block.as_ref().map(|b| b.block_type) != Some(block_type) {
            return Err(misuse("records of different types cannot share a section"));
        }
        let block = self.block.as_mut().expect("just ensured");
        if let Added::Yes = block.add(&key, val_type, &value) {
            self.last_key = key;
            return Ok(());
        }
        // The block is full: write it and retry in a fresh one, which is the only chance to fit.
        self.flush_block()?;
        self.start_block(block_type);
        let block = self.block.as_mut().expect("just started");
        if let Added::TooBig = block.add(&key, val_type, &value) {
            return Err(misuse("record does not fit into a block of the configured size"));
        }
        self.last_key = key;
        Ok(())
    }

    fn flush_block(&mut self) -> Result<()> {
        if self.block.as_ref().is_none_or(|block| block.entries == 0) {
            return Ok(());
        }
        let block = self.block.take().expect("checked above");
        let block_type = block.block_type;
        let (mut bytes, last_key) = block.finish(&mut self.compress)?;
        let padding = if !self.unpadded && block_type != BlockType::Log {
            self.block_size.saturating_sub(bytes.len())
        } else {
            0
        };
        if block_type == BlockType::Log && self.log_blocks == 0 {
            self.log_offset = self.next;
        }
        if block_type == BlockType::Log {
            self.log_blocks += 1;
        }
        if self.next == 0 {
            let version = if self.hash_kind.len_in_bytes() == 32 { 2 } else { 1 };
            let header = self.header(version);
            bytes[..header.len()].copy_from_slice(&header);
        }
        self.padded_write(&bytes, padding)?;
        self.index.push(IndexRecord {
            last_key,
            offset: self.next,
        });
        self.next += (padding + bytes.len()) as u64;
        Ok(())
    }

    /// Write the zeroes that pad the previous block, then `data`, and remember `padding` for after it.
    fn padded_write(&mut self, data: &[u8], padding: usize) -> Result<()> {
        if self.pending_padding > 0 {
            self.sink
                .write_all(&vec![0; self.pending_padding])
                .or_raise(|| message("could not write block padding"))?;
        }
        self.pending_padding = padding;
        self.sink
            .write_all(data)
            .or_raise(|| message("could not write a table block"))
    }

    /// Flush the current block and write the index of the section if its blocks are numerous enough.
    fn finish_section(&mut self) -> Result<()> {
        let block_type = self.block.as_ref().map(|block| block.block_type);
        self.flush_block()?;

        // Index levels pointing at lower ones are written until a level has so few blocks that readers can scan it.
        let threshold = if self.unpadded { 1 } else { 3 };
        let mut index_start = 0;
        while self.index.len() > threshold {
            index_start = self.next;
            self.last_key.clear();
            self.start_block(BlockType::Index);
            for record in std::mem::take(&mut self.index) {
                self.add_record(Record::Index(record))?;
            }
            self.flush_block()?;
        }
        self.index.clear();
        match block_type {
            Some(BlockType::Ref) => self.ref_index_offset = index_start,
            Some(BlockType::Log) => self.log_index_offset = index_start,
            _ => {}
        }
        // The next section starts with any key.
        self.last_key.clear();
        self.block = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bstr::BString;
    use gix_hash::ObjectId;

    use super::*;
    use crate::{blocksource::BlockSource, record::RefValue, table::Table};

    fn oid(seed: u8) -> ObjectId {
        ObjectId::try_from([seed; 20].as_slice()).expect("sha1 is enabled in tests")
    }

    fn val1(name: impl Into<BString>, update_index: u64, seed: u8) -> RefRecord {
        RefRecord {
            refname: name.into(),
            update_index,
            value: RefValue::Val1(oid(seed)),
        }
    }

    fn write(options: Options, f: impl FnOnce(&mut Writer<Vec<u8>>) -> Result<()>) -> Result<Vec<u8>> {
        let mut writer = Writer::new(Vec::new(), Kind::Sha1, options)?;
        writer.set_limits(1, 3)?;
        f(&mut writer)?;
        writer.finish()
    }

    #[test]
    fn keys_must_ascend_even_across_blocks() {
        let options = Options {
            block_size: 96,
            ..Default::default()
        };
        let err = write(options, |w| {
            for idx in 0..10u8 {
                w.add_ref(val1(format!("refs/heads/{idx}"), 2, idx))?;
            }
            // Goes back to a key of an earlier block, which is not noticed by looking at the current block only.
            w.add_ref(val1("refs/heads/0", 2, 0))
        })
        .expect_err("out of order");
        assert!(err.is_validation(), "{err:?}");
        assert!(
            write(Options::default(), |w| {
                w.add_ref(val1("a", 2, 1))?;
                w.add_ref(val1("a", 2, 1))
            })
            .is_err(),
            "duplicates are rejected"
        );
    }

    #[test]
    fn limits_apply_to_refs_but_logs_may_precede_the_minimum() {
        let log = |index| LogRecord {
            refname: "a".into(),
            update_index: index,
            value: LogValue::Update {
                old_hash: oid(0),
                new_hash: oid(1),
                name: "n".into(),
                email: "e".into(),
                time: 0,
                tz_offset: 0,
                message: "m".into(),
            },
        };
        assert!(
            write(Options::default(), |w| w.add_ref(val1("a", 0, 1))).is_err(),
            "below the minimum"
        );
        assert!(
            write(Options::default(), |w| w.add_ref(val1("a", 4, 1))).is_err(),
            "above the maximum"
        );
        assert!(
            write(Options::default(), |w| w.add_log(log(0))).is_ok(),
            "reflog entries may be older than the table"
        );
        assert!(
            write(Options::default(), |w| w.add_log(log(4))).is_err(),
            "but not newer"
        );
    }

    #[test]
    fn a_record_which_exceeds_the_block_size_is_rejected() {
        let options = Options {
            block_size: 64,
            ..Default::default()
        };
        let err = write(options, |w| w.add_ref(val1("x".repeat(100), 2, 1))).expect_err("too big");
        assert!(err.is_validation());
    }

    #[test]
    fn padding_is_dropped_where_nothing_follows_it() {
        let small = write(Options::default(), |w| w.add_ref(val1("a", 2, 1))).expect("valid");
        assert!(
            small.len() < 200,
            "a single block is not padded to the block size, but was {} bytes",
            small.len()
        );
    }

    #[test]
    fn written_tables_can_be_read_back() {
        let bytes = write(
            Options {
                block_size: 128,
                ..Default::default()
            },
            |w| {
                for idx in 0..40u8 {
                    w.add_ref(val1(format!("refs/heads/{idx:02}"), 2, idx))?;
                }
                Ok(())
            },
        )
        .expect("valid");
        let table = Table::from_block_source("mem".into(), BlockSource::from_bytes(bytes)).expect("valid table");
        let mut iter = table.ref_iter().expect("refs");
        let mut count = 0;
        while iter.next_record().expect("valid").is_some() {
            count += 1;
        }
        assert_eq!(count, 40);
    }
}
