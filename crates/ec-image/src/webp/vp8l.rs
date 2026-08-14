//! VP8L: the WebP lossless bitstream.
//!
//! Prefix-coded ARGB literals, LZ77 backward references over a
//! two-dimensionally mapped distance space, a hash-addressed colour cache, and
//! four reversible transforms (predictor, colour, subtract-green, colour
//! indexing) applied in the reverse of the order they were read.
//!
//! Decoding is pixel-exact by construction: every step is integer and the
//! format is lossless, so a differential test against any other implementation
//! either matches every sample or has found a bug.

use ec_core::{Error, Result};

/// Distance codes 1..=120 name a pixel in the neighbourhood above/left of the
/// current one, as `(x, y)` offsets, straight from the specification's table.
const DISTANCE_MAP: [(i32, i32); 120] = [
    (0, 1),
    (1, 0),
    (1, 1),
    (-1, 1),
    (0, 2),
    (2, 0),
    (1, 2),
    (-1, 2),
    (2, 1),
    (-2, 1),
    (2, 2),
    (-2, 2),
    (0, 3),
    (3, 0),
    (1, 3),
    (-1, 3),
    (3, 1),
    (-3, 1),
    (2, 3),
    (-2, 3),
    (3, 2),
    (-3, 2),
    (0, 4),
    (4, 0),
    (1, 4),
    (-1, 4),
    (4, 1),
    (-4, 1),
    (3, 3),
    (-3, 3),
    (2, 4),
    (-2, 4),
    (4, 2),
    (-4, 2),
    (0, 5),
    (3, 4),
    (-3, 4),
    (4, 3),
    (-4, 3),
    (5, 0),
    (1, 5),
    (-1, 5),
    (5, 1),
    (-5, 1),
    (2, 5),
    (-2, 5),
    (5, 2),
    (-5, 2),
    (4, 4),
    (-4, 4),
    (3, 5),
    (-3, 5),
    (5, 3),
    (-5, 3),
    (0, 6),
    (6, 0),
    (1, 6),
    (-1, 6),
    (6, 1),
    (-6, 1),
    (2, 6),
    (-2, 6),
    (6, 2),
    (-6, 2),
    (4, 5),
    (-4, 5),
    (5, 4),
    (-5, 4),
    (3, 6),
    (-3, 6),
    (6, 3),
    (-6, 3),
    (0, 7),
    (7, 0),
    (1, 7),
    (-1, 7),
    (5, 5),
    (-5, 5),
    (7, 1),
    (-7, 1),
    (4, 6),
    (-4, 6),
    (6, 4),
    (-6, 4),
    (2, 7),
    (-2, 7),
    (7, 2),
    (-7, 2),
    (3, 7),
    (-3, 7),
    (7, 3),
    (-7, 3),
    (5, 6),
    (-5, 6),
    (6, 5),
    (-6, 5),
    (8, 0),
    (4, 7),
    (-4, 7),
    (7, 4),
    (-7, 4),
    (8, 1),
    (8, 2),
    (6, 6),
    (-6, 6),
    (8, 3),
    (5, 7),
    (-5, 7),
    (7, 5),
    (-7, 5),
    (8, 4),
    (6, 7),
    (-6, 7),
    (7, 6),
    (-7, 6),
    (8, 5),
    (7, 7),
    (-7, 7),
    (8, 6),
    (8, 7),
];

/// The order code-length code lengths arrive in.
const CODE_LENGTH_ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

const GREEN_EXTRA: usize = 24;
const MAX_ALLOWED_CODE_LENGTH: usize = 15;

/// Least-significant-bit-first bit reader, as VP8L reads its stream.
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Bit position from the start of `data`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    /// A reader over `data`, starting at its first bit.
    pub fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, pos: 0 }
    }

    /// Read `n` bits (`n <= 24`), low bit first.
    ///
    /// Past the end of the data the reader yields zeros and records that it
    /// ran out; the decode loop reports the truncation rather than panicking
    /// on a short buffer.
    pub fn read(&mut self, n: u32) -> u32 {
        let mut value = 0u32;
        for i in 0..n {
            let byte = self.data.get(self.pos >> 3).copied().unwrap_or(0);
            let bit = (byte >> (self.pos & 7)) & 1;
            value |= u32::from(bit) << i;
            self.pos += 1;
        }
        value
    }

    /// True once more bits have been read than the buffer holds.
    pub fn exhausted(&self) -> bool {
        self.pos > self.data.len() * 8
    }
}

