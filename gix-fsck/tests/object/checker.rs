//! Tests of the behavior of the checker which Git's output alone doesn't show.
use std::collections::HashMap;

use gix_error::TestResult;
use gix_fsck::{Checker, Diagnostic, LinkError, MessageId, Options, Refusal, Severity};
use gix_hash::ObjectId;
use gix_object::{Data, Find, FindHeader, Header, Kind};

use super::corpus::{IDENT, tag, tree};
use super::oracle::Base;

/// A database that has what it was given.
#[derive(Default)]
struct Db(HashMap<ObjectId, (Kind, Vec<u8>)>);

impl Db {
    fn add(&mut self, kind: Kind, data: &[u8]) -> ObjectId {
        let id = gix_object::compute_hash(gix_testtools::object_hash(), kind, data).expect("hashable");
        self.0.insert(id, (kind, data.to_vec()));
        id
    }
}

impl Find for Db {
    fn try_find<'a>(&self, id: &gix_hash::oid, buffer: &'a mut Vec<u8>) -> gix_error::Result<Option<Data<'a>>> {
        Ok(self.0.get(id).map(|(kind, data)| {
            buffer.clear();
            buffer.extend_from_slice(data);
            Data {
                kind: *kind,
                object_hash: gix_testtools::object_hash(),
                data: buffer,
            }
        }))
    }
}

impl FindHeader for Db {
    fn try_header(&self, id: &gix_hash::oid) -> gix_error::Result<Option<Header>> {
        Ok(self.0.get(id).map(|(kind, data)| Header {
            kind: *kind,
            size: data.len() as u64,
        }))
    }
}

fn hash() -> gix_hash::Kind {
    gix_testtools::object_hash()
}

fn id_of(kind: Kind, data: &[u8]) -> ObjectId {
    gix_object::compute_hash(hash(), kind, data).expect("hashable")
}

fn check(checker: &mut Checker, kind: Kind, data: &[u8]) -> Result<(bool, Vec<Diagnostic>), Refusal> {
    let mut out = Vec::new();
    let failed = checker.check(&id_of(kind, data), kind, data.len() as u64, Some(data), &mut out)?;
    Ok((failed, out))
}

fn ids(out: &[Diagnostic]) -> Vec<(MessageId, Severity)> {
    out.iter()
        .filter_map(|d| match d {
            Diagnostic::Report(r) => Some((r.id, r.severity)),
            Diagnostic::Note(_) => None,
        })
        .collect()
}

#[test]
fn objects_that_cannot_be_parsed_are_refused_whatever_is_configured() -> TestResult {
    let base = Base::new()?;
    let missing_tree = format!("author {IDENT}\ncommitter {IDENT}\n\nm\n").into_bytes();
    let bad_object = format!(
        "object {}\ntype commit\ntag v1\n",
        "x".repeat(base.commit.to_string().len())
    )
    .into_bytes();
    let unknown_type = format!("object {}\ntype bogus\ntag v1\n\n", base.commit).into_bytes();
    for (kind, data) in [
        (Kind::Commit, missing_tree),
        (Kind::Tag, bad_object),
        (Kind::Tag, unknown_type),
    ] {
        let mut options = Options::strict();
        for id in [
            MessageId::MissingTree,
            MessageId::BadTreeSha1,
            MessageId::BadParentSha1,
            MessageId::MissingObject,
            MessageId::BadObjectSha1,
            MessageId::MissingTypeEntry,
            MessageId::BadType,
            MessageId::MissingTagEntry,
        ] {
            options.set(id, Severity::Ignore);
        }
        options.skip(id_of(kind, &data));
        let mut checker = Checker::new(options, hash());
        let err = check(&mut checker, kind, &data).expect_err("not even parsable, whatever the configuration");
        assert!(
            matches!(err, Refusal::Unparsable { kind: k, .. } if k == kind),
            "{err:?}"
        );
        assert_eq!(err.to_string(), format!("invalid {kind}"));
        // In lenient mode nothing is parsed ahead of the check, which is left to report what it finds.
        let mut lenient = Checker::new(Options::lenient(), hash());
        assert!(check(&mut lenient, kind, &data)?.0, "the check itself fails these");
    }
    Ok(())
}

