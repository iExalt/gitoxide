//! Objects written here have to be the objects Git writes for the same input, so that ids agree between the two and
//! Git reads what was written. Git is the judge in each of these.
use gix::bstr::ByteSlice;
use gix::prelude::ObjectIdExt;
use gix_testtools::TestResult;

/// Run `git` with `args` in the repository of `repo`, feeding it `stdin`, and return its trimmed output.
fn git_with(repo: &gix::Repository, args: &[&str], stdin: &[u8], envs: &[(&str, &str)]) -> TestResult<String> {
    use std::io::Write;
    let mut cmd = gix_testtools::git_command(repo.workdir().unwrap_or(repo.git_dir()));
    cmd.args(args)
        .envs(envs.iter().copied())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    child.stdin.take().expect("piped").write_all(stdin)?;
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        output.stderr.as_bstr()
    );
    Ok(output.stdout.as_bstr().trim_end().to_str()?.to_owned())
}

#[test]
fn blobs_have_the_id_git_gives_them_and_git_reads_them() -> TestResult {
    let (repo, _keep) = crate::basic_rw_repo()?;
    let big = vec![b'x'; 300_000];
    for content in [
        &b""[..],
        b"hello\n",
        b"no newline at the end",
        b"with\0nul\0bytes\xff\xfe",
        b"windows\r\nline endings\r\n",
        &big,
    ] {
        let id = repo.write_blob(content)?;
        assert_eq!(
            id.to_string(),
            git_with(&repo, &["hash-object", "--stdin"], content, &[])?,
            "the id of {} bytes",
            content.len()
        );
        assert_eq!(
            git_with(&repo, &["cat-file", "-s", &id.to_string()], b"", &[])?,
            content.len().to_string(),
            "Git finds the object that was written and agrees on its size"
        );
    }
    Ok(())
}

#[test]
fn commits_have_the_id_git_gives_them() -> TestResult {
    let (repo, _keep) = crate::basic_rw_repo()?;
    let tree_id = repo.head_tree_id()?.detach();
    let head_id = repo.head_id()?.detach();
    let other_parent = repo.commit_as(
        signature("Other", "other@example.com", "1600000000 +0000"),
        signature("Other", "other@example.com", "1600000000 +0000"),
        "refs/heads/other-parent",
        "other parent\n",
        tree_id,
        [head_id],
    )?;
    let other_parent_id = other_parent.detach();

    let cases = [
        (
            "simple",
            "subject\n",
            0,
            ("A U Thor", "a@example.com", "1700000000 +0000"),
        ),
        (
            "no newline",
            "subject",
            1,
            ("A U Thor", "a@example.com", "1700000000 +0000"),
        ),
        (
            "empty message",
            "",
            1,
            ("A U Thor", "a@example.com", "1700000000 +0000"),
        ),
        (
            "body",
            "subject\n\nbody line\n\nsecond paragraph\n",
            1,
            ("A U Thor", "a@example.com", "1700000000 +0000"),
        ),
        (
            "trailing blank lines",
            "subject\n\n\n",
            2,
            ("A U Thor", "a@example.com", "1700000000 +0000"),
        ),
        (
            "two parents",
            "merge\n",
            2,
            ("A U Thor", "a@example.com", "1700000000 +0000"),
        ),
        (
            "non-ascii",
            "ünï côdé ✓\n",
            1,
            ("Ünï Çödé", "u@example.com", "1700000000 +0000"),
        ),
        (
            "a positive offset",
            "tz\n",
            1,
            ("A U Thor", "a@example.com", "1700000000 +0530"),
        ),
        (
            "a negative offset",
            "tz\n",
            1,
            ("A U Thor", "a@example.com", "1700000000 -0845"),
        ),
        ("the epoch", "epoch\n", 0, ("A U Thor", "a@example.com", "0 +0000")),
    ];
    for (index, (name, message, parent_count, (who, email, time))) in cases.into_iter().enumerate() {
        let parents: Vec<_> = [head_id, other_parent_id].into_iter().take(parent_count).collect();
        let reference = format!("refs/heads/case-{index}");
        if let Some(first) = parents.first() {
            git_with(&repo, &["update-ref", &reference, &first.to_string()], b"", &[])?;
        }
        let author = signature(who, email, time);
        let committer = signature("C O Mitter", "c@example.com", "1700000100 +0100");
        let written = repo
            .commit_as(committer, author, reference.as_str(), message, tree_id, parents.clone())?
            .detach();

        let mut args = vec!["commit-tree".to_owned(), tree_id.to_string()];
        for parent in &parents {
            args.extend(["-p".to_owned(), parent.to_string()]);
        }
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        let (git_time, git_offset) = time.split_once(' ').expect("time and offset");
        let git_date = format!("{git_time} {git_offset}");
        let git_id = git_with(
            &repo,
            &args,
            message.as_bytes(),
            &[
                ("GIT_AUTHOR_NAME", who),
                ("GIT_AUTHOR_EMAIL", email),
                ("GIT_AUTHOR_DATE", &format!("@{git_date}")),
                ("GIT_COMMITTER_NAME", "C O Mitter"),
                ("GIT_COMMITTER_EMAIL", "c@example.com"),
                ("GIT_COMMITTER_DATE", "@1700000100 +0100"),
            ],
        )?;
        assert_eq!(
            written.to_string(),
            git_id,
            "the commit of case `{name}` has the id Git gives it"
        );
    }
    git_with(&repo, &["fsck", "--no-dangling", "--strict"], b"", &[])?;
    Ok(())
}

