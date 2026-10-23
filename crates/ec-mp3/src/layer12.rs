//! Layers I and II (MP1/MP2): bit allocation, scalefactors, requantisation and
//! the same 32-band polyphase synthesis Layer III uses.
//!
//! Layer II's allocation tables (ISO/IEC 11172-3 Tables 3-B.2a..d and the
//! ISO/IEC 13818-3 low-sampling-frequency table) are normative data, so like
//! the Layer III tables they were *measured* from an oracle decoder rather than
//! copied: `scripts/mp2-tables/measure.py` sets one allocation code at a time and
//! reads the quantiser size back from the decoded level.

use crate::header::{ChannelMode, FrameHeader, Version, crc16_bits};
use crate::window::WINDOW;
use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};

// Quantiser sizes (number of levels) by allocation code; 0 means "not coded".
const Q_A: [u32; 16] = [
    0, 3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767, 65535,
];
const Q_B: [u32; 16] = [
    0, 3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 65535,
];
const Q_C: [u32; 8] = [0, 3, 5, 7, 9, 15, 31, 65535];
const Q_D: [u32; 4] = [0, 3, 5, 65535];
const Q_E: [u32; 16] = [
    0, 3, 5, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767,
];
const Q_F: [u32; 8] = [0, 3, 5, 9, 15, 31, 63, 127];
const Q_G: [u32; 16] = [
    0, 3, 5, 7, 9, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383,
];
const Q_H: [u32; 4] = [0, 3, 5, 9];

/// One subband's allocation: how many bits the code takes, and what each code means.
type Row = &'static [u32];

/// The five Layer II allocation tables as `(row, subband count)` runs.
const TABLES: [&[(&[u32], usize)]; 5] = [
    // 3-B.2a: 27 bands.
    &[(&Q_A, 3), (&Q_B, 8), (&Q_C, 12), (&Q_D, 4)],
    // 3-B.2b: 30 bands.
    &[(&Q_A, 3), (&Q_B, 8), (&Q_C, 12), (&Q_D, 7)],
    // 3-B.2c: 8 bands.
    &[(&Q_E, 2), (&Q_F, 6)],
    // 3-B.2d: 12 bands.
    &[(&Q_E, 2), (&Q_F, 10)],
    // 13818-3 LSF: 30 bands.
    &[(&Q_G, 4), (&Q_F, 7), (&Q_H, 19)],
];

/// Which table a Layer II stream uses (ISO/IEC 11172-3 2.4.2.3).
fn select_table(header: &FrameHeader) -> usize {
    if header.version != Version::Mpeg1 {
        return 4;
    }
    let ch_bitrate = header.bitrate_kbps / header.channels() as u32;
    let rate = header.sample_rate;
    if (rate == 48000 && ch_bitrate >= 56) || (56..=80).contains(&ch_bitrate) {
        0
    } else if rate != 48000 && ch_bitrate >= 96 {
        1
    } else if rate != 32000 && ch_bitrate <= 48 {
        2
    } else {
        3
    }
}

/// Scalefactor multipliers, `2^(1 - i/3)`.
fn scalefactor(index: u32) -> f32 {
    (2.0f64).powf(1.0 - f64::from(index) / 3.0) as f32
}

/// Bits one sample takes for a quantiser of `levels` levels.
fn sample_bits(levels: u32) -> u32 {
    match levels {
        3 => 2,
        5 => 3,
        9 => 4,
        n => 32 - n.leading_zeros(),
    }
}

/// `(2v - L + 1) / L`: the requantised value of code `v` out of `L` levels,
/// before the scalefactor. (The standard's `C * (s + D)` collapses to this.)
fn level(v: u32, levels: u32) -> f32 {
    ((2.0 * f64::from(v) - f64::from(levels) + 1.0) / f64::from(levels)) as f32
}

/// The stateful half of Layer I/II decoding: just the synthesis history.
#[derive(Debug, Default)]
pub(crate) struct Layer12 {
    pub(crate) synthesis: [Synth; 2],
}

/// Matrixing cosines `cos(pi m (2k+1) / 64)` and the synthesis window `D[i]`.
struct Tables {
    cos: [[f64; 32]; 32],
    window: [f64; 512],
}

/// The standard's `D[i]` are integers over 65536 (ISO/IEC 11172-3 Table 3-B.3
/// lists them to nine decimals) and the window measured for Layer III is within
/// 2e-6 of them, well inside the 7.6e-6 half-step, so rounding recovers the exact
/// table. Layers I/II run the whole bank in f64 with it, which leaves the
/// result limited by the f32 output alone.
fn tables() -> &'static Tables {
    static T: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = Tables {
            cos: [[0.0; 32]; 32],
            window: [0.0; 512],
        };
        for (m, row) in t.cos.iter_mut().enumerate() {
            for (k, c) in row.iter_mut().enumerate() {
                *c = (std::f64::consts::PI * (m * (2 * k + 1)) as f64 / 64.0).cos();
            }
        }
        for (o, w) in t.window.iter_mut().zip(WINDOW.iter()) {
            *o = (f64::from(*w) * 65536.0).round() / 65536.0;
        }
        t
    })
}

