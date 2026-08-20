//! Hardware encoding: H.264, HEVC, and AV1 behind an explicit opt-in.
//!
//! One frame in, one coded frame out. The GOP is IDR-then-P with a single
//! reference, which is what this GPU advertises (`VAConfigAttribEncMaxRefFrames`
//! reports one list-0 reference for HEVC) and what an editor's export wants:
//! no reordering, so a coded frame comes back in the call that fed it.
//!
//! # AV1
//!
//! AV1 encoding is off unless [`EncoderConfig::allow_av1`] is set, and never
//! reachable from an "auto" codec choice. The reason is on the record rather
//! than theoretical: an AV1 encode submission on this driver generation took
//! the GPU down hard enough to need a reset. The path is built, typed and
//! probed; turning it on is a caller's explicit decision.

use std::sync::Arc;

use ec_va::caps::{Entrypoint, Profile};
use ec_va::{
    Buffer, CapReport, Config, ConfigAttrib, Context, Display, Image, MappedBuffer, Picture,
    Surface, SurfaceSpec, Vpp, sys,
};

use crate::error::{Error, Result};
use crate::frame::I420;
use crate::params::enc::{
    MISC_FRAME_RATE, MISC_HRD, MISC_RATE_CONTROL, PackedHeaderParameterBuffer, RateControl,
    misc_bytes,
};
use crate::params::param_buffer;
use crate::pool::{PooledSurface, SurfacePool};

mod av1;
mod h264;
mod headers;
mod hevc;

/// The codecs this crate encodes in hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncCodec {
    /// H.264 / AVC, High profile.
    H264,
    /// H.265 / HEVC, Main profile.
    H265,
    /// AV1 Profile 0 — opt-in only, see the module docs.
    Av1,
}

/// How the driver should spend bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControlMode {
    /// Constant quantiser: `qp` for every picture, bitrate ignored.
    ConstantQp {
        /// The quantiser, 0..51 for H.264/HEVC, 0..255 for AV1.
        qp: u32,
    },
    /// Constant bitrate, the driver picking quantisers to hold it.
    ConstantBitrate,
}

/// What can be changed between frames without rebuilding the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tunings {
    /// Target bitrate in bits per second.
    pub bitrate: u32,
    /// Frame rate as `(numerator, denominator)`.
    pub framerate: (u32, u32),
}

/// Per-frame instructions from the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameMetadata {
    /// Presentation timestamp, carried through to the coded frame.
    pub timestamp: i64,
    /// Start a new GOP with this frame.
    pub force_keyframe: bool,
}

/// How an encoder is set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    /// Which codec.
    pub codec: EncCodec,
    /// Displayed width; the coded size is rounded up for the codec's blocks.
    pub width: u32,
    /// Displayed height.
    pub height: u32,
    /// Frame rate as `(numerator, denominator)`.
    pub framerate: (u32, u32),
    /// Target bitrate in bits per second.
    pub bitrate: u32,
    /// Pictures per GOP; the first of each is an IDR / key frame.
    pub gop_size: u32,
    /// Rate control mode.
    pub rate_control: RateControlMode,
    /// Driver quality/speed level, 1 (best quality) upwards; 0 = driver default.
    pub quality: u32,
    /// Permit AV1 encoding. Without it, [`EncCodec::Av1`] is refused.
    pub allow_av1: bool,
    /// The colour description to write into the VUI, or `None` to leave it
    /// unsignalled (`video_signal_type_present_flag = 0`) as before.
    pub colour: Option<Colour>,
}

/// H.265 E.2.1 / H.264 E.1.1 colour description code points for the VUI.
///
/// Carried verbatim into `colour_primaries`, `transfer_characteristics` and
/// `matrix_coeffs` — this crate does not interpret them, so a caller wanting
/// BT.2020/PQ passes `(9, 16, 9)`, BT.709 SDR `(1, 1, 1)`, and so on (H.273
/// tables 2/3/4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colour {
    /// `colour_primaries`.
    pub primaries: u8,
    /// `transfer_characteristics`.
    pub transfer: u8,
    /// `matrix_coeffs`.
    pub matrix: u8,
    /// `video_full_range_flag`.
    pub full_range: bool,
}

