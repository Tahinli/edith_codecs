//! The one set of transforms every edith_codecs audio and video coder uses.
//!
//! AAC, Vorbis, Opus/CELT, MP3 and the JPEG-class video paths all reduce to the
//! same three kernels, so they live here once: a power-of-two complex [`Fft`]
//! (with a real-input wrapper), the DCT family built on top of it, and
//! [`Mdct`] — the lapped transform those codecs actually call — routed through
//! an `N/4`-point FFT.
//!
//! Contracts worth knowing before implementing against this crate:
//!
//! - **The FFT is never optional.** There is no feature flag, no fallback, no
//!   `O(n²)` path anywhere: a codec that links this crate gets the fast
//!   transform or does not compile. Two incumbent crates shipped an `O(n²)`
//!   transform behind a default-off feature and ran 40x slower than the
//!   product needed; that failure mode is designed out rather than tested for.
//! - **Plans own their scratch.** [`Fft`], [`Dct`], [`Dct4`] and [`Mdct`] are
//!   built once per size and then reused; the transform methods take `&mut
//!   self` and allocate nothing. Clone a plan to use it from another thread.
//! - **Sizes are checked at construction, not per call.** A bad size is API
//!   misuse and panics in the constructor; the hot path has no error branch.
//! - **`f32` and `f64` are the same code.** Twiddles are always computed in
//!   `f64` and rounded once, so the `f32` plans keep the accuracy their table
//!   allows.
//! - **Inverses are scaled.** [`Fft::inverse`] divides by `n`, so a forward
//!   followed by an inverse returns the input. [`Mdct`] follows the usual
//!   codec convention (`2/N` on the inverse) and reconstructs exactly under
//!   overlap-add with any Princen-Bradley window from [`window`].
//!
//! Hot loops are vectorised with the `wide` crate through [`Real`]; the scalar
//! and SIMD paths are the same expressions over the same data layout, so
//! neither can drift from the other in correctness.
//!
//! No unsafe, no allocation on the transform path, no other dependencies.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod dct;
pub mod fft;
pub mod mdct;
pub mod window;

pub use dct::{Dct, Dct4};
pub use fft::{Complex, Fft, RealFft};
pub use mdct::Mdct;
pub use window::Window;

use core::fmt::Debug;
use core::ops::{Add, Div, Mul, Neg, Sub};

/// The sample types the transforms are generic over: `f32` and `f64`.
///
/// The trait exists to give one implementation of each transform for both
/// precisions *including* its SIMD form: [`Real::Lanes`] is the widest `wide`
/// vector for the type, and the kernels fall back to scalar code on the
/// remainder of a row. Implementing it for anything else is not supported —
/// the transforms assume IEEE-754 binary arithmetic.
pub trait Real:
    Copy
    + Debug
    + Default
    + PartialOrd
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
    + Send
    + Sync
    + 'static
{
    /// A vector of [`Real::LANES`] values, backed by `wide`.
    type Lanes: Copy
        + Add<Output = Self::Lanes>
        + Sub<Output = Self::Lanes>
        + Mul<Output = Self::Lanes>;

    /// Values per [`Real::Lanes`] vector (8 for `f32`, 4 for `f64`).
    const LANES: usize;
    /// Additive identity.
    const ZERO: Self;
    /// Multiplicative identity.
    const ONE: Self;

    /// Rounds an `f64` — how every table entry in this crate is produced.
    fn from_f64(v: f64) -> Self;
    /// Widens to `f64`, for reference computations and reporting.
    fn to_f64(self) -> f64;
    /// Square root.
    fn sqrt(self) -> Self;
    /// Absolute value.
    fn abs(self) -> Self;
    /// Broadcasts one value across a vector.
    fn splat(self) -> Self::Lanes;
    /// Loads exactly [`Real::LANES`] values. Panics if `src` is another
    /// length — callers feed it `chunks_exact(LANES)`, which is what lets the
    /// bounds check fold away in the transform inner loops.
    fn load(src: &[Self]) -> Self::Lanes;
    /// Stores exactly [`Real::LANES`] values. Panics if `dst` is another length.
    fn store(v: Self::Lanes, dst: &mut [Self]);
}

impl Real for f32 {
    type Lanes = wide::f32x8;

    const LANES: usize = 8;
    const ZERO: f32 = 0.0;
    const ONE: f32 = 1.0;

    #[inline]
    fn from_f64(v: f64) -> f32 {
        v as f32
    }
    #[inline]
    fn to_f64(self) -> f64 {
        self as f64
    }
    #[inline]
    fn sqrt(self) -> f32 {
        f32::sqrt(self)
    }
    #[inline]
    fn abs(self) -> f32 {
        f32::abs(self)
    }
    #[inline]
    fn splat(self) -> wide::f32x8 {
        wide::f32x8::splat(self)
    }
    #[inline]
    fn load(src: &[f32]) -> wide::f32x8 {
        let chunk: [f32; 8] = src.try_into().expect("load needs exactly 8 values");
        wide::bytemuck::cast(chunk)
    }
    #[inline]
    fn store(v: wide::f32x8, dst: &mut [f32]) {
        dst.copy_from_slice(&v.to_array());
    }
}

impl Real for f64 {
    type Lanes = wide::f64x4;

    const LANES: usize = 4;
    const ZERO: f64 = 0.0;
    const ONE: f64 = 1.0;

