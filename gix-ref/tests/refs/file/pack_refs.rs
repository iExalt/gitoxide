//! Packing references natively, with `git pack-refs --all --prune` as the oracle. See `make_pack_refs_repo.sh` for what
//! the repository has.
use std::path::{Path, PathBuf};

use gix_ref::pack_refs::Options;
use gix_testtools::{Creation, TestResult};

use crate::file::odb_at;

fn fixture() -> TestResult<(gix_testtools::tempfile::TempDir, gix_ref::file::Store, gix_odb::Handle)> {
    let dir = crate::scripted_fixture_writable_with_args("make_pack_refs_repo.sh", None::<String>, Creation::Execute)?;
    let git_dir = dir.path().join("repo/.git");
    let store = gix_ref::file::Store::at(git_dir.clone(), crate::fixture_hash_kind());
    let odb = odb_at(git_dir.join("objects"))?;
    Ok((dir, store, odb))
}

fn options(change: impl FnOnce(&mut Options)) -> Options {
    let mut options = Options::default();
    change(&mut options);
    options
}

/// All files below `refs/` and `packed-refs`, with directories, sorted, to compare what is left on disk.
fn layout(git_dir: &Path) -> TestResult<Vec<String>> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) -> TestResult {
        for entry in std::fs::read_dir(dir)? {
            let path: PathBuf = entry?.path();
            let relative = path.strip_prefix(base)?.to_string_lossy().into_owned();
            if path.is_dir() {
                out.push(format!("{relative}/"));
                walk(&path, base, out)?;
            } else {
                out.push(relative);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(&git_dir.join("refs"), git_dir, &mut out)?;
    out.sort();
    Ok(out)
}

#[test]
fn the_result_is_what_git_pack_refs_all_prune_leaves_behind() -> TestResult {
    let (git_fixture, ..) = fixture()?;
    let (_native_fixture, store, odb) = fixture()?;
    let (git_dir, native_dir) = (git_fixture.path().join("repo/.git"), store.git_dir().to_owned());

    gix_testtools::git(git_fixture.path().join("repo"), "pack-refs --all --prune")?;
    let outcome = gix_ref::StoreRef::from(&store).pack_refs(&odb, Options::default(), None)?;

    assert_eq!(
        std::fs::read_to_string(native_dir.join("packed-refs"))?,
        std::fs::read_to_string(git_dir.join("packed-refs"))?,
        "packed-refs is the same, with the loose values on top of what was packed before, and peeled values for tags"
    );
    assert_eq!(
        layout(&native_dir)?,
        layout(&git_dir)?,
        "the same loose references remain, and the directories they emptied are gone"
    );
    assert_eq!(
        std::fs::read_dir(native_dir.join("logs"))?.count(),
        std::fs::read_dir(git_dir.join("logs"))?.count(),
        "reflogs are not touched"
    );
    assert!(
        outcome.packed >= outcome.pruned && outcome.pruned > 0,
        "{outcome:?}: it reports what it did, and not everything that was packed could be pruned as one is locked"
    );
    assert_eq!(outcome.skipped, 1, "the locked reference was skipped: {outcome:?}");
    Ok(())
}

#[test]
fn without_pruning_the_loose_references_stay() -> TestResult {
    let (_dir, store, odb) = fixture()?;
    let before = layout(store.git_dir())?;
    let outcome = gix_ref::StoreRef::from(&store).pack_refs(&odb, options(|options| options.prune = false), None)?;
    assert_eq!(layout(store.git_dir())?, before, "nothing is deleted");
    assert_eq!((outcome.pruned, outcome.skipped), (0, 0));
    assert!(outcome.packed > 0);
    Ok(())
}

#[test]
fn a_namespace_does_not_limit_what_is_packed() -> TestResult {
    let (git_fixture, ..) = fixture()?;
    let (_dir, mut store, odb) = fixture()?;
    gix_testtools::git(git_fixture.path().join("repo"), "pack-refs --all --prune")?;
    store.namespace = Some(gix_ref::namespace::expand("elsewhere")?);
    gix_ref::StoreRef::from(&store).pack_refs(&odb, Options::default(), None)?;
    assert_eq!(
        std::fs::read_to_string(store.git_dir().join("packed-refs"))?,
        std::fs::read_to_string(git_fixture.path().join("repo/.git/packed-refs"))?,
        "packed-refs isn't affected by the namespace of the store, like in Git"
    );
    Ok(())
}

mod auto {
    use gix_testtools::TestResult;

    fn repo_with_loose_branches(
        count: usize,
    ) -> TestResult<(gix_testtools::tempfile::TempDir, gix_ref::file::Store, gix_odb::Handle)> {
        let dir = gix_testtools::tempfile::TempDir::new()?;
        gix_testtools::git(dir.path(), "init -q -b main")?;
        gix_testtools::git(dir.path(), "commit -q --allow-empty -m c1")?;
        for idx in 1..count {
            gix_testtools::git(dir.path(), &format!("branch b{idx}"))?;
        }
        let git_dir = dir.path().join(".git");
        let store = gix_ref::file::Store::at(git_dir.clone(), crate::fixture_hash_kind());
        let odb = crate::file::odb_at(git_dir.join("objects"))?;
        Ok((dir, store, odb))
    }

    #[test]
    fn packing_waits_for_sixteen_loose_references_if_there_are_no_packed_ones() -> TestResult {
        for (count, packs) in [(15, false), (16, true)] {
            let (git_repo, ..) = repo_with_loose_branches(count)?;
            gix_testtools::git(git_repo.path(), "pack-refs --auto --all --prune")?;
            assert_eq!(
                git_repo.path().join(".git/packed-refs").exists(),
                packs,
                "{count} references: Git packs them or not depending on the threshold"
            );

            let (native_repo, store, odb) = repo_with_loose_branches(count)?;
            let outcome =
                gix_ref::StoreRef::from(&store).pack_refs(&odb, super::options(|options| options.auto = true), None)?;
            assert_eq!(
                native_repo.path().join(".git/packed-refs").exists(),
                packs,
                "{count} references: the same decision as Git, {outcome:?}"
            );
        }
        Ok(())
    }
}

/// What the `reference-transaction` hook of Git and an observer of the native implementation see for packing references.
mod hook {
    use std::{fmt::Write, path::Path};

    use gix_ref::{
        Target,
        pack_refs::Options,
        transaction::observe::{Event, State, Veto},
    };
    use gix_testtools::TestResult;

    use super::{fixture, layout};

    /// Install a hook that logs its argument and its standard input in a way that is easy to compare with
    /// [`format()`], and let it refuse the state in `$VETO` if that is set.
    fn log_hook(git_dir: &Path, log: &Path) -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let path = git_dir.join("hooks/reference-transaction");
        std::fs::create_dir_all(path.parent().expect("a hooks directory"))?;
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"$1\" >> {log}\nsed 's/^/  /' >> {log}\n[ \"$VETO\" = \"$1\" ] && exit 1\nexit 0\n",
                log = log.display()
            ),
        )?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    fn format(event: &Event<'_>, null: &gix_hash::oid) -> String {
        let value = |target: Option<&Target>| match target {
            Some(Target::Object(id)) => id.to_string(),
            Some(Target::Symbolic(_)) => unreachable!("only objects are packed"),
            None => null.to_string(),
        };
        let mut out = format!("{}\n", event.state.as_str());
        for update in event.updates {
            writeln!(
                out,
                "  {} {} {}",
                value(update.previous.as_ref()),
                value(update.new.as_ref()),
                update.name
            )
            .expect("writing to a string doesn't fail");
        }
        out
    }

    /// Return what the observer saw as text, refusing `veto` as the hook of Git does.
    fn observe(
        seen: &mut String,
        null: gix_hash::ObjectId,
        veto: Option<State>,
    ) -> impl FnMut(&Event<'_>) -> Result<(), Veto> + '_ {
        move |event: &Event<'_>| {
            seen.push_str(&format(event, &null));
            if veto == Some(event.state) { Err(Veto) } else { Ok(()) }
        }
    }

    #[test]
    fn what_the_observer_sees_is_what_the_hook_of_git_sees() -> TestResult {
        let (git_fixture, ..) = fixture()?;
        let (_native, store, odb) = fixture()?;
        let log = git_fixture.path().join("hook.log");
        log_hook(&git_fixture.path().join("repo/.git"), &log)?;
        gix_testtools::git(git_fixture.path().join("repo"), "pack-refs --all --prune")?;

        let mut seen = String::new();
        let null = crate::fixture_hash_kind().null();
        gix_ref::StoreRef::from(&store).pack_refs(
            &odb,
            Options::default(),
            Some(&mut observe(&mut seen, null, None)),
        )?;
        assert_eq!(
            seen,
            std::fs::read_to_string(&log)?,
            "one transaction that packs everything, then one for each loose reference that is pruned, in the opposite order, \
             and the reference that is locked is only reported as preparing"
        );
        Ok(())
    }

    #[test]
    fn nothing_to_pack_is_still_reported_like_git_does() -> TestResult {
        let (git_fixture, ..) = fixture()?;
        let (_native, store, odb) = fixture()?;
        // Everything that can be packed is, so that there is nothing left when the hook is installed. The locked reference
        // remains loose, but isn't the one that matters.
        gix_testtools::git(git_fixture.path().join("repo"), "pack-refs --all --prune")?;
        gix_ref::StoreRef::from(&store).pack_refs(&odb, Options::default(), None)?;
        for dir in [git_fixture.path().join("repo/.git"), store.git_dir().to_owned()] {
            std::fs::remove_dir_all(dir.join("refs/heads"))?;
            std::fs::remove_dir_all(dir.join("refs/remotes"))?;
        }

        let log = git_fixture.path().join("hook.log");
        log_hook(&git_fixture.path().join("repo/.git"), &log)?;
        gix_testtools::git(git_fixture.path().join("repo"), "pack-refs --all --prune")?;
        let mut seen = String::new();
        let null = crate::fixture_hash_kind().null();
        gix_ref::StoreRef::from(&store).pack_refs(
            &odb,
            Options::default(),
            Some(&mut observe(&mut seen, null, None)),
        )?;
        assert_eq!(seen, std::fs::read_to_string(&log)?);
        assert_eq!(
            seen, "preparing\nprepared\ncommitted\n",
            "an empty transaction, which Git runs as well"
        );
        Ok(())
    }

    #[test]
    fn refusing_to_pack_changes_nothing_and_reports_the_abort() -> TestResult {
        for veto in [State::Preparing, State::Prepared] {
            let (git_fixture, ..) = fixture()?;
            let (_native, store, odb) = fixture()?;
            let log = git_fixture.path().join("hook.log");
            log_hook(&git_fixture.path().join("repo/.git"), &log)?;
            let status = gix_testtools::git_command(git_fixture.path().join("repo"))
                .env("VETO", veto.as_str())
                .args(["pack-refs", "--all", "--prune"])
                .status()?;
            assert!(!status.success(), "Git fails if the hook refuses");

            let before = layout(store.git_dir())?;
            let mut seen = String::new();
            let null = crate::fixture_hash_kind().null();
            let err = gix_ref::StoreRef::from(&store)
                .pack_refs(
                    &odb,
                    Options::default(),
                    Some(&mut observe(&mut seen, null, Some(veto))),
                )
                .expect_err("the observer refuses");
            assert!(
                err.downcast_any_ref::<gix_ref::transaction::observe::Vetoed>()
                    .is_some(),
                "{veto:?}: the error says why"
            );
            assert_eq!(seen, std::fs::read_to_string(&log)?, "{veto:?}: the same states as Git");
            assert_eq!(layout(store.git_dir())?, before, "{veto:?}: nothing changed");
            assert!(
                !store.git_dir().join("packed-refs.lock").exists() && !store.git_dir().join("packed-refs.new").exists(),
                "{veto:?}: no lock or temporary file is left"
            );
        }
        Ok(())
    }

    #[test]
    fn a_reference_that_the_observer_refuses_to_prune_stays_loose() -> TestResult {
        let (_native, store, odb) = fixture()?;
        let outcome = gix_ref::StoreRef::from(&store).pack_refs(
            &odb,
            Options::default(),
            Some(&mut |event: &Event<'_>| {
                // Only deletions are refused, which is what pruning does, and the transaction that packs is let through.
                let is_prune = event.updates.iter().all(|update| update.new.is_none());
                if is_prune && !event.updates.is_empty() && event.state == State::Prepared {
                    Err(Veto)
                } else {
                    Ok(())
                }
            }),
        )?;
        assert_eq!(outcome.pruned, 0, "every prune is refused, which isn't an error");
        assert!(outcome.skipped > 0);
        assert!(
            store.git_dir().join("refs/heads/main").is_file(),
            "the loose reference is still there, and packed as well"
        );
        Ok(())
    }
}
