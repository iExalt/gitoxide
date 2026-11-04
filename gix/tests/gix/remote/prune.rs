//! Pruning stale references while fetching, with `git fetch --prune` as the oracle: the same scenario is run with Git on one copy of
//! the fixture and with the fork on another, and the references they leave are compared. See `make_prune_repos.sh` for what exists.
use std::{path::PathBuf, sync::atomic::AtomicBool};

use gix::remote::{
    Direction,
    fetch::{Status, Tags},
};
use gix_hash::Kind;
use gix_testtools::{Creation, TestResult};

const HEADS: &str = "+refs/heads/*:refs/p/heads/*";

struct World {
    dir: gix_testtools::tempfile::TempDir,
}

impl World {
    fn new(kind: Kind) -> TestResult<Self> {
        let dir = gix_testtools::scripted_fixture_writable_with_args(
            "make_prune_repos.sh",
            [kind.to_string()],
            Creation::CopyFromReadOnly,
        )?;
        Ok(World { dir })
    }

    /// Two copies of the same fixture, one for Git and one for the fork, in that order.
    fn twins(kind: Kind) -> TestResult<(Self, Self)> {
        Ok((World::new(kind)?, World::new(kind)?))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn git(&self, repo: &str, args: &str) -> TestResult<String> {
        Ok(gix_testtools::git(self.path(repo), args)?)
    }

    /// All references of the client with what they point to, symbolic references included, as the oracle sees them.
    fn refs(&self) -> TestResult<String> {
        self.git("client", "for-each-ref --format='%(refname) %(objectname) %(symref)'")
    }

    /// Delete `branch` on the upstream.
    fn delete_upstream_branch(&self, branch: &str) -> TestResult {
        self.git("upstream", &format!("update-ref -d refs/heads/{branch}"))?;
        Ok(())
    }

    /// Run `git fetch --prune` in the client with `specs`, and return whether it succeeded.
    fn git_fetch(&self, specs: &[&str], extra_args: &[&str]) -> TestResult<bool> {
        let output = gix_testtools::git_command(self.path("client"))
            .args(["fetch", "--prune"])
            .args(extra_args)
            .arg(self.path("upstream"))
            .args(specs)
            .output()?;
        Ok(output.status.success())
    }

    fn fetch(&self, specs: &[&str], prune: bool) -> TestResult<gix::remote::fetch::Outcome> {
        self.fetch_with(specs, prune, Tags::Included, false)
    }

    fn fetch_with(
        &self,
        specs: &[&str],
        prune: bool,
        tags: Tags,
        dry_run: bool,
    ) -> TestResult<gix::remote::fetch::Outcome> {
        let repo = gix::open_opts(self.path("client"), gix::open::Options::isolated())?;
        let remote = repo
            .remote_at(self.path("upstream").to_string_lossy().into_owned())?
            .with_fetch_tags(tags)
            .with_refspecs(specs.iter().copied(), Direction::Fetch)?;
        Ok(remote
            .connect(Direction::Fetch)?
            .prepare_fetch(gix::progress::Discard, Default::default())?
            .with_prune(prune)
            .with_dry_run(dry_run)
            .receive(gix::progress::Discard, &AtomicBool::new(false))?)
    }

    fn fsck(&self) -> TestResult {
        self.git("client", "fsck --strict")?;
        Ok(())
    }
}

fn pruned(outcome: &gix::remote::fetch::Outcome) -> Vec<String> {
    let update_refs = match &outcome.status {
        Status::NoPackReceived { update_refs, .. } | Status::Change { update_refs, .. } => update_refs,
    };
    update_refs.pruned().map(ToString::to_string).collect()
}

fn isolated() -> TestResult<bool> {
    Ok(gix_testtools::run_in_isolated_process()?)
}

#[test]
fn stale_destinations_are_deleted_after_a_fetch_and_nothing_else_matches_git() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    for kind in [Kind::Sha1, Kind::Sha256]
        .into_iter()
        .filter(|kind| *kind == Kind::Sha1 || cfg!(feature = "sha256"))
    {
        let (git, fork) = World::twins(kind)?;
        for world in [&git, &fork] {
            world.delete_upstream_branch("b2")?;
        }
        assert!(git.git_fetch(&[HEADS], &[])?);
        let outcome = fork.fetch(&[HEADS], true)?;

        assert_eq!(
            fork.refs()?,
            git.refs()?,
            "{kind}: the same references as after `git fetch --prune`"
        );
        assert!(
            matches!(outcome.status, Status::Change { .. }),
            "{kind}: the pack brought in `three`"
        );
        let mut pruned = pruned(&outcome);
        pruned.sort();
        assert_eq!(
            pruned,
            ["refs/p/heads/b2", "refs/p/heads/gone", "refs/p/heads/sub/dir"],
            "{kind}: what the remote lacks is deleted, and only below the destination"
        );
        let refs = fork.refs()?;
        for kept in ["refs/p/other/keep", "refs/tags/local-only", "refs/p/heads/alias"] {
            assert!(
                refs.contains(kept),
                "{kind}: {kept} is no destination of a spec, or it is a symbolic reference: {refs}"
            );
        }
        assert!(
            fork.path("client/.git/refs/p/heads/dangling").is_file(),
            "{kind}: a symbolic reference stays even if what it points to was deleted, which `for-each-ref` doesn't list, like Git"
        );
        // `fsck` doesn't like symbolic references to nowhere, which were already checked above.
        std::fs::remove_file(fork.path("client/.git/refs/p/heads/dangling"))?;
        fork.fsck()?;
    }
    Ok(())
}

