//! The streaming encoder facade — I11 of `lanes/av1-inter-plan.md`, the
//! surface `edith_replica`'s engine calls in place of rav1e.
//!
//! One picture in, one [`Packet`] out, same contract as the repo's other
//! software encoder shims (`shims/rusty_h264`'s `Encoder::try_encode`,
//! "nothing is ever held back"): [`Av1Encoder::encode`] never buffers a
//! picture past its own call, so there is no flush to drain. Internally it
//! is [`crate::encode::encode_key_frame_inner`] and
//! [`crate::encode::encode_inter_frame`] driven by a small state machine —
//! a key frame every `gop` pictures, an inter frame predicting from the
//! previous picture's own reconstruction otherwise — which is the same core
//! [`crate::encode::encode_sequence`] uses for its one-key-frame-then-all-inter
//! case; both now share it rather than each padding/cropping/refreshing the
//! reference on their own.

use ec_av1_syntax::sequence::{ChromaSamplePosition, ColorConfig};
use ec_core::{Error, Result};

use crate::encode::{
    Encoded, Picture, SUPERBLOCK, crop_encoded, encode_inter_frame, encode_key_frame_inner,
    split_blocks,
};
use crate::intra::KEY_FRAME_MODES;

/// A quantizer step round to nearest: half a step either way costs the same
/// rate, so this is the deadzone every facade-driven frame is coded with.
/// (`crate::encode`'s public entry points take a caller-chosen deadzone
/// instead; the facade has no field for it because nothing downstream of it
/// picks one.)
const DEADZONE: f64 = 0.5;

/// The colour a played-back frame is transformed by, named at what a
/// container/player picks a colour space from (spec 5.5.2's CICP triple plus
/// range) rather than at the raw integers. `edith_replica`'s rav1e seat sets
/// limited-range BT.709 or BT.601 (`export.rs:3494-3507`); this crate's
/// pre-facade sequence header always wrote [`Colour::Unspecified`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Colour {
    /// `color_primaries`/`transfer_characteristics`/`matrix_coefficients` =
    /// 1 (BT.709, H.273), studio range: consumer HD video's own space, and
    /// the default a player assumes when nothing is signalled at all.
    #[default]
    Bt709Limited,
    /// The same three fields = 6 (BT.601, H.273), studio range: SD video's
    /// space.
    Bt601Limited,
    /// Every field = 2 ("unspecified", H.273): what this crate wrote before
    /// any of this was configurable.
    Unspecified,
}

impl Colour {
    fn color_config(self) -> ColorConfig {
        let (color_primaries, transfer_characteristics, matrix_coefficients) = match self {
            Colour::Bt709Limited => (1, 1, 1),
            Colour::Bt601Limited => (6, 6, 6),
            Colour::Unspecified => (2, 2, 2),
        };
        ColorConfig {
            bit_depth: 8,
            mono_chrome: false,
            num_planes: 3,
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            // Every variant here is studio (limited) swing; a full-range
            // variant would need its own name, not a fourth field on this
            // enum, since a played-back frame's range is a property of the
            // pictures fed in, not just a label.
            color_range: false,
            subsampling_x: 1,
            subsampling_y: 1,
            chroma_sample_position: ChromaSamplePosition::Unknown,
            separate_uv_delta_q: false,
        }
    }
}

