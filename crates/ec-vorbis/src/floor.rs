//! Floor decode and curve synthesis — the spectral envelope a residue rides on.
//!
//! Two floor types share nothing but their job. Floor 1 (§7.2) is a
//! piecewise-linear curve in a 0.54 dB quantised log domain, drawn with
//! integer Bresenham arithmetic so every decoder draws the same line. Floor 0
//! (§6.2) is an LSP filter response evaluated on a Bark-spaced map — only old
//! encoders emit it, and the Xiph `lsp-test` vectors are exactly those.

use std::sync::LazyLock;

use crate::bits::Bits;
use crate::codebook::{Codebook, ilog};
use crate::setup::{Floor0, Floor1};

/// §7.2.4's amplitude table, 256 entries of `10^((y - 255) * 140/256 / 20)`.
///
/// The spec prints the values; this states the law behind them, which is the
/// same table to within a float ulp (its first entry, `1.0649863e-07`, is
/// `10^(-255 * 0.02734375)` exactly). Generated rather than pasted so the
/// relationship to the 140 dB the floor spans stays visible.
static INVERSE_DB: LazyLock<[f32; 256]> = LazyLock::new(|| {
    let mut table = [0.0f32; 256];
    for (y, slot) in table.iter_mut().enumerate() {
        *slot = 10f64.powf((y as f64 - 255.0) * (140.0 / 256.0) / 20.0) as f32;
    }
    table
});

/// What a floor packet said, kept until the residue is in and the curve is
/// multiplied through.
pub enum FloorState {
    /// The channel codes no audio in this packet.
    Unused,
    /// Floor 1 amplitudes, in coding order, with their step-2 flags.
    One {
        /// Final Y value per coding-order index.
        y: Vec<i32>,
        /// Whether the value was coded rather than predicted.
        step2: Vec<bool>,
    },
    /// Floor 0 amplitude and LSP coefficients.
    Zero {
        /// Coded amplitude.
        amplitude: u32,
        /// LSP coefficients.
        coefficients: Vec<f32>,
    },
}

impl FloorState {
    /// True when the channel has no spectrum this packet.
    pub fn is_unused(&self) -> bool {
        matches!(self, FloorState::Unused)
    }
}

/// Decode a floor-1 packet header (§7.2.2 and §7.2.3), amplitudes only.
pub fn decode_floor1(floor: &Floor1, codebooks: &[Codebook], bits: &mut Bits) -> FloorState {
    if !bits.bit() || bits.eop() {
        return FloorState::Unused;
    }
    let range = [256i32, 128, 86, 64][(floor.multiplier - 1) as usize];
    let values = floor.x_list.len();
    let mut y = vec![0i32; values];
    let field = ilog((range - 1) as u32);
    y[0] = bits.read(field) as i32;
    y[1] = bits.read(field) as i32;

    let mut offset = 2usize;
    for &class in &floor.partition_classes {
        let dimensions = floor.class_dimensions[class];
        let subclass_bits = floor.class_subclasses[class];
        let mask = (1u32 << subclass_bits) - 1;
        let mut class_value = 0u32;
        if subclass_bits > 0 {
            let book = &codebooks[floor.class_masterbooks[class]];
            match book.decode_scalar(bits) {
                Some(value) => class_value = value,
                None => return FloorState::Unused,
            }
        }
        for _ in 0..dimensions {
            let book = floor.subclass_books[class][(class_value & mask) as usize];
            class_value >>= subclass_bits;
            if offset >= values {
                break;
            }
            y[offset] = match book {
                book if book >= 0 => match codebooks[book as usize].decode_scalar(bits) {
                    Some(value) => value as i32,
                    None => return FloorState::Unused,
                },
                _ => 0,
            };
            offset += 1;
        }
    }
    if bits.eop() {
        return FloorState::Unused;
    }

    // §7.2.3 amplitude synthesis: every value past the first two is a
    // correction to the line its two already-coded neighbours predict, folded
    // into the room that line leaves above and below.
    let mut final_y = y.clone();
    let mut step2 = vec![false; values];
    step2[0] = true;
    step2[1] = true;
    for i in 2..values {
        let (low, high) = floor.neighbours[i];
        let predicted = render_point(
            floor.x_list[low],
            final_y[low],
            floor.x_list[high],
            final_y[high],
            floor.x_list[i],
        );
        let val = y[i];
        let high_room = range - predicted;
        let low_room = predicted;
        let room = high_room.min(low_room) * 2;
        if val != 0 {
            step2[low] = true;
            step2[high] = true;
            step2[i] = true;
            final_y[i] = match val >= room {
                true => match high_room > low_room {
                    true => val - low_room + predicted,
                    false => predicted - val + high_room - 1,
                },
                false => match val & 1 == 1 {
                    true => predicted - (val + 1) / 2,
                    false => predicted + val / 2,
                },
            };
        } else {
            step2[i] = false;
            final_y[i] = predicted;
        }
        final_y[i] = final_y[i].clamp(0, range - 1);
    }
    FloorState::One { y: final_y, step2 }
}

