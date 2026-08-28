//! The CDFs a tile adapts as it is written.
//!
//! AV1's symbol decoder updates every non-literal CDF it reads (spec 8.3.2),
//! so an encoder that wants `disable_cdf_update` off has to keep the same
//! state and update it in the same order. What lives here is that state: one
//! owned copy of each table the tile writer touches, seeded from the defaults
//! in [`crate::cdf`].
//!
//! The tables are owned once, not once per use: two coefficient table sets
//! that share a table -- a 64x64 luma transform reads the 32x32 base-range
//! rows, and both luma sets read the same DC sign table -- share the *adapted*
//! table too, which is what the decoder does.

use crate::cdf;

/// Which of the four coefficient table sets a transform block is coded with.
#[derive(Clone, Copy)]
pub(crate) enum TxbSet {
    /// The 32x32 luma transform of an intra 32x32 block. `get_tx_set` is
    /// DCT-only at this size for an intra block, so no `tx_type` symbol is
    /// coded; an `is_inter` 32x32 block reads a different set and uses
    /// [`Luma32Inter`](TxbSet::Luma32Inter) instead.
    Luma32,
    /// The 32x32 luma transform of an `is_inter` 32x32 block: the same
    /// coefficient tables [`Luma32`](TxbSet::Luma32) reads, but `get_tx_set`
    /// (spec 5.11.48) returns `TX_SET_INTER_3` rather than `TX_SET_DCTONLY`
    /// at this size for an inter block, so this variant carries the
    /// `tx_type` symbol `Luma32` does not.
    Luma32Inter,
    /// The 64x64 luma transform of a whole superblock, scanned as a 32x32.
    Luma64,
    /// The 16x16 luma transform of a 16x16 block.
    Luma16,
    /// The 16x16 luma transform of an `is_inter` 16x16 block, coded under
    /// `reduced_tx_set` (see [`crate::cdf::INTER_TX_TYPE_SET3_16`]'s doc
    /// comment): the same coefficient tables [`Luma16`](TxbSet::Luma16)
    /// reads, but its own two-symbol `tx_type` table rather than `Luma16`'s
    /// five-symbol, mode-indexed one.
    Luma16Inter,
    /// [`Luma16Inter`](TxbSet::Luma16Inter)'s `reduced_tx_set: false`
    /// counterpart (lane-cdffwd2, root-caused off `cdfflake-stream-seed45`):
    /// the same coefficient tables, but the twelve-symbol
    /// `EXT_TX_SET_DTT9_IDTX_1DDCT` `tx_type` table
    /// ([`crate::cdf::INTER_TX_TYPE_SET2_16`]) rather than `Luma16Inter`'s
    /// two-symbol reduced one.
    Luma16InterSet1,
    /// The 8x8 luma transform of an intra 8x8 leaf under a straddling 16x16
    /// block (lane-av1-rect). An `is_inter` leaf reads
    /// [`Luma8Inter`](TxbSet::Luma8Inter) instead.
    Luma8,
    /// [`Luma8`](TxbSet::Luma8)'s `reduced_tx_set: false` counterpart (round
    /// 8): the same coefficient tables, but the seven-symbol `TX_SET_INTRA_1`
    /// `tx_type` table rather than `Luma8`'s five-symbol reduced one.
    Luma8Set1,
    /// The 8x8 luma transform of an `is_inter` 8x8 leaf under a straddling
    /// 16x16 block (lane-av1inter8): the same coefficient tables
    /// [`Luma8`](TxbSet::Luma8) reads, but `get_tx_set` (spec 5.11.48)
    /// returns `TX_SET_INTER_3` at this size too under this encoder's
    /// always-on `reduced_tx_set` -- the same two-symbol alphabet
    /// [`Luma16Inter`](TxbSet::Luma16Inter) carries, just its own CDF (libaom
    /// `default_inter_ext_tx_cdf[EXT_TX_SET_DCT_IDTX][TX_8X8]`, a distinct
    /// adapted table from the 16x16 and 32x32 sizes' own entries in the same
    /// set, per `entropymode.c`).
    Luma8Inter,
    /// [`Luma8Inter`](TxbSet::Luma8Inter)'s `reduced_tx_set: false`
    /// counterpart (lane-cdffwd2): the sixteen-symbol `EXT_TX_SET_ALL16`
    /// `tx_type` table ([`crate::cdf::INTER_TX_TYPE_SET1_8`]).
    Luma8InterSet1,
    /// The 4x4 luma transform of a `TxMode::Select` 8x8 leaf or 16x16 block
    /// whose `tx_depth` resolves all the way down (lane-av1tx4). An
    /// `is_inter` block has no writer path yet, so this crate has no
    /// `Luma4Inter` counterpart to [`Luma8Inter`](TxbSet::Luma8Inter).
    Luma4,
    /// [`Luma4`](TxbSet::Luma4)'s `reduced_tx_set: false` counterpart, same
    /// split as [`Luma8Set1`](TxbSet::Luma8Set1).
    Luma4Set1,
    /// The 8x8 transform of a chroma plane of a 16x16 block.
    Chroma8,
    /// The 4x4 transform of a chroma plane of an 8x8 leaf (lane-av1-rect).
    /// r5 correction: libaom's `is_chroma_reference` (`av1_common_int.h`) is
    /// unconditionally true for `bsize >= BLOCK_8X8`, so chroma *does* follow
    /// luma all the way down to 8x8 — it is coded once per 8x8 leaf, not once
    /// per parent 16x16 (that "stays at the parent's Chroma8 granularity"
    /// framing this doc carried through r4 was never checked against a real
    /// decoder and is wrong; only `BLOCK_4X4`/`4X8`/`8X4`, one mi-unit wide or
    /// high, get the "last sub-block only" chroma-merge treatment). A real
    /// writer for the straddling-16x16 case must use this variant per leaf.
    Chroma4,
    /// The 16x16 transform of a chroma plane of a 32x32 block.
    Chroma16,
    /// The 32x32 transform of a chroma plane of a whole superblock.
    Chroma32,
}

/// The tables one transform block is coded with, borrowed from the state for
/// as long as that block is being written.
pub(crate) struct TxbTables<'a> {
    /// Side of the transform, in coefficients.
    pub side: usize,
    /// The all-zero flag, indexed by its own context.
    pub txb_skip: &'a mut [[u16; 3]],
    /// The end-of-block group, whose alphabet the transform size sets.
    pub eob_pt: &'a mut [u16],
    /// `eob_pt`'s `TX_CLASS_HORIZ`/`TX_CLASS_VERT` sibling table (r5): read
    /// instead of `eob_pt` once this TU's `tx_type` is known to be
    /// `V_DCT`/`H_DCT`. `None` for any size/set whose `tx_type` alphabet
    /// cannot produce those two symbols (chroma, and every inter set).
    pub eob_pt_class1: Option<&'a mut [u16]>,
    /// The top bit of the offset inside that group.
    pub eob_extra: &'a mut [[u16; 3]; 9],
    /// A coefficient's base level.
    pub base: &'a mut [[u16; 5]; 42],
    /// The base level of the last coefficient in the scan.
    pub base_eob: &'a mut [[u16; 4]; 4],
    /// The base-range tail above level two.
    pub br: &'a mut [[u16; 5]; 21],
    /// The sign of the DC.
    pub dc_sign: &'a mut [[u16; 3]; 3],
    /// The transform type, for the one size whose set holds more than one --
    /// a plain slice because the intra 16x16 table's alphabet (five types)
    /// and the inter 32x32 table's (two, `TX_SET_INTER_3`) are different
    /// sizes.
    pub tx_type: Option<&'a mut [u16]>,
}

