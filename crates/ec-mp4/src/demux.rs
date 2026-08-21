//! The reader: top-level walk, `moov` parse, sample tables, fragments, seek.

use std::io::{Read, Seek};

use ec_core::{
    AudioParameters, Buf, ChannelLayout, CodecId, CodecParameters, ColorInfo, Demuxer, Error,
    MediaParameters, MediaType, Packet, PacketFlags, Result, Rounding, SeekMode, StreamInfo,
    TimeBase, Timestamp, VideoParameters, color, color::Tags,
};

use crate::boxes::{Boxes, FourCc, Src, be16, be32, be64, full};
use crate::esds::{Esds, object_type};

/// The whole movie header is read into memory at once — it is the index, and a
/// two-hour film's is a few megabytes. Past this it is a crafted size field,
/// not a header.
const MOOV_LIMIT: u64 = 256 << 20;
/// One fragment header. Fragments are small by construction: a `moof` describes
/// a few seconds.
const MOOF_LIMIT: u64 = 64 << 20;
/// A single sample. The biggest real one is an intra 8K picture, a few
/// megabytes.
const SAMPLE_LIMIT: u64 = 256 << 20;

/// One sample of one track, resolved: everything a packet needs but its bytes.
///
/// This *is* the index an mp4 carries of itself, so it is public: a caller that
/// counts frames, builds a seek table or asks how many bytes a track spends
/// reads it here rather than walking the file a second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// Byte offset in the file.
    pub offset: u64,
    /// Size in bytes.
    pub size: u32,
    /// How long it lasts, in track ticks.
    pub duration: u32,
    /// Decode time in track ticks, edit list applied.
    pub dts: i64,
    /// Composition (presentation) time in track ticks, `ctts` and edit list
    /// applied.
    pub cts: i64,
    /// Whether a decoder may be started on it.
    pub sync: bool,
}

/// One `trak`, as far as the packet loop cares about it.
struct Trak {
    track_id: u32,
    /// What this track's `colr` box stated, tier by tier — empty for a track
    /// with no such box and for one that is not a picture.
    color_tags: Tags,
    /// Index in [`Mp4Demuxer::streams`], or [`None`] for a track whose sample
    /// entry this build has no [`CodecId`] for — its samples are walked past,
    /// never handed out as some other codec's.
    stream: Option<u32>,
    time_base: TimeBase,
    samples: Vec<Sample>,
    cursor: usize,
    title: Option<String>,
}

/// What the container says about one track, whether or not this build has a
/// [`CodecId`] for what is in it.
///
/// A track whose sample entry is unknown here has `stream: None` — its samples
/// are never handed out as some other codec's — but it is still *listed*, with
/// the four-character code that named it, so a caller can say what it is leaving
/// out instead of pretending the file has fewer tracks than it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    /// `tkhd` track id, which is what the file itself names the track by.
    pub track_id: u32,
    /// What the `hdlr` says the track carries, [`None`] for a handler this does
    /// not map (a chapter or a hint track).
    pub media: Option<MediaType>,
    /// Four-character code of the first `stsd` entry: `avc1`, `hvc1`, `mp4a`,
    /// `ac-3`, `tx3g`, ... Zero where the track has no sample entry at all.
    pub sample_entry: [u8; 4],
    /// Index in [`Mp4Demuxer::streams`], or [`None`] for a track walked past.
    pub stream: Option<u32>,
    /// ISO 639-2 language from the `mdhd`, [`None`] for the `und` a muxer
    /// writes when it was never told.
    pub language: Option<String>,
    /// `media_time` of the edit list's first real entry, in track ticks: where
    /// the presentation starts inside the media, which for a sound track is its
    /// encoder delay. [`None`] where the track carries no edit list, which is
    /// not the same claim as an edit list stating zero.
    pub media_time: Option<i64>,
}

/// `trex` defaults: what a fragment leaves unsaid.
#[derive(Debug, Clone, Copy, Default)]
struct Trex {
    track_id: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

/// An MP4/ISOBMFF or QuickTime `.mov` reader over anything seekable.
///
/// Opening reads the `moov` and builds the sample tables, which *is* the index:
/// every sample's offset, size, decode and composition time and whether it is a
/// random access point. Fragments (`moof`) are walked at open too and their
/// samples appended to the same tables, so a fragmented file seeks exactly like
/// a plain one.
pub struct Mp4Demuxer<R> {
    src: Src<R>,
    streams: Vec<StreamInfo>,
    traks: Vec<Trak>,
    /// One per [`Mp4Demuxer::traks`], same order: what the container said about
    /// the track, including the ones no stream was made for.
    tracks: Vec<TrackInfo>,
    title: Option<String>,
    fragments: usize,
    /// `mvhd` duration and timescale: how long the *movie* plays, which on a
    /// file whose sound outlasts its picture is neither track's own length.
    movie: (u64, u32),
}

impl<R: Read + Seek> Mp4Demuxer<R> {
    /// Read the movie header of `r` and build its sample tables.
    pub fn new(r: R) -> Result<Mp4Demuxer<R>> {
        let mut src = Src::new(r)?;
        let mut head = [0u8; 12];
        let got = src.read_upto(0, &mut head)?;
        if got >= 8 && !crate::is_mp4(&head[..got]) {
            return Err(Error::unsupported(
                "this file",
                "it is not an mp4 or QuickTime container",
            ));
        }
        let end = src.len;
        let mut moov = None;
        let mut moofs = Vec::new();
        let mut at = 0u64;
        // The top level, box by box: the header of each is 8 bytes and the rest
        // is skipped by seeking, so opening a 12 GB film costs a handful of
        // reads.
        while let Some((kind, body, stop)) = src.header_at(at, end)? {
            match &kind {
                b"moov" => moov = Some((body, stop)),
                b"moof" => moofs.push((at, body, stop)),
                _ => {}
            }
            if stop <= at {
                break;
            }
            at = stop;
        }
        let (body, stop) = moov.ok_or_else(|| match moofs.is_empty() {
            // A file that ends before its trailing moov is a truncated
            // download, not a broken container.
            true => Error::NeedMore,
            false => Error::corrupt("mp4: fragments with no moov to describe them"),
        })?;
        let data = src.read_vec(body, stop.saturating_sub(body), MOOV_LIMIT)?;
        let mut me = Mp4Demuxer {
            src,
            streams: Vec::new(),
            traks: Vec::new(),
            tracks: Vec::new(),
            title: None,
            fragments: moofs.len(),
            movie: (0, 0),
        };
        let trex = me.read_moov(&data)?;
        for (moof_at, body, stop) in moofs {
            let data = me
                .src
                .read_vec(body, stop.saturating_sub(body), MOOF_LIMIT)?;
            me.read_moof(moof_at, &data, &trex)?;
        }
        for trak in &mut me.traks {
            trak.samples.sort_by_key(|s| s.dts);
            if let Some(stream) = trak.stream {
                let info = &mut me.streams[stream as usize];
                info.start_time = trak.samples.first().map(|s| s.cts);
                if info.duration.is_none() {
                    info.duration = trak
                        .samples
                        .last()
                        .map(|s| s.dts + i64::from(s.duration) - trak.samples[0].dts);
                }
                // A fragmented file's `moov` carries an empty sample table, so
                // its frame rate is the one the fragments turned out to state.
                if let MediaParameters::Video(video) = &mut info.params.media
                    && video.frame_rate.is_none()
                {
                    let ticks: i64 = trak.samples.iter().map(|s| i64::from(s.duration)).sum();
                    video.frame_rate =
                        TimeBase::try_new(trak.samples.len() as i64 * trak.time_base.den(), ticks)
                            .ok();
                }
            }
        }
        Ok(me)
    }

