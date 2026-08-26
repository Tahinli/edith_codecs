//! TIFF: the tag-directory format scanners, cameras and every "save as
//! uncompressed" dialog write.
//!
//! A TIFF is a header pointing at a directory of tags, and the tags say where
//! the pixels are and how they were packed. Almost nothing about the layout is
//! fixed: rows may be grouped into strips or into tiles, samples may be
//! interleaved or stored one plane at a time, a sample may be 1, 2, 4, 8 or 16
//! bits wide, and the bytes may be little- or big-endian. This reads all of
//! those, with the compressions in general use -- none, LZW, PackBits and
//! Deflate -- and the horizontal-differencing predictor that usually
//! accompanies the last two.
//!
//! Refused by name, because the capability is genuinely absent rather than
//! merely unwritten: the CCITT fax compressions (2, 3, 4), which are a
//! different coder altogether, JPEG-in-TIFF (7), and BigTIFF, whose header is
//! a different format that happens to share a signature byte. The incumbent
//! `image` crate refuses all three as well.

use crate::{Image, ImageFormat, Info, Limits, Metadata, Pixels};
use ec_core::{Error, Result};

const TAG_WIDTH: u16 = 256;
const TAG_HEIGHT: u16 = 257;
const TAG_BITS: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_ORIENTATION: u16 = 274;
const TAG_SAMPLES: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_COUNTS: u16 = 279;
const TAG_PLANAR: u16 = 284;
const TAG_PREDICTOR: u16 = 317;
const TAG_COLOR_MAP: u16 = 320;
const TAG_TILE_WIDTH: u16 = 322;
const TAG_TILE_HEIGHT: u16 = 323;
const TAG_TILE_OFFSETS: u16 = 324;
const TAG_TILE_COUNTS: u16 = 325;
const TAG_EXTRA_SAMPLES: u16 = 338;
const TAG_SAMPLE_FORMAT: u16 = 339;

const COMPRESSION_NONE: u64 = 1;
const COMPRESSION_LZW: u64 = 5;
const COMPRESSION_DEFLATE: u64 = 8;
const COMPRESSION_PACKBITS: u64 = 32773;
const COMPRESSION_DEFLATE_OLD: u64 = 32946;

/// The endianness the file declared, and the bytes it declared it about.
struct Reader<'a> {
    data: &'a [u8],
    big_endian: bool,
}

impl<'a> Reader<'a> {
    fn u16_at(&self, at: usize) -> Result<u16> {
        let bytes = self
            .data
            .get(at..at + 2)
            .ok_or_else(|| Error::corrupt("TIFF: a field past the end of the file"))?;
        Ok(if self.big_endian {
            u16::from_be_bytes([bytes[0], bytes[1]])
        } else {
            u16::from_le_bytes([bytes[0], bytes[1]])
        })
    }

    fn u32_at(&self, at: usize) -> Result<u32> {
        let bytes = self
            .data
            .get(at..at + 4)
            .ok_or_else(|| Error::corrupt("TIFF: a field past the end of the file"))?;
        Ok(if self.big_endian {
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        })
    }
}

/// One directory entry, with its values already read.
struct Entry {
    tag: u16,
    values: Vec<u64>,
}

