//! Macroblock prediction modes and their coding trees
//! (RFC 6386 §8.2, §11.2, §11.4, §16.1).

use crate::bool::BoolDecoder;

/// Intra macroblock luma modes (RFC 6386 §8.2 enum values are normative;
/// chroma modes are the first four).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum YMode {
    /// Predict DC from the row above and column to the left.
    DcPred = 0,
    /// Predict rows from the row above.
    VPred = 1,
    /// Predict columns from the column to the left.
    HPred = 2,
    /// True-motion (second-difference) prediction.
    TmPred = 3,
    /// Each 4x4 Y subblock is independently predicted.
    BPred = 4,
}

impl YMode {
    /// Value as coded in the trees (the RFC's enum integers).
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// The subblock mode a 16x16 mode masquerades as for B_PRED context
    /// purposes (RFC 6386 §11.3 item 4).
    pub fn as_bmode(self) -> BMode {
        match self {
            YMode::DcPred => BMode::BDCPred,
            YMode::VPred => BMode::BVEPred,
            YMode::HPred => BMode::BHEPred,
            YMode::TmPred => BMode::BTMPred,
            YMode::BPred => unreachable!("B_PRED has no single subblock mode"),
        }
    }
}

/// The ten 4x4 subblock prediction modes (RFC 6386 §11.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BMode {
    /// DC from above row and left column.
    BDCPred = 0,
    /// True-motion.
    BTMPred = 1,
    /// Vertical from smoothed above row.
    BVEPred = 2,
    /// Horizontal from smoothed left column.
    BHEPred = 3,
    /// Down-left diagonal.
    BLDPred = 4,
    /// Down-right diagonal.
    BRDPred = 5,
    /// Vertical-right diagonal.
    BVRPred = 6,
    /// Vertical-left diagonal.
    BVLPred = 7,
    /// Horizontal-down diagonal.
    BHDPred = 8,
    /// Horizontal-up diagonal.
    BHUPred = 9,
}

impl BMode {
    /// Value as coded in the bmode tree (the RFC's enum integers).
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Key-frame Y-mode coding tree (RFC 6386 §11.2): B_PRED, then the four
/// full-block modes. Leaf values are the [`YMode`] enum integers.
pub const KF_YMODE_TREE: [i8; 8] = [-4, 2, 4, 6, 0, -1, -2, -3];

/// Key-frame Y-mode probabilities (fixed; RFC 6386 §11.2 and the
/// reference decoder's `kf_y_mode_probs`).
pub const KF_YMODE_PROBS: [u8; 4] = [145, 156, 163, 128];

/// Interframe Y-mode coding tree (RFC 6386 §8.2).
pub const YMODE_TREE: [i8; 8] = [0, 2, 4, 6, -1, -2, -3, -4];

/// Chroma-mode coding tree (RFC 6386 §8.2/§11.4): DC, V, H, TM.
pub const UV_MODE_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];

/// Key-frame chroma-mode probabilities (fixed; RFC 6386 §11.4).
pub const KF_UV_MODE_PROBS: [u8; 3] = [142, 114, 183];

/// 4x4 subblock-mode coding tree (RFC 6386 §11.2). Leaf values are the
/// [`BMode`] enum integers; note the leaf 0 (B_DC_PRED) relies on the
/// tree reader's `i <= 0` leaf test.
pub const BMODE_TREE: [i8; 18] = [
    0, 2, // B_DC_PRED = "0"
    -1, 4, // B_TM = "10"
    -2, 6, // B_VE = "110"
    8, 12, // inner
    -3, 10, // B_HE = "11100"
    -5, -6, // B_RD = "111010", B_VR = "111011"
    -4, 14, // B_LD = "111110"
    -7, 16, // B_VL = "1111110"
    -8, -9, // B_HD = "11111110", B_HU = "11111111"
];

/// Subblock modes in interframes decode with this constant probability
/// array, no context (RFC 6386 §11.3 item 5 / §16).
pub const INTER_BMODE_PROBS: [u8; 9] = [120, 90, 79, 133, 87, 85, 80, 111, 151];

/// Decode a key-frame Y mode (RFC 6386 §11.2).
pub fn read_kf_ymode(d: &mut BoolDecoder<'_>) -> YMode {
    YMode::from_u8(d.read_tree(&KF_YMODE_TREE, &KF_YMODE_PROBS))
}

/// Decode an interframe Y mode at the persisted probabilities
/// (RFC 6386 §16.1; `probs` is the frame header's updated array).
pub fn read_ymode(d: &mut BoolDecoder<'_>, probs: &[u8; 4]) -> YMode {
    YMode::from_u8(d.read_tree(&YMODE_TREE, probs))
}

