//! The sample type a decoded picture is stored in: `u8` for 8-bit video,
//! `u16` for High 10. The 8-bit instantiation is the original decoder and is
//! kept byte-identical; `u16` shares all parsing and reconstruction and swaps
//! only the SIMD-specialised kernels for plain scalar ones.

use crate::deblock::{
    EdgeParams, filter_chroma_h_edge8, filter_chroma_line, filter_luma_h_edge16, filter_luma_line,
};
use crate::decoder::{SliceCtx, compensate8, compensate16};
use crate::dpb::Picture;
use ec_core::error::Result;

/// A picture sample.
pub(crate) trait Sample:
    Copy + Default + PartialEq + std::fmt::Debug + Send + Sync + 'static
{
    /// True for the 8-bit instantiation.
    const IS_8: bool;
    /// Bytes per sample in an output frame.
    const BYTES: usize;
    /// Append `src` to an output plane, little-endian, shifted left by `shift`.
    fn extend_bytes(out: &mut Vec<u8>, src: &[Self], shift: u32);
    fn to_i32(self) -> i32;
    /// Truncating conversion; the value is known to be in range.
    fn from_i32(v: i32) -> Self;
    /// `Clip3(0, max, v)`; the 8-bit instantiation ignores `max`.
    fn clip(v: i32, max: i32) -> Self;

    /// Motion compensation of one partition (clause 8.4.2).
    #[allow(clippy::too_many_arguments)]
    fn compensate(
        pic: &mut Picture<Self>,
        refs: &[Picture<Self>],
        ctx: &SliceCtx,
        mb_x: usize,
        mb_y: usize,
        px: usize,
        py: usize,
        pw: usize,
        ph: usize,
        mv: [[i16; 2]; 2],
        ref_idx: [i8; 2],
    ) -> Result<()>;

    /// Filter 16 luma samples across a horizontal edge whose q0 row starts at
    /// `q0_row` (a uniform boundary strength). The scalar form here is the
    /// definition; `u8` overrides it with the SIMD kernel.
    fn filter_luma_h_edge16(
        data: &mut [Self],
        max: i32,
        q0_row: usize,
        stride: usize,
        e: &EdgeParams,
    ) {
        for k in 0..16 {
            filter_luma_line(data, max, q0_row + k, stride, e);
        }
    }

    /// Eight-sample chroma counterpart of [`Sample::filter_luma_h_edge16`].
    fn filter_chroma_h_edge8(
        data: &mut [Self],
        max: i32,
        q0_row: usize,
        stride: usize,
        e: &EdgeParams,
    ) {
        for k in 0..8 {
            filter_chroma_line(data, max, q0_row + k, stride, e);
        }
    }
}

impl Sample for u8 {
    const IS_8: bool = true;
    const BYTES: usize = 1;
    #[inline]
    fn extend_bytes(out: &mut Vec<u8>, src: &[u8], _shift: u32) {
        out.extend_from_slice(src);
    }
    #[inline(always)]
    fn to_i32(self) -> i32 {
        i32::from(self)
    }
    #[inline(always)]
    fn from_i32(v: i32) -> u8 {
        v as u8
    }
    #[inline(always)]
    fn clip(v: i32, _max: i32) -> u8 {
        v.clamp(0, 255) as u8
    }
    #[allow(clippy::too_many_arguments)]
    fn compensate(
        pic: &mut Picture<u8>,
        refs: &[Picture<u8>],
        ctx: &SliceCtx,
        mb_x: usize,
        mb_y: usize,
        px: usize,
        py: usize,
        pw: usize,
        ph: usize,
        mv: [[i16; 2]; 2],
        ref_idx: [i8; 2],
    ) -> Result<()> {
        compensate8(pic, refs, ctx, mb_x, mb_y, px, py, pw, ph, mv, ref_idx)
    }
    #[inline]
    fn filter_luma_h_edge16(
        data: &mut [u8],
        _max: i32,
        q0_row: usize,
        stride: usize,
        e: &EdgeParams,
    ) {
        filter_luma_h_edge16(data, q0_row, stride, e);
    }
    #[inline]
    fn filter_chroma_h_edge8(
        data: &mut [u8],
        _max: i32,
        q0_row: usize,
        stride: usize,
        e: &EdgeParams,
    ) {
        filter_chroma_h_edge8(data, q0_row, stride, e);
    }
}

impl Sample for u16 {
    const IS_8: bool = false;
    const BYTES: usize = 2;
    #[inline]
    fn extend_bytes(out: &mut Vec<u8>, src: &[u16], shift: u32) {
        for &v in src {
            out.extend_from_slice(&(v << shift).to_le_bytes());
        }
    }
    #[inline(always)]
    fn to_i32(self) -> i32 {
        i32::from(self)
    }
    #[inline(always)]
    fn from_i32(v: i32) -> u16 {
        v as u16
    }
    #[allow(clippy::too_many_arguments)]
    fn compensate(
        pic: &mut Picture<u16>,
        refs: &[Picture<u16>],
        ctx: &SliceCtx,
        mb_x: usize,
        mb_y: usize,
        px: usize,
        py: usize,
        pw: usize,
        ph: usize,
        mv: [[i16; 2]; 2],
        ref_idx: [i8; 2],
    ) -> Result<()> {
        compensate16(pic, refs, ctx, mb_x, mb_y, px, py, pw, ph, mv, ref_idx)
    }
    #[inline(always)]
    fn clip(v: i32, max: i32) -> u16 {
        v.clamp(0, max) as u16
    }
}
