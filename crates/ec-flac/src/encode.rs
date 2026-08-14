//! FLAC encoding: fixed-block-size streams with constant, fixed, LPC and
//! verbatim subframes, a Rice partition search per residual and a per-block
//! stereo decorrelation decision.
//!
//! The output is a complete `.flac` file: `fLaC`, one `STREAMINFO` (carrying
//! the MD5 of the input, so any decoder can check us), then the frames.

use ec_core::bitio::BitWriter;
use ec_core::error::{Error, Result};

use crate::checksum::{crc8, crc16, md5_of_samples};
use crate::decode::StreamInfo;

/// Encoder settings. [`EncoderConfig::default`] is the level the family ships:
/// 4096-sample blocks, LPC to order 12, Rice partitions to order 6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderConfig {
    /// Inter-channel samples per frame.
    pub block_size: usize,
    /// Highest LPC order tried; 0 disables LPC and leaves the fixed predictors.
    pub max_lpc_order: usize,
    /// Bits each quantised LPC coefficient is stored in (5..=15).
    pub qlp_precision: u32,
    /// Highest Rice partition order tried.
    pub max_partition_order: u32,
    /// Try left/side, right/side and mid/side on stereo input.
    pub stereo_decorrelation: bool,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        EncoderConfig {
            block_size: 4096,
            max_lpc_order: 12,
            qlp_precision: 15,
            max_partition_order: 6,
            stereo_decorrelation: true,
        }
    }
}

impl EncoderConfig {
    /// Refuse settings that cannot produce a legal stream.
    pub fn verify(&self) -> Result<()> {
        if !(16..=65535).contains(&self.block_size) {
            return Err(Error::unsupported(
                format!("block size {}", self.block_size),
                "RFC 9639 allows 16..=65535",
            ));
        }
        if self.max_lpc_order > 32 {
            return Err(Error::unsupported(
                format!("LPC order {}", self.max_lpc_order),
                "RFC 9639 allows up to 32",
            ));
        }
        if !(5..=15).contains(&self.qlp_precision) {
            return Err(Error::unsupported(
                format!("coefficient precision {}", self.qlp_precision),
                "RFC 9639 allows 1..=15; below 5 is never useful",
            ));
        }
        if self.max_partition_order > 15 {
            return Err(Error::unsupported(
                format!("partition order {}", self.max_partition_order),
                "RFC 9639 allows up to 15",
            ));
        }
        Ok(())
    }
}