/// Decode a chroma mode at the given (key-frame fixed or persisted
/// interframe) probabilities.
pub fn read_uvmode(d: &mut BoolDecoder<'_>, probs: &[u8; 3]) -> YMode {
    YMode::from_u8(d.read_tree(&UV_MODE_TREE, probs))
}

/// Decode a 4x4 subblock mode at `probs` (RFC 6386 §11.3: in key frames
/// the probabilities come from the above/left context table).
pub fn read_bmode(d: &mut BoolDecoder<'_>, probs: &[u8]) -> BMode {
    BMode::from_u8(d.read_tree(&BMODE_TREE, probs))
}

impl YMode {
    fn from_u8(v: u8) -> YMode {
        match v {
            0 => YMode::DcPred,
            1 => YMode::VPred,
            2 => YMode::HPred,
            3 => YMode::TmPred,
            4 => YMode::BPred,
            _ => unreachable!("tree cannot produce mode {v}"),
        }
    }
}

impl BMode {
    fn from_u8(v: u8) -> BMode {
        match v {
            0 => BMode::BDCPred,
            1 => BMode::BTMPred,
            2 => BMode::BVEPred,
            3 => BMode::BHEPred,
            4 => BMode::BLDPred,
            5 => BMode::BRDPred,
            6 => BMode::BVRPred,
            7 => BMode::BVLPred,
            8 => BMode::BHDPred,
            9 => BMode::BHUPred,
            _ => unreachable!("tree cannot produce bmode {v}"),
        }
    }
}

/// Inter-frame motion-vector reference modes (RFC 6386 §16.2), offset
/// from the intra modes by `num_ymodes` in the reference enum; here they
/// are their own small enum with the RFC's tree integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MvRef {
    /// Zero motion vector ("10" second branch; coded 0).
    Zero = 0,
    /// "Nearest" vector (coded 1).
    Nearest = 1,
    /// "Near" vector (coded 2).
    Near = 2,
    /// Explicit offset from best (coded 3).
    New = 3,
    /// Per-subblock vectors (coded 4).
    Split = 4,
}

/// mv_ref coding tree (RFC 6386 §16.2): leaves are [`MvRef`] integers.
/// mv_ref coding tree (RFC 6386 §16.2); leaves are [`MvRef`] integers.
pub const MV_REF_TREE: [i8; 8] = [0, 2, -1, 4, -2, 6, -3, -4];

/// Motion-vector component: row then column, each with the 19-probability
/// table (§17.1).
/// Probability-table offsets of one MV component (RFC 6386 §17.1).
pub const MVP_IS_SHORT: usize = 0;
/// Sign probability offset (§17.1).
pub const MVP_SIGN: usize = 1;
/// Short-value tree offset (§17.1).
/// Short-value tree offset (§17.1).
pub const MVP_SHORT: usize = 2;
/// Long-value bit probabilities offset (§17.1).
/// Long-value bit probabilities offset (§17.1).
pub const MVP_BITS: usize = MVP_SHORT + 7;
/// Total probabilities per MV component (§17.1).
/// Total probabilities per MV component (§17.1).
pub const MVP_COUNT: usize = MVP_BITS + 10;

/// Small-value coding tree for MV components (§17.1 `small_mvtree`).
/// Small-value coding tree for MV components (§17.1).
pub const SMALL_MV_TREE: [i8; 14] = [
    2, 8, // "0" / "1" subtrees
    4, 6, // "00" / "01"
    0, -1, // 0 = "000", 1 = "001"
    -2, -3, // 2, 3
    10, 12, // "10" / "11"
    -4, -5, // 4, 5
    -6, -7, // 6, 7
];

/// SPLITMV partition shapes tree + fixed probs (§16.4). Leaves are the
/// RFC's `MVpartition` integers: 0 = top/bottom, 1 = left/right,
/// 2 = quarters, 3 = MV_16.
pub const MV_PARTITION_TREE: [i8; 6] = [-3, 2, -2, 4, 0, -1];
/// SPLITMV partition tree fixed probabilities (§16.4).
pub const MV_PARTITION_PROBS: [u8; 3] = [110, 111, 150];

