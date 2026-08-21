//! Blu-ray Presentation Graphic Stream: the subtitles that are *pictures*.
//!
//! A PGS stream is a sequence of display sets, each a handful of segments: a
//! presentation composition (PCS — the canvas and where the objects go on it), a
//! window definition (WDS), a palette (PDS), one or more run-length objects
//! (ODS) and an end marker (END). A set is decoded against the state the sets
//! before it left behind, which is why this is a decoder with an epoch and not
//! a function: an acquisition point re-states everything, a normal-case set may
//! re-use an object or merely swap the palette.
//!
//! What comes out is one straight-alpha RGBA frame per display set, the size of
//! the composition's own canvas (1920×1080 off a disc), transparent everywhere
//! the cue paints nothing. That is the shape a compositor wants and the shape
//! `.sup` tooling shows: a caller lays the whole canvas over the film rather
//! than placing a sprite.
//!
//! The framing read here is the `.sup` one — `PG`, two timestamps, type, size —
//! because that is what a `.sup` file holds and what a Matroska block becomes
//! once its header is put back on ([`ec_subs`] is the text half of the same
//! story). Colour is BT.601 limited-range, which is what the palette entries of
//! a disc are and what every other decoder of this format assumes.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use ec_core::{
    CodecId, CodecParameters, Decoder, Error, Frame, PixelFormat, Plane, Result, VideoFrame,
};

/// Palette definition segment.
pub const SEG_PDS: u8 = 0x14;
/// Object definition segment: the run-length bitmap itself.
pub const SEG_ODS: u8 = 0x15;
/// Presentation composition segment: canvas size and object placement.
pub const SEG_PCS: u8 = 0x16;
/// Window definition segment.
pub const SEG_WDS: u8 = 0x17;
/// End of display set — the point at which a frame is complete.
pub const SEG_END: u8 = 0x80;

/// The magic every `.sup` segment starts with.
const MAGIC: [u8; 2] = *b"PG";
/// Bytes before a segment's body: magic, PTS, DTS, type, size.
const HEADER: usize = 13;

/// A decoder over a `.sup` byte stream.
///
/// Feed it whole segments ([`PgsDecoder::push`]) — a partial one is held until
/// the rest arrives, so a caller may split the stream anywhere — and take a
/// frame per display set ([`PgsDecoder::take_frame`]).
pub struct PgsDecoder {
    params: CodecParameters,
    /// Bytes not yet forming a whole segment.
    pending: Vec<u8>,
    /// Objects by id, decoded run-length data as the stream stated it.
    objects: HashMap<u16, Object>,
    /// The object being assembled out of a multi-segment ODS sequence.
    partial: Option<(u16, Object)>,
    /// Palettes by id: 256 straight-alpha RGBA entries each.
    palettes: HashMap<u8, Box<[[u8; 4]; 256]>>,
    /// Windows by id, for clipping.
    windows: HashMap<u8, Rect>,
    /// The composition the current display set is building.
    composition: Composition,
    /// Frames finished but not yet taken.
    ready: Vec<VideoFrame>,
}

#[derive(Clone, Default)]
struct Object {
    width: u32,
    height: u32,
    rle: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Clone, Default)]
struct Composition {
    width: u32,
    height: u32,
    palette_id: u8,
    objects: Vec<Placement>,
}

#[derive(Clone, Copy)]
struct Placement {
    object_id: u16,
    window_id: u8,
    x: u32,
    y: u32,
    crop: Option<Rect>,
}

impl Default for PgsDecoder {
    fn default() -> Self {
        PgsDecoder::new()
    }
}

impl PgsDecoder {
    /// A decoder with no epoch yet: the first display set states everything.
    pub fn new() -> PgsDecoder {
        PgsDecoder {
            params: CodecParameters::new(CodecId::Pgs),
            pending: Vec::new(),
            objects: HashMap::new(),
            partial: None,
            palettes: HashMap::new(),
            windows: HashMap::new(),
            composition: Composition::default(),
            ready: Vec::new(),
        }
    }

