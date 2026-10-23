//! Slice decoding: entropy-coded DC/AC coefficients, dequantisation, IDCT, alpha.

use ec_core::error::{Error, Result};

use crate::idct::idct_put;
use crate::tables::*;

/// MSB-first bit reader over a zero-padded copy of one component (at least 8 bytes of padding),
/// so a 64-bit window can always be loaded. Reads past the end return zeros and are
/// reported by [`Br::overrun`].
pub struct Br<'a> {
    buf: &'a [u8],
    /// Real data length in bits.
    total: usize,
    pos: usize,
    err: bool,
}

/// Sentinel length for an oversized codeword.
const BAD: u32 = 255;

impl<'a> Br<'a> {
    /// `buf` is the component data followed by at least 8 zero bytes; `len` the data length.
    pub fn new(buf: &'a [u8], len: usize) -> Self {
        debug_assert!(buf.len() >= len + 8);
        Br { buf, total: len * 8, pos: 0, err: false }
    }

    /// 64 bits at the current position (the top 57 are always valid).
    #[inline(always)]
    fn peek(&self) -> u64 {
        let byte = (self.pos >> 3).min(self.buf.len() - 8);
        let w = u64::from_be_bytes(self.buf[byte..byte + 8].try_into().unwrap());
        w << (self.pos & 7)
    }

    #[inline(always)]
    fn advance(&mut self, n: u32) {
        self.pos += n as usize;
    }

    #[inline(always)]
    fn bits(&mut self, n: u32) -> u32 {
        let v = ((self.peek() >> 1) >> (63 - n)) as u32;
        self.advance(n);
        v
    }

    #[inline(always)]
    pub fn overrun(&self) -> bool {
        self.err || self.pos > self.total
    }

    #[inline(always)]
    fn left(&self) -> usize {
        self.total.saturating_sub(self.pos)
    }

    /// Decode one Rice / exp-Golomb hybrid codeword from the window `w` (RDD 36 section
    /// 7.1.1): `(value, length)`, length [`BAD`] when oversized.
    #[inline(always)]
    fn decode_w(cb: u8, w: u64) -> (u32, u32) {
        let switch_bits = u32::from(cb & 3) + 1;
        let rice = u32::from(cb >> 5);
        let exp = u32::from((cb >> 2) & 7);
        let q = w.leading_zeros();
        if q < switch_bits {
            let r = (((w << (q + 1)) >> 1) >> (63 - rice)) as u32;
            ((q << rice) + r, q + 1 + rice)
        } else {
            if q > 24 {
                return (0, BAD);
            }
            let nb = q - switch_bits + exp; // bits after the leading 1, at most 30
            let rest = (((w << (q + 1)) >> 1) >> (63 - nb)) as u32;
            (((1u32 << nb) + rest - (1 << exp)) + (switch_bits << rice), q + 1 + nb)
        }
    }

    #[inline(always)]
    fn codeword(&mut self, cb: u8) -> u32 {
        let (v, l) = Self::decode_w(cb, self.peek());
        if l == BAD {
            self.err = true;
            return 0;
        }
        self.advance(l);
        v
    }
}

/// 8-bit-window lookup tables for the codebooks the AC coefficients use, indexed by
/// `context * 256 + window byte`. Entry bits: 0..6 code length (0 when the code is longer than
/// 8 bits; level entries include the sign bit), 8..20 decoded value, 20.. the table offset (`context * 256`) of the next code of the
/// same kind, so the dependent chain through the tables needs no clamping.
struct Luts {
    run: [u32; 16 * 256],
    level: [u32; 10 * 256],
    /// DC codebooks, entry `value << 4 | length`, 0 for codes over 8 bits.
    dc: [[u16; 256]; 7],
}

fn luts() -> &'static Luts {
    static L: std::sync::OnceLock<Box<Luts>> = std::sync::OnceLock::new();
    L.get_or_init(|| {
        let mut l = Box::new(Luts { run: [0; 16 * 256], level: [0; 10 * 256], dc: [[0; 256]; 7] });
        for ctx in 0..7 {
            for i in 0..256usize {
                let (v, n) = Br::decode_w(DC_CB[ctx], (i as u64) << 56);
                if n <= 8 && v < 4096 {
                    l.dc[ctx][i] = (v as u16) << 4 | n as u16;
                }
            }
        }
        for ctx in 0..16 {
            for i in 0..256usize {
                let (v, n) = Br::decode_w(RUN_CB[ctx], (i as u64) << 56);
                if n <= 8 && v < 4000 {
                    l.run[ctx * 256 + i] = n | (v + 1) << 8 | (v.min(15) << 8) << 20;
                }
            }
        }
        for ctx in 0..10 {
            for i in 0..256usize {
                let (v, n) = Br::decode_w(LEVEL_CB[ctx], (i as u64) << 56);
                if n <= 8 && v < 4096 {
                    l.level[ctx * 256 + i] = (n + 1) | v << 8 | ((v + 1).min(9) << 8) << 20;
                }
            }
        }
        l
    })
}

