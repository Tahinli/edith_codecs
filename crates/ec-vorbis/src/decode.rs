//! The decoder: header packets in, audio packets in, planar float out.

use std::collections::VecDeque;

use ec_core::frame::ChannelPosition::{self, *};
use ec_core::{
    AudioFrame, AudioParameters, Buf, ChannelLayout, CodecId, CodecParameters, Decoder, Error,
    Frame, MediaParameters, Packet, Result, SampleFormat, TimeBase, Timestamp,
};
use ec_dsp::Mdct;

use crate::bits::Bits;
use crate::codebook::ilog;
use crate::floor::{self, FloorState};
use crate::residue;
use crate::setup::{Comments, FloorConfig, Identification, Setup};
use crate::window::Windows;

/// Vorbis I decoder.
///
/// Feed it the three header packets, then audio packets; every audio packet
/// answers the samples that became final when its window closed, which is why
/// the *first* audio packet of a stream answers none — its block only reaches
/// halfway into the first sample the stream states.
pub struct VorbisDecoder {
    ident: Identification,
    comments: Comments,
    setup: Setup,
    params: CodecParameters,
    layout: ChannelLayout,
    /// `to_ec[vorbis channel] = ec-core channel`.
    to_ec: Vec<usize>,
    windows: Windows,
    mdct_short: Mdct<f32>,
    mdct_long: Mdct<f32>,
    /// Per channel, the overlap-add accumulator, in Vorbis channel order.
    lap: Vec<Vec<f32>>,
    /// Absolute sample position of `lap[..][0]`.
    lap_start: i64,
    /// Centre of the block decoded last, absolute; `None` before the first.
    centre: Option<i64>,
    /// Blocksize of the block decoded last.
    previous_n: usize,
    /// Samples handed out so far, for the frames' timestamps.
    position: i64,
    /// Timestamp the next frame carries, when the container stated one.
    next_pts: Option<i64>,
    frames: VecDeque<Frame>,
    drained: bool,
}

impl VorbisDecoder {
    /// Build a decoder from the three header packets, in order.
    pub fn new(headers: &[&[u8]]) -> Result<VorbisDecoder> {
        let [ident, comment, setup] = headers else {
            return Err(Error::corrupt(format!(
                "Vorbis needs 3 header packets, got {}",
                headers.len()
            )));
        };
        let ident = Identification::parse(ident)?;
        let comments = Comments::parse(comment)?;
        let setup = Setup::parse(setup, &ident)?;
        let channels = usize::from(ident.channels);
        let (layout, to_ec) = channel_map(channels);
        let params = CodecParameters {
            codec: CodecId::Vorbis,
            media: MediaParameters::Audio(AudioParameters {
                sample_rate: ident.rate,
                layout: layout.clone(),
                format: Some(SampleFormat::F32),
                bits_per_sample: None,
            }),
            extradata: None,
        };
        Ok(VorbisDecoder {
            windows: Windows::new(ident.blocksize_0, ident.blocksize_1),
            mdct_short: Mdct::new(ident.blocksize_0),
            mdct_long: Mdct::new(ident.blocksize_1),
            lap: vec![Vec::new(); channels],
            lap_start: 0,
            centre: None,
            previous_n: ident.blocksize_1,
            position: 0,
            next_pts: None,
            frames: VecDeque::new(),
            drained: false,
            ident,
            comments,
            setup,
            params,
            layout,
            to_ec,
        })
    }

    /// Build a decoder from Xiph-laced `extradata`, the form containers carry
    /// the three headers in.
    pub fn from_extradata(extradata: &[u8]) -> Result<VorbisDecoder> {
        let packets = unlace(extradata)?;
        let borrowed: Vec<&[u8]> = packets.iter().map(|p| &p[..]).collect();
        VorbisDecoder::new(&borrowed)
    }

    /// The identification header's contents.
    pub fn identification(&self) -> &Identification {
        &self.ident
    }

    /// The comment header's contents.
    pub fn comments(&self) -> &Comments {
        &self.comments
    }

    /// Channel layout of the decoded frames.
    pub fn layout(&self) -> &ChannelLayout {
        &self.layout
    }

