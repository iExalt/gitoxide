//! The `reference-transaction` hook as `git update-ref` runs it, compared with `edit_references_with_hooks()`.
//!
//! Each row runs Git on one copy of a repository and the same change through this crate on another copy, with a hook that
//! records its arguments and standard input. The two records have to be identical, which includes the nested transaction on
//! `packed-refs` that Git opens when it deletes references: a hook can't tell it from the one that is asked for.
#![cfg(unix)]
use std::path::Path;

use gix::refs::{
    Target,
    transaction::{Change, LogChange, PreviousValue, RefEdit, observe::Vetoed},
};
use gix_testtools::TestResult;

/// A repository with `main` at `c2`, `refs/heads/old` at `c1` and a few references in `packed-refs`:
/// `refs/heads/packed` (packed only), `refs/heads/both` (packed and loose, with the loose one newer).
fn repo(reftable: bool) -> TestResult<(gix::Repository, gix_testtools::tempfile::TempDir)> {
    let (repo, tmp) = if reftable {
        let tmp = gix_testtools::tempfile::tempdir()?;
        gix_testtools::git(tmp.path(), "init -q -b main --ref-format=reftable .")?;
        gix_testtools::git(tmp.path(), "commit -q --allow-empty -m c1")?;
        gix_testtools::git(tmp.path(), "commit -q --allow-empty -m c2")?;
        (gix::open_opts(tmp.path(), crate::util::restricted())?, tmp)
    } else {
        crate::basic_rw_repo()?
    };
    let dir = repo.workdir().expect("non-bare");
    gix_testtools::git(dir, "update-ref refs/heads/old main~1")?;
    gix_testtools::git(dir, "update-ref refs/heads/packed main~1")?;
    gix_testtools::git(dir, "update-ref refs/heads/both main~1")?;
    gix_testtools::git(dir, "pack-refs --all")?;
    gix_testtools::git(dir, "update-ref refs/heads/both main")?;
    gix_testtools::git(dir, "update-ref refs/heads/old main~1")?;
    Ok((repo, tmp))
}

/// Install a hook that appends `state | line|line|` to `log` and fails in the state in the file `veto` if that exists.
fn install_hook(repo: &gix::Repository, log: &Path, veto: &Path) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        "#!/bin/sh\nstate=$1\nlines=$(cat | tr '\\n' '|')\necho \"$state | $lines\" >> '{log}'\n\
         if [ \"$(cat '{veto}' 2>/dev/null)\" = \"$state\" ]; then exit 1; fi\n",
        log = log.display(),
        veto = veto.display()
    );
    let path = repo.git_dir().join("hooks/reference-transaction");
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    std::fs::write(&path, script)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn id(repo: &gix::Repository, spec: &str) -> TestResult<gix::ObjectId> {
    let hex = gix_testtools::git(repo.workdir().expect("non-bare"), &format!("rev-parse {spec}"))?;
    Ok(gix::ObjectId::from_hex(hex.trim().as_bytes())?)
}

fn update(name: &str, new: gix::ObjectId, expected: PreviousValue, deref: bool) -> RefEdit {
    RefEdit {
        change: Change::Update {
            log: LogChange::default(),
            expected,
            new: Target::Object(new),
        },
        name: name.try_into().expect("valid name"),
        deref,
    }
}

fn exists(id: gix::ObjectId) -> PreviousValue {
    PreviousValue::MustExistAndMatch(Target::Object(id))
}

fn delete(name: &str, expected: PreviousValue, deref: bool) -> RefEdit {
    RefEdit {
        change: Change::Delete {
            expected,
            log: gix::refs::transaction::RefLog::AndReference,
        },
        name: name.try_into().expect("valid name"),
        deref,
    }
}

struct Row {
    name: &'static str,
    /// What to give to `git update-ref --stdin`.
    git_stdin: &'static str,
    /// The namespace to work in, as `GIT_NAMESPACE` would say.
    namespace: Option<&'static str>,
    /// The state in which the hook fails, if any.
    veto: Option<&'static str>,
    edits: fn(&gix::Repository) -> TestResult<Vec<RefEdit>>,
}

