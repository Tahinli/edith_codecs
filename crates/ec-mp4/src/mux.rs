//! The writer: `ftyp`, then one `mdat` the samples stream into, then the `moov`
//! built from what was written.

use std::io::{Seek, SeekFrom, Write};

use ec_core::{
    CodecId, ContentLight, Error, Muxer, Packet, Result, Rounding, StreamInfo, TimeBase,
};

use crate::boxes::{close, full_head, leaf, open};
use crate::esds::{Esds, object_type};

/// The clock the movie header counts in. Only `mvhd`/`tkhd` durations are in it;
/// every sample time is in its own track's timescale.
const MOVIE_TIMESCALE: u32 = 1_000;

/// One written sample, as the sample tables will state it.
struct Sample {
    size: u32,
    dts: i64,
    cts: i64,
    duration: u32,
    sync: bool,
}

/// One declared stream and everything written on it.
struct Track {
    info: StreamInfo,
    /// Ticks a second on this track: `time_base.den()`, so a `1001/24000` base
    /// becomes a 24000 clock counting 1001 a frame — exact, never rounded.
    timescale: u32,
    time_base: TimeBase,
    samples: Vec<Sample>,
    /// `(file offset, sample count)` per chunk: a run of this track's samples
    /// written back to back, which is what an interleaved file is made of.
    chunks: Vec<(u64, u32)>,
    /// Where the last sample of this track ended, to tell "same chunk" from "a
    /// new one".
    chunk_end: u64,
    title: Option<String>,
}

/// An MP4 writer over anything seekable.
///
/// **Layout: `mdat` first, `moov` last.** Samples are written the instant they
/// arrive, so muxing costs one pass and no temporary file, and the sample tables
/// — which cannot be written before the sizes they state are known — go at the
/// end. The cost is that a player streaming the file over HTTP has to reach the
/// end before it can start; a faststart (`moov` in front) writer would have to
/// copy the whole `mdat` a second time, and is not implemented here.
///
/// Interleaving is the caller's: packets are written in the order they are
/// handed over, and the chunk table follows that order exactly.
pub struct Mp4Muxer<W> {
    w: W,
    tracks: Vec<Track>,
    /// Current write offset.
    pos: u64,
    /// Offset of the `mdat` box header, patched with its real size at
    /// [`Mp4Muxer::finish`].
    mdat_at: u64,
    started: bool,
    finished: bool,
    title: Option<String>,
}

impl<W: Write + Seek> Mp4Muxer<W> {
    /// A writer over `w`, which is positioned at its start.
    pub fn new(mut w: W) -> Result<Mp4Muxer<W>> {
        let pos = w.seek(SeekFrom::Start(0))?;
        Ok(Mp4Muxer {
            w,
            tracks: Vec::new(),
            pos,
            mdat_at: 0,
            started: false,
            finished: false,
            title: None,
        })
    }

