//! Proxies are used where they are configured, authenticate with the credentials in their URL or those of credential helpers,
//! and are skipped for hosts in `no_proxy`. The peer is a proxy that forwards to itself, which means that a host name that
//! can't be resolved is reachable only through it.
//!
//! These tests run without `NO_PROXY` in their environment, unlike the others: `curl` lets it take precedence over a proxy that
//! is configured, so it would make the proxy under test disappear. Don't copy this isolation without that reason.
use gix::{protocol::transport::client::blocking_io::http, remote::Direction};
use gix_testtools::{
    TestResult,
    http_peer::{BasicAuth, Config, HttpPeer, ProxyMode},
};

use crate::{HASHES, Helper, PASSWORD, Setup, USER, for_each_backend, isolated_for_proxy_tests, transport};

const UNREACHABLE_HOST: &str = "http://git.example.invalid";

fn proxy_peer(setup: &Setup) -> TestResult<HttpPeer> {
    Ok(HttpPeer::start(Config {
        project_root: setup.path("."),
        proxy_mode: ProxyMode::Forward,
        proxy_auth: Some(BasicAuth {
            username: USER.into(),
            password: PASSWORD.into(),
        }),
        ..Default::default()
    })?)
}

fn ref_map<H: http::Http + Default + 'static>(
    setup: &Setup,
    url: &str,
    config: &[(&str, String)],
    helper: Option<&Helper>,
) -> Result<(), gix::Error> {
    let mut repo = setup.dest().expect("the fixture repository can be opened");
    if let Some(helper) = helper {
        helper.configure(&mut repo).expect("a valid configuration");
    }
    {
        let mut snapshot = repo.config_snapshot_mut();
        for (key, value) in config {
            snapshot.set_raw_value(*key, value.as_str()).expect("valid key");
        }
    }
    repo.remote_at(url)?
        .with_refspecs(Some("+refs/heads/*:refs/remotes/origin/*"), Direction::Fetch)?
        .to_connection_with_transport(transport::<H>(url).expect("a valid URL"))
        .ref_map(gix::progress::Discard, Default::default())?;
    Ok(())
}

fn a_proxy_with_credentials_in_its_url_carries_the_traffic<H: http::Http + Default + 'static>(
    hash: &'static str,
) -> TestResult {
    let setup = Setup::new(hash)?;
    let peer = proxy_peer(&setup)?;
    let proxy = format!("http://{USER}:{PASSWORD}@{}", peer.addr());
    ref_map::<H>(
        &setup,
        &format!("{UNREACHABLE_HOST}/remote.git"),
        &[("http.proxy", proxy)],
        None,
    )?;
    let requests = peer.requests();
    assert!(
        !requests.is_empty() && requests.iter().all(|req| req.target.starts_with(UNREACHABLE_HOST)),
        "{hash}: all requests went to the proxy: {requests:?}"
    );
    // `curl` tries without credentials first, and is refused, to learn which authentication the proxy wants.
    assert!(
        requests
            .iter()
            .filter(|req| req.status == 200)
            .all(|req| req.header("proxy-authorization").is_some())
            && requests.iter().any(|req| req.status == 200),
        "{hash}: the proxy served only requests that carried its credentials: {requests:?}"
    );
    Ok(())
}

