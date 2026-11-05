use gix::bstr::ByteSlice;
use gix_testtools::TestResult;

use super::{Setup, fixtures, fork_serve, git_serve, ls_refs_request};

/// The advertisement is what Git writes, but for what isn't served, see the tests of `gix-protocol`.
#[test]
fn the_listing_is_what_git_lists_for_each_combination_of_arguments() -> TestResult {
    let combinations: &[&[&str]] = &[
        &[],
        &["peel"],
        &["symrefs"],
        &["peel", "symrefs"],
        &["peel", "symrefs", "unborn"],
        &["symrefs", "unborn", "ref-prefix HEAD"],
        &["peel", "ref-prefix refs/tags/", "ref-prefix refs/heads/m"],
        &["symrefs", "ref-prefix refs/remotes/"],
        &["ref-prefix nothing-matches"],
    ];
    for f in fixtures()? {
        for name in [
            "remote.git",
            "unborn.git",
            "unborn-head.git",
            "detached.git",
            "namespaced.git",
        ] {
            for args in combinations {
                let request = ls_refs_request(f.kind, args);
                let git = git_serve(&f.repo(name), &Setup::default(), &request)?;
                let (fork, result) = fork_serve(&f.repo(name), &Setup::default(), &request)?;
                result?;
                assert_eq!(
                    fork.response.as_bstr(),
                    git.response.as_bstr(),
                    "{} {name} {args:?}: the listing is byte for byte the one of Git",
                    f.label()
                );
            }
        }
    }
    Ok(())
}

#[test]
fn the_listing_is_not_vacuous() -> TestResult {
    // Guard against fixtures that silently stop exercising what the comparison above is meant to cover.
    for f in fixtures()? {
        let list = |name: &str, args: &[&str]| -> TestResult<String> {
            let git = git_serve(&f.repo(name), &Setup::default(), &ls_refs_request(f.kind, args))?;
            Ok(git.response.to_str_lossy().into_owned())
        };
        let all = list("remote.git", &["peel", "symrefs", "unborn"])?;
        for needle in [
            " HEAD symref-target:refs/heads/main",
            " refs/tags/v1 peeled:",
            " refs/remotes/origin/HEAD symref-target:refs/remotes/origin/main",
            " refs/tags/light\n",
        ] {
            assert!(all.contains(needle), "{}: {needle:?} is listed:\n{all}", f.label());
        }
        assert!(
            list("unborn.git", &["symrefs", "unborn"])?.contains("unborn HEAD symref-target:refs/heads/main"),
            "{}: an unborn HEAD is shown to clients that ask",
            f.label()
        );
        assert!(
            !list("unborn.git", &["unborn"])?.contains("unborn HEAD"),
            "{}: but not without symrefs",
            f.label()
        );
        let detached = list("detached.git", &["symrefs"])?;
        let head = detached
            .lines()
            .find(|line| line.split(' ').nth(1) == Some("HEAD"))
            .expect("HEAD is listed");
        assert!(
            !head.contains("symref-target"),
            "{}: a detached HEAD has no symref-target: {head}",
            f.label()
        );
    }
    Ok(())
}

#[test]
fn what_lsrefs_unborn_allows_decides_if_an_unborn_head_is_shown() -> TestResult {
    for f in fixtures()? {
        for value in ["advertise", "allow", "ignore"] {
            for name in ["unborn.git", "unborn-head.git"] {
                let setup = Setup {
                    config: &[&format!("lsrefs.unborn={value}")],
                    ..Setup::default()
                };
                let request = ls_refs_request(f.kind, &["symrefs", "unborn"]);
                let git = git_serve(&f.repo(name), &setup, &request)?;
                let (fork, result) = fork_serve(&f.repo(name), &setup, &request)?;
                result?;
                assert_eq!(
                    fork.response.as_bstr(),
                    git.response.as_bstr(),
                    "{} {name} lsrefs.unborn={value}",
                    f.label()
                );
                assert_eq!(
                    fork.advertisement.contains_str("ls-refs=unborn"),
                    git.advertisement.contains_str("ls-refs=unborn"),
                    "{} {name} lsrefs.unborn={value}: only `advertise` announces it",
                    f.label()
                );
                assert_eq!(
                    git.response.contains_str("unborn HEAD"),
                    value != "ignore",
                    "{} {name} {value}: what the clients get is as the setting says",
                    f.label()
                );
            }
        }
    }
    Ok(())
}

