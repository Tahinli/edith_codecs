//! `raw_data_block` decoding: ISO/IEC 14496-3 §4.4.2 syntax, §4.6 tools.
//!
//! Band, group and window indices are the domain's own coordinates and address
//! several parallel arrays at once, so the loops here are written over indices
//! rather than iterators.
#![allow(clippy::needless_range_loop)]

use ec_core::{BitReader, Error, Result};
use ec_dsp::{Mdct, Window};

use crate::huffman::Books;
use crate::tables::{CODEBOOKS, SWB_LONG, SWB_SHORT, TNS_MAX_BANDS_LONG, TNS_MAX_BANDS_SHORT};

/// Samples one AAC frame carries per channel.
pub const FRAME_LEN: usize = 1024;
/// Samples one short block carries.
pub const SHORT_LEN: usize = 128;
/// The `sect_cb` value for a band coded as all zeroes.
const ZERO_HCB: u8 = 0;
/// The `sect_cb` value for perceptual noise substitution.
const NOISE_HCB: u8 = 13;
/// Out-of-phase intensity stereo.
const INTENSITY_HCB2: u8 = 14;
/// In-phase intensity stereo.
const INTENSITY_HCB: u8 = 15;
/// `sf` is an exponent in quarter-powers of two around this offset (§4.6.2).
const SF_OFFSET: i32 = 100;
/// The dequantiser works in the standard's own units, where full scale is
/// 2^16; the public API speaks the usual +/-1 float range. Checked against a
/// reference decode in `adts_fixtures_match_the oracle_per_channel`, which compares
/// amplitudes and not just shape.
pub(crate) const OUTPUT_SCALE: f32 = 1.0 / 65536.0;
/// The longest TNS filter AAC-LC allows (long blocks; short blocks stop at 7).
const TNS_MAX_ORDER_LONG: usize = 12;
const TNS_MAX_ORDER_SHORT: usize = 7;

/// The four window sequences of §4.6.11.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowSequence {
    OnlyLong,
    LongStart,
    EightShort,
    LongStop,
}

impl WindowSequence {
    fn from_bits(v: u32) -> WindowSequence {
        match v {
            1 => WindowSequence::LongStart,
            2 => WindowSequence::EightShort,
            3 => WindowSequence::LongStop,
            _ => WindowSequence::OnlyLong,
        }
    }

    /// True for `EIGHT_SHORT_SEQUENCE`, which changes almost every field width.
    pub fn is_short(self) -> bool {
        self == WindowSequence::EightShort
    }
}

/// One `raw_data_block` element's coding-tool usage, gated by
/// `EC_AAC_TOOL_SIDEINFO_DEBUG` -- mirrors `sbr_chain::SbrSideInfoRow`'s
/// pattern for the LC core, so a real file's PNS/M-S/IS/TNS mix can be read
/// back per AU without re-parsing debug text.
#[derive(Clone, Debug)]
pub struct ToolSideInfoRow {
    pub au: usize,
    pub tag: u8,
    pub is_cpe: bool,
    pub window_sequence: WindowSequence,
    pub ms_bands: usize,
    pub pns_bands: usize,
    pub is_bands: usize,
    pub tns_present: bool,
}

static TOOL_SIDEINFO_LOG: std::sync::OnceLock<std::sync::Mutex<Vec<ToolSideInfoRow>>> =
    std::sync::OnceLock::new();
static TOOL_SIDEINFO_AU: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn tool_sideinfo_enabled() -> bool {
    std::env::var("EC_AAC_TOOL_SIDEINFO_DEBUG").is_ok()
}

fn log_tool_sideinfo(row: ToolSideInfoRow) {
    let log = TOOL_SIDEINFO_LOG.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut l) = log.lock() {
        l.push(row);
    }
}

