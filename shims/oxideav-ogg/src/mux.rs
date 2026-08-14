//! The muxing half — the only half the replica calls.

use ec_core::Muxer as _;
use ec_ogg::granule_side_data;
use oxideav_core::{Error, Muxer, Packet, Result, StreamInfo};

use crate::WriteSeek;

/// Lace header packets into the `extradata` form Vorbis setup data travels in:
/// [`None`] when they cannot be described that way.
pub fn xiph_lace(packets: &[&[u8]]) -> Option<Vec<u8>> {
    ec_ogg::xiph_lace(packets)
}

/// Open a writer over `output` with `streams` already declared.
pub fn open_concrete(output: Box<dyn WriteSeek>, streams: &[StreamInfo]) -> Result<OggMuxer> {
    let mut inner = ec_ogg::OggMuxer::new(output);
    for stream in streams {
        inner.add_stream(to_ec_stream(stream)?)?;
    }
    Ok(OggMuxer { inner })
}

/// The concrete muxer, so callers reach [`OggMuxer::set_page_target_bytes`]
/// without a downcast.
pub struct OggMuxer {
    inner: ec_ogg::OggMuxer<Box<dyn WriteSeek>>,
}

impl OggMuxer {
    /// Aim for `bytes` of body per page; [`None`] restores the default.
    pub fn set_page_target_bytes(&mut self, bytes: Option<usize>) {
        self.inner.set_page_target_bytes(bytes);
    }
}

impl Muxer for OggMuxer {
    fn format_name(&self) -> &str {
        "ogg"
    }

    fn write_header(&mut self) -> Result<()> {
        Ok(self.inner.write_headers()?)
    }

    /// The caller's `pts` *is* the granule position — where this packet ends —
    /// which is the convention the incumbent muxer wrote onto its pages. It is
    /// moved into the side data `ec_ogg` reads positions from, and the packet
    /// keeps no start timestamp, because Ogg does not carry one.
    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let mut out = packet.clone();
        if let Some(granule) = packet.pts {
            out.side_data.push(granule_side_data(granule));
        }
        out.pts = None;
        out.dts = None;
        Ok(self.inner.write_packet(&out)?)
    }

    fn write_trailer(&mut self) -> Result<()> {
        Ok(self.inner.finish()?)
    }
}

/// The replica's stream description, in family terms.
fn to_ec_stream(stream: &StreamInfo) -> Result<ec_core::StreamInfo> {
    let params = &stream.params;
    let codec = params.codec_id.to_ec().ok_or_else(|| {
        Error::Unsupported(format!(
            "Ogg mux: codec {} is not one this family carries",
            params.codec_id
        ))
    })?;
    let mut out = ec_core::CodecParameters::new(codec);
    if let ec_core::MediaParameters::Audio(audio) = &mut out.media {
        if let Some(rate) = params.sample_rate {
            audio.sample_rate = rate;
        }
        if let Some(channels) = params.channels {
            audio.layout = ec_core::ChannelLayout::from_count(usize::from(channels).max(1));
        }
    }
    if !params.extradata.is_empty() {
        out.extradata = Some(ec_core::Buf::copy_from_slice(&params.extradata));
    }
    let mut info = ec_core::StreamInfo::new(stream.index, stream.time_base, out);
    info.duration = stream.duration;
    info.start_time = stream.start_time;
    Ok(info)
}
