//! Reading: pages in, packets out.
//!
//! Three things make an Ogg demuxer more than a loop over pages. Packets are
//! *not* page-sized — one packet can span pages and one page can hold dozens —
//! so segments are reassembled across pages. Timing comes from the page, not
//! the packet: a granule position states where the last packet finishing on
//! that page ends, so packet timestamps are carried forward from each page
//! boundary and re-synchronised at the next one (exactly, for a mapping that
//! states per-packet durations; page by page for one that does not — see
//! [`Mapping::packet_duration`]). And a page whose checksum fails is not fatal:
//! the reader drops it, discards whatever packet it was assembling and scans
//! on for the next capture pattern, so damage costs the packets inside the
//! damaged page and nothing after it.

use std::collections::VecDeque;
use std::io::{BufReader, Read, Seek, SeekFrom};

use ec_core::{
    Buf, CodecParameters, Demuxer, Error, MediaParameters, Packet, Result, SeekMode, StreamInfo,
    Timestamp,
};

use crate::mapping::Mapping;
use crate::page::{self, PageHeader};
use crate::{granule_side_data, xiph_lace};

/// One page, read and verified.
struct RawPage {
    header: PageHeader,
    body: Vec<u8>,
    /// Byte offset of the capture pattern.
    offset: u64,
}

/// What one attempt at reading a page produced.
enum Step {
    /// A page whose checksum verified.
    Page(RawPage),
    /// A capture pattern whose page did not verify; scanning resumes just past
    /// it.
    Damaged,
    /// No further page in the file.
    End,
}

/// Per logical stream state, in discovery order.
struct Track {
    serial: u32,
    mapping: Mapping,
    /// Header packets, in order, until [`Mapping::header_packets`] are in.
    headers: Vec<Vec<u8>>,
    /// Bytes of a packet still being assembled across a page boundary.
    partial: Vec<u8>,
    /// A packet is mid-assembly (true even when its bytes so far are empty,
    /// which happens when a page ends exactly on a 255-byte segment).
    assembling: bool,
    /// Granule position the next packet starts at.
    pos: i64,
    /// False between a packet of unknown duration and the next page granule.
    pos_known: bool,
    /// The stream has seen its end-of-stream page.
    eos: bool,
}

impl Track {
    fn headers_complete(&self) -> bool {
        self.headers.len() >= self.mapping.header_packets()
    }
}

/// An Ogg reader over anything seekable.
///
/// [`OggDemuxer::open`] consumes the header packets of every logical stream, so
/// [`Demuxer::streams`] is complete before the first [`Demuxer::next_packet`],
/// and header packets are never handed out as packets — they live in
/// [`CodecParameters::extradata`], Xiph-laced for Vorbis, the raw `OpusHead`
/// for Opus.
pub struct OggDemuxer<R: Read + Seek> {
    inner: BufReader<R>,
    streams: Vec<StreamInfo>,
    tracks: Vec<Track>,
    queue: VecDeque<Packet>,
    /// Offset of the first page past the last header page.
    data_start: u64,
    /// Pages dropped for a failed checksum.
    damaged: u64,
}

impl<R: Read + Seek> OggDemuxer<R> {
    /// Read every stream's headers and stop at the first audio page.
    pub fn open(inner: R) -> Result<OggDemuxer<R>> {
        let mut demuxer = OggDemuxer {
            inner: BufReader::new(inner),
            streams: Vec::new(),
            tracks: Vec::new(),
            queue: VecDeque::new(),
            data_start: 0,
            damaged: 0,
        };
        demuxer.read_headers()?;
        demuxer.measure_durations()?;
        Ok(demuxer)
    }

    /// How many pages were dropped because their checksum did not verify.
    pub fn damaged_pages(&self) -> u64 {
        self.damaged
    }

    /// The mapping of stream `index`, for callers that need the pre-skip or the
    /// granule clock behind [`StreamInfo::time_base`].
    pub fn mapping(&self, index: usize) -> Option<Mapping> {
        self.tracks.get(index).map(|t| t.mapping)
    }

    /// Read pages until every discovered stream has all of its header packets.
    fn read_headers(&mut self) -> Result<()> {
        loop {
            if !self.tracks.is_empty() && self.tracks.iter().all(Track::headers_complete) {
                self.data_start = self.inner.stream_position()?;
                return Ok(());
            }
            match read_page(&mut self.inner)? {
                Step::Page(page) => self.consume(page)?,
                Step::Damaged => self.damaged += 1,
                Step::End => match self.tracks.is_empty() {
                    true => {
                        return Err(Error::corrupt(
                            "Ogg: no logical stream with a known mapping",
                        ));
                    }
                    // Headers that never completed: whatever streams were found
                    // are described as far as their headers went.
                    false => {
                        self.data_start = self.inner.stream_position()?;
                        return Ok(());
                    }
                },
            }
        }
    }

