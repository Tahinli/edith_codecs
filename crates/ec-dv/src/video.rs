//! The DV video frame decoder: a line-for-line port of FFmpeg's `dvdec.c` arithmetic.

use std::sync::OnceLock;

use ec_core::error::{Error, Result};
use ec_core::frame::Interlace;

use crate::idct::{idct248_tile, idct_tile};
use crate::profile::{Chroma, PROFILE_BYTES, Profile, WorkChunk, frame_profile, work_chunks};
use crate::tables::*;

const DV_VIDEO_CONTROL: u8 = 0x61;
const IWEIGHT_BITS: u32 = 14;

/// The standard 8x8 zig-zag scan (ITU-T T.81 Figure A.6; ISO/IEC 13818-2 Figure 7-2 scan 0).
/// Regenerated algorithmically and compared in `zigzag_is_standard`.
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58,
    59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

#[derive(Clone, Copy, Default)]
struct VlcEntry {
    len: u8,
    run: u8,
    level: i16,
}

/// Direct 16-bit lookup: next 16 bits -> (code length incl. sign, run + 1, signed level).
fn vlc_table() -> &'static [VlcEntry] {
    static T: OnceLock<Box<[VlcEntry]>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![VlcEntry::default(); 1 << 16];
        let mut code: u32 = 0;
        for i in 0..NB_VLC {
            let len = u32::from(VLC_LEN[i]);
            let cur = code >> (32 - len);
            code = code.wrapping_add(1u32 << (32 - len));
            let run = VLC_RUN[i].wrapping_add(1);
            let level = i16::from(VLC_LEVEL[i]);
            let mut fill = |c: u32, l: u32, lv: i16| {
                let start = (c << (16 - l)) as usize;
                for e in &mut t[start..start + (1usize << (16 - l))] {
                    *e = VlcEntry { len: l as u8, run, level: lv };
                }
            };
            if level != 0 {
                fill(cur << 1, len + 1, level);
                fill((cur << 1) | 1, len + 1, -level);
            } else {
                fill(cur, len, 0);
            }
        }
        // FFmpeg decodes through a 10-bit first level plus sub-tables and consumes the sub-table's
        // full depth for every code under a prefix, not the code's own length. Only the 15-bit
        // `run 0, level 0` code is affected (it shares its prefix with 16-bit signed codes and is
        // consumed as 16 bits); reproduce that.
        for block in t.chunks_mut(1 << 6) {
            let max = block.iter().map(|e| e.len).max().unwrap_or(0);
            if max > 10 {
                for e in block.iter_mut() {
                    e.len = max;
                }
            }
        }
        t.into_boxed_slice()
    })
}

#[inline(always)]
fn peek32(data: &[u8], p: usize) -> u32 {
    let byte = p >> 3;
    let v = if let Some(w) = data.get(byte..byte + 8) {
        u64::from_be_bytes(w.try_into().unwrap_or([0; 8]))
    } else {
        let mut v = 0u64;
        for i in 0..8 {
            v = (v << 8) | u64::from(data.get(byte + i).copied().unwrap_or(0));
        }
        v
    };
    ((v << (p & 7)) >> 32) as u32
}

struct Gb<'a> {
    data: &'a [u8],
    size: usize,
    pos: usize,
}

impl<'a> Gb<'a> {
    fn left(&self) -> usize {
        self.size.saturating_sub(self.pos)
    }
}

/// Append-only bit sink (MSB first).
struct Bw {
    buf: Vec<u8>,
    acc: u64,
    nacc: u32,
}