/// Encode interleaved samples into a complete FLAC stream.
///
/// `samples` are interleaved by channel and must already fit in
/// `bits_per_sample`: a sample outside that range is [`Error::Corrupt`],
/// because silently clipping the caller's audio would make a lossless codec
/// lossy.
pub fn encode(
    config: &EncoderConfig,
    samples: &[i32],
    channels: usize,
    bits_per_sample: u32,
    sample_rate: u32,
) -> Result<Vec<u8>> {
    config.verify()?;
    if !(1..=8).contains(&channels) {
        return Err(Error::unsupported(
            format!("{channels} channels"),
            "FLAC codes 1..=8 channels",
        ));
    }
    if !(4..=32).contains(&bits_per_sample) {
        return Err(Error::unsupported(
            format!("{bits_per_sample} bits per sample"),
            "FLAC codes 4..=32 bits",
        ));
    }
    if sample_rate == 0 || sample_rate > 655_350 {
        return Err(Error::unsupported(
            format!("{sample_rate} Hz"),
            "FLAC codes 1..=655350 Hz",
        ));
    }
    if !samples.len().is_multiple_of(channels) {
        return Err(Error::corrupt(format!(
            "{} samples do not divide into {channels} channels",
            samples.len()
        )));
    }
    let (low, high) = match bits_per_sample {
        32 => (i32::MIN, i32::MAX),
        n => (-(1i32 << (n - 1)), (1i32 << (n - 1)) - 1),
    };
    if let Some(bad) = samples.iter().find(|&&s| s < low || s > high) {
        return Err(Error::corrupt(format!(
            "sample {bad} does not fit in {bits_per_sample} bits"
        )));
    }

    let frames = samples.len() / channels;
    let mut info = StreamInfo {
        min_block_size: config.block_size as u16,
        max_block_size: config.block_size as u16,
        min_frame_size: 0,
        max_frame_size: 0,
        sample_rate,
        channels: channels as u8,
        bits_per_sample: bits_per_sample as u8,
        total_samples: frames as u64,
        md5: md5_of_samples(samples, bits_per_sample),
    };

    let mut out = Vec::with_capacity(samples.len() * bits_per_sample as usize / 8);
    out.extend_from_slice(&crate::decode::MAGIC);
    out.push(0x80); // last metadata block, type 0 (STREAMINFO)
    out.extend_from_slice(&[0, 0, 34]);
    let streaminfo_at = out.len();
    out.extend_from_slice(&info.to_bytes());

    let mut planes: Vec<Vec<i32>> = (0..channels)
        .map(|_| Vec::with_capacity(config.block_size))
        .collect();
    let mut scratch = Scratch::default();
    let mut min_frame = u32::MAX;
    let mut max_frame = 0u32;
    for (number, block) in samples.chunks(config.block_size * channels).enumerate() {
        let block_size = block.len() / channels;
        for (c, plane) in planes.iter_mut().enumerate() {
            plane.clear();
            plane.extend(block.iter().skip(c).step_by(channels).copied());
        }
        let frame = encode_frame(
            config,
            &planes,
            block_size,
            bits_per_sample,
            sample_rate,
            number as u64,
            &mut scratch,
        );
        min_frame = min_frame.min(frame.len() as u32);
        max_frame = max_frame.max(frame.len() as u32);
        out.extend_from_slice(&frame);
    }
    if frames > 0 {
        info.min_frame_size = min_frame;
        info.max_frame_size = max_frame;
        out[streaminfo_at..streaminfo_at + 34].copy_from_slice(&info.to_bytes());
    }
    Ok(out)
}

/// Buffers reused across blocks: the two stereo candidates and the analysis
/// window, which depends only on the block size and so is built once.
#[derive(Debug, Default)]
struct Scratch {
    mid: Vec<i32>,
    side: Vec<i32>,
    window: Vec<f64>,
}

