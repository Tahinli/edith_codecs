//! Matroska (`.mkv`, `.mka`, `.mks`, `.mk3d`) and WebM, both directions.
//!
//! [`MatroskaDemuxer`] reads a container into [`ec_core::Packet`]s through the
//! [`ec_core::Demuxer`] contract, so nothing above it knows which container it
//! is holding.
//!
//! Contracts worth knowing before implementing against this crate:
//!
//! - **Seeking uses the reader it was opened with.** `Cues` are read once, on
//!   the first seek — from in front of the clusters where a muxer put them
//!   there, through the `SeekHead` where it put them at the far end, and from a
//!   walk of the cluster headers for a file that has no index at all. A seek
//!   then lands on a random-access point at or before the target
//!   ([`ec_core::SeekMode::SyncBefore`]) without ever reopening anything.
//! - **Timing is integer.** `TimestampScale` becomes an exact
//!   [`ec_core::TimeBase`] (`1/1000` for the millisecond every muxer writes) and
//!   every packet carries it; the frame rate comes off `DefaultDuration`, which
//!   is the only exact statement of it a Matroska file makes.
//! - **Packets are zero-copy.** One cluster is read into one allocation and
//!   every packet of it is a [`ec_core::Buf`] slice of that allocation — a
//!   refcount bump, not a copy. A track under `ContentEncodings` is the one
//!   exception: unpacking it has to build the frame it hands back.
//! - **A compressed track is never read as if it were plain.** zlib and header
//!   stripping are undone; anything else — encryption, bzlib, lzo1x — is refused
//!   by name, because a compressed track read raw decodes into garbage and that
//!   is the one thing this must not do quietly.
//! - **Malformed input is an error, never a panic.** A cluster whose element
//!   chain breaks is resynchronised on the next `Cluster` id, so a truncated
//!   download plays its prefix.
//!
//! No async, no unsafe.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod demux;
mod ebml;

pub use demux::MatroskaDemuxer;

/// True when `head` starts with an EBML header, which is what a Matroska or
/// WebM file begins with and what a probe needs to see to hand a file here.
///
/// The doc type is checked by [`MatroskaDemuxer::new`], not here: this answers
/// "EBML", and a `.webm` and a `.mkv` are the same four magic bytes.
pub fn is_matroska(head: &[u8]) -> bool {
    head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
}