fn decode_dc(br: &mut Br, out: &mut [i16], nblocks: usize, q: &[i16; 64]) -> Result<()> {
    let mut code = br.codeword(FIRST_DC_CB);
    let mut prev = (((code >> 1) as i32) ^ -((code & 1) as i32)) as i16;
    let q0 = i32::from(q[0]);
    out[0] = (i32::from(prev).wrapping_mul(q0)) as i16;
    let lt = luts();
    let mut delta: i16 = 3;
    // The first delta's codebook is fixed (index 5), whatever the first DC code was.
    code = 5;
    for b in 1..nblocks {
        let ctx = (code as usize).min(6);
        let e = lt.dc[ctx][(br.peek() >> 56) as usize];
        if e & 15 != 0 {
            br.advance(u32::from(e & 15));
            code = u32::from(e >> 4);
        } else {
            code = br.codeword(DC_CB[ctx]);
        }
        let sign: i32 = -((((delta >> 15) & 1) as i32) ^ (code & 1) as i32);
        delta = ((((code.wrapping_add(1)) >> 1) as i32 ^ sign) - sign) as i16;
        prev = prev.wrapping_add(delta);
        out[b * 64] = (i32::from(prev).wrapping_mul(q0)) as i16;
    }
    if br.overrun() {
        return Err(Error::corrupt("ProRes slice: DC data past the component size or oversized codeword"));
    }
    Ok(())
}

/// Register-buffered AC decoder for one component of one slice. Several of these advance
/// in lock step so the CPU overlaps their (latency-bound) bit-stream dependency chains.
struct AcState<'a> {
    buf: &'a [u8],
    total: usize,
    ptr: usize,
    acc: u64,
    cnt: u32,
    roff: usize,
    loff: usize,
    pos: usize,
    log2: u32,
    mask: usize,
    max_coeffs: usize,
    out: &'a mut [i16],
    scan: &'a [u8; 64],
    q: &'a [i16; 64],
}

impl<'a> AcState<'a> {
    /// `buf`: component data plus at least 40 zero bytes; `bitpos` where the AC data starts.
    fn new(buf: &'a [u8], len: usize, bitpos: usize, out: &'a mut [i16], nblocks: usize, scan: &'a [u8; 64], q: &'a [i16; 64]) -> Self {
        let (ptr, acc, cnt) = sync(buf, bitpos);
        let log2 = nblocks.trailing_zeros();
        AcState {
            buf,
            total: len * 8,
            ptr,
            acc,
            cnt,
            roff: 4 << 8,
            loff: 2 << 8,
            pos: nblocks - 1,
            log2,
            mask: nblocks - 1,
            max_coeffs: 64usize << log2,
            out,
            scan,
            q,
        }
    }

    /// Stores one decoded coefficient (`level` is the raw level, one less than its magnitude).
    #[inline(always)]
    fn put(&mut self, run: u32, level: u32, sign: u64) -> Result<()> {
        self.pos += run as usize;
        if self.pos >= self.max_coeffs {
            return Err(Error::corrupt("ProRes slice: AC run past the block"));
        }
        let sign = sign as i32;
        let v = (((level as i32 + 1) ^ -sign) + sign) as i16;
        let at = self.scan[(self.pos >> self.log2) & 63] as usize & 63;
        let idx = ((self.pos & self.mask) << 6) + at;
        self.out[idx] = i32::from(v).wrapping_mul(i32::from(self.q[at])) as i16;
        Ok(())
    }

    /// One coefficient; `Ok(false)` when it took the slow path (state resynchronised).
    #[inline(always)]
    fn step(&mut self, lt: &Luts) -> Result<bool> {
        let w = self.acc;
        let e1 = lt.run[self.roff + (w >> 56) as usize];
        let l1 = e1 & 63;
        let w1 = w << l1;
        let e2 = lt.level[self.loff + (w1 >> 56) as usize];
        let l2 = e2 & 63;
        if (l1 != 0) & (l2 != 0) {
            self.acc = w1 << l2;
            self.cnt -= l1 + l2;
            self.roff = (e1 >> 20) as usize;
            self.loff = (e2 >> 20) as usize;
            self.put((e1 >> 8) & 0xfff, (e2 >> 8) & 0xfff, (w1 >> (64 - l2)) & 1)?;
            Ok(true)
        } else {
            self.slow()?;
            Ok(false)
        }
    }

