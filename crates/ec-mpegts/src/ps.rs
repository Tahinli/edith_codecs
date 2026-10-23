//! MPEG-1 system streams, MPEG-2 program streams (`.mpg`, `.mpeg`, `.vob`, `.m2p`)
//! and raw MPEG-1/2 video elementary streams (`.m2v`, `.mpv`).
//!
//! # Joins (byte-concatenated programs)
//!
//! A pack clock (SCR) that steps back by more than 0.5 s, or forward by more than 1 s,
//! between two consecutive packs starts a new *epoch* (33-bit wraps are removed first).
//! The SCR only places an epoch on its own clock; at open, each epoch after the first is
//! shifted so that its first picture is presented where the previous epoch's last
//! picture ends (latest presentation time plus one frame), measured on the video track.
//! Audio takes the same shift, so its offset to the video stays the muxer's. The
//! timeline is therefore continuous: the duration is the sum of the segments and a seek
//! past a join is as exact as one before it. Without a video picture on either side of a
//! join the SCR-continuous shift is kept. A restart of the PTS without one of the SCR
//! is not detected.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek};

use ec_core::registry::{Demuxer, SeekMode, StreamInfo};
use ec_core::{Error, Packet, PacketFlags, Result, Rounding, TimeBase, Timestamp};

use crate::codec::{StreamKind, annexb_to_length_prefixed};
use crate::demux::{Stats, TrackInfo, VideoFraming, parse_pes, read_ts, unwrap33};
use crate::psi::{EsEntry, descriptors};
use crate::track::{PesIn, Queued, TICKS, Track};
use crate::window::Window;

const MAX_TRACKS: usize = 128;
const PROBE_BYTES: u64 = 32 << 20;
/// Files up to this size are walked completely at open to find SCR discontinuities.
const FULL_WALK: u64 = 96 << 20;
/// Samples taken to look for a backwards SCR in a larger file.
const SAMPLES: u64 = 256;
const BISECT_STOP: u64 = 64 << 10;
/// A pack clock stepping back by more than this (0.5 s) starts a new timeline epoch.
const BACK: i64 = TICKS / 2;
/// A pack clock stepping forward by more than this (1 s) between two consecutive packs
/// also starts a new epoch (a join of two programs that began at different clocks).
const FWD: i64 = TICKS;
/// How far before a join the end of the old segment is measured.
const JOIN_WINDOW: u64 = 4 << 20;
/// How far after a join the start of the new segment is measured.
const JOIN_HEAD: u64 = 1 << 20;
/// Bytes of a raw elementary stream handed to the picture cutter at a time.
const ES_CHUNK: usize = 64 << 10;
/// Pictures indexed in a raw elementary stream.
const MAX_PICS: usize = 4 << 20;
/// Largest element: a 16-bit PES length plus its 6-byte header.
const MAX_EL: usize = 6 + 65535;

/// What the file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsKind {
    /// MPEG-1 system stream (`0x2` marker in the pack header).
    Mpeg1System,
    /// MPEG-2 program stream (`0x44` marker), VOB included.
    Mpeg2Program,
    /// A bare MPEG-1/2 video elementary stream; timestamps are made from the frame rate.
    VideoElementary,
}

// ---------------------------------------------------------------------------
// Pack and element parsing
// ---------------------------------------------------------------------------

/// A pack header at `b[0..]`: `(length, scr in 90 kHz ticks, mpeg2)`.
fn parse_pack(b: &[u8]) -> Option<(usize, u64, bool)> {
    if b.get(..4)? != [0, 0, 1, 0xBA] {
        return None;
    }
    let h = b.get(4..)?;
    if h.first()? >> 6 == 1 {
        let h = h.get(..10)?;
        if h[0] & 0xC4 != 0x44 || h[2] & 4 == 0 || h[4] & 4 == 0 || h[5] & 1 == 0 {
            return None;
        }
        let scr = (u64::from((h[0] >> 3) & 7) << 30)
            | (u64::from(h[0] & 3) << 28)
            | (u64::from(h[1]) << 20)
            | (u64::from(h[2] >> 3) << 15)
            | (u64::from(h[2] & 3) << 13)
            | (u64::from(h[3]) << 5)
            | u64::from(h[4] >> 3);
        let stuff = usize::from(*b.get(13)? & 7);
        Some((14 + stuff, scr, true))
    } else if h.first()? >> 4 == 2 {
        let scr = read_ts(h)?;
        Some((12, scr, false))
    } else {
        None
    }
}

/// True when `b` starts with a system-level start code: pack, system header, PES or end code.
fn system_code(b: &[u8]) -> bool {
    b.len() >= 4 && b[..3] == [0, 0, 1] && b[3] >= 0xB9
}

#[derive(Debug, Clone, Copy)]
enum El {
    /// End of file.
    Eof,
    /// Not a valid element: resynchronise.
    Bad,
    Pack {
        len: usize,
        scr: u64,
    },
    /// Anything with a 16-bit length (system header, PSM, PES, padding), or the end code.
    Pes {
        id: u8,
        len: usize,
        /// The file ends inside this element.
        cut: bool,
    },
}

