//! Expiring and deleting reflogs with the store of each reference format, with Git as the judge of what remains.
//!
//! The fixture is the same for both formats, and Git's own `reflog expire` provides the expectation for what removing
//! some lines should leave.
use gix_date::parse::TimeBuf;
use gix_lock::acquire::Fail;
use gix_ref::{
    FullName, Store, Target,
    log::expire::Options,
    transaction::{PreviousValue, RefEdit},
    view::transaction::LockOptions,
};
use gix_testtools::{Creation, TestResult};

use crate::{fixture_hash_kind, scripted_fixture_writable_with_args};

/// The reference formats this build can store references in.
fn formats() -> Vec<&'static str> {
    let mut formats = vec!["files"];
    if cfg!(feature = "reftable") {
        formats.push("reftable");
    }
    formats
}

struct Fixture {
    dir: gix_testtools::tempfile::TempDir,
    format: &'static str,
    c1: gix_hash::ObjectId,
    c2: gix_hash::ObjectId,
}

impl Fixture {
    fn new(format: &'static str) -> TestResult<Self> {
        let dir = scripted_fixture_writable_with_args("make_reflog_repo.sh", [format], Creation::CopyFromReadOnly)?;
        let c1 = gix_hash::ObjectId::from_hex(
            gix_testtools::git(dir.path().join("repo"), "rev-parse :/c1")?
                .trim()
                .as_bytes(),
        )?;
        let c2 = gix_hash::ObjectId::from_hex(
            gix_testtools::git(dir.path().join("repo"), "rev-parse :/c2")?
                .trim()
                .as_bytes(),
        )?;
        Ok(Fixture { dir, format, c1, c2 })
    }

    fn git_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("repo/.git")
    }

    fn store(&self) -> Store {
        match self.format {
            "files" => gix_ref::file::Store::at(self.git_dir(), fixture_hash_kind()).into(),
            #[cfg(feature = "reftable")]
            "reftable" => gix_ref::reftable::Store::at(self.git_dir(), fixture_hash_kind()).into(),
            other => unreachable!("{other} is not a format that is tested"),
        }
    }

    fn git(&self, args: &str) -> TestResult<String> {
        Ok(gix_testtools::git(self.dir.path().join("repo"), args)?)
    }

    /// The messages of the reflog of `reference` as Git shows them, newest first.
    fn messages(&self, reference: &str) -> TestResult<Vec<String>> {
        Ok(self
            .git(&format!("reflog show --format=%gs {reference} --"))?
            .lines()
            .map(ToOwned::to_owned)
            .collect())
    }

    fn reflog_exists(&self, reference: &str) -> bool {
        self.git(&format!("reflog exists {reference}")).is_ok()
    }

    fn git_agrees_that_nothing_is_wrong(&self) -> TestResult {
        self.git("refs verify")?;
        self.git("fsck --no-dangling")?;
        Ok(())
    }
}

fn name(name: &str) -> FullName {
    FullName::try_from(name).expect("valid name")
}

fn options() -> Options {
    let mut options = Options::default();
    options.lock = Fail::Immediately;
    options
}

#[test]
fn removing_lines_leaves_what_git_leaves_and_asks_from_the_oldest_to_the_newest_line() -> TestResult {
    for format in formats() {
        let ours = Fixture::new(format)?;
        let git = Fixture::new(format)?;
        // 1700000250 is 2023-11-14 22:17:30 UTC. Lines of commits that the branch doesn't contain are older than Git's default
        // for unreachable ones, so it has to be told that they are as good as the others.
        git.git("reflog expire --expire=2023-11-14T22:17:30+00:00 --expire-unreachable=never refs/heads/topic")?;

        let mut asked = Vec::new();
        let mut currents = Vec::new();
        let outcome =
            ours.store()
                .as_ref()
                .reflog_expire(name("refs/heads/topic").as_ref(), options(), |line, current| {
                    asked.push(line.message.to_string());
                    currents.push(current.map(ToOwned::to_owned));
                    line.signature.time.seconds >= 1700000250
                })?;
        assert_eq!(
            asked,
            ["one", "two", "three", "four"],
            "{format}: Git decides from the oldest line to the newest"
        );
        assert_eq!(
            currents,
            vec![Some(ours.c1); 4],
            "{format}: the current value is the tip of the branch, which is passed for every line"
        );
        assert_eq!(outcome.removed, 2, "{format}");
        assert_eq!(outcome.kept, 2, "{format}");
        assert_eq!(
            ours.messages("refs/heads/topic")?,
            ["four", "three"],
            "{format}: what Git reads after we removed the older lines"
        );
        assert_eq!(
            ours.messages("refs/heads/topic")?,
            git.messages("refs/heads/topic")?,
            "{format}: the same as when Git removes them"
        );
        assert_eq!(
            ours.messages("refs/tags/logged")?,
            ["tagged"],
            "{format}: the reflogs of other references are not touched"
        );
        ours.git_agrees_that_nothing_is_wrong()?;
    }
    Ok(())
}