/// Bumps the shared AU counter once per `raw_data_block`, returning the value
/// that call's rows should carry.
fn next_tool_sideinfo_au() -> usize {
    TOOL_SIDEINFO_AU.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Every row logged so far this process, in call order. Empty unless
/// `EC_AAC_TOOL_SIDEINFO_DEBUG` was set before decoding.
pub fn tool_sideinfo_log() -> Vec<ToolSideInfoRow> {
    let Some(log) = TOOL_SIDEINFO_LOG.get() else {
        return Vec::new();
    };
    log.lock().map(|l| l.clone()).unwrap_or_default()
}

/// `ics_info` (§4.4.2.1) plus the grouping it implies.
#[derive(Clone, Debug)]
pub struct IcsInfo {
    pub window_sequence: WindowSequence,
    /// False for the sine window, true for Kaiser-Bessel-derived.
    pub window_shape_kbd: bool,
    pub max_sfb: usize,
    pub num_windows: usize,
    pub num_groups: usize,
    /// Windows per group, `num_groups` entries.
    pub group_len: [usize; 8],
    /// Band offsets in force for this window shape, `num_swb + 1` entries.
    pub swb: &'static [u16],
    /// Highest band a TNS filter may reach at this rate and window length.
    pub tns_max_bands: usize,
}

impl IcsInfo {
    fn num_swb(&self) -> usize {
        self.swb.len() - 1
    }

    /// First window index of each group.
    fn group_start(&self, g: usize) -> usize {
        self.group_len[..g].iter().sum()
    }
}

fn parse_ics_info(r: &mut BitReader<'_>, sf_index: u8) -> Result<IcsInfo> {
    let _reserved = r.read_bit()?;
    let window_sequence = WindowSequence::from_bits(r.read_bits(2)?);
    let window_shape_kbd = r.read_bit()?;
    let idx = usize::from(sf_index).min(11);
    let (max_sfb, num_windows, num_groups, group_len, swb, tns_max_bands) =
        if window_sequence.is_short() {
            let max_sfb = r.read_bits(4)? as usize;
            let grouping = r.read_bits(7)?;
            let mut group_len = [0usize; 8];
            let mut groups = 1;
            group_len[0] = 1;
            for i in 0..7 {
                if (grouping >> (6 - i)) & 1 == 1 {
                    group_len[groups - 1] += 1;
                } else {
                    group_len[groups] = 1;
                    groups += 1;
                }
            }
            (
                max_sfb,
                8,
                groups,
                group_len,
                SWB_SHORT[idx],
                TNS_MAX_BANDS_SHORT[idx],
            )
        } else {
            let max_sfb = r.read_bits(6)? as usize;
            if r.read_bit()? {
                // AAC-LC has no predictor; LTP/main streams are a different profile.
                return Err(Error::unsupported(
                    "aac",
                    "predictor_data_present: main or LTP profile, not AAC-LC",
                ));
            }
            let mut group_len = [0usize; 8];
            group_len[0] = 1;
            (
                max_sfb,
                1,
                1,
                group_len,
                SWB_LONG[idx],
                TNS_MAX_BANDS_LONG[idx],
            )
        };
    if max_sfb > swb.len() - 1 {
        return Err(Error::corrupt("aac: max_sfb past the band table"));
    }
    Ok(IcsInfo {
        window_sequence,
        window_shape_kbd,
        max_sfb,
        num_windows,
        num_groups,
        group_len,
        swb,
        tns_max_bands,
    })
}

/// One TNS filter as transmitted, already converted to LPC coefficients.
#[derive(Clone, Debug, Default)]
struct TnsFilter {
    /// Bands the filter spans, counted down from the top.
    length: usize,
    order: usize,
    /// True when the filter runs from high frequency down.
    downward: bool,
    lpc: Vec<f32>,
}

#[derive(Clone, Debug, Default)]
struct Tns {
    /// Filters per window.
    filters: Vec<Vec<TnsFilter>>,
}

fn parse_tns(r: &mut BitReader<'_>, ics: &IcsInfo) -> Result<Tns> {
    let short = ics.window_sequence.is_short();
    let (n_filt_bits, len_bits, order_bits) = if short { (1, 4, 3) } else { (2, 6, 5) };
    let max_order = if short {
        TNS_MAX_ORDER_SHORT
    } else {
        TNS_MAX_ORDER_LONG
    };
    let mut filters = Vec::with_capacity(ics.num_windows);
    for _ in 0..ics.num_windows {
        let n_filt = r.read_bits(n_filt_bits)? as usize;
        let mut coef_res = 0;
        if n_filt > 0 {
            coef_res = u32::from(r.read_bit()?);
        }
        let mut win = Vec::with_capacity(n_filt);
        for _ in 0..n_filt {
            let length = r.read_bits(len_bits)? as usize;
            let order = r.read_bits(order_bits)? as usize;
            if order == 0 {
                win.push(TnsFilter {
                    length,
                    ..TnsFilter::default()
                });
                continue;
            }
            // A stream may state a longer filter than the profile allows; the
            // extra taps are dropped rather than the frame refused.
            let order = order.min(max_order);
            let downward = r.read_bit()?;
            let compress = u32::from(r.read_bit()?);
            let res_bits = coef_res + 3;
            let bits = res_bits - compress;
            // §4.6.9.3: the transmitted index is a signed fraction of a quarter
            // turn, and the two half-step factors keep the mapping symmetric.
            let iqfac = ((1 << (res_bits - 1)) as f64 - 0.5) / core::f64::consts::FRAC_PI_2;
            let iqfac_m = ((1 << (res_bits - 1)) as f64 + 0.5) / core::f64::consts::FRAC_PI_2;
            let mut parcor = Vec::with_capacity(order);
            for _ in 0..order {
                let raw = r.read_bits(bits)? as i32;
                let sign_bit = 1 << (bits - 1);
                let value = if raw & sign_bit != 0 {
                    raw - (1 << bits)
                } else {
                    raw
                };
                let scaled = if value >= 0 {
                    f64::from(value) / iqfac
                } else {
                    f64::from(value) / iqfac_m
                };
                parcor.push(scaled.sin());
            }
            win.push(TnsFilter {
                length,
                order,
                downward,
                lpc: parcor_to_lpc(&parcor),
            });
        }
        filters.push(win);
    }
    Ok(Tns { filters })
}

/// Reflection coefficients to direct-form LPC (§4.6.9.3), `a[0] == 1`.
fn parcor_to_lpc(parcor: &[f64]) -> Vec<f32> {
    let mut a = vec![0.0f64; parcor.len() + 1];
    let mut tmp = vec![0.0f64; parcor.len() + 1];
    a[0] = 1.0;
    for (m, &k) in parcor.iter().enumerate() {
        for i in 1..=m {
            tmp[i] = a[i] + k * a[m + 1 - i];
        }
        a[1..=m].copy_from_slice(&tmp[1..=m]);
        a[m + 1] = k;
    }
    a.into_iter().map(|v| v as f32).collect()
}

/// Applies every TNS filter of one window in place (§4.6.9.2).
fn apply_tns(coef: &mut [f32], tns: &Tns, ics: &IcsInfo, window: usize) {
    let Some(filters) = tns.filters.get(window) else {
        return;
    };
    let num_swb = ics.num_swb();
    let mut bottom = num_swb;
    for filter in filters {
        let top = bottom;
        bottom = top.saturating_sub(filter.length);
        if filter.order == 0 {
            continue;
        }
        let cap = ics.max_sfb.min(ics.tns_max_bands);
        let start = usize::from(ics.swb[bottom.min(cap)]);
        let end = usize::from(ics.swb[top.min(cap)]);
        if end <= start {
            continue;
        }
        let order = filter.order;
        let mut state = vec![0.0f32; order + 1];
        let walk = |i: usize, state: &mut Vec<f32>, coef: &mut [f32]| {
            let mut y = coef[i];
            for j in 1..=order {
                y -= filter.lpc[j] * state[j - 1];
            }
            for j in (1..=order).rev() {
                state[j] = state[j - 1];
            }
            state[0] = y;
            coef[i] = y;
        };
        if filter.downward {
            for i in (start..end).rev() {
                walk(i, &mut state, coef);
            }
        } else {
            for i in start..end {
                walk(i, &mut state, coef);
            }
        }
    }
}

/// Per-channel data of one `individual_channel_stream`.
struct Ics {
    ics: IcsInfo,
    /// `sect_cb` by group and band.
    cb: Vec<Vec<u8>>,
    /// Scalefactor, intensity position or noise energy by group and band.
    sf: Vec<Vec<i32>>,
    coef: Vec<f32>,
    tns: Tns,
}

/// The decoder's per-channel memory: the filterbank overlap and the window
/// shape the next frame's rising edge has to match.
#[derive(Clone, Debug)]
pub struct ChannelState {
    overlap: Vec<f32>,
    prev_shape_kbd: bool,
    prev_short: bool,
}

impl Default for ChannelState {
    fn default() -> ChannelState {
        ChannelState {
            overlap: vec![0.0; FRAME_LEN],
            prev_shape_kbd: false,
            prev_short: false,
        }
    }
}

/// The four windows and two transforms the filterbank needs, built once.
pub struct FilterBank {
    long: Mdct<f32>,
    short: Mdct<f32>,
    sine_long: Vec<f32>,
    kbd_long: Vec<f32>,
    sine_short: Vec<f32>,
    kbd_short: Vec<f32>,
    scratch_long: Vec<f32>,
    scratch_short: Vec<f32>,
}

impl FilterBank {
    pub fn new() -> FilterBank {
        FilterBank {
            long: Mdct::new(2 * FRAME_LEN),
            short: Mdct::new(2 * SHORT_LEN),
            sine_long: Window::<f32>::sine(2 * FRAME_LEN).as_slice().to_vec(),
            kbd_long: Window::<f32>::kbd(2 * FRAME_LEN, 4.0).as_slice().to_vec(),
            sine_short: Window::<f32>::sine(2 * SHORT_LEN).as_slice().to_vec(),
            kbd_short: Window::<f32>::kbd(2 * SHORT_LEN, 6.0).as_slice().to_vec(),
            scratch_long: vec![0.0; 2 * FRAME_LEN],
            scratch_short: vec![0.0; 2 * SHORT_LEN],
        }
    }

    fn long_win(&self, kbd: bool) -> &[f32] {
        if kbd { &self.kbd_long } else { &self.sine_long }
    }

    fn short_win(&self, kbd: bool) -> &[f32] {
        if kbd {
            &self.kbd_short
        } else {
            &self.sine_short
        }
    }

    /// Inverse filterbank and overlap-add for one channel (§4.6.11).
    fn synthesize(
        &mut self,
        spec: &[f32],
        ics: &IcsInfo,
        state: &mut ChannelState,
        out: &mut [f32],
    ) {
        let mut frame = vec![0.0f32; 2 * FRAME_LEN];
        match ics.window_sequence {
            WindowSequence::EightShort => {
                for w in 0..8 {
                    let src = &spec[w * SHORT_LEN..(w + 1) * SHORT_LEN];
                    self.short.inverse(src, &mut self.scratch_short);
                    let rise = self.short_win(if w == 0 {
                        state.prev_shape_kbd
                    } else {
                        ics.window_shape_kbd
                    });
                    let fall = self.short_win(ics.window_shape_kbd);
                    let base = 448 + w * SHORT_LEN;
                    for i in 0..SHORT_LEN {
                        frame[base + i] += self.scratch_short[i] * rise[i];
                        frame[base + SHORT_LEN + i] +=
                            self.scratch_short[SHORT_LEN + i] * fall[SHORT_LEN + i];
                    }
                }
            }
            seq => {
                self.long.inverse(spec, &mut self.scratch_long);
                let prev_long = self.long_win(state.prev_shape_kbd);
                let prev_short = self.short_win(state.prev_shape_kbd);
                let cur_long = self.long_win(ics.window_shape_kbd);
                let cur_short = self.short_win(ics.window_shape_kbd);
                for i in 0..FRAME_LEN {
                    let rise = match seq {
                        // A stop window opens flat: nothing to overlap with the
                        // short block that preceded it until sample 448.
                        WindowSequence::LongStop => match i {
                            0..448 => 0.0,
                            448..576 => prev_short[i - 448],
                            _ => 1.0,
                        },
                        _ => prev_long[i],
                    };
                    frame[i] = self.scratch_long[i] * rise;
                }
                for i in 0..FRAME_LEN {
                    let fall = match seq {
                        WindowSequence::LongStart => match i {
                            0..448 => 1.0,
                            448..576 => cur_short[SHORT_LEN + i - 448],
                            _ => 0.0,
                        },
                        _ => cur_long[FRAME_LEN + i],
                    };
                    frame[FRAME_LEN + i] = self.scratch_long[FRAME_LEN + i] * fall;
                }
            }
        }
        for i in 0..FRAME_LEN {
            out[i] = (state.overlap[i] + frame[i]) * OUTPUT_SCALE;
        }
        state.overlap.copy_from_slice(&frame[FRAME_LEN..]);
        state.prev_shape_kbd = ics.window_shape_kbd;
        state.prev_short = ics.window_sequence.is_short();
    }
}

impl Default for FilterBank {
    fn default() -> FilterBank {
        FilterBank::new()
    }
}

/// Deterministic noise source for PNS; the standard leaves the sequence free,
/// only the band energy is normative.
#[derive(Clone, Debug)]
pub struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        i32::from_ne_bytes(self.0.to_ne_bytes()) as f32
    }
}

