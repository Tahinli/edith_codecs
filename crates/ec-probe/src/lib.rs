//! Format detection and one reader for every audio file edith opens.
//!
//! [`Reader`] sniffs the content (never the extension — a mislabelled file is
//! still a file), builds the right demuxer, and hands out packets with rational
//! timestamps plus a sample-accurate [`Reader::seek`] that reports where it
//! actually landed. [`AudioDecoder`] is the registry half: one seat for every
//! audio codec the family carries.
//!
//! What this crate does *not* do is parse containers a sibling crate already
//! parses. Matroska goes to [`ec_matroska`], mp4 to [`ec_mp4`], Ogg to
//! [`ec_ogg`]; there is exactly one EBML parser in this family and it is not
//! here. What is implemented in [`raw`] is the four formats that are a codec
//! and nothing else: WAV, MP3, FLAC and ADTS.
//!
//! Two contracts worth knowing:
//!
//! - **One reader, always.** Opening takes a `Read + Seek` source and never
//!   reopens it, whatever a seek does. Seeking a Matroska file a hundred times
//!   costs one file handle and one cue table.
//! - **A track this cannot decode is listed, not hidden.** [`Reader::streams`]
//!   reports every stream the container declares and
//!   [`Reader::unsupported`] names the ones with no decoder *and why* — a
//!   DTS track says "DTS", not silence. Cover art is not a video track
//!   and never appears as one.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod decoder;
mod raw;
pub mod tags;

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use ec_core::error::{Error, Result};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, Demuxer, MediaType, SeekMode, StreamInfo};
use ec_core::timebase::{Rounding, TimeBase, Timestamp};

pub use decoder::{AudioDecoder, opus_layout, opus_pre_skip};
pub use tags::Tags;

/// Bytes read to sniff a format and to find the first frame of a raw stream.
const HEAD: usize = 64 << 10;
/// Packets a seek may look past to find one of the stream it was asked about.
const SEEK_LOOKAHEAD: usize = 64;
/// Samples of synthesis delay every Layer III decoder carries, on top of
/// whatever delay the encoder wrote into the LAME tag. The number is the format
/// 's, not this implementation's: a LAME tag is written to be read with it.
const MP3_DECODER_DELAY: u32 = 529;

/// A container (or the lack of one) this crate can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// RIFF/WAVE PCM.
    Wav,
    /// Native FLAC.
    Flac,
    /// MPEG-1/2/2.5 Layer III, with or without an ID3 tag.
    Mp3,
    /// Raw AAC in ADTS framing.
    Adts,
    /// Ogg: Vorbis, Opus or FLAC inside it.
    Ogg,
    /// ISO base media: mp4, m4a, mov.
    Mp4,
    /// Matroska and WebM.
    Matroska,
}

impl Format {
    /// Short lowercase name for logs and capability tables.
    pub fn name(&self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::Flac => "flac",
            Format::Mp3 => "mp3",
            Format::Adts => "adts",
            Format::Ogg => "ogg",
            Format::Mp4 => "mp4",
            Format::Matroska => "matroska",
        }
    }

    /// Extensions that usually carry this format. Advisory only: the sniff
    /// decides.
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Format::Wav => &["wav", "wave"],
            Format::Flac => &["flac"],
            Format::Mp3 => &["mp3"],
            Format::Adts => &["aac", "adts"],
            Format::Ogg => &["ogg", "oga", "opus"],
            Format::Mp4 => &["mp4", "m4a", "m4v", "mov"],
            Format::Matroska => &["mkv", "mka", "mks", "mk3d", "webm"],
        }
    }
}

