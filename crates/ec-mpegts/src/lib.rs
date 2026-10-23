//! MPEG-2 Transport Stream demuxer (and, in [`PsDemuxer`], MPEG-1 system / MPEG-2
//! program streams and raw MPEG-1/2 video): `.ts`/`.mpg`-style 188-byte streams and the
//! 192-byte BDAV (`.m2ts`, `.mts`) files AVCHD camcorders and Blu-ray write.
//!
//! [`TsDemuxer`] speaks [`ec_core::Demuxer`]. Contracts worth knowing before
//! integrating:
//!
//! - **Packet size and resync.** 188, 192 (4-byte `TP_extra_header`) and 204
//!   (Reed-Solomon) are told apart by the sync-byte stride. A lost sync byte is
//!   searched for again (four confirming strides) and the PES in flight is flagged
//!   `corrupt`; nothing panics on any input.
//! - **Streams.** The first program is read (`with_program` picks another;
//!   [`TsDemuxer::programs`] lists all). Streams the family can decode are in
//!   `streams()` (MPEG audio layers I, II and III as `Mp1`, `Mp2`, `Mp3`); the rest (LATM, odd-channel
//!   LPCM, DVB subtitles, DTS, ...) are in [`TsDemuxer::tracks`] with a
//!   refusal that names the codec. A PMT change mid-stream keeps the indices of
//!   streams that stay and appends new ones.
//! - **Timing.** Every packet is in a `1/90000` time base. 33-bit PTS/DTS are
//!   unwrapped into monotonic `i64`; `StreamInfo::start_time` is the first PTS.
//! - **Video** is delivered as Annex-B access units (split on PES boundaries, and on
//!   access unit delimiters inside a PES for progressive H.264 and HEVC), with the
//!   keyframe flag from IDR/IRAP NAL types, recovery-point SEI or the PES
//!   `random_access_indicator`. `extradata` carries an `avcC`/`hvcC` built from the
//!   in-band parameter sets; [`VideoFraming::LengthPrefixed`] converts packets to
//!   the matching length-prefixed form.
//! - **MPEG-1/2 video** (stream types 0x01, 0x02) is exposed as
//!   `CodecId::Mpeg2Video`, one packet per coded picture with the sequence/GOP
//!   headers in front of it (a PES holding several pictures is cut at picture start
//!   codes; a picture split over several timestamped PES is not re-joined). Keyframe
//!   = an I picture or a sequence header. Size, frame rate, aspect and chroma come
//!   from the sequence header and sequence_extension, which are also the extradata.
//! - **Audio** is split into frames (ADTS, AC-3/E-AC-3 syncframes, MPEG audio);
//!   ADTS frames keep their headers and `extradata` is the derived
//!   AudioSpecificConfig. HDMV LPCM loses its 4-byte header.
//! - **TrueHD** (stream type 0x83, Blu-ray) is exposed as `CodecId::TrueHd`, one
//!   packet per access unit: the 4-byte access-unit header (length word, input
//!   timing) and everything after it, the form `ec_truehd::TrueHdDecoder` decodes
//!   (and the form the MP4 and Matroska demuxers hand over). Blu-ray interleaves an
//!   AC-3 core (0x0B77 syncframes) in the same PID; those frames are dropped, not
//!   exposed, since the TrueHD access units carry the same audio losslessly and
//!   they take no time on the timeline. Timestamps come from the first access unit
//!   that starts in a PES and advance by 40 << n samples per access unit (n from the
//!   major sync's rate code), so the AC-3 frames do not shift them.
//! - **Seeking** bisects the byte range on the video track's timestamps, then scans
//!   forward to the keyframe at or before the target. It is exact: the packets read
//!   after a seek are those a sequential read yields from
//!   [`TsDemuxer::last_packet_offset`] on.
//!
//! No async, no unsafe.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod codec;
mod demux;
mod dts;
mod ps;
mod psi;
mod psvideo;
mod track;
mod ts;
mod window;

#[doc(hidden)]
pub mod fuzz_api;

pub use codec::{
    LpcmHeader, NalIter, StreamKind, annexb_to_length_prefixed, audio_specific_config, build_avcc,
    build_hvcc, nal_units, parse_lpcm_header,
};
pub use demux::{ProgramInfo, Stats, TrackInfo, TsDemuxer, VideoFraming};
pub use ps::{PsDemuxer, PsKind, is_mpeg_video_es, is_mpegps};
pub use psi::{EsEntry, Pmt};

/// True when `head` (the first few KiB of a file) is a transport stream at one of
/// the three packet sizes. A probe hands the file to [`TsDemuxer`] on this.
pub fn is_mpegts(head: &[u8]) -> bool {
    ts::looks_like_ts(head)
}
