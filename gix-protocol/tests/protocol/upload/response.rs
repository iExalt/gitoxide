use gix_hash::ObjectId;
use gix_protocol::upload::response::{self, Reference};
use gix_testtools::TestResult;

#[test]
fn ls_refs_lines_are_formatted_like_git() -> TestResult {
    let id = ObjectId::from_hex(b"8590be53f83c8dd4ba4e5ff1d2c0e092f2a4ca3b")?;
    let tag = ObjectId::from_hex(b"3fb257d1300707098c4d562b1926e0c34f19992e")?;
    let lines = [
        Reference {
            id: Some(id),
            name: "HEAD".into(),
            symref_target: Some("refs/heads/main".into()),
            peeled: None,
        },
        Reference {
            id: Some(tag),
            name: "refs/tags/v1".into(),
            symref_target: None,
            peeled: Some(id),
        },
        Reference {
            id: None,
            name: "HEAD".into(),
            symref_target: Some("refs/heads/unborn".into()),
            peeled: None,
        },
    ];
    assert_eq!(
        String::from_utf8(response::ls_refs(&lines)?)?,
        "00508590be53f83c8dd4ba4e5ff1d2c0e092f2a4ca3b HEAD symref-target:refs/heads/main\n\
         006a3fb257d1300707098c4d562b1926e0c34f19992e refs/tags/v1 peeled:8590be53f83c8dd4ba4e5ff1d2c0e092f2a4ca3b\n\
         0030unborn HEAD symref-target:refs/heads/unborn\n\
         0000"
    );
    Ok(())
}

#[test]
fn acknowledgments_end_with_a_delimiter_only_if_the_pack_follows() -> TestResult {
    let id = ObjectId::from_hex(b"8590be53f83c8dd4ba4e5ff1d2c0e092f2a4ca3b")?;
    assert_eq!(
        String::from_utf8(response::acknowledgments(&[], false)?)?,
        "0014acknowledgments\n0008NAK\n0000"
    );
    assert_eq!(
        String::from_utf8(response::acknowledgments(&[id], false)?)?,
        "0014acknowledgments\n0031ACK 8590be53f83c8dd4ba4e5ff1d2c0e092f2a4ca3b\n0000"
    );
    assert_eq!(
        String::from_utf8(response::acknowledgments(&[id], true)?)?,
        "0014acknowledgments\n0031ACK 8590be53f83c8dd4ba4e5ff1d2c0e092f2a4ca3b\n000aready\n0001"
    );
    Ok(())
}

#[test]
fn error_packets_are_single_lines() {
    assert_eq!(response::error("not our ref\nabc"), b"0017ERR not our ref abc");
    assert_eq!(response::packfile(), b"000dpackfile\n");
}
