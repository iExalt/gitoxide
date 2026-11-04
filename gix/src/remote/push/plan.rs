use std::collections::BTreeMap;

use crate::bstr::{BString, ByteSlice};
use gix_hash::ObjectId;
use gix_protocol::handshake::Ref;

use super::{Lease, Policy, Update};

/// Why an update isn't sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The remote reference doesn't match the lease, so the caller's knowledge is outdated.
    StaleLease {
        /// What the remote advertised for the reference, or `None` if it didn't list it, which is also the case if it hides it.
        advertised: Option<ObjectId>,
    },
    /// The new value isn't a descendant of the value the reference has, and the update doesn't allow rewriting.
    NotFastForward,
    /// The object the reference is expected to have isn't in the local object database, so it is unknown if the update
    /// is a fast-forward. Fetch it first.
    LeaseObjectMissing,
    /// A tag already exists, and updating it requires [`Policy::Rewrite`].
    TagUpdate,
}

/// What is to be done about an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The update is sent to the remote.
    Send,
    /// The remote reference already has the new value, so nothing is sent.
    UpToDate,
    /// The update isn't sent, for the given reason.
    Refused(Refusal),
}

/// An update and what is to be done about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The update as requested.
    pub update: Update,
    /// The value the remote advertised for the reference, or `None` if it didn't list it.
    pub advertised: Option<ObjectId>,
    /// What happens to it.
    pub decision: Decision,
}

/// The decisions for all updates of a push, in the order of the updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// One entry per update.
    pub entries: Vec<Entry>,
}

impl Plan {
    /// Return all entries that are going to be sent.
    pub fn to_send(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|entry| entry.decision == Decision::Send)
    }

    /// Return all entries that were refused.
    pub fn refused(&self) -> impl Iterator<Item = (&Entry, &Refusal)> {
        self.entries.iter().filter_map(|entry| match &entry.decision {
            Decision::Refused(refusal) => Some((entry, refusal)),
            _ => None,
        })
    }
}

/// What the remote advertised.
#[derive(Debug, Default)]
pub(crate) struct Advertisement {
    /// The references by name and the object each points to. Peeled values of tags are only used as `haves`.
    pub refs: BTreeMap<BString, ObjectId>,
    /// Every object the remote says it has, which includes those of references, their peeled values, and the ones of `.have` lines
    /// that announce what the remote borrows from alternates. These are not references.
    pub haves: Vec<ObjectId>,
}

impl Advertisement {
    pub(crate) fn from_refs(refs: &[Ref]) -> Self {
        let mut out = Advertisement::default();
        for r in refs {
            let (name, id, peeled) = match r {
                Ref::Direct { full_ref_name, object } => (full_ref_name, *object, None),
                Ref::Peeled {
                    full_ref_name,
                    tag,
                    object,
                } => (full_ref_name, *tag, Some(*object)),
                Ref::Symbolic {
                    full_ref_name, object, ..
                } => (full_ref_name, *object, None),
                Ref::Unborn { .. } => continue,
            };
            out.haves.push(id);
            out.haves.extend(peeled);
            if name.as_bytes() != b".have" {
                out.refs.insert(name.clone(), id);
            }
        }
        out
    }
}

/// How `lease` relates to `new`, for the ancestry check of fast-forward only updates.
pub(crate) enum Ancestry {
    Descendant,
    NotDescendant,
    LeaseObjectMissing,
}

/// Decide what to do with `updates` given what the remote `advertisement`, with `ancestry` telling if the first id is an ancestor of
/// the second one, which both are known to exist locally if the lease isn't unknown.
pub(crate) fn evaluate(
    updates: Vec<Update>,
    advertisement: &Advertisement,
    ancestry: &mut dyn FnMut(&gix_hash::oid, &gix_hash::oid) -> gix_error::Result<Ancestry>,
) -> gix_error::Result<Plan> {
    let mut entries = Vec::with_capacity(updates.len());
    for update in updates {
        let advertised = advertisement.refs.get(update.name.as_bstr()).copied();
        let lease_matches = match (update.lease, advertised) {
            (Lease::Absent, None) => true,
            (Lease::At(lease), Some(advertised)) => lease == advertised,
            _ => false,
        };
        let decision = if !lease_matches {
            Decision::Refused(Refusal::StaleLease { advertised })
        } else {
            match (update.new, update.lease) {
                (None, _) | (Some(_), Lease::Absent) => Decision::Send,
                (Some(new), Lease::At(lease)) if new == lease => Decision::UpToDate,
                (Some(_), Lease::At(_)) if update.policy == Policy::Rewrite => Decision::Send,
                (Some(_), Lease::At(_)) if update.name.as_bstr().starts_with(b"refs/tags/") => {
                    Decision::Refused(Refusal::TagUpdate)
                }
                (Some(new), Lease::At(lease)) => match ancestry(&lease, &new)? {
                    Ancestry::Descendant => Decision::Send,
                    Ancestry::NotDescendant => Decision::Refused(Refusal::NotFastForward),
                    Ancestry::LeaseObjectMissing => Decision::Refused(Refusal::LeaseObjectMissing),
                },
            }
        };
        entries.push(Entry {
            update,
            advertised,
            decision,
        });
    }
    Ok(Plan { entries })
}