/// The format `head` opens with, by content.
///
/// An ID3v2 tag is skipped first, because `.aac` and `.mp3` files both carry
/// one and the format is whatever follows it.
pub fn sniff(head: &[u8]) -> Option<Format> {
    let at = tags::id3v2_len(head).unwrap_or(0) as usize;
    let data = head.get(at..).filter(|d| !d.is_empty()).unwrap_or(head);
    if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WAVE" {
        return Some(Format::Wav);
    }
    if data.starts_with(b"fLaC") {
        return Some(Format::Flac);
    }
    if data.starts_with(b"OggS") {
        return Some(Format::Ogg);
    }
    if ec_matroska::is_matroska(data) {
        return Some(Format::Matroska);
    }
    if ec_mp4::is_mp4(data) {
        return Some(Format::Mp4);
    }
    // Whichever of the two framed formats starts *first*, and only where three
    // frames chain: AAC payload bytes routinely parse as a lone MPEG audio
    // header, which is how an `.aac` file came out an `.mp3` once.
    for at in 0..data.len().saturating_sub(8).min(SYNC_SCAN) {
        if data[at] != 0xFF {
            continue;
        }
        for format in [Format::Adts, Format::Mp3] {
            if chains(&data[at..], format) {
                return Some(format);
            }
        }
    }
    None
}

/// Offsets a sync search looks through before giving up.
const SYNC_SCAN: usize = 8 << 10;
/// Consecutive frames that must parse for a sync to be believed.
const SYNC_CHAIN: usize = 3;

/// One frame's length, as its own format states it.
fn frame_len(data: &[u8], format: Format) -> Option<usize> {
    match format {
        Format::Mp3 => ec_mp3::FrameHeader::parse(data).ok()?.frame_len(),
        Format::Adts => Some(ec_aac::parse_adts(data).ok()?.frame_length),
        _ => None,
    }
}

/// True when [`SYNC_CHAIN`] frames of `format` chain from the head of `data`
/// (or the data runs out first, which is as much as a head can prove).
fn chains(data: &[u8], format: Format) -> bool {
    let mut at = 0;
    for _ in 0..SYNC_CHAIN {
        match frame_len(&data[at..], format) {
            Some(len) if len > 0 => at += len,
            _ => return false,
        }
        if at >= data.len() {
            return true;
        }
    }
    true
}

/// The first offset where a frame of `format` really starts.
fn frame_sync(data: &[u8], format: Format) -> Option<usize> {
    (0..data.len().saturating_sub(8).min(SYNC_SCAN))
        .find(|&at| data[at] == 0xFF && chains(&data[at..], format))
}

/// A stream nothing here decodes, and the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// Index in [`Reader::streams`].
    pub stream: u32,
    /// What the container says it is.
    pub codec: CodecId,
    /// Why no decoder was built, in words a user can read.
    pub reason: String,
}

/// One open media file: streams in, packets out.
pub struct Reader {
    format: Format,
    inner: Box<dyn Demuxer>,
    tags: Tags,
    /// Packets read ahead of the caller by a seek looking for its landing.
    queue: VecDeque<Packet>,
    /// Container-native track id per stream index — a Matroska `TrackNumber`,
    /// an mp4 `track_ID`, or the index itself.
    native: Vec<u64>,
}

impl Reader {
    /// Open `path`, sniffing its content for the format.
    pub fn open(path: impl AsRef<Path>) -> Result<Reader> {
        let path = path.as_ref();
        let hint = path.extension().and_then(|e| e.to_str()).map(str::to_owned);
        Reader::new(BufReader::new(File::open(path)?), hint.as_deref())
    }

