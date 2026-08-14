//! The Matroska/WebM writer: EBML header, one `Segment` of clusters, and the
//! `SeekHead` + `Cues` a player seeks with.
//!
//! The file is patched three times at [`Muxer::finish`] — the segment size, the
//! duration and the `SeekHead` reserved in front of the `Info` — so a killed
//! process leaves an unfinished file rather than a wrong one.

use std::io::{Seek, SeekFrom, Write};

use ec_core::{
    CodecId, Error, MediaParameters, MediaType, Muxer, Packet, Result, Rounding, StreamInfo,
    TimeBase,
};

use crate::ebml::{self, elem, elem_head_len, float, put_id, put_size, uint};

/// One millisecond, the tick every Matroska muxer writes and this one's block
/// timestamps are in. The *rate* is not derived from it — `DefaultDuration` is,
/// in exact nanoseconds — so a millisecond is precise enough for the
/// presentation times and coarse enough to keep a cluster's 16-bit relative
/// timestamp covering half a minute.
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;
/// The time base that scale is: every packet is rescaled into it.
pub const MUX_TIME_BASE: TimeBase = TimeBase::new(1, 1_000);
/// A cluster is buffered whole (its size is a header field), so it is flushed
/// at the first keyframe past this — a ceiling on what the muxer holds, not a
/// target.
const CLUSTER_BYTES: usize = 4 << 20;
/// ...and on how far a block's timestamp may sit from its cluster's: the field
/// is a signed 16-bit millisecond count.
const CLUSTER_MS: i64 = 30_000;
/// Bytes reserved in front of the `Info` for the `SeekHead` written at
/// [`Muxer::finish`]: three entries need about 60, and whatever is left over is
/// filled with a `Void`.
const SEEK_HEAD_RESERVE: usize = 128;
/// A block writes its track number as a one-byte EBML integer (`0x80 | track`),
/// and 127 is one number too far: its byte is `0xFF`, the all-ones
/// variable-length integer EBML spells *unknown* with. The numbers therefore run
/// `1..=126`, and a file asking for more is refused by name rather than written
/// with a byte that means another track, or none.
pub const MAX_TRACKS: usize = 0x7E;

struct MuxTrack {
    number: u64,
    media: MediaType,
    time_base: TimeBase,
    /// Nanoseconds a frame lasts, where the stream stated a rate.
    frame_ns: Option<u64>,
    /// `TrackName`, where a caller named the track.
    title: String,
    /// `CodecDelay` in nanoseconds, where a caller stated one.
    delay_ns: Option<u64>,
}

/// One `CuePoint` waiting for [`Muxer::finish`].
struct CuePoint {
    time: i64,
    track: u64,
    /// Offset of the cluster from the start of the segment's payload, which is
    /// what `CueClusterPosition` states.
    cluster: u64,
    /// ...and of the block from the start of that cluster's payload.
    relative: u64,
}

/// Writes Matroska (`.mkv`) or, with [`MatroskaMuxer::webm`], the WebM subset.
///
/// Streams are declared first and packets arrive in storage order; their
/// timestamps are rescaled into milliseconds, which is the tick this writes.
/// A new cluster opens at every video keyframe, so every cue point is one.
pub struct MatroskaMuxer<W> {
    w: W,
    pos: u64,
    webm: bool,
    tracks: Vec<MuxTrack>,
    streams: Vec<StreamInfo>,
    /// Where the header's patchable fields landed.
    segment_size_at: u64,
    segment_body: u64,
    seek_head_at: u64,
    duration_at: u64,
    info_at: u64,
    tracks_at: u64,
    cluster: Vec<u8>,
    cluster_ts: i64,
    cluster_at: u64,
    cues: Vec<CuePoint>,
    last_end: i64,
    finished: bool,
}

