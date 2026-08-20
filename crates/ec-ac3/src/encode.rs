//! An AC-3 encoder: PCM in, spec-valid ATSC A/52 syncframes out.
//!
//! The bitstream writer mirrors the parser's normative order (`syncinfo()`,
//! `bsi()`, six `audblk()`s, `auxdata`/CRC). Per block and channel a transient
//! detector picks the long or the block-switched short transform, an exponent
//! planner picks D15/D25/D45/reuse ([`Ac3Encoder::plan`]), and a rate loop
//! binary-searches the SNR offsets so the frame fills its fixed size
//! ([`Ac3Encoder::encode_frame`]). The bit allocation parameters themselves
//! are constants and `crate::bitalloc::compute` is the single source of truth
//! for what each mantissa gets.
//!
//! No coupling, no rematrixing, no delta bit allocation, no AHT: all of that
//! is legal to leave off (every presence bit for them is simply sent as 0),
//! and all of it is future work rather than a correctness gap in what is
//! implemented here.

use ec_core::{
    AudioParameters, BitWriter, Buf, ChannelLayout, CodecId, CodecParameters, Encoder, Error,
    Frame, MediaParameters, Packet, Result, SampleFormat, TimeBase,
};
use ec_dsp::Mdct;

use crate::bitalloc::{self, Allocation, BitAllocParams, Channel};
use crate::bsi::Acmod;
use crate::exps::Strategy;
use crate::transform::Imdct;
use crate::tables::{BIT_RATE_KBPS, FRAME_SIZE_WORDS, QNTZTAB, QUANT_LEVELS, SAMPLE_RATE, WINDOW};

/// Samples per channel one syncframe codes.
const FRAME_SAMPLES: usize = 1536;
/// Blocks per syncframe.
const BLOCKS: usize = 6;
/// Coefficients per channel per block ([`crate::decode::COEFFS`], not public).
const COEFFS: usize = 256;
/// Lowest bit rate per coded channel this encoder accepts: below it the
/// side information of the cheapest legal frame no longer fits (see
/// [`Ac3Encoder::encode_frame`]); [`Ac3Encoder::new`] raises the request to it.
const MIN_KBPS_PER_CHANNEL: u32 = 32;
/// Bit allocation parameters (§7.2.2.3), constant for every frame.
const BA_PARAMS: BitAllocParams = BitAllocParams { sdcycod: 2, fdcycod: 1, sgaincod: 1, dbpbcod: 2, floorcod: 7 };
/// Fast gain code, constant for every channel and frame.
const FGAINCOD: u8 = 4;
/// [`envelope_moved`]: exponent steps (6 dB each) a band's peak may drift
/// before the block re-sends exponents.
const RESEND_STEPS: u8 = 2;
/// [`is_transient`]: a segment this many times louder than everything before it.
const TRANSIENT_RATIO: f32 = 8.0;
/// [`is_transient`]: segment energy (64 high-passed samples) below which
/// nothing is an attack.
const TRANSIENT_FLOOR: f32 = 1e-2;
/// LFE `endmant`, fixed by the standard.
const LFE_ENDMANT: usize = 7;
/// Scale from `ec_dsp::Mdct::forward_windowed`'s raw sum (which equals the
/// plain A/52 §7.9.4 cosine sum) to the coefficients `crate::transform::Imdct`
/// expects: the standard's `-2/N` (N = 512) forward normalisation, with its
/// `2.0x` on the way back being the decoder's own half of that pair.
const FORWARD_GAIN: f32 = -2.0 / 512.0;
/// Scale from the transpose of the decoder's short inverse to the
/// coefficients it expects back; measured in `tests::tdac_short_*`.
const SHORT_GAIN: f32 = 1.0 / 128.0;

/// How the encoder is set up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncoderConfig {
    /// Sample rate in Hz: 48000, 44100 or 32000.
    pub sample_rate: u32,
    /// Channel count, 1 to 6 (6 is 3/2 + LFE).
    pub channels: u16,
    /// Target bit rate in kbit/s, snapped to the nearest legal A/52 rate.
    pub bitrate_kbps: u32,
}

/// The coded-channel order this encoder writes to the bit stream, alongside
/// which "family" (public, L/R/C/LFE/Ls/Rs) slot each one reads its PCM from.
///
/// Mirrors `decode::Core::channel_order` (decode.rs:737-754) exactly, so a
/// frame encoded from family-order PCM and decoded back comes out in the same
/// family order it went in. `None` marks the LFE slot.
fn channel_order(acmod: Acmod, nfchans: usize, lfeon: bool) -> Vec<Option<usize>> {
    let mut order: Vec<Option<usize>> = match acmod {
        Acmod::Surround3_0 | Acmod::Surround3_1 | Acmod::Surround3_2 => {
            let mut v = vec![Some(0), Some(2), Some(1)];
            v.extend((3..nfchans).map(Some));
            v
        }
        _ => (0..nfchans).map(Some).collect(),
    };
    if lfeon {
        let fronts = match acmod {
            Acmod::Surround3_0 | Acmod::Surround3_1 | Acmod::Surround3_2 => 3,
            Acmod::Mono => 1,
            _ => 2,
        };
        order.insert(fronts.min(order.len()), None);
    }
    order
}

/// Acmod and LFE presence for a channel count, 1..=6.
fn acmod_for(channels: u16) -> Result<(Acmod, bool)> {
    Ok(match channels {
        1 => (Acmod::Mono, false),
        2 => (Acmod::Stereo, false),
        3 => (Acmod::Surround3_0, false),
        4 => (Acmod::Surround2_2, false),
        5 => (Acmod::Surround3_2, false),
        6 => (Acmod::Surround3_2, true),
        other => {
            return Err(Error::unsupported(
                format!("{other} channels"),
                "this encoder codes 1 to 6 channels (6 is 3/2 + LFE)",
            ));
        }
    })
}

