//! Decide what a transaction writes, without touching any storage.
//!
//! The decisions follow Git's `prepare_single_update()` and `write_transaction_table()` of its reftable backend, with
//! the verification of expected values in the way the files store reports it, so that callers can recover from a failed
//! precondition without knowing how references are stored. What exists is answered by a [`Lookup`].
use gix_error::{ErrorExt, Result, bail, message, not_found, validation};
use gix_hash::ObjectId;
use gix_object::bstr::{BStr, BString};
use gix_reftable::record::RefValue;

use crate::{
    FullName, FullNameRef, Namespace, Target,
    file::transaction::prepare::{MustNotExist, ReferenceOutOfDate},
    store::WriteReflog,
    transaction::{Change, ExistingRefs, LogChange, PreviousValue, RefEdit, RefLog, verify_available},
};

/// What a transaction can ask about the references as they are, using their names as stored, so with their namespace.
pub(super) trait Lookup: ExistingRefs {
    /// The value of the reference `name`, or `None` if it doesn't exist.
    fn current(&self, name: &BStr) -> Result<Option<Target>>;
    /// The object the reference `name` leads to after following symbolic references, or `None` if it doesn't.
    fn resolve(&self, name: &BStr) -> Result<Option<ObjectId>>;
    /// Whether a reflog exists for `name`, even without entries.
    fn reflog_exists(&self, name: &BStr) -> Result<bool>;
    /// The update indices of the entries of the reflog of `name` that aren't deleted, including a marker entry.
    fn reflog_indices(&self, name: &BStr) -> Result<Vec<u64>>;
}

/// The settings the decisions depend on.
pub(super) struct Settings<'a> {
    pub hash: gix_hash::Kind,
    pub write_reflog: WriteReflog,
    /// The longest reflog message that is stored in bytes, which is half the block size.
    pub message_limit: usize,
    pub namespace: Option<&'a Namespace>,
}

impl Settings<'_> {
    /// The name `name` has in storage.
    pub fn stored(&self, name: &FullNameRef) -> BString {
        match self.namespace {
            Some(namespace) => namespace.clone().into_namespaced_name(name).0,
            None => name.as_bstr().to_owned(),
        }
    }

    /// The name `name` has in storage, for a name that is not known to be valid.
    pub fn stored_bstr(&self, name: &BStr) -> BString {
        self.stored(FullNameRef::new_unchecked(name))
    }
}

/// What to do with the reflog of a reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Log {
    /// Nothing, as no reflog is wanted for the reference.
    None,
    /// Append an entry by the committer, which is only known when the transaction is committed.
    Entry {
        old: ObjectId,
        new: ObjectId,
        /// The message in the form in which it is stored, without a final newline.
        message: BString,
    },
    /// Delete the entries with these update indices, as their reference is deleted.
    Delete(Vec<u64>),
}

/// An edit that has been verified, and what it takes to apply it.
#[derive(Debug, Clone)]
pub(super) struct Planned {
    /// The edit with the value that was found filled in as expected one, if there was any.
    pub edit: RefEdit,
    /// The name of the reference as stored.
    pub stored: BString,
    /// The record to write for the reference, or `None` if only its reflog changes or nothing needs to be done.
    pub record: Option<RefValue>,
    pub log: Log,
}

