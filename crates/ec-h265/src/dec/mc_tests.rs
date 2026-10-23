//! Tests of the motion compensation: hand-computed values for every fraction,
//! edge clamping, weighted prediction, a per-sample reference model compared on
//! random blocks, and a 1080p timing run.

use crate::dec::cu_syntax::Motion;
use crate::dec::mc::{
    Block, McScratch, PlaneMut, PlaneRef, RefPicture, interp_chroma, interp_luma, predict_pu,
};
use crate::dec::weighted::{ExplicitWeights, Weight, store};
use ec_h265_syntax::{PredWeightTable, WeightEntry};

/// A plane of `w x h` samples from `f(x, y)`, stride `w`.
fn plane_from(w: usize, h: usize, bd: u32, f: impl Fn(usize, usize) -> u16) -> Vec<u16> {
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| f(x, y))
        .map(|v| v.min((1 << bd) - 1))
        .collect()
}

fn view(samples: &[u16], w: usize, h: usize, bd: u32) -> PlaneRef<'_> {
    PlaneRef {
        samples,
        stride: w,
        width: w,
        height: h,
        bit_depth: bd,
    }
}

fn motion(pred_flags: [bool; 2], ref_idx: [i8; 2], mv: [[i32; 2]; 2]) -> Motion {
    Motion {
        pred_flags,
        ref_idx,
        mv,
    }
}

/// A 16x16 luma plane that is zero except for `value` at (8, 8).
fn impulse16(value: u16) -> Vec<u16> {
    plane_from(16, 16, 10, |x, y| if (x, y) == (8, 8) { value } else { 0 })
}

#[test]
fn luma_impulse_fractions_8bit_hand_values() {
    // Impulse of 100 at column 8; the block starts at column 4 on row 8, so output
    // j reads the impulse with tap index 7 - j. shift1 = 0 at 8 bits.
    let p = impulse16(100);
    let pl = view(&p, 16, 16, 8);
    let blk = Block {
        x: 4,
        y: 8,
        width: 4,
        height: 1,
    };
    let mut scratch = McScratch::new();
    let mut out = [0i32; 4];
    let cases: [(i32, [i32; 4]); 3] = [
        (1, [0, 100, -500, 1700]),
        (2, [-100, 400, -1100, 4000]),
        (3, [-100, 400, -1000, 5800]),
    ];
    for (frac, want) in cases {
        interp_luma(&pl, blk, [frac, 0], &mut scratch, &mut out).unwrap();
        assert_eq!(out, want, "luma horizontal fraction {frac}");
    }
    // Full-sample position: the impulse shifted by shift3 = 14 - 8 = 6.
    let full = Block {
        x: 8,
        y: 8,
        width: 1,
        height: 1,
    };
    interp_luma(&pl, full, [0, 0], &mut scratch, &mut out).unwrap();
    assert_eq!(out[0], 100 << 6);
}

#[test]
fn luma_impulse_fractions_10bit_hand_values() {
    // shift1 = 2 at 10 bits, so each tap sum is divided by 4 (exact here).
    let p = impulse16(1000);
    let pl = view(&p, 16, 16, 10);
    let blk = Block {
        x: 4,
        y: 8,
        width: 4,
        height: 1,
    };
    let mut scratch = McScratch::new();
    let mut out = [0i32; 4];
    let cases: [(i32, [i32; 4]); 3] = [
        (1, [0, 250, -1250, 4250]),
        (2, [-250, 1000, -2750, 10000]),
        (3, [-250, 1000, -2500, 14500]),
    ];
    for (frac, want) in cases {
        interp_luma(&pl, blk, [frac, 0], &mut scratch, &mut out).unwrap();
        assert_eq!(out, want, "10-bit luma horizontal fraction {frac}");
    }
    let full = Block {
        x: 8,
        y: 8,
        width: 1,
        height: 1,
    };
    interp_luma(&pl, full, [0, 0], &mut scratch, &mut out).unwrap();
    assert_eq!(out[0], 1000 << 4, "10-bit full sample shift3 = 4");
}

#[test]
fn luma_vertical_only_matches_horizontal_by_symmetry() {
    let p = impulse16(100);
    let pl = view(&p, 16, 16, 8);
    // Block column 8, rows 4..8: output row k reads the impulse at tap 7 - k.
    let blk = Block {
        x: 8,
        y: 4,
        width: 1,
        height: 4,
    };
    let mut scratch = McScratch::new();
    let mut out = [0i32; 4];
    interp_luma(&pl, blk, [0, 1], &mut scratch, &mut out).unwrap();
    assert_eq!(out, [0, 100, -500, 1700]);
    interp_luma(&pl, blk, [0, 3], &mut scratch, &mut out).unwrap();
    assert_eq!(out, [-100, 400, -1000, 5800]);
}

