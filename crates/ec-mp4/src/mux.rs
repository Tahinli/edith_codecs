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
    /// An audio encoder's priming: samples it emits before the first audible
    /// one (an AAC-LC encoder's 1024-sample delay), in this track's
    /// timescale. Written the same way as a video track's reorder delay: an
    /// `edts/elst` media_time shift, never a trim.
    audio_delay: i64,
    /// An explicit edit set by [`Mp4Muxer::set_edit`]: `(media_time, duration)`
    /// in this track's timescale. When set it replaces the priming shift and
    /// the track's presentation length, so the edit plays exactly this span.
    edit: Option<(u32, u32)>,
    /// Written as a `fiel` box on a video track. [`Some`]`(Progressive)` unless
    /// [`Mp4Muxer::set_field_order`] says otherwise; [`None`] writes no box.
    field_order: Option<ec_core::Interlace>,
    /// Write a QuickTime `chan` channel-layout box in an audio sample entry
    /// that would not otherwise carry one ([`Mp4Muxer::set_channel_layout_box`]).
    chan: bool,
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
    /// The `ftyp` brands a caller chose, as `(major, compatible)`. `None` is the
    /// default: `isom` with the video codecs' own brands and `mp41`.
    brands: Option<([u8; 4], Vec<[u8; 4]>)>,
    /// Chapter marks as `(start in ns, title)`, sorted by start. Written as a
    /// Nero `chpl` in `moov/udta` and as a QuickTime text track.
    chapters: Vec<(u64, String)>,
}

/// The chapter text track as it is laid out in the file: its samples sit in the
/// `mdat`, one per chapter, after the media.
struct ChapterText {
    /// The first chapter's start in movie milliseconds. The text track's media
    /// time starts at zero, so an edit list shifts it to this point.
    first_ms: i64,
    /// The end of the film in movie milliseconds, which the last chapter runs to.
    end_ms: i64,
    /// `(duration in ms, sample size in bytes)` per chapter.
    samples: Vec<(u32, u32)>,
    /// File offset of the first sample; the samples are contiguous.
    offset: u64,
}

impl<W: Write + Seek> Mp4Muxer<W> {
    /// A writer over `w`, which is positioned at its start.
    pub fn new(w: W) -> Result<Mp4Muxer<W>> {
        Self::build(w, None)
    }

    /// A writer whose `ftyp` states `major` and then `compatible`, in that
    /// order, instead of the default brands. An audio-only file names its own
    /// kind here: `M4A ` for an `.m4a`, `isom` for an `.mp4` of sound.
    pub fn with_brands(w: W, major: [u8; 4], compatible: &[[u8; 4]]) -> Result<Mp4Muxer<W>> {
        Self::build(w, Some((major, compatible.to_vec())))
    }

    fn build(mut w: W, brands: Option<([u8; 4], Vec<[u8; 4]>)>) -> Result<Mp4Muxer<W>> {
        let pos = w.seek(SeekFrom::Start(0))?;
        Ok(Mp4Muxer {
            w,
            tracks: Vec::new(),
            pos,
            mdat_at: 0,
            started: false,
            finished: false,
            title: None,
            brands,
            chapters: Vec::new(),
        })
    }

