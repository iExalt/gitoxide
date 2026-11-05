//! The serving side of fetching and listing references, like `git upload-pack` speaks protocol version 2: what to advertise, how to
//! read the requests of a client and how to answer them.
//!
//! This module performs no I/O, so that drivers can share it. The order of operations is:
//!
//! 1. Write the [advertisement](Capabilities::encode()) once.
//! 2. Feed every packet line of a request to a [`request::Parser`], or let `read_request()` do that, until the request is complete.
//!    A client that has nothing more to ask ends the session with a flush packet or by closing the connection, which is no request.
//! 3. Answer the [`Request`] with what [`response`] encodes: the references for [`LsRefs`], and for [`Fetch`] the pack in a
//!    [`response::packfile()`] section, multiplexed with [`side_band`](crate::receive::side_band) as the protocol demands.
//!
//! Only protocol version 2 is spoken. Versions 0 and 1 advertise references up front, which isn't what drivers of this module do.
//!
//! ## Compatibility with Git
//!
//! The advertisement lists what is served in the order `git upload-pack` of Git 2.56 uses. What it doesn't serve yet is
//! missing from it, which is the `shallow` and `wait-for-done` features of `fetch` and `object-info`, and clients that use them
//! anyway get an error from the driver. The parser reads the arguments of `shallow`, `deepen`, `deepen-relative`, `deepen-since`,
//! `deepen-not` and `wait-for-done` and keeps them in the [`Fetch`] it returns, so that drivers can refuse them by name.
//! Arguments that Git would only accept if their feature was configured, such as `filter`, `want-ref` and `sideband-all`, are
//! errors like they are in Git by default.
mod capabilities;
pub use capabilities::Capabilities;

///
pub mod request;
pub use request::{Command, Fetch, LsRefs, Parser, Request};

///
pub mod response;

#[cfg(feature = "blocking-server")]
mod function;
#[cfg(feature = "blocking-server")]
pub use function::{read_request, write_advertisement};
