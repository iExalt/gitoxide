//! Editing references that are stored in reftables, with Git as the judge of what was written: it reads the stacks the
//! store changed, and its `refs verify` and `fsck` have to agree that nothing is wrong with them.
//!
//! The fixture is the one of the reading tests, a repository with a linked worktree `w1` whose own stack holds the
//! references private to it. Object ids are those of its three commits, which are `c1`, `c2` and `c3`.
use std::{cell::RefCell, rc::Rc};

use gix_date::parse::TimeBuf;
use gix_error::ErrorExt;
use gix_lock::acquire::Fail;
use gix_ref::{
    FullName, Target,
    reftable::Store,
    transaction::{
        PreviousValue, RefEdit,
        commit_failure::{NothingApplied, Progress, Uncertain},
        observe::{Event, Observer, State, Veto, Vetoed},
    },
    view::transaction::{LockOptions, MissingCommitter, NameConflict, Outcome, ReferenceOutOfDate},
};
use gix_testtools::TestResult;

use crate::{fixture_hash_kind, scripted_fixture_writable};

struct Fixture {
    dir: gix_testtools::tempfile::TempDir,
    c1: gix_hash::ObjectId,
    c2: gix_hash::ObjectId,
    c3: gix_hash::ObjectId,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let dir = scripted_fixture_writable("make_reftable_repo.sh")?;
        let rev = |spec: &str| -> TestResult<gix_hash::ObjectId> {
            Ok(gix_hash::ObjectId::from_hex(
                gix_testtools::git(dir.path().join("repo"), &format!("rev-parse {spec}"))?
                    .trim()
                    .as_bytes(),
            )?)
        };
        let fixture = Fixture {
            c1: rev(":/c1")?,
            c2: rev(":/c2")?,
            c3: rev(":/c3")?,
            dir,
        };
        // The copy of the fixture has to find its way back to the linked worktree, which still points to where it was made.
        fixture.git("repo", "worktree repair ../w1")?;
        // The symbolic references that lead nowhere are there to be read, and keep `git fsck` from judging the rest.
        for broken in ["dangling", "cycle-a", "cycle-b"] {
            fixture.git("repo", &format!("symbolic-ref --delete refs/heads/{broken}"))?;
        }
        Ok(fixture)
    }

    fn main(&self) -> Store {
        Store::at(self.dir.path().join("repo/.git"), fixture_hash_kind())
    }

    fn worktree(&self) -> Store {
        Store::for_linked_worktree(
            self.dir.path().join("repo/.git/worktrees/w1"),
            self.dir.path().join("repo/.git"),
            fixture_hash_kind(),
        )
    }

    fn git(&self, sub_directory: &str, args: &str) -> TestResult<String> {
        Ok(gix_testtools::git(self.dir.path().join(sub_directory), args)?)
    }

    /// The names of the tables in the stack at `dir`, relative to the repository.
    fn tables(&self, dir: &str) -> TestResult<Vec<String>> {
        let list = std::fs::read_to_string(self.dir.path().join(dir).join("tables.list"))?;
        Ok(list.lines().map(ToOwned::to_owned).collect())
    }

    /// Fail unless Git considers what is stored valid.
    fn git_agrees_that_nothing_is_wrong(&self) -> TestResult {
        self.git("repo", "refs verify")?;
        self.git("repo", "fsck --no-dangling")?;
        Ok(())
    }
}

fn committer() -> gix_actor::Signature {
    gix_actor::Signature {
        name: "Committer".into(),
        email: "committer@example.com".into(),
        time: gix_date::parse_header("1700100000 +0130").expect("valid"),
    }
}

fn name(name: &str) -> FullName {
    FullName::try_from(name).expect("valid name")
}

fn lock() -> LockOptions {
    LockOptions::new(Fail::Immediately, Fail::Immediately)
}

fn commit(store: &Store, edits: Vec<RefEdit>) -> gix_error::Result<Outcome> {
    let store = gix_ref::Store::from(store.clone());
    store
        .as_ref()
        .transaction()
        .prepare(edits, lock())?
        .commit(committer().to_ref(&mut TimeBuf::default()))
}

