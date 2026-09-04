//! The inverse transform (spec 7.13), so the encoder can reconstruct exactly
//! what a decoder will show.
//!
//! Only the inverse DCT is here, which is all this encoder's subset codes: a
//! square `DCT_DCT` transform per block. The butterfly network is the spec's
//! own, stage for stage — every stage's rounding and intermediate clamp is
//! part of the bitstream contract, so a mathematically equal but differently
//! rounded DCT would drift away from the decoder by a sample here and there.

/// `Cos128_Lookup` (spec 7.13.2.1): `4096 * cos(angle * pi / 128)` rounded.
const COS128_LOOKUP: [i32; 65] = [
    4096, 4095, 4091, 4085, 4076, 4065, 4052, 4036, 4017, 3996, 3973, 3948, 3920, 3889, 3857, 3822,
    3784, 3745, 3703, 3659, 3612, 3564, 3513, 3461, 3406, 3349, 3290, 3229, 3166, 3102, 3035, 2967,
    2896, 2824, 2751, 2675, 2598, 2520, 2440, 2359, 2276, 2191, 2106, 2019, 1931, 1842, 1751, 1660,
    1567, 1474, 1380, 1285, 1189, 1092, 995, 897, 799, 700, 601, 501, 401, 301, 201, 101, 0,
];

/// The fixed-point position of the cosine table.
const COS_BITS: usize = 12;

/// [`COS128_LOOKUP`] unfolded to the whole circle at compile time -- the same
/// four quadrant cases the spec writes, evaluated once here so a butterfly's
/// cosine is one load instead of a four-way branch per call.
const fn build_cos128_full() -> [i32; 256] {
    let mut t = [0i32; 256];
    let mut a = 0usize;
    while a < 256 {
        t[a] = if a <= 64 {
            COS128_LOOKUP[a]
        } else if a <= 128 {
            -COS128_LOOKUP[128 - a]
        } else if a <= 192 {
            -COS128_LOOKUP[a - 128]
        } else {
            COS128_LOOKUP[256 - a]
        };
        a += 1;
    }
    t
}

static COS128_FULL: [i32; 256] = build_cos128_full();

/// `cos128(angle)` (spec 7.13.2.1), folded from the quarter table.
fn cos128(angle: i32) -> i32 {
    COS128_FULL[(angle & 255) as usize]
}

/// `sin128(angle)` (spec 7.13.2.1).
fn sin128(angle: i32) -> i32 {
    cos128(angle - 64)
}

/// `brev(numBits, x)` (spec 7.13.2.1) tabulated at compile time, one row per
/// `num_bits`. The bit-fold ran per butterfly index at runtime and was the
/// single hottest instruction sequence in `inverse_dct`'s annotation.
const fn build_brev(num_bits: u32) -> [u8; 64] {
    let mut t = [0u8; 64];
    let n0 = 1usize << num_bits;
    let mut x = 0usize;
    while x < n0 {
        let mut v = 0usize;
        let mut i = 0u32;
        while i < num_bits {
            v |= ((x >> i) & 1) << (num_bits - 1 - i);
            i += 1;
        }
        t[x] = v as u8;
        x += 1;
    }
    t
}

static BREV_TAB: [[u8; 64]; 7] = [
    build_brev(0),
    build_brev(1),
    build_brev(2),
    build_brev(3),
    build_brev(4),
    build_brev(5),
    build_brev(6),
];

/// `brev(numBits, x)` (spec 7.13.2.1): the bit reversal of the low `num_bits`.
fn brev(num_bits: u32, x: usize) -> usize {
    BREV_TAB[num_bits as usize][x] as usize
}

/// `Round2(x, n)` (spec 4.7): shift down `n` places, rounding halves up.
fn round2(x: i32, n: usize) -> i32 {
    if n == 0 {
        x
    } else {
        // The intermediate can exceed 32 bits before the shift brings it
        // back, which is why it is done in 64.
        ((i64::from(x) + (1i64 << (n - 1))) >> n) as i32
    }
}

/// `Clip3` against a signed integer of `r` bits, which is what bounds every
/// intermediate of the transform.
fn clamp_range(x: i32, r: usize) -> i32 {
    let hi = ((1i64 << (r - 1)) - 1) as i32;
    let lo = (-(1i64 << (r - 1))) as i32;
    x.clamp(lo, hi)
}

/// `B(a, b, angle, flip, r)` (spec 7.13.2.1): a butterfly rotation, and an
/// exchange of the two entries when `flip` is set.
fn butterfly(t: &mut [i32; 64], a: usize, b: usize, angle: i32, flip: bool, _r: usize) {
    let (ta, tb) = (i64::from(t[a]), i64::from(t[b]));
    let (c, s) = (i64::from(cos128(angle)), i64::from(sin128(angle)));
    let x = ta * c - tb * s;
    let y = ta * s + tb * c;
    t[a] = ((x + (1 << (COS_BITS - 1))) >> COS_BITS) as i32;
    t[b] = ((y + (1 << (COS_BITS - 1))) >> COS_BITS) as i32;
    if flip {
        t.swap(a, b);
    }
}

/// `H(a, b, flip, r)` (spec 7.13.2.1): a Hadamard rotation, with the indices
/// exchanged when `flip` is set.
fn hadamard(t: &mut [i32; 64], a: usize, b: usize, flip: bool, r: usize) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let (x, y) = (t[a], t[b]);
    t[a] = clamp_range(x.wrapping_add(y), r);
    t[b] = clamp_range(x.wrapping_sub(y), r);
}

/// The inverse DCT array permutation (spec 7.13.2.2): an in-place bit-reversal
/// of the first `1 << n` entries.
fn permute(t: &mut [i32; 64], n: u32) {
    let n0 = 1usize << n;
    // A stack copy: this ran once per 1D transform and its `Vec` was the
    // `malloc` at the top of every `inverse_dct` frame in the annotation.
    let mut copy = [0i32; 64];
    copy[..n0].copy_from_slice(&t[..n0]);
    let tab = &BREV_TAB[n as usize];
    for (i, v) in t[..n0].iter_mut().enumerate() {
        *v = copy[tab[i] as usize];
    }
}

/// The inverse DCT process (spec 7.13.2.3): an in-place inverse DCT of the
/// first `1 << n` entries of `t`, clamping intermediates to `r` bits.
///
/// The stage list is the spec's, in the spec's order; the guards on `n` are
/// what make one network serve every transform size from 4 to 64.
pub fn inverse_dct(t: &mut [i32], n: u32, r: usize) {
    // lane-perf10: the network below indexes a 64-entry scratch with computed
    // indices, and on a `&mut [i32]` every one of them carried a bounds check.
    // A `&mut [i32; 64]` lets the optimiser discharge them all against the
    // constant length; this wrapper keeps the slice-taking signature for
    // callers outside the 2D driver.
    let m = 1usize << n;
    let mut fixed = [0i32; 64];
    fixed[..m].copy_from_slice(&t[..m]);
    inverse_dct_fixed(&mut fixed, n, r);
    t[..m].copy_from_slice(&fixed[..m]);
}

