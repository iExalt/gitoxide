//! Pushing from a repository to a local remote that runs the real `git receive-pack`, with `git push` as the oracle: the same
//! scenario is run with Git on one copy of the fixture and with the fork on another, and what they leave behind is compared.
use std::{path::PathBuf, sync::atomic::AtomicBool};

use gix::{
    ObjectId,
    bstr::ByteSlice,
    refs::FullName,
    remote::{
        Direction,
        push::{self, Decision, Observed, Policy, Refusal, Status, Update},
    },
};
use gix_hash::Kind;
use gix_testtools::{Creation, TestResult};

/// Fixtures for one object format, see `make_push_repos.sh` for what they contain.
struct World {
    kind: Kind,
    dir: gix_testtools::tempfile::TempDir,
}

impl World {
    fn new(kind: Kind) -> TestResult<Self> {
        let dir = gix_testtools::scripted_fixture_writable_with_args(
            "make_push_repos.sh",
            [kind.to_string()],
            Creation::CopyFromReadOnly,
        )?;
        Ok(World { kind, dir })
    }

    /// Two copies of the same fixture, one for Git and one for the fork, in that order.
    fn twins(kind: Kind) -> TestResult<(Self, Self)> {
        Ok((World::new(kind)?, World::new(kind)?))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn repo(&self, name: &str) -> TestResult<gix::Repository> {
        Ok(gix::open_opts(self.path(name), gix::open::Options::isolated())?)
    }

    fn id(&self, repo: &str, rev: &str) -> TestResult<ObjectId> {
        Ok(ObjectId::from_hex(
            gix_testtools::git(self.path(repo), &format!("rev-parse {rev}"))?
                .trim()
                .as_bytes(),
        )?)
    }

    /// The references of `repo` as `name id` lines.
    fn refs(&self, repo: &str) -> TestResult<String> {
        Ok(gix_testtools::git(
            self.path(repo),
            "for-each-ref --format='%(refname) %(objectname)'",
        )?)
    }

    /// Run `git push` in `from` towards `remote` with `args`, which are flags if they start with a dash and refspecs otherwise,
    /// and return whether it succeeded.
    fn git_push(&self, from: &str, remote: &str, args: &[&str]) -> TestResult<bool> {
        let (flags, refspecs): (Vec<&str>, Vec<&str>) = args.iter().copied().partition(|arg| arg.starts_with('-'));
        let output = gix_testtools::git_command(self.path(from))
            .arg("push")
            .args(flags)
            .arg(self.path(remote))
            .args(refspecs)
            .output()?;
        Ok(output.status.success())
    }

    /// Run the fork's push from `from` to `remote`.
    fn push(
        &self,
        from: &str,
        remote: &str,
        updates: Vec<Update>,
        options: push::Options,
    ) -> TestResult<gix::error::Result<push::Outcome>> {
        let repo = self.repo(from)?;
        let remote = repo.remote_at(self.path(remote).to_string_lossy().into_owned())?;
        let prepare = remote
            .connect(Direction::Push)?
            .prepare_push(gix::progress::Discard, updates, options)?;
        Ok(prepare.send(&AtomicBool::new(false)))
    }

    /// The plan the fork makes for `updates`, without sending anything.
    fn plan(&self, from: &str, remote: &str, updates: Vec<Update>) -> TestResult<push::Plan> {
        let repo = self.repo(from)?;
        let remote = repo.remote_at(self.path(remote).to_string_lossy().into_owned())?;
        let prepare =
            remote
                .connect(Direction::Push)?
                .prepare_push(gix::progress::Discard, updates, push::Options::default())?;
        Ok(prepare.plan().clone())
    }

    /// Look at what `remote` has for the updates of `outcome`, with a fresh connection.
    fn reconcile(&self, from: &str, remote: &str, outcome: &push::Outcome) -> TestResult<push::Reconciliation> {
        let repo = self.repo(from)?;
        let remote = repo.remote_at(self.path(remote).to_string_lossy().into_owned())?;
        Ok(remote
            .connect(Direction::Push)?
            .reconcile(gix::progress::Discard, outcome)?)
    }

    /// All `*.lock` files below `repo`, which a server leaves behind if it is interrupted while it changes references.
    fn lock_files(&self, repo: &str) -> TestResult<Vec<PathBuf>> {
        fn collect(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    collect(&path, out)?;
                } else if path.extension().is_some_and(|ext| ext == "lock") {
                    out.push(path);
                }
            }
            Ok(())
        }
        let mut out = Vec::new();
        collect(&self.path(repo), &mut out)?;
        Ok(out)
    }

    fn fsck(&self, repo: &str) -> TestResult {
        gix_testtools::git(self.path(repo), "fsck --strict")?;
        Ok(())
    }

    fn write_hook(&self, repo: &str, name: &str, script: &str) -> TestResult {
        let hook = self.path(repo).join("hooks").join(name);
        std::fs::create_dir_all(hook.parent().expect("a parent"))?;
        std::fs::write(&hook, format!("#!/bin/sh\n{script}\n"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    }
}

fn name(name: &str) -> FullName {
    name.try_into().expect("valid reference name")
}

fn isolated() -> TestResult<bool> {
    Ok(gix_testtools::run_in_isolated_process()?)
}

fn sha1_only() -> TestResult<(World, World)> {
    World::twins(Kind::Sha1)
}

fn kinds() -> impl Iterator<Item = Kind> {
    Kind::all()
        .iter()
        .copied()
        .filter(|kind| *kind == Kind::Sha1 || cfg!(feature = "sha256"))
}

fn is_rejected(status: &Status) -> bool {
    matches!(status, Status::Rejected { .. })
}

#[test]
fn creating_updating_and_deleting_in_one_atomic_push_matches_git() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    for kind in kinds() {
        let (git, fork) = World::twins(kind)?;
        assert!(git.git_push(
            "source",
            "remote.git",
            &[
                "--atomic",
                "main:refs/heads/new",
                "main:refs/heads/main",
                ":refs/heads/victim"
            ]
        )?);

        let (one, two, three) = (
            fork.id("source", "light")?,
            fork.id("source", "main~1")?,
            fork.id("source", "main")?,
        );
        let outcome = fork
            .push(
                "source",
                "remote.git",
                vec![
                    Update::create(name("refs/heads/new"), three),
                    Update::change(name("refs/heads/main"), two, three),
                    Update::delete(name("refs/heads/victim"), one),
                ],
                push::Options::default(),
            )?
            .expect("the request was sent");
        assert!(outcome.is_success(), "{kind}: {outcome:?}");
        assert!(
            outcome
                .entries
                .iter()
                .all(|entry| matches!(entry.status, Status::Applied { .. })),
            "{kind}: {outcome:?}"
        );
        assert!(outcome.server_finished, "{kind}: the server was waited for");
        assert_eq!(
            fork.refs("remote.git")?,
            git.refs("remote.git")?,
            "{kind}: the same references as after `git push`"
        );
        fork.fsck("remote.git")?;
    }
    Ok(())
}

