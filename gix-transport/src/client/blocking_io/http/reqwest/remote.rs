use std::{
    any::Any,
    io::{Read, Write},
    pin::Pin,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use gix_error::{Class, Error, ErrorExt, OptionExt, Result, ResultExt, message};
use gix_utils::io::pipe;
use parking_lot::Mutex;

use crate::client::blocking_io::http::{
    self,
    options::FollowRedirects,
    redirect::{self, Action as RedirectAction},
    reqwest::Remote,
    traits::PostBodyDataKind,
};

fn classify_reqwest(err: reqwest::Error) -> Error {
    let class = match err.status() {
        Some(reqwest::StatusCode::FORBIDDEN) => Some(Class::PermissionDenied),
        Some(reqwest::StatusCode::CONFLICT) => Some(Class::Conflict),
        Some(reqwest::StatusCode::NOT_IMPLEMENTED) => Some(Class::Unsupported),
        _ if err.is_timeout() || err.is_connect() || err.status().is_some_and(|status| status.is_server_error()) => {
            Some(Class::Retryable)
        }
        _ => None,
    };
    match class {
        Some(class) => Error::from_error(gix_error::ClassificationMarker::with_source(class, err)),
        None => Error::from_error(err),
    }
}

fn authority_changed(curr_url: &reqwest::Url, prev_url: &reqwest::Url) -> bool {
    curr_url.scheme() != prev_url.scheme()
        || curr_url.host_str() != prev_url.host_str()
        || curr_url.port_or_known_default() != prev_url.port_or_known_default()
}

impl Default for Remote {
    fn default() -> Self {
        let (req_send, req_recv) = tokio::sync::mpsc::channel(1);
        let (res_send, res_recv) = tokio::sync::mpsc::channel(1);
        let redirected_base_url_shared = Arc::new(Mutex::new(None));
        let redirected_base_url_shared_for_field = redirected_base_url_shared.clone();
        let handle = std::thread::spawn(gix_trace::in_thread(move || -> Result {
            // One runtime that is driven for as long as the worker lives, as connections that idle in the pool
            // have to be polled to notice that the server closed them. Otherwise, the next request could pick a
            // dead connection up, and a `POST` can't be repeated.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .or_raise(|| message("Could not start the runtime of the HTTP client"))?;
            runtime.block_on(serve(req_recv, res_send, redirected_base_url_shared))
        }));

        Remote {
            handle: Some(handle),
            request: req_send,
            response: res_recv,
            config: http::Options::default(),
            redirected_base_url: redirected_base_url_shared_for_field,
        }
    }
}

/// The time to wait for a connection to be established if nothing else is configured, like `curl` does.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(300);

/// The credentials of an authenticating proxy, which are stored once the proxy accepted them, and erased when it didn't.
struct ProxyCredentials {
    next: gix_credentials::helper::NextAction,
    authenticate: Arc<std::sync::Mutex<http::options::AuthenticateFn>>,
    stored: bool,
}

impl ProxyCredentials {
    fn update(&self, action: gix_credentials::helper::Action) -> Result {
        (self.authenticate.lock().expect("no panics in other threads"))(action)
            .or_raise(|| message("Could not update proxy credentials"))?;
        Ok(())
    }
}

/// `reqwest` has no crypto provider of its own with this feature, so name one unless the application chose another.
#[cfg(all(
    feature = "http-client-reqwest-rust-tls-ring",
    not(any(
        feature = "http-client-reqwest-rust-tls",
        feature = "http-client-reqwest-rust-tls-trust-dns"
    ))
))]
fn install_default_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        // Failing means that another thread installed one in the meantime, which is just as good.
        rustls::crypto::ring::default_provider().install_default().ok();
    }
}

#[cfg(not(all(
    feature = "http-client-reqwest-rust-tls-ring",
    not(any(
        feature = "http-client-reqwest-rust-tls",
        feature = "http-client-reqwest-rust-tls-trust-dns"
    ))
)))]
fn install_default_crypto_provider() {}

