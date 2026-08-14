//! The four formats that are a codec and nothing else: WAV, MP3, FLAC and
//! ADTS. One demuxer serves all four, because they differ only in how a frame
//! states its own length — everything after that (the packet stream, the frame
//! index, sample-accurate seeking) is the same walk.
//!
//! Framing comes from the codec crates rather than being re-derived here:
//! [`ec_mp3::FrameHeader`], [`ec_flac::decode::find_frame_sync`] and
//! [`ec_aac::parse_adts`] each already know their own header.

use std::io::{Read, Seek, SeekFrom};

use ec_core::error::{Error, Result};
use ec_core::frame::{ChannelLayout, SampleFormat};
use ec_core::packet::{Buf, Packet, PacketFlags};
use ec_core::registry::{
    AudioParameters, CodecId, CodecParameters, Demuxer, MediaParameters, SeekMode, StreamInfo,
};
use ec_core::timebase::{Rounding, TimeBase, Timestamp};

/// Bytes read per refill.
const CHUNK: usize = 64 << 10;
/// Consumed bytes tolerated in front of the window before it is compacted.
const COMPACT_AT: usize = 256 << 10;
/// Frames of PCM in one WAV packet: big enough that a packet is not a syscall
/// per millisecond, small enough to seek inside without decoding a second.
const WAV_FRAMES: usize = 4096;

/// How a format states the length of one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Fixed-size PCM: a "frame" is one interleaved sample.
    Wav {
        block_align: usize,
    },
    Mp3,
    Flac,
    Adts,
}

/// A frame's byte length and how many inter-channel samples it holds.
struct Framing {
    bytes: usize,
    samples: u64,
}

/// One elementary audio stream read straight out of a file.
pub(crate) struct RawDemuxer<R> {
    r: R,
    kind: Kind,
    streams: Vec<StreamInfo>,
    /// Offset of the first frame and one past the last audio byte.
    start: u64,
    end: u64,
    /// Offset the window starts at, and the file offset the reader itself sits
    /// at (window end).
    pos: u64,
    read_at: u64,
    buf: Vec<u8>,
    head: usize,
    /// Next packet's first sample, in the stream's own time base.
    sample: u64,
    rate: u32,
    /// `STREAMINFO`, needed to read a FLAC frame header.
    flac: Option<ec_flac::StreamInfo>,
    /// `(first sample, offset)` per frame, built on the first seek.
    index: Vec<(u64, u64)>,
    indexed: bool,
}

impl<R: Read + Seek> RawDemuxer<R> {
    pub(crate) fn new(
        mut r: R,
        kind: Kind,
        params: CodecParameters,
        start: u64,
        end: u64,
        total_samples: Option<u64>,
        flac: Option<ec_flac::StreamInfo>,
    ) -> Result<RawDemuxer<R>> {
        let rate = params
            .audio()
            .map(|a| a.sample_rate)
            .filter(|r| *r > 0)
            .ok_or_else(|| Error::corrupt("a raw audio stream with no sample rate"))?;
        let mut info = StreamInfo::new(0, TimeBase::from_rate(rate), params);
        info.start_time = Some(0);
        info.duration = total_samples.map(|n| n as i64);
        r.seek(SeekFrom::Start(start))?;
        Ok(RawDemuxer {
            r,
            kind,
            streams: vec![info],
            start,
            end,
            pos: start,
            read_at: start,
            buf: Vec::with_capacity(CHUNK),
            head: 0,
            sample: 0,
            rate,
            flac,
            index: Vec::new(),
            indexed: false,
        })
    }

    /// Samples in a typical frame of this format, for the tail of an estimate.
    fn frame_samples(&self) -> u64 {
        match self.kind {
            Kind::Wav { .. } => WAV_FRAMES as u64,
            Kind::Mp3 => 1152,
            Kind::Flac => self
                .flac
                .as_ref()
                .map_or(4096, |i| u64::from(i.max_block_size)),
            Kind::Adts => 1024,
        }
    }

    /// At least `want` bytes in the window, or fewer at the end of the stream.
    fn fill(&mut self, want: usize) -> Result<()> {
        if self.head > COMPACT_AT {
            self.buf.drain(..self.head);
            self.pos += self.head as u64;
            self.head = 0;
        }
        while self.buf.len() - self.head < want && self.read_at < self.end {
            let chunk = CHUNK.min((self.end - self.read_at) as usize);
            let at = self.buf.len();
            self.buf.resize(at + chunk, 0);
            let mut got = 0;
            while got < chunk {
                match self.r.read(&mut self.buf[at + got..]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(Error::Io(e)),
                }
            }
            self.buf.truncate(at + got);
            self.read_at += got as u64;
            if got == 0 {
                break;
            }
        }
        Ok(())
    }

