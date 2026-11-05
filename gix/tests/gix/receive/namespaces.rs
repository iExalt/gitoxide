//! A receiver that has a namespace sees the references in it without it, leaves the others alone, and tells hooks about it,
//! all like `git receive-pack` does with `GIT_NAMESPACE`. The scenarios of `receive_pack` also run in a namespace, which has
//! copies of all references, so that is where the rules about names are compared.
//!
//! The namespaces are made by `Push::namespace`, which copies all references into one, and the receiver is told about it
//! by setting the namespace of the repository, which is what `GIT_NAMESPACE` does for Git.
use gix_testtools::TestResult;

use super::receive_pack::{Cmd, Hook, Push, World, compare_on, fork_receive, namespace_of, push, with_setup};

#[test]
fn the_advertisement_shows_the_namespace_and_the_rest_as_have() -> TestResult {
    let world = World::new()?;
    // A tree that no reference of the namespace has, which only another namespace leads to.
    let elsewhere = "update-ref refs/namespaces/ns2/refs/tags/elsewhere refs/heads/main^{tree}";
    let cases: &[(&str, Push)] = &[
        (
            "plain",
            Push {
                namespace: Some("ns1"),
                ..with_setup(Push::default(), &[elsewhere])
            },
        ),
        (
            "nested",
            Push {
                namespace: Some("a/b"),
                ..with_setup(Push::default(), &[elsewhere])
            },
        ),
        (
            "hidden-by-name-in-namespace",
            Push {
                namespace: Some("ns1"),
                ..with_setup(Push::default(), &[elsewhere, "config receive.hideRefs refs/heads/side"])
            },
        ),
        (
            "hidden-by-full-name",
            Push {
                namespace: Some("ns1"),
                ..with_setup(
                    Push::default(),
                    &[
                        elsewhere,
                        "config receive.hideRefs ^refs/namespaces/ns1/refs/heads/side",
                    ],
                )
            },
        ),
        (
            "plain-patterns-leave-other-namespaces-alone",
            Push {
                namespace: Some("ns1"),
                ..with_setup(
                    Push::default(),
                    &[
                        elsewhere,
                        "config receive.hideRefs refs/namespaces/ns2/refs/tags/elsewhere",
                    ],
                )
            },
        ),
        (
            "other-namespaces-hidden-by-full-name",
            Push {
                namespace: Some("ns1"),
                ..with_setup(
                    Push::default(),
                    &[
                        elsewhere,
                        "config receive.hideRefs ^refs/namespaces/ns2/refs/tags/elsewhere",
                    ],
                )
            },
        ),
        (
            // The capabilities go with the first line, which is one for what isn't in the namespace.
            "empty-namespace-next-to-others",
            Push {
                namespace: Some("ns1"),
                ..with_setup(
                    Push::default(),
                    &[
                        elsewhere,
                        "update-ref -d refs/heads/main",
                        "update-ref -d refs/heads/side",
                    ],
                )
            },
        ),
        (
            "nothing-at-all",
            Push {
                namespace: Some("ns1"),
                ..with_setup(
                    Push::default(),
                    &["update-ref -d refs/heads/main", "update-ref -d refs/heads/side"],
                )
            },
        ),
    ];
    for (label, case) in cases {
        for format in ["files", "reftable"] {
            let git_remote = world.remote(format, &format!("adv-git-{label}"), case)?;
            let mut command = gix_testtools::git_command(&git_remote);
            command
                .env("LC_ALL", "C")
                .env("LANG", "C")
                .env(
                    "GIT_NAMESPACE",
                    namespace_of(&git_remote).expect("the namespace was made"),
                )
                .args(["receive-pack", "--advertise-refs"])
                .arg(std::fs::canonicalize(&git_remote)?);
            let output = command.output()?;
            assert!(output.status.success(), "{format}/{label}: Git advertises");
            if *label == "plain" {
                assert!(
                    super::receive_pack::without_agent(&output.stdout)
                        .iter()
                        .any(|line| line.contains(" .have")),
                    "{format}/{label}: what is in other namespaces is shown as `.have`, so the comparison isn't vacuous"
                );
            }

            let fork_remote = world.remote(format, &format!("adv-fork-{label}"), case)?;
            let mut repo = gix::open_opts(&fork_remote, gix::open::Options::isolated())?;
            repo.set_namespace(namespace_of(&fork_remote).expect("the namespace was made").as_str())?;
            let advertisement = repo
                .receive()?
                .advertisement()?
                .encode(gix_protocol::transport::Protocol::V0)?;
            assert_eq!(
                super::receive_pack::without_agent(&advertisement),
                super::receive_pack::without_agent(&output.stdout),
                "{format}/{label}: the same is offered"
            );
        }
    }
    Ok(())
}

