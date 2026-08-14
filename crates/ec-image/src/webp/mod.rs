//! WebP: the RIFF container, its lossy (`VP8 `) and lossless (`VP8L`) payloads,
//! and the separate alpha plane (`ALPH`) an extended file may carry.
//!
//! Animation is refused by name. A still decoder that quietly handed back the
//! first frame of an animation would make "this file plays" and "this file
//! decoded" indistinguishable to its caller.

pub mod vp8;
mod vp8_tables;
pub mod vp8l;

use crate::{Image, ImageFormat, Info, Limits, Metadata, Pixels};
use ec_core::{Error, Result};

/// One RIFF chunk.
struct Chunk<'a> {
    tag: [u8; 4],
    data: &'a [u8],
}

/// Walk the chunks inside a `WEBP` RIFF file.
fn chunks(data: &[u8]) -> Result<Vec<Chunk<'_>>> {
    let riff_size = data
        .get(4..8)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        .ok_or_else(|| Error::corrupt("WebP: truncated RIFF header"))?;
    // The declared size may exceed what is present in a truncated file; take
    // whichever ends sooner rather than trusting the header.
    let end = (riff_size + 8).min(data.len());
    let mut out = Vec::new();
    let mut at = 12usize;
    while at + 8 <= end {
        let tag = [data[at], data[at + 1], data[at + 2], data[at + 3]];
        let size =
            u32::from_le_bytes([data[at + 4], data[at + 5], data[at + 6], data[at + 7]]) as usize;
        let body = data
            .get(at + 8..at + 8 + size)
            .ok_or_else(|| Error::corrupt("WebP: chunk runs past the file"))?;
        out.push(Chunk { tag, data: body });
        // Chunks are padded to an even length.
        at += 8 + size + (size & 1);
    }
    if out.is_empty() {
        return Err(Error::corrupt("WebP: no chunks"));
    }
    Ok(out)
}

/// Dimensions from the container's headers alone.
pub fn info(data: &[u8]) -> Result<Info> {
    let chunks = chunks(data)?;
    for chunk in &chunks {
        match &chunk.tag {
            b"VP8X" if chunk.data.len() >= 10 => {
                let d = chunk.data;
                let width = 1 + (u32::from(d[4]) | u32::from(d[5]) << 8 | u32::from(d[6]) << 16);
                let height = 1 + (u32::from(d[7]) | u32::from(d[8]) << 8 | u32::from(d[9]) << 16);
                return Ok(Info {
                    format: ImageFormat::WebP,
                    width,
                    height,
                });
            }
            b"VP8 " => {
                let (width, height) = vp8::dimensions(chunk.data)?;
                return Ok(Info {
                    format: ImageFormat::WebP,
                    width,
                    height,
                });
            }
            b"VP8L" => {
                let d = chunk.data;
                if d.len() < 5 || d[0] != 0x2f {
                    return Err(Error::corrupt("WebP: short VP8L header"));
                }
                let packed = u32::from_le_bytes([d[1], d[2], d[3], d[4]]);
                return Ok(Info {
                    format: ImageFormat::WebP,
                    width: (packed & 0x3fff) + 1,
                    height: ((packed >> 14) & 0x3fff) + 1,
                });
            }
            _ => {}
        }
    }
    Err(Error::corrupt("WebP: no image chunk"))
}

