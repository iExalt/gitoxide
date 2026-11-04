//! How `gix` invokes `ssh`, compared with how `git` does it, using a fake `ssh` that records what it was called with.
use gix::remote::Direction;
use gix_testtools::TestResult;
use gix_testtools::fake_ssh::{Behavior, FakeSsh, Invocation};

use crate::{Setup, isolated};

/// What an invocation means, independent of how the arguments are spelled and ordered.
#[derive(Debug, PartialEq, Eq)]
struct Meaning {
    host: String,
    port: Option<String>,
    batch: bool,
    sends_git_protocol: bool,
    git_protocol: Option<String>,
    command: String,
}

fn meaning(invocation: &Invocation) -> Meaning {
    let mut out = Meaning {
        host: String::new(),
        port: None,
        batch: false,
        sends_git_protocol: false,
        git_protocol: invocation.env("GIT_PROTOCOL").map(ToOwned::to_owned),
        command: invocation.remote_command().unwrap_or_default(),
    };
    let mut args = invocation.args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-batch" => out.batch = true,
            "-p" | "-P" => out.port = args.next().cloned(),
            "-o" => out.sends_git_protocol |= args.next().is_some_and(|value| value == "SendEnv=GIT_PROTOCOL"),
            _ if arg.starts_with("-p") && arg.len() > 2 => out.port = Some(arg[2..].to_owned()),
            _ if arg.starts_with('-') => {}
            _ => {
                out.host.clone_from(arg);
                break;
            }
        }
    }
    out
}

/// Let `git` connect to `url` with `ssh.variant` set to `variant` if given, and return how it called `ssh`.
fn via_git(setup: &Setup, ssh: &FakeSsh, url: &str, variant: Option<&str>) -> TestResult<Meaning> {
    let mut cmd = gix_testtools::git_command(setup.path("work"));
    cmd.env("GIT_SSH_COMMAND", ssh.command());
    if let Some(variant) = variant {
        cmd.args(["-c", &format!("ssh.variant={variant}")]);
    }
    cmd.args(["-c", "protocol.version=2", "ls-remote", url]).output()?;
    let invocations = ssh.invocations();
    Ok(meaning(invocations.last().expect("git invoked ssh")))
}

fn via_gix(setup: &Setup, ssh: &FakeSsh, url: &str, variant: Option<&str>) -> TestResult<Meaning> {
    let mut repo = setup.work()?;
    {
        let mut config = repo.config_snapshot_mut();
        config.set_raw_value("core.sshCommand", ssh.command().display().to_string().as_str())?;
        if let Some(variant) = variant {
            config.set_raw_value("ssh.variant", variant)?;
        }
    }
    // The connection is made lazily, and fails as the fake isn't a server, which isn't what is tested.
    repo.remote_at(url)?
        .connect(Direction::Fetch)
        .map(|connection| connection.ref_map(gix::progress::Discard, Default::default()))
        .ok();
    let invocations = ssh.invocations();
    Ok(meaning(invocations.last().expect("gix invoked ssh")))
}

#[test]
fn the_same_invocations_as_git_for_the_same_configuration() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let setup = Setup::new("sha1")?;
    for (url, variant) in [
        ("ssh://host.example/repo.git", None),
        ("ssh://user@host.example/repo.git", None),
        ("ssh://user@host.example:2222/repo.git", None),
        ("ssh://user@host.example:2222/repo.git", Some("ssh")),
        ("ssh://user@host.example:2222/repo.git", Some("plink")),
        ("ssh://user@host.example:2222/repo.git", Some("putty")),
        ("ssh://user@host.example:2222/repo.git", Some("tortoiseplink")),
        ("ssh://user@host.example/repo.git", Some("simple")),
        ("user@host.example:repo.git", None),
        ("host.example:path/to/repo.git", None),
    ] {
        // `Hang` so that no server-side command is run, which makes it faster to see what the fake was called with.
        let git_ssh = FakeSsh::new(Behavior::PermissionDenied)?;
        let gix_ssh = FakeSsh::new(Behavior::PermissionDenied)?;
        assert_eq!(
            via_gix(&setup, &gix_ssh, url, variant)?,
            via_git(&setup, &git_ssh, url, variant)?,
            "{url} with variant {variant:?}: `gix` calls `ssh` like `git` does"
        );
    }
    Ok(())
}