    #[cold]
    #[inline(never)]
    fn slow(&mut self) -> Result<()> {
        let buf = self.buf;
        let at = self.ptr * 8 - self.cnt as usize;
        let (run, a) = Br::decode_w(RUN_CB[self.roff >> 8], be_at(buf, at));
        if a == BAD {
            return Err(Error::corrupt("ProRes slice: oversized AC run codeword"));
        }
        let (level, b) = Br::decode_w(LEVEL_CB[self.loff >> 8], be_at(buf, at + a as usize));
        if b == BAD {
            return Err(Error::corrupt("ProRes slice: oversized AC level codeword"));
        }
        let sign = be_at(buf, at + (a + b) as usize) >> 63;
        (self.ptr, self.acc, self.cnt) = sync(buf, at + (a + b) as usize + 1);
        self.roff = (run.min(15) as usize) << 8;
        self.loff = ((level + 1).min(9) as usize) << 8;
        self.put(run + 1, level, sign)
    }

    /// Decodes a few coefficients; `Ok(true)` when the component is finished.
    #[inline(always)]
    fn advance(&mut self, lt: &Luts) -> Result<bool> {
        // Branch-free refill to at least 57 valid bits.
        self.acc |= be64(self.buf, self.ptr) >> self.cnt;
        let take = (64 - self.cnt) >> 3;
        self.ptr += take as usize;
        self.cnt += take * 8;
        let left = self.total.saturating_sub(self.ptr * 8 - self.cnt as usize);
        if left < 192 {
            // Near the end: all-zero padding terminates the data.
            if left < 32 && (left == 0 || (self.acc >> (64 - left as u32)) == 0) {
                return self.finish();
            }
            self.step(lt)?;
        } else if self.step(lt)? && self.step(lt)? {
            // 57+ bits cover three fast coefficients (17 bits at most each).
            self.step(lt)?;
        }
        Ok(false)
    }

    fn finish(&self) -> Result<bool> {
        if self.ptr * 8 - self.cnt as usize > self.total {
            return Err(Error::corrupt("ProRes slice: AC data past the component size"));
        }
        Ok(true)
    }
}

/// Runs one or two AC decoders to completion, interleaved.
fn run_ac(a: &mut AcState, b: Option<&mut AcState>) -> Result<()> {
    let lt = luts();
    match b {
        Some(b) => {
            let (mut da, mut db) = (false, false);
            while !(da || db) {
                da = a.advance(lt)?;
                db = b.advance(lt)?;
            }
            while !da {
                da = a.advance(lt)?;
            }
            while !db {
                db = b.advance(lt)?;
            }
        }
        None => while !a.advance(lt)? {},
    }
    Ok(())
}

#[inline(always)]
fn be64(buf: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(buf[at..at + 8].try_into().unwrap())
}

/// Register bit buffer state `(ptr, acc, cnt)` for stream bit position `pos`: `acc` holds `cnt`
/// valid bits MSB-first (the bits below are zero or the true following bits), the next
/// unloaded byte is `ptr`, and the consumed position is `ptr * 8 - cnt`.
#[inline(always)]
fn sync(buf: &[u8], pos: usize) -> (usize, u64, u32) {
    let p = pos >> 3;
    (p + 7, be64(buf, p) << (pos & 7), 56 - (pos & 7) as u32)
}

/// 64 bits at bit position `pos`, the top 57 valid.
#[inline(always)]
fn be_at(buf: &[u8], pos: usize) -> u64 {
    be64(buf, pos >> 3) << (pos & 7)
}

/// Everything slice decoding needs to know about the picture.
pub struct SliceCtx<'a> {
    pub is_444: bool,
    pub alpha_bits: u8,
    pub scan: &'a [u8; 64],
    pub qmat_luma: &'a [u8; 64],
    pub qmat_chroma: &'a [u8; 64],
    /// 12-bit output (4:4:4) rather than 10-bit.
    pub b12: bool,
}

/// Destination rows of one group of macroblock rows. Planes are little-endian `u16` samples, rows
/// `16 * mb_rows` tall, `ys`/`cs` samples wide.
pub struct RowBuf {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub a: Vec<u16>,
    pub ys: usize,
    pub cs: usize,
}

pub struct Scratch {
    coef: [Vec<i16>; 2],
    pad: [Vec<u8>; 2],
    alpha: Vec<u16>,
}

