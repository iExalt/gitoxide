//! The native `store` helper against `git credential-store`, which is the oracle: both get the same files and the same
//! sequence of actions, after which their answers and files have to be identical.
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
};

use gix_credentials::{
    Program, helper,
    program::Environment,
    protocol::{Context, ContextOptions},
};
use gix_testtools::{TestResult, tempfile::TempDir};

/// One side of the comparison, with a home directory of its own and `files` as the initial content of its credential files.
struct Side {
    dir: TempDir,
    helper_definition: String,
}

impl Side {
    fn new(files: &[(&str, &str)], helper_arguments: &str) -> TestResult<Self> {
        let dir = gix_testtools::tempfile::tempdir()?;
        for (name, content) in files {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().expect("files are within the home directory"))?;
            std::fs::write(path, content)?;
        }
        Ok(Side {
            dir,
            helper_definition: format!("store{helper_arguments}"),
        })
    }

    fn home(&self) -> &Path {
        self.dir.path()
    }

    /// Where `credential_file` is, as the test refers to it relative to the home directory.
    fn path_of(&self, credential_file: &str) -> PathBuf {
        self.home().join(credential_file)
    }

    /// The program as the configuration `credential.helper = <definition>` would create it, with `home` being the home directory,
    /// and with `{home}` in the definition standing for it.
    fn native(&self) -> TestResult<Program> {
        let definition = self
            .helper_definition
            .replace("{home}", &self.home().display().to_string());
        Ok(
            Program::from_custom_definition(definition)?.with_environment(Environment {
                home: Some(self.home().to_owned()),
                xdg_config_home: Some(self.home().join("xdg")),
                ..Default::default()
            }),
        )
    }

    /// Run `git credential-store` through a shell, like `git` does for a helper that is configured with arguments, and return what it printed.
    fn git(&self, action: &str, input: &str) -> TestResult<Vec<u8>> {
        let definition = self
            .helper_definition
            .replace("{home}", &self.home().display().to_string());
        let arguments = definition.strip_prefix("store").expect("always a store definition");
        // Take the isolated environment of Git commands, but have a shell expand what Git's helper arguments would be.
        let isolated = gix_testtools::git_command(self.home());
        let mut cmd = std::process::Command::new("sh");
        cmd.env_clear();
        for (name, value) in isolated.get_envs() {
            if let Some(value) = value {
                cmd.env(name, value);
            }
        }
        cmd.env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.home().join("xdg"))
            .current_dir(self.home())
            .arg("-c")
            .arg(format!("git credential-store{arguments} {action}"));
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child.stdin.take().expect("configured").write_all(input.as_bytes())?;
        let out = child.wait_with_output()?;
        assert!(
            out.status.success(),
            "git credential-store {action} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(out.stdout)
    }
}

/// Return the content and permissions of all of `files`, or `None` for those that don't exist.
/// The name of a file with its content and permissions, if it exists.
type FileState = (String, Option<(Vec<u8>, u32)>);

fn snapshot(side: &Side, files: &[&str]) -> Vec<FileState> {
    files
        .iter()
        .map(|file| {
            let path = side.path_of(file);
            let state = std::fs::read(&path).ok().map(|content| {
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::metadata(&path).expect("exists").permissions().mode() & 0o777
                };
                #[cfg(not(unix))]
                let mode = 0;
                (content, mode)
            });
            ((*file).to_owned(), state)
        })
        .collect()
}

