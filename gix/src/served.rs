//! What receiving pushes and serving fetches have in common: hiding references and refusing what can't be served.
//! References that aren't to be shown or updated by a receiver, which `receive.hideRefs` and `transfer.hideRefs` configure.
use gix_error::{Result, ResultExt, bail, message, unsupported};
use gix_object::bstr::{BStr, BString, ByteSlice};

use crate::Repository;

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

    #[cfg_attr(not(feature = "receive"), allow(dead_code))]
    /// Create the patterns from their text, as they would be configured one after the other.
    pub fn from_patterns(patterns: impl IntoIterator<Item = impl Into<BString>>) -> Self {
        HiddenRefs {
            patterns: patterns.into_iter().map(Into::into).collect(),
        }
    }

    /// Return `true` if no reference is hidden.
    #[cfg_attr(not(feature = "receive"), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Return `true` if the reference named `name`, which is the full name like `refs/heads/main`, is hidden.
    #[cfg_attr(not(feature = "receive"), allow(dead_code))]
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

/// What a repository is used for, which decides how refusals are worded.
#[cfg_attr(not(all(feature = "receive", feature = "upload")), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Receive,
    Upload,
}

/// Refuse what makes a repository unsafe or impossible to serve for `role` in the same way for both, which is to be partial,
/// to be owned by somebody else, to declare extensions that aren't known to matter, and to be shallow.
pub(crate) fn refuse_unsupported_repository(repo: &Repository, role: Role) -> Result<()> {
    let config = repo.config_snapshot();
    if config
        .plumbing()
        .sections_by_name("remote")
        .into_iter()
        .flatten()
        .any(|section| {
            section.body().value("promisor").is_some() || section.body().value("partialclonefilter").is_some()
        })
    {
        bail!(unsupported(
            "a promisor remote is configured, and partial clones aren't supported"
        ));
    }
    // Git doesn't enter a repository that isn't owned by the user, and a reduced trust hides configured hooks from the gate
    // below, so what Git would refuse must not be received into at all.
    if repo.git_dir_trust() != gix_sec::Trust::Full {
        bail!(unsupported(match role {
            Role::Receive => "the repository isn't fully trusted, which Git refuses to receive into as well",
            Role::Upload => {
                "serving a repository that isn't fully trusted isn't supported yet, even though Git serves it, as nothing it has is run"
            }
        }));
    }
    // What a repository declares to need is unknown unless it was checked to not matter here, as `compatObjectFormat`
    // makes Git store a mapping of every object that it writes.
    for section in config.plumbing().sections_by_name("extensions").into_iter().flatten() {
        for key in section.body().value_names() {
            let key = key.to_string();
            if ![
                "objectformat",
                "refstorage",
                "worktreeconfig",
                "noop",
                "noop-v1",
                "preciousobjects",
                "relativeworktrees",
                "submodulepathconfig",
            ]
            .iter()
            .any(|known| key.eq_ignore_ascii_case(known))
            {
                bail!(unsupported(format!(
                    "extensions.{key} isn't known to be safe to {} with",
                    match role {
                        Role::Receive => "receive",
                        Role::Upload => "serve",
                    }
                )));
            }
        }
    }
    if repo
        .is_shallow()
        .or_raise(|| message("Could not read the shallow file"))?
    {
        bail!(unsupported(match role {
            Role::Receive => "receiving into a shallow repository isn't supported",
            Role::Upload => "serving a shallow repository isn't supported yet",
        }));
    }
    Ok(())
}
