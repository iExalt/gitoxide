//! The receiving side of pushing, like `git receive-pack` speaks it: what to advertise, how to read the request
//! of a client and how to report back.
//!
//! This module performs no I/O, so that drivers can share it. The order of operations is:
//!
//! 1. Describe what the repository has with an [`Advertisement`] and write it with
//!    [`Advertisement::encode()`] to the client, passing the protocol version it asked for.
//! 2. Feed every packet line the client sends to a [`request::Parser`], or let `read_request()` do that, which
//!    also reads the push options. What follows is the pack, if one is [expected](Request::expects_pack()).
//! 3. After handling the commands, describe what happened with a [`Report`] and [encode](Report::encode()) it,
//!    wrapping it with [`side_band::encode()`] if the client [asked for sidebands](Features::side_band_64k).
//!    With side bands, a flush packet ends the response, even if the client asked for no report.
//!    Nothing is sent if there were no commands.
//!
//! Protocol versions 0 and 1 are supported, which is all `git receive-pack` speaks as well.
//!
//! ## Compatibility with Git
//!
//! The functions follow `receive-pack` of Git 2.56 closely, down to the quirks of its parsers. Unlike
//! [`push::Command`](crate::push::Command), a [`RawCommand`] doesn't validate its reference name, as
//! Git accepts any name from the wire and rejects it with a status for that command only.
//!
//! ## Not supported yet
//!
//! Push certificates (`push-cert`) are refused as a protocol error, as they are never advertised. Git processes them even if
//! they weren't, so this is a deliberate difference until they are supported.
mod capabilities;
pub use capabilities::{Capabilities, Features};

///
pub mod advertise;
pub use advertise::{Advertisement, Reference};

///
pub mod request;
pub use request::{Parser, RawCommand, Request};

///
pub mod report;
pub use report::Report;

///
pub mod side_band;

#[cfg(feature = "blocking-server")]
pub(crate) mod function;
#[cfg(feature = "blocking-server")]
pub use function::{read_request, write_advertisement, write_report};
