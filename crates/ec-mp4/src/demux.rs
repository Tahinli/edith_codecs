//! The reader: top-level walk, `moov` parse, sample tables, fragments, seek.

use std::io::{Read, Seek};

use ec_core::{
    AudioParameters, Buf, ChannelLayout, CodecId, CodecParameters, ColorInfo, Demuxer, Error,
    MediaParameters, MediaType, Packet, PacketFlags, Result, Rotation, Rounding, SeekMode,
    StreamInfo, TimeBase, Timestamp, VideoParameters, color, color::Tags,
};

use crate::asc;
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
    /// The `fiel` box, where the sample entry carried one.
    field_order: Option<ec_core::Interlace>,
    /// Index in [`Mp4Demuxer::streams`], or [`None`] for a track whose sample
    /// entry this build has no [`CodecId`] for — its samples are walked past,
    /// never handed out as some other codec's.
    stream: Option<u32>,
    time_base: TimeBase,
    samples: Vec<Sample>,
    cursor: usize,
    title: Option<String>,
    /// [`presentation_trim`](Mp4Demuxer::presentation_trim): the media ticks the
    /// edit list trims off the front, or [`None`] with no edit list.
    trim: Option<u64>,
    /// True for a track with both a `ctts` and an `stss` table, the only tracks
    /// whose samples are stored out of display order and whose sync points need
    /// the display-order table ([`Mp4Demuxer::sync_in_display_order`]).
    reorders: bool,
}

/// One entry of an `elst` edit list, as the file states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    /// How long the edit plays, in movie-timescale ticks.
    pub segment_duration: u64,
    /// Where in the media the edit starts, in track ticks. `-1` (see
    /// [`Edit::is_empty`]) is an empty edit: a gap with no media in it.
    pub media_time: i64,
    /// The playback rate, 16.16 fixed point; `0x0001_0000` is normal speed.
    pub media_rate: u32,
}

impl Edit {
    /// An empty edit: `media_time` of `-1`, a delay with no media in it.
    pub fn is_empty(&self) -> bool {
        self.media_time < 0
    }

