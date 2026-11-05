//! Packing references for `reftable` merges tables, which must not change what Git sees. See `make_reftable_repo.sh` for the
//! repository, which has a stack for the main worktree and a smaller one for the linked worktree `w1`.
use std::path::Path;

use gix_ref::{StoreRef, pack_refs::Options, reftable::Store};
use gix_testtools::{Creation, TestResult};

use crate::{fixture_hash_kind, scripted_fixture_writable_with_args};

fn tables(stack_dir: &Path) -> TestResult<usize> {
    Ok(std::fs::read_to_string(stack_dir.join("reftable/tables.list"))?
        .lines()
        .count())
}

fn table_files(stack_dir: &Path) -> TestResult<usize> {
    Ok(std::fs::read_dir(stack_dir.join("reftable"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".ref"))
        .count())
}

fn what_git_sees(repo: &Path) -> TestResult<String> {
    Ok(format!(
        "{}\n{}\n{}",
        gix_testtools::git(
            repo,
            "for-each-ref --format=%(refname)%20%(objectname)%20%(symref)%20%(*objectname)"
        )?,
        gix_testtools::git(repo, "reflog list")?,
        gix_testtools::git(repo, "log -g --format=%H%x20%gs main")?
    ))
}

#[test]
fn compacting_the_stack_of_a_worktree_leaves_what_git_sees_unchanged() -> TestResult {
    let dir = scripted_fixture_writable_with_args("make_reftable_repo.sh", None::<String>, Creation::Execute)?;
    let (repo, git_dir) = (dir.path().join("repo"), dir.path().join("repo/.git"));
    let before = what_git_sees(&repo)?;
    let tables_before = tables(&git_dir)?;
    assert!(tables_before > 1, "the fixture has a stack with many tables to merge");

    let store = Store::at(git_dir.clone(), fixture_hash_kind());
    let mut told = 0;
    let outcome = StoreRef::from(&store).pack_refs(
        &gix_object::find::Never,
        Options::default(),
        Some(&mut |_: &gix_ref::transaction::observe::Event<'_>| {
            told += 1;
            Ok(())
        }),
    )?;
    assert_eq!(
        told, 0,
        "Git doesn't run the reference-transaction hook to merge tables either"
    );

    assert_eq!(tables(&git_dir)?, 1, "all tables were merged into one");
    assert_eq!(
        table_files(&git_dir)?,
        1,
        "and the tables that were merged are gone, as nothing lists them anymore"
    );
    assert_eq!(outcome.merged_tables, tables_before, "{outcome:?}");
    assert_eq!(
        what_git_sees(&repo)?,
        before,
        "references and reflogs are the same, with all that was deleted or expired not coming back"
    );

    let outcome = StoreRef::from(&store).pack_refs(
        &gix_object::find::Never,
        {
            let mut options = Options::default();
            options.auto = true;
            options
        },
        None,
    )?;
    assert_eq!(
        outcome.merged_tables, 0,
        "a single table has nothing to merge, even automatically"
    );
    assert!(
        !StoreRef::from(&store).pack_refs_required(&gix_object::find::Never)?,
        "and it isn't due any more"
    );
    Ok(())
}

#[test]
fn a_linked_worktree_compacts_only_its_own_stack() -> TestResult {
    let dir = scripted_fixture_writable_with_args("make_reftable_repo.sh", None::<String>, Creation::Execute)?;
    let (repo, git_dir) = (dir.path().join("w1"), dir.path().join("repo/.git"));
    let worktree_dir = git_dir.join("worktrees/w1");
    let (main_before, own_before) = (tables(&git_dir)?, tables(&worktree_dir)?);
    let before = what_git_sees(&repo)?;

    let store = Store::for_linked_worktree(worktree_dir.clone(), git_dir.clone(), fixture_hash_kind());
    StoreRef::from(&store).pack_refs(&gix_object::find::Never, Options::default(), None)?;

    assert_eq!(
        tables(&git_dir)?,
        main_before,
        "the shared stack is left to the main worktree, like in Git"
    );
    assert_eq!(
        tables(&worktree_dir)?,
        1,
        "its own stack was merged, from {own_before} tables"
    );
    assert_eq!(what_git_sees(&repo)?, before);
    Ok(())
}

#[test]
fn merging_is_due_if_automatic_merging_would_merge_something() -> TestResult {
    let dir = scripted_fixture_writable_with_args("make_reftable_repo.sh", None::<String>, Creation::Execute)?;
    let git_dir = dir.path().join("repo/.git");
    let store = Store::at(git_dir.clone(), fixture_hash_kind());
    let required = StoreRef::from(&store).pack_refs_required(&gix_object::find::Never)?;
    let mut options = Options::default();
    options.auto = true;
    let outcome = StoreRef::from(&store).pack_refs(&gix_object::find::Never, options, None)?;
    assert_eq!(
        required,
        outcome.merged_tables > 0,
        "it is due exactly if the automatic merge does something: {outcome:?}"
    );

    // Tables that break the sequence: the same as many small tables on top of what is there.
    let linked = Store::for_linked_worktree(git_dir.join("worktrees/w1"), git_dir.clone(), fixture_hash_kind());
    let before = tables(&git_dir.join("worktrees/w1"))?;
    let required = StoreRef::from(&linked).pack_refs_required(&gix_object::find::Never)?;
    let outcome = StoreRef::from(&linked).pack_refs(&gix_object::find::Never, options, None)?;
    assert_eq!(
        required,
        outcome.merged_tables > 0,
        "also for the stack of a worktree, which has {before} tables: {outcome:?}"
    );
    StoreRef::from(&linked).pack_refs(&gix_object::find::Never, Options::default(), None)?;
    assert!(
        !StoreRef::from(&linked).pack_refs_required(&gix_object::find::Never)?,
        "a single table is never due"
    );
    Ok(())
}
