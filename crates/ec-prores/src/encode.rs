//! ProRes encoder (RDD 36 bitstream, progressive or interlaced, optional alpha): forward DCT, per-slice rate control,
//! entropy coding, slice-parallel.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use wide::f32x8;

use ec_core::error::{Error, Result};
use ec_core::frame::{ColorInfo, Frame, Interlace, PixelFormat, VideoFrame};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, Encoder, MediaParameters};

use crate::frame::AlphaPlane;
use crate::header::MAX_DIM;
use crate::tables::*;

/// ProRes profile (the sample-entry fourcc follows it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// 422 Proxy (`apco`).
    Proxy,
    /// 422 LT (`apcs`).
    Lt,
    /// 422 (`apcn`).
    Standard,
    /// 422 HQ (`apch`).
    Hq,
    /// 4444 (`ap4h`), no alpha.
    P4444,
    /// 4444 XQ (`ap4x`), no alpha.
    P4444Xq,
}

const QM_PROXY_LUMA: [u8; 64] = [
    4, 7, 9, 11, 13, 14, 15, 63, 7, 7, 11, 12, 14, 15, 63, 63, 9, 11, 13, 14, 15, 63, 63, 63, 11,
    11, 13, 14, 63, 63, 63, 63, 11, 13, 14, 63, 63, 63, 63, 63, 13, 14, 63, 63, 63, 63, 63, 63, 13,
    63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63,
];
const QM_PROXY_CHROMA: [u8; 64] = [
    4, 7, 9, 11, 13, 14, 63, 63, 7, 7, 11, 12, 14, 63, 63, 63, 9, 11, 13, 14, 63, 63, 63, 63, 11,
    11, 13, 14, 63, 63, 63, 63, 11, 13, 14, 63, 63, 63, 63, 63, 13, 14, 63, 63, 63, 63, 63, 63, 13,
    63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63,
];
const QM_LT: [u8; 64] = [
    4, 5, 6, 7, 9, 11, 13, 15, 5, 5, 7, 8, 11, 13, 15, 17, 6, 7, 9, 11, 13, 15, 15, 17, 7, 7, 9,
    11, 13, 15, 17, 19, 7, 9, 11, 13, 14, 16, 19, 23, 9, 11, 13, 14, 16, 19, 23, 29, 9, 11, 13,
    15, 17, 21, 28, 35, 11, 13, 16, 17, 21, 28, 35, 41,
];
const QM_STD: [u8; 64] = [
    4, 4, 5, 5, 6, 7, 7, 9, 4, 4, 5, 6, 7, 7, 9, 9, 5, 5, 6, 7, 7, 9, 9, 10, 5, 5, 6, 7, 7, 9, 9,
    10, 5, 6, 7, 7, 8, 9, 10, 12, 6, 7, 7, 8, 9, 10, 12, 15, 6, 7, 7, 9, 10, 11, 14, 17, 7, 7, 9,
    10, 11, 14, 17, 21,
];
const QM_HQ: [u8; 64] = [
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    5, 4, 4, 4, 4, 4, 4, 5, 5, 4, 4, 4, 4, 4, 5, 5, 6, 4, 4, 4, 4, 5, 5, 6, 7, 4, 4, 4, 4, 5, 6,
    7, 7,
];

impl Profile {
    /// The QuickTime / MP4 sample-entry fourcc.
    pub fn fourcc(self) -> [u8; 4] {
        match self {
            Profile::Proxy => *b"apco",
            Profile::Lt => *b"apcs",
            Profile::Standard => *b"apcn",
            Profile::Hq => *b"apch",
            Profile::P4444 => *b"ap4h",
            Profile::P4444Xq => *b"ap4x",
        }
    }

    /// The profile a sample-entry fourcc names.
    pub fn from_fourcc(f: [u8; 4]) -> Option<Profile> {
        [Profile::Proxy, Profile::Lt, Profile::Standard, Profile::Hq, Profile::P4444, Profile::P4444Xq]
            .into_iter()
            .find(|p| p.fourcc() == f)
    }

    /// True for the 4:4:4 profiles.
    pub fn is_444(self) -> bool {
        matches!(self, Profile::P4444 | Profile::P4444Xq)
    }

    /// Apple's nominal frame size at 1920x1080 (29.97 fps), in bytes.
    fn nominal_1080(self) -> f64 {
        let mbps = match self {
            Profile::Proxy => 45.0,
            Profile::Lt => 102.0,
            Profile::Standard => 147.0,
            Profile::Hq => 220.0,
            Profile::P4444 => 330.0,
            Profile::P4444Xq => 500.0,
        };
        mbps * 1e6 / 29.97 / 8.0
    }

    fn qmats(self) -> (&'static [u8; 64], &'static [u8; 64]) {
        match self {
            Profile::Proxy => (&QM_PROXY_LUMA, &QM_PROXY_CHROMA),
            Profile::Lt => (&QM_LT, &QM_LT),
            Profile::Standard => (&QM_STD, &QM_STD),
            _ => (&QM_HQ, &QM_HQ),
        }
    }

    /// A starting slice quantiser for the rate search.
    fn start_q(self) -> usize {
        match self {
            Profile::Proxy => 12,
            Profile::Lt => 8,
            Profile::Standard => 5,
            _ => 3,
        }
    }
}