impl EncoderConfig {
    /// A configuration with sensible defaults for `codec` at `width x height`.
    pub fn new(codec: EncCodec, width: u32, height: u32) -> EncoderConfig {
        EncoderConfig {
            codec,
            width,
            height,
            framerate: (30, 1),
            bitrate: 8_000_000,
            gop_size: 60,
            rate_control: RateControlMode::ConstantBitrate,
            quality: 0,
            allow_av1: false,
            colour: None,
        }
    }

    /// Set the VUI colour description; H.265 E.2.1 / H.264 E.1.1 code points.
    pub fn colour(
        mut self,
        primaries: u8,
        transfer: u8,
        matrix: u8,
        full_range: bool,
    ) -> EncoderConfig {
        self.colour = Some(Colour {
            primaries,
            transfer,
            matrix,
            full_range,
        });
        self
    }
}

/// One coded picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodedFrame {
    /// The bitstream: an Annex B access unit for H.264/HEVC, an OBU sequence
    /// for AV1.
    pub data: Vec<u8>,
    /// The timestamp the caller supplied with the source frame.
    pub timestamp: i64,
    /// True for an IDR / key frame.
    pub is_keyframe: bool,
}

/// A hardware encoder.
pub struct Encoder {
    config: EncoderConfig,
    context: Arc<Context>,
    pool: SurfacePool,
    coded_size: (u32, u32),
    /// The reconstruction of the previous picture, the single reference.
    reference: Option<Arc<PooledSurface>>,
    /// Pictures coded since the last key frame.
    gop_position: u32,
    /// Pictures coded in total, which drives frame_num and order hints.
    coded: u64,
    tunings: Tunings,
    /// Set when the rate control parameters have to be resent.
    tunings_dirty: bool,
    profile: Profile,
    /// An upload image, reused across frames.
    upload: Option<Image>,
    /// P010 -> NV12 GPU-resident conversion pipeline for [`Encoder::encode_frame`]
    /// on a 10-bit source; built lazily, since most encoders never see one.
    vpp: Option<Vpp>,
    /// The small pool [`Encoder::vpp`] converts into, round-robined rather
    /// than pool-acquired: each conversion is fully synced before this
    /// encoder's own picture submission returns, so two surfaces are enough
    /// to never hand out one still in flight.
    vpp_surfaces: Vec<Arc<Surface>>,
    vpp_next: usize,
}

impl Encoder {
    /// Build an encoder for `config` on `display`.
    pub fn new(display: &Arc<Display>, config: EncoderConfig) -> Result<Encoder> {
        if config.codec == EncCodec::Av1 && !config.allow_av1 {
            return Err(Error::unsupported(
                "AV1 hardware encoding",
                "it is opt-in (EncoderConfig::allow_av1) after a GPU recovery incident",
            ));
        }
        let profile = match config.codec {
            // High is the profile every H.264 encoder on this driver reports and
            // the one a High-profile decoder expects; Main is a strict subset.
            EncCodec::H264 => Profile::H264High,
            EncCodec::H265 => Profile::HEVCMain,
            EncCodec::Av1 => Profile::AV1Profile0,
        };
        let block = match config.codec {
            EncCodec::H264 => 16,
            EncCodec::H265 => 32,
            EncCodec::Av1 => 64,
        };
        let coded_size = (
            config.width.next_multiple_of(block),
            config.height.next_multiple_of(block),
        );

        let caps = CapReport::probe(display)?;
        let entry = caps.entry(profile, Entrypoint::EncSlice).ok_or_else(|| {
            Error::unsupported(
                format!("{profile:?} encode"),
                "the driver advertises no EncSlice entrypoint for this profile",
            )
        })?;
        if let Some(surfaces) = &entry.surfaces
            && !surfaces.allows(coded_size.0, coded_size.1)
        {
            // The minimum encode size is a driver fact, not a guess: radeonsi
            // refuses anything under 96x32 for H.264 and 384x128 for HEVC.
            return Err(Error::unsupported(
                format!("{}x{} {profile:?} encode", coded_size.0, coded_size.1),
                format!(
                    "the driver accepts {:?}..{:?} x {:?}..{:?}",
                    surfaces.min_width,
                    surfaces.max_width,
                    surfaces.min_height,
                    surfaces.max_height
                ),
            ));
        }

        // The packed-header attribute is not decoration: a config that does not
        // ask for them gets a driver that writes its own headers and silently
        // ignores the application's (measured on radeonsi — the parameter sets
        // went in, the slice header did not, and every P picture came back
        // missing its `byte_alignment()`).
        let packed = entry.packed_headers
            & (sys::VA_ENC_PACKED_HEADER_SEQUENCE
                | sys::VA_ENC_PACKED_HEADER_PICTURE
                | sys::VA_ENC_PACKED_HEADER_SLICE);
        let config_id = Config::new(
            display,
            profile,
            Entrypoint::EncSlice,
            &[
                ConfigAttrib::rt_format(sys::VA_RT_FORMAT_YUV420),
                ConfigAttrib {
                    type_: sys::VAConfigAttribEncPackedHeaders,
                    value: packed,
                },
            ],
        )?;
        let spec = SurfaceSpec::nv12(coded_size.0, coded_size.1)
            .with_usage_hint(sys::VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER);
        // Input, reconstruction, one reference, and slack for the driver to
        // keep a submission in flight.
        let pool = SurfacePool::new(display, &spec, 8)?;
        let context = Context::new(
            &config_id,
            coded_size.0,
            coded_size.1,
            sys::VA_PROGRESSIVE,
            pool.targets(),
        )?;

        Ok(Encoder {
            tunings: Tunings {
                bitrate: config.bitrate,
                framerate: config.framerate,
            },
            config,
            context,
            pool,
            coded_size,
            reference: None,
            gop_position: 0,
            coded: 0,
            tunings_dirty: true,
            profile,
            upload: None,
            vpp: None,
            vpp_surfaces: Vec::new(),
            vpp_next: 0,
        })
    }

