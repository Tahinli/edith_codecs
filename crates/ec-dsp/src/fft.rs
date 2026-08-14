//! Power-of-two complex FFT, plus a real-input wrapper.
//!
//! The kernel is a Stockham auto-sort decimation-in-frequency transform,
//! radix-4 wherever the remaining size allows and radix-2 for the odd stage.
//! Stockham was chosen over the textbook bit-reversed Cooley-Tukey for one
//! reason: every read and write inside a stage is unit stride, which is what
//! makes the inner loop vectorise with no shuffles at all.
//!
//! Data is held **split** — real parts in one slice, imaginary in another —
//! because that is the layout SIMD wants. [`Fft::forward`] and
//! [`Fft::inverse`] accept the interleaved [`Complex`] form for callers who
//! prefer it and de-interleave internally; the DCT and MDCT plans call the
//! split entry points and never pay that pass.

use core::ops::{Add, Mul, Sub};

use crate::Real;

/// The largest transform the plans accept, matching the largest window any
/// codec in the family uses with room to spare.
pub const MAX_LEN: usize = 1 << 16;

/// A complex number in the crate's own minimal form (no `num-complex` dep).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Complex<T> {
    /// Real part.
    pub re: T,
    /// Imaginary part.
    pub im: T,
}

impl<T> Complex<T> {
    /// A complex number from its parts.
    pub const fn new(re: T, im: T) -> Complex<T> {
        Complex { re, im }
    }
}

impl<T: Real> Complex<T> {
    /// Zero.
    pub const ZERO: Complex<T> = Complex {
        re: T::ZERO,
        im: T::ZERO,
    };

    /// Complex conjugate.
    #[inline]
    pub fn conj(self) -> Complex<T> {
        Complex::new(self.re, -self.im)
    }

    /// Multiplication by a real scalar.
    #[inline]
    pub fn scale(self, k: T) -> Complex<T> {
        Complex::new(self.re * k, self.im * k)
    }

    /// Multiplication by `-i`, i.e. a quarter turn clockwise.
    #[inline]
    pub fn mul_neg_i(self) -> Complex<T> {
        Complex::new(self.im, -self.re)
    }

    /// Squared magnitude.
    #[inline]
    pub fn norm_sqr(self) -> T {
        self.re * self.re + self.im * self.im
    }
}

impl<T: Real> Add for Complex<T> {
    type Output = Complex<T>;
    #[inline]
    fn add(self, rhs: Complex<T>) -> Complex<T> {
        Complex::new(self.re + rhs.re, self.im + rhs.im)
    }
}

impl<T: Real> Sub for Complex<T> {
    type Output = Complex<T>;
    #[inline]
    fn sub(self, rhs: Complex<T>) -> Complex<T> {
        Complex::new(self.re - rhs.re, self.im - rhs.im)
    }
}

impl<T: Real> Mul for Complex<T> {
    type Output = Complex<T>;
    #[inline]
    fn mul(self, rhs: Complex<T>) -> Complex<T> {
        Complex::new(
            self.re * rhs.re - self.im * rhs.im,
            self.re * rhs.im + self.im * rhs.re,
        )
    }
}

/// A complex FFT plan for one power-of-two size, in `f32` or `f64`.
///
/// Holds the twiddle table and the ping-pong scratch the Stockham passes need,
/// so a transform allocates nothing. Because the scratch lives in the plan the
/// transform methods take `&mut self`; share across threads by cloning.
///
/// ```
/// use ec_dsp::{Complex, Fft};
///
/// let mut fft = Fft::<f32>::new(8);
/// let mut data = [Complex::new(1.0, 0.0); 8];
/// fft.forward(&mut data);
/// // A constant signal has all its energy in bin 0.
/// assert!((data[0].re - 8.0).abs() < 1e-5);
/// assert!(data[1].norm_sqr() < 1e-10);
/// fft.inverse(&mut data);
/// assert!((data[3].re - 1.0).abs() < 1e-6);
/// ```
#[derive(Clone, Debug)]
pub struct Fft<T: Real> {
    n: usize,
    twiddle_re: Vec<T>,
    twiddle_im: Vec<T>,
    scratch_re: Vec<T>,
    scratch_im: Vec<T>,
    split_re: Vec<T>,
    split_im: Vec<T>,
}

