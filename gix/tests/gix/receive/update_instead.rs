//! `receive.denyCurrentBranch=updateInstead`: a push to the branch that a work tree has checked out brings the work tree and the
//! index along, as `git receive-pack` does it with `update-index`, `diff-files`, `diff-index` and `read-tree -u -m`. Both receive
//! the same pushes, and what they report, say, update and leave in the index and the files has to be the same.
//!
//! `make_update_instead_repos.sh` says what the branches of the client are, and what the work trees are. Each row pushes one of
//! them to `main` of `work`, after something was done to the work tree that makes it special, like a changed file or one that
//! is in the way. The preparation is the only thing that differs between rows, so what it does is named for the row.
use std::{io::Write as _, path::Path};

use gix_testtools::TestResult;

use super::receive_pack::{Cmd, Hook, Push, World, compare_on};

const FORMATS: &[&str] = &["files", "reftable"];

// What Git tells the client, which each row names to show that it exercises what it is about.
const OK: &str = "ok refs/heads/main";
const UNSTAGED: &str = "ng refs/heads/main Working directory has unstaged changes";
const STAGED: &str = "ng refs/heads/main Working directory has staged changes";
const UP_TO_DATE_FAILED: &str = "ng refs/heads/main Up-to-date check failed";
const OK_LINKED: &str = "ok refs/heads/linked";
const UPDATE_FAILED: &str = "ng refs/heads/main Could not update working tree to new HEAD";

fn world() -> TestResult<World> {
    World::from_fixture("make_update_instead_repos.sh")
}

/// A push of `branch` of the client to `main` of `work`, where `main` is checked out.
fn onto_main(branch: &'static str) -> Push {
    Push {
        remote: Some("work"),
        // The hook would leave its recording in the work tree, which makes it untracked files.
        no_recorder: true,
        compare_work_tree: true,
        setup: vec!["config receive.denyCurrentBranch updateInstead"],
        commands: vec![Cmd("main", branch, "refs/heads/main")],
        ..Default::default()
    }
}

fn prepared(mut push: Push, prepare: fn(&Path, &Path) -> TestResult) -> Push {
    push.prepare = Some(prepare);
    push
}

fn with_config(mut push: Push, setup: &'static str) -> Push {
    push.setup.push(setup);
    push
}

/// Receive `push` with Git and with the fork, and compare what they say and leave behind. Git has to tell `verdict`, so that no
/// row can pass by being refused for another reason than the one it is about.
fn same(world: &World, label: &str, push: &Push, verdict: &str) -> TestResult {
    for result in compare_on(world, label, push, FORMATS)? {
        let report = String::from_utf8_lossy(&result.report);
        assert!(
            report.contains(verdict),
            "{label}: Git is to tell {verdict:?}, but told: {report}"
        );
    }
    Ok(())
}

fn git(repo: &Path, args: &str) -> TestResult {
    gix_testtools::git(repo, args)?;
    Ok(())
}

fn write(repo: &Path, relative: &str, content: &str) -> TestResult {
    let path = repo.join(relative);
    std::fs::create_dir_all(path.parent().expect("a parent"))?;
    std::fs::write(path, content)?;
    Ok(())
}

#[cfg(unix)]
/// Every directory that a test made as the place that nothing must be written to, which `paths` of work trees refer to by name.
fn assert_nothing_written_outside(world: &World) -> TestResult {
    let mut seen = 0;
    for entry in std::fs::read_dir(world.dir.path())? {
        let path = entry?.path();
        if path.to_string_lossy().ends_with("-outside") {
            seen += 1;
            assert_eq!(
                std::fs::read_dir(&path)?.count(),
                0,
                "nothing is written through a link that leads out of the work tree: {}",
                path.display()
            );
        }
    }
    assert_eq!(
        seen, 1,
        "the place outside of the work trees exists, so the check isn't vacuous"
    );
    Ok(())
}

