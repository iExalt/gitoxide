//! Hooks of a receiver are run like `git receive-pack` runs them: with the same arguments, input and environment, in the
//! same order, with what they say relayed to the client and with their exit codes deciding the same.
//!
//! The hooks are scripts that record how they were called, see `Hook`, so the recordings of Git and the receiver are compared
//! on top of everything that `receive_pack` compares. Where the position of a hook in a recording is what matters, it is
//! asserted on what Git recorded, so that the comparison can't be passed by neither doing it.
#![cfg(unix)]
use gix_testtools::TestResult;

use super::receive_pack::{Cmd, Hook, Push, World, assert_same, compare_on, push, with_setup};

fn all_hooks() -> Vec<Hook> {
    ["pre-receive", "update", "post-receive", "post-update"]
        .into_iter()
        .map(Hook::new)
        .collect()
}

fn with_hooks(push: Push, hooks: Vec<Hook>) -> Push {
    Push { hooks, ..push }
}

/// The position of `needle` in what a hook recorded, which has to be there.
fn at(log: &str, needle: &str) -> usize {
    log.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} is in the recording of Git:\n{log}"))
}

fn git_log(world: &World, label: &str, push: &Push, formats: &[&str]) -> TestResult<String> {
    let mut results = compare_on(world, label, push, formats)?;
    Ok(results.remove(0).hooks)
}

#[test]
fn all_hooks_see_what_they_see_in_git() -> TestResult {
    let world = World::new()?;
    let commands = [
        Cmd("0", "next", "refs/heads/next"),
        Cmd("main", "next", "refs/heads/main"),
        Cmd("side", "0", "refs/heads/side"),
    ];
    let pushed = with_hooks(
        with_setup(push(&commands), &["config receive.denyCurrentBranch ignore"]),
        all_hooks(),
    );
    let log = git_log(&world, "hooks-non-atomic", &pushed, &["files", "reftable"])?;
    assert!(
        log.contains("GIT_PUSH_OPTION_COUNT=0\n"),
        "the count is set without options, as Git does it:\n{log}"
    );
    let pre = &log[at(&log, "== pre-receive")..at(&log, "== update")];
    assert!(
        pre.contains("object_dir=quarantine") && pre.contains("quarantine=set") && pre.contains("visible: commit"),
        "pre-receive sees the objects of the push in the quarantine:\n{pre}"
    );
    let update = &log[at(&log, "== update")..at(&log, "== post-receive")];
    assert!(
        update.contains("quarantine=unset") && update.contains("GIT_PUSH_OPTION_COUNT=unset"),
        "update is called after the objects moved, and gets no push options:\n{update}"
    );
    assert!(
        at(&log, "== update refs/heads/side") < at(&log, "== update refs/heads/next"),
        "without atomicity deletions are executed first, and so their update hooks run first:\n{log}"
    );
    let post = &log[at(&log, "== post-receive")..at(&log, "== post-update")];
    assert!(
        post.contains("quarantine=unset") && post.contains("GIT_PUSH_OPTION_COUNT=0"),
        "post-receive has no quarantine but the count:\n{post}"
    );
    assert!(
        log[at(&log, "== post-update")..]
            .starts_with("== post-update refs/heads/next refs/heads/main refs/heads/side\n"),
        "post-update gets the names of what was updated, in the order of the commands:\n{log}"
    );
    git_log(&world, "hooks-atomic", &Push { atomic: true, ..pushed }, &["files"])?;
    Ok(())
}