/// A canonical prefix code, decoded bit by bit.
struct Huffman {
    /// Number of codes of each length, 1..=15.
    counts: [u16; MAX_ALLOWED_CODE_LENGTH + 1],
    /// Symbols ordered by (length, symbol).
    symbols: Vec<u16>,
    /// The one symbol, when the code is a single leaf that consumes no bits.
    single: Option<u16>,
}

impl Huffman {
    fn from_lengths(lengths: &[u8]) -> Result<Huffman> {
        let mut counts = [0u16; MAX_ALLOWED_CODE_LENGTH + 1];
        let mut used = 0;
        let mut last = 0u16;
        for (symbol, &len) in lengths.iter().enumerate() {
            if len as usize > MAX_ALLOWED_CODE_LENGTH {
                return Err(Error::corrupt(format!("VP8L: code length {len}")));
            }
            if len > 0 {
                counts[len as usize] += 1;
                used += 1;
                last = symbol as u16;
            }
        }
        if used == 0 {
            return Err(Error::corrupt("VP8L: prefix code with no symbols"));
        }
        if used == 1 {
            return Ok(Huffman {
                counts,
                symbols: vec![last],
                single: Some(last),
            });
        }
        // Kraft check: an over- or under-subscribed code is a corrupt stream,
        // and an under-subscribed one would otherwise decode into a hole.
        let mut left = 1i32;
        for len in 1..=MAX_ALLOWED_CODE_LENGTH {
            left <<= 1;
            left -= i32::from(counts[len]);
            if left < 0 {
                return Err(Error::corrupt("VP8L: over-subscribed prefix code"));
            }
        }
        if left != 0 {
            return Err(Error::corrupt("VP8L: incomplete prefix code"));
        }
        // Symbols ordered by code length, then by symbol: exactly the order
        // `decode` walks them in.
        let mut offsets = [0usize; MAX_ALLOWED_CODE_LENGTH + 1];
        let mut total = 0usize;
        for len in 1..=MAX_ALLOWED_CODE_LENGTH {
            offsets[len] = total;
            total += usize::from(counts[len]);
        }
        let mut symbols = vec![0u16; used];
        for (symbol, &len) in lengths.iter().enumerate() {
            if len > 0 {
                symbols[offsets[len as usize]] = symbol as u16;
                offsets[len as usize] += 1;
            }
        }
        Ok(Huffman {
            counts,
            symbols,
            single: None,
        })
    }

