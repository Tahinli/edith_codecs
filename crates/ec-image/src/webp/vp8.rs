//! VP8 key frame decoding (RFC 6386), which is all a still WebP contains.
//!
//! A `VP8 ` chunk in a still image is one key frame: intra prediction only, no
//! reference buffers and no motion vectors. Everything else the frame format
//! carries is here — the boolean entropy decoder, segmentation, per-partition
//! token decoding, the WHT/DCT inversions, all sixteen prediction modes and
//! both loop filters — because a key frame uses all of it.
//!
//! Inter-frame syntax (motion vectors, reference selection, sub-pixel
//! interpolation) is deliberately absent: this crate decodes stills, and a
//! decoder that pretends to accept an inter frame it cannot reconstruct is
//! worse than one that names the refusal.

use super::vp8_tables::{
    AC_QLOOKUP, COEFF_BANDS, COEFF_UPDATE_PROBS, DC_QLOOKUP, DEFAULT_COEFF_PROBS, KF_BMODE_PROBS,
    ZIGZAG,
};
use crate::Limits;
use ec_core::{Error, Result};

/// 16x16 luma / 8x8 chroma prediction modes.
const DC_PRED: usize = 0;
const V_PRED: usize = 1;
const H_PRED: usize = 2;
const TM_PRED: usize = 3;
const B_PRED: usize = 4;

/// Trees are flat arrays: a positive entry is the next node, anything else is
/// the leaf value negated (so leaf 0 encodes as 0, which is never a node).
const KF_YMODE_TREE: [i8; 8] = [-(B_PRED as i8), 2, 4, 6, 0, -1, -2, -3];
const KF_YMODE_PROBS: [u8; 4] = [145, 156, 163, 128];
const UV_MODE_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
const KF_UV_MODE_PROBS: [u8; 3] = [142, 114, 183];
const BMODE_TREE: [i8; 18] = [
    0, 2, -1, 4, -2, 6, 8, 12, -3, 10, -5, -6, -4, 14, -7, 16, -8, -9,
];
const SEGMENT_TREE: [i8; 6] = [2, 4, 0, -1, -2, -3];
const COEFF_TREE: [i8; 22] = [
    -11, 2, 0, 4, -1, 6, 8, 12, -2, 10, -3, -4, 14, 16, -5, -6, 18, 20, -7, -8, -9, -10,
];

/// Extra-bit probabilities for the six "category" tokens.
const PCAT: [&[u8]; 6] = [
    &[159],
    &[165, 145],
    &[173, 148, 140],
    &[176, 155, 140, 135],
    &[180, 157, 141, 134, 130],
    &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129],
];
/// Smallest value each category token stands for.
const CAT_BASE: [i32; 6] = [5, 7, 11, 19, 35, 67];

/// Subblock modes, in the enumeration order the trees and tables use.
const B_DC_PRED: usize = 0;
const B_TM_PRED: usize = 1;
const B_VE_PRED: usize = 2;
const B_HE_PRED: usize = 3;

/// The boolean entropy decoder of RFC 6386 section 7.3.
pub struct BoolDecoder<'a> {
    data: &'a [u8],
    at: usize,
    range: u32,
    value: u32,
    bit_count: i32,
}

impl<'a> BoolDecoder<'a> {
    /// Start decoding at the beginning of `data`.
    pub fn new(data: &'a [u8]) -> BoolDecoder<'a> {
        let mut value = 0u32;
        for i in 0..2 {
            value = (value << 8) | u32::from(data.get(i).copied().unwrap_or(0));
        }
        BoolDecoder {
            data,
            at: 2,
            range: 255,
            value,
            bit_count: 0,
        }
    }

    /// One bool with probability `prob / 256` of being zero.
    pub fn bool(&mut self, prob: u8) -> u32 {
        let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
        let big_split = split << 8;
        let bit = if self.value >= big_split {
            self.range -= split;
            self.value -= big_split;
            1
        } else {
            self.range = split;
            0
        };
        while self.range < 128 {
            self.value <<= 1;
            self.range <<= 1;
            self.bit_count += 1;
            if self.bit_count == 8 {
                self.bit_count = 0;
                // Past the end the decoder reads zeros; a truncated partition
                // then produces a wrong picture, never a panic.
                self.value |= u32::from(self.data.get(self.at).copied().unwrap_or(0));
                self.at += 1;
            }
        }
        bit
    }

    /// `n`-bit literal, high bit first, each bit at probability 128.
    pub fn literal(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) + self.bool(128);
        }
        v
    }

    /// A magnitude-and-sign field: `flag ? +/- L(n) : 0`.
    fn signed(&mut self, n: u32) -> i32 {
        let magnitude = self.literal(n) as i32;
        if self.bool(128) == 1 {
            -magnitude
        } else {
            magnitude
        }
    }

    /// Optional magnitude-and-sign field.
    fn maybe_signed(&mut self, n: u32) -> i32 {
        if self.bool(128) == 1 {
            self.signed(n)
        } else {
            0
        }
    }

    /// Walk a tree, reading one bool per node.
    fn tree(&mut self, tree: &[i8], probs: &[u8], start: usize) -> usize {
        let mut i = start;
        loop {
            let bit = self.bool(probs[i >> 1]) as usize;
            let next = tree[i + bit];
            if next > 0 {
                i = next as usize;
            } else {
                return (-next) as usize;
            }
        }
    }

    /// True once the decoder has read past the end of its partition.
    fn exhausted(&self) -> bool {
        self.at > self.data.len() + 2
    }
}

/// Per-segment quantizer factors: `[dc, ac]` for Y, Y2 and chroma.
#[derive(Clone, Copy, Default)]
struct Dequant {
    y: [i32; 2],
    y2: [i32; 2],
    uv: [i32; 2],
}

/// Everything the frame header set up.
struct Header {
    segmentation: bool,
    update_map: bool,
    segment_probs: [u8; 3],
    filter_simple: bool,
    filter_level: i32,
    sharpness: i32,
    delta_enabled: bool,
    ref_delta: [i32; 4],
    mode_delta: [i32; 4],
    segment_lf: [i32; 4],
    segment_lf_absolute: bool,
    dequant: [Dequant; 4],
    coeff_probs: [[[[u8; 11]; 3]; 8]; 4],
    skip_enabled: bool,
    skip_prob: u8,
}

/// What one macroblock decided, kept for the loop filter pass.
#[derive(Clone, Copy)]
struct MbInfo {
    y_mode: usize,
    segment: usize,
    has_coeffs: bool,
}

/// A decoded key frame's planes, cropped to the visible picture.
pub struct Frame {
    /// Visible width.
    pub width: u32,
    /// Visible height.
    pub height: u32,
    /// Luma plane, `stride_y` bytes per row.
    pub y: Vec<u8>,
    /// U plane, `stride_uv` bytes per row.
    pub u: Vec<u8>,
    /// V plane, `stride_uv` bytes per row.
    pub v: Vec<u8>,
    /// Bytes per luma row.
    pub stride_y: usize,
    /// Bytes per chroma row.
    pub stride_uv: usize,
}