#[test]
fn the_detail_of_unparsable_commits_and_tags_is_what_git_says() -> TestResult {
    let base = Base::new()?;
    let hex = base.commit.to_string().len();
    let detail = |kind, data: String| match check(&mut Checker::new(Options::strict(), hash()), kind, data.as_bytes()) {
        Err(Refusal::Unparsable { detail, oid, .. }) => (detail, oid),
        other => panic!("{other:?}"),
    };
    let tree = base.tree;
    let (d, oid) = detail(Kind::Commit, format!("tree {tree}\n"));
    assert_eq!(d.as_deref(), Some(format!("bogus commit object {oid}").as_str()));
    let (d, oid) = detail(Kind::Commit, format!("tree {}\nauthor x\n", "g".repeat(hex)));
    assert_eq!(d.as_deref(), Some(format!("bad tree pointer in commit {oid}").as_str()));
    let (d, oid) = detail(
        Kind::Commit,
        format!("tree {tree}\nparent {}\nauthor x\n", "g".repeat(hex)),
    );
    assert_eq!(d.as_deref(), Some(format!("bad parents in commit {oid}").as_str()));
    let (d, oid) = detail(Kind::Tag, format!("object {}\ntype bogus\ntag t\n\n", base.commit));
    assert_eq!(
        d.as_deref(),
        Some(format!("unknown tag type 'bogus' in {oid}").as_str())
    );
    let (d, _) = detail(Kind::Tag, format!("object {}\ntype commit\n", base.commit));
    assert_eq!(d, None, "most structural problems of tags are silent");
    Ok(())
}

#[test]
fn shallow_commits_have_no_parents_to_link_but_their_parent_lines_must_still_be_valid() -> TestResult {
    let base = Base::new()?;
    // The parent isn't known, which is fine for a commit at the boundary.
    let mut db = Db::default();
    db.add(Kind::Tree, &tree(&[]));
    let tree_id = id_of(Kind::Tree, &tree(&[]));
    let data = format!(
        "tree {tree_id}\nparent {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n",
        base.parent
    )
    .into_bytes();
    let boundary_id = id_of(Kind::Commit, &data);
    let mut checker_boundary = Checker::new(Options::strict(), hash()).with_shallow([boundary_id]);
    let (failed, _) = check(&mut checker_boundary, Kind::Commit, &data)?;
    assert!(!failed);
    assert!(
        !checker_boundary
            .finish(&db, &mut Vec::new())
            .expect("parents aren't linked")
    );

    // Without it, the parent is missing.
    let mut regular = Checker::new(Options::strict(), hash());
    check(&mut regular, Kind::Commit, &data)?;
    assert!(matches!(
        regular.finish(&db, &mut Vec::new()),
        Err(LinkError::Missing { id, expected: Kind::Commit }) if id == base.parent
    ));

    // The parent line must be valid even for boundary commits.
    let malformed = format!(
        "tree {tree_id}\nparent {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n",
        "z".repeat(base.parent.to_string().len())
    )
    .into_bytes();
    let malformed_id = id_of(Kind::Commit, &malformed);
    let mut shallow = Checker::new(Options::strict(), hash()).with_shallow([malformed_id]);
    assert!(matches!(
        check(&mut shallow, Kind::Commit, &malformed),
        Err(Refusal::Unparsable { kind: Kind::Commit, .. })
    ));
    Ok(())
}

