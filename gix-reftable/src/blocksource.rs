//! Where the bytes of a table come from.

use std::{path::Path, sync::Arc};

use gix_error::{Result, ResultExt, message};

/// The complete contents of one table file, shared cheaply between clones.
#[derive(Clone, Debug)]
pub struct BlockSource {
    data: Arc<[u8]>,
}

impl BlockSource {
    /// Read the file at `path` into memory.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let data = std::fs::read(path).or_raise(|| message!("could not read reftable {}", path.display()))?;
        Ok(Self::from_bytes(data))
    }

    /// Use `data`, which are the bytes of a complete table.
    pub fn from_bytes(data: Vec<u8>) -> Self {
        Self { data: data.into() }
    }

    /// The size of the table in bytes.
    pub fn size(&self) -> u64 {
        self.data.len() as u64
    }

    /// All bytes of the table.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Up to `size` bytes starting at `offset`, which is fewer at the end and empty past it.
    pub(crate) fn read(&self, offset: u64, size: usize) -> &[u8] {
        let Some(start) = usize::try_from(offset).ok().filter(|start| *start < self.data.len()) else {
            return &[];
        };
        &self.data[start..start.saturating_add(size).min(self.data.len())]
    }
}
