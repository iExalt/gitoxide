//! Port of `fsck_commit()` of Git 2.56's `fsck.c`, and the parsing of `parse_commit_buffer()` that precedes it during transfers.
use gix_hash::{ObjectId, oid};
use gix_object::Kind;

use crate::{
    MessageId,
    check::{
        Refusal, Reporter,
        ident::{self, verify_headers},
    },
};

/// The parts of a commit that matter for the links of objects.
pub(super) struct Parsed {
    pub tree: ObjectId,
    pub parents: Vec<ObjectId>,
}

/// Parse a commit like `parse_commit_buffer()` does, which transfers do before checking it, and fail with
/// what Git says if the commit isn't well-formed enough.
pub(super) fn parse(id: &oid, data: &[u8], hash_kind: gix_hash::Kind) -> Result<Parsed, Refusal> {
    let hex = hash_kind.len_in_hex();
    let unparsable = |detail: String| Refusal::Unparsable {
        oid: id.to_owned(),
        kind: Kind::Commit,
        detail: Some(detail),
    };
    let tree_entry_len = hex + 5;
    let parent_entry_len = hex + 7;
    if data.len() <= tree_entry_len + 1 || !data.starts_with(b"tree ") || data[tree_entry_len] != b'\n' {
        return Err(unparsable(format!("bogus commit object {id}")));
    }
    let Some(tree) = ObjectId::from_hex(&data[5..5 + hex]).ok() else {
        return Err(unparsable(format!("bad tree pointer in commit {id}")));
    };
    let mut pos = tree_entry_len + 1;
    let mut parents = Vec::new();
    while pos + parent_entry_len < data.len() && data[pos..].starts_with(b"parent ") {
        let parent = if data.len() <= pos + parent_entry_len + 1 || data[pos + parent_entry_len] != b'\n' {
            None
        } else {
            ObjectId::from_hex(&data[pos + 7..pos + 7 + hex]).ok()
        };
        let Some(parent) = parent else {
            return Err(unparsable(format!("bad parents in commit {id}")));
        };
        parents.push(parent);
        pos += parent_entry_len + 1;
    }
    Ok(Parsed { tree, parents })
}

/// Check the commit `data` like `fsck_commit()` does, which stops at the first problem that is reported as error. Return `true` on failure.
///
/// If a hex id has the wrong format but reporting it isn't an error, Git continues at whatever its pointer happens to be, while we
/// continue at the next line.
pub(super) fn check(r: &mut Reporter<'_>, data: &[u8], hash_kind: gix_hash::Kind) -> bool {
    // Everything that follows only reports, so there is nothing to do for objects that are on the skip list.
    if r.is_skipped() {
        return false;
    }
    if verify_headers(r, data) {
        return true;
    }
    let hex = hash_kind.len_in_hex();
    let mut pos = 0;
    let Some(next) = skip_prefix(data, pos, b"tree ") else {
        return r.report(
            MessageId::MissingTree,
            format_args!("invalid format - expected 'tree' line"),
        );
    };
    pos = next;
    if let Some(next) = hex_line(data, pos, hex) {
        pos = next;
    } else {
        if r.report(
            MessageId::BadTreeSha1,
            format_args!("invalid 'tree' line format - bad sha1"),
        ) {
            return true;
        }
        pos = next_line(data, pos);
    }
    while let Some(next) = skip_prefix(data, pos, b"parent ") {
        pos = next;
        if let Some(next) = hex_line(data, pos, hex) {
            pos = next;
        } else {
            if r.report(
                MessageId::BadParentSha1,
                format_args!("invalid 'parent' line format - bad sha1"),
            ) {
                return true;
            }
            pos = next_line(data, pos);
        }
    }
    let mut author_count = 0;
    while let Some(next) = skip_prefix(data, pos, b"author ") {
        pos = next;
        author_count += 1;
        if ident::check(r, data, &mut pos) {
            return true;
        }
    }
    let failed = match author_count.cmp(&1) {
        std::cmp::Ordering::Less => r.report(
            MessageId::MissingAuthor,
            format_args!("invalid format - expected 'author' line"),
        ),
        std::cmp::Ordering::Greater => r.report(
            MessageId::MultipleAuthors,
            format_args!("invalid format - multiple 'author' lines"),
        ),
        std::cmp::Ordering::Equal => false,
    };
    if failed {
        return true;
    }
    let Some(next) = skip_prefix(data, pos, b"committer ") else {
        return r.report(
            MessageId::MissingCommitter,
            format_args!("invalid format - expected 'committer' line"),
        );
    };
    pos = next;
    if ident::check(r, data, &mut pos) {
        return true;
    }
    if data.contains(&0)
        && r.report(
            MessageId::NulInCommit,
            format_args!("NUL byte in the commit object body"),
        )
    {
        return true;
    }
    false
}

/// Return the position after `prefix` if `data` has it at `pos`, and `pos` is inside `data`.
pub(super) fn skip_prefix(data: &[u8], pos: usize, prefix: &[u8]) -> Option<usize> {
    if pos >= data.len() {
        return None;
    }
    data[pos..].starts_with(prefix).then(|| pos + prefix.len())
}

/// Return the position after a hex id and the newline that follows it, if `data` has these at `pos`.
fn hex_line(data: &[u8], pos: usize, hex: usize) -> Option<usize> {
    let id = data.get(pos..pos + hex)?;
    (id.iter().all(u8::is_ascii_hexdigit) && data.get(pos + hex) == Some(&b'\n')).then_some(pos + hex + 1)
}

/// The position after the next newline at or after `pos`, or the end of `data`.
pub(super) fn next_line(data: &[u8], pos: usize) -> usize {
    data.get(pos..)
        .and_then(|rest| rest.iter().position(|b| *b == b'\n'))
        .map_or(data.len(), |newline| pos + newline + 1)
}
