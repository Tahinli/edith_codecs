//! The public decoder: DTS core frames (any stream word order, with or
//! without DTS-HD substreams behind them) in, interleaved `f32` frames out.

use ec_core::{
    AudioFrame, AudioParameters, Buf, ChannelLayout, ChannelPosition, CodecId, CodecParameters,
    Decoder, Error, Frame, MediaParameters, Packet, Result, SampleFormat, Timestamp,
};

use crate::core::{Core, Extensions, SPK_C, SPK_CS, SPK_L, SPK_LFE, SPK_LS, SPK_R, SPK_RS};
use crate::exss::{parse_exss, ExssInfo, EXSS_LBR, EXSS_X96, EXSS_XBR, EXSS_XXCH};
use crate::header::{
    find_sync, parse_canonical, to_canonical, FrameHeader, StreamFormat, SYNC_EXSS,
};

/// Decoder knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    /// Refuse a frame whose DTS-HD substream carries XLL (lossless) data
    /// instead of decoding its backward compatible core. Off by default: the
    /// core is always the output, and [`FrameInfo::xll`] says that a lossless
    /// layer was present and not decoded.
    pub refuse_xll: bool,
}

/// What a decoded frame said about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameInfo {
    /// The core frame header.
    pub header: FrameHeader,
    /// Core extensions that were decoded.
    pub extensions: Extensions,
    /// A DTS-HD extension substream followed the core.
    pub hd_substream: bool,
    /// The substream carried XLL (lossless) data. It is not decoded: the
    /// output is the core, a lossy rendering of the same signal.
    pub xll: bool,
    /// The substream carried components this decoder does not decode (XBR,
    /// XXCH, LBR or an X96 that the core did not signal).
    pub hd_extensions_ignored: bool,
    /// Samples per channel in the output frame.
    pub samples: usize,
    /// Output sample rate (twice the core rate when X96 was decoded).
    pub sample_rate: u32,
}

/// A DTS decoder.
pub struct DtsDecoder {
    core: Core,
    params: CodecParameters,
    options: Options,
    pending: Option<Frame>,
    info: Option<FrameInfo>,
    flushed: bool,
}


impl Default for DtsDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl DtsDecoder {
    /// A decoder with default [`Options`].
    pub fn new() -> DtsDecoder {
        DtsDecoder::with_options(Options::default())
    }

    /// A decoder with explicit options.
    pub fn with_options(options: Options) -> DtsDecoder {
        DtsDecoder {
            core: Core::new(),
            params: CodecParameters::new(CodecId::Dts),
            options,
            pending: None,
            info: None,
            flushed: false,
        }
    }

    /// The options in force; changes take effect on the next packet.
    pub fn options_mut(&mut self) -> &mut Options {
        &mut self.options
    }

    /// What the last decoded frame's header said.
    pub fn frame_info(&self) -> Option<&FrameInfo> {
        self.info.as_ref()
    }