impl<W: Write + Seek> MatroskaMuxer<W> {
    /// A Matroska writer over anything seekable.
    pub fn new(w: W) -> MatroskaMuxer<W> {
        MatroskaMuxer {
            w,
            pos: 0,
            webm: false,
            tracks: Vec::new(),
            streams: Vec::new(),
            segment_size_at: 0,
            segment_body: 0,
            seek_head_at: 0,
            duration_at: 0,
            info_at: 0,
            tracks_at: 0,
            cluster: Vec::new(),
            cluster_ts: 0,
            cluster_at: 0,
            cues: Vec::new(),
            last_end: 0,
            finished: false,
        }
    }

    /// The same writer declaring the WebM subset in its EBML header. Every
    /// stream must then be one WebM carries — VP8/VP9/AV1 beside Opus and
    /// Vorbis — and anything else is refused by name at
    /// [`Muxer::add_stream`] rather than written into a file no WebM reader
    /// will open.
    pub fn webm(w: W) -> MatroskaMuxer<W> {
        MatroskaMuxer {
            webm: true,
            ..MatroskaMuxer::new(w)
        }
    }

    /// Names a track: `TrackName`, the title a player's menu shows beside the
    /// language ("Signs", "Commentary"). Must be set before the first packet,
    /// which is when the `Tracks` element is written.
    pub fn set_track_title(&mut self, stream: u32, title: &str) -> Result<()> {
        let track = self
            .tracks
            .get_mut(stream as usize)
            .ok_or_else(|| Error::corrupt(format!("Matroska: no stream {stream} to name")))?;
        if self.pos > 0 {
            return Err(Error::corrupt(
                "Matroska: a track named after the first packet",
            ));
        }
        track.title = title.to_string();
        Ok(())
    }

    /// States a track's encoder delay: `CodecDelay`, nanoseconds of decoded
    /// output a player throws away before the audible stream starts.
    ///
    /// Opus needs no call — its `OpusHead` states the pre-skip and that is read
    /// straight off the extradata. AAC does: its delay is one access unit and
    /// nothing in the bitstream or the container says so, so an mp4 reader drops
    /// it by convention while Matroska has to be told. A track written without
    /// it plays ~21 ms early against the picture.
    ///
    /// Must be set before the first packet, which is when `Tracks` is written.
    pub fn set_track_delay(&mut self, stream: u32, delay_ns: u64) -> Result<()> {
        if self.pos > 0 {
            return Err(Error::corrupt(
                "Matroska: a track delayed after the first packet",
            ));
        }
        let track = self
            .tracks
            .get_mut(stream as usize)
            .ok_or_else(|| Error::corrupt(format!("Matroska: no stream {stream} to delay")))?;
        track.delay_ns = Some(delay_ns);
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.w.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }

    /// The EBML header, the `Segment`, the room its `SeekHead` will take, the
    /// `Info` and the `Tracks`. Written when the first packet arrives, which is
    /// the last moment every stream is known to have been declared.
    fn write_header(&mut self) -> Result<()> {
        if self.tracks.is_empty() {
            return Err(Error::corrupt("Matroska: a file with no tracks"));
        }
        let mut head = Vec::new();
        let mut header = Vec::new();
        uint(&mut header, ebml::EBML_VERSION, 1);
        uint(&mut header, ebml::EBML_READ_VERSION, 1);
        uint(&mut header, ebml::EBML_MAX_ID_LENGTH, 4);
        uint(&mut header, ebml::EBML_MAX_SIZE_LENGTH, 8);
        elem(
            &mut header,
            ebml::DOC_TYPE,
            if self.webm {
                b"webm".as_slice()
            } else {
                b"matroska"
            },
        );
        uint(
            &mut header,
            ebml::DOC_TYPE_VERSION,
            if self.webm { 2 } else { 4 },
        );
        uint(&mut header, ebml::DOC_TYPE_READ_VERSION, 2);
        elem(&mut head, ebml::EBML_HEADER, &header);

        // The segment's size is only known at `finish`; it is reserved at the
        // widest encoding (8 bytes) so patching it never moves a byte after it.
        put_id(&mut head, ebml::SEGMENT);
        self.segment_size_at = head.len() as u64;
        head.extend_from_slice(&[0x01, 0, 0, 0, 0, 0, 0, 0]);
        self.segment_body = head.len() as u64;

        // Room for the `SeekHead`, whose entries name elements that are not
        // written yet. A `Void` holds the space until then: a reader that meets
        // this file half-written skips it rather than reading a table of zeros.
        self.seek_head_at = head.len() as u64;
        elem(&mut head, ebml::VOID, &[0u8; SEEK_HEAD_RESERVE]);

        let mut info = Vec::new();
        uint(&mut info, ebml::TIMESTAMP_SCALE, TIMESTAMP_SCALE_NS);
        elem(&mut info, ebml::MUXING_APP, b"ec-matroska");
        elem(&mut info, ebml::WRITING_APP, b"ec-matroska");
        put_id(&mut info, ebml::DURATION);
        put_size(&mut info, 8);
        // Where those eight bytes land once `Info` is written into `head`, which
        // is what `finish` seeks back to.
        self.info_at = head.len() as u64;
        self.duration_at =
            (head.len() + elem_head_len(ebml::INFO, info.len() + 8) + info.len()) as u64;
        info.extend_from_slice(&0f64.to_be_bytes());
        elem(&mut head, ebml::INFO, &info);

        self.tracks_at = head.len() as u64;
        let mut tracks = Vec::new();
        for (i, info) in self.streams.clone().iter().enumerate() {
            let entry = self.track_entry(info, i)?;
            elem(&mut tracks, ebml::TRACK_ENTRY, &entry);
        }
        elem(&mut head, ebml::TRACKS, &tracks);
        self.write(&head)
    }