#[test]
fn luma_two_dimensional_hand_values() {
    // 10 bits, impulse 4096 at (8, 8), block at (4, 4), fractions (2, 2).
    // Horizontal pass on the impulse row gives tmp = [-1024, 4096, -11264, 40960]
    // (shift1 = 2), and the vertical pass with shift2 = 6 yields the 4x4 block.
    // Built raw: plane_from would clamp the impulse to the 10-bit range.
    let mut p = vec![0u16; 16 * 16];
    p[8 * 16 + 8] = 4096;
    let pl = view(&p, 16, 16, 10);
    let blk = Block {
        x: 4,
        y: 4,
        width: 4,
        height: 4,
    };
    let mut scratch = McScratch::new();
    let mut out = [0i32; 16];
    interp_luma(&pl, blk, [2, 2], &mut scratch, &mut out).unwrap();
    assert_eq!(
        out,
        [
            16, -64, 176, -640, //
            -64, 256, -704, 2560, //
            176, -704, 1936, -7040, //
            -640, 2560, -7040, 25600,
        ]
    );
}

#[test]
fn chroma_impulse_fractions_hand_values() {
    // 8 bits, impulse 100 at (8, 8) in the chroma plane; block at column 5 (so
    // output j reads tap 4 - j), the four taps that land in range being j = 1..=3.
    let p = plane_from(16, 16, 8, |x, y| if (x, y) == (8, 8) { 100 } else { 0 });
    let pl = view(&p, 16, 16, 8);
    let blk = Block {
        x: 5,
        y: 8,
        width: 4,
        height: 1,
    };
    let mut scratch = McScratch::new();
    let mut out = [0i32; 4];
    let cases: [(i32, [i32; 4]); 7] = [
        (1, [0, -200, 1000, 5800]),
        (2, [0, -200, 1600, 5400]),
        (3, [0, -400, 2800, 4600]),
        (4, [0, -400, 3600, 3600]),
        (5, [0, -600, 4600, 2800]),
        (6, [0, -400, 5400, 1600]),
        (7, [0, -200, 5800, 1000]),
    ];
    for (frac, want) in cases {
        interp_chroma(&pl, blk, [frac, 0], &mut scratch, &mut out).unwrap();
        assert_eq!(out, want, "chroma horizontal eighth {frac}");
    }
    let full = Block {
        x: 8,
        y: 8,
        width: 1,
        height: 1,
    };
    interp_chroma(&pl, full, [0, 0], &mut scratch, &mut out).unwrap();
    assert_eq!(out[0], 100 << 6);
}

#[test]
fn chroma_motion_vector_splits_integer_and_eighth() {
    // A chroma mv of 8 + 3 eighths moves one whole sample plus the eighth-3 filter.
    let p = plane_from(16, 16, 8, |x, y| if (x, y) == (8, 8) { 100 } else { 0 });
    let pl = view(&p, 16, 16, 8);
    let mut scratch = McScratch::new();
    let mut a = [0i32; 4];
    let mut b = [0i32; 4];
    let start = Block {
        x: 4,
        y: 8,
        width: 4,
        height: 1,
    };
    interp_chroma(&pl, start, [11, 0], &mut scratch, &mut a).unwrap();
    let shifted = Block {
        x: 5,
        y: 8,
        width: 4,
        height: 1,
    };
    interp_chroma(&pl, shifted, [3, 0], &mut scratch, &mut b).unwrap();
    assert_eq!(a, b);
}

#[test]
fn luma_edges_clamp_at_all_four_borders() {
    // v(x, y) = 10 * y + x on an 8x8 plane: every border has distinct values.
    let p = plane_from(8, 8, 8, |x, y| (10 * y + x) as u16);
    let pl = view(&p, 8, 8, 8);
    let mut scratch = McScratch::new();
    let mut out = [0i32; 16];
    // Far left: every column reads column 0, so each row is v(0, y) << 6.
    let blk = Block {
        x: 0,
        y: 0,
        width: 4,
        height: 4,
    };
    interp_luma(&pl, blk, [-4000, 0], &mut scratch, &mut out).unwrap();
    for r in 0..4 {
        for c in 0..4 {
            assert_eq!(out[r * 4 + c], (10 * r as i32) << 6, "far left");
        }
    }
    // Far right: every column reads column 7.
    let blk = Block {
        x: 4,
        y: 0,
        width: 4,
        height: 4,
    };
    interp_luma(&pl, blk, [4000, 0], &mut scratch, &mut out).unwrap();
    for r in 0..4 {
        for c in 0..4 {
            assert_eq!(out[r * 4 + c], (10 * r as i32 + 7) << 6, "far right");
        }
    }
    // Far up: every row reads row 0, so each column is v(x, 0) = x.
    let blk = Block {
        x: 4,
        y: 0,
        width: 4,
        height: 4,
    };
    interp_luma(&pl, blk, [0, -4000], &mut scratch, &mut out).unwrap();
    for r in 0..4 {
        for c in 0..4 {
            assert_eq!(out[r * 4 + c], (4 + c as i32) << 6, "far up");
        }
    }
    // Far down: every row reads row 7.
    let blk = Block {
        x: 4,
        y: 4,
        width: 4,
        height: 4,
    };
    interp_luma(&pl, blk, [0, 4000], &mut scratch, &mut out).unwrap();
    for r in 0..4 {
        for c in 0..4 {
            assert_eq!(out[r * 4 + c], ((70 + 4 + c) as i32) << 6, "far down");
        }
    }
    // Far outside in both directions, to each corner.
    let corners = [
        (0, 0, -4000, -4000, 0),
        (4, 0, 4000, -4000, 7),
        (0, 4, -4000, 4000, 70),
        (4, 4, 4000, 4000, 77),
    ];
    for (x, y, mx, my, want) in corners {
        let blk = Block {
            x,
            y,
            width: 4,
            height: 4,
        };
        interp_luma(&pl, blk, [mx, my], &mut scratch, &mut out).unwrap();
        assert!(out.iter().all(|&v| v == want << 6), "corner {want}");
    }
}

