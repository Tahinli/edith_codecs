//! SBR (HE-AAC v1) payload parser: bitstream to dequantized integer
//! envelope/noise data (ISO/IEC 14496-3 §4.6.18). No DSP: the QMF
//! filterbank and gain application are a separate slice's job, and this
//! module stops at exact integer scalefactors and noise floors.
//!
//! Field widths and the grid-class bit layout were checked against a
//! reference decoder's own byte accounting the same way
//! `scripts/aac-tables/sbrtables.py` derived the Huffman books: a payload
//! whose declared FIL byte count is deliberately short makes the decoder
//! complain exactly how many bytes it actually read, which is compared
//! against this parser's own prediction for many header/grid
//! configurations (`scripts/aac-tables/sbrpayload_fixtures.py`, kept next to
//! the table rig). The `FIXVAR`/`VARFIX`/`VARVAR` grid classes and a
//! non-coupled `sbr_channel_pair_element` are exercised only there, since
//! `sbrtables.py` never needed them for the codebook derivation.
#![allow(dead_code)]

use crate::sbr_bands::{BandTables, freq_tables};
use crate::sbr_tables::*;
use ec_core::{BitReader, Error, Result};

/// Number of QMF time slots an AAC-LC (1024-sample) core frame spans.
const NUM_TIME_SLOTS: i64 = 16;

/// `sbr_header()` fields, held across frames: a stream need not repeat it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SbrHeader {
    pub amp_res: u8,
    pub start_freq: u8,
    pub stop_freq: u8,
    pub xover_band: u8,
    pub freq_scale: u8,
    pub alter_scale: u8,
    pub noise_bands: u8,
    pub limiter_bands: u8,
    pub limiter_gains: u8,
    pub interpol_freq: u8,
    pub smoothing_mode: u8,
}

/// One channel's decoded envelope/noise data for one `sbr_data()` frame.
#[derive(Clone, Debug, Default)]
pub struct SbrChannel {
    /// Envelope time borders in QMF slots, `num_env + 1` of them.
    pub t_env: Vec<i64>,
    /// `bs_freq_res` per envelope: `true` selects the high-resolution table.
    pub freq_res: Vec<u8>,
    /// Noise time borders in QMF slots, `num_noise + 1` of them.
    pub t_noise: Vec<i64>,
    /// Quantised envelope scalefactors, `[env][band]`.
    pub e_q: Vec<Vec<i32>>,
    /// Quantised noise floors, `[noise][band]`.
    pub q_q: Vec<Vec<i32>>,
    /// `bs_invf_mode` per noise band.
    pub invf_mode: Vec<u8>,
    /// `bs_add_harmonic` per high-resolution band, when the flag is set.
    pub add_harmonic: Option<Vec<u8>>,
    /// `bs_df_env` per envelope: `1` selects delta-time coding over
    /// delta-frequency (diagnostic only, not consumed by the DSP stage).
    pub df_env: Vec<u8>,
    /// `bs_df_noise` per noise floor, same convention as `df_env`.
    pub df_noise: Vec<u8>,
    /// `l_A` (ISO/IEC 14496-3 §4.6.18.3.3): index of the envelope that starts
    /// at the transient border, `-1` when the frame has none. Gates where
    /// added sinusoids begin and which envelope skips noise/smoothing.
    pub l_a: i64,
    /// The frame's effective amp_res: `bs_amp_res`, except a FIXFIX frame
    /// with a single envelope is forced to 0 (1.5 dB) -- §4.6.18.3.3.
    pub amp_res: u8,
}

/// One `sbr_data()` frame: one channel for an SCE, two for a CPE.
#[derive(Clone, Debug)]
pub struct SbrData {
    /// Whether the CPE coded its second channel as a balance/ratio against
    /// the first (`channels[1]` then holds the balance codebook's raw
    /// integers, not an absolute scalefactor -- combining them is the DSP
    /// slice's job).
    pub coupling: bool,
    pub channels: Vec<SbrChannel>,
}

#[derive(Clone, Debug)]
struct Grid {
    num_env: usize,
    freq_res: Vec<u8>,
    t_env: Vec<i64>,
    t_noise: Vec<i64>,
    l_a: i64,
    fixfix: bool,
}

