use std::{collections::BTreeSet, io::Write, path::Path, process::Stdio};

use gix::bstr::ByteSlice;
use gix_hash::Kind;
use gix_testtools::TestResult;

use super::{Fixture, Setup, command_request, fixtures, fork_serve, git_serve};

/// What the answer to `fetch` contains.
struct Fetched {
    /// The pack, if there was one.
    pack: Option<Vec<u8>>,
    /// The text of an error packet or of the error band, if there was one.
    error: Option<String>,
}

fn read_answer(response: &[u8]) -> Fetched {
    let mut at = 0;
    let mut pack: Option<Vec<u8>> = None;
    let mut error = None;
    let mut in_pack = false;
    while at + 4 <= response.len() {
        let len = usize::from_str_radix(std::str::from_utf8(&response[at..at + 4]).expect("hex"), 16).expect("length");
        if len < 4 {
            at += 4;
            continue;
        }
        let data = &response[at + 4..at + len];
        at += len;
        if let Some(text) = data.strip_prefix(b"ERR ") {
            error = Some(text.to_str_lossy().into_owned());
        } else if data == b"packfile\n" {
            in_pack = true;
            pack = Some(Vec::new());
        } else if in_pack {
            match data[0] {
                1 => pack.as_mut().expect("in a pack").extend_from_slice(&data[1..]),
                2 => {}
                _ => error = Some(data[1..].to_str_lossy().into_owned()),
            }
        }
    }
    Fetched { pack, error }
}

fn fetch_request(kind: Kind, wants: &[&str], haves: &[&str], args: &[&str]) -> Vec<u8> {
    let lines: Vec<String> = wants
        .iter()
        .map(|id| format!("want {id}"))
        .chain(haves.iter().map(|id| format!("have {id}")))
        .chain(args.iter().map(|arg| (*arg).to_owned()))
        .collect();
    command_request("fetch", kind, &lines.iter().map(String::as_str).collect::<Vec<_>>())
}

/// An empty repository to apply packs to, for the object format `kind`.
fn empty_repo(kind: Kind) -> TestResult<gix_testtools::tempfile::TempDir> {
    let dir = gix_testtools::tempfile::tempdir()?;
    gix_testtools::git(dir.path(), &format!("init -q --bare --object-format={kind}"))?;
    Ok(dir)
}

