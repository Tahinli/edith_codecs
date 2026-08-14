//! The Matroska/WebM reader: header walk, cluster walk, cue-driven seek.

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

use ec_core::{
    AudioParameters, Buf, ChannelLayout, CodecId, CodecParameters, ColorInfo, ContentLight,
    Demuxer, Error, MediaParameters, MediaType, Packet, PacketFlags, Result, Rounding, SeekMode,
    StreamInfo, TimeBase, Timestamp, VideoParameters,
};

use crate::ebml::{self, Elements};

/// The largest header field this reads whole: a `CodecPrivate` is a few hundred
/// bytes and an `Info` is smaller, so a megabyte is a ceiling and never a limit.
const FIELD_LIMIT: u64 = 1 << 20;
/// ...and for the two elements that are legitimately large: a 2160p film's
/// `Cues` are a few hundred kilobytes, and one cluster is a few megabytes.
const CUES_LIMIT: u64 = 64 << 20;
const CLUSTER_LIMIT: u64 = 256 << 20;
/// How much of a cluster whose length was never written is read at a time.
const UNKNOWN_CLUSTER_WINDOW: u64 = 8 << 20;
/// A block that says it inflates to more than this is refused rather than
/// allocated: a crafted zlib block must not become all the memory there is.
const INFLATE_LIMIT: usize = 64 << 20;

/// What a track's `ContentEncodings` element did to every one of its frames,
/// undone here on the way back out.
#[derive(Debug, Clone, Default, PartialEq)]
enum Unpack {
    #[default]
    None,
    /// Header stripping (`ContentCompAlgo` 3): bytes every frame of the track
    /// begins with, cut off by the muxer and written once into
    /// `ContentCompSettings`. A decoder handed the rest sees garbage — one
    /// stripped zero byte is the whole difference between a film that plays and
    /// a film that does not.
    Prepend(Vec<u8>),
    /// zlib (`ContentCompAlgo` 0), which mkvmerge compressed subtitle tracks
    /// with by default for years.
    Zlib,
    /// A scheme this cannot undo, in the words a caller refuses with. Carried
    /// rather than raised where the tracks are walked, so one unreadable *audio*
    /// track does not refuse a file whose picture is fine.
    Refused(String),
}

impl Unpack {
    /// One frame put back the way the encoder wrote it. [`Unpack::None`] hands
    /// back the slice of the cluster buffer it already is — no copy, which is
    /// the whole point of reading a cluster into one allocation.
    fn frame(&self, buf: &Buf, range: Range<usize>) -> Result<Buf> {
        Ok(match self {
            Unpack::None => buf.slice(range),
            Unpack::Prepend(head) => {
                let mut out = Vec::with_capacity(head.len() + range.len());
                out.extend_from_slice(head);
                out.extend_from_slice(&buf[range]);
                Buf::from_vec(out)
            }
            Unpack::Zlib => Buf::from_vec(ec_inflate::inflate_zlib(&buf[range], INFLATE_LIMIT)?),
            Unpack::Refused(why) => {
                return Err(Error::unsupported(
                    why.clone(),
                    "no decoder for this encoding",
                ));
            }
        })
    }
}

/// One `TrackEntry`, as far as the packet loop cares about it.
struct Track {
    number: u64,
    /// Index in [`MatroskaDemuxer::streams`], or [`None`] for a track this
    /// build has no codec id for — its blocks are walked past, not refused.
    stream: Option<u32>,
    unpack: Unpack,
    /// `DefaultDuration` in timestamp-scale ticks: what a laced frame steps by
    /// and what a block with no `BlockDuration` lasts.
    default_duration: Option<i64>,
}

/// One `CuePoint`: an instant, the track it indexes and where that cluster is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cue {
    time: i64,
    track: u64,
    /// Absolute file offset of the `Cluster` element.
    cluster: u64,
}

/// A Matroska (`.mkv`, `.mka`, `.mks`, `.mk3d`) or WebM reader over anything
/// seekable.
///
/// Opening walks the header only — everything up to the first `Cluster` — so a
/// 12 GB film costs a handful of reads. The index a seek needs is built the
/// first time [`Demuxer::seek`] is called: `Cues` where the file has them,
/// found in front of the clusters or through the `SeekHead` at the far end, and
/// a walk of the cluster headers where it does not. Both live on the *one*
/// reader this was opened with; nothing here ever reopens the file.
pub struct MatroskaDemuxer<R> {
    src: Src<R>,
    /// Body start and end of the `Segment`.
    segment: (u64, u64),
    /// `TimestampScale`, in nanoseconds, as the time base every timestamp in
    /// this file is in.
    time_base: TimeBase,
    doc_type: String,
    streams: Vec<StreamInfo>,
    tracks: Vec<Track>,
    /// Where the top-level walk resumes.
    next_pos: u64,
    first_cluster: u64,
    /// Body range of the `Cues` element, when the header walk or the `SeekHead`
    /// named one; resolved into `cues` on the first seek.
    cues_at: Option<(u64, u64)>,
    cues: Vec<Cue>,
    /// `(cluster timestamp, file offset)` for a file with no usable `Cues`,
    /// built by walking the cluster headers once.
    clusters: Vec<(i64, u64)>,
    indexed: bool,
    queue: VecDeque<Packet>,
}

/// Positioned reads over one reader, which is what keeps a seek from ever
/// needing a second one.
struct Src<R> {
    r: R,
    pos: u64,
    len: u64,
}

impl<R: Read + Seek> Src<R> {
    fn new(mut r: R) -> Result<Src<R>> {
        let len = r.seek(SeekFrom::End(0))?;
        r.seek(SeekFrom::Start(0))?;
        Ok(Src { r, pos: 0, len })
    }

    fn seek_to(&mut self, at: u64) -> Result<()> {
        if at != self.pos {
            self.r.seek(SeekFrom::Start(at))?;
            self.pos = at;
        }
        Ok(())
    }

    /// Up to `buf.len()` bytes, as many as are left.
    fn read_upto(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut got = 0;
        while got < buf.len() {
            match self.r.read(&mut buf[got..])? {
                0 => break,
                n => got += n,
            }
        }
        self.pos += got as u64;
        Ok(got)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        if self.read_upto(buf)? != buf.len() {
            return Err(Error::NeedMore);
        }
        Ok(())
    }

    /// The element header at `at`: its id, where its payload starts and where it
    /// ends. `None` at `end`. An element of unknown length — what a muxer writes
    /// while it is still recording — runs to the end of its parent, which is
    /// exactly how a reader is told to take it.
    /// The fourth answer is whether the length was *stated*: an unknown-length
    /// cluster runs to its parent's end, and reading that as a size would ask
    /// for the rest of the file in one allocation.
    fn elem_at(&mut self, at: u64, end: u64) -> Result<Option<(u32, u64, u64, bool)>> {
        if at + 2 > end {
            return Ok(None);
        }
        let mut head = [0u8; 12];
        self.seek_to(at)?;
        let n = self.read_upto(&mut head[..(end - at).min(12) as usize])?;
        let (id, size, head_len) = ebml::header(&head[..n])?;
        let body = at + head_len as u64;
        if body > end {
            return Err(Error::corrupt("Matroska: element header past its parent"));
        }
        let stop = match size {
            Some(size) => body.saturating_add(size).min(end),
            None => end,
        };
        Ok(Some((id, body, stop, size.is_some())))
    }

