//! The client side of pushing to `git receive-pack`: commands, capability negotiation, the request header and
//! the strict decoding of the status report.
//!
//! This module performs no I/O, so the blocking and asynchronous drivers can share it. The order of operations is:
//!
//! 1. Handshake with [`Service::ReceivePack`](gix_transport::Service::ReceivePack), then
//!    [parse the advertised capabilities](ServerCapabilities::from_capabilities()).
//! 2. Describe each ref update as a validated [`Command`].
//! 3. Build a [`Request`], which refuses everything the server did not offer instead of silently degrading.
//! 4. Write the [header](Request::encode_header()), followed by a pack if [one is expected](Request::expects_pack()).
//! 5. [Parse the report](Request::parse_report()) the server sends back.
//!
//! With the `blocking-client` feature, [`push()`] performs steps 4 and 5 over a transport.
//!
//! ## Outcomes
//!
//! A [`Report`] means the server answered completely and consistently, even when it rejected some or all commands.
//! Even then, a [`Status::Uncertain`](report::Status::Uncertain) command may have been applied despite the `ng`:
//! see its documentation.
//! An error from [`Request::parse_report()`] means the answer was missing, malformed or contradictory, so
//! commands may or may not have been applied: the outcome is *uncertain* and has to be reconciled
//! with the remote before retrying. Such errors are classified as [`Class::Corruption`](gix_error::Class::Corruption).
//! Errors from [`Request::new()`] are raised before anything was sent and are classified as
//! [`Class::Validation`](gix_error::Class::Validation) or [`Class::Unsupported`](gix_error::Class::Unsupported).
//!
//! ## Out of scope
//!
//! Policy (leases, ancestry checks, which objects to send) belongs to the caller. Signed pushes (`push-cert`),
//! `shallow` lines and protocol version 2 are not supported: receive-pack speaks protocol version 0 and 1 only.
//!
//! [`Class::Corruption`]: gix_error::Class::Corruption
mod capabilities;
pub use capabilities::ServerCapabilities;

mod command;
pub use command::Command;

mod request;
pub use request::{Options, Request, StatusFormat};

#[cfg(feature = "blocking-client")]
mod function;
#[cfg(feature = "blocking-client")]
pub use function::{PackProducerFailedAfterCompletion, push};

///
pub mod report;
pub use report::Report;
