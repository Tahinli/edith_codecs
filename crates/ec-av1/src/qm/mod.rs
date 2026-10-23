//! AV1 inverse quantisation matrices: spec 5.9.12's `using_qmatrix` +
//! `qm_y`/`qm_u`/`qm_v` and the per-position `dqv` scale of spec 7.12.3,
//! ported bug-for-bug from libaom (`av1/common/quant_common.c`'s
//! `iwt_matrix_ref` + `av1_qm_init` + `av1_get_iqmatrix`,
//! `av1/decoder/decodetxb.c`'s `get_dqv`). lane-av1-qmatrix.
//!
//! The shape of the port, and why each piece looks like it does:
//!
//! - The parsed 4-bit `qm_y`/`qm_u`/`qm_v` ARE the table level (libaom
//!   `quant_params->qmatrix_level_y/u/v`, `decodeframe.c:1902-1917`). The
//!   `aom_get_qmlevel(qindex, ...)` derivation that maps base_q_idx onto a
//!   level is ENCODER-side only (how aomenc picks what to write); a decoder
//!   never sees it.
//! - Level 15 is libaom's flat sentinel: `av1_qm_init` stores NULL pointers
//!   for `NUM_QM_LEVELS - 1`, and 1D/identity transforms read that NULL slot
//!   unconditionally (`av1_get_iqmatrix`'s `is_2d_transform` branch), so
//!   `V_DCT`/`H_DCT`/`IDTX`-class units never scale their `dqv`. Both facts
//!   land here as "return [`Iqm`] or `None`", and `None` dequantises exactly
//!   as before this module existed.
//! - Per `av1_use_qmatrix` (`setup_segmentation_dequant`), a LOSSLESS
//!   segment (quantizer index clamped to 0 with all five deltas zero) also
//!   dequantises flat; the decode-side caller checks that per block through
//!   the same quantizer index every dequant call site already uses.
//! - The tables are per plane KIND, not just per level: `iwt_matrix_ref`
//!   is `[level][luma/chroma][...]` and chroma matrices are genuinely
//!   different data (`av1_qm_init`'s `&iwt_matrix_ref[q][c >= 1][..]`).
//! - libaom stores one flat 3344-entry buffer per (level, kind); the
//!   14 own-storage shapes live at [`shape_range`]'s offsets in
//!   `av1_qm_init`'s `TX_SIZES_ALL` walk order (the C file's own
//!   `/* Size ... */` comments mark that same order), and the 64-axis
//!   shapes REUSE a smaller shape's matrix (`av1_get_adjusted_tx_size`):
//!   64x64/64x32/32x64 point at the 32x32 matrix, 64x16 at 32x16, 16x64 at
//!   16x32. libaom indexes those reuses through its ACTIVE-AREA coeff
//!   buffer (whose eob `av1_get_max_eob` caps at 1024), which is why
//!   [`dqv_at`] re-strides our true-w raster onto `stride`.

/// libaom `NUM_QM_LEVELS` (`quant_common.h`: `1 << QM_LEVEL_BITS`).
pub const NUM_QM_LEVELS: usize = 16;
/// libaom `AOM_QM_BITS` (`aom_dsp/aom_dsp_common.h`): the fixed-point
/// shift of the matrix multiply, `(qm * dqv + 16) >> 5`.
pub const AOM_QM_BITS: u32 = 5;

/// One selected inverse matrix: libaom's `const qm_val_t *` plus the
/// stride that pointer runs at -- the ACTIVE-AREA HEIGHT, because libaom's
/// coefficient positions are COLUMN-major ([`dqv_at`]).
#[derive(Clone, Copy)]
pub struct Iqm {
    /// The matrix entries, in libaom's transposed position order.
    pub data: &'static [u8],
    /// The column stride of libaom's position space: the active-area
    /// height.
    pub stride: usize,
}

/// `av1_get_adjusted_tx_size` (`av1/common/blockd.h:1361`): the matrix to
/// use for a transform whose true shape stores no matrix of its own.
fn adjusted(w: usize, h: usize) -> (usize, usize) {
    match (w, h) {
        (64, 64) | (64, 32) | (32, 64) => (32, 32),
        (64, 16) => (32, 16),
        (16, 64) => (16, 32),
        _ => (w, h),
    }
}

