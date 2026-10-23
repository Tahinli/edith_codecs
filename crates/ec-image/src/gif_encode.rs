//! Animated GIF encoding: GIF89a with a per-frame palette and LZW.
//!
//! Each [`Encoder::push_rgba8`] quantises one frame to at most `max_colors`
//! entries (median cut, optionally Floyd-Steinberg dithered) and writes it as a
//! Graphic Control Extension plus an image with its own local colour table.
//! Alpha below 128 maps to one reserved transparent index.
//!
//! The file is built so that the canvas a decoder composites is what the frames
//! show. Every frame uses disposal "do not dispose", so a pixel equal to the
//! previous frame is encoded as transparent and keeps the value already on the
//! canvas, and each image is cropped to the bounding box of the changed pixels.
//! A frame with no change folds its delay into the previous frame, so a static
//! run costs one picture and no extra frames.
//!
//! Output decodes through [`crate::gif::decode_animation`]. No unsafe, no dependencies.

use ec_core::{Error, Result};
use std::collections::HashMap;

/// Options for [`Encoder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GifOptions {
    /// NETSCAPE2.0 loop count. `None` writes no extension, so the animation plays once;
    /// `Some(0)` loops forever; `Some(n)` writes `n` as the count the decoder repeats.
    pub loop_count: Option<u16>,
    /// Palette size per frame, 2..=256. A frame that needs a transparent index spends one
    /// of these entries on it.
    pub max_colors: u16,
    /// Floyd-Steinberg dithering of the opaque pixels. Off gives the nearest palette entry.
    pub dither: bool,
}

impl Default for GifOptions {
    fn default() -> Self {
        GifOptions {
            loop_count: None,
            max_colors: 256,
            dither: false,
        }
    }
}

/// Builds one GIF89a file frame by frame.
///
/// Push frames with [`Encoder::push_rgba8`], then take the bytes with [`Encoder::finish`].
pub struct Encoder {
    width: u16,
    height: u16,
    opts: GifOptions,
    out: Vec<u8>,
    /// The previous frame's RGBA input, the reference for "unchanged".
    prev: Option<Vec<u8>>,
    /// Offset of the last frame's delay field in `out`, and that delay in 1/100 s.
    last: Option<(usize, u16)>,
    frames: usize,
}

/// Graphic Control disposal 1: "do not dispose", leave the frame on the canvas.
const DISPOSE_KEEP: u8 = 1 << 2;

