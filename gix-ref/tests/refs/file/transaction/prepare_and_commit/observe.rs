//! The states that Git's `reference-transaction` hook sees for a transaction, compared with what an
//! [`Observer`](gix_ref::transaction::observe::Observer) of the files store sees for the same edits.
//!
//! Each row runs Git with a hook that logs its arguments and standard input on one repository, and the files store on a
//! copy of it with an observer that logs the same. A hook can't tell the transaction on `packed-refs` that Git
//! opens for deletions from the one that is asked for, so the scope of events is dropped before comparing.
//! The rows are those of `docs/push-durability-evidence.md`, and the order of events in a failing transaction.
#![cfg(unix)]
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use gix_lock::acquire::Fail;
use gix_ref::{
    Target,
    transaction::{
        Change, LogChange, PreviousValue, RefEdit, RefLog,
        observe::{Event, Observer, Veto, Vetoed},
    },
};
use gix_testtools::TestResult;

/// What a caller expects of the previous value.
#[derive(Clone, Copy)]
enum Expect {
    Any,
    Absent,
    /// A commit, `C1` or `C2`.
    Commit(&'static str),
}

#[derive(Clone, Copy)]
enum Edit {
    Update {
        name: &'static str,
        new: &'static str,
        expect: Expect,
    },
    /// Update through symbolic references like `HEAD`.
    UpdateDeref {
        name: &'static str,
        new: &'static str,
    },
    Delete {
        name: &'static str,
        expect: Expect,
    },
    /// Delete through symbolic references like `HEAD`.
    DeleteDeref {
        name: &'static str,
    },
    Symbolic {
        name: &'static str,
        target: &'static str,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum Finish {
    Commit,
    /// Prepare, and drop the transaction like `update-ref --stdin`'s `abort` does.
    Abort,
}

/// Where to refuse, as the number of the occurrence of a state, counting from 1.
type VetoAt = Option<(&'static str, usize)>;

struct Row {
    name: &'static str,
    /// Git commands to run before the hook is installed, with `C1` and `C2` replaced by the commits.
    setup: &'static [&'static [&'static str]],
    /// The arguments for Git, with `C1` and `C2` replaced, and what to feed it.
    git: (&'static [&'static str], Option<&'static str>),
    /// The same in terms of edits.
    edits: &'static [Edit],
    finish: Finish,
    veto: VetoAt,
    /// Create `refs/heads/main.lock` before to make locking fail.
    lock_conflict: bool,
    /// Create `packed-refs.lock` before to make locking `packed-refs` fail.
    packed_lock_conflict: bool,
}

const ROW: Row = Row {
    name: "",
    setup: &[],
    git: (&[], None),
    edits: &[],
    finish: Finish::Commit,
    veto: None,
    lock_conflict: false,
    packed_lock_conflict: false,
};

/// The state of each repository: `main` at `C2`, and `HEAD` pointing to it.
const BASE: &[&[&str]] = &[&["update-ref", "refs/heads/main", "C2"]];
const WITH_PACKED: &[&[&str]] = &[
    &["update-ref", "refs/heads/main", "C2"],
    &["update-ref", "refs/heads/packed", "C1"],
    &["pack-refs", "--all"],
];

fn rows() -> Vec<Row> {
    use Edit::{Delete, DeleteDeref, Symbolic, Update, UpdateDeref};
    use Expect::{Absent, Any, Commit};
    vec![
        Row {
            name: "create",
            setup: BASE,
            git: (&["update-ref", "refs/heads/new", "C1"], None),
            edits: &[Update {
                name: "refs/heads/new",
                new: "C1",
                expect: Any,
            }],
            ..ROW
        },
        Row {
            name: "update with old value",
            setup: BASE,
            git: (&["update-ref", "refs/heads/main", "C1", "C2"], None),
            edits: &[Update {
                name: "refs/heads/main",
                new: "C1",
                expect: Commit("C2"),
            }],
            ..ROW
        },
        Row {
            name: "update without change",
            setup: BASE,
            git: (&["update-ref", "refs/heads/main", "C2"], None),
            edits: &[Update {
                name: "refs/heads/main",
                new: "C2",
                expect: Any,
            }],
            ..ROW
        },
        Row {
            name: "delete loose reference",
            setup: BASE,
            git: (&["update-ref", "-d", "refs/heads/main"], None),
            edits: &[Delete {
                name: "refs/heads/main",
                expect: Any,
            }],
            ..ROW
        },
        Row {
            name: "delete packed reference",
            setup: WITH_PACKED,
            git: (&["update-ref", "-d", "refs/heads/packed"], None),
            edits: &[Delete {
                name: "refs/heads/packed",
                expect: Any,
            }],
            ..ROW
        },
        Row {
            name: "delete reference that doesn't exist",
            setup: BASE,
            git: (&["update-ref", "-d", "refs/heads/nope"], None),
            edits: &[Delete {
                name: "refs/heads/nope",
                expect: Any,
            }],
            ..ROW
        },
        Row {
            name: "stale old value",
            setup: BASE,
            git: (&["update-ref", "refs/heads/main", "C2", "C1"], None),
            edits: &[Update {
                name: "refs/heads/main",
                new: "C2",
                expect: Commit("C1"),
            }],
            ..ROW
        },
        Row {
            name: "create what exists",
            setup: BASE,
            git: (&["update-ref", "refs/heads/main", "C1", "Z"], None),
            edits: &[Update {
                name: "refs/heads/main",
                new: "C1",
                expect: Absent,
            }],
            ..ROW
        },
        Row {
            name: "lock conflict",
            setup: BASE,
            git: (
                &[
                    "-c",
                    "core.filesRefLockTimeout=0",
                    "update-ref",
                    "refs/heads/main",
                    "C1",
                ],
                None,
            ),
            edits: &[Update {
                name: "refs/heads/main",
                new: "C1",
                expect: Any,
            }],
            lock_conflict: true,
            ..ROW
        },
        Row {
            name: "veto while preparing",
            setup: BASE,
            git: (&["update-ref", "refs/heads/main", "C1"], None),
            edits: &[Update {
                name: "refs/heads/main",
                new: "C1",
                expect: Any,
            }],
            veto: Some(("preparing", 1)),
            ..ROW
        },
        Row {
            name: "veto once prepared",
            setup: BASE,
            git: (&["update-ref", "refs/heads/main", "C1"], None),
            edits: &[Update {
                name: "refs/heads/main",
                new: "C1",
                expect: Any,
            }],
            veto: Some(("prepared", 1)),
            ..ROW
        },
        Row {
            name: "veto of committed is ignored",
            setup: BASE,
            git: (&["update-ref", "refs/heads/main", "C1"], None),
            edits: &[Update {
                name: "refs/heads/main",
                new: "C1",
                expect: Any,
            }],
            veto: Some(("committed", 1)),
            ..ROW
        },
        Row {
            name: "several edits",
            setup: BASE,
            git: (
                &["update-ref", "--stdin"],
                Some("update refs/heads/main C1 C2\ncreate refs/heads/a C1\ncreate refs/heads/b C2\n"),
            ),
            edits: &[
                Update {
                    name: "refs/heads/main",
                    new: "C1",
                    expect: Commit("C2"),
                },
                Update {
                    name: "refs/heads/a",
                    new: "C1",
                    expect: Absent,
                },
                Update {
                    name: "refs/heads/b",
                    new: "C2",
                    expect: Absent,
                },
            ],
            ..ROW
        },
        Row {
            name: "prepared and abandoned",
            setup: BASE,
            git: (
                &["update-ref", "--stdin"],
                Some("start\ncreate refs/heads/c C1\nprepare\nabort\n"),
            ),
            edits: &[Update {
                name: "refs/heads/c",
                new: "C1",
                expect: Absent,
            }],
            finish: Finish::Abort,
            ..ROW
        },
        Row {
            name: "one of several is stale",
            setup: BASE,
            git: (
                &["update-ref", "--stdin"],
                Some("create refs/heads/d C1\nupdate refs/heads/main C2 C1\n"),
            ),
            edits: &[
                Update {
                    name: "refs/heads/d",
                    new: "C1",
                    expect: Absent,
                },
                Update {
                    name: "refs/heads/main",
                    new: "C2",
                    expect: Commit("C1"),
                },
            ],
            ..ROW
        },
        Row {
            name: "update through HEAD",
            setup: BASE,
            git: (&["update-ref", "HEAD", "C1"], None),
            edits: &[UpdateDeref {
                name: "HEAD",
                new: "C1",
            }],
            ..ROW
        },
        Row {
            name: "HEAD and another reference come in the order of the split",
            setup: BASE,
            git: (
                &["update-ref", "--stdin"],
                Some("update HEAD C1\nupdate refs/heads/a C1\n"),
            ),
            edits: &[
                UpdateDeref {
                    name: "HEAD",
                    new: "C1",
                },
                Update {
                    name: "refs/heads/a",
                    new: "C1",
                    expect: Any,
                },
            ],
            ..ROW
        },
        Row {
            name: "point HEAD to a branch",
            setup: BASE,
            git: (&["symbolic-ref", "HEAD", "refs/heads/a"], None),
            edits: &[Symbolic {
                name: "HEAD",
                target: "refs/heads/a",
            }],
            ..ROW
        },
        Row {
            name: "delete then stale update fails after a deletion was locked",
            setup: WITH_PACKED,
            git: (
                &["update-ref", "--stdin"],
                Some("delete refs/heads/packed\nupdate refs/heads/main C1 C1\n"),
            ),
            edits: &[
                Delete {
                    name: "refs/heads/packed",
                    expect: Any,
                },
                Update {
                    name: "refs/heads/main",
                    new: "C1",
                    expect: Commit("C1"),
                },
            ],
            ..ROW
        },
        Row {
            name: "stale update then delete fails before any deletion was locked",
            setup: WITH_PACKED,
            git: (
                &["update-ref", "--stdin"],
                Some("update refs/heads/main C1 C1\ndelete refs/heads/packed\n"),
            ),
            edits: &[
                Update {
                    name: "refs/heads/main",
                    new: "C1",
                    expect: Commit("C1"),
                },
                Delete {
                    name: "refs/heads/packed",
                    expect: Any,
                },
            ],
            ..ROW
        },
        Row {
            name: "veto in the transaction on packed-refs",
            setup: WITH_PACKED,
            git: (&["update-ref", "-d", "refs/heads/packed"], None),
            edits: &[Delete {
                name: "refs/heads/packed",
                expect: Any,
            }],
            veto: Some(("prepared", 1)),
            ..ROW
        },
        Row {
            name: "veto once prepared with a transaction on packed-refs",
            setup: WITH_PACKED,
            git: (&["update-ref", "-d", "refs/heads/packed"], None),
            edits: &[Delete {
                name: "refs/heads/packed",
                expect: Any,
            }],
            veto: Some(("prepared", 2)),
            ..ROW
        },
        Row {
            name: "delete through HEAD",
            setup: BASE,
            git: (&["update-ref", "-d", "HEAD"], None),
            edits: &[DeleteDeref { name: "HEAD" }],
            ..ROW
        },
        Row {
            name: "duplicate of the target of a symbolic reference",
            setup: BASE,
            git: (
                &["update-ref", "--stdin"],
                Some("update HEAD C1\ndelete refs/heads/main\n"),
            ),
            edits: &[
                UpdateDeref {
                    name: "HEAD",
                    new: "C1",
                },
                Delete {
                    name: "refs/heads/main",
                    expect: Any,
                },
            ],
            ..ROW
        },
        Row {
            name: "duplicate of the target of a symbolic reference after a deletion",
            setup: &[
                &["update-ref", "refs/heads/main", "C2"],
                &["update-ref", "refs/heads/x", "C1"],
            ],
            git: (
                &["update-ref", "--stdin"],
                Some("delete refs/heads/x\nupdate HEAD C1\ndelete refs/heads/main\n"),
            ),
            edits: &[
                Delete {
                    name: "refs/heads/x",
                    expect: Any,
                },
                UpdateDeref {
                    name: "HEAD",
                    new: "C1",
                },
                Delete {
                    name: "refs/heads/main",
                    expect: Any,
                },
            ],
            ..ROW
        },
        Row {
            name: "packed-refs can't be locked",
            setup: BASE,
            git: (
                &["-c", "core.packedRefsTimeout=0", "update-ref", "--stdin"],
                Some("delete refs/heads/x\nupdate refs/heads/y C1\n"),
            ),
            edits: &[
                Delete {
                    name: "refs/heads/x",
                    expect: Any,
                },
                Update {
                    name: "refs/heads/y",
                    new: "C1",
                    expect: Any,
                },
            ],
            packed_lock_conflict: true,
            ..ROW
        },
        Row {
            name: "duplicate references",
            setup: BASE,
            git: (
                &["update-ref", "--stdin"],
                Some("create refs/heads/x C1\ncreate refs/heads/x C2\n"),
            ),
            edits: &[
                Update {
                    name: "refs/heads/x",
                    new: "C1",
                    expect: Absent,
                },
                Update {
                    name: "refs/heads/x",
                    new: "C2",
                    expect: Absent,
                },
            ],
            ..ROW
        },
    ]
}

struct Repo {
    dir: PathBuf,
    c1: String,
    c2: String,
    null: String,
}

const COMMITTER: [(&str, &str); 3] = [
    ("GIT_COMMITTER_NAME", "committer"),
    ("GIT_COMMITTER_EMAIL", "committer@example.com"),
    ("GIT_COMMITTER_DATE", "@1234 +0800"),
];

fn git(dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, String)]) -> TestResult<(bool, String)> {
    use std::io::Write;
    let mut cmd = gix_testtools::git_command(dir);
    cmd.args(args)
        .envs(COMMITTER)
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    if let Some(input) = stdin {
        child.stdin.take().expect("piped").write_all(input.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    Ok((
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    ))
}

/// A repository with two commits whose ids are the same in every repository, and `HEAD` pointing to `main`.
fn repo(dir: &Path) -> TestResult<Repo> {
    std::fs::create_dir_all(dir)?;
    git(dir, &["init", "-q", "-b", "main"], None, &[])?;
    let (_, tree) = git(dir, &["hash-object", "-w", "-t", "tree", "--stdin"], None, &[])?;
    let author: Vec<_> = ["AUTHOR", "COMMITTER"]
        .iter()
        .flat_map(|who| {
            [
                (format!("GIT_{who}_NAME"), "committer"),
                (format!("GIT_{who}_EMAIL"), "committer@example.com"),
                (format!("GIT_{who}_DATE"), "@1234 +0800"),
            ]
        })
        .collect();
    let commit = |parent: Option<&str>| -> TestResult<String> {
        let mut cmd = gix_testtools::git_command(dir);
        cmd.args(["commit-tree", tree.trim(), "-m", "c"]);
        if let Some(parent) = parent {
            cmd.args(["-p", parent]);
        }
        for (key, value) in &author {
            cmd.env(key, value);
        }
        let out = cmd.output()?;
        assert!(out.status.success(), "commit-tree failed");
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    };
    let c1 = commit(None)?;
    let c2 = commit(Some(&c1))?;
    let null = "0".repeat(c1.len());
    Ok(Repo {
        dir: dir.to_owned(),
        c1,
        c2,
        null,
    })
}

impl Repo {
    fn fill(&self, text: &str) -> String {
        text.replace("C1", &self.c1)
            .replace("C2", &self.c2)
            .replace('Z', &self.null)
    }

    fn fill_all(&self, args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| self.fill(arg)).collect()
    }

    /// Turn what an observer or a hook saw into a text with `C1`, `C2` and `Z` in place of ids.
    fn label(&self, text: &str) -> String {
        text.replace(&self.c1, "C1")
            .replace(&self.c2, "C2")
            .replace(&self.null, "Z")
    }

    fn setup(&self, commands: &[&[&str]]) -> TestResult {
        for command in commands {
            let args = self.fill_all(command);
            let args: Vec<_> = args.iter().map(String::as_str).collect();
            let (ok, output) = git(&self.dir, &args, None, &[])?;
            assert!(ok, "setup {command:?} failed: {output}");
        }
        Ok(())
    }

    fn id(&self, label: &str) -> gix_hash::ObjectId {
        let hex = match label {
            "C1" => &self.c1,
            "C2" => &self.c2,
            other => unreachable!("unknown commit {other}"),
        };
        gix_hash::ObjectId::from_hex(hex.as_bytes()).expect("valid hex")
    }

    fn edit(&self, edit: Edit) -> RefEdit {
        let expected = |expect: Expect| match expect {
            Expect::Any => PreviousValue::Any,
            Expect::Absent => PreviousValue::MustNotExist,
            Expect::Commit(label) => PreviousValue::MustExistAndMatch(Target::Object(self.id(label))),
        };
        let log = LogChange {
            mode: RefLog::AndReference,
            force_create_reflog: false,
            message: "m".into(),
        };
        match edit {
            Edit::Update { name, new, expect } => RefEdit::update_with_log(
                name.try_into().expect("valid name"),
                Target::Object(self.id(new)),
                expected(expect),
                log,
            ),
            Edit::UpdateDeref { name, new } => RefEdit::update_with_log(
                name.try_into().expect("valid name"),
                Target::Object(self.id(new)),
                PreviousValue::Any,
                log,
            )
            .with_deref(true),
            Edit::Delete { name, expect } => RefEdit {
                change: Change::Delete {
                    expected: expected(expect),
                    log: RefLog::AndReference,
                },
                name: name.try_into().expect("valid name"),
                deref: false,
            },
            Edit::DeleteDeref { name } => RefEdit {
                change: Change::Delete {
                    expected: PreviousValue::Any,
                    log: RefLog::AndReference,
                },
                name: name.try_into().expect("valid name"),
                deref: true,
            },
            Edit::Symbolic { name, target } => RefEdit::update_with_log(
                name.try_into().expect("valid name"),
                Target::Symbolic(target.try_into().expect("valid name")),
                PreviousValue::Any,
                log,
            ),
        }
    }
}

/// Logs events like the hook, with a line per event consisting of the state, `|`, and `old new name|` per update.
struct Recorder {
    log: Rc<RefCell<Vec<String>>>,
    veto: VetoAt,
    seen: HashMap<&'static str, usize>,
    /// The id that stands for no value.
    null: String,
}

impl Observer for Recorder {
    fn observe(&mut self, event: &Event<'_>) -> Result<(), Veto> {
        use std::fmt::Write;
        let target = |target: Option<&Target>, null: &str| match target {
            None => null.to_owned(),
            Some(Target::Object(id)) => id.to_string(),
            Some(Target::Symbolic(name)) => format!("ref:{name}"),
        };
        let mut line = format!("{} | ", event.state.as_str());
        for update in event.updates {
            write!(
                line,
                "{} {} {}|",
                target(update.previous.as_ref(), &self.null),
                target(update.new.as_ref(), &self.null),
                update.name.as_bstr()
            )
            .expect("writing to a string");
        }
        self.log.borrow_mut().push(line);
        if let Some((state, nth)) = self.veto
            && event.state.as_str() == state
        {
            let seen = self.seen.entry(state).or_default();
            *seen += 1;
            if *seen == nth {
                return Err(Veto);
            }
        }
        Ok(())
    }
}

fn hook(repo: &Repo, log: &Path) -> TestResult {
    let script = format!(
        "#!/bin/sh\nstate=$1\nlines=$(cat | tr '\\n' '|')\necho \"$state | $lines\" >> '{log}'\n\
         if [ \"${{VETO_STATE:-}}\" = \"$state\" ]; then\n\
           n=$(cat '{log}.'$state 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{log}.'$state\n\
           if [ \"$n\" = \"${{VETO_NTH:-1}}\" ]; then exit 1; fi\nfi\n",
        log = log.display()
    );
    let path = repo.dir.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(path.parent().expect("has parent"))?;
    std::fs::write(&path, script)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[test]
fn events_match_the_reference_transaction_hook_of_git() -> TestResult {
    let root = gix_testtools::tempfile::tempdir()?;
    let mut failures = Vec::new();
    for (idx, row) in rows().iter().enumerate() {
        let git_repo = repo(&root.path().join(format!("git-{idx}")))?;
        let fork_repo = repo(&root.path().join(format!("fork-{idx}")))?;
        for repo in [&git_repo, &fork_repo] {
            repo.setup(row.setup)?;
            if row.lock_conflict {
                std::fs::write(repo.dir.join(".git/refs/heads/main.lock"), "")?;
            }
            if row.packed_lock_conflict {
                std::fs::write(repo.dir.join(".git/packed-refs.lock"), "")?;
            }
        }

        // Git
        let log = root.path().join(format!("hook-{idx}.log"));
        std::fs::write(&log, "")?;
        hook(&git_repo, &log)?;
        let args = git_repo.fill_all(row.git.0);
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        let mut env = Vec::new();
        if let Some((state, nth)) = row.veto {
            env.push(("VETO_STATE", state.to_owned()));
            env.push(("VETO_NTH", nth.to_string()));
        }
        let stdin = row.git.1.map(|stdin| git_repo.fill(stdin));
        let (git_ok, _) = git(&git_repo.dir, &args, stdin.as_deref(), &env)?;
        let git_events: Vec<_> = std::fs::read_to_string(&log)?
            .lines()
            .map(|line| git_repo.label(line))
            .collect();

        // The fork
        let events = Rc::new(RefCell::new(Vec::new()));
        let store = gix_ref::file::Store::at(fork_repo.dir.join(".git"), crate::fixture_hash_kind());
        let committer = gix_actor::Signature {
            name: "committer".into(),
            email: "committer@example.com".into(),
            time: gix_date::parse_header("1234 +0800").expect("valid"),
        };
        let mut buf = Default::default();
        let edits: Vec<_> = row.edits.iter().map(|edit| fork_repo.edit(*edit)).collect();
        let transaction = store
            .transaction()
            .observer(Recorder {
                log: events.clone(),
                veto: row.veto,
                seen: HashMap::new(),
                null: fork_repo.null.clone(),
            })
            .prepare(edits, Fail::Immediately, Fail::Immediately);
        let (fork_ok, vetoed) = match transaction {
            Ok(transaction) if row.finish == Finish::Abort => {
                drop(transaction);
                (true, false)
            }
            Ok(transaction) => (transaction.commit(committer.to_ref(&mut buf)).is_ok(), false),
            Err(err) => (false, err.downcast_any_ref::<Vetoed>().is_some()),
        };
        let fork_events: Vec<_> = events.borrow().iter().map(|line| fork_repo.label(line)).collect();

        assert!(
            !git_events.is_empty() || row.name == "duplicate references",
            "{}: the hook ran, or the row tests nothing",
            row.name
        );
        if std::env::var_os("GIX_PRINT_EVENTS").is_some() {
            eprintln!("{} (ok={git_ok}): {git_events:?}", row.name);
        }
        if git_ok != fork_ok || git_events != fork_events {
            failures.push(format!(
                "{}:\n  git  (ok={git_ok}): {git_events:?}\n  fork (ok={fork_ok}): {fork_events:?}",
                row.name
            ));
        }
        if row.veto.is_some_and(|(state, _)| state != "committed") {
            assert!(vetoed, "{}: a veto is reported as such", row.name);
        }
    }
    assert!(failures.is_empty(), "differences from Git:\n{}", failures.join("\n"));
    Ok(())
}

mod after_preparing {
    use std::{cell::RefCell, rc::Rc};

    use gix_lock::acquire::Fail;
    use gix_ref::transaction::{
        PreviousValue, RefEdit,
        observe::{Event, Scope, State, Veto},
    };
    use gix_testtools::TestResult;

    use crate::file::transaction::prepare_and_commit::{committer, create_at, empty_store};

    type Seen = Rc<RefCell<Vec<(Scope, State)>>>;

    fn observer(seen: &Seen) -> impl FnMut(&Event<'_>) -> Result<(), Veto> + use<> {
        let seen = seen.clone();
        move |event: &Event<'_>| {
            seen.borrow_mut().push((event.scope, event.state));
            Ok(())
        }
    }

    /// Git says nothing after it failed to finish a transaction. Reporting an abort would claim that nothing happened when
    /// something may have, which is what the [error](gix_ref::transaction::commit_failure::Uncertain) is for.
    #[test]
    fn failing_to_commit_leaves_the_observer_with_what_it_saw_while_preparing() -> TestResult {
        let (dir, store) = empty_store()?;
        let blocked = dir.path().join("logs/refs/heads/b");
        std::fs::create_dir_all(&blocked)?;
        std::fs::write(blocked.join("content"), "makes this a directory that is not empty")?;

        let seen = Seen::default();
        let transaction = store.transaction().observer(observer(&seen)).prepare(
            [create_at("refs/heads/a"), create_at("refs/heads/b")],
            Fail::Immediately,
            Fail::Immediately,
        )?;
        transaction
            .commit(committer().to_ref(&mut Default::default()))
            .expect_err("the second reflog can't be written");
        assert_eq!(
            *seen.borrow(),
            [
                (Scope::Transaction, State::Preparing),
                (Scope::Transaction, State::Prepared)
            ],
            "neither committed nor aborted is reported"
        );
        Ok(())
    }

    /// Nothing was changed if there is no committer for a reflog entry, so the transaction is known to be abandoned, which
    /// is reported when the locks are gone, as Git does.
    #[test]
    fn failing_before_the_first_change_is_reported_as_aborted_after_unlocking() -> TestResult {
        let (dir, store) = empty_store()?;
        let lock = dir.path().join("refs/heads/a.lock");
        let lock_was_held = Rc::new(RefCell::new(Vec::new()));
        let seen = Seen::default();
        let mut observe = observer(&seen);
        store
            .transaction()
            .observer({
                let (lock_was_held, lock) = (lock_was_held.clone(), lock.clone());
                move |event: &Event<'_>| {
                    lock_was_held.borrow_mut().push((event.state, lock.is_file()));
                    observe(event)
                }
            })
            .prepare([create_at("refs/heads/a")], Fail::Immediately, Fail::Immediately)?
            .commit(None)
            .expect_err("a reflog entry needs a committer");
        assert_eq!(
            seen.borrow().last(),
            Some(&(Scope::Transaction, State::Aborted)),
            "the observer learns that nothing will follow"
        );
        assert_eq!(
            *lock_was_held.borrow(),
            [
                (State::Preparing, false),
                (State::Prepared, true),
                (State::Aborted, false)
            ],
            "the lock file exists while prepared and is gone when the observer learns about the abort"
        );
        Ok(())
    }

    /// Edits that only write reflogs are not references that change, so they are not reported.
    #[test]
    fn reflog_only_edits_are_not_reported() -> TestResult {
        let (_dir, store) = empty_store()?;
        let updates = Rc::new(RefCell::new(Vec::new()));
        let mut edit = create_at("refs/heads/a");
        if let gix_ref::transaction::Change::Update { log, .. } = &mut edit.change {
            log.mode = gix_ref::transaction::RefLog::Only;
        }
        store
            .transaction()
            .observer({
                let updates = updates.clone();
                move |event: &Event<'_>| {
                    updates.borrow_mut().push(event.updates.len());
                    Ok(())
                }
            })
            .prepare([edit], Fail::Immediately, Fail::Immediately)?
            .commit(committer().to_ref(&mut Default::default()))?;
        assert_eq!(
            *updates.borrow(),
            [0, 0, 0],
            "preparing, prepared and committed see no update"
        );
        Ok(())
    }