#[test]
fn luma_fractional_edges_clamp_to_the_border_sample() {
    // A fractional mv far outside reads one border column (or row) only, so the
    // filter sums to 64 times that border sample.
    let p = plane_from(8, 8, 8, |x, y| (10 * y + x) as u16);
    let pl = view(&p, 8, 8, 8);
    let mut scratch = McScratch::new();
    let mut out = [0i32; 16];
    let blk = Block {
        x: 0,
        y: 0,
        width: 4,
        height: 4,
    };
    // -4001 quarter samples: integer part -1001, fraction 3.
    interp_luma(&pl, blk, [-4001, 0], &mut scratch, &mut out).unwrap();
    for r in 0..4 {
        assert!(
            out[r * 4..r * 4 + 4]
                .iter()
                .all(|&v| v == 64 * 10 * r as i32)
        );
    }
    interp_luma(&pl, blk, [0, -4001], &mut scratch, &mut out).unwrap();
    for c in 0..4 {
        assert!((0..4).all(|r| out[r * 4 + c] == 64 * c as i32));
    }
}

#[test]
fn chroma_edges_clamp_at_all_four_borders() {
    let p = plane_from(8, 8, 8, |x, y| (10 * y + x) as u16);
    let pl = view(&p, 8, 8, 8);
    let mut scratch = McScratch::new();
    let mut out = [0i32; 4];
    let blk = Block {
        x: 0,
        y: 0,
        width: 2,
        height: 2,
    };
    // Eighth-sample mv of -8001: integer part -1001, fraction 7.
    interp_chroma(&pl, blk, [-8001, 0], &mut scratch, &mut out).unwrap();
    assert_eq!(out, [0, 0, 640, 640]);
    let blk = Block {
        x: 6,
        y: 6,
        width: 2,
        height: 2,
    };
    interp_chroma(&pl, blk, [8000, 8000], &mut scratch, &mut out).unwrap();
    assert!(out.iter().all(|&v| v == 77 << 6));
}

#[test]
fn default_uni_and_bi_hand_values() {
    let mut buf = [0u16; 4];
    let blk = Block {
        x: 0,
        y: 0,
        width: 4,
        height: 1,
    };
    // 8 bits: shift1 = 6 for uni, shift2 = 7 for bi.
    let mut dst = PlaneMut {
        samples: &mut buf,
        stride: 4,
        bit_depth: 8,
        origin: 0,
    };
    store(
        &mut dst,
        blk,
        [Some(&[6400, -100, 16320, 16384]), None],
        None,
    )
    .unwrap();
    assert_eq!(buf, [100, 0, 255, 255]);

    let mut buf = [0u16; 2];
    let blk2 = Block {
        x: 0,
        y: 0,
        width: 2,
        height: 1,
    };
    let mut dst = PlaneMut {
        samples: &mut buf,
        stride: 2,
        bit_depth: 8,
        origin: 0,
    };
    store(
        &mut dst,
        blk2,
        [Some(&[6400, 6400]), Some(&[6400, 6464])],
        None,
    )
    .unwrap();
    assert_eq!(buf, [100, 101]);

    // 10 bits: shift1 = 4 for uni, shift2 = 5 for bi.
    let mut buf = [0u16; 3];
    let blk3 = Block {
        x: 0,
        y: 0,
        width: 3,
        height: 1,
    };
    let mut dst = PlaneMut {
        samples: &mut buf,
        stride: 3,
        bit_depth: 10,
        origin: 0,
    };
    store(&mut dst, blk3, [Some(&[6400, 16368, 16384]), None], None).unwrap();
    assert_eq!(buf, [400, 1023, 1023]);
    let mut dst = PlaneMut {
        samples: &mut buf,
        stride: 3,
        bit_depth: 10,
        origin: 0,
    };
    store(
        &mut dst,
        blk3,
        [Some(&[6400, 0, 0]), Some(&[12800, 0, 0])],
        None,
    )
    .unwrap();
    assert_eq!(buf[0], 600);
}