/// The syntactic and tool state one `raw_data_block` needs.
pub struct BlockDecoder {
    pub books: Books,
    pub bank: FilterBank,
    pub states: Vec<ChannelState>,
    noise: Noise,
    sf_index: u8,
    sbr: Option<crate::sbr_chain::SbrChain>,
}

impl BlockDecoder {
    pub fn new(sf_index: u8) -> BlockDecoder {
        BlockDecoder {
            books: Books::new(),
            bank: FilterBank::new(),
            states: Vec::new(),
            noise: Noise(0x1f2e_3d4c),
            sf_index,
            sbr: None,
        }
    }

    pub fn set_sf_index(&mut self, sf_index: u8) {
        self.sf_index = sf_index;
    }

    /// Enables (or disables, passing `None`) SBR reconstruction at the
    /// given extension (doubled) sample rate.
    pub fn set_sbr_rate(&mut self, rate: Option<u32>) {
        self.sbr = rate.map(crate::sbr_chain::SbrChain::new);
    }

    fn section_data(&self, r: &mut BitReader<'_>, ics: &IcsInfo) -> Result<Vec<Vec<u8>>> {
        let bits = if ics.window_sequence.is_short() { 3 } else { 5 };
        let esc = (1u32 << bits) - 1;
        let mut out = Vec::with_capacity(ics.num_groups);
        for _ in 0..ics.num_groups {
            let mut row = vec![ZERO_HCB; ics.max_sfb];
            let mut k = 0usize;
            while k < ics.max_sfb {
                let cb = r.read_bits(4)? as u8;
                let mut len = 0usize;
                loop {
                    let incr = r.read_bits(bits)?;
                    len += incr as usize;
                    if incr != esc {
                        break;
                    }
                }
                if len == 0 || k + len > ics.max_sfb {
                    return Err(Error::corrupt("aac: section runs past max_sfb"));
                }
                row[k..k + len].fill(cb);
                k += len;
            }
            out.push(row);
        }
        Ok(out)
    }

