//! Receive what clients push into a repository on the local machine, like `git receive-pack` does, without running Git.
//!
//! The parts are the [`connectivity`] check that decides if the objects of a push are complete.
pub mod connectivity;