/// Apple's nominal size of one frame of `mbs` macroblocks (16x16 luma blocks), in bytes: the
/// 1080p29.97 rate scaled by `(mbs / 8160) ^ 0.846`, which lands on Apple's UHD figures (3.21x
/// for 4x the area) and on the SD ones.
pub fn nominal_frame_bytes(profile: Profile, mbs: usize) -> usize {
    (profile.nominal_1080() * (mbs as f64 / 8160.0).powf(0.846)) as usize
}

/// Encoder settings.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    /// Profile to produce.
    pub profile: Profile,
    /// Frame size in pixels.
    pub width: u32,
    /// Frame size in pixels.
    pub height: u32,
    /// Colour tags written in the frame header (and the stream parameters).
    pub color: ColorInfo,
    /// Worker threads, 1..=6 (default: available cores, at most 6).
    pub threads: usize,
    /// Target bits per macroblock; `None` = Apple's nominal rate for the profile and size.
    pub bits_per_mb: Option<u32>,
    /// Fixed slice quantiser index (1..=224) instead of rate control.
    pub qscale: Option<u8>,
    /// Four-byte encoder id written in the frame header.
    pub creator: [u8; 4],
    /// Coded alpha depth for the 4444 profiles: 0 = none, 8 or 16 bits. See
    /// [`ProResEncoder::encode_with_alpha`].
    pub alpha_bits: u8,
}

impl EncoderConfig {
    /// Defaults for `profile` at `width` x `height`.
    pub fn new(profile: Profile, width: u32, height: u32) -> EncoderConfig {
        EncoderConfig {
            profile,
            width,
            height,
            color: ColorInfo::default(),
            threads: std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 6),
            bits_per_mb: None,
            qscale: None,
            creator: *b"edth",
            alpha_bits: 0,
        }
    }
}

// --- bit sinks -------------------------------------------------------------------------

trait Sink {
    fn cw(&mut self, cb: u8, v: u32);
    fn coef(&mut self, run_ctx: usize, lvl_ctx: usize, zeros: u32, a: u32, neg: bool);
}

#[inline(always)]
fn cw_bits(cb: u8, v: u32) -> u32 {
    let sw = u32::from(cb & 3) + 1;
    let rice = u32::from(cb >> 5);
    let exp = u32::from((cb >> 2) & 7);
    if v < (sw << rice) {
        (v >> rice) + 1 + rice
    } else {
        let vv = v - (sw << rice) + (1 << exp);
        let nb = 31 - vv.leading_zeros();
        2 * nb + sw - exp + 1
    }
}

/// `(code, length)` of codeword `v` when it fits 32 bits.
fn cw_code(cb: u8, v: u32) -> (u32, u32) {
    let sw = u32::from(cb & 3) + 1;
    let rice = u32::from(cb >> 5);
    let exp = u32::from((cb >> 2) & 7);
    if v < (sw << rice) {
        let q = v >> rice;
        ((1 << rice) | (v & ((1 << rice) - 1)), q + 1 + rice)
    } else {
        let vv = v - (sw << rice) + (1 << exp);
        let nb = 31 - vv.leading_zeros();
        (vv, 2 * nb + sw - exp + 1)
    }
}

const TAB: usize = 64;

/// Code / length lookup for the small run and level values.
struct Tabs {
    run: [[(u32, u8); TAB]; 16],
    lvl: [[(u32, u8); TAB]; 10],
}

fn tabs() -> &'static Tabs {
    static T: OnceLock<Tabs> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = Tabs { run: [[(0, 0); TAB]; 16], lvl: [[(0, 0); TAB]; 10] };
        for (i, &cb) in RUN_CB.iter().enumerate() {
            for v in 0..TAB {
                let (c, l) = cw_code(cb, v as u32);
                t.run[i][v] = (c, l as u8);
            }
        }
        for (i, &cb) in LEVEL_CB.iter().enumerate() {
            for v in 0..TAB {
                let (c, l) = cw_code(cb, v as u32);
                t.lvl[i][v] = (c, l as u8);
            }
        }
        t
    })
}

struct Counter {
    bits: u32,
    t: &'static Tabs,
}
impl Sink for Counter {
    #[inline(always)]
    fn cw(&mut self, cb: u8, v: u32) {
        self.bits += cw_bits(cb, v);
    }
    #[inline(always)]
    fn coef(&mut self, run_ctx: usize, lvl_ctx: usize, zeros: u32, a: u32, _neg: bool) {
        let (rc, lc) = (run_ctx.min(15), lvl_ctx.min(9));
        let r = if (zeros as usize) < TAB { u32::from(self.t.run[rc][zeros as usize].1) } else { cw_bits(RUN_CB[rc], zeros) };
        let l = if ((a - 1) as usize) < TAB { u32::from(self.t.lvl[lc][(a - 1) as usize].1) } else { cw_bits(LEVEL_CB[lc], a - 1) };
        self.bits += r + l + 1;
    }
}