    /// Feed stream bytes. Every complete segment in them is consumed; a partial
    /// tail is kept for the next call.
    ///
    /// A segment whose body is nonsense (a palette entry short, an object
    /// claiming more pixels than its bitmap holds) is dropped and the stream
    /// carries on: one torn display set out of eleven thousand must not cost
    /// the track.
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        let mut buffer = std::mem::take(&mut self.pending);
        buffer.extend_from_slice(bytes);
        let mut data: &[u8] = &buffer;
        loop {
            if data.len() < HEADER {
                break;
            }
            if data[..2] != MAGIC {
                // Not a segment boundary at all: resynchronising would invent
                // structure, so this is the one hard error.
                return Err(Error::corrupt("PGS: segment does not start with `PG`"));
            }
            let size = u16::from_be_bytes([data[11], data[12]]) as usize;
            if data.len() < HEADER + size {
                break;
            }
            let kind = data[10];
            let body = &data[HEADER..HEADER + size];
            self.segment(kind, body);
            data = &data[HEADER + size..];
        }
        self.pending = data.to_vec();
        Ok(())
    }

    /// The next finished display set, oldest first.
    pub fn take_frame(&mut self) -> Option<VideoFrame> {
        match self.ready.is_empty() {
            true => None,
            false => Some(self.ready.remove(0)),
        }
    }

    /// One segment against the running state.
    fn segment(&mut self, kind: u8, body: &[u8]) {
        match kind {
            SEG_PCS => self.pcs(body),
            SEG_WDS => self.wds(body),
            SEG_PDS => self.pds(body),
            SEG_ODS => self.ods(body),
            SEG_END => {
                if let Some(frame) = self.render() {
                    self.ready.push(frame);
                }
            }
            _ => {}
        }
    }

    fn pcs(&mut self, body: &[u8]) {
        if body.len() < 11 {
            return;
        }
        let composition_state = body[7];
        // Epoch start: nothing from before this point is referenced again.
        if composition_state & 0x80 != 0 {
            self.objects.clear();
            self.palettes.clear();
            self.windows.clear();
            self.partial = None;
        }
        let mut composition = Composition {
            width: u32::from(u16::from_be_bytes([body[0], body[1]])),
            height: u32::from(u16::from_be_bytes([body[2], body[3]])),
            palette_id: body[9],
            objects: Vec::new(),
        };
        let count = body[10] as usize;
        let mut at = 11;
        for _ in 0..count {
            if body.len() < at + 8 {
                break;
            }
            let flags = body[at + 3];
            let cropped = flags & 0x80 != 0;
            let placement = Placement {
                object_id: u16::from_be_bytes([body[at], body[at + 1]]),
                window_id: body[at + 2],
                x: u32::from(u16::from_be_bytes([body[at + 4], body[at + 5]])),
                y: u32::from(u16::from_be_bytes([body[at + 6], body[at + 7]])),
                crop: match cropped && body.len() >= at + 16 {
                    true => Some(Rect {
                        x: u32::from(u16::from_be_bytes([body[at + 8], body[at + 9]])),
                        y: u32::from(u16::from_be_bytes([body[at + 10], body[at + 11]])),
                        width: u32::from(u16::from_be_bytes([body[at + 12], body[at + 13]])),
                        height: u32::from(u16::from_be_bytes([body[at + 14], body[at + 15]])),
                    }),
                    false => None,
                },
            };
            at += if cropped { 16 } else { 8 };
            composition.objects.push(placement);
        }
        self.composition = composition;
    }

    fn wds(&mut self, body: &[u8]) {
        let count = body.first().copied().unwrap_or(0) as usize;
        for i in 0..count {
            let at = 1 + i * 9;
            if body.len() < at + 9 {
                break;
            }
            self.windows.insert(
                body[at],
                Rect {
                    x: u32::from(u16::from_be_bytes([body[at + 1], body[at + 2]])),
                    y: u32::from(u16::from_be_bytes([body[at + 3], body[at + 4]])),
                    width: u32::from(u16::from_be_bytes([body[at + 5], body[at + 6]])),
                    height: u32::from(u16::from_be_bytes([body[at + 7], body[at + 8]])),
                },
            );
        }
    }

    fn pds(&mut self, body: &[u8]) {
        if body.len() < 2 {
            return;
        }
        // A palette segment *updates* the palette it names: entries it does not
        // mention keep whatever the epoch left there.
        let palette = self
            .palettes
            .entry(body[0])
            .or_insert_with(|| Box::new([[0, 0, 0, 0]; 256]));
        for entry in body[2..].chunks_exact(5) {
            palette[entry[0] as usize] = rgba(entry[1], entry[2], entry[3], entry[4]);
        }
    }

    fn ods(&mut self, body: &[u8]) {
        if body.len() < 4 {
            return;
        }
        let id = u16::from_be_bytes([body[0], body[1]]);
        let sequence = body[3];
        if sequence & 0x80 != 0 {
            // First (or only) segment of the sequence: the dimensions ride here.
            if body.len() < 11 {
                return;
            }
            let object = Object {
                width: u32::from(u16::from_be_bytes([body[7], body[8]])),
                height: u32::from(u16::from_be_bytes([body[9], body[10]])),
                rle: body[11..].to_vec(),
            };
            self.partial = Some((id, object));
        } else if let Some((pending_id, object)) = self.partial.as_mut()
            && *pending_id == id
        {
            object.rle.extend_from_slice(&body[4..]);
        }
        if sequence & 0x40 != 0
            && let Some((id, object)) = self.partial.take()
        {
            self.objects.insert(id, object);
        }
    }

    /// The current composition as one RGBA canvas, or `None` when it states no
    /// canvas at all (a display set seen before any PCS).
    fn render(&mut self) -> Option<VideoFrame> {
        let (width, height) = (self.composition.width, self.composition.height);
        if width == 0 || height == 0 {
            return None;
        }
        let stride = width as usize * 4;
        let mut canvas = vec![0u8; stride.checked_mul(height as usize)?];
        let palette = self.palettes.get(&self.composition.palette_id);
        for placement in &self.composition.objects {
            let (Some(object), Some(palette)) = (self.objects.get(&placement.object_id), palette)
            else {
                continue;
            };
            let Some(indices) = decode_rle(&object.rle, object.width, object.height) else {
                continue;
            };
            // A window clips its objects; a crop takes a rectangle out of the
            // object before it is placed.
            let window = self.windows.get(&placement.window_id).copied();
            let crop = placement.crop.unwrap_or(Rect {
                x: 0,
                y: 0,
                width: object.width,
                height: object.height,
            });
            for row in 0..crop.height.min(object.height.saturating_sub(crop.y)) {
                let target_y = placement.y + row;
                if target_y >= height {
                    break;
                }
                for col in 0..crop.width.min(object.width.saturating_sub(crop.x)) {
                    let target_x = placement.x + col;
                    if target_x >= width {
                        break;
                    }
                    if let Some(w) = window
                        && (target_x < w.x
                            || target_y < w.y
                            || target_x >= w.x + w.width
                            || target_y >= w.y + w.height)
                    {
                        continue;
                    }
                    let index = indices[((crop.y + row) * object.width + crop.x + col) as usize];
                    let colour = palette[index as usize];
                    // Fully transparent palette entries paint nothing, which is
                    // what lets two objects share a window.
                    if colour[3] == 0 {
                        continue;
                    }
                    let at = target_y as usize * stride + target_x as usize * 4;
                    canvas[at..at + 4].copy_from_slice(&colour);
                }
            }
        }
        VideoFrame::try_new(
            PixelFormat::Rgba8,
            width,
            height,
            vec![Plane::new(canvas, stride)],
        )
        .ok()
    }
}