    /// Decode one audio packet into per-channel samples in ec-core channel
    /// order. The first packet of a stream answers empty vectors.
    pub fn decode_audio(&mut self, data: &[u8]) -> Result<Vec<Vec<f32>>> {
        let channels = usize::from(self.ident.channels);
        let mut bits = Bits::new(data);
        if bits.bit() {
            return Err(Error::corrupt("audio packet flagged as a header"));
        }
        let mode_bits = ilog(self.setup.modes.len() as u32 - 1);
        let mode_index = bits.read(mode_bits) as usize;
        let Some(&mode) = self.setup.modes.get(mode_index) else {
            return Err(Error::corrupt(
                "audio packet names a mode that is not there",
            ));
        };
        let n = match mode.block_flag {
            true => self.ident.blocksize_1,
            false => self.ident.blocksize_0,
        };
        let half = n / 2;
        let (previous_long, next_long) = match mode.block_flag {
            true => (bits.bit(), bits.bit()),
            false => (true, true),
        };
        if bits.eop() {
            return Err(Error::corrupt("audio packet header truncated"));
        }

        let mapping = &self.setup.mappings[mode.mapping];
        let mut floors = Vec::with_capacity(channels);
        let mut no_residue = Vec::with_capacity(channels);
        for &submap in mapping.mux.iter().take(channels) {
            let state = match &self.setup.floors[mapping.submaps[submap].0] {
                FloorConfig::Zero(config) => {
                    floor::decode_floor0(config, &self.setup.codebooks, &mut bits)
                }
                FloorConfig::One(config) => {
                    floor::decode_floor1(config, &self.setup.codebooks, &mut bits)
                }
            };
            no_residue.push(state.is_unused());
            floors.push(state);
        }
        // A coupled pair shares its residue, so one live channel keeps both.
        for &(magnitude, angle) in &mapping.coupling {
            if !no_residue[magnitude] || !no_residue[angle] {
                no_residue[magnitude] = false;
                no_residue[angle] = false;
            }
        }

        let mut spectra: Vec<Vec<f32>> = vec![vec![0.0; half]; channels];
        for (index, &(_, residue_index)) in mapping.submaps.iter().enumerate() {
            let members: Vec<usize> = (0..channels).filter(|&c| mapping.mux[c] == index).collect();
            if members.is_empty() {
                continue;
            }
            let config = &self.setup.residues[residue_index];
            let skip: Vec<bool> = members.iter().map(|&c| no_residue[c]).collect();
            let mut decoded: Vec<Vec<f32>> = vec![vec![0.0; half]; members.len()];
            if config.kind == 2 {
                // Type 2 codes the whole submap as one interleaved vector, so
                // it is decoded whole and split afterwards.
                let mut interleaved = vec![vec![0.0f32; half * members.len()]];
                let all_skipped = [skip.iter().all(|&s| s)];
                residue::decode(
                    config,
                    &self.setup.codebooks,
                    &mut bits,
                    &mut interleaved,
                    &all_skipped,
                );
                residue::deinterleave(&interleaved[0], members.len(), &mut decoded);
            } else {
                residue::decode(
                    config,
                    &self.setup.codebooks,
                    &mut bits,
                    &mut decoded,
                    &skip,
                );
            }
            for (slot, &channel) in members.iter().enumerate() {
                spectra[channel] = std::mem::take(&mut decoded[slot]);
            }
        }

        // §9.4.2 inverse coupling, newest step first.
        for &(magnitude, angle) in mapping.coupling.iter().rev() {
            // Taken out and put back so the two vectors can be walked in step;
            // a coupling step never names one channel twice.
            let mut mags = std::mem::take(&mut spectra[magnitude]);
            let mut angles = std::mem::take(&mut spectra[angle]);
            for (m, a) in mags.iter_mut().zip(angles.iter_mut()) {
                let (new_m, new_a) = match (*m > 0.0, *a > 0.0) {
                    (true, true) => (*m, *m - *a),
                    (true, false) => (*m + *a, *m),
                    (false, true) => (*m, *m + *a),
                    (false, false) => (*m - *a, *m),
                };
                *m = new_m;
                *a = new_a;
            }
            spectra[magnitude] = mags;
            spectra[angle] = angles;
        }

        // Floor times residue, then out of the frequency domain.
        let window = self.windows.get(mode.block_flag, previous_long, next_long);
        let mdct = match mode.block_flag {
            true => &mut self.mdct_long,
            false => &mut self.mdct_short,
        };
        let mut curve = vec![0.0f32; half];
        let mut block = vec![0.0f32; n];
        let centre = match self.centre {
            Some(previous) => previous + (self.previous_n + n) as i64 / 4,
            None => {
                self.lap_start = 0;
                self.lap = vec![vec![0.0; n]; channels];
                n as i64 / 2
            }
        };
        let start = centre - n as i64 / 2;

        for channel in 0..channels {
            match &floors[channel] {
                FloorState::Unused => spectra[channel].fill(0.0),
                FloorState::One { y, step2 } => {
                    let submap = mapping.mux[channel];
                    let FloorConfig::One(config) = &self.setup.floors[mapping.submaps[submap].0]
                    else {
                        return Err(Error::corrupt("floor type changed between packets"));
                    };
                    floor::render_floor1(config, y, step2, &mut curve);
                    for (value, &gain) in spectra[channel].iter_mut().zip(curve.iter()) {
                        *value *= gain;
                    }
                }
                FloorState::Zero {
                    amplitude,
                    coefficients,
                } => {
                    let submap = mapping.mux[channel];
                    let FloorConfig::Zero(config) = &self.setup.floors[mapping.submaps[submap].0]
                    else {
                        return Err(Error::corrupt("floor type changed between packets"));
                    };
                    floor::render_floor0(config, *amplitude, coefficients, &mut curve);
                    for (value, &gain) in spectra[channel].iter_mut().zip(curve.iter()) {
                        *value *= gain;
                    }
                }
            }
            // Undo the codec-side normalisation ec-dsp's inverse applies: a
            // Vorbis coefficient is already `2/N` of the raw transform, so the
            // synthesis here is the plain sum.
            for value in spectra[channel].iter_mut() {
                *value *= (half / 2) as f32;
            }
            mdct.inverse_windowed(&spectra[channel], window, &mut block);
            let lap = &mut self.lap[channel];
            let offset = (start - self.lap_start).max(0) as usize;
            if lap.len() < offset + n {
                lap.resize(offset + n, 0.0);
            }
            // A block starting before what has already been handed out only
            // ever reaches back into its own zero run (§4.3.1 puts the slope
            // inside the block when the neighbour is shorter), so the skipped
            // samples are zeroes by construction.
            let skipped = (self.lap_start - start).max(0) as usize;
            for i in skipped..n {
                lap[offset + i - skipped] += block[i];
            }
        }

        let emit = (centre - self.lap_start).max(0) as usize;
        let first = self.centre.is_none();
        let mut out = vec![Vec::new(); channels];
        for (channel, lap) in self.lap.iter_mut().enumerate() {
            let take = emit.min(lap.len());
            let head: Vec<f32> = lap.drain(..take).collect();
            if !first {
                out[self.to_ec[channel]] = head;
            }
        }
        self.lap_start = centre;
        self.centre = Some(centre);
        self.previous_n = n;
        Ok(out)
    }