#[test]
fn a_fetch_without_news_prunes_too() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = World::twins(Kind::Sha1)?;
    for world in [&git, &fork] {
        // Bring the client up to date first, so the next fetch has no pack to receive.
        assert!(world.git_fetch(&[HEADS], &[])?);
        world.delete_upstream_branch("b1")?;
    }
    assert!(git.git_fetch(&[HEADS], &[])?);
    let outcome = fork.fetch(&[HEADS], true)?;
    assert!(
        matches!(outcome.status, Status::NoPackReceived { dry_run: false, .. }),
        "{outcome:?}"
    );
    assert!(pruned(&outcome).contains(&"refs/p/heads/b1".to_string()), "{outcome:?}");
    assert_eq!(fork.refs()?, git.refs()?);
    Ok(())
}

#[test]
fn nothing_is_pruned_unless_asked_to_and_a_dry_run_only_reports() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (_, fork) = World::twins(Kind::Sha1)?;
    fork.delete_upstream_branch("b2")?;
    let before = fork.refs()?;
    let outcome = fork.fetch_with(&[HEADS], true, Tags::Included, true)?;
    assert_eq!(
        pruned(&outcome).len(),
        3,
        "a dry-run tells what would be deleted: {outcome:?}"
    );
    assert_eq!(fork.refs()?, before, "and changes nothing");

    let outcome = fork.fetch(&[HEADS], false)?;
    assert!(pruned(&outcome).is_empty(), "pruning is opt-in");
    assert!(
        fork.refs()?.contains("refs/p/heads/gone"),
        "so the stale reference remains"
    );
    Ok(())
}

#[test]
fn tags_are_not_destinations_of_the_implicit_tag_spec() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = World::twins(Kind::Sha1)?;
    assert!(git.git_fetch(&[HEADS], &["--tags"])?);
    let outcome = fork.fetch_with(&[HEADS], true, Tags::All, false)?;
    assert!(
        !pruned(&outcome).iter().any(|name| name.starts_with("refs/tags/")),
        "`--tags` follows tags without making them destinations that can be stale: {outcome:?}"
    );
    assert_eq!(fork.refs()?, git.refs()?, "and Git keeps the local-only tag as well");
    assert!(fork.refs()?.contains("refs/tags/local-only"));
    Ok(())
}

#[test]
fn a_partial_source_name_is_covered_but_its_destination_is_not_stale() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = World::twins(Kind::Sha1)?;
    let spec = "+main:refs/p/exact/main";
    assert!(git.git_fetch(&[spec], &[])?);
    let outcome = fork.fetch(&[spec], true)?;
    assert!(pruned(&outcome).is_empty(), "{outcome:?}");
    assert_eq!(fork.refs()?, git.refs()?);
    assert!(
        fork.refs()?.contains("refs/p/exact/main"),
        "the reference that was just updated survives"
    );
    Ok(())
}

#[test]
fn what_another_spec_maps_into_the_same_namespace_is_not_stale() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = World::twins(Kind::Sha1)?;
    let specs = [HEADS, "+refs/tags/*:refs/p/heads/tags/*"];
    for world in [&git, &fork] {
        world.git("client", "update-ref refs/p/heads/tags/t1 refs/p/heads/main")?;
        world.git("client", "update-ref refs/p/heads/tags/not-there refs/p/heads/main")?;
    }
    assert!(git.git_fetch(&specs, &[])?);
    let outcome = fork.fetch(&specs, true)?;
    assert_eq!(
        pruned(&outcome)
            .into_iter()
            .filter(|name| name.contains("tags/"))
            .collect::<Vec<_>>(),
        ["refs/p/heads/tags/not-there"],
        "`t1` is produced by the second spec, even though the first spec covers it too: {outcome:?}"
    );
    assert_eq!(fork.refs()?, git.refs()?);
    Ok(())
}

#[test]
fn negative_specs_protect_what_they_exclude() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = World::twins(Kind::Sha1)?;
    let specs = [HEADS, "^refs/heads/b2"];
    for world in [&git, &fork] {
        world.delete_upstream_branch("b1")?;
        world.git("client", "update-ref refs/p/heads/b3 refs/p/heads/main")?;
    }
    assert!(git.git_fetch(&specs, &[])?);
    let outcome = fork.fetch(&specs, true)?;
    let pruned = pruned(&outcome);
    assert!(pruned.contains(&"refs/p/heads/b1".to_string()), "{pruned:?}");
    assert!(
        !pruned.contains(&"refs/p/heads/b2".to_string()),
        "the upstream has `b2`, but it's excluded, so its tracking reference isn't covered: {pruned:?}"
    );
    assert_eq!(fork.refs()?, git.refs()?);
    Ok(())
}

