//! BMP: the Windows device-independent bitmap, as `image` and every screenshot
//! tool on Windows write it.
//!
//! One file header, one info header, an optional palette and then rows, and
//! almost all of the difficulty is in how many shapes those four things take:
//! headers of five different lengths, samples 1, 2, 4, 8, 16, 24 or 32 bits
//! wide, palettes with three-byte or four-byte entries, rows stored bottom-up
//! or top-down, run-length encoding for the 4- and 8-bit forms, and arbitrary
//! channel masks under `BI_BITFIELDS`.
//!
//! `BI_PNG` and `BI_JPEG` files are a BMP header wrapped around a whole PNG or
//! JPEG file, so they are handed to those decoders rather than refused.
//!
//! An OS/2 v2 header is read through its first 16 bytes, which is where it
//! stops agreeing with the Windows one; the fields past that describe halftoning
//! and a colour table format no writer emits. ICC profiles in a v5 header are
//! metadata this crate does not carry, so they are ignored rather than refused.

use crate::{Image, ImageFormat, Info, Limits, Metadata, Pixels};
use ec_core::{Error, Result};

/// Fields of the info header this decoder acts on.
struct Header {
    width: u32,
    height: u32,
    /// Rows are stored bottom-up unless the height was negative.
    top_down: bool,
    bits: u16,
    compression: u32,
    /// Palette entries actually present in the file.
    colours: usize,
    /// Bytes per palette entry: 4 in a Windows header, 3 in an OS/2 one.
    palette_stride: usize,
    /// Where the info header ends and the palette begins.
    palette_at: usize,
    /// Channel masks, present under `BI_BITFIELDS` and defaulted otherwise.
    masks: [u32; 4],
}

const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;
const BI_JPEG: u32 = 4;
const BI_PNG: u32 = 5;
const BI_ALPHABITFIELDS: u32 = 6;

fn u16_at(data: &[u8], at: usize) -> Result<u16> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| Error::corrupt("BMP: header runs past the file"))
}

fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| Error::corrupt("BMP: header runs past the file"))
}

