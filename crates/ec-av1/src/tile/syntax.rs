use super::*;

/// One quantised coefficient of a transform block.
///
/// `row` and `col` are its position in the transform it belongs to — 32x32 for
/// luma, 16x16 for each chroma plane — not in the picture: a coefficient at
/// row 0 varies along the picture's width and one at column 0 along its
/// height, and the coefficient at the origin is the DC the block's average
/// sample value rides on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coeff {
    /// The coefficient's row in the transform.
    pub row: u8,
    /// The coefficient's column in the transform.
    pub col: u8,
    /// Its quantised level, which the base and base-range syntax carry for
    /// magnitudes up to [`MAX_LEVEL`].
    pub level: i32,
}

/// The coefficients of one coded block: a 32x32 luma transform and, at 4:2:0,
/// the 16x16 transform each chroma plane covers the same area with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockCoeffs {
    /// The luma transform's coefficients.
    pub luma: Vec<Coeff>,
    /// One transform type per luma transform unit, raster order (lane-txset).
    /// Empty is `DCT_DCT` throughout -- what every block coded before the
    /// search offered a type, and what an inter block still carries.
    pub luma_tx_types: Vec<TxType>,
    /// The U plane's.
    pub u: Vec<Coeff>,
    /// The V plane's.
    pub v: Vec<Coeff>,
    /// The luma intra mode the block is predicted with, one of the thirteen
    /// modes a key frame codes (`DC_PRED` is zero, which is what
    /// [`Default`] and a plain coefficient list give).
    pub mode: u8,
    /// The chroma intra mode both chroma planes are predicted with (spec
    /// `uv_mode`, one symbol for the pair). `DC_PRED` is zero, which is what
    /// [`Default`] gives; a non-`DC_PRED` mode also changes the transform
    /// type the decoder derives for chroma (`Intra_Mode_To_Tx_Type`, spec
    /// 9.3), which is never coded as a symbol.
    pub uv_mode: u8,
    /// Whether the block carries no residual at all (spec `skip`). An intra
    /// block may still be skipped; `false` (the [`Default`]) codes whatever
    /// `luma`/`u`/`v` carry.
    pub skip: bool,
    /// The block's inter mode and motion vector, or `None` for an intra
    /// block. Only [`sb_coeff_inter_frame_tile`] codes `is_inter`; the key
    /// frame writers never read this field.
    pub inter: Option<InterInfo>,
    /// This (single-reference inter) block's spec `motion_mode`, written by
    /// [`write_motion_mode`]: 0 `SIMPLE_TRANSLATION` (the [`Default`], and
    /// the only mode this writer coded before lane-av1obmc), 1 `OBMC_CAUSAL`,
    /// 2 `WARPED_CAUSAL` (lane-av1obmc2).
    pub motion_mode: u8,
    /// The 8x8 leaves a straddling 16x16 block is split into (lane-av1-rect),
    /// each with its own 8x8 luma transform and 4x4 chroma transforms, in
    /// raster order among the leaves that are inside the true frame. `Some`
    /// overrides this `BlockCoeffs`'s own `luma`/`u`/`v`/`mode`, which are left
    /// at their defaults and unused, the same way a `Whole` [`Superblock`]'s
    /// per-quadrant fields are unused.
    pub eight: Option<Vec<BlockCoeffs>>,
    /// The luma palette this block is predicted with (spec 5.11.46
    /// `palette_mode_info`), or `None` for an ordinary intra block. Only a
    /// `DC_PRED` block on a frame whose header set
    /// `allow_screen_content_tools` may carry one, and it forces
    /// `tx_depth` to zero (the whole block is one transform over the
    /// palette's own prediction).
    pub palette: Option<PaletteY>,
    /// The chroma palette this block's two chroma planes are predicted with
    /// (spec 5.11.46 `palette_mode_info`'s plane-1 half), or `None`. Only a
    /// `UV_DC_PRED` block of an `allow_screen_content_tools` frame may carry
    /// one; it is independent of [`Self::palette`] (either, both or neither).
    pub palette_uv: Option<PaletteUv>,
    /// How many times this block's luma transform is halved under
    /// `TxMode::Select` (spec `tx_depth`, 0 = one transform over the whole
    /// block). Read only by the writers a `tx_select` frame calls; a
    /// `TxMode::Largest` frame codes no symbol and leaves this at zero.
    /// `luma` then carries the whole block's levels in BLOCK coordinates,
    /// which the writer slices per transform unit.
    pub tx_depth: u8,
    /// This block's `UV_CFL_PRED` alphas (`alpha_q3` for U and V, spec
    /// 5.11.45's `cfl_alpha_signs`/`cfl_alpha_u`/`cfl_alpha_v`), or `None` for
    /// any other chroma mode. `Some` iff [`Self::uv_mode`] is
    /// [`UV_CFL_PRED`]; at least one of the pair is nonzero, since the joint
    /// sign symbol has no (ZERO, ZERO) value.
    pub cfl_alphas: Option<(i32, i32)>,
    /// This block's `filter_intra_mode` (spec 5.11.14, 0..=4) when it is
    /// coded with recursive filter intra, or `None`. `Some` only on a DC_PRED
    /// luma block of at most 32x32 with no luma palette, on a sequence whose
    /// header set `enable_filter_intra` ([`arm_filter_intra`]); the block's
    /// luma transform types are then coded from
    /// `fimode_to_intradir[filter_intra_mode]`'s row ([`tx_row`]) while its
    /// neighbour mode stays `DC_PRED`.
    pub filter_intra: Option<u8>,
    /// This block's `angle_delta_y` (spec `read_intra_angle_info`, -3..=3),
    /// zero for every non-directional luma mode -- which is what
    /// [`Default`] gives, and what every writer coded before lane-av1cfl.
    pub angle_delta_y: i8,
    /// This block's intra block-copy vector (spec 5.11.13 `use_intrabc` ->
    /// `assign_dv`), in the spec's 1/8-pel `(row, col)` units, or `None` for
    /// an ordinary intra block. Only a key frame whose header set
    /// `allow_intrabc` ([`arm_intrabc`]) may carry one; the block is then
    /// coded `skip` with no residual, no mode, no palette -- exactly what
    /// decode.rs `read_intra_mode` reads back and returns early on.
    ///
    /// lane-av1ibc corner-cut, ceiling named: a residual-carrying intrabc
    /// block needs the INTER coefficient tables plus the var-tx tree
    /// decode.rs `decode_block`'s `intrabc_vartx` arm reads; the upgrade path
    /// is to route the block through [`write_tx_syntax_inter`] instead of
    /// returning early here.
    pub dv: Option<(i32, i32)>,
}

/// A block's luma palette: its base colours (ascending, only the first
/// `size` entries are real) and the per-pixel colour-index map, `side*side`
/// row-major -- exactly what [`crate::decode`]'s own `PaletteY` carries back
/// out of the stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteY {
    /// How many base colours, 2..=8 (spec `PaletteSizeY`).
    pub size: u8,
    /// The base colours, ascending.
    pub colors: [u16; 8],
    /// Which colour each pixel takes, row-major over the block's own side.
    pub map: Vec<u8>,
}

/// As [`PaletteY`], for a block's chroma palette -- one colour-index map
/// SHARED by U and V (spec `av1_visit_palette`'s plane-1 pass; the two planes
/// are co-located after 4:2:0 subsampling) plus each plane's own base
/// colours, exactly what [`crate::decode`]'s own `PaletteUv` carries out of
/// the stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteUv {
    /// How many base colours, 2..=8 (spec `PaletteSizeUV`).
    pub size: u8,
    /// U's base colours, ASCENDING and distinct -- `read_palette_colors_uv`
    /// merges the cached and transmitted lists as two ascending runs, and its
    /// delta form only ever steps upwards.
    pub u_colors: [u16; 8],
    /// V's base colours, in the same cluster order as [`Self::u_colors`] --
    /// the shared map indexes both, and V's own coding needs no ordering.
    pub v_colors: [u16; 8],
    /// Which colour each chroma pixel takes, row-major over the subsampled
    /// block's own `(side / 2).max(4)` side.
    pub map: Vec<u8>,
}

/// One inter mode [`sb_coeff_inter_frame_tile`]'s blocks may take: the whole
/// single-reference set of spec 5.11.24 `read_inter_mode`, each written by
/// [`write_inter_mode`] with the decoder's own contexts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterMode {
    /// `NEARESTMV`: takes `MvStack::nearest_mv` outright, no residual, no DRL.
    NearestMv,
    /// `NEARMV`: takes the stack entry [`InterInfo::ref_mv_idx`] names
    /// (clamped to at least 1, the spec's own `RefMvIdx` base for this mode).
    NearMv,
    /// `GLOBALMV`: the identity global model's zero vector, two symbols and
    /// no MV at all.
    GlobalMv,
    /// `NEWMV`: codes `mv` as a residual against the stack entry
    /// [`InterInfo::ref_mv_idx`] names.
    NewMv,
    /// `NEAREST_NEARESTMV`: both sides take the compound stack's own nearest
    /// pair, no residual, no DRL.
    NearestNearestMv,
    /// `NEAR_NEARMV`: both sides take compound stack entry
    /// [`InterInfo::ref_mv_idx`]` + 1` (decode.rs `assign_compound_mv`'s own
    /// `ref_mv_idx + 1`), reached by a DRL walk that starts at index 1.
    NearNearMv,
    /// `NEAREST_NEWMV`: the first side takes the stack's nearest vector, the
    /// second codes a residual against stack entry 0.
    NearestNewMv,
    /// `NEW_NEARESTMV`: the first side codes a residual against stack entry 0,
    /// the second takes the stack's nearest vector.
    NewNearestMv,
    /// `GLOBAL_GLOBALMV`: both sides take the identity global model's zero
    /// vector.
    GlobalGlobalMv,
    /// `NEW_NEWMV`: both sides code a residual against the compound stack
    /// entry [`InterInfo::ref_mv_idx`] names.
    NewNewMv,
}

impl InterMode {
    /// The spec's `INTER_COMPOUND_MODES` index (libaom `enums.h` order,
    /// `NEAREST_NEARESTMV = 0`..`NEW_NEWMV = 7`) for a compound mode, `None`
    /// for a single-reference one.
    pub(super) fn compound_index(self) -> Option<usize> {
        match self {
            InterMode::NearestNearestMv => Some(0),
            InterMode::NearNearMv => Some(1),
            InterMode::NearestNewMv => Some(2),
            InterMode::NewNearestMv => Some(3),
            InterMode::GlobalGlobalMv => Some(6),
            InterMode::NewNewMv => Some(7),
            _ => None,
        }
    }
}

/// An inter-coded block's mode and motion vector, `mv` in the spec's 1/8-pel
/// `(row, col)` units. For [`InterMode::NearestMv`] this writer ignores `mv`
/// and codes the stack's own candidate instead — the decoder derives it, so
/// nothing here can disagree with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterInfo {
    /// The mode this block is coded with.
    pub mode: InterMode,
    /// The motion vector a [`InterMode::NewMv`] block's residual targets.
    pub mv: (i32, i32),
    /// The reference this block predicts from (`LAST_FRAME`..=`ALTREF_FRAME`,
    /// spec 6.10.24's alphabet), written by [`write_single_ref`].
    pub ref_frame: i8,
    /// The block's `RefMvIdx` (spec 5.11.24 `read_drl_idx`): which
    /// `MvStack::entries` slot `NEWMV` codes its residual against, or
    /// `NEARMV` takes outright. Ignored by the two modes that code no DRL
    /// index (`NEARESTMV`, `GLOBALMV`).
    pub ref_mv_idx: u8,
    /// The block's second reference (spec `RefFrames[1]`), `None` for a
    /// single-reference block. `Some` only on a `reference_select` frame, and
    /// only beside one of [`InterMode`]'s compound modes.
    pub ref1: Option<i8>,
    /// The motion vector into `ref1`, for the compound modes that code one
    /// (`NEW_NEWMV`); the other two derive both sides.
    pub mv1: (i32, i32),
}

impl From<Vec<Coeff>> for BlockCoeffs {
    /// A block that codes luma only.
    fn from(luma: Vec<Coeff>) -> Self {
        Self {
            luma,
            ..Self::default()
        }
    }
}

/// Side of the luma transform the coefficient writer codes.
pub(super) const TX32: usize = 32;
/// Side of the larger of the two blocks a superblock splits into, in samples.
pub(super) const BLOCK: usize = 32;
/// Side of the chroma transform beside it at 4:2:0, and of the luma transform
/// of a 16x16 block.
pub(super) const TX16: usize = 16;
/// Side of the chroma transform of a 16x16 block at 4:2:0, and of the luma
/// transform of an 8x8 leaf under a straddling 16x16 (lane-av1-rect).
pub(super) const TX8: usize = 8;
/// Side of the chroma transform of an 8x8 leaf at 4:2:0 (lane-av1-rect).
pub(super) const TX4: usize = 4;
/// Side of a superblock, in samples, which is the size a block outside the
/// tile reads as.
pub(super) const SB: usize = 64;

/// The `above_side`/`left_side` value a cell with no coded neighbour holds
/// (tile start, superblock-row start) -- the writer's copy of
/// `crate::decode::NO_NEIGHBOUR_SIDE`, and 128 for the same reason: at 64 it
/// reads identically at every level up to 64x64 but sets the bit at the
/// `BLOCK_128X128` root (64 * 2 <= 128), putting that root's partition symbol
/// on CDF row 3 where libaom uses row 0.
pub(super) const NO_NEIGHBOUR_SIDE: usize = 128;
/// Side of the smallest block the writer codes, in samples, which is the grid
/// the neighbour bookkeeping is kept on.
pub(super) const SUB: usize = 16;
/// Side of a 4x4 mode-info unit, in samples: the granularity libaom's above
/// and left entropy-context arrays are actually kept on (spec
/// `get_txb_ctx`/`av1_set_entropy_contexts`), finer than [`SUB`].
pub(super) const MI: usize = 4;

