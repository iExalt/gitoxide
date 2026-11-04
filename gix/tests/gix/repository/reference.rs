use gix_testtools::TestResult;

#[test]
fn ref_store_is_a_view_of_the_repository_references_that_follows_its_namespace() -> TestResult {
    let (mut repo, _keep) = crate::repo_rw("make_references_repo.sh")?;
    let via_view = repo.ref_store().iter()?.all()?.filter_map(Result::ok).count();
    assert_eq!(
        via_view,
        repo.references()?.all()?.filter_map(Result::ok).count(),
        "the view sees what the repository sees"
    );
    assert!(via_view > 0, "the fixture has references");

    repo.set_namespace("foo")?;
    let view = repo.ref_store();
    assert_eq!(
        view.namespace().map(|ns| ns.as_bstr().to_owned()),
        Some("refs/namespaces/foo/".into()),
        "a view borrows the repository and thus its namespace"
    );
    assert_eq!(view.iter()?.all()?.filter_map(Result::ok).count(), 0);
    Ok(())
}

mod set_namespace {
    use gix::refs::transaction::PreviousValue;
    use gix_testtools::tempfile;
    use gix_testtools::{Result, TestResult};

    fn easy_repo_rw() -> Result<(gix::Repository, tempfile::TempDir)> {
        crate::repo_rw("make_references_repo.sh")
    }

    #[test]
    fn affects_edits_and_iteration() -> TestResult {
        let (mut repo, _keep) = easy_repo_rw()?;
        assert_eq!(
            repo.references()?.all()?.count(),
            17,
            "there are plenty of references in the default namespace"
        );
        assert!(repo.namespace().is_none(), "no namespace is set initially");
        assert!(repo.set_namespace("foo")?.is_none(), "there is no previous namespace");

        assert_eq!(
            repo.references()?.all()?.filter_map(std::result::Result::ok).count(),
            0,
            "no references are in the namespace yet"
        );

        repo.tag_reference("new-tag", repo.object_hash().empty_tree(), PreviousValue::MustNotExist)?;

        repo.reference(
            "refs/heads/new-branch",
            repo.object_hash().empty_tree(),
            PreviousValue::MustNotExist,
            "message",
        )?;

        assert_eq!(
            repo.references()?
                .all()?
                .filter_map(std::result::Result::ok)
                .collect::<Vec<_>>(),
            vec!["refs/heads/new-branch", "refs/tags/new-tag"],
            "namespaced references appear like normal ones"
        );

        assert_eq!(
            repo.references()?
                .prefixed("refs/tags/")?
                .filter_map(std::result::Result::ok)
                .collect::<Vec<_>>(),
            vec!["refs/tags/new-tag"],
            "namespaced references appear like normal ones"
        );
        let fully_qualified_tag_name = "refs/tags/new-tag";
        assert_eq!(
            repo.find_reference(fully_qualified_tag_name)?,
            fully_qualified_tag_name,
            "fully qualified (yet namespaced) names work"
        );
        assert_eq!(
            repo.find_reference("new-tag")?,
            fully_qualified_tag_name,
            "namespaces are transparent"
        );

        let previous_ns = repo.clear_namespace().expect("namespace set");
        assert_eq!(previous_ns, "refs/namespaces/foo/");
        assert!(repo.clear_namespace().is_none(), "it doesn't invent namespaces");

        assert_eq!(
            repo.references()?.all()?.count(),
            19,
            "it lists all references, also the ones in namespaces"
        );
        Ok(())
    }
}

#[test]
fn try_find_reference_with_existing_ref_as_path_prefix_returns_none() -> TestResult {
    let (repo, _tmp) = crate::repo_rw("make_references_repo.sh")?;
    std::fs::create_dir_all(repo.git_dir().join("refs/heads"))?;
    std::fs::write(
        repo.git_dir().join("refs/heads/A"),
        repo.head_id()?.to_hex().to_string(),
    )?;

    assert!(
        repo.try_find_reference("refs/heads/A/new")?.is_none(),
        "a ref whose path prefix is an existing ref does not exist"
    );
    Ok(())
}