/// [`inverse_dct`] on the driver's own 64-entry scratch.
fn inverse_dct_fixed(t: &mut [i32; 64], n: u32, r: usize) {
    assert!((2..=6).contains(&n), "the inverse DCT is defined for 4..64");
    permute(t, n);

    if n == 6 {
        for i in 0..16 {
            butterfly(t, 32 + i, 63 - i, 63 - 4 * brev(4, i) as i32, false, r);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            butterfly(
                t,
                16 + i,
                31 - i,
                6 + ((brev(3, 7 - i) as i32) << 3),
                false,
                r,
            );
        }
    }
    if n == 6 {
        for i in 0..16 {
            hadamard(t, 32 + i * 2, 33 + i * 2, i & 1 == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            butterfly(
                t,
                8 + i,
                15 - i,
                12 + ((brev(2, 3 - i) as i32) << 4),
                false,
                r,
            );
        }
    }
    if n >= 5 {
        for i in 0..8 {
            hadamard(t, 16 + 2 * i, 17 + 2 * i, i & 1 == 1, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..2 {
                let angle = 60 - 16 * brev(2, i) as i32 + 64 * j as i32;
                butterfly(t, 62 - i * 4 - j, 33 + i * 4 + j, angle, true, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..2 {
            butterfly(t, 4 + i, 7 - i, 56 - 32 * i as i32, false, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            hadamard(t, 8 + 2 * i, 9 + 2 * i, i & 1 == 1, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..2 {
                let angle = 24 + ((j as i32) << 6) + ((1 - i as i32) << 5);
                butterfly(t, 30 - 4 * i - j, 17 + 4 * i + j, angle, true, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            for j in 0..2 {
                hadamard(t, 32 + i * 4 + j, 35 + i * 4 - j, i & 1 == 1, r);
            }
        }
    }
    for i in 0..2 {
        butterfly(t, 2 * i, 2 * i + 1, 32 + 16 * i as i32, i == 0, r);
    }
    if n >= 3 {
        for i in 0..2 {
            hadamard(t, 4 + 2 * i, 5 + 2 * i, i == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            butterfly(t, 14 - i, 9 + i, 48 + 64 * i as i32, true, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            for j in 0..2 {
                hadamard(t, 16 + 4 * i + j, 19 + 4 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..2 {
            for j in 0..4 {
                let angle = 56 - (i as i32) * 32 + ((j as i32) >> 1) * 64;
                butterfly(t, 61 - i * 8 - j, 34 + i * 8 + j, angle, true, r);
            }
        }
    }
    for i in 0..2 {
        hadamard(t, i, 3 - i, false, r);
    }
    if n >= 3 {
        butterfly(t, 6, 5, 32, true, r);
    }
    if n >= 4 {
        for i in 0..2 {
            for j in 0..2 {
                hadamard(t, 8 + 4 * i + j, 11 + 4 * i - j, i == 1, r);
            }
        }
    }
    if n >= 5 {
        for i in 0..4 {
            butterfly(t, 29 - i, 18 + i, 48 + ((i as i32) >> 1) * 64, true, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..4 {
                hadamard(t, 32 + 8 * i + j, 39 + 8 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..4 {
            hadamard(t, i, 7 - i, false, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            butterfly(t, 13 - i, 10 + i, 32, true, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..4 {
                hadamard(t, 16 + i * 8 + j, 23 + i * 8 - j, i == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            butterfly(t, 59 - i, 36 + i, if i < 4 { 48 } else { 112 }, true, r);
        }
    }
    if n >= 4 {
        for i in 0..8 {
            hadamard(t, i, 15 - i, false, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            butterfly(t, 27 - i, 20 + i, 32, true, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            hadamard(t, 32 + i, 47 - i, false, r);
            hadamard(t, 48 + i, 63 - i, true, r);
        }
    }
    if n >= 5 {
        for i in 0..16 {
            hadamard(t, i, 31 - i, false, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            butterfly(t, 55 - i, 40 + i, 32, true, r);
        }
    }
    if n == 6 {
        for i in 0..32 {
            hadamard(t, i, 63 - i, false, r);
        }
    }
}

/// `Transform_Row_Shift` (spec 7.13.3) / `av1_inv_txfm_shift_ls`
/// (`av1_inv_txfm2d.c:132-158`): `shift[0]`, keyed on the full `(w, h)` TX
/// size, not a single log2 -- the five square values (unchanged from the
/// pre-lane `row_shift(log2)`) plus the fourteen rectangular ones the lane
/// charter transcribed from the same table.
fn row_shift_wh(w: usize, h: usize) -> usize {
    match (w, h) {
        (4, 4) => 0,
        (8, 8) => 1,
        (16, 16) | (32, 32) | (64, 64) => 2,
        (4, 8) | (8, 4) => 0,
        (8, 16) | (16, 8) => 1,
        (16, 32) | (32, 16) => 1,
        (32, 64) | (64, 32) => 1,
        (4, 16) | (16, 4) => 1,
        (8, 32) | (32, 8) => 2,
        (16, 64) | (64, 16) => 2,
        _ => unreachable!("unsupported transform size {w}x{h}"),
    }
}

/// One 1D inverse transform family, ported from libaom's `av1_inv_txfm1d.c`
/// (`av1_iadst4`/`8`/`16`, `av1_iidentity*`) rather than derived from the
/// spec's own butterfly writeup -- see the ADST family below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TxType1d {
    Dct,
    Adst,
    Identity,
}

/// `Tx_Type_Intra_Inv_Set2`'s five members (spec 9.3), in the CDF's own
/// symbol order (`0..=4`): `IDTX, DCT_DCT, ADST_ADST, ADST_DCT, DCT_ADST`.
/// [`crate::decode`] reads the symbol; this is what it dispatches to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TxType {
    /// Identity on both axes.
    Idtx,
    /// DCT on both axes -- this crate's original, still most common case.
    DctDct,
    /// ADST on both axes.
    AdstAdst,
    /// ADST down the column axis, DCT across the row axis.
    AdstDct,
    /// DCT down the column axis, ADST across the row axis.
    DctAdst,
    /// DCT down the column axis, identity across the row axis
    /// (`V_DCT` -- `TX_SET_INTRA_1` only).
    VDct,
    /// Identity down the column axis, DCT across the row axis
    /// (`H_DCT` -- `TX_SET_INTRA_1` only).
    HDct,
    /// FLIPADST down the column axis, DCT across the row axis (lane-cdffwd2:
    /// reachable once `reduced_tx_set == false`'s wider inter sets are read
    /// -- `EXT_TX_SET_DTT9_IDTX_1DDCT`/`EXT_TX_SET_ALL16`). FLIPADST is the
    /// same 1D ADST kernel [`AdstDct`] uses (`vtx_tab`/`htx_tab`,
    /// `common_data.h`), just read/written flipped -- see [`TxType::flip`].
    FlipAdstDct,
    /// DCT down the column axis, FLIPADST across the row axis.
    DctFlipAdst,
    /// FLIPADST on both axes.
    FlipAdstFlipAdst,
    /// ADST down the column axis, FLIPADST across the row axis.
    AdstFlipAdst,
    /// FLIPADST down the column axis, ADST across the row axis.
    FlipAdstAdst,
    /// ADST down the column axis, identity across the row axis (`V_ADST` --
    /// `EXT_TX_SET_ALL16` only).
    VAdst,
    /// Identity down the column axis, ADST across the row axis (`H_ADST`).
    HAdst,
    /// FLIPADST down the column axis, identity across the row axis
    /// (`V_FLIPADST`).
    VFlipAdst,
    /// Identity down the column axis, FLIPADST across the row axis
    /// (`H_FLIPADST`).
    HFlipAdst,
}

impl TxType {
    /// The inverse of `Tx_Type_Intra_Inv_Set2`'s CDF symbol order.
    pub fn from_symbol(t: usize) -> Option<Self> {
        match t {
            0 => Some(Self::Idtx),
            1 => Some(Self::DctDct),
            2 => Some(Self::AdstAdst),
            3 => Some(Self::AdstDct),
            4 => Some(Self::DctAdst),
            _ => None,
        }
    }

    /// The inverse of `Tx_Type_Intra_Inv_Set1`'s CDF symbol order
    /// (`av1_ext_tx_inv[EXT_TX_SET_DTT4_IDTX_1DDCT]`, entropymode.h:
    /// `{9, 0, 10, 11, 3, 1, 2}` = IDTX, DCT_DCT, V_DCT, H_DCT, ADST_ADST,
    /// ADST_DCT, DCT_ADST) -- the seven-type set a `reduced_tx_set == 0`
    /// frame's small intra transforms read from.
    pub fn from_symbol_set1(t: usize) -> Option<Self> {
        match t {
            0 => Some(Self::Idtx),
            1 => Some(Self::DctDct),
            2 => Some(Self::VDct),
            3 => Some(Self::HDct),
            4 => Some(Self::AdstAdst),
            5 => Some(Self::AdstDct),
            6 => Some(Self::DctAdst),
            _ => None,
        }
    }

    /// The inverse of `Tx_Type_Inter_Inv_Set2`'s CDF symbol order
    /// (`av1_ext_tx_inv[EXT_TX_SET_DTT9_IDTX_1DDCT]`, `entropymode.h`:
    /// `{9, 10, 11, 0, 1, 2, 4, 5, 3, 6, 7, 8}`) -- the twelve-type set a
    /// `reduced_tx_set == 0` frame's 16x16 *inter* transform reads from
    /// (`av1_get_ext_tx_set_type`'s `tx_size_sqr == TX_16X16` branch of
    /// `av1_ext_tx_set_lookup[1]`).
    pub fn from_symbol_set2_12(t: usize) -> Option<Self> {
        match t {
            0 => Some(Self::Idtx),
            1 => Some(Self::VDct),
            2 => Some(Self::HDct),
            3 => Some(Self::DctDct),
            4 => Some(Self::AdstDct),
            5 => Some(Self::DctAdst),
            6 => Some(Self::FlipAdstDct),
            7 => Some(Self::DctFlipAdst),
            8 => Some(Self::AdstAdst),
            9 => Some(Self::FlipAdstFlipAdst),
            10 => Some(Self::AdstFlipAdst),
            11 => Some(Self::FlipAdstAdst),
            _ => None,
        }
    }

    /// The inverse of `Tx_Type_Inter_Inv_Set1`'s CDF symbol order
    /// (`av1_ext_tx_inv[EXT_TX_SET_ALL16]`, `entropymode.h`:
    /// `{9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 4, 5, 3, 6, 7, 8}`) -- the full
    /// sixteen-type set a `reduced_tx_set == 0` frame's 8x8 (and 4x4) inter
    /// transform reads from (`av1_ext_tx_set_lookup[1][0]`).
    pub fn from_symbol_all16(t: usize) -> Option<Self> {
        match t {
            0 => Some(Self::Idtx),
            1 => Some(Self::VDct),
            2 => Some(Self::HDct),
            3 => Some(Self::VAdst),
            4 => Some(Self::HAdst),
            5 => Some(Self::VFlipAdst),
            6 => Some(Self::HFlipAdst),
            7 => Some(Self::DctDct),
            8 => Some(Self::AdstDct),
            9 => Some(Self::DctAdst),
            10 => Some(Self::FlipAdstDct),
            11 => Some(Self::DctFlipAdst),
            12 => Some(Self::AdstAdst),
            13 => Some(Self::FlipAdstFlipAdst),
            14 => Some(Self::AdstFlipAdst),
            15 => Some(Self::FlipAdstAdst),
            _ => None,
        }
    }

    /// `(row, col)`: the transform `inverse_transform_2d_typed`'s row pass
    /// (`htx_tab`, spec/libaom "horizontal") and column pass (`vtx_tab`,
    /// "vertical") each run. Read off libaom's `av1_inv_txfm2d.c` `vtx_tab`/
    /// `htx_tab` tables directly, not derived: e.g. `ADST_DCT` has
    /// `vtx_tab = ADST_1D` (column), `htx_tab = DCT_1D` (row) -- the name's
    /// first half is the *column* transform, not the row one.
    fn axes(self) -> (TxType1d, TxType1d) {
        use TxType1d::{Adst, Dct, Identity};
        match self {
            Self::Idtx => (Identity, Identity),
            Self::DctDct => (Dct, Dct),
            Self::AdstAdst => (Adst, Adst),
            Self::AdstDct => (Dct, Adst),
            Self::DctAdst => (Adst, Dct),
            // `av1_inv_txfm2d.c`: V_DCT vtx=DCT htx=IDTX, H_DCT the mirror.
            Self::VDct => (Identity, Dct),
            Self::HDct => (Dct, Identity),
            // FLIPADST reads the same 1D ADST kernel as ADST -- `vtx_tab`/
            // `htx_tab` (`common_data.h`) map `FLIPADST_1D` and `ADST_1D`
            // to the exact same butterfly network; only [`Self::flip`]
            // differs. So every FLIPADST-bearing axis below mirrors its
            // plain-ADST counterpart's kernel pairing.
            Self::FlipAdstDct => (Dct, Adst),
            Self::DctFlipAdst => (Adst, Dct),
            Self::FlipAdstFlipAdst | Self::AdstFlipAdst | Self::FlipAdstAdst => (Adst, Adst),
            Self::VAdst | Self::VFlipAdst => (Identity, Adst),
            Self::HAdst | Self::HFlipAdst => (Adst, Identity),
        }
    }

    /// `(ud_flip, lr_flip)` (`get_flip_cfg`, `av1_txfm.h`): whether the
    /// column pass reads its input right-to-left and/or the finished
    /// residual is written bottom-to-top. Every FLIPADST-bearing type sets
    /// one or both; every other type is `(false, false)`.
    fn flip(self) -> (bool, bool) {
        match self {
            Self::FlipAdstDct | Self::FlipAdstAdst | Self::VFlipAdst => (true, false),
            Self::DctFlipAdst | Self::AdstFlipAdst | Self::HFlipAdst => (false, true),
            Self::FlipAdstFlipAdst => (true, true),
            _ => (false, false),
        }
    }
}

/// `sinpi[]` at `cos_bit = 12` (`av1_sinpi_arr_data[2]`, `av1_txfm.c`): the
/// fixed constants [`inverse_adst4`] alone uses, unrelated to [`cos128`].
const SINPI: [i64; 5] = [0, 1321, 2482, 3344, 3803];

/// `NewSqrt2`/`NewSqrt2Bits` (`av1_txfm.h`): the `sqrt(2)` scale
/// [`inverse_identity`] uses at sizes 4 and 16.
const NEW_SQRT2: i64 = 5793;
const NEW_SQRT2_BITS: u32 = 12;

/// `round_shift` (`av1_txfm.h`): round-to-nearest to `bit` places, taking the
/// pre-shift value in 64 bits since the ADST/identity intermediates
/// routinely exceed 32.
fn round_shift(x: i64, bit: u32) -> i32 {
    ((x + (1i64 << (bit - 1))) >> bit) as i32
}

/// `half_btf(w0, in0, w1, in1, bit)` (`av1_txfm.h`) at `bit = COS_BITS`,
/// the only value AV1's inverse transforms ever pass (`INV_COS_BIT`).
fn half_btf(w0: i32, in0: i32, w1: i32, in1: i32) -> i32 {
    let acc = i64::from(w0) * i64::from(in0) + i64::from(w1) * i64::from(in1);
    round_shift(acc, COS_BITS as u32)
}

/// `av1_iadst4` (`av1_inv_txfm1d.c`): every intermediate stage there goes
/// through `range_check_value`, which is a debug-only no-op in a conformant
/// decoder (unlike `av1_iadst8`/`16`'s `clamp_value`, a real clamp) -- so
/// this port carries no [`clamp_range`] calls, matching the reference
/// exactly rather than adding a clamp libaom's own default build never
/// takes.
fn inverse_adst4(t: &mut [i32]) {
    let (x0, x1, x2, x3) = (
        i64::from(t[0]),
        i64::from(t[1]),
        i64::from(t[2]),
        i64::from(t[3]),
    );
    if x0 == 0 && x1 == 0 && x2 == 0 && x3 == 0 {
        t[0] = 0;
        t[1] = 0;
        t[2] = 0;
        t[3] = 0;
        return;
    }
    let mut s0 = SINPI[1] * x0;
    let mut s1 = SINPI[2] * x0;
    let s2 = SINPI[3] * x1;
    let mut s3 = SINPI[4] * x2;
    let s4 = SINPI[1] * x2;
    let s5 = SINPI[2] * x3;
    let s6 = SINPI[4] * x3;
    let s7 = (x0 - x2) + x3;

    s0 += s3;
    s1 -= s4;
    s3 = s2;
    let s2 = SINPI[3] * s7;
    s0 += s5;
    s1 -= s6;

    let o0 = s0 + s3;
    let o1 = s1 + s3;
    let o2 = s2;
    let o3 = (s0 + s1) - s3;

    t[0] = round_shift(o0, COS_BITS as u32);
    t[1] = round_shift(o1, COS_BITS as u32);
    t[2] = round_shift(o2, COS_BITS as u32);
    t[3] = round_shift(o3, COS_BITS as u32);
}

/// `av1_iadst8` (`av1_inv_txfm1d.c`), stage for stage; `r` is the same
/// per-stage clamp bit width `av1_gen_inv_stage_range` hands every stage in
/// the square, 8-bit-depth case this decoder targets (row and column both
/// land on 16 there, matching [`inverse_transform_2d_typed`]'s own
/// `row_clamp`/`col_clamp`).
fn inverse_adst8(t: &mut [i32], r: usize) {
    let bf1 = [t[7], t[0], t[5], t[2], t[3], t[4], t[1], t[6]];

    let mut step = [0i32; 8];
    step[0] = half_btf(cos128(4), bf1[0], cos128(60), bf1[1]);
    step[1] = half_btf(cos128(60), bf1[0], -cos128(4), bf1[1]);
    step[2] = half_btf(cos128(20), bf1[2], cos128(44), bf1[3]);
    step[3] = half_btf(cos128(44), bf1[2], -cos128(20), bf1[3]);
    step[4] = half_btf(cos128(36), bf1[4], cos128(28), bf1[5]);
    step[5] = half_btf(cos128(28), bf1[4], -cos128(36), bf1[5]);
    step[6] = half_btf(cos128(52), bf1[6], cos128(12), bf1[7]);
    step[7] = half_btf(cos128(12), bf1[6], -cos128(52), bf1[7]);

    let mut out = [0i32; 8];
    for i in 0..4 {
        out[i] = clamp_range(step[i] + step[i + 4], r);
        out[i + 4] = clamp_range(step[i] - step[i + 4], r);
    }

    step[0] = out[0];
    step[1] = out[1];
    step[2] = out[2];
    step[3] = out[3];
    step[4] = half_btf(cos128(16), out[4], cos128(48), out[5]);
    step[5] = half_btf(cos128(48), out[4], -cos128(16), out[5]);
    step[6] = half_btf(-cos128(48), out[6], cos128(16), out[7]);
    step[7] = half_btf(cos128(16), out[6], cos128(48), out[7]);

    out[0] = clamp_range(step[0] + step[2], r);
    out[1] = clamp_range(step[1] + step[3], r);
    out[2] = clamp_range(step[0] - step[2], r);
    out[3] = clamp_range(step[1] - step[3], r);
    out[4] = clamp_range(step[4] + step[6], r);
    out[5] = clamp_range(step[5] + step[7], r);
    out[6] = clamp_range(step[4] - step[6], r);
    out[7] = clamp_range(step[5] - step[7], r);

    step[0] = out[0];
    step[1] = out[1];
    step[2] = half_btf(cos128(32), out[2], cos128(32), out[3]);
    step[3] = half_btf(cos128(32), out[2], -cos128(32), out[3]);
    step[4] = out[4];
    step[5] = out[5];
    step[6] = half_btf(cos128(32), out[6], cos128(32), out[7]);
    step[7] = half_btf(cos128(32), out[6], -cos128(32), out[7]);

    t[0] = step[0];
    t[1] = -step[4];
    t[2] = step[6];
    t[3] = -step[2];
    t[4] = step[3];
    t[5] = -step[7];
    t[6] = step[5];
    t[7] = -step[1];
}

/// `av1_iadst16` (`av1_inv_txfm1d.c`), stage for stage; `r` as
/// [`inverse_adst8`].
fn inverse_adst16(t: &mut [i32], r: usize) {
    let inp: [i32; 16] = t[..16].try_into().expect("16-point input");
    let bf1 = [
        inp[15], inp[0], inp[13], inp[2], inp[11], inp[4], inp[9], inp[6], inp[7], inp[8], inp[5],
        inp[10], inp[3], inp[12], inp[1], inp[14],
    ];

    let mut step = [0i32; 16];
    step[0] = half_btf(cos128(2), bf1[0], cos128(62), bf1[1]);
    step[1] = half_btf(cos128(62), bf1[0], -cos128(2), bf1[1]);
    step[2] = half_btf(cos128(10), bf1[2], cos128(54), bf1[3]);
    step[3] = half_btf(cos128(54), bf1[2], -cos128(10), bf1[3]);
    step[4] = half_btf(cos128(18), bf1[4], cos128(46), bf1[5]);
    step[5] = half_btf(cos128(46), bf1[4], -cos128(18), bf1[5]);
    step[6] = half_btf(cos128(26), bf1[6], cos128(38), bf1[7]);
    step[7] = half_btf(cos128(38), bf1[6], -cos128(26), bf1[7]);
    step[8] = half_btf(cos128(34), bf1[8], cos128(30), bf1[9]);
    step[9] = half_btf(cos128(30), bf1[8], -cos128(34), bf1[9]);
    step[10] = half_btf(cos128(42), bf1[10], cos128(22), bf1[11]);
    step[11] = half_btf(cos128(22), bf1[10], -cos128(42), bf1[11]);
    step[12] = half_btf(cos128(50), bf1[12], cos128(14), bf1[13]);
    step[13] = half_btf(cos128(14), bf1[12], -cos128(50), bf1[13]);
    step[14] = half_btf(cos128(58), bf1[14], cos128(6), bf1[15]);
    step[15] = half_btf(cos128(6), bf1[14], -cos128(58), bf1[15]);

    let mut out = [0i32; 16];
    for i in 0..8 {
        out[i] = clamp_range(step[i] + step[i + 8], r);
        out[i + 8] = clamp_range(step[i] - step[i + 8], r);
    }

    step[..8].copy_from_slice(&out[..8]);
    step[8] = half_btf(cos128(8), out[8], cos128(56), out[9]);
    step[9] = half_btf(cos128(56), out[8], -cos128(8), out[9]);
    step[10] = half_btf(cos128(40), out[10], cos128(24), out[11]);
    step[11] = half_btf(cos128(24), out[10], -cos128(40), out[11]);
    step[12] = half_btf(-cos128(56), out[12], cos128(8), out[13]);
    step[13] = half_btf(cos128(8), out[12], cos128(56), out[13]);
    step[14] = half_btf(-cos128(24), out[14], cos128(40), out[15]);
    step[15] = half_btf(cos128(40), out[14], cos128(24), out[15]);

    out[0] = clamp_range(step[0] + step[4], r);
    out[1] = clamp_range(step[1] + step[5], r);
    out[2] = clamp_range(step[2] + step[6], r);
    out[3] = clamp_range(step[3] + step[7], r);
    out[4] = clamp_range(step[0] - step[4], r);
    out[5] = clamp_range(step[1] - step[5], r);
    out[6] = clamp_range(step[2] - step[6], r);
    out[7] = clamp_range(step[3] - step[7], r);
    out[8] = clamp_range(step[8] + step[12], r);
    out[9] = clamp_range(step[9] + step[13], r);
    out[10] = clamp_range(step[10] + step[14], r);
    out[11] = clamp_range(step[11] + step[15], r);
    out[12] = clamp_range(step[8] - step[12], r);
    out[13] = clamp_range(step[9] - step[13], r);
    out[14] = clamp_range(step[10] - step[14], r);
    out[15] = clamp_range(step[11] - step[15], r);

    step[0] = out[0];
    step[1] = out[1];
    step[2] = out[2];
    step[3] = out[3];
    step[4] = half_btf(cos128(16), out[4], cos128(48), out[5]);
    step[5] = half_btf(cos128(48), out[4], -cos128(16), out[5]);
    step[6] = half_btf(-cos128(48), out[6], cos128(16), out[7]);
    step[7] = half_btf(cos128(16), out[6], cos128(48), out[7]);
    step[8] = out[8];
    step[9] = out[9];
    step[10] = out[10];
    step[11] = out[11];
    step[12] = half_btf(cos128(16), out[12], cos128(48), out[13]);
    step[13] = half_btf(cos128(48), out[12], -cos128(16), out[13]);
    step[14] = half_btf(-cos128(48), out[14], cos128(16), out[15]);
    step[15] = half_btf(cos128(16), out[14], cos128(48), out[15]);

    out[0] = clamp_range(step[0] + step[2], r);
    out[1] = clamp_range(step[1] + step[3], r);
    out[2] = clamp_range(step[0] - step[2], r);
    out[3] = clamp_range(step[1] - step[3], r);
    out[4] = clamp_range(step[4] + step[6], r);
    out[5] = clamp_range(step[5] + step[7], r);
    out[6] = clamp_range(step[4] - step[6], r);
    out[7] = clamp_range(step[5] - step[7], r);
    out[8] = clamp_range(step[8] + step[10], r);
    out[9] = clamp_range(step[9] + step[11], r);
    out[10] = clamp_range(step[8] - step[10], r);
    out[11] = clamp_range(step[9] - step[11], r);
    out[12] = clamp_range(step[12] + step[14], r);
    out[13] = clamp_range(step[13] + step[15], r);
    out[14] = clamp_range(step[12] - step[14], r);
    out[15] = clamp_range(step[13] - step[15], r);

    step[0] = out[0];
    step[1] = out[1];
    step[2] = half_btf(cos128(32), out[2], cos128(32), out[3]);
    step[3] = half_btf(cos128(32), out[2], -cos128(32), out[3]);
    step[4] = out[4];
    step[5] = out[5];
    step[6] = half_btf(cos128(32), out[6], cos128(32), out[7]);
    step[7] = half_btf(cos128(32), out[6], -cos128(32), out[7]);
    step[8] = out[8];
    step[9] = out[9];
    step[10] = half_btf(cos128(32), out[10], cos128(32), out[11]);
    step[11] = half_btf(cos128(32), out[10], -cos128(32), out[11]);
    step[12] = out[12];
    step[13] = out[13];
    step[14] = half_btf(cos128(32), out[14], cos128(32), out[15]);
    step[15] = half_btf(cos128(32), out[14], -cos128(32), out[15]);

    t[0] = step[0];
    t[1] = -step[8];
    t[2] = step[12];
    t[3] = -step[4];
    t[4] = step[6];
    t[5] = -step[14];
    t[6] = step[10];
    t[7] = -step[2];
    t[8] = step[3];
    t[9] = -step[11];
    t[10] = step[15];
    t[11] = -step[7];
    t[12] = step[5];
    t[13] = -step[13];
    t[14] = step[9];
    t[15] = -step[1];
}

/// `av1_iidentity4_c`/`8_c`/`16_c`/`32_c` (`av1_inv_txfm1d.c`): a fixed
/// per-size scale, `sqrt(2)`-based at 4 and 16 (`NewSqrt2`/`NewSqrt2Bits`),
/// exact doubling at 8 and 32.
fn inverse_identity(t: &mut [i32], side: usize) {
    match side {
        4 => {
            for v in &mut t[..4] {
                *v = round_shift(NEW_SQRT2 * i64::from(*v), NEW_SQRT2_BITS);
            }
        }
        8 => {
            for v in &mut t[..8] {
                *v = (i64::from(*v) * 2) as i32;
            }
        }
        16 => {
            for v in &mut t[..16] {
                *v = round_shift(NEW_SQRT2 * 2 * i64::from(*v), NEW_SQRT2_BITS);
            }
        }
        32 => {
            for v in &mut t[..32] {
                *v = (i64::from(*v) * 4) as i32;
            }
        }
        _ => unreachable!("identity is defined at sizes 4, 8, 16 and 32"),
    }
}

/// Dispatches one row or column's 1D inverse transform by [`TxType1d`],
/// `log2` the spec's transform-size log (as [`inverse_dct`] takes).
fn inverse_1d(t: &mut [i32; 64], log2: u32, r: usize, kind: TxType1d) {
    match kind {
        TxType1d::Dct => inverse_dct_fixed(t, log2, r),
        TxType1d::Adst => match log2 {
            2 => inverse_adst4(t),
            3 => inverse_adst8(t, r),
            4 => inverse_adst16(t, r),
            _ => unreachable!("ADST is undefined at sizes above 16 (spec: 32+ uses DCT/identity)"),
        },
        TxType1d::Identity => inverse_identity(t, 1usize << log2),
    }
}

/// The 2D inverse transform (spec 7.13.3) for a square block of any
/// [`TxType`]. `dequant` is the dequantized coefficient grid in raster order;
/// the returned residual is in the same order. A 64-point transform only ever
/// carries coefficients in its first 32 rows and columns, and the spec
/// zeroes the rest before the row transform, which is what the `< 32` guard
/// does (`TxType`s other than `DctDct` never reach a 64-point transform in
/// this decoder -- `Luma64`'s CDF set carries no `tx_type` symbol).
pub fn inverse_transform_2d_typed(
    dequant: &[i32],
    side: usize,
    bit_depth: u8,
    tx_type: TxType,
) -> Vec<i32> {
    inverse_transform_2d_typed_wh(dequant, side, side, bit_depth, tx_type)
}

/// [`inverse_transform_2d_typed`] widened to `(w, h)` (lane-recttx): the row
/// pass transforms `w`-point vectors (`log2(w)`), the column pass
/// `h`-point vectors (`log2(h)`), and a rect-only pre-row-transform scale
/// is applied when the two axes are exactly one power-of-two apart (spec
/// 7.13.3; `av1_inv_txfm2d.c:272-276`, `abs(rect_type) == 1`). `dequant` is
/// still raster order (`dequant[i * w + j]`, row `i` of `h`, column `j` of
/// `w`) -- libaom's own buffer for the same math is column-major, an
/// unrelated fact about ITS storage, not the spec's arithmetic (class
/// `reference-layout-not-spec`).
pub fn inverse_transform_2d_typed_wh(
    dequant: &[i32],
    w: usize,
    h: usize,
    bit_depth: u8,
    tx_type: TxType,
) -> Vec<i32> {
    let log2w = w.trailing_zeros();
    let log2h = h.trailing_zeros();
    assert_eq!(1usize << log2w, w, "width must be a power of two");
    assert_eq!(1usize << log2h, h, "height must be a power of two");
    assert_eq!(dequant.len(), w * h, "one coefficient per position");
    let row_clamp = usize::from(bit_depth) + 8;
    let col_clamp = (usize::from(bit_depth) + 6).max(16);
    let (row_kind, col_kind) = tx_type.axes();
    let (ud_flip, lr_flip) = tx_type.flip();
    // `get_rect_tx_log_ratio` (`av1_inv_txfm2d.c:248`): the scale fires only
    // at ratio exactly 1, never 0 (square) or 2 (e.g. 4x16/16x4/8x32/32x8/
    // 16x64/64x16).
    let rect_scale = (log2w as i32 - log2h as i32).abs() == 1;
    // Loop-invariant: the shift is keyed on the transform size, and was being
    // looked up through a match once per output sample.
    let shift = row_shift_wh(w, h);
    // A FLIPADST axis reads the row-pass output right-to-left and/or writes
    // its own output bottom-to-top (`inv_txfm2d_add_c`, `av1_inv_txfm2d.c`
    // lines 291-314): `out` is a second buffer, not the row scratch reused in
    // place, because a flipped read/write pair aliases a column this same
    // loop has not visited yet (column `j`'s write target under `lr_flip`
    // is a column an unflipped loop would still need to read from later).
    // `lr_flip` mirrors over `w` (the row axis), `ud_flip` over `h`.
    let mut out = vec![0i32; w * h];
    let cols = w.min(32);
    ROW_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        if scratch.len() < w * h {
            scratch.resize(64 * 64, 0);
        }
        let residual = &mut scratch[..w * h];
        let mut t = [0i32; 64];
        // lane-perf10: rows the row pass left non-zero, and whether the first
        // of them came out constant along the row -- see the fast path below.
        let mut nz_rows = 0usize;
        let mut nz_row = 0usize;
        let mut const_row = false;
        for i in 0..h {
            let dst = &mut residual[i * w..(i + 1) * w];
            // Rows at or past 32 carry no coefficients (the spec zeroes them
            // before the row transform), and a zero row transforms to a zero
            // row: every butterfly and Hadamard of zeros is zero, and
            // `Round2(0, n)` is zero at every `n`, so this is the full
            // network's own answer, not an approximation of it.
            let src = if i < 32 { &dequant[i * w..i * w + cols] } else { &[][..] };
            if src.iter().all(|&c| c == 0) {
                dst.fill(0);
                continue;
            }
            if rect_scale {
                for (tj, &c) in t[..cols].iter_mut().zip(src) {
                    *tj = round2(c * 2896, 12);
                }
            } else {
                t[..cols].copy_from_slice(src);
            }
            t[cols..w].fill(0);
            inverse_1d(&mut t, log2w, row_clamp, row_kind);
            // The row's output is shifted, then clamped before the column
            // transform reads it back.
            for (d, &v) in dst.iter_mut().zip(t[..w].iter()) {
                *d = clamp_range(round2(v, shift), col_clamp);
            }
            nz_rows += 1;
            nz_row = i;
            const_row = nz_rows == 1 && dst.iter().all(|&v| v == dst[0]);
        }

        // A block whose row pass left exactly one non-zero row, constant along
        // that row, feeds every column of the column pass the same vector --
        // the DC-only case, which dominates inter residuals. `inverse_1d` is a
        // pure function of its input, so one column transform broadcast across
        // the block is byte-identical to `w` of them, and the per-column gather
        // disappears with it.
        if nz_rows <= 1 && const_row {
            t[..h].fill(0);
            t[nz_row] = residual[nz_row * w];
            inverse_1d(&mut t, log2h, col_clamp, col_kind);
            for i in 0..h {
                let dst_row = if ud_flip { h - 1 - i } else { i };
                out[dst_row * w..dst_row * w + w].fill(round2(t[i], 4));
            }
            return;
        }

        for j in 0..w {
            let src_col = if lr_flip { w - 1 - j } else { j };
            let mut any = 0i32;
            for i in 0..h {
                let v = residual[i * w + src_col];
                t[i] = v;
                any |= v;
            }
            // Same zero-in/zero-out argument as the row pass, and `out` is
            // already zero at every position this column would write.
            if any == 0 {
                continue;
            }
            inverse_1d(&mut t, log2h, col_clamp, col_kind);
            for i in 0..h {
                let dst_row = if ud_flip { h - 1 - i } else { i };
                out[dst_row * w + j] = round2(t[i], 4);
            }
        }
    });
    out
}

thread_local! {
    /// Row-pass scratch, reused per thread. The row pass writes every position
    /// it later reads, so the buffer needs no zeroing, and the two `Vec`s this
    /// function allocated per transform unit become one.
    static ROW_SCRATCH: std::cell::RefCell<Vec<i32>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// [`inverse_transform_2d_typed`] at `DCT_DCT`, this crate's original
/// (and still most common) transform.
pub fn inverse_transform_2d(dequant: &[i32], side: usize, bit_depth: u8) -> Vec<i32> {
    inverse_transform_2d_typed(dequant, side, bit_depth, TxType::DctDct)
}

/// Dequantize (spec 7.12.3) and inverse transform one square block of any
/// [`TxType`], returning its residual in raster order.
///
/// This is the encoder's model of what the decoder will add to its prediction,
/// so it follows the spec's dequantization exactly, truncation toward zero and
/// all.
#[allow(clippy::too_many_arguments)]
pub fn dequant_and_inverse_typed(
    levels: &[i32],
    side: usize,
    bit_depth: u8,
    q_idx: i32,
    dc_delta: i32,
    ac_delta: i32,
    tx_type: TxType,
) -> Vec<i32> {
    dequant_and_inverse_typed_wh(levels, side, side, bit_depth, q_idx, dc_delta, ac_delta, tx_type)
}

/// [`dequant_and_inverse_typed`] widened to `(w, h)` (lane-recttx). `dc_delta`/
/// `ac_delta` are the per-plane quantizer-index offsets (lane-sbpart r11,
/// spec 5.9.12/7.12.2, [`crate::quant::QuantDeltas`]) -- `0`/`0` for the
/// unaffected callers below.
#[allow(clippy::too_many_arguments)]
pub fn dequant_and_inverse_typed_wh(
    levels: &[i32],
    w: usize,
    h: usize,
    bit_depth: u8,
    q_idx: i32,
    dc_delta: i32,
    ac_delta: i32,
    tx_type: TxType,
) -> Vec<i32> {
    DQ_SCRATCH.with(|cell| {
        let mut dq = cell.borrow_mut();
        crate::quant::dequant_wh_into(levels, &mut dq, w, h, bit_depth, q_idx, dc_delta, ac_delta);
        inverse_transform_2d_typed_wh(&dq, w, h, bit_depth, tx_type)
    })
}

thread_local! {
    /// The dequantized grid, reused per thread: it lives only until the
    /// inverse transform has read it, so it was a `Vec` allocated and dropped
    /// once per transform unit.
    static DQ_SCRATCH: std::cell::RefCell<Vec<i32>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// [`dequant_and_inverse_typed`] at `DCT_DCT`, no per-plane quantizer delta.
pub fn dequant_and_inverse(levels: &[i32], side: usize, bit_depth: u8, q_idx: i32) -> Vec<i32> {
    dequant_and_inverse_typed(levels, side, bit_depth, q_idx, 0, 0, TxType::DctDct)
}

/// The orthonormal DCT-II basis row for output `u` of an `n`-point transform.
///
/// The decoder's network is this basis scaled by a constant the encoder has to
/// undo, and nothing else: measuring the network's response to a unit
/// coefficient at every size gives the same `1 / (8 * sqrt(2))` per unit of
/// `dqDenom`, which is what [`forward_transform_2d`] divides out.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
fn build_dct_basis(n: usize) -> Vec<f64> {
    let mut basis = vec![0.0f64; n * n];
    let scale = (2.0 / n as f64).sqrt();
    for u in 0..n {
        let alpha = if u == 0 { 1.0 / 2.0f64.sqrt() } else { 1.0 };
        for i in 0..n {
            let angle = std::f64::consts::PI * (2.0 * i as f64 + 1.0) * u as f64 / (2.0 * n as f64);
            basis[u * n + i] = alpha * scale * angle.cos();
        }
    }
    basis
}

/// [`build_dct_basis`], computed once per transform size and reused: the
/// search calls [`forward_transform_2d`] for every mode of every block, and
/// the basis does not depend on the residual, only on `n` -- recomputing
/// `n * n` cosines per call was measured as this search's single largest
/// per-trial cost (`stage_timing_breakdown`, ec-av1 perf lane). `n` is always
/// one of the transform sizes this crate codes (4 to 64, a power of two), so
/// a small fixed table indexed by `log2(n)` covers every caller without a
/// hash lookup.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
fn dct_basis(n: usize) -> &'static [f64] {
    use std::sync::OnceLock;
    // log2(4)=2 .. log2(64)=6, so index by trailing_zeros() - 2.
    static CACHE: OnceLock<[OnceLock<Vec<f64>>; 5]> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::array::from_fn(|_| OnceLock::new()));
    let idx = (n.trailing_zeros() as usize).saturating_sub(2);
    cache[idx].get_or_init(|| build_dct_basis(n))
}

/// How much the decoder's inverse transform shrinks an orthonormal DCT.
///
/// Measured, not asserted: feeding a single dequantized coefficient through
/// `inverse_transform_2d` reproduces the orthonormal basis function scaled by
/// `dq_denom(side) / 8` at every size (see `the_inverse_network_is_an_orthonormal_dct_over_eight`).
/// The dequantizer has already divided by `dq_denom`, so the two cancel and
/// what an encoder owes a level is size-independent:
/// `level = 8 * orthonormal(residual) / q`.
///
/// The spec fixes the decoder; the encoder is free in how it reaches the
/// coefficients it sends, and this constant is the one thing the two ends have
/// to agree on for a level to mean what the encoder thinks it means.
const INVERSE_GAIN_RECIPROCAL: f64 = 8.0;

/// A complex sample, `(re, im)`, for the radix-2 FFT beneath [`dct1d`].
type Complex = (f64, f64);

fn cmul(a: Complex, b: Complex) -> Complex {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

/// An in-place iterative radix-2 Cooley-Tukey FFT (forward, i.e. the `exp(-i
/// theta)` convention), `a.len()` a power of two. Only [`dct1d`] calls this,
/// on sizes 4 to 64, so no bounds beyond "power of two" are enforced.
fn fft(a: &mut [Complex]) {
    let n = a.len();
    if n <= 1 {
        return;
    }
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        let wlen = (ang.cos(), ang.sin());
        let mut i = 0;
        while i < n {
            let mut w = (1.0, 0.0);
            for k in 0..len / 2 {
                let u = a[i + k];
                let v = cmul(a[i + k + len / 2], w);
                a[i + k] = (u.0 + v.0, u.1 + v.1);
                a[i + k + len / 2] = (u.0 - v.0, u.1 - v.1);
                w = cmul(w, wlen);
            }
            i += len;
        }
        len <<= 1;
    }
}

/// The orthonormal DCT-II of one line of `n` samples, in `O(n log n)` via
/// Makhoul's FFT reduction rather than the `n^2` direct dot product against
/// [`build_dct_basis`]'s rows.
///
/// The reduction: reorder `x` into `v[i] = x[2i]`, `v[n-1-i] = x[2i+1]` for
/// `i < n/2`; an `n`-point FFT of `v` then gives the unnormalized DCT-II as
/// `X_k = Re(V_k * exp(-i*pi*k/(2n)))` (measured against the direct
/// definition, not the textbook `2 * Re(...)` form, which double-counts here
/// because `v`'s construction already folds each input pair once). What is
/// returned here is that,
/// scaled to match [`build_dct_basis`]'s convention exactly (`alpha_0 =
/// 1/sqrt(2)`, `alpha_k = 1` otherwise, both times `sqrt(2/n)`) so the result
/// is bit-for-bit the same *contract* as the old matmul, only reordered in
/// its summation (equal up to floating-point rounding, checked in
/// `fast_forward_matches_the_naive_matmul`).
/// The post-FFT twist for [`dct1d`], `(cos(theta_k), sin(theta_k), alpha_k *
/// sqrt(2/n))` per output `k` -- cached per size for the same reason
/// [`dct_basis`] is: computed from a `k`-dependent angle, so a call-time
/// `cos`/`sin` per output (`n` of them, every call) was measured as this
/// factorization's actual bottleneck, worse than the matmul it was meant to
/// beat.
fn dct_twist(n: usize) -> &'static [(f64, f64, f64)] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<[OnceLock<Vec<(f64, f64, f64)>>; 5]> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::array::from_fn(|_| OnceLock::new()));
    let idx = (n.trailing_zeros() as usize).saturating_sub(2);
    cache[idx].get_or_init(|| {
        (0..n)
            .map(|k| {
                let theta = std::f64::consts::PI * k as f64 / (2.0 * n as f64);
                let alpha = if k == 0 { 1.0 / 2.0f64.sqrt() } else { 1.0 };
                (theta.cos(), theta.sin(), alpha * (2.0 / n as f64).sqrt())
            })
            .collect()
    })
}

///
/// `x` and `out` are both exactly `n` long (`n` at most 64, the largest
/// transform side this crate codes); the FFT itself runs on a fixed 64-slot
/// stack array rather than a heap `Vec` -- this call happens twice per row
/// and twice per column of every RD trial's transform, and the allocations
/// that a `Vec` per call cost here were measured as a first source of a first
/// cut of this factorization being *slower* than the matmul it replaced (the
/// call-time trig in [`dct_twist`]'s table was the other, larger one).
fn dct1d(x: &[f64], out: &mut [f64]) {
    let n = x.len();
    let half = n / 2;
    let mut v = [(0.0f64, 0.0f64); 64];
    for i in 0..half {
        v[i] = (x[2 * i], 0.0);
        v[n - 1 - i] = (x[2 * i + 1], 0.0);
    }
    fft(&mut v[..n]);
    let twist = dct_twist(n);
    for (k, &(c, s, scale)) in twist.iter().enumerate() {
        out[k] = scale * (v[k].0 * c + v[k].1 * s);
    }
}

/// The forward transform (encoder side, spec-free): the transpose of what the
/// decoder's inverse does, so that `inverse_transform_2d` of the result
/// reproduces `residual`.
///
/// AV1 specifies only the inverse transform. The encoder may reach its
/// coefficients however it likes, and this reaches them via [`dct1d`], an
/// `O(n log n)` orthonormal DCT-II factorization equal to the direct
/// `O(n^2)` matmul up to floating-point rounding (see
/// `fast_forward_matches_the_naive_matmul`), scaled to the decoder's
/// fixed-point gain. It is deterministic, and its accuracy is measured
/// against the decoder's own inverse rather than asserted.
pub fn forward_transform_2d(residual: &[i32], side: usize) -> Vec<f64> {
    assert_eq!(
        residual.len(),
        side * side,
        "one residual sample per position"
    );
    let mut rows = vec![0.0f64; side * side];
    let mut buf = [0.0f64; 64];
    let mut line = [0.0f64; 64];
    for i in 0..side {
        for (j, &r) in residual[i * side..][..side].iter().enumerate() {
            buf[j] = f64::from(r);
        }
        dct1d(&buf[..side], &mut line[..side]);
        rows[i * side..][..side].copy_from_slice(&line[..side]);
    }
    let mut out = vec![0.0f64; side * side];
    for v in 0..side {
        for (i, c) in buf[..side].iter_mut().enumerate() {
            *c = rows[i * side + v];
        }
        dct1d(&buf[..side], &mut line[..side]);
        for (u, &t) in line[..side].iter().enumerate() {
            out[u * side + v] = t;
        }
    }
    let scale = INVERSE_GAIN_RECIPROCAL;
    for v in &mut out {
        *v *= scale;
    }
    out
}

/// The old direct `O(n^2)` matmul against [`build_dct_basis`], kept only as
/// the differential oracle for [`forward_transform_2d`]'s fast replacement
/// (`fast_forward_matches_the_naive_matmul`).
#[cfg(test)]
fn forward_transform_2d_naive(residual: &[i32], side: usize) -> Vec<f64> {
    let basis = dct_basis(side);
    let mut rows = vec![0.0f64; side * side];
    for i in 0..side {
        let residual_row = &residual[i * side..][..side];
        for u in 0..side {
            let sum: f64 = residual_row
                .iter()
                .zip(&basis[u * side..][..side])
                .map(|(&r, &b)| f64::from(r) * b)
                .sum();
            rows[i * side + u] = sum;
        }
    }
    let mut rows_t = vec![0.0f64; side * side];
    for i in 0..side {
        for v in 0..side {
            rows_t[v * side + i] = rows[i * side + v];
        }
    }
    let mut out = vec![0.0f64; side * side];
    for u in 0..side {
        for v in 0..side {
            let sum: f64 = rows_t[v * side..][..side]
                .iter()
                .zip(&basis[u * side..][..side])
                .map(|(&r, &b)| r * b)
                .sum();
            out[u * side + v] = sum;
        }
    }
    let scale = INVERSE_GAIN_RECIPROCAL;
    for v in &mut out {
        *v *= scale;
    }
    out
}

/// Quantize forward-transform coefficients into the levels the tile syntax
/// carries.
///
/// `deadzone` is the fraction of a quantizer step a coefficient has to reach
/// before it is coded at all, as a rounding offset: 0.5 rounds to nearest,
/// smaller values pull coefficients toward zero, which is what buys the rate
/// back on noisy content. A 64-point transform only carries its top-left
/// 32x32, so everything outside that is dropped here rather than silently by
/// the writer.
pub fn quantize(coeffs: &[f64], side: usize, bit_depth: u8, q_idx: i32, deadzone: f64) -> Vec<i32> {
    assert_eq!(coeffs.len(), side * side, "one coefficient per position");
    let dc = f64::from(crate::quant::dc_q(bit_depth, q_idx));
    let ac = f64::from(crate::quant::ac_q(bit_depth, q_idx));
    let mut levels = vec![0i32; side * side];
    for (i, &c) in coeffs.iter().enumerate() {
        let (row, col) = (i / side, i % side);
        if row >= 32 || col >= 32 {
            continue;
        }
        let q = if i == 0 { dc } else { ac };
        let scaled = c / q;
        let magnitude = scaled.abs() + deadzone;
        let level = if magnitude < 1.0 {
            0
        } else {
            magnitude.floor().min(f64::from(i32::MAX)) as i32
        };
        levels[i] = if scaled < 0.0 { -level } else { level };
    }
    levels
}

/// Transform and quantize one block's residual, the encoder's half of the
/// round trip [`dequant_and_inverse`] completes.
pub fn forward_and_quantize(
    residual: &[i32],
    side: usize,
    bit_depth: u8,
    q_idx: i32,
    deadzone: f64,
) -> Vec<i32> {
    let coeffs = forward_transform_2d(residual, side);
    quantize(&coeffs, side, bit_depth, q_idx, deadzone)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row and column passes skip an all-zero vector instead of running
    /// the butterfly network over it. That is only bit-exact because a zero
    /// vector transforms to a zero vector at every size, type and bit depth --
    /// which is what this pins, per 1D transform and end to end.
    #[test]
    fn a_zero_vector_transforms_to_zero() {
        for log2 in 2..=6u32 {
            for kind in [TxType1d::Dct, TxType1d::Adst, TxType1d::Identity] {
                if kind == TxType1d::Adst && log2 > 4 {
                    continue;
                }
                if kind == TxType1d::Identity && log2 > 5 {
                    continue;
                }
                for bit_depth in [8u8, 10, 12] {
                    let r = (usize::from(bit_depth) + 6).max(16);
                    let mut t = [0i32; 64];
                    inverse_1d(&mut t, log2, r, kind);
                    assert!(
                        t.iter().all(|&v| v == 0),
                        "1D {kind:?} log2={log2} depth={bit_depth} left a non-zero"
                    );
                }
            }
        }
        for (w, h) in [(4, 4), (8, 8), (16, 16), (32, 32), (64, 64), (8, 16), (16, 8), (4, 16), (32, 8)] {
            for tx in [TxType::DctDct, TxType::AdstAdst, TxType::Idtx, TxType::AdstDct, TxType::DctAdst] {
                if w.max(h) > 16 && tx != TxType::DctDct && tx != TxType::Idtx {
                    continue;
                }
                if w.max(h) > 32 && tx != TxType::DctDct {
                    continue;
                }
                let out = inverse_transform_2d_typed_wh(&vec![0i32; w * h], w, h, 10, tx);
                assert!(out.iter().all(|&v| v == 0), "2D {w}x{h} {tx:?} left a non-zero");
            }
        }
    }

    /// The orthonormal DCT-II basis the forward transform is written against,
    /// computed independently of `dct_basis` so a test is not checking a
    /// function against itself.
    fn reference_basis(n: usize) -> Vec<f64> {
        let mut c = vec![0.0; n * n];
        for u in 0..n {
            let alpha = if u == 0 { (0.5f64).sqrt() } else { 1.0 };
            for x in 0..n {
                let angle =
                    (2.0 * x as f64 + 1.0) * u as f64 * std::f64::consts::PI / (2.0 * n as f64);
                c[u * n + x] = alpha * (2.0 / n as f64).sqrt() * angle.cos();
            }
        }
        c
    }

    /// A deterministic pseudo-random residual in `-100..=100`.
    fn noise(len: usize, seed: u64) -> Vec<i32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((state >> 33) % 201) as i32 - 100
            })
            .collect()
    }

    #[test]
    #[ignore]
    fn scratch_probe_32x32_dequant() {
        let mut levels = vec![0i32; 32 * 32];
        levels[0] = -2;
        levels[1] = -2;
        levels[2] = -2;
        let dq = crate::quant::dequant(&levels, 32, 8, 60);
        eprintln!("dq[0..4]={:?}", &dq[0..4]);
        let residual = dequant_and_inverse(&levels, 32, 8, 60);
        eprintln!("row0: {:?}", &residual[0..32]);
        eprintln!("row1: {:?}", &residual[32..64]);

        // superposition probe: does inverse_transform_2d at side=32 sum
        // three simultaneous low-frequency coefficients the same as it
        // reconstructs each alone?
        let side = 32usize;
        let mut single_sum = vec![0i32; side * side];
        for &(pos, val) in &[(0usize, -57i32), (1, -67), (2, -67)] {
            let mut c = vec![0i32; side * side];
            c[pos] = val;
            let got = inverse_transform_2d(&c, side, 8);
            for i in 0..side * side {
                single_sum[i] += got[i];
            }
        }
        let mut combined = vec![0i32; side * side];
        combined[0] = -57;
        combined[1] = -67;
        combined[2] = -67;
        let got_combined = inverse_transform_2d(&combined, side, 8);
        eprintln!("single_sum row0: {:?}", &single_sum[0..8]);
        eprintln!("combined  row0: {:?}", &got_combined[0..8]);
    }

    fn rmse(a: &[i32], b: &[i32]) -> f64 {
        let sum: f64 = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| f64::from(x - y) * f64::from(x - y))
            .sum();
        (sum / a.len() as f64).sqrt()
    }

    /// [`forward_transform_2d`]'s fast FFT-based factorization against
    /// [`forward_transform_2d_naive`]'s direct matmul: same summation
    /// reordered, so equal up to floating-point rounding, not bit-exact.
    #[test]
    fn fast_forward_matches_the_naive_matmul() {
        for &side in &[4usize, 8, 16, 32, 64] {
            for seed in [1u64, 2, 3] {
                let residual = noise(side * side, seed * 1000 + side as u64);
                let fast = forward_transform_2d(&residual, side);
                let naive = forward_transform_2d_naive(&residual, side);
                for (i, (&f, &n)) in fast.iter().zip(&naive).enumerate() {
                    assert!(
                        (f - n).abs() < 1e-6 * n.abs().max(1.0),
                        "{side}x{side} seed {seed} coefficient {i}: fast {f} vs naive {n}"
                    );
                }
            }
        }
    }

    /// The one thing the encoder and the decoder have to agree on.
    ///
    /// The spec's inverse network is not documented as a scaled orthonormal
    /// DCT, so this measures it: each dequantized coefficient, on its own,
    /// comes back out as its orthonormal basis function scaled by
    /// `dq_denom(side) / 8` — the same factor for every coefficient of a
    /// size, which is what makes a single constant enough for the encoder.
    /// The fit is checked as well as the scale, so an inverse that scaled
    /// right but mixed positions apart could not pass.
    #[test]
    fn the_inverse_network_is_an_orthonormal_dct_over_eight() {
        for &side in &[4usize, 8, 16, 32, 64] {
            let basis = reference_basis(side);
            for &(u, v) in &[(0usize, 0usize), (0, 1), (1, 0), (2, 3), (5, 7)] {
                if u >= side || v >= side {
                    continue;
                }
                let mut coeffs = vec![0i32; side * side];
                coeffs[u * side + v] = 4096;
                let got = inverse_transform_2d(&coeffs, side, 8);
                let want: Vec<f64> = (0..side * side)
                    .map(|i| 4096.0 * basis[u * side + i / side] * basis[v * side + i % side])
                    .collect();
                // Least-squares fit of one scale over the whole block.
                let num: f64 = want.iter().zip(&got).map(|(w, &g)| w * f64::from(g)).sum();
                let den: f64 = want.iter().map(|w| w * w).sum();
                let k = num / den;
                let expected = crate::quant::dq_denom(side) as f64 / 8.0;
                assert!(
                    (k - expected).abs() < 0.01 * expected,
                    "{side}x{side} ({u},{v}): gain {k}, expected {expected}"
                );
                // And the block really is that basis function, not merely a
                // block of the same energy: no sample off by more than one.
                for (i, (w, &g)) in want.iter().zip(&got).enumerate() {
                    let scaled = k * w;
                    assert!(
                        (scaled - f64::from(g)).abs() <= 1.0,
                        "{side}x{side} ({u},{v}) sample {i}: {g} vs {scaled}"
                    );
                }
            }
        }
    }

    /// A fine quantizer costs almost nothing: forward, quantize, dequantize
    /// and invert returns the residual to within a sample or two at every
    /// transform size the spec's DCT covers.
    ///
    /// 64x64 is excluded here because it cannot carry a white-noise residual
    /// at all — see
    /// [`a_64x64_transform_keeps_what_fits_in_its_coded_quarter`].
    #[test]
    fn a_fine_quantizer_roundtrips_a_residual_almost_exactly() {
        for &side in &[4usize, 8, 16, 32] {
            let residual = noise(side * side, 12_345 + side as u64);
            let levels = forward_and_quantize(&residual, side, 8, 10, 0.5);
            let back = dequant_and_inverse(&levels, side, 8, 10);
            let error = rmse(&back, &residual);
            assert!(error < 1.0, "{side}x{side}: rmse {error}");
        }
    }

    /// Error grows with the quantizer and with nothing else. A calibration
    /// that is off by a constant factor shows up here as a floor that a finer
    /// quantizer cannot get under — which is exactly what a wrong gain
    /// constant produced while this was being written.
    #[test]
    fn the_roundtrip_error_is_the_quantizer_and_only_the_quantizer() {
        for &side in &[4usize, 8, 16, 32] {
            let residual = noise(side * side, 999 + side as u64);
            let mut previous = 0.0;
            for &q_idx in &[10i32, 60, 100, 180] {
                let levels = forward_and_quantize(&residual, side, 8, q_idx, 0.5);
                let back = dequant_and_inverse(&levels, side, 8, q_idx);
                let error = rmse(&back, &residual);
                assert!(
                    error > previous,
                    "{side}x{side} q {q_idx}: {error} <= {previous}"
                );
                // A quantizer step is q/8 in residual units, and rounding to
                // nearest costs no more than half a step per coefficient.
                let step = f64::from(crate::quant::ac_q(8, q_idx)) / 8.0;
                assert!(
                    error < step,
                    "{side}x{side} q {q_idx}: rmse {error}, step {step}"
                );
                previous = error;
            }
        }
    }

    /// A 64x64 transform codes only its top-left 32x32 coefficients, so it
    /// keeps everything below half the Nyquist rate in each direction and
    /// drops the rest. A band-limited residual survives it; white noise loses
    /// the three quarters of its energy that live outside the coded quarter,
    /// and that loss is the transform's, not the quantizer's — it does not
    /// move when the quantizer does.
    #[test]
    fn a_64x64_transform_keeps_what_fits_in_its_coded_quarter() {
        let side = 64;
        // Band-limited: built from basis functions inside the coded quarter.
        let basis = reference_basis(side);
        let mut smooth = vec![0.0f64; side * side];
        for &(u, v, amplitude) in &[(0usize, 0usize, 900.0f64), (1, 2, 400.0), (9, 30, 250.0)] {
            for i in 0..side * side {
                smooth[i] += amplitude * basis[u * side + i / side] * basis[v * side + i % side];
            }
        }
        let smooth: Vec<i32> = smooth.iter().map(|&s| s.round() as i32).collect();
        let levels = forward_and_quantize(&smooth, side, 8, 10, 0.5);
        let back = dequant_and_inverse(&levels, side, 8, 10);
        let kept = rmse(&back, &smooth);
        assert!(kept < 1.5, "band-limited 64x64: rmse {kept}");

        let residual = noise(side * side, 4_242);
        let mut errors = Vec::new();
        for &q_idx in &[10i32, 180] {
            let levels = forward_and_quantize(&residual, side, 8, q_idx, 0.5);
            let back = dequant_and_inverse(&levels, side, 8, q_idx);
            errors.push(rmse(&back, &residual));
        }
        // Three quarters of a white-noise block's energy is outside the coded
        // quarter, so the error is sqrt(3/4) of the residual's own magnitude
        // whatever the quantizer does.
        let magnitude = rmse(&residual, &vec![0; side * side]);
        for error in &errors {
            let ratio = error / magnitude;
            assert!(
                (ratio - 0.75f64.sqrt()).abs() < 0.05,
                "64x64 white noise: ratio {ratio}"
            );
        }
        assert!(
            (errors[1] - errors[0]).abs() < 0.05 * errors[0],
            "64x64 white noise moved with the quantizer: {errors:?}"
        );
    }

    /// The deadzone pulls coefficients toward zero: a wider one codes fewer
    /// of them and costs more error, and rounding to nearest is the tightest
    /// of them.
    #[test]
    fn a_wider_deadzone_codes_fewer_coefficients() {
        let side = 16;
        let residual = noise(side * side, 77);
        let mut previous_nonzero = usize::MAX;
        let mut previous_error = 0.0;
        for &deadzone in &[0.5f64, 0.35, 0.2] {
            let levels = forward_and_quantize(&residual, side, 8, 100, deadzone);
            let back = dequant_and_inverse(&levels, side, 8, 100);
            let nonzero = levels.iter().filter(|&&l| l != 0).count();
            let error = rmse(&back, &residual);
            assert!(
                nonzero < previous_nonzero,
                "deadzone {deadzone}: {nonzero} coefficients"
            );
            assert!(error > previous_error, "deadzone {deadzone}: rmse {error}");
            previous_nonzero = nonzero;
            previous_error = error;
        }
    }

    /// Negating a residual negates its levels: the forward transform carries
    /// no offset of its own.
    #[test]
    fn negating_the_residual_negates_every_level() {
        for &side in &[4usize, 32] {
            let residual = noise(side * side, 5_150 + side as u64);
            let negated: Vec<i32> = residual.iter().map(|&r| -r).collect();
            let levels = forward_and_quantize(&residual, side, 8, 100, 0.5);
            let other = forward_and_quantize(&negated, side, 8, 100, 0.5);
            for (i, (a, b)) in levels.iter().zip(&other).enumerate() {
                assert_eq!(*a, -*b, "{side}x{side} coefficient {i}");
            }
        }
    }

    /// A flat residual is a DC level and nothing else, at the value the
    /// dequantizer's own arithmetic asks for.
    #[test]
    fn a_flat_residual_is_a_dc_level_alone() {
        for &side in &[4usize, 8, 16, 32, 64] {
            let residual = vec![40i32; side * side];
            let levels = forward_and_quantize(&residual, side, 8, 100, 0.5);
            for (i, &level) in levels.iter().enumerate() {
                if i == 0 {
                    // level = 8 * (40 * side) / dc_q, the orthonormal DC of a
                    // flat block being its value times the side.
                    let want = (8.0 * 40.0 * side as f64 / f64::from(crate::quant::dc_q(8, 100)))
                        .round() as i32;
                    assert_eq!(level, want, "{side}x{side} DC");
                } else {
                    assert_eq!(level, 0, "{side}x{side} coefficient {i}");
                }
            }
        }
    }

    // --- ADST/identity family: reference vectors from libaom's own
    // `av1_iadst4`/`8`/`16` and `av1_iidentity*_c`, computed by a small C
    // probe (`gcc` against `/tmp/libaom-src/build/decoder-debug/libaom.a`,
    // calling those exported symbols directly with the inputs pinned below --
    // no hand-derived arithmetic) so these are decisive, not memory-derived.

    #[test]
    fn iadst4_matches_libaoms_own_output() {
        let mut t = [100, -50, 25, 7];
        inverse_adst4(&mut t);
        assert_eq!(t, [19, 5, 67, 147]);
    }

    #[test]
    fn iadst4_an_all_zero_input_short_circuits_to_zero() {
        let mut t = [0, 0, 0, 0];
        inverse_adst4(&mut t);
        assert_eq!(t, [0, 0, 0, 0]);
    }

    #[test]
    fn iadst8_matches_libaoms_own_output() {
        let mut t = [100, -50, 25, 7, -3, 60, -12, 8];
        inverse_adst8(&mut t, 16);
        assert_eq!(t, [59, 15, -25, 52, 25, 27, 207, 130]);
    }

    #[test]
    fn iadst16_matches_libaoms_own_output() {
        let mut t = [
            100, -50, 25, 7, -3, 60, -12, 8, 15, -40, 33, -5, 22, 1, -70, 44,
        ];
        inverse_adst16(&mut t, 16);
        assert_eq!(
            t,
            [
                32, 73, 28, 6, -45, -28, 111, -52, 135, -54, 85, -67, 305, 190, 102, 145
            ]
        );
    }

    #[test]
    fn iidentity_matches_libaoms_own_output_at_every_size() {
        let mut t4 = [100, -50, 25, 7];
        inverse_identity(&mut t4, 4);
        assert_eq!(t4, [141, -71, 35, 10]);

        let mut t8 = [100, -50, 25, 7, -3, 60, -12, 8];
        inverse_identity(&mut t8, 8);
        assert_eq!(t8, [200, -100, 50, 14, -6, 120, -24, 16]);

        let mut t16 = [
            100, -50, 25, 7, -3, 60, -12, 8, 15, -40, 33, -5, 22, 1, -70, 44,
        ];
        inverse_identity(&mut t16, 16);
        assert_eq!(
            t16,
            [
                283, -141, 71, 20, -8, 170, -34, 23, 42, -113, 93, -14, 62, 3, -198, 124
            ]
        );

        let mut t32 = [0i32; 32];
        for (i, v) in t32.iter_mut().enumerate() {
            *v = i as i32 * 7 - 100;
        }
        inverse_identity(&mut t32, 32);
        assert_eq!(
            t32,
            [
                -400, -372, -344, -316, -288, -260, -232, -204, -176, -148, -120, -92, -64, -36,
                -8, 20, 48, 76, 104, 132, 160, 188, 216, 244, 272, 300, 328, 356, 384, 412, 440,
                468
            ]
        );
    }

    /// The axis-convention gate: an impulse in row 0 (`ADST_DCT`'s row
    /// transform is DCT, so an ADST call here stands in for `DCT_ADST`'s row
    /// pass) fed through `inverse_adst8` at "row position 0" must differ from
    /// the same impulse at "column position 1" -- i.e. ADST is not a
    /// symmetric function of its input's position, which is what lets
    /// `ADST_DCT` and `DCT_ADST` decode to different residuals for the same
    /// coefficient grid. Values pinned from the same libaom probe.
    #[test]
    fn iadst8_is_not_symmetric_in_its_input_position() {
        let mut row = [40, 0, 0, 0, 0, 0, 0, 0];
        let mut col = [0, 40, 0, 0, 0, 0, 0, 0];
        inverse_adst8(&mut row, 16);
        inverse_adst8(&mut col, 16);
        assert_eq!(row, [4, 12, 18, 25, 31, 35, 38, 40]);
        assert_eq!(col, [12, 31, 40, 35, 18, -4, -26, -38]);
        assert_ne!(
            row, col,
            "an asymmetric input must not decode symmetrically"
        );
    }

    /// The decisive end-to-end asymmetry gate `inverse_transform_2d_typed`
    /// itself: `ADST_DCT` and `DCT_ADST` must decode the same asymmetric
    /// coefficient grid to *different* residuals, which is only true if the
    /// row/column axis assignment (`TxType::axes`) is not accidentally
    /// symmetric (e.g. both pointing at the same 1D transform, or swapped
    /// with each other in a way that cancels out on this input).
    #[test]
    fn adst_dct_and_dct_adst_disagree_on_an_asymmetric_grid() {
        let side = 8;
        let mut dequant = vec![0i32; side * side];
        // An asymmetric coefficient: nonzero only at (row=0, col=1).
        dequant[1] = 64;
        let adst_dct = inverse_transform_2d_typed(&dequant, side, 8, TxType::AdstDct);
        let dct_adst = inverse_transform_2d_typed(&dequant, side, 8, TxType::DctAdst);
        assert_ne!(
            adst_dct, dct_adst,
            "ADST_DCT and DCT_ADST must not be interchangeable on an asymmetric grid"
        );
        // Also distinct from plain DCT_DCT and ADST_ADST at the same input.
        let dct_dct = inverse_transform_2d_typed(&dequant, side, 8, TxType::DctDct);
        let adst_adst = inverse_transform_2d_typed(&dequant, side, 8, TxType::AdstAdst);
        assert_ne!(adst_dct, dct_dct);
        assert_ne!(dct_adst, dct_dct);
        assert_ne!(adst_adst, dct_dct);
    }

    /// The lane-recttx charter's asymmetric coefficient block (`coeff` in
    /// `lanes/recttx_dump.c`, which this pin was transcribed alongside): a
    /// DC of 640 plus a distinct row-weighted/col-weighted 4x4 corner, zero
    /// elsewhere, so a transposed axis produces a genuinely different
    /// checksum rather than a coincidentally equal one.
    fn recttx_coeff(w: usize, h: usize) -> Vec<i32> {
        let mut d = vec![0i32; w * h];
        for i in 0..h {
            for j in 0..w {
                d[i * w + j] = if i == 0 && j == 0 {
                    640
                } else if i < 4 && j < 4 {
                    (i as i32 + 1) * 24 - (j as i32 + 1) * 17
                } else {
                    0
                };
            }
        }
        d
    }

    fn weighted_checksum(residual: &[i32]) -> i64 {
        residual
            .iter()
            .enumerate()
            .map(|(idx, &v)| i64::from(v) * (idx as i64 + 1))
            .sum()
    }

    /// Every one of the 14 rectangular sizes, DCT_DCT, `bit_depth = 8`,
    /// pinned against `lanes/recttx_dump.c` / `.expected.txt` -- a real
    /// libaom 1D-kernel-linked C harness, not a from-scratch reimplement.
    /// The transposed HxW twin runs in the SAME test (class
    /// `scan-weights-cross-axis`): a swapped-axis bug in
    /// `inverse_transform_2d_typed_wh` would either fail its own pin or
    /// accidentally match its twin's pin, and the two expected values here
    /// are deliberately different, so neither escape is possible.
    #[test]
    fn rect_sizes_pinned_against_libaom() {
        let cases: &[(usize, usize, i64)] = &[
            (4, 8, 7290),
            (8, 4, 7223),
            (8, 16, 56211),
            (16, 8, 56161),
            (16, 32, 893635),
            (32, 16, 893210),
            (32, 64, 14260693),
            (64, 32, 14265125),
            (4, 16, 20091),
            (16, 4, 20156),
            (8, 32, 159207),
            (32, 8, 159430),
            (16, 64, 2537255),
            (64, 16, 2548119),
        ];
        for &(w, h, expected) in cases {
            let dequant = recttx_coeff(w, h);
            let residual = inverse_transform_2d_typed_wh(&dequant, w, h, 8, TxType::DctDct);
            assert_eq!(
                weighted_checksum(&residual),
                expected,
                "{w}x{h} checksum mismatch"
            );
        }
    }

    /// The square path must not move: `row_shift_wh(side, side)` reduces to
    /// the pre-lane `row_shift(log2)` table, and the `side`-taking wrapper
    /// must still equal the `(w, h)` core called with `w == h == side`.
    #[test]
    fn square_wrapper_matches_wh_core() {
        for side in [4, 8, 16, 32, 64] {
            let dequant = recttx_coeff(side, side);
            let via_wrapper = inverse_transform_2d_typed(&dequant, side, 8, TxType::DctDct);
            let via_wh = inverse_transform_2d_typed_wh(&dequant, side, side, 8, TxType::DctDct);
            assert_eq!(via_wrapper, via_wh, "side {side}");
        }
    }
}
