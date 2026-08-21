//! One ALAC frame: the element walk, the adaptive-Golomb residual decoder, the
//! adapting LPC predictor and the stereo un-mix.
//!
//! Written from the published ALAC format description (element tags and frame
//! header layout, the Golomb/Rice parameter adaptation with its zero-run mode,
//! the sign-adapting predictor and the mid/side un-mix). Nothing here is
//! derived from the reference decoder sources.

use crate::MagicCookie;
use ec_core::error::{Error, Result};

/// Mean-tracking parameters live in Q9.
const QBSHIFT: u32 = 9;
/// `1 << QBSHIFT`, the point where the mean says "the residuals are all zero".
const QB: u32 = 1 << QBSHIFT;
const MMULSHIFT: u32 = 2;
const MDENSHIFT: u32 = QBSHIFT - MMULSHIFT - 1;
const MOFF: u32 = 1 << (MDENSHIFT - 2);
const BITOFF: i32 = 24;
/// Unary prefixes this long are an escape: the value follows raw.
const MAX_PREFIX: u32 = 9;
/// Width of the raw value after a zero-run escape.
const MAX_RUN_BITS: u32 = 16;
/// A mean this large is clamped rather than allowed to run away.
const MEAN_CLAMP: u32 = 0xffff;

/// Syntax element tags, three bits at the head of every element.
const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_CCE: u32 = 2;
const ID_LFE: u32 = 3;
const ID_DSE: u32 = 4;
const ID_PCE: u32 = 5;
const ID_FIL: u32 = 6;
const ID_END: u32 = 7;

/// Coefficient count the predictor reads as "first-order difference" rather
/// than as a filter length.
const ORDER_ADAPT: u32 = 31;

/// An MSB-first bit reader that reads zeroes past the end of its buffer, so a
/// 32-bit look-ahead near the last byte of a frame is not an error in itself —
/// running *out* of frame is caught by the element walk instead.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, pos: 0 }
    }

    fn end(&self) -> usize {
        self.data.len() * 8
    }

    fn at_end(&self) -> bool {
        self.pos >= self.end()
    }

    /// The next 32 bits, zero-padded past the buffer.
    fn peek32(&self) -> u32 {
        let byte = self.pos >> 3;
        let off = (self.pos & 7) as u32;
        let mut v = 0u64;
        for i in 0..5 {
            v = (v << 8) | u64::from(self.data.get(byte + i).copied().unwrap_or(0));
        }
        (v >> (8 - off)) as u32
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.pos = self.pos.saturating_add(n);
        match self.pos > self.end() {
            true => Err(Error::NeedMore),
            false => Ok(()),
        }
    }

    fn read(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 32);
        if n == 0 {
            return Ok(0);
        }
        if self.pos + n as usize > self.end() {
            return Err(Error::NeedMore);
        }
        let v = self.peek32() >> (32 - n);
        self.pos += n as usize;
        Ok(v)
    }

    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

/// `floor(log2(x + 3))`, the Rice parameter the mean maps to.
fn lg3a(x: u32) -> u32 {
    31 - (x + 3).leading_zeros()
}

/// One adaptive-Golomb code: a unary prefix of `pre` (worth `pre * m`) and a
/// remainder of `k` bits, where remainders 0 and 1 share a `k-1`-bit slot. A
/// prefix of [`MAX_PREFIX`] ones escapes to a raw `maxbits` value.
fn golomb(bits: &mut Bits<'_>, m: u32, k: u32, maxbits: u32) -> u32 {
    let stream = bits.peek32();
    let pre = (!stream).leading_zeros();
    if pre >= MAX_PREFIX {
        // The raw value can be up to 32 bits wide (a 24-bit stereo element
        // codes 25), so it does not fit in the same 32-bit peek as the
        // prefix: step past the prefix and peek again.
        bits.pos += MAX_PREFIX as usize;
        let raw = match maxbits {
            0 => 0,
            n => bits.peek32() >> (32 - n),
        };
        bits.pos += maxbits as usize;
        return raw;
    }
    bits.pos += pre as usize + 1;
    let mut result = pre * m;
    if k > 1 {
        let v = (stream << (pre + 1)) >> (32 - k);
        match v >= 2 {
            true => {
                result += v - 1;
                bits.pos += k as usize;
            }
            // Zero and one are coded in `k - 1` bits, so only that many were
            // consumed and the value is zero.
            false => bits.pos += (k - 1) as usize,
        }
    }
    result
}

