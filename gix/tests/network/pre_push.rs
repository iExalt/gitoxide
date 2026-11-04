//! The `pre-push` hook of `gix`' push, compared with the one of `git push` for the cases that matter to it: a new branch, a
//! fast-forward, a deletion, an update that is up to date and one that Git refuses as it isn't a fast-forward.
//!
//! Both clients push to the same synthetic HTTP peer, one fixture each, with a hook that records what it was given.
use std::{path::Path, sync::atomic::AtomicBool};

use gix::{protocol::transport::client::blocking_io::http, remote::push::Update};
use gix_testtools::{
    TestResult,
    http_peer::{Config, HttpPeer},
};

use crate::{Setup, for_each_backend, grandparent_of_head, isolated, transport};

/// A hook that records how it was called, in `log`, and exits with `exit_code`.
fn install_hook(work: &Path, log: &Path, exit_code: u8) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let hook = work.join(".git/hooks/pre-push");
    std::fs::write(
        &hook,
        format!(
            r#"#!/bin/sh
{{
  echo "args: $*"
  echo "cwd: $(pwd)"
  echo "git_dir: ${{GIT_DIR-unset}}"
  echo "git_prefix: ${{GIT_PREFIX-unset}}"
  echo "git_index_file: ${{GIT_INDEX_FILE-unset}}"
  echo "stdin:"
  cat
}} >>'{}'
echo "visible on the standard output of the hook"
exit {exit_code}
"#,
            log.display()
        ),
    )?;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// What happened in the case that `refspec` and `update` describe: the log of the hook and whether the remote changed.
struct Run {
    hook_log: String,
    sent: bool,
}

/// Where a case ends up, as pairs of `(git's refspec, how gix names the update)`.
enum Case {
    NewBranch,
    FastForward,
    Delete,
    UpToDate,
    NotFastForward,
}

impl Case {
    fn refspec(&self) -> &'static str {
        match self {
            Case::NewBranch => "HEAD:refs/heads/new",
            Case::FastForward => "HEAD:refs/heads/main",
            Case::Delete => ":refs/tags/v1",
            Case::UpToDate => "refs/tags/v1:refs/tags/v1",
            Case::NotFastForward => "HEAD~2:refs/heads/main",
        }
    }
}

fn normalize(log: &str, setup: &Setup) -> String {
    let root = setup.path(".").canonicalize().expect("the fixture exists");
    log.replace(&root.display().to_string(), "<fixture>")
        .replace(&peer_placeholder(log), "<peer>")
}

/// The authority of the URL that the hook was given, which differs between runs.
fn peer_placeholder(log: &str) -> String {
    log.lines()
        .find_map(|line| line.strip_prefix("args: "))
        .and_then(|args| args.split_whitespace().nth(1))
        .and_then(|url| url.strip_suffix("/remote.git"))
        .unwrap_or("http://unused")
        .to_owned()
}

fn run_git(case: &Case, hook_exit_code: u8) -> TestResult<(Run, String)> {
    let setup = Setup::new("sha1")?;
    let peer = HttpPeer::start(Config {
        project_root: setup.path("."),
        ..Default::default()
    })?;
    let log = setup.path("hook.log");
    install_hook(&setup.path("work"), &log, hook_exit_code)?;
    let url = peer.url("remote.git");
    let mut command = gix_testtools::git_command(setup.path("work"));
    command.args(["push", &url, case.refspec()]);
    let _status = command.output()?;
    let hook_log = std::fs::read_to_string(&log).unwrap_or_default();
    let sent = peer
        .requests()
        .iter()
        .any(|request| request.method == "POST" && request.body_len > 4);
    Ok((
        Run {
            hook_log: normalize(&hook_log, &setup),
            sent,
        },
        url,
    ))
}

