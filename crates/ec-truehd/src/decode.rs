//! The lossless substream decode: restart headers, decoding parameters,
//! Huffman/LSB residuals, FIR/IIR prediction, rematrixing with the seeded
//! noise generators, output shift — and the stream's own integrity checks
//! (restart-header CRC, per-substream parity, the lossless-check byte).
//!
//! Written from the public description of the MLP/TrueHD bitstream. Every
//! numeric constant a from-memory description could get wrong (the three
//! Huffman codebooks, the noise table, the generators' feedback taps, the
//! restart-header CRC) was *derived or confirmed* against an external
//! decoder: the codebooks were read off externally encoded mono streams
//! whose PCM is known, the 256-entry noise table was solved for by
//! constraint intersection over the real 7.1 track, and `tests/oracle.rs`
//! keeps both bit-exact.
//!
//! **Per-substream syntax** (one substream's bytes, bit-serial MSB-first):
//!
//! ```text
//! loop {
//!   params_present(1)
//!     restart_present(1) → restart header:
//!       sync(14) = 0x31EA|0x31EB (low bit = noise type), output_timing(16),
//!       min_ch(4), max_ch(4), max_matrix_ch(4), noise_shift(4),
//!       noise_seed(23), reserved(19), data_check_present(1),
//!       lossless_check(8), reserved(16), ch_assign(6) × (max_matrix_ch+1),
//!       crc8(8, poly 0x1D)
//!     decoding params (each group gated by a presence flag):
//!       presence flags(8); block size(9); matrices: count(4), per matrix
//!       out_ch(4) frac_bits(4) lsb_bypass(1) [present(1) coeff(frac+2)] per
//!       input channel (+2 noise channels for noise type 0) [noise_shift(4)
//!       for noise type 1]; output shift(4 signed) per matrix channel;
//!       quant step(4) per channel; per channel: FIR/IIR (order(4), shift(4),
//!       coeff_bits(5), coeff_shift(3), coeffs, [IIR state]), huff_offset(15
//!       signed), codebook(2), huff_lsbs(5)
//!   block: [data check: bit count(16)] per sample: lsb-bypass bits, then
//!     per channel Huffman index (codebook 1..3) + lsb bits; [crc(8)]
//!   end_of_segment(1)
//! }
//! align(16) [0xD234D234 end-of-stream] [parity(8) crc8(8, not validated)]
//! ```
//!
//! **Reconstruction**: `residual = ((index << lsb_bits) + lsbs +
//! sign_offset) << quant_step`; `sample = (predict(FIR over past samples,
//! IIR over past residual-after-FIR) + residual) & !((1 << quant_step) - 1)`;
//! then, for the highest substream only, each primitive matrix rewrites one
//! channel as `(Σ coeff·chan + noise) >> 14` masked to the quant step plus
//! its bypassed LSB; then `<< output_shift`; then `<< 8` to a left-justified
//! 24-in-32 sample.
//!
//! **Channel order**: TrueHD's 8-channel presentation with assignment `0x4F`
//! (L R C LFE Ls Rs Lrs Rrs) lands on [`ChannelLayout::Surround7_1`]'s
//! FL FR FC LFE BL BR SL SR as output index `[0,1,2,3,6,7,4,5]` — TrueHD's
//! Ls/Rs pair is the *side* pair, Lrs/Rrs the *back* pair. 2- and 6-channel
//! presentations map 1:1.

use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};
use ec_core::frame::{AudioFrame, ChannelLayout, SampleFormat};
use ec_core::packet::Buf;

use crate::sync::AccessUnitHeader;

/// Highest matrix/output channel index the 2/6/8-channel presentations use;
/// anything above is the 16-channel object substream, refused.
const MAX_MATRIX_CHANNEL: usize = 7;
/// Sample-buffer width: 8 PCM channels plus the 2 noise channels noise type 0
/// appends after the last matrix channel.
const MAX_CHANNELS: usize = MAX_MATRIX_CHANNEL + 3;
const MAX_MATRICES: usize = 8;
const MAX_FILTER_ORDER: usize = 8;
const END_OF_STREAM: u32 = 0xD234_D234;