/// Bytes one value of each TIFF type occupies.
fn type_size(kind: u16) -> Option<usize> {
    Some(match kind {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

/// The image file directory at `offset`, as entries whose integer values are
/// already resolved.
///
/// Values of four bytes or fewer live in the entry itself; anything longer is
/// at an offset the entry holds. Non-integer types (rationals, floats, ASCII)
/// are read as raw words and ignored by every caller here, which only asks for
/// the geometry tags.
fn directory(reader: &Reader, offset: usize) -> Result<Vec<Entry>> {
    let count = reader.u16_at(offset)? as usize;
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let at = offset + 2 + i * 12;
        let tag = reader.u16_at(at)?;
        let kind = reader.u16_at(at + 2)?;
        let n = reader.u32_at(at + 4)? as usize;
        let Some(size) = type_size(kind) else {
            continue; // a type this decoder has no use for
        };
        // A count large enough to overflow the file is corrupt, and a count
        // large enough to exhaust memory is corrupt too: the biggest legitimate
        // value list is one offset per strip.
        if n > reader.data.len() {
            return Err(Error::corrupt("TIFF: a tag claiming more values than bytes"));
        }
        let total = n * size;
        let base = if total <= 4 {
            at + 8
        } else {
            reader.u32_at(at + 8)? as usize
        };
        let mut values = Vec::with_capacity(n);
        for v in 0..n {
            let at = base + v * size;
            values.push(match size {
                1 => u64::from(
                    *reader
                        .data
                        .get(at)
                        .ok_or_else(|| Error::corrupt("TIFF: a value past the end of the file"))?,
                ),
                2 => u64::from(reader.u16_at(at)?),
                4 => u64::from(reader.u32_at(at)?),
                // Rationals and doubles: the numerator is the only half any
                // tag read here would want, and none of them do.
                _ => u64::from(reader.u32_at(at)?),
            });
        }
        entries.push(Entry { tag, values });
    }
    Ok(entries)
}

/// Everything the tags say about the pixels.
struct Layout {
    width: u32,
    height: u32,
    bits: Vec<u16>,
    samples: usize,
    photometric: u64,
    compression: u64,
    planar: u64,
    predictor: u64,
    /// Strip or tile data offsets, in the order the file lists them.
    offsets: Vec<u64>,
    counts: Vec<u64>,
    /// `None` for a stripped file; the tile size for a tiled one.
    tile: Option<(u32, u32)>,
    rows_per_strip: u32,
    colour_map: Vec<u64>,
    /// Tag 338: whether the extra sample is alpha, and whether it is premultiplied.
    extra_samples: Vec<u64>,
    orientation: Option<u8>,
}

fn tag<'e>(entries: &'e [Entry], tag: u16) -> Option<&'e Entry> {
    entries.iter().find(|e| e.tag == tag)
}

fn first(entries: &[Entry], want: u16, default: u64) -> u64 {
    tag(entries, want)
        .and_then(|e| e.values.first().copied())
        .unwrap_or(default)
}