#[test]
fn a_lease_that_disagrees_with_the_remote_is_refused_before_anything_is_sent() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (_, fork) = sha1_only()?;
    let (one, two, three) = (
        fork.id("source", "light")?,
        fork.id("source", "main~1")?,
        fork.id("source", "main")?,
    );
    // An object the remote doesn't have: `receive-pack` deletes a reference whose expected value it doesn't know without any check.
    let unknown = ObjectId::from_hex(&vec![b'f'; fork.kind.len_in_hex()])?;
    let before = fork.refs("remote.git")?;
    let stale = |advertised| Decision::Refused(Refusal::StaleLease { advertised });
    for (update, expected) in [
        (Update::delete(name("refs/heads/main"), unknown), stale(Some(two))),
        (Update::delete(name("refs/heads/main"), one), stale(Some(two))),
        (Update::change(name("refs/heads/main"), one, three), stale(Some(two))),
        (Update::create(name("refs/heads/main"), three), stale(Some(two))),
        (Update::delete(name("refs/heads/missing"), one), stale(None)),
        (Update::change(name("refs/heads/missing"), one, three), stale(None)),
    ] {
        let description = format!("{update:?}");
        let plan = fork.plan("source", "remote.git", vec![update.clone()])?;
        assert_eq!(plan.entries[0].decision, expected, "{description}");

        let err = fork
            .push("source", "remote.git", vec![update.clone()], push::Options::default())?
            .expect_err("an atomic push with a refused update sends nothing");
        assert!(err.classify().is_validation(), "{description}: {err:#}");
        let outcome = fork
            .push(
                "source",
                "remote.git",
                vec![update],
                push::Options {
                    atomic: false,
                    ..Default::default()
                },
            )?
            .expect("a refused update is an outcome without a request");
        assert!(
            matches!(outcome.entries[0].status, Status::Refused(Refusal::StaleLease { .. })),
            "{description}: {outcome:?}"
        );
        assert!(
            !outcome.is_success() && !outcome.is_uncertain(),
            "{description}: {outcome:?}"
        );
        assert_eq!(fork.refs("remote.git")?, before, "{description}: nothing changed");
    }
    Ok(())
}

