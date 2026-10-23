//! A tiny demuxer for raw DIF streams (`.dv`, `.dif`): frame boundaries come from the DIF header,
//! one video packet per frame, plus 16-bit PCM audio packets (one stream per stereo pair).

use std::io::{Read, Seek, SeekFrom};

use ec_core::error::{Error, Result};
use ec_core::frame::{ChannelLayout, SampleFormat};
use ec_core::packet::{Packet, PacketFlags};
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, Demuxer, MediaParameters, SeekMode, StreamInfo, VideoParameters};
use ec_core::timebase::{Rounding, TimeBase, Timestamp};

use crate::audio::extract_audio_with;
use crate::decoder::output_format;
use crate::profile::{PROFILE_BYTES, Profile, frame_profile};

/// Probe score (0..=100) that `head` is a raw DV stream, after FFmpeg's `dv_probe`: counts DIF
/// section headers (`1f 07 00 3f/bf`) that recur about every 12000 bytes.
pub fn dv_probe_score(p: &[u8]) -> u8 {
    if p.len() < 5 {
        return 0;
    }
    let (mut matches, mut firstmatch, mut secondary, mut marker_pos) = (0usize, false, 0usize, 0usize);
    for i in 0..p.len() - 4 {
        let state = u32::from_be_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]);
        if state & 0x0007_f840 == 0x0007_0000 {
            if state & 0xff07_ff7f == 0x1f07_003f {
                secondary += 1;
                if state & 0xffff_ff7f == 0x1f07_003f {
                    matches += 1;
                    if i == 0 {
                        firstmatch = true;
                    }
                }
            }
            if state == 0x003f_0700 || state == 0xff3f_0700 {
                marker_pos = i;
            }
            if state == 0xff3f_0701 && i - marker_pos == 80 {
                matches += 1;
            }
        }
    }
    if matches > 0 && p.len() / matches < 1024 * 1024 {
        if matches > 4 || firstmatch || (secondary >= 10 && p.len() / secondary < 24000) {
            return 75;
        }
        return 25;
    }
    0
}

/// True when `head` (the first few KB of a file) looks like a raw DV/DIF stream.
pub fn is_dv(head: &[u8]) -> bool {
    dv_probe_score(head) >= 75
}

fn find_header(r: &mut (impl Read + Seek)) -> Result<u64> {
    // Scan for the first DIF header block (`1f 07 00 3f`, dsf bit ignored), allowing leading junk.
    let mut buf = vec![0u8; 1 << 16];
    r.seek(SeekFrom::Start(0))?;
    let n = read_full(r, &mut buf)?;
    for i in 0..n.saturating_sub(4) {
        let state = u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
        if state & 0xffff_ff7f == 0x1f07_003f {
            return Ok(i as u64);
        }
    }
    Err(Error::corrupt("DV: no DIF header found"))
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(n)
}

/// Raw DV demuxer. Stream 0 is the video (`CodecId::DvVideo`, packets are whole frames); streams
/// `1..` are 16-bit little-endian stereo PCM (`CodecId::PcmS16Le`), one per stereo pair the audio
/// source pack announces in the first frame.
pub struct DvDemuxer<R> {
    r: R,
    start: u64,
    sys: &'static Profile,
    streams: Vec<StreamInfo>,
    /// Indices of the DIF stereo pairs behind streams 1.. .
    pair_of_stream: Vec<usize>,
    video_tb: TimeBase,
    frame: i64,
    next_audio_pts: i64,
    pending: std::collections::VecDeque<Packet>,
    total_frames: Option<i64>,
}

