//! Layer III decoding: side info, scalefactors, spectrum, stereo, and the
//! filterbank that turns 576 coefficients back into PCM.

use crate::filterbank::{Imdct, Synthesis, alias_reduce};
use crate::header::{ChannelMode, FrameHeader, Version, crc16};
use crate::huffman;
use crate::tables::{
    LSF_PARTITIONS, MAX_QUANT, PRETAB, SLEN, long_starts, power43, short_starts, short_widths,
};
use ec_core::bitio::BitReader;
use ec_core::error::{Error, Result};

/// One decoded frame: interleaved `f32` samples in `[-1, 1]`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodedFrame {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count, 1 or 2.
    pub channels: usize,
    /// Interleaved samples, `channels * samples_per_frame` of them.
    pub samples: Vec<f32>,
}

#[derive(Clone, Copy, Debug, Default)]
struct GranuleInfo {
    part2_3_length: u32,
    big_values: u32,
    global_gain: u32,
    scalefac_compress: u32,
    window_switching: bool,
    block_type: u8,
    mixed_block: bool,
    table_select: [u8; 3],
    subblock_gain: [u32; 3],
    region0_count: u32,
    region1_count: u32,
    preflag: bool,
    scalefac_scale: bool,
    count1table_select: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct SideInfo {
    main_data_begin: u32,
    scfsi: [[bool; 4]; 2],
    granules: [[GranuleInfo; 2]; 2],
}

fn parse_side_info(header: &FrameHeader, bytes: &[u8]) -> Result<SideInfo> {
    let channels = header.channels();
    let lsf = header.version != Version::Mpeg1;
    let mut r = BitReader::new(bytes);
    let mut side = SideInfo {
        main_data_begin: r.read_bits(if lsf { 8 } else { 9 })?,
        ..SideInfo::default()
    };
    let private = match (lsf, channels) {
        (true, 1) => 1,
        (true, _) => 2,
        (false, 1) => 5,
        (false, _) => 3,
    };
    r.skip_bits(private)?;
    if !lsf {
        for ch in 0..channels {
            for band in 0..4 {
                side.scfsi[ch][band] = r.read_bit()?;
            }
        }
    }
    for gr in 0..header.granules() {
        for ch in 0..channels {
            let mut g = GranuleInfo {
                part2_3_length: r.read_bits(12)?,
                big_values: r.read_bits(9)?,
                global_gain: r.read_bits(8)?,
                scalefac_compress: r.read_bits(if lsf { 9 } else { 4 })?,
                window_switching: r.read_bit()?,
                ..GranuleInfo::default()
            };
            if g.big_values > 288 {
                return Err(Error::corrupt(format!(
                    "mp3: big_values {} exceeds 288",
                    g.big_values
                )));
            }
            if g.window_switching {
                g.block_type = r.read_bits(2)? as u8;
                g.mixed_block = r.read_bit()?;
                for slot in &mut g.table_select[..2] {
                    *slot = r.read_bits(5)? as u8;
                }
                for slot in &mut g.subblock_gain {
                    *slot = r.read_bits(3)?;
                }
                g.region0_count = if g.block_type == 2 && !g.mixed_block {
                    8
                } else {
                    7
                };
                g.region1_count = 20 - g.region0_count;
                if g.block_type == 0 {
                    return Err(Error::corrupt(
                        "mp3: window switching with normal block type",
                    ));
                }
            } else {
                for slot in &mut g.table_select {
                    *slot = r.read_bits(5)? as u8;
                }
                g.region0_count = r.read_bits(4)?;
                g.region1_count = r.read_bits(3)?;
            }
            if !lsf {
                g.preflag = r.read_bit()?;
            }
            g.scalefac_scale = r.read_bit()?;
            g.count1table_select = r.read_bit()?;
            side.granules[gr][ch] = g;
        }
    }
    Ok(side)
}

/// The stateful half of the decoder: one frame in, one frame of PCM out.
///
/// State that crosses frames — the bit reservoir, the IMDCT overlap and the
/// polyphase window history — lives here, so [`Mp3Decode::reset`] after a seek
/// is the whole story.
#[derive(Debug)]
pub struct Mp3Decode {
    imdct: Imdct,
    synthesis: [Synthesis; 2],
    overlap: [Box<[[f32; 18]; 32]>; 2],
    reservoir: Vec<u8>,
    is: [Box<[i32; 576]>; 2],
    xr: [Box<[f32; 576]>; 2],
    scalefac_l: [[u8; 23]; 2],
    scalefac_s: [[[u8; 3]; 13]; 2],
}

impl Default for Mp3Decode {
    fn default() -> Mp3Decode {
        Mp3Decode {
            imdct: Imdct::default(),
            synthesis: [Synthesis::default(), Synthesis::default()],
            overlap: [Box::new([[0.0; 18]; 32]), Box::new([[0.0; 18]; 32])],
            reservoir: Vec::with_capacity(2048),
            is: [Box::new([0; 576]), Box::new([0; 576])],
            xr: [Box::new([0.0; 576]), Box::new([0.0; 576])],
            scalefac_l: [[0; 23]; 2],
            scalefac_s: [[[0; 3]; 13]; 2],
        }
    }
}

impl Mp3Decode {
    /// A decoder with an empty reservoir and silent history.
    pub fn new() -> Mp3Decode {
        Mp3Decode::default()
    }