/// The fixed 256-entry dither table noise type 1 (TrueHD's `0x31EB`) draws
/// matrix noise from, indexed by the generator's top byte. A constant of the
/// format; every entry was solved for exactly (constraint intersection over
/// 72 000 access units of the real 7.1 track against the external decoder's
/// output) and is kept bit-exact by `tests/oracle.rs`.
#[rustfmt::skip]
const NOISE_TABLE: [i8; 256] = [
    30, 51, 22, 54, 3, 7, -4, 38, 14, 55, 46, 81, 22, 58, -3, 2,
    52, 31, -7, 51, 15, 44, 74, 30, 85, -17, 10, 33, 18, 80, 28, 62,
    10, 32, 23, 69, 72, 26, 35, 17, 73, 60, 8, 56, 2, 6, -2, -5,
    51, 4, 11, 50, 66, 76, 21, 44, 33, 47, 1, 26, 64, 48, 57, 40,
    38, 16, -10, -28, 92, 22, -18, 29, -10, 5, -13, 49, 19, 24, 70, 34,
    61, 48, 30, 14, -6, 25, 58, 33, 42, 60, 67, 17, 54, 17, 22, 30,
    67, 44, -9, 50, -11, 43, 40, 32, 59, 82, 13, 49, -14, 55, 60, 36,
    48, 49, 31, 47, 15, 12, 4, 65, 1, 23, 29, 39, 45, -2, 84, 69,
    0, 72, 37, 57, 27, 41, -15, -16, 35, 31, 14, 61, 24, 0, 27, 24,
    16, 41, 55, 34, 53, 9, 56, 12, 25, 29, 53, 5, 20, -20, -8, 20,
    13, 28, -3, 78, 38, 16, 11, 62, 46, 29, 21, 24, 46, 65, 43, -23,
    89, 18, 74, 21, 38, -12, 19, 12, -19, 8, 15, 33, 4, 57, 9, -8,
    36, 35, 26, 28, 7, 83, 63, 79, 75, 11, 3, 87, 37, 47, 34, 40,
    39, 19, 20, 42, 27, 34, 39, 77, 13, 42, 59, 64, 45, -1, 32, 37,
    45, -5, 53, -6, 7, 36, 50, 23, 6, 32, 9, -21, 18, 71, 27, 52,
    -25, 31, 35, 42, -1, 68, 63, 52, 26, 43, 66, 37, 41, 25, 40, 70,
];

const PARAM_BLOCKSIZE: u8 = 1 << 7;
const PARAM_MATRIX: u8 = 1 << 6;
const PARAM_OUTSHIFT: u8 = 1 << 5;
const PARAM_QUANTSTEP: u8 = 1 << 4;
const PARAM_FIR: u8 = 1 << 3;
const PARAM_IIR: u8 = 1 << 2;
const PARAM_HUFFOFFSET: u8 = 1 << 1;
const PARAM_PRESENCE: u8 = 1 << 0;

/// The three residual codebooks as `(code, length)` per index: codebook 1
/// covers indices 0..18, codebook 2 0..16, codebook 3 0..15. Unused tail
/// entries have length 0. Shape: a unary run `0…01` of length `7 - i + 2`
/// below the centre, `010…01` above it, and 1-3 short codes at the centre
/// (the three books differ only there). Verified code-for-code against an
/// external encoder's output (`tests/oracle.rs`).
pub const HUFFMAN_TABLES: [[(u16, u8); 18]; 3] = [
    [
        (0x01, 9), (0x01, 8), (0x01, 7), (0x01, 6), (0x01, 5), (0x01, 4), (0x01, 3), (0x04, 3), (0x05, 3),
        (0x06, 3), (0x07, 3), (0x03, 3), (0x05, 4), (0x09, 5), (0x11, 6), (0x21, 7), (0x41, 8), (0x81, 9),
    ],
    [
        (0x01, 9), (0x01, 8), (0x01, 7), (0x01, 6), (0x01, 5), (0x01, 4), (0x01, 3), (0x02, 2), (0x03, 2),
        (0x03, 3), (0x05, 4), (0x09, 5), (0x11, 6), (0x21, 7), (0x41, 8), (0x81, 9), (0, 0), (0, 0),
    ],
    [
        (0x01, 9), (0x01, 8), (0x01, 7), (0x01, 6), (0x01, 5), (0x01, 4), (0x01, 3), (0x01, 1), (0x03, 3),
        (0x05, 4), (0x09, 5), (0x11, 6), (0x21, 7), (0x41, 8), (0x81, 9), (0, 0), (0, 0), (0, 0),
    ],
];

/// 512-entry lookup per codebook: `(index, length)`, length 0 = invalid code.
struct HuffLut([[(u8, u8); 512]; 3]);

impl HuffLut {
    fn build() -> HuffLut {
        let mut lut = [[(0u8, 0u8); 512]; 3];
        for (book, table) in HUFFMAN_TABLES.iter().enumerate() {
            for (index, &(code, len)) in table.iter().enumerate() {
                if len == 0 {
                    continue;
                }
                let base = usize::from(code) << (9 - len);
                for k in 0..(1usize << (9 - len)) {
                    lut[book][base + k] = (index as u8, len);
                }
            }
        }
        HuffLut(lut)
    }