    /// The payload of a header field, bounded.
    fn body(&mut self, body: u64, stop: u64, limit: u64) -> Result<Vec<u8>> {
        let len = stop.saturating_sub(body);
        if len > limit {
            return Err(Error::corrupt(format!(
                "Matroska: a {len}-byte element where at most {limit} is readable"
            )));
        }
        let mut buf = vec![0u8; len as usize];
        self.seek_to(body)?;
        self.read_exact(&mut buf)?;
        Ok(buf)
    }
}

impl<R: Read + Seek> MatroskaDemuxer<R> {
    /// Open a Matroska stream: EBML header, `Segment`, `Info` and `Tracks`.
    ///
    /// Reading stops at the first `Cluster`, so this costs a handful of seeks
    /// whatever the file weighs.
    pub fn new(reader: R) -> Result<MatroskaDemuxer<R>> {
        let mut src = Src::new(reader)?;
        let end = src.len;

        // The EBML header, for the doc type; `matroska` and `webm` are the two
        // this reads, and anything else is a different format wearing the same
        // element grammar.
        let mut doc_type = String::from("matroska");
        let mut at = 0;
        let segment = loop {
            let Some((id, body, stop, _)) = src.elem_at(at, end)? else {
                return Err(Error::corrupt("Matroska: no Segment element"));
            };
            at = stop;
            match id {
                ebml::EBML_HEADER => {
                    let head = src.body(body, stop, FIELD_LIMIT)?;
                    for (id, r) in Elements::new(&head) {
                        if id == ebml::DOC_TYPE {
                            doc_type = ebml::string_of(&head[r]);
                        }
                    }
                    if doc_type != "matroska" && doc_type != "webm" {
                        return Err(Error::unsupported(
                            format!("EBML DocType {doc_type:?}"),
                            "this reader handles matroska and webm",
                        ));
                    }
                }
                ebml::SEGMENT => break (body, stop),
                _ => {}
            }
        };

        let mut scale_ns = 1_000_000u64;
        let mut duration_ticks = None;
        let mut tracks_body = None;
        let mut cues_at = None;
        let mut seek_head = Vec::new();
        let mut at = segment.0;
        let first_cluster = loop {
            let (id, body, stop, _) = match src.elem_at(at, segment.1) {
                Ok(Some(element)) => element,
                Ok(None) => break segment.1,
                // A header element that does not parse is where this file's
                // damage starts; the packet walk resynchronises on the next
                // `Cluster` from here rather than refusing to open at all.
                Err(_) => break at,
            };
            match id {
                ebml::CLUSTER => break at,
                ebml::INFO => {
                    let info = src.body(body, stop, FIELD_LIMIT)?;
                    for (id, r) in Elements::new(&info) {
                        match id {
                            ebml::TIMESTAMP_SCALE => scale_ns = ebml::uint_of(&info[r]).max(1),
                            ebml::DURATION => duration_ticks = Some(ebml::float_of(&info[r])),
                            _ => {}
                        }
                    }
                }
                // Kept as bytes: `Tracks` is parsed once the timestamp scale is
                // known, and a file may state either element first.
                ebml::TRACKS => tracks_body = Some(src.body(body, stop, CUES_LIMIT)?),
                ebml::CUES => cues_at = Some((body, stop)),
                ebml::SEEK_HEAD => {
                    let head = src.body(body, stop, FIELD_LIMIT)?;
                    seek_head.extend(seek_positions(&head));
                }
                _ => {}
            }
            at = stop;
        };

        // Nearly every file writes its index at the far end and points at it
        // from the `SeekHead`. The pointer is believed only as far as the
        // element it lands on: a stale table — a file edited by a tool that
        // moved the index — points at something that is not `Cues`, and reading
        // that as one is a seek into the middle of a frame.
        if cues_at.is_none()
            && let Some(pos) = seek_head
                .iter()
                .find(|(id, _)| *id == ebml::CUES)
                .map(|(_, p)| *p)
            && let Some((ebml::CUES, body, stop, _)) = src.elem_at(segment.0 + pos, segment.1)?
        {
            cues_at = Some((body, stop));
        }
        // ...and a file whose `Tracks` sit behind the clusters, which is rare
        // and legal, is followed the same way.
        if tracks_body.is_none()
            && let Some(pos) = seek_head
                .iter()
                .find(|(id, _)| *id == ebml::TRACKS)
                .map(|(_, p)| *p)
            && let Some((ebml::TRACKS, body, stop, _)) = src.elem_at(segment.0 + pos, segment.1)?
        {
            tracks_body = Some(src.body(body, stop, CUES_LIMIT)?);
        }

        let time_base = TimeBase::try_new(scale_ns as i64, 1_000_000_000)?;
        let duration = duration_ticks
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| d.round() as i64);
        let (streams, tracks) = parse_tracks(
            tracks_body.as_deref().unwrap_or_default(),
            scale_ns,
            time_base,
            duration,
        );
        if streams.is_empty() {
            return Err(Error::corrupt("Matroska: no readable track in Tracks"));
        }