    /// Name the movie: a `moov/udta/name` box.
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = Some(title.into());
    }

    /// Name one track: a `trak/udta/name` box, which is what an mp4 calls a
    /// subtitle track in a player's menu.
    pub fn set_track_title(&mut self, stream: u32, title: impl Into<String>) -> Result<()> {
        let track = self
            .tracks
            .get_mut(stream as usize)
            .ok_or_else(|| Error::corrupt(format!("mp4: no stream {stream} to name")))?;
        track.title = Some(title.into());
        Ok(())
    }

    /// Say what a video stream's samples mean after the stream was declared.
    ///
    /// The `colr` box is part of the sample entry and the sample entry is
    /// written at [`Muxer::finish`], so this may be said any time before then —
    /// which is what a caller that learns the source's colour space after it has
    /// opened the file needs.
    pub fn set_color(&mut self, stream: u32, color: ec_core::ColorInfo) -> Result<()> {
        let track = self
            .tracks
            .get_mut(stream as usize)
            .ok_or_else(|| Error::corrupt(format!("mp4: no stream {stream} to colour")))?;
        match &mut track.info.params.media {
            ec_core::MediaParameters::Video(video) => video.color = color,
            _ => {
                return Err(Error::corrupt(format!(
                    "mp4: stream {stream} is not a video track"
                )));
            }
        }
        Ok(())
    }

    /// Take the writer back once the file is finished.
    pub fn into_inner(self) -> W {
        self.w
    }

    fn start(&mut self) -> Result<()> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        let mut head = Vec::with_capacity(64);
        let at = open(&mut head, b"ftyp");
        head.extend_from_slice(b"isom");
        head.extend_from_slice(&512u32.to_be_bytes());
        head.extend_from_slice(b"isom");
        head.extend_from_slice(b"iso2");
        for track in &self.tracks {
            if let Some(brand) = video_entry(track.info.params.codec) {
                head.extend_from_slice(brand);
            }
        }
        head.extend_from_slice(b"mp41");
        close(&mut head, at);
        self.mdat_at = head.len() as u64;
        // The 64-bit form of an `mdat` header, always: a two-hour 4K export goes
        // past 4 GB and the placeholder cannot grow later without moving every
        // sample in the file.
        head.extend_from_slice(&1u32.to_be_bytes());
        head.extend_from_slice(b"mdat");
        head.extend_from_slice(&0u64.to_be_bytes());
        self.w.write_all(&head)?;
        self.pos = head.len() as u64;
        Ok(())
    }

    /// Sample durations, resolved: a packet that stated one keeps it, and one
    /// that did not gets the step to the next sample — which is what the last
    /// sample has no next for, so it repeats the step before it.
    fn resolve_durations(track: &mut Track) {
        for i in 0..track.samples.len() {
            if track.samples[i].duration != 0 {
                continue;
            }
            let step = match track.samples.get(i + 1) {
                Some(next) => next.dts - track.samples[i].dts,
                None => i
                    .checked_sub(1)
                    .map_or(0, |prev| i64::from(track.samples[prev].duration)),
            };
            track.samples[i].duration = u32::try_from(step.max(0)).unwrap_or(0);
        }
    }

    fn write_moov(&mut self) -> Result<()> {
        let mut moov = Vec::with_capacity(4096);
        let at = open(&mut moov, b"moov");
        let movie_base = TimeBase::from_rate(MOVIE_TIMESCALE);
        let duration = self
            .tracks
            .iter()
            .map(|t| {
                t.time_base
                    .rescale(track_ticks(t), movie_base, Rounding::Up)
            })
            .max()
            .unwrap_or(0);

        let mvhd = open(&mut moov, b"mvhd");
        full_head(&mut moov, 0, 0);
        moov.extend_from_slice(&0u32.to_be_bytes()); // creation
        moov.extend_from_slice(&0u32.to_be_bytes()); // modification
        moov.extend_from_slice(&MOVIE_TIMESCALE.to_be_bytes());
        moov.extend_from_slice(&(duration.max(0) as u32).to_be_bytes());
        moov.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
        moov.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        moov.extend_from_slice(&[0; 2 + 8]);
        moov.extend_from_slice(&UNITY_MATRIX);
        moov.extend_from_slice(&[0; 24]); // pre_defined
        moov.extend_from_slice(&(self.tracks.len() as u32 + 1).to_be_bytes());
        close(&mut moov, mvhd);

        for i in 0..self.tracks.len() {
            self.write_trak(&mut moov, i, duration)?;
        }
        if let Some(title) = &self.title {
            let udta = open(&mut moov, b"udta");
            leaf(&mut moov, b"name", title.as_bytes());
            close(&mut moov, udta);
        }
        close(&mut moov, at);
        self.w.write_all(&moov)?;
        self.pos += moov.len() as u64;
        Ok(())
    }

    fn write_trak(&self, out: &mut Vec<u8>, i: usize, movie_duration: i64) -> Result<()> {
        let track = &self.tracks[i];
        let codec = track.info.params.codec;
        let at = open(out, b"trak");

        let tkhd = open(out, b"tkhd");
        full_head(out, 0, 0x7); // enabled, in movie, in preview
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(i as u32 + 1).to_be_bytes()); // track_ID
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(movie_duration.max(0) as u32).to_be_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&0u16.to_be_bytes()); // layer
        out.extend_from_slice(&0u16.to_be_bytes()); // alternate group
        let volume: u16 = match codec.media_type() {
            ec_core::MediaType::Audio => 0x0100,
            _ => 0,
        };
        out.extend_from_slice(&volume.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&UNITY_MATRIX);
        let (width, height) = match track.info.params.video() {
            Some(v) => (v.width, v.height),
            None => (0, 0),
        };
        out.extend_from_slice(&(width << 16).to_be_bytes());
        out.extend_from_slice(&(height << 16).to_be_bytes());
        close(out, tkhd);

        // An `elst`, where the first picture is not the first *decoded* one: a
        // reordered stream's earliest composition time is the reorder delay, and
        // without an edit saying so every player starts the track that much
        // late. This is where a demuxer's `media_time` comes from, and it is a
        // shift, never a trim.
        let priming = presentation_start(track);
        if priming > 0 {
            let edts = open(out, b"edts");
            let elst = open(out, b"elst");
            full_head(out, 0, 0);
            out.extend_from_slice(&1u32.to_be_bytes());
            let movie_base = TimeBase::from_rate(MOVIE_TIMESCALE);
            let segment =
                track
                    .time_base
                    .rescale(track_ticks(track) - priming, movie_base, Rounding::Up);
            out.extend_from_slice(&(segment.max(0) as u32).to_be_bytes());
            out.extend_from_slice(&(priming as u32).to_be_bytes());
            out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // media rate 1.0
            close(out, elst);
            close(out, edts);
        }

        let mdia = open(out, b"mdia");
        let mdhd = open(out, b"mdhd");
        full_head(out, 0, 0);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&track.timescale.to_be_bytes());
        out.extend_from_slice(&(track_ticks(track).max(0) as u32).to_be_bytes());
        out.extend_from_slice(&pack_language(track.info.language.as_deref()).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
        close(out, mdhd);

        let (handler, handler_name) = match codec.media_type() {
            ec_core::MediaType::Video => (b"vide", "VideoHandler"),
            ec_core::MediaType::Audio => (b"soun", "SoundHandler"),
            ec_core::MediaType::Subtitle => (b"sbtl", "SubtitleHandler"),
        };
        let hdlr = open(out, b"hdlr");
        full_head(out, 0, 0);
        out.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
        out.extend_from_slice(handler);
        out.extend_from_slice(&[0; 12]); // reserved
        out.extend_from_slice(handler_name.as_bytes());
        out.push(0);
        close(out, hdlr);

        let minf = open(out, b"minf");
        match codec.media_type() {
            ec_core::MediaType::Video => {
                let vmhd = open(out, b"vmhd");
                full_head(out, 0, 1);
                out.extend_from_slice(&[0; 8]); // graphicsmode + opcolor
                close(out, vmhd);
            }
            ec_core::MediaType::Audio => {
                let smhd = open(out, b"smhd");
                full_head(out, 0, 0);
                out.extend_from_slice(&[0; 4]); // balance + reserved
                close(out, smhd);
            }
            ec_core::MediaType::Subtitle => {
                let nmhd = open(out, b"nmhd");
                full_head(out, 0, 0);
                close(out, nmhd);
            }
        }
        let dinf = open(out, b"dinf");
        let dref = open(out, b"dref");
        full_head(out, 0, 0);
        out.extend_from_slice(&1u32.to_be_bytes());
        let url = open(out, b"url ");
        full_head(out, 0, 1); // flag 1: the media is in this very file
        close(out, url);
        close(out, dref);
        close(out, dinf);

        let stbl = open(out, b"stbl");
        let stsd = open(out, b"stsd");
        full_head(out, 0, 0);
        out.extend_from_slice(&1u32.to_be_bytes());
        sample_entry(out, &track.info)?;
        close(out, stsd);
        sample_tables(out, track);
        close(out, stbl);
        close(out, minf);
        close(out, mdia);

        if let Some(title) = &track.title {
            let udta = open(out, b"udta");
            leaf(out, b"name", title.as_bytes());
            close(out, udta);
        }
        close(out, at);
        Ok(())
    }
}

