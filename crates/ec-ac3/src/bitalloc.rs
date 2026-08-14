//! The parametric bit allocation model of A/52 §7.2.2.
//!
//! This is the part of AC-3 that has no slack in it: the decoder recomputes,
//! from the exponents alone, exactly how many bits the encoder spent on every
//! mantissa. One wrong `bap` and the rest of the block is read at the wrong bit
//! offset, so the seven steps below are transcribed step for step, integer for
//! integer, from the standard's pseudo code.

use crate::aht_tables::HEBAPTAB;
use crate::tables::{
    BAPTAB, BNDTAB, DBPBTAB, FASTDEC, FASTGAIN, FLOORTAB, HTH, LATAB, MASKTAB, SLOWDEC, SLOWGAIN,
};

/// The bit allocation parameters a block either sends (`baie`) or reuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BitAllocParams {
    /// Slow decay code.
    pub sdcycod: u8,
    /// Fast decay code.
    pub fdcycod: u8,
    /// Slow gain code.
    pub sgaincod: u8,
    /// dB/bit code.
    pub dbpbcod: u8,
    /// Masking floor code.
    pub floorcod: u8,
}

/// Delta bit allocation for one channel (§7.2.2.6): up to 8 segments of
/// masking-curve adjustment. `nseg == 0` means "no adjustment", which is the
/// state the standard asks decoders to initialise to every syncframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeltaBa {
    /// Number of active segments.
    pub nseg: usize,
    /// Band offset of each segment, relative to the previous segment's end.
    pub offset: [u8; 8],
    /// Band count of each segment.
    pub length: [u8; 8],
    /// Adjustment code of each segment.
    pub delta: [u8; 8],
}

/// Which kind of channel the allocation is being run for; it selects the two
/// special cases the standard carves out of the excitation function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// A full-bandwidth channel: starts at bin 0, runs the `lowcomp` path.
    Fbw,
    /// The LFE channel: like `Fbw`, but `calc_lowcomp` is skipped for its last
    /// band (bin 6) because there is no band above it.
    Lfe,
    /// The coupling channel: starts high, and its leak state is initialised
    /// from `cplfleak`/`cplsleak` rather than from the first bands.
    Coupling {
        /// `(cplfleak, cplsleak)` as sent, already shifted into leak units.
        leak: (i32, i32),
    },
}

/// One channel's inputs to the model.
#[derive(Debug, Clone, Copy)]
pub struct Allocation<'a> {
    /// Sample rate code, indexing the hearing threshold table.
    pub fscod: usize,
    /// Parameters shared by every channel of the block.
    pub params: BitAllocParams,
    /// First and last-plus-one mantissa bin.
    pub range: (usize, usize),
    /// Fast gain code for this channel.
    pub fgaincod: u8,
    /// SNR offset, already combined from `csnroffst` and the channel's fine
    /// offset: `(((csnroffst - 15) << 4) + fsnroffst) << 2`.
    pub snroffset: i32,
    /// Channel kind.
    pub kind: Channel,
    /// Delta bit allocation, when the block sent or is reusing some.
    pub dba: Option<&'a DeltaBa>,
    /// Emit Annex E's high-efficiency pointers (`hebaptab[]`, Table E3.1)
    /// instead of `baptab[]`, which is what an AHT channel is allocated with
    /// (§E3.4.3.1). Everything up to the final table lookup is identical.
    pub high_efficiency: bool,
}

/// Log-domain addition of two PSD values (§7.2.2.3).
fn logadd(a: i32, b: i32) -> i32 {
    let c = a - b;
    let address = ((c.abs() >> 1) as usize).min(255);
    if c >= 0 {
        a + LATAB[address]
    } else {
        b + LATAB[address]
    }
}

/// `calc_lowcomp()` (§7.2.2.4).
fn calc_lowcomp(a: i32, b0: i32, b1: i32, bin: usize) -> i32 {
    if bin < 7 {
        if b0 + 256 == b1 {
            384
        } else if b0 > b1 {
            (a - 64).max(0)
        } else {
            a
        }
    } else if bin < 20 {
        if b0 + 256 == b1 {
            320
        } else if b0 > b1 {
            (a - 64).max(0)
        } else {
            a
        }
    } else {
        (a - 128).max(0)
    }
}