#[test]
fn links_are_verified_in_whatever_order_objects_arrive() -> TestResult {
    let base = Base::new()?;
    let blob_data = b"content";
    let blob_id = id_of(Kind::Blob, blob_data);
    let tree_data = tree(&[("100644", b"f", &blob_id)]);
    let tree_id = id_of(Kind::Tree, &tree_data);
    let commit_data = format!("tree {tree_id}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n").into_bytes();
    let commit_id = id_of(Kind::Commit, &commit_data);
    let all = [
        (commit_id, Kind::Commit, commit_data),
        (tree_id, Kind::Tree, tree_data),
        (blob_id, Kind::Blob, blob_data.to_vec()),
    ];
    let orders: [[usize; 3]; 6] = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
    for order in orders {
        let mut checker = Checker::new(Options::strict(), hash());
        for idx in order {
            let (id, kind, data) = &all[idx];
            let mut out = Vec::new();
            let failed = checker.check(id, *kind, data.len() as u64, Some(data), &mut out)?;
            assert!(!failed && out.is_empty(), "order {order:?}: {out:?}");
        }
        assert!(
            !checker
                .finish(&Db::default(), &mut Vec::new())
                .expect("everything arrived"),
            "order {order:?}"
        );
    }

    // The same object as a different kind than it is linked as, in both orders.
    let wrong_blob = tree(&[("100644", b"f", &tree_id)]);
    let wrong_blob_id = id_of(Kind::Tree, &wrong_blob);
    for tree_first in [true, false] {
        let mut checker = Checker::new(Options::strict(), hash());
        let arrivals = if tree_first {
            [
                (wrong_blob_id, Kind::Tree, wrong_blob.clone()),
                (tree_id, Kind::Tree, all[1].2.clone()),
            ]
        } else {
            [
                (tree_id, Kind::Tree, all[1].2.clone()),
                (wrong_blob_id, Kind::Tree, wrong_blob.clone()),
            ]
        };
        let mut result = Ok(false);
        for (id, kind, data) in arrivals {
            result = checker.check(&id, kind, data.len() as u64, Some(&data), &mut Vec::new());
            if result.is_err() {
                break;
            }
        }
        let err = result.expect_err("a tree isn't a blob");
        assert!(
            matches!(&err, Refusal::Link(LinkError::WrongType { id, expected: Kind::Blob, found: Kind::Tree, received: true }) if *id == tree_id),
            "{tree_first}: {err:?}"
        );
        assert_eq!(err.to_string(), format!("object type mismatch at {tree_id}"));
    }

    // What isn't received is looked up.
    let mut checker = Checker::new(Options::strict(), hash());
    let (id, kind, data) = &all[1];
    checker.check(id, *kind, data.len() as u64, Some(data), &mut Vec::new())?;
    let mut db = Db::default();
    assert!(
        matches!(checker.finish(&db, &mut Vec::new()), Err(LinkError::Missing { id, expected: Kind::Blob }) if id == blob_id)
    );
    let mut checker = Checker::new(Options::strict(), hash());
    checker.check(id, *kind, data.len() as u64, Some(data), &mut Vec::new())?;
    db.add(Kind::Commit, b"not a blob");
    db.0.insert(blob_id, (Kind::Commit, Vec::new()));
    let err = checker.finish(&db, &mut Vec::new()).expect_err("wrong kind");
    assert_eq!(
        err.to_string(),
        format!("object {blob_id}: expected type blob, found commit")
    );
    let mut checker = Checker::new(Options::strict(), hash());
    checker.check(id, *kind, data.len() as u64, Some(data), &mut Vec::new())?;
    db.add(Kind::Blob, blob_data);
    assert!(!checker.finish(&db, &mut Vec::new()).expect("exists now"));

    // Submodules aren't followed.
    let mut checker = Checker::new(Options::strict(), hash());
    let gitlink = tree(&[("160000", b"sub", &base.commit)]);
    check(&mut checker, Kind::Tree, &gitlink)?;
    assert!(
        !checker
            .finish(&Db::default(), &mut Vec::new())
            .expect("gitlinks aren't links")
    );
    Ok(())
}

#[test]
fn a_tree_that_cannot_be_decoded_but_isnt_failed_cant_be_walked() -> TestResult {
    let mut options = Options::strict();
    options.set(MessageId::BadTree, Severity::Ignore);
    let mut checker = Checker::new(options, hash());
    let err = check(&mut checker, Kind::Tree, b"100644").expect_err("no first entry");
    assert!(matches!(err, Refusal::BrokenLinks { .. }));
    assert!(err.to_string().starts_with("Not all child objects of "));
    Ok(())
}

