//! The CELT encoder — RFC 6716 Section 5.3, throughput-first.
//!
//! The *coding* path — every symbol written, every `tell()` the bit allocation
//! reads — mirrors the normative reference (RFC 6716 Appendix A) exactly,
//! because the decoder derives the allocation from the same arithmetic and one
//! bit of drift desynchronises the frame. The *analysis* path is where this
//! encoder buys its speed, with fast deterministic heuristics in place of the
//! reference's searches:
//!
//! - **No pitch prefilter.** The postfilter-off bit is coded every frame,
//!   which removes the most expensive analysis in the reference encoder (an
//!   autocorrelation search over 1024 lags). Same choice the reference makes
//!   below complexity 5.
//! - **Time/frequency Viterbi ported but gated off.** `tf_analysis()` and the
//!   libopus float-mode `transient_analysis()` (returning `tf_estimate`/
//!   `tf_chan`) are ported from the reference; `const TF_ANALYSIS: bool`
//!   gates the Viterbi search. It is `false`: the 2-row gate (sadie/hein @64k)
//!   vs the no-analysis baseline showed both corr gaps grow and minsec drop,
//!   so the search is disabled. With the flag off, `tf_res` falls back to the
//!   reference's no-analysis default — raw `is_transient` per band, which under
//!   `tf_select = 0` codes "no tf change" (long frames keep full frequency
//!   resolution; transient frames keep their short blocks).
//! - **Fixed spreading.** `SPREAD_NORMAL` every frame, coded explicitly.
//! - **Single-pass coarse energy** with the reference's own delayed-intra
//!   heuristic, not the two-pass encode-both-and-compare.
//!
//! What stays: transient detection, the closed-form allocation-trim and
//! dynalloc spike heuristics, the stereo-mode analysis, intensity thresholds
//! and constrained-VBR reservoir — all cheap O(frame) arithmetic that decides
//! *rates*, not searches.
//!
//! Steady state allocates nothing: every buffer is owned by the encoder and
//! sized at construction.

use ec_core::{Error, Result};

use crate::celt::{
    self, BETA_COEF, BETA_INTRA, BITRES, CACHE_CAPS, E_BANDS, E_MEANS, E_PROB_MODEL, LOG_N,
    MAX_FINE_BITS, NB_BANDS, OVERLAP, PRED_COEF, PREEMPH, QTHETA_OFFSET, QTHETA_OFFSET_TWOPHASE,
    SHORT_MDCT, SIG_SCALE, SMALL_ENERGY_ICDF, SPREAD_AGGRESSIVE, SPREAD_ICDF, SPREAD_LIGHT,
    SPREAD_NONE, SPREAD_NORMAL, TF_SELECT, TRIM_ICDF,
    bitexact_cos, bitexact_log2tan, bits2pulses, cache_index, compute_allocation_encode,
    compute_qn, deinterleave_hadamard, exp_rotation, frac_mul16, get_pulses, haar1, pulses2bits,
    unext,
};
use crate::range::RangeEncoder;

const CACHE_BITS: &[u8] = &crate::celt::CACHE_BITS;
const INV_TABLE: [u8; 128] = [
    255,255,156,110, 86, 70, 59, 51, 45, 40, 37, 33, 31, 28, 26, 25,
     23, 22, 21, 20, 19, 18, 17, 16, 16, 15, 15, 14, 13, 13, 12, 12,
     12, 12, 11, 11, 11, 10, 10, 10,  9,  9,  9,  9,  9,  9,  8,  8,
      8,  8,  8,  7,  7,  7,  7,  7,  7,  6,  6,  6,  6,  6,  6,  6,
      6,  6,  6,  6,  6,  6,  6,  6,  6,  5,  5,  5,  5,  5,  5,  5,
      5,  5,  5,  5,  5,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,
      4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  4,  3,  3,
      3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  2,
];

// ---------------------------------------------------------------------------
// Forward MDCT
// ---------------------------------------------------------------------------

/// One forward-MDCT plan, the exact adjoint of the decoder's `ImdctPlan`:
/// window-fold, pre-rotation, an `l/2`-point FFT and post-rotation, scaled by
/// `2/l` so that decode(encode(x)) reconstructs at unit gain.
#[derive(Clone, Debug)]
struct MdctPlan {
    l: usize,
    fft: celt::Fft15,
    rot_re: Vec<f32>,
    rot_im: Vec<f32>,
    fre: Vec<f32>,
    fim: Vec<f32>,
    t: Vec<f32>,
}

impl MdctPlan {
    fn new(l: usize) -> MdctPlan {
        let n = 2 * l;
        let quarter = l / 2;
        let mut rot_re = vec![0.0; quarter];
        let mut rot_im = vec![0.0; quarter];
        for i in 0..quarter {
            let a = 2.0 * core::f64::consts::PI * (i as f64 + 0.125) / n as f64;
            rot_re[i] = a.cos() as f32;
            rot_im[i] = a.sin() as f32;
        }
        MdctPlan {
            l,
            fft: celt::Fft15::new(quarter),
            rot_re,
            rot_im,
            fre: vec![0.0; quarter],
            fim: vec![0.0; quarter],
            t: vec![0.0; l],
        }
    }

    /// Transforms `l + OVERLAP` time samples starting at `input[off]` — the
    /// support of the low-overlap window — into `l` MDCT coefficients written
    /// at `out[out_off + k*stride]`.
    fn forward(
        &mut self,
        input: &[f32],
        off: usize,
        window: &[f32],
        out: &mut [f32],
        out_off: usize,
        stride: usize,
    ) {
        let l = self.l;
        let quarter = l / 2;
        let flat = quarter - OVERLAP / 2;
        let mirror = quarter + OVERLAP / 2;
        let t = &mut self.t;
        // Window fold: the adjoint of the inverse's scatter, so every
        // coefficient, sign and window position matches by construction.
        for i in 0..flat {
            t[quarter - 1 - i] = input[off + mirror - 1 - i];
        }
        for i in flat..quarter {
            let w = i - flat;
            t[quarter - 1 - i] =
                -window[w] * input[off + w] + window[OVERLAP - 1 - w] * input[off + mirror - 1 - i];
        }
        for i in 0..flat {
            t[quarter + i] = input[off + mirror + i];
        }
        for i in flat..quarter {
            let w = i - flat;
            t[quarter + i] = window[w] * input[off + l + OVERLAP - 1 - w]
                + window[OVERLAP - 1 - w] * input[off + mirror + i];
        }
        // Shuffle into the complex FFT input.
        for i in 0..quarter {
            self.fre[i] = -t[2 * i];
            self.fim[i] = t[2 * (quarter - 1 - i) + 1];
        }
        // Rotate by -theta (adjoint of the inverse's post-rotation).
        for i in 0..quarter {
            let (c, s) = (self.rot_re[i], self.rot_im[i]);
            let (re, im) = (self.fre[i], self.fim[i]);
            self.fre[i] = re * c + im * s;
            self.fim[i] = im * c - re * s;
        }
        // Forward FFT = conjugate, +i-convention FFT, conjugate.
        for v in self.fim.iter_mut() {
            *v = -*v;
        }
        self.fft.inverse(&mut self.fre, &mut self.fim);
        for v in self.fim.iter_mut() {
            *v = -*v;
        }
        // Adjoint of the pre-rotation, with the 2/l forward scaling CELT puts
        // on this side of the transform.
        let scale = 2.0 / l as f32;
        for i in 0..quarter {
            let (c, s) = (self.rot_re[i], self.rot_im[i]);
            let (re, im) = (self.fre[i], self.fim[i]);
            out[out_off + (2 * i) * stride] = scale * (s * re - c * im);
            out[out_off + (l - 1 - 2 * i) * stride] = scale * (-c * re - s * im);
        }
    }
}

// ---------------------------------------------------------------------------
// Symbol-level helpers
// ---------------------------------------------------------------------------

/// `ec_laplace_encode()`: the coarse-energy prediction error. May clamp
/// `value`, which is why it is passed back.
fn laplace_encode(enc: &mut RangeEncoder, value: &mut i32, mut fs: u32, decay: i32) -> i32 {
    const MINP: u32 = 1;
    const NMIN: u32 = 16;
    let mut fl = 0u32;
    let mut val = *value;
    if val != 0 {
        let s = if val < 0 { -1i32 } else { 0 };
        val = (val + s) ^ s;
        fl = fs;
        let fs0 = fs;
        fs = ((32768 - MINP * (2 * NMIN) - fs0) * (16384 - decay as u32)) >> 15;
        // Search the decaying part of the pdf.
        let mut i = 1i32;
        while fs > 0 && i < val {
            fs *= 2;
            fl += fs + 2 * MINP;
            fs = (fs * decay as u32) >> 15;
            i += 1;
        }
        if fs == 0 {
            // Everything beyond has probability MINP.
            let mut ndi_max = (32768 - fl + MINP - 1) as i32;
            ndi_max = (ndi_max - s) >> 1;
            let di = (val - i).min(ndi_max - 1);
            fl += ((2 * di + 1 + s) as u32) * MINP;
            fs = MINP.min(32768 - fl);
            *value = (i + di + s) ^ s;
        } else {
            fs += MINP;
            if s == 0 {
                fl += fs;
            }
        }
        debug_assert!(fl + fs <= 32768);
        debug_assert!(fs > 0);
    }
    enc.encode_bin(fl, fl + fs, 15);
    *value
}

/// `encode_pulses()` for the general case: index the pulse vector in the
/// V(N,K) enumeration and code it as one uniform symbol (Section 5.3.4).
fn encode_pulses(enc: &mut RangeEncoder, iy: &[i32], n: usize, k: usize, u: &mut [u32]) {
    debug_assert!(n >= 2 && k > 0);
    u[0] = 0;
    for (kk, slot) in u.iter_mut().enumerate().take(k + 2).skip(1) {
        *slot = (2 * kk - 1) as u32;
    }
    let mut i = u32::from(iy[n - 1] < 0);
    let mut kk = iy[n - 1].unsigned_abs() as usize;
    let j = n - 2;
    i = i.wrapping_add(u[kk]);
    kk += iy[j].unsigned_abs() as usize;
    if iy[j] < 0 {
        i = i.wrapping_add(u[kk + 1]);
    }
    let mut j = j;
    while j > 0 {
        j -= 1;
        unext(&mut u[..k + 2], 0);
        i = i.wrapping_add(u[kk]);
        kk += iy[j].unsigned_abs() as usize;
        if iy[j] < 0 {
            i = i.wrapping_add(u[kk + 1]);
        }
    }
    let nc = u[kk].wrapping_add(u[kk + 1]);
    enc.enc_uint(i, nc.max(2));
}

/// `stereo_itheta()`: the angle between mid and side (or the two halves of a
/// split mono band), in Q14 (0..16384).
fn stereo_itheta(x: &[f32], y: &[f32], stereo: bool, n: usize) -> i32 {
    let mut emid = 1e-15f32;
    let mut eside = 1e-15f32;
    if stereo {
        for j in 0..n {
            let m = x[j] + y[j];
            let s = x[j] - y[j];
            emid += m * m;
            eside += s * s;
        }
    } else {
        for j in 0..n {
            emid += x[j] * x[j];
            eside += y[j] * y[j];
        }
    }
    let mid = emid.sqrt();
    let side = eside.sqrt();
    // 0.63662 is the reference's own 2/pi literal (vq.c, stereo_itheta); the
    // exact constant would round a boundary theta differently.
    #[allow(clippy::approx_constant)]
    const TWO_OVER_PI: f64 = 0.63662;
    (0.5 + 16384.0 * TWO_OVER_PI * (side as f64).atan2(mid as f64)).floor() as i32
}