    /// Name the movie: a `moov/udta/name` box.
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = Some(title.into());
    }

    /// Set the chapter marks: `(start in ns, title)` each. They are sorted by
    /// start (stably, so equal starts keep the order given) and replace any set
    /// before. The file gets both a Nero `chpl` box in `moov/udta` (version 1,
    /// 100 ns units, titles cut to 255 bytes) and a QuickTime text track with one
    /// sample per chapter, referenced from the first video track (or the first
    /// audio track, else the first track). Each chapter runs to the next one's
    /// start, and the last to the end of the film. An empty list writes neither.
    ///
    /// More than 255 chapters: the `chpl` box counts them in one byte, so it is left out
    /// and the chapter text track, which has no such limit, carries all of them.
    pub fn set_chapters(&mut self, chapters: &[(u64, String)]) -> Result<()> {
        let mut sorted = chapters.to_vec();
        sorted.sort_by_key(|(start, _)| *start);
        self.chapters = sorted;
        Ok(())
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

    /// State a video stream's HDR light levels after the stream was declared:
    /// the `mdcv` (mastering display colour volume) and `clli` (content light
    /// level) boxes of its sample entry, written at [`Muxer::finish`]. `mdcv`
    /// is written when `light` has a mastering luminance or primaries (BT.2020 /
    /// D65 where only the luminances are given), `clli` when it has MaxCLL or
    /// MaxFALL. Units are those of [`ec_core::ContentLight`]: nits, with
    /// chromaticities in increments of 0.00002. A track never told writes no
    /// such box.
    pub fn set_content_light(&mut self, stream: u32, light: ContentLight) -> Result<()> {
        let track = self
            .tracks
            .get_mut(stream as usize)
            .ok_or_else(|| Error::corrupt(format!("mp4: no stream {stream} to light")))?;
        match &mut track.info.params.media {
            ec_core::MediaParameters::Video(video) => video.light = light,
            _ => {
                return Err(Error::corrupt(format!(
                    "mp4: stream {stream} is not a video track"
                )));
            }
        }
        Ok(())
    }

    /// Write a QuickTime `chan` channel-layout box in `stream`'s audio sample
    /// entry (mono, stereo and 5.1 have one; other layouts write none). The
    /// QuickTime PCM entries (`in24`, `in32`, `fl32`) always carry it, as
    /// ffmpeg writes them; this adds it to the entries that otherwise do not,
    /// such as `sowt`. Without this call those are unchanged.
    pub fn set_channel_layout_box(&mut self, stream: u32, on: bool) -> Result<()> {
        let track = self
            .tracks
            .get_mut(stream as usize)
            .ok_or_else(|| Error::corrupt(format!("mp4: no stream {stream} to lay out")))?;
        track.chan = on;
        Ok(())
    }

    /// Say how a video stream's pictures are scanned: written as a `fiel` box
    /// at [`Muxer::finish`]. Every video track is `Some(Progressive)` until
    /// told otherwise; [`None`] writes no box (field order unknown).
    pub fn set_field_order(
        &mut self,
        stream: u32,
        order: Option<ec_core::Interlace>,
    ) -> Result<()> {
        let track = self
            .tracks
            .get_mut(stream as usize)
            .ok_or_else(|| Error::corrupt(format!("mp4: no stream {stream} to scan")))?;
        track.field_order = order;
        Ok(())
    }

    /// Declare an audio track's encoder delay: `priming_samples` produced
    /// before the first audible one, in the track's own sample rate (which is
    /// its timescale, so this is a sample count with no conversion). Written
    /// as an `edts/elst` media_time shift at [`Muxer::finish`] — the same
    /// mechanism a reordered video track's `elst` uses, and the ISO
    /// 14496-14 convention for AAC priming.
    pub fn set_audio_delay(&mut self, stream: u32, priming_samples: u32) -> Result<()> {
        let track = self
            .tracks
            .get_mut(stream as usize)
            .ok_or_else(|| Error::corrupt(format!("mp4: no stream {stream} to delay")))?;
        track.audio_delay = i64::from(priming_samples);
        Ok(())
    }

    /// Play `stream` from `media_time` for `duration` ticks, and no further: the
    /// audio edit is trimmed to a stated length rather than running to the end
    /// of the samples written. Both are in the track's own timescale (for audio,
    /// its sample rate), so `media_time` is the priming to skip and `duration`
    /// the picture's length, with no conversion by the caller.
    ///
    /// Written at [`Muxer::finish`] as one `edts/elst` entry with its
    /// `segment_duration` in the movie timescale (rounded up, as the movie
    /// durations are), and the track's `tkhd` and the movie's `mvhd` durations
    /// follow it. It replaces [`Mp4Muxer::set_audio_delay`] for this stream: the
    /// `media_time` given is the shift. Without this call the output is unchanged.
    pub fn set_edit(&mut self, stream: u32, media_time: u32, duration: u32) -> Result<()> {
        let track = self
            .tracks
            .get_mut(stream as usize)
            .ok_or_else(|| Error::corrupt(format!("mp4: no stream {stream} to edit")))?;
        track.edit = Some((media_time, duration));
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
        match &self.brands {
            Some((major, compatible)) => {
                head.extend_from_slice(major);
                head.extend_from_slice(&512u32.to_be_bytes());
                for brand in compatible {
                    head.extend_from_slice(brand);
                }
            }
            None => {
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
            }
        }
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

    /// The film's length in movie milliseconds: the longest track's presentation.
    fn movie_duration(&self) -> i64 {
        let movie_base = TimeBase::from_rate(MOVIE_TIMESCALE);
        self.tracks
            .iter()
            .map(|t| presentation_movie_ticks(t, movie_base))
            .max()
            .unwrap_or(0)
    }

    /// The track a chapter text track is referenced from: the first video track,
    /// else the first audio track, else the first track of any kind.
    fn chapter_reference(&self) -> Option<usize> {
        let is = |kind: ec_core::MediaType| {
            self.tracks
                .iter()
                .position(|t| t.info.params.codec.media_type() == kind)
        };
        is(ec_core::MediaType::Video)
            .or_else(|| is(ec_core::MediaType::Audio))
            .or(match self.tracks.is_empty() {
                true => None,
                false => Some(0),
            })
    }

    /// Lay the chapter samples down in the `mdat`, at the current write position:
    /// each is a 16-bit length and the title's UTF-8 bytes, as QuickTime text
    /// samples are. Their times come from the starts, rounded to milliseconds.
    fn write_chapter_text(&mut self) -> Result<ChapterText> {
        let starts: Vec<i64> = self
            .chapters
            .iter()
            .map(|(ns, _)| (ns.saturating_add(500_000) / 1_000_000) as i64)
            .collect();
        let first_ms = starts.first().copied().unwrap_or(0);
        let end_ms = self
            .movie_duration()
            .max(starts.last().copied().unwrap_or(0));
        let offset = self.pos;
        let mut payload = Vec::new();
        let mut samples = Vec::with_capacity(self.chapters.len());
        for (i, (_, title)) in self.chapters.iter().enumerate() {
            let next = starts.get(i + 1).copied().unwrap_or(end_ms);
            let mut duration = (next - starts[i]).max(0) as u32;
            if i + 1 == starts.len() {
                // Readers that end the last chapter at the text track's media duration
                // (ffmpeg) do not count the leading empty edit, so the media is padded by
                // the first chapter's start; the edit list below still plays only to the end.
                duration += first_ms.max(0) as u32;
            }
            let text = clip(title, usize::from(u16::MAX));
            payload.extend_from_slice(&(text.len() as u16).to_be_bytes());
            payload.extend_from_slice(text.as_bytes());
            samples.push((duration, 2 + text.len() as u32));
        }
        self.w.write_all(&payload)?;
        self.pos += payload.len() as u64;
        Ok(ChapterText {
            first_ms,
            end_ms,
            samples,
            offset,
        })
    }

    fn write_moov(&mut self, chapters: Option<&ChapterText>) -> Result<()> {
        let mut moov = Vec::with_capacity(4096);
        let at = open(&mut moov, b"moov");
        let duration = self.movie_duration().max(chapters.map_or(0, |c| c.end_ms));
        let chapter_id = chapters.map(|_| self.tracks.len() as u32 + 1);
        let reference = self.chapter_reference();

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
        let next_id = chapter_id.unwrap_or(self.tracks.len() as u32) + 1;
        moov.extend_from_slice(&next_id.to_be_bytes());
        close(&mut moov, mvhd);

        for i in 0..self.tracks.len() {
            let chapter_ref = match reference == Some(i) {
                true => chapter_id,
                false => None,
            };
            self.write_trak(&mut moov, i, duration, chapter_ref)?;
        }
        if let (Some(text), Some(id)) = (chapters, chapter_id) {
            write_chapter_trak(&mut moov, id, text);
        }
        let chpl = (1..=255).contains(&self.chapters.len());
        if self.title.is_some() || chpl {
            let udta = open(&mut moov, b"udta");
            if let Some(title) = &self.title {
                leaf(&mut moov, b"name", title.as_bytes());
            }
            if chpl {
                write_chpl(&mut moov, &self.chapters);
            }
            close(&mut moov, udta);
        }
        close(&mut moov, at);
        self.w.write_all(&moov)?;
        self.pos += moov.len() as u64;
        Ok(())
    }

    /// One `trak`. `chapter_ref` is the id of the chapter text track this track
    /// points at with a `tref/chap`, if it is the one the chapters hang off.
    fn write_trak(
        &self,
        out: &mut Vec<u8>,
        i: usize,
        movie_duration: i64,
        chapter_ref: Option<u32>,
    ) -> Result<()> {
        let track = &self.tracks[i];
        let codec = track.info.params.codec;
        let at = open(out, b"trak");

        let tkhd = open(out, b"tkhd");
        full_head(out, 0, 0x7); // enabled, in movie, in preview
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(i as u32 + 1).to_be_bytes()); // track_ID
        out.extend_from_slice(&0u32.to_be_bytes());
        let tkhd_duration = match track.edit {
            Some(_) => presentation_movie_ticks(track, TimeBase::from_rate(MOVIE_TIMESCALE)),
            None => movie_duration,
        };
        out.extend_from_slice(&(tkhd_duration.max(0) as u32).to_be_bytes());
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

        if let Some(id) = chapter_ref {
            let tref = open(out, b"tref");
            let chap = open(out, b"chap");
            out.extend_from_slice(&id.to_be_bytes());
            close(out, chap);
            close(out, tref);
        }

        // An `elst`, where the first picture is not the first *decoded* one, or
        // an audio track's encoder primed samples ahead of the first audible
        // one (`set_audio_delay`): a reordered stream's earliest composition
        // time is the reorder delay, and without an edit saying so every
        // player starts the track that much late. This is where a demuxer's
        // `media_time` comes from, and it is a shift, never a trim.
        if let Some((media_time, _)) = track.edit {
            let edts = open(out, b"edts");
            let elst = open(out, b"elst");
            full_head(out, 0, 0);
            out.extend_from_slice(&1u32.to_be_bytes());
            let movie_base = TimeBase::from_rate(MOVIE_TIMESCALE);
            let segment = presentation_movie_ticks(track, movie_base);
            out.extend_from_slice(&(segment.max(0) as u32).to_be_bytes());
            out.extend_from_slice(&media_time.to_be_bytes());
            out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // media rate 1.0
            close(out, elst);
            close(out, edts);
        }
        let priming = presentation_start(track)
            .max(track.audio_delay)
            .max(opus_pre_skip(track));
        if track.edit.is_none() && priming > 0 {
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
        sample_entry(out, &track.info, track.field_order, track.chan)?;
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
            audio_delay: 0,
            edit: None,
            field_order: Some(ec_core::Interlace::Progressive),
            chan: false,
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
        for track in &mut self.tracks {
            Mp4Muxer::<W>::resolve_durations(track);
        }
        // The chapter text samples are media, so they go in the mdat before its
        // size is known.
        let chapters = match self.chapters.is_empty() {
            true => None,
            false => Some(self.write_chapter_text()?),
        };
        // The mdat's real size, into the 64-bit field left for it.
        let mdat_len = self.pos - self.mdat_at;
        self.w.seek(SeekFrom::Start(self.mdat_at + 8))?;
        self.w.write_all(&mdat_len.to_be_bytes())?;
        self.w.seek(SeekFrom::Start(self.pos))?;
        self.write_moov(chapters.as_ref())?;
        self.w.flush()?;
        Ok(())
    }
}

/// The QuickTime chapter text track: one `text` sample per chapter, in the movie
/// clock (milliseconds), with a `co64` offset so it stays right past 4 GB. It is
/// disabled, as a chapter track is in a QuickTime file, and its `edts` puts the
/// first chapter at its own start when that is not zero.
fn write_chapter_trak(out: &mut Vec<u8>, id: u32, chap: &ChapterText) {
    let at = open(out, b"trak");

    let tkhd = open(out, b"tkhd");
    full_head(out, 0, 0); // not enabled, not in the movie: a reference target
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(chap.end_ms.max(0) as u32).to_be_bytes());
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&0u16.to_be_bytes()); // layer
    out.extend_from_slice(&0u16.to_be_bytes()); // alternate group
    out.extend_from_slice(&0u16.to_be_bytes()); // volume
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&UNITY_MATRIX);
    out.extend_from_slice(&[0; 8]); // width, height
    close(out, tkhd);

    if chap.first_ms > 0 {
        let edts = open(out, b"edts");
        let elst = open(out, b"elst");
        full_head(out, 0, 0);
        out.extend_from_slice(&2u32.to_be_bytes());
        // An empty edit: the chapter track is silent until the first chapter.
        out.extend_from_slice(&(chap.first_ms as u32).to_be_bytes());
        out.extend_from_slice(&u32::MAX.to_be_bytes()); // media_time -1
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        // The chapters themselves, from media time zero.
        let length = (chap.end_ms - chap.first_ms).max(0) as u32;
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        close(out, elst);
        close(out, edts);
    }

    let mdia = open(out, b"mdia");
    let mdhd = open(out, b"mdhd");
    full_head(out, 0, 0);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&MOVIE_TIMESCALE.to_be_bytes());
    let total: u64 = chap.samples.iter().map(|(d, _)| u64::from(*d)).sum();
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&pack_language(None).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    close(out, mdhd);

    let hdlr = open(out, b"hdlr");
    full_head(out, 0, 0);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(b"text");
    out.extend_from_slice(&[0; 12]);
    out.extend_from_slice(b"ChapterHandler");
    out.push(0);
    close(out, hdlr);

    let minf = open(out, b"minf");
    let nmhd = open(out, b"nmhd");
    full_head(out, 0, 0);
    close(out, nmhd);
    let dinf = open(out, b"dinf");
    let dref = open(out, b"dref");
    full_head(out, 0, 0);
    out.extend_from_slice(&1u32.to_be_bytes());
    let url = open(out, b"url ");
    full_head(out, 0, 1);
    close(out, url);
    close(out, dref);
    close(out, dinf);

    let stbl = open(out, b"stbl");
    let stsd = open(out, b"stsd");
    full_head(out, 0, 0);
    out.extend_from_slice(&1u32.to_be_bytes());
    // A QuickTime `text` sample description with the defaults: left-justified,
    // white, no font name. The samples carry only the title.
    let text = open(out, b"text");
    out.extend_from_slice(&[0; 6]);
    out.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    out.extend_from_slice(&[0; 4 + 4 + 6 + 8 + 8]); // flags, justification, colours, box
    out.extend_from_slice(&[0; 2 + 2 + 1 + 2]); // font id, face, reserved
    for _ in 0..3 {
        out.extend_from_slice(&0xFFFFu16.to_be_bytes()); // foreground: white
    }
    out.push(0); // empty font name
    close(out, text);
    close(out, stsd);

    let stts = open(out, b"stts");
    full_head(out, 0, 0);
    out.extend_from_slice(&(chap.samples.len() as u32).to_be_bytes());
    for (duration, _) in &chap.samples {
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&duration.to_be_bytes());
    }
    close(out, stts);

    let stsc = open(out, b"stsc");
    full_head(out, 0, 0);
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes()); // first chunk
    out.extend_from_slice(&(chap.samples.len() as u32).to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes()); // description index
    close(out, stsc);

    let stsz = open(out, b"stsz");
    full_head(out, 0, 0);
    out.extend_from_slice(&0u32.to_be_bytes()); // sizes differ, so none is common
    out.extend_from_slice(&(chap.samples.len() as u32).to_be_bytes());
    for (_, size) in &chap.samples {
        out.extend_from_slice(&size.to_be_bytes());
    }
    close(out, stsz);

    let co64 = open(out, b"co64");
    full_head(out, 0, 0);
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&chap.offset.to_be_bytes());
    close(out, co64);
    close(out, stbl);
    close(out, minf);
    close(out, mdia);
    close(out, at);
}

