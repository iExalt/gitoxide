//! Port of `fsck_tree()` and `verify_ordered()` of Git 2.56's `fsck.c`, along with the decoding of tree entries of `tree-walk.c`.
use gix_hash::ObjectId;
use gix_object::Kind;
use gix_object::bstr::{BStr, ByteSlice};
use gix_validate::dotfile::{DotFile, is_hfs, is_ntfs};

use crate::{MessageId, check::Reporter, check::blob::Found};

const S_IFMT: u16 = 0o170000;
const S_IFREG: u16 = 0o100000;
const S_IFLNK: u16 = 0o120000;
const S_IFDIR: u16 = 0o040000;
const S_IFGITLINK: u16 = 0o160000;

/// An entry as `decode_tree_entry()` reads it.
#[derive(Clone, Copy)]
struct Entry<'a> {
    /// The mode as Git reads it, without canonicalizing it and truncated to 16 bits.
    mode: u16,
    name: &'a [u8],
    id: &'a [u8],
    /// The amount of bytes it takes in the tree.
    len: usize,
}

impl Entry<'_> {
    fn is_symlink(&self) -> bool {
        self.mode & S_IFMT == S_IFLNK
    }
}

/// Decode the entry at the start of `buf`, which has all bytes of the tree that haven't been consumed yet.
/// Fail with the message Git prints as error.
fn decode(buf: &[u8], hash_size: usize) -> Result<Entry<'_>, &'static str> {
    if buf.len() < hash_size + 3 || buf[buf.len() - (hash_size + 1)] != 0 {
        return Err("too-short tree object");
    }
    if buf[0] == b' ' {
        return Err("malformed mode in tree entry");
    }
    let mut mode = 0u32;
    let mut name_start = None;
    for (idx, byte) in buf.iter().enumerate() {
        match byte {
            b' ' => {
                name_start = Some(idx + 1);
                break;
            }
            b'0'..=b'7' => mode = (mode << 3).wrapping_add(u32::from(byte - b'0')),
            _ => break,
        }
    }
    let Some(name_start) = name_start else {
        return Err("malformed mode in tree entry");
    };
    let name_len = buf[name_start..]
        .iter()
        .position(|b| *b == 0)
        .ok_or("too-short tree object")?;
    if name_len == 0 {
        return Err("empty filename in tree entry");
    }
    let id_start = name_start + name_len + 1;
    let id = buf.get(id_start..id_start + hash_size).ok_or("too-short tree object")?;
    Ok(Entry {
        mode: mode as u16,
        name: &buf[name_start..name_start + name_len],
        id,
        len: id_start + hash_size,
    })
}