/// `l1_metric()` from the reference: L1 norm scaled by `(1 + LM*bias)`.
/// In float mode `MAC16_32_Q15(c, a*b) = c + a*b`, so this is `L1*(1 + LM*bias)`.
fn l1_metric(tmp: &[f32], n: usize, lm: usize, bias: f32) -> f32 {
    let mut l1 = 0.0f32;
    for i in 0..n {
        l1 += tmp[i].abs();
    }
    l1 * (1.0 + lm as f32 * bias)
}

// ---------------------------------------------------------------------------
// The encoder
// ---------------------------------------------------------------------------

/// Arguments threaded through the band-encode recursion; the encode-side
/// mirror of the decoder's `BandArgs`, minus everything only resynthesis
/// needs (folding sources, gains, collapse fill).
#[derive(Clone, Copy, Debug)]
struct EncBand {
    i: usize,
    x: usize,
    y: Option<usize>,
    n: usize,
    b: i32,
    spread: usize,
    blocks: usize,
    intensity: usize,
    tf_change: i32,
    level: i32,
    lm: i32,
}

/// Per-frame encoder diagnostics for the dropout investigation. Captured at
/// the end of [`CeltEncoder::encode`]; read via [`CeltEncoder::last_diag`].
#[derive(Clone, Debug)]
pub struct CeltFrameDiag {
    /// Transient flag as coded into the bitstream.
    pub is_transient: bool,
    /// `short_blocks` (0 = long, else `m` short blocks).
    pub short_blocks: usize,
    /// Coarse-energy intra-frame reset was used.
    pub intra: bool,
    /// The whole frame was coded as silence.
    pub silence: bool,
    /// Frame-size exponent (`120 << lm`).
    pub lm: usize,
    /// First/last coded band indices.
    pub start: usize,
    /// Highest band actually allocated pulses.
    pub coded_bands: usize,
    /// Stereo intensity band (bands >= this collapse to mono).
    pub intensity: usize,
    /// Dual-stereo (no mid/side folding) flag.
    pub dual_stereo: bool,
    /// Allocation trim index (0..10).
    pub alloc_trim: i32,
    /// Constrained-VBR reservoir level after this frame, in 1/8-bit units.
    pub vbr_reservoir: i32,
    /// Final compressed payload of this frame, in bytes.
    pub nb_compressed: usize,
    /// Per-band log2 energy minus `E_MEANS`, both channels (ch0 then ch1).
    pub band_log_e: [f32; 2 * NB_BANDS],
    /// Per-band allocated pulses (ch0/ch1 share for intensity bands).
    pub pulses: [i32; NB_BANDS],
    /// Per-band fine-energy precision bits.
    pub fine_quant: [i32; NB_BANDS],
}

impl Default for CeltFrameDiag {
    fn default() -> Self {
        Self {
            is_transient: false,
            short_blocks: 0,
            intra: false,
            silence: false,
            lm: 0,
            start: 0,
            coded_bands: 0,
            intensity: 0,
            dual_stereo: false,
            alloc_trim: 0,
            vbr_reservoir: 0,
            nb_compressed: 0,
            band_log_e: [0.0; 2 * NB_BANDS],
            pulses: [0; NB_BANDS],
            fine_quant: [0; NB_BANDS],
        }
    }
}

/// A CELT-layer encoder for one elementary Opus stream (mono or stereo).
/// CELT itself always runs at 48 kHz; slower input is zero-stuffed up to it.
#[derive(Clone, Debug)]
pub struct CeltEncoder {
    channels: usize,
    /// 48000 / input rate, 1 for native 48 kHz input.
    upsample: usize,
    window: Vec<f32>,
    plans: Vec<MdctPlan>,
    /// Preemphasised history, `channels * OVERLAP`.
    in_mem: Vec<f32>,
    preemph_mem: [f32; 2],
    /// Coarse-energy prediction state, `2 * NB_BANDS`.
    old_band_e: Vec<f32>,
    delayed_intra: f32,
    consec_transient: u32,
    // Spreading decision state (libopus celt_encoder.c:2534-2537).
    spread_decision: usize,
    tonal_average: i32,
    hf_average: i32,
    tapset_decision: i32,
    last_coded_bands: usize,
    force_intra: bool,
    // Constrained-VBR reservoir.
    vbr_reservoir: i32,
    vbr_drift: i32,
    vbr_offset: i32,
    vbr_count: i32,
    // Per-frame scratch, allocated once.
    in_buf: Vec<f32>,
    freq: Vec<f32>,
    x: Vec<f32>,
    band_e: Vec<f32>,
    band_log_e: Vec<f32>,
    /// Long-window log energies used by dynalloc (`bandLogE2`).
    band_log_e2: Vec<f32>,
    error: Vec<f32>,
    transient_tmp: Vec<f32>,
    hadamard_tmp: Vec<f32>,
    tf_res: [i32; NB_BANDS],
    pulses: [i32; NB_BANDS],
    fine_quant: [i32; NB_BANDS],
    fine_priority: [i32; NB_BANDS],
    offsets: [i32; NB_BANDS],
    caps: [i32; NB_BANDS],
    iy: Vec<i32>,
    pvq_y: Vec<f32>,
    pvq_sign: Vec<i32>,
    urow: Vec<u32>,
    /// Per-frame diagnostics captured at the end of the last `encode` call.
    last_diag: CeltFrameDiag,
}

impl CeltEncoder {
    /// An encoder for `channels` channels (1 or 2) fed at `48000/upsample` Hz.
    pub fn new(channels: usize, upsample: usize) -> CeltEncoder {
        assert!((1..=2).contains(&channels));
        assert!(upsample >= 1);
        let max_n = SHORT_MDCT * 8;
        CeltEncoder {
            channels,
            upsample,
            window: celt::overlap_window(),
            plans: (0..4).map(|lm| MdctPlan::new(SHORT_MDCT << lm)).collect(),
            in_mem: vec![0.0; channels * OVERLAP],
            preemph_mem: [0.0; 2],
            old_band_e: vec![0.0; 2 * NB_BANDS],
            delayed_intra: 1.0,
            consec_transient: 0,
            last_coded_bands: 0,
            force_intra: true,
            spread_decision: SPREAD_NORMAL,
            tonal_average: 256,
            hf_average: 0,
            tapset_decision: 0,
            vbr_reservoir: 0,
            vbr_drift: 0,
            vbr_offset: 0,
            vbr_count: 0,
            in_buf: vec![0.0; channels * (max_n + OVERLAP)],
            freq: vec![0.0; channels * max_n],
            x: vec![0.0; channels * max_n],
            band_e: vec![0.0; 2 * NB_BANDS],
            band_log_e: vec![0.0; 2 * NB_BANDS],
            band_log_e2: vec![0.0; 2 * NB_BANDS],
            error: vec![0.0; 2 * NB_BANDS],
            transient_tmp: vec![0.0; max_n + OVERLAP],
            hadamard_tmp: vec![0.0; max_n],
            tf_res: [0; NB_BANDS],
            pulses: [0; NB_BANDS],
            fine_quant: [0; NB_BANDS],
            fine_priority: [0; NB_BANDS],
            offsets: [0; NB_BANDS],
            caps: [0; NB_BANDS],
            iy: vec![0; max_n],
            pvq_y: vec![0.0; max_n],
            pvq_sign: vec![0; max_n],
            urow: vec![0; 1280],
            last_diag: CeltFrameDiag::default(),
        }
    }

    /// Channels this encoder codes.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Diagnostics captured at the end of the most recent [`encode`] call.
    ///
    /// [`encode`]: CeltEncoder::encode
    pub fn last_diag(&self) -> &CeltFrameDiag {
        &self.last_diag
    }

    /// Drops all inter-frame state.
    pub fn reset(&mut self) {
        self.in_mem.fill(0.0);
        self.preemph_mem = [0.0; 2];
        self.old_band_e.fill(0.0);
        self.delayed_intra = 1.0;
        self.consec_transient = 0;
        self.last_coded_bands = 0;
        self.force_intra = true;
        self.spread_decision = SPREAD_NORMAL;
        self.tonal_average = 256;
        self.hf_average = 0;
        self.tapset_decision = 0;
        self.vbr_reservoir = 0;
        self.vbr_drift = 0;
        self.vbr_offset = 0;
        self.vbr_count = 0;
    }