/// A quality/size targeting surface for callers who would rather not pick a
/// `base_q_idx` themselves, oracled at CRF's own shape (a single "quality"
/// dial that holds a stable perceptual level across content, `ffmpeg -crf`)
/// since a from-scratch scheme is not this lane's charter to invent.
/// [`Av1Encoder::with_rate_target`] is additive: [`EncoderConfig::base_q_idx`]
/// keeps working exactly as before for every existing caller, this is a
/// second, opt-in constructor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RateTarget {
    /// Exactly `base_q_idx`, unchanged — the `EncoderConfig` field's own
    /// value, for a caller that already has one.
    QIndex(u8),
    /// A CRF-like single dial, 0 (smallest/worst) to 100 (largest/best),
    /// mapped to a fixed `base_q_idx` by the calibration in
    /// `encode::tests::calibration_sweep_base_q_idx` (linear in `q_idx`
    /// across the sweep's own 40..=240 span, since the measured bytes/PSNR
    /// curve is close enough to log-linear there that a straight line is
    /// the "simplest correct" fit this lane's charter asks for). No
    /// per-frame feedback: one value, picked once at construction.
    Quality(u8),
    /// A closed loop that steers `base_q_idx` frame by frame to land each
    /// coded frame near this many bytes, via [`RateLoop`] — see there for
    /// the controller and its windup bound.
    BytesPerFrame(u32),
}

/// `quality`'s mapping to `base_q_idx`, linear across the calibration sweep's
/// span (`q_idx` 40 at `quality` 100 down to `q_idx` 240 at `quality` 0);
/// `quality` above 100 clamps to the best point measured, matching a CRF
/// dial's own saturating ends rather than extrapolating past calibrated
/// data.
fn quality_to_q_idx(quality: u8) -> u8 {
    let quality = f64::from(quality.min(100));
    (240.0 - quality * 2.0).round() as u8
}

/// The closed-loop controller behind [`RateTarget::BytesPerFrame`]: a
/// proportional step on `base_q_idx`, sized from the calibration sweep's own
/// log-linear slope (bytes roughly halve every ~45 `q_idx` steps there, i.e.
/// `d(ln bytes)/d(q_idx) ≈ -0.0154`), clamped per frame. Pure proportional —
/// no accumulated error term — is the windup bound itself
/// ([[vorbis-rate-loop-windup]]'s class): the only state carried between
/// frames is `q`, already clamped to `0..=255`, so a quiet lead-in cannot
/// build a debt a later transient has to repay; each frame's step is bounded
/// by `STEP_CLAMP` regardless of history.
#[derive(Debug, Clone, Copy)]
struct RateLoop {
    target_bytes: u32,
    q: f64,
}

impl RateLoop {
    /// The steepest a single frame is allowed to move `base_q_idx`, in
    /// either direction — chosen so one wildly over/under-sized frame
    /// (a scene cut, a black lead-in) nudges the next frame's quantizer
    /// rather than slamming it to an extreme.
    const STEP_CLAMP: f64 = 12.0;
    /// The calibration sweep's own slope (see the struct doc), inverted to
    /// convert a bytes ratio into a `q_idx` step.
    const GAIN: f64 = 1.0 / 0.0154;

    fn new(target_bytes: u32, start_q: f64) -> Self {
        Self {
            target_bytes,
            q: start_q,
        }
    }

    fn q_idx(&self) -> u8 {
        self.q.round().clamp(0.0, 255.0) as u8
    }

    /// Steers `q` toward `target_bytes` from this frame's actual coded size.
    fn update(&mut self, actual_bytes: usize) {
        if self.target_bytes == 0 || actual_bytes == 0 {
            return;
        }
        let ratio = actual_bytes as f64 / f64::from(self.target_bytes);
        let step = (ratio.ln() * Self::GAIN).clamp(-Self::STEP_CLAMP, Self::STEP_CLAMP);
        self.q = (self.q + step).clamp(0.0, 255.0);
    }
}

/// What [`Av1Encoder::new`] takes: geometry, rate, key-frame cadence and
/// colour. `base_q_idx` is picked directly (0..=255, `crate::encode`'s own
/// unit) rather than a bitrate — a bitrate control loop is not part of this
/// lane's charter, and a caller that wants one can still derive a
/// `base_q_idx` from its own target and set that field. A caller who wants
/// [`RateTarget`]'s quality/size dials instead of picking `base_q_idx`
/// itself uses [`Av1Encoder::with_rate_target`], which still starts from an
/// `EncoderConfig` (its `base_q_idx` is the loop's seed/fallback for
/// [`RateTarget::BytesPerFrame`], and is overwritten outright for the other
/// two variants).
#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    /// The picture's width in luma samples; must be even and nonzero.
    pub width: usize,
    /// The picture's height in luma samples; must be even and nonzero.
    pub height: usize,
    /// The quantizer index every frame is coded at (0..=255).
    pub base_q_idx: u8,
    /// Pictures between key frames, inclusive of the key frame itself: `1`
    /// codes every picture as a key frame, `gop` codes picture `gop` (and
    /// `2*gop`, ...) as a key frame and every other picture inter.
    pub gop: usize,
    /// The colour the sequence header signals.
    pub colour: Colour,
}

