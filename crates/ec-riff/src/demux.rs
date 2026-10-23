//! RIFF/AVI audio and video demuxer.
//!
//! [`AviDemuxer`] reads the stream headers (`strh`/`strf`/`vprp`/`indx`), builds a complete
//! chunk table at open and then serves packets in file order through the family's
//! [`Demuxer`] trait. The older, audio-only [`crate::AviReader`] is untouched.
//!
//! Index sources, in order of preference:
//! 1. OpenDML: the `indx` super-index and its `ix##` standard indices (files over 1 GB are
//!    split into `RIFF AVI ` plus `RIFF AVIX` chunks and only this index spans them);
//! 2. the legacy `idx1`;
//! 3. a linear walk of every `movi` list (and `rec ` lists in them) when there is no index, or
//!    when the index names no chunk of any known stream. A truncated file ends at the last whole
//!    chunk.
//!
//! Timestamps. Every stream's time base is its `dwScale/dwRate`. A video packet's pts is its
//! frame slot number: the n-th chunk of the stream is frame n, so a zero-size chunk (a dropped
//! or repeated frame) emits no packet but still takes its slot and the following pts jumps.
//! An audio stream with a fixed `dwSampleSize` counts samples (bytes / sample size); any other
//! audio chunk is one unit. `dts == pts` always.
//!
//! Limit, B-frames: AVI has no per-packet timestamps. Packets come in decode order, so for an
//! H.264 or MPEG-2 stream with B-frames the pts is the *decode-order slot*, not the true
//! presentation time; only the decoder (POC / temporal reference) knows the display order.
//! MPEG-4 Part 2 "packed bitstream" (a P-VOP and a B-VOP in one chunk, the B slot filled by a
//! tiny N-VOP) is carried as is: `ec-mpeg4` unpacks it, and its pts is again the slot number.
//!
//! Keyframes come from the index (`AVIIF_KEYFRAME`, or the size's top bit in OpenDML). Without
//! one the reader sniffs the packet: H.264 key means an IDR (or SPS) NAL precedes the first
//! slice; MPEG-2 key means an I picture; MPEG-4 Part 2 key means an I-VOP. Audio and intra-only video (Mjpeg, ProRes) are
//! always key.

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};

use crate::rawvideo;

use ec_core::frame::{ChannelLayout, Interlace, SampleFormat};
use ec_core::registry::{
    AudioParameters, CodecId, CodecParameters, Demuxer, MediaParameters, MediaType, Rotation,
    SeekMode, StreamInfo, VideoParameters,
};
use ec_core::{Buf, Error, Packet, PacketFlags, Result, Rounding, TimeBase, Timestamp};

/// Hard cap on chunks held in the table.
const MAX_ENTRIES: usize = 1 << 24;
/// Deepest `rec ` list nesting the scan walks.
const MAX_REC_DEPTH: usize = 16;
/// Largest header-chunk body kept in memory.
const MAX_HEADER_BODY: u64 = 16 << 20;

/// True when `head` (the first 12 bytes or more) starts an AVI file.
pub fn is_avi(head: &[u8]) -> bool {
    head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"AVI "
}

/// Which index the chunk table came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AviIndexKind {
    /// OpenDML super-index and standard indices.
    OpenDml,
    /// Legacy `idx1`.
    Idx1,
    /// No usable index: the table was built by walking `movi`.
    Scan,
}

/// What the file said about one `strl` stream, supported or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AviTrackInfo {
    /// AVI stream number (the two digits in chunk ids).
    pub number: u32,
    /// Audio or video; `None` for a stream type this reader does not carry (text, midi).
    pub media: Option<MediaType>,
    /// Video: `biCompression` (or the `strh` handler when that is zero). Audio: `wFormatTag`
    /// in the first two bytes. Zero when unknown.
    pub tag: [u8; 4],
    /// Index in [`AviDemuxer::streams`], or `None` when the stream is refused.
    pub stream: Option<u32>,
    /// Why the stream is refused, by name.
    pub unsupported: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    track: u32,
    /// File offset of the chunk header.
    offset: u64,
    size: u32,
    key: bool,
    /// Stream units at the chunk start.
    pts: i64,
    units: u32,
}

struct Track {
    info: AviTrackInfo,
    time_base: TimeBase,
    sample_size: u32,
    video_codec: Option<CodecId>,
    field_order: Option<Interlace>,
    /// Entry indices of this track in file order.
    idxs: Vec<u32>,
    cursor: usize,
    /// DV type-1 (`iavs`) stream: the audio streams split out of each frame chunk.
    dv: Option<DvTrack>,
}

#[derive(Default)]
struct DvTrack {
    /// `(stream index, DIF stereo pair)` for every audio stream split off this track.
    audio: Vec<(u32, usize)>,
    next_audio_pts: i64,
}

/// A demuxer over an AVI file: audio and video.
pub struct AviDemuxer<R> {
    inner: R,
    streams: Vec<StreamInfo>,
    tracks: Vec<Track>,
    entries: Vec<Entry>,
    kind: AviIndexKind,
    pending: VecDeque<Packet>,
}

struct Strl {
    handler: [u8; 4],
    kind: [u8; 4],
    scale: u32,
    rate: u32,
    length: u32,
    sample_size: u32,
    strf: Vec<u8>,
    indx: Vec<u8>,
    vprp: Option<Interlace>,
    progressive_vprp: bool,
}

struct Raw {
    number: u32,
    id: [u8; 4],
    offset: u64,
    size: u32,
    key: bool,
}

