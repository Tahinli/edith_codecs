//! PNG decoding (W3C PNG / RFC 2083) over [`ec_inflate`].
//!
//! Every critical chunk is honoured: `IHDR`, `PLTE`, `tRNS`, all five filter
//! types, all five colour types at every legal bit depth (1/2/4/8/16) and both
//! interlace methods. `gAMA` and `sRGB` are parsed into [`Metadata`]; other
//! ancillary chunks are skipped by length, which is what "safe to ignore"
//! means in a format whose chunk list keeps growing.
//!
//! Every chunk's CRC is verified. A PNG that fails its own checksum is
//! [`Error::Corrupt`], because the alternative — decoding it anyway — turns a
//! detectable bit flip into a picture nobody can tell is wrong.

use crate::{Image, ImageFormat, Info, Limits, Metadata, Pixels};
use ec_core::{Error, Result};

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Adam7: `(x_start, y_start, x_step, y_step)` per pass.
const ADAM7: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// IHDR, the only chunk that must come first.
#[derive(Debug, Clone, Copy)]
struct Header {
    width: u32,
    height: u32,
    depth: u8,
    color_type: u8,
    interlaced: bool,
}

impl Header {
    /// Samples per pixel for the colour type (palette indices count as one).
    fn channels(&self) -> usize {
        match self.color_type {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4,
        }
    }

    /// Bytes in one unfiltered scanline of `width` pixels.
    fn row_bytes(&self, width: u32) -> usize {
        let bits = (width as usize) * self.channels() * (self.depth as usize);
        bits.div_ceil(8)
    }

    /// Filter unit: bytes per pixel, rounded up, at least one.
    fn filter_step(&self) -> usize {
        ((self.channels() * self.depth as usize) / 8).max(1)
    }
}

/// CRC-32 (IEEE) as PNG specifies it, table built at compile time.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c = CRC_TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
    }
    c ^ 0xffff_ffff
}

fn be32(data: &[u8]) -> u32 {
    u32::from_be_bytes([data[0], data[1], data[2], data[3]])
}

/// One chunk: type, payload, and the offset the next chunk starts at.
struct Chunk<'a> {
    kind: [u8; 4],
    data: &'a [u8],
    next: usize,
}

/// Read the chunk at `at`, verifying its CRC.
fn chunk(data: &[u8], at: usize) -> Result<Chunk<'_>> {
    let head = data
        .get(at..at + 8)
        .ok_or_else(|| Error::corrupt("PNG: truncated chunk header"))?;
    let len = be32(head) as usize;
    let kind = [head[4], head[5], head[6], head[7]];
    // The length field is 31-bit by spec; a bigger one is a corrupt file, not
    // a reason to try to address past the buffer.
    if len > 0x7fff_ffff {
        return Err(Error::corrupt(format!("PNG: chunk length {len}")));
    }
    let body = data
        .get(at + 8..at + 8 + len)
        .ok_or_else(|| Error::corrupt("PNG: truncated chunk data"))?;
    let crc = data
        .get(at + 8 + len..at + 12 + len)
        .ok_or_else(|| Error::corrupt("PNG: truncated chunk CRC"))?;
    let want = be32(crc);
    let got = crc32(&data[at + 4..at + 8 + len]);
    if want != got {
        return Err(Error::corrupt(format!(
            "PNG: {} chunk CRC {got:#010x}, file says {want:#010x}",
            String::from_utf8_lossy(&kind)
        )));
    }
    Ok(Chunk {
        kind,
        data: body,
        next: at + 12 + len,
    })
}

fn header(data: &[u8]) -> Result<Header> {
    if !data.starts_with(&SIGNATURE) {
        return Err(Error::corrupt("PNG: bad signature"));
    }
    let ihdr = chunk(data, 8)?;
    if &ihdr.kind != b"IHDR" || ihdr.data.len() != 13 {
        return Err(Error::corrupt("PNG: first chunk is not a 13-byte IHDR"));
    }
    let d = ihdr.data;
    let header = Header {
        width: be32(d),
        height: be32(&d[4..]),
        depth: d[8],
        color_type: d[9],
        interlaced: d[12] == 1,
    };
    let depths: &[u8] = match header.color_type {
        0 => &[1, 2, 4, 8, 16],
        3 => &[1, 2, 4, 8],
        2 | 4 | 6 => &[8, 16],
        other => {
            return Err(Error::corrupt(format!("PNG: colour type {other}")));
        }
    };
    if !depths.contains(&header.depth) {
        return Err(Error::corrupt(format!(
            "PNG: bit depth {} with colour type {}",
            header.depth, header.color_type
        )));
    }
    if d[10] != 0 {
        return Err(Error::corrupt(format!("PNG: compression method {}", d[10])));
    }
    if d[11] != 0 {
        return Err(Error::corrupt(format!("PNG: filter method {}", d[11])));
    }
    if d[12] > 1 {
        return Err(Error::corrupt(format!("PNG: interlace method {}", d[12])));
    }
    Ok(header)
}