    /// Open an already-open source. `hint` is an extension, used only when the
    /// content says nothing.
    pub fn new<R: Read + Seek + Send + 'static>(mut src: R, hint: Option<&str>) -> Result<Reader> {
        let end = src.seek(SeekFrom::End(0))?;
        src.rewind()?;
        let mut head = vec![0u8; HEAD.min(end as usize)];
        read_upto(&mut src, &mut head)?;
        let format = sniff(&head)
            .or_else(|| {
                let hint = hint?.to_ascii_lowercase();
                [
                    Format::Wav,
                    Format::Flac,
                    Format::Mp3,
                    Format::Adts,
                    Format::Ogg,
                    Format::Mp4,
                    Format::Matroska,
                ]
                .into_iter()
                .find(|f| f.extensions().contains(&hint.as_str()))
            })
            .ok_or_else(|| {
                Error::unsupported(
                    "this file",
                    "its first bytes match no format this build reads (wav, flac, mp3, adts, ogg, mp4, matroska)",
                )
            })?;
        src.rewind()?;
        match format {
            Format::Matroska => {
                let demuxer = ec_matroska::MatroskaDemuxer::new(src)?;
                let native = demuxer
                    .streams()
                    .iter()
                    .map(|s| demuxer.track_number(s.index).unwrap_or(u64::from(s.index)))
                    .collect();
                Ok(Reader::wrap(
                    format,
                    Box::new(demuxer),
                    Tags::default(),
                    native,
                ))
            }
            Format::Mp4 => {
                let demuxer = ec_mp4::Mp4Demuxer::new(src)?;
                let tags = Tags {
                    title: demuxer.title().map(str::to_owned),
                    ..Tags::default()
                };
                let native = (0..demuxer.streams().len() as u64).collect();
                Ok(Reader::wrap(format, Box::new(demuxer), tags, native))
            }
            Format::Ogg => {
                let demuxer = ec_ogg::OggDemuxer::open(src)?;
                // Vorbis carries its comment packet inside the header triplet
                // this crate publishes as extradata; Opus does not publish its
                // tags packet at all, so an `.opus` file reports none.
                let tags = demuxer
                    .streams()
                    .iter()
                    .find(|s| s.params.codec == CodecId::Vorbis)
                    .and_then(|s| s.params.extradata.as_deref())
                    .and_then(|data| ec_ogg::xiph_unlace(data).ok())
                    .and_then(|packets| packets.get(1).map(|p| tags::from_vorbis_comment(&p[7..])))
                    .unwrap_or_default();
                let native = (0..demuxer.streams().len() as u64).collect();
                Ok(Reader::wrap(format, Box::new(demuxer), tags, native))
            }
            Format::Wav => open_wav(src, end),
            Format::Flac => open_flac(src, &head, end),
            Format::Mp3 => open_mp3(src, &head, end),
            Format::Adts => open_adts(src, &head, end),
        }
    }

    fn wrap(format: Format, inner: Box<dyn Demuxer>, tags: Tags, native: Vec<u64>) -> Reader {
        Reader {
            format,
            inner,
            tags,
            queue: VecDeque::new(),
            native,
        }
    }

    /// Which format the content turned out to be.
    pub fn format(&self) -> Format {
        self.format
    }

    /// Every stream the container declares, decodable or not.
    pub fn streams(&self) -> &[StreamInfo] {
        self.inner.streams()
    }

    /// The stream of `kind` a caller that just wants "the audio" means: the one
    /// the container flagged ([`StreamInfo::default`]), and the first of that
    /// kind where nothing is flagged.
    ///
    /// The flag comes first because it is the whole answer to which language a
    /// dual-audio remux opens in -- a muxer that put the French track second
    /// and marked it default meant the French one, and file order would play
    /// the other.
    pub fn default_stream(&self, kind: MediaType) -> Option<&StreamInfo> {
        let of_kind = || {
            self.streams()
                .iter()
                .filter(move |s| s.params.codec.media_type() == kind)
        };
        of_kind().find(|s| s.default).or_else(|| of_kind().next())
    }

    /// The container's own id for a stream: a Matroska `TrackNumber`, which is
    /// how a caller names one language of a dual-audio file.
    pub fn native_id(&self, stream: u32) -> u64 {
        self.native
            .get(stream as usize)
            .copied()
            .unwrap_or(u64::from(stream))
    }

    /// The stream index carrying container-native id `id`.
    pub fn stream_of_native(&self, id: u64) -> Option<u32> {
        self.native.iter().position(|&n| n == id).map(|i| i as u32)
    }

    /// What the file says about itself. Empty for a file with no tags, and for
    /// the formats [`tags`] does not read.
    pub fn tags(&self) -> &Tags {
        &self.tags
    }

    /// The longest stream's duration, when any stream states or implies one.
    ///
    /// What a player would play: [`StreamInfo::initial_padding`] is off it,
    /// because an MP3's encoder delay is silence the file carries and nobody
    /// hears.
    pub fn duration(&self) -> Option<Timestamp> {
        self.streams()
            .iter()
            .filter_map(|s| {
                s.duration.map(|d| {
                    // `initial_padding` is a sample count, not a `time_base`
                    // tick count — the two only coincide when the stream's
                    // time base happens to be `1/sample_rate`, which a
                    // container's own duration field usually is not.
                    let padding_ticks = s
                        .params
                        .audio()
                        .filter(|_| s.initial_padding > 0)
                        .map(|a| {
                            TimeBase::from_rate(a.sample_rate).rescale(
                                i64::from(s.initial_padding),
                                s.time_base,
                                Rounding::Down,
                            )
                        })
                        .unwrap_or(0);
                    Timestamp::new(d - padding_ticks, s.time_base)
                })
            })
            .max_by(|a, b| {
                a.as_secs_f64()
                    .partial_cmp(&b.as_secs_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Every audio stream with no decoder behind it, and why.
    ///
    /// Non-audio streams are not listed: this is an audio probe, and a file's
    /// picture is another crate's business.
    pub fn unsupported(&self) -> Vec<Unsupported> {
        self.streams()
            .iter()
            .filter(|s| s.params.codec.media_type() == MediaType::Audio)
            .filter_map(|s| match AudioDecoder::new(&s.params) {
                Ok(_) => None,
                Err(e) => Some(Unsupported {
                    stream: s.index,
                    codec: s.params.codec,
                    reason: e.to_string(),
                }),
            })
            .collect()
    }

    /// A decoder for one stream.
    pub fn make_decoder(&self, stream: u32) -> Result<AudioDecoder> {
        let info = self
            .streams()
            .iter()
            .find(|s| s.index == stream)
            .ok_or_else(|| Error::corrupt(format!("no stream {stream} in this file")))?;
        AudioDecoder::new(&info.params)
    }

    /// The next packet in storage order, or [`Error::Eof`].
    pub fn next_packet(&mut self) -> Result<Packet> {
        match self.queue.pop_front() {
            Some(packet) => Ok(packet),
            None => self.inner.next_packet(),
        }
    }

    /// Seek `stream` to `to` and answer where the reader actually landed.
    ///
    /// The landing is the next packet's own timestamp, so a caller can tell
    /// how far ahead of (or behind) its target the container put it — which is
    /// the number a sample-accurate segment start is trimmed against. Nothing
    /// is reopened and no packet is dropped: packets of other streams read on
    /// the way to the landing are queued, not discarded.
    pub fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<Timestamp> {
        self.queue.clear();
        self.inner.seek(stream, to, mode)?;
        let base = self
            .streams()
            .iter()
            .find(|s| s.index == stream)
            .map(|s| s.time_base)
            .unwrap_or(TimeBase::MILLIS);
        for _ in 0..SEEK_LOOKAHEAD {
            let packet = match self.inner.next_packet() {
                Ok(packet) => packet,
                // Seeking past the end lands at the end; the target is as good
                // an answer as any and the next read says `Eof`.
                Err(e) if e.is_eof() => return Ok(to.rescale(base, Rounding::Down)),
                Err(e) => return Err(e),
            };
            let hit = packet.stream == stream;
            let pts = packet.pts;
            self.queue.push_back(packet);
            if hit {
                return Ok(Timestamp::new(pts.unwrap_or(0), base));
            }
        }
        Ok(to.rescale(base, Rounding::Down))
    }
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field("format", &self.format.name())
            .field("streams", &self.streams().len())
            .finish()
    }
}

/// Read until `buf` is full or the source ends; answers how much arrived.
fn read_upto(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut at = 0;
    while at < buf.len() {
        match r.read(&mut buf[at..]) {
            Ok(0) => break,
            Ok(n) => at += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(at)
}

fn open_wav<R: Read + Seek + Send + 'static>(mut src: R, end: u64) -> Result<Reader> {
    src.rewind()?;
    let (spec, frames) = {
        let reader = ec_riff::WavReader::new(&mut src)?;
        (reader.spec(), reader.duration())
    };
    let start = src.stream_position()?;
    let (params, block_align) = raw::wav_parameters(spec)?;
    // A `data` size the header did not state (a streamed writer's placeholder)
    // means the file itself is the bound.
    let audio_end = frames
        .map(|n| start + n * block_align as u64)
        .unwrap_or(end)
        .min(end);
    let total = frames.or(Some((audio_end - start) / block_align as u64));
    let demuxer = raw::RawDemuxer::new(
        src,
        raw::Kind::Wav { block_align },
        params,
        start,
        audio_end,
        total,
        None,
    )?;
    Ok(Reader::wrap(
        Format::Wav,
        Box::new(demuxer),
        Tags::default(),
        vec![0],
    ))
}

fn open_flac<R: Read + Seek + Send + 'static>(mut src: R, head: &[u8], end: u64) -> Result<Reader> {
    let base = tags::id3v2_len(head).unwrap_or(0);
    src.seek(SeekFrom::Start(base))?;
    let mut magic = [0u8; 4];
    read_upto(&mut src, &mut magic)?;
    if &magic != b"fLaC" {
        return Err(Error::corrupt("FLAC: no stream marker"));
    }
    let mut info: Option<ec_flac::StreamInfo> = None;
    let mut tags = Tags::default();
    let mut at = base + 4;
    loop {
        let mut header = [0u8; 4];
        if read_upto(&mut src, &mut header)? < 4 {
            return Err(Error::corrupt("FLAC: metadata ended before any frame"));
        }
        let last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7f;
        let len = u64::from(u32::from_be_bytes([0, header[1], header[2], header[3]])).min(1 << 24);
        let mut body = vec![0u8; len as usize];
        read_upto(&mut src, &mut body)?;
        at += 4 + len;
        match kind {
            0 => info = Some(ec_flac::StreamInfo::parse(&body)?),
            4 => tags.merge(tags::from_vorbis_comment(&body)),
            _ => {}
        }
        if last {
            break;
        }
    }
    let info = info.ok_or_else(|| Error::corrupt("FLAC: no STREAMINFO block"))?;
    let params = ec_flac::codec_parameters(&info);
    let total = match info.total_samples {
        0 => None,
        n => Some(n),
    };
    let demuxer = raw::RawDemuxer::new(src, raw::Kind::Flac, params, at, end, total, Some(info))?;
    Ok(Reader::wrap(Format::Flac, Box::new(demuxer), tags, vec![0]))
}

fn open_mp3<R: Read + Seek + Send + 'static>(mut src: R, head: &[u8], end: u64) -> Result<Reader> {
    let (start, tags) = id3(&mut src, head, end)?;
    let mut window = vec![0u8; HEAD.min((end - start) as usize)];
    src.seek(SeekFrom::Start(start))?;
    let got = read_upto(&mut src, &mut window)?;
    window.truncate(got);
    let at = frame_sync(&window, Format::Mp3)
        .ok_or_else(|| Error::corrupt("mp3: no frame in the first 64 KiB"))?;
    let header = ec_mp3::FrameHeader::parse(&window[at..])?;
    let mut start = start + at as u64;
    let audio_end = end.saturating_sub(id3v1_len(&mut src, end)?);
    // A Xing/Info header states the frame count exactly; without one the
    // duration is the bitrate estimate ffmpeg also falls back to, which is
    // right for CBR and approximate for VBR.
    let per_frame = header.samples_per_frame() as u64;
    let xing = xing_frames(&window[at..], &header);
    if xing.is_some() {
        // The Xing frame carries no audio — it is a header wearing a frame's
        // clothes, and a decoder handed it emits a granule of silence. Both
        // ffmpeg and this reader start after it.
        start += header.frame_len().unwrap_or(0) as u64;
    }
    // Gapless, as the LAME tag states it: a decoder emits the encoder delay
    // plus its own 529 samples of Layer III synthesis delay before the first
    // sample that was ever recorded, and the file runs `padding` samples past
    // the last one. Both are silence a player must not play, and a file with no
    // tag has neither — its frames are all there is to go on.
    let (padding, total) = match &xing {
        Some(x) => (
            x.delay + MP3_DECODER_DELAY,
            Some(x.frames * per_frame - u64::from(x.padding.saturating_sub(MP3_DECODER_DELAY))),
        ),
        None => (
            0,
            header
                .frame_len()
                .and_then(|len| (len > 0).then(|| (audio_end - start) / len as u64 * per_frame)),
        ),
    };
    let demuxer = raw::RawDemuxer::new(
        src,
        raw::Kind::Mp3,
        raw::mp3_parameters(&header),
        start,
        audio_end,
        total,
        None,
    )?
    .with_initial_padding(padding);
    Ok(Reader::wrap(Format::Mp3, Box::new(demuxer), tags, vec![0]))
}

fn open_adts<R: Read + Seek + Send + 'static>(mut src: R, head: &[u8], end: u64) -> Result<Reader> {
    let (start, tags) = id3(&mut src, head, end)?;
    let mut window = vec![0u8; HEAD.min((end - start) as usize)];
    src.seek(SeekFrom::Start(start))?;
    let got = read_upto(&mut src, &mut window)?;
    window.truncate(got);
    let at = frame_sync(&window, Format::Adts)
        .ok_or_else(|| Error::corrupt("aac: no ADTS frame in the first 64 KiB"))?;
    let header = ec_aac::parse_adts(&window[at..])?;
    let start = start + at as u64;
    let audio_end = end.saturating_sub(id3v1_len(&mut src, end)?);
    // ADTS states no duration anywhere: every frame is 1024 samples, so the
    // file's size over its *average* frame is the estimate — the same one
    // ffmpeg prints "estimating duration from bitrate" about. Averaged rather
    // than taken off the first frame, which on a real encode is nothing like
    // the mean (a 3-second fixture came out 5 seconds long that way).
    let (mut sum, mut count, mut walk) = (0u64, 0u64, at);
    while count < 512 {
        let Ok(h) = ec_aac::parse_adts(&window[walk..]) else {
            break;
        };
        if h.frame_length == 0 || walk + h.frame_length > window.len() {
            break;
        }
        sum += h.frame_length as u64;
        count += 1;
        walk += h.frame_length;
    }
    let total = (count > 0).then(|| (audio_end - start) * count / sum * 1024);
    let demuxer = raw::RawDemuxer::new(
        src,
        raw::Kind::Adts,
        raw::adts_parameters(&header),
        start,
        audio_end,
        total,
        None,
    )?;
    Ok(Reader::wrap(Format::Adts, Box::new(demuxer), tags, vec![0]))
}

