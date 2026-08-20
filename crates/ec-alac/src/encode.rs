//! ALAC encoding: the inverse of `decode.rs`'s element walk, adaptive-Golomb
//! coder and predictor. Every constant and code shape here mirrors that file
//! deliberately, so a `AlacEncoder::encode_frame` output is exactly what
//! `AlacDecoder::decode` expects.
//!
//! Written from the published ALAC format description, same as `decode.rs`.
//! Nothing here is derived from Apple's encoder sources.

use crate::MagicCookie;
use ec_core::error::{Error, Result};
use ec_core::frame::{Frame, SampleFormat};
use ec_core::packet::Packet;
use ec_core::registry::{CodecParameters, Encoder};

const QBSHIFT: u32 = 9;
const QB: u32 = 1 << QBSHIFT;
const MMULSHIFT: u32 = 2;
const MDENSHIFT: u32 = QBSHIFT - MMULSHIFT - 1;
const MOFF: u32 = 1 << (MDENSHIFT - 2);
const BITOFF: i32 = 24;
const MAX_PREFIX: u32 = 9;
const MAX_RUN_BITS: u32 = 16;
const MEAN_CLAMP: u32 = 0xffff;

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_END: u32 = 7;

/// Filter orders searched for the single-pass (mode 0) predictor, plus 31 —
/// the format's sentinel for "whole-block first-order difference", which
/// `decode.rs`'s `predict` special-cases regardless of any coefficients
/// stored alongside it (`decode.rs:245`).
const ORDER_CANDIDATES: [usize; 5] = [4, 8, 12, 16, 31];
const ORDER_DIFF: usize = 31;

/// `floor(log2(x + 3))`, duplicated from `decode.rs` because that module's
/// helpers are private to it.
fn lg3a(x: u32) -> u32 {
    31 - (x + 3).leading_zeros()
}

fn sign(x: i32) -> i32 {
    (x > 0) as i32 - (x < 0) as i32
}

/// A zigzag of a signed residual into decode.rs's `ndecode` space: 0, -1, 1,
/// -2, 2, ... maps to 0, 1, 2, 3, 4, ...
fn zigzag(v: i32) -> u32 {
    let v = i64::from(v);
    (if v >= 0 { v * 2 } else { -v * 2 - 1 }) as u32
}

/// Where encoded bits go: a real bit writer, or a running count for the
/// order/mix search that never touches memory for the bits themselves.
trait BitSink {
    fn put(&mut self, value: u32, n: u32);
}

/// MSB-first bit writer, byte-aligned on [`BitWriter::finish`] — the write
/// side of `decode.rs`'s `Bits`.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    cur: u8,
    nbits: u32,
}

impl BitWriter {
    fn align(&mut self) {
        while self.nbits != 0 {
            self.put(0, 1);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.align();
        self.bytes
    }
}

impl BitSink for BitWriter {
    fn put(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            self.cur = (self.cur << 1) | ((value >> i) & 1) as u8;
            self.nbits += 1;
            if self.nbits == 8 {
                self.bytes.push(self.cur);
                self.cur = 0;
                self.nbits = 0;
            }
        }
    }
}

/// A bit count only, for measuring a candidate before committing to it.
#[derive(Default)]
struct CostSink(u64);

impl BitSink for CostSink {
    fn put(&mut self, _value: u32, n: u32) {
        self.0 += u64::from(n);
    }
}

/// One adaptive-Golomb code, the write side of `decode.rs`'s `golomb`: a
/// unary quotient of `n / m` (escaping to a raw `maxbits` value past
/// [`MAX_PREFIX`]) and a truncated-binary remainder in `k` or `k - 1` bits.
fn golomb_write<S: BitSink>(sink: &mut S, n: u32, m: u32, k: u32, maxbits: u32) {
    if m == 0 {
        // `decode.rs`'s reader only reaches this when k <= 1 too, in which
        // case it never looks at a remainder at all — a mean-derived k this
        // small essentially never occurs (see `rice_encode`'s zero-run k),
        // but the fallback here is a plain unary-free raw write rather than
        // a divide by zero.
        sink.put(n, maxbits.max(1));
        return;
    }
    let q = n / m;
    if q >= MAX_PREFIX {
        sink.put(u32::MAX, MAX_PREFIX);
        if maxbits > 0 {
            sink.put(n, maxbits);
        }
        return;
    }
    if q > 0 {
        sink.put(u32::MAX, q);
    }
    sink.put(0, 1);
    if k > 1 {
        let r = n - q * m;
        match r {
            0 => sink.put(0, k - 1),
            r => sink.put(r + 1, k),
        }
    }
}

