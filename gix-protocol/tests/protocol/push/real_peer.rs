//! Pushing to a real `git receive-pack`, which is the oracle for what the server accepts and reports.
use std::{io::Write, path::Path};

use gix_error::Class;
use gix_hash::{Kind, ObjectId};
use gix_protocol::push::{Command, Options, Request, ServerCapabilities, push, report::Status};
use gix_testtools::{Creation, TestResult};
use gix_transport::{Protocol, Service};

use super::classes;

/// The refs of the repository in `dir`, as `name id` lines.
pub(super) fn refs(dir: &Path) -> TestResult<String> {
    Ok(gix_testtools::git(
        dir,
        "for-each-ref --format='%(refname) %(objectname)'",
    )?)
}

pub(super) fn rev(dir: &Path, rev: &str) -> TestResult<ObjectId> {
    Ok(ObjectId::from_hex(
        gix_testtools::git(dir, &format!("rev-parse {rev}"))?.trim().as_bytes(),
    )?)
}

/// A pack without objects, as Git produces it for the repository at `dir`.
pub(super) fn empty_pack(dir: &Path) -> TestResult<Vec<u8>> {
    let output = gix_testtools::git_command(dir)
        .args(["pack-objects", "--stdout"])
        .stdin(std::process::Stdio::null())
        .output()?;
    assert!(output.status.success(), "{output:?}");
    Ok(output.stdout)
}

pub(super) struct Fixture {
    pub(super) kind: Kind,
    _dir: gix_testtools::tempfile::TempDir,
    pub(super) source: std::path::PathBuf,
    pub(super) remote: std::path::PathBuf,
    pub(super) empty: std::path::PathBuf,
}

pub(super) fn fixtures() -> TestResult<Vec<Fixture>> {
    Kind::all()
        .iter()
        .copied()
        .map(|kind| {
            let dir = gix_testtools::scripted_fixture_writable_with_args(
                "make_push_remote.sh",
                [kind.to_string()],
                Creation::CopyFromReadOnly,
            )?;
            Ok(Fixture {
                kind,
                source: dir.path().join("source"),
                remote: dir.path().join("remote.git"),
                empty: dir.path().join("empty.git"),
                _dir: dir,
            })
        })
        .collect()
}

/// Perform the handshake with the repository at `remote`, and return what's needed to push to it.
pub(super) fn connect(
    remote: &Path,
) -> TestResult<(
    gix_transport::client::blocking_io::file::SpawnProcessOnDemand,
    gix_protocol::Handshake,
)> {
    let mut transport =
        gix_transport::client::blocking_io::file::connect(remote.to_string_lossy().into_owned(), Protocol::V1, false)?;
    let handshake = gix_protocol::handshake(
        &mut transport,
        Service::ReceivePack,
        |_| Ok(None),
        Vec::new(),
        &mut gix_utils::progress::Discard,
    )?;
    Ok((transport, handshake))
}

fn push_to(
    kind: Kind,
    remote: &Path,
    commands: Vec<Command>,
    options: Options,
    pack: impl FnOnce(&mut dyn Write) -> gix_error::Result,
) -> TestResult<gix_error::Result<gix_protocol::push::Report>> {
    let (mut transport, handshake) = connect(remote)?;
    let request = Request::new(
        commands,
        kind,
        &ServerCapabilities::from_capabilities(&handshake.capabilities),
        options,
    )?;
    Ok(push(&mut transport, &request, pack, None, false))
}

pub(super) fn write_all(bytes: Vec<u8>) -> impl FnOnce(&mut dyn Write) -> gix_error::Result {
    move |out| out.write_all(&bytes).map_err(gix_error::Error::from_error)
}

