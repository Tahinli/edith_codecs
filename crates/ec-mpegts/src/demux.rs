//! The transport stream demuxer.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek};

use ec_core::registry::{Demuxer, SeekMode, StreamInfo};
use ec_core::{Error, Packet, PacketFlags, Result, Rounding, TimeBase, Timestamp};

use crate::codec::{StreamKind, annexb_to_length_prefixed};
use crate::psi::{self, EsEntry, PatEntry, Pmt, SectionBuf};
use crate::track::{MAX_PES, PesIn, Queued, TICKS, Track};
use crate::ts::{self, Layout, SYNC, TS_LEN};
use crate::window::Window;

/// Most programs listed, tracks kept (across PMT changes) and bytes probed.
const MAX_PROGRAMS: usize = 64;
const MAX_TRACKS: usize = 128;
const PROBE_BYTES: u64 = 32 << 20;
/// Bisection stops when the interval is this small; the rest is a forward scan.
const BISECT_STOP: u64 = 64 << 10;

/// How video access units are delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoFraming {
    /// Annex B start codes, parameter sets in band (what the bytes of the stream are).
    #[default]
    AnnexB,
    /// Every NAL unit preceded by a 4-byte big-endian length, as `avc1`/`hvc1` samples
    /// are. The `extradata` of the stream is the matching `avcC`/`hvcC` record.
    LengthPrefixed,
}

/// A program of the PAT, with what its PMT says once that was seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramInfo {
    /// `program_number`.
    pub number: u16,
    /// PID of the PMT.
    pub pmt_pid: u16,
    /// PID that carries the PCR, once the PMT was seen.
    pub pcr_pid: Option<u16>,
    /// The PMT's elementary streams (empty until the PMT was seen).
    pub streams: Vec<EsEntry>,
}

/// One elementary stream of the selected program, whether or not it is exposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    /// Elementary PID.
    pub pid: u16,
    /// PMT `stream_type`.
    pub stream_type: u8,
    /// What the stream carries.
    pub kind: StreamKind,
    /// Index in [`Demuxer::streams`] when the stream is exposed.
    pub stream: Option<u32>,
    /// Why the stream is not exposed, naming the codec.
    pub refusal: Option<String>,
    /// ISO 639 language, when the PMT gives one.
    pub language: Option<String>,
}

/// Counters of what the demuxer had to put up with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Times the sync byte was lost and found again.
    pub resyncs: u64,
    /// Continuity-counter gaps on tracked PIDs.
    pub cc_errors: u64,
    /// Duplicate packets dropped.
    pub duplicates: u64,
    /// PSI sections with a bad CRC.
    pub crc_errors: u64,
    /// Packets with the transport error indicator set.
    pub tei: u64,
    /// `discontinuity_indicator` seen.
    pub discontinuities: u64,
    /// PMT changes after the first (of the selected program).
    pub pmt_changes: u64,
    /// PES packets dropped (bad start code, oversize).
    pub bad_pes: u64,
}

enum Cc {
    Ok,
    Dup,
    Gap,
}

pub(crate) fn unwrap33(raw: u64, last: Option<i64>) -> i64 {
    const M: i64 = 1 << 33;
    let raw = raw as i64;
    match last {
        None => raw,
        Some(l) => {
            let mut d = (raw - l).rem_euclid(M);
            if d >= M / 2 {
                d -= M;
            }
            l + d
        }
    }
}

pub(crate) fn read_ts(b: &[u8]) -> Option<u64> {
    let b = b.get(..5)?;
    if b[0] & 1 == 0 || b[2] & 1 == 0 || b[4] & 1 == 0 {
        return None;
    }
    Some(
        (u64::from((b[0] >> 1) & 7) << 30)
            | (u64::from(b[1]) << 22)
            | (u64::from(b[2] >> 1) << 15)
            | (u64::from(b[3]) << 7)
            | u64::from(b[4] >> 1),
    )
}

/// PES header facts.
pub(crate) struct PesHead {
    pub pts: Option<u64>,
    pub dts: Option<u64>,
    pub payload: usize,
}

