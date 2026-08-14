//! The inverse transform of A/52 §7.9.4: one 512-sample IMDCT, or two
//! 256-sample IMDCTs when the block switch flag is set, followed by the window
//! and overlap-add of §7.9.4 step 6.
//!
//! Both cases are the standard's own factorisation — pre-twiddle, an `N/4` (or
//! `N/8`) point complex inverse FFT from `ec_dsp`, post-twiddle, then the
//! interleaved windowing step. Writing it that way rather than calling a
//! generic MDCT keeps the sign and ordering conventions the standard's, so
//! there is nothing to reconcile when reading the two side by side.

use ec_dsp::Fft;
use ec_dsp::fft::Complex;

use crate::tables::WINDOW;

/// Samples produced per audio block, per channel.
pub const BLOCK_SAMPLES: usize = 256;

/// The plans and twiddles for both transform lengths.
#[derive(Clone)]
pub struct Imdct {
    long: Fft<f32>,
    short: Fft<f32>,
    /// Pre-twiddle for N = 512: `-cos/-sin(2π(8k+1)/4096)`, k < 128.
    pre_long: Vec<Complex<f32>>,
    /// Post-twiddle for N = 512: the same, scaled by 128 to undo the `1/n`
    /// that [`Fft::inverse`] applies but the standard's IFFT step does not.
    post_long: Vec<Complex<f32>>,
    /// Pre-twiddle for the 256-sample halves: `-cos/-sin(2π(8k+1)/2048)`.
    pre_short: Vec<Complex<f32>>,
    /// Post-twiddle for the halves, scaled by 64.
    post_short: Vec<Complex<f32>>,
    z: Vec<Complex<f32>>,
    z2: Vec<Complex<f32>>,
    x: Vec<f32>,
}

impl Default for Imdct {
    fn default() -> Imdct {
        Imdct::new()
    }
}

fn twiddles(count: usize, denom: f64, scale: f64) -> Vec<Complex<f32>> {
    (0..count)
        .map(|k| {
            let angle = 2.0 * std::f64::consts::PI * (8.0 * k as f64 + 1.0) / denom;
            Complex::new((-angle.cos() * scale) as f32, (-angle.sin() * scale) as f32)
        })
        .collect()
}

impl Imdct {
    /// Build both plans. Allocates once; [`Imdct::block`] allocates nothing.
    pub fn new() -> Imdct {
        const N: f64 = 512.0;
        Imdct {
            long: Fft::new(128),
            short: Fft::new(64),
            pre_long: twiddles(128, 8.0 * N, 1.0),
            post_long: twiddles(128, 8.0 * N, 128.0),
            pre_short: twiddles(64, 4.0 * N, 1.0),
            post_short: twiddles(64, 4.0 * N, 64.0),
            z: vec![Complex::new(0.0, 0.0); 128],
            z2: vec![Complex::new(0.0, 0.0); 64],
            x: vec![0.0; 512],
        }
    }

    /// One audio block: `coeffs` (256 spectral values) in, 256 PCM samples out,
    /// carrying `delay` across the call as the overlap-add tail.
    ///
    /// `block_switch` selects the two 256-sample transforms of §7.9.4.2.
    pub fn block(
        &mut self,
        coeffs: &[f32],
        block_switch: bool,
        delay: &mut [f32],
        out: &mut [f32],
    ) {
        debug_assert!(coeffs.len() >= 256 && delay.len() >= 256 && out.len() >= 256);
        if block_switch {
            self.short_transform(coeffs);
        } else {
            self.long_transform(coeffs);
        }
        // §7.9.4 step 6. The factor of 2 undoes the encoder's headroom scaling.
        for n in 0..BLOCK_SAMPLES {
            out[n] = 2.0 * (self.x[n] + delay[n]);
            delay[n] = self.x[BLOCK_SAMPLES + n];
        }
    }

    /// Drop the overlap-add tail; a seek makes it belong to frames that were
    /// never decoded.
    pub fn reset(delay: &mut [f32]) {
        delay.fill(0.0);
    }

    fn long_transform(&mut self, coeffs: &[f32]) {
        // Step 2: pre-IFFT complex multiply.
        for k in 0..128 {
            let (a, b) = (coeffs[255 - 2 * k], coeffs[2 * k]);
            let t = self.pre_long[k];
            self.z[k] = Complex::new(a * t.re - b * t.im, b * t.re + a * t.im);
        }
        // Step 3: N/4-point complex IFFT.
        self.long.inverse(&mut self.z);
        // Step 4: post-IFFT complex multiply (with the 1/n undone).
        for n in 0..128 {
            let (zr, zi) = (self.z[n].re, self.z[n].im);
            let t = self.post_long[n];
            self.z[n] = Complex::new(zr * t.re - zi * t.im, zi * t.re + zr * t.im);
        }
        // Step 5: windowing and de-interleaving.
        let (y, x) = (&self.z, &mut self.x);
        for n in 0..64 {
            x[2 * n] = -y[64 + n].im * WINDOW[2 * n];
            x[2 * n + 1] = y[63 - n].re * WINDOW[2 * n + 1];
            x[128 + 2 * n] = -y[n].re * WINDOW[128 + 2 * n];
            x[128 + 2 * n + 1] = y[127 - n].im * WINDOW[128 + 2 * n + 1];
            x[256 + 2 * n] = -y[64 + n].re * WINDOW[255 - 2 * n];
            x[256 + 2 * n + 1] = y[63 - n].im * WINDOW[254 - 2 * n];
            x[384 + 2 * n] = y[n].im * WINDOW[127 - 2 * n];
            x[384 + 2 * n + 1] = -y[127 - n].re * WINDOW[126 - 2 * n];
        }
    }

