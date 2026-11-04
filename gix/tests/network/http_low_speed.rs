//! Transfers that are too slow are ended by the low-speed limits, wherever the server stops: before it sends headers, in the
//! middle of the body, or by trickling it. The same cases run with each backend, as they have to agree.
use std::time::{Duration, Instant};

use gix::{protocol::transport::client::blocking_io::http, remote::Direction};
use gix_testtools::{
    TestResult,
    http_peer::{Config, Fault, HttpPeer},
};

use crate::{Setup, isolated, transport};

/// Listing the references of the remote with the limits `limit` bytes per second for `time` seconds, which returns how long it took.
fn ref_map<H: http::Http + Default + 'static>(
    setup: &Setup,
    peer: &HttpPeer,
    limit: &str,
    time: &str,
) -> (Instant, Result<(), gix::Error>) {
    let mut dest = setup.dest().expect("the fixture repository can be opened");
    {
        let mut config = dest.config_snapshot_mut();
        config.set_raw_value("http.lowSpeedLimit", limit).expect("valid key");
        config.set_raw_value("http.lowSpeedTime", time).expect("valid key");
    }
    let url = peer.url("remote.git");
    let started = Instant::now();
    let result = (|| {
        let remote = dest
            .remote_at(url.as_str())?
            .with_refspecs(Some("+refs/heads/*:refs/remotes/origin/*"), Direction::Fetch)?;
        remote
            .to_connection_with_transport(transport::<H>(&url).expect("a valid URL"))
            .ref_map(gix::progress::Discard, Default::default())?;
        Ok(())
    })();
    (started, result)
}

fn peer(setup: &Setup, fault: Fault) -> TestResult<HttpPeer> {
    Ok(HttpPeer::start(Config {
        project_root: setup.path("."),
        fault,
        ..Default::default()
    })?)
}

/// The message of the error that the low-speed limits end a transfer with, in the words of each backend.
fn is_low_speed_error(err: &gix::Error) -> bool {
    let text = format!("{err:?}");
    text.contains("slower than") || text.contains("too slow") || text.contains("Operation too slow")
}

fn a_server_that_stalls_is_given_up_on<H: http::Http + Default + 'static>(fault: Fault, what: &str) -> TestResult {
    let setup = Setup::new("sha1")?;
    let peer = peer(&setup, fault)?;
    let (started, result) = ref_map::<H>(&setup, &peer, "1000", "1");
    let err = result.expect_err("the server never finishes the response");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{what}: the transfer ended long before the 30 s of the old timeouts, after {:?}",
        started.elapsed()
    );
    assert!(
        is_low_speed_error(&err),
        "{what}: the reason is the speed of the transfer: {err:?}"
    );
    Ok(())
}

fn a_trickle_is_judged_by_its_speed<H: http::Http + Default + 'static>() -> TestResult {
    let setup = Setup::new("sha1")?;
    // 60 bytes per second, which takes a few seconds for the advertisement, longer than the limits wait for the abort.
    let trickle = Fault::SlowResponse {
        chunk: 6,
        interval: Duration::from_millis(100),
    };
    let peer = peer(&setup, trickle)?;
    let (_, result) = ref_map::<H>(&setup, &peer, "6", "5");
    result.map_err(|err| format!("a transfer that is ten times faster than the limit isn't stopped: {err:?}"))?;

    let (_, result) = ref_map::<H>(&setup, &peer, "600", "1");
    let err = result.expect_err("a transfer that is ten times slower than the limit is stopped");
    assert!(is_low_speed_error(&err), "{err:?}");
    Ok(())
}

/// Run each case with one backend as tests of their own, which lets them run in parallel.
macro_rules! backend_tests {
    ($module:ident, $backend:ty, $feature:literal) => {
        #[cfg(feature = $feature)]
        mod $module {
            use super::*;

            #[test]
            fn a_server_that_sends_no_headers_is_given_up_on() -> TestResult {
                let Some(_environment) = isolated()? else {
                    return Ok(());
                };
                a_server_that_stalls_is_given_up_on::<$backend>(Fault::Silent, "no response headers")
            }

            #[test]
            fn a_server_that_stops_in_the_middle_of_the_body_is_given_up_on() -> TestResult {
                let Some(_environment) = isolated()? else {
                    return Ok(());
                };
                a_server_that_stalls_is_given_up_on::<$backend>(Fault::StallAfterHeaders, "a stalled body")
            }

            #[test]
            fn a_slow_response_is_stopped_by_the_limit_and_not_by_its_duration() -> TestResult {
                let Some(_environment) = isolated()? else {
                    return Ok(());
                };
                a_trickle_is_judged_by_its_speed::<$backend>()
            }
        }
    };
}

backend_tests!(curl, http::curl::Curl, "blocking-http-transport-curl");
backend_tests!(reqwest, http::reqwest::Remote, "blocking-http-transport-reqwest");
