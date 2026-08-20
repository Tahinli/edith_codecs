//! TrueHD/MLP access-unit framing: the 16-bit length word, major sync and the
//! per-substream directory.
//!
//! Every field parsed here is calibrated against a real Blu-ray 7.1 TrueHD
//! remux (`~/Downloads/.../Book of Dragons ... TrueHD 7 1 ...mkv`, see
//! `tests/library.rs`): the access-unit length, the major sync's sample-rate
//! codes, and the substream directory's end pointers all reproduce ffprobe's
//! own 48000 Hz / 8-channel / 24-bit report on that file, AU for AU.
//!
//! **Bit layout implemented here** (byte offsets are from the start of the
//! access unit, i.e. from `data[0]`):
//!
//! - `data[0..2]` big-endian: top 4 bits a check/parity nibble (not
//!   validated — no public description of its generator was available to
//!   check against), low 12 bits the access-unit length in 16-bit words
//!   *including this 4-byte header*. [`frame_length`] returns that length in
//!   bytes.
//! - `data[2..4]`: input timing, carried on [`AccessUnitHeader::input_timing`]
//!   uninterpreted — a decoder does not need it to reconstruct samples.
//! - `data[4..8]`: `0xF8726FBA` (TrueHD) or `0xF8726FBB` (MLP) names a major
//!   sync at this access unit; absent on most access units, present roughly
//!   once per video frame.
//! - When a major sync is present, it occupies exactly 28 bytes starting at
//!   `data[4]` (format id + 24 more bytes, ending where the substream
//!   directory begins): `data[8]` packs two 4-bit sample-rate codes,
//!   `group1` in the high nibble and `group2` (a secondary/backward-compat
//!   rate, `0xF` when unused) in the low nibble. A code maps to a rate via
//!   [`sample_rate`]; codes 0-2 are the 48 kHz family (`48000 * 2^n`), codes
//!   8-10 the 44.1 kHz family (`44100 * 2^n`). The rest of the major sync
//!   (channel-assignment bitfields, flags) is skipped: this build does not
//!   independently verify their bit positions, so [`ChannelLayout`] is
//!   derived from the substream *directory* instead (see below), which is
//!   fully verified against the sample file.
//! - The substream directory starts right after the major sync (or right
//!   after the 4-byte access-unit header, on an access unit with none), one
//!   [`SubstreamInfo`] entry per substream: a 2-byte word whose top 4 bits
//!   are flags (bit `0x8` says a second 2-byte word — a checksum/parity/DRC
//!   word, [`SubstreamInfo::extra`] — follows) and whose low 12 bits are the
//!   substream's *end pointer*: the cumulative byte offset, in 16-bit words
//!   from the end of the whole directory, where this substream's coded data
//!   ends. There is no explicit substream-count field; a directory entry is
//!   the *last* one exactly when `directory_end + end_pointer_bytes` lands
//!   on the access unit's own total length — which is how
//!   [`AccessUnitHeader::parse`] finds the count, bounded at 4 entries
//!   (Dolby's own substream cap).
//!
//! Substream count maps to [`ChannelLayout`] by Dolby's own published
//! substream convention — substream 0 always carries a 2-ch presentation,
//! substream 1 (when present) adds a discrete 6-ch (5.1) presentation,
//! substream 2 (when present) adds a discrete 8-ch (7.1) presentation, and a
//! 4th substream carries an unrelated object-audio payload — rather than
//! from the major sync's own channel-assignment bitfields, for the reason
//! above.

use ec_core::error::{Error, Result};
use ec_core::frame::{AudioFrame, ChannelLayout, Frame};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, Decoder};

/// TrueHD's major sync word, immediately followed by the extension fields
/// TrueHD adds over plain MLP.
pub const MAJOR_SYNC_TRUEHD: u32 = 0xF872_6FBA;
/// MLP's major sync word (lossless-only, no TrueHD extensions).
pub const MAJOR_SYNC_MLP: u32 = 0xF872_6FBB;

/// Bytes a major sync occupies, format id included, ending where the
/// substream directory begins.
const MAJOR_SYNC_LEN: usize = 28;

