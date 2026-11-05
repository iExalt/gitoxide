//! Objects that are valid or invalid in many ways, and mutations of them.
use gix_hash::ObjectId;
use gix_object::Kind;

use super::oracle::{Base, Case, case};

pub const IDENT: &str = "A U Thor <a@example.com> 1234567890 +0100";

/// An id that isn't in `base` or anywhere else.
pub fn nowhere(_base: &Base) -> ObjectId {
    gix_object::compute_hash(gix_testtools::object_hash(), Kind::Blob, b"nowhere").expect("hashable")
}

pub fn commit(base: &Base, author: &str, committer: &str) -> Vec<u8> {
    format!(
        "tree {}\nparent {}\nauthor {author}\ncommitter {committer}\n\nmessage\n",
        base.tree, base.parent
    )
    .into_bytes()
}

pub fn tag(base: &Base, tagger: &str) -> Vec<u8> {
    format!(
        "object {}\ntype commit\ntag v1\ntagger {tagger}\n\nmessage\n",
        base.commit
    )
    .into_bytes()
}

pub fn tree(entries: &[(&str, &[u8], &ObjectId)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (mode, name, id) in entries {
        out.extend_from_slice(mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(name);
        out.push(0);
        out.extend_from_slice(id.as_slice());
    }
    out
}

/// The ways an author, committer or tagger line can be wrong, with the message they are expected to cause in strict mode.
pub fn bad_idents() -> Vec<(&'static str, &'static str)> {
    vec![
        ("missing-name-before-email", "<a@example.com> 1234567890 +0100"),
        ("missing-email", "A U Thor"),
        ("missing-email-newline-after-name", "A U Thor "),
        ("bad-name", "A U Thor> <a@example.com> 1234567890 +0100"),
        ("missing-space-before-email", "A U Thor<a@example.com> 1234567890 +0100"),
        ("bad-email-nested", "A U Thor <a@<example.com> 1234567890 +0100"),
        ("bad-email-unterminated", "A U Thor <a@example.com 1234567890 +0100"),
        ("missing-space-before-date", "A U Thor <a@example.com>1234567890 +0100"),
        ("bad-date-letters", "A U Thor <a@example.com> x +0100"),
        ("bad-date-missing", "A U Thor <a@example.com> "),
        ("zero-padded-date", "A U Thor <a@example.com> 01234 +0100"),
        (
            "date-overflow-20-digits",
            "A U Thor <a@example.com> 99999999999999999999 +0100",
        ),
        (
            "date-overflow-23-digits",
            "A U Thor <a@example.com> 99999999999999999999999 +0100",
        ),
        (
            "date-overflow-i64-max-plus-one",
            "A U Thor <a@example.com> 9223372036854775808 +0100",
        ),
        ("bad-date-no-space-after", "A U Thor <a@example.com> 1234567890+0100"),
        ("bad-date-tab-after", "A U Thor <a@example.com> 1234567890\t+0100"),
        ("bad-timezone-short", "A U Thor <a@example.com> 1234567890 +010"),
        ("bad-timezone-no-sign", "A U Thor <a@example.com> 1234567890 0100"),
        ("bad-timezone-long", "A U Thor <a@example.com> 1234567890 +01000"),
        (
            "bad-timezone-trailing-space",
            "A U Thor <a@example.com> 1234567890 +0100 ",
        ),
        ("bad-timezone-letter", "A U Thor <a@example.com> 1234567890 +01x0"),
    ]
}

/// Idents that are fine even if unusual.
pub fn good_idents() -> Vec<(&'static str, &'static str)> {
    vec![
        ("zero-date", "A U Thor <a@example.com> 0 +0000"),
        ("i64-max-date", "A U Thor <a@example.com> 9223372036854775807 +0100"),
        (
            "extra-whitespace-before-date",
            "A U Thor <a@example.com>   \t 1234567890 +0100",
        ),
        ("negative-timezone", "A U Thor <a@example.com> 1234567890 -1200"),
        ("empty-email", "A U Thor <> 1234567890 +0100"),
        ("space-only-name", " <a@example.com> 1234567890 +0100"),
    ]
}

pub fn commits(base: &Base) -> Vec<Case> {
    let mut out = vec![case("commit-valid", Kind::Commit, commit(base, IDENT, IDENT))];
    for (name, ident) in bad_idents() {
        out.push(case(
            format!("commit-author-{name}"),
            Kind::Commit,
            commit(base, ident, IDENT),
        ));
        out.push(case(
            format!("commit-committer-{name}"),
            Kind::Commit,
            commit(base, IDENT, ident),
        ));
    }
    for (name, ident) in good_idents() {
        out.push(case(
            format!("commit-good-ident-{name}"),
            Kind::Commit,
            commit(base, ident, ident),
        ));
    }
    let (tree, parent) = (base.tree, base.parent);
    let valid = String::from_utf8(commit(base, IDENT, IDENT)).expect("valid");
    let raw = |name: &str, data: String| case(name, Kind::Commit, data.into_bytes());
    out.extend([
        raw(
            "commit-no-parent",
            format!("tree {tree}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n"),
        ),
        raw(
            "commit-two-parents",
            format!("tree {tree}\nparent {parent}\nparent {parent}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n"),
        ),
        raw(
            "commit-missing-author",
            format!("tree {tree}\ncommitter {IDENT}\n\nm\n"),
        ),
        raw(
            "commit-multiple-authors",
            format!("tree {tree}\nauthor {IDENT}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n"),
        ),
        raw(
            "commit-missing-committer",
            format!("tree {tree}\nauthor {IDENT}\n\nm\n"),
        ),
        raw(
            "commit-missing-committer-no-body",
            format!("tree {tree}\nauthor {IDENT}\n"),
        ),
        raw(
            "commit-missing-tree",
            format!("author {IDENT}\ncommitter {IDENT}\n\nm\n"),
        ),
        raw("commit-only-tree-line", format!("tree {tree}\n")),
        raw("commit-tree-line-without-newline", format!("tree {tree}")),
        raw(
            "commit-bad-tree-hex",
            format!(
                "tree {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n",
                "g".repeat(tree.to_string().len())
            ),
        ),
        raw(
            "commit-short-tree-hex",
            format!(
                "tree {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n",
                &tree.to_string()[1..]
            ),
        ),
        raw(
            "commit-uppercase-tree-hex",
            format!(
                "tree {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n",
                tree.to_string().to_uppercase()
            ),
        ),
        raw(
            "commit-bad-parent-hex",
            format!(
                "tree {tree}\nparent {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n",
                "z".repeat(parent.to_string().len())
            ),
        ),
        raw(
            "commit-short-parent",
            format!(
                "tree {tree}\nparent {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n",
                &parent.to_string()[3..]
            ),
        ),
        raw("commit-parent-at-the-end", format!("tree {tree}\nparent {parent}\n")),
        raw("commit-parent-without-newline", format!("tree {tree}\nparent {parent}")),
        raw(
            "commit-parent-missing-object",
            format!(
                "tree {tree}\nparent {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n",
                nowhere(base)
            ),
        ),
        raw(
            "commit-parent-same-id-as-tree",
            format!("tree {tree}\nparent {tree}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n"),
        ),
        raw(
            "commit-tree-is-a-blob",
            format!("tree {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n", base.blob),
        ),
        raw(
            "commit-unterminated",
            format!("tree {tree}\nauthor {IDENT}\ncommitter {IDENT}"),
        ),
        raw(
            "commit-body-without-header-end",
            format!("tree {tree}\nauthor {IDENT}\ncommitter {IDENT}\nbody"),
        ),
        raw(
            "commit-nul-in-header",
            format!("tree {tree}\nauthor {IDENT}\ncommitter {IDENT}\nx\0y\n\nm\n"),
        ),
        raw(
            "commit-nul-in-body",
            format!("tree {tree}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\0m\n"),
        ),
        raw(
            "commit-extra-headers",
            format!(
                "tree {tree}\nauthor {IDENT}\ncommitter {IDENT}\nencoding latin1\ngpgsig x\n y\n z\nmergetag w\n\nm\n"
            ),
        ),
        raw(
            "commit-no-message",
            format!("tree {tree}\nauthor {IDENT}\ncommitter {IDENT}\n"),
        ),
        raw(
            "commit-empty-message-body",
            format!("tree {tree}\nauthor {IDENT}\ncommitter {IDENT}\n\n"),
        ),
        raw(
            "commit-author-after-committer",
            format!("tree {tree}\ncommitter {IDENT}\nauthor {IDENT}\n\nm\n"),
        ),
        raw(
            "commit-parent-after-author",
            format!("tree {tree}\nauthor {IDENT}\nparent {parent}\ncommitter {IDENT}\n\nm\n"),
        ),
        raw("commit-blank-first", format!("\n{valid}")),
        raw("commit-empty", String::new()),
    ]);
    out
}

pub fn tags(base: &Base) -> Vec<Case> {
    let mut out = vec![case("tag-valid", Kind::Tag, tag(base, IDENT))];
    for (name, ident) in bad_idents() {
        out.push(case(format!("tag-tagger-{name}"), Kind::Tag, tag(base, ident)));
        // The quirk of Git 2.56: the error of the tagger isn't a failure if an extra header follows.
        let mut data = tag(base, ident);
        let pos = data.windows(2).position(|w| w == b"\n\n").expect("header end");
        data.splice(pos + 1..pos + 1, b"foo bar\n".iter().copied());
        out.push(case(format!("tag-tagger-{name}-extra-header"), Kind::Tag, data));
    }
    for (name, ident) in good_idents() {
        out.push(case(format!("tag-good-ident-{name}"), Kind::Tag, tag(base, ident)));
    }
    let (commit, blob) = (base.commit, base.blob);
    let raw = |name: &str, data: String| case(name, Kind::Tag, data.into_bytes());
    let body = "\nmessage\n";
    out.extend([
        raw("tag-no-tagger", format!("object {commit}\ntype commit\ntag v1\n{body}")),
        raw(
            "tag-no-tagger-no-body",
            format!("object {commit}\ntype commit\ntag v1\n"),
        ),
        raw(
            "tag-of-blob",
            format!("object {blob}\ntype blob\ntag v1\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-type-mismatch",
            format!("object {blob}\ntype commit\ntag v1\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-of-tag",
            format!("object {}\ntype tag\ntag v1\ntagger {IDENT}\n{body}", base.tag),
        ),
        raw(
            "tag-of-missing-object",
            format!("object {}\ntype commit\ntag v1\ntagger {IDENT}\n{body}", base.tree),
        ),
        raw(
            "tag-bad-type",
            format!("object {commit}\ntype bogus\ntag v1\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-long-type",
            format!(
                "object {commit}\ntype {}\ntag v1\ntagger {IDENT}\n{body}",
                "x".repeat(25)
            ),
        ),
        raw(
            "tag-missing-object",
            format!("type commit\ntag v1\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-bad-object-hex",
            format!(
                "object {}\ntype commit\ntag v1\ntagger {IDENT}\n{body}",
                "q".repeat(commit.to_string().len())
            ),
        ),
        raw(
            "tag-missing-type",
            format!("object {commit}\ntag v1\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-missing-tag",
            format!("object {commit}\ntype commit\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-empty",
            format!("object {commit}\ntype commit\ntag \ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-dotdot",
            format!("object {commit}\ntype commit\ntag a..b\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-space",
            format!("object {commit}\ntype commit\ntag a b\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-trailing-slash",
            format!("object {commit}\ntype commit\ntag v/\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-lock",
            format!("object {commit}\ntype commit\ntag v.lock\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-control",
            format!("object {commit}\ntype commit\ntag v\u{1}1\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-at-brace",
            format!("object {commit}\ntype commit\ntag v@{{1}}\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-nested",
            format!("object {commit}\ntype commit\ntag release/v1\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-unicode",
            format!("object {commit}\ntype commit\ntag v\u{e9}\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-star",
            format!("object {commit}\ntype commit\ntag v*\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-backslash",
            format!("object {commit}\ntype commit\ntag v\\1\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-leading-dot",
            format!("object {commit}\ntype commit\ntag .v1\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-name-single-at",
            format!("object {commit}\ntype commit\ntag @\ntagger {IDENT}\n{body}"),
        ),
        raw(
            "tag-gpgsig",
            format!(
                "object {commit}\ntype commit\ntag v1\ntagger {IDENT}\ngpgsig -----BEGIN\n second\n -----END\n{body}"
            ),
        ),
        raw(
            "tag-gpgsig-sha256",
            format!("object {commit}\ntype commit\ntag v1\ntagger {IDENT}\ngpgsig-sha256 -----BEGIN\n second\n{body}"),
        ),
        raw(
            "tag-gpgsig-then-extra",
            format!("object {commit}\ntype commit\ntag v1\ntagger {IDENT}\ngpgsig x\n y\nfoo bar\n{body}"),
        ),
        raw(
            "tag-gpgsig-unterminated",
            format!("object {commit}\ntype commit\ntag v1\ntagger {IDENT}\ngpgsig x\n y"),
        ),
        raw(
            "tag-extra-header",
            format!("object {commit}\ntype commit\ntag v1\ntagger {IDENT}\nfoo bar\n{body}"),
        ),
        raw(
            "tag-extra-header-no-tagger",
            format!("object {commit}\ntype commit\ntag v1\nfoo bar\n{body}"),
        ),
        raw(
            "tag-unterminated",
            format!("object {commit}\ntype commit\ntag v1\ntagger {IDENT}"),
        ),
        raw(
            "tag-nul-in-header",
            format!("object {commit}\ntype commit\ntag v1\ntagger {IDENT}\nx\0\n{body}"),
        ),
        raw(
            "tag-nul-in-body",
            format!("object {commit}\ntype commit\ntag v1\ntagger {IDENT}\n\nm\0m\n"),
        ),
        raw("tag-minimal-no-body", format!("object {commit}\ntype commit\ntag v1\n")),
        raw("tag-empty", String::new()),
    ]);
    out
}

pub fn trees(base: &Base) -> Vec<Case> {
    let (blob, sub) = (base.blob, base.subtree);
    let null = ObjectId::null(gix_testtools::object_hash());
    let t = |name: &str, entries: &[(&str, &[u8], &ObjectId)]| case(name, Kind::Tree, tree(entries));
    let long_name = vec![b'a'; 4097];
    let ok_long_name = vec![b'a'; 4096];
    let valid = tree(&[("100644", b"a", &blob), ("40000", b"d", &sub)]);
    let mut out = vec![
        t("tree-valid", &[("100644", b"a", &blob), ("40000", b"d", &sub)]),
        t("tree-empty", &[]),
        t("tree-executable", &[("100755", b"x", &blob)]),
        t("tree-symlink", &[("120000", b"l", &blob)]),
        t("tree-gitlink", &[("160000", b"s", &blob)]),
        t("tree-gitlink-to-anything", &[("160000", b"s", &null)]),
        t("tree-zero-padded-mode", &[("0100644", b"a", &blob)]),
        t("tree-zero-padded-directory", &[("040000", b"d", &sub)]),
        t("tree-mode-100664", &[("100664", b"a", &blob)]),
        t("tree-mode-100600", &[("100600", b"a", &blob)]),
        t("tree-mode-20000", &[("20000", b"a", &blob)]),
        t("tree-mode-0", &[("0", b"a", &blob)]),
        t("tree-mode-with-overflow", &[("177777777777777", b"a", &blob)]),
        t("tree-mode-non-octal", &[("10x644", b"a", &blob)]),
        t("tree-mode-8", &[("100648", b"a", &blob)]),
        t("tree-mode-empty", &[("", b"a", &blob)]),
        t("tree-null-sha", &[("100644", b"a", &null)]),
        t("tree-missing-blob", &[("100644", b"a", &sub)]),
        t("tree-blob-entry-is-tree", &[("100644", b"a", &sub)]),
        t("tree-dir-entry-is-blob", &[("40000", b"d", &blob)]),
        t("tree-full-path", &[("100644", b"a/b", &blob)]),
        t("tree-dot", &[("100644", b".", &blob)]),
        t("tree-dotdot", &[("100644", b"..", &blob)]),
        t("tree-empty-name", &[("100644", b"", &blob)]),
        t("tree-dotgit", &[("100644", b".git", &blob)]),
        t("tree-dotgit-upper", &[("100644", b".GIT", &blob)]),
        t("tree-dotgit-mixed", &[("100644", b".GiT", &blob)]),
        t("tree-dotgit-dir", &[("40000", b".git", &sub)]),
        t("tree-git-tilde-1", &[("100644", b"git~1", &blob)]),
        t("tree-git-tilde-1-upper", &[("100644", b"GIT~1", &blob)]),
        t("tree-git-tilde-2", &[("100644", b"git~2", &blob)]),
        t("tree-dotgit-space", &[("100644", b".git ", &blob)]),
        t("tree-dotgit-dots", &[("100644", b".git...", &blob)]),
        t("tree-dotgit-stream", &[("100644", b".git::$INDEX_ALLOCATION", &blob)]),
        t("tree-dotgit-colon", &[("100644", b".git:stream", &blob)]),
        t("tree-dotgit-backslash-suffix", &[("100644", b"a\\.git", &blob)]),
        t("tree-dotgit-backslash-nested", &[("100644", b"a\\b\\.git\\c", &blob)]),
        t("tree-dotgit-backslash-after", &[("100644", b".git\\a", &blob)]),
        t("tree-dotgit-hfs-zwnj", &[("100644", ".gi\u{200c}t".as_bytes(), &blob)]),
        t("tree-dotgit-hfs-bom", &[("100644", ".git\u{feff}".as_bytes(), &blob)]),
        t("tree-dotgit-hfs-invalid-utf8", &[("100644", b".gi\xc0t", &blob)]),
        t("tree-dotgitx", &[("100644", b".gitx", &blob)]),
        t("tree-dot-git-lookalike", &[("100644", b".gi", &blob)]),
        t("tree-gitmodules-symlink", &[("120000", b".gitmodules", &blob)]),
        t("tree-gitmodules-symlink-upper", &[("120000", b".GITMODULES", &blob)]),
        t("tree-gitmodules-symlink-ntfs-short", &[("120000", b"GITMOD~1", &blob)]),
        t(
            "tree-gitmodules-symlink-ntfs-fallback",
            &[("120000", b"GI7EBA~1", &blob)],
        ),
        t(
            "tree-gitmodules-symlink-ntfs-space",
            &[("120000", b".gitmodules ", &blob)],
        ),
        t(
            "tree-gitmodules-symlink-ntfs-stream",
            &[("120000", b".gitmodules:x", &blob)],
        ),
        t(
            "tree-gitmodules-symlink-backslash",
            &[("120000", b"a\\.gitmodules", &blob)],
        ),
        t(
            "tree-gitmodules-symlink-hfs",
            &[("120000", ".git\u{200d}modules".as_bytes(), &blob)],
        ),
        t("tree-gitattributes-symlink", &[("120000", b".gitattributes", &blob)]),
        t("tree-gitattributes-symlink-ntfs", &[("120000", b"GITATT~1", &blob)]),
        t(
            "tree-gitattributes-symlink-ntfs-fallback",
            &[("120000", b"gi7d29~1", &blob)],
        ),
        t("tree-gitignore-symlink", &[("120000", b".gitignore", &blob)]),
        t("tree-gitignore-symlink-ntfs", &[("120000", b"gitign~1", &blob)]),
        t(
            "tree-gitignore-symlink-ntfs-fallback",
            &[("120000", b"gi250a~1", &blob)],
        ),
        t("tree-mailmap-symlink", &[("120000", b".mailmap", &blob)]),
        t("tree-mailmap-symlink-ntfs", &[("120000", b"MAILMA~1", &blob)]),
        t("tree-mailmap-symlink-ntfs-fallback", &[("120000", b"maba30~1", &blob)]),
        t("tree-mailmap-regular-file", &[("100644", b".mailmap", &blob)]),
        t("tree-gitignore-regular-file", &[("100644", b".gitignore", &blob)]),
        t("tree-unsorted", &[("100644", b"b", &blob), ("100644", b"a", &blob)]),
        t(
            "tree-unsorted-directory-slash",
            &[("40000", b"a", &sub), ("100644", b"a.b", &blob)],
        ),
        t(
            "tree-sorted-directory-slash",
            &[("100644", b"a.b", &blob), ("40000", b"a", &sub)],
        ),
        t("tree-duplicate", &[("100644", b"a", &blob), ("100644", b"a", &blob)]),
        t(
            "tree-duplicate-file-and-directory",
            &[("100644", b"a", &blob), ("40000", b"a", &sub)],
        ),
        t(
            "tree-duplicate-non-consecutive",
            &[
                ("100644", b"foo", &blob),
                ("100644", b"foo.bar", &blob),
                ("100644", b"foo.bar.baz", &blob),
                ("40000", b"foo.bar", &sub),
                ("40000", b"foo", &sub),
            ],
        ),
        t(
            "tree-no-duplicate-lookalike",
            &[
                ("100644", b"foo", &blob),
                ("100644", b"foo.bar", &blob),
                ("40000", b"foo.baz", &sub),
            ],
        ),
        t("tree-large-name", &[("100644", &long_name, &blob)]),
        t("tree-name-at-the-limit", &[("100644", &ok_long_name, &blob)]),
        t("tree-name-with-newline", &[("100644", b"a\nb", &blob)]),
        t("tree-name-with-space", &[("100644", b"a b", &blob)]),
        t("tree-name-non-utf8", &[("100644", b"\xff\xfe", &blob)]),
        t(
            "tree-many-problems",
            &[
                ("0100644", b"b/c", &null),
                ("100664", b".", &blob),
                ("40000", b"..", &sub),
                ("100644", b".git", &blob),
                ("100644", b"", &blob),
                ("100644", b"a", &blob),
            ],
        ),
    ];
    out.push(case(
        "tree-trailing-garbage",
        Kind::Tree,
        [valid.clone(), b"x".to_vec()].concat(),
    ));
    out.push(case("tree-trailing-nul", Kind::Tree, [valid.clone(), vec![0]].concat()));
    out.push(case("tree-only-mode", Kind::Tree, b"100644".to_vec()));
    out.push(case("tree-mode-and-space-only", Kind::Tree, b"100644 ".to_vec()));
    out.push(case(
        "tree-leading-space-mode",
        Kind::Tree,
        [b" a\0".to_vec(), blob.as_slice().to_vec()].concat(),
    ));
    out.push(case("tree-second-entry-truncated", Kind::Tree, {
        let mut data = tree(&[("100644", b"a", &blob), ("100644", b"b", &blob)]);
        data.truncate(data.len() - 3);
        data
    }));
    out.push(case("tree-second-entry-empty-name", Kind::Tree, {
        let mut data = tree(&[("100644", b"a", &blob)]);
        data.extend_from_slice(&tree(&[("100644", b"", &blob)]));
        data
    }));
    out.push(case("tree-second-entry-bad-mode", Kind::Tree, {
        let mut data = tree(&[("100644", b"a", &blob)]);
        data.extend_from_slice(&tree(&[("1x", b"b", &blob)]));
        data
    }));
    out.push(case("tree-bad-entry-after-bad-name", Kind::Tree, {
        let mut data = tree(&[("100644", b".git", &blob), ("100644", b"b/c", &null)]);
        data.extend_from_slice(b"junk");
        data
    }));
    out
}

/// All valid objects the truncation tests start from.
pub fn valid(base: &Base) -> Vec<Case> {
    let (blob, sub) = (base.blob, base.subtree);
    vec![
        case("commit", Kind::Commit, commit(base, IDENT, IDENT)),
        case("tag", Kind::Tag, tag(base, IDENT)),
        case(
            "tree",
            Kind::Tree,
            tree(&[("100644", b"a", &blob), ("40000", b"d", &sub), ("120000", b"l", &blob)]),
        ),
    ]
}

/// Every proper prefix of `valid` objects, including the empty one.
pub fn truncations(base: &Base) -> Vec<Case> {
    let mut out = Vec::new();
    for valid in valid(base) {
        for len in 0..valid.data.len() {
            out.push(case(
                format!("{}-truncated-to-{len}", valid.name),
                valid.kind,
                valid.data[..len].to_vec(),
            ));
        }
    }
    out
}

/// Cases with other severities, to see how they change the outcome, and objects to skip.
pub fn configured(base: &Base) -> Vec<Case> {
    let (blob, sub) = (base.blob, base.subtree);
    let null = ObjectId::null(gix_testtools::object_hash());
    let bad_tz = || {
        case(
            "commit",
            Kind::Commit,
            commit(base, "A U Thor <a@example.com> 1234567890 +010", IDENT),
        )
    };
    let zero_pad = || case("tree-zero-pad", Kind::Tree, tree(&[("0100644", b"a", &blob)]));
    let many = || {
        case(
            "tree-many",
            Kind::Tree,
            tree(&[
                ("0100644", b"b/c", &null),
                ("100664", b".", &blob),
                ("40000", b"..", &sub),
                ("100644", b".git", &blob),
                ("100644", b"", &blob),
                ("100644", b"a", &blob),
            ]),
        )
    };
    let extra_tag = || {
        let mut data = tag(base, IDENT);
        let pos = data.windows(2).position(|w| w == b"\n\n").expect("header end");
        data.splice(pos + 1..pos + 1, b"foo bar\n".iter().copied());
        case("tag-extra", Kind::Tag, data)
    };
    let mut out = vec![
        bad_tz().with("badTimezone", "ignore"),
        bad_tz().with("badTimezone", "warn"),
        bad_tz().with("badTimezone", "error"),
        bad_tz().with("badtimezone", "ignore"),
        bad_tz().with("BADTIMEZONE", "warn"),
        bad_tz().with("badDate", "ignore"),
        bad_tz().skipped(),
        zero_pad().with("zeroPaddedFilemode", "ignore"),
        zero_pad().with("zeroPaddedFilemode", "warn"),
        zero_pad().with("zeroPaddedFilemode", "error"),
        zero_pad().with("badFilemode", "error"),
        zero_pad().skipped(),
        many().with("nullSha1", "ignore"),
        many().with("fullPathname", "ignore").with("hasDot", "warn"),
        many().with("badFilemode", "error").with("hasDotdot", "ignore"),
        many()
            .with("hasDotgit", "ignore")
            .with("emptyName", "ignore")
            .with("treeNotSorted", "warn"),
        many().skipped(),
        extra_tag().with("extraHeaderEntry", "error"),
        extra_tag().with("extraHeaderEntry", "warn"),
        extra_tag().with("extraHeaderEntry", "ignore"),
        case(
            "tag-no-tagger",
            Kind::Tag,
            format!("object {}\ntype commit\ntag v1\n\nm\n", base.commit),
        )
        .with("missingTaggerEntry", "error"),
        case(
            "tag-no-tagger",
            Kind::Tag,
            format!("object {}\ntype commit\ntag v1\n\nm\n", base.commit),
        )
        .with("missingTaggerEntry", "ignore"),
        case(
            "tag-bad-name",
            Kind::Tag,
            format!("object {}\ntype commit\ntag a..b\ntagger {IDENT}\n\nm\n", base.commit),
        )
        .with("badTagName", "error"),
        case(
            "tag-bad-name",
            Kind::Tag,
            format!("object {}\ntype commit\ntag a..b\ntagger {IDENT}\n\nm\n", base.commit),
        )
        .with("badTagName", "ignore"),
        case("tree-symlink", Kind::Tree, tree(&[("120000", b".gitignore", &blob)])).with("gitignoreSymlink", "error"),
        case("tree-symlink", Kind::Tree, tree(&[("120000", b".gitmodules", &blob)]))
            .with("gitmodulesSymlink", "ignore"),
        case("tree-symlink", Kind::Tree, tree(&[("120000", b".gitmodules", &blob)])).with("gitmodulesSymlink", "warn"),
        case(
            "tree-symlink",
            Kind::Tree,
            tree(&[("120000", b".gitattributes", &blob)]),
        )
        .with("gitattributesSymlink", "error"),
        case("tree-symlink", Kind::Tree, tree(&[("120000", b".mailmap", &blob)])).with("mailmapSymlink", "error"),
        case("tree-large-name", Kind::Tree, tree(&[("100644", &[b'a'; 150], &blob)])).with("largePathname", "warn:100"),
        case("tree-large-name", Kind::Tree, tree(&[("100644", &[b'a'; 150], &blob)]))
            .with("largePathname", "error:100"),
        case("tree-large-name", Kind::Tree, tree(&[("100644", &[b'a'; 150], &blob)])).with("largePathname", "ignore"),
        case("tree-large-name", Kind::Tree, tree(&[("100644", &[b'a'; 150], &blob)])).with("largePathname", "error:1k"),
        case(
            "tree-large-name",
            Kind::Tree,
            tree(&[("100644", &vec![b'a'; 1500], &blob)]),
        )
        .with("largePathname", "warn:1k"),
        case("tree-large-name", Kind::Tree, tree(&[("100644", &[b'a'; 150], &blob)]))
            .with("largePathname", "warn:0x64"),
        case("tree-large-name", Kind::Tree, tree(&[("100644", &[b'a'; 150], &blob)]))
            .with("largePathname", "warn:0144"),
        case("tree-large-name", Kind::Tree, tree(&[("100644", &[b'a'; 150], &blob)])).with("largePathname", "warn:-1"),
        case(
            "commit-nul-header",
            Kind::Commit,
            format!("tree {}\nauthor {IDENT}\ncommitter {IDENT}\nx\0y\n\nm\n", base.tree),
        )
        .with("nulInHeader", "error"),
        case(
            "commit-nul-header",
            Kind::Commit,
            format!("tree {}\nauthor {IDENT}\ncommitter {IDENT}\nx\0y\n\nm\n", base.tree),
        )
        .skipped(),
        case(
            "commit-nul-body",
            Kind::Commit,
            format!("tree {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\0m\n", base.tree),
        )
        .with("nulInCommit", "warn"),
        case(
            "commit-nul-body",
            Kind::Commit,
            format!("tree {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\0m\n", base.tree),
        )
        .with("nulInCommit", "ignore"),
        case("commit-bad-author", Kind::Commit, commit(base, "A U Thor", IDENT)).with("missingEmail", "warn"),
        case("commit-bad-author", Kind::Commit, commit(base, "A U Thor", IDENT)).with("missingEmail", "ignore"),
        case("commit-bad-author", Kind::Commit, commit(base, "A U Thor", IDENT)).skipped(),
        case(
            "tag-missing-object",
            Kind::Tag,
            format!("type commit\ntag v1\ntagger {IDENT}\n\nm\n"),
        )
        .skipped(),
        case(
            "tag-missing-object",
            Kind::Tag,
            format!("type commit\ntag v1\ntagger {IDENT}\n\nm\n"),
        )
        .with("missingObject", "ignore"),
        case(
            "commit-missing-tree",
            Kind::Commit,
            format!("author {IDENT}\ncommitter {IDENT}\n\nm\n"),
        )
        .with("missingTree", "ignore"),
        case(
            "commit-bad-tree",
            Kind::Commit,
            format!(
                "tree {}\nauthor {IDENT}\ncommitter {IDENT}\n\nm\n",
                "g".repeat(base.tree.to_string().len())
            ),
        )
        .with("badTreeSha1", "ignore"),
    ];
    out.push(case("tree-ok", Kind::Tree, tree(&[("100644", b"a", &blob)])).with("badTree", "ignore"));
    out
}