#[test]
fn failed_edits_keep_the_marker_that_tells_what_they_did_to_the_repository() -> TestResult {
    use gix::refs::transaction::{
        Change, LogChange, PreviousValue, RefEdit,
        commit_failure::{NothingApplied, Progress, Uncertain},
    };
    let (repo, _tmp) = crate::basic_rw_repo()?;
    let id = repo.head_id()?.detach();
    let create = |name: &str| RefEdit {
        change: Change::Update {
            log: LogChange {
                force_create_reflog: true,
                ..Default::default()
            },
            expected: PreviousValue::MustNotExist,
            new: gix::refs::Target::Object(id),
        },
        name: name.try_into().expect("valid name"),
        deref: false,
    };

    let err = repo
        .edit_references_as([create("refs/heads/x")], None)
        .expect_err("a reflog entry can't be written without a committer");
    assert!(
        err.downcast_any_ref::<NothingApplied>().is_some() && err.downcast_any_ref::<Uncertain>().is_none(),
        "retrying is known to be safe, and the porcelain doesn't hide it"
    );
    assert!(repo.try_find_reference("refs/heads/x")?.is_none());

    // A directory with content where the reflog of the second reference goes, as in the plumbing tests.
    let blocked = repo.git_dir().join("logs/refs/heads/y");
    std::fs::create_dir_all(&blocked)?;
    std::fs::write(blocked.join("content"), "makes this a directory that is not empty")?;
    let committer = gix::actor::SignatureRef {
        name: "committer".into(),
        email: "committer@example.com".into(),
        time: "0 +0000",
    };
    let err = repo
        .edit_references_as([create("refs/heads/x"), create("refs/heads/y")], Some(committer))
        .expect_err("the reflog of the second reference can't be written");
    assert_eq!(
        err.downcast_any_ref::<Uncertain>().expect("it changed something").edits,
        [
            ("refs/heads/x".try_into()?, Progress::Applied),
            ("refs/heads/y".try_into()?, Progress::InDoubt)
        ],
        "the porcelain reports how far each edit got"
    );
    Ok(())
}

