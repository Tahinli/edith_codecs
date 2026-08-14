//! The `rusty_h264` 0.9.1 surface, served by `ec-h264`.
//!
//! Only what edith consumes is here, at the incumbent's signatures:
//! `Decoder::new` + `decode` over whole access units (decode.rs:1478-1500,
//! export.rs:4200), and `EncoderConfig` + `Encoder::new` + `try_encode` +
//! `try_flush` with `YuvFrame` and `Preset` (export.rs:3407-3422, 3640-3650,
//! 3727-3736).
//!
//! Two differences from the incumbent, both deliberate and both visible only
//! as *absences*:
//!
//! - No `#[global_allocator]`. The incumbent's `common` crate installed one
//!   process-wide and had to be vendored out; there is nothing to vendor here.
//! - No lookahead buffering. `try_encode` returns the access unit of the
//!   picture it was handed, every time, so `try_flush` has nothing left to
//!   give and the caller's per-picture timing is exact whatever the rate
//!   control mode.
//!
//! Pictures come out in decode order, which is what the incumbent did and what
//! edith's decode loop assumes. [`Decoder::in_display_order`] is the same
//! decoder with the reordering the incumbent never had.

#![forbid(unsafe_code)]

use ec_h264::{NalOutcome, OutputOrder, PictureView};

/// The incumbent's error type: one variant per refusal reason.
#[derive(Debug)]
pub enum Error {
    NeedMore,
    Invalid(String),
    Unsupported(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NeedMore => write!(f, "need more data"),
            Error::Invalid(m) => write!(f, "invalid: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<ec_h264::Error> for Error {
    fn from(e: ec_h264::Error) -> Error {
        match e {
            ec_h264::Error::NeedMore => Error::NeedMore,
            ec_h264::Error::Unsupported { .. } => Error::Unsupported(e.to_string()),
            other => Error::Invalid(other.to_string()),
        }
    }
}

/// A decoded — or about to be encoded — I420 picture, planes owned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct YuvFrame {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

/// Speed/quality ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    #[default]
    Fast,
    Balanced,
}

impl From<Preset> for ec_h264::Preset {
    fn from(p: Preset) -> ec_h264::Preset {
        match p {
            Preset::Fast => ec_h264::Preset::Fast,
            Preset::Balanced => ec_h264::Preset::Balanced,
        }
    }
}

/// The H.264 software decoder.
pub struct Decoder {
    inner: ec_h264::Decoder,
}

impl Default for Decoder {
    fn default() -> Decoder {
        Decoder::new()
    }
}

impl Decoder {
    /// A decoder that emits pictures in decode order.
    pub fn new() -> Decoder {
        let mut inner = ec_h264::Decoder::new();
        inner.set_output_order(OutputOrder::Decode);
        Decoder { inner }
    }

    /// A decoder that emits pictures in *display* order, which a stream with
    /// B pictures needs and the incumbent could not do. Pictures then come out
    /// with the stream's own reordering delay, so a caller that stops feeding
    /// must call [`Decoder::flush`] to collect the tail.
    pub fn in_display_order() -> Decoder {
        let mut inner = ec_h264::Decoder::new();
        inner.set_output_order(OutputOrder::Display);
        Decoder { inner }
    }

    /// Decode one access unit (Annex B, one coded picture), returning the
    /// picture it completed if any.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<YuvFrame>, Error> {
        self.feed(au)?;
        if self.inner.picture_open() {
            self.inner.end_picture()?;
        }
        Ok(self.take())
    }

    /// Decode a whole Annex B stream at once, returning every picture in it —
    /// what a test that holds an encoder's output in memory asks for, where
    /// [`Decoder::decode`] serves a demuxer handing over one unit at a time.
    pub fn decode_stream(&mut self, stream: &[u8]) -> Result<Vec<YuvFrame>, Error> {
        let mut out = Vec::new();
        self.feed(stream)?;
        // The tail: whatever the last picture and the reordering delay hold.
        self.inner.flush()?;
        while let Some(frame) = self.take() {
            out.push(frame);
        }
        Ok(out)
    }

    /// Push every NAL in `bytes`, closing a picture wherever one ends.
    fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
            if self.inner.push_nal(nal)? == NalOutcome::PictureBoundary {
                // The next picture starts here: close the one before it.
                self.inner.end_picture()?;
                self.inner.push_nal(nal)?;
            }
        }
        Ok(())
    }

    /// Collect a picture the reordering delay is still holding, at end of
    /// stream.
    pub fn flush(&mut self) -> Result<Option<YuvFrame>, Error> {
        self.inner.flush()?;
        Ok(self.take())
    }

    fn take(&mut self) -> Option<YuvFrame> {
        let frame = self.inner.next_frame()?;
        let (w, h) = (frame.width as usize, frame.height as usize);
        let plane = |i: usize, pw: usize, ph: usize| -> Vec<u8> {
            let p = &frame.planes[i];
            let mut out = Vec::with_capacity(pw * ph);
            for row in 0..ph {
                out.extend_from_slice(&p.data[row * p.stride..row * p.stride + pw]);
            }
            out
        };
        Some(YuvFrame {
            width: w,
            height: h,
            y: plane(0, w, h),
            u: plane(1, w / 2, h / 2),
            v: plane(2, w / 2, h / 2),
        })
    }
}