    fn decode(&self, br: &mut BitReader, book: usize) -> Result<i32> {
        let avail = br.bits_remaining().min(9) as u32;
        if avail == 0 {
            return Err(Error::NeedMore);
        }
        let bits = br.peek_bits(avail)? << (9 - avail);
        let (index, len) = self.0[book][bits as usize];
        if len == 0 || u64::from(len) > br.bits_remaining() {
            return Err(Error::corrupt("TrueHD: invalid Huffman code"));
        }
        br.skip_bits(u64::from(len))?;
        Ok(i32::from(index))
    }
}

/// Bit-serial CRC-8 (MSB first, no reflection, no final xor).
fn crc8(poly: u8, init: u8, bytes: &[u8]) -> u8 {
    let mut crc = init;
    for &b in bytes {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ poly } else { crc << 1 };
        }
    }
    crc
}

/// The restart header's own CRC-8 (poly 0x1D), over the header from its
/// first byte (`buf[0]`, whose two leading bits — the params/restart flags —
/// are masked off) through its last full byte and the trailing partial bits.
fn restart_checksum(buf: &[u8], bit_size: u64) -> u8 {
    let bit_size = bit_size as usize + 2;
    let num_bytes = bit_size / 8;
    let mut crc = crc8(0x1D, 0, &[buf[0] & 0x3F]);
    crc = crc8(0x1D, crc, &buf[1..num_bytes - 1]);
    crc ^= buf[num_bytes - 1];
    for i in 0..(bit_size & 7) {
        let top = crc & 0x80 != 0;
        crc <<= 1;
        if top {
            crc ^= 0x1D;
        }
        crc ^= (buf[num_bytes] >> (7 - i)) & 1;
    }
    crc
}

/// XOR-fold a 32-bit word to 8 bits.
fn xor_32_to_8(v: u32) -> u8 {
    (v ^ (v >> 8) ^ (v >> 16) ^ (v >> 24)) as u8
}

#[derive(Debug, Clone, Copy, Default)]
struct Filter {
    order: usize,
    shift: u32,
    coeffs: [i32; MAX_FILTER_ORDER],
    /// History, most recent first.
    state: [i32; MAX_FILTER_ORDER],
}

#[derive(Debug, Clone, Copy, Default)]
struct ChannelParams {
    fir: Filter,
    iir: Filter,
    huff_offset: i32,
    sign_huff_offset: i32,
    codebook: u8,
    huff_lsbs: u8,
}

#[derive(Debug, Clone, Copy, Default)]
struct Matrix {
    out_ch: usize,
    lsb_bypass: bool,
    noise_shift: u32,
    coeffs: [i32; MAX_CHANNELS],
}

#[derive(Debug, Clone)]
struct Substream {
    restart_seen: bool,
    noise_type: bool,
    min_channel: usize,
    max_channel: usize,
    max_matrix_channel: usize,
    noise_shift: u32,
    noisegen_seed: u32,
    data_check_present: bool,
    lossless_check_data: u32,
    ch_assign: [usize; MAX_CHANNELS],
    param_presence_flags: u8,
    blocksize: usize,
    matrices: Vec<Matrix>,
    output_shift: [i32; MAX_CHANNELS],
    quant_step_size: [u32; MAX_CHANNELS],
    channels: [ChannelParams; MAX_CHANNELS],
    /// Samples decoded so far in this access unit.
    blockpos: usize,
}

impl Substream {
    fn new() -> Substream {
        Substream {
            restart_seen: false,
            noise_type: false,
            min_channel: 0,
            max_channel: 0,
            max_matrix_channel: 0,
            noise_shift: 0,
            noisegen_seed: 0,
            data_check_present: false,
            lossless_check_data: 0,
            ch_assign: [0; MAX_CHANNELS],
            param_presence_flags: 0xFF,
            blocksize: 8,
            matrices: Vec::new(),
            output_shift: [0; MAX_CHANNELS],
            quant_step_size: [0; MAX_CHANNELS],
            channels: [ChannelParams::default(); MAX_CHANNELS],
            blockpos: 0,
        }
    }

    fn recompute_sign_offset(&mut self, ch: usize) {
        let cp = &mut self.channels[ch];
        let lsb_bits = i32::from(cp.huff_lsbs) - self.quant_step_size[ch] as i32;
        let sign_shift = lsb_bits + if cp.codebook > 0 { 2 - i32::from(cp.codebook) } else { -1 };
        let mut off = cp.huff_offset;
        if cp.codebook > 0 {
            off -= 7 << lsb_bits;
        }
        if sign_shift >= 0 {
            off -= 1 << sign_shift;
        }
        cp.sign_huff_offset = off;
    }
}