/// Width, height and scale codes from the uncompressed chunk.
pub fn dimensions(data: &[u8]) -> Result<(u32, u32)> {
    let head = data
        .get(..10)
        .ok_or_else(|| Error::corrupt("VP8: frame shorter than its header"))?;
    let tag = u32::from(head[0]) | (u32::from(head[1]) << 8) | (u32::from(head[2]) << 16);
    if tag & 1 != 0 {
        return Err(Error::unsupported(
            "VP8 inter frame in a still image",
            "a still WebP carries one key frame; there is no reference to predict from",
        ));
    }
    if head[3..6] != [0x9d, 0x01, 0x2a] {
        return Err(Error::corrupt("VP8: bad key frame start code"));
    }
    let width = u32::from(u16::from_le_bytes([head[6], head[7]])) & 0x3fff;
    let height = u32::from(u16::from_le_bytes([head[8], head[9]])) & 0x3fff;
    Ok((width, height))
}

/// Decode one VP8 key frame.
pub fn decode(data: &[u8], limits: Limits) -> Result<Frame> {
    let (width, height) = dimensions(data)?;
    limits.check(width, height)?;
    let tag = u32::from(data[0]) | (u32::from(data[1]) << 8) | (u32::from(data[2]) << 16);
    let first_part_size = ((tag >> 5) & 0x7ffff) as usize;
    let body = data
        .get(10..)
        .ok_or_else(|| Error::corrupt("VP8: no frame body"))?;
    let first = body
        .get(..first_part_size)
        .ok_or_else(|| Error::corrupt("VP8: first partition runs past the chunk"))?;
    let mut bd = BoolDecoder::new(first);

    let _color_space = bd.literal(1);
    let _clamping = bd.literal(1);
    let (header, partition_count) = read_header(&mut bd)?;

    // Token partitions follow the first one, preceded by their sizes.
    let rest = &body[first_part_size..];
    let partitions = split_partitions(rest, partition_count)?;

    let mb_cols = (width as usize).div_ceil(16);
    let mb_rows = (height as usize).div_ceil(16);
    let stride_y = mb_cols * 16;
    let stride_uv = mb_cols * 8;
    let mut frame = Frame {
        width,
        height,
        y: vec![0; stride_y * mb_rows * 16],
        u: vec![0; stride_uv * mb_rows * 8],
        v: vec![0; stride_uv * mb_rows * 8],
        stride_y,
        stride_uv,
    };

    let mut token_decoders: Vec<BoolDecoder<'_>> =
        partitions.iter().map(|p| BoolDecoder::new(p)).collect();
    let mut above_bmodes = vec![B_DC_PRED; mb_cols * 4];
    let mut above_nz = vec![[0u8; 9]; mb_cols];
    let mut infos = Vec::with_capacity(mb_cols * mb_rows);
    let mut coeffs = [[0i16; 16]; 25];
    let mut bmodes = [B_DC_PRED; 16];

    for mb_y in 0..mb_rows {
        let mut left_bmodes = [B_DC_PRED; 4];
        let mut left_nz = [0u8; 9];
        for mb_x in 0..mb_cols {
            // Prediction record: segment, skip flag, then the modes.
            let segment = if header.segmentation && header.update_map {
                bd.tree(&SEGMENT_TREE, &header.segment_probs, 0)
            } else {
                0
            };
            let skip = header.skip_enabled && bd.bool(header.skip_prob) == 1;
            let y_mode = bd.tree(&KF_YMODE_TREE, &KF_YMODE_PROBS, 0);
            if y_mode == B_PRED {
                for by in 0..4 {
                    for bx in 0..4 {
                        let above = if by == 0 {
                            above_bmodes[mb_x * 4 + bx]
                        } else {
                            bmodes[(by - 1) * 4 + bx]
                        };
                        let left = if bx == 0 {
                            left_bmodes[by]
                        } else {
                            bmodes[by * 4 + bx - 1]
                        };
                        bmodes[by * 4 + bx] = bd.tree(&BMODE_TREE, &KF_BMODE_PROBS[above][left], 0);
                    }
                }
            } else {
                // A whole-macroblock mode still predicts the *next*
                // macroblock's subblock modes, through this equivalence.
                let implied = match y_mode {
                    V_PRED => B_VE_PRED,
                    H_PRED => B_HE_PRED,
                    TM_PRED => B_TM_PRED,
                    _ => B_DC_PRED,
                };
                bmodes = [implied; 16];
            }
            let uv_mode = bd.tree(&UV_MODE_TREE, &KF_UV_MODE_PROBS, 0);
            above_bmodes[mb_x * 4..mb_x * 4 + 4].copy_from_slice(&bmodes[12..16]);
            left_bmodes = [bmodes[3], bmodes[7], bmodes[11], bmodes[15]];

            // Residual record, from this row's token partition.
            let tokens = &mut token_decoders[mb_y % partitions.len()];
            let has_y2 = y_mode != B_PRED;
            let mut has_coeffs = false;
            for block in coeffs.iter_mut() {
                block.fill(0);
            }
            if skip {
                left_nz[..8].fill(0);
                above_nz[mb_x][..8].fill(0);
                if has_y2 {
                    left_nz[8] = 0;
                    above_nz[mb_x][8] = 0;
                }
            } else {
                has_coeffs = decode_residual(
                    tokens,
                    &header,
                    header.dequant[segment],
                    has_y2,
                    &mut coeffs,
                    &mut left_nz,
                    &mut above_nz[mb_x],
                )?;
            }

            reconstruct(
                &mut frame,
                mb_x,
                mb_y,
                mb_cols,
                mb_rows,
                y_mode,
                uv_mode,
                &bmodes,
                &mut coeffs,
                has_y2,
            );
            infos.push(MbInfo {
                y_mode,
                segment,
                has_coeffs,
            });
            if bd.exhausted() {
                return Err(Error::corrupt("VP8: first partition ran out of data"));
            }
        }
    }

    if header.filter_level > 0 {
        loop_filter(&mut frame, &header, &infos, mb_cols, mb_rows);
    }
    Ok(frame)
}