fn create(reference: &str, id: gix_hash::ObjectId, message: &str) -> RefEdit {
    RefEdit::update(
        name(reference),
        Target::Object(id),
        PreviousValue::MustNotExist,
        message,
    )
}

fn update(reference: &str, id: gix_hash::ObjectId, expected: PreviousValue, message: &str) -> RefEdit {
    RefEdit::update(name(reference), Target::Object(id), expected, message)
}

fn id_of(store: &Store, reference: &str) -> TestResult<gix_hash::ObjectId> {
    Ok(store.find(reference)?.target.try_id().expect("direct").to_owned())
}

/// The messages of the reflog of `reference` as Git shows them, newest first.
fn reflog_messages(fixture: &Fixture, sub_directory: &str, reference: &str) -> TestResult<Vec<String>> {
    let output = gix_testtools::git_command(fixture.dir.path().join(sub_directory))
        .args(["reflog", "show", "--format=%gs", reference, "--"])
        .output()?;
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .map(ToOwned::to_owned)
        .collect())
}

#[test]
fn what_the_store_writes_is_what_git_reads() -> TestResult {
    let f = Fixture::new()?;
    let store = f.main();
    let tag = name("refs/tags/lightweight");
    let outcome = commit(
        &store,
        vec![
            create("refs/heads/new", f.c2, "branch: Created from c2"),
            update(
                "refs/heads/side",
                f.c1,
                PreviousValue::MustExistAndMatch(Target::Object(f.c3)),
                "moved   back\n\twith   odd whitespace ",
            ),
            RefEdit::delete(
                name("refs/heads/feature"),
                PreviousValue::MustExistAndMatch(Target::Object(f.c1)),
            ),
            RefEdit::update(tag.clone(), Target::Object(f.c3), PreviousValue::MustNotExist, "a tag"),
            RefEdit::update(
                name("refs/heads/pointer"),
                Target::Symbolic(name("refs/heads/new")),
                PreviousValue::MustNotExist,
                "symbolic",
            ),
        ],
    )?;
    assert_eq!(
        outcome.edits.len(),
        5,
        "no symbolic reference is dereferenced, and HEAD is not involved"
    );

    assert_eq!(
        f.git("repo", "for-each-ref --format=%(refname)=%(objectname)=%(symref) refs/heads/new refs/heads/side refs/heads/feature refs/tags/lightweight refs/heads/pointer")?,
        format!(
            "refs/heads/new={}=\nrefs/heads/pointer={}=refs/heads/new\nrefs/heads/side={}=\nrefs/tags/lightweight={}=\n",
            f.c2, f.c2, f.c1, f.c3
        ),
        "Git sees the created, moved and symbolic references, and not the deleted one"
    );
    assert_eq!(
        reflog_messages(&f, "repo", "refs/heads/new")?,
        ["branch: Created from c2"],
        "branches get a log"
    );
    assert_eq!(
        reflog_messages(&f, "repo", "refs/heads/side")?[0],
        "moved back with odd whitespace",
        "messages are cleaned up like Git does"
    );
    assert!(
        reflog_messages(&f, "repo", "refs/heads/feature")?.is_empty(),
        "the log of a deleted reference is deleted too"
    );
    assert!(
        reflog_messages(&f, "repo", "refs/tags/lightweight")?.is_empty(),
        "tags don't get a log unless there is one"
    );
    let reflog = f.git("repo", "reflog show --format=%gn|%ge|%gd --date=raw refs/heads/new --")?;
    assert_eq!(
        reflog.trim(),
        "Committer|committer@example.com|new@{1700100000 +0130}",
        "the committer and the time zone are recorded as given"
    );
    f.git_agrees_that_nothing_is_wrong()?;
    assert_eq!(
        id_of(&store, "refs/heads/side")?,
        f.c1,
        "the store itself sees its own writes"
    );
    Ok(())
}