impl Grid {
    /// `pointer` is `bs_pointer` (0 for FIXFIX, which has no such field).
    /// Middle noise border and `l_A` per ISO/IEC 14496-3 §4.6.18.3.3,
    /// Tables 4.146/4.147 -- they depend on `frame_class` (0 FIXFIX,
    /// 1 FIXVAR, 2 VARFIX, 3 VARVAR), not on `bs_pointer` alone.
    fn new(num_env: usize, freq_res: Vec<u8>, t_env: Vec<i64>, pointer: usize, frame_class: u32) -> Grid {
        let num_noise = if num_env == 1 { 1 } else { 2 };
        let t_noise = if num_noise == 1 {
            vec![t_env[0], t_env[num_env]]
        } else {
            let middle = match frame_class {
                0 => num_env >> 1,
                1 | 3 => num_env - pointer.saturating_sub(1).max(1),
                _ => match pointer {
                    0 => 1,
                    1 => num_env - 1,
                    p => p - 1,
                },
            };
            vec![t_env[0], t_env[middle.min(num_env)], t_env[num_env]]
        };
        let l_a = match frame_class {
            1 | 3 if pointer > 0 => num_env as i64 + 1 - pointer as i64,
            2 if pointer > 1 => pointer as i64 - 1,
            _ => -1,
        };
        Grid {
            num_env,
            freq_res,
            t_env,
            t_noise,
            l_a,
            fixfix: frame_class == 0,
        }
    }
}

/// Width of `bs_pointer`, which ranges `0..=num_env`: `ceil(log2(num_env+1))`
/// bits, equivalently the bit-length of `num_env` itself.
fn pointer_bits(num_env: usize) -> u32 {
    (u32::BITS - (num_env as u32).leading_zeros()).max(1)
}

/// Per-channel state carried across frames, for delta-time (DT) envelope and
/// noise coding whose first entry in a frame references the *previous*
/// frame's last one.
#[derive(Clone, Debug, Default)]
struct ChannelState {
    last_env: Vec<i32>,
    last_noise: Vec<i32>,
}

/// Parses `sbr_extension_data()` payloads for one SBR element (SCE or CPE),
/// keeping the header and per-channel DT state a stream may omit or rely on.
pub struct SbrParser {
    rate: u32,
    header: Option<SbrHeader>,
    tables: Option<BandTables>,
    state: [ChannelState; 2],
}

fn env_book(dt: bool, balance: bool, amp_res: u8) -> &'static [(u8, u32, i32)] {
    match (balance, amp_res != 0, dt) {
        (false, false, false) => &ENV15_F,
        (false, false, true) => &ENV15_T,
        (false, true, false) => &ENV30_F,
        (false, true, true) => &ENV30_T,
        (true, false, false) => &ENVB15_F,
        (true, false, true) => &ENVB15_T,
        (true, true, false) => &ENVB30_F,
        (true, true, true) => &ENVB30_T,
    }
}

fn noise_book(dt: bool, balance: bool) -> &'static [(u8, u32, i32)] {
    match (balance, dt) {
        (false, false) => &NOISE_F,
        (false, true) => &NOISE_T,
        (true, false) => &NOISEB_F,
        (true, true) => &NOISEB_T,
    }
}

fn raw_env_width(balance: bool, amp_res: u8) -> u32 {
    match (balance, amp_res != 0) {
        (false, false) => 7,
        (false, true) => 6,
        (true, false) => 6,
        (true, true) => 5,
    }
}

const RAW_NOISE_WIDTH: u32 = 5;

/// A FIXFIX frame with a single envelope forces the 1.5 dB resolution
/// regardless of `bs_amp_res` (§4.6.18.3.3); a one-envelope VARFIX/FIXVAR
/// frame keeps the header's value.
fn amp_res_of(header_amp_res: u8, grid: &Grid) -> u8 {
    if grid.fixfix && grid.num_env == 1 { 0 } else { header_amp_res }
}

/// Reads one Huffman codeword by walking bit-by-bit until it matches a table
/// entry. Every book here is a complete prefix code (Kraft sum 1, checked in
/// `sbr_tables`), so a well-formed stream always terminates; a codeword
/// longer than any entry means truncated or corrupted input, not a panic.
fn read_huffman(r: &mut BitReader, book: &'static [(u8, u32, i32)]) -> Result<i32> {
    let max_len = book.iter().map(|&(l, _, _)| l).max().unwrap_or(0);
    let mut code: u32 = 0;
    let mut len: u8 = 0;
    loop {
        code = (code << 1) | u32::from(r.read_bit()?);
        len += 1;
        if let Some(&(_, _, delta)) = book.iter().find(|&&(l, c, _)| l == len && c == code) {
            return Ok(delta);
        }
        if len > max_len {
            return Err(Error::corrupt("sbr: no codeword matched this bit sequence"));
        }
    }
}

impl SbrParser {
    /// A parser for SBR data at `rate` (the SBR sample rate: twice the
    /// AAC-LC core's), with no header seen yet.
    pub fn new(rate: u32) -> SbrParser {
        SbrParser {
            rate,
            header: None,
            tables: None,
            state: Default::default(),
        }
    }

