//! Hardware encoding: H.264 (ported to cros-libva), HEVC and AV1 (refused
//! with [`Error::Unsupported`] until their parameter buffers are ported).
//!
//! The bitstream writers in [`headers`] are pure. The parameter buffers are
//! built from cros-libva's typed constructors in each codec module.
//!
//! One frame in, one coded frame out. The GOP is IDR-then-P with a single
//! reference, which is what this GPU advertises (`VAConfigAttribEncMaxRefFrames`
//! reports one list-0 reference for HEVC) and what an editor's export wants:
//! no reordering, so a coded frame comes back in the call that fed it.
//!
//! # AV1
//!
//! AV1 encode was opt-in until 2026-09-07, after a submission on an older
//! driver took the GPU down. It is on by default now because the refusal was
//! retired by measurement, not by opinion: on mesa 26.1.8 (gfx1200) ffmpeg's
//! own `av1_vaapi` encodes, this path encodes, and `gpu.rs`'s
//! `av1_encode_agrees_between_two_decoders` proves every coded frame decodes
//! identically through `ec-av1` and libdav1d. Unlike H.264 and HEVC, the
//! driver composes no headers of its own from thin air: it parses the
//! sequence and frame header OBUs this crate packs (see `enc::headers::av1`)
//! and writes the stream's headers from them.
//!
//! AV1 encodes at 8 or 10 bits ([`EncoderConfig::bit_depth`]); 10-bit is the
//! depth his own library is stored at, and `gpu.rs`'s
//! `av1_10bit_encode_agrees_between_two_decoders` proves a 10-bit stream out
//! of this path decodes identically through `ec-av1` and libdav1d with
//! ffprobe reading `yuv420p10le` out of the sequence header.
//!
//! # Rate control
//!
//! [`RateControlMode`] is not advisory: it is set as
//! `VAConfigAttribRateControl` when the VA config is created, so an encoder
//! built for CQP ignores a bitrate handed to it later. CQP, CBR, VBR and QVBR
//! are all measured by `gpu.rs`'s `av1_encode_rate_control_lands`, which
//! prints what each mode delivered against what it was asked for.

#![allow(dead_code)] // the header writers wait for the encoder port

use std::rc::Rc;

use ec_va::caps::{Entrypoint, Profile};
use ec_va::va::{
    BufferType, EncMiscParameter, EncMiscParameterFrameRate, EncMiscParameterHRD,
    EncMiscParameterRateControl, EncPackedHeaderParameter, EncPackedHeaderType, RcFlags,
    VAConfigAttribType, VAImage, VA_ENC_PACKED_HEADER_PICTURE, VA_ENC_PACKED_HEADER_SEQUENCE,
    VA_ENC_PACKED_HEADER_SLICE, VA_FOURCC_NV12, VA_FOURCC_P010, VA_RT_FORMAT_YUV420,
    VA_RT_FORMAT_YUV420_10, VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER,
};
use ec_va::va::SurfaceMemoryDescriptor;
use ec_va::{CapReport, Config, ConfigAttrib, Context, Display, Surface, SurfaceSpec, Vpp};

use crate::error::{Error, Result};
use crate::frame::{Frame, I420, I420_16};
use crate::pool::{PooledSurface, SurfacePool};

mod av1;
mod h264;
mod headers;
mod hevc;

/// `VA_RC_QVBR`, `va.h:1652`. radeonsi advertises it in
/// `VAConfigAttribRateControl` (probed 0x416 = CQP|CBR|VBR|QVBR on mesa 26.1.8).
const VA_RC_QVBR: u32 = 0x0000_0400;

/// The codecs this crate encodes in hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncCodec {
    /// H.264 / AVC; the profile is [`EncoderConfig::h264_profile`].
    H264,
    /// H.265 / HEVC, Main profile.
    H265,
    /// AV1 Profile 0.
    Av1,
}