impl<W: Write + Seek + Send> Muxer for Mp4Muxer<W> {
    /// Declare a stream — **at any point before [`Muxer::finish`]**, which is
    /// more than the trait promises. Samples are written where they arrive and
    /// the chunk table follows, so a track declared after the picture is
    /// already in the `mdat` costs nothing: its `trak` is built at the end with
    /// every other. That is what lets an export mix and encode its sound *while*
    /// it writes the picture, and add its subtitles once the film's length is
    /// known.
    fn add_stream(&mut self, info: StreamInfo) -> Result<u32> {
        let timescale = u32::try_from(info.time_base.den()).map_err(|_| {
            Error::corrupt(format!(
                "mp4: a time base of 1/{} does not fit a 32-bit timescale",
                info.time_base.den()
            ))
        })?;
        if timescale == 0 {
            return Err(Error::corrupt("mp4: a stream with a zero timescale"));
        }
        let index = self.tracks.len() as u32;
        self.tracks.push(Track {
            info,
            timescale,
            time_base: TimeBase::from_rate(timescale),
            samples: Vec::new(),
            chunks: Vec::new(),
            chunk_end: u64::MAX,
            title: None,
        });
        Ok(index)
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        self.start()?;
        let pos = self.pos;
        let track = self
            .tracks
            .get_mut(packet.stream as usize)
            .ok_or_else(|| Error::corrupt(format!("mp4: no stream {}", packet.stream)))?;
        let to = track.time_base;
        let at = |ticks: i64| packet.time_base.rescale(ticks, to, Rounding::Nearest);
        let dts = match (packet.dts, packet.pts) {
            (Some(dts), _) => at(dts),
            (None, Some(pts)) => at(pts),
            // A stream that states no time at all is laid end to end, which is
            // what a constant-rate audio track is anyway.
            (None, None) => track
                .samples
                .last()
                .map_or(0, |s| s.dts + i64::from(s.duration)),
        };
        let cts = packet.pts.map_or(dts, at);
        let size = u32::try_from(packet.data.len())
            .map_err(|_| Error::corrupt("mp4: a sample over 4 GB"))?;
        track.samples.push(Sample {
            size,
            dts,
            cts,
            duration: packet.duration.map_or(0, |d| at(d).max(0) as u32),
            sync: packet.flags.keyframe,
        });
        match track.chunk_end == pos {
            true => {
                if let Some(chunk) = track.chunks.last_mut() {
                    chunk.1 += 1;
                }
            }
            false => track.chunks.push((pos, 1)),
        }
        track.chunk_end = pos + u64::from(size);
        self.w.write_all(&packet.data)?;
        self.pos = pos + u64::from(size);
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.start()?;
        // The mdat's real size, into the 64-bit field left for it.
        let mdat_len = self.pos - self.mdat_at;
        self.w.seek(SeekFrom::Start(self.mdat_at + 8))?;
        self.w.write_all(&mdat_len.to_be_bytes())?;
        self.w.seek(SeekFrom::Start(self.pos))?;
        for track in &mut self.tracks {
            Mp4Muxer::<W>::resolve_durations(track);
        }
        self.write_moov()?;
        self.w.flush()?;
        Ok(())
    }
}

