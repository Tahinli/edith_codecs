//! The encoder: PCM in, Vorbis packets out, with a setup header written for the
//! stream's own channel layout.
//!
//! ## What the analysis actually is
//!
//! Per block, per channel: an MDCT, a peak magnitude per floor point, a
//! spreading function over those points, a Bark-domain near-band masker under
//! it, an absolute threshold of hearing under that, and a headroom the rate
//! loop moves. The floor
//! that comes out is the quantiser step: the residue is the spectrum divided by
//! the floor and rounded, so headroom in dB *is* precision in bits. That is
//! psychoacoustics-lite and it is stated as such — there is no tonality
//! estimate beyond a two-mode block switch and no per-band bit allocation
//! beyond what the floor implies.
//!
//! ## Block switching (§1.3.2 / §4.3)
//!
//! Two modes: a 256-sample short block and a 2048-sample long block. Steady
//! content codes long blocks only. A transient — energy in a 128-sample tick
//! jumping past its predecessor, which is also what an onset after digital
//! silence looks like — swaps one pair of would-be long blocks for a
//! long-with-short-right-window (the block that would otherwise carry the
//! pre-echo), eight short blocks spanning the transient itself, and a
//! long-with-short-left-window resuming steady state. The granule advance
//! across that whole run is exactly two long hops, so the switch never moves
//! the steady-state grid.
//!
//! ## Codebooks
//!
//! Designed here rather than inherited: a floor book over the folded amplitude
//! range, a class book over partition-class pairs, and eight residue books whose
//! ranges (+-1, +-2, +-4 ... +-64, +-127) the partition classifier picks between —
//! a partition's values sit about a sixth of its class's range on average,
//! so one book per octave of range keeps every value near the top of its
//! own book. All of
//! them are Huffman codes over stated distributions, built at construction and
//! written into every stream's own setup header — which is why this encoder
//! needs no embedded profile and has no channel count it cannot serve. Both
//! block sizes share these books; only the floor's X grid and the residue's
//! partition size are sized per block.

use ec_core::{
    AudioParameters, Buf, CodecId, CodecParameters, Encoder, Error, Frame, MediaParameters, Packet,
    Result, SampleFormat, TimeBase,
};
use ec_dsp::Mdct;

use crate::bits::{BitsOut, float32_pack};
use crate::codebook::{CodebookSpec, ilog, lookup1_values};
use crate::decode::channel_map;
use crate::floor::render_floor1;
use crate::setup::Floor1;
use crate::window::Windows;

/// Long block size.
const BLOCK_LONG: usize = 2048;
/// Short block size.
const BLOCK_SHORT: usize = 256;
/// Coefficients in a long block.
const HALF_LONG: usize = BLOCK_LONG / 2;
/// Coefficients in a short block.
const HALF_SHORT: usize = BLOCK_SHORT / 2;
/// Long-block coefficients per residue partition.
const PARTITION_LONG: usize = 16;
/// Short-block coefficients per residue partition.
const PARTITION_SHORT: usize = 16;
/// Long floor X points besides the two endpoints. Fewer is better here, on
/// real music at 128 kbps: 12 beat 16, 20, 32 and 44 in that order, since a
/// finer floor costs bits of its own and then follows every peak closely
/// enough to spend residue bits under all of them.
const FLOOR_POINTS_LONG: usize = 12;
/// Short floor X points besides the two endpoints.
const FLOOR_POINTS_SHORT: usize = 8;
/// Floor values per class.
const FLOOR_CLASS_DIM: usize = 4;
/// Residue classes; class 0 codes nothing at all.
const CLASSES: usize = 9;
/// Largest quantised residue each class's book can state.
const CLASS_RANGE: [i32; CLASSES] = [0, 1, 2, 4, 8, 16, 32, 64, 127];
/// Residue codebook dimensions by class; class 0 codes no residue.
const RESIDUE_BOOK_DIM: [usize; CLASSES] = [0, 4, 2, 2, 1, 1, 1, 1, 1];
/// The vector books save packet bits but add setup and make the rate loop spend
/// harder; this keeps the realised rate at the caller's target.
const RATE_TARGET_SCALE: f64 = 0.97;
/// Headroom in dB the widest residue book can actually state: 20 log10 127
/// is 42, but a coupled angle channel is a difference of two magnitudes, so a
/// pair's step must leave half that range — 63, 36 dB. Past this the rate loop's extra headroom lowers the absolute-threshold
/// bound instead, so sparse or quiet content — where most partitions sit under
/// the threshold and code as class 0 whatever the step — gains bins to spend
/// bits on rather than a finer step nothing can represent.
const HEADROOM_RANGE: f64 = 36.0;
/// A short block's range: the full 42 dB. Its step is what sets the pre-echo
/// left inside the block that carries an onset, and 6 dB of it is the
/// difference between a leak twice the reference's and one under it.
/// Anything the wider range cannot state (a pair's angle, a peak at a floor
/// region's edge) is caught by the clamp refit in `encode_block`.
const HEADROOM_RANGE_SHORT: f64 = 42.0;
/// Nothing more than this many dB under a block's loudest floor point is
/// coded: a whole-spectrum co-masking cap after local spreading. The Bark
/// curve keeps nearby bands honest; this cap only stops very quiet distant
/// bins from spending bits below the noise floor at these rates. Swept on
/// three tracks at 128 kbps: 50 dB kept the accuracy win without inflating the
/// files.
const CO_MASK_RANGE: f64 = 50.0;
/// Extra dB above the spread threshold. This is the single long-block
/// rate/quality knob: raising the curve zeroes masked coefficients earlier,
/// while peaks still ride as residue above it.
const MASKING_OFFSET_DB: f64 = 9.0;
/// All-zero partition guard: if the masked band still has a bin within this
/// fraction of the floor, keep that bin as one sign-only residue sample.
const NOISE_NORMALISE_MIN_RATIO: f32 = 0.5;
/// A short block gets no co-masking cap: a bin it drops is an error spread
/// across its whole window, the half before an onset included, and that is
/// the pre-echo the short block exists to prevent (44.1k mono, onset on the
/// grid: peak 0.0085 with the cap, under 0.007 without).
const CO_MASK_RANGE_SHORT: f64 = f64::INFINITY;
/// Above this frequency the coupled pair's angle is dropped and only the
/// magnitude coded (point stereo): the ear does not place HF detail by
/// inter-channel level difference finely enough to pay for coding it, and at
/// this rate the bits buy more accuracy in the magnitude. Below it the pair
/// is coded losslessly. Swept on real music at 128 kbps: 3-6 kHz are equal,
/// 10 kHz and no cutoff are worse.
const POINT_STEREO_HZ: f64 = 4_000.0;
/// Least headroom a short block is quantised with, whatever the rate loop's.
const HEADROOM_SHORT_MIN: f64 = 42.0;
/// Samples per transient-detection tick — the short block's own hop, so a
/// tick lines up with the finest time resolution the encoder has.
const TICK: usize = BLOCK_SHORT / 4;
/// Short blocks a detected transient inserts between the two transition long
/// blocks. Detection looks one long hop ahead of the transition-out
/// candidate, so the transient can land anywhere in that hop; the run is
/// sized to three long hops' worth of short-hop resolution (any `8 + 8k`
/// count keeps the steady-state grid aligned) so it comfortably straddles a
/// transient found anywhere in the hop being watched, not just at its start.
const SHORT_RUN: u8 = 16;
/// Mode index the setup header and every packet agree the short mode is.
const MODE_SHORT: u32 = 0;
/// Mode index the setup header and every packet agree the long mode is.
const MODE_LONG: u32 = 1;
/// A tick energy jump past this multiple of its predecessor is a transient;
/// a near-zero predecessor makes this trip on any onset above the floor.
const TRANSIENT_RATIO: f64 = 14.0;
/// Below this per-sample mean square (-60 dBFS), a tick is not "content" at
/// all — the floor that keeps digital silence, dither and fade tails from
/// tripping the detector.
const TRANSIENT_FLOOR: f64 = 1e-6;
/// Ticks before the candidate whose loudest one the jump is measured from.
const TRANSIENT_HISTORY: usize = 4;

