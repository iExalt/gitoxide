use gix_hash::Kind;
use gix_protocol::{
    push::StatusFormat,
    receive::{Advertisement, Capabilities, Report, side_band, write_advertisement, write_report},
};
use gix_testtools::TestResult;
use gix_transport::Protocol;

#[test]
fn side_bands_split_data_like_git_does() {
    use side_band::{Channel, MAX_DATA_LEN, encode};
    assert!(
        encode(Channel::Data, b"").is_empty(),
        "no packet is written for no data"
    );
    assert_eq!(encode(Channel::Progress, b"hi"), b"0007\x02hi");
    let data = vec![b'x'; MAX_DATA_LEN * 2 + 10];
    let encoded = encode(Channel::Data, &data);
    assert_eq!(encoded.len(), data.len() + 3 * 5);
    assert_eq!(
        &encoded[..5],
        b"fff0\x01",
        "packets are as large as `LARGE_PACKET_MAX` of Git allows"
    );
    let second = 4 + MAX_DATA_LEN + 1;
    assert_eq!(&encoded[second..second + 5], b"fff0\x01");
    assert_eq!(
        &encoded[encoded.len() - 15..encoded.len() - 10],
        b"000f\x01",
        "the rest of the data is last"
    );
}

#[test]
fn the_drivers_write_what_the_encoders_return() -> TestResult {
    let kind = Kind::Sha1;
    let advertisement = Advertisement {
        capabilities: Capabilities::new(kind, "git/2.56.0"),
        references: Vec::new(),
        shallow: Vec::new(),
    };
    let mut out = Vec::new();
    write_advertisement(&mut out, &advertisement, Protocol::V1)?;
    assert_eq!(out, advertisement.encode(Protocol::V1)?);
    assert!(out.starts_with(b"000eversion 1\n"));

    let report = Report {
        unpack: None,
        commands: Vec::new(),
    };
    let plain = report.encode(StatusFormat::V1)?;
    let mut out = Vec::new();
    write_report(&mut out, &report, StatusFormat::V1, false)?;
    assert_eq!(out, plain);
    let mut out = Vec::new();
    write_report(&mut out, &report, StatusFormat::V1, true)?;
    assert_eq!(
        out,
        side_band::encode(side_band::Channel::Data, &plain),
        "the caller ends the response with a flush packet"
    );
    Ok(())
}
