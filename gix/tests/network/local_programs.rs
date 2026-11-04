//! A push or fetch to a local path runs `git-receive-pack` or `git-upload-pack` as a program, like Git does. Consumers that must not
//! run Git programs refuse it with Git's own setting, `protocol.file.allow=never` or `protocol.allow=never`, which fails before
//! any program is looked up.
use gix::remote::Direction;
use gix_testtools::TestResult;

use crate::{Setup, isolated};

#[test]
fn a_local_url_is_refused_by_configuration_before_a_program_runs() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    let setup = Setup::new("sha1")?;
    for (key, value) in [("protocol.file.allow", "never"), ("protocol.allow", "never")] {
        let mut repo = setup.work()?;
        repo.config_snapshot_mut().set_raw_value(key, value)?;
        let remote = repo.remote_at(setup.path("remote.git").display().to_string().as_str())?;
        for direction in [Direction::Push, Direction::Fetch] {
            let err = remote
                .connect(direction)
                .err()
                .unwrap_or_else(|| panic!("{key}={value}: {direction:?} to a local path is refused"));
            assert!(
                gix_error::classify(&err).is_permission_denied(),
                "{key}={value}: the refusal says that the protocol is denied by configuration: {err:?}"
            );
        }
    }
    Ok(())
}
