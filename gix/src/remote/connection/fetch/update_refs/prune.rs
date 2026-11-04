//! Find the local references that a fetch is responsible for and that the remote no longer has.
use std::collections::HashSet;

use crate::bstr::{BStr, BString, ByteSlice};
use gix_ref::{FullName, Target, transaction::PreviousValue};

use crate::{Repository, Result, remote::fetch::refmap::Mapping};

/// A local reference that no longer has a counterpart on the remote.
pub(super) struct Stale {
    pub name: FullName,
    /// The value it has, which must still be the value when it is deleted.
    pub target: Target,
}

impl Stale {
    pub(super) fn into_delete_edit(self) -> gix_ref::transaction::RefEdit {
        gix_ref::transaction::RefEdit::delete(self.name, PreviousValue::MustExistAndMatch(self.target))
    }
}

/// Return the references that are destinations of `refspecs` and have no counterpart in `mappings`, which are
/// all references that the remote has for these specs.
///
/// This is how Git decides it: a local reference is covered by a spec if the spec's destination, or its reverse mapping of
/// the reference's name, matches it, which also applies negative specs to the source the reference maps to.
/// Symbolic references aren't considered, like in Git, as the reference they point to is what tracks the remote.
/// The destinations of implicit specs, like the one that follows tags, aren't covered, but what they map counts as
/// present.
pub(super) fn stale_destinations(
    repo: &Repository,
    refspecs: &[gix_refspec::RefSpec],
    mappings: &[Mapping],
) -> Result<Vec<Stale>> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for spec in refspecs {
        let spec = spec.to_ref();
        let Some(destination) = spec.destination() else {
            continue;
        };
        let prefix = literal_prefix(destination);
        for reference in repo.references()?.prefixed(prefix.as_bstr())? {
            let reference = reference?;
            let Some(id) = reference.target().try_id().map(ToOwned::to_owned) else {
                continue;
            };
            if seen.insert(reference.name().as_bstr().to_owned()) {
                candidates.push((reference.name().to_owned(), id));
            }
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let items = candidates.iter().map(|(name, id)| gix_refspec::match_group::Item {
        full_ref_name: name.as_bstr(),
        target: id,
        object: None,
    });
    // A spec that asks the remote for no reference prefix at all may not have had its references listed if the server
    // filtered them by prefix, so what it maps to can't be told to be gone. Negative specs only ever exclude.
    let listed = |spec: &gix_refspec::RefSpecRef<'_>| {
        let mut prefixes = Vec::new();
        spec.expand_prefixes(&mut prefixes);
        !prefixes.is_empty()
            || matches!(
                spec.instruction(),
                gix_refspec::Instruction::Fetch(gix_refspec::instruction::Fetch::Exclude { .. })
            )
    };
    let covered: HashSet<usize> =
        gix_refspec::MatchGroup::from_fetch_specs(refspecs.iter().map(|spec| spec.to_ref()).filter(listed))
            .match_rhs(items)
            .mappings
            .into_iter()
            .filter_map(|mapping| mapping.item_index)
            .collect();
    let present: HashSet<&BStr> = mappings
        .iter()
        .filter_map(|mapping| mapping.local.as_ref().map(|name| name.as_bstr()))
        .collect();
    Ok(candidates
        .into_iter()
        .enumerate()
        .filter(|(index, (name, _))| covered.contains(index) && !present.contains(name.as_bstr()))
        .map(|(_, (name, id))| Stale {
            name,
            target: Target::Object(id),
        })
        .collect())
}

/// The part of `destination` up to the last `/` before the first `*`, which all names that the destination can match start with.
fn literal_prefix(destination: &BStr) -> BString {
    let literal = destination.split_str("*").next().unwrap_or_default();
    match literal.rfind_byte(b'/') {
        Some(slash) if literal.len() != destination.len() => literal[..=slash].into(),
        _ => destination.to_owned(),
    }
}

/// Return `true` if `name` can't exist next to `other` as one would have to be a directory and the other a file.
pub(super) fn conflicts(name: &BStr, other: &BStr) -> bool {
    let is_parent =
        |parent: &BStr, child: &BStr| child.starts_with(parent.as_bytes()) && child.get(parent.len()) == Some(&b'/');
    is_parent(name, other) || is_parent(other, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_literal_prefix_of_a_destination_ends_at_a_directory() {
        for (destination, expected) in [
            ("refs/remotes/o/*", "refs/remotes/o/"),
            ("refs/remotes/o/pre*", "refs/remotes/o/"),
            ("refs/remotes/o/main", "refs/remotes/o/main"),
            ("refs/*", "refs/"),
        ] {
            assert_eq!(literal_prefix(destination.into()), expected, "{destination}");
        }
    }

    #[test]
    fn names_conflict_if_one_would_have_to_be_a_directory() {
        for (name, other, expected) in [
            ("refs/p/a", "refs/p/a/b", true),
            ("refs/p/a/b", "refs/p/a", true),
            ("refs/p/a", "refs/p/ab", false),
            ("refs/p/a", "refs/p/a", false),
            ("refs/p/a", "refs/q/a/b", false),
        ] {
            assert_eq!(conflicts(name.into(), other.into()), expected, "{name} vs {other}");
        }
    }
}