#[test]
fn reference_transaction_sees_the_transactions_of_the_receiver_like_in_git() -> TestResult {
    let world = World::new()?;
    let mut hooks = all_hooks();
    hooks.push(Hook::new("reference-transaction"));
    let commands = [
        Cmd("0", "next", "refs/heads/next"),
        Cmd("main", "next", "refs/heads/main"),
        Cmd("side", "0", "refs/heads/side"),
    ];
    let pushed = with_hooks(
        with_setup(push(&commands), &["config receive.denyCurrentBranch ignore"]),
        hooks,
    );
    let log = git_log(&world, "reftx-non-atomic", &pushed, &["files", "reftable"])?;
    let phases: Vec<_> = log
        .lines()
        .filter(|line| line.starts_with("== reference-transaction "))
        .collect();
    assert_eq!(
        phases,
        [
            // Deleting a reference makes the files backend look at `packed-refs` first, which it does in a transaction of its own
            // that is aborted when there is nothing to delete there.
            "== reference-transaction preparing",
            "== reference-transaction aborted",
            "== reference-transaction prepared",
            "== reference-transaction committed",
            "== reference-transaction preparing",
            "== reference-transaction prepared",
            "== reference-transaction committed",
        ],
        "without atomicity deletions are a transaction before all other updates, and the hook hears of each state:\n{log}"
    );
    assert!(
        at(&log, "== update refs/heads/side") < at(&log, "== reference-transaction preparing")
            && at(&log, "== reference-transaction committed") < at(&log, "== update refs/heads/next"),
        "the update hooks of each transaction decide before it touches references, and the deletions are done first:\n{log}"
    );
    assert!(
        at(&log, "== reference-transaction committed") < at(&log, "== post-receive"),
        "the hooks that follow are run after the references changed:\n{log}"
    );
    git_log(
        &world,
        "reftx-atomic",
        &Push { atomic: true, ..pushed },
        &["files", "reftable"],
    )?;
    Ok(())
}

#[test]
fn reference_transaction_sees_a_packed_reference_being_deleted() -> TestResult {
    let world = World::new()?;
    let pushed = with_hooks(
        with_setup(
            push(&[Cmd("side", "0", "refs/heads/side")]),
            &["config receive.denyDeletes false", "pack-refs --all"],
        ),
        vec![Hook::new("reference-transaction")],
    );
    // The files backend adds a transaction for `packed-refs`, which is compared as well as the one of the reference.
    let log = git_log(&world, "reftx-packed", &pushed, &["files", "reftable"])?;
    assert!(
        log.contains("== reference-transaction prepared"),
        "Git ran the hook, so the comparison isn't vacuous:\n{log}"
    );
    Ok(())
}

#[test]
fn a_reference_transaction_hook_that_refuses_ends_the_push_without_a_report() -> TestResult {
    let world = World::new()?;
    let commands = [Cmd("0", "next", "refs/heads/next"), Cmd("side", "0", "refs/heads/side")];
    for state in ["preparing", "prepared"] {
        for atomic in [false, true] {
            let mut hooks = all_hooks();
            hooks.push(Hook::new("reference-transaction").declining(state));
            let pushed = Push {
                atomic,
                ..with_hooks(
                    with_setup(push(&commands), &["config receive.denyDeletes false"]),
                    hooks,
                )
            };
            let label = format!("reftx-refuses-{state}-{atomic}");
            let results = compare_on(&world, &label, &pushed, &["files", "reftable"])?;
            for result in &results {
                assert!(
                    result.died && result.report.is_empty(),
                    "{label}: Git exits instead of reporting if the hook refuses in '{state}':\n{}",
                    result.hooks
                );
                assert!(
                    result.hooks.contains("== update refs/heads/side") && !result.hooks.contains("== post-"),
                    "{label}: the update hooks ran, and nothing follows:\n{}",
                    result.hooks
                );
                assert!(
                    result.messages.contains("update says hi"),
                    "{label}: what hooks said before the refusal is told to the client: {:?}",
                    result.messages
                );
            }
        }
    }
    // What isn't a state that can be refused is a message only.
    let mut hooks = all_hooks();
    hooks.push(Hook::new("reference-transaction").declining("committed"));
    let results = compare_on(
        &world,
        "reftx-refuses-committed",
        &with_hooks(
            with_setup(push(&commands), &["config receive.denyDeletes false"]),
            hooks,
        ),
        &["files", "reftable"],
    )?;
    assert!(
        !results[0].died && results[0].hooks.contains("== post-update"),
        "a failure after the commit changes nothing:\n{}",
        results[0].hooks
    );
    Ok(())
}

/// `count` commands that create references below `refs/heads/bulk/`, enough to fill a pipe, which holds 64KiB, with 800.
fn bulk_commands(count: usize) -> Vec<Cmd> {
    (0..count)
        .map(|index| {
            let name: &'static str = Box::leak(format!("refs/heads/bulk/{index}").into_boxed_str());
            Cmd("0", "next", name)
        })
        .collect()
}

