//! A transport to a repository on the same machine that serves it from within the process, without starting
//! `git-receive-pack` or `git-upload-pack`.
//!
//! The server runs on a thread of its own and talks to the client through pipes, so that what is exchanged is the same as with a
//! process that was started: the client gets the same advertisement and has to send the same requests, and what the server
//! does after it replied, like running hooks, still happens after the client got its answer.
//!
//! Use it by setting `gitoxide.core.nativeLocalTransport` or by creating it with [`NativeLocal::new()`]. Repositories that
//! can't be served, because they have hooks or settings that aren't supported, fail with an error whose kind is
//! [`std::io::ErrorKind::Unsupported`] before anything is exchanged, and they aren't served by a program instead.
use std::{
    any::Any,
    borrow::Cow,
    io::{PipeReader, PipeWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use gix_error::{ErrorExt, ResultExt, message};
use gix_transport::{
    Protocol, Service,
    client::{
        self, MessageKind, TransportWithoutIO, WriteMode,
        blocking_io::{RequestWriter, SetServiceResponse, Transport},
        git::{ConnectMode, blocking_io::Connection},
    },
};

use crate::bstr::{BStr, BString};
#[cfg(feature = "receive")]
use crate::receive::Receiver;

/// What the client end of the connection sends to the server, which can be closed from other threads to end the server's wait.
#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Option<PipeWriter>>>);

impl SharedWriter {
    fn close_unless_writing(&self) {
        if let Ok(mut writer) = self.0.try_lock() {
            writer.take();
        }
    }

    fn close(&self) {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            Some(writer) => writer.write(buf),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the connection was closed",
            )),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            Some(writer) => writer.flush(),
            None => Ok(()),
        }
    }
}

/// The server thread, and what it needs to be interrupted.
struct Server {
    interrupted: AtomicBool,
    input: SharedWriter,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Server {
    fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
        // The server is waiting for the client if it isn't busy, and it ends if there is nothing more to read. A write of the client that
        // blocks because the server doesn't read holds the lock, and can't be ended from here, but that mustn't block the interrupt too:
        // the server sees the flag as soon as it looks.
        self.input.close_unless_writing();
    }

    /// Wait for the server thread to end for at most `deadline` and return `true` if it did, which also joins it.
    fn wait(&self, deadline: Duration) -> std::thread::Result<bool> {
        let start = Instant::now();
        let mut pause = Duration::from_millis(1);
        loop {
            {
                let mut thread = self.thread.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                match thread.as_ref() {
                    None => return Ok(true),
                    Some(handle) if handle.is_finished() => {
                        return thread.take().map_or(Ok(true), |handle| handle.join().map(|()| true));
                    }
                    Some(_) => {}
                }
            }
            let remaining = deadline.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Ok(false);
            }
            std::thread::sleep(pause.min(remaining));
            pause = (pause * 2).min(Duration::from_millis(25));
        }
    }
}

/// A connection to a repository on this machine that is served by this process.
///
/// Dropping it ends the connection and waits for the server for up to 5 seconds, or what [`with_drop_grace()`](Self::with_drop_grace()) says. A server that is still busy then, for example
/// with a hook that runs after the client got its answer, is interrupted, which stops the maintenance that follows a push at its
/// next step, and waited for for up to 5 seconds more, though it doesn't stop a hook that is running. What is still busy after that is left running on its thread until it ends,
/// and it can't be killed as a program could. Use [`wait_for_server()`](TransportWithoutIO::wait_for_server()) to wait for it, which `Connection`
/// does when a push finishes.
///
/// [Interrupting](TransportWithoutIO::interrupt_handle()) makes the transport fail from then on, and ends the server if it
/// waits for the client. It can't end a server that is busy, and a client that is blocked because the server doesn't read what
/// it sends is only released when the server reads, or ends.
pub struct NativeLocal {
    git_dir: PathBuf,
    url: gix_url::Url,
    desired_version: Protocol,
    trace: bool,
    connection: Option<Connection<PipeReader, SharedWriter>>,
    server: Arc<Server>,
    drop_grace: Duration,
}

impl NativeLocal {
    /// Create a transport that serves the repository whose Git directory is at `git_dir`, and speaks `desired_version`
    /// of the protocol if the service does. If `trace` is `true`, all packetlines are passed to `gix-trace`.
    ///
    /// Nothing is opened or started until the handshake.
    pub fn new(git_dir: impl Into<PathBuf>, desired_version: Protocol, trace: bool) -> Self {
        let git_dir = git_dir.into();
        let path = BString::from(git_dir.to_string_lossy().into_owned());
        NativeLocal {
            url: gix_url::Url::from_parts(gix_url::Scheme::File, None, None, None, None, path, true)
                .expect("a file url is valid"),
            git_dir,
            desired_version,
            trace,
            connection: None,
            server: Arc::new(Server {
                interrupted: AtomicBool::new(false),
                input: SharedWriter(Arc::new(Mutex::new(None))),
                thread: Mutex::new(None),
            }),
            drop_grace: Duration::from_secs(5),
        }
    }

