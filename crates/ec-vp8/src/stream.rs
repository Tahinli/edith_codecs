//! Whole-stream decoding: an IVF file in, decoded pictures out.
//!
//! VP8 frames are not self-delimiting — a bare frame is a 3-byte
//! uncompressed tag followed by partitions whose total size only the
//! container knows — so the whole-stream API takes the stream in the
//! container VP8 actually ships in: IVF (the RFC's companion container,
//! `DKIF`/`VP80`; also what vpxenc, ffmpeg and the WebM toolchain emit
//! for raw VP8 elementaries). Single frames from any other demuxer feed
//! [`crate::decode::Decoder::decode`] directly.
//!
//! The shape mirrors `ec_av1::stream`: [`decode_stream`] collects every
//! shown picture into a `Vec`, and [`decode_stream_with`] hands each
//! picture to a sink the moment it exists.

use crate::decode::{Decoder, Picture};
use crate::{Error, Result};

/// The IVF file magic, `DKIF`.
const DKIF: &[u8; 4] = b"DKIF";
/// The fourcc for VP8 video (`VP80`; `VP8L`/`VP8x` are WebP-lossless/alpha).
const VP80: &[u8; 4] = b"VP80";

/// One demuxed IVF frame: presentation timestamp plus the compressed
/// VP8 payload.
struct IvfFrame<'a> {
    pts: u64,
    data: &'a [u8],
}

/// Split an IVF byte stream into its frames, validating the container
/// envelope (magic, fourcc, framing arithmetic) as [`Error`]s rather
/// than panics.
fn demux(data: &[u8]) -> Result<Vec<IvfFrame<'_>>> {
    if data.len() < 32 || &data[0..4] != DKIF {
        return Err(Error::corrupt("not an IVF stream (missing DKIF magic)"));
    }
    // Bytes 4..6 version (informational), 6..8 header length, 8..12 fourcc.
    let hdr_len = u16::from_le_bytes([data[6], data[7]]) as usize;
    if data.len() < hdr_len || hdr_len < 12 {
        return Err(Error::corrupt("IVF header length out of range"));
    }
    if &data[8..12] != VP80 {
        return Err(Error::unsupported(
            "IVF fourcc",
            "the stream is not VP80 (VP8 video)",
        ));
    }
    // Bytes 12..16 carry the nominal width/height; VP8 streams set them
    // to 0 and take the real dimensions from the key frame, so the
    // decoder does not consult them.
    let _ = u16::from_le_bytes([data[12], data[13]]);
    let _ = u16::from_le_bytes([data[14], data[15]]);
    let mut frames = Vec::new();
    let mut pos = hdr_len;
    while pos + 12 <= data.len() {
        let sz =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                as usize;
        let pts = u64::from_le_bytes([
            data[pos + 4],
            data[pos + 5],
            data[pos + 6],
            data[pos + 7],
            data[pos + 8],
            data[pos + 9],
            data[pos + 10],
            data[pos + 11],
        ]);
        pos += 12;
        if pos + sz > data.len() {
            return Err(Error::corrupt(format!(
                "IVF frame {} extends past end of stream (need {} bytes \
                 at offset {})",
                frames.len(),
                sz,
                pos
            )));
        }
        frames.push(IvfFrame {
            pts,
            data: &data[pos..pos + sz],
        });
        pos += sz;
    }
    Ok(frames)
}

/// Decode every frame in an IVF VP8 stream, in presentation order.
///
/// The stream must start with a key frame; hidden frames (alternate
/// references, `show_frame == 0`) update the reference buffers but are
/// not part of the output, exactly as a player would not display them.
///
/// # Errors
/// Returns an error when the container is truncated or not VP80 (as
/// [`demux`] reports it), when a frame is malformed (as
/// [`crate::decode::Decoder::decode`] reports it), or when an inter
/// frame appears before any key frame has supplied references.
pub fn decode_stream(data: &[u8]) -> Result<Vec<Picture>> {
    let mut pictures: Vec<Picture> = Vec::new();
    decode_stream_with(data, |picture, _decode_idx, _pts| {
        pictures.push(picture.clone());
        Ok(())
    })?;
    Ok(pictures)
}

/// Streaming form of [`decode_stream`]: hand every shown picture to
/// `sink` the moment it exists and drop it right after, so peak memory
/// is one output picture plus the reference frames instead of the whole
/// decoded stream. `decode_stream` itself is this function collecting
/// into a `Vec`.
///
/// `sink` is called with the picture, the decode-order index of the
/// coded frame it came from, and the IVF presentation timestamp. A
/// hidden frame (`show_frame == 0`) updates the decoder state but
/// produces no picture, so the sink is not called for it. An `Err` the
/// sink returns aborts the decode and is returned unchanged.
///
/// # Errors
/// As [`decode_stream`], plus whatever `sink` returns.
pub fn decode_stream_with(
    data: &[u8],
    mut sink: impl FnMut(&Picture, usize, u64) -> Result<()>,
) -> Result<()> {
    let frames = demux(data)?;
    let mut dec = Decoder::new();
    for (decode_idx, frame) in frames.iter().enumerate() {
        if let Some(picture) = dec.decode(frame.data)? {
            sink(&picture, decode_idx, frame.pts)?;
        }
    }
    Ok(())
}