/// Add `pack` to the repository at `repo`, which fails if it isn't a valid pack.
fn apply(repo: &Path, pack: &[u8]) -> TestResult {
    let mut child = gix_testtools::git_command(repo)
        .args(["index-pack", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().expect("piped").write_all(pack)?;
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "the pack is valid: {}",
        output.stderr.to_str_lossy()
    );
    Ok(())
}

fn objects_in(repo: &Path) -> TestResult<BTreeSet<String>> {
    Ok(
        gix_testtools::git(repo, "cat-file --batch-all-objects --batch-check=%(objectname)")?
            .lines()
            .map(ToOwned::to_owned)
            .collect(),
    )
}

/// The objects a pack contains.
fn objects_of(kind: Kind, pack: &[u8]) -> TestResult<BTreeSet<String>> {
    let repo = empty_repo(kind)?;
    apply(repo.path(), pack)?;
    objects_in(repo.path())
}

/// What a client that has the objects of `have` needs to know what is new, as `git rev-list --objects` computes it.
fn needed(f: &Fixture, wants: &[&str], haves: &[&str]) -> TestResult<BTreeSet<String>> {
    let revs: Vec<String> = wants
        .iter()
        .map(|id| (*id).to_owned())
        .chain(haves.iter().map(|id| format!("^{id}")))
        .collect();
    Ok(
        gix_testtools::git(f.repo("remote.git"), &format!("rev-list --objects {}", revs.join(" ")))?
            .lines()
            .map(|line| line.split(' ').next().expect("an id").to_owned())
            .collect(),
    )
}

fn serve_both(f: &Fixture, name: &str, request: &[u8]) -> TestResult<(Fetched, Fetched, gix::Result<()>)> {
    let git = git_serve(&f.repo(name), &Setup::default(), request)?;
    let (fork, result) = fork_serve(&f.repo(name), &Setup::default(), request)?;
    Ok((read_answer(&git.response), read_answer(&fork.response), result))
}

#[test]
fn a_clone_gets_the_objects_git_sends_and_a_valid_repository() -> TestResult {
    for f in fixtures()? {
        let tips: Vec<String> = ["main", "side", "merged", "v1"]
            .iter()
            .map(|rev| f.rev("remote.git", rev))
            .collect::<TestResult<_>>()?;
        let wants: Vec<&str> = tips.iter().map(String::as_str).collect();
        let request = fetch_request(f.kind, &wants, &[], &["ofs-delta", "no-progress", "done"]);
        let (git, fork, result) = serve_both(&f, "remote.git", &request)?;
        result?;
        let (git_objects, fork_objects) = (
            objects_of(f.kind, &git.pack.expect("Git sends a pack"))?,
            objects_of(f.kind, &fork.pack.clone().expect("the fork sends a pack"))?,
        );
        assert_eq!(
            fork_objects,
            git_objects,
            "{}: without haves nothing can be excluded, so the objects are the same",
            f.label()
        );

        let client = empty_repo(f.kind)?;
        apply(client.path(), &fork.pack.expect("a pack"))?;
        for (name, id) in ["main", "side", "merged"].iter().zip(&tips) {
            gix_testtools::git(client.path(), &format!("update-ref refs/heads/{name} {id}"))?;
        }
        gix_testtools::git(client.path(), "fsck --connectivity-only --strict")?;
    }
    Ok(())
}

#[test]
fn an_incremental_fetch_sends_what_is_needed_and_the_edge_of_what_the_client_has() -> TestResult {
    for f in fixtures()? {
        let (one, two) = (f.rev("remote.git", "main~1")?, f.rev("remote.git", "main")?);
        let request = fetch_request(f.kind, &[&two], &[&one], &["ofs-delta", "no-progress", "done"]);
        let (git, fork, result) = serve_both(&f, "remote.git", &request)?;
        result?;
        let fork_pack = fork.pack.expect("the fork sends a pack");
        let (git_objects, fork_objects) = (
            objects_of(f.kind, &git.pack.expect("Git sends a pack"))?,
            objects_of(f.kind, &fork_pack)?,
        );
        let needed = needed(&f, &[&two], &[&one])?;
        assert_eq!(git_objects, needed, "{}: Git sends exactly what is new", f.label());
        assert!(
            fork_objects.is_superset(&needed),
            "{}: the client gets everything it needs",
            f.label()
        );
        let edge: BTreeSet<String> = [one.clone(), f.rev("remote.git", "main~1^{tree}")?].into();
        assert_eq!(
            fork_objects.difference(&needed).cloned().collect::<BTreeSet<_>>(),
            edge,
            "{}: on top of that, the commit where the walk stops and its tree are sent, nothing else, so haves do exclude",
            f.label()
        );

        // What the client has is what `old.git` has, which the new objects connect to.
        let client = std::fs::canonicalize(f.repo("old.git"))?;
        let copy = gix_testtools::tempfile::tempdir()?;
        gix_testtools::git(copy.path(), &format!("clone -q --bare {} .", client.display()))?;
        apply(copy.path(), &fork_pack)?;
        gix_testtools::git(copy.path(), &format!("update-ref refs/heads/main {two}"))?;
        gix_testtools::git(copy.path(), "fsck --connectivity-only --strict")?;
    }
    Ok(())
}

#[test]
fn merges_send_what_one_of_their_parents_changed_which_is_more_than_git_does() -> TestResult {
    for f in fixtures()? {
        let (merged, side) = (f.rev("remote.git", "merged")?, f.rev("remote.git", "side")?);
        let request = fetch_request(f.kind, &[&merged], &[&side], &["ofs-delta", "no-progress", "done"]);
        let (_, fork, result) = serve_both(&f, "remote.git", &request)?;
        result?;
        let fork_objects = objects_of(f.kind, &fork.pack.expect("a pack"))?;
        let needed = needed(&f, &[&merged], &[&side])?;
        assert!(
            fork_objects.is_superset(&needed),
            "{}: nothing needed is missing",
            f.label()
        );
        // This is over-sending that is intended, and known: the merge is compared with both of its parents, and what one of them
        // changed is sent even though the client has it. The commits where the walk stops and their trees are the other part.
        let intended: BTreeSet<String> = [
            f.rev("remote.git", "main~1")?,
            f.rev("remote.git", "main~1^{tree}")?,
            side.clone(),
            f.rev("remote.git", "side^{tree}")?,
            f.rev("remote.git", "side:side")?,
        ]
        .into();
        assert_eq!(
            fork_objects.difference(&needed).cloned().collect::<BTreeSet<_>>(),
            intended,
            "{}: the extras are the edge commits with their trees, and the blob that `side` added, which the client has",
            f.label()
        );
    }
    Ok(())
}

#[test]
fn objects_that_do_not_exist_are_refused_like_git_does() -> TestResult {
    for f in fixtures()? {
        let missing = "1".repeat(f.kind.len_in_hex());
        let request = fetch_request(f.kind, &[&missing], &[], &["done"]);
        let git = git_serve(&f.repo("remote.git"), &Setup::default(), &request)?;
        let (fork, result) = fork_serve(&f.repo("remote.git"), &Setup::default(), &request)?;
        assert_eq!(
            fork.response.as_bstr(),
            git.response.as_bstr(),
            "{}: the error packet is the one of Git",
            f.label()
        );
        assert!(
            git.response
                .contains_str(format!("ERR upload-pack: not our ref {missing}")),
            "{}: and it says what is wrong",
            f.label()
        );
        assert!(result.is_err(), "the session ends");
    }
    Ok(())
}

#[test]
fn a_request_without_wants_is_not_answered() -> TestResult {
    for f in fixtures()? {
        let request = fetch_request(f.kind, &[], &[], &["done"]);
        let (git, fork, result) = serve_both(&f, "remote.git", &request)?;
        result?;
        assert!(git.pack.is_none() && fork.pack.is_none() && git.error.is_none() && fork.error.is_none());
        let again = git_serve(&f.repo("remote.git"), &Setup::default(), &request)?;
        let (served, _) = fork_serve(&f.repo("remote.git"), &Setup::default(), &request)?;
        assert_eq!(
            served.response,
            again.response,
            "{}: nothing is sent, which is what Git does",
            f.label()
        );
        assert_eq!(again.response, b"", "{}: nothing at all", f.label());
    }
    Ok(())
}

#[test]
fn what_is_not_served_is_refused_with_an_error_packet() -> TestResult {
    for f in fixtures()? {
        let main = f.rev("remote.git", "main")?;
        let one = f.rev("remote.git", "main~1")?;
        let cases: [(&str, Vec<u8>, &str); 3] = [
            (
                "a shallow clone",
                fetch_request(f.kind, &[&main], &[], &["deepen 1", "done"]),
                "shallow clones",
            ),
            (
                "wait-for-done",
                fetch_request(f.kind, &[&main], &[], &["wait-for-done"]),
                "wait-for-done",
            ),
            (
                "a client that knows about shallow commits",
                fetch_request(f.kind, &[&main], &[], &[&format!("shallow {one}"), "done"]),
                "shallow clones",
            ),
        ];
        for (what, request, text) in cases {
            let (fork, result) = fork_serve(&f.repo("remote.git"), &Setup::default(), &request)?;
            let err = result.expect_err(what);
            assert!(err.classify().is_unsupported(), "{} {what}: {err:#}", f.label());
            let answer = read_answer(&fork.response);
            assert!(
                answer.error.is_some_and(|error| error.contains(text)) && answer.pack.is_none(),
                "{} {what}: the client is told why, and no pack is started",
                f.label()
            );
        }
    }
    Ok(())
}

#[test]
fn a_blob_that_cannot_be_read_ends_the_pack_with_an_error_message() -> TestResult {
    for f in fixtures()? {
        let main = f.rev("remote.git", "main")?;
        let request = fetch_request(f.kind, &[&main], &[], &["done"]);
        let (fork, result) = fork_serve(&f.repo("broken-blob.git"), &Setup::default(), &request)?;
        assert!(result.is_err(), "{}: the failure is not hidden", f.label());
        let answer = read_answer(&fork.response);
        assert!(
            answer.error.is_some(),
            "{}: the client gets a message, and not just the end of the stream",
            f.label()
        );
    }
    Ok(())
}

#[test]
fn the_requests_of_a_session_are_answered_one_after_the_other() -> TestResult {
    for f in fixtures()? {
        let main = f.rev("remote.git", "main")?;
        let one = fetch_request(f.kind, &[&main], &[], &["no-progress", "done"]);
        let request = [one.clone(), one].concat();
        let (fork, result) = fork_serve(&f.repo("remote.git"), &Setup::default(), &request)?;
        result?;
        let packets = fork.response.find_iter(b"packfile\n").count();
        assert_eq!(packets, 2, "{}: both requests of the session were answered", f.label());
    }
    Ok(())
}

#[test]
fn tags_that_point_into_the_pack_come_along_with_include_tag_and_only_then() -> TestResult {
    for f in fixtures()? {
        let (main, v1) = (f.rev("remote.git", "main")?, f.rev("remote.git", "v1")?);
        let without = fetch_request(f.kind, &[&main], &[], &["ofs-delta", "no-progress", "done"]);
        let with = fetch_request(
            f.kind,
            &[&main],
            &[],
            &["ofs-delta", "no-progress", "include-tag", "done"],
        );
        for (request, has_tag) in [(without, false), (with, true)] {
            let (git, fork, result) = serve_both(&f, "remote.git", &request)?;
            result?;
            let (git_objects, fork_objects) = (
                objects_of(f.kind, &git.pack.expect("Git sends a pack"))?,
                objects_of(f.kind, &fork.pack.expect("the fork sends a pack"))?,
            );
            assert_eq!(
                fork_objects,
                git_objects,
                "{}: include-tag={has_tag}: the same objects",
                f.label()
            );
            assert_eq!(
                fork_objects.contains(&v1),
                has_tag,
                "{}: the annotated tag on `main` is in the pack if the client asked for tags",
                f.label()
            );
        }
    }
    Ok(())
}

#[test]
fn include_tag_looks_at_the_tags_of_the_whole_repository_even_in_a_namespace_and_hidden_ones() -> TestResult {
    for f in fixtures()? {
        let main = f
            .rev("namespaced.git", "main")
            .or_else(|_| f.rev("remote.git", "main"))?;
        let (global, namespaced) = (
            f.rev("namespaced.git", "refs/tags/global-tag")?,
            f.rev("namespaced.git", "refs/namespaces/team/refs/tags/ns-tag")?,
        );
        let request = fetch_request(
            f.kind,
            &[&main],
            &[],
            &["ofs-delta", "no-progress", "include-tag", "done"],
        );
        for config in [&[][..], &["uploadpack.hideRefs=refs/tags/global-tag"]] {
            let setup = Setup {
                config,
                namespace: Some("team"),
            };
            let git = git_serve(&f.repo("namespaced.git"), &setup, &request)?;
            let (fork, result) = fork_serve(&f.repo("namespaced.git"), &setup, &request)?;
            result?;
            let (git_objects, fork_objects) = (
                objects_of(f.kind, &read_answer(&git.response).pack.expect("Git sends a pack"))?,
                objects_of(
                    f.kind,
                    &read_answer(&fork.response).pack.expect("the fork sends a pack"),
                )?,
            );
            assert_eq!(fork_objects, git_objects, "{} {config:?}: the same objects", f.label());
            assert!(
                fork_objects.contains(&global) && !fork_objects.contains(&namespaced),
                "{} {config:?}: the tag outside of the namespace, hidden or not, comes along, and the one inside doesn't",
                f.label()
            );
        }
    }
    Ok(())
}

/// Where a `fetch` answer that is ready to send the pack starts it.
fn before_the_pack(response: &[u8]) -> &[u8] {
    match response.find(b"000dpackfile\n") {
        Some(at) => &response[..at],
        None => response,
    }
}

#[test]
fn negotiation_is_acknowledged_like_git_and_ends_with_the_pack_when_there_is_enough_in_common() -> TestResult {
    for f in fixtures()? {
        let rev = |name: &str| f.rev("remote.git", name);
        let (one, two, side, merged, v1, orphan) = (
            rev("main~1")?,
            rev("main")?,
            rev("side")?,
            rev("merged")?,
            rev("v1")?,
            rev("orphan")?,
        );
        let tree_of_one = rev("main~1^{tree}")?;
        let unknown = "1".repeat(f.kind.len_in_hex());
        let cases: Vec<(&str, Vec<&str>, Vec<&str>, bool)> = vec![
            ("an ancestor of the only want", vec![&two], vec![&one], true),
            (
                "a sibling of the want, which has its parent in common, and that parent is older than the have, so the date cutoff doesn't apply to it",
                vec![&side],
                vec![&two],
                true,
            ),
            (
                "a root commit that has nothing in common with what the client has",
                vec![&orphan],
                vec![&two],
                false,
            ),
            ("an object nobody has", vec![&two], vec![&unknown], false),
            (
                "an object nobody has and one that is common",
                vec![&two],
                vec![&unknown, &one],
                true,
            ),
            (
                "a tree, which can't lead anywhere",
                vec![&two],
                vec![&tree_of_one],
                false,
            ),
            ("a tag that peels to the want", vec![&v1], vec![&one], true),
            (
                "a merge on top of what the client has",
                vec![&merged],
                vec![&side],
                true,
            ),
            (
                "two wants that have something in common",
                vec![&two, &side],
                vec![&one],
                true,
            ),
            (
                "two wants that reach the parent of what the client has",
                vec![&two, &side],
                vec![&side],
                true,
            ),
            (
                "haves where the second is the parent of the first, which Git doesn't acknowledge again",
                vec![&side],
                vec![&two, &one],
                true,
            ),
            (
                "haves where the parent comes first, which Git acknowledges, and then the child",
                vec![&side],
                vec![&one, &two],
                true,
            ),
            ("a have that is repeated", vec![&two], vec![&one, &one], true),
            (
                "two wants of which one has nothing in common",
                vec![&two, &orphan],
                vec![&one],
                false,
            ),
        ];
        for (what, wants, haves, ready) in cases {
            let request = fetch_request(f.kind, &wants, &haves, &["ofs-delta", "no-progress"]);
            let git = git_serve(&f.repo("remote.git"), &Setup::default(), &request)?;
            let (fork, result) = fork_serve(&f.repo("remote.git"), &Setup::default(), &request)?;
            result?;
            assert_eq!(
                before_the_pack(&fork.response).as_bstr(),
                before_the_pack(&git.response).as_bstr(),
                "{} {what}: the acknowledgments are those of Git",
                f.label()
            );
            let (git_pack, fork_pack) = (read_answer(&git.response).pack, read_answer(&fork.response).pack);
            assert_eq!(
                git_pack.is_some(),
                ready,
                "{} {what}: Git is ready to send the pack, or isn't",
                f.label()
            );
            assert_eq!(fork_pack.is_some(), ready, "{} {what}: and so is the fork", f.label());
            if let Some(pack) = fork_pack {
                let existing: Vec<&str> = haves.iter().copied().filter(|have| *have != unknown).collect();
                let needed = needed(&f, &wants, &existing)?;
                assert!(
                    objects_of(f.kind, &pack)?.is_superset(&needed),
                    "{} {what}: the pack has what the client needs",
                    f.label()
                );
            } else {
                assert_eq!(
                    fork.response,
                    git.response,
                    "{} {what}: the whole answer is the same",
                    f.label()
                );
            }
        }
    }
    Ok(())
}
