//! Repositories whose references are stored in a way that isn't supported must still be opened, as their objects are
//! accessible, but their references must never be read or written as if they were stored in files.
//!
//! Reftables are supported with the `reftable` feature, and unsupported otherwise.
use gix_testtools::{Result, scripted_fixture_read_only};

/// Open the repository `name`, or return `None` if it can't exist, which is the case for version 0 repositories
/// when objects aren't SHA-1.
fn open(name: &str) -> Result<Option<gix::Result<gix::Repository>>> {
    let root = scripted_fixture_read_only("make_ref_storage_repos.sh")?;
    let path = root.join(name);
    Ok(path
        .exists()
        .then(|| gix::open_opts(path, gix::open::Options::isolated())))
}

/// Version 0 repositories can't exist unless objects are SHA-1, and nothing else may be missing, or a mistyped
/// name would turn into a test that checks nothing.
fn assert_may_be_missing(name: &str) {
    assert!(
        name.starts_with("v0") && gix_testtools::object_hash() != gix::hash::Kind::Sha1,
        "{name}: only version 0 variants of other hashes may be missing"
    );
}

#[test]
fn files_storage_is_used_if_it_is_selected_or_not_selected_at_all() -> Result {
    for name in ["v0", "v1-without-extension", "v1-files", "v1-reftable-then-files"] {
        let Some(repo) = open(name)? else {
            assert_may_be_missing(name);
            continue;
        };
        let repo = repo.expect("the repository is opened");
        assert_eq!(
            repo.refs.unsupported_backend(),
            None,
            "{name}: references are stored in files"
        );
        repo.head_id().expect("references can be read");
    }
    Ok(())
}

/// The variants that select reftables without a location, which are only unsupported without the `reftable` feature.
const REFTABLE_VARIANTS: [&str; 3] = ["v1-reftable", "v1-files-then-reftable", "v1-key-in-uppercase"];

#[test]
#[cfg(feature = "reftable")]
fn reftable_storage_is_used_if_it_is_selected_and_supported() -> Result {
    for name in REFTABLE_VARIANTS {
        let Some(repo) = open(name)? else {
            assert_may_be_missing(name);
            continue;
        };
        let repo = repo.expect("the repository is opened");
        assert_eq!(
            repo.refs.as_ref().kind(),
            gix::refs::view::Kind::Reftable,
            "{name}: references are looked for in reftables"
        );
        assert_eq!(repo.refs.unsupported_backend(), None, "{name}");
        assert!(
            repo.has_object(repo.object_hash().empty_tree()),
            "{name}: objects are accessible"
        );
    }
    let repo = open("v1-reftable-then-files")?.map(|repo| repo.expect("it opens"));
    if let Some(repo) = repo {
        assert_eq!(
            repo.refs.as_ref().kind(),
            gix::refs::view::Kind::Files,
            "the last value wins, like in Git"
        );
    }
    Ok(())
}

