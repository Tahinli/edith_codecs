//! Analysis/synthesis windows for the lapped transforms.
//!
//! All three shapes here satisfy the Princen-Bradley condition
//! `w[n]^2 + w[n + N]^2 == 1`, which is what makes MDCT overlap-add
//! reconstruct exactly; [`Window::princen_bradley_error`] measures it.
//!
//! - [`Window::sine`] — MP3, AAC's other long window, CELT's basis.
//! - [`Window::kbd`] — AAC's Kaiser-Bessel-derived window (alpha 4 for long
//!   blocks, 6 for short, per ISO/IEC 14496-3).
//! - [`Window::vorbis`] — the Vorbis I "slope" window, also used by Opus.
//!
//! Codecs that need asymmetric start/stop windows build them from the halves
//! of two of these; nothing here presumes a symmetric block.

use crate::Real;

/// A precomputed window, one value per sample of the transform block.
///
/// ```
/// use ec_dsp::Window;
///
/// let w = Window::<f64>::kbd(2048, 4.0);
/// assert_eq!(w.len(), 2048);
/// assert!(w.princen_bradley_error() < 1e-12);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct Window<T: Real> {
    samples: Vec<T>,
}

impl<T: Real> Window<T> {
    /// Wraps precomputed values, for shapes this module does not provide
    /// (asymmetric transition windows, for instance).
    ///
    /// # Panics
    /// If `samples` is empty or has odd length.
    pub fn from_samples(samples: Vec<T>) -> Window<T> {
        assert!(
            !samples.is_empty() && samples.len().is_multiple_of(2),
            "a window must have an even, non-zero length"
        );
        Window { samples }
    }

    /// Sine window: `w[n] = sin(pi/L (n + 1/2))`, `L` the full window length.
    ///
    /// # Panics
    /// If `len` is zero or odd.
    pub fn sine(len: usize) -> Window<T> {
        Window::from_samples(
            (0..len)
                .map(|n| {
                    let a = core::f64::consts::PI / len as f64 * (n as f64 + 0.5);
                    T::from_f64(a.sin())
                })
                .collect(),
        )
    }

    /// Vorbis window: `w[n] = sin(pi/2 * sin^2(pi/L (n + 1/2)))`.
    ///
    /// # Panics
    /// If `len` is zero or odd.
    pub fn vorbis(len: usize) -> Window<T> {
        Window::from_samples(
            (0..len)
                .map(|n| {
                    let inner = (core::f64::consts::PI / len as f64 * (n as f64 + 0.5)).sin();
                    T::from_f64((core::f64::consts::FRAC_PI_2 * inner * inner).sin())
                })
                .collect(),
        )
    }

    /// Kaiser-Bessel-derived window with shape parameter `alpha` (AAC uses 4
    /// for 2048-sample blocks and 6 for 256-sample blocks).
    ///
    /// Built the defining way: a Kaiser window of `len/2 + 1` points,
    /// cumulatively summed and square-rooted, then mirrored — which is what
    /// makes the Princen-Bradley identity hold by construction rather than by
    /// numerical luck.
    ///
    /// # Panics
    /// If `len` is not a positive multiple of 4, or `alpha` is negative.
    pub fn kbd(len: usize, alpha: f64) -> Window<T> {
        assert!(
            len >= 4 && len.is_multiple_of(4),
            "a kbd window needs a length that is a positive multiple of 4, got {len}"
        );
        assert!(alpha >= 0.0, "kbd alpha must be non-negative, got {alpha}");
        let half = len / 2;
        let beta = core::f64::consts::PI * alpha;
        let denom = bessel_i0(beta);
        let mut cumulative = Vec::with_capacity(half + 1);
        let mut running = 0.0;
        for m in 0..=half {
            let ratio = 2.0 * m as f64 / half as f64 - 1.0;
            running += bessel_i0(beta * (1.0 - ratio * ratio).max(0.0).sqrt()) / denom;
            cumulative.push(running);
        }
        let total = running;
        let mut samples = vec![T::ZERO; len];
        for n in 0..half {
            let v = T::from_f64((cumulative[n] / total).sqrt());
            samples[n] = v;
            samples[len - 1 - n] = v;
        }
        Window::from_samples(samples)
    }

