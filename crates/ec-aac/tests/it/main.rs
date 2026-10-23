#![forbid(unsafe_code)]
//! Merged integration tests for ec-aac: one module per former `tests/*.rs` file.

#[path = "../common/mod.rs"]
mod shared;
mod common;

mod he_parity;
mod implicit_ps;
mod implicit_sbr;
mod reelh_per_frame;
mod multichannel_parity;
mod no_panic;
mod ps_mp4;
mod ps_oracle;
