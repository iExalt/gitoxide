//! `.gitmodules` files with all kinds of values, which are received together with the tree that names them.
use std::fmt::Write;

use gix_object::Kind;

use super::{
    corpus::tree,
    oracle::{Base, Case, case, id_of},
};

/// A tree that has `entry` for `content`, with the content before or after it in the pack.
fn pack(base: &Base, name: &str, entry: &[u8], mode: &str, content: &[u8], blob_first: bool) -> Case {
    let blob_id = id_of(Kind::Blob, content);
    let _ = base;
    let case = case(name, Kind::Tree, tree(&[(mode, entry, &blob_id)]));
    if blob_first {
        case.blob_before(content)
    } else {
        case.blob_after(content)
    }
}

pub const NAMES: &[&str] = &[
    "a",
    "sub",
    "a/b",
    "..",
    "../a",
    "a/..",
    "a/../b",
    "..\\a",
    "a\\..",
    ".",
    "...",
    "a..b",
    "x/..y",
    "a/b/../c",
    "..a",
    "a b",
    "é",
    "a.b",
    "a/./b",
    "a//b",
    "a/",
    "/a",
    "\\",
    "a\\\\..\\\\b",
];

pub const URLS: &[&str] = &[
    "https://example.com/r.git",
    "-x",
    "--upload-pack=x",
    "../x",
    "./x",
    "../../x",
    "../:x",
    "..//x",
    ".././../x",
    "..\\x",
    ".\\x",
    ".\\..\\x",
    "./../x",
    "../",
    "..",
    "./",
    ".",
    "git://h/p",
    "git://h/%0ap",
    "git://h/%0Ap",
    "git://h/%250a",
    "git://h/\\n",
    "./a%0Ab",
    "./a%0ab%0",
    "./a%00b",
    "../a\\nb",
    "http::https://x/y",
    "http::x",
    "http::",
    "https::example.com/x",
    "ftp::ftp://x/y",
    "ftps::ftps://x/y%0a",
    "https:///x",
    "https://",
    "https://host:70000/x",
    "https://host:65535/x",
    "https://host:65536/x",
    "https://host:0/x",
    "https://host:000/x",
    "https://host:80x/x",
    "https://host:/x",
    "https://host:00443/x",
    "http://host:80/x",
    "http://host:0080/x",
    "http://host:8080/x",
    "https://u:p@host/p",
    "https://u:p%0a@host/p",
    "https://u@@host/p",
    "https://@host/p",
    "https://:@host/p",
    "https://u:p%zz@host/p",
    "https://h/%zz",
    "https://h/%z",
    "https://h/%",
    "https://h/a b",
    "https://h/%0a",
    "https://h/%0A",
    "https://h/a\\nb",
    "https://h/a\\tb",
    "https://h/a%09b",
    "https://h/%00",
    "https://h/%25%30a",
    "https://h/%2e%2e/x",
    "https://h/a/../../x",
    "https://h/a/..",
    "https://h/..",
    "https://h/../x",
    "https://h/./x",
    "https://h/.",
    "https://h/a/./b",
    "https://h/a/%2E%2E/b",
    "https://h/a//b",
    "https://h//",
    "http://h:80/../../x",
    "http://h/a/../..",
    "ftp://h/x",
    "ftps://h/x",
    "ssh://h/x",
    "file:///x",
    "x://y",
    "HTTP://H/x",
    "Https://H.Example.COM/X%2f",
    "https://h:/x",
    "https://[::1]:8080/x",
    "https://[::1]/x",
    "https://[::1/x",
    "https://h_x.y/p",
    "https://h x/p",
    "https://h*x/p",
    "https://h/?q=%0A",
    "https://h/?q=%0a#%0a",
    "https://h?x",
    "https://h#x",
    "https://h:80?x",
    "https://h:80?x%0a",
    "https://h:8x0",
    "https://h:123456/x",
    "https://h:00000080/x",
    "https:/h/x",
    "https:h/x",
    "1http://h/x",
    "ht+tp://h/x",
    "http+x://h/x",
    "http://h/x%0A%0a",
    "http://h/%7e",
    "http://h/%7E%2D%2e",
    "http://h/%e9",
    "http://h/\\u{e9}",
    "https://h/a\nb",
    "../a\nb",
    "git://h/\n",
    "./a\nb",
    "\thttps://h/%0a",
    "git://x",
    "git:/x",
    "GIT://h/%0a",
    "git@host:path",
    "host:path",
    "/abs/path",
    "relative/path",
    "",
    " https://h/x",
    "\\thttps://h/%0a",
];

