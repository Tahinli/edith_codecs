//! DVD-Video subpictures (VobSub): the other bitmap subtitle of the disc era.
//!
//! A subpicture unit (SPU) is `size:u16, control_offset:u16`, a two-field run-length bitmap of
//! 2-bit pixels, and a chain of control sequences. Each sequence has a delay (units of
//! 1024/90000 s after the unit's PTS) and commands: forced start, start, stop, the four palette
//! indices, the four alpha values, the display area, the two field offsets of the bitmap.
//!
//! The palette (16 RGB entries) is not in the unit. It comes from the disc's IFO, from the
//! `.idx` text (`palette: 000000, ffffff, ...`) that Matroska keeps as `CodecPrivate`, or from the
//! `mp4s` sample entry's `esds`. [`parse_idx`] reads the text form.
//!
//! What comes out is the shape `ec-pgs` produces: a straight-alpha RGBA frame the size of the
//! canvas (the screen, from the idx `size:` line or the caller), transparent where the cue paints
//! nothing, plus the cue's timing and forced flag ([`Cue`]).
//!
//! Also here: [`SpuAssembler`] (an SPU is split over several PES packets in a program stream) and
//! [`sub_file_cues`] (a standalone `.sub` MPEG-PS read against its `.idx`).

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use ec_core::{
    CodecId, CodecParameters, Decoder, Error, Frame, PixelFormat, Plane, Result, TimeBase,
    Timestamp, VideoFrame,
};

/// Largest canvas side accepted.
pub const MAX_DIMENSION: u32 = 4096;
/// Ticks of the SPU delay unit: 1024 periods of the 90 kHz clock.
pub const DELAY_UNIT_90K: i64 = 1024;
/// Finished cues kept for [`DvdSubDecoder::take_cue`].
pub const MAX_PENDING: usize = 64;

/// 16 RGB entries.
pub type Palette = [[u8; 3]; 16];

/// The DVD default palette ffmpeg's encoder writes (used when a stream has no palette at all).
pub const DEFAULT_PALETTE: Palette = [
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0xff],
    [0x00, 0xff, 0x00],
    [0xff, 0x00, 0x00],
    [0xff, 0xff, 0x00],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
    [0x80, 0x80, 0x00],
    [0x80, 0x80, 0xff],
    [0x80, 0x00, 0x80],
    [0x80, 0xff, 0x80],
    [0x00, 0x80, 0x80],
    [0xff, 0x80, 0x80],
    [0x55, 0x55, 0x55],
    [0xaa, 0xaa, 0xaa],
];

/// What an `.idx` file (or Matroska `CodecPrivate`) says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Idx {
    /// `size: 720x576`.
    pub size: Option<(u32, u32)>,
    /// `palette:` line.
    pub palette: Option<Palette>,
    /// `id: en, index: 0` pairs.
    pub languages: Vec<(String, u32)>,
    /// `timestamp: hh:mm:ss:mmm, filepos: hex` entries, per the stream selected by the last `id:`
    /// line (index into `languages`). `(stream index, time in ms, byte offset in the .sub)`.
    pub entries: Vec<(u32, i64, u64)>,
}

