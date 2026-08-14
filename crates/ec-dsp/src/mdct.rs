//! MDCT and IMDCT — the lapped transform AAC, Vorbis, MP3 and CELT code with.
//!
//! Definition, for a window of `2N` samples producing `N` coefficients:
//!
//! ```text
//! X[k] = sum_{n=0}^{2N-1} x[n] cos(pi/N (n + 1/2 + N/2)(k + 1/2))
//! y[n] = (2/N) sum_{k=0}^{N-1} X[k] cos(pi/N (n + 1/2 + N/2)(k + 1/2))
//! ```
//!
//! The `2/N` on the inverse is the codec convention: with any
//! Princen-Bradley window from [`crate::window`], applied on both analysis and
//! synthesis, 50 %-overlapped frames add up to the original signal exactly
//! (TDAC). [`Mdct::perfect_reconstruction_error`] is that claim as a
//! runnable check.
//!
//! Implementation is the standard fold-then-DCT-IV: the `2N` window folds to
//! `N` points by the transform's own symmetries, and the DCT-IV runs on an
//! `N/2`-point complex FFT. So an AAC long block (2048 samples, 1024
//! coefficients) costs one 512-point FFT — not a 2048x1024 matrix product.

use crate::Real;
use crate::dct::Dct4;

/// An MDCT/IMDCT plan for one window size.
///
/// ```
/// use ec_dsp::{Mdct, Window};
///
/// // AAC long block.
/// let mut mdct = Mdct::<f32>::new(2048);
/// assert_eq!(mdct.spectrum_len(), 1024);
/// let window = Window::<f32>::sine(2048);
/// let error = mdct.perfect_reconstruction_error(&window, 0x51de);
/// assert!(error < 1e-6, "TDAC error {error}");
/// ```
#[derive(Clone, Debug)]
pub struct Mdct<T: Real> {
    half: usize,
    dct: Dct4<T>,
    fold: Vec<T>,
}

impl<T: Real> Mdct<T> {
    /// A plan for a window of `window_len` samples, producing `window_len/2`
    /// coefficients. AAC uses 2048 and 256; Vorbis and CELT sizes are the same
    /// shape.
    ///
    /// # Panics
    /// If `window_len` is not a power of two of at least 4.
    pub fn new(window_len: usize) -> Mdct<T> {
        assert!(
            window_len.is_power_of_two() && window_len >= 4,
            "mdct window must be a power of two of at least 4, got {window_len}"
        );
        let half = window_len / 2;
        Mdct {
            half,
            dct: Dct4::new(half),
            fold: vec![T::ZERO; half],
        }
    }

    /// Samples per window (`2N`).
    pub fn window_len(&self) -> usize {
        self.half * 2
    }

    /// Coefficients per window (`N`).
    pub fn spectrum_len(&self) -> usize {
        self.half
    }

    /// Transforms one window of samples into coefficients.
    ///
    /// # Panics
    /// If `input.len() != window_len()` or `output.len() != spectrum_len()`.
    pub fn forward(&mut self, input: &[T], output: &mut [T]) {
        self.fold_input(input, None);
        output.copy_from_slice(&self.fold);
        self.dct.transform(output);
    }

    /// [`Mdct::forward`] with the analysis window applied in the same pass —
    /// the fold reads each sample once, so windowing here costs one multiply
    /// and no extra buffer.
    ///
    /// # Panics
    /// If any length is wrong, including `window.len() != window_len()`.
    pub fn forward_windowed(&mut self, input: &[T], window: &[T], output: &mut [T]) {
        assert_eq!(
            window.len(),
            self.window_len(),
            "window length must equal the mdct window"
        );
        self.fold_input(input, Some(window));
        output.copy_from_slice(&self.fold);
        self.dct.transform(output);
    }

    /// Transforms coefficients back into one window of samples, ready for
    /// overlap-add. Includes the `2/N` scaling.
    ///
    /// # Panics
    /// If `spectrum.len() != spectrum_len()` or `output.len() != window_len()`.
    pub fn inverse(&mut self, spectrum: &[T], output: &mut [T]) {
        self.unfold(spectrum, output, None);
    }