impl<R: Read + Seek> AviDemuxer<R> {
    /// Parse the headers and build the chunk table.
    pub fn new(mut inner: R) -> Result<AviDemuxer<R>> {
        let end = inner.seek(SeekFrom::End(0))?;
        inner.rewind()?;
        let mut head = [0u8; 12];
        read_full(&mut inner, &mut head)?;
        if !is_avi(&head) {
            return Err(Error::corrupt("not a RIFF/AVI file"));
        }
        let mut strls: Vec<Strl> = Vec::new();
        let mut total_frames = 0u32;
        let mut movis: Vec<(u64, u64)> = Vec::new();
        let mut idx1: Vec<u8> = Vec::new();
        let mut pos = 12u64;
        while pos + 8 <= end {
            let Some((id, size)) = chunk_head(&mut inner, pos)? else {
                break;
            };
            let data = pos + 8;
            let next = data.saturating_add(size).saturating_add(size & 1);
            match &id {
                // A second RIFF (AVIX) is walked flat: its children follow its form type.
                b"RIFF" => {
                    pos = data + 4;
                    continue;
                }
                b"LIST" => {
                    let mut kind = [0u8; 4];
                    if size >= 4 && read_some(&mut inner, &mut kind)? == 4 {
                        let body = data + 4;
                        let body_end = data.saturating_add(size).min(end);
                        match &kind {
                            b"hdrl" => parse_hdrl(
                                &mut inner,
                                body,
                                body_end,
                                &mut strls,
                                &mut total_frames,
                            )?,
                            b"movi" => movis.push((body, body_end)),
                            _ => {}
                        }
                    }
                }
                b"idx1" if idx1.is_empty() => {
                    let n = size.min(end.saturating_sub(data));
                    idx1 = read_body(&mut inner, n)?;
                }
                _ => {}
            }
            pos = next;
        }
        if movis.is_empty() {
            return Err(Error::corrupt("AVI: no movi list"));
        }

        // Streams and tracks.
        let mut tracks: Vec<Track> = Vec::new();
        let mut streams: Vec<StreamInfo> = Vec::new();
        let mut first_refusal: Option<String> = None;
        for (number, s) in strls.iter().enumerate() {
            let number = number as u32;
            let (info, params, time_base, codec, field_order) = describe(number, s);
            let mut info = info;
            if let Some(p) = params {
                let index = streams.len() as u32;
                info.stream = Some(index);
                streams.push(StreamInfo {
                    index,
                    time_base,
                    params: p,
                    start_time: Some(0),
                    duration: (s.length > 0).then_some(i64::from(s.length)),
                    language: None,
                    initial_padding: 0,
                    default: false,
                    rotation: Rotation::None,
                });
            } else if first_refusal.is_none() {
                first_refusal = info.unsupported.clone();
            }
            tracks.push(Track {
                info,
                time_base,
                sample_size: s.sample_size,
                video_codec: codec,
                field_order,
                idxs: Vec::new(),
                cursor: 0,
                dv: (&s.kind == b"iavs").then(DvTrack::default),
            });
        }
        if streams.is_empty() {
            return Err(Error::unsupported(
                first_refusal.unwrap_or_else(|| "AVI without audio or video streams".into()),
                "ec-riff carries the audio and video streams it has a codec id for",
            ));
        }

        // Index.
        let movi0 = movis[0].0;
        let mut raw: Vec<Raw> = Vec::new();
        let mut kind = AviIndexKind::Scan;
        let mut odml: Vec<Raw> = Vec::new();
        for (n, s) in strls.iter().enumerate() {
            if !s.indx.is_empty() {
                odml.extend(read_odml(&mut inner, &s.indx, n as u32, end)?);
            }
        }
        if has_known(&odml, &tracks) {
            raw = odml;
            kind = AviIndexKind::OpenDml;
        } else if !idx1.is_empty() {
            let legacy = resolve_idx1(&mut inner, movi0, end, &tracks, &idx1)?;
            if has_known(&legacy, &tracks) {
                raw = legacy;
                kind = AviIndexKind::Idx1;
            }
        }
        if kind == AviIndexKind::Scan {
            for &(start, stop) in &movis {
                scan_range(&mut inner, start, stop, &tracks, &mut raw, 0)?;
            }
        }
        let sniff = kind == AviIndexKind::Scan;

        // Chunk table: file order, deduplicated, with per-stream clocks.
        raw.retain(|r| {
            tracks
                .get(r.number as usize)
                .is_some_and(|t| t.info.stream.is_some())
                && r.offset.saturating_add(8).saturating_add(u64::from(r.size)) <= end
        });
        raw.sort_by_key(|r| r.offset);
        raw.dedup_by_key(|r| r.offset);
        if raw.len() > MAX_ENTRIES {
            return Err(Error::corrupt("AVI: too many chunks"));
        }
        let mut clocks = vec![0i64; tracks.len()];
        let mut entries = Vec::with_capacity(raw.len());
        for r in &raw {
            let t = r.number as usize;
            let video = tracks[t].video_codec.is_some();
            // A zero-size video chunk is a dropped frame and keeps its slot; an empty audio chunk
            // (ffmpeg pads B-frame interleave with them) is not an audio frame at all.
            let units = match tracks[t].sample_size {
                _ if r.size == 0 && !video => 0u32,
                0 => 1u32,
                n if !video => r.size / n,
                _ => 1,
            };
            let mut key = r.key;
            match tracks[t].video_codec {
                None
                | Some(CodecId::Mjpeg | CodecId::ProRes | CodecId::DvVideo | CodecId::RawVideo) => {
                    key = true
                }
                Some(codec) if sniff && r.size > 0 => {
                    key = sniff_key(&mut inner, codec, r.offset + 8, r.size)?;
                }
                _ => {}
            }
            let idx = entries.len() as u32;
            entries.push(Entry {
                track: r.number,
                offset: r.offset,
                size: r.size,
                key,
                pts: clocks[t],
                units,
            });
            clocks[t] += i64::from(units);
            tracks[t].idxs.push(idx);
        }
        // Duration: the frame count the index shows (OpenDML `dmlh` for the first video).
        for (t, track) in tracks.iter().enumerate() {
            if let Some(si) = track.info.stream {
                let mut d = clocks[t];
                if d == 0 {
                    d = streams[si as usize].duration.unwrap_or(0);
                }
                if track.video_codec.is_some() && total_frames > 0 && kind == AviIndexKind::OpenDml
                {
                    d = d.max(i64::from(total_frames));
                }
                streams[si as usize].duration = (d > 0).then_some(d);
            }
        }
        // DV type-1: the first frame names the picture and the audio pairs.
        for track in tracks.iter_mut() {
            let Some(si) = track.info.stream else {
                continue;
            };
            if track.dv.is_none() {
                continue;
            }
            let first = track
                .idxs
                .iter()
                .map(|&i| entries[i as usize])
                .find(|e| e.size > 0);
            let Some(e) = first else { continue };
            inner.seek(SeekFrom::Start(e.offset + 8))?;
            let mut data = vec![0u8; e.size as usize];
            if read_some(&mut inner, &mut data)? < data.len() {
                continue;
            }
            let Some(sys) = ec_dv::frame_profile(None, &data) else {
                continue;
            };
            if let MediaParameters::Video(v) = &mut streams[si as usize].params.media {
                v.width = sys.width;
                v.height = sys.height;
                v.format = Some(ec_dv::output_format(sys.chroma));
                v.sample_aspect_ratio = Some(TimeBase::new(
                    i64::from(sys.sar[0].0),
                    i64::from(sys.sar[0].1),
                ));
            }
            if let Some(a) = ec_dv::extract_audio(&data) {
                for pair in 0..a.pairs.len() {
                    let mut ap = CodecParameters::new(CodecId::PcmS16Le);
                    ap.media = MediaParameters::Audio(AudioParameters {
                        sample_rate: a.sample_rate,
                        layout: ChannelLayout::Stereo,
                        format: Some(SampleFormat::S16),
                        bits_per_sample: Some(16),
                    });
                    let index = streams.len() as u32;
                    let mut st = StreamInfo::new(index, TimeBase::from_rate(a.sample_rate), ap);
                    st.start_time = Some(0);
                    streams.push(st);
                    if let Some(dv) = &mut track.dv {
                        dv.audio.push((index, pair));
                    }
                }
            }
        }
        Ok(AviDemuxer {
            inner,
            streams,
            tracks,
            entries,
            kind,
            pending: VecDeque::new(),
        })
    }

