//! The receiving side, compared with `git receive-pack` as the oracle for what a server advertises, accepts and reports.
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
};

use bstr::ByteSlice;
use gix_hash::{Kind, ObjectId};
use gix_protocol::receive::{Capabilities, Reference};
use gix_testtools::TestResult;

mod advertise;
mod driver;
mod report;
mod request;

/// Repositories made by `make_receive_remotes.sh`, see there for what they contain.
pub struct Fixture {
    pub kind: Kind,
    pub root: PathBuf,
    _guard: Option<gix_testtools::tempfile::TempDir>,
}

impl Fixture {
    pub fn remote(&self) -> PathBuf {
        self.root.join("remote.git")
    }
    /// The id of `rev` in the repository at `dir`.
    pub fn rev(&self, dir: &Path, rev: &str) -> TestResult<ObjectId> {
        Ok(ObjectId::from_hex(
            gix_testtools::git(dir, &format!("rev-parse {rev}"))?.trim().as_bytes(),
        )?)
    }
}

/// Fixtures for every enabled hash that must not be modified.
pub fn read_only() -> TestResult<Vec<Fixture>> {
    Kind::all()
        .iter()
        .map(|&kind| {
            Ok(Fixture {
                kind,
                root: gix_testtools::scripted_fixture_read_only_with_args(
                    "make_receive_remotes.sh",
                    [kind.to_string()],
                )?,
                _guard: None,
            })
        })
        .collect()
}

/// Fixtures for every enabled hash that can be modified.
pub fn writable() -> TestResult<Vec<Fixture>> {
    Kind::all()
        .iter()
        .map(|&kind| {
            let dir = gix_testtools::scripted_fixture_writable_with_args(
                "make_receive_remotes.sh",
                [kind.to_string()],
                gix_testtools::Creation::CopyFromReadOnly,
            )?;
            Ok(Fixture {
                kind,
                root: dir.path().to_owned(),
                _guard: Some(dir),
            })
        })
        .collect()
}

/// What `git receive-pack` did.
pub struct Run {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub success: bool,
}

/// Run `git receive-pack` for `repo` with `config` as `-c` options, `protocol` as `GIT_PROTOCOL` and `stdin` as what the client sends.
pub fn receive_pack(repo: &Path, config: &[&str], protocol: Option<&str>, stdin: &[u8]) -> TestResult<Run> {
    let mut cmd = gix_testtools::git_command(repo);
    for config in config {
        cmd.args(["-c", config]);
    }
    cmd.arg("receive-pack").arg(std::fs::canonicalize(repo)?);
    if let Some(protocol) = protocol {
        cmd.env("GIT_PROTOCOL", protocol);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Git may stop reading early, which is fine.
    let _ = child.stdin.take().expect("piped").write_all(stdin);
    let output = child.wait_with_output()?;
    Ok(Run {
        stdout: output.stdout,
        stderr: output.stderr.to_str_lossy().into_owned(),
        success: output.status.success(),
    })
}

/// A data packet with `line` as its payload.
pub fn pkt(line: impl AsRef<[u8]>) -> Vec<u8> {
    let line = line.as_ref();
    let mut out = format!("{:04x}", line.len() + 4).into_bytes();
    out.extend_from_slice(line);
    out
}

pub const FLUSH: &[u8] = b"0000";

/// An id made of `digit` repeated, for `kind`.
pub fn hex_of(kind: Kind, digit: char) -> String {
    digit.to_string().repeat(kind.len_in_hex())
}

/// The references of the repository at `dir` as Git advertises them, which is all of them in the order of their names.
pub fn references(dir: &Path) -> TestResult<Vec<Reference>> {
    gix_testtools::git(dir, "for-each-ref --format='%(objectname) %(refname)'")?
        .lines()
        .map(|line| {
            let (id, name) = line.split_once(' ').expect("an id and a name");
            Ok(Reference::new(name, ObjectId::from_hex(id.as_bytes())?))
        })
        .collect()
}

/// The ids in the shallow file of the repository at `dir`, if there is one.
pub fn shallow(dir: &Path) -> TestResult<Vec<ObjectId>> {
    let path = ["shallow", ".git/shallow"]
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file());
    path.map_or(Ok(Vec::new()), |path| {
        std::fs::read_to_string(path)?
            .lines()
            .map(|line| Ok(ObjectId::from_hex(line.as_bytes())?))
            .collect()
    })
}

/// The value of the capability `name` in what Git advertised in `advertisement`.
pub fn capability_value(advertisement: &[u8], name: &str) -> Option<String> {
    let first_line = advertisement.lines().find(|line| line.contains(&0))?;
    let capabilities = &first_line[first_line.find_byte(0)? + 1..];
    capabilities
        .to_str_lossy()
        .split(' ')
        .find_map(|capability| capability.strip_prefix(&format!("{name}=")).map(ToOwned::to_owned))
}

/// Capabilities like Git advertised them in `advertisement` when configured with `config`.
pub fn capabilities(kind: Kind, advertisement: &[u8], config: &[&str]) -> Capabilities {
    let mut capabilities = Capabilities::new(kind, capability_value(advertisement, "agent").expect("an agent"));
    capabilities.push_options = config.contains(&"receive.advertisePushOptions=true");
    capabilities.atomic = !config.contains(&"receive.advertiseAtomic=false");
    capabilities.ofs_delta = !config.contains(&"repack.useDeltaBaseOffset=false");
    capabilities.session_id = capability_value(advertisement, "session-id");
    capabilities
}

/// A pack without objects, as Git produces it for the repository at `dir`.
pub fn empty_pack(dir: &Path) -> TestResult<Vec<u8>> {
    let output = gix_testtools::git_command(dir)
        .args(["pack-objects", "--stdout"])
        .stdin(Stdio::null())
        .output()?;
    assert!(output.status.success(), "{output:?}");
    Ok(output.stdout)
}

/// Split `bytes` into the payloads of its packet lines up to and including the first flush packet, which is returned
/// as `None`, and the bytes after it.
pub fn split_at_flush(bytes: &[u8]) -> (Vec<&[u8]>, &[u8]) {
    let mut lines = Vec::new();
    let mut rest = bytes;
    loop {
        let len = usize::from_str_radix(std::str::from_utf8(&rest[..4]).expect("hex"), 16).expect("hex length");
        rest = &rest[4..];
        if len == 0 {
            return (lines, rest);
        }
        lines.push(&rest[..len - 4]);
        rest = &rest[len - 4..];
    }
}
