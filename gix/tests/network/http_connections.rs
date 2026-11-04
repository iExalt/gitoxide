//! How the HTTP backends deal with connections that the server keeps alive and then closes, which a `POST` can't recover from.
use gix::protocol::transport::{Service, client::blocking_io::http};
use gix_testtools::{
    TestResult,
    http_peer::{Config, HttpPeer},
};
use std::time::Duration;

use crate::{HASHES, Setup, for_each_backend, isolated, push_handshake_and_flush, transport};

fn connections_are_reused_while_alive_and_replaced_after_the_server_closed_them<H: http::Http + Default + 'static>(
    hash: &'static str,
) -> TestResult {
    const IDLE: Duration = Duration::from_millis(200);
    let setup = Setup::new(hash)?;
    let peer = HttpPeer::start(Config {
        project_root: setup.path("."),
        keep_alive_idle: Some(IDLE),
        ..Default::default()
    })?;
    let client = setup.work()?;
    let url = peer.url("remote.git");
    let mut transport = transport::<H>(&url)?;
    let advertisement = |transport: &mut http::Transport<H>, service| {
        gix::protocol::handshake(
            transport,
            service,
            |_| Ok(None),
            Vec::new(),
            &mut gix::progress::Discard,
        )
    };

    advertisement(&mut transport, Service::UploadPack)?;
    push_handshake_and_flush(&client, &mut transport)?;
    let connections: Vec<_> = peer.requests().iter().map(|req| req.connection).collect();
    assert!(
        connections.len() > 2 && connections.iter().all(|connection| *connection == connections[0]),
        "{hash}: the connection is reused while the server keeps it alive: {connections:?}"
    );
    let before = connections.len();

    // The server closes the connection while the client isn't looking. The next request must not be sent into it.
    std::thread::sleep(IDLE * 3);
    push_handshake_and_flush(&client, &mut transport)?;
    let requests = peer.requests();
    let connections: Vec<_> = requests[before..].iter().map(|req| req.connection).collect();
    assert!(
        connections.iter().all(|connection| *connection != 0),
        "{hash}: a new connection serves the request after the old one was closed: {connections:?}"
    );
    assert!(
        requests[before..].iter().all(|req| req.status == 200),
        "{hash}: the push didn't fail: {requests:?}"
    );
    Ok(())
}

#[test]
fn a_push_after_the_server_closed_an_idle_connection_succeeds() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for hash in HASHES {
        for_each_backend!(connections_are_reused_while_alive_and_replaced_after_the_server_closed_them(hash));
    }
    Ok(())
}
