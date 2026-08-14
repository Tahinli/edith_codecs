//! DCT-II, DCT-III and DCT-IV, all routed through an FFT.
//!
//! Every one of these is `O(n log n)` here. Definitions, unnormalised, for
//! length `N`:
//!
//! - DCT-II:  `X[k] = sum_n x[n] cos(pi/N (n + 1/2) k)`
//! - DCT-III: `X[k] = x[0]/2 + sum_{n>=1} x[n] cos(pi/N n (k + 1/2))`
//! - DCT-IV:  `X[k] = sum_n x[n] cos(pi/N (n + 1/2)(k + 1/2))`
//!
//! DCT-III is the inverse of DCT-II up to the usual `2/N`, i.e.
//! `x == (2/N) * dct3(dct2(x))`. DCT-IV is its own inverse up to the same
//! factor, which is why [`crate::Mdct`] needs only the one plan for both
//! directions.
//!
//! II and III use an `N`-point real FFT (Makhoul's method); IV uses an
//! `N/2`-point complex FFT with a quarter-sample twiddle on each side.

use crate::Real;
use crate::fft::{Complex, Fft, RealFft};

/// DCT-II / DCT-III plan for one size.
///
/// ```
/// use ec_dsp::Dct;
///
/// let mut dct = Dct::<f64>::new(8);
/// let mut data = [1.0, 2.0, 3.0, 4.0, 4.0, 3.0, 2.0, 1.0];
/// let original = data;
/// dct.dct2(&mut data);
/// dct.dct3(&mut data);
/// // dct3 . dct2 = (N/2) * identity
/// assert!(data.iter().zip(&original).all(|(a, b)| (a / 4.0 - b).abs() < 1e-12));
/// ```
#[derive(Clone, Debug)]
pub struct Dct<T: Real> {
    n: usize,
    rfft: RealFft<T>,
    twiddle_re: Vec<T>,
    twiddle_im: Vec<T>,
    shuffled: Vec<T>,
    spectrum: Vec<Complex<T>>,
}

impl<T: Real> Dct<T> {
    /// A plan for `n` points.
    ///
    /// # Panics
    /// If `n` is not a power of two of at least 4 (see [`crate::fft::MAX_LEN`]).
    pub fn new(n: usize) -> Dct<T> {
        let rfft = RealFft::new(n);
        let mut twiddle_re = Vec::with_capacity(n);
        let mut twiddle_im = Vec::with_capacity(n);
        for k in 0..n {
            // exp(-i*pi*k/(2N)): the half-sample shift that turns a DFT of the
            // even/odd-shuffled signal into a DCT.
            let angle = -core::f64::consts::PI * k as f64 / (2.0 * n as f64);
            twiddle_re.push(T::from_f64(angle.cos()));
            twiddle_im.push(T::from_f64(angle.sin()));
        }
        Dct {
            n,
            rfft,
            twiddle_re,
            twiddle_im,
            shuffled: vec![T::ZERO; n],
            spectrum: vec![Complex::ZERO; n / 2 + 1],
        }
    }

    /// Points per transform.
    pub fn size(&self) -> usize {
        self.n
    }

    /// DCT-II in place.
    ///
    /// # Panics
    /// If `data.len() != self.size()`.
    pub fn dct2(&mut self, data: &mut [T]) {
        assert_eq!(data.len(), self.n, "dct input length must equal plan size");
        let n = self.n;
        // Even samples in order, odd samples reversed: a real DFT of this
        // sequence carries the DCT-II in its real part after a twiddle.
        for j in 0..n / 2 {
            self.shuffled[j] = data[2 * j];
            self.shuffled[n - 1 - j] = data[2 * j + 1];
        }
        self.rfft.forward(&self.shuffled, &mut self.spectrum);
        for (k, out) in data.iter_mut().enumerate() {
            let bin = if k <= n / 2 {
                self.spectrum[k]
            } else {
                self.spectrum[n - k].conj()
            };
            let tw = Complex::new(self.twiddle_re[k], self.twiddle_im[k]);
            *out = (tw * bin).re;
        }
    }

