//! A fetch is received by `git fetch` and by the native fetch with `fetch.fsckObjects` and its relatives, and both have to refuse
//! what they refuse, and say the same about it and about what they accept with a warning. The remotes are made by
//! `make_fsck_remotes.sh`, which says what they contain; its `client` is the repository that is fetched from.
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use gix::remote::Direction::Fetch;
use gix_testtools::TestResult;

/// Branches that Git refuses with `fetch.fsckObjects`, and which are accepted without.
const REFUSED: &[&str] = &[
    "missing-committer",
    "bad-email",
    "bad-timezone",
    "zero-padded-date",
    "two-flaws",
    "zero-padded-mode",
    "unsorted",
    "duplicate",
    "dotgit",
    "dotgit-ntfs",
    "full-pathname",
    "dot-entry",
    "dotdot-entry",
    "symlink-gitmodules",
    "gitmodules-bad-url",
    "gitmodules-bad-name",
];

/// Branches that pass, and which of them have something to say, which is a warning.
const ACCEPTED: &[(&str, bool)] = &[
    ("ok", false),
    ("extra-header", false),
    ("gitmodules-ok", false),
    ("bad-mode-info", true),
];

const CHECK: &[&str] = &["config fetch.fsckObjects true"];

/// What is left in a repository after a fetch, as far as it is the same for both.
#[derive(Debug, PartialEq, Eq)]
struct Left {
    refs: String,
    objects: String,
    shallow: String,
    packs: Vec<String>,
    /// What is said about objects, line by line.
    messages: Vec<String>,
}

/// What happened to a fetch.
struct Fetched {
    accepted: bool,
    left: Left,
    /// What is left of a fetch in the object directory that Git and the native fetch leave differently.
    leftovers: Vec<String>,
}

fn client() -> TestResult<(PathBuf, PathBuf)> {
    let fixture = gix_testtools::scripted_fixture_read_only("make_fsck_remotes.sh")?.canonicalize()?;
    Ok((fixture.join("files"), fixture.join("files").join("client")))
}

fn copy_dir(from: &Path, to: &Path) -> TestResult {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// What a fetch of `refspec` into the repository `dest` is to do.
#[derive(Clone)]
struct Plan<'a> {
    /// The directory of the repository in the fixture that is copied to make the one to fetch into.
    into: &'a str,
    /// Git arguments to apply to it beforehand, like `config fetch.fsckObjects true`.
    setup: &'a [&'a str],
    /// The branch to fetch, which is what `refs/heads/<branch>` of the client is.
    branch: &'a str,
    depth: Option<u32>,
}

fn fetch<'a>(into: &'a str, setup: &'a [&'a str], branch: &'a str) -> Plan<'a> {
    Plan {
        into,
        setup,
        branch,
        depth: None,
    }
}

fn prepare(fixture: &Path, tmp: &Path, name: &str, what: &Plan<'_>) -> TestResult<PathBuf> {
    let dest = tmp.join(name);
    copy_dir(&fixture.join(what.into), &dest)?;
    for args in what.setup {
        gix_testtools::git(&dest, args)?;
    }
    Ok(dest)
}

fn lines_about_objects(text: &str) -> Vec<String> {
    let mut lines: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("error: object ") || line.starts_with("warning: object "))
        .map(ToOwned::to_owned)
        .collect();
    lines.sort();
    lines
}

fn left(dest: &Path, messages: Vec<String>) -> TestResult<(Left, Vec<String>)> {
    let list = |args: &str| -> TestResult<String> { Ok(gix_testtools::git(dest, args)?) };
    let mut packs = Vec::new();
    let mut leftovers = Vec::new();
    let pack_dir = dest.join("objects").join("pack");
    if pack_dir.is_dir() {
        for entry in std::fs::read_dir(&pack_dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.starts_with("tmp_") {
                leftovers.push(name);
            } else {
                // The pack is named after its hash, which depends on how the objects were compressed.
                packs.push(match name.rsplit_once('.') {
                    Some((_, extension)) => format!("pack-X.{extension}"),
                    None => name,
                });
            }
        }
    }
    packs.sort();
    // Objects, in an order that doesn't depend on how they are stored.
    let mut objects: Vec<_> = list("cat-file --batch-all-objects --batch-check")?
        .lines()
        .map(ToOwned::to_owned)
        .collect();
    objects.sort();
    Ok((
        Left {
            refs: list("for-each-ref")?,
            objects: objects.join("\n"),
            shallow: std::fs::read_to_string(dest.join("shallow")).unwrap_or_default(),
            packs,
            messages,
        },
        leftovers,
    ))
}

