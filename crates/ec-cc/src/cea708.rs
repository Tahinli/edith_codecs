//! CEA-708 (DTVCC) decoder: service 1 only.
//!
//! Service 1 is the primary caption service. The decoder assembles DTVCC packets from `cc_type`
//! 2/3 triplets, splits service blocks and interprets window definition (`DF0`..`DF7`), window
//! selection / clear / display / hide / toggle / delete, pen attributes and location, the C0
//! text controls (ETX, BS, FF, CR, HCR), and the G0/G1/G2/G3 text sets. Other services (2 to 63) are
//! refused by name: their service numbers are reported by [`Cea708Decoder::unsupported_services`]
//! and the 608 compatibility bytes (cc_type 0/1) remain the fallback.

use crate::cea608::{CaptionCue, Cell, EMPTY, push_merged, rows_to_cue};
use std::collections::BTreeSet;

#[derive(Clone)]
struct Window {
    defined: bool,
    visible: bool,
    rows: usize,
    cols: usize,
    relative: bool,
    anchor_v: u8,
    anchor_h: u8,
    anchor_point: u8,
    cells: Vec<Vec<Cell>>,
    pen_r: usize,
    pen_c: usize,
}

impl Window {
    fn new() -> Window {
        Window {
            defined: false,
            visible: false,
            rows: 1,
            cols: 1,
            relative: false,
            anchor_v: 0,
            anchor_h: 0,
            anchor_point: 0,
            cells: Vec::new(),
            pen_r: 0,
            pen_c: 0,
        }
    }
}

struct Service1 {
    win: Vec<Window>,
    cur: usize,
    italic: bool,
    underline: bool,
    color: u8,
    show_since: i64,
    cues: Vec<CaptionCue>,
}

fn color_index(b: u8) -> u8 {
    let (r, g, bl) = ((b >> 4) & 3, (b >> 2) & 3, b & 3);
    match (r >= 2, g >= 2, bl >= 2) {
        (true, true, true) => 0,
        (false, true, false) => 1,
        (false, false, true) => 2,
        (false, true, true) => 3,
        (true, false, false) => 4,
        (true, true, false) => 5,
        (true, false, true) => 6,
        (false, false, false) => 7,
    }
}

fn g2(c: u8) -> char {
    match c {
        0x20 | 0x21 => ' ',
        0x25 => '\u{2026}',
        0x2A => 'Š',
        0x2C => 'Œ',
        0x30 => '\u{2588}',
        0x31 => '\u{2018}',
        0x32 => '\u{2019}',
        0x33 => '\u{201C}',
        0x34 => '\u{201D}',
        0x35 => '\u{2022}',
        0x39 => '\u{2122}',
        0x3A => 'š',
        0x3C => 'œ',
        0x3D => '\u{2120}',
        0x3F => 'Ÿ',
        0x76 => '\u{215B}',
        0x77 => '\u{215C}',
        0x78 => '\u{215D}',
        0x79 => '\u{215E}',
        0x7A => '\u{2502}',
        0x7B => '\u{2510}',
        0x7C => '\u{2514}',
        0x7D => '\u{2500}',
        0x7E => '\u{2518}',
        0x7F => '\u{250C}',
        _ => '_',
    }
}

impl Service1 {
    fn new() -> Service1 {
        Service1 {
            win: vec![Window::new(); 8],
            cur: 0,
            italic: false,
            underline: false,
            color: 0,
            show_since: 0,
            cues: Vec::new(),
        }
    }

    fn display_visible(&self) -> bool {
        self.win.iter().any(|w| {
            w.defined
                && w.visible
                && w.cells
                    .iter()
                    .any(|r| r.iter().any(|c| c.ch != '\0' && c.ch != ' '))
        })
    }

    fn commit(&mut self, t: i64) {
        if self.display_visible() && t > self.show_since {
            let mut rows: Vec<(u8, Vec<Cell>)> = Vec::new();
            for w in self.win.iter().filter(|w| w.defined && w.visible) {
                let (row0, col0) = origin(w);
                for (r, cells) in w.cells.iter().enumerate() {
                    let mut v = vec![EMPTY; col0];
                    v.extend_from_slice(cells);
                    rows.push(((row0 + r) as u8, v));
                }
            }
            rows.sort_by_key(|r| r.0);
            let refs: Vec<(u8, &[Cell])> = rows.iter().map(|(r, v)| (*r, &v[..])).collect();
            if let Some(c) = rows_to_cue(1, &refs, 42.0, 15.0, self.show_since, t) {
                push_merged(&mut self.cues, c);
            }
        }
        self.show_since = t;
    }

