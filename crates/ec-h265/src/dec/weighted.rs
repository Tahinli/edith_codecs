//! Weighted sample prediction (8.5.3.3.4): the default and explicit combinations
//! of the interpolated `predSamplesLX` arrays into clipped output samples, and the
//! derivation of `LumaWeightLX` and `ChromaWeightLX` from a parsed
//! `pred_weight_table()` (7.4.7.3).

use super::mc::{Block, PlaneMut, check_depth, check_out};
use ec_core::error::{Error, Result};
use ec_h265_syntax::PredWeightTable;

/// One weight and its offset for one component of one reference picture.
/// `o` already carries the `<< (BitDepth - 8)` of the explicit formula.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Weight {
    /// `LumaWeightLX` or `ChromaWeightLX`.
    pub w: i32,
    /// `o0` or `o1` of 8.5.3.3.4.3, after the bit depth shift.
    pub o: i32,
}

/// The explicit weights of one slice: a denominator per component and, per list
/// and reference index, a weight for Y, Cb and Cr.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExplicitWeights {
    /// `luma_log2_weight_denom` for Y, `ChromaLog2WeightDenom` for Cb and Cr.
    pub log2_denom: [u32; 3],
    /// `entries[list][ref_idx][component]`.
    pub entries: [Vec<[Weight; 3]>; 2],
}

impl ExplicitWeights {
    /// Derive the weights of a slice from its parsed `pred_weight_table()`.
    ///
    /// A reference whose luma (or chroma) weight flag was 0 gets the default
    /// weight `2^denom` with offset 0. Offsets are scaled by
    /// `1 << (BitDepth - 8)`, or not scaled with `high_precision_offsets`.
    pub fn from_table(
        table: &PredWeightTable,
        bit_depth_luma: u32,
        bit_depth_chroma: u32,
        high_precision_offsets: bool,
    ) -> Result<ExplicitWeights> {
        check_depth(bit_depth_luma)?;
        check_depth(bit_depth_chroma)?;
        let luma_denom = table.luma_log2_weight_denom;
        let chroma_denom = i64::from(luma_denom) + i64::from(table.delta_chroma_log2_weight_denom);
        if luma_denom > 7 || !(0..=7).contains(&chroma_denom) {
            return Err(Error::corrupt(
                "HEVC pred_weight_table: weight denominator out of range",
            ));
        }
        let chroma_denom = chroma_denom as u32;
        // WpOffsetBdShift (7-56): 0 with high_precision_offsets_enabled_flag.
        let shift_y = if high_precision_offsets {
            0
        } else {
            bit_depth_luma - 8
        };
        let shift_c = if high_precision_offsets {
            0
        } else {
            bit_depth_chroma - 8
        };
        let mut entries: [Vec<[Weight; 3]>; 2] = [Vec::new(), Vec::new()];
        for (list, src) in [&table.l0, &table.l1].into_iter().enumerate() {
            entries[list] = src
                .iter()
                .map(|e| -> Result<[Weight; 3]> {
                    let luma = match e.luma {
                        Some((dw, off)) => Weight {
                            w: (1 << luma_denom) + check_delta_weight(dw)?,
                            o: check_offset(off, bit_depth_luma, high_precision_offsets)?
                                << shift_y,
                        },
                        None => Weight {
                            w: 1 << luma_denom,
                            o: 0,
                        },
                    };
                    let mut out = [luma, Weight::default(), Weight::default()];
                    for (c, slot) in out.iter_mut().skip(1).enumerate() {
                        *slot = match e.chroma {
                            // `ChromaOffset` is already derived by 7-56 in the parser.
                            Some(pair) => {
                                let (dw, off) = pair[c];
                                Weight {
                                    w: (1 << chroma_denom) + check_delta_weight(dw)?,
                                    o: check_offset(off, bit_depth_chroma, high_precision_offsets)?
                                        << shift_c,
                                }
                            }
                            None => Weight {
                                w: 1 << chroma_denom,
                                o: 0,
                            },
                        };
                    }
                    Ok(out)
                })
                .collect::<Result<Vec<_>>>()?;
        }
        Ok(ExplicitWeights {
            log2_denom: [luma_denom, chroma_denom, chroma_denom],
            entries,
        })
    }

    /// The weight of `component` for `refIdxLX` `ref_idx` in list `list`.
    pub fn lookup(&self, list: usize, ref_idx: usize, component: usize) -> Result<Weight> {
        self.entries
            .get(list)
            .and_then(|l| l.get(ref_idx))
            .map(|w| w[component])
            .ok_or_else(|| {
                Error::corrupt(format!(
                    "HEVC weighted prediction: no weights for list {list} index {ref_idx}"
                ))
            })
    }
}