impl Bw {
    fn new() -> Bw {
        Bw { buf: Vec::with_capacity(512), acc: 0, nacc: 0 }
    }
    fn clear(&mut self) {
        self.buf.clear();
        self.acc = 0;
        self.nacc = 0;
    }
    fn count(&self) -> usize {
        self.buf.len() * 8 + self.nacc as usize
    }
    fn put(&mut self, n: u32, v: u32) {
        self.acc = (self.acc << n) | u64::from(v & ((1u32 << n) - 1));
        self.nacc += n;
        while self.nacc >= 8 {
            self.buf.push((self.acc >> (self.nacc - 8)) as u8);
            self.nacc -= 8;
        }
        self.acc &= (1u64 << self.nacc) - 1;
    }
    /// Copy every bit still unread in `gb`.
    fn bit_copy(&mut self, gb: &mut Gb) {
        while gb.left() > 0 {
            let n = gb.left().min(16);
            let v = peek32(gb.data, gb.pos) >> (32 - n);
            self.put(n as u32, v);
            gb.pos += n;
        }
    }
    /// Zero padding so readers may look past the end.
    fn pad(&mut self) {
        if self.nacc > 0 {
            self.put(8 - self.nacc, 0);
        }
        self.buf.extend_from_slice(&[0; 8]);
    }
}

#[derive(Clone, Copy, Default)]
struct BlockInfo {
    factor: usize,
    scan: u8,
    use248: bool,
    pos: u16,
    pbc: u8,
    pbuf: u32,
}

fn decode_ac(gb: &mut Gb, mb: &mut BlockInfo, block: &mut [i16; 64], factors: &[u32], vlc: &[VlcEntry], scan: &[u8; 64]) {
    let size = gb.size as i32;
    let mut idx = gb.pos as i32;
    let mut cache = peek32(gb.data, gb.pos);
    if mb.pbc > 0 {
        cache = (cache >> mb.pbc) | mb.pbuf;
        idx -= i32::from(mb.pbc);
        mb.pbc = 0;
    }
    let mut pos = usize::from(mb.pos);
    let factor = &factors[mb.factor..mb.factor + 64];
    loop {
        let e = vlc[(cache >> 16) as usize];
        let len = i32::from(e.len.max(1));
        if idx + len > size {
            let n = (size - idx).max(0) as u32;
            mb.pbc = n as u8;
            mb.pbuf = cache & !(u32::MAX >> n);
            idx = size;
            break;
        }
        idx += len;
        pos += usize::from(e.run);
        if pos >= 64 {
            break;
        }
        let level = (i32::from(e.level).wrapping_mul(factor[pos] as i32).wrapping_add(1 << (IWEIGHT_BITS - 1))) >> IWEIGHT_BITS;
        block[usize::from(scan[pos])] = level as i16;
        cache = peek32(gb.data, idx as usize);
    }
    gb.pos = idx.max(0) as usize;
    mb.pos = pos as u16;
}

/// Planar output of one decoded DV frame in the stream's native layout.
#[derive(Debug, Clone)]
pub struct DvPicture {
    /// The profile the frame was decoded under.
    pub profile: &'static Profile,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Native chroma layout (`Yuv411`, `Yuv420` or `Yuv422`).
    pub chroma: Chroma,
    /// Y, Cb, Cr planes, tightly packed (stride = plane width).
    pub planes: [Vec<u8>; 3],
    /// Row pitch of each plane in bytes.
    pub strides: [usize; 3],
    /// Field structure per the VAUX video-control pack.
    pub interlace: Interlace,
    /// True when the VAUX pack signals a 16:9 picture.
    pub is_16_9: bool,
}

impl DvPicture {
    /// Chroma plane width in samples.
    pub fn chroma_width(&self) -> usize {
        chroma_dims(self.chroma, self.width as usize, self.height as usize).0
    }
    /// Chroma plane height in rows.
    pub fn chroma_height(&self) -> usize {
        chroma_dims(self.chroma, self.width as usize, self.height as usize).1
    }
}

pub(crate) fn chroma_dims(c: Chroma, w: usize, h: usize) -> (usize, usize) {
    match c {
        Chroma::Yuv411 => (w / 4, h),
        Chroma::Yuv420 => (w / 2, h / 2),
        Chroma::Yuv422 => (w / 2, h),
    }
}

/// Stateful DV frame decoder (keeps the previous profile for damaged-header recovery and caches
/// the dequantisation tables per profile).
pub struct DvVideoDecoder {
    sys: Option<&'static Profile>,
    chunks: Vec<WorkChunk>,
    factors: Vec<u32>,
    zigzag: [[u8; 64]; 2],
}

impl Default for DvVideoDecoder {
    fn default() -> Self {
        DvVideoDecoder::new()
    }
}