/// Check the tree `data` and report what is wrong with it, and return `true` if one of the reports is an error.
pub(super) fn check(r: &mut Reporter<'_>, data: &[u8], hash_kind: gix_hash::Kind, found: &mut Found) -> bool {
    let hash_size = hash_kind.len_in_bytes();
    let strict = r.options.is_strict();
    let max_name_len = r.options.max_tree_entry_len();
    let mut failed = false;
    let (mut has_null_sha1, mut has_full_path, mut has_empty_name, mut has_dot, mut has_dotdot) =
        (false, false, false, false, false);
    let (mut has_dotgit, mut has_zero_pad, mut has_bad_modes, mut has_dup_entries) = (false, false, false, false);
    let (mut not_properly_sorted, mut has_large_name) = (false, false);

    let mut current = if data.is_empty() {
        None
    } else {
        match decode(data, hash_size) {
            Ok(entry) => Some(entry),
            Err(message) => {
                r.note(message);
                failed |= r.report(MessageId::BadTree, format_args!("cannot be parsed as a tree"));
                return failed;
            }
        }
    };
    let mut previous: Option<(u16, &[u8])> = None;
    let mut candidates: Vec<&[u8]> = Vec::new();
    let mut pos = 0;
    while let Some(entry) = current {
        let name = entry.name;
        let name_bstr: &BStr = name.as_bstr();
        has_null_sha1 |= entry.id.iter().all(|b| *b == 0);
        has_full_path |= name.contains(&b'/');
        has_empty_name |= name.is_empty();
        has_dot |= name == b".";
        has_dotdot |= name == b"..";
        has_dotgit |= is_hfs(DotFile::Git, name_bstr) || is_ntfs(DotFile::Git, name_bstr);
        has_zero_pad |= data[pos] == b'0';
        has_large_name |= isize::try_from(name.len()).map_or(true, |len| len > max_name_len);

        if is_hfs(DotFile::Gitmodules, name_bstr) || is_ntfs(DotFile::Gitmodules, name_bstr) {
            if !entry.is_symlink() {
                found.insert_gitmodules(entry.id, hash_kind);
            } else {
                failed |= r.report(
                    MessageId::GitmodulesSymlink,
                    format_args!(".gitmodules is a symbolic link"),
                );
            }
        }
        if is_hfs(DotFile::Gitattributes, name_bstr) || is_ntfs(DotFile::Gitattributes, name_bstr) {
            if !entry.is_symlink() {
                found.insert_attributes(entry.id, hash_kind);
            } else {
                failed |= r.report(
                    MessageId::GitattributesSymlink,
                    format_args!(".gitattributes is a symlink"),
                );
            }
        }
        if entry.is_symlink() {
            if is_hfs(DotFile::Gitignore, name_bstr) || is_ntfs(DotFile::Gitignore, name_bstr) {
                failed |= r.report(MessageId::GitignoreSymlink, format_args!(".gitignore is a symlink"));
            }
            if is_hfs(DotFile::Mailmap, name_bstr) || is_ntfs(DotFile::Mailmap, name_bstr) {
                failed |= r.report(MessageId::MailmapSymlink, format_args!(".mailmap is a symlink"));
            }
        }
        // Git also looks at what follows a backslash as these are separators for NTFS.
        let mut rest = name;
        while let Some(backslash) = rest.iter().position(|b| *b == b'\\') {
            rest = &rest[backslash + 1..];
            let suffix: &BStr = rest.as_bstr();
            has_dotgit |= is_ntfs(DotFile::Git, suffix);
            if is_ntfs(DotFile::Gitmodules, suffix) {
                if !entry.is_symlink() {
                    found.insert_gitmodules(entry.id, hash_kind);
                } else {
                    failed |= r.report(
                        MessageId::GitmodulesSymlink,
                        format_args!(".gitmodules is a symbolic link"),
                    );
                }
            }
        }

        let next_pos = pos + entry.len;
        current = if next_pos < data.len() {
            match decode(&data[next_pos..], hash_size) {
                Ok(next) => Some(next),
                Err(message) => {
                    r.note(message);
                    failed |= r.report(MessageId::BadTree, format_args!("cannot be parsed as a tree"));
                    break;
                }
            }
        } else {
            None
        };

        match entry.mode {
            0o100755 | 0o100644 | S_IFLNK | S_IFDIR | S_IFGITLINK => {}
            // Nonstandard, but there were some of them early on when Git honored all mode bits.
            0o100664 if !strict => {}
            _ => has_bad_modes = true,
        }
        if let Some((previous_mode, previous_name)) = previous {
            match verify_ordered(previous_mode, previous_name, entry.mode, name, &mut candidates) {
                Ordering::Ordered => {}
                Ordering::Unordered => not_properly_sorted = true,
                Ordering::HasDuplicates => has_dup_entries = true,
            }
        }
        previous = Some((entry.mode, name));
        pos = next_pos;
    }

    let mut report = |flag: bool, id: MessageId, message: &str| {
        if flag {
            failed |= r.report(id, format_args!("{message}"));
        }
    };
    report(
        has_null_sha1,
        MessageId::NullSha1,
        "contains entries pointing to null sha1",
    );
    report(has_full_path, MessageId::FullPathname, "contains full pathnames");
    report(has_empty_name, MessageId::EmptyName, "contains empty pathname");
    report(has_dot, MessageId::HasDot, "contains '.'");
    report(has_dotdot, MessageId::HasDotdot, "contains '..'");
    report(has_dotgit, MessageId::HasDotgit, "contains '.git'");
    report(
        has_zero_pad,
        MessageId::ZeroPaddedFilemode,
        "contains zero-padded file modes",
    );
    report(has_bad_modes, MessageId::BadFilemode, "contains bad file modes");
    report(
        has_dup_entries,
        MessageId::DuplicateEntries,
        "contains duplicate file entries",
    );
    report(not_properly_sorted, MessageId::TreeNotSorted, "not properly sorted");
    report(
        has_large_name,
        MessageId::LargePathname,
        "contains excessively large pathname",
    );
    failed
}

