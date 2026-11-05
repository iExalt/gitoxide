//! References that aren't to be shown or updated by a receiver, which `receive.hideRefs` and `transfer.hideRefs` configure.
use gix_object::bstr::{BStr, BString, ByteSlice};

/// The patterns of `transfer.hideRefs` and `receive.hideRefs` in the order in which they were configured.
///
/// A pattern hides the reference of that name and all the references below it. The last pattern that matches decides, and a
/// pattern that starts with `!` shows what it matches. A pattern that starts with `^` matches the name with the namespace,
/// while others match the name without it, see [`HiddenRefs::is_hidden_in()`].
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
        self.is_hidden_in(Some(name), name)
    }

    /// Return `true` if the reference with the full name `full` is hidden, where `name` is the name it has in the namespace, or
    /// `None` if it isn't in the namespace, as with references of other namespaces. Patterns that start with `^` match `full`,
    /// and all other patterns only match `name`, so they never hide what isn't in the namespace.
    pub fn is_hidden_in(&self, name: Option<&BStr>, full: &BStr) -> bool {
        for pattern in self.patterns.iter().rev() {
            let mut pattern = pattern.as_bstr();
            let negated = pattern.first() == Some(&b'!');
            if negated {
                pattern = &pattern[1..];
            }
            let mut subject = name;
            if pattern.first() == Some(&b'^') {
                pattern = &pattern[1..];
                subject = Some(full);
            }
            let Some(subject) = subject else { continue };
            if let Some(rest) = subject.strip_prefix(pattern.as_bytes())
                && (rest.is_empty() || rest[0] == b'/')
            {
                return !negated;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use gix_object::bstr::ByteSlice;

    use super::HiddenRefs;

    #[test]
    fn patterns_with_a_caret_match_the_full_name_and_others_only_the_name_in_the_namespace() {
        let hidden = HiddenRefs::from_patterns(["refs/heads/a", "^refs/namespaces/ns/refs/heads/b"]);
        let full = |name: &str| format!("refs/namespaces/ns/{name}");
        let is_hidden = |name: &str| {
            let full = full(name);
            hidden.is_hidden_in(Some(name.as_bytes().as_bstr()), full.as_bytes().as_bstr())
        };
        assert!(
            is_hidden("refs/heads/a"),
            "the name in the namespace is what plain patterns match"
        );
        assert!(is_hidden("refs/heads/b"), "the full name is what a caret matches");
        assert!(!is_hidden("refs/heads/c"));
        assert!(
            !hidden.is_hidden_in(None, "refs/heads/a".as_bytes().as_bstr()),
            "plain patterns never hide what isn't in the namespace"
        );
        assert!(
            HiddenRefs::from_patterns(["^refs/namespaces/other"])
                .is_hidden_in(None, "refs/namespaces/other/refs/heads/x".as_bytes().as_bstr()),
            "but a caret does"
        );
    }
}