pub(crate) fn parse_pes(buf: &[u8]) -> Option<PesHead> {
    if buf.get(..3)? != [0, 0, 1] {
        return None;
    }
    let id = *buf.get(3)?;
    if matches!(id, 0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF) {
        return Some(PesHead {
            pts: None,
            dts: None,
            payload: 6,
        });
    }
    let h = buf.get(..9)?;
    if h[6] & 0xC0 != 0x80 {
        return None;
    }
    let flags = h[7] >> 6;
    let payload = 9 + usize::from(h[8]);
    if payload > buf.len() {
        return None;
    }
    let pts = if flags & 2 != 0 {
        read_ts(buf.get(9..)?)
    } else {
        None
    };
    let dts = if flags == 3 {
        read_ts(buf.get(14..)?)
    } else {
        None
    };
    Some(PesHead { pts, dts, payload })
}

/// MPEG-2 transport stream demuxer.
///
/// Opens `.ts` (188-byte), BDAV `.m2ts`/`.mts` (192-byte, 4-byte `TP_extra_header`)
/// and 204-byte (Reed-Solomon) streams, detected by sync-byte stride. The first
/// program is exposed unless [`TsDemuxer::with_program`] names another.
pub struct TsDemuxer<R> {
    win: Window<R>,
    file_len: u64,
    layout: Layout,
    first_off: u64,
    pos: u64,
    programs: Vec<ProgramInfo>,
    sel: Option<u16>,
    pmt_seen: bool,
    pcr_pid: Option<u16>,
    first_pcr: Option<u64>,
    sections: HashMap<u16, SectionBuf>,
    /// Last continuity counter per PID, with the packet that carried it.
    cc: HashMap<u16, (u8, Box<[u8; TS_LEN]>)>,
    tracks: Vec<Track>,
    pid_map: HashMap<u16, usize>,
    streams: Vec<StreamInfo>,
    queue: VecDeque<Queued>,
    eof: bool,
    stats: Stats,
    framing: VideoFraming,
    last_off: u64,
    duration: Option<i64>,
    /// While scanning (tail, bisection) PSI is read but not acted on.
    frozen: bool,
}

impl<R: Read + Seek> TsDemuxer<R> {
    /// Opens `reader`, exposing the first program.
    pub fn new(reader: R) -> Result<TsDemuxer<R>> {
        Self::open(reader, None)
    }

    /// Opens `reader`, exposing the program with this `program_number`.
    pub fn with_program(reader: R, program_number: u16) -> Result<TsDemuxer<R>> {
        Self::open(reader, Some(program_number))
    }

    fn open(reader: R, program: Option<u16>) -> Result<TsDemuxer<R>> {
        let mut win = Window::new(reader);
        let file_len = win.len()?;
        let head = win.get(0, 64 * 1024)?;
        let Some((layout, first)) = ts::detect(head) else {
            return Err(Error::corrupt("mpegts: no transport packet sync found"));
        };
        let first_off = first as u64;
        let mut d = TsDemuxer {
            win,
            file_len,
            layout,
            first_off,
            pos: first_off,
            programs: Vec::new(),
            sel: program,
            pmt_seen: false,
            pcr_pid: None,
            first_pcr: None,
            sections: HashMap::new(),
            cc: HashMap::new(),
            tracks: Vec::new(),
            pid_map: HashMap::new(),
            streams: Vec::new(),
            queue: VecDeque::new(),
            eof: false,
            stats: Stats::default(),
            framing: VideoFraming::AnnexB,
            last_off: first_off,
            duration: None,
            frozen: false,
        };
        d.probe()?;
        if !d.pmt_seen {
            return Err(Error::corrupt(match (d.programs.is_empty(), program) {
                (true, _) => "mpegts: no PAT found".to_string(),
                (false, Some(n)) => format!("mpegts: program {n} has no PMT"),
                (false, None) => "mpegts: no PMT found".to_string(),
            }));
        }
        d.finish_open()?;
        Ok(d)
    }

    // ----- public queries ---------------------------------------------------

    /// Bytes per transport packet in the file: 188, 192 or 204.
    pub fn packet_size(&self) -> usize {
        self.layout.size
    }

