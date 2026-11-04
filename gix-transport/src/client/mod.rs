///
#[cfg(feature = "async-client")]
pub mod async_io;

mod traits;
#[cfg(feature = "blocking-client")]
pub(crate) use traits::interrupted_error;
pub use traits::{InterruptHandle, TransportWithoutIO};

///
#[cfg(feature = "blocking-client")]
pub mod blocking_io;

///
pub mod capabilities;
#[doc(inline)]
pub use capabilities::Capabilities;

mod non_io_types;
pub use gix_sec::identity::Account;
pub use non_io_types::{AuthenticationRequired, Error, MessageKind, WriteMode};

///
#[cfg(any(feature = "blocking-client", feature = "async-client"))]
pub mod git;