/// Parse the file and info headers.
fn header(data: &[u8]) -> Result<Header> {
    if !data.starts_with(b"BM") {
        return Err(Error::corrupt("BMP: no BM signature"));
    }
    let info_size = u32_at(data, 14)? as usize;
    // 12 is the OS/2 v1 header (BITMAPCOREHEADER), 40 the Windows v3 one, and
    // 52/56/108/124 are v3 plus masks, v4 and v5. Everything past the first 40
    // bytes is colour-space description this crate does not carry.
    let (width, height, top_down, bits, compression, declared_colours) = if info_size == 12 {
        (
            u32::from(u16_at(data, 18)?),
            u32::from(u16_at(data, 20)?),
            false,
            u16_at(data, 24)?,
            BI_RGB,
            0usize,
        )
    } else if (16..40).contains(&info_size) {
        // An OS/2 v2 header, whose first 16 bytes are the Windows layout: the
        // dimensions are 32-bit and there is no compression field at all.
        let raw_height = u32_at(data, 22)? as i32;
        (
            u32_at(data, 18)?,
            raw_height.unsigned_abs(),
            raw_height < 0,
            u16_at(data, 28)?,
            BI_RGB,
            0usize,
        )
    } else if info_size >= 40 {
        let raw_height = u32_at(data, 22)? as i32;
        (
            u32_at(data, 18)?,
            raw_height.unsigned_abs(),
            raw_height < 0,
            u16_at(data, 28)?,
            u32_at(data, 30)?,
            u32_at(data, 46)? as usize,
        )
    } else {
        return Err(Error::corrupt(format!(
            "BMP: a {info_size}-byte info header, shorter than any the format defines"
        )));
    };

    if !matches!(bits, 1 | 2 | 4 | 8 | 16 | 24 | 32) {
        return Err(Error::corrupt(format!("BMP: {bits} bits per pixel")));
    }
    match compression {
        // Handled before the header is read; reaching here means the caller
        // asked for the fields of a file that has none of them.
        BI_JPEG | BI_PNG => {}
        BI_RGB | BI_RLE8 | BI_RLE4 | BI_BITFIELDS | BI_ALPHABITFIELDS => {}
        other => {
            return Err(Error::corrupt(format!("BMP: compression {other}")));
        }
    }
    if (compression == BI_RLE8 && bits != 8) || (compression == BI_RLE4 && bits != 4) {
        return Err(Error::corrupt(format!(
            "BMP: run-length encoding with {bits} bits per pixel"
        )));
    }

    // Under BI_BITFIELDS the masks follow the 40-byte header in a v3 file and
    // live inside a v4/v5 one; either way they start at offset 54.
    let bitfields = compression == BI_BITFIELDS || compression == BI_ALPHABITFIELDS;
    let mut masks = [0u32; 4];
    let mut palette_at = 14 + info_size;
    if bitfields && info_size == 40 {
        for (i, mask) in masks.iter_mut().enumerate().take(3) {
            *mask = u32_at(data, 54 + i * 4)?;
        }
        if compression == BI_ALPHABITFIELDS {
            masks[3] = u32_at(data, 66)?;
        }
        palette_at += 12
            + if compression == BI_ALPHABITFIELDS {
                4
            } else {
                0
            };
    } else if bitfields {
        for (i, mask) in masks.iter_mut().enumerate() {
            *mask = u32_at(data, 54 + i * 4)?;
        }
    } else if info_size >= 108 {
        // A v4 header carries masks whether or not BI_BITFIELDS is set; they
        // are only meaningful for the 16- and 32-bit forms.
        for (i, mask) in masks.iter_mut().enumerate() {
            *mask = u32_at(data, 54 + i * 4)?;
        }
    }
    if masks[..3].iter().all(|&m| m == 0) {
        // The defaults the format documents: 5-5-5 at 16 bits, 8-8-8 at 24 and
        // 32, with the top eight bits of a 32-bit sample unused.
        masks = match bits {
            16 => [0x7c00, 0x03e0, 0x001f, 0],
            _ => [0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0],
        };
    }

    let palette_stride = if info_size == 12 { 3 } else { 4 };
    let colours = if bits <= 8 {
        let max = 1usize << bits;
        // Zero means "as many as the depth allows"; a larger claim than the
        // depth allows is the file lying about itself.
        if declared_colours == 0 || declared_colours > max {
            max
        } else {
            declared_colours
        }
    } else {
        0
    };

    Ok(Header {
        width,
        height,
        top_down,
        bits,
        compression,
        colours,
        palette_stride,
        palette_at,
        masks,
    })
}

/// Dimensions from the headers alone.
pub fn info(data: &[u8]) -> Result<Info> {
    if let Some(embedded) = embedded(data)? {
        let mut info = match embedded.0 {
            ImageFormat::Png => crate::png::info(embedded.1)?,
            _ => crate::jpeg::info(embedded.1)?,
        };
        // The wrapper is what the caller opened, so it is what they are told.
        info.format = ImageFormat::Bmp;
        return Ok(info);
    }
    let header = header(data)?;
    Ok(Info {
        format: ImageFormat::Bmp,
        width: header.width,
        height: header.height,
    })
}

/// The payload of a `BI_PNG` or `BI_JPEG` file, which is a whole file of that
/// other format rather than any arrangement of BMP rows.
///
/// `biSizeImage` is the payload's length; a zero there means the writer left it
/// out, and the rest of the file is the payload.
fn embedded(data: &[u8]) -> Result<Option<(ImageFormat, &[u8])>> {
    if !data.starts_with(b"BM") || u32_at(data, 14)? < 40 {
        return Ok(None);
    }
    let format = match u32_at(data, 30)? {
        BI_PNG => ImageFormat::Png,
        BI_JPEG => ImageFormat::Jpeg,
        _ => return Ok(None),
    };
    let offset = u32_at(data, 10)? as usize;
    let payload = data
        .get(offset..)
        .ok_or_else(|| Error::corrupt("BMP: the payload offset is past the file"))?;
    let declared = u32_at(data, 34)? as usize;
    let payload = if declared == 0 || declared > payload.len() {
        payload
    } else {
        &payload[..declared]
    };
    Ok(Some((format, payload)))
}

