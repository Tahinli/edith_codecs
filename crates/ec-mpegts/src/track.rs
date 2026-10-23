//! One elementary stream under the selected program: PES reassembly state and the
//! per-codec packetisers that turn PES payloads into access units and frames.

use std::collections::VecDeque;

use ec_core::{
    AudioParameters, ChannelLayout, CodecId, CodecParameters, MediaParameters, PixelFormat,
    SampleFormat, StreamInfo, TimeBase, registry::VideoParameters,
};

use crate::codec::{
    self, FrameHdr, Framing, LpcmHeader, StreamKind, build_avcc, build_hvcc, frame_header,
    header_lookahead, scan_au,
};
use crate::psi::EsEntry;
use ec_truehd::{AccessUnitHeader, frame_length};

/// 90 kHz, the clock of PTS and DTS.
pub(crate) const TICKS: i64 = 90_000;

/// Largest access unit or PES this crate buffers.
pub(crate) const MAX_PES: usize = 32 << 20;

/// Longest audio frame any supported framing allows, with margin.
const MAX_FRAME: usize = 16 * 1024;

/// Bytes a TrueHD packetiser needs before it can judge a position: an AC-3 sync
/// header is 8 bytes, a TrueHD access unit's length word is 2.
const TRUEHD_LOOKAHEAD: usize = 8;

/// A finished packet on its way out of the engine.
#[derive(Debug)]
pub(crate) struct Queued {
    pub track: usize,
    /// File offset of the transport packet that began the PES.
    pub off: u64,
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub dur: Option<i64>,
    pub key: bool,
    pub corrupt: bool,
    /// The timestamps were derived from neighbouring pictures, not read from a packet.
    pub derived: bool,
    pub data: Vec<u8>,
}

/// A PES payload with the facts its header and transport packets gave.
pub(crate) struct PesIn<'a> {
    pub payload: &'a [u8],
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub off: u64,
    pub rai: bool,
    pub bad: bool,
}

pub(crate) struct Au {
    pub data: Vec<u8>,
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    pub off: u64,
    pub rai: bool,
    pub bad: bool,
}

struct Frame {
    data: Vec<u8>,
    pts: Option<i64>,
    dur: i64,
    off: u64,
    bad: bool,
}

/// Facts collected from the stream itself.
#[derive(Default, Clone)]
pub(crate) struct Cfg {
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    pub vps: Vec<Vec<u8>>,
    pub hdr: Option<FrameHdr>,
    pub lpcm: Option<LpcmHeader>,
    pub mpeg2: Option<codec::Mpeg2Facts>,
    pub truehd: Option<TrueHdFacts>,
}

/// What a TrueHD stream's major syncs say: the sample rate, the channel layout of
/// the highest PCM presentation, and the samples one access unit holds.
#[derive(Clone)]
pub(crate) struct TrueHdFacts {
    pub rate: u32,
    pub layout: ChannelLayout,
    pub samples: u32,
}

/// How the program-stream front end hands pictures over (see `psvideo`).
#[derive(Default)]
pub(crate) struct PsVideo {
    /// The track belongs to a program stream.
    pub ps: bool,
    pub on: bool,
    pub buf: crate::psvideo::VBuf,
}

/// One elementary stream.
pub(crate) struct Track {
    pub es: EsEntry,
    pub kind: StreamKind,
    pub refusal: Option<String>,
    pub stream: Option<u32>,
    pub active: bool,
    pub cfg: Cfg,
    pub psv: PsVideo,
    // PES reassembly.
    pub pes: Vec<u8>,
    pub pes_on: bool,
    pub pes_off: u64,
    pub pes_rai: bool,
    pub pes_bad: bool,
    pub pes_expect: Option<usize>,
    pub last_pts: Option<i64>,
    // Packetiser state.
    au: Option<Au>,
    carry: Vec<u8>,
    carry_off: u64,
    /// Timestamp the next audio frame gets.
    anchor: Option<i64>,
    pending: Option<Frame>,
    // Timing facts.
    pub min_pts: Option<i64>,
    pub max_end: Option<i64>,
    pub dts_seen: Vec<i64>,
    pub start_time: Option<i64>,
    pub packets: u64,
    /// DVD subpicture units are split over PES packets.
    spu: ec_dvdsub::SpuAssembler,
}