    fn scale_factor_data(
        &self,
        r: &mut BitReader<'_>,
        ics: &IcsInfo,
        cb: &[Vec<u8>],
        global_gain: u8,
    ) -> Result<Vec<Vec<i32>>> {
        let mut scale = i32::from(global_gain);
        let mut intensity = 0i32;
        let mut noise = i32::from(global_gain) - 90;
        let mut noise_pcm = true;
        let mut out = Vec::with_capacity(ics.num_groups);
        for g in 0..ics.num_groups {
            let mut row = vec![0i32; ics.max_sfb];
            for sfb in 0..ics.max_sfb {
                row[sfb] = match cb[g][sfb] {
                    ZERO_HCB => 0,
                    INTENSITY_HCB | INTENSITY_HCB2 => {
                        intensity += self.books.scalefactor.decode(r)? as i32 - 60;
                        intensity
                    }
                    NOISE_HCB => {
                        if noise_pcm {
                            noise_pcm = false;
                            noise += r.read_bits(9)? as i32 - 256;
                        } else {
                            noise += self.books.scalefactor.decode(r)? as i32 - 60;
                        }
                        noise
                    }
                    _ => {
                        scale += self.books.scalefactor.decode(r)? as i32 - 60;
                        if !(0..=255).contains(&scale) {
                            return Err(Error::corrupt("aac: scalefactor out of range"));
                        }
                        scale
                    }
                };
            }
            out.push(row);
        }
        Ok(out)
    }

