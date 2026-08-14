//! The encoder: PCM in, Vorbis packets out, with a setup header written for the
//! stream's own channel layout.
//!
//! ## What the analysis actually is
//!
//! Per block, per channel: an MDCT, a peak magnitude per floor point, a
//! spreading function over those points (a one-pole decay up and down the
//! spectrum, which is the cheap stand-in for a masking curve), an absolute
//! threshold of hearing under it, and a headroom the rate loop moves. The floor
//! that comes out is the quantiser step: the residue is the spectrum divided by
//! the floor and rounded, so headroom in dB *is* precision in bits. That is
//! psychoacoustics-lite and it is stated as such — there is no tonality
//! estimate, no temporal pre-echo control and no per-band bit allocation beyond
//! what the floor implies.
//!
//! ## Codebooks
//!
//! Designed here rather than inherited: a floor book over the folded amplitude
//! range, a class book over partition-class pairs, and four residue books whose
//! ranges (+-1, +-4, +-16, +-127) the partition classifier picks between. All of
//! them are Huffman codes over stated distributions, built at construction and
//! written into every stream's own setup header — which is why this encoder
//! needs no embedded profile and has no channel count it cannot serve.

use std::collections::VecDeque;

use ec_core::{
    AudioParameters, Buf, CodecId, CodecParameters, Encoder, Error, Frame, MediaParameters, Packet,
    Result, SampleFormat, TimeBase,
};
use ec_dsp::Mdct;

use crate::bits::{BitsOut, float32_pack};
use crate::codebook::{CodebookSpec, ilog};
use crate::decode::channel_map;
use crate::floor::render_floor1;
use crate::setup::Floor1;
use crate::window;

/// Long block size; this encoder codes long blocks only.
const BLOCK: usize = 2048;
/// Samples between block centres.
const HOP: usize = BLOCK / 2;
/// Coefficients per block.
const HALF: usize = BLOCK / 2;
/// Coefficients per residue partition.
const PARTITION: usize = 32;
/// Floor X points besides the two endpoints.
const FLOOR_POINTS: usize = 20;
/// Floor values per class.
const FLOOR_CLASS_DIM: usize = 4;
/// Residue classes; class 0 codes nothing at all.
const CLASSES: usize = 5;
/// Vorbis states spectral coefficients on the scale where a full-scale tone is
/// a coefficient of about one, which is what lets the floor table (whose top is
/// exactly 1.0) carry them. That is `2/N` times the unnormalised transform
/// ec-dsp computes, `N` being the coefficient count.
const VORBIS_MDCT_SCALE: f32 = 2.0 / HALF as f32;
/// Largest quantised residue each class's book can state.
const CLASS_RANGE: [i32; CLASSES] = [0, 1, 4, 16, 127];

/// How the encoder is set up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncoderConfig {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count, in [`ec_core::ChannelLayout`] order.
    pub channels: u16,
    /// Target bitrate. Zero or less runs in pure quality mode.
    pub bitrate_bps: i32,
    /// Quality on `[0, 1]`: where the rate loop starts, and the fixed setting
    /// when `bitrate_bps` is not positive.
    pub quality: f32,
}

impl Default for EncoderConfig {
    fn default() -> EncoderConfig {
        EncoderConfig {
            sample_rate: 48_000,
            channels: 2,
            bitrate_bps: 128_000,
            quality: 0.5,
        }
    }
}

/// One encoded packet and where it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedPacket {
    /// Packet payload.
    pub data: Vec<u8>,
    /// Sample position this packet ends at — the Ogg granule.
    pub granule: i64,
    /// Samples this packet finalises.
    pub samples: i64,
}

/// Vorbis I encoder.
pub struct VorbisEncoder {
    config: EncoderConfig,
    params: CodecParameters,
    /// `to_vorbis[ec channel] = vorbis channel`.
    to_vorbis: Vec<usize>,
    headers: Vec<Vec<u8>>,
    floor: Floor1,
    floor_book: CodebookSpec,
    class_book: CodebookSpec,
    residue_books: Vec<CodebookSpec>,
    /// Absolute threshold of hearing per floor point, in dBFS.
    ath: Vec<f64>,
    /// Channel pairs coupled by the mapping; empty unless stereo.
    coupling: Vec<(usize, usize)>,
    mdct: Mdct<f32>,
    window: Vec<f32>,
    /// Per Vorbis channel, input samples with the first block's left half of
    /// zeroes already in front.
    buffer: Vec<Vec<f32>>,
    /// Input samples taken from the caller.
    fed: i64,
    /// Blocks already emitted.
    blocks: i64,
    /// Granule of the packet emitted last.
    granule: i64,
    /// Headroom under the masking curve, in dB — the rate loop's variable.
    headroom: f64,
    finished: bool,
    packets: VecDeque<EncodedPacket>,
}