/// What one coded block leaves behind for the blocks that read it as a
/// neighbour: whether it coded anything at all, and the sign of its DC.
#[derive(Clone, Copy, Default)]
pub(super) struct Neighbour {
    /// Whether the plane's transform block carried a coefficient.
    pub(super) coded: bool,
    /// Its cumulative coefficient level (spec `cul_level`, clamped to 7) --
    /// what the *luma* `txb_skip_ctx` of a transform unit smaller than its
    /// own block reads (`get_txb_ctx_general`, decode.rs `luma_skip_ctx`),
    /// where "coded or not" is not enough.
    pub(super) level: u8,
    /// The sign of its DC, absent when the DC itself is zero.
    pub(super) dc: Option<bool>,
}

/// Rejects a frame with no mode-info grid at all.
///
/// `mi_cols`/`mi_rows` are the frame's *true* (unpadded) size in 4x4 units
/// (spec `compute_image_size`), not necessarily a multiple of the 32x32 block
/// grid: a block whose origin sits at or past this bound is not coded (spec's
/// `decode_partition` never visits it), and one that straddles the bound is
/// coded whole, its samples coming from the padded planes.
pub(super) fn check_blocks(mi_cols: u32, mi_rows: u32) -> Result<()> {
    if mi_cols == 0 || mi_rows == 0 {
        return Err(Error::unsupported(
            "AV1 tile",
            "a coefficient key frame needs a nonzero mode-info grid",
        ));
    }
    Ok(())
}

/// The coefficients of one superblock: either one 64x64 block covering it, or
/// the four 32x32 blocks it is split into.
///
/// A superblock at the right-hand or bottom edge of the frame may be half
/// outside it, and such a superblock cannot be left whole — the spec has no
/// partition that keeps a block outside the frame — so it must be
/// [`Split`](Superblock::Split), and carries only the quadrants that are
/// inside, in raster order among themselves.
// Whole is the common case; boxing it would add an allocation per superblock on the decode path.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Superblock {
    /// One 64x64 block, whose luma transform is 64x64 — of which only the
    /// top-left 32x32 carries coefficients — and whose chroma transforms are
    /// 32x32 each at 4:2:0.
    Whole(BlockCoeffs),
    /// The 32x32 quadrants the superblock is split into, each either one
    /// block or four 16x16 blocks of its own.
    Split(Vec<Quadrant>),
}

/// One 32x32 quadrant of a split superblock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Quadrant {
    /// One 32x32 block, with a 32x32 luma transform and a 16x16 transform per
    /// chroma plane.
    Whole(BlockCoeffs),
    /// The four 16x16 blocks it is split into, in raster order, each with a
    /// 16x16 luma transform and an 8x8 transform per chroma plane.
    Split(Vec<BlockCoeffs>),
    /// lane-b64: not a quadrant at all -- the whole 64x64 superblock this
    /// quadrant is the TOP-LEFT of, coded as one block (`PARTITION_NONE` at
    /// `BLOCK_64X64`). Only the top-left quadrant of a superblock carries it
    /// and the other three carry [`Covered`](Quadrant::Covered), so the
    /// per-32x32-entry shape the inter writer takes is unchanged.
    ///
    /// The block is single-reference; the writer refuses a compound one. It
    /// may be skipped or carry a real residual -- one TX_64X64 luma transform
    /// whose coded quarter is a 32x32 level grid, and one TX_32X32 per chroma
    /// plane (lane-tx64, `crate::encode::b64_residual`).
    Whole64(BlockCoeffs),
    /// lane-b128: the whole 128x128 SUPERBLOCK ROOT this quadrant is the
    /// top-left of, coded as one block (`PARTITION_NONE` at the root's
    /// `partition_w128` symbol). Carried by the top-left 32x32 entry of the
    /// root's sixteen; the other fifteen carry [`Covered`](Quadrant::Covered).
    ///
    /// SKIP only: a 128x128 block's residual is four TX_64X64 luma units
    /// interleaved with its chroma per 64x64 mu chunk, which this writer has
    /// no path for; [`crate::tile::write_inter_block_128`] refuses one.
    Whole128(BlockCoeffs),
    /// lane-b128hv: the two 128x64 (`true`) or 64x128 (`false`) inter blocks a
    /// `PARTITION_HORZ` / `PARTITION_VERT` 128 superblock root carries, in the
    /// order the decoder reads them (top then bottom, left then right).
    /// Carried by the top-left 32x32 entry of the root's sixteen, exactly as
    /// [`Whole128`](Quadrant::Whole128) is.
    ///
    /// Skip-only by default; a piece carries a real residual under
    /// `EC_AV1_RECTRES` (`crate::encode::rectres`), single reference or
    /// compound either way.
    Rect128(bool, Vec<BlockCoeffs>),
    /// lane-ab128: the THREE inter blocks an AB shape at a 128 superblock
    /// root carries -- two 64x64 pieces and one 128x64 / 64x128 half -- in
    /// the order the decoder reads them
    /// ([`crate::encode::ab128_pieces`]). The `u8` is the partition symbol
    /// itself (4 `PARTITION_HORZ_A` .. 7 `PARTITION_VERT_B`). Carried by the
    /// top-left 32x32 entry of the root's sixteen, exactly as
    /// [`Rect128`](Quadrant::Rect128) is.
    ///
    /// Skip-only by default, same writer; under `EC_AV1_RECTRES` any of the
    /// three pieces may carry a real residual.
    Ab128(u8, Vec<BlockCoeffs>),
    /// A quadrant a [`Whole64`](Quadrant::Whole64) at its superblock's
    /// top-left already coded; it carries no block of its own.
    Covered,
}

impl Quadrant {
    /// The blocks it carries, in the order they are coded.
    pub(crate) fn blocks(&self) -> &[BlockCoeffs] {
        match self {
            Quadrant::Whole(block) | Quadrant::Whole64(block) | Quadrant::Whole128(block) => {
                std::slice::from_ref(block)
            }
            Quadrant::Split(blocks) | Quadrant::Rect128(_, blocks) | Quadrant::Ab128(_, blocks) => {
                blocks.as_slice()
            }
            Quadrant::Covered => &[],
        }
    }
}

/// What the blocks above and to the left of the one being coded left behind,
/// kept on the 16x16 grid the smallest block sits on.
pub(super) struct Neighbours {
    /// This tile's own top-left mode-info origin (spec 5.11.1's `MiRowStart`/
    /// `MiColStart`): every "is there a neighbour above/left of me" answer is
    /// TILE-relative, not frame-relative -- the decoder's own
    /// `Neighbours::start_tile` sets the same pair, and a writer that asked
    /// `mi_row > 0` instead coded a tile's own first row against a neighbour
    /// the decoder does not have (class: new map ignores tile edge).
    pub(super) tile_row0_mi: usize,
    pub(super) tile_col0_mi: usize,
    /// Per plane, whether the neighbour coded anything and the sign of its DC.
    pub(super) above: Vec<[Neighbour; 3]>,
    /// The same, down the left edge.
    pub(super) left: Vec<[Neighbour; 3]>,
    /// The neighbour's luma intra mode, which picks the CDF the next block's
    /// mode is coded with.
    pub(super) above_mode: Vec<usize>,
    /// The same, down the left edge.
    pub(super) left_mode: Vec<usize>,
    /// The side of the neighbour block, in samples, which is what the
    /// partition symbol's context reads.
    pub(super) above_side: Vec<usize>,
    /// The same, down the left edge.
    pub(super) left_side: Vec<usize>,
    /// The same as `above_side`/`left_side`, but kept at the finer mi (4x4)
    /// granularity [`Self::above`]/[`Self::left`] are, rather than [`SUB`]:
    /// two 8x8 leaves of one straddling 16x16 block (lane-av1-rect) share a
    /// single [`SUB`]-grid cell, so a coarse array cannot tell the second
    /// leaf's partition symbol that the first one was coded at 8x8 (finer
    /// than the 16x16 it sits in) -- exactly what its `above`/`left` context
    /// needs to read (spec 9.3's `AbovePartitionContext`/`LeftPartitionContext`,
    /// which libaom keeps per mi unit for this reason).
    pub(super) above_side_mi: Vec<usize>,
    /// The same, down the left edge.
    pub(super) left_side_mi: Vec<usize>,
    /// Whether the neighbour carried no residual, for [`sb_coeff_inter_frame_tile`]'s
    /// skip context. Unused (and left at its default) by the key frame writers.
    /// Kept per 4x4 mode-info unit, like [`Self::above_side_mi`] and for the
    /// same reason: under a real PARTITION_SPLIT to 8x8 the four leaves of a
    /// 16x16 read (and leave) different contexts inside one [`SUB`] slot.
    pub(super) above_skip: Vec<bool>,
    /// The same, down the left edge.
    pub(super) left_skip: Vec<bool>,
    /// The side, in pixels, of the transform last written in this 4x4
    /// mode-info column -- the deblock `tx_grid` a key frame's
    /// `get_tx_size_context` reads (decode.rs `tx_size_context_txfm`/`tx_px_at`),
    /// which is what picks the CDF row of a `TxMode::Select` block's
    /// `tx_depth` symbol.
    pub(super) above_tx: Vec<u8>,
    /// The same, down the left edge (the transform's HEIGHT, `tx_h_px_at`;
    /// every transform this writer codes is square, so one value serves).
    pub(super) left_tx: Vec<u8>,
    /// libaom's `above_txfm_context`/`left_txfm_context` (`TXFM_CONTEXT`), per
    /// 4x4 mode-info unit: what an INTER frame's `txfm_split` and `tx_depth`
    /// symbols read their context from (decode.rs `txfm_partition_ctx_rect`/
    /// `tx_size_context_txfm_rect`). Initialised to the widest transform, not
    /// zero (decode.rs `TXFM_CTX_INIT`).
    pub(super) above_txfm: Vec<u8>,
    /// The same, down the left edge (transform HEIGHT).
    pub(super) left_txfm: Vec<u8>,
    /// The palette size each 4x4 mode-info column last left behind, and its
    /// base colours -- `av1_get_palette_mode_ctx`'s neighbour lookup and
    /// `av1_get_palette_cache`'s colour source, mirrored from the decoder's
    /// own `above_palette_size`/`above_palette_colors` bands (decode.rs
    /// `palette_ctx_and_cache_mi`). Every intra block of a screen-content
    /// frame writes them, `0` when it took no palette, so a stale size never
    /// survives into the next block's context.
    pub(super) above_palette_size: Vec<u8>,
    /// The same, down the left edge.
    pub(super) left_palette_size: Vec<u8>,
    /// What the block being written right now will leave in the two palette
    /// bands, published by [`Self::record_mi_planes`] -- the one point EVERY
    /// block of either frame type passes through, so an inter block (or an
    /// intra block that took no palette) clears the bands rather than letting
    /// an earlier palette block's size stand (the neighbour-band hazard
    /// lane-t900 r23 hit on the decoder side).
    pub(super) pending_palette: Option<(u8, [u16; 8])>,
    /// The colours beside [`Self::above_palette_size`].
    pub(super) above_palette_colors: Vec<[u16; 8]>,
    /// The chroma (U-channel) halves of the three bands above --
    /// `av1_get_palette_cache(xd, 1, cache)`'s source, mirrored from the
    /// decoder's own `above_palette_uv_*`/`left_palette_uv_*`.
    pub(super) above_palette_uv_size: Vec<u8>,
    pub(super) left_palette_uv_size: Vec<u8>,
    pub(super) above_palette_uv_colors: Vec<[u16; 8]>,
    pub(super) left_palette_uv_colors: Vec<[u16; 8]>,
    /// [`Self::pending_palette`] for the chroma palette.
    pub(super) pending_palette_uv: Option<(u8, [u16; 8])>,
    /// The same, down the left edge.
    pub(super) left_palette_colors: Vec<[u16; 8]>,
    /// Whether the neighbour was coded inter, for the `is_inter` context.
    /// Unused by the key frame writers.
    pub(super) above_inter: Vec<bool>,
    /// The same, down the left edge.
    pub(super) left_inter: Vec<bool>,
    /// The reference each neighbour named, `-1` for an intra or unavailable
    /// one -- what the decoder keeps in its own `Neighbours::above_ref`
    /// (decode.rs) and feeds `read_single_ref`'s context functions.
    pub(super) above_ref: Vec<i8>,
    /// The same, down the left edge.
    pub(super) left_ref: Vec<i8>,
    /// Each neighbour's SECOND reference, `None` for a single-reference or
    /// intra one -- decode.rs `Neighbours::above_ref1`, read by every compound
    /// context function (`reference_mode_ctx`, `comp_reference_type_ctx`, the
    /// `comp_ref`/`comp_bwdref` votes).
    pub(super) above_ref1: Vec<Option<i8>>,
    /// The same, down the left edge.
    pub(super) left_ref1: Vec<Option<i8>>,
    /// What decode.rs `Neighbours::above_comp_group_idx` holds: a compound
    /// neighbour's own `comp_group_idx`, or libaom's single-reference special
    /// case (`3` for an ALTREF block, `0` otherwise).
    pub(super) above_comp_group_idx: Vec<u8>,
    /// The same, down the left edge.
    pub(super) left_comp_group_idx: Vec<u8>,
    /// The same for `compound_idx` (`1` for a single-ref ALTREF block).
    pub(super) above_compound_idx: Vec<u8>,
    /// The same, down the left edge.
    pub(super) left_compound_idx: Vec<u8>,
    /// The frame's true (unpadded) width and height, in 4x4 mode-info units,
    /// which is what clamps [`Self::above`]/[`Self::left`] at the edge (spec
    /// `av1_set_entropy_contexts`): a block whose row or column run spills
    /// past this bound leaves its trailing 4x4 units at their default,
    /// mid-cell if need be.
    pub(super) mi_cols: usize,
    pub(super) mi_rows: usize,
}

