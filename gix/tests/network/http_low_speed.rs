//! Transfers that are too slow are ended by the low-speed limits, wherever the server stops: before it sends headers, in the
//! middle of the body, or by trickling it. The same cases run with each backend, as they have to agree.
use std::{
    io::Read,
    time::{Duration, Instant},
};

use gix::{
    protocol::transport::{
        Service,
        client::{
            MessageKind, TransportWithoutIO, WriteMode,
            blocking_io::{Transport, http},
        },
    },
    remote::Direction,
};
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

#[cfg(feature = "blocking-http-transport-reqwest")]
fn upload_stall_is_given_up_on<H: http::Http + Default + 'static>() -> TestResult {
    use std::io::Write as _;
    let setup = Setup::new("sha1")?;
    // Only the `POST` stalls, so that the advertisement arrives.
    let peer = HttpPeer::start(Config {
        project_root: setup.path("."),
        fault: Fault::StopReadingRequestBody,
        fault_posts_only: true,
        receive_buffer: Some(4096),
        ..Default::default()
    })?;
    let mut transport = transport::<H>(&peer.url("remote.git"))?;
    transport.configure(&http::Options {
        low_speed_limit_bytes_per_second: 1000,
        low_speed_time_seconds: 1,
        ..Default::default()
    })?;
    transport.handshake(Service::UploadPack, &[])?;
    let mut writer = transport.request(WriteMode::Binary, MessageKind::Flush, false)?;
    let started = Instant::now();
    // More than what the kernel buffers on both sides, so that the upload has to wait for the server, which doesn't read.
    let chunk = vec![0u8; 1024 * 1024];
    let mut written = 0usize;
    loop {
        if writer.write_all(&chunk).is_err() {
            break;
        }
        written += chunk.len();
        assert!(
            written < 512 * 1024 * 1024,
            "a server that doesn't read can't take this much"
        );
    }
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the upload ended long before the 30 s of the old timeouts: {:?}",
        started.elapsed()
    );
    let (_writer, mut reader) = writer.into_parts();
    let err = reader.read_to_end(&mut Vec::new()).expect_err("the request failed");
    assert!(
        is_low_speed_error(&gix::Error::from_error(err)),
        "the reason that the upload ended is the speed of the transfer"
    );
    Ok(())
}

/// A consumer that doesn't read for longer than the limits allow, and then reads everything quickly, doesn't make a transfer slow:
/// the time that the transfer waits for the consumer isn't held against the server.
fn a_consumer_that_reads_late_doesnt_trip_the_limits<H: http::Http + Default + 'static>() -> TestResult {
    const PADDING: u64 = 16 * 1024 * 1024;
    let setup = Setup::new("sha1")?;
    // The advertisement is served as it is, and the response to the request after it is followed by more data than the
    // kernel and the backends buffer, so the server has to wait for the consumer.
    let peer = HttpPeer::start(Config {
        project_root: setup.path("."),
        fault: Fault::PaddedResponse { bytes: PADDING },
        fault_after_requests: 1,
        ..Default::default()
    })?;
    let mut transport = transport::<H>(&peer.url("remote.git"))?;
    transport.configure(&http::Options {
        low_speed_limit_bytes_per_second: 1000,
        low_speed_time_seconds: 1,
        ..Default::default()
    })?;
    transport.handshake(Service::UploadPack, &[])?;
    let writer = transport.request(WriteMode::Binary, MessageKind::Flush, false)?;
    let (raw_writer, mut reader) = writer.into_parts();
    // The request is complete once its body ends.
    drop(raw_writer);
    std::thread::sleep(Duration::from_secs(3));
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    let end = loop {
        match reader.read(&mut buf) {
            Ok(0) => break None,
            Ok(n) => total += n as u64,
            Err(err) => break Some(gix::Error::from_error(err)),
        }
    };
    // The padding is not a whole response, so the reader of packet lines may complain about its end, which is fine.
    if let Some(err) = end {
        assert!(
            !is_low_speed_error(&err),
            "the stall of the consumer was held against the server: {err:?}"
        );
    }
    assert!(
        total >= PADDING / 2, // half of it is payload, the rest are the lengths of the packet lines
        "all the padding arrived after the consumer woke up: {total} bytes"
    );
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

            #[test]
            fn a_consumer_that_reads_late_is_not_a_slow_server() -> TestResult {
                let Some(_environment) = isolated()? else {
                    return Ok(());
                };
                a_consumer_that_reads_late_doesnt_trip_the_limits::<$backend>()
            }
        }
    };
}

backend_tests!(curl, http::curl::Curl, "blocking-http-transport-curl");
backend_tests!(reqwest, http::reqwest::Remote, "blocking-http-transport-reqwest");

/// A server that doesn't read the request ends an upload by the speed limits, as with the downloads above.
///
/// This runs with `reqwest` only. A stalled upload doesn't stop on loopback: the kernel keeps accepting about 64 KiB a second of what
/// `curl` sends, which is far above the limits, for tens of seconds with the defaults and for at least 20 with the smallest receive
/// buffer. `curl` ends such an upload when the connection ends, as the interruption test of this target shows, but only
/// a real network stalls it in a way that its speed check sees.
#[cfg(feature = "blocking-http-transport-reqwest")]
#[test]
fn an_upload_that_the_server_doesnt_take_is_given_up_on() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    upload_stall_is_given_up_on::<http::reqwest::Remote>()
}