impl DvVideoDecoder {
    /// A decoder with no stream state.
    pub fn new() -> DvVideoDecoder {
        DvVideoDecoder { sys: None, chunks: Vec::new(), factors: Vec::new(), zigzag: [ZIGZAG, ZIGZAG248] }
    }

    /// Forget the remembered profile.
    pub fn reset(&mut self) {
        self.sys = None;
    }

    fn init_weights(&mut self, d: &Profile) {
        self.factors.clear();
        if d.is_hd() {
            const QSTEP: [u32; 16] = [1, 1, 2, 3, 4, 5, 6, 7, 8, 16, 18, 20, 22, 24, 28, 52];
            let (w1, w2): (&[u16; 64], &[u16; 64]) = if d.height == 720 {
                (&IWEIGHT_720_Y, &IWEIGHT_720_C)
            } else {
                (&IWEIGHT_1080_Y, &IWEIGHT_1080_C)
            };
            let mut f1 = Vec::with_capacity(4096);
            let mut f2 = Vec::with_capacity(4096);
            for c in 0..4 {
                for s in 0..16 {
                    for i in 0..64 {
                        f1.push((QSTEP[s] << (c + 9)).wrapping_mul(u32::from(w1[i])));
                        f2.push((QSTEP[s] << (c + 9)).wrapping_mul(u32::from(w2[i])));
                    }
                }
            }
            self.factors = f1;
            self.factors.extend(f2);
        } else {
            const AREAS: [usize; 4] = [6, 21, 43, 64];
            let mut f1 = Vec::with_capacity(2816);
            let mut f2 = Vec::with_capacity(2816);
            for iw in [&IWEIGHT_88, &IWEIGHT_248] {
                for s in 0..22 {
                    let mut i = 0;
                    for c in 0..4 {
                        while i < AREAS[c] {
                            let v = u32::from(iw[i]) << (QUANT_SHIFTS[s][c] + 1);
                            f1.push(v);
                            f2.push(v << 1);
                            i += 1;
                        }
                    }
                }
            }
            self.factors = f1;
            self.factors.extend(f2);
        }
    }

