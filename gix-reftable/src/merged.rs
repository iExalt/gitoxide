//! A view of a stack of tables as if it was a single table.

use std::{cmp::Reverse, collections::BinaryHeap};

use gix_error::Result;
use gix_hash::Kind;

use crate::{
    basics::misuse,
    record::{BlockType, Record},
    table::{Table, TableIter},
};

/// Several tables which are merged into one view, with newer tables taking precedence over older ones.
#[derive(Debug, Clone)]
pub struct MergedTable {
    tables: Vec<Table>,
    hash_kind: Kind,
    suppress_deletions: bool,
}

impl MergedTable {
    /// Merge `tables`, which are ordered from the oldest to the newest as they are in `tables.list`.
    ///
    /// Fails if the tables don't all use the same hash.
    pub fn new(tables: Vec<Table>) -> Result<Self> {
        let hash_kind = tables.first().map_or_else(Kind::default, Table::hash_kind);
        if tables.iter().any(|table| table.hash_kind() != hash_kind) {
            return Err(misuse("all tables of a stack must use the same hash"));
        }
        Ok(Self {
            tables,
            hash_kind,
            suppress_deletions: false,
        })
    }

    /// Hide tombstones when iterating. This is what readers of a stack want, while compaction needs to see them.
    pub fn with_deletions_suppressed(mut self, suppress: bool) -> Self {
        self.suppress_deletions = suppress;
        self
    }

    /// The tables of the view, from the oldest to the newest.
    pub fn tables(&self) -> &[Table] {
        &self.tables
    }

    /// The hash all tables use.
    pub fn hash_kind(&self) -> Kind {
        self.hash_kind
    }

    /// The lowest update index of all tables, or 0 if there are none.
    pub fn min_update_index(&self) -> u64 {
        self.tables.iter().map(Table::min_update_index).min().unwrap_or(0)
    }

    /// The highest update index of all tables, or 0 if there are none.
    pub fn max_update_index(&self) -> u64 {
        self.tables.iter().map(Table::max_update_index).max().unwrap_or(0)
    }

    /// Iterate the merged reference records.
    pub fn ref_iter(&self) -> Result<MergedIter<'_>> {
        MergedIter::new(self, BlockType::Ref)
    }

    /// Iterate the merged reflog records.
    pub fn log_iter(&self) -> Result<MergedIter<'_>> {
        MergedIter::new(self, BlockType::Log)
    }
}

/// An entry of the queue, ordered so that the smallest key pops first, and among equal keys the newest table.
#[derive(Debug)]
struct Head {
    key: Vec<u8>,
    table_idx: usize,
    record: Record,
}

impl Head {
    fn order(&self) -> (Reverse<&[u8]>, usize) {
        (Reverse(self.key.as_slice()), self.table_idx)
    }
}

impl PartialEq for Head {
    fn eq(&self, other: &Self) -> bool {
        self.order() == other.order()
    }
}

impl Eq for Head {}

impl PartialOrd for Head {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Head {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order().cmp(&other.order())
    }
}

/// An iterator over records of one kind of all tables of a [`MergedTable`].
pub struct MergedIter<'a> {
    subiters: Vec<TableIter<'a>>,
    heads: BinaryHeap<Head>,
    suppress_deletions: bool,
}

impl<'a> MergedIter<'a> {
    fn new(merged: &'a MergedTable, block_type: BlockType) -> Result<Self> {
        let subiters = merged
            .tables
            .iter()
            .map(|table| match block_type {
                BlockType::Log => table.log_iter(),
                _ => table.ref_iter(),
            })
            .collect::<Result<Vec<_>>>()?;
        let mut iter = Self {
            subiters,
            heads: BinaryHeap::new(),
            suppress_deletions: merged.suppress_deletions,
        };
        iter.refill()?;
        Ok(iter)
    }

    fn refill(&mut self) -> Result<()> {
        self.heads.clear();
        for idx in 0..self.subiters.len() {
            self.advance(idx)?;
        }
        Ok(())
    }

    fn advance(&mut self, table_idx: usize) -> Result<()> {
        if let Some(record) = self.subiters[table_idx].next_record()? {
            self.heads.push(Head {
                key: record.key(),
                table_idx,
                record,
            });
        }
        Ok(())
    }

    /// Position all tables at the first record whose key is not smaller than `want`.
    pub fn seek_key(&mut self, want: &[u8]) -> Result<()> {
        for iter in &mut self.subiters {
            iter.seek_key(want)?;
        }
        self.refill()
    }

    /// Return the next record, which for every key is the one of the newest table that has it.
    pub fn next_record(&mut self) -> Result<Option<Record>> {
        while let Some(head) = self.heads.pop() {
            self.advance(head.table_idx)?;
            while let Some(shadowed) = self.heads.peek().filter(|other| other.key == head.key) {
                let table_idx = shadowed.table_idx;
                self.heads.pop();
                self.advance(table_idx)?;
            }
            if self.suppress_deletions && head.record.is_deletion() {
                continue;
            }
            return Ok(Some(head.record));
        }
        Ok(None)
    }
}