/// The Nero `chpl` box in `moov/udta`: version 1 (its 4 extra bytes are
/// reserved), start times in 100 ns units, and titles cut to 255 bytes. Written
/// only for 1 to 255 chapters: the count is one byte.
fn write_chpl(out: &mut Vec<u8>, chapters: &[(u64, String)]) {
    let chpl = open(out, b"chpl");
    full_head(out, 1, 0);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.push(chapters.len() as u8);
    for (start_ns, title) in chapters {
        out.extend_from_slice(&(start_ns / 100).to_be_bytes());
        let text = clip(title, usize::from(u8::MAX));
        out.push(text.len() as u8);
        out.extend_from_slice(text.as_bytes());
    }
    close(out, chpl);
}

/// `title` cut to at most `max` bytes, on a character boundary.
fn clip(title: &str, max: usize) -> &str {
    if title.len() <= max {
        return title;
    }
    let mut end = max;
    while !title.is_char_boundary(end) {
        end -= 1;
    }
    &title[..end]
}

/// A track's presentation length in movie-timescale ticks: the explicit edit's
/// duration when one was set, else the whole track.
fn presentation_movie_ticks(track: &Track, movie_base: TimeBase) -> i64 {
    match track.edit {
        Some((_, duration)) => {
            track
                .time_base
                .rescale(i64::from(duration), movie_base, Rounding::Up)
        }
        None => track
            .time_base
            .rescale(track_ticks(track), movie_base, Rounding::Up),
    }
}