    /// The window values.
    pub fn as_slice(&self) -> &[T] {
        &self.samples
    }

    /// Samples in the window.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Always false — a window cannot be empty (see [`Window::from_samples`]).
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Multiplies `block` by the window in place, vectorised.
    ///
    /// # Panics
    /// If `block.len() != self.len()`.
    pub fn apply(&self, block: &mut [T]) {
        assert_eq!(
            block.len(),
            self.samples.len(),
            "block length must equal window length"
        );
        let lanes = T::LANES;
        let mut chunks = block.chunks_exact_mut(lanes);
        for (chunk, w) in (&mut chunks).zip(self.samples.chunks_exact(lanes)) {
            T::store(T::load(chunk) * T::load(w), chunk);
        }
        let done = self.samples.len() - self.samples.len() % lanes;
        for (v, w) in chunks
            .into_remainder()
            .iter_mut()
            .zip(&self.samples[done..])
        {
            *v = *v * *w;
        }
    }

    /// Worst deviation from the Princen-Bradley condition
    /// `w[n]^2 + w[n + len/2]^2 == 1`, the property MDCT overlap-add needs.
    pub fn princen_bradley_error(&self) -> f64 {
        let half = self.samples.len() / 2;
        (0..half)
            .map(|n| {
                let a = self.samples[n].to_f64();
                let b = self.samples[n + half].to_f64();
                (a * a + b * b - 1.0).abs()
            })
            .fold(0.0, f64::max)
    }
}

/// Modified Bessel function of the first kind, order zero, by its power
/// series — accurate to `f64` for the arguments a KBD window uses (`beta` up
/// to about 20).
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    while term > 1e-18 * sum {
        term *= (half / k) * (half / k);
        sum += term;
        k += 1.0;
        if k > 200.0 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_shapes_satisfy_princen_bradley() {
        for len in [64usize, 256, 2048] {
            for (name, w) in [
                ("sine", Window::<f64>::sine(len)),
                ("vorbis", Window::<f64>::vorbis(len)),
                ("kbd-4", Window::<f64>::kbd(len, 4.0)),
                ("kbd-6", Window::<f64>::kbd(len, 6.0)),
                ("kbd-0", Window::<f64>::kbd(len, 0.0)),
            ] {
                let error = w.princen_bradley_error();
                assert!(error < 1e-12, "{name} {len}: PB error {error:e}");
            }
        }
    }

    #[test]
    fn windows_are_symmetric_and_bounded() {
        let len = 512;
        for w in [
            Window::<f64>::sine(len),
            Window::<f64>::vorbis(len),
            Window::<f64>::kbd(len, 4.0),
        ] {
            let s = w.as_slice();
            for n in 0..len {
                assert!((0.0..=1.0).contains(&s[n]), "value out of range: {}", s[n]);
                assert!(
                    (s[n] - s[len - 1 - n]).abs() < 1e-12,
                    "asymmetric at {n}: {} vs {}",
                    s[n],
                    s[len - 1 - n]
                );
            }
        }
    }

    #[test]
    fn bessel_i0_matches_known_values() {
        // Abramowitz & Stegun table values.
        for (x, want) in [
            (0.0, 1.0),
            (1.0, 1.266_065_877_752_008_4),
            (5.0, 27.239_871_823_604_44),
            (10.0, 2_815.716_628_466_254),
        ] {
            let got = bessel_i0(x);
            assert!(
                (got - want).abs() <= 1e-9 * want.abs().max(1.0),
                "I0({x}) = {got}, want {want}"
            );
        }
    }

    #[test]
    fn apply_matches_scalar_multiply() {
        let len = 262; // not a multiple of the f64 lane count
        let w = Window::<f64>::sine(len);
        let block: Vec<f64> = (0..len).map(|n| n as f64 * 0.5 - 3.0).collect();
        let want: Vec<f64> = block.iter().zip(w.as_slice()).map(|(b, s)| b * s).collect();
        let mut got = block.clone();
        w.apply(&mut got);
        assert_eq!(got, want);
    }

    #[test]
    #[should_panic(expected = "multiple of 4")]
    fn kbd_rejects_odd_lengths() {
        let _ = Window::<f32>::kbd(30, 4.0);
    }
}
