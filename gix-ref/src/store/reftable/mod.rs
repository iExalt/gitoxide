//! A store for references that are kept in the reftable format, which is what `extensions.refStorage = reftable` selects.
//!
//! References and their reflogs are records in immutable tables. A *stack* of tables, listed by `tables.list`, is the
//! state of the references stored in a directory. A repository has one stack with the shared references and those of
//! the main worktree, in `<common-dir>/reftable`, and every linked worktree has another one for the references that are
//! private to it, like its `HEAD`, in `<git-dir>/reftable`. Which stack holds a reference follows Git, see
//! [`Store::find()`].
//!
//! Stacks are opened when they are first needed, which is why creating and cloning a [`Store`] cannot fail and is cheap.
//! Clones share what was read, and every lookup of a single reference reads the list of its stack again to see what other
//! writers published, like Git does. Iterations and [snapshots](Store::snapshot()) use the state of the stacks as they
//! found it. Reflog reads look for changes too, whereas Git's iteration of reflogs doesn't, as its processes are short-lived.
//!
//! References are edited with a [transaction](crate::Store::as_ref()) that adds one table to every stack it changes.
use std::path::{Path, PathBuf};

use gix_error::{ErrorExt, Result};
use gix_parallel::OwnShared;

use crate::{Namespace, store::WriteReflog};

mod find;
pub use find::Snapshot;
/// How stacks are changed: the lock timeout, the flushing, the layout of tables and when to compact them.
pub use gix_reftable::stack::Options as StackOptions;
mod iter;
pub use iter::{Iter, Platform};
mod log;
mod reflog_edit;
mod stacks;
mod transaction;
pub(crate) use transaction::{Prepared, Transaction};

/// A store for references that are kept in stacks of reftables.
///
/// Create it with [`at_opts()`](Store::at_opts()) or [`for_linked_worktree_opts()`](Store::for_linked_worktree_opts()),
/// and turn it into a [`gix_ref::Store`](crate::Store) with [`From`] to use it like any other reference storage.
#[derive(Debug, Clone)]
pub struct Store {
    git_dir: PathBuf,
    common_dir: Option<PathBuf>,
    object_hash: gix_hash::Kind,
    pub(crate) write_reflog: WriteReflog,
    pub(crate) namespace: Option<Namespace>,
    unsupported_backend: Option<String>,
    stack_options: std::result::Result<StackOptions, std::sync::Arc<gix_error::Error>>,
    stacks: OwnShared<stacks::Stacks>,
}

/// Lifecycle
impl Store {
    /// Create a store for the repository whose `.git` directory is `git_dir`, and whose references point to objects of
    /// `object_hash`. Nothing is read until the references are used.
    pub fn at(git_dir: PathBuf, object_hash: gix_hash::Kind) -> Self {
        Self::at_opts(git_dir, object_hash, Default::default())
    }

    /// Like [`at()`](Self::at()), but with `options`, of which only the way reflogs are written has an effect: names are
    /// bytes in a table, and nothing is stored in files whose names would need to be adjusted.
    /// That also means that, unlike with files, partial names aren't precomposed for `precompose_unicode`.
    pub fn at_opts(git_dir: PathBuf, object_hash: gix_hash::Kind, options: crate::store::init::Options) -> Self {
        Self::new(git_dir, None, object_hash, options)
    }

    /// Like [`at()`](Self::at()), but for a _linked_ worktree whose `git_dir` holds the stack of the references that are
    /// private to it, whereas all others are in the stack of the repository at `common_dir`.
    pub fn for_linked_worktree(git_dir: PathBuf, common_dir: PathBuf, object_hash: gix_hash::Kind) -> Self {
        Self::for_linked_worktree_opts(git_dir, common_dir, object_hash, Default::default())
    }

    /// Like [`for_linked_worktree()`](Self::for_linked_worktree()), but with `options`, which are used like those of
    /// [`at_opts()`](Self::at_opts()).
    pub fn for_linked_worktree_opts(
        git_dir: PathBuf,
        common_dir: PathBuf,
        object_hash: gix_hash::Kind,
        options: crate::store::init::Options,
    ) -> Self {
        Self::new(git_dir, Some(common_dir), object_hash, options)
    }

    fn new(
        git_dir: PathBuf,
        common_dir: Option<PathBuf>,
        object_hash: gix_hash::Kind,
        options: crate::store::init::Options,
    ) -> Self {
        let stacks = stacks::Stacks::new(&git_dir, common_dir.as_deref(), object_hash);
        Store {
            git_dir,
            common_dir,
            object_hash,
            write_reflog: options.write_reflog,
            namespace: None,
            unsupported_backend: None,
            stack_options: Ok(StackOptions::default()),
            stacks: OwnShared::new(stacks),
        }
    }