/// Every table a key frame's tile writer adapts.
#[derive(Clone)]
pub(crate) struct Cdfs {
    /// The partition symbol of a 64x64 superblock.
    pub partition_w64: [[u16; 11]; 4],
    /// The partition symbol of a 32x32 block.
    pub partition_w32: [[u16; 11]; 4],
    /// Whether a block carries no residual at all.
    pub skip: [[u16; 3]; 3],
    /// The luma mode of a key frame's block, under the two neighbours' modes.
    pub kf_y_mode: [[[u16; 14]; 5]; 5],
    /// The chroma mode of a block too big to be offered chroma from luma.
    pub uv_mode_no_cfl: [[u16; 14]; 13],
    /// The chroma mode of a block that is offered it.
    pub uv_mode_cfl: [[u16; 15]; 13],
    /// The joint sign of a `UV_CFL_PRED` block's U/V alpha.
    pub cfl_sign: [u16; 9],
    /// A `UV_CFL_PRED` block's per-plane alpha magnitude, by joint-sign context.
    pub cfl_alpha: [[u16; 17]; 6],
    /// The angle a directional mode is nudged by.
    pub angle_delta: [[u16; 8]; 8],
    /// The all-zero flag of a 16x16 luma transform, 7 contexts (index 0 for
    /// a lone TU whose own bsize equals the block's; 1..6 the
    /// `skip_contexts[top][left]` table when `TxMode::Select` splits the
    /// block's luma into several smaller transform units, spec
    /// `get_txb_ctx_general`).
    pub txb_skip_luma_16: [[u16; 3]; 7],
    /// The same, for an 8x8 luma transform.
    pub txb_skip_luma_8: [[u16; 3]; 7],
    /// The same, for a 4x4 luma transform (lane-av1tx4).
    pub txb_skip_luma_4: [[u16; 3]; 7],
    /// The all-zero flag of an 8x8 chroma transform.
    pub txb_skip_chroma_8: [[u16; 3]; 3],
    /// The all-zero flag of a 4x4 chroma transform.
    pub txb_skip_chroma_4: [[u16; 3]; 3],
    /// The end-of-block group of a 16x16 luma transform.
    pub eob_pt_256_luma: [u16; 10],
    /// The end-of-block group of an 8x8 luma transform.
    pub eob_pt_64_luma: [u16; 8],
    /// The end-of-block group of an 8x8 chroma transform.
    pub eob_pt_64_chroma: [u16; 8],
    /// The end-of-block group of a 4x4 chroma transform.
    pub eob_pt_16_chroma: [u16; 6],
    /// The end-of-block group of a 4x4 luma transform (lane-av1tx4).
    pub eob_pt_16_luma: [u16; 6],
    /// `eob_pt_16_luma`'s `TX_CLASS_HORIZ`/`TX_CLASS_VERT` sibling, read
    /// instead whenever the TU's `tx_type` is `V_DCT`/`H_DCT` (r5).
    pub eob_pt_16_luma_class1: [u16; 6],
    /// `eob_pt_64_luma`'s class-1 sibling (r5), see `eob_pt_16_luma_class1`.
    pub eob_pt_64_luma_class1: [u16; 8],
    /// The end-of-block offset bit of a 16x16 luma transform.
    pub eob_extra_luma_16: [[u16; 3]; 9],
    /// The same, for an 8x8 luma transform.
    pub eob_extra_luma_8: [[u16; 3]; 9],
    /// The same, for a 4x4 luma transform.
    pub eob_extra_luma_4: [[u16; 3]; 9],
    /// The same, for an 8x8 chroma transform.
    pub eob_extra_chroma_8: [[u16; 3]; 9],
    /// The same, for a 4x4 chroma transform.
    pub eob_extra_chroma_4: [[u16; 3]; 9],
    /// The base level of a 16x16 luma coefficient.
    pub base_luma_16: [[u16; 5]; 42],
    /// The base level of an 8x8 luma coefficient.
    pub base_luma_8: [[u16; 5]; 42],
    /// The base level of a 4x4 luma coefficient.
    pub base_luma_4: [[u16; 5]; 42],
    /// The base level of an 8x8 chroma coefficient.
    pub base_chroma_8: [[u16; 5]; 42],
    /// The base level of a 4x4 chroma coefficient.
    pub base_chroma_4: [[u16; 5]; 42],
    /// The last coefficient's base level, for a 16x16 luma transform.
    pub base_eob_luma_16: [[u16; 4]; 4],
    /// The same, for an 8x8 luma transform.
    pub base_eob_luma_8: [[u16; 4]; 4],
    /// The same, for a 4x4 luma transform.
    pub base_eob_luma_4: [[u16; 4]; 4],
    /// The same, for an 8x8 chroma transform.
    pub base_eob_chroma_8: [[u16; 4]; 4],
    /// The same, for a 4x4 chroma transform.
    pub base_eob_chroma_4: [[u16; 4]; 4],
    /// The base-range tail of a 16x16 luma coefficient.
    pub br_luma_16: [[u16; 5]; 21],
    /// The base-range tail of an 8x8 luma coefficient.
    pub br_luma_8: [[u16; 5]; 21],
    /// The base-range tail of a 4x4 luma coefficient.
    pub br_luma_4: [[u16; 5]; 21],
    /// The base-range tail of an 8x8 chroma coefficient.
    pub br_chroma_8: [[u16; 5]; 21],
    /// The base-range tail of a 4x4 chroma coefficient.
    pub br_chroma_4: [[u16; 5]; 21],
    /// The partition symbol of a 16x16 block.
    pub partition_w16: [[u16; 11]; 4],
    /// The partition symbol of an 8x8 block (lane-av1-rect): only
    /// `PARTITION_NONE` is ever coded against it, but the alphabet still
    /// adapts on every read, same as every other partition table.
    pub partition_w8: [[u16; 5]; 4],
    /// The same as `txb_skip_luma_16`'s 7 contexts, for a 32x32 luma transform.
    pub txb_skip_luma_32: [[u16; 3]; 7],
    /// The same, for a 64x64 luma transform.
    pub txb_skip_luma_64: [[u16; 3]; 7],
    /// The all-zero flag of a 16x16 chroma transform.
    pub txb_skip_chroma_16: [[u16; 3]; 3],
    /// The all-zero flag of a 32x32 chroma transform.
    pub txb_skip_chroma_32: [[u16; 3]; 3],
    /// The end-of-block group of a luma transform of 1024 positions.
    pub eob_pt_1024_luma: [u16; 12],
    /// The same, for chroma.
    pub eob_pt_1024_chroma: [u16; 12],
    /// The end-of-block group of a 16x16 chroma transform.
    pub eob_pt_256_chroma: [u16; 10],
    /// The end-of-block offset bit, per table set.
    pub eob_extra_luma_32: [[u16; 3]; 9],
    /// The same, for a 64x64 luma transform.
    pub eob_extra_luma_64: [[u16; 3]; 9],
    /// The same, for a 16x16 chroma transform.
    pub eob_extra_chroma_16: [[u16; 3]; 9],
    /// The same, for a 32x32 chroma transform.
    pub eob_extra_chroma_32: [[u16; 3]; 9],
    /// The base level of a 32x32 luma coefficient.
    pub base_luma_32: [[u16; 5]; 42],
    /// The base level of a 64x64 luma coefficient.
    pub base_luma_64: [[u16; 5]; 42],
    /// The base level of a 16x16 chroma coefficient.
    pub base_chroma_16: [[u16; 5]; 42],
    /// The base level of a 32x32 chroma coefficient.
    pub base_chroma_32: [[u16; 5]; 42],
    /// The last coefficient's base level, per table set.
    pub base_eob_luma_32: [[u16; 4]; 4],
    /// The same, for a 64x64 luma transform.
    pub base_eob_luma_64: [[u16; 4]; 4],
    /// The same, for a 16x16 chroma transform.
    pub base_eob_chroma_16: [[u16; 4]; 4],
    /// The same, for a 32x32 chroma transform.
    pub base_eob_chroma_32: [[u16; 4]; 4],
    /// The base-range tail of a luma coefficient, which a 64x64 transform
    /// reads too because its index is clamped at the 32x32 size.
    pub br_luma_32: [[u16; 5]; 21],
    /// The base-range tail of a 16x16 chroma coefficient.
    pub br_chroma_16: [[u16; 5]; 21],
    /// The base-range tail of a 32x32 chroma coefficient.
    pub br_chroma_32: [[u16; 5]; 21],
    /// The DC sign of a luma coefficient, shared by both luma sets.
    pub dc_sign_luma: [[u16; 3]; 3],
    /// The transform type of a 16x16 intra luma transform, by luma mode.
    pub intra_tx_type_16: [[u16; 6]; 13],
    /// The transform type of an 8x8 intra luma transform, by luma mode.
    pub intra_tx_type_8: [[u16; 6]; 13],
    /// The transform type of an 8x8 intra luma transform, by luma mode, when
    /// `reduced_tx_set` is false (spec `TX_SET_INTRA_1`, seven types) --
    /// [`Self::intra_tx_type_8`]'s five-type table is only the
    /// `reduced_tx_set` row (round 8: a real `aomenc` stream with
    /// `reduced_tx_set` off desyncs an 8x8 TU read against this row's
    /// wrong-length CDF).
    pub intra_tx_type_8_set1: [[u16; 8]; 13],
    /// The transform type of a 4x4 intra luma transform, by luma mode
    /// (lane-av1tx4): the flat five-symbol `reduced_tx_set` row, byte-
    /// identical to [`Self::intra_tx_type_8`]'s own table
    /// ([`cdf::INTRA_TX_TYPE_SET2_8`]) -- libaom leaves `TX_4X4` at the same
    /// uniform default.
    pub intra_tx_type_4: [[u16; 6]; 13],
    /// [`Self::intra_tx_type_4`]'s `reduced_tx_set: false` counterpart, same
    /// split as [`Self::intra_tx_type_8_set1`].
    pub intra_tx_type_4_set1: [[u16; 8]; 13],
    /// The transform type of an `is_inter` 32x32 luma transform (spec
    /// `TX_SET_INTER_3`, `TX_32X32` row) -- unlike the intra 16x16 table, it
    /// is not indexed by mode.
    pub inter_tx_type_32: [u16; 3],
    /// The transform type of an `is_inter` 16x16 luma transform under
    /// `reduced_tx_set` (spec `TX_SET_INTER_3`, `TX_16X16` row) -- not
    /// indexed by mode, same as [`Self::inter_tx_type_32`].
    pub inter_tx_type_16: [u16; 3],
    /// The transform type of an `is_inter` 8x8 luma transform under
    /// `reduced_tx_set` (spec `TX_SET_INTER_3`, `TX_8X8` row) -- not indexed
    /// by mode, same as [`Self::inter_tx_type_32`] (lane-av1inter8).
    pub inter_tx_type_8: [u16; 3],
    /// [`Self::inter_tx_type_16`]'s `reduced_tx_set: false` counterpart
    /// (lane-cdffwd2): the twelve-symbol `EXT_TX_SET_DTT9_IDTX_1DDCT` row,
    /// not indexed by mode.
    pub inter_tx_type_16_set2: [u16; 13],
    /// [`Self::inter_tx_type_8`]'s `reduced_tx_set: false` counterpart
    /// (lane-cdffwd2): the sixteen-symbol `EXT_TX_SET_ALL16` row, not
    /// indexed by mode.
    pub inter_tx_type_8_set1: [u16; 17],
    /// The DC sign of a chroma coefficient, shared by both chroma sets.
    pub dc_sign_chroma: [[u16; 3]; 3],
    /// Whether an inter frame's block is coded as intra.
    pub intra_inter: [[u16; 3]; 4],
    /// The six binary decisions of an inter block's single reference frame.
    pub single_ref: [[[u16; 3]; 6]; 3],
    /// Whether a block reads `SINGLE_REFERENCE` or `COMPOUND_REFERENCE`
    /// (lane-av1comp, spec 5.11.25's `comp_mode`).
    pub comp_mode: [[u16; 3]; 5],
    /// Whether a block reads `skip_mode` (lane-av1comp, spec 5.11.29's
    /// `skip_mode`), indexed by `av1_get_skip_mode_context`.
    pub skip_mode: [[u16; 3]; 3],
    /// `use_obmc` (spec 5.11.24's `read_motion_mode`, the `OBMC_CAUSAL`-vs-
    /// `SIMPLE_TRANSLATION` two-symbol alphabet libaom's `obmc_cdf` reads
    /// whenever `motion_mode_allowed` can't offer `WARPED_CAUSAL` --
    /// lane-motionmode round 1 only reaches this arm, never the three-way
    /// `motion_mode_cdf`), indexed by this block's own square bsize: 0 =
    /// `BLOCK_8X8`, 1 = `BLOCK_16X16`, 2 = `BLOCK_32X32`, 3 = `BLOCK_64X64`
    /// (`default_obmc_cdf`'s own `BLOCK_SIZES_ALL` indices 3/6/9/12 --
    /// this decoder only ever codes those four square sizes).
    pub obmc: [[u16; 3]; 4],
    /// `interintra`, indexed by `size_group_lookup[bsize]` -- see
    /// `cdf::INTERINTRA`'s own doc.
    pub interintra: [[u16; 3]; 4],
    /// Which of the eight `INTER_COMPOUND_MODES` a `COMPOUND_REFERENCE`
    /// block takes (lane-av1comp, spec 5.11.24's `compound_mode`).
    pub inter_compound_mode: [[u16; 9]; 8],
    /// Unidirectional vs. bidirectional compound reference pair
    /// (lane-av1comp, spec 5.11.25's `comp_reference_type`).
    pub comp_ref_type: [[u16; 3]; 5],
    /// The three binary decisions of a unidirectional compound pair.
    pub uni_comp_ref: [[[u16; 3]; 3]; 3],
    /// The three binary decisions of a bidirectional pair's forward ref.
    pub comp_ref: [[[u16; 3]; 3]; 3],
    /// The two binary decisions of a bidirectional pair's backward ref.
    pub comp_bwdref: [[[u16; 3]; 2]; 3],
    /// Whether a compound block's `comp_group_idx` picks masked compound
    /// (lane-av1comp, spec 5.11.25's `comp_group_idx`).
    pub comp_group_idx: [[u16; 3]; 6],
    /// Whether a compound block's `compound_idx` picks the simple average
    /// over the distance-weighted blend (lane-av1comp, spec 5.11.25's
    /// `compound_idx`).
    pub compound_idx: [[u16; 3]; 6],
    /// The luma mode of an inter frame's intra block, by its size group.
    pub y_mode: [[u16; 14]; 4],
    /// Whether an inter block codes its motion vector explicitly.
    pub new_mv: [[u16; 3]; 6],
    /// Whether an inter block's motion vector is the zero vector.
    pub zero_mv: [[u16; 3]; 2],
    /// Which reference motion vector candidate an inter block takes.
    pub ref_mv: [[u16; 3]; 6],
    /// The dynamic reference list index of a motion vector prediction.
    pub drl_mode: [[u16; 3]; 3],
    /// A motion vector's joint (which components change), shared by both
    /// components since it is read once per vector, not once per component.
    pub mv_joint: [u16; 5],
    /// The two motion vector components' own tables: separate owned state,
    /// since spec 8.3.2 adapts one component without touching the other.
    pub mv_comp: [MvComponentCdfs; 2],
    /// The `use_filter_intra` flag, indexed by block-size class (`[0]`=4x4,
    /// `[1]`=8x8, `[2]`=16x16, `[3]`=32x32) -- see [`cdf::FILTER_INTRA`].
    pub filter_intra: [[u16; 3]; 4],
    /// Which `FILTER_INTRA_MODES` entry a `use_filter_intra` block picks.
    pub filter_intra_mode: [u16; 6],
    /// `TxMode::Select`'s `tx_depth` flag at an 8x8 block, by `tx_size_context`
    /// (0..=2) -- see [`cdf::TX_SIZE_CAT0`].
    pub tx_size_cat0: [[u16; 3]; 3],
    /// `tx_depth` at a 16x16 block -- see [`cdf::TX_SIZE_CAT1`].
    pub tx_size_cat1: [[u16; 4]; 3],
    /// `tx_depth` at a 32x32 block -- see [`cdf::TX_SIZE_CAT2`].
    pub tx_size_cat2: [[u16; 4]; 3],
    /// `tx_depth` at a 64x64 block -- see [`cdf::TX_SIZE_CAT3`].
    pub tx_size_cat3: [[u16; 4]; 3],
    /// A `SWITCHABLE`-filter inter block's own `interp_filter[dir]` -- see
    /// [`cdf::SWITCHABLE_INTERP`].
    pub switchable_interp: [[u16; 4]; 16],
}