/// CRC-16, poly `x^16 + x^15 + x^2 + 1` (0x8005), MSB first, as A/52 §7.10
/// states it: the bit-serial form, not a reflected table.
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The reconstruction-level index closest to `value` for an `levels`-level
/// symmetric quantizer — the forward of [`crate::tables::symmetric_level`].
fn quantize_index(levels: u32, value: f32) -> u32 {
    let k = ((value * levels as f32 + (levels as f32 - 1.0)) / 2.0).round();
    k.clamp(0.0, (levels - 1) as f32) as u32
}

/// Writes a block's mantissas in stream order. The grouped quantizers (bap
/// 1, 2, 4) are the subtle part: [`crate::mantissa::Mantissas`] reads a whole
/// group's code word at the *first* of its 3 (or 2) values and the later
/// members consume nothing, with the group running on across channels
/// (§7.3.5). So the code word has to be emitted where the first member sits,
/// which means looking ahead past whatever other-bap mantissas lie in between
/// for the group's remaining members. A group the block ends partway through
/// is padded with its midpoint (zero); the decoder never reads the padding.
fn write_mantissas(w: &mut BitWriter, values: &[(u8, f32)]) {
    let mut left = [0usize; 16];
    for (i, &(bap, value)) in values.iter().enumerate() {
        let (levels, bits, size, radix) = match bap {
            0 => continue,
            1 => (QUANT_LEVELS[1], 5, 3, 3),
            2 => (QUANT_LEVELS[2], 7, 3, 5),
            4 => (QUANT_LEVELS[4], 7, 2, 11),
            3 | 5 => {
                let bits = QNTZTAB[bap as usize];
                w.write_bits(quantize_index(QUANT_LEVELS[bap as usize], value), bits);
                continue;
            }
            _ => {
                // 6..=15: asymmetric two's complement (§7.3.2).
                let bits = QNTZTAB[(bap as usize).min(15)];
                let scale = (1u32 << (bits - 1)) as f32;
                let signed = (value * scale).round().clamp(-scale, scale - 1.0) as i32;
                w.write_signed(signed, bits);
                continue;
            }
        };
        if left[bap as usize] == 0 {
            let mut code = 0u32;
            let mut members = values[i..].iter().filter(|v| v.0 == bap).map(|v| quantize_index(levels, v.1));
            for _ in 0..size {
                code = code * radix + members.next().unwrap_or((levels - 1) / 2);
            }
            w.write_bits(code, bits);
            left[bap as usize] = size;
        }
        left[bap as usize] -= 1;
    }
}

/// Fits an ideal exponent envelope to what the group-diff coding can carry
/// (each step moves the running exponent by at most 2, and the first bin's
/// absolute exponent has 4 bits), only ever *lowering* exponents to get
/// there: a lower exponent than ideal costs mantissa precision, a higher one
/// clips the mantissa at full scale and loses the coefficient — which is
/// exactly what a one-directional "walk toward the ideal" does on the rising
/// edge of a spectral peak. A backward then a forward min-pass make every
/// neighbouring difference fit, so [`write_exp_groups`]' clamp is then a no-op
/// and the decoder sees precisely this array.
fn smooth_exps(exps: &mut [u8]) {
    exps[0] = exps[0].min(15);
    for i in (0..exps.len() - 1).rev() {
        exps[i] = exps[i].min(exps[i + 1] + 2);
    }
    for i in 1..exps.len() {
        exps[i] = exps[i].min(exps[i - 1] + 2);
    }
}

/// Writes an exponent set as [`plan_exponents`] produced it: the 4-bit
/// absolute exponent, then 7-bit groups of three differentials. `sent` has
/// already been through [`smooth_exps`], so every differential is within ±2
/// and the decoder reconstructs precisely these values.
fn write_exp_groups(w: &mut BitWriter, sent: &[u8]) {
    w.write_bits(u32::from(sent[0]), 4);
    let mut prev = i32::from(sent[0]);
    for grp in sent[1..].chunks_exact(3) {
        let mut code = 0u32;
        for &v in grp {
            let diff = i32::from(v) - prev;
            debug_assert!((-2..=2).contains(&diff));
            code = code * 5 + (diff + 2) as u32;
            prev = i32::from(v);
        }
        w.write_bits(code, 7);
    }
}

/// PCM in, AC-3 syncframes out.
pub struct Ac3Encoder {
    config: EncoderConfig,
    params: CodecParameters,
    acmod: Acmod,
    lfeon: bool,
    nfchans: usize,
    /// Coded channels, LFE included when present; index `nfchans` is the LFE
    /// slot whenever `lfeon`.
    coded: usize,
    fscod: usize,
    frmsizecod: u8,
    frame_bytes: usize,
    order: Vec<Option<usize>>,
    mdct: Mdct<f32>,
    window: [f32; 512],
    /// The decoder's short (block-switched) inverse as a matrix, 512 samples
    /// per coefficient, row-major by coefficient — the forward short
    /// transform is its transpose ([`Ac3Encoder::forward_short`]).
    short_basis: Vec<f32>,
    chbwcod: u32,
    fbw_endmant: usize,
    stats: EncodeStats,
    /// Previous block's tail, per coded channel — the MDCT's 50% overlap.
    history: Vec<[f32; COEFFS]>,
    pcm: Vec<f32>,
    packets: std::collections::VecDeque<Vec<u8>>,
    finished: bool,
}

