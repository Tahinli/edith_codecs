//! Writing: packets in, pages out.
//!
//! Two rules shape every page this writes. A granule position states where the
//! *last packet finishing on that page* ends, so a page is only ever closed
//! after a packet whose position is known — [`crate::granule_of`] on the packet,
//! or `pts + duration` when both are stated. And the header packets of every
//! mapping in this family get pages of their own: the identification packet
//! alone on the beginning-of-stream page, the rest flushed before the first
//! audio byte, which is what Vorbis I and RFC 7845 §3 both require.
//!
//! Page size is targeted, not fixed: packets are packed until the body passes
//! [`OggMuxer::set_page_target_bytes`] (4 KiB by default, the size RFC 3533
//! calls usual) and the next packet boundary with a known granule arrives.
//! Packets larger than a page are split across pages with the continuation flag,
//! and a page never carries more than 255 segments.

use std::io::Write;

use ec_core::{Error, Muxer, Packet, Result, StreamInfo};

use crate::mapping::Mapping;
use crate::page::{self, PageHeader};
use crate::{granule_of, xiph_unlace};

/// Default page body target: RFC 3533 describes pages as "usually 4-8 kB", and
/// one packet per page (the alternative) costs 28 bytes of header per 20 ms of
/// audio.
pub const DEFAULT_PAGE_TARGET: usize = 4096;

/// Vendor string in a synthesized `OpusTags` packet.
const VENDOR: &str = "ec-ogg";

/// One logical stream being written.
struct MuxTrack {
    serial: u32,
    /// Header packets in order; the first goes on the beginning-of-stream page.
    headers: Vec<Vec<u8>>,
    /// True once the caller supplied headers explicitly, which replace whatever
    /// `extradata` produced.
    headers_from_caller: bool,
    /// Lacing values of the page being built.
    segments: Vec<u8>,
    /// Body of the page being built.
    body: Vec<u8>,
    /// The page being built opens with the tail of a packet.
    continued: bool,
    /// Where the last packet finishing on this page ends, if one has.
    page_granule: Option<i64>,
    /// False when a packet finished on this page without stating its position:
    /// the page cannot be closed here without lying about where it ends.
    can_end: bool,
    /// The first data page has not been written yet. It is closed at the first
    /// position the stream states, whatever the size target says: a decoder
    /// reads its head trim out of that first granule (a Vorbis stream whose
    /// first page ends below what its packets decode to is saying "discard the
    /// difference"), and a page target large enough to swallow it would silently
    /// add the pre-roll back to the output.
    first_page_pending: bool,
    sequence: u32,
    /// Position written on the last closed page, for the end-of-stream page.
    last_granule: i64,
}

impl MuxTrack {
    fn pending(&self) -> bool {
        !self.segments.is_empty()
    }
}

/// An Ogg writer over any sink.
pub struct OggMuxer<W: Write> {
    out: W,
    streams: Vec<StreamInfo>,
    tracks: Vec<MuxTrack>,
    page_target: usize,
    headers_written: bool,
    finished: bool,
}

impl<W: Write> OggMuxer<W> {
    /// A writer with no streams yet.
    pub fn new(out: W) -> OggMuxer<W> {
        OggMuxer {
            out,
            streams: Vec::new(),
            tracks: Vec::new(),
            page_target: DEFAULT_PAGE_TARGET,
            headers_written: false,
            finished: false,
        }
    }

    /// Aim for `bytes` of body per page; [`None`] restores
    /// [`DEFAULT_PAGE_TARGET`]. A page still ends early when its segment table
    /// fills, and still ends late when the next packet boundary has no granule.
    pub fn set_page_target_bytes(&mut self, bytes: Option<usize>) {
        self.page_target = bytes.unwrap_or(DEFAULT_PAGE_TARGET).max(1);
    }

    /// Override the serial number of stream `index`. Serials only have to be
    /// unique within the file; the default is derived from the index so that
    /// two runs over the same input produce the same bytes.
    pub fn set_serial(&mut self, index: u32, serial: u32) -> Result<()> {
        let track = self
            .tracks
            .get_mut(index as usize)
            .ok_or_else(|| Error::corrupt(format!("Ogg mux: no stream {index}")))?;
        track.serial = serial;
        Ok(())
    }

