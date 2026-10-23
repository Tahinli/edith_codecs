//! One integration-test binary for ec-ac3; each module is one former `tests/*.rs` file.
#![forbid(unsafe_code)]

mod common;
mod decode_matrix;
mod dependent;
mod encode_matrix;
mod ffmpeg_parity;
mod packet_walk;
mod real_library;
