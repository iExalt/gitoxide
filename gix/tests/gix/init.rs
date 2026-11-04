mod bare {
    use gix_testtools::TestResult;
    use gix_testtools::tempfile;

    #[test]
    #[serial_test::serial]
    fn init_into_non_existing_directory_creates_it() -> TestResult {
        let _environment = gix_testtools::isolate_git_environment()?;
        let tmp = tempfile::tempdir()?;
        let git_dir = tmp.path().join("bare.git");
        let repo = gix::init_bare(&git_dir)?;
        assert_eq!(repo.kind(), gix::repository::Kind::Common);
        assert!(
            repo.workdir().is_none(),
            "a worktree isn't present in bare repositories"
        );
        assert_eq!(
            repo.git_dir(),
            git_dir,
            "the repository is placed into the given directory without added sub-directories"
        );
        assert_eq!(gix::open_opts(repo.git_dir(), crate::restricted())?, repo);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn init_into_empty_directory_uses_it_directly() -> TestResult {
        let _environment = gix_testtools::isolate_git_environment()?;
        super::assert_init_into_empty_directory(gix::create::Kind::Bare)
    }

    #[test]
    fn init_into_non_empty_directory_is_not_allowed() -> TestResult {
        let tmp = tempfile::tempdir()?;
        std::fs::write(tmp.path().join("existing.txt"), b"I was here before you")?;

        insta::assert_debug_snapshot!(gix_testtools::redact_debug_snapshot(&(gix::init_bare(tmp.path())
                .expect_err("init into non empty directory is not allowed")), &[(&(tmp.path()).to_string_lossy(), "<destination>")]), "init into non empty directory is not allowed", @r#"Refusing to initialize the non-empty directory as, input="<destination>""#);
        Ok(())
    }
}

mod non_bare {
    use gix_testtools::TestResult;
    use gix_testtools::tempfile;

    #[test]
    fn init_bare_with_custom_branch_name() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let repo: gix::Repository = gix::ThreadSafeRepository::init_opts(
            tmp.path(),
            gix::create::Kind::Bare,
            gix::create::Options::default(),
            gix::open::Options::isolated().config_overrides([
                "user.name=a",
                "user.email=b",
                "init.defaultBranch=special",
            ]),
        )?
        .into();
        assert_eq!(repo.head()?.referent_name().expect("name"), "refs/heads/special");
        Ok(())
    }

    #[test]
    fn init_bare_with_fully_qualified_custom_branch_name_is_not_prefixed_again() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let repo: gix::Repository = gix::ThreadSafeRepository::init_opts(
            tmp.path(),
            gix::create::Kind::Bare,
            gix::create::Options::default(),
            gix::open::Options::isolated().config_overrides([
                "user.name=a",
                "user.email=b",
                "init.defaultBranch=refs/heads/special",
            ]),
        )?
        .into();
        assert_eq!(repo.head()?.referent_name().expect("name"), "refs/heads/special");
        assert_eq!(
            repo.is_pristine(),
            Some(true),
            "the expected default ref uses the de-duplicated fully qualified branch name"
        );
        Ok(())
    }

    #[test]
    fn init_bare_rejects_reserved_branch_name() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let err = gix::ThreadSafeRepository::init_opts(
            tmp.path(),
            gix::create::Kind::Bare,
            gix::create::Options::default(),
            gix::open::Options::isolated().config_overrides(["user.name=a", "user.email=b", "init.defaultBranch=HEAD"]),
        )
        .unwrap_err();
        insta::assert_debug_snapshot!(err, "init bare rejects reserved branch name", @r#"
        Invalid default branch name, input="HEAD"

        Caused by:
            0: Reference name is reserved and cannot be used: "refs/heads/HEAD"
        "#);
        assert!(matches!(
            err.classify().filter(|classification| classification.class() == gix_error::Class::Validation)
                .find_map(|classification| classification.error().downcast_ref::<gix_error::Message>()),
            Some(gix_error::Message { values, .. }) if values.get("input") == Some(&gix_error::MetadataValue::Bytes("HEAD".into()))
        ));
        assert!(matches!(
            err.downcast_any_ref(),
            Some(gix_validate::reference::name::Error::Reserved { name }) if name == "refs/heads/HEAD"
        ));
        Ok(())
    }

    #[test]
    fn init_bare_rejects_reserved_fully_qualified_branch_name() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let err = gix::ThreadSafeRepository::init_opts(
            tmp.path(),
            gix::create::Kind::Bare,
            gix::create::Options::default(),
            gix::open::Options::isolated().config_overrides([
                "user.name=a",
                "user.email=b",
                "init.defaultBranch=refs/heads/HEAD",
            ]),
        )
        .unwrap_err();
        insta::assert_debug_snapshot!(err, "init bare rejects reserved fully qualified branch name", @r#"
        Invalid default branch name, input="refs/heads/HEAD"

        Caused by:
            0: Reference name is reserved and cannot be used: "refs/heads/HEAD"
        "#);
        assert!(matches!(
            err.classify().filter(|classification| classification.class() == gix_error::Class::Validation)
                .find_map(|classification| classification.error().downcast_ref::<gix_error::Message>()),
            Some(gix_error::Message { values, .. }) if values.get("input") == Some(&gix_error::MetadataValue::Bytes("refs/heads/HEAD".into()))
        ));
        assert!(matches!(
            err.downcast_any_ref(),
            Some(gix_validate::reference::name::Error::Reserved { name }) if name == "refs/heads/HEAD"
        ));
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn init_into_empty_directory_creates_a_dot_git_dir() -> TestResult {
        let _environment = gix_testtools::isolate_git_environment()?;
        let tmp = tempfile::tempdir()?;
        let repo = gix::init(tmp.path())?;
        assert_eq!(repo.kind(), gix::repository::Kind::Common);
        assert_eq!(repo.workdir(), Some(tmp.path()), "there is a work tree by default");
        assert_eq!(
            repo.git_dir(),
            tmp.path().join(".git"),
            "there is a work tree by default"
        );
        assert_eq!(gix::open_opts(repo.git_dir(), crate::restricted())?, repo);
        assert_eq!(
            gix::open_opts(repo.workdir().as_ref().expect("non-bare repo"), crate::restricted())?,
            repo
        );
        super::assert_init_into_empty_directory(gix::create::Kind::WithWorktree)
    }

    #[test]
    fn init_into_non_empty_directory_is_allowed_if_option_is_none_or_false() -> TestResult {
        for destination_must_be_empty in [None, Some(false)] {
            let tmp = tempfile::tempdir()?;
            std::fs::write(tmp.path().join("existing.txt"), b"I was here before you")?;
            let repo: gix::Repository = gix::ThreadSafeRepository::init_opts(
                tmp.path(),
                gix::create::Kind::WithWorktree,
                gix::create::Options {
                    destination_must_be_empty,
                    ..Default::default()
                },
                gix::open::Options::isolated(),
            )?
            .into();
            assert_eq!(repo.workdir().expect("present"), tmp.path());
            assert_eq!(
                repo.git_dir(),
                tmp.path().join(".git"),
                "gitdir is inside of the workdir"
            );
        }
        Ok(())
    }

    #[test]
    fn init_into_non_empty_directory_is_not_allowed_if_option_is_true() -> TestResult {
        let tmp = tempfile::tempdir()?;
        std::fs::write(tmp.path().join("existing.txt"), b"I was here before you")?;

        let err = gix::ThreadSafeRepository::init_opts(
            tmp.path(),
            gix::create::Kind::WithWorktree,
            gix::create::Options {
                destination_must_be_empty: Some(true),
                ..Default::default()
            },
            gix::open::Options::isolated(),
        )
        .unwrap_err();
        insta::assert_debug_snapshot!(gix_testtools::redact_debug_snapshot(&(err), &[(&(tmp.path()).to_string_lossy(), "<destination>")]), "init into non empty directory is not allowed if option is true", @r#"Refusing to initialize the non-empty directory as, input="<destination>""#);
        Ok(())
    }
}

#[test]
fn invalid_default_object_format_is_an_error_unless_explicitly_overridden() -> gix_testtools::TestResult {
    for repo_kind in [gix::create::Kind::Bare, gix::create::Kind::WithWorktree] {
        for &requested_object_hash in OBJECT_FORMATS {
            let tmp = gix_testtools::tempfile::tempdir()?;
            let result = gix::ThreadSafeRepository::init_opts(
                tmp.path(),
                repo_kind,
                gix::create::Options {
                    object_hash: requested_object_hash,
                    ..Default::default()
                },
                crate::restricted().config_overrides(["init.defaultObjectFormat=invalid"]),
            );
            if let Some(expected_object_hash) = requested_object_hash {
                assert_eq!(
                    result?.to_thread_local().object_hash(),
                    expected_object_hash,
                    "an explicit hash bypasses an invalid configured default for {repo_kind:?}"
                );
            } else {
                let err = result.expect_err("an unknown default hash must fail initialization");
                assert!(err.is_validation());
                assert_eq!(
                    err.metadata_merged().get("key"),
                    Some(&gix_error::MetadataValue::from("init.defaultObjectFormat")),
                    "the error identifies the invalid configuration key"
                );
                assert!(
                    !tmp.path().join("HEAD").exists() && !tmp.path().join(".git").exists(),
                    "invalid defaults are rejected before creating a repository"
                );
            }
        }
    }
    Ok(())
}

const OBJECT_FORMATS: &[Option<gix_hash::Kind>] = &[
    None,
    #[cfg(feature = "sha1")]
    Some(gix_hash::Kind::Sha1),
    #[cfg(feature = "sha256")]
    Some(gix_hash::Kind::Sha256),
];

fn assert_init_into_empty_directory(repo_kind: gix::create::Kind) -> gix_testtools::TestResult {
    use gix::config::tree::{Core, Extensions, Init, Key};

    #[cfg(feature = "sha1")]
    let build_default_hash = gix_hash::Kind::Sha1;
    #[cfg(not(feature = "sha1"))]
    let build_default_hash = gix_hash::Kind::Sha256;
    for &default_object_format in OBJECT_FORMATS {
        for &requested_object_hash in OBJECT_FORMATS {
            let case = format!(
                "repo_kind={repo_kind:?}, requested_object_hash={requested_object_hash:?}, default_object_format={default_object_format:?}"
            );
            let tmp = gix_testtools::tempfile::tempdir()?;
            let overrides = default_object_format
                .map(|format| Init::DEFAULT_OBJECT_FORMAT.validated_assignment_fmt(&format))
                .transpose()?;
            let repo = gix::ThreadSafeRepository::init_opts(
                tmp.path(),
                repo_kind,
                gix::create::Options {
                    object_hash: requested_object_hash,
                    ..Default::default()
                },
                crate::restricted().config_overrides(overrides),
            )?
            .to_thread_local();
            assert_eq!(repo.kind(), gix::repository::Kind::Common, "{case}");
            match repo_kind {
                gix::create::Kind::Bare => {
                    assert!(repo.workdir().is_none(), "bare repositories have no worktree: {case}");
                    assert_eq!(
                        repo.git_dir(),
                        tmp.path(),
                        "bare repositories use the directory itself: {case}"
                    );
                }
                gix::create::Kind::WithWorktree => {
                    assert_eq!(
                        repo.workdir(),
                        Some(tmp.path()),
                        "the worktree uses the destination: {case}"
                    );
                    assert_eq!(
                        repo.git_dir(),
                        tmp.path().join(".git"),
                        "the git directory is inside the worktree: {case}"
                    );
                }
            }
            let expected_object_hash = requested_object_hash
                .or(default_object_format)
                .unwrap_or(build_default_hash);
            assert_eq!(
                repo.object_hash(),
                expected_object_hash,
                "explicit request, then configured default, then build default: {case}"
            );
            let (expected_repository_format_version, expected_object_format): (i64, Option<&str>) =
                match expected_object_hash {
                    #[cfg(feature = "sha1")]
                    gix_hash::Kind::Sha1 => (0, None),
                    #[cfg(feature = "sha256")]
                    gix_hash::Kind::Sha256 => (1, Some("sha256")),
                    _ => unreachable!("the cases only use enabled hashes"),
                };
            let config =
                gix::config::File::from_path_no_includes(repo.git_dir().join("config"), gix::config::Source::Local)?;
            assert_eq!(
                config.integer(Core::REPOSITORY_FORMAT_VERSION)?,
                Some(expected_repository_format_version),
                "the on-disk repository version matches its object format: {case}"
            );
            assert_eq!(
                config.string(Extensions::OBJECT_FORMAT),
                expected_object_format.map(Into::into),
                "only SHA-256 needs an object-format extension: {case}"
            );
            let reopened = gix::open_opts(repo.git_dir(), crate::restricted())?;
            assert_eq!(
                reopened.object_hash(),
                expected_object_hash,
                "the object hash survives reopening: {case}"
            );
            assert_eq!(reopened, repo, "the repository paths survive reopening: {case}");
        }
    }
    Ok(())
}

/// Repositories that store references as `reftable` are created on request, with Git's precedence of what asks for it, if
/// the `reftable` feature is enabled, and refused before anything is created otherwise.
mod ref_format {
    use gix_testtools::{Result, tempfile};
    use serial_test::serial;

    /// Initialize a repository in a not yet existing directory with `overrides` and, if `environment` is set, with
    /// these variables, which are allowed to be read.
    fn init(
        overrides: &[&str],
        environment: &[(&str, &str)],
    ) -> Result<(
        tempfile::TempDir,
        std::path::PathBuf,
        gix::Result<gix::ThreadSafeRepository>,
    )> {
        let tmp = tempfile::tempdir()?;
        let destination = tmp.path().join("new");
        let mut env = gix_testtools::isolate_git_environment()?;
        for &(name, value) in environment {
            env = env.set(name, value);
        }
        let mut permissions = gix::open::Permissions::isolated();
        if !environment.is_empty() {
            permissions.env = gix::open::permissions::Environment::all();
            permissions.config.user = true;
        }
        let open_options = gix::open::Options::isolated()
            .permissions(permissions)
            .config_overrides(
                ["user.name=a", "user.email=b"]
                    .into_iter()
                    .chain(overrides.iter().copied()),
            );
        let res = gix::ThreadSafeRepository::init_opts(
            &destination,
            gix::create::Kind::WithWorktree,
            gix::create::Options {
                object_hash: Some(gix_testtools::object_hash()),
                ..Default::default()
            },
            open_options,
        );
        drop(env);
        Ok((tmp, destination, res))
    }

    /// Configuration that doesn't come from the API, so that it is as lenient as the one of a configuration file. It is
    /// the global configuration, which has to be kept alive as long as it is used.
    fn configured(key_values: &[(&str, &str)]) -> (tempfile::TempDir, Vec<(String, String)>) {
        let tmp = tempfile::tempdir().expect("a temporary directory can be created");
        use std::fmt::Write;
        let mut contents = String::new();
        for (key, value) in key_values {
            let (section, name) = key.split_once('.').expect("keys have a section");
            writeln!(contents, "[{section}]\n\t{name} = {value}").expect("writing to a string works");
        }
        let path = tmp.path().join("global.config");
        std::fs::write(&path, contents).expect("the file can be written");
        (tmp, vec![("GIT_CONFIG_GLOBAL".to_owned(), path.display().to_string())])
    }

    fn as_refs(env: &[(String, String)]) -> Vec<(&str, &str)> {
        env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
    }

    #[test]
    #[serial]
    fn files_is_created_unless_something_else_is_requested() -> Result {
        let (_bogus_dir, bogus) = configured(&[("init.defaultRefFormat", "bogus")]);
        let (_wrong_case_dir, wrong_case) = configured(&[("init.defaultRefFormat", "Files")]);
        for (overrides, environment) in [
            (&[][..], &[][..]),
            (&["init.defaultRefFormat=files"][..], &[][..]),
            (&["feature.experimental=false"][..], &[][..]),
            (
                &["feature.experimental=true", "init.defaultRefFormat=files"][..],
                &[][..],
            ),
            (
                &["init.defaultRefFormat=reftable"][..],
                &[("GIT_REFERENCE_BACKEND", "files")][..],
            ),
            (
                &["init.defaultRefFormat=files"][..],
                &[("GIT_DEFAULT_REF_FORMAT", "reftable")][..],
            ),
            (&[][..], &as_refs(&bogus)[..]),
            (&[][..], &as_refs(&wrong_case)[..]),
        ] {
            let (_tmp, destination, repo) = init(overrides, environment)?;
            let repo = repo.unwrap_or_else(|err| panic!("{overrides:?} {environment:?}: {err:?}"));
            assert_eq!(repo.refs.unsupported_backend(), None);
            assert!(
                destination.join(".git/refs/heads").is_dir(),
                "{overrides:?} {environment:?}"
            );
        }
        Ok(())
    }

    #[test]
    #[serial]
    fn formats_at_other_locations_are_refused_before_anything_is_created() -> Result {
        let (_tmp, destination, repo) = init(&[], &[("GIT_REFERENCE_BACKEND", "files://elsewhere")])?;
        let err = repo
            .map(|_| ())
            .expect_err("a format that isn't supported must not turn into files");
        assert!(
            err.is_unsupported(),
            "callers can tell that a different strategy is needed: {err:?}"
        );
        assert!(!destination.exists(), "nothing is created for a refused request");
        Ok(())
    }

    #[test]
    #[serial]
    #[cfg(not(feature = "reftable"))]
    fn reftable_is_refused_before_anything_is_created_without_the_feature() -> Result {
        let (_experimental_dir, experimental) = configured(&[("feature.experimental", "true")]);
        for (overrides, environment) in [
            (&["init.defaultRefFormat=reftable"][..], &[][..]),
            (&["feature.experimental=true"][..], &[][..]),
            (&["feature.experimental"][..], &[][..]),
            (&[][..], &as_refs(&experimental)[..]),
            (
                &["init.defaultRefFormat=files"][..],
                &[("GIT_REFERENCE_BACKEND", "reftable")][..],
            ),
            (&[][..], &[("GIT_DEFAULT_REF_FORMAT", "reftable")][..]),
        ] {
            let (_tmp, destination, repo) = init(overrides, environment)?;
            let err = repo
                .map(|_| ())
                .expect_err("a format that isn't supported must not turn into files");
            assert!(
                err.is_unsupported(),
                "{overrides:?} {environment:?}: callers can tell that a different strategy is needed: {err:?}"
            );
            assert!(
                !destination.exists(),
                "{overrides:?} {environment:?}: nothing is created for a refused request"
            );
        }
        Ok(())
    }

    #[test]
    #[serial]
    fn invalid_values_are_errors_where_git_dies_and_ignored_where_it_warns() -> Result {
        for (overrides, environment) in [
            // A format that is passed explicitly, like `--ref-format`, and one of the environment, are strict.
            (&["init.defaultRefFormat=bogus"][..], &[][..]),
            (&["init.defaultRefFormat=Files"][..], &[][..]),
            // A value that is missing is fatal whatever its source.
            (&["init.defaultRefFormat"][..], &[][..]),
            (&[][..], &[("GIT_DEFAULT_REF_FORMAT", "bogus")][..]),
            (&[][..], &[("GIT_REFERENCE_BACKEND", "bogus")][..]),
        ] {
            let (_tmp, destination, repo) = init(overrides, environment)?;
            let err = repo.map(|_| ()).expect_err("Git refuses these");
            assert!(
                err.is_validation(),
                "{overrides:?} {environment:?}: the value is invalid rather than just unsupported: {err:?}"
            );
            assert!(!destination.exists(), "{overrides:?} {environment:?}");
        }
        Ok(())
    }

    #[cfg(feature = "reftable")]
    mod reftable {
        use gix_testtools::Result;
        use serial_test::serial;

        use super::{as_refs, configured, init};

        #[test]
        #[serial]
        fn it_is_created_by_each_request_that_wins_with_git_precedence() -> Result {
            let (_a, experimental_unknown) =
                configured(&[("init.defaultRefFormat", "bogus"), ("feature.experimental", "true")]);
            let (_b, configured_reftable) = configured(&[("init.defaultRefFormat", "reftable")]);
            let (_c, configured_experimental) = configured(&[("feature.experimental", "true")]);
            let (_d, files_beats_experimental) =
                configured(&[("feature.experimental", "true"), ("init.defaultRefFormat", "files")]);
            for (overrides, environment, reftable) in [
                (&["init.defaultRefFormat=reftable"][..], &[][..], true),
                (&["feature.experimental=true"][..], &[][..], true),
                (&["feature.experimental"][..], &[][..], true),
                (&[][..], &as_refs(&configured_reftable)[..], true),
                (&[][..], &as_refs(&configured_experimental)[..], true),
                (&[][..], &as_refs(&experimental_unknown)[..], true),
                (&[][..], &as_refs(&files_beats_experimental)[..], false),
                (&[][..], &[("GIT_DEFAULT_REF_FORMAT", "reftable")][..], true),
                (&[][..], &[("GIT_REFERENCE_BACKEND", "reftable")][..], true),
                (
                    &["init.defaultRefFormat=files"][..],
                    &[("GIT_REFERENCE_BACKEND", "reftable")][..],
                    true,
                ),
                (
                    &["init.defaultRefFormat=reftable"][..],
                    &[("GIT_DEFAULT_REF_FORMAT", "files")][..],
                    true,
                ),
            ] {
                let (_tmp, destination, repo) = init(overrides, environment)?;
                let repo = repo.unwrap_or_else(|err| panic!("{overrides:?} {environment:?}: {err:?}"));
                assert_eq!(
                    repo.refs.as_reftable().is_some(),
                    reftable,
                    "{overrides:?} {environment:?}"
                );
                if reftable {
                    assert_eq!(
                        gix_testtools::git(&destination, "rev-parse --show-ref-format")?.trim(),
                        "reftable",
                        "{overrides:?} {environment:?}: Git agrees on the format"
                    );
                    assert_eq!(
                        gix_testtools::git(&destination, "symbolic-ref HEAD")?.trim(),
                        "refs/heads/main",
                        "{overrides:?} {environment:?}: HEAD is stored in the stack that was created"
                    );
                    gix_testtools::git(&destination, "refs verify")?;
                }
            }
            Ok(())
        }

        #[test]
        #[serial]
        fn the_new_repository_looks_like_the_one_git_creates() -> Result {
            let _environment = gix_testtools::isolate_git_environment()?;
            let hash = gix_testtools::object_hash();
            let tmp = gix_testtools::tempfile::tempdir()?;
            let git_root = tmp.path().join("git");
            let ours_root = tmp.path().join("ours");
            std::fs::create_dir(&git_root)?;
            gix_testtools::git(
                &git_root,
                &format!("init -q --ref-format=reftable --object-format={hash} -b trunk ."),
            )?;
            let repo = gix::ThreadSafeRepository::init_opts(
                &ours_root,
                gix::create::Kind::WithWorktree,
                gix::create::Options {
                    object_hash: Some(hash),
                    ..Default::default()
                },
                gix::open::Options::isolated()
                    .config_overrides(["init.defaultRefFormat=reftable", "init.defaultBranch=trunk"]),
            )?;
            assert_eq!(repo.objects.object_hash(), hash);

            let (git_dir, ours_dir) = (git_root.join(".git"), ours_root.join(".git"));
            assert_eq!(
                listing(&git_dir)?,
                listing(&ours_dir)?,
                "the same files and directories with the same modes and the contents that make Git find the repository"
            );
            assert_eq!(
                std::fs::read_to_string(ours_dir.join("refs/heads"))?,
                "this repository uses the reftable format\n",
                "the stub is what Git leaves, and only a file so that tools can't mistake it for a directory of references"
            );
            assert_eq!(
                std::fs::read_to_string(ours_dir.join("HEAD"))?,
                "ref: refs/heads/.invalid\n",
                "HEAD can't name a branch while the stack holds the references"
            );
            for key in [
                "core.repositoryformatversion",
                "core.bare",
                "core.logallrefupdates",
                "extensions.refstorage",
                "extensions.objectformat",
            ] {
                assert_eq!(
                    gix_testtools::git(&ours_root, &format!("config --local --get {key}")).ok(),
                    gix_testtools::git(&git_root, &format!("config --local --get {key}")).ok(),
                    "{key}"
                );
            }
            assert_eq!(
                gix_testtools::git(&ours_root, "symbolic-ref HEAD")?,
                gix_testtools::git(&git_root, "symbolic-ref HEAD")?,
                "Git reads the HEAD that is in the table"
            );
            assert_eq!(
                gix_testtools::git(&ours_root, "for-each-ref")?,
                gix_testtools::git(&git_root, "for-each-ref")?
            );
            gix_testtools::git(&ours_root, "refs verify")?;
            gix_testtools::git(&ours_root, "fsck")?;
            Ok(())
        }

        /// Everything of a repository that Git's templates don't decide, as `(path, what it is, mode)`, with the file names of
        /// tables normalized as they contain a hash of the table's content, and the contents of the files that make Git find
        /// the repository.
        fn listing(git_dir: &std::path::Path) -> Result<Vec<String>> {
            use std::os::unix::fs::PermissionsExt;
            fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) -> Result {
                for entry in std::fs::read_dir(dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    let relative = path.strip_prefix(root)?.to_string_lossy().into_owned();
                    if ["hooks", "info", "description"].contains(&relative.as_str()) {
                        continue;
                    }
                    let metadata = entry.metadata()?;
                    let mode = metadata.permissions().mode() & 0o777;
                    if metadata.is_dir() {
                        out.push(format!("{relative}/ {mode:o}"));
                        walk(root, &path, out)?;
                    } else {
                        // The names of tables have the range of update indices, which Git's are the same as ours, and a hash
                        // of the content, which is of no concern.
                        let relative = if relative.starts_with("reftable/0x") {
                            let (range, _) = relative.rsplit_once('-').expect("table names have a hash");
                            format!("{range}-<hash>.ref")
                        } else {
                            relative
                        };
                        let contents = if ["HEAD", "refs/heads"].contains(&relative.as_str()) {
                            format!(" {:?}", std::fs::read_to_string(&path)?)
                        } else {
                            String::new()
                        };
                        out.push(format!("{relative} {mode:o}{contents}"));
                    }
                }
                Ok(())
            }
            let mut out = Vec::new();
            walk(git_dir, git_dir, &mut out)?;
            out.sort();
            Ok(out)
        }
    }
}

mod init_or_open {
    use gix_testtools::{TestResult, tempfile};

    #[test]
    #[serial_test::serial]
    fn a_missing_or_empty_directory_gets_a_repository_and_an_existing_one_is_left_alone() -> TestResult {
        let _environment = gix_testtools::isolate_git_environment()?;
        let tmp = tempfile::tempdir()?;
        let directory = tmp.path().join("new");

        let created = gix::init_or_open(&directory)?;
        assert_eq!(
            created.workdir(),
            Some(directory.as_path()),
            "a missing directory is created"
        );
        assert!(created.head()?.is_unborn(), "the new repository is empty");

        gix_testtools::git(&directory, "commit --allow-empty -m first")?;
        gix_testtools::git(&directory, "config custom.setting kept")?;
        let config_before = std::fs::read(directory.join(".git/config"))?;
        let head_before = std::fs::read(directory.join(".git/HEAD"))?;

        let reopened = gix::init_or_open(&directory)?;
        assert_eq!(
            reopened.head_id()?.to_string().len(),
            reopened.object_hash().len_in_hex()
        );
        assert_eq!(
            std::fs::read(directory.join(".git/config"))?,
            config_before,
            "the configuration is not rewritten"
        );
        assert_eq!(
            std::fs::read(directory.join(".git/HEAD"))?,
            head_before,
            "HEAD stays where it was, unlike what initializing again would risk"
        );

        let empty = tmp.path().join("empty");
        std::fs::create_dir(&empty)?;
        gix::init_or_open(&empty)?;
        assert!(empty.join(".git").is_dir(), "an empty directory is initialized");
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn a_directory_inside_a_repository_gets_its_own_and_a_broken_repository_is_an_error() -> TestResult {
        let _environment = gix_testtools::isolate_git_environment()?;
        let tmp = tempfile::tempdir()?;
        let outer = gix::init_or_open(tmp.path().join("outer"))?;
        let inner_dir = outer.workdir().expect("non-bare").join("inner");
        std::fs::create_dir(&inner_dir)?;
        let inner = gix::init_or_open(&inner_dir)?;
        assert_eq!(
            inner.workdir(),
            Some(inner_dir.as_path()),
            "parent directories are not searched, so the repository is the one that was asked for"
        );

        let broken = tmp.path().join("broken");
        std::fs::create_dir_all(broken.join(".git"))?;
        std::fs::write(broken.join(".git/not-a-repository"), "")?;
        gix::init_or_open(&broken)
            .expect_err("a directory that looks like a broken repository is not initialized over");
        assert!(
            !broken.join(".git/HEAD").exists(),
            "nothing was written into the broken repository"
        );
        Ok(())
    }
}