#[test]
fn a_bad_tagger_with_an_extra_header_is_reported_but_not_failed() -> TestResult {
    // Git 2.56 overwrites the result of checking the tagger with that of the extra header, which is ignored by default.
    let base = Base::new()?;
    let mut data = tag(&base, "A U Thor");
    let pos = data.windows(2).position(|w| w == b"\n\n").expect("header end");
    data.splice(pos + 1..pos + 1, b"foo bar\n".iter().copied());
    let (failed, out) = check(&mut Checker::new(Options::strict(), hash()), Kind::Tag, &data)?;
    assert_eq!(
        ids(&out),
        [(MessageId::MissingEmail, Severity::Error)],
        "the error is reported"
    );
    assert!(!failed, "but the tag isn't failed");

    let (failed, out) = check(
        &mut Checker::new(Options::strict(), hash()),
        Kind::Tag,
        &tag(&base, "A U Thor"),
    )?;
    assert_eq!(ids(&out), [(MessageId::MissingEmail, Severity::Error)]);
    assert!(failed, "without the extra header, it is");
    Ok(())
}

#[test]
fn trees_report_everything_while_commits_stop_at_the_first_problem() -> TestResult {
    let base = Base::new()?;
    let null = ObjectId::null(hash());
    let data = tree(&[("0100644", b"a/b", &null), ("100644", b".git", &base.blob)]);
    let (failed, out) = check(&mut Checker::new(Options::strict(), hash()), Kind::Tree, &data)?;
    assert!(failed);
    assert_eq!(
        ids(&out),
        [
            (MessageId::NullSha1, Severity::Error),
            (MessageId::FullPathname, Severity::Error),
            (MessageId::HasDotgit, Severity::Error),
            (MessageId::ZeroPaddedFilemode, Severity::Error),
            (MessageId::TreeNotSorted, Severity::Error),
        ],
        "all of them, in the order Git reports them"
    );

    let data = format!("tree {}\nauthor <a@b> x +0\ncommitter <a@b> x +0\n\n", base.tree);
    let (failed, out) = check(
        &mut Checker::new(Options::strict(), hash()),
        Kind::Commit,
        data.as_bytes(),
    )?;
    assert!(failed);
    assert_eq!(ids(&out), [(MessageId::MissingNameBeforeEmail, Severity::Error)]);
    Ok(())
}

#[test]
fn warnings_are_errors_in_strict_mode_only_and_info_never_fails() -> TestResult {
    let base = Base::new()?;
    let data = tree(&[("0100644", b"a", &base.blob)]);
    let (failed, out) = check(&mut Checker::new(Options::lenient(), hash()), Kind::Tree, &data)?;
    assert!(!failed);
    assert_eq!(ids(&out), [(MessageId::ZeroPaddedFilemode, Severity::Warn)]);
    let (failed, out) = check(&mut Checker::new(Options::strict(), hash()), Kind::Tree, &data)?;
    assert!(failed);
    assert_eq!(ids(&out), [(MessageId::ZeroPaddedFilemode, Severity::Error)]);

    let data = tree(&[("20000", b"a", &base.blob)]);
    let (failed, out) = check(&mut Checker::new(Options::strict(), hash()), Kind::Tree, &data)?;
    assert!(!failed);
    assert_eq!(
        ids(&out),
        [(MessageId::BadFilemode, Severity::Warn)],
        "info is reported as warning, and accepted"
    );
    Ok(())
}