impl Encoder {
    /// Start a `width` x `height` animation with the given options.
    pub fn new(width: u16, height: u16, opts: GifOptions) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::unsupported(
                "GIF encode",
                format!("{width}x{height} is not a GIF-sized image"),
            ));
        }
        if !(2..=256).contains(&opts.max_colors) {
            return Err(Error::unsupported(
                "GIF encode",
                format!("max_colors {} is outside 2..=256", opts.max_colors),
            ));
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"GIF89a");
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        // No global colour table: every frame carries its own.
        out.extend_from_slice(&[0, 0, 0]);
        if let Some(loops) = opts.loop_count {
            out.extend_from_slice(&[0x21, 0xff, 0x0b]);
            out.extend_from_slice(b"NETSCAPE2.0");
            out.extend_from_slice(&[0x03, 0x01]);
            out.extend_from_slice(&loops.to_le_bytes());
            out.push(0);
        }
        Ok(Encoder {
            width,
            height,
            opts,
            out,
            prev: None,
            last: None,
            frames: 0,
        })
    }

    /// Append one frame of packed R,G,B,A pixels (`width * height * 4` bytes), shown for
    /// `delay_cs` hundredths of a second.
    pub fn push_rgba8(&mut self, pixels: &[u8], delay_cs: u16) -> Result<()> {
        let (w, h) = (usize::from(self.width), usize::from(self.height));
        if pixels.len() != w * h * 4 {
            return Err(Error::unsupported(
                "GIF encode",
                format!(
                    "{} bytes of pixels for {}x{} (need {})",
                    pixels.len(),
                    self.width,
                    self.height,
                    w * h * 4
                ),
            ));
        }
        let prev = self.prev.as_deref();

        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        let mut any = false;
        for y in 0..h {
            for x in 0..w {
                if differs(prev, pixels, y * w + x) {
                    any = true;
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        let (x0, y0, x1, y1) = if any {
            (x0, y0, x1, y1)
        } else {
            // Nothing changed: fold the delay into the previous frame when it fits.
            if let Some((at, last)) = self.last {
                let merged = u32::from(last) + u32::from(delay_cs);
                if merged <= u32::from(u16::MAX) {
                    let bytes = (merged as u16).to_le_bytes();
                    self.out[at..at + 2].copy_from_slice(&bytes);
                    self.last = Some((at, merged as u16));
                    return Ok(());
                }
            }
            // Too long to merge: a 1x1 transparent frame carries the delay.
            (0, 0, 0, 0)
        };
        let (rw, rh) = (x1 - x0 + 1, y1 - y0 + 1);

        // Source of each pixel in the cropped rectangle: None is the transparent index.
        let mut src: Vec<Option<[u8; 3]>> = Vec::with_capacity(rw * rh);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let i = y * w + x;
                let visible = differs(prev, pixels, i) && pixels[i * 4 + 3] >= 128;
                src.push(visible.then(|| [pixels[i * 4], pixels[i * 4 + 1], pixels[i * 4 + 2]]));
            }
        }

        let transparent = src.iter().any(Option::is_none);
        let budget = usize::from(self.opts.max_colors) - usize::from(transparent);
        let colors: Vec<u32> = src.iter().flatten().map(|&c| pack(c)).collect();
        let palette = quantize(colors, budget);
        let indices = index_of(&src, rw, &palette, self.opts.dither);

        let palette_len = palette.len();
        let entries = palette_len + usize::from(transparent);
        let size = entries.max(2).next_power_of_two();
        let bits = size.trailing_zeros() as u8;
        let min_code = bits.max(2);
        let mut table = palette;
        if transparent {
            table.push([0, 0, 0]);
        }
        table.resize(size, [0, 0, 0]);

        // The transparent index sits right after the opaque palette.
        let tidx = palette_len as u8;
        let delay = delay_cs.to_le_bytes();
        let packed = DISPOSE_KEEP | u8::from(transparent);
        let at = self.out.len() + 4;
        self.out
            .extend_from_slice(&[0x21, 0xf9, 0x04, packed, delay[0], delay[1], tidx, 0]);
        self.last = Some((at, delay_cs));

        self.out.push(0x2c);
        for v in [x0, y0, rw, rh] {
            self.out.extend_from_slice(&(v as u16).to_le_bytes());
        }
        self.out.push(0x80 | (bits - 1));
        for c in &table {
            self.out.extend_from_slice(c);
        }
        self.out.push(min_code);
        put_sub_blocks(&mut self.out, &lzw(&indices, min_code));

        self.prev = Some(pixels.to_vec());
        self.frames += 1;
        Ok(())
    }

    /// Close the file and return its bytes. Fails when no frame was pushed.
    pub fn finish(mut self) -> Result<Vec<u8>> {
        if self.frames == 0 {
            return Err(Error::unsupported("GIF encode", "no frames were pushed"));
        }
        self.out.push(0x3b);
        Ok(self.out)
    }
}

/// Whether pixel `i` differs from the previous frame; the first frame differs everywhere.
fn differs(prev: Option<&[u8]>, pixels: &[u8], i: usize) -> bool {
    match prev {
        None => true,
        Some(p) => p[i * 4..i * 4 + 4] != pixels[i * 4..i * 4 + 4],
    }
}