    /// Walk to the end once to learn each stream's last granule position, then
    /// come back. Cheap (a bounded tail scan) and it is the only way an Ogg
    /// file states its duration at all.
    fn measure_durations(&mut self) -> Result<()> {
        let resume = self.inner.stream_position()?;
        let end = self.inner.seek(SeekFrom::End(0))?;
        // Tail window, widened until a page for every stream turns up or the
        // whole file has been walked.
        let mut window = 64 * 1024;
        let mut last: Vec<Option<i64>> = vec![None; self.tracks.len()];
        loop {
            let from = end.saturating_sub(window);
            self.inner.seek(SeekFrom::Start(from))?;
            loop {
                match read_page(&mut self.inner)? {
                    Step::Page(page) => {
                        if let Some(i) = self.track_index(page.header.serial)
                            && page.header.granule != page::NO_GRANULE
                        {
                            last[i] = Some(page.header.granule);
                        }
                    }
                    Step::Damaged => continue,
                    Step::End => break,
                }
            }
            if last.iter().all(Option::is_some) || from == 0 {
                break;
            }
            window *= 8;
        }
        for (i, granule) in last.into_iter().enumerate() {
            if let Some(granule) = granule {
                let start = self.streams[i].start_time.unwrap_or(0);
                self.streams[i].duration = Some((granule - start).max(0));
            }
        }
        self.inner.seek(SeekFrom::Start(resume))?;
        Ok(())
    }

    fn track_index(&self, serial: u32) -> Option<usize> {
        self.tracks.iter().position(|t| t.serial == serial)
    }

    /// Turn one page into packets: header packets into the stream description,
    /// audio packets into the queue.
    fn consume(&mut self, page: RawPage) -> Result<()> {
        let serial = page.header.serial;
        let index = match self.track_index(serial) {
            Some(i) => i,
            None => match page.header.bos {
                true => match self.start_track(&page) {
                    Some(i) => i,
                    // A mapping nothing in this family claims (Theora, Skeleton,
                    // a private stream): its pages are skipped, not an error —
                    // the audio streams beside it still read.
                    None => return Ok(()),
                },
                false => return Ok(()),
            },
        };

        let mapping = self.tracks[index].mapping;
        let time_base = self.streams[index].time_base;
        let mut packet = std::mem::take(&mut self.tracks[index].partial);
        let mut assembling = self.tracks[index].assembling;
        let mut pos = self.tracks[index].pos;
        let mut pos_known = self.tracks[index].pos_known;
        let mut headers_done = self.tracks[index].headers_complete();
        let mut at = 0usize;
        let mut segments = &page.header.segments[..];

        match (page.header.continued, assembling) {
            // The head of this packet was in a page we never saw: drop segments
            // up to and including the first terminator.
            (true, false) => {
                let mut skipped = 0;
                for (i, &lace) in segments.iter().enumerate() {
                    at += usize::from(lace);
                    skipped = i + 1;
                    if lace < 255 {
                        break;
                    }
                }
                segments = &segments[skipped..];
            }
            // A packet was mid-assembly and this page does not continue it: the
            // partial is orphaned.
            (false, true) => {
                packet.clear();
                assembling = false;
            }
            _ => {}
        }

        let first_new = self.queue.len();
        for &lace in segments {
            let end = at + usize::from(lace);
            let Some(slice) = page.body.get(at..end) else {
                // The segment table promised more body than the page carried.
                return Err(Error::corrupt("Ogg page: segment table overruns the body"));
            };
            packet.extend_from_slice(slice);
            at = end;
            if lace == 255 {
                assembling = true;
                continue;
            }
            assembling = false;
            let done = std::mem::take(&mut packet);
            match headers_done {
                false => {
                    self.tracks[index].headers.push(done);
                    if self.tracks[index].headers_complete() {
                        headers_done = true;
                        self.finish_stream(index)?;
                    }
                }
                true => {
                    // The zero-length segment a muxer uses to carry an
                    // end-of-stream flag on an otherwise empty page is not a
                    // packet; handing it out would inflate every packet count.
                    if done.is_empty() {
                        continue;
                    }
                    let duration = mapping.packet_duration(&done);
                    let mut out = Packet::new(index as u32, time_base, done);
                    out.pts = pos_known.then_some(pos);
                    out.dts = out.pts;
                    out.duration = duration;
                    // Every Ogg audio packet is a decodable entry point as far
                    // as the container is concerned; codec pre-roll (a Vorbis
                    // block needs its predecessor to overlap) is the decoder's.
                    out.flags.keyframe = true;
                    match duration {
                        Some(d) if pos_known => pos += d,
                        Some(_) => {}
                        None => pos_known = false,
                    }
                    self.queue.push_back(out);
                }
            }
        }
        let track = &mut self.tracks[index];
        track.partial = packet;
        track.assembling = assembling;
        track.pos = pos;
        track.pos_known = pos_known;
        track.eos |= page.header.eos;

        // The granule is where the last packet finishing on this page ends, so
        // it is also where the next one starts — the point every mapping's
        // timing is re-anchored to.
        if page.header.granule != page::NO_GRANULE {
            if self.queue.len() > first_new {
                let last = self.queue.len() - 1;
                self.queue[last]
                    .side_data
                    .push(granule_side_data(page.header.granule));
            }
            let track = &mut self.tracks[index];
            track.pos = page.header.granule;
            track.pos_known = true;
        }
        Ok(())
    }

