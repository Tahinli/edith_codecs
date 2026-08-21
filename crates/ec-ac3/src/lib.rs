//! AC-3 and Enhanced AC-3 decoding, from the ATSC A/52 standard.
//!
//! Two entry points, depending on how much of the format the caller needs:
//!
//! - [`Ac3Decoder`] takes syncframes and hands back interleaved `f32` audio
//!   frames, with the coded channel layout or a §7.8 downmix.
//! - [`syncinfo::parse`], [`bsi::parse`] and [`eac3::bsi::parse`] read the
//!   headers alone, which is what a container needs to state a track's sample
//!   rate and channel count before decoding anything.
//!
//! Contracts worth knowing before implementing against this crate:
//!
//! - **Encoding has no psychoacoustic model yet.** [`Ac3Encoder`] writes
//!   spec-valid syncframes — real MDCT, per-block/channel transient-driven
//!   block switching, an exponent planner (D15/D25/D45/reuse) and a
//!   binary-searched SNR-offset rate loop that fills a frame to roughly
//!   93-99.6% of its budget depending on content and bit rate — but the bit
//!   allocation curve itself is the standard's masking model rather than a
//!   perceptual search; see [`encode`] for exactly what is and is not
//!   adaptive yet.
//! - **Truncated input is [`Error::NeedMore`]**, a broken bit stream is
//!   [`Error::Corrupt`], and a construct this build does not implement is
//!   [`Error::Unsupported`] naming *what* and *why*. No input panics — the bit
//!   reader bounds every read and the exponent and bandwidth fields are range
//!   checked before they index anything.
//! - **Dynamic range compression is applied by default**, because §7.7.1 says a
//!   decoder shall: `dynrng` is the program provider's compression curve and
//!   leaving it out makes explosions 20 dB louder than the mix intended. The
//!   raw words stay visible on [`FrameInfo`], and [`Options::drc_scale`] turns
//!   it down or off.
//! - **Dialogue normalisation is surfaced, never applied.** §7.6 puts
//!   `dialnorm` in the volume control, which is not in this crate.
//! - **The noise this format asks for is on by default**: dither for zero-bit
//!   mantissas (§7.3.4) and the spectral extension blend (§E3.6.4.2.4). Neither
//!   sequence is specified, so two conformant decoders differ by exactly that
//!   noise; [`Options::dither`] turns both off when a comparison needs to
//!   isolate everything else.
//!
//! Annex E coverage: the adaptive hybrid transform (with its vector and
//! gain-adaptive quantizers) and spectral extension are decoded; enhanced
//! coupling and dependent substreams above 5.1 are refused by name rather than
//! decoded wrongly. On a real Dolby Digital Plus stream that uses both, the
//! coded bins land within 0.01% of the oracle's energy and the whole channel within
//! 0.999 correlation once the two decoders' independent noise is taken out of
//! the comparison ([`Options::dither`]).
//!
//! No unsafe, no allocation on the block path beyond the output buffer, and no
//! dependencies beyond the family IR and DSP crates.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// A bit stream parser indexes several per-channel and per-band arrays from one
// loop variable, which is the domain's own notion of position; iterator
// rewrites of those loops obscure which array is which.
#![allow(clippy::needless_range_loop)]

mod aht;
mod aht_tables;

pub mod bitalloc;
pub mod bsi;
pub mod eac3;
pub mod exps;
pub mod mantissa;
pub mod syncinfo;
pub mod tables;
pub mod transform;

mod decode;
mod decoder;
pub mod encode;

pub use decode::Syntax;
pub use decoder::{Ac3Decoder, Downmix, FrameInfo, Options};
pub use encode::{Ac3Encoder, EncodeStats, EncoderConfig};
pub use ec_core::Error;

/// Samples per channel one audio block produces.
pub use transform::BLOCK_SAMPLES;

/// Blocks in an AC-3 syncframe; Enhanced AC-3 may send 1, 2, 3 or 6.
pub const AC3_BLOCKS_PER_FRAME: usize = 6;

/// Length in bytes of the syncframe starting at `data[0]`, for either syntax.
///
/// This is what walking an elementary stream needs — the frame length lives in
/// `frmsizecod` for AC-3 and `frmsiz` for E-AC-3, in different places and
/// different units. A short buffer is [`Error::NeedMore`]; anything that is not
/// a syncframe is [`Error::Corrupt`].
pub fn frame_size(data: &[u8]) -> ec_core::Result<usize> {
    if data.len() < 6 {
        return Err(Error::NeedMore);
    }
    if data[0] != 0x0B || data[1] != 0x77 {
        return Err(Error::corrupt(
            "AC-3: no syncword at the start of the frame",
        ));
    }
    if data[5] >> 3 <= 10 {
        Ok(syncinfo::parse(data)?.frame_size)
    } else {
        Ok(eac3::parse(&data[2..])?.frame_size)
    }
}
