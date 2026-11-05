//! Port of `verify_headers()` and `fsck_ident()` of Git 2.56's `fsck.c`.
use crate::{MessageId, check::Reporter};

/// Confirm that the headers of a commit or tag end in a reasonable way, either with the usual `\n\n`
/// separator or at least with a trailing newline on the final header line. Return `true` on failure.
pub(super) fn verify_headers(r: &mut Reporter<'_>, data: &[u8]) -> bool {
    for (offset, byte) in data.iter().enumerate() {
        match byte {
            0 => {
                return r.report(
                    MessageId::NulInHeader,
                    format_args!("unterminated header: NUL at offset {offset}"),
                );
            }
            b'\n' if data.get(offset + 1) == Some(&b'\n') => return false,
            _ => {}
        }
    }
    // Not having a body is not a crime, but the last header line needs its newline.
    if data.last() == Some(&b'\n') {
        return false;
    }
    r.report(MessageId::UnterminatedHeader, format_args!("unterminated header"))
}

/// Check the author, committer or tagger line that starts at `*pos`, and advance `*pos` past it.
/// Return `true` on failure, and stop at the first problem even if it isn't an error.
pub(super) fn check(r: &mut Reporter<'_>, data: &[u8], pos: &mut usize) -> bool {
    let end = data.len();
    let at = |i: usize| data.get(i).copied().unwrap_or(0);
    let mut p = *pos;
    // The headers were verified to have a newline, but this can be called after that failed to be reported.
    *pos = data[p.min(end)..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(end, |newline| p + newline + 1);

    if at(p) == b'<' {
        return r.report(
            MessageId::MissingNameBeforeEmail,
            format_args!("invalid author/committer line - missing space before email"),
        );
    }
    loop {
        if p >= end || at(p) == b'\n' {
            return r.report(
                MessageId::MissingEmail,
                format_args!("invalid author/committer line - missing email"),
            );
        }
        if at(p) == b'>' {
            return r.report(
                MessageId::BadName,
                format_args!("invalid author/committer line - bad name"),
            );
        }
        if at(p) == b'<' {
            break;
        }
        p += 1;
    }
    if p == 0 || at(p - 1) != b' ' {
        return r.report(
            MessageId::MissingSpaceBeforeEmail,
            format_args!("invalid author/committer line - missing space before email"),
        );
    }
    p += 1;
    loop {
        if p >= end || at(p) == b'<' || at(p) == b'\n' {
            return r.report(
                MessageId::BadEmail,
                format_args!("invalid author/committer line - bad email"),
            );
        }
        if at(p) == b'>' {
            break;
        }
        p += 1;
    }
    p += 1;
    if at(p) != b' ' {
        return r.report(
            MessageId::MissingSpaceBeforeDate,
            format_args!("invalid author/committer line - missing space before date"),
        );
    }
    p += 1;
    // Extra whitespace before the date has always been allowed, but never a newline.
    while at(p) == b' ' || at(p) == b'\t' {
        p += 1;
    }
    if !at(p).is_ascii_digit() {
        return r.report(
            MessageId::BadDate,
            format_args!("invalid author/committer line - bad date"),
        );
    }
    if at(p) == b'0' && at(p + 1) != b' ' {
        return r.report(
            MessageId::ZeroPaddedDate,
            format_args!("invalid author/committer line - zero-padded date"),
        );
    }
    let digits = data[p..end].iter().take_while(|b| b.is_ascii_digit()).count();
    if date_overflows(&data[p..p + digits]) {
        return r.report(
            MessageId::BadDateOverflow,
            format_args!("invalid author/committer line - date causes integer overflow"),
        );
    }
    p += digits;
    if at(p) != b' ' {
        return r.report(
            MessageId::BadDate,
            format_args!("invalid author/committer line - bad date"),
        );
    }
    p += 1;
    if !(at(p) == b'+' || at(p) == b'-')
        || !at(p + 1).is_ascii_digit()
        || !at(p + 2).is_ascii_digit()
        || !at(p + 3).is_ascii_digit()
        || !at(p + 4).is_ascii_digit()
        || at(p + 5) != b'\n'
    {
        return r.report(
            MessageId::BadTimezone,
            format_args!("invalid author/committer line - bad time zone"),
        );
    }
    false
}

/// Return `true` if the number in `digits` doesn't fit a signed 64 bit integer, which is what Git's `date_overflows()` tests
/// for timestamps. Git parses numbers of 23 digits and more as the largest value there is.
fn date_overflows(digits: &[u8]) -> bool {
    if digits.len() >= 23 {
        return true;
    }
    let value = digits.iter().fold(0u128, |acc, d| acc * 10 + u128::from(d - b'0'));
    value > i64::MAX as u128
}