/// The H.264 profile an [`Encoder`] writes.
///
/// Each profile is a strict subset of the next in what a decoder must accept:
/// [`H264Profile::ConstrainedBaseline`] is CAVLC with no B slices, [`H264Profile::Main`]
/// adds CABAC, and [`H264Profile::High`] adds the High-only SPS tail
/// (chroma format, bit depth, scaling lists). This encoder codes no B slices and no
/// 8x8 transform, so the three differ only in what the SPS and PPS say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum H264Profile {
    /// `profile_idc` 66 with `constraint_set1_flag`; CAVLC entropy coding.
    ConstrainedBaseline,
    /// `profile_idc` 77; CABAC entropy coding, no High-only syntax.
    Main,
    /// `profile_idc` 100; CABAC, with the High SPS tail. The default, and what
    /// this encoder wrote before the profile was selectable.
    #[default]
    High,
}

impl H264Profile {
    /// The VA profile the driver is asked for.
    fn va_profile(self) -> Profile {
        match self {
            H264Profile::ConstrainedBaseline => Profile::H264ConstrainedBaseline,
            H264Profile::Main => Profile::H264Main,
            H264Profile::High => Profile::H264High,
        }
    }

    /// True when the slice data is CABAC (`entropy_coding_mode_flag` = 1).
    pub(crate) fn cabac(self) -> bool {
        !matches!(self, H264Profile::ConstrainedBaseline)
    }
}

/// The rate window the request is spread over, in milliseconds (`window_size`).
const RATE_WINDOW_MS: u32 = 1000;

/// The HRD buffer for a GOP of `gop_size` pictures, in milliseconds of the
/// requested bitrate.
///
/// The buffer is what radeonsi's constant-bitrate loop works against, and it
/// must be short when pictures are mostly key frames. Measured at 720p,
/// testsrc2, 240 pictures, HEVC, requested 1, 3 and 8 Mb/s:
///
/// | GOP | buffer | delivered (3 Mb/s request) |
/// |---|---|---|
/// | 1 | 1000 ms | 7.63 Mb/s (+154%) |
/// | 1 | 33 ms | 3.29 Mb/s (+9.5%) |
/// | 1 | 20 ms | 3.02 Mb/s (+0.7%) |
/// | 2 | 1000 ms | +9.4% |
/// | 5 | 1000 ms | +4.3% |
/// | 10 | 1000 ms | +1.8% |
/// | 30 | 1000 ms | +1.0%, 43.5 dB |
/// | 30 | 20 ms | +0.0%, 40.1 dB |
///
/// So a GOP shorter than ten pictures gets a 20 ms buffer. A GOP of thirty keeps
/// the full rate window: the short buffer lands on the request there too, but
/// costs 3.4 dB at the same rate.
fn hrd_buffer_ms(gop_size: u32) -> u32 {
    if gop_size < 10 { 20 } else { RATE_WINDOW_MS }
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
    /// Variable bitrate: `bitrate` is the average asked for, and the driver
    /// may spend up to 1.5x it inside the rate window.
    VariableBitrate,
    /// Quality-defined variable bitrate: the driver holds `quality` (a
    /// quantiser-like target, lower is better quality) and treats `bitrate`
    /// as the ceiling it may not exceed.
    Qvbr {
        /// The quality factor, in the codec's quantiser scale.
        quality: u32,
    },
}

impl RateControlMode {
    /// The `VAConfigAttribRateControl` value this mode is configured with.
    ///
    /// The attribute is set at config creation, not per picture: a driver
    /// that was configured for CQP ignores a bitrate handed to it later
    /// (which is how the AV1 path measured CQP-only until 2026-09-07).
    fn va_mode(self) -> u32 {
        match self {
            RateControlMode::ConstantQp { .. } => ec_va::va::VA_RC_CQP,
            RateControlMode::ConstantBitrate => ec_va::va::VA_RC_CBR,
            RateControlMode::VariableBitrate => ec_va::va::VA_RC_VBR,
            RateControlMode::Qvbr { .. } => VA_RC_QVBR,
        }
    }

