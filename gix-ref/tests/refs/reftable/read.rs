use gix_ref::{PartialNameRef, reftable::Store};
use gix_testtools::TestResult;

use super::*;

/// References that `git for-each-ref` leaves out as they don't resolve to an object, whereas iterators of stores list everything.
const BROKEN: [&str; 3] = ["refs/heads/dangling", "refs/heads/cycle-a", "refs/heads/cycle-b"];

/// Return one line per reference like `git for-each-ref` shows it, as far as an iterator can tell: symbolic references
/// are not resolved, so they only show where they point to.
fn all_like_git(store: &Store) -> TestResult<Vec<String>> {
    let platform = store.iter()?;
    let mut lines = Vec::new();
    for reference in platform.all()? {
        let reference = reference?;
        if !BROKEN.contains(&reference.name.to_string().as_str()) {
            lines.push(format_like_git(&reference));
        }
    }
    Ok(lines)
}

/// Return what Git says without the object id of symbolic references, which iterators don't resolve.
fn expected_without_resolved_symrefs(name: &str) -> TestResult<Vec<String>> {
    Ok(expected(name)?
        .lines()
        .map(|line| {
            let mut fields = line.splitn(4, ' ');
            let (name, id, symref, peeled) = (
                fields.next().expect("name"),
                fields.next().expect("id"),
                fields.next().expect("symref"),
                fields.next().expect("peeled"),
            );
            let id = if symref.is_empty() { id } else { "" };
            format!("{name} {id} {symref} {peeled}")
        })
        .collect())
}

#[test]
fn iteration_of_all_references_matches_git_in_the_main_and_a_linked_worktree() -> TestResult {
    for (store, expected_name) in [(main_store()?, "repo.refs"), (worktree_store()?, "w1.refs")] {
        assert_eq!(
            all_like_git(&store)?,
            expected_without_resolved_symrefs(expected_name)?,
            "{expected_name}: same references in the same order, with the same targets and peeled values"
        );
    }
    Ok(())
}

#[test]
fn partial_names_are_completed_and_symbolic_references_resolved_like_git() -> TestResult {
    for (store, expected_name) in [(main_store()?, "repo.resolved"), (worktree_store()?, "w1.resolved")] {
        let snapshot = store.snapshot()?;
        for line in expected(expected_name)?.lines() {
            let (name, expected_id) = line.split_once(' ').expect("name and id");
            if name.contains('^') {
                continue;
            }
            let partial: &PartialNameRef = name.try_into()?;
            let actual = match store.try_find_resolved(partial) {
                Ok(Some(mut reference)) => snapshot
                    .follow_to_object(&mut reference)
                    .map_or_else(|_| "missing".into(), |id| id.to_string()),
                _ => "missing".into(),
            };
            assert_eq!(actual, expected_id, "{expected_name}: {name}");
        }
    }
    Ok(())
}

fn format_offset(offset: i32) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let offset = offset.abs();
    format!("{sign}{:02}{:02}", offset / 3600, offset % 3600 / 60)
}

#[test]
fn reflogs_match_git_including_time_zones_with_minutes_and_the_marker_of_an_emptied_log() -> TestResult {
    for (store, prefix) in [(main_store()?, "repo"), (worktree_store()?, "w1")] {
        let store = gix_ref::Store::from(store);
        let store = store.as_ref();
        for (name, file_suffix) in [
            ("refs/heads/main", "refs_heads_main"),
            ("refs/heads/side", "refs_heads_side"),
            ("refs/heads/emptied", "refs_heads_emptied"),
            ("HEAD", "HEAD"),
        ] {
            let name: &gix_ref::FullNameRef = name.try_into()?;
            let mut platform = store.reflog(name);
            let ours: Vec<String> = platform
                .rev()?
                .expect("the log exists")
                .map(|line| {
                    let line = line?;
                    Ok(format!(
                        "{}\t{}@{{{} {}}}\t{}\t{}\t{}",
                        line.new_oid,
                        name.shorten(),
                        line.signature.time.seconds,
                        format_offset(line.signature.time.offset),
                        line.signature.name,
                        line.signature.email,
                        line.message
                    ))
                })
                .collect::<gix_error::Result<_>>()?;
            let theirs = expected(&format!("{prefix}.reflog.{file_suffix}"))?;
            assert_eq!(
                ours,
                theirs.lines().collect::<Vec<_>>(),
                "{prefix} {name}: Git's lines, most recent first, with the marker of an emptied log hidden"
            );
            assert!(
                store.reflog_exists(name)?,
                "{prefix} {name}: the log exists, even if there are no lines"
            );
            let oldest_first: Vec<_> = platform
                .all()?
                .expect("the log exists")
                .collect::<gix_error::Result<_>>()?;
            assert_eq!(
                oldest_first.len(),
                ours.len(),
                "{prefix} {name}: forward iteration has as many lines"
            );
        }
    }
    let store = gix_ref::Store::from(main_store()?);
    let store = store.as_ref();
    for (name, exists) in [
        ("refs/heads/feature", true),
        ("refs/heads/gone", false),
        ("refs/heads/never-existed", false),
    ] {
        let name: &gix_ref::FullNameRef = name.try_into()?;
        assert_eq!(
            store.reflog_exists(name)?,
            exists,
            "{name}: reflogs of deleted references are gone with their tombstones"
        );
        assert_eq!(store.reflog(name).rev()?.is_some(), exists, "{name}");
    }
    Ok(())
}