    /// Drop the lapping state after a seek; the next packet starts a new run.
    pub fn reset_state(&mut self) {
        self.centre = None;
        self.lap_start = 0;
        self.position = 0;
        for lap in &mut self.lap {
            lap.clear();
        }
        self.frames.clear();
        self.drained = false;
    }
}

/// Vorbis channel order (§4.3.9) mapped onto [`ChannelLayout`]'s order.
///
/// Vorbis orders a 5.1 stream FL, FC, FR, BL, BR, LFE and a 7.1 stream FL, FC,
/// FR, SL, SR, BL, BR, LFE; this family orders both FL, FR, FC, LFE, ... So the
/// two named surround layouts are permuted on the way out and everything else
/// passes through in the order the stream states, carrying the positions Vorbis
/// names for it.
pub fn channel_map(channels: usize) -> (ChannelLayout, Vec<usize>) {
    let positions: &[ChannelPosition] = match channels {
        1 => &[FrontCenter],
        2 => &[FrontLeft, FrontRight],
        3 => &[FrontLeft, FrontCenter, FrontRight],
        4 => &[FrontLeft, FrontRight, BackLeft, BackRight],
        5 => &[FrontLeft, FrontCenter, FrontRight, BackLeft, BackRight],
        6 => &[FrontLeft, FrontCenter, FrontRight, BackLeft, BackRight, Lfe],
        7 => &[
            FrontLeft,
            FrontCenter,
            FrontRight,
            SideLeft,
            SideRight,
            BackCenter,
            Lfe,
        ],
        8 => &[
            FrontLeft,
            FrontCenter,
            FrontRight,
            SideLeft,
            SideRight,
            BackLeft,
            BackRight,
            Lfe,
        ],
        _ => &[],
    };
    match channels {
        1 => (ChannelLayout::Mono, vec![0]),
        2 => (ChannelLayout::Stereo, vec![0, 1]),
        6 => (ChannelLayout::Surround5_1, vec![0, 2, 1, 4, 5, 3]),
        8 => (ChannelLayout::Surround7_1, vec![0, 2, 1, 6, 7, 4, 5, 3]),
        n if n <= 8 => (ChannelLayout::Custom(positions.to_vec()), (0..n).collect()),
        n => (ChannelLayout::from_count(n), (0..n).collect()),
    }
}

