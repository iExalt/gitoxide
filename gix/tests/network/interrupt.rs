//! Cancellation reaches I/O that blocks on a peer that doesn't answer: a handle obtained from the transport ends it from another
//! thread, as promptly as the transport can tell, whatever the transport is.
use std::time::{Duration, Instant};

use gix::protocol::transport::{
    Protocol, Service,
    client::{
        TransportWithoutIO,
        blocking_io::{Transport, ssh},
    },
};
use gix_testtools::{
    TestResult,
    fake_ssh::{Behavior, FakeSsh},
};

use crate::isolated;

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