impl Ac3Encoder {
    /// An encoder for this configuration.
    pub fn new(config: EncoderConfig) -> Result<Ac3Encoder> {
        let (acmod, lfeon) = acmod_for(config.channels)?;
        let nfchans = acmod.nfchans();
        let coded = nfchans + usize::from(lfeon);
        let fscod = SAMPLE_RATE
            .iter()
            .position(|&r| r == config.sample_rate)
            .ok_or_else(|| {
                Error::unsupported(
                    format!("{} Hz", config.sample_rate),
                    "AC-3 codes 48000, 44100 or 32000 Hz",
                )
            })?;
        let kbps = config.bitrate_kbps.max(MIN_KBPS_PER_CHANNEL * coded as u32);
        let idx = BIT_RATE_KBPS
            .iter()
            .enumerate()
            .min_by_key(|&(_, &k)| k.abs_diff(kbps))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let frmsizecod = (idx * 2) as u8;
        let frame_bytes = usize::from(FRAME_SIZE_WORDS[frmsizecod as usize][fscod]) * 2;

        let mut window = [0.0f32; 512];
        for i in 0..256 {
            window[i] = WINDOW[i];
            window[511 - i] = WINDOW[i];
        }

        let mut imdct = Imdct::new();
        let mut short_basis = vec![0.0f32; COEFFS * 512];
        for k in 0..COEFFS {
            let mut spec = [0.0f32; COEFFS];
            spec[k] = 1.0;
            let (mut delay, mut out) = ([0.0f32; 256], [0.0f32; 256]);
            imdct.block(&spec, true, &mut delay, &mut out);
            let row = &mut short_basis[k * 512..(k + 1) * 512];
            for n in 0..256 {
                row[n] = out[n] / 2.0; // `block` doubles its first half
                row[256 + n] = delay[n];
            }
        }
        let chbwcod = chbwcod_for(BIT_RATE_KBPS[idx], nfchans);

        let mut params = CodecParameters::new(CodecId::Ac3);
        params.media = MediaParameters::Audio(AudioParameters {
            sample_rate: config.sample_rate,
            layout: ChannelLayout::from_count(coded),
            format: Some(SampleFormat::F32),
            bits_per_sample: None,
        });

        Ok(Ac3Encoder {
            order: channel_order(acmod, nfchans, lfeon),
            config,
            params,
            acmod,
            lfeon,
            nfchans,
            coded,
            fscod,
            frmsizecod,
            frame_bytes,
            mdct: Mdct::new(512),
            window,
            short_basis,
            chbwcod,
            fbw_endmant: (chbwcod as usize + 12) * 3 + 37,
            stats: EncodeStats::default(),
            history: vec![[0.0; COEFFS]; coded],
            pcm: Vec::new(),
            packets: std::collections::VecDeque::new(),
            finished: false,
        })
    }

    /// Codec parameters this encoder declares.
    pub fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    /// Samples this encoder emits before the first audible one: half a
    /// window (one block), always, because the very first block has no
    /// previous block to overlap-add against and is pure priming. A muxer
    /// writing this track's `elst`/`CodecDelay` states it here.
    pub fn encoder_delay(&self) -> usize {
        COEFFS
    }

    /// Coding decisions summed over every frame so far.
    pub fn stats(&self) -> EncodeStats {
        self.stats
    }

    /// Feeds interleaved `f32` PCM, family order (L, R, C, LFE, Ls, Rs — the
    /// same order [`crate::Ac3Decoder`] hands frames out in).
    pub fn push_pcm_f32(&mut self, interleaved: &[f32]) -> Result<()> {
        self.pcm.extend_from_slice(interleaved);
        self.drain();
        Ok(())
    }

    /// Ends the stream: pushes the last block's samples out of the overlap
    /// (see [`Ac3Encoder::encoder_delay`]), pads the tail to a whole frame
    /// and flushes it.
    pub fn finish(&mut self) {
        let need = FRAME_SAMPLES * self.coded;
        if !self.pcm.is_empty() || self.stats.frames > 0 {
            self.pcm.resize(self.pcm.len() + COEFFS * self.coded, 0.0);
            let padded = self.pcm.len().div_ceil(need) * need;
            self.pcm.resize(padded, 0.0);
        }
        self.drain();
        self.finished = true;
    }

    fn drain(&mut self) {
        let need = FRAME_SAMPLES * self.coded;
        while self.pcm.len() >= need {
            let block: Vec<f32> = self.pcm.drain(..need).collect();
            let packet = self.encode_frame(&block);
            self.packets.push_back(packet);
        }
    }

