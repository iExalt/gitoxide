//! Rules about the reflog of `HEAD` and about updates that change nothing, which Git applies to every ref store alike.
//!
//! Updating the branch that `HEAD` points at is also a change of `HEAD`, so Git writes the reflog entry to `HEAD` as
//! well, even though no edit mentions it. Storage only tells what `HEAD` points at and what a reference is, and
//! these functions decide what else is to be done, so that every backend does the same.
use gix_error::{Result, bail, conflict};
use gix_hash::ObjectId;
use gix_object::bstr::{BString, ByteSlice};

use crate::{
    FullName, FullNameRef, Target,
    transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
};

/// How a transaction treats `HEAD` and the branch it points at, which Git logs together.
///
/// The default is what Git does. The other is a deliberate deviation for callers that need more than Git offers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HeadLog {
    /// Do what Git does: writing or deleting the branch that `HEAD` points at is logged in `HEAD` as well, also if the
    /// value of the branch doesn't change, and a transaction that edits both `HEAD` and that branch is refused with
    /// `multiple updates for 'HEAD' (including one via its referent '…') are not allowed`.
    #[default]
    Git,
    /// Let the transaction decide what `HEAD` logs, which differs from Git in two ways.
    ///
    /// * If `HEAD` is edited as well as its branch, the edit of `HEAD` is the only one that is logged there, and the
    ///   transaction isn't refused. That allows moving a branch while attaching `HEAD` atomically.
    /// * An edit that expects exactly the value it writes (`PreviousValue::MustExistAndMatch(value)` with the same
    ///   `value` as new target) is a verification that logs nothing, not even in `HEAD`, and isn't a change of `HEAD`.
    ///   It guards a reference that is left as it is.
    ///
    /// Reftable storage doesn't implement this yet.
    CallerDecides,
}

/// Return the edit that writes the reflog entry of `HEAD` for `edit`, if Git would add one, or `None` if it wouldn't.
///
/// It does if `edit` writes or deletes the reference that `head_referent` names, which is what the symbolic
/// `HEAD` of the store points at, without following further symbolic references. Like in Git, the edit that is returned
/// only changes the reflog, and has the old and new value of `edit`, even if they are equal, whereas the reflog of
/// `edit` itself has no entry for an edit that changes nothing. A deletion is logged as a change to the null id.
///
/// * `via_head` is `true` if `edit` is the one that was split off from an edit of `HEAD`, which already is
///   the log entry of `HEAD`, so another one isn't added.
/// * `names_in_transaction` are the names of all edits, including those added when splitting symbolic references, and
///   with [`HeadLog::CallerDecides`] without those that are [verifications](is_verification()). It is sorted.
///   If it contains `HEAD`, the transaction is refused, as `HEAD` would be changed twice, or with
///   [`HeadLog::CallerDecides`], no entry is added as the edit of `HEAD` decides what it logs.
/// * `mode` is how the transaction treats `HEAD`.
/// * `object_hash` is the kind of hash of the repository, as a deletion has to name the null id.
///
/// `head_referent` is of the `HEAD` of the store itself, so for a linked worktree, that of the worktree, and `None` if
/// `HEAD` isn't symbolic or doesn't exist. Edits that only change a reflog, and edits of symbolic references, which
/// have no reflog, don't change what `HEAD` logs and thus never have a log entry for `HEAD`.
pub(crate) fn head_log_update(
    edit: &RefEdit,
    via_head: bool,
    head_referent: Option<&FullNameRef>,
    names_in_transaction: &[BString],
    object_hash: gix_hash::Kind,
    mode: HeadLog,
) -> Result<Option<RefEdit>> {
    let Some(head_referent) = head_referent else {
        return Ok(None);
    };
    if via_head || edit.name.as_ref() != head_referent || (mode == HeadLog::CallerDecides && is_verification(edit)) {
        return Ok(None);
    }
    let (log_mode, force_create_reflog, message, new) = match &edit.change {
        Change::Update {
            log,
            new: Target::Object(new),
            ..
        } => (log.mode, log.force_create_reflog, log.message.clone(), *new),
        Change::Delete { log, .. } => (*log, false, BString::default(), ObjectId::null(object_hash)),
        Change::Update {
            new: Target::Symbolic(_),
            ..
        } => return Ok(None),
    };
    if log_mode == RefLog::Only {
        return Ok(None);
    }
    if names_in_transaction
        .binary_search_by(|name| name.as_bstr().cmp(b"HEAD".as_bstr()))
        .is_ok()
    {
        if mode == HeadLog::CallerDecides {
            return Ok(None);
        }
        bail!(conflict(format!(
            "multiple updates for 'HEAD' (including one via its referent '{}') are not allowed",
            edit.name.as_bstr()
        )));
    }
    Ok(Some(RefEdit::update_with_log(
        FullName::try_from("HEAD").expect("a valid name"),
        new,
        PreviousValue::Any,
        LogChange {
            mode: RefLog::Only,
            force_create_reflog,
            message,
        },
    )))
}