    /// Drops the reservoir and the filterbank history, as a seek requires.
    pub fn reset(&mut self) {
        self.reservoir.clear();
        for synth in &mut self.synthesis {
            synth.reset();
        }
        for overlap in &mut self.overlap {
            overlap.iter_mut().for_each(|sb| sb.fill(0.0));
        }
    }

    /// Decodes one complete frame, header included.
    ///
    /// A frame whose main data has not arrived yet — the bit reservoir points
    /// further back than we have seen, which is normal for the first frames
    /// after a seek — decodes as silence rather than as an error, because that
    /// is what the reservoir mechanism means.
    pub fn decode_frame(&mut self, frame: &[u8]) -> Result<DecodedFrame> {
        let header = FrameHeader::parse(frame)?;
        if header.layer != 3 {
            return Err(Error::unsupported(
                format!("MPEG audio layer {}", header.layer),
                "only Layer III is implemented in ec-mp3",
            ));
        }
        let Some(frame_len) = header.frame_len() else {
            return Err(Error::unsupported(
                "free-format MPEG audio",
                "no bitrate index, so the frame length is only knowable by scanning",
            ));
        };
        if frame.len() < frame_len {
            return Err(Error::NeedMore);
        }
        let side_len = header.side_info_len();
        let crc_len = usize::from(header.crc) * 2;
        let side_start = 4 + crc_len;
        if header.crc {
            let stored = u16::from_be_bytes([frame[4], frame[5]]);
            let computed = crc16(
                &[frame[0], frame[1], frame[2], frame[3]],
                &frame[side_start..side_start + side_len],
            );
            if stored != computed {
                return Err(Error::corrupt(format!(
                    "mp3: side info CRC {computed:#06x} != {stored:#06x}"
                )));
            }
        }
        let side = parse_side_info(&header, &frame[side_start..side_start + side_len])?;
        let main = &frame[side_start + side_len..frame_len];

        let begin = side.main_data_begin as usize;
        let have = self.reservoir.len();
        self.reservoir.extend_from_slice(main);
        let channels = header.channels();
        let mut samples = vec![0.0f32; header.samples_per_frame() * channels];
        if begin > have {
            // The reservoir does not reach back that far: this frame's own
            // spectrum is unreadable, so emit silence and keep the bytes for
            // the frames that follow.
            self.trim_reservoir();
            return Ok(DecodedFrame {
                sample_rate: header.sample_rate,
                channels,
                samples,
            });
        }
        let start = have - begin;
        let data = self.reservoir[start..].to_vec();
        let mut bit = 0u64;
        for gr in 0..header.granules() {
            for ch in 0..channels {
                self.decode_granule(&header, &side, gr, ch, &data, bit)?;
                bit += u64::from(side.granules[gr][ch].part2_3_length);
            }
            self.stereo(&header, &side, gr);
            for ch in 0..channels {
                let granule = side.granules[gr][ch];
                self.synthesise(&granule, ch, gr, channels, &mut samples);
            }
        }
        self.trim_reservoir();
        Ok(DecodedFrame {
            sample_rate: header.sample_rate,
            channels,
            samples,
        })
    }

    fn trim_reservoir(&mut self) {
        // 511 bytes is the largest `main_data_begin` a frame can name.
        const KEEP: usize = 511;
        if self.reservoir.len() > KEEP {
            let drop = self.reservoir.len() - KEEP;
            self.reservoir.drain(..drop);
        }
    }

