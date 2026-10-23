//! Caption extraction from video elementary streams, restoring presentation order.
//!
//! The extractors are a side channel: they read SEI NALs / user data plus the minimum of each
//! picture's header needed to order it (picture order count for H.264/HEVC, `temporal_reference`
//! for MPEG-2) and leave the decoders and their pictures untouched.
//!
//! Every picture is recorded, with or without captions, so that after [`CcStream::finish`] the
//! position in the list is the frame number in presentation order and a frame-rate derived clock
//! is exact. When the container supplied a pts for every picture that wins instead.

use crate::ccdata::{CcTriplet, parse_mpeg2_user_data, parse_t35};
use ec_core::BitReader;
use ec_h264_syntax::{
    Pps, SliceHeader, Sps, nal::NalHeader as H264Header, nal::NalUnitType as H264Nal,
};
use ec_h265_syntax::{Pps as HPps, Sps as HSps};

/// One picture's captions, in decode order, with its ordering key.
#[derive(Clone, Debug)]
struct PictureCc {
    pts_us: Option<i64>,
    epoch: u32,
    order: i64,
    triplets: Vec<CcTriplet>,
}

/// Captions of one presentation-order picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimedCc {
    /// Presentation time in microseconds.
    pub time_us: i64,
    /// The picture's triplets in transmission order (empty for pictures without captions).
    pub triplets: Vec<CcTriplet>,
}

/// Decode-order picture list that reorders itself into presentation order.
#[derive(Clone, Debug, Default)]
pub struct CcStream {
    pictures: Vec<PictureCc>,
}

impl CcStream {
    fn add(&mut self, p: PictureCc) {
        self.pictures.push(p);
    }

    /// Number of pictures seen.
    pub fn len(&self) -> usize {
        self.pictures.len()
    }

    /// True before the first picture.
    pub fn is_empty(&self) -> bool {
        self.pictures.is_empty()
    }

    /// Presentation order. `frame_rate` is `(num, den)` and only matters when pts is missing.
    pub fn finish(mut self, frame_rate: (u32, u32)) -> Vec<TimedCc> {
        let all_pts = !self.pictures.is_empty() && self.pictures.iter().all(|p| p.pts_us.is_some());
        if all_pts {
            self.pictures.sort_by_key(|p| p.pts_us);
            self.pictures
                .into_iter()
                .map(|p| TimedCc {
                    time_us: p.pts_us.unwrap_or(0),
                    triplets: p.triplets,
                })
                .collect()
        } else {
            self.pictures.sort_by_key(|p| (p.epoch, p.order));
            let (n, d) = (frame_rate.0.max(1) as i128, frame_rate.1.max(1) as i128);
            self.pictures
                .into_iter()
                .enumerate()
                .map(|(i, p)| TimedCc {
                    time_us: (i as i128 * 1_000_000 * d / n) as i64,
                    triplets: p.triplets,
                })
                .collect()
        }
    }
}

/// How NAL units are delimited in the bytes handed to an extractor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NalFormat {
    /// Start codes (`00 00 01`).
    AnnexB,
    /// Big-endian length prefix of 1, 2 or 4 bytes (mp4 / mkv samples).
    LengthPrefixed(u8),
}

fn for_each_nal(data: &[u8], fmt: NalFormat, mut f: impl FnMut(&[u8])) {
    match fmt {
        NalFormat::AnnexB => {
            let mut starts = Vec::new();
            let mut i = 0;
            while i + 3 <= data.len() {
                if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                    starts.push(i + 3);
                    i += 3;
                } else {
                    i += 1;
                }
            }
            for (n, &s) in starts.iter().enumerate() {
                let mut e = starts.get(n + 1).map_or(data.len(), |&x| x - 3);
                while e > s && data[e - 1] == 0 {
                    e -= 1;
                }
                if e > s {
                    f(&data[s..e]);
                }
            }
        }
        NalFormat::LengthPrefixed(n) => {
            let n = n.clamp(1, 4) as usize;
            let mut i = 0;
            while i + n <= data.len() {
                let len = data[i..i + n]
                    .iter()
                    .fold(0usize, |a, &b| (a << 8) | b as usize);
                i += n;
                if len == 0 || i + len > data.len() {
                    break;
                }
                f(&data[i..i + len]);
                i += len;
            }
        }
    }
}