    /// `spectral_data` (§4.4.2.7) into quantised integers, window-major.
    fn spectral_data(
        &self,
        r: &mut BitReader<'_>,
        ics: &IcsInfo,
        cb: &[Vec<u8>],
        quant: &mut [i32],
    ) -> Result<()> {
        for g in 0..ics.num_groups {
            let start = ics.group_start(g);
            for sfb in 0..ics.max_sfb {
                let book = cb[g][sfb];
                if !(1..=11).contains(&book) {
                    continue;
                }
                let spec = &CODEBOOKS[usize::from(book) - 1];
                let tree = &self.books.spectral[usize::from(book) - 1];
                let lo = usize::from(ics.swb[sfb]);
                let hi = usize::from(ics.swb[sfb + 1]);
                for w in 0..ics.group_len[g] {
                    let base = if ics.window_sequence.is_short() {
                        (start + w) * SHORT_LEN
                    } else {
                        0
                    };
                    let mut k = lo;
                    while k < hi {
                        let mut values = [0i32; 4];
                        let symbol = tree.decode(r)?;
                        decode_tuple(spec, symbol, &mut values);
                        let dim = usize::from(spec.dim);
                        if spec.unsigned {
                            for v in values.iter_mut().take(dim) {
                                if *v != 0 && r.read_bit()? {
                                    *v = -*v;
                                }
                            }
                        }
                        if spec.esc {
                            for v in values.iter_mut().take(dim) {
                                if v.unsigned_abs() == 16 {
                                    let mut n = 0u32;
                                    while r.read_bit()? {
                                        n += 1;
                                        if n > 20 {
                                            return Err(Error::corrupt("aac: escape too long"));
                                        }
                                    }
                                    let magnitude = (1i32 << (n + 4)) | r.read_bits(n + 4)? as i32;
                                    *v = if *v < 0 { -magnitude } else { magnitude };
                                }
                            }
                        }
                        for (i, &v) in values.iter().take(dim).enumerate() {
                            if k + i < hi {
                                quant[base + k + i] = v;
                            }
                        }
                        k += dim;
                    }
                }
            }
        }
        Ok(())
    }

    /// One `individual_channel_stream` (§4.4.2.4).
    fn channel_stream(&mut self, r: &mut BitReader<'_>, shared: Option<&IcsInfo>) -> Result<Ics> {
        let global_gain = r.read_bits(8)? as u8;
        let ics = match shared {
            Some(info) => info.clone(),
            None => parse_ics_info(r, self.sf_index)?,
        };
        let cb = self.section_data(r, &ics)?;
        let sf = self.scale_factor_data(r, &ics, &cb, global_gain)?;
        let mut pulse = None;
        if r.read_bit()? {
            if ics.window_sequence.is_short() {
                return Err(Error::corrupt("aac: pulse data on a short window"));
            }
            pulse = Some(parse_pulse(r)?);
        }
        let tns = if r.read_bit()? {
            parse_tns(r, &ics)?
        } else {
            Tns::default()
        };
        if r.read_bit()? {
            return Err(Error::unsupported("aac", "gain control data: SSR profile"));
        }
        let mut quant = vec![0i32; FRAME_LEN];
        self.spectral_data(r, &ics, &cb, &mut quant)?;
        if let Some(p) = pulse {
            apply_pulse(&mut quant, &p, &ics);
        }
        Ok(Ics {
            coef: dequantize(&quant, &ics, &cb, &sf),
            ics,
            cb,
            sf,
            tns,
        })
    }