/// Adaptive-Golomb-encode `res` into `sink`, exactly inverting
/// `decode.rs`'s `Rice::decode` — same mean update, same zero-run escape.
fn rice_encode<S: BitSink>(sink: &mut S, cfg: &MagicCookie, pb_factor: u32, res: &[i32], chan_bits: u32) {
    let kb = u32::from(cfg.kb).clamp(1, 31);
    let pb = u32::from(cfg.pb) * pb_factor / 4;
    let wb = (1u32 << kb) - 1;
    let mut mean = u32::from(cfg.mb);
    let mut zmode = 0u32;
    let mut c = 0usize;
    while c < res.len() {
        let ndecode = zigzag(res[c]);
        // Safe: `zmode` is only 1 right after a zero run that consumed every
        // contiguous zero, so the sample here is never itself zero.
        let n = ndecode - zmode;
        let k = lg3a(mean >> QBSHIFT).min(kb);
        let m = (1u32 << k) - 1;
        golomb_write(sink, n, m, k, chan_bits);
        c += 1;

        mean = pb
            .wrapping_mul(ndecode)
            .wrapping_add(mean)
            .wrapping_sub(pb.wrapping_mul(mean) >> QBSHIFT);
        if n > MEAN_CLAMP {
            mean = MEAN_CLAMP;
        }

        zmode = 0;
        if (mean << MMULSHIFT) < QB && c < res.len() {
            zmode = 1;
            let k2 = (mean.leading_zeros() as i32 - BITOFF + ((mean + MOFF) >> MDENSHIFT) as i32)
                .clamp(0, 31) as u32;
            let mz = ((1u32 << k2) - 1) & wb;
            let mut run = 0u32;
            while (c + run as usize) < res.len() && res[c + run as usize] == 0 {
                run += 1;
                if run as usize >= (1 << MAX_RUN_BITS) - 1 {
                    break;
                }
            }
            golomb_write(sink, run, mz, k2, MAX_RUN_BITS);
            c += run as usize;
            if run >= (1 << MAX_RUN_BITS) - 1 {
                zmode = 0;
            }
            mean = 0;
        }
    }
}