#[test]
fn references_that_change_after_the_advertisement_are_rejected_by_the_server_and_keep_their_value() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = sha1_only()?;
    let (one, two, three) = (
        fork.id("source", "light")?,
        fork.id("source", "main~1")?,
        fork.id("source", "main")?,
    );
    // The references are changed while the push is received, which is after the advertisement. Reference updates are refused
    // in the quarantine of a push, which the hook has to leave. An update, a deletion and a creation are checked in
    // different ways by the server, so all of them are moved.
    for world in [&git, &fork] {
        world.write_hook(
            "remote.git",
            "pre-receive",
            &format!(
                "for update in main:{one} victim:{two} new:{one}; do\n  env -u GIT_QUARANTINE_PATH git update-ref refs/heads/${{update%%:*}} ${{update#*:}}\ndone"
            ),
        )?;
    }
    assert!(!git.git_push(
        "source",
        "remote.git",
        &["main:refs/heads/main", ":refs/heads/victim", "main:refs/heads/new"]
    )?);

    let outcome = fork
        .push(
            "source",
            "remote.git",
            vec![
                Update::change(name("refs/heads/main"), two, three),
                Update::delete(name("refs/heads/victim"), one),
                Update::create(name("refs/heads/new"), three),
            ],
            push::Options {
                atomic: false,
                ..Default::default()
            },
        )?
        .expect("the request was sent");
    assert!(
        outcome.entries.iter().all(|entry| is_rejected(&entry.status)),
        "the server checked every lease again, and each was stale: {outcome:?}"
    );
    assert!(!outcome.is_uncertain(), "{outcome:?}");
    assert_eq!(
        fork.refs("remote.git")?,
        git.refs("remote.git")?,
        "the values of the concurrent writer survived, as with Git"
    );
    assert_eq!(fork.id("remote.git", "main")?, one);
    assert_eq!(fork.id("remote.git", "victim")?, two);
    assert_eq!(fork.id("remote.git", "new")?, one);
    Ok(())
}