/// A coded `delta_luma_weight` or `delta_chroma_weight` lies in `-128..=127` (7.4.7.3).
fn check_delta_weight(dw: i32) -> Result<i32> {
    if (-128..=127).contains(&dw) {
        Ok(dw)
    } else {
        Err(Error::corrupt(
            "HEVC pred_weight_table: weight delta outside its range",
        ))
    }
}

/// Luma and chroma offsets are coded in `-128..=127` (7-54, 7-56 with the
/// high-precision flag clear), or in `-(1 << (BitDepth - 1))..(1 << (BitDepth - 1))` with it.
fn check_offset(o: i32, bit_depth: u32, high_precision: bool) -> Result<i32> {
    let half = 1i32 << if high_precision { bit_depth - 1 } else { 7 };
    if (-half..half).contains(&o) {
        Ok(o)
    } else {
        Err(Error::corrupt(
            "HEVC pred_weight_table: offset outside its range",
        ))
    }
}

/// Combine the interpolated arrays of one component into the output rectangle.
///
/// `pred[list]` is `Some` for each list in use, holding `blk.width * blk.height`
/// values in row-major order. `explicit` carries `log2Wd`'s base denominator and
/// the weights of both lists (the unused one is ignored); `None` selects the
/// default weighted prediction.
pub fn store(
    dst: &mut PlaneMut<'_>,
    blk: Block,
    pred: [Option<&[i32]>; 2],
    explicit: Option<(u32, [Weight; 2])>,
) -> Result<()> {
    check_out(dst, blk)?;
    let bd = dst.bit_depth;
    let max = (1i32 << bd) - 1;
    // Equations of 8.5.3.3.4.2, with shift1 = 14 - bitDepth and shift2 = 15 - bitDepth.
    let shift1 = 14 - bd;
    let shift2 = 15 - bd;
    let w = blk.width;
    for r in 0..blk.height {
        let base = (blk.y + r - dst.origin) * dst.stride + blk.x;
        let row = &mut dst.samples[base..base + w];
        let span = r * w..(r + 1) * w;
        match (pred, explicit) {
            ([Some(p), Some(q)], None) => {
                bi_default(&p[span.clone()], &q[span], row, shift2, max);
            }
            ([Some(p), Some(q)], Some((denom, wt))) => {
                bi_explicit(&p[span.clone()], &q[span], row, denom + shift1, wt, max);
            }
            ([Some(p), None], None) | ([None, Some(p)], None) => {
                uni_default(&p[span], row, shift1, max);
            }
            ([Some(p), None], Some((denom, wt))) => {
                uni_explicit(&p[span], row, denom + shift1, wt[0], max);
            }
            ([None, Some(p)], Some((denom, wt))) => {
                uni_explicit(&p[span], row, denom + shift1, wt[1], max);
            }
            _ => {
                return Err(Error::corrupt(
                    "HEVC weighted prediction: no prediction array for the block",
                ));
            }
        }
    }
    Ok(())
}

/// Default uni-prediction: `Clip3(0, max, (predSamples + offset1) >> shift1)`.
fn uni_default(pred: &[i32], out: &mut [u16], shift1: u32, max: i32) {
    let offset1 = 1i32 << (shift1 - 1);
    for (o, &p) in out.iter_mut().zip(pred) {
        *o = ((p + offset1) >> shift1).clamp(0, max) as u16;
    }
}

/// Default bi-prediction: `Clip3(0, max, (p0 + p1 + offset2) >> shift2)`.
fn bi_default(p0: &[i32], p1: &[i32], out: &mut [u16], shift2: u32, max: i32) {
    let offset2 = 1i32 << (shift2 - 1);
    for ((o, &a), &b) in out.iter_mut().zip(p0).zip(p1) {
        *o = ((a + b + offset2) >> shift2).clamp(0, max) as u16;
    }
}

/// Explicit uni-prediction (8-252 and 8-253): `log2wd` is `denom + shift1`.
fn uni_explicit(pred: &[i32], out: &mut [u16], log2wd: u32, wt: Weight, max: i32) {
    for (o, &p) in out.iter_mut().zip(pred) {
        let v = if log2wd >= 1 {
            ((p * wt.w + (1 << (log2wd - 1))) >> log2wd) + wt.o
        } else {
            p * wt.w + wt.o
        };
        *o = v.clamp(0, max) as u16;
    }
}

/// Explicit bi-prediction (8-254): `(p0*w0 + p1*w1 + ((o0 + o1 + 1) << log2wd)) >> (log2wd + 1)`.
fn bi_explicit(p0: &[i32], p1: &[i32], out: &mut [u16], log2wd: u32, wt: [Weight; 2], max: i32) {
    let [a, b] = wt;
    let round = (a.o + b.o + 1) << log2wd;
    for ((o, &p), &q) in out.iter_mut().zip(p0).zip(p1) {
        *o = ((p * a.w + q * b.w + round) >> (log2wd + 1)).clamp(0, max) as u16;
    }
}