#[test]
fn deleting_a_reference_needs_no_pack() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let victim = rev(&f.source, "victim")?;
        let report = push_to(
            kind,
            &f.remote,
            vec![Command::new("refs/tags/victim", victim, kind.null())?],
            Options::default(),
            |_| panic!("a pack is not sent when only deleting"),
        )??;
        assert!(report.is_success(), "{kind}: {report:?}");
        assert!(
            !refs(&f.remote)?.contains("refs/tags/victim"),
            "{kind}: the server deleted the tag"
        );
    }
    Ok(())
}

#[test]
fn updates_to_existing_objects_are_sent_with_a_pack_without_objects() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two) = (rev(&f.source, "victim")?, rev(&f.source, "main")?);
        let commands = || -> TestResult<_> {
            Ok(vec![
                Command::new("refs/heads/main", two, one)?,
                Command::new("refs/heads/copy", kind.null(), two)?,
            ])
        };
        let report = push_to(
            kind,
            &f.remote,
            commands()?,
            Options::default(),
            write_all(empty_pack(&f.remote)?),
        )??;
        assert!(report.is_success(), "{kind}: {report:?}");
        assert_eq!(
            refs(&f.remote)?,
            format!("refs/heads/copy {two}\nrefs/heads/main {one}\nrefs/tags/victim {one}\n"),
            "{kind}: both updates were applied, with a pack that has no objects"
        );
    }
    Ok(())
}

#[test]
fn a_stale_old_id_is_rejected_by_the_server_and_nothing_changes() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two) = (rev(&f.source, "victim")?, rev(&f.source, "main")?);
        let before = refs(&f.remote)?;
        // The caller believes `main` is still at "one" even though the remote has it at "two".
        let report = push_to(
            kind,
            &f.remote,
            vec![Command::new("refs/heads/main", one, two)?],
            Options::default(),
            write_all(empty_pack(&f.remote)?),
        )??;
        assert!(
            matches!(&report.commands[0].status, Status::Rejected { .. }) && report.is_definite(),
            "{kind}: the server verified the expectation we sent, which a fresh advertisement didn't replace: {report:?}"
        );
        assert_eq!(refs(&f.remote)?, before, "{kind}: nothing changed");
    }
    Ok(())
}

