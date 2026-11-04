//! A single reftable file: parsing its header and footer, and iterating or seeking its records.

use std::path::Path;

use gix_error::Result;
use gix_hash::Kind;

use crate::{
    basics::{be24, be32, be64, corrupt, unsupported},
    block::{Block, BlockIter, footer_size, header_size},
    blocksource::BlockSource,
    record::{BlockType, IndexRecord, Record},
};

pub(crate) const FORMAT_ID_SHA1: u32 = 0x7368_6131;
pub(crate) const FORMAT_ID_SHA256: u32 = 0x7332_3536;

/// The longest chain of index blocks we follow, which is far more than any real table needs.
const MAX_INDEX_LEVELS: usize = 32;

/// Where a section of records starts, and where its index starts, if it has one.
#[derive(Debug, Clone, Copy, Default)]
struct Section {
    present: bool,
    offset: u64,
    index_offset: u64,
}

/// A single reftable file, held in memory.
#[derive(Debug, Clone)]
pub struct Table {
    name: String,
    source: BlockSource,
    /// The size of the table without its footer.
    size: u64,
    hash_kind: Kind,
    version: u8,
    block_size: u32,
    min_update_index: u64,
    max_update_index: u64,
    refs: Section,
    logs: Section,
}

impl Table {
    /// Read the table at `path`. Its name is the name of the file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        Self::from_block_source(name, BlockSource::from_file(path)?)
    }

    /// Parse `source` as a table, which is called `name`.
    ///
    /// Fails with a corruption error if the header or footer are not valid, which includes a footer whose checksum does not match.
    pub fn from_block_source(name: String, source: BlockSource) -> Result<Self> {
        let data = source.bytes();
        let magic_and_version = data
            .get(..5)
            .filter(|bytes| bytes.starts_with(b"REFT"))
            .ok_or_else(|| corrupt("file does not start with the reftable signature"))?;
        let version = magic_and_version[4];
        if version != 1 && version != 2 {
            return Err(unsupported("reftable has an unknown version"));
        }
        let (header_len, footer_len) = (header_size(version), footer_size(version));
        let size = data
            .len()
            .checked_sub(footer_len)
            .filter(|size| *size >= header_len)
            .ok_or_else(|| corrupt("file is too small for a header and footer"))?;
        let (header, footer) = (&data[..header_len], &data[size..]);
        if header != &footer[..header_len] {
            return Err(corrupt("footer does not repeat the header"));
        }
        let mut pos = header_len + 8 * 5;
        let expected_crc = crc32fast::hash(&footer[..pos]);
        if be32(footer, pos)? != expected_crc {
            return Err(corrupt("footer checksum does not match"));
        }

        let block_size = be24(header, 5)?;
        let (min_update_index, max_update_index) = (be64(header, 8)?, be64(header, 16)?);
        let hash_kind = if version == 1 {
            kind_for_size(20)?
        } else {
            match be32(header, 24)? {
                FORMAT_ID_SHA1 => kind_for_size(20)?,
                FORMAT_ID_SHA256 => kind_for_size(32)?,
                _ => return Err(corrupt("table has an unknown hash format id")),
            }
        };

        pos = header_len;
        let ref_index_offset = be64(footer, pos)?;
        // The object index offset shares its field with the length of the object id prefixes, which is of no use to us.
        let _obj_offset = be64(footer, pos + 8)? >> 5;
        let log_offset = be64(footer, pos + 24)?;
        let log_index_offset = be64(footer, pos + 32)?;

        let first_block_type = data.get(header_len).copied();
        let refs = Section {
            present: first_block_type == Some(BlockType::Ref as u8),
            offset: 0,
            index_offset: ref_index_offset,
        };
        let logs = Section {
            present: first_block_type == Some(BlockType::Log as u8) || log_offset > 0,
            offset: log_offset,
            index_offset: log_index_offset,
        };
        let size = size as u64;
        if [ref_index_offset, log_offset, log_index_offset]
            .iter()
            .any(|off| *off >= size.max(1))
        {
            return Err(corrupt("footer points beyond the end of the data"));
        }
        Ok(Self {
            name,
            source,
            size,
            hash_kind,
            version,
            block_size,
            min_update_index,
            max_update_index,
            refs,
            logs,
        })
    }

    /// The name of the table, which is the name of its file.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The hash used for object ids in this table.
    pub fn hash_kind(&self) -> Kind {
        self.hash_kind
    }

    /// The format version, which is 1 for SHA-1 only tables and 2 otherwise.
    pub fn version(&self) -> u8 {
        self.version
    }

    /// The block size the table was written with, or 0 if its blocks are not padded to a common size.
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// The lowest update index of any transaction that contributed records to this table.
    pub fn min_update_index(&self) -> u64 {
        self.min_update_index
    }

    /// The highest update index of any transaction that contributed records to this table.
    pub fn max_update_index(&self) -> u64 {
        self.max_update_index
    }

    /// The size of the table without its footer, in bytes.
    pub fn data_size(&self) -> u64 {
        self.size
    }

    /// The size of the whole file in bytes, including its footer.
    pub fn file_size(&self) -> u64 {
        self.source.size()
    }

    /// Whether the table contains reference records.
    pub fn has_refs(&self) -> bool {
        self.refs.present
    }

    /// Whether the table contains reflog records.
    pub fn has_logs(&self) -> bool {
        self.logs.present
    }

    fn section(&self, block_type: BlockType) -> Section {
        match block_type {
            BlockType::Ref => self.refs,
            BlockType::Log => self.logs,
            // We neither read the object index nor index blocks by type.
            BlockType::Obj | BlockType::Index => Section::default(),
        }
    }

    fn init_block(&self, offset: u64, want: Option<BlockType>) -> Result<Option<Block>> {
        if offset >= self.size {
            return Ok(None);
        }
        let header_off = if offset == 0 { header_size(self.version) } else { 0 };
        Block::init(
            &self.source,
            offset,
            header_off,
            self.block_size as usize,
            self.hash_kind.len_in_bytes(),
            want,
        )
    }

    /// Iterate all reference records.
    pub fn ref_iter(&self) -> Result<TableIter<'_>> {
        TableIter::new(self, BlockType::Ref)
    }

    /// Iterate all reflog records, newest first within each reference.
    pub fn log_iter(&self) -> Result<TableIter<'_>> {
        TableIter::new(self, BlockType::Log)
    }
}