/// Draw the floor-1 curve into `out` (§7.2.4), one value per coefficient.
pub fn render_floor1(floor: &Floor1, y: &[i32], step2: &[bool], out: &mut [f32]) {
    let n = out.len();
    let table = &*INVERSE_DB;
    let multiplier = floor.multiplier;
    let mut lx = 0usize;
    let mut ly = y[0] * multiplier;
    let mut hx = 0usize;
    let mut hy = ly;
    for &i in floor.sorted.iter().skip(1) {
        if !step2[i] {
            continue;
        }
        hy = y[i] * multiplier;
        hx = floor.x_list[i] as usize;
        render_line(lx, ly, hx, hy, out, table);
        lx = hx;
        ly = hy;
    }
    if hx < n {
        render_line(hx, hy, n, hy, out, table);
    }
}

/// §7.2.2 `render_point`: the height a line between two coded values has at `x`.
fn render_point(x0: u32, y0: i32, x1: u32, y1: i32, x: u32) -> i32 {
    let dy = y1 - y0;
    let adx = (x1 - x0) as i32;
    let ady = dy.abs();
    let off = (ady * (x - x0) as i32) / adx;
    match dy < 0 {
        true => y0 - off,
        false => y0 + off,
    }
}

/// §7.2.4 `render_line`: integer Bresenham, so two decoders never disagree by
/// one sample of tilt.
fn render_line(x0: usize, y0: i32, x1: usize, y1: i32, out: &mut [f32], table: &[f32; 256]) {
    let n = out.len();
    let dy = y1 - y0;
    let adx = (x1 - x0) as i32;
    if adx <= 0 {
        return;
    }
    let mut ady = dy.abs();
    let base = dy / adx;
    let sy = match dy < 0 {
        true => base - 1,
        false => base + 1,
    };
    ady -= base.abs() * adx;
    let mut y = y0;
    let mut err = 0i32;
    if x0 < n {
        out[x0] = table[y.clamp(0, 255) as usize];
    }
    let end = x1.min(n);
    for slot in out[(x0 + 1).min(end)..end].iter_mut() {
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        *slot = table[y.clamp(0, 255) as usize];
    }
}

