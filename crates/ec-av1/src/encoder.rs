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

/// What [`Av1Encoder::new`] takes: geometry, rate, key-frame cadence and
/// colour. `base_q_idx` is picked directly (0..=255, `crate::encode`'s own
/// unit) rather than a bitrate — a bitrate control loop is not part of this
/// lane's charter, and a caller that wants one can still derive a
/// `base_q_idx` from its own target and set that field.
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
}

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
        })
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

        let encoded: Encoded = if is_key {
            encode_key_frame_inner(
                &padded,
                self.config.base_q_idx,
                DEADZONE,
                &KEY_FRAME_MODES,
                split_blocks(),
                render,
                self.color_config,
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
                self.config.base_q_idx,
                DEADZONE,
                order as u32,
                render,
            )?
        };

        self.reference = Some(encoded.reconstruction.clone());
        self.next_index += 1;
        let cropped = crop_encoded(&encoded, render.0, render.1);
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

    fn have_ffmpeg() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn test_card(width: usize, height: usize, shift: usize) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let sx = (x + shift) % width;
                picture.y[y * width + x] = ((sx * 3 + y * 5) % 256) as u8;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let i = y * (width / 2) + x;
                picture.u[i] = ((100 + shift * 4) % 256) as u8;
                picture.v[i] = ((200 + 256 - shift * 2 % 256) % 256) as u8;
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
}
