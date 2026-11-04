//! The HTTP peer against `git` as the client, to show that it behaves like a real server, with its
//! authentication modes, before any other client is tried against it.
#![cfg(unix)]
use std::path::{Path, PathBuf};

use gix_testtools::{
    Creation, TestResult,
    http_peer::{BasicAuth, Config, Fault, HttpPeer, Protect},
};

fn git_supports_http() -> bool {
    let supported = gix_path::env::core_dir_program("git-remote-http").is_some();
    assert!(
        supported || !gix_testtools::is_ci::cached(),
        "CI has a git that can use http, but {:?} doesn't have git-remote-http",
        gix_path::env::core_dir()
    );
    if !supported {
        eprintln!("SKIPPED: the git in use has no support for http, run with one that has");
    }
    supported
}

struct Setup {
    dir: gix_testtools::tempfile::TempDir,
}

impl Setup {
    fn new(object_format: &str) -> TestResult<Self> {
        Ok(Setup {
            dir: gix_testtools::scripted_fixture_writable_with_args(
                "make_http_peer_repos.sh",
                [object_format],
                Creation::CopyFromReadOnly,
            )?,
        })
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_owned()
    }

    fn work(&self) -> PathBuf {
        self.dir.path().join("work")
    }

    fn peer(&self, protect: Protect, fault: Fault) -> TestResult<HttpPeer> {
        Ok(HttpPeer::start(Config {
            project_root: self.root(),
            auth: Some(BasicAuth {
                username: "user".into(),
                password: "secret".into(),
            }),
            protect,
            fault,
            ..Default::default()
        })?)
    }

    fn remote_refs(&self) -> TestResult<String> {
        Ok(gix_testtools::git(
            self.dir.path().join("remote.git"),
            "for-each-ref --format='%(refname)'",
        )?)
    }
}

/// Run git in `dir`, with a credential helper providing `credentials` if set.
fn git(dir: &Path, credentials: Option<(&str, &str)>, args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut cmd = gix_testtools::git_command(dir);
    if let Some((user, password)) = credentials {
        cmd.args([
            "-c",
            &format!("credential.helper=!f() {{ echo username={user}; echo password={password}; }}; f"),
        ]);
    }
    cmd.args(args).output()
}

const GOOD: Option<(&str, &str)> = Some(("user", "secret"));

