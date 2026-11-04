//! References that aren't to be shown or updated by a receiver, which `receive.hideRefs` and `transfer.hideRefs` configure.
use gix_object::bstr::{BStr, BString, ByteSlice};

/// The patterns of `transfer.hideRefs` and `receive.hideRefs` in the order in which they were configured.
///
/// A pattern hides the reference of that name and all the references below it. The last pattern that matches decides, and a
/// pattern that starts with `!` shows what it matches. A pattern that starts with `^` matches the name with the namespace,
/// which isn't a difference as long as namespaces aren't supported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HiddenRefs {
    patterns: Vec<BString>,
}

impl HiddenRefs {
    /// Read the patterns for `section`, which is `receive` for a receiver, from `config`, in the order in which Git sees them:
    /// `transfer.hideRefs` and `<section>.hideRefs` are one list.
    pub fn from_config(config: &gix_config::File, section: &str) -> Self {
        let mut patterns = Vec::new();
        for candidate in config.sections() {
            let header = candidate.header();
            let name = header.name();
            if header.subsection_name().is_some()
                || !(name.eq_ignore_ascii_case(b"transfer") || name.eq_ignore_ascii_case(section.as_bytes()))
            {
                continue;
            }
            for value in candidate.body().values("hideRefs") {
                let mut pattern: BString = value;
                // Git strips trailing slashes.
                while pattern.last() == Some(&b'/') {
                    pattern.pop();
                }
                patterns.push(pattern);
            }
        }
        HiddenRefs { patterns }
    }

    /// Create the patterns from their text, as they would be configured one after the other.
    pub fn from_patterns(patterns: impl IntoIterator<Item = impl Into<BString>>) -> Self {
        HiddenRefs {
            patterns: patterns.into_iter().map(Into::into).collect(),
        }
    }

    /// Return `true` if no reference is hidden.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Return `true` if the reference named `name`, which is the full name like `refs/heads/main`, is hidden.
    pub fn is_hidden(&self, name: &BStr) -> bool {
        for pattern in self.patterns.iter().rev() {
            let mut pattern = pattern.as_bstr();
            let negated = pattern.first() == Some(&b'!');
            if negated {
                pattern = &pattern[1..];
            }
            // Without namespaces, the name with and without them is the same.
            if pattern.first() == Some(&b'^') {
                pattern = &pattern[1..];
            }
            if let Some(rest) = name.strip_prefix(pattern.as_bytes())
                && (rest.is_empty() || rest[0] == b'/')
            {
                return !negated;
            }
        }
        false
    }
}