    /// Profile of the frame last decoded.
    pub fn profile(&self) -> Option<&'static Profile> {
        self.sys
    }

    /// Decode one DV frame (at least `frame_size` bytes; extra bytes are ignored).
    pub fn decode(&mut self, buf: &[u8]) -> Result<DvPicture> {
        let sys = match frame_profile(self.sys, buf) {
            Some(s) if buf.len() >= s.frame_size => s,
            _ => return Err(Error::corrupt("DV: could not find a frame profile (short or unrecognised frame)")),
        };
        if self.sys.map(|p| p as *const Profile) != Some(sys as *const Profile) {
            self.chunks = work_chunks(sys);
            self.init_weights(sys);
            self.sys = Some(sys);
        }
        let (w, h) = (sys.width as usize, sys.height as usize);
        let (cw, ch) = chroma_dims(sys.chroma, w, h);
        let mut planes = [vec![0u8; w * h], vec![0u8; cw * ch], vec![0u8; cw * ch]];
        let strides = [w, cw, cw];

        let vsc = &buf[80 * 5 + 48 + 5..];
        let mut interlace = Interlace::Progressive;
        let mut is_16_9 = false;
        if vsc[0] == DV_VIDEO_CONTROL {
            let apt = buf[4] & 7;
            is_16_9 = (vsc[2] & 7) == 2 || (apt == 0 && (vsc[2] & 7) == 7);
            if sys.height == 720 {
                interlace = Interlace::Progressive;
            } else if sys.height == 1080 {
                interlace = if vsc[3] & 0x40 == 0x40 { Interlace::TopFieldFirst } else { Interlace::BottomFieldFirst };
            } else if vsc[3] & 0x10 == 0x10 {
                interlace = if vsc[3] & 0x40 == 0 { Interlace::TopFieldFirst } else { Interlace::BottomFieldFirst };
            }
        }

        let vlc = vlc_table();
        let chunks = std::mem::take(&mut self.chunks);
        {
            let lens = [planes[0].len(), planes[1].len(), planes[2].len()];
            let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8).min(chunks.len().max(1));
            let per = chunks.len().div_ceil(threads).max(1);
            let this = &*self;
            let outs: Vec<Out> = std::thread::scope(|sc| {
                let handles: Vec<_> = chunks
                    .chunks(per)
                    .map(|part| {
                        sc.spawn(move || {
                            let mut out = Out { ops: Vec::new(), lens, strides };
                            for chunk in part {
                                this.decode_segment(buf, sys, chunk, &mut out, vlc);
                            }
                            out
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().expect("DV segment worker panicked")).collect()
            });
            for o in &outs {
                o.apply(&mut planes);
            }
        }
        self.chunks = chunks;
        Ok(DvPicture {
            profile: sys,
            width: sys.width,
            height: sys.height,
            chroma: sys.chroma,
            planes,
            strides,
            interlace,
            is_16_9,
        })
    }

    fn decode_segment(&self, buf: &[u8], sys: &Profile, chunk: &WorkChunk, out: &mut Out, vlc: &[VlcEntry]) {
        let bpm = sys.bpm;
        let hd = sys.is_hd();
        let mut sblock = [[0i16; 64]; 40];
        let mut mbd = [BlockInfo::default(); 40];
        let mut is_field = [false; 5];
        let mut vs_damaged = false;
        let mut mb_damaged = [false; 5];
        let mut retried = false;
        let mut seg_sta = 0u8;
        let mut pb = Bw::new();
        let mut vs_pb = Bw::new();

        loop {
            let mut base = usize::from(chunk.buf_offset) * 80;
            vs_pb.clear();
            for mb_index in 0..5 {
                let m0 = mb_index * bpm;
                let quant = usize::from(buf[base + 3] & 0x0f);
                // Error concealment as the ffmpeg CLI runs it (`-ec` defaults to 3): a DIF block
                // with status 0xE, or a status that differs inside the segment, marks the
                // segment's shuffled bit stream as damaged.
                let sta = buf[base + 3] >> 4;
                if sta == 0x0e {
                    vs_damaged = true;
                }
                if mb_index == 0 {
                    seg_sta = sta;
                } else if seg_sta != sta {
                    vs_damaged = true;
                }
                base += 4;
                pb.clear();
                is_field[mb_index] = false;
                for j in 0..bpm {
                    let last_index = usize::from(sys.block_sizes[j]);
                    let data = &buf[base..base + (last_index >> 3)];
                    let mut gb = Gb { data, size: last_index, pos: 12 };
                    let v = (u32::from(data[0]) << 8) | u32::from(data[1]);
                    let dc = (((v >> 7) as i32) << 23) >> 23;
                    let dct_mode = (v >> 6) & 1 != 0;
                    let class1 = ((v >> 4) & 3) as usize;
                    let mb = &mut mbd[m0 + j];
                    if hd {
                        mb.use248 = false;
                        mb.scan = 0;
                        mb.factor = usize::from(j >= 4) * 4 * 16 * 64 + class1 * 16 * 64 + quant * 64;
                        is_field[mb_index] |= j == 0 && dct_mode;
                    } else {
                        mb.use248 = dct_mode;
                        mb.scan = u8::from(dct_mode);
                        mb.factor = usize::from(class1 == 3) * 2 * 22 * 64
                            + usize::from(dct_mode) * 22 * 64
                            + (quant + usize::from(QUANT_OFFSET[class1])) * 64;
                    }
                    sblock[m0 + j][0] = (dc * 4 + 1024) as i16;
                    base += last_index >> 3;
                    mb.pos = 0;
                    mb.pbc = 0;
                    let scan = &self.zigzag[usize::from(mb.scan)];
                    decode_ac(&mut gb, mb, &mut sblock[m0 + j], &self.factors, vlc, scan);
                    if mb.pos >= 64 {
                        pb.bit_copy(&mut gb);
                    }
                    if mb.pos >= 64 && mb.pos < 127 {
                        vs_damaged = true;
                        mb_damaged[mb_index] = true;
                    }
                }
                if mb_damaged[mb_index] {
                    continue;
                }
                // pass 2: the leftover bits of this macroblock's finished blocks
                let n = pb.count();
                pb.pad();
                let mut gb = Gb { data: &pb.buf, size: n, pos: 0 };
                let mut j = 0;
                while j < bpm {
                    let mb = &mut mbd[m0 + j];
                    if mb.pos < 64 && gb.left() > 0 {
                        let scan = &self.zigzag[usize::from(mb.scan)];
                        decode_ac(&mut gb, mb, &mut sblock[m0 + j], &self.factors, vlc, scan);
                        if mb.pos < 64 {
                            break;
                        }
                        if mb.pos < 127 {
                            vs_damaged = true;
                            mb_damaged[mb_index] = true;
                        }
                    }
                    j += 1;
                }
                if j >= bpm {
                    vs_pb.bit_copy(&mut gb);
                }
            }

            // pass 3: leftovers of the whole video segment
            let n = vs_pb.count();
            vs_pb.pad();
            let mut gb = Gb { data: &vs_pb.buf, size: n, pos: 0 };
            for i in 0..5 * bpm {
                let mb = &mut mbd[i];
                if mb.pos < 64 && gb.left() > 0 && !vs_damaged {
                    let scan = &self.zigzag[usize::from(mb.scan)];
                    decode_ac(&mut gb, mb, &mut sblock[i], &self.factors, vlc, scan);
                }
                if mb.pos >= 64 && mb.pos < 127 {
                    vs_damaged = true;
                }
            }
            if vs_damaged && !retried {
                retried = true;
                sblock = [[0; 64]; 40];
                continue;
            }
            break;
        }

        self.place(buf, sys, chunk, &mut sblock, &mbd, &is_field, out);
    }

    #[allow(clippy::too_many_arguments)]
    fn place(&self, buf: &[u8], sys: &Profile, chunk: &WorkChunk, sblock: &mut [[i16; 64]; 40], mbd: &[BlockInfo; 40], is_field: &[bool; 5], out: &mut Out) {
        let bpm = sys.bpm;
        let [ls0, ls1, ls2] = out.strides;
        let lsz = [ls0, ls1, ls2];
        let chroma = sys.chroma;
        for mb_index in 0..5 {
            let mb_x = usize::from(chunk.mb[mb_index] & 0xff);
            let mut mb_y = usize::from(chunk.mb[mb_index] >> 8);
            if sys.height == 720 && buf[1] & 0x0c == 0 {
                mb_y = if mb_y > 17 { mb_y - 18 } else { mb_y + 72 };
            }
            let m0 = mb_index * bpm;
            let field = is_field[mb_index];
            let mut bi = 0usize; // block index within the macroblock

            // luminance
            let y_stride = if chroma == Chroma::Yuv420
                || (chroma == Chroma::Yuv411 && mb_x >= 704 / 8)
                || (sys.height >= 720 && mb_y != 134)
            {
                ls0 << (usize::from(!field) * 3)
            } else {
                2 << 3
            };
            let y_off = (mb_y * ls0 + mb_x) << 3;
            if mb_y == 134 && field {
                last_row_field_luma(out, 0, y_off, ls0, &mut sblock[m0..m0 + 4]);
            } else {
                let linesize = ls0 << usize::from(field);
                let put = |out: &mut Out, k: usize, off: usize, sblock: &mut [[i16; 64]; 40]| {
                    out.put_block(0, off, linesize, &mut sblock[m0 + k], mbd[m0 + k].use248);
                };
                if sys.video_stype == 4 {
                    put(out, 0, y_off, sblock);
                    put(out, 2, y_off + 8, sblock);
                } else {
                    put(out, 0, y_off, sblock);
                    put(out, 1, y_off + 8, sblock);
                    put(out, 2, y_off + y_stride, sblock);
                    put(out, 3, y_off + 8 + y_stride, sblock);
                }
            }
            bi += 4;

            // chrominance
            let cshift_y = usize::from(chroma == Chroma::Yuv420);
            let cshift_x = if chroma == Chroma::Yuv411 { 2 } else { 1 };
            for j in [2usize, 1] {
                let cs = lsz[j];
                let c_off = ((mb_y >> cshift_y) * cs + (mb_x >> cshift_x)) << 3;
                if chroma == Chroma::Yuv411 && mb_x >= 704 / 8 {
                    let k = m0 + bi;
                    let pixels = out.put_pixels(0, 8, &mut sblock[k], mbd[k].use248);
                    for y in 0..8 {
                        let c0 = c_off + y * cs;
                        let c1 = c0 + (cs << 3);
                        for x in 0..4 {
                            out.set(j, c0 + x, pixels[y * 8 + x]);
                            out.set(j, c1 + x, pixels[y * 8 + 4 + x]);
                        }
                    }
                    bi += 1;
                } else {
                    let y_stride = if mb_y == 134 { 8 } else { cs << (usize::from(!field) * 3) };
                    if mb_y == 134 && field {
                        last_row_field_chroma(out, j, c_off, cs, &mut sblock[m0 + bi..m0 + bi + 2]);
                        bi += 2;
                    } else {
                        let linesize = cs << usize::from(field);
                        let k = m0 + bi;
                        out.put_block(j, c_off, linesize, &mut sblock[k], mbd[k].use248);
                        bi += 1;
                        if bpm == 8 {
                            let k = m0 + bi;
                            out.put_block(j, c_off + y_stride, linesize, &mut sblock[k], mbd[k].use248);
                            bi += 1;
                        }
                    }
                }
            }
        }
    }
}

/// Pixel writes recorded by a segment decode, applied to the planes afterwards in segment order
/// (so segments can be decoded on several threads with a result identical to a serial decode).
enum Op {
    Tile { plane: u8, off: usize, stride: usize, px: [u8; 64] },
    Px { plane: u8, idx: usize, v: u8 },
}

struct Out {
    ops: Vec<Op>,
    lens: [usize; 3],
    strides: [usize; 3],
}

impl Out {
    fn apply(&self, planes: &mut [Vec<u8>; 3]) {
        for op in &self.ops {
            match op {
                Op::Tile { plane, off, stride, px } => {
                    let d = &mut planes[usize::from(*plane)];
                    for (r, row) in px.as_chunks::<8>().0.iter().enumerate() {
                        let o = off + r * stride;
                        d[o..o + 8].copy_from_slice(row);
                    }
                }
                Op::Px { plane, idx, v } => planes[usize::from(*plane)][*idx] = *v,
            }
        }
    }

    #[inline]
    fn set(&mut self, plane: usize, i: usize, v: u8) {
        if i < self.lens[plane] {
            self.ops.push(Op::Px { plane: plane as u8, idx: i, v });
        }
    }

    /// IDCT-and-store one block, skipping it when it would fall outside the plane.
    fn put_block(&mut self, plane: usize, off: usize, stride: usize, block: &mut [i16; 64], use248: bool) {
        // Both transforms touch rows 0..8 at `stride` pitch, so the last byte is at 7 * stride + 7.
        if off.saturating_add(7usize.saturating_mul(stride)).saturating_add(7) >= self.lens[plane] {
            return;
        }
        let px = if use248 { idct248_tile(block) } else { idct_tile(block) };
        self.ops.push(Op::Tile { plane: plane as u8, off, stride, px });
    }

    fn put_pixels(&mut self, off: usize, stride: usize, block: &mut [i16; 64], use248: bool) -> [u8; 64] {
        let _ = (off, stride);
        if use248 { idct248_tile(block) } else { idct_tile(block) }
    }
}

/// In-place (no clamp) IDCT as `s->idsp.idct`: results are stored as `int16`.
fn idct_inplace(block: &mut [i16; 64]) {
    crate::idct::idct_i16(block);
}

fn put_block_8x4(src: &[i16], out: &mut Out, plane: usize, off: usize, stride: usize) {
    for i in 0..4 {
        for j in 0..8 {
            out.set(plane, off + i * stride + j, src[i * 8 + j].clamp(0, 255) as u8);
        }
    }
}

fn last_row_field_chroma(out: &mut Out, plane: usize, off: usize, stride: usize, blocks: &mut [[i16; 64]]) {
    idct_inplace(&mut blocks[0]);
    idct_inplace(&mut blocks[1]);
    let s2 = stride << 1;
    put_block_8x4(&blocks[0][..32], out, plane, off, s2);
    put_block_8x4(&blocks[0][32..], out, plane, off + 8, s2);
    put_block_8x4(&blocks[1][..32], out, plane, off + stride, s2);
    put_block_8x4(&blocks[1][32..], out, plane, off + 8 + stride, s2);
}

fn last_row_field_luma(out: &mut Out, plane: usize, off: usize, stride: usize, blocks: &mut [[i16; 64]]) {
    for b in blocks.iter_mut() {
        idct_inplace(b);
    }
    let s2 = stride << 1;
    put_block_8x4(&blocks[0][..32], out, plane, off, s2);
    put_block_8x4(&blocks[0][32..], out, plane, off + 16, s2);
    put_block_8x4(&blocks[1][..32], out, plane, off + 8, s2);
    put_block_8x4(&blocks[1][32..], out, plane, off + 24, s2);
    put_block_8x4(&blocks[2][..32], out, plane, off + stride, s2);
    put_block_8x4(&blocks[2][32..], out, plane, off + 16 + stride, s2);
    put_block_8x4(&blocks[3][..32], out, plane, off + 8 + stride, s2);
    put_block_8x4(&blocks[3][32..], out, plane, off + 24 + stride, s2);
}

/// `true` when `buf` starts a plausible DV frame header (DIF header block of a DV file).
pub fn is_dv_header(buf: &[u8]) -> bool {
    buf.len() >= PROFILE_BYTES && (buf[0] == 0x1f) && buf[1] & 0x07 == 0x07 && buf[2] == 0 && (buf[3] & 0x7f) == 0x3f
}

#[cfg(test)]
mod tests {
    use super::ZIGZAG;
    #[test]
    fn zigzag_is_standard() {
        let mut z = Vec::new();
        for d in 0..15usize {
            let (lo, hi) = (d.saturating_sub(7), d.min(7));
            let rows: Vec<usize> = if d % 2 == 0 { (lo..=hi).rev().collect() } else { (lo..=hi).collect() };
            for r in rows {
                z.push((r * 8 + (d - r)) as u8);
            }
        }
        assert_eq!(z, ZIGZAG.to_vec());
    }

    use super::*;
    use crate::profile::PROFILES;

    #[test]
    fn vlc_table_is_a_complete_prefix_code() {
        let t = vlc_table();
        assert_eq!(t.len(), 1 << 16);
        assert!(t.iter().all(|e| e.len >= 2), "unused code space");
        // EOB is the 4-bit code 0110: run 127 + 1, no level.
        let e = t[0b0110 << 12];
        assert_eq!((e.len, e.run, e.level), (4, 128, 0));
        // the one length quirk: the 15-bit `run 0, level 0` code is consumed as 16 bits.
        let quirky = t.iter().filter(|e| e.len == 16 && e.level == 0).count();
        assert_eq!(quirky, 2);
    }

    #[test]
    fn work_chunks_stay_inside_the_frame_and_the_picture() {
        for (i, p) in PROFILES.iter().enumerate() {
            let chunks = work_chunks(p);
            assert_eq!(chunks.len(), p.work_size(), "profile {i}");
            let mut seen = std::collections::HashSet::new();
            for c in &chunks {
                assert!(usize::from(c.buf_offset) * 80 + 5 * 80 <= p.frame_size, "profile {i}: segment outside frame");
                assert!(seen.insert(c.buf_offset), "profile {i}: duplicate segment");
            }
        }
    }

    #[test]
    fn short_and_unknown_frames_are_refused() {
        let mut d = DvVideoDecoder::new();
        assert!(d.decode(&[]).is_err());
        assert!(d.decode(&[0u8; 100]).is_err());
        assert!(d.decode(&vec![0xffu8; 120000]).is_err() || d.profile().is_some());
        let mut f = vec![0u8; 120000];
        f[80 * 5 + 48 + 3] = 0; // dsf 0, stype 0 => NTSC DV25
        assert!(d.decode(&f[..119999]).is_err(), "truncated frame");
        assert!(d.decode(&f).is_ok());
    }
}