/// One channel's polyphase synthesis: 32 subband samples in, 32 PCM out.
#[derive(Debug)]
pub(crate) struct Synth {
    blocks: Vec<[f64; 64]>,
    pos: usize,
}

impl Default for Synth {
    fn default() -> Synth {
        Synth {
            blocks: vec![[0.0; 64]; 16],
            pos: 0,
        }
    }
}

impl Synth {
    fn reset(&mut self) {
        self.blocks.iter_mut().for_each(|b| b.fill(0.0));
        self.pos = 0;
    }

    fn slot(&mut self, subband: &[f32; 32], out: &mut [f32]) {
        let t = tables();
        let mut g = [0.0f64; 32];
        for (m, g) in g.iter_mut().enumerate() {
            *g = subband
                .iter()
                .zip(&t.cos[m])
                .map(|(s, c)| f64::from(*s) * c)
                .sum();
        }
        self.pos = (self.pos + 15) % 16;
        let v = &mut self.blocks[self.pos];
        for (b, slot) in v.iter_mut().enumerate() {
            *slot = match b {
                0..=15 => g[b + 16],
                16 => 0.0,
                17..=47 => -g[48 - b],
                _ => -g[b - 48],
            };
        }
        for (j, sample) in out.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for i in 0..16 {
                let b = if i % 2 == 0 { j } else { j + 32 };
                acc += t.window[j + 32 * i] * self.blocks[(self.pos + i) % 16][b];
            }
            *sample = acc as f32;
        }
    }
}

impl Layer12 {
    pub(crate) fn reset(&mut self) {
        for s in &mut self.synthesis {
            s.reset();
        }
    }

    /// Decodes one frame of Layer I or II; `frame` is the whole frame, header included.
    pub(crate) fn decode(&mut self, header: &FrameHeader, frame: &[u8]) -> Result<Vec<f32>> {
        let channels = header.channels();
        let body = &frame[4 + usize::from(header.crc) * 2..];
        let mut r = BitReader::new(body);
        let mut subband = vec![[[0.0f32; 32]; 2]; 36];
        let slots = if header.layer == 1 {
            self.read_layer1(header, frame, &mut r, &mut subband)?;
            12
        } else {
            self.read_layer2(header, frame, &mut r, &mut subband)?;
            36
        };
        let mut out = vec![0.0f32; slots * 32 * channels];
        let mut pcm = [0.0f32; 32];
        for (t, slot) in subband.iter().take(slots).enumerate() {
            for ch in 0..channels {
                self.synthesis[ch].slot(&slot[ch], &mut pcm);
                for (j, sample) in pcm.iter().enumerate() {
                    out[(t * 32 + j) * channels + ch] = *sample;
                }
            }
        }
        Ok(out)
    }

    fn bound(header: &FrameHeader, limit: usize) -> usize {
        if header.mode == ChannelMode::JointStereo {
            (4 + 4 * usize::from(header.mode_ext)).min(limit)
        } else {
            limit
        }
    }

    fn check_crc(header: &FrameHeader, frame: &[u8], body: &[u8], bits: u64) -> Result<()> {
        if !header.crc {
            return Ok(());
        }
        let stored = u16::from_be_bytes([frame[4], frame[5]]);
        let computed = crc16_bits(
            &[frame[0], frame[1], frame[2], frame[3]],
            body,
            bits as usize,
        );
        if stored != computed {
            return Err(Error::corrupt(format!(
                "mp2: allocation CRC {computed:#06x} != {stored:#06x}"
            )));
        }
        Ok(())
    }