    /// True when the driver is told exactly which quantiser to use, so it has
    /// nothing to back-annotate into the header this crate packs.
    pub(crate) fn is_cqp(self) -> bool {
        matches!(self, RateControlMode::ConstantQp { .. })
    }
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
    /// The H.264 profile to write (ignored by HEVC and AV1).
    pub h264_profile: H264Profile,
    /// Rate control mode.
    pub rate_control: RateControlMode,
    /// Driver quality/speed level, 1 (best quality) upwards; 0 = driver default.
    pub quality: u32,
    /// The colour description to write into the VUI, or `None` to leave it
    /// unsignalled (`video_signal_type_present_flag = 0`) as before.
    pub colour: Option<Colour>,
    /// Luma bit depth: 8, or 10 for AV1 (his own library is `yuv420p10le`).
    /// A 10-bit encoder takes its pixels through [`Encoder::encode_16`] or
    /// straight from a 10-bit [`crate::Frame`].
    pub bit_depth: u8,
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
            h264_profile: H264Profile::High,
            rate_control: RateControlMode::ConstantBitrate,
            quality: 0,
            colour: None,
            bit_depth: 8,
        }
    }

    /// Encode at 10 bits (AV1 only); see [`EncoderConfig::bit_depth`].
    pub fn ten_bit(mut self) -> EncoderConfig {
        self.bit_depth = 10;
        self
    }

    /// Choose the H.264 profile written into the SPS and PPS.
    pub fn h264_profile(mut self, profile: H264Profile) -> EncoderConfig {
        self.h264_profile = profile;
        self
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
/// A hardware encoder.
pub struct Encoder {
    /// The GPU reset epoch this encoder was created in.
    born_epoch: u64,
    config: EncoderConfig,
    context: Rc<Context>,
    pool: SurfacePool,
    coded_size: (u32, u32),
    /// The reconstruction of the previous picture, the single reference.
    reference: Option<Rc<PooledSurface>>,
    /// Pictures coded since the last key frame.
    gop_position: u32,
    /// Key frames coded so far. Consecutive IDR pictures must differ in
    /// `idr_pic_id` (7.4.3), so this alternates the id even at GOP 1.
    idr_count: u32,
    tunings: Tunings,
    /// Set when the rate control parameters have to be resent.
    tunings_dirty: bool,
    profile: Profile,
    /// P010 -> NV12 GPU-resident conversion pipeline for [`Encoder::encode_frame`]
    /// on a source that is not the encoder's own format or size; built lazily,
    /// since most encoders never see one.
    vpp: Option<Vpp>,
    /// The small pool [`Encoder::vpp`] converts into, round-robined rather
    /// than pool-acquired: each conversion is fully synced before this
    /// encoder's own picture submission returns, so two surfaces are enough
    /// to never hand out one still in flight.
    vpp_surfaces: Vec<Rc<Surface>>,
    vpp_next: usize,
}

impl Encoder {
    /// Build an encoder for `config` on `display`.
    ///
    /// H.264 and HEVC are ported to cros-libva; AV1 is refused with
    /// [`Error::Unsupported`].
    pub fn new(display: &Rc<Display>, config: EncoderConfig) -> Result<Encoder> {
        let born_epoch = crate::reset::gpu_reset_epoch();
        if crate::reset::gpu_recently_reset() {
            return Err(Error::hw_reset(
                "the GPU was reset moments ago; new hardware sessions wait out the grace period",
            ));
        }
        if config.bit_depth != 8 && config.bit_depth != 10 {
            return Err(Error::config(format!(
                "encoder bit depth {} : this crate encodes 8-bit, or 10-bit AV1",
                config.bit_depth
            )));
        }
        if config.bit_depth == 10 && config.codec != EncCodec::Av1 {
            // Not a driver refusal: H264High and HEVCMain are 8-bit profiles by
            // definition, and this crate configures no other.
            return Err(Error::config(
                "10-bit encode is AV1 only here: the H.264 and H.265 profiles this crate \
                 configures are 8-bit",
            ));
        }
        let (profile, block) = match config.codec {
            EncCodec::H264 => (config.h264_profile.va_profile(), 16),
            EncCodec::H265 => (Profile::HEVCMain, 32),
            EncCodec::Av1 => (Profile::AV1Profile0, 64),
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
            // refuses anything under 96x32 for H.264.
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
            & (VA_ENC_PACKED_HEADER_SEQUENCE
                | VA_ENC_PACKED_HEADER_PICTURE
                | VA_ENC_PACKED_HEADER_SLICE);
        let config_id = Config::new(
            display,
            profile,
            Entrypoint::EncSlice,
            &[
                ConfigAttrib::rt_format(if config.bit_depth == 10 {
                    VA_RT_FORMAT_YUV420_10
                } else {
                    VA_RT_FORMAT_YUV420
                }),
                ConfigAttrib {
                    type_: VAConfigAttribType::VAConfigAttribRateControl,
                    value: config.rate_control.va_mode(),
                },
                ConfigAttrib {
                    type_: VAConfigAttribType::VAConfigAttribEncPackedHeaders,
                    value: packed,
                },
            ],
        )?;
        let spec = surface_spec(config.bit_depth, coded_size);
        // Input, reconstruction, one reference, and slack for the driver to
        // keep a submission in flight.
        let surfaces = ec_va::create_pool(display, &spec, 8)?;
        let context = ec_va::picture::create_context(
            display,
            &config_id,
            coded_size.0,
            coded_size.1,
            Some(&surfaces),
            true,
        )?;
        let pool = SurfacePool::from_surfaces(surfaces, &spec);

        Ok(Encoder {
            born_epoch,
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
            idr_count: 0,
            tunings_dirty: true,
            profile,
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
        if self.config.bit_depth != 8 {
            return Err(Error::config(format!(
                "encode: this encoder is configured for {} bits; call encode_16",
                self.config.bit_depth
            )));
        }
        self.check_size(frame.width, frame.height)?;
        let input = self
            .pool
            .acquire()
            .ok_or_else(|| Error::config("encode: every surface is still in use"))?;
        upload(&self.context, frame, input.surface(), self.coded_size)?;
        self.encode_settled(Rc::clone(input.surface()), meta)
    }

    /// Encode one 10-bit frame; the [`Encoder::encode`] of a 10-bit encoder.
    ///
    /// Samples are 0..1023 in the low ten bits of each `u16`, shifted into
    /// P010's top ten bits on upload. Only a 10-bit (AV1) encoder takes this path.
    pub fn encode_16(&mut self, frame: &I420_16, meta: FrameMetadata) -> Result<CodedFrame> {
        if self.config.bit_depth != 10 {
            return Err(Error::config(format!(
                "encode_16: this encoder is configured for {} bits; call encode",
                self.config.bit_depth
            )));
        }
        self.check_size(frame.width, frame.height)?;
        let input = self
            .pool
            .acquire()
            .ok_or_else(|| Error::config("encode: every surface is still in use"))?;
        upload_16(&self.context, frame, input.surface(), self.coded_size)?;
        self.encode_settled(Rc::clone(input.surface()), meta)
    }

    /// Encode a decoded frame's GPU surface directly: no `to_i420` read-back,
    /// no re-upload, just the decoder's picture handed straight to the
    /// encoder as its source. This is the path a decode-then-encode pipeline
    /// (e.g. a transcode) wants — the alternative is a full frame through
    /// system memory and back for no reason.
    ///
    /// A source whose size differs from the encoder's coded size, or whose
    /// depth differs from the encoder's, goes through the GPU video-processing
    /// pipeline into an NV12 surface of the encoder's size first.
    ///
    /// Refused, rather than silently working around, when:
    /// - `frame` was decoded on a different [`ec_va::Display`]: a surface id
    ///   only means anything on the display that created it.
    /// - `frame`'s bit depth is neither 8 nor 10.
    pub fn encode_frame(&mut self, frame: &Frame, meta: FrameMetadata) -> Result<CodedFrame> {
        if frame.bit_depth != 8 && frame.bit_depth != 10 {
            return Err(Error::unsupported(
                format!("encode_frame of a {}-bit source", frame.bit_depth),
                "the VPP conversion this crate builds reads 8-bit NV12 and 10-bit P010 only",
            ));
        }
        if !Rc::ptr_eq(frame.display(), self.context.display()) {
            return Err(Error::config(
                "encode_frame: the frame was decoded on a different Display than this encoder",
            ));
        }
        let source =
            if frame.bit_depth != self.config.bit_depth || frame.coded_size != self.coded_size {
                self.vpp_into_pool(frame.surface(), frame.display_size)?
            } else {
                Rc::clone(frame.surface())
            };
        self.encode_settled(source, meta)
    }

    /// Encode a surface imported with [`ec_va::Surface::import_prime`] or
    /// decoded elsewhere on this encoder's display: read by the GPU in place.
    ///
    /// The surface must be NV12, since H.264 is 8-bit only. A surface of another
    /// size goes through the same GPU conversion as [`Encoder::encode_frame`];
    /// `display_size` is the picture's visible size (its cropped size, when the
    /// surface is padded to the codec's block size).
    pub fn encode_surface<D: SurfaceMemoryDescriptor>(
        &mut self,
        surface: &Rc<ec_va::va::Surface<D>>,
        display_size: (u32, u32),
        meta: FrameMetadata,
    ) -> Result<CodedFrame> {
        if surface.size() != self.coded_size {
            let converted = self.vpp_into_pool(surface, display_size)?;
            self.encode_settled(converted, meta)
        } else {
            self.encode_settled(Rc::clone(surface), meta)
        }
    }

    fn check_size(&self, width: u32, height: u32) -> Result<()> {
        if width != self.config.width || height != self.config.height {
            return Err(Error::config(format!(
                "encoder configured for {}x{} was given a {}x{} frame",
                self.config.width, self.config.height, width, height
            )));
        }
        Ok(())
    }

    /// Place a source surface's visible rect into this encoder's coded size,
    /// in NV12, on the GPU: a whole-surface convert when the source is
    /// already the encoder's coded size, a region copy/convert otherwise.
    /// Builds the VPP pipeline and its two-surface pool on first use, painting
    /// the pool black so any padding beyond `display_size` a region copy never
    /// touches is deterministic.
    fn vpp_into_pool<D: SurfaceMemoryDescriptor>(
        &mut self,
        source: &ec_va::va::Surface<D>,
        display_size: (u32, u32),
    ) -> Result<Rc<Surface>> {
        if self.vpp.is_none() {
            let spec = surface_spec(self.config.bit_depth, self.coded_size);
            let display = Rc::clone(self.context.display());
            let surfaces = ec_va::create_pool(&display, &spec, 2)?;
            for s in &surfaces {
                fill_black(&display, s, self.coded_size, self.config.bit_depth)?;
            }
            let vpp = Vpp::new(
                &display,
                VA_RT_FORMAT_YUV420 | VA_RT_FORMAT_YUV420_10,
                self.coded_size.0,
                self.coded_size.1,
            )?;
            self.vpp_surfaces = surfaces.into_iter().map(Rc::new).collect();
            self.vpp = Some(vpp);
        }
        let dest = Rc::clone(&self.vpp_surfaces[self.vpp_next]);
        self.vpp_next = (self.vpp_next + 1) % self.vpp_surfaces.len();
        let vpp = self.vpp.as_ref().expect("just built above if it was None");
        if display_size == self.coded_size {
            Ok(vpp.convert(source, dest)?)
        } else {
            let rect = (0, 0, display_size.0, display_size.1);
            Ok(vpp.convert_region(source, rect, dest, rect)?)
        }
    }

    /// Shared submission path for [`Encoder::encode`], [`Encoder::encode_frame`]
    /// and [`Encoder::encode_surface`]: everything after the source surface is
    /// settled.
    fn encode_settled<D: SurfaceMemoryDescriptor>(
        &mut self,
        source: Rc<ec_va::va::Surface<D>>,
        meta: FrameMetadata,
    ) -> Result<CodedFrame> {
        if crate::reset::gpu_reset_epoch() != self.born_epoch {
            return Err(Error::hw_reset(
                "the GPU was reset after this encoder was created; its context is lost",
            ));
        }
        let keyframe = meta.force_keyframe
            || self.reference.is_none()
            || self.gop_position >= self.config.gop_size.max(1);

        let recon = self
            .pool
            .acquire()
            .ok_or_else(|| Error::config("encode: no surface for the reconstruction"))?;

        // 6 bits per pixel of headroom: an intra frame at a low quantiser is the
        // worst case, and a coded buffer that overflows loses the picture.
        let coded_bytes_len = (self.coded_size.0 * self.coded_size.1 * 3 / 4).max(1 << 20);
        let coded = ec_va::error::va_ok(
            "vaCreateBuffer",
            self.context.create_enc_coded(coded_bytes_len as usize),
        )?;

        let mut params = Vec::new();
        match self.config.codec {
            EncCodec::H264 => h264::parameters(self, &recon, coded.id(), keyframe, &mut params)?,
            EncCodec::H265 => hevc::parameters(self, &recon, coded.id(), keyframe, &mut params)?,
            EncCodec::Av1 => av1::parameters(self, &recon, coded.id(), keyframe, &mut params)?,
        }
        params.extend(self.rate_control_buffers()?);
        let mut buffers = Vec::with_capacity(params.len());
        for ty in params {
            buffers.push(ec_va::picture::buffer(&self.context, ty)?);
        }

        let picture = ec_va::picture::submit(&self.context, Rc::clone(&source), buffers)?;
        crate::reset::sync_watched(&source)?;
        drop(picture);

        let mut data = {
            let mapped = ec_va::error::va_ok(
                "vaMapBuffer",
                ec_va::picture::MappedCodedBuffer::new(&coded),
            )?;
            let mut out = Vec::new();
            for segment in mapped.segments() {
                out.extend_from_slice(segment.buf);
            }
            out
        };
        if data.is_empty() {
            return Err(Error::config("the encoder produced an empty coded buffer"));
        }
        if self.config.codec == EncCodec::Av1 {
            // Every AV1 temporal unit begins with a temporal delimiter (5.6),
            // and this driver emits none: its coded buffer starts at the
            // sequence header. ffmpeg's demuxer refuses such a stream
            // ("Missing Temporal Delimiter"), so the two bytes are prepended
            // here rather than left to the caller.
            data.splice(0..0, [0x12u8, 0x00]);
        }
        // The coded buffer is no longer read once it is copied out.
        drop(coded);

        self.reference = Some(recon);
        if keyframe {
            self.idr_count = self.idr_count.wrapping_add(1);
        }
        self.gop_position = if keyframe { 1 } else { self.gop_position + 1 };
        Ok(CodedFrame {
            data,
            timestamp: meta.timestamp,
            is_keyframe: keyframe,
        })
    }

    /// The misc parameter buffers that carry rate control, resent when tuned.
    fn rate_control_buffers(&mut self) -> Result<Vec<BufferType>> {
        if !self.tunings_dirty {
            return Ok(Vec::new());
        }
        self.tunings_dirty = false;
        // VBR's `bits_per_second` is the peak, not the average: libva takes
        // the average as `target_percentage` of it, so an average-rate ask
        // becomes a 1.5x ceiling at 66%.
        let (initial_qp, min_qp, max_qp, bits, target, quality) = match self.config.rate_control {
            RateControlMode::ConstantQp { qp } => (qp, qp, qp, 0, 100, 0),
            RateControlMode::ConstantBitrate => (0, 0, 0, self.tunings.bitrate, 100, 0),
            RateControlMode::VariableBitrate => {
                (0, 0, 0, self.tunings.bitrate.saturating_mul(3) / 2, 66, 0)
            }
            RateControlMode::Qvbr { quality } => (0, 0, 0, self.tunings.bitrate, 100, quality),
        };
        let rc_flags = RcFlags::new(0, 0, 0, 0, 0, 0, 0, 0, 0);
        let mut out = vec![BufferType::EncMiscParameter(EncMiscParameter::RateControl(
            EncMiscParameterRateControl::new(
                bits,
                target,
                RATE_WINDOW_MS,
                initial_qp,
                min_qp,
                0,
                rc_flags,
                0,
                max_qp,
                quality,
                0,
            ),
        ))];
        // libva packs a fractional rate as `den << 16 | num`, and a whole one
        // as the plain numerator.
        let num = self.tunings.framerate.0.max(1);
        let den = self.tunings.framerate.1.max(1);
        let packed = if den <= 1 {
            num
        } else {
            (den << 16) | (num & 0xffff)
        };
        out.push(BufferType::EncMiscParameter(EncMiscParameter::FrameRate(
            EncMiscParameterFrameRate::new(packed, 0),
        )));
        if bits > 0 {
            // The buffer starts full. Starting at three quarters full cost a
            // quarter of a window of budget: radeonsi then spent 12.5% under the
            // ask on a 2 s clip, and the gap is exactly that quarter over the
            // clip length (measured, 2026-10-16). Its size: see `hrd_buffer_ms`.
            let buffer_ms = u64::from(hrd_buffer_ms(self.config.gop_size));
            let hrd_bits = (u64::from(bits) * buffer_ms / 1000) as u32;
            out.push(BufferType::EncMiscParameter(EncMiscParameter::HRD(
                EncMiscParameterHRD::new(hrd_bits, hrd_bits),
            )));
        }
        Ok(out)
    }

    /// Append one packed header: its parameter buffer, then its bytes.
    ///
    /// Both go in the same picture, in this order, which is what the driver
    /// reads them in (`va.h:2446`).
    pub(crate) fn push_packed(
        &self,
        packed: &headers::Packed,
        out: &mut Vec<BufferType>,
    ) -> Result<()> {
        let kind = match packed.kind {
            headers::PACKED_HEADER_SEQUENCE => EncPackedHeaderType::Sequence,
            headers::PACKED_HEADER_PICTURE => EncPackedHeaderType::Picture,
            headers::PACKED_HEADER_SLICE => EncPackedHeaderType::Slice,
            other => {
                return Err(Error::config(format!("unknown packed header kind {other}")));
            }
        };
        out.push(BufferType::EncPackedHeaderParameter(
            EncPackedHeaderParameter::new(kind, packed.bits, true),
        ));
        out.push(BufferType::EncPackedHeaderData(packed.bytes.clone()));
        Ok(())
    }

    pub(crate) fn context(&self) -> &Rc<Context> {
        &self.context
    }

    pub(crate) fn config(&self) -> &EncoderConfig {
        &self.config
    }

    pub(crate) fn reference(&self) -> Option<&Rc<PooledSurface>> {
        self.reference.as_ref()
    }

    pub(crate) fn gop_position(&self) -> u32 {
        self.gop_position
    }

    /// Key frames submitted before the picture now being coded (the count is
    /// bumped once a picture is submitted), so the first IDR reads 0.
    pub(crate) fn idr_count(&self) -> u32 {
        self.idr_count
    }

    /// The VA profile this encoder was configured with.
    pub fn profile(&self) -> Profile {
        self.profile
    }
}

/// The surface format an encoder of `bit_depth` works in.
fn surface_spec(bit_depth: u8, size: (u32, u32)) -> SurfaceSpec {
    let spec = if bit_depth == 10 {
        SurfaceSpec::p010(size.0, size.1)
    } else {
        SurfaceSpec::nv12(size.0, size.1)
    };
    spec.with_usage_hint(VA_SURFACE_ATTRIB_USAGE_HINT_ENCODER)
}

/// The image fourcc matching `bit_depth`.
fn fourcc(bit_depth: u8) -> u32 {
    if bit_depth == 10 {
        VA_FOURCC_P010
    } else {
        VA_FOURCC_NV12
    }
}

/// Copy `surface`'s image through `fill`, then write it back.
///
/// cros-libva writes a mutated image back to the surface when the image is
/// dropped (`vaPutImage`), so `fill` runs against the image's own bytes and
/// the write-back happens at the end of this function.
fn write_surface(
    display: &Display,
    surface: &Surface,
    fourcc: u32,
    size: (u32, u32),
    fill: impl FnOnce(&VAImage, &mut [u8]),
) -> Result<()> {
    let format = ec_va::surface::image_format(display, fourcc)?;
    let mut image = ec_va::surface::create_image(surface, format, size, size)?;
    let layout = *image.image();
    fill(&layout, AsMut::<[u8]>::as_mut(&mut image));
    Ok(())
}

/// Upload one frame into a surface as NV12.
fn upload(
    context: &Rc<Context>,
    frame: &I420,
    surface: &Surface,
    coded: (u32, u32),
) -> Result<()> {
    let display: &Display = context.display();
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    write_surface(display, surface, VA_FOURCC_NV12, coded, |layout, data| {
        let y_pitch = layout.pitches[0] as usize;
        let uv_pitch = layout.pitches[1] as usize;
        let y_base = layout.offsets[0] as usize;
        let uv_base = layout.offsets[1] as usize;
        // The two planes are written in turn; the loops are the interleave
        // `i420_to_nv12` does for callers that own both buffers.
        for row in 0..h {
            let d = y_base + row * y_pitch;
            if let Some(dst) = data.get_mut(d..d + w) {
                dst.copy_from_slice(&frame.y[row * w..row * w + w]);
            } else {
                break;
            }
        }
        for row in 0..ch {
            let d = uv_base + row * uv_pitch;
            let Some(dst) = data.get_mut(d..d + cw * 2) else {
                break;
            };
            for col in 0..cw {
                dst[col * 2] = frame.u[row * cw + col];
                dst[col * 2 + 1] = frame.v[row * cw + col];
            }
        }
    })
}

/// Paint an NV12 surface black (Y=16, chroma=128, TV-range neutral): the
/// one-time initialisation [`Encoder::vpp_into_pool`] gives its pool so a
/// region copy's untouched padding reads the same deterministic value on
/// every driver instead of whatever the fresh allocation contained.
fn fill_black(display: &Display, surface: &Surface, size: (u32, u32), bit_depth: u8) -> Result<()> {
    write_surface(display, surface, fourcc(bit_depth), size, |layout, data| {
        // P010 carries the sample in the top ten bits, so black is the 8-bit
        // value shifted up by six, little endian.
        let samples = |eight: u8| -> Vec<u8> {
            if bit_depth == 8 {
                vec![eight]
            } else {
                (u16::from(eight) << 6).to_le_bytes().to_vec()
            }
        };
        for (plane, value, rows) in [(0usize, 16u8, size.1 as usize), (1, 128, size.1 as usize / 2)] {
            let pitch = layout.pitches[plane] as usize;
            let base = layout.offsets[plane] as usize;
            let unit = samples(value);
            let width = size.0 as usize * unit.len();
            for row in 0..rows {
                let start = base + row * pitch;
                if let Some(line) = data.get_mut(start..start + width) {
                    for px in line.chunks_exact_mut(unit.len()) {
                        px.copy_from_slice(&unit);
                    }
                }
            }
        }
    })
}

/// Upload one 10-bit frame into a surface as P010: samples move from the low
/// ten bits of the caller's `u16` into P010's top ten.
fn upload_16(
    context: &Rc<Context>,
    frame: &I420_16,
    surface: &Surface,
    coded: (u32, u32),
) -> Result<()> {
    let display: &Display = context.display();
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    write_surface(display, surface, VA_FOURCC_P010, coded, |layout, data| {
        let y_pitch = layout.pitches[0] as usize;
        let uv_pitch = layout.pitches[1] as usize;
        let y_base = layout.offsets[0] as usize;
        let uv_base = layout.offsets[1] as usize;
        for row in 0..h {
            let d = y_base + row * y_pitch;
            let Some(dst) = data.get_mut(d..d + w * 2) else {
                break;
            };
            for col in 0..w {
                let v = (frame.y[row * w + col] & 0x3ff) << 6;
                dst[col * 2..col * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
        for row in 0..ch {
            let d = uv_base + row * uv_pitch;
            let Some(dst) = data.get_mut(d..d + cw * 4) else {
                break;
            };
            for col in 0..cw {
                let u = (frame.u[row * cw + col] & 0x3ff) << 6;
                let v = (frame.v[row * cw + col] & 0x3ff) << 6;
                dst[col * 4..col * 4 + 2].copy_from_slice(&u.to_le_bytes());
                dst[col * 4 + 2..col * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_is_high() {
        assert_eq!(H264Profile::default(), H264Profile::High);
    }
}
