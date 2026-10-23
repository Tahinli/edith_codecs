#![allow(clippy::needless_range_loop, clippy::field_reassign_with_default, clippy::too_many_arguments, clippy::type_complexity)]
//! MPEG-1 and MPEG-2 video decoder.
#![forbid(unsafe_code)]
mod bits;
mod block;
mod decoder;
mod header;
mod idct;
mod mc;
mod tables;

pub use decoder::PictureInfo;
pub use header::{PictureType, SequenceInfo, Structure};
mod codec;
pub use codec::Mpeg2Decoder;
