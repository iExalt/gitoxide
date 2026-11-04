//! A push is received by `git receive-pack` and by the native receiver, and both have to say the same and leave the same behind.
//! The remotes are made by `make_receive_remotes.sh`, which says what they contain.
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::AtomicBool,
};

use gix_testtools::TestResult;

/// A hook that records how it was called, says something on both outputs and exits with a code.
#[derive(Clone)]
pub(super) struct Hook {
    pub(super) name: &'static str,
    pub(super) exit: i32,
    /// For `update`: decline only the reference of this name.
    pub(super) decline_ref: Option<&'static str>,
    /// Seconds to wait before doing anything.
    pub(super) sleep: f32,
    /// The file in the remote that it records to.
    log: &'static str,
}

impl Hook {
    pub(super) fn new(name: &'static str) -> Self {
        Hook {
            name,
            exit: 0,
            decline_ref: None,
            sleep: 0.0,
            log: "hook.log",
        }
    }
    /// A `reference-transaction` hook that records everything it sees to a file of its own, and says yes. The recording is
    /// compared in all pushes, so that the transactions of all scenarios are compared with those of Git.
    pub(super) fn recorder() -> Self {
        Hook {
            log: "reftx.log",
            ..Hook::new("reference-transaction")
        }
    }
    pub(super) fn exiting(mut self, code: i32) -> Self {
        self.exit = code;
        self
    }
    pub(super) fn declining(mut self, name: &'static str) -> Self {
        self.decline_ref = Some(name);
        self.exit = 1;
        self
    }
    pub(super) fn sleeping(mut self, seconds: f32) -> Self {
        self.sleep = seconds;
        self
    }

    #[cfg(unix)]
    fn install(&self, remote: &Path) -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let log = remote.join(self.log);
        let name = self.name;
        let decline = match self.decline_ref {
            Some(reference) => format!("[ \"$1\" = {reference} ] && exit {}\nexit 0", self.exit),
            None => format!("exit {}", self.exit),
        };
        let script = format!(
            r#"#!/bin/sh
{{
  echo "== {name} $*"
  echo "cwd=$(pwd -P)"
  echo "GIT_DIR=${{GIT_DIR-unset}}"
  echo "GIT_PUSH_OPTION_COUNT=${{GIT_PUSH_OPTION_COUNT-unset}}"
  i=0
  while [ "$i" -lt "${{GIT_PUSH_OPTION_COUNT:-0}}" ]; do eval "echo GIT_PUSH_OPTION_$i=\$GIT_PUSH_OPTION_$i"; i=$((i+1)); done
  echo "quarantine=$([ -n "${{GIT_QUARANTINE_PATH-}}" ] && echo set || echo unset)"
  case "${{GIT_OBJECT_DIRECTORY-}}" in *tmp_objdir-incoming-*) echo object_dir=quarantine;; '') echo object_dir=unset;; *) echo object_dir=other;; esac
  if [ "{name}" != update ]; then
    while read old new ref; do
      echo "stdin: $old $new $ref"
      case "$new" in *[!0]*) echo "visible: $(git cat-file -t "$new" 2>&1 | head -1)";; esac
    done
  fi
}} >> "{log}"
sleep {sleep}
echo "{name} says hi"
echo "{name} says it on the other output" >&2
printf "{name} without a newline"
{decline}
"#,
            log = log.display(),
            sleep = self.sleep,
        );
        let hooks = remote.join("hooks");
        std::fs::create_dir_all(&hooks)?;
        let path = hooks.join(name);
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn install(&self, _remote: &Path) -> TestResult {
        unreachable!("hooks are scripts, which aren't tried where they can't run")
    }
}

/// A command to send, with revisions of the client instead of ids, where `0` is the null id.
#[derive(Clone, Copy)]
pub(super) struct Cmd(
    pub(super) &'static str,
    pub(super) &'static str,
    pub(super) &'static str,
);

/// What a client sends.
#[derive(Clone, Default)]
pub(super) struct Push {
    pub(super) commands: Vec<Cmd>,
    pub(super) atomic: bool,
    pub(super) quiet: bool,
    /// Don't use a side band. The default is to use one, as Git does.
    pub(super) no_side_band: bool,
    /// Revisions that the pack contains the history of, on top of what the remote has. `None` for all new values.
    pub(super) pack_of: Option<Vec<&'static str>>,
    /// Send no pack at all, even though there are commands that need one.
    pub(super) no_pack: bool,
    /// Arguments of Git commands to run in the remote before anything happens, like `config receive.denyDeletes true`.
    pub(super) setup: Vec<&'static str>,
    /// The directory of the remote in the directory of the ref format, which is `remote.git` unless set.
    pub(super) remote: Option<&'static str>,
    /// Something else to do to the remote, which gets the path of the client.
    /// Hooks to install in the remote, which record how they were run.
    pub(super) hooks: Vec<Hook>,
    /// The push options to send, which the remote is told to accept with `receive.advertisePushOptions`.
    pub(super) options: Vec<&'static str>,
    pub(super) prepare: Option<fn(remote: &Path, client: &Path) -> TestResult>,
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

pub(super) fn pkt(line: impl AsRef<[u8]>) -> Vec<u8> {
    let line = line.as_ref();
    let mut out = format!("{:04x}", line.len() + 4).into_bytes();
    out.extend_from_slice(line);
    out
}

pub(super) struct World {
    pub(super) dir: gix_testtools::tempfile::TempDir,
    pub(super) kind: gix_hash::Kind,
}

impl World {
    pub(super) fn new() -> TestResult<Self> {
        Ok(World {
            dir: gix_testtools::scripted_fixture_writable("make_receive_remotes.sh")?,
            kind: gix_testtools::object_hash_from_env().unwrap_or(gix_hash::Kind::Sha1),
        })
    }

    pub(super) fn client(&self, format: &str) -> PathBuf {
        self.dir.path().join(format).join("client")
    }

    pub(super) fn id(&self, format: &str, rev: &str) -> TestResult<String> {
        if rev == "0" {
            return Ok(gix_hash::ObjectId::null(self.kind).to_string());
        }
        Ok(gix_testtools::git(self.client(format), &format!("rev-parse {rev}"))?
            .trim()
            .to_owned())
    }

