//! A quarantine receives packs like `git receive-pack` does, which is the oracle for what ends up in the object database.
//!
//! The scenarios send the same pack to two copies of a repository, one with Git and one with the quarantine, and
//! compare the objects, loose and packed, that the repository ends up with.
use std::{
    collections::BTreeSet,
    io::{Cursor, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::AtomicBool,
};

use gix_odb::quarantine::{Options, Quarantine, Received, Rejected};
use gix_testtools::TestResult;

use crate::odb_at;

fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path).extension().is_some_and(|ext| ext == extension)
}

/// Not configuration but a hidden option of `git receive-pack`, passed along with the configuration here.
const REJECT_THIN: &str = "reject-thin-pack-for-testing";

struct Repos {
    dir: tempfile::TempDir,
}

impl Repos {
    fn new() -> TestResult<Self> {
        Ok(Repos {
            dir: gix_testtools::scripted_fixture_writable("make_receive_repos.sh")?,
        })
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    /// Copy the repository `name` to `<name>-<suffix>` and return its path.
    fn copy(&self, name: &str, suffix: &str) -> TestResult<PathBuf> {
        let dest = self.path(&format!("{name}-{suffix}"));
        copy_dir(&self.path(name), &dest)?;
        Ok(dest)
    }
    fn rev(&self, rev: &str) -> TestResult<String> {
        Ok(gix_testtools::git(self.path("source"), &format!("rev-parse {rev}"))?
            .trim()
            .to_owned())
    }
    /// A pack with everything `rev` needs that isn't reachable from `exclude`, thin if asked for.
    fn pack(&self, rev: &str, exclude: &str, thin: bool) -> TestResult<Vec<u8>> {
        let mut cmd = gix_testtools::git_command(self.path("source"));
        cmd.args(["pack-objects", "--revs", "--stdout", "-q"]);
        if thin {
            cmd.arg("--thin");
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("piped")
            .write_all(format!("{rev}\n^{exclude}\n").as_bytes())?;
        let output = child.wait_with_output()?;
        assert!(output.status.success(), "{output:?}");
        Ok(output.stdout)
    }
}

fn copy_dir(from: &Path, to: &Path) -> TestResult {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn entries(pack: &[u8]) -> u32 {
    u32::from_be_bytes(pack[8..12].try_into().expect("4 bytes"))
}

/// What Git did, with `reason` being what it reported after `unpack`.
struct GitRun {
    reason: String,
}

/// The command to push `pack` to `repo` as `new` with `git receive-pack` and `config` as `-c` options, along with the file
/// that it reads from standard input.
fn git_receive_command(
    repo: &Path,
    config: &[&str],
    new: &str,
    pack: &[u8],
) -> TestResult<(std::process::Command, PathBuf)> {
    let zero = "0".repeat(new.len());
    let mut request = Vec::new();
    let command = format!(
        "{zero} {new} refs/heads/pushed\0report-status object-format={}",
        gix_testtools::object_hash()
    );
    request.extend(format!("{:04x}{command}", command.len() + 4).bytes());
    request.extend(b"0000");
    request.extend(pack);
    let request_path = repo.with_extension(format!("{}.request", std::process::id()));
    std::fs::write(&request_path, request)?;
    let mut cmd = gix_testtools::git_command(repo);
    for config in config.iter().filter(|config| **config != REJECT_THIN) {
        cmd.args(["-c", config]);
    }
    cmd.arg("receive-pack");
    if config.contains(&REJECT_THIN) {
        cmd.arg("--reject-thin-pack-for-testing");
    }
    cmd.arg(std::fs::canonicalize(repo)?);
    Ok((cmd, request_path))
}

/// Push `pack` to `repo` as `new` with `git receive-pack` and `config` as `-c` options.
fn git_receive(repo: &Path, config: &[&str], new: &str, pack: &[u8]) -> TestResult<GitRun> {
    let (mut cmd, request) = git_receive_command(repo, config, new, pack)?;
    let output = cmd
        .stdin(std::fs::File::open(request)?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let reason = stdout
        .split("unpack ")
        .nth(1)
        .and_then(|rest| rest.lines().next())
        .unwrap_or("")
        .to_owned();
    Ok(GitRun { reason })
}

/// Receive `pack` into `repo` with the quarantine, and migrate it if that worked.
fn quarantine_receive(
    repo: &Path,
    config: &[&str],
    pack: &[u8],
) -> TestResult<std::result::Result<Received, Rejected>> {
    quarantine_receive_with(repo, options(config)?, pack)
}

fn quarantine_receive_with(
    repo: &Path,
    options: Options,
    pack: &[u8],
) -> TestResult<std::result::Result<Received, Rejected>> {
    let mut quarantine = Quarantine::new(repo.join("objects"), options)?;
    let received = match quarantine.receive_pack(&mut Cursor::new(pack), &AtomicBool::new(false)) {
        Ok(received) => received,
        Err(rejected) => return Ok(Err(rejected)),
    };
    quarantine.migrate()?.release()?;
    Ok(Ok(received))
}

fn options(config: &[&str]) -> TestResult<Options> {
    let get = |key: &str| {
        config
            .iter()
            .rev()
            .find_map(|entry| entry.split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case(key)))
            .map(|(_, value)| value.to_owned())
    };
    let mut options = Options::from_config(gix_testtools::object_hash(), Default::default(), &get)?;
    options.reject_thin = config.contains(&REJECT_THIN);
    Ok(options)
}

/// The objects of a repository as loose objects and packs, ignoring how packs are named.
#[derive(Debug, PartialEq, Eq)]
struct State {
    loose: BTreeSet<String>,
    packs: usize,
    keeps: usize,
    leftovers: BTreeSet<String>,
    objects: BTreeSet<String>,
}

fn state(repo: &Path) -> TestResult<State> {
    let objects = repo.join("objects");
    let mut state = State {
        loose: BTreeSet::new(),
        packs: 0,
        keeps: 0,
        leftovers: BTreeSet::new(),
        objects: gix_testtools::git(repo, "cat-file --batch-all-objects --batch-check")?
            .lines()
            .map(ToOwned::to_owned)
            .collect(),
    };
    for entry in std::fs::read_dir(&objects)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() == 2 && entry.file_type()?.is_dir() {
            for object in std::fs::read_dir(entry.path())? {
                state
                    .loose
                    .insert(format!("{name}/{}", object?.file_name().to_string_lossy()));
            }
        } else if name == "pack" {
            for file in std::fs::read_dir(entry.path())? {
                let file = file?.file_name().to_string_lossy().into_owned();
                match file.rsplit('.').next() {
                    Some("pack") => state.packs += 1,
                    Some("keep") => state.keeps += 1,
                    Some("idx" | "rev") => {}
                    _ => {
                        state.leftovers.insert(format!("pack/{file}"));
                    }
                }
            }
        } else if name != "info" {
            state.leftovers.insert(name);
        }
    }
    Ok(state)
}

/// Send `pack` to copies of `repo` with Git and the quarantine and assert both did the same.
/// Returns the resulting state and whether the pack was accepted.
fn assert_same(
    repos: &Repos,
    repo: &str,
    config: &[&str],
    new: &str,
    pack: &[u8],
    what: &str,
) -> TestResult<(State, bool)> {
    let git_repo = repos.copy(repo, &format!("git-{what}"))?;
    let our_repo = repos.copy(repo, &format!("ours-{what}"))?;
    let git = git_receive(&git_repo, config, new, pack)?;
    let ours = quarantine_receive(&our_repo, config, pack)?;
    let ours_reason = match &ours {
        Ok(_) => "ok".to_owned(),
        Err(rejected) => rejected.reason.clone(),
    };
    assert_eq!(ours_reason, git.reason, "{what}: the reported reason is the one of Git");
    let (ours_state, git_state) = (state(&our_repo)?, state(&git_repo)?);
    assert_eq!(
        ours_state, git_state,
        "{what}: the repository has the same objects, as loose objects and in packs, without leftovers"
    );
    Ok((ours_state, ours.is_ok()))
}

#[test]
fn unpack_and_keep_around_the_limit_match_git() -> TestResult {
    let repos = Repos::new()?;
    let pack = repos.pack("two", "one", false)?;
    let new = repos.rev("two")?;
    let count = entries(&pack);
    for (limit, expect_kept, create_object) in [
        (count, true, "link"),
        (count + 1, false, "link"),
        (0, true, "link"),
        (1, true, "link"),
        (u32::MAX, false, "link"),
        (count, true, "rename"),
        (count + 1, false, "rename"),
    ] {
        let limit_config = format!("receive.unpackLimit={limit}");
        let create_config = format!("core.createObject={create_object}");
        let (state, accepted) = assert_same(
            &repos,
            "target.git",
            &[&limit_config, &create_config],
            &new,
            &pack,
            &format!("limit-{limit}-{create_object}"),
        )?;
        assert!(accepted, "limit {limit}: the pack is fine");
        assert_eq!(
            state.packs,
            1 + usize::from(expect_kept),
            "limit {limit} with {count} entries: the pack is kept if there are at least as many entries as the limit"
        );
        assert_eq!(state.keeps, 0, "the keep file is gone once references are in place");
    }
    Ok(())
}

#[test]
fn an_empty_pack_is_unpacked_and_writes_nothing() -> TestResult {
    let repos = Repos::new()?;
    let new = repos.rev("one")?;
    let pack = repos.pack("one", "one", false)?;
    assert_eq!(entries(&pack), 0, "an empty pack");
    let (state, _) = assert_same(&repos, "target.git", &[], &new, &pack, "empty")?;
    assert_eq!(state.packs, 1, "no pack was added to the one that is there");
    Ok(())
}

#[test]
fn unpack_limit_prefers_receive_over_transfer() -> TestResult {
    let limit = |config: &[&str]| options(config).map(|o| o.unpack_limit);
    assert_eq!(limit(&[])?, 100, "the default of Git");
    assert_eq!(limit(&["transfer.unpackLimit=7"])?, 7);
    assert_eq!(
        limit(&["transfer.unpackLimit=7", "receive.unpackLimit=3"])?,
        3,
        "receive wins"
    );
    assert_eq!(
        limit(&["receive.unpackLimit=3", "transfer.unpackLimit=7"])?,
        3,
        "irrespective of order"
    );
    assert_eq!(
        limit(&["receive.unpackLimit=0"])?,
        0,
        "zero is valid and keeps all packs"
    );
    assert_eq!(limit(&["receive.unpackLimit=1k"])?, 1024, "suffixes are known");
    assert!(
        limit(&["receive.unpackLimit=many"]).is_err(),
        "bad numbers are rejected like Git does"
    );
    Ok(())
}

#[test]
fn thin_packs_are_completed_on_both_paths() -> TestResult {
    let repos = Repos::new()?;
    let pack = repos.pack("three", "one", true)?;
    let new = repos.rev("three")?;
    let count = entries(&pack);
    for (target, limit) in [
        ("target.git", count + 1),
        ("target.git", count),
        ("thin-alternate.git", count + 1),
        ("thin-alternate.git", count),
        ("target-loose.git", count + 1),
        ("target-loose.git", count),
    ] {
        let limit_config = format!("receive.unpackLimit={limit}");
        assert_same(
            &repos,
            target,
            &[&limit_config],
            &new,
            &pack,
            &format!("thin-{target}-{limit}"),
        )?;
    }
    Ok(())
}

#[test]
fn thin_packs_can_be_rejected() -> TestResult {
    let repos = Repos::new()?;
    let pack = repos.pack("three", "one", true)?;
    let new = repos.rev("three")?;
    let count = entries(&pack);
    for limit in [count + 1, count] {
        let limit_config = format!("receive.unpackLimit={limit}");
        assert_same(
            &repos,
            "target.git",
            &[REJECT_THIN, &limit_config],
            &new,
            &pack,
            &format!("reject-thin-{limit}"),
        )?;
    }
    Ok(())
}

#[test]
fn broken_packs_are_rejected_with_the_reasons_of_git() -> TestResult {
    let repos = Repos::new()?;
    let pack = repos.pack("two", "one", false)?;
    let new = repos.rev("two")?;
    let mut bad_signature = pack.clone();
    bad_signature[0] = b'X';
    let mut bad_version = pack.clone();
    bad_version[7] = 9;
    let mut bad_trailer = pack.clone();
    *bad_trailer.last_mut().expect("trailer") ^= 0xff;
    let mut bad_data = pack.clone();
    bad_data[20] ^= 0xff;
    let truncated = pack[..pack.len() - 30].to_vec();
    let count = entries(&pack);
    for (name, pack) in [
        ("empty-stream", Vec::new()),
        ("short-header", pack[..7].to_vec()),
        ("signature", bad_signature),
        ("version", bad_version),
        ("trailer", bad_trailer),
        ("data", bad_data),
        ("truncated", truncated),
    ] {
        for limit in [count + 1, count] {
            let limit_config = format!("receive.unpackLimit={limit}");
            let (state, accepted) = assert_same(
                &repos,
                "target.git",
                &[&limit_config],
                &new,
                &pack,
                &format!("broken-{name}-{limit}"),
            )?;
            assert!(!accepted, "{name}: a broken pack is rejected");
            assert_eq!(state.packs, 1, "{name}: nothing was added");
            assert!(state.loose.is_empty(), "{name}: no loose object was added");
        }
    }
    Ok(())
}

#[test]
fn the_maximum_input_size_is_enforced_like_git_does() -> TestResult {
    let repos = Repos::new()?;
    let pack = repos.pack("two", "one", false)?;
    let new = repos.rev("two")?;
    let len = pack.len() as u64;
    let count = entries(&pack);
    for limit in [count + 1, count] {
        let limit_config = format!("receive.unpackLimit={limit}");
        let (mut accepted, mut rejected) = (0, 0);
        for max in len.saturating_sub(40)..=len + 2 {
            let max_config = format!("receive.maxInputSize={max}");
            let (_, ok) = assert_same(
                &repos,
                "target.git",
                &[&limit_config, &max_config],
                &new,
                &pack,
                &format!("max-{limit}-{max}"),
            )?;
            if ok {
                accepted += 1;
            } else {
                rejected += 1;
            }
        }
        assert!(
            accepted > 0 && rejected > 0,
            "limit {limit}: the window contains the boundary, so the comparison isn't vacuous: {accepted} vs {rejected}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn existing_loose_and_packed_objects_are_freshened_when_unpacking() -> TestResult {
    use filetime::FileTime;
    let repos = Repos::new()?;
    let pack = repos.pack("two", "one", false)?;
    let new = repos.rev("two")?;
    let two_blob = repos.rev("two:two")?;
    let long_ago = FileTime::from_unix_time(1_000_000_000, 0);
    for packed in [false, true] {
        for who in ["git", "ours"] {
            let repo = repos.copy("target.git", &format!("freshen-{packed}-{who}"))?;
            // The blob the pack contains is in the repository already, but unreachable.
            let loose = repo.join("objects").join(&two_blob[..2]).join(&two_blob[2..]);
            let mut write = gix_testtools::git_command(&repo)
                .args(["hash-object", "-w", "--stdin"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            write.stdin.take().expect("piped").write_all(b"two\n")?;
            assert!(write.wait()?.success());
            let existing = if packed {
                let mut cmd = gix_testtools::git_command(&repo);
                let mut child = cmd
                    .args(["pack-objects", "-q"])
                    .arg(repo.join("objects/pack/pack"))
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()?;
                child
                    .stdin
                    .take()
                    .expect("piped")
                    .write_all(format!("{two_blob}\n").as_bytes())?;
                let output = child.wait_with_output()?;
                assert!(output.status.success());
                std::fs::remove_file(&loose)?;
                repo.join(format!(
                    "objects/pack/pack-{}.pack",
                    String::from_utf8_lossy(&output.stdout).trim()
                ))
            } else {
                loose
            };
            assert!(existing.is_file(), "the object to freshen is where it is expected");
            filetime::set_file_mtime(&existing, long_ago)?;
            if who == "git" {
                git_receive(&repo, &[], &new, &pack)?;
            } else {
                quarantine_receive(&repo, &[], &pack)?.expect("the pack is accepted");
            }
            let mtime = FileTime::from_last_modification_time(&std::fs::metadata(&existing)?);
            assert!(
                mtime > long_ago,
                "{who}, packed={packed}: the existing {} is freshened to keep it from being pruned",
                if packed { "pack" } else { "loose object" },
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn the_quarantine_is_private_and_removed_with_its_content() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let repos = Repos::new()?;
    let repo = repos.copy("target.git", "layout")?;
    let quarantine = Quarantine::new(repo.join("objects"), options(&[])?)?;
    let name = quarantine
        .path()
        .file_name()
        .expect("a name")
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("tmp_objdir-incoming-"),
        "so that prune can recognize and delete leftovers: {name}"
    );
    assert_eq!(
        quarantine.path().parent(),
        Some(repo.join("objects").as_path()),
        "it is in the object directory"
    );
    assert_eq!(
        std::fs::metadata(quarantine.path())?.permissions().mode() & 0o777,
        0o700,
        "only the owner can see what isn't accepted yet"
    );
    assert!(quarantine.path().join("pack").is_dir());
    let path = quarantine.path().to_owned();
    drop(quarantine);
    assert!(!path.exists(), "it is gone with its owner");

    let before = state(&repo)?;
    let failed = quarantine_receive(&repo, &[], b"PACK")?;
    assert!(failed.is_err());
    assert_eq!(state(&repo)?, before, "a failed ingest leaves nothing behind, like Git");
    Ok(())
}

#[test]
fn the_environment_makes_programs_see_the_quarantine() -> TestResult {
    let repos = Repos::new()?;
    let repo = repos.copy("target.git", "env")?;
    let quarantine = Quarantine::new(repo.join("objects"), options(&[])?)?;
    let main = std::path::absolute(repo.join("objects"))?;
    let env = |inherited: Option<&str>| -> Vec<(String, String)> {
        quarantine
            .environment(inherited.map(std::ffi::OsStr::new))
            .into_iter()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
            .collect()
    };
    let quarantine_path = quarantine.path().to_string_lossy().into_owned();
    assert!(quarantine.path().is_absolute());
    assert_eq!(
        env(None),
        [
            (
                "GIT_ALTERNATE_OBJECT_DIRECTORIES".to_owned(),
                main.to_string_lossy().into_owned()
            ),
            ("GIT_OBJECT_DIRECTORY".to_owned(), quarantine_path.clone()),
            ("GIT_QUARANTINE_PATH".to_owned(), quarantine_path),
        ]
    );
    assert_eq!(
        env(Some("/elsewhere"))[0].1,
        format!("/elsewhere:{}", main.display()),
        "an inherited value is extended, not replaced"
    );
    Ok(())
}

#[test]
fn unsupported_configuration_is_refused() -> TestResult {
    for config in [
        &["core.sharedRepository=group"][..],
        &["core.sharedRepository=0660"],
        &["receive.fsckObjects=true"],
        &["transfer.fsckObjects=true"],
        &["transfer.fsckObjects=true", "receive.fsckObjects=yes"],
        &["pack.indexVersion=1"],
    ] {
        assert!(
            options(config).is_err(),
            "{config:?} isn't supported and mustn't be ignored"
        );
    }
    for config in [
        &["core.sharedRepository=false"][..],
        &["core.sharedRepository=umask"],
        &["receive.fsckObjects=false", "transfer.fsckObjects=true"],
        &["pack.indexVersion=2"],
        &["core.createObject=rename"],
    ] {
        assert!(options(config).is_ok(), "{config:?} has no consequence or is supported");
    }
    assert!(
        options(&["core.createObject=copy"]).is_err(),
        "unknown values are errors like in Git"
    );
    Ok(())
}

#[test]
fn migrating_continues_after_a_colliding_pack_and_reports_it() -> TestResult {
    let repos = Repos::new()?;
    let repo = repos.copy("target.git", "collision")?;
    let pack = repos.pack("two", "one", false)?;
    let mut quarantine = Quarantine::new(repo.join("objects"), options(&["receive.unpackLimit=0"])?)?;
    quarantine
        .receive_pack(&mut Cursor::new(&pack), &AtomicBool::new(false))
        .expect("accepted");
    let kept_pack = std::fs::read_dir(quarantine.path().join("pack"))?
        .filter_map(|e| Some(e.ok()?.file_name()))
        .find(|name| has_extension(&name.to_string_lossy(), "pack"))
        .expect("the pack is kept");
    // Another pack with the same name but different content, which must never be replaced.
    std::fs::write(repo.join("objects/pack").join(&kept_pack), b"not the same")?;
    let quarantine_path = quarantine.path().to_owned();
    let err = quarantine
        .migrate()
        .expect_err("the pack can't be moved without replacing the other");
    assert!(err.to_string().contains("differ in contents"), "{err}");
    assert!(!quarantine_path.exists(), "the quarantine is gone nonetheless");
    assert_eq!(
        std::fs::read(repo.join("objects/pack").join(&kept_pack))?,
        b"not the same",
        "the existing file wasn't touched"
    );
    let idx = Path::new(&kept_pack).with_extension("idx");
    assert!(
        repo.join("objects/pack").join(idx).is_file(),
        "the remaining files are moved"
    );
    Ok(())
}

/// Compare the file system effects of Git and the quarantine with `strace`, which is the only way to see how
/// and in which order data is flushed and made visible.
#[cfg(target_os = "linux")]
mod effects {
    use std::path::Path;

    use gix_testtools::{
        TestResult,
        strace::{self, Event, Trace},
    };

    use super::{Repos, git_receive_command, has_extension, options};

    const REPO: &str = "GIX_ODB_QUARANTINE_REPO";
    const PACK: &str = "GIX_ODB_QUARANTINE_PACK";
    const CONFIG: &str = "GIX_ODB_QUARANTINE_CONFIG";

    /// What the quarantine does in the re-executed child. Doesn't do anything in a regular run.
    #[test]
    fn fork_child() -> TestResult {
        let Some(repo) = std::env::var_os(REPO) else {
            return Ok(());
        };
        let pack = std::fs::read(std::env::var_os(PACK).expect("set along with the repository"))?;
        let config = std::env::var(CONFIG)?;
        let config: Vec<_> = config.lines().collect();
        let mut options = options(&config)?;
        options.thread_limit = Some(1);
        // The configuration of fsync is that of the repository in Git, but the quarantine is told directly.
        options.fsync = fsync_from(&config);
        quarantine_receive_with(Path::new(&repo), options, &pack)?.expect("accepted");
        Ok(())
    }

    fn fsync_from(config: &[&str]) -> gix_fs::fsync::Options {
        let mut fsync = gix_fs::fsync::Options::default();
        for entry in config {
            match entry.split_once('=') {
                Some(("core.fsync", value)) => fsync.components = gix_fs::fsync::Components::parse(value.into()).0,
                Some(("core.fsyncMethod", value)) => {
                    fsync.method = gix_fs::fsync::Method::parse(value.into()).unwrap_or(fsync.method);
                }
                _ => {}
            }
        }
        fsync
    }

    use super::quarantine_receive_with;

    /// Reduce a trace to what happened to the data of objects: which kind of file was flushed or written out, when the
    /// barrier took place, and when objects were made visible in the object database.
    ///
    /// Files are classified by their final name, as that's what tells a temporary pack from a temporary index.
    /// `.rev` files are ignored, as they are not written, and so is everything that happens in the scratch directory.
    fn abstract_sequence(trace: &Trace) -> Vec<&'static str> {
        let moved: std::collections::HashMap<&str, &str> = trace
            .observations
            .iter()
            .filter(|o| o.ok)
            .filter_map(|o| match &o.event {
                Event::Rename(from, to) | Event::Link(from, to) => Some((from.as_str(), to.as_str())),
                _ => None,
            })
            .collect();
        let class = |path: &str| -> Option<&'static str> {
            let mut path = path;
            while let Some(next) = moved.get(path) {
                path = next;
            }
            if path.contains("bulk_fsync_") {
                return Some("barrier");
            }
            if path.contains("scratch-") || has_extension(path, "rev") {
                return None;
            }
            Some(match path.rsplit('.').next() {
                Some("pack") => "pack",
                Some("idx") => "index",
                _ => "loose",
            })
        };
        let mut out = Vec::new();
        for observation in trace.observations.iter().filter(|o| o.ok) {
            let token = match &observation.event {
                Event::Flush(path) if path.starts_with("objects/") => match class(path) {
                    Some("barrier") => "barrier",
                    Some("pack") => "flush pack",
                    Some("index") => "flush index",
                    Some(_) => "flush loose",
                    None => continue,
                },
                Event::Writeout(path) if path.starts_with("objects/") => match class(path) {
                    Some("barrier") => "barrier",
                    Some(_) => "write out loose",
                    None => continue,
                },
                Event::Link(_, to) | Event::Rename(_, to)
                    if to.starts_with("objects/") && !to.contains("tmp_objdir-") && !has_extension(to, "rev") =>
                {
                    "make visible"
                }
                _ => continue,
            };
            out.push(token);
        }
        out
    }

    fn collapsed(sequence: &[&'static str]) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for token in sequence {
            if out.last() != Some(token) {
                out.push(token);
            }
        }
        out
    }

    fn fork_side(repo: &Path, pack_file: &Path, config: &[&str], inject: Option<&strace::Inject>) -> TestResult<Trace> {
        let mut child = std::process::Command::new(std::env::current_exe()?);
        let test = module_path!().split_once("::").expect("crate prefix").1;
        child
            .args([
                "--exact",
                &format!("{test}::fork_child"),
                "--nocapture",
                "--test-threads=1",
            ])
            .current_dir(repo)
            .env(REPO, repo)
            .env(PACK, pack_file)
            .env(CONFIG, config.join("\n"));
        Ok(strace::trace(&child, repo, repo, inject, None)?)
    }

    #[test]
    fn flushes_and_makes_visible_in_the_order_git_does() -> TestResult {
        if !strace::available_or_skip(module_path!()) {
            return Ok(());
        }
        let repos = Repos::new()?;
        let pack = repos.pack("two", "one", false)?;
        let new = repos.rev("two")?;
        let count = super::entries(&pack);
        let pack_file = repos.path("request.pack");
        std::fs::write(&pack_file, &pack)?;
        let unpack = format!("receive.unpackLimit={}", count + 1);
        let keep = format!("receive.unpackLimit={count}");
        let mut flushed = std::collections::BTreeSet::new();
        for (name, config) in [
            ("unpack with defaults", vec![unpack.as_str()]),
            ("keep with defaults", vec![keep.as_str()]),
            ("unpack flushing everything", vec![unpack.as_str(), "core.fsync=all"]),
            ("keep flushing everything", vec![keep.as_str(), "core.fsync=all"]),
            (
                "unpack writing out",
                vec![
                    unpack.as_str(),
                    "core.fsync=loose-object",
                    "core.fsyncMethod=writeout-only",
                ],
            ),
            (
                "unpack in a batch",
                vec![unpack.as_str(), "core.fsync=loose-object", "core.fsyncMethod=batch"],
            ),
            ("keep without flushing", vec![keep.as_str(), "core.fsync=none"]),
        ] {
            let git_repo = repos.copy("target.git", &format!("git-{}", name.replace(' ', "-")))?;
            let our_repo = repos.copy("target.git", &format!("ours-{}", name.replace(' ', "-")))?;
            let (cmd, request) = git_receive_command(&git_repo, &config, &new, &pack)?;
            let git = strace::trace(&cmd, &git_repo, &git_repo, None, Some(&request))?;
            let ours = fork_side(&our_repo, &pack_file, &config, None)?;
            assert!(
                git.status.success() && ours.status.success(),
                "{name}: both accept the pack"
            );
            let (git_sequence, our_sequence) = (abstract_sequence(&git), abstract_sequence(&ours));
            assert!(
                git_sequence.contains(&"make visible") && git_sequence.contains(&"barrier"),
                "{name}: the trace of Git has what it needs to be meaningful: {git_sequence:?}"
            );
            assert_eq!(
                collapsed(&our_sequence),
                collapsed(&git_sequence),
                "{name}: data is flushed before the barrier, which is before anything is visible"
            );
            let count = |sequence: &[&str], token: &str| sequence.iter().filter(|t| **t == token).count();
            for token in ["flush loose", "write out loose", "flush pack", "flush index", "barrier"] {
                assert_eq!(
                    count(&our_sequence, token),
                    count(&git_sequence, token),
                    "{name}: {token} as often as Git does"
                );
            }
            flushed.extend(git_sequence.iter().copied());
        }
        assert!(
            [
                "flush loose",
                "write out loose",
                "flush pack",
                "flush index",
                "barrier",
                "make visible"
            ]
            .iter()
            .all(|token| flushed.contains(token)),
            "the configurations cover every kind of flush: {flushed:?}"
        );
        Ok(())
    }

    #[test]
    fn a_crash_while_migrating_never_leaves_an_index_without_its_pack() -> TestResult {
        if !strace::available_or_skip(module_path!()) {
            return Ok(());
        }
        let repos = Repos::new()?;
        let pack = repos.pack("two", "one", false)?;
        let pack_file = repos.path("request.pack");
        std::fs::write(&pack_file, &pack)?;
        let keep = format!("receive.unpackLimit={}", super::entries(&pack));
        let mut killed = 0;
        for when in 1..=5 {
            let repo = repos.copy("target.git", &format!("crash-{when}"))?;
            let inject = strace::Inject {
                syscalls: &["linkat", "link"],
                action: "signal=KILL",
                when,
            };
            let trace = fork_side(&repo, &pack_file, &[keep.as_str()], Some(&inject))?;
            if trace.status.success() {
                continue;
            }
            killed += 1;
            let mut names: Vec<String> = std::fs::read_dir(repo.join("objects/pack"))?
                .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect::<Result<_, _>>()?;
            names.sort();
            for index in names.iter().filter(|name| has_extension(name, "idx")) {
                let pack = index.replace(".idx", ".pack");
                assert!(
                    names.contains(&pack),
                    "crash at link {when}: the index can only be found if the pack is there: {names:?}"
                );
            }
        }
        assert!(
            killed >= 3,
            "the pack's files are linked in turn, so that most of the attempts crash: {killed}"
        );
        Ok(())
    }
}

#[test]
fn the_store_of_a_quarantine_sees_what_it_received_along_with_the_repository() -> TestResult {
    use gix_object::Exists;
    let repos = Repos::new()?;
    let pack = repos.pack("two", "one", false)?;
    let (one, two) = (repos.rev("one")?, repos.rev("two")?);
    let id = |hex: &str| gix_hash::ObjectId::from_hex(hex.as_bytes()).expect("valid id");
    let count = entries(&pack);
    for (target, limit) in [
        ("target.git", count + 1),
        ("target.git", count),
        ("thin-alternate.git", count + 1),
    ] {
        let repo = repos.copy(target, &format!("store-{target}-{limit}"))?;
        let mut quarantine = Quarantine::new(
            repo.join("objects"),
            options(&[&format!("receive.unpackLimit={limit}")])?,
        )?;
        let handle = quarantine.store().to_handle();
        assert!(!handle.exists(&id(&two)), "{target}: nothing has been received yet");
        quarantine
            .receive_pack(&mut Cursor::new(&pack), &AtomicBool::new(false))
            .expect("accepted");
        assert!(
            handle.exists(&id(&two)),
            "{target}, limit {limit}: received objects are found"
        );
        assert!(
            handle.exists(&id(&one)),
            "{target}: objects of the repository, even from its alternates, are found"
        );
        let main = odb_at(repo.join("objects"))?;
        assert!(
            !main.exists(&id(&two)),
            "{target}: the repository doesn't see the quarantined object"
        );
        quarantine.migrate()?.release()?;
        assert!(
            odb_at(repo.join("objects"))?.exists(&id(&two)),
            "{target}: once migrated, it does"
        );
    }
    Ok(())
}

#[test]
fn objects_in_cruft_packs_are_written_loose_instead_of_freshened_like_git_does() -> TestResult {
    let repos = Repos::new()?;
    let pack = repos.pack("two", "one", false)?;
    let new = repos.rev("two")?;
    let two_blob = repos.rev("two:two")?;
    let unpack = format!("receive.unpackLimit={}", entries(&pack) + 1);
    let mut loose_afterwards = Vec::new();
    for who in ["git", "ours"] {
        let repo = repos.copy("target.git", &format!("cruft-{who}"))?;
        let mut write = gix_testtools::git_command(&repo)
            .args(["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        write.stdin.take().expect("piped").write_all(b"two\n")?;
        assert!(write.wait()?.success());
        // The blob is unreachable, which is what moves it into a cruft pack.
        gix_testtools::git(&repo, "repack --cruft -d")?;
        let loose = repo.join("objects").join(&two_blob[..2]).join(&two_blob[2..]);
        assert!(!loose.exists(), "{who}: the blob is in the cruft pack now");
        if who == "git" {
            git_receive(&repo, &[&unpack], &new, &pack)?;
        } else {
            quarantine_receive(&repo, &[&unpack], &pack)?.expect("accepted");
        }
        loose_afterwards.push(loose.is_file());
    }
    assert_eq!(
        loose_afterwards,
        [true, true],
        "touching a cruft pack protects nothing as expiry follows its .mtimes, so a loose copy is written, by Git and by us"
    );
    Ok(())
}

#[test]
fn dropping_what_was_migrated_releases_the_keep_files() -> TestResult {
    let repos = Repos::new()?;
    let repo = repos.copy("target.git", "drop-keep")?;
    let pack = repos.pack("two", "one", false)?;
    let mut quarantine = Quarantine::new(repo.join("objects"), options(&["receive.unpackLimit=0"])?)?;
    quarantine
        .receive_pack(&mut Cursor::new(&pack), &AtomicBool::new(false))
        .expect("accepted");
    let migrated = quarantine.migrate()?;
    assert_eq!(migrated.keep_files.len(), 1);
    assert!(
        migrated.keep_files[0].is_file(),
        "the pack is protected while the migration is current"
    );
    let keep = migrated.keep_files[0].clone();
    drop(migrated);
    assert!(
        !keep.exists(),
        "a migration that is dropped, for instance on an error path, doesn't leak the keep file"
    );
    Ok(())
}

type Seen = std::sync::Arc<std::sync::Mutex<Vec<(gix_hash::ObjectId, gix_object::Kind, bool)>>>;

/// An inspector that notes what it was shown and decides with `verdict`.
fn inspecting(seen: &Seen, verdict: gix_odb::quarantine::Inspected) -> gix_odb::quarantine::Inspector {
    let seen = seen.clone();
    gix_odb::quarantine::Inspector::new(move |inspection| {
        let mut seen = seen.lock().expect("no panic while held");
        inspection.objects(&mut |object| {
            seen.push((object.id().to_owned(), object.kind(), inspection.kept()));
            assert_eq!(
                object.data()?.len() as u64,
                object.size(),
                "the size that is known without decoding is that of the decoded object"
            );
            Ok(std::ops::ControlFlow::Continue(()))
        })?;
        Ok(verdict.clone())
    })
}

#[test]
fn an_inspector_sees_what_was_received_on_both_paths_before_anything_is_visible_and_can_refuse() -> TestResult {
    use gix_odb::quarantine::Inspected;

    let repos = Repos::new()?;
    // The pack is thin: what its deltas are based on is the repository's and not part of what was received.
    let pack = repos.pack("three", "one", true)?;
    let count = entries(&pack);
    for (limit, kept) in [(count + 1, false), (count, true)] {
        let limit_config = format!("receive.unpackLimit={limit}");
        let repo = repos.copy("target.git", &format!("inspected-{limit}"))?;
        let before = state(&repo)?;

        let seen = Default::default();
        let mut options = options(&[&limit_config])?;
        options.inspector = Some(inspecting(
            &seen,
            Inspected {
                accepted: true,
                report: b"warning: something\n".to_vec(),
            },
        ));
        let received = quarantine_receive_with(&repo, options, &pack)?.expect("accepted");
        assert_eq!(
            received.report, b"warning: something\n",
            "what the inspector says is passed on"
        );
        let seen = std::mem::take(&mut *seen.lock().expect("no panic while held"));
        assert_eq!(
            seen.len() as u32,
            count,
            "every entry of the pack is shown, and nothing else"
        );
        assert!(
            seen.iter().all(|(_, _, was_kept)| *was_kept == kept),
            "it is told which way the pack goes"
        );
        assert_ne!(state(&repo)?, before, "an accepted pack is part of the repository");

        let repo = repos.copy("target.git", &format!("refused-{limit}"))?;
        let seen = Default::default();
        let mut options = self::options(&[&limit_config])?;
        options.inspector = Some(inspecting(
            &seen,
            Inspected {
                accepted: false,
                report: b"fatal: no\n".to_vec(),
            },
        ));
        let rejected = quarantine_receive_with(&repo, options, &pack)?.expect_err("refused");
        assert_eq!(rejected.report, b"fatal: no\n");
        assert_eq!(
            rejected.reason,
            if kept {
                "index-pack abnormal exit"
            } else {
                "unpack-objects abnormal exit"
            },
            "the pack is refused like Git refuses what its checks fail"
        );
        assert!(rejected.cause.is_none(), "there is nothing to add to what was said");
        assert_eq!(state(&repo)?, before, "nothing of a refused pack is left behind");

        let mut options = self::options(&[&limit_config])?;
        options.inspector = Some(gix_odb::quarantine::Inspector::new(|_| {
            gix_error::bail!(gix_error::message("it can't decide"))
        }));
        let rejected = quarantine_receive_with(&repo, options, &pack)?.expect_err("failed");
        assert!(
            rejected.cause.is_some() && rejected.report.is_empty(),
            "a failure is the reason of the refusal"
        );
    }
    Ok(())
}