/// Bytes the fixed part of an access unit's own header takes: the 16-bit
/// length/check word and the 16-bit input timing.
const AU_HEADER_LEN: usize = 4;

/// Access-unit length in bytes, from the 16-bit length word at `data[0..2]`.
///
/// A short buffer is [`Error::NeedMore`]; a length that could not even cover
/// its own 4-byte header is [`Error::Corrupt`].
pub fn frame_length(data: &[u8]) -> Result<usize> {
    if data.len() < 2 {
        return Err(Error::NeedMore);
    }
    let word = u16::from_be_bytes([data[0], data[1]]);
    let len = usize::from(word & 0x0FFF) * 2;
    if len < AU_HEADER_LEN {
        return Err(Error::corrupt(format!(
            "TrueHD: {len}-byte access unit shorter than its own header"
        )));
    }
    Ok(len)
}

/// Which format a major sync names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MajorSyncFormat {
    /// `0xF8726FBA`.
    TrueHd,
    /// `0xF8726FBB`.
    Mlp,
}

/// The format info an access unit's major sync carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MajorSyncInfo {
    /// Which sync word introduced it.
    pub format: MajorSyncFormat,
    /// The primary sample rate, decoded from `data[8]`'s high nibble.
    pub sample_rate: u32,
    /// The raw 4-bit rate code (`& 7` is the `2^n` multiplier, which also
    /// sizes the access unit: `40 << n` samples).
    pub rate_code: u8,
    /// The 8-channel presentation's 13-bit channel-assignment mask (bit 0 =
    /// L/R, 1 = C, 2 = LFE, 3 = Ls/Rs, 6 = Lrs/Rrs; `0x4F` is standard 7.1);
    /// 0 when the major sync is plain MLP, which has no such field.
    pub ch8_assignment: u16,
}

/// A rate code's `44.1k*2^n` / `48k*2^n` value, or [`Error::Unsupported`] for
/// a code the format reserves.
fn sample_rate(code: u8) -> Result<u32> {
    match code {
        0..=2 => Ok(48_000 << code),
        8..=10 => Ok(44_100 << (code - 8)),
        0xF => Ok(0), // "not present" (group2's usual value)
        other => Err(Error::unsupported(
            format!("TrueHD sample rate code {other}"),
            "reserved by the format",
        )),
    }
}

impl MajorSyncInfo {
    /// Parses the major sync starting at `data[0]` (the sync word itself),
    /// i.e. `data` is `access_unit[4..]`.
    fn parse(data: &[u8]) -> Result<MajorSyncInfo> {
        if data.len() < MAJOR_SYNC_LEN {
            return Err(Error::NeedMore);
        }
        let word = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let format = match word {
            MAJOR_SYNC_TRUEHD => MajorSyncFormat::TrueHd,
            MAJOR_SYNC_MLP => MajorSyncFormat::Mlp,
            _ => return Err(Error::corrupt("TrueHD: no major sync word")),
        };
        // TrueHD: format_info starts with the rate code; MLP puts its two
        // 4-bit word-length codes first and the rate codes one byte later.
        let rate_code = match format {
            MajorSyncFormat::TrueHd => data[4] >> 4,
            MajorSyncFormat::Mlp => data[5] >> 4,
        };
        let sample_rate = sample_rate(rate_code)?;
        // format_info's last 13 bits: data[6] low 5 bits + data[7].
        let ch8_assignment = match format {
            MajorSyncFormat::TrueHd => (u16::from(data[6] & 0x1F) << 8) | u16::from(data[7]),
            MajorSyncFormat::Mlp => 0,
        };
        Ok(MajorSyncInfo {
            format,
            sample_rate,
            rate_code,
            ch8_assignment,
        })
    }
}

/// One substream directory entry: where its coded data ends and whether a
/// checksum/parity/DRC word rides along with the end pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubstreamInfo {
    /// Raw 4-bit flag nibble; bit `0x8` is the only one this build interprets
    /// (it gates [`SubstreamInfo::extra`]).
    pub flags: u8,
    /// Cumulative end offset of this substream's coded data, in 16-bit words
    /// from the end of the substream directory.
    pub end_offset_words: u16,
    /// The second directory word, when `flags & 0x8` said one follows —
    /// observed constant (DRC default) across a real substream's directory
    /// entries; not decoded further.
    pub extra: Option<u16>,
}

