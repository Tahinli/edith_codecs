//! An AC-3 encoder: PCM in, spec-valid ATSC A/52 syncframes out.
//!
//! This is the skeleton subtask: the bitstream writer mirrors the parser's
//! normative order (`syncinfo()`, `bsi()`, six `audblk()`s, `auxdata`/CRC) but
//! runs a *fixed* allocation rather than a real psychoacoustic search —
//! exponent strategy is D15 on block 0 and reused for the rest of the frame,
//! the bit allocation parameters are constants, and the only thing that
//! adapts per frame is `csnroffst`, stepped down until the fixed allocation's
//! mantissas fit the frame's byte budget. A2 replaces the fixed exponent
//! strategy and per-frame envelope with a real one; A3 replaces the
//! `csnroffst` step-down with a proper rate-controlled search
//! ([`Ac3Encoder::try_csnroffst`], [`Ac3Encoder::build_frame`]).
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
use crate::tables::{BIT_RATE_KBPS, FRAME_SIZE_WORDS, QNTZTAB, QUANT_LEVELS, SAMPLE_RATE, WINDOW};

/// Samples per channel one syncframe codes.
const FRAME_SAMPLES: usize = 1536;
/// Blocks per syncframe.
const BLOCKS: usize = 6;
/// Coefficients per channel per block ([`crate::decode::COEFFS`], not public).
const COEFFS: usize = 256;
/// `chbwcod` this encoder always sends: maximum bandwidth.
const CHBWCOD: u32 = 60;
/// Full-bandwidth `endmant` at [`CHBWCOD`]: `(chbwcod + 12) * 3 + 37`.
const FBW_ENDMANT: usize = (CHBWCOD as usize + 12) * 3 + 37;
/// LFE `endmant`, fixed by the standard.
const LFE_ENDMANT: usize = 7;
/// Scale from `ec_dsp::Mdct::forward_windowed`'s raw sum (which equals the
/// plain A/52 §7.9.4 cosine sum) to the coefficients `crate::transform::Imdct`
/// expects: the standard's `-2/N` (N = 512) forward normalisation, with its
/// `2.0x` on the way back being the decoder's own half of that pair.
const FORWARD_GAIN: f32 = -2.0 / 512.0;

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