/// One coded picture: its OBUs, in the order a demuxer that just wants bytes
/// on the wire can concatenate them, and the two things a muxer needs to
/// know without parsing them back out.
#[derive(Debug, Clone)]
pub struct Packet {
    /// A temporal delimiter, a sequence header and the frame OBU for a key
    /// frame; a temporal delimiter and the frame OBU alone for an inter
    /// frame (the sequence header is identical across a stream's key
    /// frames, since only [`EncoderConfig`]'s own fields feed it, so it is
    /// simplest to let every key frame carry its own copy rather than have
    /// the facade special-case the first one).
    pub data: Vec<u8>,
    /// Whether this picture was coded as a key frame.
    pub key: bool,
    /// This picture's position in the stream, in coding (== presentation,
    /// since this encoder has no B frames) order, starting at 0.
    pub order: u64,
}

/// The AV1 software encoder: [`EncoderConfig`] in, one [`Packet`] out per
/// [`Av1Encoder::encode`] call.
#[derive(Debug)]
pub struct Av1Encoder {
    config: EncoderConfig,
    color_config: ColorConfig,
    /// The previous picture's own (padded, uncropped) reconstruction — what
    /// the next inter frame predicts from — or `None` right after a key
    /// frame's turn comes up again, where it is about to be replaced rather
    /// than read.
    reference: Option<Picture>,
    next_index: u64,
    /// `Some` only when constructed via
    /// [`Av1Encoder::with_rate_target`]`(_, RateTarget::BytesPerFrame(_))`;
    /// otherwise every picture is coded at `config.base_q_idx`, unchanged
    /// from before this field existed.
    rate_loop: Option<RateLoop>,
    /// This stream's decode-side per-frame state, owned here so `encode`
    /// keeps the public signature it had before the state stopped being
    /// thread-local. One per encoder, never per frame: several of its fields
    /// (the inter-frame inheritance guards) carry state ACROSS frames.
    fctx: crate::decode::FrameCtx,
}

/// The encoder stays `Send` now that it owns a `FrameCtx` (whose cells are
/// `Send` but `!Sync`): it can move to another thread, it just cannot be
/// shared by reference across threads.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Av1Encoder>();
};

impl Av1Encoder {
    /// # Errors
    /// Returns an error when `config.width`/`config.height` are zero, odd,
    /// or larger than the 16-bit frame size an AV1 sequence header carries,
    /// or when `config.gop` is zero (there would be no picture to code the
    /// next key frame from a cadence of).
    pub fn new(config: EncoderConfig) -> Result<Self> {
        if config.gop == 0 {
            return Err(Error::unsupported("AV1 encode", "gop must be at least 1"));
        }
        // Validate the geometry the same way the picture-level entry points
        // do, before the first `encode()` call rather than only surfacing it
        // then.
        Picture::grey(config.width, config.height).check_even()?;
        Ok(Self {
            color_config: config.colour.color_config(),
            config,
            reference: None,
            next_index: 0,
            rate_loop: None,
            fctx: crate::decode::FrameCtx::new(),
        })
    }

