//! Watch the states a transaction goes through, like Git's `reference-transaction` hook does, and veto it before it
//! changes anything.
//!
//! This is the plumbing for such a hook: an [`Observer`] is told about [`State`] changes with the [`Update`]s that are
//! concerned, and may refuse the two that precede the first change. Starting a process, if that's what an observer
//! does, is for the caller.
//!
//! The states follow Git, which is observable by hooks and relied upon by them:
//!
//! * [`Preparing`](State::Preparing) comes before anything is locked, and is the first state of every transaction. A
//!   [veto](Veto) of it ends the transaction with [`Aborted`](State::Aborted).
//! * [`Prepared`](State::Prepared) follows once all references are locked, with updates that are split along symbolic
//!   references. Updating `HEAD` names `HEAD` while preparing and the branch it points to once prepared.
//!   A veto ends the transaction with [`Aborted`](State::Aborted), after the locks were released.
//! * [`Committed`](State::Committed) is the final state of a successful transaction, and [`Aborted`](State::Aborted) that
//!   of one that was prepared, or vetoed while preparing, and not committed.
//!
//! Failing to prepare, for example because of a lock that is held or a value that isn't the expected one, ends the
//! transaction after [`Preparing`](State::Preparing) without any other state. So does a failure to commit, which
//! [says what became of each edit](super::commit_failure) instead, as Git fires nothing after it failed to finish a
//! transaction. The one exception is a commit that failed [before it changed anything](super::commit_failure::NothingApplied),
//! for example for want of a committer, which is [`Aborted`](State::Aborted). Git has no counterpart.
//!
//! Neither [`Committed`](State::Committed) nor [`Aborted`](State::Aborted) can be vetoed, and the result of the observer is
//! ignored for them. The same name edited twice is rejected before anything is reported, as in Git. A name that is also the
//! target of a symbolic reference being edited is only found while locking, and thus after [`Preparing`](State::Preparing).
//!
//! # Transactions on `packed-refs`
//!
//! The files store reports the transaction that Git opens on `packed-refs` as soon as it locks the first reference that
//! is deleted, with [`Scope::PackedRefs`] and the deleted references (without expected values), and also gets
//! [`Aborted`](State::Aborted) if preparing fails afterwards, for example if another edit can't be locked, `packed-refs` is
//! locked by somebody else or a symbolic reference duplicates another edit. Git locks `packed-refs` last, and the files
//! store takes that lock first, which it reports as if all deletions were locked before. If one of the deleted references is in `packed-refs`, the
//! transaction is prepared (and may be vetoed, which doesn't abort the outer transaction, as Git exits right away) and
//! committed, in the middle of committing the outer one. Otherwise it is aborted right away, which is why deleting a loose
//! reference is reported as `preparing`, `aborted`, `prepared` and `committed`, and is what Git does.
//!
//! A hook can't tell these events from the others other than by the references they contain, so a bridge to Git's hook
//! should present them as they are. Observers that are only interested in the outcome, or that serve other backends,
//! can ignore all events of this scope.
//!
//! The events of this scope match Git only as long as the store is configured like Git, which only deletes from
//! `packed-refs`. Updates that [pack](crate::file::transaction::PackedRefs) references are reported as well, which Git
//! has no counterpart for.
use crate::{FullName, Target};

/// A state of a transaction, named like the argument that Git passes to its `reference-transaction` hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum State {
    /// The transaction is about to lock references and nothing has been done yet.
    Preparing,
    /// All references are locked and the transaction is ready to be committed.
    Prepared,
    /// The transaction was committed.
    Committed,
    /// The transaction was not, and won't be committed.
    Aborted,
}

impl State {
    /// The name of the state as Git passes it to its hook, one of `preparing`, `prepared`, `committed` and `aborted`.
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Preparing => "preparing",
            State::Prepared => "prepared",
            State::Committed => "committed",
            State::Aborted => "aborted",
        }
    }
}

/// The part of the storage that a state refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Scope {
    /// The transaction itself.
    Transaction,
    /// The transaction that the files store opens on `packed-refs` for the references it deletes, or packs.
    PackedRefs,
}

/// A change of a reference as it is passed to an observer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// The name of the reference.
    pub name: FullName,
    /// The value that the reference was expected to have, as asked for by the caller, or `None` if there was no
    /// expectation about a value. Git hooks see the null id then.
    pub previous: Option<Target>,
    /// The value to write, or `None` if the reference is deleted.
    pub new: Option<Target>,
}

/// What an [`Observer`] is told.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Event<'a> {
    /// The state the transaction is in.
    pub state: State,
    /// The part of the storage that it refers to.
    pub scope: Scope,
    /// The changes of references that the transaction consists of, in the order in which they were requested.
    /// Edits that only affect a reflog are left out, as in Git.
    pub updates: &'a [Update],
}

/// Refuse to go on with a transaction, which only [`Preparing`](State::Preparing) and [`Prepared`](State::Prepared) can do.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Veto;

/// Get told about the states of a transaction, and refuse to let it proceed before it changed anything.
pub trait Observer {
    /// Called for each [`Event`]. Return a [`Veto`] to end the transaction without changing anything, which is only
    /// possible in the states [`Preparing`](State::Preparing) and [`Prepared`](State::Prepared) and ignored otherwise.
    fn observe(&mut self, event: &Event<'_>) -> Result<(), Veto>;
}

impl<F> Observer for F
where
    F: FnMut(&Event<'_>) -> Result<(), Veto>,
{
    fn observe(&mut self, event: &Event<'_>) -> Result<(), Veto> {
        self(event)
    }
}

/// The reason for a failure to prepare, as the marker of an error that can be found with
/// [`Error::downcast_any_ref()`](gix_error::Error::downcast_any_ref()). Nothing was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vetoed {
    /// The state in which the observer refused.
    pub state: State,
    /// The scope of the state.
    pub scope: Scope,
}

impl std::fmt::Display for Vetoed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The transaction was refused by an observer in the '{}' state",
            self.state.as_str()
        )
    }
}

impl std::error::Error for Vetoed {}