#[test]
fn removing_every_line_leaves_a_reflog_that_exists_so_that_updates_of_the_reference_are_still_logged() -> TestResult {
    for format in formats() {
        let ours = Fixture::new(format)?;
        let git = Fixture::new(format)?;
        git.git("reflog expire --expire=all refs/tags/logged")?;

        let outcome = ours
            .store()
            .as_ref()
            .reflog_expire(name("refs/tags/logged").as_ref(), options(), |_, _| false)?;
        assert_eq!((outcome.removed, outcome.kept), (1, 0), "{format}");
        assert!(
            ours.reflog_exists("refs/tags/logged"),
            "{format}: the reflog is empty, but still there"
        );
        assert_eq!(
            ours.reflog_exists("refs/tags/logged"),
            git.reflog_exists("refs/tags/logged"),
            "{format}"
        );
        assert!(ours.messages("refs/tags/logged")?.is_empty(), "{format}");
        ours.git_agrees_that_nothing_is_wrong()?;

        let commit = |fixture: &Fixture| -> TestResult {
            fixture
                .store()
                .as_ref()
                .transaction()
                .prepare(
                    [RefEdit::update(
                        name("refs/tags/logged"),
                        Target::Object(fixture.c2),
                        PreviousValue::Any,
                        "after expiry",
                    )],
                    LockOptions::new(Fail::Immediately, Fail::Immediately),
                )?
                .commit(
                    gix_actor::Signature {
                        name: "Committer".into(),
                        email: "committer@example.com".into(),
                        time: gix_date::parse_header("1700100000 +0000").expect("valid"),
                    }
                    .to_ref(&mut TimeBuf::default()),
                )?;
            Ok(())
        };
        commit(&ours)?;
        assert_eq!(
            ours.messages("refs/tags/logged")?,
            ["after expiry"],
            "{format}: a tag only gets a line if its reflog exists, which it still does"
        );
    }
    Ok(())
}

#[test]
fn a_dry_run_says_what_would_be_removed_and_changes_nothing() -> TestResult {
    for format in formats() {
        let fixture = Fixture::new(format)?;
        let mut options = options();
        options.dry_run = true;
        let outcome =
            fixture
                .store()
                .as_ref()
                .reflog_expire(name("refs/heads/topic").as_ref(), options, |line, _| {
                    line.message == "four"
                })?;
        assert_eq!((outcome.removed, outcome.kept), (3, 1), "{format}");
        assert_eq!(
            fixture.messages("refs/heads/topic")?,
            ["four", "three", "two", "one"],
            "{format}: nothing was removed"
        );
    }
    Ok(())
}

#[test]
fn expiring_a_missing_reflog_does_nothing_and_creates_none() -> TestResult {
    for format in formats() {
        let fixture = Fixture::new(format)?;
        let outcome =
            fixture
                .store()
                .as_ref()
                .reflog_expire(name("refs/tags/unlogged").as_ref(), options(), |_, _| false)?;
        assert_eq!((outcome.removed, outcome.kept), (0, 0), "{format}");
        assert!(!fixture.reflog_exists("refs/tags/unlogged"), "{format}");
    }
    Ok(())
}

#[test]
fn deleting_a_reflog_removes_it_but_not_its_reference() -> TestResult {
    for format in formats() {
        let fixture = Fixture::new(format)?;
        let store = fixture.store();
        assert!(
            store.as_ref().reflog_delete(name("refs/heads/topic").as_ref())?,
            "{format}: there was one"
        );
        assert!(
            !fixture.reflog_exists("refs/heads/topic"),
            "{format}: Git doesn't find it either"
        );
        assert_eq!(
            fixture.git("rev-parse refs/heads/topic")?.trim(),
            fixture.c1.to_string(),
            "{format}: the reference is the same"
        );
        assert!(
            !store.as_ref().reflog_delete(name("refs/heads/topic").as_ref())?,
            "{format}: there is none to delete anymore"
        );
        assert_eq!(
            fixture.messages("refs/tags/logged")?,
            ["tagged"],
            "{format}: other reflogs stay"
        );
        fixture.git_agrees_that_nothing_is_wrong()?;
    }
    Ok(())
}

#[test]
fn a_held_lock_fails_expiring_before_anything_changes() -> TestResult {
    for format in formats() {
        let fixture = Fixture::new(format)?;
        // Writers of either format lock what they write to: the reference for files, and the list of tables for reftables.
        let lock = match format {
            "files" => fixture.git_dir().join("refs/heads/topic.lock"),
            _ => fixture.git_dir().join("reftable/tables.list.lock"),
        };
        std::fs::write(&lock, b"")?;
        let err = fixture
            .store()
            .as_ref()
            .reflog_expire(name("refs/heads/topic").as_ref(), options(), |_, _| false)
            .expect_err("somebody else is writing");
        assert!(
            format!("{err:#?}").to_lowercase().contains("lock"),
            "{format}: the error names the lock that is held: {err:#?}"
        );
        std::fs::remove_file(&lock)?;
        assert_eq!(
            fixture.messages("refs/heads/topic")?,
            ["four", "three", "two", "one"],
            "{format}: nothing was removed"
        );
    }
    Ok(())
}
