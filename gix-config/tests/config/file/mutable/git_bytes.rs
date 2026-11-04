//! The bytes that `git config` leaves in a file are what a consumer that edits configuration with this crate has to
//! produce as well, so that the file stays one that people and Git edit alike.
use gix_config::File;
use gix_testtools::TestResult;

/// What `git config --file` leaves behind after `args`, starting out with `initial`.
fn git(initial: &str, args: &[&str]) -> TestResult<String> {
    let dir = gix_testtools::tempfile::tempdir()?;
    std::fs::write(dir.path().join("config"), initial)?;
    let output = gix_testtools::git_command(dir.path())
        .args(["config", "--file", "config"])
        .args(args)
        .output()?;
    assert!(
        output.status.success(),
        "git config {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(std::fs::read_to_string(dir.path().join("config"))?)
}

/// What this crate leaves behind after `edit`, starting out with `initial`.
fn gix(initial: &str, edit: impl FnOnce(&mut File) -> TestResult) -> TestResult<String> {
    let mut file = File::try_from(initial)?;
    edit(&mut file)?;
    Ok(file.to_bstring().to_string())
}

fn set(file: &mut File, key: &str, value: &str) -> TestResult {
    file.set_raw_value(key, value)?;
    Ok(())
}

fn set_by(file: &mut File, section: &str, subsection: Option<&str>, key: &str, value: &str) -> TestResult {
    file.set_raw_value_by(section, subsection.map(bstr::BStr::new), key, value)?;
    Ok(())
}

type Edit = fn(&mut File) -> TestResult;

struct Case {
    name: &'static str,
    initial: &'static str,
    git: &'static [&'static str],
    gix: Edit,
}