#[test]
fn hooks_are_found_where_git_looks_for_them_and_only_if_they_can_be_executed() -> TestResult {
    let world = World::new()?;
    let commands = [Cmd("0", "next", "refs/heads/next")];
    // `core.hooksPath` replaces the directory of hooks, and relative paths start where the hooks run, which is the Git directory.
    let pushed = Push {
        hooks: vec![
            Hook::new("pre-receive").exiting(1),
            Hook::new("pre-receive").in_dir("custom-hooks"),
            Hook::new("post-update").in_dir("custom-hooks"),
        ],
        ..with_setup(push(&commands), &["config core.hooksPath custom-hooks"])
    };
    let results = compare_on(&world, "hooks-path", &pushed, &["files", "reftable"])?;
    assert!(
        !results[0].report.is_empty()
            && !String::from_utf8_lossy(&results[0].report).contains("ng ")
            && results[0].hooks.contains("== post-update"),
        "the hooks of the configured directory ran, and the declining one of the default directory didn't:\n{}",
        results[0].hooks
    );
    // A hook that isn't executable is ignored, with a hint for people that Git prints but doesn't tell the client.
    let pushed = Push {
        hooks: vec![
            Hook::new("pre-receive").exiting(1).with_mode(0o644),
            Hook::new("post-receive"),
        ],
        ..push(&commands)
    };
    let results = compare_on(&world, "hooks-not-executable", &pushed, &["files"])?;
    assert!(
        !results[0].hooks.contains("== pre-receive") && results[0].hooks.contains("== post-receive"),
        "the hook that can't be executed is skipped:\n{}",
        results[0].hooks
    );
    Ok(())
}

#[test]
fn hooks_from_configuration_run_with_those_of_the_directory_like_in_git() -> TestResult {
    let world = World::new()?;
    let commands = [Cmd("0", "next", "refs/heads/next")];
    let configured = |jobs: &'static str| {
        let hooks = vec![
            // What hooks that run at the same time say is relayed in the order of their completion, which is a race.
            Hook::new("pre-receive").named("directory").silent(),
            Hook::new("pre-receive").named("first").in_dir("configured").silent(),
            Hook::new("pre-receive")
                .named("second")
                .in_dir("configured")
                .exiting(1)
                .silent(),
        ];
        Push {
            hooks,
            ..with_setup(
                push(&commands),
                &[
                    "config hook.first.event pre-receive",
                    "config hook.first.command ./configured/first",
                    "config hook.second.event pre-receive",
                    "config hook.second.command ./configured/second",
                    jobs,
                ],
            )
        }
    };
    // `directory` is the hook of the event, which has to be called `pre-receive` to be found.
    let mut pushed = configured("config hook.jobs 1");
    pushed.hooks[0].file = None;
    let results = compare_on(&world, "config-hooks-serial", &pushed, &["files"])?;
    assert!(
        results[0].hooks.contains("== first") && results[0].hooks.contains("== second"),
        "Git runs the hooks of the configuration:\n{}",
        results[0].hooks
    );
    assert!(
        String::from_utf8_lossy(&results[0].report).contains("ng refs/heads/next pre-receive hook declined"),
        "the exit code of one hook declines the push:\n{}",
        String::from_utf8_lossy(&results[0].report)
    );
    // At the same time, each hook has to get everything the other gets, and a failure of one is the failure of all.
    let mut pushed = configured("config hook.jobs 3");
    pushed.hooks[0].file = None;
    let setup: Vec<&'static str> = pushed
        .setup
        .iter()
        .copied()
        .chain(["config hook.first.parallel true", "config hook.second.parallel true"])
        .collect();
    pushed.setup = setup;
    let results = compare_on(&world, "config-hooks-parallel", &pushed, &["files"])?;
    assert_eq!(
        results[0].hooks.matches("stdin: ").count(),
        3,
        "all three hooks got the command on their input:\n{}",
        results[0].hooks
    );
    Ok(())
}