/// Total media ticks of a track: from the first sample's decode time to the end
/// of the last one. A span rather than an endpoint, because a track handed over
/// with an edit list already applied starts at a negative decode time and its
/// duration is still the whole of it.
fn track_ticks(track: &Track) -> i64 {
    let first = track.samples.first().map_or(0, |s| s.dts);
    track
        .samples
        .last()
        .map_or(0, |s| s.dts + i64::from(s.duration) - first)
}

/// Where the first *picture* is on the written timeline: decode times start at
/// zero in the file (`stts` states durations, not instants), so this is the
/// smallest composition offset — the reorder delay of a stream with B-frames and
/// zero for everything else.
fn presentation_start(track: &Track) -> i64 {
    let first = track.samples.first().map_or(0, |s| s.dts);
    track
        .samples
        .iter()
        .map(|s| s.cts - first)
        .min()
        .unwrap_or(0)
}

/// The `stts`/`ctts`/`stss`/`stsc`/`stsz` and chunk-offset tables of one track.
fn sample_tables(out: &mut Vec<u8>, track: &Track) {
    // stts: runs of equal durations, which for constant-rate video is one entry
    // for the whole film.
    let stts = open(out, b"stts");
    full_head(out, 0, 0);
    let count_at = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let mut runs = 0u32;
    let mut i = 0;
    while i < track.samples.len() {
        let delta = track.samples[i].duration;
        let mut run = 0u32;
        while i < track.samples.len() && track.samples[i].duration == delta {
            run += 1;
            i += 1;
        }
        out.extend_from_slice(&run.to_be_bytes());
        out.extend_from_slice(&delta.to_be_bytes());
        runs += 1;
    }
    out[count_at..count_at + 4].copy_from_slice(&runs.to_be_bytes());
    close(out, stts);

    // ctts, only where something is actually reordered.
    let offsets: Vec<i64> = track.samples.iter().map(|s| s.cts - s.dts).collect();
    if offsets.iter().any(|&o| o != 0) {
        let negative = offsets.iter().any(|&o| o < 0);
        let ctts = open(out, b"ctts");
        // Version 1 states the offset as signed, which is the only way to say
        // that a picture is shown *before* it is decoded.
        full_head(out, u8::from(negative), 0);
        let count_at = out.len();
        out.extend_from_slice(&0u32.to_be_bytes());
        let mut runs = 0u32;
        let mut i = 0;
        while i < offsets.len() {
            let offset = offsets[i];
            let mut run = 0u32;
            while i < offsets.len() && offsets[i] == offset {
                run += 1;
                i += 1;
            }
            out.extend_from_slice(&run.to_be_bytes());
            out.extend_from_slice(&(offset as i32).to_be_bytes());
            runs += 1;
        }
        out[count_at..count_at + 4].copy_from_slice(&runs.to_be_bytes());
        close(out, ctts);
    }

    // stss, only where some samples are not sync ones: no box at all means
    // every sample is a random access point, which is the truth for audio.
    if track.samples.iter().any(|s| !s.sync) {
        let stss = open(out, b"stss");
        full_head(out, 0, 0);
        let syncs: Vec<u32> = track
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.sync)
            .map(|(i, _)| i as u32 + 1)
            .collect();
        out.extend_from_slice(&(syncs.len() as u32).to_be_bytes());
        for n in syncs {
            out.extend_from_slice(&n.to_be_bytes());
        }
        close(out, stss);
    }

    // stsc: runs of chunks holding the same number of samples.
    let stsc = open(out, b"stsc");
    full_head(out, 0, 0);
    let count_at = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let mut runs = 0u32;
    let mut first = 0usize;
    while first < track.chunks.len() {
        let per = track.chunks[first].1;
        out.extend_from_slice(&(first as u32 + 1).to_be_bytes());
        out.extend_from_slice(&per.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index
        runs += 1;
        first += 1;
        while first < track.chunks.len() && track.chunks[first].1 == per {
            first += 1;
        }
    }
    out[count_at..count_at + 4].copy_from_slice(&runs.to_be_bytes());
    close(out, stsc);

    // stsz: one size for all, where they really are all one size.
    let stsz = open(out, b"stsz");
    full_head(out, 0, 0);
    let uniform = track
        .samples
        .first()
        .map(|f| f.size)
        .filter(|&size| track.samples.iter().all(|s| s.size == size))
        .unwrap_or(0);
    out.extend_from_slice(&uniform.to_be_bytes());
    out.extend_from_slice(&(track.samples.len() as u32).to_be_bytes());
    if uniform == 0 {
        for sample in &track.samples {
            out.extend_from_slice(&sample.size.to_be_bytes());
        }
    }
    close(out, stsz);

    // co64 where the file is big enough to need it, stco where it is not.
    let big = track.chunks.iter().any(|&(at, _)| at > u64::from(u32::MAX));
    let kind = if big { b"co64" } else { b"stco" };
    let chunks = open(out, kind);
    full_head(out, 0, 0);
    out.extend_from_slice(&(track.chunks.len() as u32).to_be_bytes());
    for &(at, _) in &track.chunks {
        match big {
            true => out.extend_from_slice(&at.to_be_bytes()),
            false => out.extend_from_slice(&(at as u32).to_be_bytes()),
        }
    }
    close(out, chunks);
}