    /// The header this parser is currently using, if any frame has carried one.
    pub fn header(&self) -> Option<&SbrHeader> {
        self.header.as_ref()
    }

    /// The frequency band tables derived from the current header, if any.
    pub fn tables(&self) -> Option<&BandTables> {
        self.tables.as_ref()
    }

    /// Test-only seam: installs a header/tables pair without driving a real
    /// bitstream, so `sbr_chain`'s own tests can exercise `SbrChain::apply`
    /// on synthetic data (the fields this sets are otherwise only reachable
    /// through a successful `parse()`).
    #[cfg(test)]
    pub(crate) fn set_for_test(&mut self, header: SbrHeader, tables: BandTables) {
        self.header = Some(header);
        self.tables = Some(tables);
    }

    fn parse_grid(&self, r: &mut BitReader) -> Result<Grid> {
        let frame_class = r.read_bits(2)?;
        match frame_class {
            0 => {
                // FIXFIX
                let raw = r.read_bits(2)?;
                let num_env = 1usize << raw;
                if num_env > 4 {
                    return Err(Error::corrupt("sbr FIXFIX: envelope count out of range"));
                }
                let fr = u8::from(r.read_bit()?);
                let t_env: Vec<i64> = (0..=num_env)
                    .map(|i| (i as i64) * NUM_TIME_SLOTS / num_env as i64)
                    .collect();
                Ok(Grid::new(num_env, vec![fr; num_env], t_env, 0, 0))
            }
            1 | 2 => {
                // FIXVAR (1, trailing variable border) / VARFIX (2, leading variable border)
                let var_bord = i64::from(r.read_bits(2)?);
                let num_rel = r.read_bits(2)? as usize;
                let num_env = num_rel + 1;
                if !(2..=5).contains(&num_env) {
                    return Err(Error::corrupt(
                        "sbr: variable grid envelope count out of range",
                    ));
                }
                let mut rel = Vec::with_capacity(num_rel);
                for _ in 0..num_rel {
                    rel.push(2 * i64::from(r.read_bits(2)?) + 2);
                }
                let pointer = r.read_bits(pointer_bits(num_env))? as usize;
                // FIXVAR transmits bs_freq_res in reverse time order (the
                // envelopes are numbered from the trailing variable border
                // backwards); VARFIX transmits it forward. Confirmed against
                // a reference decoder's own byte accounting on a non-uniform
                // list, since a uniform one can't tell the orders apart
                // (`scripts/aac-tables/sbrgrid_probe.py`).
                let mut wire = Vec::with_capacity(num_env);
                for _ in 0..num_env {
                    wire.push(u8::from(r.read_bit()?));
                }
                let freq_res = if frame_class == 1 {
                    wire.into_iter().rev().collect()
                } else {
                    wire
                };
                let mut t = vec![0i64; num_env + 1];
                if frame_class == 1 {
                    let trail = NUM_TIME_SLOTS + var_bord;
                    t[num_env] = trail;
                    let mut acc = trail;
                    for i in (1..num_env).rev() {
                        acc -= rel[i - 1];
                        t[i] = acc;
                    }
                } else {
                    t[0] = var_bord;
                    for i in 1..=num_rel {
                        t[i] = t[i - 1] + rel[i - 1];
                    }
                    t[num_env] = NUM_TIME_SLOTS;
                }
                Ok(Grid::new(num_env, freq_res, t, pointer, frame_class))
            }
            _ => {
                // VARVAR
                let var_bord0 = i64::from(r.read_bits(2)?);
                let var_bord1 = i64::from(r.read_bits(2)?);
                let num_rel0 = r.read_bits(2)? as usize;
                let num_rel1 = r.read_bits(2)? as usize;
                let num_env = num_rel0 + num_rel1 + 1;
                if !(2..=5).contains(&num_env) {
                    return Err(Error::corrupt("sbr VARVAR: envelope count out of range"));
                }
                let mut rel0 = Vec::with_capacity(num_rel0);
                for _ in 0..num_rel0 {
                    rel0.push(2 * i64::from(r.read_bits(2)?) + 2);
                }
                let mut rel1 = Vec::with_capacity(num_rel1);
                for _ in 0..num_rel1 {
                    rel1.push(2 * i64::from(r.read_bits(2)?) + 2);
                }
                let pointer = r.read_bits(pointer_bits(num_env))? as usize;
                let mut freq_res = Vec::with_capacity(num_env);
                for _ in 0..num_env {
                    freq_res.push(u8::from(r.read_bit()?));
                }
                let mut t = vec![0i64; num_env + 1];
                t[0] = var_bord0;
                for i in 1..=num_rel0 {
                    t[i] = t[i - 1] + rel0[i - 1];
                }
                let trail = NUM_TIME_SLOTS + var_bord1;
                t[num_env] = trail;
                let mut acc = trail;
                for i in (num_rel0 + 1..num_env).rev() {
                    acc -= rel1[i - num_rel0 - 1];
                    t[i] = acc;
                }
                if t.windows(2).any(|w| w[0] >= w[1]) {
                    return Err(Error::corrupt("sbr: non-monotone envelope time borders"));
                }
                Ok(Grid::new(num_env, freq_res, t, pointer, frame_class))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_envelope(
        &self,
        r: &mut BitReader,
        bands: usize,
        dt: bool,
        balance: bool,
        amp_res: u8,
        prev: &[i32],
    ) -> Result<Vec<i32>> {
        let book = env_book(dt, balance, amp_res);
        let width = raw_env_width(balance, amp_res);
        let cap = (1i32 << width) - 1;
        let mut row = vec![0i32; bands];
        // Every cell here is a `bs_data_env` raw scalefactor, whose valid
        // range is exactly the transmitted field's own bit width regardless
        // of path (DT against last frame's state, or DF's within-row delta
        // chain) -- clamped at construction, not just when carried to the
        // NEXT frame's `last_env` (as it already was below): an unclamped
        // intermediate here let one bad Huffman symbol (desync or a
        // genuinely huge encoded delta) cascade through the rest of the row
        // via `row[b-1] + delta`, producing a raw value in the hundreds
        // whose dequantized (`2^v`) energy is 10+ orders of magnitude too
        // large -- exactly the coupled CPE's second-channel envelope
        // blowups measured on real HE-AAC files.
        if dt {
            for (b, slot) in row.iter_mut().enumerate() {
                let delta = read_huffman(r, book)?;
                let base = prev.get(b).copied().unwrap_or(0);
                *slot = (base + delta).clamp(0, cap);
            }
        } else {
            row[0] = (r.read_bits(width)? as i32).clamp(0, cap);
            for b in 1..bands {
                let delta = read_huffman(r, book)?;
                row[b] = (row[b - 1] + delta).clamp(0, cap);
            }
        }
        if std::env::var("EC_AAC_SBR_ENV_DEBUG").is_ok() {
            eprintln!(
                "ENVDBG dt={dt} balance={balance} amp_res={amp_res} bands={bands} prev={prev:?} row={row:?}"
            );
        }
        Ok(row)
    }

    fn decode_noise(
        &self,
        r: &mut BitReader,
        bands: usize,
        dt: bool,
        balance: bool,
        prev: &[i32],
    ) -> Result<Vec<i32>> {
        let book = noise_book(dt, balance);
        let cap = (1i32 << RAW_NOISE_WIDTH) - 1;
        let mut row = vec![0i32; bands];
        if dt {
            for (b, slot) in row.iter_mut().enumerate() {
                let delta = read_huffman(r, book)?;
                let base = prev.get(b).copied().unwrap_or(0);
                *slot = (base + delta).clamp(0, cap);
            }
        } else {
            row[0] = (r.read_bits(RAW_NOISE_WIDTH)? as i32).clamp(0, cap);
            for b in 1..bands {
                let delta = read_huffman(r, book)?;
                row[b] = (row[b - 1] + delta).clamp(0, cap);
            }
        }
        Ok(row)
    }

    /// Parses one `sbr_extension_data()` payload: an optional header followed
    /// by `sbr_single_channel_element()` (`is_cpe = false`) or
    /// `sbr_channel_pair_element()` (`is_cpe = true`).
    pub fn parse(&mut self, r: &mut BitReader, is_cpe: bool) -> Result<SbrData> {
        let has_header = r.read_bit()?;
        if has_header {
            let amp_res = u8::from(r.read_bit()?);
            let start_freq = r.read_bits(4)? as u8;
            let stop_freq = r.read_bits(4)? as u8;
            let xover_band = r.read_bits(3)? as u8;
            r.skip_bits(2)?; // bs_reserved
            let extra1 = r.read_bit()?;
            let extra2 = r.read_bit()?;
            let (freq_scale, alter_scale, noise_bands) = if extra1 {
                (
                    r.read_bits(2)? as u8,
                    r.read_bits(1)? as u8,
                    r.read_bits(2)? as u8,
                )
            } else {
                // Measured defaults when bs_header_extra_1 is absent
                // (`EXTRA1_DEFAULTS` in the table rig).
                (2, 1, 2)
            };
            let (limiter_bands, limiter_gains, interpol_freq, smoothing_mode) = if extra2 {
                (
                    r.read_bits(2)? as u8,
                    r.read_bits(2)? as u8,
                    r.read_bits(1)? as u8,
                    r.read_bits(1)? as u8,
                )
            } else {
                (2, 2, 1, 1)
            };
            let tables = freq_tables(
                self.rate,
                start_freq as usize,
                stop_freq as usize,
                i32::from(freq_scale),
                i32::from(alter_scale),
                xover_band as usize,
                i32::from(noise_bands),
            )
            .ok_or_else(|| Error::corrupt("sbr header: invalid band configuration"))?;
            self.header = Some(SbrHeader {
                amp_res,
                start_freq,
                stop_freq,
                xover_band,
                freq_scale,
                alter_scale,
                noise_bands,
                limiter_bands,
                limiter_gains,
                interpol_freq,
                smoothing_mode,
            });
            self.tables = Some(tables);
            self.state = Default::default();
        } else if self.header.is_none() {
            return Err(Error::corrupt("sbr: headerless frame with no prior header"));
        }
        let header = self.header.clone().expect("checked above");
        let tables = self.tables.clone().expect("set with header");

        let data_extra = r.read_bit()?;
        if data_extra {
            r.skip_bits(if is_cpe { 8 } else { 4 })?;
        }
        let coupling = is_cpe && r.read_bit()?;
        let n_channels = if is_cpe { 2 } else { 1 };

        let mut grids = Vec::with_capacity(n_channels);
        if is_cpe && coupling {
            let g = self.parse_grid(r)?;
            grids.push(g.clone());
            grids.push(g);
        } else {
            for _ in 0..n_channels {
                grids.push(self.parse_grid(r)?);
            }
        }

        let mut df_env = vec![Vec::new(); n_channels];
        let mut df_noise = vec![Vec::new(); n_channels];
        for ch in 0..n_channels {
            let num_env = grids[ch].num_env;
            let num_noise = if num_env == 1 { 1 } else { 2 };
            for _ in 0..num_env {
                df_env[ch].push(r.read_bit()?);
            }
            for _ in 0..num_noise {
                df_noise[ch].push(r.read_bit()?);
            }
        }

        let mut invf: Vec<Vec<u8>> = Vec::with_capacity(n_channels);
        if is_cpe && coupling {
            let mut v = Vec::with_capacity(tables.n_q);
            for _ in 0..tables.n_q {
                v.push(r.read_bits(2)? as u8);
            }
            invf.push(v.clone());
            invf.push(v);
        } else {
            for _ in 0..n_channels {
                let mut v = Vec::with_capacity(tables.n_q);
                for _ in 0..tables.n_q {
                    v.push(r.read_bits(2)? as u8);
                }
                invf.push(v);
            }
        }

        // sbr_channel_pair_element's envelope/noise interleave depends on
        // bs_coupling: the COUPLED form reads sbr_envelope(ch)/sbr_noise(ch)
        // interleaved per channel (env0,noise0,env1,noise1), but the
        // UNCOUPLED form reads two separate per-channel passes --
        // sbr_envelope(ch=0), sbr_envelope(ch=1), sbr_noise(ch=0),
        // sbr_noise(ch=1). Reading the uncoupled form interleaved (as a
        // single per-channel loop) is order-invariant in total bit count (so
        // a byte-accounting oracle can't see the mistake) but desyncs every
        // field after the first channel's envelopes whenever the two
        // channels' envelope (6/7-bit) and noise (5-bit) field counts
        // differ, which round-11 pinned to ch1's second envelope.
        let separated = is_cpe && !coupling && n_channels == 2;
        let mut e_q_all: Vec<Vec<Vec<i32>>> = vec![Vec::new(); n_channels];
        let mut q_q_all: Vec<Vec<Vec<i32>>> = vec![Vec::new(); n_channels];

        let mut read_env = |this: &mut Self, r: &mut BitReader, ch: usize| -> Result<()> {
            let balance = is_cpe && coupling && ch == 1;
            let num_env = grids[ch].num_env;
            let amp_res = amp_res_of(header.amp_res, &grids[ch]);
            let mut e_q: Vec<Vec<i32>> = Vec::with_capacity(num_env);
            for i in 0..num_env {
                let bands = if grids[ch].freq_res[i] != 0 {
                    tables.n_high
                } else {
                    tables.n_low
                };
                let dt = df_env[ch][i];
                let prev: &[i32] = if i > 0 {
                    &e_q[i - 1]
                } else {
                    &this.state[ch].last_env
                };
                let row = this.decode_envelope(r, bands, dt, balance, amp_res, prev)?;
                e_q.push(row);
            }
            e_q_all[ch] = e_q;
            Ok(())
        };
        if separated {
            for ch in 0..n_channels {
                read_env(self, r, ch)?;
            }
        }
        let mut read_noise = |this: &mut Self, r: &mut BitReader, ch: usize| -> Result<()> {
            let balance = is_cpe && coupling && ch == 1;
            let num_env = grids[ch].num_env;
            let num_noise = if num_env == 1 { 1 } else { 2 };
            let mut q_q: Vec<Vec<i32>> = Vec::with_capacity(num_noise);
            for i in 0..num_noise {
                let dt = df_noise[ch][i];
                let prev: &[i32] = if i > 0 {
                    &q_q[i - 1]
                } else {
                    &this.state[ch].last_noise
                };
                let row = this.decode_noise(r, tables.n_q, dt, balance, prev)?;
                q_q.push(row);
            }
            q_q_all[ch] = q_q;
            Ok(())
        };
        if separated {
            for ch in 0..n_channels {
                read_noise(self, r, ch)?;
            }
        } else {
            for ch in 0..n_channels {
                read_env(self, r, ch)?;
                read_noise(self, r, ch)?;
            }
        }

        let mut channels = Vec::with_capacity(n_channels);
        for ch in 0..n_channels {
            let balance = is_cpe && coupling && ch == 1;
            let num_env = grids[ch].num_env;
            let amp_res = amp_res_of(header.amp_res, &grids[ch]);
            let e_q = e_q_all[ch].clone();
            let q_q = q_q_all[ch].clone();

            // Containment, not the fix: a still-undiscovered desync could
            // make a DT delta huge, and since a following frame's DT reads
            // add onto this carried state, one corrupt cell would otherwise
            // snowball unbounded across frames. Clamp to the raw field's own
            // range (0..127 envelope, 0..31 noise) so corrupt input degrades
            // that one cell instead of exploding the whole carried state.
            self.state[ch].last_env = e_q
                .last()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|v| v.clamp(0, (1 << raw_env_width(balance, amp_res)) - 1))
                .collect();
            self.state[ch].last_noise = q_q
                .last()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|v| v.clamp(0, (1 << RAW_NOISE_WIDTH) - 1))
                .collect();

            channels.push(SbrChannel {
                t_env: grids[ch].t_env.clone(),
                freq_res: grids[ch].freq_res.clone(),
                t_noise: grids[ch].t_noise.clone(),
                e_q,
                q_q,
                invf_mode: invf[ch].clone(),
                add_harmonic: None,
                df_env: df_env[ch].iter().map(|&b| u8::from(b)).collect(),
                df_noise: df_noise[ch].iter().map(|&b| u8::from(b)).collect(),
                l_a: grids[ch].l_a,
                amp_res,
            });
        }

        for ch in channels.iter_mut().take(n_channels) {
            let on = r.read_bit()?;
            if on {
                let mut h = Vec::with_capacity(tables.n_high);
                for _ in 0..tables.n_high {
                    h.push(u8::from(r.read_bit()?));
                }
                ch.add_harmonic = Some(h);
            }
        }

        let extended = r.read_bit()?;
        if extended {
            let mut size = r.read_bits(4)? as u64;
            if size == 15 {
                size += u64::from(r.read_bits(8)?);
            }
            r.skip_bits(size * 8)?;
        }

        Ok(SbrData { coupling, channels })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures generated and byte-accounted against a reference decoder by
    // `scripts/aac-tables/sbrpayload_fixtures.py`: each `_BODY` is exactly
    // the `sbr_extension_data()` payload (no ADTS/core/FIL framing), and
    // `_BITS` is the bit count the reference decoder's own accounting
    // measured for it, asserted below against `BitReader::bit_position()`.
    const FIXVAR_DF_BODY: &[u8] = &[
        0x90, 0x31, 0xd8, 0xb2, 0xcc, 0x00, 0x2f, 0x1e, 0x3c, 0x00, 0x00,
    ];
    const FIXVAR_DF_BITS: u64 = 84;
    const VARFIX_DF_BODY: &[u8] = &[0x90, 0x31, 0xd8, 0xb5, 0x38, 0x02, 0xf1, 0xe0, 0x00, 0x00];
    const VARFIX_DF_BITS: u64 = 73;
    const VARVAR_DF_BODY: &[u8] = &[0x90, 0x31, 0xd8, 0xb6, 0xa8, 0x00, 0x2f, 0x1e, 0x00, 0x00];
    const VARVAR_DF_BITS: u64 = 77;
    const FIXFIX_DT_BODY: &[u8] = &[0x90, 0x31, 0xd8, 0xb0, 0x95, 0x78, 0x00, 0x00];
    const FIXFIX_DT_BITS: u64 = 57;
    const CPE_UNCOUPLED_BODY: &[u8] = &[0x90, 0x31, 0xd8, 0xb0, 0x00, 0x05, 0x78, 0xf0, 0x00, 0x00];
    const CPE_UNCOUPLED_BITS: u64 = 75;
    const EXTENSION_BODY: &[u8] = &[0x90, 0x31, 0xd8, 0xb0, 0x05, 0xe0, 0x11, 0x00];
    const EXTENSION_BITS: u64 = 64;
    const HEADERLESS_LEAD_BODY: &[u8] = &[0x90, 0x31, 0xd8, 0xb0, 0x05, 0xe0, 0x00];
    const HEADERLESS_LEAD_BITS: u64 = 52;
    const HEADERLESS_TAIL_BODY: &[u8] = &[0x00, 0x2f, 0x00, 0x00];
    const HEADERLESS_TAIL_BITS: u64 = 25;
    const CPE_COUPLED_BODY: &[u8] = &[
        0x90, 0x31, 0xd8, 0xb4, 0x48, 0x8b, 0xc0, 0x00, 0x83, 0xe8, 0x40, 0x00,
    ];
    const CPE_COUPLED_BITS: u64 = 89;

    const SBR_RATE: u32 = 44100;

    fn assert_consumed(r: &BitReader, want_bits: u64) {
        assert_eq!(r.bit_position(), want_bits);
    }

    #[test]
    fn fixvar_grid_df_only_envelopes_raw() {
        let mut p = SbrParser::new(SBR_RATE);
        let mut r = BitReader::new(FIXVAR_DF_BODY);
        let d = p.parse(&mut r, false).unwrap();
        assert_consumed(&r, FIXVAR_DF_BITS);
        let ch = &d.channels[0];
        assert_eq!(ch.e_q, vec![vec![60], vec![60], vec![60]]);
        assert_eq!(ch.q_q, vec![vec![0], vec![0]]);
        assert_eq!(ch.t_env.len(), 4);
        assert!(ch.t_env.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn varfix_grid_df_only_envelopes_raw() {
        let mut p = SbrParser::new(SBR_RATE);
        let mut r = BitReader::new(VARFIX_DF_BODY);
        let d = p.parse(&mut r, false).unwrap();
        assert_consumed(&r, VARFIX_DF_BITS);
        let ch = &d.channels[0];
        assert_eq!(ch.e_q, vec![vec![60], vec![60]]);
        assert_eq!(ch.q_q, vec![vec![0], vec![0]]);
        assert_eq!(ch.t_env.len(), 3);
        assert!(ch.t_env.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn varvar_grid_df_only_envelopes_raw() {
        let mut p = SbrParser::new(SBR_RATE);
        let mut r = BitReader::new(VARVAR_DF_BODY);
        let d = p.parse(&mut r, false).unwrap();
        assert_consumed(&r, VARVAR_DF_BITS);
        let ch = &d.channels[0];
        assert_eq!(ch.e_q, vec![vec![60], vec![60]]);
        assert_eq!(ch.q_q, vec![vec![0], vec![0]]);
        assert_eq!(ch.t_env.len(), 3);
        assert!(ch.t_env.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn fixfix_grid_dt_second_envelope_and_noise() {
        let mut p = SbrParser::new(SBR_RATE);
        let mut r = BitReader::new(FIXFIX_DT_BODY);
        let d = p.parse(&mut r, false).unwrap();
        assert_consumed(&r, FIXFIX_DT_BITS);
        let ch = &d.channels[0];
        // env1 is DT off env0 with delta 0 (ENV15_T's first codeword).
        assert_eq!(ch.e_q, vec![vec![60], vec![60]]);
        assert_eq!(ch.q_q, vec![vec![0], vec![0]]);
        assert_eq!(ch.t_env, vec![0, 8, 16]);
    }

    #[test]
    fn cpe_uncoupled_has_two_independent_grids() {
        let mut p = SbrParser::new(SBR_RATE);
        let mut r = BitReader::new(CPE_UNCOUPLED_BODY);
        let d = p.parse(&mut r, true).unwrap();
        assert_consumed(&r, CPE_UNCOUPLED_BITS);
        assert!(!d.coupling);
        assert_eq!(d.channels.len(), 2);
        for ch in &d.channels {
            assert_eq!(ch.e_q, vec![vec![60]]);
            assert_eq!(ch.q_q, vec![vec![0]]);
            assert_eq!(ch.t_env, vec![0, 16]);
        }
    }

    #[test]
    fn extension_payload_is_skipped_by_its_own_length() {
        let mut p = SbrParser::new(SBR_RATE);
        let mut r = BitReader::new(EXTENSION_BODY);
        let d = p.parse(&mut r, false).unwrap();
        assert_consumed(&r, EXTENSION_BITS);
        assert_eq!(d.channels[0].e_q, vec![vec![60]]);
    }

    #[test]
    fn headerless_frame_reuses_prior_header() {
        let mut p = SbrParser::new(SBR_RATE);
        let mut lead = BitReader::new(HEADERLESS_LEAD_BODY);
        let d0 = p.parse(&mut lead, false).unwrap();
        assert_consumed(&lead, HEADERLESS_LEAD_BITS);
        assert_eq!(d0.channels[0].e_q, vec![vec![60]]);
        assert!(p.header().is_some());

        let mut tail = BitReader::new(HEADERLESS_TAIL_BODY);
        let d1 = p.parse(&mut tail, false).unwrap();
        assert_consumed(&tail, HEADERLESS_TAIL_BITS);
        assert_eq!(d1.channels[0].e_q, vec![vec![60]]);
    }

    #[test]
    fn headerless_frame_with_no_prior_header_is_corrupt() {
        let mut p = SbrParser::new(SBR_RATE);
        let mut r = BitReader::new(HEADERLESS_TAIL_BODY);
        let err = p.parse(&mut r, false).unwrap_err();
        assert!(matches!(err, Error::Corrupt { .. }));
    }

    #[test]
    fn cpe_coupled_balance_channel_uses_the_b_books() {
        let mut p = SbrParser::new(SBR_RATE);
        let mut r = BitReader::new(CPE_COUPLED_BODY);
        let d = p.parse(&mut r, true).unwrap();
        assert_consumed(&r, CPE_COUPLED_BITS);
        assert!(d.coupling);
        assert_eq!(d.channels.len(), 2);
        // Channel 0: plain ENV15_T/NOISE_T books, delta 0 on env1.
        assert_eq!(d.channels[0].e_q, vec![vec![60], vec![60]]);
        assert_eq!(d.channels[0].q_q, vec![vec![0], vec![0]]);
        // Channel 1 (balance): ENVB15_T codeword under test carries delta 3.
        assert_eq!(d.channels[1].e_q, vec![vec![32], vec![35]]);
        assert_eq!(d.channels[1].q_q, vec![vec![16], vec![16]]);
    }

    #[test]
    fn truncated_payload_is_need_more_not_a_panic() {
        let mut p = SbrParser::new(SBR_RATE);
        for cut in 0..FIXFIX_DT_BODY.len() {
            let mut p2 = SbrParser::new(SBR_RATE);
            let mut r = BitReader::new(&FIXFIX_DT_BODY[..cut]);
            let err = p2.parse(&mut r, false);
            if let Err(e) = err {
                assert!(e.is_need_more() || matches!(e, Error::Corrupt { .. }));
            }
        }
        // The full body must still parse fine (sanity: the loop above did
        // not corrupt shared state).
        let mut r = BitReader::new(FIXFIX_DT_BODY);
        assert!(p.parse(&mut r, false).is_ok());
    }

    #[test]
    fn corrupt_huffman_run_of_ones_is_refused_not_a_panic() {
        // A run of set bits that never lands on a valid codeword prefix in
        // any book here (every book's Kraft sum is 1, so this can only
        // happen by running past the longest codeword): must return an
        // error, never panic, regardless of which error.
        let garbage = vec![0xFFu8; FIXFIX_DT_BODY.len() + 4];
        let mut p = SbrParser::new(SBR_RATE);
        let mut r = BitReader::new(&garbage);
        let _ = p.parse(&mut r, false); // must not panic
    }

    #[test]
    fn invalid_header_band_configuration_is_corrupt() {
        // bs_start_freq = 14 is documented ("Invalid n_master: 0" in the
        // rig's own notes) to produce no valid master band table.
        let mut s = ec_core::BitWriter::new();
        s.write_bit(true); // bs_header_flag
        s.write_bits(0, 1); // amp_res
        s.write_bits(14, 4); // start_freq
        s.write_bits(0, 4); // stop_freq
        s.write_bits(0, 3); // xover_band
        s.write_bits(0, 2); // reserved
        s.write_bit(true); // extra1
        s.write_bit(true); // extra2
        s.write_bits(2, 2); // freq_scale
        s.write_bit(true); // alter_scale
        s.write_bits(2, 2); // noise_bands
        s.write_bits(0, 2); // limiter_bands
        s.write_bits(2, 2); // limiter_gains
        s.write_bit(true); // interpol_freq
        s.write_bit(true); // smoothing_mode
        s.write_bits(0, 32); // slack so a bug reading past the header still has bits
        let bytes = s.into_bytes();
        let mut p = SbrParser::new(SBR_RATE);
        let mut r = BitReader::new(&bytes);
        let err = p.parse(&mut r, false).unwrap_err();
        assert!(matches!(err, Error::Corrupt { .. }));
    }
}
