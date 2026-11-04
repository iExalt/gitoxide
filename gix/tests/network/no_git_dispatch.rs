//! Fetching and pushing over HTTP with authentication and over `ssh` never runs `git` or one of its helpers in the client.
//!
//! Programs that stand in for `git` and for the helpers of Git that a client could reach through `PATH` leave a trace when run. The
//! servers that the test starts, the peer and the program that stands in for `ssh`, find Git before the programs are put in place,
//! as they are infrastructure of the test and not part of what is checked.
use std::{path::Path, sync::atomic::AtomicBool};

use gix::{protocol::transport::client::blocking_io::http, remote::Direction, remote::push::Update};
use gix_testtools::{
    Env, TestResult,
    fake_ssh::{Behavior, FakeSsh},
};

use crate::{Helper, PASSWORD, Protect, Setup, USER, for_each_backend, isolated, transport};

fn put_shims_in_front(dir: &Path) -> TestResult<(Env<'static>, std::path::PathBuf)> {
    use std::os::unix::fs::PermissionsExt;
    let shims = dir.join("shims");
    std::fs::create_dir(&shims)?;
    let log = dir.join("git-was-run");
    let shim = format!("#!/bin/sh\necho \"$0 $@\" >> '{}'\nexit 1\n", log.display());
    for name in [
        "git",
        "git-credential-store",
        "git-credential-cache",
        "git-credential-osxkeychain",
        "git-credential-libsecret",
        "git-credential-manager",
        "git-receive-pack",
        "git-upload-pack",
        "git-remote-http",
        "git-remote-https",
    ] {
        let path = shims.join(name);
        std::fs::write(&path, &shim)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    let path = format!("{}:{}", shims.display(), std::env::var("PATH").unwrap_or_default());
    Ok((Env::new().set("PATH", path), log))
}

fn clients_never_run_git<H: http::Http + Default + 'static>() -> TestResult {
    let http_setup = Setup::new("sha1")?;
    let ssh_setup = Setup::new("sha1")?;
    let peer = http_setup.peer(Protect::Everything)?;
    let ssh = FakeSsh::new(Behavior::RunCommand)?;
    let dir = gix_testtools::tempfile::tempdir()?;
    let (_shims, log) = put_shims_in_front(dir.path())?;

    // HTTP, with credentials from a helper that is a program of its own, and the native credential cascade around it.
    {
        let helper = Helper::new(&http_setup.path("."), USER, PASSWORD)?;
        let mut repo = http_setup.work()?;
        helper.configure(&mut repo)?;
        let head_id = repo.head_id()?.detach();
        let url = peer.url("remote.git");
        let outcome = repo
            .remote_at(url.as_str())?
            .to_connection_with_transport(transport::<H>(&url)?)
            .prepare_push(
                gix::progress::Discard,
                vec![Update::create(
                    gix::refs::FullName::try_from("refs/heads/via-http")?,
                    head_id,
                )],
                Default::default(),
            )?
            .send(&AtomicBool::new(false))?;
        assert!(outcome.is_success(), "{outcome:?}");

        let mut dest = http_setup.dest()?;
        helper.configure(&mut dest)?;
        let remote = dest
            .remote_at(url.as_str())?
            .with_refspecs(Some("+refs/heads/*:refs/remotes/origin/*"), Direction::Fetch)?;
        remote
            .to_connection_with_transport(transport::<H>(&url)?)
            .prepare_fetch(gix::progress::Discard, Default::default())?
            .receive(gix::progress::Discard, &AtomicBool::new(false))?;
        assert!(
            dest.try_find_reference("refs/remotes/origin/via-http")?.is_some(),
            "the fetch brought in what the push sent"
        );
        assert!(!helper.actions().is_empty(), "the helper was asked for credentials");
    }

    // ssh, where the program that stands in for `ssh` runs the server side.
    {
        let helper = Helper::new(&ssh_setup.path("."), USER, PASSWORD)?;
        let mut repo = ssh_setup.work()?;
        helper.configure(&mut repo)?;
        repo.config_snapshot_mut()
            .set_raw_value("core.sshCommand", ssh.command().display().to_string().as_str())?;
        let head_id = repo.head_id()?.detach();
        let url = format!("ssh://host.example{}/remote.git", ssh_setup.path(".").display());
        let outcome = repo
            .remote_at(url.as_str())?
            .connect(Direction::Push)?
            .prepare_push(
                gix::progress::Discard,
                vec![Update::create(
                    gix::refs::FullName::try_from("refs/heads/via-ssh")?,
                    head_id,
                )],
                Default::default(),
            )?
            .send(&AtomicBool::new(false))?;
        assert!(outcome.is_success(), "{outcome:?}");
    }

    assert!(
        !log.exists(),
        "a client ran `git` or one of its helpers: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    // The shims are what a program that runs `git` by name would find, which this shows by doing it.
    std::process::Command::new("git").arg("canary").status()?;
    assert!(
        log.exists(),
        "the shim of `git` is found first, so the checks above could have seen a run"
    );
    Ok(())
}

#[test]
fn fetching_and_pushing_over_http_and_ssh_never_runs_git_in_the_client_with_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_backend!(clients_never_run_git());
    Ok(())
}