impl VorbisEncoder {
    /// Build an encoder and write its three header packets.
    pub fn new(config: EncoderConfig) -> Result<VorbisEncoder> {
        let channels = usize::from(config.channels);
        if channels == 0 || channels > 255 {
            return Err(Error::unsupported(
                format!("{channels} channels"),
                "Vorbis states a channel count in one byte",
            ));
        }
        if config.sample_rate == 0 {
            return Err(Error::corrupt("encoder sample rate of zero"));
        }
        let (layout, to_ec) = channel_map(channels);
        let mut to_vorbis = vec![0usize; channels];
        for (vorbis, &ec) in to_ec.iter().enumerate() {
            to_vorbis[ec] = vorbis;
        }
        // Coupling pays for a stereo pair and nothing else here: the surround
        // layouts have no two channels that carry the same programme.
        let coupling = match channels {
            2 => vec![(0usize, 1usize)],
            _ => Vec::new(),
        };

        let floor = build_floor();
        let floor_book = design_floor_book();
        let class_book = design_class_book();
        let residue_books = design_residue_books();
        let ath = floor
            .x_list
            .iter()
            .map(|&x| {
                let hz = f64::from(config.sample_rate) * 0.5 * f64::from(x) / HALF as f64;
                absolute_threshold(hz)
            })
            .collect();

        let headers = write_headers(
            &config,
            channels,
            &floor,
            &floor_book,
            &class_book,
            &residue_books,
            &coupling,
        );
        let params = CodecParameters {
            codec: CodecId::Vorbis,
            media: MediaParameters::Audio(AudioParameters {
                sample_rate: config.sample_rate,
                layout,
                format: Some(SampleFormat::F32),
                bits_per_sample: None,
            }),
            extradata: Some(Buf::from_vec(lace(&headers))),
        };

        let quality = f64::from(config.quality.clamp(0.0, 1.0));
        Ok(VorbisEncoder {
            mdct: Mdct::new(BLOCK),
            window: window::build(BLOCK, BLOCK, true, true),
            // The first block is centred on input sample 0, so its left half is
            // the only pre-roll there is and the caller never sees it.
            buffer: vec![vec![0.0; HOP]; channels],
            fed: 0,
            blocks: 0,
            granule: 0,
            headroom: 4.0 + 26.0 * quality,
            finished: false,
            packets: VecDeque::new(),
            config,
            params,
            to_vorbis,
            headers,
            floor,
            floor_book,
            class_book,
            residue_books,
            ath,
            coupling,
        })
    }

    /// The three header packets, in order.
    pub fn headers(&self) -> Vec<&[u8]> {
        self.headers.iter().map(|h| &h[..]).collect()
    }

    /// The three header packets Xiph-laced, the form containers carry.
    pub fn extradata(&self) -> Vec<u8> {
        lace(&self.headers)
    }

    /// Parameters describing the stream, `extradata` included.
    pub fn parameters(&self) -> &CodecParameters {
        &self.params
    }

    /// Push planar samples, one slice per channel in [`ec_core::ChannelLayout`] order.
    pub fn push_planar(&mut self, channels: &[&[f32]]) -> Result<()> {
        if channels.len() != usize::from(self.config.channels) {
            return Err(Error::corrupt(format!(
                "{} channels pushed into a {}-channel encoder",
                channels.len(),
                self.config.channels
            )));
        }
        if self.finished {
            return Err(Error::corrupt("samples pushed after finish"));
        }
        let samples = channels.first().map_or(0, |c| c.len());
        for (ec, data) in channels.iter().enumerate() {
            if data.len() != samples {
                return Err(Error::corrupt("channels of different lengths"));
            }
            self.buffer[self.to_vorbis[ec]].extend_from_slice(data);
        }
        self.fed += samples as i64;
        self.encode_ready();
        Ok(())
    }

