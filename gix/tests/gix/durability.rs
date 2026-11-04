//! Check that `core.fsync` reaches the object database of a repository, by observing the flushes of a process with
//! `strace` on Linux. The comparison with Git is made where the object store is, in `gix-odb`.
#![cfg(target_os = "linux")]
use gix_testtools::{
    TestResult,
    strace::{self, Event},
};

const REPO: &str = "GIX_STRACE_REPO";
const CONFIG: &str = "GIX_STRACE_CONFIG";

/// Write a blob like `gix` does for a repository with the configuration given in the environment.
/// Doesn't do anything in a regular run.
#[test]
fn child_writes_a_blob() -> TestResult {
    let Some(path) = std::env::var_os(REPO) else {
        return Ok(());
    };
    let mut options = crate::restricted();
    if let Some(config) = std::env::var_os(CONFIG) {
        options = options.config_overrides([config.to_string_lossy().into_owned()]);
    }
    let repo = gix::open_opts(path, options)?;
    repo.write_blob("flush me\n")?;
    Ok(())
}

fn flushes_of_loose_objects(config: Option<&str>) -> TestResult<Option<Vec<String>>> {
    if !strace::available_or_skip(module_path!()) {
        return Ok(None);
    }
    let dir = gix_testtools::tempfile::tempdir()?;
    gix_testtools::git(dir.path(), "init -q")?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    let test = module_path!().split_once("::").expect("crate prefix").1;
    cmd.args([
        "--exact",
        &format!("{test}::child_writes_a_blob"),
        "--nocapture",
        "--test-threads=1",
    ])
    .current_dir(dir.path())
    .env(REPO, dir.path());
    if let Some(config) = config {
        cmd.env(CONFIG, config);
    }
    let trace = strace::trace(&cmd, dir.path(), dir.path(), None, None)?;
    assert!(trace.status.success(), "the child wrote the blob");
    assert!(
        trace
            .observations
            .iter()
            .any(|o| o.ok
                && matches!(&o.event, Event::Rename(_, to) | Event::Link(_, to) if to.contains(".git/objects/"))),
        "an object was made visible, which makes the absence of flushes meaningful"
    );
    Ok(Some(
        trace
            .observations
            .iter()
            .filter(|o| o.ok)
            .filter_map(|o| match &o.event {
                Event::Flush(path) | Event::Writeout(path) if path.contains(".git/objects/") => Some(path.clone()),
                _ => None,
            })
            .collect(),
    ))
}

#[test]
fn loose_objects_are_flushed_if_core_fsync_asks_for_it() -> TestResult {
    let Some(without) = flushes_of_loose_objects(None)? else {
        return Ok(());
    };
    assert!(
        without.is_empty(),
        "nothing is flushed by default, like in Git: {without:?}"
    );

    let with = flushes_of_loose_objects(Some("core.fsync=loose-object"))?.expect("available");
    assert_eq!(with.len(), 1, "the object is flushed once: {with:?}");

    let object_files = flushes_of_loose_objects(Some("core.fsyncObjectFiles=true"))?.expect("available");
    assert_eq!(
        object_files.len(),
        1,
        "the deprecated setting is honored: {object_files:?}"
    );

    let other = flushes_of_loose_objects(Some("core.fsync=reference"))?.expect("available");
    assert!(other.is_empty(), "other components don't flush objects: {other:?}");
    Ok(())
}