    /// [`Av1Encoder::new`], but `rate` picks `base_q_idx` (or steers it,
    /// frame by frame, for [`RateTarget::BytesPerFrame`]) instead of
    /// `config.base_q_idx` being used as-is.
    ///
    /// # Errors
    /// Same as [`Av1Encoder::new`].
    pub fn with_rate_target(mut config: EncoderConfig, rate: RateTarget) -> Result<Self> {
        let rate_loop = match rate {
            RateTarget::QIndex(q) => {
                config.base_q_idx = q;
                None
            }
            RateTarget::Quality(quality) => {
                config.base_q_idx = quality_to_q_idx(quality);
                None
            }
            RateTarget::BytesPerFrame(target_bytes) => {
                Some(RateLoop::new(target_bytes, f64::from(config.base_q_idx)))
            }
        };
        let mut encoder = Self::new(config)?;
        encoder.rate_loop = rate_loop;
        Ok(encoder)
    }

    /// The coded (padded-to-64) frame size every picture in this stream is
    /// coded at — what a decoder allocates and what `ffprobe` reports.
    #[must_use]
    pub fn coded_size(&self) -> (usize, usize) {
        (
            self.config.width.next_multiple_of(SUPERBLOCK),
            self.config.height.next_multiple_of(SUPERBLOCK),
        )
    }

    /// The display (render) size every frame header signals — [`EncoderConfig`]'s
    /// own `width`/`height`, always, whatever the coded size pads it to.
    #[must_use]
    pub fn display_size(&self) -> (usize, usize) {
        (self.config.width, self.config.height)
    }

    /// Encodes one picture, key or inter by this stream's `gop` cadence, and
    /// returns its packet — always exactly one, never held back for a later
    /// call.
    ///
    /// # Errors
    /// Returns an error when `picture`'s size does not match
    /// [`EncoderConfig::width`]/[`EncoderConfig::height`], or under the same
    /// conditions [`crate::encode::encode_key_frame`]/
    /// [`crate::encode::encode_sequence`] do.
    pub fn encode(&mut self, picture: &Picture) -> Result<Packet> {
        if (picture.width, picture.height) != (self.config.width, self.config.height) {
            return Err(Error::unsupported(
                "AV1 encode",
                format!(
                    "picture is {}x{}, encoder is {}x{}",
                    picture.width, picture.height, self.config.width, self.config.height
                ),
            ));
        }
        let render = (self.config.width, self.config.height);
        let padded = picture.padded_to(SUPERBLOCK);
        let is_key = self.next_index % self.config.gop as u64 == 0;
        let order = self.next_index;
        let base_q_idx = self
            .rate_loop
            .as_ref()
            .map_or(self.config.base_q_idx, RateLoop::q_idx);

        let encoded: Encoded = if is_key {
            encode_key_frame_inner(
                &padded,
                base_q_idx,
                DEADZONE,
                &KEY_FRAME_MODES,
                split_blocks(),
                render,
                self.color_config,
                &self.fctx,
            )?
        } else {
            let reference = self.reference.as_ref().ok_or_else(|| {
                Error::unsupported(
                    "AV1 encode",
                    "an inter frame needs a previous reconstruction",
                )
            })?;
            encode_inter_frame(
                &padded,
                reference,
                base_q_idx,
                DEADZONE,
                order as u32,
                render,
                &self.fctx,
            )?
        };

        self.reference = Some(encoded.reconstruction.clone());
        self.next_index += 1;
        let cropped = crop_encoded(&encoded, render.0, render.1);
        if let Some(rate_loop) = self.rate_loop.as_mut() {
            rate_loop.update(cropped.stream.len());
        }
        Ok(Packet {
            data: cropped.stream,
            key: is_key,
            order,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// Whether ffmpeg is on PATH. Absence normally SKIPs, but
    /// `EC_AV1_REQUIRE_FFMPEG=1` -- or `EC_AV1_REQUIRE_AOMENC=1`, since every
    /// aomenc gate decodes its stream through ffmpeg and is meaningless
    /// without it -- turns it into a hard failure. Without this the require
    /// flag was silently short-circuited: `!have_ffmpeg()` is evaluated first
    /// in `if !have_ffmpeg() || !have_aomenc()`, so a machine with no ffmpeg
    /// printed SKIP and reported green (class gate-skips-on-its-own-failure).
    fn have_ffmpeg() -> bool {
        let present = Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(
            present
                || (std::env::var_os("EC_AV1_REQUIRE_FFMPEG").is_none()
                    && std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none()),
            "EC_AV1_REQUIRE_FFMPEG/EC_AV1_REQUIRE_AOMENC is set but no working ffmpeg on PATH"
        );
        present
    }

    fn test_card(width: usize, height: usize, shift: usize) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let sx = (x + shift) % width;
                picture.y[y * width + x] = ((sx * 3 + y * 5) % 256) as u16;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let i = y * (width / 2) + x;
                picture.u[i] = ((100 + shift * 4) % 256) as u16;
                picture.v[i] = ((200 + 256 - shift * 2 % 256) % 256) as u16;
            }
        }
        picture
    }