#[test]
fn hooks_hear_the_names_of_the_namespace_and_which_it_is() -> TestResult {
    let world = World::new()?;
    let hooks: Vec<_> = [
        "pre-receive",
        "update",
        "post-receive",
        "post-update",
        "reference-transaction",
    ]
    .into_iter()
    .map(Hook::new)
    .collect();
    let commands = [
        Cmd("0", "next", "refs/heads/next"),
        Cmd("main", "next", "refs/heads/main"),
        Cmd("side", "0", "refs/heads/side"),
    ];
    for namespace in ["ns1", "a/b"] {
        let pushed = Push {
            hooks: hooks.clone(),
            namespace: Some(namespace),
            ..with_setup(push(&commands), &["config receive.denyDeletes false"])
        };
        let results = compare_on(
            &world,
            &format!("hooks-in-{}", namespace.replace('/', "-")),
            &pushed,
            &["files", "reftable"],
        )?;
        let log = &results[0].hooks;
        assert!(
            log.contains(&format!("GIT_NAMESPACE={namespace}\n"))
                && log.contains("== update refs/heads/next ")
                && log.contains(" refs/heads/side\n"),
            "hooks are told which namespace it is, and the names that clients know, in `{namespace}`:\n{log}"
        );
        assert!(
            log.contains(&format!(
                " refs/namespaces/{}/refs/heads/next\n",
                namespace.replace('/', "/refs/namespaces/")
            )),
            "the reference-transaction hook sees the references with the namespace, as the store has them:\n{log}"
        );
    }
    Ok(())
}

/// Git crashes when it is to update a symbolic reference of the namespace that leads out of it, so there is nothing to compare.
/// The receiver refuses, and leaves everything as it is, instead of changing a reference that isn't the pusher's.
#[test]
fn a_symbolic_reference_that_leads_out_of_the_namespace_is_refused() -> TestResult {
    fn leads_to_a_global_reference(remote: &std::path::Path, _client: &std::path::Path) -> TestResult {
        gix_testtools::git(
            remote,
            "symbolic-ref refs/namespaces/ns1/refs/heads/sym refs/heads/main",
        )?;
        Ok(())
    }
    fn leads_to_another_namespace(remote: &std::path::Path, _client: &std::path::Path) -> TestResult {
        gix_testtools::git(
            remote,
            "update-ref refs/namespaces/ns2/refs/heads/other refs/heads/main",
        )?;
        gix_testtools::git(
            remote,
            "symbolic-ref refs/namespaces/ns1/refs/heads/sym refs/namespaces/ns2/refs/heads/other",
        )?;
        Ok(())
    }
    fn leads_nowhere_outside(remote: &std::path::Path, _client: &std::path::Path) -> TestResult {
        gix_testtools::git(
            remote,
            "symbolic-ref refs/namespaces/ns1/refs/heads/sym refs/heads/nowhere",
        )?;
        Ok(())
    }
    let world = World::new()?;
    for (label, prepare) in [
        (
            "global",
            leads_to_a_global_reference as fn(&std::path::Path, &std::path::Path) -> TestResult,
        ),
        ("other-namespace", leads_to_another_namespace),
        ("dangling", leads_nowhere_outside),
    ] {
        let pushed = Push {
            namespace: Some("ns1"),
            prepare: Some(prepare),
            // The symbolic reference doesn't lead to `main` in the namespace, so its value is that of `main` only by chance.
            ..push(&[Cmd("0", "next", "refs/heads/sym")])
        };
        let format = "files";
        let request = world.request(format, &pushed)?;
        let remote = world.remote(format, &format!("outside-{label}"), &pushed)?;
        let before = gix_testtools::git(&remote, "for-each-ref --format='%(refname) %(objectname) %(symref)'")?;
        let fork = fork_receive(&remote, &request, true)?;
        assert!(
            String::from_utf8_lossy(&fork.report).contains("ng refs/heads/sym broken symref"),
            "{label}: the push is refused, and not reported as done:\n{}",
            String::from_utf8_lossy(&fork.report)
        );
        assert!(
            fork.messages
                .contains("refusing update to broken symref 'refs/heads/sym'"),
            "{label}: the pusher learns why: {}",
            fork.messages
        );
        let after = gix_testtools::git(&remote, "for-each-ref --format='%(refname) %(objectname) %(symref)'")?;
        assert_eq!(
            after, before,
            "{label}: no reference changed, in or out of the namespace"
        );
    }
    Ok(())
}