fn kind_for_size(len: usize) -> Result<Kind> {
    Kind::all()
        .iter()
        .copied()
        .find(|kind| kind.len_in_bytes() == len)
        .ok_or_else(|| unsupported("table uses a hash which is not enabled in this build"))
}

/// An iterator over all records of one kind in a single table.
pub struct TableIter<'a> {
    table: &'a Table,
    block_type: BlockType,
    /// The offset of the block `block_iter` iterates.
    block_off: u64,
    block_iter: Option<BlockIter>,
}

impl<'a> TableIter<'a> {
    fn new(table: &'a Table, block_type: BlockType) -> Result<Self> {
        let section = table.section(block_type);
        let mut iter = Self {
            table,
            block_type,
            block_off: section.offset,
            block_iter: None,
        };
        if section.present {
            iter.load(section.offset)?;
        }
        Ok(iter)
    }

    fn load(&mut self, offset: u64) -> Result<()> {
        self.block_off = offset;
        self.block_iter = self
            .table
            .init_block(offset, Some(self.block_type))?
            .map(BlockIter::new);
        Ok(())
    }

    /// Position the iterator at the first record whose key is not smaller than `want`.
    pub fn seek_key(&mut self, want: &[u8]) -> Result<()> {
        let section = self.table.section(self.block_type);
        self.block_iter = None;
        if !section.present {
            return Ok(());
        }
        if section.index_offset > 0 {
            self.seek_indexed(section.index_offset, want)
        } else {
            self.seek_linear(section.offset, want)
        }
    }

    /// Walk down the index levels, starting at the topmost, to the block which can hold `want`.
    fn seek_indexed(&mut self, index_offset: u64, want: &[u8]) -> Result<()> {
        let Some(mut record) = self.find_in_index_level(index_offset, want)? else {
            // `want` is after the last key of the table.
            return Ok(());
        };
        for _ in 0..MAX_INDEX_LEVELS {
            let Some(block) = self.table.init_block(record.offset, None)? else {
                return Err(corrupt("index points beyond the end of the data"));
            };
            let mut iter = BlockIter::new(block);
            iter.seek_key(want)?;
            match iter.block().block_type {
                BlockType::Index => match iter.next_record()? {
                    Some(Record::Index(lower)) => record = lower,
                    _ => return Ok(()),
                },
                block_type if block_type == self.block_type => {
                    self.block_off = record.offset;
                    self.block_iter = Some(iter);
                    return Ok(());
                }
                _ => return Err(corrupt("index leads to a block of the wrong type")),
            }
        }
        Err(corrupt("index has more levels than any table can need"))
    }

    /// Find the first index record whose key is not smaller than `want` in the level that starts at `start`.
    ///
    /// The topmost level may consist of up to three blocks, which are searched one after another.
    fn find_in_index_level(&self, start: u64, want: &[u8]) -> Result<Option<IndexRecord>> {
        let mut at = start;
        loop {
            let Some(block) = self.table.init_block(at, None)? else {
                return Ok(None);
            };
            if block.block_type != BlockType::Index {
                return if at == start {
                    Err(corrupt("the offset of an index does not lead to an index block"))
                } else {
                    Ok(None)
                };
            }
            let next = at + block.full_block_size as u64;
            let mut iter = BlockIter::new(block);
            iter.seek_key(want)?;
            if let Some(Record::Index(record)) = iter.next_record()? {
                return Ok(Some(record));
            }
            at = next;
        }
    }

    /// Find the block holding `want` by checking the first keys of all blocks of the section.
    fn seek_linear(&mut self, start: u64, want: &[u8]) -> Result<()> {
        self.load(start)?;
        loop {
            let Some(current) = self.block_iter.as_ref() else {
                return Ok(());
            };
            let next_off = self.block_off + current.block().full_block_size as u64;
            let Some(next) = self.table.init_block(next_off, Some(self.block_type))? else {
                break;
            };
            if next.first_key()?.as_slice() > want {
                break;
            }
            self.block_off = next_off;
            self.block_iter = Some(BlockIter::new(next));
        }
        if let Some(iter) = self.block_iter.as_mut() {
            iter.seek_key(want)?;
        }
        Ok(())
    }

    /// Return the next record, or `None` once the section is exhausted.
    ///
    /// The update index of reference records is the one of the transaction that wrote it,
    /// not the delta to the table's minimum which is what is stored.
    pub fn next_record(&mut self) -> Result<Option<Record>> {
        loop {
            let Some(iter) = self.block_iter.as_mut() else {
                return Ok(None);
            };
            if let Some(mut record) = iter.next_record()? {
                if let Record::Ref(r) = &mut record {
                    r.update_index = r
                        .update_index
                        .checked_add(self.table.min_update_index)
                        .ok_or_else(|| corrupt("update index overflows"))?;
                }
                return Ok(Some(record));
            }
            let next_off = self.block_off + iter.block().full_block_size as u64;
            self.load(next_off)?;
        }
    }
}