#[test]
fn edits_leave_the_bytes_that_git_leaves() -> TestResult {
    let cases = [
        Case {
            name: "set a new key in a new file",
            initial: "",
            git: &["core.abbrev", "12"],
            gix: |f| set(f, "core.abbrev", "12"),
        },
        Case {
            name: "set a new key in an existing section",
            initial: "[core]\n\tbare = false\n",
            git: &["core.abbrev", "12"],
            gix: |f| set(f, "core.abbrev", "12"),
        },
        Case {
            name: "replace the value of an existing key",
            initial: "[core]\n\tabbrev = 7\n\tbare = false\n",
            git: &["core.abbrev", "12"],
            gix: |f| set(f, "core.abbrev", "12"),
        },
        Case {
            name: "set a key in a new subsection",
            initial: "[core]\n\tbare = false\n",
            git: &["remote.origin.url", "https://example.com/repo.git"],
            gix: |f| set(f, "remote.origin.url", "https://example.com/repo.git"),
        },
        Case {
            name: "set a key in a subsection with characters that need escaping",
            initial: "",
            git: &["branch.a\"b\\c.remote", "origin"],
            gix: |f| set_by(f, "branch", Some("a\"b\\c"), "remote", "origin"),
        },
        Case {
            name: "value with a space inside",
            initial: "",
            git: &["user.name", "Some One"],
            gix: |f| set(f, "user.name", "Some One"),
        },
        Case {
            name: "value with leading and trailing space",
            initial: "",
            git: &["user.name", " padded "],
            gix: |f| set(f, "user.name", " padded "),
        },
        Case {
            name: "value with a hash and a semicolon",
            initial: "",
            git: &["user.name", "a # b ; c"],
            gix: |f| set(f, "user.name", "a # b ; c"),
        },
        Case {
            name: "value with a backslash and a double quote",
            initial: "",
            git: &["user.name", "a\\b\"c"],
            gix: |f| set(f, "user.name", "a\\b\"c"),
        },
        Case {
            name: "value with a tab and a newline",
            initial: "",
            git: &["user.name", "a\tb\nc"],
            gix: |f| set(f, "user.name", "a\tb\nc"),
        },
        Case {
            name: "empty value",
            initial: "",
            git: &["user.name", ""],
            gix: |f| set(f, "user.name", ""),
        },
        Case {
            name: "add a second value to a multi-valued key",
            initial: "[remote \"origin\"]\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
            git: &["--add", "remote.origin.fetch", "+refs/tags/*:refs/tags/*"],
            gix: |f| {
                f.section_mut_by_key("remote.origin")
                    .expect("the section exists")
                    .push("fetch", Some("+refs/tags/*:refs/tags/*".into()))?;
                Ok(())
            },
        },
        Case {
            name: "unset a key between two others",
            initial: "[core]\n\tbare = false\n\tabbrev = 7\n\tfilemode = true\n",
            git: &["--unset", "core.abbrev"],
            gix: |f| {
                f.section_mut_by_key("core")
                    .expect("the section exists")
                    .remove("abbrev");
                Ok(())
            },
        },
        Case {
            name: "set a key in a section with a differently cased name",
            initial: "[CORE]\n\tbare = false\n",
            git: &["core.abbrev", "12"],
            gix: |f| set(f, "core.abbrev", "12"),
        },
        Case {
            name: "set a new key with two sections of the same name",
            initial: "[core]\n\tbare = false\n[user]\n\tname = n\n[core]\n\tfilemode = true\n",
            git: &["core.abbrev", "12"],
            gix: |f| set(f, "core.abbrev", "12"),
        },
        Case {
            name: "remove a section",
            initial: "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = x\n[user]\n\tname = n\n",
            git: &["--remove-section", "remote.origin"],
            gix: |f| {
                f.remove_section("remote", Some("origin".into()));
                Ok(())
            },
        },
        Case {
            name: "rename a section",
            initial: "[branch \"a\"]\n\tremote = origin\n\tmerge = refs/heads/a\n",
            git: &["--rename-section", "branch.a", "branch.b"],
            gix: |f| Ok(f.rename_section("branch", Some("a".into()), "branch", Some(bstr::BString::from("b")))?),
        },
    ];

    let mut divergences = Vec::new();
    for case in cases {
        let expected = git(case.initial, case.git)?;
        let actual = gix(case.initial, case.gix)?;
        if expected != actual {
            divergences.push(format!("{}:\n--- git\n{expected}--- gix\n{actual}---", case.name));
        }
    }
    assert!(
        divergences.is_empty(),
        "{} cases differ from Git:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
    Ok(())
}

/// Where this crate writes differently from Git, and what it leaves instead. These are pinned so that a change of them is
/// noticed, not because they are wanted.
#[test]
fn known_differences_to_git() -> TestResult {
    // Git removes a section that has no entries left (it keeps it if a comment line remains in it, and drops a comment on
    // its header with it), while `raw_values_mut().delete_all()` leaves the header and the line that held the value.
    let initial = "[core]\n\tbare = false\n\tabbrev = 7\n\tfilemode = true\n";
    assert_eq!(
        git(initial, &["--unset", "core.abbrev"])?,
        "[core]\n\tbare = false\n\tfilemode = true\n"
    );
    assert_eq!(
        gix(initial, |f| {
            f.raw_values_mut("core.abbrev")?.delete_all();
            Ok(())
        })?,
        "[core]\n\tbare = false\n\t\n\tfilemode = true\n",
        "deleting leaves the indentation of the line that held the value, and its newline"
    );
    assert_eq!(git("[core]\n\tabbrev = 7\n", &["--unset", "core.abbrev"])?, "");
    assert_eq!(
        gix("[core]\n\tabbrev = 7\n", |f| {
            f.section_mut_by_key("core").expect("exists").remove("abbrev");
            Ok(())
        })?,
        "[core]\n",
        "the header stays when the last value is removed"
    );

    // Git adds a key behind the last one of the section with a tab, this crate follows the layout of the section.
    let initial = "# top\n[core] # side\n  bare=false ; trailing\n\n[user]\n\tname = n\n";
    assert_eq!(
        git(initial, &["core.abbrev", "12"])?,
        "# top\n[core] # side\n  bare=false ; trailing\n\tabbrev = 12\n\n[user]\n\tname = n\n"
    );
    assert_eq!(
        gix(initial, |f| set(f, "core.abbrev", "12"))?,
        "# top\n[core] # side\n  bare=false ; trailing\n\n  abbrev=12\n[user]\n\tname = n\n",
        "the new key is placed after the empty line, with the spacing of the key before"
    );

    // Git adds the newline that the last line of a file lacks and indents the key like the others. This crate moves the
    // indentation of the new key in front of the newline of the line before, which leaves it with trailing whitespace and the
    // new key without indentation, though both are read the same.
    let initial = "[core]\n\tbare = false";
    assert_eq!(
        git(initial, &["core.abbrev", "12"])?,
        "[core]\n\tbare = false\n\tabbrev = 12\n"
    );
    assert_eq!(
        gix(initial, |f| set(f, "core.abbrev", "12"))?,
        "[core]\n\tbare = false\t\nabbrev = 12\n",
        "the layout of a file without a trailing newline is off"
    );

    // Git writes the key as it is spelled in the command, this crate keeps what the file has.
    let initial = "[core]\n\tAbbrev = 7\n";
    assert_eq!(git(initial, &["core.abbrev", "12"])?, "[core]\n\tabbrev = 12\n");
    assert_eq!(
        gix(initial, |f| set(f, "core.abbrev", "12"))?,
        "[core]\n\tAbbrev = 12\n",
        "the spelling of an existing key is kept"
    );

    // Git replaces the value where it is. This crate sets it in the last section of that name, as documented, so a value in
    // an earlier section is shadowed instead of replaced. The value that is read is the one that was set in both cases.
    let initial = "[core]\n\tabbrev = 7\n[user]\n\tname = n\n[core]\n\tfilemode = true\n";
    assert_eq!(
        git(initial, &["core.abbrev", "12"])?,
        "[core]\n\tabbrev = 12\n[user]\n\tname = n\n[core]\n\tfilemode = true\n"
    );
    assert_eq!(
        gix(initial, |f| set(f, "core.abbrev", "12"))?,
        "[core]\n\tabbrev = 7\n[user]\n\tname = n\n[core]\n\tfilemode = true\n\tabbrev = 12\n"
    );
    Ok(())
}