fn pack(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

fn unpack(k: u32) -> [u8; 3] {
    [(k >> 16) as u8, (k >> 8) as u8, k as u8]
}

/// Channel `ch` (0 = red) of a packed colour.
fn chan(k: u32, ch: usize) -> u32 {
    (k >> (16 - 8 * ch)) & 0xff
}

/// Median-cut palette of at most `budget` entries over the given colours, weighted by count.
/// A colour set that already fits is returned exactly.
fn quantize(mut colors: Vec<u32>, budget: usize) -> Vec<[u8; 3]> {
    colors.sort_unstable();
    let mut uniq: Vec<(u32, u32)> = Vec::new();
    for c in colors {
        match uniq.last_mut() {
            Some((k, n)) if *k == c => *n += 1,
            _ => uniq.push((c, 1)),
        }
    }
    if uniq.len() <= budget {
        return uniq.iter().map(|&(k, _)| unpack(k)).collect();
    }

    let mut boxes = vec![uniq];
    while boxes.len() < budget {
        // Split the box whose widest channel spans the most.
        let mut pick: Option<(usize, usize, u32)> = None;
        for (bi, b) in boxes.iter().enumerate() {
            if b.len() < 2 {
                continue;
            }
            for ch in 0..3 {
                let (lo, hi) = b
                    .iter()
                    .fold((u32::MAX, 0), |(lo, hi), &(k, _)| {
                        let v = chan(k, ch);
                        (lo.min(v), hi.max(v))
                    });
                let range = hi - lo;
                match pick {
                    Some((_, _, r)) if r >= range => {}
                    _ => pick = Some((bi, ch, range)),
                }
            }
        }
        let Some((bi, ch, _)) = pick else { break };
        let mut b = boxes.swap_remove(bi);
        b.sort_unstable_by_key(|&(k, _)| chan(k, ch));
        let total: u64 = b.iter().map(|&(_, n)| u64::from(n)).sum();
        let mut acc = 0u64;
        let mut split = b.len() - 1;
        for (i, &(_, n)) in b.iter().enumerate() {
            acc += u64::from(n);
            if acc * 2 >= total {
                split = (i + 1).clamp(1, b.len() - 1);
                break;
            }
        }
        let right = b.split_off(split);
        boxes.push(b);
        boxes.push(right);
    }

    boxes
        .iter()
        .map(|b| {
            let total: u64 = b.iter().map(|&(_, n)| u64::from(n)).sum();
            std::array::from_fn(|ch| {
                let sum: u64 = b
                    .iter()
                    .map(|&(k, n)| u64::from(chan(k, ch)) * u64::from(n))
                    .sum();
                ((sum + total / 2) / total) as u8
            })
        })
        .collect()
}

/// Palette index for each cropped pixel; a `None` source is the transparent index, which
/// sits right after the opaque palette.
fn index_of(src: &[Option<[u8; 3]>], rw: usize, palette: &[[u8; 3]], dither: bool) -> Vec<u8> {
    if dither {
        return dither_floyd_steinberg(src, rw, palette);
    }
    let clear = palette.len() as u8;
    let mut memo: HashMap<[u8; 3], u8> = HashMap::new();
    src.iter()
        .map(|px| match px {
            None => clear,
            Some(c) => *memo
                .entry(*c)
                .or_insert_with(|| nearest(palette, c.map(i32::from))),
        })
        .collect()
}

/// Floyd-Steinberg error diffusion over the opaque pixels. Errors are carried only between
/// opaque pixels; a transparent pixel drops the error that reached it.
fn dither_floyd_steinberg(src: &[Option<[u8; 3]>], rw: usize, palette: &[[u8; 3]]) -> Vec<u8> {
    let rh = src.len() / rw;
    let clear = palette.len() as u8;
    let mut out = Vec::with_capacity(src.len());
    // Two rows of accumulated error, offset by one so x - 1 and x + 1 need no bounds test.
    let mut cur = vec![[0i32; 3]; rw + 2];
    let mut next = vec![[0i32; 3]; rw + 2];
    for y in 0..rh {
        for x in 0..rw {
            let Some(c) = src[y * rw + x] else {
                out.push(clear);
                continue;
            };
            let e = cur[x + 1];
            let v: [i32; 3] = std::array::from_fn(|ch| (i32::from(c[ch]) + e[ch]).clamp(0, 255));
            let p = nearest(palette, v);
            out.push(p);
            let q = palette[usize::from(p)].map(i32::from);
            let err = [v[0] - q[0], v[1] - q[1], v[2] - q[2]];
            for (ch, &d) in err.iter().enumerate() {
                cur[x + 2][ch] += d * 7 / 16;
                next[x][ch] += d * 3 / 16;
                next[x + 1][ch] += d * 5 / 16;
                next[x + 2][ch] += d / 16;
            }
        }
        std::mem::swap(&mut cur, &mut next);
        next.iter_mut().for_each(|e| *e = [0; 3]);
    }
    out
}

/// Index of the palette entry nearest to `rgb` in squared RGB distance.
fn nearest(palette: &[[u8; 3]], rgb: [i32; 3]) -> u8 {
    let mut best = 0usize;
    let mut best_d = i32::MAX;
    for (i, p) in palette.iter().enumerate() {
        let d: i32 = rgb
            .iter()
            .zip(p)
            .map(|(&a, &b)| {
                let t = a - i32::from(b);
                t * t
            })
            .sum();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best as u8
}

/// LSB-first bit packer for LZW codes.
#[derive(Default)]
struct Bits {
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, out: &mut Vec<u8>, code: u16, width: u32) {
        self.acc |= u32::from(code) << self.n;
        self.n += width;
        while self.n >= 8 {
            out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    fn flush(&mut self, out: &mut Vec<u8>) {
        if self.n > 0 {
            out.push(self.acc as u8);
            self.acc = 0;
            self.n = 0;
        }
    }
}

/// GIF LZW over palette indices, with `min_code` bits per literal.
///
/// Codes start at `min_code + 1` bits and widen as the table fills. The table is cleared
/// with a clear code when it reaches 4096 entries.
fn lzw(indices: &[u8], min_code: u8) -> Vec<u8> {
    let min = u32::from(min_code);
    let clear = 1u16 << min_code;
    let eoi = clear + 1;
    let mut out = Vec::new();
    let mut bits = Bits::default();
    let mut dict: HashMap<u32, u16> = HashMap::new();
    let mut width = min + 1;
    let mut next = eoi + 1;
    bits.put(&mut out, clear, width);
    let mut prefix: Option<u16> = None;
    for &b in indices {
        let p = match prefix {
            None => {
                prefix = Some(u16::from(b));
                continue;
            }
            Some(p) => p,
        };
        let key = (u32::from(p) << 8) | u32::from(b);
        if let Some(&code) = dict.get(&key) {
            prefix = Some(code);
            continue;
        }
        bits.put(&mut out, p, width);
        dict.insert(key, next);
        next += 1;
        if next == 4096 {
            bits.put(&mut out, clear, width);
            dict.clear();
            next = eoi + 1;
            width = min + 1;
        } else if u32::from(next) > (1 << width) {
            width += 1;
        }
        prefix = Some(u16::from(b));
    }
    if let Some(p) = prefix {
        bits.put(&mut out, p, width);
    }
    bits.put(&mut out, eoi, width);
    bits.flush(&mut out);
    out
}

/// Write LZW data as sub-blocks of at most 255 bytes, each prefixed by its length, closed by
/// the zero-length terminator.
fn put_sub_blocks(out: &mut Vec<u8>, data: &[u8]) {
    for chunk in data.chunks(255) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out.push(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{gif, AnimationFrame, Limits};

    fn encode_frames(w: u16, h: u16, opts: GifOptions, frames: &[(&[u8], u16)]) -> Vec<u8> {
        let mut enc = Encoder::new(w, h, opts).unwrap();
        for (px, delay) in frames {
            enc.push_rgba8(px, *delay).unwrap();
        }
        enc.finish().unwrap()
    }

    fn decode(data: &[u8]) -> Vec<AnimationFrame> {
        gif::decode_animation(data, Limits::default()).unwrap()
    }

    fn noise(seed: &mut u32) -> u8 {
        *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (*seed >> 24) as u8
    }

    /// A smooth gradient with per-channel noise: photographic enough to need quantising.
    fn photo(w: usize, h: usize) -> Vec<u8> {
        let mut seed = 7u32;
        let mut px = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / (w - 1)) as i32 + i32::from(noise(&mut seed) % 13) - 6;
                let g = (y * 255 / (h - 1)) as i32 + i32::from(noise(&mut seed) % 13) - 6;
                let b = ((x + y) * 127 / (w + h - 2)) as i32 + i32::from(noise(&mut seed) % 13) - 6;
                px.extend_from_slice(&[
                    r.clamp(0, 255) as u8,
                    g.clamp(0, 255) as u8,
                    b.clamp(0, 255) as u8,
                    255,
                ]);
            }
        }
        px
    }

    fn mean_abs_error(decoded: &[u8], source: &[u8]) -> f64 {
        let sum: u64 = decoded
            .iter()
            .zip(source)
            .enumerate()
            .filter(|(i, _)| i % 4 != 3)
            .map(|(_, (a, b))| u64::from(a.abs_diff(*b)))
            .sum();
        sum as f64 / (decoded.len() / 4 * 3) as f64
    }

    #[test]
    fn a_palette_that_fits_is_reproduced_exactly() {
        let px: Vec<u8> = (0..256)
            .flat_map(|i: usize| {
                let (x, y) = (i % 16, i / 16);
                [(x * 17) as u8, (y * 17) as u8, ((x ^ y) * 5) as u8, 255]
            })
            .collect();
        let gif = encode_frames(16, 16, GifOptions::default(), &[(&px, 4)]);
        let frames = decode(&gif);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].image.to_rgba8(), px);
    }

    #[test]
    fn a_changing_sequence_decodes_frame_for_frame() {
        let (w, h) = (32usize, 32usize);
        let a: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let c = [[200, 30, 30], [30, 200, 30], [30, 30, 200], [220, 220, 40]][(x / 8 + y / 8) % 4];
                [c[0], c[1], c[2], 255]
            })
            .collect();
        let mut b = a.clone();
        for y in 10..14 {
            for x in 10..14 {
                b[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[9, 9, 9, 255]);
            }
        }
        let mut d = b.clone();
        d[0..4].copy_from_slice(&[250, 0, 250, 255]);
        let gif = encode_frames(
            32,
            32,
            GifOptions::default(),
            // C is B again, so it merges into B's delay: 7 + 9 = 16.
            &[(&a, 5), (&b, 7), (&b, 9), (&d, 3)],
        );
        let frames = decode(&gif);
        let delays: Vec<u32> = frames.iter().map(|f| f.delay_num).collect();
        assert_eq!(delays, vec![5, 16, 3]);
        assert_eq!(frames[0].image.to_rgba8(), a);
        assert_eq!(frames[1].image.to_rgba8(), b);
        assert_eq!(frames[2].image.to_rgba8(), d);
    }

    #[test]
    fn the_loop_extension_is_written_only_when_asked() {
        let px = [1u8, 2, 3, 255];
        let play_once = encode_frames(1, 1, GifOptions::default(), &[(&px, 1)]);
        assert!(!play_once.windows(11).any(|w| w == b"NETSCAPE2.0"));

        for (loops, expect) in [(0u16, [0u8, 0]), (3, [3, 0])] {
            let opts = GifOptions {
                loop_count: Some(loops),
                ..GifOptions::default()
            };
            let gif = encode_frames(1, 1, opts, &[(&px, 1)]);
            let at = gif
                .windows(11)
                .position(|w| w == b"NETSCAPE2.0")
                .expect("NETSCAPE2.0 extension");
            let body = &gif[at + 11..];
            assert_eq!(&body[..2], &[0x03, 0x01]);
            assert_eq!(&body[2..4], &expect);
        }
    }

    #[test]
    fn transparent_pixels_stay_transparent_and_unchanged_ones_keep_the_canvas() {
        let (w, h) = (8u16, 4u16);
        let mut f1 = Vec::new();
        for _y in 0..4 {
            for x in 0..8 {
                if x < 4 {
                    f1.extend_from_slice(&[9, 9, 9, 0]);
                } else {
                    f1.extend_from_slice(&[200, 30 + x as u8, 30, 255]);
                }
            }
        }
        let mut f2 = f1.clone();
        f2[4 * 4..4 * 4 + 4].copy_from_slice(&[10, 200, 10, 255]);
        let gif = encode_frames(w, h, GifOptions::default(), &[(&f1, 5), (&f2, 5)]);
        let frames = decode(&gif);
        assert_eq!(frames.len(), 2);
        let (d1, d2) = (frames[0].image.to_rgba8(), frames[1].image.to_rgba8());
        for (i, px) in d1.chunks(4).enumerate() {
            let x = i % 8;
            if x < 4 {
                assert_eq!(px[3], 0, "pixel {i} should be transparent");
            } else {
                assert_eq!(px, &f1[i * 4..i * 4 + 4], "pixel {i}");
            }
        }
        for (i, px) in d2.chunks(4).enumerate() {
            let x = i % 8;
            if x < 4 {
                assert_eq!(px[3], 0, "pixel {i} stays transparent");
            } else {
                assert_eq!(px, &f2[i * 4..i * 4 + 4], "pixel {i}");
            }
        }
    }

    #[test]
    fn photographic_content_stays_within_a_small_error() {
        let (w, h) = (96usize, 96usize);
        let src = photo(w, h);
        for dither in [false, true] {
            let opts = GifOptions {
                dither,
                ..GifOptions::default()
            };
            let gif = encode_frames(96, 96, opts, &[(&src, 10)]);
            let frames = decode(&gif);
            let err = mean_abs_error(&frames[0].image.to_rgba8(), &src);
            assert!(err < 6.0, "dither={dither}: mean abs error {err:.2}");
        }
    }

    #[test]
    fn a_static_frame_repeated_ten_times_costs_little_more_than_one() {
        let (w, h) = (64usize, 64usize);
        let px = photo(w, h);
        let one = encode_frames(64, 64, GifOptions::default(), &[(&px, 10)]);
        let frames: Vec<(&[u8], u16)> = (0..10).map(|_| (&px[..], 10)).collect();
        let ten = encode_frames(64, 64, GifOptions::default(), &frames);
        assert!(
            (ten.len() as f64) < 1.5 * one.len() as f64,
            "ten frames {} bytes vs one frame {} bytes",
            ten.len(),
            one.len()
        );
        // The ten identical frames fold into one, carrying the summed delay.
        let decoded = decode(&ten);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].delay_num, 100);
    }

    #[test]
    fn bad_arguments_are_refused() {
        assert!(Encoder::new(0, 4, GifOptions::default()).is_err());
        assert!(Encoder::new(4, 0, GifOptions::default()).is_err());
        for max_colors in [1u16, 257] {
            let opts = GifOptions {
                max_colors,
                ..GifOptions::default()
            };
            assert!(Encoder::new(4, 4, opts).is_err(), "max_colors {max_colors}");
        }
        let mut enc = Encoder::new(2, 2, GifOptions::default()).unwrap();
        assert!(enc.push_rgba8(&[0; 15], 1).is_err());
        assert!(enc.push_rgba8(&[0; 17], 1).is_err());
        assert!(Encoder::new(2, 2, GifOptions::default()).unwrap().finish().is_err());
    }

    #[test]
    fn ffprobe_reads_the_output_when_it_is_installed() {
        let probe = std::process::Command::new("ffprobe").arg("-version").output();
        if probe.is_err() {
            return;
        }
        let px: Vec<u8> = (0..8 * 4).flat_map(|i: u8| [i * 8, 128, 255 - i * 8, 255]).collect();
        let gif = encode_frames(8, 4, GifOptions::default(), &[(&px, 5), (&px, 5)]);
        let path = std::env::temp_dir().join(format!("ec-gif-encode-{}.gif", std::process::id()));
        std::fs::write(&path, &gif).unwrap();
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
            .args(["stream=width,height", "-of", "csv=p=0"])
            .arg(&path)
            .output();
        let _ = std::fs::remove_file(&path);
        let out = out.expect("ffprobe runs");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "8,4");
    }
}