/// Return the name of the reference `HEAD` points to if it is a symbolic reference, and the edit that gives `HEAD` the
/// entry in its reflog which Git writes when the reference it points to is updated directly.
///
/// Edits are left alone if they don't change what `HEAD` leads to, or if `HEAD` is a part of the transaction already
/// because the edit comes from it, which is told by `HEAD` being a log-only edit. Anything else that involves both
/// is refused as Git does.
pub(super) fn head_log_edit(edits: &[RefEdit], head_referent: Option<&FullName>) -> Result<Option<RefEdit>> {
    let Some(referent) = head_referent else {
        return Ok(None);
    };
    let Some(edit) = edits
        .iter()
        .find(|edit| edit.name == *referent && edit.change_log_mode() != RefLog::Only)
    else {
        return Ok(None);
    };
    if let Some(head) = edits.iter().find(|edit| edit.name.as_bstr() == "HEAD") {
        if head.change_log_mode() == RefLog::Only {
            return Ok(None);
        }
        bail!(gix_error::conflict(format!(
            "multiple updates for 'HEAD' (including one via its referent '{referent}') are not allowed"
        )));
    }
    let head = FullName::try_from("HEAD").expect("valid name");
    Ok(Some(match &edit.change {
        Change::Update { log, new, .. } => RefEdit::update_with_log(
            head,
            new.clone(),
            PreviousValue::Any,
            LogChange {
                mode: RefLog::Only,
                ..log.clone()
            },
        ),
        Change::Delete { .. } => RefEdit::delete_with_log(head, PreviousValue::Any, RefLog::Only),
    }))
}

impl RefEdit {
    fn change_log_mode(&self) -> RefLog {
        match &self.change {
            Change::Update { log, .. } => log.mode,
            Change::Delete { log, .. } => *log,
        }
    }
}

/// Collapse runs of whitespace into single spaces and trim the result, which is how Git cleans a reflog message,
/// and keep at most `limit` bytes of it.
pub(super) fn normalize_message(message: &BStr, limit: usize) -> BString {
    let mut out = BString::default();
    let mut was_space = true;
    for byte in message.iter().copied() {
        let is_space = byte.is_ascii_whitespace() || byte == 0x0b;
        if was_space && is_space {
            continue;
        }
        was_space = is_space;
        out.push(if is_space { b' ' } else { byte });
    }
    while out.last() == Some(&b' ') {
        out.pop();
    }
    out.truncate(limit);
    out
}

/// Git's rule to write a reflog entry for `name` if nobody asked for it to be forced.
fn should_write_log(mode: WriteReflog, name: &BStr, lookup: &impl Lookup) -> Result<bool> {
    Ok(match mode {
        WriteReflog::Disable => false,
        WriteReflog::Always => true,
        WriteReflog::Normal => {
            name.starts_with(b"refs/heads/")
                || name.starts_with(b"refs/remotes/")
                || name.starts_with(b"refs/notes/")
                || name == "HEAD"
                || lookup.reflog_exists(name)?
        }
    })
}

/// The check of the expected value of an edit against what exists, with the errors of the files store.
fn check_expected(edit: &RefEdit, existing: Option<&Target>) -> Result<()> {
    let name = || edit.name.0.clone();
    let out_of_date = |previous: &Target, actual: &Target| {
        ReferenceOutOfDate {
            full_name: name(),
            actual: actual.clone(),
        }
        .and_raise(message!("Expected reference content {previous}"))
    };
    match &edit.change {
        Change::Delete { expected, .. } => match (expected, existing) {
            (PreviousValue::MustNotExist, _) => {
                bail!(validation(
                    "A reference can't be required not to exist while it is deleted"
                ))
            }
            (PreviousValue::ExistingMustMatch(_) | PreviousValue::Any, None)
            | (PreviousValue::MustExist | PreviousValue::Any, Some(_)) => Ok(()),
            (PreviousValue::MustExist | PreviousValue::MustExistAndMatch(_), None) => {
                bail!(not_found("The reference to delete must exist"))
            }
            (PreviousValue::MustExistAndMatch(previous) | PreviousValue::ExistingMustMatch(previous), Some(actual)) => {
                if previous == actual {
                    Ok(())
                } else {
                    Err(out_of_date(previous, actual))
                }
            }
        },
        Change::Update { expected, new, .. } => match (expected, existing) {
            (PreviousValue::Any, _)
            | (PreviousValue::MustExist, Some(_))
            | (PreviousValue::MustNotExist | PreviousValue::ExistingMustMatch(_), None) => Ok(()),
            (PreviousValue::MustExist, None) => bail!(not_found("The reference to update must exist")),
            // Even a reference that has the new value exists, and creating it is a duplicate write that the caller
            // wants to know about, as Git does.
            (PreviousValue::MustNotExist, Some(actual)) => Err(MustNotExist {
                full_name: name(),
                actual: actual.clone(),
            }
            .and_raise(message!("Expected the reference not to exist when writing {new}"))),
            (PreviousValue::MustExistAndMatch(previous) | PreviousValue::ExistingMustMatch(previous), Some(actual)) => {
                if previous == actual {
                    Ok(())
                } else {
                    Err(out_of_date(previous, actual))
                }
            }
            (PreviousValue::MustExistAndMatch(previous), None) => {
                bail!(not_found(format!("The reference must exist with content {previous}")))
            }
        },
    }
}