    /// The movie's title from `moov/udta/name`, when it has one.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// One track's title from its `trak/udta/name`, when it has one — what an
    /// mp4 says a subtitle track is *called*.
    pub fn track_title(&self, stream: u32) -> Option<&str> {
        self.traks
            .iter()
            .find(|t| t.stream == Some(stream))
            .and_then(|t| t.title.as_deref())
    }

    /// How many `moof` fragments the file carries; 0 for a plain one.
    pub fn fragment_count(&self) -> usize {
        self.fragments
    }

    /// Every track the file declares, in file order — the order the `moov`
    /// wrote them in, which is the order a caller may number them by and get
    /// the same answer twice.
    pub fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// How long the *movie* plays, out of the `mvhd`: one length spanning every
    /// track, which is what a file's byte rate is measured over. [`None`] where
    /// the header states none.
    pub fn duration_secs(&self) -> Option<f64> {
        let (duration, timescale) = self.movie;
        (duration > 0 && timescale > 0).then(|| duration as f64 / f64::from(timescale))
    }

    /// What `stream`'s `colr` box **stated** about its colour, field by field:
    /// [`None`] wherever the box said nothing.
    ///
    /// Not the same answer as [`VideoParameters::color`], and the difference is
    /// the reason this exists. A `ColorInfo` has to fill in a `full_range` flag
    /// whether or not the file carried one, so a QuickTime `nclc` box — ten
    /// bytes, no range byte — reads there as a *declaration* of limited range
    /// and shadows a bitstream that said full. Here it is a matrix and a
    /// transfer and no range at all, which is what the file says.
    pub fn color_tags(&self, stream: u32) -> Tags {
        self.traks
            .iter()
            .find(|t| t.stream == Some(stream))
            .map_or(Tags::default(), |t| t.color_tags)
    }

    /// Every sample of `stream`, in decode order: the index the file carries of
    /// itself. Empty for a stream that does not exist.
    pub fn samples(&self, stream: u32) -> &[Sample] {
        self.traks
            .iter()
            .find(|t| t.stream == Some(stream))
            .map_or(&[], |t| &t.samples[..])
    }

    /// One sample of one stream by its index in [`Mp4Demuxer::samples`], read
    /// **without moving the packet cursor**: random access for a caller holding
    /// an index of its own, beside the sequential [`Demuxer::next_packet`].
    pub fn read_sample(&mut self, stream: u32, index: usize) -> Result<Packet> {
        let Some(trak) = self.traks.iter().find(|t| t.stream == Some(stream)) else {
            return Err(Error::corrupt(format!("mp4: no stream {stream} to read")));
        };
        let Some(sample) = trak.samples.get(index).copied() else {
            return Err(Error::Eof);
        };
        let time_base = trak.time_base;
        let data = self
            .src
            .read_vec(sample.offset, u64::from(sample.size), SAMPLE_LIMIT)?;
        Ok(packet_of(stream, time_base, &sample, data))
    }

    fn read_moov(&mut self, data: &[u8]) -> Result<Vec<Trex>> {
        let mut movie_timescale = 1_000u32;
        for child in Boxes::new(data) {
            let (kind, payload) = child?;
            if &kind == b"mvhd" {
                let (version, _, rest) = full(payload)?;
                let (timescale, duration) = match version {
                    0 => (be32(rest, 8)?, u64::from(be32(rest, 12)?)),
                    _ => (be32(rest, 16)?, be64(rest, 20)?),
                };
                movie_timescale = timescale;
                self.movie = (duration, timescale);
            }
        }
        let mut trex = Vec::new();
        for child in Boxes::new(data) {
            let (kind, payload) = child?;
            match &kind {
                b"trak" => self.read_trak(payload, movie_timescale)?,
                b"mvex" => {
                    for child in Boxes::new(payload) {
                        let (kind, payload) = child?;
                        if &kind == b"trex" {
                            let (_, _, rest) = full(payload)?;
                            trex.push(Trex {
                                track_id: be32(rest, 0)?,
                                duration: be32(rest, 8)?,
                                size: be32(rest, 12)?,
                                flags: be32(rest, 16)?,
                            });
                        }
                    }
                }
                b"udta" => self.title = udta_name(payload)?,
                _ => {}
            }
        }
        Ok(trex)
    }