/// The four SPLITMV partitionings as subblock groups (§16.4), indexed
/// by the `MVpartition` integer: each group shares one motion vector,
/// decoded in the listed (leader) order.
pub const MV_PARTITIONS: [&[usize]; 4] = [
    // mv_top_bottom: {0..7} then {8..15}.
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    // mv_left_right: {0,1,4,5,8,9,12,13} then {2,3,6,7,10,11,14,15}.
    &[0, 1, 4, 5, 8, 9, 12, 13, 2, 3, 6, 7, 10, 11, 14, 15],
    // mv_quarters: {0,1,4,5} {2,3,6,7} {8,9,12,13} {10,11,14,15}.
    &[0, 1, 4, 5, 2, 3, 6, 7, 8, 9, 12, 13, 10, 11, 14, 15],
    // MV_16: every subblock gets its own vector, raster order.
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
];

/// Number of distinct vectors per partition shape (§16.4).
pub const MV_PARTITION_COUNT: [usize; 4] = [2, 2, 4, 16];

/// Inter subblock modes (§16.4): reuse the already-coded MV left/above,
/// zero, or an explicit offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SubMvRef {
    /// Copy the left neighbour's MV (coded 0).
    Left4x4 = 0,
    /// Copy the above neighbour's MV (coded 1).
    Above4x4 = 1,
    /// Zero MV (coded 2).
    Zero4x4 = 2,
    /// Explicit offset from best (coded 3).
    New4x4 = 3,
}

/// Sub-MV coding tree (RFC 6386 §16.4); leaves are [`SubMvRef`] integers.
/// Sub-MV coding tree (RFC 6386 §16.4); leaves are [`SubMvRef`] integers.
pub const SUB_MV_REF_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];

/// Context-conditioned sub-MV probabilities (§16.4 `sub_mv_ref_prob`).
pub const SUB_MV_REF_PROBS: [[u8; 3]; 5] = [
    [147, 136, 18],
    [106, 145, 1],
    [179, 121, 1],
    [223, 1, 34],
    [208, 1, 1],
];

/// Sub-MV tree context (§16.4 `vp8_mvCont`): 4 = left==above==zero,
/// 3 = left==above, 2 = above zero, 1 = left zero, 0 = normal.
/// Sub-MV tree context (§16.4 `vp8_mvCont`): 4 = left==above==zero,
pub fn mv_cont(l: (i16, i16), a: (i16, i16)) -> usize {
    let lez = l == (0, 0);
    let aez = a == (0, 0);
    let lea = l == a;
    if lea && lez {
        4
    } else if lea {
        3
    } else if aez {
        2
    } else if lez {
        1
    } else {
        0
    }
}

/// Weighted-census probability table for the mv_ref tree
/// (§16.3 `vp8_mode_contexts`, indexed by clamped count per slot).
pub const MODE_CONTEXTS: [[u8; 4]; 6] = [
    [7, 1, 1, 143],
    [14, 18, 14, 107],
    [135, 64, 57, 68],
    [60, 56, 128, 65],
    [159, 134, 128, 34],
    [234, 188, 128, 28],
];

/// Decode an mv_ref at the census-derived probabilities (§16.2/§16.3).
pub fn read_mv_ref(d: &mut BoolDecoder<'_>, probs: &[u8; 4]) -> MvRef {
    match d.read_tree(&MV_REF_TREE, probs) {
        0 => MvRef::Zero,
        1 => MvRef::Nearest,
        2 => MvRef::Near,
        3 => MvRef::New,
        4 => MvRef::Split,
        v => unreachable!("mv_ref tree produced {v}"),
    }
}

/// Decode a SPLITMV partition shape (§16.4); returns the `MVpartition`
/// integer indexing [`MV_PARTITIONS`].
pub fn read_mv_partition(d: &mut BoolDecoder<'_>) -> usize {
    usize::from(d.read_tree(&MV_PARTITION_TREE, &MV_PARTITION_PROBS))
}

/// Decode a sub-MV reference (§16.4) at the context's probabilities.
pub fn read_sub_mv_ref(d: &mut BoolDecoder<'_>, ctx: usize) -> SubMvRef {
    match d.read_tree(&SUB_MV_REF_TREE, &SUB_MV_REF_PROBS[ctx]) {
        0 => SubMvRef::Left4x4,
        1 => SubMvRef::Above4x4,
        2 => SubMvRef::Zero4x4,
        3 => SubMvRef::New4x4,
        v => unreachable!("sub_mv_ref tree produced {v}"),
    }
}

