//! Fetching over HTTP without authentication, to show that each backend negotiates protocol version 2 and the object format.
use std::sync::atomic::AtomicBool;

use gix::{progress::Discard, protocol::transport::client::blocking_io::http, remote::Direction};
use gix_testtools::{TestResult, http_peer::Protect};

use crate::{HASHES, Setup, for_each_backend, isolated, transport};

fn fetch_case<H: http::Http + Default + 'static>(hash: &'static str) -> TestResult {
    let setup = Setup::new(hash)?;
    let peer = setup.peer(Protect::Nothing)?;
    let url = peer.url("remote.git");
    let dest = setup.dest()?;
    assert_eq!(
        dest.object_hash().to_string(),
        hash,
        "the destination uses the format that is tested"
    );

    let remote = dest
        .remote_at(url.as_str())?
        .with_refspecs(Some("+refs/heads/*:refs/remotes/origin/*"), Direction::Fetch)?;
    remote
        .to_connection_with_transport(transport::<H>(&url)?)
        .prepare_fetch(Discard, Default::default())?
        .receive(Discard, &AtomicBool::new(false))?;

    let expected = gix_testtools::git(setup.path("remote.git"), "rev-parse main")?;
    let tracking = dest
        .find_reference("refs/remotes/origin/main")?
        .into_fully_peeled_id()?;
    assert_eq!(
        tracking.to_string(),
        expected.trim(),
        "{hash}: the branch arrived with the objects it needs"
    );
    assert!(dest.find_object(tracking)?.kind.is_commit());

    let requests = peer.requests();
    let advertisement = requests
        .iter()
        .find(|req| req.target.contains("info/refs"))
        .expect("the advertisement was requested");
    assert_eq!(
        advertisement.header("git-protocol"),
        Some("version=2"),
        "{hash}: protocol version 2 is requested"
    );
    assert!(
        requests.iter().all(|req| req.header("authorization").is_none()),
        "{hash}: credentials are not sent to a peer that doesn't ask for them"
    );
    assert!(
        requests.iter().all(|req| req.status == 200),
        "{hash}: nothing was rejected: {requests:?}"
    );
    Ok(())
}

#[test]
fn fetch_with_protocol_v2_for_each_hash_and_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for hash in HASHES {
        for_each_backend!(fetch_case(hash));
    }
    Ok(())
}
