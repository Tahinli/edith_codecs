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
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ec_va::caps::{Entrypoint, Profile};
use ec_va::va;
use ec_va::{CapReport, Config, ConfigAttrib, Context, Display, Surface, SurfaceSpec};

use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::pool::SurfacePool;

mod av1;
mod dpb264;
mod h264;
mod vp9;

#[doc(hidden)]
pub use av1::Av1Decoder;
#[doc(hidden)]
pub use av1::fuzz_build_parameters as av1_fuzz_build_parameters;
mod hevc;

#[doc(hidden)]
pub use h264::H264Decoder;
#[doc(hidden)]
pub use h264::fuzz_build_parameters as h264_fuzz_build_parameters;
#[doc(hidden)]
pub use vp9::Vp9Decoder;
#[doc(hidden)]
pub use vp9::fuzz_build_parameters as vp9_fuzz_build_parameters;
pub use hevc::HevcDecoder;
#[doc(hidden)]
pub use hevc::fuzz_build_parameters as hevc_fuzz_build_parameters;
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

/// The units of a fuzz input: a run of little-endian `u32` lengths, each followed
/// by that many bytes. A unit cut short by the end of the input is kept, and it ends
/// the walk. Lets a fuzz target feed several chunks to one persistent parser.
pub(crate) fn units(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = data;
    while let Some((len, tail)) = rest.split_first_chunk::<4>() {
        let take = (u32::from_le_bytes(*len) as usize).min(tail.len());
        out.push(&tail[..take]);
        rest = &tail[take..];
    }
    out
}

/// The RT format a decode context is created with: P010 (`YUV420_10`) for any
/// stream above 8 bits, HEVC Main 10 included, and 8-bit 4:2:0 otherwise. A
/// context that asks for 8-bit only cannot take a 10-bit surface.
pub(crate) fn rt_format_for_bit_depth(bit_depth: u8) -> u32 {
    if bit_depth > 8 {
        va::VA_RT_FORMAT_YUV420_10
    } else {
        va::VA_RT_FORMAT_YUV420
    }
}

/// A hardware decoder for one elementary stream.
pub struct Decoder {
    inner: Inner,
}

enum Inner {
    H264(Box<H264Decoder>),
    Vp9(Box<Vp9Decoder>),
    Av1(Box<Av1Decoder>),
    H265(Box<HevcDecoder>),
}

impl Decoder {
    /// Open a decoder for `codec` on `display`.
    ///
    /// Nothing is allocated on the GPU yet: the surface pool and context need
    /// the stream's size and bit depth, which only its first parameter set
    /// carries, so they are created on the first [`Decoder::decode`] call.
    pub fn new(display: &Rc<Display>, codec: Codec) -> Result<Decoder> {
        let inner = match codec {
            Codec::H264 => Inner::H264(Box::new(H264Decoder::new(display))),
            Codec::Vp9 => Inner::Vp9(Box::new(Vp9Decoder::new(display))),
            Codec::Av1 => Inner::Av1(Box::new(Av1Decoder::new(display))),
            Codec::H265 => Inner::H265(Box::new(HevcDecoder::new(display))),
        };
        Ok(Decoder { inner })
    }

    /// Feed one access unit (H.264/HEVC: Annex B; VP9: one chunk; AV1: one
    /// temporal unit) and queue whatever pictures it completed.
    pub fn decode(&mut self, data: &[u8], timestamp: i64) -> Result<()> {
        match &mut self.inner {
            Inner::H264(d) => d.decode(data, timestamp),
            Inner::Vp9(d) => d.decode(data, timestamp),
            Inner::Av1(d) => d.decode(data, timestamp),
            Inner::H265(d) => d.decode(data, timestamp),
        }
    }

    /// The next frame in display order, if one is ready.
    pub fn next_frame(&mut self) -> Option<Frame> {
        match &mut self.inner {
            Inner::H264(d) => d.next_frame(),
            Inner::Vp9(d) => d.next_frame(),
            Inner::Av1(d) => d.next_frame(),
            Inner::H265(d) => d.next_frame(),
        }
    }

    /// End of stream: push every buffered picture out.
    pub fn flush(&mut self) -> Result<()> {
        match &mut self.inner {
            Inner::H264(d) => d.flush(),
            Inner::Vp9(d) => d.flush(),
            Inner::Av1(d) => d.flush(),
            Inner::H265(d) => d.flush(),
        }
        Ok(())
    }