    /// A beginning-of-stream page: identify the mapping from its first packet.
    fn start_track(&mut self, page: &RawPage) -> Option<usize> {
        let len = usize::from(*page.header.segments.first()?);
        let first = page.body.get(..len)?;
        let mapping = Mapping::identify(first)?;
        let mut params = CodecParameters::new(mapping.codec());
        if let MediaParameters::Audio(audio) = &mut params.media {
            audio.sample_rate = mapping.sample_rate();
            audio.layout = mapping.layout();
        }
        let index = self.tracks.len();
        let mut info = StreamInfo::new(index as u32, mapping.time_base(), params);
        info.start_time = Some(match mapping {
            // Opus granules include the samples a decoder throws away, so
            // presentation starts at the pre-skip rather than at zero.
            Mapping::Opus { pre_skip, .. } => i64::from(pre_skip),
            _ => 0,
        });
        self.streams.push(info);
        self.tracks.push(Track {
            serial: page.header.serial,
            mapping,
            headers: Vec::new(),
            partial: Vec::new(),
            assembling: false,
            pos: 0,
            pos_known: true,
            eos: false,
        });
        Some(index)
    }

    /// All header packets are in: publish them as `extradata`.
    fn finish_stream(&mut self, index: usize) -> Result<()> {
        let track = &self.tracks[index];
        let extradata = match track.mapping {
            // The three-packet triplet, laced the way every consumer of Vorbis
            // extradata outside Ogg expects it.
            Mapping::Vorbis { .. } => {
                let refs: Vec<&[u8]> = track.headers.iter().map(Vec::as_slice).collect();
                xiph_lace(&refs)
            }
            // RFC 7845 identification header, verbatim; the tags packet is
            // metadata, not decoder setup.
            Mapping::Opus { .. } | Mapping::Flac { .. } => Some(track.headers[0].clone()),
        };
        self.streams[index].params.extradata = extradata.map(Buf::from_vec);
        Ok(())
    }

    /// Scan forward from `offset` for the next page of `serial`.
    fn page_at_or_after(&mut self, offset: u64, serial: u32) -> Result<Option<(u64, i64)>> {
        self.inner.seek(SeekFrom::Start(offset))?;
        loop {
            match read_page(&mut self.inner)? {
                Step::Page(page) if page.header.serial == serial => {
                    return Ok(Some((page.offset, page.header.granule)));
                }
                Step::Page(_) | Step::Damaged => continue,
                Step::End => return Ok(None),
            }
        }
    }

    /// Drop assembly state and queued packets after a jump.
    fn reset_state(&mut self) {
        self.queue.clear();
        for track in &mut self.tracks {
            track.partial.clear();
            track.assembling = false;
            track.pos_known = false;
            track.eos = false;
        }
    }
}