/// What one plane's neighbours leave for a block, gathered across every cell
/// the block spans -- which is what the decoder's own derivation reads
/// (spec 5.11.39), and what a single cell only stands in for while every block
/// is the same size.
#[derive(Clone, Copy, Default)]
pub(super) struct Around {
    /// Whether any transform block above this one carried a coefficient.
    pub(super) above_coded: bool,
    /// The same, to the left.
    pub(super) left_coded: bool,
    /// The running vote of the neighbours' DC signs: negative DCs count down,
    /// positive ones up, and a neighbour with no DC does not count.
    pub(super) dc_vote: i32,
}

impl Neighbours {
    /// The state a tile starts from: a block outside it reads as `DC_PRED`,
    /// as having coded nothing, and as unsplit. `cols`/`rows` are in [`SUB`]
    /// units; `mi_cols`/`mi_rows` are the frame's true (unpadded) size in 4x4
    /// mode-info units.
    pub(super) fn new(cols: usize, rows: usize, mi_cols: usize, mi_rows: usize) -> Self {
        Self {
            tile_row0_mi: 0,
            tile_col0_mi: 0,
            above: vec![[Neighbour::default(); 3]; cols * (SUB / MI)],
            left: vec![[Neighbour::default(); 3]; rows * (SUB / MI)],
            above_mode: vec![DC_PRED; cols],
            left_mode: vec![DC_PRED; rows],
            above_side: vec![NO_NEIGHBOUR_SIDE; cols],
            left_side: vec![NO_NEIGHBOUR_SIDE; rows],
            above_side_mi: vec![NO_NEIGHBOUR_SIDE; cols * (SUB / MI)],
            left_side_mi: vec![NO_NEIGHBOUR_SIDE; rows * (SUB / MI)],
            above_skip: vec![false; cols * (SUB / MI)],
            left_skip: vec![false; rows * (SUB / MI)],
            above_inter: vec![false; cols * (SUB / MI)],
            left_inter: vec![false; rows * (SUB / MI)],
            above_ref: vec![-1; cols * (SUB / MI)],
            left_ref: vec![-1; rows * (SUB / MI)],
            above_ref1: vec![None; cols * (SUB / MI)],
            left_ref1: vec![None; rows * (SUB / MI)],
            above_comp_group_idx: vec![0; cols * (SUB / MI)],
            left_comp_group_idx: vec![0; rows * (SUB / MI)],
            above_compound_idx: vec![0; cols * (SUB / MI)],
            left_compound_idx: vec![0; rows * (SUB / MI)],
            above_tx: vec![0; cols * (SUB / MI)],
            left_tx: vec![0; rows * (SUB / MI)],
            above_txfm: vec![TXFM_CTX_INIT; cols * (SUB / MI)],
            left_txfm: vec![TXFM_CTX_INIT; rows * (SUB / MI)],
            above_palette_size: vec![0; cols * (SUB / MI)],
            left_palette_size: vec![0; rows * (SUB / MI)],
            pending_palette: None,
            above_palette_colors: vec![[0; 8]; cols * (SUB / MI)],
            above_palette_uv_size: vec![0; cols * (SUB / MI)],
            left_palette_uv_size: vec![0; rows * (SUB / MI)],
            above_palette_uv_colors: vec![[0; 8]; cols * (SUB / MI)],
            left_palette_uv_colors: vec![[0; 8]; rows * (SUB / MI)],
            pending_palette_uv: None,
            left_palette_colors: vec![[0; 8]; rows * (SUB / MI)],
            mi_cols,
            mi_rows,
        }
    }

    /// Clears the left edge, which a tile starts every superblock row with.
    /// Points this writer's availability answers at one tile
    /// ([`Self::tile_row0_mi`]).
    pub(super) fn set_tile_origin(&mut self, row0_mi: usize, col0_mi: usize) {
        self.tile_row0_mi = row0_mi;
        self.tile_col0_mi = col0_mi;
    }

    /// Whether the block at mode-info row `mi_r` has a neighbour above it
    /// INSIDE its own tile, and the same to the left.
    pub(super) fn has_above(&self, mi_r: usize) -> bool {
        mi_r > self.tile_row0_mi
    }

    pub(super) fn has_left(&self, mi_c: usize) -> bool {
        mi_c > self.tile_col0_mi
    }

    pub(super) fn start_row(&mut self) {
        self.left.iter_mut().for_each(|l| *l = Default::default());
        self.left_mode.iter_mut().for_each(|m| *m = DC_PRED);
        self.left_side
            .iter_mut()
            .for_each(|s| *s = NO_NEIGHBOUR_SIDE);
        self.left_side_mi
            .iter_mut()
            .for_each(|s| *s = NO_NEIGHBOUR_SIDE);
        self.left_skip.iter_mut().for_each(|s| *s = false);
        self.left_inter.iter_mut().for_each(|i| *i = false);
        self.left_ref.iter_mut().for_each(|r| *r = -1);
        self.left_ref1.iter_mut().for_each(|r| *r = None);
        self.left_comp_group_idx.iter_mut().for_each(|r| *r = 0);
        self.left_compound_idx.iter_mut().for_each(|r| *r = 0);
        self.left_tx.iter_mut().for_each(|t| *t = 0);
        self.left_txfm.iter_mut().for_each(|t| *t = TXFM_CTX_INIT);
        self.left_palette_size.iter_mut().for_each(|s| *s = 0);
        self.left_palette_uv_size.iter_mut().for_each(|s| *s = 0);
        self.left_palette_uv_colors
            .iter_mut()
            .for_each(|c| *c = [0u16; 8]);
    }

    /// `av1_get_palette_mode_ctx` + `av1_get_palette_cache`, mirrored from
    /// the decoder's [`crate::decode`] `palette_ctx_and_cache_mi`: the
    /// `palette_y_mode` context (how many of the two neighbours took a
    /// palette) and the ascending, deduplicated colour cache the neighbours
    /// offer. `mi` is the block's own position in 4x4 units; this writer
    /// codes one tile starting at (0, 0), which is what the `r > 0`/`c > 0`
    /// availability terms read against.
    pub(super) fn palette_ctx_and_cache(&self, (r, c): (usize, usize)) -> (usize, Vec<u16>) {
        // `av1_get_palette_cache` drops the above neighbour on a 64-px
        // superblock boundary (pred_common.c:76) -- but the mode CONTEXT does
        // not, which is why the two terms below are asked separately.
        let above_ok = r % 16 != 0;
        let above_is_palette = r > 0 && self.above_palette_size[c] > 0;
        let (above_n, above_colors) = if above_ok && self.above_palette_size[c] > 0 {
            (
                usize::from(self.above_palette_size[c]),
                self.above_palette_colors[c],
            )
        } else {
            (0, [0u16; 8])
        };
        let (left_n, left_colors) = if c > 0 && self.left_palette_size[r] > 0 {
            (
                usize::from(self.left_palette_size[r]),
                self.left_palette_colors[r],
            )
        } else {
            (0, [0u16; 8])
        };
        let ctx = usize::from(above_is_palette) + usize::from(left_n > 0);
        let mut cache = Vec::with_capacity(16);
        let push_dedup = |cache: &mut Vec<u16>, v: u16| {
            if cache.last() != Some(&v) {
                cache.push(v);
            }
        };
        let (mut ai, mut li) = (0usize, 0usize);
        while ai < above_n && li < left_n {
            let (va, vl) = (above_colors[ai], left_colors[li]);
            if vl < va {
                push_dedup(&mut cache, vl);
                li += 1;
            } else {
                push_dedup(&mut cache, va);
                ai += 1;
                if vl == va {
                    li += 1;
                }
            }
        }
        while ai < above_n {
            push_dedup(&mut cache, above_colors[ai]);
            ai += 1;
        }
        while li < left_n {
            push_dedup(&mut cache, left_colors[li]);
            li += 1;
        }
        (ctx, cache)
    }

    /// [`Self::palette_ctx_and_cache`]'s cache half for the U channel
    /// (`av1_get_palette_cache(xd, 1, cache)`), mirrored from the decoder's
    /// `palette_uv_cache_mi`: the `palette_uv_mode` context needs no
    /// neighbour lookup at all (it is this block's own just-decided Y palette
    /// use), so only the cache is wanted here.
    pub(super) fn palette_uv_cache(&self, (r, c): (usize, usize)) -> Vec<u16> {
        let above_ok = r % 16 != 0;
        let (above_n, above_colors) = if above_ok && self.above_palette_uv_size[c] > 0 {
            (
                usize::from(self.above_palette_uv_size[c]),
                self.above_palette_uv_colors[c],
            )
        } else {
            (0, [0u16; 8])
        };
        let (left_n, left_colors) = if c > 0 && self.left_palette_uv_size[r] > 0 {
            (
                usize::from(self.left_palette_uv_size[r]),
                self.left_palette_uv_colors[r],
            )
        } else {
            (0, [0u16; 8])
        };
        let mut cache = Vec::with_capacity(16);
        let push_dedup = |cache: &mut Vec<u16>, v: u16| {
            if cache.last() != Some(&v) {
                cache.push(v);
            }
        };
        let (mut ai, mut li) = (0usize, 0usize);
        while ai < above_n && li < left_n {
            let (va, vl) = (above_colors[ai], left_colors[li]);
            if vl < va {
                push_dedup(&mut cache, vl);
                li += 1;
            } else {
                push_dedup(&mut cache, va);
                ai += 1;
                if vl == va {
                    li += 1;
                }
            }
        }
        while ai < above_n {
            push_dedup(&mut cache, above_colors[ai]);
            ai += 1;
        }
        while li < left_n {
            push_dedup(&mut cache, left_colors[li]);
            li += 1;
        }
        cache
    }

    /// [`Self::record_palette_y`] for the chroma bands (decode.rs
    /// `record_palette_uv_rect`) -- `size == 0` clears stale state the same
    /// way, and the span is in LUMA pixels for both planes' bands alike.
    pub(super) fn record_palette_uv(
        &mut self,
        (r, c): (usize, usize),
        (w, h): (usize, usize),
        size: u8,
        colors: [u16; 8],
    ) {
        // lane-b128hv: the above band spans the block's WIDTH and the left
        // band its HEIGHT; identical to the one loop this replaces at w == h.
        for cell in 0..(w / MI).max(1) {
            if let Some(e) = self.above_palette_uv_size.get_mut(c + cell) {
                *e = size;
                if size > 0 {
                    self.above_palette_uv_colors[c + cell] = colors;
                }
            }
        }
        for cell in 0..(h / MI).max(1) {
            if let Some(e) = self.left_palette_uv_size.get_mut(r + cell) {
                *e = size;
                if size > 0 {
                    self.left_palette_uv_colors[r + cell] = colors;
                }
            }
        }
    }

    /// What a just-written block leaves in the palette bands (decode.rs
    /// `record_palette_y_rect`): its size and colours over every 4x4 unit it
    /// spans, `0` for a block that took no palette.
    pub(super) fn record_palette_y(
        &mut self,
        (r, c): (usize, usize),
        (w, h): (usize, usize),
        size: u8,
        colors: [u16; 8],
    ) {
        // lane-b128hv: the above band spans the block's WIDTH and the left
        // band its HEIGHT; identical to the one loop this replaces at w == h.
        for cell in 0..(w / MI).max(1) {
            if let Some(e) = self.above_palette_size.get_mut(c + cell) {
                *e = size;
                if size > 0 {
                    self.above_palette_colors[c + cell] = colors;
                }
            }
        }
        for cell in 0..(h / MI).max(1) {
            if let Some(e) = self.left_palette_size.get_mut(r + cell) {
                *e = size;
                if size > 0 {
                    self.left_palette_colors[r + cell] = colors;
                }
            }
        }
    }

    /// `txfm_partition_update`/`set_txfm_ctxs` (decode.rs): what every block
    /// of an INTER frame leaves in the two `TXFM_CONTEXT` bands -- its
    /// resolved transform size over the span `(w_px, h_px)`, which for a
    /// SKIPPED inter block is its own block size instead.
    pub(super) fn record_txfm(
        &mut self,
        (mi_r, mi_c): (usize, usize),
        tx_px: usize,
        w_px: usize,
        h_px: usize,
    ) {
        for i in 0..h_px / MI {
            if let Some(cell) = self.left_txfm.get_mut(mi_r + i) {
                *cell = tx_px as u8;
            }
        }
        for i in 0..w_px / MI {
            if let Some(cell) = self.above_txfm.get_mut(mi_c + i) {
                *cell = tx_px as u8;
            }
        }
    }

    /// lane-b128hv: decode.rs `txfm_partition_update_rect` -- the two bands
    /// take the transform's own HEIGHT (left) and WIDTH (above), over the
    /// txb's own rect span. [`Self::record_txfm`] is this at `tx_w == tx_h`.
    pub(super) fn record_txfm_rect(
        &mut self,
        (mi_r, mi_c): (usize, usize),
        (tx_w, tx_h): (usize, usize),
        (txb_w, txb_h): (usize, usize),
    ) {
        for i in 0..txb_h / MI {
            if let Some(cell) = self.left_txfm.get_mut(mi_r + i) {
                *cell = tx_h as u8;
            }
        }
        for i in 0..txb_w / MI {
            if let Some(cell) = self.above_txfm.get_mut(mi_c + i) {
                *cell = tx_w as u8;
            }
        }
    }