    /// Every stream the file declared, including the refused ones.
    pub fn tracks(&self) -> Vec<AviTrackInfo> {
        self.tracks.iter().map(|t| t.info.clone()).collect()
    }

    /// Scan order for `stream`: the interlace the file states. [`Interlace::BottomFieldFirst`]
    /// for every Mjpeg stream (ffmpeg's convention for AVI: the first JPEG of a two-field
    /// packet holds the bottom lines; harmless for progressive packets). Otherwise from the
    /// `vprp` header: [`Interlace::Progressive`] for one field per frame, the field with the
    /// smaller `VideoYValidStartLine` first for two. `None` where the file states nothing.
    pub fn field_order(&self, stream: u32) -> Option<Interlace> {
        self.tracks
            .iter()
            .find(|t| t.info.stream == Some(stream))
            .and_then(|t| t.field_order)
    }

    /// Where the chunk table came from.
    pub fn index_kind(&self) -> AviIndexKind {
        self.kind
    }

    /// Chunks (zero-size ones included) of `stream`.
    pub fn chunk_count(&self, stream: u32) -> usize {
        self.tracks
            .iter()
            .find(|t| t.info.stream == Some(stream))
            .map_or(0, |t| t.idxs.len())
    }

    /// Back to the first packet of every stream.
    pub fn rewind(&mut self) {
        self.pending.clear();
        for t in &mut self.tracks {
            t.cursor = 0;
            if let Some(dv) = &mut t.dv {
                dv.next_audio_pts = 0;
            }
        }
    }

