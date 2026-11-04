//! Cancellation reaches I/O that blocks on a peer that doesn't answer: a handle obtained from the transport ends it from another
//! thread, as promptly as the transport can tell, whatever the transport is.
use std::time::{Duration, Instant};

use std::io::{Read, Write};

use gix::protocol::transport::{
    Protocol, Service,
    client::{
        MessageKind, TransportWithoutIO, WriteMode,
        blocking_io::{Transport, http, ssh},
    },
};
use gix_testtools::{
    TestResult,
    fake_ssh::{Behavior, FakeSsh},
    http_peer::{Config, Fault, HttpPeer},
};

use crate::{Setup, for_each_backend, isolated, transport};

/// The time that a stalled peer is given to end what its client does, which is far less than the time that it would otherwise keep it.
const PROMPTLY: Duration = Duration::from_secs(10);

fn interrupt_after(
    handle: gix::protocol::transport::client::InterruptHandle,
    delay: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        handle.interrupt();
    })
}

fn ssh_transport(ssh: &FakeSsh) -> TestResult<impl Transport + TransportWithoutIO> {
    Ok(ssh::connect(
        gix::url::parse("ssh://host.example/remote.git")?,
        Protocol::V2,
        ssh::connect::Options {
            command: Some(ssh.command().into()),
            ..Default::default()
        },
        false,
    )?)
}

#[test]
fn an_interrupt_ends_the_handshake_with_a_server_that_says_nothing() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let ssh = FakeSsh::new(Behavior::Hang(120))?;
    let mut transport = ssh_transport(&ssh)?;
    let interrupter = interrupt_after(
        transport.interrupt_handle().expect("processes can be ended"),
        Duration::from_millis(300),
    );
    let start = Instant::now();
    let result = transport.handshake(Service::UploadPack, &[]);
    interrupter.join().expect("the thread doesn't panic");
    assert!(
        result.is_err(),
        "the server never answered, so the handshake can't have succeeded"
    );
    assert!(
        start.elapsed() < PROMPTLY,
        "the handshake ended when it was interrupted, not when the server finished: {:?}",
        start.elapsed()
    );
    assert_eq!(ssh.invocations().len(), 1, "the program was started once");
    Ok(())
}

#[test]
fn a_transport_that_was_interrupted_starts_nothing() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let ssh = FakeSsh::new(Behavior::RunCommand)?;
    let mut transport = ssh_transport(&ssh)?;
    transport
        .interrupt_handle()
        .expect("processes can be ended")
        .interrupt();
    let Err(err) = transport.handshake(Service::UploadPack, &[]) else {
        panic!("an interrupted transport can't be used");
    };
    assert!(
        format!("{err:?}").contains("interrupted"),
        "the failure says what happened: {err:?}"
    );
    assert!(ssh.invocations().is_empty(), "no process is started after an interrupt");
    Ok(())
}

#[test]
fn an_interrupt_ends_the_handshake_with_a_daemon_that_says_nothing() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    // Accept the connection, and keep it open without a word until the test ends.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let _connection = listener.accept();
        done_rx.recv().ok();
    });
    let mut transport = gix::protocol::transport::client::git::blocking_io::connect(
        "127.0.0.1",
        "/remote.git".into(),
        Protocol::V2,
        Some(port),
        false,
    )?;
    let interrupter = interrupt_after(
        transport.interrupt_handle().expect("sockets can be closed"),
        Duration::from_millis(300),
    );
    let start = Instant::now();
    let result = transport.handshake(Service::UploadPack, &[]);
    interrupter.join().expect("the thread doesn't panic");
    done_tx.send(()).ok();
    server.join().expect("the thread doesn't panic");
    assert!(
        result.is_err(),
        "the daemon never answered, so the handshake can't have succeeded"
    );
    assert!(
        start.elapsed() < PROMPTLY,
        "the socket was closed when interrupted: {:?}",
        start.elapsed()
    );
    Ok(())
}

fn peer_with(setup: &Setup, fault: Fault, posts_only: bool) -> TestResult<HttpPeer> {
    Ok(HttpPeer::start(Config {
        project_root: setup.path("."),
        fault,
        fault_posts_only: posts_only,
        ..Default::default()
    })?)
}

fn is_cancelled(err: &dyn std::error::Error) -> bool {
    format!("{err:?}").contains("interrupted")
}