/// Install an executable `name` hook in `remote` that runs `script`.
fn install_hook(remote: &Path, name: &str, script: &str) -> TestResult {
    let hook = remote.join("hooks").join(name);
    std::fs::create_dir_all(hook.parent().expect("a parent"))?;
    std::fs::write(&hook, format!("#!/bin/sh\n{script}\n"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[test]
fn without_side_band_the_server_finishes_its_hooks_before_push_returns() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    // The hash doesn't matter for what is tested here, so one is enough.
    for f in fixtures()?.into_iter().take(1) {
        let kind = f.kind;
        let (one, two) = (rev(&f.source, "victim")?, rev(&f.source, "main")?);
        // `post-receive` runs after the report was sent, and its working directory is the remote.
        install_hook(&f.remote, "post-receive", "sleep 1\necho done >hook-finished")?;
        let (mut transport, handshake) = connect(&f.remote)?;
        let mut server = ServerCapabilities::from_capabilities(&handshake.capabilities);
        server.side_band_64k = false;
        let request = Request::new(
            vec![Command::new("refs/heads/main", two, one)?],
            kind,
            &server,
            Options::default(),
        )?;
        assert!(
            !request.uses_side_band(),
            "{kind}: the report is a plain sequence of packet lines"
        );
        let report = push(&mut transport, &request, write_all(empty_pack(&f.remote)?), None, false)?;
        assert!(report.is_success(), "{kind}: {report:?}");
        assert!(
            f.remote.join("hook-finished").is_file(),
            "{kind}: like `git push`, we wait for the server to finish, instead of killing it with the hooks it still runs when the transport is dropped"
        );
    }
    Ok(())
}

#[test]
fn a_producer_that_fails_after_a_complete_pack_leaves_the_outcome_unknown() -> TestResult {
    use gix_transport::client::TransportWithoutIO;
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()?.into_iter().take(1) {
        let kind = f.kind;
        let (one, two) = (rev(&f.source, "victim")?, rev(&f.source, "main")?);
        let pack = empty_pack(&f.remote)?;
        let (mut transport, handshake) = connect(&f.remote)?;
        let request = Request::new(
            vec![Command::new("refs/heads/main", two, one)?],
            kind,
            &ServerCapabilities::from_capabilities(&handshake.capabilities),
            Options::default(),
        )?;
        let err = push(
            &mut transport,
            &request,
            move |out| {
                out.write_all(&pack).map_err(gix_error::Error::from_error)?;
                Err(gix_error::ErrorExt::raise(gix_error::message(
                    "something went missing after the last object",
                )))
            },
            None,
            false,
        )
        .expect_err("the producer failed");
        assert_eq!(classes(&err), [Class::Corruption], "{kind}: {err:#}");
        assert!(
            err.downcast_any_ref::<gix_protocol::push::PackProducerFailedAfterCompletion>()
                .is_some_and(|e| e.reason.contains("went missing")),
            "{kind}: the failure can be told apart, and keeps the producer's words: {err:#}"
        );
        assert!(transport.wait_for_server(std::time::Duration::from_secs(60))?);
        assert_eq!(
            rev(&f.remote, "refs/heads/main")?,
            one,
            "{kind}: the server acted on the complete pack, which is why this isn't a plain failure"
        );
    }
    Ok(())
}

#[test]
fn a_server_that_was_sent_an_incomplete_pack_ends_when_it_is_waited_for() -> TestResult {
    use gix_transport::client::TransportWithoutIO;
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()?.into_iter().take(1) {
        let kind = f.kind;
        let (one, two) = (rev(&f.source, "victim")?, rev(&f.source, "main")?);
        let before = refs(&f.remote)?;
        let (mut transport, handshake) = connect(&f.remote)?;
        let request = Request::new(
            vec![Command::new("refs/heads/main", two, one)?],
            kind,
            &ServerCapabilities::from_capabilities(&handshake.capabilities),
            Options::default(),
        )?;
        // The server is told that a pack follows, and waits for it, which is what makes it hang around once we stop.
        let err = push(&mut transport, &request, write_all(b"PACK".to_vec()), None, false)
            .expect_err("a pack without trailing hash is refused locally");
        assert_eq!(classes(&err), [Class::Validation], "{kind}: {err:#}");
        assert!(
            transport.wait_for_server(std::time::Duration::from_secs(60))?,
            "{kind}: the server ends once its input is closed, and is waited for instead of killed"
        );
        assert_eq!(refs(&f.remote)?, before, "{kind}: nothing was applied");
    }
    Ok(())
}

#[test]
fn the_remote_hanging_up_without_a_report_is_an_uncertain_outcome() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two) = (rev(&f.source, "victim")?, rev(&f.source, "main")?);
        // Fail the second batch of updates of a non-atomic push, which makes `receive-pack` die without a report
        // after the first batch, which deletes, was committed.
        let hook = f.remote.join("hooks/reference-transaction");
        std::fs::create_dir_all(hook.parent().expect("a parent"))?;
        std::fs::write(
            &hook,
            "#!/bin/sh\nif [ \"$1\" = prepared ] && ! awk '$2 ~ /^0+$/ { found = 1 } END { exit !found }'; then exit 1; fi\n",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
        }
        let err = push_to(
            kind,
            &f.remote,
            vec![
                Command::new("refs/tags/victim", one, kind.null())?,
                Command::new("refs/heads/main", two, one)?,
            ],
            Options::default(),
            write_all(empty_pack(&f.remote)?),
        )?
        .expect_err("there is no report to parse");
        assert_eq!(classes(&err), [Class::Corruption], "{kind}: {err:#}");
        assert!(
            format!("{err:#}").contains("outcome of the push is unknown"),
            "{kind}: {err:#}"
        );
        assert!(
            !refs(&f.remote)?.contains("refs/tags/victim"),
            "{kind}: the deletion was committed before the server died, which is why the outcome is unknown"
        );
    }
    Ok(())
}