impl<R: Read + Seek + Send> Demuxer for OggDemuxer<R> {
    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    fn next_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(packet) = self.queue.pop_front() {
                return Ok(packet);
            }
            match read_page(&mut self.inner)? {
                Step::Page(page) => self.consume(page)?,
                Step::Damaged => self.damaged += 1,
                Step::End => return Err(Error::Eof),
            }
        }
    }

    /// Land on a page boundary at or before `to` by bisecting the file on
    /// granule positions — the only index Ogg has.
    ///
    /// [`SeekMode::Exact`] is served as [`SeekMode::SyncBefore`]: pages are the
    /// finest position the container offers, and a decoder that needs the
    /// sample exactly discards forward from there.
    fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<()> {
        let index = usize::try_from(stream).unwrap_or(usize::MAX);
        let info = self
            .streams
            .get(index)
            .ok_or_else(|| Error::corrupt(format!("Ogg seek: no stream {stream}")))?;
        let serial = self.tracks[index].serial;
        let target = to.rescale(info.time_base, ec_core::Rounding::Down).ticks;
        let first_stream_page = self
            .page_at_or_after(self.data_start, serial)?
            .map(|(offset, _)| offset);

        let end = self.inner.seek(SeekFrom::End(0))?;
        let mut lo = self.data_start;
        let mut hi = end;
        // Last page start whose granule is at or before the target, and the
        // first one at or after it — one bisection answers both.
        let mut before: Option<(u64, i64)> = None;
        let mut after: Option<(u64, i64)> = None;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.page_at_or_after(mid, serial)? {
                Some((offset, granule)) if granule != page::NO_GRANULE && granule <= target => {
                    before = Some((offset, granule));
                    // Always make progress: the found page may start before mid.
                    lo = offset.max(mid) + 1;
                }
                Some((offset, granule)) if granule != page::NO_GRANULE => {
                    after = Some((offset, granule));
                    hi = match offset > mid {
                        true => mid,
                        false => offset,
                    };
                }
                // A page with no granule says nothing about position; step past
                // it rather than splitting the same interval forever.
                Some((offset, _)) => lo = offset.max(mid) + 1,
                None => hi = mid,
            }
        }
        let found = match mode {
            SeekMode::SyncAfter => after.or(before),
            // No page ends at or before the target: it is inside the first one,
            // and the stream's beginning is what "at or before" means there.
            _ => before,
        };
        self.reset_state();
        let granule = match found {
            // The first data page is the stream head. Replaying it must behave
            // exactly like a fresh open: its packet carries the head trim
            // granule, and the packet's timestamp still starts at zero.
            Some((offset, _)) if Some(offset) == first_stream_page => {
                self.inner.seek(SeekFrom::Start(offset))?;
                0
            }
            // A granule is where a page *ends*, and a Vorbis or FLAC packet
            // states no duration of its own — so reading from the page the
            // bisection found would hand out its packets with no timestamp at
            // all, and a seek could not say where it landed. The landing is the
            // page *after* it, whose first sample is that page's granule.
            Some((offset, granule)) => {
                self.inner.seek(SeekFrom::Start(offset))?;
                read_page(&mut self.inner)?;
                granule
            }
            // Nothing indexed the target: the stream's own beginning is the
            // only honest answer, and every packet of it is still to come.
            None => {
                self.inner.seek(SeekFrom::Start(self.data_start))?;
                0
            }
        };
        // Never resume inside the headers: they are not audio and must not be
        // handed out as packets.
        if self.inner.stream_position()? < self.data_start {
            self.inner.seek(SeekFrom::Start(self.data_start))?;
        }
        let track = &mut self.tracks[index];
        track.pos = granule;
        track.pos_known = true;
        Ok(())
    }
}

/// Read one page: scan for the capture pattern, verify the checksum, hand back
/// header and body.
fn read_page<R: Read + Seek>(inner: &mut BufReader<R>) -> Result<Step> {
    let mut fixed = [0u8; page::HEADER_LEN];
    let mut window = 0usize;
    // Scan for "OggS". The pattern has no proper prefix that is also a suffix,
    // so a failed match can never hide the start of a real one.
    while window < 4 {
        let mut byte = [0u8; 1];
        match inner.read(&mut byte) {
            Ok(0) => return Ok(Step::End),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
        window = match byte[0] == page::CAPTURE[window] {
            true => window + 1,
            false => usize::from(byte[0] == page::CAPTURE[0]),
        };
    }
    let offset = inner.stream_position()? - 4;
    fixed[..4].copy_from_slice(&page::CAPTURE);
    if read_exact_or_end(inner, &mut fixed[4..])? {
        return Ok(Step::End);
    }

    let Ok((mut header, nsegs)) = PageHeader::parse_fixed(&fixed) else {
        inner.seek(SeekFrom::Start(offset + 4))?;
        return Ok(Step::Damaged);
    };
    let mut segments = vec![0u8; nsegs];
    if read_exact_or_end(inner, &mut segments)? {
        return Ok(Step::End);
    }
    header.segments = segments;
    let mut body = vec![0u8; header.body_len()];
    if read_exact_or_end(inner, &mut body)? {
        return Ok(Step::End);
    }

    let stored = PageHeader::stored_crc(&fixed);
    let mut zeroed = fixed;
    zeroed[22..26].fill(0);
    if crate::crc::crc32(&[&zeroed, &header.segments, &body]) != stored {
        // Damage is local: resume scanning right after this capture pattern so
        // the very next intact page is found.
        inner.seek(SeekFrom::Start(offset + 4))?;
        return Ok(Step::Damaged);
    }
    Ok(Step::Page(RawPage {
        header,
        body,
        offset,
    }))
}

/// `read_exact`, with a truncated file reported as end rather than as an error:
/// a half-written page at EOF is the normal shape of an interrupted recording.
fn read_exact_or_end<R: Read>(inner: &mut BufReader<R>, buf: &mut [u8]) -> Result<bool> {
    match inner.read_exact(buf) {
        Ok(()) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(true),
        Err(e) => Err(e.into()),
    }
}
