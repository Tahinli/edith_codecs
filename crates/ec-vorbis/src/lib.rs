//! Vorbis I — decoder and encoder.
//!
//! The decoder is the whole format: both floor types, all three residue types,
//! square-polar channel coupling, every legal blocksize and the four window
//! shapes a long block takes next to a short one. It is written against the
//! Xiph Vorbis I specification and nothing else.
//!
//! The encoder is deliberately narrower and states its own terms:
//!
//! - **Any channel count, natively.** Mono, stereo and surround each get a
//!   setup header written for that layout, with coupling where coupling pays
//!   (stereo) and none where it does not. There is no stereo-only profile to
//!   work around and no dual-mono widening of a mono mix.
//! - **No pre-roll.** The first block is centred on input sample 0, so the
//!   decoder's first output sample *is* input sample 0. A caller does not feed
//!   silence ahead of the mix and does not subtract a hop from every granule.
//! - **Exact length.** The last packet's granule states the input's own sample
//!   count, so a file decodes to exactly as many samples as went in — no block
//!   grid overshoot survives to the player.
//! - **ABR.** [`EncoderConfig::bitrate_bps`] is tracked by a feedback loop over
//!   the headroom the floor leaves under the masking curve;
//!   [`EncoderConfig::quality`] sets where that loop starts, and holds the
//!   headroom still when no bitrate is asked for. The loop tracks rather than
//!   hits: a signal too simple to spend the budget on comes out under it.
//!
//! Timing contract: an audio packet answers the samples that became final when
//! its window closed, so the *first* packet of a stream answers none. That is
//! Vorbis, not an artefact — a block's left half is only half of the samples it
//! overlaps.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bits;
mod codebook;
mod decode;
mod encode;
mod floor;
mod residue;
mod setup;
mod window;

pub use decode::{VorbisDecoder, channel_map};
pub use encode::{BlockLog, EncoderConfig, VorbisEncoder};
pub use setup::{Comments, Identification};