#[test]
fn blobs_are_only_looked_at_if_a_tree_says_they_are_special() -> TestResult {
    let base = Base::new()?;
    let attributes = format!("{}\n", "a".repeat(2048)).into_bytes();
    let attributes_id = id_of(Kind::Blob, &attributes);
    let mut checker = Checker::new(Options::strict(), hash());
    assert!(!checker.needs_blob_content(&attributes_id, attributes.len() as u64));
    check(
        &mut checker,
        Kind::Tree,
        &tree(&[("100644", b".gitattributes", &attributes_id)]),
    )?;
    assert!(checker.needs_blob_content(&attributes_id, attributes.len() as u64));
    assert!(
        !checker.needs_blob_content(&attributes_id, 101 * 1024 * 1024),
        "too-large attributes are reported without reading them"
    );
    let (failed, out) = check(&mut checker, Kind::Blob, &attributes)?;
    assert!(failed);
    assert_eq!(ids(&out), [(MessageId::GitattributesLineLength, Severity::Error)]);
    assert!(
        !checker.needs_blob_content(&attributes_id, attributes.len() as u64),
        "done"
    );

    // A blob that is checked later is read from the database.
    let mut checker = Checker::new(Options::strict(), hash());
    let mut db = Db::default();
    db.add(Kind::Blob, &attributes);
    checker.check(
        &attributes_id,
        Kind::Blob,
        attributes.len() as u64,
        None,
        &mut Vec::new(),
    )?;
    check(
        &mut checker,
        Kind::Tree,
        &tree(&[("100644", b".gitattributes", &attributes_id)]),
    )?;
    let mut out = Vec::new();
    assert!(checker.finish(&db, &mut out)?);
    assert_eq!(ids(&out), [(MessageId::GitattributesLineLength, Severity::Error)]);

    // Or reported as missing, or as not a blob. In strict mode, the links catch these first.
    for (stored, expected) in [
        (None, MessageId::GitattributesMissing),
        (Some(Kind::Tree), MessageId::GitattributesBlob),
    ] {
        let mut checker = Checker::new(Options::lenient(), hash());
        check(
            &mut checker,
            Kind::Tree,
            &tree(&[("100644", b".gitattributes", &base.subtree)]),
        )?;
        let mut db = Db::default();
        if let Some(kind) = stored {
            db.0.insert(base.subtree, (kind, tree(&[])));
        }
        let mut out = Vec::new();
        assert!(checker.finish(&db, &mut out)?, "{expected:?}");
        assert_eq!(ids(&out), [(expected, Severity::Error)]);
    }
    let mut checker = Checker::new(Options::strict(), hash());
    check(
        &mut checker,
        Kind::Tree,
        &tree(&[("100644", b".gitattributes", &base.subtree)]),
    )?;
    assert!(matches!(
        checker.finish(&Db::default(), &mut Vec::new()),
        Err(LinkError::Missing { .. })
    ));
    Ok(())
}

/// Check `.gitmodules` as it is part of a tree that comes first, and return what is reported about it.
fn gitmodules_reports(content: &[u8], options: Options) -> TestResult<Vec<gix_fsck::Report>> {
    let blob_id = id_of(Kind::Blob, content);
    let mut checker = Checker::new(options, hash());
    let mut out = Vec::new();
    check(&mut checker, Kind::Tree, &tree(&[("100644", b".gitmodules", &blob_id)]))?;
    let mut failed = checker.needs_blob_content(&blob_id, content.len() as u64);
    if failed {
        let data = check(&mut checker, Kind::Blob, content)?;
        out = data.1;
        failed = data.0;
    } else {
        // Too large to be read, which is reported when it is received.
        checker.check(&blob_id, Kind::Blob, content.len() as u64, None, &mut out)?;
    }
    let _ = failed;
    Ok(out
        .into_iter()
        .filter_map(|d| match d {
            Diagnostic::Report(r) => Some(r),
            Diagnostic::Note(_) => None,
        })
        .collect())
}

fn gitmodules_ids(content: &[u8]) -> TestResult<Vec<(MessageId, Severity)>> {
    Ok(gitmodules_reports(content, Options::strict())?
        .into_iter()
        .map(|r| (r.id, r.severity))
        .collect())
}