/// Return `true` if `edit` only verifies that the reference has the value it would be written with, which is how a caller
/// who has nothing to change guards a reference while a transaction changes others. It writes no reflog entry, and
/// doesn't change `HEAD` through its referent.
///
/// Git has a command for that, `update-ref --stdin` with `verify`. An update that expects exactly its new value is not
/// one in Git, which logs it in `HEAD` like any other, so this only applies with [`HeadLog::CallerDecides`].
pub(crate) fn is_verification(edit: &RefEdit) -> bool {
    matches!(
        &edit.change,
        Change::Update {
            expected: PreviousValue::MustExistAndMatch(expected),
            new,
            log: LogChange { mode: RefLog::AndReference, .. },
        } if expected == new
    )
}

/// Return `true` if `edit` changes nothing because the reference it writes already has the value, given the `current`
/// value of the reference as it was read while holding its lock, or `None` if it doesn't exist.
///
/// Such an edit writes neither the reference nor a reflog entry, which doesn't apply to edits that only change
/// a reflog, as these have no reference to compare with, and to symbolic references, whose reflog isn't written.
pub(crate) fn is_noop_update(edit: &RefEdit, current: Option<&Target>) -> bool {
    matches!(
        (&edit.change, current),
        (
            Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    ..
                },
                new: Target::Object(new),
                ..
            },
            Some(Target::Object(current)),
        ) if new == current
    )
}

#[cfg(test)]
mod tests {
    use gix_hash::Kind;

    use super::*;

    fn name(name: &str) -> FullName {
        name.try_into().expect("valid name")
    }