    /// Every OBU stream this test writes to `ffmpeg`/`ffprobe`, concatenated
    /// in order.
    fn concat(packets: &[Packet]) -> Vec<u8> {
        packets.iter().flat_map(|p| p.data.clone()).collect()
    }

    /// One call to [`Av1Encoder::encode`] returns exactly one packet, always
    /// — the facade's whole "nothing held back" contract, checked without
    /// ffmpeg since it is a property of the return type, not the bytes.
    #[test]
    fn one_in_one_out() {
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 2,
            colour: Colour::Bt709Limited,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        for t in 0..5u64 {
            let packet = enc.encode(&test_card(64, 64, t as usize)).unwrap();
            assert!(!packet.data.is_empty(), "picture {t}: empty packet");
            assert_eq!(packet.order, t, "picture {t}: order");
        }
    }

    /// A key frame every `gop` pictures, inter otherwise — checked against
    /// what the facade itself reports, and (below) against what `ffprobe`
    /// reads back out of the coded bytes.
    #[test]
    fn gop_cadence_is_honored() {
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 3,
            colour: Colour::Unspecified,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let keys: Vec<bool> = (0..7)
            .map(|t| enc.encode(&test_card(64, 64, t)).unwrap().key)
            .collect();
        assert_eq!(keys, vec![true, false, false, true, false, false, true]);
    }

