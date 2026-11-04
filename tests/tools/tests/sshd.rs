//! The `sshd` peer against `git` as the client, to show that it is a server that clients can really use.
#![cfg(unix)]
use gix_testtools::{Creation, TestResult, sshd::Sshd};

fn sshd() -> TestResult<Option<Sshd>> {
    let server = Sshd::start()?;
    assert!(
        server.is_some() || !gix_testtools::is_ci::cached(),
        "CI has `sshd`, `ssh` and `ssh-keygen`, and doesn't run as root"
    );
    if server.is_none() {
        eprintln!("SKIPPED: `sshd` isn't installed, or the user is root");
    }
    Ok(server)
}

fn ls_remote(dir: &std::path::Path, config: &std::path::Path, url: &str) -> std::io::Result<std::process::Output> {
    gix_testtools::git_command(dir)
        .env("GIT_SSH_COMMAND", Sshd::command(config))
        .env("GIT_TRACE_PACKET", "1")
        .args(["-c", "protocol.version=2", "ls-remote", url])
        .output()
}

#[test]
fn git_logs_in_with_the_authorized_key_and_is_refused_with_another() -> TestResult {
    let Some(server) = sshd()? else {
        return Ok(());
    };
    let dir = gix_testtools::scripted_fixture_writable_with_args(
        "make_http_peer_repos.sh",
        ["sha1"],
        Creation::CopyFromReadOnly,
    )?;
    let url = Sshd::url(&dir.path().join("remote.git"));

    let out = ls_remote(dir.path(), &server.client_config(), &url)?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("refs/heads/main"));
    assert!(
        stderr.contains("version 2"),
        "the server received `GIT_PROTOCOL` as it accepts it, so it speaks protocol version 2: {stderr}"
    );

    assert_eq!(
        server.server_side_git_exec_paths(),
        [gix_path::env::core_dir()
            .expect("git has an exec path")
            .display()
            .to_string()],
        "the commands of clients are run by the Git that the test uses, and not by what the shell of the user finds"
    );

    let out = ls_remote(dir.path(), &server.client_config_with_unknown_key(), &url)?;
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Permission denied"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}
