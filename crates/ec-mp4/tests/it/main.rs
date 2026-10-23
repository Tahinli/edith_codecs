//! The crate's integration tests, in one binary. Run all of them with
//! `cargo test -p ec-mp4 --test it`, or one file with `--test it <file>::`.

#![forbid(unsafe_code)]

mod chapters;
mod common;
mod dts;
mod edit_trim;
mod edith_parity;
mod fuzz_regressions;
mod mp4;
mod mux_entries;
mod prores_entries;
mod qt_pcm_hdr;
mod rotation;