/// A channel mask reduced to a shift and a scale.
///
/// Masks are arbitrary -- a 16-bit file may carry 5-6-5, 5-5-5 or anything
/// else -- so each channel is read as a field and stretched to eight bits so
/// that a full-scale field becomes 255 rather than 248.
struct Field {
    shift: u32,
    max: u32,
}

impl Field {
    fn new(mask: u32) -> Option<Field> {
        if mask == 0 {
            return None;
        }
        let shift = mask.trailing_zeros();
        Some(Field {
            shift,
            max: mask >> shift,
        })
    }

    fn sample(&self, raw: u32, mask: u32) -> u8 {
        let value = (raw & mask) >> self.shift;
        if self.max == 0 {
            return 0;
        }
        ((value * 255 + self.max / 2) / self.max) as u8
    }
}

/// Decode a BMP file.
pub fn decode(data: &[u8], limits: Limits) -> Result<Image> {
    if let Some((format, payload)) = embedded(data)? {
        return match format {
            ImageFormat::Png => crate::png::decode(payload, limits),
            _ => crate::jpeg::decode(payload, limits),
        };
    }
    let header = header(data)?;
    limits.check(header.width, header.height)?;
    let (width, height) = (header.width as usize, header.height as usize);
    if width == 0 || height == 0 {
        return Err(Error::corrupt("BMP: a zero-sized picture"));
    }

    let palette = palette(data, &header)?;
    let offset = u32_at(data, 10)? as usize;
    let body = data
        .get(offset..)
        .ok_or_else(|| Error::corrupt("BMP: the pixel offset is past the file"))?;

    // Indices first, then colour: every form below produces either palette
    // indices or direct samples, and the bottom-up flip is the same either way.
    let has_alpha = header.masks[3] != 0
        || (header.bits == 32 && header.compression == BI_RGB && palette.is_empty());
    let mut out = vec![0u8; width * height * if has_alpha { 4 } else { 3 }];
    let channels = if has_alpha { 4 } else { 3 };
    let row_of = |y: usize| if header.top_down { y } else { height - 1 - y };

    match header.compression {
        BI_RLE8 | BI_RLE4 => {
            let indices = rle(body, &header)?;
            for y in 0..height {
                for x in 0..width {
                    let index = indices[y * width + x] as usize;
                    let rgb = *palette
                        .get(index)
                        .ok_or_else(|| Error::corrupt("BMP: palette index past the table"))?;
                    let at = (row_of(y) * width + x) * channels;
                    out[at..at + 3].copy_from_slice(&rgb);
                    if has_alpha {
                        out[at + 3] = 255;
                    }
                }
            }
        }
        _ => {
            // Rows are padded to a four-byte boundary.
            let stride = (width * usize::from(header.bits))
                .div_ceil(8)
                .next_multiple_of(4);
            if body.len() < stride * height {
                return Err(Error::corrupt(format!(
                    "BMP: {} bytes of pixels, {} rows of {stride} needed",
                    body.len(),
                    height
                )));
            }
            let fields = [
                Field::new(header.masks[0]),
                Field::new(header.masks[1]),
                Field::new(header.masks[2]),
                Field::new(header.masks[3]),
            ];
            for y in 0..height {
                let row = &body[y * stride..y * stride + stride];
                for x in 0..width {
                    let at = (row_of(y) * width + x) * channels;
                    let (rgb, alpha) = match header.bits {
                        1 | 2 | 4 | 8 => {
                            let bits = usize::from(header.bits);
                            let bit = x * bits;
                            let byte = row[bit / 8];
                            let shift = 8 - bits - (bit % 8);
                            let index = usize::from((byte >> shift) & ((1 << bits) - 1) as u8);
                            let rgb = *palette.get(index).ok_or_else(|| {
                                Error::corrupt("BMP: palette index past the table")
                            })?;
                            (rgb, 255)
                        }
                        16 => {
                            let raw = u32::from(u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]));
                            sample(raw, &header.masks, &fields)
                        }
                        24 => ([row[x * 3 + 2], row[x * 3 + 1], row[x * 3]], 255),
                        _ => {
                            let raw = u32::from_le_bytes([
                                row[x * 4],
                                row[x * 4 + 1],
                                row[x * 4 + 2],
                                row[x * 4 + 3],
                            ]);
                            sample(raw, &header.masks, &fields)
                        }
                    };
                    out[at..at + 3].copy_from_slice(&rgb);
                    if has_alpha {
                        out[at + 3] = alpha;
                    }
                }
            }
        }
    }

    Ok(Image {
        width: header.width,
        height: header.height,
        pixels: if has_alpha {
            Pixels::Rgba8(out)
        } else {
            Pixels::Rgb8(out)
        },
        meta: Metadata::default(),
    })
}