/// First subblock of each SPLITMV subset, per partition shape (libvpx
/// `vp8_mbsplit_offset`; shapes 16x8, 8x16, 8x8, 4x4).
pub const MBSPLIT_OFFSET: [[u8; 16]; 4] = [
    [0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 2, 8, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
];

/// Subblocks filled by one subset, per partition shape (libvpx
/// `mbsplit_fill_count`).
pub const MBSPLIT_FILL_COUNT: [usize; 4] = [8, 8, 4, 1];

/// Subblock indices each subset fills, per shape (libvpx
/// `mbsplit_fill_offset`; subset j starts at j * fill_count).
pub const MBSPLIT_FILL_OFFSET: [[u8; 16]; 4] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [0, 1, 4, 5, 8, 9, 12, 13, 2, 3, 6, 7, 10, 11, 14, 15],
    [0, 1, 4, 5, 2, 3, 6, 7, 8, 9, 12, 13, 10, 11, 14, 15],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
];

/// Decode one motion-vector component (§17.1, libvpx `read_mvcomponent`):
/// either a 3-bit small value off [`SMALL_MV_TREE`] or a 9-bit long form
/// whose bit 3 is sometimes implicit, then a sign flag.
pub fn read_mv_component(d: &mut BoolDecoder<'_>, probs: &[u8; MVP_COUNT]) -> i16 {
    let mut x: i32 = 0;
    if d.read_bool(probs[MVP_IS_SHORT]) {
        // Large: bits 0..2, then 8..4 (bit 3 is implicit below).
        for i in 0..3 {
            x += i32::from(d.read_bool(probs[MVP_BITS + i])) << i;
        }
        let mut i = 8;
        loop {
            x += i32::from(d.read_bool(probs[MVP_BITS + i])) << i;
            i -= 1;
            if i <= 3 {
                break;
            }
        }
        if (x & 0xFFF0) == 0 || d.read_bool(probs[MVP_BITS + 3]) {
            x += 8;
        }
    } else {
        x = i32::from(d.read_tree(&SMALL_MV_TREE, &probs[MVP_SHORT..MVP_SHORT + 7]));
    }
    if x != 0 && d.read_bool(probs[MVP_SIGN]) {
        x = -x;
    }
    x as i16
}

/// Both components of a whole-block motion vector (§17.1); the decoded
/// quarter-pel components double into the decoder's eighth-pel units.
pub fn read_mv(d: &mut BoolDecoder<'_>, probs: &[[u8; MVP_COUNT]; 2]) -> (i16, i16) {
    (
        read_mv_component(d, &probs[0]).wrapping_mul(2),
        read_mv_component(d, &probs[1]).wrapping_mul(2),
    )
}

/// Flip a candidate motion vector when the reference it was coded
/// against has a different sign bias than the current reference
/// (§16.3, libvpx `mv_bias`).
pub fn mv_bias(from_bias: bool, to_bias: bool, mv: (i16, i16)) -> (i16, i16) {
    if from_bias != to_bias {
        (-mv.0, -mv.1)
    } else {
        mv
    }
}

/// One neighbour macroblock's motion-vector census input (§16.3).
pub struct MvNeighbor {
    /// The MB is intra-coded (no MV contribution at all).
    pub intra: bool,
    /// The MB is inter-coded with a zero motion vector (counts like
    /// intra in the census).
    pub zero: bool,
    /// The MB's motion vector (already sign-bias-adjusted if applicable
    /// — pass `sign_bias` and the census applies it).
    pub mv: (i16, i16),
    /// The MB is SPLITMV (feeds the split census count).
    pub is_split: bool,
    /// The sign bias of the reference the MB's MV points into.
    pub sign_bias: bool,
}

/// Slot indices of the census result (§16.3).
pub const CNT_INTRA: usize = 0;
/// Slot index: nearest-candidate count.
pub const CNT_NEAREST: usize = 1;
/// Slot index: near-candidate count.
pub const CNT_NEAR: usize = 2;
/// Slot index: split-neighbour count.
pub const CNT_SPLITMV: usize = 3;

