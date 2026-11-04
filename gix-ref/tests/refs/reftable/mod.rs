use std::path::PathBuf;

use gix_ref::{Reference, Target, reftable::Store};
use gix_testtools::TestResult;

use crate::{fixture_hash_kind, scripted_fixture_read_only};

mod git_cases;
mod read;
mod write;

/// Return the root of the fixture, which has the repository `repo`, its linked worktree `w1` and what Git says in `expected/`.
fn fixture() -> TestResult<PathBuf> {
    Ok(scripted_fixture_read_only("make_reftable_repo.sh")?)
}

/// The store of the main worktree.
fn main_store() -> TestResult<Store> {
    Ok(Store::at(fixture()?.join("repo/.git"), fixture_hash_kind()))
}

/// The store of the linked worktree `w1`.
fn worktree_store() -> TestResult<Store> {
    let root = fixture()?;
    Ok(Store::for_linked_worktree(
        root.join("repo/.git/worktrees/w1"),
        root.join("repo/.git"),
        fixture_hash_kind(),
    ))
}

/// What Git reported in the fixture.
fn expected(name: &str) -> TestResult<String> {
    Ok(std::fs::read_to_string(fixture()?.join("expected").join(name))?)
}

/// Format a reference like `git for-each-ref --format='%(refname) %(objectname) %(symref) %(*objectname)'` does.
fn format_like_git(reference: &Reference) -> String {
    let (id, symref) = match &reference.target {
        Target::Object(id) => (id.to_string(), String::new()),
        Target::Symbolic(name) => (String::new(), name.to_string()),
    };
    let peeled = reference.peeled.map(|id| id.to_string()).unwrap_or_default();
    format!("{} {id} {symref} {peeled}", reference.name)
}