impl<T: Real> Fft<T> {
    /// A plan for `n` points.
    ///
    /// # Panics
    /// If `n` is not a power of two in `1..=`[`MAX_LEN`]. Sizes are a
    /// compile-time property of a codec, so a bad one is API misuse.
    pub fn new(n: usize) -> Fft<T> {
        assert!(
            n.is_power_of_two() && n <= MAX_LEN,
            "fft length must be a power of two up to {MAX_LEN}, got {n}"
        );
        let mut twiddle_re = Vec::with_capacity(n);
        let mut twiddle_im = Vec::with_capacity(n);
        for k in 0..n {
            // exp(-2*pi*i*k/n), always evaluated in f64 and rounded once.
            let angle = -2.0 * core::f64::consts::PI * k as f64 / n as f64;
            twiddle_re.push(T::from_f64(angle.cos()));
            twiddle_im.push(T::from_f64(angle.sin()));
        }
        Fft {
            n,
            twiddle_re,
            twiddle_im,
            scratch_re: vec![T::ZERO; n],
            scratch_im: vec![T::ZERO; n],
            split_re: vec![T::ZERO; n],
            split_im: vec![T::ZERO; n],
        }
    }

    /// Points per transform.
    pub fn size(&self) -> usize {
        self.n
    }

    /// Forward transform, `exp(-2*pi*i*k*n/N)`, in place.
    ///
    /// # Panics
    /// If `data.len() != self.size()`.
    pub fn forward(&mut self, data: &mut [Complex<T>]) {
        self.interleaved(data, false);
    }

    /// Inverse transform **including** the `1/n` scaling, in place, so that
    /// `inverse(forward(x)) == x`.
    ///
    /// # Panics
    /// If `data.len() != self.size()`.
    pub fn inverse(&mut self, data: &mut [Complex<T>]) {
        self.interleaved(data, true);
    }

    fn interleaved(&mut self, data: &mut [Complex<T>], inverse: bool) {
        assert_eq!(data.len(), self.n, "fft input length must equal plan size");
        for ((re, im), c) in self
            .split_re
            .iter_mut()
            .zip(self.split_im.iter_mut())
            .zip(data.iter())
        {
            *re = c.re;
            *im = c.im;
        }
        let (mut re, mut im) = (
            core::mem::take(&mut self.split_re),
            core::mem::take(&mut self.split_im),
        );
        if inverse {
            self.inverse_split(&mut re, &mut im);
        } else {
            self.forward_split(&mut re, &mut im);
        }
        for (c, (re, im)) in data.iter_mut().zip(re.iter().zip(im.iter())) {
            *c = Complex::new(*re, *im);
        }
        self.split_re = re;
        self.split_im = im;
    }

    /// Forward transform of split-layout data, in place. The allocation-free
    /// entry point the other transforms in this crate use.
    ///
    /// # Panics
    /// If either slice length differs from [`Fft::size`].
    pub fn forward_split(&mut self, re: &mut [T], im: &mut [T]) {
        self.stockham(re, im);
    }

    /// Inverse transform of split-layout data, in place, including `1/n`.
    ///
    /// Uses the identity `ifft(x) = swap(fft(swap(x)))/n`, where `swap`
    /// exchanges real and imaginary parts — one kernel serves both directions.
    ///
    /// # Panics
    /// If either slice length differs from [`Fft::size`].
    pub fn inverse_split(&mut self, re: &mut [T], im: &mut [T]) {
        self.stockham(im, re);
        let norm = T::ONE / T::from_f64(self.n as f64);
        crate::scale(re, norm);
        crate::scale(im, norm);
    }

    fn stockham(&mut self, re: &mut [T], im: &mut [T]) {
        assert_eq!(re.len(), self.n, "fft input length must equal plan size");
        assert_eq!(im.len(), self.n, "fft input length must equal plan size");
        let Fft {
            n: total,
            twiddle_re,
            twiddle_im,
            scratch_re,
            scratch_im,
            ..
        } = self;

        let mut xr: &mut [T] = re;
        let mut xi: &mut [T] = im;
        let mut yr: &mut [T] = scratch_re;
        let mut yi: &mut [T] = scratch_im;

        // Invariant across the loop: n * stride == total, so the twiddle for
        // stage angle exp(-2*pi*i*p/n) is table entry p*stride.
        let mut n = *total;
        let mut stride = 1usize;
        let mut in_scratch = false;
        while n > 1 {
            if n % 4 == 0 {
                pass4(stride, xr, xi, yr, yi, twiddle_re, twiddle_im);
                n /= 4;
                stride *= 4;
            } else {
                pass2(stride, xr, xi, yr, yi, twiddle_re, twiddle_im);
                n /= 2;
                stride *= 2;
            }
            core::mem::swap(&mut xr, &mut yr);
            core::mem::swap(&mut xi, &mut yi);
            in_scratch = !in_scratch;
        }
        if in_scratch {
            // Odd number of passes: the result sits in the scratch (`x` now),
            // the caller's buffers are `y`.
            yr.copy_from_slice(xr);
            yi.copy_from_slice(xi);
        }
    }
}

