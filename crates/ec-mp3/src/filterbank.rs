//! The two transforms every Layer III sample passes through: the 36/12-point
//! IMDCT with its alias butterflies, and the 32-band polyphase bank.
//!
//! The polyphase matrixing is a 32-point DCT from `ec-dsp` in both directions —
//! DCT-II going out, DCT-III coming in — after folding the 64 (or 80) cosine
//! arguments onto the 32 the transform actually computes. That is what keeps
//! synthesis at 512 window multiplies and one short DCT per 32 output samples
//! instead of a 32x64 product.
//!
//! The IMDCT sizes (36 and 12) are not powers of two, so `ec-dsp`'s MDCT plan
//! does not apply. They are computed instead from the transform's own two
//! symmetries — `y[17-n] = -y[n]` and `y[35-m] = y[18+m]` — which halve a
//! fixed-size product that costs under 1 % of decode time either way; the
//! polyphase bank below is where the time actually goes.

use crate::tables::{alias_coefficients, windows};
use crate::window::WINDOW;
use ec_dsp::Dct;

/// Precomputed IMDCT bases, shared by the decoder and the encoder.
#[derive(Clone, Debug)]
pub(crate) struct Imdct {
    long: [[f32; 18]; 18],
    short: [[f32; 6]; 6],
}

impl Default for Imdct {
    fn default() -> Imdct {
        let mut long = [[0.0f32; 18]; 18];
        for (row, out) in long.iter_mut().enumerate() {
            let n = if row < 9 { row } else { row + 9 };
            for (k, slot) in out.iter_mut().enumerate() {
                let angle = std::f64::consts::PI / 72.0 * ((2 * n + 19) * (2 * k + 1)) as f64;
                *slot = angle.cos() as f32;
            }
        }
        let mut short = [[0.0f32; 6]; 6];
        for (row, out) in short.iter_mut().enumerate() {
            let n = if row < 3 { row } else { row + 3 };
            for (k, slot) in out.iter_mut().enumerate() {
                let angle = std::f64::consts::PI / 24.0 * ((2 * n + 7) * (2 * k + 1)) as f64;
                *slot = angle.cos() as f32;
            }
        }
        Imdct { long, short }
    }
}

impl Imdct {
    /// 18 coefficients to 36 samples, windowed with `block_type`'s window.
    pub(crate) fn long(&self, x: &[f32], block_type: u8, out: &mut [f32; 36]) {
        let mut half = [0.0f32; 18];
        for (row, slot) in self.long.iter().zip(half.iter_mut()) {
            *slot = row.iter().zip(x).map(|(c, v)| c * v).sum();
        }
        for n in 0..9 {
            out[n] = half[n];
            out[17 - n] = -half[n];
            out[18 + n] = half[9 + n];
            out[35 - n] = half[9 + n];
        }
        let window = &windows()[usize::from(block_type)];
        for (slot, w) in out.iter_mut().zip(window) {
            *slot *= w;
        }
    }

    /// Three 6-coefficient short blocks to one 36-sample overlap buffer.
    pub(crate) fn short(&self, x: &[f32], out: &mut [f32; 36]) {
        out.fill(0.0);
        let window = &windows()[2];
        for w in 0..3 {
            let mut half = [0.0f32; 6];
            for (row, slot) in self.short.iter().zip(half.iter_mut()) {
                *slot = (0..6).map(|k| row[k] * x[k * 3 + w]).sum();
            }
            let mut block = [0.0f32; 12];
            for n in 0..3 {
                block[n] = half[n];
                block[5 - n] = -half[n];
                block[6 + n] = half[3 + n];
                block[11 - n] = half[3 + n];
            }
            for (i, value) in block.iter().enumerate() {
                out[6 + 6 * w + i] += value * window[i];
            }
        }
    }
}

/// The decoder's alias reduction: eight butterflies each side of every long
/// subband boundary.
pub(crate) fn alias_reduce(xr: &mut [f32], bands: usize) {
    let coeffs = alias_coefficients();
    for sb in 1..bands {
        for (i, (cs, ca)) in coeffs.iter().enumerate() {
            let lo = sb * 18 - 1 - i;
            let hi = sb * 18 + i;
            let (a, b) = (xr[lo], xr[hi]);
            xr[lo] = a * cs - b * ca;
            xr[hi] = b * cs + a * ca;
        }
    }
}

/// The encoder's inverse of [`alias_reduce`], applied before quantisation so
/// that the decoder's butterflies undo it exactly.
pub(crate) fn alias_expand(xr: &mut [f32], bands: usize) {
    let coeffs = alias_coefficients();
    for sb in 1..bands {
        for (i, (cs, ca)) in coeffs.iter().enumerate() {
            let lo = sb * 18 - 1 - i;
            let hi = sb * 18 + i;
            let (a, b) = (xr[lo], xr[hi]);
            xr[lo] = a * cs + b * ca;
            xr[hi] = b * cs - a * ca;
        }
    }
}

/// Folds the 64 matrixing rows onto the 32 a DCT-II computes.
fn unfold(g: &[f32; 32], v: &mut [f32; 64]) {
    for (b, slot) in v.iter_mut().enumerate() {
        *slot = match b {
            0..=15 => g[b + 16],
            16 => 0.0,
            17..=47 => -g[48 - b],
            _ => -g[b - 48],
        };
    }
}

/// The synthesis polyphase bank for one channel.
#[derive(Clone, Debug)]
pub(crate) struct Synthesis {
    blocks: Box<[[f32; 64]; 16]>,
    pos: usize,
    dct: Dct<f32>,
}