fn signature<'a>(name: &'a str, email: &'a str, time: &'a str) -> gix::actor::SignatureRef<'a> {
    gix::actor::SignatureRef {
        name: name.into(),
        email: email.into(),
        time,
    }
}

#[test]
fn trees_have_the_id_git_gives_them() -> TestResult {
    use gix::objs::tree::EntryKind;
    use std::fmt::Write;
    let (repo, _keep) = crate::basic_rw_repo()?;
    let file_id = repo.write_blob("file\n")?.detach();
    let other_id = repo.write_blob("other\n")?.detach();
    // Names that sort differently with and without the slash that ends a directory's name for sorting.
    let entries = [
        ("a", EntryKind::Tree, file_id),
        ("a.txt", EntryKind::Blob, file_id),
        ("a-b", EntryKind::Blob, other_id),
        ("a0", EntryKind::BlobExecutable, other_id),
        ("b", EntryKind::Link, file_id),
        ("Z", EntryKind::Blob, file_id),
        ("ünï", EntryKind::Blob, file_id),
        ("dir/nested/file", EntryKind::Blob, file_id),
        ("dir/nested.txt", EntryKind::Blob, other_id),
    ];
    let mut editor = repo.edit_tree(repo.empty_tree().id)?;
    let mut mktree_input = String::new();
    let sub_tree_id = {
        // `a` is a directory, which needs a tree of its own that Git can read.
        let mut sub = repo.edit_tree(repo.empty_tree().id)?;
        sub.upsert("inner", EntryKind::Blob, file_id)?;
        sub.write()?.detach()
    };
    for (path, kind, id) in entries {
        let id = if path == "a" { sub_tree_id } else { id };
        editor.upsert(path, kind, id)?;
        if !path.contains('/') {
            let (mode, kind_name) = match kind {
                EntryKind::Tree => ("040000", "tree"),
                EntryKind::Blob => ("100644", "blob"),
                EntryKind::BlobExecutable => ("100755", "blob"),
                EntryKind::Link => ("120000", "blob"),
                EntryKind::Commit => ("160000", "commit"),
            };
            writeln!(mktree_input, "{mode} {kind_name} {id}\t{path}")?;
        }
    }
    // The nested entries are written by Git through the tree of `dir`.
    let nested_id = {
        let mut nested = repo.edit_tree(repo.empty_tree().id)?;
        nested.upsert("file", EntryKind::Blob, file_id)?;
        nested.write()?.detach()
    };
    let dir_id = {
        let mut dir = repo.edit_tree(repo.empty_tree().id)?;
        dir.upsert("nested", EntryKind::Tree, nested_id)?;
        dir.upsert("nested.txt", EntryKind::Blob, other_id)?;
        dir.write()?.detach()
    };
    writeln!(mktree_input, "040000 tree {dir_id}\tdir")?;

    assert_eq!(
        repo.empty_tree().id.to_string(),
        git_with(&repo, &["mktree"], b"", &[])?,
        "the empty tree is the one Git makes"
    );
    let written = editor.write()?.detach();
    assert_eq!(
        written.to_string(),
        git_with(&repo, &["mktree"], mktree_input.as_bytes(), &[])?,
        "the tree has the id that `git mktree` gives the same entries, in the order Git sorts them"
    );
    git_with(&repo, &["fsck", "--no-dangling", "--strict"], b"", &[])?;
    Ok(())
}

