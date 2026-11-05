//! The serving side, compared with `git upload-pack` speaking protocol version 2 as the oracle for what a server advertises,
//! accepts and refuses.
use std::{io::Write, path::Path, process::Stdio};

use bstr::ByteSlice;
use gix_testtools::TestResult;

mod advertise;
mod request;
mod response;

pub use super::receive::{FLUSH, hex_of, pkt, read_only};

/// What `git upload-pack` did.
pub struct Run {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub success: bool,
}

/// Run `git upload-pack` for `repo` speaking version 2, with `config` as `-c` options and `stdin` as what the client sends.
pub fn upload_pack(repo: &Path, config: &[&str], stdin: &[u8]) -> TestResult<Run> {
    let mut cmd = gix_testtools::git_command(repo);
    for config in config {
        cmd.args(["-c", config]);
    }
    cmd.arg("upload-pack")
        .arg(std::fs::canonicalize(repo)?)
        .env("GIT_PROTOCOL", "version=2");
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

/// The data of all packet lines in `bytes`, with `None` for packets without data, which are flush, delimiter and response-end.
pub fn packets(mut bytes: &[u8]) -> Vec<Option<Vec<u8>>> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let len = usize::from_str_radix(std::str::from_utf8(&bytes[..4]).expect("hex"), 16).expect("a length");
        if len < 4 {
            out.push(None);
            bytes = &bytes[4..];
        } else {
            out.push(Some(bytes[4..len].to_vec()));
            bytes = &bytes[len..];
        }
    }
    out
}