/// Decode a floor-0 packet header (§6.2.2): an amplitude and an LSP vector.
pub fn decode_floor0(floor: &Floor0, codebooks: &[Codebook], bits: &mut Bits) -> FloorState {
    let amplitude = bits.read(floor.amplitude_bits);
    if bits.eop() {
        return FloorState::Unused;
    }
    if amplitude == 0 {
        return FloorState::Unused;
    }
    // §6.2.2 states the book number in `ilog(number_of_books)` bits — the count
    // itself, not the largest index, which is one bit wider than it looks.
    let index = bits.read(ilog(floor.books.len() as u32)) as usize;
    let Some(&book) = floor.books.get(index) else {
        return FloorState::Unused;
    };
    let book = &codebooks[book];
    if !book.has_values() {
        return FloorState::Unused;
    }
    let mut coefficients = Vec::with_capacity(floor.order);
    let mut last = 0.0f32;
    while coefficients.len() < floor.order {
        let Some(vector) = book.decode_vector(bits) else {
            return FloorState::Unused;
        };
        for &value in vector {
            coefficients.push(value + last);
        }
        last = *coefficients.last().unwrap_or(&0.0);
    }
    coefficients.truncate(floor.order);
    FloorState::Zero {
        amplitude,
        coefficients,
    }
}

/// Draw the floor-0 curve into `out` (§6.2.3): the LSP pair evaluated on a
/// Bark-spaced frequency map, one height per Bark bin.
pub fn render_floor0(floor: &Floor0, amplitude: u32, coefficients: &[f32], out: &mut [f32]) {
    let n = out.len();
    if n == 0 || coefficients.is_empty() {
        out.fill(0.0);
        return;
    }
    let order = coefficients.len();
    let bark_size = floor.bark_map_size as f64;
    let scale = bark_size / bark(0.5 * f64::from(floor.rate));
    let map: Vec<f64> = (0..n)
        .map(|i| {
            let hz = f64::from(floor.rate) * 0.5 * i as f64 / n as f64;
            (bark(hz) * scale).floor().clamp(0.0, bark_size - 1.0)
        })
        .collect();
    let amplitude_max = ((1u64 << floor.amplitude_bits) - 1) as f64;
    let offset = f64::from(floor.amplitude_offset);
    let cos_theta: Vec<f64> = coefficients.iter().map(|&c| f64::from(c).cos()).collect();

    let mut i = 0usize;
    while i < n {
        let bin = map[i];
        let omega = std::f64::consts::PI * bin / bark_size;
        let cos_omega = omega.cos();
        // P and Q are the two LSP polynomials on the unit circle; their
        // prefactors are the (1 +- z^-1) factors the odd and even orders differ
        // by, and each root contributes 4(cos(theta) - cos(omega))^2.
        let (mut p, mut q) = match order % 2 {
            1 => (1.0 - cos_omega * cos_omega, 0.25),
            _ => ((1.0 - cos_omega) * 0.5, (1.0 + cos_omega) * 0.5),
        };
        let mut j = 1usize;
        while j < order {
            let d = cos_theta[j] - cos_omega;
            p *= 4.0 * d * d;
            j += 2;
        }
        let mut j = 0usize;
        while j < order {
            let d = cos_theta[j] - cos_omega;
            q *= 4.0 * d * d;
            j += 2;
        }
        let value = (0.11512925
            * (f64::from(amplitude) * offset / (amplitude_max * (p + q).sqrt()) - offset))
            .exp() as f32;
        while i < n && map[i] == bin {
            out[i] = value;
            i += 1;
        }
    }
}

/// §6.2.3's Bark scale.
fn bark(x: f64) -> f64 {
    13.1 * (0.00074 * x).atan() + 2.24 * (0.0000000185 * x * x).atan() + 0.0001 * x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_db_table_matches_the_spec_endpoints() {
        let table = &*INVERSE_DB;
        assert!((table[0] - 1.0649863e-07).abs() < 1e-13, "{}", table[0]);
        assert!((table[255] - 1.0).abs() < 1e-6, "{}", table[255]);
        // Monotonic, which is what makes a bigger Y a louder floor.
        assert!(table.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn render_line_draws_the_spec_slope() {
        let mut out = vec![0.0f32; 8];
        let table = &*INVERSE_DB;
        render_line(0, 0, 8, 16, &mut out, table);
        // Sixteen steps over eight samples is two per sample, starting at zero.
        let expect: Vec<f32> = (0..8).map(|i| table[i * 2]).collect();
        assert_eq!(out, expect);
    }
}
