//! The fake `ssh` against `git` as the client, to show that it is a faithful stand-in before other clients are tried against it.
#![cfg(unix)]
use gix_testtools::{
    Creation, TestResult,
    fake_ssh::{Behavior, FakeSsh},
};

fn setup() -> TestResult<gix_testtools::tempfile::TempDir> {
    Ok(gix_testtools::scripted_fixture_writable_with_args(
        "make_http_peer_repos.sh",
        ["sha1"],
        Creation::CopyFromReadOnly,
    )?)
}

fn ls_remote(dir: &std::path::Path, ssh: &FakeSsh, url: &str) -> std::io::Result<std::process::Output> {
    gix_testtools::git_command(dir)
        .env("GIT_SSH", ssh.command())
        .args(["ls-remote", url])
        .output()
}

#[test]
fn git_runs_its_server_side_through_the_fake_and_the_fake_is_isolated_from_the_client_environment() -> TestResult {
    let dir = setup()?;
    let ssh = FakeSsh::new(Behavior::RunCommand)?;
    let url = format!("ssh://host.example{}/remote.git", dir.path().display());
    let out = gix_testtools::git_command(dir.path())
        .env("GIT_SSH", ssh.command())
        // A variable of the client that must not reach the server side, as real `ssh` doesn't send it.
        .env("GIT_DIR", dir.path().join("does-not-exist"))
        .args(["-c", "protocol.version=2", "ls-remote", &url])
        .output()?;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("refs/heads/main"));
    let invocations = ssh.invocations();
    assert_eq!(invocations.len(), 1);
    let invocation = &invocations[0];
    assert!(
        invocation
            .remote_command()
            .is_some_and(|cmd| cmd.contains("git-upload-pack")),
        "{invocation:?}"
    );
    assert_eq!(
        invocation.env("GIT_PROTOCOL"),
        Some("version=2"),
        "the protocol version arrives in the way a server would see it"
    );
    Ok(())
}

#[test]
fn permission_denied_fails_the_client_like_a_refused_key() -> TestResult {
    let dir = setup()?;
    let ssh = FakeSsh::new(Behavior::PermissionDenied)?;
    let url = format!("ssh://host.example{}/remote.git", dir.path().display());
    let out = ls_remote(dir.path(), &ssh, &url)?;
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Permission denied"),
        "the message of ssh is passed on: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}
