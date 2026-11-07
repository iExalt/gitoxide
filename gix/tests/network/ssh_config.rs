//! How `gix` invokes `ssh`, compared with how `git` does it, using a fake `ssh` that records what it was called with.
use gix::{bstr::ByteSlice, remote::Direction};
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

#[cfg(unix)]
fn via_gix_non_interactive(
    setup: &Setup,
    ssh: &FakeSsh,
    url: &str,
    wrapper: &std::path::Path,
) -> TestResult<Invocation> {
    let mut repo = setup.work()?;
    let wrapper_command = gix_quote::single(wrapper.to_string_lossy().as_bytes().as_bstr())
        .to_os_str_lossy()
        .into_owned();
    {
        let mut config = repo.config_snapshot_mut();
        config.set_raw_value(
            "core.sshCommand",
            format!(
                "GIT_DIR=local-canary GIT_PROTOCOL=first GIT_PROTOCOL=caller LANG='caller locale' {} -o BatchMode=no",
                wrapper_command.to_string_lossy()
            )
            .as_str(),
        )?;
        config.set_raw_value("gitoxide.ssh.nonInteractive", "false")?;
        config.append_config(["gitoxide.ssh.nonInteractive=true"], gix_config::Source::Cli)?;
        config.set_raw_value("ssh.variant", "ssh")?;
    }
    // The fake records its arguments and exits before any server-side action or prompt.
    let _ = repo
        .remote_at(url)?
        .connect(Direction::Fetch)?
        .ref_map(gix::progress::Discard, Default::default());
    let mut invocations = ssh.invocations();
    assert_eq!(invocations.len(), 1, "the explicit SSH variant avoids a feature probe");
    Ok(invocations.pop().expect("gix invoked the fake ssh executable"))
}

#[cfg(unix)]
fn assert_noninteractive_refusal(
    setup: &Setup,
    ssh: &FakeSsh,
    command: &str,
    variant: Option<&str>,
    context: &str,
) -> TestResult {
    let mut repo = setup.work()?;
    {
        let mut config = repo.config_snapshot_mut();
        config.set_raw_value("core.sshCommand", command)?;
        config.set_raw_value("gitoxide.ssh.nonInteractive", "true")?;
        if let Some(variant) = variant {
            config.set_raw_value("ssh.variant", variant)?;
        }
    }
    let remote = repo.remote_at("ssh://host.example/repo.git")?;
    let refused = match remote.connect(Direction::Fetch) {
        Err(_) => true,
        Ok(connection) => {
            let _ = connection.ref_map(gix::progress::Discard, Default::default());
            false
        }
    };
    assert!(refused, "{context}: unsupported SSH invocation must be refused");
    assert!(
        ssh.invocations().is_empty(),
        "{context}: no SSH process or feature probe ran"
    );
    Ok(())
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

#[test]
#[cfg(unix)]
fn command_line_non_interactive_policy_overrides_repo_default_and_places_batch_first() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let setup = Setup::new("sha1")?;
    let ssh = FakeSsh::new(Behavior::PermissionDenied)?;
    let wrapper = ssh.dir().join("forwarding-wrapper");
    let env_leak_marker = ssh.dir().join("local-repo-env-leaked");
    std::fs::write(
        &wrapper,
        "#!/bin/sh\nif [ \"${GIT_DIR+x}\" = x ]; then : > \"${0%/*}/local-repo-env-leaked\"; fi\nexec \"${0%/*}/ssh\" \"$@\"\n",
    )?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))?;
    let invocation = via_gix_non_interactive(&setup, &ssh, "ssh://host.example/repo.git", &wrapper)?;

    assert_eq!(
        invocation.args.first().map(String::as_str),
        Some("-oBatchMode=yes"),
        "non-interactive policy is the first OpenSSH option, before caller options"
    );
    assert!(
        invocation
            .args
            .windows(2)
            .any(|pair| pair[0] == "-o" && pair[1] == "BatchMode=no"),
        "the configured conflicting option remains forwarded after the enforced option"
    );
    assert_eq!(
        invocation.env("GIT_PROTOCOL"),
        Some("caller"),
        "the final caller assignment overrides the generated protocol version"
    );
    assert_eq!(
        invocation.env("LANG"),
        Some("caller locale"),
        "quoted static environment values are preserved"
    );
    assert!(
        !env_leak_marker.exists(),
        "repository-local GIT_DIR is stripped before the SSH wrapper runs"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn non_interactive_policy_refuses_unsupported_commands_before_spawn_or_probe() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let setup = Setup::new("sha1")?;
    let ssh = FakeSsh::new(Behavior::PermissionDenied)?;
    let wrapper = ssh.dir().join("refused-wrapper");
    let marker = ssh.dir().join("process-started");
    std::fs::write(&wrapper, "#!/bin/sh\n: > \"${0%/*}/process-started\"\nexit 0\n")?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))?;
    let wrapper_command = gix_quote::single(wrapper.to_string_lossy().as_bytes().as_bstr())
        .to_os_str_lossy()
        .into_owned();
    let shell_form = format!("{}; true", wrapper_command.to_string_lossy());

    for (context, command, variant) in [
        ("unknown wrapper", wrapper_command.to_string_lossy().into_owned(), None),
        ("shell command", shell_form, None),
        ("simple variant", ssh.command().display().to_string(), Some("simple")),
        ("putty variant", ssh.command().display().to_string(), Some("putty")),
    ] {
        assert_noninteractive_refusal(&setup, &ssh, &command, variant, context)?;
        assert!(!marker.exists(), "{context}: the marker shim never started");
    }
    Ok(())
}

#[test]
#[cfg(unix)]
fn interactive_policy_keeps_the_unknown_variant_feature_probe() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let setup = Setup::new("sha1")?;
    let mut repo = setup.work()?;
    let dir = gix_testtools::tempfile::tempdir()?;
    let wrapper = dir.path().join("unknown-ssh-wrapper");
    let probe = dir.path().join("feature-probe-ran");
    let invocation = dir.path().join("ssh-invocation-ran");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = -G ]; then : > '{}'; exit 0; fi\n: > '{}'\nexit 1\n",
            probe.display(),
            invocation.display()
        ),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))?;
    }
    let wrapper_command = gix_quote::single(wrapper.to_string_lossy().as_bytes().as_bstr())
        .to_os_str_lossy()
        .into_owned();
    {
        let mut config = repo.config_snapshot_mut();
        config.set_raw_value("core.sshCommand", wrapper_command.to_string_lossy().as_ref())?;
        config.set_raw_value("gitoxide.ssh.nonInteractive", "false")?;
    }
    let remote = repo.remote_at("ssh://host.example/repo.git")?;
    let _connection = remote.connect(Direction::Fetch)?;
    assert!(probe.exists(), "the default interactive policy keeps the `-G` probe");
    assert!(!invocation.exists(), "the actual SSH connection remains deferred");
    Ok(())
}