    fn decode_granule(
        &mut self,
        header: &FrameHeader,
        side: &SideInfo,
        gr: usize,
        ch: usize,
        data: &[u8],
        start_bit: u64,
    ) -> Result<()> {
        let granule = side.granules[gr][ch];
        let reader = &mut BitReader::new(data);
        reader.skip_bits(start_bit)?;
        let end_bit = start_bit + u64::from(granule.part2_3_length);
        if header.version == Version::Mpeg1 {
            self.read_scalefactors_v1(side, gr, ch, reader)?;
        } else {
            self.read_scalefactors_lsf(header, &granule, ch, reader)?;
        }
        let scalefactor_bits = reader.bit_position() - start_bit;
        if scalefactor_bits > u64::from(granule.part2_3_length) {
            return Err(Error::corrupt("mp3: scalefactors longer than the granule"));
        }
        // Whatever the granule reserved but did not use is stuffing, and the
        // next granule starts at its own offset regardless, so nothing here
        // needs to seek to the end.
        self.read_spectrum(header, &granule, ch, reader, end_bit)?;
        self.requantise(header, &granule, ch);
        Ok(())
    }

    fn read_scalefactors_v1(
        &mut self,
        side: &SideInfo,
        gr: usize,
        ch: usize,
        reader: &mut BitReader<'_>,
    ) -> Result<()> {
        let granule = side.granules[gr][ch];
        let (slen1, slen2) = SLEN[(granule.scalefac_compress & 15) as usize];
        if granule.window_switching && granule.block_type == 2 {
            if granule.mixed_block {
                for sfb in 0..8 {
                    self.scalefac_l[ch][sfb] = reader.read_bits(slen1)? as u8;
                }
                for sfb in 3..6 {
                    for w in 0..3 {
                        self.scalefac_s[ch][sfb][w] = reader.read_bits(slen1)? as u8;
                    }
                }
            } else {
                for sfb in 0..6 {
                    for w in 0..3 {
                        self.scalefac_s[ch][sfb][w] = reader.read_bits(slen1)? as u8;
                    }
                }
            }
            for sfb in 6..12 {
                for w in 0..3 {
                    self.scalefac_s[ch][sfb][w] = reader.read_bits(slen2)? as u8;
                }
            }
            self.scalefac_s[ch][12] = [0; 3];
        } else {
            const GROUPS: [(usize, usize); 4] = [(0, 6), (6, 11), (11, 16), (16, 21)];
            for (group, (from, to)) in GROUPS.into_iter().enumerate() {
                if gr == 1 && side.scfsi[ch][group] {
                    continue; // reuse granule 0's values
                }
                let slen = if from < 11 { slen1 } else { slen2 };
                for sfb in from..to {
                    self.scalefac_l[ch][sfb] = reader.read_bits(slen)? as u8;
                }
            }
            self.scalefac_l[ch][21] = 0;
            self.scalefac_l[ch][22] = 0;
        }
        Ok(())
    }

    fn read_scalefactors_lsf(
        &mut self,
        header: &FrameHeader,
        granule: &GranuleInfo,
        ch: usize,
        reader: &mut BitReader<'_>,
    ) -> Result<()> {
        let intensity = header.mode == ChannelMode::JointStereo && header.mode_ext & 1 != 0;
        let mut sfc = granule.scalefac_compress;
        let slen;
        let block_number;
        if intensity && ch == 1 {
            sfc >>= 1;
            if sfc < 180 {
                slen = [sfc / 36, (sfc % 36) / 6, sfc % 6, 0];
                block_number = 3;
            } else if sfc < 244 {
                sfc -= 180;
                slen = [(sfc % 64) >> 4, (sfc % 16) >> 2, sfc % 4, 0];
                block_number = 4;
            } else {
                sfc -= 244;
                slen = [sfc / 3, sfc % 3, 0, 0];
                block_number = 5;
            }
        } else if sfc < 400 {
            slen = [(sfc >> 4) / 5, (sfc >> 4) % 5, (sfc % 16) >> 2, sfc % 4];
            block_number = 0;
        } else if sfc < 500 {
            sfc -= 400;
            slen = [(sfc >> 2) / 5, (sfc >> 2) % 5, sfc % 4, 0];
            block_number = 1;
        } else {
            sfc -= 500;
            slen = [sfc / 3, sfc % 3, 0, 0];
            block_number = 2;
        }
        let short = granule.window_switching && granule.block_type == 2;
        let kind = if !short {
            0
        } else if granule.mixed_block {
            2
        } else {
            1
        };
        let counts = LSF_PARTITIONS[block_number][kind];
        self.scalefac_l[ch] = [0; 23];
        self.scalefac_s[ch] = [[0; 3]; 13];
        let mut band = 0usize;
        for (partition, count) in counts.into_iter().enumerate() {
            for _ in 0..count {
                let value = reader.read_bits(slen[partition])? as u8;
                match kind {
                    0 => self.scalefac_l[ch][band] = value,
                    1 => self.scalefac_s[ch][band / 3][band % 3] = value,
                    _ if band < 6 => self.scalefac_l[ch][band] = value,
                    _ => {
                        let index = band - 6;
                        self.scalefac_s[ch][3 + index / 3][index % 3] = value;
                    }
                }
                band += 1;
            }
        }
        Ok(())
    }