fn layout(data: &[u8]) -> Result<Layout> {
    if data.len() < 8 {
        return Err(Error::corrupt("TIFF: a file too short to hold a header"));
    }
    let big_endian = match &data[0..2] {
        b"II" => false,
        b"MM" => true,
        _ => return Err(Error::corrupt("TIFF: no II or MM byte order mark")),
    };
    let reader = Reader { data, big_endian };
    match reader.u16_at(2)? {
        42 => {}
        43 => {
            return Err(Error::unsupported(
                "TIFF: a BigTIFF file (version 43)",
                "BigTIFF replaces every offset and count with a 64-bit one; \
                 it shares the byte order mark but not the directory format",
            ));
        }
        other => {
            return Err(Error::corrupt(format!("TIFF: version {other}")));
        }
    }
    let entries = directory(&reader, reader.u32_at(4)? as usize)?;

    let compression = first(&entries, TAG_COMPRESSION, COMPRESSION_NONE);
    match compression {
        COMPRESSION_NONE
        | COMPRESSION_LZW
        | COMPRESSION_DEFLATE
        | COMPRESSION_PACKBITS
        | COMPRESSION_DEFLATE_OLD => {}
        2 | 3 | 4 => {
            return Err(Error::unsupported(
                format!("TIFF: CCITT compression {compression}"),
                "the fax coders are a separate format with their own \
                 two-dimensional coding modes",
            ));
        }
        6 | 7 => {
            return Err(Error::unsupported(
                "TIFF: a JPEG-compressed strip",
                "the strips hold JPEG scans with the tables in a separate tag; \
                 decode such a file with the JPEG decoder once it is assembled",
            ));
        }
        other => return Err(Error::corrupt(format!("TIFF: compression {other}"))),
    }

    let samples = first(&entries, TAG_SAMPLES, 1) as usize;
    if samples == 0 || samples > 4 {
        return Err(Error::corrupt(format!(
            "TIFF: {samples} samples per pixel"
        )));
    }
    let bits: Vec<u16> = tag(&entries, TAG_BITS)
        .map(|e| e.values.iter().map(|&v| v as u16).collect())
        .unwrap_or_else(|| vec![1; samples]);
    if bits.len() != samples {
        return Err(Error::corrupt(
            "TIFF: a bit-depth list that does not match the sample count",
        ));
    }
    if bits.iter().any(|b| *b != bits[0]) {
        return Err(Error::unsupported(
            "TIFF: channels of different depths",
            "every channel is unpacked with one width; a file mixing them \
             would need a per-channel unpacker",
        ));
    }
    if !matches!(bits[0], 1 | 2 | 4 | 8 | 16) {
        return Err(Error::unsupported(
            format!("TIFF: {}-bit samples", bits[0]),
            "1, 2, 4, 8 and 16 are the widths this decoder unpacks",
        ));
    }
    if first(&entries, TAG_SAMPLE_FORMAT, 1) == 3 {
        return Err(Error::unsupported(
            "TIFF: floating-point samples",
            "sample format 3 stores IEEE floats, which have no fixed range to \
             map onto an integer picture",
        ));
    }

    let photometric = first(&entries, TAG_PHOTOMETRIC, 1);
    if !matches!(photometric, 0 | 1 | 2 | 3) {
        return Err(Error::unsupported(
            format!("TIFF: photometric interpretation {photometric}"),
            "white-is-zero, black-is-zero, RGB and palette are the colour \
             models this decoder converts",
        ));
    }

    let predictor = first(&entries, TAG_PREDICTOR, 1);
    if !matches!(predictor, 1 | 2) {
        return Err(Error::unsupported(
            format!("TIFF: predictor {predictor}"),
            "1 (none) and 2 (horizontal differencing) are the integer \
             predictors; 3 is the floating-point one",
        ));
    }

    let tile = match (tag(&entries, TAG_TILE_WIDTH), tag(&entries, TAG_TILE_HEIGHT)) {
        (Some(w), Some(h)) => Some((
            *w.values.first().unwrap_or(&0) as u32,
            *h.values.first().unwrap_or(&0) as u32,
        )),
        _ => None,
    };
    let (offsets_tag, counts_tag) = if tile.is_some() {
        (TAG_TILE_OFFSETS, TAG_TILE_COUNTS)
    } else {
        (TAG_STRIP_OFFSETS, TAG_STRIP_COUNTS)
    };
    let offsets = tag(&entries, offsets_tag)
        .map(|e| e.values.clone())
        .ok_or_else(|| Error::corrupt("TIFF: no strip or tile offsets"))?;
    let counts = tag(&entries, counts_tag)
        .map(|e| e.values.clone())
        .ok_or_else(|| Error::corrupt("TIFF: no strip or tile byte counts"))?;
    if offsets.len() != counts.len() {
        return Err(Error::corrupt(
            "TIFF: as many offsets as byte counts is the one thing the format promises",
        ));
    }

    let orientation = match first(&entries, TAG_ORIENTATION, 0) {
        v @ 1..=8 => Some(v as u8),
        _ => None,
    };

    Ok(Layout {
        width: first(&entries, TAG_WIDTH, 0) as u32,
        height: first(&entries, TAG_HEIGHT, 0) as u32,
        bits,
        samples,
        photometric,
        compression,
        planar: first(&entries, TAG_PLANAR, 1),
        predictor,
        offsets,
        counts,
        tile,
        rows_per_strip: first(&entries, TAG_ROWS_PER_STRIP, u64::from(u32::MAX)).min(u64::from(u32::MAX))
            as u32,
        colour_map: tag(&entries, TAG_COLOR_MAP)
            .map(|e| e.values.clone())
            .unwrap_or_default(),
        extra_samples: tag(&entries, TAG_EXTRA_SAMPLES)
            .map(|e| e.values.clone())
            .unwrap_or_default(),
        orientation,
    })
}