    /// Set how long dropping the transport waits for a server that is still busy, and then again after it interrupted that server,
    /// which is 5 seconds by default.
    pub fn with_drop_grace(mut self, grace: Duration) -> Self {
        self.drop_grace = grace;
        self
    }

    /// Open the repository like a started `git-receive-pack` does: without what the environment of the client says about
    /// repositories and configuration, but with the namespace of `GIT_NAMESPACE`, which programs inherit.
    fn open(git_dir: &Path) -> std::io::Result<crate::Repository> {
        let mut repo = crate::open_opts(
            git_dir,
            crate::open::Options::default().ignore_repository_environment(true),
        )
        .map_err(io_error)?;
        if let Some(namespace) = std::env::var_os("GIT_NAMESPACE").filter(|namespace| !namespace.is_empty()) {
            repo.set_namespace(namespace.to_string_lossy().as_ref())
                .map_err(io_error)?;
        }
        Ok(repo)
    }
}

/// Say why `repo` can't be served for `service`, which is the case if it asks for what isn't supported, if the version of the
/// protocol isn't one that is spoken, or if support for the service wasn't built.
fn check_service(repo: &crate::Repository, service: Service, version: Protocol) -> Result<(), String> {
    match service {
        #[cfg(feature = "receive")]
        Service::ReceivePack => repo.receive().map(|_| ()).map_err(|err| err.to_string()),
        #[cfg(feature = "upload")]
        Service::UploadPack => {
            if version != Protocol::V2 {
                return Err("serving fetches from within the process needs protocol version 2".into());
            }
            repo.upload().map(|_| ()).map_err(|err| err.to_string())
        }
        #[allow(unreachable_patterns)]
        _ => {
            let _ = (repo, version);
            Err(format!(
                "this build of `gix` can't serve {} from within the process, as the matching feature is disabled",
                service.as_str()
            ))
        }
    }
}

/// Serve `service` from `repo` to what `input` and `output` are connected to, and return what went wrong, if anything.
fn serve(
    repo: &crate::Repository,
    service: Service,
    input: &mut dyn std::io::Read,
    output: &mut dyn Write,
    version: Protocol,
    interrupted: &AtomicBool,
) -> Option<gix_error::Error> {
    match service {
        #[cfg(feature = "receive")]
        Service::ReceivePack => repo
            .receive()
            .map(Receiver::with_hooks_without_local_repo_env)
            .map(|receiver| receiver.with_auto_maintenance(|repo| run_auto_maintenance(repo, interrupted)))
            .and_then(|receiver| receiver.serve(input, output, version, interrupted))
            .err(),
        #[cfg(feature = "upload")]
        Service::UploadPack => repo
            .upload()
            .and_then(|upload| upload.serve(input, output, interrupted))
            .err(),
        #[allow(unreachable_patterns)]
        _ => {
            let _ = (repo, input, output, version, interrupted);
            None
        }
    }
}

/// What `git receive-pack` does after objects were received and `receive.autogc` allows it: run `maintenance run --auto` if
/// `maintenance.auto`, or else `gc.auto`, doesn't turn it off. It runs here, after the report was sent to the client, and doesn't
/// detach into the background as Git does with `maintenance.autoDetach` or `gc.autoDetach`, so the client of a push, which reads what
/// the server says until it ends, waits for it as it does with `maintenance.autoDetach=false`. A client that stops waiting earlier
/// ends it with `interrupted` when the transport is dropped, which makes it stop at its next step and release its locks, as a
/// program that ends while it runs would leave `objects/maintenance.lock` behind.
/// The flag is the one that interrupts the whole server on purpose: an interrupt of the transport asks for the work done for it to
/// stop, and maintenance can be stopped at any of its steps without loss, so it needs no flag of its own.
/// A caller that wants Git's detaching can turn `receive.autogc` off and run [`auto_maintenance()`](crate::Repository::auto_maintenance())
/// itself.
#[cfg(feature = "receive")]
fn run_auto_maintenance(repo: &crate::Repository, interrupted: &AtomicBool) -> gix_error::Result<()> {
    if repo.auto_maintenance()?.enabled {
        let options = crate::maintenance::run::Options {
            auto: true,
            ..Default::default()
        };
        repo.run_maintenance(options, interrupted)?;
    }
    Ok(())
}

fn io_error(err: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> std::io::Error {
    std::io::Error::other(err)
}

fn interrupted() -> client::Error {
    client::Error::Io(std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "the transport was interrupted",
    ))
}