#[test]
fn gitmodules_are_parsed_like_the_file_that_is_checked_out_is() -> TestResult {
    let bad = [(MessageId::GitmodulesUrl, Severity::Error)];
    let parse_error = [(MessageId::GitmodulesParse, Severity::Warn)];
    let entry = b"[submodule \"a\"]\n\turl = -oProxyCommand=x\n";
    assert_eq!(gitmodules_ids(entry)?, bad, "the baseline is refused");

    // The file is read like the one in a work tree: every byte but the end is data, and a byte order mark at the start is skipped.
    for (name, content) in [
        ("bom", [&[0xef, 0xbb, 0xbf][..], entry].concat()),
        (
            "0xff in an earlier value",
            [&b"[submodule \"b\"]\n\tpath = ok\xff\n"[..], entry].concat(),
        ),
        ("0xff in a comment", [&b"# c\xff\n"[..], entry].concat()),
        (
            "0xff in an earlier name",
            [&b"[submodule \"b\xff\"]\n"[..], entry].concat(),
        ),
        ("0xff at the start", [&b"\xff"[..], entry].concat()),
    ] {
        let ids = gitmodules_ids(&content)?;
        if name == "0xff at the start" {
            // That's not a valid file, which Git only warns about as a file that can't be parsed can't do harm.
            assert_eq!(ids, parse_error, "{name}");
        } else {
            assert_eq!(ids, bad, "{name}: the entry is found");
        }
    }

    for (name, content) in [
        ("partial mark", [&[0xef, 0xbb][..], entry].concat()),
        ("just the first byte", vec![0xef]),
        (
            "mark twice",
            [&[0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf][..], entry].concat(),
        ),
    ] {
        assert_eq!(gitmodules_ids(&content)?, parse_error, "{name} can't be parsed");
    }

    let reports = gitmodules_reports(b"[submodule \"a\"]\n\turl = -x\xff\xc3\n", Options::strict())?;
    assert_eq!(
        reports.iter().map(|r| r.message.as_slice()).collect::<Vec<_>>(),
        [&b"disallowed submodule url: -x\xff\xc3"[..]],
        "values are shown as they are, even though they aren't UTF-8"
    );
    Ok(())
}

#[test]
fn a_byte_order_mark_doesnt_change_what_is_found() -> TestResult {
    let mut compared = 0;
    for (name, content) in super::gitmodules::syntax_variants() {
        if content.first() == Some(&0xef) {
            continue;
        }
        let with_mark = [&[0xef, 0xbb, 0xbf][..], &content].concat();
        assert_eq!(
            gitmodules_ids(&with_mark)?,
            gitmodules_ids(&content)?,
            "{name}: a byte order mark is skipped"
        );
        compared += 1;
    }
    assert!(compared > 50, "there is a lot to compare: {compared}");
    Ok(())
}

#[test]
fn gitmodules_that_are_too_large_are_never_parsed_regardless_of_the_order() -> TestResult {
    let content = b"[submodule \"a\"]\n\turl = -x\n";
    let blob_id = id_of(Kind::Blob, content);
    let mut options = Options::strict();
    options.set_big_file_threshold(content.len() as u64 - 1);
    let large = [(MessageId::GitmodulesLarge, Severity::Error)];

    // The tree is first, so nothing has to be read.
    assert_eq!(gitmodules_reports(content, options.clone())?.len(), 1);
    assert_eq!(
        gitmodules_reports(content, options.clone())?[0].id,
        MessageId::GitmodulesLarge
    );

    // The blob is first, which makes it a deferred check that must not be read by `finish()`.
    let mut db = Db::default();
    db.add(Kind::Blob, content);
    let mut checker = Checker::new(options, hash());
    let mut out = Vec::new();
    checker.check(&blob_id, Kind::Blob, content.len() as u64, None, &mut out)?;
    check(&mut checker, Kind::Tree, &tree(&[("100644", b".gitmodules", &blob_id)]))?;
    assert!(checker.finish(&db, &mut out)?);
    assert_eq!(ids(&out), large);
    Ok(())
}

#[test]
fn what_is_linked_to_can_be_left_out_if_asked_not_to_look_for_it() -> TestResult {
    // A commit whose tree wasn't received, as it is when a remote leaves out objects on purpose.
    let tree_id = id_of(Kind::Tree, &tree(&[]));
    let commit = format!("tree {tree_id}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n").into_bytes();
    let commit_id = id_of(Kind::Commit, &commit);
    for (checker, expect_found_missing) in [
        (Checker::new(Options::strict(), hash()), true),
        (Checker::new(Options::strict(), hash()).without_links(), false),
    ] {
        let mut checker = checker;
        let failed = checker.check(
            &commit_id,
            Kind::Commit,
            commit.len() as u64,
            Some(&commit),
            &mut Vec::new(),
        )?;
        assert!(!failed, "the commit itself is fine");
        let result = checker.finish(&Db::default(), &mut Vec::new());
        assert_eq!(
            result.is_err(),
            expect_found_missing,
            "a missing tree is only noticed if links are looked at: {result:?}"
        );
    }
    Ok(())
}