struct BitW {
    out: Vec<u8>,
    acc: u64,
    n: u32,
    t: &'static Tabs,
}
impl BitW {
    fn new() -> BitW {
        BitW { out: Vec::with_capacity(4096), acc: 0, n: 0, t: tabs() }
    }
    /// `n <= 32` bits of `v`.
    #[inline(always)]
    fn put(&mut self, v: u32, n: u32) {
        debug_assert!(n <= 32);
        let mask = if n >= 32 { u32::MAX } else { (1u32 << n) - 1 };
        self.acc = (self.acc << n) | u64::from(v & mask);
        self.n += n;
        if self.n >= 32 {
            self.n -= 32;
            self.out.extend_from_slice(&((self.acc >> self.n) as u32).to_be_bytes());
        }
    }
    fn finish(mut self) -> Vec<u8> {
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
        if self.n > 0 {
            self.out.push(((self.acc << (8 - self.n)) & 0xff) as u8);
        }
        self.out
    }
}
impl Sink for BitW {
    #[inline(always)]
    fn cw(&mut self, cb: u8, v: u32) {
        let sw = u32::from(cb & 3) + 1;
        let rice = u32::from(cb >> 5);
        let exp = u32::from((cb >> 2) & 7);
        if v < (sw << rice) {
            let q = v >> rice;
            self.put(0, q);
            self.put((1 << rice) | (v & ((1 << rice) - 1)), rice + 1);
        } else {
            let vv = v - (sw << rice) + (1 << exp);
            let nb = 31 - vv.leading_zeros();
            self.put(0, nb + sw - exp);
            self.put(vv, nb + 1); // the leading one of vv is the stop bit
        }
    }
    #[inline(always)]
    fn coef(&mut self, run_ctx: usize, lvl_ctx: usize, zeros: u32, a: u32, neg: bool) {
        let (rc, lc) = (run_ctx.min(15), lvl_ctx.min(9));
        if (zeros as usize) < TAB && ((a - 1) as usize) < TAB {
            let (rcode, rl) = self.t.run[rc][zeros as usize];
            let (lcode, ll) = self.t.lvl[lc][(a - 1) as usize];
            let (rl, ll) = (u32::from(rl), u32::from(ll));
            if rl + ll < 32 {
                self.put((rcode << (ll + 1)) | (lcode << 1) | u32::from(neg), rl + ll + 1);
                return;
            }
        }
        self.cw(RUN_CB[rc], zeros);
        self.cw(LEVEL_CB[lc], a - 1);
        self.put(u32::from(neg), 1);
    }
}

fn code_component<S: Sink>(s: &mut S, q: &[i16], nb: usize) {
    // DC.
    let first = i32::from(q[0]);
    s.cw(FIRST_DC_CB, ((first << 1) ^ (first >> 31)) as u32);
    let mut prev = first;
    let mut prev_neg = false;
    let mut cbi = 5usize;
    for b in 1..nb {
        let cur = i32::from(q[b]);
        let d = cur - prev;
        let neg = d < 0;
        let code = if d == 0 { 0 } else { 2 * d.unsigned_abs() - u32::from(neg != prev_neg) };
        s.cw(DC_CB[cbi], code);
        cbi = (code as usize).min(6);
        prev_neg = neg;
        prev = cur;
    }
    // AC, scan-index major, block minor.
    let mut run_ctx = 4usize;
    let mut lvl_ctx = 2usize;
    let mut zeros = 0u32;
    let ac = &q[nb..];
    let mut i = 0;
    while i < ac.len() {
        // Skip zeros four at a time.
        while i + 4 <= ac.len() && (ac[i] | ac[i + 1] | ac[i + 2] | ac[i + 3]) == 0 {
            zeros += 4;
            i += 4;
        }
        if i >= ac.len() {
            break;
        }
        let v = ac[i];
        i += 1;
        if v == 0 {
            zeros += 1;
            continue;
        }
        let a = u32::from(v.unsigned_abs());
        s.coef(run_ctx, lvl_ctx, zeros, a, v < 0);
        run_ctx = zeros as usize;
        lvl_ctx = a as usize;
        zeros = 0;
    }
}

// --- transform -------------------------------------------------------------------------

/// `0.5 * cos(k * pi / 16)`.
fn ck(k: u32) -> f32 {
    (0.5 * (f64::from(k) * std::f64::consts::PI / 16.0).cos()) as f32
}

/// Orthonormal 8-point DCT along the array index; the eight lanes are independent columns.
#[inline(always)]
fn dct8(x: [f32x8; 8]) -> [f32x8; 8] {
    let (c1, c2, c3, c4, c5, c6, c7) =
        (f32x8::splat(ck(1)), f32x8::splat(ck(2)), f32x8::splat(ck(3)), f32x8::splat(ck(4)), f32x8::splat(ck(5)), f32x8::splat(ck(6)), f32x8::splat(ck(7)));
    let s0 = x[0] + x[7];
    let s1 = x[1] + x[6];
    let s2 = x[2] + x[5];
    let s3 = x[3] + x[4];
    let d0 = x[0] - x[7];
    let d1 = x[1] - x[6];
    let d2 = x[2] - x[5];
    let d3 = x[3] - x[4];
    let e0 = s0 + s3;
    let e1 = s1 + s2;
    let f0 = s0 - s3;
    let f1 = s1 - s2;
    [
        c4 * (e0 + e1),
        c1 * d0 + c3 * d1 + c5 * d2 + c7 * d3,
        c2 * f0 + c6 * f1,
        c3 * d0 - c7 * d1 - c1 * d2 - c5 * d3,
        c4 * (e0 - e1),
        c5 * d0 - c1 * d1 + c7 * d2 + c3 * d3,
        c6 * f0 - c2 * f1,
        c7 * d0 - c5 * d1 + c3 * d2 - c1 * d3,
    ]
}

#[inline(always)]
fn transpose(x: &[f32x8; 8]) -> [f32x8; 8] {
    let a: [[f32; 8]; 8] = [
        x[0].to_array(), x[1].to_array(), x[2].to_array(), x[3].to_array(),
        x[4].to_array(), x[5].to_array(), x[6].to_array(), x[7].to_array(),
    ];
    let mut o = [f32x8::splat(0.0); 8];
    for c in 0..8 {
        o[c] = f32x8::new([a[0][c], a[1][c], a[2][c], a[3][c], a[4][c], a[5][c], a[6][c], a[7][c]]);
    }
    o
}