fn an_interrupt_ends_the_wait_for_a_server_that_doesnt_respond<H: http::Http + Default + 'static>() -> TestResult {
    let setup = Setup::new("sha1")?;
    let peer = peer_with(&setup, Fault::Silent, false)?;
    let mut transport = transport::<H>(&peer.url("remote.git"))?;
    let interrupter = interrupt_after(
        transport.interrupt_handle().expect("requests can be ended"),
        Duration::from_millis(300),
    );
    let start = Instant::now();
    let result = transport.handshake(Service::UploadPack, &[]).map(|_| ());
    interrupter.join().expect("the thread doesn't panic");
    let err = result.expect_err("a server that never responds can't have been shaken hands with");
    assert!(
        start.elapsed() < PROMPTLY,
        "the wait ended when interrupted: {:?}",
        start.elapsed()
    );
    assert!(
        is_cancelled(&err),
        "the failure says that the transport was interrupted: {err:?}"
    );
    // The transport can't be used anymore.
    assert!(
        transport.handshake(Service::UploadPack, &[]).is_err(),
        "an interrupted transport stays interrupted"
    );
    Ok(())
}

fn an_interrupt_ends_the_wait_for_a_response_body_that_stalls<H: http::Http + Default + 'static>() -> TestResult {
    let setup = Setup::new("sha1")?;
    let peer = peer_with(&setup, Fault::StallAfterHeaders, true)?;
    let mut transport = transport::<H>(&peer.url("remote.git"))?;
    let interrupter = interrupt_after(
        transport.interrupt_handle().expect("requests can be ended"),
        Duration::from_millis(300),
    );
    // The advertisement is served normally, and the response to the first request that follows has headers and then nothing.
    transport.handshake(Service::UploadPack, &[])?;
    let mut writer = transport.request(WriteMode::Binary, MessageKind::Flush, false)?;
    writer.write_all(b"0014command=ls-refs\n0000")?;
    let (_writer, mut reader) = writer.into_parts();
    let start = Instant::now();
    let mut sink = Vec::new();
    let result = reader.read_to_end(&mut sink);
    interrupter.join().expect("the thread doesn't panic");
    let err = result.expect_err("the response never completes");
    assert!(
        start.elapsed() < PROMPTLY,
        "the read ended when interrupted: {:?}",
        start.elapsed()
    );
    assert!(
        is_cancelled(&err),
        "the failure says that the transport was interrupted: {err:?}"
    );
    Ok(())
}

fn an_interrupt_ends_an_upload_that_the_server_doesnt_take<H: http::Http + Default + 'static>() -> TestResult {
    let setup = Setup::new("sha1")?;
    let peer = peer_with(&setup, Fault::StopReadingRequestBody, true)?;
    let mut transport = transport::<H>(&peer.url("remote.git"))?;
    let interrupter = interrupt_after(
        transport.interrupt_handle().expect("requests can be ended"),
        Duration::from_millis(1500),
    );
    // The upload of a push has the same shape. Its handshake isn't used here as it ends with a `POST`, which this peer doesn't answer.
    transport.handshake(Service::UploadPack, &[])?;
    let mut writer = transport.request(WriteMode::Binary, MessageKind::Flush, false)?;
    let start = Instant::now();
    // More than the kernel buffers on both sides, so the upload has to wait for the server that doesn't read.
    let chunk = vec![0u8; 1024 * 1024];
    let mut written = 0usize;
    let err = loop {
        match writer.write_all(&chunk) {
            Ok(()) => written += chunk.len(),
            Err(err) => break err,
        }
        assert!(
            written < 512 * 1024 * 1024,
            "the server takes this much only if it reads, which it doesn't"
        );
    };
    interrupter.join().expect("the thread doesn't panic");
    assert!(
        start.elapsed() < PROMPTLY,
        "the upload ended when interrupted: {:?}",
        start.elapsed()
    );
    // The pipe to the worker closed, which is all that the writer of a request can tell. The reason is told by the response.
    assert!(
        err.kind() == std::io::ErrorKind::BrokenPipe || is_cancelled(&err),
        "the upload failed as its connection ended: {err:?}"
    );
    Ok(())
}

#[test]
fn an_interrupt_ends_requests_that_wait_for_the_server_with_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_backend!(an_interrupt_ends_the_wait_for_a_server_that_doesnt_respond());
    for_each_backend!(an_interrupt_ends_the_wait_for_a_response_body_that_stalls());
    Ok(())
}

#[test]
fn an_interrupt_ends_uploads_that_the_server_doesnt_take_with_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_backend!(an_interrupt_ends_an_upload_that_the_server_doesnt_take());
    Ok(())
}
