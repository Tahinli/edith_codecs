//! Stateless hardware decoders, one per codec, behind one [`Decoder`].
//!
//! # The shape of a stateless decode
//!
//! For every picture: parse the headers in Rust, fill the codec's
//! `VAPictureParameterBuffer*` from them, fill one
//! `VASliceParameterBuffer*` per slice or tile, hand the driver the original
//! (still escaped) bitstream bytes, and submit. Between pictures the decoder
//! keeps whatever the codec spreads across frames — the H.264 DPB and its
//! reference lists, HEVC's reference picture sets, VP9's and AV1's reference
//! slots — because the driver keeps nothing.
//!
//! # Output order
//!
//! Frames leave in *display* order. H.264 and HEVC reorder through their
//! decoded picture buffer (bumping, C.4.5.3); VP9 and AV1 emit in decode order
//! with `show_existing_frame` handled. A caller that wants decode order has the
//! per-frame timestamp it fed in.

use std::collections::VecDeque;
use std::sync::Arc;

use ec_va::caps::{Entrypoint, Profile};
use ec_va::{CapReport, Config, ConfigAttrib, Context, Display, Surface, SurfaceSpec, sys};

use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::pool::SurfacePool;

mod av1;
mod dpb264;
mod h264;
mod hevc;
mod vp9;

pub use av1::Av1Decoder;
pub use h264::H264Decoder;
pub use hevc::HevcDecoder;
pub use vp9::Vp9Decoder;

/// The codecs this crate decodes in hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// H.264 / AVC, Annex B byte stream.
    H264,
    /// H.265 / HEVC, Annex B byte stream.
    H265,
    /// VP9, one frame (or superframe) per call.
    Vp9,
    /// AV1, one temporal unit per call.
    Av1,
}

/// What the stream turned out to be, once its first headers were parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    /// Coded size, a multiple of the codec's block size.
    pub coded_size: (u32, u32),
    /// Size after cropping — what a player shows.
    pub display_size: (u32, u32),
    /// Luma bit depth.
    pub bit_depth: u8,
    /// The VA profile the context was created with.
    pub profile: Profile,
    /// Surfaces allocated for this stream.
    pub num_surfaces: usize,
}

/// A hardware decoder for one elementary stream.
pub struct Decoder {
    inner: Inner,
}

enum Inner {
    H264(Box<H264Decoder>),
    H265(Box<HevcDecoder>),
    Vp9(Box<Vp9Decoder>),
    Av1(Box<Av1Decoder>),
}

impl Decoder {
    /// Open a decoder for `codec` on `display`.
    ///
    /// Nothing is allocated on the GPU yet: the surface pool and context need
    /// the stream's size and bit depth, which only its first parameter set
    /// carries, so they are created on the first [`Decoder::decode`] call.
    pub fn new(display: &Arc<Display>, codec: Codec) -> Result<Decoder> {
        let inner = match codec {
            Codec::H264 => Inner::H264(Box::new(H264Decoder::new(display))),
            Codec::H265 => Inner::H265(Box::new(HevcDecoder::new(display))),
            Codec::Vp9 => Inner::Vp9(Box::new(Vp9Decoder::new(display))),
            Codec::Av1 => Inner::Av1(Box::new(Av1Decoder::new(display))),
        };
        Ok(Decoder { inner })
    }

    /// Feed one access unit (H.264/HEVC: Annex B; VP9: one chunk; AV1: one
    /// temporal unit) and queue whatever pictures it completed.
    pub fn decode(&mut self, data: &[u8], timestamp: i64) -> Result<()> {
        match &mut self.inner {
            Inner::H264(d) => d.decode(data, timestamp),
            Inner::H265(d) => d.decode(data, timestamp),
            Inner::Vp9(d) => d.decode(data, timestamp),
            Inner::Av1(d) => d.decode(data, timestamp),
        }
    }

    /// The next frame in display order, if one is ready.
    pub fn next_frame(&mut self) -> Option<Frame> {
        match &mut self.inner {
            Inner::H264(d) => d.next_frame(),
            Inner::H265(d) => d.next_frame(),
            Inner::Vp9(d) => d.next_frame(),
            Inner::Av1(d) => d.next_frame(),
        }
    }

    /// End of stream: push every buffered picture out.
    pub fn flush(&mut self) -> Result<()> {
        match &mut self.inner {
            Inner::H264(d) => d.flush(),
            Inner::H265(d) => d.flush(),
            Inner::Vp9(d) => d.flush(),
            Inner::Av1(d) => d.flush(),
        }
        Ok(())
    }

    /// Discard all state after a seek. Parameter sets are kept.
    pub fn reset(&mut self) {
        match &mut self.inner {
            Inner::H264(d) => d.reset(),
            Inner::H265(d) => d.reset(),
            Inner::Vp9(d) => d.reset(),
            Inner::Av1(d) => d.reset(),
        }
    }

    /// Pictures inferred for H.264 `frame_num` gaps (8.2.5.2); zero for the
    /// other codecs, which have no such concept.
    pub fn gap_frames_synthesized(&self) -> u64 {
        match &self.inner {
            Inner::H264(d) => d.gap_frames_synthesized(),
            _ => 0,
        }
    }