    fn ffprobe_frames(stream: &[u8]) -> Vec<(bool, u32, u32)> {
        let path = std::env::temp_dir().join(format!(
            "ec-av1-facade-probe-{}-{}.obu",
            std::process::id(),
            std::ptr::addr_of!(stream) as usize
        ));
        std::fs::write(&path, stream).expect("writing the probe stream");
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-f",
                "obu",
                "-select_streams",
                "v:0",
                "-show_entries",
                "frame=key_frame,width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .expect("ffprobe failed to run");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "ffprobe refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|line| {
                let mut f = line.trim().split(',');
                let key: u32 = f.next().unwrap().parse().unwrap();
                let width: u32 = f.next().unwrap().parse().unwrap();
                let height: u32 = f.next().unwrap().parse().unwrap();
                (key == 1, width, height)
            })
            .collect()
    }

    fn ffprobe_colour(stream: &[u8]) -> String {
        let path = std::env::temp_dir().join(format!(
            "ec-av1-facade-colour-probe-{}-{}.obu",
            std::process::id(),
            std::ptr::addr_of!(stream) as usize
        ));
        std::fs::write(&path, stream).expect("writing the probe stream");
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-f",
                "obu",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=color_primaries,color_transfer,color_space,color_range",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .expect("ffprobe failed to run");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "ffprobe refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A 30-picture stream at gop 15 codes 2 key frames (pictures 0 and 15)
    /// and 28 inter ones, at an odd-ish (not superblock-multiple) size that
    /// exercises the pad/crop path -- `ffprobe`'s own `key_frame` flags and
    /// coded size confirm what the facade already reported.
    #[test]
    fn thirty_pictures_at_gop_fifteen_decode_to_two_key_frames() {
        if !have_ffmpeg() {
            eprintln!("SKIP thirty_pictures_at_gop_fifteen_decode_to_two_key_frames: no ffmpeg");
            return;
        }
        let (width, height) = (96usize, 96usize);
        let config = EncoderConfig {
            width,
            height,
            base_q_idx: 120,
            gop: 15,
            colour: Colour::Bt709Limited,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        assert_eq!(enc.display_size(), (width, height));
        let (coded_w, coded_h) = enc.coded_size();
        assert_eq!(
            (coded_w, coded_h),
            (
                width.next_multiple_of(SUPERBLOCK),
                height.next_multiple_of(SUPERBLOCK)
            )
        );

        let packets: Vec<Packet> = (0..30)
            .map(|t| enc.encode(&test_card(width, height, t)).unwrap())
            .collect();
        let expect_key: Vec<bool> = (0..30).map(|t| t % 15 == 0).collect();
        assert_eq!(
            packets.iter().map(|p| p.key).collect::<Vec<_>>(),
            expect_key,
            "facade's own key/inter flags"
        );

        let stream = concat(&packets);
        let frames = ffprobe_frames(&stream);
        assert_eq!(frames.len(), 30, "ffprobe frame count");
        for (t, (key, w, h)) in frames.iter().enumerate() {
            assert_eq!(*key, expect_key[t], "picture {t}: ffprobe key_frame flag");
            assert_eq!(
                (*w, *h),
                (width as u32, height as u32),
                "picture {t}: ffprobe reports the true (display) size, not the padded coded one"
            );
        }
    }

    /// The configured colour properties are what `ffprobe` reports back,
    /// spec 5.5.2's CICP triple plus range -- BT.709 limited here, since it
    /// is the default a player assumes and the one this test would silently
    /// pass without wiring for (`ffprobe` reports "unknown" for the
    /// hardcoded "unspecified" this crate wrote before the facade existed,
    /// which is a visibly different string).
    #[test]
    fn bt709_limited_colour_is_reported_by_ffprobe() {
        if !have_ffmpeg() {
            eprintln!("SKIP bt709_limited_colour_is_reported_by_ffprobe: no ffmpeg");
            return;
        }
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 4,
            colour: Colour::Bt709Limited,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let packet = enc.encode(&test_card(64, 64, 0)).unwrap();
        let colour = ffprobe_colour(&packet.data);
        assert_eq!(colour, "tv,bt709,bt709,bt709", "ffprobe colour fields");
    }

    /// The same, for BT.601: a different set of CICP integers must produce a
    /// different string, or the wiring could be a no-op that happens to read
    /// as BT.709 for every input.
    #[test]
    fn bt601_limited_colour_is_reported_by_ffprobe() {
        if !have_ffmpeg() {
            eprintln!("SKIP bt601_limited_colour_is_reported_by_ffprobe: no ffmpeg");
            return;
        }
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 4,
            colour: Colour::Bt601Limited,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let packet = enc.encode(&test_card(64, 64, 0)).unwrap();
        let colour = ffprobe_colour(&packet.data);
        assert_eq!(
            colour, "tv,smpte170m,smpte170m,smpte170m",
            "ffprobe colour fields"
        );
    }

    /// A frame whose size does not match the encoder's configured geometry
    /// is refused by name.
    #[test]
    fn geometry_mismatch_is_refused() {
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 4,
            colour: Colour::Bt709Limited,
        };
        let mut enc = Av1Encoder::new(config).unwrap();
        let err = enc.encode(&Picture::grey(32, 32)).unwrap_err();
        assert!(err.to_string().contains("64x64"), "{err}");
    }

    /// `gop == 0` is refused at construction, not the first `encode()` call.
    #[test]
    fn zero_gop_is_refused() {
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 0,
            colour: Colour::Bt709Limited,
        };
        let err = Av1Encoder::new(config).unwrap_err();
        assert!(err.to_string().contains("gop"), "{err}");
    }

    /// The 12-frame real clip `real_clip_encodes_within_its_quality_and_size_budget`
    /// already gates on at `q=100` (`crates/ec-av1/src/encode.rs`), decoded down
    /// to this facade's own input size, and re-coded through the facade so the
    /// rate-target surface is exercised at the entry point a caller actually
    /// drives.
    fn h264_clip_frames(width: usize, height: usize, frames: usize) -> Option<Vec<Picture>> {
        if !have_ffmpeg() {
            return None;
        }
        let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/video/h264-1080p-23.976-8bit.mp4");
        if !clip.exists() {
            return None;
        }
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i", clip.to_str().unwrap()])
            .args(["-frames:v", &frames.to_string()])
            .args(["-vf", &format!("scale={width}:{height}")])
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
            .output()
            .expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_len = luma + 2 * chroma;
        assert_eq!(
            out.stdout.len(),
            frame_len * frames,
            "expected {frames} 4:2:0 frames"
        );
        Some(
            (0..frames)
                .map(|i| {
                    let bytes = &out.stdout[i * frame_len..][..frame_len];
                    Picture {
                        width,
                        height,
                        y: bytes[..luma].iter().map(|&v| u16::from(v)).collect(),
                        u: bytes[luma..luma + chroma].iter().map(|&v| u16::from(v)).collect(),
                        v: bytes[luma + chroma..].iter().map(|&v| u16::from(v)).collect(),
                    }
                })
                .collect(),
        )
    }

    /// [`RateTarget::BytesPerFrame`] on a real, 24-frame clip lands its total
    /// coded size within ±20% of `frames * target` once the first few
    /// frames' settling has been discarded (the key frame's own reference
    /// state and the controller's first couple of steps).
    #[test]
    fn bytes_per_frame_target_settles_within_20_percent() {
        let Some(pictures) = h264_clip_frames(640, 384, 24) else {
            eprintln!("SKIP bytes_per_frame_target_settles_within_20_percent: no ffmpeg/fixture");
            return;
        };
        let target_bytes = 4_000u32;
        let config = EncoderConfig {
            width: 640,
            height: 384,
            base_q_idx: 100,
            gop: 24,
            colour: Colour::Bt709Limited,
        };
        let mut enc =
            Av1Encoder::with_rate_target(config, RateTarget::BytesPerFrame(target_bytes)).unwrap();
        let sizes: Vec<usize> = pictures
            .iter()
            .map(|p| enc.encode(p).unwrap().data.len())
            .collect();
        // Discard the key frame (always far larger than an inter target) and
        // the next 4 inter frames the loop needs to step toward it.
        let settled = &sizes[5..];
        let mean = settled.iter().sum::<usize>() as f64 / settled.len() as f64;
        let low = f64::from(target_bytes) * 0.8;
        let high = f64::from(target_bytes) * 1.2;
        assert!(
            mean >= low && mean <= high,
            "settled mean {mean:.0} bytes/frame outside ±20% of {target_bytes} ({low:.0}..{high:.0}); sizes={sizes:?}"
        );
    }

    /// The `BytesPerFrame` controller's own windup bound: no frame's coded
    /// `base_q_idx` step exceeds [`RateLoop::STEP_CLAMP`], across a real
    /// clip's full range of content (so a scene cut can't be the one frame
    /// that breaks the bound).
    #[test]
    fn bytes_per_frame_controller_never_oscillates_past_its_clamp() {
        let Some(pictures) = h264_clip_frames(640, 384, 24) else {
            eprintln!(
                "SKIP bytes_per_frame_controller_never_oscillates_past_its_clamp: no ffmpeg/fixture"
            );
            return;
        };
        let config = EncoderConfig {
            width: 640,
            height: 384,
            base_q_idx: 100,
            gop: 24,
            colour: Colour::Bt709Limited,
        };
        let mut enc =
            Av1Encoder::with_rate_target(config, RateTarget::BytesPerFrame(4_000)).unwrap();
        let mut prev_q = enc.rate_loop.as_ref().unwrap().q_idx();
        for picture in &pictures {
            enc.encode(picture).unwrap();
            let q = enc.rate_loop.as_ref().unwrap().q_idx();
            let step = (i32::from(q) - i32::from(prev_q)).abs();
            assert!(
                f64::from(step) <= RateLoop::STEP_CLAMP + 1.0, // +1 for u8 rounding
                "q stepped from {prev_q} to {q}, past the {} clamp",
                RateLoop::STEP_CLAMP
            );
            prev_q = q;
        }
    }

    fn psnr(a: &[u16], b: &[u16]) -> f64 {
        let squared: f64 = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| {
                let d = f64::from(x) - f64::from(y);
                d * d
            })
            .sum();
        if squared == 0.0 {
            return f64::INFINITY;
        }
        10.0 * (255.0 * 255.0 * a.len() as f64 / squared).log10()
    }

    /// Decodes an OBU stream back to raw luma planes via `ffmpeg`/dav1d, one
    /// entry per coded frame, in order — this facade's own equivalent of
    /// `encode::tests::ffmpeg_decode_sequence`, needed here too since a
    /// PSNR check needs the decoded pixels, not just the coded byte count.
    fn ffmpeg_decode_luma(stream: &[u8], width: usize, height: usize) -> Vec<Vec<u8>> {
        let path = std::env::temp_dir().join(format!(
            "ec-av1-facade-rate-decode-{}-{}.obu",
            std::process::id(),
            std::ptr::addr_of!(stream) as usize
        ));
        std::fs::write(&path, stream).expect("writing the decode probe stream");
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-f", "obu", "-i"])
            .arg(&path)
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
            .output()
            .expect("ffmpeg failed to run");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_len = luma + 2 * chroma;
        out.stdout
            .chunks_exact(frame_len)
            .map(|f| f[..luma].to_vec())
            .collect()
    }

    /// [`RateTarget::Quality`] is monotone: a higher quality dial never
    /// produces a smaller stream or a worse mean PSNR than a lower one, on
    /// the same real clip.
    #[test]
    fn quality_target_is_monotone_in_bytes_and_psnr() {
        if !have_ffmpeg() {
            eprintln!("SKIP quality_target_is_monotone_in_bytes_and_psnr: no ffmpeg");
            return;
        }
        let (width, height) = (640, 384);
        let Some(pictures) = h264_clip_frames(width, height, 8) else {
            eprintln!("SKIP quality_target_is_monotone_in_bytes_and_psnr: no fixture");
            return;
        };
        let mut prev_bytes = 0usize;
        let mut prev_psnr = 0.0f64;
        for quality in [20u8, 50, 80] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: 100,
                gop: 8,
                colour: Colour::Bt709Limited,
            };
            let mut enc =
                Av1Encoder::with_rate_target(config, RateTarget::Quality(quality)).unwrap();
            let mut stream = Vec::new();
            let mut total_bytes = 0usize;
            for picture in &pictures {
                let packet = enc.encode(picture).unwrap();
                total_bytes += packet.data.len();
                stream.extend_from_slice(&packet.data);
            }
            let decoded = ffmpeg_decode_luma(&stream, width, height);
            assert_eq!(
                decoded.len(),
                pictures.len(),
                "quality {quality}: dav1d frame count"
            );
            let mean_psnr: f64 = decoded
                .iter()
                .zip(&pictures)
                .map(|(d, p)| {
                    let d16: Vec<u16> = d.iter().map(|&v| u16::from(v)).collect();
                    psnr(&d16, &p.y)
                })
                .sum::<f64>()
                / decoded.len() as f64;
            assert!(
                total_bytes >= prev_bytes,
                "quality {quality}: {total_bytes} bytes not >= previous {prev_bytes}"
            );
            assert!(
                mean_psnr >= prev_psnr - 0.01, // rounding slack, same-ish q_idx neighbours
                "quality {quality}: {mean_psnr:.2} dB not >= previous {prev_psnr:.2} dB"
            );
            prev_bytes = total_bytes;
            prev_psnr = mean_psnr;
        }
    }
}