#[test]
fn annotated_tags_have_the_id_git_gives_them() -> TestResult {
    let (repo, _keep) = crate::basic_rw_repo()?;
    let head_id = repo.head_id()?.detach();
    let tagger = signature("T Agger", "t@example.com", "1700000200 +0200");
    for (index, message) in ["release\n", "release\n\nwith a body\n", "no newline"]
        .into_iter()
        .enumerate()
    {
        let name = format!("v{index}");
        let reference = repo.tag(
            &name,
            head_id,
            gix::objs::Kind::Commit,
            Some(tagger),
            message,
            gix::refs::transaction::PreviousValue::MustNotExist,
        )?;
        let written = reference.id().detach();
        let tag_id = repo.find_reference(name.as_str())?.target().id().to_owned();
        assert_eq!(tag_id, written, "the reference points to the tag object");

        let git_tag = git_with(
            &repo,
            &["mktag"],
            format!(
                "object {head_id}\ntype commit\ntag {name}\ntagger T Agger <t@example.com> 1700000200 +0200\n\n{message}"
            )
            .as_bytes(),
            &[],
        )?;
        assert_eq!(
            written.to_string(),
            git_tag,
            "the tag has the id that `git mktag` gives the same fields, for `{message:?}`"
        );
    }
    let tree_id = repo.head_tree_id()?.detach();
    let tag_of_tree = repo.tag(
        "tree-tag",
        tree_id,
        gix::objs::Kind::Tree,
        Some(tagger),
        "a tree\n",
        gix::refs::transaction::PreviousValue::MustNotExist,
    )?;
    assert_eq!(
        tag_of_tree.id().to_string(),
        git_with(
            &repo,
            &["mktag"],
            format!(
                "object {tree_id}\ntype tree\ntag tree-tag\ntagger T Agger <t@example.com> 1700000200 +0200\n\na tree\n"
            )
            .as_bytes(),
            &[]
        )?,
        "a tag of a tree"
    );
    git_with(&repo, &["fsck", "--no-dangling", "--strict"], b"", &[])?;
    Ok(())
}

#[test]
fn short_ids_have_the_length_git_gives_them() -> TestResult {
    let (mut repo, _keep) = crate::basic_rw_repo()?;
    let id = repo.head_id()?.detach();
    for setting in ["auto", "4", "7", "12", "40", "41", "100", "no", "false"] {
        repo.config_snapshot_mut().set_raw_value("core.abbrev", setting)?;
        let git_args = ["-c", &format!("core.abbrev={setting}"), "rev-parse", "--short", "HEAD"];
        assert_eq!(
            id.attach(&repo).shorten()?.to_string(),
            git_with(&repo, &git_args, b"", &[])?,
            "core.abbrev={setting}"
        );
    }
    Ok(())
}

#[test]
fn a_recursive_listing_of_a_tree_is_what_git_lists() -> TestResult {
    let (repo, _keep) = crate::basic_rw_repo()?;
    let file_id = repo.write_blob("file\n")?;
    let other_id = repo.write_blob("other\n")?;
    let gitlink_id = repo.head_id()?;
    // Entries of every kind, at several depths, where names sort differently depending on whether they are directories.
    let nested = git_with(
        &repo,
        &["mktree"],
        format!("100644 blob {other_id}\tz\n100755 blob {file_id}\ta.sh\n120000 blob {file_id}\tlink\n").as_bytes(),
        &[],
    )?;
    let root = git_with(
        &repo,
        &["mktree"],
        format!(
            "100644 blob {file_id}\ta.txt\n040000 tree {nested}\ta\n160000 commit {gitlink_id}\tsub\n040000 tree {nested}\ta0\n100644 blob {other_id}\tb\n"
        )
        .as_bytes(),
        &[],
    )?;
    let tree = repo
        .find_object(gix::hash::ObjectId::from_hex(root.as_bytes())?)?
        .into_tree();

    let mut actual: Vec<_> = tree
        .traverse()
        .breadthfirst
        .files()?
        .into_iter()
        .filter(|entry| !entry.mode.is_tree())
        .map(|entry| format!("{:06o} {}\t{}", entry.mode.value(), entry.oid, entry.filepath))
        .collect();
    actual.sort_by(|a, b| a.split('\t').nth(1).cmp(&b.split('\t').nth(1)));
    let expected: Vec<_> = git_with(&repo, &["ls-tree", "-r", &root], b"", &[])?
        .lines()
        .map(|line| {
            let (meta, path) = line.split_once('\t').expect("tab");
            let mut parts = meta.split(' ');
            let (mode, _kind, id) = (parts.next().expect("mode"), parts.next(), parts.next().expect("id"));
            format!("{mode} {id}\t{path}")
        })
        .collect();
    assert_eq!(
        actual, expected,
        "files, links and gitlinks of every depth, with their modes and ids"
    );
    Ok(())
}