    /// [`Mdct::inverse`] with the synthesis window applied as the samples are
    /// written out.
    ///
    /// # Panics
    /// If any length is wrong, including `window.len() != window_len()`.
    pub fn inverse_windowed(&mut self, spectrum: &[T], window: &[T], output: &mut [T]) {
        assert_eq!(
            window.len(),
            self.window_len(),
            "window length must equal the mdct window"
        );
        self.unfold(spectrum, output, Some(window));
    }

    /// Folds `2N` samples down to the `N` points DCT-IV acts on, optionally
    /// windowing on the way in.
    fn fold_input(&mut self, input: &[T], window: Option<&[T]>) {
        assert_eq!(
            input.len(),
            self.window_len(),
            "mdct input must be one full window"
        );
        let n = self.half;
        let quarter = n / 2;
        let at = |i: usize| match window {
            Some(w) => input[i] * w[i],
            None => input[i],
        };
        for i in 0..quarter {
            // First half: -c reversed - d. Second half: a - b reversed.
            self.fold[i] = -at(3 * quarter - 1 - i) - at(3 * quarter + i);
            self.fold[quarter + i] = at(i) - at(n - 1 - i);
        }
    }

    /// Runs DCT-IV and unfolds back to `2N` samples with the transform's
    /// symmetries, optionally windowing on the way out.
    fn unfold(&mut self, spectrum: &[T], output: &mut [T], window: Option<&[T]>) {
        assert_eq!(
            spectrum.len(),
            self.half,
            "mdct spectrum must be one full block"
        );
        assert_eq!(
            output.len(),
            self.window_len(),
            "mdct output must be one full window"
        );
        let n = self.half;
        let quarter = n / 2;
        self.fold.copy_from_slice(spectrum);
        self.dct.transform(&mut self.fold);
        let gain = T::from_f64(2.0 / n as f64);
        let mut put = |i: usize, v: T| {
            output[i] = match window {
                Some(w) => v * gain * w[i],
                None => v * gain,
            };
        };
        for i in 0..quarter {
            put(i, self.fold[quarter + i]);
            put(quarter + i, -self.fold[n - 1 - i]);
            put(n + i, -self.fold[quarter - 1 - i]);
            put(n + quarter + i, -self.fold[i]);
        }
    }