    /// What the stream turned out to be, once a session exists.
    pub fn stream_info(&self) -> Option<StreamInfo> {
        match &self.inner {
            Inner::H264(d) => d.stream_info(),
            Inner::H265(d) => d.stream_info(),
            Inner::Vp9(d) => d.stream_info(),
            Inner::Av1(d) => d.stream_info(),
        }
    }
}

/// Frames waiting to be collected, oldest first.
///
/// A decode call produces frames rather than returning one, because one access
/// unit can complete none, one or several pictures. There is no event fd behind
/// it: nothing in this family polls a decoder from an event loop, and adding an
/// `eventfd(2)` would mean a second FFI surface in a crate whose whole unsafe
/// budget is meant to be libva.
#[derive(Debug, Default)]
pub(crate) struct ReadyFrames {
    queue: VecDeque<Frame>,
}

impl ReadyFrames {
    pub(crate) fn push(&mut self, frame: Frame) {
        self.queue.push_back(frame);
    }

    pub(crate) fn pop(&mut self) -> Option<Frame> {
        self.queue.pop_front()
    }

    pub(crate) fn clear(&mut self) {
        self.queue.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }
}

/// The GPU-side state one decoded stream needs: config, context, surfaces.
pub(crate) struct Session {
    pub(crate) context: Arc<Context>,
    /// The readback image every frame of this stream reuses.
    pub(crate) images: crate::frame::ImageCache,
    pub(crate) pool: SurfacePool,
    pub(crate) coded_size: (u32, u32),
    pub(crate) display_size: (u32, u32),
    pub(crate) bit_depth: u8,
    pub(crate) profile: Profile,
}

impl Session {
    /// Create a decode session, deriving the surface format from the *stream's*
    /// bit depth.
    ///
    /// The bit depth is the whole point of this signature. An 8-bit context on
    /// a 10-bit stream is the incumbent defect this crate exists to not repeat:
    /// the driver either refuses the config or silently decodes to NV12 and
    /// throws away two bits per sample.
    pub(crate) fn new(
        display: &Arc<Display>,
        profile: Profile,
        coded_size: (u32, u32),
        display_size: (u32, u32),
        bit_depth: u8,
        num_surfaces: usize,
    ) -> Result<Session> {
        let caps = CapReport::probe(display)?;
        let entry = caps.entry(profile, Entrypoint::VLD).ok_or_else(|| {
            Error::unsupported(
                format!("{profile:?} decode"),
                "the driver advertises no VLD entrypoint for this profile",
            )
        })?;

        let rt_format = if bit_depth > 8 {
            sys::VA_RT_FORMAT_YUV420_10
        } else {
            sys::VA_RT_FORMAT_YUV420
        };
        if !entry.supports_rt_format(rt_format) {
            return Err(Error::unsupported(
                format!("{bit_depth}-bit {profile:?} decode"),
                format!(
                    "the driver's RT format mask for this profile is {:#x}",
                    entry.rt_formats
                ),
            ));
        }

        let (w, h) = coded_size;
        if let Some(surfaces) = &entry.surfaces
            && !surfaces.allows(w, h)
        {
            return Err(Error::unsupported(
                format!("{w}x{h} {profile:?} decode"),
                format!(
                    "the driver accepts {:?}..{:?} x {:?}..{:?}",
                    surfaces.min_width,
                    surfaces.max_width,
                    surfaces.min_height,
                    surfaces.max_height
                ),
            ));
        }

        let config = Config::new(
            display,
            profile,
            Entrypoint::VLD,
            &[ConfigAttrib::rt_format(rt_format)],
        )?;

        let spec = if bit_depth > 8 {
            SurfaceSpec::p010(w, h)
        } else {
            SurfaceSpec::nv12(w, h)
        }
        .with_usage_hint(sys::VA_SURFACE_ATTRIB_USAGE_HINT_DECODER);
        let pool = SurfacePool::new(display, &spec, num_surfaces)?;
        let context = Context::new(&config, w, h, sys::VA_PROGRESSIVE, pool.targets())?;

        Ok(Session {
            context,
            images: crate::frame::ImageCache::default(),
            pool,
            coded_size,
            display_size,
            bit_depth,
            profile,
        })
    }

    /// Submit one picture: begin, render every buffer, end. No sync — the
    /// driver keeps submissions in order within a context, and the wait belongs
    /// where the pixels are actually read.
    pub(crate) fn submit(&self, target: &Arc<Surface>, buffers: Vec<ec_va::Buffer>) -> Result<()> {
        if buffers.is_empty() {
            // radeonsi answers an empty submission with INVALID_CONTEXT, which
            // reads like a driver bug report rather than what it is: a picture
            // with no slices, i.e. a stream that lost its data.
            return Err(Error::config(
                "refusing to submit a picture with no parameter buffers",
            ));
        }
        ec_va::Picture::new(&self.context, Arc::clone(target))
            .begin()?
            .render_all(buffers)?
            .end()?;
        Ok(())
    }

    pub(crate) fn info(&self) -> StreamInfo {
        StreamInfo {
            coded_size: self.coded_size,
            display_size: self.display_size,
            bit_depth: self.bit_depth,
            profile: self.profile,
            num_surfaces: self.pool.len(),
        }
    }
}