    /// Push interleaved samples in [`ec_core::ChannelLayout`] order.
    pub fn push_interleaved(&mut self, samples: &[f32]) -> Result<()> {
        let channels = usize::from(self.config.channels);
        if !samples.len().is_multiple_of(channels) {
            return Err(Error::corrupt("interleaved push is not a whole frame"));
        }
        if self.finished {
            return Err(Error::corrupt("samples pushed after finish"));
        }
        for (i, &value) in samples.iter().enumerate() {
            self.buffer[self.to_vorbis[i % channels]].push(value);
        }
        self.fed += (samples.len() / channels) as i64;
        self.encode_ready();
        Ok(())
    }

    /// No more input; pad the grid out and encode the tail.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        // Enough blocks for every input sample to sit left of a block centre,
        // and enough padding for that last block to be whole.
        let last = ((self.fed + HOP as i64 - 1) / HOP as i64).max(1) as usize;
        let needed = last * HOP + BLOCK;
        for channel in &mut self.buffer {
            if channel.len() < needed {
                channel.resize(needed, 0.0);
            }
        }
        self.encode_ready();
    }

    /// Take the next packet, or [`Error::Eof`] once the encoder is drained.
    pub fn next_packet(&mut self) -> Result<EncodedPacket> {
        match self.packets.pop_front() {
            Some(packet) => Ok(packet),
            None if self.finished => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    /// Encode every block the buffer now holds whole.
    ///
    /// Block `k` is centred on input sample `k * HOP`, and that centre is the
    /// granule: the decoder's output reaches exactly there when the block's
    /// window closes. The last block's granule is the input's own sample count
    /// instead, which is what trims the grid's overshoot off the file.
    fn encode_ready(&mut self) {
        let channels = usize::from(self.config.channels);
        loop {
            if self.finished && self.blocks > 0 && self.granule >= self.fed {
                break;
            }
            let start = self.blocks as usize * HOP;
            if self.buffer[0].len() < start + BLOCK {
                break;
            }
            let centre = self.blocks * HOP as i64;
            let granule = match self.finished {
                true => centre.min(self.fed),
                false => centre,
            };
            let data = self.encode_block(start, channels);
            self.packets.push_back(EncodedPacket {
                data,
                granule,
                samples: (granule - self.granule).max(0),
            });
            self.granule = granule;
            self.blocks += 1;
        }
    }

    /// One block: analyse, quantise, write.
    fn encode_block(&mut self, start: usize, channels: usize) -> Vec<u8> {
        let mut spectra: Vec<Vec<f32>> = Vec::with_capacity(channels);
        let mut block = vec![0.0f32; BLOCK];
        for channel in 0..channels {
            block.copy_from_slice(&self.buffer[channel][start..start + BLOCK]);
            let mut coefficients = vec![0.0f32; HALF];
            self.mdct
                .forward_windowed(&block, &self.window, &mut coefficients);
            for value in coefficients.iter_mut() {
                *value *= VORBIS_MDCT_SCALE;
            }
            spectra.push(coefficients);
        }

        // Floor per channel — one shared floor per coupled pair, so the two
        // normalised spectra are on the same scale and the angle channel is
        // small where the pair agrees.
        let mut curves: Vec<Vec<f32>> = vec![Vec::new(); channels];
        let mut floor_values: Vec<Vec<i32>> = vec![Vec::new(); channels];
        let mut coded = vec![false; channels];
        for &(magnitude, angle) in &self.coupling {
            let peaks = self.peaks_of(&[&spectra[magnitude], &spectra[angle]]);
            let (y, step2) = self.fit_floor(&peaks);
            let mut curve = vec![0.0f32; HALF];
            render_floor1(&self.floor, &y, &step2, &mut curve);
            curves[magnitude] = curve.clone();
            curves[angle] = curve;
            floor_values[magnitude] = y.clone();
            floor_values[angle] = y;
            coded[magnitude] = true;
            coded[angle] = true;
        }
        for channel in 0..channels {
            if coded[channel] {
                continue;
            }
            let peaks = self.peaks_of(&[&spectra[channel]]);
            let (y, step2) = self.fit_floor(&peaks);
            let mut curve = vec![0.0f32; HALF];
            render_floor1(&self.floor, &y, &step2, &mut curve);
            curves[channel] = curve;
            floor_values[channel] = y;
        }

        // Normalise, couple, quantise.
        let mut quantised: Vec<Vec<i32>> = (0..channels)
            .map(|channel| {
                spectra[channel]
                    .iter()
                    .zip(curves[channel].iter())
                    .map(|(&value, &floor)| match floor > 0.0 {
                        true => value / floor,
                        false => 0.0,
                    })
                    .map(|normalised| normalised.round() as i32)
                    .collect::<Vec<i32>>()
            })
            .collect();
        for &(magnitude, angle) in &self.coupling {
            // Taken out and put back so the pair can be walked in step; a
            // coupling step never names one channel twice.
            let mut left = std::mem::take(&mut quantised[magnitude]);
            let mut right = std::mem::take(&mut quantised[angle]);
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                let (m, a) = couple(*l, *r);
                *l = m;
                *r = a;
            }
            quantised[magnitude] = left;
            quantised[angle] = right;
        }
        let limit = CLASS_RANGE[CLASSES - 1];
        for channel in quantised.iter_mut() {
            for value in channel.iter_mut() {
                *value = (*value).clamp(-limit, limit);
            }
        }

        let mut out = BitsOut::new();
        out.write(0, 1);
        // One mode means `ilog(modes - 1)` is zero bits: there is nothing to
        // state. The two window flags follow because the mode is a long block.
        out.bit(true);
        out.bit(true);
        for values in floor_values.iter().take(channels) {
            self.write_floor(&mut out, values);
        }
        self.write_residue(&mut out, &quantised);
        let bits = out.len();
        self.update_rate(bits, channels);
        out.finish()
    }

    /// Peak magnitude around each floor X point, over every channel given.
    ///
    /// Each point owns the coefficients halfway to its neighbours either side,
    /// so the whole spectrum is covered exactly once and a peak between two
    /// points still raises the floor that has to carry it.
    fn peaks_of(&self, spectra: &[&Vec<f32>]) -> Vec<f64> {
        let x = &self.floor.x_list;
        x.iter()
            .map(|&centre| {
                let below = x.iter().filter(|&&o| o < centre).max().copied();
                let above = x.iter().filter(|&&o| o > centre).min().copied();
                let lo = below.map_or(0, |b| ((b + centre) / 2) as usize);
                let hi = above.map_or(HALF, |a| ((a + centre) / 2) as usize + 1);
                let lo = lo.min(HALF - 1);
                let hi = hi.clamp(lo + 1, HALF);
                let mut peak = 0.0f64;
                for spectrum in spectra {
                    for &value in &spectrum[lo..hi] {
                        peak = peak.max(f64::from(value.abs()));
                    }
                }
                peak
            })
            .collect()
    }

    /// Turn peak magnitudes into coded floor amplitudes.
    ///
    /// Spread first (a masker covers its neighbours), then subtract the rate
    /// loop's headroom, then hold the absolute threshold of hearing as a lower
    /// bound: below it the residue quantises to zero and costs a class-0
    /// partition.
    fn fit_floor(&self, peaks: &[f64]) -> (Vec<i32>, Vec<bool>) {
        let mut db: Vec<f64> = peaks
            .iter()
            .map(|&peak| 20.0 * (peak.max(1e-9)).log10())
            .collect();
        // Frequency order, not coding order: spreading is a fact about the ear.
        let order = &self.floor.sorted;
        for window in order.windows(2) {
            let (previous, current) = (window[0], window[1]);
            db[current] = db[current].max(db[previous] - 12.0);
        }
        for window in order.windows(2).rev() {
            let (current, next) = (window[0], window[1]);
            db[current] = db[current].max(db[next] - 24.0);
        }
        let target: Vec<f64> = db
            .iter()
            .zip(self.ath.iter())
            .map(|(&level, &threshold)| (level - self.headroom).max(threshold))
            .collect();
        let y: Vec<i32> = target
            .iter()
            .map(|&level| ((level / (140.0 / 256.0)).round() as i32 + 255).clamp(0, 255))
            .collect();
        self.fold_floor(&y)
    }

    /// Run the decoder's own amplitude synthesis forwards, so the curve the
    /// encoder normalises against is the curve the decoder will draw.
    fn fold_floor(&self, wanted: &[i32]) -> (Vec<i32>, Vec<bool>) {
        let values = wanted.len();
        let range = 256i32;
        let mut final_y = vec![0i32; values];
        let mut step2 = vec![false; values];
        final_y[0] = wanted[0];
        final_y[1] = wanted[1];
        step2[0] = true;
        step2[1] = true;
        for i in 2..values {
            let (low, high) = self.floor.neighbours[i];
            let predicted = predict(
                self.floor.x_list[low],
                final_y[low],
                self.floor.x_list[high],
                final_y[high],
                self.floor.x_list[i],
            );
            let value = fold(predicted, wanted[i], range);
            if value != 0 {
                step2[low] = true;
                step2[high] = true;
                step2[i] = true;
            }
            final_y[i] = wanted[i];
        }
        (final_y, step2)
    }

    /// Write one channel's floor: the two endpoints raw, the rest folded
    /// against their neighbours and Huffman coded.
    fn write_floor(&self, out: &mut BitsOut, y: &[i32]) {
        out.bit(true);
        out.write(y[0] as u32, 8);
        out.write(y[1] as u32, 8);
        for i in 2..y.len() {
            let (low, high) = self.floor.neighbours[i];
            let predicted = predict(
                self.floor.x_list[low],
                y[low],
                self.floor.x_list[high],
                y[high],
                self.floor.x_list[i],
            );
            let value = fold(predicted, y[i], 256);
            self.floor_book.write(out, value as usize);
        }
    }

    /// Write the residue: one type-2 interleaved vector over every channel,
    /// classified per partition and coded in one pass.
    fn write_residue(&self, out: &mut BitsOut, quantised: &[Vec<i32>]) {
        let channels = quantised.len();
        let total = HALF * channels;
        let mut interleaved = vec![0i32; total];
        for (i, slot) in interleaved.iter_mut().enumerate() {
            *slot = quantised[i % channels][i / channels];
        }
        let partitions = total / PARTITION;
        let classes: Vec<usize> = (0..partitions)
            .map(|partition| {
                let slice = &interleaved[partition * PARTITION..(partition + 1) * PARTITION];
                let peak = slice.iter().map(|v| v.abs()).max().unwrap_or(0);
                CLASS_RANGE
                    .iter()
                    .position(|&range| peak <= range)
                    .unwrap_or(CLASSES - 1)
            })
            .collect();
        let per_word = self.class_book.dimensions;
        let mut partition = 0usize;
        while partition < partitions {
            let mut word = 0usize;
            for i in 0..per_word {
                word = word * CLASSES + classes.get(partition + i).copied().unwrap_or(0);
            }
            self.class_book.write(out, word);
            for _ in 0..per_word {
                if partition >= partitions {
                    break;
                }
                let class = classes[partition];
                if class > 0 {
                    let book = &self.residue_books[class - 1];
                    let range = CLASS_RANGE[class];
                    for &value in &interleaved[partition * PARTITION..(partition + 1) * PARTITION] {
                        book.write(out, (value.clamp(-range, range) + range) as usize);
                    }
                }
                partition += 1;
            }
        }
    }

    /// Move the headroom so the running bitrate meets the target.
    ///
    /// One dB of headroom is about one sixth of a bit per coefficient, so the
    /// step is the bit error divided by that — a proportional loop that settles
    /// within a few blocks and is clamped either side so a transient cannot
    /// swing the whole file.
    fn update_rate(&mut self, bits: u64, channels: usize) {
        if self.config.bitrate_bps <= 0 {
            return;
        }
        let target =
            f64::from(self.config.bitrate_bps) * HOP as f64 / f64::from(self.config.sample_rate);
        let coefficients = (HALF * channels) as f64;
        let step = ((target - bits as f64) / (coefficients / 6.0)).clamp(-2.0, 2.0);
        // Negative headroom is not a mistake: it puts the floor *above* the
        // signal, which quantises the coefficients under it to zero and lets a
        // whole partition become class 0 for two bits. Without that a low target
        // cannot be reached at all — coding every coefficient with the shortest
        // codeword there is already costs about a bit each.
        self.headroom = (self.headroom + step).clamp(-24.0, 36.0);
    }
}