/// Parses the text of an `.idx` file or a Matroska `S_VOBSUB` `CodecPrivate`. Unknown lines are
/// ignored; this never fails.
pub fn parse_idx(data: &[u8]) -> Idx {
    let text = String::from_utf8_lossy(data);
    let mut idx = Idx::default();
    let mut current = 0u32;
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "size" => {
                if let Some((w, h)) = value.split_once('x')
                    && let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>())
                    && w > 0
                    && h > 0
                    && w <= MAX_DIMENSION
                    && h <= MAX_DIMENSION
                {
                    idx.size = Some((w, h));
                }
            }
            "palette" => {
                let mut p = DEFAULT_PALETTE;
                let mut n = 0;
                for (i, tok) in value.split(',').take(16).enumerate() {
                    if let Ok(v) = u32::from_str_radix(tok.trim(), 16) {
                        p[i] = [(v >> 16) as u8, (v >> 8) as u8, v as u8];
                        n += 1;
                    }
                }
                if n > 0 {
                    idx.palette = Some(p);
                }
            }
            "id" => {
                let lang = value.split(',').next().unwrap_or("").trim().to_string();
                let index = value
                    .split("index:")
                    .nth(1)
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(idx.languages.len() as u32);
                current = index;
                idx.languages.push((lang, index));
            }
            "timestamp" => {
                let (ts, rest) = value.split_once(',').unwrap_or((value, ""));
                let pos = rest
                    .split_once(':')
                    .and_then(|(_, v)| u64::from_str_radix(v.trim(), 16).ok());
                let parts: Vec<&str> = ts.trim().split(':').collect();
                if parts.len() == 4
                    && let Some(pos) = pos
                {
                    let neg = parts[0].starts_with('-');
                    let n = |s: &str| s.trim_start_matches('-').parse::<i64>().ok();
                    if let (Some(h), Some(m), Some(s), Some(ms)) =
                        (n(parts[0]), n(parts[1]), n(parts[2]), n(parts[3]))
                    {
                        let t = ((h * 60 + m) * 60 + s) * 1000 + ms;
                        idx.entries.push((current, if neg { -t } else { t }, pos));
                    }
                }
            }
            _ => {}
        }
    }
    idx
}

/// One decoded subpicture.
#[derive(Debug, Clone)]
pub struct Cue {
    /// Straight-alpha RGBA canvas, transparent where nothing is painted.
    pub frame: VideoFrame,
    /// The display area on the canvas: x, y, width, height.
    pub area: (u32, u32, u32, u32),
    /// Start, in 90 kHz ticks after the unit's PTS.
    pub start_90k: i64,
    /// Stop, in 90 kHz ticks after the unit's PTS; `None` when the unit never states a stop
    /// (shown until the next one).
    pub end_90k: Option<i64>,
    /// The forced-display start command (0x00) was used.
    pub forced: bool,
}

#[derive(Default)]
struct Control {
    start: Option<i64>,
    stop: Option<i64>,
    forced: bool,
    colors: Option<[u8; 4]>,
    alphas: Option<[u8; 4]>,
    area: Option<(u32, u32, u32, u32)>, // x1 x2 y1 y2
    offsets: Option<(usize, usize)>,
}

fn be16(d: &[u8], at: usize) -> Option<usize> {
    Some(usize::from(u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?])))
}

fn parse_controls(d: &[u8]) -> Result<Control> {
    let mut c = Control::default();
    let mut seq = be16(d, 2).ok_or_else(|| Error::corrupt("dvdsub: short unit"))?;
    let mut hops = 0;
    loop {
        hops += 1;
        if hops > 64 {
            return Err(Error::corrupt("dvdsub: control chain too long"));
        }
        let date = be16(d, seq).ok_or_else(|| Error::corrupt("dvdsub: control past end"))? as i64;
        let next = be16(d, seq + 2).ok_or_else(|| Error::corrupt("dvdsub: control past end"))?;
        let mut p = seq + 4;
        loop {
            let Some(&cmd) = d.get(p) else {
                return Err(Error::corrupt("dvdsub: control sequence without end"));
            };
            p += 1;
            match cmd {
                0x00 => {
                    c.forced = true;
                    c.start.get_or_insert(date * DELAY_UNIT_90K);
                }
                0x01 => {
                    c.start.get_or_insert(date * DELAY_UNIT_90K);
                }
                0x02 => {
                    c.stop.get_or_insert(date * DELAY_UNIT_90K);
                }
                0x03 | 0x04 => {
                    let (a, b) = (
                        *d.get(p).ok_or_else(|| Error::corrupt("dvdsub: short command"))?,
                        *d.get(p + 1).ok_or_else(|| Error::corrupt("dvdsub: short command"))?,
                    );
                    p += 2;
                    // Nibbles, high to low, are entries 3, 2, 1, 0.
                    let v = [b & 15, b >> 4, a & 15, a >> 4];
                    if cmd == 0x03 {
                        c.colors = Some(v);
                    } else {
                        c.alphas = Some(v);
                    }
                }
                0x05 => {
                    let b = d.get(p..p + 6).ok_or_else(|| Error::corrupt("dvdsub: short area"))?;
                    p += 6;
                    let x1 = (u32::from(b[0]) << 4) | u32::from(b[1] >> 4);
                    let x2 = (u32::from(b[1] & 15) << 8) | u32::from(b[2]);
                    let y1 = (u32::from(b[3]) << 4) | u32::from(b[4] >> 4);
                    let y2 = (u32::from(b[4] & 15) << 8) | u32::from(b[5]);
                    c.area = Some((x1, x2, y1, y2));
                }
                0x06 => {
                    let o1 = be16(d, p).ok_or_else(|| Error::corrupt("dvdsub: short offsets"))?;
                    let o2 = be16(d, p + 2).ok_or_else(|| Error::corrupt("dvdsub: short offsets"))?;
                    p += 4;
                    c.offsets = Some((o1, o2));
                }
                0x07 => {
                    // Colour/contrast change: a size-prefixed blob this decoder does not apply.
                    let n = be16(d, p).ok_or_else(|| Error::corrupt("dvdsub: short chg_colcon"))?;
                    if n < 2 {
                        return Err(Error::corrupt("dvdsub: bad chg_colcon size"));
                    }
                    p += n;
                }
                0xff => break,
                _ => return Err(Error::corrupt("dvdsub: unknown control command")),
            }
        }
        if next == seq || next >= d.len() {
            break;
        }
        if next < seq {
            return Err(Error::corrupt("dvdsub: control chain goes backwards"));
        }
        seq = next;
    }
    Ok(c)
}

