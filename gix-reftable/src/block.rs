//! Reading single blocks: decoding them, seeking inside them and iterating their records.

use gix_error::Result;
use gix_zlib::{Decompress, FlushDecompress, Status};

use crate::{
    basics::{be16, be24, corrupt, take},
    blocksource::BlockSource,
    record::{BlockType, Record, decode_key, decode_key_len},
};

/// The size of the file header, which is also what the first block of the table starts after.
pub(crate) const fn header_size(version: u8) -> usize {
    if version == 2 { 28 } else { 24 }
}

/// The size of the file footer.
pub(crate) const fn footer_size(version: u8) -> usize {
    if version == 2 { 72 } else { 68 }
}

pub(crate) const DEFAULT_BLOCK_SIZE: usize = 4096;

/// A decoded block. Log blocks are inflated, others are used as they are.
#[derive(Debug, Clone)]
pub(crate) struct Block {
    /// Non-zero only for the first block of a table, which starts with the table header.
    header_off: usize,
    /// The block's bytes, up to and including the restart table and its length. The block type and size are at `header_off`.
    data: Vec<u8>,
    hash_size: usize,
    restart_count: usize,
    restart_off: usize,
    /// How many bytes of the file belong to this block, including padding. The next block starts after them.
    pub(crate) full_block_size: usize,
    pub(crate) block_type: BlockType,
}

impl Block {
    /// Decode the block at `offset`.
    ///
    /// Returns `None` if `offset` is at the end of the data, or if the block is not of `want_type`.
    pub(crate) fn init(
        source: &BlockSource,
        offset: u64,
        header_off: usize,
        table_block_size: usize,
        hash_size: usize,
        want_type: Option<BlockType>,
    ) -> Result<Option<Self>> {
        let guess = if table_block_size > 0 {
            table_block_size
        } else {
            DEFAULT_BLOCK_SIZE
        };
        let first = source.read(offset, guess);
        if first.is_empty() {
            return Ok(None);
        }
        let block_type = BlockType::from_byte(
            *first
                .get(header_off)
                .ok_or_else(|| corrupt("block is shorter than its header"))?,
        )
        .ok_or_else(|| corrupt("block has an unknown type"))?;
        if want_type.is_some_and(|want| want != block_type) {
            return Ok(None);
        }
        let block_size = be24(first, header_off + 1)? as usize;
        if block_size < header_off + 4 + 2 {
            return Err(corrupt("block size is too small to hold a restart count"));
        }
        let raw = if block_size > first.len() {
            source.read(offset, block_size)
        } else {
            first
        };

        let (data, full_block_size) = if block_type == BlockType::Log {
            let skip = header_off + 4;
            let compressed = raw
                .get(skip..)
                .ok_or_else(|| corrupt("log block is shorter than its header"))?;
            let mut data = vec![0u8; block_size];
            data[..skip].copy_from_slice(&raw[..skip]);
            let mut inflate = Decompress::new();
            let status = inflate
                .decompress(compressed, &mut data[skip..], FlushDecompress::Finish)
                .map_err(|_| corrupt("log block does not hold a valid zlib stream"))?;
            let produced = usize::try_from(inflate.total_out()).unwrap_or(usize::MAX);
            if status != Status::StreamEnd || produced != block_size - skip {
                return Err(corrupt("log block does not inflate to the size its header states"));
            }
            let consumed = usize::try_from(inflate.total_in()).unwrap_or(usize::MAX);
            (data, skip + consumed)
        } else {
            let data = raw
                .get(..block_size)
                .ok_or_else(|| corrupt("block is truncated"))?
                .to_vec();
            let mut full = if table_block_size == 0 {
                block_size
            } else {
                table_block_size
            };
            // Without padding, the next block follows right after this one, which is when
            // what lies beyond this block's size is not the zeroes of padding.
            if block_size < full && raw.get(block_size).is_some_and(|byte| *byte != 0) {
                full = block_size;
            }
            (data, full)
        };

        let restart_count = usize::from(be16(&data, block_size - 2)?);
        let restart_off = (block_size - 2)
            .checked_sub(3 * restart_count)
            .filter(|off| *off >= header_off + 4)
            .ok_or_else(|| corrupt("restart table does not fit into its block"))?;
        Ok(Some(Self {
            header_off,
            data,
            hash_size,
            restart_count,
            restart_off,
            full_block_size,
            block_type,
        }))
    }

