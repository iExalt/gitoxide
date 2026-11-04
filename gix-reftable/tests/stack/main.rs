//! Stacks of tables, which Git wrote or which Git reads.

use gix_testtools::Result;

#[path = "../table/util.rs"]
#[allow(dead_code)]
mod util;

mod compact;
mod read;
mod write;
