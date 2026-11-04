//! Compare the flushes that Git's `index-pack --stdin` and the bundle writer perform for a pack they receive, with `strace` on
//! Linux: the index is flushed first, then the pack, the `.keep` file isn't flushed, and the pack becomes visible before its index.
//!
//! The comparison is made on the abstract sequence of what happened to the files, as the writers use temporary files with
//! their own names, and Git also writes a reverse index, which is disabled for it here. Every test skips itself if `strace`
//! can't trace here, unless `GIX_TEST_REQUIRE_STRACE` is set.
//! A full flush and a write-out are one event in these sequences: which of the two `core.fsyncMethod` selects is tested
//! with the flush counts in `gix-fs`.
#![cfg(target_os = "linux")]
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::AtomicBool,
};

use gix_testtools::{
    TestResult,
    strace::{self, Event, Trace},
};
use gix_utils::progress;

const REPO: &str = "GIX_PACK_STRACE_REPO";
const PACK: &str = "GIX_PACK_STRACE_PACK";
const FSYNC: &str = "GIX_PACK_STRACE_FSYNC";
const METHOD: &str = "GIX_PACK_STRACE_METHOD";

/// Write the pack in the environment with the bundle writer. Doesn't do anything in a regular run.
#[test]
fn fork_child() -> TestResult {
    let (Some(repo), Some(pack)) = (std::env::var_os(REPO), std::env::var_os(PACK)) else {
        return Ok(());
    };
    let mut options = gix_pack::bundle::write::Options::default();
    if let Some(components) = std::env::var_os(FSYNC) {
        options.fsync.components = gix_fs::fsync::Components::parse(components.to_string_lossy().as_ref().into()).0;
    }
    if let Some(method) = std::env::var_os(METHOD) {
        options.fsync.method =
            gix_fs::fsync::Method::parse(method.to_string_lossy().as_ref().into()).unwrap_or(options.fsync.method);
    }
    let mut pack = std::io::BufReader::new(std::fs::File::open(pack)?);
    gix_pack::Bundle::write_to_directory(
        &mut pack,
        Some(&PathBuf::from(repo).join(".git/objects/pack")),
        &mut progress::Discard,
        &AtomicBool::new(false),
        None::<gix_object::find::Never>,
        gix_testtools::object_hash(),
        options,
    )?;
    Ok(())
}

/// Reduce a trace to what happened to the pack and its index, ignoring how the temporary files were named.
fn abstract_sequence(trace: &Trace) -> Vec<&'static str> {
    let extension_of = |path: &str| {
        Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_owned)
    };
    let kind_of_final = |path: &str| match extension_of(path).as_deref() {
        Some("pack") => Some("pack"),
        Some("idx") => Some("index"),
        _ => None,
    };
    // What each temporary file turned out to be, which is only known when it is renamed.
    let mut renamed: HashMap<&str, &'static str> = HashMap::new();
    for observation in trace.observations.iter().filter(|o| o.ok) {
        if let Event::Rename(from, to) | Event::Link(from, to) = &observation.event
            && let Some(kind) = kind_of_final(to)
        {
            renamed.insert(from, kind);
        }
    }
    let mut out = Vec::new();
    for observation in trace.observations.iter().filter(|o| o.ok) {
        let token = match &observation.event {
            Event::Flush(path) | Event::Writeout(path) if path.contains("objects/pack/") => {
                match renamed.get(path.as_str()).copied().or_else(|| kind_of_final(path)) {
                    Some("pack") => "flush pack",
                    Some("index") => "flush index",
                    _ => "flush something else",
                }
            }
            Event::OpenForWriting(path) if extension_of(path).as_deref() == Some("keep") => "write keep file",
            Event::Rename(_, to) | Event::Link(_, to) => match kind_of_final(to) {
                Some("pack") => "make pack visible",
                Some("index") => "make index visible",
                _ => continue,
            },
            _ => continue,
        };
        out.push(token);
    }
    out
}