#[test]
fn updating_the_branch_head_points_to_logs_it_for_head_as_well_even_if_nothing_changes() -> TestResult {
    let f = Fixture::new()?;
    let store = f.main();
    let head_log_before = reflog_messages(&f, "repo", "HEAD")?.len();
    let main_log_before = reflog_messages(&f, "repo", "refs/heads/main")?.len();

    commit(
        &store,
        vec![update(
            "refs/heads/main",
            f.c2,
            PreviousValue::Any,
            "reset: moving to c2",
        )],
    )?;
    assert_eq!(
        reflog_messages(&f, "repo", "HEAD")?[0],
        "reset: moving to c2",
        "HEAD's log gets the entry as HEAD points to the branch"
    );
    assert_eq!(
        reflog_messages(&f, "repo", "refs/heads/main")?[0],
        "reset: moving to c2"
    );

    commit(
        &store,
        vec![update(
            "refs/heads/main",
            f.c2,
            PreviousValue::Any,
            "reset: to the same value",
        )],
    )?;
    assert_eq!(
        reflog_messages(&f, "repo", "refs/heads/main")?.len(),
        main_log_before + 1,
        "an update that changes nothing writes no entry for the branch"
    );
    assert_eq!(
        reflog_messages(&f, "repo", "HEAD")?[0],
        "reset: to the same value",
        "but HEAD logs it, as Git does"
    );
    assert_eq!(reflog_messages(&f, "repo", "HEAD")?.len(), head_log_before + 2);

    let err = commit(
        &store,
        vec![
            update("refs/heads/main", f.c3, PreviousValue::Any, "a"),
            update("HEAD", f.c3, PreviousValue::Any, "b"),
        ],
    )
    .expect_err("HEAD is edited directly as well as via its referent");
    assert!(err.to_string().contains("multiple updates for 'HEAD'"), "{err:?}");
    f.git_agrees_that_nothing_is_wrong()?;
    Ok(())
}

#[test]
fn a_reference_that_was_deleted_can_be_created_with_a_log_that_starts_afresh() -> TestResult {
    let f = Fixture::new()?;
    let store = f.main();
    assert_eq!(reflog_messages(&f, "repo", "refs/heads/side")?.len(), 2);
    commit(
        &store,
        vec![RefEdit::delete(name("refs/heads/side"), PreviousValue::MustExist)],
    )?;
    assert!(store.try_find("refs/heads/side")?.is_none());
    assert!(reflog_messages(&f, "repo", "refs/heads/side")?.is_empty());
    commit(&store, vec![create("refs/heads/side", f.c1, "branch: recreated")])?;
    assert_eq!(
        reflog_messages(&f, "repo", "refs/heads/side")?,
        ["branch: recreated"],
        "only the new entry is there"
    );
    f.git_agrees_that_nothing_is_wrong()?;
    Ok(())
}

#[test]
fn expected_values_are_checked_against_what_is_stored_when_the_stack_is_locked() -> TestResult {
    let f = Fixture::new()?;
    let store = f.main();
    assert_eq!(id_of(&store, "refs/heads/side")?, f.c3);
    // Git moves the reference after the store has read it.
    f.git("repo", &format!("update-ref refs/heads/side {}", f.c1))?;

    let err = commit(
        &store,
        vec![update(
            "refs/heads/side",
            f.c2,
            PreviousValue::MustExistAndMatch(Target::Object(f.c3)),
            "stale",
        )],
    )
    .expect_err("the reference isn't where it was read");
    let out_of_date = err
        .downcast_any_ref::<ReferenceOutOfDate>()
        .expect("a conflict that is recoverable");
    assert_eq!(
        out_of_date.actual,
        Target::Object(f.c1),
        "what Git wrote is what the failure reports"
    );
    assert!(err.is_conflict());
    assert_eq!(
        f.git("repo", "rev-parse refs/heads/side")?.trim(),
        f.c1.to_string(),
        "nothing was changed"
    );

    let err = commit(&store, vec![create("refs/heads/side", f.c2, "exists")]).expect_err("it exists");
    assert!(
        err.downcast_any_ref::<gix_ref::view::transaction::MustNotExist>()
            .is_some(),
        "{err:?}"
    );
    let err = commit(
        &store,
        vec![RefEdit::delete(name("refs/heads/nothing"), PreviousValue::MustExist)],
    )
    .expect_err("there is nothing to delete");
    assert!(err.is_not_found(), "{err:?}");
    assert!(
        !f.dir.path().join("repo/.git/reftable/tables.list.lock").exists(),
        "failing releases the lock"
    );
    Ok(())
}

