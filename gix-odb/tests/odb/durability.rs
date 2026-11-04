//! Compare the flushes that Git and the loose object store perform when writing an object, with `strace` on Linux.
//!
//! Git writes a temporary file next to the object, flushes it if configured, and links it into place, where this store
//! renames it. The two are compared as the abstract sequence *temporary file written*, *flushed*, *made visible under its
//! final name*. Every test skips itself if `strace` can't trace here, unless `GIX_TEST_REQUIRE_STRACE` is set.
//!
//! Under `core.fsyncMethod=batch` with `loose-object`, Git only batches flushes inside a multi-object operation, which this
//! store doesn't have, so a single object is flushed fully by both.
//! A full flush and a write-out are one event in these sequences: which of the two `core.fsyncMethod` selects is tested
//! with the flush counts in `gix-fs`.
#![cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

use gix_testtools::{
    TestResult,
    strace::{self, Event, Trace},
};

const REPO: &str = "GIX_ODB_STRACE_REPO";
const FSYNC: &str = "GIX_ODB_STRACE_FSYNC";
const METHOD: &str = "GIX_ODB_STRACE_METHOD";
const OBJECT_FILES: &str = "GIX_ODB_STRACE_OBJECT_FILES";
const CONTENT: &[u8] = b"loose object to flush\n";

/// What the store does in the re-executed child. Doesn't do anything in a regular run.
#[test]
fn fork_child() -> TestResult {
    let Some(repo) = std::env::var_os(REPO) else {
        return Ok(());
    };
    let mut options = gix_fs::fsync::Options::default();
    if let Some(components) = std::env::var_os(FSYNC) {
        options.components = gix_fs::fsync::Components::parse(components.to_string_lossy().as_ref().into()).0;
    }
    if let Some(method) = std::env::var_os(METHOD) {
        options.method =
            gix_fs::fsync::Method::parse(method.to_string_lossy().as_ref().into()).unwrap_or(options.method);
    }
    options.fsync_object_files = std::env::var_os(OBJECT_FILES).is_some_and(|value| value == "true");
    let store = gix_odb::loose::Store::at_opts(
        PathBuf::from(repo).join(".git/objects"),
        gix_testtools::object_hash(),
        gix_odb::loose::Options {
            fsync: options,
            ..Default::default()
        },
    );
    use gix_object::Write;
    store.write_buf(gix_object::Kind::Blob, CONTENT)?;
    Ok(())
}

/// Reduce a trace to what happened to the object, ignoring how files were made visible.
fn abstract_sequence(trace: &Trace) -> Vec<&'static str> {
    let is_final = |path: &str| {
        let mut parts = path.rsplit('/');
        let (name, dir) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        dir.len() == 2 && name.chars().all(|c| c.is_ascii_hexdigit()) && !name.is_empty()
    };
    let mut out = Vec::new();
    for observation in trace.observations.iter().filter(|o| o.ok) {
        let token = match &observation.event {
            Event::OpenForWriting(path) if path.contains("objects/") && !is_final(path) => "write temporary file",
            Event::Flush(path) if path.contains("objects/") => "flush",
            Event::Writeout(path) if path.contains("objects/") => "write out",
            Event::Link(_, to) | Event::Rename(_, to) if is_final(to) => "make visible",
            _ => continue,
        };
        // Git writes the file in pieces, which shows as repeated opens only if it reopens, which it doesn't.
        if out.last() != Some(&token) || token == "flush" {
            out.push(token);
        }
    }
    out
}

fn git_side(repo: &Path, config: &[String], file: &Path) -> TestResult<Trace> {
    let mut cmd = gix_testtools::git_command(repo);
    for config in config {
        cmd.args(["-c", config]);
    }
    cmd.args(["hash-object", "-w"]).arg(file);
    Ok(strace::trace(&cmd, repo, repo, None, None)?)
}

fn fork_side(repo: &Path, env: &[(&str, &str)]) -> TestResult<Trace> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    let test = module_path!().split_once("::").expect("crate prefix").1;
    cmd.args([
        "--exact",
        &format!("{test}::fork_child"),
        "--nocapture",
        "--test-threads=1",
    ])
    .current_dir(repo)
    .env(REPO, repo);
    for (key, value) in env {
        cmd.env(key, value);
    }
    Ok(strace::trace(&cmd, repo, repo, None, None)?)
}

#[test]
fn flushes_before_the_object_becomes_visible_as_configured_like_git() -> TestResult {
    if !strace::available_or_skip(module_path!()) {
        return Ok(());
    }
    // (core.fsync, core.fsyncMethod, core.fsyncObjectFiles)
    let configs: &[(Option<&str>, Option<&str>, Option<&str>)] = &[
        (None, None, None),
        (Some("none"), None, None),
        (Some("loose-object"), None, None),
        (Some("all"), None, None),
        (Some("reference"), None, None),
        (Some("-loose-object,all"), None, None),
        (Some("loose-object"), Some("writeout-only"), None),
        (Some("loose-object"), Some("batch"), None),
        (None, None, Some("true")),
        (Some("none"), None, Some("true")),
    ];
    let root = gix_testtools::tempfile::tempdir()?;
    let mut flushed = Vec::new();
    for (idx, (fsync, method, object_files)) in configs.iter().copied().enumerate() {
        let context = format!("{fsync:?} {method:?} {object_files:?}");
        let mut config = Vec::new();
        let mut env = Vec::new();
        if let Some(fsync) = fsync {
            config.push(format!("core.fsync={fsync}"));
            env.push((FSYNC, fsync));
        }
        if let Some(method) = method {
            config.push(format!("core.fsyncMethod={method}"));
            env.push((METHOD, method));
        }
        if let Some(files) = object_files {
            config.push(format!("core.fsyncObjectFiles={files}"));
            env.push((OBJECT_FILES, files));
        }

        let git_repo = root.path().join(format!("git-{idx}"));
        let fork_repo = root.path().join(format!("fork-{idx}"));
        for repo in [&git_repo, &fork_repo] {
            std::fs::create_dir_all(repo)?;
            gix_testtools::git(repo, "init -q")?;
        }
        let file = root.path().join(format!("content-{idx}"));
        std::fs::write(&file, CONTENT)?;

        let git = git_side(&git_repo, &config, &file)?;
        let fork = fork_side(&fork_repo, &env)?;
        assert!(
            git.status.success() && fork.status.success(),
            "{context}: both write the object"
        );
        let git_sequence = abstract_sequence(&git);
        assert!(
            git_sequence.contains(&"make visible"),
            "{context}: the object was recognized as published, so that the trace isn't empty: {git_sequence:?}"
        );
        assert_eq!(
            abstract_sequence(&fork),
            git_sequence,
            "{context}: flushed in the same way, before the object has its name"
        );
        flushed.push(git_sequence.iter().any(|token| matches!(*token, "flush" | "write out")));
    }
    assert!(
        flushed.contains(&true) && flushed.contains(&false),
        "the configurations include some that make Git flush and some that don't: {flushed:?}"
    );
    Ok(())
}