/// Forward DCT of the 8x8 block at `src[at..]` (row pitch `stride`). `r[u]` holds the
/// coefficients of horizontal frequency `u`, lane `v` = vertical frequency.
#[inline(always)]
fn fdct(src: &[f32], at: usize, stride: usize) -> [f32x8; 8] {
    let mut x = [f32x8::splat(0.0); 8];
    for (r, row) in x.iter_mut().enumerate() {
        let o = at + r * stride;
        *row = f32x8::new(src[o..o + 8].try_into().unwrap());
    }
    dct8(transpose(&dct8(x)))
}

// --- slice encoding --------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Geom {
    is_444: bool,
    mb_w: usize,
    /// Visible sizes of the three planes.
    pw: [usize; 3],
    ph: [usize; 3],
}

struct Src<'a> {
    planes: [&'a [u8]; 3],
    strides: [usize; 3],
    bytes2: bool,
    /// Left shift to the 12-bit domain (8-bit: widened first).
    fmt_bits: u32,
}

impl Src<'_> {
    /// One row of plane `p` as 12-bit-domain samples minus 2048.
    fn row(&self, p: usize, y: usize, w: usize, dst: &mut [f32]) {
        let at = y * self.strides[p];
        if self.bytes2 {
            let src = &self.planes[p][at..at + w * 2];
            let (sh, bias) = match self.fmt_bits {
                10 => (2u32, 2048i32),
                _ => (0, 2048),
            };
            for (d, s) in dst.iter_mut().zip(src.as_chunks::<2>().0) {
                *d = (((u32::from(u16::from_le_bytes([s[0], s[1]])) << sh) as i32) - bias) as f32;
            }
        } else {
            let src = &self.planes[p][at..at + w];
            for (d, &s) in dst.iter_mut().zip(src) {
                let v = u32::from(s);
                *d = (((v << 4) | (v >> 4)) as i32 - 2048) as f32;
            }
        }
    }
}

fn qscale_of(idx: usize) -> f32 {
    if idx > 128 { ((idx - 96) << 2) as f32 } else { idx as f32 }
}

struct Params {
    g: Geom,
    scan_pos: [usize; 64],
    /// 1 / qmat at each scan index (luma, chroma).
    rinv: [[f32; 64]; 2],
    rnd: f32,
    /// Slices carry an alpha payload (and the 8-byte slice header).
    alpha: bool,
}

/// The DCT coefficients of one slice, scan-index major / block minor, per component, and its
/// coded alpha (empty without an alpha channel).
struct SliceCoefs {
    coef: [Vec<f32>; 3],
    alpha: Vec<u8>,
}

/// One field's (or frame's) alpha samples, 16-bit full scale.
struct AlphaSrc<'a> {
    data: &'a [u16],
    stride: usize,
    w: usize,
    h: usize,
    bits: u8,
}

/// Run-length alpha coding (RDD 36 section 7.3) of `px` (raster order, 16-bit full scale), the
/// inverse of the decoder's `unpack_alpha`.
fn encode_alpha(px: &[u16], bits: u8) -> Vec<u8> {
    let mask: i32 = (1 << bits) - 1;
    let v: Vec<i32> = px.iter().map(|&a| if bits == 16 { i32::from(a) } else { i32::from(a >> 8) }).collect();
    let mut w = BitW::new();
    let (short_bits, max_short) = if bits == 16 { (7, 64) } else { (4, 8) };
    let mut prev = mask;
    let mut i = 0;
    let n = v.len();
    while i < n {
        loop {
            let d = (v[i] - prev) & mask;
            let sd = if d > mask / 2 { d - (mask + 1) } else { d };
            if sd != 0 && sd.abs() <= max_short {
                let m = sd.abs();
                w.put(0, 1);
                w.put((2 * m - 2 + i32::from(sd < 0)) as u32, short_bits);
            } else {
                w.put(1, 1);
                w.put(d as u32, u32::from(bits));
            }
            prev = v[i];
            i += 1;
            if i >= n {
                return w.finish();
            }
            if v[i] == prev {
                w.put(0, 1);
                break;
            }
            w.put(1, 1);
        }
        let mut run = 0;
        while i + run < n && v[i + run] == prev && run < 2047 {
            run += 1;
        }
        if run < 16 {
            w.put(run as u32, 4);
        } else {
            w.put(0, 4);
            w.put(run as u32, 11);
        }
        i += run;
    }
    w.finish()
}

/// Per-thread scratch: quantised coefficients.
struct Scratch {
    q: [Vec<i16>; 3],
}

impl Scratch {
    fn new() -> Scratch {
        Scratch { q: [Vec::new(), Vec::new(), Vec::new()] }
    }
}

fn load_row(g: &Geom, src: &Src, mb_y: usize, rows: &mut [Vec<f32>; 3]) {
    for p in 0..3 {
        let pitch = if p == 0 || g.is_444 { g.mb_w * 16 } else { g.mb_w * 8 };
        let (w, h) = (g.pw[p], g.ph[p]);
        rows[p].resize(pitch * 16, 0.0);
        for yy in 0..16 {
            let sy = (mb_y * 16 + yy).min(h - 1);
            let dst = &mut rows[p][yy * pitch..(yy + 1) * pitch];
            src.row(p, sy, w, &mut dst[..w]);
            let last = dst[w - 1];
            dst[w..].fill(last);
        }
    }
}