/// The Xiph lacing containers wrap the three headers in.
fn unlace(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let count = *data
        .first()
        .ok_or_else(|| Error::corrupt("empty Vorbis extradata"))? as usize;
    if count != 2 {
        return Err(Error::corrupt("Vorbis extradata does not hold 3 packets"));
    }
    let mut lengths = Vec::with_capacity(2);
    let mut pos = 1usize;
    for _ in 0..count {
        let mut length = 0usize;
        loop {
            let byte = *data
                .get(pos)
                .ok_or_else(|| Error::corrupt("Vorbis extradata lacing"))?;
            pos += 1;
            length += usize::from(byte);
            if byte != 255 {
                break;
            }
        }
        lengths.push(length);
    }
    let mut packets = Vec::with_capacity(3);
    for length in lengths {
        let end = pos
            .checked_add(length)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| Error::corrupt("Vorbis extradata is shorter than it states"))?;
        packets.push(data[pos..end].to_vec());
        pos = end;
    }
    packets.push(data[pos..].to_vec());
    Ok(packets)
}

impl Decoder for VorbisDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        let samples = self.decode_audio(&packet.data)?;
        if samples.iter().all(Vec::is_empty) {
            // The first packet of a run: its block only reaches the first
            // sample the stream states, so there is nothing final yet.
            self.next_pts = packet.pts;
            return Ok(());
        }
        let count = samples[0].len();
        let data: Vec<Buf> = samples
            .into_iter()
            .map(|channel| {
                let mut bytes = Vec::with_capacity(channel.len() * 4);
                for value in channel {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                Buf::from_vec(bytes)
            })
            .collect();
        let mut frame = AudioFrame::try_new(
            SampleFormat::F32,
            true,
            self.layout.clone(),
            self.ident.rate,
            count,
            data,
        )?;
        let base = TimeBase::new(1, i64::from(self.ident.rate));
        frame.pts = Some(Timestamp::new(self.next_pts.unwrap_or(self.position), base));
        self.next_pts = None;
        self.position += count as i64;
        self.frames.push_back(Frame::Audio(frame));
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.frames.pop_front() {
            Some(frame) => Ok(frame),
            None if self.drained => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        // §1.3.2: the terminal block's un-overlapped right half (everything
        // past the last emitted centre) is still valid output up to the
        // stream's final granule position; without this the decoder is
        // always exactly one hop short at true EOS.
        if self.centre.is_some() {
            let count = self.lap[0].len();
            if count > 0 {
                let mut out = vec![Vec::new(); self.lap.len()];
                for (channel, lap) in self.lap.iter_mut().enumerate() {
                    out[self.to_ec[channel]] = std::mem::take(lap);
                }
                let data: Vec<Buf> = out
                    .into_iter()
                    .map(|channel| {
                        let mut bytes = Vec::with_capacity(channel.len() * 4);
                        for value in channel {
                            bytes.extend_from_slice(&value.to_le_bytes());
                        }
                        Buf::from_vec(bytes)
                    })
                    .collect();
                let mut frame = AudioFrame::try_new(
                    SampleFormat::F32,
                    true,
                    self.layout.clone(),
                    self.ident.rate,
                    count,
                    data,
                )?;
                let base = TimeBase::new(1, i64::from(self.ident.rate));
                frame.pts = Some(Timestamp::new(self.position, base));
                self.position += count as i64;
                self.frames.push_back(Frame::Audio(frame));
            }
        }
        self.drained = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.reset_state();
    }
}