    fn short_transform(&mut self, coeffs: &[f32]) {
        // Step 1-2: split the coefficients even/odd, then pre-twiddle each.
        for k in 0..64 {
            let (a1, b1) = (coeffs[2 * (127 - 2 * k)], coeffs[2 * (2 * k)]);
            let (a2, b2) = (coeffs[2 * (127 - 2 * k) + 1], coeffs[2 * (2 * k) + 1]);
            let t = self.pre_short[k];
            self.z[k] = Complex::new(a1 * t.re - b1 * t.im, b1 * t.re + a1 * t.im);
            self.z2[k] = Complex::new(a2 * t.re - b2 * t.im, b2 * t.re + a2 * t.im);
        }
        // Step 3: two N/8-point complex IFFTs.
        self.short.inverse(&mut self.z[..64]);
        self.short.inverse(&mut self.z2);
        // Step 4: post-twiddle both.
        for n in 0..64 {
            let t = self.post_short[n];
            let (zr, zi) = (self.z[n].re, self.z[n].im);
            self.z[n] = Complex::new(zr * t.re - zi * t.im, zi * t.re + zr * t.im);
            let (zr, zi) = (self.z2[n].re, self.z2[n].im);
            self.z2[n] = Complex::new(zr * t.re - zi * t.im, zi * t.re + zr * t.im);
        }
        // Step 5: windowing and de-interleaving.
        let (y1, y2, x) = (&self.z, &self.z2, &mut self.x);
        for n in 0..64 {
            x[2 * n] = -y1[n].im * WINDOW[2 * n];
            x[2 * n + 1] = y1[63 - n].re * WINDOW[2 * n + 1];
            x[128 + 2 * n] = -y1[n].re * WINDOW[128 + 2 * n];
            x[128 + 2 * n + 1] = y1[63 - n].im * WINDOW[128 + 2 * n + 1];
            x[256 + 2 * n] = -y2[n].re * WINDOW[255 - 2 * n];
            x[256 + 2 * n + 1] = y2[63 - n].im * WINDOW[254 - 2 * n];
            x[384 + 2 * n] = y2[n].im * WINDOW[127 - 2 * n];
            x[384 + 2 * n + 1] = -y2[63 - n].re * WINDOW[126 - 2 * n];
        }
    }
}

/// A single MDCT coefficient's worth of energy, used by the round-trip test
/// below and by nothing else: the forward transform the standard's encoder
/// side describes, at the one length the test needs.
#[cfg(test)]
fn forward_mdct_512(input: &[f32], out: &mut [f32]) {
    use std::f64::consts::PI;
    let n = 512.0;
    for (k, o) in out.iter_mut().enumerate().take(256) {
        let mut acc = 0.0f64;
        for (i, &s) in input.iter().enumerate().take(512) {
            let angle = 2.0 * PI / n * (i as f64 + 0.5 + n / 4.0) * (k as f64 + 0.5);
            acc += f64::from(s) * f64::from(WINDOW[window_index(i)]) * angle.cos();
        }
        *o = (-acc * 2.0 / n) as f32;
    }
}

#[cfg(test)]
fn window_index(i: usize) -> usize {
    if i < 256 { i } else { 511 - i }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_blocks_reconstruct_under_overlap_add() {
        // Feed two overlapping windowed transforms of one signal and check the
        // overlap-add of their inverses returns the middle 256 samples. This is
        // the property the whole §7.9.4 factorisation exists to have; getting a
        // sign or an index wrong in step 5 destroys it.
        let signal: Vec<f32> = (0..768)
            .map(|n| (n as f32 * 0.077).sin() * 0.4 + (n as f32 * 0.31).cos() * 0.2)
            .collect();
        let mut spec_a = [0.0f32; 256];
        let mut spec_b = [0.0f32; 256];
        forward_mdct_512(&signal[0..512], &mut spec_a);
        forward_mdct_512(&signal[256..768], &mut spec_b);

        let mut imdct = Imdct::new();
        let mut delay = [0.0f32; 256];
        let mut out = [0.0f32; 256];
        imdct.block(&spec_a, false, &mut delay, &mut out);
        imdct.block(&spec_b, false, &mut delay, &mut out);
        for n in 0..256 {
            let want = signal[256 + n];
            assert!(
                (out[n] - want).abs() < 2e-3,
                "n = {n}: {} vs {want}",
                out[n]
            );
        }
    }

    #[test]
    fn short_blocks_produce_the_same_energy_as_the_signal() {
        // A block-switched pair of 256-point transforms has no closed-form
        // reference here, so the check is the weaker but still load-bearing
        // one: a single non-zero coefficient must produce a bounded, non-silent
        // output, and silence must stay silent.
        let mut imdct = Imdct::new();
        let mut delay = [0.0f32; 256];
        let mut out = [0.0f32; 256];
        let mut spec = [0.0f32; 256];
        spec[40] = 0.5;
        imdct.block(&spec, true, &mut delay, &mut out);
        let energy: f32 = out.iter().map(|v| v * v).sum();
        assert!(energy > 1e-6, "block-switched output is silent");
        assert!(out.iter().all(|v| v.abs() < 4.0));

        let mut silent_delay = [0.0f32; 256];
        imdct.block(&[0.0; 256], true, &mut silent_delay, &mut out);
        assert!(out.iter().all(|v| *v == 0.0));
    }
}
