//! A receiver refuses what it can't do yet instead of ignoring it, because skipping a hook that guards a repository or a
//! setting that changes what a push does is not what whoever configured it wants. These refusals happen before anything is
//! advertised, and they are classified as unsupported so that callers can fall back to something that can.
use gix_testtools::TestResult;

fn bare() -> TestResult<(gix_testtools::tempfile::TempDir, gix::Repository)> {
    let dir = gix_testtools::tempfile::tempdir()?;
    let repo = gix::init_bare(dir.path())?;
    Ok((dir, repo))
}

fn open(dir: &std::path::Path, overrides: &[&str]) -> TestResult<gix::Repository> {
    Ok(gix::open_opts(
        dir,
        gix::open::Options::isolated().config_overrides(overrides.iter().copied()),
    )?)
}

#[test]
fn an_unconfigured_repository_can_receive() -> TestResult {
    let (dir, _repo) = bare()?;
    open(dir.path(), &[])?
        .receive()
        .expect("nothing is configured that a receiver can't do");
    Ok(())
}

#[test]
fn settings_that_change_what_a_push_does_are_refused() -> TestResult {
    let (dir, _repo) = bare()?;
    for (setting, needle) in [
        ("receive.denyCurrentBranch=updateInstead", "updateInstead"),
        ("receive.certNonceSeed=seed", "receive.certNonceSeed"),
        ("receive.procReceiveRefs=refs/for", "receive.procReceiveRefs"),
        ("transfer.advertiseSID=true", "transfer.advertiseSID"),
        ("receive.updateServerInfo=true", "receive.updateServerInfo"),
        ("core.alternateRefsCommand=true", "core.alternateRefsCommand"),
        ("core.alternateRefsPrefixes=refs/heads", "core.alternateRefsPrefixes"),
        ("extensions.partialClone=origin", "partial clone"),
        ("remote.origin.promisor=true", "promisor"),
        ("extensions.compatObjectFormat=sha256", "compatObjectFormat"),
        ("extensions.somethingNew=true", "somethingNew"),
    ] {
        let err = open(dir.path(), &[setting])?
            .receive()
            .err()
            .unwrap_or_else(|| panic!("{setting} has to be refused"));
        assert!(
            err.is_unsupported(),
            "{setting}: callers can tell that they have to do this differently: {err}"
        );
        assert!(
            err.to_string().contains(needle),
            "{setting}: the refusal names what is refused: {err}"
        );
    }
    Ok(())
}

#[test]
fn what_is_configured_but_supported_is_accepted() -> TestResult {
    let (dir, _repo) = bare()?;
    open(
        dir.path(),
        &[
            "receive.denyDeletes=true",
            "receive.denyNonFastForwards=true",
            "receive.denyCurrentBranch=refuse",
            "receive.hideRefs=refs/hidden",
            "receive.autogc=false",
            "receive.updateServerInfo=false",
            "transfer.advertiseSID=false",
            "extensions.worktreeConfig=true",
            "extensions.preciousObjects=true",
            "extensions.relativeWorktrees=true",
        ],
    )?
    .receive()
    .expect("these are supported, and refusing them would make the receiver unusable");
    Ok(())
}

#[cfg(unix)]
#[test]
fn hooks_that_cannot_run_yet_are_refused_and_only_those_that_exist() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let (dir, _repo) = bare()?;
    std::fs::create_dir_all(dir.path().join("hooks"))?;
    // A hook that isn't executable doesn't run in Git either, and neither does one that has a name of its own.
    std::fs::write(dir.path().join("hooks/pre-receive"), "#!/bin/sh\n")?;
    std::fs::write(dir.path().join("hooks/pre-receive.sample"), "#!/bin/sh\n")?;
    open(dir.path(), &[])?
        .receive()
        .expect("a hook that can't run is no hook");

    // These run, and so are no reason to refuse.
    for event in [
        "pre-receive",
        "update",
        "post-receive",
        "post-update",
        "reference-transaction",
    ] {
        let hook = dir.path().join("hooks").join(event);
        std::fs::write(&hook, "#!/bin/sh\nexit 0\n")?;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
        open(dir.path(), &[])?
            .receive()
            .unwrap_or_else(|err| panic!("the {event} hook can be run: {err}"));
        std::fs::remove_file(hook)?;
    }
    {
        let event = "push-to-checkout";
        let hook = dir.path().join("hooks").join(event);
        std::fs::write(&hook, "#!/bin/sh\nexit 0\n")?;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
        let err = open(dir.path(), &[])?
            .receive()
            .err()
            .unwrap_or_else(|| panic!("the {event} hook can't run yet, so the receiver has to refuse"));
        assert!(
            err.is_unsupported(),
            "{event}: callers can fall back to something that runs hooks: {err}"
        );
        assert!(
            err.to_string().contains(event),
            "{event}: the refusal names the hook: {err}"
        );
        std::fs::remove_file(hook)?;
    }
    Ok(())
}

#[test]
fn namespaces_and_shallow_repositories_are_refused() -> TestResult {
    let (dir, _repo) = bare()?;
    let mut repo = open(dir.path(), &[])?;
    repo.set_namespace("ns")?;
    let err = repo.receive().err().expect("namespaces change every reference name");
    assert!(err.is_unsupported(), "namespaces: {err}");

    std::fs::write(dir.path().join("shallow"), "1111111111111111111111111111111111111111\n")?;
    let err = open(dir.path(), &[])?
        .receive()
        .err()
        .expect("a shallow repository has a different notion of connected");
    assert!(err.is_unsupported(), "shallow: {err}");
    Ok(())
}

#[test]
fn a_repository_that_is_not_fully_trusted_is_refused_as_its_hooks_would_be_hidden() -> TestResult {
    let (dir, _repo) = bare()?;
    let config = dir.path().join("config");
    let mut text = std::fs::read_to_string(&config)?;
    text.push_str("[hook \"guard\"]\n\tevent = pre-receive\n\tcommand = true\n");
    std::fs::write(&config, text)?;

    let reduced = gix::open_opts(
        dir.path(),
        gix::open::Options::isolated().with_trust(gix_sec::Trust::Reduced),
    )?;
    let err = reduced
        .receive()
        .err()
        .expect("with reduced trust, hooks of the repository aren't seen, so it can't be told that none would run");
    assert!(
        err.is_unsupported(),
        "callers can tell that this has to be done differently: {err}"
    );
    assert!(err.to_string().contains("trusted"), "the refusal says why: {err}");
    Ok(())
}