fn assert_ok(out: std::io::Result<std::process::Output>, what: &str) -> TestResult<String> {
    let out = out?;
    assert!(out.status.success(), "{what}: {}", String::from_utf8_lossy(&out.stderr));
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn assert_fails(out: std::io::Result<std::process::Output>, what: &str) -> TestResult<String> {
    let out = out?;
    assert!(!out.status.success(), "{what} should have failed");
    Ok(String::from_utf8_lossy(&out.stderr).into_owned())
}

fn statuses(peer: &HttpPeer) -> Vec<(String, u16)> {
    peer.requests()
        .into_iter()
        .map(|req| {
            (
                format!("{} {}", req.method, req.target.split('?').next().unwrap_or_default()),
                req.status,
            )
        })
        .collect()
}

#[test]
fn unprotected_peer_serves_reads_and_accepts_pushes_with_protocol_v2() -> TestResult {
    if !git_supports_http() {
        return Ok(());
    }
    for hash in ["sha1", "sha256"] {
        let setup = Setup::new(hash)?;
        let peer = setup.peer(Protect::Nothing, Fault::None)?;
        let url = peer.url("remote.git");
        let refs = assert_ok(git(&setup.work(), None, &["ls-remote", &url]), "ls-remote")?;
        assert!(refs.contains("refs/heads/main"), "{hash}: {refs}");
        let info_refs = peer
            .requests()
            .into_iter()
            .find(|req| req.target.contains("info/refs"))
            .expect("one request for the advertisement");
        assert_eq!(
            info_refs.header("git-protocol"),
            Some("version=2"),
            "{hash}: the peer forwards the protocol version the client asked for to the backend"
        );

        assert_ok(
            git(&setup.work(), None, &["push", &url, "main:refs/heads/pushed"]),
            "push without protection",
        )?;
        assert!(
            setup.remote_refs()?.contains("refs/heads/pushed"),
            "{hash}: the push arrived"
        );
    }
    Ok(())
}

#[test]
fn everything_protected_needs_the_right_credentials() -> TestResult {
    if !git_supports_http() {
        return Ok(());
    }
    let setup = Setup::new("sha1")?;
    let peer = setup.peer(Protect::Everything, Fault::None)?;
    let url = peer.url("remote.git");
    assert_fails(git(&setup.work(), None, &["ls-remote", &url]), "no credentials")?;
    assert_fails(
        git(&setup.work(), Some(("user", "wrong")), &["ls-remote", &url]),
        "wrong password",
    )?;
    assert_eq!(
        statuses(&peer).last(),
        Some(&("GET /remote.git/info/refs".to_owned(), 401)),
        "the peer challenges and git gives up"
    );
    assert_ok(git(&setup.work(), GOOD, &["ls-remote", &url]), "good credentials")?;
    assert_ok(
        git(&setup.work(), GOOD, &["push", &url, "main:refs/heads/pushed"]),
        "push with good credentials",
    )?;
    assert!(setup.remote_refs()?.contains("refs/heads/pushed"));
    Ok(())
}

#[test]
fn only_post_protected_challenges_the_push_and_not_the_advertisement() -> TestResult {
    if !git_supports_http() {
        return Ok(());
    }
    let setup = Setup::new("sha1")?;
    let peer = setup.peer(Protect::Post, Fault::None)?;
    let url = peer.url("remote.git");
    assert_ok(git(&setup.work(), None, &["ls-remote", &url]), "reading is public")?;
    assert_fails(
        git(&setup.work(), None, &["push", &url, "main:refs/heads/pushed"]),
        "pushing needs credentials",
    )?;
    assert!(!setup.remote_refs()?.contains("refs/heads/pushed"));
    let before = statuses(&peer);
    assert_ok(
        git(&setup.work(), GOOD, &["push", &url, "main:refs/heads/pushed"]),
        "git re-authenticates the POST",
    )?;
    let after = statuses(&peer);
    assert!(
        after[before.len()..]
            .iter()
            .any(|(request, status)| request == "POST /remote.git/git-receive-pack" && *status == 401),
        "the first POST of git is challenged: {after:?}"
    );
    assert!(setup.remote_refs()?.contains("refs/heads/pushed"));
    Ok(())
}

#[test]
fn credentials_for_reading_only_are_rejected_when_pushing() -> TestResult {
    if !git_supports_http() {
        return Ok(());
    }
    let setup = Setup::new("sha1")?;
    let peer = setup.peer(Protect::ReadOnlyCredentials, Fault::None)?;
    let url = peer.url("remote.git");
    assert_ok(
        git(&setup.work(), GOOD, &["ls-remote", &url]),
        "reading with credentials works",
    )?;
    let err = assert_fails(
        git(&setup.work(), GOOD, &["push", &url, "main:refs/heads/pushed"]),
        "the POST is rejected",
    )?;
    assert!(err.contains("401") || err.contains("Authentication failed"), "{err}");
    assert!(!setup.remote_refs()?.contains("refs/heads/pushed"));
    Ok(())
}

#[test]
fn only_the_advertisement_protected_challenges_pushes_there() -> TestResult {
    if !git_supports_http() {
        return Ok(());
    }
    let setup = Setup::new("sha1")?;
    let peer = setup.peer(Protect::InfoRefs, Fault::None)?;
    let url = peer.url("remote.git");
    assert_fails(
        git(&setup.work(), None, &["push", &url, "main:refs/heads/pushed"]),
        "challenged early",
    )?;
    assert_ok(
        git(&setup.work(), GOOD, &["push", &url, "main:refs/heads/pushed"]),
        "good credentials",
    )?;
    assert!(setup.remote_refs()?.contains("refs/heads/pushed"));
    Ok(())
}

#[test]
fn receive_pack_protection_leaves_reading_public_and_challenges_the_push_advertisement() -> TestResult {
    if !git_supports_http() {
        return Ok(());
    }
    let setup = Setup::new("sha1")?;
    let peer = setup.peer(Protect::ReceivePack, Fault::None)?;
    let url = peer.url("remote.git");
    assert_ok(git(&setup.work(), None, &["ls-remote", &url]), "reading is public")?;
    assert_fails(
        git(&setup.work(), None, &["push", &url, "main:refs/heads/pushed"]),
        "pushing needs credentials",
    )?;
    assert!(
        statuses(&peer).contains(&("GET /remote.git/info/refs".to_owned(), 401)),
        "the receive-pack advertisement is challenged, unlike the one for reading: {:?}",
        statuses(&peer)
    );
    assert_ok(
        git(&setup.work(), GOOD, &["push", &url, "main:refs/heads/pushed"]),
        "push with credentials",
    )?;
    assert!(setup.remote_refs()?.contains("refs/heads/pushed"));
    Ok(())
}

/// Send `request` and then `body` as the body, without waiting for the response, and return the response head.
fn send_raw(peer: &HttpPeer, head: &str, body: &[u8]) -> std::io::Result<String> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(peer.addr())?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(20)))?;
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    Ok(String::from_utf8_lossy(&response).into_owned())
}