fn rows() -> Vec<Row> {
    use PreviousValue::{Any, MustNotExist};
    vec![
        Row {
            name: "create",
            git_stdin: "create refs/heads/new main\n",
            veto: None,
            namespace: None,
            edits: |r| Ok(vec![update("refs/heads/new", id(r, "main")?, MustNotExist, false)]),
        },
        Row {
            name: "update without an expected value",
            git_stdin: "update refs/heads/old main\n",
            veto: None,
            namespace: None,
            edits: |r| Ok(vec![update("refs/heads/old", id(r, "main")?, Any, false)]),
        },
        Row {
            name: "update with an expected value",
            git_stdin: "update refs/heads/old main main~1\n",
            veto: None,
            namespace: None,
            edits: |r| {
                Ok(vec![update(
                    "refs/heads/old",
                    id(r, "main")?,
                    exists(id(r, "main~1")?),
                    false,
                )])
            },
        },
        Row {
            name: "delete a loose reference",
            git_stdin: "delete refs/heads/old\n",
            veto: None,
            namespace: None,
            edits: |_| Ok(vec![delete("refs/heads/old", Any, false)]),
        },
        Row {
            name: "delete a reference that is only packed",
            git_stdin: "delete refs/heads/packed\n",
            veto: None,
            namespace: None,
            edits: |_| Ok(vec![delete("refs/heads/packed", Any, false)]),
        },
        Row {
            name: "delete a reference that is packed and loose, with an expected value",
            git_stdin: "delete refs/heads/both main\n",
            veto: None,
            namespace: None,
            edits: |r| Ok(vec![delete("refs/heads/both", exists(id(r, "main")?), false)]),
        },
        Row {
            name: "delete a loose and a packed reference, and update another",
            git_stdin: "delete refs/heads/old\ndelete refs/heads/packed\nupdate refs/heads/main main~1\n",
            veto: None,
            namespace: None,
            edits: |r| {
                Ok(vec![
                    delete("refs/heads/old", Any, false),
                    delete("refs/heads/packed", Any, false),
                    update("refs/heads/main", id(r, "main~1")?, Any, false),
                ])
            },
        },
        Row {
            name: "update through HEAD",
            git_stdin: "update HEAD main~1\n",
            veto: None,
            namespace: None,
            edits: |r| Ok(vec![update("HEAD", id(r, "main~1")?, Any, true)]),
        },
        Row {
            name: "no changes at all",
            git_stdin: "",
            namespace: None,
            veto: None,
            edits: |_| Ok(vec![]),
        },
        Row {
            name: "update HEAD itself without following it",
            git_stdin: "option no-deref\nupdate HEAD main~1\n",
            namespace: None,
            veto: None,
            edits: |r| Ok(vec![update("HEAD", id(r, "main~1")?, Any, false)]),
        },
        Row {
            name: "refused while preparing",
            git_stdin: "update refs/heads/old main\n",
            namespace: None,
            veto: Some("preparing"),
            edits: |r| Ok(vec![update("refs/heads/old", id(r, "main")?, Any, false)]),
        },
        Row {
            name: "refused once prepared",
            git_stdin: "update refs/heads/old main\n",
            namespace: None,
            veto: Some("prepared"),
            edits: |r| Ok(vec![update("refs/heads/old", id(r, "main")?, Any, false)]),
        },
        Row {
            name: "a hook that fails after the commit changes nothing",
            git_stdin: "update refs/heads/old main\n",
            namespace: None,
            veto: Some("committed"),
            edits: |r| Ok(vec![update("refs/heads/old", id(r, "main")?, Any, false)]),
        },
        Row {
            name: "refused when deleting a packed reference once prepared",
            git_stdin: "delete refs/heads/packed\n",
            namespace: None,
            veto: Some("prepared"),
            edits: |_| Ok(vec![delete("refs/heads/packed", Any, false)]),
        },
        Row {
            name: "an expected value that isn't there",
            git_stdin: "update refs/heads/old main main\n",
            veto: None,
            namespace: None,
            edits: |r| {
                Ok(vec![update(
                    "refs/heads/old",
                    id(r, "main")?,
                    exists(id(r, "main")?),
                    false,
                )])
            },
        },
    ]
}

fn refs(repo: &gix::Repository) -> TestResult<String> {
    Ok(gix_testtools::git(
        repo.workdir().expect("non-bare"),
        "for-each-ref --format=%(refname)\\ %(objectname)",
    )?)
}

#[test]
fn the_hook_sees_what_it_sees_when_git_changes_references() -> TestResult {
    the_hook_sees_what_git_shows_it(false)
}

#[test]
#[cfg(feature = "reftable")]
fn the_hook_sees_what_it_sees_when_git_changes_references_in_reftables() -> TestResult {
    the_hook_sees_what_git_shows_it(true)
}

