//! Differential tests of hooks: `git` ran each hook while the fixture was created and recorded what it saw.
//! The tests run the same hook with `gix` in the same repository and compare.
use std::path::PathBuf;

use gix_testtools::TestResult;

use crate::util::named_subrepo_opts;

const FIXTURE: &str = "make_hooks_repos.sh";

fn open(name: &str) -> TestResult<(gix::Repository, PathBuf)> {
    let repo = named_subrepo_opts(FIXTURE, name, crate::util::restricted())?;
    Ok((repo, gix_testtools::scripted_fixture_read_only(FIXTURE)?))
}

/// Run `event` of the repository `name` and return what the hook recorded.
fn run(name: &str, event: &str) -> TestResult<String> {
    let (repo, _fixture) = open(name)?;
    let log_dir = gix_testtools::tempfile::tempdir()?;
    let log = log_dir.path().join("run.log");
    let outcome = repo.hook(event)?.env("RUN_LOG", &log).run()?;
    assert!(outcome.is_success() && outcome.invoked(), "the hook ran: {outcome:?}");
    Ok(std::fs::read_to_string(log)?)
}

fn expected(name: &str) -> TestResult<String> {
    let fixture = gix_testtools::scripted_fixture_read_only(FIXTURE)?;
    Ok(std::fs::read_to_string(fixture.join(format!("expected-{name}.log")))?)
}

#[test]
fn hooks_run_at_the_top_of_the_worktree_without_git_dir() -> TestResult {
    let actual = run("plain", "pre-commit")?;
    assert_eq!(actual, expected("plain")?);
    assert!(
        actual.contains("git_dir=unset"),
        "Git doesn't set GIT_DIR for a discovered repository"
    );
    Ok(())
}

#[test]
fn a_relative_hooks_path_is_relative_to_the_top_of_the_worktree() -> TestResult {
    let actual = run("relative-hooks-path", "pre-commit")?;
    assert_eq!(actual, expected("relative-hooks-path")?);
    assert!(actual.contains("dir=custom-hooks"), "the default directory isn't used");
    Ok(())
}

#[test]
fn an_absolute_hooks_path_is_used_as_is() -> TestResult {
    let actual = run("absolute-hooks-path", "pre-commit")?;
    assert_eq!(actual, expected("absolute-hooks-path")?);
    assert!(actual.contains("dir=absolute-hooks"));
    Ok(())
}

#[test]
fn linked_worktrees_use_shared_hooks_and_their_own_git_dir() -> TestResult {
    let actual = run("linked", "pre-commit")?;
    assert_eq!(actual, expected("linked")?);
    assert!(actual.contains("git_dir=") && !actual.contains("git_dir=unset"));
    Ok(())
}

#[test]
fn receiving_hooks_run_in_the_git_dir_with_git_dir_set() -> TestResult {
    let bare = run("bare.git", "pre-receive")?;
    assert_eq!(bare, expected("bare")?, "bare repositories");
    let non_bare = run("receiving", "pre-receive")?;
    assert_eq!(
        non_bare,
        expected("receiving")?,
        "non-bare repositories, in their `.git`"
    );
    Ok(())
}

#[test]
fn deliberate_differences_to_git_are_pinned() -> TestResult {
    let (repo, fixture) = open("bare.git")?;
    let git_pin = std::fs::read_to_string(fixture.join("expected-bare-pin.log"))?;
    assert!(
        git_pin.contains("git_dir_raw=.\n"),
        "Git spells GIT_DIR as `.`: {git_pin}"
    );
    assert!(
        git_pin.contains("git-core"),
        "Git prepends its exec-path to PATH: {git_pin}"
    );

    let log_dir = gix_testtools::tempfile::tempdir()?;
    let pin_log = log_dir.path().join("pin.log");
    let outcome = repo.hook("post-update")?.env("PIN_LOG", &pin_log).run()?;
    assert!(outcome.is_success() && outcome.invoked());
    let actual = std::fs::read_to_string(pin_log)?;
    let git_dir_raw = actual
        .lines()
        .find_map(|line| line.strip_prefix("git_dir_raw="))
        .expect("recorded");
    assert!(
        std::path::Path::new(git_dir_raw).is_absolute(),
        "GIT_DIR is absolute where Git passes `.`: {git_dir_raw}"
    );
    let path = actual
        .lines()
        .find_map(|line| line.strip_prefix("path="))
        .expect("recorded");
    assert!(
        !path.contains("git-core"),
        "no Git exec-path is prepended to PATH: {path}"
    );
    Ok(())
}