pub const PATHS: &[&str] = &["-x", "x", "", "--x", "a-b", " -x", "./-x", "-"];

pub const UPDATES: &[&str] = &[
    "none", "checkout", "rebase", "merge", "!x", "!", "other", "", "!none", " !x", "None", "!x y",
];

/// How a value is spelled.
pub fn quoted(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `.gitmodules` file contents which have different syntax, valid and not.
pub fn syntax_variants() -> Vec<(&'static str, Vec<u8>)> {
    let url = "https://example.com/r.git";
    let bad = "-x";
    let v = |name: &'static str, text: &str| (name, text.as_bytes().to_vec());
    vec![
        v("empty", ""),
        v("only-comment", "# nothing\n"),
        v("plain", &format!("[submodule \"a\"]\n\tpath = a\n\turl = {url}\n")),
        v("bad-url", &format!("[submodule \"a\"]\n\tpath = a\n\turl = {bad}\n")),
        v("bad-path", "[submodule \"a\"]\n\tpath = -x\n\turl = ../x\n"),
        v("bad-update", "[submodule \"a\"]\n\tupdate = !x\n"),
        v("bad-name-and-url", "[submodule \"..\"]\n\turl = -x\n"),
        v("name-empty", "[submodule \"\"]\n\turl = x\n"),
        v("legacy-section", "[submodule.a]\n\turl = -x\n"),
        v("legacy-section-dots", "[submodule.a.b]\n\turl = -x\n"),
        v("legacy-section-dotdot", "[submodule...]\n\turl = x\n"),
        v("section-case", "[SubModule \"A\"]\n\tURL = -x\n"),
        v("no-subsection", "[submodule]\n\turl = -x\n"),
        v("other-section", "[other \"a\"]\n\turl = -x\n"),
        v("section-prefix", "[submodules \"a\"]\n\turl = -x\n"),
        v(
            "two-subsections",
            "[submodule \"a\"]\n\turl = -x\n[submodule \"b\"]\n\turl = https://h/r\n[submodule \"c\"]\n\tpath = -y\n",
        ),
        v("key-without-value", "[submodule \"a\"]\n\turl\n\tpath\n\tupdate\n"),
        v("key-without-value-at-end", "[submodule \"a\"]\n\turl"),
        v("empty-value", "[submodule \"a\"]\n\turl =\n\tpath =\n\tupdate =\n"),
        v("value-no-newline-at-end", "[submodule \"a\"]\n\turl = -x"),
        v("crlf", "[submodule \"a\"]\r\n\turl = -x\r\n"),
        v("cr-in-value", "[submodule \"a\"]\n\turl = -x\r\r\n"),
        // Byte order marks and `0xff` bytes are tested in `checker.rs`. What is compared with Git here is a second mark, and a partial one.
        v("double-bom", "\u{feff}\u{feff}[submodule \"a\"]\n\turl = -x\n"),
        (
            "partial-bom-bytes",
            [&[0xef, 0xbb][..], b"[submodule \"a\"]\n\turl = -x\n"].concat(),
        ),
        v("comment-after-value", "[submodule \"a\"]\n\turl = -x # comment\n"),
        v(
            "semicolon-comment-after-value",
            "[submodule \"a\"]\n\turl = -x ; comment\n",
        ),
        v("comment-lines", "# c\n; c\n[submodule \"a\"]\n# c\n\turl = -x\n; c\n"),
        v("comment-in-quotes", "[submodule \"a\"]\n\turl = \"-x # no\"\n"),
        v("comment-after-section", "[submodule \"a\"] # c\n\turl = -x\n"),
        v("key-on-section-line", "[submodule \"a\"] url = -x\n"),
        v("quoted-value", "[submodule \"a\"]\n\turl = \"-x\"\n"),
        v("partly-quoted-value", "[submodule \"a\"]\n\turl = -\"x\"\n"),
        v("spaces-in-value", "[submodule \"a\"]\n\turl =   -x   y   \n"),
        v("trailing-spaces", "[submodule \"a\"]\n\turl = -x   \n"),
        v("trailing-spaces-in-quotes", "[submodule \"a\"]\n\turl = \"-x   \"\n"),
        v("tab-value", "[submodule \"a\"]\n\turl =\t-x\t\n"),
        v("continuation", "[submodule \"a\"]\n\turl = -\\\nx\n"),
        v("continuation-crlf", "[submodule \"a\"]\r\n\turl = -\\\r\nx\r\n"),
        v("escape-n", "[submodule \"a\"]\n\turl = \"a\\nb\"\n"),
        v("escape-t", "[submodule \"a\"]\n\turl = a\\tb\n"),
        v("escape-b", "[submodule \"a\"]\n\turl = a\\bb\n"),
        v("escape-quote", "[submodule \"a\"]\n\turl = \"a\\\"b\"\n"),
        v("escape-backslash-then-dash", "[submodule \"a\"]\n\tpath = \\\\-x\n"),
        v(
            "bad-escape",
            "[submodule \"a\"]\n\turl = -x\n\tpath = a\\qb\n\tupdate = !x\n",
        ),
        v("unterminated-quote", "[submodule \"a\"]\n\turl = -x\n\tpath = \"a\n"),
        v("unterminated-section", "[submodule \"a\"\n\turl = -x\n"),
        v("section-no-close", "[submodule \"a\n\turl = -x\n"),
        v("section-bad-char", "[sub$module \"a\"]\n\turl = -x\n"),
        v("section-empty", "[]\n\turl = -x\n"),
        v("section-only-space", "[ ]\n\turl = -x\n"),
        v("section-single-quote", "[submodule 'a']\n\turl = -x\n"),
        v("section-extra-space", "[submodule   \"a\"  ]\n\turl = -x\n"),
        v("section-trailing-after-quote", "[submodule \"a\"x]\n\turl = -x\n"),
        v("section-escape", "[submodule \"a\\\"b\"]\n\turl = -x\n"),
        v("section-escape-backslash", "[submodule \"a\\\\b\"]\n\turl = -x\n"),
        v("section-escape-dotdot", "[submodule \"a\\/..\"]\n\turl = -x\n"),
        v("section-dot-in-name", "[submodule \"a.url\"]\n\tpath = -x\n"),
        v("entry-before-section", "url = -x\n"),
        v("key-with-digit-first", "[submodule \"a\"]\n\t1url = -x\n"),
        v("key-with-dash", "[submodule \"a\"]\n\tu-rl = -x\n\tpa-th = -x\n"),
        v("key-with-underscore", "[submodule \"a\"]\n\tu_rl = -x\n"),
        v(
            "key-uppercase",
            "[submodule \"a\"]\n\tURL = -x\n\tPATH = -x\n\tUpDate = !x\n",
        ),
        v(
            "duplicate-keys",
            "[submodule \"a\"]\n\turl = https://h/ok\n\turl = -x\n",
        ),
        v("space-before-equals", "[submodule \"a\"]\n\turl  =  -x\n"),
        v("no-space-around-equals", "[submodule \"a\"]\n\turl=-x\n"),
        v("colon-instead-of-equals", "[submodule \"a\"]\n\turl: -x\n"),
        v("garbage-line", "[submodule \"a\"]\n\turl = -x\n$garbage\n\tpath = -y\n"),
        v("garbage-first", "garbage\n[submodule \"a\"]\n\turl = -x\n"),
        v("nul-in-value", "[submodule \"a\"]\n\turl = https://h/\u{0}-x\n"),
        v("nul-in-name", "[submodule \"a\u{0}..\"]\n\turl = x\n"),
        v("nul-leading-value", "[submodule \"a\"]\n\turl = \u{0}-x\n"),
        v("nul-at-start", "\u{0}[submodule \"a\"]\n\turl = -x\n"),
        v("vertical-tab", "[submodule \"a\"]\n\turl\u{b}= -x\n"),
        v("form-feed-before-key", "[submodule \"a\"]\n\u{c}\turl = -x\n"),
        v(
            "long-value",
            &format!("[submodule \"a\"]\n\turl = {}\n", "a".repeat(5000)),
        ),
        ("invalid-utf8", [&b"[submodule \"a\"]\n\turl = -x\x80\n"[..]].concat()),
        (
            "invalid-utf8-name",
            [&b"[submodule \"a\x80..\"]\n\turl = -x\n"[..]].concat(),
        ),
        v("update-only-bang", "[submodule \"a\"]\n\tupdate = !\n"),
        v("update-with-quote", "[submodule \"a\"]\n\tupdate = \"!x\"\n"),
        v("update-leading-space", "[submodule \"a\"]\n\tupdate = \" !x\"\n"),
        v("path-with-quote", "[submodule \"a\"]\n\tpath = \"-x\"\n"),
        v("path-leading-space", "[submodule \"a\"]\n\tpath = \" -x\"\n"),
        v(
            "many-submodules",
            &(0..20).fold(String::new(), |mut out, n| {
                let _ = writeln!(out, "[submodule \"s{n}\"]\n\tpath = s{n}\n\turl = ../s{n}");
                out
            }),
        ),
    ]
}

/// A deterministic pseudo-random number generator, as the cases don't change between runs.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn pick<'a, T>(&mut self, from: &'a [T]) -> &'a T {
        &from[(self.next() % from.len() as u64) as usize]
    }
}