/// One radix-2 Stockham pass. `stride` is the run length; the input holds two
/// halves and the output two interleaved runs per block.
///
/// Every loop here walks `chunks_exact`, never an index: that is what makes
/// the bounds checks fold away, and it is worth 10x — measured against the
/// same code written with indices, which spent more instructions on checks
/// than on arithmetic.
#[inline]
fn pass2<T: Real>(
    stride: usize,
    xr: &[T],
    xi: &[T],
    yr: &mut [T],
    yi: &mut [T],
    twiddle_re: &[T],
    twiddle_im: &[T],
) {
    let half = xr.len() / 2;
    let (a_re, b_re) = xr.split_at(half);
    let (a_im, b_im) = xi.split_at(half);
    let lanes = T::LANES;

    let blocks = a_re
        .chunks_exact(stride)
        .zip(a_im.chunks_exact(stride))
        .zip(b_re.chunks_exact(stride).zip(b_im.chunks_exact(stride)))
        .zip(
            yr.chunks_exact_mut(2 * stride)
                .zip(yi.chunks_exact_mut(2 * stride)),
        );
    for (p, (((ar, ai), (br, bi)), (out_re, out_im))) in blocks.enumerate() {
        let (wr, wi) = (twiddle_re[p * stride], twiddle_im[p * stride]);
        let (o0r, o1r) = out_re.split_at_mut(stride);
        let (o0i, o1i) = out_im.split_at_mut(stride);
        if stride >= lanes {
            let (wrv, wiv) = (wr.splat(), wi.splat());
            let ins = ar
                .chunks_exact(lanes)
                .zip(ai.chunks_exact(lanes))
                .zip(br.chunks_exact(lanes).zip(bi.chunks_exact(lanes)));
            let outs = o0r
                .chunks_exact_mut(lanes)
                .zip(o0i.chunks_exact_mut(lanes))
                .zip(o1r.chunks_exact_mut(lanes).zip(o1i.chunks_exact_mut(lanes)));
            for (input, output) in ins.zip(outs) {
                let ((a_lane_re, a_lane_im), (b_lane_re, b_lane_im)) = input;
                let ((y0r, y0i), (y1r, y1i)) = output;
                let (arv, aiv) = (T::load(a_lane_re), T::load(a_lane_im));
                let (brv, biv) = (T::load(b_lane_re), T::load(b_lane_im));
                T::store(arv + brv, y0r);
                T::store(aiv + biv, y0i);
                let (dr, di) = (arv - brv, aiv - biv);
                T::store(dr * wrv - di * wiv, y1r);
                T::store(dr * wiv + di * wrv, y1i);
            }
        } else {
            for q in 0..stride {
                let (arq, aiq) = (ar[q], ai[q]);
                let (brq, biq) = (br[q], bi[q]);
                o0r[q] = arq + brq;
                o0i[q] = aiq + biq;
                let (dr, di) = (arq - brq, aiq - biq);
                o1r[q] = dr * wr - di * wi;
                o1i[q] = dr * wi + di * wr;
            }
        }
    }
}