    /// The bytes a client sends for `push`.
    pub(super) fn request(&self, format: &str, push: &Push) -> TestResult<Vec<u8>> {
        let mut caps = format!("report-status-v2 object-format={} agent=test", self.kind);
        if !push.no_side_band {
            caps.push_str(" side-band-64k");
        }
        if push.quiet {
            caps.push_str(" quiet");
        }
        if push.atomic {
            caps.push_str(" atomic");
        }
        if !push.options.is_empty() {
            caps.push_str(" push-options");
        }
        let mut out = Vec::new();
        let mut revs = String::new();
        for (index, Cmd(old, new, name)) in push.commands.iter().enumerate() {
            let line = format!("{} {} {name}", self.id(format, old)?, self.id(format, new)?);
            if index == 0 {
                let mut line = line.into_bytes();
                line.push(0);
                line.extend_from_slice(caps.as_bytes());
                out.extend(pkt(line));
            } else {
                out.extend(pkt(line));
            }
            if *new != "0" {
                revs.push_str(&format!("{}\n", self.id(format, new)?));
            }
            if *old != "0" {
                revs.push_str(&format!("^{}\n", self.id(format, old)?));
            }
        }
        out.extend(b"0000");
        if !push.options.is_empty() {
            for option in &push.options {
                out.extend(pkt(option));
            }
            out.extend(b"0000");
        }
        let needs_pack = push.commands.iter().any(|Cmd(_, new, _)| *new != "0");
        if needs_pack && !push.no_pack {
            if let Some(pack_of) = &push.pack_of {
                revs = pack_of
                    .iter()
                    .map(|rev| self.id(format, rev).map(|id| format!("{id}\n")))
                    .collect::<TestResult<String>>()?;
            }
            let mut child = gix_testtools::git_command(self.client(format))
                .env("LC_ALL", "C")
                .env("LANG", "C")
                .args(["pack-objects", "--revs", "--stdout"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            child.stdin.take().expect("piped").write_all(revs.as_bytes())?;
            let output = child.wait_with_output()?;
            assert!(output.status.success(), "the pack can be made");
            out.extend(output.stdout);
        }
        Ok(out)
    }

    /// A copy of the remote of `format` with `config` applied.
    pub(super) fn remote(&self, format: &str, label: &str, push: &Push) -> TestResult<PathBuf> {
        let dest = self.dir.path().join(format!("{format}-{label}"));
        copy_dir(
            &self.dir.path().join(format).join(push.remote.unwrap_or("remote.git")),
            &dest,
        )?;
        // Bare repositories keep no reflogs unless told to, and what is written to them is to be compared.
        gix_testtools::git(&dest, "config core.logAllRefUpdates always")?;
        if !push.options.is_empty() {
            gix_testtools::git(&dest, "config receive.advertisePushOptions true")?;
        }
        for args in &push.setup {
            gix_testtools::git(&dest, args)?;
        }
        for hook in &push.hooks {
            hook.install(&dest)?;
        }
        #[cfg(unix)]
        if !push.hooks.iter().any(|hook| hook.name == "reference-transaction") {
            Hook::recorder().install(&dest)?;
        }
        if let Some(prepare) = push.prepare {
            prepare(&dest, &self.client(format))?;
        }
        Ok(dest)
    }
}

/// What a receiver did, as far as can be compared.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Result_ {
    /// What the client is told, without what is told on the side band for people.
    pub(super) report: Vec<u8>,
    pub(super) refs: Vec<String>,
    pub(super) objects: Vec<String>,
    /// What people are told on the side band, which is what Git prints to its standard error.
    pub(super) messages: String,
    /// Anything that was left behind that isn't an object or reference, like a quarantine.
    pub(super) leftovers: Vec<String>,
    /// How many times the receiver told the client that it is still working, which depends on the time that it takes.
    pub(super) keepalives: usize,
    /// What the hooks recorded about how they were run, with what they differ in made the same.
    pub(super) hooks: String,
    /// What the `reference-transaction` hook recorded in all pushes that didn't install one, which are the transactions.
    pub(super) reftx: String,
    /// `true` if the receiver didn't get to report, which is what a refusal by the `reference-transaction` hook causes.
    pub(super) died: bool,
}

/// The number of packets on the side band that are empty, which is how a client is told to keep waiting.
fn count_keepalives(bytes: &[u8]) -> usize {
    let mut count = 0;
    let mut rest = bytes;
    while rest.len() >= 4 {
        let len = usize::from_str_radix(std::str::from_utf8(&rest[..4]).expect("hex"), 16).expect("hex");
        if len == 0 {
            rest = &rest[4..];
            continue;
        }
        if len == 5 && rest[4] == 1 {
            count += 1;
        }
        rest = &rest[len..];
    }
    count
}

/// Split what is on a side band from the data of the band.
fn band_data(bytes: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (mut data, mut progress) = (Vec::new(), Vec::new());
    let mut rest = bytes;
    while rest.len() >= 4 {
        let len = usize::from_str_radix(std::str::from_utf8(&rest[..4]).expect("hex"), 16).expect("hex");
        if len == 0 {
            rest = &rest[4..];
            continue;
        }
        let payload = &rest[4..len];
        match payload[0] {
            1 => data.extend(&payload[1..]),
            2 => progress.extend(&payload[1..]),
            _ => {}
        }
        rest = &rest[len..];
    }
    (data, progress)
}

/// What Git says on the side band that is not about decisions of the receiver, but about the object database it uses,
/// the programs it runs or how the references are stored, is left out or made the same.
fn normalize_messages(text: &str) -> String {
    text.split(['\n', '\r'])
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("Resolving deltas") && !line.starts_with("fatal: bad object"))
        .map(|line| {
            if line.starts_with("error: cannot lock ref") || line.starts_with("error: Could not prepare reference edit")
            {
                "error: <the reference store says why it refuses>".to_owned()
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn state(repo: &Path) -> TestResult<(Vec<String>, Vec<String>, Vec<String>)> {
    let lines = |output: String| -> Vec<String> {
        let mut lines: Vec<_> = output.lines().map(ToOwned::to_owned).collect();
        lines.sort();
        lines
    };
    let mut refs = lines(gix_testtools::git(
        repo,
        "for-each-ref --format='%(refname) %(objectname)'",
    )?);
    // Who made a reflog entry isn't compared, as Git derives that from the machine if nothing is configured.
    // Walking reflogs fails if a reference leads to a missing object, in which case there is nothing to compare.
    refs.extend(lines(
        gix_testtools::git(repo, "log --walk-reflogs --all --format='reflog %gD %H %gs'").unwrap_or_default(),
    ));
    refs.sort();
    let objects = lines(gix_testtools::git(
        repo,
        "cat-file --batch-all-objects --batch-check='%(objectname) %(objecttype)'",
    )?);
    let mut leftovers = Vec::new();
    let git_dir = PathBuf::from(gix_testtools::git(repo, "rev-parse --absolute-git-dir")?.trim());
    for entry in std::fs::read_dir(git_dir.join("objects"))? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with("tmp_objdir") {
            leftovers.push(name);
        }
    }
    Ok((refs, objects, leftovers))
}

/// Run `git receive-pack` on `remote`.
pub(super) fn git_receive(remote: &Path, request: &[u8], side_band: bool) -> TestResult<Result_> {
    let mut child = gix_testtools::git_command(remote)
        // Messages are compared, so they must not be translated.
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .args(["receive-pack"])
        .arg(std::fs::canonicalize(remote)?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let _ = child.stdin.take().expect("piped").write_all(request);
    let output = child.wait_with_output()?;
    finish(remote, &output.stdout, side_band, !output.status.success())
}

fn finish(remote: &Path, stdout: &[u8], side_band: bool, died: bool) -> TestResult<Result_> {
    // The advertisement ends with the first flush packet.
    let mut rest = stdout;
    loop {
        let len = usize::from_str_radix(std::str::from_utf8(&rest[..4])?, 16)?;
        rest = &rest[len.max(4)..];
        if len == 0 {
            break;
        }
    }
    let (report, messages) = if side_band {
        band_data(rest)
    } else {
        (rest.to_vec(), Vec::new())
    };
    let (refs, objects, leftovers) = state(remote)?;
    let remote_path = std::fs::canonicalize(remote)?.display().to_string();
    let recording = |file: &str| {
        std::fs::read_to_string(remote.join(file))
            .unwrap_or_default()
            .replace(&remote_path, "<REMOTE>")
            // Git passes `.` where the fork passes the absolute path, and the quarantine's name is random.
            .replace("GIT_DIR=.\n", "GIT_DIR=<REMOTE>\n")
    };
    let (hooks, reftx) = (recording("hook.log"), recording("reftx.log"));
    Ok(Result_ {
        died,
        reftx,
        keepalives: if side_band { count_keepalives(rest) } else { 0 },
        hooks,
        report,
        messages: normalize_messages(&String::from_utf8_lossy(&messages)),
        refs,
        objects,
        leftovers,
    })
}

/// Run the native receiver on `remote`.
pub(super) fn fork_receive(remote: &Path, request: &[u8], side_band: bool) -> TestResult<Result_> {
    // Git gets its identity from the environment of the test, which isolated repositories don't see.
    fork_receive_as(remote, request, side_band, &["user.name=a", "user.email=a@example.com"])
}

pub(super) fn fork_receive_as(
    remote: &Path,
    request: &[u8],
    side_band: bool,
    overrides: &[&str],
) -> TestResult<Result_> {
    let repo = gix::open_opts(
        remote,
        gix::open::Options::isolated().config_overrides(overrides.iter().copied()),
    )?;
    let receiver = repo.receive()?;
    let mut input = request;
    let mut output = Vec::new();
    let served = receiver.serve(
        &mut input,
        &mut output,
        gix_protocol::transport::Protocol::V0,
        &AtomicBool::new(false),
    );
    finish(remote, &output, side_band, served.is_err())
}

pub(super) fn assert_same(world: &World, label: &str, push: &Push) -> TestResult {
    assert_same_on(world, label, push, &["files", "reftable"])
}

/// Like [`assert_same()`], for the given reference formats, which is only worth varying if a push reaches the reference store.
pub(super) fn assert_same_on(world: &World, label: &str, push: &Push, formats: &[&str]) -> TestResult {
    compare_on(world, label, push, formats).map(drop)
}

/// Like [`assert_same_on()`], and return what Git did for each of the `formats`.
pub(super) fn compare_on(world: &World, label: &str, push: &Push, formats: &[&str]) -> TestResult<Vec<Result_>> {
    let mut results = Vec::new();
    for format in formats.iter().copied() {
        let request = world.request(format, push)?;
        let git = git_receive(
            &world.remote(format, &format!("git-{label}"), push)?,
            &request,
            !push.no_side_band,
        )?;
        let fork = fork_receive(
            &world.remote(format, &format!("fork-{label}"), push)?,
            &request,
            !push.no_side_band,
        )?;
        assert_eq!(
            String::from_utf8_lossy(&fork.report),
            String::from_utf8_lossy(&git.report),
            "{format}/{label}: the client is told the same"
        );
        assert_eq!(
            fork.messages, git.messages,
            "{format}/{label}: people are told the same"
        );
        let is_head_entry = |line: &String| line.starts_with("reflog HEAD@");
        assert_eq!(
            fork.refs.iter().filter(|line| !is_head_entry(line)).collect::<Vec<_>>(),
            git.refs.iter().filter(|line| !is_head_entry(line)).collect::<Vec<_>>(),
            "{format}/{label}: the same references and the same reflogs of them result"
        );
        // The reflog of `HEAD`, without the position of entries. Git appends an entry for an update that it then rejects,
        // which the fork doesn't copy, so the fork may have fewer entries, and only for new values of rejected commands.
        let head_entries = |refs: &[String]| -> Vec<String> {
            let mut entries: Vec<_> = refs
                .iter()
                .filter(|line| is_head_entry(line))
                .map(|line| {
                    line.split_once("} ")
                        .map_or(line.clone(), |(_, entry)| entry.to_owned())
                })
                .collect();
            entries.sort();
            entries
        };
        let report = String::from_utf8_lossy(&git.report).into_owned();
        let mut rejected_ids = Vec::new();
        for Cmd(_, new, name) in &push.commands {
            if report.contains(&format!("ng {name} ")) {
                rejected_ids.push(world.id(format, new)?);
            }
        }
        let mut fork_entries = head_entries(&fork.refs).into_iter().peekable();
        for entry in head_entries(&git.refs) {
            if fork_entries.peek() == Some(&entry) {
                fork_entries.next();
            } else {
                assert!(
                    rejected_ids.iter().any(|id| entry.starts_with(id.as_str())),
                    "{format}/{label}: Git has a reflog entry of `HEAD` that the fork lacks, which is only expected for an update it rejected: {entry}"
                );
            }
        }
        assert!(
            fork_entries.next().is_none(),
            "{format}/{label}: the fork has no reflog entry of `HEAD` that Git doesn't have"
        );
        assert_eq!(
            fork.objects, git.objects,
            "{format}/{label}: the same objects are there"
        );
        assert_eq!(
            fork.leftovers, git.leftovers,
            "{format}/{label}: nothing is left behind"
        );
        assert_eq!(
            fork.hooks, git.hooks,
            "{format}/{label}: hooks are called with the same arguments, input and environment"
        );
        assert_eq!(
            fork.reftx, git.reftx,
            "{format}/{label}: the transactions that the `reference-transaction` hook sees are the same, in the same order"
        );
        assert_eq!(fork.died, git.died, "{format}/{label}: both die or neither does");
        assert!(
            !git.report.is_empty() || git.died,
            "{format}/{label}: Git reported, so the comparison isn't vacuous"
        );
        results.push(git);
    }
    Ok(results)
}

#[test]
fn creating_updating_and_deleting_references() -> TestResult {
    let world = World::new()?;
    assert_same(
        &world,
        "create",
        &Push {
            commands: vec![Cmd("0", "next", "refs/heads/next")],
            ..Default::default()
        },
    )?;
    assert_same(
        &world,
        "update",
        &Push {
            commands: vec![Cmd("main", "next", "refs/heads/main")],
            ..Default::default()
        },
    )?;
    assert_same(
        &world,
        "delete",
        &Push {
            commands: vec![Cmd("side", "0", "refs/heads/side")],
            ..Default::default()
        },
    )
}

pub(super) fn push(commands: &[Cmd]) -> Push {
    Push {
        commands: commands.to_vec(),
        ..Default::default()
    }
}

pub(super) fn with_setup(mut push: Push, setup: &[&'static str]) -> Push {
    push.setup = setup.to_vec();
    push
}

#[test]
fn non_fast_forwards_and_denying_them() -> TestResult {
    let world = World::new()?;
    let diverge = [Cmd("main", "diverge", "refs/heads/main")];
    assert_same(&world, "non-ff-allowed", &push(&diverge))?;
    assert_same(
        &world,
        "non-ff-denied",
        &with_setup(push(&diverge), &["config receive.denyNonFastForwards true"]),
    )?;
    assert_same(
        &world,
        "ff-with-deny",
        &with_setup(
            push(&[Cmd("main", "next", "refs/heads/main")]),
            &["config receive.denyNonFastForwards true"],
        ),
    )?;
    // The old value is a tag and not a commit, to a branch.
    assert_same(
        &world,
        "non-commit-new-value",
        &push(&[Cmd("0", "v1", "refs/heads/tag-as-branch")]),
    )?;
    assert_same(
        &world,
        "non-commit-with-deny",
        &with_setup(
            push(&[Cmd("main", "v1", "refs/heads/main")]),
            &["config receive.denyNonFastForwards true"],
        ),
    )
}

#[test]
fn denying_deletions() -> TestResult {
    let world = World::new()?;
    let delete_side = [Cmd("side", "0", "refs/heads/side")];
    assert_same(
        &world,
        "deny-deletes",
        &with_setup(push(&delete_side), &["config receive.denyDeletes true"]),
    )?;
    // `HEAD` leads to `main`.
    let delete_main = [Cmd("main", "0", "refs/heads/main")];
    assert_same(&world, "delete-current-default", &push(&delete_main))?;
    for action in ["ignore", "warn", "refuse", "updateInstead"] {
        assert_same(
            &world,
            &format!("delete-current-{action}"),
            &with_setup(
                push(&delete_main),
                &[Box::leak(
                    format!("config receive.denyDeleteCurrent {action}").into_boxed_str(),
                )],
            ),
        )?;
    }
    assert_same(
        &world,
        "deny-deletes-only-heads",
        &with_setup(
            push(&[Cmd("0", "v1", "refs/tags/v1"), Cmd("v1", "0", "refs/tags/v1")]),
            &["config receive.denyDeletes true"],
        ),
    )
}

#[test]
fn the_checked_out_branch_of_a_worktree() -> TestResult {
    let world = World::new()?;
    let on_work = |commands: &[Cmd], setup: &[&'static str]| Push {
        remote: Some("work"),
        ..with_setup(push(commands), setup)
    };
    let update_main = [Cmd("main", "next", "refs/heads/main")];
    assert_same(&world, "checked-out-default", &on_work(&update_main, &[]))?;
    for action in ["ignore", "warn", "refuse", "false", "true"] {
        assert_same(
            &world,
            &format!("checked-out-{action}"),
            &on_work(
                &update_main,
                &[Box::leak(
                    format!("config receive.denyCurrentBranch {action}").into_boxed_str(),
                )],
            ),
        )?;
    }
    assert_same(
        &world,
        "delete-checked-out",
        &on_work(
            &[Cmd("main", "0", "refs/heads/main")],
            &["config receive.denyCurrentBranch ignore"],
        ),
    )?;
    assert_same(
        &world,
        "other-branch-of-worktree",
        &on_work(&[Cmd("side", "next", "refs/heads/side")], &[]),
    )
}

/// A branch that is being rebased or bisected in a worktree is as much checked out as one that `HEAD` points to.
fn rebasing_side(remote: &Path, _client: &Path) -> TestResult {
    gix_testtools::git(remote, "checkout --detach")?;
    let dir = remote.join(".git/rebase-merge");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("head-name"), "refs/heads/side\n")?;
    Ok(())
}

fn bisecting_side(remote: &Path, _client: &Path) -> TestResult {
    gix_testtools::git(remote, "checkout --detach")?;
    std::fs::write(remote.join(".git/BISECT_START"), "side\n")?;
    std::fs::write(remote.join(".git/BISECT_LOG"), "")?;
    Ok(())
}

#[test]
fn a_branch_that_is_rebased_or_bisected_in_a_worktree_is_checked_out() -> TestResult {
    let world = World::new()?;
    for (label, prepare) in [
        ("rebasing", rebasing_side as fn(&Path, &Path) -> TestResult),
        ("bisecting", bisecting_side),
    ] {
        for action in ["refuse", "ignore"] {
            assert_same(
                &world,
                &format!("{label}-{action}"),
                &Push {
                    remote: Some("work"),
                    prepare: Some(prepare),
                    ..with_setup(
                        push(&[Cmd("side", "next", "refs/heads/side")]),
                        &[if action == "refuse" {
                            "config receive.denyCurrentBranch refuse"
                        } else {
                            "config receive.denyCurrentBranch ignore"
                        }],
                    )
                },
            )?;
        }
    }
    Ok(())
}

/// Worktrees that are linked by relative paths need an extension, which is no reason to not see what they have checked out.
#[test]
fn a_branch_checked_out_in_a_worktree_with_relative_paths() -> TestResult {
    let world = World::new()?;
    let relative = with_setup(
        push(&[Cmd("side", "next", "refs/heads/side")]),
        &["worktree add --relative-paths wt side"],
    );
    assert_same(&world, "relative-worktree", &relative)?;
    let request = world.request("files", &relative)?;
    let fork = fork_receive(
        &world.remote("files", "relative-worktree-check", &relative)?,
        &request,
        true,
    )?;
    assert!(
        fork.messages
            .contains("refusing to update checked out branch: refs/heads/side"),
        "the branch of the linked worktree is seen as checked out, so the comparison isn't vacuous: {}",
        fork.messages
    );
    Ok(())
}

#[test]
fn hidden_references_cannot_be_updated_or_deleted() -> TestResult {
    let world = World::new()?;
    let hide = ["config receive.hideRefs refs/heads/side"];
    assert_same(
        &world,
        "hidden-update",
        &with_setup(push(&[Cmd("side", "next", "refs/heads/side")]), &hide),
    )?;
    assert_same(
        &world,
        "hidden-delete",
        &with_setup(push(&[Cmd("side", "0", "refs/heads/side")]), &hide),
    )?;
    assert_same(
        &world,
        "hidden-and-visible",
        &with_setup(
            push(&[
                Cmd("side", "next", "refs/heads/side"),
                Cmd("0", "next", "refs/heads/next"),
            ]),
            &hide,
        ),
    )?;
    assert_same(
        &world,
        "hidden-everything",
        &with_setup(
            push(&[Cmd("side", "next", "refs/heads/side")]),
            &["config transfer.hideRefs refs/heads"],
        ),
    )?;
    assert_same(
        &world,
        "hidden-negated",
        &with_setup(
            push(&[
                Cmd("side", "next", "refs/heads/side"),
                Cmd("main", "next", "refs/heads/main"),
            ]),
            &[
                "config transfer.hideRefs refs/heads",
                "config --add receive.hideRefs !refs/heads/main",
            ],
        ),
    )
}

#[test]
fn atomic_and_partial_failures() -> TestResult {
    let world = World::new()?;
    // The old value of `side` is wrong, and the creation of `next` is fine.
    let commands = [
        Cmd("0", "next", "refs/heads/next"),
        Cmd("main", "next", "refs/heads/side"),
    ];
    assert_same(&world, "partial", &push(&commands))?;
    assert_same(
        &world,
        "atomic",
        &Push {
            atomic: true,
            ..push(&commands)
        },
    )?;
    assert_same(
        &world,
        "atomic-not-advertised",
        &Push {
            atomic: true,
            ..with_setup(push(&commands), &["config receive.advertiseAtomic false"])
        },
    )?;
    assert_same(
        &world,
        "atomic-all-fine",
        &Push {
            atomic: true,
            ..push(&[
                Cmd("0", "next", "refs/heads/next"),
                Cmd("side", "main", "refs/heads/side"),
            ])
        },
    )?;
    assert_same(
        &world,
        "already-exists",
        &push(&[Cmd("0", "next", "refs/heads/main"), Cmd("0", "next", "refs/heads/new")]),
    )?;
    assert_same(
        &world,
        "delete-with-wrong-old",
        &push(&[Cmd("main", "0", "refs/heads/side"), Cmd("0", "next", "refs/heads/new")]),
    )?;
    assert_same(
        &world,
        "delete-missing",
        &push(&[Cmd("main", "0", "refs/heads/nope"), Cmd("0", "next", "refs/heads/new")]),
    )
}

#[test]
fn funny_reference_names() -> TestResult {
    let world = World::new()?;
    for (index, name) in [
        "heads/foo",
        "refs/foo",
        "refs/heads/a..b",
        "refs/heads/x.lock",
        "refs/heads/",
        "refs/heads//x",
        "refs/heads/-x",
        "refs/heads/a b",
        "refs/heads/@{x}",
        "refs/heads/@",
        "refs/heads/ok/ok",
        "refs/HEAD",
        "refs/heads/HEAD",
        "HEAD",
        "refs",
        "refs/",
    ]
    .into_iter()
    .enumerate()
    {
        // Names are judged before the reference store is touched, so only a valid one has to be tried with both.
        let formats: &[&str] = if name == "refs/heads/ok/ok" {
            &["files", "reftable"]
        } else {
            &["files"]
        };
        assert_same_on(
            &world,
            &format!("funny-create-{index}"),
            &push(&[Cmd("0", "next", name)]),
            formats,
        )?;
        assert_same_on(
            &world,
            &format!("funny-delete-{index}"),
            &push(&[Cmd("main", "0", name)]),
            formats,
        )?;
    }
    Ok(())
}

#[test]
fn objects_that_are_missing_or_not_needed() -> TestResult {
    let world = World::new()?;
    let create = [Cmd("0", "next", "refs/heads/next")];
    assert_same(
        &world,
        "pack-lacks-objects",
        &Push {
            pack_of: Some(vec!["main"]),
            ..push(&create)
        },
    )?;
    assert_same(
        &world,
        "empty-pack",
        &Push {
            pack_of: Some(vec![]),
            ..push(&create)
        },
    )?;
    assert_same(
        &world,
        "empty-pack-for-known-commit",
        &Push {
            pack_of: Some(vec![]),
            ..push(&[Cmd("0", "main", "refs/heads/copy")])
        },
    )?;
    assert_same(
        &world,
        "no-pack",
        &Push {
            no_pack: true,
            ..push(&create)
        },
    )?;
    assert_same(
        &world,
        "one-of-two-lacks-objects",
        &Push {
            pack_of: Some(vec!["next"]),
            ..push(&[
                Cmd("0", "next", "refs/heads/next"),
                Cmd("0", "diverge", "refs/heads/diverge"),
            ])
        },
    )?;
    assert_same(
        &world,
        "pack-with-more-than-needed",
        &Push {
            pack_of: Some(vec!["next", "diverge", "v1"]),
            ..push(&create)
        },
    )?;
    assert_same(
        &world,
        "all-rejected-leaves-objects-alone",
        &with_setup(
            push(&[Cmd("side", "next", "refs/heads/side")]),
            &["config receive.hideRefs refs/heads/side"],
        ),
    )
}

#[test]
fn tags_and_the_shape_of_the_response() -> TestResult {
    let world = World::new()?;
    assert_same(&world, "tag", &push(&[Cmd("0", "v1", "refs/tags/v1")]))?;
    assert_same(
        &world,
        "quiet",
        &Push {
            quiet: true,
            ..push(&[Cmd("0", "next", "refs/heads/next")])
        },
    )?;
    assert_same(
        &world,
        "no-side-band",
        &Push {
            no_side_band: true,
            ..push(&[Cmd("0", "next", "refs/heads/next")])
        },
    )?;
    assert_same(
        &world,
        "unpack-limit-keeps-pack",
        &with_setup(
            push(&[Cmd("0", "next", "refs/heads/next")]),
            &["config receive.unpackLimit 1"],
        ),
    )
}

/// Git derives an identity for the reflog from the machine if none is configured, and a server that receives pushes often
/// has none. The fork makes one up, as pushes must not fail for it.
#[test]
fn a_push_without_any_identity_is_received() -> TestResult {
    let world = World::new()?;
    let push = push(&[Cmd("0", "next", "refs/heads/next"), Cmd("main", "0", "refs/heads/side")]);
    for format in ["files", "reftable"] {
        let request = world.request(format, &push)?;
        let git = git_receive(&world.remote(format, "git-anonymous", &push)?, &request, true)?;
        let fork = fork_receive_as(&world.remote(format, "fork-anonymous", &push)?, &request, true, &[])?;
        assert_eq!(fork.report, git.report, "{format}: the client is told the same");
        assert_eq!(
            fork.refs, git.refs,
            "{format}: the references and their reflogs are there without an identity"
        );
        assert!(
            fork.refs.iter().any(|line| line.starts_with("reflog ")),
            "{format}: a reflog was written, so the comparison isn't vacuous: {:?}",
            fork.refs
        );
    }
    Ok(())
}

/// An accepted update of the branch `HEAD` leads to, next to a rejected one, has to log `HEAD` as Git does.
#[test]
fn an_accepted_update_of_the_current_branch_next_to_a_rejected_one() -> TestResult {
    let world = World::new()?;
    assert_same(
        &world,
        "mixed-head",
        &Push {
            remote: Some("work"),
            ..with_setup(
                push(&[
                    Cmd("main", "next", "refs/heads/main"),
                    Cmd("main", "next", "refs/heads/side"),
                ]),
                &["config receive.denyCurrentBranch ignore"],
            )
        },
    )
}

/// Without `core.logAllRefUpdates`, bare repositories keep no reflogs, which is what Git does and the fork has to do.
#[test]
fn bare_repositories_keep_no_reflogs_by_default() -> TestResult {
    let world = World::new()?;
    let push = push(&[
        Cmd("main", "next", "refs/heads/main"),
        Cmd("0", "next", "refs/heads/next"),
    ]);
    for format in ["files", "reftable"] {
        let request = world.request(format, &push)?;
        let remote = |label: &str| -> TestResult<PathBuf> {
            let dest = world.remote(format, label, &push)?;
            gix_testtools::git(&dest, "config --unset core.logAllRefUpdates")?;
            Ok(dest)
        };
        let git = git_receive(&remote("git-default-log")?, &request, true)?;
        let fork = fork_receive(&remote("fork-default-log")?, &request, true)?;
        assert_eq!(fork.refs, git.refs, "{format}: references and reflogs are the same");
        assert!(
            !git.refs.iter().any(|line| line.starts_with("reflog ")),
            "{format}: Git wrote no reflog, which is what the fork has to do as well: {:?}",
            git.refs
        );
        assert!(
            git.refs.iter().any(|line| line.starts_with("refs/heads/next ")),
            "{format}: the push was received, so the comparison isn't vacuous"
        );
    }
    Ok(())
}

#[test]
fn the_same_reference_twice_in_one_push() -> TestResult {
    let world = World::new()?;
    let twice = [
        Cmd("0", "next", "refs/heads/dup"),
        Cmd("0", "diverge", "refs/heads/dup"),
    ];
    assert_same(&world, "dup-names", &push(&twice))?;
    assert_same(
        &world,
        "dup-names-atomic",
        &Push {
            atomic: true,
            ..push(&twice)
        },
    )
}

#[test]
fn symbolic_references_that_are_updated_together_with_their_target() -> TestResult {
    let world = World::new()?;
    let alias = ["symbolic-ref refs/heads/alias refs/heads/main"];
    // The same update twice is done once.
    assert_same(
        &world,
        "alias-consistent",
        &with_setup(
            push(&[
                Cmd("main", "next", "refs/heads/alias"),
                Cmd("main", "next", "refs/heads/main"),
            ]),
            &alias,
        ),
    )?;
    assert_same(
        &world,
        "alias-inconsistent",
        &with_setup(
            push(&[
                Cmd("main", "next", "refs/heads/alias"),
                Cmd("main", "diverge", "refs/heads/main"),
            ]),
            &alias,
        ),
    )?;
    // The alias leads to a branch that is born by the push.
    assert_same(
        &world,
        "alias-dangling",
        &with_setup(
            push(&[
                Cmd("0", "next", "refs/heads/alias"),
                Cmd("0", "next", "refs/heads/unborn"),
            ]),
            &["symbolic-ref refs/heads/alias refs/heads/unborn"],
        ),
    )?;
    // Git deletes the alias and what it leads to. Where that is the current branch, only the fork protects it, see
    // `the_current_branch_is_protected_when_reached_through_an_alias_unlike_in_git`.
    assert_same(
        &world,
        "alias-delete",
        &with_setup(
            push(&[Cmd("main", "0", "refs/heads/alias")]),
            &[alias[0], "config receive.denyDeleteCurrent ignore"],
        ),
    )?;
    assert_same(
        &world,
        "alias-delete-other-branch",
        &with_setup(
            push(&[Cmd("side", "0", "refs/heads/alias")]),
            &["symbolic-ref refs/heads/alias refs/heads/side"],
        ),
    )?;
    assert_same(
        &world,
        "alias-alone",
        &with_setup(push(&[Cmd("main", "next", "refs/heads/alias")]), &alias),
    )?;
    assert_same(
        &world,
        "alias-cycle",
        &with_setup(
            push(&[Cmd("0", "next", "refs/heads/a")]),
            &[
                "symbolic-ref refs/heads/a refs/heads/b",
                "symbolic-ref refs/heads/b refs/heads/a",
            ],
        ),
    )
}

/// Make the reference `refs/heads/broken` lead to an object that doesn't exist, which only Git's files storage allows.
fn break_a_reference(remote: &Path, _client: &Path) -> TestResult {
    let git_dir = PathBuf::from(gix_testtools::git(remote, "rev-parse --absolute-git-dir")?.trim());
    let id = "ab".repeat(gix_hash::Kind::Sha1.len_in_bytes());
    let id = if git_dir.join("reftable").exists() {
        return Ok(());
    } else if gix_testtools::object_hash_from_env() == Some(gix_hash::Kind::Sha256) {
        "ab".repeat(gix_hash::Kind::Sha256.len_in_bytes())
    } else {
        id
    };
    std::fs::write(git_dir.join("refs/heads/broken"), format!("{id}\n"))?;
    Ok(())
}

#[test]
fn a_reference_to_a_missing_object_fails_pushes_of_objects_but_not_deletions() -> TestResult {
    let world = World::new()?;
    let broken = |commands: &[Cmd]| Push {
        prepare: Some(break_a_reference),
        ..push(commands)
    };
    assert_same(
        &world,
        "broken-ref-delete-only",
        &broken(&[Cmd("side", "0", "refs/heads/side")]),
    )?;
    assert_same(
        &world,
        "broken-ref-update",
        &broken(&[Cmd("0", "next", "refs/heads/next")]),
    )?;
    assert_same(
        &world,
        "broken-ref-update-and-delete",
        &broken(&[Cmd("0", "next", "refs/heads/next"), Cmd("side", "0", "refs/heads/side")]),
    )
}

/// What is told in the advertisement, without the agent that tells it and the lengths of lines that follow from it.
fn without_agent(advertisement: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest = advertisement;
    while rest.len() >= 4 {
        let len = usize::from_str_radix(std::str::from_utf8(&rest[..4]).expect("hex"), 16).expect("hex");
        if len == 0 {
            lines.push("flush".to_owned());
            rest = &rest[4..];
            continue;
        }
        let mut line = String::from_utf8_lossy(&rest[4..len]).into_owned();
        if let Some(start) = line.find(" agent=") {
            let end = line[start + 1..]
                .find(['\n', ' '])
                .map_or(line.len(), |end| start + 1 + end);
            line.replace_range(start..end, "");
        }
        lines.push(line);
        rest = &rest[len..];
    }
    lines
}

fn add_alternate_of_client(remote: &Path, client: &Path) -> TestResult {
    let git_dir = PathBuf::from(gix_testtools::git(remote, "rev-parse --absolute-git-dir")?.trim());
    std::fs::write(
        git_dir.join("objects/info/alternates"),
        format!("{}\n", client.join(".git/objects").display()),
    )?;
    Ok(())
}

#[test]
fn the_advertisement_is_that_of_git() -> TestResult {
    let world = World::new()?;
    let cases: &[(&str, Push)] = &[
        ("plain", Push::default()),
        (
            "hidden",
            with_setup(Push::default(), &["config receive.hideRefs refs/heads/side"]),
        ),
        (
            "no-atomic",
            with_setup(Push::default(), &["config receive.advertiseAtomic false"]),
        ),
        (
            "push-options",
            with_setup(Push::default(), &["config receive.advertisePushOptions true"]),
        ),
        (
            "no-ofs-delta",
            with_setup(Push::default(), &["config repack.useDeltaBaseOffset false"]),
        ),
        (
            "alias",
            with_setup(Push::default(), &["symbolic-ref refs/heads/alias refs/heads/main"]),
        ),
        (
            "alternate",
            Push {
                prepare: Some(add_alternate_of_client),
                ..Push::default()
            },
        ),
        (
            "empty",
            with_setup(
                Push::default(),
                &["update-ref -d refs/heads/main", "update-ref -d refs/heads/side"],
            ),
        ),
    ];
    for (label, case) in cases {
        for format in ["files", "reftable"] {
            let git_remote = world.remote(format, &format!("adv-git-{label}"), case)?;
            let output = gix_testtools::git_command(&git_remote)
                .env("LC_ALL", "C")
                .env("LANG", "C")
                .args(["receive-pack", "--advertise-refs"])
                .arg(std::fs::canonicalize(&git_remote)?)
                .output()?;
            assert!(output.status.success(), "{format}/{label}: Git advertises");
            let fork_remote = world.remote(format, &format!("adv-fork-{label}"), case)?;
            let repo = gix::open_opts(&fork_remote, gix::open::Options::isolated())?;
            let advertisement = repo
                .receive()?
                .advertisement()?
                .encode(gix_protocol::transport::Protocol::V0)?;
            assert_eq!(
                without_agent(&advertisement),
                without_agent(&output.stdout),
                "{format}/{label}: the same is offered"
            );
        }
    }
    Ok(())
}

#[test]
fn a_deletion_without_old_value_is_denied_like_other_deletions_unlike_in_git() -> TestResult {
    let world = World::new()?;
    let format = "files";
    let null_delete = push(&[Cmd("0", "0", "refs/heads/side")]);
    // Without a rule, Git and the fork delete the reference.
    assert_same(&world, "null-delete", &null_delete)?;
    assert_same(
        &world,
        "null-delete-missing",
        &push(&[Cmd("0", "0", "refs/heads/missing")]),
    )?;
    // `denyDeletes` isn't applied by Git, which lets a crafted request get around it.
    let denied = with_setup(null_delete, &["config receive.denyDeletes true"]);
    let request = world.request(format, &denied)?;
    let git = git_receive(&world.remote(format, "null-git", &denied)?, &request, true)?;
    let fork = fork_receive(&world.remote(format, "null-fork", &denied)?, &request, true)?;
    assert!(
        String::from_utf8_lossy(&git.report).contains("ok refs/heads/side"),
        "Git reports success: {}",
        String::from_utf8_lossy(&git.report)
    );
    assert!(
        !git.refs.iter().any(|line| line.starts_with("refs/heads/side ")),
        "and Git deleted the branch that the setting protects"
    );
    assert!(
        String::from_utf8_lossy(&fork.report).contains("ng refs/heads/side deletion prohibited"),
        "the fork refuses: {}",
        String::from_utf8_lossy(&fork.report)
    );
    assert!(
        fork.refs.iter().any(|line| line.starts_with("refs/heads/side ")),
        "and keeps the branch"
    );
    // The same for the current branch, which `HEAD` leads to.
    let current = push(&[Cmd("0", "0", "refs/heads/main")]);
    let request = world.request(format, &current)?;
    let git = git_receive(&world.remote(format, "null-current-git", &current)?, &request, true)?;
    let fork = fork_receive(&world.remote(format, "null-current-fork", &current)?, &request, true)?;
    assert!(
        String::from_utf8_lossy(&git.report).contains("ok refs/heads/main"),
        "Git deletes the current branch"
    );
    assert!(
        String::from_utf8_lossy(&fork.report).contains("ng refs/heads/main deletion of the current branch prohibited"),
        "the fork doesn't"
    );
    Ok(())
}

/// Git looks at the name that is pushed to when it protects the checked out branch, so a symbolic reference to it is a way
/// around `receive.denyDeleteCurrent` and `receive.denyCurrentBranch`. The fork follows the reference.
#[test]
fn the_current_branch_is_protected_when_reached_through_an_alias_unlike_in_git() -> TestResult {
    let world = World::new()?;
    let alias = ["symbolic-ref refs/heads/alias refs/heads/main"];
    for format in ["files", "reftable"] {
        // `HEAD` of the bare remote leads to `main`.
        let delete = with_setup(push(&[Cmd("main", "0", "refs/heads/alias")]), &alias);
        // The worktree of `work` has `main` checked out.
        let update = Push {
            remote: Some("work"),
            ..with_setup(push(&[Cmd("main", "next", "refs/heads/alias")]), &alias)
        };
        for (label, push, git_says, fork_says, kept) in [
            (
                "delete",
                delete,
                "ok refs/heads/alias",
                "ng refs/heads/alias deletion of the current branch prohibited",
                "refs/heads/main ",
            ),
            (
                "update",
                update,
                "ok refs/heads/alias",
                "ng refs/heads/alias branch is currently checked out",
                "refs/heads/main ",
            ),
        ] {
            let request = world.request(format, &push)?;
            let git = git_receive(
                &world.remote(format, &format!("alias-{label}-git"), &push)?,
                &request,
                true,
            )?;
            let fork = fork_receive(
                &world.remote(format, &format!("alias-{label}-fork"), &push)?,
                &request,
                true,
            )?;
            let (git_report, fork_report) = (
                String::from_utf8_lossy(&git.report),
                String::from_utf8_lossy(&fork.report),
            );
            assert!(
                git_report.contains(git_says),
                "{format}/{label}: Git lets it through: {git_report}"
            );
            assert!(
                fork_report.contains(fork_says),
                "{format}/{label}: the fork refuses: {fork_report}"
            );
            let main = |refs: &[String]| refs.iter().find(|line| line.starts_with(kept)).cloned();
            assert_ne!(
                main(&git.refs),
                main(&fork.refs),
                "{format}/{label}: Git changed or deleted the current branch, and the fork kept it"
            );
            assert!(
                main(&fork.refs).is_some(),
                "{format}/{label}: the branch is still there"
            );
        }
    }
    Ok(())
}

#[test]
fn a_stale_update_through_a_symbolic_reference_is_rejected_unlike_in_git() -> TestResult {
    let world = World::new()?;
    let format = "files";
    // `alias` leads to `main` at "c2", and the old value that is sent is "c1".
    let stale = with_setup(
        push(&[
            Cmd("side", "next", "refs/heads/alias"),
            Cmd("0", "next", "refs/heads/other"),
        ]),
        &["symbolic-ref refs/heads/alias refs/heads/main"],
    );
    let request = world.request(format, &stale)?;
    let git = git_receive(&world.remote(format, "alias-git", &stale)?, &request, true)?;
    let fork = fork_receive(&world.remote(format, "alias-fork", &stale)?, &request, true)?;
    let git_report = String::from_utf8_lossy(&git.report).into_owned();
    assert!(
        git_report.contains("ok refs/heads/alias") && git_report.contains("ok refs/heads/other"),
        "Git reports success for the rejected update, as it looks for the failure by the name of what it updated: {git_report}"
    );
    let fork_report = String::from_utf8_lossy(&fork.report).into_owned();
    assert!(
        fork_report.contains("ng refs/heads/alias incorrect old value provided\n")
            && fork_report.contains("ok refs/heads/other\n"),
        "the fork says that it's rejected, which it is: {fork_report}"
    );
    let without_reflogs = |refs: &[String]| -> Vec<String> {
        refs.iter()
            .filter(|line| !line.starts_with("reflog "))
            .cloned()
            .collect()
    };
    assert_eq!(
        without_reflogs(&fork.refs),
        without_reflogs(&git.refs),
        "both left `main` alone and created the other branch, while Git logged an update of the alias that didn't happen"
    );
    Ok(())
}

fn receiver_with_fault(world: &World, label: &str, push: &Push, fault: gix::receive::Fault) -> TestResult<Result_> {
    let format = "files";
    let request = world.request(format, push)?;
    let remote = world.remote(format, label, push)?;
    let repo = gix::open_opts(
        &remote,
        gix::open::Options::isolated().config_overrides(["user.name=a", "user.email=a@example.com"]),
    )?;
    let mut input = request.as_slice();
    let mut output = Vec::new();
    repo.receive()?.with_fault(fault).serve(
        &mut input,
        &mut output,
        gix_protocol::transport::Protocol::V0,
        &AtomicBool::new(false),
    )?;
    finish(&remote, &output, true, false)
}

#[test]
fn failures_of_the_reference_store_fail_every_command_like_git_does_in_its_loops() -> TestResult {
    use gix::receive::Fault;
    let world = World::new()?;
    // `funny` is rejected before anything is started, but is told to have failed like the others.
    let commands = [
        Cmd("0", "next", "refs/heads/next"),
        Cmd("main", "0", "refs/heads/side"),
        Cmd("0", "next", "refs/heads/funny name"),
    ];
    // Without the one that fails by itself, which would make an atomic push fail before the store is asked.
    let commands_that_may_apply = &commands[..2];
    for (fault, atomic, expected) in [
        (Fault::CommitTransaction, false, "failed to update refs"),
        (Fault::StartTransaction, false, "transaction failed to start"),
        (Fault::CommitTransaction, true, "atomic transaction failed"),
        (Fault::StartTransaction, true, "transaction failed to start"),
    ] {
        let commands = if atomic { commands_that_may_apply } else { &commands[..] };
        let result = receiver_with_fault(
            &world,
            &format!("fault-{fault:?}-{atomic}"),
            &Push {
                atomic,
                ..push(commands)
            },
            fault,
        )?;
        let report = String::from_utf8_lossy(&result.report).into_owned();
        assert_eq!(
            report.matches(expected).count(),
            commands.len(),
            "{fault:?}/{atomic}: all commands are told {expected:?}, even the one that was rejected for another reason: {report}"
        );
        assert!(
            result.refs.iter().any(|line| line.starts_with("refs/heads/side ")),
            "{fault:?}/{atomic}: nothing was applied"
        );
        assert!(
            !result.refs.iter().any(|line| line.starts_with("refs/heads/next ")),
            "{fault:?}/{atomic}: nothing was applied"
        );
    }
    Ok(())
}

/// What the file system sees of a push, as it was observed for `git receive-pack` is what the native receiver does as well:
/// the same objects are written and made durable in the same order, and the same references are locked, flushed and renamed.
#[cfg(target_os = "linux")]
mod effects {
    use std::process::Command;

    use gix_testtools::strace::{self, Event};

    use super::*;

    const REMOTE: &str = "GIX_RECV_REMOTE";
    const REQUEST: &str = "GIX_RECV_REQUEST";

    /// What the native receiver does in the re-executed child, which is traced alone. Does nothing in a regular run.
    #[test]
    fn fork_child() -> TestResult {
        let (Some(remote), Some(request)) = (std::env::var_os(REMOTE), std::env::var_os(REQUEST)) else {
            return Ok(());
        };
        let request = std::fs::read(request)?;
        let repo = gix::open_opts(
            remote,
            gix::open::Options::isolated().config_overrides(["user.name=a", "user.email=a@example.com"]),
        )?;
        let mut input = request.as_slice();
        let mut output = Vec::new();
        repo.receive()?.serve(
            &mut input,
            &mut output,
            gix_protocol::transport::Protocol::V0,
            &AtomicBool::new(false),
        )?;
        Ok(())
    }

    /// Make names that depend on content or chance the same, and leave out what is not about the objects and references of the push.
    fn normalize(path: &str) -> String {
        let is_hex = |segment: &str| !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_hexdigit());
        path.split('/')
            .map(|segment| {
                if segment.starts_with("tmp_objdir-incoming-") {
                    "INCOMING".to_owned()
                } else if segment.len() == 2 && is_hex(segment) {
                    "xx".to_owned()
                } else if is_hex(segment) && (segment.len() == 38 || segment.len() == 62) {
                    "OID".to_owned()
                } else if let Some(rest) = segment.strip_prefix("pack-")
                    && let Some((hash, extension)) = rest.split_once('.')
                    && is_hex(hash)
                {
                    format!("pack-HASH.{extension}")
                } else {
                    segment.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// What is observed of references and `HEAD`, which has to be the same.
    fn reference_effects(trace: &strace::Trace) -> Vec<String> {
        trace
            .observations
            .iter()
            .filter(|observation| observation.ok && !matches!(observation.event, Event::Directory(_)))
            .map(|observation| normalize(&observation.event.render()))
            // Git locks `HEAD` and releases it without writing when it updates the branch `HEAD` points to; this is
            // a recorded divergence, as the fork neither takes nor needs that lock.
            .filter(|line| !line.contains("objects/") && !line.contains("HEAD.lock") && line.contains("refs/"))
            .collect()
    }

    /// The phases of a whole push as the files show them: objects are flushed in the quarantine, a barrier makes all of them
    /// durable, they are moved into the object database, and only then are references updated. Repeats of one phase are
    /// collapsed because Git writes loose objects with `unpack-objects` and the fork with its own writer, so the number of
    /// files and their temporary names differ in ways that don't matter; how the quarantine itself writes is compared with
    /// Git in the `gix-odb` tests.
    fn push_phases(trace: &strace::Trace) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for observation in trace.observations.iter().filter(|observation| observation.ok) {
            let line = normalize(&observation.event.render());
            if line.contains("maintenance.lock") || line.contains("objects/info") {
                continue;
            }
            let phase = match &observation.event {
                Event::Flush(_) if line.contains("bulk_fsync") => "barrier",
                Event::Flush(_) | Event::Writeout(_) if line.contains("objects/") => "flush-in-quarantine",
                Event::Rename(_, to) | Event::Link(_, to) => {
                    let to = normalize(to);
                    if to.contains("refs/heads/") && !to.ends_with(".lock") {
                        "update-reference"
                    } else if to.contains("objects/") && !to.contains("INCOMING") {
                        "migrate"
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };
            if out.last() != Some(&phase) {
                out.push(phase);
            }
        }
        out
    }

    fn compare(label: &str, setup: &[&'static str], commands: &[Cmd]) -> TestResult {
        if !strace::available_or_skip(module_path!()) {
            return Ok(());
        }
        let world = World::new()?;
        let format = "files";
        let push = with_setup(push(commands), setup);
        let request_file = world.dir.path().join(format!("request-{label}"));
        std::fs::write(&request_file, world.request(format, &push)?)?;

        let git_remote = world.remote(format, &format!("trace-git-{label}"), &push)?;
        let mut git = gix_testtools::git_command(&git_remote);
        git.env("LC_ALL", "C").env("LANG", "C");
        git.args(["receive-pack"]).arg(&git_remote);
        let git_trace = strace::trace(&git, &git_remote, &git_remote, None, Some(&request_file))?;

        let fork_remote = world.remote(format, &format!("trace-fork-{label}"), &push)?;
        let mut fork = Command::new(std::env::current_exe()?);
        let test = module_path!().split_once("::").expect("crate prefix").1;
        fork.args([
            "--exact",
            &format!("{test}::fork_child"),
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(&fork_remote)
        .env(REMOTE, &fork_remote)
        .env(REQUEST, &request_file);
        let fork_trace = strace::trace(&fork, &fork_remote, &fork_remote, None, None)?;
        assert!(
            git_trace.status.success() && fork_trace.status.success(),
            "{label}: both receive the push"
        );
        let (git_refs, fork_refs) = (reference_effects(&git_trace), reference_effects(&fork_trace));
        assert!(
            git_refs
                .iter()
                .any(|line| line.starts_with("rename") && line.contains("refs/heads/")),
            "{label}: Git updated a reference, so the comparison isn't vacuous: {git_refs:#?}"
        );
        assert_eq!(
            fork_refs, git_refs,
            "{label}: references are locked, flushed and renamed as Git does\nfork: {fork_refs:#?}\ngit: {git_refs:#?}"
        );
        let (git_objects, fork_objects) = (push_phases(&git_trace), push_phases(&fork_trace));
        assert!(
            git_objects.contains(&"barrier"),
            "{label}: Git flushed a barrier, so the comparison isn't vacuous: {git_objects:#?}"
        );
        assert_eq!(
            fork_objects, git_objects,
            "{label}: objects are flushed, barriered, migrated and only then referenced, as Git does\nfork: {fork_objects:#?}\ngit: {git_objects:#?}"
        );
        Ok(())
    }

    #[test]
    fn receiving_a_few_objects_writes_and_flushes_like_git() -> TestResult {
        let commands = [Cmd("main", "next", "refs/heads/main")];
        compare("defaults", &[], &commands)?;
        compare("fsync-all", &["config core.fsync all"], &commands)?;
        compare("fsync-reference", &["config core.fsync reference"], &commands)?;
        compare("fsync-none", &["config core.fsync none"], &commands)?;
        compare("fsync-loose-object", &["config core.fsync loose-object"], &commands)
    }

    #[test]
    fn receiving_a_pack_writes_and_flushes_like_git() -> TestResult {
        let commands = [Cmd("main", "next", "refs/heads/main")];
        compare("keep-defaults", &["config receive.unpackLimit 1"], &commands)?;
        compare(
            "keep-fsync-all",
            &["config receive.unpackLimit 1", "config core.fsync all"],
            &commands,
        )?;
        compare(
            "keep-fsync-none",
            &["config receive.unpackLimit 1", "config core.fsync none"],
            &commands,
        )
    }
}