/// The neighborhood motion-vector census (§16.3, libvpx
/// `find_near_mvs` inlined in `read_mb_modes_mv`): up to three distinct
/// candidate vectors with per-slot counts, scanned above, left,
/// above-left; each inter neighbour counts 2/2/1 toward either the
/// candidate slot it extends or the zero/intra count.
pub fn find_near_mvs(
    above: MvNeighbor,
    left: MvNeighbor,
    aboveleft: MvNeighbor,
    to_bias: bool,
) -> ([(i16, i16); 4], [i32; 4]) {
    let mut mvs = [(0i16, 0i16); 4];
    let mut cnt = [0i32; 4];
    let mut idx = 0usize;
    let mut cntx = 0usize;

    // Above: the first candidate is pushed unconditionally.
    if !above.intra {
        if !above.zero {
            idx += 1;
            mvs[idx] = mv_bias(above.sign_bias, to_bias, above.mv);
            cntx += 1;
        }
        cnt[cntx] += 2;
    }
    for (mb, w) in [(&left, 2), (&aboveleft, 1)] {
        if !mb.intra {
            if !mb.zero {
                let this = mv_bias(mb.sign_bias, to_bias, mb.mv);
                if this != mvs[idx] {
                    idx += 1;
                    mvs[idx] = this;
                    cntx += 1;
                }
                cnt[cntx] += w;
            } else {
                cnt[CNT_INTRA] += i32::from(w);
            }
        }
    }

    // libvpx checks `cnt[CNT_SPLITMV]` BEFORE computing it (the write
    // happens later in the same branch), so the above-left merge is
    // effectively dead there; ported as-is with the same dead guard.
    if cnt[CNT_SPLITMV] != 0 && mvs[idx] == mvs[CNT_NEAREST] {
        cnt[CNT_NEAREST] += 1;
    }
    cnt[CNT_SPLITMV] = i32::from(above.is_split) * 2
        + i32::from(left.is_split) * 2
        + i32::from(aboveleft.is_split);

    if cnt[CNT_NEAR] > cnt[CNT_NEAREST] {
        cnt.swap(CNT_NEAR, CNT_NEAREST);
        mvs.swap(CNT_NEAR, CNT_NEAREST);
    }
    (mvs, cnt)
}

/// Per-MB motion-vector clamp edges in eighth-pels (libvpx
/// `mb_to_*_edge`): distance from the MB's edge to the frame edge.
pub fn mb_edges(col: usize, row: usize, mb_cols: usize, mb_rows: usize) -> (i32, i32, i32, i32) {
    (
        -((i32::try_from(col).unwrap() * 16) << 3),
        ((i32::try_from(mb_cols - 1 - col).unwrap() * 16) << 3),
        -((i32::try_from(row).unwrap() * 16) << 3),
        ((i32::try_from(mb_rows - 1 - row).unwrap() * 16) << 3),
    )
}

/// Clamp a candidate MV into the current MB's allowed range plus a
/// 128-eighth-pel margin (§16.2, libvpx `vp8_clamp_mv2`).
pub fn clamp_mv2(mv: &mut (i16, i16), edges: (i32, i32, i32, i32)) {
    let (l, r, t, b) = edges;
    let c = i32::from(mv.1);
    if c < l - 128 {
        mv.1 = (l - 128) as i16;
    } else if c > r + 128 {
        mv.1 = (r + 128) as i16;
    }
    let ro = i32::from(mv.0);
    if ro < t - 128 {
        mv.0 = (t - 128) as i16;
    } else if ro > b + 128 {
        mv.0 = (b + 128) as i16;
    }
}

/// Whether an explicit MV exceeds the MB's bounds (libvpx
/// `vp8_check_mv_bounds`): strictly outside, no margin.
pub fn mv_out_of_bounds(mv: (i16, i16), edges: (i32, i32, i32, i32)) -> bool {
    let (l, r, t, b) = edges;
    i32::from(mv.1) < l || i32::from(mv.1) > r || i32::from(mv.0) < t || i32::from(mv.0) > b
}

/// Chroma motion vector for a whole-MB mode (libvpx
/// `vp8_build_inter16x16_predictors_mb`): halve each component with
/// round-half-away-from-zero (`x += 1 | (x >> 31); x /= 2`).
pub fn chroma_mv_whole(mv: (i16, i16)) -> (i16, i16) {
    fn half(x: i16) -> i16 {
        let x = i32::from(x);
        ((x + (1 | (x >> 31))) / 2) as i16
    }
    (half(mv.0), half(mv.1))
}

/// Chroma motion vector for one 8x8 quadrant of a SPLITMB (libvpx
/// `build_4x4uvmvs`): sum the quadrant's four subblock vectors, then
/// divide by 8 with round-half-away-from-zero
/// (`t += 4 + (t >> 31) * 8; t /= 8`).
pub fn chroma_mv_split(quadrant: &[(i16, i16); 4]) -> (i16, i16) {
    fn eighth(parts: [i16; 4]) -> i16 {
        let t = i32::from(parts[0])
            + i32::from(parts[1])
            + i32::from(parts[2])
            + i32::from(parts[3]);
        let t = t + 4 + (t >> 31) * 8;
        (t / 8) as i16
    }
    (
        eighth([quadrant[0].0, quadrant[1].0, quadrant[2].0, quadrant[3].0]),
        eighth([quadrant[0].1, quadrant[1].1, quadrant[2].1, quadrant[3].1]),
    )
}
