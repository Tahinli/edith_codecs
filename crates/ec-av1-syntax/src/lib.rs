//! AV1 bitstream syntax: OBU framing, the sequence header and the frame header.
//!
//! This crate parses what a *stateless* hardware decoder has to be told before
//! it can decode a frame — everything outside the entropy-coded tile data. That
//! is the whole of `sequence_header_obu` and `uncompressed_header` plus the tile
//! group headers, which is exactly the shape of
//! `VADecPictureParameterBufferAV1` and `VASliceParameterBufferAV1`; the tile
//! payloads themselves are handed to the driver as bytes, located by
//! [`Tile::offset`] and [`Tile::size`].
//!
//! [`Av1Parser`] holds the state the format spreads across frames: the sequence
//! header (without which a frame header cannot even be parsed), the eight
//! reference slots with their sizes, order hints, saved segmentation, loop
//! filter deltas, global motion and film grain, and the reference update after
//! each frame (spec 7.20). Feed it every OBU of a stream in order and it answers
//! with headers whose fields are the spec's, under the spec's names.
//!
//! What is deliberately not here: the symbol decoder and everything behind it
//! (tile data, CDFs), Annex B length-delimited framing (containers hand over the
//! low-overhead format), the large scale tile mode, and metadata payloads —
//! [`metadata_type`] names the type and leaves the body to a caller that has a
//! use for HDR metadata, which is [`ec_core::color`]'s business rather than a
//! decode submission's.
//!
//! Malformed input is an [`Error`], never a panic: truncation is
//! [`Error::NeedMore`], a rule violation is [`Error::Corrupt`].
//!
//! AV1 is covered by the AOMedia Patent License 1.0, which grants what an
//! independent implementation of the specification needs; this crate is such an
//! implementation, written from the specification alone.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod frame;
pub mod obu;
pub mod sequence;
mod warp;

use ec_core::{BitReader, Error, Result};

pub use frame::{
    CdefParams, DeltaParams, FilmGrainParams, FrameHeader, FrameType, InterpolationFilter,
    LoopFilterParams, LoopRestorationParams, QuantizationParams, ReferenceSlot, RestorationType,
    SegmentationParams, Tile, TileInfo, TxMode, WarpModel, WarpParams,
};
pub use obu::{ObuHeader, ObuType};
pub use sequence::{
    ChromaSamplePosition, ColorConfig, DecoderModelInfo, OperatingPoint, SequenceHeader,
    TimingInfo, metadata_type,
};

use frame::{FrameState, apply_refresh, apply_show_existing_refresh, parse_tile_group};

/// Reference frame slots in the AV1 decoded frame buffer (spec 3).
pub const NUM_REF_FRAMES: usize = 8;
/// References a single inter frame may name: LAST through ALTREF (spec 3).
pub const REFS_PER_FRAME: usize = 7;
/// References including intra, which is what the loop filter deltas are indexed by.
pub const TOTAL_REFS_PER_FRAME: usize = 8;
/// `PRIMARY_REF_NONE` (spec 3): this frame inherits no probability or filter state.
pub const PRIMARY_REF_NONE: u8 = 7;
/// Number of segments a frame can define (spec 3).
pub const MAX_SEGMENTS: usize = 8;
/// Number of per-segment features (spec 3).
pub const SEG_LVL_MAX: usize = 8;
/// Segment feature: alternate quantizer index (spec 3).
pub const SEG_LVL_ALT_Q: usize = 0;
/// Segment feature: alternate loop filter level, luma vertical (spec 3).
pub const SEG_LVL_ALT_LF_Y_V: usize = 1;
/// Segment feature: reference frame override (spec 3).
pub const SEG_LVL_REF_FRAME: usize = 5;
/// Segment feature: skip (spec 3).
pub const SEG_LVL_SKIP: usize = 6;
/// Segment feature: global motion vector (spec 3).
pub const SEG_LVL_GLOBALMV: usize = 7;
/// `MAX_TILE_WIDTH` in samples (spec 3).
pub const MAX_TILE_WIDTH: u32 = 4096;
/// `MAX_TILE_AREA` in samples (spec 3).
pub const MAX_TILE_AREA: u32 = 4096 * 2304;
/// `MAX_TILE_COLS` (spec 3).
pub const MAX_TILE_COLS: u32 = 64;
/// `MAX_TILE_ROWS` (spec 3).
pub const MAX_TILE_ROWS: u32 = 64;
/// `RESTORATION_TILESIZE_MAX` (spec 3).
pub const RESTORATION_TILESIZE_MAX: u32 = 256;

