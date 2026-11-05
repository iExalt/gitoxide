//! Receive what clients push into a repository on the local machine, like `git receive-pack` does, without running Git.
//!
//! The parts are the [`connectivity`] check that decides if the objects of a push are complete, the [`HiddenRefs`] that
//! a receiver doesn't show or update, and [`Options`] that configure how it treats pushes.
pub mod connectivity;

mod hooks;
pub use crate::served::HiddenRefs;

mod options;
pub use options::{DenyAction, Options};

mod policy;
pub use policy::{Level, Message};

mod receiver;
#[doc(hidden)]
pub use receiver::Fault;
pub use receiver::{Outcome, Receiver};