    /// See [`crate::Store::with_locations()`]. The stacks that were read are not kept, as they are somewhere else now.
    pub(crate) fn with_locations(&self, git_dir: PathBuf, common_dir: Option<PathBuf>) -> Self {
        Store {
            stacks: OwnShared::new(stacks::Stacks::new(&git_dir, common_dir.as_deref(), self.object_hash)),
            git_dir,
            common_dir,
            object_hash: self.object_hash,
            write_reflog: self.write_reflog,
            namespace: self.namespace.clone(),
            unsupported_backend: self.unsupported_backend.clone(),
            stack_options: self.stack_options.clone(),
        }
    }
}

/// Configuration
impl Store {
    /// Change how stacks are changed from now on, which is how tables are laid out, how long to wait for locks, what
    /// to flush and when to compact. It has no effect on reading.
    ///
    /// Options that couldn't be determined, for example because the configuration they come from is invalid, are an
    /// error that is reported by whatever needs them, which is changing references, and not before. That's what Git does,
    /// which reads reftable configuration when it writes for the first time and thus can read references with invalid
    /// settings.
    pub fn set_stack_options(&mut self, options: Result<StackOptions>) {
        self.stack_options = options.map_err(std::sync::Arc::new);
    }

    /// The options with which stacks are changed, or the reason they are invalid, see
    /// [`set_stack_options()`](Self::set_stack_options()).
    pub fn stack_options(&self) -> Result<&StackOptions> {
        self.stack_options.as_ref().map_err(|reason| {
            InvalidOptions(reason.clone()).and_raise(gix_error::validation("The reftable configuration is invalid"))
        })
    }
}

/// The error that made options invalid, which is shared between all failures that are caused by it.
#[derive(Debug)]
struct InvalidOptions(std::sync::Arc<gix_error::Error>);

impl std::fmt::Display for InvalidOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.0, f)
    }
}

impl std::error::Error for InvalidOptions {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&*self.0)
    }
}

/// Access
impl Store {
    /// Return the kind of hash that is used by the objects references point to.
    pub fn object_hash(&self) -> gix_hash::Kind {
        self.object_hash
    }

    /// Return the `.git` directory at which the references that are private to a worktree are kept.
    ///
    /// For linked worktrees, this is their private location, and [`common_dir()`](Self::common_dir()) is
    /// `Some(parent_git_dir)`.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// If this is a linked worktree, return the directory of the parent repository which holds the shared references.
    pub fn common_dir(&self) -> Option<&Path> {
        self.common_dir.as_deref()
    }

    /// Return the directory of the repository that holds the shared references, which is the
    /// [common directory](Self::common_dir()), or the [git directory](Self::git_dir()) if this isn't a linked worktree.
    pub fn common_dir_resolved(&self) -> &Path {
        self.common_dir.as_deref().unwrap_or(&self.git_dir)
    }

    /// Return the name of the unsupported reference storage backend that this store refuses to operate on, if any.
    pub fn unsupported_backend(&self) -> Option<&str> {
        self.unsupported_backend.as_deref()
    }

    /// Mark this store as unable to handle the reference storage backend named `backend`, like `reftable://<payload>`, or
    /// reset the mark with `None`.
    ///
    /// Afterwards every operation that finds, iterates, reads reflogs of, or edits references fails with an error of class
    /// [`Unsupported`](gix_error::Class::Unsupported), like it does for [`file::Store::set_unsupported_backend()`](crate::file::Store::set_unsupported_backend()),
    /// as the stacks this store would read are not where the references are.
    pub fn set_unsupported_backend(&mut self, backend: Option<String>) {
        self.unsupported_backend = backend;
    }

    /// Fail with an error of class [`Unsupported`](gix_error::Class::Unsupported) if
    /// [`unsupported_backend()`](Self::unsupported_backend()) is set.
    pub fn assure_backend_supported(&self) -> gix_error::Result<()> {
        match self.unsupported_backend.as_deref() {
            Some(backend) => gix_error::bail!(gix_error::unsupported(format!(
                "References are stored using the unsupported `{backend}` backend"
            ))),
            None => Ok(()),
        }
    }

    /// Return `Some(true)` if this is a freshly initialized store without any observable changes, with `HEAD` still
    /// pointing to `default_ref`, or `None` if `HEAD` couldn't be read.
    pub fn is_pristine(&self, default_ref: &crate::FullNameRef) -> Option<bool> {
        use crate::Target;
        let head = self.try_find("HEAD").ok()??;
        match head.target {
            Target::Object(_) => return Some(false),
            Target::Symbolic(name) if name.as_ref() != default_ref => return Some(false),
            Target::Symbolic(_) => {}
        }
        let platform = self.iter().ok()?;
        let mut all = platform.all().ok()?;
        Some(all.next().is_none())
    }
}