/// Decodes one field of 2-bit run-length data into rows `first, first+2, ...` of `out`
/// (`w` x `h` colour indices). Truncated data leaves the rest of the bitmap at index 0.
fn rle_field(data: &[u8], start: usize, w: usize, h: usize, first: usize, out: &mut [u8]) {
    let total = data.len() * 2;
    let mut nib = start * 2;
    let get = |n: usize| -> u32 {
        let b = data[n / 2];
        u32::from(if n.is_multiple_of(2) { b >> 4 } else { b & 15 })
    };
    let (mut x, mut y) = (0usize, first);
    while y < h && nib < total {
        let mut v = get(nib);
        nib += 1;
        let need = |v: &mut u32, nib: &mut usize| -> bool {
            if *nib >= total {
                return false;
            }
            *v = (*v << 4) | get(*nib);
            *nib += 1;
            true
        };
        if v < 4 {
            if !need(&mut v, &mut nib) {
                return;
            }
            if v < 0x10 {
                if !need(&mut v, &mut nib) {
                    return;
                }
                if v < 0x40 && !need(&mut v, &mut nib) {
                    return;
                }
            }
        }
        let colour = (v & 3) as u8;
        let mut len = (v >> 2) as usize;
        if len == 0 {
            len = w - x;
        }
        let len = len.min(w - x);
        if colour != 0 {
            let row = &mut out[y * w + x..y * w + x + len];
            row.fill(colour);
        }
        x += len;
        if x >= w {
            x = 0;
            y += 2;
            nib = nib.div_ceil(2) * 2; // lines start on a byte
        }
    }
}