    /// The next encoded syncframe; [`Error::Eof`] once drained after
    /// [`Ac3Encoder::finish`], [`Error::NeedMore`] before that.
    pub fn next_packet(&mut self) -> Result<Vec<u8>> {
        match self.packets.pop_front() {
            Some(p) => Ok(p),
            None if self.finished => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    /// One syncframe from `interleaved` (family-order PCM, `coded` channels *
    /// [`FRAME_SAMPLES`] samples).
    fn encode_frame(&mut self, interleaved: &[f32]) -> Vec<u8> {
        // Deinterleave into coded-channel order, pick long or short transform
        // per block from the transient detector, and run the MDCT.
        let mut coeffs = vec![vec![[0.0f32; COEFFS]; self.coded]; BLOCKS];
        let mut blksw = vec![vec![false; self.coded]; BLOCKS];
        for ch in 0..self.coded {
            let slot = if ch < self.nfchans {
                self.order.iter().position(|&s| s == Some(ch)).unwrap()
            } else {
                self.order.iter().position(|&s| s.is_none()).unwrap()
            };
            for blk in 0..BLOCKS {
                let mut window_in = [0.0f32; 512];
                window_in[..256].copy_from_slice(&self.history[ch]);
                for n in 0..256 {
                    window_in[256 + n] = interleaved[(blk * 256 + n) * self.coded + slot];
                }
                // The LFE has no blksw bit in the stream.
                let short = ch < self.nfchans && is_transient(&window_in);
                blksw[blk][ch] = short;
                let mut spectrum = [0.0f32; COEFFS];
                if short {
                    self.forward_short(&window_in, &mut spectrum);
                } else {
                    self.mdct
                        .forward_windowed(&window_in, &self.window, &mut spectrum);
                    for v in &mut spectrum {
                        // Measured in `tests::tdac_*`: this makes the forward the
                        // exact adjoint of the decoder's transform.rs IMDCT.
                        *v *= FORWARD_GAIN;
                    }
                }
                self.history[ch].copy_from_slice(&window_in[256..]);
                coeffs[blk][ch] = spectrum;
            }
        }

        let endmant: Vec<usize> = (0..self.coded)
            .map(|ch| if ch < self.nfchans { self.fbw_endmant } else { LFE_ENDMANT })
            .collect();
        let budget_bits = self.frame_bytes as u64 * 8 - 2 /* auxdataflag, crcrsv */ - 16 /* crc2 */;

        // Rate loop: the exponent plan fixes the side information, then the
        // largest csnroffst (and, at that csnroffst, the largest fsnroffst)
        // whose mantissas still fit is found by binary search — both are
        // monotone in bits spent. A plan whose side information alone does
        // not fit (a frame of strategy churn at a low bit rate) is replaced
        // by the conservative one: a single D45 envelope for the whole frame.
        let mut chosen: Option<(BitWriter, Plan, u8)> = None;
        for conservative in [false, true] {
            let plan = self.plan(&coeffs, &blksw, &endmant, conservative);
            let fits = |c: u8, f: u8| {
                let bap = self.allocate(&plan, &endmant, c, f);
                let w = self.write_frame(&coeffs, &plan, &endmant, &bap, c, f);
                (w.bit_len() <= budget_bits).then_some(w)
            };
            if fits(0, 0).is_none() {
                continue;
            }
            let (mut lo, mut hi) = (0u8, 63u8);
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                if fits(mid, 0).is_some() { lo = mid } else { hi = mid - 1 }
            }
            let c = lo;
            let (mut lo, mut hi) = (0u8, 15u8);
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                if fits(c, mid).is_some() { lo = mid } else { hi = mid - 1 }
            }
            chosen = Some((fits(c, lo).unwrap(), plan, c));
            break;
        }
        let (w, plan, csnroffst) = chosen.unwrap_or_else(|| {
            // corner-cut: unreachable with the 32 kbps/channel floor `new`
            // applies (the conservative plan's side info is ~150 bits per
            // channel per frame); below that the frame would be truncated.
            // Upgrade path: drop chbwcod until it fits.
            debug_assert!(false, "side information does not fit the frame");
            let plan = self.plan(&coeffs, &blksw, &endmant, true);
            let bap = self.allocate(&plan, &endmant, 0, 0);
            (self.write_frame(&coeffs, &plan, &endmant, &bap, 0, 0), plan, 0)
        });

        self.stats.frames += 1;
        self.stats.csnroffst_sum += u64::from(csnroffst);
        self.stats.bits_used += w.bit_len();
        self.stats.bits_budget += budget_bits;
        for blk in 0..BLOCKS {
            for ch in 0..self.nfchans {
                self.stats.blksw_blocks += u64::from(blksw[blk][ch]);
                match plan.strategy[blk][ch] {
                    Strategy::Reuse => self.stats.reuse += 1,
                    Strategy::D15 => self.stats.d15 += 1,
                    Strategy::D25 => self.stats.d25 += 1,
                    Strategy::D45 => self.stats.d45 += 1,
                }
            }
        }
        self.finish_frame(w)
    }

    /// The two 256-point transforms of a block-switched block (§7.9.4.2), as
    /// the transpose of the decoder's own short inverse (`short_basis`), so
    /// the pair is TDAC-exact by construction — see `tests::tdac_short_*`.
    fn forward_short(&self, window_in: &[f32; 512], spectrum: &mut [f32; COEFFS]) {
        for (k, out) in spectrum.iter_mut().enumerate() {
            let row = &self.short_basis[k * 512..(k + 1) * 512];
            *out = SHORT_GAIN * row.iter().zip(window_in).map(|(a, b)| a * b).sum::<f32>();
        }
    }

    /// Exponent strategy and exponents for every block and channel.
    ///
    /// Block 0 always sends. A later block re-sends when its block switch
    /// flag changed or its envelope moved ([`envelope_moved`]) since the last
    /// send; otherwise it reuses. Each run of
    /// blocks sharing one set codes the run's loudest magnitude per bin, so no
    /// block's mantissa is asked for more than its exponent's headroom. The
    /// sending block uses D15 for the LFE channel, D25 when block-switched
    /// (the short transform's interleaved halves make D15's per-bin cost too
    /// high — see the D25-vs-D15 note below), D45 for a run of 3+ blocks
    /// (stationary), D25 otherwise. `conservative` forces one D45 set per
    /// channel for the whole frame: the cheapest legal side information.
    fn plan(&self, coeffs: &[Vec<[f32; COEFFS]>], blksw: &[Vec<bool>], endmant: &[usize], conservative: bool) -> Plan {
        let ideal: Vec<Vec<[u8; COEFFS]>> = coeffs
            .iter()
            .map(|blk| (0..self.coded).map(|ch| ideal_exps(&blk[ch], endmant[ch])).collect())
            .collect();
        let mut plan = Plan {
            blksw: blksw.to_vec(),
            strategy: vec![vec![Strategy::Reuse; self.coded]; BLOCKS],
            sent: vec![vec![Vec::new(); self.coded]; BLOCKS],
            exps: vec![vec![[24u8; COEFFS]; self.coded]; BLOCKS],
        };
        for ch in 0..self.coded {
            let mut starts = vec![0usize];
            if !conservative {
                for blk in 1..BLOCKS {
                    let last = *starts.last().unwrap();
                    if blksw[blk][ch] != blksw[blk - 1][ch]
                        || envelope_moved(&ideal[blk][ch], &ideal[last][ch], endmant[ch])
                    {
                        starts.push(blk);
                    }
                }
            }
            for (i, &start) in starts.iter().enumerate() {
                let end = starts.get(i + 1).copied().unwrap_or(BLOCKS);
                let mut env = ideal[start][ch];
                for blk in start + 1..end {
                    for bin in 0..endmant[ch] {
                        env[bin] = env[bin].min(ideal[blk][ch][bin]);
                    }
                }
                let strategy = if ch >= self.nfchans {
                    Strategy::D15
                } else if blksw[start][ch] {
                    // The short halves interleave, so pairs (X1[k], X2[k])
                    // share one exponent: the louder half's. D15 would
                    // follow the alternation exactly but at 592 bits per
                    // send — more than a block's whole budget at 96 kbps per
                    // channel — against D25's 298.
                    Strategy::D25
                } else if conservative || end - start >= 3 {
                    Strategy::D45
                } else {
                    Strategy::D25
                };
                let (sent, exps) = plan_exponents(&env, endmant[ch], strategy);
                plan.strategy[start][ch] = strategy;
                plan.sent[start][ch] = sent;
                for blk in start..end {
                    plan.exps[blk][ch] = exps;
                }
            }
        }
        plan
    }