        Ok(MatroskaDemuxer {
            src,
            segment,
            time_base,
            doc_type,
            streams,
            tracks,
            next_pos: first_cluster,
            first_cluster,
            cues_at,
            cues: Vec::new(),
            clusters: Vec::new(),
            indexed: false,
            queue: VecDeque::new(),
        })
    }

    /// `"matroska"` or `"webm"`, as the file's own EBML header states it.
    pub fn doc_type(&self) -> &str {
        &self.doc_type
    }

    /// The time base every timestamp of this file is in — `TimestampScale`
    /// nanoseconds, exact and rational.
    pub fn time_base(&self) -> TimeBase {
        self.time_base
    }

    /// The `TrackNumber` a stream index came from.
    ///
    /// Stream indices are this crate's own numbering; the `TrackNumber` is the
    /// file's, and it is what a caller naming one language of a dual-audio
    /// remux is holding.
    pub fn track_number(&self, stream: u32) -> Option<u64> {
        self.tracks
            .iter()
            .find(|t| t.stream == Some(stream))
            .map(|t| t.number)
    }

    /// The next cluster into [`MatroskaDemuxer::queue`]; `false` at the end of
    /// the segment. `at` comes back holding the offset of the cluster read.
    fn load_cluster(&mut self, at: &mut u64) -> Result<bool> {
        loop {
            if self.next_pos >= self.segment.1 {
                return Ok(false);
            }
            let start = self.next_pos;
            let element = match self.src.elem_at(start, self.segment.1) {
                Ok(Some(e)) => e,
                Ok(None) => return Ok(false),
                // A header that does not parse is a damaged file, not the end of
                // one: the next `Cluster` id is hunted for and the walk resumes
                // there, which is how a truncated download plays its prefix.
                Err(_) => match self.resync(start + 1)? {
                    true => continue,
                    false => return Ok(false),
                },
            };
            let (id, body, stop, known) = element;
            self.next_pos = stop.max(start + 1);
            if id != ebml::CLUSTER {
                continue;
            }
            // An unknown-length cluster — what a muxer writes while it is still
            // recording — is read a window at a time: its stated end is the end
            // of the file, and that is not a size to allocate.
            let len = match known {
                true => stop.saturating_sub(body),
                false => UNKNOWN_CLUSTER_WINDOW.min(stop.saturating_sub(body)),
            };
            if len > CLUSTER_LIMIT {
                return Err(Error::corrupt(format!("Matroska: a {len}-byte cluster")));
            }
            let mut bytes = vec![0u8; len as usize];
            self.src.seek_to(body)?;
            // A truncated last cluster is read as far as it goes: the file's
            // prefix is what a damaged download has, and it plays.
            let got = self.src.read_upto(&mut bytes)?;
            bytes.truncate(got);
            let buf = Buf::from_vec(bytes);
            let consumed = self.parse_cluster(&buf)?;
            // An unknown-length cluster ends where its children do, and their
            // bytes are where the next top-level element starts.
            if !known {
                self.next_pos = body + consumed as u64;
            }
            *at = start;
            return Ok(true);
        }
    }

    /// The next `Cluster` id at or after `from`, for a file whose element chain
    /// broke. `false` when there is none left.
    fn resync(&mut self, from: u64) -> Result<bool> {
        const WINDOW: usize = 64 << 10;
        let mut at = from;
        let mut buf = vec![0u8; WINDOW];
        while at < self.segment.1 {
            self.src.seek_to(at)?;
            let got = self.src.read_upto(&mut buf)?;
            if got < ebml::CLUSTER_MAGIC.len() {
                break;
            }
            if let Some(off) = buf[..got]
                .windows(ebml::CLUSTER_MAGIC.len())
                .position(|w| w == ebml::CLUSTER_MAGIC)
            {
                self.next_pos = at + off as u64;
                return Ok(true);
            }
            // Overlapped by three bytes so an id split across two windows is
            // still found.
            at += (got - (ebml::CLUSTER_MAGIC.len() - 1)) as u64;
        }
        self.next_pos = self.segment.1;
        Ok(false)
    }

    /// One cluster's blocks into the queue; the answer is how many of its bytes
    /// its children accounted for.
    fn parse_cluster(&mut self, buf: &Buf) -> Result<usize> {
        let mut cluster_ts = 0i64;
        let mut walk = Elements::new(buf);
        loop {
            let before = walk.offset();
            let Some((id, range)) = walk.next() else {
                return Ok(walk.offset());
            };
            match id {
                ebml::CLUSTER_TIMESTAMP => cluster_ts = ebml::uint_of(&buf[range]) as i64,
                ebml::SIMPLE_BLOCK => {
                    // Bit 7 of the flags is the keyframe bit; a `Block` inside a
                    // `BlockGroup` has no such bit, which is the case below.
                    self.emit_block(buf, range, cluster_ts, None, None)?;
                }
                ebml::BLOCK_GROUP => {
                    let start = range.start;
                    let (mut block, mut duration, mut key) = (None, None, true);
                    for (id, child) in Elements::new(&buf[range]) {
                        let child = start + child.start..start + child.end;
                        match id {
                            ebml::BLOCK => block = Some(child),
                            ebml::BLOCK_DURATION => {
                                duration = Some(ebml::uint_of(&buf[child]) as i64)
                            }
                            // A block that references another is not one a
                            // decoder can be started from.
                            ebml::REFERENCE_BLOCK => key = false,
                            _ => {}
                        }
                    }
                    if let Some(block) = block {
                        self.emit_block(buf, block, cluster_ts, duration, Some(key))?;
                    }
                }
                // A cluster whose size was unknown ends at the first element
                // that belongs to its parent instead.
                ebml::CLUSTER | ebml::CUES | ebml::TRACKS | ebml::SEGMENT | ebml::SEEK_HEAD => {
                    return Ok(before);
                }
                _ => {}
            }
        }
    }

    /// One `SimpleBlock`/`Block` into as many packets as it holds — a laced
    /// block is several frames behind a header of sizes.
    fn emit_block(
        &mut self,
        buf: &Buf,
        range: Range<usize>,
        cluster_ts: i64,
        duration: Option<i64>,
        group_key: Option<bool>,
    ) -> Result<()> {
        let head = parse_block(buf, range)?;
        let Some(track) = self.tracks.iter().position(|t| t.number == head.track) else {
            return Ok(());
        };
        let Some(stream) = self.tracks[track].stream else {
            return Ok(());
        };
        let key = group_key.unwrap_or(head.flags & 0x80 != 0);
        let step = self.tracks[track].default_duration;
        let duration = duration.or(step);
        let ts = cluster_ts + i64::from(head.rel);
        for (i, frame) in head.frames.iter().enumerate() {
            // A lace writes one timestamp for all its frames; each one starts a
            // `DefaultDuration` after the one before it, which is the only
            // statement the file makes about where they sit.
            let pts = ts + step.unwrap_or(0) * i as i64;
            let data = self.tracks[track].unpack.frame(buf, frame.clone())?;
            self.queue.push_back(Packet {
                stream,
                time_base: self.time_base,
                pts: Some(pts),
                dts: None,
                duration,
                flags: PacketFlags {
                    keyframe: key,
                    ..PacketFlags::default()
                },
                side_data: Vec::new(),
                data,
            });
        }
        Ok(())
    }

    /// `Cues` where the file has them, a walk of the cluster headers where it
    /// does not. Done once, on the first seek — an open pays for none of it.
    fn build_index(&mut self) -> Result<()> {
        if self.indexed {
            return Ok(());
        }
        self.indexed = true;
        if let Some((body, stop)) = self.cues_at {
            let bytes = self.src.body(body, stop, CUES_LIMIT)?;
            self.cues = parse_cues(&bytes, self.segment.0);
            self.cues.sort_unstable_by_key(|c| (c.time, c.track));
        }
        if !self.cues.is_empty() {
            return Ok(());
        }
        // No index: the cluster headers are walked once and their timestamps
        // kept. Only headers are read — a cluster's blocks are not touched — so
        // this is a seek per cluster and not a read of the file.
        let mut at = self.first_cluster;
        while at < self.segment.1 {
            let Some((id, body, stop, known)) = self.src.elem_at(at, self.segment.1)? else {
                break;
            };
            if stop <= at {
                break;
            }
            if id == ebml::CLUSTER {
                // Unknown-length clusters cannot be stepped over without
                // reading them, so indexing stops and the seek scans forward
                // from the last cluster it does know.
                if !known {
                    break;
                }
                let head = self.src.body(body, (body + 64).min(stop), FIELD_LIMIT)?;
                let ts = Elements::new(&head)
                    .find(|(id, _)| *id == ebml::CLUSTER_TIMESTAMP)
                    .map(|(_, r)| ebml::uint_of(&head[r]) as i64);
                match ts {
                    Some(ts) => self.clusters.push((ts, at)),
                    None => break,
                }
            }
            at = stop;
        }
        Ok(())
    }

    /// Where to start scanning for `target` on `track`: the last indexed
    /// cluster at or before it, and the first cluster of the file when nothing
    /// is indexed at all.
    fn candidate(&self, track: u64, target: i64) -> u64 {
        // A cue naming this track is worth more than one naming another: only
        // the track's own cues promise a keyframe at that instant.
        let own: Vec<&Cue> = self.cues.iter().filter(|c| c.track == track).collect();
        if !own.is_empty() {
            return match own.iter().rposition(|c| c.time <= target) {
                Some(i) => own[i].cluster,
                None => own[0].cluster,
            };
        }
        if !self.cues.is_empty() {
            return match self.cues.iter().rposition(|c| c.time <= target) {
                Some(i) => self.cues[i].cluster,
                None => self.cues[0].cluster,
            };
        }
        match self.clusters.iter().rposition(|(ts, _)| *ts <= target) {
            Some(i) => self.clusters[i].1,
            None => self.first_cluster,
        }
    }
}