/// Apply the CA bundle, the verification and the protocol versions that were configured, which `curl` takes in the same way:
/// a CA bundle replaces the certificates that are trusted otherwise, and no verification also accepts any host name.
#[cfg(any(
    feature = "http-client-reqwest-rust-tls",
    feature = "http-client-reqwest-rust-tls-ring",
    feature = "http-client-reqwest-rust-tls-trust-dns",
    feature = "http-client-reqwest-native-tls"
))]
fn configure_tls(mut builder: reqwest::ClientBuilder, config: &http::Options) -> Result<reqwest::ClientBuilder> {
    use http::options::SslVersion;
    if let Some(path) = &config.ssl_ca_info {
        let pem = std::fs::read(path).or_raise(|| message!("Could not read the CA bundle at {}", path.display()))?;
        let certificates = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(classify_reqwest)
            .or_raise(|| message!("The CA bundle at {} is not valid", path.display()))?;
        builder = builder.tls_certs_only(certificates);
    }
    if !config.ssl_verify {
        builder = builder
            .tls_danger_accept_invalid_certs(true)
            .tls_danger_accept_invalid_hostnames(true);
    }
    if let Some(versions) = config.ssl_version {
        let (min, max) = versions.min_max();
        let convert = |version: SslVersion| -> Result<Option<reqwest::tls::Version>> {
            Ok(Some(match version {
                SslVersion::Default => return Ok(None),
                SslVersion::TlsV1 | SslVersion::TlsV1_0 => reqwest::tls::Version::TLS_1_0,
                SslVersion::TlsV1_1 => reqwest::tls::Version::TLS_1_1,
                SslVersion::TlsV1_2 => reqwest::tls::Version::TLS_1_2,
                SslVersion::TlsV1_3 => reqwest::tls::Version::TLS_1_3,
                SslVersion::SslV2 | SslVersion::SslV3 => {
                    return Err(message!("The `reqwest` backend doesn't support {version:?}").raise());
                }
            }))
        };
        if let Some(min_version) = convert(min)? {
            builder = builder.tls_version_min(min_version);
        }
        // A single version, which is what `http.sslVersion` configures, is a minimum as `curl` and `git` take it.
        if min != max
            && let Some(max_version) = convert(max)?
        {
            builder = builder.tls_version_max(max_version);
        }
    }
    Ok(builder)
}

/// Without a TLS implementation there are no `https://` connections to configure.
#[cfg(not(any(
    feature = "http-client-reqwest-rust-tls",
    feature = "http-client-reqwest-rust-tls-ring",
    feature = "http-client-reqwest-rust-tls-trust-dns",
    feature = "http-client-reqwest-native-tls"
)))]
fn configure_tls(builder: reqwest::ClientBuilder, _config: &http::Options) -> Result<reqwest::ClientBuilder> {
    Ok(builder)
}