impl Track {
    pub(crate) fn new(es: EsEntry) -> Track {
        let kind = StreamKind::classify(&es);
        let refusal = match &kind {
            StreamKind::AacLatm => Some(
                "aac_latm (stream type 0x11): LATM/LOAS framing is not implemented, only ADTS"
                    .to_string(),
            ),
            StreamKind::Other(n) => Some(format!(
                "{n} (stream type 0x{:02x}): no demux path for this stream type",
                es.stream_type
            )),
            _ => None,
        };
        Track {
            es,
            kind,
            refusal,
            stream: None,
            active: true,
            cfg: Cfg::default(),
            psv: PsVideo::default(),
            pes: Vec::new(),
            pes_on: false,
            pes_off: 0,
            pes_rai: false,
            pes_bad: false,
            pes_expect: None,
            last_pts: None,
            au: None,
            carry: Vec::new(),
            carry_off: 0,
            anchor: None,
            pending: None,
            min_pts: None,
            max_end: None,
            dts_seen: Vec::new(),
            start_time: None,
            packets: 0,
            spu: ec_dvdsub::SpuAssembler::default(),
        }
    }

    /// True when packets of this track are produced at all.
    pub(crate) fn wanted(&self) -> bool {
        self.active && self.refusal.is_none()
    }

    /// Drops all in-flight data; the configuration found so far stays.
    pub(crate) fn reset(&mut self, seed: Option<i64>) {
        self.pes.clear();
        self.pes_on = false;
        self.pes_bad = false;
        self.pes_expect = None;
        self.au = None;
        self.carry.clear();
        self.anchor = None;
        self.pending = None;
        self.last_pts = seed;
        self.psv.buf.clear();
        self.spu.reset();
    }

    /// True once the stream's parameters are known well enough to describe it.
    pub(crate) fn ready(&self) -> bool {
        if !self.wanted() {
            return false;
        }
        match self.kind {
            StreamKind::H264 => {
                !self.cfg.sps.is_empty()
                    && !self.cfg.pps.is_empty()
                    && codec::h264_facts(&self.cfg.sps[0]).is_some()
            }
            StreamKind::Hevc => {
                !self.cfg.sps.is_empty()
                    && !self.cfg.pps.is_empty()
                    && !self.cfg.vps.is_empty()
                    && codec::hevc_facts(&self.cfg.sps[0]).is_some()
            }
            StreamKind::AacAdts
            | StreamKind::Ac3
            | StreamKind::Eac3
            | StreamKind::Dts
            | StreamKind::MpegAudio => self.cfg.hdr.is_some(),
            StreamKind::Mpeg1Video | StreamKind::Mpeg2Video => self.cfg.mpeg2.is_some(),
            StreamKind::Lpcm | StreamKind::DvdLpcm => self.cfg.lpcm.is_some(),
            StreamKind::Pgs | StreamKind::DvdSub | StreamKind::DvbSub => true,
            StreamKind::TrueHd => self.cfg.truehd.is_some(),
            _ => false,
        }
    }

    /// The value one video frame lasts, estimated from decode timestamps.
    pub(crate) fn frame_ticks(&self) -> Option<i64> {
        let mut deltas: Vec<i64> = self
            .dts_seen
            .windows(2)
            .map(|w| w[1] - w[0])
            .filter(|&d| d > 0)
            .collect();
        if deltas.is_empty() {
            return None;
        }
        deltas.sort_unstable();
        // The most common delta: with B-frames the decode times are even.
        let mut best = (0usize, deltas[0]);
        let mut run = 0usize;
        for i in 0..deltas.len() {
            run = if i > 0 && deltas[i] == deltas[i - 1] {
                run + 1
            } else {
                1
            };
            if run > best.0 {
                best = (run, deltas[i]);
            }
        }
        Some(best.1)
    }

