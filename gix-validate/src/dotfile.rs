//! Detect the names of files that Git treats specially, even if a file system would spell them differently.
//!
//! Case-insensitive file systems and the ones that normalize names, like HFS+ and NTFS, give a single file many names.
//! A tree that contains `.GIT`, `git~1` or `.git ` (with trailing space) would overwrite or write into `.git`, which
//! makes them dangerous to check out. Git's `fsck` detects them in every tree, regardless of the file system it runs on,
//! and so do these functions, as opposed to [`path::component()`](crate::path::component()), which does so only if asked to
//! and treats more than these names specially.
//!
//! The functions are ports of `is_hfs_dot*()` in Git's `utf8.c` and `is_ntfs_dot*()` in `path.c`.
use bstr::{BStr, ByteSlice};

/// A file with special meaning to Git, which is also known by variations of its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DotFile {
    /// `.git`, the repository itself.
    Git,
    /// `.gitmodules`, which configures submodules.
    Gitmodules,
    /// `.gitattributes`, which configures how paths are treated.
    Gitattributes,
    /// `.gitignore`, which lists what to ignore.
    Gitignore,
    /// `.mailmap`, which maps author names and addresses.
    Mailmap,
}

impl DotFile {
    fn needle(self) -> &'static [u8] {
        match self {
            DotFile::Git => b"git",
            DotFile::Gitmodules => b"gitmodules",
            DotFile::Gitattributes => b"gitattributes",
            DotFile::Gitignore => b"gitignore",
            DotFile::Mailmap => b"mailmap",
        }
    }

    /// The first six characters of the fall-back short name NTFS gives to files whose short names collide.
    fn ntfs_fallback_prefix(self) -> &'static [u8] {
        match self {
            DotFile::Git => b"",
            DotFile::Gitmodules => b"gi7eba",
            DotFile::Gitattributes => b"gi7d29",
            DotFile::Gitignore => b"gi250a",
            DotFile::Mailmap => b"maba30",
        }
    }
}

/// Return `true` if `name` is `file` or what HFS+ makes the same, which are spellings that contain code points
/// that HFS+ ignores, like a zero-width joiner. Only the case folding that matters for these names is applied.
///
/// `name` is a single path component, but a directory separator (`/`) ends it. Only `/` does, as is the case for Git on Linux,
/// while Git for Windows has `\` as well.
///
/// Malformed UTF-8 ends the name like the end of the string does, which makes `.git` followed by an invalid byte a match,
/// just like it is for Git, while it can't be part of what is matched.
pub fn is_hfs(file: DotFile, name: &BStr) -> bool {
    let mut rest = name.as_bytes();
    if next_hfs_char(&mut rest) != u32::from(b'.') {
        return false;
    }
    for expected in file.needle() {
        let c = next_hfs_char(&mut rest);
        if c >= 128 || (c as u8).to_ascii_lowercase() != *expected {
            return false;
        }
    }
    matches!(next_hfs_char(&mut rest), 0 | 0x2f)
}

/// Return `true` if `name` is `file` or what NTFS makes the same: a different case, a trailing run of spaces and dots, an
/// alternate data stream (`:…`), or one of the short names NTFS assigns, like `git~1` or `gitmod~1`.
///
/// `name` is a single path component. Unlike for [`DotFile::Git`], a separator doesn't end it.
/// For [`DotFile::Git`], both `/` and `\` end the name as `git fsck` has to work on every platform.
pub fn is_ntfs(file: DotFile, name: &BStr) -> bool {
    let name = name.as_bytes();
    let at = |i: usize| name.get(i).copied().unwrap_or(0);
    if file == DotFile::Git {
        return is_ntfs_dotgit(name);
    }
    let needle = file.needle();
    let only_spaces_and_periods = |mut i: usize| loop {
        let c = at(i);
        i += 1;
        if c == 0 || c == b':' {
            return true;
        }
        if c != b' ' && c != b'.' {
            return false;
        }
    };

    if at(0) == b'.'
        && name
            .get(1..=needle.len())
            .is_some_and(|n| n.eq_ignore_ascii_case(needle))
    {
        return only_spaces_and_periods(needle.len() + 1);
    }
    // A regular short name has six characters followed by `~1` to `~4`.
    if name.get(..6).is_some_and(|n| n.eq_ignore_ascii_case(&needle[..6]))
        && at(6) == b'~'
        && (b'1'..=b'4').contains(&at(7))
    {
        return only_spaces_and_periods(8);
    }
    // The fall-back short name, which has a number that takes at most the rest of the eight characters.
    let prefix = file.ntfs_fallback_prefix();
    let (mut i, mut saw_tilde) = (0, false);
    while i < 8 {
        let c = at(i);
        if c == 0 {
            return false;
        } else if saw_tilde {
            if !c.is_ascii_digit() {
                return false;
            }
        } else if c == b'~' {
            i += 1;
            if !(b'1'..=b'9').contains(&at(i)) {
                return false;
            }
            saw_tilde = true;
        } else if i >= 6 || c & 0x80 != 0 || c.to_ascii_lowercase() != prefix[i] {
            return false;
        }
        i += 1;
    }
    only_spaces_and_periods(i)
}