impl<R: Read + Seek + Send> Demuxer for MatroskaDemuxer<R> {
    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    fn next_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(packet) = self.queue.pop_front() {
                return Ok(packet);
            }
            let mut at = 0;
            if !self.load_cluster(&mut at)? {
                return Err(Error::Eof);
            }
        }
    }

    fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<()> {
        let track = self
            .tracks
            .iter()
            .find(|t| t.stream == Some(stream))
            .ok_or_else(|| Error::corrupt(format!("Matroska: no stream {stream}")))?
            .number;
        self.build_index()?;
        let target = to.rescale(self.time_base, Rounding::Down).ticks;

        // The landing point, found by reading forward from the indexed cluster
        // at or before the target: which cluster it is in, and how many packets
        // of that cluster come before it.
        let mut best: Option<(u64, usize)> = None;
        let mut beyond = false;
        self.next_pos = self.candidate(track, target);
        self.queue.clear();
        'scan: loop {
            let mut at = 0;
            self.queue.clear();
            if !self.load_cluster(&mut at)? {
                break;
            }
            for (i, packet) in self.queue.iter().enumerate() {
                if packet.stream != stream {
                    continue;
                }
                let pts = packet.pts.unwrap_or(i64::MIN);
                beyond |= pts > target;
                if !packet.flags.keyframe {
                    continue;
                }
                match mode {
                    SeekMode::SyncAfter => {
                        if pts >= target {
                            best = Some((at, i));
                            break 'scan;
                        }
                    }
                    // A target before the first keyframe of the file lands on
                    // that keyframe: there is nothing earlier to decode from.
                    SeekMode::SyncBefore | SeekMode::Exact => {
                        if pts <= target {
                            best = Some((at, i));
                        } else {
                            if best.is_none() {
                                best = Some((at, i));
                            }
                            break 'scan;
                        }
                    }
                }
            }
            // Past the target with a landing already found: nothing further back
            // can be nearer it.
            if beyond && best.is_some() && mode != SeekMode::SyncAfter {
                break;
            }
        }

        self.queue.clear();
        let Some((cluster, skip)) = best else {
            // Nothing to land on: the reader sits at the end, which is what a
            // seek past the last packet means.
            self.next_pos = self.segment.1;
            return Ok(());
        };
        self.next_pos = cluster;
        let mut at = 0;
        self.load_cluster(&mut at)?;
        self.queue.drain(..skip.min(self.queue.len()));
        Ok(())
    }
}

/// `(SeekID, SeekPosition)` of every entry of one `SeekHead`. Only the one
/// level: a `SeekHead` pointing at another (what a muxer writes when it appends
/// an index later) is not followed, and that file falls back to the walk.
fn seek_positions(buf: &[u8]) -> Vec<(u32, u64)> {
    let mut out = Vec::new();
    for (id, range) in Elements::new(buf) {
        if id != ebml::SEEK {
            continue;
        }
        let start = range.start;
        let (mut want, mut pos) = (None, None);
        for (id, child) in Elements::new(&buf[range]) {
            let child = start + child.start..start + child.end;
            match id {
                // The id is written as the element's own bytes, marker bit and
                // all — the same number this crate's constants are.
                ebml::SEEK_ID => want = u32::try_from(ebml::uint_of(&buf[child])).ok(),
                ebml::SEEK_POSITION => pos = Some(ebml::uint_of(&buf[child])),
                _ => {}
            }
        }
        if let (Some(want), Some(pos)) = (want, pos) {
            out.push((want, pos));
        }
    }
    out
}