/// One access unit's header: its own length, optional major sync, and the
/// substream directory that follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessUnitHeader {
    /// Total access-unit length in bytes, header included.
    pub length: usize,
    /// Input timing, carried uninterpreted.
    pub input_timing: u16,
    /// Present on roughly one access unit per video frame.
    pub major_sync: Option<MajorSyncInfo>,
    /// Byte offset where the substream directory ends and substream 0's
    /// coded data begins.
    pub data_start: usize,
    /// One entry per substream, in stream order (`substreams.len()` is the
    /// substream count).
    pub substreams: Vec<SubstreamInfo>,
}

impl AccessUnitHeader {
    /// Parses one access unit's header from `data`, which must hold at least
    /// [`frame_length`]'s worth of bytes.
    pub fn parse(data: &[u8]) -> Result<AccessUnitHeader> {
        let length = frame_length(data)?;
        if data.len() < length {
            return Err(Error::NeedMore);
        }
        let au = &data[..length];
        let input_timing = u16::from_be_bytes([au[2], au[3]]);

        let major_sync = match au.get(4..8) {
            Some(w) if w == MAJOR_SYNC_TRUEHD.to_be_bytes() || w == MAJOR_SYNC_MLP.to_be_bytes() => {
                Some(MajorSyncInfo::parse(&au[4..])?)
            }
            _ => None,
        };
        let dir_start = AU_HEADER_LEN + if major_sync.is_some() { MAJOR_SYNC_LEN } else { 0 };

        let mut substreams = Vec::new();
        let mut pos = dir_start;
        loop {
            let word = au
                .get(pos..pos + 2)
                .ok_or(Error::NeedMore)?;
            let word = u16::from_be_bytes([word[0], word[1]]);
            let flags = (word >> 12) as u8;
            let end_offset_words = word & 0x0FFF;
            pos += 2;
            let extra = if flags & 0x8 != 0 {
                let w = au.get(pos..pos + 2).ok_or(Error::NeedMore)?;
                pos += 2;
                Some(u16::from_be_bytes([w[0], w[1]]))
            } else {
                None
            };
            substreams.push(SubstreamInfo {
                flags,
                end_offset_words,
                extra,
            });
            if substreams.len() >= 4 {
                return Err(Error::unsupported(
                    "TrueHD 4th (object/16-channel) substream",
                    "only the 2/6/8-channel PCM presentations are implemented",
                ));
            }
            if pos + usize::from(end_offset_words) * 2 == length {
                break;
            }
            if pos >= length {
                return Err(Error::corrupt(
                    "TrueHD: substream directory did not sum to the access unit's length",
                ));
            }
        }

        Ok(AccessUnitHeader {
            length,
            input_timing,
            major_sync,
            data_start: pos,
            substreams,
        })
    }

    /// Byte span `(start, end)` of substream `i`'s coded data within the
    /// access unit, from the directory's cumulative end pointers.
    pub fn substream_span(&self, i: usize) -> (usize, usize) {
        let start = if i == 0 {
            self.data_start
        } else {
            self.data_start + usize::from(self.substreams[i - 1].end_offset_words) * 2
        };
        (start, self.data_start + usize::from(self.substreams[i].end_offset_words) * 2)
    }

    /// The channel layout Dolby's substream convention implies: substream 0
    /// is always a 2-ch presentation, substream 1 (when present) a discrete
    /// 5.1, substream 2 (when present) a discrete 7.1. [`parse`](Self::parse)
    /// already refuses a 4th substream, so this never sees one.
    pub fn channel_layout(&self) -> Result<ChannelLayout> {
        match self.substreams.len() {
            1 => Ok(ChannelLayout::Stereo),
            2 => Ok(ChannelLayout::Surround5_1),
            3 => Ok(ChannelLayout::Surround7_1),
            n => Err(Error::corrupt(format!("TrueHD: {n} substreams"))),
        }
    }
}

/// A TrueHD/MLP decoder for one stream: access units in, one interleaved
/// S32 (24-bit left-justified) [`AudioFrame`] of the highest presentation
/// out per access unit. See the crate-level docs for scope.
#[derive(Debug)]
pub struct TrueHdDecoder {
    params: CodecParameters,
    core: crate::decode::Core,
    pending: Option<AudioFrame>,
}