/// Forward square-polar coupling (§9.4.2 run backwards).
fn couple(left: i32, right: i32) -> (i32, i32) {
    match left > 0 {
        true => match left > right {
            true => (left, left - right),
            false => (right, left - right),
        },
        false => match right > left {
            true => (left, right - left),
            false => (right, right - left),
        },
    }
}

/// §7.2.2 `render_point`, which both sides of the floor coding need.
fn predict(x0: u32, y0: i32, x1: u32, y1: i32, x: u32) -> i32 {
    let dy = y1 - y0;
    let adx = (x1 - x0) as i32;
    let ady = dy.abs();
    let off = (ady * (x - x0) as i32) / adx;
    match dy < 0 {
        true => y0 - off,
        false => y0 + off,
    }
}

/// The exact inverse of §7.2.3's amplitude synthesis: the value to code so the
/// decoder lands on `wanted`.
fn fold(predicted: i32, wanted: i32, range: i32) -> i32 {
    let high_room = range - predicted;
    let low_room = predicted;
    let room = high_room.min(low_room) * 2;
    let delta = wanted - predicted;
    if delta == 0 {
        return 0;
    }
    if delta > 0 && 2 * delta < room {
        return 2 * delta;
    }
    if delta < 0 && -2 * delta - 1 < room {
        return -2 * delta - 1;
    }
    match high_room > low_room {
        true => wanted - predicted + low_room,
        false => predicted - wanted + high_room - 1,
    }
}

