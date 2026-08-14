//! EBML: the element grammar Matroska is written in, both directions.
//!
//! An element is an id, a size and a payload, all three of them
//! variable-length. Ids are compared *with* their leading marker bit — that is
//! how the spec writes them and how the constants below are spelled — while a
//! size has the marker stripped, and an all-ones size means "unknown", which is
//! what a muxer writes while it is still recording.

use ec_core::{Error, Result};

// Element ids, marker bit included.
pub const EBML_HEADER: u32 = 0x1A45_DFA3;
pub const EBML_VERSION: u32 = 0x4286;
pub const EBML_READ_VERSION: u32 = 0x42F7;
pub const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
pub const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
pub const DOC_TYPE: u32 = 0x4282;
pub const DOC_TYPE_VERSION: u32 = 0x4287;
pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;
pub const VOID: u32 = 0xEC;

pub const SEGMENT: u32 = 0x1853_8067;
pub const INFO: u32 = 0x1549_A966;
pub const TIMESTAMP_SCALE: u32 = 0x2AD7B1;
pub const DURATION: u32 = 0x4489;
pub const MUXING_APP: u32 = 0x4D80;
pub const WRITING_APP: u32 = 0x5741;

pub const TRACKS: u32 = 0x1654_AE6B;
pub const TRACK_ENTRY: u32 = 0xAE;
pub const TRACK_NUMBER: u32 = 0xD7;
pub const TRACK_UID: u32 = 0x73C5;
pub const TRACK_TYPE: u32 = 0x83;
pub const FLAG_LACING: u32 = 0x9C;
pub const CODEC_ID: u32 = 0x86;
pub const CODEC_PRIVATE: u32 = 0x63A2;
pub const CODEC_DELAY: u32 = 0x56AA;
pub const SEEK_PRE_ROLL: u32 = 0x56BB;
pub const DEFAULT_DURATION: u32 = 0x23E383;
pub const TRACK_LANGUAGE: u32 = 0x22B59C;
pub const TRACK_LANGUAGE_BCP47: u32 = 0x22B59D;

pub const VIDEO: u32 = 0xE0;
pub const PIXEL_WIDTH: u32 = 0xB0;
pub const PIXEL_HEIGHT: u32 = 0xBA;
pub const DISPLAY_WIDTH: u32 = 0x54B0;
pub const DISPLAY_HEIGHT: u32 = 0x54BA;
pub const COLOUR: u32 = 0x55B0;
pub const MATRIX_COEFFICIENTS: u32 = 0x55B1;
pub const RANGE: u32 = 0x55B9;
pub const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
pub const PRIMARIES: u32 = 0x55BB;
pub const MAX_CLL: u32 = 0x55BC;
pub const MAX_FALL: u32 = 0x55BD;
pub const MASTERING_METADATA: u32 = 0x55D0;
pub const LUMINANCE_MAX: u32 = 0x55D9;
pub const LUMINANCE_MIN: u32 = 0x55DA;

pub const AUDIO: u32 = 0xE1;
pub const SAMPLING_FREQUENCY: u32 = 0xB5;
pub const CHANNELS: u32 = 0x9F;
pub const BIT_DEPTH: u32 = 0x6264;

pub const CONTENT_ENCODINGS: u32 = 0x6D80;
pub const CONTENT_ENCODING: u32 = 0x6240;
pub const CONTENT_ENCODING_SCOPE: u32 = 0x5032;
pub const CONTENT_ENCODING_TYPE: u32 = 0x5033;
pub const CONTENT_COMPRESSION: u32 = 0x5034;
pub const CONTENT_COMP_ALGO: u32 = 0x4254;
pub const CONTENT_COMP_SETTINGS: u32 = 0x4255;

pub const SEEK_HEAD: u32 = 0x114D_9B74;
pub const SEEK: u32 = 0x4DBB;
pub const SEEK_ID: u32 = 0x53AB;
pub const SEEK_POSITION: u32 = 0x53AC;
pub const CUES: u32 = 0x1C53_BB6B;
pub const CUE_POINT: u32 = 0xBB;
pub const CUE_TIME: u32 = 0xB3;
pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
pub const CUE_TRACK: u32 = 0xF7;
pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
pub const CUE_RELATIVE_POSITION: u32 = 0xF0;

