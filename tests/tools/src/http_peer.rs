//! A synthetic HTTP peer to test clients that fetch from and push to servers over HTTP, which serves bare
//! repositories through `git http-backend` as a CGI program, so that real Git is the oracle for the protocol.
//!
//! What makes it useful for tests is what it adds around Git:
//!
//! * HTTP Basic authentication that protects `info/refs`, the `POST` requests, or both, including the case of
//!   credentials that are good enough to read but are rejected when writing;
//! * a log of all requests with their headers, to see which protocol version a client asked for, and which of them were challenged;
//! * [faults](crate::http_peer::Fault) to test timeouts and cancellation: a slow response, a response that never comes, and a server that stops reading;
//! * a [proxy mode](crate::http_peer::ProxyMode) to see how clients use an HTTP proxy.
//!
//! It handles each connection on its own thread, and closes it after the response. It's a test server, and trades
//! efficiency for simplicity.
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use parking_lot::Mutex;

/// The credentials that the peer accepts.
#[derive(Debug, Clone)]
pub struct BasicAuth {
    /// The accepted username.
    pub username: String,
    /// The accepted password.
    pub password: String,
}

/// Which requests need [`BasicAuth`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Protect {
    /// No request is challenged.
    #[default]
    Nothing,
    /// Only `GET /info/refs`, which is where Git servers typically challenge both fetches and pushes.
    InfoRefs,
    /// Only `POST` requests to `git-receive-pack`, as with a server that enables `http.receivepack` and protects only the request that
    /// pushes, so the advertisement is public and a client learns that it needs credentials only when it sends the push.
    /// `POST` requests that read are public.
    Post,
    /// `GET /info/refs?service=git-receive-pack` and `POST` requests to `git-receive-pack`, which is the recipe for anonymous
    /// reads and authenticated writes of `git-http-backend(1)`. Everything that reads is public.
    ReceivePack,
    /// Every request.
    Everything,
    /// Credentials are required and accepted for `GET /info/refs`, but every `POST` to `git-receive-pack` is rejected with a `401` even if
    /// the credentials are correct, as is the case with credentials that can only read.
    ReadOnlyCredentials,
}

/// Problems that a client has to deal with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fault {
    /// Behave.
    #[default]
    None,
    /// Send the response body in `chunk` bytes every `interval`, after sending the headers.
    SlowResponse {
        /// The amount of bytes to send at once.
        chunk: usize,
        /// The time between sending chunks.
        interval: Duration,
    },
    /// Send the headers of a response, and then nothing until the peer is dropped.
    StallAfterHeaders,
    /// Receive the request headers, but never read its body and never respond, until the peer is dropped.
    ///
    /// Note that a client only blocks while uploading if its body is larger than what the kernel buffers on both sides,
    /// which can be several megabytes. A smaller body is sent in full, and the client then waits for a response as it
    /// does with [`Fault::StallAfterHeaders`]. Tests of stalled uploads must send far more than that, and
    /// check from the client that the upload was still going on.
    StopReadingRequestBody,
    /// Accept the connection and read the request headers, but never respond, until the peer is dropped.
    Silent,
}

/// What the peer does with requests that carry an absolute URL, as they are sent to proxies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProxyMode {
    /// Treat them as normal requests to a server, and serve what the path of the URL names.
    #[default]
    Off,
    /// Require `Proxy-Authorization` with [`Config::proxy_auth`] and fail with `407` otherwise, but serve requests with
    /// an absolute URL by their path. This makes the peer a proxy that forwards to itself.
    Forward,
}

/// A repository that was renamed, as hosting services redirect requests to old names.
#[derive(Debug, Clone)]
pub struct Moved {
    /// The directory of the old repository name, like `old/repo.git`, without slashes around it.
    pub from: String,
    /// The directory of the new name, like `repo.git`, which is served normally.
    pub to: String,
}