    fn decode(&self, bits: &mut BitReader<'_>) -> Result<u16> {
        if let Some(symbol) = self.single {
            return Ok(symbol);
        }
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..=MAX_ALLOWED_CODE_LENGTH {
            code |= bits.read(1) as i32;
            let count = i32::from(self.counts[len]);
            if code - first < count {
                let at = (index + code - first) as usize;
                return self
                    .symbols
                    .get(at)
                    .copied()
                    .ok_or_else(|| Error::corrupt("VP8L: symbol outside its code"));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::corrupt("VP8L: no prefix code matches the bits"))
    }
}

/// One prefix code group: green+length+cache, red, blue, alpha, distance.
struct Group {
    green: Huffman,
    red: Huffman,
    blue: Huffman,
    alpha: Huffman,
    distance: Huffman,
}

/// A decoded lossless image: packed ARGB, one `u32` per pixel.
pub struct Argb {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `a << 24 | r << 16 | g << 8 | b`, scan-line order.
    pub pixels: Vec<u32>,
}

/// Decode a complete VP8L chunk, header and all.
pub fn decode(data: &[u8], limits: crate::Limits) -> Result<Argb> {
    let mut bits = BitReader::new(data);
    if bits.read(8) != 0x2f {
        return Err(Error::corrupt("VP8L: missing 0x2f signature"));
    }
    let width = bits.read(14) + 1;
    let height = bits.read(14) + 1;
    let _alpha_is_used = bits.read(1);
    let version = bits.read(3);
    if version != 0 {
        return Err(Error::corrupt(format!("VP8L: version {version}")));
    }
    limits.check(width, height)?;
    let pixels = decode_stream(&mut bits, width, height, true)?;
    Ok(Argb {
        width,
        height,
        pixels,
    })
}

/// Decode a headerless VP8L stream of known dimensions — how an `ALPH` chunk
/// carries a lossless alpha plane, and how a colour table is stored.
pub fn decode_implicit(data: &[u8], width: u32, height: u32) -> Result<Vec<u32>> {
    let mut bits = BitReader::new(data);
    decode_stream(&mut bits, width, height, true)
}

/// The image stream proper: transforms (when allowed), then entropy-coded data.
fn decode_stream(
    bits: &mut BitReader<'_>,
    width: u32,
    height: u32,
    transforms_allowed: bool,
) -> Result<Vec<u32>> {
    let mut transforms: Vec<Transform> = Vec::new();
    let mut work_width = width;
    if transforms_allowed {
        let mut seen = [false; 4];
        while bits.read(1) == 1 {
            let kind = bits.read(2) as usize;
            if seen[kind] {
                return Err(Error::corrupt("VP8L: transform used twice"));
            }
            seen[kind] = true;
            let transform = read_transform(bits, kind, work_width, height)?;
            if let Transform::ColorIndexing { width_bits, .. } = &transform {
                work_width = work_width.div_ceil(1 << width_bits);
            }
            transforms.push(transform);
        }
    }

    let mut pixels = decode_entropy_coded(bits, work_width, height, true)?;
    // Applied last-read-first, which is the inverse of the encoder's order.
    for transform in transforms.iter().rev() {
        pixels = apply_transform(transform, pixels, &mut work_width, height)?;
    }
    if work_width != width {
        return Err(Error::corrupt("VP8L: transforms left the wrong width"));
    }
    Ok(pixels)
}

/// A transform, with the data needed to invert it.
enum Transform {
    Predictor { bits: u32, image: Vec<u32>, tw: u32 },
    Color { bits: u32, image: Vec<u32>, tw: u32 },
    SubtractGreen,
    ColorIndexing { width_bits: u32, table: Vec<u32> },
}

fn read_transform(
    bits: &mut BitReader<'_>,
    kind: usize,
    width: u32,
    height: u32,
) -> Result<Transform> {
    match kind {
        0 | 1 => {
            let size_bits = bits.read(3) + 2;
            let tw = width.div_ceil(1 << size_bits);
            let th = height.div_ceil(1 << size_bits);
            let image = decode_entropy_coded(bits, tw, th, false)?;
            Ok(if kind == 0 {
                Transform::Predictor {
                    bits: size_bits,
                    image,
                    tw,
                }
            } else {
                Transform::Color {
                    bits: size_bits,
                    image,
                    tw,
                }
            })
        }
        2 => Ok(Transform::SubtractGreen),
        _ => {
            let size = bits.read(8) + 1;
            let mut table = decode_entropy_coded(bits, size, 1, false)?;
            // The table is stored as deltas between successive colours.
            for i in 1..table.len() {
                table[i] = add_argb(table[i], table[i - 1]);
            }
            let width_bits = match size {
                0..=2 => 3,
                3..=4 => 2,
                5..=16 => 1,
                _ => 0,
            };
            Ok(Transform::ColorIndexing { width_bits, table })
        }
    }
}

fn apply_transform(
    transform: &Transform,
    pixels: Vec<u32>,
    width: &mut u32,
    height: u32,
) -> Result<Vec<u32>> {
    Ok(match transform {
        Transform::SubtractGreen => pixels
            .into_iter()
            .map(|p| {
                let g = (p >> 8) & 0xff;
                let r = ((p >> 16) + g) & 0xff;
                let b = (p + g) & 0xff;
                (p & 0xff00_ff00) | (r << 16) | b
            })
            .collect(),
        Transform::Color { bits, image, tw } => {
            let mut out = pixels;
            for y in 0..height {
                for x in 0..*width {
                    let block = (y >> bits) * tw + (x >> bits);
                    let element = image.get(block as usize).copied().unwrap_or(0);
                    let index = (y * *width + x) as usize;
                    out[index] = inverse_color(out[index], element);
                }
            }
            out
        }
        Transform::Predictor { bits, image, tw } => {
            inverse_predictor(pixels, *width, height, *bits, image, *tw)
        }
        Transform::ColorIndexing { width_bits, table } => {
            let out_width = *width << width_bits;
            let mut out = vec![0u32; (out_width as usize) * (height as usize)];
            let per_pixel = 1u32 << width_bits;
            let bits_each = 8 >> width_bits;
            let mask = (1u32 << bits_each) - 1;
            for y in 0..height {
                for x in 0..*width {
                    let packed = (pixels[(y * *width + x) as usize] >> 8) & 0xff;
                    for sub in 0..per_pixel {
                        let ox = x * per_pixel + sub;
                        if ox >= out_width {
                            break;
                        }
                        let index = if *width_bits == 0 {
                            packed
                        } else {
                            (packed >> (bits_each * sub)) & mask
                        };
                        out[(y * out_width + ox) as usize] =
                            table.get(index as usize).copied().unwrap_or(0);
                    }
                }
            }
            *width = out_width;
            out
        }
    })
}

fn add_argb(a: u32, b: u32) -> u32 {
    let mut out = 0u32;
    for shift in [0, 8, 16, 24] {
        let sum = (((a >> shift) & 0xff) + ((b >> shift) & 0xff)) & 0xff;
        out |= sum << shift;
    }
    out
}

fn inverse_color(pixel: u32, element: u32) -> u32 {
    // The element carries green_to_red in blue, green_to_blue in green and
    // red_to_blue in red, as the specification lays it out.
    let green_to_red = (element & 0xff) as i8;
    let green_to_blue = ((element >> 8) & 0xff) as i8;
    let red_to_blue = ((element >> 16) & 0xff) as i8;
    let green = ((pixel >> 8) & 0xff) as u8 as i8;
    let mut red = ((pixel >> 16) & 0xff) as i32;
    let mut blue = (pixel & 0xff) as i32;
    red += delta(green_to_red, green);
    blue += delta(green_to_blue, green);
    blue += delta(red_to_blue, (red & 0xff) as u8 as i8);
    (pixel & 0xff00_ff00) | (((red & 0xff) as u32) << 16) | ((blue & 0xff) as u32)
}

/// `(t * c) >> 5` in the specification's 3.5 fixed point.
fn delta(t: i8, c: i8) -> i32 {
    (i32::from(t) * i32::from(c)) >> 5
}

fn inverse_predictor(
    mut pixels: Vec<u32>,
    width: u32,
    height: u32,
    size_bits: u32,
    image: &[u32],
    tw: u32,
) -> Vec<u32> {
    let w = width as usize;
    for y in 0..height as usize {
        for x in 0..w {
            let index = y * w + x;
            let pred = if x == 0 && y == 0 {
                0xff00_0000
            } else if y == 0 {
                pixels[index - 1]
            } else if x == 0 {
                pixels[index - w]
            } else {
                let block = ((y as u32) >> size_bits) * tw + ((x as u32) >> size_bits);
                let mode = image.get(block as usize).map_or(0, |p| (p >> 8) & 0xff);
                let left = pixels[index - 1];
                let top = pixels[index - w];
                let top_left = pixels[index - w - 1];
                // The rightmost column has no top-right; the leftmost pixel of
                // the same row stands in, as the specification requires.
                let top_right = if x + 1 == w {
                    pixels[y * w]
                } else {
                    pixels[index - w + 1]
                };
                predict(mode, left, top, top_left, top_right)
            };
            pixels[index] = add_argb(pixels[index], pred);
        }
    }
    pixels
}

fn predict(mode: u32, l: u32, t: u32, tl: u32, tr: u32) -> u32 {
    match mode {
        0 => 0xff00_0000,
        1 => l,
        2 => t,
        3 => tr,
        4 => tl,
        5 => average2(average2(l, tr), t),
        6 => average2(l, tl),
        7 => average2(l, t),
        8 => average2(tl, t),
        9 => average2(t, tr),
        10 => average2(average2(l, tl), average2(t, tr)),
        11 => select(l, t, tl),
        12 => clamp_add_subtract_full(l, t, tl),
        13 => clamp_add_subtract_half(average2(l, t), tl),
        // Modes past 13 do not exist; a corrupt predictor image gets black
        // rather than an out-of-range panic.
        _ => 0xff00_0000,
    }
}

fn average2(a: u32, b: u32) -> u32 {
    let mut out = 0u32;
    for shift in [0, 8, 16, 24] {
        let v = (((a >> shift) & 0xff) + ((b >> shift) & 0xff)) / 2;
        out |= v << shift;
    }
    out
}

fn select(l: u32, t: u32, tl: u32) -> u32 {
    let mut pl = 0i32;
    let mut pt = 0i32;
    for shift in [0, 8, 16, 24] {
        let (a, b, c) = (
            ((l >> shift) & 0xff) as i32,
            ((t >> shift) & 0xff) as i32,
            ((tl >> shift) & 0xff) as i32,
        );
        let p = a + b - c;
        pl += (p - a).abs();
        pt += (p - b).abs();
    }
    if pl < pt { l } else { t }
}

fn clamp_add_subtract_full(a: u32, b: u32, c: u32) -> u32 {
    let mut out = 0u32;
    for shift in [0, 8, 16, 24] {
        let v = (((a >> shift) & 0xff) as i32 + ((b >> shift) & 0xff) as i32
            - ((c >> shift) & 0xff) as i32)
            .clamp(0, 255) as u32;
        out |= v << shift;
    }
    out
}

fn clamp_add_subtract_half(a: u32, b: u32) -> u32 {
    let mut out = 0u32;
    for shift in [0, 8, 16, 24] {
        let (x, y) = (((a >> shift) & 0xff) as i32, ((b >> shift) & 0xff) as i32);
        let v = (x + (x - y) / 2).clamp(0, 255) as u32;
        out |= v << shift;
    }
    out
}

/// Read a prefix code: either the two-symbol shorthand or full code lengths.
fn read_code(bits: &mut BitReader<'_>, alphabet: usize) -> Result<Huffman> {
    let mut lengths = vec![0u8; alphabet];
    if bits.read(1) == 1 {
        // Simple code: one or two symbols, each one bit long.
        let count = bits.read(1) + 1;
        let first_is_byte = bits.read(1) == 1;
        let symbol0 = bits.read(if first_is_byte { 8 } else { 1 }) as usize;
        if symbol0 >= alphabet {
            return Err(Error::corrupt("VP8L: simple code symbol past the alphabet"));
        }
        lengths[symbol0] = 1;
        if count == 2 {
            let symbol1 = bits.read(8) as usize;
            if symbol1 >= alphabet {
                return Err(Error::corrupt("VP8L: simple code symbol past the alphabet"));
            }
            lengths[symbol1] = 1;
        }
        // One symbol of length 1 is a legal single-leaf code, so build from
        // the lengths rather than insisting the Kraft sum be exactly one.
        return Huffman::from_lengths(&lengths);
    }

    let num_lengths = 4 + bits.read(4) as usize;
    let mut code_length_lengths = [0u8; 19];
    for i in 0..num_lengths {
        code_length_lengths[CODE_LENGTH_ORDER[i]] = bits.read(3) as u8;
    }
    let code_length_code = Huffman::from_lengths(&code_length_lengths)?;

    let mut max_symbol = if bits.read(1) == 1 {
        let length_bits = 2 + 2 * bits.read(3);
        let max = 2 + bits.read(length_bits) as usize;
        if max > alphabet {
            return Err(Error::corrupt("VP8L: max_symbol past the alphabet"));
        }
        max
    } else {
        alphabet
    };

    let mut previous = 8u8;
    let mut index = 0usize;
    while index < alphabet {
        if max_symbol == 0 {
            break;
        }
        max_symbol -= 1;
        if bits.exhausted() {
            return Err(Error::corrupt("VP8L: truncated code lengths"));
        }
        let symbol = code_length_code.decode(bits)?;
        match symbol {
            0..=15 => {
                lengths[index] = symbol as u8;
                index += 1;
                if symbol != 0 {
                    previous = symbol as u8;
                }
            }
            16 => {
                let repeat = 3 + bits.read(2) as usize;
                for _ in 0..repeat.min(alphabet - index) {
                    lengths[index] = previous;
                    index += 1;
                }
            }
            17 => index += (3 + bits.read(3) as usize).min(alphabet - index),
            18 => index += (11 + bits.read(7) as usize).min(alphabet - index),
            other => return Err(Error::corrupt(format!("VP8L: code length symbol {other}"))),
        }
    }
    Huffman::from_lengths(&lengths)
}

/// Read the five prefix codes of one group.
fn read_group(bits: &mut BitReader<'_>, cache_bits: u32) -> Result<Group> {
    let green_alphabet = 256 + GREEN_EXTRA + if cache_bits > 0 { 1 << cache_bits } else { 0 };
    Ok(Group {
        green: read_code(bits, green_alphabet)?,
        red: read_code(bits, 256)?,
        blue: read_code(bits, 256)?,
        alpha: read_code(bits, 256)?,
        distance: read_code(bits, 40)?,
    })
}

/// Length or distance from a prefix code plus its extra bits.
fn prefix_value(bits: &mut BitReader<'_>, code: u32) -> u32 {
    if code < 4 {
        return code + 1;
    }
    let extra = (code - 2) >> 1;
    let offset = (2 + (code & 1)) << extra;
    offset + bits.read(extra) + 1
}

/// Decode an entropy-coded image: the ARGB image itself, or one of the
/// sub-resolution images a transform or the meta prefix uses.
fn decode_entropy_coded(
    bits: &mut BitReader<'_>,
    width: u32,
    height: u32,
    meta_allowed: bool,
) -> Result<Vec<u32>> {
    let count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| Error::corrupt("VP8L: image geometry overflows"))?;

