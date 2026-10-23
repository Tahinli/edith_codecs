//! PNG encoding for 8-bit RGB and RGBA: one `IDAT`, adaptive per-row filters.
//!
//! Each scanline is filtered with whichever of the five PNG filters has the
//! smallest sum of absolute residuals (the standard heuristic). The filtered
//! rows are deflated with [`ec_inflate::deflate_zlib`], so the output is
//! smaller than stored blocks but larger than zlib's dynamic-Huffman output.
//! Every chunk gets its CRC, and the output decodes through [`crate::png::decode`].

use crate::png::crc32;
use ec_core::{Error, Result};

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
/// PNG's chunk length field is a 31-bit unsigned integer.
const MAX_CHUNK: usize = 0x7fff_ffff;

/// Encode packed 8-bit R,G,B (`width * height * 3` bytes) as a PNG file.
pub fn encode_rgb8(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    encode(width, height, pixels, 3, 2)
}

/// Encode packed 8-bit R,G,B,A (`width * height * 4` bytes) as a PNG file.
pub fn encode_rgba8(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    encode(width, height, pixels, 4, 6)
}

fn encode(
    width: u32,
    height: u32,
    pixels: &[u8],
    channels: usize,
    color_type: u8,
) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width > MAX_CHUNK as u32 || height > MAX_CHUNK as u32 {
        return Err(Error::unsupported(
            "PNG encode",
            format!("{width}x{height} is not a PNG-sized image"),
        ));
    }
    let row = (width as usize) * channels;
    let expected = row
        .checked_mul(height as usize)
        .ok_or_else(|| Error::unsupported("PNG encode", "image size overflows"))?;
    if pixels.len() != expected {
        return Err(Error::unsupported(
            "PNG encode",
            format!(
                "{} bytes of pixels for {width}x{height} (need {expected})",
                pixels.len()
            ),
        ));
    }

    let zero = vec![0u8; row];
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    let mut best = Vec::with_capacity(row);
    let mut trial = Vec::with_capacity(row);
    for y in 0..height as usize {
        let cur = &pixels[y * row..(y + 1) * row];
        let prev = if y == 0 {
            &zero[..]
        } else {
            &pixels[(y - 1) * row..y * row]
        };
        let mut best_kind = 0u8;
        let mut best_score = u64::MAX;
        for kind in 0..5u8 {
            trial.clear();
            filter_row(kind, cur, prev, channels, &mut trial);
            let score: u64 = trial
                .iter()
                .map(|&b| u64::from((b as i8).unsigned_abs()))
                .sum();
            if score < best_score {
                best_score = score;
                best_kind = kind;
                std::mem::swap(&mut best, &mut trial);
            }
        }
        raw.push(best_kind);
        raw.extend_from_slice(&best);
    }

    let idat = ec_inflate::deflate_zlib(&raw);
    if idat.len() > MAX_CHUNK {
        return Err(Error::unsupported(
            "PNG encode",
            "compressed data exceeds one IDAT chunk",
        ));
    }
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, color_type, 0, 0, 0]);

    let mut out = Vec::with_capacity(idat.len() + 64);
    out.extend_from_slice(&SIGNATURE);
    put_chunk(&mut out, b"IHDR", &ihdr);
    put_chunk(&mut out, b"IDAT", &idat);
    put_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn put_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Apply PNG filter `kind` (0..=4, RFC 2083 section 6) to one scanline.
fn filter_row(kind: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut Vec<u8>) {
    for i in 0..cur.len() {
        let a = if i >= bpp { cur[i - bpp] } else { 0 };
        let b = prev[i];
        let c = if i >= bpp { prev[i - bpp] } else { 0 };
        let predictor = match kind {
            0 => 0,
            1 => a,
            2 => b,
            3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
            _ => paeth(a, b, c),
        };
        out.push(cur[i].wrapping_sub(predictor));
    }
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i16::from(a), i16::from(b), i16::from(c));
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paeth_picks_the_nearest_neighbour_with_ties_to_a_then_b() {
        assert_eq!(paeth(10, 20, 10), 20);
        assert_eq!(paeth(10, 10, 10), 10);
        assert_eq!(paeth(0, 0, 255), 0);
    }

    #[test]
    fn a_flat_image_round_trips() {
        let px = vec![7u8; 5 * 3 * 3];
        let png = encode_rgb8(5, 3, &px).unwrap();
        let img = crate::decode(&png).unwrap();
        assert_eq!(img.to_rgb8(), px);
    }

    #[test]
    fn a_size_mismatch_is_refused() {
        assert!(encode_rgb8(2, 2, &[0; 11]).is_err());
        assert!(encode_rgba8(0, 2, &[]).is_err());
    }
}