    /// Runs the stereo tools, TNS and the filterbank over one element's
    /// channels, appending each channel's PCM to `out`.
    fn finish(&mut self, chans: &mut [Ics], ms: Option<&MsMask>, out: &mut Vec<Vec<f32>>) {
        // Noise substitution (§4.6.13) must fill its bands before M/S and
        // intensity stereo run, so those tools combine the noise-filled
        // coefficients like any other spectral data rather than the zeroes
        // `dequantize` left behind for NOISE_HCB bands -- matching the
        // decode order every reference decoder uses.
        for ch in chans.iter_mut() {
            fill_noise(ch, &mut self.noise);
        }
        if let ([left, right], Some(mask)) = (&mut *chans, ms) {
            apply_ms(left, right, mask);
        }
        if let [left, right] = &mut *chans {
            apply_intensity(left, right, ms);
        }
        for ch in chans.iter_mut() {
            let windows = if ch.ics.window_sequence.is_short() {
                8
            } else {
                1
            };
            for w in 0..windows {
                let len = if windows == 8 { SHORT_LEN } else { FRAME_LEN };
                apply_tns(&mut ch.coef[w * len..(w + 1) * len], &ch.tns, &ch.ics, w);
            }
            let mut pcm = vec![0.0f32; FRAME_LEN];
            let index = out.len();
            while self.states.len() <= index {
                self.states.push(ChannelState::default());
            }
            self.bank
                .synthesize(&ch.coef, &ch.ics, &mut self.states[index], &mut pcm);
            out.push(pcm);
        }
    }

    /// Decodes one `raw_data_block`, returning one PCM plane per channel in
    /// bitstream element order.
    pub fn raw_data_block(&mut self, r: &mut BitReader<'_>) -> Result<Vec<Vec<f32>>> {
        let mut out: Vec<Vec<f32>> = Vec::new();
        // The element (tag, channel range, is_cpe) an immediately following
        // FIL's SBR payload would apply to, per §4.5.1: a fill_element
        // directly follows the element it decorates.
        let mut pending: Option<(u8, usize, usize, bool)> = None;
        let dump_tools = tool_sideinfo_enabled();
        let au = if dump_tools { next_tool_sideinfo_au() } else { 0 };
        loop {
            if r.bits_remaining() < 3 {
                break;
            }
            let id = r.read_bits(3)?;
            match id {
                // SCE and LFE are one channel each; CCE (2) is a coupling
                // element this profile never emits and cannot be skipped
                // blind, so it is refused rather than silently mis-parsed.
                0 | 3 => {
                    let tag = r.read_bits(4)? as u8;
                    let start = out.len();
                    let mut ch = [self.channel_stream(r, None)?];
                    if dump_tools {
                        log_tool_sideinfo(tool_row(au, tag, false, &ch[0], None));
                    }
                    self.finish(&mut ch, None, &mut out);
                    // §4.6.18.1: SBR is never applied to the LFE channel (no
                    // FIL/SBR payload follows it), so it must not become a
                    // pending SBR target -- otherwise a later fallback
                    // (`apply_last`/no-FIL branch) could mistakenly stretch
                    // the LFE plane to double rate using a *different*
                    // element's held SBR data, corrupting it.
                    pending = if id == 0 {
                        Some((tag, start, 1, false))
                    } else {
                        None
                    };
                }
                1 => {
                    let tag = r.read_bits(4)? as u8;
                    let start = out.len();
                    let common = r.read_bit()?;
                    let (shared, mask) = if common {
                        let info = parse_ics_info(r, self.sf_index)?;
                        let present = r.read_bits(2)?;
                        let mask = match present {
                            0 => MsMask::None,
                            1 => {
                                let mut bits = vec![vec![false; info.max_sfb]; info.num_groups];
                                for row in bits.iter_mut() {
                                    for b in row.iter_mut() {
                                        *b = r.read_bit()?;
                                    }
                                }
                                MsMask::Some(bits)
                            }
                            2 => MsMask::All,
                            _ => return Err(Error::corrupt("aac: reserved ms_mask_present")),
                        };
                        (Some(info), mask)
                    } else {
                        (None, MsMask::None)
                    };
                    let mut pair = [
                        self.channel_stream(r, shared.as_ref())?,
                        self.channel_stream(r, shared.as_ref())?,
                    ];
                    if dump_tools {
                        log_tool_sideinfo(tool_row(au, tag, true, &pair[0], Some(&mask)));
                    }
                    self.finish(&mut pair, Some(&mask), &mut out);
                    pending = Some((tag, start, 2, true));
                }
                2 => return Err(Error::unsupported("aac", "coupling channel element")),
                4 => {
                    let _tag = r.read_bits(4)?;
                    let align = r.read_bit()?;
                    let mut count = r.read_bits(8)? as u64;
                    if count == 255 {
                        count += r.read_bits(8)? as u64;
                    }
                    if align {
                        r.align_to_byte();
                    }
                    r.skip_bits(count * 8)?;
                    pending = None;
                }
                5 => {
                    crate::config::skip_program_config(r)?;
                    pending = None;
                }
                6 => {
                    let mut count = r.read_bits(4)? as u64;
                    if count == 15 {
                        count += r.read_bits(8)? as u64 - 1;
                    }
                    let end_bit = r.bit_position() + count * 8;
                    if let (Some(sbr), Some((tag, start, n, is_cpe))) =
                        (self.sbr.as_mut(), pending.take())
                    {
                        // extension_payload(): bs_extension_type(4), then
                        // (for SBR) an optional 10-bit CRC, then
                        // sbr_extension_data() itself.
                        let mut applied = false;
                        if r.bits_remaining() >= 4 {
                            let ext_type = r.peek_bits(4)?;
                            if ext_type == 13 || ext_type == 14 {
                                r.skip_bits(4)?;
                                if ext_type == 14 && r.bits_remaining() >= 10 {
                                    r.skip_bits(10)?; // bs_sbr_crc_bits
                                }
                                if let Some(data) = sbr.parse(r, tag, is_cpe) {
                                    sbr.apply(tag, is_cpe, &data, &mut out[start..start + n]);
                                    applied = true;
                                }
                            }
                        }
                        if !applied {
                            // No fresh SBR payload this frame (no SBR fill
                            // element, or one whose extension_type wasn't
                            // SBR, or a parse failure): §4.6.18 still
                            // requires doubled-rate reconstruction, using
                            // the last successfully parsed frame's data.
                            sbr.apply_last(tag, is_cpe, &mut out[start..start + n]);
                        }
                    }
                    // Resync to the FIL's declared byte boundary regardless
                    // of how many bits SBR parsing above actually consumed:
                    // a parse error or trailing padding must never desync
                    // the elements that follow.
                    let now = r.bit_position();
                    if now < end_bit {
                        r.skip_bits(end_bit - now)?;
                    }
                    pending = None;
                }
                _ => {
                    // The block ended with no FIL element ever following the
                    // last SCE/CPE (no SBR fill at all this frame, not even
                    // a non-SBR one) -- same "unavailable frame" case as the
                    // no-fresh-payload branch in id 6 above, so fall back to
                    // the last known SBR data the same way.
                    if let (Some(sbr), Some((tag, start, n, is_cpe))) =
                        (self.sbr.as_mut(), pending)
                    {
                        sbr.apply_last(tag, is_cpe, &mut out[start..start + n]);
                    }
                    break;
                }
            }
        }
        r.align_to_byte();
        Ok(out)
    }
}