    /// `get_tx_size_context` as libaom writes it for a block inside an INTER
    /// frame (decode.rs `tx_size_context_txfm_rect`): the `TXFM_CONTEXT`
    /// bands, except that an *inter* neighbour contributes its own BLOCK size.
    ///
    /// lane-av1tx2 r1: a neighbour OUTSIDE the tile contributes nothing at all
    /// -- the decoder's `tx_size_context_txfm` drops the whole term when
    /// `has_above`/`has_left` is false, where this counted the band's
    /// [`TXFM_CTX_INIT`] (64, the widest transform) as a real neighbour. Every
    /// intra block on a tile's top row or left column then took its `tx_depth`
    /// off a CDF row one higher than the decoder's, which is the first
    /// divergence of the inter `TxMode::Select` stream.
    pub(super) fn tx_size_ctx_txfm(&self, (mi_r, mi_c): (usize, usize), own_side: usize) -> usize {
        let (has_above, has_left) = (self.has_above(mi_r), self.has_left(mi_c));
        let mut above = usize::from(self.above_txfm[mi_c]) >= own_side;
        let mut left = usize::from(self.left_txfm[mi_r]) >= own_side;
        if has_above && self.above_inter[mi_c] {
            above = self.above_side_mi[mi_c] >= own_side;
        }
        if has_left && self.left_inter[mi_r] {
            left = self.left_side_mi[mi_r] >= own_side;
        }
        usize::from(has_above && above) + usize::from(has_left && left)
    }

    /// `get_tx_size_context` (decode.rs [`crate::decode::tx_size_context_txfm`]):
    /// whether the transform above is at least as wide as this block's own
    /// largest transform, plus whether the one to the left is at least as
    /// tall. A neighbour outside the tile contributes nothing.
    pub(super) fn tx_size_ctx(&self, (mi_r, mi_c): (usize, usize), max_tx: usize) -> usize {
        usize::from(self.has_above(mi_r) && usize::from(self.above_tx[mi_c]) >= max_tx)
            + usize::from(self.has_left(mi_c) && usize::from(self.left_tx[mi_r]) >= max_tx)
    }

    /// Publishes one block's resolved transform side over every 4x4 unit it
    /// covers, the way the decoder's `fill_lf_grid` does at the end of every
    /// block.
    pub(super) fn record_tx(&mut self, (mi_r, mi_c): (usize, usize), side: usize, tx_px: usize) {
        for cell in 0..side / MI {
            if mi_r + cell < self.left_tx.len() {
                self.left_tx[mi_r + cell] = tx_px as u8;
            }
            if mi_c + cell < self.above_tx.len() {
                self.above_tx[mi_c + cell] = tx_px as u8;
            }
        }
    }

    /// The luma `txb_skip_ctx` of a transform unit smaller than its own block
    /// (decode.rs `Neighbours::luma_skip_ctx`): the above and left magnitude
    /// tiers, each ORed over the unit's own span and clamped to 4.
    pub(super) fn luma_skip_ctx(&self, (mi_r, mi_c): (usize, usize), side_mi: usize) -> usize {
        let mut top = 0u8;
        let mut left = 0u8;
        for cell in 0..side_mi {
            top |= self.above[mi_c + cell][0].level;
            left |= self.left[mi_r + cell][0].level;
        }
        crate::decode::SKIP_CONTEXTS[usize::from(top).min(4)][usize::from(left).min(4)]
    }

    /// Publishes ONE luma transform unit's coefficient context, the way the
    /// decoder's `record_mi_luma` does between the units of a split block.
    pub(super) fn record_mi_luma(&mut self, (mi_r, mi_c): (usize, usize), tx_px: usize, grid: &[i32]) {
        let state = neighbour_state(grid);
        for cell in 0..tx_px / MI {
            if mi_r + cell < self.mi_rows {
                self.left[mi_r + cell][0] = state;
            }
            if mi_c + cell < self.mi_cols {
                self.above[mi_c + cell][0] = state;
            }
        }
    }

    /// lane-b128r: publishes ONE chroma transform unit's coefficient context,
    /// the decoder's own `record_mi_chroma` -- a 128x128 block's chroma is one
    /// TX_32X32 unit per plane per 64x64 mu chunk, and the next chunk reads
    /// the previous one's state (its `txb_skip_ctx` base 1 from the left
    /// neighbour). `span_px` is the unit's LUMA span, which is what the
    /// luma-mi-indexed bands are addressed in.
    pub(super) fn record_mi_chroma(
        &mut self,
        (mi_r, mi_c): (usize, usize),
        span_px: usize,
        plane: usize,
        grid: &[i32],
    ) {
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let (bound_h, bound_w) = (round_up_even(self.mi_rows), round_up_even(self.mi_cols));
        let state = neighbour_state(grid);
        for cell in 0..(span_px / MI).min(bound_h.saturating_sub(mi_r)) {
            self.left[mi_r + cell][plane] = state;
        }
        for cell in 0..(span_px / MI).min(bound_w.saturating_sub(mi_c)) {
            self.above[mi_c + cell][plane] = state;
        }
    }

    /// Writes one coded block into every 16x16 column and row it covers, and,
    /// on the finer 4x4 grid libaom's entropy context arrays are actually
    /// kept on, into every unit up to the true frame edge -- the units past
    /// it are left at their default (uncoded), even mid-16x16-cell (spec
    /// `av1_set_entropy_contexts`, which clamps to `blocks_wide`/`blocks_high`
    /// derived from the true `mi_cols`/`mi_rows`, not from this block's own
    /// side).
    pub(super) fn record(&mut self, at: (usize, usize), side: usize, mode: usize, grids: &[Vec<i32>; 3]) {
        self.record_planes(at, side, mode, grids, true);
    }

    /// [`Self::record`] with a switch for plane 0: a block whose luma was
    /// written as several transform units published each unit's own context
    /// as it went ([`Self::record_mi_luma`]), so the whole-block write here
    /// would clobber what the next block reads (decode.rs
    /// `record_split_luma`).
    pub(super) fn record_planes(
        &mut self,
        at: (usize, usize),
        side: usize,
        mode: usize,
        grids: &[Vec<i32>; 3],
        luma: bool,
    ) {
        self.record_planes_rect(at, (side, side), mode, grids, luma);
    }

    /// lane-b128hv: [`Self::record_planes`] with independent above (width) /
    /// left (height) extents, decode.rs `record_rect_mi`'s own split -- the
    /// 128 root's HORZ/VERT halves are the writer's first rect blocks.
    pub(super) fn record_planes_rect(
        &mut self,
        at: (usize, usize),
        (w, h): (usize, usize),
        mode: usize,
        grids: &[Vec<i32>; 3],
        luma: bool,
    ) {
        let (r, c) = at;
        for cell in 0..w / SUB {
            self.above_mode[c + cell] = mode;
            self.above_side[c + cell] = w;
        }
        for cell in 0..h / SUB {
            self.left_mode[r + cell] = mode;
            self.left_side[r + cell] = h;
        }
        self.record_mi_planes_rect((r * (SUB / MI), c * (SUB / MI)), (w, h), grids, luma);
    }

    /// The coefficient-context half of [`Self::record`], taking the block's
    /// position directly in 4x4 mode-info units rather than [`SUB`]-grid
    /// (r, c): an 8x8 leaf under a straddling 16x16 (lane-av1-rect) sits at a
    /// mi offset [`Self::record`]'s SUB-unit `at` cannot name, but the
    /// `above`/`left` arrays it writes into are already sized to the full mi
    /// grid, so no resizing is needed to write into them at this
    /// finer-than-SUB granularity.
    pub(super) fn record_mi(&mut self, at_mi: (usize, usize), side: usize, grids: &[Vec<i32>; 3]) {
        self.record_mi_planes(at_mi, side, grids, true);
    }

    /// [`Self::record_mi`] with [`Self::record_planes`]'s plane-0 switch.
    pub(super) fn record_mi_planes(
        &mut self,
        at_mi: (usize, usize),
        side: usize,
        grids: &[Vec<i32>; 3],
        luma: bool,
    ) {
        self.record_mi_planes_rect(at_mi, (side, side), grids, luma);
    }

    /// [`Self::record_mi_planes`] with independent above (width) / left
    /// (height) extents (lane-b128hv, decode.rs `record_mi_rect`).
    pub(super) fn record_mi_planes_rect(
        &mut self,
        at_mi: (usize, usize),
        (w, h): (usize, usize),
        grids: &[Vec<i32>; 3],
        luma: bool,
    ) {
        let (mi_r, mi_c) = at_mi;
        if screen_armed() {
            let (size, colors) = self.pending_palette.take().unwrap_or((0, [0u16; 8]));
            self.record_palette_y(at_mi, (w, h), size, colors);
            let (uv_size, uv_colors) = self.pending_palette_uv.take().unwrap_or((0, [0u16; 8]));
            self.record_palette_uv(at_mi, (w, h), uv_size, uv_colors);
        }
        let states: [Neighbour; 3] = std::array::from_fn(|plane| neighbour_state(&grids[plane]));
        let (w_mi, h_mi) = (w / MI, h / MI);
        for cell in 0..h_mi {
            self.left_side_mi[mi_r + cell] = h;
        }
        for cell in 0..w_mi {
            self.above_side_mi[mi_c + cell] = w;
        }
        // libaom rounds the luma edge up to the plane's own 4x4 unit before
        // clamping a subsampled plane (`ROUND_POWER_OF_TWO(max_blocks_high,
        // subsampling_y)` in av1_write_intra_coeffs_mb, encodetxb.c:456-459):
        // a chroma 4x4 unit straddling the true luma edge is still whole in
        // chroma's own halved grid, so it stays valid one luma-mi row/col
        // past where luma's own edge falls when that edge is odd.
        let round_up_even = |n: usize| n.div_ceil(2) * 2;
        let bound_h = [
            self.mi_rows,
            round_up_even(self.mi_rows),
            round_up_even(self.mi_rows),
        ];
        let bound_w = [
            self.mi_cols,
            round_up_even(self.mi_cols),
            round_up_even(self.mi_cols),
        ];
        for cell in 0..h_mi {
            let keep_left = self.left[mi_r + cell][0];
            self.left[mi_r + cell] = std::array::from_fn(|plane| {
                if cell < h_mi.min(bound_h[plane].saturating_sub(mi_r)) {
                    states[plane]
                } else {
                    Default::default()
                }
            });
            if !luma {
                self.left[mi_r + cell][0] = keep_left;
            }
        }
        for cell in 0..w_mi {
            let keep_above = self.above[mi_c + cell][0];
            self.above[mi_c + cell] = std::array::from_fn(|plane| {
                if cell < w_mi.min(bound_w[plane].saturating_sub(mi_c)) {
                    states[plane]
                } else {
                    Default::default()
                }
            });
            if !luma {
                self.above[mi_c + cell][0] = keep_above;
            }
        }
    }

    /// Writes one inter-frame block's skip flag and inter/intra state into
    /// every 16x16 column and row it covers, the same span [`Self::record`]
    /// fills for the coefficient and mode state.
    pub(super) fn record_inter(
        &mut self,
        at: (usize, usize),
        side: usize,
        skip: bool,
        is_inter: bool,
        ref_frame: i8,
    ) {
        let (r, c) = at;
        self.record_inter_mi(
            (r * (SUB / MI), c * (SUB / MI)),
            side,
            skip,
            is_inter,
            ref_frame,
        );
    }

    /// lane-b128hv: [`Self::record_inter_mi`] with independent above (width)
    /// / left (height) spans, decode.rs `record_inter_rect_mi`'s own split.
    pub(super) fn record_inter_rect_mi(
        &mut self,
        (mi_r, mi_c): (usize, usize),
        (w, h): (usize, usize),
        skip: bool,
        is_inter: bool,
        ref_frame: i8,
    ) {
        let altref = is_inter && ref_frame == crate::mvstack::ALTREF_FRAME;
        for cell in 0..h / MI {
            self.left_skip[mi_r + cell] = skip;
            self.left_inter[mi_r + cell] = is_inter;
            self.left_ref[mi_r + cell] = ref_frame;
            self.left_ref1[mi_r + cell] = None;
            self.left_comp_group_idx[mi_r + cell] = if altref { 3 } else { 0 };
            self.left_compound_idx[mi_r + cell] = u8::from(altref);
        }
        for cell in 0..w / MI {
            self.above_skip[mi_c + cell] = skip;
            self.above_inter[mi_c + cell] = is_inter;
            self.above_ref[mi_c + cell] = ref_frame;
            self.above_ref1[mi_c + cell] = None;
            self.above_comp_group_idx[mi_c + cell] = if altref { 3 } else { 0 };
            self.above_compound_idx[mi_c + cell] = u8::from(altref);
        }
    }

    /// [`Self::record_compound_mi`] over a rect block's own two spans.
    pub(super) fn record_compound_rect_mi(
        &mut self,
        (mi_r, mi_c): (usize, usize),
        (w, h): (usize, usize),
        ref1: i8,
        comp_group_idx: u8,
        compound_idx: u8,
    ) {
        for cell in 0..h / MI {
            self.left_ref1[mi_r + cell] = Some(ref1);
            self.left_comp_group_idx[mi_r + cell] = comp_group_idx;
            self.left_compound_idx[mi_r + cell] = compound_idx;
        }
        for cell in 0..w / MI {
            self.above_ref1[mi_c + cell] = Some(ref1);
            self.above_comp_group_idx[mi_c + cell] = comp_group_idx;
            self.above_compound_idx[mi_c + cell] = compound_idx;
        }
    }

    /// [`Self::record_inter_mi`]'s compound half (decode.rs
    /// `Neighbours::record_compound_ctx_rect_mi`): overwrites the ALTREF
    /// single-reference default with a real compound block's own second
    /// reference and `comp_group_idx`/`compound_idx` bits, called right after
    /// it for every compound-coded block.
    pub(super) fn record_compound_mi(
        &mut self,
        (mi_r, mi_c): (usize, usize),
        side: usize,
        ref1: i8,
        comp_group_idx: u8,
        compound_idx: u8,
    ) {
        for cell in 0..side / MI {
            self.above_ref1[mi_c + cell] = Some(ref1);
            self.left_ref1[mi_r + cell] = Some(ref1);
            self.above_comp_group_idx[mi_c + cell] = comp_group_idx;
            self.left_comp_group_idx[mi_r + cell] = comp_group_idx;
            self.above_compound_idx[mi_c + cell] = compound_idx;
            self.left_compound_idx[mi_r + cell] = compound_idx;
        }
    }