    /// Bit allocation for every block and coded channel at one SNR offset.
    fn allocate(&self, plan: &Plan, endmant: &[usize], csnroffst: u8, fsnroffst: u8) -> Vec<Vec<[u8; COEFFS]>> {
        let snroffset = (((i32::from(csnroffst) - 15) << 4) + i32::from(fsnroffst)) << 2;
        // §7.2.2.1.1: all-zero SNR offsets mean no mantissa bits at all —
        // the decoder short-circuits there instead of running the model.
        if csnroffst == 0 && fsnroffst == 0 {
            return vec![vec![[0u8; COEFFS]; self.coded]; BLOCKS];
        }
        let mut out: Vec<Vec<[u8; COEFFS]>> = Vec::with_capacity(BLOCKS);
        for blk in 0..BLOCKS {
            let baps = (0..self.coded)
                .map(|ch| {
                    if blk > 0 && plan.strategy[blk][ch] == Strategy::Reuse {
                        return out[blk - 1][ch];
                    }
                    let kind = if ch < self.nfchans { Channel::Fbw } else { Channel::Lfe };
                    let alloc = Allocation {
                        fscod: self.fscod,
                        params: BA_PARAMS,
                        range: (0, endmant[ch]),
                        fgaincod: FGAINCOD,
                        snroffset,
                        kind,
                        dba: None,
                        high_efficiency: false,
                    };
                    let mut bap = [0u8; COEFFS];
                    bitalloc::compute(&alloc, &plan.exps[blk][ch], &mut bap);
                    bap
                })
                .collect();
            out.push(baps);
        }
        out
    }

    /// Writes syncinfo, bsi and the six audblks; mantissas and their bit cost
    /// come from `coeffs`, `plan` and `bap` (all already computed for this
    /// frame). Everything up to the crc/padding tail this function leaves for
    /// [`Ac3Encoder::finish_frame`], so the rate loop in
    /// [`Ac3Encoder::encode_frame`] can measure a candidate's length before
    /// paying for that.
    fn write_frame(
        &self,
        coeffs: &[Vec<[f32; COEFFS]>],
        plan: &Plan,
        endmant: &[usize],
        bap: &[Vec<[u8; COEFFS]>],
        csnroffst: u8,
        fsnroffst: u8,
    ) -> BitWriter {
        let mut w = BitWriter::with_capacity(self.frame_bytes);

        // syncinfo(): crc1 is patched into the bytes afterwards.
        w.write_bits(0x0B77, 16);
        w.write_bits(0, 16);
        w.write_bits(self.fscod as u32, 2);
        w.write_bits(u32::from(self.frmsizecod), 6);

        // bsi()
        w.write_bits(8, 5); // bsid
        w.write_bits(0, 3); // bsmod
        w.write_bits(u32::from(self.acmod.code()), 3);
        if self.acmod.code() & 1 != 0 && self.acmod.code() != 1 {
            w.write_bits(1, 2); // cmixlev
        }
        if self.acmod.code() & 4 != 0 {
            w.write_bits(1, 2); // surmixlev
        }
        if self.acmod == Acmod::Stereo {
            w.write_bits(0, 2); // dsurmod
        }
        w.write_bit(self.lfeon);
        w.write_bits(31, 5); // dialnorm
        w.write_bit(false); // compr
        w.write_bit(false); // langcod
        w.write_bit(false); // mixlevel/roomtyp
        w.write_bit(false); // copyrightb
        w.write_bit(false); // origbs
        w.write_bit(false); // timecod1
        w.write_bit(false); // timecod2
        w.write_bit(false); // addbsi

        for blk in 0..BLOCKS {
            self.write_block(&mut w, blk, coeffs, plan, endmant, bap, csnroffst, fsnroffst);
        }
        w
    }

    #[allow(clippy::too_many_arguments)]
    fn write_block(
        &self,
        w: &mut BitWriter,
        blk: usize,
        coeffs: &[Vec<[f32; COEFFS]>],
        plan: &Plan,
        endmant: &[usize],
        bap: &[Vec<[u8; COEFFS]>],
        csnroffst: u8,
        fsnroffst: u8,
    ) {
        let first = blk == 0;
        for ch in 0..self.nfchans {
            w.write_bit(plan.blksw[blk][ch]);
        }
        for _ in 0..self.nfchans {
            w.write_bit(false); // dithflag
        }
        w.write_bit(false); // dynrnge
        w.write_bit(first); // cplstre: block 0 must declare a strategy
        if first {
            w.write_bit(false); // cplinu: no coupling
        }
        if self.acmod == Acmod::Stereo {
            w.write_bit(false); // rematstr (no rematrixing)
        }
        for ch in 0..self.nfchans {
            w.write_bits(strategy_code(plan.strategy[blk][ch]), 2); // chexpstr
        }
        if self.lfeon {
            w.write_bit(plan.strategy[blk][self.nfchans] != Strategy::Reuse); // lfeexpstr
        }
        for ch in 0..self.nfchans {
            if plan.strategy[blk][ch] != Strategy::Reuse {
                w.write_bits(self.chbwcod, 6);
            }
        }
        for ch in 0..self.nfchans {
            if plan.strategy[blk][ch] != Strategy::Reuse {
                write_exp_groups(w, &plan.sent[blk][ch]);
                w.write_bits(0, 2); // gainrng
            }
        }
        if self.lfeon && plan.strategy[blk][self.nfchans] != Strategy::Reuse {
            write_exp_groups(w, &plan.sent[blk][self.nfchans]);
        }
        // The allocation parameters and SNR offsets are constant across the
        // frame, so they go once, in block 0 (where they are mandatory); the
        // decoder carries them forward through blocks 1-5.
        w.write_bit(first); // baie
        if first {
            w.write_bits(u32::from(BA_PARAMS.sdcycod), 2);
            w.write_bits(u32::from(BA_PARAMS.fdcycod), 2);
            w.write_bits(u32::from(BA_PARAMS.sgaincod), 2);
            w.write_bits(u32::from(BA_PARAMS.dbpbcod), 2);
            w.write_bits(u32::from(BA_PARAMS.floorcod), 3);
        }
        w.write_bit(first); // snroffste
        if first {
            w.write_bits(u32::from(csnroffst), 6);
            for _ in 0..self.coded {
                w.write_bits(u32::from(fsnroffst), 4);
                w.write_bits(u32::from(FGAINCOD), 3);
            }
        }
        // no coupling leak (cplinu is always false: no bit here at all)
        w.write_bit(false); // deltbaie
        w.write_bit(false); // skiple

        let values: Vec<(u8, f32)> = (0..self.coded)
            .flat_map(|ch| (0..endmant[ch]).map(move |bin| (ch, bin)))
            .map(|(ch, bin)| (bap[blk][ch][bin], coeffs[blk][ch][bin] * scale(plan.exps[blk][ch][bin])))
            .collect();
        write_mantissas(w, &values);
    }