#[test]
fn history_policy_decides_between_fast_forward_and_rewrite() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = sha1_only()?;
    let (one, two, three, side) = (
        fork.id("source", "light")?,
        fork.id("source", "main~1")?,
        fork.id("source", "main")?,
        fork.id("source", "side")?,
    );
    let before = fork.refs("remote.git")?;
    assert!(
        !git.git_push("source", "remote.git", &["side:refs/heads/main"])?,
        "Git refuses to rewrite without force"
    );
    assert!(
        !git.git_push("source", "remote.git", &["main:refs/tags/light"])?,
        "and to update a tag without force"
    );

    let rewrite = |update: Update| update.with_policy(Policy::Rewrite);
    for (update, refusal) in [
        (
            Update::change(name("refs/heads/main"), two, side),
            Refusal::NotFastForward,
        ),
        (Update::change(name("refs/tags/light"), one, three), Refusal::TagUpdate),
        (
            Update::change(name("refs/heads/main"), two, one),
            Refusal::NotFastForward,
        ),
    ] {
        let plan = fork.plan("source", "remote.git", vec![update.clone()])?;
        assert_eq!(plan.entries[0].decision, Decision::Refused(refusal), "{update:?}");
        assert!(
            plan.to_send().next().is_none() && plan.refused().count() == 1,
            "{update:?}: refused entries are not sent"
        );
        assert_eq!(
            fork.plan("source", "remote.git", vec![rewrite(update.clone())])?
                .entries[0]
                .decision,
            Decision::Send,
            "{update:?}: rewriting is permitted when asked for"
        );
    }
    assert_eq!(fork.refs("remote.git")?, before);

    assert!(git.git_push(
        "source",
        "remote.git",
        &["+side:refs/heads/main", "+main:refs/tags/light"]
    )?);
    let outcome = fork
        .push(
            "source",
            "remote.git",
            vec![
                rewrite(Update::change(name("refs/heads/main"), two, side)),
                rewrite(Update::change(name("refs/tags/light"), one, three)),
            ],
            push::Options::default(),
        )?
        .expect("the request was sent");
    assert!(outcome.is_success(), "{outcome:?}");
    assert_eq!(fork.refs("remote.git")?, git.refs("remote.git")?);
    fork.fsck("remote.git")?;
    Ok(())
}

#[test]
fn a_lease_object_that_is_not_available_locally_cannot_be_shown_to_be_a_fast_forward() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (_, fork) = sha1_only()?;
    let (two, side) = (fork.id("source", "main~1")?, fork.id("source", "side")?);
    // The shallow repository has "side" but not "two", which the remote has and the lease names.
    let plan = fork.plan(
        "shallow-side",
        "remote.git",
        vec![Update::change(name("refs/heads/main"), two, side)],
    )?;
    assert_eq!(
        plan.entries[0].decision,
        Decision::Refused(Refusal::LeaseObjectMissing),
        "the remote has what the lease names, but without the object, there is no telling if it is a fast-forward"
    );
    Ok(())
}

#[test]
fn updates_the_server_refuses_are_reported_as_rejected_without_changing_anything() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = sha1_only()?;
    for world in [&git, &fork] {
        gix_testtools::git(world.path("remote.git"), "config receive.denyNonFastForwards true")?;
        gix_testtools::git(world.path("remote.git"), "config receive.denyDeletes true")?;
        gix_testtools::git(world.path("remote.git"), "config receive.hideRefs refs/heads/hidden")?;
    }
    let (one, two, side) = (
        fork.id("source", "light")?,
        fork.id("source", "main~1")?,
        fork.id("source", "side")?,
    );
    let before = fork.refs("remote.git")?;
    assert!(!git.git_push("source", "remote.git", &["+side:refs/heads/main"])?);
    assert!(!git.git_push("source", "remote.git", &[":refs/heads/victim"])?);
    assert!(!git.git_push("source", "remote.git", &["side:refs/heads/hidden"])?);

    let outcome = fork
        .push(
            "source",
            "remote.git",
            vec![
                Update::change(name("refs/heads/main"), two, side).with_policy(Policy::Rewrite),
                Update::delete(name("refs/heads/victim"), one),
                // The remote doesn't list the reference it hides, so it looks absent.
                Update::create(name("refs/heads/hidden"), side),
            ],
            push::Options {
                atomic: false,
                ..Default::default()
            },
        )?
        .expect("the request was sent");
    for (entry, needle) in outcome.entries.iter().zip(["non-fast-forward", "deletion", "hidden"]) {
        let Status::Rejected { reason } = &entry.status else {
            panic!("{needle}: expected a rejection: {outcome:?}");
        };
        assert!(reason.contains_str(needle), "{needle}: {reason}");
    }
    assert!(!outcome.is_uncertain(), "{outcome:?}");
    assert_eq!(fork.refs("remote.git")?, before, "nothing was changed");
    assert_eq!(git.refs("remote.git")?, before, "Git didn't change anything either");
    Ok(())
}