    /// The `StreamInfo` for this track, when it is ready. `index` is the stream index.
    pub(crate) fn stream_info(&self, index: u32) -> Option<StreamInfo> {
        if !self.ready() {
            return None;
        }
        let tb = TimeBase::new(1, TICKS);
        let params = match self.kind {
            StreamKind::H264 => {
                let f = codec::h264_facts(&self.cfg.sps[0])?;
                let mut p = CodecParameters::new(CodecId::H264);
                p.extradata = build_avcc(&self.cfg.sps, &self.cfg.pps).map(Into::into);
                let fr = self.frame_rate();
                p.media = MediaParameters::Video(VideoParameters {
                    width: f.width,
                    height: f.height,
                    format: pixel_format(f.chroma, f.depth_luma),
                    frame_rate: fr,
                    ..VideoParameters::default()
                });
                p
            }
            StreamKind::Hevc => {
                let f = codec::hevc_facts(&self.cfg.sps[0])?;
                let mut p = CodecParameters::new(CodecId::H265);
                p.extradata =
                    build_hvcc(&self.cfg.vps, &self.cfg.sps, &self.cfg.pps).map(Into::into);
                let fr = self.frame_rate();
                p.media = MediaParameters::Video(VideoParameters {
                    width: f.width,
                    height: f.height,
                    format: pixel_format(f.chroma, f.depth_luma),
                    frame_rate: fr,
                    ..VideoParameters::default()
                });
                p
            }
            StreamKind::Mpeg1Video | StreamKind::Mpeg2Video => {
                let f = self.cfg.mpeg2.as_ref()?;
                let mut p = CodecParameters::new(CodecId::Mpeg2Video);
                p.extradata = Some(f.extradata.clone().into());
                p.media = MediaParameters::Video(VideoParameters {
                    width: f.width,
                    height: f.height,
                    format: pixel_format(if f.has_ext { f.chroma } else { 1 }, 8),
                    frame_rate: TimeBase::try_new(i64::from(f.rate.0), i64::from(f.rate.1)).ok(),
                    sample_aspect_ratio: f
                        .sar
                        .and_then(|(n, d)| TimeBase::try_new(i64::from(n), i64::from(d)).ok()),
                    ..VideoParameters::default()
                });
                p
            }
            StreamKind::AacAdts => {
                let h = self.cfg.hdr?;
                let mut p = CodecParameters::new(CodecId::Aac);
                p.extradata =
                    Some(codec::audio_specific_config(h.aux + 1, h.sr_idx, h.ch_cfg).into());
                p.media = MediaParameters::Audio(AudioParameters {
                    sample_rate: h.rate,
                    layout: ChannelLayout::from_count(usize::from(h.channels.max(1))),
                    format: None,
                    bits_per_sample: None,
                });
                p
            }
            StreamKind::Ac3 | StreamKind::Eac3 => {
                let h = self.cfg.hdr?;
                let id = if self.kind == StreamKind::Ac3 {
                    CodecId::Ac3
                } else {
                    CodecId::EAc3
                };
                let mut p = CodecParameters::new(id);
                p.media = MediaParameters::Audio(AudioParameters {
                    sample_rate: h.rate,
                    layout: ChannelLayout::from_count(usize::from(h.channels.max(1))),
                    format: None,
                    bits_per_sample: None,
                });
                p
            }
            StreamKind::Dts => {
                let h = self.cfg.hdr?;
                let mut p = CodecParameters::new(CodecId::Dts);
                p.media = MediaParameters::Audio(AudioParameters {
                    sample_rate: h.rate,
                    layout: ChannelLayout::from_count(usize::from(h.channels.max(1))),
                    format: None,
                    bits_per_sample: None,
                });
                p
            }
            StreamKind::MpegAudio => {
                let h = self.cfg.hdr?;
                let id = match h.aux {
                    1 => CodecId::Mp1,
                    2 => CodecId::Mp2,
                    _ => CodecId::Mp3,
                };
                let mut p = CodecParameters::new(id);
                p.media = MediaParameters::Audio(AudioParameters {
                    sample_rate: h.rate,
                    layout: ChannelLayout::from_count(usize::from(h.channels.max(1))),
                    format: None,
                    bits_per_sample: None,
                });
                p
            }
            StreamKind::Lpcm | StreamKind::DvdLpcm => {
                let l = self.cfg.lpcm?;
                let id = if l.bits == 16 {
                    CodecId::PcmS16Be
                } else {
                    CodecId::PcmS24Le
                };
                let mut p = CodecParameters::new(id);
                p.media = MediaParameters::Audio(AudioParameters {
                    sample_rate: l.rate,
                    layout: ChannelLayout::from_count(usize::from(l.channels)),
                    format: (l.bits == 16).then_some(SampleFormat::S16),
                    bits_per_sample: Some(if l.bits == 16 { 16 } else { 24 }),
                });
                p
            }
            StreamKind::Pgs => CodecParameters::new(CodecId::Pgs),
            StreamKind::DvdSub => CodecParameters::new(CodecId::DvdSub),
            StreamKind::DvbSub => CodecParameters::new(CodecId::DvbSub),
            StreamKind::TrueHd => {
                let t = self.cfg.truehd.as_ref()?;
                let mut p = CodecParameters::new(CodecId::TrueHd);
                p.media = MediaParameters::Audio(AudioParameters {
                    sample_rate: t.rate,
                    layout: t.layout.clone(),
                    format: None,
                    bits_per_sample: None,
                });
                p
            }
            _ => return None,
        };
        let mut info = StreamInfo::new(index, tb, params);
        info.start_time = self.start_time.or(self.min_pts);
        info.language = self.es.language();
        Some(info)
    }