/// What an OBU turned out to hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObuKind {
    /// `OBU_SEQUENCE_HEADER`.
    SequenceHeader(Box<SequenceHeader>),
    /// `OBU_TEMPORAL_DELIMITER`: the start of a temporal unit, no payload.
    TemporalDelimiter,
    /// `OBU_FRAME_HEADER`, or `OBU_REDUNDANT_FRAME_HEADER` (whose parse changes
    /// no state, as the spec requires).
    FrameHeader(Box<FrameHeader>),
    /// `OBU_TILE_GROUP`, belonging to the frame header that preceded it.
    TileGroup(Vec<Tile>),
    /// `OBU_FRAME`: a frame header and its tile group in one OBU.
    Frame(Box<FrameHeader>, Vec<Tile>),
    /// `OBU_METADATA`, with its `metadata_type` (spec 6.7.1).
    Metadata(u32),
    /// `OBU_TILE_LIST`, the large scale tile mode this crate does not parse.
    TileList,
    /// `OBU_PADDING`.
    Padding,
    /// A reserved OBU type, skipped by size.
    Reserved(u8),
}

/// One parsed OBU, and where it sat in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedObu {
    /// The OBU header.
    pub header: ObuHeader,
    /// Byte offset of the OBU header from the start of the buffer parsed.
    pub offset: usize,
    /// Total bytes the OBU occupies, header and size field included.
    pub total_size: usize,
    /// Byte offset of the OBU payload from the start of the buffer parsed.
    pub payload_offset: usize,
    /// `obu_size`: payload length in bytes.
    pub payload_size: usize,
    /// What the payload held.
    pub kind: ObuKind,
}

/// An AV1 OBU parser carrying the state the format spreads across frames.
#[derive(Debug, Clone, Default)]
pub struct Av1Parser {
    sequence: Option<SequenceHeader>,
    state: FrameState,
    /// The frame header a following `OBU_TILE_GROUP` belongs to.
    pending: Option<FrameHeader>,
}

impl Av1Parser {
    /// A parser with no sequence header and no reference state.
    pub fn new() -> Av1Parser {
        Av1Parser::default()
    }

    /// The sequence header in force, once one has been parsed.
    pub fn sequence_header(&self) -> Option<&SequenceHeader> {
        self.sequence.as_ref()
    }

    /// The reference slots as they stand, after every frame parsed so far.
    pub fn reference_slots(&self) -> &[ReferenceSlot; NUM_REF_FRAMES] {
        &self.state.refs
    }