    // Order matters and is easy to get backwards: the colour cache size comes
    // first, then the meta prefix codes, then the prefix codes themselves.
    let cache_bits = if bits.read(1) == 1 {
        let n = bits.read(4);
        if !(1..=11).contains(&n) {
            return Err(Error::corrupt(format!("VP8L: colour cache bits {n}")));
        }
        n
    } else {
        0
    };

    let mut entropy_image: Vec<u32> = Vec::new();
    let mut prefix_bits = 0u32;
    let mut prefix_width = 0u32;
    let mut groups = 1usize;
    if meta_allowed && bits.read(1) == 1 {
        prefix_bits = bits.read(3) + 2;
        prefix_width = width.div_ceil(1 << prefix_bits);
        let prefix_height = height.div_ceil(1 << prefix_bits);
        entropy_image = decode_entropy_coded(bits, prefix_width, prefix_height, false)?;
        groups = entropy_image
            .iter()
            .map(|p| (((p >> 8) & 0xffff) as usize) + 1)
            .max()
            .unwrap_or(1);
        if groups > 1 << 16 {
            return Err(Error::corrupt("VP8L: too many prefix code groups"));
        }
    }

    let mut code_groups = Vec::with_capacity(groups);
    for _ in 0..groups {
        code_groups.push(read_group(bits, cache_bits)?);
        if bits.exhausted() {
            return Err(Error::corrupt("VP8L: truncated prefix codes"));
        }
    }