/// The configuration of a [`HttpPeer`].
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// The directory that contains the repositories, as `GIT_PROJECT_ROOT` of `git http-backend`.
    pub project_root: PathBuf,
    /// The credentials to expect, if `protect` isn't [`Protect::Nothing`].
    ///
    /// Requests that pass authentication have `REMOTE_USER` set to the username, and requests that don't need it have
    /// `REMOTE_USER=anonymous`, which lets `git-http-backend` accept pushes from anybody. Real deployments typically
    /// answer an unauthenticated push with a `403` instead, which this peer doesn't do.
    pub auth: Option<BasicAuth>,
    /// Which requests need `auth`.
    pub protect: Protect,
    /// A problem to inflict on requests, which are all requests unless `fault_posts_only` is set.
    pub fault: Fault,
    /// If `true`, `fault` applies to `POST` requests only, so that clients can get their advertisement.
    pub fault_posts_only: bool,
    /// If set, `GET` requests below the old name are redirected to the new one with a `301`, like services do for repositories
    /// that were renamed, and all other requests to the old name fail with a `404`.
    pub moved: Option<Moved>,
    /// If set, connections are kept alive for further requests, and closed from the side of the peer after being idle for this long.
    /// By default, each connection serves one request and is closed right after, which is something that clients cannot trip over.
    pub keep_alive_idle: Option<Duration>,
    /// How to treat requests that look like they are meant for a proxy.
    pub proxy_mode: ProxyMode,
    /// The credentials to expect in `Proxy-Authorization` if `proxy_mode` is [`ProxyMode::Forward`].
    pub proxy_auth: Option<BasicAuth>,
}

/// A request that the peer received.
#[derive(Debug, Clone)]
pub struct Request {
    /// The HTTP method, like `GET`.
    pub method: String,
    /// The path and query as sent, which is an absolute URL if the request was meant for a proxy.
    pub target: String,
    /// Headers with lower-case names, in the order in which they were received.
    pub headers: Vec<(String, String)>,
    /// The number of the connection that the request arrived on, starting at `0`, which shows if clients reuse connections.
    pub connection: usize,
    /// The size of the (de-chunked) request body.
    pub body_len: usize,
    /// The status code of the response, or `0` if there was none.
    pub status: u16,
}

impl Request {
    /// Return the value of the first header called `name`, which has to be lower-case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_str()))
    }
}

/// A connection that has been accepted, and which may be wrapped, for instance with TLS.
pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// Wraps accepted connections, to terminate TLS for instance.
pub type Acceptor = Arc<dyn Fn(TcpStream) -> std::io::Result<Box<dyn Stream>> + Send + Sync>;

struct Shared {
    config: Config,
    requests: Mutex<Vec<Request>>,
    shutdown: AtomicBool,
    connections: std::sync::atomic::AtomicUsize,
    git_http_backend: Option<PathBuf>,
    acceptor: Option<Acceptor>,
}

/// A running peer. Dropping it stops it.
pub struct HttpPeer {
    addr: SocketAddr,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl HttpPeer {
    /// Start serving on a free port of the loopback interface.
    ///
    /// The location of `git http-backend` is determined now. Tests that put programs on `PATH` which record how Git is
    /// invoked have to start the peer before they do, as the lookup of Git's exec path would be recorded otherwise.
    pub fn start(config: Config) -> std::io::Result<Self> {
        Self::start_inner(config, None)
    }

    /// Like [`start()`](Self::start()), but pass each accepted connection through `acceptor` first.
    pub fn start_with_acceptor(config: Config, acceptor: Acceptor) -> std::io::Result<Self> {
        Self::start_inner(config, Some(acceptor))
    }

    fn start_inner(config: Config, acceptor: Option<Acceptor>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            config,
            requests: Mutex::default(),
            shutdown: AtomicBool::new(false),
            connections: std::sync::atomic::AtomicUsize::new(0),
            // Resolved now, so a changed `PATH` of the test can't affect what the peer runs.
            git_http_backend: gix_path::env::core_dir_program("git-http-backend"),
            acceptor,
        });
        let thread = std::thread::Builder::new().name("http-peer".into()).spawn({
            let shared = shared.clone();
            move || {
                for stream in listener.incoming() {
                    if shared.shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = shared.clone();
                    std::thread::spawn(move || {
                        handle_connection(&shared, stream).ok();
                    });
                }
            }
        })?;
        Ok(HttpPeer {
            addr,
            shared,
            thread: Some(thread),
        })
    }

    /// The address the peer is listening on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL of the repository `name`, like `repo.git`, as a client would use it.
    pub fn url(&self, name: &str) -> String {
        format!("http://{}/{name}", self.addr)
    }

    /// The URL of the peer itself, as `http.proxy` would use it.
    pub fn proxy_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// All requests received so far, in the order in which their headers were received.
    pub fn requests(&self) -> Vec<Request> {
        self.shared.requests.lock().clone()
    }
}

