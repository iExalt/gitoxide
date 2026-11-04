//! SSH authenticates in the program that is run, so Git's credential helpers are never involved, unlike with HTTP.
use gix::remote::Direction;
use gix_testtools::{
    TestResult,
    fake_ssh::{Behavior, FakeSsh},
    sshd::Sshd,
};

use crate::{HASHES, Helper, PASSWORD, Setup, USER, isolated};

fn ssh_url(setup: &Setup) -> String {
    format!("ssh://host.example{}/remote.git", setup.path(".").display())
}

fn client(setup: &Setup, ssh: &FakeSsh, helper: &Helper) -> TestResult<gix::Repository> {
    let mut repo = setup.work()?;
    helper.configure(&mut repo)?;
    repo.config_snapshot_mut()
        .set_raw_value("core.sshCommand", ssh.command().display().to_string().as_str())?;
    Ok(repo)
}

#[test]
fn a_refused_login_is_reported_without_consulting_credential_helpers() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for hash in HASHES {
        let setup = Setup::new(hash)?;
        let ssh = FakeSsh::new(Behavior::PermissionDenied)?;
        let helper = Helper::new(&setup.path("."), USER, PASSWORD)?;
        let repo = client(&setup, &ssh, &helper)?;
        let remote = repo.remote_at(ssh_url(&setup))?;
        let err = remote
            .connect(Direction::Fetch)?
            .ref_map(gix::progress::Discard, Default::default())
            .expect_err("the server refuses the key");
        assert_eq!(
            helper.actions(),
            Vec::<String>::new(),
            "{hash}: nothing that the helper knows could change what `ssh` decided, and passwords for ssh:// are not asked for: {err:?}"
        );
        assert_eq!(ssh.invocations().len(), 1, "{hash}: the login isn't retried");
        assert!(
            format!("{err:?}").contains("Permission denied"),
            "{hash}: the message of ssh remains visible: {err:?}"
        );
    }
    Ok(())
}

#[test]
fn fetch_runs_the_server_side_with_protocol_v2() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for hash in HASHES {
        let setup = Setup::new(hash)?;
        let ssh = FakeSsh::new(Behavior::RunCommand)?;
        let helper = Helper::new(&setup.path("."), USER, PASSWORD)?;
        let repo = client(&setup, &ssh, &helper)?;
        let remote = repo
            .remote_at(ssh_url(&setup))?
            .with_refspecs(Some("+refs/heads/*:refs/remotes/origin/*"), Direction::Fetch)?;
        let (ref_map, _handshake) = remote
            .connect(Direction::Fetch)?
            .ref_map(gix::progress::Discard, Default::default())?;
        assert!(
            ref_map.remote_refs.iter().any(|r| r.unpack().0 == "refs/heads/main"),
            "{hash}: the refs of the server arrive"
        );
        let invocations = ssh.invocations();
        assert_eq!(invocations.len(), 1);
        assert!(
            invocations[0]
                .remote_command()
                .is_some_and(|cmd| cmd.contains("git-upload-pack")),
            "{hash}: {:?}",
            invocations[0]
        );
        assert_eq!(invocations[0].env("GIT_PROTOCOL"), Some("version=2"));
        assert!(helper.actions().is_empty());
    }
    Ok(())
}

fn sshd() -> TestResult<Option<Sshd>> {
    let server = Sshd::start()?;
    assert!(
        server.is_some() || !gix_testtools::is_ci::cached(),
        "CI has `sshd`, `ssh` and `ssh-keygen`, and doesn't run as root"
    );
    if server.is_none() {
        eprintln!("SKIPPED: `sshd` isn't installed, or the user is root");
    }
    Ok(server)
}

fn real_ssh_client(setup: &Setup, config: &std::path::Path, helper: &Helper) -> TestResult<gix::Repository> {
    let mut repo = setup.work()?;
    helper.configure(&mut repo)?;
    repo.config_snapshot_mut()
        .set_raw_value("core.sshCommand", Sshd::command(config).as_str())?;
    Ok(repo)
}

#[test]
fn a_real_server_accepts_the_authorized_key_and_speaks_protocol_v2() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let Some(server) = sshd()? else {
        return Ok(());
    };
    for hash in HASHES {
        let setup = Setup::new(hash)?;
        let helper = Helper::new(&setup.path("."), USER, PASSWORD)?;
        let repo = real_ssh_client(&setup, &server.client_config(), &helper)?;
        let remote = repo
            .remote_at(Sshd::url(&setup.path("remote.git")))?
            .with_refspecs(Some("+refs/heads/*:refs/remotes/origin/*"), Direction::Fetch)?;
        let (ref_map, handshake) = remote
            .connect(Direction::Fetch)?
            .ref_map(gix::progress::Discard, Default::default())?;
        assert_eq!(
            handshake.server_protocol_version,
            gix::protocol::transport::Protocol::V2,
            "{hash}: `GIT_PROTOCOL` gets through `ssh` and `sshd`"
        );
        assert!(
            ref_map.remote_refs.iter().any(|r| r.unpack().0 == "refs/heads/main"),
            "{hash}: the refs of the server arrive"
        );
        assert!(
            helper.actions().is_empty(),
            "{hash}: credential helpers have no say in ssh logins"
        );
    }
    Ok(())
}

#[test]
fn a_real_server_refuses_an_unknown_key_without_involving_credential_helpers() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let Some(server) = sshd()? else {
        return Ok(());
    };
    let setup = Setup::new("sha1")?;
    let helper = Helper::new(&setup.path("."), USER, PASSWORD)?;
    let repo = real_ssh_client(&setup, &server.client_config_with_unknown_key(), &helper)?;
    let err = repo
        .remote_at(Sshd::url(&setup.path("remote.git")))?
        .connect(Direction::Fetch)?
        .ref_map(gix::progress::Discard, Default::default())
        .expect_err("the key is unknown to the server");
    assert!(
        helper.actions().is_empty(),
        "no helper is consulted for a failed ssh login: {err:?}"
    );
    assert!(
        format!("{err:?}").contains("Permission denied"),
        "the reason that `ssh` gave is part of the error: {err:?}"
    );
    Ok(())
}