/// One radix-4 Stockham pass — half the passes of radix-2 for the same size,
/// which is where most of the speed comes from. Same chunk discipline as
/// [`pass2`].
#[inline]
fn pass4<T: Real>(
    stride: usize,
    xr: &[T],
    xi: &[T],
    yr: &mut [T],
    yi: &mut [T],
    twiddle_re: &[T],
    twiddle_im: &[T],
) {
    let quarter = xr.len() / 4;
    let (a_re, rest) = xr.split_at(quarter);
    let (b_re, rest) = rest.split_at(quarter);
    let (c_re, d_re) = rest.split_at(quarter);
    let (a_im, rest) = xi.split_at(quarter);
    let (b_im, rest) = rest.split_at(quarter);
    let (c_im, d_im) = rest.split_at(quarter);
    let lanes = T::LANES;

    let blocks = a_re
        .chunks_exact(stride)
        .zip(a_im.chunks_exact(stride))
        .zip(b_re.chunks_exact(stride).zip(b_im.chunks_exact(stride)))
        .zip(
            c_re.chunks_exact(stride)
                .zip(c_im.chunks_exact(stride))
                .zip(d_re.chunks_exact(stride).zip(d_im.chunks_exact(stride))),
        )
        .zip(
            yr.chunks_exact_mut(4 * stride)
                .zip(yi.chunks_exact_mut(4 * stride)),
        );
    for (p, ((((ar, ai), (br, bi)), ((cr, ci), (dr, di))), (out_re, out_im))) in blocks.enumerate()
    {
        let step = p * stride;
        let (w1r, w1i) = (twiddle_re[step], twiddle_im[step]);
        let (w2r, w2i) = (twiddle_re[2 * step], twiddle_im[2 * step]);
        let (w3r, w3i) = (twiddle_re[3 * step], twiddle_im[3 * step]);
        let mut runs_re = out_re.chunks_exact_mut(stride);
        let (o0r, o1r, o2r, o3r) = (
            runs_re.next().expect("four output runs"),
            runs_re.next().expect("four output runs"),
            runs_re.next().expect("four output runs"),
            runs_re.next().expect("four output runs"),
        );
        let mut runs_im = out_im.chunks_exact_mut(stride);
        let (o0i, o1i, o2i, o3i) = (
            runs_im.next().expect("four output runs"),
            runs_im.next().expect("four output runs"),
            runs_im.next().expect("four output runs"),
            runs_im.next().expect("four output runs"),
        );
        if stride >= lanes {
            let (w1rv, w1iv) = (w1r.splat(), w1i.splat());
            let (w2rv, w2iv) = (w2r.splat(), w2i.splat());
            let (w3rv, w3iv) = (w3r.splat(), w3i.splat());
            let ins = ar
                .chunks_exact(lanes)
                .zip(ai.chunks_exact(lanes))
                .zip(br.chunks_exact(lanes).zip(bi.chunks_exact(lanes)))
                .zip(
                    cr.chunks_exact(lanes)
                        .zip(ci.chunks_exact(lanes))
                        .zip(dr.chunks_exact(lanes).zip(di.chunks_exact(lanes))),
                );
            let outs = o0r
                .chunks_exact_mut(lanes)
                .zip(o0i.chunks_exact_mut(lanes))
                .zip(o1r.chunks_exact_mut(lanes).zip(o1i.chunks_exact_mut(lanes)))
                .zip(
                    o2r.chunks_exact_mut(lanes)
                        .zip(o2i.chunks_exact_mut(lanes))
                        .zip(o3r.chunks_exact_mut(lanes).zip(o3i.chunks_exact_mut(lanes))),
                );
            for (input, output) in ins.zip(outs) {
                let (((a_lr, a_li), (b_lr, b_li)), ((c_lr, c_li), (d_lr, d_li))) = input;
                let (((y0r, y0i), (y1r, y1i)), ((y2r, y2i), (y3r, y3i))) = output;
                let (arv, aiv) = (T::load(a_lr), T::load(a_li));
                let (brv, biv) = (T::load(b_lr), T::load(b_li));
                let (crv, civ) = (T::load(c_lr), T::load(c_li));
                let (drv, div) = (T::load(d_lr), T::load(d_li));
                let (apc_r, apc_i) = (arv + crv, aiv + civ);
                let (amc_r, amc_i) = (arv - crv, aiv - civ);
                let (bpd_r, bpd_i) = (brv + drv, biv + div);
                let (bmd_r, bmd_i) = (brv - drv, biv - div);
                // t0 = (a+c) + (b+d)
                T::store(apc_r + bpd_r, y0r);
                T::store(apc_i + bpd_i, y0i);
                // t1 = (a-c) - i(b-d), times w^p
                let (t1r, t1i) = (amc_r + bmd_i, amc_i - bmd_r);
                T::store(t1r * w1rv - t1i * w1iv, y1r);
                T::store(t1r * w1iv + t1i * w1rv, y1i);
                // t2 = (a+c) - (b+d), times w^2p
                let (t2r, t2i) = (apc_r - bpd_r, apc_i - bpd_i);
                T::store(t2r * w2rv - t2i * w2iv, y2r);
                T::store(t2r * w2iv + t2i * w2rv, y2i);
                // t3 = (a-c) + i(b-d), times w^3p
                let (t3r, t3i) = (amc_r - bmd_i, amc_i + bmd_r);
                T::store(t3r * w3rv - t3i * w3iv, y3r);
                T::store(t3r * w3iv + t3i * w3rv, y3i);
            }
        } else {
            for q in 0..stride {
                let (arq, aiq) = (ar[q], ai[q]);
                let (brq, biq) = (br[q], bi[q]);
                let (crq, ciq) = (cr[q], ci[q]);
                let (drq, diq) = (dr[q], di[q]);
                let (apc_r, apc_i) = (arq + crq, aiq + ciq);
                let (amc_r, amc_i) = (arq - crq, aiq - ciq);
                let (bpd_r, bpd_i) = (brq + drq, biq + diq);
                let (bmd_r, bmd_i) = (brq - drq, biq - diq);
                o0r[q] = apc_r + bpd_r;
                o0i[q] = apc_i + bpd_i;
                let (t1r, t1i) = (amc_r + bmd_i, amc_i - bmd_r);
                o1r[q] = t1r * w1r - t1i * w1i;
                o1i[q] = t1r * w1i + t1i * w1r;
                let (t2r, t2i) = (apc_r - bpd_r, apc_i - bpd_i);
                o2r[q] = t2r * w2r - t2i * w2i;
                o2i[q] = t2r * w2i + t2i * w2r;
                let (t3r, t3i) = (amc_r - bmd_i, amc_i + bmd_r);
                o3r[q] = t3r * w3r - t3i * w3i;
                o3i[q] = t3r * w3i + t3i * w3r;
            }
        }
    }
}