#[test]
fn proc_receive_is_looked_up_as_hook_file_only() -> TestResult {
    let (repo, _) = open("bare.git")?;
    let dir = repo.hooks_dir(gix_hook::Side::Receive)?;
    assert!(matches!(
        gix_hook::find(&dir, "proc-receive"),
        gix_hook::Found::Executable(_)
    ));
    Ok(())
}

#[test]
fn a_hook_without_execute_permission_is_ignored_and_advice_can_be_disabled() -> TestResult {
    let (repo, _) = open("not-executable")?;
    let prepare = repo.hook("pre-commit")?;
    let outcome = prepare.run()?;
    assert!(!outcome.invoked() && outcome.is_success());
    let path = outcome.not_executable.expect("reported as ignored");
    let advice = repo.hook_advice(&path).expect("enabled by default");
    assert!(
        advice.starts_with("hint: The '") && advice.contains("hook was ignored because it's not set as executable")
    );

    let (repo, _) = open("not-executable-quiet")?;
    let outcome = repo.hook("pre-commit")?.run()?;
    let path = outcome.not_executable.expect("still ignored");
    assert_eq!(repo.hook_advice(&path), None, "`advice.ignoredHook = false` is honored");
    Ok(())
}

#[test]
fn repositories_that_are_not_fully_trusted_run_no_hooks() -> TestResult {
    let repo = named_subrepo_opts(
        FIXTURE,
        "plain",
        crate::util::restricted().with_trust(gix_sec::Trust::Reduced),
    )?;
    assert!(repo.hook("pre-commit").is_err());
    Ok(())
}

#[test]
fn reference_transaction_while_receiving_runs_in_the_git_dir_with_its_hooks_path() -> TestResult {
    let (repo, _fixture) = open("receiving-reftx")?;
    let log_dir = gix_testtools::tempfile::tempdir()?;
    let log = log_dir.path().join("run.log");
    let outcome = repo
        .hook_for("reference-transaction", gix_hook::Side::Receive)?
        .arg("preparing")
        .env("RUN_LOG", &log)
        .run()?;
    assert!(
        outcome.is_success() && outcome.invoked(),
        "the relative hooks path is relative to the Git directory"
    );
    let git = expected("receiving-reftx")?;
    let mut hooks_seen = 0;
    let first_invocation: String = git
        .split_inclusive('\n')
        .take_while(|line| {
            hooks_seen += usize::from(line.starts_with("hook="));
            hooks_seen < 2
        })
        .collect();
    let actual = std::fs::read_to_string(log)?;
    assert_eq!(actual, first_invocation, "Git's `preparing` run");
    assert!(actual.contains("dir=custom-hooks") && !actual.contains("git_dir=unset"));
    Ok(())
}

#[test]
fn git_dirs_that_are_not_dot_git_are_passed_on_so_hooks_can_run_git() -> TestResult {
    for (name, repo) in [("separate", "separate"), ("core-worktree", "core-worktree.git")] {
        let log = run(repo, "pre-commit")?;
        assert_eq!(log, expected(name)?, "{name}");
        assert!(!log.contains("git_dir=unset"), "{name}: GIT_DIR is set like in Git");
    }
    Ok(())
}

#[test]
fn a_relative_hooks_path_in_a_bare_repository_is_relative_to_the_git_dir() -> TestResult {
    let actual = run("bare-relative.git", "pre-receive")?;
    assert_eq!(actual, expected("bare-relative")?);
    assert!(actual.contains("dir=custom-hooks"));
    Ok(())
}

#[test]
fn inherited_location_variables_of_other_repositories_are_not_passed_on() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    let other = gix_testtools::tempfile::tempdir()?;
    let other_dir = other.path().to_str().expect("UTF-8 temporary path");
    let _env = gix_testtools::Env::new()
        .set("GIT_DIR", other_dir)
        .set("GIT_WORK_TREE", other_dir)
        .set("GIT_COMMON_DIR", other_dir);
    let actual = run("plain", "pre-commit")?;
    assert_eq!(
        actual,
        expected("plain")?,
        "no GIT_DIR, GIT_WORK_TREE or GIT_COMMON_DIR leak into the hook"
    );
    Ok(())
}
