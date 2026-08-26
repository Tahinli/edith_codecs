//! WebP: the RIFF container, its lossy (`VP8 `) and lossless (`VP8L`) payloads,
//! and the separate alpha plane (`ALPH`) an extended file may carry.
//!
//! `decode` returns one still picture and refuses an animation by name: a still
//! decoder that quietly handed back the first frame would make "this file
//! plays" and "this file decoded" indistinguishable to its caller. The
//! sequence is `decode_frames`, which composites the `ANMF` frames onto the
//! canvas.

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

/// Whether the file is an animation rather than a still.
///
/// The question is answerable from the container alone -- the VP8X animation
/// flag, or an `ANIM`/`ANMF` chunk -- so it is answered even for a file whose
/// frames fail to decode.  Bytes that are not a readable WebP are not an
/// animation.
pub fn is_animated(data: &[u8]) -> bool {
    let Ok(chunks) = chunks(data) else {
        return false;
    };
    chunks.iter().any(|chunk| match &chunk.tag {
        b"VP8X" => chunk.data.first().is_some_and(|flags| flags & 0x02 != 0),
        b"ANIM" | b"ANMF" => true,
        _ => false,
    })
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
    decode_payload(tag, body, alpha, meta, limits)
}

/// Decode one image payload -- a `VP8 ` or `VP8L` body, with its optional
/// `ALPH` plane -- into pixels.
///
/// A still file and one frame of an animation differ only in where these
/// chunks were found, so both paths land here.
fn decode_payload(
    tag: &[u8; 4],
    body: &[u8],
    alpha: Option<&[u8]>,
    meta: Metadata,
    limits: Limits,
) -> Result<Image> {
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

/// Decode an animated WebP into composited canvas frames.
///
/// Every frame comes back as the whole canvas in RGBA, the way a player wants
/// it: the frame rectangles, the two blending methods and the dispose-to-
/// background rule are applied here rather than left to the caller.
pub fn decode_frames(data: &[u8], limits: Limits) -> Result<Vec<crate::AnimationFrame>> {
    let chunks = chunks(data)?;
    let (canvas_width, canvas_height) = {
        let info = info(data)?;
        (info.width, info.height)
    };
    limits.check(canvas_width, canvas_height)?;

    let mut frames: Vec<crate::AnimationFrame> = Vec::new();
    // The canvas starts fully transparent.  The `ANIM` background colour is
    // advice to a viewer about what to show behind the animation, not a colour
    // the frames are composited onto -- libwebp's own animation decoder
    // ignores it the same way.
    let mut canvas = vec![0u8; canvas_width as usize * canvas_height as usize * 4];
    let mut dispose: Option<(u32, u32, u32, u32)> = None;

    for chunk in &chunks {
        if &chunk.tag != b"ANMF" {
            continue;
        }
        let d = chunk.data;
        if d.len() < 16 {
            return Err(Error::corrupt("WebP: short ANMF header"));
        }
        let u24 = |at: usize| u32::from_le_bytes([d[at], d[at + 1], d[at + 2], 0]);
        let (x, y) = (u24(0) * 2, u24(3) * 2);
        let (width, height) = (u24(6) + 1, u24(9) + 1);
        let duration = u24(12);
        let blend_over = d[15] & 0x02 == 0;
        let dispose_to_background = d[15] & 0x01 != 0;
        if x + width > canvas_width || y + height > canvas_height {
            return Err(Error::corrupt(format!(
                "WebP: frame {width}x{height}+{x}+{y} runs off a {canvas_width}x{canvas_height} canvas"
            )));
        }

        // The previous frame's disposal happens before this one is drawn, so a
        // caller that keeps every frame still sees each one whole.
        if let Some((x, y, width, height)) = dispose.take() {
            for row in y..y + height {
                let at = (row as usize * canvas_width as usize + x as usize) * 4;
                canvas[at..at + width as usize * 4].fill(0);
            }
        }

        let mut alpha: Option<&[u8]> = None;
        let mut payload: Option<(&[u8; 4], &[u8])> = None;
        let mut at = 16usize;
        while at + 8 <= d.len() {
            let tag = [d[at], d[at + 1], d[at + 2], d[at + 3]];
            let size = u32::from_le_bytes([d[at + 4], d[at + 5], d[at + 6], d[at + 7]]) as usize;
            let body = d
                .get(at + 8..at + 8 + size)
                .ok_or_else(|| Error::corrupt("WebP: ANMF sub-chunk runs past the frame"))?;
            match &tag {
                b"ALPH" => alpha = Some(body),
                // Only the first image chunk of a frame is the frame; the tag
                // is kept as a static so the borrow of `d` does not outlive
                // this iteration.
                b"VP8 " if payload.is_none() => payload = Some((b"VP8 ", body)),
                b"VP8L" if payload.is_none() => payload = Some((b"VP8L", body)),
                _ => {}
            }
            at += 8 + size + (size & 1);
        }
        let (tag, body) =
            payload.ok_or_else(|| Error::corrupt("WebP: ANMF without an image chunk"))?;
        let sub = decode_payload(tag, body, alpha, Metadata::default(), limits)?;
        if sub.width != width || sub.height != height {
            return Err(Error::corrupt(format!(
                "WebP: ANMF says {width}x{height}, the payload decodes {}x{}",
                sub.width, sub.height
            )));
        }
        let pixels = sub.to_rgba8();

        for row in 0..height as usize {
            for col in 0..width as usize {
                let src = (row * width as usize + col) * 4;
                let dst = ((row + y as usize) * canvas_width as usize + col + x as usize) * 4;
                let src = &pixels[src..src + 4];
                if !blend_over || src[3] == 255 {
                    canvas[dst..dst + 4].copy_from_slice(src);
                    continue;
                }
                if src[3] == 0 {
                    continue;
                }
                // Source-over in eight-bit alpha, rounded rather than
                // truncated: a run of blended frames drifts visibly otherwise.
                let sa = u32::from(src[3]);
                let da = u32::from(canvas[dst + 3]);
                let out_a = sa + da * (255 - sa) / 255;
                for c in 0..3 {
                    let s = u32::from(src[c]) * sa;
                    let dcol = u32::from(canvas[dst + c]) * da * (255 - sa) / 255;
                    canvas[dst + c] = (s + dcol + out_a / 2)
                        .checked_div(out_a)
                        .unwrap_or(0)
                        .min(255) as u8;
                }
                canvas[dst + 3] = out_a as u8;
            }
        }

        if dispose_to_background {
            dispose = Some((x, y, width, height));
        }
        frames.push(crate::AnimationFrame {
            image: Image {
                width: canvas_width,
                height: canvas_height,
                pixels: Pixels::Rgba8(canvas.clone()),
                meta: Metadata::default(),
            },
            delay_num: duration,
            delay_den: 1000,
        });
    }

    if frames.is_empty() {
        return Err(Error::corrupt("WebP: an animation with no ANMF frame"));
    }
    Ok(frames)
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
