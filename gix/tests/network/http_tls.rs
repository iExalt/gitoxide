//! HTTPS against a peer that terminates TLS with a certificate of a generated authority: the CA bundle, the verification
//! and the protocol versions that are configured must be applied by each backend that was built with TLS support.
//!
//! The authority and the certificates exist only for the duration of a test. Nothing here reaches the network, and
//! the system's certificate store has no part in it: a client that isn't given the authority must refuse the peer.
#![cfg(any(
    feature = "blocking-http-transport-curl-openssl",
    feature = "blocking-http-transport-curl-rustls",
    feature = "blocking-http-transport-reqwest-rust-tls",
    feature = "blocking-http-transport-reqwest-rust-tls-ring",
    feature = "blocking-http-transport-reqwest-rust-tls-trust-dns",
    feature = "blocking-http-transport-reqwest-native-tls"
))]
use std::{path::PathBuf, sync::Arc};

use gix::protocol::transport::client::blocking_io::http;
use gix_testtools::{
    TestResult,
    http_peer::{Acceptor, Config, HttpPeer, Stream},
};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair, SanType};

use crate::{Setup, for_each_tls_backend, http_proxy::ref_map, isolated};

/// What the certificate of the peer is valid for.
enum Name {
    LoopbackAddress,
    AnotherHost,
}

/// A peer that serves over TLS with `versions`, and the file with the certificate of the authority that issued its certificate.
struct TlsPeer {
    peer: HttpPeer,
    ca_file: PathBuf,
}

impl TlsPeer {
    fn start(
        setup: &Setup,
        name: Name,
        versions: &'static [&'static rustls::SupportedProtocolVersion],
    ) -> TestResult<Self> {
        let mut authority = CertificateParams::new(Vec::<String>::new())?;
        authority.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        authority
            .distinguished_name
            .push(DnType::CommonName, "synthetic test authority");
        let authority = CertifiedIssuer::self_signed(authority, KeyPair::generate()?)?;

        let mut server = CertificateParams::new(Vec::<String>::new())?;
        server.subject_alt_names = vec![match name {
            Name::LoopbackAddress => SanType::IpAddress(std::net::Ipv4Addr::LOCALHOST.into()),
            Name::AnotherHost => SanType::DnsName("other.example".try_into()?),
        }];
        let server_key = KeyPair::generate()?;
        let server_certificate = server.signed_by(&server_key, &authority)?;

        let ca_file = setup.path("authority.pem");
        std::fs::write(&ca_file, authority.pem())?;

        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(versions)?
                .with_no_client_auth()
                .with_single_cert(
                    vec![server_certificate.der().clone()],
                    rustls::pki_types::PrivateKeyDer::try_from(server_key.serialize_der())
                        .map_err(std::io::Error::other)?,
                )?,
        );
        let acceptor: Acceptor = Arc::new(move |tcp| {
            let connection = rustls::ServerConnection::new(config.clone()).map_err(std::io::Error::other)?;
            Ok(Box::new(rustls::StreamOwned::new(connection, tcp)) as Box<dyn Stream>)
        });
        let peer = HttpPeer::start_with_acceptor(
            Config {
                project_root: setup.path("."),
                ..Default::default()
            },
            acceptor,
        )?;
        Ok(TlsPeer { peer, ca_file })
    }

    fn url(&self) -> String {
        format!("https://{}/remote.git", self.peer.addr())
    }

    fn ca(&self) -> (&'static str, String) {
        ("http.sslCAInfo", self.ca_file.display().to_string())
    }
}

const ALL_VERSIONS: &[&rustls::SupportedProtocolVersion] = rustls::ALL_VERSIONS;
const TLS_1_3_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];
const TLS_1_2_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS12];

fn only_the_authority_that_was_configured_is_trusted<H: http::Http + Default + 'static>() -> TestResult {
    let setup = Setup::new("sha1")?;
    let peer = TlsPeer::start(&setup, Name::LoopbackAddress, ALL_VERSIONS)?;
    ref_map::<H>(&setup, &peer.url(), &[peer.ca()], None)
        .map_err(|err| format!("the certificate of the configured authority is trusted: {err:?}"))?;
    assert!(
        ref_map::<H>(&setup, &peer.url(), &[], None).is_err(),
        "without the authority the certificate is unknown, whatever the system trusts"
    );
    ref_map::<H>(&setup, &peer.url(), &[("http.sslVerify", "false".into())], None)
        .map_err(|err| format!("without verification the certificate is accepted: {err:?}"))?;
    Ok(())
}

fn a_certificate_for_another_host_is_refused_unless_verification_is_off<H: http::Http + Default + 'static>()
-> TestResult {
    let setup = Setup::new("sha1")?;
    let peer = TlsPeer::start(&setup, Name::AnotherHost, ALL_VERSIONS)?;
    assert!(
        ref_map::<H>(&setup, &peer.url(), &[peer.ca()], None).is_err(),
        "the authority is trusted, but the certificate is for a different host"
    );
    ref_map::<H>(
        &setup,
        &peer.url(),
        &[peer.ca(), ("http.sslVerify", "false".into())],
        None,
    )
    .map_err(|err| format!("without verification the host name doesn't matter either: {err:?}"))?;
    Ok(())
}

fn the_protocol_version_is_limited_as_configured<H: http::Http + Default + 'static>() -> TestResult {
    let setup = Setup::new("sha1")?;
    let peer = TlsPeer::start(&setup, Name::LoopbackAddress, TLS_1_2_ONLY)?;
    assert!(
        ref_map::<H>(
            &setup,
            &peer.url(),
            &[peer.ca(), ("http.sslVersion", "tlsv1.3".into())],
            None
        )
        .is_err(),
        "a server that speaks TLS 1.2 only can't be reached by a client that requires 1.3"
    );
    ref_map::<H>(
        &setup,
        &peer.url(),
        &[peer.ca(), ("http.sslVersion", "tlsv1.2".into())],
        None,
    )
    .map_err(|err| format!("1.2 is what both sides speak: {err:?}"))?;

    // A single version is the oldest that is acceptable, so hardening to 1.2 must not rule out servers that only speak 1.3.
    let peer = TlsPeer::start(&setup, Name::LoopbackAddress, TLS_1_3_ONLY)?;
    ref_map::<H>(
        &setup,
        &peer.url(),
        &[peer.ca(), ("http.sslVersion", "tlsv1.2".into())],
        None,
    )
    .map_err(|err| format!("`tlsv1.2` means 1.2 or later: {err:?}"))?;
    Ok(())
}

#[test]
fn only_the_authority_that_was_configured_is_trusted_by_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_tls_backend!(only_the_authority_that_was_configured_is_trusted());
    Ok(())
}

#[test]
fn a_certificate_for_another_host_is_refused_by_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_tls_backend!(a_certificate_for_another_host_is_refused_unless_verification_is_off());
    Ok(())
}

#[test]
fn the_protocol_version_is_limited_as_configured_by_each_backend() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for_each_tls_backend!(the_protocol_version_is_limited_as_configured());
    Ok(())
}