    /// Parse every OBU in one temporal unit (or any run of whole OBUs).
    ///
    /// Offsets in the result — including [`Tile::offset`] — are relative to the
    /// start of `data`, so they can be handed to a decoder that submits `data`
    /// as one buffer.
    pub fn parse_temporal_unit(&mut self, data: &[u8]) -> Result<Vec<ParsedObu>> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < data.len() {
            let obu = self.parse_obu_at(data, pos)?;
            pos += obu.total_size;
            out.push(obu);
        }
        Ok(out)
    }

    /// Parse the single OBU that starts at the beginning of `data`.
    pub fn parse_obu(&mut self, data: &[u8]) -> Result<ParsedObu> {
        self.parse_obu_at(data, 0)
    }

    fn parse_obu_at(&mut self, data: &[u8], offset: usize) -> Result<ParsedObu> {
        let buf = data.get(offset..).ok_or(Error::NeedMore)?;
        let mut r = BitReader::new(buf);
        let header = ObuHeader::parse(&mut r)?;
        let payload_size = if header.has_size_field {
            obu::read_leb128(&mut r)? as usize
        } else {
            buf.len()
                .checked_sub(1 + usize::from(header.extension_flag))
                .ok_or(Error::NeedMore)?
        };
        let header_bytes = (r.bit_position() / 8) as usize;
        let payload = buf
            .get(header_bytes..header_bytes + payload_size)
            .ok_or(Error::NeedMore)?;
        let payload_offset = offset + header_bytes;

        // Layer dropping (spec 5.3.1): an OBU outside the selected operating
        // point is skipped without being parsed.
        let dropped = self.sequence.as_ref().is_some_and(|seq| {
            seq.operating_point_idc != 0
                && header.extension_flag
                && !matches!(
                    header.obu_type,
                    ObuType::SequenceHeader | ObuType::TemporalDelimiter
                )
                && !(((seq.operating_point_idc >> header.temporal_id) & 1 != 0)
                    && ((seq.operating_point_idc >> (header.spatial_id + 8)) & 1 != 0))
        });

        let kind = if dropped {
            ObuKind::Reserved(0)
        } else {
            self.parse_payload(&header, payload, payload_offset)?
        };

        Ok(ParsedObu {
            header,
            offset,
            total_size: header_bytes + payload_size,
            payload_offset,
            payload_size,
            kind,
        })
    }

    fn parse_payload(
        &mut self,
        header: &ObuHeader,
        payload: &[u8],
        payload_offset: usize,
    ) -> Result<ObuKind> {
        match header.obu_type {
            ObuType::SequenceHeader => {
                let seq = SequenceHeader::parse(&mut BitReader::new(payload))?;
                self.sequence = Some(seq.clone());
                Ok(ObuKind::SequenceHeader(Box::new(seq)))
            }
            ObuType::TemporalDelimiter => Ok(ObuKind::TemporalDelimiter),
            ObuType::FrameHeader | ObuType::RedundantFrameHeader => {
                let redundant = header.obu_type == ObuType::RedundantFrameHeader;
                let h = self.parse_frame_header(payload, header, redundant)?;
                Ok(ObuKind::FrameHeader(Box::new(h)))
            }
            ObuType::TileGroup => {
                let info = self
                    .pending
                    .as_ref()
                    .map(|h| h.tile_info.clone())
                    .ok_or_else(|| {
                        Error::corrupt("AV1 tile group with no preceding frame header")
                    })?;
                Ok(ObuKind::TileGroup(parse_tile_group(
                    payload,
                    payload_offset,
                    &info,
                )?))
            }
            ObuType::Frame => {
                let mut r = BitReader::new(payload);
                let h = self.parse_frame_header_from(&mut r, header, false)?;
                r.align_to_byte();
                let consumed = (r.bit_position() / 8) as usize;
                let tiles = parse_tile_group(
                    payload.get(consumed..).ok_or(Error::NeedMore)?,
                    payload_offset + consumed,
                    &h.tile_info,
                )?;
                Ok(ObuKind::Frame(Box::new(h), tiles))
            }
            ObuType::Metadata => Ok(ObuKind::Metadata(metadata_type(payload)?)),
            ObuType::TileList => Ok(ObuKind::TileList),
            ObuType::Padding => Ok(ObuKind::Padding),
            ObuType::Reserved(code) => Ok(ObuKind::Reserved(code)),
        }
    }

    fn parse_frame_header(
        &mut self,
        payload: &[u8],
        obu: &ObuHeader,
        redundant: bool,
    ) -> Result<FrameHeader> {
        let mut r = BitReader::new(payload);
        self.parse_frame_header_from(&mut r, obu, redundant)
    }

    fn parse_frame_header_from(
        &mut self,
        r: &mut BitReader<'_>,
        obu: &ObuHeader,
        redundant: bool,
    ) -> Result<FrameHeader> {
        let seq = self
            .sequence
            .clone()
            .ok_or_else(|| Error::corrupt("AV1 frame header before any sequence header"))?;
        // A redundant frame header must leave the decoder exactly as it found
        // it (spec 5.9.1), so it parses against a scratch copy of the state.
        let mut scratch;
        let state = if redundant {
            scratch = self.state.clone();
            &mut scratch
        } else {
            &mut self.state
        };
        let h = frame::parse_uncompressed_header(r, &seq, state, obu.temporal_id, obu.spatial_id)?;
        if !redundant {
            if h.show_existing_frame {
                apply_show_existing_refresh(&mut self.state, &h);
            } else {
                apply_refresh(&mut self.state, &h, seq.color_config.bit_depth);
            }
            self.pending = Some(h.clone());
        }
        Ok(h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_without_a_sequence_header_is_corrupt() {
        let mut p = Av1Parser::new();
        // OBU_FRAME_HEADER, has_size_field, size 1, one payload byte.
        let err = p.parse_obu(&[0x1a, 0x01, 0x00]).unwrap_err();
        assert!(matches!(err, Error::Corrupt { .. }), "{err}");
    }

    #[test]
    fn temporal_delimiter_round_trips() {
        let mut p = Av1Parser::new();
        let obus = p.parse_temporal_unit(&[0x12, 0x00]).unwrap();
        assert_eq!(obus.len(), 1);
        assert_eq!(obus[0].kind, ObuKind::TemporalDelimiter);
        assert_eq!(obus[0].total_size, 2);
    }

    #[test]
    fn truncated_obu_is_need_more() {
        let mut p = Av1Parser::new();
        // Claims a 100-byte payload that is not there.
        assert!(matches!(
            p.parse_obu(&[0x0a, 0x64, 0x00]),
            Err(Error::NeedMore)
        ));
    }

    #[test]
    fn tile_group_without_a_frame_header_is_corrupt() {
        let mut p = Av1Parser::new();
        assert!(matches!(
            p.parse_obu(&[0x22, 0x01, 0x00]),
            Err(Error::Corrupt { .. })
        ));
    }
}
