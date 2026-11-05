use bstr::ByteSlice;
use gix_validate::dotfile::{DotFile, is_hfs, is_ntfs};

fn ntfs(file: DotFile, name: &str) -> bool {
    is_ntfs(file, name.as_bytes().as_bstr())
}

fn hfs(file: DotFile, name: &[u8]) -> bool {
    is_hfs(file, name.as_bstr())
}

#[test]
fn ntfs_dotgit() {
    for name in [
        ".git",
        ".GIT",
        ".GiT",
        ".git ",
        ".git.",
        ".git . . .",
        ".git:stream",
        ".git::$INDEX_ALLOCATION",
        ".git/",
        ".git\\",
        ".git/x",
        ".git\\x",
        ".git .\\x",
        "git~1",
        "GIT~1",
        "Git~1.",
        "git~1 ",
        "git~1:x",
    ] {
        assert!(ntfs(DotFile::Git, name), "{name:?} is .git");
    }
    for name in [
        "",
        ".",
        ".g",
        ".gi",
        ".gitx",
        ".git x",
        ".git.x",
        "git",
        "git~",
        "git~2",
        "git~11",
        "gitx",
        "git~1x",
        ".gi t",
        "x.git",
        "a.git",
        ".git\u{200c}",
    ] {
        assert!(!ntfs(DotFile::Git, name), "{name:?} isn't .git");
    }
}

#[test]
fn hfs_dotgit() {
    for name in [
        ".git".as_bytes(),
        b".GIT",
        b".GiT",
        b".git/",
        b".git/x",
        ".gi\u{200c}t".as_bytes(),
        "\u{200c}.git".as_bytes(),
        ".git\u{200d}".as_bytes(),
        ".g\u{200e}\u{200f}it".as_bytes(),
        ".git\u{feff}".as_bytes(),
        "\u{202a}.\u{202b}g\u{202c}i\u{202d}t\u{202e}".as_bytes(),
        ".\u{206a}git\u{206f}".as_bytes(),
        // Malformed UTF-8 after the name ends it, which is how Git sees it.
        b".git\xff",
        b".git\xc0",
        b".git\xc3",
        b".git\xe2\x80",
        b".GIT\xf0\x80\x80\x80",
        b".g\xe2\x80\x8cit\xff",
    ] {
        assert!(hfs(DotFile::Git, name), "{:?} is .git", name.as_bstr());
    }
    for name in [
        "".as_bytes(),
        b".",
        b".gi",
        b".gitx",
        b".git ",
        b"git",
        b".git.",
        b".gi\xc0t",
        b".gi\xe2\x80t",
        b".\xffgit",
        b"\xff.git",
        ".gi\u{2010}t".as_bytes(),
        ".gİt".as_bytes(),
        b"x.git",
    ] {
        assert!(!hfs(DotFile::Git, name), "{:?} isn't .git", name.as_bstr());
    }
}

#[test]
fn ntfs_special_files_have_short_names_and_fall_back_names() {
    for (file, regular, short, fallback) in [
        (DotFile::Gitmodules, ".gitmodules", "GITMOD~", "GI7EBA~"),
        (DotFile::Gitattributes, ".gitattributes", "GITATT~", "GI7D29~"),
        (DotFile::Gitignore, ".gitignore", "GITIGN~", "GI250A~"),
        (DotFile::Mailmap, ".mailmap", "MAILMA~", "MABA30~"),
    ] {
        for name in [
            regular.to_string(),
            regular.to_uppercase(),
            format!("{regular} "),
            format!("{regular}. ."),
            format!("{regular}:stream"),
            format!("{short}1"),
            format!("{short}4"),
            format!("{}1", short.to_lowercase()),
            format!("{short}1 "),
            format!("{short}2.."),
            format!("{fallback}1"),
            format!("{}9", fallback.to_lowercase()),
        ] {
            assert!(ntfs(file, &name), "{name:?} is {file:?}");
        }
        for name in [
            String::new(),
            format!("{regular}x"),
            format!("{regular}/"),
            format!("{short}5"),
            format!("{short}0"),
            short.to_string(),
            format!("{short}12"),
            format!("{fallback}0"),
            fallback.to_string(),
            format!("{fallback}1x"),
            format!("x{regular}"),
            regular[1..].to_string(),
        ] {
            assert!(!ntfs(file, &name), "{name:?} isn't {file:?}");
        }
    }
}

#[test]
fn ntfs_fall_back_names_are_eight_characters_long() {
    assert!(
        ntfs(DotFile::Gitmodules, "gi7e~123"),
        "the number can take the remaining characters"
    );
    assert!(
        ntfs(DotFile::Gitmodules, "gi7e~123 "),
        "which can be followed by spaces"
    );
    assert!(!ntfs(DotFile::Gitmodules, "gi7e~1234"), "but not more");
    assert!(
        !ntfs(DotFile::Gitmodules, "gi7eba~12"),
        "also if the prefix is complete"
    );
    assert!(!ntfs(DotFile::Gitmodules, "gi7e~1"), "and shorter names aren't known");
    assert!(
        !ntfs(DotFile::Gitmodules, "gi7ebax~1"),
        "the tilde has to be within the first eight characters"
    );
    assert!(!ntfs(DotFile::Gitmodules, "gi7eb\u{e9}~1"), "non-ascii isn't matched");
}

#[test]
fn hfs_special_files() {
    for (file, regular) in [
        (DotFile::Gitmodules, ".gitmodules"),
        (DotFile::Gitattributes, ".gitattributes"),
        (DotFile::Gitignore, ".gitignore"),
        (DotFile::Mailmap, ".mailmap"),
    ] {
        for name in [
            regular.to_string(),
            regular.to_uppercase(),
            format!("{regular}/x"),
            format!("{}\u{200c}{}", &regular[..3], &regular[3..]),
            format!("{regular}\u{feff}"),
        ] {
            assert!(hfs(file, name.as_bytes()), "{name:?} is {file:?}");
        }
        for name in [
            String::new(),
            format!("{regular}x"),
            format!("{regular} "),
            regular[1..].to_string(),
        ] {
            assert!(!hfs(file, name.as_bytes()), "{name:?} isn't {file:?}");
        }
    }
    for (file, name) in [
        (DotFile::Gitmodules, ".gitmodules"),
        (DotFile::Gitattributes, ".gitattributes"),
        (DotFile::Gitignore, ".gitignore"),
        (DotFile::Mailmap, ".mailmap"),
    ] {
        assert!(
            hfs(file, &[name.as_bytes(), b"\xff"].concat()),
            "invalid UTF-8 after the name ends it like the end of the string does: {name}"
        );
    }
    assert!(!hfs(DotFile::Gitmodules, b".git\xc0modules"), "but not within it");
    assert!(
        !hfs(DotFile::Gitmodules, b".gitmodule\xff"),
        "and not before the name is complete"
    );
}
