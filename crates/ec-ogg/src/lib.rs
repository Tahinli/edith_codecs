//! Ogg (RFC 3533): the container Vorbis, Opus and FLAC-in-Ogg travel in.
//!
//! [`OggDemuxer`] reads pages into packets — continuations reassembled, page
//! checksums verified, damaged pages skipped rather than fatal — and
//! [`OggMuxer`] writes packets back into pages with correct lacing, granule
//! positions and beginning/end-of-stream flags. Both speak the family IR, so an
//! Ogg file is `Demuxer` in and `Muxer` out like every other container here.
//!
//! What a granule position *means* belongs to the mapping, not to Ogg
//! ([`Mapping`]): Vorbis and FLAC count samples at the stream's own rate, Opus
//! counts 48 kHz samples including the pre-skip a decoder discards. Timestamps
//! are therefore exact per packet where the mapping states packet durations
//! (Opus does, in its TOC byte) and per page boundary where it does not (Vorbis
//! hides block sizes behind the setup header's codebooks, which is the Vorbis
//! decoder's parse, not the container's).
//!
//! Because a page states where its *last finishing packet* ends, a remux has to
//! carry that position through: [`OggDemuxer`] attaches it to the packet it
//! belongs to as [`SideData::Custom`] with kind [`GRANULE_KIND`], and
//! [`OggMuxer`] reads it back with [`granule_of`]. A packet without one is
//! still written — it simply cannot be the last packet on a page.
//!
//! Not covered: chained streams (a second beginning-of-stream group after the
//! first ends) are read as far as the first chain and no further, Skeleton
//! indexes are ignored, and Theora/Speex pages are skipped rather than
//! described. Each is a mapping this family does not carry yet, not a silent
//! misread.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod crc;
mod demux;
mod mapping;
mod mux;
mod page;

pub use demux::OggDemuxer;
pub use ec_core::{Error, Result};
pub use mapping::{Mapping, opus_packet_samples};
pub use mux::{DEFAULT_PAGE_TARGET, OggMuxer};
pub use page::{PageHeader, lacing, xiph_lace, xiph_unlace};

use ec_core::{Buf, Packet, SideData};

/// [`SideData::Custom`] kind carrying an Ogg granule position: eight
/// little-endian bytes of `i64`, the position at which the packet ends in its
/// mapping's own units.
///
/// Ogg states positions per page rather than per packet, and only for the last
/// packet finishing on each page, so this is present on exactly those packets —
/// it is the one piece of Ogg timing that has nowhere else to live in a
/// `Packet`, and without it a remux would have to guess where its pages end.
pub const GRANULE_KIND: u32 = u32::from_be_bytes(*b"OggS");

/// Side data stating that a packet ends at `granule`.
pub fn granule_side_data(granule: i64) -> SideData {
    SideData::Custom {
        kind: GRANULE_KIND,
        data: Buf::from_vec(granule.to_le_bytes().to_vec()),
    }
}

/// The granule position attached to `packet`, if any.
pub fn granule_of(packet: &Packet) -> Option<i64> {
    packet.side_data.iter().find_map(|side| match side {
        SideData::Custom { kind, data } if *kind == GRANULE_KIND && data.len() == 8 => {
            Some(i64::from_le_bytes(data[..8].try_into().ok()?))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_core::TimeBase;

    #[test]
    fn granule_side_data_round_trips() {
        let mut packet = Packet::new(0, TimeBase::from_rate(48_000), vec![1, 2, 3]);
        assert_eq!(granule_of(&packet), None);
        packet.side_data.push(granule_side_data(-1));
        assert_eq!(granule_of(&packet), Some(-1));
        let mut other = Packet::new(0, TimeBase::from_rate(44_100), vec![]);
        other.side_data.push(granule_side_data(1_234_567_890_123));
        assert_eq!(granule_of(&other), Some(1_234_567_890_123));
    }
}