#[test]
fn rejecting_a_large_upload_is_seen_as_a_status_and_not_a_reset() -> TestResult {
    let setup = Setup::new("sha1")?;
    for protect in [Protect::Post, Protect::ReceivePack, Protect::Everything] {
        let peer = setup.peer(protect, Fault::None)?;
        let body = vec![b'x'; 8 * 1024 * 1024];
        let head = format!(
            "POST /remote.git/git-receive-pack HTTP/1.1\r\nHost: peer\r\nContent-Type: application/x-git-receive-pack-request\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let response = send_raw(&peer, &head, &body)?;
        assert!(
            response.starts_with("HTTP/1.1 401"),
            "{protect:?}: the client that sent its body right away gets the 401 as the connection isn't reset with unread data: {response:?}"
        );
        assert!(
            response.contains("WWW-Authenticate: Basic"),
            "{protect:?}: the challenge is part of the response"
        );
    }
    Ok(())
}

#[test]
fn malformed_chunked_bodies_are_a_bad_request() -> TestResult {
    let setup = Setup::new("sha1")?;
    let peer = setup.peer(Protect::Nothing, Fault::None)?;
    let head = "POST /remote.git/git-upload-pack HTTP/1.1\r\nHost: peer\r\nContent-Type: application/x-git-upload-pack-request\r\nTransfer-Encoding: chunked\r\n\r\n";
    for body in ["zz\r\nabc\r\n0\r\n\r\n", "3\r\nabcXY0\r\n\r\n"] {
        let response = send_raw(&peer, head, body.as_bytes())?;
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "framing errors are reported instead of being taken for a truncated body: {response:?}"
        );
    }
    Ok(())
}

#[test]
fn a_fault_can_be_limited_to_posts_so_that_the_advertisement_is_served() -> TestResult {
    if !git_supports_http() {
        return Ok(());
    }
    let setup = Setup::new("sha1")?;
    let peer = HttpPeer::start(Config {
        project_root: setup.root(),
        fault: Fault::StopReadingRequestBody,
        fault_posts_only: true,
        ..Default::default()
    })?;
    let response = send_raw(
        &peer,
        "GET /remote.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: peer\r\n\r\n",
        b"",
    )?;
    assert!(response.starts_with("HTTP/1.1 200"), "{response:?}");
    Ok(())
}