#[test]
fn a_hook_that_is_killed_declines_and_one_that_ignores_its_input_doesnt_matter() -> TestResult {
    let world = World::new()?;
    let commands = [Cmd("0", "next", "refs/heads/next")];
    for event in ["pre-receive", "update"] {
        let results = compare_on(
            &world,
            &format!("killed-{event}"),
            &with_hooks(push(&commands), vec![Hook::new(event).killed()]),
            &["files"],
        )?;
        assert!(
            String::from_utf8_lossy(&results[0].report).contains("ng refs/heads/next"),
            "{event}: a hook that dies of a signal declines:\n{}",
            String::from_utf8_lossy(&results[0].report)
        );
    }
    // A hook of the configuration is run by a shell, which decides if it is reported as ended by a signal or as an exit code.
    let results = compare_on(
        &world,
        "killed-configured",
        &Push {
            hooks: vec![Hook::new("pre-receive").named("guard").in_dir("configured").killed()],
            ..with_setup(
                push(&commands),
                &[
                    "config hook.guard.event pre-receive",
                    "config hook.guard.command ./configured/guard",
                ],
            )
        },
        &["files"],
    )?;
    assert!(
        String::from_utf8_lossy(&results[0].report).contains("ng refs/heads/next"),
        "a configured hook that dies of a signal declines:\n{}",
        String::from_utf8_lossy(&results[0].report)
    );
    // The hook is gone before everything was written to it, which is no reason to fail.
    // The recording hook is left out as it checks every line of its input, which is slow with this many.
    let many = bulk_commands(800);
    let results = compare_on(
        &world,
        "ignored-input",
        &Push {
            no_recorder: true,
            ..with_hooks(push(&many), vec![Hook::new("pre-receive").ignoring_stdin()])
        },
        &["files"],
    )?;
    assert!(
        !String::from_utf8_lossy(&results[0].report).contains("ng "),
        "a hook that doesn't read its input accepts the push:\n{}",
        String::from_utf8_lossy(&results[0].report)
    );
    Ok(())
}

#[test]
fn a_lot_of_output_of_a_hook_is_relayed_in_full() -> TestResult {
    let world = World::new()?;
    let results = compare_on(
        &world,
        "noisy",
        &with_hooks(
            push(&[Cmd("0", "next", "refs/heads/next")]),
            vec![Hook::new("pre-receive").noisy(10_000)],
        ),
        &["files"],
    )?;
    assert!(
        results[0]
            .messages
            .contains("pre-receive noise on the other output 9999"),
        "Git relays all of it, which is more than a pipe holds"
    );
    Ok(())
}

#[test]
fn the_objects_of_the_push_are_found_in_a_repository_that_has_a_colon_in_its_path() -> TestResult {
    let world = World::new()?;
    let pushed = with_hooks(
        push(&[Cmd("0", "next", "refs/heads/next")]),
        vec![Hook::new("pre-receive")],
    );
    // The name of the directory ends up in the alternates, where a colon separates paths, and has to be quoted.
    let log = git_log(&world, "colon:in:path", &pushed, &["files"])?;
    assert!(
        log.contains("visible: commit"),
        "Git finds the objects of the quarantine, and the receiver is compared to it:\n{log}"
    );
    Ok(())
}

#[test]
fn push_options_reach_the_hooks_that_get_them() -> TestResult {
    let world = World::new()?;
    let pushed = Push {
        options: vec!["topic=a b", "flag"],
        ..with_hooks(push(&[Cmd("0", "next", "refs/heads/next")]), all_hooks())
    };
    let log = git_log(&world, "push-options", &pushed, &["files", "reftable"])?;
    assert_eq!(
        log.matches("GIT_PUSH_OPTION_COUNT=2\nGIT_PUSH_OPTION_0=topic=a b\nGIT_PUSH_OPTION_1=flag\n")
            .count(),
        2,
        "only pre-receive and post-receive get the options:\n{log}"
    );
    Ok(())
}

#[test]
fn a_deletion_has_no_objects_and_so_no_quarantine() -> TestResult {
    let world = World::new()?;
    let pushed = with_hooks(
        with_setup(
            push(&[Cmd("side", "0", "refs/heads/side")]),
            &["config receive.denyDeletes false"],
        ),
        all_hooks(),
    );
    let log = git_log(&world, "hooks-delete-only", &pushed, &["files", "reftable"])?;
    assert!(
        log[at(&log, "== pre-receive")..at(&log, "== update")].contains("quarantine=unset"),
        "without objects pre-receive has no quarantine:\n{log}"
    );
    Ok(())
}