    fn track_entry(&self, info: &StreamInfo, track: usize) -> Result<Vec<u8>> {
        let number = self.tracks[track].number;
        let params = &info.params;
        let codec = matroska_codec_id(params.codec)?;
        if self.webm
            && !matches!(
                params.codec,
                CodecId::Vp8 | CodecId::Vp9 | CodecId::Av1 | CodecId::Opus | CodecId::Vorbis
            )
        {
            return Err(Error::unsupported(
                format!("{} in a WebM file", params.codec.name()),
                "WebM carries VP8/VP9/AV1 video and Opus/Vorbis audio only",
            ));
        }
        let mut entry = Vec::new();
        uint(&mut entry, ebml::TRACK_NUMBER, number);
        uint(&mut entry, ebml::TRACK_UID, number);
        uint(
            &mut entry,
            ebml::TRACK_TYPE,
            match params.codec.media_type() {
                MediaType::Video => 1,
                MediaType::Audio => 2,
                MediaType::Subtitle => 0x11,
            },
        );
        // Nothing here writes a lace, and a reader is entitled to believe this.
        uint(&mut entry, ebml::FLAG_LACING, 0);
        elem(&mut entry, ebml::CODEC_ID, codec.as_bytes());
        if let Some(extradata) = &params.extradata
            && !extradata.is_empty()
        {
            elem(&mut entry, ebml::CODEC_PRIVATE, extradata);
        }
        if !self.tracks[track].title.is_empty() {
            elem(
                &mut entry,
                ebml::TRACK_NAME,
                self.tracks[track].title.as_bytes(),
            );
        }
        // A track that states no language is English by spec, so a track whose
        // source said nothing says `und` here rather than claiming a language.
        elem(
            &mut entry,
            ebml::TRACK_LANGUAGE,
            info.language.as_deref().unwrap_or("und").as_bytes(),
        );
        match &params.media {
            MediaParameters::Video(v) => {
                if let Some(rate) = v.frame_rate
                    && rate.num() > 0
                {
                    let ns = TimeBase::new(1, 1_000_000_000);
                    uint(
                        &mut entry,
                        ebml::DEFAULT_DURATION,
                        rate.inverse().rescale(1, ns, Rounding::Nearest).max(1) as u64,
                    );
                }
                let mut dims = Vec::new();
                uint(&mut dims, ebml::PIXEL_WIDTH, u64::from(v.width));
                uint(&mut dims, ebml::PIXEL_HEIGHT, u64::from(v.height));
                // Anamorphic content is shown at a size its samples are not
                // square in; a file that states only the coded size plays
                // stretched.
                if let Some(sar) = v.sample_aspect_ratio
                    && sar.num() != sar.den()
                    && v.width > 0
                {
                    let shown = u64::from(v.width) * sar.num() as u64 / sar.den() as u64;
                    uint(&mut dims, ebml::DISPLAY_WIDTH, shown.max(1));
                    uint(&mut dims, ebml::DISPLAY_HEIGHT, u64::from(v.height));
                }
                // What the samples in those pixels mean, written rather than
                // left to a reader's own 720-line guess: an untagged file is how
                // a 601 source ends up displayed as 709.
                let mut colour = Vec::new();
                uint(
                    &mut colour,
                    ebml::MATRIX_COEFFICIENTS,
                    u64::from(v.color.matrix),
                );
                // Matroska says the range as a code, not a flag: 1 limited, 2 full.
                uint(&mut colour, ebml::RANGE, 1 + u64::from(v.color.full_range));
                uint(
                    &mut colour,
                    ebml::TRANSFER_CHARACTERISTICS,
                    u64::from(v.color.transfer),
                );
                uint(&mut colour, ebml::PRIMARIES, u64::from(v.color.primaries));
                if let Some(cll) = v.light.max_cll {
                    uint(&mut colour, ebml::MAX_CLL, cll.max(0.0) as u64);
                }
                if let Some(fall) = v.light.max_fall {
                    uint(&mut colour, ebml::MAX_FALL, fall.max(0.0) as u64);
                }
                // The mastering display, where the source stated one: a copied
                // HDR film that loses its grading display is graded again by
                // every tone map downstream.
                if v.light.mastering_max.is_some() || v.light.mastering_min.is_some() {
                    let mut mastering = Vec::new();
                    if let Some(max) = v.light.mastering_max {
                        float(&mut mastering, ebml::LUMINANCE_MAX, f64::from(max));
                    }
                    if let Some(min) = v.light.mastering_min {
                        float(&mut mastering, ebml::LUMINANCE_MIN, f64::from(min));
                    }
                    elem(&mut colour, ebml::MASTERING_METADATA, &mastering);
                }
                elem(&mut dims, ebml::COLOUR, &colour);
                elem(&mut entry, ebml::VIDEO, &dims);
            }
            MediaParameters::Audio(a) => {
                let mut audio = Vec::new();
                float(
                    &mut audio,
                    ebml::SAMPLING_FREQUENCY,
                    f64::from(a.sample_rate.max(1)),
                );
                uint(
                    &mut audio,
                    ebml::CHANNELS,
                    a.layout.channel_count().max(1) as u64,
                );
                if let Some(bits) = a.bits_per_sample {
                    uint(&mut audio, ebml::BIT_DEPTH, u64::from(bits));
                }
                // Opus states its own priming inside the `OpusHead` an mp4 or an
                // Ogg page carries; Matroska has to be told it in nanoseconds,
                // and a track written without it plays 6.5 ms early against the
                // picture. The pre-roll is the 80 ms the spec asks a seek to
                // decode and throw away.
                // A caller that stated the delay outright is believed first:
                // AAC's is one access unit and nothing in the file says so.
                if let Some(ns) = self.tracks[track].delay_ns {
                    uint(&mut entry, ebml::CODEC_DELAY, ns);
                } else if params.codec == CodecId::Opus
                    && let Some(head) = &params.extradata
                    && head.len() >= 12
                    && head.starts_with(b"OpusHead")
                {
                    let pre_skip = u64::from(u16::from_le_bytes([head[10], head[11]]));
                    uint(
                        &mut entry,
                        ebml::CODEC_DELAY,
                        pre_skip * 1_000_000_000 / 48_000,
                    );
                    uint(&mut entry, ebml::SEEK_PRE_ROLL, 80_000_000);
                }
                elem(&mut entry, ebml::AUDIO, &audio);
            }
            MediaParameters::Subtitle => {}
        }
        Ok(entry)
    }