/// One motion vector component's adapting state (spec 9.4's `Default_Mv_*`,
/// held once per component so that adapting one leaves the other alone).
#[derive(Clone)]
pub(crate) struct MvComponentCdfs {
    /// The magnitude class.
    pub class: [u16; 12],
    /// The integer bit of a small-class magnitude.
    pub class0_bit: [u16; 3],
    /// The fractional part of a small-class magnitude, by its integer bit.
    pub class0_fr: [[u16; 5]; 2],
    /// The half-pel bit of a small-class magnitude (spec `mv_class0_hp`),
    /// read only when the frame header sets `allow_high_precision_mv` (a
    /// real encoder's choice, not this crate's own writer's -- which always
    /// leaves the flag off and so never needs this table, but a foreign
    /// stream this decoder reads can set it per frame).
    pub class0_hp: [u16; 3],
    /// One bit of an above-small-class magnitude, by its position.
    pub bit: [[u16; 3]; 10],
    /// The fractional part of an above-small-class magnitude.
    pub fr: [u16; 5],
    /// The half-pel bit of an above-small-class magnitude (spec `mv_hp`),
    /// same `allow_high_precision_mv` gating as [`Self::class0_hp`].
    pub hp: [u16; 3],
    /// The component's sign.
    pub sign: [u16; 3],
}

