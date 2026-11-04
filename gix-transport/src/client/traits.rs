use gix_error::Result;
use std::{
    any::Any,
    borrow::Cow,
    ops::{Deref, DerefMut},
    sync::Arc,
    time::Duration,
};

use bstr::BStr;

use crate::{Protocol, client::Error};

/// A way to end what a transport is blocked on from another thread, which is obtained with
/// [`TransportWithoutIO::interrupt_handle()`] and can be cloned and sent to the thread that watches for cancellation.
///
/// Interrupting makes the operation that blocks fail, as promptly as the transport can notice, with an error that says so
/// if the transport can tell, and every later operation fail as well. The transport can't be used afterwards.
/// Calling it more than once, or after the transport was dropped, is fine.
#[derive(Clone)]
pub struct InterruptHandle(Arc<dyn Fn() + Send + Sync>);

impl InterruptHandle {
    /// Create a handle that runs `interrupt` when it is used.
    pub fn new(interrupt: impl Fn() + Send + Sync + 'static) -> Self {
        InterruptHandle(Arc::new(interrupt))
    }

    /// End what the transport is blocked on, and make it fail from now on.
    pub fn interrupt(&self) {
        (self.0)();
    }
}

/// The error of an operation that was ended by an [`InterruptHandle`], which is [cancelled](gix_error::Class::Cancelled).
///
/// Its kind isn't [`std::io::ErrorKind::Interrupted`], as readers retry on that.
#[cfg(feature = "blocking-client")]
pub(crate) fn interrupted_error() -> std::io::Error {
    std::io::Error::other(gix_error::ErrorExt::raise(gix_error::cancelled(
        "The transport was interrupted",
    )))
}

impl std::fmt::Debug for InterruptHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InterruptHandle")
    }
}

/// This trait represents all transport related functions that don't require any input/output to be done which helps
/// implementation to share more code across blocking and async programs.
pub trait TransportWithoutIO {
    /// If the handshake or subsequent reads failed with [`std::io::ErrorKind::PermissionDenied`], use this method to
    /// inform the transport layer about the identity to use for subsequent calls.
    /// If authentication continues to fail even with an identity set, consider communicating this to the provider
    /// of the identity in order to mark it as invalid. Otherwise the user might have difficulty updating obsolete
    /// credentials.
    /// Please note that most transport layers are unauthenticated and thus return [an error][Error::AuthenticationUnsupported] here.
    fn set_identity(&mut self, _identity: gix_sec::identity::Account) -> std::result::Result<(), Error> {
        Err(Error::AuthenticationUnsupported)
    }

    /// Returns the canonical URL pointing to the destination of this transport.
    fn to_url(&self) -> Cow<'_, BStr>;

    /// If the actually advertised server version is contained in the returned slice or it is empty, continue as normal,
    /// assume the server's protocol version is desired or acceptable.
    ///
    /// Otherwise, abort the fetch operation with an error to avoid continuing any interaction with the transport.
    ///
    /// In V1 this means a potentially large list of advertised refs won't be read, instead the connection is ignored
    /// leaving the server with a client who potentially unexpectedly terminated the connection.
    ///
    /// Note that `transport.close()` is not called explicitly.
    ///
    /// Custom transports can override this to prevent any use of older protocol versions.
    fn supported_protocol_versions(&self) -> &[Protocol] {
        &[]
    }

    /// Returns true if the transport provides persistent connections across multiple requests, or false otherwise.
    /// Not being persistent implies that certain information has to be resent on each 'turn'
    /// of the fetch negotiation or that the end of interaction (i.e. no further request will be made) has to be indicated
    /// to the server for most graceful termination of the connection.
    fn connection_persists_across_multiple_requests(&self) -> bool;

    /// Pass `config` can be cast and interpreted by the implementation, as documented separately.
    ///
    /// The caller must know how that `config` data looks like for the intended implementation.
    fn configure(&mut self, config: &dyn Any) -> Result;

    /// Wait for the server side of this connection to finish what it is doing and exit, for at most `deadline`, and return
    /// `true` if it did so. If the server is still running at the deadline, `false` is returned and it is left alone, which
    /// means that dropping the transport may interrupt it.
    ///
    /// This lets a caller who is done with the transport, because it received a reply or failed to send a request, avoid
    /// cutting off the server's work, which would otherwise happen when a transport that spawned the server drops it.
    /// Think of hooks that a server runs after it replied, or a ref update it is in the middle of. Git's own client
    /// also waits for the server it started.
    ///
    /// Transports without a server process of their own, which is the default implementation, have nothing to wait for.
    /// The ones that do close their input, so servers waiting for more of it end, and wait for them. The transport
    /// can't be used for further requests afterwards.
    fn wait_for_server(&mut self, _deadline: Duration) -> Result<bool> {
        Ok(true)
    }

    /// Return a handle that ends what this transport blocks on from another thread, or `None` if it can't, in which case a
    /// blocked read or write only ends when the peer acts or the transport drops.
    ///
    /// Get it before the first request, as the handshake can block too. This is how cancellation reaches I/O that would
    /// otherwise not look at a flag until it returns, such as a connection to a server that stopped responding.
    fn interrupt_handle(&self) -> Option<InterruptHandle> {
        None
    }
}

// Would be nice if the box implementation could auto-forward to all implemented traits.
impl<T: TransportWithoutIO + ?Sized> TransportWithoutIO for Box<T> {
    fn set_identity(&mut self, identity: gix_sec::identity::Account) -> std::result::Result<(), Error> {
        self.deref_mut().set_identity(identity)
    }

    fn to_url(&self) -> Cow<'_, BStr> {
        self.deref().to_url()
    }

    fn supported_protocol_versions(&self) -> &[Protocol] {
        self.deref().supported_protocol_versions()
    }

    fn connection_persists_across_multiple_requests(&self) -> bool {
        self.deref().connection_persists_across_multiple_requests()
    }

    fn configure(&mut self, config: &dyn Any) -> Result {
        self.deref_mut().configure(config)
    }

    fn wait_for_server(&mut self, deadline: Duration) -> Result<bool> {
        self.deref_mut().wait_for_server(deadline)
    }

    fn interrupt_handle(&self) -> Option<InterruptHandle> {
        self.deref().interrupt_handle()
    }
}

impl<T: TransportWithoutIO + ?Sized> TransportWithoutIO for &mut T {
    fn set_identity(&mut self, identity: gix_sec::identity::Account) -> std::result::Result<(), Error> {
        self.deref_mut().set_identity(identity)
    }

    fn to_url(&self) -> Cow<'_, BStr> {
        self.deref().to_url()
    }

    fn supported_protocol_versions(&self) -> &[Protocol] {
        self.deref().supported_protocol_versions()
    }

    fn connection_persists_across_multiple_requests(&self) -> bool {
        self.deref().connection_persists_across_multiple_requests()
    }

    fn configure(&mut self, config: &dyn Any) -> Result {
        self.deref_mut().configure(config)
    }

    fn wait_for_server(&mut self, deadline: Duration) -> Result<bool> {
        self.deref_mut().wait_for_server(deadline)
    }

    fn interrupt_handle(&self) -> Option<InterruptHandle> {
        self.deref().interrupt_handle()
    }
}
