//! The `REUC` extension, which remembers what the entries of a conflict looked like before they were resolved, so that
//! `git checkout -m` and `git update-index --unresolve` can bring the conflict back.
use bstr::{BStr, BString};
use gix_hash::ObjectId;

use crate::{extension::Signature, util::split_at_byte_exclusive};

/// All paths whose conflict was resolved.
pub type Paths = Vec<ResolvePath>;

/// A path whose conflict was resolved, along with the entries it had before.
#[derive(Clone)]
pub struct ResolvePath {
    /// relative to the root of the repository, or what would be stored in the index
    name: BString,

    /// 0 = ancestor/common, 1 = ours, 2 = theirs
    stages: [Option<Stage>; 3],
}

impl ResolvePath {
    /// The path of the entry, relative to the root of the repository.
    pub fn name(&self) -> &BStr {
        self.name.as_ref()
    }

    /// The entries the conflict had, which are the common ancestor, ours and theirs in that order. Each is `None` if the
    /// side didn't have the path.
    pub fn stages(&self) -> &[Option<Stage>; 3] {
        &self.stages
    }
}

/// One side of a resolved conflict.
#[derive(Clone, Copy)]
pub struct Stage {
    mode: u32,
    id: ObjectId,
}

impl Stage {
    /// The mode the entry had, like `0o100644`.
    pub fn mode(&self) -> u32 {
        self.mode
    }

    /// The id of the object the entry pointed to, which has to be kept as long as the index remembers it.
    pub fn id(&self) -> &gix_hash::oid {
        &self.id
    }
}

pub(crate) const SIGNATURE: Signature = *b"REUC";

pub(crate) fn decode(mut data: &[u8], object_hash: gix_hash::Kind) -> Option<Paths> {
    let hash_len = object_hash.len_in_bytes();
    let mut out = Vec::new();

    while !data.is_empty() {
        let (path, rest) = split_at_byte_exclusive(data, 0)?;
        data = rest;

        let mut modes = [0u32; 3];
        for mode in &mut modes {
            let (mode_ascii, rest) = split_at_byte_exclusive(data, 0)?;
            data = rest;
            *mode = u32::from_str_radix(std::str::from_utf8(mode_ascii).ok()?, 8).ok()?;
        }

        let mut stages = [None, None, None];
        for (mode, stage) in modes.iter().zip(stages.iter_mut()) {
            if *mode == 0 {
                continue;
            }
            let (hash, rest) = data.split_at_checked(hash_len)?;
            data = rest;
            *stage = Some(Stage {
                mode: *mode,
                id: ObjectId::from_bytes_or_panic(hash),
            });
        }

        out.push(ResolvePath {
            name: path.into(),
            stages,
        });
    }
    out.into()
}