fn build_client(
    config: &http::Options,
    redirect_action: &Arc<Mutex<RedirectAction>>,
    redirect_tail: &Arc<Mutex<String>>,
) -> Result<(reqwest::Client, Option<ProxyCredentials>)> {
    // There is no total timeout: how long a transfer may take is up to the low-speed limits, as with `curl` and `git`.
    let mut builder = reqwest::ClientBuilder::new()
        .connect_timeout(config.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT))
        .http1_title_case_headers();
    if let Some(user_agent) = &config.user_agent {
        builder = builder.user_agent(user_agent);
    }
    if config.http_version == Some(http::options::HttpVersion::V1_1) {
        builder = builder.http1_only();
    }
    install_default_crypto_provider();
    builder = configure_tls(builder, config)?;
    let mut proxy_credentials = None;
    // `reqwest` reads the proxy variables of the environment unless told not to, which would bypass the configuration:
    // it turns these variables into `http::Options` if it is allowed to, and an empty `http.proxy` is how `git` switches
    // all proxies off. (`curl` reads them itself whenever it isn't given a proxy, which is a known difference.)
    builder = builder.no_proxy();
    if let Some(proxy_url) = config.proxy.as_deref().filter(|url| !url.is_empty()) {
        use http::options::ProxyAuthMethod::*;
        if !matches!(config.proxy_auth_method, AnyAuth | Basic) {
            return Err(message!(
                "The `reqwest` backend only supports basic authentication with proxies, but {:?} was configured",
                config.proxy_auth_method
            )
            .raise());
        }
        // The credentials are applied below, and the ones in the URL stay out of messages.
        let proxy_url = if config.proxy_authenticate.is_some() {
            super::super::without_userinfo(proxy_url)
        } else {
            proxy_url.to_owned()
        };
        let mut proxy = reqwest::Proxy::all(&proxy_url)
            .map_err(classify_reqwest)
            .or_raise(|| message!("The proxy {proxy_url:?} is not valid"))?;
        if let Some((action, authenticate)) = config.proxy_authenticate.clone() {
            let credentials = (authenticate.lock().expect("no panics in other threads"))(action)
                .or_raise(|| message("Could not obtain proxy credentials"))?
                .ok_or_raise(|| message("The credential helpers provided no credentials for the proxy"))?;
            proxy = proxy.basic_auth(&credentials.identity.username, &credentials.identity.password);
            proxy_credentials = Some(ProxyCredentials {
                next: credentials.next,
                authenticate,
                stored: false,
            });
        }
        if let Some(no_proxy) = config.no_proxy.as_deref() {
            proxy = proxy.no_proxy(reqwest::NoProxy::from_string(no_proxy));
        }
        builder = builder.proxy(proxy);
    }
    if let Some(backend) = config.backend.as_ref() {
        let mut backend = backend
            .lock()
            .map_err(|_| message("HTTP backend configuration lock is poisoned").raise())?;
        if let Some(options) = backend.downcast_mut::<super::Options>()
            && let Some(configure_client) = options.configure_client.as_mut()
        {
            builder = configure_client(builder).or_raise(|| message("HTTP client configuration failed"))?;
        }
    }
    // Keep Git's redirect policy authoritative after client customization.
    let client = builder
        .redirect(reqwest::redirect::Policy::custom({
            let redirect_action = redirect_action.clone();
            let redirect_tail = redirect_tail.clone();
            move |attempt| match *redirect_action.lock() {
                RedirectAction::Follow => {
                    let curr_url = attempt.url();
                    let prev_urls = attempt.previous();
                    // emulate default git behaviour which relies on curl default behaviour apparently.
                    const CURL_DEFAULT_REDIRS: usize = 50;
                    if prev_urls.len() >= CURL_DEFAULT_REDIRS {
                        return attempt.error("too many redirects");
                    }

                    match prev_urls.last() {
                        Some(prev_url) if !redirect::scheme_is_safe(curr_url.as_str(), prev_url.as_str()) => {
                            // Don't follow insecure protocol redirects, particularly https-to-http downgrades.
                            attempt.stop()
                        }
                        Some(prev_url) if authority_changed(curr_url, prev_url) => {
                            // Allowed only if the tail doesn't change.
                            let redirect_tail = redirect_tail.lock();
                            if curr_url.as_str().ends_with(redirect_tail.as_str()) {
                                attempt.follow()
                            } else {
                                let curr_url = curr_url.as_str().to_owned();
                                let redirect_tail = redirect_tail.to_string();
                                attempt.error(format!(
                                    "redirect url {curr_url:?} does not end with expected request suffix {redirect_tail:?}",
                                ))
                            }
                        }
                        _ => attempt.follow(),
                    }
                }
                RedirectAction::RejectConfiguredHeaders => {
                    attempt.error("refusing to follow redirect after request headers were configured")
                }
                RedirectAction::Stop => attempt.stop(),
            }
        }))
        .build()
        .map_err(classify_reqwest)
        .or_raise(|| message("Could not initialize HTTP client"))?;
    Ok((client, proxy_credentials))
}

/// A request body that is read from a pipe by a thread that blocks on it, so the pipe can be written to by blocking code.
struct UploadStream {
    chunks: tokio::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    meter: Arc<Meter>,
}

impl futures_core::Stream for UploadStream {
    type Item = std::io::Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let poll = self.chunks.poll_recv(cx);
        match &poll {
            Poll::Ready(Some(Ok(chunk))) => {
                self.meter.up.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            }
            Poll::Ready(None) => self.meter.request_body_sent.store(true, Ordering::Relaxed),
            _ => {}
        }
        poll
    }
}

/// Evaluate `result`, and if it's an error, make the receiver of the response see it before the worker stops, rather
/// than headers and a body that are empty without a reason.
macro_rules! or_report {
    ($headers:expr, $result:expr) => {
        match $result {
            Ok(value) => value,
            Err(err) => {
                $headers
                    .channel
                    .send(Err(std::io::Error::other(
                        gix_error::message!("The HTTP worker failed and stops: {err:?}").raise(),
                    )))
                    .ok();
                return Err(err);
            }
        }
    };
}

