//! DV video decoder (IEC 61834, SMPTE 314M / 370M) and DIF audio extraction.
//!
//! Covers DV25 (NTSC 4:1:1, PAL 4:2:0 and PAL 4:1:1), DVCPRO25, DVCPRO50 (4:2:2) and DVCPRO HD
//! (1080i / 720p, 4:2:2). The arithmetic is FFmpeg's `dvvideo` decoder: its integer simple IDCT,
//! the 2-4-8 IDCT for field-mode blocks, and the three-pass bit-recovery of AC coefficients across
//! blocks, macroblocks and video segments, so planes are byte-identical to FFmpeg's native
//! `yuv411p` / `yuv420p` / `yuv422p` output.
//!
//! * [`DvVideoDecoder`] / [`DvPicture`] -- frame in, native planes out (4:1:1 stays 4:1:1).
//! * [`DvDecoder`] -- the `ec_core` [`Decoder`](ec_core::registry::Decoder). It widens 4:1:1 to
//!   `I422` by exact chroma duplication (ec-core has no `I411`); 4:2:0 -> `I420`; 4:2:2 -> `I422`.
//! * [`extract_audio`] -- the PCM in the audio DIF blocks (16-bit linear and 12-bit non-linear,
//!   both returned as 16-bit linear) as [`DvAudio`]. No audio `CodecId` is involved: audio leaves
//!   this crate as PCM, and [`DvDemuxer`] publishes it as `PcmS16Le` streams.
//! * [`DvDemuxer`], [`is_dv`] -- raw `.dv` / `.dif` files.
#![forbid(unsafe_code)]
#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

mod audio;
mod decoder;
mod demux;
mod idct;
mod profile;
mod tables;
mod video;

pub use audio::{DvAudio, extract_audio};
pub use decoder::{DvDecoder, output_format, to_video_frame};
pub use demux::{DvDemuxer, dv_probe_score, is_dv};
pub use profile::{Chroma, PROFILES, PROFILE_BYTES, Profile, frame_profile};
pub use video::{DvPicture, DvVideoDecoder, is_dv_header};