    /// The coded (block-aligned) size the driver works at.
    pub fn coded_size(&self) -> (u32, u32) {
        self.coded_size
    }

    /// Change bitrate or frame rate; takes effect on the next frame.
    pub fn tune(&mut self, tunings: Tunings) {
        self.tunings = tunings;
        self.tunings_dirty = true;
    }

    /// Encode one frame and return its coded bytes.
    pub fn encode(&mut self, frame: &I420, meta: FrameMetadata) -> Result<CodedFrame> {
        if frame.width != self.config.width || frame.height != self.config.height {
            return Err(Error::config(format!(
                "encoder configured for {}x{} was given a {}x{} frame",
                self.config.width, self.config.height, frame.width, frame.height
            )));
        }
        let input = self
            .pool
            .acquire()
            .ok_or_else(|| Error::config("encode: every surface is still in use"))?;
        self.upload(frame, &input)?;
        self.encode_surface(Arc::clone(input.surface()), meta)
    }

    /// Encode a decoded frame's GPU surface directly: no `to_i420` read-back,
    /// no re-upload, just the decoder's picture handed straight to the
    /// encoder as its source. This is the path a decode-then-encode pipeline
    /// (e.g. a transcode) wants — the alternative is a full frame through
    /// system memory and back for no reason.
    ///
    /// A 10-bit (P010) source is also zero-copy through the driver's own
    /// video-processing pipeline: [`Encoder`] keeps a small VPP-owned NV12
    /// surface pool (built lazily, on the first 10-bit frame) and converts
    /// into it on the GPU (`VAProfileNone`/`VAEntrypointVideoProc`) before
    /// submitting exactly as for an 8-bit source — still no read-back to
    /// system memory. Every profile this crate encodes (`H264High`,
    /// `HEVCMain`) is itself 8-bit, so the low two bits are truncated the
    /// same way [`Frame::to_i420`]'s CPU path truncates them (VPP's rounding
    /// may differ by a code point; see `gpu.rs`'s PSNR comparison of the two
    /// paths for the measured difference) — call [`Frame::to_i420_16`]
    /// instead if the extra precision must survive encoding.
    ///
    /// Refused, rather than silently working around, when:
    /// - `frame` was decoded on a different [`ec_va::Display`]: a surface id
    ///   only means anything on the display that created it.
    /// - `frame`'s bit depth is neither 8 nor 10: no profile this crate
    ///   encodes, and no VPP conversion this crate builds, has a destination
    ///   for a 12-bit source.
    ///
    /// A `frame` whose coded size does not match this encoder's (a common
    /// case: HEVC block-rounds to 32, so a 3840x2160 source's 2160 stays
    /// unrounded while the encoder's coded height becomes 2176) is not
    /// refused — it goes through the same VPP pool a 10-bit source uses,
    /// placed by explicit region rather than a whole-surface convert: only
    /// `frame.display_size` is read from the source and written into the
    /// destination at `(0, 0)`, so the padding rows/columns beyond it are
    /// never touched by the copy. Those padding pixels are still fed to the
    /// encoder as part of the coded surface, so the pool is painted black
    /// once, on creation, rather than left as whatever the driver's fresh
    /// allocation happened to contain.
    pub fn encode_frame(
        &mut self,
        frame: &crate::frame::Frame,
        meta: FrameMetadata,
    ) -> Result<CodedFrame> {
        if frame.bit_depth != 8 && frame.bit_depth != 10 {
            return Err(Error::unsupported(
                format!("encode_frame of a {}-bit source", frame.bit_depth),
                "H264High and HEVCMain, the only profiles this crate encodes, are 8-bit, and \
                 the VPP conversion this crate builds only reads P010 (10-bit); decode to I420 \
                 with Frame::to_i420 and call Encoder::encode instead",
            ));
        }
        if !Arc::ptr_eq(frame.surface().display(), self.context.display()) {
            return Err(Error::config(
                "encode_frame: the frame was decoded on a different Display than this encoder",
            ));
        }
        let source = if frame.bit_depth == 10 || frame.coded_size != self.coded_size {
            self.vpp_into_pool(frame.surface(), frame.display_size)?
        } else {
            Arc::clone(frame.surface())
        };
        self.encode_surface(source, meta)
    }