    /// Write the beginning-of-stream and remaining header pages of every
    /// stream. Called automatically before the first data packet; public
    /// because a caller that declares streams and then hits an error still
    /// wants a readable file.
    pub fn write_headers(&mut self) -> Result<()> {
        if self.headers_written {
            return Ok(());
        }
        if self.tracks.is_empty() {
            return Err(Error::corrupt("Ogg mux: no streams were declared"));
        }
        // Every beginning-of-stream page first, in declaration order, before any
        // other page in the file (RFC 3533 §4).
        for index in 0..self.tracks.len() {
            let first = match self.tracks[index].headers.first() {
                Some(packet) => packet.clone(),
                None => {
                    return Err(Error::corrupt(format!(
                        "Ogg mux: stream {index} has no identification header (extradata was empty \
                         and no header packet was written)"
                    )));
                }
            };
            self.push_packet(index, &first, Some(0))?;
            self.flush_page(index, true, false)?;
        }
        // Then the remaining headers, each stream's flushed before any audio.
        for index in 0..self.tracks.len() {
            let rest: Vec<Vec<u8>> = self.tracks[index].headers[1..].to_vec();
            for packet in rest {
                self.push_packet(index, &packet, Some(0))?;
            }
            if self.tracks[index].pending() {
                self.flush_page(index, false, false)?;
            }
        }
        self.headers_written = true;
        Ok(())
    }

    /// Append one packet's segments to the page being built, splitting across
    /// pages when the segment table fills.
    fn push_packet(&mut self, index: usize, data: &[u8], granule: Option<i64>) -> Result<()> {
        // Prefer a clean packet boundary to a continuation: if the packet cannot
        // fit in what is left of the segment table, close the page first — as
        // long as closing it here would state a truthful position.
        let needed = data.len() / 255 + 1;
        let track = &self.tracks[index];
        if track.pending() && track.can_end && track.segments.len() + needed > page::MAX_SEGMENTS {
            self.flush_page(index, false, false)?;
        }

        let mut at = 0usize;
        for lace in page::lacing(data.len()) {
            if self.tracks[index].segments.len() == page::MAX_SEGMENTS {
                // Forced mid-packet: no packet finishes on this page, which is
                // exactly what a -1 granule means.
                self.flush_page(index, false, false)?;
                self.tracks[index].continued = true;
            }
            let track = &mut self.tracks[index];
            track.segments.push(lace);
            track
                .body
                .extend_from_slice(&data[at..at + usize::from(lace)]);
            at += usize::from(lace);
        }

        // A page may only end where the *last* packet finishing on it states
        // its position, so this tracks the latest packet, not the latest known
        // granule: one positionless packet after a positioned one closes the
        // page again.
        let track = &mut self.tracks[index];
        match granule {
            Some(granule) => {
                track.page_granule = Some(granule);
                track.can_end = true;
            }
            None => track.can_end = false,
        }
        Ok(())
    }

    /// Write the page under construction.
    fn flush_page(&mut self, index: usize, bos: bool, eos: bool) -> Result<()> {
        let track = &mut self.tracks[index];
        if track.segments.is_empty() && !eos {
            return Ok(());
        }
        let granule = match (track.can_end, track.page_granule) {
            (true, Some(granule)) => granule,
            // An end-of-stream page with nothing left to carry still states
            // where the stream ends, or a player has no length to trim to.
            (_, None) if eos && track.segments.is_empty() => track.last_granule,
            _ => page::NO_GRANULE,
        };
        let header = PageHeader {
            continued: track.continued,
            bos,
            eos,
            granule,
            serial: track.serial,
            sequence: track.sequence,
            // A page always carries at least one lacing value, even an
            // end-of-stream page with nothing left to say.
            segments: match track.segments.is_empty() {
                true => vec![0],
                false => std::mem::take(&mut track.segments),
            },
        };
        let body = std::mem::take(&mut track.body);
        track.sequence += 1;
        track.continued = false;
        track.page_granule = None;
        track.can_end = true;
        if granule != page::NO_GRANULE {
            track.last_granule = granule;
        }

        let mut bytes = Vec::with_capacity(page::HEADER_LEN + header.segments.len() + body.len());
        header.write_page(&body, &mut bytes);
        self.out.write_all(&bytes)?;
        Ok(())
    }