#[test]
fn explicit_uni_and_bi_hand_values_8bit() {
    let blk = Block {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let mut buf = [0u16; 1];
    // denom 2, w = 3, o = 5: ((6400*3 + 128) >> 8) + 5 = 75 + 5.
    let mut dst = PlaneMut {
        samples: &mut buf,
        stride: 1,
        bit_depth: 8,
        origin: 0,
    };
    let w = [Weight { w: 3, o: 5 }, Weight::default()];
    store(&mut dst, blk, [Some(&[6400]), None], Some((2, w))).unwrap();
    assert_eq!(buf[0], 80);

    // Negative weight -1 and offset 200: ((-6400 + 128) >> 8) + 200 = -25 + 200.
    let w = [Weight { w: -1, o: 200 }, Weight::default()];
    let mut dst = PlaneMut {
        samples: &mut buf,
        stride: 1,
        bit_depth: 8,
        origin: 0,
    };
    store(&mut dst, blk, [Some(&[6400]), None], Some((2, w))).unwrap();
    assert_eq!(buf[0], 175);

    // Negative offset clipping at zero: w = -1, o = -200 gives -225 -> 0.
    let w = [Weight { w: -1, o: -200 }, Weight::default()];
    let mut dst = PlaneMut {
        samples: &mut buf,
        stride: 1,
        bit_depth: 8,
        origin: 0,
    };
    store(&mut dst, blk, [Some(&[6400]), None], Some((2, w))).unwrap();
    assert_eq!(buf[0], 0);

    // Bi: w0 = -2, o0 = -3 with p0 = 6400; w1 = 6, o1 = 7 with p1 = 12800.
    // (-12800 + 76800 + ((-3 + 7 + 1) << 8)) >> 9 = 127.
    let w = [Weight { w: -2, o: -3 }, Weight { w: 6, o: 7 }];
    let mut dst = PlaneMut {
        samples: &mut buf,
        stride: 1,
        bit_depth: 8,
        origin: 0,
    };
    store(&mut dst, blk, [Some(&[6400]), Some(&[12800])], Some((2, w))).unwrap();
    assert_eq!(buf[0], 127);
}

#[test]
fn explicit_uni_hand_value_10bit_with_scaled_offset() {
    // The caller scales offsets by 1 << (BitDepth - 8); 5 << 2 = 20.
    // shift1 = 4 at 10 bits, log2WD = 2 + 4 = 6: ((6400*3 + 32) >> 6) + 20 = 300 + 20.
    let blk = Block {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let mut buf = [0u16; 1];
    let mut dst = PlaneMut {
        samples: &mut buf,
        stride: 1,
        bit_depth: 10,
        origin: 0,
    };
    let w = [Weight { w: 3, o: 20 }, Weight::default()];
    store(&mut dst, blk, [Some(&[6400]), None], Some((2, w))).unwrap();
    assert_eq!(buf[0], 320);
}

#[test]
fn weight_table_derivation_with_chroma_offsets() {
    // luma denom 2, chroma denom 3. Entry 0: luma delta -1 offset 5; chroma deltas
    // 3 and -2 with the ChromaOffset values the parser already derived (-7, 9).
    // Entry 1 has no flags, so every weight is the default and offsets are 0.
    let table = PredWeightTable {
        luma_log2_weight_denom: 2,
        delta_chroma_log2_weight_denom: 1,
        l0: vec![
            WeightEntry {
                luma: Some((-1, 5)),
                chroma: Some([(3, -7), (-2, 9)]),
            },
            WeightEntry::default(),
        ],
        l1: vec![],
    };
    let ew = ExplicitWeights::from_table(&table, 8, 8, false).unwrap();
    assert_eq!(ew.log2_denom, [2, 3, 3]);
    assert_eq!(
        ew.entries[0][0],
        [
            Weight { w: 3, o: 5 },
            Weight { w: 11, o: -7 },
            Weight { w: 6, o: 9 },
        ]
    );
    assert_eq!(
        ew.entries[0][1],
        [
            Weight { w: 4, o: 0 },
            Weight { w: 8, o: 0 },
            Weight { w: 8, o: 0 }
        ]
    );
    // At 10 bits every offset is scaled by 4.
    let ew10 = ExplicitWeights::from_table(&table, 10, 10, false).unwrap();
    assert_eq!(ew10.entries[0][0][0], Weight { w: 3, o: 20 });
    assert_eq!(ew10.entries[0][0][1].o, -28);
    assert_eq!(ew10.entries[0][0][2].o, 36);
    // A coded weight delta outside -128..=127 is corrupt.
    let bad = PredWeightTable {
        luma_log2_weight_denom: 0,
        delta_chroma_log2_weight_denom: 0,
        l0: vec![WeightEntry {
            luma: Some((200, 0)),
            chroma: None,
        }],
        l1: vec![],
    };
    assert!(ExplicitWeights::from_table(&bad, 8, 8, false).is_err());
}

#[test]
fn predict_pu_constant_planes_recover_the_value() {
    // A constant picture predicts itself for any motion and any weighting that is
    // the identity: default bi of 100 and 100 gives 100.
    let bd = 8;
    let luma = plane_from(64, 64, bd, |_, _| 100);
    let chroma = plane_from(32, 32, bd, |_, _| 100);
    let pic: RefPicture<'_> = [
        view(&luma, 64, 64, bd),
        view(&chroma, 32, 32, bd),
        view(&chroma, 32, 32, bd),
    ];
    let refs = [&[pic][..], &[pic][..]];
    let mut oy = vec![0u16; 64 * 64];
    let mut ou = vec![0u16; 32 * 32];
    let mut ov = vec![0u16; 32 * 32];
    let mut out = [
        PlaneMut {
            samples: &mut oy,
            stride: 64,
            bit_depth: bd,
            origin: 0,
        },
        PlaneMut {
            samples: &mut ou,
            stride: 32,
            bit_depth: bd,
            origin: 0,
        },
        PlaneMut {
            samples: &mut ov,
            stride: 32,
            bit_depth: bd,
            origin: 0,
        },
    ];
    let mut scratch = McScratch::new();
    let pu = Block {
        x: 16,
        y: 8,
        width: 8,
        height: 4,
    };
    let m = motion([true, true], [0, 0], [[-13, 7], [22, -5]]);
    predict_pu(refs, &m, None, pu, &mut out, &mut scratch).unwrap();
    for (plane, stride, w, h, x, y) in [
        (&oy, 64, 8, 4, 16, 8),
        (&ou, 32, 4, 2, 8, 4),
        (&ov, 32, 4, 2, 8, 4),
    ] {
        for r in 0..h {
            for c in 0..w {
                assert_eq!(plane[(y + r) * stride + x + c], 100);
            }
        }
    }
}

#[test]
fn predict_pu_rejects_bad_input() {
    let bd = 8;
    let luma = plane_from(16, 16, bd, |_, _| 1);
    let chroma = plane_from(8, 8, bd, |_, _| 1);
    let pic: RefPicture<'_> = [
        view(&luma, 16, 16, bd),
        view(&chroma, 8, 8, bd),
        view(&chroma, 8, 8, bd),
    ];
    let refs = [&[pic][..], &[][..]];
    let mut oy = vec![0u16; 16 * 16];
    let mut ou = vec![0u16; 8 * 8];
    let mut ov = vec![0u16; 8 * 8];
    let mut out = [
        PlaneMut {
            samples: &mut oy,
            stride: 16,
            bit_depth: bd,
            origin: 0,
        },
        PlaneMut {
            samples: &mut ou,
            stride: 8,
            bit_depth: bd,
            origin: 0,
        },
        PlaneMut {
            samples: &mut ov,
            stride: 8,
            bit_depth: bd,
            origin: 0,
        },
    ];
    let mut scratch = McScratch::new();
    let pu = Block {
        x: 0,
        y: 0,
        width: 8,
        height: 8,
    };
    // No list in use.
    let none = motion([false, false], [0, 0], [[0; 2]; 2]);
    assert!(predict_pu(refs, &none, None, pu, &mut out, &mut scratch).is_err());
    // refIdxL1 names a picture list 1 does not have.
    let l1 = motion([false, true], [0, 0], [[0; 2]; 2]);
    assert!(predict_pu(refs, &l1, None, pu, &mut out, &mut scratch).is_err());
    // A rectangle past the plane edge.
    let off = Block {
        x: 12,
        y: 0,
        width: 8,
        height: 8,
    };
    let ok = motion([true, false], [0, 0], [[0; 2]; 2]);
    assert!(predict_pu(refs, &ok, None, off, &mut out, &mut scratch).is_err());
}