#[test]
#[cfg(feature = "reftable")]
fn how_reftables_are_written_follows_the_configuration() -> Result {
    use std::time::Duration;

    let open_with = |name: &str, overrides: &[&str], lenient: bool| -> Result<gix::Repository> {
        let root = scripted_fixture_read_only("make_ref_storage_repos.sh")?;
        Ok(gix::open_opts(
            root.join(name),
            gix::open::Options::isolated()
                .strict_config(!lenient)
                .config_overrides(overrides.iter().copied()),
        )?)
    };
    if open("v1-reftable")?.is_none() {
        return Ok(());
    }

    let defaults = open_with("v1-reftable", &[], false)?;
    let defaults = *defaults.refs.as_reftable().expect("reftable").stack_options()?;
    assert_eq!(defaults.lock_timeout, Some(Duration::from_millis(100)), "Git's default");
    assert_eq!(
        (
            defaults.write.block_size,
            defaults.write.restart_interval,
            defaults.shared_repository_permissions
        ),
        (0, 0, 0),
        "everything else is left to the defaults of the writer"
    );

    let repo = open_with(
        "v1-reftable",
        &[
            "reftable.blockSize=8192",
            "reftable.restartInterval=4",
            "reftable.geometricFactor=3",
            "reftable.lockTimeout=-1",
            "core.fsync=reference",
            "core.sharedRepository=0660",
        ],
        false,
    )?;
    let options = *repo.refs.as_reftable().expect("reftable").stack_options()?;
    assert_eq!(options.write.block_size, 8192);
    assert_eq!(options.write.restart_interval, 4);
    assert_eq!(options.geometric_factor, 3);
    assert_eq!(options.lock_timeout, None, "-1 waits as long as it takes");
    assert_ne!(options.fsync, defaults.fsync, "core.fsync reaches the stacks");
    assert_ne!(
        options.shared_repository_permissions, 0,
        "core.sharedRepository reaches the stacks"
    );

    // Like Git, which only reads these settings when it writes, invalid ones don't keep references from being read.
    for invalid in [
        "reftable.lockTimeout=-2",
        "reftable.blockSize=99999999",
        "reftable.restartInterval=65536",
    ] {
        let repo = open_with("v1-reftable", &[invalid], false)?;
        assert!(
            repo.try_find_reference("HEAD")?.is_none(),
            "{invalid}: the fixture's stack is empty, which is read without complaints"
        );
        repo.references()?.all()?.collect::<std::result::Result<Vec<_>, _>>()?;
        let err = repo
            .refs
            .as_reftable()
            .expect("reftable")
            .stack_options()
            .expect_err("writing would need the invalid value");
        let key = invalid.split('=').next().expect("a key");
        assert!(
            format!("{err:?}").to_lowercase().contains(&key.to_lowercase()),
            "{invalid}: the error names the key: {err:?}"
        );
    }
    let repo = open_with(
        "v1-reftable",
        &["reftable.lockTimeout=-2", "reftable.blockSize=99999999"],
        true,
    )?;
    let lenient = repo.refs.as_reftable().expect("reftable").stack_options()?;
    assert_eq!(
        (lenient.lock_timeout, lenient.write.block_size),
        (defaults.lock_timeout, 0),
        "lenient configuration ignores invalid values"
    );

    let files = open_with(
        "v1-files",
        &["reftable.blockSize=99999999", "reftable.lockTimeout=-5"],
        false,
    )?;
    assert_eq!(
        files.refs.as_ref().kind(),
        gix::refs::view::Kind::Files,
        "settings of a store that isn't used don't matter, as in Git"
    );
    Ok(())
}

#[test]
fn unsupported_storage_is_opened_but_refuses_references() -> Result {
    let mut unsupported = vec![
        ("v1-files-with-payload", "files://elsewhere"),
        ("v1-files-with-empty-payload", "files://"),
        ("v1-reftable-with-payload", "reftable://elsewhere"),
    ];
    if !cfg!(feature = "reftable") {
        unsupported.extend(REFTABLE_VARIANTS.map(|name| (name, "reftable")));
    }
    for (name, backend) in unsupported {
        let Some(repo) = open(name)? else {
            assert_may_be_missing(name);
            continue;
        };
        let repo = repo.expect("object access keeps working, which is why the repository is opened");
        assert_eq!(
            repo.refs.unsupported_backend(),
            Some(backend),
            "{name}: the unsupported backend is named"
        );
        let err = repo
            .head_id()
            .expect_err("references must not be read as if they were files");
        assert!(
            err.is_unsupported(),
            "{name}: callers can see that a different strategy is needed"
        );
        let err = repo
            .try_find_reference("refs/heads/main")
            .map(|_| ())
            .expect_err("a reference must not be reported as missing where its storage is unknown");
        assert!(err.is_unsupported(), "{name}");
        assert!(
            repo.has_object(repo.object_hash().empty_tree()),
            "{name}: objects are accessible"
        );
    }
    Ok(())
}

#[test]
fn invalid_selections_fail_to_open_like_in_git() -> Result {
    for name in [
        "v0-files",
        "v0-reftable",
        "v1-value-in-uppercase",
        "v1-unknown",
        "v1-empty",
        "v1-unknown-with-payload",
        "v1-without-value",
    ] {
        let Some(res) = open(name)? else {
            assert_may_be_missing(name);
            continue;
        };
        let err = res
            .err()
            .unwrap_or_else(|| panic!("{name}: Git refuses to use this repository"));
        assert!(
            !err.is_unsupported(),
            "{name}: the selection is invalid rather than unsupported: {err:?}"
        );
        assert!(
            format!("{err:?}").contains("refStorage"),
            "{name}: the extension is named in the error: {err:?}"
        );
    }
    Ok(())
}

mod environment {
    use gix_testtools::{Result, scripted_fixture_read_only};
    use serial_test::serial;

    fn open_with_environment(
        name: &str,
        backend: &str,
        allow_environment: bool,
    ) -> Result<gix::Result<gix::Repository>> {
        let root = scripted_fixture_read_only("make_ref_storage_repos.sh")?;
        let _env = gix_testtools::isolate_git_environment()?.set("GIT_REFERENCE_BACKEND", backend);
        let mut permissions = gix::open::Permissions::isolated();
        if allow_environment {
            permissions.env = gix::open::permissions::Environment::all();
        }
        Ok(gix::open_opts(
            root.join(name),
            gix::open::Options::isolated().permissions(permissions),
        ))
    }

