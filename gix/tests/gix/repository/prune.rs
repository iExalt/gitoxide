//! Pruning loose objects, with `git prune` and `git prune-packed` as the oracle. See `make_prune_repo.sh` for what is in the
//! repository, and which objects and temporary files are expected to go.
use std::{collections::BTreeSet, path::Path};

use gix::maintenance::prune::{Options, PackedOptions};
use gix_hash::Kind;
use gix_testtools::{Creation, TestResult};

/// 2010-01-01T00:00:00Z, which is after the old things of the fixture and before the recent ones.
const EXPIRE_SECONDS: i64 = 1_262_304_000;
const EXPIRE_FOR_GIT: &str = "2010-01-01T00:00:00Z";

fn kinds() -> impl Iterator<Item = Kind> {
    [Kind::Sha1, Kind::Sha256]
        .into_iter()
        .filter(|kind| *kind == Kind::Sha1 || cfg!(feature = "sha256"))
}

fn fixture(kind: Kind) -> TestResult<gix_testtools::tempfile::TempDir> {
    Ok(gix_testtools::scripted_fixture_writable_with_args(
        "make_prune_repo.sh",
        [kind.to_string()],
        Creation::Execute,
    )?)
}

fn blob_id(kind: Kind, content: &str) -> gix_hash::ObjectId {
    gix_object::compute_hash(kind, gix_object::Kind::Blob, content.as_bytes()).expect("valid object hash")
}

fn options() -> Options {
    let mut options = Options::default();
    options.expire = Some(gix_date::Time::new(EXPIRE_SECONDS, 0));
    options
}

/// Every file and directory below the object directory but `info`, relative to it, as a sorted set.
fn listing(objects_dir: &Path) -> TestResult<BTreeSet<String>> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeSet<String>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let relative = path
                .strip_prefix(root)
                .expect("below the root")
                .to_string_lossy()
                .into_owned();
            if relative == "info" {
                continue;
            }
            if path.is_dir() {
                out.insert(format!("{relative}/"));
                walk(&path, root, out)?;
            } else {
                out.insert(relative);
            }
        }
        Ok(())
    }
    let mut out = BTreeSet::new();
    walk(objects_dir, objects_dir, &mut out)?;
    Ok(out)
}

fn objects_dir(fixture: &gix_testtools::tempfile::TempDir) -> std::path::PathBuf {
    fixture.path().join("repo/.git/objects")
}

#[test]
fn what_remains_is_what_git_prune_keeps() -> TestResult {
    for kind in kinds() {
        let (git, native) = (fixture(kind)?, fixture(kind)?);
        let before = listing(&objects_dir(&native))?;
        gix_testtools::git(git.path().join("repo"), &format!("prune --expire {EXPIRE_FOR_GIT}"))?;

        let repo = gix::open_opts(native.path().join("repo"), crate::restricted())?;
        let outcome = repo.prune(options())?;

        let (expected, actual) = (listing(&objects_dir(&git))?, listing(&objects_dir(&native))?);
        assert_ne!(before, actual, "{kind}: something was removed, or the fixture is wrong");
        assert_eq!(
            actual, expected,
            "{kind}: the same loose objects, temporary files and directories remain as with `git prune`"
        );
        assert!(
            outcome.removed.contains(&blob_id(kind, "old-dangling\n")),
            "{kind}: the old dangling blob is reported as removed"
        );
        assert!(
            !outcome.removed.contains(&blob_id(kind, "new-dangling\n")),
            "{kind}: the recent dangling blob is kept"
        );
        assert_eq!(
            outcome.temporary, 3,
            "{kind}: the old `tmp_obj_*` file, `tmp_dir_old` and `tmp_pack_old` go, and the ones that are new stay"
        );
        assert!(
            outcome.removed_as_packed > 0,
            "{kind}: loose copies of what a pack has are removed as well"
        );
        assert!(
            outcome.failed.is_empty(),
            "{kind}: nothing was left behind: {:?}",
            outcome.failed
        );
    }
    Ok(())
}

#[test]
fn a_commit_graph_that_names_a_removed_commit_is_removed_with_it() -> TestResult {
    for kind in kinds() {
        let (git, native) = (fixture(kind)?, fixture(kind)?);
        let graph = |fixture: &gix_testtools::tempfile::TempDir| objects_dir(fixture).join("info/commit-graph");
        assert!(
            graph(&native).is_file(),
            "{kind}: the fixture has a commit-graph with the doomed commit"
        );
        gix_testtools::git(git.path().join("repo"), &format!("prune --expire {EXPIRE_FOR_GIT}"))?;
        assert!(
            graph(&git).is_file(),
            "{kind}: Git leaves a commit-graph behind that names a commit it removed, which is the reason to do better"
        );

        let repo = gix::open_opts(native.path().join("repo"), crate::restricted())?;
        let outcome = repo.prune(options())?;
        assert!(
            outcome.commit_graph_removed,
            "{kind}: the commit-graph is reported as removed"
        );
        assert!(
            !graph(&native).exists(),
            "{kind}: no commit-graph remains that names a commit that is gone"
        );
    }
    Ok(())
}