#[test]
fn the_work_tree_follows_what_is_pushed() -> TestResult {
    let world = world()?;
    for branch in [
        "edit",
        "delete",
        "mode",
        "file-to-dir",
        "dir-to-file",
        "link-to-file",
        "file-to-link",
        "link-to-dir",
        "add-untracked-name",
        "add-in-new-dir",
        "add-gitignore",
    ] {
        same(&world, branch, &onto_main(branch), OK)?;
    }
    // What is left alone stays, as it doesn't matter to a branch: an untracked file that nothing wants.
    same(
        &world,
        "unrelated-untracked-file",
        &prepared(onto_main("edit"), |remote, _| {
            write(remote, "untracked-and-unrelated", "mine")
        }),
        OK,
    )?;
    same(
        &world,
        "ignored-file-that-nothing-wants",
        &prepared(onto_main("edit"), |remote, _| {
            write(remote, ".git/info/exclude", "ignored-and-unrelated\n")?;
            write(remote, "ignored-and-unrelated", "mine")
        }),
        OK,
    )
}

type Prepare = fn(&Path, &Path) -> TestResult;

#[test]
fn changes_that_the_index_or_the_work_tree_have_refuse_the_update() -> TestResult {
    let world = world()?;
    let refuses: &[(&str, &str, Prepare)] = &[
        ("modified-file", UNSTAGED, |remote, _| write(remote, "file", "modified")),
        ("modified-file-that-is-pushed-too", UNSTAGED, |remote, _| {
            write(remote, "crlf.txt", "modified")
        }),
        ("modified-executable-bit", UNSTAGED, |remote, _| {
            git(remote, "update-index --chmod=+x file")
        }),
        ("deleted-file", UNSTAGED, |remote, _| {
            Ok(std::fs::remove_file(remote.join("file"))?)
        }),
        ("deleted-directory", UNSTAGED, |remote, _| {
            Ok(std::fs::remove_dir_all(remote.join("dir"))?)
        }),
        ("file-replaced-by-a-link", UNSTAGED, |remote, _| {
            std::fs::remove_file(remote.join("gone"))?;
            Ok(std::os::unix::fs::symlink("file", remote.join("gone"))?)
        }),
        ("file-replaced-by-a-directory", UNSTAGED, |remote, _| {
            std::fs::remove_file(remote.join("gone"))?;
            write(remote, "gone/inside", "x")
        }),
        ("staged-change", STAGED, |remote, _| {
            write(remote, "file", "staged")?;
            git(remote, "add file")
        }),
        ("staged-new-file", STAGED, |remote, _| {
            write(remote, "added", "staged")?;
            git(remote, "add added")
        }),
        ("staged-deletion", STAGED, |remote, _| git(remote, "rm -q gone")),
        ("intent-to-add", UNSTAGED, |remote, _| {
            write(remote, "added", "later")?;
            git(remote, "add -N added")
        }),
        ("staged-and-unstaged-both", UNSTAGED, |remote, _| {
            write(remote, "file", "staged")?;
            git(remote, "add file")?;
            write(remote, "file", "and then changed")
        }),
        ("unmerged-entry", UP_TO_DATE_FAILED, |remote, _| {
            let blob = gix_testtools::git(remote, "rev-parse HEAD:file")?;
            let blob = blob.trim();
            // Stage 0 of the path is removed, and stage 1 added, which is what a conflict leaves behind.
            let mut child = gix_testtools::git_command(remote)
                .args(["update-index", "--index-info"])
                .stdin(std::process::Stdio::piped())
                .spawn()?;
            let null = "0".repeat(blob.len());
            write!(
                child.stdin.take().expect("piped"),
                "0 {null}\tfile\n100644 {blob} 1\tfile\n"
            )?;
            assert!(child.wait()?.success(), "the index can be made to have a conflict");
            Ok(())
        }),
    ];
    for (label, verdict, prepare) in refuses {
        same(&world, label, &prepared(onto_main("edit"), *prepare), verdict)?;
    }
    Ok(())
}

