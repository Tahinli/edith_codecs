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
pub const MV_REF_TREE: [i8; 8] = [0, 2, -1, 4, -2, 6, -3, -4];

/// Motion-vector component: row then column, each with the 19-probability
/// table (§17.1).
pub const MVP_IS_SHORT: usize = 0;
pub const MVP_SIGN: usize = 1;
pub const MVP_SHORT: usize = 2;
pub const MVP_BITS: usize = MVP_SHORT + 7;
pub const MVP_COUNT: usize = MVP_BITS + 10;

/// Small-value coding tree for MV components (§17.1 `small_mvtree`).
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
