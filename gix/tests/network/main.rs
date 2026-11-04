//! Authentication, protocol negotiation and failure handling of `gix` against synthetic network peers, with each HTTP backend.
//!
//! The peers are local: an HTTP server in front of `git http-backend` that can demand credentials in different ways. Nothing
//! here reaches the network. Each test runs in an isolated process as it depends on the environment, notably on proxy variables.
#![cfg(all(
    unix,
    any(
        feature = "blocking-http-transport-curl",
        feature = "blocking-http-transport-reqwest"
    )
))]
use std::path::{Path, PathBuf};

use gix::{
    progress::Discard,
    protocol::transport::{
        Service,
        client::{MessageKind, WriteMode, blocking_io::http},
    },
};
use gix_testtools::{
    Creation, TestResult,
    http_peer::{BasicAuth, Config, HttpPeer, Protect},
};

mod http_authentication;
mod http_connections;
mod http_fetch;
mod ssh;
mod ssh_config;

/// The hashes to test each case with.
const HASHES: [&str; 2] = ["sha1", "sha256"];

/// Run `$case::<Backend>($($arg),*)` for each HTTP backend that is available, to run the same case against all of them.
macro_rules! for_each_backend {
    ($case:ident($($arg:expr),*)) => {{
        let mut ran = 0;
        #[cfg(feature = "blocking-http-transport-curl")]
        {
            eprintln!("backend: curl");
            $case::<http::curl::Curl>($($arg),*)?;
            ran += 1;
        }
        #[cfg(feature = "blocking-http-transport-reqwest")]
        {
            eprintln!("backend: reqwest");
            $case::<http::reqwest::Remote>($($arg),*)?;
            ran += 1;
        }
        assert!(ran > 0, "this test target is built with at least one HTTP backend");
    }};
}
pub(crate) use for_each_backend;

/// The credentials every peer here accepts.
pub(crate) const USER: &str = "user";
pub(crate) const PASSWORD: &str = "secret";

/// A copy of the fixture repositories for one object format, to fetch from, push to, and operate in.
pub(crate) struct Setup {
    dir: gix_testtools::tempfile::TempDir,
}

impl Setup {
    pub fn new(hash: &'static str) -> TestResult<Self> {
        Ok(Setup {
            dir: gix_testtools::scripted_fixture_writable_with_args(
                "make_network_peer_repos.sh",
                [hash],
                Creation::CopyFromReadOnly,
            )?,
        })
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The repository that clients work in, opened without any outside configuration.
    pub fn work(&self) -> TestResult<gix::Repository> {
        Ok(gix::open_opts(self.path("work"), gix::open::Options::isolated())?)
    }

    pub fn dest(&self) -> TestResult<gix::Repository> {
        Ok(gix::open_opts(self.path("dest.git"), gix::open::Options::isolated())?)
    }

    pub fn peer(&self, protect: Protect) -> TestResult<HttpPeer> {
        Ok(HttpPeer::start(Config {
            project_root: self.dir.path().to_owned(),
            auth: Some(BasicAuth {
                username: USER.into(),
                password: PASSWORD.into(),
            }),
            protect,
            ..Default::default()
        })?)
    }
}

/// A credential helper that answers with `user` and `password`, and logs the actions that it was asked to do.
pub(crate) struct Helper {
    script: PathBuf,
    log: PathBuf,
}

impl Helper {
    pub fn new(dir: &Path, user: &str, password: &str) -> TestResult<Self> {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join(format!("helper-{user}-{password}"));
        let log = dir.join(format!("helper-{user}-{password}.log"));
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 echo \"$1\" >>'{log}'\n\
                 case \"$1\" in get) echo username={user}; echo password={password};; esac\n\
                 cat >/dev/null\n",
                log = log.display(),
            ),
        )?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        Ok(Helper { script, log })
    }

    /// Make `repo` use this helper, and nothing that could prompt.
    pub fn configure(&self, repo: &mut gix::Repository) -> TestResult {
        let mut config = repo.config_snapshot_mut();
        config.set_raw_value("credential.helper", format!("!{}", self.script.display()).as_str())?;
        config.set_raw_value("gitoxide.credentials.terminalPrompt", "false")?;
        Ok(())
    }

    /// The actions the helper was asked to do, in order.
    pub fn actions(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(ToOwned::to_owned)
            .collect()
    }
}

/// The method, path without query and status of each request the peer saw, in order.
pub(crate) fn statuses(peer: &HttpPeer) -> Vec<String> {
    peer.requests()
        .into_iter()
        .map(|req| {
            format!(
                "{} {} {}",
                req.method,
                req.target.split('?').next().unwrap_or_default().trim_start_matches('/'),
                req.status
            )
        })
        .collect()
}

/// Create the transport for `url` with backend `H`, asking for protocol version 2 like `gix` does.
pub(crate) fn transport<H: http::Http + Default>(url: &str) -> TestResult<http::Transport<H>> {
    Ok(http::connect::<H>(
        url.try_into()?,
        gix::protocol::transport::Protocol::V2,
        false,
    ))
}

/// Run `case` in an isolated process with an environment that can't redirect traffic, and return `true` in the parent process.
pub(crate) fn isolated() -> TestResult<Option<gix_testtools::Env<'static>>> {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(None);
    }
    Ok(Some(
        gix_testtools::Env::new()
            .set("GIT_TERMINAL_PROMPT", "0")
            .set("GCM_INTERACTIVE", "never")
            .unset("http_proxy")
            .unset("HTTP_PROXY")
            .unset("https_proxy")
            .unset("HTTPS_PROXY")
            .unset("all_proxy")
            .unset("ALL_PROXY")
            .set("NO_PROXY", "*")
            .set("no_proxy", "*"),
    ))
}

/// Do a push-style handshake with `transport`, authenticating like `gix` does, and follow it with the first request
/// a push sends to the server: the commands, here none, which is what `git` sends to see if it is allowed to push.
pub(crate) fn push_handshake_and_flush<T>(client: &gix::Repository, transport: T) -> TestResult<()>
where
    T: gix::protocol::transport::client::blocking_io::Transport,
{
    let url = gix::url::parse(transport.to_url().as_ref())?;
    let remote = client.remote_at(url)?;
    let mut connection = remote.to_connection_with_transport(transport);
    let authenticate = connection.configured_credentials_for_current_url();
    gix::protocol::handshake(
        connection.transport_mut(),
        Service::ReceivePack,
        authenticate,
        Vec::new(),
        &mut Discard,
    )?;
    let writer = connection
        .transport_mut()
        .request(WriteMode::Binary, MessageKind::Flush, false)?;
    let mut response = writer.into_read()?;
    // A server has nothing to say about a request without commands, which is not what's being tested.
    if let Err(err) = std::io::copy(&mut response, &mut std::io::sink())
        && err.kind() != std::io::ErrorKind::UnexpectedEof
    {
        return Err(err.into());
    }
    Ok(())
}