/// Run each of `actions` as `(action, input)` on a fresh Git side and a fresh native side that both start with `files`, and
/// assert that they produce the same answers and the same files, including their permissions.
/// The first of `files` is where we expect to see changes; `observed` are all the files to compare.
fn differential(
    files: &[(&str, &str)],
    helper_arguments: &str,
    observed: &[&str],
    actions: &[(&str, &str)],
) -> TestResult<Vec<Option<(String, String)>>> {
    let (git, native) = (Side::new(files, helper_arguments)?, Side::new(files, helper_arguments)?);
    let mut program = native.native()?;
    let mut answers = Vec::new();
    for (action, input) in actions {
        let git_out = git.git(action, input)?;
        let context = Context::from_bytes(
            input.as_bytes(),
            ContextOptions {
                protect_protocol: false,
            },
        )?;
        let native_answer = match *action {
            "get" => helper::invoke(&mut program, &helper::Action::Get(context))?.map(|outcome| {
                (
                    outcome.username.unwrap_or_default(),
                    outcome.password.unwrap_or_default(),
                )
            }),
            "store" => {
                helper::invoke(&mut program, &helper::Action::Store((*input).into()))?;
                None
            }
            "erase" => {
                helper::invoke(&mut program, &helper::Action::Erase((*input).into()))?;
                None
            }
            _ => unreachable!("only known actions are used"),
        };
        if *action == "get" {
            let from_git = Context::from_bytes(
                &git_out,
                ContextOptions {
                    protect_protocol: false,
                },
            )?;
            assert_eq!(
                (
                    from_git.username.unwrap_or_default(),
                    from_git.password.unwrap_or_default()
                ),
                native_answer.clone().unwrap_or_default(),
                "{action} {input:?}: both find the same credential, or none"
            );
            answers.push(native_answer.filter(|(user, pass)| !(user.is_empty() && pass.is_empty())));
        } else {
            assert!(git_out.is_empty(), "only get prints anything");
        }
        let (git_state, native_state) = (snapshot(&git, observed), snapshot(&native, observed));
        assert_eq!(
            git_state, native_state,
            "after {action} {input:?}: files and their permissions are the same as the ones git writes"
        );
    }
    let leftovers = |side: &Side| -> Vec<PathBuf> {
        observed
            .iter()
            .map(|file| side.path_of(&format!("{file}.lock")))
            .filter(|lock| lock.exists())
            .collect()
    };
    assert!(leftovers(&native).is_empty(), "lock files are removed");
    Ok(answers)
}

const DEFAULT: &str = ".git-credentials";
const XDG: &str = "xdg/git/credentials";
const ALL: &[&str] = &[DEFAULT, XDG];

#[test]
fn store_creates_the_first_default_file_and_get_finds_it() -> TestResult {
    let answers = differential(
        &[],
        "",
        ALL,
        &[
            (
                "store",
                "protocol=https\nhost=example.com\nusername=user\npassword=secret\n",
            ),
            ("get", "protocol=https\nhost=example.com\n"),
            ("get", "protocol=https\nhost=example.com\nusername=user\n"),
            ("get", "protocol=https\nhost=example.com\nusername=other\n"),
            ("get", "protocol=http\nhost=example.com\n"),
            ("get", "protocol=https\nhost=other.com\n"),
            ("get", "protocol=https\nhost=example.com\npath=repo.git\n"),
        ],
    )?;
    assert_eq!(
        answers[0],
        Some(("user".into(), "secret".into())),
        "a credential without a path matches requests that have one, as in git, but not the other way around"
    );
    assert_eq!(answers[1], Some(("user".into(), "secret".into())));
    assert_eq!(answers[2], None, "another username has no match");
    assert_eq!(answers[3], None);
    assert_eq!(answers[4], None);
    Ok(())
}

#[test]
fn store_replaces_matching_entries_and_keeps_others() -> TestResult {
    differential(
        &[(
            DEFAULT,
            "https://old:pw@example.com\nhttps://keep:pw@example.com\nhttps://other:pw@example.org/path\n",
        )],
        "",
        ALL,
        &[
            (
                "store",
                "protocol=https\nhost=example.com\nusername=old\npassword=new\n",
            ),
            (
                "store",
                "protocol=https\nhost=example.org\npath=path\nusername=other\npassword=changed\n",
            ),
            (
                "store",
                "protocol=https\nhost=example.org\nusername=nopath\npassword=pw\n",
            ),
        ],
    )?;
    Ok(())
}

#[test]
fn values_are_percent_encoded_like_git_does() -> TestResult {
    let answers = differential(
        &[],
        "",
        ALL,
        &[
            (
                "store",
                "protocol=https\nhost=example.com:8080\nusername=us er@x\npassword=p:w/d%?#[]!é\npath=a b/c;d?e\n",
            ),
            ("get", "protocol=https\nhost=example.com:8080\npath=a b/c;d?e\n"),
            ("get", "url=https://example.com:8080/a%20b/c;d?e\n"),
            ("store", "protocol=ssh\nhost=[::1]:22\nusername=git\npassword=pw\n"),
            ("get", "protocol=ssh\nhost=[::1]:22\n"),
        ],
    )?;
    assert_eq!(answers[0], Some(("us er@x".into(), "p:w/d%?#[]!é".into())));
    Ok(())
}

#[test]
fn incomplete_credentials_are_not_stored() -> TestResult {
    differential(
        &[],
        "",
        ALL,
        &[
            ("store", "protocol=https\nhost=example.com\nusername=user\n"),
            ("store", "protocol=https\nhost=example.com\npassword=pw\n"),
            ("store", "host=example.com\nusername=user\npassword=pw\n"),
            ("store", "protocol=https\nusername=user\npassword=pw\n"),
            ("store", "protocol=file\npath=/some/repo\nusername=user\npassword=pw\n"),
        ],
    )?;
    Ok(())
}