#[test]
fn names_that_would_be_directories_of_others_conflict_in_the_stack_and_within_the_transaction() -> TestResult {
    let f = Fixture::new()?;
    let store = f.main();
    for (edits, in_transaction) in [
        (vec![create("refs/heads/main/below", f.c1, "x")], false),
        (
            vec![
                create("refs/heads/pointer", f.c1, "x"),
                create("refs/heads/pointer/below", f.c1, "x"),
            ],
            true,
        ),
    ] {
        let err = commit(&store, edits).expect_err("one would be the directory of the other");
        assert_eq!(
            err.downcast_any_ref::<NameConflict>()
                .expect("a name conflict")
                .in_transaction,
            in_transaction
        );
    }
    let err = commit(&store, vec![create("refs/heads", f.c1, "x")]).expect_err("it is a directory");
    assert!(err.downcast_any_ref::<NameConflict>().is_some(), "{err:?}");
    f.git_agrees_that_nothing_is_wrong()?;
    Ok(())
}

/// Collect what an observer is told, as the state and the names of the updates.
#[derive(Clone, Default)]
struct Log(Rc<RefCell<Vec<Seen>>>);

/// A state and the names of the updates it was reported with.
type Seen = (State, Vec<String>);

struct Watch {
    log: Log,
    veto_in: Option<State>,
    on_prepared: Option<Box<dyn Fn()>>,
}

impl Observer for Watch {
    fn observe(&mut self, event: &Event<'_>) -> Result<(), Veto> {
        assert_eq!(
            event.scope,
            gix_ref::transaction::observe::Scope::Transaction,
            "reftables have no transaction on packed-refs"
        );
        self.log.0.borrow_mut().push((
            event.state,
            event.updates.iter().map(|update| update.name.to_string()).collect(),
        ));
        if event.state == State::Prepared
            && let Some(hook) = &self.on_prepared
        {
            hook();
        }
        if Some(event.state) == self.veto_in {
            Err(Veto)
        } else {
            Ok(())
        }
    }
}

fn observed(store: &Store, edits: Vec<RefEdit>, watch: Watch) -> gix_error::Result<Outcome> {
    let store = gix_ref::Store::from(store.clone());
    store
        .as_ref()
        .transaction()
        .observer(watch)
        .prepare(edits, lock())?
        .commit(committer().to_ref(&mut TimeBuf::default()))
}