/// Decodes one subpicture unit against `palette` onto a canvas of `canvas` pixels (grown if the
/// unit's area lies outside it).
pub fn decode_spu(data: &[u8], palette: &Palette, canvas: (u32, u32)) -> Result<Cue> {
    let size = be16(data, 0).ok_or_else(|| Error::corrupt("dvdsub: short unit"))?;
    // A size of 0 means 64 KiB in some muxers; otherwise the unit may be padded but not short.
    let data = if size >= 4 && size <= data.len() { &data[..size] } else { data };
    let c = parse_controls(data)?;
    let (x1, x2, y1, y2) = c.area.ok_or_else(|| Error::corrupt("dvdsub: no display area"))?;
    let (o1, o2) = c.offsets.ok_or_else(|| Error::corrupt("dvdsub: no bitmap offsets"))?;
    if x2 < x1 || y2 < y1 {
        return Err(Error::corrupt("dvdsub: empty display area"));
    }
    let (w, h) = ((x2 - x1 + 1) as usize, (y2 - y1 + 1) as usize);
    if o1 >= data.len() || o2 >= data.len() {
        return Err(Error::corrupt("dvdsub: bitmap offset past the unit"));
    }
    let mut idx = vec![0u8; w * h];
    rle_field(data, o1, w, h, 0, &mut idx);
    rle_field(data, o2, w, h, 1, &mut idx);
    let colors = c.colors.unwrap_or([0, 1, 2, 3]);
    let alphas = c.alphas.unwrap_or([0, 15, 15, 15]);
    let mut table = [[0u8; 4]; 4];
    for i in 0..4 {
        let rgb = palette[usize::from(colors[i] & 15)];
        table[i] = [rgb[0], rgb[1], rgb[2], alphas[i] * 17];
    }
    let cw = canvas.0.max(x2 + 1).clamp(1, MAX_DIMENSION);
    let ch = canvas.1.max(y2 + 1).clamp(1, MAX_DIMENSION);
    if x2 >= MAX_DIMENSION || y2 >= MAX_DIMENSION {
        return Err(Error::corrupt("dvdsub: area past the dimension limit"));
    }
    let stride = cw as usize * 4;
    let mut px = vec![0u8; stride * ch as usize];
    for row in 0..h {
        let base = (y1 as usize + row) * stride + x1 as usize * 4;
        for col in 0..w {
            let t = table[usize::from(idx[row * w + col])];
            if t[3] != 0 {
                px[base + col * 4..base + col * 4 + 4].copy_from_slice(&t);
            }
        }
    }
    let frame = VideoFrame::try_new(PixelFormat::Rgba8, cw, ch, vec![Plane::new(px, stride)])?;
    Ok(Cue {
        frame,
        area: (x1, y1, w as u32, h as u32),
        start_90k: c.start.unwrap_or(0),
        end_90k: c.stop,
        forced: c.forced,
    })
}

/// Joins the PES fragments of subpicture units into whole units.
///
/// Units are cut from the byte stream by their own size field, so one PES may hold the end of a
/// unit and the start of the next (ffmpeg's VOB muxer packs sectors that way; a disc pads
/// instead). PTS values are handed to units in order: the k-th unit gets the k-th PTS seen.
#[derive(Default)]
pub struct SpuAssembler {
    buf: Vec<u8>,
    pts: VecDeque<i64>,
}

impl SpuAssembler {
    /// Drops any partial unit and pending PTS (after a seek or a re-read).
    pub fn reset(&mut self) {
        self.buf.clear();
        self.pts.clear();
    }

    /// Feeds one PES payload. Returns the units it completed, with their PTS (`None` when the
    /// stream stated fewer PTS values than units).
    pub fn push(&mut self, payload: &[u8], pts: Option<i64>) -> Vec<(Vec<u8>, Option<i64>)> {
        if let Some(p) = pts {
            if self.pts.len() >= 64 {
                self.pts.pop_front();
            }
            self.pts.push_back(p);
        }
        if self.buf.len() + payload.len() > 1 << 20 {
            self.buf.clear();
            return Vec::new();
        }
        self.buf.extend_from_slice(payload);
        let mut out = Vec::new();
        while self.buf.len() >= 2 {
            let size = usize::from(u16::from_be_bytes([self.buf[0], self.buf[1]]));
            if size < 4 {
                // Not a unit (padding, or a torn stream): drop it, wait for the next start.
                self.buf.clear();
                break;
            }
            if self.buf.len() < size {
                break;
            }
            let rest = self.buf.split_off(size);
            let unit = std::mem::replace(&mut self.buf, rest);
            out.push((unit, self.pts.pop_front()));
        }
        out
    }
}