/// Writes `ngrps` 7-bit exponent groups covering 3 bins each. `realized` must
/// already have been through [`smooth_exps`], so the values the decoder
/// reconstructs are exactly the ones [`crate::bitalloc::compute`] was run
/// against.
fn write_exp_groups(w: &mut BitWriter, realized: &[u8; COEFFS], mut prev: i32, first_bin: usize, ngrps: usize) {
    let mut bin = first_bin;
    for _ in 0..ngrps {
        let mut mapped = [0i32; 3];
        for slot in &mut mapped {
            let target = if bin < COEFFS { i32::from(realized[bin]) } else { prev };
            let diff = (target - prev).clamp(-2, 2);
            prev = (prev + diff).clamp(0, 24);
            *slot = diff + 2;
            bin += 1;
        }
        let code = (mapped[0] * 25 + mapped[1] * 5 + mapped[2]) as u32;
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
        let idx = BIT_RATE_KBPS
            .iter()
            .enumerate()
            .min_by_key(|&(_, &k)| k.abs_diff(config.bitrate_kbps))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let frmsizecod = (idx * 2) as u8;
        let frame_bytes = usize::from(FRAME_SIZE_WORDS[frmsizecod as usize][fscod]) * 2;

        let mut window = [0.0f32; 512];
        for i in 0..256 {
            window[i] = WINDOW[i];
            window[511 - i] = WINDOW[i];
        }

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

    /// Feeds interleaved `f32` PCM, family order (L, R, C, LFE, Ls, Rs — the
    /// same order [`crate::Ac3Decoder`] hands frames out in).
    pub fn push_pcm_f32(&mut self, interleaved: &[f32]) -> Result<()> {
        self.pcm.extend_from_slice(interleaved);
        self.drain();
        Ok(())
    }

    /// Ends the stream: pads the tail to a whole frame and flushes it.
    pub fn finish(&mut self) {
        let need = FRAME_SAMPLES * self.coded;
        if !self.pcm.is_empty() {
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
        // Deinterleave into coded-channel order and run the MDCT per block.
        let mut coeffs = vec![vec![[0.0f32; COEFFS]; self.coded]; BLOCKS];
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
                let mut spectrum = [0.0f32; COEFFS];
                self.mdct
                    .forward_windowed(&window_in, &self.window, &mut spectrum);
                for v in &mut spectrum {
                    // Measured in `tests::tdac_*`: this makes the forward the
                    // exact adjoint of the decoder's transform.rs IMDCT.
                    *v *= FORWARD_GAIN;
                }
                self.history[ch].copy_from_slice(&window_in[256..]);
                coeffs[blk][ch] = spectrum;
            }
        }

        // Exponents: one D15 envelope per coded channel, tracking the loudest
        // magnitude any of the frame's 6 blocks reaches at each bin, so no
        // block's mantissa is asked to represent more than the exponent's
        // headroom allows.
        let endmant: Vec<usize> = (0..self.coded)
            .map(|ch| if ch < self.nfchans { FBW_ENDMANT } else { LFE_ENDMANT })
            .collect();
        let exps: Vec<[u8; COEFFS]> = (0..self.coded)
            .map(|ch| {
                let mut e = [24u8; COEFFS];
                for bin in 0..endmant[ch] {
                    let mag = coeffs
                        .iter()
                        .map(|b| b[ch][bin].abs())
                        .fold(0.0f32, f32::max);
                    e[bin] = if mag <= 0.0 {
                        24
                    } else {
                        (-mag.log2()).floor().clamp(0.0, 24.0) as u8
                    };
                }
                e
            })
            .collect();
        // Fit the envelope to what the group-diff coding can carry, never
        // raising an exponent (that would clip its mantissas); the bit
        // allocation below then runs on exactly what the decoder will decode.
        let exps: Vec<[u8; COEFFS]> = exps
            .into_iter()
            .enumerate()
            .map(|(ch, mut e)| {
                smooth_exps(&mut e[..endmant[ch]]);
                e
            })
            .collect();

        // Fixed allocation: step csnroffst down from a generous default until
        // the mantissas it buys fit the frame. A3 replaces this with a rate
        // loop that also varies floorcod/gaincod; see `Ac3Encoder::try_csnroffst`.
        let mut csnroffst: i32 = 40;
        let budget_bits = self.frame_bytes as u64 * 8 - 2 /* auxdataflag, crcrsv */ - 16 /* crc2 */;
        let mut w;
        loop {
            let bap = self.allocate(&exps, &endmant, csnroffst as u8);
            w = self.write_frame(&coeffs, &exps, &endmant, &bap, csnroffst as u8);
            if w.bit_len() <= budget_bits || csnroffst == 0 {
                break;
            }
            csnroffst -= 1;
        }

        self.finish_frame(w)
    }

    /// Bit allocation for every coded channel at one `csnroffst`.
    fn allocate(&self, exps: &[[u8; COEFFS]], endmant: &[usize], csnroffst: u8) -> Vec<[u8; COEFFS]> {
        let snroffset = ((i32::from(csnroffst) - 15) << 4) << 2; // fsnroffst = 0
        let params = BitAllocParams {
            sdcycod: 2,
            fdcycod: 1,
            sgaincod: 1,
            dbpbcod: 2,
            floorcod: 7,
        };
        (0..self.coded)
            .map(|ch| {
                let kind = if ch < self.nfchans { Channel::Fbw } else { Channel::Lfe };
                let alloc = Allocation {
                    fscod: self.fscod,
                    params,
                    range: (0, endmant[ch]),
                    fgaincod: 4,
                    snroffset,
                    kind,
                    dba: None,
                    high_efficiency: false,
                };
                let mut bap = [0u8; COEFFS];
                bitalloc::compute(&alloc, &exps[ch], &mut bap);
                bap
            })
            .collect()
    }

    /// Writes syncinfo, bsi and the six audblks; mantissas and their bit cost
    /// come from `coeffs`, `exps` and `bap` (all already computed for this
    /// frame). Everything up to the crc/padding tail this function leaves for
    /// [`Ac3Encoder::finish_frame`], so the retry loop in
    /// [`Ac3Encoder::encode_frame`] can measure a candidate's length before
    /// paying for that.
    fn write_frame(
        &self,
        coeffs: &[Vec<[f32; COEFFS]>],
        exps: &[[u8; COEFFS]],
        endmant: &[usize],
        bap: &[[u8; COEFFS]],
        csnroffst: u8,
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
            self.write_block(&mut w, blk, coeffs, exps, endmant, bap, csnroffst);
        }
        w
    }

    #[allow(clippy::too_many_arguments)]
    fn write_block(
        &self,
        w: &mut BitWriter,
        blk: usize,
        coeffs: &[Vec<[f32; COEFFS]>],
        exps: &[[u8; COEFFS]],
        endmant: &[usize],
        bap: &[[u8; COEFFS]],
        csnroffst: u8,
    ) {
        let first = blk == 0;
        for _ in 0..self.nfchans {
            w.write_bit(false); // blksw
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
        for _ in 0..self.nfchans {
            w.write_bits(if first { 1 } else { 0 }, 2); // chexpstr: D15 / reuse
        }
        if self.lfeon {
            w.write_bit(first); // lfeexpstr: D15 / reuse
        }
        if first {
            for _ in 0..self.nfchans {
                w.write_bits(CHBWCOD, 6); // chbwcod
            }
        }
        if first {
            for ch in 0..self.nfchans {
                let absexp = exps[ch][0].min(15);
                w.write_bits(u32::from(absexp), 4);
                let ngrps = Strategy::D15.fbw_groups(endmant[ch]);
                write_exp_groups(w, &exps[ch], i32::from(absexp), 1, ngrps);
                w.write_bits(0, 2); // gainrng
            }
            if self.lfeon {
                let ch = self.nfchans;
                let absexp = exps[ch][0].min(15);
                w.write_bits(u32::from(absexp), 4);
                write_exp_groups(w, &exps[ch], i32::from(absexp), 1, 2);
            }
        }
        w.write_bit(first); // baie
        if first {
            w.write_bits(2, 2); // sdcycod
            w.write_bits(1, 2); // fdcycod
            w.write_bits(1, 2); // sgaincod
            w.write_bits(2, 2); // dbpbcod
            w.write_bits(7, 3); // floorcod
        }
        w.write_bit(first); // snroffste
        if first {
            w.write_bits(u32::from(csnroffst), 6);
            for _ in 0..self.nfchans {
                w.write_bits(0, 4); // fsnroffst
                w.write_bits(4, 3); // fgaincod
            }
            if self.lfeon {
                w.write_bits(0, 4);
                w.write_bits(4, 3);
            }
        }
        // no coupling leak (cplinu is always false: no bit here at all)
        w.write_bit(false); // deltbaie
        w.write_bit(false); // skiple

        let values: Vec<(u8, f32)> = (0..self.coded)
            .flat_map(|ch| (0..endmant[ch]).map(move |bin| (ch, bin)))
            .map(|(ch, bin)| (bap[ch][bin], coeffs[blk][ch][bin] * scale(exps[ch][bin])))
            .collect();
        write_mantissas(w, &values);
    }

    /// Patches crc1/crc2 into a written frame and zero-pads it to
    /// [`Ac3Encoder::frame_bytes`] — the syncframe's own length, so every
    /// frame is exactly [`crate::tables::FRAME_SIZE_WORDS`] regardless of how
    /// far under it this frame's fixed allocation landed.
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
    use crate::transform::Imdct;

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

    fn tdac(forward: impl Fn(&[f32; 512]) -> [f32; COEFFS]) -> f32 {
        let mut seed = 7u64;
        let signal: Vec<f32> = (0..256 * 6).map(|_| lcg(&mut seed)).collect();
        let mut imdct = Imdct::new();
        let mut delay = [0.0f32; 256];
        let mut out = [0.0f32; 256];
        let mut worst = 0.0f32;
        let (mut dot, mut nrm) = (0.0f64, 0.0f64);
        for blk in 0..5 {
            let mut x = [0.0f32; 512];
            x.copy_from_slice(&signal[blk * 256..blk * 256 + 512]);
            let spec = forward(&x);
            imdct.block(&spec, false, &mut delay, &mut out);
            if blk > 0 {
                // Output of block `blk` reconstructs input samples blk*256..+256.
                for n in 0..256 {
                    worst = worst.max((out[n] - signal[blk * 256 + n]).abs());
                    dot += f64::from(out[n] * signal[blk * 256 + n]);
                    nrm += f64::from(signal[blk * 256 + n] * signal[blk * 256 + n]);
                }
            }
        }
        assert!((dot / nrm - 1.0).abs() < 1e-4, "least-squares gain out/in = {}", dot / nrm);
        worst
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
