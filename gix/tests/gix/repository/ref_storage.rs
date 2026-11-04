//! Repositories whose references are stored in a way that isn't supported must still be opened, as their objects are
//! accessible, but their references must never be read or written as if they were stored in files.
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

#[test]
fn unsupported_storage_is_opened_but_refuses_references() -> Result {
    for (name, backend) in [
        ("v1-reftable", "reftable"),
        ("v1-files-then-reftable", "reftable"),
        ("v1-key-in-uppercase", "reftable"),
        ("v1-files-with-payload", "files://elsewhere"),
        ("v1-files-with-empty-payload", "files://"),
        ("v1-reftable-with-payload", "reftable://elsewhere"),
    ] {
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
        assert_eq!(
            repo.refs.unsupported_backend(),
            Some("reftable"),
            "like in Git the variable takes precedence over the configuration"
        );
        let repo = open_with_environment("v1-without-extension", "files://elsewhere", true)?
            .expect("the environment applies to repositories that don't configure the extension");
        assert_eq!(repo.refs.unsupported_backend(), Some("files://elsewhere"));
        Ok(())
    }

    #[test]
    #[serial]
    fn the_variable_doesnt_make_unsupported_storage_appear_supported() -> Result {
        let repo = open_with_environment("v1-reftable", "files", true)?.expect("object access keeps working");
        assert_eq!(
            repo.refs.unsupported_backend(),
            Some("reftable"),
            "following the override would read references where they aren't"
        );
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
        let repo = gix::open_opts(&work_dir, gix::open::Options::isolated())?;
        let head_id = gix_testtools::git(&work_dir, "rev-parse HEAD")?;
        let head_id = gix::ObjectId::from_hex(head_id.trim().as_bytes())?;
        Ok(Some((dir, repo, head_id)))
    }

    #[test]
    fn references_that_exist_are_not_reported_as_missing_and_writes_leave_no_files_git_cannot_see() -> Result {
        let Some((dir, repo, head_id)) = writable_repo()? else {
            return Ok(());
        };
        let work_dir = dir.path().join("reftable-clone");
        let before = paths_below(repo.git_dir())?;
        let listed = gix_testtools::git(&work_dir, "for-each-ref")?;
        assert!(listed.contains("refs/heads/main"), "Git sees the reference: {listed}");

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
    #[cfg(feature = "worktree-mutation")]
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