    /// Patches crc1/crc2 into a written frame and zero-pads it to
    /// [`Ac3Encoder::frame_bytes`] — the syncframe's own length, so every
    /// frame is exactly [`crate::tables::FRAME_SIZE_WORDS`].
    fn finish_frame(&self, mut w: BitWriter) -> Vec<u8> {
        w.write_bit(false); // auxdataflag
        w.write_bit(false); // crcrsv
        while (w.bit_len() as usize) < self.frame_bytes * 8 - 16 {
            w.write_bit(false);
        }
        w.write_bits(0, 16); // crc2 placeholder
        w.align_to_byte();
        let mut bytes = w.into_bytes();
        bytes.resize(self.frame_bytes, 0);

        // §7.10: crc1 covers the first 5/8 of the frame, crc2 the rest.
        let frame_size_58 = ((self.frame_bytes >> 2) + (self.frame_bytes >> 4)) << 1;
        let crc1 = crc16(&bytes[4..frame_size_58]);
        bytes[2..4].copy_from_slice(&crc1.to_be_bytes());
        let crc2 = crc16(&bytes[2..self.frame_bytes - 2]);
        bytes[self.frame_bytes - 2..].copy_from_slice(&crc2.to_be_bytes());
        bytes
    }
}

/// Per-frame coding decisions, made before the rate loop runs so every
/// candidate allocation is measured against the same side information.
struct Plan {
    /// Block switch per block per coded channel (always false for the LFE).
    blksw: Vec<Vec<bool>>,
    /// Exponent strategy per block per coded channel.
    strategy: Vec<Vec<Strategy>>,
    /// The exponent values as sent — the absolute one, then one per
    /// differential — per block per channel; empty on `Reuse`.
    sent: Vec<Vec<Vec<u8>>>,
    /// The exponents the decoder holds during each block, per channel: what
    /// the bit allocation and the quantisation both run on.
    exps: Vec<Vec<[u8; COEFFS]>>,
}

/// What the encoder decided, summed over every frame so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncodeStats {
    /// Frames coded.
    pub frames: u64,
    /// Block-switched (short transform) blocks, over full-bandwidth channels.
    pub blksw_blocks: u64,
    /// Block/channel pairs (full-bandwidth) that sent D15 exponents.
    pub d15: u64,
    /// ... D25.
    pub d25: u64,
    /// ... D45.
    pub d45: u64,
    /// ... that reused the previous block's exponents.
    pub reuse: u64,
    /// Sum of the `csnroffst` each frame settled on.
    pub csnroffst_sum: u64,
    /// Bits the frames' side information and mantissas occupied, before padding.
    pub bits_used: u64,
    /// Bits the frames had available for them.
    pub bits_budget: u64,
}

/// The 2-bit `chexpstr` code (the inverse of [`Strategy::from_code`]).
fn strategy_code(strategy: Strategy) -> u32 {
    match strategy {
        Strategy::Reuse => 0,
        Strategy::D15 => 1,
        Strategy::D25 => 2,
        Strategy::D45 => 3,
    }
}

/// `chbwcod` from the bit rate per full-bandwidth channel: the whole band when
/// there are bits to fill it, tapering down at low rates so the bits go to the
/// bands that carry the music. At 48 kHz the rows cut off at about 24, 20, 18,
/// 16 and 14 kHz.
fn chbwcod_for(kbps: u32, nfchans: usize) -> u32 {
    match kbps / nfchans as u32 {
        80.. => 60,
        56.. => 48,
        40.. => 40,
        28.. => 32,
        _ => 24,
    }
}

/// §7.9.1's spirit in its simplest form: a first-order high-pass, then the
/// energy of each of the window's eight 64-sample segments; a segment in the
/// block's own half (the new 256 samples) that is `TRANSIENT_RATIO` above
/// everything before it, and above the silence floor, is an attack worth a
/// short transform's time resolution (pre-echo would otherwise smear over
/// the whole 512-sample window).
fn is_transient(window_in: &[f32; 512]) -> bool {
    let mut energy = [0.0f32; 8];
    for (i, seg) in energy.iter_mut().enumerate() {
        *seg = (i * 64..(i + 1) * 64)
            .map(|n| {
                let d = window_in[n] - window_in[n.saturating_sub(1)];
                d * d
            })
            .sum();
    }
    (4..8).any(|j| {
        let before = energy[..j].iter().copied().fold(0.0f32, f32::max);
        energy[j] > TRANSIENT_FLOOR && energy[j] > TRANSIENT_RATIO * before
    })
}