#[test]
fn a_shallow_source_is_refused_when_the_history_crosses_its_boundary_and_not_when_the_remote_has_it() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = sha1_only()?;
    // On top of the boundary the remote has as `ahead`: nothing behind the boundary is needed.
    for world in [&git, &fork] {
        gix_testtools::git(world.path("shallow-three"), "commit --allow-empty -qm four")?;
    }
    assert!(
        git.git_push("shallow-three", "remote.git", &["main:refs/heads/ahead"])?,
        "Git can send a commit on top of a boundary the remote has"
    );
    let (three, four) = (fork.id("source", "main")?, fork.id("shallow-three", "main")?);
    let outcome = fork
        .push(
            "shallow-three",
            "remote.git",
            vec![Update::change(name("refs/heads/ahead"), three, four)],
            push::Options::default(),
        )?
        .expect("the request was sent");
    assert!(outcome.is_success(), "{outcome:?}");
    assert_eq!(fork.id("remote.git", "ahead")?, four, "the new commit arrived");
    fork.fsck("remote.git")?;

    // Nothing the remote has hides the boundary, so the history behind it would be missing.
    let side = fork.id("shallow-side", "side")?;
    assert!(
        !git.git_push("shallow-side", "empty.git", &["side:refs/heads/side"])?,
        "Git can't push the history either"
    );
    let err = fork
        .push(
            "shallow-side",
            "empty.git",
            vec![Update::create(name("refs/heads/side"), side)],
            push::Options::default(),
        )?
        .expect_err("nothing is sent");
    assert!(err.classify().is_unsupported(), "{err:#}");
    let shallow = err
        .downcast_any_ref::<push::ShallowSource>()
        .expect("the refusal can be told apart");
    assert_eq!(shallow.shallow_commit, side, "the boundary is named");
    assert_eq!(shallow.ref_name, "refs/heads/side");
    assert_eq!(fork.refs("empty.git")?, "", "nothing arrived");
    assert_eq!(git.refs("empty.git")?, "", "nor did it with Git");
    Ok(())
}

#[test]
fn the_check_before_sending_sees_what_git_gives_its_pre_push_hook_and_can_veto() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = sha1_only()?;
    let log = git.path("pre-push.log");
    git.write_hook(
        "source/.git",
        "pre-push",
        &format!("echo \"$1 $2\" >{log:?}\ncat >>{log:?}", log = log.display()),
    )?;
    let (one, two, three) = (
        fork.id("source", "light")?,
        fork.id("source", "main~1")?,
        fork.id("source", "main")?,
    );
    assert!(git.git_push("source", "remote.git", &["main:refs/heads/main", ":refs/heads/victim"])?);
    let git_view = std::fs::read_to_string(&log)?;

    let repo = fork.repo("source")?;
    let remote_path = fork.path("remote.git").to_string_lossy().into_owned();
    let remote = repo.remote_at(remote_path.clone())?;
    let mut seen = None;
    let outcome = remote
        .connect(Direction::Push)?
        .prepare_push(
            gix::progress::Discard,
            vec![
                Update::change(name("refs/heads/main"), two, three).with_local_name("refs/heads/main"),
                Update::delete(name("refs/heads/victim"), one),
            ],
            push::Options::default(),
        )?
        .with_before_send(|pre_push| {
            seen = Some(format!(
                "{} {}\n{}",
                pre_push.remote_name,
                pre_push.remote_url,
                pre_push.stdin()
            ));
            Ok(())
        })
        .send(&AtomicBool::new(false))?;
    assert!(outcome.is_success(), "{outcome:?}");
    let root = |world: &World, text: &str| text.replace(&*world.dir.path().to_string_lossy(), "<fixture>");
    assert_eq!(
        root(&fork, &seen.expect("the check ran before sending")),
        root(&git, &git_view),
        "arguments and standard input are those of Git's `pre-push` hook"
    );

    // A veto sends nothing.
    let (_, fork) = sha1_only()?;
    let before = fork.refs("remote.git")?;
    let repo = fork.repo("source")?;
    let remote = repo.remote_at(fork.path("remote.git").to_string_lossy().into_owned())?;
    let err = remote
        .connect(Direction::Push)?
        .prepare_push(
            gix::progress::Discard,
            vec![Update::change(name("refs/heads/main"), two, three)],
            push::Options::default(),
        )?
        .with_before_send(|_| Err(gix::error::ErrorExt::raise(gix::error::message("vetoed"))))
        .send(&AtomicBool::new(false))
        .expect_err("a veto is an error, as nothing was sent");
    assert!(format!("{err:#}").contains("vetoed"), "{err:#}");
    assert_eq!(fork.refs("remote.git")?, before, "nothing was sent");
    Ok(())
}