    /// True for BDAV (`.m2ts`/`.mts`) packets with a 4-byte `TP_extra_header`.
    pub fn is_bdav(&self) -> bool {
        self.layout.sync_off == 4
    }

    /// Every program of the PAT with its PMT streams.
    pub fn programs(&self) -> &[ProgramInfo] {
        &self.programs
    }

    /// `program_number` of the program being read.
    pub fn program_number(&self) -> Option<u16> {
        self.sel
    }

    /// Every elementary stream of the selected program (including refused ones), in
    /// PMT order, with later PMT additions after.
    pub fn tracks(&self) -> Vec<TrackInfo> {
        self.tracks
            .iter()
            .map(|t| TrackInfo {
                pid: t.es.pid,
                stream_type: t.es.stream_type,
                kind: t.kind.clone(),
                stream: t.stream,
                refusal: t.refusal.clone(),
                language: t.es.language(),
            })
            .collect()
    }

    /// PID of stream `stream`.
    pub fn stream_pid(&self, stream: u32) -> Option<u16> {
        self.tracks
            .iter()
            .find(|t| t.stream == Some(stream))
            .map(|t| t.es.pid)
    }

    /// The 90 kHz time base of every packet.
    pub fn time_base(&self) -> TimeBase {
        TimeBase::new(1, TICKS)
    }

    /// Length of the file in seconds, from the first and last timestamps.
    pub fn duration_secs(&self) -> Option<f64> {
        self.duration.map(|d| d as f64 / TICKS as f64)
    }

    /// Counters of what the demuxer repaired or skipped so far.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// First PCR (27 MHz) seen on the program's PCR PID.
    pub fn first_pcr(&self) -> Option<u64> {
        self.first_pcr
    }

    /// PCR PID of the selected program.
    pub fn pcr_pid(&self) -> Option<u16> {
        self.pcr_pid
    }

    /// File offset of the transport packet that began the last packet returned by
    /// [`Demuxer::next_packet`]; after [`Demuxer::seek`] it is where the read resumes.
    pub fn last_packet_offset(&self) -> u64 {
        self.last_off
    }

    /// Sets how H.264 and HEVC packets are framed (default Annex B).
    pub fn set_video_framing(&mut self, framing: VideoFraming) {
        self.framing = framing;
    }

    /// The video framing in effect.
    pub fn video_framing(&self) -> VideoFraming {
        self.framing
    }

    // ----- engine -------------------------------------------------------------

    fn cc_check(&mut self, h: &ts::Header, pkt: &[u8; TS_LEN]) -> Cc {
        if !h.has_payload {
            return Cc::Ok;
        }
        let prev = self.cc.insert(h.pid, (h.cc, Box::new(*pkt)));
        match prev {
            None => Cc::Ok,
            // A repeat of the packet before it is the one repeat the standard allows.
            Some((p, last)) if p == h.cc && *last == *pkt => Cc::Dup,
            Some((p, _)) if (p + 1) & 15 == h.cc => Cc::Ok,
            Some(_) if h.discontinuity => Cc::Ok,
            Some(_) => Cc::Gap,
        }
    }

    fn mark_bad(&mut self) {
        for t in &mut self.tracks {
            if t.pes_on {
                t.pes_bad = true;
            }
        }
    }

    /// Looks for the next confirmed packet start after `from`. `None` at end of file.
    fn resync(&mut self, from: u64) -> Result<Option<u64>> {
        let mut at = from;
        let step = 64 * 1024usize;
        let overlap = (self.layout.size * 5) as u64;
        loop {
            let buf = self.win.get(at, step)?;
            if buf.is_empty() {
                return Ok(None);
            }
            let full = buf.len() == step;
            if let Some(i) = ts::find_sync(buf, 0, self.layout) {
                return Ok(Some(at + i as u64));
            }
            if !full {
                return Ok(None);
            }
            at += step as u64 - overlap;
        }
    }