    /// Header packets from `extradata`, per mapping.
    fn headers_from_extradata(info: &StreamInfo) -> Result<Vec<Vec<u8>>> {
        let Some(extradata) = info.params.extradata.as_ref().filter(|e| !e.is_empty()) else {
            return Ok(Vec::new());
        };
        // Xiph-laced extradata carries the whole triplet; a bare identification
        // packet does not, and Vorbis has no way to invent the other two.
        if let Ok(packets) = xiph_unlace(extradata)
            && packets.len() >= 2
            && Mapping::identify(packets[0]).is_some()
        {
            return Ok(packets.into_iter().map(<[u8]>::to_vec).collect());
        }
        match Mapping::identify(extradata) {
            Some(Mapping::Opus { .. }) => Ok(vec![extradata.to_vec(), opus_tags()]),
            Some(Mapping::Vorbis { .. }) => Err(Error::corrupt(
                "Ogg mux: Vorbis extradata holds only the identification header; the comment and \
                 setup headers are needed too (Xiph-laced, as xiph_lace produces)",
            )),
            // Anything else — a FLAC mapping header, a codec this crate does not
            // name — is written verbatim as the first header packet, with the
            // rest expected as header-flagged packets.
            _ => Ok(vec![extradata.to_vec()]),
        }
    }
}

/// A minimal `OpusTags` packet: RFC 7845 §5 requires one, and a remux that only
/// carried `OpusHead` has no comments to preserve.
fn opus_tags() -> Vec<u8> {
    let mut out = Vec::from(*b"OpusTags");
    out.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    out.extend_from_slice(VENDOR.as_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // comment count
    out
}

impl<W: Write + Send> Muxer for OggMuxer<W> {
    fn add_stream(&mut self, info: StreamInfo) -> Result<u32> {
        if self.headers_written {
            return Err(Error::corrupt(
                "Ogg mux: every stream is declared before the first packet",
            ));
        }
        let index = self.tracks.len() as u32;
        let headers = OggMuxer::<W>::headers_from_extradata(&info)?;
        self.tracks.push(MuxTrack {
            // Unique within the file and stable across runs, which keeps a
            // remux byte-comparable with itself.
            serial: 0x4544_0000 ^ index,
            headers,
            headers_from_caller: false,
            segments: Vec::new(),
            body: Vec::new(),
            continued: false,
            page_granule: None,
            can_end: true,
            first_page_pending: true,
            sequence: 0,
            last_granule: 0,
        });
        let mut info = info;
        info.index = index;
        self.streams.push(info);
        Ok(index)
    }

    /// Write one packet.
    ///
    /// A packet flagged [`ec_core::PacketFlags::header`] before any audio joins
    /// the stream's header packets — the first one replaces whatever
    /// `extradata` gave, so a remux can carry the original `OpusTags` through.
    /// Otherwise the packet's granule position is [`crate::granule_of`], or
    /// `pts + duration` when both are known, or unknown — and a page never ends
    /// on a packet whose position is unknown.
    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let index = packet.stream as usize;
        if index >= self.tracks.len() {
            return Err(Error::corrupt(format!(
                "Ogg mux: packet for stream {} of {} declared",
                packet.stream,
                self.tracks.len()
            )));
        }
        if packet.flags.header && !self.headers_written {
            let track = &mut self.tracks[index];
            if !track.headers_from_caller {
                track.headers.clear();
                track.headers_from_caller = true;
            }
            track.headers.push(packet.data.to_vec());
            return Ok(());
        }
        self.write_headers()?;

        // The page is closed *before* the packet that would overflow it, never
        // after: that keeps at least one packet pending at all times, so the
        // end-of-stream flag rides a page that carries audio. A page whose only
        // content is a zero-length packet is legal Ogg and the oracle reads it as a
        // packet — "Packet processing failed", a decode error in a file that is
        // otherwise perfect.
        let track = &self.tracks[index];
        let due = track.body.len() >= self.page_target || track.first_page_pending;
        if track.pending() && track.can_end && due {
            self.flush_page(index, false, false)?;
            self.tracks[index].first_page_pending = false;
        }
        let granule = granule_of(packet).or_else(|| packet.end_pts());
        let data = packet.data.clone();
        self.push_packet(index, &data, granule)?;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.write_headers()?;
        for index in 0..self.tracks.len() {
            // The end-of-stream flag rides the last page — which still carries
            // audio, because packets are only paged out on the arrival of the
            // next one. Empty only when the stream had no data packets at all.
            self.flush_page(index, false, true)?;
        }
        self.out.flush()?;
        self.finished = true;
        Ok(())
    }
}

impl<W: Write> OggMuxer<W> {
    /// The sink, for callers that wrote into a buffer and want it back.
    pub fn into_inner(self) -> W {
        self.out
    }
}