    #[test]
    #[serial]
    fn the_variable_overrides_the_configuration_towards_unsupported_storage() -> Result {
        let repo = open_with_environment("v1-files", "reftable", true)?.expect("object access keeps working");
        if cfg!(feature = "reftable") {
            assert_eq!(
                repo.refs.as_ref().kind(),
                gix::refs::view::Kind::Reftable,
                "like in Git the variable takes precedence over the configuration"
            );
        } else {
            assert_eq!(
                repo.refs.unsupported_backend(),
                Some("reftable"),
                "like in Git the variable takes precedence over the configuration"
            );
        }
        let repo =
            open_with_environment("v1-files", "reftable://elsewhere", true)?.expect("object access keeps working");
        assert_eq!(
            repo.refs.unsupported_backend(),
            Some("reftable://elsewhere"),
            "the location can't be used, with or without support for reftables"
        );
        let repo = open_with_environment("v1-without-extension", "files://elsewhere", true)?
            .expect("the environment applies to repositories that don't configure the extension");
        assert_eq!(repo.refs.unsupported_backend(), Some("files://elsewhere"));
        Ok(())
    }

    #[test]
    #[serial]
    fn the_variable_selects_files_even_if_the_configuration_says_otherwise_like_in_git() -> Result {
        // Git then sees what is in files, which for a repository with reftables is the placeholder that makes no references.
        let repo = open_with_environment("v1-reftable", "files", true)?.expect("object access keeps working");
        assert_eq!(
            repo.refs.as_ref().kind(),
            gix::refs::view::Kind::Files,
            "the variable takes precedence over the configuration, which is what Git does"
        );
        assert_eq!(repo.refs.unsupported_backend(), None);
        let repo = open_with_environment("v1-without-extension", "files", true)?.expect("files storage is supported");
        assert_eq!(repo.refs.unsupported_backend(), None);
        Ok(())
    }

    #[test]
    #[serial]
    fn an_unknown_value_fails_to_open_like_in_git() -> Result {
        let err =
            open_with_environment("v1-without-extension", "bogus", true)?.expect_err("Git dies for unknown formats");
        assert!(
            !err.is_unsupported(),
            "an invalid value is not the same as an unsupported one: {err:?}"
        );
        Ok(())
    }

    #[test]
    #[serial]
    fn the_variable_is_ignored_if_environment_access_is_denied() -> Result {
        let repo = open_with_environment("v1-without-extension", "reftable", false)?.expect("it opens");
        assert_eq!(repo.refs.unsupported_backend(), None);
        let repo = open_with_environment("v1-without-extension", "bogus", false)?
            .expect("an invalid value isn't even looked at");
        assert_eq!(repo.refs.unsupported_backend(), None);
        Ok(())
    }
}

mod reftable {
    use gix_testtools::{Result, scripted_fixture_writable_with_git_version};