/// Golomb parameters, kept across a channel's residuals because the mean adapts
/// sample by sample.
struct Rice {
    mean: u32,
    pb: u32,
    kb: u32,
    wb: u32,
}

impl Rice {
    fn new(cfg: &MagicCookie, pb_factor: u32) -> Rice {
        let kb = u32::from(cfg.kb).clamp(1, 31);
        Rice {
            mean: u32::from(cfg.mb),
            pb: u32::from(cfg.pb) * pb_factor / 4,
            kb,
            wb: (1 << kb) - 1,
        }
    }

    /// `count` residuals into `out`, each already un-zigzagged into a signed
    /// difference.
    fn decode(&mut self, bits: &mut Bits<'_>, out: &mut [i32], chan_bits: u32) -> Result<()> {
        let mut zmode = 0u32;
        let mut c = 0usize;
        while c < out.len() {
            if bits.at_end() {
                return Err(Error::NeedMore);
            }
            let k = lg3a(self.mean >> QBSHIFT).min(self.kb);
            let m = (1u32 << k) - 1;
            let n = golomb(bits, m, k, chan_bits);

            // Low bit is the sign: 0, -1, 1, -2, 2, ...
            let ndecode = n.wrapping_add(zmode);
            let magnitude = ((ndecode as i64 + 1) >> 1) as i32;
            out[c] = match ndecode & 1 {
                0 => magnitude,
                _ => -magnitude,
            };
            c += 1;

            // Wrapping because the reference arithmetic is 32-bit unsigned:
            // an `n` big enough to overflow it is one the clamp below catches.
            self.mean = self
                .pb
                .wrapping_mul(ndecode)
                .wrapping_add(self.mean)
                .wrapping_sub(self.pb.wrapping_mul(self.mean) >> QBSHIFT);
            if n > MEAN_CLAMP {
                self.mean = MEAN_CLAMP;
            }

            zmode = 0;
            if (self.mean << MMULSHIFT) < QB && c < out.len() {
                zmode = 1;
                let k = (self.mean.leading_zeros() as i32 - BITOFF
                    + ((self.mean + MOFF) >> MDENSHIFT) as i32)
                    .clamp(0, 31) as u32;
                let mz = ((1u32 << k) - 1) & self.wb;
                let run = run_length(bits, mz, k) as usize;
                if c + run > out.len() {
                    return Err(Error::corrupt("ALAC: a zero run past the end of the frame"));
                }
                out[c..c + run].fill(0);
                c += run;
                // A run that hit the escape's ceiling is a run that was cut in
                // half, and the sample after it is not carrying a borrowed one.
                if run as u32 >= (1 << MAX_RUN_BITS) - 1 {
                    zmode = 0;
                }
                self.mean = 0;
            }
        }
        Ok(())
    }
}

/// A zero-run length: the same code as [`golomb`] with a 16-bit escape.
fn run_length(bits: &mut Bits<'_>, m: u32, k: u32) -> u32 {
    golomb(bits, m, k, MAX_RUN_BITS)
}

/// Sign of `x` as -1, 0 or 1.
fn sign(x: i32) -> i32 {
    (x > 0) as i32 - (x < 0) as i32
}