/// Every `CuePoint` of a `Cues` element, one per `(time, track)`: a film's
/// picture and its sound are cued at the same instant, each with a position of
/// its own.
fn parse_cues(buf: &[u8], segment: u64) -> Vec<Cue> {
    let mut cues = Vec::new();
    for (id, range) in Elements::new(buf) {
        if id != ebml::CUE_POINT {
            continue;
        }
        let start = range.start;
        let mut time = None;
        for (id, child) in Elements::new(&buf[range]) {
            let child = start + child.start..start + child.end;
            match id {
                ebml::CUE_TIME => time = Some(ebml::uint_of(&buf[child.clone()]) as i64),
                ebml::CUE_TRACK_POSITIONS => {
                    let start = child.start;
                    let (mut track, mut pos) = (None, None);
                    for (id, leaf) in Elements::new(&buf[child]) {
                        let leaf = start + leaf.start..start + leaf.end;
                        match id {
                            ebml::CUE_TRACK => track = Some(ebml::uint_of(&buf[leaf])),
                            ebml::CUE_CLUSTER_POSITION => pos = Some(ebml::uint_of(&buf[leaf])),
                            _ => {}
                        }
                    }
                    if let (Some(time), Some(track), Some(pos)) = (time, track, pos) {
                        cues.push(Cue {
                            time,
                            track,
                            cluster: segment + pos,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    cues
}

/// A `SimpleBlock`/`Block` header and where each of its frames sits in the
/// cluster buffer.
struct BlockHeader {
    track: u64,
    rel: i16,
    flags: u8,
    frames: Vec<Range<usize>>,
}

fn parse_block(buf: &[u8], range: Range<usize>) -> Result<BlockHeader> {
    let body = buf
        .get(range.clone())
        .ok_or_else(|| Error::corrupt("Matroska: block past its cluster"))?;
    let (track, len) = ebml::vint(body, true)?;
    let rest = body
        .get(len..len + 3)
        .ok_or_else(|| Error::corrupt("Matroska: truncated block header"))?;
    let rel = i16::from_be_bytes([rest[0], rest[1]]);
    let flags = rest[2];
    let start = range.start + len + 3;
    let end = range.end;
    let frames = match (flags >> 1) & 0x03 {
        0 => std::iter::once(start..end).collect(),
        lacing => lace(buf, start, end, lacing)?,
    };
    Ok(BlockHeader {
        track,
        rel,
        flags,
        frames,
    })
}

/// The frames of a laced block. Lacing packs several frames — always whole
/// ones, and only ones nothing else references — behind a header of sizes, in
/// one of the three shapes the spec defines. Video muxers write none, but a
/// streaming service's audio is laced by the thousand: every E-AC-3 block of a
/// WEB remux is fixed-laced, and a reader that refuses those is a film that
/// plays silent.
fn lace(buf: &[u8], start: usize, end: usize, lacing: u8) -> Result<Vec<Range<usize>>> {
    let head = buf
        .get(start..end)
        .ok_or_else(|| Error::corrupt("Matroska: lace header past its block"))?;
    let count = usize::from(
        *head
            .first()
            .ok_or_else(|| Error::corrupt("Matroska: a laced block with no frames"))?,
    ) + 1;
    let short = || Error::corrupt("Matroska: a lace header past the end of its block");
    let mut at = 1usize;
    let mut sizes = Vec::with_capacity(count);
    match lacing {
        // Xiph: every size but the last as a run of 255s and a remainder.
        1 => {
            for _ in 1..count {
                let mut size = 0usize;
                loop {
                    let &b = head.get(at).ok_or_else(short)?;
                    at += 1;
                    size += usize::from(b);
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        // Fixed: no sizes written at all, the frames divide the rest evenly.
        2 => {
            let rest = head.len() - at;
            if count == 0 || !rest.is_multiple_of(count) {
                return Err(Error::corrupt(
                    "Matroska: a fixed-lace block that does not divide evenly",
                ));
            }
            sizes = vec![rest / count; count - 1];
        }
        // EBML: the first size outright, the rest as differences from the one
        // before — signed vints, which is the same encoding with the middle of
        // its range taken as zero.
        _ => {
            let mut size = 0i64;
            for i in 1..count {
                let (raw, len) = ebml::vint(head.get(at..).ok_or_else(short)?, true)?;
                at += len;
                size = match i {
                    1 => raw as i64,
                    _ => size + raw as i64 - ((1i64 << (7 * len - 1)) - 1),
                };
                sizes.push(
                    usize::try_from(size)
                        .map_err(|_| Error::corrupt("Matroska: a negative lace size"))?,
                );
            }
        }
    }
    let mut frames = Vec::with_capacity(count);
    let mut off = start + at;
    for size in sizes {
        let stop = off
            .checked_add(size)
            .filter(|&s| s <= end)
            .ok_or_else(|| Error::corrupt("Matroska: lace frames running past their block"))?;
        frames.push(off..stop);
        off = stop;
    }
    // The last frame is whatever is left, which is the only length the fixed and
    // Xiph headers write down for it.
    frames.push(off..end);
    Ok(frames)
}

/// The `ContentEncodings` of one `TrackEntry`. Anything this cannot undo comes
/// back as [`Unpack::Refused`] with the sentence naming the feature it wanted: a
/// compressed track read as if it were plain decodes into garbage, and that is
/// the one thing this must not do quietly.
fn content_encoding(buf: &[u8], range: Range<usize>) -> Unpack {
    let mut found: Option<Unpack> = None;
    let start = range.start;
    for (id, encoding) in Elements::new(&buf[range]) {
        if id != ebml::CONTENT_ENCODING {
            continue;
        }
        let encoding = start + encoding.start..start + encoding.end;
        // Scope 1 — every frame of the track — is the default and the only one
        // written in practice; the others encode the `CodecPrivate` or the next
        // track, and a `CodecPrivate` this cannot read is a track it cannot open
        // either way.
        let (mut scope, mut kind, mut algo, mut settings) = (1, 0, None, Vec::new());
        let start = encoding.start;
        for (id, child) in Elements::new(&buf[encoding]) {
            let child = start + child.start..start + child.end;
            match id {
                ebml::CONTENT_ENCODING_SCOPE => scope = ebml::uint_of(&buf[child]),
                ebml::CONTENT_ENCODING_TYPE => kind = ebml::uint_of(&buf[child]),
                ebml::CONTENT_COMPRESSION => {
                    let start = child.start;
                    for (id, leaf) in Elements::new(&buf[child]) {
                        let leaf = start + leaf.start..start + leaf.end;
                        match id {
                            ebml::CONTENT_COMP_ALGO => algo = Some(ebml::uint_of(&buf[leaf])),
                            ebml::CONTENT_COMP_SETTINGS => settings = buf[leaf].to_vec(),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        let refuse = |what: &str| Unpack::Refused(format!("Matroska {what}"));
        let unpack = match (kind, scope, algo.unwrap_or(0)) {
            (1, _, _) => refuse("encrypted tracks"),
            (_, s, _) if s & 1 == 0 => refuse("tracks whose headers are the compressed part"),
            // 3 is header stripping and 0 is zlib; 1 (bzlib) and 2 (lzo1x) are
            // named rather than guessed at — there is no decompressor for
            // either here, and nothing has written one in twenty years.
            (_, _, 3) => Unpack::Prepend(settings),
            (_, _, 0) => Unpack::Zlib,
            (_, _, 1) => refuse("bzlib-compressed tracks"),
            (_, _, 2) => refuse("lzo1x-compressed tracks"),
            (_, _, algo) => refuse(&format!("tracks compressed with algorithm {algo}")),
        };
        found = Some(match found {
            // Chained encodings — compressed *and* encrypted, say — are legal
            // and written by nothing. Undoing one of the two is worse than
            // saying so.
            Some(_) => refuse("chained content encodings"),
            None => unpack,
        });
    }
    found.unwrap_or_default()
}

/// Which [`CodecId`] a Matroska codec id names. `A_PCM/*` needs the track's own
/// `BitDepth`, which is why the depth comes in.
pub(crate) fn codec_of(id: &str, bit_depth: u32) -> Option<CodecId> {
    Some(match id {
        "V_MPEG4/ISO/AVC" => CodecId::H264,
        "V_MPEGH/ISO/HEVC" => CodecId::H265,
        "V_VP8" => CodecId::Vp8,
        "V_VP9" => CodecId::Vp9,
        "V_AV1" => CodecId::Av1,
        "A_OPUS" => CodecId::Opus,
        "A_VORBIS" => CodecId::Vorbis,
        "A_AC3" => CodecId::Ac3,
        "A_EAC3" => CodecId::EAc3,
        // Named rather than dropped: nothing here decodes them, and a track a
        // player lists as unsupported is a better answer than a silent one.
        "A_TRUEHD" | "A_MLP" => CodecId::TrueHd,
        id if id.starts_with("A_DTS") => CodecId::Dts,
        "A_FLAC" => CodecId::Flac,
        "A_ALAC" => CodecId::Alac,
        "S_TEXT/UTF8" => CodecId::Srt,
        "S_TEXT/WEBVTT" => CodecId::WebVtt,
        "S_TEXT/ASS" | "S_TEXT/SSA" => CodecId::Ass,
        "S_HDMV/PGS" => CodecId::Pgs,
        "A_PCM/FLOAT/IEEE" => CodecId::PcmF32Le,
        "A_PCM/INT/BIG" => CodecId::PcmS16Be,
        "A_PCM/INT/LIT" => match bit_depth {
            8 => CodecId::PcmU8,
            24 => CodecId::PcmS24Le,
            32 => CodecId::PcmS32Le,
            _ => CodecId::PcmS16Le,
        },
        // `A_AAC/MPEG4/LC/SBR` and its family all decode as AAC, and
        // `A_MPEG/L3` is the only MPEG layer this carries.
        id if id.starts_with("A_AAC") => CodecId::Aac,
        "A_MPEG/L3" => CodecId::Mp3,
        _ => return None,
    })
}

/// Every `TrackEntry` of the `Tracks` element, in the order the file lists
/// them: that order *is* the stream numbering, so a careless filter here plays
/// the wrong language.
fn parse_tracks(
    buf: &[u8],
    scale_ns: u64,
    time_base: TimeBase,
    duration: Option<i64>,
) -> (Vec<StreamInfo>, Vec<Track>) {
    let (mut streams, mut tracks) = (Vec::new(), Vec::new());
    for (id, range) in Elements::new(buf) {
        if id != ebml::TRACK_ENTRY {
            continue;
        }
        let start = range.start;
        let (mut number, mut kind, mut codec) = (0u64, 0u64, String::new());
        let (mut language, mut bcp47, mut private) = (String::new(), String::new(), None);
        let (mut default_duration, mut unpack) = (None, Unpack::None);
        let (mut width, mut height, mut color) = (0u32, 0u32, ColorInfo::default());
        let mut display = (0u64, 0u64);
        let mut light = ContentLight::default();
        let (mut rate, mut channels, mut bits) = (0f64, 0u64, 0u64);
        for (id, child) in Elements::new(&buf[range]) {
            let child = start + child.start..start + child.end;
            match id {
                ebml::TRACK_NUMBER => number = ebml::uint_of(&buf[child]),
                ebml::TRACK_TYPE => kind = ebml::uint_of(&buf[child]),
                ebml::CODEC_ID => codec = ebml::string_of(&buf[child]),
                ebml::CODEC_PRIVATE => private = Some(Buf::copy_from_slice(&buf[child])),
                ebml::TRACK_LANGUAGE => language = ebml::string_of(&buf[child]),
                // The spec's own precedence: a modern file states its languages
                // in `LanguageBCP47` and leaves the legacy element out.
                ebml::TRACK_LANGUAGE_BCP47 => bcp47 = ebml::string_of(&buf[child]),
                ebml::DEFAULT_DURATION => {
                    default_duration = Some(ebml::uint_of(&buf[child])).filter(|d| *d > 0)
                }
                ebml::CONTENT_ENCODINGS => unpack = content_encoding(buf, child),
                ebml::VIDEO => {
                    let start = child.start;
                    for (id, leaf) in Elements::new(&buf[child]) {
                        let leaf = start + leaf.start..start + leaf.end;
                        match id {
                            ebml::PIXEL_WIDTH => width = ebml::uint_of(&buf[leaf]) as u32,
                            ebml::PIXEL_HEIGHT => height = ebml::uint_of(&buf[leaf]) as u32,
                            // Anamorphic content states the size it is *shown*
                            // at beside the size it is coded at; the ratio
                            // between the two is the sample aspect.
                            ebml::DISPLAY_WIDTH => display.0 = ebml::uint_of(&buf[leaf]),
                            ebml::DISPLAY_HEIGHT => display.1 = ebml::uint_of(&buf[leaf]),
                            // `Colour`: what the file says its picture's numbers
                            // mean, in the same H.273 code points the bitstream
                            // uses. An element the file leaves out stays
                            // "unspecified" and falls to the tier below.
                            ebml::COLOUR => parse_colour(buf, leaf, &mut color, &mut light),
                            _ => {}
                        }
                    }
                }
                ebml::AUDIO => {
                    let start = child.start;
                    for (id, leaf) in Elements::new(&buf[child]) {
                        let leaf = start + leaf.start..start + leaf.end;
                        match id {
                            ebml::SAMPLING_FREQUENCY => rate = ebml::float_of(&buf[leaf]),
                            ebml::CHANNELS => channels = ebml::uint_of(&buf[leaf]),
                            ebml::BIT_DEPTH => bits = ebml::uint_of(&buf[leaf]),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        let media = match kind {
            1 => Some(MediaType::Video),
            2 => Some(MediaType::Audio),
            0x11 => Some(MediaType::Subtitle),
            _ => None,
        };
        // Ticks, not nanoseconds: everything downstream counts in the file's own
        // time base and nothing here reintroduces a float clock.
        let step = default_duration.map(|ns| (ns / scale_ns.max(1)).max(1) as i64);
        let codec_id = media
            .filter(|_| kind != 0)
            .and_then(|_| codec_of(&codec, bits as u32));
        let stream = match (codec_id, media) {
            (Some(codec_id), Some(media)) => {
                let mut params = CodecParameters::new(codec_id);
                params.extradata = private;
                params.media = match media {
                    MediaType::Video => MediaParameters::Video(VideoParameters {
                        width,
                        height,
                        format: None,
                        // `DefaultDuration` is nanoseconds a frame lasts, so
                        // the rate is its inverse — the only exact statement of
                        // the frame rate a Matroska file makes.
                        frame_rate: default_duration
                            .and_then(|ns| TimeBase::try_new(1_000_000_000, ns as i64).ok()),
                        sample_aspect_ratio: match display {
                            (dw, dh) if dw > 0 && dh > 0 && width > 0 && height > 0 => {
                                TimeBase::try_new(
                                    dw as i64 * i64::from(height),
                                    dh as i64 * i64::from(width),
                                )
                                .ok()
                                .filter(|sar| sar.num() != sar.den())
                            }
                            _ => None,
                        },
                        color,
                        light,
                    }),
                    MediaType::Audio => MediaParameters::Audio(AudioParameters {
                        sample_rate: if rate > 0.0 {
                            rate.round() as u32
                        } else {
                            8_000
                        },
                        layout: ChannelLayout::from_count(channels.max(1) as usize),
                        format: None,
                        bits_per_sample: (bits > 0).then_some(bits as u32),
                    }),
                    MediaType::Subtitle => MediaParameters::Subtitle,
                };
                let index = streams.len() as u32;
                let mut info = StreamInfo::new(index, time_base, params);
                info.duration = duration;
                info.start_time = Some(0);
                info.language = Some(language_of(&language, &bcp47));
                streams.push(info);
                Some(index)
            }
            _ => None,
        };
        tracks.push(Track {
            number,
            stream,
            unpack,
            default_duration: step,
        });
    }
    (streams, tracks)
}

fn parse_colour(buf: &[u8], range: Range<usize>, color: &mut ColorInfo, light: &mut ContentLight) {
    let start = range.start;
    for (id, child) in Elements::new(&buf[range]) {
        let child = start + child.start..start + child.end;
        match id {
            ebml::MATRIX_COEFFICIENTS => color.matrix = ebml::uint_of(&buf[child]) as u8,
            ebml::TRANSFER_CHARACTERISTICS => color.transfer = ebml::uint_of(&buf[child]) as u8,
            ebml::PRIMARIES => color.primaries = ebml::uint_of(&buf[child]) as u8,
            // Matroska says the range as a code, not a flag: 1 limited, 2 full.
            ebml::RANGE => color.full_range = ebml::uint_of(&buf[child]) == 2,
            ebml::MAX_CLL => light.max_cll = nits(ebml::uint_of(&buf[child]) as f64),
            ebml::MAX_FALL => light.max_fall = nits(ebml::uint_of(&buf[child]) as f64),
            ebml::MASTERING_METADATA => {
                let start = child.start;
                for (id, leaf) in Elements::new(&buf[child]) {
                    let leaf = start + leaf.start..start + leaf.end;
                    match id {
                        ebml::LUMINANCE_MAX => {
                            light.mastering_max = nits(ebml::float_of(&buf[leaf]))
                        }
                        ebml::LUMINANCE_MIN => {
                            light.mastering_min = nits(ebml::float_of(&buf[leaf]))
                        }
                        // The eight chromaticities: the display's gamut, which
                        // nothing downstream acts on. Walked past, never refused.
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// A brightness a container stated, or [`None`] when it stated zero — which is
/// what a muxer writes for "unknown" and what a tone map must not read as a film
/// that peaks at black.
fn nits(value: f64) -> Option<f32> {
    (value > 0.0).then_some(value as f32)
}

/// What language a `TrackEntry` states, as the three-letter ISO 639-2 code.
///
/// **`LanguageBCP47` wins**, which is the spec's own precedence and not a
/// preference: a modern file states its languages there and leaves the legacy
/// element out, so reading only the old one loses them. A tag is cut to its
/// primary subtag and mapped, so `en` and `en-US` are both `eng`: the region is
/// not the language. Three letters already (`fil`, every ISO 639-3 tag) are kept
/// as they are, and anything else falls back to the legacy element rather than
/// throwing the file's word away. `und` for a track that states neither: the
/// spec's default is `eng`, but writing English into a track whose file never
/// said so is this reader's claim and not the file's.
fn language_of(legacy: &str, bcp47: &str) -> String {
    let primary = bcp47
        .split('-')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mapped = match primary.len() {
        2 => ISO_639_1_TO_2
            .iter()
            .find(|(short, _)| *short == primary)
            .map(|(_, long)| (*long).to_string()),
        3 => Some(primary),
        _ => None,
    };
    match mapped {
        Some(code) => code,
        None if !legacy.is_empty() => legacy.to_string(),
        None => "und".to_string(),
    }
}

/// The ISO 639-1 codes a `LanguageBCP47` tag is written with, against the 639-2
/// codes everything else here speaks. The languages a media file is subtitled
/// or dubbed in; a tag outside this list falls back to the legacy element.
const ISO_639_1_TO_2: &[(&str, &str)] = &[
    ("aa", "aar"),
    ("ab", "abk"),
    ("af", "afr"),
    ("am", "amh"),
    ("ar", "ara"),
    ("as", "asm"),
    ("az", "aze"),
    ("ba", "bak"),
    ("be", "bel"),
    ("bg", "bul"),
    ("bn", "ben"),
    ("bo", "bod"),
    ("br", "bre"),
    ("bs", "bos"),
    ("ca", "cat"),
    ("cs", "ces"),
    ("cy", "cym"),
    ("da", "dan"),
    ("de", "deu"),
    ("el", "ell"),
    ("en", "eng"),
    ("eo", "epo"),
    ("es", "spa"),
    ("et", "est"),
    ("eu", "eus"),
    ("fa", "fas"),
    ("fi", "fin"),
    ("fo", "fao"),
    ("fr", "fra"),
    ("ga", "gle"),
    ("gl", "glg"),
    ("gu", "guj"),
    ("he", "heb"),
    ("hi", "hin"),
    ("hr", "hrv"),
    ("ht", "hat"),
    ("hu", "hun"),
    ("hy", "hye"),
    ("id", "ind"),
    ("is", "isl"),
    ("it", "ita"),
    ("ja", "jpn"),
    ("ka", "kat"),
    ("kk", "kaz"),
    ("km", "khm"),
    ("kn", "kan"),
    ("ko", "kor"),
    ("ku", "kur"),
    ("ky", "kir"),
    ("la", "lat"),
    ("lb", "ltz"),
    ("lo", "lao"),
    ("lt", "lit"),
    ("lv", "lav"),
    ("mk", "mkd"),
    ("ml", "mal"),
    ("mn", "mon"),
    ("mr", "mar"),
    ("ms", "msa"),
    ("mt", "mlt"),
    ("my", "mya"),
    ("ne", "nep"),
    ("nl", "nld"),
    ("no", "nor"),
    ("pa", "pan"),
    ("pl", "pol"),
    ("ps", "pus"),
    ("pt", "por"),
    ("ro", "ron"),
    ("ru", "rus"),
    ("sa", "san"),
    ("si", "sin"),
    ("sk", "slk"),
    ("sl", "slv"),
    ("sq", "sqi"),
    ("sr", "srp"),
    ("sv", "swe"),
    ("sw", "swa"),
    ("ta", "tam"),
    ("te", "tel"),
    ("tg", "tgk"),
    ("th", "tha"),
    ("tk", "tuk"),
    ("tl", "tgl"),
    ("tr", "tur"),
    ("tt", "tat"),
    ("uk", "ukr"),
    ("ur", "urd"),
    ("uz", "uzb"),
    ("vi", "vie"),
    ("yi", "yid"),
    ("zh", "zho"),
    ("zu", "zul"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ebml::{elem, put_id, put_size, uint};
    use std::io::Cursor;

    /// zlib around bytes that are stored, not compressed: a deflate block of
    /// type 00 is a length and the bytes themselves, so a test needs no
    /// compressor to exercise the decompressor.
    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01, 0x01];
        out.extend_from_slice(&(data.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(data.len() as u16)).to_le_bytes());
        out.extend_from_slice(data);
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in data {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }

    fn track(number: u64, kind: u64, codec: &str, encoding: Option<&[u8]>) -> Vec<u8> {
        let mut entry = Vec::new();
        uint(&mut entry, ebml::TRACK_NUMBER, number);
        uint(&mut entry, ebml::TRACK_TYPE, kind);
        elem(&mut entry, ebml::CODEC_ID, codec.as_bytes());
        // One millisecond a frame, so a lace's frames step by a tick each.
        uint(&mut entry, ebml::DEFAULT_DURATION, 1_000_000);
        if let Some(encoding) = encoding {
            elem(&mut entry, ebml::CONTENT_ENCODINGS, encoding);
        }
        entry
    }

    fn compression(algo: u64, settings: &[u8]) -> Vec<u8> {
        let mut compression = Vec::new();
        uint(&mut compression, ebml::CONTENT_COMP_ALGO, algo);
        if !settings.is_empty() {
            elem(&mut compression, ebml::CONTENT_COMP_SETTINGS, settings);
        }
        let mut encoding = Vec::new();
        uint(&mut encoding, ebml::CONTENT_ENCODING_SCOPE, 1);
        uint(&mut encoding, ebml::CONTENT_ENCODING_TYPE, 0);
        elem(&mut encoding, ebml::CONTENT_COMPRESSION, &compression);
        let mut encodings = Vec::new();
        elem(&mut encodings, ebml::CONTENT_ENCODING, &encoding);
        encodings
    }

    fn simple_block(track: u8, rel: i16, flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut block = vec![0x80 | track];
        block.extend_from_slice(&rel.to_be_bytes());
        block.push(flags);
        block.extend_from_slice(payload);
        let mut out = Vec::new();
        elem(&mut out, ebml::SIMPLE_BLOCK, &block);
        out
    }

    /// A file with the three things nothing generated by ffmpeg carries: a
    /// zlib track, a header-stripped track, and one block of each lacing.
    fn synthetic() -> Vec<u8> {
        let mut header = Vec::new();
        elem(&mut header, ebml::DOC_TYPE, b"matroska");
        let mut out = Vec::new();
        elem(&mut out, ebml::EBML_HEADER, &header);

        let mut info = Vec::new();
        uint(&mut info, ebml::TIMESTAMP_SCALE, 1_000_000);
        let mut tracks = Vec::new();
        elem(
            &mut tracks,
            ebml::TRACK_ENTRY,
            &track(1, 0x11, "S_TEXT/UTF8", Some(&compression(0, &[]))),
        );
        elem(
            &mut tracks,
            ebml::TRACK_ENTRY,
            &track(2, 1, "V_VP9", Some(&compression(3, &[0xAA]))),
        );
        elem(&mut tracks, ebml::TRACK_ENTRY, &track(3, 2, "A_OPUS", None));

        let mut cluster = Vec::new();
        uint(&mut cluster, ebml::CLUSTER_TIMESTAMP, 10);
        cluster.extend(simple_block(1, 0, 0x80, &zlib(b"hello")));
        cluster.extend(simple_block(2, 1, 0x80, &[1, 2, 3]));
        // Xiph: sizes as runs of 255 and a remainder, last frame implied.
        cluster.extend(simple_block(3, 2, 0x82, &[2, 1, 1, b'a', b'b', b'c']));
        // EBML: the first size outright, then differences.
        // (0xBF is a signed vint zero: the middle of the one-byte range.)
        cluster.extend(simple_block(3, 5, 0x86, &[2, 0x81, 0xBF, b'd', b'e', b'f']));
        // Fixed: no sizes at all, the rest divides evenly.
        cluster.extend(simple_block(3, 8, 0x84, &[2, b'g', b'h', b'i']));

        let mut segment = Vec::new();
        elem(&mut segment, ebml::INFO, &info);
        elem(&mut segment, ebml::TRACKS, &tracks);
        elem(&mut segment, ebml::CLUSTER, &cluster);
        // A second cluster, so a walk that loses the first still finds one.
        let mut second = Vec::new();
        uint(&mut second, ebml::CLUSTER_TIMESTAMP, 100);
        second.extend(simple_block(2, 0, 0x80, &[4, 5, 6]));
        elem(&mut segment, ebml::CLUSTER, &second);

        put_id(&mut out, ebml::SEGMENT);
        put_size(&mut out, segment.len() as u64);
        out.extend_from_slice(&segment);
        out
    }

    fn packets(bytes: Vec<u8>) -> Vec<Packet> {
        let mut demuxer = MatroskaDemuxer::new(Cursor::new(bytes)).expect("demuxes");
        let mut out = Vec::new();
        loop {
            match demuxer.next_packet() {
                Ok(packet) => out.push(packet),
                Err(Error::Eof) => return out,
                Err(e) => panic!("{e}"),
            }
        }
    }

    #[test]
    fn content_encodings_and_every_lacing_come_back_as_the_encoder_wrote_them() {
        let packets = packets(synthetic());
        // zlib undone, header stripping put back, three frames per lace.
        assert_eq!(&*packets[0].data, b"hello");
        assert_eq!(&*packets[1].data, &[0xAA, 1, 2, 3]);
        let laced: Vec<&[u8]> = packets[2..11].iter().map(|p| &*p.data).collect();
        assert_eq!(
            laced,
            [b"a", b"b", b"c", b"d", b"e", b"f", b"g", b"h", b"i"].map(|f| f.as_slice())
        );
        // One timestamp is written for a whole lace; its frames step by the
        // track's own `DefaultDuration` from there.
        assert_eq!(
            packets[2..5]
                .iter()
                .map(|p| p.pts.unwrap())
                .collect::<Vec<_>>(),
            [12, 13, 14]
        );
        assert_eq!(packets.last().unwrap().pts, Some(100));
        assert_eq!(packets.len(), 12);
    }

    #[test]
    fn a_damaged_file_plays_what_is_left_of_it() {
        // A truncated download: the prefix demuxes and the end is an end, not a
        // panic.
        let whole = synthetic();
        let cut = whole[..whole.len() - 20].to_vec();
        assert!(!packets(cut).is_empty());

        // ...and a file whose element chain breaks resynchronises on the next
        // `Cluster` id rather than losing everything after the damage.
        let mut damaged = whole.clone();
        let cluster = damaged
            .windows(4)
            .position(|w| w == ebml::CLUSTER_MAGIC)
            .expect("a cluster to damage");
        damaged[cluster] = 0x00;
        let after = packets(damaged);
        assert_eq!(after.last().map(|p| p.pts), Some(Some(100)));
    }

    #[test]
    fn a_cluster_whose_length_was_never_written_still_reads() {
        // What a muxer writes while it is still recording: the size is the
        // all-ones vint EBML spells *unknown* with, and the cluster ends where
        // its children do.
        let whole = synthetic();
        let at = whole
            .windows(4)
            .position(|w| w == ebml::CLUSTER_MAGIC)
            .expect("a cluster");
        let mut live = whole.clone();
        // The size byte after a 4-byte id: one byte of 0xFF is "unknown".
        assert_eq!(live[at + 4] & 0x80, 0x80, "a one-byte cluster size");
        live[at + 4] = 0xFF;
        // ...which leaves the byte the size used to be as a stray element, so
        // the payload is one byte further on; the walk skips what it cannot
        // name and reads the blocks either way.
        let live = packets(live);
        assert!(
            live.iter().any(|p| p.pts == Some(100)),
            "the second cluster is still reached"
        );
    }

    #[test]
    fn language_prefers_the_modern_element() {
        assert_eq!(language_of("und", "en-US"), "eng");
        assert_eq!(language_of("", "tr"), "tur");
        // A tag nothing maps keeps what the legacy element said.
        assert_eq!(language_of("jpn", "x-private"), "jpn");
        assert_eq!(language_of("", ""), "und");
        assert_eq!(language_of("", "fil"), "fil");
    }

    #[test]
    fn codec_ids_cover_the_families_the_family_carries() {
        assert_eq!(codec_of("V_MPEGH/ISO/HEVC", 0), Some(CodecId::H265));
        assert_eq!(codec_of("A_AAC/MPEG4/LC/SBR", 0), Some(CodecId::Aac));
        assert_eq!(codec_of("A_PCM/INT/LIT", 24), Some(CodecId::PcmS24Le));
        assert_eq!(codec_of("A_PCM/INT/LIT", 16), Some(CodecId::PcmS16Le));
        assert_eq!(codec_of("S_HDMV/PGS", 0), Some(CodecId::Pgs));
        assert_eq!(codec_of("V_MS/VFW/FOURCC", 0), None);
    }

    #[test]
    fn unpack_undoes_what_a_track_declared() {
        let buf = Buf::from_vec(vec![1, 2, 3, 4]);
        assert_eq!(&*Unpack::None.frame(&buf, 1..3).unwrap(), &[2, 3]);
        assert_eq!(
            &*Unpack::Prepend(vec![0]).frame(&buf, 0..2).unwrap(),
            &[0, 1, 2]
        );
        assert!(
            Unpack::Refused("encrypted".into())
                .frame(&buf, 0..1)
                .is_err()
        );
        // A block that is not zlib at all is an error, never a panic.
        assert!(Unpack::Zlib.frame(&buf, 0..4).is_err());
    }
}