    fn paths_below(dir: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
        fn recurse(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                out.push(path.clone());
                if path.is_dir() {
                    recurse(&path, out)?;
                }
            }
            Ok(())
        }
        let mut out = Vec::new();
        recurse(dir, &mut out)?;
        out.sort();
        Ok(out)
    }

    fn writable_repo() -> Result<Option<(gix_testtools::tempfile::TempDir, gix::Repository, gix::ObjectId)>> {
        let Some(dir) =
            scripted_fixture_writable_with_git_version("make_reftable_repo.sh", |version| version >= (2, 44, 0))?
        else {
            return Ok(None);
        };
        let work_dir = dir.path().join("reftable-clone");
        let repo = gix::open_opts(
            &work_dir,
            gix::open::Options::isolated()
                .config_overrides(["committer.name=gitoxide", "committer.email=gitoxide@localhost"]),
        )?;
        let head_id = gix_testtools::git(&work_dir, "rev-parse HEAD")?;
        let head_id = gix::ObjectId::from_hex(head_id.trim().as_bytes())?;
        Ok(Some((dir, repo, head_id)))
    }

    #[test]
    fn references_that_exist_are_not_reported_as_missing_and_writes_leave_no_files_git_cannot_see() -> Result {
        // Without the feature, editing is refused for reftables, with or without reading them.
        let Some((dir, repo, head_id)) = writable_repo()? else {
            return Ok(());
        };
        let work_dir = dir.path().join("reftable-clone");
        let before = paths_below(repo.git_dir())?;
        let listed = gix_testtools::git(&work_dir, "for-each-ref")?;
        assert!(listed.contains("refs/heads/main"), "Git sees the reference: {listed}");

        if cfg!(feature = "reftable") {
            for name in ["refs/heads/main", "refs/remotes/origin/main", "HEAD"] {
                assert!(
                    repo.try_find_reference(name)?.is_some(),
                    "{name}: references that exist are found"
                );
            }
            assert_eq!(repo.head_id()?, head_id, "HEAD resolves like Git says");
            let names = repo
                .references()?
                .all()?
                .map(|r| r.map(|r| r.name().as_bstr().to_string()))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let expected: Vec<_> = listed
                .lines()
                .map(|line| line.rsplit_once('\t').expect("tab").1.to_owned())
                .collect();
            assert_eq!(names, expected, "iteration lists what `git for-each-ref` does");
        } else {
            for name in ["refs/heads/main", "refs/remotes/origin/main", "HEAD"] {
                let err = repo
                    .try_find_reference(name)
                    .map(|_| ())
                    .expect_err("existing references must not be reported as missing");
                assert!(err.is_unsupported(), "{name}");
            }
            assert!(repo.head_id().expect_err("HEAD is not readable").is_unsupported());
            assert!(
                repo.references().err().expect("iterating is refused").is_unsupported(),
                "also iteration is refused rather than yielding something"
            );
        }
        if cfg!(feature = "reftable") {
            for name in ["refs/tags/new", "refs/heads/new", "refs/notes/new"] {
                repo.reference(
                    name,
                    head_id,
                    gix::refs::transaction::PreviousValue::MustNotExist,
                    "message",
                )?;
            }
            repo.tag_reference("tag", head_id, gix::refs::transaction::PreviousValue::MustNotExist)?;
            let listed = gix_testtools::git(&work_dir, "for-each-ref --format=%(refname)")?;
            for name in ["refs/tags/new", "refs/heads/new", "refs/notes/new", "refs/tags/tag"] {
                assert!(
                    listed.lines().any(|line| line == name),
                    "Git sees the new reference {name}: {listed}"
                );
            }
            let changed: Vec<_> = paths_below(repo.git_dir())?
                .into_iter()
                .filter(|path| !before.contains(path))
                .collect();
            assert!(
                changed
                    .iter()
                    .all(|path| path.starts_with(repo.git_dir().join("reftable"))),
                "only the stack changed, there is no loose reference, reflog or lock file: {changed:?}"
            );
            assert_eq!(
                gix_testtools::git(&work_dir, "reflog show --format=%gs refs/heads/new")?.trim(),
                "message",
                "the reflog is written, with the configured committer"
            );
            gix_testtools::git(&work_dir, "refs verify")?;
            return Ok(());
        }
        for name in ["refs/tags/new", "refs/heads/new", "refs/notes/new"] {
            let err = repo
                .reference(
                    name,
                    head_id,
                    gix::refs::transaction::PreviousValue::MustNotExist,
                    "message",
                )
                .map(|_| ())
                .expect_err("writes are refused");
            assert!(err.is_unsupported(), "{name}");
        }
        assert!(
            repo.tag_reference("tag", head_id, gix::refs::transaction::PreviousValue::MustNotExist)
                .map(|_| ())
                .expect_err("tag creation is refused")
                .is_unsupported()
        );

        assert_eq!(
            paths_below(repo.git_dir())?,
            before,
            "nothing is written, no loose reference, reflog or lock file"
        );
        assert_eq!(
            gix_testtools::git(&work_dir, "for-each-ref")?,
            listed,
            "Git continues to see exactly what it saw"
        );
        Ok(())
    }

    #[test]
    #[cfg(all(feature = "worktree-mutation", feature = "reftable"))]
    fn adding_a_worktree_stores_its_head_in_a_stack_of_its_own_like_git() -> Result {
        let Some((dir, repo, head_id)) = writable_repo()? else {
            return Ok(());
        };
        for (name, head, expected_branch) in [
            ("detached", gix::worktree::add::Head::Detached(head_id), None),
            (
                "attached",
                gix::worktree::add::Head::Attached("refs/heads/main".try_into()?),
                Some("refs/heads/main"),
            ),
        ] {
            let destination = dir.path().join(name);
            // The branch can only be checked out once.
            if expected_branch.is_some() {
                gix_testtools::git(dir.path().join("reftable-clone"), "checkout --detach")?;
            }
            repo.prepare_add_worktree(&destination, head, &std::sync::atomic::AtomicBool::default())?
                .persist()?;
            assert_eq!(
                gix_testtools::git(&destination, "rev-parse HEAD")?.trim(),
                head_id.to_string(),
                "{name}: Git reads the HEAD that was written"
            );
            let branch = gix_testtools::git(&destination, "symbolic-ref -q HEAD").unwrap_or_default();
            assert_eq!(branch.trim(), expected_branch.unwrap_or_default(), "{name}");
            assert_eq!(
                gix_testtools::git(&destination, "reflog show --format=%H HEAD")?
                    .lines()
                    .count(),
                1,
                "{name}: HEAD has the entry that initializes it"
            );
            gix_testtools::git(dir.path().join("reftable-clone"), "refs verify")?;
            gix_testtools::git(&destination, "refs verify")?;
        }
        let listed = gix_testtools::git(dir.path().join("reftable-clone"), "worktree list --porcelain")?;
        assert_eq!(
            listed.matches("worktree ").count(),
            3,
            "the repository and both new worktrees: {listed}"
        );
        let reopened = gix::open_opts(dir.path().join("attached"), gix::open::Options::isolated())?;
        assert_eq!(
            reopened.head_id()?,
            head_id,
            "the new worktree is read back through its own stack"
        );
        assert_eq!(reopened.head_name()?.expect("attached").as_bstr(), "refs/heads/main");
        Ok(())
    }

    #[test]
    #[cfg(all(feature = "worktree-mutation", not(feature = "reftable")))]
    fn adding_a_worktree_is_refused_before_anything_is_created() -> Result {
        let Some((dir, repo, head_id)) = writable_repo()? else {
            return Ok(());
        };
        let before = paths_below(repo.git_dir())?;
        let config_before = std::fs::read(repo.git_dir().join("config"))?;
        let destination = dir.path().join("linked");

        let err = repo
            .prepare_add_worktree(
                &destination,
                gix::worktree::add::Head::Detached(head_id),
                &std::sync::atomic::AtomicBool::default(),
            )
            .expect_err("the HEAD of the new worktree can't be created");
        assert!(err.is_unsupported());

        assert!(!destination.exists(), "the destination isn't created");
        assert!(
            !repo.git_dir().join("worktrees").exists(),
            "not even the directory for registrations is created"
        );
        assert_eq!(paths_below(repo.git_dir())?, before);
        assert_eq!(
            std::fs::read(repo.git_dir().join("config"))?,
            config_before,
            "the shared configuration isn't touched, which would otherwise upgrade it for relative paths"
        );
        Ok(())
    }
}