/// The LPC predictor, run backwards: residuals in `res`, samples out, with the
/// coefficients adapting on the sign of each residual exactly as the encoder's
/// did. `coefs` is scratch — it is mutated as the block is decoded.
fn predict(
    res: &[i32],
    out: &mut [i32],
    coefs: &mut [i32],
    chan_bits: u32,
    den_shift: u32,
) -> Result<()> {
    if res.is_empty() {
        return Ok(());
    }
    let n = res.len();
    let chan_shift = 32 - chan_bits;
    let clamp = |v: i32| ((v as u32) << chan_shift) as i32 >> chan_shift;
    let order = coefs.len();
    out[0] = res[0];

    if order == 0 {
        out[..n].copy_from_slice(res);
        return Ok(());
    }
    if order as u32 == ORDER_ADAPT {
        // Not a filter: a plain first-order difference, which is what the
        // second predictor pass of a "mode != 0" frame undoes.
        let mut prev = out[0];
        for j in 1..n {
            prev = clamp(res[j].wrapping_add(prev));
            out[j] = prev;
        }
        return Ok(());
    }
    if order >= n {
        // Fewer samples than the filter is long: warm-up is the whole block.
        for j in 1..n {
            out[j] = clamp(res[j].wrapping_add(out[j - 1]));
        }
        return Ok(());
    }

    // Warm-up: the first `order` samples are plain first-order differences.
    for j in 1..=order {
        out[j] = clamp(res[j].wrapping_add(out[j - 1]));
    }

    let den_half = match den_shift {
        0 => 0,
        s => 1i32 << (s - 1),
    };
    for j in order..n - 1 {
        let top = out[j - order];
        let mut sum = 0i32;
        for (k, &c) in coefs.iter().enumerate() {
            sum = sum.wrapping_add(c.wrapping_mul(out[j - k].wrapping_sub(top)));
        }
        let del = res[j + 1];
        let mut del0 = del;
        let sg = sign(del);
        let predicted = top.wrapping_add(sum.wrapping_add(den_half) >> den_shift);
        out[j + 1] = clamp(del.wrapping_add(predicted));

        // The coefficients walk one step towards whatever would have made this
        // residual smaller, and stop as soon as they have accounted for it.
        if sg > 0 {
            for k in (0..order).rev() {
                let dd = top.wrapping_sub(out[j - k]);
                let s = sign(dd);
                coefs[k] -= s;
                del0 -= ((order - k) as i32).wrapping_mul((s.wrapping_mul(dd)) >> den_shift);
                if del0 <= 0 {
                    break;
                }
            }
        } else if sg < 0 {
            for k in (0..order).rev() {
                let dd = top.wrapping_sub(out[j - k]);
                let s = sign(dd);
                coefs[k] += s;
                del0 -= ((order - k) as i32).wrapping_mul(((-s).wrapping_mul(dd)) >> den_shift);
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// One channel's coded parameters, read straight out of the frame.
struct ChannelPlan {
    mode: u32,
    den_shift: u32,
    pb_factor: u32,
    coefs: Vec<i32>,
}

fn read_plan(bits: &mut Bits<'_>) -> Result<ChannelPlan> {
    let mode = bits.read(4)?;
    let den_shift = bits.read(4)?;
    let pb_factor = bits.read(3)?;
    let order = bits.read(5)? as usize;
    let mut coefs = Vec::with_capacity(order);
    for _ in 0..order {
        coefs.push(bits.read(16)? as i16 as i32);
    }
    Ok(ChannelPlan {
        mode,
        den_shift,
        pb_factor,
        coefs,
    })
}

/// Scratch buffers, kept between frames so a decode costs no allocation.
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    res: Vec<i32>,
    mix: [Vec<i32>; 2],
    tmp: Vec<i32>,
    shift_uv: Vec<u32>,
}

/// Decode one ALAC frame into `out`, interleaved in the stream's own channel
/// order, and answer how many samples per channel it held.
pub(crate) fn frame(
    cfg: &MagicCookie,
    scratch: &mut Scratch,
    data: &[u8],
    out: &mut Vec<i32>,
) -> Result<usize> {
    let channels = usize::from(cfg.channels.max(1));
    let mut bits = Bits::new(data);
    let mut written = 0usize;
    let mut samples = 0usize;
    out.clear();

    loop {
        let tag = bits.read(3)?;
        match tag {
            ID_SCE | ID_LFE | ID_CPE => {
                let pair = tag == ID_CPE;
                let n = element(cfg, scratch, &mut bits, pair)?;
                if samples == 0 {
                    samples = n;
                    out.resize(n * channels, 0);
                } else if n != samples {
                    return Err(Error::corrupt(
                        "ALAC: two elements of one frame disagree on length",
                    ));
                }
                let width = 1 + usize::from(pair);
                if written + width > channels {
                    return Err(Error::corrupt("ALAC: more channels than the cookie states"));
                }
                for (i, sample) in out.chunks_exact_mut(channels).enumerate().take(samples) {
                    sample[written] = scratch.mix[0][i];
                    if pair {
                        sample[written + 1] = scratch.mix[1][i];
                    }
                }
                written += width;
                if written >= channels {
                    return Ok(samples);
                }
            }
            // Ancillary elements: skipped by their own stated length.
            ID_DSE => {
                bits.skip(4)?;
                let aligned = bits.read(1)? != 0;
                let mut count = bits.read(8)?;
                if count == 255 {
                    count += bits.read(8)?;
                }
                if aligned {
                    bits.align();
                }
                bits.skip(count as usize * 8)?;
            }
            ID_FIL => {
                let mut count = bits.read(4)?;
                if count == 15 {
                    count += bits.read(8)? - 1;
                }
                bits.skip(count as usize * 8)?;
            }
            ID_END => {
                bits.align();
                return Ok(samples);
            }
            ID_CCE | ID_PCE => {
                return Err(Error::unsupported(
                    "ALAC coupling and program-config elements",
                    "no ALAC encoder writes them and no file has been seen carrying one",
                ));
            }
            _ => return Err(Error::corrupt(format!("ALAC: element tag {tag}"))),
        }
    }
}

/// One SCE/LFE (mono) or CPE (stereo) element into `scratch.mix`.
fn element(
    cfg: &MagicCookie,
    scratch: &mut Scratch,
    bits: &mut Bits<'_>,
    pair: bool,
) -> Result<usize> {
    let channels = 1 + usize::from(pair);
    // A 4-bit element instance tag and 12 reserved bits, zero in every stream
    // measured here (mono and stereo fixtures from the oracle's encoder, whose
    // predictor plan starts at bit 39 of the frame — 3 tag + these 16 + the 4
    // flags + the 16 mix bits below).
    bits.skip(16)?;
    let partial = bits.read(1)? != 0;
    let bytes_shifted = bits.read(2)?;
    let escape = bits.read(1)? != 0;
    if bytes_shifted == 3 {
        return Err(Error::corrupt("ALAC: three shifted bytes"));
    }
    let shift = bytes_shifted * 8;
    let samples = match partial {
        true => (bits.read(16)? << 16 | bits.read(16)?) as usize,
        false => cfg.frame_length as usize,
    };
    if samples == 0 || samples > MAX_FRAME_SAMPLES {
        return Err(Error::corrupt(format!("ALAC: a {samples}-sample frame")));
    }
    let bit_depth = u32::from(cfg.bit_depth);
    if !(1..=32).contains(&bit_depth) {
        return Err(Error::corrupt(format!("ALAC: {bit_depth}-bit samples")));
    }
    for buf in &mut scratch.mix {
        buf.clear();
        buf.resize(samples, 0);
    }
    scratch.shift_uv.clear();

    if escape {
        // Uncompressed: the samples are in the frame verbatim, channels
        // interleaved, and nothing was mid/side mixed or shifted.
        let chan_bits = bit_depth;
        for i in 0..samples {
            for c in 0..channels {
                let raw = bits.read(chan_bits)?;
                scratch.mix[c][i] = ((raw << (32 - chan_bits)) as i32) >> (32 - chan_bits);
            }
        }
        interleave(scratch, samples, channels, 0, 0, 0);
        return Ok(samples);
    }

    // The mid/side pair, stated by a mono element too (as zeroes, where it
    // means nothing) — measured, not assumed: a mono frame's predictor plan
    // starts 16 bits after its flags, exactly as a stereo frame's does.
    let mix_bits = bits.read(8)?;
    let mix_res = bits.read(8)? as i8 as i32;
    let mut plans = Vec::with_capacity(channels);
    for _ in 0..channels {
        plans.push(read_plan(bits)?);
    }

    // The low bytes of a 24-bit stream ride uncompressed *ahead* of the
    // residuals in the bitstream but are read back after them.
    let shift_at = bits.pos;
    if shift > 0 {
        bits.skip(shift as usize * samples * channels)?;
    }

    let chan_bits = bit_depth - shift + (channels as u32 - 1);
    if chan_bits == 0 || chan_bits > 32 {
        return Err(Error::corrupt(format!("ALAC: {chan_bits} coded bits")));
    }
    scratch.res.clear();
    scratch.res.resize(samples, 0);
    scratch.tmp.clear();
    scratch.tmp.resize(samples, 0);
    for (c, plan) in plans.iter_mut().enumerate() {
        let mut rice = Rice::new(cfg, plan.pb_factor);
        rice.decode(bits, &mut scratch.res, chan_bits)?;
        match plan.mode {
            0 => predict(
                &scratch.res,
                &mut scratch.mix[c],
                &mut plan.coefs,
                chan_bits,
                plan.den_shift,
            )?,
            // Anything else runs the difference pass first and the filter on
            // top of it.
            _ => {
                predict(
                    &scratch.res,
                    &mut scratch.tmp,
                    &mut vec![0; ORDER_ADAPT as usize],
                    chan_bits,
                    0,
                )?;
                let tmp = std::mem::take(&mut scratch.tmp);
                predict(
                    &tmp,
                    &mut scratch.mix[c],
                    &mut plan.coefs,
                    chan_bits,
                    plan.den_shift,
                )?;
                scratch.tmp = tmp;
            }
        }
    }

    if shift > 0 {
        let mut low = Bits::new(bits.data);
        low.pos = shift_at;
        for _ in 0..samples * channels {
            scratch.shift_uv.push(low.read(shift)?);
        }
    }
    interleave(scratch, samples, channels, shift, mix_bits, mix_res);
    Ok(samples)
}

/// Un-mix a decoded element in place: the mid/side pair back into left and
/// right, and the low bytes of a wide stream back onto every sample.
fn interleave(
    scratch: &mut Scratch,
    samples: usize,
    channels: usize,
    shift: u32,
    mix_bits: u32,
    mix_res: i32,
) {
    let shifted = shift > 0 && scratch.shift_uv.len() >= samples * channels;
    if channels == 1 {
        for i in 0..samples {
            let mut v = scratch.mix[0][i];
            if shifted {
                v = (v << shift) | scratch.shift_uv[i] as i32;
            }
            scratch.mix[0][i] = v;
        }
        return;
    }
    for i in 0..samples {
        let u = scratch.mix[0][i];
        let v = scratch.mix[1][i];
        let (mut l, mut r) = match mix_res {
            0 => (u, v),
            _ => {
                let l = u.wrapping_add(v).wrapping_sub((mix_res * v) >> mix_bits);
                (l, l.wrapping_sub(v))
            }
        };
        if shifted {
            l = (l << shift) | scratch.shift_uv[2 * i] as i32;
            r = (r << shift) | scratch.shift_uv[2 * i + 1] as i32;
        }
        scratch.mix[0][i] = l;
        scratch.mix[1][i] = r;
    }
}

/// The largest frame this accepts, so a corrupt partial-frame length cannot ask
/// for an allocation the file could not hold. the reference encoder writes 4096.
const MAX_FRAME_SAMPLES: usize = 1 << 20;