    /// One block into the cluster it belongs in.
    fn put(
        &mut self,
        track: usize,
        ts: i64,
        duration: Option<i64>,
        key: bool,
        payload: &[u8],
    ) -> Result<()> {
        let video = self.tracks[track].media == MediaType::Video;
        // A new cluster at every video keyframe — a seek lands on one, so a
        // cluster is a whole GOP — and at the two limits a cluster has whatever
        // the encoder keys: what it may weigh, and how far a 16-bit relative
        // timestamp reaches.
        if self.cluster.is_empty()
            || (video && key)
            || self.cluster.len() >= CLUSTER_BYTES
            || (ts - self.cluster_ts).abs() >= CLUSTER_MS
        {
            self.flush()?;
            self.cluster_ts = ts;
            self.cluster_at = self.pos;
            uint(&mut self.cluster, ebml::CLUSTER_TIMESTAMP, ts.max(0) as u64);
        }
        let relative = self.cluster.len() as u64;
        let number = self.tracks[track].number;
        let rel = i16::try_from(ts - self.cluster_ts).map_err(|_| {
            Error::corrupt("Matroska: a block more than 32 seconds from its cluster")
        })?;

        let mut block = Vec::with_capacity(payload.len() + 4);
        block.push(0x80 | number as u8); // the track number as a one-byte EBML integer
        block.extend_from_slice(&rel.to_be_bytes());
        match duration {
            // A `SimpleBlock` has nowhere to put a duration, and a subtitle
            // without one stays up until whatever a player decides — which is
            // what a subtitle must never do. So a packet that states how long it
            // lasts is written as a `BlockGroup` with the duration beside it.
            Some(duration) if self.tracks[track].media == MediaType::Subtitle => {
                // Not lacing, and "keyframe" is a `SimpleBlock` field a plain
                // `Block` does not have at all.
                block.push(0);
                block.extend_from_slice(payload);
                let mut group = Vec::new();
                elem(&mut group, ebml::BLOCK, &block);
                uint(&mut group, ebml::BLOCK_DURATION, duration.max(1) as u64);
                elem(&mut self.cluster, ebml::BLOCK_GROUP, &group);
            }
            _ => {
                block.push(if key { 0x80 } else { 0 });
                block.extend_from_slice(payload);
                elem(&mut self.cluster, ebml::SIMPLE_BLOCK, &block);
            }
        }
        // One cue per keyframe of a video track (of the first track, in a file
        // with no picture): an index naming every block of every track is three
        // times too long and seeks no better.
        let cued = match self.tracks.iter().any(|t| t.media == MediaType::Video) {
            true => video && key,
            false => track == 0 && key,
        };
        if cued {
            self.cues.push(CuePoint {
                time: ts,
                track: number,
                cluster: self.cluster_at - self.segment_body,
                relative,
            });
        }
        self.last_end = self.last_end.max(ts + duration.unwrap_or(0));
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.cluster.is_empty() {
            return Ok(());
        }
        let mut head = Vec::new();
        put_id(&mut head, ebml::CLUSTER);
        put_size(&mut head, self.cluster.len() as u64);
        let body = std::mem::take(&mut self.cluster);
        self.write(&head)?;
        self.write(&body)?;
        Ok(())
    }
}