/// Verify `edits` against what exists and decide what to write for each, in their order. Edits that don't change anything
/// are left out, and so are deletions of references that don't exist.
///
/// Failing doesn't change anything.
pub(super) fn plan<L: Lookup>(edits: Vec<RefEdit>, settings: &Settings<'_>, lookup: &L) -> Result<Vec<Planned>> {
    let stored_names: Vec<_> = edits.iter().map(|edit| settings.stored(edit.name.as_ref())).collect();
    let mut sorted_names = stored_names.clone();
    sorted_names.sort();

    let mut planned = Vec::with_capacity(edits.len());
    for (mut edit, stored) in edits.into_iter().zip(stored_names) {
        let existing = lookup.current(stored.as_ref())?;
        check_expected(&edit, existing.as_ref()).map_err(|err| {
            err.and_raise(message("Could not prepare reference edit").with("reference", edit.name.0.clone()))
        })?;
        let log_only = edit.change_log_mode() == RefLog::Only;
        let existing_is_symbolic = matches!(existing, Some(Target::Symbolic(_)));
        let current_oid = match &existing {
            Some(Target::Object(id)) => *id,
            Some(Target::Symbolic(_)) => lookup
                .resolve(stored.as_ref())?
                .unwrap_or_else(|| ObjectId::null(settings.hash)),
            None => ObjectId::null(settings.hash),
        };

        // What a deletion or a log-only edit would do to something that isn't there is nothing.
        let nothing_to_delete = existing.is_none() && matches!(edit.change, Change::Delete { .. });
        // A reference that is already what it should become isn't touched, which includes not writing a reflog entry.
        let no_change = match (&edit.change, &existing) {
            (
                Change::Update {
                    new: Target::Object(new),
                    ..
                },
                Some(Target::Object(old)),
            ) => !log_only && new == old,
            _ => false,
        };
        let value_before = existing.clone();
        if let Some(existing) = existing {
            match &mut edit.change {
                Change::Update { expected, .. } | Change::Delete { expected, .. } => {
                    *expected = PreviousValue::MustExistAndMatch(existing);
                }
            }
        }
        if nothing_to_delete || no_change {
            planned.push(Planned {
                edit,
                stored,
                record: None,
                log: Log::None,
            });
            continue;
        }

        if value_before.is_none() && !log_only && matches!(edit.change, Change::Update { .. }) {
            verify_available(stored.as_ref(), &sorted_names, lookup)?;
        }

        let record = if log_only {
            None
        } else {
            Some(match &edit.change {
                Change::Update {
                    new: Target::Object(id),
                    ..
                } => RefValue::Val1(*id),
                Change::Update {
                    new: Target::Symbolic(target),
                    ..
                } => RefValue::Symref(target.0.clone()),
                Change::Delete { .. } => RefValue::Deletion,
            })
        };

        let log = match &edit.change {
            // The reflog of a deleted reference is deleted, whatever is configured.
            Change::Delete { .. } if !existing_is_symbolic => Log::Delete(lookup.reflog_indices(stored.as_ref())?),
            Change::Update { log, .. } if wants_entry(log, settings, stored.as_ref(), lookup)? => {
                entry(&edit, current_oid, settings, lookup)?
            }
            Change::Delete { .. } if should_write_log(settings.write_reflog, stored.as_ref(), lookup)? => {
                entry(&edit, current_oid, settings, lookup)?
            }
            _ => Log::None,
        };
        planned.push(Planned {
            edit,
            stored,
            record,
            log,
        });
    }
    Ok(planned)
}