/// Dimensions from IHDR alone.
pub fn info(data: &[u8]) -> Result<Info> {
    let h = header(data)?;
    Ok(Info {
        format: ImageFormat::Png,
        width: h.width,
        height: h.height,
    })
}

/// Decode a PNG.
pub fn decode(data: &[u8], limits: Limits) -> Result<Image> {
    let hdr = header(data)?;
    limits.check(hdr.width, hdr.height)?;

    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Option<Vec<u8>> = None;
    let mut meta = Metadata::default();
    let mut idat: Vec<u8> = Vec::new();
    let mut at = chunk(data, 8)?.next;
    let mut saw_end = false;
    while !saw_end {
        let c = chunk(data, at)?;
        at = c.next;
        match &c.kind {
            b"PLTE" => {
                if c.data.len() % 3 != 0 || c.data.len() > 256 * 3 {
                    return Err(Error::corrupt(format!(
                        "PNG: PLTE of {} bytes",
                        c.data.len()
                    )));
                }
                palette = c.data.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
            }
            b"tRNS" => trns = Some(c.data.to_vec()),
            b"gAMA" if c.data.len() == 4 => {
                meta.gamma = Some(f64::from(be32(c.data)) / 100_000.0);
            }
            b"sRGB" if c.data.len() == 1 => meta.srgb_intent = Some(c.data[0]),
            b"IDAT" => idat.extend_from_slice(c.data),
            b"IEND" => saw_end = true,
            // Anything else is skipped by its own length: unknown ancillary
            // chunks are exactly what the chunk layout exists to survive.
            _ => {}
        }
    }
    if idat.is_empty() {
        return Err(Error::corrupt("PNG: no IDAT data"));
    }
    if hdr.color_type == 3 && palette.is_empty() {
        return Err(Error::corrupt("PNG: palette image without a PLTE chunk"));
    }

    // Exactly how many bytes the passes need; a stream claiming more is
    // truncated by the limit rather than believed.
    let expected: usize = passes(&hdr)
        .iter()
        .map(|&(w, h)| {
            if w == 0 || h == 0 {
                0
            } else {
                (h as usize) * (hdr.row_bytes(w) + 1)
            }
        })
        .sum();
    if expected > limits.max_alloc {
        return Err(Error::unsupported(
            "PNG",
            format!("{expected} bytes of pixel data is past the allocation limit"),
        ));
    }
    let raw = ec_inflate::inflate_zlib(&idat, expected).map_err(Error::from)?;
    if raw.len() < expected {
        return Err(Error::corrupt(format!(
            "PNG: {} bytes of scanlines, {expected} needed",
            raw.len()
        )));
    }

    // Samples are carried at 16 bits internally whatever the file's depth, so
    // one expansion path serves every colour type; the narrowing back to 8
    // happens once, at the end.
    let channels_out = out_channels(&hdr, &trns);
    let mut out = vec![0u16; (hdr.width as usize) * (hdr.height as usize) * channels_out];
    let mut offset = 0;
    for (pass, &(pw, ph)) in passes(&hdr).iter().enumerate() {
        if pw == 0 || ph == 0 {
            continue;
        }
        let row_bytes = hdr.row_bytes(pw);
        let step = hdr.filter_step();
        let mut previous = vec![0u8; row_bytes];
        let mut current = vec![0u8; row_bytes];
        let (x0, y0, dx, dy) = if hdr.interlaced {
            ADAM7[pass]
        } else {
            (0, 0, 1, 1)
        };
        for row in 0..ph {
            let start = offset + (row as usize) * (row_bytes + 1);
            let filter = raw[start];
            current.copy_from_slice(&raw[start + 1..start + 1 + row_bytes]);
            unfilter(filter, step, &previous, &mut current)?;
            let y = y0 + row * dy;
            expand_row(
                &current,
                pw,
                &hdr,
                &palette,
                trns.as_deref(),
                channels_out,
                |px, sample| {
                    let x = x0 + px * dx;
                    let base = ((y as usize) * (hdr.width as usize) + x as usize) * channels_out;
                    out[base..base + channels_out].copy_from_slice(sample);
                },
            )?;
            std::mem::swap(&mut previous, &mut current);
        }
        offset += (ph as usize) * (row_bytes + 1);
    }

    let pixels = if hdr.depth == 16 {
        match channels_out {
            1 => Pixels::L16(out),
            2 => Pixels::La16(out),
            3 => Pixels::Rgb16(out),
            _ => Pixels::Rgba16(out),
        }
    } else {
        let bytes: Vec<u8> = out.into_iter().map(|s| s as u8).collect();
        match channels_out {
            1 => Pixels::L8(bytes),
            2 => Pixels::La8(bytes),
            3 => Pixels::Rgb8(bytes),
            _ => Pixels::Rgba8(bytes),
        }
    };
    Ok(Image {
        width: hdr.width,
        height: hdr.height,
        pixels,
        meta,
    })
}