/// Slice layout of one macroblock row: (first macroblock, macroblock count) per slice.
fn row_slices(mb_w: usize) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut mb_x = 0;
    while mb_x < mb_w {
        let mut cnt = 8usize;
        while mb_w - mb_x < cnt {
            cnt >>= 1;
        }
        v.push((mb_x, cnt));
        mb_x += cnt;
    }
    v
}

/// DCT every slice of macroblock row `mb_y`.
fn transform_row(pr: &Params, src: &Src, alpha: Option<&AlphaSrc>, mb_y: usize) -> Vec<SliceCoefs> {
    let g = &pr.g;
    let mut rows: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    load_row(g, src, mb_y, &mut rows);
    let mut out = Vec::new();
    for (mb_x, mbs) in row_slices(g.mb_w) {
        let mut coef: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for p in 0..3 {
            let luma_like = p == 0 || g.is_444;
            let nbk = if luma_like { 4 } else { 2 };
            let nb = mbs * nbk;
            let pitch = if luma_like { g.mb_w * 16 } else { g.mb_w * 8 };
            let c = &mut coef[p];
            c.resize(64 * nb, 0.0);
            let mbw_px = if luma_like { 16 } else { 8 };
            for mi in 0..mbs {
                for k in 0..nbk {
                    let (bx, by) = if p == 0 {
                        ((k & 1) * 8, (k >> 1) * 8)
                    } else if g.is_444 {
                        ((k >> 1) * 8, (k & 1) * 8)
                    } else {
                        (0, k * 8)
                    };
                    let x0 = (mb_x + mi) * mbw_px + bx;
                    let blk = fdct(&rows[p], by * pitch + x0, pitch);
                    let b = mi * nbk + k;
                    for u in 0..8 {
                        let col = blk[u].to_array();
                        for v in 0..8 {
                            c[pr.scan_pos[v * 8 + u] * nb + b] = col[v];
                        }
                    }
                }
            }
        }
        let alpha = alpha.map_or_else(Vec::new, |a| {
            let w = mbs * 16;
            let mut px = Vec::with_capacity(w * 16);
            for r in 0..16 {
                let sy = (mb_y * 16 + r).min(a.h - 1);
                for x in 0..w {
                    px.push(a.data[sy * a.stride + (mb_x * 16 + x).min(a.w - 1)]);
                }
            }
            encode_alpha(&px, a.bits)
        });
        out.push(SliceCoefs { coef, alpha });
    }
    out
}

fn quantize(pr: &Params, coef: &[f32], p: usize, qidx: usize, q: &mut Vec<i16>) {
    let qs = 1.0 / qscale_of(qidx);
    let rinv = &pr.rinv[usize::from(p != 0)];
    let nb = coef.len() / 64;
    q.resize(coef.len(), 0);
    let rnd = pr.rnd;
    for i in 0..64 {
        let inv = rinv[i] * qs;
        let src = &coef[i * nb..(i + 1) * nb];
        let dst = &mut q[i * nb..(i + 1) * nb];
        for (d, &c) in dst.iter_mut().zip(src) {
            let t = c * inv;
            let a = (t.abs() + rnd) as i32 as i16;
            *d = if t < 0.0 { -a } else { a };
        }
    }
}

fn slice_cost(pr: &Params, sc: &mut Scratch, s: &SliceCoefs, qidx: usize) -> usize {
    let mut total = if pr.alpha { 8 } else { 6 } + s.alpha.len();
    for p in 0..3 {
        quantize(pr, &s.coef[p], p, qidx, &mut sc.q[p]);
        let nb = s.coef[p].len() / 64;
        let mut c = Counter { bits: 0, t: tabs() };
        code_component(&mut c, &sc.q[p], nb);
        total += (c.bits as usize).div_ceil(8);
    }
    total
}

fn write_slice(pr: &Params, sc: &mut Scratch, s: &SliceCoefs, qidx: usize) -> Result<Vec<u8>> {
    let mut comps: Vec<Vec<u8>> = Vec::with_capacity(3);
    for p in 0..3 {
        quantize(pr, &s.coef[p], p, qidx, &mut sc.q[p]);
        let nb = s.coef[p].len() / 64;
        let mut w = BitW::new();
        code_component(&mut w, &sc.q[p], nb);
        comps.push(w.finish());
    }
    if comps.iter().any(|c| c.len() > 65535) {
        return Err(Error::unsupported("ProRes slice over 64 KiB per component", "lower the rate"));
    }
    let mut out = Vec::with_capacity(8 + s.alpha.len() + comps.iter().map(Vec::len).sum::<usize>());
    out.push(if pr.alpha { 8 << 3 } else { 6 << 3 });
    out.push(qidx as u8);
    out.extend_from_slice(&(comps[0].len() as u16).to_be_bytes());
    out.extend_from_slice(&(comps[1].len() as u16).to_be_bytes());
    if pr.alpha {
        out.extend_from_slice(&(comps[2].len() as u16).to_be_bytes());
    }
    for c in &comps {
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&s.alpha);
    if out.len() > 65535 {
        return Err(Error::unsupported("ProRes slice over 64 KiB", "lower the rate"));
    }
    Ok(out)
}