fn with_git(fixture: &Path, tmp: &Path, client: &Path, what: &Plan<'_>) -> TestResult<Fetched> {
    let dest = prepare(fixture, tmp, "git", what)?;
    let mut command = gix_testtools::git_command(&dest);
    command.arg("fetch").arg("--no-tags").arg("--no-write-fetch-head");
    if let Some(depth) = what.depth {
        command.arg(format!("--depth={depth}"));
    }
    let output = command
        .arg(format!("file://{}", client.display()))
        .arg(format!("+refs/heads/{0}:refs/heads/{0}", what.branch))
        .output()?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let (left, leftovers) = left(&dest, lines_about_objects(&stderr))?;
    Ok(Fetched {
        accepted: output.status.success(),
        left,
        leftovers,
    })
}

fn with_gix(fixture: &Path, tmp: &Path, client: &Path, what: &Plan<'_>) -> TestResult<Fetched> {
    let dest = prepare(fixture, tmp, "gix", what)?;
    let repo = gix::open_opts(&dest, gix::open::Options::isolated())?;
    let remote = repo
        .remote_at(format!("file://{}", client.display()))?
        .with_fetch_tags(gix::remote::fetch::Tags::None)
        .with_refspecs(
            Some(format!("+refs/heads/{0}:refs/heads/{0}", what.branch).as_str()),
            Fetch,
        )?;
    let mut prepared = remote
        .connect(Fetch)?
        .prepare_fetch(gix::progress::Discard, Default::default())?;
    if let Some(depth) = what.depth {
        prepared = prepared.with_shallow(gix::remote::fetch::Shallow::DepthAtRemote(
            depth.try_into().expect("a positive depth"),
        ));
    }
    let (accepted, messages) = match prepared.receive(gix::progress::Discard, &AtomicBool::default()) {
        Ok(outcome) => (
            true,
            lines_about_objects(&String::from_utf8_lossy(&outcome.object_warnings)),
        ),
        Err(err) => {
            let refused = err.downcast_any_ref::<gix::remote::fetch::ObjectsRefused>();
            (
                false,
                lines_about_objects(&refused.map_or_else(String::new, |refused| refused.report.to_string())),
            )
        }
    };
    let (left, leftovers) = left(&dest, messages)?;
    Ok(Fetched {
        accepted,
        left,
        leftovers,
    })
}

/// Fetch with Git and with the native fetch, and require that they agree on what is left of it, and that the fetch was accepted or
/// refused as `accepted` says, which makes sure that the comparison is about something. Return what was said about objects.
fn same(label: &str, what: &Plan<'_>, accepted: bool) -> TestResult<Vec<String>> {
    let (fixture, client) = client()?;
    let tmp = gix_testtools::tempfile::tempdir()?;
    let git = with_git(&fixture, tmp.path(), &client, what)?;
    let mut gix = with_gix(&fixture, tmp.path(), &client, what)?;
    let mut git = git;
    if accepted {
        // Where the objects are kept isn't about checking: Git explodes small packs unless they are checked, and writes reverse indices.
        git.left.packs.clear();
        gix.left.packs.clear();
    }
    assert_eq!(
        git.accepted, accepted,
        "{label}: Git's verdict is the one expected\n{:?}",
        git.left
    );
    assert_eq!(
        gix.accepted, git.accepted,
        "{label}: the verdict is the same\n{:?}\n{:?}",
        git.left, gix.left
    );
    assert_eq!(
        gix.left, git.left,
        "{label}: what is left, and what is said, is the same"
    );
    assert!(
        gix.leftovers.is_empty(),
        "{label}: the native fetch leaves no temporary files: {:?}",
        gix.leftovers
    );
    if accepted {
        assert!(
            git.leftovers.is_empty(),
            "{label}: Git leaves none either: {:?}",
            git.leftovers
        );
    }
    Ok(git.left.messages)
}