#[test]
fn a_file_that_was_touched_but_not_changed_is_up_to_date() -> TestResult {
    let world = world()?;
    same(
        &world,
        "touched",
        &prepared(onto_main("edit"), |remote, _| {
            let file = std::fs::File::options().write(true).open(remote.join("file"))?;
            file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))?;
            Ok(())
        }),
        OK,
    )?;
    same(
        &world,
        "rewritten-with-the-same-content",
        &prepared(onto_main("edit"), |remote, _| write(remote, "dir/inner", "inner\n")),
        OK,
    )
}

#[test]
fn untracked_files_in_the_way_refuse_the_update_unless_they_are_ignored_or_empty_directories() -> TestResult {
    let world = world()?;
    let in_the_way: &[(&str, &str, &'static str, Prepare)] = &[
        ("untracked-file", UPDATE_FAILED, "add-untracked-name", |remote, _| {
            write(remote, "untracked-name", "mine")
        }),
        ("ignored-file", OK, "add-untracked-name", |remote, _| {
            write(remote, ".git/info/exclude", "untracked-name\n")?;
            write(remote, "untracked-name", "mine")
        }),
        ("empty-directory", OK, "add-untracked-name", |remote, _| {
            Ok(std::fs::create_dir(remote.join("untracked-name"))?)
        }),
        (
            "directory-with-an-untracked-file",
            UPDATE_FAILED,
            "add-untracked-name",
            |remote, _| write(remote, "untracked-name/inside", "mine"),
        ),
        (
            "directory-that-becomes-a-file-with-an-untracked-file-in-it",
            UPDATE_FAILED,
            "dir-to-file",
            |remote, _| write(remote, "tracked-dir/untracked", "mine"),
        ),
        (
            "directory-that-becomes-a-file-with-an-ignored-file-in-it",
            OK,
            "dir-to-file",
            |remote, _| {
                write(remote, ".git/info/exclude", "untracked\n")?;
                write(remote, "tracked-dir/untracked", "mine")
            },
        ),
        (
            "directory-that-becomes-a-file-with-an-empty-directory-in-it",
            OK,
            "dir-to-file",
            |remote, _| Ok(std::fs::create_dir(remote.join("tracked-dir/empty"))?),
        ),
        (
            "file-where-a-directory-is-to-be",
            UPDATE_FAILED,
            "add-in-new-dir",
            |remote, _| write(remote, "outside-link", "mine"),
        ),
        (
            "ignored-file-where-a-directory-is-to-be",
            OK,
            "add-in-new-dir",
            |remote, _| {
                write(remote, ".git/info/exclude", "outside-link\n")?;
                write(remote, "outside-link", "mine")
            },
        ),
        (
            "untracked-file-in-the-way-of-a-file-in-a-new-directory",
            UPDATE_FAILED,
            "edit",
            |remote, _| write(remote, "newdir", "a file where a directory is to be"),
        ),
    ];
    for (label, verdict, branch, prepare) in in_the_way {
        same(&world, label, &prepared(onto_main(branch), *prepare), verdict)?;
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn nothing_is_written_through_a_link_that_leads_out_of_the_work_tree() -> TestResult {
    let world = world()?;
    // The link is untracked, where the push is to create a directory, and nothing may be created below it.
    same(
        &world,
        "untracked-link-in-the-way-of-a-directory",
        &prepared(onto_main("add-in-new-dir"), |remote, _| {
            let outside = remote.parent().expect("the world directory").join("shared-outside");
            std::fs::create_dir_all(&outside)?;
            Ok(std::os::unix::fs::symlink(&outside, remote.join("outside-link"))?)
        }),
        UPDATE_FAILED,
    )?;
    // The same link is ignored, which makes it replaceable, but the directory that replaces it is not where the link led.
    same(
        &world,
        "ignored-link-in-the-way-of-a-directory",
        &prepared(onto_main("add-in-new-dir"), |remote, _| {
            let outside = remote.parent().expect("the world directory").join("shared-outside");
            std::fs::create_dir_all(&outside)?;
            write(remote, ".git/info/exclude", "outside-link\n")?;
            Ok(std::os::unix::fs::symlink(&outside, remote.join("outside-link"))?)
        }),
        OK,
    )?;
    // The link is tracked and goes away, as the push makes a directory of it, which then holds a file.
    same(
        &world,
        "tracked-link-becomes-a-directory",
        &prepared(onto_main("link-to-dir"), |remote, _| {
            std::fs::create_dir_all(remote.parent().expect("the world directory").join("shared-outside"))?;
            Ok(())
        }),
        OK,
    )?;
    assert_nothing_written_outside(&world)
}

#[test]
fn the_update_hook_decides_before_the_work_tree_is_touched() -> TestResult {
    let world = world()?;
    same(
        &world,
        "update-hook-declines",
        &Push {
            hooks: vec![
                Hook::new("update")
                    .in_dir(".git/hooks")
                    .declining("refs/heads/main")
                    .silent(),
            ],
            ..onto_main("edit")
        },
        "ng refs/heads/main hook declined",
    )?;
    same(
        &world,
        "update-hook-agrees",
        &Push {
            hooks: vec![Hook::new("update").in_dir(".git/hooks").silent()],
            ..onto_main("edit")
        },
        OK,
    )
}

#[test]
fn a_lock_on_the_index_fails_the_up_to_date_check() -> TestResult {
    let world = world()?;
    same(
        &world,
        "index-is-locked",
        &prepared(onto_main("edit"), |remote, _| write(remote, ".git/index.lock", "")),
        UP_TO_DATE_FAILED,
    )
}

#[test]
fn a_bare_repository_has_no_work_tree_and_accepts_the_update() -> TestResult {
    let world = world()?;
    same(
        &world,
        "bare",
        &Push {
            remote: None,
            ..onto_main("edit")
        },
        OK,
    )
}

#[test]
fn other_branches_deletions_and_atomic_pushes() -> TestResult {
    let world = world()?;
    // A branch that isn't checked out doesn't touch the work tree.
    same(
        &world,
        "other-branch",
        &Push {
            commands: vec![Cmd("0", "edit", "refs/heads/other")],
            ..onto_main("edit")
        },
        "ok refs/heads/other",
    )?;
    // Deleting the current branch is refused unless allowed, and then it asks for the work tree to be updated to nothing.
    same(
        &world,
        "delete-current-refused",
        &Push {
            commands: vec![Cmd("main", "0", "refs/heads/main")],
            ..onto_main("edit")
        },
        "ng refs/heads/main deletion of the current branch prohibited",
    )?;
    same(
        &world,
        "delete-current-allowed",
        &with_config(
            Push {
                commands: vec![Cmd("main", "0", "refs/heads/main")],
                ..onto_main("edit")
            },
            "config receive.denyDeleteCurrent ignore",
        ),
        UPDATE_FAILED,
    )?;
    // The work tree is updated when its command is processed, so a failure of a later command leaves it updated.
    let failing_later = |atomic| Push {
        atomic,
        pack_of: Some(vec!["edit"]),
        commands: vec![
            Cmd("main", "edit", "refs/heads/main"),
            Cmd("main", "0", "refs/heads/not-there"),
        ],
        ..onto_main("edit")
    };
    same(
        &world,
        "atomic-failure-after-the-work-tree",
        &failing_later(true),
        "ng refs/heads/not-there atomic transaction failed",
    )?;
    same(
        &world,
        "non-atomic-failure-of-another-command",
        &failing_later(false),
        "ng refs/heads/not-there reference does not exist",
    )
}

#[test]
fn a_branch_that_does_not_exist_yet_in_a_work_tree_without_commits() -> TestResult {
    let world = world()?;
    let unborn = || Push {
        remote: Some("work-unborn"),
        commands: vec![Cmd("0", "main", "refs/heads/main")],
        ..onto_main("edit")
    };
    same(&world, "unborn", &unborn(), OK)?;
    same(
        &world,
        "unborn-with-a-file-in-the-way",
        &prepared(unborn(), |remote, _| write(remote, "file", "mine")),
        UPDATE_FAILED,
    )?;
    same(
        &world,
        "unborn-with-a-staged-file",
        &prepared(unborn(), |remote, _| {
            write(remote, "staged", "mine")?;
            git(remote, "add staged")
        }),
        STAGED,
    )
}

#[test]
fn submodules_are_directories_that_only_the_index_follows() -> TestResult {
    let world = world()?;
    same(&world, "submodule-added", &onto_main("module-base"), OK)?;
    // `main` of `work-module` is at `module-base`, so these two are fast-forwards of it.
    let from_module_base = |branch| Push {
        remote: Some("work-module"),
        commands: vec![Cmd("module-base", branch, "refs/heads/main")],
        ..onto_main(branch)
    };
    same(&world, "submodule-moved", &from_module_base("module-moved"), OK)?;
    same(&world, "submodule-removed", &from_module_base("module-removed"), OK)?;
    same(
        &world,
        "submodule-checked-out-and-removed",
        &prepared(from_module_base("module-removed"), |remote, _| {
            write(remote, "module/checked-out", "content")
        }),
        OK,
    )
}

#[test]
fn trees_that_cannot_be_checked_out_are_refused() -> TestResult {
    let world = world()?;
    for branch in [
        "evil-dotgit-file",
        "evil-dotgit-dir",
        "evil-dotgit-below",
        "evil-dotgit-case",
        "evil-dotdot",
        "evil-gitmodules-link",
        "evil-ntfs-short-name",
    ] {
        same(&world, branch, &onto_main(branch), UPDATE_FAILED)?;
    }
    Ok(())
}

#[test]
fn settings_of_the_work_tree_that_change_how_it_is_written() -> TestResult {
    let world = world()?;
    same(
        &world,
        "no-filemode",
        &with_config(onto_main("mode"), "config core.filemode false"),
        OK,
    )?;
    same(
        &world,
        "no-symlinks",
        &with_config(onto_main("file-to-link"), "config core.symlinks false"),
        OK,
    )?;
    same(
        &world,
        "no-symlinks-link-to-file",
        &with_config(onto_main("link-to-file"), "config core.symlinks false"),
        OK,
    )?;
    same(
        &world,
        "autocrlf",
        &with_config(onto_main("edit"), "config core.autocrlf true"),
        OK,
    )?;
    same(
        &world,
        "eol-lf",
        &with_config(onto_main("edit"), "config core.eol lf"),
        OK,
    )?;
    same(
        &world,
        "index-version-4",
        &prepared(onto_main("edit"), |remote, _| {
            git(remote, "update-index --index-version 4")
        }),
        OK,
    )?;
    same(
        &world,
        "skip-hash",
        &with_config(onto_main("edit"), "config index.skipHash true"),
        OK,
    )
}

#[test]
fn trees_that_make_the_index_ambiguous_or_unordered_are_refused_although_git_goes_ahead() -> TestResult {
    use super::receive_pack::{fork_receive, work_tree_state};

    let world = world()?;
    for branch in ["evil-same-name-twice", "evil-file-and-directory", "evil-unsorted"] {
        let push = onto_main(branch);
        for format in FORMATS {
            let request = world.request(format, &push)?;
            let untouched = work_tree_state(&world.remote(format, &format!("untouched-{branch}"), &push)?)?;
            let fork = fork_receive(&world.remote(format, &format!("fork-{branch}"), &push)?, &request, true)?;
            assert_eq!(
                String::from_utf8_lossy(&fork.report),
                "000eunpack ok\n0041ng refs/heads/main Could not update working tree to new HEAD\n0000",
                "{format}/{branch}: an index can't have a path twice, as a file and a directory, or out of order, so the push is declined, which Git doesn't do"
            );
            assert_eq!(
                fork.work, untouched,
                "{format}/{branch}: the index and the work tree were not touched by the refused push"
            );
        }
    }
    Ok(())
}

fn add_linked(remote: &Path, _: &Path) -> TestResult {
    let name = remote.file_name().unwrap_or_default().to_string_lossy();
    let path = remote.with_file_name(format!("{name}-linked"));
    git(remote, &format!("worktree add -q {} -b linked main", path.display()))
}

#[test]
fn the_branch_that_a_linked_work_tree_has_checked_out_is_the_one_that_is_updated() -> TestResult {
    let world = world()?;
    let in_linked_work_tree = |prepare: Prepare| Push {
        commands: vec![Cmd("main", "edit", "refs/heads/linked")],
        ..prepared(onto_main("edit"), prepare)
    };
    same(&world, "linked", &in_linked_work_tree(add_linked), OK_LINKED)?;
    same(
        &world,
        "linked-with-changes",
        &in_linked_work_tree(|remote, client| {
            add_linked(remote, client)?;
            let name = remote.file_name().unwrap_or_default().to_string_lossy();
            write(&remote.with_file_name(format!("{name}-linked")), "file", "modified")
        }),
        "ng refs/heads/linked Working directory has unstaged changes",
    )
}

/// What Git doesn't do, or doesn't look at, but the fork refuses because it can't update the work tree faithfully: the push is
/// declined with the work tree left alone.
#[test]
fn entries_that_a_checkout_would_have_to_treat_specially_are_declined_by_the_fork() -> TestResult {
    use super::receive_pack::{fork_receive, work_tree_state};

    let world = world()?;
    let declined: &[(&str, Prepare)] = &[
        ("skip-worktree", |remote, _| {
            git(remote, "update-index --skip-worktree file")
        }),
        ("assume-unchanged", |remote, _| {
            git(remote, "update-index --assume-unchanged file")
        }),
        ("sparse-checkout", |remote, _| {
            git(remote, "config core.sparseCheckout true")
        }),
    ];
    for (label, prepare) in declined {
        let push = prepared(onto_main("edit"), *prepare);
        for format in FORMATS {
            let request = world.request(format, &push)?;
            let untouched = work_tree_state(&world.remote(format, &format!("untouched-{label}"), &push)?)?;
            let remote = world.remote(format, &format!("fork-{label}"), &push)?;
            // Settings are refused when the receiver is made, which is before anything is read.
            let report = fork_receive(&remote, &request, true).map(|result| (result.report, result.work));
            match report {
                Ok((report, work)) => {
                    assert!(
                        String::from_utf8_lossy(&report)
                            .contains("ng refs/heads/main Could not update working tree to new HEAD"),
                        "{format}/{label}: the push is declined"
                    );
                    assert_eq!(work, untouched, "{format}/{label}: the work tree is left alone");
                }
                Err(err) => assert!(
                    label == &"sparse-checkout" && format!("{err:?}").contains("sparse"),
                    "{format}/{label}: only a setting that can't be honored refuses the receiver itself: {err:?}"
                ),
            }
        }
    }
    Ok(())
}

#[test]
fn a_push_through_a_symbolic_reference_updates_the_work_tree_only_in_the_receiver() -> TestResult {
    use super::receive_pack::{fork_receive, git_receive};

    let world = world()?;
    let push = prepared(
        Push {
            commands: vec![Cmd("main", "edit", "refs/heads/alias")],
            ..onto_main("edit")
        },
        |remote, _| git(remote, "symbolic-ref refs/heads/alias refs/heads/main"),
    );
    for format in FORMATS {
        let request = world.request(format, &push)?;
        let git = git_receive(&world.remote(format, "git-alias", &push)?, &request, true)?;
        let fork = fork_receive(&world.remote(format, "fork-alias", &push)?, &request, true)?;
        assert_eq!(
            fork.report, git.report,
            "{format}: both move the branch that the alias leads to, and say so"
        );
        assert_eq!(fork.refs, git.refs, "{format}: the references are the same");
        assert!(
            git.work.iter().any(|line| line.starts_with("status M")),
            "{format}: Git looks for a work tree by the name that is pushed, finds none, and leaves the index and files behind the branch: {:?}",
            git.work
        );
        assert!(
            !fork.work.iter().any(|line| line.starts_with("status ")),
            "{format}: the receiver follows the alias to the checked out branch, like its deny rules do, and the work tree is clean: {:?}",
            fork.work
        );
    }
    Ok(())
}

#[test]
fn attributes_that_the_work_tree_has_and_the_index_hasnt_apply_like_they_do_for_git() -> TestResult {
    let world = world()?;
    same(
        &world,
        "untracked-attributes",
        &prepared(onto_main("edit"), |remote, _| {
            write(remote, "newdir/.gitattributes", "new eol=crlf\n")
        }),
        OK,
    )
}

/// A `push-to-checkout` hook that records how it was called, in the Git directory of the work tree that is pushed to.
fn push_to_checkout() -> Hook {
    Hook::new("push-to-checkout").in_dir(".git/hooks").silent()
}

fn with_hooks(hooks: Vec<Hook>, push: Push) -> Push {
    Push { hooks, ..push }
}

#[test]
fn a_push_to_checkout_hook_takes_the_place_of_the_checks_and_the_checkout() -> TestResult {
    let world = world()?;
    // The hook is told the commit, in the environment that points to the work tree, and decides what happens to it, so that
    // a work tree with changes in it is no reason to refuse.
    same(
        &world,
        "hook-does-nothing-in-a-dirty-work-tree",
        &prepared(with_hooks(vec![push_to_checkout()], onto_main("edit")), |remote, _| {
            write(remote, "file", "modified")
        }),
        OK,
    )?;
    same(
        &world,
        "hook-updates-the-work-tree",
        &with_hooks(
            vec![push_to_checkout().running("git read-tree -u -m \"$1\"")],
            onto_main("edit"),
        ),
        OK,
    )?;
    same(
        &world,
        "hook-declines",
        &with_hooks(vec![push_to_checkout().exiting(1)], onto_main("edit")),
        "ng refs/heads/main push-to-checkout hook declined",
    )?;
    // A hook of a push to a branch that isn't checked out doesn't run.
    same(
        &world,
        "hook-for-another-branch",
        &Push {
            commands: vec![Cmd("0", "edit", "refs/heads/other")],
            ..with_hooks(vec![push_to_checkout().exiting(1)], onto_main("edit"))
        },
        "ok refs/heads/other",
    )?;
    // It gets the null id when the current branch is deleted, which it may be told to allow.
    same(
        &world,
        "hook-is-told-about-a-deletion",
        &with_config(
            Push {
                commands: vec![Cmd("main", "0", "refs/heads/main")],
                ..with_hooks(vec![push_to_checkout()], onto_main("edit"))
            },
            "config receive.denyDeleteCurrent ignore",
        ),
        OK,
    )?;
    same(
        &world,
        "hook-in-the-linked-work-tree",
        &prepared(
            Push {
                commands: vec![Cmd("main", "edit", "refs/heads/linked")],
                ..with_hooks(
                    vec![push_to_checkout().running("git read-tree -u -m \"$1\"")],
                    onto_main("edit"),
                )
            },
            add_linked,
        ),
        OK_LINKED,
    )
}

#[test]
fn what_comes_before_and_after_the_push_to_checkout_hook() -> TestResult {
    let world = world()?;
    // The `update` hook decides first, so the other hook doesn't run.
    same(
        &world,
        "update-hook-declines-first",
        &with_hooks(
            vec![
                Hook::new("update")
                    .in_dir(".git/hooks")
                    .declining("refs/heads/main")
                    .silent(),
                push_to_checkout().named("push-to-checkout"),
            ],
            onto_main("edit"),
        ),
        "ng refs/heads/main hook declined",
    )?;
    // The work tree is updated when the command is processed, so a failing command that follows doesn't undo that.
    same(
        &world,
        "hook-then-a-failing-atomic-command",
        &Push {
            atomic: true,
            pack_of: Some(vec!["edit"]),
            commands: vec![
                Cmd("main", "edit", "refs/heads/main"),
                Cmd("main", "0", "refs/heads/not-there"),
            ],
            ..with_hooks(
                vec![push_to_checkout().running("git read-tree -u -m \"$1\"")],
                onto_main("edit"),
            )
        },
        "ng refs/heads/not-there atomic transaction failed",
    )
}

#[test]
fn a_hook_that_cannot_be_run_is_no_hook_and_the_work_tree_is_updated_by_the_checks() -> TestResult {
    let world = world()?;
    // Git only knows that a hook ran if it ended, so one that can't start leaves it to the checks, which refuse a dirty work tree.
    let dirty: Prepare = |remote, _| write(remote, "file", "modified");
    same(
        &world,
        "interpreter-is-missing",
        &prepared(
            with_hooks(
                vec![push_to_checkout().interpreted_by("/nonexistent/sh")],
                onto_main("edit"),
            ),
            dirty,
        ),
        UNSTAGED,
    )?;
    same(
        &world,
        "hook-is-not-executable",
        &prepared(
            with_hooks(vec![push_to_checkout().with_mode(0o644)], onto_main("edit")),
            dirty,
        ),
        UNSTAGED,
    )?;
    same(
        &world,
        "interpreter-is-missing-in-a-clean-work-tree",
        &with_hooks(
            vec![push_to_checkout().interpreted_by("/nonexistent/sh")],
            onto_main("edit"),
        ),
        OK,
    )
}

#[test]
fn a_configured_hook_and_a_hook_file_both_run_and_either_one_declines() -> TestResult {
    let world = world()?;
    same(
        &world,
        "configured-hook-declines",
        &with_config(
            with_config(
                with_hooks(vec![push_to_checkout()], onto_main("edit")),
                "config hook.guard.event push-to-checkout",
            ),
            "config hook.guard.command false",
        ),
        "ng refs/heads/main push-to-checkout hook declined",
    )
}

#[test]
fn a_configured_hook_that_cannot_start_ends_the_run_and_declines_only_if_one_ran_before_it() -> TestResult {
    let world = world()?;
    same(
        &world,
        "configured-hook-cannot-start",
        &with_config(
            with_config(
                with_hooks(vec![push_to_checkout()], onto_main("edit")),
                "config hook.broken.event push-to-checkout",
            ),
            "config hook.broken.command /nonexistent/program",
        ),
        OK,
    )
}

#[test]
fn a_hook_that_cannot_start_after_one_that_ran_declines() -> TestResult {
    let world = world()?;
    // Configured hooks run in the order of their configuration, so the one that works goes first, and then the one that
    // can't start counts as a failure of the hook that took over, which leaves nothing to fall back to.
    same(
        &world,
        "configured-hook-cannot-start-after-one-that-ran",
        &with_config(
            with_config(
                with_config(
                    with_config(onto_main("edit"), "config hook.fine.event push-to-checkout"),
                    "config hook.fine.command true",
                ),
                "config hook.broken.event push-to-checkout",
            ),
            "config hook.broken.command /nonexistent/program",
        ),
        "ng refs/heads/main push-to-checkout hook declined",
    )
}