    fn frame_rate(&self) -> Option<TimeBase> {
        let d = self.frame_ticks()?;
        TimeBase::try_new(TICKS, d).ok()
    }

    // -----------------------------------------------------------------------
    // PES in
    // -----------------------------------------------------------------------

    pub(crate) fn push_pes(&mut self, ti: usize, p: &PesIn<'_>, q: &mut VecDeque<Queued>) {
        match self.kind {
            StreamKind::H264
            | StreamKind::Hevc
            | StreamKind::Mpeg1Video
            | StreamKind::Mpeg2Video => self.video_push(ti, p, q),
            StreamKind::AacAdts => self.audio_push(ti, p, Framing::Adts, q),
            StreamKind::Ac3 | StreamKind::Eac3 => self.audio_push(ti, p, Framing::Ac3, q),
            StreamKind::MpegAudio => self.audio_push(ti, p, Framing::Mpa, q),
            StreamKind::Dts => self.audio_push(ti, p, Framing::Dts, q),
            StreamKind::TrueHd => self.truehd_push(ti, p, q),
            StreamKind::Lpcm => self.lpcm_push(ti, p, q),
            StreamKind::DvdLpcm => self.dvd_lpcm_push(ti, p, q),
            StreamKind::DvdSub if !p.payload.is_empty() => {
                for (unit, pts) in self.spu.push(p.payload, p.pts) {
                    self.emit(
                        q,
                        Queued {
                            track: ti,
                            off: p.off,
                            pts,
                            dts: pts,
                            dur: None,
                            key: true,
                            corrupt: p.bad,
                            derived: false,
                            data: unit,
                        },
                    );
                }
            }
            StreamKind::Pgs | StreamKind::DvbSub if !p.payload.is_empty() => {
                self.emit(
                    q,
                    Queued {
                        track: ti,
                        off: p.off,
                        pts: p.pts,
                        dts: p.dts,
                        dur: None,
                        key: true,
                        corrupt: p.bad,
                        derived: false,
                        data: p.payload.to_vec(),
                    },
                );
            }
            _ => {}
        }
    }

    /// Flushes whatever is buffered (end of stream, PMT removal).
    pub(crate) fn flush(&mut self, ti: usize, q: &mut VecDeque<Queued>) {
        self.ps_flush(ti, q);
        self.flush_au(ti, q);
        if let Some(f) = self.pending.take() {
            self.emit_frame(ti, f, q);
        }
    }