pub const CLUSTER: u32 = 0x1F43_B675;
pub const CLUSTER_TIMESTAMP: u32 = 0xE7;
pub const SIMPLE_BLOCK: u32 = 0xA3;
pub const BLOCK_GROUP: u32 = 0xA0;
pub const BLOCK: u32 = 0xA1;
pub const BLOCK_DURATION: u32 = 0x9B;
pub const REFERENCE_BLOCK: u32 = 0xFB;

/// The four bytes a `Cluster` id is written as, which is what a damaged file is
/// resynchronised on.
pub const CLUSTER_MAGIC: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];

/// EBML variable-length integer: the leading zeros of the first byte say how
/// many bytes it takes. `strip` clears the marker bit, which is what a *size*
/// wants and an *id* does not — an id is written and compared with it. An
/// all-ones size means unknown length, and comes back as [`u64::MAX`].
pub fn vint(buf: &[u8], strip: bool) -> Result<(u64, usize)> {
    let &first = buf.first().ok_or(Error::NeedMore)?;
    if first == 0 {
        // A 9-byte-or-longer integer; Matroska defines none.
        return Err(Error::corrupt(
            "EBML: variable-length integer wider than 8 bytes",
        ));
    }
    let len = first.leading_zeros() as usize + 1;
    let bytes = buf.get(..len).ok_or(Error::NeedMore)?;
    // `0xFF >> len` in `u16`: an 8-byte integer shifts a `u8` mask right off
    // its own end, and the marker bit is then all the first byte was.
    let mut value = u64::from(if strip {
        first & (0xFFu16 >> len) as u8
    } else {
        first
    });
    for &b in &bytes[1..] {
        value = (value << 8) | u64::from(b);
    }
    if strip && value == (1u64 << (7 * len)) - 1 {
        return Ok((u64::MAX, len));
    }
    Ok((value, len))
}

/// Id, size and header length of the element at the head of `buf`. The size is
/// [`None`] for an unknown-length element.
pub fn header(buf: &[u8]) -> Result<(u32, Option<u64>, usize)> {
    let (id, id_len) = vint(buf, false)?;
    let (size, size_len) = vint(buf.get(id_len..).ok_or(Error::NeedMore)?, true)?;
    let id =
        u32::try_from(id).map_err(|_| Error::corrupt("EBML: element id wider than 4 bytes"))?;
    Ok((id, (size != u64::MAX).then_some(size), id_len + size_len))
}

/// The children of an element already in memory, as `(id, payload range)`.
///
/// Iteration stops at the first header that does not fit, which is how a caller
/// bounds a child to its parent: by handing over the parent's bytes.
pub struct Elements<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Elements<'a> {
    pub fn new(buf: &'a [u8]) -> Elements<'a> {
        Elements { buf, at: 0 }
    }

    /// How far the walk got, in bytes from the start of the buffer.
    pub fn offset(&self) -> usize {
        self.at
    }
}

impl Iterator for Elements<'_> {
    type Item = (u32, std::ops::Range<usize>);

    fn next(&mut self) -> Option<(u32, std::ops::Range<usize>)> {
        if self.at >= self.buf.len() {
            return None;
        }
        let Ok((id, size, head)) = header(&self.buf[self.at..]) else {
            return None;
        };
        let body = self.at + head;
        // An unknown-length child inside a buffer runs to the end of it, which
        // is the same rule the file-level walk applies to its parent.
        let stop = match size {
            Some(size) => match usize::try_from(size).ok().and_then(|s| body.checked_add(s)) {
                Some(stop) if stop <= self.buf.len() => stop,
                _ => return None,
            },
            None => self.buf.len(),
        };
        self.at = stop;
        Some((id, body..stop))
    }
}