    fn read_trak(&mut self, data: &[u8], movie_timescale: u32) -> Result<()> {
        let mut track_id = 0u32;
        let mut edit = None;
        let mut mdia = None;
        let mut title = None;
        let mut display = (0u32, 0u32);
        for child in Boxes::new(data) {
            let (kind, payload) = child?;
            match &kind {
                b"tkhd" => {
                    let (version, _, rest) = full(payload)?;
                    let at = if version == 0 { 8 } else { 16 };
                    track_id = be32(rest, at)?;
                    // ...and the display size, 16.16, at the far end of the box.
                    let end = rest.len().saturating_sub(8);
                    display = (be32(rest, end)? >> 16, be32(rest, end + 4)? >> 16);
                }
                b"edts" => {
                    for child in Boxes::new(payload) {
                        let (kind, payload) = child?;
                        if &kind == b"elst" {
                            edit = Some(elst(payload)?);
                        }
                    }
                }
                b"mdia" => mdia = Some(payload),
                b"udta" => title = udta_name(payload)?,
                _ => {}
            }
        }
        let Some(mdia) = mdia else {
            return Err(Error::corrupt("mp4: a trak with no mdia"));
        };

        let mut timescale = 0u32;
        let mut duration = 0u64;
        let mut language = None;
        let mut handler = [0u8; 4];
        let mut stbl = None;
        for child in Boxes::new(mdia) {
            let (kind, payload) = child?;
            match &kind {
                b"mdhd" => {
                    let (version, _, rest) = full(payload)?;
                    let (ts, dur, lang) = match version {
                        0 => (be32(rest, 8)?, u64::from(be32(rest, 12)?), be16(rest, 16)?),
                        _ => (be32(rest, 16)?, be64(rest, 20)?, be16(rest, 28)?),
                    };
                    timescale = ts;
                    duration = dur;
                    language = unpack_language(lang);
                }
                b"hdlr" => {
                    let (_, _, rest) = full(payload)?;
                    handler.copy_from_slice(rest.get(4..8).unwrap_or(&[0; 4]));
                }
                b"minf" => {
                    for child in Boxes::new(payload) {
                        let (kind, payload) = child?;
                        if &kind == b"stbl" {
                            stbl = Some(payload);
                        }
                    }
                }
                _ => {}
            }
        }
        if timescale == 0 {
            return Err(Error::corrupt("mp4: a track whose mdhd timescale is zero"));
        }
        let time_base = TimeBase::try_new(1, i64::from(timescale))?;
        let Some(stbl) = stbl else {
            return Err(Error::corrupt("mp4: a track with no sample table"));
        };

        // What the edit list moves the whole track by: an initial empty edit is
        // a delay, and the first real edit's media_time is where the media
        // starts. Both are pure shifts here — no sample is ever dropped, which
        // is the bug this exists not to have (a muxer writes media_time equal to
        // the first ctts delay, and reading that as a trim throws away real
        // pictures).
        let mut shift = 0i64;
        let media_time = edit
            .iter()
            .flatten()
            .map(|&(_, media_time, _)| media_time)
            .find(|&t| t >= 0);
        if let Some(edits) = edit {
            let mut empty = 0i64;
            for (segment, media_time, _) in edits {
                if media_time < 0 {
                    empty = empty.saturating_add(segment as i64);
                    continue;
                }
                shift = -media_time;
                break;
            }
            let movie = TimeBase::try_new(1, i64::from(movie_timescale.max(1)))?;
            shift = shift.saturating_add(movie.rescale(empty, time_base, Rounding::Nearest));
        }

        let table = SampleTable::read(stbl, self.src.len)?;
        let samples = table.build(shift)?;
        let track_language = language.clone();
        let color_tags = table
            .entry
            .as_ref()
            .map_or(Tags::default(), |e| e.color_tags);
        let stream = match table.entry {
            Some(entry) => {
                let media = match entry.codec.media_type() {
                    MediaType::Video => {
                        let mut video = entry.video.unwrap_or_default();
                        if video.width == 0 {
                            video.width = display.0;
                            video.height = display.1;
                        }
                        video.frame_rate = frame_rate(&table.stts, timescale);
                        MediaParameters::Video(video)
                    }
                    MediaType::Audio => MediaParameters::Audio(entry.audio.unwrap_or_default()),
                    MediaType::Subtitle => MediaParameters::Subtitle,
                };
                let index = self.streams.len() as u32;
                self.streams.push(StreamInfo {
                    index,
                    time_base,
                    params: CodecParameters {
                        codec: entry.codec,
                        extradata: entry.extradata,
                        media,
                    },
                    start_time: None,
                    duration: (duration > 0).then_some(duration as i64),
                    language,
                    // ISO-BMFF has no "play this one" flag: `tkhd` says whether
                    // a track is enabled, not which of two languages was meant,
                    // so file order is the whole answer here.
                    default: false,
                    // An mp4 states its encoder delay in the edit list, which is
                    // read above as a pure shift of the whole track rather than
                    // a trim: no sample is dropped, so none is announced as
                    // padding either.
                    initial_padding: 0,
                });
                Some(index)
            }
            None => None,
        };
        self.tracks.push(TrackInfo {
            track_id,
            media: match &handler {
                b"vide" => Some(MediaType::Video),
                b"soun" => Some(MediaType::Audio),
                b"sbtl" | b"subt" | b"text" => Some(MediaType::Subtitle),
                _ => None,
            },
            sample_entry: table.entry_kind,
            stream,
            language: track_language,
            media_time,
        });
        self.traks.push(Trak {
            track_id,
            color_tags,
            stream,
            time_base,
            samples,
            cursor: 0,
            title,
        });
        Ok(())
    }