/// Read the frame header; returns it with the token partition count.
fn read_header(bd: &mut BoolDecoder<'_>) -> Result<(Header, usize)> {
    let mut header = Header {
        segmentation: false,
        update_map: false,
        segment_probs: [255; 3],
        filter_simple: false,
        filter_level: 0,
        sharpness: 0,
        delta_enabled: false,
        ref_delta: [0; 4],
        mode_delta: [0; 4],
        segment_lf: [0; 4],
        segment_lf_absolute: false,
        dequant: [Dequant::default(); 4],
        coeff_probs: DEFAULT_COEFF_PROBS,
        skip_enabled: false,
        skip_prob: 0,
    };
    let mut segment_quant = [0i32; 4];

    header.segmentation = bd.literal(1) == 1;
    if header.segmentation {
        header.update_map = bd.literal(1) == 1;
        let update_data = bd.literal(1) == 1;
        if update_data {
            header.segment_lf_absolute = bd.literal(1) == 1;
            for q in &mut segment_quant {
                *q = bd.maybe_signed(7);
            }
            for lf in &mut header.segment_lf {
                *lf = bd.maybe_signed(6);
            }
        }
        if header.update_map {
            for p in &mut header.segment_probs {
                *p = if bd.literal(1) == 1 {
                    bd.literal(8) as u8
                } else {
                    255
                };
            }
        }
    }

    header.filter_simple = bd.literal(1) == 1;
    header.filter_level = bd.literal(6) as i32;
    header.sharpness = bd.literal(3) as i32;
    header.delta_enabled = bd.literal(1) == 1;
    if header.delta_enabled && bd.literal(1) == 1 {
        for d in &mut header.ref_delta {
            if bd.literal(1) == 1 {
                *d = bd.signed(6);
            }
        }
        for d in &mut header.mode_delta {
            if bd.literal(1) == 1 {
                *d = bd.signed(6);
            }
        }
    }

    let partition_count = 1usize << bd.literal(2);

    // Quantizer indices, then the six factors per segment.
    let base = bd.literal(7) as i32;
    let y_dc = bd.maybe_signed(4);
    let y2_dc = bd.maybe_signed(4);
    let y2_ac = bd.maybe_signed(4);
    let uv_dc = bd.maybe_signed(4);
    let uv_ac = bd.maybe_signed(4);
    for segment in 0..4 {
        let q = if header.segmentation {
            if header.segment_lf_absolute {
                segment_quant[segment]
            } else {
                base + segment_quant[segment]
            }
        } else {
            base
        };
        header.dequant[segment] = Dequant {
            y: [dc_q(q + y_dc), ac_q(q)],
            y2: [dc_q(q + y2_dc) * 2, (ac_q(q + y2_ac) * 155 / 100).max(8)],
            uv: [dc_q(q + uv_dc).min(132), ac_q(q + uv_ac)],
        };
    }

    let _refresh_entropy = bd.literal(1);
    for i in 0..4 {
        for j in 0..8 {
            for k in 0..3 {
                for t in 0..11 {
                    if bd.bool(COEFF_UPDATE_PROBS[i][j][k][t]) == 1 {
                        header.coeff_probs[i][j][k][t] = bd.literal(8) as u8;
                    }
                }
            }
        }
    }
    header.skip_enabled = bd.literal(1) == 1;
    if header.skip_enabled {
        header.skip_prob = bd.literal(8) as u8;
    }
    Ok((header, partition_count))
}

fn clamp_q(q: i32) -> usize {
    q.clamp(0, 127) as usize
}

fn dc_q(q: i32) -> i32 {
    DC_QLOOKUP[clamp_q(q)]
}

fn ac_q(q: i32) -> i32 {
    AC_QLOOKUP[clamp_q(q)]
}

/// Split the token partitions, whose sizes precede them three bytes each.
fn split_partitions(data: &[u8], count: usize) -> Result<Vec<&[u8]>> {
    let table = (count - 1) * 3;
    let body = data
        .get(table..)
        .ok_or_else(|| Error::corrupt("VP8: partition size table runs past the chunk"))?;
    let mut parts = Vec::with_capacity(count);
    let mut at = 0usize;
    for i in 0..count - 1 {
        let size = usize::from(data[i * 3])
            | (usize::from(data[i * 3 + 1]) << 8)
            | (usize::from(data[i * 3 + 2]) << 16);
        let part = body
            .get(at..at + size)
            .ok_or_else(|| Error::corrupt("VP8: token partition runs past the chunk"))?;
        parts.push(part);
        at += size;
    }
    parts.push(&body[at..]);
    Ok(parts)
}

/// Decode one macroblock's 25 blocks of coefficients, dequantizing as it goes.
#[allow(clippy::too_many_arguments)]
fn decode_residual(
    bd: &mut BoolDecoder<'_>,
    header: &Header,
    dequant: Dequant,
    has_y2: bool,
    coeffs: &mut [[i16; 16]; 25],
    left_nz: &mut [u8; 9],
    above_nz: &mut [u8; 9],
) -> Result<bool> {
    let mut any = false;
    if has_y2 {
        let ctx = usize::from(left_nz[8]) + usize::from(above_nz[8]);
        let nonzero = decode_block(bd, header, 1, ctx, 0, dequant.y2, &mut coeffs[24])?;
        left_nz[8] = u8::from(nonzero);
        above_nz[8] = u8::from(nonzero);
        any |= nonzero;
    }
    let (plane, first) = if has_y2 {
        (0usize, 1usize)
    } else {
        (3usize, 0usize)
    };
    for by in 0..4 {
        for bx in 0..4 {
            let ctx = usize::from(left_nz[by]) + usize::from(above_nz[bx]);
            let nonzero = decode_block(
                bd,
                header,
                plane,
                ctx,
                first,
                dequant.y,
                &mut coeffs[by * 4 + bx],
            )?;
            left_nz[by] = u8::from(nonzero);
            above_nz[bx] = u8::from(nonzero);
            any |= nonzero;
        }
    }
    for (chroma, base) in [(0usize, 16usize), (1, 20)] {
        for by in 0..2 {
            for bx in 0..2 {
                let li = 4 + chroma * 2 + by;
                let ai = 4 + chroma * 2 + bx;
                let ctx = usize::from(left_nz[li]) + usize::from(above_nz[ai]);
                let nonzero = decode_block(
                    bd,
                    header,
                    2,
                    ctx,
                    0,
                    dequant.uv,
                    &mut coeffs[base + by * 2 + bx],
                )?;
                left_nz[li] = u8::from(nonzero);
                above_nz[ai] = u8::from(nonzero);
                any |= nonzero;
            }
        }
    }
    if bd.exhausted() {
        return Err(Error::corrupt("VP8: token partition ran out of data"));
    }
    Ok(any)
}

