use bstr::ByteSlice;
use gix_protocol::upload::Capabilities;
use gix_testtools::TestResult;

use super::{packets, read_only, upload_pack};

/// What Git advertises that isn't served yet, and what is advertised in its place. Shallow clones are not served, and the
/// `wait-for-done` feature goes with the negotiation that isn't there yet.
const NOT_SERVED: &[(&str, &str)] = &[("fetch=shallow wait-for-done", "fetch")];

#[test]
fn the_advertisement_is_what_git_upload_pack_writes_for_what_is_served() -> TestResult {
    let configurations: &[(&[&str], bool, bool)] = &[
        (&[], true, false),
        (&["lsrefs.unborn=allow"], false, false),
        (&["lsrefs.unborn=ignore"], false, false),
        (&["transfer.advertiseSID=true"], true, true),
    ];
    for f in read_only()? {
        for name in ["remote.git", "empty.git", "shallow.git"] {
            for (config, unborn, session_id) in configurations {
                // The client leaves, which ends the session of Git without an error.
                let git = upload_pack(&f.root.join(name), config, b"")?;
                assert!(git.success, "{name}: Git ends when the client leaves: {}", git.stderr);
                let lines: Vec<String> = packets(&git.stdout)
                    .into_iter()
                    .flatten()
                    .map(|line| String::from_utf8(line).expect("utf8").trim_end().to_owned())
                    .collect();
                let find = |prefix: &str| {
                    lines
                        .iter()
                        .find_map(|line| line.strip_prefix(prefix))
                        .map(ToOwned::to_owned)
                };
                let mut capabilities = Capabilities::new(f.kind, find("agent=").expect("Git announces its agent"));
                capabilities.unborn = *unborn;
                capabilities.session_id = session_id.then(|| find("session-id=").expect("Git announces the session"));
                let mut expected = git.stdout.to_str_lossy().into_owned();
                for (git_has, served) in NOT_SERVED {
                    let before = format!("{:04x}{git_has}\n", git_has.len() + 5);
                    let after = format!("{:04x}{served}\n", served.len() + 5);
                    assert!(expected.contains(&before), "Git advertises '{git_has}':\n{expected}");
                    expected = expected.replace(&before, &after);
                }
                assert_eq!(
                    capabilities.encode()?.to_str_lossy(),
                    expected,
                    "{} {name} {config:?}: byte for byte, with the agent and session id being those of Git and what isn't served yet left out",
                    f.kind
                );
            }
        }
    }
    Ok(())
}

#[test]
fn values_that_cannot_be_represented_are_refused() {
    for invalid in ["", "has space", "nul\0byte"] {
        let mut capabilities = Capabilities::new(gix_hash::Kind::Sha1, "git/2.56.0");
        capabilities.agent = invalid.into();
        assert!(
            capabilities.encode().is_err(),
            "agent {invalid:?} would corrupt the advertisement"
        );
        capabilities.agent = "git/2.56.0".into();
        capabilities.session_id = Some(invalid.into());
        assert!(
            capabilities.encode().is_err(),
            "session id {invalid:?} would corrupt the advertisement"
        );
    }
}