/// Decode a WebP file.
pub fn decode(data: &[u8], limits: Limits) -> Result<Image> {
    let chunks = chunks(data)?;
    let mut meta = Metadata::default();
    let mut alpha: Option<&[u8]> = None;
    let mut image: Option<(&[u8; 4], &[u8])> = None;

    for chunk in &chunks {
        match &chunk.tag {
            b"VP8X" => {
                let flags = chunk
                    .data
                    .first()
                    .copied()
                    .ok_or_else(|| Error::corrupt("WebP: empty VP8X"))?;
                if flags & 0x02 != 0 {
                    return Err(Error::unsupported(
                        "animated WebP",
                        "this decoder returns one still picture, not a sequence of frames",
                    ));
                }
            }
            b"ANIM" | b"ANMF" => {
                return Err(Error::unsupported(
                    "animated WebP",
                    "this decoder returns one still picture, not a sequence of frames",
                ));
            }
            b"ALPH" => alpha = Some(chunk.data),
            b"VP8 " | b"VP8L" => {
                if image.is_none() {
                    image = Some((&chunk.tag, chunk.data));
                }
            }
            b"EXIF" => {
                if let Some(o) = exif_orientation(chunk.data) {
                    meta.orientation = Some(o);
                }
            }
            _ => {}
        }
    }

    let (tag, body) = image.ok_or_else(|| Error::corrupt("WebP: no VP8 or VP8L chunk"))?;
    if tag == b"VP8L" {
        let argb = vp8l::decode(body, limits)?;
        let mut rgba = Vec::with_capacity(argb.pixels.len() * 4);
        for p in &argb.pixels {
            rgba.extend_from_slice(&[
                ((p >> 16) & 0xff) as u8,
                ((p >> 8) & 0xff) as u8,
                (p & 0xff) as u8,
                ((p >> 24) & 0xff) as u8,
            ]);
        }
        return Ok(Image {
            width: argb.width,
            height: argb.height,
            pixels: Pixels::Rgba8(rgba),
            meta,
        });
    }

    let frame = vp8::decode(body, limits)?;
    let (w, h) = (frame.width as usize, frame.height as usize);
    // Chroma is stored at half resolution and upsampled with the same triangle
    // filter libwebp uses; nearest-neighbour here costs about a decibel.
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let crop = |plane: &[u8]| -> Vec<u8> {
        (0..ch)
            .flat_map(|y| plane[y * frame.stride_uv..y * frame.stride_uv + cw].to_vec())
            .collect()
    };
    let u_plane = crate::upsample::upsample(&crop(&frame.u), cw, ch, w, h);
    let v_plane = crate::upsample::upsample(&crop(&frame.v), cw, ch, w, h);
    let alpha_plane = match alpha {
        Some(chunk) => Some(decode_alpha(chunk, frame.width, frame.height)?),
        None => None,
    };
    let channels = if alpha_plane.is_some() { 4 } else { 3 };
    let mut out = vec![0u8; w * h * channels];
    for y in 0..h {
        for x in 0..w {
            let luma = frame.y[y * frame.stride_y + x];
            let cb = u_plane[y * w + x];
            let cr = v_plane[y * w + x];
            let rgb = yuv_to_rgb(luma, cb, cr);
            let at = (y * w + x) * channels;
            out[at..at + 3].copy_from_slice(&rgb);
            if let Some(plane) = &alpha_plane {
                out[at + 3] = plane[y * w + x];
            }
        }
    }
    Ok(Image {
        width: frame.width,
        height: frame.height,
        pixels: if channels == 4 {
            Pixels::Rgba8(out)
        } else {
            Pixels::Rgb8(out)
        },
        meta,
    })
}

/// BT.601 studio-swing YCbCr to RGB, in 16-bit fixed point.
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let y = (i32::from(y) - 16) * 76284;
    let u = i32::from(u) - 128;
    let v = i32::from(v) - 128;
    let r = y + 104595 * v;
    let g = y - 25624 * u - 53281 * v;
    let b = y + 132251 * u;
    [
        ((r + 32768) >> 16).clamp(0, 255) as u8,
        ((g + 32768) >> 16).clamp(0, 255) as u8,
        ((b + 32768) >> 16).clamp(0, 255) as u8,
    ]
}