#[test]
fn updates_that_are_up_to_date_or_refused_do_not_prevent_the_others_in_a_non_atomic_push() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (_, fork) = sha1_only()?;
    let (one, two, three) = (
        fork.id("source", "light")?,
        fork.id("source", "main~1")?,
        fork.id("source", "main")?,
    );
    let outcome = fork
        .push(
            "source",
            "remote.git",
            vec![
                Update::change(name("refs/heads/main"), two, two),
                Update::change(name("refs/heads/victim"), two, three),
                Update::create(name("refs/heads/new"), three),
            ],
            push::Options {
                atomic: false,
                ..Default::default()
            },
        )?
        .expect("the request was sent");
    let statuses: Vec<_> = outcome.entries.iter().map(|entry| &entry.status).collect();
    assert!(matches!(statuses[0], Status::UpToDate), "{outcome:?}");
    assert!(
        matches!(statuses[1], Status::Refused(Refusal::StaleLease { advertised: Some(id) }) if *id == one),
        "{outcome:?}"
    );
    assert!(matches!(statuses[2], Status::Applied { .. }), "{outcome:?}");
    assert!(
        !outcome.is_success(),
        "a refused update means the push is not a success"
    );
    assert_eq!(fork.id("remote.git", "new")?, three);
    assert_eq!(
        fork.id("remote.git", "victim")?,
        one,
        "the refused update left the reference alone"
    );
    Ok(())
}

#[test]
fn a_server_that_dies_after_committing_gives_an_uncertain_outcome_that_reconciles_to_the_new_value() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (_, fork) = sha1_only()?;
    let (two, three) = (fork.id("source", "main~1")?, fork.id("source", "main")?);
    // Kill `receive-pack` once the reference is committed, which is before it can report anything.
    fork.write_hook(
        "remote.git",
        "reference-transaction",
        "[ \"$1\" = committed ] && kill -9 $PPID\nexit 0",
    )?;
    let update = Update::change(name("refs/heads/main"), two, three);
    let outcome = fork
        .push("source", "remote.git", vec![update.clone()], push::Options::default())?
        .expect("the request was sent, so the result is an outcome and not an error");
    assert!(outcome.is_uncertain() && !outcome.is_success(), "{outcome:?}");
    assert!(
        outcome.uncertain.is_some() && outcome.entries[0].status == Status::Unknown,
        "no report came back, so nothing can be said about the update: {outcome:?}"
    );
    assert!(outcome.entries[0].was_sent());

    let reconciliation = fork.reconcile("source", "remote.git", &outcome)?;
    assert_eq!(
        reconciliation.observations,
        [push::Observation {
            update,
            observed: Observed::AtNew
        }],
        "the reference was committed before the server died"
    );
    assert_eq!(
        fork.lock_files("remote.git")?,
        Vec::<PathBuf>::new(),
        "the server was waited for, and nothing it was doing was interrupted by us"
    );
    Ok(())
}