/// The sample entry name and configuration box of a video codec.
fn video_entry(codec: CodecId) -> Option<&'static [u8; 4]> {
    Some(match codec {
        CodecId::H264 => b"avc1",
        // `hvc1` and not `hev1`: the parameter sets are in the `hvcC`, which is
        // exactly what the `hvc1` name promises a reader.
        CodecId::H265 => b"hvc1",
        CodecId::Av1 => b"av01",
        CodecId::Vp9 => b"vp09",
        CodecId::Vp8 => b"vp08",
        _ => return None,
    })
}

fn config_box(codec: CodecId) -> &'static [u8; 4] {
    match codec {
        CodecId::H264 => b"avcC",
        CodecId::H265 => b"hvcC",
        CodecId::Av1 => b"av1C",
        _ => b"vpcC",
    }
}

/// One `stsd` entry for `info`.
fn sample_entry(out: &mut Vec<u8>, info: &StreamInfo) -> Result<()> {
    let codec = info.params.codec;
    let extradata = info.params.extradata.as_deref();
    if let Some(name) = video_entry(codec) {
        let video = info.params.video().ok_or_else(|| {
            Error::corrupt("mp4: a video codec declared without video parameters")
        })?;
        let config = extradata.ok_or_else(|| {
            Error::unsupported(
                format!("mp4: a {} track with no codec configuration", codec.name()),
                "an ISOBMFF video sample entry has to carry its avcC/hvcC/av1C/vpcC record",
            )
        })?;
        let at = open(out, name);
        out.extend_from_slice(&[0; 6]); // reserved
        out.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        out.extend_from_slice(&[0; 16]); // pre_defined + reserved
        out.extend_from_slice(&(video.width as u16).to_be_bytes());
        out.extend_from_slice(&(video.height as u16).to_be_bytes());
        out.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi
        out.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // reserved
        out.extend_from_slice(&1u16.to_be_bytes()); // frame_count
        out.extend_from_slice(&[0; 32]); // compressorname
        out.extend_from_slice(&0x0018u16.to_be_bytes()); // depth: colour, no alpha
        out.extend_from_slice(&(-1i16).to_be_bytes()); // pre_defined
        leaf(out, config_box(codec), config);
        colour_boxes(out, video.color, video.light);
        if let Some(sar) = video.sample_aspect_ratio {
            let pasp = open(out, b"pasp");
            out.extend_from_slice(&(sar.num() as u32).to_be_bytes());
            out.extend_from_slice(&(sar.den() as u32).to_be_bytes());
            close(out, pasp);
        }
        close(out, at);
        return Ok(());
    }
    if codec == CodecId::Tx3g {
        let at = open(out, b"tx3g");
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&1u16.to_be_bytes());
        match extradata {
            // Round-tripped from another mp4: the style it already stated.
            Some(style) if style.len() >= 30 => out.extend_from_slice(style),
            _ => {
                out.extend_from_slice(&0u32.to_be_bytes()); // displayFlags
                out.push(1); // horizontal justification: centred
                out.push(0xFF); // vertical: bottom
                out.extend_from_slice(&[0, 0, 0, 0]); // background: transparent
                out.extend_from_slice(&[0; 8]); // text box: the whole picture
                out.extend_from_slice(&0u16.to_be_bytes()); // startChar
                out.extend_from_slice(&0u16.to_be_bytes()); // endChar
                out.extend_from_slice(&1u16.to_be_bytes()); // font-ID
                out.push(0); // face-style-flags
                out.push(18); // font-size
                out.extend_from_slice(&[0xFF; 4]); // text colour: opaque white
                let ftab = open(out, b"ftab");
                out.extend_from_slice(&1u16.to_be_bytes());
                out.extend_from_slice(&1u16.to_be_bytes());
                out.push(5);
                out.extend_from_slice(b"Serif");
                close(out, ftab);
            }
        }
        close(out, at);
        return Ok(());
    }

    let (name, audio) = match info.params.audio() {
        Some(audio) => (audio_entry(codec)?, audio),
        None => {
            return Err(Error::unsupported(
                format!("mp4: a {} track", codec.name()),
                "this writer has a sample entry for video, AAC-family audio and tx3g text only; \
                 Matroska carries the rest",
            ));
        }
    };
    let at = open(out, name);
    out.extend_from_slice(&[0; 6]);
    out.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    out.extend_from_slice(&[0; 8]); // version, revision, vendor
    out.extend_from_slice(&(audio.layout.channel_count() as u16).to_be_bytes());
    out.extend_from_slice(&(audio.bits_per_sample.unwrap_or(16) as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    // 16.16, so a rate over 65535 is stated as its low half — which is what
    // every writer does and what the `mdhd` timescale says exactly anyway.
    out.extend_from_slice(&((audio.sample_rate as u16 as u32) << 16).to_be_bytes());
    match codec {
        CodecId::Aac => {
            let asc = extradata.ok_or_else(|| {
                Error::unsupported(
                    "mp4: an AAC track with no AudioSpecificConfig",
                    "an esds descriptor is the only place an mp4 states an AAC track's setup",
                )
            })?;
            let esds = open(out, b"esds");
            out.extend_from_slice(&Esds::aac(asc).write());
            close(out, esds);
        }
        CodecId::Mp3 => {
            let mut mp3 = Esds::aac(&[][..]);
            mp3.object_type = object_type::MPEG1_AUDIO;
            mp3.decoder_specific = None;
            let esds = open(out, b"esds");
            out.extend_from_slice(&mp3.write());
            close(out, esds);
        }
        CodecId::Ac3 | CodecId::EAc3 | CodecId::Flac | CodecId::Opus | CodecId::Alac => {
            let config = extradata.ok_or_else(|| {
                Error::unsupported(
                    format!("mp4: a {} track with no codec configuration", codec.name()),
                    "its sample entry has to carry the codec's own configuration box",
                )
            })?;
            match codec {
                // ALAC's cookie is carried with its box header, so it is written
                // back exactly as it was read.
                CodecId::Alac if config.len() > 8 && &config[4..8] == b"alac" => {
                    out.extend_from_slice(config)
                }
                CodecId::Opus => leaf(out, b"dOps", &dops(config)),
                _ => leaf(out, audio_config_box(codec), config),
            }
        }
        _ => {}
    }
    close(out, at);
    Ok(())
}

/// `colr`, and the two HDR boxes where the grade stated them — metadata a remux
/// must not quietly drop, because nothing downstream can reconstruct it.
fn colour_boxes(out: &mut Vec<u8>, color: ec_core::ColorInfo, light: ContentLight) {
    if color != ec_core::ColorInfo::default() {
        let colr = open(out, b"colr");
        out.extend_from_slice(b"nclx");
        out.extend_from_slice(&u16::from(color.primaries).to_be_bytes());
        out.extend_from_slice(&u16::from(color.transfer).to_be_bytes());
        out.extend_from_slice(&u16::from(color.matrix).to_be_bytes());
        out.push(u8::from(color.full_range) << 7);
        close(out, colr);
    }
    if light.mastering_max.is_some() || light.mastering_min.is_some() {
        let mdcv = open(out, b"mdcv");
        // Display primaries and white point: not carried by [`ContentLight`],
        // which holds the luminances a tone map needs. BT.2020 is what an HDR
        // grade is mastered on and what every writer states here.
        for xy in BT2020_PRIMARIES {
            out.extend_from_slice(&xy.to_be_bytes());
        }
        let nits = |v: Option<f32>| ((v.unwrap_or(0.0) * 10_000.0) as u32).to_be_bytes();
        out.extend_from_slice(&nits(light.mastering_max));
        out.extend_from_slice(&nits(light.mastering_min));
        close(out, mdcv);
    }
    if light.max_cll.is_some() || light.max_fall.is_some() {
        let clli = open(out, b"clli");
        out.extend_from_slice(&(light.max_cll.unwrap_or(0.0) as u16).to_be_bytes());
        out.extend_from_slice(&(light.max_fall.unwrap_or(0.0) as u16).to_be_bytes());
        close(out, clli);
    }
}

/// BT.2020 green, blue, red and D65, in the 0.00002 units an `mdcv` states them.
const BT2020_PRIMARIES: [u16; 8] = [8500, 39850, 6550, 2300, 35400, 14600, 15635, 16450];

const UNITY_MATRIX: [u8; 36] = {
    let mut m = [0u8; 36];
    m[0] = 0x00;
    m[1] = 0x01;
    m[16] = 0x00;
    m[17] = 0x01;
    m[32] = 0x40;
    m
};

fn audio_entry(codec: CodecId) -> Result<&'static [u8; 4]> {
    Ok(match codec {
        CodecId::Aac | CodecId::Mp3 => b"mp4a",
        CodecId::Ac3 => b"ac-3",
        CodecId::EAc3 => b"ec-3",
        CodecId::Alac => b"alac",
        CodecId::Flac => b"fLaC",
        CodecId::Opus => b"Opus",
        CodecId::PcmS16Le => b"sowt",
        CodecId::PcmS16Be => b"twos",
        CodecId::PcmU8 => b"raw ",
        _ => {
            return Err(Error::unsupported(
                format!("mp4: a {} audio track", codec.name()),
                "no ISOBMFF sample entry is defined for it",
            ));
        }
    })
}