impl Default for TrueHdDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl TrueHdDecoder {
    /// A decoder with no stream state yet; TrueHD carries its own format
    /// info in-band (the major sync), so there is no magic-cookie equivalent
    /// to hand in up front.
    pub fn new() -> TrueHdDecoder {
        TrueHdDecoder {
            params: CodecParameters::new(CodecId::TrueHd),
            core: crate::decode::Core::new(),
            pending: None,
        }
    }

    /// The stream-side self-check tallies so far (restart CRC, parity,
    /// lossless check); all stay zero on a clean decode.
    pub fn check_stats(&self) -> crate::decode::CheckStats {
        self.core.stats
    }

    /// One access unit in; one frame of the highest presentation out —
    /// `None` until the first major sync and every substream's first
    /// restart header have been seen.
    pub fn push(&mut self, data: &[u8]) -> Result<Option<AudioFrame>> {
        self.decode_access_unit(data)
    }

    /// See [`TrueHdDecoder::push`].
    pub fn decode_access_unit(&mut self, data: &[u8]) -> Result<Option<AudioFrame>> {
        let header = AccessUnitHeader::parse(data)?;
        self.core.decode(&header, data)
    }
}

impl Decoder for TrueHdDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        if let Some(mut frame) = self.decode_access_unit(&packet.data)? {
            frame.pts = packet
                .pts
                .map(|ticks| ec_core::timebase::Timestamp::new(ticks, packet.time_base));
            self.pending = Some(frame);
        }
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending.take().map(Frame::Audio).ok_or(Error::NeedMore)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn reset(&mut self) {
        self.pending = None;
        self.core.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-built access unit with no major sync, one substream (a bare
    /// 2-channel presentation): 4-byte header + a 2-byte directory entry +
    /// 4 bytes of "coded data" nobody reads yet.
    fn stereo_au() -> Vec<u8> {
        vec![
            0x30, 0x05, // check nibble 3, length 5 words (10 bytes)
            0x12, 0x34, // input timing (uninterpreted)
            0x00, 0x02, // substream 0: flags 0 (no extra word), end +2 words
            0xAA, 0xBB, 0xCC, 0xDD, // "coded data"
        ]
    }

    /// A hand-built access unit that does carry a major sync (TrueHD,
    /// 48 kHz) and two substreams (a 5.1 presentation on top of the 2-ch
    /// one).
    fn surround51_au() -> Vec<u8> {
        let mut au = vec![
            0x50, 0x16, // check nibble 5, length 22 words (44 bytes)
            0x00, 0x00, // input timing
        ];
        au.extend_from_slice(&MAJOR_SYNC_TRUEHD.to_be_bytes());
        au.push(0x0F); // sample rate: group1 code 0 (48000), group2 0xF (absent)
        au.extend(std::iter::repeat_n(0u8, 23)); // rest of the major sync, unused here
        au.extend_from_slice(&[0x00, 0x01]); // substream 0: end +1 word (not the last)
        au.extend_from_slice(&[0x00, 0x04]); // substream 1: end +4 words (lands on EOF)
        au.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        au
    }

    #[test]
    fn frame_length_reads_the_16_bit_word() {
        assert_eq!(frame_length(&stereo_au()).unwrap(), 10);
        assert_eq!(frame_length(&surround51_au()).unwrap(), 44);
    }

    #[test]
    fn frame_length_needs_two_bytes() {
        assert!(matches!(frame_length(&[0x30]), Err(Error::NeedMore)));
        assert!(matches!(frame_length(&[]), Err(Error::NeedMore)));
    }

    #[test]
    fn parses_a_stereo_access_unit_with_no_major_sync() {
        let au = AccessUnitHeader::parse(&stereo_au()).unwrap();
        assert_eq!(au.length, 10);
        assert_eq!(au.input_timing, 0x1234);
        assert!(au.major_sync.is_none());
        assert_eq!(au.substreams.len(), 1);
        assert_eq!(au.channel_layout().unwrap(), ChannelLayout::Stereo);
    }

    #[test]
    fn parses_a_5_1_access_unit_with_a_major_sync() {
        let au = AccessUnitHeader::parse(&surround51_au()).unwrap();
        assert_eq!(au.length, 44);
        let sync = au.major_sync.expect("major sync detected");
        assert_eq!(sync.format, MajorSyncFormat::TrueHd);
        assert_eq!(sync.sample_rate, 48_000);
        assert_eq!(au.substreams.len(), 2);
        assert_eq!(au.channel_layout().unwrap(), ChannelLayout::Surround5_1);
    }

    #[test]
    fn a_corrupted_major_sync_word_does_not_panic() {
        let mut au = surround51_au();
        // Flip a byte inside the format-sync word so it names neither
        // TrueHD nor MLP; the directory walk that follows now reads
        // major-sync bytes as if they were directory entries and cannot
        // land on the access unit's real end, but must not panic.
        au[5] = 0x00;
        assert!(AccessUnitHeader::parse(&au).is_err());
    }

    #[test]
    fn a_truncated_access_unit_is_need_more() {
        let au = surround51_au();
        // The header claims 44 bytes; hand over fewer.
        assert!(matches!(
            AccessUnitHeader::parse(&au[..20]),
            Err(Error::NeedMore)
        ));
    }

    #[test]
    fn a_fourth_substream_is_named_unsupported() {
        // A 20-byte access unit, followed by five directory entries whose
        // end pointers never land on that length -- forces the walk past
        // the 4-substream cap before it can terminate.
        let mut au = vec![0x00, 0x0A, 0x00, 0x00]; // length 10 words = 20 bytes
        for _ in 0..5 {
            au.extend_from_slice(&[0x00, 0x01]);
        }
        au.resize(20, 0);
        let err = AccessUnitHeader::parse(&au).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    }

    #[test]
    fn a_well_formed_4_entry_directory_is_still_named_unsupported() {
        // A 20-byte access unit whose directory has exactly 4 entries, the
        // last of which lands squarely on the access unit's end (the
        // well-formed object-audio shape, not a walk that overruns a
        // malformed directory). This must be refused by name, not parsed as
        // Ok(4) and only caught later in channel_layout().
        let mut au = vec![0x00, 0x0A, 0x00, 0x00]; // length 10 words = 20 bytes
        au.extend_from_slice(&[0x00, 0x01]); // substream 0: doesn't land on EOF yet
        au.extend_from_slice(&[0x00, 0x01]); // substream 1: doesn't land on EOF yet
        au.extend_from_slice(&[0x00, 0x01]); // substream 2: doesn't land on EOF yet
        au.extend_from_slice(&[0x00, 0x04]); // substream 3: pos(12) + 4*2 == 20, lands on EOF
        au.resize(20, 0);
        let err = AccessUnitHeader::parse(&au).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    }

    #[test]
    fn a_well_formed_3_entry_directory_still_parses() {
        // Control for the test above: the same shape one entry shorter, its
        // last pointer landing on the access unit's end, must still parse.
        let mut au = vec![0x00, 0x0A, 0x00, 0x00]; // length 10 words = 20 bytes
        au.extend_from_slice(&[0x00, 0x01]); // substream 0: doesn't land on EOF yet
        au.extend_from_slice(&[0x00, 0x01]); // substream 1: doesn't land on EOF yet
        au.extend_from_slice(&[0x00, 0x05]); // substream 2: pos(10) + 5*2 == 20, lands on EOF
        au.resize(20, 0);
        let parsed = AccessUnitHeader::parse(&au).unwrap();
        assert_eq!(parsed.substreams.len(), 3);
        assert_eq!(parsed.channel_layout().unwrap(), ChannelLayout::Surround7_1);
    }

    #[test]
    fn decoder_yields_nothing_before_a_major_sync() {
        let mut decoder = TrueHdDecoder::new();
        assert_eq!(decoder.decode_access_unit(&stereo_au()).unwrap(), None);
    }

    #[test]
    fn substream_spans_follow_the_directory() {
        let au = AccessUnitHeader::parse(&surround51_au()).unwrap();
        assert_eq!(au.substream_span(0), (36, 38));
        assert_eq!(au.substream_span(1), (38, 44));
    }
}