impl Drop for HttpPeer {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Relaxed);
        // Wake up the accept loop.
        TcpStream::connect(self.addr).ok();
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

fn wait_for_shutdown(shared: &Shared) {
    while !shared.shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn handle_connection(shared: &Shared, stream: TcpStream) -> std::io::Result<()> {
    let control = stream.try_clone()?;
    let mut stream: Box<dyn Stream> = match &shared.acceptor {
        Some(acceptor) => acceptor(stream)?,
        None => Box::new(stream),
    };
    let connection = shared.connections.fetch_add(1, Ordering::Relaxed);
    let reusable = std::cell::Cell::new(false);
    loop {
        reusable.set(false);
        handle_request(shared, &mut stream, &control, &reusable, connection)?;
        if !reusable.get() {
            return Ok(());
        }
    }
}

/// Serve a single request, and set `reusable` if the connection can serve another one.
fn handle_request(
    shared: &Shared,
    stream: &mut Box<dyn Stream>,
    control: &TcpStream,
    reusable: &std::cell::Cell<bool>,
    connection: usize,
) -> std::io::Result<()> {
    // Waiting for the next request on an idle connection can be limited, to have the connection closed from this side.
    control.set_read_timeout(Some(shared.config.keep_alive_idle.unwrap_or(Duration::from_secs(30))))?;
    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    control.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Ok(());
    };
    let (method, target) = (method.to_owned(), target.to_owned());
    let mut headers = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let index = {
        let mut requests = shared.requests.lock();
        requests.push(Request {
            method: method.clone(),
            target: target.clone(),
            headers: headers.clone(),
            connection,
            body_len: 0,
            status: 0,
        });
        requests.len() - 1
    };
    let header = |name: &str| {
        headers
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_str()))
    };

    let fault = if shared.config.fault_posts_only && method != "POST" {
        Fault::None
    } else {
        shared.config.fault
    };
    if fault == Fault::Silent || (fault == Fault::StopReadingRequestBody && method == "POST") {
        wait_for_shutdown(shared);
        return Ok(());
    }

    // The request is meant for a proxy if it carries an absolute URL.
    let path_and_query = match target.find("://") {
        Some(scheme_end) => target[scheme_end + 3..]
            .find('/')
            .map_or("/", |slash| &target[scheme_end + 3 + slash..])
            .to_owned(),
        None => target.clone(),
    };
    let keep_alive = shared.config.keep_alive_idle.is_some();
    let respond = |stream: &mut Box<dyn Stream>, status: u16, extra: &[String], body: &[u8]| -> std::io::Result<()> {
        shared.requests.lock()[index].status = status;
        let mut head = format!(
            "HTTP/1.1 {status} X\r\nConnection: {}\r\nContent-Length: {}\r\n",
            if keep_alive { "keep-alive" } else { "close" },
            body.len()
        );
        for header in extra {
            head.push_str(header);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(body)?;
        stream.flush()?;
        reusable.set(keep_alive);
        Ok(())
    };

    if shared.config.proxy_mode == ProxyMode::Forward
        && target.contains("://")
        && let Some(expected) = &shared.config.proxy_auth
        && !basic_auth_matches(header("proxy-authorization"), expected)
    {
        drain_unread_body(&mut reader, &headers);
        return respond(
            stream,
            407,
            &["Proxy-Authenticate: Basic realm=\"gix-test-proxy\"".into()],
            b"proxy authentication required",
        );
    }

    if let Some(moved) = &shared.config.moved
        && let Some(rest) = path_and_query.strip_prefix(&format!("/{}/", moved.from))
    {
        if method != "GET" {
            return respond(stream, 404, &[], b"not found");
        }
        let host = header("host").unwrap_or_default();
        return respond(
            stream,
            301,
            &[format!("Location: http://{host}/{}/{rest}", moved.to)],
            b"moved permanently",
        );
    }

    let is_advertisement = method == "GET"
        && path_and_query
            .split('?')
            .next()
            .is_some_and(|path| path.ends_with("/info/refs"));
    let is_push_advertisement = is_advertisement && path_and_query.contains("service=git-receive-pack");
    let is_push = method == "POST"
        && path_and_query
            .split('?')
            .next()
            .is_some_and(|path| path.ends_with("/git-receive-pack"));
    let needs_auth = match shared.config.protect {
        Protect::Nothing => false,
        Protect::InfoRefs | Protect::ReadOnlyCredentials => is_advertisement,
        Protect::Post => is_push,
        Protect::ReceivePack => is_push || is_push_advertisement,
        Protect::Everything => true,
    };
    let rejects_even_if_valid = shared.config.protect == Protect::ReadOnlyCredentials && is_push;
    let mut remote_user = "anonymous".to_owned();
    if needs_auth || rejects_even_if_valid {
        let valid = shared
            .config
            .auth
            .as_ref()
            .is_some_and(|expected| basic_auth_matches(header("authorization"), expected));
        if !valid || rejects_even_if_valid {
            drain_unread_body(&mut reader, &headers);
            return respond(
                stream,
                401,
                &["WWW-Authenticate: Basic realm=\"gix-test\"".into()],
                b"authentication required",
            );
        }
        remote_user = shared
            .config
            .auth
            .as_ref()
            .map(|a| a.username.clone())
            .unwrap_or(remote_user);
    }

    if header("expect").is_some_and(|value| value.eq_ignore_ascii_case("100-continue")) {
        reader.get_mut().write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    }
    let body = match read_body(&mut reader, header("transfer-encoding"), header("content-length")) {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
            return respond(stream, 400, &[], b"malformed request body");
        }
        Err(err) => return Err(err),
    };
    shared.requests.lock()[index].body_len = body.len();
    drop(reader);

    let Some(backend) = &shared.git_http_backend else {
        return respond(stream, 500, &[], b"git-http-backend is not available");
    };
    let (path, query) = path_and_query.split_once('?').unwrap_or((&path_and_query, ""));
    let mut cmd = Command::new(backend);
    crate::configure_git_environment(&mut cmd, &shared.config.project_root);
    cmd.env("GIT_PROJECT_ROOT", &shared.config.project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", &method)
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("REMOTE_USER", remote_user)
        .env("REMOTE_ADDR", "127.0.0.1")
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("CONTENT_TYPE", header("content-type").unwrap_or_default());
    if let Some(protocol) = header("git-protocol") {
        cmd.env("GIT_PROTOCOL", protocol).env("HTTP_GIT_PROTOCOL", protocol);
    }
    if let Some(encoding) = header("content-encoding") {
        cmd.env("HTTP_CONTENT_ENCODING", encoding);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("configured");
    let writer = std::thread::spawn(move || {
        stdin.write_all(&body).ok();
    });
    let mut output = Vec::new();
    child.stdout.take().expect("configured").read_to_end(&mut output)?;
    writer.join().ok();
    child.wait()?;

    let (headers, body) = split_cgi_output(&output);
    let mut status = 200;
    let mut extra = Vec::new();
    for header in headers {
        match header.split_once(':') {
            Some((name, value)) if name.eq_ignore_ascii_case("status") => {
                status = value
                    .split_whitespace()
                    .next()
                    .and_then(|code| code.parse().ok())
                    .unwrap_or(200);
            }
            Some((name, _)) if name.eq_ignore_ascii_case("content-length") => {}
            _ => extra.push(header.to_owned()),
        }
    }
    match fault {
        Fault::SlowResponse { chunk, interval } => {
            shared.requests.lock()[index].status = status;
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n",
                body.len()
            );
            for header in &extra {
                head.push_str(header);
                head.push_str("\r\n");
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes())?;
            stream.flush()?;
            for piece in body.chunks(chunk.max(1)) {
                stream.write_all(piece)?;
                stream.flush()?;
                std::thread::sleep(interval);
            }
            Ok(())
        }
        Fault::StallAfterHeaders => {
            shared.requests.lock()[index].status = status;
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n",
                body.len() + 1
            );
            for header in &extra {
                head.push_str(header);
                head.push_str("\r\n");
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes())?;
            stream.flush()?;
            wait_for_shutdown(shared);
            Ok(())
        }
        Fault::None | Fault::StopReadingRequestBody | Fault::Silent => respond(stream, status, &extra, body),
    }
}

