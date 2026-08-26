//! GIF decoding (GIF87a and GIF89a), including animations.
//!
//! A GIF frame is a palette-indexed sub-rectangle of the logical screen, so a
//! decoder that returns frames must also composite them: each frame is blended
//! onto the canvas -- a transparent pixel keeps whatever was underneath -- and
//! then the frame's disposal method says what the *next* frame starts from.
//! Disposal 2 clears the frame's rectangle to transparent rather than to the
//! logical-screen background colour, which is what every renderer in practice
//! does and what a caller compositing over its own background needs.
//!
//! The LZW dictionary widens its code one code earlier than a reading of the
//! format's prose suggests: the width grows once the entry at index
//! `2^width - 1` has been added, not at `2^width`. Encoders write it that way,
//! so a decoder that waits one code longer reads small pictures correctly and
//! large ones as noise.

use crate::{AnimationFrame, Image, ImageFormat, Info, Limits, Metadata, Pixels};
use ec_core::{Error, Result};

/// GIF interlace: `(start, step)` for each of the four passes.
const INTERLACE: [(usize, usize); 4] = [(0, 8), (4, 8), (2, 4), (1, 2)];

/// Maximum LZW code width (GIF caps at 12 bits → 4096 entries).
const MAX_CODE_SIZE: u8 = 12;

/// Maximum number of entries in the LZW dictionary.
const MAX_DICT: usize = 4096;

/// LSB-first LZW decompressor for a single GIF image-data stream.
///
/// GIF packs codes least-significant-bit first, starting at `min_code_size + 1`
/// bits.  Clear and end codes occupy `1 << min_code_size` and `+1`; derived
/// entries start at `+2`.  The dictionary is stored as parallel prefix/suffix
/// arrays so reconstruction is a single backward chain per code.
struct Lzw {
    min_code_size: u8,
    code_size: u8,
    clear_code: u16,
    end_code: u16,
    /// Code that will be assigned to the next new entry.
    next_code: u16,
    /// Prefix code for each derived entry (index = code − clear_code − 2).
    prefixes: Vec<u16>,
    /// Suffix byte for each derived entry.
    suffixes: Vec<u8>,
    // Bit accumulator
    bit_buffer: u64,
    bits_in_buffer: u8,
    /// True until the first data code after a clear has been consumed.
    first: bool,
    /// Last code decoded (needed to build the next dictionary entry).
    prev: u16,
}

impl Lzw {
    fn new(min_code_size: u8) -> Self {
        let clear_code = 1u16 << min_code_size;
        Lzw {
            min_code_size,
            code_size: min_code_size + 1,
            clear_code,
            end_code: clear_code + 1,
            next_code: clear_code + 2,
            prefixes: Vec::new(),
            suffixes: Vec::new(),
            bit_buffer: 0,
            bits_in_buffer: 0,
            first: true,
            prev: 0,
        }
    }

    fn reset(&mut self) {
        self.code_size = self.min_code_size + 1;
        self.next_code = self.clear_code + 2;
        self.prefixes.clear();
        self.suffixes.clear();
        self.first = true;
    }

    /// Pull the next `code_size`-bit code from `data`.  Returns `None` at end
    /// of input (not enough bits to fill a code).
    fn read_code(&mut self, data: &[u8], pos: &mut usize) -> Option<u16> {
        while self.bits_in_buffer < self.code_size {
            let byte = *data.get(*pos)?;
            *pos += 1;
            self.bit_buffer |= u64::from(byte) << self.bits_in_buffer;
            self.bits_in_buffer += 8;
        }
        let mask = (1u64 << self.code_size) - 1;
        let code = (self.bit_buffer & mask) as u16;
        self.bit_buffer >>= self.code_size;
        self.bits_in_buffer -= self.code_size;
        Some(code)
    }

    /// Walk the prefix chain for `code`, appending the decoded bytes to `out`.
    /// Returns the first byte of the sequence (needed for the `cScSc` case).
    fn reconstruct(&self, code: u16, out: &mut Vec<u8>) -> u8 {
        // Build the byte sequence by walking backwards, then reverse.
        let mut stack: Vec<u8> = Vec::new();
        let mut c = code;
        loop {
            if c < self.clear_code {
                // Root entry — a single literal byte.
                stack.push(c as u8);
                break;
            }
            let idx = (c - self.clear_code - 2) as usize;
            stack.push(self.suffixes[idx]);
            c = self.prefixes[idx];
        }
        let first = *stack.last().unwrap();
        // stack is in reverse order; pop to emit forward.
        while let Some(b) = stack.pop() {
            out.push(b);
        }
        first
    }