/// Reads a standalone `.sub` (MPEG program stream, private stream 1 substreams 0x20..0x3f) and
/// returns, for `stream`, every unit with its PTS in 90 kHz ticks. The `.idx` is not needed to
/// find the units (they are in the stream), only for the palette.
pub fn sub_file_units(sub: &[u8], stream: u8) -> Vec<(Vec<u8>, i64)> {
    let mut out = Vec::new();
    let mut asm = SpuAssembler::default();
    let mut i = 0;
    while i + 4 <= sub.len() {
        if sub[i] != 0 || sub[i + 1] != 0 || sub[i + 2] != 1 {
            i += 1;
            continue;
        }
        let id = sub[i + 3];
        match id {
            0xBA => {
                // Pack header: MPEG-2 (0x44 marker) is 14 bytes + stuffing; MPEG-1 is 12.
                let Some(&b) = sub.get(i + 4) else { break };
                i += if b >> 6 == 1 {
                    14 + usize::from(sub.get(i + 13).copied().unwrap_or(0) & 7)
                } else {
                    12
                };
            }
            0xB9 => break,
            0xBB..=0xEF => {
                let Some(len) = be16(sub, i + 4) else { break };
                let end = (i + 6 + len).min(sub.len());
                if id == 0xBD {
                    let body = &sub[i + 6..end];
                    if body.len() >= 3 {
                        let flags = body[1];
                        let hl = usize::from(body[2]);
                        let mut pts = None;
                        if flags & 0x80 != 0 && body.len() >= 8 {
                            let p = &body[3..8];
                            pts = Some(
                                (i64::from(p[0] >> 1 & 7) << 30)
                                    | (i64::from(p[1]) << 22)
                                    | (i64::from(p[2] >> 1) << 15)
                                    | (i64::from(p[3]) << 7)
                                    | i64::from(p[4] >> 1),
                            );
                        }
                        if let Some(&sid) = body.get(3 + hl)
                            && (0x20..=0x3f).contains(&sid)
                            && sid - 0x20 == stream
                            && let Some(payload) = body.get(4 + hl..)
                        {
                            for (unit, p) in asm.push(payload, pts) {
                                out.push((unit, p.unwrap_or(0)));
                            }
                        }
                    }
                }
                i = end;
            }
            _ => i += 4,
        }
    }
    out
}

/// A decoder of subpicture units: one packet is one unit.
pub struct DvdSubDecoder {
    params: CodecParameters,
    palette: Palette,
    canvas: (u32, u32),
    ready: VecDeque<(Cue, Option<Timestamp>)>,
}

impl DvdSubDecoder {
    /// A decoder whose palette and canvas come from `.idx`-style text (`extradata`), or the
    /// DVD default palette on a 720x576 canvas.
    pub fn new(extradata: Option<&[u8]>) -> DvdSubDecoder {
        let idx = extradata.map(parse_idx).unwrap_or_default();
        let mut params = CodecParameters::new(CodecId::DvdSub);
        params.extradata = extradata.map(ec_core::Buf::copy_from_slice);
        DvdSubDecoder {
            params,
            palette: idx.palette.unwrap_or(DEFAULT_PALETTE),
            canvas: idx.size.unwrap_or((720, 576)),
            ready: VecDeque::new(),
        }
    }

    /// Decodes one unit; `pts` is the unit's PTS. The cue's `frame.pts` is `pts` plus the start
    /// delay when `pts` is given.
    pub fn push_unit(&mut self, unit: &[u8], pts: Option<Timestamp>) -> Result<()> {
        let mut cue = decode_spu(unit, &self.palette, self.canvas)?;
        if let Some(p) = pts {
            let base = TimeBase::try_new(1, 90_000).map_err(|_| Error::corrupt("dvdsub: time base"))?;
            let delay = Timestamp::new(cue.start_90k, base)
                .rescale(p.base, ec_core::Rounding::Nearest);
            cue.frame.pts = Some(Timestamp::new(p.ticks.saturating_add(delay.ticks), p.base));
        }
        if self.ready.len() >= MAX_PENDING {
            self.ready.pop_front();
        }
        self.ready.push_back((cue, pts));
        Ok(())
    }

    /// The next decoded cue with its timing, oldest first.
    pub fn take_cue(&mut self) -> Option<Cue> {
        self.ready.pop_front().map(|(c, _)| c)
    }
}

