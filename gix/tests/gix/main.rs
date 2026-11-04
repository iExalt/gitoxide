mod util;
use util::*;

mod clone;
mod clone_refs;
mod commit;
mod config;
#[cfg(feature = "blob-diff")]
mod diff;
#[cfg(target_os = "linux")]
mod durability;
mod error;
mod head;
mod id;
mod init;
mod object;
#[cfg(feature = "receive")]
mod receive;
mod reference;
mod remote;
mod repository;
#[cfg(feature = "revision")]
mod revision;
#[cfg(feature = "status")]
mod status;
#[cfg(feature = "attributes")]
mod submodule;
