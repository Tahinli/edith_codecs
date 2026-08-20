//! Dolby TrueHD and MLP decoding for the edith_codecs family.
//!
//! Written from Dolby's public TrueHD/MLP documentation of the bitstream's
//! *shape* — major sync, per-substream access-unit directory, FIR/IIR filter
//! parameters, the Huffman/LSB entropy coder and matrixing — not from any
//! third party's source. The syntax layer ([`sync`]) is parsed and verified
//! against a real 7.1 Blu-ray remux; the substream *decode* itself
//! ([`TrueHdDecoder::decode_access_unit`]) is not implemented yet.
//!
//! Scope, stated once so a caller knows what to expect:
//!
//! - **In scope**: major sync detection and parsing (format id, sample rate,
//!   channel presentations), the access-unit header's per-substream
//!   directory (end pointers, checksum/parity), and — once implemented — the
//!   three PCM-presentation substreams a real Blu-ray track carries: 2-ch,
//!   6-ch (5.1) and 8-ch (7.1), each built from restart headers, FIR/IIR
//!   predictor filters, Huffman/LSB residual entropy and the channel
//!   matrixing that turns decorrelated substream channels back into PCM,
//!   plus the stream's own lossless check.
//! - **Out of scope, refused by name via [`Error::Unsupported`]**: the 4th
//!   substream (16-channel object/Atmos audio) — a different, unrelated
//!   payload riding the same container, not a bigger version of the 3 PCM
//!   presentations above — and any metadata semantics beyond parsing the
//!   bytes (dialogue normalisation curves, downmix hints and the like stay
//!   unopinionated data on the parsed structs, same as this family treats
//!   AC-3's `dialnorm`).
//!
//! No unsafe, no panics on malformed input: truncated data is
//! [`Error::NeedMore`], a bitstream that violates its own rules is
//! [`Error::Corrupt`], and a construct this build does not implement is
//! [`Error::Unsupported`] naming *what* and *why*.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod sync;

pub use ec_core::Error;
pub use sync::{AccessUnitHeader, MajorSyncInfo, MajorSyncFormat, SubstreamInfo, TrueHdDecoder, frame_length};