    /// Encodes one frame of interleaved `f32` (`frame_size` samples per
    /// channel at 48 kHz, one of 120/240/480/960 — of which
    /// `frame_size/upsample` are read from `pcm`) into `enc`, which must have
    /// been `reset` to the frame's byte budget. `start..end` bounds the coded
    /// bands — `end` is how the caller limits the coded bandwidth, `start`
    /// (0, or 17 under a hybrid packet's SILK layer) skips the low bands
    /// exactly as the decoder's `start_band` does: no energy, no shapes, no
    /// postfilter bit, allocation over the coded span only. With
    /// `vbr_rate_bps` set, the frame may be shrunk below that budget
    /// (constrained VBR); the return value is the final frame size in bytes.
    pub fn encode(
        &mut self,
        enc: &mut RangeEncoder,
        pcm: &[f32],
        frame_size: usize,
        start: usize,
        end: usize,
        vbr_rate_bps: u32,
    ) -> Result<usize> {
        let lm: usize = match frame_size {
            120 => 0,
            240 => 1,
            480 => 2,
            960 => 3,
            _ => {
                return Err(Error::unsupported(
                    format!("celt frame of {frame_size} samples"),
                    "CELT frames are 2.5, 5, 10 or 20 ms at 48 kHz",
                ));
            }
        };
        let m = 1usize << lm;
        let n = frame_size;
        let c = self.channels;
        let in_n = n / self.upsample;
        if pcm.len() < in_n * c {
            return Err(Error::corrupt(format!(
                "celt encode: {} samples for a {c}-channel {in_n}-sample frame",
                pcm.len()
            )));
        }
        let end = end.clamp(start + 1, NB_BANDS);

        let mut nb_compressed = enc.storage();
        let mut nb_available = nb_compressed as i32;
        // The target rate in 1/8-bit units per frame, VBR only.
        let (vbr_rate, mut effective_bytes) = if vbr_rate_bps > 0 {
            let den = 48000i64 >> BITRES;
            let vr = ((vbr_rate_bps as i64 * n as i64 + (den >> 1)) / den) as i32;
            (vr, vr >> (3 + BITRES))
        } else {
            (0, nb_compressed as i32)
        };

        if vbr_rate > 0 {
            // Constrained VBR: cap this frame so the reservoir never dips
            // below one frame of buffering.
            let vbr_bound = vbr_rate;
            let max_allowed = (2i32
                .max((vbr_rate + vbr_bound - self.vbr_reservoir) >> (BITRES + 3)))
            .min(nb_available);
            if max_allowed < nb_available {
                nb_compressed = max_allowed as usize;
                nb_available = max_allowed;
                enc.shrink(nb_compressed);
            }
        }
        let mut total_bits = (nb_compressed * 8) as i32;

        // --- Preemphasis ----------------------------------------------------
        let stride = n + OVERLAP;
        let mut silence = true;
        for ch in 0..c {
            let base = ch * stride;
            self.in_buf[base..base + OVERLAP]
                .copy_from_slice(&self.in_mem[ch * OVERLAP..(ch + 1) * OVERLAP]);
            let mut mem = self.preemph_mem[ch];
            let up = self.upsample;
            for i in 0..n {
                // Zero-stuffing to 48 kHz: the images above the input's own
                // Nyquist are never coded (the caller caps `end`) and the
                // decoder's decimation drops them.
                let mut x = if up == 1 {
                    pcm[i * c + ch] * SIG_SCALE
                } else if i.is_multiple_of(up) {
                    pcm[(i / up) * c + ch] * SIG_SCALE * up as f32
                } else {
                    0.0
                };
                if !x.is_finite() {
                    x = 0.0;
                }
                x = x.clamp(-65536.0, 65536.0);
                let v = x + mem;
                mem = -PREEMPH * x;
                self.in_buf[base + OVERLAP + i] = v;
                silence &= v == 0.0;
            }
            self.preemph_mem[ch] = mem;
            self.in_mem[ch * OVERLAP..(ch + 1) * OVERLAP]
                .copy_from_slice(&self.in_buf[base + n..base + n + OVERLAP]);
        }

        // --- Silence flag (first symbol of the frame) -----------------------
        // Only exists when CELT opens the stream (decoder: `tell == 1`); under
        // a hybrid packet's SILK layer the frame is never flagged silent.
        if enc.tell() == 1 {
            enc.enc_bit_logp(silence, 15);
        } else {
            silence = false;
        }
        if silence {
            if vbr_rate > 0 {
                nb_compressed = nb_compressed.min(2);
                total_bits = (nb_compressed * 8) as i32;
                nb_available = 2;
                enc.shrink(nb_compressed);
            }
            enc.pad_to_end();
        }

        // --- Postfilter: always off (throughput-first, = complexity < 5) ----
        // The flag exists only when band 0 is coded (decoder: `start == 0`).
        if start == 0 && enc.tell() as i32 + 16 <= total_bits {
            enc.enc_bit_logp(false, 1);
        }

        // --- Transient flag -------------------------------------------------
        let mut is_transient = false;
        let mut short_blocks = 0usize;
        let mut tf_estimate = 0.0f32;
        let mut tf_chan = 0usize;
        if lm > 0 && enc.tell() as i32 + 3 <= total_bits {
            let (ta, te, tc) = self.transient_analysis(n + OVERLAP, c, stride);
            is_transient = ta;
            tf_estimate = te;
            tf_chan = tc;
            if is_transient {
                short_blocks = m;
            }
            enc.enc_bit_logp(is_transient, 3);
        }

        // --- Forward MDCTs, energies, normalisation -------------------------
        // dynalloc looks at long-window energies (`bandLogE2`): on transient
        // frames that is a second MDCT pass, offset by LM/2 for the block size.
        if short_blocks != 0 {
            self.compute_mdcts(0, lm, n, c);
            for ch in 0..c {
                for i in start..end {
                    let mut sum = 1e-27f32;
                    for j in m * E_BANDS[i]..m * E_BANDS[i + 1] {
                        let v = self.freq[ch * n + j];
                        sum += v * v;
                    }
                    self.band_log_e2[i + ch * NB_BANDS] =
                        (sum.sqrt() as f64).log2() as f32 - E_MEANS[i] + 0.5 * lm as f32;
                }
            }
        }
        self.compute_mdcts(short_blocks, lm, n, c);
        for ch in 0..c {
            self.x[ch * n..ch * n + m * E_BANDS[start]].fill(0.0);
            for i in start..end {
                let mut sum = 1e-27f32;
                for j in m * E_BANDS[i]..m * E_BANDS[i + 1] {
                    let v = self.freq[ch * n + j];
                    sum += v * v;
                }
                self.band_e[i + ch * NB_BANDS] = sum.sqrt();
                self.band_log_e[i + ch * NB_BANDS] =
                    (self.band_e[i + ch * NB_BANDS] as f64).log2() as f32 - E_MEANS[i];
                if short_blocks == 0 {
                    self.band_log_e2[i + ch * NB_BANDS] = self.band_log_e[i + ch * NB_BANDS];
                }
            }
            for i in start..end {
                let g = 1.0 / (1e-27 + self.band_e[i + ch * NB_BANDS]);
                for j in m * E_BANDS[i]..m * E_BANDS[i + 1] {
                    self.x[ch * n + j] = self.freq[ch * n + j] * g;
                }
            }
        }

        // --- tf_res ---------------------------------------------------------
        // `TF_ANALYSIS`: run the libopus Viterbi tf search when there are
        // enough bits; otherwise fall back to the no-analysis default (raw
        // `is_transient` per band codes "no tf change" under tf_select=0).
        const TF_ANALYSIS: bool = true;
        let tf_select = if TF_ANALYSIS && effective_bytes >= 15 * c as i32 {
            let lambda = (80i32).max(20480 / effective_bytes + 2);
            let mut importance = [0i32; NB_BANDS];
            for i in start..end {
                importance[i] = 13;
            }
            self.tf_analysis(end, is_transient, lambda, n, lm, tf_estimate, tf_chan, &importance)
        } else {
            self.tf_res = [i32::from(is_transient); NB_BANDS];
            0usize
        };

        // --- Coarse energy --------------------------------------------------
        let two_pass = false;
        let mut intra = self.force_intra
            || (!two_pass
                && self.delayed_intra > (2 * c * (end - start)) as f32
                && nb_available > ((end - start) * c) as i32);
        let new_distortion = {
            let mut dist = 0.0f32;
            for ch in 0..c {
                for i in start..end {
                    let d = self.band_log_e[i + ch * NB_BANDS] - self.old_band_e[i + ch * NB_BANDS];
                    dist += d * d;
                }
            }
            dist.min(200.0)
        };
        let tell = enc.tell();
        if tell + 3 > total_bits as u32 {
            intra = false;
        }
        let max_decay = 16.0f32.min(0.125 * nb_available as f32);
        self.quant_coarse_energy(enc, start, end, intra, c, lm, total_bits, max_decay);
        self.delayed_intra = if intra {
            new_distortion
        } else {
            PRED_COEF[lm] * PRED_COEF[lm] * self.delayed_intra + new_distortion
        };

        // --- tf encode ------------------------------------------------------
        self.tf_encode(enc, start, end, is_transient, lm, tf_select, nb_compressed);

        // --- Dynalloc analysis (offsets + spread_weight) --------------------
        self.offsets = [0; NB_BANDS];
        let mut spread_weight = [0i32; NB_BANDS];
        self.dynalloc_analysis(
            start, end, c, lm, is_transient, effective_bytes, &mut spread_weight,
        );

        // --- Spread decision (libopus celt_encoder.c:1933-1977) --------------
        // No LFE, no hybrid, no complexity field (treated as ≥ 3): the only
        // guards that apply are shortBlocks and nbAvailableBytes < 10*C.
        if enc.tell() as i32 + 4 <= total_bits {
            if short_blocks != 0 || nb_available < 10 * c as i32 {
                self.spread_decision = SPREAD_NORMAL;
            } else {
                self.spread_decision =
                    self.spreading_decision(end, c, lm, &spread_weight);
            }
            enc.enc_icdf(self.spread_decision, &SPREAD_ICDF, 5);
        }

        // --- Caps ------------------------------------------------------------
        for i in 0..NB_BANDS {
            let bw = (E_BANDS[i + 1] - E_BANDS[i]) << lm;
            self.caps[i] =
                ((CACHE_CAPS[NB_BANDS * (2 * lm + c - 1) + i] as i32 + 64) * c as i32 * bw as i32)
                    >> 2;
        }
        let mut dynalloc_logp = 6u32;
        let total_bits_frac = total_bits << BITRES;
        let mut total_boost = 0i32;
        let mut tell_frac = enc.tell_frac() as i32;
        for i in start..end {
            let width = ((c * (E_BANDS[i + 1] - E_BANDS[i])) << lm) as i32;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut loop_logp = dynalloc_logp;
            let mut boost = 0i32;
            let mut j = 0i32;
            while tell_frac + ((loop_logp as i32) << BITRES) < total_bits_frac - total_boost
                && boost < self.caps[i]
            {
                let flag = j < self.offsets[i];
                enc.enc_bit_logp(flag, loop_logp);
                tell_frac = enc.tell_frac() as i32;
                if !flag {
                    break;
                }
                boost += quanta;
                total_boost += quanta;
                loop_logp = 1;
                j += 1;
            }
            if j > 0 {
                dynalloc_logp = dynalloc_logp.saturating_sub(1).max(2);
            }
            self.offsets[i] = boost;
        }

        // --- Allocation trim ------------------------------------------------
        let mut alloc_trim = 5i32;
        if tell_frac + (6 << BITRES) <= total_bits_frac - total_boost {
            alloc_trim = self.alloc_trim_analysis(end, lm, c, n);
            enc.enc_icdf(alloc_trim as usize, &TRIM_ICDF, 7);
            tell_frac = enc.tell_frac() as i32;
        }

        // --- Constrained VBR ------------------------------------------------
        if vbr_rate > 0 {
            let lm_diff = 3 - lm as i32;
            let base_target =
                vbr_rate + (self.vbr_offset >> lm_diff) - ((40 * c as i32 + 20) << BITRES);
            let mut target = base_target;
            // compute_vbr shaping (libopus celt_encoder.c:1605-1716), minus the
            // terms that need tonality/tf analysis this encoder does not run.
            let coded_bands = end - 1;
            let intensity_est = if c == 2 {
                // Same thresholds as the stereo section below (16 at 64k stereo).
                let effective_rate = 2 * (((8 * effective_bytes - 80) >> lm) as i32) / 5;
                let i = if effective_rate < 35 {
                    8usize
                } else if effective_rate < 50 {
                    12
                } else if effective_rate < 68 {
                    16
                } else if effective_rate < 84 {
                    18
                } else if effective_rate < 102 {
                    19
                } else if effective_rate < 130 {
                    20
                } else {
                    100
                };
                i.clamp(start, coded_bands)
            } else {
                0
            };
            let coded_bins =
                (E_BANDS[coded_bands] + if c == 2 { E_BANDS[intensity_est] } else { 0 }) << lm;
            // Activity cut. libopus reads activity from tonality analysis; the
            // peak-band level (dB, both channels) is a monotone stand-in:
            // silence maps to ~0 (max cut), music to ~1 (no cut).
            let mut peak = f32::NEG_INFINITY;
            for ch in 0..c {
                for i in start..end {
                    let lvl = self.band_log_e[i + ch * NB_BANDS] + E_MEANS[i];
                    if lvl > peak {
                        peak = lvl;
                    }
                }
            }
            let activity = ((peak * 3.0103 - 5.0) / 20.0).clamp(0.0, 1.0);
            if activity < 0.4 {
                target -= (coded_bins as f32 * 8.0 * (0.4 - activity)) as i32;
            }
            // dynalloc boost minus its calibration average.
            target += total_boost - (19 << lm);
            // tf boost: transient frames take the fixed 7/4 stand-in; steady
            // frames take the tf-bias at the music-average estimate 0.05
            // (libopus: 2*(tf_estimate-0.044)*target).
            if short_blocks != 0 {
                target = 7 * target / 4;
            } else {
                target += (0.012 * target as f32) as i32;
            }
            // Spectral-floor cap (libopus floor_depth): binds only when every
            // band sits close to the coding noise floor.
            {
                let bins = E_BANDS[NB_BANDS - 2] << lm;
                let mut max_depth = -31.9f32 / 3.0103;
                for ch in 0..c {
                    for i in start..end {
                        let noise_floor = LOG_N[i] as f32 / 385.3
                            + 0.166
                            - 4.983
                            - E_MEANS[i] / 48.2
                            + 0.00206 * (i + 5) as f32 * (i + 5) as f32;
                        let depth = self.band_log_e[i + ch * NB_BANDS] + E_MEANS[i] - noise_floor;
                        if depth > max_depth {
                            max_depth = depth;
                        }
                    }
                }
                let floor_depth = ((c * bins) as f32 * 3.0103 * 8.0 * max_depth) as i32;
                let floor_depth = floor_depth.max(target >> 2);
                target = target.min(floor_depth);
            }
            // Never more than double the base rate.
            target = target.min(2 * base_target);
            target += tell_frac;
            let min_allowed =
                ((tell_frac + total_boost + (1 << (BITRES + 3)) - 1) >> (BITRES + 3)) + 2;
            let mut nb_avail = (target + (1 << (BITRES + 2))) >> (BITRES + 3);
            nb_avail = nb_avail.max(min_allowed);
            nb_avail = nb_avail.min(nb_compressed as i32);
            let mut delta = target - vbr_rate;
            target = nb_avail << (BITRES + 3);
            if silence {
                delta = 0;
            }
            let alpha = if self.vbr_count < 970 {
                self.vbr_count += 1;
                1.0 / (self.vbr_count + 20) as f32
            } else {
                0.001
            };
            self.vbr_reservoir += target - vbr_rate;
            self.vbr_drift += (alpha
                * ((delta * (1 << lm_diff)) as f32
                    - self.vbr_offset as f32
                    - self.vbr_drift as f32)) as i32;
            self.vbr_offset = -self.vbr_drift;
            // Bound banked credit at eight frames' worth of rate.
            self.vbr_reservoir = self.vbr_reservoir.max(-8 * vbr_rate);
            if self.vbr_reservoir < 0 {
                // Release banked credit slowly (8 bytes per frame) so it stays
                // available for transient boosts; libopus CVBR re-adds all of
                // it to the next frame, which would defeat banking entirely.
                let adjust = ((-self.vbr_reservoir) / (8 << BITRES)).min(8);
                if !silence {
                    nb_avail += adjust;
                    self.vbr_reservoir += adjust << (BITRES + 3);
                } else {
                    self.vbr_reservoir = 0;
                }
            }
            let shrunk = (nb_compressed as i32).min(nb_avail).max(2) as usize;
            if shrunk < nb_compressed {
                nb_compressed = shrunk;
                enc.shrink(nb_compressed);
            }
        }

        // --- Stereo decisions -----------------------------------------------
        let mut intensity = 0usize;
        let mut dual_stereo = false;
        if c == 2 {
            if lm != 0 {
                dual_stereo = self.stereo_analysis(lm, n);
            }
            let effective_rate = 2 * (((8 * effective_bytes - 80) >> lm) as i32) / 5;
            intensity = if effective_rate < 35 {
                8
            } else if effective_rate < 50 {
                12
            } else if effective_rate < 68 {
                16
            } else if effective_rate < 84 {
                18
            } else if effective_rate < 102 {
                19
            } else if effective_rate < 130 {
                20
            } else {
                100
            };
            intensity = intensity.clamp(start, end);
        }
        let _ = &mut effective_bytes;

        // --- Bit allocation -------------------------------------------------
        let mut bits = (((nb_compressed * 8) as i32) << BITRES) - enc.tell_frac() as i32 - 1;
        let anti_collapse_rsv = if is_transient && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) {
            1 << BITRES
        } else {
            0
        };
        bits -= anti_collapse_rsv;
        let alloc = compute_allocation_encode(
            enc,
            start,
            end,
            alloc_trim,
            bits,
            c,
            lm,
            &self.offsets,
            &self.caps,
            &mut self.pulses,
            &mut self.fine_quant,
            &mut self.fine_priority,
            intensity,
            dual_stereo,
            self.last_coded_bands,
        );
        let intensity = alloc.intensity;
        let dual_stereo = alloc.dual_stereo;
        let balance = alloc.balance;
        let coded_bands = alloc.coded_bands;
        self.last_coded_bands = coded_bands;