/// Run `f(chunk_index)` for `0..n` over up to `threads` scoped threads, results in order.
fn par_map<R: Send>(n: usize, threads: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    if threads <= 1 || n <= 1 {
        return (0..n).map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..n).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..threads.min(n) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let r = f(i);
                    results.lock().unwrap()[i] = Some(r);
                }
            });
        }
    });
    results.into_inner().unwrap().into_iter().map(|r| r.unwrap()).collect()
}

const MAXQ: usize = 224;

/// Smallest quantiser index whose estimated frame cost fits `budget`, by a secant search on
/// (ln qscale, ln bytes) inside a shrinking bracket. Returns (`hi` fits, `lo` over budget or 0,
/// every cost evaluated).
fn search_q(cost: &impl Fn(usize) -> f64, budget: f64, start: usize) -> (usize, usize, std::collections::HashMap<usize, f64>) {
    let mut costs: Vec<(usize, f64)> = Vec::new();
    let (mut lo, mut hi) = (0usize, MAXQ + 1);
    let mut q = start.clamp(1, MAXQ);
    while hi - lo > 1 {
        let c = cost(q);
        costs.push((q, c));
        if c <= budget {
            hi = q;
        } else {
            lo = q;
        }
        if hi - lo <= 1 {
            break;
        }
        let mut alpha = 0.8;
        if costs.len() >= 2 {
            let (qa, ca) = costs[costs.len() - 2];
            let (qb, cb) = costs[costs.len() - 1];
            let d = f64::from(qscale_of(qb) / qscale_of(qa)).ln();
            if d.abs() > 1e-6 && ca > 0.0 && cb > 0.0 {
                alpha = (-(cb / ca).ln() / d).clamp(0.3, 2.0);
            }
        }
        let want = f64::from(qscale_of(q)).ln() + (c.max(1.0) / budget.max(1.0)).ln() / alpha;
        let s = want.exp();
        let guess = if s <= 128.0 { s.round() as usize } else { (s / 4.0).round() as usize + 96 };
        let mut next = guess.clamp(lo + 1, hi - 1);
        if next == q {
            next = if c <= budget { q - 1 } else { q + 1 }.clamp(lo + 1, hi - 1);
        }
        q = next;
    }
    (hi, lo, costs.into_iter().collect())
}

/// Apple ProRes encoder. One call to [`ProResEncoder::encode`] turns a picture into one frame
/// (the sample a QuickTime / MP4 track holds).
///
/// Input: 4:2:2 profiles take [`PixelFormat::I210`] or [`PixelFormat::I422`]; 4444 profiles take
/// [`PixelFormat::I410`], [`PixelFormat::I412`] or [`PixelFormat::I444`]. The frame is coded
/// progressive or, when the frame says so, as two fields, and with an alpha channel only through
/// [`ProResEncoder::encode_with_alpha`].
pub struct ProResEncoder {
    cfg: EncoderConfig,
    params: CodecParameters,
    out: VecDeque<Packet>,
    eof: bool,
}

impl ProResEncoder {
    /// A new encoder.
    pub fn new(mut cfg: EncoderConfig) -> Result<ProResEncoder> {
        if cfg.width == 0 || cfg.height == 0 || cfg.width > MAX_DIM || cfg.height > MAX_DIM {
            return Err(Error::unsupported(
                format!("ProRes picture size {}x{}", cfg.width, cfg.height),
                "sizes 1..=16384 on each side",
            ));
        }
        if !matches!(cfg.alpha_bits, 0 | 8 | 16) || (cfg.alpha_bits != 0 && !cfg.profile.is_444()) {
            return Err(Error::unsupported(
                format!("ProRes alpha_bits {} for {:?}", cfg.alpha_bits, cfg.profile),
                "alpha is 8 or 16 bits and only the 4444 profiles carry it",
            ));
        }
        cfg.threads = cfg.threads.clamp(1, 6);
        let mut params = CodecParameters::new(CodecId::ProRes);
        params.extradata = Some(cfg.profile.fourcc().to_vec().into());
        if let MediaParameters::Video(v) = &mut params.media {
            v.width = cfg.width;
            v.height = cfg.height;
            v.format = Some(if cfg.profile.is_444() { PixelFormat::I412 } else { PixelFormat::I210 });
            v.color = cfg.color;
        }
        Ok(ProResEncoder { cfg, params, out: VecDeque::new(), eof: false })
    }

    /// The configuration in force.
    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// Encode one picture into a ProRes frame. With [`EncoderConfig::alpha_bits`] set the alpha
    /// channel is coded fully opaque; see [`ProResEncoder::encode_with_alpha`].
    pub fn encode(&self, frame: &VideoFrame) -> Result<Vec<u8>> {
        self.encode_with_alpha(frame, None)
    }