    fn put(&mut self, ch: char, t: i64) {
        let was = self.display_visible();
        let cell = Cell {
            ch,
            color: self.color,
            italic: self.italic,
            underline: self.underline,
        };
        let w = &mut self.win[self.cur];
        if !w.defined {
            return;
        }
        if w.pen_c >= w.cols {
            w.pen_c = w.cols - 1;
        }
        let (r, c) = (w.pen_r, w.pen_c);
        w.cells[r][c] = cell;
        if w.pen_c + 1 < w.cols {
            w.pen_c += 1;
        }
        let vis = w.visible;
        if vis && !was && ch != ' ' {
            self.show_since = t;
        }
    }

    fn bitmap(&mut self, bits: u8, t: i64, f: impl Fn(&mut Window)) {
        self.commit(t);
        for i in 0..8 {
            if bits & (1 << i) != 0 && self.win[i].defined {
                f(&mut self.win[i]);
            }
        }
        self.show_since = t;
    }

    fn feed(&mut self, b: &[u8], t: i64) {
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            i += 1;
            let a: [u8; 6] = std::array::from_fn(|k| b.get(i + k).copied().unwrap_or(0));
            match c {
                0x00 => {}
                0x03 => self.commit(t),
                0x08 => {
                    let w = &mut self.win[self.cur];
                    if w.defined && w.pen_c > 0 {
                        w.pen_c -= 1;
                        let (r, cc) = (w.pen_r, w.pen_c);
                        w.cells[r][cc] = EMPTY;
                    }
                }
                0x0C => {
                    self.commit(t);
                    let w = &mut self.win[self.cur];
                    for r in &mut w.cells {
                        r.fill(EMPTY);
                    }
                    w.pen_r = 0;
                    w.pen_c = 0;
                }
                0x0D => {
                    self.commit(t);
                    let w = &mut self.win[self.cur];
                    if w.defined {
                        w.pen_c = 0;
                        if w.pen_r + 1 < w.rows {
                            w.pen_r += 1;
                        } else {
                            w.cells.remove(0);
                            w.cells.push(vec![EMPTY; w.cols]);
                        }
                    }
                }
                0x0E => {
                    let w = &mut self.win[self.cur];
                    if w.defined {
                        let r = w.pen_r;
                        w.cells[r].fill(EMPTY);
                        w.pen_c = 0;
                    }
                }
                0x01..=0x0F => {}
                0x10 => {
                    let e = a[0];
                    i += 1;
                    match e {
                        0x00..=0x07 => {}
                        0x08..=0x0F => i += 1,
                        0x10..=0x17 => i += 2,
                        0x18..=0x1F => i += 3,
                        0x20..=0x7F => self.put(g2(e), t),
                        0x80..=0x87 => i += 4,
                        0x88..=0x8F => i += 5,
                        0x90..=0x9F => i += 1 + (a[0] & 0x3F) as usize,
                        _ => self.put('\u{00A4}', t),
                    }
                }
                0x11..=0x17 => i += 1,
                0x18 => {
                    let code = u16::from_be_bytes([a[0], a[1]]);
                    i += 2;
                    self.put(char::from_u32(code as u32).unwrap_or('?'), t);
                }
                0x19..=0x1F => i += 2,
                0x20..=0x7E => self.put(c as char, t),
                0x7F => self.put('\u{266A}', t),
                0x80..=0x87 => self.cur = (c - 0x80) as usize,
                0x88 => {
                    let m = a[0];
                    i += 1;
                    self.bitmap(m, t, |w| {
                        for r in &mut w.cells {
                            r.fill(EMPTY);
                        }
                        w.pen_r = 0;
                        w.pen_c = 0;
                    });
                }
                0x89 => {
                    let m = a[0];
                    i += 1;
                    self.bitmap(m, t, |w| w.visible = true);
                }
                0x8A => {
                    let m = a[0];
                    i += 1;
                    self.bitmap(m, t, |w| w.visible = false);
                }
                0x8B => {
                    let m = a[0];
                    i += 1;
                    self.bitmap(m, t, |w| w.visible = !w.visible);
                }
                0x8C => {
                    let m = a[0];
                    i += 1;
                    self.bitmap(m, t, |w| *w = Window::new());
                }
                0x8D => i += 1,
                0x8E => {}
                0x8F => {
                    self.commit(t);
                    self.win = vec![Window::new(); 8];
                    self.italic = false;
                    self.underline = false;
                    self.color = 0;
                }
                0x90 => {
                    self.italic = a[1] & 0x80 != 0;
                    self.underline = a[1] & 0x40 != 0;
                    i += 2;
                }
                0x91 => {
                    self.color = color_index(a[0]);
                    i += 3;
                }
                0x92 => {
                    let w = &mut self.win[self.cur];
                    if w.defined {
                        w.pen_r = ((a[0] & 0x0F) as usize).min(w.rows - 1);
                        w.pen_c = ((a[1] & 0x3F) as usize).min(w.cols - 1);
                    }
                    i += 2;
                }
                0x93..=0x96 => {}
                0x97 => i += 4,
                0x98..=0x9F => {
                    let id = (c - 0x98) as usize;
                    self.commit(t);
                    let (p1, p2, p3, p4, p5) = (a[0], a[1], a[2], a[3], a[4]);
                    i += 6;
                    let w = &mut self.win[id];
                    let rows = ((p4 & 0x0F) as usize + 1).min(15);
                    let cols = ((p5 & 0x3F) as usize + 1).min(42);
                    if !w.defined || w.rows != rows || w.cols != cols {
                        w.cells = vec![vec![EMPTY; cols]; rows];
                        w.pen_r = 0;
                        w.pen_c = 0;
                    }
                    w.defined = true;
                    w.rows = rows;
                    w.cols = cols;
                    w.visible = p1 & 0x20 != 0;
                    w.relative = p2 & 0x80 != 0;
                    w.anchor_v = p2 & 0x7F;
                    w.anchor_h = p3;
                    w.anchor_point = p4 >> 4;
                    self.cur = id;
                    self.show_since = t;
                }
                0xA0..=0xFF => self.put(char::from_u32(c as u32).unwrap_or('?'), t),
            }
        }
    }
}