fn the_hook_sees_what_git_shows_it(reftable: bool) -> TestResult {
    let committer = gix::actor::SignatureRef {
        name: "committer".into(),
        email: "committer@example.com".into(),
        time: "0 +0000",
    };
    let mut problems = Vec::new();
    for row in rows() {
        let (git_repo, _keep_git) = repo(reftable)?;
        let (gix_repo, _keep_gix) = repo(reftable)?;
        let logs = gix_testtools::tempfile::tempdir()?;
        let mut transcripts = Vec::new();
        let mut outcomes = Vec::new();
        for (who, repo) in [("git", &git_repo), ("gix", &gix_repo)] {
            let log = logs.path().join(format!("{who}.log"));
            let veto = logs.path().join(format!("{who}.veto"));
            if let Some(state) = row.veto {
                std::fs::write(&veto, state)?;
            }
            install_hook(repo, &log, &veto)?;
            let succeeded = if who == "git" {
                let mut cmd = gix_testtools::git_command(repo.workdir().expect("non-bare"));
                cmd.args(["update-ref", "--stdin"]).stdin(std::process::Stdio::piped());
                if let Some(namespace) = row.namespace {
                    cmd.env("GIT_NAMESPACE", namespace);
                }
                let mut child = cmd.stderr(std::process::Stdio::null()).spawn()?;
                std::io::Write::write_all(&mut child.stdin.take().expect("piped"), row.git_stdin.as_bytes())?;
                child.wait()?.success()
            } else {
                let mut repo = repo.clone();
                if let Some(namespace) = row.namespace {
                    repo.set_namespace(namespace)?;
                }
                match repo.edit_references_with_hooks((row.edits)(&repo)?, Some(committer)) {
                    Ok(_) => true,
                    Err(err) => {
                        if row.veto == Some("preparing") || row.veto == Some("prepared") {
                            assert!(
                                err.downcast_any_ref::<Vetoed>().is_some(),
                                "{}: a refusal is marked as such: {err:?}",
                                row.name
                            );
                        }
                        false
                    }
                }
            };
            transcripts.push(std::fs::read_to_string(&log).unwrap_or_default());
            outcomes.push((succeeded, refs(repo)?));
        }
        assert!(
            transcripts[0].starts_with("preparing | "),
            "{}: Git called the hook, so the comparison is about something",
            row.name
        );
        if row.name == "delete a reference that is only packed" && !reftable {
            assert_eq!(
                transcripts[0].matches("prepared | ").count(),
                2,
                "Git runs a transaction on `packed-refs` inside the one that was asked for, which is what is pinned here"
            );
        }
        if transcripts[0] != transcripts[1] {
            problems.push(format!(
                "{}: the hook saw\n{}but Git showed it\n{}",
                row.name, transcripts[1], transcripts[0]
            ));
        }
        if outcomes[0] != outcomes[1] {
            problems.push(format!(
                "{}: the outcome and the references left are {:?} and not {:?}",
                row.name, outcomes[1], outcomes[0]
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    Ok(())
}

#[test]
fn a_repository_that_ignores_the_environment_changes_references_inside_a_quarantine_environment() -> TestResult {
    let (repo, _keep) = repo(false)?;
    let dir = repo.workdir().expect("non-bare").to_owned();
    let quarantine = gix_testtools::tempfile::tempdir()?;
    let _environment = gix_testtools::Env::new().set("GIT_QUARANTINE_PATH", quarantine.path().display().to_string());
    let mut git = gix_testtools::git_command(&dir);
    git.args(["update-ref", "refs/heads/by-git", "main"])
        .env("GIT_QUARANTINE_PATH", quarantine.path());
    assert!(
        !git.output()?.status.success(),
        "Git refuses to change references inside a quarantine environment, which is the reason for the default"
    );

    let repo = gix::open_opts(
        repo.git_dir(),
        crate::util::restricted().ignore_repository_environment(true),
    )?;
    let main = id(&repo, "main")?;
    repo.edit_references([update("refs/heads/new", main, PreviousValue::MustNotExist, false)])?;
    assert_eq!(
        repo.find_reference("refs/heads/new")?.id(),
        main,
        "ignoring the repository environment includes the one that says a quarantine is in effect"
    );

    let logs = gix_testtools::tempfile::tempdir()?;
    let log = logs.path().join("hook.log");
    install_hook(&repo, &log, &logs.path().join("veto"))?;
    repo.edit_references_with_hooks(
        [update("refs/heads/hooked", main, PreviousValue::MustNotExist, false)],
        Some(gix::actor::SignatureRef {
            name: "committer".into(),
            email: "committer@example.com".into(),
            time: "0 +0000",
        }),
    )?;
    assert!(
        std::fs::read_to_string(&log)?.contains("committed"),
        "the hook is run as without a quarantine environment, as the references are changed"
    );
    Ok(())
}

#[test]
fn nothing_changes_inside_a_quarantine_environment_and_the_hook_isnt_called() -> TestResult {
    let (repo, _keep) = repo(false)?;
    let logs = gix_testtools::tempfile::tempdir()?;
    let log = logs.path().join("hook.log");
    install_hook(&repo, &log, &logs.path().join("veto"))?;
    let before = refs(&repo)?;
    let _environment = gix_testtools::Env::new().set("GIT_QUARANTINE_PATH", logs.path().display().to_string());
    let err = repo
        .edit_references_with_hooks(
            [update("refs/heads/new", id(&repo, "main")?, PreviousValue::Any, false)],
            None,
        )
        .expect_err("Git refuses as well");
    assert!(err.to_string().contains("quarantine"), "{err:?}");
    repo.edit_references([update("refs/heads/new", id(&repo, "main")?, PreviousValue::Any, false)])
        .expect_err("the other ways to edit references refuse as well, as Git refuses them all");
    assert!(!log.exists(), "the hook isn't even told, as with Git");
    assert_eq!(refs(&repo)?, before);
    Ok(())
}