impl<R: Read + Seek + Send> DvDemuxer<R> {
    /// Open a raw DV stream: locate the first DIF header, identify the system and the audio.
    pub fn new(mut r: R) -> Result<DvDemuxer<R>> {
        let start = find_header(&mut r)?;
        r.seek(SeekFrom::Start(start))?;
        let mut head = vec![0u8; PROFILE_BYTES];
        if read_full(&mut r, &mut head)? < PROFILE_BYTES {
            return Err(Error::corrupt("DV: stream shorter than one DIF sequence header"));
        }
        let sys = frame_profile(None, &head).ok_or_else(|| Error::corrupt("DV: cannot determine the system of the stream"))?;
        let end = r.seek(SeekFrom::End(0))?;
        let total_frames = Some(((end.saturating_sub(start)) / sys.frame_size as u64) as i64);
        r.seek(SeekFrom::Start(start))?;
        let video_tb = TimeBase::new(i64::from(sys.time_base.0), i64::from(sys.time_base.1));

        let mut vp = CodecParameters::new(CodecId::DvVideo);
        if let MediaParameters::Video(v) = &mut vp.media {
            *v = VideoParameters {
                width: sys.width,
                height: sys.height,
                format: Some(output_format(sys.chroma)),
                frame_rate: Some(video_tb.inverse()),
                sample_aspect_ratio: Some(TimeBase::new(i64::from(sys.sar[0].0), i64::from(sys.sar[0].1))),
                ..VideoParameters::default()
            };
        }
        let mut video = StreamInfo::new(0, video_tb, vp);
        video.start_time = Some(0);
        video.duration = total_frames;
        let mut streams = vec![video];

        // Look at the first frame's audio to declare the audio streams.
        let mut first = vec![0u8; sys.frame_size];
        let mut pair_of_stream = Vec::new();
        if read_full(&mut r, &mut first)? == sys.frame_size
            && let Some(a) = extract_audio_with(&first, sys)
        {
            for i in 0..a.pairs.len() {
                let mut ap = CodecParameters::new(CodecId::PcmS16Le);
                ap.media = MediaParameters::Audio(AudioParameters {
                    sample_rate: a.sample_rate,
                    layout: ChannelLayout::Stereo,
                    format: Some(SampleFormat::S16),
                    bits_per_sample: Some(16),
                });
                let idx = streams.len() as u32;
                let mut s = StreamInfo::new(idx, TimeBase::from_rate(a.sample_rate), ap);
                s.start_time = Some(0);
                streams.push(s);
                pair_of_stream.push(i);
            }
        }
        r.seek(SeekFrom::Start(start))?;
        Ok(DvDemuxer { r, start, sys, streams, pair_of_stream, video_tb, frame: 0, next_audio_pts: 0, pending: Default::default(), total_frames })
    }

    /// The DV system of this stream.
    pub fn profile(&self) -> &'static Profile {
        self.sys
    }

    /// Number of whole frames in the file.
    pub fn frame_count(&self) -> Option<i64> {
        self.total_frames
    }

    fn fill(&mut self) -> Result<()> {
        let mut buf = vec![0u8; self.sys.frame_size];
        let n = read_full(&mut self.r, &mut buf)?;
        if n < self.sys.frame_size {
            return Err(Error::Eof);
        }
        let idx = self.frame;
        self.frame += 1;
        let audio = extract_audio_with(&buf, self.sys);
        let mut v = Packet::new(0, self.video_tb, buf);
        v.pts = Some(idx);
        v.dts = Some(idx);
        v.duration = Some(1);
        v.flags = PacketFlags { keyframe: true, ..PacketFlags::default() };
        self.pending.push_back(v);
        if let Some(a) = audio {
            let tb = TimeBase::from_rate(a.sample_rate);
            let dur = a.samples as i64;
            // FFmpeg resynchronises to the video clock when audio drifts by a frame or more.
            let vpts = self.video_tb.rescale(idx, tb, Rounding::Nearest);
            let pts = if (vpts - self.next_audio_pts).abs() >= dur { vpts } else { self.next_audio_pts };
            self.next_audio_pts = pts + dur;
            for (si, &pi) in self.pair_of_stream.iter().enumerate() {
                let Some(pcm) = a.pairs.get(pi).filter(|p| !p.is_empty()) else { continue };
                let mut bytes = Vec::with_capacity(pcm.len() * 2);
                for s in pcm {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                let mut p = Packet::new(si as u32 + 1, tb, bytes);
                p.pts = Some(pts);
                p.dts = Some(pts);
                p.duration = Some(dur);
                p.flags = PacketFlags { keyframe: true, ..PacketFlags::default() };
                self.pending.push_back(p);
            }
        }
        Ok(())
    }
}

impl<R: Read + Seek + Send> Demuxer for DvDemuxer<R> {
    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    fn next_packet(&mut self) -> Result<Packet> {
        if self.pending.is_empty() {
            self.fill()?;
        }
        self.pending.pop_front().ok_or(Error::Eof)
    }

    fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<()> {
        if stream as usize >= self.streams.len() {
            return Err(Error::unsupported(format!("seek on stream {stream}"), "no such stream"));
        }
        let rounding = match mode {
            SeekMode::SyncAfter => Rounding::Up,
            SeekMode::SyncBefore => Rounding::Down,
            SeekMode::Exact => Rounding::Nearest,
        };
        let mut idx = to.base.checked_rescale(to.ticks, self.video_tb, rounding).unwrap_or(0).max(0);
        if let Some(t) = self.total_frames {
            idx = idx.min((t - 1).max(0));
        }
        self.r.seek(SeekFrom::Start(self.start + idx as u64 * self.sys.frame_size as u64))?;
        self.frame = idx;
        self.pending.clear();
        self.next_audio_pts = 0;
        Ok(())
    }
}