#[test]
fn erase_removes_matching_entries_from_all_files_and_requires_a_pattern() -> TestResult {
    differential(
        &[
            (DEFAULT, "https://a:one@example.com\nhttps://b:two@example.com\n"),
            (XDG, "https://a:one@example.com\nhttps://a:three@example.com\n"),
        ],
        "",
        ALL,
        &[
            ("erase", "protocol=https\nhost=example.com\nusername=a\npassword=one\n"),
            ("erase", "password=two\n"),
            ("erase", "protocol=https\nhost=example.com\nusername=b\n"),
            (
                "erase",
                "protocol=https\nhost=example.com\nusername=a\npassword=wrong\n",
            ),
        ],
    )?;
    Ok(())
}

#[test]
fn get_consults_files_in_order_and_store_goes_to_the_first_existing_one() -> TestResult {
    let answers = differential(
        &[(XDG, "https://xdg:pw@example.com\n")],
        "",
        ALL,
        &[
            ("get", "protocol=https\nhost=example.com\n"),
            ("store", "protocol=https\nhost=example.com\nusername=new\npassword=pw\n"),
            ("get", "protocol=https\nhost=example.com\n"),
        ],
    )?;
    assert_eq!(
        answers[0],
        Some(("xdg".into(), "pw".into())),
        "the second file is used if the first is missing"
    );
    Ok(())
}

#[test]
fn first_existing_file_wins_for_get_and_gets_the_new_entries() -> TestResult {
    differential(
        &[
            (DEFAULT, "https://first:pw@example.com\n"),
            (XDG, "https://second:pw@example.com\nhttps://second:pw@example.org\n"),
        ],
        "",
        ALL,
        &[
            ("get", "protocol=https\nhost=example.com\n"),
            ("get", "protocol=https\nhost=example.org\n"),
            ("store", "protocol=https\nhost=example.net\nusername=n\npassword=pw\n"),
        ],
    )?;
    Ok(())
}

#[test]
fn explicit_file_is_the_only_one_used() -> TestResult {
    differential(
        &[
            (DEFAULT, "https://default:pw@example.com\n"),
            ("custom/store", "https://custom:pw@example.com\n"),
        ],
        " --file {home}/custom/store",
        &[DEFAULT, "custom/store"],
        &[
            ("get", "protocol=https\nhost=example.com\n"),
            ("store", "protocol=https\nhost=example.org\nusername=u\npassword=pw\n"),
            ("erase", "protocol=https\nhost=example.com\nusername=custom\n"),
        ],
    )?;
    differential(
        &[],
        " --file={home}/created",
        &["created", DEFAULT],
        &[("store", "protocol=https\nhost=example.org\nusername=u\npassword=pw\n")],
    )?;
    Ok(())
}

#[test]
fn unusual_lines_are_kept_and_skipped_like_git_does() -> TestResult {
    let content = "u:pw@example.com\n://v:pw@example.com\nhttps://w:pw@/path\n# a comment\n\nnot a url\nhttps://nopass@example.com\nhttps://:emptyuser@example.com\nhttps://u:p@example.com/a/b//\nhttps://user:pw%0a@example.com\n  https://spaces:pw@example.com\nhttps://last:pw@example.com";
    let answers = differential(
        &[(DEFAULT, content)],
        "",
        ALL,
        &[
            ("get", "protocol=https\nhost=example.com\nusername=nopass\n"),
            ("get", "protocol=https\nhost=example.com\nusername=\n"),
            ("get", "protocol=https\nhost=example.com\npath=a/b\n"),
            ("get", "protocol=https\nhost=example.com\nusername=last\n"),
            ("get", "protocol=https\nhost=example.com\nusername=spaces\n"),
            ("get", "host=example.com\nusername=u\n"),
            ("get", "path=path\nusername=w\n"),
            ("erase", "host=example.com\nusername=v\n"),
            ("store", "protocol=https\nhost=example.org\nusername=u\npassword=pw\n"),
            ("erase", "protocol=https\nhost=example.com\nusername=last\n"),
        ],
    )?;
    assert_eq!(
        answers[2],
        Some(("u".into(), "p".into())),
        "trailing slashes of paths are trimmed"
    );
    Ok(())
}