/// Buffer samples of left pre-roll before input sample 0 — one long half, so
/// even a block starting at the very front of the stream finds zeroes rather
/// than reading off the front of the buffer.
const PREROLL: usize = HALF_LONG;

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

/// One block's shape: its mode and, for a long block, the window flags §4.3
/// states beside it.
#[derive(Debug, Clone, Copy)]
struct BlockPlan {
    is_long: bool,
    prev_long: bool,
    next_long: bool,
}

/// The block-switch scheduler's state between calls.
#[derive(Debug, Clone, Copy)]
enum Sched {
    /// Long blocks, deciding fresh each time whether the next one is a
    /// transient's transition-out.
    Steady,
    /// Short blocks left to emit, counting this call down to zero.
    ShortRun(u8),
    /// The long block that resumes steady state after a short run.
    TransitionIn,
}

/// A block size's own floor, codebook-sharing residue geometry and masking data.
struct BlockConfig {
    half: usize,
    partition: usize,
    floor: Floor1,
    ath: Vec<f64>,
    bark: Vec<f64>,
    /// Headroom the residue range lets this block size state.
    range: f64,
    /// dB under the loudest floor point below which nothing is coded.
    co_mask: f64,
}

/// Vorbis I encoder.
pub struct VorbisEncoder {
    config: EncoderConfig,
    params: CodecParameters,
    /// `to_vorbis[ec channel] = vorbis channel`.
    to_vorbis: Vec<usize>,
    headers: Vec<Vec<u8>>,
    long: BlockConfig,
    short: BlockConfig,
    floor_book: CodebookSpec,
    class_book: CodebookSpec,
    residue_books: Vec<CodebookSpec>,
    /// Channel pairs coupled by the mapping; empty unless stereo.
    coupling: Vec<(usize, usize)>,
    mdct_long: Mdct<f32>,
    mdct_short: Mdct<f32>,
    windows: Windows,
    /// Per Vorbis channel, input samples with [`PREROLL`] zeroes already in
    /// front.
    buffer: Vec<Vec<f32>>,
    /// Input samples taken from the caller.
    fed: i64,
    /// Blocks already emitted.
    blocks_emitted: i64,
    /// Centre of the block emitted last; the sentinel one long hop before
    /// sample 0, so the first real centre falls out of the same recurrence
    /// every later one uses.
    centre: i64,
    /// Blocksize of the block emitted last.
    prev_n: usize,
    /// Granule of the packet emitted last.
    granule: i64,
    scheduler: Sched,
    /// Headroom under the masking curve, in dB — the rate loop's variable.
    headroom: f64,
    /// Bits the forced-precision blocks spent past their share, repaid by the
    /// steady blocks that follow (see [`VorbisEncoder::update_rate`]).
    reservoir_debt: f64,
    finished: bool,
    packets: std::collections::VecDeque<EncodedPacket>,
    /// Per-block captured quantised residue: (half, per-channel quantised).
    /// Populated only when `enable_residue_capture` is set.
    residue_capture: Vec<(usize, Vec<Vec<i32>>)>,
    /// When true, `encode_block` copies each block's quantised residue into
    /// `residue_capture` for offline histogram analysis.
    enable_residue_capture: bool,
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

        let floor_book = design_floor_book();
        let class_book = design_class_book();
        let residue_books = design_residue_books();
        let long = build_block_config(
            HALF_LONG,
            PARTITION_LONG,
            FLOOR_POINTS_LONG,
            HEADROOM_RANGE,
            CO_MASK_RANGE,
            &config,
        );
        let short = build_block_config(
            HALF_SHORT,
            PARTITION_SHORT,
            FLOOR_POINTS_SHORT,
            HEADROOM_RANGE_SHORT,
            CO_MASK_RANGE_SHORT,
            &config,
        );