/// An unsigned EBML integer, big-endian in as many bytes as it was written
/// with. An absent element is a zero by the spec's own default.
pub fn uint_of(body: &[u8]) -> u64 {
    body.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

/// An EBML float: IEEE big-endian, 4 or 8 bytes wide. Zero for any other width,
/// which is what an absent one is by spec.
pub fn float_of(body: &[u8]) -> f64 {
    match body.len() {
        4 => f64::from(f32::from_be_bytes([body[0], body[1], body[2], body[3]])),
        8 => f64::from_be_bytes([
            body[0], body[1], body[2], body[3], body[4], body[5], body[6], body[7],
        ]),
        _ => 0.0,
    }
}

/// An EBML string, trimmed of the zero padding a muxer may pad it to width with.
pub fn string_of(body: &[u8]) -> String {
    let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    String::from_utf8_lossy(&body[..end]).into_owned()
}

// ---------------------------------------------------------------- writing ---

/// An element id, in the bytes it is written as.
pub fn put_id(out: &mut Vec<u8>, id: u32) {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take_while(|&&b| b == 0).count();
    out.extend_from_slice(&bytes[skip..]);
}

/// An element size as a variable-length integer, in the fewest bytes that hold
/// it. A value of all ones means *unknown* length, so a size that would land on
/// one takes a byte more.
pub fn put_size(out: &mut Vec<u8>, size: u64) {
    let mut len = 1;
    while len < 8 && size >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    let value = (1u64 << (7 * len)) | size;
    out.extend_from_slice(&value.to_be_bytes()[8 - len..]);
}

/// A whole element: id, size, payload.
pub fn elem(out: &mut Vec<u8>, id: u32, payload: &[u8]) {
    put_id(out, id);
    put_size(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

/// An unsigned integer element, big-endian in as few bytes as it takes (one at
/// the least: a zero is a byte, not an absence).
pub fn uint(out: &mut Vec<u8>, id: u32, value: u64) {
    let bytes = value.to_be_bytes();
    let skip = bytes.iter().take_while(|&&b| b == 0).count().min(7);
    elem(out, id, &bytes[skip..]);
}

/// A 64-bit float element.
pub fn float(out: &mut Vec<u8>, id: u32, value: f64) {
    elem(out, id, &value.to_be_bytes());
}

/// How many bytes an element's id and size take, which is what an offset inside
/// a payload has to be moved by to become an offset in the file.
pub fn elem_head_len(id: u32, payload: usize) -> usize {
    let (mut head, mut size) = (Vec::new(), Vec::new());
    put_id(&mut head, id);
    put_size(&mut size, payload as u64);
    head.len() + size.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vints_round_trip_through_both_ends() {
        for value in [0u64, 1, 126, 127, 128, 16382, 16383, 1 << 40, (1 << 56) - 2] {
            let mut out = Vec::new();
            put_size(&mut out, value);
            let (back, len) = vint(&out, true).expect("written size parses");
            assert_eq!(back, value, "size {value} round-trips");
            assert_eq!(len, out.len());
        }
        // All-ones is "unknown length" and never spelled by a real size.
        assert_eq!(vint(&[0xFF], true).unwrap(), (u64::MAX, 1));
        assert_eq!(vint(&[0x81], false).unwrap(), (0x81, 1));
        // An id keeps its marker bit; a size loses it.
        let mut out = Vec::new();
        put_id(&mut out, SEGMENT);
        assert_eq!(vint(&out, false).unwrap(), (u64::from(SEGMENT), 4));
        // Malformed and truncated are errors, never panics.
        assert!(vint(&[], true).unwrap_err().is_need_more());
        assert!(vint(&[0x00, 1, 2], true).is_err());
        assert!(vint(&[0x40], true).unwrap_err().is_need_more());
    }

    #[test]
    fn elements_walk_children_and_flag_truncation() {
        let mut buf = Vec::new();
        uint(&mut buf, TRACK_NUMBER, 1);
        elem(&mut buf, CODEC_ID, b"V_AV1");
        float(&mut buf, DURATION, 1234.5);
        let got: Vec<_> = Elements::new(&buf)
            .map(|(id, r)| (id, buf[r].to_vec()))
            .collect();
        assert_eq!(got.len(), 3);
        assert_eq!(uint_of(&got[0].1), 1);
        assert_eq!(string_of(&got[1].1), "V_AV1");
        assert_eq!(float_of(&got[2].1), 1234.5);

        // A child running past its parent ends the walk instead of reading
        // bytes that are not its own.
        let short = &buf[..buf.len() - 2];
        assert_eq!(Elements::new(short).count(), 2);
    }
}
