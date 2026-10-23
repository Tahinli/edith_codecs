//! Integration tests for ec-mpegts, one binary: the TS and PS demuxers against ffmpeg's
//! output, committed fixtures, synthetic streams and damaged input.
#![forbid(unsafe_code)]

mod common;

mod committed;
mod committed_ps;
mod dts;
mod ffmpeg_parity;
mod mpeg2;
mod ps;
mod robust;
mod synthetic;
mod truehd;