impl MvComponentCdfs {
    fn new() -> MvComponentCdfs {
        MvComponentCdfs {
            class: cdf::MV_CLASS,
            class0_bit: cdf::MV_CLASS0_BIT,
            class0_fr: cdf::MV_CLASS0_FR,
            class0_hp: cdf::MV_CLASS0_HP,
            bit: cdf::MV_BIT,
            fr: cdf::MV_FR,
            hp: cdf::MV_HP,
            sign: cdf::MV_SIGN,
        }
    }
}

/// Picks one of the four q-context variants of a coefficient table
/// (`crate::cdf`'s `_Q0`/`_Q1`/plain/`_Q3` constants), `q_ctx` already reduced
/// to 0..=3 by the caller.
pub(crate) fn pick<T: Copy>(q_ctx: usize, q0: T, q1: T, q2: T, q3: T) -> T {
    match q_ctx {
        0 => q0,
        1 => q1,
        2 => q2,
        _ => q3,
    }
}

/// `reset_cdf_symbol_counter` (spec 7.20's `save_cdfs`, libaom
/// `av1_reset_cdf_symbol_counters`, `entropy.c`): every adapting table's last
/// entry is a symbol counter that slows [`update_cdf`](crate::msac)'s rate
/// down for a table's first 32 observations. A tile decoded with
/// `disable_frame_end_update_cdf == false` saves its *adapted* table into the
/// reference slots the frame refreshes, but the counters go back to zero
/// first -- forwarding the raw counts (this crate's bug until it was found:
/// the counts climbed unchecked, so every table forwarded across two or more
/// frames adapted *faster* than the real decoder's, a divergence too small to
/// flip any one symbol in a single hop but compounding into a visible
/// mismatch by the third).
fn reset1<const N: usize>(a: &mut [u16; N]) {
    a[N - 1] = 0;
}
fn reset2<const N: usize, const M: usize>(a: &mut [[u16; N]; M]) {
    a.iter_mut().for_each(reset1);
}
fn reset3<const N: usize, const M: usize, const K: usize>(a: &mut [[[u16; N]; M]; K]) {
    a.iter_mut().for_each(reset2);
}

impl MvComponentCdfs {
    fn reset_counts(&mut self) {
        reset1(&mut self.class);
        reset1(&mut self.class0_bit);
        reset2(&mut self.class0_fr);
        reset1(&mut self.class0_hp);
        reset2(&mut self.bit);
        reset1(&mut self.fr);
        reset1(&mut self.hp);
        reset1(&mut self.sign);
    }
}

impl Cdfs {
    /// Zeroes every table's symbol counter (its last entry), leaving the
    /// probabilities themselves untouched -- spec 7.20's `save_cdfs`. Call
    /// this on the tile's own end-of-tile adapted state before storing it
    /// into a reference slot for cross-frame forwarding; never on a table
    /// still being read within the same tile.
    pub(crate) fn reset_counts(&mut self) {
        reset2(&mut self.partition_w64);
        reset2(&mut self.partition_w32);
        reset2(&mut self.skip);
        reset3(&mut self.kf_y_mode);
        reset2(&mut self.uv_mode_no_cfl);
        reset2(&mut self.uv_mode_cfl);
        reset1(&mut self.cfl_sign);
        reset2(&mut self.cfl_alpha);
        reset2(&mut self.angle_delta);
        reset2(&mut self.txb_skip_luma_16);
        reset2(&mut self.txb_skip_luma_8);
        reset2(&mut self.txb_skip_luma_4);
        reset2(&mut self.txb_skip_chroma_8);
        reset2(&mut self.txb_skip_chroma_4);
        reset1(&mut self.eob_pt_256_luma);
        reset1(&mut self.eob_pt_64_luma);
        reset1(&mut self.eob_pt_64_chroma);
        reset1(&mut self.eob_pt_16_chroma);
        reset1(&mut self.eob_pt_16_luma);
        reset1(&mut self.eob_pt_16_luma_class1);
        reset1(&mut self.eob_pt_64_luma_class1);
        reset2(&mut self.eob_extra_luma_16);
        reset2(&mut self.eob_extra_luma_8);
        reset2(&mut self.eob_extra_luma_4);
        reset2(&mut self.eob_extra_chroma_8);
        reset2(&mut self.eob_extra_chroma_4);
        reset2(&mut self.base_luma_16);
        reset2(&mut self.base_luma_8);
        reset2(&mut self.base_luma_4);
        reset2(&mut self.base_chroma_8);
        reset2(&mut self.base_chroma_4);
        reset2(&mut self.base_eob_luma_16);
        reset2(&mut self.base_eob_luma_8);
        reset2(&mut self.base_eob_luma_4);
        reset2(&mut self.base_eob_chroma_8);
        reset2(&mut self.base_eob_chroma_4);
        reset2(&mut self.br_luma_16);
        reset2(&mut self.br_luma_8);
        reset2(&mut self.br_luma_4);
        reset2(&mut self.br_chroma_8);
        reset2(&mut self.br_chroma_4);
        reset2(&mut self.partition_w16);
        reset2(&mut self.partition_w8);
        reset2(&mut self.txb_skip_luma_32);
        reset2(&mut self.txb_skip_luma_64);
        reset2(&mut self.txb_skip_chroma_16);
        reset2(&mut self.txb_skip_chroma_32);
        reset1(&mut self.eob_pt_1024_luma);
        reset1(&mut self.eob_pt_1024_chroma);
        reset1(&mut self.eob_pt_256_chroma);
        reset2(&mut self.eob_extra_luma_32);
        reset2(&mut self.eob_extra_luma_64);
        reset2(&mut self.eob_extra_chroma_16);
        reset2(&mut self.eob_extra_chroma_32);
        reset2(&mut self.base_luma_32);
        reset2(&mut self.base_luma_64);
        reset2(&mut self.base_chroma_16);
        reset2(&mut self.base_chroma_32);
        reset2(&mut self.base_eob_luma_32);
        reset2(&mut self.base_eob_luma_64);
        reset2(&mut self.base_eob_chroma_16);
        reset2(&mut self.base_eob_chroma_32);
        reset2(&mut self.br_luma_32);
        reset2(&mut self.br_chroma_16);
        reset2(&mut self.br_chroma_32);
        reset2(&mut self.dc_sign_luma);
        reset2(&mut self.intra_tx_type_16);
        reset2(&mut self.intra_tx_type_8);
        reset2(&mut self.intra_tx_type_8_set1);
        reset2(&mut self.intra_tx_type_4);
        reset2(&mut self.intra_tx_type_4_set1);
        reset1(&mut self.inter_tx_type_32);
        reset1(&mut self.inter_tx_type_16);
        reset1(&mut self.inter_tx_type_8);
        reset1(&mut self.inter_tx_type_16_set2);
        reset1(&mut self.inter_tx_type_8_set1);
        reset2(&mut self.dc_sign_chroma);
        reset2(&mut self.intra_inter);
        reset3(&mut self.single_ref);
        reset2(&mut self.comp_mode);
        reset2(&mut self.skip_mode);
        reset2(&mut self.obmc);
        reset2(&mut self.inter_compound_mode);
        reset2(&mut self.comp_ref_type);
        reset3(&mut self.uni_comp_ref);
        reset3(&mut self.comp_ref);
        reset3(&mut self.comp_bwdref);
        reset2(&mut self.comp_group_idx);
        reset2(&mut self.compound_idx);
        reset2(&mut self.y_mode);
        reset2(&mut self.new_mv);
        reset2(&mut self.zero_mv);
        reset2(&mut self.ref_mv);
        reset2(&mut self.drl_mode);
        reset1(&mut self.mv_joint);
        self.mv_comp
            .iter_mut()
            .for_each(MvComponentCdfs::reset_counts);
        reset2(&mut self.filter_intra);
        reset1(&mut self.filter_intra_mode);
        reset2(&mut self.tx_size_cat0);
        reset2(&mut self.tx_size_cat1);
        reset2(&mut self.tx_size_cat2);
        reset2(&mut self.tx_size_cat3);
    }