impl Decoder for PgsDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &ec_core::Packet) -> Result<()> {
        self.push(&packet.data)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.take_frame() {
            Some(frame) => Ok(Frame::Video(frame)),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn reset(&mut self) {
        let params = self.params.clone();
        *self = PgsDecoder::new();
        self.params = params;
    }
}

/// A PGS decoder as the family's registry hands one out.
pub fn make_decoder(_params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    Ok(Box::new(PgsDecoder::new()))
}

/// One palette entry: BT.601 Y'CbCr with straight alpha, the way a disc writes
/// it, into RGBA.
///
/// The luma is taken at its own scale — `Y' = 235` comes out 235, not the 255
/// an expansion of the 16..235 video range would give. That is not a reading of
/// the Blu-ray spec but a measurement of what every player draws: the oracle's own
/// `pgssub` decoder, burning `fixtures/subs/pgs-1080p.sup` over black, tops out
/// at 235 (`scripts/pgs-white-level.sh`), and a subtitle that is brighter than
/// the one the disc is mastered against is our bug, not the world's.
fn rgba(y: u8, cr: u8, cb: u8, alpha: u8) -> [u8; 4] {
    let y = i32::from(y) << 8;
    let cb = i32::from(cb) - 128;
    let cr = i32::from(cr) - 128;
    let clamp = |v: i32| ((v + 128) >> 8).clamp(0, 255) as u8;
    [
        clamp(y + 359 * cr),
        clamp(y - 88 * cb - 183 * cr),
        clamp(y + 454 * cb),
        alpha,
    ]
}

/// The run-length coding of §8.2 of the format: a byte is a pixel of that
/// palette index; a zero byte opens a run whose length and colour follow.
///
/// `None` for data that does not describe a `width` × `height` bitmap — a
/// truncated object, or one whose rows run past their width.
fn decode_rle(data: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let (width, height) = (width as usize, height as usize);
    let mut out = vec![0u8; width.checked_mul(height)?];
    let mut at = 0;
    let mut x = 0;
    let mut y = 0;
    while at < data.len() {
        let (colour, run) = match data[at] {
            0 => {
                let flags = *data.get(at + 1)?;
                match flags {
                    // A line ends where the encoder says it does, even short.
                    0 => {
                        at += 2;
                        x = 0;
                        y += 1;
                        continue;
                    }
                    // 00 0LLLLLL — L pixels of entry 0.
                    f if f & 0xC0 == 0x00 => {
                        at += 2;
                        (0, usize::from(f & 0x3F))
                    }
                    // 00 01LLLLLL LLLLLLLL — 14-bit run of entry 0.
                    f if f & 0xC0 == 0x40 => {
                        let run = (usize::from(f & 0x3F) << 8) | usize::from(*data.get(at + 2)?);
                        at += 3;
                        (0, run)
                    }
                    // 00 10LLLLLL CC — L pixels of entry CC.
                    f if f & 0xC0 == 0x80 => {
                        let colour = *data.get(at + 2)?;
                        at += 3;
                        (colour, usize::from(f & 0x3F))
                    }
                    // 00 11LLLLLL LLLLLLLL CC — 14-bit run of entry CC.
                    f => {
                        let run = (usize::from(f & 0x3F) << 8) | usize::from(*data.get(at + 2)?);
                        let colour = *data.get(at + 3)?;
                        at += 4;
                        (colour, run)
                    }
                }
            }
            colour => {
                at += 1;
                (colour, 1)
            }
        };
        if y >= height || x + run > width {
            return None;
        }
        out[y * width + x..y * width + x + run].fill(colour);
        x += run;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `.sup` segment: the framing a Matroska block gets back on the way in.
    fn segment(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER + body.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&[0; 8]);
        out.push(kind);
        out.extend_from_slice(&(body.len() as u16).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    /// A display set: an 8×2 white-on-transparent object at (2, 1) of a 16×4
    /// canvas.
    fn display_set() -> Vec<u8> {
        let mut pcs = vec![0, 16, 0, 4, 0x10, 0, 0, 0x80, 0, 0, 1];
        pcs.extend_from_slice(&[0, 0, 0, 0, 0, 2, 0, 1]); // object 0, window 0, at (2,1)
        let wds = vec![1, 0, 0, 0, 0, 0, 0, 16, 0, 4];
        // Entry 1 is opaque white, entry 2 opaque black; entry 0 stays clear.
        let pds = vec![0, 0, 1, 235, 128, 128, 255, 2, 16, 128, 128, 255];
        // Two rows of 8: a run of 8 of entry 1, then a run of 8 of entry 2.
        let rle = vec![0x00, 0x88, 0x01, 0x00, 0x00, 0x00, 0x88, 0x02, 0x00, 0x00];
        let mut ods = vec![0, 0, 0, 0xC0];
        ods.extend_from_slice(&((rle.len() + 4) as u32).to_be_bytes()[1..]);
        ods.extend_from_slice(&[0, 8, 0, 2]);
        ods.extend_from_slice(&rle);
        [
            segment(SEG_PCS, &pcs),
            segment(SEG_WDS, &wds),
            segment(SEG_PDS, &pds),
            segment(SEG_ODS, &ods),
            segment(SEG_END, &[]),
        ]
        .concat()
    }

    #[test]
    fn a_display_set_decodes_to_the_canvas_it_composes() {
        let mut decoder = PgsDecoder::new();
        decoder.push(&display_set()).unwrap();
        let frame = decoder.take_frame().expect("one frame per display set");
        assert_eq!((frame.width, frame.height), (16, 4));
        assert_eq!(frame.format, PixelFormat::Rgba8);
        let stride = frame.planes[0].stride;
        let pixel = |x: usize, y: usize| &frame.planes[0].data[y * stride + x * 4..][..4];
        // Outside the object: nothing painted at all.
        assert_eq!(pixel(0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(1, 1), [0, 0, 0, 0]);
        // Row 1 of the object is white, row 2 is black, both opaque. White is
        // the palette's own `Y' = 235`, black its `Y' = 16` — see [`rgba`].
        assert_eq!(pixel(2, 1), [235, 235, 235, 255]);
        assert_eq!(pixel(9, 1), [235, 235, 235, 255]);
        assert_eq!(pixel(2, 2), [16, 16, 16, 255]);
        // Past the object's width again.
        assert_eq!(pixel(10, 1), [0, 0, 0, 0]);
        assert!(decoder.take_frame().is_none());
    }

    #[test]
    fn a_stream_split_anywhere_decodes_the_same() {
        let bytes = display_set();
        for split in [1, 7, 13, 20, bytes.len() - 1] {
            let mut decoder = PgsDecoder::new();
            decoder.push(&bytes[..split]).unwrap();
            decoder.push(&bytes[split..]).unwrap();
            let frame = decoder.take_frame().expect("split framing still decodes");
            assert_eq!((frame.width, frame.height), (16, 4));
        }
    }

    #[test]
    fn rubbish_never_panics() {
        // Not segment framing at all.
        let mut decoder = PgsDecoder::new();
        assert!(decoder.push(b"not a sup file at all").is_err());
        // Well-framed segments with torn bodies: dropped, no frame, no panic.
        let mut decoder = PgsDecoder::new();
        let torn = [
            segment(SEG_PCS, &[0, 16, 0, 4]),
            segment(SEG_ODS, &[0, 0, 0, 0x80, 9, 9, 9]),
            segment(SEG_PDS, &[0]),
            segment(SEG_END, &[]),
        ]
        .concat();
        decoder.push(&torn).unwrap();
        assert!(decoder.take_frame().is_none());
    }

    #[test]
    fn run_length_shapes_and_their_refusals() {
        // Short form, long form, coloured runs, end of line.
        let data = [0x00, 0x03, 0x01, 0x00, 0x00, 0x00, 0x83, 0x02, 0x00, 0x00];
        let out = decode_rle(&data, 4, 2).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 2, 2, 2, 0]);
        // A run past the row is a refusal, not a wrap.
        assert!(decode_rle(&[0x00, 0x08, 0x01], 4, 1).is_none());
        // A truncated run is a refusal too.
        assert!(decode_rle(&[0x00, 0x80], 4, 1).is_none());
    }

    #[test]
    fn palette_entries_convert_at_their_own_luma_scale() {
        // A disc's white and black land where the oracle's PGS decoder puts them —
        // 235 and 16, not an expansion to 255 and 0 — and alpha rides through
        // untouched.
        assert_eq!(rgba(235, 128, 128, 255), [235, 235, 235, 255]);
        assert_eq!(rgba(16, 128, 128, 128), [16, 16, 16, 128]);
        // A red palette entry: Y=81, Cr=240, Cb=90 in BT.601.
        let red = rgba(81, 240, 90, 255);
        assert!(red[0] > 230 && red[1] < 30 && red[2] < 30, "{red:?}");
    }
}