    /// Updates that are packed have no counterpart in Git, but are reported like the deletions that Git does pack.
    #[test]
    fn packing_updates_is_reported_as_a_transaction_on_packed_refs() -> TestResult {
        use gix_ref::file::transaction::PackedRefs;

        let (_dir, store) = empty_store()?;
        let seen = Seen::default();
        store
            .transaction()
            .packed_refs(PackedRefs::DeletionsAndNonSymbolicUpdates(Box::new(
                crate::file::EmptyCommit,
            )))
            .observer(observer(&seen))
            .prepare([create_at("refs/heads/a")], Fail::Immediately, Fail::Immediately)?
            .commit(committer().to_ref(&mut Default::default()))?;
        assert_eq!(
            *seen.borrow(),
            [
                (Scope::Transaction, State::Preparing),
                (Scope::PackedRefs, State::Preparing),
                (Scope::PackedRefs, State::Prepared),
                (Scope::Transaction, State::Prepared),
                (Scope::PackedRefs, State::Committed),
                (Scope::Transaction, State::Committed),
            ]
        );
        Ok(())
    }

    /// Git commits the transaction on `packed-refs` in the middle of committing, before the loose files are removed. If
    /// that fails afterwards, the outer transaction has no final state.
    #[cfg(unix)]
    #[test]
    fn the_transaction_on_packed_refs_is_committed_before_loose_references_are_removed() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        // SAFETY: `geteuid()` has no preconditions and no side effects.
        #[allow(unsafe_code)]
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("SKIPPED: the superuser ignores permissions");
            return Ok(());
        }
        let (dir, store) = empty_store()?;
        store
            .transaction()
            .prepare([create_at("refs/heads/a")], Fail::Immediately, Fail::Immediately)?
            .commit(committer().to_ref(&mut Default::default()))?;
        std::fs::write(
            dir.path().join("packed-refs"),
            format!(
                "# pack-refs with: peeled fully-peeled sorted \n{} refs/heads/a\n",
                crate::hex_to_id("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")
            ),
        )?;

        let seen = Seen::default();
        let transaction = store.transaction().observer(observer(&seen)).prepare(
            [RefEdit::delete("refs/heads/a".try_into()?, PreviousValue::Any)],
            Fail::Immediately,
            Fail::Immediately,
        )?;
        let heads = dir.path().join("refs/heads");
        let permissions = std::fs::metadata(&heads)?.permissions();
        std::fs::set_permissions(&heads, std::fs::Permissions::from_mode(0o555))?;
        let result = transaction.commit(None);
        std::fs::set_permissions(&heads, permissions)?;
        result.expect_err("the loose reference can't be removed");
        assert_eq!(
            *seen.borrow(),
            [
                (Scope::Transaction, State::Preparing),
                (Scope::PackedRefs, State::Preparing),
                (Scope::PackedRefs, State::Prepared),
                (Scope::Transaction, State::Prepared),
                (Scope::PackedRefs, State::Committed),
            ],
            "the transaction on packed-refs was committed, and the outer one has no final state"
        );
        Ok(())
    }
}