    /// The defaults a key frame starts from (spec 8.4, `init_coeff_cdfs` and
    /// `init_non_coeff_cdfs`), for coefficient q-context `q_ctx` (0..=3, spec
    /// 8.3.2's `Get_Qctx`). The non-coefficient tables do not vary with the
    /// quantizer, so they are the same for every `q_ctx`.
    pub fn new(q_ctx: usize) -> Cdfs {
        Cdfs {
            partition_w64: cdf::PARTITION_W64,
            partition_w32: cdf::PARTITION_W32,
            skip: cdf::SKIP,
            kf_y_mode: cdf::KF_Y_MODE,
            uv_mode_no_cfl: cdf::UV_MODE_NO_CFL,
            uv_mode_cfl: cdf::UV_MODE_CFL,
            cfl_sign: cdf::CFL_SIGN,
            cfl_alpha: cdf::CFL_ALPHA,
            angle_delta: cdf::ANGLE_DELTA,
            txb_skip_luma_16: pick(
                q_ctx,
                cdf::TXB_SKIP_LUMA_16_Q0_CTX,
                cdf::TXB_SKIP_LUMA_16_Q1_CTX,
                cdf::TXB_SKIP_LUMA_16_CTX,
                cdf::TXB_SKIP_LUMA_16_Q3_CTX,
            ),
            txb_skip_luma_8: pick(
                q_ctx,
                cdf::TXB_SKIP_LUMA_8_Q0_CTX,
                cdf::TXB_SKIP_LUMA_8_Q1_CTX,
                cdf::TXB_SKIP_LUMA_8_CTX,
                cdf::TXB_SKIP_LUMA_8_Q3_CTX,
            ),
            txb_skip_luma_4: pick(
                q_ctx,
                cdf::TXB_SKIP_LUMA_4_Q0_CTX,
                cdf::TXB_SKIP_LUMA_4_Q1_CTX,
                cdf::TXB_SKIP_LUMA_4_CTX,
                cdf::TXB_SKIP_LUMA_4_Q3_CTX,
            ),
            txb_skip_chroma_8: pick(
                q_ctx,
                cdf::TXB_SKIP_CHROMA_8_Q0,
                cdf::TXB_SKIP_CHROMA_8_Q1,
                cdf::TXB_SKIP_CHROMA_8,
                cdf::TXB_SKIP_CHROMA_8_Q3,
            ),
            txb_skip_chroma_4: pick(
                q_ctx,
                cdf::TXB_SKIP_CHROMA_4_Q0,
                cdf::TXB_SKIP_CHROMA_4_Q1,
                cdf::TXB_SKIP_CHROMA_4,
                cdf::TXB_SKIP_CHROMA_4_Q3,
            ),
            eob_pt_256_luma: pick(
                q_ctx,
                cdf::EOB_PT_256_LUMA_Q0,
                cdf::EOB_PT_256_LUMA_Q1,
                cdf::EOB_PT_256_LUMA,
                cdf::EOB_PT_256_LUMA_Q3,
            ),
            eob_pt_64_luma: pick(
                q_ctx,
                cdf::EOB_PT_64_LUMA_Q0,
                cdf::EOB_PT_64_LUMA_Q1,
                cdf::EOB_PT_64_LUMA,
                cdf::EOB_PT_64_LUMA_Q3,
            ),
            eob_pt_64_chroma: pick(
                q_ctx,
                cdf::EOB_PT_64_CHROMA_Q0,
                cdf::EOB_PT_64_CHROMA_Q1,
                cdf::EOB_PT_64_CHROMA,
                cdf::EOB_PT_64_CHROMA_Q3,
            ),
            eob_pt_16_chroma: pick(
                q_ctx,
                cdf::EOB_PT_16_CHROMA_Q0,
                cdf::EOB_PT_16_CHROMA_Q1,
                cdf::EOB_PT_16_CHROMA,
                cdf::EOB_PT_16_CHROMA_Q3,
            ),
            eob_pt_16_luma: pick(
                q_ctx,
                cdf::EOB_PT_16_LUMA_Q0,
                cdf::EOB_PT_16_LUMA_Q1,
                cdf::EOB_PT_16_LUMA,
                cdf::EOB_PT_16_LUMA_Q3,
            ),
            eob_pt_16_luma_class1: pick(
                q_ctx,
                cdf::EOB_PT_16_LUMA_CLASS1_Q0,
                cdf::EOB_PT_16_LUMA_CLASS1_Q1,
                cdf::EOB_PT_16_LUMA_CLASS1,
                cdf::EOB_PT_16_LUMA_CLASS1_Q3,
            ),
            eob_pt_64_luma_class1: pick(
                q_ctx,
                cdf::EOB_PT_64_LUMA_CLASS1_Q0,
                cdf::EOB_PT_64_LUMA_CLASS1_Q1,
                cdf::EOB_PT_64_LUMA_CLASS1,
                cdf::EOB_PT_64_LUMA_CLASS1_Q3,
            ),
            eob_extra_luma_16: pick(
                q_ctx,
                cdf::EOB_EXTRA_LUMA_16_Q0,
                cdf::EOB_EXTRA_LUMA_16_Q1,
                cdf::EOB_EXTRA_LUMA_16,
                cdf::EOB_EXTRA_LUMA_16_Q3,
            ),
            eob_extra_luma_8: pick(
                q_ctx,
                cdf::EOB_EXTRA_LUMA_8_Q0,
                cdf::EOB_EXTRA_LUMA_8_Q1,
                cdf::EOB_EXTRA_LUMA_8,
                cdf::EOB_EXTRA_LUMA_8_Q3,
            ),
            eob_extra_luma_4: pick(
                q_ctx,
                cdf::EOB_EXTRA_LUMA_4_Q0,
                cdf::EOB_EXTRA_LUMA_4_Q1,
                cdf::EOB_EXTRA_LUMA_4,
                cdf::EOB_EXTRA_LUMA_4_Q3,
            ),
            eob_extra_chroma_8: pick(
                q_ctx,
                cdf::EOB_EXTRA_CHROMA_8_Q0,
                cdf::EOB_EXTRA_CHROMA_8_Q1,
                cdf::EOB_EXTRA_CHROMA_8,
                cdf::EOB_EXTRA_CHROMA_8_Q3,
            ),
            eob_extra_chroma_4: pick(
                q_ctx,
                cdf::EOB_EXTRA_CHROMA_4_Q0,
                cdf::EOB_EXTRA_CHROMA_4_Q1,
                cdf::EOB_EXTRA_CHROMA_4,
                cdf::EOB_EXTRA_CHROMA_4_Q3,
            ),
            base_luma_16: pick(
                q_ctx,
                cdf::COEFF_BASE_LUMA_16_Q0,
                cdf::COEFF_BASE_LUMA_16_Q1,
                cdf::COEFF_BASE_LUMA_16,
                cdf::COEFF_BASE_LUMA_16_Q3,
            ),
            base_luma_8: pick(
                q_ctx,
                cdf::COEFF_BASE_LUMA_8_Q0,
                cdf::COEFF_BASE_LUMA_8_Q1,
                cdf::COEFF_BASE_LUMA_8,
                cdf::COEFF_BASE_LUMA_8_Q3,
            ),
            base_luma_4: pick(
                q_ctx,
                cdf::COEFF_BASE_LUMA_4_Q0,
                cdf::COEFF_BASE_LUMA_4_Q1,
                cdf::COEFF_BASE_LUMA_4,
                cdf::COEFF_BASE_LUMA_4_Q3,
            ),
            base_chroma_8: pick(
                q_ctx,
                cdf::COEFF_BASE_CHROMA_8_Q0,
                cdf::COEFF_BASE_CHROMA_8_Q1,
                cdf::COEFF_BASE_CHROMA_8,
                cdf::COEFF_BASE_CHROMA_8_Q3,
            ),
            base_chroma_4: pick(
                q_ctx,
                cdf::COEFF_BASE_CHROMA_4_Q0,
                cdf::COEFF_BASE_CHROMA_4_Q1,
                cdf::COEFF_BASE_CHROMA_4,
                cdf::COEFF_BASE_CHROMA_4_Q3,
            ),
            base_eob_luma_16: pick(
                q_ctx,
                cdf::COEFF_BASE_EOB_LUMA_16_Q0,
                cdf::COEFF_BASE_EOB_LUMA_16_Q1,
                cdf::COEFF_BASE_EOB_LUMA_16,
                cdf::COEFF_BASE_EOB_LUMA_16_Q3,
            ),
            base_eob_luma_8: pick(
                q_ctx,
                cdf::COEFF_BASE_EOB_LUMA_8_Q0,
                cdf::COEFF_BASE_EOB_LUMA_8_Q1,
                cdf::COEFF_BASE_EOB_LUMA_8,
                cdf::COEFF_BASE_EOB_LUMA_8_Q3,
            ),
            base_eob_luma_4: pick(
                q_ctx,
                cdf::COEFF_BASE_EOB_LUMA_4_Q0,
                cdf::COEFF_BASE_EOB_LUMA_4_Q1,
                cdf::COEFF_BASE_EOB_LUMA_4,
                cdf::COEFF_BASE_EOB_LUMA_4_Q3,
            ),
            base_eob_chroma_8: pick(
                q_ctx,
                cdf::COEFF_BASE_EOB_CHROMA_8_Q0,
                cdf::COEFF_BASE_EOB_CHROMA_8_Q1,
                cdf::COEFF_BASE_EOB_CHROMA_8,
                cdf::COEFF_BASE_EOB_CHROMA_8_Q3,
            ),
            base_eob_chroma_4: pick(
                q_ctx,
                cdf::COEFF_BASE_EOB_CHROMA_4_Q0,
                cdf::COEFF_BASE_EOB_CHROMA_4_Q1,
                cdf::COEFF_BASE_EOB_CHROMA_4,
                cdf::COEFF_BASE_EOB_CHROMA_4_Q3,
            ),
            br_luma_16: pick(
                q_ctx,
                cdf::COEFF_BR_LUMA_16_Q0,
                cdf::COEFF_BR_LUMA_16_Q1,
                cdf::COEFF_BR_LUMA_16,
                cdf::COEFF_BR_LUMA_16_Q3,
            ),
            br_luma_8: pick(
                q_ctx,
                cdf::COEFF_BR_LUMA_8_Q0,
                cdf::COEFF_BR_LUMA_8_Q1,
                cdf::COEFF_BR_LUMA_8,
                cdf::COEFF_BR_LUMA_8_Q3,
            ),
            br_luma_4: pick(
                q_ctx,
                cdf::COEFF_BR_LUMA_4_Q0,
                cdf::COEFF_BR_LUMA_4_Q1,
                cdf::COEFF_BR_LUMA_4,
                cdf::COEFF_BR_LUMA_4_Q3,
            ),
            br_chroma_8: pick(
                q_ctx,
                cdf::COEFF_BR_CHROMA_8_Q0,
                cdf::COEFF_BR_CHROMA_8_Q1,
                cdf::COEFF_BR_CHROMA_8,
                cdf::COEFF_BR_CHROMA_8_Q3,
            ),
            br_chroma_4: pick(
                q_ctx,
                cdf::COEFF_BR_CHROMA_4_Q0,
                cdf::COEFF_BR_CHROMA_4_Q1,
                cdf::COEFF_BR_CHROMA_4,
                cdf::COEFF_BR_CHROMA_4_Q3,
            ),
            partition_w16: cdf::PARTITION_W16,
            partition_w8: cdf::PARTITION_W8,
            txb_skip_luma_32: pick(
                q_ctx,
                cdf::TXB_SKIP_LUMA_32_Q0_CTX,
                cdf::TXB_SKIP_LUMA_32_Q1_CTX,
                cdf::TXB_SKIP_LUMA_32_CTX,
                cdf::TXB_SKIP_LUMA_32_Q3_CTX,
            ),
            txb_skip_luma_64: pick(
                q_ctx,
                cdf::TXB_SKIP_LUMA_64_Q0_CTX,
                cdf::TXB_SKIP_LUMA_64_Q1_CTX,
                cdf::TXB_SKIP_LUMA_64_CTX,
                cdf::TXB_SKIP_LUMA_64_Q3_CTX,
            ),
            txb_skip_chroma_16: pick(
                q_ctx,
                cdf::TXB_SKIP_CHROMA_16_Q0,
                cdf::TXB_SKIP_CHROMA_16_Q1,
                cdf::TXB_SKIP_CHROMA_16,
                cdf::TXB_SKIP_CHROMA_16_Q3,
            ),
            txb_skip_chroma_32: pick(
                q_ctx,
                cdf::TXB_SKIP_CHROMA_32_Q0,
                cdf::TXB_SKIP_CHROMA_32_Q1,
                cdf::TXB_SKIP_CHROMA_32,
                cdf::TXB_SKIP_CHROMA_32_Q3,
            ),
            eob_pt_1024_luma: pick(
                q_ctx,
                cdf::EOB_PT_1024_LUMA_Q0,
                cdf::EOB_PT_1024_LUMA_Q1,
                cdf::EOB_PT_1024_LUMA,
                cdf::EOB_PT_1024_LUMA_Q3,
            ),
            eob_pt_1024_chroma: pick(
                q_ctx,
                cdf::EOB_PT_1024_CHROMA_Q0,
                cdf::EOB_PT_1024_CHROMA_Q1,
                cdf::EOB_PT_1024_CHROMA,
                cdf::EOB_PT_1024_CHROMA_Q3,
            ),
            eob_pt_256_chroma: pick(
                q_ctx,
                cdf::EOB_PT_256_CHROMA_Q0,
                cdf::EOB_PT_256_CHROMA_Q1,
                cdf::EOB_PT_256_CHROMA,
                cdf::EOB_PT_256_CHROMA_Q3,
            ),
            eob_extra_luma_32: pick(
                q_ctx,
                cdf::EOB_EXTRA_LUMA_32_Q0,
                cdf::EOB_EXTRA_LUMA_32_Q1,
                cdf::EOB_EXTRA_LUMA_32,
                cdf::EOB_EXTRA_LUMA_32_Q3,
            ),
            eob_extra_luma_64: pick(
                q_ctx,
                cdf::EOB_EXTRA_LUMA_64_Q0,
                cdf::EOB_EXTRA_LUMA_64_Q1,
                cdf::EOB_EXTRA_LUMA_64,
                cdf::EOB_EXTRA_LUMA_64_Q3,
            ),
            eob_extra_chroma_16: pick(
                q_ctx,
                cdf::EOB_EXTRA_CHROMA_16_Q0,
                cdf::EOB_EXTRA_CHROMA_16_Q1,
                cdf::EOB_EXTRA_CHROMA_16,
                cdf::EOB_EXTRA_CHROMA_16_Q3,
            ),
            eob_extra_chroma_32: pick(
                q_ctx,
                cdf::EOB_EXTRA_CHROMA_32_Q0,
                cdf::EOB_EXTRA_CHROMA_32_Q1,
                cdf::EOB_EXTRA_CHROMA_32,
                cdf::EOB_EXTRA_CHROMA_32_Q3,
            ),
            base_luma_32: pick(
                q_ctx,
                cdf::COEFF_BASE_LUMA_32_Q0,
                cdf::COEFF_BASE_LUMA_32_Q1,
                cdf::COEFF_BASE_LUMA_32,
                cdf::COEFF_BASE_LUMA_32_Q3,
            ),
            base_luma_64: pick(
                q_ctx,
                cdf::COEFF_BASE_LUMA_64_Q0,
                cdf::COEFF_BASE_LUMA_64_Q1,
                cdf::COEFF_BASE_LUMA_64,
                cdf::COEFF_BASE_LUMA_64_Q3,
            ),
            base_chroma_16: pick(
                q_ctx,
                cdf::COEFF_BASE_CHROMA_16_Q0,
                cdf::COEFF_BASE_CHROMA_16_Q1,
                cdf::COEFF_BASE_CHROMA_16,
                cdf::COEFF_BASE_CHROMA_16_Q3,
            ),
            base_chroma_32: pick(
                q_ctx,
                cdf::COEFF_BASE_CHROMA_32_Q0,
                cdf::COEFF_BASE_CHROMA_32_Q1,
                cdf::COEFF_BASE_CHROMA_32,
                cdf::COEFF_BASE_CHROMA_32_Q3,
            ),
            base_eob_luma_32: pick(
                q_ctx,
                cdf::COEFF_BASE_EOB_LUMA_32_Q0,
                cdf::COEFF_BASE_EOB_LUMA_32_Q1,
                cdf::COEFF_BASE_EOB_LUMA_32,
                cdf::COEFF_BASE_EOB_LUMA_32_Q3,
            ),
            base_eob_luma_64: pick(
                q_ctx,
                cdf::COEFF_BASE_EOB_LUMA_64_Q0,
                cdf::COEFF_BASE_EOB_LUMA_64_Q1,
                cdf::COEFF_BASE_EOB_LUMA_64,
                cdf::COEFF_BASE_EOB_LUMA_64_Q3,
            ),
            base_eob_chroma_16: pick(
                q_ctx,
                cdf::COEFF_BASE_EOB_CHROMA_16_Q0,
                cdf::COEFF_BASE_EOB_CHROMA_16_Q1,
                cdf::COEFF_BASE_EOB_CHROMA_16,
                cdf::COEFF_BASE_EOB_CHROMA_16_Q3,
            ),
            base_eob_chroma_32: pick(
                q_ctx,
                cdf::COEFF_BASE_EOB_CHROMA_32_Q0,
                cdf::COEFF_BASE_EOB_CHROMA_32_Q1,
                cdf::COEFF_BASE_EOB_CHROMA_32,
                cdf::COEFF_BASE_EOB_CHROMA_32_Q3,
            ),
            br_luma_32: pick(
                q_ctx,
                cdf::COEFF_BR_LUMA_32_Q0,
                cdf::COEFF_BR_LUMA_32_Q1,
                cdf::COEFF_BR_LUMA_32,
                cdf::COEFF_BR_LUMA_32_Q3,
            ),
            br_chroma_16: pick(
                q_ctx,
                cdf::COEFF_BR_CHROMA_16_Q0,
                cdf::COEFF_BR_CHROMA_16_Q1,
                cdf::COEFF_BR_CHROMA_16,
                cdf::COEFF_BR_CHROMA_16_Q3,
            ),
            br_chroma_32: pick(
                q_ctx,
                cdf::COEFF_BR_CHROMA_32_Q0,
                cdf::COEFF_BR_CHROMA_32_Q1,
                cdf::COEFF_BR_CHROMA_32,
                cdf::COEFF_BR_CHROMA_32_Q3,
            ),
            dc_sign_luma: cdf::DC_SIGN_LUMA,
            intra_tx_type_16: cdf::INTRA_TX_TYPE_SET2_16,
            intra_tx_type_8: cdf::INTRA_TX_TYPE_SET2_8,
            intra_tx_type_8_set1: cdf::INTRA_TX_TYPE_SET1_8,
            intra_tx_type_4: cdf::INTRA_TX_TYPE_SET2_8,
            intra_tx_type_4_set1: cdf::INTRA_TX_TYPE_SET1_4,
            inter_tx_type_32: cdf::INTER_TX_TYPE_SET3_32,
            inter_tx_type_16: cdf::INTER_TX_TYPE_SET3_16,
            inter_tx_type_8: cdf::INTER_TX_TYPE_SET3_8,
            inter_tx_type_16_set2: cdf::INTER_TX_TYPE_SET2_16,
            inter_tx_type_8_set1: cdf::INTER_TX_TYPE_SET1_8,
            dc_sign_chroma: cdf::DC_SIGN_CHROMA,
            intra_inter: cdf::INTRA_INTER,
            single_ref: cdf::SINGLE_REF,
            comp_mode: cdf::COMP_MODE,
            inter_compound_mode: cdf::INTER_COMPOUND_MODE,
            comp_ref_type: cdf::COMP_REF_TYPE,
            uni_comp_ref: cdf::UNI_COMP_REF,
            comp_ref: cdf::COMP_REF,
            comp_bwdref: cdf::COMP_BWDREF,
            skip_mode: cdf::SKIP_MODE,
            obmc: cdf::OBMC,
            interintra: cdf::INTERINTRA,
            comp_group_idx: cdf::COMP_GROUP_IDX,
            compound_idx: cdf::COMPOUND_IDX,
            y_mode: cdf::Y_MODE,
            new_mv: cdf::NEW_MV,
            zero_mv: cdf::ZERO_MV,
            ref_mv: cdf::REF_MV,
            drl_mode: cdf::DRL_MODE,
            mv_joint: cdf::MV_JOINT,
            mv_comp: [MvComponentCdfs::new(), MvComponentCdfs::new()],
            filter_intra: cdf::FILTER_INTRA,
            filter_intra_mode: cdf::FILTER_INTRA_MODE,
            tx_size_cat0: cdf::TX_SIZE_CAT0,
            tx_size_cat1: cdf::TX_SIZE_CAT1,
            tx_size_cat2: cdf::TX_SIZE_CAT2,
            tx_size_cat3: cdf::TX_SIZE_CAT3,
            switchable_interp: cdf::SWITCHABLE_INTERP,
        }
    }