/// What has been transferred of the current request, which is what the low-speed limits look at.
#[derive(Default)]
struct Meter {
    /// The bytes of the request body that were handed to the connection.
    up: AtomicU64,
    /// The bytes of the response body that were received.
    down: AtomicU64,
    /// `true` once the entire request body was handed to the connection.
    request_body_sent: AtomicBool,
}

/// The low-speed limits of `curl`: a transfer fails if it moves fewer than `limit` bytes per second for `time`.
#[derive(Clone, Copy)]
struct LowSpeed {
    limit: u32,
    time: Duration,
}

impl LowSpeed {
    fn from_options(config: &http::Options) -> Option<Self> {
        (config.low_speed_limit_bytes_per_second > 0 && config.low_speed_time_seconds > 0).then_some(LowSpeed {
            limit: config.low_speed_limit_bytes_per_second,
            time: Duration::from_secs(config.low_speed_time_seconds),
        })
    }

    /// Watch `meter` each second, and return the error to fail the transfer with once it's been too slow for too long.
    ///
    /// This follows `curl`. The speed is the larger of the bytes sent and received per second, averaged over the last five seconds.
    /// The clock of the limit starts with the first sample that is too slow, and stops with a sample that is fast enough.
    /// The time in which the runtime can't run, which is the time that is spent waiting for the consumer of the response, shows up
    /// as a single slow sample. As with `curl`, that alone doesn't end the transfer, as the data that was held back
    /// flows again after it.
    /// Never returns if the transfer is fast enough, which is why it is meant to be raced against it.
    async fn watch(self, meter: &Meter) -> std::io::Error {
        const WINDOW: usize = 5;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut samples = std::collections::VecDeque::from([(Instant::now(), 0u64, 0u64)]);
        ticker.tick().await; // the first tick is immediate
        // When the speed was first too low, and for how many samples that has been the case since.
        let mut slow: Option<(Instant, u64)> = None;
        loop {
            ticker.tick().await;
            let now = Instant::now();
            let (up, down) = (meter.up.load(Ordering::Relaxed), meter.down.load(Ordering::Relaxed));
            let &(oldest, oldest_up, oldest_down) = samples.front().expect("at least one sample");
            let moved = (up - oldest_up).max(down - oldest_down) as f64;
            let speed = moved / now.duration_since(oldest).as_secs_f64();
            samples.push_back((now, up, down));
            if samples.len() > WINDOW {
                samples.pop_front();
            }
            if speed < f64::from(self.limit) {
                let (since, samples_since) = slow.get_or_insert((now, 0));
                *samples_since += 1;
                // Samples are a second apart, but the time between them is a little off, which could let a sample
                // that is due be missed by a hair, so both ways of counting are used.
                if now.duration_since(*since) >= self.time || *samples_since > self.time.as_secs() {
                    let err = gix_error::ClassificationMarker::with_source(
                        Class::Retryable,
                        std::io::Error::from(std::io::ErrorKind::TimedOut),
                    )
                    .and_raise(message!(
                        "The transfer was slower than {} bytes per second for {} seconds, and {}",
                        self.limit,
                        self.time.as_secs(),
                        if meter.request_body_sent.load(Ordering::Relaxed) {
                            "the entire request had been handed to the connection"
                        } else {
                            "the request had not been handed to the connection entirely"
                        }
                    ));
                    return std::io::Error::new(std::io::ErrorKind::TimedOut, err);
                }
            } else {
                slow = None;
            }
        }
    }
}

/// Move everything from `reader` to `sender` until either side is done, which is what makes a request body that is
/// written by a thread that isn't driven by the runtime available to it, with a bounded amount of buffering.
fn pump_upload(mut reader: pipe::Reader, sender: tokio::sync::mpsc::Sender<std::io::Result<Vec<u8>>>) {
    let mut buf = vec![0; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => {
                if sender.blocking_send(Ok(buf[..read].to_vec())).is_err() {
                    break;
                }
            }
            Err(err) => {
                sender.blocking_send(Err(err)).ok();
                break;
            }
        }
    }
}