impl<W: Write + Seek + Send> Muxer for MatroskaMuxer<W> {
    fn add_stream(&mut self, info: StreamInfo) -> Result<u32> {
        if self.pos > 0 {
            return Err(Error::corrupt(
                "Matroska: a stream declared after the first packet",
            ));
        }
        if self.tracks.len() >= MAX_TRACKS {
            return Err(Error::unsupported(
                format!("{} tracks in one Matroska file", self.tracks.len() + 1),
                "a block writes its track number in one byte",
            ));
        }
        let index = self.tracks.len() as u32;
        // Track numbers start at 1: zero is not a legal `TrackNumber`.
        let number = u64::from(index) + 1;
        let frame_ns = match &info.params.media {
            MediaParameters::Video(v) => v.frame_rate.and_then(|r| {
                r.inverse()
                    .checked_rescale(1, TimeBase::new(1, 1_000_000_000), Rounding::Nearest)
                    .map(|ns| ns.max(1) as u64)
            }),
            _ => None,
        };
        self.tracks.push(MuxTrack {
            number,
            media: info.params.codec.media_type(),
            time_base: info.time_base,
            frame_ns,
            title: String::new(),
            delay_ns: None,
        });
        let mut info = info;
        info.index = index;
        self.streams.push(info);
        Ok(index)
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let track = packet.stream as usize;
        if track >= self.tracks.len() {
            return Err(Error::corrupt(format!(
                "Matroska: packet for stream {} of {}",
                packet.stream,
                self.tracks.len()
            )));
        }
        if self.pos == 0 {
            self.write_header()?;
        }
        // The packet's own base where it carries one, the stream's otherwise:
        // a demuxer states both and an encoder often only the second.
        let base = match packet.time_base.num() > 0 {
            true => packet.time_base,
            false => self.tracks[track].time_base,
        };
        let ts = match packet.pts {
            Some(pts) => base.rescale(pts, MUX_TIME_BASE, Rounding::Nearest),
            // A packet with no timestamp at all follows the one before it, by a
            // frame where the track states a rate and by nothing where it does
            // not — which is what a player would have to assume anyway.
            None => self.last_end,
        };
        let duration = packet
            .duration
            .map(|d| base.rescale(d, MUX_TIME_BASE, Rounding::Nearest))
            .or_else(|| {
                self.tracks[track]
                    .frame_ns
                    .map(|ns| (ns / TIMESTAMP_SCALE_NS).max(1) as i64)
            });
        if packet.data.is_empty() {
            return Err(Error::corrupt("Matroska: an empty block"));
        }
        self.put(track, ts, duration, packet.flags.keyframe, &packet.data)
    }

    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        if self.pos == 0 {
            self.write_header()?;
        }
        self.flush()?;

        // The index, in the order a seek reads it. Kept as a file offset like
        // the other two: the `SeekHead` below is what makes them all relative,
        // and a position taken off the segment twice points at the middle of a
        // cluster — where a reader that follows it finds an element that cannot
        // be there and gives up on the file.
        let cues_at = self.pos;
        let mut cues = Vec::new();
        for cue in std::mem::take(&mut self.cues) {
            let mut positions = Vec::new();
            uint(&mut positions, ebml::CUE_TRACK, cue.track);
            uint(&mut positions, ebml::CUE_CLUSTER_POSITION, cue.cluster);
            uint(&mut positions, ebml::CUE_RELATIVE_POSITION, cue.relative);
            let mut point = Vec::new();
            uint(&mut point, ebml::CUE_TIME, cue.time.max(0) as u64);
            elem(&mut point, ebml::CUE_TRACK_POSITIONS, &positions);
            elem(&mut cues, ebml::CUE_POINT, &point);
        }
        let mut element = Vec::new();
        elem(&mut element, ebml::CUES, &cues);
        self.write(&element)?;

        let end = self.pos;
        // The reserved 8-byte segment size: marker bit in the top byte, value
        // under it.
        let body = end - self.segment_body;
        let size = (1u64 << 56) | body;
        self.w.seek(SeekFrom::Start(self.segment_size_at))?;
        self.w.write_all(&size.to_be_bytes())?;

        // One tick past the last block shown, which is where the file really
        // ends.
        self.w.seek(SeekFrom::Start(self.duration_at))?;
        self.w.write_all(&(self.last_end as f64).to_be_bytes())?;