    /// Processes one transport packet. `Ok(false)` once the end was reached and
    /// everything buffered was flushed.
    fn step(&mut self) -> Result<bool> {
        if self.eof {
            return Ok(false);
        }
        let size = self.layout.size;
        let so = self.layout.sync_off;
        let mut pkt = [0u8; TS_LEN];
        let ok = {
            let b = self.win.get(self.pos, size + so + 1)?;
            if b.len() < so + TS_LEN {
                if !b.is_empty() {
                    // A packet cut short: whatever PES was open is cut short with it.
                    self.mark_bad();
                }
                self.finish();
                return Ok(false);
            }
            let next_ok = b.get(size + so).is_none_or(|&n| n == SYNC);
            if b[so] == SYNC && next_ok {
                pkt.copy_from_slice(&b[so..so + TS_LEN]);
                true
            } else {
                false
            }
        };
        if !ok {
            self.stats.resyncs += 1;
            self.mark_bad();
            self.cc.clear();
            for s in self.sections.values_mut() {
                s.reset();
            }
            match self.resync(self.pos + 1)? {
                Some(p) => self.pos = p,
                None => {
                    self.finish();
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        let off = self.pos;
        self.pos += size as u64;
        let Some(h) = ts::parse(&pkt) else {
            self.mark_bad();
            return Ok(true);
        };
        if h.pid == 0x1FFF {
            return Ok(true);
        }
        if h.tei {
            self.stats.tei += 1;
            if let Some(&ti) = self.pid_map.get(&h.pid) {
                self.tracks[ti].pes_bad = true;
            }
            return Ok(true);
        }
        if Some(h.pid) == self.pcr_pid
            && let Some(pcr) = h.pcr
        {
            self.first_pcr.get_or_insert(pcr);
        }
        if h.discontinuity {
            self.stats.discontinuities += 1;
        }
        let is_psi = h.pid == 0 || self.programs.iter().any(|p| p.pmt_pid == h.pid);
        let tracked = self.pid_map.get(&h.pid).copied();
        if !is_psi && tracked.is_none() {
            return Ok(true);
        }
        let cc = self.cc_check(&h, &pkt);
        match cc {
            Cc::Dup => {
                self.stats.duplicates += 1;
                return Ok(true);
            }
            Cc::Gap => {
                self.stats.cc_errors += 1;
                if let Some(ti) = tracked {
                    self.tracks[ti].pes_bad = true;
                }
                if let Some(s) = self.sections.get_mut(&h.pid) {
                    s.reset();
                }
            }
            Cc::Ok => {}
        }
        if !h.has_payload {
            return Ok(true);
        }
        let payload = &pkt[h.payload.0..h.payload.1];
        if is_psi {
            let mut out = Vec::new();
            let bad = self
                .sections
                .entry(h.pid)
                .or_default()
                .push(h.pusi, payload, &mut out);
            self.stats.crc_errors += u64::from(bad);
            for s in out {
                self.on_section(h.pid, &s);
            }
            return Ok(true);
        }
        if let Some(ti) = tracked {
            self.on_es_payload(ti, &h, payload, off);
        }
        Ok(true)
    }

    fn on_es_payload(&mut self, ti: usize, h: &ts::Header, payload: &[u8], off: u64) {
        if !self.tracks[ti].wanted() {
            return;
        }
        if h.pusi {
            self.finish_pes(ti);
            let t = &mut self.tracks[ti];
            t.pes.clear();
            t.pes_on = true;
            t.pes_off = off;
            t.pes_rai = h.random_access;
            t.pes_bad = false;
            t.pes_expect = None;
            if payload.len() >= 3 && payload[..3] != [0, 0, 1] {
                t.pes_on = false;
                self.stats.bad_pes += 1;
                return;
            }
        } else if !self.tracks[ti].pes_on {
            return;
        }
        let t = &mut self.tracks[ti];
        t.pes.extend_from_slice(payload);
        if t.pes_expect.is_none() && t.pes.len() >= 6 {
            let len = (usize::from(t.pes[4]) << 8) | usize::from(t.pes[5]);
            if len > 0 {
                t.pes_expect = Some(6 + len);
            }
        }
        if t.pes.len() > MAX_PES {
            t.pes_on = false;
            t.pes.clear();
            self.stats.bad_pes += 1;
            return;
        }
        if let Some(n) = t.pes_expect
            && t.pes.len() >= n
        {
            t.pes.truncate(n);
            self.finish_pes(ti);
        }
    }

    fn finish_pes(&mut self, ti: usize) {
        let t = &mut self.tracks[ti];
        if !t.pes_on {
            return;
        }
        t.pes_on = false;
        let buf = std::mem::take(&mut t.pes);
        let Some(head) = parse_pes(&buf) else {
            self.stats.bad_pes += 1;
            t.pes = buf;
            t.pes.clear();
            return;
        };
        let pts = head.pts.map(|raw| unwrap33(raw, t.last_pts));
        if let Some(p) = pts {
            t.last_pts = Some(p);
        }
        let dts = match (head.dts, head.pts, pts) {
            (Some(d), Some(rp), Some(p)) => {
                const M: i64 = 1 << 33;
                let mut delta = (d as i64 - rp as i64).rem_euclid(M);
                if delta >= M / 2 {
                    delta -= M;
                }
                Some(p + delta)
            }
            _ => None,
        };
        let input = PesIn {
            payload: &buf[head.payload..],
            pts,
            dts,
            off: t.pes_off,
            rai: t.pes_rai,
            bad: t.pes_bad,
        };
        t.push_pes(ti, &input, &mut self.queue);
        t.pes_bad = false;
        t.pes = buf;
        t.pes.clear();
    }

    fn finish(&mut self) {
        for ti in 0..self.tracks.len() {
            let t = &mut self.tracks[ti];
            if t.pes_on && t.pes_expect.is_some_and(|n| t.pes.len() < n) {
                t.pes_bad = true;
            }
            self.finish_pes(ti);
            self.tracks[ti].flush(ti, &mut self.queue);
        }
        self.eof = true;
    }

    // ----- PSI --------------------------------------------------------------

    fn on_section(&mut self, pid: u16, sec: &[u8]) {
        if self.frozen {
            return;
        }
        if pid == 0 {
            if let Some((_, entries)) = psi::parse_pat(sec) {
                self.apply_pat(entries);
            }
        } else if let Some(pmt) = psi::parse_pmt(sec)
            && self
                .programs
                .iter()
                .any(|p| p.pmt_pid == pid && p.number == pmt.program_number)
        {
            self.apply_pmt(pmt);
        }
    }

    fn apply_pat(&mut self, entries: Vec<PatEntry>) {
        let mut next: Vec<ProgramInfo> = Vec::new();
        for e in entries.into_iter().take(MAX_PROGRAMS) {
            if next.iter().any(|p| p.number == e.program_number) {
                continue;
            }
            let old = self.programs.iter().find(|p| p.number == e.program_number);
            next.push(ProgramInfo {
                number: e.program_number,
                pmt_pid: e.pmt_pid,
                pcr_pid: old.and_then(|o| o.pcr_pid),
                streams: old.map(|o| o.streams.clone()).unwrap_or_default(),
            });
        }
        if self.sel.is_none() {
            self.sel = next.first().map(|p| p.number);
        }
        self.programs = next;
    }

    fn apply_pmt(&mut self, pmt: Pmt) {
        if let Some(p) = self
            .programs
            .iter_mut()
            .find(|p| p.number == pmt.program_number)
        {
            p.pcr_pid = Some(pmt.pcr_pid);
            p.streams = pmt.streams.clone();
        }
        if Some(pmt.program_number) != self.sel {
            return;
        }
        self.pcr_pid = Some(pmt.pcr_pid);
        let same = self.pmt_seen
            && self
                .tracks
                .iter()
                .filter(|t| t.active)
                .map(|t| &t.es)
                .eq(pmt.streams.iter());
        if same {
            return;
        }
        if self.pmt_seen {
            self.stats.pmt_changes += 1;
        }
        self.pmt_seen = true;
        for ti in 0..self.tracks.len() {
            let t = &self.tracks[ti];
            let kept = pmt
                .streams
                .iter()
                .any(|e| e.pid == t.es.pid && e.stream_type == t.es.stream_type);
            if t.active && !kept {
                self.finish_pes(ti);
                self.tracks[ti].flush(ti, &mut self.queue);
                self.tracks[ti].active = false;
            }
        }
        for e in &pmt.streams {
            if let Some(t) = self
                .tracks
                .iter_mut()
                .find(|t| t.es.pid == e.pid && t.es.stream_type == e.stream_type)
            {
                t.es = e.clone();
                if !t.active {
                    t.active = true;
                    t.reset(None);
                }
            } else if self.tracks.len() < MAX_TRACKS {
                self.tracks.push(Track::new(e.clone()));
            }
        }
        self.pid_map.clear();
        for (i, t) in self.tracks.iter().enumerate() {
            if t.active {
                self.pid_map.insert(t.es.pid, i);
            }
        }
    }

    // ----- open ---------------------------------------------------------------

    fn probe_complete(&self) -> bool {
        if !self.pmt_seen {
            return false;
        }
        let all_ready = self.tracks.iter().filter(|t| t.wanted()).all(|t| t.ready());
        // A dozen pictures to read a frame rate from, a few frames of everything else.
        let timed = self.tracks.iter().filter(|t| t.wanted()).all(|t| {
            if t.kind.is_video() {
                t.dts_seen.len() >= 12
            } else {
                t.packets >= 3 || t.kind.is_bitmap_sub()
            }
        });
        all_ready && timed
    }

    fn probe(&mut self) -> Result<()> {
        let start = self.pos;
        loop {
            self.queue.clear();
            if self.eof {
                break;
            }
            let read = self.pos - start;
            if read >= PROBE_BYTES || self.probe_complete() {
                break;
            }
            self.step()?;
        }
        self.queue.clear();
        Ok(())
    }

    fn reset_at(&mut self, off: u64, seed: Option<i64>) {
        self.queue.clear();
        self.eof = false;
        self.pos = off;
        self.cc.clear();
        for s in self.sections.values_mut() {
            s.reset();
        }
        for t in &mut self.tracks {
            let s = seed.or(t.start_time);
            t.reset(s);
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
        self.reset_at(self.first_off, None);
        self.stats = Stats::default();
        Ok(())
    }

    fn assign_streams(&mut self) {
        for ti in 0..self.tracks.len() {
            self.make_stream(ti);
        }
    }

    /// Gives track `ti` a stream index when it can be described. Returns it.
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
        let size = self.layout.size as u64;
        let mut win: u64 = 4 << 20;
        loop {
            for t in &mut self.tracks {
                t.max_end = None;
            }
            let from = self.file_len.saturating_sub(win);
            let off = if from <= self.first_off {
                self.first_off
            } else {
                self.first_off + (from - self.first_off) / size * size
            };
            self.reset_at(off, None);
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

    // ----- reading --------------------------------------------------------------

    fn pump(&mut self) -> Result<Option<Queued>> {
        loop {
            if let Some(q) = self.queue.pop_front() {
                return Ok(Some(q));
            }
            if !self.step()? {
                return Ok(self.queue.pop_front());
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

    // ----- seeking ----------------------------------------------------------------

    fn align(&self, off: u64) -> u64 {
        let size = self.layout.size as u64;
        if off <= self.first_off {
            self.first_off
        } else {
            self.first_off + (off - self.first_off) / size * size
        }
    }

    /// Decode (or presentation) time of the first packet of track `ti` at or after `off`.
    fn time_at(&mut self, off: u64, ti: usize) -> Result<Option<i64>> {
        self.reset_at(off, None);
        while let Some(q) = self.pump()? {
            if q.track == ti
                && let Some(t) = q.dts.or(q.pts)
            {
                return Ok(Some(t));
            }
        }
        Ok(None)
    }

    /// Scans forward from `from` for the landing packet of a seek to `target`.
    fn scan_for(
        &mut self,
        from: u64,
        ti: usize,
        target: i64,
        mode: SeekMode,
        all_sync: bool,
    ) -> Result<Option<(u64, i64)>> {
        self.reset_at(from, None);
        let mut best = None;
        while let Some(q) = self.pump()? {
            if q.track != ti {
                continue;
            }
            let Some(pts) = q.pts.or(q.dts) else {
                continue;
            };
            let dts = q.dts.unwrap_or(pts);
            let key = all_sync || q.key;
            match mode {
                SeekMode::SyncAfter => {
                    if key && pts >= target {
                        return Ok(Some((q.off, pts)));
                    }
                }
                SeekMode::Exact => {
                    if dts > target {
                        break;
                    }
                    best = Some((q.off, pts));
                }
                SeekMode::SyncBefore => {
                    if dts > target {
                        break;
                    }
                    if key && pts <= target {
                        best = Some((q.off, pts));
                    }
                }
            }
        }
        Ok(best)
    }
}

impl<R: Read + Seek + Send> Demuxer for TsDemuxer<R> {
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
        // The landing re-read starts with the PSI of the stream again.
        r
    }
}

impl<R: Read + Seek> TsDemuxer<R> {
    fn seek_scan(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<()> {
        let Some(ti) = self.tracks.iter().position(|t| t.stream == Some(stream)) else {
            return Err(Error::corrupt(format!(
                "mpegts: no stream {stream} to seek"
            )));
        };
        // The picture decides where to land; without one, the stream itself.
        let ref_ti = self
            .tracks
            .iter()
            .position(|t| t.stream.is_some() && t.kind.is_video())
            .unwrap_or(ti);
        let all_sync = !self.tracks[ref_ti].kind.is_video();
        let target = to.rescale(self.time_base(), Rounding::Nearest).ticks;
        let start = self.tracks[ref_ti].start_time.unwrap_or(0);
        let landing = if target <= start && mode != SeekMode::Exact {
            None
        } else {
            let mut lo = self.first_off;
            let mut hi = self.file_len;
            while hi - lo > BISECT_STOP {
                let mid = self.align(lo + (hi - lo) / 2);
                if mid <= lo {
                    break;
                }
                match self.time_at(mid, ref_ti)? {
                    Some(t) if t <= target => lo = mid,
                    _ => hi = mid,
                }
            }
            let mut back: u64 = 256 << 10;
            loop {
                let from = self.align(lo.saturating_sub(back));
                if let Some(found) = self.scan_for(from, ref_ti, target, mode, all_sync)? {
                    break Some(found);
                }
                if from <= self.first_off {
                    break None;
                }
                back = back.saturating_mul(4);
            }
        };
        match landing {
            Some((off, pts)) => {
                self.reset_at(off, Some(pts));
                self.last_off = off;
            }
            None if mode == SeekMode::SyncAfter => {
                self.reset_at(self.file_len, None);
                self.last_off = self.file_len;
                return Err(Error::Eof);
            }
            None => {
                self.reset_at(self.first_off, None);
                self.last_off = self.first_off;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwrap_follows_the_clock_across_2_pow_33() {
        let m = 1i64 << 33;
        assert_eq!(unwrap33(5, None), 5);
        assert_eq!(unwrap33(10, Some(5)), 10);
        // Forward over the wrap.
        assert_eq!(unwrap33(100, Some(m - 100)), m + 100);
        // A little backwards (B-frame pts) across the wrap stays put.
        assert_eq!(unwrap33(m as u64 - 50, Some(m + 20)), m - 50);
        // Far later: still the nearest candidate.
        assert_eq!(unwrap33(7, Some(3 * m + 1)), 3 * m + 7);
    }

    #[test]
    fn pes_header_timestamps() {
        let mut p = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0xC0, 10];
        p.extend_from_slice(&[0x31, 0, 1, 0, 3]); // pts 1
        p.extend_from_slice(&[0x11, 0, 1, 0, 1]); // dts 0
        p.extend_from_slice(&[9, 9]);
        let h = parse_pes(&p).unwrap();
        assert_eq!((h.pts, h.dts, h.payload), (Some(1), Some(0), 19));
        assert!(parse_pes(&p[..12]).is_none());
        assert!(parse_pes(&[1, 2, 3]).is_none());
    }
}
