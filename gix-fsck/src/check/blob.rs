//! Port of `fsck_blob()`, `fsck_blobs()` and `fsck_finish()` of Git 2.56's `fsck.c`: blobs that are named `.gitmodules` and
//! `.gitattributes` by a tree are checked, wherever their content is available.
use gix_hash::{ObjectId, oid};
use gix_hashtable::HashSet;
use gix_object::Kind;

use crate::{
    MessageId, Options,
    check::{Diagnostic, Reporter},
};

/// The size of `.gitattributes` files above which they are reported as too large, which is Git's `ATTR_MAX_FILE_SIZE`.
const ATTR_MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;
/// The length of lines in `.gitattributes` files from which they are reported as too long, which is Git's `ATTR_MAX_LINE_LENGTH`.
const ATTR_MAX_LINE_LENGTH: usize = 2048;

/// The blobs that trees say are special files, which are `done` if they have been checked.
#[derive(Debug, Default)]
struct Set {
    members: HashSet,
    /// The members in the order they were found.
    order: Vec<ObjectId>,
    done: HashSet,
}

impl Set {
    fn insert(&mut self, id: ObjectId) {
        if self.members.insert(id) {
            self.order.push(id);
        }
    }

    fn contains(&self, id: &oid) -> bool {
        self.members.contains(id)
    }

    fn is_pending(&self, id: &oid) -> bool {
        self.members.contains(id) && !self.done.contains(id)
    }
}

#[derive(Debug, Default)]
pub(super) struct Found {
    gitmodules: Set,
    attributes: Set,
}

impl Found {
    pub(super) fn insert_gitmodules(&mut self, id: &[u8], hash_kind: gix_hash::Kind) {
        if let Some(id) = object_id(id, hash_kind) {
            self.gitmodules.insert(id);
        }
    }

    pub(super) fn insert_attributes(&mut self, id: &[u8], hash_kind: gix_hash::Kind) {
        if let Some(id) = object_id(id, hash_kind) {
            self.attributes.insert(id);
        }
    }

    pub(super) fn needs_content(&self, options: &Options, id: &oid, size: u64) -> bool {
        (self.gitmodules.is_pending(id) && size <= options.big_file_threshold())
            || (self.attributes.is_pending(id) && size <= ATTR_MAX_FILE_SIZE.min(options.big_file_threshold()))
    }

    /// Check the blob `id` if it is known to be special. Return `true` if the check failed.
    pub(super) fn check(
        &mut self,
        options: &Options,
        id: &oid,
        size: u64,
        data: Option<&[u8]>,
        out: &mut Vec<Diagnostic>,
    ) -> bool {
        if options.is_skipped(id) {
            return false;
        }
        let mut r = Reporter::new(options, id, Kind::Blob, out);
        let mut failed = false;
        if self.gitmodules.contains(id) {
            self.gitmodules.done.insert(id.to_owned());
            let Some(data) = data else {
                return r.report(
                    MessageId::GitmodulesLarge,
                    format_args!(".gitmodules too large to parse"),
                );
            };
            failed |= gitmodules(&mut r, data);
        }
        if self.attributes.contains(id) {
            self.attributes.done.insert(id.to_owned());
            let Some(data) = data.filter(|_| size <= ATTR_MAX_FILE_SIZE) else {
                return r.report(
                    MessageId::GitattributesLarge,
                    format_args!(".gitattributes too large to parse"),
                );
            };
            failed |= attributes(&mut r, data);
        }
        failed
    }

    /// Check the special blobs that haven't been seen, and return `true` if one of them failed.
    pub(super) fn finish<F>(&mut self, options: &Options, find: &F, out: &mut Vec<Diagnostic>) -> bool
    where
        F: gix_object::Find + gix_object::FindHeader + ?Sized,
    {
        let mut failed = false;
        let mut buf = Vec::new();
        for (missing, not_a_blob, name, attributes_only) in [
            (
                MessageId::GitmodulesMissing,
                MessageId::GitmodulesBlob,
                ".gitmodules",
                false,
            ),
            (
                MessageId::GitattributesMissing,
                MessageId::GitattributesBlob,
                ".gitattributes",
                true,
            ),
        ] {
            let set = if attributes_only {
                &self.attributes
            } else {
                &self.gitmodules
            };
            let pending: Vec<_> = set.order.iter().filter(|id| !set.done.contains(*id)).copied().collect();
            for id in pending {
                let header = find.try_header(&id).ok().flatten();
                let Some(header) = header else {
                    failed |= Reporter::new(options, &id, Kind::Blob, out)
                        .report(missing, format_args!("unable to read {name} blob"));
                    continue;
                };
                if header.kind != Kind::Blob {
                    failed |= Reporter::new(options, &id, header.kind, out)
                        .report(not_a_blob, format_args!("non-blob found at {name}"));
                    continue;
                }
                // Git reads the object in full, even if it is larger than the threshold for big files.
                let only_too_large_attributes =
                    attributes_only && !self.gitmodules.contains(&id) && header.size > ATTR_MAX_FILE_SIZE;
                let data = if only_too_large_attributes {
                    None
                } else {
                    match find.try_find(&id, &mut buf).ok().flatten() {
                        Some(object) => Some(object.data),
                        None => {
                            failed |= Reporter::new(options, &id, Kind::Blob, out)
                                .report(missing, format_args!("unable to read {name} blob"));
                            continue;
                        }
                    }
                };
                failed |= self.check(options, &id, header.size, data, out);
            }
        }
        *self = Found::default();
        failed
    }
}

fn object_id(id: &[u8], hash_kind: gix_hash::Kind) -> Option<ObjectId> {
    ObjectId::try_from(id).ok().filter(|id| id.kind() == hash_kind)
}

/// Check the content of a `.gitmodules` file. Return `true` on failure.
fn gitmodules(_r: &mut Reporter<'_>, _data: &[u8]) -> bool {
    false
}

/// Check the content of a `.gitattributes` file for lines that are too long. Return `true` on failure.
fn attributes(r: &mut Reporter<'_>, data: &[u8]) -> bool {
    // Git reads a C string, so a NUL byte ends the file.
    let data = data.split(|b| *b == 0).next().unwrap_or_default();
    for line in data.split(|b| *b == b'\n') {
        if line.len() >= ATTR_MAX_LINE_LENGTH {
            return r.report(
                MessageId::GitattributesLineLength,
                format_args!(".gitattributes has too long lines to parse"),
            );
        }
    }
    false
}