    let mut cache = vec![0u32; if cache_bits > 0 { 1 << cache_bits } else { 0 }];
    let mut pixels = vec![0u32; count];
    let mut at = 0usize;
    while at < count {
        if bits.exhausted() {
            return Err(Error::corrupt("VP8L: truncated image data"));
        }
        let group = if entropy_image.is_empty() {
            &code_groups[0]
        } else {
            let x = (at % width as usize) as u32;
            let y = (at / width as usize) as u32;
            let block = (y >> prefix_bits) * prefix_width + (x >> prefix_bits);
            let meta = entropy_image
                .get(block as usize)
                .map_or(0, |p| ((p >> 8) & 0xffff) as usize);
            code_groups
                .get(meta)
                .ok_or_else(|| Error::corrupt("VP8L: meta prefix code past its group list"))?
        };
        let symbol = u32::from(group.green.decode(bits)?);
        if symbol < 256 {
            let red = u32::from(group.red.decode(bits)?);
            let blue = u32::from(group.blue.decode(bits)?);
            let alpha = u32::from(group.alpha.decode(bits)?);
            let pixel = (alpha << 24) | (red << 16) | (symbol << 8) | blue;
            pixels[at] = pixel;
            if cache_bits > 0 {
                cache[cache_index(pixel, cache_bits)] = pixel;
            }
            at += 1;
        } else if symbol < 256 + GREEN_EXTRA as u32 {
            let length = prefix_value(bits, symbol - 256) as usize;
            let distance_code = u32::from(group.distance.decode(bits)?);
            let distance = prefix_value(bits, distance_code) as usize;
            let distance = map_distance(distance, width);
            if distance > at || length > count - at {
                return Err(Error::corrupt("VP8L: backward reference outside the image"));
            }
            for i in 0..length {
                let pixel = pixels[at - distance + i];
                pixels[at + i] = pixel;
                if cache_bits > 0 {
                    cache[cache_index(pixel, cache_bits)] = pixel;
                }
            }
            at += length;
        } else {
            let index = (symbol - 256 - GREEN_EXTRA as u32) as usize;
            let pixel = *cache
                .get(index)
                .ok_or_else(|| Error::corrupt("VP8L: colour cache index out of range"))?;
            pixels[at] = pixel;
            at += 1;
        }
    }
    Ok(pixels)
}

