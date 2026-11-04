use crate::bstr::BString;

use super::{Refusal, Update};

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