pub fn cases(base: &Base) -> Vec<Case> {
    let mut out = Vec::new();
    for (name, content) in syntax_variants() {
        for blob_first in [false, true] {
            out.push(pack(
                base,
                &format!("gitmodules-{name}{}", if blob_first { "-blob-first" } else { "" }),
                b".gitmodules",
                "100644",
                &content,
                blob_first,
            ));
        }
    }
    for (idx, name) in NAMES.iter().enumerate() {
        let content = format!("[submodule {}]\n\turl = https://h/r\n", quoted(name));
        out.push(pack(
            base,
            &format!("gitmodules-name-{idx}"),
            b".gitmodules",
            "100644",
            content.as_bytes(),
            idx % 2 == 0,
        ));
    }
    for (idx, url) in URLS.iter().enumerate() {
        let content = format!("[submodule \"a\"]\n\turl = {}\n", quoted(url));
        out.push(pack(
            base,
            &format!("gitmodules-url-{idx}"),
            b".gitmodules",
            "100644",
            content.as_bytes(),
            idx % 2 == 0,
        ));
    }
    for (idx, path) in PATHS.iter().enumerate() {
        let content = format!("[submodule \"a\"]\n\tpath = {}\n", quoted(path));
        out.push(pack(
            base,
            &format!("gitmodules-path-{idx}"),
            b".gitmodules",
            "100644",
            content.as_bytes(),
            idx % 2 == 0,
        ));
    }
    for (idx, update) in UPDATES.iter().enumerate() {
        let content = format!("[submodule \"a\"]\n\tupdate = {}\n", quoted(update));
        out.push(pack(
            base,
            &format!("gitmodules-update-{idx}"),
            b".gitmodules",
            "100644",
            content.as_bytes(),
            idx % 2 == 0,
        ));
    }
    // Spellings of the file name that all lead to the blob being checked.
    for (idx, spelling) in [
        ".gitmodules",
        ".GITMODULES",
        "GITMOD~1",
        "gitmod~4",
        "GI7EBA~1",
        ".gitmodules ",
        ".gitmodules.",
        ".gitmodules:x",
        "a\\.gitmodules",
        "a\\b\\GITMOD~1",
        ".git\u{200c}modules",
        ".GitModules\u{feff}",
    ]
    .iter()
    .enumerate()
    {
        out.push(pack(
            base,
            &format!("gitmodules-spelling-{idx}"),
            spelling.as_bytes(),
            "100644",
            b"[submodule \"a\"]\n\turl = -x\n",
            idx % 2 == 0,
        ));
        out.push(pack(
            base,
            &format!("gitmodules-spelling-executable-{idx}"),
            spelling.as_bytes(),
            "100755",
            b"[submodule \"a\"]\n\turl = -x\n",
            idx % 2 == 1,
        ));
    }
    // Names that look similar to `.gitmodules` aren't looked at.
    for (idx, spelling) in [
        ".gitmodulesx",
        "gitmodules",
        ".gitmodule",
        "x.gitmodules",
        "GITMOD~5",
        "gi7ebax",
        "gitmod~",
    ]
    .iter()
    .enumerate()
    {
        out.push(pack(
            base,
            &format!("gitmodules-lookalike-{idx}"),
            spelling.as_bytes(),
            "100644",
            b"[submodule \"a\"]\n\turl = -x\n",
            false,
        ));
    }
    // A pseudo-random combination of everything.
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for idx in 0..400 {
        let mut content = String::new();
        for _ in 0..1 + rng.next() % 3 {
            let name = rng.pick(NAMES);
            let _ = writeln!(content, "[submodule {}]", quoted(name));
            for _ in 0..1 + rng.next() % 3 {
                let (key, value) = match rng.next() % 4 {
                    0 => ("url", *rng.pick(URLS)),
                    1 => ("path", *rng.pick(PATHS)),
                    2 => ("update", *rng.pick(UPDATES)),
                    _ => ("branch", "main"),
                };
                let _ = writeln!(content, "\t{key} = {}", quoted(value));
            }
        }
        out.push(pack(
            base,
            &format!("gitmodules-random-{idx}"),
            b".gitmodules",
            "100644",
            content.as_bytes(),
            rng.next().is_multiple_of(2),
        ));
    }
    // Mutations of the syntax, which can break parsing at any point.
    for (name, content) in syntax_variants().into_iter().take(12) {
        for pos in (0..content.len()).step_by(3) {
            let mut mutated = content.clone();
            mutated[pos] = [b'"', b'\n', b'\\', b'=', b'[', b']', b' ', b'#', 0][pos % 9];
            out.push(pack(
                base,
                &format!("gitmodules-mutated-{name}-{pos}"),
                b".gitmodules",
                "100644",
                &mutated,
                pos % 2 == 0,
            ));
        }
    }
    out
}