/// One 4x4 block of tokens, written out dequantized in raster order.
fn decode_block(
    bd: &mut BoolDecoder<'_>,
    header: &Header,
    plane: usize,
    ctx: usize,
    first: usize,
    factors: [i32; 2],
    out: &mut [i16; 16],
) -> Result<bool> {
    let mut ctx = ctx;
    let mut nonzero = false;
    let mut index = first;
    let mut skip_eob = false;
    while index < 16 {
        let probs = &header.coeff_probs[plane][COEFF_BANDS[index]][ctx];
        let token = bd.tree(&COEFF_TREE, probs, if skip_eob { 2 } else { 0 });
        if token == 11 {
            break;
        }
        let value = match token {
            0 => 0,
            1..=4 => token as i32,
            _ => {
                let cat = token - 5;
                let mut extra = 0i32;
                for &p in PCAT[cat] {
                    extra = 2 * extra + bd.bool(p) as i32;
                }
                CAT_BASE[cat] + extra
            }
        };
        ctx = match value {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        skip_eob = value == 0;
        if value != 0 {
            nonzero = true;
            let signed = if bd.bool(128) == 1 { -value } else { value };
            let factor = if index == 0 { factors[0] } else { factors[1] };
            out[ZIGZAG[index]] = (signed * factor).clamp(-32768, 32767) as i16;
        }
        index += 1;
    }
    Ok(nonzero)
}

/// Inverse Walsh-Hadamard: the Y2 block's values become the Y blocks' DCs.
fn inverse_wht(input: &[i16; 16], coeffs: &mut [[i16; 16]; 25]) {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let a1 = i32::from(input[i]) + i32::from(input[12 + i]);
        let b1 = i32::from(input[4 + i]) + i32::from(input[8 + i]);
        let c1 = i32::from(input[4 + i]) - i32::from(input[8 + i]);
        let d1 = i32::from(input[i]) - i32::from(input[12 + i]);
        tmp[i] = a1 + b1;
        tmp[4 + i] = c1 + d1;
        tmp[8 + i] = a1 - b1;
        tmp[12 + i] = d1 - c1;
    }
    for i in 0..4 {
        let a1 = tmp[i * 4] + tmp[i * 4 + 3];
        let b1 = tmp[i * 4 + 1] + tmp[i * 4 + 2];
        let c1 = tmp[i * 4 + 1] - tmp[i * 4 + 2];
        let d1 = tmp[i * 4] - tmp[i * 4 + 3];
        coeffs[i * 4][0] = ((a1 + b1 + 3) >> 3) as i16;
        coeffs[i * 4 + 1][0] = ((c1 + d1 + 3) >> 3) as i16;
        coeffs[i * 4 + 2][0] = ((a1 - b1 + 3) >> 3) as i16;
        coeffs[i * 4 + 3][0] = ((d1 - c1 + 3) >> 3) as i16;
    }
}

const COS_PI8_SQRT2_MINUS1: i32 = 20091;
const SIN_PI8_SQRT2: i32 = 35468;

/// The 4x4 inverse DCT of RFC 6386 section 14.4, bit-exact as specified.
fn inverse_dct(block: &[i16; 16]) -> [i32; 16] {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let ip = |k: usize| i32::from(block[k * 4 + i]);
        let a1 = ip(0) + ip(2);
        let b1 = ip(0) - ip(2);
        let t1 = (ip(1) * SIN_PI8_SQRT2) >> 16;
        let t2 = ip(3) + ((ip(3) * COS_PI8_SQRT2_MINUS1) >> 16);
        let c1 = t1 - t2;
        let t1 = ip(1) + ((ip(1) * COS_PI8_SQRT2_MINUS1) >> 16);
        let t2 = (ip(3) * SIN_PI8_SQRT2) >> 16;
        let d1 = t1 + t2;
        tmp[i] = a1 + d1;
        tmp[12 + i] = a1 - d1;
        tmp[4 + i] = b1 + c1;
        tmp[8 + i] = b1 - c1;
    }
    let mut out = [0i32; 16];
    for i in 0..4 {
        let ip = |k: usize| tmp[i * 4 + k];
        let a1 = ip(0) + ip(2);
        let b1 = ip(0) - ip(2);
        let t1 = (ip(1) * SIN_PI8_SQRT2) >> 16;
        let t2 = ip(3) + ((ip(3) * COS_PI8_SQRT2_MINUS1) >> 16);
        let c1 = t1 - t2;
        let t1 = ip(1) + ((ip(1) * COS_PI8_SQRT2_MINUS1) >> 16);
        let t2 = (ip(3) * SIN_PI8_SQRT2) >> 16;
        let d1 = t1 + t2;
        out[i * 4] = (a1 + d1 + 4) >> 3;
        out[i * 4 + 3] = (a1 - d1 + 4) >> 3;
        out[i * 4 + 1] = (b1 + c1 + 4) >> 3;
        out[i * 4 + 2] = (b1 - c1 + 4) >> 3;
    }
    out
}

/// The macroblock's working buffer: one row of "above" pixels (with the four
/// above-right ones a subblock may reach for) and one column of "left" ones,
/// surrounding the 16x16 luma and 8x8 chroma blocks being built.
struct Work {
    y: [[u8; 21]; 17],
    u: [[u8; 9]; 9],
    v: [[u8; 9]; 9],
}

#[allow(clippy::too_many_arguments)]
fn reconstruct(
    frame: &mut Frame,
    mb_x: usize,
    mb_y: usize,
    mb_cols: usize,
    _mb_rows: usize,
    y_mode: usize,
    uv_mode: usize,
    bmodes: &[usize; 16],
    coeffs: &mut [[i16; 16]; 25],
    has_y2: bool,
) {
    if has_y2 {
        let y2 = coeffs[24];
        inverse_wht(&y2, coeffs);
    }
    let mut work = Work {
        y: [[0; 21]; 17],
        u: [[0; 9]; 9],
        v: [[0; 9]; 9],
    };
    load_edges(frame, &mut work, mb_x, mb_y, mb_cols);

    if y_mode == B_PRED {
        for by in 0..4 {
            for bx in 0..4 {
                predict_subblock(&mut work, bx, by, bmodes[by * 4 + bx]);
                add_residual_4x4(&mut work.y, by * 4 + 1, bx * 4 + 1, &coeffs[by * 4 + bx]);
            }
        }
    } else {
        predict_luma_16x16(&mut work, y_mode, mb_x, mb_y);
        for by in 0..4 {
            for bx in 0..4 {
                add_residual_4x4(&mut work.y, by * 4 + 1, bx * 4 + 1, &coeffs[by * 4 + bx]);
            }
        }
    }
    predict_chroma(&mut work, uv_mode, mb_x, mb_y);
    for by in 0..2 {
        for bx in 0..2 {
            add_residual_4x4_c(
                &mut work.u,
                by * 4 + 1,
                bx * 4 + 1,
                &coeffs[16 + by * 2 + bx],
            );
            add_residual_4x4_c(
                &mut work.v,
                by * 4 + 1,
                bx * 4 + 1,
                &coeffs[20 + by * 2 + bx],
            );
        }
    }
    store_macroblock(frame, &work, mb_x, mb_y);
}