#[test]
fn a_wildcard_source_without_a_fixed_prefix_lists_everything_so_nothing_that_exists_is_pruned() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = World::twins(Kind::Sha1)?;
    let specs = ["+refs/*:refs/p/all/*"];
    for world in [&git, &fork] {
        world.git("client", "update-ref refs/p/all/heads/main refs/p/heads/main")?;
        world.git("client", "update-ref refs/p/all/heads/gone refs/p/heads/main")?;
    }
    assert!(git.git_fetch(&specs, &[])?);
    let outcome = fork.fetch(&specs, true)?;
    assert_eq!(
        pruned(&outcome),
        ["refs/p/all/heads/gone"],
        "only what the remote lacks goes, but `refs/*` has to list its references first: {outcome:?}"
    );
    let refs = fork.refs()?;
    assert!(
        refs.contains("refs/p/all/heads/b1") && refs.contains("refs/p/all/tags/v1"),
        "{refs}"
    );
    assert_eq!(refs, git.refs()?);
    Ok(())
}

#[test]
fn a_wildcard_in_the_middle_of_a_source_is_listed_beside_the_prefix_of_another_spec() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = World::twins(Kind::Sha1)?;
    let specs = [HEADS, "+refs/*/main:refs/p/m/*/main"];
    for world in [&git, &fork] {
        world.git("upstream", "update-ref refs/notes/main refs/heads/b1")?;
        world.git("client", "update-ref refs/p/m/notes/main refs/p/heads/main")?;
        world.git("client", "update-ref refs/p/m/gone/main refs/p/heads/main")?;
    }
    assert!(git.git_fetch(&specs, &[])?);
    let outcome = fork.fetch(&specs, true)?;
    let pruned = pruned(&outcome);
    assert!(
        !pruned.contains(&"refs/p/m/notes/main".to_string()),
        "the remote has `refs/notes/main`, which only the wildcard spec asks for: {pruned:?}"
    );
    assert_eq!(
        pruned
            .iter()
            .filter(|name| name.starts_with("refs/p/m/"))
            .collect::<Vec<_>>(),
        ["refs/p/m/gone/main"]
    );
    assert_eq!(fork.refs()?, git.refs()?);
    Ok(())
}

#[test]
fn a_failing_update_prunes_nothing() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (_, fork) = World::twins(Kind::Sha1)?;
    fork.delete_upstream_branch("b2")?;
    let before = fork.refs()?;
    // Somebody else holds the reference `main` is going to be updated at.
    std::fs::write(fork.path("client/.git/refs/p/heads/main.lock"), b"")?;
    fork.fetch(&[HEADS], true)
        .expect_err("the update of `main` can't take the lock");
    assert_eq!(
        fork.refs()?,
        before,
        "the deletions are part of the transaction that failed"
    );
    Ok(())
}

#[test]
fn a_stale_reference_in_the_way_of_an_update_is_deleted_first_and_alone() -> TestResult {
    if isolated()? {
        return Ok(());
    }
    let (git, fork) = World::twins(Kind::Sha1)?;
    // `refs/p/heads/x` is a file where `refs/p/heads/x/y` has to be created, and the remote has no `x` anymore.
    for world in [&git, &fork] {
        world.git("client", "update-ref refs/p/heads/x refs/p/heads/main")?;
    }
    assert!(git.git_fetch(&[HEADS], &[])?);
    let outcome = fork.fetch(&[HEADS], true)?;
    assert!(pruned(&outcome).contains(&"refs/p/heads/x".to_string()), "{outcome:?}");
    assert_eq!(
        fork.refs()?,
        git.refs()?,
        "like in Git, the new reference could be created"
    );
    assert!(fork.refs()?.contains("refs/p/heads/x/y"));

    // If the update fails after the blocking reference was deleted, only that one is gone: Git deletes everything first.
    let (_, fork) = World::twins(Kind::Sha1)?;
    fork.git("client", "update-ref refs/p/heads/x refs/p/heads/main")?;
    std::fs::write(fork.path("client/.git/refs/p/heads/main.lock"), b"")?;
    fork.fetch(&[HEADS], true)
        .expect_err("`main` can't be updated, as it is locked");
    let refs = fork.refs()?;
    assert!(
        !refs.contains("refs/p/heads/x "),
        "the reference in the way was deleted before the update failed: {refs}"
    );
    assert!(
        refs.contains("refs/p/heads/gone") && refs.contains("refs/p/heads/sub/dir"),
        "everything else that is stale is still there: {refs}"
    );
    Ok(())
}