#[test]
fn hidden_references_are_not_listed_like_in_git() -> TestResult {
    let configurations: &[&[&str]] = &[
        &["uploadpack.hideRefs=refs/heads/side"],
        &["transfer.hideRefs=refs/tags"],
        &["transfer.hideRefs=refs/heads", "uploadpack.hideRefs=!refs/heads/main"],
        &["uploadpack.hideRefs=refs/remotes"],
        &[
            "uploadpack.hideRefs=refs/heads/side",
            "uploadpack.hideRefs=refs/heads/merged",
            "transfer.hideRefs=refs/tags/v1",
        ],
        &["uploadpack.hideRefs=refs/heads/ma"],
        // `receive.hideRefs` is not for readers.
        &["receive.hideRefs=refs/heads"],
    ];
    for f in fixtures()? {
        for config in configurations {
            let setup = Setup {
                config,
                ..Setup::default()
            };
            let request = ls_refs_request(f.kind, &["peel", "symrefs"]);
            let git = git_serve(&f.repo("remote.git"), &setup, &request)?;
            let (fork, result) = fork_serve(&f.repo("remote.git"), &setup, &request)?;
            result?;
            assert_eq!(
                fork.response.as_bstr(),
                git.response.as_bstr(),
                "{} {config:?}: the same references are hidden",
                f.label()
            );
            let unhidden = git_serve(&f.repo("remote.git"), &Setup::default(), &request)?;
            assert_eq!(
                git.response != unhidden.response,
                !(config[0].starts_with("receive.") || config[0].ends_with("refs/heads/ma")),
                "{} {config:?}: the configuration hides something, except if it is for writers or isn't a whole component of a name",
                f.label()
            );
        }
    }
    Ok(())
}

#[test]
fn namespaces_show_their_own_head_and_no_references_of_others() -> TestResult {
    for f in fixtures()? {
        for (namespace, config) in [
            ("team", &[][..]),
            ("team-unborn", &[]),
            ("missing", &[]),
            ("team", &["uploadpack.hideRefs=refs/heads/side"]),
            ("team", &["uploadpack.hideRefs=^refs/namespaces/team/refs/heads/side"]),
            ("team", &["uploadpack.hideRefs=refs/namespaces/team"]),
        ] {
            let setup = Setup {
                config,
                namespace: Some(namespace),
            };
            for args in [&["peel", "symrefs", "unborn"][..], &["symrefs"], &["ref-prefix HEAD"]] {
                let request = ls_refs_request(f.kind, args);
                let git = git_serve(&f.repo("namespaced.git"), &setup, &request)?;
                let (fork, result) = fork_serve(&f.repo("namespaced.git"), &setup, &request)?;
                result?;
                assert_eq!(
                    fork.response.as_bstr(),
                    git.response.as_bstr(),
                    "{} namespace {namespace} {config:?} {args:?}",
                    f.label()
                );
            }
        }
        let team = git_serve(
            &f.repo("namespaced.git"),
            &Setup {
                namespace: Some("team"),
                ..Setup::default()
            },
            &ls_refs_request(f.kind, &["symrefs"]),
        )?;
        let text = team.response.to_str_lossy();
        assert!(
            text.contains(" HEAD symref-target:refs/heads/main\n") && !text.contains("outside"),
            "{}: the namespace has its own HEAD, with the target stripped of the namespace:\n{text}",
            f.label()
        );
    }
    Ok(())
}
