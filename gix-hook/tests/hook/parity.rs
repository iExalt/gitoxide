//! Differential tests: Git runs the hooks while the fixture is created, and `gix-hook` has to do the same.
use std::path::{Path, PathBuf};

use gix_hook::{Hooks, Prepare, Settings, Stdin};

use crate::Result;

/// The fixture records Git 2.55 behavior, so older local versions can't run these tests. CI always does.
fn too_old_git() -> bool {
    gix_testtools::should_skip_as_git_version_is_smaller_than(2, 55, 0)
}

fn fixture() -> Result<PathBuf> {
    gix_testtools::scripted_fixture_read_only("hooks.sh")
}

fn hooks(repo: &Path, event: &str) -> Result<Hooks> {
    let config = gix_config::File::from_path_no_includes(repo.join(".git/config"), gix_config::Source::Local)?;
    let settings = Settings::from_config(&config, &mut |_| true)?;
    Ok(Hooks::resolve(event, &repo.join(".git/hooks"), Some(&settings)))
}

/// Run the hooks of `pre-commit` in the repository `name` and return the log they wrote along with the exit code.
fn run_pre_commit(name: &str, configure: impl FnOnce(Prepare) -> Prepare) -> Result<(String, i32)> {
    let repo = fixture()?.join(name);
    let log_dir = gix_testtools::tempfile::tempdir()?;
    let log = log_dir.path().join("run.log");
    let prepare = Prepare::new(hooks(&repo, "pre-commit")?)
        .current_dir(&repo)
        .env("RUN_LOG", &log);
    let outcome = configure(prepare).run()?;
    Ok((std::fs::read_to_string(log).unwrap_or_default(), outcome.exit_code()))
}

fn expected(name: &str, suffix: &str) -> Result<String> {
    Ok(std::fs::read_to_string(
        fixture()?.join(name).join(format!("expected-{name}.{suffix}")),
    )?)
}

fn sorted(lines: &str) -> Vec<&str> {
    let mut lines: Vec<_> = lines.lines().collect();
    lines.sort_unstable();
    lines
}

#[test]
fn configured_hooks_run_before_the_hook_file_with_git_exact_shell_arguments_and_stdin() -> Result {
    if too_old_git() {
        return Ok(());
    }
    let stdin = std::fs::read(fixture()?.join("stdin.txt"))?;
    let (log, exit_code) = run_pre_commit("order", |p| p.args(["x", "y"]).stdin(Stdin::Bytes(stdin)))?;
    assert_eq!(
        log,
        expected("order", "log")?,
        "order, `$0` being the command and duplicated arguments, and standard input are as in Git"
    );
    assert_eq!(exit_code, 0);
    Ok(())
}

#[test]
fn a_disabled_event_only_disables_configured_hooks() -> Result {
    if too_old_git() {
        return Ok(());
    }
    let (log, _) = run_pre_commit("event-disabled", |p| p)?;
    assert_eq!(
        log,
        expected("event-disabled", "log")?,
        "the hook file still runs, like in Git"
    );
    assert!(!log.contains("cfg-a"), "the configured hook doesn't");
    Ok(())
}

#[test]
fn a_disabled_hook_is_skipped() -> Result {
    if too_old_git() {
        return Ok(());
    }
    let (log, _) = run_pre_commit("hook-disabled", |p| p)?;
    assert_eq!(log, expected("hook-disabled", "log")?);
    Ok(())
}

#[test]
fn all_hooks_run_after_failures_and_exit_codes_are_combined() -> Result {
    if too_old_git() {
        return Ok(());
    }
    let (log, exit_code) = run_pre_commit("exit-codes", |p| p.env("HOOK_EXIT", "4"))?;
    assert_eq!(
        log,
        expected("exit-codes", "log")?,
        "a failing hook doesn't stop the others"
    );
    assert_eq!(
        exit_code.to_string(),
        expected("exit-codes", "code")?.trim(),
        "the exit codes 1, 2 and 4 are OR'ed like in Git"
    );
    Ok(())
}

#[test]
fn parallel_hooks_all_run_and_fail_together() -> Result {
    if too_old_git() {
        return Ok(());
    }
    let (log, exit_code) = run_pre_commit("parallel", |p| p)?;
    assert_eq!(
        sorted(&log),
        sorted(&expected("parallel", "log")?),
        "every hook ran, in no particular order"
    );
    assert_eq!(exit_code.to_string(), expected("parallel", "code")?.trim());
    Ok(())
}

#[test]
fn a_hook_file_without_execute_permission_is_ignored_with_git_advice() -> Result {
    if too_old_git() {
        return Ok(());
    }
    let repo = fixture()?.join("not-executable");
    let hooks = hooks(&repo, "pre-commit")?;
    assert!(hooks.is_empty());
    let path = hooks.not_executable().expect("the hook is reported as ignored");
    assert_eq!(
        gix_hook::ignored_hook_hint(path.strip_prefix(&repo)?),
        expected("not-executable", "stderr")?,
        "the advice matches Git's, which shows the path relative to the repository"
    );
    Ok(())
}