/// Fill the working buffer's borders from the already-reconstructed frame,
/// using 127 above the picture and 129 to its left as the format requires.
fn load_edges(frame: &Frame, work: &mut Work, mb_x: usize, mb_y: usize, mb_cols: usize) {
    let sy = frame.stride_y;
    let suv = frame.stride_uv;
    // Luma above row plus four above-right pixels.
    if mb_y == 0 {
        work.y[0] = [127; 21];
    } else {
        let row = (mb_y * 16 - 1) * sy;
        work.y[0][0] = if mb_x == 0 {
            129
        } else {
            frame.y[row + mb_x * 16 - 1]
        };
        for i in 0..16 {
            work.y[0][1 + i] = frame.y[row + mb_x * 16 + i];
        }
        for i in 0..4 {
            work.y[0][17 + i] = if mb_x + 1 < mb_cols {
                frame.y[row + mb_x * 16 + 16 + i]
            } else {
                // The rightmost macroblock reuses the last visible pixel of
                // the row above, per section 12.3.
                frame.y[row + mb_x * 16 + 15]
            };
        }
    }
    for r in 0..16 {
        work.y[r + 1][0] = if mb_x == 0 {
            129
        } else {
            frame.y[(mb_y * 16 + r) * sy + mb_x * 16 - 1]
        };
    }
    // Chroma above rows and left columns.
    for (plane, buf) in [(&frame.u, &mut work.u), (&frame.v, &mut work.v)] {
        if mb_y == 0 {
            buf[0] = [127; 9];
        } else {
            let row = (mb_y * 8 - 1) * suv;
            buf[0][0] = if mb_x == 0 {
                129
            } else {
                plane[row + mb_x * 8 - 1]
            };
            for i in 0..8 {
                buf[0][1 + i] = plane[row + mb_x * 8 + i];
            }
        }
        for r in 0..8 {
            buf[r + 1][0] = if mb_x == 0 {
                129
            } else {
                plane[(mb_y * 8 + r) * suv + mb_x * 8 - 1]
            };
        }
    }
}

fn store_macroblock(frame: &mut Frame, work: &Work, mb_x: usize, mb_y: usize) {
    let sy = frame.stride_y;
    let suv = frame.stride_uv;
    for r in 0..16 {
        let dst = (mb_y * 16 + r) * sy + mb_x * 16;
        frame.y[dst..dst + 16].copy_from_slice(&work.y[r + 1][1..17]);
    }
    for r in 0..8 {
        let dst = (mb_y * 8 + r) * suv + mb_x * 8;
        frame.u[dst..dst + 8].copy_from_slice(&work.u[r + 1][1..9]);
        frame.v[dst..dst + 8].copy_from_slice(&work.v[r + 1][1..9]);
    }
}

fn add_residual_4x4(buf: &mut [[u8; 21]; 17], row: usize, col: usize, block: &[i16; 16]) {
    if block.iter().all(|&c| c == 0) {
        return;
    }
    let residue = inverse_dct(block);
    for r in 0..4 {
        for c in 0..4 {
            let v = i32::from(buf[row + r][col + c]) + residue[r * 4 + c];
            buf[row + r][col + c] = v.clamp(0, 255) as u8;
        }
    }
}

fn add_residual_4x4_c(buf: &mut [[u8; 9]; 9], row: usize, col: usize, block: &[i16; 16]) {
    if block.iter().all(|&c| c == 0) {
        return;
    }
    let residue = inverse_dct(block);
    for r in 0..4 {
        for c in 0..4 {
            let v = i32::from(buf[row + r][col + c]) + residue[r * 4 + c];
            buf[row + r][col + c] = v.clamp(0, 255) as u8;
        }
    }
}

/// DC prediction's rounded average, over whichever edges exist.
fn dc_value(above: &[u8], left: &[u8], have_above: bool, have_left: bool) -> u8 {
    let mut sum = 0u32;
    let mut count = 0u32;
    if have_above {
        sum += above.iter().map(|&p| u32::from(p)).sum::<u32>();
        count += above.len() as u32;
    }
    if have_left {
        sum += left.iter().map(|&p| u32::from(p)).sum::<u32>();
        count += left.len() as u32;
    }
    if count == 0 {
        return 128;
    }
    let shift = count.trailing_zeros();
    ((sum + (1 << (shift - 1))) >> shift) as u8
}

fn predict_luma_16x16(work: &mut Work, mode: usize, mb_x: usize, mb_y: usize) {
    let above: Vec<u8> = work.y[0][1..17].to_vec();
    let left: Vec<u8> = (0..16).map(|r| work.y[r + 1][0]).collect();
    let corner = work.y[0][0];
    let dc = dc_value(&above, &left, mb_y > 0, mb_x > 0);
    for r in 0..16 {
        for c in 0..16 {
            work.y[r + 1][c + 1] = match mode {
                V_PRED => above[c],
                H_PRED => left[r],
                TM_PRED => (i32::from(left[r]) + i32::from(above[c]) - i32::from(corner))
                    .clamp(0, 255) as u8,
                DC_PRED => dc,
                _ => dc,
            };
        }
    }
}

fn predict_chroma(work: &mut Work, mode: usize, mb_x: usize, mb_y: usize) {
    for buf in [&mut work.u, &mut work.v] {
        let above: Vec<u8> = buf[0][1..9].to_vec();
        let left: Vec<u8> = (0..8).map(|r| buf[r + 1][0]).collect();
        let corner = buf[0][0];
        let dc = dc_value(&above, &left, mb_y > 0, mb_x > 0);
        for r in 0..8 {
            for c in 0..8 {
                buf[r + 1][c + 1] = match mode {
                    V_PRED => above[c],
                    H_PRED => left[r],
                    TM_PRED => (i32::from(left[r]) + i32::from(above[c]) - i32::from(corner))
                        .clamp(0, 255) as u8,
                    DC_PRED => dc,
                    _ => dc,
                };
            }
        }
    }
}

fn avg3(x: u8, y: u8, z: u8) -> u8 {
    ((u32::from(x) + 2 * u32::from(y) + u32::from(z) + 2) >> 2) as u8
}

fn avg2(x: u8, y: u8) -> u8 {
    ((u32::from(x) + u32::from(y) + 1) >> 1) as u8
}