// ---------------------------------------------------------------------------
// Reference model: a deliberately naive transcription of 8.5.3.3 per sample.
// It shares no code with the optimised path and copies the filter tables from
// the spec again, so a transcription error in either shows up as a mismatch.
// ---------------------------------------------------------------------------

mod naive {
    use crate::dec::cu_syntax::Motion;
    use crate::dec::mc::{Block, PlaneRef, RefPicture};
    use crate::dec::weighted::ExplicitWeights;

    /// Table 8-11, luma. Row 0 is unused.
    const LUMA: [[i64; 8]; 4] = [
        [0, 0, 0, 64, 0, 0, 0, 0],
        [-1, 4, -10, 58, 17, -5, 1, 0],
        [-1, 4, -11, 40, 40, -11, 4, -1],
        [0, 1, -5, 17, 58, -10, 4, -1],
    ];
    /// Table 8-12, chroma. Row 0 is unused.
    const CHROMA: [[i64; 4]; 8] = [
        [0, 64, 0, 0],
        [-2, 58, 10, -2],
        [-4, 54, 16, -2],
        [-6, 46, 28, -4],
        [-4, 36, 36, -4],
        [-4, 28, 46, -6],
        [-2, 16, 54, -4],
        [-2, 10, 58, -2],
    ];

