//! Pushes over HTTP that really change a repository, with each backend, against the `git http-backend` that the synthetic peer runs.
use std::{io::Write, sync::atomic::AtomicBool};

use gix::{protocol::transport::client::blocking_io::http, remote::push::Update};
use gix_testtools::{
    TestResult,
    http_peer::{Config, HttpPeer},
};

use crate::{Setup, for_each_backend, isolated, transport};

/// Push a new branch at the tip of `work`, after `prepare_remote` changed `remote.git`, and return what the push reported with the
/// requests that the peer saw.
fn push_new_branch<H: http::Http + Default + 'static>(
    prepare_remote: impl FnOnce(&std::path::Path) -> TestResult,
) -> TestResult<(gix::remote::push::Outcome, Vec<gix_testtools::http_peer::Request>, bool)> {
    push_new_branch_with::<H>(prepare_remote, |_| Ok(()))
}

/// Like [`push_new_branch()`], but with `configure_client` changing the configuration of the repository that pushes.
fn push_new_branch_with<H: http::Http + Default + 'static>(
    prepare_remote: impl FnOnce(&std::path::Path) -> TestResult,
    configure_client: impl FnOnce(&mut gix::config::SnapshotMut<'_>) -> TestResult,
) -> TestResult<(gix::remote::push::Outcome, Vec<gix_testtools::http_peer::Request>, bool)> {
    let setup = Setup::new("sha1")?;
    prepare_remote(&setup.path("remote.git"))?;
    let peer = HttpPeer::start(Config {
        project_root: setup.path("."),
        ..Default::default()
    })?;
    let mut repo = setup.work()?;
    configure_client(&mut repo.config_snapshot_mut())?;
    let head_id = repo.head_id()?.detach();
    let url = peer.url("remote.git");
    let outcome = repo
        .remote_at(url.as_str())?
        .to_connection_with_transport(transport::<H>(&url)?)
        .prepare_push(
            gix::progress::Discard,
            vec![Update::create(
                gix::refs::FullName::try_from("refs/heads/pushed")?,
                head_id,
            )],
            Default::default(),
        )?
        .send(&AtomicBool::new(false))?;
    let remote = gix::open_opts(setup.path("remote.git"), gix::open::Options::isolated())?;
    let arrived = remote
        .try_find_reference("refs/heads/pushed")?
        .is_some_and(|reference| reference.id() == head_id);
    Ok((outcome, peer.requests(), arrived))
}

fn a_push_creates_a_branch<H: http::Http + Default + 'static>() -> TestResult {
    let (outcome, requests, arrived) = push_new_branch::<H>(|_| Ok(()))?;
    assert!(outcome.is_success(), "the server accepted the update: {outcome:?}");
    assert!(arrived, "the branch exists in the pushed-to repository");
    let upload = requests.last().expect("the push sent requests");
    assert!(
        upload.body_len > 4,
        "the request that carries the commands and the pack has more than the flush packet of the handshake probe, \
         which a body that kept the size of the earlier request wouldn't: {} bytes",
        upload.body_len
    );
    Ok(())
}

#[test]
fn a_push_over_http_changes_the_repository_with_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_backend!(a_push_creates_a_branch());
    Ok(())
}

/// `git receive-pack` sends an empty packet on the progress band every `receive.keepAlive` seconds while a hook is silent, so that
/// networks don't drop the connection. The peer relays the output of the server once it is complete, so this checks that the
/// report is still found between these packets, but not that the limits of the transfer are met while the hook runs.
fn a_slow_hook_with_keep_alive_packets_still_reports<H: http::Http + Default + 'static>() -> TestResult {
    let (outcome, requests, arrived) = push_new_branch::<H>(|remote| {
        use std::os::unix::fs::PermissionsExt;
        let hook = remote.join("hooks/pre-receive");
        std::fs::write(&hook, "#!/bin/sh\nsleep 2\n")?;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
        let mut config = std::fs::OpenOptions::new().append(true).open(remote.join("config"))?;
        writeln!(config, "[receive]\n\tkeepAlive = 1")?;
        Ok(())
    })?;
    let response = &requests.last().expect("the push sent requests").response_body;
    assert!(
        response.windows(5).any(|packet| packet == b"0005\x01"),
        "the server sent keep-alive packets while the hook slept, which this test is about"
    );
    assert!(
        outcome.is_success(),
        "the report is read despite the keep-alive packets: {outcome:?}"
    );
    assert!(arrived, "the branch exists in the pushed-to repository");
    Ok(())
}

#[test]
#[cfg(unix)]
fn keep_alive_packets_of_a_slow_pre_receive_hook_are_read_with_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_backend!(a_slow_hook_with_keep_alive_packets_still_reports());
    Ok(())
}

fn install_silent_hook(remote: &std::path::Path) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let hook = remote.join("hooks/pre-receive");
    std::fs::write(&hook, "#!/bin/sh\nsleep 5\n")?;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// The limits count what arrives, so a server that says nothing while a hook runs ends the push when it is silent for as long as
/// they allow. `git push` with `curl` does the same, and that is the behavior that the backends have to agree with.
#[test]
#[cfg(unix)]
fn a_silent_hook_ends_a_push_with_the_limits_as_it_does_for_git() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let setup = Setup::new("sha1")?;
    install_silent_hook(&setup.path("remote.git"))?;
    let peer = HttpPeer::start(Config {
        project_root: setup.path("."),
        ..Default::default()
    })?;
    let output = gix_testtools::git_command(setup.path("work"))
        // The message is compared as Git words it in English.
        .env("LC_ALL", "C")
        .args([
            "-c",
            "http.lowSpeedLimit=1000",
            "-c",
            "http.lowSpeedTime=2",
            "push",
            &peer.url("remote.git"),
            "HEAD:refs/heads/pushed",
        ])
        .output()?;
    assert!(!output.status.success(), "Git gives up on the silent server");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Operation too slow"),
        "Git's reason is the speed of the transfer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn a_silent_hook_is_ended_by_the_limits<H: http::Http + Default + 'static>() -> TestResult {
    let (outcome, _, _) = push_new_branch_with::<H>(install_silent_hook, |config| {
        config.set_raw_value("http.lowSpeedLimit", "1000")?;
        config.set_raw_value("http.lowSpeedTime", "2")?;
        Ok(())
    })?;
    assert!(
        outcome.is_uncertain(),
        "the request was sent in full and the limits cut the wait for the response, so the server may still act on it: {outcome:?}"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn a_silent_hook_ends_a_push_with_the_limits_with_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_backend!(a_silent_hook_is_ended_by_the_limits());
    Ok(())
}
