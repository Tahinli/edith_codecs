//! Caption stream synthesis for tests and tools: a CEA-608 byte-pair generator, `cc_data`
//! builders, and SEI / user-data writers that splice captions into existing H.264, HEVC and
//! MPEG-2 elementary streams.

use crate::ccdata::CcTriplet;

/// Set odd parity on bit 7.
pub fn parity(b: u8) -> u8 {
    let b = b & 0x7F;
    if b.count_ones().is_multiple_of(2) {
        b | 0x80
    } else {
        b
    }
}

/// A 608 byte-pair script builder (field 1). Each element is one pair; schedule one pair per frame.
#[derive(Default, Clone, Debug)]
pub struct Script608 {
    /// The pairs, parity already applied.
    pub pairs: Vec<[u8; 2]>,
}

impl Script608 {
    /// Empty script.
    pub fn new() -> Script608 {
        Script608::default()
    }

    /// A control pair, sent twice as the standard asks. `ch2` selects the second data channel.
    pub fn ctrl(&mut self, b1: u8, b2: u8) -> &mut Self {
        let p = [parity(b1), parity(b2)];
        self.pairs.push(p);
        self.pairs.push(p);
        self
    }

    /// RCL (resume caption loading).
    pub fn rcl(&mut self) -> &mut Self {
        self.ctrl(0x14, 0x20)
    }
    /// ENM (erase non-displayed memory).
    pub fn enm(&mut self) -> &mut Self {
        self.ctrl(0x14, 0x2E)
    }
    /// EDM (erase displayed memory).
    pub fn edm(&mut self) -> &mut Self {
        self.ctrl(0x14, 0x2C)
    }
    /// EOC (end of caption).
    pub fn eoc(&mut self) -> &mut Self {
        self.ctrl(0x14, 0x2F)
    }
    /// CR (carriage return).
    pub fn cr(&mut self) -> &mut Self {
        self.ctrl(0x14, 0x2D)
    }
    /// RUn (n = 2..=4).
    pub fn roll_up(&mut self, n: u8) -> &mut Self {
        self.ctrl(0x14, 0x23 + n)
    }
    /// RDC (resume direct captioning, paint-on).
    pub fn rdc(&mut self) -> &mut Self {
        self.ctrl(0x14, 0x29)
    }
    /// PAC for 1-based `row` (1..=15), `indent` 0,4,..,28 columns, white; `underline` flag.
    pub fn pac(&mut self, row: usize, indent: usize, underline: bool) -> &mut Self {
        self.pac_style(row, indent, 0, false, underline)
    }
    /// PAC with colour index 0..6 or italics.
    pub fn pac_style(
        &mut self,
        row: usize,
        indent: usize,
        color: u8,
        italic: bool,
        underline: bool,
    ) -> &mut Self {
        let (b1, hi) = match row {
            1 => (0x11, 0),
            2 => (0x11, 1),
            3 => (0x12, 0),
            4 => (0x12, 1),
            5 => (0x15, 0),
            6 => (0x15, 1),
            7 => (0x16, 0),
            8 => (0x16, 1),
            9 => (0x17, 0),
            10 => (0x17, 1),
            11 => (0x10, 0),
            12 => (0x13, 0),
            13 => (0x13, 1),
            14 => (0x14, 0),
            _ => (0x14, 1),
        };
        let v: u8 = if indent > 0 {
            8 + (indent / 4) as u8
        } else if italic {
            7
        } else {
            color
        };
        let b2 = 0x40 + hi * 0x20 + (v << 1) + u8::from(underline);
        self.ctrl(b1, b2)
    }
    /// Mid-row code: colour 0..6, or italics when `italic`.
    pub fn midrow(&mut self, color: u8, italic: bool, underline: bool) -> &mut Self {
        let v = if italic { 7 } else { color };
        self.ctrl(0x11, 0x20 + (v << 1) + u8::from(underline))
    }
    /// Special character 0..15 (`0x30 + n`), e.g. 7 is the music note.
    pub fn special(&mut self, n: u8) -> &mut Self {
        self.pairs.push([parity(0x11), parity(0x30 + n)]);
        self
    }
    /// Extended character: `set` 0 for 0x12, 1 for 0x13; `n` 0..31. Preceded by a filler char.
    pub fn extended(&mut self, set: u8, n: u8) -> &mut Self {
        self.pairs.push([parity(b'?'), 0x80]);
        self.pairs.push([parity(0x12 + set), parity(0x20 + n)]);
        self
    }
    /// Plain text (basic character set bytes), two chars per pair, padded with a null.
    pub fn text(&mut self, s: &str) -> &mut Self {
        for c in s.as_bytes().chunks(2) {
            self.pairs
                .push([parity(c[0]), if c.len() > 1 { parity(c[1]) } else { 0x80 }]);
        }
        self
    }
    /// Null pairs (idle frames).
    pub fn idle(&mut self, n: usize) -> &mut Self {
        for _ in 0..n {
            self.pairs.push([0x80, 0x80]);
        }
        self
    }
}