    /// `ref[Clip3(0, W-1, x)][Clip3(0, H-1, y)]`, one sample.
    fn sample(p: &PlaneRef<'_>, x: i64, y: i64) -> i64 {
        let cx = x.clamp(0, p.width as i64 - 1) as usize;
        let cy = y.clamp(0, p.height as i64 - 1) as usize;
        i64::from(p.samples[cy * p.stride + cx])
    }

    /// One predSampleLX at integer position (xi, yi) with fractions (fx, fy).
    fn interp(p: &PlaneRef<'_>, xi: i64, yi: i64, fx: usize, fy: usize, luma: bool) -> i64 {
        let bd = i64::from(p.bit_depth);
        let shift1 = (bd - 8).min(4);
        let shift2 = 6;
        let shift3 = (14 - bd).max(2);
        let taps: i64 = if luma { 8 } else { 4 };
        let pad = taps / 2 - 1;
        let coef = |frac: usize, i: i64| -> i64 {
            if luma {
                LUMA[frac][i as usize]
            } else {
                CHROMA[frac][i as usize]
            }
        };
        if fx == 0 && fy == 0 {
            return sample(p, xi, yi) << shift3;
        }
        if fy == 0 {
            let mut s = 0;
            for i in 0..taps {
                s += coef(fx, i) * sample(p, xi + i - pad, yi);
            }
            return s >> shift1;
        }
        if fx == 0 {
            let mut s = 0;
            for i in 0..taps {
                s += coef(fy, i) * sample(p, xi, yi + i - pad);
            }
            return s >> shift1;
        }
        let mut s = 0;
        for n in 0..taps {
            let mut temp = 0;
            for i in 0..taps {
                temp += coef(fx, i) * sample(p, xi + i - pad, yi + n - pad);
            }
            s += coef(fy, n) * (temp >> shift1);
        }
        s >> shift2
    }

    /// The weighted sample prediction of 8.5.3.3.4 for one sample.
    /// `preds[list]` is `Some(predSamplesLX)` for each list in use.
    fn weight(bd: i64, preds: [Option<i64>; 2], explicit: Option<(i64, [(i64, i64); 2])>) -> i64 {
        let max = (1 << bd) - 1;
        let shift1 = 14 - bd;
        let clip = |v: i64| v.clamp(0, max);
        match (preds, explicit) {
            ([Some(p0), Some(p1)], None) => {
                let shift2 = 15 - bd;
                clip((p0 + p1 + (1 << (shift2 - 1))) >> shift2)
            }
            ([Some(p0), Some(p1)], Some((denom, [(w0, o0), (w1, o1)]))) => {
                let log2wd = denom + shift1;
                clip((p0 * w0 + p1 * w1 + ((o0 + o1 + 1) << log2wd)) >> (log2wd + 1))
            }
            ([Some(p), None], None) | ([None, Some(p)], None) => {
                clip((p + (1 << (shift1 - 1))) >> shift1)
            }
            ([Some(p), None], Some((denom, [(w, o), _]))) => {
                let log2wd = denom + shift1;
                clip(if log2wd >= 1 {
                    ((p * w + (1 << (log2wd - 1))) >> log2wd) + o
                } else {
                    p * w + o
                })
            }
            ([None, Some(p)], Some((denom, [_, (w, o)]))) => {
                let log2wd = denom + shift1;
                clip(if log2wd >= 1 {
                    ((p * w + (1 << (log2wd - 1))) >> log2wd) + o
                } else {
                    p * w + o
                })
            }
            _ => panic!("naive model: no prediction"),
        }
    }

    /// The final samples of one PU component, row-major `blk.width * blk.height`.
    pub fn predict_component(
        refs: [&[RefPicture<'_>]; 2],
        motion: &Motion,
        weights: Option<&ExplicitWeights>,
        blk: Block,
        comp: usize,
    ) -> Vec<i64> {
        let luma = comp == 0;
        let bd = i64::from(
            refs.iter()
                .enumerate()
                .find_map(|(l, r)| {
                    motion.pred_flags[l].then(|| r[motion.ref_idx[l] as usize][comp].bit_depth)
                })
                .unwrap(),
        );
        let mut out = Vec::with_capacity(blk.width * blk.height);
        for r in 0..blk.height {
            for c in 0..blk.width {
                let mut preds = [None, None];
                let mut wts = [(0i64, 0i64); 2];
                for list in 0..2 {
                    if !motion.pred_flags[list] {
                        continue;
                    }
                    let pic = &refs[list][motion.ref_idx[list] as usize];
                    let p = &pic[comp];
                    let mv = motion.mv[list];
                    let (xi, yi, fx, fy) = if luma {
                        (
                            blk.x as i64 + i64::from(mv[0] >> 2) + c as i64,
                            blk.y as i64 + i64::from(mv[1] >> 2) + r as i64,
                            (mv[0] & 3) as usize,
                            (mv[1] & 3) as usize,
                        )
                    } else {
                        (
                            blk.x as i64 + i64::from(mv[0] >> 3) + c as i64,
                            blk.y as i64 + i64::from(mv[1] >> 3) + r as i64,
                            (mv[0] & 7) as usize,
                            (mv[1] & 7) as usize,
                        )
                    };
                    preds[list] = Some(interp(p, xi, yi, fx, fy, luma));
                    if let Some(ew) = weights {
                        let w = ew.entries[list][motion.ref_idx[list] as usize][comp];
                        wts[list] = (i64::from(w.w), i64::from(w.o));
                    }
                }
                let explicit = weights.map(|ew| (i64::from(ew.log2_denom[comp]), wts));
                out.push(weight(bd, preds, explicit));
            }
        }
        out
    }
}

/// SplitMix64: a fixed-seed generator, so every run sees the same blocks.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next_u64() % (hi - lo + 1) as u64) as i64
    }
}