    fn read_spectrum(
        &mut self,
        header: &FrameHeader,
        granule: &GranuleInfo,
        ch: usize,
        reader: &mut BitReader<'_>,
        end_bit: u64,
    ) -> Result<()> {
        let is = &mut self.is[ch];
        is.fill(0);
        let starts = long_starts(header.sample_rate);
        let short = granule.window_switching && granule.block_type == 2;
        let (region1, region2) = if granule.window_switching {
            if short {
                // Three short bands, all three windows: 36 lines at every rate
                // whose first bands are four wide, but 72 at 8 kHz, where they
                // are eight. The fixed 36 that most descriptions quote is that
                // coincidence, and 8 kHz is where it stops holding.
                (short_starts(header.sample_rate)[3] as usize * 3, 576)
            } else {
                (starts[8] as usize, 576)
            }
        } else {
            let r0 = (granule.region0_count + 1).min(22) as usize;
            let r1 = (granule.region0_count + granule.region1_count + 2).min(22) as usize;
            (starts[r0] as usize, starts[r1] as usize)
        };
        let big = granule.big_values as usize * 2;
        let mut index = 0usize;
        let mut pair = [0.0f32; 2];
        while index < big {
            let region = if index < region1 {
                0
            } else if index < region2 {
                1
            } else {
                2
            };
            let select = usize::from(granule.table_select[region]);
            let table = huffman::big_table(select)?;
            if table.codes.is_empty() {
                index += 2;
                continue;
            }
            if reader.bit_position() >= end_bit {
                break;
            }
            huffman::decode_pair(reader, select, table, &mut pair)?;
            is[index] = pair[0] as i32;
            is[index + 1] = pair[1] as i32;
            index += 2;
        }
        let mut quad = [0.0f32; 4];
        while index + 4 <= 576 && reader.bit_position() < end_bit {
            let before = reader.bit_position();
            if huffman::decode_quad(reader, granule.count1table_select, &mut quad).is_err() {
                break;
            }
            if reader.bit_position() > end_bit {
                // The last quadruple ran past the granule: encoders are allowed
                // to leave it there, and its values are not part of the
                // spectrum.
                let _ = before;
                break;
            }
            for (slot, value) in is[index..index + 4].iter_mut().zip(quad) {
                *slot = value as i32;
            }
            index += 4;
        }
        Ok(())
    }