/// Top-left row / column of a window on the 15 by 42 grid.
fn origin(w: &Window) -> (usize, usize) {
    let (v, h) = if w.relative {
        (
            w.anchor_v as usize * 15 / 100,
            w.anchor_h as usize * 42 / 100,
        )
    } else {
        (
            w.anchor_v as usize * 15 / 75,
            w.anchor_h as usize * 42 / 210,
        )
    };
    let row = match w.anchor_point / 3 {
        0 => v,
        1 => v.saturating_sub((w.rows - 1) / 2),
        _ => v.saturating_sub(w.rows - 1),
    };
    let col = match w.anchor_point % 3 {
        0 => h,
        1 => h.saturating_sub(w.cols / 2),
        _ => h.saturating_sub(w.cols - 1),
    };
    (row.min(14), col.min(41))
}

/// DTVCC decoder for service 1.
pub struct Cea708Decoder {
    pkt: Vec<u8>,
    need: usize,
    svc: Service1,
    unsupported: BTreeSet<u8>,
}

impl Default for Cea708Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Cea708Decoder {
    /// New decoder.
    pub fn new() -> Cea708Decoder {
        Cea708Decoder {
            pkt: Vec::new(),
            need: 0,
            svc: Service1::new(),
            unsupported: BTreeSet::new(),
        }
    }

    /// Service numbers (2..=63) that appeared and were refused.
    pub fn unsupported_services(&self) -> Vec<u8> {
        self.unsupported.iter().copied().collect()
    }

    /// Feed one `cc_type` 2 (packet data) or 3 (packet start) triplet.
    pub fn push_triplet(&mut self, t: i64, cc_type: u8, b1: u8, b2: u8) {
        if cc_type == 3 {
            self.pkt.clear();
            let code = (b1 & 0x3F) as usize;
            self.need = if code == 0 { 128 } else { code * 2 };
        } else if cc_type != 2 || self.pkt.is_empty() {
            return;
        }
        self.pkt.extend_from_slice(&[b1, b2]);
        if self.pkt.len() >= self.need {
            let pkt = std::mem::take(&mut self.pkt);
            self.process(&pkt[1..self.need], t);
        }
    }

    fn process(&mut self, d: &[u8], t: i64) {
        let mut i = 0;
        while i < d.len() {
            let h = d[i];
            i += 1;
            let mut svc = h >> 5;
            let size = (h & 0x1F) as usize;
            if svc == 0 {
                break;
            }
            if svc == 7 {
                let Some(&e) = d.get(i) else { break };
                svc = e & 0x3F;
                i += 1;
            }
            let Some(block) = d.get(i..i + size) else {
                break;
            };
            i += size;
            if svc == 1 {
                self.svc.feed(block, t);
            } else {
                self.unsupported.insert(svc);
            }
        }
    }

    /// Close the display at `end_us` and return the service 1 cues.
    pub fn finish(&mut self, end_us: i64) -> Vec<CaptionCue> {
        self.svc.commit(end_us);
        std::mem::take(&mut self.svc.cues)
    }
}