fn audio_config_box(codec: CodecId) -> &'static [u8; 4] {
    match codec {
        CodecId::Ac3 => b"dac3",
        CodecId::EAc3 => b"dec3",
        CodecId::Flac => b"dfLa",
        _ => b"dOps",
    }
}

/// The `dOps` payload from an `OpusHead`: same fields, opposite byte order, and
/// without the magic — the inverse of what the demuxer does on the way in.
fn dops(head: &[u8]) -> Vec<u8> {
    let body = match head.starts_with(b"OpusHead") {
        true => &head[8..],
        false => head,
    };
    let le16 = |at: usize| {
        u16::from_le_bytes([
            body.get(at).copied().unwrap_or(0),
            body.get(at + 1).copied().unwrap_or(0),
        ])
    };
    let mut out = Vec::with_capacity(body.len());
    out.push(0); // dOps version
    out.push(body.get(1).copied().unwrap_or(2)); // channel count
    out.extend_from_slice(&le16(2).to_be_bytes()); // pre-skip
    let rate = u32::from_le_bytes([
        body.get(4).copied().unwrap_or(0),
        body.get(5).copied().unwrap_or(0),
        body.get(6).copied().unwrap_or(0),
        body.get(7).copied().unwrap_or(0),
    ]);
    out.extend_from_slice(&rate.to_be_bytes());
    out.extend_from_slice(&le16(8).to_be_bytes()); // output gain
    out.extend_from_slice(&body[10.min(body.len())..]);
    out
}