/// Whether a reflog entry is written for an update of the reference `stored`.
fn wants_entry(log: &LogChange, settings: &Settings<'_>, stored: &BStr, lookup: &impl Lookup) -> Result<bool> {
    if settings.write_reflog == WriteReflog::Disable {
        return Ok(false);
    }
    Ok(log.force_create_reflog || should_write_log(settings.write_reflog, stored, lookup)?)
}

/// The reflog entry for `edit`, or none if the new value of a symbolic reference can't be told.
fn entry(edit: &RefEdit, old: ObjectId, settings: &Settings<'_>, lookup: &impl Lookup) -> Result<Log> {
    let (new, message) = match &edit.change {
        Change::Update { new, log, .. } => (
            match new {
                Target::Object(id) => *id,
                // Dangling symbolic references don't get an entry, as in Git.
                Target::Symbolic(target) => match lookup.resolve(settings.stored(target.as_ref()).as_ref())? {
                    Some(id) => id,
                    None => return Ok(Log::None),
                },
            },
            log.message.as_ref(),
        ),
        Change::Delete { .. } => (ObjectId::null(settings.hash), BStr::new(b"")),
    };
    Ok(Log::Entry {
        old,
        new,
        message: normalize_message(message, settings.message_limit),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gix_error::Error;

    use super::*;

    const ID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn id(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex.as_bytes()).expect("valid")
    }

    fn name(name: &str) -> FullName {
        FullName::try_from(name).expect("valid name")
    }

    #[derive(Default)]
    struct World {
        refs: BTreeMap<BString, Target>,
        logs: BTreeMap<BString, Vec<u64>>,
    }

    impl World {
        fn with(mut self, name_: &str, target: Target) -> Self {
            self.refs.insert(name_.into(), target);
            self
        }
        fn with_log(mut self, name_: &str, indices: &[u64]) -> Self {
            self.logs.insert(name_.into(), indices.to_vec());
            self
        }
    }

    impl ExistingRefs for World {
        fn exists(&self, name: &BStr) -> Result<bool> {
            Ok(self.refs.contains_key(name))
        }
        fn any_below(&self, directory: &BStr) -> Result<Option<BString>> {
            Ok(self.refs.keys().find(|key| key.starts_with(directory)).cloned())
        }
    }

    impl Lookup for World {
        fn current(&self, name: &BStr) -> Result<Option<Target>> {
            Ok(self.refs.get(name).cloned())
        }
        fn resolve(&self, name: &BStr) -> Result<Option<ObjectId>> {
            let mut name = name.to_owned();
            for _ in 0..5 {
                match self.refs.get(&name) {
                    Some(Target::Object(id)) => return Ok(Some(*id)),
                    Some(Target::Symbolic(target)) => name = target.0.clone(),
                    None => return Ok(None),
                }
            }
            Ok(None)
        }
        fn reflog_exists(&self, name: &BStr) -> Result<bool> {
            Ok(self.logs.contains_key(name))
        }
        fn reflog_indices(&self, name: &BStr) -> Result<Vec<u64>> {
            Ok(self.logs.get(name).cloned().unwrap_or_default())
        }
    }

    fn settings(write_reflog: WriteReflog) -> Settings<'static> {
        Settings {
            hash: gix_hash::Kind::Sha1,
            write_reflog,
            message_limit: 2048,
            namespace: None,
        }
    }

    fn update(name_: &str, new: &str, expected: PreviousValue) -> RefEdit {
        RefEdit::update(
            name(name_),
            Target::Object(id(new)),
            expected,
            "  a  message\n with\tspace ",
        )
    }

    fn only(planned: Vec<Planned>) -> Planned {
        assert_eq!(planned.len(), 1);
        planned.into_iter().next().expect("one")
    }

    fn failure(result: Result<Vec<Planned>>) -> Error {
        result.expect_err("planning fails")
    }

    #[test]
    fn messages_collapse_whitespace_and_are_cut_to_the_limit() {
        assert_eq!(
            normalize_message(" \t a   b\n\nc \n".into(), 100),
            "a b c",
            "whitespace runs become one space, and the ends are trimmed"
        );
        assert_eq!(normalize_message("abcdef".into(), 3), "abc", "the limit is in bytes");
    }

    #[test]
    fn creating_a_reference_writes_it_and_its_log_for_branches_only() {
        let world = World::default();
        for (reference, logged) in [("refs/heads/a", true), ("refs/tags/a", false), ("refs/notes/a", true)] {
            let planned = only(
                plan(
                    vec![update(reference, ID_A, PreviousValue::MustNotExist)],
                    &settings(WriteReflog::Normal),
                    &world,
                )
                .expect("plans"),
            );
            assert_eq!(planned.record, Some(RefValue::Val1(id(ID_A))));
            match (&planned.log, logged) {
                (Log::Entry { old, new, message }, true) => {
                    assert!(old.is_null());
                    assert_eq!(*new, id(ID_A));
                    assert_eq!(message, "a message with space", "the message is normalized");
                }
                (Log::None, false) => {}
                (log, _) => panic!("{reference}: unexpected {log:?}"),
            }
        }
    }

    #[test]
    fn reflog_modes_decide_for_references_outside_of_the_usual_places() {
        let world = World::default()
            .with("refs/tags/t", Target::Object(id(ID_A)))
            .with_log("refs/tags/t", &[1]);
        for (mode, forced, expected_entry) in [
            (WriteReflog::Normal, false, true),
            (WriteReflog::Always, false, true),
            (WriteReflog::Disable, false, false),
            (WriteReflog::Disable, true, false),
        ] {
            let mut edit = update("refs/tags/t", ID_B, PreviousValue::Any);
            if let Change::Update { log, .. } = &mut edit.change {
                log.force_create_reflog = forced;
            }
            let planned = only(plan(vec![edit], &settings(mode), &world).expect("plans"));
            assert_eq!(
                matches!(planned.log, Log::Entry { .. }),
                expected_entry,
                "{mode:?}, forced: {forced}: an existing log is appended to unless logging is disabled"
            );
        }
        let world = World::default().with("refs/tags/u", Target::Object(id(ID_A)));
        let mut edit = update("refs/tags/u", ID_B, PreviousValue::Any);
        assert!(matches!(
            only(plan(vec![edit.clone()], &settings(WriteReflog::Normal), &world).expect("plans")).log,
            Log::None
        ));
        if let Change::Update { log, .. } = &mut edit.change {
            log.force_create_reflog = true;
        }
        assert!(
            matches!(
                only(plan(vec![edit], &settings(WriteReflog::Normal), &world).expect("plans")).log,
                Log::Entry { .. }
            ),
            "forcing creates a log"
        );
    }

    #[test]
    fn an_update_to_the_same_value_changes_nothing_and_leaves_no_log_entry() {
        let world = World::default().with("refs/heads/a", Target::Object(id(ID_A)));
        let planned = only(
            plan(
                vec![update("refs/heads/a", ID_A, PreviousValue::Any)],
                &settings(WriteReflog::Normal),
                &world,
            )
            .expect("plans"),
        );
        assert_eq!((planned.record, planned.log), (None, Log::None));
    }

    #[test]
    fn deleting_a_reference_deletes_all_its_log_entries_and_deleting_nothing_is_a_no_op() {
        let world = World::default()
            .with("refs/heads/a", Target::Object(id(ID_A)))
            .with_log("refs/heads/a", &[7, 3, 1]);
        let planned = only(
            plan(
                vec![RefEdit::delete(name("refs/heads/a"), PreviousValue::Any)],
                &settings(WriteReflog::Disable),
                &world,
            )
            .expect("plans"),
        );
        assert_eq!(planned.record, Some(RefValue::Deletion));
        assert_eq!(planned.log, Log::Delete(vec![7, 3, 1]));

        let planned = only(
            plan(
                vec![RefEdit::delete(name("refs/heads/missing"), PreviousValue::Any)],
                &settings(WriteReflog::Normal),
                &world,
            )
            .expect("plans"),
        );
        assert_eq!((planned.record, planned.log), (None, Log::None));
    }

    #[test]
    fn expected_values_fail_with_the_errors_of_the_files_store() {
        let world = World::default().with("refs/heads/a", Target::Object(id(ID_A)));
        let s = settings(WriteReflog::Normal);

        let err = failure(plan(
            vec![update(
                "refs/heads/a",
                ID_B,
                PreviousValue::MustExistAndMatch(Target::Object(id(ID_B))),
            )],
            &s,
            &world,
        ));
        let out_of_date = err.downcast_any_ref::<ReferenceOutOfDate>().expect("out of date");
        assert_eq!(out_of_date.actual, Target::Object(id(ID_A)));

        let err = failure(plan(
            vec![update("refs/heads/a", ID_B, PreviousValue::MustNotExist)],
            &s,
            &world,
        ));
        assert!(err.downcast_any_ref::<MustNotExist>().is_some());
        let err = failure(plan(
            vec![update("refs/heads/a", ID_A, PreviousValue::MustNotExist)],
            &s,
            &world,
        ));
        assert_eq!(
            err.downcast_any_ref::<MustNotExist>().expect("exists").actual,
            Target::Object(id(ID_A)),
            "creating what is there already is refused, as in Git, and the error tells what is there"
        );

        for edit in [
            update("refs/heads/missing", ID_B, PreviousValue::MustExist),
            update(
                "refs/heads/missing",
                ID_B,
                PreviousValue::MustExistAndMatch(Target::Object(id(ID_A))),
            ),
            RefEdit::delete(name("refs/heads/missing"), PreviousValue::MustExist),
        ] {
            assert!(failure(plan(vec![edit], &s, &world)).is_not_found());
        }
    }

    #[test]
    fn a_name_that_would_be_the_directory_of_another_reference_conflicts() {
        let world = World::default().with("refs/heads/a", Target::Object(id(ID_A)));
        let s = settings(WriteReflog::Normal);
        let err = failure(plan(
            vec![update("refs/heads/a/b", ID_B, PreviousValue::Any)],
            &s,
            &world,
        ));
        assert!(err.downcast_any_ref::<crate::transaction::NameConflict>().is_some());
        let err = failure(plan(
            vec![
                update("refs/heads/q", ID_B, PreviousValue::Any),
                update("refs/heads/q/r", ID_B, PreviousValue::Any),
            ],
            &s,
            &world,
        ));
        assert!(
            err.downcast_any_ref::<crate::transaction::NameConflict>()
                .expect("conflict")
                .in_transaction,
            "two edits of one transaction can't both be written"
        );
    }

    #[test]
    fn updating_the_branch_head_points_to_adds_a_log_only_edit_for_head() {
        let edits = [update("refs/heads/main", ID_B, PreviousValue::Any)];
        let head = head_log_edit(&edits, Some(&name("refs/heads/main")))
            .expect("fine")
            .expect("added");
        assert_eq!(head.name.as_bstr(), "HEAD");
        assert_eq!(head.change_log_mode(), RefLog::Only);
        assert_eq!(head.change.new_value(), edits[0].change.new_value());

        assert!(
            head_log_edit(&edits, Some(&name("refs/heads/other")))
                .expect("fine")
                .is_none()
        );
        assert!(head_log_edit(&edits, None).expect("fine").is_none(), "HEAD is detached");

        let deletion = [RefEdit::delete(name("refs/heads/main"), PreviousValue::Any)];
        let head = head_log_edit(&deletion, Some(&name("refs/heads/main")))
            .expect("fine")
            .expect("added");
        assert!(matches!(head.change, Change::Delete { log: RefLog::Only, .. }));

        let edits = [
            update("refs/heads/main", ID_B, PreviousValue::Any),
            update("HEAD", ID_B, PreviousValue::Any),
        ];
        let err = head_log_edit(&edits, Some(&name("refs/heads/main"))).expect_err("both are edited");
        assert!(err.to_string().contains("multiple updates for 'HEAD'"), "{err}");

        let mut split = update("HEAD", ID_B, PreviousValue::Any);
        if let Change::Update { log, .. } = &mut split.change {
            log.mode = RefLog::Only;
        }
        let edits = [update("refs/heads/main", ID_B, PreviousValue::Any), split];
        assert!(
            head_log_edit(&edits, Some(&name("refs/heads/main")))
                .expect("fine")
                .is_none(),
            "an edit that comes from splitting HEAD already logs it"
        );
    }

    #[test]
    fn the_head_entry_keeps_the_value_of_the_branch_as_its_old_one_and_is_written_even_if_nothing_changes() {
        let world = World::default()
            .with("HEAD", Target::Symbolic(name("refs/heads/main")))
            .with("refs/heads/main", Target::Object(id(ID_A)));
        let edits = vec![update("refs/heads/main", ID_A, PreviousValue::Any)];
        let head = head_log_edit(&edits, Some(&name("refs/heads/main")))
            .expect("fine")
            .expect("added");
        let mut all = edits;
        all.push(head);
        let planned = plan(all, &settings(WriteReflog::Normal), &world).expect("plans");
        assert_eq!(
            (planned[0].record.clone(), planned[0].log.clone()),
            (None, Log::None),
            "the branch isn't touched"
        );
        assert_eq!(planned[1].record, None, "HEAD itself is not rewritten");
        assert_eq!(
            planned[1].log,
            Log::Entry {
                old: id(ID_A),
                new: id(ID_A),
                message: "a message with space".into()
            },
            "but its log gets an entry, with the value of the branch as old one"
        );
    }

    #[test]
    fn deleting_the_branch_head_points_to_logs_the_deletion_in_head_and_keeps_the_log_of_head() {
        let world = World::default()
            .with("HEAD", Target::Symbolic(name("refs/heads/main")))
            .with("refs/heads/main", Target::Object(id(ID_A)))
            .with_log("refs/heads/main", &[4])
            .with_log("HEAD", &[4]);
        let delete = RefEdit::delete(name("refs/heads/main"), PreviousValue::Any);
        let head = head_log_edit(std::slice::from_ref(&delete), Some(&name("refs/heads/main")))
            .expect("fine")
            .expect("added");
        let planned = plan(vec![delete, head], &settings(WriteReflog::Normal), &world).expect("plans");
        assert_eq!(planned[0].log, Log::Delete(vec![4]));
        assert!(matches!(&planned[1].log, Log::Entry { old, new, .. } if *old == id(ID_A) && new.is_null()));
    }

    #[test]
    fn a_dangling_symbolic_reference_update_writes_no_log_entry() {
        let world = World::default();
        let edit = RefEdit::update(
            name("refs/heads/sym"),
            Target::Symbolic(name("refs/heads/nothing")),
            PreviousValue::Any,
            "m",
        );
        let planned = only(plan(vec![edit], &settings(WriteReflog::Normal), &world).expect("plans"));
        assert_eq!(planned.record, Some(RefValue::Symref("refs/heads/nothing".into())));
        assert_eq!(planned.log, Log::None);

        let world = World::default().with("refs/heads/real", Target::Object(id(ID_A)));
        let edit = RefEdit::update(
            name("refs/heads/sym"),
            Target::Symbolic(name("refs/heads/real")),
            PreviousValue::Any,
            "m",
        );
        let planned = only(plan(vec![edit], &settings(WriteReflog::Normal), &world).expect("plans"));
        assert!(
            matches!(planned.log, Log::Entry { new, .. } if new == id(ID_A)),
            "the entry has the resolved id"
        );
    }
}
