//! A push to a local path that is served from within the process runs no program, which includes `git-receive-pack`: programs that
//! stand in for `git` and its helpers fail loudly when run, and the push with the receiver of this process still works.
use std::sync::atomic::AtomicBool;

use gix::remote::{Direction, push::Update};
use gix_testtools::TestResult;

use crate::{Setup, isolated, no_git_dispatch::put_shims_in_front};

fn push_new_branch(repo: &gix::Repository, url: &str, branch: &str) -> TestResult<gix::remote::push::Outcome> {
    let head_id = repo.head_id()?.detach();
    Ok(repo
        .remote_at(url)?
        .connect(Direction::Push)?
        .prepare_push(
            gix::progress::Discard,
            vec![Update::create(
                gix::refs::FullName::try_from(format!("refs/heads/{branch}"))?,
                head_id,
            )],
            Default::default(),
        )?
        .send(&AtomicBool::new(false))?)
}

#[test]
fn a_push_to_a_local_path_runs_no_program_with_the_native_transport_and_does_with_the_default() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let setup = Setup::new("sha1")?;
    let dir = gix_testtools::tempfile::tempdir()?;
    let (_shims, log) = put_shims_in_front(dir.path())?;
    let url = setup.path("remote.git").display().to_string();

    let mut repo = setup.work()?;
    repo.config_snapshot_mut()
        .set_raw_value("gitoxide.core.nativeLocalTransport", "true")?;
    let outcome = push_new_branch(&repo, &url, "native")?;
    assert!(outcome.is_success(), "{outcome:?}");
    assert!(
        outcome.server_finished,
        "the in-process server is waited for like a program"
    );
    assert!(
        !log.exists(),
        "no program was run for the push: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    assert!(
        setup.dest()?.try_find_reference("refs/heads/native")?.is_none(),
        "sanity: dest is a different repository"
    );
    assert!(
        setup.remote()?.try_find_reference("refs/heads/native")?.is_some(),
        "the reference was created in the remote"
    );

    // Without the setting, the programs that stand in for Git are found, which shows that the check above could have seen them.
    repo.config_snapshot_mut()
        .set_raw_value("gitoxide.core.nativeLocalTransport", "false")?;
    push_new_branch(&repo, &url, "spawned")
        .err()
        .expect("the stand-in for `git-receive-pack` fails");
    assert!(
        log.exists(),
        "the default transport looked for and ran `git-receive-pack`"
    );
    Ok(())
}

#[test]
fn a_repository_that_cannot_be_served_is_refused_without_running_a_program() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let setup = Setup::new("sha1")?;
    let dir = gix_testtools::tempfile::tempdir()?;
    let (_shims, log) = put_shims_in_front(dir.path())?;
    // A shallow repository has a different notion of what is connected, which the receiver doesn't implement.
    std::fs::write(
        setup.path("remote.git/shallow"),
        format!("{}\n", setup.work()?.head_id()?),
    )?;

    let mut repo = setup.work()?;
    repo.config_snapshot_mut()
        .set_raw_value("gitoxide.core.nativeLocalTransport", "true")?;
    let err = push_new_branch(&repo, &setup.path("remote.git").display().to_string(), "native")
        .err()
        .expect("the receiver refuses what it can't serve");
    assert!(
        format!("{err:?}").contains("shallow"),
        "the error says why the repository can't be served: {err:?}"
    );
    assert!(!log.exists(), "there is no silent fallback to a program");
    Ok(())
}

#[test]
fn the_environment_of_the_client_does_not_redirect_the_server_except_with_the_namespace_that_programs_inherit()
-> TestResult {
    let Some(environment) = isolated()? else {
        return Ok(());
    };
    let setup = Setup::new("sha1")?;
    let url = setup.path("remote.git").display().to_string();
    let seen = setup.path("remote.git/seen-by-hook");
    std::fs::write(
        setup.path("remote.git/hooks/pre-receive"),
        "#!/bin/sh\nenv | grep -E '^GIT_(CONFIG_COUNT|CONFIG_PARAMETERS|SHALLOW_FILE|NO_REPLACE_OBJECTS|PREFIX|GRAFT_FILE|INDEX_FILE|WORK_TREE)=' >>\"$GIT_DIR/seen-by-hook\"\nexit 0\n",
    )?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            setup.path("remote.git/hooks/pre-receive"),
            std::fs::Permissions::from_mode(0o755),
        )?;
    }
    // A started `git-receive-pack` inherits `GIT_NAMESPACE`, but not what describes the repository, the configuration or the working
    // tree of its parent, which Git removes. Among them is a configuration that would deny the deletion below.
    let _environment = environment
        .set("GIT_DIR", setup.path("dest.git").display().to_string())
        .set("GIT_NAMESPACE", "team")
        .set("GIT_CONFIG_COUNT", "1")
        .set("GIT_CONFIG_KEY_0", "receive.denyDeletes")
        .set("GIT_CONFIG_VALUE_0", "true")
        .set(
            "GIT_SHALLOW_FILE",
            setup.path("no-such-shallow-file").display().to_string(),
        )
        .set("GIT_NO_REPLACE_OBJECTS", "1")
        .set("GIT_PREFIX", "sub/");
    let mut repo = setup.work()?;
    repo.config_snapshot_mut()
        .set_raw_value("gitoxide.core.nativeLocalTransport", "true")?;
    let outcome = push_new_branch(&repo, &url, "scoped")?;
    assert!(outcome.is_success(), "{outcome:?}");

    let remote = setup.remote()?;
    assert!(
        remote
            .try_find_reference("refs/namespaces/team/refs/heads/scoped")?
            .is_some(),
        "the namespace of the environment applies, as it does to a program"
    );
    assert!(
        remote.try_find_reference("refs/heads/scoped")?.is_none(),
        "and nothing is written outside of it"
    );
    assert!(
        setup.dest()?.references()?.all()?.next().is_none(),
        "`GIT_DIR` of the client doesn't select another repository"
    );

    let head_id = repo.head_id()?.detach();
    let outcome = repo
        .remote_at(url.as_str())?
        .connect(Direction::Push)?
        .prepare_push(
            gix::progress::Discard,
            vec![Update::delete(
                gix::refs::FullName::try_from("refs/heads/scoped")?,
                head_id,
            )],
            Default::default(),
        )?
        .send(&AtomicBool::new(false))?;
    assert!(
        outcome.is_success(),
        "the configuration of the client's environment doesn't apply to the server, so deleting is allowed: {outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&seen)?,
        "",
        "hooks don't see the variables that describe the process which pushed"
    );
    Ok(())
}