/// The floor-1 configuration this encoder writes and renders against.
fn build_floor() -> Floor1 {
    let mut sorted_x: Vec<u32> = Vec::with_capacity(FLOOR_POINTS);
    let mut last = 0u32;
    for i in 0..FLOOR_POINTS {
        let t = i as f64 / (FLOOR_POINTS - 1) as f64;
        // Log spacing over the band, nudged to stay strictly increasing at the
        // bottom where the grid is finer than one coefficient.
        let value = (1023f64).powf(t).round() as u32;
        last = value.max(last + 1);
        sorted_x.push(last);
    }
    // Coding order: endpoints first, then repeated bisection, so every value is
    // predicted from the two coded values that bracket it most closely.
    let mut x_list = vec![0u32, HALF as u32];
    let mut ranges = vec![(0usize, sorted_x.len())];
    while let Some((lo, hi)) = ranges.pop() {
        if lo >= hi {
            continue;
        }
        let mid = (lo + hi) / 2;
        x_list.push(sorted_x[mid]);
        ranges.insert(0, (lo, mid));
        ranges.insert(0, (mid + 1, hi));
    }
    let values = x_list.len();
    let mut sorted: Vec<usize> = (0..values).collect();
    sorted.sort_by_key(|&i| x_list[i]);
    let neighbours = (0..values)
        .map(|i| {
            let mut low = 0usize;
            let mut high = 0usize;
            let mut low_x = None;
            let mut high_x = None;
            for j in 0..i {
                if x_list[j] < x_list[i] && low_x.is_none_or(|b| x_list[j] > b) {
                    low = j;
                    low_x = Some(x_list[j]);
                }
                if x_list[j] > x_list[i] && high_x.is_none_or(|b| x_list[j] < b) {
                    high = j;
                    high_x = Some(x_list[j]);
                }
            }
            (low, high)
        })
        .collect();
    let partitions = (values - 2) / FLOOR_CLASS_DIM;
    Floor1 {
        partition_classes: vec![0; partitions],
        class_dimensions: vec![FLOOR_CLASS_DIM],
        class_subclasses: vec![0],
        class_masterbooks: vec![0],
        subclass_books: vec![vec![0]],
        multiplier: 1,
        x_list,
        sorted,
        neighbours,
    }
}