#[test]
fn objects_are_checked_when_asked_to_and_not_unless() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for branch in REFUSED {
        same(&format!("{branch}-checked"), &fetch("remote.git", CHECK, branch), false)?;
        let unchecked = same(&format!("{branch}-unchecked"), &fetch("remote.git", &[], branch), true)?;
        assert!(
            unchecked.is_empty(),
            "{branch}: nothing is said if nothing is checked: {unchecked:?}"
        );
    }
    for (branch, warns) in ACCEPTED {
        let messages = same(&format!("{branch}-checked"), &fetch("remote.git", CHECK, branch), true)?;
        assert_eq!(
            messages.iter().any(|line| line.starts_with("warning: ")),
            *warns,
            "{branch}: warnings: {messages:?}"
        );
    }
    Ok(())
}

#[test]
fn the_settings_are_those_of_fetch_and_transfer_and_not_those_of_others() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    let cases: &[(&str, &str, &'static [&'static str], bool)] = &[
        (
            "transfer",
            "zero-padded-mode",
            &["config transfer.fsckObjects true"],
            false,
        ),
        (
            "fetch-beats-transfer",
            "zero-padded-mode",
            &["config transfer.fsckObjects true", "config fetch.fsckObjects false"],
            true,
        ),
        (
            "transfer-is-for-the-others-if-fetch-says-yes",
            "zero-padded-mode",
            &["config transfer.fsckObjects false", "config fetch.fsckObjects true"],
            false,
        ),
        (
            "receive-is-for-receive",
            "zero-padded-mode",
            &["config receive.fsckObjects true"],
            true,
        ),
        (
            "ignored",
            "zero-padded-mode",
            &[
                "config fetch.fsckObjects true",
                "config fetch.fsck.zeroPaddedFilemode ignore",
            ],
            true,
        ),
        (
            "warned",
            "zero-padded-mode",
            &[
                "config fetch.fsckObjects true",
                "config fetch.fsck.zeroPaddedFilemode warn",
            ],
            true,
        ),
        (
            "plain-fsck-is-for-git-fsck",
            "zero-padded-mode",
            &["config fetch.fsckObjects true", "config fsck.zeroPaddedFilemode ignore"],
            false,
        ),
        (
            "receive-severities-are-for-receive",
            "zero-padded-mode",
            &[
                "config fetch.fsckObjects true",
                "config receive.fsck.zeroPaddedFilemode ignore",
            ],
            false,
        ),
        (
            "unknown-id-is-skipped",
            "zero-padded-mode",
            &["config fetch.fsckObjects true", "config fetch.fsck.nonsense error"],
            false,
        ),
        (
            "case-insensitive-name",
            "zero-padded-mode",
            &[
                "config fetch.fsckObjects true",
                "config fetch.fsck.ZEROPADDEDFILEMODE warn",
            ],
            true,
        ),
        (
            "others-stay-errors",
            "unsorted",
            &[
                "config fetch.fsckObjects true",
                "config fetch.fsck.zeroPaddedFilemode ignore",
            ],
            false,
        ),
        (
            "promotion-stays-for-the-others",
            "full-pathname",
            &[
                "config fetch.fsckObjects true",
                "config fetch.fsck.zeroPaddedFilemode ignore",
            ],
            false,
        ),
    ];
    for (label, branch, setup, accepted) in cases {
        let messages = same(label, &fetch("remote.git", setup, branch), *accepted)?;
        if label.contains("warned") || label.contains("case-insensitive") {
            assert!(
                messages.iter().any(|line| line.starts_with("warning: ")),
                "{label}: {messages:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn objects_the_repository_has_are_not_what_is_checked() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    // `remote-legacy.git` has a flawed tree and `legacy-next` is a delta of it, so that the pack is thin on a base that is
    // received, which isn't checked, and the new tree doesn't have the flaw.
    same(
        "thin-on-a-flawed-base",
        &fetch("remote-legacy.git", CHECK, "legacy-next"),
        true,
    )?;
    // The same flawed tree again, in full, is received and is checked.
    same(
        "flawed-tree-again",
        &fetch("remote-legacy.git", CHECK, "legacy-resend"),
        false,
    )?;
    Ok(())
}

#[test]
fn shallow_fetches_are_checked_with_the_boundary_they_make() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for depth in [1, 2] {
        for branch in ["ok", "zero-padded-mode"] {
            let accepted = branch == "ok";
            let mut what = fetch("remote-empty.git", CHECK, branch);
            what.depth = Some(depth);
            same(&format!("{branch}-at-depth-{depth}"), &what, accepted)?;
        }
    }
    Ok(())
}

#[test]
fn promisor_remotes_are_checked_as_others_are() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    const PROMISOR: &[&str] = &[
        "config fetch.fsckObjects true",
        "config extensions.partialClone origin",
        "config remote.origin.promisor true",
    ];
    for (branch, accepted) in [
        ("ok", true),
        ("zero-padded-mode", false),
        ("unsorted", false),
        ("dotgit", false),
    ] {
        same(
            &format!("{branch}-from-promisor"),
            &fetch("remote.git", PROMISOR, branch),
            accepted,
        )?;
    }
    Ok(())
}