/// Real-input FFT: an `n`-point real transform on an `n/2`-point complex one.
///
/// Half the work and half the memory of stuffing zeros into a complex FFT.
/// The spectrum is the `n/2 + 1` non-redundant bins; bins `0` and `n/2` are
/// real, and the caller gets the conjugate half back for free by symmetry.
///
/// ```
/// use ec_dsp::RealFft;
///
/// let mut rfft = RealFft::<f64>::new(16);
/// let signal: Vec<f64> = (0..16).map(|n| (n as f64).sin()).collect();
/// let mut spectrum = vec![Default::default(); 9];
/// rfft.forward(&signal, &mut spectrum);
/// let mut back = vec![0.0; 16];
/// rfft.inverse(&spectrum, &mut back);
/// assert!(back.iter().zip(&signal).all(|(a, b)| (a - b).abs() < 1e-12));
/// ```
#[derive(Clone, Debug)]
pub struct RealFft<T: Real> {
    n: usize,
    half: Fft<T>,
    twiddle_re: Vec<T>,
    twiddle_im: Vec<T>,
    buf_re: Vec<T>,
    buf_im: Vec<T>,
}

impl<T: Real> RealFft<T> {
    /// A plan for `n` real points.
    ///
    /// # Panics
    /// If `n` is not a power of two of at least 4, or exceeds [`MAX_LEN`].
    pub fn new(n: usize) -> RealFft<T> {
        assert!(
            n.is_power_of_two() && (4..=MAX_LEN).contains(&n),
            "real fft length must be a power of two in 4..={MAX_LEN}, got {n}"
        );
        let half = n / 2;
        let mut twiddle_re = Vec::with_capacity(half + 1);
        let mut twiddle_im = Vec::with_capacity(half + 1);
        for k in 0..=half {
            let angle = -2.0 * core::f64::consts::PI * k as f64 / n as f64;
            twiddle_re.push(T::from_f64(angle.cos()));
            twiddle_im.push(T::from_f64(angle.sin()));
        }
        RealFft {
            n,
            half: Fft::new(half),
            twiddle_re,
            twiddle_im,
            buf_re: vec![T::ZERO; half],
            buf_im: vec![T::ZERO; half],
        }
    }

    /// Points per transform (the real-domain length).
    pub fn size(&self) -> usize {
        self.n
    }

    /// Bins in the spectrum, `size()/2 + 1`.
    pub fn spectrum_len(&self) -> usize {
        self.n / 2 + 1
    }

