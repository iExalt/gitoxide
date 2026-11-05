//! Housekeeping for the object database and the references, which must never lose what somebody still needs.
//!
//! Everything here decides what is wanted before anything is removed, and errs on the side of keeping an object.

pub mod prune;
pub mod reachable;
pub mod reflog;
pub mod repack;
