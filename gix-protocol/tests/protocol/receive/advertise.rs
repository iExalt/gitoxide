use gix_hash::Kind;
use gix_protocol::receive::{Advertisement, Capabilities, Reference};
use gix_testtools::TestResult;
use gix_transport::Protocol;

use super::{capabilities, read_only, receive_pack, references, shallow};

fn agent_capabilities(kind: Kind) -> Capabilities {
    Capabilities::new(kind, "git/2.56.0")
}

#[test]
fn capabilities_are_listed_in_the_order_of_git() -> TestResult {
    assert_eq!(
        agent_capabilities(Kind::Sha1).to_line()?,
        "report-status report-status-v2 delete-refs side-band-64k quiet atomic ofs-delta object-format=sha1 agent=git/2.56.0"
    );
    let mut all = agent_capabilities(Kind::Sha1);
    all.atomic = false;
    all.ofs_delta = false;
    all.push_options = true;
    all.push_cert_nonce = Some("nonce".into());
    all.session_id = Some("sid".into());
    assert_eq!(
        all.to_line()?,
        "report-status report-status-v2 delete-refs side-band-64k quiet push-cert=nonce push-options session-id=sid object-format=sha1 agent=git/2.56.0",
        "optional capabilities are offered in the order `receive-pack` uses"
    );
    Ok(())
}

#[test]
fn values_that_cannot_be_represented_are_refused() {
    for invalid in ["", "has space", "nul\0byte"] {
        let mut capabilities = agent_capabilities(Kind::Sha1);
        capabilities.agent = invalid.into();
        assert!(
            capabilities.to_line().is_err(),
            "agent {invalid:?} would corrupt the capability list"
        );
    }
}

#[test]
fn without_references_the_capabilities_travel_with_the_null_id() -> TestResult {
    let kind = Kind::Sha1;
    let advertisement = Advertisement {
        capabilities: agent_capabilities(kind),
        references: Vec::new(),
        shallow: Vec::new(),
    };
    let encoded = advertisement.encode(Protocol::V0)?;
    assert_eq!(
        String::from_utf8(encoded)?,
        "00b10000000000000000000000000000000000000000 capabilities^{}\0report-status report-status-v2 delete-refs side-band-64k quiet atomic ofs-delta object-format=sha1 agent=git/2.56.0\n0000"
    );
    Ok(())
}

#[test]
fn the_advertisement_is_what_git_receive_pack_writes() -> TestResult {
    let configurations: &[(&[&str], Option<&str>)] = &[
        (&[], None),
        (&[], Some("version=1")),
        (&[], Some("version=2")),
        (
            &[
                "receive.advertisePushOptions=true",
                "receive.advertiseAtomic=false",
                "repack.useDeltaBaseOffset=false",
            ],
            None,
        ),
        (&["transfer.advertiseSID=true"], None),
    ];
    for f in read_only()? {
        for name in ["remote.git", "empty.git", "shallow.git"] {
            let dir = f.root.join(name);
            for (config, protocol) in configurations {
                // The client leaves, which makes Git fail after it wrote the advertisement.
                let git = receive_pack(&dir, config, *protocol, b"")?;
                assert!(!git.success, "{name}: Git expects a request after the advertisement");
                let version = match *protocol {
                    Some("version=1") => Protocol::V1,
                    Some("version=2") => Protocol::V2,
                    _ => Protocol::V0,
                };
                let advertisement = Advertisement {
                    capabilities: capabilities(f.kind, &git.stdout, config),
                    references: references(&dir)?,
                    shallow: shallow(&dir)?,
                };
                assert_eq!(
                    advertisement.encode(version)?.as_slice(),
                    git.stdout.as_slice(),
                    "{} {name} {config:?} {protocol:?}: byte for byte, with the agent and session id being those of Git",
                    f.kind
                );
            }
        }
    }
    Ok(())
}

#[test]
fn fixtures_cover_the_interesting_advertisements() -> TestResult {
    // Guard against fixtures that silently stop exercising what the comparison above is meant to cover.
    for f in read_only()? {
        let shallow_ids = shallow(&f.root.join("shallow.git"))?;
        assert_eq!(shallow_ids.len(), 1, "{}: the shallow boundary is advertised", f.kind);
        assert!(
            references(&f.root.join("empty.git"))?.is_empty(),
            "{}: an empty repository advertises capabilities^{{}}",
            f.kind
        );
        assert!(
            references(&f.remote())?
                .iter()
                .any(|r: &Reference| r.name == "refs/tags/v1"),
            "{}: annotated tags are advertised by the id of their tag object, which is not peeled",
            f.kind
        );
    }
    Ok(())
}