fn cache_index(pixel: u32, cache_bits: u32) -> usize {
    ((0x1e35_a7bd_u32.wrapping_mul(pixel)) >> (32 - cache_bits)) as usize
}

/// Distance codes 1..=120 name a nearby pixel; anything larger is a plain
/// scan-line distance offset by 120.
fn map_distance(code: usize, width: u32) -> usize {
    if code > 120 {
        return code - 120;
    }
    let (xi, yi) = DISTANCE_MAP[code - 1];
    let distance = yi * width as i32 + xi;
    distance.max(1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_come_out_low_bit_first() {
        let mut bits = BitReader::new(&[0b1010_0110, 0xff]);
        assert_eq!(bits.read(1), 0);
        assert_eq!(bits.read(2), 0b11);
        assert_eq!(bits.read(5), 0b1_0100);
        assert_eq!(bits.read(8), 0xff);
        assert!(!bits.exhausted());
        bits.read(1);
        assert!(bits.exhausted());
    }

    #[test]
    fn a_canonical_code_decodes_its_own_symbols() {
        // Lengths 1,2,3,3 -> codes 0, 10, 110, 111.
        let code = Huffman::from_lengths(&[1, 2, 3, 3]).unwrap();
        let mut bits = BitReader::new(&[0b1101_1100, 0b0000_0001]);
        // Bits low-first: 0,0,1,1,1,0,1,1 -> symbol 0, then 110 -> 2 ...
        assert_eq!(code.decode(&mut bits).unwrap(), 0);
        assert_eq!(code.decode(&mut bits).unwrap(), 0);
        assert_eq!(code.decode(&mut bits).unwrap(), 3);
    }

    #[test]
    fn an_incomplete_code_is_refused() {
        assert!(Huffman::from_lengths(&[1, 2, 3]).is_err());
        assert!(Huffman::from_lengths(&[1, 1, 1, 1]).is_err());
        assert!(Huffman::from_lengths(&[0, 0]).is_err());
        // A single leaf is legal and consumes no bits.
        let single = Huffman::from_lengths(&[0, 1, 0]).unwrap();
        let mut bits = BitReader::new(&[]);
        assert_eq!(single.decode(&mut bits).unwrap(), 1);
    }

    #[test]
    fn distance_codes_map_into_the_neighbourhood() {
        // Code 1 is the pixel directly above; code 2 the one to the left.
        assert_eq!(map_distance(1, 16), 16);
        assert_eq!(map_distance(2, 16), 1);
        assert_eq!(map_distance(121, 16), 1);
        // Every mapped distance must be at least one pixel back.
        for code in 1..=120 {
            assert!(map_distance(code, 4) >= 1, "code {code}");
        }
    }

    #[test]
    fn the_predictors_agree_with_their_definitions() {
        let l = 0xff20_4060;
        let t = 0xff40_6080;
        let tl = 0xff10_2030;
        assert_eq!(predict(1, l, t, tl, 0), l);
        assert_eq!(predict(2, l, t, tl, 0), t);
        assert_eq!(predict(7, l, t, tl, 0), 0xff30_5070);
        assert_eq!(predict(12, l, t, tl, 0), clamp_add_subtract_full(l, t, tl));
        assert_eq!(predict(99, l, t, tl, 0), 0xff00_0000);
    }
}