#[test]
fn a_dry_run_reports_what_would_happen_and_changes_nothing() -> TestResult {
    for kind in kinds() {
        let (dry, real) = (fixture(kind)?, fixture(kind)?);
        let before = listing(&objects_dir(&dry))?;
        let mut dry_options = options();
        dry_options.dry_run = true;

        let dry_outcome = gix::open_opts(dry.path().join("repo"), crate::restricted())?.prune(dry_options)?;
        assert_eq!(listing(&objects_dir(&dry))?, before, "{kind}: nothing was removed");
        assert!(
            objects_dir(&dry).join("info/commit-graph").is_file(),
            "{kind}: the commit-graph remains as well"
        );

        let real_outcome = gix::open_opts(real.path().join("repo"), crate::restricted())?.prune(options())?;
        assert_eq!(
            (
                dry_outcome.removed,
                dry_outcome.removed_as_packed,
                dry_outcome.temporary,
                dry_outcome.commit_graph_removed
            ),
            (
                real_outcome.removed,
                real_outcome.removed_as_packed,
                real_outcome.temporary,
                real_outcome.commit_graph_removed
            ),
            "{kind}: a dry run reports what the real one does"
        );
    }
    Ok(())
}

#[test]
fn without_an_expiry_everything_that_is_unreachable_goes_like_it_does_in_git() -> TestResult {
    for kind in kinds() {
        let (git, native) = (fixture(kind)?, fixture(kind)?);
        gix_testtools::git(git.path().join("repo"), "prune")?;
        let repo = gix::open_opts(native.path().join("repo"), crate::restricted())?;
        repo.prune(Options::default())?;
        assert_eq!(
            listing(&objects_dir(&native))?,
            listing(&objects_dir(&git))?,
            "{kind}: even recent objects are removed if they aren't reachable, and temporary files go"
        );
    }
    Ok(())
}

#[test]
fn prune_packed_removes_what_git_prune_packed_removes() -> TestResult {
    for kind in kinds() {
        let (git, native) = (fixture(kind)?, fixture(kind)?);
        gix_testtools::git(git.path().join("repo"), "prune-packed")?;
        let repo = gix::open_opts(native.path().join("repo"), crate::restricted())?;
        let count = repo.prune_packed(PackedOptions::default())?;
        assert!(count > 0, "{kind}: the fixture has loose copies of packed objects");
        assert_eq!(
            listing(&objects_dir(&native))?,
            listing(&objects_dir(&git))?,
            "{kind}: the same loose objects remain"
        );
    }
    Ok(())
}

#[test]
fn repositories_that_may_not_have_all_their_objects_are_left_alone() -> TestResult {
    let dir = fixture(Kind::Sha1)?;
    let repo_dir = dir.path().join("repo");
    let before = listing(&objects_dir(&dir))?;

    std::fs::write(repo_dir.join(".git/objects/pack/pack-fake.promisor"), b"")?;
    let repo = gix::open_opts(&repo_dir, crate::restricted())?;
    let err = repo
        .prune(options())
        .expect_err("promisor packs may be what some objects are fetched from later");
    assert!(format!("{err:?}").contains("promisor"), "the reason is given: {err:?}");
    std::fs::remove_file(repo_dir.join(".git/objects/pack/pack-fake.promisor"))?;

    gix_testtools::git(&repo_dir, "config core.repositoryformatversion 1")?;
    gix_testtools::git(&repo_dir, "config extensions.preciousObjects true")?;
    let repo = gix::open_opts(&repo_dir, crate::restricted())?;
    let err = repo
        .prune(options())
        .expect_err("a repository whose objects are precious is never pruned");
    assert!(format!("{err:?}").contains("precious"), "the reason is given: {err:?}");
    repo.prune_packed(PackedOptions::default())
        .expect_err("neither is it pruned of packed objects");
    assert_eq!(listing(&objects_dir(&dir))?, before, "nothing was removed");
    Ok(())
}

#[test]
fn a_shallow_file_loses_the_commits_that_nothing_reaches_any_more() -> TestResult {
    for kind in kinds() {
        let (git, native) = (fixture(kind)?, fixture(kind)?);
        let shallow_file = |fixture: &gix_testtools::tempfile::TempDir| fixture.path().join("shallow/.git/shallow");
        assert!(
            shallow_file(&native).is_file(),
            "{kind}: the fixture is a shallow repository"
        );
        gix_testtools::git(git.path().join("shallow"), &format!("prune --expire {EXPIRE_FOR_GIT}"))?;
        assert!(
            !shallow_file(&git).exists(),
            "{kind}: Git removes the only commit of the shallow file, as nothing has it"
        );

        let repo = gix::open_opts(native.path().join("shallow"), crate::restricted())?;
        let outcome = repo.prune(options())?;
        assert_eq!(
            outcome.unshallowed, 1,
            "{kind}: the commit is reported as removed from the file"
        );
        assert!(
            !shallow_file(&native).exists(),
            "{kind}: and the file is gone with the last of its commits, like with Git"
        );
    }
    Ok(())
}
