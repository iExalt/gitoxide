// The blocking variants of tests that are shared with the async ones keep what only the async variants need.
#![allow(clippy::while_let_on_iterator, clippy::drop_non_drop)]
extern crate core;

pub mod bisync {
    pub use gix_macros::{discard as only_async, keep as only_sync, sync as bisync};
}

use std::path::PathBuf;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub use gix_testtools::TestResult;

pub fn fixture_bytes(path: &str) -> Vec<u8> {
    fn fixture_path(path: &str) -> PathBuf {
        PathBuf::from("tests").join("fixtures").join(path)
    }
    std::fs::read(fixture_path(path)).expect("fixture to be present and readable")
}

mod client;
mod http_helpers;