/// Folded floor amplitudes: mostly small corrections, occasionally an escape
/// near the top of the range.
fn design_floor_book() -> CodebookSpec {
    let weights: Vec<f64> = (0..256)
        .map(|v| (-(f64::from(v)) / 7.0).exp() + 0.002)
        .collect();
    CodebookSpec::huffman(&weights, Vec::new())
}

/// Class pairs, on the assumption that most partitions are silent.
fn design_class_book() -> CodebookSpec {
    let per_class = [0.55f64, 0.20, 0.15, 0.08, 0.02];
    let mut weights = Vec::with_capacity(CLASSES * CLASSES);
    for first in per_class {
        for second in per_class {
            weights.push(first * second);
        }
    }
    let mut spec = CodebookSpec::huffman(&weights, Vec::new());
    spec.dimensions = 2;
    spec
}

/// One book per non-silent class, Laplacian over its own range.
fn design_residue_books() -> Vec<CodebookSpec> {
    CLASS_RANGE[1..]
        .iter()
        .map(|&range| {
            let sigma = f64::from(range).max(1.0) / 2.5;
            let weights: Vec<f64> = (-range..=range)
                .map(|v| (-f64::from(v.abs()) / sigma).exp() + 1e-4)
                .collect();
            let values: Vec<f32> = (-range..=range).map(|v| v as f32).collect();
            CodebookSpec::huffman(&weights, values)
        })
        .collect()
}