/// Git neither creates reflogs in bare repositories nor with `core.logAllRefUpdates=false`, but appends to those that exist.
#[test]
fn reflogs_are_only_appended_to_when_they_are_not_autocreated() -> TestResult {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit};
    let committer = gix::actor::SignatureRef {
        name: "committer".into(),
        email: "committer@example.com".into(),
        time: "0 +0000",
    };
    let update = |repo: &gix::Repository, name: &str| -> TestResult {
        repo.edit_references_as(
            [RefEdit {
                change: Change::Update {
                    log: LogChange::default(),
                    expected: PreviousValue::Any,
                    new: gix::refs::Target::Object(repo.empty_tree().id),
                },
                name: name.try_into()?,
                deref: false,
            }],
            Some(committer),
        )?;
        Ok(())
    };
    let log_lines = |repo: &gix::Repository, name: &str| -> std::io::Result<Option<usize>> {
        match std::fs::read(repo.git_dir().join("logs").join(name)) {
            Ok(content) => Ok(Some(content.iter().filter(|b| **b == b'\n').count())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    };
    let prepare_existing_log = |repo: &gix::Repository, name: &str| -> TestResult {
        let path = repo.git_dir().join("logs").join(name);
        std::fs::create_dir_all(path.parent().expect("a log is never at the root"))?;
        std::fs::write(path, "")?;
        Ok(())
    };

    let tmp = gix_testtools::tempfile::tempdir()?;
    let bare = crate::util::init_repo_isolated(tmp.path(), gix::create::Kind::Bare)?.to_thread_local();
    prepare_existing_log(&bare, "refs/heads/existing")?;
    update(&bare, "refs/heads/existing")?;
    update(&bare, "refs/heads/new")?;
    assert_eq!(
        log_lines(&bare, "refs/heads/existing")?,
        Some(1),
        "a bare repository appends to a reflog that exists"
    );
    assert_eq!(
        log_lines(&bare, "refs/heads/new")?,
        None,
        "bare repositories create no reflogs by default"
    );

    let (mut repo, _tmp) = crate::basic_rw_repo()?;
    repo.config_snapshot_mut()
        .set_value(&gix::config::tree::Core::LOG_ALL_REF_UPDATES, "false")?;
    prepare_existing_log(&repo, "refs/heads/existing")?;
    update(&repo, "refs/heads/existing")?;
    update(&repo, "refs/heads/new")?;
    assert_eq!(
        log_lines(&repo, "refs/heads/existing")?,
        Some(1),
        "`core.logAllRefUpdates=false` still appends to existing reflogs"
    );
    assert_eq!(
        log_lines(&repo, "refs/heads/new")?,
        None,
        "`core.logAllRefUpdates=false` creates no reflogs"
    );
    Ok(())
}

mod iter_references {
    use crate::util::hex_to_id;
    use gix_testtools::{Result, TestResult};

    fn repo() -> Result<gix::Repository> {
        crate::repo("make_references_repo.sh").map(|r| r.to_thread_local())
    }

    #[test]
    fn all() -> TestResult {
        let repo = repo()?;
        assert_eq!(
            repo.references()?
                .all()?
                .filter_map(std::result::Result::ok)
                .collect::<Vec<_>>(),
            vec![
                "refs/d1",
                "refs/heads/d1",
                "refs/heads/dt1",
                "refs/heads/main",
                "refs/heads/multi-link-target1",
                "refs/loop-a",
                "refs/loop-b",
                "refs/multi-link",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
                "refs/remotes/origin/multi-link-target3",
                "refs/tags/dt1",
                "refs/tags/dt2",
                "refs/tags/dt3",
                "refs/tags/multi-link-target2",
                "refs/tags/t1"
            ]
        );
        Ok(())
    }

    /// What Git lists is what resolves: it skips broken references and symbolic references that dangle or loop, with a
    /// warning. Here these are returned or fail to peel, so a caller that wants Git's list keeps what resolves.
    #[test]
    fn what_resolves_is_what_git_lists_and_the_rest_can_be_told_apart() -> TestResult {
        let (repo, _tmp) = crate::repo_rw("make_references_repo.sh")?;
        std::fs::write(repo.git_dir().join("refs/heads/dangling"), "ref: refs/heads/gone\n")?;
        std::fs::create_dir(repo.git_dir().join("refs/heads/m"))?;
        std::fs::write(repo.git_dir().join("refs/heads/m/x"), repo.head_id()?.to_string())?;
        std::fs::write(repo.git_dir().join("refs/heads/mine"), repo.head_id()?.to_string())?;
        let git_lists = |args: &str| -> TestResult<Vec<String>> {
            Ok(gix_testtools::git(repo.git_dir(), args)?
                .lines()
                .map(ToOwned::to_owned)
                .collect())
        };
        let resolvable = |prefix: Option<&str>| -> TestResult<Vec<String>> {
            let platform = repo.references()?;
            let mut names = Vec::new();
            let iter = match prefix {
                Some(prefix) => platform.under(prefix)?,
                None => platform.all()?,
            };
            for reference in iter.filter_map(std::result::Result::ok) {
                let name = reference.name().as_bstr().to_string();
                if reference.clone().peel_to_id().is_ok() {
                    names.push(name);
                }
            }
            Ok(names)
        };
        assert_eq!(
            resolvable(None)?,
            git_lists("for-each-ref --format=%(refname)")?,
            "the references that resolve are the ones `git for-each-ref` lists, in the same order"
        );
        assert_eq!(
            resolvable(Some("refs/heads/"))?,
            git_lists("for-each-ref --format=%(refname) refs/heads/")?,
            "also with a prefix that ends in a directory"
        );
        assert_eq!(
            resolvable(Some("refs/heads/m"))?,
            git_lists("for-each-ref --format=%(refname) refs/heads/m")?,
            "`under()` is made of whole components, as it is for Git, so `m` doesn't match `main`"
        );

        for exact_or_directory in [
            "refs/heads/m",
            "refs/heads/m/",
            "refs/heads/main",
            "refs/heads/main/",
            "refs/heads/mine",
            "refs/tags",
            "refs/remotes/origin",
        ] {
            let git = git_lists(&format!("for-each-ref --format=%(refname) {exact_or_directory}"))?;
            assert_eq!(
                resolvable(Some(exact_or_directory))?,
                git,
                "{exact_or_directory} is matched like Git does it, as a name or a directory"
            );
            if !exact_or_directory.ends_with("main/") {
                assert!(
                    !git.is_empty(),
                    "{exact_or_directory} has matches, so this isn't comparing nothing"
                );
            }
        }

        assert!(
            repo.try_find_reference("refs/nope")?.is_none(),
            "absent references are None"
        );
        assert!(
            repo.try_find_reference("refs/broken").is_err(),
            "a corrupt reference is an error, not an absent one"
        );
        for dangling in ["refs/heads/dangling", "refs/loop-a"] {
            let mut reference = repo
                .try_find_reference(dangling)?
                .expect("a symbolic reference that doesn't resolve still exists");
            assert!(
                reference.target().try_name().is_some(),
                "{dangling} is a symbolic reference, which Git's `symbolic-ref` reads"
            );
            reference.peel_to_id().expect_err("but it doesn't resolve to an object");
        }
        Ok(())
    }

    #[test]
    fn prefixed() -> TestResult {
        let repo = repo()?;
        assert_eq!(
            repo.references()?
                .prefixed("refs/heads/")?
                .filter_map(std::result::Result::ok)
                .map(|r| (
                    r.name().as_bstr().to_string(),
                    r.target().try_id().map(ToOwned::to_owned)
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "refs/heads/d1".to_string(),
                    Some(hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"))
                ),
                (
                    "refs/heads/dt1".into(),
                    hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03").into()
                ),
                (
                    "refs/heads/main".into(),
                    hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03").into()
                ),
                ("refs/heads/multi-link-target1".into(), None),
            ]
        );
        Ok(())
    }

    #[test]
    fn prefixed_and_peeled() -> TestResult {
        let repo = repo()?;
        assert_eq!(
            repo.references()?
                .prefixed(b"refs/heads/")?
                .peeled()?
                .filter_map(std::result::Result::ok)
                .map(|r| (
                    r.name().as_bstr().to_string(),
                    r.target().try_id().map(ToOwned::to_owned)
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "refs/heads/d1".to_string(),
                    Some(hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03"))
                ),
                (
                    "refs/heads/dt1".into(),
                    hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03").into()
                ),
                (
                    "refs/heads/main".into(),
                    hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03").into()
                ),
                (
                    "refs/remotes/origin/multi-link-target3".into(),
                    hex_to_id("134385f6d781b7e97062102c6a483440bfda2a03").into()
                ),
            ]
        );
        Ok(())
    }

    /// Regression test for https://github.com/GitoxideLabs/gitoxide/issues/2103
    /// This only ensures we can return a reference, not that the code below is correct
    #[test]
    fn tags() -> TestResult {
        let repo = repo()?;
        let actual = repo
            .references()?
            .tags()?
            .filter_map(std::result::Result::ok)
            .max_by_key(|tag| tag.name().shorten().to_owned())
            .ok_or(std::io::Error::other("latest tag not found"))?;
        assert_eq!(actual, "refs/tags/t1");
        Ok(())
    }
}

mod head {

    use gix_ref::transaction::PreviousValue;
    use gix_testtools::TestResult;

    use crate::util::hex_to_id;

    #[test]
    fn symbolic() -> TestResult {
        let repo = crate::basic_repo()?;
        let head = repo.head()?;
        match &head.kind {
            gix::head::Kind::Symbolic(r) => {
                assert_eq!(
                    r.target.try_id().map(ToOwned::to_owned),
                    Some(hex_to_id("3189cd3cb0af8586c39a838aa3e54fd72a872a41"))
                );
            }
            _ => panic!("unexpected head kind"),
        }
        assert_eq!(head.referent_name().expect("born"), "refs/heads/main");
        assert!(!head.is_detached());
        Ok(())
    }

    #[test]
    fn detached() -> TestResult {
        let (repo, _keep) = crate::basic_rw_repo()?;
        repo.reference(
            "HEAD",
            hex_to_id("3189cd3cb0af8586c39a838aa3e54fd72a872a41"),
            PreviousValue::Any,
            "",
        )?;

        let head = repo.head()?;
        assert!(head.is_detached(), "head is detached");
        assert!(head.referent_name().is_none());
        Ok(())
    }
}