/// Dimensions from the first directory, without decoding the pixels.
pub fn info(data: &[u8]) -> Result<Info> {
    let layout = layout(data)?;
    Ok(Info {
        format: ImageFormat::Tiff,
        width: layout.width,
        height: layout.height,
    })
}

/// TIFF's LZW: MSB-first codes, and the width grows one code early.
///
/// The "one code early" is the difference from the GIF variant: a TIFF encoder
/// switches to the next width when the *next* code to be assigned would not
/// fit, so a decoder that waits for the code itself desynchronises a byte into
/// every image.
fn lzw(input: &[u8], hint: usize) -> Result<Vec<u8>> {
    const CLEAR: u16 = 256;
    const END: u16 = 257;
    let mut out = Vec::with_capacity(hint);
    // Each entry is a previous code plus one byte, walked backwards on emit,
    // which keeps the table one allocation instead of one per string.
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut next = 258usize;
    let mut width = 9u32;
    let mut previous: Option<u16> = None;
    let (mut bit, mut acc, mut have) = (0usize, 0u32, 0u32);
    let mut stack = Vec::with_capacity(4096);
    loop {
        while have < width {
            let Some(&byte) = input.get(bit) else {
                return Ok(out);
            };
            bit += 1;
            acc = (acc << 8) | u32::from(byte);
            have += 8;
        }
        let code = ((acc >> (have - width)) & ((1 << width) - 1)) as u16;
        have -= width;
        match code {
            CLEAR => {
                next = 258;
                width = 9;
                previous = None;
                continue;
            }
            END => return Ok(out),
            _ => {}
        }
        stack.clear();
        let first_byte = if (code as usize) < next {
            let mut walk = code;
            while walk >= 258 {
                stack.push(suffix[walk as usize]);
                walk = prefix[walk as usize];
            }
            if walk > 255 {
                return Err(Error::corrupt("TIFF: an LZW code with no string"));
            }
            stack.push(walk as u8);
            *stack.last().unwrap()
        } else if let Some(previous) = previous {
            // The KwKwK case: the code being emitted is the one this step is
            // about to define.
            let mut walk = previous;
            while walk >= 258 {
                stack.push(suffix[walk as usize]);
                walk = prefix[walk as usize];
            }
            stack.push(walk as u8);
            let first = *stack.last().unwrap();
            stack.insert(0, first);
            first
        } else {
            return Err(Error::corrupt("TIFF: an LZW stream opening with a new code"));
        };
        out.extend(stack.iter().rev());
        if let Some(previous) = previous {
            if next < 4096 {
                prefix[next] = previous;
                suffix[next] = first_byte;
                next += 1;
            }
        }
        previous = Some(code);
        // One code early: 511, not 512, is where nine bits stop being enough.
        if next + 1 >= (1 << width) && width < 12 {
            width += 1;
        }
    }
}

/// PackBits: a run-length coder whose control byte is signed.
fn packbits(input: &[u8], hint: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(hint);
    let mut at = 0usize;
    while at < input.len() {
        let control = input[at] as i8;
        at += 1;
        if control >= 0 {
            let run = control as usize + 1;
            let end = (at + run).min(input.len());
            out.extend_from_slice(&input[at..end]);
            at = end;
        } else if control != -128 {
            let run = (-i32::from(control)) as usize + 1;
            let Some(&byte) = input.get(at) else { break };
            at += 1;
            out.extend(std::iter::repeat_n(byte, run));
        } else {
            // -128 is a no-op the format reserves.
        }
    }
    out
}

/// One strip or tile, decompressed.
fn segment(layout: &Layout, data: &[u8], index: usize, expect: usize) -> Result<Vec<u8>> {
    let at = layout.offsets[index] as usize;
    let len = layout.counts[index] as usize;
    let body = data
        .get(at..at.saturating_add(len))
        .ok_or_else(|| Error::corrupt("TIFF: a strip reaching past the end of the file"))?;
    Ok(match layout.compression {
        COMPRESSION_NONE => body.to_vec(),
        COMPRESSION_LZW => lzw(body, expect)?,
        COMPRESSION_PACKBITS => packbits(body, expect),
        _ => ec_inflate::inflate_zlib(body, expect.saturating_mul(2).max(1024))
            .map_err(|e| Error::corrupt(format!("TIFF: a Deflate strip: {e}")))?,
    })
}