    pub(crate) fn emit(&mut self, q: &mut VecDeque<Queued>, item: Queued) {
        self.packets += 1;
        if let Some(pts) = item.pts {
            self.min_pts = Some(self.min_pts.map_or(pts, |m| m.min(pts)));
            let end = pts + item.dur.unwrap_or(0);
            self.max_end = Some(self.max_end.map_or(end, |m| m.max(end)));
        }
        if self.kind.is_video()
            && self.dts_seen.len() < 48
            && let Some(d) = item.dts.or(item.pts)
        {
            self.dts_seen.push(d);
        }
        q.push_back(item);
    }

    // ---- video ------------------------------------------------------------

    fn video_push(&mut self, ti: usize, p: &PesIn<'_>, q: &mut VecDeque<Queued>) {
        if p.pts.is_some() {
            self.flush_au(ti, q);
        }
        let au = self.au.get_or_insert_with(|| Au {
            data: Vec::new(),
            pts: p.pts,
            dts: p.dts,
            off: p.off,
            rai: p.rai,
            bad: false,
        });
        if au.data.len() + p.payload.len() > MAX_PES {
            au.bad = true;
        } else {
            au.data.extend_from_slice(p.payload);
        }
        au.bad |= p.bad;
    }

    fn flush_au(&mut self, ti: usize, q: &mut VecDeque<Queued>) {
        let Some(au) = self.au.take() else {
            return;
        };
        if au.data.is_empty() {
            return;
        }
        if matches!(self.kind, StreamKind::Mpeg1Video | StreamKind::Mpeg2Video) {
            self.flush_mpeg2(ti, au, q);
            return;
        }
        self.emit_annexb(ti, au, q);
    }

    /// Emits an H.264/HEVC access unit (split further on access unit delimiters when
    /// one PES carries several progressive pictures).
    pub(crate) fn emit_annexb(&mut self, ti: usize, au: Au, q: &mut VecDeque<Queued>) {
        let hevc = self.kind == StreamKind::Hevc;
        let scan = scan_au(&au.data, hevc);
        for (dst, src) in [
            (&mut self.cfg.sps, &scan.sps),
            (&mut self.cfg.pps, &scan.pps),
            (&mut self.cfg.vps, &scan.vps),
        ] {
            for s in src {
                if dst.len() < 8 && !dst.contains(s) {
                    // A changed set replaces the stale ones of its kind only when the
                    // list is full; keeping the first keeps extradata stable.
                    dst.push(s.clone());
                }
            }
        }
        let split_ok = scan.aud >= 2
            && (hevc
                || self
                    .cfg
                    .sps
                    .first()
                    .and_then(|s| codec::h264_facts(s))
                    .is_some_and(|f| f.frame_mbs_only));
        if !split_ok {
            self.emit(
                q,
                Queued {
                    track: ti,
                    off: au.off,
                    pts: au.pts,
                    dts: au.dts,
                    dur: None,
                    key: scan.key || au.rai,
                    corrupt: au.bad,
                    derived: false,
                    data: au.data,
                },
            );
            return;
        }
        let mut cuts: Vec<usize> = vec![0];
        for &a in &scan.aud_at {
            let mut s = a;
            if s > 0 && au.data[s - 1] == 0 {
                s -= 1;
            }
            if s > *cuts.last().unwrap_or(&0) {
                cuts.push(s);
            }
        }
        cuts.push(au.data.len());
        for (i, w) in cuts.windows(2).enumerate() {
            let piece = &au.data[w[0]..w[1]];
            let key = if i == 0 {
                scan_au(piece, hevc).key || au.rai
            } else {
                scan_au(piece, hevc).key
            };
            self.emit(
                q,
                Queued {
                    track: ti,
                    off: au.off,
                    pts: if i == 0 { au.pts } else { None },
                    dts: if i == 0 { au.dts } else { None },
                    dur: None,
                    key,
                    corrupt: au.bad,
                    derived: false,
                    data: piece.to_vec(),
                },
            );
        }
    }