/// Perform the requests that arrive over `requests`, until the sender goes away or the other side of `responses` does.
///
/// The expected order for each request is that the receiver of its [`Response`]
/// - writes `upload_body`, and drops it,
/// - reads `headers` to the end, and
/// - reads `body` to the end.
async fn serve(
    mut requests: tokio::sync::mpsc::Receiver<Request>,
    responses: tokio::sync::mpsc::Sender<Response>,
    redirected_base_url_shared: Arc<Mutex<Option<String>>>,
) -> Result {
    let mut follow = None;
    let redirect_action = Arc::new(Mutex::new(RedirectAction::Stop));
    let redirect_tail = Arc::new(Mutex::new(String::new()));

    let mut client = None;
    let mut proxy_credentials: Option<ProxyCredentials> = None;
    let mut discard_client = false;

    while let Some(Request {
        url,
        base_url,
        headers,
        upload_body_kind,
        config,
    }) = requests.recv().await
    {
        if std::mem::take(&mut discard_client) {
            client = None;
        }
        if client.is_none() {
            let (new_client, credentials) = build_client(&config, &redirect_action, &redirect_tail)?;
            client = Some(new_client);
            proxy_credentials = credentials;
        }
        let client = client.as_ref().expect("initialized before making requests");
        let redirected_base_url = redirected_base_url_shared.lock().clone();
        let effective_url = redirect::swap_tails(redirected_base_url.as_deref(), &base_url, url.clone());
        let has_configured_extra_headers = !config.extra_headers.is_empty();
        let mut req_builder = if upload_body_kind.is_some() {
            client.post(&effective_url)
        } else {
            client.get(&effective_url)
        }
        .headers(headers);
        let meter = Arc::new(Meter::default());
        let (post_body_tx, mut post_body_rx) = pipe::unidirectional(0);
        let (mut response_body_tx, response_body_rx) = pipe::unidirectional(0);
        let (mut headers_tx, headers_rx) = pipe::unidirectional(0);
        if responses
            .send(Response {
                headers: headers_rx,
                body: response_body_rx,
                upload_body: post_body_tx,
            })
            .await
            .is_err()
        {
            // This means our internal protocol is violated as the one who sent the request isn't listening anymore.
            // Shut down as something is off.
            break;
        }
        req_builder = match upload_body_kind {
            Some(PostBodyDataKind::BoundedAndFitsIntoMemory) => {
                // It's sent with a `Content-Length`, as some servers and proxies don't handle chunked requests.
                let mut buf = Vec::<u8>::with_capacity(512);
                or_report!(
                    headers_tx,
                    post_body_rx
                        .read_to_end(&mut buf)
                        .or_raise(|| message("Could not finish reading all data to post to the remote"))
                );
                meter.up.fetch_add(buf.len() as u64, Ordering::Relaxed);
                meter.request_body_sent.store(true, Ordering::Relaxed);
                req_builder.body(buf)
            }
            Some(PostBodyDataKind::Unbounded) => {
                let (sender, receiver) = tokio::sync::mpsc::channel(1);
                std::thread::spawn(move || pump_upload(post_body_rx, sender));
                req_builder.body(reqwest::Body::wrap_stream(UploadStream {
                    chunks: receiver,
                    meter: meter.clone(),
                }))
            }
            None => {
                meter.request_body_sent.store(true, Ordering::Relaxed);
                req_builder
            }
        };
        let mut req = or_report!(
            headers_tx,
            req_builder
                .build()
                .map_err(classify_reqwest)
                .or_raise(|| message("Could not build HTTP request"))
        );
        let mut has_configure_request = false;
        if let Some(ref mut request_options) = config.backend.as_ref().and_then(|backend| backend.lock().ok())
            && let Some(options) = request_options.downcast_mut::<super::Options>()
            && let Some(configure_request) = &mut options.configure_request
        {
            has_configure_request = true;
            or_report!(
                headers_tx,
                configure_request(&mut req).or_raise(|| message("Request configuration failed"))
            );
        }

        let follow = follow.get_or_insert(config.follow_redirects);
        let may_follow_redirects = matches!(*follow, FollowRedirects::Initial | FollowRedirects::All);
        let has_configured_request_headers = has_configure_request || has_configured_extra_headers;
        *redirect_action.lock() = RedirectAction::from_request(may_follow_redirects, has_configured_request_headers);
        url.strip_prefix(&base_url)
            .expect("BUG: caller assures `base_url` is subset of `url`")
            .clone_into(&mut redirect_tail.lock());

        if *follow == FollowRedirects::Initial {
            *follow = FollowRedirects::None;
        }

        // The watchdog is raced against everything that waits for the server, so that a transfer that stalls ends, and the
        // futures that wait are dropped, which closes the connection.
        // This starts after the body of the request was read from the caller, which is local, and not what the limits are for.
        let low_speed = LowSpeed::from_options(&config);
        let mut watchdog = std::pin::pin!(async {
            match low_speed {
                Some(low_speed) => low_speed.watch(&meter).await,
                None => std::future::pending().await,
            }
        });
        let mut www_authenticate = Vec::new();
        let sent = tokio::select! {
            biased;
            sent = client.execute(req) => sent,
            err = &mut watchdog => {
                headers_tx.channel.send(Err(err)).ok();
                continue;
            }
        };
        let mut res = match sent.and_then(|res| {
            if res.status() == reqwest::StatusCode::UNAUTHORIZED {
                www_authenticate = res
                    .headers()
                    .get_all(reqwest::header::WWW_AUTHENTICATE)
                    .iter()
                    .map(|value| value.as_bytes().into())
                    .collect();
            }
            res.error_for_status()
        }) {
            Ok(res) => res,
            Err(err) => {
                if err.status() == Some(reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED) {
                    // The proxy didn't accept what it was given, so forget it and ask again with the next request. A handshake
                    // that fails with this error isn't retried here, so it can't loop: the caller decides whether to try again.
                    if let Some(credentials) = proxy_credentials.take() {
                        credentials.update(credentials.next.clone().erase())?;
                    }
                    discard_client = true;
                }
                // `error_for_status()` preserves the final URL for HTTP error responses. Capture it here so
                // authentication retries after redirected 401 responses use the redirected base URL.
                if let Some(actual_url) = err.url().map(reqwest::Url::as_str)
                    && actual_url != effective_url
                {
                    let new_base_url = or_report!(headers_tx, redirect::base_url(actual_url, &base_url, url.clone()));
                    *redirected_base_url_shared.lock() = Some(new_base_url);
                }
                let err = match err.status() {
                    Some(reqwest::StatusCode::UNAUTHORIZED) => std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        crate::client::AuthenticationRequired { www_authenticate },
                    ),
                    Some(status) => {
                        let kind = if status.is_server_error() && status != reqwest::StatusCode::NOT_IMPLEMENTED {
                            std::io::ErrorKind::ConnectionAborted
                        } else {
                            std::io::ErrorKind::Other
                        };
                        std::io::Error::new(
                            kind,
                            classify_reqwest(err).and_raise(message!("Received HTTP status {}", status.as_str())),
                        )
                    }
                    // Preserve the `reqwest::Error` as the source so the underlying cause -- e.g. a
                    // connection or TLS failure -- isn't lost. It was previously stringified, which
                    // dead-ended `source()` and hid the real reason a request failed. See #2140.
                    None => std::io::Error::other(classify_reqwest(err)),
                };
                headers_tx.channel.send(Err(err)).ok();
                continue;
            }
        };

        if let Some(credentials) = proxy_credentials.as_mut().filter(|credentials| !credentials.stored) {
            credentials.update(credentials.next.clone().store())?;
            credentials.stored = true;
        }
        let actual_url = res.url().as_str();
        if actual_url != effective_url.as_str() {
            let new_base_url = or_report!(headers_tx, redirect::base_url(actual_url, &base_url, url));
            *redirected_base_url_shared.lock() = Some(new_base_url);
        }

        // We don't have to care if anybody is receiving the header, as a matter of fact we cannot fail sending them.
        // Thus an error means the receiver failed somehow, but might also have decided not to read headers at all. Fine with us.
        (|| -> std::io::Result<()> {
            for (name, value) in res.headers() {
                headers_tx.write_all(name.as_str().as_bytes())?;
                headers_tx.write_all(b":")?;
                headers_tx.write_all(value.as_bytes())?;
                headers_tx.write_all(b"\n")?;
            }
            Ok(())
        })()
        .ok();
        // Dropping the writer signals the reader that all headers are there.
        drop(headers_tx);

        // Reading the response body is streaming and may fail for many reasons. If so, we send the error over the response
        // body channel and that's all we can do.
        loop {
            let chunk = tokio::select! {
                biased;
                chunk = res.chunk() => chunk,
                err = &mut watchdog => {
                    response_body_tx.channel.send(Err(err)).ok();
                    break;
                }
            };
            match chunk {
                Ok(Some(chunk)) => {
                    meter.down.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    // This blocks the runtime for as long as the consumer doesn't take the data, which is intended: nothing else
                    // runs meanwhile, and a consumer that stalls is a stalled transfer for the low-speed limits, as with `curl`
                    // whose callback blocks in the same way. Handing this off to another thread would change that.
                    // With HTTP/2 the other streams of the connection, its pings and its flow control wait as well, which
                    // only matters for stalls so short that the low-speed limits don't end them.
                    if response_body_tx.write_all(&chunk).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    response_body_tx
                        .channel
                        .send(Err(std::io::Error::other(classify_reqwest(err))))
                        .ok();
                    break;
                }
            }
        }
    }
    Ok(())
}