        // The `SeekHead`, into the room reserved for it, with a `Void` over
        // whatever it did not need.
        let mut entries = Vec::new();
        for (id, at) in [
            (ebml::INFO, self.info_at),
            (ebml::TRACKS, self.tracks_at),
            (ebml::CUES, cues_at),
        ] {
            let mut seek = Vec::new();
            let mut id_bytes = Vec::new();
            put_id(&mut id_bytes, id);
            elem(&mut seek, ebml::SEEK_ID, &id_bytes);
            // Fixed eight bytes, not the fewest that hold it: the table is
            // written into a reservation, and a `SeekHead` whose length depends
            // on how big the file turned out is one whose `Void` filler can land
            // on a length EBML cannot spell.
            put_id(&mut seek, ebml::SEEK_POSITION);
            put_size(&mut seek, 8);
            seek.extend_from_slice(&at.saturating_sub(self.segment_body).to_be_bytes());
            elem(&mut entries, ebml::SEEK, &seek);
        }
        let mut head = Vec::new();
        elem(&mut head, ebml::SEEK_HEAD, &entries);
        let room = SEEK_HEAD_RESERVE + elem_head_len(ebml::VOID, SEEK_HEAD_RESERVE);
        if head.len() > room {
            return Err(Error::corrupt(
                "Matroska: SeekHead larger than its reservation",
            ));
        }
        // Whatever the table did not need becomes a `Void`, which needs two
        // bytes of its own at the least. The three entries are fixed-width, so
        // the leftover is a constant and this arithmetic is exact.
        match room - head.len() {
            0 => {}
            1 => {
                return Err(Error::corrupt(
                    "Matroska: one byte left over after the SeekHead",
                ));
            }
            left => {
                let payload = left - elem_head_len(ebml::VOID, left - 2);
                elem(&mut head, ebml::VOID, &vec![0u8; payload]);
            }
        }
        if head.len() != room {
            return Err(Error::corrupt(
                "Matroska: SeekHead reservation not filled exactly",
            ));
        }
        self.w.seek(SeekFrom::Start(self.seek_head_at))?;
        self.w.write_all(&head)?;
        self.w.seek(SeekFrom::Start(end))?;
        self.pos = end;
        self.w.flush()?;
        Ok(())
    }
}

/// The Matroska codec id a [`CodecId`] is written as. The reverse of the
/// reader's own table, and refusing rather than guessing is the point: a stream
/// written under the wrong id is a file that opens and plays noise.
pub(crate) fn matroska_codec_id(codec: CodecId) -> Result<&'static str> {
    Ok(match codec {
        CodecId::H264 => "V_MPEG4/ISO/AVC",
        CodecId::H265 => "V_MPEGH/ISO/HEVC",
        CodecId::Vp8 => "V_VP8",
        CodecId::Vp9 => "V_VP9",
        CodecId::Av1 => "V_AV1",
        CodecId::Aac => "A_AAC",
        CodecId::Ac3 => "A_AC3",
        CodecId::EAc3 => "A_EAC3",
        CodecId::Alac => "A_ALAC",
        CodecId::Flac => "A_FLAC",
        CodecId::Mp3 => "A_MPEG/L3",
        CodecId::Opus => "A_OPUS",
        CodecId::Vorbis => "A_VORBIS",
        CodecId::PcmU8 | CodecId::PcmS16Le | CodecId::PcmS24Le | CodecId::PcmS32Le => {
            "A_PCM/INT/LIT"
        }
        CodecId::PcmS16Be => "A_PCM/INT/BIG",
        CodecId::PcmF32Le => "A_PCM/FLOAT/IEEE",
        CodecId::Srt => "S_TEXT/UTF8",
        CodecId::WebVtt => "S_TEXT/WEBVTT",
        CodecId::Ass => "S_TEXT/ASS",
        CodecId::Pgs => "S_HDMV/PGS",
        codec => {
            return Err(Error::unsupported(
                format!("{} in Matroska", codec.name()),
                "the format defines no codec id for it",
            ));
        }
    })
}