#[derive(Debug)]
enum MsMask {
    None,
    All,
    Some(Vec<Vec<bool>>),
}

impl MsMask {
    fn used(&self, g: usize, sfb: usize) -> bool {
        match self {
            MsMask::None => false,
            MsMask::All => true,
            MsMask::Some(bits) => bits[g][sfb],
        }
    }
}

/// The symbol index back into its tuple; the index is the tuple read as digits
/// in base `lav + 1` (unsigned) or `2 * lav + 1` (signed), first coefficient
/// most significant.
fn decode_tuple(cb: &crate::tables::Codebook, symbol: usize, out: &mut [i32; 4]) {
    let span = if cb.unsigned {
        usize::from(cb.lav) + 1
    } else {
        2 * usize::from(cb.lav) + 1
    };
    let off = if cb.unsigned { 0 } else { i32::from(cb.lav) };
    let dim = usize::from(cb.dim);
    let mut rest = symbol;
    for i in (0..dim).rev() {
        out[i] = (rest % span) as i32 - off;
        rest /= span;
    }
}

#[derive(Debug)]
struct Pulse {
    start_sfb: usize,
    offsets: Vec<usize>,
    amps: Vec<i32>,
}

fn parse_pulse(r: &mut BitReader<'_>) -> Result<Pulse> {
    let count = r.read_bits(2)? as usize + 1;
    let start_sfb = r.read_bits(6)? as usize;
    let mut offsets = Vec::with_capacity(count);
    let mut amps = Vec::with_capacity(count);
    for _ in 0..count {
        offsets.push(r.read_bits(5)? as usize);
        amps.push(r.read_bits(4)? as i32);
    }
    Ok(Pulse {
        start_sfb,
        offsets,
        amps,
    })
}

fn apply_pulse(quant: &mut [i32], pulse: &Pulse, ics: &IcsInfo) {
    if pulse.start_sfb >= ics.swb.len() {
        return;
    }
    let mut k = usize::from(ics.swb[pulse.start_sfb]);
    for (&off, &amp) in pulse.offsets.iter().zip(&pulse.amps) {
        k += off;
        if k >= quant.len() {
            return;
        }
        // The pulse grows the magnitude, keeping whatever sign the coefficient
        // already had (§4.6.3).
        quant[k] += if quant[k] < 0 { -amp } else { amp };
    }
}

/// `x^(4/3) * 2^((sf - 100) / 4)` per band (§4.6.2), window-major.
fn dequantize(quant: &[i32], ics: &IcsInfo, cb: &[Vec<u8>], sf: &[Vec<i32>]) -> Vec<f32> {
    let mut coef = vec![0.0f32; FRAME_LEN];
    let short = ics.window_sequence.is_short();
    for g in 0..ics.num_groups {
        let start = ics.group_start(g);
        for sfb in 0..ics.max_sfb {
            let book = cb[g][sfb];
            if !(1..=11).contains(&book) {
                continue;
            }
            let gain = 2f32.powf((sf[g][sfb] - SF_OFFSET) as f32 * 0.25);
            let lo = usize::from(ics.swb[sfb]);
            let hi = usize::from(ics.swb[sfb + 1]);
            for w in 0..ics.group_len[g] {
                let base = if short { (start + w) * SHORT_LEN } else { 0 };
                for k in lo..hi {
                    let q = quant[base + k];
                    if q != 0 {
                        let mag = (q.unsigned_abs() as f32).powf(4.0 / 3.0) * gain;
                        coef[base + k] = if q < 0 { -mag } else { mag };
                    }
                }
            }
        }
    }
    coef
}