/// Three lowercase letters into the five-bit-a-letter packing an `mdhd` states a
/// language in; `und` where the track never said.
fn pack_language(language: Option<&str>) -> u16 {
    let tag = language.filter(|l| l.len() == 3).unwrap_or("und");
    tag.bytes().fold(0u16, |packed, b| {
        (packed << 5) | u16::from(b.to_ascii_lowercase().wrapping_sub(0x60) & 0x1F)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_packing_is_the_demuxers_inverse() {
        for tag in ["tur", "eng", "und"] {
            let packed = pack_language(Some(tag));
            let back: String = (0..3)
                .rev()
                .map(|i| char::from(0x60 + ((packed >> (i * 5)) & 0x1F) as u8))
                .collect();
            assert_eq!(back, tag);
        }
        assert_eq!(pack_language(None), pack_language(Some("und")));
        assert_eq!(pack_language(Some("nonsense")), pack_language(Some("und")));
    }

    #[test]
    fn an_opus_head_round_trips_through_dops() {
        let head = crate::demux::opus_head(&[0, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 1, 2, 3]);
        let back = dops(&head);
        assert_eq!(back[0], 0, "dOps version");
        assert_eq!(back[1], 2, "channels");
        assert_eq!(u16::from_be_bytes([back[2], back[3]]), 312);
        assert_eq!(
            u32::from_be_bytes([back[4], back[5], back[6], back[7]]),
            48_000
        );
        assert_eq!(&back[10..], &[1, 2, 3], "mapping family and table");
    }
}