    /// The key of the first record, which is stored without any prefix.
    pub(crate) fn first_key(&self) -> Result<Vec<u8>> {
        let records = &self.data[self.header_off + 4..self.restart_off];
        let mut key = Vec::new();
        decode_key(&mut key, records)?;
        if key.is_empty() {
            return Err(corrupt("first record of a block has an empty key"));
        }
        Ok(key)
    }

    fn restart_offset(&self, idx: usize) -> Result<usize> {
        debug_assert!(idx < self.restart_count);
        Ok(be24(&self.data, self.restart_off + 3 * idx)? as usize)
    }

    /// The key at restart point `idx`, which has no shared prefix.
    fn restart_key(&self, idx: usize) -> Result<&[u8]> {
        let off = self.restart_offset(idx)?;
        let records = self
            .data
            .get(off..self.restart_off)
            .ok_or_else(|| corrupt("restart point lies outside of the records"))?;
        let (prefix_len, suffix_len, _, consumed) = decode_key_len(records)?;
        if prefix_len != 0 {
            return Err(corrupt("restart point has a key with a shared prefix"));
        }
        take(records, consumed, suffix_len)
    }
}

/// An iterator over the records of one block.
#[derive(Debug, Clone)]
pub(crate) struct BlockIter {
    block: Block,
    next_off: usize,
    last_key: Vec<u8>,
    /// A record that was decoded while seeking and is returned first.
    peeked: Option<Record>,
}

impl BlockIter {
    pub(crate) fn new(block: Block) -> Self {
        Self {
            next_off: block.header_off + 4,
            block,
            last_key: Vec::new(),
            peeked: None,
        }
    }

    pub(crate) fn block(&self) -> &Block {
        &self.block
    }

    /// Position the iterator at the first record with a key that is not smaller than `want`.
    pub(crate) fn seek_key(&mut self, want: &[u8]) -> Result<()> {
        // Find the first restart point whose key exceeds `want`, the records we need start at the one before.
        let (mut low, mut high) = (0, self.block.restart_count);
        while low < high {
            let mid = low + (high - low) / 2;
            if self.block.restart_key(mid)? > want {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        self.next_off = if low > 0 {
            self.block.restart_offset(low - 1)?
        } else {
            self.block.header_off + 4
        };
        self.last_key.clear();
        self.peeked = None;

        while let Some(record) = self.decode_next()? {
            if self.last_key.as_slice() >= want {
                self.peeked = Some(record);
                break;
            }
        }
        Ok(())
    }

    /// Decode the next record, or return `None` at the end of the block.
    pub(crate) fn next_record(&mut self) -> Result<Option<Record>> {
        if let Some(record) = self.peeked.take() {
            return Ok(Some(record));
        }
        self.decode_next()
    }

    fn decode_next(&mut self) -> Result<Option<Record>> {
        if self.next_off >= self.block.restart_off {
            return Ok(None);
        }
        let input = &self.block.data[self.next_off..self.block.restart_off];
        let (key_len, extra) = decode_key(&mut self.last_key, input)?;
        if self.last_key.is_empty() {
            return Err(corrupt("record has an empty key"));
        }
        let (record, value_len) = Record::decode(
            self.block.block_type,
            &self.last_key,
            extra,
            &input[key_len..],
            self.block.hash_size,
        )?;
        self.next_off += key_len + value_len;
        Ok(Some(record))
    }
}
