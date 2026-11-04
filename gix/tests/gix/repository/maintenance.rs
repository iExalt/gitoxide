//! Finding what is reachable, with Git as the oracle for which objects that includes. See `make_maintenance_roots_repo.sh` for
//! everything that keeps an object, and for what doesn't.
use std::collections::BTreeSet;

use gix::maintenance::reachable::Options;
use gix_hash::Kind;
use gix_testtools::{Creation, TestResult};

fn kinds() -> impl Iterator<Item = Kind> {
    [Kind::Sha1, Kind::Sha256]
        .into_iter()
        .filter(|kind| *kind == Kind::Sha1 || cfg!(feature = "sha256"))
}

fn blob_id(kind: Kind, content: &str) -> gix_hash::ObjectId {
    gix_object::compute_hash(kind, gix_object::Kind::Blob, content.as_bytes()).expect("valid object hash")
}

/// Everything Git finds with `--objects` from every kind of root, with replacements switched off as `prune` does.
fn git_reachable(repo_dir: &std::path::Path) -> TestResult<BTreeSet<String>> {
    Ok(gix_testtools::git(
        repo_dir,
        "--no-replace-objects rev-list --objects --all --reflog --indexed-objects",
    )?
    .lines()
    .map(|line| line.split(' ').next().expect("an id").to_owned())
    .collect())
}

fn reachable(repo: &gix::Repository) -> TestResult<BTreeSet<String>> {
    let roots = repo.reachability_roots(Options::default())?;
    Ok(repo.reachable_objects(roots)?.iter().map(ToString::to_string).collect())
}

#[test]
fn reachable_objects_are_the_ones_git_finds_from_all_its_kinds_of_roots() -> TestResult {
    for kind in kinds() {
        let fixtures =
            gix_testtools::scripted_fixture_read_only_with_args("make_maintenance_roots_repo.sh", [kind.to_string()])?;
        let expected = git_reachable(&fixtures.join("repo"))?;
        for worktree in ["repo", "w1", "w2"] {
            let repo = gix::open_opts(fixtures.join(worktree), crate::restricted())?;
            assert_eq!(
                reachable(&repo)?,
                expected,
                "{kind} {worktree}: the same objects as `git rev-list --objects --all --reflog --indexed-objects`, \
                 whichever worktree is used to ask for them"
            );
        }

        let mut repo = gix::open_opts(fixtures.join("repo"), crate::restricted())?;
        repo.set_namespace("someone-else")?;
        assert_eq!(
            reachable(&repo)?,
            expected,
            "{kind}: a namespace shows only some references, but everything that any namespace refers to is wanted"
        );
        repo.clear_namespace();
        let found = reachable(&repo)?;
        for (content, why) in [
            (
                "tagged\n",
                "`feature` has it, which is replaced, and replacements aren't followed",
            ),
            ("their-side\n", "the index has it"),
            ("theirs\n", "only the resolve-undo extension of the index remembers it"),
            ("reflog\n", "only the reflog of HEAD remembers it"),
            ("stash\n", "`refs/stash` has it"),
            ("w1-staged\n", "the index of a linked worktree has it"),
            (
                "w2\n",
                "a detached HEAD of a linked worktree points to the commit with it",
            ),
            ("blob-tag\n", "a tag points to the blob"),
        ] {
            assert!(found.contains(&blob_id(kind, content).to_string()), "{kind}: {why}");
        }
        assert!(
            !found.contains(&blob_id(kind, "dangling\n").to_string()),
            "{kind}: nothing refers to a dangling blob, so it isn't reachable"
        );
    }
    Ok(())
}

#[test]
fn objects_behind_references_must_exist_but_those_behind_reflogs_may_be_missing() -> TestResult {
    for kind in kinds() {
        let fixtures = gix_testtools::scripted_fixture_writable_with_args(
            "make_maintenance_roots_repo.sh",
            [kind.to_string()],
            Creation::Execute,
        )?;
        let repo_dir = fixtures.path().join("repo");
        let loose_path = |id: &str| repo_dir.join(".git/objects").join(&id[..2]).join(&id[2..]);
        let tree_of = |commit: &str| -> TestResult<String> {
            Ok(gix_testtools::git(&repo_dir, &format!("rev-parse {commit}^{{tree}}"))?
                .trim()
                .to_owned())
        };

        let reflog_only_commit = gix_testtools::git(&repo_dir, "log --all --reflog --grep=reflog.only --format=%H -1")?
            .trim()
            .to_owned();
        std::fs::remove_file(loose_path(&tree_of(&reflog_only_commit)?))?;
        let repo = gix::open_opts(&repo_dir, crate::restricted())?;
        let found = reachable(&repo)?;
        assert!(
            found.contains(&reflog_only_commit),
            "{kind}: the commit that only a reflog remembers is found even though its tree isn't there"
        );

        std::fs::remove_file(loose_path(&tree_of("main")?))?;
        let err = reachable(&repo).expect_err("the tree of the commit that a branch points to is gone");
        assert!(
            format!("{err:?}").contains("missing"),
            "{kind}: it's reported as missing: {err:?}"
        );
    }
    Ok(())
}