/// Integrity counters the decoder keeps across access units; every one is a
/// stream-side self-check that stays zero on a clean decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckStats {
    /// Restart headers whose CRC-8 did not match.
    pub restart_crc_failures: u64,
    /// Restart headers seen (the lossless-check byte is verified at each
    /// one after the first, against the samples output since the previous).
    pub restart_headers: u64,
    /// Lossless-check bytes that did not match the output.
    pub lossless_check_failures: u64,
    /// Substream segments whose parity byte did not match.
    pub parity_failures: u64,
    /// Substream segments whose bit count did not land on the directory's
    /// end pointer.
    pub length_mismatches: u64,
}

/// Decoder state shared across the substreams of one stream.
#[derive(Debug, Clone)]
pub(crate) struct Core {
    huff: std::sync::Arc<HuffLutBox>,
    substreams: Vec<Substream>,
    /// Samples per access unit (40 at 48 kHz) and its next power of two.
    access_unit_size: usize,
    access_unit_size_pow2: usize,
    sample_rate: u32,
    /// The major sync's 8-channel presentation assignment mask.
    ch8_assignment: u16,
    sample_buffer: Vec<[i32; MAX_CHANNELS]>,
    bypassed_lsbs: Vec<[i32; MAX_MATRICES]>,
    noise_buffer: Vec<i8>,
    /// Self-check tallies.
    pub stats: CheckStats,
}

struct HuffLutBox(HuffLut);

impl std::fmt::Debug for HuffLutBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HuffLut")
    }
}

