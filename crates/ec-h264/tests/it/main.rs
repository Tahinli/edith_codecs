//! ec-h264 integration tests, one binary. Each module is one former
//! `tests/<name>.rs` file; the test path is `<module>::<test>`.
//!
//! `conformance.rs` stays a separate binary: it installs a `#[global_allocator]`
//! and measures wall-clock speed.

#![forbid(unsafe_code)]

mod common;
mod encode;
mod fuzz_regressions;
mod high10;
mod high422;
mod high422_jvt;
mod high444;
mod high444_jvt;
mod interlaced;
mod mbaff_x264;
mod no_panic;
mod refusals;