#[test]
fn permissions_of_created_and_replaced_files_are_private() -> TestResult {
    differential(
        &[],
        "",
        ALL,
        &[("store", "protocol=https\nhost=example.com\nusername=u\npassword=pw\n")],
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let side = Side::new(&[(DEFAULT, "https://u:p@example.com\n")], "")?;
        std::fs::set_permissions(side.path_of(DEFAULT), std::fs::Permissions::from_mode(0o644))?;
        helper::invoke(
            &mut side.native()?,
            &helper::Action::Store("protocol=https\nhost=example.org\nusername=u\npassword=pw\n".into()),
        )?;
        assert_eq!(
            std::fs::metadata(side.path_of(DEFAULT))?.permissions().mode() & 0o777,
            0o600,
            "like git with its umask, a rewritten file becomes private"
        );
    }
    Ok(())
}

#[test]
fn symlinked_credential_files_are_updated_in_place() -> TestResult {
    #[cfg(unix)]
    {
        let side = Side::new(&[("target/credentials", "https://u:p@example.com\n")], "")?;
        std::os::unix::fs::symlink(side.path_of("target/credentials"), side.path_of(DEFAULT))?;
        helper::invoke(
            &mut side.native()?,
            &helper::Action::Store("protocol=https\nhost=example.org\nusername=a\npassword=b\n".into()),
        )?;
        assert!(side.path_of(DEFAULT).is_symlink(), "the link is kept");
        assert_eq!(
            std::fs::read_to_string(side.path_of("target/credentials"))?,
            "https://a:b@example.org\nhttps://u:p@example.com\n"
        );
    }
    Ok(())
}

#[test]
fn a_helper_that_cannot_run_fails_without_stopping_the_cascade() -> TestResult {
    let no_home = Environment {
        home: None,
        xdg_config_home: None,
        ..Default::default()
    };
    let program = Program::from_custom_definition("store")?.with_environment(no_home);
    let mut cascade = helper::Cascade::default().extend([
        program,
        Program::from_custom_definition("!f() { echo username=fallback; echo password=pw; }; f")?,
    ]);
    let outcome = cascade
        .invoke(
            helper::Action::get_for_url("https://example.com/repo"),
            gix_prompt::Options {
                mode: gix_prompt::Mode::Disable,
                ..Default::default()
            },
        )?
        .expect("the second helper provides credentials");
    assert_eq!(outcome.identity.username, "fallback");
    Ok(())
}

#[test]
fn file_arguments_are_split_like_a_shell_does() -> TestResult {
    for (arguments, home_relative_file) in [
        (" --file ~/in-home", "in-home"),
        (" --file '{home}/dir with space/store'", "dir with space/store"),
        (" --file \"{home}/dir with space/store\"", "dir with space/store"),
        (" --fi {home}/abbreviated", "abbreviated"),
        (" --file={home}/inline", "inline"),
        (" --file {home}/dir\\ with\\ space/store", "dir with space/store"),
    ] {
        let files: &[(&str, &str)] = &[("dir with space/store", "https://pre:existing@example.com\n")];
        let observed = [home_relative_file, DEFAULT];
        let observed: Vec<&str> = observed.into_iter().filter(|file| !file.is_empty()).collect();
        let answers = differential(
            files,
            arguments,
            &observed,
            &[
                ("store", "protocol=https\nhost=example.org\nusername=u\npassword=pw\n"),
                ("get", "protocol=https\nhost=example.org\n"),
            ],
        )
        .map_err(|err| format!("{arguments:?}: {err:?}"))?;
        assert_eq!(answers[0], Some(("u".into(), "pw".into())), "{arguments:?}");
    }
    Ok(())
}

#[test]
fn arguments_that_a_shell_would_expand_or_that_are_unknown_make_the_helper_fail_without_running_git() -> TestResult {
    let side = Side::new(&[], "")?;
    for arguments in [
        " --file $HOME/x",
        " --file `echo x`",
        " --file ~other/x",
        " --unknown",
        " --file",
        " --file a b",
        " --file *",
    ] {
        let mut program = Program::from_custom_definition(format!("store{arguments}"))?.with_environment(Environment {
            home: Some(side.home().to_owned()),
            xdg_config_home: None,
            path: Some("".into()),
            ..Default::default()
        });
        assert!(
            matches!(program.kind, gix_credentials::program::Kind::Store { .. }),
            "it is always the native helper, even if it can't do what is asked: {arguments:?}"
        );
        let err = helper::invoke(&mut program, &helper::Action::get_for_url("https://example.com"))
            .expect_err("the helper fails");
        assert!(
            format!("{err:?}").contains("Credentials helper failed"),
            "{arguments:?}: a failure of the helper that lets the cascade continue: {err:?}"
        );
    }
    Ok(())
}