#[test]
#[cfg(feature = "reftable")]
fn a_linked_worktree_reads_its_own_references_and_the_shared_ones_like_git() -> Result {
    let Some(root) =
        gix_testtools::scripted_fixture_read_only_with_git_version("make_reftable_worktree_repo.sh", |version| {
            version >= (2, 44, 0)
        })?
    else {
        return Ok(());
    };
    let main = gix::open_opts(root.join("repo"), gix::open::Options::isolated())?;
    let wt = gix::open_opts(root.join("wt"), gix::open::Options::isolated())?;
    assert_eq!(wt.refs.as_ref().kind(), gix::refs::view::Kind::Reftable);
    assert_eq!(
        wt.refs.common_dir().map(std::path::Path::canonicalize).transpose()?,
        Some(main.git_dir().canonicalize()?),
        "the linked worktree knows where the shared stack is"
    );

    let git_rev_parse = |dir: &str, rev: &str| -> Result<String> {
        Ok(gix_testtools::git(root.join(dir), &format!("rev-parse {rev}"))?
            .trim()
            .to_owned())
    };
    assert_eq!(main.head_id()?.to_string(), git_rev_parse("repo", "HEAD")?);
    assert_eq!(
        wt.head_id()?.to_string(),
        git_rev_parse("wt", "HEAD")?,
        "the worktree has a HEAD of its own"
    );
    assert_ne!(main.head_id()?, wt.head_id()?);
    assert_eq!(wt.head_name()?.expect("on a branch").as_bstr(), "refs/heads/wt-branch");
    assert_eq!(
        wt.find_reference("refs/heads/main")?.id().to_string(),
        git_rev_parse("wt", "refs/heads/main")?,
        "shared references come from the main stack"
    );
    assert!(
        wt.try_find_reference("refs/worktree/private")?.is_some(),
        "references private to a worktree are in its stack"
    );
    assert!(
        main.try_find_reference("refs/worktree/private")?.is_none(),
        "and not visible to the main worktree"
    );
    assert_eq!(
        main.find_reference("worktrees/wt/HEAD")?.peel_to_id()?.to_string(),
        wt.head_id()?.to_string(),
        "the main worktree reaches the one of the other with its name"
    );
    Ok(())
}