    fn read_layer1(
        &mut self,
        header: &FrameHeader,
        frame: &[u8],
        r: &mut BitReader<'_>,
        out: &mut [[[f32; 32]; 2]],
    ) -> Result<()> {
        let channels = header.channels();
        let bound = Self::bound(header, 32);
        let mut alloc = [[0u32; 32]; 2];
        for sb in 0..32 {
            if sb < bound {
                for a in alloc.iter_mut().take(channels) {
                    a[sb] = r.read_bits(4)?;
                }
            } else {
                let v = r.read_bits(4)?;
                alloc[0][sb] = v;
                alloc[1][sb] = v;
            }
        }
        Self::check_crc(header, frame, &frame[4 + 2..], r.bit_position())?;
        if alloc.iter().take(channels).flatten().any(|&a| a == 15) {
            return Err(Error::corrupt("mp1: forbidden allocation code 15"));
        }
        let mut sf = [[0.0f32; 32]; 2];
        for sb in 0..32 {
            for ch in 0..channels {
                if alloc[ch][sb] != 0 {
                    sf[ch][sb] = scalefactor(r.read_bits(6)?);
                }
            }
        }
        for slot in out.iter_mut().take(12) {
            for sb in 0..32 {
                if sb < bound {
                    for ch in 0..channels {
                        let a = alloc[ch][sb];
                        if a != 0 {
                            let nb = a + 1;
                            let v = r.read_bits(nb)?;
                            slot[ch][sb] = level(v, (1 << nb) - 1) * sf[ch][sb];
                        }
                    }
                } else {
                    let a = alloc[0][sb];
                    if a != 0 {
                        let nb = a + 1;
                        let v = r.read_bits(nb)?;
                        let x = level(v, (1 << nb) - 1);
                        for ch in 0..channels {
                            slot[ch][sb] = x * sf[ch][sb];
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn read_layer2(
        &mut self,
        header: &FrameHeader,
        frame: &[u8],
        r: &mut BitReader<'_>,
        out: &mut [[[f32; 32]; 2]],
    ) -> Result<()> {
        let channels = header.channels();
        let table = TABLES[select_table(header)];
        let mut rows: Vec<Row> = Vec::with_capacity(30);
        for &(row, count) in table {
            rows.extend(std::iter::repeat_n(row, count));
        }
        let sblimit = rows.len();
        let bound = Self::bound(header, sblimit);
        // Quantiser levels per band and channel.
        let mut levels = [[0u32; 30]; 2];
        for sb in 0..sblimit {
            let bits = rows[sb].len().trailing_zeros();
            if sb < bound {
                for l in levels.iter_mut().take(channels) {
                    l[sb] = rows[sb][r.read_bits(bits)? as usize];
                }
            } else {
                let v = rows[sb][r.read_bits(bits)? as usize];
                levels[0][sb] = v;
                levels[1][sb] = v;
            }
        }
        let mut scfsi = [[0u32; 30]; 2];
        for sb in 0..sblimit {
            for ch in 0..channels {
                if levels[ch][sb] != 0 {
                    scfsi[ch][sb] = r.read_bits(2)?;
                }
            }
        }
        Self::check_crc(header, frame, &frame[4 + 2..], r.bit_position())?;
        let mut sf = [[[0.0f32; 3]; 30]; 2];
        for sb in 0..sblimit {
            for ch in 0..channels {
                if levels[ch][sb] == 0 {
                    continue;
                }
                let mut idx = [0u32; 3];
                match scfsi[ch][sb] {
                    0 => {
                        for v in &mut idx {
                            *v = r.read_bits(6)?;
                        }
                    }
                    1 => {
                        idx[0] = r.read_bits(6)?;
                        idx[1] = idx[0];
                        idx[2] = r.read_bits(6)?;
                    }
                    2 => {
                        idx = [r.read_bits(6)?; 3];
                    }
                    _ => {
                        idx[0] = r.read_bits(6)?;
                        idx[1] = r.read_bits(6)?;
                        idx[2] = idx[1];
                    }
                }
                for (part, i) in idx.iter().enumerate() {
                    sf[ch][sb][part] = scalefactor(*i);
                }
            }
        }
        for gr in 0..12 {
            let part = gr / 4;
            let mut vals = [[[0.0f32; 3]; 30]; 2];
            for sb in 0..sblimit {
                if sb < bound {
                    for ch in 0..channels {
                        vals[ch][sb] = read_samples(r, levels[ch][sb])?;
                    }
                } else {
                    let v = read_samples(r, levels[0][sb])?;
                    vals[0][sb] = v;
                    vals[1][sb] = v;
                }
            }
            for s in 0..3 {
                for sb in 0..sblimit {
                    for ch in 0..channels {
                        out[gr * 3 + s][ch][sb] = vals[ch][sb][s] * sf[ch][sb][part];
                    }
                }
            }
        }
        Ok(())
    }
}

/// Three consecutive samples of one band: grouped into a single code for the
/// 3-, 5- and 9-level quantisers, otherwise one code each.
fn read_samples(r: &mut BitReader<'_>, levels: u32) -> Result<[f32; 3]> {
    let mut out = [0.0f32; 3];
    if levels == 0 {
        return Ok(out);
    }
    if matches!(levels, 3 | 5 | 9) {
        let bits = match levels {
            3 => 5,
            5 => 7,
            _ => 10,
        };
        let mut code = r.read_bits(bits)?;
        for o in &mut out {
            *o = level(code % levels, levels);
            code /= levels;
        }
    } else {
        let bits = sample_bits(levels);
        for o in &mut out {
            *o = level(r.read_bits(bits)?, levels);
        }
    }
    Ok(out)
}