        // --- Fine energy ----------------------------------------------------
        for i in start..end {
            if self.fine_quant[i] <= 0 {
                continue;
            }
            let frac = 1 << self.fine_quant[i];
            for ch in 0..c {
                let e = &mut self.error[i + ch * NB_BANDS];
                let q2 = (((*e + 0.5) * frac as f32).floor() as i32).clamp(0, frac - 1);
                enc.enc_bits(q2 as u32, self.fine_quant[i] as u32);
                let offset = (q2 as f32 + 0.5) / frac as f32 - 0.5;
                self.old_band_e[i + ch * NB_BANDS] += offset;
                *e -= offset;
            }
        }

        // --- Shapes ---------------------------------------------------------
        self.quant_all_bands(
            enc,
            start,
            end,
            short_blocks,
            self.spread_decision,
            dual_stereo,
            intensity,
            (nb_compressed as i32 * (8 << BITRES)) - anti_collapse_rsv,
            balance,
            lm,
            coded_bands,
            c,
            n,
        );

        // --- Anti-collapse --------------------------------------------------
        if anti_collapse_rsv > 0 {
            let on = self.consec_transient < 2;
            enc.enc_bits(u32::from(on), 1);
        }

        // --- Leftover fine bits ---------------------------------------------
        let mut bits_left = nb_compressed as i32 * 8 - enc.tell() as i32;
        for prio in 0..2 {
            for i in start..end {
                if bits_left < c as i32 {
                    break;
                }
                if self.fine_quant[i] >= MAX_FINE_BITS || self.fine_priority[i] != prio {
                    continue;
                }
                for ch in 0..c {
                    let q2 = i32::from(self.error[i + ch * NB_BANDS] >= 0.0);
                    enc.enc_bits(q2 as u32, 1);
                    let offset = (q2 as f32 - 0.5) / (1 << (self.fine_quant[i] + 1)) as f32;
                    self.old_band_e[i + ch * NB_BANDS] += offset;
                    bits_left -= 1;
                }
            }
        }

        if silence {
            for e in self.old_band_e.iter_mut() {
                *e = -28.0;
            }
        }
        if c == 1 {
            for i in 0..NB_BANDS {
                self.old_band_e[NB_BANDS + i] = self.old_band_e[i];
            }
        }
        // Uncoded bands hold no energy history (the decoder clears them the
        // same way), so a later start/end change predicts from the same state.
        for ch in 0..2 {
            for i in (0..start).chain(end..NB_BANDS) {
                self.old_band_e[ch * NB_BANDS + i] = 0.0;
            }
        }
        if is_transient {
            self.consec_transient += 1;
        } else {
            self.consec_transient = 0;
        }
        self.force_intra = false;