/// Skip an ID3v2 tag at the head of a stream, reading its text on the way past.
fn id3<R: Read + Seek>(src: &mut R, head: &[u8], end: u64) -> Result<(u64, Tags)> {
    let mut tags = Tags::default();
    let start = match tags::id3v2_len(head) {
        Some(len) => {
            let len = len.min(end);
            let mut tag = vec![0u8; len as usize];
            src.rewind()?;
            read_upto(src, &mut tag)?;
            tags.merge(tags::from_id3v2(&tag));
            len
        }
        None => 0,
    };
    // The v1 block at the end fills in what v2 did not say.
    if end >= 128 {
        src.seek(SeekFrom::Start(end - 128))?;
        let mut tail = [0u8; 128];
        read_upto(src, &mut tail)?;
        tags.merge(tags::from_id3v1(&tail));
    }
    Ok((start, tags))
}

/// Bytes of ID3v1 at the end of a stream, which are not audio.
fn id3v1_len<R: Read + Seek>(src: &mut R, end: u64) -> Result<u64> {
    if end < 128 {
        return Ok(0);
    }
    src.seek(SeekFrom::Start(end - 128))?;
    let mut tail = [0u8; 3];
    read_upto(src, &mut tail)?;
    Ok(u64::from(&tail == b"TAG") * 128)
}