/// Finds the first confirmed pack header in `buf` (a valid pack followed by a system
/// start code, and the elements after it as far as `buf` shows them).
fn find_pack(buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 4 <= buf.len() {
        if buf[i + 2] > 1 {
            i += 3;
            continue;
        }
        if buf[i..i + 4] == [0, 0, 1, 0xBA]
            && let Some((len, _, _)) = parse_pack(&buf[i..])
        {
            let rest = buf.get(i + len..).unwrap_or(&[]);
            if rest.len() < 4 || system_code(rest) {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// True when `head` (the first few KiB of a file) starts an MPEG-1 system or MPEG-2
/// program stream (a pack header, after at most a few bytes of padding).
pub fn is_mpegps(head: &[u8]) -> bool {
    find_pack(head, 0).is_some_and(|at| at < 4096)
}

/// True when `head` starts a raw MPEG-1/2 video elementary stream (a sequence header,
/// after at most a few zero bytes).
pub fn is_mpeg_video_es(head: &[u8]) -> bool {
    es_start(head).is_some()
}

fn es_start(head: &[u8]) -> Option<usize> {
    let z = head.iter().take(16).take_while(|&&b| b == 0).count();
    let at = z.saturating_sub(2);
    let h = head.get(at..at + 11)?;
    let ok = h[..4] == [0, 0, 1, 0xB3]
        && (u16::from(h[4]) << 4 | u16::from(h[5] >> 4)) != 0
        && (u16::from(h[5] & 15) << 8 | u16::from(h[6])) != 0
        && h[10] & 0x20 != 0;
    ok.then_some(at)
}

// ---------------------------------------------------------------------------
// Clock and epochs
// ---------------------------------------------------------------------------

/// A PSM entry: stream type and its descriptors (tag, body).
type PsmEntry = (u8, Vec<(u8, Vec<u8>)>);

/// The pack clock unwrapped into a continuous `u` (33-bit wraps removed) and the shift
/// that keeps the presented timeline monotonic over SCR resets: `T = u + shift`.
#[derive(Debug, Clone, Copy)]
struct Clock {
    last_raw: Option<u64>,
    cur_u: i64,
    shift: i64,
}

impl Clock {
    /// Feeds a pack's SCR. `true` when it stepped back far enough to start a new epoch.
    fn feed(&mut self, raw: u64) -> bool {
        const M: i64 = 1 << 33;
        let Some(last) = self.last_raw.replace(raw) else {
            self.cur_u = unwrap33(raw, Some(self.cur_u));
            return false;
        };
        let mut d = (raw as i64 - last as i64).rem_euclid(M);
        if d >= M / 2 {
            d -= M;
        }
        if !(-BACK..=FWD).contains(&d) {
            let last_t = self.cur_u + self.shift;
            self.cur_u = raw as i64;
            self.shift = last_t - self.cur_u;
            true
        } else {
            self.cur_u += d;
            false
        }
    }
}

/// A stretch of the file with one SCR timeline.
#[derive(Debug, Clone, Copy)]
struct Epoch {
    off: u64,
    shift: i64,
    /// `u` of the first pack.
    base: i64,
}

// ---------------------------------------------------------------------------
// Raw video elementary streams: the picture index
// ---------------------------------------------------------------------------

struct Pic {
    off: u64,
    key: bool,
    pts: i64,
    dts: i64,
}

/// Every picture of a raw stream with timestamps made from the frame rate: pictures are
/// grouped (a GOP or sequence header, or an anchor whose temporal reference does not
/// advance, starts a group), the display index is the group's frame count so far plus
/// the temporal reference, `pts` is that index and `dts` the decode index less the
/// reordering depth, both in frame durations.
struct EsIndex {
    pics: Vec<Pic>,
    start: i64,
    end: i64,
}

// ---------------------------------------------------------------------------
// The demuxer
// ---------------------------------------------------------------------------

/// MPEG-1 system / MPEG-2 program stream demuxer (and raw MPEG video elementary
/// streams).
pub struct PsDemuxer<R> {
    win: Window<R>,
    file_len: u64,
    kind: PsKind,
    first_off: u64,
    pos: u64,
    psm: HashMap<u8, PsmEntry>,
    tracks: Vec<Track>,
    key_map: HashMap<u16, usize>,
    streams: Vec<StreamInfo>,
    queue: VecDeque<Queued>,
    eof: bool,
    stats: Stats,
    framing: VideoFraming,
    last_off: u64,
    duration: Option<i64>,
    frozen: bool,
    clock: Clock,
    epochs: Vec<Epoch>,
    first_scr: Option<i64>,
    es: Option<EsIndex>,
    scratch: Vec<u8>,
    /// A damaged element was skipped since the last PES; flags the next one.
    bad_next: bool,
    /// After a seek: drop this many pictures of the reference video track that start
    /// in the PES at the landing offset ahead of the keyframe.
    skip: Option<(usize, u64, u32)>,
    /// After a seek that started reading earlier (see [`PsDemuxer::land`]): drop every
    /// packet that starts before this offset.
    pre_drop: Option<u64>,
    walk_limit: u64,
}

impl<R: Read + Seek> PsDemuxer<R> {
    /// Opens `reader` as a program stream, a system stream or a raw video stream.
    pub fn new(reader: R) -> Result<PsDemuxer<R>> {
        Self::open(reader, FULL_WALK)
    }

    /// As [`PsDemuxer::new`], with files larger than `walk_limit` bytes checked for SCR
    /// resets by sampling instead of a full walk (tests exercise that path on small files).
    #[doc(hidden)]
    pub fn with_walk_limit(reader: R, walk_limit: u64) -> Result<PsDemuxer<R>> {
        Self::open(reader, walk_limit)
    }

    fn open(reader: R, walk_limit: u64) -> Result<PsDemuxer<R>> {
        let mut win = Window::new(reader);
        let file_len = win.len()?;
        let head = win.get(0, 64 * 1024)?;
        let (kind, first_off) = if let Some(at) = find_pack(head, 0) {
            let mpeg2 = parse_pack(&head[at..]).is_some_and(|p| p.2);
            (
                if mpeg2 {
                    PsKind::Mpeg2Program
                } else {
                    PsKind::Mpeg1System
                },
                at as u64,
            )
        } else if let Some(at) = es_start(head) {
            (PsKind::VideoElementary, at as u64)
        } else {
            return Err(Error::corrupt("mpegps: no pack header or sequence header"));
        };
        let mut d = PsDemuxer {
            win,
            file_len,
            kind,
            first_off,
            pos: first_off,
            psm: HashMap::new(),
            tracks: Vec::new(),
            key_map: HashMap::new(),
            streams: Vec::new(),
            queue: VecDeque::new(),
            eof: false,
            stats: Stats::default(),
            framing: VideoFraming::AnnexB,
            last_off: first_off,
            duration: None,
            frozen: false,
            clock: Clock {
                last_raw: None,
                cur_u: 0,
                shift: 0,
            },
            epochs: Vec::new(),
            first_scr: None,
            es: None,
            scratch: Vec::new(),
            bad_next: false,
            skip: None,
            pre_drop: None,
            walk_limit,
        };
        if kind == PsKind::VideoElementary {
            d.open_es()?;
        } else {
            d.open_ps()?;
        }
        Ok(d)
    }

    // ----- public queries -----------------------------------------------------

    /// What the file is: system stream, program stream or raw video.
    pub fn kind(&self) -> PsKind {
        self.kind
    }

    /// Every elementary stream seen (including refused ones), in order of appearance.
    /// `pid` is the `stream_id` in the high byte and, for private_stream_1, the
    /// substream id in the low byte; `stream_type` is the program stream map's value
    /// when there is one, else the MPEG-TS type of what the stream is.
    pub fn tracks(&self) -> Vec<TrackInfo> {
        self.tracks
            .iter()
            .map(|t| TrackInfo {
                pid: t.es.pid,
                stream_type: t.es.stream_type,
                kind: t.kind.clone(),
                stream: t.stream,
                refusal: t.refusal.clone(),
                language: None,
            })
            .collect()
    }

    /// `stream_id` of stream `stream`.
    pub fn stream_id(&self, stream: u32) -> Option<u8> {
        self.stream_key(stream).map(|k| (k >> 8) as u8)
    }

    /// `(stream_id << 8) | substream_id` of stream `stream` (substream 0 outside private_stream_1).
    pub fn stream_key(&self, stream: u32) -> Option<u16> {
        self.tracks
            .iter()
            .find(|t| t.stream == Some(stream))
            .map(|t| t.es.pid)
    }

    /// The 90 kHz time base of every packet.
    pub fn time_base(&self) -> TimeBase {
        TimeBase::new(1, TICKS)
    }

    /// Length in seconds, from the first and last timestamps.
    pub fn duration_secs(&self) -> Option<f64> {
        self.duration.map(|d| d as f64 / TICKS as f64)
    }

    /// Counters of what was repaired or skipped (`resyncs`, `discontinuities` = SCR
    /// resets, `bad_pes`); the transport-only counters stay 0.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// SCR resets found in the file (concatenated VOBs, cell boundaries): timestamps
    /// after each are shifted to continue the timeline.
    pub fn epoch_count(&self) -> usize {
        self.epochs.len().max(1)
    }

    /// File offset of the PES (or raw picture) that began the last packet returned;
    /// after [`Demuxer::seek`] it is where the read resumes.
    pub fn last_packet_offset(&self) -> u64 {
        self.last_off
    }

    /// Sets how H.264 and HEVC packets are framed (default Annex B).
    pub fn set_video_framing(&mut self, framing: VideoFraming) {
        self.framing = framing;
    }

    // ----- elements ---------------------------------------------------------------

    fn el_at(&mut self, pos: u64) -> Result<El> {
        let avail = self.file_len.saturating_sub(pos);
        if avail == 0 {
            return Ok(El::Eof);
        }
        let b = self.win.get(pos, 32)?;
        if b.len() < 4 {
            return Ok(El::Bad);
        }
        if !system_code(b) {
            return Ok(El::Bad);
        }
        let code = b[3];
        let (total, pack) = if code == 0xBA {
            match parse_pack(b) {
                Some((len, scr, _)) => (len, Some(scr)),
                None => return Ok(El::Bad),
            }
        } else if code == 0xB9 {
            (4, None)
        } else {
            if b.len() < 6 {
                return Ok(El::Bad);
            }
            (6 + ((usize::from(b[4]) << 8) | usize::from(b[5])), None)
        };
        let cut = (total as u64) > avail;
        if !cut {
            // What follows must be another element (or the end of the file).
            let after = self.win.get(pos + total as u64, 4)?;
            if !after.is_empty() && !system_code(after) && after.len() >= 4 {
                return Ok(El::Bad);
            }
        }
        Ok(match pack {
            Some(scr) if !cut => El::Pack { len: total, scr },
            Some(_) => El::Bad,
            None => El::Pes {
                id: code,
                len: total,
                cut,
            },
        })
    }

    /// The next confirmed pack header at or after `from`.
    fn next_pack(&mut self, from: u64) -> Result<Option<u64>> {
        let mut at = from;
        loop {
            let buf = self.win.get(at, 64 * 1024)?;
            if buf.len() < 4 {
                return Ok(None);
            }
            let full = buf.len() == 64 * 1024;
            let limit = if full { buf.len() - 32 } else { buf.len() };
            if let Some(i) = find_pack(buf, 0).filter(|&i| i < limit) {
                return Ok(Some(at + i as u64));
            }
            if !full {
                return Ok(None);
            }
            at += limit as u64;
        }
    }

    // ----- engine -------------------------------------------------------------------

    fn mark_bad(&mut self) {
        self.bad_next = true;
    }

    /// Processes one element. `Ok(false)` once the end was reached and everything
    /// buffered was flushed.
    fn step(&mut self) -> Result<bool> {
        if self.eof {
            return Ok(false);
        }
        if self.kind == PsKind::VideoElementary {
            return self.step_es();
        }
        let pos = self.pos;
        match self.el_at(pos)? {
            El::Eof => {
                self.finish();
                Ok(false)
            }
            El::Bad => {
                self.stats.resyncs += 1;
                self.mark_bad();
                match self.next_pack(pos + 1)? {
                    Some(p) => {
                        self.pos = p;
                        Ok(true)
                    }
                    None => {
                        self.finish();
                        Ok(false)
                    }
                }
            }
            El::Pack { len, scr } => {
                self.pos += len as u64;
                self.on_pack(scr, pos);
                Ok(true)
            }
            El::Pes { id, len, cut } => {
                let avail = (self.file_len - pos) as usize;
                let take = len.min(avail).min(MAX_EL);
                let mut buf = std::mem::take(&mut self.scratch);
                buf.clear();
                buf.extend_from_slice(self.win.get(pos, take)?);
                self.pos += take as u64;
                if cut {
                    self.mark_bad();
                }
                match id {
                    0xBC => self.on_psm(&buf),
                    0xBD | 0xC0..=0xEF | 0xFD..=0xFE | 0xFA | 0xFB | 0xFC => {
                        self.on_pes(pos, &buf);
                    }
                    _ => {}
                }
                self.scratch = buf;
                if cut {
                    self.finish();
                    return Ok(false);
                }
                Ok(true)
            }
        }
    }

    fn on_pack(&mut self, scr: u64, at: u64) {
        if self.first_scr.is_none() {
            self.first_scr = Some(scr as i64);
        }
        if self.clock.feed(scr) {
            self.stats.discontinuities += 1;
            // The table made at open knows this join with its aligned shift.
            if let Some(e) = self.epochs.iter().find(|e| e.off == at) {
                self.clock.shift = e.shift;
            }
            // Whatever is half-read belongs to the old timeline.
            for ti in 0..self.tracks.len() {
                self.tracks[ti].flush(ti, &mut self.queue);
                self.tracks[ti].reset(None);
            }
        }
    }

    fn finish(&mut self) {
        for ti in 0..self.tracks.len() {
            self.tracks[ti].flush(ti, &mut self.queue);
        }
        self.eof = true;
    }

    fn on_psm(&mut self, b: &[u8]) {
        if self.frozen || b.len() < 16 || b[6] & 0x80 == 0 {
            return;
        }
        let info = (usize::from(b[8]) << 8) | usize::from(b[9]);
        let at = 10 + info;
        let Some(map_len) = b
            .get(at..at + 2)
            .map(|m| (usize::from(m[0]) << 8) | usize::from(m[1]))
        else {
            return;
        };
        let end = (at + 2 + map_len).min(b.len().saturating_sub(4));
        let mut i = at + 2;
        while i + 4 <= end && self.psm.len() < 256 {
            let (ty, id) = (b[i], b[i + 1]);
            let dl = (usize::from(b[i + 2]) << 8) | usize::from(b[i + 3]);
            let d = b.get(i + 4..(i + 4 + dl).min(end)).unwrap_or(&[]);
            self.psm.insert(id, (ty, descriptors(d)));
            i += 4 + dl;
        }
    }

    /// Timestamps of a PES header on the presented timeline.
    fn stamp(&self, pts: Option<u64>, dts: Option<u64>) -> (Option<i64>, Option<i64>) {
        const M: i64 = 1 << 33;
        let t = |raw: u64| unwrap33(raw, Some(self.clock.cur_u)) + self.clock.shift;
        let p = pts.map(t);
        let d = match (dts, pts, p) {
            (Some(d), Some(rp), Some(p)) => {
                let mut delta = (d as i64 - rp as i64).rem_euclid(M);
                if delta >= M / 2 {
                    delta -= M;
                }
                Some(p + delta)
            }
            _ => None,
        };
        (p, d)
    }

    fn on_pes(&mut self, off: u64, b: &[u8]) {
        let id = b[3];
        let (pts, dts, mut payload) = if b.get(6).is_some_and(|&x| x & 0xC0 == 0x80) {
            match parse_pes(b) {
                Some(h) => (h.pts, h.dts, h.payload),
                None => {
                    self.stats.bad_pes += 1;
                    return;
                }
            }
        } else {
            match parse_pes1(b) {
                Some(h) => h,
                None => {
                    self.stats.bad_pes += 1;
                    return;
                }
            }
        };
        let mut sub = 0u8;
        if id == 0xBD {
            let Some(&s) = b.get(payload) else {
                return;
            };
            sub = s;
            payload += match s {
                0x20..=0x3F => 1,
                0x80..=0xAF => 4,
                _ => 0,
            };
            if payload > b.len() {
                return;
            }
        }
        let key = (u16::from(id) << 8) | u16::from(sub);
        let ti = match self.key_map.get(&key) {
            Some(&ti) => ti,
            None => {
                if self.frozen || self.tracks.len() >= MAX_TRACKS {
                    return;
                }
                let Some(t) = self.make_track(id, sub, &b[payload..]) else {
                    return;
                };
                self.tracks.push(t);
                self.key_map.insert(key, self.tracks.len() - 1);
                self.tracks.len() - 1
            }
        };
        if !self.tracks[ti].wanted() {
            return;
        }
        let (pts, dts) = self.stamp(pts, dts);
        let input = PesIn {
            payload: &b[payload..],
            pts,
            dts,
            off,
            rai: false,
            bad: std::mem::take(&mut self.bad_next),
        };
        let t = &mut self.tracks[ti];
        if t.psv.on {
            t.ps_video_push(ti, &input, &mut self.queue);
        } else {
            t.push_pes(ti, &input, &mut self.queue);
        }
    }

    fn make_track(&self, id: u8, sub: u8, payload: &[u8]) -> Option<Track> {
        let key = (u16::from(id) << 8) | u16::from(sub);
        let entry = |stream_type: u8, descriptors: Vec<(u8, Vec<u8>)>| EsEntry {
            stream_type,
            pid: key,
            descriptors,
        };
        let mapped = self.psm.get(&id).cloned();
        let mut t = match id {
            0xBD => match sub {
                0x80..=0x87 => Track::new(entry(0x81, Vec::new())),
                0xA0..=0xA7 => {
                    let mut t = Track::new(entry(0x80, Vec::new()));
                    t.kind = StreamKind::DvdLpcm;
                    t.refusal = None;
                    t
                }
                0x20..=0x3F => {
                    let mut t = Track::new(entry(0x06, Vec::new()));
                    t.kind = StreamKind::DvdSub;
                    t.refusal = None;
                    t
                }
                0x88..=0x8F | 0x98..=0x9F => {
                    let mut t = Track::new(entry(0x82, Vec::new()));
                    t.refusal = Some(format!(
                        "dts (private_stream_1 substream 0x{sub:02x}): DTS has no decoder in the family"
                    ));
                    t
                }
                s => {
                    let mut t = Track::new(entry(0x06, Vec::new()));
                    t.kind = StreamKind::Other(format!("private_stream_1_0x{s:02x}"));
                    t.refusal = Some(format!(
                        "private_stream_1 substream 0x{s:02x}: no demux path for this substream"
                    ));
                    t
                }
            },
            0xE0..=0xEF => {
                let (ty, d) = match mapped {
                    Some(m) => m,
                    None => (sniff_video(payload)?, Vec::new()),
                };
                Track::new(entry(ty, d))
            }
            0xC0..=0xDF => {
                let (ty, d) = match mapped {
                    Some(m) => m,
                    None => (sniff_audio(payload)?, Vec::new()),
                };
                Track::new(entry(ty, d))
            }
            _ => {
                let (ty, d) = mapped?;
                Track::new(entry(ty, d))
            }
        };
        t.psv.ps = true;
        t.psv.on = t.kind.is_video();
        Some(t)
    }

    // ----- open (program streams) -----------------------------------------------------

    fn open_ps(&mut self) -> Result<()> {
        let first = match self.el_at(self.first_off)? {
            El::Pack { scr, .. } => scr as i64,
            _ => return Err(Error::corrupt("mpegps: no pack header")),
        };
        self.epochs = vec![Epoch {
            off: self.first_off,
            shift: 0,
            base: first,
        }];
        self.reset_at(self.first_off);
        self.probe()?;
        if self.tracks.is_empty() {
            return Err(Error::corrupt("mpegps: no elementary streams found"));
        }
        self.build_epochs()?;
        self.finish_open()
    }

    fn probe_complete(&self, read: u64) -> bool {
        let wanted: Vec<&Track> = self.tracks.iter().filter(|t| t.wanted()).collect();
        if wanted.is_empty() {
            return false;
        }
        let ready = wanted.iter().all(|t| t.ready());
        let timed = wanted.iter().all(|t| {
            if t.kind.is_video() {
                t.dts_seen.len() >= 12
            } else {
                t.packets >= 3
            }
        });
        let span = self
            .first_scr
            .is_some_and(|f| self.clock.cur_u - f >= TICKS);
        ready && timed && (span || read >= 4 << 20)
    }

    fn probe(&mut self) -> Result<()> {
        let start = self.pos;
        loop {
            self.queue.clear();
            if self.eof {
                break;
            }
            let read = self.pos - start;
            if read >= PROBE_BYTES || self.probe_complete(read) {
                break;
            }
            self.step()?;
        }
        self.queue.clear();
        Ok(())
    }

    /// Finds SCR resets: the whole file when it is small, otherwise a sample of pack
    /// headers decides whether a full walk is needed.
    fn build_epochs(&mut self) -> Result<()> {
        let full = if self.file_len <= self.walk_limit {
            true
        } else {
            let mut last: Option<u64> = None;
            let mut back = false;
            for k in 0..SAMPLES {
                let off = self.first_off + (self.file_len - self.first_off) * k / SAMPLES;
                let Some(p) = self.next_pack(off)? else {
                    break;
                };
                if let El::Pack { scr, .. } = self.el_at(p)? {
                    if let Some(l) = last {
                        let d = (scr as i64 - l as i64).rem_euclid(1 << 33);
                        if d >= 1 << 32 {
                            back = true;
                            break;
                        }
                    }
                    last = Some(scr);
                }
            }
            back
        };
        if !full {
            return Ok(());
        }
        let first = self.epochs[0];
        let mut clock = Clock {
            last_raw: None,
            cur_u: first.base,
            shift: 0,
        };
        let mut epochs = vec![first];
        let mut pos = self.first_off;
        loop {
            match self.el_at(pos)? {
                El::Eof => break,
                El::Bad => match self.next_pack(pos + 1)? {
                    Some(p) => pos = p,
                    None => break,
                },
                El::Pack { len, scr } => {
                    if clock.feed(scr) && epochs.len() < 1 << 16 {
                        epochs.push(Epoch {
                            off: pos,
                            shift: clock.shift,
                            base: clock.cur_u,
                        });
                    }
                    pos += len as u64;
                }
                El::Pes { len, .. } => pos += len as u64,
            }
        }
        self.epochs = epochs;
        self.align_epochs()
    }

    /// Makes the timeline continuous over joins. The SCR says where a segment sits on its
    /// own clock only; the presentation times (what players and `duration` use) are
    /// aligned instead: segment `k` is shifted so that its first picture is shown where
    /// segment `k-1`'s last picture ends (latest presentation time plus one frame).
    /// Audio takes the segment's shift too, so its offset to the video is the muxer's.
    /// Without a video picture on either side the SCR-continuous shift is kept.
    fn align_epochs(&mut self) -> Result<()> {
        if self.epochs.len() < 2 {
            return Ok(());
        }
        let Some(rt) = self
            .tracks
            .iter()
            .position(|t| t.wanted() && t.kind.is_video())
        else {
            return Ok(());
        };
        let ft = self.tracks[rt].frame_ticks().unwrap_or(0);
        let snapshot: Vec<_> = self
            .tracks
            .iter()
            .map(|t| (t.min_pts, t.max_end, t.packets, t.dts_seen.clone()))
            .collect();
        let was_frozen = std::mem::replace(&mut self.frozen, true);
        let prov: Vec<i64> = self.epochs.iter().map(|e| e.shift).collect();
        let mut result = Ok(());
        for k in 1..self.epochs.len() {
            let (prev, cur) = (self.epochs[k - 1], self.epochs[k]);
            let next_off = self.epochs.get(k + 1).map_or(self.file_len, |e| e.off);
            // End of the segment before the join, on its final timeline.
            let from = prev.off.max(cur.off.saturating_sub(JOIN_WINDOW));
            let end = match self.scan_segment(from, cur.off, rt) {
                Ok((_, e)) => e.map(|e| e + ft),
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            // Start of the new segment on its own clock (shift 0).
            self.epochs[k].shift = 0;
            let start = match self.scan_segment(cur.off, next_off.min(cur.off + JOIN_HEAD), rt) {
                Ok((s, _)) => s,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            self.epochs[k].shift = match (end, start) {
                (Some(end), Some(start)) => end - start,
                _ => prov[k] - prov[k - 1] + prev.shift,
            };
        }
        self.frozen = was_frozen;
        for (t, (min, max, packets, seen)) in self.tracks.iter_mut().zip(snapshot) {
            t.min_pts = min;
            t.max_end = max;
            t.packets = packets;
            t.dts_seen = seen;
        }
        self.reset_at(self.first_off);
        result
    }

    /// Reads `[from, to)` and returns the video track `rt`'s earliest presentation time
    /// and latest end seen there.
    fn scan_segment(
        &mut self,
        from: u64,
        to: u64,
        rt: usize,
    ) -> Result<(Option<i64>, Option<i64>)> {
        self.reset_at(from);
        self.tracks[rt].min_pts = None;
        self.tracks[rt].max_end = None;
        while self.pos < to {
            let more = self.step()?;
            self.queue.clear();
            if !more {
                break;
            }
        }
        for ti in 0..self.tracks.len() {
            self.tracks[ti].flush(ti, &mut self.queue);
        }
        self.queue.clear();
        Ok((self.tracks[rt].min_pts, self.tracks[rt].max_end))
    }

    fn epoch_at(&self, off: u64) -> Epoch {
        let i = self.epochs.partition_point(|e| e.off <= off);
        self.epochs[i.saturating_sub(1)]
    }

    fn reset_at(&mut self, off: u64) {
        self.queue.clear();
        self.eof = false;
        self.pos = off;
        self.bad_next = false;
        self.skip = None;
        self.pre_drop = None;
        for t in &mut self.tracks {
            t.reset(None);
        }
        if self.kind != PsKind::VideoElementary {
            let e = self.epoch_at(off);
            self.clock = Clock {
                last_raw: None,
                cur_u: e.base,
                shift: e.shift,
            };
        }
    }

    fn finish_open(&mut self) -> Result<()> {
        for t in &mut self.tracks {
            t.start_time = t.min_pts;
        }
        self.assign_streams();
        self.frozen = true;
        let tail = self.scan_tail();
        self.frozen = false;
        tail?;
        self.reset_at(self.first_off);
        self.stats = Stats::default();
        Ok(())
    }

    fn assign_streams(&mut self) {
        for ti in 0..self.tracks.len() {
            self.make_stream(ti);
        }
    }

    fn make_stream(&mut self, ti: usize) -> Option<u32> {
        if let Some(s) = self.tracks[ti].stream {
            return Some(s);
        }
        let index = self.streams.len() as u32;
        let mut info = self.tracks[ti].stream_info(index)?;
        if !self
            .streams
            .iter()
            .any(|s| s.params.codec.media_type() == info.params.codec.media_type() && s.default)
        {
            info.default = true;
        }
        self.tracks[ti].stream = Some(index);
        self.streams.push(info);
        Some(index)
    }

    fn scan_tail(&mut self) -> Result<()> {
        let mut win: u64 = 4 << 20;
        loop {
            for t in &mut self.tracks {
                t.max_end = None;
            }
            let from = self.file_len.saturating_sub(win);
            let off = from.max(self.first_off);
            self.reset_at(off);
            // The timestamps are unwrapped against the epoch's base, so a clock that
            // wrapped 33 bits since the start of the file still lands near it.
            while self.step()? {
                self.queue.clear();
            }
            self.queue.clear();
            let timed = self
                .tracks
                .iter()
                .filter(|t| t.stream.is_some() && !t.kind.is_bitmap_sub())
                .all(|t| t.max_end.is_some());
            if timed || off == self.first_off {
                break;
            }
            win = win.saturating_mul(4);
        }
        let mut longest: Option<i64> = None;
        for t in &self.tracks {
            // Subtitles do not define the programme's length: its duration is the audio
            // and video's, whatever the subtitle cues span.
            if t.kind.is_bitmap_sub() {
                continue;
            }
            let (Some(si), Some(end), Some(start)) = (t.stream, t.max_end, t.start_time) else {
                continue;
            };
            let end = end
                + if t.kind.is_video() {
                    t.frame_ticks().unwrap_or(0)
                } else {
                    0
                };
            let d = end - start;
            if d > 0
                && let Some(s) = self.streams.get_mut(si as usize)
            {
                s.duration = Some(d);
                longest = Some(longest.map_or(d, |l| l.max(d)));
            }
        }
        self.duration = longest;
        Ok(())
    }

    // ----- reading -------------------------------------------------------------------------

    fn pump(&mut self) -> Result<Option<Queued>> {
        loop {
            if let Some(mut q) = self.queue.pop_front() {
                if self.pre_drop.is_some_and(|o| q.off < o) {
                    continue;
                }
                if let Some((ti, off, n)) = &mut self.skip
                    && q.track == *ti
                    && q.off == *off
                    && *n > 0
                {
                    *n -= 1;
                    continue;
                }
                if let Some(ix) = &self.es
                    && let Ok(i) = ix.pics.binary_search_by_key(&q.off, |p| p.off)
                {
                    q.pts = Some(ix.pics[i].pts);
                    q.dts = Some(ix.pics[i].dts);
                }
                return Ok(Some(q));
            }
            if !self.step()? {
                if self.queue.is_empty() {
                    return Ok(None);
                }
                continue;
            }
        }
    }

    fn make_packet(&mut self, q: Queued) -> Option<Packet> {
        let ti = q.track;
        let stream = match self.tracks[ti].stream {
            Some(s) => s,
            None => {
                if self.tracks[ti].start_time.is_none() {
                    self.tracks[ti].start_time = self.tracks[ti].min_pts;
                }
                self.make_stream(ti)?
            }
        };
        let kind = &self.tracks[ti].kind;
        let data = if self.framing == VideoFraming::LengthPrefixed && kind.is_annexb_video() {
            annexb_to_length_prefixed(&q.data, 4)
        } else {
            q.data
        };
        let mut p = Packet::new(stream, self.time_base(), data);
        p.pts = q.pts;
        p.dts = q.dts;
        p.duration = q.dur;
        p.flags = PacketFlags {
            keyframe: q.key,
            corrupt: q.corrupt,
            ..PacketFlags::default()
        };
        self.last_off = q.off;
        Some(p)
    }

    // ----- seeking ------------------------------------------------------------------------------

    fn time_at(&mut self, off: u64, ti: usize) -> Result<Option<i64>> {
        self.reset_at(off);
        while let Some(q) = self.pump()? {
            if q.track == ti
                && let Some(t) = q.dts.or(q.pts)
            {
                return Ok(Some(t));
            }
        }
        Ok(None)
    }

    fn scan_for(
        &mut self,
        from: u64,
        ti: usize,
        target: i64,
        mode: SeekMode,
        all_sync: bool,
    ) -> Result<Option<(u64, u32, u64)>> {
        self.reset_at(from);
        let mut best = None;
        // Offsets of the two latest keyframes of the track, for [`PsDemuxer::land`].
        let (mut key_cur, mut key_prev): (Option<u64>, Option<u64>) = (None, None);
        // Pictures of the track seen so far that start in the same packet.
        let (mut run_off, mut run_n) = (u64::MAX, 0u32);
        while let Some(q) = self.pump()? {
            if q.track != ti {
                continue;
            }
            if q.off != run_off {
                (run_off, run_n) = (q.off, 0);
            }
            let idx = run_n;
            run_n += 1;
            if q.key && key_cur != Some(q.off) {
                (key_prev, key_cur) = (key_cur, Some(q.off));
            }
            let pre = key_prev.or(key_cur).unwrap_or(q.off).min(q.off);
            // A picture whose timestamps were derived from its neighbours cannot start a read.
            // (A keyframe whose time follows from the display order is trusted once the
            // scan has seen the GOP before it.)
            let trusted = !q.derived || (q.key && q.pts.is_some() && key_prev.is_some());
            let Some(pts) = q.pts.or(q.dts).filter(|_| trusted) else {
                continue;
            };
            let dts = q.dts.unwrap_or(pts);
            let key = all_sync || q.key;
            match mode {
                SeekMode::SyncAfter => {
                    if key && pts >= target {
                        return Ok(Some((q.off, idx, pre)));
                    }
                }
                SeekMode::Exact => {
                    if dts > target {
                        break;
                    }
                    best = Some((q.off, idx, pre));
                }
                SeekMode::SyncBefore => {
                    if dts > target {
                        break;
                    }
                    if key && pts <= target {
                        best = Some((q.off, idx, pre));
                    }
                }
            }
        }
        Ok(best)
    }

    fn seek_scan(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<()> {
        let Some(ti) = self.tracks.iter().position(|t| t.stream == Some(stream)) else {
            return Err(Error::corrupt(format!(
                "mpegps: no stream {stream} to seek"
            )));
        };
        let target = to.rescale(self.time_base(), Rounding::Nearest).ticks;
        if self.es.is_some() {
            return self.seek_es(target, mode);
        }
        let ref_ti = self
            .tracks
            .iter()
            .position(|t| t.stream.is_some() && t.kind.is_video())
            .unwrap_or(ti);
        let all_sync = !self.tracks[ref_ti].kind.is_video();
        let start = self.tracks[ref_ti].start_time.unwrap_or(0);
        let landing = if target <= start && mode != SeekMode::Exact {
            None
        } else {
            let mut lo = self.first_off;
            let mut hi = self.file_len;
            while hi - lo > BISECT_STOP {
                let mid = lo + (hi - lo) / 2;
                match self.time_at(mid, ref_ti)? {
                    Some(t) if t <= target => lo = mid,
                    _ => hi = mid,
                }
            }
            let mut back: u64 = 256 << 10;
            loop {
                let from = lo.saturating_sub(back).max(self.first_off);
                if let Some(found) = self.scan_for(from, ref_ti, target, mode, all_sync)? {
                    break Some(found);
                }
                if from <= self.first_off {
                    break None;
                }
                back = back.saturating_mul(4);
            }
        };
        self.land(landing, mode, Some(ref_ti))
    }

    fn land(
        &mut self,
        landing: Option<(u64, u32, u64)>,
        mode: SeekMode,
        ref_ti: Option<usize>,
    ) -> Result<()> {
        match landing {
            Some((off, n, pre)) => {
                // MPEG-1/2 pictures without a stamp take their time from the display order
                // so far, which needs the whole GOP before the landing one: reading starts
                // at the keyframe before the landing keyframe (`pre`) and drops what starts
                // ahead of the landing offset, so the packets equal the sequential read's.
                let pre = if ref_ti.is_some_and(|ti| self.tracks[ti].kind.is_mpeg_video()) {
                    pre.min(off)
                } else {
                    off
                };
                self.reset_at(pre);
                self.last_off = off;
                if pre < off {
                    self.pre_drop = Some(off);
                }
                if n > 0 {
                    self.skip = ref_ti.map(|ti| (ti, off, n));
                }
            }
            None if mode == SeekMode::SyncAfter => {
                self.reset_at(self.file_len);
                self.last_off = self.file_len;
                return Err(Error::Eof);
            }
            None => {
                self.reset_at(self.first_off);
                self.last_off = self.first_off;
            }
        }
        Ok(())
    }

    // ----- raw video elementary streams ------------------------------------------------------------

    fn open_es(&mut self) -> Result<()> {
        let mut t = Track::new(EsEntry {
            stream_type: 0x02,
            pid: 0xE000,
            descriptors: Vec::new(),
        });
        t.psv.on = true;
        t.psv.buf.exact = true;
        self.tracks.push(t);
        self.key_map.insert(0xE000, 0);
        self.reset_at(self.first_off);
        while !self.eof && !self.tracks[0].ready() && self.pos - self.first_off < PROBE_BYTES {
            self.step()?;
            self.queue.clear();
        }
        self.queue.clear();
        let Some(f) = self.tracks[0].cfg.mpeg2.clone() else {
            return Err(Error::corrupt("mpegps: no decodable sequence header"));
        };
        let ix = self.index_es(f.rate)?;
        self.tracks[0].start_time = Some(ix.start);
        self.duration = Some(ix.end - ix.start);
        self.es = Some(ix);
        self.assign_streams();
        if let Some(s) = self.streams.first_mut() {
            s.duration = self.duration;
        }
        self.reset_at(self.first_off);
        Ok(())
    }

    fn step_es(&mut self) -> Result<bool> {
        let pos = self.pos;
        let b = self.win.get(pos, ES_CHUNK)?;
        if b.is_empty() {
            self.finish();
            return Ok(false);
        }
        let mut chunk = std::mem::take(&mut self.scratch);
        chunk.clear();
        chunk.extend_from_slice(b);
        self.pos += chunk.len() as u64;
        let input = PesIn {
            payload: &chunk,
            pts: None,
            dts: None,
            off: pos,
            rai: false,
            bad: false,
        };
        self.tracks[0].ps_video_push(0, &input, &mut self.queue);
        self.scratch = chunk;
        Ok(true)
    }

    /// Reads the whole stream once for its pictures.
    fn index_es(&mut self, rate: (u32, u32)) -> Result<EsIndex> {
        const CH: usize = 1 << 20;
        struct Raw {
            off: u64,
            tr: i32,
            ty: u8,
            key: bool,
            group: bool,
        }
        let mut raw: Vec<Raw> = Vec::new();
        let mut off = self.first_off;
        // The chain of header codes in front of the next picture header.
        let mut chain: Option<(u64, bool, bool)> = None; // (start, has B3, has B3 or B8)
        loop {
            let buf = self.win.get(off, CH + 8)?;
            let at_end = buf.len() < CH + 8;
            let limit = if at_end { buf.len() } else { CH };
            let mut i = 0usize;
            while i < limit && i + 4 <= buf.len() {
                if buf[i + 2] > 1 {
                    i += 3;
                    continue;
                }
                if !(buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1) {
                    i += 1;
                    continue;
                }
                let code = buf[i + 3];
                let here = off + i as u64;
                match code {
                    0xB2 | 0xB5 => {
                        chain.get_or_insert((here, false, false));
                    }
                    0xB3 | 0xB8 => {
                        let c = chain.get_or_insert((here, false, false));
                        c.2 = true;
                        if code == 0xB3 {
                            c.1 = true;
                        }
                    }
                    0x00 => {
                        let (start, seq, grp) = chain.take().unwrap_or((here, false, false));
                        if raw.len() >= MAX_PICS {
                            return Err(Error::corrupt("mpegps: too many pictures to index"));
                        }
                        let (b4, b5) = (buf.get(i + 4).copied(), buf.get(i + 5).copied());
                        if let (Some(b4), Some(b5)) = (b4, b5) {
                            let ty = (b5 >> 3) & 7;
                            raw.push(Raw {
                                off: start,
                                tr: (i32::from(b4) << 2) | i32::from(b5 >> 6),
                                ty,
                                key: ty == 1 || seq,
                                group: grp,
                            });
                        }
                    }
                    _ => chain = None,
                }
                i += 4;
            }
            if at_end {
                break;
            }
            off += i as u64;
        }
        if raw.is_empty() {
            return Err(Error::corrupt("mpegps: no pictures"));
        }
        let (num, den) = (i128::from(rate.0.max(1)), i128::from(rate.1.max(1)));
        let ticks =
            |n: i64| -> i64 { (i128::from(n) * i128::from(TICKS) * den).div_euclid(num) as i64 };
        // Groups and display indices.
        let mut disp: Vec<i64> = Vec::with_capacity(raw.len());
        let (mut base, mut gmax, mut anchor) = (0i64, -1i64, None::<i32>);
        for (i, r) in raw.iter().enumerate() {
            let new_group = r.group || (r.ty != 3 && anchor.is_some_and(|a| r.tr < a));
            if new_group && i > 0 {
                base += gmax + 1;
                gmax = -1;
                anchor = None;
            }
            disp.push(base + i64::from(r.tr));
            gmax = gmax.max(i64::from(r.tr));
            if r.ty != 3 {
                anchor = Some(r.tr);
            }
        }
        let min_disp = disp.iter().copied().min().unwrap_or(0);
        // How far a picture is displayed behind its decode position, over the first
        // dozens of pictures.
        let delay = disp
            .iter()
            .take(96)
            .enumerate()
            .map(|(i, d)| i as i64 - (d - min_disp))
            .max()
            .unwrap_or(0)
            .max(0);
        let mut pics = Vec::with_capacity(raw.len());
        let mut start = i64::MAX;
        let mut end = i64::MIN;
        for (i, r) in raw.iter().enumerate() {
            let pts = ticks(disp[i] - min_disp);
            let dts = ticks(i as i64 - delay).min(pts);
            start = start.min(pts);
            end = end.max(pts);
            pics.push(Pic {
                off: r.off,
                key: r.key,
                pts,
                dts,
            });
        }
        let one = ticks(1);
        Ok(EsIndex {
            pics,
            start,
            end: end + one,
        })
    }

    fn seek_es(&mut self, target: i64, mode: SeekMode) -> Result<()> {
        let Some(ix) = &self.es else {
            return Ok(());
        };
        let pics = &ix.pics;
        let upto = pics.partition_point(|p| p.dts <= target);
        let landing = match mode {
            SeekMode::Exact => upto.checked_sub(1).map(|i| (pics[i].off, 0, pics[i].off)),
            SeekMode::SyncBefore => pics[..upto]
                .iter()
                .rev()
                .find(|p| p.key && p.pts <= target)
                .map(|p| (p.off, 0, p.off)),
            SeekMode::SyncAfter => pics
                .iter()
                .find(|p| p.key && p.pts >= target)
                .map(|p| (p.off, 0, p.off)),
        };
        self.land(landing, mode, None)
    }
}

/// The codec of a video stream with no program stream map entry, from the first start
/// code of its first packet that tells: 0x02 MPEG video, 0x1B H.264, 0x24 HEVC.
fn sniff_video(p: &[u8]) -> Option<u8> {
    let mut i = 0;
    for _ in 0..64 {
        while i + 4 <= p.len() && p[i..i + 3] != [0, 0, 1] {
            i += 1;
        }
        let b = *p.get(i + 3)?;
        match b {
            0x00 | 0xB2 | 0xB3 | 0xB5 | 0xB8 => return Some(0x02),
            0x40 | 0x42 | 0x44 | 0x46 | 0x4E | 0x26 | 0x28 => return Some(0x24),
            // SPS / PPS with a non-zero nal_ref_idc; slice-like bytes are MPEG-2 slice codes.
            0x47 | 0x48 | 0x67 | 0x68 => return Some(0x1B),
            _ => {}
        }
        i += 4;
    }
    None
}

/// MPEG audio (0x03) or AAC in ADTS (0x0F) from the first sync pattern of the payload.
fn sniff_audio(p: &[u8]) -> Option<u8> {
    p.windows(2)
        .find(|w| w[0] == 0xFF && w[1] & 0xF0 == 0xF0)
        .map(|w| if w[1] & 0x06 == 0 { 0x0F } else { 0x03 })
}

/// MPEG-1 PES header: `(pts, dts, payload offset)`.
fn parse_pes1(b: &[u8]) -> Option<(Option<u64>, Option<u64>, usize)> {
    let mut i = 6usize;
    let mut stuff = 0;
    while *b.get(i)? == 0xFF {
        i += 1;
        stuff += 1;
        if stuff > 16 {
            return None;
        }
    }
    if b[i] & 0xC0 == 0x40 {
        i += 2;
    }
    let f = *b.get(i)?;
    match f >> 4 {
        2 => Some((Some(read_ts(b.get(i..)?)?), None, i + 5)),
        3 => Some((
            Some(read_ts(b.get(i..)?)?),
            Some(read_ts(b.get(i + 5..)?)?),
            i + 10,
        )),
        _ if f == 0x0F => Some((None, None, i + 1)),
        _ => None,
    }
}

impl<R: Read + Seek + Send> Demuxer for PsDemuxer<R> {
    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    fn next_packet(&mut self) -> Result<Packet> {
        loop {
            let Some(q) = self.pump()? else {
                return Err(Error::Eof);
            };
            if let Some(p) = self.make_packet(q) {
                return Ok(p);
            }
        }
    }

    fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<()> {
        let saved = self.stats;
        self.frozen = true;
        let r = self.seek_scan(stream, to, mode);
        self.frozen = false;
        self.stats = saved;
        r
    }
}
