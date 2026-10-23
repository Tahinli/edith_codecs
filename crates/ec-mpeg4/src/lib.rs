#![allow(
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::type_complexity
)]
#![forbid(unsafe_code)]
//! MPEG-4 Part 2 Visual (Simple and Advanced Simple profile) and H.263 baseline decoder.

mod bits;
mod codec;
mod core;
mod frame;
mod h263;
mod h263data;
mod header;
mod mb;
mod msdata;
mod msmpeg4;
mod recon;
mod vlc;

pub mod dsp;
pub mod tables;

pub use codec::Mpeg4Decoder;
pub use core::PictureInfo;