    /// Place a source surface's visible rect into this encoder's coded size,
    /// in NV12, on the GPU: a whole-surface convert when the source is
    /// already the encoder's coded size (the common 10-bit case), a region
    /// copy/convert otherwise. Builds the VPP pipeline and its two-surface
    /// pool on first use, painting the pool black so any padding beyond
    /// `display_size` a region copy never touches is deterministic.
    fn vpp_into_pool(
        &mut self,
        source: &Arc<Surface>,
        display_size: (u32, u32),
    ) -> Result<Arc<Surface>> {
        if self.vpp.is_none() {
            let spec = SurfaceSpec::nv12(self.coded_size.0, self.coded_size.1)
                .with_usage_hint(sys::VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER);
            let display = self.context.display();
            let surfaces = Surface::create_pool(display, &spec, 2)?;
            for s in &surfaces {
                Self::fill_black(s, self.coded_size)?;
            }
            let vpp = Vpp::new(
                display,
                sys::VA_RT_FORMAT_YUV420 | sys::VA_RT_FORMAT_YUV420_10,
                &surfaces,
            )?;
            self.vpp_surfaces = surfaces;
            self.vpp = Some(vpp);
        }
        let dest = Arc::clone(&self.vpp_surfaces[self.vpp_next]);
        self.vpp_next = (self.vpp_next + 1) % self.vpp_surfaces.len();
        let vpp = self.vpp.as_ref().expect("just built above if it was None");
        if display_size == self.coded_size {
            Ok(vpp.convert(source, dest)?)
        } else {
            let rect = (0, 0, display_size.0, display_size.1);
            Ok(vpp.convert_region(source, rect, dest, rect)?)
        }
    }

    /// Paint an NV12 surface black (Y=16, chroma=128, TV-range neutral): the
    /// one-time initialisation [`Encoder::vpp_into_pool`] gives its pool so a
    /// region copy's untouched padding reads the same deterministic value on
    /// every driver instead of whatever the fresh allocation contained.
    fn fill_black(surface: &Arc<Surface>, size: (u32, u32)) -> Result<()> {
        let mut image = Image::create(surface.display(), sys::VA_FOURCC_NV12, size.0, size.1)?;
        {
            let mut mapped = image.map()?;
            if let Some(y) = mapped.plane_mut(0) {
                y.fill(16);
            }
            if let Some(uv) = mapped.plane_mut(1) {
                uv.fill(128);
            }
        }
        surface.write_from(&image, size.0, size.1)?;
        Ok(())
    }

