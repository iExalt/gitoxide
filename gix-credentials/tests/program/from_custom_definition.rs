use gix_credentials::{
    Program, helper,
    program::{Environment, Kind},
};
use gix_testtools::TestResult;
use std::{path::PathBuf, sync::LazyLock};

/// Create a directory containing an executable `git-credential-<name>`, and an environment that finds it only there.
fn environment_with_helper(name: &str) -> TestResult<(gix_testtools::tempfile::TempDir, Environment, PathBuf)> {
    let dir = gix_testtools::tempfile::tempdir()?;
    let helper = dir
        .path()
        .join(format!("git-credential-{name}{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&helper, "#!/bin/sh\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))?;
    }
    let environment = Environment {
        exec_path: vec![dir.path().to_owned()],
        path: Some("".into()),
        ..Default::default()
    };
    Ok((dir, environment, helper))
}

static SH: LazyLock<&'static str> = LazyLock::new(|| {
    gix_path::env::shell()
        .to_str()
        .expect("some `from_custom_definition` tests must be run where 'sh' path is valid Unicode")
});

// The basename of the default shell, used as the `command_name` operand after
// `-c <script>` and observable inside the shell as `$0`. The default shell is
// `gix_path::env::shell()`, documented as `/bin/sh` on Unix and a path ending
// in `sh.exe` on Windows.
const SH_BASENAME: &str = if cfg!(windows) { "sh.exe" } else { "sh" };

#[test]
fn empty() -> TestResult {
    let prog = Program::from_custom_definition("")?;
    assert!(matches!(&prog.kind, Kind::ExternalName { name_and_args } if name_and_args.is_empty()));
    let err = prog
        .with_environment(Environment::default())
        .to_command(&helper::Action::Store("egal".into()))
        .expect_err("not useful, but allowed, and it can't be found");
    assert!(
        err.is_not_found(),
        "the lack of a name is just a helper that isn't there"
    );
    Ok(())
}

#[test]
fn name_that_cannot_be_found_is_not_found_and_never_runs_git() -> TestResult {
    let prog = Program::from_custom_definition("missing --arg")?.with_environment(Environment {
        path: Some("".into()),
        ..Default::default()
    });
    let err = prog
        .to_command(&helper::Action::Store("egal".into()))
        .expect_err("there is no `git-credential-missing`");
    assert!(err.is_not_found(), "{err:?}");
    assert!(
        err.to_string().contains("git-credential-missing"),
        "the diagnostic says what was looked for: {err}"
    );
    Ok(())
}

#[test]
fn names_are_not_paths() -> TestResult {
    let (_dir, environment, _helper) = environment_with_helper("name")?;
    for input in ["../name", "sub/name"] {
        let err = Program::from_custom_definition(input)?
            .with_environment(environment.clone())
            .to_command(&helper::Action::Store("egal".into()))
            .expect_err("only plain names are looked up");
        assert!(err.is_not_found(), "{input}: {err:?}");
    }
    Ok(())
}

#[test]
fn simple_script_in_path() -> TestResult {
    let prog = Program::from_custom_definition("!exe")?;
    assert!(matches!(&prog.kind, Kind::ExternalShellScript(script) if script == "exe"));
    let cmd = prog.to_command(&helper::Action::Store("egal".into()))?;
    assert_eq!(
        cmd.get_program(),
        std::process::Command::try_from(gix_command::prepare("exe"))?.get_program(),
        "it didn't detect anything shell-scripty, and thus doesn't use a shell"
    );
    assert_eq!(
        cmd.get_args().collect::<Vec<_>>(),
        ["store"],
        "the helper receives its action"
    );
    Ok(())
}

#[test]
fn name_with_args() -> TestResult {
    let input = "name --arg --bar=\"a b\"";
    let (_dir, environment, helper) = environment_with_helper("name")?;
    let prog = Program::from_custom_definition(input)?.with_environment(environment);
    assert!(matches!(&prog.kind, Kind::ExternalName{name_and_args} if name_and_args == input));
    let cmd = prog.to_command(&helper::Action::Store("egal".into()))?;
    assert_eq!(
        std::path::Path::new(cmd.get_program()),
        std::path::Path::new(std::process::Command::try_from(gix_command::prepare(&helper))?.get_program()),
        "the helper is found without Git and runs directly"
    );
    assert_eq!(
        cmd.get_args().collect::<Vec<_>>(),
        ["--arg", "--bar=a b", "store"],
        "quoted arguments are split without losing their contents"
    );
    Ok(())
}

