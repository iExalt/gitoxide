//! Edits that are rejected on their own while the rest of a transaction goes on, which is what Git's `--batch-updates` does.
use gix_object::bstr::{BStr, BString, ByteSlice};

use crate::file::transaction::prepare::{MustNotExist, ReferenceOutOfDate};
use crate::transaction::{Change, LogChange, NameConflict, PreviousValue, RefEdit, RefLog, observe::Update};

/// Why an edit was rejected, with the classes and descriptions that Git has for it.
///
/// Anything else that goes wrong while preparing a transaction fails all of it, like in Git.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RejectionReason {
    /// The name is a directory of another reference, or has one as a directory, in the repository or the transaction.
    NameConflict,
    /// The reference was to be created, but exists already. This is also the reason if the reference is locked by somebody else.
    CreateExists,
    /// The reference was expected to exist, but doesn't.
    NonexistentRef,
    /// The reference doesn't have the value the edit expected.
    IncorrectOldValue,
    /// The new value isn't valid, for instance because it's not a commit but meant for a branch, or doesn't exist. Stores don't make
    /// this decision, but callers may use it to report a failed edit like Git does.
    InvalidNewValue,
    /// A symbolic reference was expected, but the reference has an object id. Stores don't make this decision
    /// as they can't express the expectation.
    ExpectedSymref,
    /// Two references of the transaction differ only in case, on a case-insensitive file system.
    CaseConflict,
}

impl RejectionReason {
    /// The description that Git has for this reason, which is what `receive-pack` reports for an edit that was rejected.
    pub fn as_str(&self) -> &'static str {
        match self {
            RejectionReason::NameConflict => "refname conflict",
            RejectionReason::CreateExists => "reference already exists",
            RejectionReason::NonexistentRef => "reference does not exist",
            RejectionReason::IncorrectOldValue => "incorrect old value provided",
            RejectionReason::InvalidNewValue => "invalid new value provided",
            RejectionReason::ExpectedSymref => "expected symref but found regular ref",
            RejectionReason::CaseConflict => "reference conflict due to case-insensitive filesystem",
        }
    }
}

impl std::fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An edit that was rejected when a transaction was [prepared](super::super::view::transaction::Transaction::batch_updates()).
#[derive(Debug, Clone)]
pub struct Rejected {
    /// The position of the edit among those that were passed to prepare the transaction.
    pub index: usize,
    /// The edit as it was passed.
    pub edit: RefEdit,
    /// Why it was rejected.
    pub reason: RejectionReason,
    /// The details of the failure, with the explanation of what exactly was wrong.
    pub error: std::sync::Arc<gix_error::Error>,
}

/// Decide if `error`, which made a transaction fail to prepare, is the rejection of one of the edits named by `names`, which are
/// the names of all edits in order, and return the position of that edit and why it failed.
///
/// An edit whose name conflicts with that of an earlier edit of the transaction is the one to reject, like in Git, which
/// handles edits in order, or sorted by name if `by_name` is set as it is for reftables.
pub(crate) fn find_offender(
    error: &gix_error::Error,
    names: &[&BStr],
    by_name: bool,
) -> Option<(usize, RejectionReason)> {
    let conflict = error.downcast_any_ref::<NameConflict>();
    let reason = if error.downcast_any_ref::<ReferenceOutOfDate>().is_some() {
        RejectionReason::IncorrectOldValue
    } else if error.downcast_any_ref::<MustNotExist>().is_some() {
        RejectionReason::CreateExists
    } else if conflict.is_some() {
        RejectionReason::NameConflict
    } else if error.is_not_found() {
        RejectionReason::NonexistentRef
    } else if error
        .iter_errors()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io| io.kind() == std::io::ErrorKind::AlreadyExists)
    {
        // The lock that is needed for the reference is held by somebody else.
        RejectionReason::CreateExists
    } else {
        return None;
    };
    // Not all storage names the reference in the metadata, but the markers that identify a failure have it.
    let name = error
        .metadata()
        .find_map(|values| match values.get("reference")? {
            gix_error::MetadataValue::Bytes(name) => Some(BString::from(name.clone())),
            gix_error::MetadataValue::String(name) => Some(BString::from(name.as_str())),
            _ => None,
        })
        .or_else(|| conflict.map(|conflict| conflict.full_name.clone()))
        .or_else(|| {
            error
                .downcast_any_ref::<ReferenceOutOfDate>()
                .map(|marker| marker.full_name.clone())
        })
        .or_else(|| {
            error
                .downcast_any_ref::<MustNotExist>()
                .map(|marker| marker.full_name.clone())
        })?;
    let position = names.iter().position(|edited| *edited == name.as_bstr())?;
    let position = match conflict {
        Some(NameConflict {
            other,
            in_transaction: true,
            ..
        }) => names
            .iter()
            .position(|edited| *edited == other.as_bstr())
            .map_or(position, |other| {
                if by_name {
                    // Git's reftable backend handles edits sorted by name, so the name that sorts later conflicts.
                    if names[other] > names[position] {
                        other
                    } else {
                        position
                    }
                } else {
                    position.max(other)
                }
            }),
        _ => position,
    };
    Some((position, reason))
}