    /// The unread bytes of the window.
    fn window(&self) -> &[u8] {
        &self.buf[self.head..]
    }

    /// Where the window's head sits in the file.
    fn offset(&self) -> u64 {
        self.pos + self.head as u64
    }

    /// Drop the window and resume reading at `offset`.
    fn jump(&mut self, offset: u64) -> Result<()> {
        self.r.seek(SeekFrom::Start(offset))?;
        self.buf.clear();
        self.head = 0;
        self.pos = offset;
        self.read_at = offset;
        Ok(())
    }

    /// The frame at the window's head, or [`Error::Eof`] at the end.
    ///
    /// Resyncs over junk: a byte that does not open a frame is skipped, which
    /// is what carries a reader over an ID3 tag in the middle of a stream or a
    /// scrap of a torn download.
    fn framing(&mut self) -> Result<Framing> {
        let want = match self.kind {
            Kind::Wav { block_align } => block_align * WAV_FRAMES,
            // A FLAC frame is bounded by the *next* sync word, so the window
            // has to hold a whole one plus the head of its successor.
            Kind::Flac => 1 << 20,
            _ => 8192,
        };
        let flac = self.flac.clone();
        let kind = self.kind;
        let mut skipped = 0u64;
        loop {
            self.fill(want)?;
            let left = self.window().len();
            if left == 0 {
                return Err(Error::Eof);
            }
            let tail = self.read_at >= self.end;
            match frame_at(kind, self.window(), flac.as_ref()) {
                Ok(f) => return Ok(f),
                Err(e) if e.is_need_more() && tail => {
                    // The tail of a file: whatever is left is the last frame.
                    return Ok(Framing {
                        bytes: left,
                        samples: self.frame_samples(),
                    });
                }
                Err(_) if skipped < (1 << 20) => {
                    self.head += 1;
                    skipped += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Walk every frame once, recording where each starts.
    fn build_index(&mut self) -> Result<()> {
        if self.indexed {
            return Ok(());
        }
        let (at, sample) = (self.offset(), self.sample);
        self.jump(self.start)?;
        let mut sample_at = 0u64;
        while self.offset() < self.end {
            let f = match self.framing() {
                Ok(f) => f,
                Err(e) if e.is_eof() => break,
                Err(e) => return Err(e),
            };
            // `framing` may have resynced past junk: index where the frame
            // really starts, not where the walk was standing.
            self.index.push((sample_at, self.offset()));
            sample_at += f.samples;
            self.head += f.bytes;
        }
        self.indexed = true;
        // Put the reader back where the caller had it.
        self.jump(at)?;
        self.sample = sample;
        Ok(())
    }
}

/// One frame's geometry, read from the head of `data`.
fn frame_at(kind: Kind, data: &[u8], flac: Option<&ec_flac::StreamInfo>) -> Result<Framing> {
    match kind {
        Kind::Wav { block_align } => {
            let frames = (data.len() / block_align).min(WAV_FRAMES);
            match frames {
                0 => Err(Error::Eof),
                n => Ok(Framing {
                    bytes: n * block_align,
                    samples: n as u64,
                }),
            }
        }
        Kind::Mp3 => {
            let header = ec_mp3::FrameHeader::parse(data)?;
            let bytes = header
                .frame_len()
                .ok_or_else(|| Error::corrupt("mp3: a free-format frame states no length"))?;
            if bytes > data.len() {
                return Err(Error::NeedMore);
            }
            Ok(Framing {
                bytes,
                samples: header.samples_per_frame() as u64,
            })
        }
        Kind::Adts => {
            let header = ec_aac::parse_adts(data)?;
            if header.frame_length > data.len() {
                return Err(Error::NeedMore);
            }
            Ok(Framing {
                bytes: header.frame_length,
                samples: 1024 * u64::from(header.raw_blocks),
            })
        }
        Kind::Flac => {
            if ec_flac::decode::find_frame_sync(data, 0, flac) != Some(0) {
                return Err(Error::corrupt("flac: not a frame header"));
            }
            let mut r = ec_core::BitReader::new(data);
            let header = ec_flac::decode::parse_frame_header(&mut r, flac)?;
            // A frame ends where the next one starts; the last frame of a file
            // ends at the file's end, which the caller's `NeedMore` handles.
            let bytes = ec_flac::decode::find_frame_sync(data, 1, flac).ok_or(Error::NeedMore)?;
            Ok(Framing {
                bytes,
                samples: header.block_size as u64,
            })
        }
    }
}

impl<R: Read + Seek + Send> Demuxer for RawDemuxer<R> {
    fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    fn next_packet(&mut self) -> Result<Packet> {
        if self.offset() >= self.end {
            return Err(Error::Eof);
        }
        let f = self.framing()?;
        let data = Buf::copy_from_slice(&self.window()[..f.bytes]);
        self.head += f.bytes;
        let pts = self.sample;
        self.sample += f.samples;
        let mut packet = Packet::new(0, TimeBase::from_rate(self.rate), data);
        packet.pts = Some(pts as i64);
        packet.dts = Some(pts as i64);
        packet.duration = Some(f.samples as i64);
        packet.flags = PacketFlags {
            keyframe: true,
            ..PacketFlags::default()
        };
        Ok(packet)
    }

    fn seek(&mut self, _stream: u32, to: Timestamp, mode: SeekMode) -> Result<()> {
        let target = to
            .rescale(TimeBase::from_rate(self.rate), Rounding::Down)
            .ticks
            .max(0) as u64;
        // PCM needs no index and no rounding: every sample is a random access
        // point, at a byte offset arithmetic gives.
        if let Kind::Wav { block_align } = self.kind {
            let at = self.start + target * block_align as u64;
            self.jump(at.min(self.end))?;
            self.sample = target;
            return Ok(());
        }
        self.build_index()?;
        let found = self.index.partition_point(|(s, _)| *s <= target);
        let i = match mode {
            SeekMode::SyncAfter => found.min(self.index.len().saturating_sub(1)),
            // At or before: the frame the target sits inside.
            _ => found.saturating_sub(1),
        };
        let (sample, at) = match self.index.get(i) {
            Some(&entry) => entry,
            None => return Err(Error::Eof),
        };
        self.jump(at)?;
        self.sample = sample;
        Ok(())
    }
}

/// Codec parameters for a WAVE stream, whose `fmt ` chunk names its codec
/// outright.
pub(crate) fn wav_parameters(spec: ec_riff::WavSpec) -> Result<(CodecParameters, usize)> {
    let format = spec.sample_format()?;
    let codec = match (format, spec.bits_per_sample) {
        (SampleFormat::F32, _) => CodecId::PcmF32Le,
        (_, 8) => CodecId::PcmU8,
        (_, 16) => CodecId::PcmS16Le,
        (_, 24) => CodecId::PcmS24Le,
        (_, 32) => CodecId::PcmS32Le,
        (_, bits) => {
            return Err(Error::unsupported(
                format!("a {bits}-bit WAVE file"),
                "only 8, 16, 24 and 32 bit samples have a PCM codec id",
            ));
        }
    };
    let mut params = CodecParameters::new(codec);
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: spec.sample_rate,
        layout: spec.layout(),
        format: Some(format),
        bits_per_sample: Some(u32::from(spec.bits_per_sample)),
    });
    Ok((params, spec.block_align()))
}

/// Codec parameters for an ADTS stream, from its first frame header.
pub(crate) fn adts_parameters(header: &ec_aac::AdtsHeader) -> CodecParameters {
    let mut params = CodecParameters::new(CodecId::Aac);
    params.media = MediaParameters::Audio(AudioParameters {
        sample_rate: header.sample_rate,
        layout: ChannelLayout::from_count(usize::from(header.channels.max(1))),
        format: Some(SampleFormat::F32),
        bits_per_sample: None,
    });
    // ADTS states its configuration in every frame, but a decoder built from
    // parameters alone still wants the ASC form of it.
    params.extradata = Some(Buf::from_vec(ec_aac::audio_specific_config_bytes(
        header.sample_rate,
        header.channels,
    )));
    params
}

/// Codec parameters for an MP3 stream, from its first frame header.
pub(crate) fn mp3_parameters(header: &ec_mp3::FrameHeader) -> CodecParameters {
    ec_mp3::codec_parameters(header.sample_rate, header.channels())
}