fn a_proxy_that_needs_a_password_gets_it_from_the_credential_helpers<H: http::Http + Default + 'static>(
    hash: &'static str,
    password: &str,
) -> TestResult {
    let setup = Setup::new(hash)?;
    let peer = proxy_peer(&setup)?;
    let helper = Helper::new(&setup.path("."), USER, password)?;
    let result = ref_map::<H>(
        &setup,
        &format!("{UNREACHABLE_HOST}/remote.git"),
        &[("http.proxy", format!("http://{USER}@{}", peer.addr()))],
        Some(&helper),
    );
    // `curl` asks for each request, and `reqwest` only again if the proxy refused what it had.
    let actions = helper.actions();
    let accepted = password == PASSWORD;
    assert_eq!(
        actions.first().map(String::as_str),
        Some("get"),
        "{hash}: the password is asked for first"
    );
    assert_eq!(
        actions.iter().any(|action| action == "store"),
        accepted,
        "{hash}: a password is remembered if the proxy took it: {actions:?}"
    );
    assert_eq!(
        actions.iter().any(|action| action == "erase"),
        !accepted,
        "{hash}: a password is forgotten if the proxy refused it: {actions:?}"
    );
    assert_eq!(
        result.is_ok(),
        password == PASSWORD,
        "{hash}: only the right password gets through the proxy: {result:?}"
    );
    Ok(())
}

fn hosts_that_are_not_proxied_are_reached_directly<H: http::Http + Default + 'static>(
    hash: &'static str,
) -> TestResult {
    let setup = Setup::new(hash)?;
    let peer = proxy_peer(&setup)?;
    // The proxy would refuse this client, as it doesn't know its credentials, so success means it wasn't used.
    ref_map::<H>(
        &setup,
        &peer.url("remote.git"),
        &[
            ("http.proxy", format!("http://{}", peer.addr())),
            ("gitoxide.http.noProxy", "127.0.0.1".into()),
        ],
        None,
    )?;
    let requests = peer.requests();
    assert!(
        !requests.is_empty() && requests.iter().all(|req| req.target.starts_with('/')),
        "{hash}: requests were sent to the server, and not asked of a proxy: {requests:?}"
    );
    Ok(())
}

fn an_empty_proxy_setting_switches_all_proxies_off<H: http::Http + Default + 'static>(
    hash: &'static str,
) -> TestResult {
    let setup = Setup::new(hash)?;
    let peer = proxy_peer(&setup)?;
    // The environment points to a proxy that doesn't exist, which fails the request if anything uses it.
    ref_map::<H>(&setup, &peer.url("remote.git"), &[("http.proxy", String::new())], None)?;
    let requests = peer.requests();
    assert!(
        !requests.is_empty() && requests.iter().all(|req| req.target.starts_with('/')),
        "{hash}: requests went to the server directly: {requests:?}"
    );
    Ok(())
}

#[test]
fn proxies_with_credentials_in_their_url_are_used() -> TestResult {
    let Some(_environment) = isolated_for_proxy_tests()? else {
        return Ok(());
    };
    for hash in HASHES {
        for_each_backend!(a_proxy_with_credentials_in_its_url_carries_the_traffic(hash));
    }
    Ok(())
}

#[test]
fn proxy_passwords_come_from_credential_helpers_and_are_forgotten_if_refused() -> TestResult {
    let Some(_environment) = isolated_for_proxy_tests()? else {
        return Ok(());
    };
    for password in [PASSWORD, "wrong"] {
        for_each_backend!(a_proxy_that_needs_a_password_gets_it_from_the_credential_helpers(
            "sha1", password
        ));
    }
    Ok(())
}

#[test]
fn hosts_in_no_proxy_are_not_sent_through_the_proxy() -> TestResult {
    let Some(_environment) = isolated_for_proxy_tests()? else {
        return Ok(());
    };
    for_each_backend!(hosts_that_are_not_proxied_are_reached_directly("sha1"));
    Ok(())
}

#[test]
fn an_empty_proxy_setting_overrides_the_proxy_of_the_environment() -> TestResult {
    let Some(environment) = isolated_for_proxy_tests()? else {
        return Ok(());
    };
    let _environment = environment
        .set("http_proxy", "http://127.0.0.1:1")
        .set("HTTP_PROXY", "http://127.0.0.1:1")
        .set("all_proxy", "http://127.0.0.1:1");
    for_each_backend!(an_empty_proxy_setting_switches_all_proxies_off("sha1"));
    Ok(())
}