#[test]
fn a_renamed_repository_is_followed_by_git_for_reading_and_pushing() -> TestResult {
    if !git_supports_http() {
        return Ok(());
    }
    let setup = Setup::new("sha1")?;
    let peer = HttpPeer::start(Config {
        project_root: setup.root(),
        moved: Some(gix_testtools::http_peer::Moved {
            from: "old/remote.git".into(),
            to: "remote.git".into(),
        }),
        ..Default::default()
    })?;
    let url = peer.url("old/remote.git");
    assert_ok(
        git(&setup.work(), None, &["ls-remote", &url]),
        "reading through the redirect",
    )?;
    assert_ok(
        git(&setup.work(), None, &["push", &url, "main:refs/heads/pushed"]),
        "pushing through the redirect",
    )?;
    assert!(setup.remote_refs()?.contains("refs/heads/pushed"));
    assert!(
        statuses(&peer).contains(&("GET /old/remote.git/info/refs".to_owned(), 301)),
        "the old name is redirected: {:?}",
        statuses(&peer)
    );
    Ok(())
}

#[test]
fn keep_alive_serves_several_requests_per_connection_and_closes_idle_ones() -> TestResult {
    use std::io::{Read, Write};
    let setup = Setup::new("sha1")?;
    let peer = HttpPeer::start(Config {
        project_root: setup.root(),
        keep_alive_idle: Some(std::time::Duration::from_millis(200)),
        ..Default::default()
    })?;
    let mut stream = std::net::TcpStream::connect(peer.addr())?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let request = "GET /remote.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: peer\r\n\r\n";
    for _ in 0..2 {
        stream.write_all(request.as_bytes())?;
        let mut head = [0; 12];
        stream.read_exact(&mut head)?;
        assert_eq!(
            &head, b"HTTP/1.1 200",
            "a second request is served on the same connection"
        );
        // Skip to the end of the response, which has a `Content-Length`, by reading what the peer has sent so far.
        std::thread::sleep(std::time::Duration::from_millis(50));
        stream.set_nonblocking(true)?;
        let mut sink = [0; 65536];
        while stream.read(&mut sink).is_ok_and(|read| read > 0) {}
        stream.set_nonblocking(false)?;
    }
    assert_eq!(
        peer.requests().iter().map(|req| req.connection).collect::<Vec<_>>(),
        [0, 0],
        "both requests were served by one connection"
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest)?;
    assert!(
        rest.is_empty(),
        "the peer closed the idle connection without sending anything"
    );
    Ok(())
}

#[test]
fn git_uses_the_peer_as_an_authenticating_proxy() -> TestResult {
    if !git_supports_http() {
        return Ok(());
    }
    let setup = Setup::new("sha1")?;
    let peer = HttpPeer::start(Config {
        project_root: setup.root(),
        proxy_mode: gix_testtools::http_peer::ProxyMode::Forward,
        proxy_auth: Some(BasicAuth {
            username: "user".into(),
            password: "secret".into(),
        }),
        ..Default::default()
    })?;
    let url = "http://git.example.invalid/remote.git";
    // The proxy is the point of this test, so the isolation's `NO_PROXY=*` must not apply.
    let proxy = |credentials: &str| format!("http.proxy=http://{credentials}{}", peer.addr());
    let git = |args: &[&str]| -> std::io::Result<std::process::Output> {
        let mut cmd = gix_testtools::git_command(setup.work());
        cmd.env_remove("NO_PROXY").env_remove("no_proxy").args(args);
        cmd.output()
    };
    assert_fails(
        git(&["-c", &proxy(""), "ls-remote", url]),
        "the proxy asks for credentials",
    )?;
    assert_ok(
        git(&["-c", &proxy("user:secret@"), "ls-remote", url]),
        "with credentials the proxy serves the request",
    )?;
    let requests = peer.requests();
    assert!(
        requests.iter().any(|req| req.status == 407),
        "a request without credentials is refused: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|req| req.target.starts_with("http://git.example.invalid/") && req.status == 200),
        "{requests:?}"
    );
    Ok(())
}