/// Undo horizontal differencing across one row of `channels` interleaved samples.
/// Horizontal differencing runs at the sample's own width, so the sum wraps at
/// `mask + 1` -- an 8-bit file wraps at 256, not at the u16 these are held in.
fn undo_predictor(row: &mut [u16], channels: usize, mask: u16) {
    for i in channels..row.len() {
        row[i] = row[i].wrapping_add(row[i - channels]) & mask;
    }
}

/// Read `count` samples of `bits` width from `bytes`, MSB-first.
fn unpack(bytes: &[u8], bits: u16, count: usize, out: &mut Vec<u16>, big_endian: bool) {
    match bits {
        16 => {
            for i in 0..count {
                let (a, b) = (
                    bytes.get(i * 2).copied().unwrap_or(0),
                    bytes.get(i * 2 + 1).copied().unwrap_or(0),
                );
                out.push(if big_endian {
                    u16::from_be_bytes([a, b])
                } else {
                    u16::from_le_bytes([a, b])
                });
            }
        }
        8 => out.extend(
            (0..count).map(|i| u16::from(bytes.get(i).copied().unwrap_or(0))),
        ),
        _ => {
            let per_byte = 8 / bits as usize;
            let mask = (1u16 << bits) - 1;
            for i in 0..count {
                let byte = bytes.get(i / per_byte).copied().unwrap_or(0);
                let shift = 8 - bits as usize * (i % per_byte + 1);
                out.push((u16::from(byte) >> shift) & mask);
            }
        }
    }
}

/// Decode the first image in the file.
pub fn decode(data: &[u8], limits: Limits) -> Result<Image> {
    let layout = layout(data)?;
    limits.check(layout.width, layout.height)?;
    let (width, height) = (layout.width as usize, layout.height as usize);
    if width == 0 || height == 0 {
        return Err(Error::corrupt("TIFF: a zero-sized picture"));
    }
    let big_endian = data.starts_with(b"MM");
    let bits = layout.bits[0];
    let samples = layout.samples;
    let planar = layout.planar == 2;
    let planes = if planar { samples } else { 1 };
    let per_plane = if planar { 1 } else { samples };

    // Samples land here interleaved, whatever the file's own arrangement was.
    let mut raw = vec![0u16; width * height * samples];

    // Strips and tiles differ only in the rectangle one segment covers.
    let (seg_w, seg_h) = match layout.tile {
        Some((w, h)) if w > 0 && h > 0 => (w as usize, h as usize),
        Some(_) => return Err(Error::corrupt("TIFF: a zero-sized tile")),
        None => (width, layout.rows_per_strip.max(1) as usize),
    };
    let across = width.div_ceil(seg_w);
    let down = height.div_ceil(seg_h);
    let per_plane_segments = across * down;
    if layout.offsets.len() < per_plane_segments * planes {
        return Err(Error::corrupt(
            "TIFF: fewer strips than the geometry needs",
        ));
    }

    // A tile is always its full width; a strip is only as wide as the picture,
    // and its last one only as tall as the rows that remain.
    let row_samples = seg_w * per_plane;
    let row_bytes = (row_samples * bits as usize).div_ceil(8);
    let mut row = Vec::with_capacity(row_samples);
    let full = ((1u32 << bits) - 1) as u16;
    for index in 0..per_plane_segments * planes {
        let plane = index / per_plane_segments;
        let within = index % per_plane_segments;
        let (col, band) = (within % across, within / across);
        let expect = row_bytes * seg_h;
        let body = segment(&layout, data, index, expect)?;
        for r in 0..seg_h {
            let y = band * seg_h + r;
            if y >= height {
                break;
            }
            let at = r * row_bytes;
            let Some(bytes) = body.get(at..(at + row_bytes).min(body.len())) else {
                break;
            };
            row.clear();
            unpack(bytes, bits, row_samples, &mut row, big_endian);
            if layout.predictor == 2 {
                undo_predictor(&mut row, per_plane, full);
            }
            for x in 0..seg_w {
                let px = col * seg_w + x;
                if px >= width {
                    break;
                }
                for s in 0..per_plane {
                    let channel = if planar { plane } else { s };
                    raw[(y * width + px) * samples + channel] = row[x * per_plane + s];
                }
            }
        }
    }

    // Widths below 8 bits are stretched to full scale so that a bilevel file's
    // 1 becomes white rather than one two-hundred-and-fifty-fifth of it.
    let pixels = build(&layout, &raw, width * height, bits, full)?;
    Ok(Image {
        width: layout.width,
        height: layout.height,
        pixels,
        meta: Metadata {
            orientation: layout.orientation,
            ..Metadata::default()
        },
    })
}

