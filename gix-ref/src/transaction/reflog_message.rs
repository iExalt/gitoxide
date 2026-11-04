//! How a message is written to a reflog, which doesn't depend on how references are stored.
use gix_object::bstr::{BStr, BString};

/// Return `message` the way Git writes it to a reflog: leading whitespace is dropped, every run of whitespace,
/// including newlines, becomes one space, and trailing whitespace is dropped. A reflog has one line per entry, which
/// a newline in a message would break.
///
/// Whitespace is what Git's own `isspace()` says, which is only space, `\t`, `\n` and `\r`: vertical tabs and form
/// feeds are kept, as are other bytes that are spaces in Unicode.
pub(crate) fn normalize_reflog_message(message: &BStr) -> BString {
    let is_space = |byte: u8| matches!(byte, b' ' | b'\t' | b'\n' | b'\r');
    let mut out = BString::default();
    let mut was_space = true;
    for byte in message.iter().copied() {
        if was_space && is_space(byte) {
            continue;
        }
        was_space = is_space(byte);
        out.push(if was_space { b' ' } else { byte });
    }
    while out.last().is_some_and(|byte| is_space(*byte)) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(message: &str) -> BString {
        normalize_reflog_message(message.into())
    }

    #[test]
    fn whitespace_is_collapsed_and_trimmed() {
        assert_eq!(normalized("  a \t b\n\nc  "), "a b c");
        assert_eq!(normalized("a\r\nb"), "a b");
        assert_eq!(
            normalized("a\u{b}\u{c}b"),
            "a\u{b}\u{c}b",
            "Git doesn't count vertical tabs or form feeds"
        );
        assert_eq!(normalized(" \t\n "), "", "nothing is left of whitespace");
        assert_eq!(normalized(""), "");
    }

    #[test]
    fn everything_else_is_kept_byte_for_byte() {
        assert_eq!(
            normalized("a\u{a0}b  é"),
            "a\u{a0}b é",
            "no-break spaces aren't spaces in the C locale"
        );
        assert_eq!(
            normalize_reflog_message(b"\xff \xfe".as_slice().into()),
            b"\xff \xfe".as_slice(),
            "messages aren't necessarily UTF-8"
        );
    }
}