    /// DCT-III in place — [`Dct::dct2`] run backwards, times `N/2`.
    ///
    /// # Panics
    /// If `data.len() != self.size()`.
    pub fn dct3(&mut self, data: &mut [T]) {
        assert_eq!(data.len(), self.n, "dct input length must equal plan size");
        let n = self.n;
        for k in 0..=n / 2 {
            // The spectrum whose real part is `data`: G[k] = X[k] - i X[N-k].
            let hi = if k == 0 { T::ZERO } else { data[n - k] };
            let g = Complex::new(data[k], -hi);
            let tw = Complex::new(self.twiddle_re[k], self.twiddle_im[k]).conj();
            self.spectrum[k] = tw * g;
        }
        self.rfft.inverse(&self.spectrum, &mut self.shuffled);
        let gain = T::from_f64(n as f64 / 2.0);
        for j in 0..n / 2 {
            data[2 * j] = self.shuffled[j] * gain;
            data[2 * j + 1] = self.shuffled[n - 1 - j] * gain;
        }
    }
}

/// DCT-IV plan for one size — the kernel [`crate::Mdct`] is built on.
///
/// ```
/// use ec_dsp::Dct4;
///
/// let mut dct = Dct4::<f64>::new(8);
/// let mut data = [1.0, -2.0, 3.0, 0.5, 0.0, 1.5, -1.0, 2.0];
/// let original = data;
/// dct.transform(&mut data);
/// dct.transform(&mut data);
/// // DCT-IV is an involution up to N/2.
/// assert!(data.iter().zip(&original).all(|(a, b)| (a / 4.0 - b).abs() < 1e-12));
/// ```
#[derive(Clone, Debug)]
pub struct Dct4<T: Real> {
    n: usize,
    fft: Fft<T>,
    pre_re: Vec<T>,
    pre_im: Vec<T>,
    post_re: Vec<T>,
    post_im: Vec<T>,
    buf_re: Vec<T>,
    buf_im: Vec<T>,
}

impl<T: Real> Dct4<T> {
    /// A plan for `n` points, `n` a power of two of at least 2.
    ///
    /// # Panics
    /// If `n` is not a power of two, or `n/2` is not a legal FFT size.
    pub fn new(n: usize) -> Dct4<T> {
        assert!(
            n.is_power_of_two() && n >= 2,
            "dct-iv length must be a power of two of at least 2, got {n}"
        );
        let half = n / 2;
        let mut pre_re = Vec::with_capacity(half);
        let mut pre_im = Vec::with_capacity(half);
        let mut post_re = Vec::with_capacity(half);
        let mut post_im = Vec::with_capacity(half);
        for j in 0..half {
            // exp(-i*pi*(4j+1)/(4N)) before the FFT, exp(-i*pi*k/N) after:
            // together they supply the (n+1/2)(k+1/2) phase of a DCT-IV.
            let pre = -core::f64::consts::PI * (4 * j + 1) as f64 / (4.0 * n as f64);
            pre_re.push(T::from_f64(pre.cos()));
            pre_im.push(T::from_f64(pre.sin()));
            let post = -core::f64::consts::PI * j as f64 / n as f64;
            post_re.push(T::from_f64(post.cos()));
            post_im.push(T::from_f64(post.sin()));
        }
        Dct4 {
            n,
            fft: Fft::new(half),
            pre_re,
            pre_im,
            post_re,
            post_im,
            buf_re: vec![T::ZERO; half],
            buf_im: vec![T::ZERO; half],
        }
    }

    /// Points per transform.
    pub fn size(&self) -> usize {
        self.n
    }