/// The objects the tree `data` links to, with the kind they must have, as Git's `fsck_walk_tree()` sees them.
/// `None` if the tree can't be walked at all, which is the case if its first entry can't be decoded.
///
/// Like Git, stop at the first entry that can't be decoded and don't return the entry before it either.
pub(super) fn links(data: &[u8], hash_kind: gix_hash::Kind) -> Option<Vec<(ObjectId, Kind)>> {
    let hash_size = hash_kind.len_in_bytes();
    let mut out = Vec::new();
    if data.is_empty() {
        return Some(out);
    }
    let mut current = decode(data, hash_size).ok()?;
    let mut pos = 0;
    loop {
        let next_pos = pos + current.len;
        let next = if next_pos < data.len() {
            match decode(&data[next_pos..], hash_size) {
                Ok(next) => Some(next),
                Err(_) => break,
            }
        } else {
            None
        };
        // Everything that isn't a regular file, symlink or directory is a submodule once the mode is canonicalized.
        let kind = match current.mode & S_IFMT {
            S_IFDIR => Some(Kind::Tree),
            S_IFREG | S_IFLNK => Some(Kind::Blob),
            _ => None,
        };
        if let Some(kind) = kind {
            out.push((ObjectId::try_from(current.id).ok()?, kind));
        }
        match next {
            Some(next) => {
                current = next;
                pos = next_pos;
            }
            None => break,
        }
    }
    Some(out)
}

enum Ordering {
    Ordered,
    Unordered,
    HasDuplicates,
}

fn is_less_than_slash(c: u8) -> bool {
    0 < c && c < b'/'
}

/// Return how the entries `(mode1, name1)` and `(mode2, name2)`, which follow each other, relate to each other.
/// `candidates` is the stack for the detection of duplicates which aren't next to each other, like `foo` and `foo/`.
fn verify_ordered<'a>(
    mode1: u16,
    name1: &'a [u8],
    mode2: u16,
    name2: &'a [u8],
    candidates: &mut Vec<&'a [u8]>,
) -> Ordering {
    let len = name1.len().min(name2.len());
    match name1[..len].cmp(&name2[..len]) {
        std::cmp::Ordering::Less => return Ordering::Ordered,
        std::cmp::Ordering::Greater => return Ordering::Unordered,
        std::cmp::Ordering::Equal => {}
    }
    // The first `len` characters are the same. Order the next one, with a directory ending in a slash.
    let mut c1 = name1.get(len).copied().unwrap_or(0);
    let mut c2 = name2.get(len).copied().unwrap_or(0);
    if c1 == 0 && c2 == 0 {
        // `git write-tree` used to write entries with the same name, one blob and one tree.
        return Ordering::HasDuplicates;
    }
    if c1 == 0 && mode1 & S_IFMT == S_IFDIR {
        c1 = b'/';
    }
    if c2 == 0 && mode2 & S_IFMT == S_IFDIR {
        c2 = b'/';
    }
    // There can be non-consecutive duplicates due to the implicitly added slash, like in
    // `foo`, `foo.bar`, `foo.bar.baz`, `foo.bar/` and `foo/`. Record non-directory candidates on a stack
    // and check directory candidates against it.
    if c1 == 0 && is_less_than_slash(c2) {
        candidates.push(name1);
    } else if c2 == b'/' && is_less_than_slash(c1) {
        while let Some(candidate) = candidates.pop() {
            let Some(rest) = name2.strip_prefix(candidate) else {
                continue;
            };
            match rest.first() {
                None => return Ordering::HasDuplicates,
                Some(c) if is_less_than_slash(*c) => {
                    candidates.push(candidate);
                    break;
                }
                Some(_) => {}
            }
        }
    }
    if c1 < c2 {
        Ordering::Ordered
    } else {
        Ordering::Unordered
    }
}
