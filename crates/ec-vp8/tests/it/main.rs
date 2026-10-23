//! ec-vp8 integration tests, one binary. Each module is one former `tests/*.rs` file;
//! `common` holds the IVF/WebP/ffmpeg helpers they share.
#![forbid(unsafe_code)]

mod common;
mod fuzz_regressions;
mod header_parse;
mod keyframe_exact;
mod stream_exact;
