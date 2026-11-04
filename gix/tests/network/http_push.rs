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
    let setup = Setup::new("sha1")?;
    prepare_remote(&setup.path("remote.git"))?;
    let peer = HttpPeer::start(Config {
        project_root: setup.path("."),
        ..Default::default()
    })?;
    let repo = setup.work()?;
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
    let arrived = remote.find_reference("refs/heads/pushed")?.id() == head_id;
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