#[test]
fn pre_receive_declines_everything_and_the_hooks_after_it_do_not_run() -> TestResult {
    let world = World::new()?;
    let pushed = with_hooks(
        push(&[Cmd("0", "next", "refs/heads/next"), Cmd("side", "0", "refs/heads/side")]),
        vec![
            Hook::new("pre-receive").exiting(1),
            Hook::new("update"),
            Hook::new("post-receive"),
            Hook::new("post-update"),
        ],
    );
    let log = git_log(&world, "pre-receive-declines", &pushed, &["files", "reftable"])?;
    assert!(
        log.contains("== pre-receive") && !log.contains("== update") && !log.contains("== post-"),
        "only pre-receive ran:\n{log}"
    );
    Ok(())
}

#[test]
fn pre_receive_is_told_of_commands_that_failed_already_but_not_after_an_unpack_error() -> TestResult {
    let world = World::new()?;
    let hooks = || vec![Hook::new("pre-receive"), Hook::new("post-receive")];
    // One of two commands lacks its objects, and the hook still hears about both.
    let log = git_log(
        &world,
        "pre-receive-after-connectivity",
        &with_hooks(
            Push {
                pack_of: Some(vec!["next"]),
                ..push(&[
                    Cmd("0", "next", "refs/heads/next"),
                    Cmd("0", "diverge", "refs/heads/diverge"),
                ])
            },
            hooks(),
        ),
        &["files", "reftable"],
    )?;
    assert_eq!(
        log.matches("stdin: ").count(),
        3,
        "pre-receive gets both commands, and post-receive the one that worked:\n{log}"
    );
    // Everything is hidden or lacks objects, and the hook is called all the same, but nothing follows.
    let log = git_log(
        &world,
        "pre-receive-all-failed",
        &with_hooks(
            with_setup(
                push(&[Cmd("side", "next", "refs/heads/side")]),
                &["config receive.hideRefs refs/heads/side"],
            ),
            hooks(),
        ),
        &["files", "reftable"],
    )?;
    assert!(
        log.contains("== pre-receive") && !log.contains("== post-receive"),
        "pre-receive is called even though everything failed, and post-receive isn't:\n{log}"
    );
    // An unpack error ends it all before any hook.
    let log = git_log(
        &world,
        "no-hook-after-unpack-error",
        &with_hooks(
            Push {
                no_pack: true,
                ..push(&[Cmd("0", "next", "refs/heads/next")])
            },
            hooks(),
        ),
        &["files", "reftable"],
    )?;
    assert!(log.is_empty(), "no hook ran after an unpack error:\n{log}");
    Ok(())
}

#[test]
fn a_declining_update_hook_stops_its_reference_and_atomic_pushes() -> TestResult {
    let world = World::new()?;
    let commands = [
        Cmd("0", "next", "refs/heads/next"),
        Cmd("side", "next", "refs/heads/side"),
        Cmd("0", "diverge", "refs/heads/diverge"),
    ];
    let hooks = vec![
        Hook::new("update").declining("refs/heads/side"),
        Hook::new("post-update"),
    ];
    let log = git_log(
        &world,
        "update-declines",
        &with_hooks(push(&commands), hooks.clone()),
        &["files", "reftable"],
    )?;
    assert!(
        log.contains("== update refs/heads/diverge")
            && log.contains("== post-update refs/heads/next refs/heads/diverge\n"),
        "the others went on and post-update was told of them only:\n{log}"
    );
    let log = git_log(
        &world,
        "update-declines-atomic",
        &Push {
            atomic: true,
            ..with_hooks(push(&commands), hooks)
        },
        &["files", "reftable"],
    )?;
    assert!(
        !log.contains("== update refs/heads/diverge") && !log.contains("== post-update"),
        "an atomic push stops at the first that is declined, so the later ones are not asked:\n{log}"
    );
    Ok(())
}

