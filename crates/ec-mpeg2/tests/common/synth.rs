//! A random-syntax MPEG-2 video stream writer for differential testing.
//!
//! It emits *valid* streams that exercise every picture/macroblock tool of the
//! decoder (field pictures, dual prime, 16x8 MC, concealment vectors, both
//! intra VLC tables, alternate scan, ...), so a reference decoder's output on
//! them can be compared byte for byte. It is a test oracle driver, not an
//! encoder: the content is noise.
#![allow(dead_code, unused)]

#[path = "../../src/tables.rs"]
mod tables;
use tables::*;

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: u32) -> u32 {
        ((self.next() >> 33) % n as u64) as u32
    }
    pub fn chance(&mut self, pct: u32) -> bool {
        self.below(100) < pct
    }
    pub fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u32) as i32
    }
}

#[derive(Default)]
pub struct BitWriter {
    pub buf: Vec<u8>,
    cur: u64,
    n: u32,
}
impl BitWriter {
    pub fn put(&mut self, v: u32, len: u32) {
        assert!(len <= 32);
        if len == 0 {
            return;
        }
        self.cur = (self.cur << len) | (v as u64 & ((1u64 << len) - 1));
        self.n += len;
        while self.n >= 8 {
            self.buf.push((self.cur >> (self.n - 8)) as u8);
            self.n -= 8;
        }
        self.cur &= (1u64 << self.n) - 1;
    }
    pub fn align(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put(0, pad);
        }
    }
    pub fn start_code(&mut self, code: u8) {
        self.align();
        self.buf.extend_from_slice(&[0, 0, 1, code]);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pt {
    I,
    P,
    B,
}

#[derive(Clone, Debug)]
pub struct SynthCfg {
    pub mb_w: usize,
    /// Frame macroblock rows (even when interlaced).
    pub mb_h: usize,
    pub chroma422: bool,
    pub progressive_sequence: bool,
    pub frames: usize,
    pub seed: u64,
    /// Allow field pictures.
    pub fields: bool,
    /// Allow dual prime and 16x8 (only meaningful with fields / !frame_pred).
    pub heavy: bool,
    pub load_matrices: bool,
}

struct PicParams {
    pt: Pt,
    structure: u32, // 1 top, 2 bottom, 3 frame
    dc_prec: u32,
    tff: bool,
    frame_pred_frame_dct: bool,
    conceal: bool,
    q_scale_type: bool,
    intra_vlc: bool,
    alt_scan: bool,
    rff: bool,
    progressive_frame: bool,
    f_code: [[u32; 2]; 2],
}

#[derive(Clone, Copy, Default)]
struct Mot {
    fwd: bool,
    bwd: bool,
}

struct Synth {
    no_prior: bool,
    rng: Rng,
    w: BitWriter,
    cfg: SynthCfg,
    intra_map: std::collections::HashMap<(u8, u8), (u16, u8)>,
    intra_map1: std::collections::HashMap<(u8, u8), (u16, u8)>,
    qm_intra: [u8; 64],
    qm_non: [u8; 64],
    qm_cintra: [u8; 64],
    qm_cnon: [u8; 64],
}

fn bitlen(v: u32) -> u32 {
    32 - v.leading_zeros()
}

pub fn generate(cfg: &SynthCfg) -> Vec<u8> {
    let mut s = Synth {
        no_prior: false,
        rng: Rng(cfg.seed.wrapping_mul(0x9E3779B97F4A7C15) | 1),
        w: BitWriter::default(),
        cfg: cfg.clone(),
        intra_map: B14.iter().map(|r| ((r.0, r.1), (r.2, r.3))).collect(),
        intra_map1: B15.iter().map(|r| ((r.0, r.1), (r.2, r.3))).collect(),
        qm_intra: [0; 64],
        qm_non: [16; 64],
        qm_cintra: [0; 64],
        qm_cnon: [16; 64],
    };
    for i in 0..4 {
        s.rng.next();
    }
    s.run();
    s.w.buf
}

impl Synth {
    fn run(&mut self) {
        let cfg = self.cfg.clone();
        let (w, h) = (cfg.mb_w * 16, cfg.mb_h * 16);
        self.w.start_code(0xB3);
        self.w.put(w as u32, 12);
        self.w.put(h as u32, 12);
        self.w.put(1, 4); // aspect
        self.w.put(3, 4); // 25 fps
        self.w.put(0x3FFFF, 18);
        self.w.put(1, 1);
        self.w.put(112, 10);
        self.w.put(0, 1);
        // default intra matrix in raster order for dequant bound computation
        for i in 0..64 {
            self.qm_intra[ZIGZAG[i] as usize] = DEFAULT_INTRA[i];
        }
        self.qm_cintra = self.qm_intra;
        if cfg.load_matrices && self.rng.chance(60) {
            self.w.put(1, 1);
            for i in 0..64 {
                let v = if i == 0 { 8 } else { self.rng.range(8, 64) as u8 };
                self.w.put(v as u32, 8);
                self.qm_intra[ZIGZAG[i] as usize] = v;
            }
            self.qm_cintra = self.qm_intra;
        } else {
            self.w.put(0, 1);
        }
        if cfg.load_matrices && self.rng.chance(60) {
            self.w.put(1, 1);
            for i in 0..64 {
                let v = self.rng.range(8, 64) as u8;
                self.w.put(v as u32, 8);
                self.qm_non[ZIGZAG[i] as usize] = v;
            }
            self.qm_cnon = self.qm_non;
        } else {
            self.w.put(0, 1);
        }
        // sequence extension
        self.w.start_code(0xB5);
        self.w.put(1, 4);
        self.w.put(0x48, 8);
        self.w.put(cfg.progressive_sequence as u32, 1);
        self.w.put(if cfg.chroma422 { 2 } else { 1 }, 2);
        self.w.put(0, 2);
        self.w.put(0, 2);
        self.w.put(0, 12);
        self.w.put(1, 1);
        self.w.put(0, 8);
        self.w.put(0, 1);
        self.w.put(0, 2);
        self.w.put(0, 5);
        // GOP
        self.w.start_code(0xB8);
        self.w.put(0, 25);
        self.w.put(1, 1);
        self.w.put(0, 1);
        self.w.put(0, 1); // pad to byte? (25+1+1 = 27 bits)
        self.w.align();

        let mut have_refs = 0; // number of reference frames so far
        let mut tr = 0u32;
        let mut n_coded = 0;
        while n_coded < cfg.frames {
            // choose frame type in coded order
            let pt = if n_coded == 0 {
                Pt::I
            } else if have_refs < 2 {
                if self.rng.chance(15) { Pt::I } else { Pt::P }
            } else {
                match self.rng.below(10) {
                    0 => Pt::I,
                    1..=4 => Pt::P,
                    _ => Pt::B,
                }
            };
            let as_fields = cfg.fields && !cfg.progressive_sequence && self.rng.chance(50);
            let tff = self.rng.chance(50);
            let rff = self.rng.chance(20);
            let pf = if cfg.progressive_sequence { true } else { self.rng.chance(30) };
            self.no_prior = have_refs == 0;
            let pf = pf && !as_fields;
            let rff = rff && !as_fields;
            if as_fields {
                let first = if tff { 1 } else { 2 };
                let second = 3 - first;
                self.picture(pt, first, tff, rff, pf, tr, false);
                // second field: I frames may carry a P second field
                let pt2 = if pt == Pt::I && self.rng.chance(60) { Pt::P } else { pt };
                self.picture(pt2, second, tff, rff, pf, tr, true);
            } else {
                self.picture(pt, 3, tff, rff, pf, tr, false);
            }
            if pt != Pt::B {
                have_refs += 1;
            }
            tr += 1;
            n_coded += 1;
        }
        self.w.start_code(0xB7);
        self.w.align();
    }

    fn picture(&mut self, pt: Pt, structure: u32, tff: bool, rff: bool, pf: bool, tr: u32, second: bool) {
        let cfg = self.cfg.clone();
        let frame = structure == 3;
        let fpfd = if cfg.progressive_sequence { true } else if frame { self.rng.chance(40) } else { false };
        let p = PicParams {
            pt,
            structure,
            dc_prec: self.rng.below(4),
            tff,
            frame_pred_frame_dct: fpfd,
            conceal: self.rng.chance(20),
            q_scale_type: self.rng.chance(40),
            intra_vlc: self.rng.chance(50),
            alt_scan: self.rng.chance(40),
            rff,
            progressive_frame: pf,
            f_code: [
                [self.rng.range(1, 3) as u32, self.rng.range(1, 3) as u32],
                [self.rng.range(1, 3) as u32, self.rng.range(1, 3) as u32],
            ],
        };
        self.w.start_code(0x00);
        self.w.put(tr & 0x3FF, 10);
        self.w.put(match pt { Pt::I => 1, Pt::P => 2, Pt::B => 3 }, 3);
        self.w.put(0xFFFF, 16);
        if pt != Pt::I {
            self.w.put(0, 1);
            self.w.put(7, 3);
            if pt == Pt::B {
                self.w.put(0, 1);
                self.w.put(7, 3);
            }
        }
        self.w.align();
        // picture coding extension
        self.w.start_code(0xB5);
        self.w.put(8, 4);
        for s in 0..2 {
            for t in 0..2 {
                let used = match pt {
                    Pt::I => p.conceal && s == 0,
                    Pt::P => s == 0,
                    Pt::B => true,
                };
                self.w.put(if used { p.f_code[s][t] } else { 15 }, 4);
            }
        }
        self.w.put(p.dc_prec, 2);
        self.w.put(p.structure, 2);
        self.w.put(p.tff as u32, 1);
        self.w.put(p.frame_pred_frame_dct as u32, 1);
        self.w.put(p.conceal as u32, 1);
        self.w.put(p.q_scale_type as u32, 1);
        self.w.put(p.intra_vlc as u32, 1);
        self.w.put(p.alt_scan as u32, 1);
        self.w.put(p.rff as u32, 1);
        self.w.put(1, 1); // chroma_420_type
        self.w.put(p.progressive_frame as u32, 1);
        self.w.put(0, 1); // composite_display_flag
        self.w.align();
        let rows = if frame { cfg.mb_h } else { cfg.mb_h / 2 };
        for row in 0..rows {
            let mut col = 0;
            while col < cfg.mb_w {
                let end = if false && col + 1 < cfg.mb_w {
                    col + 1 + self.rng.below((cfg.mb_w - col - 1) as u32) as usize
                } else {
                    cfg.mb_w
                };
                self.slice(&p, row, col, end);
                col = end;
            }
        }
    }

    fn qscale(&self, p: &PicParams, code: u32) -> i32 {
        if p.q_scale_type { NONLINEAR_QSCALE[code as usize] as i32 } else { code as i32 * 2 }
    }

    fn slice(&mut self, p: &PicParams, row: usize, c0: usize, c1: usize) {
        let cfg = self.cfg.clone();
        self.w.start_code((row + 1) as u8);
        let mut qcode = self.rng.range(1, 31) as u32;
        // keep quantiser scale small enough that coefficients stay in range easily
        self.w.put(qcode, 5);
        self.w.put(0, 1); // extra_bit_slice
        let dc_reset = 1i32 << (7 + p.dc_prec);
        let mut dc_pred = [dc_reset; 3];
        let mut pmv = [[[0i32; 2]; 2]; 2];
        let mut last_intra = true;
        let mut prev_fb = Mot { fwd: true, bwd: false };
        let mut skip_run = 0u32;
        let field_pic = p.structure != 3;
        let mut col = c0;
        let nblocks = if cfg.chroma422 { 8 } else { 6 };
        while col < c1 {
            let first = col == c0;
            let last = col == c1 - 1;
            // Decide skip.
            let can_skip = !first && !last && p.pt != Pt::I && !(p.pt == Pt::B && last_intra);
            let skip_inside = p.pt != Pt::B || {
                let (mw, mh) = (self.cfg.mb_w as i32, self.cfg.mb_h as i32);
                let eh = if field_pic { mh * 8 } else { mh * 16 };
                (0..2).all(|s| {
                    let used = if s == 0 { prev_fb.fwd } else { prev_fb.bwd };
                    !used || {
                        let (mx, my) = (pmv[0][s][0], pmv[0][s][1]);
                        let x0 = col as i32 * 16 * 2 + mx;
                        let y0 = row as i32 * 16 * 2 + my;
                        x0 >= 4 && x0 <= (mw * 16 - 16) * 2 - 4 && y0 >= 4 && y0 <= (eh - 16) * 2 - 4
                    }
                })
            };
            if can_skip && skip_inside && self.rng.chance(25) {
                skip_run += 1;
                // Skipped macroblock effects
                dc_pred = [dc_reset; 3];
                if p.pt == Pt::P {
                    pmv = [[[0; 2]; 2]; 2];
                    prev_fb = Mot { fwd: true, bwd: false };
                }
                last_intra = false;
                col += 1;
                continue;
            }
            // macroblock_address_increment
            let mut inc = skip_run + 1 + if first { c0 as u32 } else { 0 };
            skip_run = 0;
            while inc > 33 {
                self.put_mba(34);
                inc -= 33;
            }
            self.put_mba(inc);
            // Choose type.
            let (flags, rows): (u16, &[(u16, u8, u16)]) = match p.pt {
                Pt::I => {
                    let f = if self.rng.chance(30) { MB_INTRA | MB_QUANT } else { MB_INTRA };
                    (f, MBTYPE_I)
                }
                Pt::P => {
                    let opts = [MB_FWD | MB_PAT, MB_PAT, MB_FWD, MB_INTRA, MB_FWD | MB_PAT | MB_QUANT, MB_PAT | MB_QUANT, MB_INTRA | MB_QUANT];
                    let w = [30, 8, 10, 8, 20, 4, 4];
                    (self.pick(&opts, &w), MBTYPE_P)
                }
                Pt::B => {
                    let opts = [MB_FWD | MB_BWD, MB_FWD | MB_BWD | MB_PAT, MB_BWD, MB_BWD | MB_PAT, MB_FWD, MB_FWD | MB_PAT, MB_INTRA, MB_FWD | MB_BWD | MB_PAT | MB_QUANT, MB_FWD | MB_PAT | MB_QUANT, MB_BWD | MB_PAT | MB_QUANT, MB_INTRA | MB_QUANT];
                    let w = [10, 15, 6, 8, 6, 8, 5, 10, 6, 6, 3];
                    (self.pick(&opts, &w), MBTYPE_B)
                }
            };
            let (code, len, _) = *rows.iter().find(|r| r.2 == flags).unwrap();
            self.w.put(code as u32, len as u32);
            let intra = flags & MB_INTRA != 0;
            let fwd = flags & MB_FWD != 0;
            let bwd = flags & MB_BWD != 0;
            // motion type
            let mut kind = 0u32; // frame: 1 field, 2 frame, 3 dual; field: 1 field, 2 16x8, 3 dual
            if fwd || bwd {
                if !(p.structure == 3 && p.frame_pred_frame_dct) {
                    let mut opts = vec![if p.structure == 3 { 2 } else { 1 }];
                    opts.push(if p.structure == 3 { 1 } else { 2 });
                    if cfg.heavy && std::env::var_os("EC_SYNTH_NODUAL").is_none() && p.pt == Pt::P && fwd && !bwd && !(self.no_prior && p.structure != 3) && col > 0 && col + 1 < cfg.mb_w && row > 0 && row + 1 < (if field_pic { cfg.mb_h / 2 } else { cfg.mb_h }) {
                        opts.push(3);
                    }
                    kind = opts[self.rng.below(opts.len() as u32) as usize];
                    if cfg.heavy {
                        self.w.put(kind, 2);
                    } else {
                        // keep it simple: only the first two kinds
                        kind = opts[0];
                        self.w.put(kind, 2);
                    }
                } else {
                    kind = 2;
                }
            }
            let mut dct_field = false;
            if p.structure == 3 && !p.frame_pred_frame_dct && (intra || flags & MB_PAT != 0) {
                dct_field = self.rng.chance(50);
                self.w.put(dct_field as u32, 1);
            }
            if flags & MB_QUANT != 0 {
                qcode = self.rng.range(1, 31) as u32;
                self.w.put(qcode, 5);
            }
            // motion vectors
            let eff_kind = if intra { if field_pic { 1 } else { 2 } } else { kind };
            if fwd || (intra && p.conceal) {
                self.write_mvs(p, 0, eff_kind, &mut pmv, field_pic, (col, row));
            }
            if bwd {
                self.write_mvs(p, 1, eff_kind, &mut pmv, field_pic, (col, row));
            }
            if intra && p.conceal {
                self.w.put(1, 1);
            }
            if intra {
                if !p.conceal {
                    pmv = [[[0; 2]; 2]; 2];
                }
            } else {
                if p.pt == Pt::P && !fwd {
                    pmv = [[[0; 2]; 2]; 2];
                }
                dc_pred = [dc_reset; 3];
            }
            // cbp
            let mut cbp = 0u32;
            if intra {
                cbp = (1 << nblocks) - 1;
            } else if flags & MB_PAT != 0 {
                loop {
                    cbp = self.rng.below(1 << nblocks);
                    if (if cfg.chroma422 { cbp >> 2 } else { cbp }) != 0 {
                        break;
                    }
                }
                let six = if cfg.chroma422 { cbp >> 2 } else { cbp };
                let (c, l, _) = *CBP.iter().find(|r| r.2 as u32 == six).unwrap();
                self.w.put(c as u32, l as u32);
                if cfg.chroma422 {
                    self.w.put(cbp & 3, 2);
                }
            }
            let qs = self.qscale(p, qcode);
            for i in 0..nblocks {
                if cbp & (1 << (nblocks - 1 - i)) == 0 {
                    continue;
                }
                if intra {
                    self.write_intra_block(p, i, qs, &mut dc_pred);
                } else {
                    self.write_inter_block(p, i, qs);
                }
            }
            if !intra
                && p.pt == Pt::B {
                    prev_fb = Mot { fwd, bwd };
                }
            last_intra = intra;
            col += 1;
        }
    }

    fn pick(&mut self, opts: &[u16], w: &[u32]) -> u16 {
        let total: u32 = w.iter().sum();
        let mut r = self.rng.below(total);
        for (o, wt) in opts.iter().zip(w) {
            if r < *wt {
                return *o;
            }
            r -= wt;
        }
        opts[0]
    }

    fn put_mba(&mut self, v: u32) {
        let r = MBA.iter().find(|r| r.2 as u32 == if v == 34 { 34 } else { v }).unwrap();
        self.w.put(r.0 as u32, r.1 as u32);
    }

    fn write_mvs(&mut self, p: &PicParams, s: usize, kind: u32, pmv: &mut [[[i32; 2]; 2]; 2], field_pic: bool, mbpos: (usize, usize)) {
        let frame_pic = !field_pic;
        // kind semantic: frame pic: 1 field, 2 frame, 3 dual; field pic: 1 field16, 2 16x8, 3 dual
        let dual = kind == 3;
        let n = if (frame_pic && kind == 1) || (field_pic && kind == 2) { 2 } else { 1 };
        let field_fmt = field_pic || kind != 2;
        let frame_field = frame_pic && field_fmt;
        for r in 0..n {
            if field_fmt && !dual {
                let sel = if self.no_prior && field_pic { (p.structure == 1) as u32 } else { self.rng.below(2) };
                self.w.put(sel, 1);
            }
            for t in 0..2 {
                let f_code = p.f_code[s][t];
                let f = 1i32 << (f_code - 1);
                let (lo, hi, range) = (-16 * f, 16 * f - 1, 32 * f);
                let mut pred = pmv[r][s][t];
                if t == 1 && frame_field {
                    pred >>= 1;
                }
                // target within a modest range
                // keep the predicted block inside the picture (ffmpeg skips MC otherwise)
                let (mbx, mby) = (mbpos.0 as i32, mbpos.1 as i32);
                let (mw, mh) = (self.cfg.mb_w as i32, self.cfg.mb_h as i32);
                let (p0, ext, blk) = if t == 0 {
                    (mbx * 16, mw * 16, 16)
                } else if field_pic {
                    (mby * 16, mh * 8, 16)
                } else if kind == 1 || kind == 3 {
                    (mby * 8, mh * 8, 8)
                } else {
                    (mby * 16, mh * 16, 16)
                };
                let mut blo = (-p0 * 2 + 4).max(lo);
                let mut bhi = ((ext - blk - p0) * 2 - 4).min(hi);
                if dual {
                    blo = ((blo - 4) * 2 + 8 * 2) / 3 + 1;
                    bhi = ((bhi + 4) * 2 - 8 * 2) / 3 - 1;
                }
                if blo > bhi {
                    blo = 0;
                    bhi = 0;
                }
                let lim = bhi.min(40);
                let llo = blo.max(-40);
                let target = if self.rng.chance(15) { self.rng.range(blo, bhi) } else { self.rng.range(llo, lim) };
                let mut d = target - pred;
                if d > hi {
                    d -= range;
                }
                if d < lo {
                    d += range;
                }
                self.put_motion(f_code, d);
                pmv[r][s][t] = if t == 1 && frame_field { target << 1 } else { target };
                if dual {
                    let dm = self.rng.range(-1, 1);
                    match dm {
                        0 => self.w.put(0, 1),
                        1 => self.w.put(0b10, 2),
                        _ => self.w.put(0b11, 2),
                    }
                }
            }
        }
        if n == 1 {
            pmv[1][s] = pmv[0][s];
        }
    }

    fn put_motion(&mut self, f_code: u32, d: i32) {
        let r_size = f_code - 1;
        let f = 1i32 << r_size;
        if d == 0 {
            let (c, l) = MOTION[0];
            self.w.put(c as u32, l as u32);
            return;
        }
        let mag = d.abs();
        let code = (mag - 1) / f + 1;
        let res = (mag - 1) % f;
        let (c, l) = MOTION[code as usize];
        self.w.put(c as u32, l as u32);
        self.w.put((d < 0) as u32, 1);
        if r_size > 0 {
            self.w.put(res as u32, r_size);
        }
    }

    fn write_intra_block(&mut self, p: &PicParams, blk: usize, qs: i32, dc_pred: &mut [i32; 3]) {
        let comp = if blk < 4 { 0 } else { 1 + (blk & 1) };
        let max_dc = (256i32 << p.dc_prec) - 1;
        let target = if self.rng.chance(20) {
            self.rng.range(0, max_dc)
        } else {
            (dc_pred[comp] + self.rng.range(-40, 40)).clamp(0, max_dc)
        };
        let diff = target - dc_pred[comp];
        dc_pred[comp] = target;
        let size = bitlen(diff.unsigned_abs());
        let (c, l) = if comp == 0 { DC_LUMA[size as usize] } else { DC_CHROMA[size as usize] };
        self.w.put(c as u32, l as u32);
        if size > 0 {
            let v = if diff > 0 { diff } else { diff + (1 << size) - 1 };
            self.w.put(v as u32, size);
        }
        let chroma = blk >= 4;
        let m = if chroma { self.qm_cintra } else { self.qm_intra };
        self.write_coefs(p, qs, &m, true, 1);
    }

    fn write_inter_block(&mut self, p: &PicParams, blk: usize, qs: i32) {
        let chroma = blk >= 4;
        let m = if chroma { self.qm_cnon } else { self.qm_non };
        self.write_coefs(p, qs, &m, false, 0);
    }

    fn write_coefs(&mut self, p: &PicParams, qs: i32, m: &[u8; 64], intra: bool, start: usize) {
        let scan: &[u8; 64] = if p.alt_scan { &ALT_SCAN } else { &ZIGZAG };
        let n = if intra { self.rng.below(7) } else { 1 + self.rng.below(7) } as usize;
        let mut pos = start;
        let table = if intra && p.intra_vlc { &self.intra_map1 } else { &self.intra_map };
        let table = table.clone();
        let mut first = true;
        for _ in 0..n {
            let run = if self.rng.chance(60) { 0 } else { self.rng.below(12) as usize };
            let ipos = pos + run;
            if ipos > 63 {
                break;
            }
            let j = scan[ipos] as usize;
            let wgt = m[j] as i32;
            let lmax = if intra { (150 * 16) / (qs * wgt) } else { ((150 * 32) / (qs * wgt) - 1) / 2 };
            let lmax = lmax.clamp(0, 255);
            if lmax < 1 {
                // cannot code anything here without leaving the legal range
                pos = ipos + 1;
                continue;
            }
            let mag = if self.rng.chance(85) { 1 + self.rng.below(3.min(lmax as u32)) as i32 } else { 1 + self.rng.below(lmax as u32) as i32 }.min(lmax);
            let neg = self.rng.chance(50);
            let sign = neg as u32;
            if !intra && first && run == 0 && mag == 1 {
                self.w.put(1, 1);
                self.w.put(sign, 1);
            } else if let Some(&(c, l)) = table.get(&(run as u8, mag as u8)) {
                self.w.put(c as u32, l as u32);
                self.w.put(sign, 1);
            } else {
                self.w.put(ESCAPE.0 as u32, ESCAPE.1 as u32);
                self.w.put(run as u32, 6);
                let lv = if neg { -mag } else { mag };
                self.w.put((lv & 0xFFF) as u32, 12);
            }
            first = false;
            pos = ipos + 1;
        }
        if !intra && first {
            // a coded non-intra block needs at least one coefficient
            self.w.put(1, 1);
            self.w.put(0, 1);
        }
        let (c, l) = if intra && p.intra_vlc { EOB_B15 } else { EOB_B14 };
        self.w.put(c as u32, l as u32);
    }
}
