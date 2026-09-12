//! Frame-level structures and the raw (uncompressed) frame tag
//! (RFC 6386 §9.1).

use ec_core::{Error, Result};

/// Key frame or interframe — the 1-bit frame type from the frame tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    /// Key frame (`frame_type` bit 0).
    Key,
    /// Interframe (`frame_type` bit 1); predicted from earlier frames.
    Inter,
}

/// The 3-byte frame tag every frame starts with (RFC 6386 §9.1 item 1-4):
/// 1 bit type, 3 bits version, 1 bit show_frame, 19 bits first-partition
/// size — all read out of one little-endian 24-bit word.
#[derive(Debug, Clone, Copy)]
pub struct FrameTag {
    /// Key or inter frame.
    pub frame_type: FrameType,
    /// 3-bit version (reconstruction-filter / loop-filter profile; only
    /// bits that matter historically — the header's own loop filter fields
    /// govern decoding).
    pub version: u8,
    /// Whether the decoded frame is for display.
    pub show_frame: bool,
    /// Size in bytes of the first (header/mode) data partition.
    pub first_part_size: u32,
}

/// Bytes occupied by the 3-byte frame tag.
pub const FRAME_TAG_SZ: usize = 3;

/// Bytes occupied by the key frame's start code + size fields (§9.1).
pub const KEYFRAME_HEADER_SZ: usize = 7;

impl FrameTag {
    /// Parse the tag from the first 3 bytes of a frame.
    pub fn parse(data: &[u8]) -> Result<FrameTag> {
        if data.len() < FRAME_TAG_SZ {
            return Err(Error::corrupt("VP8 frame shorter than its 3-byte tag"));
        }
        let tag = u32::from(data[0]) | (u32::from(data[1]) << 8) | (u32::from(data[2]) << 16);
        Ok(FrameTag {
            frame_type: if tag & 1 == 0 {
                FrameType::Key
            } else {
                FrameType::Inter
            },
            version: ((tag >> 1) & 7) as u8,
            show_frame: (tag >> 4) & 1 == 1,
            first_part_size: tag >> 5,
        })
    }
}

/// Key-frame dimensions and scaling flags (RFC 6386 §9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyFrameDims {
    /// Encoded (pre-scaling) width in pixels, 14 bits.
    pub width: u16,
    /// Encoded height in pixels, 14 bits.
    pub height: u16,
    /// 2-bit horizontal scale (0 = none, 1 = 5/4, 2 = 5/3, 3 = 2).
    pub h_scale: u8,
    /// 2-bit vertical scale.
    pub v_scale: u8,
}

/// Parse the key-frame start code (`9d 01 2a`) and dimensions that follow
/// the frame tag. `data` starts at the frame tag.
pub fn parse_keyframe_dims(data: &[u8]) -> Result<KeyFrameDims> {
    let dims_off = FRAME_TAG_SZ;
    if data.len() < dims_off + KEYFRAME_HEADER_SZ {
        return Err(Error::corrupt("VP8 key frame header truncated"));
    }
    let c = &data[dims_off..];
    if c[0] != 0x9d || c[1] != 0x01 || c[2] != 0x2a {
        return Err(Error::corrupt(format!(
            "VP8 key frame start code 9d 01 2a not found (got {:02x} {:02x} {:02x})",
            c[0], c[1], c[2]
        )));
    }
    let w = u16::from_le_bytes([c[3], c[4]]);
    let h = u16::from_le_bytes([c[5], c[6]]);
    Ok(KeyFrameDims {
        width: w & 0x3fff,
        height: h & 0x3fff,
        h_scale: (w >> 14) as u8,
        v_scale: (h >> 14) as u8,
    })
}

/// Number of 16×16 macroblock columns/rows covering `width × height`
/// pixels (the encoded frame is padded up to whole macroblocks).
pub fn mb_geometry(width: u16, height: u16) -> (usize, usize) {
    (
        (usize::from(width) + 15) / 16,
        (usize::from(height) + 15) / 16,
    )
}