    /// One fragment: `traf` by `traf`, appending to the tables the `moov` set up.
    fn read_moof(&mut self, moof_at: u64, data: &[u8], trex: &[Trex]) -> Result<()> {
        for child in Boxes::new(data) {
            let (kind, traf) = child?;
            if &kind != b"traf" {
                continue;
            }
            let mut tfhd = None;
            let mut base_time = None;
            let mut truns = Vec::new();
            for child in Boxes::new(traf) {
                let (kind, payload) = child?;
                match &kind {
                    b"tfhd" => tfhd = Some(full(payload)?),
                    b"tfdt" => {
                        let (version, _, rest) = full(payload)?;
                        base_time = Some(match version {
                            0 => i64::from(be32(rest, 0)?),
                            _ => be64(rest, 0)? as i64,
                        });
                    }
                    b"trun" => truns.push(full(payload)?),
                    _ => {}
                }
            }
            let Some((_, flags, rest)) = tfhd else {
                continue;
            };
            let track_id = be32(rest, 0)?;
            let mut at = 4;
            let mut base = moof_at;
            if flags & 0x01 != 0 {
                base = be64(rest, at)?;
                at += 8;
            }
            if flags & 0x02 != 0 {
                at += 4;
            }
            let defaults = trex.iter().find(|t| t.track_id == track_id).copied();
            let mut default_duration = defaults.map_or(0, |t| t.duration);
            let mut default_size = defaults.map_or(0, |t| t.size);
            let mut default_flags = defaults.map_or(0, |t| t.flags);
            if flags & 0x08 != 0 {
                default_duration = be32(rest, at)?;
                at += 4;
            }
            if flags & 0x10 != 0 {
                default_size = be32(rest, at)?;
                at += 4;
            }
            if flags & 0x20 != 0 {
                default_flags = be32(rest, at)?;
            }

            let Some(trak) = self.traks.iter_mut().find(|t| t.track_id == track_id) else {
                continue;
            };
            let mut dts = base_time.unwrap_or_else(|| {
                trak.samples
                    .last()
                    .map_or(0, |s| s.dts + i64::from(s.duration))
            });
            for (_, flags, rest) in truns {
                let count = be32(rest, 0)?;
                let mut at = 4usize;
                let mut offset = base;
                if flags & 0x01 != 0 {
                    offset = base.saturating_add_signed(i64::from(be32(rest, at)? as i32));
                    at += 4;
                }
                let mut first_flags = None;
                if flags & 0x04 != 0 {
                    first_flags = Some(be32(rest, at)?);
                    at += 4;
                }
                let width = (flags & 0x100 != 0) as usize * 4
                    + (flags & 0x200 != 0) as usize * 4
                    + (flags & 0x400 != 0) as usize * 4
                    + (flags & 0x800 != 0) as usize * 4;
                // A count that does not fit its own box is a crafted number, not
                // a sample run.
                if width > 0 && (count as u64) * (width as u64) > (rest.len() - at) as u64 {
                    return Err(Error::corrupt(format!(
                        "mp4: a trun of {count} samples inside {} bytes",
                        rest.len() - at
                    )));
                }
                for i in 0..count {
                    let mut duration = default_duration;
                    let mut size = default_size;
                    let mut sample_flags = match (i, first_flags) {
                        (0, Some(f)) => f,
                        _ => default_flags,
                    };
                    let mut cts_offset = 0i32;
                    if flags & 0x100 != 0 {
                        duration = be32(rest, at)?;
                        at += 4;
                    }
                    if flags & 0x200 != 0 {
                        size = be32(rest, at)?;
                        at += 4;
                    }
                    if flags & 0x400 != 0 {
                        sample_flags = be32(rest, at)?;
                        at += 4;
                    }
                    if flags & 0x800 != 0 {
                        cts_offset = be32(rest, at)? as i32;
                        at += 4;
                    }
                    trak.samples.push(Sample {
                        offset,
                        size,
                        duration,
                        dts,
                        cts: dts.saturating_add(i64::from(cts_offset)),
                        // Bit 16 of the sample flags is
                        // `sample_is_non_sync_sample`.
                        sync: sample_flags & 0x0001_0000 == 0,
                    });
                    offset = offset.saturating_add(u64::from(size));
                    dts = dts.saturating_add(i64::from(duration));
                }
            }
        }
        Ok(())
    }
}

impl<R: Read + Seek + Send> Demuxer for Mp4Demuxer<R> {
    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    fn next_packet(&mut self) -> Result<Packet> {
        // Storage order: whichever track's next sample sits earliest in the
        // file, which is exactly the interleave the muxer wrote.
        let mut pick: Option<(usize, u64)> = None;
        for (i, trak) in self.traks.iter().enumerate() {
            if trak.stream.is_none() {
                continue;
            }
            if let Some(sample) = trak.samples.get(trak.cursor)
                && pick.is_none_or(|(_, best)| sample.offset < best)
            {
                pick = Some((i, sample.offset));
            }
        }
        let Some((i, _)) = pick else {
            return Err(Error::Eof);
        };
        let trak = &mut self.traks[i];
        let sample = trak.samples[trak.cursor];
        trak.cursor += 1;
        let stream = trak.stream.unwrap_or(0);
        let time_base = trak.time_base;
        let data = self
            .src
            .read_vec(sample.offset, u64::from(sample.size), SAMPLE_LIMIT)?;
        Ok(packet_of(stream, time_base, &sample, data))
    }

    fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<()> {
        let Some(i) = self.traks.iter().position(|t| t.stream == Some(stream)) else {
            return Err(Error::corrupt(format!("mp4: no stream {stream} to seek")));
        };
        let target = to.rescale(self.traks[i].time_base, Rounding::Nearest).ticks;
        let trak = &self.traks[i];
        if trak.samples.is_empty() {
            return Err(Error::Eof);
        }
        // Decode times are sorted, so the sample holding the instant is a binary
        // search; the random access point is the walk out from it.
        let mut at = trak
            .samples
            .partition_point(|s| s.dts <= target)
            .saturating_sub(1);
        at = match mode {
            SeekMode::Exact => at,
            SeekMode::SyncBefore => {
                let mut back = at;
                while back > 0 && !trak.samples[back].sync {
                    back -= 1;
                }
                back
            }
            SeekMode::SyncAfter => {
                let mut fwd = at;
                while fwd + 1 < trak.samples.len() && !trak.samples[fwd].sync {
                    fwd += 1;
                }
                fwd
            }
        };
        let landed = Timestamp::new(trak.samples[at].dts, trak.time_base);
        self.traks[i].cursor = at;
        // Every other track resumes at the same instant, so the sound that goes
        // with the picture is there when the picture is decoded.
        for (j, trak) in self.traks.iter_mut().enumerate() {
            if j == i {
                continue;
            }
            let want = landed.rescale(trak.time_base, Rounding::Nearest).ticks;
            trak.cursor = trak.samples.partition_point(|s| s.dts < want);
        }
        Ok(())
    }
}

/// One resolved sample and its bytes as the packet a caller is handed.
fn packet_of(stream: u32, time_base: TimeBase, sample: &Sample, data: Vec<u8>) -> Packet {
    let mut packet = Packet::new(stream, time_base, data);
    packet.pts = Some(sample.cts);
    packet.dts = Some(sample.dts);
    packet.duration = Some(i64::from(sample.duration));
    packet.flags = PacketFlags {
        keyframe: sample.sync,
        ..PacketFlags::default()
    };
    packet
}

/// The five tables that turn a sample number into a byte range and an instant,
/// plus the one sample entry describing what those bytes are.
struct SampleTable<'a> {
    entry: Option<Entry>,
    /// The first `stsd` entry's four-character code, kept whether or not this
    /// build knows the codec behind it.
    entry_kind: FourCc,
    stts: Vec<(u32, u32)>,
    ctts: Vec<(u32, i32)>,
    stss: Vec<u32>,
    stsc: Vec<(u32, u32)>,
    stsz: (u32, u32, &'a [u8]),
    chunks: Vec<u64>,
    /// Field size of a compact `stz2` table, or 0 for a plain `stsz`.
    stz2_bits: u8,
}

impl<'a> SampleTable<'a> {
    fn read(stbl: &'a [u8], file_len: u64) -> Result<SampleTable<'a>> {
        let mut me = SampleTable {
            entry: None,
            entry_kind: [0; 4],
            stts: Vec::new(),
            ctts: Vec::new(),
            stss: Vec::new(),
            stsc: Vec::new(),
            stsz: (0, 0, &[]),
            chunks: Vec::new(),
            stz2_bits: 0,
        };
        for child in Boxes::new(stbl) {
            let (kind, payload) = child?;
            match &kind {
                b"stsd" => {
                    let (_, _, rest) = full(payload)?;
                    // entry_count, then the entries; only the first is used —
                    // a track whose samples change codec mid-file is not a
                    // thing any muxer writes.
                    if let Some(child) = Boxes::new(rest.get(4..).unwrap_or(&[])).next() {
                        let (kind, payload) = child?;
                        me.entry_kind = kind;
                        me.entry = sample_entry(&kind, payload)?;
                    }
                }
                b"stts" => me.stts = pairs(payload, |a, b| (a, b))?,
                b"ctts" => me.ctts = pairs(payload, |a, b| (a, b as i32))?,
                b"stss" => {
                    let (_, _, rest) = full(payload)?;
                    me.stss = words(rest)?;
                }
                b"stsc" => {
                    let (_, _, rest) = full(payload)?;
                    let count = count_of(rest, 12, file_len)?;
                    me.stsc = Vec::with_capacity(count);
                    for i in 0..count {
                        me.stsc
                            .push((be32(rest, 4 + i * 12)?, be32(rest, 8 + i * 12)?));
                    }
                }
                b"stsz" => {
                    let (_, _, rest) = full(payload)?;
                    let size = be32(rest, 0)?;
                    let count = be32(rest, 4)?;
                    me.stsz = (size, count, rest.get(8..).unwrap_or(&[]));
                }
                b"stz2" => {
                    let (_, _, rest) = full(payload)?;
                    me.stz2_bits = *rest
                        .get(3)
                        .ok_or_else(|| Error::corrupt("mp4: a stz2 with no field size"))?;
                    let count = be32(rest, 4)?;
                    me.stsz = (0, count, rest.get(8..).unwrap_or(&[]));
                }
                b"stco" => {
                    let (_, _, rest) = full(payload)?;
                    me.chunks = words(rest)?.into_iter().map(u64::from).collect();
                }
                b"co64" => {
                    let (_, _, rest) = full(payload)?;
                    let count = count_of(rest, 8, file_len)?;
                    me.chunks = Vec::with_capacity(count);
                    for i in 0..count {
                        me.chunks.push(be64(rest, 4 + i * 8)?);
                    }
                }
                _ => {}
            }
        }
        // A sample needs at least one byte in the file, so the file's own length
        // is the ceiling on how many of them there can be. Without this a four
        // byte `stsz` header claiming four billion samples allocates the machine.
        if u64::from(me.stsz.1) > file_len {
            return Err(Error::corrupt(format!(
                "mp4: a sample table of {} samples in a {file_len}-byte file",
                me.stsz.1
            )));
        }
        Ok(me)
    }

    /// The size of sample `i`, from whichever of the two size tables this track
    /// has.
    fn size(&self, i: usize) -> Result<u32> {
        let (fixed, _, table) = self.stsz;
        if fixed != 0 {
            return Ok(fixed);
        }
        match self.stz2_bits {
            0 | 32 => be32(table, i * 4),
            16 => be16(table, i * 2).map(u32::from),
            8 => table
                .get(i)
                .map(|&b| u32::from(b))
                .ok_or_else(|| Error::corrupt("mp4: stz2 ends inside a sample size")),
            4 => table
                .get(i / 2)
                .map(|&b| {
                    u32::from(if i.is_multiple_of(2) {
                        b >> 4
                    } else {
                        b & 0x0F
                    })
                })
                .ok_or_else(|| Error::corrupt("mp4: stz2 ends inside a sample size")),
            n => Err(Error::unsupported(
                format!("mp4: stz2 field size {n}"),
                "ISO 14496-12 allows 4, 8 and 16 only",
            )),
        }
    }

    /// Every sample resolved: chunk offsets walked out through `stsc`, decode
    /// times accumulated through `stts`, composition offsets from `ctts`, sync
    /// flags from `stss` — and `shift` (the edit list) folded into both clocks.
    fn build(&self, shift: i64) -> Result<Vec<Sample>> {
        let count = self.stsz.1 as usize;
        let mut out: Vec<Sample> = Vec::with_capacity(count);
        // Chunk walk: `stsc` states runs of chunks holding the same number of
        // samples, so a run ends where the next entry's first_chunk begins.
        let mut sample = 0usize;
        for (i, &(first, per_chunk)) in self.stsc.iter().enumerate() {
            let last = match self.stsc.get(i + 1) {
                Some(&(next, _)) => next.saturating_sub(1) as usize,
                None => self.chunks.len(),
            };
            let first = (first.max(1) - 1) as usize;
            if per_chunk == 0 {
                return Err(Error::corrupt("mp4: an stsc run of zero samples a chunk"));
            }
            for chunk in first..last.min(self.chunks.len()) {
                let mut offset = self.chunks[chunk];
                for _ in 0..per_chunk {
                    if sample >= count {
                        break;
                    }
                    let size = self.size(sample)?;
                    out.push(Sample {
                        offset,
                        size,
                        duration: 0,
                        dts: 0,
                        cts: 0,
                        sync: self.stss.is_empty(),
                    });
                    offset = offset.saturating_add(u64::from(size));
                    sample += 1;
                }
            }
        }
        // Decode times, and the composition offset on top of them.
        let mut dts = shift;
        let mut i = 0usize;
        for &(run, delta) in &self.stts {
            for _ in 0..run {
                let Some(s) = out.get_mut(i) else {
                    break;
                };
                s.dts = dts;
                s.cts = dts;
                s.duration = delta;
                dts = dts.saturating_add(i64::from(delta));
                i += 1;
            }
        }
        let mut i = 0usize;
        for &(run, offset) in &self.ctts {
            for _ in 0..run {
                let Some(s) = out.get_mut(i) else {
                    break;
                };
                s.cts = s.dts.saturating_add(i64::from(offset));
                i += 1;
            }
        }
        for &n in &self.stss {
            if let Some(s) = out.get_mut((n.max(1) - 1) as usize) {
                s.sync = true;
            }
        }
        Ok(out)
    }
}

/// A sample entry, read.
struct Entry {
    codec: CodecId,
    extradata: Option<Buf>,
    video: Option<VideoParameters>,
    audio: Option<AudioParameters>,
    /// What the `colr` box *stated*, which is not what
    /// [`VideoParameters::color`] answers: see [`colr`].
    color_tags: Tags,
}

/// What one `stsd` entry says its samples are, or [`None`] for a codec this
/// build has no id for — which is a track walked past, never one decoded as
/// something else.
fn sample_entry(kind: &FourCc, payload: &[u8]) -> Result<Option<Entry>> {
    // Visual and audio entries have different fixed prefixes; the child boxes
    // (the codec configuration, `colr`, the HDR pair) start after them.
    if let Some((codec, config)) = video_codec(kind) {
        let mut video = VideoParameters {
            width: u32::from(be16(payload, 24)?),
            height: u32::from(be16(payload, 26)?),
            ..VideoParameters::default()
        };
        let mut extradata = None;
        let mut color_tags = Tags::default();
        for child in Boxes::new(payload.get(78..).unwrap_or(&[])) {
            let (kind, body) = child?;
            if &kind == config {
                extradata = Some(Buf::copy_from_slice(body));
            } else if &kind == b"colr" {
                if let Some((info, tags)) = colr(body) {
                    video.color = info;
                    color_tags = tags;
                }
            } else if &kind == b"mdcv" {
                video.light = color::mdcv(body).over(video.light);
            } else if &kind == b"clli" {
                video.light = color::clli(body).over(video.light);
            } else if &kind == b"pasp" {
                let (num, den) = (be32(body, 0)?, be32(body, 4)?);
                video.sample_aspect_ratio = TimeBase::try_new(num.into(), den.into()).ok();
            }
        }
        return Ok(Some(Entry {
            codec,
            extradata,
            video: Some(video),
            audio: None,
            color_tags,
        }));
    }
    if kind == b"tx3g" {
        return Ok(Some(Entry {
            color_tags: Tags::default(),
            codec: CodecId::Tx3g,
            // The sample entry past its 8-byte header: the justification,
            // background colour and default style a renderer needs, which is
            // what every other reader of a `tx3g` track takes as its extradata.
            extradata: payload.get(8..).map(Buf::copy_from_slice),
            video: None,
            audio: None,
        }));
    }

    let version = be16(payload, 8).unwrap_or(0);
    let mut audio = AudioParameters {
        sample_rate: u32::from(be16(payload, 24).unwrap_or(0)),
        layout: ChannelLayout::from_count(be16(payload, 16).unwrap_or(2).max(1) as usize),
        ..AudioParameters::default()
    };
    let children = match version {
        0 => 28,
        1 => 28 + 16,
        _ => {
            // QuickTime's version 2 sound entry states the channel count as a
            // 32-bit field of its own; its sample rate is a double, which the
            // `mdhd` timescale says exactly instead.
            audio.layout =
                ChannelLayout::from_count(be32(payload, 28 + 12).unwrap_or(2).max(1) as usize);
            audio.sample_rate = 0;
            28 + 36
        }
    };
    let mut config = None;
    for child in Boxes::new(payload.get(children..).unwrap_or(&[])) {
        let (kind, body) = child?;
        config = Some((kind, body));
        if !matches!(&kind, b"btrt" | b"wave" | b"chan") {
            break;
        }
    }
    let (codec, extradata) = match kind {
        b"mp4a" => {
            let esds = match config {
                Some((k, body)) if &k == b"esds" => {
                    let (_, _, rest) = full(body)?;
                    Some(Esds::parse(rest)?)
                }
                _ => None,
            };
            match esds {
                // An mp4a entry is not automatically AAC: MP3 lives in one too,
                // and a track whose object type says so is kept as MP3 rather
                // than dropped for not being AAC.
                Some(esds) if esds.is_aac() => (CodecId::Aac, esds.decoder_specific),
                Some(esds)
                    if matches!(
                        esds.object_type,
                        object_type::MPEG1_AUDIO | object_type::MPEG2_AUDIO
                    ) =>
                {
                    (CodecId::Mp3, None)
                }
                // No esds at all, or one naming something else: AAC is what an
                // `mp4a` entry means by default and the extradata says the rest.
                Some(esds) => (CodecId::Aac, esds.decoder_specific),
                None => (CodecId::Aac, None),
            }
        }
        b".mp3" | b"ms\x00\x55" => (CodecId::Mp3, None),
        b"ac-3" => (CodecId::Ac3, config_bytes(config, b"dac3")),
        b"ec-3" => (CodecId::EAc3, config_bytes(config, b"dec3")),
        // Named rather than dropped: see `codec_of` in ec-matroska. A track
        // listed as unsupported beats a track that vanished.
        b"mlpa" => (CodecId::TrueHd, None),
        b"dtsc" | b"dtse" | b"dtsh" | b"dtsl" => (CodecId::Dts, None),
        b"alac" => (
            CodecId::Alac,
            // ALAC's magic cookie is taken with its box header, which is the
            // form every ALAC decoder is handed it in.
            config.filter(|(k, _)| k == b"alac").map(|(kind, body)| {
                let mut out = Vec::with_capacity(body.len() + 8);
                out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
                out.extend_from_slice(&kind);
                out.extend_from_slice(body);
                Buf::from_vec(out)
            }),
        ),
        b"fLaC" => (CodecId::Flac, config_bytes(config, b"dfLa")),
        b"Opus" => (
            CodecId::Opus,
            config
                .filter(|(k, _)| k == b"dOps")
                .map(|(_, body)| opus_head(body)),
        ),
        b"sowt" => (CodecId::PcmS16Le, None),
        b"twos" => (CodecId::PcmS16Be, None),
        b"raw " => (CodecId::PcmU8, None),
        _ => return Ok(None),
    };
    Ok(Some(Entry {
        codec,
        extradata,
        video: None,
        audio: Some(audio),
        color_tags: Tags::default(),
    }))
}

fn config_bytes(config: Option<(FourCc, &[u8])>, want: &FourCc) -> Option<Buf> {
    config
        .filter(|(kind, _)| kind == want)
        .map(|(_, body)| Buf::copy_from_slice(body))
}

/// The `OpusHead` a decoder expects, out of the `dOps` box an mp4 states the
/// same fields in — same order, opposite byte order, and the magic and version
/// the box leaves out.
pub(crate) fn opus_head(dops: &[u8]) -> Buf {
    let mut out = Vec::with_capacity(dops.len() + 8);
    out.extend_from_slice(b"OpusHead");
    out.push(1);
    out.push(dops.get(1).copied().unwrap_or(2)); // channel count
    let be16_at = |at: usize| {
        u16::from_be_bytes([
            dops.get(at).copied().unwrap_or(0),
            dops.get(at + 1).copied().unwrap_or(0),
        ])
    };
    out.extend_from_slice(&be16_at(2).to_le_bytes()); // pre-skip
    let rate = u32::from_be_bytes([
        dops.get(4).copied().unwrap_or(0),
        dops.get(5).copied().unwrap_or(0),
        dops.get(6).copied().unwrap_or(0),
        dops.get(7).copied().unwrap_or(0),
    ]);
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&be16_at(8).to_le_bytes()); // output gain
    out.extend_from_slice(&dops[10.min(dops.len())..]); // mapping family and table
    Buf::from_vec(out)
}

fn video_codec(kind: &FourCc) -> Option<(CodecId, &'static FourCc)> {
    Some(match kind {
        b"avc1" | b"avc3" => (CodecId::H264, b"avcC"),
        b"hvc1" | b"hev1" => (CodecId::H265, b"hvcC"),
        b"vp08" => (CodecId::Vp8, b"vpcC"),
        b"vp09" => (CodecId::Vp9, b"vpcC"),
        b"av01" => (CodecId::Av1, b"av1C"),
        _ => return None,
    })
}

/// A `ColourInformationBox`: the H.273 triplet, when it is the `nclx`/`nclc`
/// kind rather than an ICC profile — as the raw triplet *and* as the tags that
/// triplet really states.
///
/// The two are not the same claim, and the difference is a whole box: an `nclx`
/// carries a range bit and a QuickTime `nclc` is ten bytes that stop before it.
/// [`ColorInfo`] has to answer `full_range` either way, so a caller reading only
/// that would take an `nclc` for a declaration of limited range and shadow a
/// bitstream that said full. [`Tags`] can say *nothing*, so that is what an
/// `nclc` — and an `nclx` truncated before its range byte — comes back as.
fn colr(body: &[u8]) -> Option<(ColorInfo, Tags)> {
    let kind = body.get(..4)?;
    if kind != b"nclx" && kind != b"nclc" {
        return None;
    }
    let (primaries, transfer, matrix) = (
        be16(body, 4).ok()?,
        be16(body, 6).ok()?,
        be16(body, 8).ok()?,
    );
    // The Matroska `Range` coding [`Tags::from_codes`] takes: 0 says nothing,
    // 1 limited, 2 full. A plain `video_full_range_flag` is `1 + flag`, and a
    // box that never stated one is the 0.
    let range = match kind == b"nclx" {
        true => body.get(10).map_or(0, |b| 1 + u64::from(b >> 7)),
        false => 0,
    };
    let info = ColorInfo {
        primaries: primaries.min(255) as u8,
        transfer: transfer.min(255) as u8,
        matrix: matrix.min(255) as u8,
        full_range: range == 2,
    };
    Some((
        info,
        Tags::from_codes(u64::from(matrix), u64::from(transfer), range),
    ))
}

/// Frames per second off the sample table, as the rational it is.
///
/// Whole track over whole track: a constant-delta table comes out as exactly
/// `timescale/delta`, and a table spreading 3753/3754 ticks to average 3753.75
/// comes out as exactly 24000/1001 rather than as the 23.0 a truncating
/// millisecond division answers.
fn frame_rate(stts: &[(u32, u32)], timescale: u32) -> Option<TimeBase> {
    let (samples, ticks) = stts.iter().fold((0u64, 0u64), |(n, t), &(count, delta)| {
        (
            n + u64::from(count),
            t + u64::from(count) * u64::from(delta),
        )
    });
    if ticks == 0 || samples == 0 {
        return None;
    }
    TimeBase::try_new(
        (samples as i128 * i128::from(timescale)).try_into().ok()?,
        ticks.try_into().ok()?,
    )
    .ok()
}

/// A `udta`'s `name` box, which is what an mp4 calls a thing.
fn udta_name(payload: &[u8]) -> Result<Option<String>> {
    for child in Boxes::new(payload) {
        let (kind, body) = child?;
        if &kind == b"name" {
            let text = body.split(|&b| b == 0).next().unwrap_or(body);
            return Ok(Some(String::from_utf8_lossy(text).into_owned()));
        }
    }
    Ok(None)
}

/// ISO 639-2/T out of the five-bit-a-letter packing an `mdhd` states it in.
/// `und` is what a muxer writes when it was never told, so it is not a language.
fn unpack_language(packed: u16) -> Option<String> {
    let letters: String = (0..3)
        .rev()
        .map(|i| char::from(0x60 + ((packed >> (i * 5)) & 0x1F) as u8))
        .collect();
    letters
        .chars()
        .all(|c| c.is_ascii_lowercase())
        .then_some(letters)
        .filter(|l| l != "und")
}

/// The `(count, value)` pairs of an `stts`/`ctts` table.
fn pairs<T>(payload: &[u8], make: impl Fn(u32, u32) -> T) -> Result<Vec<T>> {
    let (_, _, rest) = full(payload)?;
    let count = count_of(rest, 8, u64::MAX)?;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        out.push(make(be32(rest, 4 + i * 8)?, be32(rest, 8 + i * 8)?));
    }
    Ok(out)
}