fn is_ntfs_dotgit(name: &[u8]) -> bool {
    let mut bytes = name.iter().copied().chain(std::iter::repeat(0));
    let mut next = move || bytes.next().unwrap_or(0);
    match next() {
        b'.' => {
            if !next().eq_ignore_ascii_case(&b'g')
                || !next().eq_ignore_ascii_case(&b'i')
                || !next().eq_ignore_ascii_case(&b't')
            {
                return false;
            }
        }
        b'g' | b'G' => {
            if !next().eq_ignore_ascii_case(&b'i')
                || !next().eq_ignore_ascii_case(&b't')
                || next() != b'~'
                || next() != b'1'
            {
                return false;
            }
        }
        _ => return false,
    }
    loop {
        match next() {
            0 | b'/' | b'\\' | b':' => return true,
            b'.' | b' ' => {}
            _ => return false,
        }
    }
}

/// Decode one character like Git's `next_hfs_char()`, skipping the code points HFS+ ignores.
/// Return `0` for malformed UTF-8 and at the end of `input`, which is what Git does as that is good enough for it
/// to realize that it can't be what is looked for, or that it's the end of it.
fn next_hfs_char(input: &mut &[u8]) -> u32 {
    loop {
        let Some((c, len)) = pick_one_utf8_char(input) else {
            return 0;
        };
        // Past the end the character is the terminating NUL, which is not to be consumed.
        *input = input.get(len..).unwrap_or_default();
        if matches!(
            c,
            0x200c..=0x200f | 0x202a..=0x202e | 0x206a..=0x206f | 0xfeff
        ) {
            continue;
        }
        return c;
    }
}

/// Decode a character the way `pick_one_utf8_char()` does, including its rejection of overlong forms, surrogates,
/// `U+FFFE` and `U+FFFF` and values above `U+10FFFF`. The end of `input` is the NUL of the C string.
fn pick_one_utf8_char(input: &[u8]) -> Option<(u32, usize)> {
    let at = |i: usize| input.get(i).copied().unwrap_or(0);
    let s0 = at(0);
    let (s1, s2, s3) = (at(1), at(2), at(3));
    let cont = |b: u8| b & 0xc0 == 0x80;
    Some(if s0 < 0x80 {
        (u32::from(s0), 1)
    } else if s0 & 0xe0 == 0xc0 {
        if !cont(s1) || s0 & 0xfe == 0xc0 {
            return None;
        }
        ((u32::from(s0 & 0x1f) << 6) | u32::from(s1 & 0x3f), 2)
    } else if s0 & 0xf0 == 0xe0 {
        if !cont(s1)
            || !cont(s2)
            || (s0 == 0xe0 && s1 & 0xe0 == 0x80)
            || (s0 == 0xed && s1 & 0xe0 == 0xa0)
            || (s0 == 0xef && s1 == 0xbf && s2 & 0xfe == 0xbe)
        {
            return None;
        }
        (
            (u32::from(s0 & 0x0f) << 12) | (u32::from(s1 & 0x3f) << 6) | u32::from(s2 & 0x3f),
            3,
        )
    } else if s0 & 0xf8 == 0xf0 {
        if !cont(s1)
            || !cont(s2)
            || !cont(s3)
            || (s0 == 0xf0 && s1 & 0xf0 == 0x80)
            || (s0 == 0xf4 && s1 > 0x8f)
            || s0 > 0xf4
        {
            return None;
        }
        (
            (u32::from(s0 & 0x07) << 18)
                | (u32::from(s1 & 0x3f) << 12)
                | (u32::from(s2 & 0x3f) << 6)
                | u32::from(s3 & 0x3f),
            4,
        )
    } else {
        return None;
    })
}