fn git_side(repo: &Path, config: &[String], pack: &Path) -> TestResult<Trace> {
    let mut cmd = gix_testtools::git_command(repo);
    cmd.args(["-c", "pack.writeReverseIndex=false"]);
    for config in config {
        cmd.args(["-c", config]);
    }
    cmd.args(["index-pack", "--stdin", "--keep"]);
    Ok(strace::trace(&cmd, repo, repo, None, Some(pack))?)
}

fn fork_side(repo: &Path, pack: &Path, env: &[(&str, &str)]) -> TestResult<Trace> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    let test = module_path!().split_once("::").expect("crate prefix").1;
    cmd.args([
        "--exact",
        &format!("{test}::fork_child"),
        "--nocapture",
        "--test-threads=1",
    ])
    .current_dir(repo)
    .env(REPO, repo)
    .env(PACK, pack);
    for (key, value) in env {
        cmd.env(key, value);
    }
    Ok(strace::trace(&cmd, repo, repo, None, None)?)
}

/// A pack with three blobs, created by Git.
fn pack(dir: &Path) -> TestResult<PathBuf> {
    let source = dir.join("source");
    std::fs::create_dir_all(&source)?;
    gix_testtools::git(&source, "init -q")?;
    let mut ids = String::new();
    for (name, content) in [("a", "one\n"), ("b", "two\n"), ("c", "three\n")] {
        std::fs::write(source.join(name), content)?;
        ids.push_str(gix_testtools::git(&source, &format!("hash-object -w {name}"))?.trim());
        ids.push('\n');
    }
    std::fs::write(source.join("ids"), ids)?;
    let mut cmd = gix_testtools::git_command(&source);
    let out = cmd
        .args(["pack-objects", "--stdout"])
        .stdin(std::fs::File::open(source.join("ids"))?)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;
    assert!(out.status.success(), "pack-objects succeeded");
    let path = dir.join("input.pack");
    std::fs::write(&path, out.stdout)?;
    Ok(path)
}

#[test]
fn flushes_index_then_pack_and_publishes_pack_before_index_like_git() -> TestResult {
    if !strace::available_or_skip(module_path!()) {
        return Ok(());
    }
    // (core.fsync, core.fsyncMethod)
    let configs: &[(Option<&str>, Option<&str>)] = &[
        (None, None),
        (Some("none"), None),
        (Some("pack"), None),
        (Some("pack-metadata"), None),
        (Some("-pack-metadata"), None),
        (Some("all"), None),
        (Some("-pack"), None),
        (Some("all"), Some("writeout-only")),
        (Some("all"), Some("batch")),
        (Some("reference"), None),
    ];
    let root = gix_testtools::tempfile::tempdir()?;
    let pack = pack(root.path())?;
    let mut flushes = Vec::new();
    for (idx, (fsync, method)) in configs.iter().copied().enumerate() {
        let context = format!("{fsync:?} {method:?}");
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
        let git_repo = root.path().join(format!("git-{idx}"));
        let fork_repo = root.path().join(format!("fork-{idx}"));
        for repo in [&git_repo, &fork_repo] {
            std::fs::create_dir_all(repo)?;
            gix_testtools::git(repo, "init -q")?;
        }

        let git = git_side(&git_repo, &config, &pack)?;
        let fork = fork_side(&fork_repo, &pack, &env)?;
        assert!(
            git.status.success() && fork.status.success(),
            "{context}: both write the pack"
        );
        let git_sequence = abstract_sequence(&git);
        assert!(
            git_sequence.contains(&"make pack visible") && git_sequence.contains(&"make index visible"),
            "{context}: the files were recognized, so that the trace isn't empty: {git_sequence:?}"
        );
        assert_eq!(
            abstract_sequence(&fork),
            git_sequence,
            "{context}: flushed and published in the same order"
        );
        flushes.push(git_sequence.iter().filter(|token| token.starts_with("flush")).count());
    }
    assert!(
        flushes.contains(&0) && flushes.contains(&1) && flushes.contains(&2),
        "the configurations include those that flush nothing, one file and both files: {flushes:?}"
    );
    Ok(())
}