impl Default for Synthesis {
    fn default() -> Synthesis {
        Synthesis {
            blocks: Box::new([[0.0f32; 64]; 16]),
            pos: 0,
            dct: Dct::new(32),
        }
    }
}

impl Synthesis {
    /// Forgets the window history — what a seek needs.
    pub(crate) fn reset(&mut self) {
        self.blocks.iter_mut().for_each(|b| b.fill(0.0));
        self.pos = 0;
    }

    /// One slot: 32 subband samples in, 32 PCM samples out.
    pub(crate) fn slot(&mut self, subband: &[f32; 32], out: &mut [f32]) {
        let mut g = *subband;
        self.dct.dct2(&mut g);
        self.pos = (self.pos + 15) % 16;
        unfold(&g, &mut self.blocks[self.pos]);
        for (j, sample) in out.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for i in 0..16 {
                let b = if i % 2 == 0 { j } else { j + 32 };
                acc += WINDOW[j + 32 * i] * self.blocks[(self.pos + i) % 16][b];
            }
            *sample = acc;
        }
    }
}

/// The analysis polyphase bank: the adjoint of [`Synthesis`], so that an
/// encode/decode pair reconstructs to the filterbank's own -85 dB limit.
///
/// One subband slot needs the sixteen PCM slots starting at it, so the bank
/// runs fifteen slots behind the samples fed to it — the filterbank delay every
/// Layer III encoder has.
#[derive(Clone, Debug)]
pub(crate) struct Analysis {
    pcm: Box<[[f32; 32]; 16]>,
    pos: usize,
    filled: usize,
    dct: Dct<f32>,
}

impl Default for Analysis {
    fn default() -> Analysis {
        Analysis {
            pcm: Box::new([[0.0f32; 32]; 16]),
            pos: 0,
            filled: 0,
            dct: Dct::new(32),
        }
    }
}

impl Analysis {
    /// Pushes 32 PCM samples; yields the subband slot that became complete.
    pub(crate) fn push(&mut self, pcm: &[f32]) -> Option<[f32; 32]> {
        self.pcm[self.pos].copy_from_slice(pcm);
        self.pos = (self.pos + 1) % 16;
        if self.filled < 15 {
            self.filled += 1;
            return None;
        }
        let mut r = [0.0f32; 32];
        for j in 0..32 {
            let mut even = 0.0f32;
            let mut odd = 0.0f32;
            for i in 0..16 {
                let sample = self.pcm[(self.pos + i) % 16][j] * WINDOW[j + 32 * i];
                if i % 2 == 0 {
                    even += sample;
                } else {
                    odd += sample;
                }
            }
            // fold b = j (even taps) and b = j + 32 (odd taps) onto 0..31
            match j {
                0..=15 => r[j + 16] += even,
                16 => {}
                _ => r[48 - j] -= even,
            }
            let b = j + 32;
            match b {
                32..=47 => r[48 - b] -= odd,
                _ => r[b - 48] -= odd,
            }
        }
        // `Dct::dct3` halves its zeroth term; the matrixing wants it whole.
        r[0] *= 2.0;
        self.dct.dct3(&mut r);
        for slot in &mut r {
            *slot /= 32.0;
        }
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_butterflies_are_exactly_invertible() {
        let mut xr: Vec<f32> = (0..576).map(|i| ((i * 37 % 101) as f32) - 50.0).collect();
        let original = xr.clone();
        alias_expand(&mut xr, 32);
        alias_reduce(&mut xr, 32);
        for (a, b) in xr.iter().zip(&original) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    /// Analysis into synthesis is the pseudo-QMF's round trip: a pure delay of
    /// 15 slots and -80 dB of residual, which is the bank's design limit.
    #[test]
    fn filterbank_round_trip_reconstructs() {
        let mut analysis = Analysis::default();
        let mut synthesis = Synthesis::default();
        let n = 64 * 32;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32;
                (t * 0.031).sin() * 0.4 + (t * 0.21).sin() * 0.2 + (t * 0.7).cos() * 0.1
            })
            .collect();
        let mut output = vec![0.0f32; n];
        let mut slot_out = 0usize;
        for chunk in input.chunks(32) {
            if let Some(subband) = analysis.push(chunk) {
                let (start, end) = (slot_out * 32, slot_out * 32 + 32);
                synthesis.slot(&subband, &mut output[start..end]);
                slot_out += 1;
            }
        }
        // The bank reconstructs in place: subband slot t is the sixteen PCM
        // slots from t, and synthesising it lands back on PCM slot t. Only the
        // first fifteen slots are a ramp, so the comparison starts past them.
        let (from, to) = (20 * 32, 40 * 32);
        let mut error = 0.0f64;
        let mut energy = 0.0f64;
        for i in from..to {
            let want = input[i] as f64;
            let got = output[i] as f64;
            error += (want - got).powi(2);
            energy += want.powi(2);
        }
        let snr = 10.0 * (energy / error).log10();
        assert!(snr > 80.0, "filterbank round trip snr {snr:.1} dB");
    }

    #[test]
    fn imdct_matches_its_definition() {
        let imdct = Imdct::default();
        let x: Vec<f32> = (0..18).map(|k| ((k * 13 % 7) as f32) - 3.0).collect();
        let mut got = [0.0f32; 36];
        imdct.long(&x, 0, &mut got);
        let window = &windows()[0];
        for (n, value) in got.iter().enumerate() {
            let want: f64 = (0..18)
                .map(|k| {
                    x[k] as f64
                        * (std::f64::consts::PI / 72.0 * ((2 * n + 19) * (2 * k + 1)) as f64).cos()
                })
                .sum::<f64>()
                * window[n] as f64;
            assert!((want - *value as f64).abs() < 1e-3, "n={n} {want} {value}");
        }
    }
}