/// `floor(-log2 |X|)` per bin, clamped to 0..=24 (24 also for zero), up to
/// `endmant`: the exponent that leaves each coefficient's mantissa in
/// `[0.5, 1)`.
fn ideal_exps(coeffs: &[f32; COEFFS], endmant: usize) -> [u8; COEFFS] {
    let mut e = [24u8; COEFFS];
    for bin in 0..endmant {
        let mag = coeffs[bin].abs();
        e[bin] = if mag <= 0.0 { 24 } else { (-mag.log2()).floor().clamp(0.0, 24.0) as u8 };
    }
    e
}

/// Whether two ideal envelopes differ enough to be worth re-sending: the peak
/// (minimum exponent) of some 12-bin band within 60 dB of the loudest band
/// moved by more than [`RESEND_STEPS`]. A re-send costs 150-300 bits per
/// channel — at 192 kbps stereo that is 30-60 % of a block's whole budget —
/// so the bands the masking model will starve anyway do not get a vote.
fn envelope_moved(a: &[u8; COEFFS], b: &[u8; COEFFS], endmant: usize) -> bool {
    let loudest = a[..endmant].iter().min().unwrap().min(b[..endmant].iter().min().unwrap());
    a[..endmant].chunks(12).zip(b[..endmant].chunks(12)).any(|(x, y)| {
        let (px, py) = (*x.iter().min().unwrap(), *y.iter().min().unwrap());
        px.min(py) <= loudest + 10 && px.abs_diff(py) > RESEND_STEPS
    })
}

/// The exponent set a channel sends for a run of blocks: `ideal` is the run's
/// envelope (min over the run's blocks of each bin's ideal exponent), grouped
/// by `strategy` (min over each group's bins), fitted to the ±2 differential
/// coding by [`smooth_exps`], then expanded to what the decoder will hold.
/// Returns (values as sent, decoded exponents).
fn plan_exponents(ideal: &[u8; COEFFS], endmant: usize, strategy: Strategy) -> (Vec<u8>, [u8; COEFFS]) {
    let gs = strategy.group_size();
    let ngrps = strategy.fbw_groups(endmant);
    let mut sent = vec![ideal[0].min(15)];
    for g in 0..ngrps * 3 {
        let lo = 1 + g * gs;
        let hi = (lo + gs).min(endmant);
        sent.push(if lo < hi { *ideal[lo..hi].iter().min().unwrap() } else { 24 });
    }
    smooth_exps(&mut sent);
    let mut exps = [24u8; COEFFS];
    exps[0] = sent[0];
    for g in 0..ngrps * 3 {
        for bin in 1 + g * gs..(1 + (g + 1) * gs).min(COEFFS) {
            exps[bin] = sent[g + 1];
        }
    }
    (sent, exps)
}

/// `2^exponent` — the inverse of [`crate::mantissa::scale`], since the
/// encoder starts from a linear coefficient and has to divide it back out to
/// the `[-1, 1)` mantissa the quantizers expect.
fn scale(exponent: u8) -> f32 {
    f32::from_bits(127u32.wrapping_add(u32::from(exponent)) << 23)
}