/// The ten 4x4 subblock predictions of section 12.3.
fn predict_subblock(work: &mut Work, bx: usize, by: usize, mode: usize) {
    let row = by * 4;
    let col = bx * 4;
    // A[-1] is P; A[0..8] the above row and its four right-hand neighbours.
    let mut a = [0u8; 9];
    a[0] = work.y[row][col];
    for i in 0..8 {
        // Subblocks on the macroblock's right edge take their above-right from
        // row -1 whatever their own row is: the pixels to their upper right
        // have not been reconstructed yet.
        a[1 + i] = if bx == 3 && i >= 4 {
            work.y[0][17 + (i - 4)]
        } else {
            work.y[row][col + 1 + i]
        };
    }
    let l = [
        work.y[row + 1][col],
        work.y[row + 2][col],
        work.y[row + 3][col],
        work.y[row + 4][col],
    ];
    let p = a[0];
    let above = &a[1..9];
    // E[0..9]: the left column reversed, then P, then the above row.
    let e = [
        l[3], l[2], l[1], l[0], p, above[0], above[1], above[2], above[3],
    ];
    let mut b = [[0u8; 4]; 4];
    match mode {
        B_DC_PRED => {
            let mut v = 4u32;
            for i in 0..4 {
                v += u32::from(above[i]) + u32::from(l[i]);
            }
            let v = (v >> 3) as u8;
            b = [[v; 4]; 4];
        }
        B_TM_PRED => {
            for r in 0..4 {
                for c in 0..4 {
                    b[r][c] =
                        (i32::from(l[r]) + i32::from(above[c]) - i32::from(p)).clamp(0, 255) as u8;
                }
            }
        }
        B_VE_PRED => {
            for c in 0..4 {
                let v = avg3(
                    if c == 0 { p } else { above[c - 1] },
                    above[c],
                    above[c + 1],
                );
                for r in 0..4 {
                    b[r][c] = v;
                }
            }
        }
        B_HE_PRED => {
            let values = [
                avg3(p, l[0], l[1]),
                avg3(l[0], l[1], l[2]),
                avg3(l[1], l[2], l[3]),
                avg3(l[2], l[3], l[3]),
            ];
            for r in 0..4 {
                b[r] = [values[r]; 4];
            }
        }
        4 => {
            // B_LD_PRED: 45 degrees down-left.
            let d = |i: usize| avg3(above[i - 1], above[i], above[i + 1]);
            b[0][0] = d(1);
            b[0][1] = d(2);
            b[1][0] = d(2);
            b[0][2] = d(3);
            b[1][1] = d(3);
            b[2][0] = d(3);
            b[0][3] = d(4);
            b[1][2] = d(4);
            b[2][1] = d(4);
            b[3][0] = d(4);
            b[1][3] = d(5);
            b[2][2] = d(5);
            b[3][1] = d(5);
            b[2][3] = d(6);
            b[3][2] = d(6);
            b[3][3] = avg3(above[6], above[7], above[7]);
        }
        5 => {
            // B_RD_PRED: 45 degrees down-right.
            let d = |i: usize| avg3(e[i - 1], e[i], e[i + 1]);
            b[3][0] = d(1);
            b[3][1] = d(2);
            b[2][0] = d(2);
            b[3][2] = d(3);
            b[2][1] = d(3);
            b[1][0] = d(3);
            b[3][3] = d(4);
            b[2][2] = d(4);
            b[1][1] = d(4);
            b[0][0] = d(4);
            b[2][3] = d(5);
            b[1][2] = d(5);
            b[0][1] = d(5);
            b[1][3] = d(6);
            b[0][2] = d(6);
            b[0][3] = d(7);
        }
        6 => {
            // B_VR_PRED.
            let d3 = |i: usize| avg3(e[i - 1], e[i], e[i + 1]);
            let d2 = |i: usize| avg2(e[i], e[i + 1]);
            b[3][0] = d3(2);
            b[2][0] = d3(3);
            b[3][1] = d3(4);
            b[1][0] = d3(4);
            b[2][1] = d2(4);
            b[0][0] = d2(4);
            b[3][2] = d3(5);
            b[1][1] = d3(5);
            b[2][2] = d2(5);
            b[0][1] = d2(5);
            b[3][3] = d3(6);
            b[1][2] = d3(6);
            b[2][3] = d2(6);
            b[0][2] = d2(6);
            b[1][3] = d3(7);
            b[0][3] = d2(7);
        }
        7 => {
            // B_VL_PRED.
            let d3 = |i: usize| avg3(above[i - 1], above[i], above[i + 1]);
            let d2 = |i: usize| avg2(above[i], above[i + 1]);
            b[0][0] = d2(0);
            b[1][0] = d3(1);
            b[2][0] = d2(1);
            b[0][1] = d2(1);
            b[1][1] = d3(2);
            b[3][0] = d3(2);
            b[2][1] = d2(2);
            b[0][2] = d2(2);
            b[3][1] = d3(3);
            b[1][2] = d3(3);
            b[2][2] = d2(3);
            b[0][3] = d2(3);
            b[3][2] = d3(4);
            b[1][3] = d3(4);
            b[2][3] = d3(5);
            b[3][3] = d3(6);
        }
        8 => {
            // B_HD_PRED.
            let d3 = |i: usize| avg3(e[i - 1], e[i], e[i + 1]);
            let d2 = |i: usize| avg2(e[i], e[i + 1]);
            b[3][0] = d2(0);
            b[3][1] = d3(1);
            b[2][0] = d2(1);
            b[3][2] = d2(1);
            b[2][1] = d3(2);
            b[3][3] = d3(2);
            b[2][2] = d2(2);
            b[1][0] = d2(2);
            b[2][3] = d3(3);
            b[1][1] = d3(3);
            b[1][2] = d2(3);
            b[0][0] = d2(3);
            b[1][3] = d3(4);
            b[0][1] = d3(4);
            b[0][2] = d3(5);
            b[0][3] = d3(6);
        }
        _ => {
            // B_HU_PRED.
            b[0][0] = avg2(l[0], l[1]);
            b[0][1] = avg3(l[0], l[1], l[2]);
            b[0][2] = avg2(l[1], l[2]);
            b[1][0] = avg2(l[1], l[2]);
            b[0][3] = avg3(l[1], l[2], l[3]);
            b[1][1] = avg3(l[1], l[2], l[3]);
            b[1][2] = avg2(l[2], l[3]);
            b[2][0] = avg2(l[2], l[3]);
            b[1][3] = avg3(l[2], l[3], l[3]);
            b[2][1] = avg3(l[2], l[3], l[3]);
            b[2][2] = l[3];
            b[2][3] = l[3];
            b[3][0] = l[3];
            b[3][1] = l[3];
            b[3][2] = l[3];
            b[3][3] = l[3];
        }
    }
    for r in 0..4 {
        for c in 0..4 {
            work.y[row + 1 + r][col + 1 + c] = b[r][c];
        }
    }
}

// ---- Loop filter (section 15) --------------------------------------------

fn c8(v: i32) -> i32 {
    v.clamp(-128, 127)
}

fn u2s(v: u8) -> i32 {
    i32::from(v) - 128
}

fn s2u(v: i32) -> u8 {
    (c8(v) + 128) as u8
}

/// The shared 2-or-4-tap adjustment; returns the value the wider filters need.
fn common_adjust(use_outer: bool, plane: &mut [u8], q0: usize, step: usize) -> i32 {
    let p1 = u2s(plane[q0 - 2 * step]);
    let p0 = u2s(plane[q0 - step]);
    let q0v = u2s(plane[q0]);
    let q1 = u2s(plane[q0 + step]);
    let a = c8((if use_outer { c8(p1 - q1) } else { 0 }) + 3 * (q0v - p0));
    let b = c8(a + 3) >> 3;
    let a = c8(a + 4) >> 3;
    plane[q0] = s2u(q0v - a);
    plane[q0 - step] = s2u(p0 + b);
    a
}