/// Turn raw samples into the crate's colour form.
fn build(layout: &Layout, raw: &[u16], count: usize, bits: u16, full: u16) -> Result<Pixels> {
    let samples = layout.samples;
    let alpha = samples == 2 || samples == 4;
    // Extra sample type 1 is straight alpha and 2 is associated -- the colour
    // channels already multiplied by it. This crate carries straight alpha, so
    // an associated file is divided back out; where alpha is full, which is
    // what ImageMagick writes for an opaque picture, that is the identity.
    let associated = alpha && layout.extra_samples.first() == Some(&2);
    // 8-bit output for everything but a 16-bit file, whose depth survives.
    let widen = |v: u16| -> u16 {
        if bits == 16 {
            v
        } else {
            // round(v * 65535 / full)
            ((u32::from(v) * 65535 + u32::from(full) / 2) / u32::from(full)) as u16
        }
    };
    let narrow = |v: u16| -> u8 { ((u32::from(v) * 255 + 32767) / 65535) as u8 };

    if layout.photometric == 3 {
        // A palette: three ranges of 16-bit values, one per channel, in a
        // single tag.
        let entries = layout.colour_map.len() / 3;
        if entries == 0 {
            return Err(Error::corrupt("TIFF: a palette picture with no colour map"));
        }
        let mut out = Vec::with_capacity(count * 3);
        for &index in raw.iter().take(count) {
            let index = index as usize;
            if index >= entries {
                return Err(Error::corrupt("TIFF: a palette index past the colour map"));
            }
            for c in 0..3 {
                out.push(narrow(layout.colour_map[c * entries + index] as u16));
            }
        }
        return Ok(Pixels::Rgb8(out));
    }

    let invert = layout.photometric == 0;
    let mut wide: Vec<u16> = Vec::with_capacity(raw.len());
    for (i, &v) in raw.iter().enumerate() {
        let is_alpha = alpha && (i % samples) == samples - 1;
        let v = widen(v);
        wide.push(if invert && !is_alpha { 65535 - v } else { v });
    }
    if associated {
        for pixel in wide.chunks_mut(samples) {
            let a = u32::from(pixel[samples - 1]);
            for channel in &mut pixel[..samples - 1] {
                *channel = if a == 0 {
                    0
                } else {
                    (((u32::from(*channel) * 65535 + a / 2) / a).min(65535)) as u16
                };
            }
        }
    }

    Ok(match (samples, bits) {
        (1, 16) => Pixels::L16(wide),
        (2, 16) => Pixels::La16(wide),
        (3, 16) => Pixels::Rgb16(wide),
        (4, 16) => Pixels::Rgba16(wide),
        (1, _) => Pixels::L8(wide.iter().map(|&v| narrow(v)).collect()),
        (2, _) => Pixels::La8(wide.iter().map(|&v| narrow(v)).collect()),
        (3, _) => Pixels::Rgb8(wide.iter().map(|&v| narrow(v)).collect()),
        _ => Pixels::Rgba8(wide.iter().map(|&v| narrow(v)).collect()),
    })
}