/// Read and discard the body of a request that is rejected before it is used, unless the client waits for `100 Continue`
/// and so doesn't send it. Closing a connection with unread data makes the kernel reset it, which lets clients that send
/// their body right away fail with a reset connection rather than see the status that explains the rejection.
fn drain_unread_body(reader: &mut impl BufRead, headers: &[(String, String)]) {
    let header = |name: &str| {
        headers
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_str()))
    };
    if header("expect").is_none() {
        read_body(reader, header("transfer-encoding"), header("content-length")).ok();
    }
}

fn basic_auth_matches(header: Option<&str>, expected: &BasicAuth) -> bool {
    let Some(encoded) = header.and_then(|value| value.strip_prefix("Basic ")) else {
        return false;
    };
    base64_decode(encoded.trim())
        .is_some_and(|decoded| decoded == format!("{}:{}", expected.username, expected.password).as_bytes())
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut buffer, mut bits) = (0u32, 0);
    for byte in input.bytes().take_while(|b| *b != b'=') {
        buffer = (buffer << 6) | ALPHABET.iter().position(|c| *c == byte)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn read_body(
    reader: &mut impl BufRead,
    transfer_encoding: Option<&str>,
    content_length: Option<&str>,
) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();
    if transfer_encoding.is_some_and(|value| value.eq_ignore_ascii_case("chunked")) {
        let mut line = String::new();
        loop {
            line.clear();
            reader.read_line(&mut line)?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or_default(), 16)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed chunk size"))?;
            if size == 0 {
                // Trailers and the final empty line.
                loop {
                    line.clear();
                    if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                break;
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..])?;
            let mut crlf = [0; 2];
            reader.read_exact(&mut crlf)?;
            if &crlf != b"\r\n" {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "chunk isn't terminated by CRLF",
                ));
            }
        }
    } else if let Some(length) = content_length.and_then(|value| value.parse::<usize>().ok()) {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    }
    Ok(body)
}

fn split_cgi_output(output: &[u8]) -> (Vec<&str>, &[u8]) {
    let (head_end, body_start) = output
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| (pos, pos + 4))
        .or_else(|| {
            output
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|pos| (pos, pos + 2))
        })
        .unwrap_or((output.len(), output.len()));
    let headers = std::str::from_utf8(&output[..head_end])
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .collect();
    (headers, &output[body_start..])
}