#[test]
fn a_rejected_update_reconciles_to_the_other_writers_value_and_a_retry_cannot_overwrite_it() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (_, fork) = sha1_only()?;
    let (one, two, three) = (
        fork.id("source", "light")?,
        fork.id("source", "main~1")?,
        fork.id("source", "main")?,
    );
    fork.write_hook(
        "remote.git",
        "pre-receive",
        &format!("env -u GIT_QUARANTINE_PATH git update-ref refs/heads/main {one}"),
    )?;
    let update = Update::change(name("refs/heads/main"), two, three);
    let outcome = fork
        .push("source", "remote.git", vec![update.clone()], push::Options::default())?
        .expect("the request was sent");
    assert!(is_rejected(&outcome.entries[0].status), "{outcome:?}");
    assert_eq!(
        fork.reconcile("source", "remote.git", &outcome)?.observations[0].observed,
        Observed::Elsewhere(Some(one)),
        "somebody else moved the reference, which is neither what we expected nor what we wanted"
    );

    // The retry keeps its lease, which is now stale, so it can't replace the other writer's value.
    std::fs::remove_file(fork.path("remote.git/hooks/pre-receive"))?;
    let retried = fork
        .push("source", "remote.git", vec![update], push::Options::default())?
        .expect_err("the lease no longer matches, so nothing is sent");
    assert!(retried.classify().is_validation(), "{retried:#}");
    assert_eq!(
        fork.id("remote.git", "main")?,
        one,
        "the other writer's value is intact"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_generic_failure_of_a_batch_is_uncertain_and_reconciles_per_update() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    if isolated()? {
        return Ok(());
    }
    let (_, fork) = sha1_only()?;
    let (one, two, three) = (
        fork.id("source", "light")?,
        fork.id("source", "main~1")?,
        fork.id("source", "main")?,
    );
    // Deleting a tag doesn't need the directory of the branches, but updating a branch does. A non-atomic push commits deletions first, and then
    // reports the failure of the second batch for every command, including the deletion that was committed.
    let heads = fork.path("remote.git/refs/heads");
    std::fs::set_permissions(&heads, std::fs::Permissions::from_mode(0o555))?;
    let restore = scopeguard(heads.clone());
    if std::fs::File::create(heads.join("probe")).is_ok() {
        std::fs::remove_file(heads.join("probe"))?;
        eprintln!("skipped as read-only directories can't be enforced, which happens when running as root");
        return Ok(());
    }
    let delete = Update::delete(name("refs/tags/light"), one);
    let change = Update::change(name("refs/heads/main"), two, three);
    let outcome = fork
        .push(
            "source",
            "remote.git",
            vec![delete.clone(), change.clone()],
            push::Options {
                atomic: false,
                ..Default::default()
            },
        )?
        .expect("the request was sent");
    drop(restore);
    assert!(
        !matches!(outcome.entries[0].status, Status::Rejected { .. }),
        "a deletion that happened is never called rejected: {outcome:?}"
    );
    assert!(
        !matches!(outcome.entries[1].status, Status::Applied { .. }),
        "an update that didn't happen is never called applied: {outcome:?}"
    );
    let reconciliation = fork.reconcile("source", "remote.git", &outcome)?;
    assert_eq!(
        reconciliation
            .observations
            .iter()
            .map(|o| o.observed)
            .collect::<Vec<_>>(),
        [Observed::AtNew, Observed::AtLease],
        "the deletion happened and the update didn't, whatever the server said for the batch"
    );
    Ok(())
}

/// Make `dir` writable again when dropped, so it can be removed even if a test fails early.
#[cfg(unix)]
fn scopeguard(dir: PathBuf) -> impl Drop {
    struct MakeWritable(PathBuf);
    impl Drop for MakeWritable {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755)).ok();
        }
    }
    MakeWritable(dir)
}

#[test]
fn pushing_to_a_remote_without_references_or_objects_creates_them() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    for kind in kinds() {
        let (git, fork) = World::twins(kind)?;
        assert!(git.git_push(
            "source",
            "empty.git",
            &["--atomic", "main:refs/heads/main", "side:refs/heads/side"]
        )?);
        let (three, side) = (fork.id("source", "main")?, fork.id("source", "side")?);
        let outcome = fork
            .push(
                "source",
                "empty.git",
                vec![
                    Update::create(name("refs/heads/main"), three),
                    Update::create(name("refs/heads/side"), side),
                ],
                push::Options::default(),
            )?
            .expect("the request was sent");
        assert!(outcome.is_success(), "{kind}: {outcome:?}");
        assert_eq!(fork.refs("empty.git")?, git.refs("empty.git")?, "{kind}");
        fork.fsck("empty.git")?;
    }
    Ok(())
}