/// Encoder settings, at the incumbent's field names.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: usize,
    pub height: usize,
    pub framerate: f32,
    /// Bits per second; zero codes at a constant quantiser instead.
    pub bitrate: u32,
    pub gop_size: u32,
    /// Consecutive B pictures. Accepted at any value and coded as zero: this
    /// encoder has no B pictures yet, and refusing the field would break a
    /// caller that sets it.
    pub bframes: u32,
    pub preset: Preset,
}

impl EncoderConfig {
    /// The incumbent's constructor: geometry now, everything else by field.
    pub fn new(width: usize, height: usize) -> EncoderConfig {
        EncoderConfig {
            width,
            height,
            framerate: 30.0,
            bitrate: 0,
            gop_size: 250,
            bframes: 0,
            preset: Preset::Fast,
        }
    }
}

/// The H.264 software encoder.
pub struct Encoder {
    inner: ec_h264::Encoder,
    width: usize,
    height: usize,
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Result<Encoder, Error> {
        let mut inner_cfg = ec_h264::EncoderConfig::new(cfg.width as u32, cfg.height as u32);
        inner_cfg.framerate = cfg.framerate;
        inner_cfg.bitrate = cfg.bitrate;
        inner_cfg.gop_size = cfg.gop_size;
        inner_cfg.bframes = cfg.bframes;
        inner_cfg.preset = cfg.preset.into();
        let inner = ec_h264::Encoder::new(inner_cfg)?;
        Ok(Encoder {
            inner,
            width: cfg.width,
            height: cfg.height,
        })
    }

    /// One picture in, its access unit out — always, with no lookahead delay.
    pub fn try_encode(&mut self, frame: &YuvFrame) -> Result<Vec<u8>, Error> {
        if frame.width != self.width || frame.height != self.height {
            return Err(Error::Invalid(format!(
                "frame is {}x{}, encoder is {}x{}",
                frame.width, frame.height, self.width, self.height
            )));
        }
        let view = PictureView::i420(
            self.width as u32,
            self.height as u32,
            &frame.y,
            &frame.u,
            &frame.v,
        );
        Ok(self.inner.encode(&view)?.au)
    }

    /// Nothing is ever held back, so this is always empty; it exists because
    /// the caller's drain path calls it.
    pub fn try_flush(&mut self) -> Result<Vec<u8>, Error> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, t: usize) -> YuvFrame {
        YuvFrame {
            width: w,
            height: h,
            y: (0..w * h)
                .map(|i| ((i / w + i % w + t * 4) % 256) as u8)
                .collect(),
            u: vec![110; w / 2 * h / 2],
            v: vec![150; w / 2 * h / 2],
        }
    }

    /// The whole surface edith calls, in the order edith calls it: configure,
    /// encode picture by picture, drain, then decode what came out.
    #[test]
    fn the_edith_call_sequence_round_trips() {
        let (w, h) = (176usize, 144usize);
        let mut cfg = EncoderConfig::new(w, h);
        cfg.framerate = 25.0;
        cfg.bitrate = 600_000;
        cfg.gop_size = 5;
        cfg.bframes = 0;
        cfg.preset = Preset::Fast;
        let mut enc = Encoder::new(cfg).expect("encoder");
        let mut units = Vec::new();
        for t in 0..6 {
            let au = enc.try_encode(&frame(w, h, t)).expect("encode");
            assert!(!au.is_empty(), "picture {t} produced no access unit");
            units.push(au);
        }
        assert!(enc.try_flush().expect("flush").is_empty());

        let mut dec = Decoder::new();
        for (t, au) in units.iter().enumerate() {
            let got = dec.decode(au).expect("decode").expect("a picture");
            assert_eq!((got.width, got.height), (w, h), "picture {t} geometry");
            assert_eq!(got.y.len(), w * h);
            assert_eq!(got.u.len(), w / 2 * h / 2);
            // A coded picture is close to its source; this is the shim's own
            // proof that the two halves are wired to each other.
            let src = frame(w, h, t);
            let mse: f64 = got
                .y
                .iter()
                .zip(&src.y)
                .map(|(&a, &b)| {
                    let d = f64::from(a) - f64::from(b);
                    d * d
                })
                .sum::<f64>()
                / (w * h) as f64;
            assert!(mse < 25.0, "picture {t}: mean squared error {mse:.1}");
        }

        // And the same units as one stream, which is how a test that kept the
        // encoder's output in a `Vec` decodes it back.
        let whole: Vec<u8> = units.concat();
        let frames = Decoder::new().decode_stream(&whole).expect("decode stream");
        assert_eq!(frames.len(), units.len(), "one picture per access unit");
    }

    /// A frame whose geometry does not match the encoder is refused by name.
    #[test]
    fn geometry_mismatch_is_refused() {
        let mut enc = Encoder::new(EncoderConfig::new(64, 64)).expect("encoder");
        let err = enc.try_encode(&frame(32, 32, 0)).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
    }
}