/// Lay pairs onto frames starting at `start_frame`: one field 1 triplet per frame. Frames
/// without a pair stay empty. Returns `frames` entries.
pub fn schedule(frames: usize, start_frame: usize, pairs: &[[u8; 2]]) -> Vec<Vec<CcTriplet>> {
    let mut out = vec![Vec::new(); frames];
    for (i, p) in pairs.iter().enumerate() {
        if let Some(f) = out.get_mut(start_frame + i) {
            f.push(CcTriplet {
                valid: true,
                cc_type: 0,
                data: *p,
            });
        }
    }
    out
}

/// Serialise `cc_data()` (flags, em_data, triplets, marker).
pub fn cc_data_bytes(t: &[CcTriplet]) -> Vec<u8> {
    let mut v = vec![0xC0 | (t.len() as u8 & 0x1F), 0xFF];
    for x in t {
        v.push(0xF8 | (u8::from(x.valid) << 2) | (x.cc_type & 3));
        v.extend_from_slice(&x.data);
    }
    v.push(0xFF);
    v
}

fn escape(rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// SEI RBSP carrying one ATSC A/53 T.35 message.
pub fn sei_rbsp(triplets: &[CcTriplet]) -> Vec<u8> {
    let cc = cc_data_bytes(triplets);
    let mut payload = vec![0xB5, 0x00, 0x31, b'G', b'A', b'9', b'4', 0x03];
    payload.extend(cc);
    let mut r = vec![4u8];
    let mut n = payload.len();
    while n >= 255 {
        r.push(0xFF);
        n -= 255;
    }
    r.push(n as u8);
    r.extend(payload);
    r.push(0x80);
    r
}

fn nals(data: &[u8]) -> Vec<(usize, usize)> {
    // (start of start code, end) ranges for each NAL including its start code
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &s)| (s, starts.get(n + 1).copied().unwrap_or(data.len())))
        .collect()
}

/// Insert an SEI NAL before the first slice of every picture of an Annex B H.264 (`hevc` false)
/// or HEVC (`hevc` true) stream. `per_picture[k]` is the triplet list of the k-th picture in
/// decode order; a missing or empty entry inserts nothing.
pub fn insert_sei(stream: &[u8], hevc: bool, per_picture: &[Vec<CcTriplet>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(stream.len() + per_picture.len() * 24);
    let mut pic = 0usize;
    for (s, e) in nals(stream) {
        let mut sc = s;
        while stream[sc] == 0 {
            sc += 1;
        }
        let hdr = sc + 1;
        let first_slice = if hevc {
            let ty = (stream[hdr] >> 1) & 0x3F;
            ty < 32 && stream.get(hdr + 2).is_some_and(|b| b & 0x80 != 0)
        } else {
            let ty = stream[hdr] & 0x1F;
            (ty == 1 || ty == 5) && stream.get(hdr + 1).is_some_and(|b| b & 0x80 != 0)
        };
        if first_slice {
            if let Some(t) = per_picture.get(pic).filter(|t| !t.is_empty()) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                if hevc {
                    out.extend_from_slice(&[0x4E, 0x01]);
                } else {
                    out.push(0x06);
                }
                out.extend(escape(&sei_rbsp(t)));
            }
            pic += 1;
        }
        out.extend_from_slice(&stream[s..e]);
    }
    out
}

/// Insert ATSC `GA94` user data after each picture header / picture coding extension of an
/// MPEG-2 elementary stream. `per_picture[k]` is the k-th picture in decode order. With `dvd`
/// the `CC 01 F8` form (field 1 / field 2 pairs only) is written instead.
pub fn insert_mpeg2_user_data(stream: &[u8], per_picture: &[Vec<CcTriplet>], dvd: bool) -> Vec<u8> {
    let segs = nals(stream);
    let mut out = Vec::with_capacity(stream.len() + per_picture.len() * 24);
    let mut pic = 0usize;
    let mut pending: Option<usize> = None;
    for (k, &(s, e)) in segs.iter().enumerate() {
        out.extend_from_slice(&stream[s..e]);
        let code = stream[s + 3];
        let is_ext = code == 0xB5 && stream.get(s + 4).is_some_and(|b| b >> 4 == 8);
        if code == 0x00 {
            pending = Some(pic);
            pic += 1;
        }
        let next_is_ext = segs.get(k + 1).is_some_and(|&(s2, _)| {
            stream[s2 + 3] == 0xB5 && stream.get(s2 + 4).is_some_and(|b| b >> 4 == 8)
        });
        if let Some(p) = pending
            && (is_ext || (code == 0x00 && !next_is_ext))
        {
            {
                pending = None;
                if let Some(t) = per_picture.get(p).filter(|t| !t.is_empty()) {
                    out.extend_from_slice(&[0, 0, 1, 0xB2]);
                    if dvd {
                        let n = t.len().min(31) as u8;
                        out.extend_from_slice(&[b'C', b'C', 0x01, 0xF8, 0xC0 | (n << 1)]);
                        for x in t {
                            out.extend_from_slice(&[
                                if x.cc_type == 0 { 0xFF } else { 0xFE },
                                x.data[0],
                                x.data[1],
                            ]);
                        }
                    } else {
                        out.extend_from_slice(b"GA94\x03");
                        out.extend(cc_data_bytes(t));
                    }
                }
            }
        }
    }
    out
}