    /// MPEG-1/2 video: one packet per coded picture, with the sequence, GOP and
    /// extension headers in front of it. A PES carrying several pictures is cut in
    /// front of each later picture's header run (the later pieces carry no
    /// timestamps); a picture spread over several PES with timestamps on the
    /// continuation is not re-joined.
    fn flush_mpeg2(&mut self, ti: usize, au: Au, q: &mut VecDeque<Queued>) {
        let sc = codec::mpeg2_scan(&au.data);
        if self.cfg.mpeg2.is_none() {
            self.cfg.mpeg2 = sc.facts.clone();
        }
        let mut cuts: Vec<usize> = vec![0];
        for &s in sc.starts.iter().skip(1) {
            if s > *cuts.last().unwrap_or(&0) {
                cuts.push(s);
            }
        }
        cuts.push(au.data.len());
        for (i, w) in cuts.windows(2).enumerate() {
            let piece = &au.data[w[0]..w[1]];
            let key = if cuts.len() == 2 {
                sc.key
            } else {
                codec::mpeg2_scan(piece).key
            };
            self.emit(
                q,
                Queued {
                    track: ti,
                    off: au.off,
                    pts: if i == 0 { au.pts } else { None },
                    dts: if i == 0 { au.dts } else { None },
                    dur: None,
                    key: key || (i == 0 && au.rai),
                    corrupt: au.bad,
                    derived: false,
                    data: piece.to_vec(),
                },
            );
        }
    }

    // ---- audio ------------------------------------------------------------

    fn ticks(&self, samples: u64, rate: u32) -> i64 {
        let r = u64::from(rate.max(1));
        (samples.saturating_mul(TICKS as u64) / r) as i64
    }

    fn audio_push(&mut self, ti: usize, p: &PesIn<'_>, framing: Framing, q: &mut VecDeque<Queued>) {
        if p.bad {
            self.carry.clear();
        }
        let new_start = self.carry.len();
        let mut data = std::mem::take(&mut self.carry);
        if data.is_empty() {
            self.carry_off = p.off;
        }
        data.extend_from_slice(p.payload);
        let carry_off = self.carry_off;
        let mut cur = 0usize;
        let mut anchored = false;
        while cur < data.len() {
            if data.len() - cur < header_lookahead(framing) {
                if data[cur] == 0xFF
                    || data[cur] == 0x0B
                    || (framing == Framing::Dts && matches!(data[cur], 0x7F | 0xFE | 0x1F))
                {
                    break;
                }
                cur += 1;
                continue;
            }
            let Some(h) = frame_header(framing, &data[cur..]) else {
                cur += 1;
                continue;
            };
            if h.len > MAX_FRAME {
                cur += 1;
                continue;
            }
            if cur + h.len > data.len() {
                // Program streams: the packet's timestamp belongs to the first frame that
                // starts in it, complete or not (a read that begins at this packet needs it).
                if self.psv.ps && cur >= new_start && !anchored && p.pts.is_some() {
                    self.anchor = p.pts;
                }
                break;
            }
            let next = cur + h.len;
            if data.len() - next >= header_lookahead(framing)
                && frame_header(framing, &data[next..]).is_none()
            {
                // Not followed by another frame: a false sync inside payload.
                cur += 1;
                continue;
            }
            if self.cfg.hdr.is_none() {
                self.cfg.hdr = Some(h);
            }
            if cur >= new_start && !anchored && p.pts.is_some() {
                self.anchor = p.pts;
                anchored = true;
            }
            // The first frame of a PES takes the PES timestamp; the ones after it
            // follow at one frame duration, rounded down, each (as ffmpeg does).
            let pts = self.anchor;
            let dur = self.ticks(u64::from(h.samples), h.rate);
            self.anchor = pts.map(|a| a.saturating_add(dur));
            let frame = Frame {
                data: data[cur..next].to_vec(),
                pts,
                dur,
                off: if cur < new_start { carry_off } else { p.off },
                bad: p.bad,
            };
            if framing == Framing::Ac3 && self.kind == StreamKind::Eac3 {
                match (&mut self.pending, h.join) {
                    (Some(prev), true) => prev.data.extend_from_slice(&frame.data),
                    _ => {
                        if let Some(prev) = self.pending.replace(frame) {
                            self.emit_frame(ti, prev, q);
                        }
                    }
                }
            } else {
                self.emit_frame(ti, frame, q);
            }
            cur = next;
        }
        let rest = &data[cur.min(data.len())..];
        if rest.len() <= MAX_FRAME {
            self.carry = rest.to_vec();
            if cur >= new_start {
                self.carry_off = p.off;
            }
        }
    }