    /// Add the derived entry `(prev, first_byte)`, and widen the code if that
    /// entry filled the width: the comparison happens before `next_code` is
    /// incremented, so the widening lands one code earlier than the format's
    /// prose reads (see the module docs).
    fn derive(&mut self, prev: u16, first_byte: u8) {
        if (self.next_code as usize) < MAX_DICT {
            self.prefixes.push(prev);
            self.suffixes.push(first_byte);
            if self.next_code >= (1u16 << self.code_size) - 1 && self.code_size < MAX_CODE_SIZE {
                self.code_size += 1;
            }
            self.next_code += 1;
        }
    }
}

/// Decompress a GIF image-data stream into a vector of palette indices.
///
/// `expected` is `width * height` — the output is checked against it so a
/// truncated stream is caught rather than silently producing a short frame.
fn lzw_decode(min_code_size: u8, data: &[u8], expected: usize) -> Result<Vec<u8>> {
    if !(1..=11).contains(&min_code_size) {
        return Err(Error::corrupt(format!(
            "GIF: LZW min code size {min_code_size}"
        )));
    }

    let mut lzw = Lzw::new(min_code_size);
    let mut out = Vec::with_capacity(expected);
    let mut pos = 0;

    while let Some(code) = lzw.read_code(data, &mut pos) {
        if code == lzw.clear_code {
            lzw.reset();
            continue;
        }
        if code == lzw.end_code {
            break;
        }

        if lzw.first {
            // The first code after a clear outputs directly and seeds `prev`
            // but creates no dictionary entry — the GIF LZW variant does not
            // add an entry for the very first code in a run.
            if code >= lzw.next_code {
                return Err(Error::corrupt(format!(
                    "GIF: first LZW code {code} ≥ next {}",
                    lzw.next_code
                )));
            }
            lzw.reconstruct(code, &mut out);
            lzw.prev = code;
            lzw.first = false;
            continue;
        }

        let first_byte = if code < lzw.next_code {
            // Code is already in the dictionary.
            lzw.reconstruct(code, &mut out)
        } else if code == lzw.next_code {
            // The `cScSc` special case: the code references the entry that is
            // about to be created.  Its expansion is `prev + prev[0]`.
            let f = lzw.reconstruct(lzw.prev, &mut out);
            out.push(f);
            f
        } else {
            return Err(Error::corrupt(format!(
                "GIF: LZW code {code} > next {}",
                lzw.next_code
            )));
        };

        lzw.derive(lzw.prev, first_byte);
        lzw.prev = code;
    }

    if out.len() < expected {
        return Err(Error::corrupt(format!(
            "GIF: LZW produced {} indices, expected {expected}",
            out.len()
        )));
    }
    out.truncate(expected);
    Ok(out)
}

/// Cursor over the raw GIF bytes with bounds-checked primitives.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn u8(&mut self) -> Result<u8> {
        let b = self
            .data
            .get(self.pos)
            .copied()
            .ok_or_else(|| Error::corrupt("GIF: unexpected end of data"))?;
        self.pos += 1;
        Ok(b)
    }

    fn u16(&mut self) -> Result<u16> {
        let lo = u16::from(self.u8()?);
        let hi = u16::from(self.u8()?);
        Ok(lo | (hi << 8))
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let slice = self
            .data
            .get(self.pos..self.pos + n)
            .ok_or_else(|| Error::corrupt("GIF: unexpected end of data"))?;
        self.pos += n;
        Ok(slice)
    }

    /// Read a sub-block chain (size-prefixed segments, terminated by a 0
    /// size byte) into one contiguous buffer.
    fn sub_blocks(&mut self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let size = self.u8()? as usize;
            if size == 0 {
                break;
            }
            out.extend_from_slice(self.bytes(size)?);
        }
        Ok(out)
    }

    /// Skip a sub-block chain without copying.
    fn skip_sub_blocks(&mut self) -> Result<()> {
        loop {
            let size = self.u8()? as usize;
            if size == 0 {
                break;
            }
            self.bytes(size)?;
        }
        Ok(())
    }
}

/// Graphic Control Extension fields that affect compositing.
#[derive(Debug, Clone, Copy)]
struct Gce {
    disposal: u8,
    has_transparent: bool,
    transparent_index: u8,
    /// Delay in centiseconds (1/100 s).
    delay: u16,
}

/// One image descriptor plus its decoded palette indices and pending GCE.
struct RawFrame {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    interlaced: bool,
    palette: Vec<[u8; 3]>,
    indices: Vec<u8>,
    gce: Option<Gce>,
}