/// An owned picture: Y, Cb, Cr planes at the 4:2:0 sizes of a `w x h` luma picture.
struct Pic {
    planes: [Vec<u16>; 3],
    w: usize,
    h: usize,
    bd: u32,
}

impl Pic {
    fn random(rng: &mut Rng, w: usize, h: usize, bd: u32) -> Pic {
        let max = (1u64 << bd) - 1;
        let mut fill =
            |n: usize| -> Vec<u16> { (0..n).map(|_| rng.below(max + 1) as u16).collect() };
        let planes = [fill(w * h), fill(w * h / 4), fill(w * h / 4)];
        Pic { planes, w, h, bd }
    }

    fn view(&self) -> RefPicture<'_> {
        let dims = [
            (self.w, self.h),
            (self.w / 2, self.h / 2),
            (self.w / 2, self.h / 2),
        ];
        std::array::from_fn(|c| PlaneRef {
            samples: &self.planes[c],
            stride: dims[c].0,
            width: dims[c].0,
            height: dims[c].1,
            bit_depth: self.bd,
        })
    }
}

#[test]
fn random_blocks_match_the_reference_model() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let mut scratch = McScratch::new();
    let mut checked = 0usize;
    let mut sizes_8x4_or_4x8 = 0usize;
    let mut uni = 0usize;
    let mut bi = 0usize;
    let mut explicit = 0usize;
    for case in 0..10_000 {
        let bd = if rng.below(2) == 0 { 8 } else { 10 };
        let pw = 8 * rng.range(8, 28) as usize;
        let ph = 8 * rng.range(8, 28) as usize;
        // Sizes 4..=64 in steps of 4; every eighth case is forced to 8x4 or 4x8.
        let (w, h) = if case % 8 == 0 {
            if rng.below(2) == 0 { (8, 4) } else { (4, 8) }
        } else {
            (4 * rng.range(1, 16) as usize, 4 * rng.range(1, 16) as usize)
        };
        if (w, h) == (8, 4) || (w, h) == (4, 8) {
            sizes_8x4_or_4x8 += 1;
        }
        let x = 4 * rng.below(((pw - w) / 4 + 1) as u64) as usize;
        let y = 4 * rng.below(((ph - h) / 4 + 1) as u64) as usize;

        let lists: [Vec<Pic>; 2] = [
            (0..rng.range(1, 3))
                .map(|_| Pic::random(&mut rng, pw, ph, bd))
                .collect(),
            (0..rng.range(1, 3))
                .map(|_| Pic::random(&mut rng, pw, ph, bd))
                .collect(),
        ];
        let views: [Vec<RefPicture<'_>>; 2] = [
            lists[0].iter().map(Pic::view).collect(),
            lists[1].iter().map(Pic::view).collect(),
        ];

        let pred_flags = match rng.below(3) {
            0 => [true, false],
            1 => [false, true],
            _ => [true, true],
        };
        if pred_flags[0] && pred_flags[1] {
            bi += 1;
        } else {
            uni += 1;
        }
        let mut ref_idx = [0i8; 2];
        let mut mv = [[0i32; 2]; 2];
        let reach = 4 * (pw.max(ph) as i64 + 96);
        for (list, mv_list) in mv.iter_mut().enumerate() {
            ref_idx[list] = rng.below(lists[list].len() as u64) as i8;
            *mv_list = std::array::from_fn(|_| {
                if rng.below(2) == 0 {
                    rng.range(-reach, reach) as i32
                } else {
                    rng.range(-40, 40) as i32
                }
            });
        }
        let m = Motion {
            pred_flags,
            ref_idx,
            mv,
        };

        let weights = if rng.below(2) == 0 {
            None
        } else {
            explicit += 1;
            let shift = bd - 8;
            let denom: [u32; 3] = [
                rng.below(8) as u32,
                rng.below(8) as u32,
                rng.below(8) as u32,
            ];
            let mut entries: [Vec<[Weight; 3]>; 2] = [Vec::new(), Vec::new()];
            for (list, slot) in entries.iter_mut().enumerate() {
                *slot = (0..lists[list].len())
                    .map(|_| {
                        std::array::from_fn(|_| Weight {
                            w: rng.range(-128, 127) as i32,
                            o: (rng.range(-128, 127) as i32) << shift,
                        })
                    })
                    .collect();
            }
            Some(ExplicitWeights {
                log2_denom: denom,
                entries,
            })
        };

        let pu = Block {
            x,
            y,
            width: w,
            height: h,
        };
        let mut planes = [
            vec![0u16; pw * ph],
            vec![0u16; pw * ph / 4],
            vec![0u16; pw * ph / 4],
        ];
        let dims = [(pw, ph), (pw / 2, ph / 2), (pw / 2, ph / 2)];
        {
            let [a, b, c] = &mut planes;
            let mut out = [
                PlaneMut {
                    samples: a,
                    stride: pw,
                    bit_depth: bd,
                    origin: 0,
                },
                PlaneMut {
                    samples: b,
                    stride: pw / 2,
                    bit_depth: bd,
                    origin: 0,
                },
                PlaneMut {
                    samples: c,
                    stride: pw / 2,
                    bit_depth: bd,
                    origin: 0,
                },
            ];
            let refs = [&views[0][..], &views[1][..]];
            predict_pu(refs, &m, weights.as_ref(), pu, &mut out, &mut scratch).unwrap();
        }

        let refs = [&views[0][..], &views[1][..]];
        for comp in 0..3 {
            let blk = if comp == 0 {
                pu
            } else {
                Block {
                    x: x / 2,
                    y: y / 2,
                    width: w / 2,
                    height: h / 2,
                }
            };
            let want = naive::predict_component(refs, &m, weights.as_ref(), blk, comp);
            let stride = dims[comp].0;
            for r in 0..blk.height {
                for c in 0..blk.width {
                    let got = i64::from(planes[comp][(blk.y + r) * stride + blk.x + c]);
                    assert_eq!(
                        got,
                        want[r * blk.width + c],
                        "case {case} bd {bd} comp {comp} pu {pu:?} mv {mv:?} refs {ref_idx:?} flags {pred_flags:?} r {r} c {c}"
                    );
                }
            }
        }
        checked += 1;
    }
    assert_eq!(checked, 10_000);
    eprintln!(
        "mc reference model: {checked} random PUs agree ({uni} uni, {bi} bi, {explicit} explicit weighted, {sizes_8x4_or_4x8} of 8x4/4x8)"
    );
}