impl Decoder for DvdSubDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &ec_core::Packet) -> Result<()> {
        let pts = packet.pts.map(|t| Timestamp::new(t, packet.time_base));
        self.push_unit(&packet.data, pts)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.take_cue() {
            Some(c) => Ok(Frame::Video(c.frame)),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn reset(&mut self) {
        self.ready.clear();
    }
}

/// A DVD subpicture decoder as the family's registry hands one out.
pub fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    Ok(Box::new(DvdSubDecoder::new(params.extradata.as_deref())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit: 4x2 area at (10, 20); row 0 = [1,1,2,2], row 1 = [3,3,0,0] (fill to end).
    fn unit() -> Vec<u8> {
        // Field 0 (row 0): runs (len 2, colour 1), (len 2, colour 2): nibbles 9, A -> 0x9A.
        // Field 1 (row 1): (len 2, colour 3) = 0xB, then len 0 (fill) colour 0 = 0x0000 -> 4 nibbles.
        let rle0 = [0x9A];
        let rle1 = [0xB0, 0x00, 0x00];
        let ctrl_at = 4 + rle0.len() + rle1.len();
        let mut u = vec![0, 0, (ctrl_at >> 8) as u8, ctrl_at as u8];
        u.extend_from_slice(&rle0);
        u.extend_from_slice(&rle1);
        let seq = [
            0x00, 0x00, (ctrl_at >> 8) as u8, ctrl_at as u8, // date 0, next = itself
            0x01, // start
            0x03, 0x32, 0x10, // colours: entries 3,2,1,0 -> palette idx 3,2,1,0
            0x04, 0xFF, 0xF0, // alpha: 15,15,15,0
            0x05, 0x00, 0xA0, 0x0D, 0x01, 0x40, 0x15, // x 10..13, y 20..21
            0x06, 0x00, 0x04, 0x00, 0x05, // field offsets
            0xFF,
        ];
        u.extend_from_slice(&seq);
        let n = u.len();
        u[0] = (n >> 8) as u8;
        u[1] = n as u8;
        u
    }

    #[test]
    fn a_small_unit_decodes_to_its_pixels() {
        let cue = decode_spu(&unit(), &DEFAULT_PALETTE, (32, 32)).unwrap();
        assert_eq!(cue.area, (10, 20, 4, 2));
        let px = &cue.frame.planes[0].data;
        let at = |x: usize, y: usize| &px[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
        assert_eq!(at(10, 20)[3], 255);
        assert_eq!(at(10, 20)[..3], DEFAULT_PALETTE[1]);
        assert_eq!(at(12, 20)[..3], DEFAULT_PALETTE[2]);
        assert_eq!(at(10, 21)[..3], DEFAULT_PALETTE[3]);
        assert_eq!(at(12, 21)[3], 0);
        assert_eq!(at(0, 0)[3], 0);
    }

    #[test]
    fn idx_text_is_read() {
        let i = parse_idx(b"size: 720x480\npalette: 000000, ffffff\nid: de, index: 1\ntimestamp: 00:00:01:500, filepos: 000000800\n");
        assert_eq!(i.size, Some((720, 480)));
        assert_eq!(i.palette.unwrap()[1], [255, 255, 255]);
        assert_eq!(i.languages, vec![("de".to_string(), 1)]);
        assert_eq!(i.entries, vec![(1, 1500, 0x800)]);
    }

    #[test]
    fn rubbish_never_panics() {
        let mut x = 0x9E37_79B9u32;
        let mut mutate = unit();
        for n in 0..4000usize {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            let at = (x >> 8) as usize % mutate.len();
            mutate[at] = (x >> 24) as u8;
            let _ = decode_spu(&mutate, &DEFAULT_PALETTE, (64, 64));
            if n % 50 == 0 {
                mutate = unit();
            }
            let junk: Vec<u8> = (0..(n % 200)).map(|i| (i as u32 ^ x) as u8).collect();
            let _ = decode_spu(&junk, &DEFAULT_PALETTE, (64, 64));
            let _ = sub_file_units(&junk, 0);
        }
    }

    #[test]
    fn units_split_across_and_packed_within_pes_are_cut_by_size() {
        let u = unit();
        let mut stream = u.clone();
        stream.extend_from_slice(&u);
        let mut a = SpuAssembler::default();
        let cut = u.len() + 5;
        let mut got = a.push(&stream[..cut], Some(100));
        got.extend(a.push(&stream[cut..], Some(200)));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], (u.clone(), Some(100)));
        assert_eq!(got[1], (u, Some(200)));
    }
}