/// Make the directory writable again so it can be removed, even if the test fails early.
#[cfg(unix)]
struct MakeWritable(std::path::PathBuf);

#[cfg(unix)]
impl Drop for MakeWritable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755)).ok();
    }
}

#[cfg(unix)]
#[test]
fn a_failed_commit_never_turns_into_a_wrong_report_of_what_happened() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two) = (rev(&f.source, "victim")?, rev(&f.source, "main")?);
        let heads = f.remote.join("refs/heads");
        std::fs::set_permissions(&heads, std::fs::Permissions::from_mode(0o555))?;
        let _make_writable_again = MakeWritable(heads.clone());
        let writable = std::fs::File::create(heads.join("probe")).is_ok();
        if writable {
            std::fs::remove_file(heads.join("probe"))?;
            eprintln!("{kind}: skipped as read-only directories can't be enforced, which happens when running as root");
            continue;
        }
        let report = push_to(
            kind,
            &f.remote,
            vec![
                Command::new("refs/tags/victim", one, kind.null())?,
                Command::new("refs/heads/main", two, one)?,
            ],
            Options::default(),
            write_all(empty_pack(&f.remote)?),
        )?;
        let report = report?;
        // What the server says about this differs between versions of Git. Older ones fail the refs one by one, newer ones
        // fail a whole batch of updates, and say so for every command, including deletions that were committed.
        // What must hold for all of them is that a command isn't reported as not applied if it was, and the other way around.
        let status = |name: &str| {
            &report
                .commands
                .iter()
                .find(|command| command.ref_name == name)
                .expect("every command is reported")
                .status
        };
        assert!(
            !refs(&f.remote)?.contains("refs/tags/victim"),
            "{kind}: the deletion was committed, as it doesn't need the read-only directory"
        );
        assert!(
            !matches!(status("refs/tags/victim"), Status::Rejected { .. }),
            "{kind}: a deletion that happened is never called rejected: {report:?}"
        );
        assert_eq!(
            rev(&f.remote, "refs/heads/main")?,
            two,
            "{kind}: the read-only directory prevented the update"
        );
        assert!(
            !matches!(status("refs/heads/main"), Status::Accepted { .. }),
            "{kind}: an update that didn't happen is never called accepted: {report:?}"
        );
        for command in &report.commands {
            if let Status::Rejected { reason } | Status::Uncertain { reason } = &command.status {
                assert_eq!(
                    reason.starts_with(b"failed to update refs"),
                    matches!(command.status, Status::Uncertain { .. }),
                    "{kind}: only the generic failure of a batch, which says nothing about single commands, is uncertain: {report:?}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn remotes_announce_their_object_format_and_mismatching_pushes_are_refused_before_sending() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        for remote in [&f.remote, &f.empty] {
            let (_transport, handshake) = connect(remote)?;
            let server = ServerCapabilities::from_capabilities(&handshake.capabilities);
            assert_eq!(
                server.object_format.as_ref().map(ToString::to_string),
                Some(kind.to_string()),
                "{kind}: the object format is announced even by an empty repository"
            );
            assert!(server.report_status && server.delete_refs, "{kind}: {server:?}");
            if let Some(other) = Kind::all().iter().copied().find(|other| *other != kind) {
                let err = Request::new(
                    vec![Command::new(
                        "refs/heads/a",
                        other.null(),
                        ObjectId::from_hex(&vec![b'1'; other.len_in_hex()])?,
                    )?],
                    other,
                    &server,
                    Options::default(),
                )
                .expect_err("the remote uses another hash");
                assert_eq!(classes(&err), [Class::Unsupported], "{kind}: {err:#}");
            }
        }
    }
    Ok(())
}