fn unescape(p: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(p.len());
    let mut zeros = 0;
    for &b in p {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// Triplets of every ITU-T T.35 message in an SEI RBSP (after the NAL header, unescaped).
fn sei_triplets(rbsp: &[u8]) -> Vec<CcTriplet> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 <= rbsp.len() {
        let mut ptype = 0usize;
        while i < rbsp.len() && rbsp[i] == 0xFF {
            ptype += 255;
            i += 1;
        }
        let Some(&b) = rbsp.get(i) else { break };
        ptype += b as usize;
        i += 1;
        let mut size = 0usize;
        while i < rbsp.len() && rbsp[i] == 0xFF {
            size += 255;
            i += 1;
        }
        let Some(&b) = rbsp.get(i) else { break };
        size += b as usize;
        i += 1;
        let Some(payload) = rbsp.get(i..i + size) else {
            break;
        };
        if let (4, Some(t)) = (ptype, parse_t35(payload)) {
            out.extend(t);
        }
        i += size;
    }
    out
}

/// H.264 caption extractor.
pub struct H264Extractor {
    fmt: NalFormat,
    sps: Vec<Option<Sps>>,
    pps: Vec<Option<Pps>>,
    pending: Vec<CcTriplet>,
    stream: CcStream,
    epoch: u32,
    prev_msb: i64,
    prev_lsb: i64,
    counter: i64,
    last_top_field: Option<(u32, u32)>,
}

impl H264Extractor {
    /// Extractor for the given NAL framing.
    pub fn new(fmt: NalFormat) -> H264Extractor {
        H264Extractor {
            fmt,
            sps: (0..32).map(|_| None).collect(),
            pps: (0..256).map(|_| None).collect(),
            pending: Vec::new(),
            stream: CcStream::default(),
            epoch: 0,
            prev_msb: 0,
            prev_lsb: 0,
            counter: 0,
            last_top_field: None,
        }
    }

    /// Feed `avcC` extradata (sets the length-prefix size and loads the parameter sets).
    pub fn set_extradata(&mut self, avcc: &[u8]) {
        if avcc.len() < 7 || avcc[0] != 1 {
            return;
        }
        self.fmt = NalFormat::LengthPrefixed((avcc[4] & 3) + 1);
        let mut i = 5;
        let mut sets: Vec<&[u8]> = Vec::new();
        for round in 0..2 {
            let Some(&c) = avcc.get(i) else { return };
            let count = if round == 0 {
                (c & 0x1F) as usize
            } else {
                c as usize
            };
            i += 1;
            for _ in 0..count {
                let Some(l) = avcc.get(i..i + 2) else { return };
                let l = u16::from_be_bytes([l[0], l[1]]) as usize;
                let Some(n) = avcc.get(i + 2..i + 2 + l) else {
                    return;
                };
                sets.push(n);
                i += 2 + l;
            }
        }
        for n in sets {
            self.nal(n, None);
        }
    }

    /// Feed one access unit (a container packet) or a whole Annex B stream.
    ///
    /// `pts_us` is attached to every picture that starts inside `data`.
    pub fn push(&mut self, data: &[u8], pts_us: Option<i64>) {
        let fmt = self.fmt;
        for_each_nal(data, fmt, |n| self.nal(n, pts_us));
    }

    fn nal(&mut self, n: &[u8], pts: Option<i64>) {
        let Ok(h) = H264Header::parse(n[0]) else {
            return;
        };
        let rbsp = unescape(&n[1..]);
        match h.unit_type {
            H264Nal::Sps => {
                if let Ok(s) = Sps::parse(&rbsp) {
                    let id = s.id as usize;
                    if id < 32 {
                        self.sps[id] = Some(s);
                    }
                }
            }
            H264Nal::Pps => {
                let sps = &self.sps;
                if let Ok(p) = Pps::parse(&rbsp, |id| sps.get(id as usize).and_then(|s| s.as_ref()))
                {
                    let id = p.id as usize;
                    if id < 256 {
                        self.pps[id] = Some(p);
                    }
                }
            }
            H264Nal::Sei => self.pending.extend(sei_triplets(&rbsp)),
            H264Nal::Slice | H264Nal::SliceIdr => self.slice(h, &rbsp, pts),
            _ => {}
        }
    }

    fn slice(&mut self, h: H264Header, rbsp: &[u8], pts: Option<i64>) {
        let mut r = BitReader::new(rbsp);
        let (Ok(first_mb), Ok(_), Ok(pps_id)) = (r.read_ue(), r.read_ue(), r.read_ue()) else {
            return;
        };
        if first_mb != 0 {
            return;
        }
        let triplets = std::mem::take(&mut self.pending);
        let hdr = self
            .pps
            .get(pps_id as usize)
            .and_then(|p| p.as_ref())
            .and_then(|p| {
                self.sps
                    .get(p.sps_id as usize)
                    .and_then(|s| s.as_ref())
                    .map(|s| (s, p))
            })
            .and_then(|(s, p)| SliceHeader::parse(rbsp, h, s, p).ok().map(|x| (s, x)));
        let Some((sps, sh)) = hdr else {
            self.counter += 1;
            self.stream.add(PictureCc {
                pts_us: pts,
                epoch: self.epoch,
                order: self.counter,
                triplets,
            });
            return;
        };
        if h.unit_type == H264Nal::SliceIdr {
            self.epoch += 1;
            self.prev_msb = 0;
            self.prev_lsb = 0;
            self.counter = 0;
        }
        let order = if sps.pic_order_cnt_type == 0 {
            let max = 1i64 << sps.log2_max_pic_order_cnt_lsb;
            let lsb = sh.pic_order_cnt_lsb as i64;
            let msb = if lsb < self.prev_lsb && self.prev_lsb - lsb >= max / 2 {
                self.prev_msb + max
            } else if lsb > self.prev_lsb && lsb - self.prev_lsb > max / 2 {
                self.prev_msb - max
            } else {
                self.prev_msb
            };
            if h.ref_idc != 0 {
                self.prev_msb = msb;
                self.prev_lsb = lsb;
            }
            msb + lsb
        } else {
            self.counter += 1;
            self.counter
        };
        // Second field of a frame: fold into the first field's picture.
        if sh.field_pic {
            if sh.bottom_field {
                if self.last_top_field == Some((sh.frame_num, self.epoch))
                    && let Some(last) = self.stream.pictures.last_mut()
                {
                    last.triplets.extend(triplets);
                    self.last_top_field = None;
                    return;
                }
            } else {
                self.last_top_field = Some((sh.frame_num, self.epoch));
            }
        } else {
            self.last_top_field = None;
        }
        self.stream.add(PictureCc {
            pts_us: pts,
            epoch: self.epoch,
            order,
            triplets,
        });
    }

    /// End of input; yields the presentation-ordered list.
    pub fn finish(self, frame_rate: (u32, u32)) -> Vec<TimedCc> {
        self.stream.finish(frame_rate)
    }
}

/// HEVC caption extractor.
pub struct HevcExtractor {
    fmt: NalFormat,
    sps: Vec<Option<HSps>>,
    pps: Vec<Option<HPps>>,
    pending: Vec<CcTriplet>,
    stream: CcStream,
    epoch: u32,
    prev_msb: i64,
    prev_lsb: i64,
}

impl HevcExtractor {
    /// Extractor for the given NAL framing.
    pub fn new(fmt: NalFormat) -> HevcExtractor {
        HevcExtractor {
            fmt,
            sps: (0..16).map(|_| None).collect(),
            pps: (0..64).map(|_| None).collect(),
            pending: Vec::new(),
            stream: CcStream::default(),
            epoch: 0,
            prev_msb: 0,
            prev_lsb: 0,
        }
    }

    /// Feed `hvcC` extradata.
    pub fn set_extradata(&mut self, hvcc: &[u8]) {
        if hvcc.len() < 23 {
            return;
        }
        self.fmt = NalFormat::LengthPrefixed((hvcc[21] & 3) + 1);
        let mut i = 23;
        for _ in 0..hvcc[22] {
            let Some(h) = hvcc.get(i..i + 3) else { return };
            let count = u16::from_be_bytes([h[1], h[2]]) as usize;
            i += 3;
            for _ in 0..count {
                let Some(l) = hvcc.get(i..i + 2) else { return };
                let l = u16::from_be_bytes([l[0], l[1]]) as usize;
                let Some(n) = hvcc.get(i + 2..i + 2 + l) else {
                    return;
                };
                self.nal(n, None);
                i += 2 + l;
            }
        }
    }

    /// Feed one access unit or a whole Annex B stream.
    pub fn push(&mut self, data: &[u8], pts_us: Option<i64>) {
        let fmt = self.fmt;
        for_each_nal(data, fmt, |n| self.nal(n, pts_us));
    }

    fn nal(&mut self, n: &[u8], pts: Option<i64>) {
        if n.len() < 3 || n[0] & 0x80 != 0 {
            return;
        }
        let ty = (n[0] >> 1) & 0x3F;
        let layer = ((n[0] & 1) << 5) | (n[1] >> 3);
        if layer != 0 {
            return;
        }
        let rbsp = unescape(&n[2..]);
        match ty {
            33 => {
                if let Ok(s) = HSps::parse(&rbsp) {
                    let id = s.id as usize;
                    if id < 16 {
                        self.sps[id] = Some(s);
                    }
                }
            }
            34 => {
                if let Ok(p) = HPps::parse(&rbsp) {
                    let id = p.id as usize;
                    if id < 64 {
                        self.pps[id] = Some(p);
                    }
                }
            }
            39 | 40 => self.pending.extend(sei_triplets(&rbsp)),
            0..=21 => self.slice(ty, &rbsp, pts),
            _ => {}
        }
    }

    fn slice(&mut self, ty: u8, rbsp: &[u8], pts: Option<i64>) {
        let mut r = BitReader::new(rbsp);
        let Ok(first) = r.read_bit() else { return };
        if !first {
            return;
        }
        let triplets = std::mem::take(&mut self.pending);
        let irap = (16..=23).contains(&ty);
        let idr = ty == 19 || ty == 20;
        let bla = (16..=18).contains(&ty);
        let parsed = (|| {
            if irap {
                r.read_bit().ok()?;
            }
            let pps = self.pps.get(r.read_ue().ok()? as usize)?.as_ref()?;
            let sps = self.sps.get(pps.sps_id as usize)?.as_ref()?;
            r.skip_bits(pps.num_extra_slice_header_bits as u64).ok()?;
            r.read_ue().ok()?;
            if pps.output_flag_present {
                r.read_bit().ok()?;
            }
            if sps.separate_colour_plane {
                r.read_bits(2).ok()?;
            }
            if idr {
                return Some((0i64, sps.log2_max_poc_lsb_minus4 + 4));
            }
            let bits = sps.log2_max_poc_lsb_minus4 + 4;
            Some((r.read_bits(bits).ok()? as i64, bits))
        })();
        if idr || bla {
            self.epoch += 1;
            self.prev_msb = 0;
            self.prev_lsb = 0;
        }
        let order = match parsed {
            Some((lsb, bits)) => {
                let max = 1i64 << bits;
                let msb = if idr || bla {
                    0
                } else if lsb < self.prev_lsb && self.prev_lsb - lsb >= max / 2 {
                    self.prev_msb + max
                } else if lsb > self.prev_lsb && lsb - self.prev_lsb > max / 2 {
                    self.prev_msb - max
                } else {
                    self.prev_msb
                };
                // Sub-layer non-reference pictures (types 0,2,4,...,14 even) do not update prevTid0.
                if ty >= 16 || ty % 2 == 1 {
                    self.prev_msb = msb;
                    self.prev_lsb = lsb;
                }
                msb + lsb
            }
            None => i64::MAX / 2,
        };
        self.stream.add(PictureCc {
            pts_us: pts,
            epoch: self.epoch,
            order,
            triplets,
        });
    }

    /// End of input; yields the presentation-ordered list.
    pub fn finish(self, frame_rate: (u32, u32)) -> Vec<TimedCc> {
        self.stream.finish(frame_rate)
    }
}

/// MPEG-2 video caption extractor (ATSC `GA94` and DVD `CC` user data).
#[derive(Default)]
pub struct Mpeg2Extractor {
    pending: Vec<CcTriplet>,
    stream: CcStream,
    epoch: u32,
    after_picture: bool,
    last_tr: Option<(u32, u32)>,
}

impl Mpeg2Extractor {
    /// New extractor.
    pub fn new() -> Mpeg2Extractor {
        Mpeg2Extractor::default()
    }

    /// Feed elementary stream bytes (a whole stream, or one packet aligned to start codes).
    pub fn push(&mut self, data: &[u8], pts_us: Option<i64>) {
        let buf = data;
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 4 <= buf.len() {
            if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
                starts.push(i + 3);
                i += 3;
            } else {
                i += 1;
            }
        }
        for (n, &s) in starts.iter().enumerate() {
            let e = starts.get(n + 1).map_or(buf.len(), |&x| x - 3);
            let code = buf[s];
            let body = &buf[s + 1..e];
            match code {
                0xB3 => {
                    self.epoch += 1;
                    self.after_picture = false;
                    self.last_tr = None;
                }
                0xB8 => {
                    self.epoch += 1;
                    self.after_picture = false;
                    self.last_tr = None;
                }
                0x00 if body.len() >= 2 => {
                    let tr = ((body[0] as u32) << 2) | (body[1] >> 6) as u32;
                    let triplets = std::mem::take(&mut self.pending);
                    if self.last_tr == Some((self.epoch, tr)) {
                        if let Some(l) = self.stream.pictures.last_mut() {
                            l.triplets.extend(triplets);
                        }
                    } else {
                        self.stream.add(PictureCc {
                            pts_us,
                            epoch: self.epoch,
                            order: tr as i64,
                            triplets,
                        });
                        self.last_tr = Some((self.epoch, tr));
                    }
                    self.after_picture = true;
                }
                0xB2 => {
                    if let Some(t) = parse_mpeg2_user_data(body) {
                        if self.after_picture {
                            if let Some(l) = self.stream.pictures.last_mut() {
                                l.triplets.extend(t);
                            }
                        } else {
                            self.pending.extend(t);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// End of input; yields the presentation-ordered list.
    pub fn finish(self, frame_rate: (u32, u32)) -> Vec<TimedCc> {
        self.stream.finish(frame_rate)
    }
}
