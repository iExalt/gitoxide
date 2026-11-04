//! Prove that credential helpers are found and run without ever invoking `git`.
use gix_credentials::{Program, helper};
use gix_testtools::{Env, TestResult};

#[test]
#[cfg(unix)]
fn named_helpers_including_store_never_run_git() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    let dir = gix_testtools::tempfile::tempdir()?;
    let (shims, helpers, home) = (
        dir.path().join("shims"),
        dir.path().join("helpers"),
        dir.path().join("home"),
    );
    for dir in [&shims, &helpers, &home] {
        std::fs::create_dir(dir)?;
    }
    let log = dir.path().join("git-was-run");
    let executable = |dir: &std::path::Path, name: &str, script: String| -> TestResult {
        let path = dir.join(name);
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    };
    // Anything that runs `git` or one of its helpers by name leaves a trace, as these shadow what could be installed.
    let shim = format!("#!/bin/sh\necho \"$0 $@\" >> '{}'\nexit 1\n", log.display());
    for name in [
        "git",
        "git-credential-store",
        "git-credential-osxkeychain",
        "git-credential-libsecret",
    ] {
        executable(&shims, name, shim.clone())?;
    }
    executable(
        &helpers,
        "git-credential-fake",
        "#!/bin/sh\ncase \"$1\" in get) echo username=fake; echo password=pw;; esac\n".into(),
    )?;
    let _env = Env::new()
        .set(
            "PATH",
            format!("{}:{}:/usr/bin:/bin", shims.display(), helpers.display()),
        )
        .set("HOME", home.display().to_string())
        .unset("GIT_EXEC_PATH")
        .unset("XDG_CONFIG_HOME");

    let mut cascade = helper::Cascade::default().extend([
        Program::from_custom_definition("store")?,
        Program::from_custom_definition("not-installed")?,
        Program::from_custom_definition("fake")?,
    ]);
    let prompt = || gix_prompt::Options {
        mode: gix_prompt::Mode::Disable,
        ..Default::default()
    };
    let outcome = cascade
        .invoke(helper::Action::get_for_url("https://example.com/repo"), prompt())?
        .expect("the helper that exists provides credentials");
    assert_eq!(outcome.identity.username, "fake");
    cascade.invoke(outcome.next.store(), prompt())?;
    assert_eq!(
        std::fs::read_to_string(home.join(".git-credentials"))?,
        "https://fake:pw@example.com\n",
        "the native `store` helper approved the credentials"
    );

    let outcome = cascade
        .invoke(helper::Action::get_for_url("https://example.com/repo"), prompt())?
        .expect("stored credentials are found");
    assert_eq!(outcome.identity.password, "pw");
    cascade.invoke(outcome.next.erase(), prompt())?;
    assert_eq!(
        std::fs::read_to_string(home.join(".git-credentials"))?,
        "",
        "and erased them again"
    );

    let mut with_file =
        helper::Cascade::default().extend([Program::from_custom_definition("store --file ~/custom-store")?]);
    let outcome = with_file.invoke(
        helper::Action::Get(gix_credentials::protocol::Context {
            protocol: Some("https".into()),
            host: Some("example.net".into()),
            username: Some("u".into()),
            password: Some("p".into()),
            ..Default::default()
        }),
        prompt(),
    )?;
    with_file.invoke(outcome.expect("complete identity").next.store(), prompt())?;
    assert_eq!(
        std::fs::read_to_string(home.join("custom-store"))?,
        "https://u:p@example.net\n",
        "a `store` helper with arguments that need a shell to be understood is native as well"
    );
    assert!(
        !log.exists(),
        "git or one of its helpers was run: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    Ok(())
}