fn simple_segment(edge_limit: i32, plane: &mut [u8], q0: usize, step: usize) {
    let p1 = i32::from(plane[q0 - 2 * step]);
    let p0 = i32::from(plane[q0 - step]);
    let q0v = i32::from(plane[q0]);
    let q1 = i32::from(plane[q0 + step]);
    if (p0 - q0v).abs() * 2 + (p1 - q1).abs() / 2 <= edge_limit {
        common_adjust(true, plane, q0, step);
    }
}

fn filter_yes(interior: i32, edge: i32, plane: &[u8], q0: usize, step: usize) -> bool {
    let at = |k: i32| i32::from(plane[(q0 as i32 + k * step as i32) as usize]);
    let (p3, p2, p1, p0) = (at(-4), at(-3), at(-2), at(-1));
    let (q0v, q1, q2, q3) = (at(0), at(1), at(2), at(3));
    (p0 - q0v).abs() * 2 + (p1 - q1).abs() / 2 <= edge
        && (p3 - p2).abs() <= interior
        && (p2 - p1).abs() <= interior
        && (p1 - p0).abs() <= interior
        && (q3 - q2).abs() <= interior
        && (q2 - q1).abs() <= interior
        && (q1 - q0v).abs() <= interior
}

fn hev(threshold: i32, plane: &[u8], q0: usize, step: usize) -> bool {
    let at = |k: i32| i32::from(plane[(q0 as i32 + k * step as i32) as usize]);
    (at(-2) - at(-1)).abs() > threshold || (at(1) - at(0)).abs() > threshold
}

fn subblock_filter(
    hev_threshold: i32,
    interior: i32,
    edge: i32,
    plane: &mut [u8],
    q0: usize,
    step: usize,
) {
    if !filter_yes(interior, edge, plane, q0, step) {
        return;
    }
    let high = hev(hev_threshold, plane, q0, step);
    let a = (common_adjust(high, plane, q0, step) + 1) >> 1;
    if !high {
        let q1 = u2s(plane[q0 + step]);
        let p1 = u2s(plane[q0 - 2 * step]);
        plane[q0 + step] = s2u(q1 - a);
        plane[q0 - 2 * step] = s2u(p1 + a);
    }
}

fn mb_filter(
    hev_threshold: i32,
    interior: i32,
    edge: i32,
    plane: &mut [u8],
    q0: usize,
    step: usize,
) {
    if !filter_yes(interior, edge, plane, q0, step) {
        return;
    }
    if hev(hev_threshold, plane, q0, step) {
        common_adjust(true, plane, q0, step);
        return;
    }
    let p2 = u2s(plane[q0 - 3 * step]);
    let p1 = u2s(plane[q0 - 2 * step]);
    let p0 = u2s(plane[q0 - step]);
    let q0v = u2s(plane[q0]);
    let q1 = u2s(plane[q0 + step]);
    let q2 = u2s(plane[q0 + 2 * step]);
    let w = c8(c8(p1 - q1) + 3 * (q0v - p0));
    let a = c8((27 * w + 63) >> 7);
    plane[q0] = s2u(q0v - a);
    plane[q0 - step] = s2u(p0 + a);
    let a = c8((18 * w + 63) >> 7);
    plane[q0 + step] = s2u(q1 - a);
    plane[q0 - 2 * step] = s2u(p1 + a);
    let a = c8((9 * w + 63) >> 7);
    plane[q0 + 2 * step] = s2u(q2 - a);
    plane[q0 - 3 * step] = s2u(p2 + a);
}

/// Per-macroblock filter strength, exactly as section 15.4 derives it.
fn filter_parameters(header: &Header, info: &MbInfo) -> (i32, i32, i32) {
    let mut level = header.filter_level;
    if header.segmentation {
        level = if header.segment_lf_absolute {
            header.segment_lf[info.segment]
        } else {
            level + header.segment_lf[info.segment]
        };
    }
    level = level.clamp(0, 63);
    if header.delta_enabled {
        // A key frame's every macroblock references the current frame, so the
        // reference delta is always index 0 and the mode delta only applies to
        // B_PRED.
        level += header.ref_delta[0];
        if info.y_mode == B_PRED {
            level += header.mode_delta[0];
        }
    }
    let level = level.clamp(0, 63);
    let mut interior = level;
    if header.sharpness > 0 {
        interior >>= if header.sharpness > 4 { 2 } else { 1 };
        interior = interior.min(9 - header.sharpness);
    }
    interior = interior.max(1);
    let mut hev_threshold = i32::from(level >= 15);
    if level >= 40 {
        hev_threshold += 1;
    }
    (level, interior, hev_threshold)
}