#[test]
fn the_current_branch_is_the_one_that_head_leads_to_even_if_that_is_in_the_namespace() -> TestResult {
    let world = World::new()?;
    // `HEAD` is global, and leads to `main` of the namespace, which only a push into the namespace can update or delete.
    let head_in_namespace = "symbolic-ref HEAD refs/namespaces/ns1/refs/heads/main";
    let commands: [(&str, Vec<Cmd>); 2] = [
        ("update", vec![Cmd("main", "next", "refs/heads/main")]),
        ("delete", vec![Cmd("main", "0", "refs/heads/main")]),
    ];
    for remote in [None, Some("work")] {
        for (what, commands) in &commands {
            for rule in [None, Some("ignore"), Some("warn"), Some("refuse")] {
                let mut setup = vec![head_in_namespace, "config receive.denyCurrentBranch ignore"];
                if let Some(rule) = rule {
                    setup.push(Box::leak(
                        format!("config receive.denyDeleteCurrent {rule}").into_boxed_str(),
                    ));
                }
                let pushed = Push {
                    namespace: Some("ns1"),
                    remote,
                    ..with_setup(push(commands), &setup)
                };
                let label = format!("head-in-namespace-{what}-{}-{rule:?}", remote.unwrap_or("bare"));
                let results = compare_on(&world, &label, &pushed, &["files"])?;
                let report = String::from_utf8_lossy(&results[0].report).into_owned();
                if *what == "delete" && rule.is_none() {
                    assert!(
                        report.contains("deletion of the current branch prohibited"),
                        "{label}: Git protects the branch that `HEAD` leads to, which makes this comparison meaningful:\n{report}"
                    );
                }
            }
        }
    }
    Ok(())
}

/// What Git does when it deletes a reference whose old object is missing is to look for the reference by the name without the
/// namespace, which is a different reference, to tell if it is corrupt or doesn't exist, which tells if post hooks run.
#[test]
fn deleting_a_corrupt_reference_in_a_namespace_looks_at_the_name_without_it_like_git() -> TestResult {
    fn corrupt_in_namespace(remote: &std::path::Path, _client: &std::path::Path) -> TestResult {
        write_corrupt_reference(remote)
    }
    fn corrupt_in_namespace_and_present_outside(remote: &std::path::Path, _client: &std::path::Path) -> TestResult {
        write_corrupt_reference(remote)?;
        gix_testtools::git(remote, "update-ref refs/heads/broken refs/heads/main")?;
        Ok(())
    }
    fn write_corrupt_reference(remote: &std::path::Path) -> TestResult {
        let git_dir = std::path::PathBuf::from(gix_testtools::git(remote, "rev-parse --absolute-git-dir")?.trim());
        if git_dir.join("reftable").exists() {
            return Ok(());
        }
        let path = git_dir.join("refs/namespaces/ns1/refs/heads/broken");
        std::fs::create_dir_all(path.parent().expect("a parent"))?;
        std::fs::write(path, format!("{}\n", corrupt_id()))?;
        Ok(())
    }
    fn corrupt_id() -> String {
        let kind = gix_testtools::object_hash_from_env().unwrap_or(gix_hash::Kind::Sha1);
        "ab".repeat(kind.len_in_bytes())
    }
    let world = World::new()?;
    let id: &'static str = Box::leak(corrupt_id().into_boxed_str());
    for (label, prepare) in [
        (
            "only-in-namespace",
            corrupt_in_namespace as fn(&std::path::Path, &std::path::Path) -> TestResult,
        ),
        ("also-outside", corrupt_in_namespace_and_present_outside),
    ] {
        let hooks = ["post-receive", "post-update"].into_iter().map(Hook::new).collect();
        let pushed = Push {
            namespace: Some("ns1"),
            prepare: Some(prepare),
            hooks,
            ..push(&[Cmd(id, "0", "refs/heads/broken")])
        };
        let results = compare_on(&world, &format!("corrupt-{label}"), &pushed, &["files"])?;
        assert!(
            String::from_utf8_lossy(&results[0].report).contains("ok refs/heads/broken"),
            "{label}: Git deletes the corrupt reference:\n{}",
            String::from_utf8_lossy(&results[0].report)
        );
    }
    Ok(())
}