    /// The playback rate as a float: the integer and fraction halves of the
    /// 16.16 field, `1.0` for normal speed.
    pub fn rate(&self) -> f64 {
        f64::from(self.media_rate) / 65536.0
    }
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
    /// Every `elst` entry in file order, empty edits included. Empty for a track
    /// with no edit list.
    pub edits: Vec<Edit>,
    /// The quarter turn the `tkhd` display matrix asks for, as
    /// [`Rotation::Other`] for a matrix that is not one. Reported for every
    /// track, a stream or not.
    pub rotation: Rotation,
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
        while let Some((kind, body, stop, complete)) = src.header_at(at, end)? {
            match &kind {
                b"moov" if complete => moov = Some((body, stop)),
                b"moof" if complete => moofs.push((at, body, stop)),
                _ => {}
            }
            if stop <= at {
                break;
            }
            at = stop;
        }
        let (body, stop) = moov.ok_or(Error::NeedMore)?;
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
        // An `mp4a` entry whose object type says MPEG audio is named MP3 above, and ffmpeg
        // writes MP2 under the same object type (0x6B). The first frame's header states the
        // layer, so a layer II or I track is relabelled: the decoder reads any layer, but the
        // name a warning or a stream listing gives must be the codec's own.
        for stream in 0..me.streams.len() as u32 {
            if me.streams[stream as usize].params.codec != CodecId::Mp3 {
                continue;
            }
            let Ok(first) = me.read_sample(stream, 0) else {
                continue;
            };
            let codec = match mpeg_audio_layer(&first.data) {
                Some(1) => CodecId::Mp1,
                Some(2) => CodecId::Mp2,
                _ => continue,
            };
            me.streams[stream as usize].params.codec = codec;
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

    /// The scan order the `fiel` box of `stream`'s sample entry stated, or
    /// [`None`] where there was no box (or an unrecognised one).
    pub fn field_order(&self, stream: u32) -> Option<ec_core::Interlace> {
        self.traks
            .iter()
            .find(|t| t.stream == Some(stream))
            .and_then(|t| t.field_order)
    }

    /// Every sample of `stream`, in decode order: the index the file carries of
    /// itself. Empty for a stream that does not exist.
    pub fn samples(&self, stream: u32) -> &[Sample] {
        self.traks
            .iter()
            .find(|t| t.stream == Some(stream))
            .map_or(&[], |t| &t.samples[..])
    }

    /// The media ticks the edit list trims off the front of `stream`, as the
    /// engine above this crate computes it: the first real edit's `media_time`
    /// less the first composition offset (a `ctts` delay is not a trim). [`None`]
    /// with no edit list. Empty edits are not counted, so this differs from the
    /// shift [`Sample::dts`] and [`Sample::cts`] apply; see [`crate`] for why both
    /// exist.
    pub fn presentation_trim(&self, stream: u32) -> Option<u64> {
        self.trak_of(stream).and_then(|t| t.trim)
    }

    /// The 1-based sample id of the sample the presentation starts on: the one
    /// whose decode interval holds [`Mp4Demuxer::presentation_trim`]. `1` with no
    /// trim, and the last sample when the trim runs past the end of the track.
    pub fn first_frame_sample(&self, stream: u32) -> u32 {
        let Some(trak) = self.trak_of(stream) else {
            return 1;
        };
        let Some(trim) = trak.trim else {
            return 1;
        };
        // Decode time from zero, so the first sample's own start is subtracted
        // back out of the shifted clock the samples carry.
        let base = trak.samples.first().map_or(0, |s| s.dts);
        let target = i64::try_from(trim).unwrap_or(i64::MAX);
        match trak.samples.iter().position(|s| {
            let start = s.dts.saturating_sub(base);
            target < start.saturating_add(i64::from(s.duration))
        }) {
            Some(index) => index as u32 + 1,
            None => (trak.samples.len() as u32).max(1),
        }
    }

    /// Every sync sample of `stream` paired with the display index it really
    /// occupies, `(display index, decode position)`, ascending. Non-empty only
    /// for a track with both `ctts` and `stss`, the case where decode order and
    /// display order differ; an open-GOP stream's keyframes land on a different
    /// display rank than their decode rank without this.
    pub fn sync_in_display_order(&self, stream: u32) -> Vec<(u32, u32)> {
        let Some(trak) = self.trak_of(stream) else {
            return Vec::new();
        };
        if !trak.reorders {
            return Vec::new();
        }
        let mut order: Vec<usize> = (0..trak.samples.len()).collect();
        // Stable, so samples sharing a composition time keep decode order.
        order.sort_by_key(|&i| trak.samples[i].cts);
        order
            .into_iter()
            .enumerate()
            .filter(|&(_, decode)| trak.samples[decode].sync)
            .map(|(display, decode)| (display as u32, decode as u32))
            .collect()
    }

    fn trak_of(&self, stream: u32) -> Option<&Trak> {
        self.traks.iter().find(|t| t.stream == Some(stream))
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
        let data = match self
            .src
            .read_vec(sample.offset, u64::from(sample.size), SAMPLE_LIMIT)
        {
            Ok(data) => data,
            Err(e) if e.is_need_more() => return Err(Error::Eof),
            Err(e) => return Err(e),
        };
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
        let mut rotation = Rotation::None;
        for child in Boxes::new(data) {
            let (kind, payload) = child?;
            match &kind {
                b"tkhd" => {
                    let (version, _, rest) = full(payload)?;
                    let at = if version == 0 { 8 } else { 16 };
                    track_id = be32(rest, at)?;
                    // The display matrix -- nine 16.16 fixed-point `s32` -- and
                    // the display size, also 16.16, at the far end of the box.
                    // The matrix is the 36 bytes just before the size; only its
                    // leading 2x2 (`a b c d`) says which way the picture turns.
                    let end = rest.len().saturating_sub(8);
                    let matrix = end.saturating_sub(36);
                    let (a, b, c, d) = (
                        be32(rest, matrix)? as i32,
                        be32(rest, matrix + 4)? as i32,
                        be32(rest, matrix + 12)? as i32,
                        be32(rest, matrix + 16)? as i32,
                    );
                    rotation = display_rotation(a, b, c, d);
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
        let edit_list: Vec<(u64, i64, u32)> = edit.clone().unwrap_or_default();
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

        let table = SampleTable::read(stbl, self.src.len, &handler == b"vide")?;
        let mut samples = table.build(shift)?;
        if let Some(entry) = &table.entry
            && let Some(audio) = &entry.audio
            && is_wide_pcm(entry.codec)
        {
            let bytes = if matches!(entry.codec, CodecId::PcmS24Le | CodecId::PcmS24Be) {
                3
            } else {
                4
            };
            group_pcm(&mut samples, bytes * audio.layout.channel_count() as u32);
        }
        // What the edit list trims, as the engine above this crate reads it: the
        // first real edit's media time less the first composition offset, which
        // a muxer writes as exactly that delay and which is not a trim at all.
        let first_offset = table.ctts.first().map_or(0, |&(_, o)| i64::from(o).max(0));
        let trim = media_time.map(|t| (t.max(0) as u64).saturating_sub(first_offset as u64));
        let reorders = !table.ctts.is_empty() && !table.stss.is_empty();
        let edits: Vec<Edit> = edit_list
            .iter()
            .map(|&(segment_duration, media_time, media_rate)| Edit {
                segment_duration,
                media_time,
                media_rate,
            })
            .collect();
        let track_language = language.clone();
        let color_tags = table
            .entry
            .as_ref()
            .map_or(Tags::default(), |e| e.color_tags);
        let field_order = table.entry.as_ref().and_then(|e| e.field_order);
        let stream = match table.entry {
            Some(entry) => {
                let media = match entry.codec.media_type() {
                    MediaType::Video => {
                        let mut video = entry.video.unwrap_or_default();
                        // The coded size is the sample entry's for H.264 and the
                        // `tkhd` display size for every other codec -- what the
                        // mp4 crate edith read these through answers (`Mp4Track::width`
                        // reads `avc1` and falls back to `tkhd`). A VP8 entry that
                        // is eight bytes short reads 72 wide here, and 1280 from
                        // `tkhd`, which is also what ffprobe reports for it.
                        if (entry.codec != CodecId::H264 || video.width == 0) && display.0 != 0 {
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
                    // The `tkhd` display matrix: which way the coded picture
                    // turns to be seen as shot. Reported, not applied here.
                    rotation,
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
            edits,
            rotation,
        });
        self.traks.push(Trak {
            track_id,
            color_tags,
            field_order,
            stream,
            time_base,
            samples,
            cursor: 0,
            title,
            trim,
            reorders,
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

            let file_len = self.src.len;
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
                // A run with no per-sample fields states its samples in its count
                // alone, so the box cannot bound it: the file's length does, as it
                // does for `stsz`. Without this a 16-byte `trun` claiming four
                // billion samples allocates the machine.
                if trak.samples.len() as u64 + u64::from(count) > file_len {
                    return Err(Error::corrupt(format!(
                        "mp4: a trun of {count} samples past the {file_len}-byte file"
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
        let data = match self
            .src
            .read_vec(sample.offset, u64::from(sample.size), SAMPLE_LIMIT)
        {
            Ok(data) => data,
            Err(e) if e.is_need_more() => return Err(Error::Eof),
            Err(e) => return Err(e),
        };
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
    fn read(stbl: &'a [u8], file_len: u64, video_track: bool) -> Result<SampleTable<'a>> {
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
                        me.entry = sample_entry(&kind, payload, video_track)?;
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
    field_order: Option<ec_core::Interlace>,
}

/// What one `stsd` entry says its samples are, or [`None`] for a codec this
/// build has no id for — which is a track walked past, never one decoded as
/// something else.
/// The most channels a sample entry may state. The widest real layouts are a few
/// dozen (a 22.2 bed, an Atmos object bed); past this the count is a crafted
/// field, and building a layout from it allocates per channel.
const MAX_CHANNELS: u32 = 64;

/// The layout a sample entry's channel count names, refused past [`MAX_CHANNELS`].
/// QuickTime's version-2 count is a 32-bit field, so without the bound a single
/// sound description can ask for four billion channel positions.
fn channel_layout(count: u32) -> Result<ChannelLayout> {
    if count > MAX_CHANNELS {
        return Err(Error::corrupt(format!(
            "mp4: a sample entry stating {count} channels, past {MAX_CHANNELS}"
        )));
    }
    Ok(ChannelLayout::from_count(count.max(1) as usize))
}

/// The MPEG audio layer (1, 2 or 3) of the frame `data` starts with, from its sync word and
/// layer bits; [`None`] when `data` does not start with a frame header.
fn mpeg_audio_layer(data: &[u8]) -> Option<u8> {
    match data {
        [0xFF, b1, ..] if b1 & 0xE0 == 0xE0 => match (b1 >> 1) & 0x3 {
            3 => Some(1),
            2 => Some(2),
            1 => Some(3),
            _ => None,
        },
        _ => None,
    }
}

fn sample_entry(kind: &FourCc, payload: &[u8], video_track: bool) -> Result<Option<Entry>> {
    // Visual and audio entries have different fixed prefixes; the child boxes
    // (the codec configuration, `colr`, the HDR pair) start after them.
    if let Some((codec, config)) = video_codec(kind, video_track) {
        let mut video = VideoParameters {
            width: u32::from(be16(payload, 24)?),
            height: u32::from(be16(payload, 26)?),
            ..VideoParameters::default()
        };
        let mut extradata = None;
        let mut color_tags = Tags::default();
        let mut field_order = None;
        let children = payload.get(78..).unwrap_or(&[]);
        for child in Boxes::new(children) {
            let (kind, body) = match child {
                Ok(c) => c,
                // QuickTime ends a sample description with a 32-bit zero
                // (ffmpeg's mov muxer writes it after the Avid boxes).
                Err(_) if children.ends_with(&[0; 4]) => break,
                Err(e) => return Err(e),
            };
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
            } else if &kind == b"fiel" {
                // field_count 1 is progressive; 2 fields name their order in
                // field_detail: 1/9 top first, 6/14 bottom first.
                field_order = match (body.first(), body.get(1)) {
                    (Some(1), _) => Some(ec_core::Interlace::Progressive),
                    (Some(2), Some(1 | 9)) => Some(ec_core::Interlace::TopFieldFirst),
                    (Some(2), Some(6 | 14)) => Some(ec_core::Interlace::BottomFieldFirst),
                    _ => None,
                };
            } else if &kind == b"pasp" {
                let (num, den) = (be32(body, 0)?, be32(body, 4)?);
                video.sample_aspect_ratio = TimeBase::try_new(num.into(), den.into()).ok();
            }
        }
        if codec == CodecId::RawVideo {
            // A 40-byte BITMAPINFOHEADER look-alike that `RawVideoDecoder` reads: "mov " in the
            // biSize slot, width, height, planes, depth and the entry's FourCC.
            let depth = be16(payload, 74)?;
            let ok = match kind {
                b"raw " => matches!(depth, 16 | 24 | 32),
                _ => true,
            };
            if !ok || video.width == 0 || video.height == 0 {
                return Ok(None);
            }
            let mut h = vec![0u8; 40];
            h[..4].copy_from_slice(b"mov ");
            h[4..8].copy_from_slice(&video.width.to_le_bytes());
            h[8..12].copy_from_slice(&video.height.to_le_bytes());
            h[12..14].copy_from_slice(&1u16.to_le_bytes());
            h[14..16].copy_from_slice(&depth.to_le_bytes());
            h[16..20].copy_from_slice(kind);
            extradata = Some(Buf::from_vec(h));
        }
        if kind == b"mp4v" {
            // objectTypeIndication 0x20 is Visual ISO/IEC 14496-2; any other value
            // in an `mp4v` entry (MPEG-2 video, JPEG, ...) is not this codec.
            let Some(body) = extradata.take() else {
                return Ok(None);
            };
            let (_, _, rest) = full(&body)?;
            let esds = Esds::parse(rest)?;
            if esds.object_type != object_type::MPEG4_VISUAL {
                return Ok(None);
            }
            extradata = esds.decoder_specific;
        }
        return Ok(Some(Entry {
            codec,
            extradata,
            video: Some(video),
            audio: None,
            color_tags,
            field_order,
        }));
    }
    if kind == b"mp4s" {
        // MPEG-4 systems stream: only the DVD subpicture object type (0xE0) is a codec here.
        // Its decoder-specific info is the `.idx` text (`size:`, `palette:`).
        for child in Boxes::new(payload.get(8..).unwrap_or(&[])) {
            let (child_kind, body) = child?;
            if child_kind == *b"esds" {
                let (_, _, rest) = full(body)?;
                let esds = Esds::parse(rest)?;
                if esds.object_type == object_type::VOBSUB {
                    return Ok(Some(Entry {
                        color_tags: Tags::default(),
                        field_order: None,
                        codec: CodecId::DvdSub,
                        extradata: esds.decoder_specific,
                        video: None,
                        audio: None,
                    }));
                }
            }
        }
        return Ok(None);
    }
    if kind == b"tx3g" {
        return Ok(Some(Entry {
            color_tags: Tags::default(),
            field_order: None,
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
        layout: channel_layout(u32::from(be16(payload, 16).unwrap_or(2).max(1)))?,
        ..AudioParameters::default()
    };
    let children = match version {
        0 => 28,
        1 => 28 + 16,
        _ => {
            // QuickTime's version 2 sound entry replaces the classic fields
            // with a block of its own: the channel count is a 32-bit field and
            // the sample rate a double, and the fixed 16-bit rate the classic
            // layout holds reads as 65536 for every one of them.
            audio.layout = channel_layout(be32(payload, 28 + 4 + 8).unwrap_or(2).max(1))?;
            audio.sample_rate = be64(payload, 28 + 4)
                .map(|bits| f64::from_bits(bits).round())
                .unwrap_or(0.0)
                .clamp(0.0, f64::from(u32::MAX)) as u32;
            28 + 36
        }
    };
    let mut config = None;
    // `wave/enda`: QuickTime's byte order for `in24`, `in32` and `fl32`, whose
    // default without the atom is big-endian.
    let mut little = false;
    for child in Boxes::new(payload.get(children..).unwrap_or(&[])) {
        let (child_kind, body) = child?;
        // QuickTime keeps the codec configuration one level down, in a `wave`
        // box beside the `frma` naming the format again: a reader that walks
        // past `wave` finds no `esds` at all and calls the track silent.
        if &child_kind == b"wave" {
            for inner in Boxes::new(body).map_while(|b| b.ok()) {
                if &inner.0 == b"enda" {
                    little = inner.1.get(1).copied().unwrap_or(0) != 0;
                }
            }
            if let Some(found) = wave_config(kind, body) {
                config = Some(found);
                break;
            }
            continue;
        }
        config = Some((child_kind, body));
        if !matches!(&child_kind, b"btrt" | b"chan") {
            break;
        }
    }
    // The channel count and output rate of an AAC or AC-3 track are the codec
    // configuration's, which is what edith and ffprobe both read; the entry's own
    // fields are fixed at two channels and carry the core's rate for HE-AAC.
    if kind == b"ac-3"
        && let Some((k, body)) = config
        && &k == b"dac3"
        && let Some(channels) = asc::ac3_channels(body)
    {
        audio.layout = channel_layout(u32::from(channels))?;
    }
    if matches!(kind, b"dtsc" | b"dtsh" | b"dtsl" | b"dtse")
        && let Some((k, body)) = config
        && &k == b"ddts"
        && let Some((rate, channels)) = asc::dts_config(body)
    {
        audio.sample_rate = rate;
        audio.layout = channel_layout(u32::from(channels))?;
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
            if let Some(esds) = &esds
                && esds.is_aac()
                && let Some((rate, channels)) =
                    esds.decoder_specific.as_deref().and_then(asc::aac_output)
            {
                audio.sample_rate = rate;
                audio.layout = channel_layout(u32::from(channels))?;
            }
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
                // ffmpeg stores DTS in an `mp4a` entry, the object type naming it.
                Some(esds)
                    if matches!(
                        esds.object_type,
                        object_type::DTS
                            | object_type::DTS_HD_HRA
                            | object_type::DTS_HD_MA
                            | object_type::DTS_EXPRESS
                    ) =>
                {
                    (CodecId::Dts, None)
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
        b"dtsc" | b"dtse" | b"dtsh" | b"dtsl" => (CodecId::Dts, config_bytes(config, b"ddts")),
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
        // AMR-NB: always 8 kHz mono whatever the entry says; `damr` body is the extradata.
        b"samr" => {
            audio.sample_rate = 8000;
            audio.layout = ChannelLayout::Mono;
            (CodecId::AmrNb, config_bytes(config, b"damr"))
        }
        // AMR-WB: always 16 kHz mono whatever the entry says.
        b"sawb" => {
            audio.sample_rate = 16000;
            audio.layout = ChannelLayout::Mono;
            (CodecId::AmrWb, config_bytes(config, b"damr"))
        }
        b"fLaC" => (CodecId::Flac, config_bytes(config, b"dfLa")),
        b"Opus" => (
            CodecId::Opus,
            config
                .filter(|(k, _)| k == b"dOps")
                .map(|(_, body)| opus_head(body)),
        ),
        b"ima4" => (CodecId::ImaAdpcmQt, None),
        b"alaw" => (CodecId::PcmAlaw, None),
        b"ulaw" => (CodecId::PcmMulaw, None),
        b"sowt" => (CodecId::PcmS16Le, None),
        b"twos" => (CodecId::PcmS16Be, None),
        b"raw " => (CodecId::PcmU8, None),
        b"in24" => (
            if little {
                CodecId::PcmS24Le
            } else {
                CodecId::PcmS24Be
            },
            None,
        ),
        b"in32" => (
            if little {
                CodecId::PcmS32Le
            } else {
                CodecId::PcmS32Be
            },
            None,
        ),
        b"fl32" => (
            if little {
                CodecId::PcmF32Le
            } else {
                CodecId::PcmF32Be
            },
            None,
        ),
        // QuickTime's version 2 linear PCM: the format flags and the bit depth
        // are in the description itself (bit 0 float, bit 1 big-endian).
        b"lpcm" if version == 2 => {
            let bits = be32(payload, 28 + 20).unwrap_or(0);
            let flags = be32(payload, 28 + 24).unwrap_or(0);
            let (float, big) = (flags & 1 != 0, flags & 2 != 0);
            let codec = match (float, bits, big) {
                (false, 16, false) => CodecId::PcmS16Le,
                (false, 16, true) => CodecId::PcmS16Be,
                (false, 24, false) => CodecId::PcmS24Le,
                (false, 24, true) => CodecId::PcmS24Be,
                (false, 32, false) => CodecId::PcmS32Le,
                (false, 32, true) => CodecId::PcmS32Be,
                (true, 32, false) => CodecId::PcmF32Le,
                (true, 32, true) => CodecId::PcmF32Be,
                _ => return Ok(None),
            };
            (codec, None)
        }
        _ => return Ok(None),
    };
    if is_wide_pcm(codec) {
        audio.bits_per_sample = Some(match codec {
            CodecId::PcmS24Le | CodecId::PcmS24Be => 24,
            _ => 32,
        });
    }
    Ok(Some(Entry {
        codec,
        extradata,
        video: None,
        audio: Some(audio),
        color_tags: Tags::default(),
        field_order: None,
    }))
}

/// The PCM codecs QuickTime writes a frame a sample for (`in24`, `in32`, `fl32`
/// and the version 2 `lpcm` carrying them).
fn is_wide_pcm(codec: CodecId) -> bool {
    matches!(
        codec,
        CodecId::PcmS24Le
            | CodecId::PcmS24Be
            | CodecId::PcmS32Le
            | CodecId::PcmS32Be
            | CodecId::PcmF32Le
            | CodecId::PcmF32Be
    )
}

/// Merge PCM stored a frame to a sample into packets of up to 4096 frames, so a
/// minute of sound is a few dozen packets and not three million. Only runs of
/// single-frame samples that follow each other in the file are merged.
fn group_pcm(samples: &mut Vec<Sample>, frame_bytes: u32) {
    const FRAMES: u32 = 4096;
    if frame_bytes == 0 || samples.iter().any(|s| s.size != frame_bytes) {
        return;
    }
    let mut out: Vec<Sample> = Vec::with_capacity(samples.len() / FRAMES as usize + 1);
    for s in samples.drain(..) {
        if let Some(last) = out.last_mut()
            && last.size + s.size <= FRAMES * frame_bytes
            && last.offset + u64::from(last.size) == s.offset
        {
            last.size += s.size;
            last.duration = last.duration.saturating_add(s.duration);
            continue;
        }
        out.push(s);
    }
    *samples = out;
}

/// The configuration box a `wave` holds for the sample entry `kind`, or
/// [`None`] where it holds none: `wave`'s other children are the `frma`
/// restating the format, an ISO-shaped copy of the sample entry itself, and a
/// zero-sized terminator, and a malformed one ends the walk rather than
/// erroring the whole track.
fn wave_config<'a>(kind: &FourCc, wave: &'a [u8]) -> Option<(FourCc, &'a [u8])> {
    let want: &FourCc = match kind {
        b"mp4a" => b"esds",
        b"ac-3" => b"dac3",
        b"ec-3" => b"dec3",
        b"alac" => b"alac",
        b"fLaC" => b"dfLa",
        b"Opus" => b"dOps",
        _ => return None,
    };
    for child in Boxes::new(wave) {
        let Ok((child_kind, body)) = child else {
            return None;
        };
        // The terminator: an eight-byte box of type zero, which is where a
        // `wave` ends even when the parent box says there is room for more.
        if child_kind == [0, 0, 0, 0] {
            return None;
        }
        if &child_kind == want {
            return Some((child_kind, body));
        }
    }
    None
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

fn video_codec(kind: &FourCc, video_track: bool) -> Option<(CodecId, &'static FourCc)> {
    Some(match kind {
        b"avc1" | b"avc3" => (CodecId::H264, b"avcC"),
        b"hvc1" | b"hev1" => (CodecId::H265, b"hvcC"),
        b"vp08" => (CodecId::Vp8, b"vpcC"),
        b"vp09" => (CodecId::Vp9, b"vpcC"),
        b"av01" => (CodecId::Av1, b"av1C"),
        // Motion JPEG: QuickTime `jpeg` (Photo JPEG) and `mjpa` (MJPEG-A) carry whole JFIF
        // frames and no configuration box. `mjpb` (MJPEG-B) packets have a different layout
        // (no SOI, marker-less blocks at header offsets); ec-mjpeg detects and rewraps them.
        // Uncompressed video; `raw ` is also the 8-bit PCM audio entry, so it needs a `vide` track.
        // The decoder (ec-riff `RawVideoDecoder`) reads the layout from the synthesized header.
        b"raw " | b"yuvs" if !video_track => return None,
        b"raw " | b"yuvs" | b"2vuy" | b"yuv2" | b"v210" | b"v410" | b"b16g" => {
            (CodecId::RawVideo, b"\0\0\0\0")
        }
        b"jpeg" | b"mjpa" | b"mjpb" => (CodecId::Mjpeg, b"\0\0\0\0"),
        // ProRes entries carry no configuration box: the frame header is self-describing.
        b"apco" | b"apcs" | b"apcn" | b"apch" | b"ap4h" | b"ap4x" => (CodecId::ProRes, b"\0\0\0\0"),
        // Avid DNxHD (`AVdn`) and DNxHR (`AVdh`); the optional `DNxD` box is
        // the only child worth keeping as extradata.
        b"AVdn" | b"AVdh" => (CodecId::Dnxhd, b"DNxD"),
        // MPEG-4 Part 2 Visual: `mp4v` carries the VOL header in the `esds`
        // DecoderSpecificInfo; the 3GPP/QuickTime H.263 entries (`s263`, `h263`)
        // carry no configuration the baseline decoder needs (`d263` is ignored).
        b"mp4v" => (CodecId::Mpeg4Part2, b"esds"),
        b"s263" | b"h263" => (CodecId::Mpeg4Part2, b"\0\0\0\0"),
        // DV: QuickTime sample entries for DV25/DVCPRO25 (`dvc `/`dvcp`/`dvpp`), DVCPRO50
        // (`dv5n`/`dv5p`) and DVCPRO HD (`dvhq`/`dvh3`/`dvh5`/`dvh6`/`dvhp`...). The frame is
        // self-describing: no configuration box.
        b"dvc " | b"dvcp" | b"dvpp" | b"dvcs" | b"dvis" | b"dv5n" | b"dv5p" | b"dv5q" | b"dvhq"
        | b"dvh1" | b"dvh2" | b"dvh3" | b"dvh4" | b"dvh5" | b"dvh6" | b"dvhp" | b"dvhs"
        | b"dvhn" | b"dvhr" => (CodecId::DvVideo, b"\0\0\0\0"),
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
    // Saturating: untrusted entries can sum past u64. A saturated total fails the
    // `try_into` below, so the rate comes out as None rather than a panic.
    let (samples, ticks) = stts.iter().fold((0u64, 0u64), |(n, t), &(count, delta)| {
        (
            n.saturating_add(u64::from(count)),
            t.saturating_add(u64::from(count).saturating_mul(u64::from(delta))),
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

/// The quarter turn a `tkhd` display matrix asks for, read the way the engine
/// above this crate reads it: the four linear 16.16 terms are normalised by the
/// matrix's own scale, so a copy of a quarter turn at another scale still reads
/// as one, and a column length or a shear past 1e-3 -- a mirror, an anisotropic
/// scale, a 45-degree quirk -- is [`Rotation::Other`] carrying the raw terms.
/// The angle is taken to half a degree, which is where a matrix a muxer rounded
/// to 16.16 lands.
fn display_rotation(a: i32, b: i32, c: i32, d: i32) -> Rotation {
    let fixed = |raw: i32| f64::from(raw) / 65536.0;
    let (fa, fb, fc, fd) = (fixed(a), fixed(b), fixed(c), fixed(d));
    let scale = fa.hypot(fb);
    let other = Rotation::Other { a, b, c, d };
    if scale == 0.0
        || (scale - fc.hypot(fd)).abs() > 1e-3
        || ((fa - fd) / scale).abs() > 1e-3
        || ((fb + fc) / scale).abs() > 1e-3
    {
        return other;
    }
    let degrees = fb.atan2(fa).to_degrees();
    let steps = (degrees / 90.0).round();
    if (degrees - steps * 90.0).abs() > 0.5 {
        return other;
    }
    match steps.rem_euclid(4.0) as u8 {
        0 => Rotation::None,
        1 => Rotation::Cw90,
        2 => Rotation::Cw180,
        _ => Rotation::Cw270,
    }
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

    #[test]
    fn avid_dnxhd_and_dnxhr_entries_map_to_dnxhd() {
        let mut payload = vec![0u8; 78];
        payload[24..26].copy_from_slice(&1920u16.to_be_bytes());
        payload[26..28].copy_from_slice(&1080u16.to_be_bytes());
        for kind in [b"AVdn", b"AVdh"] {
            let e = sample_entry(kind, &payload, false).unwrap().unwrap();
            assert_eq!(e.codec, CodecId::Dnxhd);
            let v = e.video.unwrap();
            assert_eq!((v.width, v.height), (1920, 1080));
        }
    }

    #[test]
    fn a_sound_description_stating_billions_of_channels_is_refused() {
        // Found by fuzz/demux_mp4: a version-2 entry's 32-bit channel count went
        // straight into a layout, one channel position allocated per count.
        let mut payload = vec![0u8; 72];
        payload[8..10].copy_from_slice(&2u16.to_be_bytes());
        payload[40..44].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
        assert!(sample_entry(b"mp4a", &payload, false).is_err());
        payload[40..44].copy_from_slice(&6u32.to_be_bytes());
        let entry = sample_entry(b"mp4a", &payload, false).unwrap().unwrap();
        assert_eq!(entry.audio.unwrap().layout.channel_count(), 6);
    }

    /// A QuickTime version-2 sound description, which no ffmpeg-written
    /// fixture in this corpus has: its 36-byte block states the sample rate as
    /// a double and the channel count as a 32-bit field, and the `esds` sits
    /// two levels down, inside the `wave`.
    #[test]
    fn quicktime_version_2_sound_entry_reads_its_rate_and_its_wave_esds() {
        fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            out
        }
        // esds: an ES descriptor naming AAC-LC with a two-byte config.
        let esds = boxed(
            b"esds",
            &[
                0, 0, 0, 0, // FullBox version and flags
                0x03, 0x16, 0x00, 0x01, 0x00, // ES_Descr
                // DecoderConfigDescr: AAC-LC, audio stream, zero buffer and
                // bitrates, then the config itself.
                0x04, 0x11, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x05, 0x02, 0x11,
                0x90, // DecoderSpecificInfo
            ],
        );
        let mut wave = boxed(b"frma", b"mp4a");
        wave.extend_from_slice(&boxed(b"mp4a", &[0, 0, 0, 0]));
        wave.extend_from_slice(&esds);
        wave.extend_from_slice(&[0, 0, 0, 8, 0, 0, 0, 0]); // terminator

        let mut entry = vec![0u8; 6];
        entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        entry.extend_from_slice(&2u16.to_be_bytes()); // version 2
        entry.extend_from_slice(&[0; 6]); // revision, vendor
        entry.extend_from_slice(&3u16.to_be_bytes()); // "always 3"
        entry.extend_from_slice(&16u16.to_be_bytes());
        entry.extend_from_slice(&0xFFFEu16.to_be_bytes());
        entry.extend_from_slice(&[0, 0]);
        entry.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // the fixed 65536
        entry.extend_from_slice(&72u32.to_be_bytes()); // sizeOfStructOnly
        entry.extend_from_slice(&48_000f64.to_bits().to_be_bytes());
        entry.extend_from_slice(&2u32.to_be_bytes()); // numAudioChannels
        entry.extend_from_slice(&0x7F00_0000u32.to_be_bytes());
        entry.extend_from_slice(&[0; 16]); // the four constant LPCM fields
        entry.extend_from_slice(&boxed(b"wave", &wave));

        let read = sample_entry(b"mp4a", &entry, false)
            .expect("entry parses")
            .expect("mp4a is a codec this build has an id for");
        let audio = read.audio.expect("a sound entry");
        assert_eq!(read.codec, CodecId::Aac);
        assert_eq!(audio.sample_rate, 48_000);
        assert_eq!(audio.layout.channel_count(), 2);
        assert_eq!(
            read.extradata.as_deref(),
            Some(&[0x11, 0x90][..]),
            "the AudioSpecificConfig inside wave/esds"
        );
    }

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

#[cfg(test)]
mod display_matrix_tests {
    use super::*;

    const ONE: i32 = 65_536;

    #[test]
    fn a_quarter_turn_reads_as_one_at_any_scale_and_with_rounding() {
        // `-display_rotation 90`'s matrix, as written, and at twice the scale.
        assert_eq!(display_rotation(0, -ONE, ONE, 0), Rotation::Cw270);
        assert_eq!(display_rotation(0, -2 * ONE, 2 * ONE, 0), Rotation::Cw270);
        // One 16.16 ulp of rounding on the terms: still the same quarter turn,
        // where the exact integer test in `ec_core` would call it `Other`.
        assert_eq!(display_rotation(1, -ONE, ONE - 1, 0), Rotation::Cw270);
        assert_eq!(display_rotation(ONE, 0, 0, ONE), Rotation::None);
        assert_eq!(display_rotation(-ONE, 0, 0, -ONE), Rotation::Cw180);
    }

    #[test]
    fn a_mirror_a_shear_or_a_no_matrix_is_refused_by_name() {
        let other = |a, b, c, d| Rotation::Other { a, b, c, d };
        // A horizontal mirror: negative determinant.
        assert_eq!(display_rotation(ONE, 0, 0, -ONE), other(ONE, 0, 0, -ONE));
        // A shear.
        assert_eq!(
            display_rotation(ONE, 0, ONE / 2, ONE),
            other(ONE, 0, ONE / 2, ONE)
        );
        // The zero matrix has no angle at all.
        assert_eq!(display_rotation(0, 0, 0, 0), other(0, 0, 0, 0));
    }
}