        let headers = write_headers(
            &config,
            channels,
            HeaderSetup {
                long: &long,
                short: &short,
                floor_book: &floor_book,
                class_book: &class_book,
                residue_books: &residue_books,
                coupling: &coupling,
            },
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
            mdct_long: Mdct::new(BLOCK_LONG),
            mdct_short: Mdct::new(BLOCK_SHORT),
            windows: Windows::new(BLOCK_SHORT, BLOCK_LONG),
            buffer: vec![vec![0.0; PREROLL]; channels],
            fed: 0,
            blocks_emitted: 0,
            centre: -(HALF_LONG as i64),
            prev_n: BLOCK_LONG,
            granule: 0,
            scheduler: Sched::Steady,
            headroom: 4.0 + 26.0 * quality,
            reservoir_debt: 0.0,
            finished: false,
            packets: std::collections::VecDeque::new(),
            config,
            params,
            to_vorbis,
            headers,
            long,
            short,
            floor_book,
            class_book,
            residue_books,
            coupling,
            residue_capture: Vec::new(),
            enable_residue_capture: false,
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

    /// Enable per-block quantised-residue capture for offline histogram analysis.
    pub fn enable_residue_capture(&mut self) {
        self.enable_residue_capture = true;
    }

    /// Take the captured per-block quantised residue: each entry is `(half, per-channel quantised)`.
    pub fn take_residue_capture(&mut self) -> Vec<(usize, Vec<Vec<i32>>)> {
        std::mem::take(&mut self.residue_capture)
    }

    /// Frequency in Barks for a given Hz — public so tests can bin by band.
    pub fn bark_hz(hz: f64) -> f64 {
        bark(hz)
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
        // Generous padding: a long block's own half plus a whole short-run's
        // worth of slack, so whatever the scheduler is doing when the input
        // runs out still finds real (zero) samples rather than the buffer's
        // edge.
        let needed = PREROLL + self.fed.max(0) as usize + 2 * BLOCK_LONG;
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

    /// Decide the next block's shape without committing the scheduler state;
    /// used to check whether the data it needs is buffered yet.
    fn peek_plan(&self) -> BlockPlan {
        match self.scheduler {
            Sched::Steady => BlockPlan {
                is_long: true,
                prev_long: true,
                next_long: !self.transient_ahead(),
            },
            Sched::ShortRun(_) => BlockPlan {
                is_long: false,
                prev_long: true,
                next_long: true,
            },
            Sched::TransitionIn => BlockPlan {
                is_long: true,
                prev_long: false,
                next_long: true,
            },
        }
    }

    /// Advance the scheduler past the plan [`peek_plan`] just returned.
    fn commit_plan(&mut self, plan: BlockPlan) {
        self.scheduler = match self.scheduler {
            Sched::Steady if !plan.next_long => Sched::ShortRun(SHORT_RUN),
            Sched::Steady => Sched::Steady,
            Sched::ShortRun(remaining) if remaining > 1 => Sched::ShortRun(remaining - 1),
            Sched::ShortRun(_) => Sched::TransitionIn,
            Sched::TransitionIn => Sched::Steady,
        };
    }

    /// Whether the *next* steady long block's window carries a transient —
    /// checked one hop ahead of the block being decided now, so the block
    /// this call is about to type as transition-out still finishes its own
    /// carried region (up to a short hop past its centre) safely before the
    /// transient it is making room for. A tick's energy jumping past its
    /// predecessor's is exactly what an onset after digital silence looks
    /// like when the predecessor is zero.
    fn transient_ahead(&self) -> bool {
        // The very first block's "predecessor" is the encoder's own silent
        // pre-roll, not a real signal boundary; nothing to detect there.
        if self.centre == -(HALF_LONG as i64) {
            return false;
        }
        let candidate_centre = self.centre + (self.prev_n + BLOCK_LONG) as i64 / 4;
        let lookahead_centre = candidate_centre + BLOCK_LONG as i64 / 2;
        let window_start = lookahead_centre - HALF_LONG as i64;
        let buffer_start = window_start + PREROLL as i64;
        if buffer_start < 0 || self.buffer[0].len() < (buffer_start as usize) + BLOCK_LONG {
            return false;
        }
        let ticks = BLOCK_LONG / TICK;
        let energy = |t: usize| -> f64 {
            let start = buffer_start as usize + t * TICK;
            let mut sum = 0.0f64;
            for channel in &self.buffer {
                for &s in &channel[start..start + TICK] {
                    sum += f64::from(s) * f64::from(s);
                }
            }
            sum
        };
        // Only the right half is new; its left neighbour was already the
        // right half the last time this block's window was checked.
        let floor = TRANSIENT_FLOOR * (TICK * self.buffer.len()) as f64;
        for t in ticks / 2..ticks {
            // Against the loudest of the few ticks before it, not just the
            // one: music rising over a few ticks is not a transient, a drum
            // hit or an onset after silence is.
            let previous = (t.saturating_sub(TRANSIENT_HISTORY)..t)
                .map(energy)
                .fold(0.0f64, f64::max);
            let current = energy(t);
            if current > floor && current > previous * TRANSIENT_RATIO {
                return true;
            }
        }
        false
    }

    /// Encode every block the buffer now holds whole.
    fn encode_ready(&mut self) {
        let channels = usize::from(self.config.channels);
        loop {
            if self.finished && self.blocks_emitted > 0 && self.granule >= self.fed {
                break;
            }
            let plan = self.peek_plan();
            let n = if plan.is_long {
                BLOCK_LONG
            } else {
                BLOCK_SHORT
            };
            let half = if plan.is_long { HALF_LONG } else { HALF_SHORT };
            let centre = self.centre + (self.prev_n + n) as i64 / 4;
            let window_start = centre - (n / 2) as i64;
            let buffer_start = window_start + PREROLL as i64;
            if buffer_start < 0 || self.buffer[0].len() < buffer_start as usize + n {
                break;
            }
            // A steady long block is typed by the hop *after* it; until that
            // hop is buffered the plan just peeked was decided blind.
            if !self.finished
                && matches!(self.scheduler, Sched::Steady)
                && self.buffer[0].len() < buffer_start as usize + n + BLOCK_LONG
            {
                break;
            }
            let granule = match self.finished {
                true => centre.min(self.fed),
                false => centre,
            };
            let data = self.encode_block(buffer_start as usize, n, half, plan, channels);
            self.packets.push_back(EncodedPacket {
                data,
                granule,
                samples: (granule - self.granule).max(0),
            });
            self.granule = granule;
            self.centre = centre;
            self.prev_n = n;
            self.blocks_emitted += 1;
            self.commit_plan(plan);
        }
    }

    /// One block: analyse, quantise, write.
    fn encode_block(
        &mut self,
        start: usize,
        n: usize,
        half: usize,
        plan: BlockPlan,
        channels: usize,
    ) -> Vec<u8> {
        let bc = if plan.is_long {
            &self.long
        } else {
            &self.short
        };
        let partition = bc.partition;
        let scale = 2.0 / half as f32;
        let window = self
            .windows
            .get(plan.is_long, plan.prev_long, plan.next_long)
            .to_vec();

        let mut spectra: Vec<Vec<f32>> = Vec::with_capacity(channels);
        let mut block = vec![0.0f32; n];
        for channel in 0..channels {
            block.copy_from_slice(&self.buffer[channel][start..start + n]);
            let mut coefficients = vec![0.0f32; half];
            let mdct = if plan.is_long {
                &mut self.mdct_long
            } else {
                &mut self.mdct_short
            };
            mdct.forward_windowed(&block, &window, &mut coefficients);
            for value in coefficients.iter_mut() {
                *value *= scale;
            }
            spectra.push(coefficients);
        }

        // Floor per channel — one shared floor per coupled pair, so the two
        // normalised spectra are on the same scale and the angle channel is
        // small where the pair agrees. The floor is set per X point from the
        // peak of the region that point owns, then drawn as straight lines
        // between points: a peak at a region's edge can sit further under the
        // curve than the range states and clamp. Such a block lifts the two
        // points either side of every clamping bin by the excess and refits
        // until nothing clamps, so the curve hugs the peaks the range is
        // measured from.
        let limit = CLASS_RANGE[CLASSES - 1];
        // Blocks next to short windows bound pre-echo too: the transition-out
        // long block still overlaps the samples just before the onset, so high
        // bitrate transients get extra minimum precision.
        let transient_headroom = if self.config.bitrate_bps >= 128_000 {
            54.0
        } else {
            HEADROOM_SHORT_MIN
        };
        let headroom = match plan.is_long && plan.prev_long && plan.next_long {
            true => self.headroom,
            false => self.headroom.max(transient_headroom),
        };
        let mut lift: Vec<Vec<f64>> = vec![vec![1.0; bc.floor.x_list.len()]; channels];
        // Per X point, the span of bins whose curve it has a hand in.
        let spans: Vec<(usize, usize)> = bc
            .floor
            .x_list
            .iter()
            .map(|&x| {
                let below = bc
                    .floor
                    .x_list
                    .iter()
                    .filter(|&&o| o < x)
                    .max()
                    .map_or(0, |&o| o as usize);
                let above = bc
                    .floor
                    .x_list
                    .iter()
                    .filter(|&&o| o > x)
                    .min()
                    .map_or(half, |&o| o as usize);
                (below, above)
            })
            .collect();
        let mut passes = 0;
        let steady = plan.is_long && plan.prev_long && plan.next_long;
        let masking_offset = if !steady && self.config.bitrate_bps >= 128_000 {
            0.0
        } else {
            MASKING_OFFSET_DB
        };
        let (floor_values, quantised) = loop {
            let mut curves: Vec<Vec<f32>> = vec![Vec::new(); channels];
            let mut floor_values: Vec<Vec<i32>> = vec![Vec::new(); channels];
            let mut fit = |group: &[usize]| {
                let group_spectra: Vec<&Vec<f32>> = group.iter().map(|&c| &spectra[c]).collect();
                let mut peaks = peaks_of(&bc.floor, half, &group_spectra);
                for &c in group {
                    for (peak, &boost) in peaks.iter_mut().zip(&lift[c]) {
                        *peak *= boost;
                    }
                }
                let (y, step2) = fit_floor(
                    &bc.floor,
                    &bc.ath,
                    &bc.bark,
                    headroom,
                    bc.range,
                    bc.co_mask,
                    masking_offset,
                    &peaks,
                );
                let mut curve = vec![0.0f32; half];
                render_floor1(&bc.floor, &y, &step2, &mut curve);
                for &c in group {
                    curves[c] = curve.clone();
                    floor_values[c] = y.clone();
                }
            };
            let mut coded = vec![false; channels];
            for &(magnitude, angle) in &self.coupling {
                fit(&[magnitude, angle]);
                coded[magnitude] = true;
                coded[angle] = true;
            }
            for channel in (0..channels).filter(|&c| !coded[c]) {
                fit(&[channel]);
            }

            // Normalise, couple, quantise. The floor is the masking threshold:
            // bins that do not clear it are inaudible here and code as zero;
            // bins that do clear it keep their full rounded residue, producing
            // sparse small classes and larger tonal residues.
            let mut quantised: Vec<Vec<i32>> = (0..channels)
                .map(|channel| {
                    spectra[channel]
                        .iter()
                        .zip(curves[channel].iter())
                        .map(|(&value, &floor)| {
                            if floor <= 0.0 || value.abs() <= floor {
                                0
                            } else {
                                (value / floor).round() as i32
                            }
                        })
                        .collect::<Vec<i32>>()
                })
                .collect();
            for channel in 0..channels {
                for (band, values) in quantised[channel].chunks_mut(partition).enumerate() {
                    if values.iter().any(|&value| value != 0) {
                        continue;
                    }
                    let start = band * partition;
                    let end = (start + values.len()).min(half);
                    let mut peak = (NOISE_NORMALISE_MIN_RATIO, 0usize, 0i32);
                    for bin in start..end {
                        let floor = curves[channel][bin];
                        if floor <= 0.0 {
                            continue;
                        }
                        let value = spectra[channel][bin];
                        let ratio = value.abs() / floor;
                        if ratio > peak.0 {
                            peak = (ratio, bin - start, value.signum() as i32);
                        }
                    }
                    if peak.2 != 0 {
                        values[peak.1] = peak.2;
                    }
                }
            }
            for &(magnitude, angle) in &self.coupling {
                let mut left = std::mem::take(&mut quantised[magnitude]);
                let mut right = std::mem::take(&mut quantised[angle]);
                let cutoff = if self.config.bitrate_bps >= 128_000 {
                    6_000.0
                } else {
                    POINT_STEREO_HZ
                };
                let point = match self.headroom < bc.range {
                    true => (cutoff / (f64::from(self.config.sample_rate) * 0.5) * half as f64)
                        as usize,
                    false => half,
                };
                for (bin, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
                    let (m, a) = couple(*l, *r);
                    *l = m;
                    *r = if bin < point {
                        a
                    } else {
                        let delta = (spectra[magnitude][bin] - spectra[angle][bin]).abs();
                        if delta > curves[magnitude][bin].max(curves[angle][bin]) {
                            a.signum()
                        } else {
                            0
                        }
                    };
                }
                quantised[magnitude] = left;
                quantised[angle] = right;
            }
            let mut clamps = false;
            for (channel, values) in quantised.iter().enumerate() {
                for (bin, &value) in values.iter().enumerate() {
                    if value.abs() <= limit {
                        continue;
                    }
                    clamps = true;
                    let excess = f64::from(value.abs()) / f64::from(limit) * 1.05;
                    for (point, &(below, above)) in spans.iter().enumerate() {
                        if bin >= below && bin <= above {
                            lift[channel][point] = lift[channel][point].max(excess);
                        }
                    }
                }
            }
            passes += 1;
            // Eight lifts is 8 x 6 dB past anything a 42 dB range can leave over.
            if !clamps || passes == 8 {
                for channel in quantised.iter_mut() {
                    for value in channel.iter_mut() {
                        *value = (*value).clamp(-limit, limit);
                    }
                }
                break (floor_values, quantised);
            }
        };
        if self.enable_residue_capture {
            // The bitstream stores the coupled magnitude/angle form; the
            // decoder inverse-couples before floor multiply.  Capture in that
            // same per-channel domain so the histogram matches the decoder-
            // side reference capture exactly (sanity: decoding our own .ogg
            // reproduces these vectors).
            // Main codes the full `half * channels` interleaved region (no
            // HF truncation), so every bin below `half` is coded and the
            // capture keeps the whole per-channel vector.
            let coded = half;
            let mut per_channel: Vec<Vec<i32>> = quantised
                .iter()
                .map(|ch| {
                    let mut v = ch[..coded].to_vec();
                    v.resize(half, 0);
                    v
                })
                .collect();
            for &(magnitude, angle) in &self.coupling {
                let mut mags = std::mem::take(&mut per_channel[magnitude]);
                let mut angs = std::mem::take(&mut per_channel[angle]);
                for (m, a) in mags.iter_mut().zip(angs.iter_mut()) {
                    let (l, r) = decouple(*m, *a);
                    *m = l;
                    *a = r;
                }
                per_channel[magnitude] = mags;
                per_channel[angle] = angs;
            }
            self.residue_capture.push((half, per_channel));
        }

        let mut out = BitsOut::new();
        out.bit(false);
        let mode = if plan.is_long { MODE_LONG } else { MODE_SHORT };
        out.write(mode, 1);
        if plan.is_long {
            out.bit(plan.prev_long);
            out.bit(plan.next_long);
        }
        for values in floor_values.iter().take(channels) {
            write_floor(&mut out, &bc.floor, &self.floor_book, values);
        }
        write_residue(
            &mut out,
            &quantised,
            half,
            partition,
            &self.class_book,
            &self.residue_books,
        );
        let bits = out.len();
        let delta = (self.centre + (self.prev_n + n) as i64 / 4) - self.centre;
        let steady = plan.is_long && plan.prev_long && plan.next_long;
        self.update_rate(bits, half, channels, delta, steady);
        out.finish()
    }

    /// Move the headroom so the running bitrate meets the target.
    ///
    /// One dB of headroom is about one sixth of a bit per coefficient, so the
    /// step is the bit error divided by that — a proportional loop that settles
    /// within a few blocks and is clamped either side so a transient cannot
    /// swing the whole file. The target itself scales with this block's own
    /// granule advance, so a short block (a small fraction of a long hop) is
    /// judged against a proportionally small bit budget. The ceiling is well
    /// past [`HEADROOM_RANGE`]: beyond it headroom buys bins under the
    /// threshold rather than step, which is what quiet content needs.
    ///
    /// Blocks next to a short window are quantised with a forced minimum
    /// precision the loop cannot lower, so their overspend must not step the
    /// headroom: a run of 16–32 short blocks at −2 dB each drove it to the
    /// −24 dB floor, which put the floor above the signal and emitted ~10
    /// silent long blocks (200 ms dropouts at −20 dBFS) after every
    /// transient. Their excess goes into a reservoir debt instead, repaid by
    /// the steady blocks that follow at no more than a quarter of each block's
    /// share, so the rate still lands on target over about a second.
    fn update_rate(&mut self, bits: u64, half: usize, channels: usize, delta: i64, steady: bool) {
        if self.config.bitrate_bps <= 0 || delta <= 0 {
            return;
        }
        let target = f64::from(self.config.bitrate_bps) * RATE_TARGET_SCALE * delta as f64
            / f64::from(self.config.sample_rate);
        if !steady {
            self.reservoir_debt += bits as f64 - target;
            return;
        }
        let repay = self.reservoir_debt.clamp(-target * 0.25, target * 0.25);
        self.reservoir_debt -= repay;
        let coefficients = (half * channels) as f64;
        let step = ((target - repay - bits as f64) / (coefficients / 6.0)).clamp(-2.0, 2.0);
        // Negative headroom is not a mistake: it puts the floor *above* the
        // signal, which quantises the coefficients under it to zero and lets a
        // whole partition become class 0 for two bits. Without that a low target
        // cannot be reached at all — coding every coefficient with the shortest
        // codeword there is already costs about a bit each.
        self.headroom = (self.headroom + step).clamp(-24.0, 84.0);
    }
}

/// Peak magnitude around each floor X point, over every channel given.
///
/// Each point owns the coefficients halfway to its neighbours either side, so
/// the whole spectrum is covered exactly once and a peak between two points
/// still raises the floor that has to carry it.
fn peaks_of(floor: &Floor1, half: usize, spectra: &[&Vec<f32>]) -> Vec<f64> {
    let x = &floor.x_list;
    x.iter()
        .map(|&centre| {
            let below = x.iter().filter(|&&o| o < centre).max().copied();
            let above = x.iter().filter(|&&o| o > centre).min().copied();
            let lo = below.map_or(0, |b| ((b + centre) / 2) as usize);
            let hi = above.map_or(half, |a| ((a + centre) / 2) as usize + 1);
            let lo = lo.min(half - 1);
            let hi = hi.clamp(lo + 1, half);
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
/// Spread first (a masker covers its neighbours, with an extra Bark-local
/// near-band pass), then subtract the rate loop's headroom, then hold the
/// absolute threshold of hearing as a lower bound: below it the residue
/// quantises to zero and costs a class-0 partition.
fn fit_floor(
    floor: &Floor1,
    ath: &[f64],
    bark: &[f64],
    headroom: f64,
    range: f64,
    co_mask: f64,
    masking_offset: f64,
    peaks: &[f64],
) -> (Vec<i32>, Vec<bool>) {
    let db: Vec<f64> = peaks
        .iter()
        .map(|&peak| 20.0 * (peak.max(1e-9)).log10())
        .collect();
    let mut spread = db.clone();
    let order = &floor.sorted;
    for window in order.windows(2) {
        let (previous, current) = (window[0], window[1]);
        spread[current] = spread[current].max(spread[previous] - 12.0);
    }
    for window in order.windows(2).rev() {
        let (current, next) = (window[0], window[1]);
        spread[current] = spread[current].max(spread[next] - 24.0);
    }
    for (source, &level) in db.iter().enumerate() {
        for (target, masked) in spread.iter_mut().enumerate() {
            let distance = (bark[target] - bark[source]).abs();
            if distance > 0.25 {
                continue;
            }
            let slope = if bark[target] >= bark[source] {
                48.0
            } else {
                80.0
            };
            *masked = (*masked).max(level - slope * distance);
        }
    }
    let loudest = db.iter().copied().fold(f64::MIN, f64::max);
    let target: Vec<f64> = spread
        .iter()
        .zip(ath.iter())
        .map(|(&level, &threshold)| {
            let threshold = threshold.max(loudest - co_mask);
            let offset = (masking_offset - (headroom - range).max(0.0)).max(0.0);
            (level - headroom.min(range) + offset)
                .max(threshold - (headroom - range).max(0.0))
        })
        .collect();
    let y: Vec<i32> = target
        .iter()
        .map(|&level| ((level / (140.0 / 256.0)).round() as i32 + 255).clamp(0, 255))
        .collect();
    fold_floor(floor, &y)
}

/// Run the decoder's own amplitude synthesis forwards, so the curve the
/// encoder normalises against is the curve the decoder will draw.
fn fold_floor(floor: &Floor1, wanted: &[i32]) -> (Vec<i32>, Vec<bool>) {
    let values = wanted.len();
    let range = 256i32;
    let mut final_y = vec![0i32; values];
    let mut step2 = vec![false; values];
    final_y[0] = wanted[0];
    final_y[1] = wanted[1];
    step2[0] = true;
    step2[1] = true;
    for i in 2..values {
        let (low, high) = floor.neighbours[i];
        let predicted = predict(
            floor.x_list[low],
            final_y[low],
            floor.x_list[high],
            final_y[high],
            floor.x_list[i],
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

/// Write one channel's floor: the two endpoints raw, the rest folded against
/// their neighbours and Huffman coded.
fn write_floor(out: &mut BitsOut, floor: &Floor1, floor_book: &CodebookSpec, y: &[i32]) {
    out.bit(true);
    out.write(y[0] as u32, 8);
    out.write(y[1] as u32, 8);
    for i in 2..y.len() {
        let (low, high) = floor.neighbours[i];
        let predicted = predict(
            floor.x_list[low],
            y[low],
            floor.x_list[high],
            y[high],
            floor.x_list[i],
        );
        let value = fold(predicted, y[i], 256);
        floor_book.write(out, value as usize);
    }
}

/// Write the residue: one type-2 interleaved vector over every channel,
/// classified per partition and coded in one pass.
fn write_residue(
    out: &mut BitsOut,
    quantised: &[Vec<i32>],
    half: usize,
    partition: usize,
    class_book: &CodebookSpec,
    residue_books: &[CodebookSpec],
) {
    let channels = quantised.len();
    let total = half * channels;
    let mut interleaved = vec![0i32; total];
    for (i, slot) in interleaved.iter_mut().enumerate() {
        *slot = quantised[i % channels][i / channels];
    }
    let partitions = total / partition;
    let classes: Vec<usize> = (0..partitions)
        .map(|p| {
            let slice = &interleaved[p * partition..(p + 1) * partition];
            let peak = slice.iter().map(|v| v.abs()).max().unwrap_or(0);
            CLASS_RANGE
                .iter()
                .position(|&range| peak <= range)
                .unwrap_or(CLASSES - 1)
        })
        .collect();
    let per_word = class_book.dimensions;
    let mut p = 0usize;
    while p < partitions {
        let mut word = 0usize;
        for i in 0..per_word {
            word = word * CLASSES + classes.get(p + i).copied().unwrap_or(0);
        }
        class_book.write(out, word);
        for _ in 0..per_word {
            if p >= partitions {
                break;
            }
            let class = classes[p];
            if class > 0 {
                let book = &residue_books[class - 1];
                let range = CLASS_RANGE[class];
                for chunk in interleaved[p * partition..(p + 1) * partition].chunks(book.dimensions)
                {
                    book.write(out, residue_entry(chunk, range, book.dimensions));
                }
            }
            p += 1;
        }
    }
}

/// Entry number for an exact vector value in one residue book.
fn residue_entry(values: &[i32], range: i32, dimensions: usize) -> usize {
    let base = (2 * range + 1) as usize;
    let mut entry = 0usize;
    let mut multiplier = 1usize;
    for i in 0..dimensions {
        let value = values.get(i).copied().unwrap_or(0).clamp(-range, range);
        entry += (value + range) as usize * multiplier;
        multiplier *= base;
    }
    entry
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
/// Inverse square-polar coupling on integer residue — the exact inverse of
/// [`couple`] and the integer form of the decoder's §9.4.2 step.  Used only to
/// bring the residue capture back to per-channel (magnitude/angle undone) so it
/// matches the decoder-side capture domain.
fn decouple(m: i32, a: i32) -> (i32, i32) {
    match (m > 0, a > 0) {
        (true, true) => (m, m - a),
        (true, false) => (m + a, m),
        (false, true) => (m, m + a),
        (false, false) => (m - a, m),
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

/// The floor-1 configuration for one block size: `half` coefficients, `points`
/// non-endpoint X values.
fn build_floor(half: usize, points: usize) -> Floor1 {
    let mut sorted_x: Vec<u32> = Vec::with_capacity(points);
    let mut last = 0u32;
    for i in 0..points {
        let t = i as f64 / (points - 1) as f64;
        // Log spacing over the band, nudged to stay strictly increasing at the
        // bottom where the grid is finer than one coefficient.
        let value = ((half - 1) as f64).powf(t).round() as u32;
        last = value.max(last + 1);
        sorted_x.push(last.min(half as u32 - 1));
    }
    // Coding order: endpoints first, then repeated bisection, so every value is
    // predicted from the two coded values that bracket it most closely.
    let mut x_list = vec![0u32, half as u32];
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

/// Build one block size's floor, ATH curve and Bark positions.
fn build_block_config(
    half: usize,
    partition: usize,
    points: usize,
    range: f64,
    co_mask: f64,
    config: &EncoderConfig,
) -> BlockConfig {
    let floor = build_floor(half, points);
    let hz: Vec<f64> = floor
        .x_list
        .iter()
        .map(|&x| f64::from(config.sample_rate) * 0.5 * f64::from(x) / half as f64)
        .collect();
    let ath = hz.iter().map(|&hz| absolute_threshold(hz)).collect();
    let bark = hz.iter().map(|&hz| bark(hz)).collect();
    BlockConfig {
        half,
        partition,
        floor,
        ath,
        bark,
        range,
        co_mask,
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
    // Measured on real music at 128 kbps: silent partitions are four in ten,
    // the three smallest ranges split most of the rest.
    let per_class = [0.40f64, 0.18, 0.19, 0.16, 0.05, 0.011, 0.008, 0.007, 0.001];
    let mut weights = Vec::with_capacity(CLASSES * CLASSES);
    for &first in &per_class {
        for &second in &per_class {
            weights.push(first * second);
        }
    }
    let mut spec = CodebookSpec::huffman(&weights, Vec::new());
    spec.dimensions = 2;
    spec
}

/// One book per non-silent class. Small ranges use vector entries because
/// non-zero partitions still contain many zero coefficients; wider ranges stay
/// scalar to keep setup overhead below the bits they can save.
fn design_residue_books() -> Vec<CodebookSpec> {
    CLASS_RANGE[1..]
        .iter()
        .enumerate()
        .map(|(offset, &range)| {
            let class = offset + 1;
            let dimensions = RESIDUE_BOOK_DIM[class];
            let base = (2 * range + 1) as usize;
            let entries = base.pow(dimensions as u32);
            let sigma = f64::from(range).max(1.0);
            let mut weights = Vec::with_capacity(entries);
            let mut values = Vec::with_capacity(entries * dimensions);
            for entry in 0..entries {
                let mut nonzero = 0usize;
                let mut weight = 1.0f64;
                for position in 0..dimensions {
                    let div = base.pow(position as u32);
                    let value = (entry / div % base) as i32 - range;
                    values.push(value as f32);
                    if value == 0 {
                        weight *= 0.60;
                    } else {
                        nonzero += 1;
                        weight *= 0.40 * (-f64::from(value.abs()) / sigma).exp();
                    }
                }
                let sparse = match nonzero {
                    0 => 4.0,
                    1 => 2.0,
                    2 => 0.8,
                    _ => 0.25,
                };
                weights.push(weight * sparse + 1e-8);
            }
            let mut spec = CodebookSpec::huffman(&weights, values);
            spec.dimensions = dimensions;
            spec
        })
        .collect()
}

/// Absolute threshold of hearing in dBFS, full scale taken as 96 dB SPL.
///
/// Capped at -80 dBFS: the curve's own values at DC and above ~17 kHz
/// (-13 / -10 dBFS) are not a quantiser step the rest of a block can live
/// with — an onset inside a short block puts real energy in exactly those
/// bins, and coding them that coarsely leaked -38 dB of pre-echo across the
/// whole 256-sample window.
fn absolute_threshold(hz: f64) -> f64 {
    let khz = (hz / 1000.0).clamp(0.02, 20.0);
    let spl =
        3.64 * khz.powf(-0.8) - 6.5 * (-0.6 * (khz - 3.3).powi(2)).exp() + 0.001 * khz.powi(4);
    (spl - 96.0).clamp(-120.0, -80.0)
}

/// Frequency in Barks, using the same compact psychoacoustic scale for every
/// sample rate.
fn bark(hz: f64) -> f64 {
    13.0 * (0.00076 * hz).atan() + 3.5 * ((hz / 7500.0) * (hz / 7500.0)).atan()
}

struct HeaderSetup<'a> {
    long: &'a BlockConfig,
    short: &'a BlockConfig,
    floor_book: &'a CodebookSpec,
    class_book: &'a CodebookSpec,
    residue_books: &'a [CodebookSpec],
    coupling: &'a [(usize, usize)],
}

/// The three header packets, in order.
fn write_headers(config: &EncoderConfig, channels: usize, setup: HeaderSetup<'_>) -> Vec<Vec<u8>> {
    let HeaderSetup {
        long,
        short,
        floor_book,
        class_book,
        residue_books,
        coupling,
    } = setup;
    let mut ident = vec![1u8];
    ident.extend_from_slice(b"vorbis");
    ident.extend_from_slice(&0u32.to_le_bytes());
    ident.push(channels as u8);
    ident.extend_from_slice(&config.sample_rate.to_le_bytes());
    ident.extend_from_slice(&0i32.to_le_bytes());
    ident.extend_from_slice(&config.bitrate_bps.max(0).to_le_bytes());
    ident.extend_from_slice(&0i32.to_le_bytes());
    let log2 = |n: usize| n.trailing_zeros() as u8;
    ident.push(log2(BLOCK_SHORT) | (log2(BLOCK_LONG) << 4));
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
    // Two floors, type 1: long first, then short.
    out.write(1, 6);
    for bc in [long, short] {
        out.write(1, 16);
        out.write(bc.floor.partition_classes.len() as u32, 5);
        for _ in &bc.floor.partition_classes {
            out.write(0, 4);
        }
        out.write(FLOOR_CLASS_DIM as u32 - 1, 3);
        out.write(0, 2);
        out.write(1, 8);
        out.write(0, 2);
        let range_bits = ilog(bc.half as u32) - 1;
        out.write(range_bits, 4);
        for &x in bc.floor.x_list.iter().skip(2) {
            out.write(x, range_bits);
        }
    }
    // Two residues, type 2: long first, then short, each over every channel of
    // its own submap and sharing the class/residue books.
    out.write(1, 6);
    for bc in [long, short] {
        out.write(2, 16);
        out.write(0, 24);
        out.write((bc.half * channels) as u32, 24);
        out.write(bc.partition as u32 - 1, 24);
        out.write(CLASSES as u32 - 1, 6);
        out.write(1, 8);
        for class in 0..CLASSES {
            out.write(u32::from(class > 0), 3);
            out.write(0, 1);
        }
        for class in 1..CLASSES {
            out.write(class as u32 + 1, 8);
        }
    }
    // Two mappings, type 0: mapping 0 is long (floor/residue 0), mapping 1 is
    // short (floor/residue 1); both couple the same channels.
    out.write(1, 6);
    for (floor, residue) in [(0u32, 0u32), (1, 1)] {
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
        out.write(floor, 8);
        out.write(residue, 8);
    }
    // Two modes: mode `MODE_SHORT` is a short block on mapping 1, mode
    // `MODE_LONG` a long block on mapping 0.
    out.write(1, 6);
    out.bit(false);
    out.write(0, 16);
    out.write(0, 16);
    out.write(1, 8);
    out.bit(true);
    out.write(0, 16);
    out.write(0, 16);
    out.write(0, 8);
    out.write(1, 1);
    let setup = out.finish();

    vec![ident, comment, setup]
}

/// One codebook in setup-header form: flat lengths and, when present, a type-1
/// lookup table carrying the scalar alphabet used by entry digits.
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
    out.write(1, 4);
    let minimum = book.values.iter().copied().fold(f32::INFINITY, f32::min);
    let maximum = book
        .values
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    out.write(float32_pack(minimum), 32);
    out.write(float32_pack(1.0), 32);
    let lookup_values = lookup1_values(book.entries(), book.dimensions);
    let value_bits = ilog((maximum - minimum).round() as u32).max(1);
    out.write(value_bits - 1, 4);
    out.write(0, 1);
    for value in 0..lookup_values {
        out.write(value as u32, value_bits);
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
    fn vector_residue_books_address_exact_values() {
        let books = design_residue_books();
        assert_eq!(
            books.iter().map(|b| b.dimensions).collect::<Vec<_>>(),
            vec![4, 2, 2, 1, 1, 1, 1, 1]
        );
        for (class, book) in books.iter().enumerate().take(3) {
            let range = CLASS_RANGE[class + 1];
            let vector: Vec<i32> = (0..book.dimensions)
                .map(|i| if i % 2 == 0 { range } else { -range })
                .collect();
            let entry = residue_entry(&vector, range, book.dimensions);
            let start = entry * book.dimensions;
            let decoded: Vec<i32> = book.values[start..start + book.dimensions]
                .iter()
                .map(|&v| v as i32)
                .collect();
            assert_eq!(decoded, vector);
        }
    }

    #[test]
    fn tail_granule_44100_mono() {
        let mut enc = VorbisEncoder::new(EncoderConfig {
            sample_rate: 44_100,
            channels: 2,
            bitrate_bps: -1,
            quality: 0.85,
        })
        .unwrap();
        let pcm = vec![0.1f32; 44_100 * 2];
        enc.push_interleaved(&pcm).unwrap();
        enc.finish();
        let mut last = 0i64;
        let mut n = 0;
        loop {
            match enc.next_packet() {
                Ok(p) => {
                    last = p.granule;
                    n += 1;
                }
                Err(Error::Eof) => break,
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(n, 45, "packet count");
        assert_eq!(last, 44_100, "last granule");
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

    /// A stationary tone never trips the transient detector: every packet
    /// decodes to mode `MODE_LONG`.
    #[test]
    fn a_stationary_signal_uses_only_long_blocks() {
        let mut enc = VorbisEncoder::new(EncoderConfig {
            sample_rate: 48_000,
            channels: 2,
            bitrate_bps: -1,
            quality: 0.6,
        })
        .unwrap();
        let samples = 48_000usize;
        let mut pcm = vec![0.0f32; samples * 2];
        for i in 0..samples {
            let t = i as f32 / 48_000.0;
            let v = (2.0 * std::f32::consts::PI * 1_000.0 * t).sin() * 0.5;
            pcm[2 * i] = v;
            pcm[2 * i + 1] = v;
        }
        enc.push_interleaved(&pcm).unwrap();
        enc.finish();
        let mut modes = std::collections::HashSet::new();
        loop {
            match enc.next_packet() {
                Ok(p) => {
                    let mut bits = crate::bits::Bits::new(&p.data);
                    assert!(!bits.bit());
                    modes.insert(bits.read(1));
                }
                Err(Error::Eof) => break,
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(modes, std::collections::HashSet::from([MODE_LONG]));
    }

    /// An onset after real silence trips the detector: some packet decodes to
    /// mode `MODE_SHORT`.
    #[test]
    fn an_onset_after_silence_uses_short_blocks() {
        let mut enc = VorbisEncoder::new(EncoderConfig {
            sample_rate: 44_100,
            channels: 2,
            bitrate_bps: -1,
            quality: 0.6,
        })
        .unwrap();
        let silence = 44_100usize;
        let tone = 4_410usize;
        let mut pcm = vec![0.0f32; (silence + tone) * 2];
        for i in 0..tone {
            let t = i as f32 / 44_100.0;
            let v = (2.0 * std::f32::consts::PI * 1_000.0 * t).sin();
            pcm[2 * (silence + i)] = v;
            pcm[2 * (silence + i) + 1] = v;
        }
        enc.push_interleaved(&pcm).unwrap();
        enc.finish();
        let mut saw_short = false;
        loop {
            match enc.next_packet() {
                Ok(p) => {
                    let mut bits = crate::bits::Bits::new(&p.data);
                    assert!(!bits.bit());
                    if bits.read(1) == MODE_SHORT {
                        saw_short = true;
                    }
                }
                Err(Error::Eof) => break,
                Err(e) => panic!("{e}"),
            }
        }
        assert!(saw_short, "no short block coded around the onset");
    }
}