#[cfg(test)]
mod tests {
    use gix_hash::{Kind, ObjectId};
    use gix_ref::FullName;

    use super::*;

    fn id(byte: u8) -> ObjectId {
        ObjectId::from_bytes_or_panic(&vec![byte; Kind::Sha1.len_in_bytes()])
    }

    fn name(name: &str) -> FullName {
        name.try_into().expect("valid name")
    }

    fn advertised(refs: &[(&str, u8)]) -> Advertisement {
        Advertisement {
            refs: refs.iter().map(|(n, b)| (BString::from(*n), id(*b))).collect(),
            haves: Vec::new(),
        }
    }

    fn decide(updates: Vec<Update>, advertisement: &Advertisement, ancestry: Ancestry) -> Vec<Decision> {
        let mut ancestry = Some(ancestry);
        evaluate(updates, advertisement, &mut |_, _| {
            Ok(ancestry.take().unwrap_or(Ancestry::NotDescendant))
        })
        .expect("the ancestry check doesn't fail")
        .entries
        .into_iter()
        .map(|entry| entry.decision)
        .collect()
    }

    #[test]
    fn a_lease_that_differs_from_the_advertisement_is_refused_for_every_kind_of_update() {
        let remote = advertised(&[("refs/heads/a", 1), ("refs/heads/b", 1)]);
        let stale = |advertised| Decision::Refused(Refusal::StaleLease { advertised });
        assert_eq!(
            decide(
                vec![
                    Update::change(name("refs/heads/a"), id(9), id(2)),
                    Update::delete(name("refs/heads/b"), id(9)),
                    Update::create(name("refs/heads/a"), id(2)),
                    Update::delete(name("refs/heads/missing"), id(9)),
                    Update::change(name("refs/heads/missing"), id(9), id(2)),
                ],
                &remote,
                Ancestry::Descendant
            ),
            [
                stale(Some(id(1))),
                stale(Some(id(1))),
                stale(Some(id(1))),
                stale(None),
                stale(None),
            ],
            "for deletions this is the only protection, as `receive-pack` deletes without a check if it doesn't know the expected object"
        );
    }

    #[test]
    fn matching_leases_are_sent_and_unchanged_values_are_up_to_date() {
        let remote = advertised(&[("refs/heads/a", 1), ("refs/heads/b", 1)]);
        assert_eq!(
            decide(
                vec![
                    Update::delete(name("refs/heads/a"), id(1)),
                    Update::create(name("refs/heads/new"), id(2)),
                    Update::change(name("refs/heads/b"), id(1), id(1)),
                ],
                &remote,
                Ancestry::Descendant
            ),
            [Decision::Send, Decision::Send, Decision::UpToDate]
        );
    }

    #[test]
    fn the_policy_decides_about_history() {
        let remote = advertised(&[("refs/heads/a", 1), ("refs/tags/t", 1)]);
        let update = |name_: &str| Update::change(name(name_), id(1), id(2));
        assert_eq!(
            decide(vec![update("refs/heads/a")], &remote, Ancestry::Descendant),
            [Decision::Send]
        );
        assert_eq!(
            decide(vec![update("refs/heads/a")], &remote, Ancestry::NotDescendant),
            [Decision::Refused(Refusal::NotFastForward)]
        );
        assert_eq!(
            decide(vec![update("refs/heads/a")], &remote, Ancestry::LeaseObjectMissing),
            [Decision::Refused(Refusal::LeaseObjectMissing)],
            "without the object, the answer can't be a fast-forward"
        );
        assert_eq!(
            decide(
                vec![update("refs/heads/a").with_policy(Policy::Rewrite)],
                &remote,
                Ancestry::NotDescendant
            ),
            [Decision::Send]
        );
        assert_eq!(
            decide(vec![update("refs/tags/t")], &remote, Ancestry::Descendant),
            [Decision::Refused(Refusal::TagUpdate)],
            "even a fast-forward of a tag needs force, like in Git"
        );
        assert_eq!(
            decide(
                vec![update("refs/tags/t").with_policy(Policy::Rewrite)],
                &remote,
                Ancestry::NotDescendant
            ),
            [Decision::Send]
        );
    }

    #[test]
    fn have_lines_are_objects_the_remote_has_but_never_references() {
        let advertisement = Advertisement::from_refs(&[
            Ref::Direct {
                full_ref_name: ".have".into(),
                object: id(7),
            },
            Ref::Direct {
                full_ref_name: "refs/heads/a".into(),
                object: id(1),
            },
            Ref::Peeled {
                full_ref_name: "refs/tags/t".into(),
                tag: id(2),
                object: id(3),
            },
            Ref::Unborn {
                full_ref_name: "HEAD".into(),
                target: "refs/heads/main".into(),
            },
        ]);
        assert_eq!(
            advertisement.refs.keys().map(ToString::to_string).collect::<Vec<_>>(),
            ["refs/heads/a", "refs/tags/t"]
        );
        assert_eq!(
            advertisement.refs[b"refs/tags/t".as_bstr()],
            id(2),
            "a tag reference has the tag's id, not the peeled one"
        );
        assert_eq!(advertisement.haves, [id(7), id(1), id(2), id(3)]);
    }
}