fn loop_filter(
    frame: &mut Frame,
    header: &Header,
    infos: &[MbInfo],
    mb_cols: usize,
    mb_rows: usize,
) {
    let sy = frame.stride_y;
    let suv = frame.stride_uv;
    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let info = infos[mb_y * mb_cols + mb_x];
            let (level, interior, hev_threshold) = filter_parameters(header, &info);
            if level == 0 {
                continue;
            }
            let mb_edge = (level + 2) * 2 + interior;
            let sub_edge = level * 2 + interior;
            // Interior edges are skipped where the macroblock has no residue
            // and predicts as a whole; nothing there can have a block edge.
            let skip_interior = !info.has_coeffs && info.y_mode != B_PRED;
            let (yx, yy) = (mb_x * 16, mb_y * 16);
            let (cx, cy) = (mb_x * 8, mb_y * 8);

            if header.filter_simple {
                if mb_x > 0 {
                    for r in 0..16 {
                        simple_segment(mb_edge, &mut frame.y, (yy + r) * sy + yx, 1);
                    }
                }
                if !skip_interior {
                    for c in [4, 8, 12] {
                        for r in 0..16 {
                            simple_segment(sub_edge, &mut frame.y, (yy + r) * sy + yx + c, 1);
                        }
                    }
                }
                if mb_y > 0 {
                    for c in 0..16 {
                        simple_segment(mb_edge, &mut frame.y, yy * sy + yx + c, sy);
                    }
                }
                if !skip_interior {
                    for r in [4, 8, 12] {
                        for c in 0..16 {
                            simple_segment(sub_edge, &mut frame.y, (yy + r) * sy + yx + c, sy);
                        }
                    }
                }
                continue;
            }

            if mb_x > 0 {
                for r in 0..16 {
                    mb_filter(
                        hev_threshold,
                        interior,
                        mb_edge,
                        &mut frame.y,
                        (yy + r) * sy + yx,
                        1,
                    );
                }
                for r in 0..8 {
                    mb_filter(
                        hev_threshold,
                        interior,
                        mb_edge,
                        &mut frame.u,
                        (cy + r) * suv + cx,
                        1,
                    );
                    mb_filter(
                        hev_threshold,
                        interior,
                        mb_edge,
                        &mut frame.v,
                        (cy + r) * suv + cx,
                        1,
                    );
                }
            }
            if !skip_interior {
                for c in [4, 8, 12] {
                    for r in 0..16 {
                        subblock_filter(
                            hev_threshold,
                            interior,
                            sub_edge,
                            &mut frame.y,
                            (yy + r) * sy + yx + c,
                            1,
                        );
                    }
                }
                for r in 0..8 {
                    subblock_filter(
                        hev_threshold,
                        interior,
                        sub_edge,
                        &mut frame.u,
                        (cy + r) * suv + cx + 4,
                        1,
                    );
                    subblock_filter(
                        hev_threshold,
                        interior,
                        sub_edge,
                        &mut frame.v,
                        (cy + r) * suv + cx + 4,
                        1,
                    );
                }
            }
            if mb_y > 0 {
                for c in 0..16 {
                    mb_filter(
                        hev_threshold,
                        interior,
                        mb_edge,
                        &mut frame.y,
                        yy * sy + yx + c,
                        sy,
                    );
                }
                for c in 0..8 {
                    mb_filter(
                        hev_threshold,
                        interior,
                        mb_edge,
                        &mut frame.u,
                        cy * suv + cx + c,
                        suv,
                    );
                    mb_filter(
                        hev_threshold,
                        interior,
                        mb_edge,
                        &mut frame.v,
                        cy * suv + cx + c,
                        suv,
                    );
                }
            }
            if !skip_interior {
                for r in [4, 8, 12] {
                    for c in 0..16 {
                        subblock_filter(
                            hev_threshold,
                            interior,
                            sub_edge,
                            &mut frame.y,
                            (yy + r) * sy + yx + c,
                            sy,
                        );
                    }
                }
                for c in 0..8 {
                    subblock_filter(
                        hev_threshold,
                        interior,
                        sub_edge,
                        &mut frame.u,
                        (cy + 4) * suv + cx + c,
                        suv,
                    );
                    subblock_filter(
                        hev_threshold,
                        interior,
                        sub_edge,
                        &mut frame.v,
                        (cy + 4) * suv + cx + c,
                        suv,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoder of RFC 6386 section 7.3, so the decoder can be checked
    /// against the thing it has to invert rather than against an assumption
    /// about what the bits look like.
    struct BoolEncoder {
        out: Vec<u8>,
        range: u32,
        bottom: u32,
        bit_count: i32,
    }

    impl BoolEncoder {
        fn new() -> BoolEncoder {
            BoolEncoder {
                out: Vec::new(),
                range: 255,
                bottom: 0,
                bit_count: 24,
            }
        }

        fn carry(&mut self) {
            let mut i = self.out.len();
            while i > 0 {
                i -= 1;
                if self.out[i] == 255 {
                    self.out[i] = 0;
                } else {
                    self.out[i] += 1;
                    return;
                }
            }
        }

        fn write(&mut self, prob: u8, value: u32) {
            let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
            if value != 0 {
                self.bottom += split;
                self.range -= split;
            } else {
                self.range = split;
            }
            while self.range < 128 {
                self.range <<= 1;
                if self.bottom & (1 << 31) != 0 {
                    self.carry();
                }
                self.bottom <<= 1;
                self.bit_count -= 1;
                if self.bit_count == 0 {
                    self.out.push((self.bottom >> 24) as u8);
                    self.bottom &= (1 << 24) - 1;
                    self.bit_count = 8;
                }
            }
        }

        fn literal(&mut self, n: u32, value: u32) {
            for i in (0..n).rev() {
                self.write(128, (value >> i) & 1);
            }
        }

        fn finish(mut self) -> Vec<u8> {
            let mut c = self.bit_count;
            let mut v = self.bottom;
            if v & (1u32 << (32 - c)) != 0 {
                self.carry();
            }
            v <<= c & 7;
            c >>= 3;
            while c > 0 {
                v <<= 8;
                c -= 1;
            }
            for _ in 0..4 {
                self.out.push((v >> 24) as u8);
                v <<= 8;
            }
            self.out
        }
    }

    #[test]
    fn the_bool_decoder_inverts_the_specification_encoder() {
        let probs = [128u8, 1, 255, 200, 32, 145];
        let bits: Vec<u32> = (0..600)
            .map(|i: u32| (i.wrapping_mul(2_654_435_761)) >> 31)
            .collect();
        let mut encoder = BoolEncoder::new();
        for (i, &bit) in bits.iter().enumerate() {
            encoder.write(probs[i % probs.len()], bit);
        }
        encoder.literal(8, 0xb2);
        encoder.literal(12, 0x4d3);
        let data = encoder.finish();

        let mut bd = BoolDecoder::new(&data);
        for (i, &bit) in bits.iter().enumerate() {
            assert_eq!(bd.bool(probs[i % probs.len()]), bit, "bool {i}");
        }
        assert_eq!(bd.literal(8), 0xb2);
        assert_eq!(bd.literal(12), 0x4d3);
    }

    #[test]
    fn trees_decode_their_leaves() {
        // A stream of zero bits walks the leftmost branch of each tree.
        let data = [0u8; 8];
        let mut bd = BoolDecoder::new(&data);
        assert_eq!(bd.tree(&KF_YMODE_TREE, &KF_YMODE_PROBS, 0), B_PRED);
        let mut bd = BoolDecoder::new(&data);
        assert_eq!(bd.tree(&UV_MODE_TREE, &KF_UV_MODE_PROBS, 0), DC_PRED);
        let mut bd = BoolDecoder::new(&data);
        assert_eq!(bd.tree(&BMODE_TREE, &KF_BMODE_PROBS[0][0], 0), B_DC_PRED);
        let mut bd = BoolDecoder::new(&data);
        assert_eq!(
            bd.tree(&COEFF_TREE, &[128; 11], 0),
            11,
            "eob is the 0 branch"
        );
    }

    #[test]
    fn the_inverse_dct_of_a_dc_only_block_is_flat() {
        let mut block = [0i16; 16];
        block[0] = 80;
        let out = inverse_dct(&block);
        for v in out {
            assert_eq!(v, 10, "DC 80 spreads to (80 + 4) >> 3 everywhere");
        }
    }

    #[test]
    fn the_inverse_wht_matches_its_specification() {
        let mut input = [0i16; 16];
        input[0] = 64;
        let mut coeffs = [[0i16; 16]; 25];
        inverse_wht(&input, &mut coeffs);
        for block in coeffs.iter().take(16) {
            assert_eq!(block[0], 8, "a flat WHT spreads its DC over the 16 blocks");
        }
    }

    #[test]
    fn dequant_factors_follow_the_scaling_rules() {
        assert_eq!(dc_q(0), 4);
        assert_eq!(ac_q(127), 284);
        // Y2 AC is scaled by 155/100 with a floor of 8, chroma DC caps at 132.
        assert_eq!((ac_q(0) * 155 / 100).max(8), 8);
        assert_eq!(dc_q(127).min(132), 132);
    }
}