impl Scratch {
    pub fn new() -> Self {
        Scratch { coef: [vec![0; 64 * 32], vec![0; 64 * 32]], pad: [Vec::new(), Vec::new()], alpha: Vec::new() }
    }
}

/// Quantiser (raster order in the header) times the slice quantiser, as the 16-bit values
/// ffmpeg multiplies by, in transposed layout (`[col * 8 + row]`) to match the blocks.
fn scaled(q: &[u8; 64], qscale: i32) -> [i16; 64] {
    let mut o = [0i16; 64];
    for i in 0..64 {
        o[(i & 7) * 8 + (i >> 3)] = (i32::from(q[i]) * qscale) as i16;
    }
    o
}

/// One slice to decode: its bytes and where it lands (macroblock column, group-relative row).
pub struct SliceIn<'a> {
    pub data: &'a [u8],
    pub mb_count: usize,
    pub mb_x: usize,
    pub row: usize,
}

struct Parsed<'a> {
    comp: [&'a [u8]; 3],
    alpha: &'a [u8],
    q: [[i16; 64]; 2],
    mb_count: usize,
    mb_x: usize,
    row: usize,
}

fn parse<'a>(cx: &SliceCtx, s: &SliceIn<'a>) -> Result<Parsed<'a>> {
    let data = s.data;
    let hdr = usize::from(data.first().copied().unwrap_or(0) >> 3);
    if hdr < 6 || hdr > data.len() {
        return Err(Error::corrupt(format!("ProRes slice header: size {hdr}")));
    }
    let idx = i32::from(data[1]);
    let qscale = if idx > 128 { (idx - 96) << 2 } else { idx };
    let ysz = usize::from(u16::from_be_bytes([data[2], data[3]]));
    let usz = usize::from(u16::from_be_bytes([data[4], data[5]]));
    let total = data.len();
    let rest = total - hdr;
    let vsz = if hdr > 7 {
        usize::from(u16::from_be_bytes([data[6], data[7]]))
    } else {
        rest.checked_sub(ysz + usz).ok_or_else(|| Error::corrupt("ProRes slice: component sizes past the slice"))?
    };
    // With alpha, the remainder after Y/U/V is the alpha data.
    let yuv = ysz + usz + vsz;
    if yuv > rest {
        return Err(Error::corrupt("ProRes slice: component sizes past the slice"));
    }
    let asz = if cx.alpha_bits != 0 { rest - yuv } else { 0 };
    let body = &data[hdr..];
    Ok(Parsed {
        comp: [&body[..ysz], &body[ysz..ysz + usz], &body[ysz + usz..yuv]],
        alpha: &body[yuv..yuv + asz],
        q: [scaled(cx.qmat_luma, qscale), scaled(cx.qmat_chroma, qscale)],
        mb_count: s.mb_count,
        mb_x: s.mb_x,
        row: s.row,
    })
}

/// Zeroes the coefficient buffer, copies the component behind zero padding and decodes its DC
/// coefficients; returns the bit position where the AC data starts.
fn prep(pad: &mut Vec<u8>, coef: &mut [i16], data: &[u8], nblocks: usize, q: &[i16; 64]) -> Result<usize> {
    coef.fill(0);
    pad.clear();
    pad.extend_from_slice(data);
    pad.extend_from_slice(&[0; 40]);
    let mut br = Br::new(pad, data.len());
    decode_dc(&mut br, coef, nblocks, q)?;
    Ok(br.pos)
}

/// Inverse transform every block of one component of one slice into `dst`.
fn put_blocks(cx: &SliceCtx, comp: usize, p: &Parsed, coef: &mut [i16], dst: &mut [u8], stride: usize) {
    let (per_mb, mbw) = match (comp, cx.is_444) {
        (0, _) | (_, true) => (4, 16),
        _ => (2, 8),
    };
    let base = p.row * 16 * stride + p.mb_x * mbw;
    for m in 0..p.mb_count {
        for k in 0..per_mb {
            let off = match (comp, cx.is_444) {
                (0, _) => base + m * 16 + (k & 1) * 8 + (k >> 1) * 8 * stride,
                (_, true) => base + m * 16 + (k >> 1) * 8 + (k & 1) * 8 * stride,
                _ => base + m * 8 + k * 8 * stride,
            };
            let i = m * per_mb + k;
            let blk: &mut [i16; 64] = (&mut coef[i * 64..i * 64 + 64]).try_into().unwrap();
            if cx.b12 {
                idct_put::<true>(&mut dst[off * 2..], stride, blk);
            } else {
                idct_put::<false>(&mut dst[off * 2..], stride, blk);
            }
        }
    }
}

