//! Expiring reflog entries, with `git reflog expire --all` as the oracle for the ages that are configured. See
//! `make_reflog_expire_repo.sh` for the repository and the ages of its entries.
use std::fmt::Write;

use gix::maintenance::reflog::Options;
use gix_testtools::{Creation, TestResult};

fn formats() -> Vec<&'static str> {
    let mut formats = vec!["files"];
    if cfg!(feature = "reftable") {
        formats.push("reftable");
    }
    formats
}

fn fixture(format: &'static str) -> TestResult<gix_testtools::tempfile::TempDir> {
    Ok(gix_testtools::scripted_fixture_writable_with_args(
        "make_reflog_expire_repo.sh",
        [format],
        Creation::Execute,
    )?)
}

/// Every reflog of the repository and its linked worktree with the subject of each entry, as Git shows it. The fixture is created
/// once for Git and once for the native code, which may be a second apart, so the times and ids of the entries aren't compared.
fn reflogs(dir: &std::path::Path) -> TestResult<String> {
    let mut out = String::new();
    for (cwd, prefix) in [("repo", ""), ("w1", "w1: ")] {
        let cwd = dir.join(cwd);
        let mut names: Vec<_> = gix_testtools::git(&cwd, "reflog list")?
            .lines()
            .map(ToOwned::to_owned)
            .collect();
        names.sort();
        for name in names {
            writeln!(out, "{prefix}[{name}]")?;
            out.push_str(&gix_testtools::git(
                &cwd,
                &format!("reflog show --format=%gs {name} --"),
            )?);
        }
    }
    Ok(out)
}

#[test]
fn with_the_ages_set_what_remains_is_what_git_keeps() -> TestResult {
    for format in formats() {
        let git = fixture(format)?;
        let native = fixture(format)?;
        gix_testtools::git(git.path().join("repo"), "reflog expire --all")?;
        let before = reflogs(native.path())?;
        let repo = gix::open_opts(native.path().join("repo"), crate::restricted())?;
        let outcome = repo.expire_reflogs(Options::default())?;

        let (expected, actual) = (reflogs(git.path())?, reflogs(native.path())?);
        assert_ne!(
            before, actual,
            "{format}: entries were removed, or the fixture is wrong"
        );
        assert_eq!(
            actual, expected,
            "{format}: the same entries remain, with the ages configured so that Git's defaults don't matter: {outcome:?}"
        );
        assert!(outcome.removed > 0 && outcome.kept > 0, "{format}: {outcome:?}");
        assert!(outcome.failed.is_empty(), "{format}: {outcome:?}");
    }
    Ok(())
}

fn without_ages(dir: &std::path::Path) -> TestResult {
    for key in [
        "gc.reflogExpire",
        "gc.reflogExpireUnreachable",
        "gc.refs/heads/pinned.reflogExpire",
        "gc.refs/heads/pinned.reflogExpireUnreachable",
        "gc.refs/heads/pinned-total-only.reflogExpireUnreachable",
    ] {
        gix_testtools::git(dir.join("repo"), &format!("config --unset {key}"))?;
    }
    Ok(())
}

/// The commit messages of the entries that remain in the reflog of `HEAD` of the main worktree, oldest first.
fn head_entries(dir: &std::path::Path) -> TestResult<Vec<String>> {
    Ok(
        gix_testtools::git(dir.join("repo"), "reflog show --format=%gs HEAD --")?
            .lines()
            .rev()
            .map(ToOwned::to_owned)
            .collect(),
    )
}

#[test]
fn by_default_only_what_the_documentation_and_the_code_of_git_both_expire_is_removed() -> TestResult {
    let git = fixture("files")?;
    let native = fixture("files")?;
    for dir in [git.path(), native.path()] {
        without_ages(dir)?;
    }
    let all = head_entries(git.path())?;
    assert!(
        all.iter().any(|entry| entry.contains("c45")),
        "the fixture has the entry that is 45 days old: {all:?}"
    );

    gix_testtools::git(git.path().join("repo"), "reflog expire --all")?;
    assert!(
        !head_entries(git.path())?.iter().any(|entry| entry.contains("c45")),
        "Git 2.56 expires reachable entries after 30 days with nothing configured, while its documentation says 90: if this \
         fails, Git was fixed and the module documentation and this test are to be adjusted"
    );

    let repo = gix::open_opts(native.path().join("repo"), crate::restricted())?;
    repo.expire_reflogs(Options::default())?;
    let kept = head_entries(native.path())?;
    assert!(
        kept.iter().any(|entry| entry.contains("c45")),
        "an entry that the documentation keeps stays even though the code of Git expires it: {kept:?}"
    );
    assert!(
        !kept
            .iter()
            .any(|entry| entry.contains("c100") || entry.contains("c400")),
        "what both expire is gone: {kept:?}"
    );
    Ok(())
}

#[test]
fn a_dry_run_reports_what_would_happen_and_changes_nothing() -> TestResult {
    let dir = fixture("files")?;
    let before = reflogs(dir.path())?;
    let repo = gix::open_opts(dir.path().join("repo"), crate::restricted())?;
    let mut dry_run = Options::default();
    dry_run.dry_run = true;
    let dry = repo.expire_reflogs(dry_run)?;
    assert_eq!(reflogs(dir.path())?, before, "nothing was removed");
    let real = repo.expire_reflogs(Options::default())?;
    assert_eq!(
        (dry.reflogs, dry.removed, dry.kept),
        (real.reflogs, real.removed, real.kept),
        "the report is the same as for the real run"
    );
    assert!(real.removed > 0);
    Ok(())
}

#[test]
fn a_reflog_that_is_locked_doesnt_stop_the_others() -> TestResult {
    let dir = fixture("files")?;
    let repo = gix::open_opts(dir.path().join("repo"), crate::restricted())?;
    // The lock of the reference, like a concurrent push holds it.
    std::fs::write(dir.path().join("repo/.git/refs/heads/rewritten.lock"), "")?;
    let outcome = repo.expire_reflogs(Options::default())?;
    let failed: Vec<_> = outcome.failed.iter().map(|(name, _)| name.to_string()).collect();
    assert_eq!(failed, ["refs/heads/rewritten"], "{outcome:?}");
    assert!(outcome.removed > 0, "the other reflogs were expired: {outcome:?}");
    Ok(())
}

#[test]
fn the_forms_of_ages_that_git_understands_are_accepted() -> TestResult {
    let dir = fixture("files")?;
    let repo_dir = dir.path().join("repo");
    for value in [
        "3.months.ago",
        "1.year",
        "2 weeks ago",
        "now",
        "never",
        "false",
        "all",
        "90.days.ago",
    ] {
        gix_testtools::git(&repo_dir, &format!("config gc.reflogExpire {value:?}"))?;
        let repo = gix::open_opts(&repo_dir, crate::restricted())?;
        let mut options = Options::default();
        options.dry_run = true;
        repo.expire_reflogs(options)
            .map_err(|err| format!("gc.reflogExpire = {value}: {err:#?}"))?;
    }
    Ok(())
}