    /// Seek `stream` as [`Demuxer::seek`] and return the instant it landed on (the chosen
    /// packet's pts, in that stream's time base). Other streams resume at the same instant.
    pub fn seek_stream(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<Timestamp> {
        let Some(ti) = self.tracks.iter().position(|t| {
            t.info.stream == Some(stream)
                || t.dv
                    .as_ref()
                    .is_some_and(|d| d.audio.iter().any(|a| a.0 == stream))
        }) else {
            return Err(Error::corrupt(format!("avi: no stream {stream} to seek")));
        };
        self.pending.clear();
        if let Some(dv) = &mut self.tracks[ti].dv {
            dv.next_audio_pts = 0;
        }
        let tb = self.tracks[ti].time_base;
        let target = to.rescale(tb, Rounding::Nearest).ticks;
        let entries = &self.entries;
        let idxs = &self.tracks[ti].idxs;
        if idxs.is_empty() {
            return Err(Error::Eof);
        }
        let mut at = idxs
            .partition_point(|&i| entries[i as usize].pts <= target)
            .saturating_sub(1);
        match mode {
            SeekMode::Exact => {}
            SeekMode::SyncBefore => {
                while at > 0 && !entries[idxs[at] as usize].key {
                    at -= 1;
                }
            }
            SeekMode::SyncAfter => {
                while at + 1 < idxs.len() && !entries[idxs[at] as usize].key {
                    at += 1;
                }
            }
        }
        let landed = Timestamp::new(entries[idxs[at] as usize].pts, tb);
        self.tracks[ti].cursor = at;
        for j in 0..self.tracks.len() {
            if j == ti {
                continue;
            }
            let want = landed
                .rescale(self.tracks[j].time_base, Rounding::Nearest)
                .ticks;
            let entries = &self.entries;
            let t = &mut self.tracks[j];
            t.cursor = t.idxs.partition_point(|&i| entries[i as usize].pts < want);
        }
        Ok(landed)
    }

    fn read_next(&mut self) -> Result<Packet> {
        if let Some(p) = self.pending.pop_front() {
            return Ok(p);
        }
        loop {
            let mut pick: Option<(usize, u64)> = None;
            for (i, t) in self.tracks.iter_mut().enumerate() {
                // Zero-size chunks are dropped frames: they hold a slot but yield no packet.
                while let Some(&e) = t.idxs.get(t.cursor) {
                    if self.entries[e as usize].size == 0 {
                        t.cursor += 1;
                    } else {
                        break;
                    }
                }
                if let Some(&e) = t.idxs.get(t.cursor) {
                    let off = self.entries[e as usize].offset;
                    if pick.is_none_or(|(_, best)| off < best) {
                        pick = Some((i, off));
                    }
                }
            }
            let Some((ti, _)) = pick else {
                return Err(Error::Eof);
            };
            let e = self.entries[self.tracks[ti].idxs[self.tracks[ti].cursor] as usize];
            self.tracks[ti].cursor += 1;
            self.inner.seek(SeekFrom::Start(e.offset))?;
            let mut head = [0u8; 8];
            if read_some(&mut self.inner, &mut head)? < 8 {
                return Err(Error::Eof);
            }
            let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
            // A stale index entry (id or size not matching the chunk): skip it.
            if stream_number(&[head[0], head[1], head[2], head[3]]) != Some(e.track)
                || size != e.size
            {
                continue;
            }
            let mut data = vec![0u8; e.size as usize];
            if read_some(&mut self.inner, &mut data)? < data.len() {
                return Err(Error::Eof);
            }
            let stream = self.tracks[ti].info.stream.unwrap_or(0);
            let vtb = self.tracks[ti].time_base;
            if let Some(dv) = &mut self.tracks[ti].dv
                && let Some(a) = ec_dv::extract_audio(&data)
            {
                {
                    // Same clock rule as the raw DV demuxer: follow the video unless the audio
                    // runs within a frame of it.
                    let tb = TimeBase::from_rate(a.sample_rate);
                    let dur = a.samples as i64;
                    let vpts = vtb.rescale(e.pts, tb, Rounding::Nearest);
                    let pts = if (vpts - dv.next_audio_pts).abs() >= dur {
                        vpts
                    } else {
                        dv.next_audio_pts
                    };
                    dv.next_audio_pts = pts + dur;
                    for &(si, pair) in &dv.audio {
                        let Some(pcm) = a.pairs.get(pair).filter(|p| !p.is_empty()) else {
                            continue;
                        };
                        let mut bytes = Vec::with_capacity(pcm.len() * 2);
                        for s in pcm {
                            bytes.extend_from_slice(&s.to_le_bytes());
                        }
                        let mut ap = Packet::new(si, tb, Buf::from(bytes));
                        ap.pts = Some(pts);
                        ap.dts = Some(pts);
                        ap.duration = Some(dur);
                        ap.flags = PacketFlags {
                            keyframe: true,
                            ..PacketFlags::default()
                        };
                        self.pending.push_back(ap);
                    }
                }
            }
            let mut p = Packet::new(stream, self.tracks[ti].time_base, Buf::from(data));
            p.pts = Some(e.pts);
            p.dts = Some(e.pts);
            p.duration = Some(i64::from(e.units));
            p.flags = PacketFlags {
                keyframe: e.key,
                ..PacketFlags::default()
            };
            return Ok(p);
        }
    }
}

impl<R: Read + Seek + Send> Demuxer for AviDemuxer<R> {
    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    fn next_packet(&mut self) -> Result<Packet> {
        self.read_next()
    }

    fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<()> {
        self.seek_stream(stream, to, mode).map(|_| ())
    }
}

// ---------------------------------------------------------------------------------------------
// Stream description

type Described = (
    AviTrackInfo,
    Option<CodecParameters>,
    TimeBase,
    Option<CodecId>,
    Option<Interlace>,
);

fn describe(number: u32, s: &Strl) -> Described {
    let scale = i64::from(s.scale.max(1));
    let rate = i64::from(s.rate.max(1));
    let mut info = AviTrackInfo {
        number,
        media: None,
        tag: [0; 4],
        stream: None,
        unsupported: None,
    };
    let mut tb = TimeBase::new(scale, rate);
    match &s.kind {
        b"vids" => {
            info.media = Some(MediaType::Video);
            let f = &s.strf;
            if f.len() < 40 {
                info.unsupported = Some("video stream with a short BITMAPINFOHEADER".into());
                return (info, None, tb, None, None);
            }
            let width = le_u32(f, 4).unwrap_or(0);
            let height = (le_u32(f, 8).unwrap_or(0) as i32).unsigned_abs();
            let bits = u16::from_le_bytes([f[14], f[15]]);
            let comp = [f[16], f[17], f[18], f[19]];
            info.tag = if comp == [0; 4] { s.handler } else { comp };
            let codec = match map_video(&comp, bits) {
                Ok(c) => Ok(c),
                // biCompression unknown: some muxers only fill the `strh` handler.
                Err(e) => map_video(&s.handler, bits).map_err(|_| e),
            };
            if s.scale == 0 || s.rate == 0 {
                tb = TimeBase::new(1, 25);
            }
            match codec {
                Ok(codec) => {
                    let mut p = CodecParameters::new(codec);
                    if let MediaParameters::Video(v) = &mut p.media {
                        let VideoParameters {
                            width: w,
                            height: h,
                            frame_rate,
                            ..
                        } = v;
                        *w = width;
                        *h = height;
                        *frame_rate = Some(TimeBase::new(tb.den(), tb.num()));
                    }
                    let bih = le_u32(f, 0).unwrap_or(40) as usize;
                    if codec == CodecId::RawVideo {
                        let signed = le_u32(f, 8).unwrap_or(0) as i32;
                        let Some(layout) = rawvideo::layout_of(&comp, bits, signed) else {
                            info.unsupported = Some(format!(
                                "uncompressed video (FourCC '{}', {bits} bpp): raw layout not carried",
                                fourcc_str(&comp)
                            ));
                            return (info, None, tb, None, None);
                        };
                        if let MediaParameters::Video(v) = &mut p.media {
                            v.format = Some(layout.format());
                        }
                        let keep = if bits == 8 {
                            (40 + 4 * 256).min(f.len())
                        } else {
                            40
                        };
                        p.extradata = Some(Buf::from(&f[..keep]));
                    } else if f.len() > bih.max(40) && codec != CodecId::Mjpeg {
                        p.extradata = Some(Buf::from(&f[bih.max(40)..]));
                    }
                    let fo = if codec == CodecId::Mjpeg {
                        Some(Interlace::BottomFieldFirst)
                    } else if s.progressive_vprp {
                        Some(Interlace::Progressive)
                    } else {
                        s.vprp
                    };
                    (info, Some(p), tb, Some(codec), fo)
                }
                Err(why) => {
                    info.unsupported = Some(why.to_string());
                    (info, None, tb, None, None)
                }
            }
        }
        b"auds" => {
            info.media = Some(MediaType::Audio);
            if s.scale == 0 || s.rate == 0 {
                tb = TimeBase::new(1, i64::from(le_u32(&s.strf, 4).unwrap_or(0).max(1)));
            }
            match map_audio(&s.strf) {
                Ok((tag, p)) => {
                    info.tag = [tag as u8, (tag >> 8) as u8, 0, 0];
                    (info, Some(p), tb, None, None)
                }
                Err((tag, why)) => {
                    info.tag = [tag as u8, (tag >> 8) as u8, 0, 0];
                    info.unsupported = Some(why);
                    (info, None, tb, None, None)
                }
            }
        }
        b"iavs" => {
            // DV type-1: whole DV frames; the picture size and the audio pairs come from the
            // first frame once the chunk table exists.
            info.media = Some(MediaType::Video);
            info.tag = s.handler;
            if s.scale == 0 || s.rate == 0 {
                tb = TimeBase::new(1001, 30000);
            }
            let mut p = CodecParameters::new(CodecId::DvVideo);
            if let MediaParameters::Video(v) = &mut p.media {
                v.frame_rate = Some(TimeBase::new(tb.den(), tb.num()));
            }
            (info, Some(p), tb, Some(CodecId::DvVideo), None)
        }
        other => {
            info.unsupported = Some(format!(
                "AVI stream type '{}'",
                String::from_utf8_lossy(other)
            ));
            (info, None, tb, None, None)
        }
    }
}

fn fourcc_str(c: &[u8; 4]) -> String {
    c.iter()
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

/// Video FourCC to codec id. `Err` names the refusal.
fn map_video(c: &[u8; 4], bits: u16) -> std::result::Result<CodecId, String> {
    let up: Vec<u8> = c.iter().map(|b| b.to_ascii_uppercase()).collect();
    let name = fourcc_str(c);
    Ok(match (&up[..], &c[..]) {
        (b"MJPG" | b"JPEG" | b"DMB1" | b"AVRN" | b"AVDJ" | b"MJPA" | b"IJPG", _) => CodecId::Mjpeg,
        (b"H264" | b"X264" | b"AVC1" | b"DAVC" | b"VSSH" | b"HD10", _) => CodecId::H264,
        (b"MPG2" | b"MPEG" | b"MPG1" | b"MP2V" | b"M2V1" | b"HDV1" | b"HDV2", _) => {
            CodecId::Mpeg2Video
        }
        (_, b"apco" | b"apcs" | b"apcn" | b"apch" | b"ap4h" | b"ap4x") => CodecId::ProRes,
        (b"AVDN" | b"AVDH", _) => CodecId::Dnxhd,
        (
            b"XVID" | b"XVIX" | b"DIVX" | b"DX50" | b"FMP4" | b"MP4V" | b"M4S2" | b"MP4S" | b"RMP4"
            | b"DVX1" | b"BLZ0" | b"UMP4",
            _,
        ) => CodecId::Mpeg4Part2,
        (b"MPG4" | b"DIV1", _) => CodecId::MsMpeg4V1,
        (b"MP42" | b"DIV2" | b"MP41", _) => CodecId::MsMpeg4V2,
        (
            b"DIV3" | b"MP43" | b"DIV4" | b"DIV5" | b"DIV6" | b"AP41" | b"COL1" | b"COL0" | b"3IVD",
            _,
        ) => CodecId::MsMpeg4V3,
        (b"3IV2" | b"SEDG", _) => {
            return Err(format!(
                "MPEG-4 variant video (FourCC '{name}'): no decoder"
            ));
        }
        (
            b"DVSD" | b"DV25" | b"DVH1" | b"DVHD" | b"DVSL" | b"DV50" | b"DVC " | b"CDVC" | b"DVCS"
            | b"DVPP",
            _,
        ) => CodecId::DvVideo,
        (
            b"YUY2" | b"YUYV" | b"YUV2" | b"V422" | b"UYVY" | b"2VUY" | b"I420" | b"IYUV" | b"YV12"
            | b"YVYU" | b"NV12" | b"Y800" | b"Y8  " | b"GREY" | b"V210",
            _,
        ) => CodecId::RawVideo,
        (_, [0, 0, 0, 0]) | (b"RGB " | b"BGR " | b"RAW ", _)
            if matches!(bits, 8 | 15 | 16 | 24 | 32) =>
        {
            CodecId::RawVideo
        }
        (_, [0, 0, 0, 0]) => {
            return Err(format!(
                "uncompressed DIB video ({bits} bpp): raw layout is not carried"
            ));
        }
        (b"RGB " | b"BGR " | b"RAW ", _) => {
            return Err(format!(
                "uncompressed video (FourCC '{name}'): raw video is not carried"
            ));
        }
        _ => return Err(format!("video FourCC '{name}'")),
    })
}

/// Audio `strf` to parameters. `Err` carries the tag and a reason.
fn map_audio(f: &[u8]) -> std::result::Result<(u16, CodecParameters), (u16, String)> {
    if f.len() < 16 {
        return Err((0, "audio stream with a short WAVEFORMAT".into()));
    }
    let mut tag = u16::from_le_bytes([f[0], f[1]]);
    let channels = u16::from_le_bytes([f[2], f[3]]);
    let sample_rate = le_u32(f, 4).unwrap_or(0);
    let bits = u16::from_le_bytes([f[14], f[15]]);
    let cb = if f.len() >= 18 {
        usize::from(u16::from_le_bytes([f[16], f[17]]))
    } else {
        0
    };
    let extra = f
        .get(18..18 + cb.min(f.len().saturating_sub(18)))
        .unwrap_or(&[]);
    if tag == 0xFFFE && extra.len() >= 22 {
        tag = u16::from_le_bytes([extra[8], extra[9]]);
    }
    if channels == 0 || sample_rate == 0 {
        return Err((
            tag,
            format!("audio tag {tag:#06x} with zero channels or rate"),
        ));
    }
    let (codec, format, depth) = match (tag, bits) {
        (1, 8) => (CodecId::PcmU8, Some(SampleFormat::U8), Some(8)),
        (1, 16) => (CodecId::PcmS16Le, Some(SampleFormat::S16), Some(16)),
        (1, 24) => (CodecId::PcmS24Le, Some(SampleFormat::S32), Some(24)),
        (1, 32) => (CodecId::PcmS32Le, Some(SampleFormat::S32), Some(32)),
        (3, 32) => (CodecId::PcmF32Le, Some(SampleFormat::F32), Some(32)),
        (0x0011 | 0x0002 | 0x0006 | 0x0007, _) => (
            crate::WavCompressed::codec_of(tag).unwrap_or(CodecId::PcmAlaw),
            Some(SampleFormat::S16),
            Some(16),
        ),
        (0x50, _) => (CodecId::Mp2, None, None),
        (0x55, _) => (CodecId::Mp3, None, None),
        (0x2000, _) => (CodecId::Ac3, None, None),
        (0x2001, _) => (CodecId::Dts, None, None),
        (0xFF | 0x706D, _) => (CodecId::Aac, None, None),
        _ => {
            return Err((tag, format!("audio format tag {tag:#06x} ({bits}-bit)")));
        }
    };
    let mut p = CodecParameters::new(codec);
    p.media = MediaParameters::Audio(AudioParameters {
        sample_rate,
        layout: ChannelLayout::from_count(usize::from(channels)),
        format,
        bits_per_sample: depth,
    });
    if (codec == CodecId::Aac || crate::WavCompressed::codec_of(tag).is_some()) && !extra.is_empty()
    {
        p.extradata = Some(Buf::from(extra));
    }
    Ok((tag, p))
}

// ---------------------------------------------------------------------------------------------
// Header parsing

fn parse_hdrl<R: Read + Seek>(
    inner: &mut R,
    start: u64,
    end: u64,
    strls: &mut Vec<Strl>,
    total_frames: &mut u32,
) -> Result<()> {
    let mut pos = start;
    while pos + 8 <= end {
        let Some((id, size)) = chunk_head(inner, pos)? else {
            break;
        };
        let data = pos + 8;
        if &id == b"LIST" {
            let mut kind = [0u8; 4];
            if size >= 4 && read_some(inner, &mut kind)? == 4 {
                match &kind {
                    b"strl" => strls.push(parse_strl(
                        inner,
                        data + 4,
                        data.saturating_add(size).min(end),
                    )?),
                    b"odml" => {
                        // `dmlh`: dwTotalFrames.
                        let mut h = [0u8; 12];
                        if read_some(inner, &mut h)? == 12 && &h[0..4] == b"dmlh" {
                            *total_frames = le_u32(&h, 8).unwrap_or(0);
                        }
                    }
                    _ => {}
                }
            }
        }
        pos = data.saturating_add(size).saturating_add(size & 1);
    }
    Ok(())
}

fn parse_strl<R: Read + Seek>(inner: &mut R, start: u64, end: u64) -> Result<Strl> {
    let mut s = Strl {
        handler: [0; 4],
        kind: [0; 4],
        scale: 1,
        rate: 1,
        length: 0,
        sample_size: 0,
        strf: Vec::new(),
        indx: Vec::new(),
        vprp: None,
        progressive_vprp: false,
    };
    let mut pos = start;
    while pos + 8 <= end {
        let Some((id, size)) = chunk_head(inner, pos)? else {
            break;
        };
        let data = pos + 8;
        let cap = size.min(end.saturating_sub(data)).min(MAX_HEADER_BODY);
        match &id {
            b"strh" => {
                let b = read_body(inner, cap.min(64))?;
                if b.len() >= 48 {
                    s.kind = [b[0], b[1], b[2], b[3]];
                    s.handler = [b[4], b[5], b[6], b[7]];
                    s.scale = le_u32(&b, 20).unwrap_or(0);
                    s.rate = le_u32(&b, 24).unwrap_or(0);
                    s.length = le_u32(&b, 32).unwrap_or(0);
                    s.sample_size = le_u32(&b, 44).unwrap_or(0);
                }
            }
            b"strf" => s.strf = read_body(inner, cap)?,
            b"indx" => s.indx = read_body(inner, cap)?,
            b"vprp" => {
                let b = read_body(inner, cap)?;
                let nb = le_u32(&b, 32).unwrap_or(0);
                if nb == 1 {
                    s.progressive_vprp = true;
                } else if nb == 2 {
                    // VIDEO_FIELD_DESC is 8 dwords at 36; VideoYValidStartLine is the last.
                    let l0 = le_u32(&b, 36 + 28);
                    let l1 = le_u32(&b, 36 + 32 + 28);
                    s.vprp = Some(match (l0, l1) {
                        (Some(a), Some(b)) if b < a => Interlace::BottomFieldFirst,
                        _ => Interlace::TopFieldFirst,
                    });
                }
            }
            _ => {}
        }
        pos = data.saturating_add(size).saturating_add(size & 1);
    }
    Ok(s)
}

// ---------------------------------------------------------------------------------------------
// Index

fn has_known(raw: &[Raw], tracks: &[Track]) -> bool {
    raw.iter().any(|r| {
        tracks
            .get(r.number as usize)
            .is_some_and(|t| t.info.stream.is_some())
    })
}

fn is_media_id(id: &[u8; 4]) -> bool {
    matches!(&id[2..4], b"dc" | b"db" | b"wb")
}

fn resolve_idx1<R: Read + Seek>(
    inner: &mut R,
    movi_body: u64,
    end: u64,
    tracks: &[Track],
    body: &[u8],
) -> Result<Vec<Raw>> {
    let mut entries = Vec::new();
    for e in body.as_chunks::<16>().0 {
        let id = [e[0], e[1], e[2], e[3]];
        let Some(number) = stream_number(&id).filter(|_| is_media_id(&id)) else {
            continue;
        };
        if !tracks
            .get(number as usize)
            .is_some_and(|t| t.info.stream.is_some())
        {
            continue;
        }
        entries.push(Raw {
            number,
            id,
            offset: u64::from(u32::from_le_bytes([e[8], e[9], e[10], e[11]])),
            size: u32::from_le_bytes([e[12], e[13], e[14], e[15]]),
            key: u32::from_le_bytes([e[4], e[5], e[6], e[7]]) & 0x10 != 0,
        });
    }
    let Some(first) = entries.first() else {
        return Ok(entries);
    };
    // idx1 offsets are relative to the `movi` fourcc, or absolute in some files.
    let mut base = None;
    for b in [movi_body.saturating_sub(4), 0, movi_body] {
        if chunk_matches(inner, b.saturating_add(first.offset), &first.id, end)? {
            base = Some(b);
            break;
        }
    }
    let Some(base) = base else {
        return Ok(Vec::new());
    };
    for e in &mut entries {
        e.offset = base.saturating_add(e.offset);
    }
    Ok(entries)
}

fn read_odml<R: Read + Seek>(
    inner: &mut R,
    indx: &[u8],
    number: u32,
    end: u64,
) -> Result<Vec<Raw>> {
    if indx.len() < 24 {
        return Ok(Vec::new());
    }
    match indx[3] {
        1 => Ok(parse_standard(inner, indx, number, end)?),
        0 => {
            let n = le_u32(indx, 4).unwrap_or(0) as usize;
            let mut out = Vec::new();
            for i in 0..n {
                let at = 24 + i * 16;
                if at + 16 > indx.len() {
                    break;
                }
                let off = le_u64(indx, at).unwrap_or(0);
                let size = u64::from(le_u32(indx, at + 8).unwrap_or(0));
                if off == 0 || size < 24 || off.saturating_add(8) > end {
                    continue;
                }
                let Some((_, body_size)) = chunk_head(inner, off)? else {
                    continue;
                };
                let body = read_body(inner, body_size.min(end - off - 8).min(MAX_HEADER_BODY))?;
                if body.len() >= 24 && body[3] == 1 {
                    out.extend(parse_standard(inner, &body, number, end)?);
                }
                if out.len() > MAX_ENTRIES {
                    return Err(Error::corrupt("AVI: too many chunks"));
                }
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

/// An `AVI_INDEX_OF_CHUNKS` body: offsets are relative to `qwBaseOffset` and point at chunk
/// data; the top bit of the size marks a non-key chunk.
fn parse_standard<R: Read + Seek>(
    inner: &mut R,
    body: &[u8],
    number: u32,
    end: u64,
) -> Result<Vec<Raw>> {
    let longs = usize::from(u16::from_le_bytes([body[0], body[1]])).max(2);
    let n = le_u32(body, 4).unwrap_or(0) as usize;
    let id = [body[8], body[9], body[10], body[11]];
    let base = le_u64(body, 12).unwrap_or(0);
    let step = longs * 4;
    let mut out = Vec::new();
    // Offset to the chunk header: spec says data - 8; some writers point at the header.
    let mut back = 8u64;
    for i in 0..n {
        let at = 24 + i * step;
        if at + 8 > body.len() {
            break;
        }
        let off = base.saturating_add(u64::from(le_u32(body, at).unwrap_or(0)));
        let sz = le_u32(body, at + 4).unwrap_or(0);
        if i == 0 {
            let size = sz & 0x7fff_ffff;
            if !chunk_matches_sized(inner, off.saturating_sub(8), &id, size, end)?
                && chunk_matches_sized(inner, off, &id, size, end)?
            {
                back = 0;
            }
        }
        out.push(Raw {
            number,
            id,
            offset: off.saturating_sub(back),
            size: sz & 0x7fff_ffff,
            key: sz & 0x8000_0000 == 0,
        });
    }
    Ok(out)
}

fn chunk_matches<R: Read + Seek>(
    inner: &mut R,
    offset: u64,
    id: &[u8; 4],
    end: u64,
) -> Result<bool> {
    let Some((got, size)) = chunk_head(inner, offset)? else {
        return Ok(false);
    };
    Ok(&got == id && offset.saturating_add(8).saturating_add(size) <= end)
}

fn chunk_matches_sized<R: Read + Seek>(
    inner: &mut R,
    offset: u64,
    id: &[u8; 4],
    size: u32,
    end: u64,
) -> Result<bool> {
    if offset.saturating_add(8) > end {
        return Ok(false);
    }
    Ok(chunk_matches(inner, offset, id, end)?
        && chunk_head(inner, offset)?.is_some_and(|(_, s)| s == u64::from(size)))
}

/// Walk `start..end` chunk by chunk, descending into `rec ` lists.
fn scan_range<R: Read + Seek>(
    inner: &mut R,
    start: u64,
    end: u64,
    tracks: &[Track],
    out: &mut Vec<Raw>,
    depth: usize,
) -> Result<()> {
    if depth > MAX_REC_DEPTH {
        return Err(Error::corrupt("AVI rec lists nested too deeply"));
    }
    let mut pos = start;
    while pos + 8 <= end {
        let Some((id, size)) = chunk_head(inner, pos)? else {
            break;
        };
        let data = pos + 8;
        // A chunk running past the range is a truncated tail: stop.
        if data.saturating_add(size) > end {
            break;
        }
        if &id == b"LIST" {
            let mut kind = [0u8; 4];
            if size >= 4 && read_some(inner, &mut kind)? == 4 && &kind == b"rec " {
                scan_range(inner, data + 4, data + size, tracks, out, depth + 1)?;
            }
        } else if let Some(number) = stream_number(&id).filter(|_| is_media_id(&id))
            && tracks
                .get(number as usize)
                .is_some_and(|t| t.info.stream.is_some())
        {
            if out.len() >= MAX_ENTRIES {
                return Err(Error::corrupt("AVI: too many chunks"));
            }
            out.push(Raw {
                number,
                id,
                offset: pos,
                size: size as u32,
                key: true,
            });
        }
        pos = data + size + (size & 1);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Keyframe sniffing (no index)

fn sniff_key<R: Read + Seek>(inner: &mut R, codec: CodecId, data: u64, size: u32) -> Result<bool> {
    inner.seek(SeekFrom::Start(data))?;
    let mut buf = vec![0u8; (size as usize).min(4096)];
    let got = read_some(inner, &mut buf)?;
    buf.truncate(got);
    Ok(match codec {
        CodecId::H264 => h264_key(&buf),
        CodecId::Mpeg2Video => mpeg2_key(&buf),
        CodecId::Mpeg4Part2 => mpeg4_key(&buf),
        // MS-MPEG4: the first two bits are the picture type, 0 = I.
        CodecId::MsMpeg4V2 | CodecId::MsMpeg4V3 => buf.first().is_some_and(|b| b >> 6 == 0),
        _ => true,
    })
}

fn start_codes(buf: &[u8]) -> impl Iterator<Item = usize> + '_ {
    buf.windows(4)
        .enumerate()
        .filter(|(_, w)| w[0] == 0 && w[1] == 0 && w[2] == 1)
        .map(|(i, _)| i + 3)
}

fn h264_key(buf: &[u8]) -> bool {
    for at in start_codes(buf) {
        match buf[at] & 0x1f {
            5 | 7 => return true,
            1 => return false,
            _ => {}
        }
    }
    false
}

fn mpeg2_key(buf: &[u8]) -> bool {
    for at in start_codes(buf) {
        if buf[at] == 0x00
            && let Some(b) = buf.get(at + 2)
        {
            return (b >> 3) & 7 == 1;
        }
    }
    false
}

/// MPEG-4 Part 2: the first VOP (`00 00 01 B6`) is an I-VOP (`vop_coding_type` 0).
fn mpeg4_key(buf: &[u8]) -> bool {
    for at in start_codes(buf) {
        if buf[at] == 0xB6 {
            return buf.get(at + 1).is_some_and(|b| b >> 6 == 0);
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------
// Byte helpers

fn chunk_head<R: Read + Seek>(inner: &mut R, pos: u64) -> Result<Option<([u8; 4], u64)>> {
    inner.seek(SeekFrom::Start(pos))?;
    let mut h = [0u8; 8];
    if read_some(inner, &mut h)? < 8 {
        return Ok(None);
    }
    Ok(Some((
        [h[0], h[1], h[2], h[3]],
        u64::from(u32::from_le_bytes([h[4], h[5], h[6], h[7]])),
    )))
}

fn stream_number(id: &[u8; 4]) -> Option<u32> {
    let a = id[0].checked_sub(b'0')?;
    let b = id[1].checked_sub(b'0')?;
    (a < 10 && b < 10).then_some(u32::from(a) * 10 + u32::from(b))
}

fn le_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn le_u64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        data.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn read_some(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut at = 0;
    while at < buf.len() {
        match r.read(&mut buf[at..]) {
            Ok(0) => break,
            Ok(n) => at += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(at)
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<()> {
    if read_some(r, buf)? == buf.len() {
        Ok(())
    } else {
        Err(Error::corrupt("AVI header ends mid-field"))
    }
}

/// Read up to `size` bytes, growing as bytes arrive (a lying size costs only what exists).
/// A short body is not an error here: header chunks may be cut by truncation.
fn read_body(r: &mut impl Read, size: u64) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    r.by_ref().take(size).read_to_end(&mut body)?;
    Ok(body)
}