#[test]
fn an_observer_sees_one_set_of_states_like_git_and_may_veto_before_anything_changes() -> TestResult {
    let f = Fixture::new()?;
    let store = f.main();
    let tables_before = f.tables("repo/.git/reftable")?;
    let edits = || {
        vec![
            create("refs/heads/observed", f.c2, "o"),
            RefEdit::delete(name("refs/heads/feature"), PreviousValue::Any),
            update("refs/heads/main", f.c1, PreviousValue::Any, "moved"),
        ]
    };
    let (preparing, prepared) = (
        vec!["refs/heads/observed", "refs/heads/feature", "refs/heads/main"],
        // HEAD's log-only edit is not an update, as Git's hook doesn't see these either.
        vec!["refs/heads/observed", "refs/heads/feature", "refs/heads/main"],
    );

    let log = Log::default();
    observed(
        &store,
        edits(),
        Watch {
            log: log.clone(),
            veto_in: None,
            on_prepared: None,
        },
    )?;
    let states: Vec<_> = log.0.borrow().iter().map(|(state, _)| *state).collect();
    assert_eq!(states, [State::Preparing, State::Prepared, State::Committed]);
    for ((_, names), expected) in log.0.borrow().iter().zip([&preparing, &prepared, &prepared]) {
        assert_eq!(names, expected);
    }
    assert_ne!(
        f.tables("repo/.git/reftable")?,
        tables_before,
        "the transaction was committed"
    );

    for (veto_in, expected_states) in [
        (State::Preparing, vec![State::Preparing, State::Aborted]),
        (State::Prepared, vec![State::Preparing, State::Prepared, State::Aborted]),
    ] {
        let tables = f.tables("repo/.git/reftable")?;
        let log = Log::default();
        let err = observed(
            &store,
            vec![update("refs/heads/main", f.c3, PreviousValue::Any, "vetoed")],
            Watch {
                log: log.clone(),
                veto_in: Some(veto_in),
                on_prepared: None,
            },
        )
        .expect_err("the observer refuses");
        assert_eq!(err.downcast_any_ref::<Vetoed>().expect("marked").state, veto_in);
        let states: Vec<_> = log.0.borrow().iter().map(|(state, _)| *state).collect();
        assert_eq!(states, expected_states);
        assert_eq!(
            f.tables("repo/.git/reftable")?,
            tables,
            "{veto_in:?}: nothing was written"
        );
        assert!(
            !f.dir.path().join("repo/.git/reftable/tables.list.lock").exists(),
            "{veto_in:?}: the lock was released"
        );
        assert_ne!(id_of(&store, "refs/heads/main")?, f.c3);
    }

    let log = Log::default();
    let store_for_drop = gix_ref::Store::from(f.main());
    let prepared = store_for_drop
        .as_ref()
        .transaction()
        .observer(Watch {
            log: log.clone(),
            veto_in: None,
            on_prepared: None,
        })
        .prepare(vec![create("refs/heads/dropped", f.c1, "d")], lock())?;
    drop(prepared);
    let states: Vec<_> = log.0.borrow().iter().map(|(state, _)| *state).collect();
    assert_eq!(
        states,
        [State::Preparing, State::Prepared, State::Aborted],
        "dropping aborts"
    );
    Ok(())
}

#[test]
fn a_transaction_in_a_linked_worktree_changes_the_stacks_of_the_worktree_and_the_repository() -> TestResult {
    let f = Fixture::new()?;
    let store = f.worktree();
    let main_tables = f.tables("repo/.git/reftable")?;
    let worktree_tables = f.tables("repo/.git/worktrees/w1/reftable")?;

    // HEAD of the worktree points to `w1-branch`, so updating it through HEAD edits the branch in the shared stack and
    // logs HEAD in the stack of the worktree.
    commit(
        &store,
        vec![
            RefEdit::update(
                name("HEAD"),
                Target::Object(f.c1),
                PreviousValue::Any,
                "reset: moving to c1",
            )
            .with_deref(true),
            create("refs/worktree/private", f.c3, "private"),
        ],
    )?;
    assert_eq!(f.git("w1", "rev-parse HEAD")?.trim(), f.c1.to_string());
    assert_eq!(
        f.git("repo", "rev-parse refs/heads/w1-branch")?.trim(),
        f.c1.to_string(),
        "the branch is shared"
    );
    assert_eq!(f.git("w1", "rev-parse refs/worktree/private")?.trim(), f.c3.to_string());
    assert!(
        f.git("repo", "rev-parse refs/worktree/private").is_err(),
        "a reference private to the worktree is not in the main stack"
    );
    assert_eq!(reflog_messages(&f, "w1", "HEAD")?[0], "reset: moving to c1");
    assert_ne!(
        f.tables("repo/.git/reftable")?,
        main_tables,
        "the shared stack was changed, which compaction may have merged tables of"
    );
    assert_ne!(
        f.tables("repo/.git/worktrees/w1/reftable")?,
        worktree_tables,
        "and so was the one of the worktree"
    );
    f.git_agrees_that_nothing_is_wrong()?;
    f.git("w1", "refs verify")?;

    // The main worktree can edit the references of another one by their qualified names.
    commit(
        &f.main(),
        vec![create(
            "worktrees/w1/refs/worktree/qualified",
            f.c2,
            "from the main worktree",
        )],
    )?;
    assert_eq!(
        f.git("w1", "rev-parse refs/worktree/qualified")?.trim(),
        f.c2.to_string()
    );
    Ok(())
}