/// Absolute threshold of hearing in dBFS, full scale taken as 96 dB SPL.
fn absolute_threshold(hz: f64) -> f64 {
    let khz = (hz / 1000.0).clamp(0.02, 20.0);
    let spl =
        3.64 * khz.powf(-0.8) - 6.5 * (-0.6 * (khz - 3.3).powi(2)).exp() + 0.001 * khz.powi(4);
    (spl - 96.0).clamp(-120.0, -10.0)
}

/// The three header packets, in order.
fn write_headers(
    config: &EncoderConfig,
    channels: usize,
    floor: &Floor1,
    floor_book: &CodebookSpec,
    class_book: &CodebookSpec,
    residue_books: &[CodebookSpec],
    coupling: &[(usize, usize)],
) -> Vec<Vec<u8>> {
    let mut ident = vec![1u8];
    ident.extend_from_slice(b"vorbis");
    ident.extend_from_slice(&0u32.to_le_bytes());
    ident.push(channels as u8);
    ident.extend_from_slice(&config.sample_rate.to_le_bytes());
    ident.extend_from_slice(&0i32.to_le_bytes());
    ident.extend_from_slice(&config.bitrate_bps.max(0).to_le_bytes());
    ident.extend_from_slice(&0i32.to_le_bytes());
    let log2 = |n: usize| n.trailing_zeros() as u8;
    ident.push(log2(BLOCK) | (log2(BLOCK) << 4));
    ident.push(1);

    let mut comment = vec![3u8];
    comment.extend_from_slice(b"vorbis");
    let vendor = b"ec-vorbis";
    comment.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    comment.extend_from_slice(vendor);
    comment.extend_from_slice(&0u32.to_le_bytes());
    comment.push(1);

    let mut out = BitsOut::new();
    for byte in [5u8].iter().chain(b"vorbis") {
        out.write(u32::from(*byte), 8);
    }
    let books: Vec<&CodebookSpec> = std::iter::once(floor_book)
        .chain(std::iter::once(class_book))
        .chain(residue_books.iter())
        .collect();
    out.write(books.len() as u32 - 1, 8);
    for book in &books {
        write_codebook(&mut out, book);
    }
    // One time-domain transform, stated as zero.
    out.write(0, 6);
    out.write(0, 16);
    // One floor, type 1.
    out.write(0, 6);
    out.write(1, 16);
    out.write(floor.partition_classes.len() as u32, 5);
    for _ in &floor.partition_classes {
        out.write(0, 4);
    }
    out.write(FLOOR_CLASS_DIM as u32 - 1, 3);
    out.write(0, 2);
    // Book number plus one; zero would mean "this subclass codes nothing".
    out.write(1, 8);
    out.write(0, 2);
    let range_bits = ilog(HALF as u32) - 1;
    out.write(range_bits, 4);
    for &x in floor.x_list.iter().skip(2) {
        out.write(x, range_bits);
    }
    // One residue, type 2, over every channel of the one submap.
    out.write(0, 6);
    out.write(2, 16);
    out.write(0, 24);
    out.write((HALF * channels) as u32, 24);
    out.write(PARTITION as u32 - 1, 24);
    out.write(CLASSES as u32 - 1, 6);
    out.write(1, 8);
    for class in 0..CLASSES {
        // Class 0 codes nothing at all; the rest have one book in pass 0.
        out.write(u32::from(class > 0), 3);
        out.write(0, 1);
    }
    for class in 1..CLASSES {
        out.write(class as u32 + 1, 8);
    }
    // One mapping, type 0.
    out.write(0, 6);
    out.write(0, 16);
    out.write(0, 1);
    match coupling.is_empty() {
        true => out.write(0, 1),
        false => {
            out.write(1, 1);
            out.write(coupling.len() as u32 - 1, 8);
            let field = ilog(channels as u32 - 1);
            for &(magnitude, angle) in coupling {
                out.write(magnitude as u32, field);
                out.write(angle as u32, field);
            }
        }
    }
    out.write(0, 2);
    out.write(0, 8);
    out.write(0, 8);
    out.write(0, 8);
    // One mode: a long block on mapping 0.
    out.write(0, 6);
    out.write(1, 1);
    out.write(0, 16);
    out.write(0, 16);
    out.write(0, 8);
    out.write(1, 1);
    let setup = out.finish();

    vec![ident, comment, setup]
}