/// The frame count a Xing/Info header states, when the first frame carries one.
fn xing_frames(frame: &[u8], header: &ec_mp3::FrameHeader) -> Option<Xing> {
    let at = 4 + usize::from(header.crc) * 2 + header.side_info_len();
    let tag = frame.get(at..at + 8)?;
    if &tag[..4] != b"Xing" && &tag[..4] != b"Info" {
        return None;
    }
    let flags = u32::from_be_bytes([tag[4], tag[5], tag[6], tag[7]]);
    if flags & 1 == 0 {
        return None;
    }
    let n = frame.get(at + 8..at + 12)?;
    let frames = u64::from(u32::from_be_bytes([n[0], n[1], n[2], n[3]]));
    // The LAME extension sits behind whichever optional fields the flags
    // claimed -- frame count, byte count, the 100-byte seek table, quality --
    // and states the two trims 21 bytes into itself, twelve bits each.
    let mut lame = at + 12;
    lame += usize::from(flags & 2 != 0) * 4;
    lame += usize::from(flags & 4 != 0) * 100;
    lame += usize::from(flags & 8 != 0) * 4;
    let (delay, padding) = match frame.get(lame + 21..lame + 24) {
        Some(&[a, b, c]) => (
            u32::from(a) << 4 | u32::from(b) >> 4,
            (u32::from(b) & 0xF) << 8 | u32::from(c),
        ),
        _ => (0, 0),
    };
    Some(Xing {
        frames,
        delay,
        padding,
    })
}