    fn requantise(&mut self, header: &FrameHeader, granule: &GranuleInfo, ch: usize) {
        let power = power43();
        let xr = &mut self.xr[ch];
        let is = &self.is[ch];
        let short = granule.window_switching && granule.block_type == 2;
        let multiplier = if granule.scalefac_scale { 1.0 } else { 0.5 };
        let base = (granule.global_gain as f32 - 210.0) * 0.25;
        let value = |v: i32| -> f32 {
            let magnitude = power[(v.unsigned_abs() as usize).min(MAX_QUANT)];
            if v < 0 { -magnitude } else { magnitude }
        };
        xr.fill(0.0);
        let long_bands = if !short {
            22
        } else if granule.mixed_block {
            // Mixed blocks keep the first two subbands long, which is 36
            // coefficients: eight bands at an MPEG-1 rate, six at an LSF one.
            if header.version == Version::Mpeg1 {
                8
            } else {
                6
            }
        } else {
            0
        };
        let starts = long_starts(header.sample_rate);
        for sfb in 0..long_bands {
            let (from, to) = (starts[sfb] as usize, starts[sfb + 1] as usize);
            let mut sf = f32::from(self.scalefac_l[ch][sfb]);
            if granule.preflag {
                sf += f32::from(PRETAB[sfb]);
            }
            let gain = (base - multiplier * sf).exp2();
            for i in from..to.min(576) {
                xr[i] = value(is[i]) * gain;
            }
        }
        if short {
            // Short blocks arrive band by band, window by window, and land in a
            // spectrum whose three windows interleave line by line: bitstream
            // index (sfb, window, line) becomes 3 * (band start + line) +
            // window, which is the order the short IMDCT reads back.
            let widths = short_widths(header.sample_rate);
            let starts = short_starts(header.sample_rate);
            let first = if granule.mixed_block { 3 } else { 0 };
            let mut index = if granule.mixed_block { 36 } else { 0 };
            for sfb in first..13 {
                let width = widths[sfb] as usize;
                let band_start = starts[sfb] as usize;
                for window in 0..3 {
                    let sf = f32::from(self.scalefac_s[ch][sfb][window]);
                    let gain =
                        (base - 2.0 * granule.subblock_gain[window] as f32 - multiplier * sf)
                            .exp2();
                    for line in 0..width {
                        if index >= 576 {
                            break;
                        }
                        let target = (band_start + line) * 3 + window;
                        if target < 576 {
                            xr[target] = value(is[index]) * gain;
                        }
                        index += 1;
                    }
                }
            }
        }
    }

    fn stereo(&mut self, header: &FrameHeader, side: &SideInfo, gr: usize) {
        if header.channels() != 2 {
            return;
        }
        let joint = header.mode == ChannelMode::JointStereo;
        let ms = joint && header.mode_ext & 2 != 0;
        let intensity = joint && header.mode_ext & 1 != 0;
        let granule = side.granules[gr][0];
        let short = granule.window_switching && granule.block_type == 2;
        // Intensity coding starts at the first band above the right channel's
        // last non-zero line.
        let bound = if intensity {
            let last = (0..576)
                .rev()
                .find(|&i| self.is[1][i] != 0)
                .map_or(0, |i| i + 1);
            let starts = long_starts(header.sample_rate);
            starts
                .iter()
                .find(|&&s| usize::from(s) >= last)
                .map_or(576, |s| usize::from(*s))
        } else {
            576
        };
        if ms {
            const INV_SQRT2: f32 = std::f32::consts::FRAC_1_SQRT_2;
            for i in 0..bound {
                let (m, s) = (self.xr[0][i], self.xr[1][i]);
                self.xr[0][i] = (m + s) * INV_SQRT2;
                self.xr[1][i] = (m - s) * INV_SQRT2;
            }
        }
        if intensity && bound < 576 {
            let starts = long_starts(header.sample_rate);
            for sfb in 0..22 {
                let (from, to) = (starts[sfb] as usize, starts[sfb + 1] as usize);
                if from < bound {
                    continue;
                }
                let is_pos = if short {
                    u32::from(self.scalefac_s[1][sfb.min(12)][0])
                } else {
                    u32::from(self.scalefac_l[1][sfb])
                };
                if is_pos >= 7 {
                    continue; // 7 is not a legal position; leave the band alone
                }
                let ratio = (std::f32::consts::PI / 12.0 * is_pos as f32).tan();
                let (kl, kr) = (ratio / (1.0 + ratio), 1.0 / (1.0 + ratio));
                for i in from..to.min(576) {
                    let value = self.xr[0][i];
                    self.xr[0][i] = value * kl;
                    self.xr[1][i] = value * kr;
                }
            }
        }
    }

