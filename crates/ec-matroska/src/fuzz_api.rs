//! Entry points the fuzz targets reach below the public API: the EBML walk, lacing,
//! block headers, `Tracks` (with `CodecPrivate` and `ContentEncodings`), and the
//! `Cues`/`SeekHead` index. Each function is a thin call into the module that owns
//! the parser. Hidden from the docs; not a stable interface.

use std::ops::Range;

use ec_core::{Result, TimeBase};

use crate::{demux, ebml};

/// How deep [`ebml_walk`] descends into nested masters.
const WALK_DEPTH: u32 = 8;

/// Walks `buf` as EBML: the head element, then every child of every master up to
/// [`WALK_DEPTH`], each payload read as an unsigned, float and string. Returns the
/// number of elements visited.
pub fn ebml_walk(buf: &[u8]) -> usize {
    let _ = ebml::header(buf);
    let _ = ebml::vint(buf, true);
    let _ = ebml::vint(buf, false);
    walk(buf, 0)
}

fn walk(buf: &[u8], depth: u32) -> usize {
    let mut count = 0;
    for (_, range) in ebml::Elements::new(buf) {
        let body = &buf[range];
        let _ = (
            ebml::uint_of(body),
            ebml::float_of(body),
            ebml::string_of(body),
        );
        count += 1;
        if depth < WALK_DEPTH {
            count += walk(body, depth + 1);
        }
    }
    count
}

/// `buf` as the payload of a laced block with the given lacing (1 Xiph, 2 fixed,
/// 3 EBML). Returns the frame ranges.
pub fn lace(buf: &[u8], lacing: u8) -> Result<Vec<Range<usize>>> {
    demux::lace(buf, 0, buf.len(), lacing)
}

/// `buf` as a whole `SimpleBlock` or `Block` body: track number, timecode, flags and
/// the lace. Returns the number of frames it holds.
pub fn block(buf: &[u8]) -> Result<usize> {
    demux::parse_block(buf, 0..buf.len()).map(|head| head.frames.len())
}

/// `buf` as a `Tracks` body, with `scale_ns` as the file's `TimestampScale`. Returns
/// the number of streams that resolve.
pub fn tracks(buf: &[u8], scale_ns: u64) -> Result<usize> {
    let time_base = TimeBase::try_new(scale_ns as i64, 1_000_000_000)?;
    demux::parse_tracks(buf, scale_ns, time_base, Some(1_000)).map(|(streams, _)| streams.len())
}

/// `buf` as a `Cues` body in a segment that starts at `segment`. Returns the cue
/// points that resolve.
pub fn cues(buf: &[u8], segment: u64) -> usize {
    demux::parse_cues(buf, segment).len()
}

/// `buf` as a `SeekHead` body. Returns the entries that resolve.
pub fn seek_head(buf: &[u8]) -> usize {
    demux::seek_positions(buf).len()
}