impl Scratch {
    fn ensure_window(&mut self, n: usize) {
        if self.window.len() != n {
            self.window = tukey_window(n);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn encode_frame(
    config: &EncoderConfig,
    planes: &[Vec<i32>],
    block_size: usize,
    bits_per_sample: u32,
    sample_rate: u32,
    number: u64,
    scratch: &mut Scratch,
) -> Vec<u8> {
    let channels = planes.len();
    // Channel assignment: 0..7 = that many independent channels, 8 = left/side,
    // 9 = right/side, 10 = mid/side. Each candidate signal is planned once and
    // the cheapest pairing wins — planning carries all the search cost, writing
    // carries none.
    let stereo = channels == 2 && config.stereo_decorrelation && bits_per_sample < 32;
    if stereo {
        scratch.mid.clear();
        scratch.side.clear();
        for (&l, &r) in planes[0].iter().zip(&planes[1]) {
            // i64 first: `l - r` of two full-depth samples does not fit in i32.
            scratch
                .mid
                .push(((i64::from(l) + i64::from(r)) >> 1) as i32);
            scratch.side.push((i64::from(l) - i64::from(r)) as i32);
        }
    }
    scratch.ensure_window(block_size);
    let window = &scratch.window;

    let mut plans: Vec<SubframePlan> = planes
        .iter()
        .map(|plane| plan_subframe(config, plane, bits_per_sample, window))
        .collect();
    let mut assignment = channels as u32 - 1;
    if stereo {
        let mid = plan_subframe(config, &scratch.mid, bits_per_sample, window);
        // The side channel needs one bit more than the stream's depth, which is
        // why a 32-bit stream cannot be decorrelated at all.
        let side = plan_subframe(config, &scratch.side, bits_per_sample + 1, window);
        let (l, r) = (plans[0].bits, plans[1].bits);
        let (_, best) = [
            (l + r, 1u32),
            (l + side.bits, 8),
            (side.bits + r, 9),
            (mid.bits + side.bits, 10),
        ]
        .into_iter()
        .min()
        .expect("four candidates");
        assignment = best;
        match best {
            8 => plans[1] = side,
            9 => plans[0] = side,
            10 => {
                plans[0] = mid;
                plans[1] = side;
            }
            _ => {}
        }
    }

    let mut w = BitWriter::with_capacity(block_size * channels * 2);
    write_frame_header(
        &mut w,
        block_size,
        sample_rate,
        assignment,
        bits_per_sample,
        number,
    );
    debug_assert!(w.is_byte_aligned());
    let crc = crc8(w.as_bytes());
    w.write_bits(u32::from(crc), 8);
    for (c, plan) in plans.iter().enumerate() {
        let side = matches!((assignment, c), (8, 1) | (10, 1) | (9, 0));
        write_subframe(&mut w, plan, bits_per_sample + u32::from(side));
    }
    w.align_to_byte();
    let crc = crc16(w.as_bytes());
    w.write_bits(u32::from(crc), 16);
    w.into_bytes()
}

fn write_frame_header(
    w: &mut BitWriter,
    block_size: usize,
    sample_rate: u32,
    assignment: u32,
    bits_per_sample: u32,
    number: u64,
) {
    let (bs_code, bs_extra) = match block_size {
        192 => (1, None),
        576 => (2, None),
        1152 => (3, None),
        2304 => (4, None),
        4608 => (5, None),
        256 => (8, None),
        512 => (9, None),
        1024 => (10, None),
        2048 => (11, None),
        4096 => (12, None),
        8192 => (13, None),
        16384 => (14, None),
        32768 => (15, None),
        n if n <= 256 => (6, Some((n as u32 - 1, 8))),
        n => (7, Some((n as u32 - 1, 16))),
    };
    let (sr_code, sr_extra) = match sample_rate {
        88200 => (1, None),
        176_400 => (2, None),
        192_000 => (3, None),
        8000 => (4, None),
        16000 => (5, None),
        22050 => (6, None),
        24000 => (7, None),
        32000 => (8, None),
        44100 => (9, None),
        48000 => (10, None),
        96000 => (11, None),
        n if n.is_multiple_of(1000) && n / 1000 <= 255 => (12, Some((n / 1000, 8))),
        n if n <= 65535 => (13, Some((n, 16))),
        n if n.is_multiple_of(10) && n / 10 <= 65535 => (14, Some((n / 10, 16))),
        // Every rate `encode` accepts is covered above; code 0 (take the rate
        // from STREAMINFO) is the honest fallback rather than a panic.
        _ => (0, None),
    };
    let bps_code = match bits_per_sample {
        8 => 1,
        12 => 2,
        16 => 4,
        20 => 5,
        24 => 6,
        32 => 7,
        _ => 0,
    };
    w.write_bits(0x3ffe, 14);
    w.write_bit(false); // reserved
    w.write_bit(false); // fixed block size: the coded number is a frame number
    w.write_bits(bs_code, 4);
    w.write_bits(sr_code, 4);
    w.write_bits(assignment, 4);
    w.write_bits(bps_code, 3);
    w.write_bit(false); // reserved
    write_utf8_number(w, number);
    if let Some((value, bits)) = bs_extra {
        w.write_bits(value, bits);
    }
    if let Some((value, bits)) = sr_extra {
        w.write_bits(value, bits);
    }
}

/// The UTF-8-shaped coded number of a frame header (up to 36 bits, so wider
/// than real UTF-8 ever goes).
fn write_utf8_number(w: &mut BitWriter, value: u64) {
    if value < 0x80 {
        w.write_bits(value as u32, 8);
        return;
    }
    // `extra` continuation bytes carry 6 bits each; the lead byte carries
    // `6 - extra` after its prefix of `extra + 1` ones and a zero.
    let mut extra = 1usize;
    while extra < 6 && value >= 1u64 << (6 * extra + (6 - extra)) {
        extra += 1;
    }
    let lead_bits = 6 - extra as u32;
    let prefix = (0xffu32 << (7 - extra)) & 0xff;
    let lead = prefix | ((value >> (6 * extra)) as u32 & ((1 << lead_bits) - 1));
    w.write_bits(lead, 8);
    for i in (0..extra).rev() {
        w.write_bits(0x80 | ((value >> (6 * i)) as u32 & 0x3f), 8);
    }
}

/// What a subframe will be written as, with its exact cost in bits.
#[derive(Debug, Clone)]
struct SubframePlan {
    bits: usize,
    wasted: u32,
    kind: PlanKind,
}

#[derive(Debug, Clone)]
enum PlanKind {
    Constant(i32),
    Verbatim(Vec<i32>),
    Fixed {
        warmup: Vec<i32>,
        residual: Vec<i32>,
        rice: RicePlan,
    },
    Lpc {
        warmup: Vec<i32>,
        precision: u32,
        shift: u32,
        coefs: Vec<i32>,
        residual: Vec<i32>,
        rice: RicePlan,
    },
}

#[derive(Debug, Clone, Default)]
struct RicePlan {
    /// 4 or 5: the width of a partition's Rice parameter field.
    param_bits: u32,
    partition_order: u32,
    /// Predictor order, which is what makes partition 0 shorter than the rest.
    order: usize,
    params: Vec<u32>,
}

fn plan_subframe(config: &EncoderConfig, signal: &[i32], bps: u32, window: &[f64]) -> SubframePlan {
    // Wasted bits: when every sample shares trailing zeros, state them once in
    // the subframe header instead of once per sample.
    let any_set = signal.iter().fold(0u32, |acc, &s| acc | s as u32);
    let wasted = match any_set {
        0 => 0,
        n => n.trailing_zeros().min(bps - 1),
    };
    let shifted: Vec<i32>;
    let signal = match wasted {
        0 => signal,
        n => {
            shifted = signal.iter().map(|&s| s >> n).collect();
            &shifted
        }
    };
    let bps = bps - wasted;
    let header_bits = 8 + wasted as usize;

    if signal.is_empty() {
        return SubframePlan {
            bits: header_bits + bps as usize,
            wasted,
            kind: PlanKind::Constant(0),
        };
    }
    if signal.iter().all(|&s| s == signal[0]) {
        return SubframePlan {
            bits: header_bits + bps as usize,
            wasted,
            kind: PlanKind::Constant(signal[0]),
        };
    }

    let mut best = SubframePlan {
        bits: header_bits + signal.len() * bps as usize,
        wasted,
        kind: PlanKind::Verbatim(signal.to_vec()),
    };
    if let Some(plan) = plan_fixed(config, signal, bps, header_bits)
        && plan.bits < best.bits
    {
        best = SubframePlan { wasted, ..plan };
    }
    if config.max_lpc_order > 0
        && signal.len() > config.max_lpc_order
        && window.len() == signal.len()
        && let Some(plan) = plan_lpc(config, signal, bps, header_bits, window)
        && plan.bits < best.bits
    {
        best = SubframePlan { wasted, ..plan };
    }
    best
}

/// The five fixed predictors, the order chosen by the sum of absolute
/// residuals — the standard estimate; the Rice search then costs it exactly.
fn plan_fixed(
    config: &EncoderConfig,
    signal: &[i32],
    bps: u32,
    header_bits: usize,
) -> Option<SubframePlan> {
    let max_order = 4.min(signal.len().saturating_sub(1));
    let mut diffs: Vec<Vec<i64>> = Vec::with_capacity(max_order + 1);
    diffs.push(signal.iter().map(|&s| i64::from(s)).collect());
    for order in 1..=max_order {
        diffs.push(diffs[order - 1].windows(2).map(|w| w[1] - w[0]).collect());
    }
    // Compare orders over the same samples, so the estimate is not biased by a
    // higher order simply covering fewer of them.
    let order = (0..=max_order).min_by_key(|&o| {
        let tail: u64 = diffs[o][max_order - o..]
            .iter()
            .map(|&d| d.unsigned_abs())
            .sum();
        tail + (o as u64) * u64::from(bps)
    })?;
    let residual: Vec<i32> = diffs[order]
        .iter()
        .map(|&d| i32::try_from(d))
        .collect::<std::result::Result<_, _>>()
        .ok()?; // 32-bit input can overflow the residual; verbatim wins then.
    let rice = plan_rice(config, &residual, signal.len(), order);
    Some(SubframePlan {
        bits: header_bits + order * bps as usize + rice.bits,
        wasted: 0,
        kind: PlanKind::Fixed {
            warmup: signal[..order].to_vec(),
            residual,
            rice: rice.plan,
        },
    })
}

fn plan_lpc(
    config: &EncoderConfig,
    signal: &[i32],
    bps: u32,
    header_bits: usize,
    window: &[f64],
) -> Option<SubframePlan> {
    let max_order = config.max_lpc_order.min(signal.len() - 1).min(32);
    if max_order == 0 {
        return None;
    }
    let windowed: Vec<f64> = window
        .iter()
        .zip(signal)
        .map(|(w, &s)| w * f64::from(s))
        .collect();
    let autoc = autocorrelation(&windowed, max_order);
    if autoc[0] == 0.0 {
        return None;
    }
    let (coefs_by_order, errors) = levinson_durbin(&autoc, max_order);

    // Order choice by the usual estimate: every order costs its coefficients
    // and saves half a bit per sample per halving of the prediction error.
    let n = signal.len() as f64;
    let cost = |o: usize| {
        n / 2.0 * errors[o].max(1e-9).log2() + (o as f64) * f64::from(config.qlp_precision)
    };
    let order = (1..=max_order)
        .filter(|&o| !coefs_by_order[o].is_empty())
        .min_by(|&a, &b| cost(a).total_cmp(&cost(b)))?;

    let (coefs, shift) = quantize_coefficients(&coefs_by_order[order], config.qlp_precision)?;
    let mut residual = Vec::with_capacity(signal.len() - order);
    for i in order..signal.len() {
        let mut sum = 0i64;
        for (k, &c) in coefs.iter().enumerate() {
            sum += i64::from(c) * i64::from(signal[i - 1 - k]);
        }
        let r = i64::from(signal[i]) - (sum >> shift);
        residual.push(i32::try_from(r).ok()?);
    }
    let rice = plan_rice(config, &residual, signal.len(), order);
    Some(SubframePlan {
        bits: header_bits
            + order * bps as usize
            + 4
            + 5
            + order * config.qlp_precision as usize
            + rice.bits,
        wasted: 0,
        kind: PlanKind::Lpc {
            warmup: signal[..order].to_vec(),
            precision: config.qlp_precision,
            shift,
            coefs,
            residual,
            rice: rice.plan,
        },
    })
}

struct RiceChoice {
    bits: usize,
    plan: RicePlan,
}

/// Choose the partition order and per-partition Rice parameters.
///
/// Parameters come from partition sums with the usual estimate, then the whole
/// configuration is costed exactly — so orders are compared by real bit counts
/// rather than by two estimates.
fn plan_rice(
    config: &EncoderConfig,
    residual: &[i32],
    block_size: usize,
    order: usize,
) -> RiceChoice {
    let folded: Vec<u32> = residual.iter().map(|&r| fold(r)).collect();
    let mut best: Option<RiceChoice> = None;
    for partition_order in 0..=config.max_partition_order {
        let partitions = 1usize << partition_order;
        if !block_size.is_multiple_of(partitions) {
            break;
        }
        let partition_len = block_size / partitions;
        if partition_len <= order {
            break;
        }
        let mut params = Vec::with_capacity(partitions);
        let mut bits = 0usize;
        let mut at = 0usize;
        let mut max_param = 0u32;
        for p in 0..partitions {
            let count = partition_len - if p == 0 { order } else { 0 };
            let part = &folded[at..at + count];
            at += count;
            let sum: u64 = part.iter().map(|&v| u64::from(v)).sum();
            let param = best_rice_param(sum, count as u64);
            bits += part
                .iter()
                .map(|&v| (v >> param) as usize + 1 + param as usize)
                .sum::<usize>();
            max_param = max_param.max(param);
            params.push(param);
        }
        let param_bits = if max_param > 14 { 5 } else { 4 };
        bits += 2 + 4 + partitions * param_bits as usize;
        let choice = RiceChoice {
            bits,
            plan: RicePlan {
                param_bits,
                partition_order,
                order,
                params,
            },
        };
        if best.as_ref().is_none_or(|b| choice.bits < b.bits) {
            best = Some(choice);
        }
    }
    best.expect("partition order 0 is always legal")
}

/// Zig-zag fold: FLAC codes residuals as unsigned with the sign in bit 0.
fn fold(r: i32) -> u32 {
    ((r << 1) ^ (r >> 31)) as u32
}

/// The Rice parameter whose quotients cost least for a partition of `count`
/// folded values summing to `sum`, capped at 30 (31 is the escape code).
fn best_rice_param(sum: u64, count: u64) -> u32 {
    if count == 0 {
        return 0;
    }
    let mean = sum / count;
    let mut param = 0u32;
    while param < 30 && (1u64 << (param + 1)) <= mean {
        param += 1;
    }
    let cost = |k: u32| count * (1 + u64::from(k)) + (sum >> k);
    let mut best = param;
    for k in param.saturating_sub(1)..=(param + 1).min(30) {
        if cost(k) < cost(best) {
            best = k;
        }
    }
    best
}

fn write_subframe(w: &mut BitWriter, plan: &SubframePlan, bps: u32) {
    let kind_code = match &plan.kind {
        PlanKind::Constant(_) => 0,
        PlanKind::Verbatim(_) => 1,
        PlanKind::Fixed { warmup, .. } => 8 + warmup.len() as u32,
        PlanKind::Lpc { warmup, .. } => 31 + warmup.len() as u32,
    };
    w.write_bit(false);
    w.write_bits(kind_code, 6);
    w.write_bit(plan.wasted > 0);
    if plan.wasted > 0 {
        // Unary: `wasted - 1` zeroes then a one.
        for _ in 0..plan.wasted - 1 {
            w.write_bit(false);
        }
        w.write_bit(true);
    }
    let bps = bps - plan.wasted;
    match &plan.kind {
        PlanKind::Constant(v) => w.write_signed(*v, bps),
        PlanKind::Verbatim(samples) => {
            for &s in samples {
                w.write_signed(s, bps);
            }
        }
        PlanKind::Fixed {
            warmup,
            residual,
            rice,
        } => {
            for &s in warmup {
                w.write_signed(s, bps);
            }
            write_residual(w, residual, rice);
        }
        PlanKind::Lpc {
            warmup,
            precision,
            shift,
            coefs,
            residual,
            rice,
        } => {
            for &s in warmup {
                w.write_signed(s, bps);
            }
            w.write_bits(precision - 1, 4);
            w.write_bits(*shift, 5);
            for &c in coefs {
                w.write_signed(c, *precision);
            }
            write_residual(w, residual, rice);
        }
    }
}

fn write_residual(w: &mut BitWriter, residual: &[i32], rice: &RicePlan) {
    w.write_bits(u32::from(rice.param_bits == 5), 2);
    w.write_bits(rice.partition_order, 4);
    let partitions = 1usize << rice.partition_order;
    let partition_len = (residual.len() + rice.order) / partitions;
    let mut at = 0usize;
    for (p, &param) in rice.params.iter().enumerate() {
        let count = partition_len - if p == 0 { rice.order } else { 0 };
        w.write_bits(param, rice.param_bits);
        for &r in &residual[at..at + count] {
            let folded = fold(r);
            for _ in 0..folded >> param {
                w.write_bit(false);
            }
            w.write_bit(true);
            if param > 0 {
                w.write_bits(folded & ((1 << param) - 1), param);
            }
        }
        at += count;
    }
    debug_assert_eq!(at, residual.len());
}

fn tukey_window(n: usize) -> Vec<f64> {
    // Tukey(0.5): a Hann taper over the outer quarter at each end, flat between.
    const P: f64 = 0.5;
    let taper = (P * (n as f64 - 1.0) / 2.0).max(1.0);
    (0..n)
        .map(|i| {
            let i = i as f64;
            let last = n as f64 - 1.0;
            if i < taper {
                0.5 * (1.0 - (std::f64::consts::PI * i / taper).cos())
            } else if i > last - taper {
                0.5 * (1.0 - (std::f64::consts::PI * (last - i) / taper).cos())
            } else {
                1.0
            }
        })
        .collect()
}

fn autocorrelation(signal: &[f64], max_order: usize) -> Vec<f64> {
    (0..=max_order)
        .map(|lag| {
            signal[lag..]
                .iter()
                .zip(signal)
                .map(|(a, b)| a * b)
                .sum::<f64>()
        })
        .collect()
}

/// Levinson-Durbin: predictor coefficients for every order up to `max_order`,
/// plus the prediction error each order leaves.
///
/// The returned coefficients are in FLAC's sense — `x[n] ~ sum c[k]*x[n-1-k]` —
/// which is the negation of the recursion's internal ones.
fn levinson_durbin(autoc: &[f64], max_order: usize) -> (Vec<Vec<f64>>, Vec<f64>) {
    let mut coefs = vec![Vec::new(); max_order + 1];
    let mut errors = vec![autoc[0]; max_order + 1];
    let mut a = vec![0.0f64; max_order];
    let mut err = autoc[0];
    for i in 0..max_order {
        if err <= 0.0 {
            break;
        }
        let mut r = -autoc[i + 1];
        for j in 0..i {
            r -= a[j] * autoc[i - j];
        }
        r /= err;
        a[i] = r;
        for j in 0..i / 2 {
            let tmp = a[j];
            a[j] += r * a[i - 1 - j];
            a[i - 1 - j] += r * tmp;
        }
        if i % 2 == 1 {
            let j = i / 2;
            a[j] += a[j] * r;
        }
        err *= 1.0 - r * r;
        errors[i + 1] = err;
        coefs[i + 1] = a[..=i].iter().map(|&v| -v).collect();
    }
    (coefs, errors)
}

/// Quantise predictor coefficients to `precision` bits and a shift, carrying
/// the rounding error forward.
fn quantize_coefficients(coefs: &[f64], precision: u32) -> Option<(Vec<i32>, u32)> {
    const MAX_SHIFT: i32 = 15;
    let cmax = coefs.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    if cmax <= 0.0 || !cmax.is_finite() {
        return None;
    }
    let log2cmax = cmax.log2().floor() as i32;
    let shift = (precision as i32 - 1 - log2cmax - 1).min(MAX_SHIFT);
    if shift < 0 {
        return None; // coefficients too large to quantise; fixed predictors win.
    }
    let limit = 1i32 << (precision - 1);
    let scale = f64::from(1i32 << shift);
    let mut error = 0.0f64;
    let mut out = Vec::with_capacity(coefs.len());
    for &c in coefs {
        error += c * scale;
        let q = error.round().clamp(f64::from(-limit), f64::from(limit - 1)) as i32;
        error -= f64::from(q);
        out.push(q);
    }
    Some((out, shift as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{Block, FlacReader, read_utf8_number};
    use ec_core::bitio::BitReader;

    fn tone(frames: usize, channels: usize, bps: u32) -> Vec<i32> {
        let amp = f64::from(1i32 << (bps - 2));
        (0..frames * channels)
            .map(|i| {
                let t = (i / channels) as f64;
                let c = (i % channels) as f64;
                (amp * ((t * 0.031 + c * 0.7).sin() + 0.2 * (t * 0.29).sin())) as i32
            })
            .collect()
    }

    fn round_trip(samples: &[i32], channels: usize, bps: u32, rate: u32) {
        let config = EncoderConfig::default();
        let bytes = encode(&config, samples, channels, bps, rate).expect("encode");
        let mut reader = FlacReader::new(&bytes).expect("open");
        let info = reader.stream_info().expect("streaminfo").clone();
        assert_eq!(info.sample_rate, rate);
        assert_eq!(usize::from(info.channels), channels);
        assert_eq!(u32::from(info.bits_per_sample), bps);
        assert_eq!(info.total_samples, (samples.len() / channels) as u64);
        let decoded = reader.decode_all().expect("decode");
        assert_eq!(
            decoded.interleaved(),
            samples,
            "{channels}ch/{bps}bit/{rate}Hz did not round-trip"
        );
        assert_eq!(
            info.md5,
            md5_of_samples(&decoded.interleaved(), bps),
            "STREAMINFO MD5 must match the decoded audio"
        );
    }

    #[test]
    fn round_trips_every_shape_the_family_uses() {
        for (channels, bps, rate) in [
            (1usize, 16u32, 44100u32),
            (2, 16, 44100),
            (2, 16, 48000),
            (2, 24, 96000),
            (6, 16, 48000),
            (8, 24, 48000),
            (1, 8, 8000),
            (2, 20, 192_000),
        ] {
            round_trip(&tone(5000, channels, bps), channels, bps, rate);
        }
    }

    #[test]
    fn round_trips_the_awkward_signals() {
        let config = EncoderConfig::default();
        let cases: Vec<Vec<i32>> = vec![
            vec![0; 4096 * 2],              // digital silence
            vec![i32::from(i16::MIN); 100], // constant full scale
            (0..8192)
                .map(|i| if i % 64 < 32 { 3000 } else { -3000 })
                .collect(), // square
            vec![-1],                       // one sample
            (0..4096).map(|i| (i % 7) * 256).collect(), // wasted bits
            (0..4096)
                .map(|i| ((i * 2654435761u64 % 65536) as i32) - 32768)
                .collect(), // noise
        ];
        for samples in cases {
            let bytes = encode(&config, &samples, 1, 16, 44100).expect("encode");
            let decoded = FlacReader::new(&bytes)
                .expect("open")
                .decode_all()
                .expect("decode");
            assert_eq!(decoded.interleaved(), samples);
        }
    }

    #[test]
    fn a_short_last_block_still_decodes() {
        let config = EncoderConfig {
            block_size: 512,
            ..EncoderConfig::default()
        };
        let samples = tone(1300, 2, 16);
        let bytes = encode(&config, &samples, 2, 16, 44100).expect("encode");
        let mut reader = FlacReader::new(&bytes).expect("open");
        let mut block = Block::default();
        let mut sizes = Vec::new();
        while reader.next_block(&mut block).expect("frame") {
            sizes.push(block.len());
        }
        assert_eq!(sizes, vec![512, 512, 276]);
    }

    #[test]
    fn refuses_input_it_cannot_encode_losslessly() {
        let config = EncoderConfig::default();
        assert!(encode(&config, &[1 << 20], 1, 16, 44100).is_err());
        assert!(encode(&config, &[1, 2, 3], 2, 16, 44100).is_err());
        assert!(encode(&config, &[0; 16], 9, 16, 44100).is_err());
        assert!(encode(&config, &[0; 16], 1, 33, 44100).is_err());
        assert!(encode(&config, &[0; 16], 1, 16, 0).is_err());
    }

    #[test]
    fn utf8_coded_numbers_round_trip() {
        for value in [
            0u64,
            1,
            0x7f,
            0x80,
            0x7ff,
            0x800,
            0xffff,
            0x10_ffff,
            0x3ff_ffff,
            0x7fff_ffff,
        ] {
            let mut w = BitWriter::new();
            write_utf8_number(&mut w, value);
            let bytes = w.into_bytes();
            let mut r = BitReader::new(&bytes);
            assert_eq!(
                read_utf8_number(&mut r, 36).unwrap(),
                value,
                "number {value}"
            );
        }
    }

    #[test]
    fn compresses_better_than_raw_pcm() {
        let samples = tone(44100, 2, 16);
        let raw = samples.len() * 2;
        let bytes = encode(&EncoderConfig::default(), &samples, 2, 16, 44100).unwrap();
        assert!(
            bytes.len() < raw / 2,
            "{} bytes from {raw} of PCM is not compression",
            bytes.len()
        );
    }
}