#[test]
fn post_hooks_cannot_change_the_outcome_and_are_skipped_if_nothing_was_updated() -> TestResult {
    let world = World::new()?;
    let log = git_log(
        &world,
        "post-hooks-fail",
        &with_hooks(
            push(&[Cmd("0", "next", "refs/heads/next")]),
            vec![
                Hook::new("post-receive").exiting(1),
                Hook::new("post-update").exiting(2),
            ],
        ),
        &["files", "reftable"],
    )?;
    assert!(
        log.contains("== post-receive") && log.contains("== post-update refs/heads/next"),
        "both hooks ran and the push still worked:\n{log}"
    );
    // Only a command that did not exist when it was deleted is "updated" for no hook.
    let log = git_log(
        &world,
        "post-hooks-for-missing-reference",
        &with_hooks(
            push(&[Cmd("main", "0", "refs/heads/nope")]),
            vec![Hook::new("post-receive"), Hook::new("post-update")],
        ),
        &["files", "reftable"],
    )?;
    assert!(
        log.is_empty(),
        "a deletion of what wasn't there doesn't call the post hooks:\n{log}"
    );
    assert_same(
        &world,
        "denied-and-allowed",
        &with_hooks(
            with_setup(
                push(&[
                    Cmd("main", "diverge", "refs/heads/main"),
                    Cmd("0", "next", "refs/heads/next"),
                ]),
                &["config receive.denyNonFastForwards true"],
            ),
            vec![Hook::new("post-receive"), Hook::new("post-update")],
        ),
    )
}

/// Receive with Git and the receiver at the same time, as waiting for slow hooks is what takes the time here.
fn keep_alive_results(
    world: &World,
    label: &str,
    pushed: &Push,
) -> TestResult<(super::receive_pack::Result_, super::receive_pack::Result_)> {
    use super::receive_pack::{fork_receive, git_receive};
    let format = "files";
    let request = world.request(format, pushed)?;
    let git_remote = world.remote(format, &format!("git-{label}"), pushed)?;
    let fork_remote = world.remote(format, &format!("fork-{label}"), pushed)?;
    std::thread::scope(|scope| {
        let git = scope.spawn(|| git_receive(&git_remote, &request, true));
        let fork = fork_receive(&fork_remote, &request, true)?;
        Ok((fork, git.join().expect("no panic")?))
    })
}

/// A client that waits for a slow hook is told every `receive.keepAlive` seconds that the receiver is still working, with an
/// empty packet on the data band, which Git does while hooks run before and after the report. How many there are depends on
/// timing, so what is compared is that there are about as many.
#[test]
fn slow_hooks_keep_the_client_waiting_with_keep_alive_packets() -> TestResult {
    let world = World::new()?;
    let pushed = with_hooks(
        with_setup(
            push(&[Cmd("0", "next", "refs/heads/next")]),
            &["config receive.keepAlive 1"],
        ),
        vec![
            Hook::new("pre-receive").sleeping(2.2),
            Hook::new("post-receive").sleeping(1.2),
        ],
    );
    let (fork, git) = keep_alive_results(&world, "keep-alive", &pushed)?;
    // pre-receive takes 2.2 s and post-receive 1.2 s, so there are at least 2 + 1 beats.
    assert!(git.keepalives >= 3, "Git keeps the client waiting: {}", git.keepalives);
    assert!(
        fork.keepalives >= 3,
        "the receiver keeps the client waiting as well: {}",
        fork.keepalives
    );
    assert!(
        git.keepalives.abs_diff(fork.keepalives) <= 1,
        "about as often as Git: {} vs {}",
        fork.keepalives,
        git.keepalives
    );
    assert_eq!(fork.hooks, git.hooks, "the same hooks ran");
    Ok(())
}

#[test]
fn a_keep_alive_of_zero_turns_them_off() -> TestResult {
    let world = World::new()?;
    let pushed = with_hooks(
        with_setup(
            push(&[Cmd("0", "next", "refs/heads/next")]),
            &["config receive.keepAlive 0"],
        ),
        vec![Hook::new("pre-receive").sleeping(1.2)],
    );
    let (fork, git) = keep_alive_results(&world, "keep-alive-off", &pushed)?;
    assert_eq!(
        (fork.keepalives, git.keepalives),
        (0, 0),
        "nobody tells the client to wait"
    );
    assert_eq!(fork.hooks, git.hooks, "the same hooks ran");
    Ok(())
}