    /// Discard all state after a seek. Parameter sets are kept.
    pub fn reset(&mut self) {
        match &mut self.inner {
            Inner::H264(d) => d.reset(),
            Inner::Vp9(d) => d.reset(),
            Inner::Av1(d) => d.reset(),
            Inner::H265(d) => d.reset(),
        }
    }

    /// Pictures inferred for H.264 `frame_num` gaps (8.2.5.2); zero for the
    /// other codecs, which have no such concept.
    pub fn gap_frames_synthesized(&self) -> u64 {
        match &self.inner {
            Inner::H264(d) => d.gap_frames_synthesized(),
            Inner::Vp9(_) | Inner::Av1(_) => 0,
            Inner::H265(_) => 0,
        }
    }

    /// What the stream turned out to be, once a session exists.
    pub fn stream_info(&self) -> Option<StreamInfo> {
        match &self.inner {
            Inner::H264(d) => d.stream_info(),
            Inner::Vp9(d) => d.stream_info(),
            Inner::Av1(d) => d.stream_info(),
            Inner::H265(d) => d.stream_info(),
        }
    }

    /// This stream's colour metadata, once a session exists. [`None`] for VP9
    /// and AV1: neither's sequence header colour config is parsed by this
    /// family yet.
    pub fn colour(&self) -> Option<crate::frame::Colour> {
        match &self.inner {
            Inner::H264(d) => d.colour(),
            Inner::Vp9(_) | Inner::Av1(_) => None,
            Inner::H265(d) => d.colour(),
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
/// A pending picture older than this is settled before the next submit, so a
/// session that sat idle never submits behind an unchecked picture.
const IDLE_SETTLE: Duration = Duration::from_millis(250);

/// VA statuses that mean the context or the hardware is gone, not that one
/// parameter was wrong.
fn context_gone(status: i32) -> bool {
    status_is(
        status,
        &[
            va::VA_STATUS_ERROR_INVALID_CONTEXT,
            va::VA_STATUS_ERROR_OPERATION_FAILED,
            va::VA_STATUS_ERROR_HW_BUSY,
            va::VA_STATUS_ERROR_TIMEDOUT,
            va::VA_STATUS_ERROR_DECODING_ERROR,
        ],
    )
}

/// Whether a `VAStatus` is one of `codes` (libva's constants are unsigned,
/// `VAStatus` is signed).
fn status_is(status: i32, codes: &[u32]) -> bool {
    codes.iter().any(|&c| c as i32 == status)
}

/// A picture submitted but not yet waited for.
struct Pending {
    surface: Rc<Surface>,
    since: Instant,
}

pub(crate) struct Session {
    /// The process-wide reset epoch this session was created in. Once it moves
    /// the context has been reset along with the guilty one.
    born_epoch: u64,
    /// The last submitted picture, waited for after the next one is submitted
    /// (depth-1 pipelining; see [`Session::submit`]).
    pending: std::sync::Mutex<Option<Pending>>,
    /// Set once a picture hung or lost the GPU context. Every submission after
    /// that is refused: on Mesa, submitting to a context the kernel has reset
    /// aborts the whole process from the driver's own thread, which no
    /// `catch_unwind` can stop.
    pub(crate) poisoned: std::sync::atomic::AtomicBool,
    pub(crate) context: Rc<Context>,
    /// The readback image every frame of this stream reuses.
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
        display: &Rc<Display>,
        profile: Profile,
        coded_size: (u32, u32),
        display_size: (u32, u32),
        bit_depth: u8,
        num_surfaces: usize,
    ) -> Result<Session> {
        let born_epoch = crate::reset::gpu_reset_epoch();
        if crate::reset::gpu_recently_reset() {
            return Err(Error::hw_reset(
                "the GPU was reset moments ago; new hardware sessions wait out the grace period",
            ));
        }
        let caps = CapReport::probe(display)?;
        let entry = caps.entry(profile, Entrypoint::VLD).ok_or_else(|| {
            Error::unsupported(
                format!("{profile:?} decode"),
                "the driver advertises no VLD entrypoint for this profile",
            )
        })?;

        let rt_format = rt_format_for_bit_depth(bit_depth);
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
        .with_usage_hint(va::VA_SURFACE_ATTRIB_USAGE_HINT_DECODER);
        let surfaces = ec_va::create_pool(display, &spec, num_surfaces)?;
        let context =
            ec_va::picture::create_context(display, &config, w, h, Some(&surfaces), true)?;
        let pool = SurfacePool::from_surfaces(surfaces, &spec);

        if crate::reset::gpu_reset_epoch() != born_epoch {
            return Err(Error::hw_reset(
                "the GPU was reset while the session was created",
            ));
        }
        Ok(Session {
            born_epoch,
            pending: std::sync::Mutex::new(None),
            poisoned: std::sync::atomic::AtomicBool::new(false),
            context,
            pool,
            coded_size,
            display_size,
            bit_depth,
            profile,
        })
    }

    /// Submit one picture: begin, render every buffer, end, then wait for the
    /// *previous* picture (depth-1 pipelining).
    ///
    /// A corrupt slice can hang the ring; the kernel resets it after its job
    /// timeout, and every context that submits afterwards is aborted by the
    /// driver. So a wait that errors or runs long condemns the whole process's
    /// sessions ([`crate::reset`]) and nothing submits to a reset context.
    ///
    /// Waiting for picture N-1 after submitting N keeps the GPU one picture
    /// ahead of the CPU. It never submits behind an undetected reset: the hung
    /// picture is waited for right after the next submit (the ring cannot have
    /// been reset yet, the job timeout is seconds), and a pending picture
    /// older than [`IDLE_SETTLE`] is settled before anything is submitted.
    /// Frames read back also watch their wait ([`crate::reset::sync_watched`]).
    pub(crate) fn submit(&self, target: &Rc<Surface>, buffers: Vec<ec_va::Buffer>) -> Result<()> {
        if buffers.is_empty() {
            // radeonsi answers an empty submission with INVALID_CONTEXT, which
            // reads like a driver bug report rather than what it is: a picture
            // with no slices, i.e. a stream that lost its data.
            return Err(Error::config(
                "refusing to submit a picture with no parameter buffers",
            ));
        }
        self.check_alive()?;
        // An old pending picture is waited for first: its wait is instant when
        // it is done, and an error condemns the session before we submit.
        let stale = self
            .pending_slot()
            .as_ref()
            .filter(|p| p.since.elapsed() > IDLE_SETTLE)
            .map(|p| Rc::clone(&p.surface));
        if let Some(old) = stale {
            self.settle(&old)?;
            self.check_alive()?;
        }
        let outcome = ec_va::picture::submit(&self.context, Rc::clone(target), buffers);
        if let Err(e) = outcome {
            match e.status() {
                Some(status)
                    if status_is(
                        status,
                        &[
                            va::VA_STATUS_ERROR_OPERATION_FAILED,
                            va::VA_STATUS_ERROR_DECODING_ERROR,
                        ],
                    ) =>
                {
                    self.poison()
                }
                Some(status) if context_gone(status) => crate::reset::report_gpu_reset(),
                _ => {}
            }
            return Err(e.into());
        }
        let previous = self.pending_slot().replace(Pending {
            surface: Rc::clone(target),
            since: Instant::now(),
        });
        if let Some(previous) = previous {
            self.settle(&previous.surface)?;
        }
        Ok(())
    }

    fn pending_slot(&self) -> std::sync::MutexGuard<'_, Option<Pending>> {
        match self.pending.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn check_alive(&self) -> Result<()> {
        if crate::reset::gpu_reset_epoch() != self.born_epoch {
            return Err(Error::hw_reset(
                "the GPU was reset after this session was created; its context is lost",
            ));
        }
        if self.poisoned.load(Ordering::Acquire) {
            return Err(Error::poisoned("an earlier picture failed on the GPU"));
        }
        Ok(())
    }

    fn poison(&self) {
        self.poisoned.store(true, Ordering::Release);
    }

    /// Wait for a submitted picture, condemning every session on a reset.
    fn settle(&self, surface: &Surface) -> Result<()> {
        crate::reset::sync_watched(surface)
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

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: a decode context was always created 8-bit 4:2:0, so no HEVC
    // Main 10 stream could decode into its P010 surfaces (cros-codecs patch
    // in edith vendor/, backend/vaapi/decoder.rs, "upstream's own TODO").
    #[test]
    fn decode_context_rt_format_follows_the_bit_depth() {
        assert_eq!(rt_format_for_bit_depth(8), va::VA_RT_FORMAT_YUV420);
        assert_eq!(rt_format_for_bit_depth(10), va::VA_RT_FORMAT_YUV420_10);
        assert_ne!(rt_format_for_bit_depth(10), rt_format_for_bit_depth(8));
    }
}