    /// Runs three overlapped frames of pseudo-random audio through
    /// analysis and synthesis with `window` and returns the worst absolute
    /// reconstruction error in the fully overlapped region.
    ///
    /// This is the property a codec actually depends on — the transform pair
    /// and the window are only correct *together* — so it is exposed rather
    /// than hidden in a test: a codec's own suite can assert it for whatever
    /// window it ships.
    ///
    /// # Panics
    /// If `window.len() != window_len()`.
    pub fn perfect_reconstruction_error(&mut self, window: &crate::Window<T>, seed: u64) -> f64 {
        let n = self.half;
        let win = window.as_slice();
        assert_eq!(win.len(), 2 * n, "window length must equal the mdct window");

        // 4 hops of N samples; frames 0..2 cover [0, 4N) with 50 % overlap.
        let mut state = seed | 1;
        let signal: Vec<T> = (0..4 * n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                T::from_f64((state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0)
            })
            .collect();

        let mut sum = vec![T::ZERO; 4 * n];
        let mut spectrum = vec![T::ZERO; n];
        let mut frame = vec![T::ZERO; 2 * n];
        for f in 0..3 {
            let start = f * n;
            self.forward_windowed(&signal[start..start + 2 * n], win, &mut spectrum);
            self.inverse_windowed(&spectrum, win, &mut frame);
            for (i, v) in frame.iter().enumerate() {
                sum[start + i] = sum[start + i] + *v;
            }
        }
        // Only [N, 3N) sees both of its overlapping frames.
        (n..3 * n)
            .map(|i| (sum[i].to_f64() - signal[i].to_f64()).abs())
            .fold(0.0, f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Window;
    use crate::tests::{noise, rel_error};
    use core::f64::consts::PI;

    /// O(n^2) reference MDCT, test-only.
    fn reference_mdct(x: &[f64]) -> Vec<f64> {
        let n = x.len() / 2;
        (0..n)
            .map(|k| {
                (0..2 * n)
                    .map(|j| {
                        x[j] * (PI / n as f64
                            * (j as f64 + 0.5 + n as f64 / 2.0)
                            * (k as f64 + 0.5))
                            .cos()
                    })
                    .sum()
            })
            .collect()
    }

    /// O(n^2) reference IMDCT including the 2/N convention, test-only.
    fn reference_imdct(x: &[f64]) -> Vec<f64> {
        let n = x.len();
        (0..2 * n)
            .map(|j| {
                2.0 / n as f64
                    * (0..n)
                        .map(|k| {
                            x[k] * (PI / n as f64
                                * (j as f64 + 0.5 + n as f64 / 2.0)
                                * (k as f64 + 0.5))
                                .cos()
                        })
                        .sum::<f64>()
            })
            .collect()
    }

    #[test]
    fn forward_matches_reference() {
        let mut worst = 0.0f64;
        // 2048 and 256 are the AAC long and short blocks; 512/1024 cover
        // Vorbis and CELT shapes.
        for window_len in [8usize, 64, 256, 512, 1024, 2048] {
            let input: Vec<f64> = noise(window_len, 0x4444 + window_len as u64);
            let want = reference_mdct(&input);
            let mut got = vec![0.0; window_len / 2];
            Mdct::new(window_len).forward(&input, &mut got);
            let error = rel_error(&got, &want);
            worst = worst.max(error);
            assert!(error < 1e-12, "mdct {window_len}: error {error:e}");
        }
        println!("MDCT vs reference: worst rel error {worst:e}");
    }

    #[test]
    fn inverse_matches_reference() {
        let mut worst = 0.0f64;
        for window_len in [8usize, 64, 256, 2048] {
            let spectrum: Vec<f64> = noise(window_len / 2, 0x5555 + window_len as u64);
            let want = reference_imdct(&spectrum);
            let mut got = vec![0.0; window_len];
            Mdct::new(window_len).inverse(&spectrum, &mut got);
            let error = rel_error(&got, &want);
            worst = worst.max(error);
            assert!(error < 1e-12, "imdct {window_len}: error {error:e}");
        }
        println!("IMDCT vs reference: worst rel error {worst:e}");
    }

    #[test]
    fn tdac_reconstructs_with_every_window() {
        for window_len in [256usize, 2048] {
            let mut mdct = Mdct::<f64>::new(window_len);
            for (name, window) in [
                ("sine", Window::<f64>::sine(window_len)),
                ("vorbis", Window::<f64>::vorbis(window_len)),
                ("kbd-4", Window::<f64>::kbd(window_len, 4.0)),
                ("kbd-6", Window::<f64>::kbd(window_len, 6.0)),
            ] {
                let error = mdct.perfect_reconstruction_error(&window, 0x6666);
                assert!(error < 1e-12, "{name} {window_len}: TDAC error {error:e}");
                println!("TDAC {name} {window_len}: max abs error {error:e}");
            }
        }
    }

    #[test]
    fn tdac_reconstructs_in_f32() {
        let mut mdct = Mdct::<f32>::new(2048);
        let window = Window::<f32>::kbd(2048, 4.0);
        let error = mdct.perfect_reconstruction_error(&window, 0x7777);
        assert!(error < 1e-5, "f32 TDAC error {error:e}");
        println!("TDAC f32 kbd-4 2048: max abs error {error:e}");
    }

    #[test]
    fn windowed_forward_equals_manual_windowing() {
        let window_len = 256;
        let input: Vec<f32> = noise(window_len, 0x8888);
        let window = Window::<f32>::sine(window_len);
        let manual: Vec<f32> = input
            .iter()
            .zip(window.as_slice())
            .map(|(x, w)| x * w)
            .collect();
        let mut mdct = Mdct::new(window_len);
        let mut a = vec![0.0f32; window_len / 2];
        let mut b = vec![0.0f32; window_len / 2];
        mdct.forward(&manual, &mut a);
        mdct.forward_windowed(&input, window.as_slice(), &mut b);
        assert_eq!(a, b);
    }
}