/// Run the seven steps for one channel, filling `bap[start..end]`.
///
/// `exps` is indexed by mantissa bin and holds the decoded 5-bit exponents.
/// Everything outside `range` in `bap` is left alone.
pub fn compute(alloc: &Allocation<'_>, exps: &[u8], bap: &mut [u8]) {
    let (start, end) = alloc.range;
    if start >= end {
        return;
    }
    let sdecay = SLOWDEC[alloc.params.sdcycod as usize & 3];
    let fdecay = FASTDEC[alloc.params.fdcycod as usize & 3];
    let sgain = SLOWGAIN[alloc.params.sgaincod as usize & 3];
    let dbknee = DBPBTAB[alloc.params.dbpbcod as usize & 3];
    let floor = FLOORTAB[alloc.params.floorcod as usize & 7];
    let fgain = FASTGAIN[alloc.fgaincod as usize & 7];
    let snroffset = alloc.snroffset;

    // Step: exponents into PSD.
    let mut psd = [0i32; 256];
    for bin in start..end {
        psd[bin] = 3072 - (i32::from(exps[bin]) << 7);
    }

    // Step: PSD integration into 1/6-octave bands.
    let mut bndpsd = [0i32; 50];
    let mut j = start;
    let mut k = MASKTAB[start];
    loop {
        let lastbin = BNDTAB[k + 1].min(end);
        bndpsd[k] = psd[j];
        j += 1;
        while j < lastbin {
            bndpsd[k] = logadd(bndpsd[k], psd[j]);
            j += 1;
        }
        k += 1;
        if end <= lastbin {
            break;
        }
    }

    // Step: excitation function.
    let bndstrt = MASKTAB[start];
    let bndend = MASKTAB[end - 1] + 1;
    let mut excite = [0i32; 50];
    let (mut fastleak, mut slowleak) = match alloc.kind {
        Channel::Coupling { leak } => leak,
        _ => (0, 0),
    };
    let begin = if bndstrt == 0 {
        // fbw and lfe channels. The standard writes the LFE exception as
        // `bndend != 7`; the LFE is the only channel that can end there, so
        // the kind says it more plainly.
        let is_lfe = alloc.kind == Channel::Lfe;
        let mut lowcomp = 0;
        lowcomp = calc_lowcomp(lowcomp, bndpsd[0], bndpsd[1], 0);
        excite[0] = bndpsd[0] - fgain - lowcomp;
        lowcomp = calc_lowcomp(lowcomp, bndpsd[1], bndpsd[2], 1);
        excite[1] = bndpsd[1] - fgain - lowcomp;
        let mut begin = 7;
        for bin in 2..7 {
            if !(is_lfe && bin == 6) {
                lowcomp = calc_lowcomp(lowcomp, bndpsd[bin], bndpsd[bin + 1], bin);
            }
            fastleak = bndpsd[bin] - fgain;
            slowleak = bndpsd[bin] - sgain;
            excite[bin] = fastleak - lowcomp;
            if !(is_lfe && bin == 6) && bndpsd[bin] <= bndpsd[bin + 1] {
                begin = bin + 1;
                break;
            }
        }
        for bin in begin..bndend.min(22) {
            if !(is_lfe && bin == 6) {
                lowcomp = calc_lowcomp(lowcomp, bndpsd[bin], bndpsd[bin + 1], bin);
            }
            fastleak = (fastleak - fdecay).max(bndpsd[bin] - fgain);
            slowleak = (slowleak - sdecay).max(bndpsd[bin] - sgain);
            excite[bin] = (fastleak - lowcomp).max(slowleak);
        }
        22
    } else {
        bndstrt
    };
    for bin in begin..bndend {
        fastleak = (fastleak - fdecay).max(bndpsd[bin] - fgain);
        slowleak = (slowleak - sdecay).max(bndpsd[bin] - sgain);
        excite[bin] = fastleak.max(slowleak);
    }

    // Step: masking curve.
    let mut mask = [0i32; 50];
    for bin in bndstrt..bndend {
        if bndpsd[bin] < dbknee {
            excite[bin] += (dbknee - bndpsd[bin]) >> 2;
        }
        mask[bin] = excite[bin].max(HTH[alloc.fscod.min(2)][bin]);
    }

    // Step: delta bit allocation.
    if let Some(dba) = alloc.dba {
        let mut band = 0usize;
        for seg in 0..dba.nseg {
            band += usize::from(dba.offset[seg]);
            let delta = if dba.delta[seg] >= 4 {
                (i32::from(dba.delta[seg]) - 3) << 7
            } else {
                (i32::from(dba.delta[seg]) - 4) << 7
            };
            for _ in 0..dba.length[seg] {
                if band >= 50 {
                    break;
                }
                mask[band] += delta;
                band += 1;
            }
        }
    }

    // Step: bap from the gap between psd and the adjusted mask.
    let mut i = start;
    let mut j = MASKTAB[start];
    loop {
        let lastbin = BNDTAB[j + 1].min(end);
        mask[j] -= snroffset;
        mask[j] -= floor;
        if mask[j] < 0 {
            mask[j] = 0;
        }
        mask[j] &= 0x1fe0;
        mask[j] += floor;
        while i < lastbin {
            let address = ((psd[i] - mask[j]) >> 5).clamp(0, 63) as usize;
            bap[i] = if alloc.high_efficiency {
                HEBAPTAB[address]
            } else {
                BAPTAB[address]
            };
            i += 1;
        }
        j += 1;
        if end <= lastbin {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_alloc(snroffset: i32) -> Allocation<'static> {
        Allocation {
            fscod: 0,
            params: BitAllocParams {
                sdcycod: 2,
                fdcycod: 1,
                sgaincod: 1,
                dbpbcod: 2,
                floorcod: 4,
            },
            range: (0, 253),
            fgaincod: 4,
            snroffset,
            kind: Channel::Fbw,
            dba: None,
            high_efficiency: false,
        }
    }

    #[test]
    fn louder_signal_and_bigger_snroffset_buy_more_bits() {
        // A flat, loud spectrum against a flat, quiet one: every allocated
        // pointer must be at least as large for the loud one, and raising the
        // SNR offset must not reduce any pointer. Those two monotonicities are
        // what the model exists to provide.
        let loud = [2u8; 256];
        let quiet = [20u8; 256];
        let (mut a, mut b, mut c) = ([0u8; 256], [0u8; 256], [0u8; 256]);
        compute(&flat_alloc(1000), &loud, &mut a);
        compute(&flat_alloc(1000), &quiet, &mut b);
        compute(&flat_alloc(1400), &quiet, &mut c);
        for bin in 0..253 {
            assert!(a[bin] >= b[bin], "bin {bin}: {} < {}", a[bin], b[bin]);
            assert!(c[bin] >= b[bin], "bin {bin}: {} < {}", c[bin], b[bin]);
        }
        assert!(a.iter().any(|&v| v > 0));
    }

    #[test]
    fn delta_bit_allocation_lowers_the_mask_where_it_points() {
        // A quiet-ish flat spectrum with a modest SNR offset, so the affected
        // bands are not already pinned at the top of baptab[].
        let exps = [16u8; 256];
        let (mut plain, mut with_dba) = ([0u8; 256], [0u8; 256]);
        compute(&flat_alloc(0), &exps, &mut plain);
        let dba = DeltaBa {
            nseg: 1,
            offset: [10, 0, 0, 0, 0, 0, 0, 0],
            length: [4, 0, 0, 0, 0, 0, 0, 0],
            // delta 0 => (0 - 4) << 7, a 4 x -6 dB mask reduction => more bits.
            delta: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        let mut alloc = flat_alloc(0);
        alloc.dba = Some(&dba);
        compute(&alloc, &exps, &mut with_dba);
        for bin in BNDTAB[10]..BNDTAB[14] {
            assert!(with_dba[bin] >= plain[bin], "bin {bin}");
        }
        assert!(
            (BNDTAB[10]..BNDTAB[14]).any(|bin| with_dba[bin] > plain[bin]),
            "the -6 dB segment bought no bits anywhere"
        );
        for bin in BNDTAB[14]..253 {
            assert_eq!(with_dba[bin], plain[bin], "bin {bin}");
        }
        for bin in 0..BNDTAB[10] {
            assert_eq!(with_dba[bin], plain[bin], "bin {bin}");
        }
    }

    #[test]
    fn all_zero_range_is_a_no_op() {
        let mut bap = [7u8; 256];
        let mut alloc = flat_alloc(0);
        alloc.range = (37, 37);
        compute(&alloc, &[5u8; 256], &mut bap);
        assert!(bap.iter().all(|&v| v == 7));
    }
}