/// Total media ticks of a track: from the first sample's decode time to the latest
/// end of any sample, in presentation or decode time. A span rather than an
/// endpoint, because a track handed over with an edit list already applied starts
/// at a negative decode time and its duration is still the whole of it. The
/// presentation end is not always the last decode end: a reordered stream's last
/// picture is shown a reorder delay after its decode time, and the `elst` that
/// starts the picture at its priming must span to there or the track ends early.
fn track_ticks(track: &Track) -> i64 {
    let first = track.samples.first().map_or(0, |s| s.dts);
    track
        .samples
        .iter()
        .map(|s| {
            let end = i64::from(s.duration);
            (s.dts + end).max(s.cts + end) - first
        })
        .max()
        .unwrap_or(0)
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

/// An Opus track's pre-skip, in its own 48 kHz clock, from the `OpusHead` it
/// carries (bytes 10 and 11, little-endian). The Opus-in-ISOBMFF convention states
/// it as the `elst` `media_time`, so a remux of an Opus file keeps its shift; 0
/// for every other track, or where the head is too short to say.
fn opus_pre_skip(track: &Track) -> i64 {
    if track.info.params.codec != CodecId::Opus {
        return 0;
    }
    let Some(head) = track.info.params.extradata.as_deref() else {
        return 0;
    };
    // `dOps` carries it big-endian at bytes 2 and 3; an `OpusHead` little-endian at 10.
    match head.starts_with(b"OpusHead") {
        true => match head.get(10..12) {
            Some(&[lo, hi]) => i64::from(u16::from_le_bytes([lo, hi])),
            _ => 0,
        },
        false => match head.get(2..4) {
            Some(&[hi, lo]) => i64::from(u16::from_be_bytes([hi, lo])),
            _ => 0,
        },
    }
}

/// The tables of a QuickTime PCM track the way QuickTime and ffmpeg lay them
/// out: a *sample* is one frame (`frame_bytes`, one `stts` tick), and a chunk
/// holds as many samples as the packets written into it had frames.
fn pcm_tables(out: &mut Vec<u8>, track: &Track, frame_bytes: u32) {
    let frame_bytes = frame_bytes.max(1);
    let total: u64 = track
        .samples
        .iter()
        .map(|s| u64::from(s.size / frame_bytes))
        .sum();
    let stts = open(out, b"stts");
    full_head(out, 0, 0);
    out.extend_from_slice(&u32::from(total > 0).to_be_bytes());
    if total > 0 {
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
    }
    close(out, stts);

    // Frames per chunk: the chunk's packets are the next `chunk.1` samples.
    let mut per_chunk = Vec::with_capacity(track.chunks.len());
    let mut at = 0usize;
    for &(_, packets) in &track.chunks {
        let end = (at + packets as usize).min(track.samples.len());
        per_chunk.push(
            track.samples[at..end]
                .iter()
                .map(|s| s.size / frame_bytes)
                .sum::<u32>(),
        );
        at = end;
    }
    let stsc = open(out, b"stsc");
    full_head(out, 0, 0);
    let count_at = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let mut runs = 0u32;
    let mut first = 0usize;
    while first < per_chunk.len() {
        let per = per_chunk[first];
        out.extend_from_slice(&(first as u32 + 1).to_be_bytes());
        out.extend_from_slice(&per.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        runs += 1;
        first += 1;
        while first < per_chunk.len() && per_chunk[first] == per {
            first += 1;
        }
    }
    out[count_at..count_at + 4].copy_from_slice(&runs.to_be_bytes());
    close(out, stsc);

    let stsz = open(out, b"stsz");
    full_head(out, 0, 0);
    out.extend_from_slice(&frame_bytes.to_be_bytes());
    out.extend_from_slice(&(total as u32).to_be_bytes());
    close(out, stsz);

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

/// The `stts`/`ctts`/`stss`/`stsc`/`stsz` and chunk-offset tables of one track.
fn sample_tables(out: &mut Vec<u8>, track: &Track) {
    if let (Some((bytes, _)), Some(audio)) =
        (qt_pcm(track.info.params.codec), track.info.params.audio())
    {
        pcm_tables(out, track, bytes * audio.layout.channel_count() as u32);
        return;
    }
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
fn sample_entry(
    out: &mut Vec<u8>,
    info: &StreamInfo,
    field_order: Option<ec_core::Interlace>,
    chan: bool,
) -> Result<()> {
    let codec = info.params.codec;
    let extradata = info.params.extradata.as_deref();
    if codec == CodecId::ProRes {
        // The profile is the sample-entry name; the frame header is self-describing, so there
        // is no configuration box. `extradata` carries the fourcc (apco apcs apcn apch ap4h ap4x).
        let name: [u8; 4] = match extradata
            .and_then(|e| e.get(..4))
            .map(|e| [e[0], e[1], e[2], e[3]])
        {
            Some(n)
                if matches!(
                    &n,
                    b"apco" | b"apcs" | b"apcn" | b"apch" | b"ap4h" | b"ap4x"
                ) =>
            {
                n
            }
            _ => {
                return Err(Error::unsupported(
                    "mp4: a ProRes track without a profile fourcc",
                    "set extradata to apco, apcs, apcn, apch, ap4h or ap4x",
                ));
            }
        };
        let video = info.params.video().ok_or_else(|| {
            Error::corrupt("mp4: a video codec declared without video parameters")
        })?;
        let at = open(out, &name);
        out.extend_from_slice(&[0; 6]); // reserved
        out.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        out.extend_from_slice(&[0; 16]); // pre_defined + reserved
        out.extend_from_slice(&(video.width as u16).to_be_bytes());
        out.extend_from_slice(&(video.height as u16).to_be_bytes());
        out.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        out.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // frame_count
        out.extend_from_slice(&[0; 32]); // compressorname
        out.extend_from_slice(&0x0018u16.to_be_bytes());
        out.extend_from_slice(&(-1i16).to_be_bytes());
        colour_boxes(out, video.color, video.light);
        if let Some(order) = field_order {
            let (count, detail) = match order {
                ec_core::Interlace::Progressive => (1u8, 0u8),
                ec_core::Interlace::TopFieldFirst => (2, 1),
                ec_core::Interlace::BottomFieldFirst => (2, 6),
            };
            leaf(out, b"fiel", &[count, detail]);
        }
        let sar = video.sample_aspect_ratio;
        let (n, d) = sar.map_or((1, 1), |s| (s.num() as u32, s.den() as u32));
        let pasp = open(out, b"pasp");
        out.extend_from_slice(&n.to_be_bytes());
        out.extend_from_slice(&d.to_be_bytes());
        close(out, pasp);
        close(out, at);
        return Ok(());
    }
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
        // ffmpeg's order: after `colr`/`pasp`. field_count 1 / detail 0 is a
        // progressive picture; 2 fields with detail 1 is top first (stored and
        // displayed), 6 bottom first: what ffmpeg writes for tt/bb (its 9/14, tb/bt,
        // are read back as top/bottom first too, but never written here).
        if let Some(order) = field_order {
            let (count, detail) = match order {
                ec_core::Interlace::Progressive => (1u8, 0u8),
                ec_core::Interlace::TopFieldFirst => (2, 1),
                ec_core::Interlace::BottomFieldFirst => (2, 6),
            };
            leaf(out, b"fiel", &[count, detail]);
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
    // QuickTime sound description version 1 for the PCM wider than 16 bits,
    // which is what ffmpeg writes for them in a .mov.
    let qt = qt_pcm(codec);
    out.extend_from_slice(&[0, u8::from(qt.is_some()), 0, 0, 0, 0, 0, 0]); // version, revision, vendor
    out.extend_from_slice(&(audio.layout.channel_count() as u16).to_be_bytes());
    let bits = if qt.is_some() {
        16
    } else {
        audio.bits_per_sample.unwrap_or(16)
    };
    out.extend_from_slice(&(bits as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    out.extend_from_slice(&0u16.to_be_bytes()); // reserved
    // 16.16, so a rate over 65535 is stated as its low half — which is what
    // every writer does and what the `mdhd` timescale says exactly anyway.
    out.extend_from_slice(&((audio.sample_rate as u16 as u32) << 16).to_be_bytes());
    if let Some((bytes, little)) = qt {
        let channels = audio.layout.channel_count() as u32;
        out.extend_from_slice(&1u32.to_be_bytes()); // samples per packet
        out.extend_from_slice(&bytes.to_be_bytes()); // bytes per packet
        out.extend_from_slice(&(bytes * channels).to_be_bytes()); // bytes per frame
        out.extend_from_slice(&2u32.to_be_bytes()); // bytes per sample (ffmpeg's constant)
        let wave = open(out, b"wave");
        leaf(out, b"frma", name);
        leaf(out, b"enda", &u16::from(little).to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 8, 0, 0, 0, 0]); // terminator: an 8-byte box of type zero
        close(out, wave);
    }
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
        CodecId::Ac3
        | CodecId::EAc3
        | CodecId::Dts
        | CodecId::Flac
        | CodecId::Opus
        | CodecId::Alac => {
            let config = extradata.ok_or_else(|| {
                Error::unsupported(
                    format!("mp4: a {} track with no codec configuration", codec.name()),
                    "its sample entry has to carry the codec's own configuration box",
                )
            })?;
            match codec {
                // ALAC's cookie is carried with its box header, so it is written
                // back exactly as it was read; a bare cookie gets its header here.
                CodecId::Alac if config.len() > 8 && &config[4..8] == b"alac" => {
                    out.extend_from_slice(config)
                }
                CodecId::Alac => leaf(out, b"alac", config),
                CodecId::Opus => leaf(out, b"dOps", &dops(config)),
                _ => leaf(out, audio_config_box(codec), config),
            }
        }
        _ => {}
    }
    if chan || qt.is_some() {
        chan_box(out, &audio.layout);
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
    if light.mastering_max.is_some()
        || light.mastering_min.is_some()
        || light.mastering_primaries.is_some()
    {
        let mdcv = open(out, b"mdcv");
        // Display primaries and white point as stated; BT.2020 is what an HDR
        // grade is mastered on and what a writer states when nothing is.
        let primaries = light
            .mastering_primaries
            .map_or(BT2020_PRIMARIES, |p| p.to_array());
        for xy in primaries {
            out.extend_from_slice(&xy.to_be_bytes());
        }
        let nits = |v: Option<f32>| {
            ((f64::from(v.unwrap_or(0.0)) * 10_000.0).round() as u32).to_be_bytes()
        };
        out.extend_from_slice(&nits(light.mastering_max));
        out.extend_from_slice(&nits(light.mastering_min));
        close(out, mdcv);
    }
    if light.max_cll.is_some() || light.max_fall.is_some() {
        let clli = open(out, b"clli");
        out.extend_from_slice(&(light.max_cll.unwrap_or(0.0).round() as u16).to_be_bytes());
        out.extend_from_slice(&(light.max_fall.unwrap_or(0.0).round() as u16).to_be_bytes());
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

/// `(bytes per sample, little-endian)` of the PCM codecs QuickTime describes
/// with a version 1 sound description and a `wave`/`enda`: `in24`, `in32`, `fl32`.
fn qt_pcm(codec: CodecId) -> Option<(u32, bool)> {
    Some(match codec {
        CodecId::PcmS24Le => (3, true),
        CodecId::PcmS24Be => (3, false),
        CodecId::PcmS32Le | CodecId::PcmF32Le => (4, true),
        CodecId::PcmS32Be | CodecId::PcmF32Be => (4, false),
        _ => return None,
    })
}

/// The QuickTime `chan` box for the layouts that have a stock one, as ffmpeg
/// writes it: a layout tag for mono and stereo, a channel bitmap for 5.1.
fn chan_box(out: &mut Vec<u8>, layout: &ec_core::ChannelLayout) {
    use ec_core::ChannelLayout;
    let (tag, bitmap) = match layout {
        ChannelLayout::Mono => (0x0064_0001u32, 0u32),
        ChannelLayout::Stereo => (0x0065_0002, 0),
        // UseChannelBitmap; L R C LFE Ls Rs.
        ChannelLayout::Surround5_1 => (0x0001_0000, 0x3f),
        _ => return,
    };
    let at = open(out, b"chan");
    full_head(out, 0, 0);
    out.extend_from_slice(&tag.to_be_bytes());
    out.extend_from_slice(&bitmap.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // number of descriptions
    close(out, at);
}

fn audio_entry(codec: CodecId) -> Result<&'static [u8; 4]> {
    Ok(match codec {
        CodecId::Aac | CodecId::Mp3 => b"mp4a",
        CodecId::Ac3 => b"ac-3",
        CodecId::EAc3 => b"ec-3",
        CodecId::Dts => b"dtsc",
        CodecId::Alac => b"alac",
        CodecId::Flac => b"fLaC",
        CodecId::Opus => b"Opus",
        CodecId::PcmS16Le => b"sowt",
        CodecId::PcmS16Be => b"twos",
        CodecId::PcmU8 => b"raw ",
        CodecId::PcmS24Le | CodecId::PcmS24Be => b"in24",
        CodecId::PcmS32Le | CodecId::PcmS32Be => b"in32",
        CodecId::PcmF32Le | CodecId::PcmF32Be => b"fl32",
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
        CodecId::Dts => b"ddts",
        CodecId::Flac => b"dfLa",
        CodecId::Alac => b"alac",
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
    use crate::Mp4Demuxer;
    use ec_core::{AudioParameters, Buf, ChannelLayout, CodecParameters, Demuxer, MediaParameters};
    use std::io::Cursor;

    /// Ten AAC packets on a sound-only file: the `M4A ` brand is the first thing
    /// `ftyp` states, no `vide` handler exists anywhere in the file, and the file
    /// reads back as the same ten packets on one AAC track.
    #[test]
    fn an_audio_only_m4a_states_its_brand_and_holds_one_aac_track() {
        let mut params = CodecParameters::new(CodecId::Aac);
        params.extradata = Some(Buf::from(vec![0x12, 0x10]));
        params.media = MediaParameters::Audio(AudioParameters {
            sample_rate: 44_100,
            layout: ChannelLayout::from_count(2),
            ..Default::default()
        });
        let mut muxer = Mp4Muxer::with_brands(
            Cursor::new(Vec::new()),
            *b"M4A ",
            &[*b"M4A ", *b"mp42", *b"isom"],
        )
        .unwrap();
        let stream = muxer
            .add_stream(StreamInfo::new(0, TimeBase::from_rate(44_100), params))
            .unwrap();
        for i in 0..10u8 {
            let mut packet = Packet::new(stream, TimeBase::from_rate(44_100), vec![0xAA, i, 0x55]);
            packet.dts = Some(i64::from(i) * 1024);
            packet.duration = Some(1024);
            packet.flags.keyframe = true;
            muxer.write_packet(&packet).unwrap();
        }
        muxer.finish().unwrap();
        let bytes = muxer.into_inner().into_inner();

        assert_eq!(&bytes[4..8], b"ftyp");
        assert_eq!(&bytes[8..12], b"M4A ", "the major brand");
        assert!(
            !bytes.windows(4).any(|w| w == b"vide"),
            "an audio-only file has no video track"
        );

        let mut demuxer = Mp4Demuxer::new(Cursor::new(bytes)).unwrap();
        assert_eq!(demuxer.streams().len(), 1, "one track");
        assert_eq!(demuxer.streams()[0].params.codec, CodecId::Aac);
        let mut back = Vec::new();
        loop {
            match demuxer.next_packet() {
                Ok(packet) => back.push(packet.data.to_vec()),
                Err(Error::Eof) => break,
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(back.len(), 10);
        assert_eq!(back[3], vec![0xAA, 3, 0x55]);
    }

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

    /// One second of silent AAC, with the chapters given, as bytes.
    fn one_aac_track(chapters: &[(u64, String)]) -> Result<Vec<u8>> {
        let mut params = CodecParameters::new(CodecId::Aac);
        params.extradata = Some(Buf::from(vec![0x12, 0x10]));
        params.media = MediaParameters::Audio(AudioParameters {
            sample_rate: 44_100,
            layout: ChannelLayout::from_count(2),
            ..Default::default()
        });
        let mut muxer = Mp4Muxer::new(Cursor::new(Vec::new()))?;
        muxer.set_chapters(chapters)?;
        let stream = muxer.add_stream(StreamInfo::new(0, TimeBase::from_rate(44_100), params))?;
        for i in 0..43u8 {
            let mut packet = Packet::new(stream, TimeBase::from_rate(44_100), vec![0xAA, i, 0x55]);
            packet.dts = Some(i64::from(i) * 1024);
            packet.duration = Some(1024);
            packet.flags.keyframe = true;
            muxer.write_packet(&packet)?;
        }
        muxer.finish()?;
        Ok(muxer.into_inner().into_inner())
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[test]
    fn chpl_is_version_one_in_hundred_nanoseconds_with_byte_counted_titles() {
        let chapters = vec![
            (0u64, "Intro".to_string()),
            (2_000_000_000, "Ünï".to_string()),
        ];
        let bytes = one_aac_track(&chapters).unwrap();
        let at = find(&bytes, b"chpl").expect("a chpl box") - 4;
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let body = &bytes[at + 8..at + size];
        assert_eq!(body[0], 1, "chpl version 1");
        assert_eq!(&body[1..4], &[0, 0, 0], "flags");
        assert_eq!(&body[4..8], &[0, 0, 0, 0], "the version-1 reserved word");
        assert_eq!(body[8], 2, "chapter count, one byte");
        assert_eq!(u64::from_be_bytes(body[9..17].try_into().unwrap()), 0);
        assert_eq!(body[17], 5, "title length, one byte");
        assert_eq!(&body[18..23], b"Intro");
        assert_eq!(
            u64::from_be_bytes(body[23..31].try_into().unwrap()),
            20_000_000,
            "2 s in 100 ns units"
        );
        assert_eq!(body[31], 5, "'Ü' and 'ï' are two bytes each");
        assert_eq!(&body[32..37], "Ünï".as_bytes());
    }

    #[test]
    fn chapters_are_referenced_from_the_audio_track_and_run_to_the_end() {
        let chapters = vec![
            (3_000_000_000, "Late".to_string()),
            (1_000_000_000, "Early".to_string()),
        ];
        let bytes = one_aac_track(&chapters).unwrap();
        let tref = find(&bytes, b"tref").expect("a tref box in the audio track");
        // The tref sits inside the one audio trak, which comes before the chapter
        // track; tref/chap names track 2, the chapter track after the audio one.
        assert_eq!(&bytes[tref + 8..tref + 12], b"chap");
        let id = u32::from_be_bytes(bytes[tref + 12..tref + 16].try_into().unwrap());
        assert_eq!(id, 2);
        // The titles are stored sorted by start, so "Early" precedes "Late" in the mdat.
        let early = find(&bytes, b"Early").expect("the Early sample");
        let late = find(&bytes, b"Late").expect("the Late sample");
        assert!(early < late, "sorted by start");
    }

    #[test]
    fn no_chapters_writes_no_chapter_boxes_or_track() {
        let bytes = one_aac_track(&[]).unwrap();
        assert!(find(&bytes, b"chpl").is_none());
        assert!(find(&bytes, b"tref").is_none());
        assert!(find(&bytes, b"text").is_none());
        assert!(find(&bytes, b"co64").is_none());
    }

    #[test]
    fn more_than_255_chapters_omit_the_chpl_box_and_keep_the_track() {
        let chapters: Vec<(u64, String)> = (0..300u64)
            .map(|i| (i * 1_000_000_000, format!("c{i}")))
            .collect();
        let bytes = one_aac_track(&chapters).unwrap();
        assert!(find(&bytes, b"chpl").is_none(), "the count is one byte");
        assert!(
            find(&bytes, b"tref").is_some(),
            "the chapter track is still referenced"
        );
        assert!(
            find(&bytes, b"c299").is_some(),
            "the last chapter's title is in the track"
        );
    }

    #[test]
    fn clipped_titles_keep_their_characters_whole() {
        assert_eq!(clip("abc", 2), "ab");
        assert_eq!(
            clip("aé", 2),
            "a",
            "a two-byte character does not fit in one byte"
        );
        assert_eq!(clip("aé", 3), "aé");
    }
}
