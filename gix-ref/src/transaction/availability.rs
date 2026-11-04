//! Tell whether a reference can be created next to the existing ones.
//!
//! Reference names are paths: `refs/heads/a` and `refs/heads/a/b` can't both exist, no matter whether they are stored
//! as files and directories, in `packed-refs`, or in tables, as readers and the next `pack-refs` or conversion would
//! trip over them. Git rejects such a transaction while preparing it, before anything is changed, and so does this crate.
//! The rule doesn't depend on how references are stored, so storage only answers the two questions of [`ExistingRefs`].
use gix_error::{ClassificationMarker, ErrorExt, Result, message};
use gix_object::bstr::{BStr, BString, ByteSlice};

/// What a store can tell about the references that exist, for [`verify_available()`].
pub(crate) trait ExistingRefs {
    /// Return `true` if a reference named exactly `name` exists, no matter if it can be decoded.
    fn exists(&self, name: &BStr) -> Result<bool>;
    /// Return the name of a reference that exists below the directory `directory`, which ends with a slash.
    fn any_below(&self, directory: &BStr) -> Result<Option<BString>>;
}

/// A reference can't be created because its name conflicts with that of another reference, as one of them would be a
/// directory of the other.
///
/// It identifies a failed precondition of an edit and is a conflict that retrying can't resolve without changing one of
/// the names, and it is raised for every kind of storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameConflict {
    /// The name of the reference that was to be created.
    pub full_name: BString,
    /// The name of the other reference.
    pub other: BString,
    /// If `true`, the other reference is edited by the same transaction, otherwise it exists already.
    pub in_transaction: bool,
}

impl std::fmt::Display for NameConflict {
    /// These are Git's messages.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.in_transaction {
            write!(
                f,
                "cannot process '{}' and '{}' at the same time",
                self.full_name, self.other
            )
        } else {
            write!(f, "'{}' exists; cannot create '{}'", self.other, self.full_name)
        }
    }
}

impl std::error::Error for NameConflict {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(const { &ClassificationMarker::CONFLICT })
    }
}

/// Verify that a reference named `name` can be created while the transaction edits the references in `in_transaction`
/// (which includes `name` itself and is sorted), next to the `existing` references.
///
/// Like in Git, the names of all edits of the transaction count, whether they delete or write, as it makes no
/// sense to rely on the order of edits. They are looked up by bisection, so that a transaction of many edits stays
/// cheap.
pub(crate) fn verify_available(name: &BStr, in_transaction: &[BString], existing: &impl ExistingRefs) -> Result<()> {
    let conflict = |other: &BStr, in_transaction: bool| {
        Err(NameConflict {
            full_name: name.to_owned(),
            other: other.to_owned(),
            in_transaction,
        }
        .and_raise(message!(
            "Reference '{name}' can't be created as its name conflicts with that of another one"
        )))
    };

    for (slash, _) in name.iter().enumerate().filter(|(_, b)| **b == b'/') {
        let parent = name[..slash].as_bstr();
        if existing.exists(parent)? {
            return conflict(parent, false);
        }
        if in_transaction
            .binary_search_by(|edited| edited.as_bstr().cmp(parent))
            .is_ok()
        {
            return conflict(parent, true);
        }
    }

    let mut directory = name.to_owned();
    directory.push(b'/');
    if let Some(child) = existing.any_below(directory.as_bstr())? {
        return conflict(child.as_bstr(), false);
    }
    // Names below the directory sort right after it.
    let first_candidate = in_transaction.partition_point(|edited| edited.as_slice() < directory.as_slice());
    if let Some(child) = in_transaction
        .get(first_candidate)
        .filter(|edited| edited.starts_with(directory.as_slice()))
    {
        return conflict(child.as_bstr(), true);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    struct Existing(BTreeSet<&'static str>);

    impl ExistingRefs for Existing {
        fn exists(&self, name: &BStr) -> Result<bool> {
            Ok(self.0.contains(name.to_str().expect("test names are UTF-8")))
        }

        fn any_below(&self, directory: &BStr) -> Result<Option<BString>> {
            let directory = directory.to_str().expect("test names are UTF-8");
            Ok(self
                .0
                .iter()
                .find(|name| name.starts_with(directory))
                .map(|name| (*name).into()))
        }
    }

    fn verify(name: &str, in_transaction: &[&str], existing: &[&'static str]) -> Result<()> {
        let mut in_transaction: Vec<BString> = in_transaction.iter().map(|name| (*name).into()).collect();
        in_transaction.sort();
        verify_available(
            name.into(),
            &in_transaction,
            &Existing(existing.iter().copied().collect()),
        )
    }

    fn conflict(err: gix_error::Error) -> NameConflict {
        err.downcast_any_ref::<NameConflict>()
            .expect("the failure is a name conflict")
            .clone()
    }

    #[test]
    fn names_that_are_not_related_are_available() {
        verify(
            "refs/heads/a",
            &["refs/heads/a", "refs/heads/ab"],
            &["refs/heads/a-b", "refs/heads/b/c"],
        )
        .expect("a common string prefix without a slash doesn't make a directory");
    }

    #[test]
    fn an_existing_reference_that_would_be_a_directory_conflicts() {
        let err = conflict(verify("refs/heads/a/b", &[], &["refs/heads/a"]).expect_err("'a' is a file"));
        assert_eq!(err.other, "refs/heads/a");
        assert!(!err.in_transaction);
        assert_eq!(err.to_string(), "'refs/heads/a' exists; cannot create 'refs/heads/a/b'");
    }

    #[test]
    fn an_existing_reference_below_the_name_conflicts() {
        let err = conflict(verify("refs/heads/a", &[], &["refs/heads/a/b"]).expect_err("'a' would be a directory"));
        assert_eq!(err.other, "refs/heads/a/b");
        assert!(!err.in_transaction);
    }

    #[test]
    fn edits_of_the_same_transaction_conflict_in_any_order() {
        for (name, other) in [("refs/heads/q", "refs/heads/q/r"), ("refs/heads/q/r", "refs/heads/q")] {
            let err = conflict(verify(name, &[other, name], &[]).expect_err("one is the directory of the other"));
            assert_eq!(err.other, other);
            assert!(err.in_transaction);
            assert_eq!(
                err.to_string(),
                format!("cannot process '{name}' and '{other}' at the same time")
            );
        }
    }
}
