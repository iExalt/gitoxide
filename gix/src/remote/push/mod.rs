//! Update references on a remote and send the objects they need, like `git push`, with leases that are always checked.
//!
//! ## Flow
//!
//! 1. Describe each reference to change as an [`Update`], which always carries a [`Lease`]: the value the caller believes
//!    the remote reference has, or that it is absent.
//! 2. Connect to the remote with [`Remote::connect(Direction::Push)`](crate::Remote::connect()) and call
//!    [`Connection::prepare_push()`](crate::remote::Connection::prepare_push()). This validates the updates, reads what the
//!    remote advertises, and decides for each update what to do about it, which is the [`Plan`].
//! 3. Look at the [`Plan`] if needed, optionally register a [`pre-push`](Prepare::with_before_send()) check, and
//!    [`send()`](Prepare::send()) what the plan allows.
//! 4. Inspect the [`Outcome`]. An error is only returned if nothing was sent. Once the request left, the result is
//!    an [`Outcome`], which says whether the outcome is [uncertain](Outcome::is_uncertain()).
//! 5. [Reconcile](crate::remote::Connection::reconcile()) an uncertain outcome with what the remote has, before retrying or
//!    recording anything as done. The result is an [`Observed`] value per update and not a verdict: only
//!    [`AtNew`](Observed::AtNew) shows that an update happened, and a retry keeps its original lease, which is checked
//!    against the advertisement again, so it can't overwrite what somebody else wrote in the meantime.
//!
//! ## Leases
//!
//! The value of a lease is what the command sent to the server expects. It is never replaced with what the remote
//! advertised, so the server verifies it again, atomically, when it updates the reference. What the remote
//! advertises is compared with the lease before anything is sent, and an update whose lease disagrees is
//! [refused](Refusal::StaleLease), as it is based on outdated knowledge.
//!
//! That comparison is not an optimization for deletions. `git receive-pack` deletes a reference without any check if the
//! expected value of the deletion names an object that it does not have, so for deletions the comparison with the advertisement
//! is the only protection. There is no way to send an update without it. What remains is a lease object that the
//! remote garbage-collects after it advertised the reference and before it deletes it, which can't be told apart from a
//! deletion that was expected to succeed.
//!
//! ## Limits
//!
//! Signed pushes, thin packs, and protocol version 2 aren't supported, and neither is pushing from a shallow
//! repository whose boundary the pushed history crosses.
mod update;
pub use update::{Lease, Policy, Update};

pub(crate) mod plan;
pub(crate) use plan::{Advertisement, evaluate};
pub use plan::{Decision, Entry, Plan, Refusal};

mod outcome;
pub use outcome::{
    Entry as OutcomeEntry, Observation, Observed, Outcome, Reconciliation, ServerUpdate, Status, Unpack,
};

mod prepare;
pub use prepare::{Options, PrePush, PrePushEntry, Prepare, ShallowSource};