fn run_gix<H: http::Http + Default + 'static>(case: &Case, hook_exit_code: u8) -> TestResult<Run> {
    let setup = Setup::new("sha1")?;
    let peer = HttpPeer::start(Config {
        project_root: setup.path("."),
        ..Default::default()
    })?;
    let log = setup.path("hook.log");
    install_hook(&setup.path("work"), &log, hook_exit_code)?;
    let repo = setup.work()?;
    let remote_repo = gix::open_opts(setup.path("remote.git"), gix::open::Options::isolated())?;
    let remote_id = |name: &str| remote_repo.find_reference(name).map(|r| r.id().detach());
    let head_id = repo.head_id()?.detach();
    let full = |name: &str| gix::refs::FullName::try_from(name);
    let update = match case {
        Case::NewBranch => Update::create(full("refs/heads/new")?, head_id).with_local_name("HEAD"),
        Case::FastForward => {
            Update::change(full("refs/heads/main")?, remote_id("refs/heads/main")?, head_id).with_local_name("HEAD")
        }
        Case::Delete => Update::delete(full("refs/tags/v1")?, remote_id("refs/tags/v1")?),
        Case::UpToDate => {
            let v1 = remote_id("refs/tags/v1")?;
            Update::change(full("refs/tags/v1")?, v1, v1).with_local_name("refs/tags/v1")
        }
        Case::NotFastForward => {
            let one = grandparent_of_head(&repo)?;
            Update::change(full("refs/heads/main")?, remote_id("refs/heads/main")?, one).with_local_name("HEAD~2")
        }
    };
    let url = peer.url("remote.git");
    let options = gix::remote::push::Options {
        atomic: false,
        ..Default::default()
    };
    let prepare = repo
        .remote_at(url.as_str())?
        .to_connection_with_transport(transport::<H>(&url)?)
        .prepare_push(gix::progress::Discard, vec![update], options)?
        .with_pre_push_hook();
    let result = prepare.send(&AtomicBool::new(false));
    let sent = peer
        .requests()
        .iter()
        .any(|request| request.method == "POST" && request.body_len > 4);
    if hook_exit_code != 0 {
        assert!(result.is_err(), "a hook that fails refuses the push");
    }
    let hook_log = std::fs::read_to_string(&log).unwrap_or_default();
    Ok(Run {
        hook_log: normalize(&hook_log, &setup),
        sent,
    })
}

fn compare_with_git<H: http::Http + Default + 'static>() -> TestResult {
    for (index, case) in [
        Case::NewBranch,
        Case::FastForward,
        Case::Delete,
        Case::UpToDate,
        Case::NotFastForward,
    ]
    .iter()
    .enumerate()
    {
        let (git, _) = run_git(case, 0)?;
        let gix = run_gix::<H>(case, 0)?;
        assert_eq!(
            gix.hook_log
                .lines()
                .filter(|line| !line.starts_with("args:"))
                .collect::<Vec<_>>(),
            git.hook_log
                .lines()
                .filter(|line| !line.starts_with("args:"))
                .collect::<Vec<_>>(),
            "case {index} ({}): the hook sees the same directory, environment and updates as with `git push`",
            case.refspec()
        );
        assert_eq!(
            gix.hook_log
                .lines()
                .find(|line| line.starts_with("args:"))
                .map(|line| line.rsplit_once(' ').map(|(first, _)| first.to_owned())),
            git.hook_log
                .lines()
                .find(|line| line.starts_with("args:"))
                .map(|line| line.rsplit_once(' ').map(|(first, _)| first.to_owned())),
            "case {index}: the first argument, which names the remote, is the one that `git push` passes"
        );
        assert_eq!(
            gix.sent,
            git.sent,
            "case {index} ({}): the push sends something if and only if Git's does",
            case.refspec()
        );
    }
    Ok(())
}

#[test]
fn the_hook_gets_what_git_push_gives_it_with_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_backend!(compare_with_git());
    Ok(())
}

fn a_failing_hook_stops_the_push<H: http::Http + Default + 'static>() -> TestResult {
    let (git, _) = run_git(&Case::NewBranch, 1)?;
    let gix = run_gix::<H>(&Case::NewBranch, 1)?;
    assert!(
        !git.sent && !gix.sent,
        "neither push sends the commands and the pack after the hook failed"
    );
    assert!(!gix.hook_log.is_empty(), "the hook did run");
    Ok(())
}

#[test]
fn a_hook_that_fails_vetoes_the_push_with_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_backend!(a_failing_hook_stops_the_push());
    Ok(())
}

fn without_the_hook_call_nothing_runs<H: http::Http + Default + 'static>() -> TestResult {
    let setup = Setup::new("sha1")?;
    let peer = HttpPeer::start(Config {
        project_root: setup.path("."),
        ..Default::default()
    })?;
    let log = setup.path("hook.log");
    install_hook(&setup.path("work"), &log, 1)?;
    let repo = setup.work()?;
    let head_id = repo.head_id()?.detach();
    let url = peer.url("remote.git");
    // Like `git push --no-verify`: the hook isn't asked for, so even one that would refuse is not run.
    let outcome = repo
        .remote_at(url.as_str())?
        .to_connection_with_transport(transport::<H>(&url)?)
        .prepare_push(
            gix::progress::Discard,
            vec![Update::create(
                gix::refs::FullName::try_from("refs/heads/new")?,
                head_id,
            )],
            Default::default(),
        )?
        .send(&AtomicBool::new(false))?;
    assert!(outcome.is_success(), "{outcome:?}");
    assert!(!log.exists(), "the hook didn't run");
    Ok(())
}

#[test]
fn not_asking_for_the_hook_is_like_no_verify_with_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_backend!(without_the_hook_call_nothing_runs());
    Ok(())
}