/// utilities
impl Remote {
    fn restore_thread_after_failure(&mut self) -> Error {
        let err_that_brought_thread_down = self
            .handle
            .take()
            .expect("thread handle present")
            .join()
            .expect("handler thread should never panic")
            .expect_err("something should have gone wrong with curl (we join on error only)");
        let config = std::mem::take(&mut self.config);
        *self = Remote::default();
        self.config = config;
        err_that_brought_thread_down.and_raise(message("Could not initialize the http client"))
    }

    fn make_request(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        upload_body_kind: Option<PostBodyDataKind>,
    ) -> Result<http::PostResponse<pipe::Reader, pipe::Reader, pipe::Writer>> {
        let mut header_map = reqwest::header::HeaderMap::new();
        for header_line in headers {
            insert_header(&mut header_map, header_line.as_ref());
        }
        for header_line in &self.config.extra_headers {
            insert_header(&mut header_map, header_line);
        }
        if self
            .request
            .blocking_send(Request {
                url: url.to_owned(),
                base_url: base_url.to_owned(),
                headers: header_map,
                upload_body_kind,
                config: self.config.clone(),
            })
            .is_err()
        {
            return Err(self.restore_thread_after_failure());
        }

        let Response {
            headers,
            body,
            upload_body,
        } = match self.response.blocking_recv() {
            Some(res) => res,
            None => {
                return Err(self.restore_thread_after_failure());
            }
        };

        Ok(http::PostResponse {
            post_body: upload_body,
            headers,
            body,
        })
    }
}

