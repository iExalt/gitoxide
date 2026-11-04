//! Behaviour of the runner that doesn't need Git as reference, as it concerns I/O mechanics.
#![cfg(unix)]
use std::{os::unix::fs::PermissionsExt, path::PathBuf};

use gix_hook::{Hooks, Prepare, Stderr, Stdin};

use crate::Result;

fn hooks_with(script: &str) -> Result<(gix_testtools::tempfile::TempDir, Hooks)> {
    let dir = gix_testtools::tempfile::tempdir()?;
    let path: PathBuf = dir.path().join("pre-push");
    std::fs::write(&path, script)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    let hooks = Hooks::resolve("pre-push", dir.path(), None);
    Ok((dir, hooks))
}

#[test]
fn large_input_and_output_do_not_deadlock() -> Result {
    let (_dir, hooks) = hooks_with("#!/bin/sh\nhead -c 1000000 /dev/zero >&2\ncat >/dev/null\n")?;
    let outcome = Prepare::new(hooks)
        .stdin(Stdin::Bytes(vec![b'x'; 4_000_000]))
        .stderr(Stderr::Capture)
        .run()?;
    assert!(outcome.is_success());
    assert_eq!(
        outcome.hooks[0].output.len(),
        1_000_000,
        "all output was captured while input was fed"
    );
    Ok(())
}

#[test]
fn a_hook_that_ignores_its_input_decides_by_exit_code_alone() -> Result {
    let (_dir, hooks) = hooks_with("#!/bin/sh\nexit 3\n")?;
    let outcome = Prepare::new(hooks).stdin(Stdin::Bytes(vec![0; 4_000_000])).run()?;
    assert_eq!(outcome.exit_code(), 3);
    assert!(outcome.hooks[0].error.is_none(), "a broken pipe isn't an error");
    assert!(outcome.into_veto().is_err());
    Ok(())
}

#[test]
fn feed_errors_other_than_broken_pipe_surface() -> Result {
    let (_dir, hooks) = hooks_with("#!/bin/sh\ncat >/dev/null\n")?;
    let outcome = Prepare::new(hooks)
        .stdin(Stdin::Feed(std::sync::Arc::new(|_| Err(std::io::Error::other("boom")))))
        .run()?;
    assert!(outcome.hooks[0].error.is_some());
    assert_eq!(outcome.exit_code(), 1, "a failed feed fails the hook");
    Ok(())
}

#[test]
fn stdout_is_merged_into_stderr_by_default() -> Result {
    let (_dir, hooks) = hooks_with("#!/bin/sh\necho out\necho err >&2\n")?;
    let outcome = Prepare::new(hooks).stderr(Stderr::Capture).run()?;
    let output = String::from_utf8(outcome.hooks[0].output.clone())?;
    assert_eq!(
        output, "out\nerr\n",
        "both streams end up in order in the captured stream"
    );
    Ok(())
}

#[test]
fn scripts_without_shebang_are_run_by_the_shell() -> Result {
    let (_dir, hooks) = hooks_with("echo ran >&2\n")?;
    let outcome = Prepare::new(hooks).stderr(Stderr::Capture).run()?;
    assert_eq!(outcome.hooks[0].output, b"ran\n");
    Ok(())
}

#[test]
fn a_directory_in_place_of_a_hook_fails_to_start() -> Result {
    let dir = gix_testtools::tempfile::tempdir()?;
    std::fs::create_dir(dir.path().join("pre-push"))?;
    let outcome = Prepare::new(Hooks::resolve("pre-push", dir.path(), None)).run()?;
    assert_eq!(
        outcome.exit_code(),
        1,
        "like in Git, passing the access check isn't enough"
    );
    Ok(())
}

#[test]
fn scripts_without_shebang_work_in_directories_with_spaces_and_without_arguments() -> Result {
    let dir = gix_testtools::tempfile::tempdir()?;
    let hooks_dir = dir.path().join("hook dir");
    std::fs::create_dir(&hooks_dir)?;
    let path = hooks_dir.join("pre-commit");
    std::fs::write(&path, "echo ran >&2\n")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    let outcome = Prepare::new(Hooks::resolve("pre-commit", &hooks_dir, None))
        .stderr(Stderr::Capture)
        .run()?;
    assert_eq!(outcome.hooks[0].output, b"ran\n");
    Ok(())
}

#[test]
fn signals_map_to_128_plus_the_signal_number() -> Result {
    let (_dir, hooks) = hooks_with("#!/bin/sh\nkill -TERM $$\n")?;
    assert_eq!(Prepare::new(hooks).run()?.exit_code(), 143);
    Ok(())
}

#[test]
fn standard_input_can_come_from_a_file() -> Result {
    let (dir, hooks) = hooks_with("#!/bin/sh\ncat >&2\n")?;
    let input = dir.path().join("input");
    std::fs::write(&input, "from file")?;
    let outcome = Prepare::new(hooks)
        .stdin(Stdin::File(input))
        .stderr(Stderr::Capture)
        .run()?;
    assert_eq!(outcome.hooks[0].output, b"from file");
    Ok(())
}