    #[inline]
    fn from_f64(v: f64) -> f64 {
        v
    }
    #[inline]
    fn to_f64(self) -> f64 {
        self
    }
    #[inline]
    fn sqrt(self) -> f64 {
        f64::sqrt(self)
    }
    #[inline]
    fn abs(self) -> f64 {
        f64::abs(self)
    }
    #[inline]
    fn splat(self) -> wide::f64x4 {
        wide::f64x4::splat(self)
    }
    #[inline]
    fn load(src: &[f64]) -> wide::f64x4 {
        let chunk: [f64; 4] = src.try_into().expect("load needs exactly 4 values");
        wide::bytemuck::cast(chunk)
    }
    #[inline]
    fn store(v: wide::f64x4, dst: &mut [f64]) {
        dst.copy_from_slice(&v.to_array());
    }
}

/// Multiplies `data` by `factor` in place, vectorised.
pub(crate) fn scale<T: Real>(data: &mut [T], factor: T) {
    let fv = factor.splat();
    let mut chunks = data.chunks_exact_mut(T::LANES);
    for chunk in &mut chunks {
        T::store(T::load(chunk) * fv, chunk);
    }
    for v in chunks.into_remainder() {
        *v = *v * factor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic test signal — no `rand` dependency in a DSP crate.
    pub(crate) fn noise<T: Real>(n: usize, seed: u64) -> Vec<T> {
        let mut state = seed | 1;
        (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
                T::from_f64(unit * 2.0 - 1.0)
            })
            .collect()
    }

    /// Relative L2 error of `got` against `want`, both read as `f64`.
    pub(crate) fn rel_error<T: Real>(got: &[T], want: &[T]) -> f64 {
        let mut num = 0.0;
        let mut den = 0.0;
        for (g, w) in got.iter().zip(want) {
            let d = g.to_f64() - w.to_f64();
            num += d * d;
            den += w.to_f64() * w.to_f64();
        }
        (num / den.max(f64::MIN_POSITIVE)).sqrt()
    }

    /// Throughput, printed rather than asserted: the numbers are machine
    /// specific, but a regression of the *order of magnitude* is what the
    /// family cares about (an incumbent shipped an O(n^2) transform and lost
    /// 40x). Run with:
    ///
    /// `cargo test -p ec-dsp --release -- --ignored --nocapture throughput`
    #[test]
    #[ignore = "timing; run in release with --ignored --nocapture"]
    fn throughput() {
        use crate::{Complex, Fft, Mdct, RealFft, Window};
        use std::time::Instant;

        let n = 2048;
        let signal: Vec<f32> = noise(n, 0xdeadbeef);
        let mut guard = 0.0f32;

        let mut fft = Fft::<f32>::new(n);
        let mut data: Vec<Complex<f32>> = signal.iter().map(|v| Complex::new(*v, 0.0)).collect();
        let iters = 50_000;
        // Alternating directions keeps the in-place data bounded, so the
        // numbers are not measured on infinities; the inverse costs one extra
        // scaling pass, which makes this average slightly pessimistic.
        let t0 = Instant::now();
        for i in 0..iters {
            if i % 2 == 0 {
                fft.forward(&mut data);
            } else {
                fft.inverse(&mut data);
            }
        }
        report("complex FFT f32 2048 fwd/inv", t0.elapsed(), iters, n);
        guard += data[0].re;

        let mut re: Vec<f32> = signal.clone();
        let mut im: Vec<f32> = vec![0.0; n];
        let t0 = Instant::now();
        for i in 0..iters {
            if i % 2 == 0 {
                fft.forward_split(&mut re, &mut im);
            } else {
                fft.inverse_split(&mut re, &mut im);
            }
        }
        report("split FFT f32 2048 fwd/inv", t0.elapsed(), iters, n);
        guard += re[0];

        let mut rfft = RealFft::<f32>::new(n);
        let mut spectrum = vec![Complex::new(0.0f32, 0.0); n / 2 + 1];
        let t0 = Instant::now();
        for _ in 0..iters {
            rfft.forward(&signal, &mut spectrum);
        }
        report("real FFT f32 2048 fwd", t0.elapsed(), iters, n);
        guard += spectrum[0].re;

        let mut mdct = Mdct::<f32>::new(n);
        let window = Window::<f32>::kbd(n, 4.0);
        let mut coeffs = vec![0.0f32; n / 2];
        let t0 = Instant::now();
        for _ in 0..iters {
            mdct.forward_windowed(&signal, window.as_slice(), &mut coeffs);
        }
        report("MDCT f32 2048 fwd (windowed)", t0.elapsed(), iters, n);
        guard += coeffs[0];

        let mut frame = vec![0.0f32; n];
        let t0 = Instant::now();
        for _ in 0..iters {
            mdct.inverse_windowed(&coeffs, window.as_slice(), &mut frame);
        }
        report("IMDCT f32 2048 inv (windowed)", t0.elapsed(), iters, n);
        guard += frame[0];

        // 48 kHz stereo AAC: 2 channels * 46.875 long blocks per second.
        let per_block = t0.elapsed().as_secs_f64() / iters as f64;
        println!("guard {guard:e}");
        println!(
            "one 2048 IMDCT = {:.2} us; a 5.1 48 kHz stream needs 6*46.9 = 281 per second",
            per_block * 1e6
        );
    }

    fn report(label: &str, elapsed: std::time::Duration, iters: u32, n: usize) {
        let per = elapsed.as_secs_f64() / iters as f64;
        println!(
            "{label:<32} {:>8.0} ns/transform  {:>7.1} Mpoint/s",
            per * 1e9,
            n as f64 / per / 1e6
        );
    }

    #[test]
    fn scale_matches_scalar_across_the_lane_boundary() {
        for n in [1usize, 3, 8, 9, 17, 64] {
            let mut data: Vec<f32> = noise(n, 7);
            let want: Vec<f32> = data.iter().map(|v| v * 0.25).collect();
            scale(&mut data, 0.25);
            assert_eq!(data, want, "n = {n}");
        }
    }
}
