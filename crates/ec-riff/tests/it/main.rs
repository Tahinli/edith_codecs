//! One integration-test binary for ec-riff. Each module below is one former `tests/*.rs` file;
//! run one with `cargo test -p ec-riff --test it <module>::`.
#![forbid(unsafe_code)]

mod common;

mod avi_fuzz_regressions;
mod avi_raw_dv;
mod avi_video;
mod dts;
mod mov_raw;
mod rf64;
mod wav;