#[test]
fn pseudo_references_come_from_the_stack_for_root_references_and_from_files_for_fetch_and_merge_head() -> TestResult {
    let names = |store: Store| -> TestResult<Vec<String>> {
        let platform = store.iter()?;
        Ok(platform
            .pseudo()?
            .map(|r| r.map(|r| r.name.to_string()))
            .collect::<gix_error::Result<_>>()?)
    };
    assert_eq!(
        names(main_store()?)?,
        ["FETCH_HEAD", "HEAD", "MERGE_HEAD", "ORIG_HEAD"],
        "Git's root references of the main worktree, and the pseudo references it keeps in files"
    );
    assert_eq!(
        names(worktree_store()?)?,
        ["HEAD", "ORIG_HEAD"],
        "those of the linked worktree, which has no files of the others"
    );
    Ok(())
}

#[test]
fn a_namespace_limits_what_is_found_and_listed_and_is_stripped_from_names() -> TestResult {
    let mut store = gix_ref::Store::from(main_store()?);
    let without_namespace = store.as_ref().find("main")?;
    store.set_namespace(Some(gix_ref::namespace::expand("ns")?));
    let store = store.as_ref();
    let platform = store.iter()?;
    let names: Vec<_> = platform
        .all()?
        .map(|r| r.map(|r| r.name.to_string()))
        .collect::<gix_error::Result<_>>()?;
    assert_eq!(names, ["refs/heads/main"], "only the reference of the namespace");
    let found = store.find("main")?;
    assert_eq!(found.name.as_bstr(), "refs/heads/main", "the namespace is stripped");
    assert_ne!(
        found.target, without_namespace.target,
        "the namespaced reference isn't the one of the repository"
    );
    assert!(store.try_find("alias")?.is_none(), "other references are hidden");
    Ok(())
}

#[test]
fn a_snapshot_keeps_what_it_has_seen_while_lookups_see_what_others_wrote() -> TestResult {
    let tmp = gix_testtools::scripted_fixture_writable("make_reftable_repo.sh")?;
    let git_dir = tmp.path().join("repo/.git");
    let store = Store::at(git_dir.clone(), fixture_hash_kind());
    let before = store.find("refs/heads/main")?;
    let snapshot = store.snapshot()?;
    let iteration = store.iter()?;

    let new_id = gix_testtools::git(tmp.path().join("repo"), "rev-parse :/c1")?;
    gix_testtools::git(
        tmp.path().join("repo"),
        &format!("update-ref refs/heads/main {}", new_id.trim()),
    )?;

    assert_eq!(
        snapshot.find("refs/heads/main")?.target,
        before.target,
        "the snapshot still has the old value"
    );
    assert_ne!(
        store.find("refs/heads/main")?.target,
        before.target,
        "a lookup reloads the stack and sees what Git wrote"
    );
    let from_iteration = iteration
        .all()?
        .map(|r| r.expect("valid"))
        .find(|r| r.name.as_bstr() == "refs/heads/main")
        .expect("exists");
    assert_eq!(
        from_iteration.target, before.target,
        "an iterator stays at the state of the platform that created it"
    );
    let cloned = store.clone();
    assert_eq!(
        cloned.find("refs/heads/main")?.target,
        store.find("refs/heads/main")?.target,
        "clones see the same references"
    );
    Ok(())
}