    /// TrueHD (stream type 0x83): one packet per access unit, the length word and
    /// everything after it, which is what `ec_truehd::TrueHdDecoder` takes. Blu-ray
    /// interleaves AC-3 core frames (sync 0x0B77) in the same PID. They are dropped:
    /// the TrueHD access units carry the same audio, so they take no time on the
    /// timeline. An access unit is accepted only when its substream directory sums
    /// to its length word; a position that fails both tests is skipped one byte at
    /// a time, as the other audio packetisers do.
    fn truehd_push(&mut self, ti: usize, p: &PesIn<'_>, q: &mut VecDeque<Queued>) {
        if p.bad {
            self.carry.clear();
        }
        let new_start = self.carry.len();
        let mut data = std::mem::take(&mut self.carry);
        if data.is_empty() {
            self.carry_off = p.off;
        }
        data.extend_from_slice(p.payload);
        let carry_off = self.carry_off;
        let mut cur = 0usize;
        let mut anchored = false;
        while cur < data.len() {
            let rest = &data[cur..];
            if rest.len() < TRUEHD_LOOKAHEAD {
                break;
            }
            if let Some(h) = codec::ac3_header(rest).map(|(h, _)| h) {
                if h.len > rest.len() {
                    break;
                }
                cur += h.len;
                continue;
            }
            let Some(len) = frame_length(rest).ok().filter(|&l| l <= MAX_FRAME) else {
                cur += 1;
                continue;
            };
            if len > rest.len() {
                // The access unit goes on in the next PES.
                break;
            }
            let Ok(head) = AccessUnitHeader::parse(&rest[..len]) else {
                cur += 1;
                continue;
            };
            if let Some(m) = head.major_sync
                && let Ok(layout) = head.channel_layout()
            {
                self.cfg.truehd = Some(TrueHdFacts {
                    rate: m.sample_rate,
                    layout,
                    // A major sync's rate code sizes the access units: 40 << n samples.
                    samples: 40 << (m.rate_code & 7),
                });
            }
            if cur >= new_start && !anchored && p.pts.is_some() {
                self.anchor = p.pts;
                anchored = true;
            }
            let (pts, dur) = match self.cfg.truehd.as_ref().map(|t| (t.rate, t.samples)) {
                Some((rate, samples)) => {
                    let dur = self.ticks(u64::from(samples), rate);
                    let pts = self.anchor;
                    self.anchor = pts.map(|a| a.saturating_add(dur));
                    (pts, dur)
                }
                None => (self.anchor, 0),
            };
            let frame = Frame {
                data: rest[..len].to_vec(),
                pts,
                dur,
                off: if cur < new_start { carry_off } else { p.off },
                bad: p.bad,
            };
            self.emit_frame(ti, frame, q);
            cur += len;
        }
        let rest = &data[cur.min(data.len())..];
        if rest.len() <= MAX_FRAME {
            self.carry = rest.to_vec();
            if cur >= new_start {
                self.carry_off = p.off;
            }
        }
    }

    fn emit_frame(&mut self, ti: usize, f: Frame, q: &mut VecDeque<Queued>) {
        if f.pts.is_none() && self.anchor.is_none() && self.packets == 0 {
            // Frames before the first timestamp cannot be placed on the timeline.
            return;
        }
        self.emit(
            q,
            Queued {
                track: ti,
                off: f.off,
                pts: f.pts,
                dts: None,
                dur: Some(f.dur),
                key: true,
                corrupt: f.bad,
                derived: false,
                data: f.data,
            },
        );
    }

    // ---- LPCM -------------------------------------------------------------