#[test]
fn clones_are_checked_and_leave_nothing_if_refused() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    let (_, client) = client()?;
    let url = format!("file://{}", client.display());
    for (branch, accepted) in [("ok", true), ("unsorted", false), ("zero-padded-mode", false)] {
        let tmp = gix_testtools::tempfile::tempdir()?;
        let git_dest = tmp.path().join("git");
        let output = gix_testtools::git_command(tmp.path())
            .args([
                "-c",
                "transfer.fsckObjects=true",
                "clone",
                "--bare",
                "--single-branch",
                "--no-tags",
                "--branch",
                branch,
            ])
            .arg(&url)
            .arg(&git_dest)
            .output()?;
        assert_eq!(
            output.status.success(),
            accepted,
            "{branch}: Git's verdict is the one expected"
        );

        let gix_dest = tmp.path().join("gix");
        let fetched = gix::prepare_clone_bare(url.as_str(), &gix_dest)?
            .with_ref_name(Some(branch))?
            .with_remote_name("origin")?
            .with_in_memory_config_overrides(["transfer.fsckObjects=true"])
            .configure_remote(move |remote| {
                remote.with_fetch_tags(gix::remote::fetch::Tags::None).with_refspecs(
                    Some(format!("+refs/heads/{branch}:refs/heads/{branch}").as_str()),
                    Fetch,
                )
            })
            .fetch_only(gix::progress::Discard, &AtomicBool::default());
        assert_eq!(
            fetched.is_ok(),
            accepted,
            "{branch}: the verdict of the native clone is the same: {:?}",
            fetched.as_ref().err()
        );
        if let Err(err) = &fetched {
            assert!(
                err.downcast_any_ref::<gix::remote::fetch::ObjectsRefused>().is_some(),
                "{branch}: the reason can be told apart: {err:?}"
            );
        }
        assert_eq!(
            gix_dest.exists(),
            git_dest.exists(),
            "{branch}: a refused clone leaves no directory, like Git's"
        );
        if accepted {
            let list = |dest: &Path| -> TestResult<String> {
                Ok(gix_testtools::git(dest, "cat-file --batch-all-objects --batch-check")?
                    .lines()
                    // Git follows tags of what it clones, which isn't what is compared here.
                    .filter(|line| !line.contains(" tag "))
                    .map(ToOwned::to_owned)
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join("\n"))
            };
            assert_eq!(list(&gix_dest)?, list(&git_dest)?, "{branch}: the same objects");
        }
    }
    Ok(())
}