#[test]
fn a_stack_that_is_locked_by_someone_else_fails_the_transaction_with_a_retryable_error_and_publishes_nothing()
-> TestResult {
    let f = Fixture::new()?;
    let store = f.worktree();
    let main_tables = f.tables("repo/.git/reftable")?;
    std::fs::write(
        f.dir.path().join("repo/.git/worktrees/w1/reftable/tables.list.lock"),
        b"",
    )?;

    let err = commit(
        &store,
        vec![
            create("refs/heads/first-in-main", f.c1, "a"),
            create("refs/worktree/second-in-worktree", f.c1, "b"),
        ],
    )
    .expect_err("the second stack is locked");
    assert!(err.is_retryable(), "contention may go away: {err:?}");
    assert_eq!(
        f.tables("repo/.git/reftable")?,
        main_tables,
        "nothing was published to the stack that was locked first"
    );
    assert!(
        !f.dir.path().join("repo/.git/reftable/tables.list.lock").exists(),
        "its lock was released again"
    );
    Ok(())
}

#[test]
fn failing_to_write_before_anything_is_published_changes_nothing_and_says_so() -> TestResult {
    let f = Fixture::new()?;
    let store = f.main();
    let tables = f.tables("repo/.git/reftable")?;

    let store_ref = gix_ref::Store::from(store.clone());
    let prepared = store_ref
        .as_ref()
        .transaction()
        .prepare(vec![create("refs/heads/needs-a-log", f.c1, "x")], lock())?;
    let err = prepared
        .commit(None::<gix_actor::SignatureRef<'_>>)
        .expect_err("a reflog entry needs a committer");
    assert!(err.downcast_any_ref::<NothingApplied>().is_some(), "{err:?}");
    assert!(
        err.downcast_any_ref::<MissingCommitter>().is_some(),
        "the cause is told: {err:?}"
    );
    assert_eq!(f.tables("repo/.git/reftable")?, tables);
    assert!(store.try_find("refs/heads/needs-a-log")?.is_none());
    assert!(
        std::fs::read_dir(f.dir.path().join("repo/.git/reftable"))?.all(|entry| {
            let entry = entry.expect("readable").file_name();
            entry.to_string_lossy() != "tables.list.lock"
        }),
        "no lock remains"
    );

    // Without reflog entries, no committer is needed.
    let tag = RefEdit::update(
        name("refs/tags/no-log"),
        Target::Object(f.c1),
        PreviousValue::MustNotExist,
        "",
    );
    gix_ref::Store::from(store)
        .as_ref()
        .transaction()
        .prepare(vec![tag], lock())?
        .commit(None::<gix_actor::SignatureRef<'_>>)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_list_that_may_have_been_replaced_leaves_the_stack_published_before_it_applied_and_its_own_in_doubt() -> TestResult
{
    let f = Fixture::new()?;
    let store = f.worktree();
    let worktree_list = f.dir.path().join("repo/.git/worktrees/w1/reftable/tables.list");
    let main_tables = f.tables("repo/.git/reftable")?.len();

    // Make the list of the second stack unreadable once it is locked. Publishing it then fails in a way that leaves it uncertain,
    // which is not the clean failure that progress.rs covers with unit tests.
    let hook = {
        let worktree_list = worktree_list.clone();
        move || {
            std::fs::remove_file(&worktree_list).expect("the list exists");
            std::fs::create_dir(&worktree_list).expect("a directory takes the place of the file");
        }
    };
    let err = observed(
        &store,
        vec![
            create("refs/heads/in-main", f.c1, "a"),
            create("refs/worktree/in-worktree", f.c1, "b"),
        ],
        Watch {
            log: Log::default(),
            veto_in: None,
            on_prepared: Some(Box::new(hook)),
        },
    )
    .expect_err("the list can't be replaced");
    let uncertain = err
        .downcast_any_ref::<Uncertain>()
        .expect("the transaction was applied in part");
    let names: Vec<_> = uncertain.edits.iter().map(|(name, _)| name.to_string()).collect();
    assert_eq!(
        names,
        ["refs/heads/in-main", "refs/worktree/in-worktree"],
        "in the order of the edits"
    );
    assert_eq!(
        uncertain.edits[0].1,
        Progress::Applied,
        "the stack that was locked first is published"
    );
    assert_ne!(
        uncertain.edits[1].1,
        Progress::Applied,
        "the second stack may or may not have been published, which the progress doesn't call applied"
    );
    assert!(err.downcast_any_ref::<NothingApplied>().is_none());
    assert_eq!(
        f.tables("repo/.git/reftable")?.len(),
        main_tables + 1,
        "the first stack has its table"
    );
    assert_eq!(f.git("repo", "rev-parse refs/heads/in-main")?.trim(), f.c1.to_string());
    Ok(())
}

/// The highest update index that a stack has, which is the end of the range in the name of its newest table.
fn newest_update_index(fixture: &Fixture, dir: &str) -> TestResult<u64> {
    let tables = fixture.tables(dir)?;
    let newest = tables.last().expect("a stack has tables");
    let max = newest
        .split('-')
        .nth(1)
        .expect("names have a range")
        .trim_start_matches("0x");
    Ok(u64::from_str_radix(max, 16)?)
}

#[test]
fn a_transaction_uses_one_update_index_for_everything_it_writes_like_git() -> TestResult {
    let f = Fixture::new()?;
    let store = f.main();
    let before = newest_update_index(&f, "repo/.git/reftable")?;
    commit(
        &store,
        vec![
            create("refs/heads/one", f.c1, "a"),
            create("refs/heads/two", f.c2, "b"),
            create("refs/heads/three", f.c3, "c"),
        ],
    )?;
    assert_eq!(
        newest_update_index(&f, "repo/.git/reftable")?,
        before + 1,
        "three edits take one update index, not one each"
    );
    f.git("repo", "update-ref refs/heads/by-git HEAD")?;
    assert_eq!(
        newest_update_index(&f, "repo/.git/reftable")?,
        before + 2,
        "Git's transaction is the next one and continues without a gap"
    );
    assert_eq!(
        reflog_messages(&f, "repo", "refs/heads/two")?,
        ["b"],
        "the log entries of all edits are found although they share an update index"
    );
    f.git_agrees_that_nothing_is_wrong()?;
    Ok(())
}

#[test]
fn stacks_are_compacted_after_enough_transactions_and_git_keeps_reading_them() -> TestResult {
    let f = Fixture::new()?;
    let store = f.main();
    let before = f.tables("repo/.git/reftable")?.len();
    let writes = 12;
    for i in 0..writes {
        commit(&store, vec![create(&format!("refs/heads/many-{i}"), f.c1, "x")])?;
        if i % 4 == 3 {
            // Other writers take turns, which a stack has to cope with.
            f.git("repo", &format!("update-ref refs/heads/by-git-{i} {}", f.c2))?;
            if i == 7 {
                f.git("repo", "pack-refs --all")?;
            }
        }
    }
    let after = f.tables("repo/.git/reftable")?.len();
    assert!(
        after < before + writes,
        "compaction merged tables: {before} tables before, {after} after {writes} transactions"
    );
    let listed = f.git(
        "repo",
        "for-each-ref --format=%(refname) refs/heads/many-* refs/heads/by-git-*",
    )?;
    assert_eq!(listed.lines().count(), writes + writes / 4, "no reference got lost");
    f.git_agrees_that_nothing_is_wrong()?;
    Ok(())
}

#[test]
fn the_configured_options_decide_how_tables_are_written() -> TestResult {
    let f = Fixture::new()?;
    let mut store = f.main();
    let mut options = *store.stack_options()?;
    options.write.block_size = 8192;
    #[cfg(unix)]
    {
        options.shared_repository_permissions = -0o640;
    }
    store.set_stack_options(Ok(options));
    commit(&store, vec![create("refs/heads/configured", f.c1, "x")])?;

    let tables = f.tables("repo/.git/reftable")?;
    let newest = f
        .dir
        .path()
        .join("repo/.git/reftable")
        .join(tables.last().expect("a table"));
    let bytes = std::fs::read(&newest)?;
    assert_eq!(&bytes[..4], b"REFT");
    assert_eq!(
        u32::from_be_bytes([0, bytes[5], bytes[6], bytes[7]]),
        8192,
        "the header says which block size was used"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [newest, f.dir.path().join("repo/.git/reftable/tables.list")] {
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o640,
                "{path:?} gets the permissions of `core.sharedRepository`"
            );
        }
    }
    f.git_agrees_that_nothing_is_wrong()?;

    // A long message is cut to half the block size, as Git does.
    let long = "x".repeat(10_000);
    commit(
        &store,
        vec![update("refs/heads/configured", f.c2, PreviousValue::Any, &long)],
    )?;
    assert_eq!(reflog_messages(&f, "repo", "refs/heads/configured")?[0].len(), 4096);
    Ok(())
}

#[test]
fn symbolic_references_are_dereferenced_through_the_locked_stacks_and_cycles_fail() -> TestResult {
    let f = Fixture::new()?;
    let store = f.main();
    // `alias` points to `main`, which `HEAD` points to as well.
    commit(
        &store,
        vec![
            RefEdit::update(
                name("refs/heads/alias"),
                Target::Object(f.c1),
                PreviousValue::Any,
                "through the alias",
            )
            .with_deref(true),
        ],
    )?;
    assert_eq!(f.git("repo", "rev-parse refs/heads/main")?.trim(), f.c1.to_string());
    assert_eq!(f.git("repo", "rev-parse HEAD")?.trim(), f.c1.to_string());
    assert!(
        f.git("repo", "symbolic-ref refs/heads/alias").is_ok(),
        "the alias is still a symbolic reference"
    );
    assert_eq!(reflog_messages(&f, "repo", "refs/heads/main")?[0], "through the alias");
    assert_eq!(reflog_messages(&f, "repo", "HEAD")?[0], "through the alias");

    let err = commit(
        &store,
        vec![
            update("refs/heads/alias", f.c2, PreviousValue::Any, "x").with_deref(true),
            update("refs/heads/main", f.c2, PreviousValue::Any, "y"),
        ],
    )
    .expect_err("one reference is edited twice");
    assert!(err.is_validation(), "{err:?}");
    f.git_agrees_that_nothing_is_wrong()?;
    Ok(())
}

#[test]
fn references_that_git_keeps_in_files_cannot_be_edited_here() -> TestResult {
    let f = Fixture::new()?;
    let err = commit(&f.main(), vec![create("FETCH_HEAD", f.c1, "x")]).expect_err("not stored in reftables");
    assert!(err.is_unsupported(), "{err:?}");
    Ok(())
}

#[test]
fn invalid_stack_options_fail_the_transaction_and_not_reading() -> TestResult {
    let f = Fixture::new()?;
    let mut store = f.main();
    store.set_stack_options(Err(gix_error::message("block size cannot exceed 16MB").raise()));
    assert!(store.try_find("refs/heads/main")?.is_some(), "reading works");
    let err = commit(&store, vec![create("refs/heads/x", f.c1, "x")]).expect_err("writing needs valid options");
    assert!(err.to_string().contains("reftable configuration is invalid"), "{err:?}");
    Ok(())
}