/// One codebook in setup-header form: flat lengths, and a scalar lookup when
/// the book carries values.
fn write_codebook(out: &mut BitsOut, book: &CodebookSpec) {
    out.write(0x0056_4342, 24);
    out.write(book.dimensions as u32, 16);
    out.write(book.entries() as u32, 24);
    out.write(0, 1);
    out.write(0, 1);
    for &length in &book.lengths {
        out.write(u32::from(length) - 1, 5);
    }
    if book.values.is_empty() {
        out.write(0, 4);
        return;
    }
    // Lookup type 1 with one dimension: the entry number *is* the index, so
    // `minimum + i * delta` states every value in `ilog(entries)` bits each.
    out.write(1, 4);
    let minimum = book.values[0];
    out.write(float32_pack(minimum), 32);
    out.write(float32_pack(1.0), 32);
    let value_bits = ilog(book.entries() as u32 - 1).max(1);
    out.write(value_bits - 1, 4);
    out.write(0, 1);
    for i in 0..book.entries() {
        out.write(i as u32, value_bits);
    }
}

/// Xiph lacing of the three headers, the form containers carry them in.
fn lace(headers: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![2u8];
    for header in headers.iter().take(headers.len() - 1) {
        let mut length = header.len();
        while length >= 255 {
            out.push(255);
            length -= 255;
        }
        out.push(length as u8);
    }
    for header in headers {
        out.extend_from_slice(header);
    }
    out
}

impl Encoder for VorbisEncoder {
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
        let channels = usize::from(self.config.channels);
        let mut planes: Vec<Vec<f32>> = vec![Vec::with_capacity(audio.samples); channels];
        match audio.planar {
            true => {
                for (channel, plane) in audio.data.iter().enumerate().take(channels) {
                    planes[channel] = floats(plane, audio.samples);
                }
            }
            false => {
                let interleaved = floats(&audio.data[0], audio.samples * channels);
                for (i, value) in interleaved.into_iter().enumerate() {
                    planes[i % channels].push(value);
                }
            }
        }
        let borrowed: Vec<&[f32]> = planes.iter().map(|p| &p[..]).collect();
        self.push_planar(&borrowed)
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        let encoded = self.next_packet()?;
        let base = TimeBase::new(1, i64::from(self.config.sample_rate));
        let mut packet = Packet::new(0, base, encoded.data);
        packet.pts = Some(encoded.granule - encoded.samples);
        packet.duration = Some(encoded.samples);
        Ok(packet)
    }

    fn flush(&mut self) -> Result<()> {
        self.finish();
        Ok(())
    }
}

/// Little-endian floats out of a plane.
fn floats(plane: &Buf, count: usize) -> Vec<f32> {
    plane
        .chunks_exact(4)
        .take(count)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_is_the_exact_inverse_of_the_decoder() {
        // Every predicted/wanted pair in the range must survive the fold, or
        // the encoder and the decoder disagree about the floor.
        for predicted in [0i32, 1, 40, 128, 200, 255] {
            for wanted in 0..256i32 {
                let value = fold(predicted, wanted, 256);
                assert!((0..256).contains(&value), "{predicted}->{wanted}: {value}");
                assert_eq!(
                    unfold(predicted, value, 256),
                    wanted,
                    "predicted {predicted}, wanted {wanted}, coded {value}"
                );
            }
        }
    }

    /// §7.2.3 as the decoder runs it, for the test above to check against.
    fn unfold(predicted: i32, value: i32, range: i32) -> i32 {
        let high_room = range - predicted;
        let low_room = predicted;
        let room = high_room.min(low_room) * 2;
        if value == 0 {
            return predicted;
        }
        match value >= room {
            true => match high_room > low_room {
                true => value - low_room + predicted,
                false => predicted - value + high_room - 1,
            },
            false => match value & 1 == 1 {
                true => predicted - (value + 1) / 2,
                false => predicted + value / 2,
            },
        }
    }

    #[test]
    fn coupling_round_trips_every_sign_case() {
        for left in -8..=8i32 {
            for right in -8..=8i32 {
                let (m, a) = couple(left, right);
                let (back_l, back_r) = match (m > 0, a > 0) {
                    (true, true) => (m, m - a),
                    (true, false) => (m + a, m),
                    (false, true) => (m, m + a),
                    (false, false) => (m - a, m),
                };
                assert_eq!((back_l, back_r), (left, right), "{left},{right}");
            }
        }
    }
}
