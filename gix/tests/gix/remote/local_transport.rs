//! The thread that serves a push from within the process ends with the transport and doesn't outlive what asked for it.
use std::time::{Duration, Instant};

use gix::{
    protocol::transport::{
        Protocol, Service,
        client::{TransportWithoutIO, blocking_io::Transport},
    },
    remote::local_transport::NativeLocal,
};
use gix_testtools::TestResult;

fn remote_git_dir() -> TestResult<std::path::PathBuf> {
    Ok(gix_testtools::scripted_fixture_read_only_with_args("make_push_repos.sh", ["sha1"])?.join("remote.git"))
}

#[test]
fn dropping_a_transport_that_was_not_used_after_the_handshake_ends_the_server() -> TestResult {
    let mut transport = NativeLocal::new(remote_git_dir()?, Protocol::V2, false);
    transport.handshake(Service::ReceivePack, &[])?;
    let start = Instant::now();
    drop(transport);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "closing what the server reads ends it, so dropping never waits for its deadline"
    );
    Ok(())
}

#[test]
fn waiting_for_the_server_ends_it_and_reports_that_it_finished() -> TestResult {
    let mut transport = NativeLocal::new(remote_git_dir()?, Protocol::V2, false);
    transport.handshake(Service::ReceivePack, &[])?;
    assert!(
        transport.wait_for_server(Duration::from_secs(10))?,
        "the server saw the end of the input and finished within the deadline"
    );
    Ok(())
}

#[test]
fn an_interrupted_transport_fails_from_then_on_and_still_ends_its_server() -> TestResult {
    let mut transport = NativeLocal::new(remote_git_dir()?, Protocol::V2, false);
    transport.handshake(Service::ReceivePack, &[])?;
    let handle = transport
        .interrupt_handle()
        .expect("an in-process server can be interrupted");
    handle.interrupt();
    let err = transport
        .handshake(Service::ReceivePack, &[])
        .err()
        .expect("an interrupted transport doesn't start another exchange");
    assert!(
        matches!(&err, gix::protocol::transport::client::Error::Io(err) if err.kind() == std::io::ErrorKind::Interrupted),
        "{err:?}"
    );
    assert!(
        transport.wait_for_server(Duration::from_secs(10))?,
        "the server stops after the interrupt"
    );
    Ok(())
}

#[test]
fn a_fetch_with_a_protocol_version_that_the_server_does_not_speak_is_refused_before_anything_is_exchanged() -> TestResult
{
    let mut transport = NativeLocal::new(remote_git_dir()?, Protocol::V1, false);
    let err = transport
        .handshake(Service::UploadPack, &[])
        .err()
        .expect("only protocol version 2 is served");
    assert!(
        matches!(&err, gix::protocol::transport::client::Error::Io(err) if err.kind() == std::io::ErrorKind::Unsupported),
        "{err:?}"
    );
    Ok(())
}

#[test]
fn a_client_that_goes_away_in_the_middle_of_a_pack_leaves_nothing_behind() -> TestResult {
    use std::io::Write;

    use gix::protocol::transport::client::{MessageKind, WriteMode};

    let dir = gix_testtools::scripted_fixture_writable_with_args(
        "make_push_repos.sh",
        ["sha1"],
        gix_testtools::Creation::CopyFromReadOnly,
    )?;
    let remote = dir.path().join("remote.git");
    let source = gix::open_opts(dir.path().join("source"), gix::open::Options::isolated())?;
    let new_id = source.head_id()?;
    let null_id = gix::hash::Kind::Sha1.null();
    let refs_before = gix_testtools::git(&remote, "for-each-ref")?;

    let mut transport = NativeLocal::new(&remote, Protocol::V2, false);
    drop(transport.handshake(Service::ReceivePack, &[])?);
    {
        // The request writer wraps each write into a packet line, which would turn the pieces of this request into
        // garbage that the server refuses at once. The parts write verbatim, so the server really sees a pack that stops.
        let (mut writer, _reader) = transport
            .request(WriteMode::Binary, MessageKind::Flush, false)?
            .into_parts();
        let command = format!("{null_id} {new_id} refs/heads/never\0 report-status\n");
        let request = format!("{:04x}{command}0000", command.len() + 4);
        writer.write_all(request.as_bytes())?;
        writer.write_all(b"PACK\0\0\0\x02\0\0\0\x03")?;
        writer.flush()?;
    }
    assert!(
        transport.wait_for_server(Duration::from_secs(10))?,
        "the server saw the end of a pack that never completed, and ended"
    );
    drop(transport);

    assert_eq!(
        gix_testtools::git(&remote, "for-each-ref")?,
        refs_before,
        "an incomplete push changes no reference"
    );
    let leftovers: Vec<_> = std::fs::read_dir(remote.join("objects"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("incoming"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the quarantine of the push was removed: {leftovers:?}"
    );
    Ok(())
}