/// Channels the decoded image ends up with: a palette becomes RGB(A), and a
/// `tRNS` chunk adds the alpha channel the colour type lacks.
fn out_channels(hdr: &Header, trns: &Option<Vec<u8>>) -> usize {
    match hdr.color_type {
        0 => 1 + usize::from(trns.is_some()),
        2 => 3 + usize::from(trns.is_some()),
        3 => 3 + usize::from(trns.is_some()),
        4 => 2,
        _ => 4,
    }
}

/// `(width, height)` of each pass — one entry for a non-interlaced image.
fn passes(hdr: &Header) -> Vec<(u32, u32)> {
    if !hdr.interlaced {
        return vec![(hdr.width, hdr.height)];
    }
    ADAM7
        .iter()
        .map(|&(x0, y0, dx, dy)| {
            (
                hdr.width.saturating_sub(x0).div_ceil(dx),
                hdr.height.saturating_sub(y0).div_ceil(dy),
            )
        })
        .collect()
}

/// Reverse one scanline's filter in place.
fn unfilter(filter: u8, step: usize, prev: &[u8], row: &mut [u8]) -> Result<()> {
    match filter {
        0 => {}
        1 => {
            for i in step..row.len() {
                row[i] = row[i].wrapping_add(row[i - step]);
            }
        }
        2 => {
            for i in 0..row.len() {
                row[i] = row[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..row.len() {
                let left = if i >= step {
                    u16::from(row[i - step])
                } else {
                    0
                };
                let up = u16::from(prev[i]);
                row[i] = row[i].wrapping_add(((left + up) / 2) as u8);
            }
        }
        4 => {
            for i in 0..row.len() {
                let (a, c) = if i >= step {
                    (i32::from(row[i - step]), i32::from(prev[i - step]))
                } else {
                    (0, 0)
                };
                let b = i32::from(prev[i]);
                let p = a + b - c;
                let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                let pred = if pa <= pb && pa <= pc {
                    a
                } else if pb <= pc {
                    b
                } else {
                    c
                };
                row[i] = row[i].wrapping_add(pred as u8);
            }
        }
        other => return Err(Error::corrupt(format!("PNG: filter type {other}"))),
    }
    Ok(())
}

/// Pull one pixel's raw samples out of a packed scanline.
fn raw_sample(row: &[u8], depth: u8, index: usize) -> u16 {
    match depth {
        16 => u16::from_be_bytes([row[index * 2], row[index * 2 + 1]]),
        8 => u16::from(row[index]),
        _ => {
            let per_byte = 8 / depth as usize;
            let byte = row[index / per_byte];
            let shift = 8 - depth as usize * (index % per_byte + 1);
            u16::from((byte >> shift) & ((1u16 << depth) - 1) as u8)
        }
    }
}

/// Expand one unfiltered scanline into per-pixel output samples.
///
/// `emit` places pixel `px` of this pass; interlacing is entirely the caller's
/// business, which keeps one expansion path serving both layouts.
fn expand_row(
    row: &[u8],
    width: u32,
    hdr: &Header,
    palette: &[[u8; 3]],
    trns: Option<&[u8]>,
    channels_out: usize,
    mut emit: impl FnMut(u32, &[u16]),
) -> Result<()> {
    // Sub-8-bit greyscale is scaled to the full range, so a 1-bit "1" is white
    // rather than a nearly black 1/255.
    let max = ((1u32 << hdr.depth) - 1) as u16;
    let scale = |v: u16| -> u16 {
        if hdr.depth == 16 || hdr.depth == 8 {
            v
        } else {
            ((u32::from(v) * 255) / u32::from(max)) as u16
        }
    };
    let mut sample = [0u16; 4];
    for px in 0..width {
        let base = (px as usize) * hdr.channels();
        match hdr.color_type {
            0 => {
                let g = raw_sample(row, hdr.depth, base);
                sample[0] = scale(g);
                if channels_out == 2 {
                    let key = trns
                        .and_then(|t| t.get(..2))
                        .map(|t| u16::from_be_bytes([t[0], t[1]]));
                    sample[1] = if Some(g) == key {
                        0
                    } else {
                        max_for(hdr.depth)
                    };
                }
            }
            2 => {
                for c in 0..3 {
                    sample[c] = raw_sample(row, hdr.depth, base + c);
                }
                if channels_out == 4 {
                    let key = trns.and_then(|t| t.get(..6)).map(|t| {
                        [
                            u16::from_be_bytes([t[0], t[1]]),
                            u16::from_be_bytes([t[2], t[3]]),
                            u16::from_be_bytes([t[4], t[5]]),
                        ]
                    });
                    sample[3] = if key == Some([sample[0], sample[1], sample[2]]) {
                        0
                    } else {
                        max_for(hdr.depth)
                    };
                }
            }
            3 => {
                let index = raw_sample(row, hdr.depth, base) as usize;
                let entry = palette.get(index).ok_or_else(|| {
                    Error::corrupt(format!(
                        "PNG: palette index {index} in a {}-entry PLTE",
                        palette.len()
                    ))
                })?;
                sample[0] = u16::from(entry[0]);
                sample[1] = u16::from(entry[1]);
                sample[2] = u16::from(entry[2]);
                if channels_out == 4 {
                    sample[3] = u16::from(trns.and_then(|t| t.get(index)).copied().unwrap_or(255));
                }
            }
            4 => {
                sample[0] = raw_sample(row, hdr.depth, base);
                sample[1] = raw_sample(row, hdr.depth, base + 1);
            }
            _ => {
                for c in 0..4 {
                    sample[c] = raw_sample(row, hdr.depth, base + c);
                }
            }
        }
        emit(px, &sample[..channels_out]);
    }
    Ok(())
}

/// Fully opaque, at the depth the output carries.
fn max_for(depth: u8) -> u16 {
    if depth == 16 { u16::MAX } else { 255 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_known_iend_value() {
        // IEND is a fixed, empty chunk; its CRC is the same in every PNG ever
        // written, which makes it the one checksum worth pinning by hand.
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
    }

    #[test]
    fn paeth_prefers_the_nearest_predictor() {
        let prev = [10u8, 200];
        let mut row = [0u8, 0];
        unfilter(4, 1, &prev, &mut row).unwrap();
        assert_eq!(row, [10, 200]);
    }

    #[test]
    fn adam7_pass_sizes_cover_every_pixel() {
        for (w, h) in [(1u32, 1u32), (7, 5), (16, 16), (33, 17)] {
            let hdr = Header {
                width: w,
                height: h,
                depth: 8,
                color_type: 2,
                interlaced: true,
            };
            let total: u32 = passes(&hdr).iter().map(|&(pw, ph)| pw * ph).sum();
            assert_eq!(total, w * h, "{w}x{h}");
        }
    }

    /// A 1x1 white 8-bit greyscale PNG, built here rather than checked in: the
    /// zlib payload is a single stored DEFLATE block, so no encoder is needed.
    fn tiny_png() -> Vec<u8> {
        fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            let crc = crc32(&out[start..]);
            out.extend_from_slice(&crc.to_be_bytes());
        }
        let scanline = [0u8, 0xff]; // filter None, one white sample
        let mut zlib = vec![0x78, 0x01, 0x01, 0x02, 0x00, 0xfd, 0xff];
        zlib.extend_from_slice(&scanline);
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in &scanline {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

        let mut png = SIGNATURE.to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
        push_chunk(&mut png, b"IHDR", &ihdr);
        push_chunk(&mut png, b"IDAT", &zlib);
        push_chunk(&mut png, b"IEND", &[]);
        png
    }

    #[test]
    fn a_one_pixel_png_decodes_and_a_bad_crc_does_not() {
        let png = tiny_png();
        let img = decode(&png, Limits::default()).expect("the hand-built PNG");
        assert_eq!(img.dimensions(), (1, 1));
        assert_eq!(img.pixels, Pixels::L8(vec![255]));

        let mut broken = png.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;
        assert!(decode(&broken, Limits::default()).is_err(), "bad IEND CRC");
    }
}