/// Add one `name: value` header line to `header_map`, ignoring malformed or unsupported input in `header_line`.
///
/// Git configuration may provide arbitrary extra header lines, so invalid names or values are skipped instead of
/// failing request construction. Multiple entries with the same header name are preserved to match curl behavior.
fn insert_header(header_map: &mut reqwest::header::HeaderMap, header_line: &str) {
    let Some(colon_pos) = header_line.find(':') else {
        return;
    };
    let header_name = &header_line[..colon_pos];
    let value = &header_line[colon_pos + 1..];

    if let Some((key, val)) = reqwest::header::HeaderName::from_str(header_name)
        .ok()
        .zip(reqwest::header::HeaderValue::try_from(value.trim()).ok())
    {
        header_map.append(key, val);
    }
}

impl http::Http for Remote {
    type Headers = pipe::Reader;
    type ResponseBody = pipe::Reader;
    type PostBody = pipe::Writer;

    fn get(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<http::GetResponse<Self::Headers, Self::ResponseBody>> {
        self.make_request(url, base_url, headers, None).map(Into::into)
    }

    fn post(
        &mut self,
        url: &str,
        base_url: &str,
        headers: impl IntoIterator<Item = impl AsRef<str>>,
        post_body_kind: PostBodyDataKind,
    ) -> Result<http::PostResponse<Self::Headers, Self::ResponseBody, Self::PostBody>> {
        self.make_request(url, base_url, headers, Some(post_body_kind))
    }

    fn configure(&mut self, config: &dyn Any) -> Result {
        if let Some(config) = config.downcast_ref::<http::Options>() {
            self.config = config.clone();
        }
        Ok(())
    }

    fn redirected_base_url(&self) -> Option<String> {
        self.redirected_base_url.lock().clone()
    }
}

pub(crate) struct Request {
    pub url: String,
    pub base_url: String,
    pub headers: reqwest::header::HeaderMap,
    pub upload_body_kind: Option<PostBodyDataKind>,
    pub config: http::Options,
}

/// A link to a thread who provides data for the contained readers.
/// The expected order is:
/// - write `upload_body`
/// - read `headers` to end
/// - read `body` to hend
pub(crate) struct Response {
    pub headers: pipe::Reader,
    pub body: pipe::Reader,
    pub upload_body: pipe::Writer,
}