/// What an MP3's Xing/Info header states: how long the file is, and the two
/// silences the encoder had to add to make whole frames of it.
struct Xing {
    /// Audio frames in the file, the header frame itself excluded.
    frames: u64,
    /// Encoder delay: samples of silence in front of the first real one.
    delay: u32,
    /// Encoder padding: samples of silence after the last real one.
    padding: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_numbers_name_their_formats() {
        assert_eq!(sniff(b"RIFF\0\0\0\0WAVEfmt "), Some(Format::Wav));
        assert_eq!(sniff(b"fLaC\0\0\0\x22"), Some(Format::Flac));
        assert_eq!(sniff(b"OggS\0\x02\0\0"), Some(Format::Ogg));
        assert_eq!(sniff(b"\x1a\x45\xdf\xa3\x01\0\0\0"), Some(Format::Matroska));
        assert_eq!(sniff(b"\0\0\0\x18ftypisom\0\0\x02\0"), Some(Format::Mp4));
        // MPEG-1 Layer III, 128 kbit/s, 44.1 kHz.
        assert_eq!(sniff(b"\xff\xfb\x90\x00\0\0\0\0\0\0"), Some(Format::Mp3));
        // ADTS: same sync word, the layer bits MPEG audio reserves.
        assert_eq!(
            sniff(b"\xff\xf1\x50\x80\x01\x7f\xfc\0\0\0"),
            Some(Format::Adts)
        );
        assert_eq!(sniff(b"not a media file at all"), None);
    }