/// One direct-colour pixel through the channel masks.
fn sample(raw: u32, masks: &[u32; 4], fields: &[Option<Field>; 4]) -> ([u8; 3], u8) {
    let mut rgb = [0u8; 3];
    for c in 0..3 {
        if let Some(field) = &fields[c] {
            rgb[c] = field.sample(raw, masks[c]);
        }
    }
    let alpha = match &fields[3] {
        Some(field) => field.sample(raw, masks[3]),
        None => 255,
    };
    (rgb, alpha)
}

/// The palette, as RGB triples in file order.
fn palette(data: &[u8], header: &Header) -> Result<Vec<[u8; 3]>> {
    if header.colours == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(header.colours);
    for i in 0..header.colours {
        let at = header.palette_at + i * header.palette_stride;
        let entry = data
            .get(at..at + 3)
            .ok_or_else(|| Error::corrupt("BMP: the palette runs past the file"))?;
        // Entries are stored blue first.
        out.push([entry[2], entry[1], entry[0]]);
    }
    Ok(out)
}

/// Expand `BI_RLE8` or `BI_RLE4` into one palette index per pixel.
///
/// The two forms share a structure: a non-zero first byte is a run, a zero
/// introduces an escape -- end of line, end of file, a delta, or a literal run
/// padded to an even number of bytes. Pixels the encoding never reaches keep
/// index zero, which is what the format means by a sparse image.
fn rle(body: &[u8], header: &Header) -> Result<Vec<u8>> {
    let (width, height) = (header.width as usize, header.height as usize);
    let mut out = vec![0u8; width * height];
    let (mut x, mut y) = (0usize, 0usize);
    let mut at = 0usize;
    let four_bit = header.compression == BI_RLE4;

    let put = |out: &mut Vec<u8>, x: &mut usize, y: usize, index: u8| {
        if *x < width && y < height {
            out[y * width + *x] = index;
        }
        *x += 1;
    };

    while at + 1 < body.len() {
        let (count, value) = (body[at], body[at + 1]);
        at += 2;
        if count > 0 {
            for i in 0..usize::from(count) {
                let index = if four_bit {
                    if i % 2 == 0 { value >> 4 } else { value & 0x0f }
                } else {
                    value
                };
                put(&mut out, &mut x, y, index);
            }
            continue;
        }
        match value {
            // End of line.
            0 => {
                x = 0;
                y += 1;
            }
            // End of the bitmap; anything after it is padding.
            1 => break,
            // A delta: move right and down by the next two bytes.
            2 => {
                let delta = body
                    .get(at..at + 2)
                    .ok_or_else(|| Error::corrupt("BMP: a truncated RLE delta"))?;
                x += usize::from(delta[0]);
                y += usize::from(delta[1]);
                at += 2;
            }
            // A literal run of `value` pixels, padded to an even byte count.
            literal => {
                let literal = usize::from(literal);
                let bytes = if four_bit {
                    literal.div_ceil(2)
                } else {
                    literal
                };
                let run = body
                    .get(at..at + bytes)
                    .ok_or_else(|| Error::corrupt("BMP: a truncated RLE literal"))?;
                for i in 0..literal {
                    let index = if four_bit {
                        if i % 2 == 0 {
                            run[i / 2] >> 4
                        } else {
                            run[i / 2] & 0x0f
                        }
                    } else {
                        run[i]
                    };
                    put(&mut out, &mut x, y, index);
                }
                at += bytes + (bytes & 1);
            }
        }
        if y > height {
            return Err(Error::corrupt("BMP: RLE runs past the last row"));
        }
    }
    Ok(out)
}
