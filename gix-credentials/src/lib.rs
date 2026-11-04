//! Interact with git credentials in various ways and launch helper programs.
//!
//! ## Running without `git`
//!
//! Helpers configured by name, like `credential.helper = foo`, are the programs `git-credential-foo` that are found in
//! [`GIT_EXEC_PATH`, the directories of the environment and `PATH`](program::Environment), without `git` being asked to
//! find them. The `store` helper is implemented natively, see the [`store`] module. The `cache` helper isn't, so it is the
//! `git-credential-cache` program of Git if it can be found, and fails otherwise.
//! Git itself would also find helpers in its compiled-in exec-path, which can't be known without running `git`, so a helper
//! that exists only there has to be made available with [`program::Environment::exec_path`].
//! Only [`builtin()`] and [`program::Kind::Builtin`] run `git credential` on purpose.
//!
//! ## Feature Flags
#![cfg_attr(
    all(doc, feature = "document-features"),
    doc = ::document_features::document_features!()
)]
#![cfg_attr(all(doc, feature = "document-features"), feature(doc_cfg))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

use gix_error::Result;
/// A program/executable implementing the credential helper protocol.
#[derive(Debug)]
pub struct Program {
    /// The kind of program, ready for launch.
    pub kind: program::Kind,
    /// If true, stderr is enabled, which is the default.
    pub stderr: bool,
    /// `Some(…)` if the process is running.
    child: Option<std::process::Child>,
    /// Where to look for helpers, or `None` to use the environment of the process.
    environment: Option<program::Environment>,
}

///
pub mod helper;

///
pub mod program;

pub mod store;

///
pub mod protocol;

/// Call the `git credential` helper program performing the given `action`, which reads all context from the git configuration
/// and does everything `git` typically does. The `action` should have been created with [`helper::Action::get_for_url()`] to
/// contain only the URL to kick off the process, or should be created by [`helper::NextAction`].
/// If more control is required, use the [`Cascade`][helper::Cascade] type.
pub fn builtin(action: helper::Action) -> Result<Option<protocol::Outcome>> {
    protocol::helper_outcome_to_result(
        helper::invoke(&mut Program::from_kind(program::Kind::Builtin), &action)?,
        action,
    )
}