/// Little-endian floats out of a plane.
fn floats(plane: &Buf, count: usize) -> Vec<f32> {
    plane
        .chunks_exact(4)
        .take(count)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

impl Encoder for Ac3Encoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let Frame::Audio(audio) = frame else {
            return Err(Error::corrupt("video frame pushed into an audio encoder"));
        };
        if audio.format != SampleFormat::F32 {
            return Err(Error::unsupported(
                format!("{:?} input", audio.format),
                "this encoder takes 32-bit float samples",
            ));
        }
        let channels = self.coded;
        let interleaved = if audio.planar {
            let mut out = vec![0.0f32; audio.samples * channels];
            for (c, plane) in audio.data.iter().enumerate().take(channels) {
                for (n, v) in floats(plane, audio.samples).into_iter().enumerate() {
                    out[n * channels + c] = v;
                }
            }
            out
        } else {
            floats(&audio.data[0], audio.samples * channels)
        };
        self.push_pcm_f32(&interleaved)
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        let data = self.next_packet()?;
        let base = TimeBase::new(1, i64::from(self.config.sample_rate));
        let mut packet = Packet::new(0, base, data);
        packet.duration = Some(FRAME_SAMPLES as i64);
        Ok(packet)
    }

    fn flush(&mut self) -> Result<()> {
        self.finish();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u64) -> f32 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*seed >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }

    /// A/52 §7.9.4 forward: X[k] = Σ x[n] w[n] cos(2π/N (n + 1/2 + N/4)(k + 1/2)), N = 512.
    fn direct_forward(x: &[f32; 512], window: &[f32; 512]) -> [f32; COEFFS] {
        let mut out = [0.0f32; COEFFS];
        for (k, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for n in 0..512 {
                let arg = 2.0 * std::f64::consts::PI / 512.0 * (n as f64 + 0.5 + 128.0) * (k as f64 + 0.5);
                acc += f64::from(x[n] * window[n]) * arg.cos();
            }
            *o = acc as f32;
        }
        out
    }

    /// Runs `forward` then the decoder's inverse over 7 overlapping blocks,
    /// block-switched where `pattern` says so (the short transform must be
    /// TDAC-exact against both a short and a long neighbour), and returns
    /// (worst sample error, least-squares gain out/in).
    fn tdac_pattern(pattern: [bool; 7], forward: impl Fn(&[f32; 512], bool) -> [f32; COEFFS]) -> (f32, f64) {
        let mut seed = 7u64;
        let signal: Vec<f32> = (0..256 * 8).map(|_| lcg(&mut seed)).collect();
        let mut imdct = Imdct::new();
        let mut delay = [0.0f32; 256];
        let mut out = [0.0f32; 256];
        let mut worst = 0.0f32;
        let (mut dot, mut nrm) = (0.0f64, 0.0f64);
        for (blk, &short) in pattern.iter().enumerate() {
            let mut x = [0.0f32; 512];
            x.copy_from_slice(&signal[blk * 256..blk * 256 + 512]);
            let spec = forward(&x, short);
            imdct.block(&spec, short, &mut delay, &mut out);
            if blk > 0 {
                // Output of block `blk` reconstructs input samples blk*256..+256.
                for n in 0..256 {
                    worst = worst.max((out[n] - signal[blk * 256 + n]).abs());
                    dot += f64::from(out[n] * signal[blk * 256 + n]);
                    nrm += f64::from(signal[blk * 256 + n] * signal[blk * 256 + n]);
                }
            }
        }
        (worst, dot / nrm)
    }

    fn tdac(forward: impl Fn(&[f32; 512]) -> [f32; COEFFS]) -> f32 {
        let (worst, gain) = tdac_pattern([false; 7], |x, _| forward(x));
        assert!((gain - 1.0).abs() < 1e-4, "least-squares gain out/in = {gain}");
        worst
    }

    #[test]
    fn tdac_short_forward_reconstructs_against_long_and_short_neighbours() {
        let mut enc = Ac3Encoder::new(EncoderConfig { sample_rate: 48000, channels: 1, bitrate_kbps: 192 }).unwrap();
        let window = enc.window;
        let basis = enc.short_basis.clone();
        let cell = std::cell::RefCell::new(&mut enc.mdct);
        let (worst, gain) = tdac_pattern([false, true, true, false, true, false, true], |x, short| {
            let mut s = [0.0f32; COEFFS];
            if short {
                for (k, out) in s.iter_mut().enumerate() {
                    let row = &basis[k * 512..(k + 1) * 512];
                    *out = SHORT_GAIN * row.iter().zip(x).map(|(a, b)| a * b).sum::<f32>();
                }
            } else {
                cell.borrow_mut().forward_windowed(x, &window, &mut s);
                for v in &mut s {
                    *v *= FORWARD_GAIN;
                }
            }
            s
        });
        let (w2, g2) = tdac_pattern([true; 7], |x, _| {
            let mut s = [0.0f32; COEFFS];
            for (k, out) in s.iter_mut().enumerate() {
                let row = &basis[k * 512..(k + 1) * 512];
                *out = SHORT_GAIN * row.iter().zip(x).map(|(a, b)| a * b).sum::<f32>();
            }
            s
        });
        eprintln!("all-short: worst {w2} gain {g2}; mixed: worst {worst} gain {gain}");
        assert!((gain - 1.0).abs() < 1e-4, "least-squares gain out/in = {gain}");
        assert!(worst < 1e-4, "short forward + Imdct TDAC error {worst}");
    }

    fn encode_one(pcm: &[f32]) -> EncodeStats {
        let mut enc = Ac3Encoder::new(EncoderConfig { sample_rate: 48000, channels: 1, bitrate_kbps: 96 }).unwrap();
        enc.push_pcm_f32(pcm).unwrap();
        enc.stats()
    }

    #[test]
    fn stationary_tone_plans_one_d45_set_and_reuses_it() {
        let pcm: Vec<f32> = (0..FRAME_SAMPLES * 2)
            .map(|n| 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 48000.0).sin())
            .collect();
        let stats = encode_one(&pcm);
        assert_eq!(stats.frames, 2);
        // Frame 0 opens from silence: an onset, so block 0 is legitimately
        // short and its first blocks may re-send; frame 1 is steady: one D45
        // set, five reuses.
        assert!(stats.blksw_blocks <= 1, "{stats:?}");
        assert!(stats.d45 >= 1 && stats.d15 == 0 && stats.d25 <= 1, "{stats:?}");
        assert!(stats.reuse >= 5, "{stats:?}");
        assert!(stats.bits_used * 10 > stats.bits_budget * 9, "rate loop left bits unused: {stats:?}");
    }

    #[test]
    fn a_click_switches_to_short_blocks_and_d25() {
        let mut pcm = vec![0.0f32; FRAME_SAMPLES];
        for n in 0..FRAME_SAMPLES {
            pcm[n] = 0.01 * ((n as f32) * 0.3).sin();
        }
        pcm[256 * 3 + 100] = 0.9;
        pcm[256 * 3 + 101] = -0.9;
        let stats = encode_one(&pcm);
        assert_eq!(stats.blksw_blocks, 1, "{stats:?}");
        assert!(stats.d25 >= 1 && stats.d15 == 0, "{stats:?}");
    }

    #[test]
    fn smoothed_exponents_never_rise_and_always_fit_the_group_coding() {
        let mut e = [24u8; COEFFS];
        e[100] = 1;
        e[0] = 20;
        let ideal = e;
        smooth_exps(&mut e);
        assert!(e.iter().zip(&ideal).all(|(s, i)| s <= i));
        assert!(e.windows(2).all(|w| (i32::from(w[0]) - i32::from(w[1])).abs() <= 2));
        assert_eq!((e[0], e[99], e[100], e[101]), (15, 3, 1, 3));
        let mut again = e;
        smooth_exps(&mut again);
        assert_eq!(again, e);
    }

    #[test]
    fn tdac_direct_forward_reconstructs() {
        let enc = Ac3Encoder::new(EncoderConfig { sample_rate: 48000, channels: 1, bitrate_kbps: 192 }).unwrap();
        let window = enc.window;
        // A/52's forward carries -2/N.
        let err = tdac(|x| {
            let mut s = direct_forward(x, &window);
            for v in &mut s {
                *v *= -2.0 / 512.0;
            }
            s
        });
        assert!(err < 1e-4, "direct forward + Imdct TDAC error {err}");
    }

    #[test]
    fn tdac_ec_dsp_forward_reconstructs() {
        let mut enc = Ac3Encoder::new(EncoderConfig { sample_rate: 48000, channels: 1, bitrate_kbps: 192 }).unwrap();
        let window = enc.window;
        let cell = std::cell::RefCell::new(&mut enc.mdct);
        let err = tdac(|x| {
            let mut s = [0.0f32; COEFFS];
            cell.borrow_mut().forward_windowed(x, &window, &mut s);
            for v in &mut s {
                *v *= FORWARD_GAIN;
            }
            s
        });
        assert!(err < 1e-4, "ec_dsp forward + Imdct TDAC error {err}");
    }
}