/// How an edit is told to observers, or `None` if it only changes a reflog.
pub(crate) fn observed(edit: &RefEdit) -> Option<Update> {
    let (log_mode, new, expected) = match &edit.change {
        Change::Delete { log, expected } => (*log, None, expected),
        Change::Update {
            log: LogChange { mode, .. },
            new,
            expected,
        } => (*mode, Some(new.clone()), expected),
    };
    (log_mode != RefLog::Only).then(|| Update {
        name: edit.name.clone(),
        previous: match expected {
            PreviousValue::MustExistAndMatch(target) | PreviousValue::ExistingMustMatch(target) => Some(target.clone()),
            PreviousValue::Any | PreviousValue::MustExist | PreviousValue::MustNotExist => None,
        },
        new,
    })
}

/// Which of `edits` are split along symbolic references, which `find` follows. Git counts such an edit as the updates of the
/// references that it leads to, which come last, so the position of a rejected edit among the others doesn't include it.
pub(crate) fn split_edits(
    edits: &[RefEdit],
    find: &mut dyn FnMut(&crate::PartialNameRef) -> Option<crate::Target>,
) -> Vec<bool> {
    use crate::transaction::ext::RefEditsExt;
    let mut copy = edits.to_vec();
    let mut split = vec![false; edits.len()];
    // A reference cycle or similar is the business of the preparation, which reports it.
    copy.extend_with_splits_of_symbolic_refs(find, &mut |parent, edit| {
        if let Some(split) = split.get_mut(parent) {
            *split = true;
        }
        edit
    })
    .ok();
    split
}

/// The position among all updates that the update of the rejected edit at `index` of `observed` takes, where `split` has those
/// that are split along symbolic references, which are counted last.
pub(crate) fn rejected_position(observed: &[Option<Update>], split: &[bool], index: usize) -> usize {
    if split[index] {
        return usize::MAX;
    }
    observed[..index]
        .iter()
        .zip(split)
        .filter(|(update, split)| update.is_some() && !**split)
        .count()
}

/// Put the `rejected` updates, each with its position among all updates, into `updates` which has none of them.
/// What `updates` has beyond that, like updates that were split along symbolic references, is appended.
pub(crate) fn splice(updates: &[Update], rejected: &[(usize, Update)]) -> Vec<Update> {
    let mut out = Vec::with_capacity(updates.len() + rejected.len());
    let mut remaining = updates.iter();
    for (position, update) in rejected {
        while out.len() < *position {
            match remaining.next() {
                Some(next) => out.push(next.clone()),
                None => break,
            }
        }
        out.push(update.clone());
    }
    out.extend(remaining.cloned());
    out
}