/// `[av1_qm_init]`'s flat-buffer offsets for the 14 own-storage shapes, in
/// its `TX_SIZES_ALL` walk order. `(offset, length)` into one (level, kind)
/// slice; `None` for a shape that stores no matrix (the 64-axis clamps land
/// here only if `adjusted` ever misses one -- it does not).
fn shape_range(w: usize, h: usize) -> Option<(usize, usize)> {
    match (w, h) {
        (4, 4) => Some((0, 16)),
        (8, 8) => Some((16, 64)),
        (16, 16) => Some((80, 256)),
        (32, 32) => Some((336, 1024)),
        (4, 8) => Some((1360, 32)),
        (8, 4) => Some((1392, 32)),
        (8, 16) => Some((1424, 128)),
        (16, 8) => Some((1552, 128)),
        (16, 32) => Some((1680, 512)),
        (32, 16) => Some((2192, 512)),
        (4, 16) => Some((2704, 64)),
        (16, 4) => Some((2768, 64)),
        (8, 32) => Some((2832, 256)),
        (32, 8) => Some((3088, 256)),
        _ => None,
    }
}

/// `av1_get_iqmatrix` narrowed to the decode path: the matrix for transform
/// `(w, h)` at `level` (0..=14) of plane kind (`chroma` = planes 1/2), or
/// `None` for libaom's NULL matrix. Level >= 15 is the flat sentinel, so a
/// qm-off frame (levels parked at 15) never selects a matrix.
pub fn iwt_matrix(level: usize, chroma: bool, w: usize, h: usize) -> Option<Iqm> {
    if level >= NUM_QM_LEVELS - 1 {
        return None;
    }
    let (sw, sh) = adjusted(w, h);
    let (off, len) = shape_range(sw, sh)?;
    Some(Iqm {
        data: &IWT[level][usize::from(chroma)][off..off + len],
        stride: sh,
    })
}

/// `get_dqv` (`av1/decoder/decodetxb.c:54-60`): the DC/AC quantizer scaled
/// by the coefficient's own matrix entry, `((m * dqv) + 16) >> 5`. The
/// subtle half is the index: libaom's scan positions -- and so
/// `iqmatrix[coeff_idx]` and its whole txb coefficient buffer -- live in a
/// COLUMN-major position space (its `mcol_scan_8x4` is the identity
/// `0,1,2,...`: position `p` strides DOWN the columns of the `h`-tall
/// transposed block), so the coefficient of our row-major `w`-wide grid at
/// `(row, col)` reads matrix entry `col * stride + row`. Verified
/// coefficient-for-coefficient against the instrumented oracle on the
/// `--enable-qm=1` repro stream -- including 8x8 units, whose stored
/// matrices are NOT symmetric, so the transposition is observable there
/// and only this mapping matches.
#[inline]
pub fn dqv_at(qm: &Iqm, pos: usize, w: usize, dqv: i64) -> i64 {
    let (row, col) = (pos / w, pos % w);
    match qm.data.get(col * qm.stride + row) {
        Some(&m) => ((i64::from(m) * dqv) + (1 << (AOM_QM_BITS - 1))) >> AOM_QM_BITS,
        // libaom's `av1_get_max_eob` caps eob at 1024 for the clamped 64
        // shapes, so a NONZERO level never reaches a position past the
        // reused matrix -- `dequant_wh_into`'s zero-level skip guards this
        // arm and it is unreachable on spec streams. Flat (dqv) if ever
        // reached.
        None => dqv,
    }
}

mod iwt_a;

mod iwt_b;

mod iwt_c;

// GENERATED from libaom's `iwt_matrix_ref[NUM_QM_LEVELS - 1][2][QM_TOTAL_SIZE]`
// (`av1/common/quant_common.c`, oracle at ~/.cache/aom-oracle) -- verbatim values.
// 15 levels x [luma, chroma] x 3344 flat entries; per (level, kind) the 14
// own-storage shapes follow `av1_qm_init`'s `TX_SIZES_ALL` walk order (the
// 64-variants reuse smaller shapes -- see `SHAPE_OFFSET`), which is also the
// order of the `/* Size ... */` comment markers in the C file itself.
// Level 15 is absent by construction: libaom stores NULL (flat) for it.
#[rustfmt::skip]
static IWT: [[[u8; 3344]; 2]; 15] = [
    iwt_a::IWT_L0,
    iwt_a::IWT_L1,
    iwt_a::IWT_L2,
    iwt_a::IWT_L3,
    iwt_a::IWT_L4,
    iwt_b::IWT_L5,
    iwt_b::IWT_L6,
    iwt_b::IWT_L7,
    iwt_b::IWT_L8,
    iwt_b::IWT_L9,
    iwt_c::IWT_L10,
    iwt_c::IWT_L11,
    iwt_c::IWT_L12,
    iwt_c::IWT_L13,
    iwt_c::IWT_L14,
];

#[cfg(test)]
mod tests;
