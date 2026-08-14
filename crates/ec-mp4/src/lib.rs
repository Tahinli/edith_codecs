//! MP4/ISOBMFF (`.mp4`, `.m4v`, `.m4a`) and QuickTime (`.mov`), both directions.
//!
//! [`Mp4Demuxer`] reads a container into [`ec_core::Packet`]s and [`Mp4Muxer`]
//! writes them back out; both speak the [`ec_core::Demuxer`] and
//! [`ec_core::Muxer`] contracts, so nothing above them knows which container it
//! is holding.
//!
//! Contracts worth knowing before implementing against this crate:
//!
//! - **The frame rate is a rational read off `stts`, never a rounded field.**
//!   Whole track over whole track, so a constant table gives exactly
//!   `timescale/delta` and a 90 kHz table spreading 3753/3754 ticks a frame
//!   gives exactly `24000/1001`. An NTSC film that arrives as 23.976 leaves as
//!   23.976, which is the one bug this crate was written to not have.
//! - **A sample entry is read for what it says it is.** An `mp4a` holding MP3 is
//!   an MP3 track, an `ac-3`/`ec-3`/`alac`/`Opus`/`fLaC` entry is that codec,
//!   and an entry this build has no [`ec_core::CodecId`] for leaves its track
//!   listed as nothing rather than decoded as something else.
//! - **`esds` hands back bytes.** [`Esds::decoder_specific`] is the raw
//!   `DecoderSpecificInfo` — an AAC track's `AudioSpecificConfig` verbatim, not
//!   a triplet of parsed fields a caller has to reassemble.
//! - **The edit list shifts, it never trims.** An initial empty edit is a delay
//!   and the first real edit's `media_time` is where the media starts; no sample
//!   is ever dropped for either, because a muxer writes `media_time` equal to
//!   the first composition offset and reading *that* as a trim throws away real
//!   pictures.
//! - **Malformed input is an error, never a panic.** Box sizes are checked
//!   against their parent, table entry counts against the box that holds them
//!   and the sample count against the length of the file, so a crafted header
//!   cannot allocate the machine. `forbid(unsafe_code)`, and no arithmetic on a
//!   parsed number that is not checked or saturating.
//! - **The muxer writes `mdat` first and `moov` last**, in one pass; see
//!   [`Mp4Muxer`] for what that costs and what it buys.
//!
//! What this crate deliberately does *not* do: convert between Annex-B and
//! length-prefixed samples. An mp4 sample is the codec's own bytes and this
//! writes what it is given — the bitstream-format question belongs to the codec
//! crates, not to the container.
//!
//! No async, no unsafe, no external dependencies.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod boxes;
mod demux;
mod esds;

pub use demux::Mp4Demuxer;
pub use esds::{Esds, object_type};

/// True when `head` looks like an ISOBMFF file: the second word of its first box
/// is one of the types an mp4 or a `.mov` starts with.
///
/// `ftyp` is what every file written this century begins with; the rest are what
/// a QuickTime file from before it does, and `styp`/`moof` are a fragment handed
/// over on its own.
pub fn is_mp4(head: &[u8]) -> bool {
    let Some(kind) = head.get(4..8) else {
        return false;
    };
    matches!(
        kind,
        b"ftyp" | b"styp" | b"moov" | b"moof" | b"mdat" | b"free" | b"skip" | b"wide" | b"pnot"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_shapes_a_file_starts_with() {
        assert!(is_mp4(b"\0\0\0\x18ftypisom"));
        assert!(is_mp4(b"\0\0\0\x10moovxxxx"));
        assert!(
            !is_mp4(b"\x1aE\xdf\xa3\x01\x00\x00\x00"),
            "that is Matroska"
        );
        assert!(!is_mp4(b"RIFF"), "and shorter than a header");
    }
}