    /// [`Self::record_inter`] taking the block's position directly in 4x4
    /// mode-info units, for a block finer than one [`SUB`] slot.
    pub(super) fn record_inter_mi(
        &mut self,
        (mi_r, mi_c): (usize, usize),
        side: usize,
        skip: bool,
        is_inter: bool,
        ref_frame: i8,
    ) {
        for cell in 0..side / MI {
            self.above_skip[mi_c + cell] = skip;
            self.left_skip[mi_r + cell] = skip;
            self.above_inter[mi_c + cell] = is_inter;
            self.left_inter[mi_r + cell] = is_inter;
            self.above_ref[mi_c + cell] = ref_frame;
            self.left_ref[mi_r + cell] = ref_frame;
            // decode.rs `record_inter`'s single-reference default: no second
            // reference, and libaom's ALTREF special case in the two compound
            // context bands. `record_compound_mi` overwrites both for a real
            // compound block.
            let altref = is_inter && ref_frame == crate::mvstack::ALTREF_FRAME;
            self.above_ref1[mi_c + cell] = None;
            self.left_ref1[mi_r + cell] = None;
            self.above_comp_group_idx[mi_c + cell] = if altref { 3 } else { 0 };
            self.left_comp_group_idx[mi_r + cell] = if altref { 3 } else { 0 };
            self.above_compound_idx[mi_c + cell] = u8::from(altref);
            self.left_compound_idx[mi_r + cell] = u8::from(altref);
        }
    }

    /// The above neighbour as decode.rs' own `NeighbourRef`, for every
    /// compound context function (`reference_mode_ctx`,
    /// `comp_reference_type_ctx`, the `comp_ref`/`comp_bwdref` votes).
    pub(super) fn above_nbr(&self, mi_c: usize) -> crate::mvstack::NeighbourRef {
        let is_inter = self.above_inter[mi_c];
        let ref0 = if is_inter { self.above_ref[mi_c] } else { 0 };
        let ref1 = self.above_ref1[mi_c];
        crate::mvstack::NeighbourRef {
            is_inter,
            ref0,
            ref1,
            uni: ref1.is_some_and(|r1| is_uni_comp_ref(ref0, r1)),
        }
    }

    /// [`Self::above_nbr`], down the left edge.
    pub(super) fn left_nbr(&self, mi_r: usize) -> crate::mvstack::NeighbourRef {
        let is_inter = self.left_inter[mi_r];
        let ref0 = if is_inter { self.left_ref[mi_r] } else { 0 };
        let ref1 = self.left_ref1[mi_r];
        crate::mvstack::NeighbourRef {
            is_inter,
            ref0,
            ref1,
            uni: ref1.is_some_and(|r1| is_uni_comp_ref(ref0, r1)),
        }
    }

    /// The context of a block's partition symbol (spec 9.3): whether the
    /// blocks above it and to its left were split finer than it is.
    /// The gathered state of the blocks above and to the left of one block,
    /// per plane.
    pub(super) fn around(&self, (r, c): (usize, usize), side: usize) -> [Around; 3] {
        self.around_mi((r * (SUB / MI), c * (SUB / MI)), side)
    }

    /// [`Self::around`] taking the block's position directly in 4x4 mode-info
    /// units, for the same reason [`Self::record_mi`] does.
    pub(super) fn around_mi(&self, (mi_r, mi_c): (usize, usize), side: usize) -> [Around; 3] {
        let side_mi = side / MI;
        std::array::from_fn(|plane| {
            let mut around = Around::default();
            for cell in 0..side_mi {
                let (above, left) = (
                    &self.above[mi_c + cell][plane],
                    &self.left[mi_r + cell][plane],
                );
                around.above_coded |= above.coded;
                around.left_coded |= left.coded;
                around.dc_vote += dc_vote(above.dc) + dc_vote(left.dc);
            }
            around
        })
    }

    pub(super) fn partition_ctx(&self, at: (usize, usize), side: usize) -> usize {
        // Delegates to the mi-precise reader (same pattern as `around` /
        // `around_mi`): `above_side`/`left_side` are only ever advanced in
        // whole-[`SUB`] steps by [`Self::record`], so a leaf8's `record_mi`
        // (lane-av1-rect) -- which only touches the finer mi arrays -- leaves
        // them stale for the *next* sibling's own partition symbol, reading a
        // 16x16-slot side that already split into 8x8s underneath it.
        let (r, c) = at;
        self.partition_ctx_mi((r * (SUB / MI), c * (SUB / MI)), side)
    }

    /// [`Self::partition_ctx`] at mi granularity, for an 8x8 leaf of a
    /// straddling 16x16 block: reads the finer `above_side_mi`/`left_side_mi`
    /// arrays so the second leaf sees the first leaf's own 8x8 side rather
    /// than the enclosing 16x16 slot's stale, shared state.
    pub(super) fn partition_ctx_mi(&self, (mi_r, mi_c): (usize, usize), side: usize) -> usize {
        2 * usize::from(self.left_side_mi[mi_r] * 2 <= side)
            + usize::from(self.above_side_mi[mi_c] * 2 <= side)
    }
}

/// Writes the payload of a one-tile key frame built from `superblocks`, each
/// either one 64x64 block or the 32x32 blocks it splits into.
///
/// `superblocks` gives one entry per superblock in raster order across the
/// frame. Every block is DC-predicted unless its [`BlockCoeffs::mode`] says
/// otherwise, and carries the coefficients its lists give; the coefficients
/// may sit anywhere in their transform, so a block is a picture rather than a
/// flat grey.
///
/// # Errors
/// Returns an error when the frame is not a whole number of 32x32 blocks, when
/// `superblocks` does not carry exactly one entry per superblock or an entry
/// does not carry one block per quadrant inside the frame, when a superblock
/// that is half outside the frame is left whole, when a block names an intra
/// mode a key frame does not code, when a coefficient sits outside its
/// transform, repeats a position, carries a zero level or one wider than the
/// Golomb tail reaches.
pub fn sb_coeff_key_frame_tile(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    superblocks: &[Superblock],
) -> Result<Vec<u8>> {
    sb_coeff_key_frame_tile_tx(mi_cols, mi_rows, base_q_idx, superblocks, false)
}

/// [`sb_coeff_key_frame_tile`] for a frame header carrying
/// `tx_mode == TxMode::Select` (`tx_select`): every block then codes a
/// `tx_depth` symbol and, where that depth is nonzero, its luma residual as
/// several transform units ([`write_luma_select`]). With `tx_select` false
/// this writes exactly the stream it always did.
///
/// # Errors
/// As [`sb_coeff_key_frame_tile`], plus a 64x64 block whose `tx_depth` is
/// nonzero (not written yet).
pub fn sb_coeff_key_frame_tile_tx(
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    superblocks: &[Superblock],
    tx_select: bool,
) -> Result<Vec<u8>> {
    let mut cdfs = Cdfs::new(q_ctx_of(base_q_idx));
    sb_coeff_key_frame_tile_cdfs(
        mi_cols,
        mi_rows,
        base_q_idx,
        superblocks,
        tx_select,
        &mut cdfs,
        TileRect::whole(mi_cols, mi_rows),
    )
}

