//! Listing the references that have a reflog, with `git reflog list` as the oracle for what each worktree sees.
//! See `make_reflog_names_repo.sh` for what the repository has.
use gix_ref::{FullName, Store};
use gix_testtools::{Creation, TestResult};

use crate::fixture_hash_kind;

struct Fixture {
    dir: gix_testtools::tempfile::TempDir,
    format: &'static str,
}

impl Fixture {
    fn new(format: &'static str) -> TestResult<Self> {
        let dir = gix_testtools::scripted_fixture_writable_with_args(
            "make_reflog_names_repo.sh",
            [format],
            Creation::Execute,
        )?;
        Ok(Fixture { dir, format })
    }

    fn store(&self, linked: bool) -> Store {
        let git_dir = self.dir.path().join("repo/.git");
        let main: Store = match self.format {
            "files" => Store::at(git_dir.clone(), fixture_hash_kind()).expect("a files store opens"),
            #[cfg(feature = "reftable")]
            "reftable" => gix_ref::reftable::Store::at(git_dir.clone(), fixture_hash_kind()).into(),
            unsupported => unreachable!("{unsupported} isn't a format of this test"),
        };
        if linked {
            main.with_locations(git_dir.join("worktrees/w1"), Some(git_dir))
        } else {
            main
        }
    }

    /// The names Git lists, as seen from the main worktree or the linked one, sorted.
    fn git_lists(&self, linked: bool) -> TestResult<Vec<String>> {
        let cwd = self.dir.path().join(if linked { "w1" } else { "repo" });
        let mut names: Vec<_> = gix_testtools::git(cwd, "reflog list")?
            .lines()
            .map(ToOwned::to_owned)
            .collect();
        names.sort();
        Ok(names)
    }
}

fn names(store: &Store) -> TestResult<Vec<String>> {
    Ok(store.as_ref().reflog_names()?.iter().map(FullName::to_string).collect())
}

fn formats() -> Vec<&'static str> {
    let mut formats = vec!["files"];
    if cfg!(feature = "reftable") {
        formats.push("reftable");
    }
    formats
}

#[test]
fn the_worktree_sees_the_reflogs_git_lists() -> TestResult {
    for format in formats() {
        let f = Fixture::new(format)?;
        let main = names(&f.store(false))?;
        let main_without_linked_ones: Vec<_> = main
            .iter()
            .filter(|name| !name.starts_with("worktrees/"))
            .cloned()
            .collect();
        assert_eq!(
            main_without_linked_ones,
            f.git_lists(false)?,
            "{format}: the main worktree lists what `git reflog list` lists there"
        );
        assert_eq!(
            names(&f.store(true))?,
            f.git_lists(true)?,
            "{format}: so does a linked worktree, which has its private references and shares the others"
        );
    }
    Ok(())
}

#[test]
fn the_main_worktree_also_sees_the_private_reflogs_of_linked_ones() -> TestResult {
    for format in formats() {
        let f = Fixture::new(format)?;
        let linked: Vec<_> = names(&f.store(false))?
            .into_iter()
            .filter(|name| name.starts_with("worktrees/"))
            .collect();
        assert_eq!(
            linked,
            [
                "worktrees/w1/HEAD",
                "worktrees/w1/refs/bisect/w1-side",
                "worktrees/w1/refs/worktree/w1-note"
            ],
            "{format}: they are named the way the references of linked worktrees are, and sorted"
        );
        assert!(
            !names(&f.store(true))?
                .iter()
                .any(|name| name.starts_with("worktrees/") || name.starts_with("main-worktree/")),
            "{format}: a linked worktree doesn't see the private reflogs of the main one or of any other worktree"
        );
    }
    Ok(())
}

#[test]
fn deleted_and_missing_reflogs_are_not_listed() -> TestResult {
    for format in formats() {
        let f = Fixture::new(format)?;
        let names = names(&f.store(false))?;
        assert!(
            !names.iter().any(|name| name.contains("deleted-branch")),
            "{format}: the reflog of a branch that was deleted is gone"
        );
        assert!(
            names.iter().any(|name| name == "refs/heads/feature/x"),
            "{format}: nested names are found"
        );
        assert!(
            !names.iter().any(|name| name == "refs/tags/anything"),
            "{format}: references without reflog aren't listed"
        );
    }
    Ok(())
}