#[test]
fn name_with_special_args() -> TestResult {
    let input = "name --arg --bar=~/folder/in/home";
    let (_dir, environment, helper) = environment_with_helper("name")?;
    let prog = Program::from_custom_definition(input)?.with_environment(environment);
    let sh = gix_path::env::shell_command();
    let helper = gix_path::to_unix_separators_on_windows(gix_path::into_bstr(helper.as_path())?);
    let quoted_helper = gix_quote::single(helper.as_ref());
    assert!(matches!(&prog.kind, Kind::ExternalName{name_and_args} if name_and_args == input));
    assert_eq!(
        format!("{:?}", prog.to_command(&helper::Action::Store("egal".into()))?),
        format!(r#"{sh:?} "-c" "{quoted_helper} --arg --bar=~/folder/in/home \"$@\"" "{SH_BASENAME}" "store""#)
    );
    Ok(())
}

#[test]
fn name() -> TestResult {
    let input = "name";
    let (_dir, environment, helper) = environment_with_helper("name")?;
    let prog = Program::from_custom_definition(input)?.with_environment(environment);
    assert!(matches!(&prog.kind, Kind::ExternalName{name_and_args} if name_and_args == input));
    let cmd = prog.to_command(&helper::Action::Store("egal".into()))?;
    assert_eq!(
        std::path::Path::new(cmd.get_program()),
        std::path::Path::new(std::process::Command::try_from(gix_command::prepare(&helper))?.get_program()),
        "we detect that this can run without shell, which is also more portable on windows"
    );
    assert_eq!(
        cmd.get_args().collect::<Vec<_>>(),
        ["store"],
        "the helper receives just its action"
    );
    Ok(())
}

#[test]
fn path_with_args_that_definitely_need_shell() -> TestResult {
    let input = "/abs/name --arg --bar=\"a b\"";
    let prog = Program::from_custom_definition(input)?;
    assert!(matches!(&prog.kind, Kind::ExternalPath{path_and_args} if path_and_args == input));
    let sh = *SH;
    assert_eq!(
        format!("{:?}", prog.to_command(&helper::Action::Store("egal".into()))?),
        if cfg!(windows) {
            r#""/abs/name" "--arg" "--bar=a b" "store""#.to_owned()
        } else {
            format!(r#""{sh}" "-c" "/abs/name --arg --bar=\"a b\" \"$@\"" "{SH_BASENAME}" "store""#)
        }
    );
    Ok(())
}

#[test]
fn path_without_args() -> TestResult {
    let input = "/abs/name";
    let prog = Program::from_custom_definition(input)?;
    assert!(matches!(&prog.kind, Kind::ExternalPath{path_and_args} if path_and_args == input));
    assert_eq!(
        format!("{:?}", prog.to_command(&helper::Action::Store("egal".into()))?),
        r#""/abs/name" "store""#,
        "no shell is used"
    );
    Ok(())
}

#[test]
fn path_with_simple_args() -> TestResult {
    let input = "/abs/name a b";
    let prog = Program::from_custom_definition(input)?;
    assert!(matches!(&prog.kind, Kind::ExternalPath{path_and_args} if path_and_args == input));
    let sh = *SH;
    assert_eq!(
        format!("{:?}", prog.to_command(&helper::Action::Store("egal".into()))?),
        if cfg!(windows) {
            r#""/abs/name" "a" "b" "store""#.to_owned()
        } else {
            format!(r#""{sh}" "-c" "/abs/name a b \"$@\"" "{SH_BASENAME}" "store""#)
        },
        "a shell is used as there are arguments, and it's generally more flexible, but on windows we split ourselves"
    );
    Ok(())
}