    /// DCT-IV in place.
    ///
    /// # Panics
    /// If `data.len() != self.size()`.
    pub fn transform(&mut self, data: &mut [T]) {
        assert_eq!(
            data.len(),
            self.n,
            "dct-iv input length must equal plan size"
        );
        let n = self.n;
        let half = n / 2;
        for j in 0..half {
            let (re, im) = (data[2 * j], data[n - 1 - 2 * j]);
            self.buf_re[j] = re * self.pre_re[j] - im * self.pre_im[j];
            self.buf_im[j] = re * self.pre_im[j] + im * self.pre_re[j];
        }
        self.fft.forward_split(&mut self.buf_re, &mut self.buf_im);
        for k in 0..half {
            let (re, im) = (self.buf_re[k], self.buf_im[k]);
            let (wr, wi) = (self.post_re[k], self.post_im[k]);
            data[2 * k] = re * wr - im * wi;
            data[n - 1 - 2 * k] = -(re * wi + im * wr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{noise, rel_error};
    use core::f64::consts::PI;

    /// O(n^2) reference DCTs, test-only.
    fn reference(kind: u8, x: &[f64]) -> Vec<f64> {
        let n = x.len();
        (0..n)
            .map(|k| match kind {
                2 => (0..n)
                    .map(|j| x[j] * (PI / n as f64 * (j as f64 + 0.5) * k as f64).cos())
                    .sum(),
                3 => {
                    x[0] / 2.0
                        + (1..n)
                            .map(|j| x[j] * (PI / n as f64 * j as f64 * (k as f64 + 0.5)).cos())
                            .sum::<f64>()
                }
                _ => (0..n)
                    .map(|j| x[j] * (PI / n as f64 * (j as f64 + 0.5) * (k as f64 + 0.5)).cos())
                    .sum(),
            })
            .collect()
    }

    #[test]
    fn dct2_and_dct3_agree_with_reference() {
        let mut worst = 0.0f64;
        for shift in 2..=13 {
            let n = 1usize << shift;
            let input: Vec<f64> = noise(n, 0x2222 + n as u64);
            for kind in [2u8, 3] {
                let want = reference(kind, &input);
                let mut got = input.clone();
                let mut dct = Dct::new(n);
                if kind == 2 {
                    dct.dct2(&mut got);
                } else {
                    dct.dct3(&mut got);
                }
                let error = rel_error(&got, &want);
                worst = worst.max(error);
                assert!(error < 1e-12, "dct-{kind} n = {n}: error {error:e}");
            }
        }
        println!("f64 DCT-II/III vs reference: worst rel error {worst:e}");
    }

    #[test]
    fn dct4_agrees_with_reference() {
        let mut worst_f64 = 0.0f64;
        let mut worst_f32 = 0.0f64;
        for shift in 1..=13 {
            let n = 1usize << shift;
            let input: Vec<f64> = noise(n, 0x3333 + n as u64);
            let want = reference(4, &input);

            let mut got = input.clone();
            Dct4::new(n).transform(&mut got);
            let error = rel_error(&got, &want);
            worst_f64 = worst_f64.max(error);
            assert!(error < 1e-12, "dct-iv f64 n = {n}: error {error:e}");

            let mut got32: Vec<f32> = input.iter().map(|v| *v as f32).collect();
            Dct4::new(n).transform(&mut got32);
            let widened: Vec<f64> = got32.iter().map(|v| *v as f64).collect();
            let error32 = rel_error(&widened, &want);
            worst_f32 = worst_f32.max(error32);
            assert!(error32 < 1e-5, "dct-iv f32 n = {n}: error {error32:e}");
        }
        println!("DCT-IV vs reference: worst rel error f64 {worst_f64:e}, f32 {worst_f32:e}");
    }

    #[test]
    fn dct2_round_trips_through_dct3() {
        let n = 1024;
        let input: Vec<f32> = noise(n, 5150);
        let mut data = input.clone();
        let mut dct = Dct::new(n);
        dct.dct2(&mut data);
        dct.dct3(&mut data);
        crate::scale(&mut data, 2.0 / n as f32);
        let error = rel_error(&data, &input);
        assert!(error < 1e-6, "round-trip error {error:e}");
        println!("f32 DCT-II/III round trip (n = {n}): rel error {error:e}");
    }
}