    /// Decode one packet holding a core frame, possibly followed by a DTS-HD
    /// extension substream, into one audio frame.
    ///
    /// `data` may start with padding: the first core sync word in any of the
    /// four stream layouts is the frame start. A packet that begins with a
    /// bare extension substream and no core is [`Error::Unsupported`].
    pub fn decode_frame(&mut self, data: &[u8]) -> Result<AudioFrame> {
        let Some((start, format)) = find_sync(data) else {
            if data.len() >= 4 && u32::from_be_bytes([data[0], data[1], data[2], data[3]]) == SYNC_EXSS
            {
                let exss = parse_exss(data)?;
                if exss.has_xll() && self.options.refuse_xll {
                    return Err(xll_error());
                }
                return Err(Error::unsupported(
                    "DTS-HD substream without a backward compatible core",
                    "only the core component is decoded and none precedes this substream",
                ));
            }
            return Err(if data.len() < 4 {
                Error::NeedMore
            } else {
                Error::corrupt("DTS: no core sync word in the packet")
            });
        };
        let data = &data[start..];
        let hdr0 = crate::header::parse_frame_header(data)?;
        let take = hdr0.stream_frame_size.min(data.len());
        let mut canon = Vec::with_capacity(hdr0.frame_size + 4);
        to_canonical(format, &data[..take], &mut canon);
        let hdr = parse_canonical(format, &canon)?;
        if canon.len() < hdr.frame_size && matches!(format, StreamFormat::Be16 | StreamFormat::Le16) {
            return Err(Error::NeedMore);
        }

        // DTS-HD substream behind the core (4-byte aligned).
        let mut exss: Option<ExssInfo> = None;
        let mut exss_bytes: Option<Vec<u8>> = None;
        if matches!(format, StreamFormat::Be16 | StreamFormat::Le16) {
            let after = hdr.frame_size.next_multiple_of(4);
            if data.len() >= after + 4 {
                let tail = &data[after..];
                // The substream is stored in the stream's own byte order only
                // for BE data; an LE stream's substream is swapped as well.
                let sw;
                let tail = if format == StreamFormat::Le16 {
                    let mut v = Vec::new();
                    to_canonical(StreamFormat::Le16, tail, &mut v);
                    sw = v;
                    &sw[..]
                } else {
                    tail
                };
                if tail.len() >= 4
                    && u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]) == SYNC_EXSS
                {
                    exss = parse_exss(tail).ok();
                    if exss.is_some() {
                        exss_bytes = Some(tail.to_vec());
                    }
                }
            }
        }
        let xll = exss.as_ref().is_some_and(|e| e.has_xll());
        if xll && self.options.refuse_xll {
            return Err(xll_error());
        }
        let mut ignored = false;
        if let Some(e) = &exss {
            for a in &e.assets {
                if a.extension_mask & (EXSS_XBR | EXSS_XXCH | EXSS_LBR) != 0 {
                    ignored = true;
                }
            }
        }

        let x96: Option<&[u8]> = exss_bytes.as_deref().zip(exss.as_ref()).and_then(|(b, e)| {
            let a = e.assets.first()?;
            if a.extension_mask & EXSS_X96 != 0 {
                b.get(a.x96_offset..a.x96_offset + a.x96_size)
            } else {
                None
            }
        });
        let dec = self.core.decode(&canon, &hdr, x96)?;
        // Interleave in the family's order.
        const ORDER: [(usize, ChannelPosition); 7] = [
            (SPK_L, ChannelPosition::FrontLeft),
            (SPK_R, ChannelPosition::FrontRight),
            (SPK_C, ChannelPosition::FrontCenter),
            (SPK_LFE, ChannelPosition::Lfe),
            (SPK_LS, ChannelPosition::BackLeft),
            (SPK_RS, ChannelPosition::BackRight),
            (SPK_CS, ChannelPosition::BackCenter),
        ];
        let mut chans: Vec<usize> = Vec::new();
        let mut positions: Vec<ChannelPosition> = Vec::new();
        for (spk, pos) in ORDER {
            if dec.ch_mask & (1 << spk) != 0 {
                chans.push(spk);
                positions.push(pos);
            }
        }
        let layout = layout_of(&positions);
        let n = dec.nsamples;
        let nch = chans.len();
        let mut bytes = Vec::with_capacity(n * nch * 4);
        const SCALE: f32 = 1.0 / 8_388_608.0;
        for i in 0..n {
            for &c in &chans {
                bytes.extend_from_slice(&(dec.out[c][i] as f32 * SCALE).to_le_bytes());
            }
        }
        self.params.media = MediaParameters::Audio(AudioParameters {
            sample_rate: dec.sample_rate,
            layout: layout.clone(),
            format: Some(SampleFormat::F32),
            bits_per_sample: None,
        });
        self.info = Some(FrameInfo {
            header: hdr,
            extensions: dec.ext,
            hd_substream: exss.is_some(),
            xll,
            hd_extensions_ignored: ignored,
            samples: n,
            sample_rate: dec.sample_rate,
        });
        AudioFrame::try_new(
            SampleFormat::F32,
            false,
            layout,
            dec.sample_rate,
            n,
            vec![Buf::from_vec(bytes)],
        )
    }
}

fn xll_error() -> Error {
    Error::unsupported(
        "DTS-HD XLL (lossless) extension",
        "only the backward compatible core is decoded; clear Options::refuse_xll to output it",
    )
}

fn layout_of(positions: &[ChannelPosition]) -> ChannelLayout {
    match positions {
        [ChannelPosition::FrontCenter] => ChannelLayout::Mono,
        [ChannelPosition::FrontLeft, ChannelPosition::FrontRight] => ChannelLayout::Stereo,
        _ if positions == ChannelLayout::Surround5_1.positions() => ChannelLayout::Surround5_1,
        _ if positions == ChannelLayout::Surround7_1.positions() => ChannelLayout::Surround7_1,
        _ => ChannelLayout::Custom(positions.to_vec()),
    }
}

impl Decoder for DtsDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        let mut frame = self.decode_frame(&packet.data)?;
        frame.pts = packet.pts.map(|ts| Timestamp::new(ts, packet.time_base));
        self.pending = Some(Frame::Audio(frame));
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.pending.take() {
            Some(frame) => Ok(frame),
            None if self.flushed => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.flushed = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.core.reset();
        self.pending = None;
        self.flushed = false;
    }
}