/// Reorder rows from GIF's four-pass interlace order into sequential order.
fn deinterlace(indices: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * height];
    let mut src = 0usize;
    for &(start, step) in &INTERLACE {
        let mut dst = start;
        while dst < height {
            out[dst * width..(dst + 1) * width]
                .copy_from_slice(&indices[src * width..(src + 1) * width]);
            src += 1;
            dst += step;
        }
    }
    out
}

/// Map palette indices to RGBA pixels; the transparent index becomes a fully
/// transparent pixel, which is what the compositing step tests for.
fn indices_to_rgba(indices: &[u8], palette: &[[u8; 3]], gce: Option<&Gce>) -> Result<Vec<[u8; 4]>> {
    let transparent = gce
        .filter(|g| g.has_transparent)
        .map(|g| g.transparent_index);
    let mut out = Vec::with_capacity(indices.len());
    for &idx in indices {
        if Some(idx) == transparent {
            out.push([0, 0, 0, 0]);
        } else {
            let c = palette
                .get(idx as usize)
                .ok_or_else(|| Error::corrupt(format!("GIF: colour index {idx} past palette")))?;
            out.push([c[0], c[1], c[2], 255]);
        }
    }
    Ok(out)
}

/// Composite every raw frame onto a full-canvas buffer: blend the frame over
/// the canvas, then apply its disposal method to the canvas the next frame
/// starts from.
fn composite(frames: Vec<RawFrame>, canvas_w: u32, canvas_h: u32) -> Result<Vec<AnimationFrame>> {
    let cw = canvas_w as usize;
    let ch = canvas_h as usize;
    let n = cw * ch;

    let mut canvas = vec![[0u8; 4]; n];
    let mut result: Vec<(Vec<[u8; 4]>, u32)> = Vec::with_capacity(frames.len());

    for frame in frames {
        let indices = if frame.interlaced {
            deinterlace(&frame.indices, frame.width as usize, frame.height as usize)
        } else {
            frame.indices
        };
        let rgba = indices_to_rgba(&indices, &frame.palette, frame.gce.as_ref())?;

        let fw = frame.width as usize;
        let fh = frame.height as usize;
        let fl = frame.left as usize;
        let ft = frame.top as usize;
        let disposal = frame.gce.map(|g| g.disposal).unwrap_or(0);

        // Start from the current canvas (out-of-bounds pixels keep canvas).
        let mut output = canvas.clone();

        for fy in 0..fh {
            let cy = ft + fy;
            if cy >= ch {
                break;
            }
            for fx in 0..fw {
                let cx = fl + fx;
                if cx >= cw {
                    break;
                }
                let src = &rgba[fy * fw + fx];
                let idx = cy * cw + cx;
                // Blend: a transparent frame pixel inherits the canvas.
                let composited = if src[3] == 0 { canvas[idx] } else { *src };
                output[idx] = composited;
                // Dispose: update the canvas for the *next* frame.
                match disposal {
                    2 => canvas[idx] = [0, 0, 0, 0], // Background
                    3 => { /* Previous: leave canvas unchanged */ }
                    _ => canvas[idx] = composited, // Keep / Any / unspecified
                }
            }
        }

        let delay = frame.gce.map(|g| g.delay).unwrap_or(0);
        result.push((output, u32::from(delay)));
    }

    // One colour form for the whole animation: a caller that walks the frames
    // must not have the pixel layout change under it halfway through, so a
    // single transparent pixel anywhere makes every frame RGBA.
    let any_alpha = result
        .iter()
        .any(|(output, _)| output.iter().any(|p| p[3] < 255));

    Ok(result
        .into_iter()
        .map(|(output, delay)| {
            let pixels = if any_alpha {
                Pixels::Rgba8(output.iter().flat_map(|p| *p).collect())
            } else {
                Pixels::Rgb8(output.iter().flat_map(|p| p[..3].to_vec()).collect())
            };
            AnimationFrame {
                image: Image {
                    width: canvas_w,
                    height: canvas_h,
                    pixels,
                    meta: Metadata::default(),
                },
                delay_num: delay,
                delay_den: 100,
            }
        })
        .collect())
}

/// Dimensions and format from the logical screen descriptor alone.
pub fn info(data: &[u8]) -> Result<Info> {
    if data.len() < 13 {
        return Err(Error::corrupt("GIF: shorter than the 13-byte header"));
    }
    let sig = &data[..6];
    if sig != b"GIF87a" && sig != b"GIF89a" {
        return Err(Error::corrupt("GIF: bad signature"));
    }
    let width = u16::from_le_bytes([data[6], data[7]]) as u32;
    let height = u16::from_le_bytes([data[8], data[9]]) as u32;
    Ok(Info {
        format: ImageFormat::Gif,
        width,
        height,
    })
}