/// The `u32` table of an `stss`/`stco`.
fn words(rest: &[u8]) -> Result<Vec<u32>> {
    let count = count_of(rest, 4, u64::MAX)?;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        out.push(be32(rest, 4 + i * 4)?);
    }
    Ok(out)
}

/// An entry count, refused when the entries it claims do not fit the box it is
/// in — the one check that keeps a crafted count from being allocated.
fn count_of(rest: &[u8], width: usize, _file_len: u64) -> Result<usize> {
    let count = be32(rest, 0)? as usize;
    let have = rest.len().saturating_sub(4) / width;
    if count > have {
        return Err(Error::corrupt(format!(
            "mp4: a table of {count} entries inside {have}"
        )));
    }
    Ok(count)
}

/// The `elst` entries: `(segment duration, media time, media rate)`.
fn elst(payload: &[u8]) -> Result<Vec<(u64, i64, u32)>> {
    let (version, _, rest) = full(payload)?;
    let width = if version == 0 { 12 } else { 20 };
    let count = count_of(rest, width, u64::MAX)?;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = 4 + i * width;
        out.push(match version {
            0 => (
                u64::from(be32(rest, at)?),
                i64::from(be32(rest, at + 4)? as i32),
                be32(rest, at + 8)?,
            ),
            _ => (
                be64(rest, at)?,
                be64(rest, at + 8)? as i64,
                be32(rest, at + 16)?,
            ),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes;

    /// The named bug: NTSC film must come back off the sample table as itself.
    #[test]
    fn ntsc_frame_rates_stay_rational() {
        // 24000/1001 on a 90 kHz clock is 3753.75 ticks a frame, so a muxer
        // spreads 3753/3754 and only the total is exact.
        assert_eq!(
            frame_rate(&[(1, 3753), (3, 3754)], 90_000),
            Some(TimeBase::new(24_000, 1001))
        );
        // The clock a muxer that can choose one picks instead.
        assert_eq!(
            frame_rate(&[(240, 1001)], 24_000),
            Some(TimeBase::new(24_000, 1001))
        );
        assert_eq!(
            frame_rate(&[(120, 1001)], 30_000),
            Some(TimeBase::new(30_000, 1001))
        );
        assert_eq!(
            frame_rate(&[(60, 1001)], 60_000),
            Some(TimeBase::new(60_000, 1001))
        );
        // Integer rates divide exactly rather than averaging.
        assert_eq!(
            frame_rate(&[(300, 3000)], 90_000),
            Some(TimeBase::new(30, 1))
        );
        assert_eq!(frame_rate(&[], 90_000), None);
        assert_eq!(frame_rate(&[(4, 0)], 90_000), None);
    }

    #[test]
    fn languages_and_names_come_out_of_their_packings() {
        // 'tur' packed five bits a letter.
        let packed =
            ((b't' - 0x60) as u16) << 10 | ((b'u' - 0x60) as u16) << 5 | (b'r' - 0x60) as u16;
        assert_eq!(unpack_language(packed).as_deref(), Some("tur"));
        let und = ((b'u' - 0x60) as u16) << 10 | ((b'n' - 0x60) as u16) << 5 | (b'd' - 0x60) as u16;
        assert_eq!(unpack_language(und), None);
        assert_eq!(unpack_language(0), None);

        let mut udta = Vec::new();
        boxes::leaf(&mut udta, b"name", b"Turkish\0");
        assert_eq!(udta_name(&udta).unwrap().as_deref(), Some("Turkish"));
    }

    #[test]
    fn a_colr_box_is_the_h273_triplet() {
        let triplet = |kind: &[u8; 4]| {
            let mut body = kind.to_vec();
            body.extend_from_slice(&9u16.to_be_bytes());
            body.extend_from_slice(&16u16.to_be_bytes());
            body.extend_from_slice(&9u16.to_be_bytes());
            body
        };
        let mut full = triplet(b"nclx");
        full.push(0x80);
        assert_eq!(
            colr(&full).map(|(info, _)| info),
            Some(ColorInfo {
                primaries: 9,
                transfer: 16,
                matrix: 9,
                full_range: true
            })
        );
        assert_eq!(
            colr(&full).map(|(_, tags)| tags.full_range),
            Some(Some(true))
        );
        let mut limited = triplet(b"nclx");
        limited.push(0x00);
        assert_eq!(
            colr(&limited).map(|(_, tags)| tags.full_range),
            Some(Some(false)),
            "an nclx that says limited is a claim, and stands"
        );
        assert_eq!(colr(b"prof\0\0"), None, "an ICC profile is not a triplet");
    }

    /// The box that has no range byte to read: QuickTime's `nclc`, and an `nclx`
    /// truncated before its own last byte. Both state a matrix and a transfer
    /// and **nothing** about range -- a fabricated `limited` here shadows a
    /// bitstream that said full, and that is a green picture on screen.
    #[test]
    fn an_nclc_states_no_range_at_all() {
        let mut body = b"nclc".to_vec();
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        assert_eq!(body.len(), 10, "an nclc payload stops at ten bytes");
        let (info, tags) = colr(&body).expect("nclc is a triplet");
        assert_eq!((info.primaries, info.transfer, info.matrix), (1, 1, 1));
        assert_eq!(tags.matrix, Some(color::Matrix::Bt709));
        assert_eq!(tags.transfer, Some(color::Transfer::Sdr));
        assert_eq!(tags.full_range, None, "an nclc box states no range");
        // ...and the same box with an nclx name but no range byte written.
        let mut short = b"nclx".to_vec();
        short.extend_from_slice(&body[4..]);
        assert_eq!(colr(&short).expect("still a triplet").1.full_range, None);
    }

    #[test]
    fn crafted_table_counts_are_refused_before_they_are_allocated() {
        // An stco stating four billion chunks inside eight bytes.
        let mut stco = vec![0, 0, 0, 0];
        stco.extend_from_slice(&u32::MAX.to_be_bytes());
        stco.extend_from_slice(&[0; 4]);
        let (_, _, rest) = full(&stco).unwrap();
        assert!(words(rest).is_err());
        assert!(pairs(&stco, |a, b| (a, b)).is_err());
        assert!(elst(&stco).is_err());
    }

    #[test]
    fn a_dops_box_becomes_an_opus_head() {
        // Version 0, 2 channels, pre-skip 312, 48 kHz, gain 0, family 0.
        let dops = [0u8, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 0];
        let head = opus_head(&dops);
        assert_eq!(&head[..8], b"OpusHead");
        assert_eq!(head[8], 1, "OpusHead version");
        assert_eq!(head[9], 2);
        assert_eq!(u16::from_le_bytes([head[10], head[11]]), 312);
        assert_eq!(
            u32::from_le_bytes([head[12], head[13], head[14], head[15]]),
            48_000
        );
        assert_eq!(head[18], 0, "channel mapping family");
    }
}