    fn id(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex.as_bytes()).expect("valid hex")
    }

    fn sha1(byte: u8) -> ObjectId {
        id(&format!("{byte:02x}").repeat(20))
    }

    fn update(branch: &str, to: ObjectId, mode: RefLog) -> RefEdit {
        RefEdit::update_with_log(
            name(branch),
            to,
            PreviousValue::Any,
            LogChange {
                mode,
                force_create_reflog: true,
                message: "message".into(),
            },
        )
    }

    fn names(names: &[&str]) -> Vec<BString> {
        let mut names: Vec<BString> = names.iter().map(|name| (*name).into()).collect();
        names.sort();
        names
    }

    const MAIN: &str = "refs/heads/main";

    fn head_log(
        edit: &RefEdit,
        via_head: bool,
        referent: Option<&str>,
        in_transaction: &[&str],
        mode: HeadLog,
    ) -> Result<Option<RefEdit>> {
        let referent = referent.map(name);
        head_log_update(
            edit,
            via_head,
            referent.as_ref().map(FullName::as_ref),
            &names(in_transaction),
            Kind::Sha1,
            mode,
        )
    }

    fn git(edit: &RefEdit, via_head: bool, referent: Option<&str>, in_transaction: &[&str]) -> Option<RefEdit> {
        head_log(edit, via_head, referent, in_transaction, HeadLog::Git).expect("no conflict")
    }

    fn verification(branch: &str, value: ObjectId) -> RefEdit {
        RefEdit::update(
            name(branch),
            Target::Object(value),
            PreviousValue::MustExistAndMatch(Target::Object(value)),
            "m",
        )
    }

    #[test]
    fn the_branch_that_head_points_at_is_logged_in_head_as_well() {
        let edit = update(MAIN, sha1(1), RefLog::AndReference);
        let head = git(&edit, false, Some(MAIN), &[MAIN]).expect("a log entry for HEAD");
        assert_eq!(head.name.as_bstr(), "HEAD");
        assert_eq!(
            head.change,
            Change::Update {
                log: LogChange {
                    mode: RefLog::Only,
                    force_create_reflog: true,
                    message: "message".into()
                },
                expected: PreviousValue::Any,
                new: Target::Object(sha1(1)),
            },
            "it only changes the reflog, and has the message and the value of the update"
        );
    }

    #[test]
    fn a_deletion_is_logged_as_a_change_to_the_null_id() {
        let edit = RefEdit::delete(name(MAIN), PreviousValue::Any);
        let head = git(&edit, false, Some(MAIN), &[MAIN]).expect("a log entry for HEAD");
        assert!(matches!(
            head.change,
            Change::Update { new: Target::Object(id), log: LogChange { mode: RefLog::Only, .. }, .. } if id.is_null()
        ));
    }

    #[test]
    fn nothing_is_logged_for_other_edits() {
        let branch = update(MAIN, sha1(1), RefLog::AndReference);
        for (description, edit, via_head, referent) in [
            (
                "another branch",
                update("refs/heads/other", sha1(1), RefLog::AndReference),
                false,
                Some(MAIN),
            ),
            ("a detached HEAD", branch.clone(), false, None),
            (
                "an update that came from HEAD, which logs it already",
                branch.clone(),
                true,
                Some(MAIN),
            ),
            (
                "an edit of the reflog only",
                update(MAIN, sha1(1), RefLog::Only),
                false,
                Some(MAIN),
            ),
            (
                "a symbolic reference, which has no reflog",
                RefEdit::update(
                    name(MAIN),
                    Target::Symbolic(name("refs/heads/x")),
                    PreviousValue::Any,
                    "m",
                ),
                false,
                Some(MAIN),
            ),
        ] {
            assert!(
                git(&edit, via_head, referent, &[MAIN]).is_none(),
                "{description} doesn't change what HEAD logs"
            );
        }
    }

    #[test]
    fn like_in_git_editing_head_and_its_branch_independently_is_refused() {
        let edit = update(MAIN, sha1(1), RefLog::AndReference);
        let err =
            head_log(&edit, false, Some(MAIN), &["HEAD", MAIN], HeadLog::Git).expect_err("HEAD would change twice");
        assert!(err.is_conflict(), "{err:?}");
        assert!(
            format!("{err:?}").contains(
                "multiple updates for 'HEAD' (including one via its referent 'refs/heads/main') are not allowed"
            ),
            "it uses Git's words: {err:?}"
        );
        assert!(
            git(&edit, true, Some(MAIN), &["HEAD", MAIN]).is_none(),
            "which is fine if the branch edit stems from the edit of HEAD"
        );
    }

    #[test]
    fn like_in_git_an_update_that_expects_what_it_writes_is_an_update() {
        let edit = verification(MAIN, sha1(1));
        assert!(is_verification(&edit));
        assert!(
            git(&edit, false, Some(MAIN), &[MAIN]).is_some(),
            "Git logs it in HEAD, as it writes a value like any update"
        );
    }

    #[test]
    fn the_caller_can_decide_what_head_logs() {
        let edit = update(MAIN, sha1(1), RefLog::AndReference);
        assert!(
            head_log(&edit, false, Some(MAIN), &["HEAD", MAIN], HeadLog::CallerDecides)
                .expect("no conflict")
                .is_none(),
            "HEAD is edited by the transaction itself, which logs what it wants"
        );
        assert!(
            head_log(&edit, false, Some(MAIN), &[MAIN], HeadLog::CallerDecides)
                .expect("no conflict")
                .is_some(),
            "everything else is as in Git"
        );
        assert!(
            head_log(
                &verification(MAIN, sha1(1)),
                false,
                Some(MAIN),
                &[],
                HeadLog::CallerDecides
            )
            .expect("no conflict")
            .is_none(),
            "a verification doesn't change what HEAD logs"
        );
        assert!(
            !is_verification(&edit),
            "an edit that expects nothing isn't a verification"
        );
    }

    #[test]
    fn only_updates_to_the_value_that_exists_are_noops() {
        let edit = update(MAIN, sha1(1), RefLog::AndReference);
        assert!(is_noop_update(&edit, Some(&Target::Object(sha1(1)))));
        assert!(
            !is_noop_update(&edit, Some(&Target::Object(sha1(2)))),
            "the value changes"
        );
        assert!(!is_noop_update(&edit, None), "it is created");
        assert!(
            !is_noop_update(&edit, Some(&Target::Symbolic(name("refs/heads/x")))),
            "a symbolic reference changes to an object"
        );
        assert!(
            !is_noop_update(&update(MAIN, sha1(1), RefLog::Only), Some(&Target::Object(sha1(1)))),
            "a log entry is written regardless"
        );
        assert!(
            !is_noop_update(
                &RefEdit::update(
                    name(MAIN),
                    Target::Symbolic(name("refs/heads/x")),
                    PreviousValue::Any,
                    ""
                ),
                Some(&Target::Symbolic(name("refs/heads/x")))
            ),
            "symbolic references are always written"
        );
        assert!(!is_noop_update(&RefEdit::delete(name(MAIN), PreviousValue::Any), None));
    }
}