/// [`sb_coeff_key_frame_tile_tx`] starting from -- and leaving behind -- the
/// caller's own CDF state, which is what a frame whose
/// `disable_frame_end_update_cdf` is off needs: the next frame's writer must
/// start where this tile ended (spec 7.20, mirrored by
/// `crate::stream::stored_cdfs_for`).
///
/// # Errors
/// As [`sb_coeff_key_frame_tile_tx`].
pub(crate) fn sb_coeff_key_frame_tile_cdfs(
    mi_cols: u32,
    mi_rows: u32,
    _base_q_idx: u8,
    superblocks: &[Superblock],
    tx_select: bool,
    cdfs: &mut Cdfs,
    // This tile's own span (spec 5.11.1): the writer walks only these
    // superblocks, and its neighbour bands start blank, so everything
    // outside reads as unavailable exactly as the decoder's own per-tile
    // reset makes it.
    tile: TileRect,
) -> Result<Vec<u8>> {
    // Consumes whatever `arm_cdef_idx`/`arm_lr` armed, on every exit path.
    let _cdef_idx = CdefIdxGuard;
    check_blocks(mi_cols, mi_rows)?;
    // The DV predictor's own mi grid, per tile exactly as decode.rs builds it
    // per frame (`decode_key_frame_tile_lr`'s `intrabc_mi_grid`): a tile that
    // allows no intrabc arms none and every `record_intrabc_mi` below is a
    // no-op.
    if intrabc_armed() {
        // No `set_tile_bounds`: decode.rs builds this grid for the WHOLE
        // frame with the default (whole-grid) bounds, so the writer must
        // too. `encode_key_frame_inner` only allows intrabc on a
        // single-tile frame, where the two grids are the same grid.
        let grid = crate::mvstack::MiGrid::new(mi_cols as usize, mi_rows as usize);
        INTRABC_GRID.with(|g| {
            *g.borrow_mut() = Some((grid, mi_cols as usize, mi_rows as usize));
        });
    }
    let (cols, rows) = block_grid(mi_cols, mi_rows);
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    if superblocks.len() != (sb_cols * sb_rows) as usize {
        return Err(Error::unsupported(
            "AV1 tile",
            "a coefficient key frame needs one entry per superblock",
        ));
    }
    let mut coded: Vec<&BlockCoeffs> = Vec::new();
    for superblock in superblocks {
        match superblock {
            Superblock::Whole(block) => coded.push(block),
            Superblock::Split(quadrants) => {
                for block in quadrants.iter().flat_map(Quadrant::blocks) {
                    match &block.eight {
                        Some(leaves) => coded.extend(leaves.iter()),
                        None => coded.push(block),
                    }
                }
            }
        }
    }
    if let Some(bad) = coded
        .into_iter()
        .find(|b| usize::from(b.mode) >= INTRA_MODES)
    {
        return Err(Error::unsupported(
            "AV1 tile",
            format!(
                "intra mode {} is not one of the thirteen a key frame codes",
                bad.mode
            ),
        ));
    }

    let sub_planes = [TxbSet::Luma16, TxbSet::Chroma8, TxbSet::Chroma8];
    let split_planes = [TxbSet::Luma32, TxbSet::Chroma16, TxbSet::Chroma16];
    let whole_planes = [TxbSet::Luma64, TxbSet::Chroma32, TxbSet::Chroma32];
    let scans = [
        default_scan(TX32),
        default_scan(TX16),
        default_scan(TX8),
        default_scan(TX4),
    ];
    // The blocks above and to the left, on the 16x16 grid. The left edge is
    // reset at every superblock row because a tile starts each row with no
    // left neighbour.
    let mut neighbours = Neighbours::new(
        cols as usize * 2,
        rows as usize * 2,
        mi_cols as usize,
        mi_rows as usize,
    );
    neighbours.set_tile_origin(tile.mi_row0 as usize, tile.mi_col0 as usize);

    // The tile adapts every non-literal CDF it writes, exactly as the decoder
    // adapts the ones it reads, so the frame header leaves `disable_cdf_update`
    // off.
    let mut enc = SymbolEncoder::new();
    let sb128 = sb128_armed();
    // lane-sb128: at a 128 superblock the four 64x64 cells of each root are
    // written in libaom's own TL/TR/BL/BR order, the root's restoration units
    // and partition symbol first; at a 64 one this is plain raster and every
    // cell is its own superblock, i.e. byte-identical to the loop before.
    for (sb_r, sb_c, row_start, sb_start) in sb_write_order(
        tile.sb_row0,
        tile.sb_row1.min(sb_rows),
        tile.sb_col0,
        tile.sb_col1.min(sb_cols),
        sb128,
    ) {
        if row_start {
            neighbours.start_row();
        }
        if sb128 && sb_start {
            write_sb128_root(
                &mut enc,
                cdfs,
                &neighbours,
                sb_r,
                sb_c,
                mi_cols,
                mi_rows,
                PARTITION_SPLIT,
            );
        }
        {
            if !sb128 {
                write_lr(&mut enc, cdfs, sb_r * SB_MI_W, sb_c * SB_MI_W, SB_MI_W);
            }
            let at = (sb_r as usize * 4, sb_c as usize * 4);
            let ctx = neighbours.partition_ctx(at, SB);
            ec_rng_trace(|| {
                format!(
                    "EC_PART mi_row={} mi_col={} bsize=12 ctx={} tell={}",
                    at.0 * 4,
                    at.1 * 4,
                    ctx,
                    enc.tell()
                )
            });
            // A superblock whose bottom or right half is outside the frame
            // cannot be left unsplit, so the decoder reads a flag instead of
            // the partition symbol — and reads nothing at all when both halves
            // are outside, where the split is the only partition left.
            let (has_cols, has_rows) = (
                sb_c * SB_MI + SB_MI / 2 < mi_cols,
                sb_r * SB_MI + SB_MI / 2 < mi_rows,
            );
            // The quadrants of this superblock that are inside the frame, as
            // positions in the 32x32 block grid.
            let quadrant_positions: Vec<(usize, usize)> = (0..4)
                .map(|q| (sb_r * 2 + q / 2, sb_c * 2 + q % 2))
                .filter(|&(r, c)| r < rows && c < cols)
                .map(|(r, c)| (r as usize, c as usize))
                .collect();

            match &superblocks[(sb_r * sb_cols + sb_c) as usize] {
                Superblock::Whole(block) => {
                    if !has_cols || !has_rows {
                        return Err(Error::unsupported(
                            "AV1 tile",
                            "a superblock that is half outside the frame cannot be left whole",
                        ));
                    }
                    enc.symbol(PARTITION_NONE, &mut cdfs.partition_w64[ctx]);
                    let grids = [
                        level_grid(&block.luma, TX32)?,
                        level_grid(&block.u, TX32)?,
                        level_grid(&block.v, TX32)?,
                    ];
                    write_block(
                        &mut enc,
                        cdfs,
                        &mut neighbours,
                        block,
                        at,
                        SB,
                        &whole_planes,
                        &grids,
                        [&scans[0], &scans[0], &scans[0]],
                        // A 64x64 block is too big to be offered chroma from
                        // luma, so its chroma mode reads the table without it.
                        false,
                        tx_select,
                        &scans,
                    )?;
                    ec_rng_trace(|| {
                        format!(
                            "EC_TOK mi_row={} mi_col={} tell={}",
                            at.0 * 4,
                            at.1 * 4,
                            enc.tell()
                        )
                    });
                }
                Superblock::Split(quadrants) => {
                    match (has_cols, has_rows) {
                        (true, true) => enc.symbol(PARTITION_SPLIT, &mut cdfs.partition_w64[ctx]),
                        // The gathered CDF an edge superblock reads is built
                        // for the read and thrown away: the decoder never
                        // stores it back, so nothing adapts here.
                        (true, false) => {
                            enc.symbol_fixed(1, &gather(&cdfs.partition_w64[ctx], VERT_ALIKE));
                        }
                        (false, true) => {
                            enc.symbol_fixed(1, &gather(&cdfs.partition_w64[ctx], HORZ_ALIKE));
                        }
                        (false, false) => {}
                    }
                    if quadrants.len() != quadrant_positions.len() {
                        return Err(Error::unsupported(
                            "AV1 tile",
                            "a split superblock needs one block per quadrant inside the frame",
                        ));
                    }
                    for (quadrant, (r, c)) in quadrants.iter().zip(quadrant_positions) {
                        let at = (r * 2, c * 2);
                        let ctx = neighbours.partition_ctx(at, BLOCK);
                        // Recomputed at this 32x32 block's own half (spec
                        // `decode_partition`, called again at every size, not
                        // just once for the superblock): the true frame edge
                        // can fall inside this quadrant even when the
                        // superblock it sits in was itself whole or safely
                        // split above.
                        let (has_cols32, has_rows32) = (
                            has_half(c as u32 * BLOCK_MI, BLOCK_MI, mi_cols),
                            has_half(r as u32 * BLOCK_MI, BLOCK_MI, mi_rows),
                        );
                        ec_rng_trace(|| {
                            format!(
                                "EC_PART mi_row={} mi_col={} bsize=9 ctx={} tell={}",
                                at.0 * 4,
                                at.1 * 4,
                                ctx,
                                enc.tell()
                            )
                        });
                        match quadrant {
                            // A 64x64 root is an INTER-frame partition; a
                            // key frame codes one as `Superblock::Whole`.
                            Quadrant::Whole64(_)
                            | Quadrant::Whole128(_)
                            | Quadrant::Rect128(..)
                            | Quadrant::Ab128(..)
                            | Quadrant::Covered => {
                                return Err(Error::unsupported(
                                    "AV1 tile",
                                    "a key frame codes a 64x64 block as `Superblock::Whole`",
                                ));
                            }
                            Quadrant::Whole(block) => {
                                if !has_cols32 || !has_rows32 {
                                    return Err(Error::unsupported(
                                        "AV1 tile",
                                        "a 32x32 block that is half outside the true frame \
                                         cannot be left whole",
                                    ));
                                }
                                enc.symbol(PARTITION_NONE, &mut cdfs.partition_w32[ctx]);
                                let grids = [
                                    level_grid(&block.luma, TX32)?,
                                    level_grid(&block.u, TX16)?,
                                    level_grid(&block.v, TX16)?,
                                ];
                                write_block(
                                    &mut enc,
                                    cdfs,
                                    &mut neighbours,
                                    block,
                                    at,
                                    BLOCK,
                                    &split_planes,
                                    &grids,
                                    [&scans[0], &scans[1], &scans[1]],
                                    true,
                                    tx_select,
                                    &scans,
                                )?;
                                ec_rng_trace(|| {
                                    format!(
                                        "EC_TOK mi_row={} mi_col={} tell={}",
                                        at.0 * 4,
                                        at.1 * 4,
                                        enc.tell()
                                    )
                                });
                            }
                            Quadrant::Split(blocks) => {
                                // The 16x16 sub-blocks this quadrant's split
                                // carries: only those whose own mi origin is
                                // inside the true frame (spec `decode_partition`'s
                                // `r >= MiRows || c >= MiCols` early return),
                                // which need not be all four when the true
                                // edge falls inside this quadrant.
                                let sub_positions: Vec<(usize, usize)> = (0..4)
                                    .map(|i| (r * 2 + i / 2, c * 2 + i % 2))
                                    .filter(|&(sr, sc)| {
                                        (sr as u32) * SUB_MI < mi_rows
                                            && (sc as u32) * SUB_MI < mi_cols
                                    })
                                    .collect();
                                if blocks.len() != sub_positions.len() {
                                    return Err(Error::unsupported(
                                        "AV1 tile",
                                        "a split 32x32 block needs one 16x16 entry per \
                                         sub-block inside the true frame",
                                    ));
                                }
                                // Same three-way spec signaling as the
                                // superblock level above, recomputed at this
                                // block's own half: a full alphabet symbol
                                // only when both halves are inside, a single
                                // gathered bit when just one is, and nothing
                                // at all (SPLIT is inferred) when neither is.
                                match (has_cols32, has_rows32) {
                                    (true, true) => {
                                        enc.symbol(PARTITION_SPLIT, &mut cdfs.partition_w32[ctx]);
                                    }
                                    (true, false) => {
                                        enc.symbol_fixed(
                                            1,
                                            &gather(&cdfs.partition_w32[ctx], VERT_ALIKE),
                                        );
                                    }
                                    (false, true) => {
                                        enc.symbol_fixed(
                                            1,
                                            &gather(&cdfs.partition_w32[ctx], HORZ_ALIKE),
                                        );
                                    }
                                    (false, false) => {}
                                }
                                for (block, (sr, sc)) in blocks.iter().zip(sub_positions) {
                                    // A 16x16 leaf's own hasRows/hasCols,
                                    // recomputed at this leaf's own half
                                    // (same three-way signaling as the 32x32
                                    // and 64x64 levels above).
                                    let (has_cols16, has_rows16) = (
                                        has_half(sc as u32 * SUB_MI, SUB_MI, mi_cols),
                                        has_half(sr as u32 * SUB_MI, SUB_MI, mi_rows),
                                    );
                                    let at = (sr, sc);
                                    let ctx = neighbours.partition_ctx(at, SUB);
                                    ec_rng_trace(|| {
                                        format!(
                                            "EC_PART mi_row={} mi_col={} bsize=6 ctx={} tell={}",
                                            at.0 * 4,
                                            at.1 * 4,
                                            ctx,
                                            enc.tell()
                                        )
                                    });
                                    if has_cols16 && has_rows16 {
                                        enc.symbol(PARTITION_NONE, &mut cdfs.partition_w16[ctx]);
                                        let grids = [
                                            level_grid(&block.luma, TX16)?,
                                            level_grid(&block.u, TX8)?,
                                            level_grid(&block.v, TX8)?,
                                        ];
                                        write_block(
                                            &mut enc,
                                            cdfs,
                                            &mut neighbours,
                                            block,
                                            at,
                                            SUB,
                                            &sub_planes,
                                            &grids,
                                            [&scans[1], &scans[2], &scans[2]],
                                            true,
                                            tx_select,
                                            &scans,
                                        )?;
                                        ec_rng_trace(|| {
                                            format!(
                                                "EC_TOK mi_row={} mi_col={} tell={}",
                                                at.0 * 4,
                                                at.1 * 4,
                                                enc.tell()
                                            )
                                        });
                                        continue;
                                    }
                                    // The true edge falls inside this 16x16
                                    // leaf itself: the block splits into the
                                    // 8x8s that are inside, each its own
                                    // leaf. Same three-way spec signaling as
                                    // every level above -- a single gathered
                                    // bit when one half is outside, and
                                    // nothing at all when BOTH are, where
                                    // `decode_partition` infers the split
                                    // (lane-av1rect: no rectangular
                                    // transform is needed for this, the
                                    // earlier refusal here misread 5.11.4).
                                    match (has_cols16, has_rows16) {
                                        (true, false) => enc.symbol_fixed(
                                            1,
                                            &gather(&cdfs.partition_w16[ctx], VERT_ALIKE),
                                        ),
                                        (false, true) => enc.symbol_fixed(
                                            1,
                                            &gather(&cdfs.partition_w16[ctx], HORZ_ALIKE),
                                        ),
                                        _ => {}
                                    }
                                    let leaves = block.eight.as_ref().ok_or_else(|| {
                                        Error::unsupported(
                                            "AV1 tile",
                                            "a 16x16 block the true frame edge cuts through \
                                             needs its `eight` leaves populated",
                                        )
                                    })?;
                                    let (mi_row0, mi_col0) =
                                        (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                    let leaf_positions: Vec<(u32, u32)> = (0..4)
                                        .map(|i| (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2))
                                        .filter(|&(mr, mc)| mr < mi_rows && mc < mi_cols)
                                        .collect();
                                    if leaves.len() != leaf_positions.len() {
                                        return Err(Error::unsupported(
                                            "AV1 tile",
                                            "a straddling 16x16 block needs one `eight` entry \
                                             per 8x8 leaf inside the true frame",
                                        ));
                                    }
                                    // r11: the enclosing 16x16 slot's
                                    // above_mode/left_mode arrays are too
                                    // coarse for a second leaf whose true
                                    // above (or left) neighbour is the FIRST
                                    // leaf -- track it here and hand it to
                                    // write_leaf8 as a context override.
                                    let mut prev_leaf: Option<((usize, usize), usize)> = None;
                                    for (leaf, (mr, mc)) in leaves.iter().zip(leaf_positions) {
                                        let leaf_mi = (mr as usize, mc as usize);
                                        // r8: read at mi granularity, not the
                                        // enclosing 16x16 slot -- the first
                                        // leaf's `record_mi` call below
                                        // updates `above_side_mi`/
                                        // `left_side_mi` at this leaf's own
                                        // mi position, which the second
                                        // leaf's ctx lookup then sees.
                                        let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                        ec_rng_trace(|| {
                                            format!(
                                                "EC_PART mi_row={} mi_col={} bsize=3 ctx={} tell={}",
                                                leaf_mi.0,
                                                leaf_mi.1,
                                                leaf_ctx,
                                                enc.tell()
                                            )
                                        });
                                        enc.symbol(
                                            PARTITION_NONE,
                                            &mut cdfs.partition_w8[leaf_ctx],
                                        );
                                        let grids = [
                                            level_grid(&leaf.luma, TX8)?,
                                            level_grid(&leaf.u, TX4)?,
                                            level_grid(&leaf.v, TX4)?,
                                        ];
                                        let leaf_mode = write_leaf8(
                                            &mut enc,
                                            cdfs,
                                            &mut neighbours,
                                            leaf,
                                            at,
                                            leaf_mi,
                                            &grids,
                                            [&scans[2], &scans[3], &scans[3]],
                                            prev_leaf,
                                            tx_select,
                                            &scans,
                                        )?;
                                        prev_leaf = Some((leaf_mi, leaf_mode));
                                        ec_rng_trace(|| {
                                            format!(
                                                "EC_TOK mi_row={} mi_col={} tell={}",
                                                leaf_mi.0,
                                                leaf_mi.1,
                                                enc.tell()
                                            )
                                        });
                                    }
                                    // `record()`'s `above_mode`/`left_mode`
                                    // write is a no-op at an 8x8 leaf's own
                                    // side (`side / SUB == 0`), so the next
                                    // 16x16 quadrant beyond this straddling
                                    // one would otherwise see whatever stale
                                    // mode sat here before it: force the
                                    // write once the whole quadrant's leaves
                                    // are done, from the last (bottom/right-
                                    // most) leaf -- lane-av1-rect r15: doing
                                    // this *inside* `write_leaf8`, once per
                                    // leaf, let the first leaf's write
                                    // clobber the true external neighbour a
                                    // second leaf of the *same* quadrant
                                    // still needed to read on its
                                    // non-adjacency axis.
                                    if let Some((_, mode)) = prev_leaf {
                                        neighbours.above_mode[at.1] = mode;
                                        neighbours.left_mode[at.0] = mode;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(enc.finish())
}

/// `av1_write_uniform` -- the inverse of [`crate::decode`]'s `read_uniform`,
/// which the palette colour-index map's first (context-free) symbol is coded
/// with.
pub(super) fn write_uniform(enc: &mut SymbolEncoder, value: usize, n: usize) {
    let l = crate::decode::ceil_log2(n as u32);
    let m = (1usize << l) - n;
    if value < m {
        enc.literal(value as u32, l - 1);
    } else {
        enc.literal(((value + m) >> 1) as u32, l - 1);
        enc.literal(((value + m) & 1) as u32, 1);
    }
}

/// `write_palette_colors_y` (bitstream.c:1178), the write side of
/// [`crate::decode`]'s `read_palette_colors_y` at 8 bits: one flag per cache
/// entry (taken when this palette holds that colour, in cache order, stopping
/// once `n` are cached), then the colours the cache did not supply -- the
/// first raw, the rest as deltas off a bit width that shrinks with the range
/// left. `colors` is ascending and distinct, so the decoder's `merge_colors`
/// of the two ascending lists puts it back exactly.
pub(super) fn write_palette_colors_y(enc: &mut SymbolEncoder, colors: &[u16], cache: &[u16]) {
    let n = colors.len();
    let mut cached: Vec<u16> = Vec::with_capacity(n);
    for &c in cache {
        if cached.len() >= n {
            break;
        }
        let take = colors.contains(&c);
        enc.literal(u32::from(take), 1);
        if take {
            cached.push(c);
        }
    }
    let transmitted: Vec<u16> = colors
        .iter()
        .copied()
        .filter(|c| !cached.contains(c))
        .collect();
    if transmitted.is_empty() {
        return;
    }
    enc.literal(u32::from(transmitted[0]), 8);
    if transmitted.len() == 1 {
        return;
    }
    // The 2-bit `extra` the decoder adds to `bit_depth - 3`: the smallest one
    // under which every delta still fits its (shrinking) width. `extra == 3`
    // always works -- a delta can never exceed the range left -- so the
    // search below always finds one.
    let fits = |extra: u32| -> bool {
        let mut bits = 5 + extra;
        let mut range = 255i32 - i32::from(transmitted[0]);
        for w in transmitted.windows(2) {
            let delta = i32::from(w[1]) - i32::from(w[0]);
            if delta > (1i32 << bits) {
                return false;
            }
            range -= delta;
            bits = bits.min(crate::decode::ceil_log2(range.max(0) as u32));
        }
        true
    };
    let extra = (0..=3).find(|&e| fits(e)).unwrap_or(3);
    enc.literal(extra, 2);
    let mut bits = 5 + extra;
    let mut range = 255i32 - i32::from(transmitted[0]);
    for w in transmitted.windows(2) {
        let delta = i32::from(w[1]) - i32::from(w[0]);
        enc.literal((delta - 1) as u32, bits);
        range -= delta;
        bits = bits.min(crate::decode::ceil_log2(range.max(0) as u32));
    }
}

/// `write_palette_colors_uv` (bitstream.c:1236), the write side of
/// [`crate::decode`]'s `read_palette_colors_uv` at 8 bits. U is
/// [`write_palette_colors_y`]'s cache/delta scheme with the reader's two
/// documented differences -- the shrinking range starts at `256 - first`
/// (not `255 - first`) and each delta is written unbiased (no `-1`).
///
/// corner-cut: V takes the reader's RAW form (the leading bit 0, then `n`
/// 8-bit literals) rather than its first-value-plus-signed-deltas form.
/// Ceiling: about `n` bits per chroma-palette block more than the delta form
/// would spend (8n against roughly 10 + (n-1)*(5..8)), which the RD pricer
/// below sees, so it only ever makes the encoder take FEWER chroma palettes
/// than it should -- never a wrong stream. Upgrade path: price both forms
/// here and write the cheaper one under its own leading bit.
pub(super) fn write_palette_colors_uv(
    enc: &mut SymbolEncoder,
    u_colors: &[u16],
    v_colors: &[u16],
    cache: &[u16],
) {
    let n = u_colors.len();
    let mut cached: Vec<u16> = Vec::with_capacity(n);
    for &c in cache {
        if cached.len() >= n {
            break;
        }
        let take = u_colors.contains(&c);
        enc.literal(u32::from(take), 1);
        if take {
            PALETTE_UV_CACHE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            cached.push(c);
        }
    }
    let transmitted: Vec<u16> = u_colors
        .iter()
        .copied()
        .filter(|c| !cached.contains(c))
        .collect();
    if !transmitted.is_empty() {
        enc.literal(u32::from(transmitted[0]), 8);
        if transmitted.len() > 1 {
            let fits = |extra: u32| -> bool {
                let mut bits = 5 + extra;
                let mut range = 256i32 - i32::from(transmitted[0]);
                for w in transmitted.windows(2) {
                    let delta = i32::from(w[1]) - i32::from(w[0]);
                    if delta >= (1i32 << bits) {
                        return false;
                    }
                    range -= delta;
                    bits = bits.min(crate::decode::ceil_log2(range.max(0) as u32));
                }
                true
            };
            let extra = (0..=3).find(|&e| fits(e)).unwrap_or(3);
            enc.literal(extra, 2);
            let mut bits = 5 + extra;
            let mut range = 256i32 - i32::from(transmitted[0]);
            for w in transmitted.windows(2) {
                let delta = i32::from(w[1]) - i32::from(w[0]);
                enc.literal(delta as u32, bits);
                range -= delta;
                bits = bits.min(crate::decode::ceil_log2(range.max(0) as u32));
            }
        }
    }
    enc.literal(0, 1);
    for &v in v_colors {
        enc.literal(u32::from(v), 8);
    }
}

/// `write_palette_color_map` (bitstream.c), the write side of
/// [`crate::decode`]'s `decode_color_index_map`: the same wavefront diagonal,
/// each symbol written as the position its colour takes in the neighbour
/// ordering `av1_get_palette_color_index_context` hands back.
pub(super) fn write_color_index_map(
    enc: &mut SymbolEncoder,
    idx_cdfs: &mut [[u16; 9]; 5],
    map: &[u8],
    side: usize,
    n: usize,
    // spec 5.11.50 `onscreenWidth`/`onscreenHeight` in this map's own units:
    // a block cut by the frame's right/bottom mi edge codes ONLY this corner
    // and the decoder replicates it outward (`decode::palette_replicate`).
    // Writing the full grid spent symbols a conformant decoder never reads,
    // desyncing the rest of the tile (lane-palw, class
    // writer-phantom-units-past-edge).
    on: (usize, usize),
) {
    let (on_w, on_h) = (on.0.clamp(1, side), on.1.clamp(1, side));
    write_uniform(enc, usize::from(map[0]), n);
    for i in 1..(on_w + on_h - 1) {
        for j in (i.saturating_sub(on_h - 1)..=i.min(on_w - 1)).rev() {
            let (row, col) = (i - j, j);
            let (ctx, color_order) =
                crate::decode::palette_color_index_context(map, side, row, col, n);
            let want = map[row * side + col];
            let symbol = color_order[..n]
                .iter()
                .position(|&c| c == want)
                .expect("every colour index is in the block\'s own palette");
            enc.symbol(symbol, &mut idx_cdfs[ctx][..=n]);
        }
    }
}

/// What one block's palette syntax costs, in bits: the `palette_y_mode` flag,
/// the size symbol, the colours and the whole colour-index map, priced
/// through the very writers above on a [`SymbolEncoder::pricer`] -- so the
/// search reads the rate the tile will really spend, not an estimate of it.
/// The neighbour cache is taken as empty (the search runs before the tile
/// knows what its neighbours will be), which only ever over-prices.
pub(crate) fn palette_bits(pal: &PaletteY, side: usize, on: (usize, usize)) -> f64 {
    let Some(bsize_ctx) = crate::decode::palette_bsize_ctx(side) else {
        return f64::INFINITY;
    };
    let n = usize::from(pal.size);
    let mut enc = SymbolEncoder::pricer();
    let mut mode_cdf = cdf::PALETTE_Y_MODE[bsize_ctx][0];
    enc.symbol(1, &mut mode_cdf);
    let mut size_cdf = cdf::PALETTE_Y_SIZE[bsize_ctx];
    enc.symbol(n - 2, &mut size_cdf);
    write_palette_colors_y(&mut enc, &pal.colors[..n], &[]);
    let mut idx_cdfs = cdf::PALETTE_Y_COLOR_INDEX[n - 2];
    write_color_index_map(&mut enc, &mut idx_cdfs, &pal.map, side, n, on);
    enc.bits()
}

/// [`palette_bits`] for the chroma palette: the `palette_uv_mode` flag, the
/// size symbol, both colour lists and the shared colour-index map, priced
/// through the very writers above (empty cache, which only over-prices).
pub(crate) fn palette_uv_bits(pal: &PaletteUv, side: usize, on: (usize, usize)) -> f64 {
    let Some(bsize_ctx) = crate::decode::palette_bsize_ctx(side) else {
        return f64::INFINITY;
    };
    let n = usize::from(pal.size);
    let mut enc = SymbolEncoder::pricer();
    let mut mode_cdf = cdf::PALETTE_UV_MODE[0];
    enc.symbol(1, &mut mode_cdf);
    let mut size_cdf = cdf::PALETTE_UV_SIZE[bsize_ctx];
    enc.symbol(n - 2, &mut size_cdf);
    write_palette_colors_uv(&mut enc, &pal.u_colors[..n], &pal.v_colors[..n], &[]);
    let mut idx_cdfs = cdf::PALETTE_UV_COLOR_INDEX[n - 2];
    write_color_index_map(
        &mut enc,
        &mut idx_cdfs,
        &pal.map,
        palette_uv_side(side),
        n,
        on,
    );
    enc.bits()
}

/// Writes everything a key frame's block carries before its coefficients: the
/// skip flag, its luma intra mode against the CDF its neighbours' modes pick,
/// the angle a directional mode is steered by, and its chroma mode. Hands back
/// the luma mode, which is what the blocks beside it read.
/// The DV predictor decode.rs `read_intrabc_dv` derives, mirrored line for
/// line off the writer's own copy of the intrabc mi grid: the `INTRA_FRAME`
/// stack's `nearest_mv` (or `near_mv` when that is zero), or -- when both are
/// -- `av1_find_ref_dv`'s fallback one superblock up, or 256 pixels plus one
/// superblock to the left on the tile's first superblock row, then floored to
/// full pel. Returns `(0, 0)` when no grid is armed, which no `allow_intrabc`
/// tile ever reaches.
pub(super) fn intrabc_dv_pred(mi_r: usize, mi_c: usize, side: usize) -> (i32, i32) {
    const INTRABC_DELAY_PIXELS: i32 = 256;
    let n4 = side / MI;
    let stack_pred = INTRABC_GRID.with(|g| {
        let g = g.borrow();
        g.as_ref().map(|(grid, mi_cols, mi_rows)| {
            let stack = crate::mvstack::find_mv_stack(
                grid, mi_r, mi_c, n4, n4, 0, /* INTRA_FRAME */
                *mi_cols, *mi_rows,
            );
            if stack.nearest_mv == (0, 0) {
                stack.near_mv
            } else {
                stack.nearest_mv
            }
        })
    });
    let mut pred = stack_pred.unwrap_or((0, 0));
    if pred == (0, 0) {
        // `av1_find_ref_dv` takes `mib_size`, the SEQUENCE's superblock size
        // in mi units (lane-b128m: hardcoding 16 here made the writer's DV
        // predictor one 64 superblock up while decode.rs `read_intrabc_dv`
        // took 128 -- identical symbols, different reconstructed DV). The
        // tile row start is 0, exactly as the reader takes it -- both are
        // written for the single-tile-row streams this encoder produces.
        let sb_mi = if sb128_armed() { 32i32 } else { 16i32 };
        let sb_px = sb_mi * MI as i32;
        pred = if (mi_r as i32) < sb_mi {
            (0, -(sb_px + INTRABC_DELAY_PIXELS) * 8)
        } else {
            (-sb_px * 8, 0)
        };
    }
    ((pred.0 >> 3) * 8, (pred.1 >> 3) * 8)
}

/// Publishes one coded block into [`INTRABC_GRID`], mirroring decode.rs
/// `record_intrabc_mi`: every block contributes its size, an intrabc one also
/// its DV as an `INTRA_FRAME` candidate.
pub(super) fn record_intrabc_mi(mi_r: usize, mi_c: usize, n4: usize, dv: Option<(i32, i32)>) {
    INTRABC_GRID.with(|g| {
        let mut g = g.borrow_mut();
        let Some((grid, _, _)) = g.as_mut() else {
            return;
        };
        let info = crate::mvstack::MiInfo {
            is_inter: dv.is_some(),
            ref_frame: 0,
            ref_frame1: crate::mvstack::NO_REF1,
            mv: crate::mvstack::mv16(dv.unwrap_or((0, 0))),
            mv1: (0, 0),
            is_new_mv: dv.is_some(),
            is_global_mv0: false,
            is_global_mv1: false,
            size: n4 as u8,
            size_h: n4 as u8,
        };
        for r in mi_r..mi_r + n4 {
            for c in mi_c..mi_c + n4 {
                grid.set(r, c, info);
            }
        }
    });
}

/// One full-pel block-vector component, [`write_mv_component`]'s
/// `force_integer_mv` twin: decode.rs `read_mv_component` infers `mv_fr`/
/// `mv_class0_fr` as 3 and the high-precision bit as 1 under
/// `force_integer_mv`, so neither symbol is written here -- writing them
/// would leave the decoder two symbols behind from this block on.
pub(super) fn write_dv_component(enc: &mut SymbolEncoder, c: &mut MvComponentCdfs, diff: i32) -> Result<()> {
    let mag = diff.unsigned_abs() as i32;
    if mag % 8 != 0 {
        return Err(Error::unsupported(
            "AV1 tile",
            "a block vector is coded at full-pel precision only",
        ));
    }
    enc.symbol(usize::from(diff < 0), &mut c.sign);
    let z = mag - 1;
    let class = mv_class_of(z);
    enc.symbol(class, &mut c.class);
    let local = z - mv_class_base(class);
    if class == 0 {
        enc.symbol(((local >> 3) & 1) as usize, &mut c.class0_bit);
    } else {
        let d = local >> 3;
        for i in 0..class {
            enc.symbol(((d >> i) & 1) as usize, &mut c.bit[i]);
        }
    }
    Ok(())
}

/// One intrabc block's DV as a residual against [`intrabc_dv_pred`], off the
/// `dv` nmv context (decode.rs `read_intrabc_dv` -> `read_mv`).
pub(super) fn write_dv(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    dv: (i32, i32),
    pred: (i32, i32),
) -> Result<()> {
    let diff = (dv.0 - pred.0, dv.1 - pred.1);
    let joint = match (diff.0 != 0, diff.1 != 0) {
        (false, false) => 0,
        (false, true) => 1,
        (true, false) => 2,
        (true, true) => 3,
    };
    enc.symbol(joint, &mut cdfs.dv_joint);
    if diff.0 != 0 {
        write_dv_component(enc, &mut cdfs.dv_comp[0], diff.0)?;
    }
    if diff.1 != 0 {
        write_dv_component(enc, &mut cdfs.dv_comp[1], diff.1)?;
    }
    Ok(())
}

/// The mode-info half of an intrabc block (decode.rs `read_intra_mode`'s
/// early return): `skip`, `cdef_idx`, `use_intrabc` and the DV, then nothing
/// -- no y/uv mode, no angle delta, no palette, and (because it is skipped)
/// no transform size and no residual. Publishes the block into every
/// neighbour band an ordinary block leaves behind, at the DC_PRED/uncoded/
/// skipped state a decoder reads back for it.
pub(super) fn write_intrabc_block(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    dv: (i32, i32),
    mi: (usize, usize),
    side: usize,
) -> Result<()> {
    let skip_ctx =
        usize::from(neighbours.above_skip[mi.1]) + usize::from(neighbours.left_skip[mi.0]);
    enc.symbol(1, &mut cdfs.skip[skip_ctx]);
    write_cdef_idx(enc, mi, true);
    enc.symbol(1, &mut cdfs.intrabc);
    write_dv(enc, cdfs, dv, intrabc_dv_pred(mi.0, mi.1, side))?;
    note_intrabc(dv);
    // The bands: DC_PRED mode (what the decoder forces), no coefficients
    // anywhere (skip), the block's own side for the partition context, the
    // largest transform (`side.min(64)`, decode.rs's `intrabc_skip_tx` arm)
    // for the deblock grid, and `skip` itself for the next block's skip
    // context.
    // One zero level per plane: `neighbour_state` reads `grid[0]` for the DC
    // sign vote, and a skipped block leaves every context cleared.
    let empty: [Vec<i32>; 3] = [vec![0], vec![0], vec![0]];
    for cell in 0..(side / SUB).max(1) {
        let (r, c) = (mi.0 / (SUB / MI), mi.1 / (SUB / MI));
        neighbours.above_mode[c + cell] = DC_PRED;
        neighbours.left_mode[r + cell] = DC_PRED;
        neighbours.above_side[c + cell] = side;
        neighbours.left_side[r + cell] = side;
    }
    neighbours.record_mi_planes(mi, side, &empty, true);
    neighbours.record_tx(mi, side, side.min(64));
    for cell in 0..side / MI {
        neighbours.above_skip[mi.1 + cell] = true;
        neighbours.left_skip[mi.0 + cell] = true;
    }
    Ok(())
}

/// `UV_CFL_PRED` (spec 6.10.19's chroma mode enum), the fourteenth entry of
/// [`crate::cdf::UV_MODE_CFL`] -- the writer's twin of the decoder's own
/// constant.
pub(crate) const UV_CFL_PRED: usize = 13;

/// The `cfl_alpha_signs` joint value (0..8) that codes this signed alpha pair,
/// inverting the decoder's own `read_cfl_alphas` split (`sign_u =
/// ((joint+1)*11)>>5`, `sign_v = (joint+1) - 3*sign_u`, with 0 ZERO, 1 NEG,
/// 2 POS). There is no (ZERO, ZERO) joint value, so at least one alpha of a
/// CfL block is nonzero by construction.
pub(crate) fn cfl_joint_sign(alpha_u: i32, alpha_v: i32) -> usize {
    let sign = |a: i32| match a.signum() {
        0 => 0,
        -1 => 1,
        _ => 2,
    };
    let (want_u, want_v) = (sign(alpha_u), sign(alpha_v));
    (0..8)
        .find(|&j| {
            let su = ((j + 1) * 11) >> 5;
            (su, (j + 1) - 3 * su) == (want_u, want_v)
        })
        .expect("cfl_alpha_signs covers every pair but (ZERO, ZERO)") as usize
}

/// The `cfl_alpha_u`/`cfl_alpha_v` magnitude CDF row for one plane, by the
/// same `CFL_CONTEXT_U`/`CFL_CONTEXT_V` derivation the decoder reads with
/// (libaom cfl.h). `joint` is [`cfl_joint_sign`]'s value.
pub(super) fn cfl_alpha_ctx(joint: usize, plane: usize) -> usize {
    let joint = joint as i32;
    let su = ((joint + 1) * 11) >> 5;
    let sv = (joint + 1) - 3 * su;
    if plane == 0 {
        (joint + 1 - 3) as usize
    } else {
        (sv * 3 + su - 3) as usize
    }
}

/// What [`write_cfl_alphas`] will spend on this alpha pair, through the same
/// tables -- the encoder's RD price for the CfL arm.
pub(crate) fn cfl_alpha_bits(alpha_u: i32, alpha_v: i32) -> f64 {
    use crate::encode::symbol_bits;
    let joint = cfl_joint_sign(alpha_u, alpha_v);
    let mut bits = symbol_bits(&crate::cdf::CFL_SIGN, joint);
    for (plane, alpha) in [alpha_u, alpha_v].into_iter().enumerate() {
        if alpha != 0 {
            bits += symbol_bits(
                &crate::cdf::CFL_ALPHA[cfl_alpha_ctx(joint, plane)],
                (alpha.unsigned_abs() - 1) as usize,
            );
        }
    }
    bits
}

/// `read_cfl_alphas` (spec 5.11.45) written: the joint sign, then each
/// plane's magnitude where its sign is nonzero.
pub(super) fn write_cfl_alphas(enc: &mut SymbolEncoder, cdfs: &mut Cdfs, (alpha_u, alpha_v): (i32, i32)) {
    let joint = cfl_joint_sign(alpha_u, alpha_v);
    enc.symbol(joint, &mut cdfs.cfl_sign);
    for (plane, alpha) in [alpha_u, alpha_v].into_iter().enumerate() {
        if alpha != 0 {
            enc.symbol(
                (alpha.unsigned_abs() - 1) as usize,
                &mut cdfs.cfl_alpha[cfl_alpha_ctx(joint, plane)],
            );
        }
    }
}

// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_intra_mode(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    block: &BlockCoeffs,
    above_mode: usize,
    left_mode: usize,
    cfl: bool,
    // This block's own position in 4x4 mode-info units, for the `cdef_idx`
    // literal that follows the `skip` symbol (see [`write_cdef_idx`]).
    mi: (usize, usize),
    // The block's side in samples, which gates the palette syntax below
    // (`av1_allow_palette`, [`crate::decode::palette_bsize_ctx`]).
    side: usize,
) -> usize {
    let mode = usize::from(block.mode);
    // `av1_get_skip_txfm_context` (decode.rs `Neighbours::skip_txfm_ctx`):
    // above + left. Every block this writer codes outside an `allow_intrabc`
    // frame is unskipped, so both bands stay false and this stays 0 -- the
    // literal every stream before this lane was written with.
    let skip_ctx =
        usize::from(neighbours.above_skip[mi.1]) + usize::from(neighbours.left_skip[mi.0]);
    enc.symbol(0, &mut cdfs.skip[skip_ctx]);
    write_cdef_idx(enc, mi, false);
    // `read_intrabc_info` (spec 5.11.13): the flag is read for EVERY intra
    // block of an `allow_intrabc` frame, not only the ones that use it.
    if intrabc_armed() {
        enc.symbol(0, &mut cdfs.intrabc);
    }
    enc.symbol(
        mode,
        &mut cdfs.kf_y_mode[INTRA_MODE_CTX[above_mode]][INTRA_MODE_CTX[left_mode]],
    );
    ec_rng_trace(|| format!("EC_YMODE mode={mode} tell={} rng={}", enc.tell(), enc.rng()));
    if (V_PRED..=D67_PRED).contains(&mode) {
        enc.symbol(
            (ANGLE_DELTA_ZERO as i32 + i32::from(block.angle_delta_y)) as usize,
            &mut cdfs.angle_delta[mode - V_PRED],
        );
    }
    // A block small enough to be offered chroma from luma reads the wider
    // table even when it does not take the mode.
    let uv_mode = usize::from(block.uv_mode);
    if cfl {
        enc.symbol(uv_mode, &mut cdfs.uv_mode_cfl[mode]);
    } else {
        debug_assert_ne!(uv_mode, UV_CFL_PRED, "is_cfl_allowed excludes this block");
        enc.symbol(uv_mode, &mut cdfs.uv_mode_no_cfl[mode]);
    }
    if uv_mode == UV_CFL_PRED {
        write_cfl_alphas(
            enc,
            cdfs,
            block
                .cfl_alphas
                .expect("a UV_CFL_PRED block carries its alphas"),
        );
    }
    // `angle_delta_uv` (spec `read_intra_angle_info`) off the same CDF array
    // the luma delta reads, indexed by the chroma mode.
    if (V_PRED..=D67_PRED).contains(&uv_mode) {
        enc.symbol(ANGLE_DELTA_ZERO, &mut cdfs.angle_delta[uv_mode - V_PRED]);
    }
    // `read_palette_mode_info` (spec 5.11.46), mirrored symbol for symbol from
    // the decoder's own read in `read_intra_mode`: on a frame whose header set
    // `allow_screen_content_tools`, a `DC_PRED` luma block and a `UV_DC_PRED`
    // chroma pair each carry a use-palette flag. Both contexts are 0 here --
    // `av1_get_palette_mode_ctx` counts palette NEIGHBOURS, and this writer
    // codes none -- exactly the ctx-0 read the decoder does under the same
    // condition.
    write_palette_syntax(
        enc,
        cdfs,
        neighbours,
        block.palette.as_ref(),
        block.palette_uv.as_ref(),
        block.filter_intra,
        mode,
        uv_mode,
        mi,
        side,
    );
    mode
}

/// `read_palette_mode_info` (spec 5.11.46) written out, mirrored symbol for
/// symbol from the decoder's own read in `read_intra_mode`: on a frame whose
/// header set `allow_screen_content_tools`, a `DC_PRED` luma block and a
/// `UV_DC_PRED` chroma pair each carry a use-palette flag, and a luma palette
/// then codes its size, its colours (against the neighbour colour cache) and
/// -- after the chroma flag, exactly where libaom's `av1_visit_palette` runs
/// -- its colour-index map. Shared by the key frame's [`write_intra_mode`] and
/// by the three intra arms of the inter-frame writers, which read the same
/// syntax back through `decode_inter_block`/`decode_intra_rect_in_inter`.
///
/// The band publication is deferred to [`Neighbours::record_mi_planes`] (the
/// one point every block passes through), so a block that takes no palette
/// still clears what the column held.
#[allow(clippy::too_many_arguments)]
pub(super) fn write_palette_syntax(
    enc: &mut SymbolEncoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    palette: Option<&PaletteY>,
    palette_uv: Option<&PaletteUv>,
    // lane-fintra: this block's chosen `filter_intra_mode`, or `None`. Written
    // here rather than by the caller because its symbol sits BETWEEN the
    // palette colour lists and the colour-index maps (decode.rs
    // `read_intra_mode`), which is inside this function.
    filter_intra: Option<u8>,
    mode: usize,
    uv_mode: usize,
    mi: (usize, usize),
    side: usize,
) {
    if !screen_armed() {
        write_filter_intra(enc, cdfs, mode, side, false, filter_intra);
        return;
    }
    let palette = palette.filter(|_| mode == DC_PRED);
    let palette_uv = palette_uv.filter(|_| uv_mode == DC_PRED);
    if let Some(bsize_ctx) = crate::decode::palette_bsize_ctx(side) {
        let (mode_ctx, cache) = neighbours.palette_ctx_and_cache(mi);
        let uv_cache = neighbours.palette_uv_cache(mi);
        if mode == DC_PRED {
            enc.symbol(
                usize::from(palette.is_some()),
                &mut cdfs.palette_y_mode[bsize_ctx][mode_ctx],
            );
        }
        if let Some(pal) = palette {
            let n = usize::from(pal.size);
            enc.symbol(n - 2, &mut cdfs.palette_y_size[bsize_ctx]);
            write_palette_colors_y(enc, &pal.colors[..n], &cache);
        }
        if uv_mode == DC_PRED {
            enc.symbol(
                usize::from(palette_uv.is_some()),
                &mut cdfs.palette_uv_mode[usize::from(palette.is_some())],
            );
        }
        if let Some(pal) = palette_uv {
            let n = usize::from(pal.size);
            enc.symbol(n - 2, &mut cdfs.palette_uv_size[bsize_ctx]);
            write_palette_colors_uv(enc, &pal.u_colors[..n], &pal.v_colors[..n], &uv_cache);
        }
        write_filter_intra(enc, cdfs, mode, side, palette.is_some(), filter_intra);
        // Both colour-index maps come after BOTH colour lists, in plane order
        // (`av1_visit_palette` runs from the caller, past `filter_intra`).
        // The on-screen extent of THIS block, from the frame mi dims armed
        // for this tile write -- the decoder's `palette_onscreen` half.
        let on = crate::decode::palette_onscreen_dims(mi.0, mi.1, side, side, frame_mi_dims());
        if (on.0 < side || on.1 < side) && (palette.is_some() || palette_uv.is_some()) {
            PALETTE_CUT_MAPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(pal) = palette {
            let n = usize::from(pal.size);
            write_color_index_map(
                enc,
                &mut cdfs.palette_y_color_index[n - 2],
                &pal.map,
                side,
                n,
                on,
            );
            note_palette(n);
        }
        if let Some(pal) = palette_uv {
            let n = usize::from(pal.size);
            let uv_side = palette_uv_side(side);
            write_color_index_map(
                enc,
                &mut cdfs.palette_uv_color_index[n - 2],
                &pal.map,
                uv_side,
                n,
                // The encoder writes 4:2:0 only (`palette_uv_side` above).
                crate::decode::palette_onscreen_uv((side, side), on, (uv_side, uv_side), (1, 1)),
            );
            note_palette_uv(n);
        }
    } else {
        // Below `av1_allow_palette`'s size bound there is no palette syntax at
        // all, but `av1_filter_intra_allowed_bsize` still admits the block.
        write_filter_intra(enc, cdfs, mode, side, false, filter_intra);
    }
    neighbours.pending_palette = palette.map(|p| (p.size, p.colors));
    neighbours.pending_palette_uv = palette_uv.map(|p| (p.size, p.u_colors));
}