/// One 1080p frame of 8x8 bi-predicted PUs, luma and 4:2:0 chroma. Timing only;
/// run with `--release --nocapture` to see the figure.
#[test]
fn bench_1080p_bipred_8x8_frame() {
    let (pw, ph, bd) = (1920usize, 1080usize, 10u32);
    let mut rng = Rng(42);
    let pic0 = Pic::random(&mut rng, pw, ph, bd);
    let pic1 = Pic::random(&mut rng, pw, ph, bd);
    let l0 = [pic0.view()];
    let l1 = [pic1.view()];
    let mut planes = [
        vec![0u16; pw * ph],
        vec![0u16; pw * ph / 4],
        vec![0u16; pw * ph / 4],
    ];
    let [a, b, c] = &mut planes;
    let mut out = [
        PlaneMut {
            samples: a,
            stride: pw,
            bit_depth: bd,
            origin: 0,
        },
        PlaneMut {
            samples: b,
            stride: pw / 2,
            bit_depth: bd,
            origin: 0,
        },
        PlaneMut {
            samples: c,
            stride: pw / 2,
            bit_depth: bd,
            origin: 0,
        },
    ];
    let mut scratch = McScratch::new();
    let start = std::time::Instant::now();
    for by in 0..ph / 8 {
        for bx in 0..pw / 8 {
            let m = Motion {
                pred_flags: [true, true],
                ref_idx: [0, 0],
                mv: [
                    [(bx as i32 * 7 % 61) - 30, (by as i32 * 5 % 53) - 26],
                    [(by as i32 * 3 % 47) - 23, (bx as i32 * 11 % 59) - 29],
                ],
            };
            let pu = Block {
                x: bx * 8,
                y: by * 8,
                width: 8,
                height: 8,
            };
            predict_pu([&l0, &l1], &m, None, pu, &mut out, &mut scratch).unwrap();
        }
    }
    let elapsed = start.elapsed();
    let checksum: u64 = planes.iter().flatten().map(|&v| u64::from(v)).sum();
    assert!(checksum > 0);
    eprintln!(
        "bench: 1080p frame, {} bi-pred 8x8 PUs (luma + 4:2:0 chroma): {:.2} ms (checksum {checksum})",
        (pw / 8) * (ph / 8),
        elapsed.as_secs_f64() * 1e3
    );
}