    fn lpcm_push(&mut self, ti: usize, p: &PesIn<'_>, q: &mut VecDeque<Queued>) {
        let Some(h) = codec::parse_lpcm_header(p.payload) else {
            return;
        };
        if self.cfg.lpcm.is_none() {
            self.cfg.lpcm = Some(h);
        }
        let body = &p.payload[4..];
        let body = &body[..h.payload_size.min(body.len())];
        // Odd channel counts are stored with one zero channel at the end of each frame.
        let width = usize::from(h.bits.div_ceil(8));
        let kept = usize::from(h.channels) * width;
        let stored = (usize::from(h.channels) + usize::from(h.padded)) * width;
        let mut data = Vec::with_capacity(body.len());
        for frame in body.chunks_exact(stored) {
            let frame = &frame[..kept];
            if h.bits == 16 {
                data.extend_from_slice(frame);
            } else {
                // 20 and 24 bit big-endian samples sit in 3 bytes; the family's PCM id
                // is little-endian.
                for s in frame.as_chunks::<3>().0 {
                    data.extend_from_slice(&[s[2], s[1], s[0]]);
                }
            }
        }
        let samples = (body.len() / stored) as u64;
        self.emit(
            q,
            Queued {
                track: ti,
                off: p.off,
                pts: p.pts,
                dts: None,
                dur: Some(self.ticks(samples, h.rate)),
                key: true,
                corrupt: p.bad,
                derived: false,
                data,
            },
        );
    }
}

impl Track {
    /// DVD-Video LPCM: the payload (after the 4-byte substream header) starts with the
    /// 3-byte audio header; samples are big-endian, 16 bit as is, 20 and 24 bit in
    /// groups of two frames (high 16 bits of every sample, then the low bytes/nibbles).
    fn dvd_lpcm_push(&mut self, ti: usize, p: &PesIn<'_>, q: &mut VecDeque<Queued>) {
        let Some(h) = codec::parse_dvd_lpcm_header(p.payload) else {
            return;
        };
        if self.cfg.lpcm.is_none() {
            self.cfg.lpcm = Some(h);
        }
        let ch = usize::from(h.channels);
        // A group of samples can straddle two packets: the rest waits in `carry`.
        let mut body = std::mem::take(&mut self.carry);
        body.extend_from_slice(&p.payload[3..]);
        let (group_in, frames_per_group) = match h.bits {
            16 => (2 * ch, 1usize),
            20 => (5 * ch, 2),
            _ => (6 * ch, 2),
        };
        let groups = body.len() / group_in;
        let mut data = Vec::with_capacity(groups * frames_per_group * ch * 3);
        for g in body.chunks_exact(group_in) {
            if h.bits == 16 {
                data.extend_from_slice(g);
                continue;
            }
            let n = 2 * ch;
            for i in 0..n {
                let hi = u32::from(u16::from_be_bytes([g[2 * i], g[2 * i + 1]]));
                let lo = if h.bits == 24 {
                    u32::from(g[4 * ch + i])
                } else {
                    let b = u32::from(g[4 * ch + i / 2]);
                    (if i % 2 == 0 { b >> 4 } else { b & 15 }) << 4
                };
                let v = (hi << 8) | lo;
                data.extend_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8]);
            }
        }
        self.carry = body[groups * group_in..].to_vec();
        if data.is_empty() {
            return;
        }
        let samples = (groups * frames_per_group) as u64;
        self.emit(
            q,
            Queued {
                track: ti,
                off: p.off,
                pts: p.pts,
                dts: None,
                dur: Some(self.ticks(samples, h.rate)),
                key: true,
                corrupt: p.bad,
                derived: false,
                data,
            },
        );
    }
}

fn pixel_format(chroma: u8, depth: u8) -> Option<PixelFormat> {
    match (chroma, depth) {
        (1, 8) => Some(PixelFormat::I420),
        (1, 10) => Some(PixelFormat::I010),
        (2, 8) => Some(PixelFormat::I422),
        (2, 10) => Some(PixelFormat::I210),
        (3, 8) => Some(PixelFormat::I444),
        (3, 10) => Some(PixelFormat::I410),
        _ => None,
    }
}