    /// Encode one picture and its alpha plane (16-bit full scale, as the decoder returns it).
    /// `alpha` must match the picture size and requires [`EncoderConfig::alpha_bits`] to be 8 or
    /// 16; with `None` the coded alpha is fully opaque. A frame whose
    /// [`interlace`](VideoFrame::interlace) is not progressive is coded as two field pictures.
    pub fn encode_with_alpha(&self, frame: &VideoFrame, alpha: Option<&AlphaPlane>) -> Result<Vec<u8>> {
        let cfg = &self.cfg;
        let is_444 = cfg.profile.is_444();
        let (fmt_bits, bytes2) = match (frame.format, is_444) {
            (PixelFormat::I422, false) => (8, false),
            (PixelFormat::I210, false) => (10, true),
            (PixelFormat::I444, true) => (8, false),
            (PixelFormat::I410, true) => (10, true),
            (PixelFormat::I412, true) => (12, true),
            (f, _) => {
                return Err(Error::unsupported(
                    format!("ProRes {:?} from {f:?}", cfg.profile),
                    "422 profiles take I210 or I422, 4444 profiles take I410, I412 or I444",
                ));
            }
        };
        if frame.width != cfg.width || frame.height != cfg.height {
            return Err(Error::unsupported(
                format!("frame {}x{} against an encoder for {}x{}", frame.width, frame.height, cfg.width, cfg.height),
                "create one encoder per picture size",
            ));
        }
        let interlaced = frame.interlace != Interlace::Progressive;
        let npics = if interlaced { 2 } else { 1 };
        if interlaced && cfg.height < 2 {
            return Err(Error::unsupported("interlaced ProRes picture one line tall", "use a progressive frame"));
        }
        if let Some(a) = alpha {
            if cfg.alpha_bits == 0 {
                return Err(Error::unsupported("an alpha plane", "set EncoderConfig::alpha_bits to 8 or 16"));
            }
            if a.width != cfg.width || a.height != cfg.height || a.data.len() < (cfg.height as usize - 1) * a.stride + cfg.width as usize {
                return Err(Error::unsupported("an alpha plane of another size", "match the picture size"));
            }
        }
        let (w, h) = (cfg.width as usize, cfg.height as usize);
        let cw = if is_444 { w } else { w.div_ceil(2) };
        let mb_w = w.div_ceil(16);
        // Rows of macroblocks per picture: a field is padded up to whole 32-line frame pairs.
        let mb_h = if interlaced { h.div_ceil(32) } else { h.div_ceil(16) };
        let scan_table = if interlaced { &INTERLACED_SCAN } else { &PROGRESSIVE_SCAN };
        let coded_alpha = cfg.alpha_bits != 0;
        let qmat = cfg.profile.qmats();
        let mut scan_pos = [0usize; 64];
        let mut rinv = [[0f32; 64]; 2];
        for (i, &r) in scan_table.iter().enumerate() {
            scan_pos[r as usize] = i;
            rinv[0][i] = 1.0 / f32::from(qmat.0[r as usize]);
            rinv[1][i] = 1.0 / f32::from(qmat.1[r as usize]);
        }
        let g = Geom { is_444, mb_w, pw: [w, cw, cw], ph: [h, h, h] };
        let pr = Params { g, scan_pos, rinv, rnd: 0.5, alpha: coded_alpha };
        let threads = cfg.threads;

        let t0 = std::time::Instant::now();
        // Phase 1: DCT of every slice of every picture (a field is every other line, starting
        // with the first field in display order), rows in parallel.
        let first_parity = usize::from(frame.interlace == Interlace::BottomFieldFirst);
        let mut slices: Vec<SliceCoefs> = Vec::new();
        for pic in 0..npics {
            let parity = if interlaced { (first_parity + pic) & 1 } else { 0 };
            let step = npics;
            let fh = (h - parity).div_ceil(step);
            let mut pg = pr.g;
            pg.ph = [fh; 3];
            let prp = Params { g: pg, scan_pos: pr.scan_pos, rinv: pr.rinv, rnd: pr.rnd, alpha: pr.alpha };
            let pl = |p: usize| &frame.planes[p].data[parity * frame.planes[p].stride..];
            let src = Src {
                planes: [pl(0), pl(1), pl(2)],
                strides: [frame.planes[0].stride * step, frame.planes[1].stride * step, frame.planes[2].stride * step],
                bytes2,
                fmt_bits,
            };
            let asrc = coded_alpha.then(|| {
                static OPAQUE: [u16; 1] = [0xFFFF];
                match alpha {
                    Some(a) => AlphaSrc { data: &a.data[parity * a.stride..], stride: a.stride * step, w, h: fh, bits: cfg.alpha_bits },
                    None => AlphaSrc { data: &OPAQUE, stride: 0, w: 1, h: fh, bits: cfg.alpha_bits },
                }
            });
            slices.extend(par_map(mb_h, threads, |r| transform_row(&prp, &src, asrc.as_ref(), r)).into_iter().flatten());
        }
        let n_slices = slices.len();
        let t1 = t0.elapsed();

        // Phase 2: one quantiser for the whole frame (equal step everywhere is the MSE optimum),
        // the smallest that fits the frame budget, found on a sample of the slices; a fraction
        // of the slices takes the next finer step so the frame lands on the budget.
        let (q_coarse, q_fine, frac) = match cfg.qscale {
            Some(q) => {
                let q = usize::from(q).clamp(1, MAXQ);
                (q, q, 0.0)
            }
            None => {
                let mbs = mb_w * mb_h * npics;
                let bits_per_mb = cfg
                    .bits_per_mb
                    .map_or_else(|| nominal_frame_bytes(cfg.profile, mbs) * 8 / mbs, |b| b as usize);
                // Tiny pictures: the headers alone would eat Apple's proportional rate.
                let total = (bits_per_mb * mbs / 8).max(3000usize.saturating_sub(28 * mbs));
                let budget = total.saturating_sub(8 + 148 + 8 * npics + 2 * n_slices).max(16 * mbs);
                let budget_f = budget as f64;
                let (pr_ref, slices_ref) = (&pr, &slices);
                let cost_at = |stride: usize| {
                    let sample: Vec<usize> = (0..n_slices).step_by(stride).collect();
                    let scale = n_slices as f64 / sample.len() as f64;
                    let chunk = sample.len().div_ceil(threads * 2).max(1);
                    let nchunks = sample.len().div_ceil(chunk);
                    move |q: usize| -> f64 {
                        let parts = par_map(nchunks, threads, |c| {
                            let mut sc = Scratch::new();
                            sample[c * chunk..((c + 1) * chunk).min(sample.len())]
                                .iter()
                                .map(|&i| slice_cost(pr_ref, &mut sc, &slices_ref[i], q))
                                .sum::<usize>()
                        });
                        parts.iter().sum::<usize>() as f64 * scale
                    }
                };
                // A cheap sample finds the neighbourhood, a larger one the exact step.
                let (coarse, _, _) = search_q(&cost_at((n_slices / 24).clamp(1, 40)), budget_f, cfg.profile.start_q());
                let (hi, lo, costs) = search_q(&cost_at((n_slices / 128).clamp(1, 8)), budget_f, coarse.min(MAXQ));
                let q = hi.min(MAXQ);
                if lo >= 1 && lo + 1 == q {
                    // Mix: fraction of slices at the finer step `lo`.
                    let c_hi = costs[&q];
                    let c_lo = costs[&lo];
                    let f = ((budget_f - c_hi) / (c_lo - c_hi)).clamp(0.0, 1.0);
                    (q, lo, f)
                } else {
                    (q, q, 0.0)
                }
            }
        };

        let t2 = t0.elapsed();
        // Phase 3: entropy-code the slices.
        let per_row = row_slices(mb_w).len();
        let q_of = |i: usize| -> usize {
            // Bresenham spread of the finer step over the slice index.
            if q_fine != q_coarse && ((i + 1) as f64 * frac).floor() > (i as f64 * frac).floor() {
                q_fine
            } else {
                q_coarse
            }
        };
        let chunk = per_row.max(1);
        let nchunks = n_slices.div_ceil(chunk);
        let parts = par_map(nchunks, threads, |c| -> Result<Vec<Vec<u8>>> {
            let mut sc = Scratch::new();
            (c * chunk..((c + 1) * chunk).min(n_slices))
                .map(|i| write_slice(&pr, &mut sc, &slices[i], q_of(i)))
                .collect()
        });
        let mut all: Vec<Vec<u8>> = Vec::with_capacity(n_slices);
        for p in parts {
            all.extend(p?);
        }
        if std::env::var_os("EC_TRACE").is_some() { eprintln!("dct {:?} search {:?} write {:?}", t1, t2 - t1, t0.elapsed() - t2); }
        // Frame.
        let per_pic = all.len() / npics;
        let (ql, qc) = cfg.profile.qmats();
        let hdr_size = 20 + 128;
        let pic_hdr = 8;
        let pic_sizes: Vec<usize> = all
            .chunks(per_pic)
            .map(|c| pic_hdr + c.len() * 2 + c.iter().map(Vec::len).sum::<usize>())
            .collect();
        let total = 8 + hdr_size + pic_sizes.iter().sum::<usize>();
        let mut f = Vec::with_capacity(total);
        f.extend_from_slice(&(total as u32).to_be_bytes());
        f.extend_from_slice(b"icpf");
        f.extend_from_slice(&(hdr_size as u16).to_be_bytes());
        f.extend_from_slice(&(u16::from(is_444)).to_be_bytes()); // version: 1 for 4444 as ffmpeg writes
        f.extend_from_slice(&cfg.creator);
        f.extend_from_slice(&(cfg.width as u16).to_be_bytes());
        f.extend_from_slice(&(cfg.height as u16).to_be_bytes());
        let frame_type: u8 = match frame.interlace {
            Interlace::Progressive => 0,
            Interlace::TopFieldFirst => 1,
            Interlace::BottomFieldFirst => 2,
        };
        f.push((if is_444 { 3 << 6 } else { 2 << 6 }) | frame_type << 2);
        f.push(0);
        f.push(cfg.color.primaries);
        f.push(cfg.color.transfer);
        f.push(cfg.color.matrix);
        f.push(match cfg.alpha_bits {
            0 => 0,
            8 => 1,
            _ => 2,
        });
        f.push(0);
        f.push(3); // both quantisation matrices present
        f.extend_from_slice(ql);
        f.extend_from_slice(qc);
        for (chunk, &pic_size) in all.chunks(per_pic).zip(&pic_sizes) {
            f.push((pic_hdr as u8) << 3);
            f.extend_from_slice(&(pic_size as u32).to_be_bytes());
            f.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
            f.push(3 << 4); // 8 macroblocks per slice
            for s in chunk {
                f.extend_from_slice(&(s.len() as u16).to_be_bytes());
            }
            for s in chunk {
                f.extend_from_slice(s);
            }
        }
        debug_assert_eq!(f.len(), total);
        Ok(f)
    }
}

impl Encoder for ProResEncoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        let Frame::Video(v) = frame else {
            return Err(Error::unsupported("a non-video frame", "the ProRes encoder takes video"));
        };
        let data = self.encode(v)?;
        let (tb, pts) = match v.pts {
            Some(t) => (t.base, Some(t.ticks)),
            None => (ec_core::timebase::TimeBase::try_new(1, 1)?, None),
        };
        let mut p = Packet::new(0, tb, data);
        p.pts = pts;
        p.dts = pts;
        p.flags.keyframe = true;
        self.out.push_back(p);
        Ok(())
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        match self.out.pop_front() {
            Some(p) => Ok(p),
            None if self.eof => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }
}