/// Mid/side to left/right, for every band the mask marks and no other (§4.6.8.1).
fn apply_ms(left: &mut Ics, right: &mut Ics, mask: &MsMask) {
    let ics = &left.ics;
    let short = ics.window_sequence.is_short();
    for g in 0..ics.num_groups {
        let start = ics.group_start(g);
        for sfb in 0..ics.max_sfb {
            if !mask.used(g, sfb) || is_intensity(right.cb[g][sfb]) {
                continue;
            }
            let lo = usize::from(ics.swb[sfb]);
            let hi = usize::from(ics.swb[sfb + 1]);
            for w in 0..ics.group_len[g] {
                let base = if short { (start + w) * SHORT_LEN } else { 0 };
                for k in base + lo..base + hi {
                    let (m, s) = (left.coef[k], right.coef[k]);
                    left.coef[k] = m + s;
                    right.coef[k] = m - s;
                }
            }
        }
    }
}

/// Builds one `EC_AAC_TOOL_SIDEINFO_DEBUG` row from an already-decoded `Ics`
/// (any channel of a CPE, since PNS/IS/TNS presence is per-channel but the
/// dump only needs whether the element used the tool at all).
fn tool_row(au: usize, tag: u8, is_cpe: bool, ch: &Ics, mask: Option<&MsMask>) -> ToolSideInfoRow {
    let mut ms_bands = 0;
    let mut pns_bands = 0;
    let mut is_bands = 0;
    for g in 0..ch.ics.num_groups {
        for sfb in 0..ch.ics.max_sfb {
            if mask.is_some_and(|m| m.used(g, sfb)) {
                ms_bands += 1;
            }
            match ch.cb[g][sfb] {
                NOISE_HCB => pns_bands += 1,
                INTENSITY_HCB | INTENSITY_HCB2 => is_bands += 1,
                _ => {}
            }
        }
    }
    ToolSideInfoRow {
        au,
        tag,
        is_cpe,
        window_sequence: ch.ics.window_sequence,
        ms_bands,
        pns_bands,
        is_bands,
        tns_present: !ch.tns.filters.is_empty(),
    }
}

fn is_intensity(cb: u8) -> bool {
    cb == INTENSITY_HCB || cb == INTENSITY_HCB2
}

/// Intensity stereo: the right channel's marked bands are the left channel's,
/// scaled by `0.5^(is_position / 4)` and signed by the codebook (§4.6.8.2).
fn apply_intensity(left: &mut Ics, right: &mut Ics, ms: Option<&MsMask>) {
    let ics = right.ics.clone();
    let short = ics.window_sequence.is_short();
    for g in 0..ics.num_groups {
        let start = ics.group_start(g);
        for sfb in 0..ics.max_sfb {
            let cb = right.cb[g][sfb];
            if !is_intensity(cb) {
                continue;
            }
            let mut sign = if cb == INTENSITY_HCB { 1.0 } else { -1.0 };
            if ms.is_some_and(|m| m.used(g, sfb)) {
                sign = -sign;
            }
            let scale = sign * 0.5f32.powf(0.25 * right.sf[g][sfb] as f32);
            let lo = usize::from(ics.swb[sfb]);
            let hi = usize::from(ics.swb[sfb + 1]);
            for w in 0..ics.group_len[g] {
                let base = if short { (start + w) * SHORT_LEN } else { 0 };
                for k in base + lo..base + hi {
                    right.coef[k] = left.coef[k] * scale;
                }
            }
        }
    }
}

/// Perceptual noise substitution: the band is filled with noise carrying the
/// transmitted energy (§4.6.13). The sequence is free, the energy is not.
fn fill_noise(ch: &mut Ics, rng: &mut Noise) {
    let ics = ch.ics.clone();
    let short = ics.window_sequence.is_short();
    for g in 0..ics.num_groups {
        let start = ics.group_start(g);
        for sfb in 0..ics.max_sfb {
            if ch.cb[g][sfb] != NOISE_HCB {
                continue;
            }
            let lo = usize::from(ics.swb[sfb]);
            let hi = usize::from(ics.swb[sfb + 1]);
            for w in 0..ics.group_len[g] {
                let base = if short { (start + w) * SHORT_LEN } else { 0 };
                let mut energy = 0.0f32;
                for k in base + lo..base + hi {
                    let v = rng.next();
                    ch.coef[k] = v;
                    energy += v * v;
                }
                if energy <= 0.0 {
                    continue;
                }
                let target = 2f32.powf(0.25 * ch.sf[g][sfb] as f32);
                let scale = target / energy.sqrt();
                for k in base + lo..base + hi {
                    ch.coef[k] *= scale;
                }
            }
        }
    }
}