    /// Shared submission path for [`Encoder::encode`] and
    /// [`Encoder::encode_frame`]: everything after the source surface is
    /// settled.
    fn encode_surface(&mut self, source: Arc<Surface>, meta: FrameMetadata) -> Result<CodedFrame> {
        let keyframe = meta.force_keyframe
            || self.reference.is_none()
            || self.gop_position >= self.config.gop_size.max(1);

        let recon = self
            .pool
            .acquire()
            .ok_or_else(|| Error::config("encode: no surface for the reconstruction"))?;

        // 6 bits per pixel of headroom: an intra frame at a low quantiser is the
        // worst case, and a coded buffer that overflows loses the picture.
        let coded_size = (self.coded_size.0 * self.coded_size.1 * 3 / 4).max(1 << 20);
        let coded_buf = Buffer::allocate(&self.context, sys::VAEncCodedBufferType, coded_size)?;
        let coded_id = coded_buf.id();

        let mut buffers = Vec::new();
        match self.config.codec {
            EncCodec::H264 => h264::parameters(self, &recon, coded_id, keyframe, &mut buffers)?,
            EncCodec::H265 => hevc::parameters(self, &recon, coded_id, keyframe, &mut buffers)?,
            EncCodec::Av1 => av1::parameters(self, &recon, coded_id, keyframe, &mut buffers)?,
        }
        buffers.extend(self.rate_control_buffers()?);
        buffers.push(coded_buf);

        let mut picture = Picture::new(&self.context, source)
            .begin()?
            .render_all(buffers)?
            .end()?
            .sync()?;

        let data = {
            let coded = picture
                .buffers_mut()
                .iter_mut()
                .find(|b| b.buffer_type() == sys::VAEncCodedBufferType)
                .ok_or_else(|| Error::config("the coded buffer vanished from the picture"))?;
            let mapped = coded.map()?;
            coded_bytes(&mapped)?
        };

        self.reference = Some(recon);
        self.coded += 1;
        self.gop_position = if keyframe { 1 } else { self.gop_position + 1 };
        Ok(CodedFrame {
            data,
            timestamp: meta.timestamp,
            is_keyframe: keyframe,
        })
    }