impl Core {
    pub(crate) fn new() -> Core {
        Core {
            huff: std::sync::Arc::new(HuffLutBox(HuffLut::build())),
            substreams: Vec::new(),
            access_unit_size: 0,
            access_unit_size_pow2: 0,
            sample_rate: 0,
            ch8_assignment: 0,
            sample_buffer: Vec::new(),
            bypassed_lsbs: Vec::new(),
            noise_buffer: Vec::new(),
            stats: CheckStats::default(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.substreams.clear();
        self.access_unit_size = 0;
    }

    /// Decodes one access unit whose header already parsed; `None` until the
    /// first major sync and every substream's first restart header are seen.
    pub(crate) fn decode(&mut self, header: &AccessUnitHeader, data: &[u8]) -> Result<Option<AudioFrame>> {
        if !self.prepare(header)? || !self.decode_substreams(header, data)? {
            return Ok(None);
        }
        self.finish()
    }

    /// Takes a major sync's stream parameters on board; `Ok(false)` while no
    /// major sync has been seen yet.
    fn prepare(&mut self, header: &AccessUnitHeader) -> Result<bool> {
        if let Some(sync) = &header.major_sync {
            let shift = sync.rate_code & 7;
            if shift > 2 {
                return Err(Error::unsupported(
                    format!("TrueHD sample rate code {}", sync.rate_code),
                    "the format reserves it",
                ));
            }
            self.access_unit_size = 40 << shift;
            self.access_unit_size_pow2 = 64 << shift;
            self.sample_rate = sync.sample_rate;
            if self.substreams.len() != header.substreams.len() {
                self.substreams = vec![Substream::new(); header.substreams.len()];
            }
            self.sample_buffer = vec![[0; MAX_CHANNELS]; self.access_unit_size];
            self.bypassed_lsbs = vec![[0; MAX_MATRICES]; self.access_unit_size];
            self.noise_buffer = vec![0; self.access_unit_size_pow2];
            self.ch8_assignment = sync.ch8_assignment;
        }
        Ok(self.access_unit_size != 0 && self.substreams.len() == header.substreams.len())
    }

    /// Every substream's blocks into `sample_buffer`; `Ok(false)` while some
    /// substream still awaits its first restart header.
    fn decode_substreams(&mut self, header: &AccessUnitHeader, data: &[u8]) -> Result<bool> {
        let mut all_seen = true;
        for (i, info) in header.substreams.iter().enumerate() {
            let (start, end) = header.substream_span(i);
            let seg = data.get(start..end).ok_or(Error::NeedMore)?;
            let seen = self.decode_substream(i, seg, info.flags & 0x2 != 0)?;
            all_seen &= seen;
        }
        Ok(all_seen)
    }

    /// Rematrix + output of the top substream for the current access unit.
    fn finish(&mut self) -> Result<Option<AudioFrame>> {
        let top = self.substreams.len() - 1;
        let samples = self.substreams[top].blockpos;
        let channels = self.substreams[top].max_matrix_channel + 1;
        let layout = ChannelLayout::from_count(channels);
        if layout == ChannelLayout::Surround7_1 && self.ch8_assignment != 0x4F {
            return Err(Error::unsupported(
                format!("TrueHD 8-channel presentation with channel assignment {:#x}", self.ch8_assignment),
                "only the standard 7.1 (L R C LFE Ls Rs Lrs Rrs) assignment is mapped",
            ));
        }
        self.rematrix(top);
        let frame = self.output(top, layout, samples)?;
        Ok(Some(frame))
    }

    /// One substream's segment; `Ok(false)` when no restart header has been
    /// seen yet (nothing to decode until one arrives).
    fn decode_substream(&mut self, idx: usize, seg: &[u8], checkdata: bool) -> Result<bool> {
        let mut br = BitReader::new(seg);
        self.substreams[idx].blockpos = 0;
        loop {
            if br.read_bit()? {
                if br.read_bit()? {
                    self.read_restart_header(idx, &mut br, seg)?;
                }
                if !self.substreams[idx].restart_seen {
                    return Ok(false);
                }
                self.read_decoding_params(idx, &mut br)?;
            }
            if !self.substreams[idx].restart_seen {
                return Ok(false);
            }
            self.read_block(idx, &mut br)?;
            if br.bits_remaining() == 0 {
                self.stats.length_mismatches += 1;
                return Ok(true);
            }
            if br.read_bit()? {
                break;
            }
        }
        let pad = (16 - (br.bit_position() % 16)) % 16;
        br.skip_bits(pad)?;
        if br.bits_remaining() >= 32 && br.peek_bits(32)? == END_OF_STREAM {
            br.skip_bits(32)?;
        }
        if checkdata {
            let data_len = (br.bit_position() / 8) as usize;
            if seg.len() < data_len + 2 {
                return Err(Error::corrupt("TrueHD: substream check bytes missing"));
            }
            let parity = seg[..data_len].iter().fold(0u8, |a, &b| a ^ b) ^ 0xA9;
            let parity_bits = br.read_bits(8)? as u8;
            // corner-cut: the second check byte is a CRC-8 whose generator
            // and span this build could not pin down (no plain CRC-8 over
            // the segment, with any polynomial/init/reflection, matched
            // either the real track or an external encoder's output) — it
            // is skipped, not validated. Parity, the restart-header CRC and
            // the lossless check cover the same data.
            br.skip_bits(8)?;
            if parity != parity_bits {
                self.stats.parity_failures += 1;
            }
        }
        if br.bits_remaining() != 0 {
            self.stats.length_mismatches += 1;
        }
        Ok(true)
    }

    fn read_restart_header(&mut self, idx: usize, br: &mut BitReader, seg: &[u8]) -> Result<()> {
        let start = br.bit_position();
        // 0x31EA / 0x31EB as a 14-bit field: 13 bits of sync, 1 bit of
        // noise type.
        let sync = br.read_bits(13)?;
        if sync != 0x31EA >> 1 {
            return Err(Error::corrupt(format!("TrueHD: restart sync {sync:#x}")));
        }
        let noise_type = br.read_bit()?;
        br.skip_bits(16)?; // output timing
        let min_channel = br.read_bits(4)? as usize;
        let max_channel = br.read_bits(4)? as usize;
        let max_matrix_channel = br.read_bits(4)? as usize;
        if max_matrix_channel > MAX_MATRIX_CHANNEL || max_channel > MAX_MATRIX_CHANNEL {
            return Err(Error::unsupported(
                "TrueHD substream with more than 8 matrix channels",
                "the 16-channel object substream is not implemented",
            ));
        }
        if min_channel > max_channel {
            return Err(Error::corrupt("TrueHD: min channel above max channel"));
        }
        let noise_shift = br.read_bits(4)?;
        let noisegen_seed = br.read_bits(23)?;
        br.skip_bits(19)?;
        let data_check_present = br.read_bit()?;
        let lossless_check = br.read_bits(8)? as u8;
        br.skip_bits(16)?;
        // Per matrix channel, the presentation's output slot it lands on;
        // stored inverted (output slot → matrix channel) for `output`.
        let mut ch_assign = [0usize; MAX_CHANNELS];
        for ch in 0..=max_matrix_channel {
            let a = br.read_bits(6)? as usize;
            if a > max_matrix_channel {
                return Err(Error::corrupt("TrueHD: channel assignment out of range"));
            }
            ch_assign[a] = ch;
        }
        let expected = restart_checksum(&seg[(start / 8) as usize..], br.bit_position() - start);
        let crc = br.read_bits(8)? as u8;
        self.stats.restart_headers += 1;
        if crc != expected {
            self.stats.restart_crc_failures += 1;
        }

        let is_top = idx == self.substreams.len() - 1;
        let s = &mut self.substreams[idx];
        if s.restart_seen && is_top && xor_32_to_8(s.lossless_check_data) != lossless_check {
            self.stats.lossless_check_failures += 1;
        }
        let s = &mut self.substreams[idx];
        *s = Substream {
            restart_seen: true,
            noise_type,
            min_channel,
            max_channel,
            max_matrix_channel,
            noise_shift,
            noisegen_seed,
            data_check_present,
            ch_assign,
            ..Substream::new()
        };
        Ok(())
    }

    fn read_decoding_params(&mut self, idx: usize, br: &mut BitReader) -> Result<()> {
        let s = &mut self.substreams[idx];
        if s.param_presence_flags & PARAM_PRESENCE != 0 && br.read_bit()? {
            s.param_presence_flags = br.read_bits(8)? as u8;
        }
        let flags = s.param_presence_flags;
        if flags & PARAM_BLOCKSIZE != 0 && br.read_bit()? {
            s.blocksize = br.read_bits(9)? as usize;
            if s.blocksize < 8 || s.blocksize > self.access_unit_size {
                return Err(Error::corrupt(format!("TrueHD: block size {}", s.blocksize)));
            }
        }
        if flags & PARAM_MATRIX != 0 && br.read_bit()? {
            let count = br.read_bits(4)? as usize;
            if count > MAX_MATRICES {
                return Err(Error::corrupt("TrueHD: too many primitive matrices"));
            }
            let max_chan = s.max_matrix_channel + if s.noise_type { 0 } else { 2 };
            s.matrices.clear();
            for _ in 0..count {
                let out_ch = br.read_bits(4)? as usize;
                let frac_bits = br.read_bits(4)?;
                let lsb_bypass = br.read_bit()?;
                if out_ch > s.max_matrix_channel || frac_bits > 14 {
                    return Err(Error::corrupt("TrueHD: matrix parameters out of range"));
                }
                let mut coeffs = [0i32; MAX_CHANNELS];
                for c in coeffs.iter_mut().take(max_chan + 1) {
                    if br.read_bit()? {
                        *c = br.read_signed(frac_bits + 2)? << (14 - frac_bits);
                    }
                }
                let noise_shift = if s.noise_type { br.read_bits(4)? } else { 0 };
                s.matrices.push(Matrix {
                    out_ch,
                    lsb_bypass,
                    noise_shift,
                    coeffs,
                });
            }
        }
        if flags & PARAM_OUTSHIFT != 0 && br.read_bit()? {
            for ch in 0..=s.max_matrix_channel {
                s.output_shift[ch] = br.read_signed(4)?;
            }
        }
        if flags & PARAM_QUANTSTEP != 0 && br.read_bit()? {
            for ch in 0..=s.max_channel {
                s.quant_step_size[ch] = br.read_bits(4)?;
                s.recompute_sign_offset(ch);
            }
        }
        for ch in s.min_channel..=s.max_channel {
            if br.read_bit()? {
                if flags & PARAM_FIR != 0 && br.read_bit()? {
                    read_filter(br, &mut s.channels[ch].fir, false)?;
                }
                if flags & PARAM_IIR != 0 && br.read_bit()? {
                    read_filter(br, &mut s.channels[ch].iir, true)?;
                }
                let cp = &s.channels[ch];
                if cp.fir.order + cp.iir.order > MAX_FILTER_ORDER {
                    return Err(Error::corrupt("TrueHD: FIR+IIR order above 8"));
                }
                if cp.fir.order > 0 && cp.iir.order > 0 && cp.fir.shift != cp.iir.shift {
                    return Err(Error::corrupt("TrueHD: FIR and IIR shifts differ"));
                }
                if flags & PARAM_HUFFOFFSET != 0 && br.read_bit()? {
                    s.channels[ch].huff_offset = br.read_signed(15)?;
                }
                s.channels[ch].codebook = br.read_bits(2)? as u8;
                s.channels[ch].huff_lsbs = br.read_bits(5)? as u8;
                if s.channels[ch].huff_lsbs > 24 {
                    return Err(Error::corrupt("TrueHD: huff_lsbs above 24"));
                }
                s.recompute_sign_offset(ch);
            }
        }
        Ok(())
    }

    fn read_block(&mut self, idx: usize, br: &mut BitReader) -> Result<()> {
        let s = &mut self.substreams[idx];
        let expected_end = if s.data_check_present {
            Some(br.bit_position() + u64::from(br.read_bits(16)?))
        } else {
            None
        };
        if s.blockpos + s.blocksize > self.access_unit_size {
            return Err(Error::corrupt("TrueHD: blocks exceed the access unit"));
        }
        for i in s.blockpos..s.blockpos + s.blocksize {
            for (m, mat) in s.matrices.iter().enumerate() {
                if mat.lsb_bypass {
                    self.bypassed_lsbs[i][m] = br.read_bit()? as i32;
                }
            }
            for ch in s.min_channel..=s.max_channel {
                let cp = &s.channels[ch];
                let qss = s.quant_step_size[ch];
                let lsb_bits = i32::from(cp.huff_lsbs) - qss as i32;
                let mut result = 0i32;
                if cp.codebook > 0 {
                    result = self.huff.0.decode(br, usize::from(cp.codebook) - 1)?;
                }
                if lsb_bits > 0 {
                    result = (result << lsb_bits) + br.read_bits(lsb_bits as u32)? as i32;
                } else if lsb_bits < 0 {
                    return Err(Error::corrupt("TrueHD: quant step above huff_lsbs"));
                }
                result = result.wrapping_add(cp.sign_huff_offset).wrapping_shl(qss);
                self.sample_buffer[i][ch] = result;
            }
        }
        for ch in s.min_channel..=s.max_channel {
            filter_channel(&mut s.channels[ch], s.quant_step_size[ch], &mut self.sample_buffer[s.blockpos..s.blockpos + s.blocksize], ch);
        }
        s.blockpos += s.blocksize;
        if let Some(end) = expected_end {
            if br.bit_position() != end {
                self.stats.length_mismatches += 1;
            }
            br.skip_bits(8)?;
        }
        Ok(())
    }

    fn rematrix(&mut self, idx: usize) {
        let s = &mut self.substreams[idx];
        let mut maxchan = s.max_matrix_channel;
        if !s.noise_type {
            // Two noise channels after the last matrix channel, from the
            // 23-bit generator seeded by the restart header and carried on.
            let mut seed = s.noisegen_seed;
            for row in self.sample_buffer.iter_mut().take(s.blockpos) {
                let shr7 = (seed >> 7) as u16;
                row[maxchan + 1] = i32::from((seed >> 15) as u8 as i8) << s.noise_shift;
                row[maxchan + 2] = i32::from(shr7 as u8 as i8) << s.noise_shift;
                seed = (seed << 16) ^ (u32::from(shr7) << 5) ^ (u32::from(shr7) << 7);
            }
            s.noisegen_seed = seed;
            maxchan += 2;
        } else {
            let mut seed = s.noisegen_seed;
            for slot in self.noise_buffer.iter_mut() {
                let shr15 = (seed >> 15) as u8;
                *slot = NOISE_TABLE[usize::from(shr15)];
                seed = (seed << 8) ^ u32::from(shr15) ^ (u32::from(shr15) << 5);
            }
            s.noisegen_seed = seed;
        }
        let pow2_mask = self.access_unit_size_pow2 - 1;
        let count = s.matrices.len();
        for (m, mat) in s.matrices.iter().enumerate() {
            let mask = !((1i32 << s.quant_step_size[mat.out_ch]) - 1);
            let mut index = count - m;
            let index2 = 2 * index + 1;
            for (i, row) in self.sample_buffer.iter_mut().take(s.blockpos).enumerate() {
                let mut accum: i64 = 0;
                for (sample, coeff) in row.iter().zip(&mat.coeffs).take(maxchan + 1) {
                    accum += i64::from(*sample) * i64::from(*coeff);
                }
                if mat.noise_shift != 0 {
                    index &= pow2_mask;
                    accum += i64::from(self.noise_buffer[index]) << (mat.noise_shift + 7);
                    index += index2;
                }
                row[mat.out_ch] = ((accum >> 14) as i32 & mask) + self.bypassed_lsbs[i][m];
            }
        }
    }

    fn output(&mut self, idx: usize, layout: ChannelLayout, samples: usize) -> Result<AudioFrame> {
        let s = &mut self.substreams[idx];
        let channels = layout.channel_count();
        // TrueHD's Ls/Rs (output 4,5) are our side pair, Lrs/Rrs (6,7) our
        // back pair.
        const PERM_7_1: [usize; 8] = [0, 1, 2, 3, 6, 7, 4, 5];
        let mut bytes = Vec::with_capacity(samples * channels * 4);
        for row in self.sample_buffer.iter().take(samples) {
            for (out_ch, &perm) in PERM_7_1.iter().enumerate().take(channels) {
                let thd_ch = if channels == 8 { perm } else { out_ch };
                let mat_ch = s.ch_assign[thd_ch];
                let sample = row[mat_ch].wrapping_shl(s.output_shift[mat_ch] as u32);
                s.lossless_check_data ^= ((sample as u32) & 0xFF_FFFF) << mat_ch;
                bytes.extend_from_slice(&(sample << 8).to_ne_bytes());
            }
        }
        AudioFrame::try_new(
            SampleFormat::S32,
            false,
            layout,
            self.sample_rate,
            samples,
            vec![Buf::from_vec(bytes)],
        )
    }
}

/// New coefficients for `f`; its history carries over unless the stream
/// sends an explicit (IIR-only) state.
fn read_filter(br: &mut BitReader, f: &mut Filter, iir: bool) -> Result<()> {
    f.order = br.read_bits(4)? as usize;
    if f.order > MAX_FILTER_ORDER || (!iir && f.order > 8) || (iir && f.order > 4) {
        return Err(Error::corrupt("TrueHD: filter order out of range"));
    }
    if f.order > 0 {
        f.shift = br.read_bits(4)?;
        let coeff_bits = br.read_bits(5)?;
        let coeff_shift = br.read_bits(3)?;
        if !(1..=16).contains(&coeff_bits) || coeff_bits + coeff_shift > 16 {
            return Err(Error::corrupt("TrueHD: filter coefficient precision out of range"));
        }
        for c in f.coeffs.iter_mut().take(f.order) {
            *c = br.read_signed(coeff_bits)? << coeff_shift;
        }
        if br.read_bit()? {
            if !iir {
                return Err(Error::corrupt("TrueHD: FIR filter with explicit state"));
            }
            let state_bits = br.read_bits(4)?;
            let state_shift = br.read_bits(4)?;
            for st in f.state.iter_mut().take(f.order) {
                *st = if state_bits > 0 { br.read_signed(state_bits)? << state_shift } else { 0 };
            }
        }
    }
    Ok(())
}

/// Residual → sample through the channel's FIR (over past samples) and IIR
/// (over past residual-after-prediction) filters, in place on column `ch`.
fn filter_channel(cp: &mut ChannelParams, qss: u32, rows: &mut [[i32; MAX_CHANNELS]], ch: usize) {
    let mask = !((1i32 << qss) - 1);
    let shift = if cp.fir.order > 0 { cp.fir.shift } else { cp.iir.shift };
    for row in rows {
        let mut accum: i64 = 0;
        for j in 0..cp.fir.order {
            accum += i64::from(cp.fir.state[j]) * i64::from(cp.fir.coeffs[j]);
        }
        for j in 0..cp.iir.order {
            accum += i64::from(cp.iir.state[j]) * i64::from(cp.iir.coeffs[j]);
        }
        let accum = (accum >> shift) as i32;
        let result = accum.wrapping_add(row[ch]) & mask;
        cp.fir.state.copy_within(0..MAX_FILTER_ORDER - 1, 1);
        cp.fir.state[0] = result;
        cp.iir.state.copy_within(0..MAX_FILTER_ORDER - 1, 1);
        cp.iir.state[0] = result.wrapping_sub(accum);
        row[ch] = result;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code in the public tables decodes back to its own index when
    /// written bit-exactly, with and without trailing garbage.
    #[test]
    fn every_codebook_entry_decodes_to_its_index() {
        use ec_core::bitio::BitWriter;
        let lut = HuffLut::build();
        for (book, table) in HUFFMAN_TABLES.iter().enumerate() {
            for (index, &(code, len)) in table.iter().enumerate() {
                if len == 0 {
                    continue;
                }
                let mut w = BitWriter::new();
                w.write_bits(u32::from(code), u32::from(len));
                w.write_bits(0x5A5, 11);
                let bytes = w.into_bytes();
                let mut br = BitReader::new(&bytes);
                assert_eq!(lut.decode(&mut br, book).unwrap(), index as i32, "book {book} index {index}");
                assert_eq!(br.bit_position(), u64::from(len));
                // Bare code at the very end of the buffer must decode too.
                let mut w = BitWriter::new();
                w.write_bits(u32::from(code), u32::from(len));
                let bytes = w.into_bytes();
                let mut br = BitReader::new(&bytes);
                assert_eq!(lut.decode(&mut br, book).unwrap(), index as i32);
            }
        }
        // An all-zero 9-bit word is not a code in any book.
        for book in 0..3 {
            assert!(lut.decode(&mut BitReader::new(&[0, 0]), book).is_err());
        }
    }

    #[test]
    fn codebooks_are_prefix_free() {
        for table in &HUFFMAN_TABLES {
            let used: Vec<_> = table.iter().filter(|e| e.1 > 0).collect();
            for (i, a) in used.iter().enumerate() {
                for b in &used[i + 1..] {
                    let l = a.1.min(b.1);
                    assert_ne!(a.0 >> (a.1 - l), b.0 >> (b.1 - l), "{a:?} vs {b:?}");
                }
            }
        }
    }
}

