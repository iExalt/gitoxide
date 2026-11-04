use crate::bstr::BString;
use gix_hash::ObjectId;

use super::{Lease, Refusal, Update};

pub use gix_protocol::push::report::{Unpack, Update as ServerUpdate};

/// What became of one update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing was sent, as the remote reference already had the new value.
    UpToDate,
    /// Nothing was sent, as the update was [refused](Refusal) before.
    Refused(Refusal),
    /// The server applied the update. With `report-status-v2`, `updates` describe what it did, which can differ from the update
    /// if a server hook redirected it.
    Applied {
        /// What the server reported to have done, which is empty for servers that only speak `report-status`.
        updates: Vec<ServerUpdate>,
    },
    /// The server refused to apply the update, and says why. The reference wasn't changed.
    Rejected {
        /// The server's explanation, which is empty if it didn't give one.
        reason: BString,
    },
    /// The server reported a failure that doesn't prove that the update was *not* applied, as it reports the same for every
    /// command of a batch, including those it already committed. See [`protocol::push::report::Status::Uncertain`](gix_protocol::push::report::Status::Uncertain).
    Uncertain {
        /// The server's explanation.
        reason: BString,
    },
    /// The update was sent, but no complete and consistent report came back, so it may or may not have been applied.
    Unknown,
}

/// An update and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The update as requested.
    pub update: Update,
    /// What became of it.
    pub status: Status,
}

/// The result of [sending](super::Prepare::send()) a push, which only exists if the request may have reached the server,
/// or if there was nothing to send.
#[derive(Debug)]
pub struct Outcome {
    /// One entry per update, in the order of the updates.
    pub entries: Vec<Entry>,
    /// What the server said about unpacking the pack, or `None` if there was no pack or no report.
    pub unpack: Option<Unpack>,
    /// Set if the request was sent but no complete and consistent report came back. In that case, the entries that were sent are
    /// [`Status::Unknown`], and the remote has to be inspected before retrying or concluding anything.
    pub uncertain: Option<gix_error::Error>,
    /// If `true`, the server process finished by itself before we returned, or there is no such process. If `false`, we
    /// gave up waiting for it after [`Options::server_wait`](super::Options::server_wait), so it may still be running, and
    /// may be interrupted when the connection is dropped.
    pub server_finished: bool,
}

impl Outcome {
    /// Return `true` if every update is up to date or was applied, and the pack was unpacked if there was one.
    pub fn is_success(&self) -> bool {
        self.uncertain.is_none()
            && self.unpack.as_ref().is_none_or(|unpack| *unpack == Unpack::Ok)
            && self
                .entries
                .iter()
                .all(|entry| matches!(entry.status, Status::UpToDate | Status::Applied { .. }))
    }

    /// Return `true` if the outcome of at least one update can't be told from what the server said, so the remote must be
    /// inspected before retrying or recording the update as done or failed.
    pub fn is_uncertain(&self) -> bool {
        self.uncertain.is_some()
            || self
                .entries
                .iter()
                .any(|entry| matches!(entry.status, Status::Uncertain { .. } | Status::Unknown))
    }
}

impl Entry {
    /// Return `true` if the update was sent to the server, which means that it may have been applied whatever the status says,
    /// as a server can change the reference and then fail to tell. [Reconcile](crate::remote::Connection::reconcile()) these
    /// with the remote before concluding anything.
    pub fn was_sent(&self) -> bool {
        !matches!(self.status, Status::UpToDate | Status::Refused(_))
    }
}

/// What the remote reference is, compared to what the push expected and wanted. It is an observation only: another writer
/// can have changed the reference since the push, and can have put it back to a value it had before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    /// The reference has the value the update wanted it to have, or doesn't exist after a deletion.
    ///
    /// Only this permits to conclude that the update happened, and to remove a marker that says it is pending.
    AtNew,
    /// The reference has the value of the lease, or doesn't exist after a creation, which is what it had before the update.
    ///
    /// The update wasn't applied, or was applied and reverted by someone else, which can't be told. It is safe to try again
    /// with the original lease, which has to pass the check against the advertisement again, but it is not a reason to conclude
    /// that the update didn't happen.
    AtLease,
    /// The reference has another value, or doesn't exist.
    Elsewhere(Option<ObjectId>),
}

impl Observed {
    /// Compare `advertised`, the value of the reference on the remote, with what `update` expected and wanted.
    pub(crate) fn of(update: &Update, advertised: Option<ObjectId>) -> Observed {
        let lease = match update.lease {
            Lease::At(id) => Some(id),
            Lease::Absent => None,
        };
        if advertised == update.new {
            Observed::AtNew
        } else if advertised == lease {
            Observed::AtLease
        } else {
            Observed::Elsewhere(advertised)
        }
    }
}

/// What the remote has for an update that was sent, which is a statement about the remote when it was observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The update that was sent.
    pub update: Update,
    /// How the remote reference compares to it.
    pub observed: Observed,
}

/// Everything observed about the updates that were sent, obtained by [reconciling](crate::remote::Connection::reconcile()).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    /// One observation for each update that was sent, in the order of the outcome.
    pub observations: Vec<Observation>,
}

#[cfg(test)]
mod tests {
    use gix_hash::{Kind, ObjectId};
    use gix_ref::FullName;

    use super::*;

    fn id(byte: u8) -> ObjectId {
        ObjectId::from_bytes_or_panic(&vec![byte; Kind::Sha1.len_in_bytes()])
    }

    fn name() -> FullName {
        "refs/heads/a".try_into().expect("valid name")
    }

    #[test]
    fn observations_compare_the_remote_with_what_was_expected_and_wanted() {
        let change = Update::change(name(), id(1), id(2));
        assert_eq!(Observed::of(&change, Some(id(2))), Observed::AtNew);
        assert_eq!(
            Observed::of(&change, Some(id(1))),
            Observed::AtLease,
            "this is also what a push that was applied and reverted looks like"
        );
        assert_eq!(Observed::of(&change, Some(id(3))), Observed::Elsewhere(Some(id(3))));
        assert_eq!(Observed::of(&change, None), Observed::Elsewhere(None));

        let delete = Update::delete(name(), id(1));
        assert_eq!(Observed::of(&delete, None), Observed::AtNew, "the reference is gone");
        assert_eq!(Observed::of(&delete, Some(id(1))), Observed::AtLease);
        assert_eq!(Observed::of(&delete, Some(id(3))), Observed::Elsewhere(Some(id(3))));

        let create = Update::create(name(), id(2));
        assert_eq!(Observed::of(&create, Some(id(2))), Observed::AtNew);
        assert_eq!(Observed::of(&create, None), Observed::AtLease, "it doesn't exist yet");
        assert_eq!(Observed::of(&create, Some(id(3))), Observed::Elsewhere(Some(id(3))));
    }
}