#[test]
fn the_neutral_store_knows_what_it_is() -> TestResult {
    let store = gix_ref::Store::from(main_store()?);
    assert_eq!(store.as_ref().kind(), gix_ref::view::Kind::Reftable);
    assert!(store.as_file().is_none(), "this isn't a files store");
    assert!(store.as_reftable().is_some());
    assert_eq!(store.unsupported_backend(), None, "reftables are supported");
    let relocated = store.with_locations(store.git_dir().to_owned(), None);
    assert_eq!(
        relocated.as_ref().find("main")?.target,
        store.as_ref().find("main")?.target,
        "a relocated store reads the same references"
    );
    Ok(())
}

#[test]
fn a_store_marked_as_unsupported_refuses_every_operation_instead_of_reading_an_empty_stack() -> TestResult {
    let mut store = gix_ref::Store::from(main_store()?);
    store.set_unsupported_backend(Some("reftable://elsewhere".into()));
    assert_eq!(store.unsupported_backend(), Some("reftable://elsewhere"));
    let relocated = store.with_locations(store.git_dir().to_owned(), None);
    for (what, store) in [
        ("the store", &store),
        ("a relocated copy", &relocated),
        ("a clone", &store.clone()),
    ] {
        let store = store.as_ref();
        let name: &gix_ref::FullNameRef = "refs/heads/main".try_into()?;
        assert!(
            store
                .try_find("main")
                .expect_err("lookups are refused")
                .is_unsupported(),
            "{what}"
        );
        assert!(
            store
                .find("FETCH_HEAD")
                .expect_err("also the pseudo references in files")
                .is_unsupported(),
            "{what}"
        );
        assert!(
            store.snapshot().err().expect("snapshots are refused").is_unsupported(),
            "{what}"
        );
        assert!(
            store.iter().err().expect("iteration is refused").is_unsupported(),
            "{what}"
        );
        assert!(
            store
                .reflog_exists(name)
                .expect_err("reflogs are refused")
                .is_unsupported(),
            "{what}"
        );
        assert!(
            store
                .reflog(name)
                .rev()
                .err()
                .expect("reading reflogs is refused")
                .is_unsupported(),
            "{what}"
        );
        assert!(
            store
                .assure_backend_supported()
                .expect_err("it says so")
                .is_unsupported(),
            "{what}"
        );
    }
    let err = match store.as_ref().transaction().prepare(
        [],
        gix_ref::view::transaction::LockOptions::new(
            gix_lock::acquire::Fail::Immediately,
            gix_lock::acquire::Fail::Immediately,
        ),
    ) {
        Ok(_) => panic!("editing is refused"),
        Err(err) => err,
    };
    assert!(err.is_unsupported());
    store.set_unsupported_backend(None);
    assert!(store.as_ref().try_find("main")?.is_some(), "the mark can be reset");
    Ok(())
}

#[test]
fn reflogs_are_read_again_so_that_a_long_lived_store_sees_what_git_wrote() -> TestResult {
    let tmp = gix_testtools::scripted_fixture_writable("make_reftable_repo.sh")?;
    let repo = tmp.path().join("repo");
    let store = gix_ref::Store::from(Store::at(repo.join(".git"), fixture_hash_kind()));
    let store = store.as_ref();
    let side: &gix_ref::FullNameRef = "refs/heads/side".try_into()?;
    let new_branch: &gix_ref::FullNameRef = "refs/heads/newbranch".try_into()?;
    let count = |name| -> TestResult<usize> { Ok(store.reflog(name).rev()?.map_or(0, Iterator::count)) };
    let before = count(side)?;
    assert!(!store.reflog_exists(new_branch)?);

    let c1 = gix_testtools::git(&repo, "rev-parse :/c1")?;
    gix_testtools::git(&repo, &format!("update-ref -m probe refs/heads/side {}", c1.trim()))?;
    gix_testtools::git(&repo, &format!("update-ref refs/heads/newbranch {}", c1.trim()))?;

    assert_eq!(count(side)?, before + 1, "the new entry written by Git is read");
    assert!(
        store.reflog_exists(new_branch)?,
        "so is a log that didn't exist before, as `git reflog exists` says"
    );
    Ok(())
}