    /// The tables one table set is coded with, borrowed out of the state.
    pub fn txb(&mut self, set: TxbSet, mode: usize) -> TxbTables<'_> {
        match set {
            TxbSet::Luma32 => TxbTables {
                side: 32,
                txb_skip: &mut self.txb_skip_luma_32,
                eob_pt: &mut self.eob_pt_1024_luma,
                eob_extra: &mut self.eob_extra_luma_32,
                base: &mut self.base_luma_32,
                base_eob: &mut self.base_eob_luma_32,
                br: &mut self.br_luma_32,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: None,
                eob_pt_class1: None,
            },
            TxbSet::Luma32Inter => TxbTables {
                side: 32,
                txb_skip: &mut self.txb_skip_luma_32,
                eob_pt: &mut self.eob_pt_1024_luma,
                eob_extra: &mut self.eob_extra_luma_32,
                base: &mut self.base_luma_32,
                base_eob: &mut self.base_eob_luma_32,
                br: &mut self.br_luma_32,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: Some(self.inter_tx_type_32.as_mut_slice()),
                eob_pt_class1: None,
            },
            TxbSet::Luma64 => TxbTables {
                side: 32,
                txb_skip: &mut self.txb_skip_luma_64,
                eob_pt: &mut self.eob_pt_1024_luma,
                eob_extra: &mut self.eob_extra_luma_64,
                base: &mut self.base_luma_64,
                base_eob: &mut self.base_eob_luma_64,
                br: &mut self.br_luma_32,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: None,
                eob_pt_class1: None,
            },
            TxbSet::Luma16 => TxbTables {
                side: 16,
                txb_skip: &mut self.txb_skip_luma_16,
                eob_pt: &mut self.eob_pt_256_luma,
                eob_extra: &mut self.eob_extra_luma_16,
                base: &mut self.base_luma_16,
                base_eob: &mut self.base_eob_luma_16,
                br: &mut self.br_luma_16,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: Some(self.intra_tx_type_16[mode].as_mut_slice()),
                eob_pt_class1: None,
            },
            TxbSet::Luma16Inter => TxbTables {
                side: 16,
                txb_skip: &mut self.txb_skip_luma_16,
                eob_pt: &mut self.eob_pt_256_luma,
                eob_extra: &mut self.eob_extra_luma_16,
                base: &mut self.base_luma_16,
                base_eob: &mut self.base_eob_luma_16,
                br: &mut self.br_luma_16,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: Some(self.inter_tx_type_16.as_mut_slice()),
                eob_pt_class1: None,
            },
            TxbSet::Luma16InterSet1 => TxbTables {
                side: 16,
                txb_skip: &mut self.txb_skip_luma_16,
                eob_pt: &mut self.eob_pt_256_luma,
                eob_extra: &mut self.eob_extra_luma_16,
                base: &mut self.base_luma_16,
                base_eob: &mut self.base_eob_luma_16,
                br: &mut self.br_luma_16,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: Some(self.inter_tx_type_16_set2.as_mut_slice()),
                eob_pt_class1: None,
            },
            TxbSet::Luma8 => TxbTables {
                side: 8,
                txb_skip: &mut self.txb_skip_luma_8,
                eob_pt: &mut self.eob_pt_64_luma,
                eob_extra: &mut self.eob_extra_luma_8,
                base: &mut self.base_luma_8,
                base_eob: &mut self.base_eob_luma_8,
                br: &mut self.br_luma_8,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: Some(self.intra_tx_type_8[mode].as_mut_slice()),
                eob_pt_class1: Some(&mut self.eob_pt_64_luma_class1),
            },
            TxbSet::Luma8Set1 => TxbTables {
                side: 8,
                txb_skip: &mut self.txb_skip_luma_8,
                eob_pt: &mut self.eob_pt_64_luma,
                eob_extra: &mut self.eob_extra_luma_8,
                base: &mut self.base_luma_8,
                base_eob: &mut self.base_eob_luma_8,
                br: &mut self.br_luma_8,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: Some(self.intra_tx_type_8_set1[mode].as_mut_slice()),
                eob_pt_class1: Some(&mut self.eob_pt_64_luma_class1),
            },
            TxbSet::Luma4 => TxbTables {
                side: 4,
                txb_skip: &mut self.txb_skip_luma_4,
                eob_pt: &mut self.eob_pt_16_luma,
                eob_extra: &mut self.eob_extra_luma_4,
                base: &mut self.base_luma_4,
                base_eob: &mut self.base_eob_luma_4,
                br: &mut self.br_luma_4,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: Some(self.intra_tx_type_4[mode].as_mut_slice()),
                eob_pt_class1: Some(&mut self.eob_pt_16_luma_class1),
            },
            TxbSet::Luma4Set1 => TxbTables {
                side: 4,
                txb_skip: &mut self.txb_skip_luma_4,
                eob_pt: &mut self.eob_pt_16_luma,
                eob_extra: &mut self.eob_extra_luma_4,
                base: &mut self.base_luma_4,
                base_eob: &mut self.base_eob_luma_4,
                br: &mut self.br_luma_4,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: Some(self.intra_tx_type_4_set1[mode].as_mut_slice()),
                eob_pt_class1: Some(&mut self.eob_pt_16_luma_class1),
            },
            TxbSet::Luma8Inter => TxbTables {
                side: 8,
                txb_skip: &mut self.txb_skip_luma_8,
                eob_pt: &mut self.eob_pt_64_luma,
                eob_extra: &mut self.eob_extra_luma_8,
                base: &mut self.base_luma_8,
                base_eob: &mut self.base_eob_luma_8,
                br: &mut self.br_luma_8,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: Some(self.inter_tx_type_8.as_mut_slice()),
                eob_pt_class1: None,
            },
            TxbSet::Luma8InterSet1 => TxbTables {
                side: 8,
                txb_skip: &mut self.txb_skip_luma_8,
                eob_pt: &mut self.eob_pt_64_luma,
                eob_extra: &mut self.eob_extra_luma_8,
                base: &mut self.base_luma_8,
                base_eob: &mut self.base_eob_luma_8,
                br: &mut self.br_luma_8,
                dc_sign: &mut self.dc_sign_luma,
                tx_type: Some(self.inter_tx_type_8_set1.as_mut_slice()),
                eob_pt_class1: Some(&mut self.eob_pt_64_luma_class1),
            },
            TxbSet::Chroma8 => TxbTables {
                side: 8,
                txb_skip: &mut self.txb_skip_chroma_8,
                eob_pt: &mut self.eob_pt_64_chroma,
                eob_extra: &mut self.eob_extra_chroma_8,
                base: &mut self.base_chroma_8,
                base_eob: &mut self.base_eob_chroma_8,
                br: &mut self.br_chroma_8,
                dc_sign: &mut self.dc_sign_chroma,
                tx_type: None,
                eob_pt_class1: None,
            },
            TxbSet::Chroma4 => TxbTables {
                side: 4,
                txb_skip: &mut self.txb_skip_chroma_4,
                eob_pt: &mut self.eob_pt_16_chroma,
                eob_extra: &mut self.eob_extra_chroma_4,
                base: &mut self.base_chroma_4,
                base_eob: &mut self.base_eob_chroma_4,
                br: &mut self.br_chroma_4,
                dc_sign: &mut self.dc_sign_chroma,
                tx_type: None,
                eob_pt_class1: None,
            },
            TxbSet::Chroma16 => TxbTables {
                side: 16,
                txb_skip: &mut self.txb_skip_chroma_16,
                eob_pt: &mut self.eob_pt_256_chroma,
                eob_extra: &mut self.eob_extra_chroma_16,
                base: &mut self.base_chroma_16,
                base_eob: &mut self.base_eob_chroma_16,
                br: &mut self.br_chroma_16,
                dc_sign: &mut self.dc_sign_chroma,
                tx_type: None,
                eob_pt_class1: None,
            },
            TxbSet::Chroma32 => TxbTables {
                side: 32,
                txb_skip: &mut self.txb_skip_chroma_32,
                eob_pt: &mut self.eob_pt_1024_chroma,
                eob_extra: &mut self.eob_extra_chroma_32,
                base: &mut self.base_chroma_32,
                base_eob: &mut self.base_eob_chroma_32,
                br: &mut self.br_chroma_32,
                dc_sign: &mut self.dc_sign_chroma,
                tx_type: None,
                eob_pt_class1: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64x64 luma transform reads the 32x32 base-range rows, so the two sets
    /// have to adapt the same table: a decoder keeps one copy, and an encoder
    /// that kept two would drift away from it the first time a level above two
    /// was coded.
    #[test]
    fn the_two_luma_sets_share_the_base_range_table() {
        let mut cdfs = Cdfs::new(2);
        cdfs.txb(TxbSet::Luma64, 0).br[0][0] = 1;
        assert_eq!(cdfs.txb(TxbSet::Luma32, 0).br[0][0], 1);
    }

    /// Both luma sets read one DC sign table, and both chroma sets another;
    /// what a luma block adapts must not reach the chroma one.
    #[test]
    fn the_sign_tables_are_shared_within_a_plane_type_and_not_across() {
        let mut cdfs = Cdfs::new(2);
        cdfs.txb(TxbSet::Luma32, 0).dc_sign[1][0] = 1;
        cdfs.txb(TxbSet::Chroma16, 0).dc_sign[2][0] = 2;
        assert_eq!(cdfs.txb(TxbSet::Luma64, 0).dc_sign[1][0], 1);
        assert_eq!(cdfs.txb(TxbSet::Chroma32, 0).dc_sign[2][0], 2);
        assert_ne!(cdfs.txb(TxbSet::Chroma32, 0).dc_sign[1][0], 1);
        assert_ne!(cdfs.txb(TxbSet::Luma64, 0).dc_sign[2][0], 2);
    }

    /// The end-of-block table a 1024-position transform reads is shared by both
    /// luma sets and separate from chroma's, which is the same hazard one table
    /// further along.
    #[test]
    fn the_luma_end_of_block_table_is_shared_and_chromas_is_not() {
        let mut cdfs = Cdfs::new(2);
        cdfs.txb(TxbSet::Luma32, 0).eob_pt[0] = 7;
        assert_eq!(cdfs.txb(TxbSet::Luma64, 0).eob_pt[0], 7);
        assert_ne!(cdfs.txb(TxbSet::Chroma32, 0).eob_pt[0], 7);
    }

    /// Every set starts from the spec's defaults, so a tile that adapts nothing
    /// writes exactly what a non-adapting one does.
    #[test]
    fn the_state_starts_at_the_defaults() {
        let mut cdfs = Cdfs::new(2);
        assert_eq!(
            cdfs.txb(TxbSet::Luma32, 0).base[3],
            cdf::COEFF_BASE_LUMA_32[3]
        );
        assert_eq!(
            cdfs.txb(TxbSet::Chroma16, 0).eob_extra[2],
            cdf::EOB_EXTRA_CHROMA_16[2]
        );
        assert_eq!(cdfs.partition_w64[2], cdf::PARTITION_W64[2]);
        assert_eq!(cdfs.intra_inter, cdf::INTRA_INTER);
        assert_eq!(cdfs.single_ref, cdf::SINGLE_REF);
        assert_eq!(cdfs.comp_mode, cdf::COMP_MODE);
        assert_eq!(cdfs.inter_compound_mode, cdf::INTER_COMPOUND_MODE);
        assert_eq!(cdfs.comp_ref_type, cdf::COMP_REF_TYPE);
        assert_eq!(cdfs.uni_comp_ref, cdf::UNI_COMP_REF);
        assert_eq!(cdfs.comp_ref, cdf::COMP_REF);
        assert_eq!(cdfs.comp_bwdref, cdf::COMP_BWDREF);
        assert_eq!(cdfs.y_mode, cdf::Y_MODE);
        assert_eq!(cdfs.new_mv, cdf::NEW_MV);
        assert_eq!(cdfs.zero_mv, cdf::ZERO_MV);
        assert_eq!(cdfs.ref_mv, cdf::REF_MV);
        assert_eq!(cdfs.drl_mode, cdf::DRL_MODE);
        assert_eq!(cdfs.mv_joint, cdf::MV_JOINT);
        assert_eq!(cdfs.mv_comp[0].class, cdf::MV_CLASS);
        assert_eq!(cdfs.mv_comp[1].sign, cdf::MV_SIGN);
    }

    /// A motion vector's two components are separate owned tables: adapting
    /// one component's CDFs must not be visible through the other, the same
    /// hazard the coefficient sets guard against above.
    #[test]
    fn the_mv_components_are_separate_and_do_not_share_adaptation() {
        let mut cdfs = Cdfs::new(2);
        cdfs.mv_comp[0].class[0] = 1;
        cdfs.mv_comp[0].bit[3][0] = 2;
        assert_ne!(cdfs.mv_comp[1].class[0], 1);
        assert_ne!(cdfs.mv_comp[1].bit[3][0], 2);
        assert_eq!(cdfs.mv_comp[1].class[0], cdf::MV_CLASS[0]);
        assert_eq!(cdfs.mv_comp[1].bit[3][0], cdf::MV_BIT[3][0]);
    }
}
