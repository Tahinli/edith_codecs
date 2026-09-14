//! Whole-stream decoding: an IVF file in, decoded pictures out.
//!
//! VP9 frames are not self-delimiting — a frame's total size only the
//! container knows (and one IVF chunk may hold a superframe with hidden
//! ALTREF frames) — so the whole-stream API takes the stream in the
//! container VP9 actually ships in: IVF (`DKIF`/`VP90`). Single frames
//! or chunks from any other demuxer feed
//! [`crate::decode::Decoder::decode`] directly, which splits superframes
//! itself.
//!
//! The shape mirrors `ec-vp8`'s: [`decode_stream`] collects every shown
//! picture into a `Vec`, and [`decode_stream_with`] hands each picture
//! to a sink the moment it exists.

use crate::decode::{Decoder, Picture};
use crate::tables::ensure;
use crate::{Error, Result};

/// The IVF file magic, `DKIF`.
const DKIF: &[u8; 4] = b"DKIF";
/// The fourcc for VP9 video (`VP90`).
const VP90: &[u8; 4] = b"VP90";

/// One demuxed IVF frame: presentation timestamp plus the compressed
/// VP9 payload (possibly a superframe chunk).
struct IvfFrame<'a> {
    pts: u64,
    data: &'a [u8],
}

/// Split an IVF byte stream into its frames, validating the container
/// envelope as [`Error`]s rather than panics.
fn demux(data: &[u8]) -> Result<Vec<IvfFrame<'_>>> {
    let mut frames = Vec::new();
    let err = |what: &str| Error::corrupt(format!("ivf: {what}"));
    ensure(data.len() >= 32, "truncated IVF header")?;
    if &data[0..4] != DKIF {
        return Err(err("not DKIF"));
    }
    if &data[8..12] != VP90 {
        return Err(err("fourcc is not VP90"));
    }
    let hdr_len = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut pos = hdr_len.max(32);
    while pos + 12 <= data.len() {
        let sz = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let pts = u64::from_le_bytes(data[pos + 4..pos + 12].try_into().unwrap());
        pos += 12;
        if sz == 0 {
            return Err(err("zero-length frame"));
        }
        if pos + sz > data.len() {
            return Err(err("truncated frame payload"));
        }
        frames.push(IvfFrame {
            pts,
            data: &data[pos..pos + sz],
        });
        pos += sz;
    }
    Ok(frames)
}

/// Decode every shown frame in an IVF VP9 stream, in presentation order.
///
/// The stream must start with a key frame; hidden frames
/// (`show_frame == 0`) update the reference buffers but are not part of
/// the output.
///
/// # Errors
/// As [`demux`] reports for the container, [`Decoder::decode`] for the
/// frames, plus `vp9 inter` refusals once a lane-ineligible frame shows
/// up.
pub fn decode_stream(data: &[u8]) -> Result<Vec<Picture>> {
    let mut out = Vec::new();
    decode_stream_with(data, |pic, _, _| {
        out.push(pic.clone());
        Ok(())
    })?;
    Ok(out)
}

/// Streaming form of [`decode_stream`]: hand every shown picture to
/// `sink` the moment it exists. `sink` receives the picture, the
/// decode-order index of the IVF chunk it came from, and the IVF
/// presentation timestamp. An `Err` from `sink` aborts the decode.
pub fn decode_stream_with(
    data: &[u8],
    mut sink: impl FnMut(&Picture, usize, u64) -> Result<()>,
) -> Result<()> {
    let frames = demux(data)?;
    let mut decoder = Decoder::new();
    for (i, frame) in frames.iter().enumerate() {
        if let Some(pic) = decoder.decode(frame.data)? {
            sink(&pic, i, frame.pts)?;
        }
    }
    Ok(())
}
