//! Locate and run Git hooks, matching how `git` treats them.
//!
//! The hooks of an event are, in order, those defined by `hook.<name>.command` configuration (see [`Settings`]) followed by
//! the executable file named after the event in the hooks directory. [`Hooks::resolve()`] lists them and [`Prepare`] runs them.
//!
//! * A missing hook file is silently skipped, and one that isn't executable is skipped with [advice](ignored_hook_hint()).
//! * Callers decide what a non-zero [exit code](Outcome::exit_code()) means: a veto for hooks like `pre-push`
//!   ([`Outcome::into_veto()`]), or a notification for hooks like `post-receive`.
//! * Arguments, standard input, environment and working directory are what the caller provides, as `git` provides them per hook.
//!   By default a hook's standard output is redirected to standard error, as `git` does.
//!
//! ### Differences to `git`
//!
//! * Warnings about invalid `hook.*` values are not printed, the values are ignored as `git` does.
//! * On Windows, no `.exe` suffix is tried for hook files.
//! * Names and commands of configured hooks are converted to UTF-8 lossily, whereas Git handles bytes.
//! * Keys without value like `hook.<name>.event` are not an error as they are in Git.
//! * No exec-path is prepended to `PATH`, as there is no `git` installation to point to.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod find;
pub use find::{Found, find, ignored_hook_hint};

///
pub mod config;
pub use config::Settings;

mod hooks;
pub use hooks::{Hook, Hooks, Kind, Side};

mod run;
pub use run::{Outcome, OutcomeOfHook, Prepare, Stderr, Stdin, Stdout};
