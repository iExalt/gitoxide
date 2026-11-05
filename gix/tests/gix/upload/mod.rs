//! Serving a repository to clients, compared with `git upload-pack` speaking protocol version 2 as the oracle: the same requests are
//! answered by Git and by the fork on the same repositories, for both hashes and both formats of references.
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::AtomicBool,
};

use gix_hash::Kind;
use gix_testtools::TestResult;

mod fetch;
mod ls_refs;

pub const FLUSH: &[u8] = b"0000";
pub const DELIM: &[u8] = b"0001";

/// Repositories made by `make_upload_repos.sh`, see there for what they contain.
pub struct Fixture {
    pub kind: Kind,
    pub format: &'static str,
    pub root: PathBuf,
}

impl Fixture {
    pub fn repo(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// A description of the fixture for assertion messages.
    pub fn label(&self) -> String {
        format!("{}/{}", self.kind, self.format)
    }

    pub fn rev(&self, name: &str, rev: &str) -> TestResult<String> {
        Ok(gix_testtools::git(self.repo(name), &format!("rev-parse {rev}"))?
            .trim()
            .to_owned())
    }
}

/// Fixtures for every enabled hash and each format of references.
pub fn fixtures() -> TestResult<Vec<Fixture>> {
    let mut out = Vec::new();
    for &kind in Kind::all() {
        for format in ["files", "reftable"] {
            out.push(Fixture {
                kind,
                format,
                root: gix_testtools::scripted_fixture_read_only_with_args(
                    "make_upload_repos.sh",
                    [kind.to_string(), format.to_owned()],
                )?,
            });
        }
    }
    Ok(out)
}

/// A data packet with `line` as its payload.
pub fn pkt(line: impl AsRef<[u8]>) -> Vec<u8> {
    let line = line.as_ref();
    let mut out = format!("{:04x}", line.len() + 4).into_bytes();
    out.extend_from_slice(line);
    out
}

/// What a server answered, split into the advertisement it sends first and everything that follows.
pub struct Answer {
    pub advertisement: Vec<u8>,
    pub response: Vec<u8>,
}

fn split_advertisement(bytes: &[u8]) -> Answer {
    let mut at = 0;
    while at + 4 <= bytes.len() {
        let len = usize::from_str_radix(std::str::from_utf8(&bytes[at..at + 4]).expect("hex"), 16).expect("length");
        at += len.max(4);
        if len == 0 {
            break;
        }
    }
    Answer {
        advertisement: bytes[..at].to_vec(),
        response: bytes[at..].to_vec(),
    }
}

/// How to set up a server for a request.
#[derive(Default, Clone)]
pub struct Setup<'a> {
    pub config: &'a [&'a str],
    pub namespace: Option<&'a str>,
}

/// Run `git upload-pack` on `repo` with `request`.
pub fn git_serve(repo: &Path, setup: &Setup<'_>, request: &[u8]) -> TestResult<Answer> {
    let mut cmd = gix_testtools::git_command(repo);
    for config in setup.config {
        cmd.args(["-c", config]);
    }
    cmd.arg("upload-pack")
        .arg(std::fs::canonicalize(repo)?)
        .env("GIT_PROTOCOL", "version=2");
    if let Some(namespace) = setup.namespace {
        cmd.env("GIT_NAMESPACE", namespace);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let _ = child.stdin.take().expect("piped").write_all(request);
    Ok(split_advertisement(&child.wait_with_output()?.stdout))
}

/// Serve `request` from `repo` with the fork, and return what it wrote along with how it ended.
pub fn fork_serve(repo: &Path, setup: &Setup<'_>, request: &[u8]) -> TestResult<(Answer, gix::Result<()>)> {
    let mut repository = gix::open_opts(
        repo,
        gix::open::Options::isolated().config_overrides(setup.config.iter().copied()),
    )?;
    if let Some(namespace) = setup.namespace {
        repository.set_namespace(namespace)?;
    }
    let mut out = Vec::new();
    let mut input = request;
    let result = repository
        .upload()
        .and_then(|server| Ok(server.serve(&mut input, &mut out, &AtomicBool::new(false))?));
    Ok((split_advertisement(&out), result))
}

/// The request for the references with `args`, to a repository with objects of `kind`.
pub fn ls_refs_request(kind: Kind, args: &[&str]) -> Vec<u8> {
    command_request("ls-refs", kind, args)
}

pub fn command_request(command: &str, kind: Kind, args: &[&str]) -> Vec<u8> {
    let mut out = pkt(format!("command={command}\n"));
    out.extend(pkt(format!("object-format={kind}\n")));
    out.extend_from_slice(DELIM);
    for arg in args {
        out.extend(pkt(format!("{arg}\n")));
    }
    out.extend_from_slice(FLUSH);
    out
}