    fn synthesise(
        &mut self,
        granule: &GranuleInfo,
        ch: usize,
        gr: usize,
        channels: usize,
        out: &mut [f32],
    ) {
        let short = granule.window_switching && granule.block_type == 2;
        let alias_bands = if !short {
            32
        } else if granule.mixed_block {
            2
        } else {
            1
        };
        alias_reduce(&mut self.xr[ch][..], alias_bands);
        let mut block = [0.0f32; 36];
        let mut slots = [[0.0f32; 32]; 18];
        // The transpose (subband-major in, slot-major out) is the point of
        // this loop, so it indexes both ways on purpose.
        #[allow(clippy::needless_range_loop)]
        for sb in 0..32 {
            let coefficients = &self.xr[ch][sb * 18..sb * 18 + 18];
            if short && !(granule.mixed_block && sb < 2) {
                self.imdct.short(coefficients, &mut block);
            } else {
                let block_type = if short { 0 } else { granule.block_type };
                self.imdct.long(coefficients, block_type, &mut block);
            }
            let overlap = &mut self.overlap[ch][sb];
            for t in 0..18 {
                let mut sample = block[t] + overlap[t];
                overlap[t] = block[18 + t];
                if sb % 2 == 1 && t % 2 == 1 {
                    sample = -sample;
                }
                slots[t][sb] = sample;
            }
        }
        let mut pcm = [0.0f32; 32];
        for (t, slot) in slots.iter().enumerate() {
            self.synthesis[ch].slot(slot, &mut pcm);
            let base = (gr * 18 + t) * 32 * channels + ch;
            for (j, sample) in pcm.iter().enumerate() {
                out[base + j * channels] = *sample;
            }
        }
    }
}

/// Turns a byte stream into frames: skips ID3v2 and the Xing/LAME/VBRI header
/// frame, resyncs over junk, and holds partial data until the rest arrives.
#[derive(Debug, Default)]
pub struct Mp3Reader {
    decoder: Mp3Decode,
    buffer: Vec<u8>,
    consumed: usize,
    started: bool,
}

impl Mp3Reader {
    /// An empty reader.
    pub fn new() -> Mp3Reader {
        Mp3Reader::default()
    }

    /// Appends bytes to decode.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.consumed > 0 {
            self.buffer.drain(..self.consumed);
            self.consumed = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// Drops buffered input and decoder history.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.consumed = 0;
        self.started = false;
        self.decoder.reset();
    }

    /// The next frame, or [`Error::NeedMore`] when the buffer holds only part
    /// of one.
    pub fn next_frame(&mut self) -> Result<DecodedFrame> {
        loop {
            let data = &self.buffer[self.consumed..];
            if data.len() < 4 {
                return Err(Error::NeedMore);
            }
            if !self.started && data.starts_with(b"ID3") {
                if data.len() < 10 {
                    return Err(Error::NeedMore);
                }
                let size = data[6..10]
                    .iter()
                    .fold(0usize, |acc, b| (acc << 7) | usize::from(b & 0x7F));
                let total = 10 + size;
                if data.len() < total {
                    return Err(Error::NeedMore);
                }
                self.consumed += total;
                continue;
            }
            let Ok(header) = FrameHeader::parse(data) else {
                self.consumed += 1;
                continue;
            };
            let Some(frame_len) = header.frame_len() else {
                return Err(Error::unsupported(
                    "free-format MPEG audio",
                    "no bitrate index, so the frame length is only knowable by scanning",
                ));
            };
            if header.layer != 3 {
                self.consumed += 1;
                continue;
            }
            if data.len() < frame_len {
                return Err(Error::NeedMore);
            }
            let frame = &data[..frame_len];
            if !self.started && is_info_frame(&header, frame) {
                self.started = true;
                self.consumed += frame_len;
                continue;
            }
            self.started = true;
            let decoded = self.decoder.decode_frame(frame);
            self.consumed += frame_len;
            return decoded;
        }
    }

    /// Decodes everything buffered, stopping at the first incomplete frame.
    pub fn decode_all(&mut self) -> Vec<DecodedFrame> {
        let mut out = Vec::new();
        while let Ok(frame) = self.next_frame() {
            out.push(frame);
        }
        out
    }
}

/// True for the Xing/Info/VBRI header frame every VBR encoder puts first: it
/// carries a tag rather than audio, and decoding it would prepend a frame of
/// silence no other decoder emits.
fn is_info_frame(header: &FrameHeader, frame: &[u8]) -> bool {
    let side = 4 + usize::from(header.crc) * 2 + header.side_info_len();
    let tagged = |at: usize, tag: &[u8]| frame.len() >= at + 4 && &frame[at..at + 4] == tag;
    tagged(side, b"Xing") || tagged(side, b"Info") || tagged(36, b"VBRI")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id3v2_and_junk_are_skipped_before_the_first_frame() {
        let mut reader = Mp3Reader::new();
        let mut stream = b"ID3\x04\x00\x00\x00\x00\x00\x05hello".to_vec();
        stream.extend_from_slice(&[0x00, 0x11, 0x22]); // junk before sync
        assert!(reader.next_frame().is_err());
        reader.push(&stream);
        assert!(matches!(reader.next_frame(), Err(Error::NeedMore)));
    }
}