        enc.done();
        if enc.error() {
            return Err(Error::corrupt("celt encode: busted the frame budget"));
        }
        // Capture per-frame diagnostics for the dropout investigation.
        let mut band_log_e = [0.0f32; 2 * NB_BANDS];
        band_log_e[..2 * NB_BANDS].copy_from_slice(&self.band_log_e);
        self.last_diag = CeltFrameDiag {
            is_transient,
            short_blocks,
            intra,
            silence,
            lm,
            start,
            coded_bands,
            intensity,
            dual_stereo,
            alloc_trim,
            vbr_reservoir: self.vbr_reservoir,
            nb_compressed,
            band_log_e,
            pulses: self.pulses,
            fine_quant: self.fine_quant,
        };
        Ok(nb_compressed)
    }
    // -- Analysis ------------------------------------------------------------

    /// `transient_analysis()` — libopus float-mode algorithm.
    /// Returns `(is_transient, tf_estimate, tf_chan)`.
    fn transient_analysis(&mut self, len: usize, c: usize, stride: usize) -> (bool, f32, usize) {
        let tmp = &mut self.transient_tmp[..len];
        let len2 = len / 2;
        let mut mask_metric: i32 = 0;
        let mut tf_chan: usize = 0;

        for ch in 0..c {
            // Copy this channel's samples (C layout: in[i + c*len] = in_buf[ch*stride + i])
            tmp.copy_from_slice(&self.in_buf[ch * stride..ch * stride + len]);

            // High-pass filter: (1 - 2*z^-1 + z^-2) / (1 - z^-1 + .5*z^-2)
            let mut mem0 = 0.0f32;
            let mut mem1 = 0.0f32;
            for v in tmp.iter_mut() {
                let x = *v;
                let y = mem0 + x;
                mem0 = mem1 + y - 2.0 * x;
                mem1 = x - 0.5 * y;
                *v = y;
            }
            // First few samples are bad because we don't propagate the memory
            for v in tmp.iter_mut().take(12) {
                *v = 0.0;
            }

            // Forward pass: post-echo threshold (forward_decay = 0.0625)
            let mut mean = 0.0f32;
            let mut fm0 = 0.0f32;
            for i in 0..len2 {
                let x2 = tmp[2 * i] * tmp[2 * i] + tmp[2 * i + 1] * tmp[2 * i + 1];
                mean += x2;
                tmp[i] = fm0 + 0.0625 * (x2 - fm0);
                fm0 = tmp[i];
            }

            // Backward pass: pre-echo threshold (backward_decay = 0.125)
            let mut bm0 = 0.0f32;
            let mut max_e = 0.0f32;
            for i in (0..len2).rev() {
                tmp[i] = bm0 + 0.125 * (tmp[i] - bm0);
                bm0 = tmp[i];
                max_e = max_e.max(bm0);
            }

            // Geometric mean of frame energy and half the max
            mean = (mean * max_e * 0.5 * len2 as f32).sqrt();

            // Inverse of the mean energy (float mode: SHL32/SHR32 are no-ops)
            let norm = len2 as f32 / (1e-15 + mean);

            // Harmonic mean discarding unreliable boundaries (1/4th of samples)
            let mut unmask: i32 = 0;
            let mut i = 12;
            while i < len2 - 5 {
                let id = ((64.0 * norm * (tmp[i] + 1e-15)).floor() as i32).clamp(0, 127) as usize;
                unmask += INV_TABLE[id] as i32;
                i += 4;
            }
            // Normalize: 1/4th sampling, factor of 6 in inv_table
            unmask = 64 * unmask * 4 / (6 * (len2 as i32 - 17));
            if unmask > mask_metric {
                tf_chan = ch;
                mask_metric = unmask;
            }
        }

        let is_transient = mask_metric > 200;
        // tf_max and tf_estimate (float mode: all Q-shifts are no-ops)
        let tf_max = ((27.0 * mask_metric as f32).sqrt() - 42.0).max(0.0);
        let tf_estimate = (0.0069 * tf_max.min(163.0) - 0.139).max(0.0).sqrt();

        (is_transient, tf_estimate, tf_chan)
    }

    /// libopus `dynalloc_analysis`: per-band boost requests from a follower /
    /// median masking model over the long-window energies. Writes
    /// `self.offsets[i]` in boost units (the loop that codes them converts to
    /// bits via the band's quanta).
    fn dynalloc_analysis(
        &mut self,
        start: usize,
        end: usize,
        c: usize,
        lm: usize,
        is_transient: bool,
        effective_bytes: i32,
        spread_weight: &mut [i32; NB_BANDS],
    ) {
        const LSB_DEPTH: f32 = 24.0;
        let mut noise_floor = [0.0f32; NB_BANDS];
        for i in 0..end {
            noise_floor[i] = 0.0625 * LOG_N[i] as f32 + 0.5 + (9.0 - LSB_DEPTH) - E_MEANS[i]
                + 0.0062 * ((i + 5) * (i + 5)) as f32;
        }
        // maxDepth = max(bandLogE - noise_floor) across channels (libopus 995-999).
        let mut max_depth = -31.9f32;
        for ch in 0..c {
            for i in 0..end {
                max_depth = max_depth.max(self.band_log_e[ch * NB_BANDS + i] - noise_floor[i]);
            }
        }
        // Simple masking model for spread_weight (libopus celt_encoder.c:1000-1035).
        let mut mask = [0.0f32; NB_BANDS];
        for i in 0..end {
            mask[i] = self.band_log_e[i] - noise_floor[i];
        }
        if c == 2 {
            for i in 0..end {
                mask[i] = mask[i].max(self.band_log_e[NB_BANDS + i] - noise_floor[i]);
            }
        }
        let mut sig = [0.0f32; NB_BANDS];
        sig[..end].copy_from_slice(&mask[..end]);
        for i in 1..end {
            mask[i] = mask[i].max(mask[i - 1] - 2.0);
        }
        for i in (0..end - 1).rev() {
            mask[i] = mask[i].max(mask[i + 1] - 3.0);
        }
        for i in 0..end {
            // SMR: mask is never more than 12 below the peak and never below the
            // noise floor.
            let smr = sig[i] - 0.0_f32.max(max_depth - 12.0).max(mask[i]);
            let shift = (5).min(0.max(-((smr + 0.5).floor() as i32)));
            spread_weight[i] = 32 >> shift;
        }
        // Dynamic allocation can't make us bust the budget (libopus 1036-1037).
        if !(effective_bytes > 50 && lm >= 1) {
            return;
        }
        let median3 = |a: &[f32]| -> f32 {
            let (x, y, z) = (a[0], a[1], a[2]);
            x.max(y).min(z).max(x.min(y))
        };
        let median5 = |a: &[f32]| -> f32 {
            let mut v = [a[0], a[1], a[2], a[3], a[4]];
            v.sort_by(|p, q| p.partial_cmp(q).unwrap());
            v[2]
        };
        let mut follower = [0.0f32; 2 * NB_BANDS];
        let mut last = 0usize;
        for ch in 0..c {
            let e2 = &self.band_log_e2[ch * NB_BANDS..ch * NB_BANDS + NB_BANDS];
            let f = &mut follower[ch * NB_BANDS..ch * NB_BANDS + NB_BANDS];
            f[0] = e2[0];
            for i in 1..end {
                // The last band at least .5 dB above its predecessor bounds
                // the backward pass (band-limited signals).
                if e2[i] > e2[i - 1] + 0.5 {
                    last = i;
                }
                f[i] = (f[i - 1] + 1.5).min(e2[i]);
            }
            for i in (0..last).rev() {
                f[i] = f[i].min((f[i + 1] + 2.0).min(e2[i]));
            }
            let offset = 1.0f32;
            for i in 2..end.saturating_sub(2) {
                f[i] = f[i].max(median5(&e2[i - 2..i + 3]) - offset);
            }
            let tmp = median3(&e2[0..3]) - offset;
            f[0] = f[0].max(tmp);
            f[1] = f[1].max(tmp);
            let tmp = median3(&e2[end - 3..end]) - offset;
            f[end - 2] = f[end - 2].max(tmp);
            f[end - 1] = f[end - 1].max(tmp);
            for i in 0..end {
                f[i] = f[i].max(noise_floor[i]);
            }
        }
        if c == 2 {
            for i in start..end {
                // 24 dB cross-talk between channels.
                follower[NB_BANDS + i] = follower[NB_BANDS + i].max(follower[i] - 4.0);
                follower[i] = follower[i].max(follower[NB_BANDS + i] - 4.0);
                follower[i] = 0.5
                    * ((self.band_log_e[i] - follower[i]).max(0.0)
                        + (self.band_log_e[NB_BANDS + i] - follower[NB_BANDS + i]).max(0.0));
            }
        } else {
            for i in start..end {
                follower[i] = (self.band_log_e[i] - follower[i]).max(0.0);
            }
        }
        // Our VBR is always constrained: halve dynalloc on steady frames.
        if !is_transient {
            for f in &mut follower[start..end] {
                *f *= 0.5;
            }
        }
        for i in start..end {
            if i < 8 {
                follower[i] *= 2.0;
            }
            if i >= 12 {
                follower[i] *= 0.5;
            }
        }
        let mut tot_boost = 0i32;
        for i in start..end {
            let f = follower[i].min(4.0);
            let width = ((c * (E_BANDS[i + 1] - E_BANDS[i])) << lm) as i32;
            let (boost, boost_bits) = if width < 6 {
                let b = f as i32;
                (b, (b * width) << BITRES)
            } else if width > 48 {
                let b = (f * 8.0) as i32;
                (b, ((b * width) << BITRES) / 8)
            } else {
                let b = (f * width as f32 / 6.0) as i32;
                (b, (b * 6) << BITRES)
            };
            // Constrained VBR, steady frame: dynalloc may take 2/3 of the bits.
            if !is_transient && ((tot_boost + boost_bits) >> BITRES >> 3) > 2 * effective_bytes / 3 {
                let cap = ((2 * effective_bytes / 3) << BITRES) << 3;
                self.offsets[i] = cap - tot_boost;
                break;
            }
            self.offsets[i] = boost;
            tot_boost += boost_bits;
        }
    }

    /// libopus `spreading_decision` (bands.c:479-570): classifies the current
    /// frame's spectral shape as `SPREAD_NONE`/`SPREAD_LIGHT`/`SPREAD_NORMAL`/
    /// `SPREAD_AGGRESSIVE` by counting how concentrated the normalized band
    /// coefficients are, weighted by the masking-based `spread_weight` from
    /// `dynalloc_analysis`. Maintains `tonal_average` (recursive averaging)
    /// and applies hysteresis against the previous decision.
    ///
    /// `update_hf` is always false here (no pitch prefilter), so the
    /// `hf_average`/`tapset_decision` state is carried but never updated —
    /// matching libopus with `pf_on = false`.
    fn spreading_decision(
        &mut self,
        end: usize,
        c: usize,
        lm: usize,
        spread_weight: &[i32; NB_BANDS],
    ) -> usize {
        let m = 1usize << lm;
        let n0 = SHORT_MDCT << lm; // = M * shortMdctSize
        // Last band too narrow: nothing to spread.
        if m * (E_BANDS[end] - E_BANDS[end - 1]) <= 8 {
            return SPREAD_NONE;
        }
        let mut sum = 0i32;
        let mut nb_bands = 0i32;
        for ch in 0..c {
            for i in 0..end {
                let n = m * (E_BANDS[i + 1] - E_BANDS[i]);
                if n <= 8 {
                    continue;
                }
                let base = ch * n0 + m * E_BANDS[i];
                let x = &self.x[base..base + n];
                let mut tcount = [0i32; 3];
                for j in 0..n {
                    // Float-mode: x2N = x[j]^2 * N; thresholds are the literal
                    // QCONST16 values (0.25, 0.0625, 0.015625).
                    let x2n = x[j] * x[j] * n as f32;
                    if x2n < 0.25 {
                        tcount[0] += 1;
                    }
                    if x2n < 0.0625 {
                        tcount[1] += 1;
                    }
                    if x2n < 0.015625 {
                        tcount[2] += 1;
                    }
                }
                // hf_sum accumulation is only used when update_hf is true
                // (prefilter on), which never happens here. The branch is
                // kept for structural fidelity but the result is unused.
                let _hf_sum_contrib = if i > NB_BANDS - 4 {
                    32 * (tcount[1] + tcount[0]) / n as i32
                } else {
                    0
                };
                let tmp = (2 * tcount[2] >= n as i32) as i32
                    + (2 * tcount[1] >= n as i32) as i32
                    + (2 * tcount[0] >= n as i32) as i32;
                sum += tmp * spread_weight[i];
                nb_bands += spread_weight[i];
            }
        }
        if nb_bands == 0 {
            return self.spread_decision;
        }
        let mut s = (sum << 8) / nb_bands;
        // Recursive averaging.
        s = (s + self.tonal_average) >> 1;
        self.tonal_average = s;
        // Hysteresis against the last decision.
        let last = self.spread_decision as i32;
        s = (3 * s + (((3 - last) << 7) + 64) + 2) >> 2;
        if s < 80 {
            SPREAD_AGGRESSIVE
        } else if s < 256 {
            SPREAD_NORMAL
        } else if s < 384 {
            SPREAD_LIGHT
        } else {
            SPREAD_NONE
        }
    }

    fn compute_mdcts(&mut self, short_blocks: usize, lm: usize, n: usize, c: usize) {
        let stride = n + OVERLAP;
        let (n2, blocks, plan_lm) = if short_blocks != 0 {
            (SHORT_MDCT, short_blocks, 0)
        } else {
            (n, 1, lm)
        };
        for ch in 0..c {
            for b in 0..blocks {
                let plan = &mut self.plans[plan_lm];
                plan.forward(
                    &self.in_buf,
                    ch * stride + b * n2,
                    &self.window,
                    &mut self.freq,
                    ch * n + b,
                    blocks,
                );
            }
        }
    }

    /// `alloc_trim_analysis()`: stereo correlation plus spectral tilt.
    fn alloc_trim_analysis(&self, end: usize, lm: usize, c: usize, n0: usize) -> i32 {
        let mut trim = 5i32;
        if c == 2 {
            let mut sum = 0.0f32;
            for i in 0..8 {
                let mut partial = 0.0f32;
                for j in (E_BANDS[i] << lm)..(E_BANDS[i + 1] << lm) {
                    partial += self.x[j] * self.x[n0 + j];
                }
                sum += partial;
            }
            sum *= 1.0 / 8.0;
            if sum > 0.995 {
                trim -= 4;
            } else if sum > 0.92 {
                trim -= 3;
            } else if sum > 0.85 {
                trim -= 2;
            } else if sum > 0.8 {
                trim -= 1;
            }
        }
        let mut diff = 0.0f32;
        for ch in 0..c {
            for i in 0..end - 1 {
                diff += self.band_log_e[i + ch * NB_BANDS]
                    * (2 + 2 * i as i32 - NB_BANDS as i32) as f32;
            }
        }
        diff /= (2 * c * (end - 1)) as f32;
        if diff > 2.0 {
            trim -= 1;
        }
        if diff > 8.0 {
            trim -= 1;
        }
        if diff < -4.0 {
            trim += 1;
        }
        if diff < -10.0 {
            trim += 1;
        }
        trim.clamp(0, 10)
    }

    /// `stereo_analysis()`: L1 entropy model of L/R against M/S.
    fn stereo_analysis(&self, lm: usize, n0: usize) -> bool {
        let mut sum_lr = 1e-15f64;
        let mut sum_ms = 1e-15f64;
        for i in 0..13 {
            for j in (E_BANDS[i] << lm)..(E_BANDS[i + 1] << lm) {
                let l = self.x[j] as f64;
                let r = self.x[n0 + j] as f64;
                sum_lr += l.abs() + r.abs();
                sum_ms += (l + r).abs() + (l - r).abs();
            }
        }
        // The reference's own literal (bands.c, stereo_analysis), kept as such.
        #[allow(clippy::approx_constant)]
        const SQRT_HALF: f64 = 0.707107;
        sum_ms *= SQRT_HALF;
        let mut thetas = 13;
        if lm <= 1 {
            thetas -= 8;
        }
        ((E_BANDS[13] << (lm + 1)) + thetas) as f64 * sum_ms
            > ((E_BANDS[13] << (lm + 1)) as f64) * sum_lr
    }

    // -- Energy --------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn quant_coarse_energy(
        &mut self,
        enc: &mut RangeEncoder,
        start: usize,
        end: usize,
        intra: bool,
        c: usize,
        lm: usize,
        budget: i32,
        max_decay: f32,
    ) {
        let tell = enc.tell() as i32;
        if tell + 3 <= budget {
            enc.enc_bit_logp(intra, 3);
        }
        let model = &E_PROB_MODEL[lm][usize::from(intra)];
        let (coef, beta) = if intra {
            (0.0, BETA_INTRA)
        } else {
            (PRED_COEF[lm], BETA_COEF[lm])
        };
        let mut prev = [0.0f32; 2];
        for i in start..end {
            for (ch, prev) in prev.iter_mut().enumerate().take(c) {
                let x = self.band_log_e[i + ch * NB_BANDS];
                let old_raw = self.old_band_e[i + ch * NB_BANDS];
                let old = old_raw.max(-9.0);
                let f = x - coef * old - *prev;
                let mut qi = (0.5 + f).floor() as i32;
                let decay_bound = old_raw.max(-28.0) - max_decay;
                // Prevent the energy from dropping faster than the decoder
                // can follow.
                if qi < 0 && x < decay_bound {
                    qi += (decay_bound - x) as i32;
                    if qi > 0 {
                        qi = 0;
                    }
                }
                let tell = enc.tell() as i32;
                let bits_left = budget - tell - 3 * (c as i32) * (end - i) as i32;
                if i != start && bits_left < 30 {
                    if bits_left < 24 {
                        qi = qi.min(1);
                    }
                    if bits_left < 16 {
                        qi = qi.max(-1);
                    }
                }
                if budget - tell >= 15 {
                    let pi = 2 * i.min(20);
                    laplace_encode(
                        enc,
                        &mut qi,
                        (model[pi] as u32) << 7,
                        (model[pi + 1] as i32) << 6,
                    );
                } else if budget - tell >= 2 {
                    qi = qi.clamp(-1, 1);
                    enc.enc_icdf(
                        ((2 * qi) ^ -i32::from(qi < 0)) as usize,
                        &SMALL_ENERGY_ICDF,
                        2,
                    );
                } else if budget - tell >= 1 {
                    qi = qi.min(0);
                    enc.enc_bit_logp(qi != 0, 1);
                } else {
                    qi = -1;
                }
                self.error[i + ch * NB_BANDS] = f - qi as f32;
                let q = qi as f32;
                self.old_band_e[i + ch * NB_BANDS] = coef * old + *prev + q;
                *prev += q - beta * q;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tf_encode(
        &mut self,
        enc: &mut RangeEncoder,
        start: usize,
        end: usize,
        transient: bool,
        lm: usize,
        mut tf_select: usize,
        len: usize,
    ) {
        let mut budget = (len * 8) as i32;
        let mut tell = enc.tell() as i32;
        let mut logp: u32 = if transient { 2 } else { 4 };
        let tf_select_rsv = lm > 0 && tell + (logp as i32) < budget;
        budget -= i32::from(tf_select_rsv);
        let mut curr = 0i32;
        let mut changed = false;
        for i in start..end {
            if tell + (logp as i32) <= budget {
                enc.enc_bit_logp((self.tf_res[i] ^ curr) != 0, logp);
                tell = enc.tell() as i32;
                curr = self.tf_res[i];
                changed |= curr != 0;
            } else {
                self.tf_res[i] = curr;
            }
            logp = if transient { 4 } else { 5 };
        }
        let t = usize::from(transient);
        let ch = usize::from(changed);
        if tf_select_rsv && TF_SELECT[lm][4 * t + ch] != TF_SELECT[lm][4 * t + 2 + ch] {
            enc.enc_bit_logp(tf_select != 0, 1);
        } else {
            tf_select = 0;
        }
        for i in start..end {
            self.tf_res[i] = TF_SELECT[lm][4 * t + 2 * tf_select + self.tf_res[i] as usize];
        }
    }

    /// `tf_analysis()` — libopus Viterbi time/frequency search.
    /// Writes `self.tf_res[0..len]` and returns `tf_select`.
    #[allow(clippy::too_many_arguments)]
    fn tf_analysis(
        &mut self,
        len: usize,
        is_transient: bool,
        lambda: i32,
        n: usize,
        lm: usize,
        tf_estimate: f32,
        tf_chan: usize,
        importance: &[i32; NB_BANDS],
    ) -> usize {
        // bias = 0.04 * max(-0.25, 0.5 - tf_estimate)
        let bias = 0.04 * (0.5 - tf_estimate).max(-0.25);
        let t = usize::from(is_transient);

        let mut metric = [0i32; NB_BANDS];
        let mut path0 = [0i32; NB_BANDS];
        let mut path1 = [0i32; NB_BANDS];
        let mut tf_res = [0i32; NB_BANDS];

        // Max band width for tmp/tmp_1 scratch
        let max_band_w = ((E_BANDS[len] - E_BANDS[len - 1]) << lm).max(1);
        let (tmp_s, tmp_1_s) = self.hadamard_tmp.split_at_mut(max_band_w);
        let x = &self.x;

        for i in 0..len {
            let band_w = (E_BANDS[i + 1] - E_BANDS[i]) << lm;
            let narrow = E_BANDS[i + 1] - E_BANDS[i] == 1;
            let tmp = &mut tmp_s[..band_w];
            let tmp_1 = &mut tmp_1_s[..band_w];

            // Copy band coefficients: X[tf_chan*N0 + (eBands[i]<<LM)]
            let offset = tf_chan * n + (E_BANDS[i] << lm);
            tmp.copy_from_slice(&x[offset..offset + band_w]);

            let mut best_level = 0i32;
            let mut best_l1 = l1_metric(tmp, band_w, if is_transient { lm } else { 0 }, bias);

            // Check the -1 case for transients
            if is_transient && !narrow {
                tmp_1.copy_from_slice(tmp);
                haar1(tmp_1, band_w >> lm, 1 << lm);
                let l1 = l1_metric(tmp_1, band_w, lm + 1, bias);
                if l1 < best_l1 {
                    best_l1 = l1;
                    best_level = -1;
                }
            }

            for k in 0..(lm + usize::from(!(is_transient || narrow))) {
                let b = if is_transient { lm - k - 1 } else { k + 1 };
                haar1(tmp, band_w >> k, 1 << k);
                let l1 = l1_metric(tmp, band_w, b, bias);
                if l1 < best_l1 {
                    best_l1 = l1;
                    best_level = (k as i32) + 1;
                }
            }

            metric[i] = if is_transient { 2 * best_level } else { -2 * best_level };
            if narrow && (metric[i] == 0 || metric[i] == -2 * lm as i32) {
                metric[i] -= 1;
            }
        }

        // Search for optimal tf resolution (tf_select)
        let mut tf_select = 0usize;
        let mut selcost = [0i32; 2];
        for sel in 0..2 {
            let mut cost0 = importance[0] * (metric[0] - 2 * TF_SELECT[lm][4 * t + 2 * sel]).abs();
            let mut cost1 = importance[0] * (metric[0] - 2 * TF_SELECT[lm][4 * t + 2 * sel + 1]).abs()
                + if is_transient { 0 } else { lambda };
            for i in 1..len {
                let curr0 = cost0.min(cost1 + lambda);
                let curr1 = (cost0 + lambda).min(cost1);
                cost0 = curr0 + importance[i] * (metric[i] - 2 * TF_SELECT[lm][4 * t + 2 * sel]).abs();
                cost1 = curr1 + importance[i] * (metric[i] - 2 * TF_SELECT[lm][4 * t + 2 * sel + 1]).abs();
            }
            selcost[sel] = cost0.min(cost1);
        }
        if selcost[1] < selcost[0] && is_transient {
            tf_select = 1;
        }

        // Final Viterbi forward pass
        let mut cost0 = importance[0] * (metric[0] - 2 * TF_SELECT[lm][4 * t + 2 * tf_select]).abs();
        let mut cost1 = importance[0] * (metric[0] - 2 * TF_SELECT[lm][4 * t + 2 * tf_select + 1]).abs()
            + if is_transient { 0 } else { lambda };
        for i in 1..len {
            // curr0: best path to state 0
            let from0 = cost0;
            let from1 = cost1 + lambda;
            let (curr0, p0) = if from0 < from1 { (from0, 0) } else { (from1, 1) };
            path0[i] = p0;
            // curr1: best path to state 1 (uses OLD cost0)
            let from0 = cost0 + lambda;
            let from1 = cost1;
            let (curr1, p1) = if from0 < from1 { (from0, 0) } else { (from1, 1) };
            path1[i] = p1;

            cost0 = curr0 + importance[i] * (metric[i] - 2 * TF_SELECT[lm][4 * t + 2 * tf_select]).abs();
            cost1 = curr1 + importance[i] * (metric[i] - 2 * TF_SELECT[lm][4 * t + 2 * tf_select + 1]).abs();
        }

        // Backward pass
        tf_res[len - 1] = if cost0 < cost1 { 0 } else { 1 };
        for i in (0..len - 1).rev() {
            tf_res[i] = if tf_res[i + 1] == 1 { path1[i + 1] } else { path0[i + 1] };
        }

        self.tf_res = tf_res;
        tf_select
    }

    // -- Band shapes ---------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn quant_all_bands(
        &mut self,
        enc: &mut RangeEncoder,
        start: usize,
        end: usize,
        short_blocks: usize,
        spread: usize,
        mut dual_stereo: bool,
        intensity: usize,
        total_bits: i32,
        mut balance: i32,
        lm: usize,
        coded_bands: usize,
        c: usize,
        n: usize,
    ) {
        let m = 1usize << lm;
        let blocks = if short_blocks != 0 { m } else { 1 };
        for i in start..end {
            let n_band = m * E_BANDS[i + 1] - m * E_BANDS[i];
            let tell = enc.tell_frac() as i32;
            if i != start {
                balance -= tell;
            }
            let mut remaining_bits = total_bits - tell - 1;
            let b = if i < coded_bands {
                let curr_balance = balance / 3.min(coded_bands - i) as i32;
                0.max(16383.min((remaining_bits + 1).min(self.pulses[i] + curr_balance)))
            } else {
                0
            };
            let tf_change = self.tf_res[i];
            if dual_stereo && i == intensity {
                dual_stereo = false;
            }
            let x_off = m * E_BANDS[i];
            let base = EncBand {
                i,
                x: x_off,
                y: None,
                n: n_band,
                b,
                spread,
                blocks,
                intensity,
                tf_change,
                level: 0,
                lm: lm as i32,
            };
            if dual_stereo {
                self.quant_band(enc, EncBand { b: b / 2, ..base }, &mut remaining_bits);
                self.quant_band(
                    enc,
                    EncBand {
                        x: n + x_off,
                        b: b / 2,
                        ..base
                    },
                    &mut remaining_bits,
                );
            } else {
                self.quant_band(
                    enc,
                    EncBand {
                        y: if c == 2 { Some(n + x_off) } else { None },
                        ..base
                    },
                    &mut remaining_bits,
                );
            }
            balance += self.pulses[i] + tell;
        }
    }

    /// Encodes one band (Section 5.3.4-5.3.5), recursing for splits and for
    /// stereo. The mirror of the decoder's `quant_band`, with the resynthesis
    /// paths removed — nothing the encoder writes depends on them.
    fn quant_band(&mut self, enc: &mut RangeEncoder, a: EncBand, remaining_bits: &mut i32) {
        let EncBand {
            i,
            x,
            mut y,
            mut n,
            mut b,
            spread,
            mut blocks,
            intensity,
            mut tf_change,
            level,
            mut lm,
        } = a;
        let stereo = y.is_some();
        let mut split = stereo;
        let long_blocks = blocks == 1;
        let mut b0 = blocks;

        if n == 1 {
            for offs in [Some(x), y].into_iter().flatten() {
                if *remaining_bits >= 1 << BITRES {
                    let sign = self.x[offs] < 0.0;
                    enc.enc_bits(u32::from(sign), 1);
                    *remaining_bits -= 1 << BITRES;
                }
            }
            return;
        }

        let mut n_b = n / blocks;
        if !stereo && level == 0 {
            let recombine = if tf_change > 0 { tf_change as usize } else { 0 };
            for k in 0..recombine {
                haar1(&mut self.x[x..], n >> k, 1 << k);
            }
            blocks >>= recombine;
            n_b <<= recombine;
            while (n_b & 1) == 0 && tf_change < 0 {
                haar1(&mut self.x[x..], n_b, blocks);
                blocks <<= 1;
                n_b >>= 1;
                tf_change += 1;
            }
            b0 = blocks;
            if b0 > 1 {
                deinterleave_hadamard(
                    &mut self.x[x..x + n],
                    &mut self.hadamard_tmp,
                    n_b >> recombine,
                    b0 << recombine,
                    long_blocks,
                );
            }
        }

        let cache_start = cache_index(i, lm);
        let cache0 = CACHE_BITS[cache_start] as usize;
        if !stereo && lm != -1 && b > CACHE_BITS[cache_start + cache0] as i32 + 12 && n > 2 {
            n >>= 1;
            y = Some(x + n);
            split = true;
            lm -= 1;
            blocks = (blocks + 1) >> 1;
        }

        if split {
            let pulse_cap = LOG_N[i] + lm * (1 << BITRES);
            let offset = (pulse_cap >> 1)
                - if stereo && n == 2 {
                    QTHETA_OFFSET_TWOPHASE
                } else {
                    QTHETA_OFFSET
                };
            let mut qn = compute_qn(n as i32, b, offset, pulse_cap, stereo);
            if stereo && i >= intensity {
                qn = 1;
            }
            // The angle between the two halves, before any quantisation.
            let mut itheta = {
                let yy = y.unwrap();
                let (xs, ys) = (&self.x[x..x + n], &self.x[yy..yy + n]);
                stereo_itheta(xs, ys, stereo, n)
            };
            let tell = enc.tell_frac() as i32;
            if qn != 1 {
                itheta = (itheta * qn + 8192) >> 14;
                // Entropy coding of the angle: step pdf for stereo, uniform
                // for the time split, triangular otherwise.
                if stereo && n > 2 {
                    let p0 = 3i32;
                    let x0 = qn / 2;
                    let ft = (p0 * (x0 + 1) + x0) as u32;
                    let xv = itheta;
                    let (fl, fh) = if xv <= x0 {
                        (p0 * xv, p0 * (xv + 1))
                    } else {
                        ((xv - 1 - x0) + (x0 + 1) * p0, (xv - x0) + (x0 + 1) * p0)
                    };
                    enc.encode(fl as u32, fh as u32, ft);
                } else if b0 > 1 || stereo {
                    enc.enc_uint(itheta as u32, (qn + 1) as u32);
                } else {
                    let ft = ((qn >> 1) + 1) * ((qn >> 1) + 1);
                    let (fl, fs) = if itheta <= qn >> 1 {
                        ((itheta * (itheta + 1)) >> 1, itheta + 1)
                    } else {
                        (
                            ft - (((qn + 1 - itheta) * (qn + 2 - itheta)) >> 1),
                            qn + 1 - itheta,
                        )
                    };
                    enc.encode(fl as u32, (fl + fs) as u32, ft as u32);
                }
                itheta = itheta * 16384 / qn;
                if stereo {
                    let yy = y.unwrap();
                    if itheta == 0 {
                        self.intensity_stereo(i, x, yy, n);
                    } else {
                        // stereo_split
                        for j in 0..n {
                            let l = core::f32::consts::FRAC_1_SQRT_2 * self.x[x + j];
                            let r = core::f32::consts::FRAC_1_SQRT_2 * self.x[yy + j];
                            self.x[x + j] = l + r;
                            self.x[yy + j] = r - l;
                        }
                    }
                }
            } else if stereo {
                let yy = y.unwrap();
                let inv = itheta > 8192;
                if inv {
                    for j in 0..n {
                        self.x[yy + j] = -self.x[yy + j];
                    }
                }
                self.intensity_stereo(i, x, yy, n);
                if b > 2 << BITRES && *remaining_bits > 2 << BITRES {
                    enc.enc_bit_logp(inv, 2);
                }
                itheta = 0;
            }
            let qalloc = enc.tell_frac() as i32 - tell;
            b -= qalloc;

            let mut delta;
            let imid;
            let iside;
            if itheta == 0 {
                imid = 32767;
                iside = 0;
                delta = -16384;
            } else if itheta == 16384 {
                imid = 0;
                iside = 32767;
                delta = 16384;
            } else {
                imid = bitexact_cos(itheta as i16) as i32;
                iside = bitexact_cos((16384 - itheta) as i16) as i32;
                delta = frac_mul16(
                    ((n as i32 - 1) << 7) as i16,
                    bitexact_log2tan(iside, imid) as i16,
                );
            }
            let _ = imid;
            let _ = iside;

            if n == 2 && stereo {
                let mut mbits = b;
                let mut sbits = 0;
                if itheta != 0 && itheta != 16384 {
                    sbits = 1 << BITRES;
                }
                mbits -= sbits;
                let yy = y.unwrap();
                let swap = itheta > 8192;
                *remaining_bits -= qalloc + sbits;
                let (x2, y2) = if swap { (yy, x) } else { (x, yy) };
                if sbits != 0 {
                    let sign = self.x[x2] * self.x[y2 + 1] - self.x[x2 + 1] * self.x[y2] < 0.0;
                    enc.enc_bits(u32::from(sign), 1);
                }
                self.quant_band(
                    enc,
                    EncBand {
                        x: x2,
                        y: None,
                        n,
                        b: mbits,
                        blocks,
                        tf_change,
                        lm,
                        ..a
                    },
                    remaining_bits,
                );
            } else {
                if b0 > 1 && !stereo && (itheta & 0x3fff) != 0 {
                    if itheta > 8192 {
                        delta -= delta >> (4 - lm);
                    } else {
                        delta = 0.min(delta + ((n as i32) << BITRES >> (5 - lm)));
                    }
                }
                let mut mbits = 0.max(b.min((b - delta) / 2));
                let mut sbits = b - mbits;
                *remaining_bits -= qalloc;
                let next_level = if stereo { level } else { level + 1 };
                let mid_args = EncBand {
                    x,
                    y: None,
                    n,
                    blocks,
                    tf_change,
                    level: next_level,
                    lm,
                    ..a
                };
                let side_args = EncBand {
                    x: y.unwrap(),
                    y: None,
                    n,
                    blocks,
                    tf_change,
                    level: next_level,
                    lm,
                    ..a
                };
                let mut rebalance = *remaining_bits;
                if mbits >= sbits {
                    self.quant_band(
                        enc,
                        EncBand {
                            b: mbits,
                            ..mid_args
                        },
                        remaining_bits,
                    );
                    rebalance = mbits - (rebalance - *remaining_bits);
                    if rebalance > 3 << BITRES && itheta != 0 {
                        sbits += rebalance - (3 << BITRES);
                    }
                    self.quant_band(
                        enc,
                        EncBand {
                            b: sbits,
                            ..side_args
                        },
                        remaining_bits,
                    );
                } else {
                    self.quant_band(
                        enc,
                        EncBand {
                            b: sbits,
                            ..side_args
                        },
                        remaining_bits,
                    );
                    rebalance = sbits - (rebalance - *remaining_bits);
                    if rebalance > 3 << BITRES && itheta != 16384 {
                        mbits += rebalance - (3 << BITRES);
                    }
                    self.quant_band(
                        enc,
                        EncBand {
                            b: mbits,
                            ..mid_args
                        },
                        remaining_bits,
                    );
                }
            }
        } else {
            // The basic no-split case: straight PVQ.
            let mut q = bits2pulses(i, lm, b);
            let mut curr_bits = pulses2bits(i, lm, q);
            *remaining_bits -= curr_bits;
            while *remaining_bits < 0 && q > 0 {
                *remaining_bits += curr_bits;
                q -= 1;
                curr_bits = pulses2bits(i, lm, q);
                *remaining_bits -= curr_bits;
            }
            if q != 0 {
                let k = get_pulses(q) as usize;
                self.alg_quant(enc, x, n, k, spread, blocks);
            }
            // q == 0: the decoder folds or fills with noise on its own;
            // nothing is coded.
        }
    }

    /// `intensity_stereo()`: collapses the pair onto a single channel weighted
    /// by the band energies; only the combined channel is coded.
    fn intensity_stereo(&mut self, band: usize, x: usize, y: usize, n: usize) {
        let left = self.band_e[band];
        let right = self.band_e[band + NB_BANDS];
        let norm = 1e-15 + (1e-15 + left * left + right * right).sqrt();
        let a1 = left / norm;
        let a2 = right / norm;
        for j in 0..n {
            let l = self.x[x + j];
            let r = self.x[y + j];
            self.x[x + j] = a1 * l + a2 * r;
        }
    }

    /// `alg_quant()`: the PVQ search — greedy pyramid projection plus one
    /// pulse at a time on the exact `Rxy^2/Ryy` criterion — then the
    /// enumeration encode.
    fn alg_quant(
        &mut self,
        enc: &mut RangeEncoder,
        x: usize,
        n: usize,
        k: usize,
        spread: usize,
        blocks: usize,
    ) {
        debug_assert!(n > 1 && k > 0);
        exp_rotation(&mut self.x[x..x + n], n, 1, blocks, k, spread);
        let xs = &mut self.x[x..x + n];
        let iy = &mut self.iy[..n];
        let y = &mut self.pvq_y[..n];
        let signs = &mut self.pvq_sign[..n];
        for j in 0..n {
            if xs[j] > 0.0 {
                signs[j] = 1;
            } else {
                signs[j] = -1;
                xs[j] = -xs[j];
            }
            iy[j] = 0;
            y[j] = 0.0;
        }
        let mut xy = 0.0f32;
        let mut yy = 0.0f32;
        let mut pulses_left = k as i32;
        if k > n >> 1 {
            let mut sum = 0.0f32;
            for v in xs.iter() {
                sum += *v;
            }
            if !(sum > 1e-15 && sum < 64.0) {
                xs[0] = 1.0;
                for v in xs.iter_mut().skip(1) {
                    *v = 0.0;
                }
                sum = 1.0;
            }
            let rcp = (k as f32 - 1.0) / sum;
            for j in 0..n {
                iy[j] = (rcp * xs[j]).floor() as i32;
                y[j] = iy[j] as f32;
                yy += y[j] * y[j];
                xy += xs[j] * y[j];
                y[j] *= 2.0;
                pulses_left -= iy[j];
            }
        }
        debug_assert!(pulses_left >= 1);
        if pulses_left > n as i32 + 3 {
            let tmp = pulses_left as f32;
            yy += tmp * tmp;
            yy += tmp * y[0];
            iy[0] += pulses_left;
            pulses_left = 0;
        }
        // The pulse-placement search is the encoder's hottest loop — O(K*N)
        // per band, dominant at high rates — so it runs eight candidates a
        // lane with `wide`. Lane-local bests are reduced after the sweep;
        // only the tie-breaking order differs from the scalar loop, and any
        // pulse choice is a valid codeword — the enumeration, not the
        // search, is what conformance constrains.
        for _ in 0..pulses_left {
            yy += 1.0;
            let mut best_id = 0usize;
            let mut best_num = -1e15f32;
            let mut best_den = 0.0f32;
            let mut j0 = 0usize;
            if n >= 16 {
                use wide::f32x8;
                let chunks = n / 8;
                let xy_v = f32x8::splat(xy);
                let yy_v = f32x8::splat(yy);
                let mut bn = f32x8::splat(-1e15);
                let mut bd = f32x8::splat(0.0);
                let mut bc = f32x8::splat(0.0);
                for c in 0..chunks {
                    let mut xa = [0f32; 8];
                    let mut ya = [0f32; 8];
                    xa.copy_from_slice(&xs[c * 8..c * 8 + 8]);
                    ya.copy_from_slice(&y[c * 8..c * 8 + 8]);
                    let rxy = xy_v + f32x8::from(xa);
                    let ryy = yy_v + f32x8::from(ya);
                    let rxy2 = rxy * rxy;
                    let mask = (bd * rxy2).simd_gt(ryy * bn);
                    // wide 1.5 deprecates `blend` in favour of a split that
                    // does not exist yet in this release; the mask here is a
                    // true lane mask, which is exactly blend's contract.
                    #[allow(deprecated)]
                    {
                        bn = mask.blend(rxy2, bn);
                        bd = mask.blend(ryy, bd);
                        bc = mask.blend(f32x8::splat(c as f32), bc);
                    }
                }
                let bn_a: [f32; 8] = bn.into();
                let bd_a: [f32; 8] = bd.into();
                let bc_a: [f32; 8] = bc.into();
                for l in 0..8 {
                    if best_den * bn_a[l] > bd_a[l] * best_num {
                        best_den = bd_a[l];
                        best_num = bn_a[l];
                        best_id = bc_a[l] as usize * 8 + l;
                    }
                }
                j0 = chunks * 8;
            }
            for j in j0..n {
                let rxy = xy + xs[j];
                let ryy = yy + y[j];
                let rxy2 = rxy * rxy;
                if best_den * rxy2 > ryy * best_num {
                    best_den = ryy;
                    best_num = rxy2;
                    best_id = j;
                }
            }
            xy += xs[best_id];
            yy += y[best_id];
            y[best_id] += 2.0;
            iy[best_id] += 1;
        }
        for j in 0..n {
            if signs[j] < 0 {
                iy[j] = -iy[j];
            }
        }
        if self.urow.len() < k + 2 {
            self.urow.resize(k + 2, 0);
        }
        encode_pulses(enc, &self.iy[..n], n, k, &mut self.urow);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::RangeDecoder;

    /// The forward MDCT against the transform's definition, at the 2/l scale
    /// CELT puts on the analysis side.
    #[test]
    fn forward_mdct_matches_the_definition() {
        for &l in &[120usize, 240, 480, 960] {
            let w = celt::overlap_window();
            let flat = (l - OVERLAP) / 2;
            // The full 2l low-overlap window.
            let mut win = vec![0.0f32; 2 * l];
            for i in 0..l {
                win[i] = if i < flat {
                    0.0
                } else if i < flat + OVERLAP {
                    w[i - flat]
                } else {
                    1.0
                };
                win[2 * l - 1 - i] = win[i];
            }
            // A signal over the window's support, zero-padded to 2l.
            let support = l + OVERLAP;
            let sig: Vec<f32> = (0..support)
                .map(|i| ((i * 2654435761usize) % 2000) as f32 / 1000.0 - 1.0)
                .collect();
            let mut x2l = vec![0.0f32; 2 * l];
            for j in 0..support {
                x2l[flat + j] = win[flat + j] * sig[j];
            }
            // Direct MDCT by definition.
            let direct: Vec<f64> = (0..l)
                .map(|k| {
                    let mut sum = 0.0f64;
                    for (nn, &v) in x2l.iter().enumerate() {
                        let a = core::f64::consts::PI / l as f64
                            * (nn as f64 + 0.5 + l as f64 / 2.0)
                            * (k as f64 + 0.5);
                        sum += v as f64 * a.cos();
                    }
                    sum * 2.0 / l as f64
                })
                .collect();
            let mut plan = MdctPlan::new(l);
            let mut out = vec![0.0f32; l];
            plan.forward(&sig, 0, &w, &mut out, 0, 1);
            let mut worst = 0.0f64;
            for k in 0..l {
                worst = worst.max((direct[k] - out[k] as f64).abs());
            }
            assert!(worst < 1e-4, "l={l}: worst coefficient error {worst}");
        }
    }

    /// encode_pulses must invert decode_pulses for every (n, k) shape the
    /// allocation can produce.
    #[test]
    fn pulse_enumeration_round_trips() {
        let mut state = 0x2545F491u32;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for &(n, k) in &[
            (2usize, 1usize),
            (2, 8),
            (3, 4),
            (4, 6),
            (5, 3),
            (8, 10),
            (16, 7),
            (24, 4),
            (48, 2),
            (96, 1),
            (176, 2),
            (22, 8),
            (32, 6),
            (11, 12),
            (64, 4),
        ] {
            for _ in 0..20 {
                // A random pulse vector with |y| summing to k.
                let mut iy = vec![0i32; n];
                for _ in 0..k {
                    let j = (rand() as usize) % n;
                    iy[j] += 1;
                }
                for v in iy.iter_mut() {
                    if *v != 0 && rand() & 1 == 1 {
                        *v = -*v;
                    }
                }
                let mut enc = RangeEncoder::new();
                enc.reset(600);
                let mut u = vec![0u32; k + 2];
                encode_pulses(&mut enc, &iy, n, k, &mut u);
                enc.done();
                assert!(!enc.error());
                let data = enc.data().to_vec();
                let mut dec = RangeDecoder::new(&data);
                let mut got = vec![0i32; n];
                let mut u2 = vec![0u32; k + 2];
                crate::celt::decode_pulses(&mut dec, n, k, &mut got, &mut u2);
                assert_eq!(got, iy, "n={n} k={k}");
            }
        }
    }

    /// The Laplace encoder against the decoder, over the whole coarse-energy
    /// model space.
    #[test]
    fn laplace_round_trips() {
        #[allow(clippy::needless_range_loop)]
        for lm in 0..4 {
            for intra in 0..2 {
                for band in 0..21usize {
                    let model = &E_PROB_MODEL[lm][intra];
                    let pi = 2 * band.min(20);
                    let fs = (model[pi] as u32) << 7;
                    let decay = (model[pi + 1] as i32) << 6;
                    for val in -12i32..=12 {
                        let mut enc = RangeEncoder::new();
                        enc.reset(64);
                        let mut v = val;
                        laplace_encode(&mut enc, &mut v, fs, decay);
                        enc.done();
                        let data = enc.data().to_vec();
                        let mut dec = RangeDecoder::new(&data);
                        let got = crate::celt::laplace_decode(&mut dec, fs, decay);
                        assert_eq!(got, v, "lm={lm} intra={intra} band={band} val={val}");
                    }
                }
            }
        }
    }

    fn allocation_caps(lm: usize, c: usize) -> [i32; NB_BANDS] {
        let mut caps = [0i32; NB_BANDS];
        for i in 0..NB_BANDS {
            let bw = (E_BANDS[i + 1] - E_BANDS[i]) << lm;
            caps[i] =
                ((CACHE_CAPS[NB_BANDS * (2 * lm + c - 1) + i] as i32 + 64) * c as i32 * bw as i32)
                    >> 2;
        }
        caps
    }

    #[test]
    fn allocation_encoder_and_decoder_match_on_grid() {
        let frame_grid = [(0usize, 120usize), (1, 240), (2, 480), (3, 960)];
        let bitrate_grid = [16_000i32, 24_000, 48_000, 96_000, 192_000];
        let stereo_grid = [
            (0usize, false),
            (8, false),
            (12, true),
            (18, false),
            (NB_BANDS, true),
        ];

        for &(lm, frame_size) in &frame_grid {
            assert_eq!(SHORT_MDCT << lm, frame_size);
            for &bitrate in &bitrate_grid {
                for &c in &[1usize, 2] {
                    let start = 0usize;
                    let end = NB_BANDS;
                    let caps = allocation_caps(lm, c);
                    let mut offsets = [0i32; NB_BANDS];
                    for i in start..end {
                        if (i + lm + bitrate as usize / 8_000 + c).is_multiple_of(7) {
                            let width = ((c * (E_BANDS[i + 1] - E_BANDS[i])) << lm) as i32;
                            offsets[i] = (width << BITRES).min(caps[i] / 2).max(0);
                        }
                    }
                    let total = ((bitrate * frame_size as i32 / 48_000) << BITRES).max(2 << BITRES);
                    let trim = ((bitrate / 24_000) + lm as i32 + c as i32).rem_euclid(11);
                    let prev = end.saturating_sub((bitrate as usize / 24_000 + lm + c) % 6);
                    let stereo_cases: &[(usize, bool)] =
                        if c == 2 { &stereo_grid } else { &[(0, false)] };

                    for &(requested_intensity, requested_dual) in stereo_cases {
                        let mut ep = [0i32; NB_BANDS];
                        let mut ef = [0i32; NB_BANDS];
                        let mut eprio = [0i32; NB_BANDS];
                        let mut enc = RangeEncoder::new();
                        enc.reset(64);
                        let er = compute_allocation_encode(
                            &mut enc,
                            start,
                            end,
                            trim,
                            total,
                            c,
                            lm,
                            &offsets,
                            &caps,
                            &mut ep,
                            &mut ef,
                            &mut eprio,
                            requested_intensity,
                            requested_dual,
                            prev,
                        );
                        let enc_tell = enc.tell_frac();
                        enc.done();
                        assert!(!enc.error());

                        let data = enc.data().to_vec();
                        let mut dp = [0i32; NB_BANDS];
                        let mut df = [0i32; NB_BANDS];
                        let mut dprio = [0i32; NB_BANDS];
                        let mut dec = RangeDecoder::new(&data);
                        let dr = celt::compute_allocation_decode(
                            &mut dec, start, end, trim, total, c, lm, &offsets, &caps, &mut dp,
                            &mut df, &mut dprio,
                        );

                        assert_eq!(dr, er, "lm={lm} bitrate={bitrate} c={c}");
                        assert_eq!(
                            dec.tell_frac(),
                            enc_tell,
                            "tell lm={lm} bitrate={bitrate} c={c}"
                        );
                        assert_eq!(
                            &dp[start..end],
                            &ep[start..end],
                            "pulses lm={lm} bitrate={bitrate} c={c}"
                        );
                        assert_eq!(
                            &df[start..end],
                            &ef[start..end],
                            "fine lm={lm} bitrate={bitrate} c={c}"
                        );
                        assert_eq!(
                            &dprio[start..end],
                            &eprio[start..end],
                            "priority lm={lm} bitrate={bitrate} c={c}"
                        );
                    }
                }
            }
        }
    }
}