    /// Upload one frame into a surface as NV12.
    ///
    /// One image is created per encoder and reused: `vaCreateImage` per frame
    /// would be an allocation per frame, which is exactly the shape of the
    /// per-frame cost this crate exists to remove.
    fn upload(&mut self, frame: &I420, surface: &Arc<Surface>) -> Result<()> {
        if self.upload.is_none() {
            self.upload = Some(Image::create(
                surface.display(),
                sys::VA_FOURCC_NV12,
                self.coded_size.0,
                self.coded_size.1,
            )?);
        }
        let image = self
            .upload
            .as_mut()
            .ok_or_else(|| Error::config("no upload image"))?;
        let (w, h) = (frame.width as usize, frame.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        {
            let mut mapped = image.map()?;
            let y_pitch = mapped.pitch(0).unwrap_or(0) as usize;
            let uv_pitch = mapped.pitch(1).unwrap_or(0) as usize;
            // The two planes are written in turn because a mapping hands out one
            // mutable plane at a time; the loops are the interleave `i420_to_nv12`
            // does for callers that own both buffers.
            if let Some(dst) = mapped.plane_mut(0) {
                for row in 0..h {
                    let (s, d) = (row * w, row * y_pitch);
                    if d + w > dst.len() {
                        break;
                    }
                    dst[d..d + w].copy_from_slice(&frame.y[s..s + w]);
                }
            }
            if let Some(dst) = mapped.plane_mut(1) {
                for row in 0..ch {
                    let d = row * uv_pitch;
                    if d + cw * 2 > dst.len() {
                        break;
                    }
                    for col in 0..cw {
                        dst[d + col * 2] = frame.u[row * cw + col];
                        dst[d + col * 2 + 1] = frame.v[row * cw + col];
                    }
                }
            }
        }
        surface.write_from(image, self.coded_size.0, self.coded_size.1)?;
        Ok(())
    }

    /// The misc parameter buffers that carry rate control, resent when tuned.
    fn rate_control_buffers(&mut self) -> Result<Vec<Buffer>> {
        if !self.tunings_dirty {
            return Ok(Vec::new());
        }
        self.tunings_dirty = false;
        let (initial_qp, min_qp, max_qp, bits) = match self.config.rate_control {
            RateControlMode::ConstantQp { qp } => (qp, qp, qp, 0),
            RateControlMode::ConstantBitrate => (0, 0, 0, self.tunings.bitrate),
        };
        let rc = RateControl {
            bits_per_second: bits,
            target_percentage: 100,
            window_size: 1000,
            initial_qp,
            min_qp,
            max_qp,
        };
        let mut out = vec![Buffer::from_bytes(
            &self.context,
            sys::VAEncMiscParameterBufferType,
            &misc_bytes(MISC_RATE_CONTROL, &rc.words()),
        )?];
        let fr = crate::params::enc::FrameRate {
            num: self.tunings.framerate.0.max(1),
            den: self.tunings.framerate.1.max(1),
        };
        out.push(Buffer::from_bytes(
            &self.context,
            sys::VAEncMiscParameterBufferType,
            &misc_bytes(MISC_FRAME_RATE, &fr.words()),
        )?);
        if bits > 0 {
            let hrd = crate::params::enc::Hrd {
                initial_buffer_fullness: bits,
                buffer_size: bits * 2,
            };
            out.push(Buffer::from_bytes(
                &self.context,
                sys::VAEncMiscParameterBufferType,
                &misc_bytes(MISC_HRD, &hrd.words()),
            )?);
        }
        Ok(out)
    }

    /// Submit one packed header: its parameter buffer, then its bytes.
    ///
    /// Both go in the same picture, in this order, which is what the driver
    /// reads them in (`va.h:2446`).
    pub(crate) fn push_packed(
        &self,
        packed: &headers::Packed,
        out: &mut Vec<Buffer>,
    ) -> Result<()> {
        let param = PackedHeaderParameterBuffer {
            type_: packed.kind,
            bit_length: packed.bits,
            has_emulation_bytes: 1,
            ..PackedHeaderParameterBuffer::default()
        };
        out.push(param_buffer(&self.context, &param)?);
        out.push(Buffer::from_bytes(
            &self.context,
            sys::VAEncPackedHeaderDataBufferType,
            &packed.bytes,
        )?);
        Ok(())
    }

    pub(crate) fn context(&self) -> &Arc<Context> {
        &self.context
    }

    pub(crate) fn config(&self) -> &EncoderConfig {
        &self.config
    }

    pub(crate) fn reference(&self) -> Option<&Arc<PooledSurface>> {
        self.reference.as_ref()
    }

    pub(crate) fn coded_count(&self) -> u64 {
        self.coded
    }

    pub(crate) fn gop_position(&self) -> u32 {
        self.gop_position
    }

    /// The VA profile this encoder was configured with.
    pub fn profile(&self) -> Profile {
        self.profile
    }
}

/// Collect the bytes of a coded buffer's segment list.
///
/// `VACodedBufferSegment` is a linked list in driver memory: `size`, `bit_offset`,
/// `status`, a `buf` pointer to that segment's bytes and a `next` pointer. There
/// is no safe way to express that in `ec-va`, so this is one of the crate's two
/// unsafe functions.
fn coded_bytes(mapped: &MappedBuffer<'_>) -> Result<Vec<u8>> {
    // Field offsets from `crates/ec-hw/abi-probe.c`: size=0 bit_offset=4
    // status=8 reserved=12 buf=16 next=24, total 48 bytes, align 8.
    const BUF_OFFSET: usize = 16;
    const NEXT_OFFSET: usize = 24;
    let mut out = Vec::new();
    // SAFETY: the mapping is a VACodedBufferSegment as libva defines it
    // (va.h:3940); the pointer is valid for the guard's lifetime.
    let mut segment = unsafe { mapped.as_ptr() };
    // A driver returning a cyclic list would otherwise hang the export; 4096
    // segments is far beyond any real picture.
    for _ in 0..4096 {
        if segment.is_null() {
            break;
        }
        // SAFETY: `segment` points at a 48-byte segment header the driver
        // wrote; every field is read at its probed offset with an unaligned
        // read, and `buf` points at `size` bytes of coded data that stay valid
        // until the buffer is unmapped (i.e. for this guard's lifetime).
        let (size, data, next) = unsafe {
            let size = segment.cast::<u32>().read_unaligned() as usize;
            let data = segment.add(BUF_OFFSET).cast::<*const u8>().read_unaligned();
            let next = segment.add(NEXT_OFFSET).cast::<*mut u8>().read_unaligned();
            (size, data, next)
        };
        if data.is_null() || size == 0 {
            break;
        }
        // SAFETY: as above — `data` is valid for `size` bytes.
        out.extend_from_slice(unsafe { std::slice::from_raw_parts(data, size) });
        segment = next;
    }
    if out.is_empty() {
        return Err(Error::config("the encoder produced an empty coded buffer"));
    }
    Ok(out)
}