    /// `initial_padding` is a *sample* count while a stream's `duration` is in
    /// `time_base` ticks -- for a Matroska file whose `TimestampScale` is not
    /// `1/sample_rate` (the common case: ticks are milliseconds), subtracting
    /// one from the other unconverted lopped a third off an AAC track's
    /// stated length (3.021 s of `CodecDelay`-bearing audio reported as 1.997
    /// s: `1024` samples read straight off as `1024` one-millisecond ticks).
    #[test]
    fn duration_converts_initial_padding_out_of_sample_units() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/audio/aac-mka-stereo-48000.mka");
        if !path.exists() {
            return; // fixtures are generated, not checked in
        }
        let reader = Reader::open(&path).unwrap();
        let d = reader.duration().expect("a stated duration").as_secs_f64();
        assert!((d - 3.0).abs() < 0.1, "duration {d}, want ~3.0s");
    }

    /// An `.aac` or `.mp3` that opens with a tag is still an `.aac` or `.mp3`.
    #[test]
    fn an_id3_tag_does_not_hide_the_format() {
        let mut file = b"ID3\x03\x00\x00\x00\x00\x00\x0a".to_vec();
        file.extend_from_slice(&[0u8; 10]);
        file.extend_from_slice(b"\xff\xfb\x90\x00\0\0\0\0\0\0");
        assert_eq!(tags::id3v2_len(&file), Some(20));
        assert_eq!(sniff(&file), Some(Format::Mp3));
    }
}