/// Decode an `ALPH` chunk into one byte of alpha per pixel.
fn decode_alpha(chunk: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let head = *chunk
        .first()
        .ok_or_else(|| Error::corrupt("WebP: empty ALPH chunk"))?;
    let method = head & 0x03;
    let filter = (head >> 2) & 0x03;
    let body = &chunk[1..];
    let count = (width as usize) * (height as usize);
    let mut plane = match method {
        0 => {
            if body.len() < count {
                return Err(Error::corrupt("WebP: ALPH shorter than the picture"));
            }
            body[..count].to_vec()
        }
        1 => {
            // Lossless-coded, dimensions implied; the alpha lives in green.
            let argb = vp8l::decode_implicit(body, width, height)?;
            argb.iter().map(|p| ((p >> 8) & 0xff) as u8).collect()
        }
        other => {
            return Err(Error::corrupt(format!(
                "WebP: ALPH compression method {other}"
            )));
        }
    };
    unfilter_alpha(&mut plane, width as usize, height as usize, filter);
    Ok(plane)
}

/// Undo the alpha plane's spatial filter (none / horizontal / vertical /
/// gradient), in place.
fn unfilter_alpha(plane: &mut [u8], width: usize, height: usize, filter: u8) {
    if filter == 0 {
        return;
    }
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let left = if x > 0 { plane[index - 1] } else { 0 };
            let above = if y > 0 { plane[index - width] } else { 0 };
            let above_left = if x > 0 && y > 0 {
                plane[index - width - 1]
            } else {
                0
            };
            let predictor = if x == 0 && y == 0 {
                0
            } else if y == 0 {
                // The top row has no pixel above, so it predicts from the left
                // whatever the filter says, and the left column from above.
                left
            } else if x == 0 {
                above
            } else {
                match filter {
                    1 => left,
                    2 => above,
                    _ => (i32::from(left) + i32::from(above) - i32::from(above_left)).clamp(0, 255)
                        as u8,
                }
            };
            plane[index] = predictor.wrapping_add(plane[index]);
        }
    }
}

/// EXIF orientation out of a bare TIFF block (no `Exif\0\0` prefix here).
fn exif_orientation(data: &[u8]) -> Option<u8> {
    let little = match data.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b = data.get(at..at + 2)?;
        Some(if little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = data.get(at..at + 4)?;
        Some(if little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let count = usize::from(u16_at(ifd)?);
    for i in 0..count {
        let entry = ifd + 2 + i * 12;
        if u16_at(entry)? == 0x0112 {
            let value = u16_at(entry + 8)?;
            return (1..=8).contains(&value).then_some(value as u8);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (tag, data) in chunks {
            body.extend_from_slice(*tag);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn odd_sized_chunks_are_padded_and_still_walked() {
        let file = riff(&[(b"XXXX", vec![1, 2, 3]), (b"VP8L", vec![0x2f, 0, 0, 0, 0])]);
        let found = chunks(&file).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(&found[1].tag, b"VP8L");
        assert_eq!(found[1].data.len(), 5);
    }

    #[test]
    fn animation_is_refused_by_name() {
        let mut vp8x = vec![0x02, 0, 0, 0];
        vp8x.extend_from_slice(&[15, 0, 0, 15, 0, 0]);
        let file = riff(&[(b"VP8X", vp8x), (b"ANMF", vec![0; 16])]);
        let err = decode(&file, Limits::default()).unwrap_err();
        assert!(format!("{err}").contains("animated"), "{err}");
    }

    #[test]
    fn yuv_conversion_puts_studio_black_and_white_at_the_ends() {
        assert_eq!(yuv_to_rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(yuv_to_rgb(235, 128, 128), [255, 255, 255]);
        let grey = yuv_to_rgb(126, 128, 128);
        assert!(grey[0] == grey[1] && grey[1] == grey[2], "{grey:?}");
    }

    #[test]
    fn the_alpha_gradient_filter_inverts_its_prediction() {
        // Horizontal filter: each stored byte is a difference from the left.
        let mut plane = vec![10u8, 5, 250, 1];
        unfilter_alpha(&mut plane, 4, 1, 1);
        assert_eq!(plane, vec![10, 15, 9, 10]);
    }
}