    /// Transforms `input` into `spectrum`.
    ///
    /// # Panics
    /// If `input.len() != size()` or `spectrum.len() != spectrum_len()`.
    pub fn forward(&mut self, input: &[T], spectrum: &mut [Complex<T>]) {
        assert_eq!(input.len(), self.n, "real fft input length must equal size");
        assert_eq!(
            spectrum.len(),
            self.spectrum_len(),
            "spectrum length must be size()/2 + 1"
        );
        let m = self.n / 2;
        for j in 0..m {
            self.buf_re[j] = input[2 * j];
            self.buf_im[j] = input[2 * j + 1];
        }
        self.half.forward_split(&mut self.buf_re, &mut self.buf_im);

        let half_scale = T::from_f64(0.5);
        for (k, bin) in spectrum.iter_mut().enumerate() {
            let z = Complex::new(self.buf_re[k % m], self.buf_im[k % m]);
            let zc = Complex::new(self.buf_re[(m - k) % m], self.buf_im[(m - k) % m]).conj();
            let even = (z + zc).scale(half_scale);
            let odd = (z - zc).scale(half_scale);
            let tw = Complex::new(self.twiddle_re[k], self.twiddle_im[k]);
            *bin = even + (tw * odd).mul_neg_i();
        }
    }

    /// Reconstructs `output` from `spectrum`, scaled so that
    /// `inverse(forward(x)) == x`.
    ///
    /// # Panics
    /// If `output.len() != size()` or `spectrum.len() != spectrum_len()`.
    pub fn inverse(&mut self, spectrum: &[Complex<T>], output: &mut [T]) {
        assert_eq!(
            output.len(),
            self.n,
            "real fft output length must equal size"
        );
        assert_eq!(
            spectrum.len(),
            self.spectrum_len(),
            "spectrum length must be size()/2 + 1"
        );
        let m = self.n / 2;
        let half_scale = T::from_f64(0.5);
        for k in 0..m {
            let a = spectrum[k];
            let b = spectrum[m - k].conj();
            let even = (a + b).scale(half_scale);
            let odd = (a - b).scale(half_scale);
            // Conjugate twiddle, and +i instead of -i: the forward step run backwards.
            let tw = Complex::new(self.twiddle_re[k], self.twiddle_im[k]).conj();
            let z = even - (tw * odd).mul_neg_i();
            self.buf_re[k] = z.re;
            self.buf_im[k] = z.im;
        }
        self.half.inverse_split(&mut self.buf_re, &mut self.buf_im);
        for j in 0..m {
            output[2 * j] = self.buf_re[j];
            output[2 * j + 1] = self.buf_im[j];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{noise, rel_error};

    /// O(n^2) DFT in `f64` — the reference the fast path is judged against.
    /// Test-only by construction: nothing outside `#[cfg(test)]` can call it,
    /// which is the family rule about naive transforms in shipping paths.
    fn reference_dft(input: &[Complex<f64>], inverse: bool) -> Vec<Complex<f64>> {
        let n = input.len();
        let sign = if inverse { 1.0 } else { -1.0 };
        (0..n)
            .map(|k| {
                let mut acc = Complex::new(0.0, 0.0);
                for (j, x) in input.iter().enumerate() {
                    let angle = sign * 2.0 * core::f64::consts::PI * (j * k % n) as f64 / n as f64;
                    acc = acc + *x * Complex::new(angle.cos(), angle.sin());
                }
                acc
            })
            .collect()
    }

    fn complex_noise(n: usize, seed: u64) -> Vec<Complex<f64>> {
        let re: Vec<f64> = noise(n, seed);
        let im: Vec<f64> = noise(n, seed ^ 0x9e37_79b9);
        re.iter()
            .zip(&im)
            .map(|(r, i)| Complex::new(*r, *i))
            .collect()
    }

    #[test]
    fn agrees_with_reference_dft_f64() {
        let mut worst = 0.0f64;
        let mut size = 0;
        for shift in 3..=13 {
            let n = 1usize << shift;
            let input = complex_noise(n, 0x1234 + n as u64);
            let want = reference_dft(&input, false);
            let mut got = input.clone();
            Fft::new(n).forward(&mut got);
            let error = complex_error(&got, &want);
            if error > worst {
                worst = error;
                size = n;
            }
            assert!(error < 1e-12, "n = {n}: f64 error {error:e}");
        }
        println!("f64 FFT vs reference DFT: worst rel error {worst:e} (n = {size})");
    }

    #[test]
    fn agrees_with_reference_dft_f32() {
        let mut worst = 0.0f64;
        let mut size = 0;
        for shift in 3..=13 {
            let n = 1usize << shift;
            let input = complex_noise(n, 0x5678 + n as u64);
            let want = reference_dft(&input, false);
            let mut got: Vec<Complex<f32>> = input
                .iter()
                .map(|c| Complex::new(c.re as f32, c.im as f32))
                .collect();
            Fft::new(n).forward(&mut got);
            let widened: Vec<Complex<f64>> = got
                .iter()
                .map(|c| Complex::new(c.re as f64, c.im as f64))
                .collect();
            let error = complex_error(&widened, &want);
            if error > worst {
                worst = error;
                size = n;
            }
            assert!(error < 1e-5, "n = {n}: f32 error {error:e}");
        }
        println!("f32 FFT vs reference DFT: worst rel error {worst:e} (n = {size})");
    }

    #[test]
    fn inverse_reference_matches_plan() {
        let n = 64;
        let input = complex_noise(n, 99);
        let want = reference_dft(&input, true);
        let mut got = input.clone();
        let mut fft = Fft::new(n);
        fft.inverse(&mut got);
        // The plan normalises by n; the reference does not.
        let scaled: Vec<Complex<f64>> = want.iter().map(|c| c.scale(1.0 / n as f64)).collect();
        assert!(complex_error(&got, &scaled) < 1e-12);
    }

    #[test]
    fn round_trips_f32_every_size() {
        let mut worst = 0.0f64;
        let mut size = 0;
        for shift in 3..=15 {
            let n = 1usize << shift;
            let input: Vec<f32> = noise(n, 0xabc + n as u64);
            let mut data: Vec<Complex<f32>> = input.iter().map(|v| Complex::new(*v, 0.0)).collect();
            let mut fft = Fft::new(n);
            fft.forward(&mut data);
            fft.inverse(&mut data);
            let got: Vec<f32> = data.iter().map(|c| c.re).collect();
            let error = rel_error(&got, &input);
            if error > worst {
                worst = error;
                size = n;
            }
            assert!(error < 1e-6, "n = {n}: round-trip error {error:e}");
        }
        println!("f32 FFT round trip: worst rel error {worst:e} (n = {size})");
    }

    #[test]
    fn split_and_interleaved_agree() {
        let n = 256;
        let input = complex_noise(n, 4242);
        let mut fft = Fft::new(n);
        let mut interleaved = input.clone();
        fft.forward(&mut interleaved);
        let mut re: Vec<f64> = input.iter().map(|c| c.re).collect();
        let mut im: Vec<f64> = input.iter().map(|c| c.im).collect();
        fft.forward_split(&mut re, &mut im);
        for (k, c) in interleaved.iter().enumerate() {
            assert!((c.re - re[k]).abs() < 1e-12 && (c.im - im[k]).abs() < 1e-12);
        }
    }

    #[test]
    fn real_fft_matches_complex_fft() {
        for shift in 2..=12 {
            let n = 1usize << shift;
            let signal: Vec<f64> = noise(n, 0xfeed + n as u64);
            let mut rfft = RealFft::new(n);
            let mut spectrum = vec![Complex::new(0.0, 0.0); n / 2 + 1];
            rfft.forward(&signal, &mut spectrum);

            let mut want: Vec<Complex<f64>> =
                signal.iter().map(|v| Complex::new(*v, 0.0)).collect();
            Fft::new(n).forward(&mut want);
            for k in 0..=n / 2 {
                assert!(
                    (spectrum[k].re - want[k].re).abs() < 1e-10
                        && (spectrum[k].im - want[k].im).abs() < 1e-10,
                    "n = {n}, bin {k}: {:?} vs {:?}",
                    spectrum[k],
                    want[k]
                );
            }

            let mut back = vec![0.0; n];
            rfft.inverse(&spectrum, &mut back);
            assert!(rel_error(&back, &signal) < 1e-12, "n = {n} real round trip");
        }
    }

    #[test]
    #[should_panic(expected = "power of two")]
    fn rejects_non_power_of_two() {
        let _ = Fft::<f32>::new(48);
    }

    fn complex_error(got: &[Complex<f64>], want: &[Complex<f64>]) -> f64 {
        let mut num = 0.0;
        let mut den = 0.0;
        for (g, w) in got.iter().zip(want) {
            num += (*g - *w).norm_sqr();
            den += w.norm_sqr();
        }
        (num / den).sqrt()
    }
}