/// Decode a GIF, returning the first composited frame as a still [`Image`].
pub fn decode(data: &[u8], limits: Limits) -> Result<Image> {
    let frames = decode_animation(data, limits)?;
    frames
        .into_iter()
        .next()
        .map(|f| f.image)
        .ok_or_else(|| Error::corrupt("GIF: no frames"))
}

/// Decode every frame of a GIF, composited onto a full-canvas buffer.
pub fn decode_animation(data: &[u8], limits: Limits) -> Result<Vec<AnimationFrame>> {
    let mut r = Reader::new(data);

    // Header / signature
    let sig = r.bytes(6)?;
    if sig != b"GIF87a" && sig != b"GIF89a" {
        return Err(Error::corrupt("GIF: bad signature"));
    }

    // Logical Screen Descriptor
    let canvas_w = u32::from(r.u16()?);
    let canvas_h = u32::from(r.u16()?);
    limits.check(canvas_w, canvas_h)?;
    let packed = r.u8()?;
    let _bg = r.u8()?;
    let _aspect = r.u8()?;

    // The canvas-sized RGBA buffer is the largest allocation; check it
    // against max_alloc before requesting it.
    let canvas_bytes = (canvas_w as usize)
        .checked_mul(canvas_h as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| Error::corrupt("GIF: canvas size overflows usize"))?;
    if canvas_bytes > limits.max_alloc {
        return Err(Error::unsupported(
            format!("{canvas_w}x{canvas_h} RGBA canvas"),
            format!("{canvas_bytes} bytes past the {} limit", limits.max_alloc),
        ));
    }

    let has_gct = packed & 0x80 != 0;
    let gct_len = 1usize << ((packed & 0x07) + 1);

    let global_palette: Vec<[u8; 3]> = if has_gct {
        let raw = r.bytes(gct_len * 3)?;
        raw.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
    } else {
        Vec::new()
    };

    // Block loop
    let mut frames = Vec::new();
    let mut pending_gce: Option<Gce> = None;

    loop {
        let block = r.u8()?;
        match block {
            0x2C => {
                // Image Descriptor
                let left = u32::from(r.u16()?);
                let top = u32::from(r.u16()?);
                let width = u32::from(r.u16()?);
                let height = u32::from(r.u16()?);
                let fpacked = r.u8()?;

                if width == 0 || height == 0 {
                    return Err(Error::corrupt("GIF: zero-size frame"));
                }

                let has_lct = fpacked & 0x80 != 0;
                let interlaced = fpacked & 0x40 != 0;
                let lct_len = 1usize << ((fpacked & 0x07) + 1);

                let palette = if has_lct {
                    let raw = r.bytes(lct_len * 3)?;
                    raw.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
                } else if global_palette.is_empty() {
                    return Err(Error::corrupt("GIF: no colour table for frame"));
                } else {
                    global_palette.clone()
                };

                // Image data: LZW min code size + sub-block chain.
                let min_code_size = r.u8()?;
                let lzw_data = r.sub_blocks()?;
                let indices = lzw_decode(
                    min_code_size,
                    &lzw_data,
                    (width as usize) * (height as usize),
                )?;

                frames.push(RawFrame {
                    left,
                    top,
                    width,
                    height,
                    interlaced,
                    palette,
                    indices,
                    gce: pending_gce.take(),
                });
            }
            0x21 => {
                // Extension
                let label = r.u8()?;
                if label == 0xF9 {
                    // Graphic Control Extension
                    let gce_data = r.sub_blocks()?;
                    if gce_data.len() < 4 {
                        return Err(Error::corrupt("GIF: GCE shorter than 4 bytes"));
                    }
                    let gpacked = gce_data[0];
                    let delay = u16::from_le_bytes([gce_data[1], gce_data[2]]);
                    let transparent_index = gce_data[3];
                    pending_gce = Some(Gce {
                        disposal: (gpacked >> 2) & 0x07,
                        has_transparent: gpacked & 0x01 != 0,
                        transparent_index,
                        delay,
                    });
                } else {
                    // Application, comment, plain-text — skip the sub-blocks.
                    r.skip_sub_blocks()?;
                }
            }
            0x3B => break, // Trailer
            _ => {
                return Err(Error::corrupt(format!(
                    "GIF: unknown block type 0x{block:02X}"
                )));
            }
        }
    }

    if frames.is_empty() {
        return Err(Error::corrupt("GIF: no image data"));
    }

    composite(frames, canvas_w, canvas_h)
}