/// Many random URLs, built from pieces that matter to the way they are normalized.
/// Git decides if a `..` leaves the URL's path by comparing a pointer into the buffer it normalizes into after it may
/// have been reallocated, which it is when escaping makes the URL longer. The result then depends on the allocator, so
/// URLs that could do that are not compared, while the `..` handling is covered by all those without bytes to escape.
fn has_dot_segment_and_bytes_to_escape(url: &str) -> bool {
    let bytes = url.as_bytes();
    let needs_escape = bytes.iter().enumerate().any(|(idx, b)| match b {
        b'%' => !bytes
            .get(idx + 1..idx + 3)
            .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit)),
        b => !(b.is_ascii_alphanumeric() || b"._~:/?#@[]-".contains(b)),
    });
    needs_escape && (url.contains("..") || url.contains("/."))
}

pub fn random_url_cases(base: &Base, count: usize) -> Vec<Case> {
    const SCHEMES: &[&str] = &[
        "http",
        "https",
        "ftp",
        "ftps",
        "http::http",
        "https::https",
        "http::",
        "ftp::ftp",
        "git",
        "ssh",
        "file",
        "x",
        "HTTP",
        "",
        "../",
        "./",
        "..\\",
        "-",
        "https:",
    ];
    const SEPARATORS: &[&str] = &["://", ":/", ":", "//", ":///", "::", ""];
    const USERS: &[&str] = &["", "", "", "u@", "u:p@", "@", ":@", "u%40@", "u:p%0a@", "u%zz@", "a@b@"];
    const HOSTS: &[&str] = &[
        "h", "h.x", "H.X", "[::1]", "[::1", "", "h_x", "h x", "h*", "h:", "h::", "h:80", "h:443", "h:0", "h:65536",
        "h:08080", "h:x", "h:00443", "h:000", "..", ".", "a..b", "h%41",
    ];
    const PATHS: &[&str] = &[
        "/", "//", "/a", "/./", "/../", "/..", "/.", "/%0a", "/%0A", "/%2e%2e", "/%zz", "/%", "/%00", "/a b", "/%25",
        "/%7e", "/%e9", "?q", "?%0a", "#f", "#%0a", "", "/a/../b", "/a/./b", "/%2E", "/%2f", "/a%0", "\\n", "\\t",
        "/\\u{e9}",
    ];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out = Vec::new();
    for idx in 0..count {
        let mut url = String::new();
        url.push_str(rng.pick(SCHEMES));
        url.push_str(rng.pick(SEPARATORS));
        url.push_str(rng.pick(USERS));
        url.push_str(rng.pick(HOSTS));
        for _ in 0..rng.next() % 4 {
            url.push_str(rng.pick(PATHS));
        }
        if has_dot_segment_and_bytes_to_escape(&url) {
            continue;
        }
        let content = format!("[submodule \"a\"]\n\turl = {}\n", quoted(&url));
        out.push(pack(
            base,
            &format!("gitmodules-random-url-{idx}"),
            b".gitmodules",
            "100644",
            content.as_bytes(),
            idx % 2 == 0,
        ));
    }
    out
}
