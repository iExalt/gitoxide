//! Read and write Git's reftable format.
//!
//! A *table* is an immutable, sorted file of reference and reflog records, and a *stack* is an ordered list of
//! tables whose newer entries shadow older ones. Tables are exchanged with Git byte for byte in both directions,
//! for SHA-1 and for SHA-256 repositories.
//!
//! ## Provenance
//!
//! The record, block and table codecs adopt the structure of Git's `reftable/` library, via a port of it to Rust that was
//! contributed to the upstream `gitoxide` project, with the writer reworked to produce the layout Git does.
//! That library is distributed under the BSD license of Google LLC, which is included as `LICENSE-BSD-3-CLAUSE`.
//!
//! ## Damaged data
//!
//! Tables carry a checksum for their footer only, so damage inside blocks is detected when it makes them structurally invalid.
//! Such damage fails with an error that [is classified as corruption](gix_error::Error::is_corrupted()), rather than panicking
//! or silently reading as if there were no records. Git's own readers do treat unreadable tables as an empty set of
//! references, which this crate deliberately does not copy.
#![deny(missing_docs, rust_2018_idioms)]
#![forbid(unsafe_code)]

mod basics;
mod block;

///
pub mod blocksource;
///
pub mod merged;
///
pub mod record;
pub mod stack;
///
pub mod table;
///
pub mod writer;

pub use blocksource::BlockSource;
pub use merged::MergedTable;
pub use stack::Stack;
pub use table::Table;
pub use writer::Writer;