/// The forward twin of `decode.rs`'s `predict`: given the true samples `y`
/// (already in their coded width), compute the residuals a from-zero
/// sign-adapting filter of `order` taps and `den_shift` would need to
/// reproduce them exactly. The coefficients this implicitly walks through
/// are never returned — the encoder always states an all-zero starting
/// filter, so the decoder's own adaptation, run from the same zero start,
/// retraces this pass exactly.
fn analyze(y: &[i32], order: usize, den_shift: u32) -> Vec<i32> {
    let n = y.len();
    let mut res = vec![0i32; n];
    if n == 0 {
        return res;
    }
    res[0] = y[0];
    if order == ORDER_DIFF {
        for j in 1..n {
            res[j] = y[j].wrapping_sub(y[j - 1]);
        }
        return res;
    }
    if order == 0 || order >= n {
        // Order 0 is decode.rs's "residuals are the samples verbatim"; a
        // caller never asks for 0 < order >= n (`best_candidate` filters
        // those out), so this path is only ever reached as the order-0 case.
        res.copy_from_slice(y);
        return res;
    }

    let mut coefs = vec![0i32; order];
    for j in 1..=order {
        res[j] = y[j].wrapping_sub(y[j - 1]);
    }
    let den_half = match den_shift {
        0 => 0,
        s => 1i32 << (s - 1),
    };
    for j in order..n - 1 {
        let top = y[j - order];
        let mut sum = 0i32;
        for (k, &c) in coefs.iter().enumerate() {
            sum = sum.wrapping_add(c.wrapping_mul(y[j - k].wrapping_sub(top)));
        }
        let predicted = top.wrapping_add(sum.wrapping_add(den_half) >> den_shift);
        let del = y[j + 1].wrapping_sub(predicted);
        res[j + 1] = del;

        let mut del0 = del;
        let sg = sign(del);
        if sg > 0 {
            for k in (0..order).rev() {
                let dd = top.wrapping_sub(y[j - k]);
                let s = sign(dd);
                coefs[k] -= s;
                del0 -= ((order - k) as i32).wrapping_mul(s.wrapping_mul(dd) >> den_shift);
                if del0 <= 0 {
                    break;
                }
            }
        } else if sg < 0 {
            for k in (0..order).rev() {
                let dd = top.wrapping_sub(y[j - k]);
                let s = sign(dd);
                coefs[k] += s;
                del0 -= ((order - k) as i32).wrapping_mul((-s).wrapping_mul(dd) >> den_shift);
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
    res
}

/// One channel's chosen filter: the mode and order fields to write, the
/// residuals it produces, and its total cost in bits (16-bit plan header +
/// coefficients + the Rice-coded residuals), for comparing against every
/// other choice.
struct Candidate {
    mode: u32,
    order: u32,
    res: Vec<i32>,
    bits: u64,
}

/// Cheapest of [`ORDER_CANDIDATES`] (plus the trivial order-0 "residuals are
/// the samples" case) for one channel of true samples, each as mode 0 — the
/// only prediction type real-world decoders accept (decode.rs also undoes
/// mode 1's extra difference pass, but nothing here emits it).
fn best_candidate(cfg: &MagicCookie, y: &[i32], chan_bits: u32, pb_factor: u32) -> Candidate {
    let mut orders: Vec<usize> = vec![0];
    orders.extend(ORDER_CANDIDATES.iter().copied().filter(|&o| o < y.len()));
    let mut best: Option<Candidate> = None;
    for order in orders {
        let res = analyze(y, order, 9);
        let mut cost = CostSink::default();
        rice_encode(&mut cost, cfg, pb_factor, &res, chan_bits);
        let bits = 16 + (order as u64) * 16 + cost.0;

        if best.as_ref().is_none_or(|b| bits < b.bits) {
            best = Some(Candidate {
                mode: 0,
                order: order as u32,
                res,
                bits,
            });
        }
    }
    best.unwrap_or(Candidate {
        mode: 0,
        order: 0,
        res: y.to_vec(),
        bits: u64::MAX,
    })
}

/// A channel's coded parameters and residuals, ready to write: the plan
/// header fields plus [`Candidate::res`].
struct ChannelPlan {
    mode: u32,
    order: u32,
    pb_factor: u32,
    res: Vec<i32>,
}

const DEN_SHIFT: u32 = 9;
const PB_FACTOR: u32 = 4;

fn write_plan<S: BitSink>(sink: &mut S, plan: &ChannelPlan) {
    sink.put(plan.mode, 4);
    sink.put(DEN_SHIFT, 4);
    sink.put(PB_FACTOR, 3);
    sink.put(plan.order, 5);
    for _ in 0..plan.order {
        sink.put(0, 16); // all-zero starting coefficients (see `analyze`).
    }
}

/// One SCE (mono) or CPE (stereo) element, choosing per-element between raw
/// (escape) and compressed, and for a pair, between direct L/R and a
/// mid/side mix, whichever is smaller.
fn write_element(cfg: &MagicCookie, w: &mut BitWriter, planes: &[Vec<i32>], samples: usize, allow_shift: bool) {
    w.put(0, 16); // element instance tag + 12 reserved bits.
    let pair = planes.len() == 2;
    let channels = planes.len() as u32;
    let bit_depth = u32::from(cfg.bit_depth);
    let partial = samples != cfg.frame_length as usize;
    let raw_bits = u64::from(channels) * u64::from(bit_depth) * samples as u64;

    // A 24-bit stream may additionally split each sample into a compressed
    // high part and an uncompressed low byte ridden ahead of the residuals
    // (decode.rs:485-490, :533-538) — bytes_shifted 1 — when that beats
    // compressing the sample whole. Only 0 and 1 are ever tried: 3 is the
    // format's escape sentinel (decode.rs:439), and a stream never carries
    // more than one low byte's worth of headroom worth shifting off.
    let shift_candidates: &[u32] = if bit_depth == 24 && allow_shift { &[0, 1] } else { &[0] };

    let compressed = (samples >= 2).then(|| {
        shift_candidates
            .iter()
            .map(|&bytes_shifted| {
                let shift_bits = bytes_shifted * 8;
                let chan_bits = bit_depth - shift_bits + channels - 1;
                let low_bits = u64::from(shift_bits) * u64::from(channels) * samples as u64;
                let shifted: Vec<Vec<i32>> = planes
                    .iter()
                    .map(|p| p.iter().map(|&s| s >> shift_bits).collect())
                    .collect();

                let (mix_bits, mix_res, plans, bits) = if pair {
                    let (l, r) = (&shifted[0], &shifted[1]);
                    let lc = best_candidate(cfg, l, chan_bits, PB_FACTOR);
                    let rc = best_candidate(cfg, r, chan_bits, PB_FACTOR);
                    let lr_bits = lc.bits + rc.bits;

                    // mix_bits = 1, mix_res = 1: v = l - r, u = r + (v >> 1).
                    let (mut u, mut v) = (vec![0i32; samples], vec![0i32; samples]);
                    for i in 0..samples {
                        v[i] = l[i] - r[i];
                        u[i] = r[i] + (v[i] >> 1);
                    }
                    let uc = best_candidate(cfg, &u, chan_bits, PB_FACTOR);
                    let vc = best_candidate(cfg, &v, chan_bits, PB_FACTOR);
                    let ms_bits = uc.bits + vc.bits;

                    match ms_bits < lr_bits {
                        true => (1u32, 1i32, vec![plan_of(uc), plan_of(vc)], ms_bits),
                        false => (2u32, 0i32, vec![plan_of(lc), plan_of(rc)], lr_bits),
                    }
                } else {
                    let c0 = best_candidate(cfg, &shifted[0], chan_bits, PB_FACTOR);
                    let bits = c0.bits;
                    (0u32, 0i32, vec![plan_of(c0)], bits)
                };
                (bytes_shifted, mix_bits, mix_res, plans, bits + low_bits)
            })
            .min_by_key(|&(_, _, _, _, bits)| bits)
            .unwrap()
    });

    let use_compressed = compressed
        .as_ref()
        .is_some_and(|(_, _, _, _, bits)| 16 + bits < raw_bits.max(1));
    let bytes_shifted = compressed.as_ref().map_or(0, |&(bs, ..)| bs);

    w.put(partial as u32, 1);
    w.put(if use_compressed { bytes_shifted } else { 0 }, 2);
    w.put(!use_compressed as u32, 1);
    if partial {
        w.put((samples as u32) >> 16, 16);
        w.put(samples as u32 & 0xffff, 16);
    }

    match use_compressed {
        false => {
            let chan_bits = bit_depth;
            for i in 0..samples {
                for plane in planes {
                    w.put(plane[i] as u32 & mask(chan_bits), chan_bits);
                }
            }
        }
        true => {
            let (bytes_shifted, mix_bits, mix_res, plans, _) = compressed.unwrap();
            let shift_bits = bytes_shifted * 8;
            w.put(mix_bits, 8);
            w.put((mix_res as i8) as u8 as u32, 8);
            for plan in &plans {
                write_plan(w, plan);
            }
            // The low bytes ride ahead of the residuals, one `shift_bits`
            // field per sample per channel, in the same i-outer/channel-inner
            // order the escape path above uses — decode.rs reads them back
            // in exactly that order (decode.rs:533-538).
            if shift_bits > 0 {
                for i in 0..samples {
                    for plane in planes {
                        w.put(plane[i] as u32 & mask(shift_bits), shift_bits);
                    }
                }
            }
            let chan_bits = bit_depth - shift_bits + channels - 1;
            for plan in &plans {
                rice_encode(w, cfg, plan.pb_factor, &plan.res, chan_bits);
            }
        }
    }
}

fn plan_of(c: Candidate) -> ChannelPlan {
    ChannelPlan {
        mode: c.mode,
        order: c.order,
        pb_factor: PB_FACTOR,
        res: c.res,
    }
}

/// `n` low bits set, for masking a two's-complement value into an escape's
/// raw field.
fn mask(n: u32) -> u32 {
    match n {
        32 => u32::MAX,
        n => (1 << n) - 1,
    }
}

/// An ALAC encoder for one mono or stereo stream.
#[derive(Debug)]
pub struct AlacEncoder {
    cookie: MagicCookie,
    params: CodecParameters,
    channels: usize,
    pending: Vec<i32>,
    packets: std::collections::VecDeque<Vec<u8>>,
    eof: bool,
    allow_shift: bool,
}

impl AlacEncoder {
    /// An encoder for a mono or stereo stream. `bit_depth` is 16 or 24;
    /// `frame_length` is the samples-per-channel every full frame carries
    /// (Apple's own encoder writes 4096).
    pub fn new(sample_rate: u32, channels: u8, bit_depth: u8, frame_length: u32) -> Result<AlacEncoder> {
        if !(1..=2).contains(&channels) {
            return Err(Error::unsupported(
                format!("ALAC encoding {channels} channels"),
                "this encoder writes mono and stereo only",
            ));
        }
        let cookie = MagicCookie {
            frame_length,
            compatible_version: 0,
            bit_depth,
            pb: 40,
            mb: 10,
            kb: 14,
            channels,
            max_run: 255,
            max_frame_bytes: 0,
            avg_bit_rate: 0,
            sample_rate,
        };
        let mut params = crate::codec_parameters(&cookie);
        params.extradata = Some(cookie.extradata_box().into());
        Ok(AlacEncoder {
            cookie,
            params,
            channels: channels as usize,
            pending: Vec::new(),
            packets: std::collections::VecDeque::new(),
            eof: false,
            allow_shift: true,
        })
    }

    /// Test-only: force bytes_shifted 0 even at 24 bits, so a test can
    /// measure both paths against the same source. Both outputs decode the
    /// same; this only trades size.
    #[doc(hidden)]
    pub fn set_byte_shift(&mut self, allow: bool) {
        self.allow_shift = allow;
    }

    /// What this encoder states about the stream it writes.
    pub fn cookie(&self) -> &MagicCookie {
        &self.cookie
    }

    /// Encode one frame: `samples` interleaved, true PCM range (not the
    /// container-shifted range [`AlacDecoder::decode`] hands back), up to
    /// `frame_length` samples per channel — a shorter slice writes a partial
    /// frame, exactly as a stream's last frame does.
    pub fn encode_frame(&self, samples: &[i32]) -> Vec<u8> {
        let channels = self.channels;
        let n = samples.len() / channels;
        let mut planes: Vec<Vec<i32>> = vec![Vec::with_capacity(n); channels];
        for chunk in samples.chunks_exact(channels) {
            for (c, plane) in planes.iter_mut().enumerate() {
                plane.push(chunk[c]);
            }
        }
        let mut w = BitWriter::default();
        w.put(if channels == 2 { ID_CPE } else { ID_SCE }, 3);
        write_element(&self.cookie, &mut w, &planes, n, self.allow_shift);
        w.put(ID_END, 3);
        w.finish()
    }
}

impl Encoder for AlacEncoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let Frame::Audio(audio) = frame else {
            return Err(Error::corrupt("video frame pushed into an ALAC encoder"));
        };
        let channels = self.channels;
        let shift = self.cookie.container_shift();
        let want = self.cookie.sample_format();
        if audio.format != want {
            return Err(Error::unsupported(
                format!("{:?} input", audio.format),
                "this encoder takes samples in the cookie's own sample format",
            ));
        }
        let mut interleaved = Vec::with_capacity(audio.samples * channels);
        match (audio.planar, want) {
            (false, SampleFormat::S16) => {
                for pair in audio.data[0].chunks_exact(2).take(audio.samples * channels) {
                    interleaved.push(i32::from(i16::from_ne_bytes([pair[0], pair[1]])) >> shift);
                }
            }
            (false, _) => {
                for word in audio.data[0].chunks_exact(4).take(audio.samples * channels) {
                    interleaved.push(i32::from_ne_bytes([word[0], word[1], word[2], word[3]]) >> shift);
                }
            }
            (true, SampleFormat::S16) => {
                for i in 0..audio.samples {
                    for plane in audio.data.iter().take(channels) {
                        let at = i * 2;
                        interleaved.push(i32::from(i16::from_ne_bytes([plane[at], plane[at + 1]])) >> shift);
                    }
                }
            }
            (true, _) => {
                for i in 0..audio.samples {
                    for plane in audio.data.iter().take(channels) {
                        let at = i * 4;
                        let w = [plane[at], plane[at + 1], plane[at + 2], plane[at + 3]];
                        interleaved.push(i32::from_ne_bytes(w) >> shift);
                    }
                }
            }
        }
        self.pending.extend(interleaved);
        let per_frame = self.cookie.frame_length as usize * channels;
        while self.pending.len() >= per_frame {
            let frame: Vec<i32> = self.pending.drain(..per_frame).collect();
            self.packets.push_back(self.encode_frame(&frame));
        }
        Ok(())
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        match self.packets.pop_front() {
            Some(data) => Ok(Packet::new(0, ec_core::timebase::TimeBase::new(1, i64::from(self.cookie.sample_rate)), data)),
            None if self.eof => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        if !self.pending.is_empty() {
            let frame = std::mem::take(&mut self.pending);
            self.packets.push_back(self.encode_frame(&frame));
        }
        self.eof = true;
        Ok(())
    }
}