impl TransportWithoutIO for NativeLocal {
    fn to_url(&self) -> Cow<'_, BStr> {
        Cow::Owned(self.url.to_bstring())
    }

    fn connection_persists_across_multiple_requests(&self) -> bool {
        true
    }

    fn configure(&mut self, _config: &dyn Any) -> gix_error::Result {
        Ok(())
    }

    fn interrupt_handle(&self) -> Option<client::InterruptHandle> {
        let server = self.server.clone();
        Some(client::InterruptHandle::new(move || server.interrupt()))
    }

    fn wait_for_server(&mut self, deadline: Duration) -> gix_error::Result<bool> {
        // Closing what the server reads ends it if it waits for more, like for the rest of an incomplete request. What it
        // writes is read to its end, as it would block on a full pipe otherwise and never finish.
        self.server.input.close();
        if let Some((mut server_output, _input)) = self.connection.take().map(Connection::into_inner) {
            std::thread::Builder::new()
                .name("drain server output".into())
                .stack_size(128 * 1024)
                .spawn(move || {
                    std::io::copy(&mut server_output, &mut std::io::sink()).ok();
                })
                .or_raise(|| message("Could not start a thread to read what the server still has to say"))?;
        }
        match self.server.wait(deadline) {
            Ok(finished) => Ok(finished),
            Err(_) => Err(message!("The server for {} panicked", self.url.to_bstring()).raise()),
        }
    }
}

impl Transport for NativeLocal {
    fn handshake<'a>(
        &mut self,
        service: Service,
        extra_parameters: &'a [(&'a str, Option<&'a str>)],
    ) -> Result<SetServiceResponse<'_>, client::Error> {
        if self.server.interrupted.load(Ordering::SeqCst) {
            return Err(interrupted());
        }
        // Everything that makes a repository unsupported shows now, and not as a connection that ends for no reason.
        let unsupported =
            |reason: String| client::Error::Io(std::io::Error::new(std::io::ErrorKind::Unsupported, reason));
        let repo = Self::open(&self.git_dir).map_err(client::Error::Io)?;
        check_service(&repo, service, self.desired_version).map_err(unsupported)?;

        let (server_input, client_output) = std::io::pipe().map_err(client::Error::Io)?;
        let (client_input, server_output) = std::io::pipe().map_err(client::Error::Io)?;
        *self
            .server
            .input
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(client_output);

        let server = self.server.clone();
        let version = self.desired_version;
        let git_dir = self.git_dir.clone();
        let handle = std::thread::Builder::new()
            .name("serve a local client in process".into())
            .spawn(move || {
                let (mut input, mut output) = (server_input, server_output);
                // A repository can't move between threads without the `parallel` feature, so the server opens its own.
                // A started program says what went wrong on its standard error, and the client sees that it ended.
                let failure = match NativeLocal::open(&git_dir) {
                    Ok(repo) => serve(&repo, service, &mut input, &mut output, version, &server.interrupted),
                    Err(err) => Some(message!("Could not open the repository to serve: {err}").raise()),
                };
                if let Some(_failure) = failure {
                    gix_trace::error!("Serving a client from within the process failed: {_failure:#}");
                }
                // What the server writes ends with it, which tells a client that waits for more.
                drop(output);
            })
            .map_err(client::Error::Io)?;
        *self
            .server
            .thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);

        let path = BString::from(self.git_dir.to_string_lossy().into_owned());
        self.connection = Some(Connection::new(
            client_input,
            self.server.input.clone(),
            self.desired_version,
            path,
            None::<(&str, Option<u16>)>,
            ConnectMode::Process,
            self.trace,
        ));
        self.connection
            .as_mut()
            .expect("it was just set")
            .handshake(service, extra_parameters)
    }

    fn request(
        &mut self,
        write_mode: WriteMode,
        on_into_read: MessageKind,
        trace: bool,
    ) -> Result<RequestWriter<'_>, client::Error> {
        self.connection
            .as_mut()
            .ok_or(client::Error::MissingHandshake)?
            .request(write_mode, on_into_read, trace)
    }
}

impl Drop for NativeLocal {
    fn drop(&mut self) {
        // Both ends close first, so that a server that blocks on writing to or reading from us ends.
        self.server.input.close();
        self.connection = None;
        // Hooks that run after the reply aren't waited for here, but by `wait_for_server()`, which callers use if it matters.
        // Maintenance that runs after a push is told to stop if it takes too long, and is waited for once more, as a program that
        // ends while it runs would leave `objects/maintenance.lock` behind, which nobody removes.
        if !self.server.wait(self.drop_grace).unwrap_or(true) {
            self.server.interrupt();
            self.server.wait(self.drop_grace).ok();
        }
    }
}