fn blocks(cx: &SliceCtx, comp: usize, mb_count: usize) -> usize {
    if comp == 0 || cx.is_444 { mb_count * 4 } else { mb_count * 2 }
}

/// Decode one or two slices into `rb`. Two slices have their entropy decoding interleaved,
/// which hides the latency of the serial bit-stream parsing.
pub fn decode_slices(cx: &SliceCtx, items: &[SliceIn], rb: &mut RowBuf, sc: &mut Scratch) -> Result<()> {
    let pa = parse(cx, &items[0])?;
    let pb = items.get(1).map(|s| parse(cx, s)).transpose()?;
    let Scratch { coef, pad, alpha } = sc;
    let [coef_a, coef_b] = coef;
    let [pad_a, pad_b] = pad;
    for comp in 0..3 {
        let qi = usize::from(comp != 0);
        let na = blocks(cx, comp, pa.mb_count);
        let (ca, cb) = (&mut coef_a[..na * 64], &mut coef_b[..]);
        let posa = prep(pad_a, ca, pa.comp[comp], na, &pa.q[qi])?;
        let nb = pb.as_ref().map_or(0, |p| blocks(cx, comp, p.mb_count));
        let posb = match &pb {
            Some(p) => prep(pad_b, &mut cb[..nb * 64], p.comp[comp], nb, &p.q[qi])?,
            None => 0,
        };
        let mut sa = AcState::new(pad_a, pa.comp[comp].len(), posa, ca, na, cx.scan, &pa.q[qi]);
        let mut sb = pb.as_ref().map(|p| AcState::new(pad_b, p.comp[comp].len(), posb, &mut cb[..nb * 64], nb, cx.scan, &p.q[qi]));
        run_ac(&mut sa, sb.as_mut())?;

        let (ys, cs) = (rb.ys, rb.cs);
        let (dst, stride) = match comp {
            0 => (&mut rb.y, ys),
            1 => (&mut rb.u, cs),
            _ => (&mut rb.v, cs),
        };
        put_blocks(cx, comp, &pa, &mut coef_a[..na * 64], dst, stride);
        if let Some(p) = &pb {
            put_blocks(cx, comp, p, &mut coef_b[..nb * 64], dst, stride);
        }
    }
    if cx.alpha_bits != 0 {
        for p in std::iter::once(&pa).chain(pb.as_ref()) {
            let n = p.mb_count * 256;
            alpha.clear();
            alpha.resize(n, 0);
            unpack_alpha(p.alpha, alpha, cx.alpha_bits)?;
            let w = p.mb_count * 16;
            for r in 0..16 {
                let o = (p.row * 16 + r) * rb.ys + p.mb_x * 16;
                rb.a[o..o + w].copy_from_slice(&alpha[r * w..r * w + w]);
            }
        }
    }
    Ok(())
}

/// Run-length coded alpha (RDD 36 section 7.3). Output is 16-bit; 8-bit alpha is replicated
/// into both bytes so full scale is 0xFFFF either way.
fn unpack_alpha(d: &[u8], dst: &mut [u16], bits: u8) -> Result<()> {
    let mut padded = d.to_vec();
    padded.extend_from_slice(&[0; 8]);
    let mut br = Br::new(&padded, d.len());
    let n = dst.len();
    let mask: i32 = (1 << bits) - 1;
    let expand = |v: i32| -> u16 { if bits == 16 { v as u16 } else { ((v << 8) | v) as u16 } };
    let mut idx = 0usize;
    let mut alpha = mask;
    while idx < n {
        loop {
            let val = if br.bits(1) != 0 {
                br.bits(u32::from(bits)) as i32
            } else {
                let v = br.bits(if bits == 16 { 7 } else { 4 }) as i32;
                let neg = v & 1 != 0;
                let m = (v + 2) >> 1;
                if neg { -m } else { m }
            };
            alpha = (alpha + val) & mask;
            dst[idx] = expand(alpha);
            idx += 1;
            if br.overrun() {
                return Err(Error::corrupt("ProRes slice: alpha data past the slice"));
            }
            if idx >= n {
                return Ok(());
            }
            if br.left() == 0 || br.bits(1) == 0 {
                break;
            }
        }
        let mut run = br.bits(4) as usize;
        if run == 0 {
            run = br.bits(11) as usize;
        }
        if br.overrun() {
            return Err(Error::corrupt("ProRes slice: alpha data past the slice"));
        }
        let run = run.min(n - idx);
        let v = expand(alpha);
        dst[idx..idx + run].fill(v);
        idx += run;
    }
    Ok(())
}
