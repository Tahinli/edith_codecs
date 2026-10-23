//! Software decoder for this crate's own key-frame tile payloads (spec 5.11),
//! the reader half of [`crate::tile`]'s writer.
//!
//! `tile.rs` is another lane's territory this round (`lane-av1-rect` is
//! touching its partition search), so nothing here imports its private
//! helpers: the handful of `pub(crate)` items it already exposes
//! ([`crate::tile::block_grid`], [`crate::tile::has_half`],
//! [`crate::tile::INTRA_MODE_CTX`], [`crate::tile::write_golomb`]) are reused,
//! and everything else (the partition/coefficient context arithmetic, the
//! scan table) is a from-spec reimplementation kept in step with the writer by
//! the gate tests below rather than by sharing code with it.
//!
//! # Scope (round 1)
//! Key frames only, and only the shapes the encoder actually still needs to
//! prove itself against: every coded block is `DC_PRED` (this crate's mode
//! search names other intra modes, but a decoder for those needs the
//! directional-prediction edge-reach plumbing [`crate::intra::predict`]
//! already carries; wiring it up is round 2), never skipped, and every
//! superblock is a whole number of 64x64 units (a frame whose true edge falls
//! inside one, forcing the gathered-CDF partial-superblock path, is round 2
//! too). Anything outside that refuses with [`Error::unsupported`] rather than
//! silently miscoding.
use crate::hits::{hit, hit_do};

use ec_av1_syntax::{
    CdefParams, DeltaParams, LoopFilterParams, LoopRestorationParams, MAX_SEGMENTS,
    SEG_LVL_ALT_LF_Y_V, SEG_LVL_ALT_Q, SegmentationParams, TileInfo,
};
use ec_core::{Error, Result};

// lane-av112bit: the 12-bit warp refusal that used to stand at all three
// warp decision sites (compound, 16x16+, 8x8 leaf) was an inline literal
// (refusal_inventory.rs extracts refusal strings from the decode source);
// it named the real gap: warp's reduce bits inherited the 12-bit
// `conv_params->round_0` (warped_motion.c:295, convolve.h:81..86) and no
// witness existed.
// lane-av112bitw lifted it from all three sites: `warp::warp_round_0`
// implements the bump (`round_0 += max(bd + FILTER_BITS - round_0 - 14, 0)`,
// 5 at 12 bits), and the single-ref witness plus the rotating-mandelbrot
// compound-warp witness are both byte-exact vs ffmpeg. Compound inter
// itself was lifted by lane-av112bitc before that (the combines absorb the
// CONV_BUF gain drop, `INTER_POST_ROUND - delta`, see `mc::combine_compound`).

mod counters;
pub use counters::*;

mod frame_tile;

mod inter_block8;
mod inter_intra_arm;
use inter_block8::*;
use inter_intra_arm::*;

mod inter_block;
use inter_block::*;

mod post_filter;
use post_filter::*;

mod coeffs;
pub(crate) use coeffs::*;

#[cfg(target_arch = "x86_64")]
mod cdef_simd;

mod rect;
pub(crate) use rect::*;

mod rect4;
use rect4::*;

mod block;
mod wave;
pub use block::*;

mod leaf8;
use leaf8::*;

mod leaf_sub8;
use leaf_sub8::*;

mod cdef;
use cdef::*;

mod inter_shapes;
pub use inter_shapes::*;

mod deblock;
pub(crate) use deblock::*;

#[cfg(target_arch = "x86_64")]
mod deblock_simd;

mod replay;
pub(crate) use replay::*;

mod key_tile;
pub use key_tile::*;

mod inter_ctx;
pub(crate) use inter_ctx::*;

#[cfg(test)]
mod obmc_skip_chroma_tests;

mod sub8_inter;
pub(crate) use frame_tile::*;
use sub8_inter::*;

/// Per-frame decode state, one instance per decoded stream (lane-thread1).
///
/// Every field here used to be a `thread_local!` static, which made a decode a
/// function of the calling THREAD's history rather than of the bitstream alone
/// -- and made frame-parallel decoding impossible, since a worker thread sees
/// empty state. The cells keep exactly the interior mutability the statics
/// had, so every read/write site is byte-identical; what changes is WHERE the
/// state lives: `stream.rs` owns one `FrameCtx` and threads `&FrameCtx` down
/// the decode call graph.
///
/// `Cell`/`RefCell` make this `!Sync` on purpose: a `FrameCtx` belongs to
/// exactly one thread's decode, and is never shared between workers.
/// One frame's in-loop filter intermediates, as [`FrameCtx::capture_stages`]
/// asks for them: each plane's post-deblock (pre-CDEF) and post-CDEF
/// (pre-restoration) samples, at the decoder's own plane stride.
pub(crate) struct FilterStages {
    /// Post-deblock, pre-CDEF planes (Y, U, V).
    pub(crate) deblocked: [Vec<u16>; 3],
    /// Post-CDEF, pre-restoration planes (Y, U, V).
    pub(crate) cdefed: [Vec<u16>; 3],
    /// Each plane's stride (and so its width in the buffers above).
    pub(crate) stride: [usize; 3],
}

/// Takes whatever the last capturing decode left (see
/// [`FrameCtx::capture_stages`]).
#[allow(dead_code)] // read from the encoder's loop restoration search
pub(crate) fn take_filter_stages(fctx: &FrameCtx) -> Option<FilterStages> {
    fctx.stages.borrow_mut().take()
}

pub(crate) struct FrameCtx {
    /// lane-av1lr: when set, the frame filter chain leaves its post-deblock
    /// and post-CDEF planes in [`FrameCtx::stages`] for the ENCODER's loop
    /// restoration search (spec 7.17 reads both: the stripe interior from
    /// the post-CDEF plane, its 3-row border from the post-deblock one).
    /// Off for every decode but that search, and off it costs one bool read
    /// per frame and not a single copy.
    pub(crate) capture_stages: std::cell::Cell<bool>,
    pub(crate) stages: std::cell::RefCell<Option<FilterStages>>,
    pub(crate) sb128_flag: std::cell::Cell<bool>,
    /// lane-refs: the frame-level `interpolation_filter` (spec 6.8.9) every
    /// motion compensation of THIS frame runs, already resolved to a kernel.
    /// `mc::predict` used to hardwire `Regular`, so the encoder's search,
    /// trial and recon could only ever build a REGULAR prediction whatever
    /// the frame header said; it lives here (and is copied by
    /// [`filter_ctx_copy`]) so a tile-search worker thread reads the same
    /// kernel the frame header wrote instead of the default.
    pub(crate) interp_filter: std::cell::Cell<crate::mc::InterpFilterKind>,
    /// lane-lossless: this frame's per-segment lossless table -- libaom's
    /// `xd->lossless[MAX_SEGMENTS]`, computed ONCE per frame in the header
    /// (spec 5.9.12 `CodedLossless`: each segment at `qindex == 0` with zero
    /// plane deltas, `get_qindex` applying that segment's own `SEG_LVL_ALT_Q`).
    /// Every block-decoder rule that reads [`decode::lossless`] resolves it
    /// through the BLOCK's own `segment_id`: TX_4X4 with the Walsh-Hadamard
    /// transform on every plane, the dropped `tx_type` symbol and the narrowed
    /// `is_cfl_allowed` (libaom `read_tx_size` (`decodeframe.c`),
    /// `read_tx_type` (`decodetxb.c`), `is_cfl_allowed` (`blockd.h`)) follow
    /// the SEGMENT, so a frame whose segments disagree codes lossless and
    /// lossy blocks side by side.
    pub(crate) lossless_per_seg: std::cell::Cell<[bool; MAX_SEGMENTS]>,
    /// lane-av1txr: this frame header's `disable_cdf_update` (spec 5.9.2).
    /// When set, the tile reader must not adapt any CDF it reads, or its
    /// tables drift away from the encoder's and the frame desyncs mid-tile
    /// (libaom: `allow_update_cdf = allow_update_cdf &&
    /// !cm->features.disable_cdf_update`, `decodeframe.c:2909`).
    pub(crate) disable_cdf_update: std::cell::Cell<bool>,
    pub(crate) cdef_bits: std::cell::Cell<u8>,
    pub(crate) bit_depth: std::cell::Cell<u8>,
    /// lane-av1mono: the sequence header's `color_config.mono_chrome` (spec
    /// 5.5.2), i.e. `NumPlanes == 1`. libaom threads this as
    /// `av1_num_planes(cm)` and every chroma read site is guarded by
    /// `!cm->seq_params->monochrome && xd->is_chroma_ref` (`decodemv.c:933`,
    /// `:1208`) or bounded by `plane < num_planes`; a monochrome frame codes
    /// NO `uv_mode`/`cfl`/`angle_delta_uv`/`palette_uv` symbol and NO chroma
    /// coefficients on any block. Set once per frame from the sequence header
    /// (like [`FrameCtx::bit_depth`]), so a mono frame decoded on a thread
    /// that last saw a 4:2:0 one never inherits the wrong plane count.
    pub(crate) mono_chrome: std::cell::Cell<bool>,
    /// lane-av1-444: sequence subsampling_x/y. 1,1 is 4:2:0; 0,0 is 4:4:4.
    pub(crate) subsampling_x: std::cell::Cell<u8>,
    pub(crate) subsampling_y: std::cell::Cell<u8>,
    pub(crate) superres: std::cell::Cell<(u32, u32)>,
    pub(crate) cdef_transmitted: std::cell::Cell<bool>,
    pub(crate) cdef_sb_cols: std::cell::Cell<usize>,
    pub(crate) cdef_idx_grid: std::cell::RefCell<Vec<u8>>,
    pub(crate) delta_q_present: std::cell::Cell<bool>,
    pub(crate) delta_q_res: std::cell::Cell<i32>,
    pub(crate) current_q_idx: std::cell::Cell<i32>,
    pub(crate) quant_deltas: std::cell::Cell<crate::quant::QuantDeltas>,
    /// lane-av1-qmatrix: this frame's quantisation-matrix LEVEL per plane,
    /// straight from spec 5.9.12's `qm_y`/`qm_u`/`qm_v` (the parsed 4-bit
    /// value IS the table level -- libaom `quant_params->qmatrix_level_y/u/v`,
    /// decodeframe.c:1902-1917; the `aom_get_qmlevel` formula that maps
    /// base_q_idx onto a level is encoder-side only). 15 is libaom's flat
    /// sentinel (`NUM_QM_LEVELS - 1`, stored as NULL tables) and the default
    /// every `using_qmatrix == 0` frame keeps, so a matrix-free stream
    /// selects no matrix.
    pub(crate) qm_levels: std::cell::Cell<[u8; 3]>,
    pub(crate) delta_lf_present: std::cell::Cell<bool>,
    pub(crate) delta_lf_res: std::cell::Cell<i32>,
    pub(crate) delta_lf_multi: std::cell::Cell<bool>,
    pub(crate) current_delta_lf: std::cell::Cell<[i32; 4]>,
    pub(crate) seg: std::cell::RefCell<SegmentationParams>,
    pub(crate) seg_ids: std::cell::RefCell<Vec<u8>>,
    pub(crate) prev_seg_ids: std::cell::RefCell<Vec<u8>>,
    pub(crate) seg_mi_dims: std::cell::Cell<(usize, usize)>,
    pub(crate) above_seg_pred: std::cell::RefCell<Vec<u8>>,
    pub(crate) left_seg_pred: std::cell::RefCell<Vec<u8>>,
    pub(crate) cur_segment_id: std::cell::Cell<u8>,
    pub(crate) seg_tile_origin: std::cell::Cell<(usize, usize)>,
    pub(crate) tx_select_inter: std::cell::Cell<bool>,
    /// Whether the tile being decoded is an INTRA-ONLY frame's (key/intra-only
    /// frame). libaom runs `set_txfm_ctxs` for EVERY block of EVERY frame
    /// (`decodeframe.c` `parse_decode_block`), so an intra-only frame
    /// maintains the `TXFM_CONTEXT` bands exactly like an inter one; only this
    /// decoder's inter-side publishing was wired. See
    /// [`Neighbours::fill_lf_grid_rect`]'s band publish.
    pub(crate) intra_only: std::cell::Cell<bool>,
    /// This FRAME's own `tx_mode == TxMode::Select`, set for EVERY frame
    /// (intra included). [`Self::tx_select_inter`] cannot stand in for it:
    /// an intrabc block rides an INTRA frame, but libaom still reads its
    /// var-tx `txfm_partition` tree under the frame's `TxMode::Select`.
    pub(crate) tx_select_frame: std::cell::Cell<bool>,
    pub(crate) reduced_tx_set_inter: std::cell::Cell<bool>,
    pub(crate) enable_filter_intra_inter: std::cell::Cell<bool>,
    pub(crate) palette_pred: std::cell::RefCell<Option<Vec<u16>>>,
    /// Set when a reconstruction input is inconsistent with its block: a
    /// palette/intrabc override or a CfL AC signal whose size is not the
    /// transform block's. A conforming stream cannot produce one, so the
    /// frame is refused with `Error::Corrupt` once its tiles are decoded.
    pub(crate) corrupt: std::cell::Cell<Option<&'static str>>,
    pub(crate) intrabc_mi_grid: std::cell::RefCell<Option<(crate::mvstack::MiGrid, usize, usize)>>,
    pub(crate) intrabc_dv: std::cell::Cell<Option<(i32, i32)>>,
    pub(crate) luma_tx_type: std::cell::Cell<TxType>,
    pub(crate) intrabc_chroma_tx: std::cell::Cell<Option<TxType>>,
    pub(crate) intra_in_inter_mode: std::cell::Cell<Option<(usize, bool)>>,
    pub(crate) inter_strip_chroma: std::cell::Cell<Option<InterStripChroma>>,
    pub(crate) inter_last_mc:
        std::cell::Cell<Option<(i8, (i32, i32), mc::InterpFilterKind, mc::InterpFilterKind)>>,
    pub(crate) enable_edge_filter: std::cell::Cell<bool>,
    pub(crate) last_frame_wide_margin: std::cell::RefCell<Option<Picture>>,
    pub(crate) reach_sb_px: std::cell::Cell<usize>,
    pub(crate) grain_bit_depth: std::cell::Cell<u32>,
}

impl std::fmt::Debug for FrameCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FrameCtx")
    }
}

impl FrameCtx {
    /// Records the first inconsistency this frame hit; the frame is then refused.
    pub(crate) fn flag_corrupt(&self, why: &'static str) {
        if self.corrupt.get().is_none() {
            self.corrupt.set(Some(why));
        }
    }

    /// A fresh context, every field at the default its thread-local
    /// predecessor was born with.
    pub(crate) fn new() -> Self {
        Self {
            // A DECODER context is seeded from the stream alone: every real
            // decode overwrites this from the parsed sequence header
            // (`set_sb128`, stream.rs). The encoder's own contexts start at
            // the size this process encodes -- see [`FrameCtx::for_encoder`].
            sb128_flag: std::cell::Cell::new(false),
            interp_filter: std::cell::Cell::new(crate::mc::InterpFilterKind::Regular),
            lossless_per_seg: std::cell::Cell::new([false; MAX_SEGMENTS]),
            disable_cdf_update: std::cell::Cell::new(false),
            cdef_bits: std::cell::Cell::new(0),
            bit_depth: std::cell::Cell::new(8),
            mono_chrome: std::cell::Cell::new(false),
            subsampling_x: std::cell::Cell::new(1),
            subsampling_y: std::cell::Cell::new(1),
            superres: std::cell::Cell::new((0, 8)),
            cdef_transmitted: std::cell::Cell::new(false),
            cdef_sb_cols: std::cell::Cell::new(0),
            capture_stages: std::cell::Cell::new(false),
            stages: std::cell::RefCell::new(None),
            cdef_idx_grid: std::cell::RefCell::new(Vec::new()),
            delta_q_present: std::cell::Cell::new(false),
            delta_q_res: std::cell::Cell::new(1),
            current_q_idx: std::cell::Cell::new(0),
            quant_deltas: std::cell::Cell::new(crate::quant::QuantDeltas {
                y_dc: 0,
                u_dc: 0,
                u_ac: 0,
                v_dc: 0,
                v_ac: 0,
            }),
            qm_levels: std::cell::Cell::new([crate::qm::NUM_QM_LEVELS as u8 - 1; 3]),
            delta_lf_present: std::cell::Cell::new(false),
            delta_lf_res: std::cell::Cell::new(1),
            delta_lf_multi: std::cell::Cell::new(false),
            current_delta_lf: std::cell::Cell::new([0; 4]),
            seg: std::cell::RefCell::new(SegmentationParams::default()),
            seg_ids: std::cell::RefCell::new(Vec::new()),
            prev_seg_ids: std::cell::RefCell::new(Vec::new()),
            seg_mi_dims: std::cell::Cell::new((0, 0)),
            above_seg_pred: std::cell::RefCell::new(Vec::new()),
            left_seg_pred: std::cell::RefCell::new(Vec::new()),
            cur_segment_id: std::cell::Cell::new(0),
            seg_tile_origin: std::cell::Cell::new((0, 0)),
            tx_select_inter: std::cell::Cell::new(false),
            tx_select_frame: std::cell::Cell::new(false),
            intra_only: std::cell::Cell::new(false),
            reduced_tx_set_inter: std::cell::Cell::new(false),
            enable_filter_intra_inter: std::cell::Cell::new(false),
            palette_pred: std::cell::RefCell::new(None),
            corrupt: std::cell::Cell::new(None),
            intrabc_mi_grid: std::cell::RefCell::new(None),
            intrabc_dv: std::cell::Cell::new(None),
            luma_tx_type: std::cell::Cell::new(TxType::DctDct),
            intrabc_chroma_tx: std::cell::Cell::new(None),
            intra_in_inter_mode: std::cell::Cell::new(None),
            inter_strip_chroma: std::cell::Cell::new(None),
            inter_last_mc: std::cell::Cell::new(None),
            enable_edge_filter: std::cell::Cell::new(false),
            last_frame_wide_margin: std::cell::RefCell::new(None),
            reach_sb_px: std::cell::Cell::new(crate::encode::SUPERBLOCK),
            grain_bit_depth: std::cell::Cell::new(8),
        }
    }

    /// A context for the ENCODER's own trial decodes: it starts at the
    /// superblock size this process encodes (`crate::encode::sb128_on`) so
    /// the mode search's `Reach` and the filter search's internal decodes
    /// agree with the tile writer. A decoder never reads that knob -- doing
    /// so coupled every decode to the encoder's process-global and raced the
    /// 128-superblock decode gates against the encoder witness.
    pub(crate) fn for_encoder() -> Self {
        let ctx = Self::new();
        let on = crate::encode::sb128_on();
        ctx.sb128_flag.set(on);
        ctx.reach_sb_px
            .set(if on { 128 } else { crate::encode::SUPERBLOCK });
        ctx
    }
}

/// `LocalKey::with` for a plain value: lets every former `STATIC.with(|c| ..)`
/// site read `fctx.field.with(|c| ..)` unchanged. `LocalKey`'s own inherent
/// `with` still wins for the scratch pools and gate counters that stay
/// thread-local.
pub(crate) trait WithRef {
    fn with<R>(&self, f: impl FnOnce(&Self) -> R) -> R {
        f(self)
    }
}
impl<T: ?Sized> WithRef for T {}

/// A [`FrameCtx`] copy for one of [`crate::par`]'s filter worker threads.
///
/// `FrameCtx` is `!Sync` on purpose (a decode's state belongs to one thread),
/// and the post-decode filter chain reads exactly these fields: the bit depth
/// (every kernel's clamp and threshold shift), the segmentation table
/// (`segment_id_at`/`lf_level` in the deblocker), the frame's superres state
/// and the film-grain bit depth. Everything else keeps [`FrameCtx::new`]'s
/// default -- a field that turns out to be read after all shows up as a hash
/// mismatch in
/// `a_real_stream_filters_identically_with_one_and_four_filter_threads`.
pub(crate) fn filter_ctx_copy(fctx: &FrameCtx) -> FrameCtx {
    let out = FrameCtx::new();
    out.bit_depth.set(fctx.bit_depth.get());
    out.mono_chrome.set(fctx.mono_chrome.get());
    out.subsampling_x.set(fctx.subsampling_x.get());
    out.subsampling_y.set(fctx.subsampling_y.get());
    out.interp_filter.set(fctx.interp_filter.get());
    out.grain_bit_depth.set(fctx.grain_bit_depth.get());
    out.superres.set(fctx.superres.get());
    out.seg_mi_dims.set(fctx.seg_mi_dims.get());
    out.seg.replace(*fctx.seg.borrow());
    out.seg_ids.replace(fctx.seg_ids.borrow().clone());
    // lane-b128m: the superblock size travels with the copy. A tile SEARCH
    // worker (`encode::search_tiles`) gets its context from here, so dropping
    // it made every worker search at 64 while the calling thread searched at
    // 128 -- the bytes a frame came out as then depended on the tile thread
    // count under `EC_AV1_SB128`.
    out.sb128_flag.set(fctx.sb128_flag.get());
    out.reach_sb_px.set(fctx.reach_sb_px.get());
    out
}

use crate::cdf;
use crate::cdf_state::{Cdfs, MvComponentCdfs, TxbSet, TxbTables};
use crate::encode::{Picture, Reach};
use crate::intra::predict;
use crate::mc;
use crate::msac::SymbolDecoder;
use crate::mvstack::{
    ALTREF_FRAME, ALTREF2_FRAME, BWDREF_FRAME, GOLDEN_FRAME, LAST_FRAME, LAST2_FRAME, LAST3_FRAME,
    MiGrid, MiInfo, NO_REF1, NO_SIGN_BIAS, NeighbourRef, SignBiasTable, comp_reference_type_ctx,
    find_mv_stack_with_sign_bias, mv16, mv32, reference_mode_ctx, single_ref_p1_ctx,
    single_ref_p2_ctx, single_ref_p3_ctx, single_ref_p4_ctx, single_ref_p5_ctx, single_ref_p6_ctx,
    uni_comp_ref_p1_ctx,
};
use crate::tile::{INTRA_MODE_CTX, block_grid, has_half};
use crate::transform::{TxType, dequant_and_inverse_typed_wh};

const PARTITION_NONE: usize = 0;
const PARTITION_HORZ: usize = 1;
const PARTITION_VERT: usize = 2;
const PARTITION_SPLIT: usize = 3;
const PARTITION_HORZ_A: usize = 4;
const PARTITION_HORZ_B: usize = 5;
const PARTITION_VERT_A: usize = 6;
const PARTITION_VERT_B: usize = 7;
const PARTITION_HORZ_4: usize = 8;
const PARTITION_VERT_4: usize = 9;
const SB_MI: u32 = 16;
const BLOCK_MI: u32 = 8;

// lane-sb128 r1: this sequence's `use_128x128_superblock` (spec 5.5.1).
// Every superblock-granular rule below (`delta_q`/`delta_lf`'s "is this the
// superblock's own top-left mi?", the loop-restoration read's unit span, the
// partition tree's own root level) keys off it; `SB_MI` stays the 64x64
// *quadrant* size a 128 superblock is decoded in, which is also what the CDEF
// grid is indexed by (libaom `read_cdef` reads one `cdef_idx` per 64x64
// inside a 128 superblock).
thread_local! {
    /// lane-frame80: `EC_TRACE_COEFF_FRAME=<decode idx>` narrows the
    /// `EC_TRACE_COEFF` rungs to a single frame in DECODE order (a 4K stream
    /// prints gigabytes otherwise). Unset = every frame, as before.
    static COEFF_TRACE_FRAME: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static COEFF_TRACE_CUR: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

/// Called by [`crate::stream::decode_stream`] before each frame's tiles with
/// that frame's DECODE-order index.
pub fn set_coeff_trace_frame(idx: usize) {
    COEFF_TRACE_FRAME.with(|c| {
        if c.get().is_none() {
            let want = match crate::envflags::var("EC_TRACE_COEFF_FRAME") {
                Ok(v) => v.parse::<usize>().unwrap_or(usize::MAX),
                // unset: keep tracing every frame, as before this rung existed
                Err(_) => usize::MAX - 1,
            };
            c.set(Some(want));
        }
    });
    COEFF_TRACE_CUR.with(|c| c.set(idx));
    if crate::envflags::env_flag!("EC_TRACE_COEFF") {
        eprintln!("EC_COEFF_FRAME decode_idx={idx}");
    }
}

pub(crate) fn coeff_trace_on() -> bool {
    if !crate::envflags::env_flag!("EC_TRACE_COEFF") {
        return false;
    }
    match COEFF_TRACE_FRAME.with(|c| c.get()) {
        // no `set_coeff_trace_frame` caller (direct tile-decode tests), or the
        // env var unset: trace every frame.
        None => true,
        Some(w) if w >= usize::MAX - 1 => true,
        Some(w) => COEFF_TRACE_CUR.with(|c| c.get()) == w,
    }
}

/// Sets [`SB128`] for this thread (called once per frame from `stream.rs`).
pub(crate) fn set_sb128(v: bool, fctx: &crate::decode::FrameCtx) {
    fctx.sb128_flag.with(|c| c.set(v));
    crate::encode::set_reach_superblock_px(if v { 128 } else { 64 }, fctx);
}

/// lane-av1mixloss: sets [`FrameCtx::lossless_per_seg`] (called once per frame
/// from `stream.rs`).
pub(crate) fn set_lossless_per_seg(table: [bool; MAX_SEGMENTS], fctx: &crate::decode::FrameCtx) {
    fctx.lossless_per_seg.with(|c| c.set(table));
}

/// The BLOCK's own lossless bit: libaom's `xd->lossless[mbmi->segment_id]`.
/// The table is computed once per frame (libaom `decodeframe.c:5205`:
/// `qindex == 0` with every plane delta 0, `av1_get_qindex` applying the
/// segment's `SEG_LVL_ALT_Q`), but the frame never carries ONE lossless rule
/// -- `read_tx_size`, `read_tx_type` and `is_cfl_allowed` all index it by the
/// BLOCK's own segment id, which the spec-order `segment_id` read sets before
/// any mode/transform symbol of the block. A frame whose segments disagree
/// codes TX_4X4/WHT blocks and normal blocks side by side.
pub(crate) fn lossless(fctx: &crate::decode::FrameCtx) -> bool {
    let seg_id = usize::from(fctx.cur_segment_id.with(|c| c.get()));
    fctx.lossless_per_seg.with(|c| c.get())[seg_id]
}

/// Sets [`FrameCtx::disable_cdf_update`] (called once per frame from
/// `stream.rs`).
pub(crate) fn set_disable_cdf_update(v: bool, fctx: &crate::decode::FrameCtx) {
    fctx.disable_cdf_update.with(|c| c.set(v));
}

/// This frame's `disable_cdf_update`, for the tile readers --
/// [`msac::SymbolDecoder::set_cdf_update`](crate::msac::SymbolDecoder::set_cdf_update).
pub(crate) fn cdf_update_disabled(fctx: &crate::decode::FrameCtx) -> bool {
    fctx.disable_cdf_update.with(|c| c.get())
}

/// The transform side a block of `side` pixels codes: its own (capped at the
/// caller's own largest) normally, 4 on a lossless BLOCK, where libaom's
/// `read_tx_size` returns TX_4X4 before it looks at anything else
/// (`decodeframe.c`: `if (xd->lossless[xd->mi[0]->segment_id]) return
/// TX_4X4;`).
pub(crate) fn lossless_tx(px: usize, fctx: &crate::decode::FrameCtx) -> usize {
    if lossless(fctx) { 4 } else { px }
}

/// `is_cfl_allowed` (libaom `blockd.h`): CfL is offered on a block up to
/// 32x32 -- but on a LOSSLESS BLOCK only when the block's CHROMA plane block
/// is exactly BLOCK_4X4, i.e. (at 4:2:0) a luma block of 8x8 or smaller.
/// Getting this wrong picks the 14-symbol CFL `uv_mode` alphabet where libaom
/// picks the 13-symbol one (class `wrong-alphabet-same-value`).
pub(crate) fn cfl_allowed_px(bw: usize, bh: usize, fctx: &crate::decode::FrameCtx) -> bool {
    if lossless(fctx) {
        // `is_cfl_allowed`: at lossless CfL is available only when the
        // partition size equals the transform size -- the CHROMA plane block
        // must be a single TX_4X4. True for an 8x8 partition at 4:2:0 (its
        // plane block is 4x4) and for 4x4 partitions; FALSE for an 8x8
        // partition at 4:4:4, whose plane block is 8x8 -- that partition
        // reads the 13-symbol uv_mode CDF, and reading the CFL alphabet
        // there desynced the tile from the leaf's own uv_mode symbol (class
        // `wrong-alphabet-same-value`).
        (bw >> ss_x(fctx)).max(4) == 4 && (bh >> ss_y(fctx)).max(4) == 4
    } else {
        bw <= 32 && bh <= 32
    }
}

fn sb128(fctx: &crate::decode::FrameCtx) -> bool {
    fctx.sb128_flag.with(|c| c.get())
}

/// This sequence's superblock size in mi units (16 = 64x64, 32 = 128x128).
pub(crate) fn sb_mi_cur(fctx: &crate::decode::FrameCtx) -> u32 {
    if sb128(fctx) { SB_MI * 2 } else { SB_MI }
}

thread_local! {
    static PART128_SYMBOLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PART128_GATHERED_SYMBOLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of `PART128_GATHERED_SYMBOLS`: 128 roots that read the
/// half-out-of-frame gathered split_or_horz/split_or_vert symbol (spec
/// 5.11.4), which this decoder used to read and throw away.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn part128_gathered_symbols() -> usize {
    PART128_GATHERED_SYMBOLS.with(|c| c.get())
}

/// Current value of [`PART128_SYMBOLS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn part128_symbols() -> usize {
    PART128_SYMBOLS.with(|c| c.get())
}

/// [`gather`] over an alphabet of any length -- the 128-level partition CDF
/// has 8 symbols (no `PARTITION_HORZ_4`/`_VERT_4` at `BLOCK_128X128`, spec
/// `partition_cdf_length`), so its forced-split gather sums 5 elements where
/// the 64/32 levels sum 6.
pub(crate) fn gather_of(cdf: &[u16], elements: &[usize]) -> [u16; 3] {
    let split: u16 = elements.iter().map(|&e| element_prob(cdf, e)).sum();
    [32768 - split, 32768, 0]
}

/// [`VERT_ALIKE`]/[`HORZ_ALIKE`] without the 1:4 arms, for the 128 root.
pub(crate) const VERT_ALIKE128: [usize; 5] = [2, PARTITION_SPLIT, 4, 6, 7];
pub(crate) const HORZ_ALIKE128: [usize; 5] = [1, PARTITION_SPLIT, 4, 5, 6];

// lane-realworld r1: this frame's `cdef.bits` (spec 5.9.19), threaded into
// [`maybe_read_cdef_idx`] the same way [`ENABLE_EDGE_FILTER`] threads a
// frame-level flag through the recursive block decode -- `0` (the default)
// makes every call a true no-op, matching every existing gate stream's
// `cdef_bits == 0` behaviour exactly.
// lane-realworld r8: the sequence header's `color_config.bit_depth`, set
// once per `decode_key_frame_tile_with_cdfs`/`decode_inter_frame_tile_with_cdfs`
// call the same way `CDEF_BITS` is above. `stream.rs`'s own refusal still
// blocks every non-8-bit stream from reaching a decode call, so this is `8`
// on every path that runs today -- threading it now (rather than the literal
// `8`) is stage 1 of widening past that refusal without re-deriving every
// clamp bound later. `PlaneBuf`'s sample storage stays `u16` regardless of
// depth (this decoder's own internal choice, matches dav1d/libaom).
/// `(1 << bit_depth) - 1` — the reconstruction clamp bound for the current
/// stream's [`BIT_DEPTH`], replacing the crate-wide literal `255`.
pub(crate) fn sample_max(fctx: &crate::decode::FrameCtx) -> i32 {
    (1i32 << fctx.bit_depth.with(std::cell::Cell::get)) - 1
}
/// The current stream's bit depth, for callers (e.g. dequant) that need the
/// raw value rather than the derived clamp bound.
pub(crate) fn bit_depth(fctx: &crate::decode::FrameCtx) -> u8 {
    fctx.bit_depth.with(std::cell::Cell::get)
}
/// Sets [`BIT_DEPTH`] for the current thread, called once per frame from
/// [`crate::stream`]'s decode loop.
pub(crate) fn set_bit_depth(bit_depth: u8, fctx: &crate::decode::FrameCtx) {
    fctx.bit_depth.with(|c| c.set(bit_depth));
}

/// This stream's `mono_chrome` (see [`FrameCtx::mono_chrome`]): a
/// monochrome frame has `NumPlanes == 1`, so no block codes any chroma
/// symbol or chroma coefficient and the output picture is luma only.
pub(crate) fn mono(fctx: &crate::decode::FrameCtx) -> bool {
    fctx.mono_chrome.with(std::cell::Cell::get)
}

/// Sets [`mono`] for the current thread, called once per frame from
/// [`crate::stream`]'s decode loop like [`set_bit_depth`].
pub(crate) fn set_mono(mono_chrome: bool, fctx: &crate::decode::FrameCtx) {
    fctx.mono_chrome.with(|c| c.set(mono_chrome));
}

/// Sequence subsampling. Set once per frame from stream.rs, like set_mono.
pub(crate) fn set_subsampling(ss_x: u8, ss_y: u8, fctx: &crate::decode::FrameCtx) {
    fctx.subsampling_x.set(ss_x);
    fctx.subsampling_y.set(ss_y);
}

#[inline]
pub(crate) fn ss_x(fctx: &crate::decode::FrameCtx) -> usize {
    fctx.subsampling_x.get() as usize
}

#[inline]
pub(crate) fn ss_y(fctx: &crate::decode::FrameCtx) -> usize {
    fctx.subsampling_y.get() as usize
}

/// libaom ROUND_POWER_OF_TWO. At ss=1 equals div_ceil(dim, 2); at ss=0 is dim.
#[inline]
pub(crate) fn round_ss(dim: usize, ss: usize) -> usize {
    (dim + ((1usize << ss) >> 1)) >> ss
}

/// This frame's superres state: `(upscaled_width, superres_denom)`, or
/// `(0, 8)` when the frame is not scaled. Spec 7.16's upscale runs
/// between CDEF and loop restoration, so LOOP RESTORATION -- unit grid,
/// `read_lr`'s unit corners and the pixel filters themselves -- works in
/// UPSCALED coordinates while deblock and CDEF work in downscaled ones
/// (libaom `av1_superres_upscale` in `decodeframe.c`, and
/// `av1_loop_restoration_corners_in_sb`'s own `mi_to_num_x`/`denom_x`
/// superres arms). Set once per frame from [`crate::stream`]'s decode
/// loop, like [`BIT_DEPTH`], and reset to `(0, 8)` on an unscaled frame
/// so a later frame never inherits it.
/// Sets [`SUPERRES`] for the current thread. `upscaled_width` is 0 (and
/// `denom` 8) for a frame that is not superres-scaled.
pub(crate) fn set_superres(upscaled_width: u32, denom: u32, fctx: &crate::decode::FrameCtx) {
    fctx.superres.with(|c| c.set((upscaled_width, denom)));
}

/// `(upscaled_width, superres_denom)` when this frame is superres-scaled
/// (libaom `av1_superres_scaled`), else `None`.
pub(crate) fn superres(fctx: &crate::decode::FrameCtx) -> Option<(u32, u32)> {
    let (w, d) = fctx.superres.with(std::cell::Cell::get);
    (w > 0 && d != 8).then_some((w, d))
}

/// `av1_init_warp_params` (libaom reconinter.c): warp is decided per PLANE and
/// bails out at `block_width < 8 || block_height < 8`, so the plane's OWN
/// block dimensions decide it -- never the square `side`/`chroma_side`, which
/// for a rectangular luma block (8x16, 16x8) or a PARTITION_HORZ/VERT strip is
/// bigger than the real block in one axis.
pub(crate) fn warp_plane_allowed(w: usize, h: usize) -> bool {
    if w >= 8 && h >= 8 {
        return true;
    }
    hit!(WARP_PLANE_SUPPRESS_HITS);
    false
}

/// This frame's [`QUANT_DELTAS`] `(dc, ac)` pair for plane `plane_idx` (0 =
/// Y, 1 = U, 2 = V) -- luma has no coded `ac` delta (spec 5.9.12 never reads
/// one), so it is always `0` here.
fn plane_q_delta(plane_idx: usize, fctx: &crate::decode::FrameCtx) -> (i32, i32) {
    let d = fctx.quant_deltas.with(|c| c.get());
    match plane_idx {
        0 => (d.y_dc, 0),
        1 => (d.u_dc, d.u_ac),
        _ => (d.v_dc, d.v_ac),
    }
}

/// lane-av1-qmatrix: records this FRAME's quantisation-matrix levels
/// ([`crate::decode::FrameCtx::qm_levels`]) from the parsed
/// `quantization_params`. Called from `stream.rs` at the frame header, where
/// the refusal this lane lifted used to sit.
pub(crate) fn set_qm_levels(levels: [u8; 3], fctx: &crate::decode::FrameCtx) {
    fctx.qm_levels.with(|c| c.set(levels));
}

/// This block's inverse quantisation matrix -- libaom `av1_get_iqmatrix`
/// (`av1/common/quant_common.c:243`) plus `setup_segmentation_dequant`'s
/// `av1_use_qmatrix` gate, evaluated per dequantisation call site the same
/// way the site already evaluates its quantizer index. `None` dequantises
/// flat (the only behaviour before lane-av1-qmatrix):
/// - a level of 15, which is both libaom's flat sentinel and what every
///   `using_qmatrix == 0` frame's levels are parked at;
/// - a 1D/identity transform (`is_2d_transform` false: libaom's
///   `av1_get_iqmatrix` reads the NULL slot for those);
/// - a LOSSLESS block (libaom `av1_use_qmatrix`'s `!xd->lossless[seg]`: the
///   segment-adjusted quantizer index clamped to 0 with all five plane
///   deltas zero -- the same `block_q_idx` every other dequant site reads).
fn block_iqmatrix(
    fctx: &crate::decode::FrameCtx,
    plane_idx: usize,
    w: usize,
    h: usize,
    tx_type: TxType,
) -> Option<crate::qm::Iqm> {
    let levels = fctx.qm_levels.with(|c| c.get());
    let level = levels[plane_idx.min(2)];
    if usize::from(level) >= crate::qm::NUM_QM_LEVELS - 1 {
        return None;
    }
    if !tx_type.is_2d_transform() {
        return None;
    }
    let d = fctx.quant_deltas.with(|c| c.get());
    if block_q_idx(fctx) == 0
        && d.y_dc == 0
        && d.u_dc == 0
        && d.u_ac == 0
        && d.v_dc == 0
        && d.v_ac == 0
    {
        return None;
    }
    crate::qm::iwt_matrix(usize::from(level), plane_idx != 0, w, h)
}

/// `read_delta_qindex`/`read_delta_q_params` (spec 5.11.10, libaom
/// `decodemv.c:85-146,735-770`), called right after [`maybe_read_cdef_idx`]
/// at every block-decode call site (spec order `skip -> cdef -> delta_q`).
/// A no-op when `delta_q_present` is unset, or when this block is not at the
/// superblock's own top-left MI position (`b_row == 0 && b_col == 0` in
/// libaom, collapsed here to a position check since a partition tree's
/// top-left leaf is the only block that can ever land there), or when that
/// leaf both *is* the whole superblock and is `skip` (the one case libaom
/// itself skips the read, state carrying over unchanged).
fn maybe_read_delta_q(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    mi_r: usize,
    mi_c: usize,
    is_whole_sb: bool,
    skip: bool,
    fctx: &crate::decode::FrameCtx,
) {
    if !fctx.delta_q_present.with(|c| c.get()) {
        return;
    }
    let sb_mi = sb_mi_cur(fctx) as usize;
    if !mi_r.is_multiple_of(sb_mi) || !mi_c.is_multiple_of(sb_mi) {
        return;
    }
    // `is_whole_sb` is the caller's "this leaf IS the superblock" flag,
    // computed against the sequence's own superblock size (lane-sb128b r1:
    // it used to be hardcoded against 64 and disabled under `sb128`, which a
    // `PARTITION_NONE` 128x128 root -- itself the whole superblock -- makes
    // wrong).
    if is_whole_sb && skip {
        return;
    }
    let mut abs = dec.symbol(&mut cdfs.delta_q) as i32;
    const DELTA_Q_SMALL: i32 = 3;
    if abs >= DELTA_Q_SMALL {
        let rem_bits = dec.literal(3) + 1;
        let thr = (1i32 << rem_bits) + 1;
        abs = dec.literal(rem_bits) as i32 + thr;
    }
    let sign_negative = if abs != 0 { dec.literal(1) != 0 } else { true };
    let reduced = if sign_negative { -abs } else { abs };
    hit!(DELTA_Q_HITS);
    let res = fctx.delta_q_res.with(|c| c.get());
    fctx.current_q_idx
        .with(|c| c.set((c.get() + reduced * res).clamp(1, 255)));
}

/// `read_delta_lflevel`/`read_delta_lf_params` (spec 5.11.11, libaom
/// `decodemv.c:99-146,735-770`) -- same shape and same call-site gating as
/// [`maybe_read_delta_q`] (spec order `delta_q -> delta_lf`), looped over
/// `FRAME_LF_COUNT = 4` planes/directions when `delta_lf_multi` is set, or
/// read once into index 0 otherwise.
fn maybe_read_delta_lf(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    mi_r: usize,
    mi_c: usize,
    is_whole_sb: bool,
    skip: bool,
    fctx: &crate::decode::FrameCtx,
) {
    if !fctx.delta_lf_present.with(|c| c.get()) {
        return;
    }
    let sb_mi = sb_mi_cur(fctx) as usize;
    if !mi_r.is_multiple_of(sb_mi) || !mi_c.is_multiple_of(sb_mi) {
        return;
    }
    // `is_whole_sb` is the caller's "this leaf IS the superblock" flag,
    // computed against the sequence's own superblock size (lane-sb128b r1:
    // it used to be hardcoded against 64 and disabled under `sb128`, which a
    // `PARTITION_NONE` 128x128 root -- itself the whole superblock -- makes
    // wrong).
    if is_whole_sb && skip {
        return;
    }
    let multi = fctx.delta_lf_multi.with(|c| c.get());
    let res = fctx.delta_lf_res.with(|c| c.get());
    let count = if multi { 4 } else { 1 };
    let mut cur = fctx.current_delta_lf.with(|c| c.get());
    // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
    #[allow(clippy::needless_range_loop)]
    for i in 0..count {
        let mut abs = if multi {
            dec.symbol(&mut cdfs.delta_lf_multi[i]) as i32
        } else {
            dec.symbol(&mut cdfs.delta_lf) as i32
        };
        const DELTA_LF_SMALL: i32 = 3;
        if abs >= DELTA_LF_SMALL {
            let rem_bits = dec.literal(3) + 1;
            let thr = (1i32 << rem_bits) + 1;
            abs = dec.literal(rem_bits) as i32 + thr;
        }
        let sign_negative = if abs != 0 { dec.literal(1) != 0 } else { true };
        let reduced = if sign_negative { -abs } else { abs };
        cur[i] = (cur[i] + reduced * res).clamp(-63, 63);
        hit!(DELTA_LF_HITS);
    }
    fctx.current_delta_lf.with(|c| c.set(cur));
}

/// `read_cdef` (spec 5.11.56, libaom `decodemv.c:read_cdef`), called right
/// after `skip` is known at every block-decode call site. A no-op at
/// `cdef.bits == 0` (0-bit literal, nothing to consume) -- and
/// `read_cdef_params` already forces `bits = 0` under `coded_lossless`/
/// `allow_intrabc`, so neither needs a separate check here. Reads once per
/// superblock, at the first non-skip block.
fn maybe_read_cdef_idx(
    dec: &mut SymbolDecoder,
    mi_r: usize,
    mi_c: usize,
    // The reading BLOCK's own span in 4x4 mode-info units: a block wider or
    // taller than one 64x64 CDEF unit is the first non-skip block of EVERY
    // unit it covers, so its one literal drives all of them (lane-b128r).
    (w_mi, h_mi): (usize, usize),
    skip: bool,
    fctx: &crate::decode::FrameCtx,
) {
    let bits = fctx.cdef_bits.with(|c| c.get());
    if bits == 0 || skip || fctx.cdef_transmitted.with(|c| c.get()) {
        return;
    }
    let idx = dec.literal(u32::from(bits)) as u8;
    fctx.cdef_transmitted.with(|c| c.set(true));
    hit!(CDEF_IDX_HITS);
    let (sb_r, sb_c) = (mi_r / SB_MI as usize, mi_c / SB_MI as usize);
    let sb_cols = fctx.cdef_sb_cols.with(|c| c.get());
    if sb_cols > 0 {
        // lane-b128r: libaom's four CDEF units of a 128 superblock all read
        // their strength off the MB_MODE_INFO of the first non-skip block in
        // their own unit -- and a block that COVERS several units (the 128
        // root's own 128x128 block, the only one there is) is that block for
        // every unit it covers, so one literal drives all four. This grid is
        // per 64x64 unit, so the read stamps every unit the block covers; one
        // that is smaller than a unit stamps exactly the cell it always did.
        let (rows, cols) = (
            (h_mi / SB_MI as usize).max(1),
            (w_mi / SB_MI as usize).max(1),
        );
        fctx.cdef_idx_grid.with(|g| {
            let mut g = g.borrow_mut();
            for dr in 0..rows {
                for dc in 0..cols {
                    let i = (sb_r + dr) * sb_cols + sb_c + dc;
                    if sb_c + dc < sb_cols && i < g.len() {
                        g[i] = idx;
                    }
                }
            }
        });
    }
}

thread_local! {
    static SEG_IDS_SEEN: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// lane-golomb2: the (lowest, highest) quantizer index any block has been
    /// dequantized with, i.e. `block_q_idx`'s own span. A gate that means to
    /// exercise `SEG_LVL_ALT_Q` needs a stream where this span is WIDER than
    /// zero (class `gate-blind-to-feature`): segmentation being enabled says
    /// nothing about the segments carrying distinct quantizers.
    static BLOCK_Q_SPAN: std::cell::Cell<(i32, i32)> = const { std::cell::Cell::new((i32::MAX, i32::MIN)) };
}

/// Installs this frame's segmentation parameters and allocates its map, with
/// `prev` the primary reference frame's saved map (spec
/// `load_previous_segment_ids`; `None` for `PRIMARY_REF_NONE` or an empty
/// slot). Called once per frame, whether or not segmentation is enabled.
pub(crate) fn set_segmentation(
    seg: SegmentationParams,
    mi_rows: usize,
    mi_cols: usize,
    prev: Option<&[u8]>,
    fctx: &crate::decode::FrameCtx,
) {
    if crate::envflags::env_flag!("EC_HDR") {
        eprintln!(
            "EC_SEGHDR enabled={} update_map={} temporal={} preskip={} last_active={} data={:?}",
            seg.enabled,
            seg.update_map,
            seg.temporal_update,
            seg.seg_id_pre_skip,
            seg.last_active_seg_id,
            seg.feature_data
        );
    }
    fctx.seg.with(|s| *s.borrow_mut() = seg);
    fctx.seg_mi_dims.with(|c| c.set((mi_rows, mi_cols)));
    fctx.seg_ids
        .with(|m| *m.borrow_mut() = vec![0u8; mi_rows * mi_cols]);
    fctx.prev_seg_ids.with(|m| {
        let mut m = m.borrow_mut();
        m.clear();
        if let Some(p) = prev
            && p.len() == mi_rows * mi_cols
        {
            m.extend_from_slice(p);
        }
    });
    fctx.above_seg_pred
        .with(|m| *m.borrow_mut() = vec![0u8; mi_cols]);
    fctx.left_seg_pred
        .with(|m| *m.borrow_mut() = vec![0u8; mi_rows]);
    fctx.cur_segment_id.with(|c| c.set(0));
}

/// This frame's decoded segment map, to be stored into whichever reference
/// slots `refresh_frame_flags` names (spec 7.20's `SavedSegmentIds`).
pub(crate) fn take_segment_ids(fctx: &crate::decode::FrameCtx) -> Vec<u8> {
    fctx.seg_ids.with(|m| m.borrow().clone())
}

/// How many distinct segment ids any decoded block has carried.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn segment_ids_seen() -> usize {
    SEG_IDS_SEEN.with(|c| c.get().count_ones() as usize)
}

/// The lowest and highest quantizer index any decoded block was dequantized
/// with (`(i32::MAX, i32::MIN)` when nothing decoded).
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn block_q_span() -> (i32, i32) {
    BLOCK_Q_SPAN.with(|c| c.get())
}

/// `seg_feature_active_idx(segment_id, feature)` for the frame currently
/// being decoded, with its `FeatureData` value.
fn seg_feature(segment_id: usize, feature: usize, fctx: &crate::decode::FrameCtx) -> Option<i32> {
    fctx.seg.with(|s| {
        let s = s.borrow();
        s.feature_active(segment_id, feature)
            .then(|| i32::from(s.feature_data[segment_id][feature]))
    })
}

/// `get_qindex(ignoreDeltaQ = 0, segment_id)` (spec 7.12.2, libaom
/// `av1_get_qindex`) for the block currently being decoded: the running
/// `CurrentQIndex` shifted by this block's segment's `SEG_LVL_ALT_Q` data.
/// Every dequant call site reads the block's quantizer through here.
fn block_q_idx(fctx: &crate::decode::FrameCtx) -> i32 {
    let base = fctx.current_q_idx.with(|c| c.get());
    let seg_id = fctx.cur_segment_id.with(|c| c.get()) as usize;
    let q = match seg_feature(seg_id, SEG_LVL_ALT_Q, fctx) {
        Some(data) => (base + data).clamp(0, 255),
        None => base,
    };
    // lane-d792: which SEGMENT a block's quantizer came out of, beside the
    // `EC_DQCOEFF` grid rung -- a dequant that is too low with the levels
    // already proven equal is a segment-id defect, not a quantizer one.
    if crate::envflags::env_flag!("EC_DQCOEFF") {
        eprintln!("OUR_Q base={base} seg={seg_id} q={q}");
    }
    hit_do!(BLOCK_Q_SPAN.with(|c| {
        let (lo, hi) = c.get();
        c.set((lo.min(q), hi.max(q)));
    }););
    q
}

/// The segment id recorded for one mi cell of the current frame's map.
fn segment_id_at(mi_r: usize, mi_c: usize, fctx: &crate::decode::FrameCtx) -> u8 {
    let (mi_rows, mi_cols) = fctx.seg_mi_dims.with(|c| c.get());
    if mi_r >= mi_rows || mi_c >= mi_cols {
        return 0;
    }
    fctx.seg_ids.with(|m| m.borrow()[mi_r * mi_cols + mi_c])
}

/// `set_segment_id`: stamps `id` over every mi cell this block covers.
fn write_segment_id(
    mi_r: usize,
    mi_c: usize,
    w_mi: usize,
    h_mi: usize,
    id: u8,
    fctx: &crate::decode::FrameCtx,
) {
    let (mi_rows, mi_cols) = fctx.seg_mi_dims.with(|c| c.get());
    let (x_mis, y_mis) = (
        w_mi.min(mi_cols.saturating_sub(mi_c)),
        h_mi.min(mi_rows.saturating_sub(mi_r)),
    );
    fctx.seg_ids.with(|m| {
        let mut m = m.borrow_mut();
        for y in 0..y_mis {
            for x in 0..x_mis {
                m[(mi_r + y) * mi_cols + mi_c + x] = id;
            }
        }
    });
    fctx.cur_segment_id.with(|c| c.set(id));
    SEG_IDS_SEEN.with(|c| c.set(c.get() | 1 << id));
}

/// `copy_segment_id` (libaom `decodemv.c:342`): the previous frame's map is
/// inherited VERBATIM over this block's own mi footprint -- per-cell copies,
/// not the minified id [`predicted_segment_id`] hands the block. Without a
/// previous map the footprint is zeroed, exactly as the C NULL branch does.
fn copy_segment_ids(
    mi_r: usize,
    mi_c: usize,
    w_mi: usize,
    h_mi: usize,
    fctx: &crate::decode::FrameCtx,
) {
    let (mi_rows, mi_cols) = fctx.seg_mi_dims.with(|c| c.get());
    let (x_mis, y_mis) = (
        w_mi.min(mi_cols.saturating_sub(mi_c)),
        h_mi.min(mi_rows.saturating_sub(mi_r)),
    );
    fctx.seg_ids.with(|m| {
        let mut m = m.borrow_mut();
        fctx.prev_seg_ids.with(|p| {
            let p = p.borrow();
            for y in 0..y_mis {
                for x in 0..x_mis {
                    let idx = (mi_r + y) * mi_cols + mi_c + x;
                    m[idx] = p.get(idx).copied().unwrap_or(0);
                }
            }
        });
    });
}

/// `get_predicted_segment_id` (libaom `dec_get_segment_id` over
/// `last_frame_seg_map`): the minimum previous-frame segment id over the
/// block's own mi footprint. `0` when there is no previous map.
fn predicted_segment_id(
    mi_r: usize,
    mi_c: usize,
    w_mi: usize,
    h_mi: usize,
    fctx: &crate::decode::FrameCtx,
) -> u8 {
    let (mi_rows, mi_cols) = fctx.seg_mi_dims.with(|c| c.get());
    fctx.prev_seg_ids.with(|m| {
        let m = m.borrow();
        if m.is_empty() {
            return 0;
        }
        let (x_mis, y_mis) = (
            w_mi.min(mi_cols.saturating_sub(mi_c)),
            h_mi.min(mi_rows.saturating_sub(mi_r)),
        );
        let mut id = u8::MAX;
        for y in 0..y_mis {
            for x in 0..x_mis {
                id = id.min(m[(mi_r + y) * mi_cols + mi_c + x]);
            }
        }
        if id == u8::MAX { 0 } else { id }
    })
}

/// `av1_neg_deinterleave` (libaom `decodemv.c:258`).
fn neg_deinterleave(diff: i32, reference: i32, max: i32) -> i32 {
    if reference == 0 {
        return diff;
    }
    if reference >= max - 1 {
        return max - diff - 1;
    }
    let half = if 2 * reference < max {
        diff <= 2 * reference
    } else {
        diff <= 2 * (max - reference - 1)
    };
    if half {
        return if diff & 1 != 0 {
            reference + ((diff + 1) >> 1)
        } else {
            reference - (diff >> 1)
        };
    }
    if 2 * reference < max {
        diff
    } else {
        max - (diff + 1)
    }
}

/// `av1_get_spatial_seg_pred`: `(prediction, cdf_index)` from the current
/// frame's already-decoded above/left/above-left neighbours.
fn spatial_seg_pred(mi_r: usize, mi_c: usize, fctx: &crate::decode::FrameCtx) -> (i32, usize) {
    let (row0, col0) = fctx.seg_tile_origin.with(|c| c.get());
    let (avail_u, avail_l) = (mi_r > row0, mi_c > col0);
    let prev_ul = (avail_u && avail_l).then(|| i32::from(segment_id_at(mi_r - 1, mi_c - 1, fctx)));
    let prev_u = avail_u.then(|| i32::from(segment_id_at(mi_r - 1, mi_c, fctx)));
    let prev_l = avail_l.then(|| i32::from(segment_id_at(mi_r, mi_c - 1, fctx)));
    let ctx = match prev_ul {
        None => 0,
        Some(ul) if Some(ul) == prev_u && Some(ul) == prev_l => 2,
        Some(ul) if Some(ul) == prev_u || Some(ul) == prev_l || prev_u == prev_l => 1,
        Some(_) => 0,
    };
    let pred = match (prev_u, prev_l) {
        (None, l) => l.unwrap_or(0),
        (Some(u), None) => u,
        (Some(u), Some(l)) => {
            if prev_ul == Some(u) {
                u
            } else {
                l
            }
        }
    };
    (pred, ctx)
}

/// `read_segment_id` (libaom `decodemv.c:280`): the spatially predicted id
/// when `skip`, otherwise one `segment_id` symbol de-interleaved against
/// that prediction. Does NOT write the map -- callers do.
fn read_segment_id(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    mi_r: usize,
    mi_c: usize,
    skip: bool,
    fctx: &crate::decode::FrameCtx,
) -> u8 {
    let (pred, ctx) = spatial_seg_pred(mi_r, mi_c, fctx);
    if skip {
        return pred as u8;
    }
    let coded = dec.symbol(&mut cdfs.segment_id[ctx]) as i32;
    hit!(SEG_ID_HITS);
    let last_active = fctx.seg.with(|s| i32::from(s.borrow().last_active_seg_id));
    let id = neg_deinterleave(coded, pred, last_active + 1).clamp(0, last_active);
    // Mirrors the oracle's own `EC_SEG mi_row=.. cdf=.. pred=.. coded=.. id=..`
    // (libaom `decodemv.c:read_segment_id`), for a cross-decoder segment diff.
    if crate::envflags::env_flag!("EC_TRACE_SEG") {
        eprintln!(
            "EC_SEG mi_row={mi_r} mi_col={mi_c} cdf={ctx} pred={pred} coded={coded} id={id} rng={}",
            dec.debug_state().0
        );
    }
    id as u8
}

/// `read_intra_segment_id` (spec 5.11.7): called twice per intra-frame block
/// -- once before `skip` when `SegIdPreSkip`, once after it otherwise (the
/// caller decides which by passing `pre_skip`).
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
fn intra_segment_id(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    mi_r: usize,
    mi_c: usize,
    w_mi: usize,
    h_mi: usize,
    skip: bool,
    fctx: &crate::decode::FrameCtx,
) {
    if !fctx.seg.with(|s| s.borrow().enabled) {
        return;
    }
    let id = read_segment_id(dec, cdfs, mi_r, mi_c, skip, fctx);
    write_segment_id(mi_r, mi_c, w_mi, h_mi, id, fctx);
}

/// Whether this frame's intra blocks read their `segment_id` before `skip`
/// (`SegIdPreSkip`); `false` also when segmentation is off.
fn seg_id_pre_skip(fctx: &crate::decode::FrameCtx) -> bool {
    fctx.seg.with(|s| {
        let s = s.borrow();
        s.enabled && s.seg_id_pre_skip
    })
}

/// `read_inter_segment_id(preskip)` (spec 5.11.9, libaom `decodemv.c:352`).
/// Called at both spec positions of an inter frame's block: `pre_skip = true`
/// before `skip_mode`/`skip`, and `pre_skip = false` after them.
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
fn inter_segment_id(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    mi_r: usize,
    mi_c: usize,
    w_mi: usize,
    h_mi: usize,
    skip: bool,
    pre_skip: bool,
    fctx: &crate::decode::FrameCtx,
) {
    let (enabled, update_map, temporal_update, pre_skip_frame) = fctx.seg.with(|s| {
        let s = s.borrow();
        (
            s.enabled,
            s.update_map,
            s.temporal_update,
            s.seg_id_pre_skip,
        )
    });
    if !enabled {
        return;
    }
    if !update_map {
        // The map is inherited from the previous frame, and libaom's
        // `copy_segment_id` (decodemv.c:342) does that VERBATIM -- a memcpy of
        // the previous map over the block's own footprint, mixed ids intact --
        // while the id the BLOCK itself carries is the min over that footprint
        // (`get_predicted_segment_id`). Stamping the min here flattened mixed
        // footprints into the saved map, and every later frame whose
        // `update_map == 0` copy or temporal prediction read that position
        // dequantized with the wrong segment's `SEG_LVL_ALT_Q` (class
        // `simplified-inheritance`).
        copy_segment_ids(mi_r, mi_c, w_mi, h_mi, fctx);
        let id = predicted_segment_id(mi_r, mi_c, w_mi, h_mi, fctx);
        fctx.cur_segment_id.with(|c| c.set(id));
        SEG_IDS_SEEN.with(|c| c.set(c.get() | 1 << id));
        return;
    }
    if pre_skip && !pre_skip_frame {
        return;
    }
    if !pre_skip && skip {
        // libaom: a skipped block never codes `seg_id_predicted`, and its
        // segment id is the spatial prediction.
        stamp_seg_pred(mi_r, mi_c, w_mi, h_mi, 0, fctx);
        let id = read_segment_id(dec, cdfs, mi_r, mi_c, true, fctx);
        write_segment_id(mi_r, mi_c, w_mi, h_mi, id, fctx);
        return;
    }
    let id = if temporal_update {
        let ctx = seg_pred_ctx(mi_r, mi_c, fctx);
        let predicted = dec.symbol(&mut cdfs.segment_pred[ctx]) != 0;
        hit!(SEG_PRED_HITS);
        stamp_seg_pred(mi_r, mi_c, w_mi, h_mi, u8::from(predicted), fctx);
        if predicted {
            predicted_segment_id(mi_r, mi_c, w_mi, h_mi, fctx)
        } else {
            read_segment_id(dec, cdfs, mi_r, mi_c, false, fctx)
        }
    } else {
        read_segment_id(dec, cdfs, mi_r, mi_c, false, fctx)
    };
    write_segment_id(mi_r, mi_c, w_mi, h_mi, id, fctx);
}

/// `av1_get_pred_context_seg_id`: above + left `seg_id_predicted` flags.
fn seg_pred_ctx(mi_r: usize, mi_c: usize, fctx: &crate::decode::FrameCtx) -> usize {
    // libaom reads the flag off `above_mbmi`/`left_mbmi`, which are NULL (and
    // so contribute 0) outside the tile -- the availability check here is
    // what keeps a previous tile's stamps out of this tile's context.
    let (row0, col0) = fctx.seg_tile_origin.with(|c| c.get());
    let above = if mi_r > row0 {
        fctx.above_seg_pred
            .with(|m| m.borrow().get(mi_c).copied().unwrap_or(0))
    } else {
        0
    };
    let left = if mi_c > col0 {
        fctx.left_seg_pred
            .with(|m| m.borrow().get(mi_r).copied().unwrap_or(0))
    } else {
        0
    };
    usize::from(above + left)
}

/// Spec 5.11.9's `AboveSegPredContext`/`LeftSegPredContext` update.
fn stamp_seg_pred(
    mi_r: usize,
    mi_c: usize,
    w_mi: usize,
    h_mi: usize,
    value: u8,
    fctx: &crate::decode::FrameCtx,
) {
    fctx.above_seg_pred.with(|m| {
        let mut m = m.borrow_mut();
        for i in 0..w_mi {
            if let Some(cell) = m.get_mut(mi_c + i) {
                *cell = value;
            }
        }
    });
    fctx.left_seg_pred.with(|m| {
        let mut m = m.borrow_mut();
        for i in 0..h_mi {
            if let Some(cell) = m.get_mut(mi_r + i) {
                *cell = value;
            }
        }
    });
}

/// [`Reach::of_tu`] for one transform unit, counting how often that per-unit
/// answer differs from the standalone-block answer the call sites used before
/// lane-palette2 r12 -- the counter a gate asserts on to prove a stream really
/// exercised the fix. `col_off`/`row_off` are in SAMPLES from the parent
/// block's top-left `(px, py)`.
#[allow(clippy::too_many_arguments)]
fn tu_reach(
    bw: usize,
    bh: usize,
    col_off: usize,
    row_off: usize,
    tx: usize,
    block: Reach,
    px: usize,
    py: usize,
    width: usize,
    height: usize,
    fctx: &crate::decode::FrameCtx,
) -> Reach {
    let fixed = Reach::of_tu(bw, bh, col_off, row_off, tx, tx, block);
    let standalone = Reach::of(tx, px + col_off, py + row_off, width, height, fctx);
    if fixed != standalone {
        hit!(SPLIT_TU_REACH_FIX_HITS);
    }
    fixed
}

/// [`tu_reach`] for a unit whose LUMA footprint is a `tx_w` x `tx_h` rect
/// (lane-av1-422b: a 4:2:2 chroma unit is square in chroma pixels, but its
/// luma anchor is `(chroma_tx << ss_x, chroma_tx << ss_y)` -- at ss (1,0)
/// twice as wide as it is tall). Same predicate as [`tu_reach`]; the
/// square mismatch counter only means anything when the footprint actually
/// is square.
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
fn tu_reach_rect(
    bw: usize,
    bh: usize,
    col_off: usize,
    row_off: usize,
    tx_w: usize,
    tx_h: usize,
    block: Reach,
    px: usize,
    py: usize,
    width: usize,
    height: usize,
    fctx: &crate::decode::FrameCtx,
) -> Reach {
    let fixed = Reach::of_tu(bw, bh, col_off, row_off, tx_w, tx_h, block);
    if tx_w == tx_h {
        let standalone = Reach::of(tx_w, px + col_off, py + row_off, width, height, fctx);
        if fixed != standalone {
            hit!(SPLIT_TU_REACH_FIX_HITS);
        }
    }
    fixed
}

/// lane-comppin r4: decode-order picture counter for `EC_AV1_PREFILT_DUMP`,
/// mirroring stream.rs's `pictures_decoded` so the two dumps index
/// identically against aomdec's own `ec_frame_idx` in decodeframe.c.
static PREFILT_PICTURE_IDX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Records one decoded forward keyframe (see [`FWD_KF_HITS`]).
pub(crate) fn note_fwd_kf() {
    hit!(FWD_KF_HITS);
}

thread_local! {
    static TXFM_SPLIT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`TXFM_SPLIT_READS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn txfm_split_reads() -> usize {
    TXFM_SPLIT_READS.with(|c| c.get())
}

/// Sets the current thread's [`TX_SELECT_INTER`]/[`REDUCED_TX_SET_INTER`]/
/// [`ENABLE_FILTER_INTRA_INTER`].
pub(crate) fn set_inter_tx_mode(
    tx_select: bool,
    reduced_tx_set: bool,
    enable_filter_intra: bool,
    fctx: &crate::decode::FrameCtx,
) {
    fctx.tx_select_inter.with(|c| c.set(tx_select));
    fctx.intra_only.with(|c| c.set(false));
    fctx.tx_select_frame.with(|c| c.set(tx_select));
    fctx.reduced_tx_set_inter.with(|c| c.set(reduced_tx_set));
    fctx.enable_filter_intra_inter
        .with(|c| c.set(enable_filter_intra));
}

fn bump_ab16(arm: usize) {
    hit_do! {
        AB16_HITS.with(|c| {
            let mut v = c.get();
            v[arm] += 1;
            c.set(v);
        });
    }
}

/// Bumps one bucket of [`INTER_EDGE_STRIP_HITS`].
pub(crate) fn bump_inter_edge_strip(idx: usize) {
    hit_do! {
        INTER_EDGE_STRIP_HITS.with(|c| {
            let mut v = c.get();
            v[idx] += 1;
            c.set(v);
        });
    }
}

fn bump_edge_both_cut(level: usize) {
    hit_do! {
        EDGE_BOTH_CUT_HITS.with(|c| {
            let mut v = c.get();
            v[level] += 1;
            c.set(v);
        });
    }
}

// lane-av1muchroma site 1: the total number of samples by which
// `decode_block`'s SKIP arm chroma bookkeeping grids fell SHORT of their own
// plane block, summed over every skipped block decoded. It reads the
// allocation's real length, so it measures the shape rather than proxying a
// condition: the square form this lane removed is `side >> ss_x` squared,
// which at 4:2:2 (ss (1,0)) is exactly HALF the `side >> ss_x` x `side >> ss_y`
// plane block, and restoring it drives this counter strictly positive on
// every 4:2:2 skip block. Zero on 4:2:0 and 4:4:4, where the two forms are
// the same number.
thread_local! {
    static SKIP_ARM_CHROMA_SHORT_SAMPLES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SKIP_ARM_CHROMA_SHORT_SAMPLES`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn skip_arm_chroma_short_samples() -> usize {
    SKIP_ARM_CHROMA_SHORT_SAMPLES.with(|c| c.get())
}

// Number of times `decode_block`'s SKIP arm ran at a shape where the
// per-axis and square chroma allocations DIFFER -- i.e. at 4:2:2, where
// the square is half the plane block. Zero on 4:2:0 and 4:4:4, where the
// two forms are the same number, so a gate that wants to prove it actually
// exercised the shape under test (rather than passing vacuously) asserts
// this moved.
thread_local! {
    static SKIP_ARM_CHROMA_RECT_SHAPES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`SKIP_ARM_CHROMA_RECT_SHAPES`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn skip_arm_chroma_rect_shapes() -> usize {
    SKIP_ARM_CHROMA_RECT_SHAPES.with(|c| c.get())
}

// lane-av1floorshift: the two FRAME chroma plane allocators
// (`decode_key_frame_tile_with_cdfs` and `decode_inter_tile`'s `PlaneBuf` trio)
// size U and V as `(width >> ss_x) x (height >> ss_y)` with `width`/`height`
// the 32-aligned coded size and `true_width`/`true_height` its `mi_cols * 4`
// counterpart. `lanes/av1subsizesweep.report.md` named these "floor-`>>`
// chroma plane allocators -- shape assumption at frame edges; not probed":
// the `>>` FLOORS where libaom's own producer CROPS (`round_ss`), so the
// question was whether an odd frame dimension makes the allocation one sample
// short of the extent every chroma walk inside the tile indexes.
//
// `FRAME_CHROMA_ALLOC_SHORT_SAMPLES` is the measured answer: the number of
// samples by which the allocated `cw * ch` fell SHORT of the `round_ss`
// extent the tile's chroma walks may address, summed over every frame. It
// reads the allocation's real length against a recomputed `round_ss` bound, so
// it measures the shape rather than proxying a condition. Restoring a bare
// `>> 1` on either axis drives it strictly positive at an odd dimension.
thread_local! {
    static FRAME_CHROMA_ALLOC_SHORT_SAMPLES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// lane-av1floorshift: frames whose coded luma dimension made `>>` and
// `round_ss` DISAGREE on a chroma axis -- i.e. the only frames on which the
// floor-vs-crop question is even decidable. Zero on every even dimension,
// which is why a gate asserting `FRAME_CHROMA_ALLOC_SHORT_SAMPLES == 0` is
// vacuous without this: on an even frame the two forms are the same number
// and the shortfall cannot move.
thread_local! {
    static FRAME_CHROMA_ALLOC_CROP_SHAPES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

// How many times a frame chroma plane allocator ran at all -- one per frame,
// from either the key-frame or the inter call site. The non-vacuity arm: the
// other two counters are ZERO on every input that exists (see
// [`note_frame_chroma_alloc`]), so a gate that only asserted them would pass
// with the instrument removed entirely.
thread_local! {
    static FRAME_CHROMA_ALLOC_CALLS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`FRAME_CHROMA_ALLOC_CALLS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn frame_chroma_alloc_calls() -> usize {
    FRAME_CHROMA_ALLOC_CALLS.with(|c| c.get())
}

/// Current value of [`FRAME_CHROMA_ALLOC_SHORT_SAMPLES`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn frame_chroma_alloc_short_samples() -> usize {
    FRAME_CHROMA_ALLOC_SHORT_SAMPLES.with(|c| c.get())
}

/// Current value of [`FRAME_CHROMA_ALLOC_CROP_SHAPES`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn frame_chroma_alloc_crop_shapes() -> usize {
    FRAME_CHROMA_ALLOC_CROP_SHAPES.with(|c| c.get())
}

/// The census both frame chroma plane allocators share. `shape` is the frame's
/// own 32-aligned coded `(width, height)`, `ss` its `(ss_x, ss_y)`.
///
/// `round_ss` is libaom's chroma CROP (`av1_round_shift`), the extent the
/// committed reference planes are stored at and the bound every chroma walk
/// inside the tile is clamped against.
///
/// `alloc` is the allocator's OWN `(cw, ch)` -- the numbers that size the
/// `PlaneBuf` -- not a recomputation of them. That is what makes the gate
/// bite: the shortfall is a difference between what was allocated and what the
/// tile may address, so replacing the shift with a bare `>> 1` (the 4:2:0-only
/// form this lane's target class is made of) moves it on the 4:2:2 and 4:4:4
/// witnesses, where the correct shift and the hardcoded one are different
/// numbers. A counter that recomputed `shape >> ss` beside the allocation
/// would have read the same value either way and proved nothing.
fn note_frame_chroma_alloc(shape: (usize, usize), alloc: (usize, usize), ss: (usize, usize)) {
    hit!(FRAME_CHROMA_ALLOC_CALLS);
    let (need_w, need_h) = (round_ss(shape.0, ss.0), round_ss(shape.1, ss.1));
    // Decidability: the floor and the crop are the same number on this frame,
    // so the shortfall below is the same number the two forms would have given.
    if (need_w, need_h) != (shape.0 >> ss.0, shape.1 >> ss.1) {
        hit!(FRAME_CHROMA_ALLOC_CROP_SHAPES);
        if crate::envflags::env_flag!("EC_AV1_CHROMA_ALLOC_SWEEP") {
            eprintln!(
                "CHROMA_ALLOC luma={}x{} ss={:?} floor={}x{} round_ss={}x{} alloc={}x{}",
                shape.0,
                shape.1,
                ss,
                shape.0 >> ss.0,
                shape.1 >> ss.1,
                need_w,
                need_h,
                alloc.0,
                alloc.1,
            );
        }
    }
    let want = need_w * need_h;
    let got = alloc.0 * alloc.1;
    if want > got {
        hit_do!(FRAME_CHROMA_ALLOC_SHORT_SAMPLES.with(|c| c.set(c.get() + want - got)));
    }
}

/// lane-av1ibcfork: this decoder's half of the per-unit BIT INTERVAL -- the
/// exact mirror of the oracle's `EC_ECDUMP_IN` / `EC_ECDUMP_OUT` /
/// `EC_ECDUMP_END` triple (`aom-oracle2/src/av1/decoder/decodetxb.c`: one
/// print in `read_coeffs_txb` before and after its `aom_read_symbol`, one in
/// the single caller `av1_read_coeffs_txb` after the whole unit).
///
/// Why three prints and not one `EC_CENSUS_UNIT`: a COUNT is not a key. The
/// two sides can walk a different NUMBER of units and still be paired, because
/// the interval is what a unit CONSUMED. Entry position alone drifts the moment
/// one side consumes a different number of bits, and list index is invalid the
/// moment one side misses a unit -- both were measured here and both
/// mis-attribute. `post_bit` after the `txb_skip` read is the same
/// `aom_reader_tell` the oracle prints there; `end_bit` is the end of the whole
/// unit including the optional luma `tx_type`, the eob, the base/BR passes and
/// the sign/Golomb loop, so a unit that reads the right symbols in the wrong
/// ORDER is caught too.
///
/// `bit` counts the bits this decoder has pulled out of its window, so it sits
/// at a CONSTANT offset from the oracle's tell (the OBU header libaom counts
/// before `aom_reader_init`). The offset is measured per fixture, never
/// assumed: +14 on `444_intrabc_rect4_witness.obu`.
#[inline]
fn unit_iv_on() -> bool {
    crate::envflags::env_flag!("EC_UNITIV")
}

/// The unit's ENTRY position. Returns it so the caller can pass the same value
/// to [`unit_iv_skip`] and [`unit_iv_end`] without reading the decoder again.
#[inline]
#[track_caller]
fn unit_iv_in(plane: usize, w: usize, h: usize, ctx: usize, dec: &SymbolDecoder) -> usize {
    if !unit_iv_on() {
        return 0;
    }
    let bit = dec.debug_bitpos();
    let (mi_r, mi_c) = SymbolDecoder::symr_mi();
    eprintln!(
        "EC_UNITIV_IN side=ours plane={plane} mi=({mi_r},{mi_c}) w={w} h={h} ctx={ctx} bit={bit} at {}",
        std::panic::Location::caller()
    );
    bit
}

/// The position immediately after this unit's `txb_skip` read. `bit_in ==
/// post_bit` means the read consumed ZERO bits (a CDF row already at a
/// boundary), so this is also the real-read filter: the reader logs a step
/// either way, and counting raw lines overstates the symbol count ~2x (measured
/// on `444_intrabc_rect4_witness.obu`: 2395 lines, 512 real reads, against the
/// oracle's 513 -- not the "~1069 fewer" an earlier round reported).
#[inline]
fn unit_iv_skip(
    dec: &SymbolDecoder,
    bit_in: usize,
    plane: usize,
    w: usize,
    h: usize,
    ctx: usize,
    all_zero: bool,
) {
    if !unit_iv_on() {
        return;
    }
    let (mi_r, mi_c) = SymbolDecoder::symr_mi();
    eprintln!(
        "EC_UNITIV_SKIP side=ours plane={plane} mi=({mi_r},{mi_c}) w={w} h={h} ctx={ctx} az={} bit={bit_in} post_bit={}",
        all_zero as i32,
        dec.debug_bitpos()
    );
}

/// The position after the WHOLE unit. One call per successful read, on both
/// the all-zero early return and the full coefficient walk.
#[inline]
fn unit_iv_end(dec: &SymbolDecoder, plane: usize, w: usize, h: usize, ctx: usize) {
    if !unit_iv_on() {
        return;
    }
    let (mi_r, mi_c) = SymbolDecoder::symr_mi();
    eprintln!(
        "EC_UNITIV_END side=ours plane={plane} mi=({mi_r},{mi_c}) w={w} h={h} ctx={ctx} end_bit={}",
        dec.debug_bitpos()
    );
}

/// A just-decoded palette-Y block's own predicted pixels
/// (`side*side`, row-major, `colors[map[i]]`), consumed by
/// [`PlaneBuf::reconstruct`]'s next call in place of `predict()` -- the
/// same off-the-call-stack idiom as [`ENABLE_EDGE_FILTER`] (threading a
/// per-block override through `read_plane`'s two dozen call sites is the
/// alternative this avoids). Always empty again once that call consumes
/// it (`.take()`), so a non-palette block's own `reconstruct` call never
/// sees stale state.
/// Arms the pending [`PALETTE_PRED`] override for the next `reconstruct`
/// (`set`) / consumes it (`take`). Set and take MUST pair: a buffer armed at a
/// site whose reconstruct never runs would be picked up by the NEXT block's
/// reconstruct (class [[refusal-hides-a-defect]] / stale thread-local), so
/// lane-lossless2: one transform unit's window on a block-sized palette
/// prediction buffer. A unit reads its prediction at ITS OWN stride, so
/// handing it the whole-block buffer makes every unit reconstruct the block's
/// top-left corner (the luma split-transform paths already window; the chroma
/// ones did not, which is invisible until a lossless frame codes a palette
/// block as 4x4 chroma units).
fn palette_window(
    buf: &[u16],
    stride: usize,
    ox: usize,
    oy: usize,
    w: usize,
    h: usize,
) -> Vec<u16> {
    // `stride` is the BLOCK's own chroma width, so a caller that handed over a
    // mismatched stride could read in-bounds GARBAGE instead of panicking --
    // a silent misread, which is the one failure mode this walk must not have.
    // The refutation pass (`lanes/refute-av1444d10.report.md`, risk (e)) named
    // that as the only thing keeping the tiled walk safe, so the invariant is
    // closed loudly here instead of resting on an `unsupported` return
    // elsewhere that a future shape could remove.
    assert!(
        w <= stride && ox + w <= stride && (oy + h) * stride <= buf.len(),
        "palette window {w}x{h} at ({ox},{oy}) does not fit a {}-entry buffer at stride {stride}",
        buf.len()
    );
    (0..h)
        .flat_map(|r| buf[(oy + r) * stride + ox..][..w].iter().copied())
        .collect()
}

/// `EC_DEBUG_PAL=1` traces every arm and every consumption with its call site.
#[track_caller]
fn set_palette_pred(buf: Vec<u16>, fctx: &crate::decode::FrameCtx) {
    if crate::envflags::env_flag!("EC_DEBUG_PAL") {
        let stale = fctx.palette_pred.with(|c| c.borrow().is_some());
        eprintln!(
            "PALSET len={} stale={stale} at {}",
            buf.len(),
            std::panic::Location::caller()
        );
    }
    fctx.palette_pred.with(|c| *c.borrow_mut() = Some(buf));
}

#[track_caller]
fn take_palette_pred(fctx: &crate::decode::FrameCtx) -> Option<Vec<u16>> {
    let got = fctx.palette_pred.with(|c| c.borrow_mut().take());
    if crate::envflags::env_flag!("EC_DEBUG_PAL") {
        eprintln!(
            "PALTAKE len={:?} at {}",
            got.as_ref().map(Vec::len),
            std::panic::Location::caller()
        );
    }
    got
}

/// The luma window a `UV_CFL_PRED` chroma block averages its AC signal over.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CflSrc {
    px: usize,
    py: usize,
    bw: usize,
    bh: usize,
}

fn cfl_src(px: usize, py: usize, side: usize) -> CflSrc {
    CflSrc {
        px,
        py,
        bw: side,
        bh: side,
    }
}

fn cfl_src_rect(px: usize, py: usize, bw: usize, bh: usize) -> CflSrc {
    CflSrc { px, py, bw, bh }
}

/// The dequantization + inverse transform of one transform unit, kept as its
/// INPUTS so the transform runs where the unit is written. `stride == 0` means
/// the dense `w x h` residual; otherwise it is re-laid into the
/// `stride x stride` grid `reconstruct_mc_rect` reads both of its buffers at.
#[derive(Clone, Copy)]
pub(crate) struct TxParams {
    /// The plane this unit belongs to (0 luma, 1 U, 2 V), carried from the
    /// reader that OWNS the unit. `dequant_and_inverse_wht4x4`'s `EC_DQCOEFF`
    /// twin prints it so its ladder pairs with the oracle's: the per-unit
    /// census plane could not, because the callers that route a lossless 4x4
    /// unit into the WHT are not all the ones that stamp the census plane
    /// (measured on `ll444-lossless-key.obu`: 1663 of 1663 chroma units paired
    /// by count, 1660 of them against an empty grid).
    pub(crate) plane: usize,
    pub(crate) w: usize,
    pub(crate) h: usize,
    pub(crate) bit_depth: u8,
    pub(crate) q_idx: i32,
    pub(crate) dc_delta: i32,
    pub(crate) ac_delta: i32,
    pub(crate) tx_type: TxType,
    /// lane-av1-qmatrix: this unit's inverse quantisation matrix, selected
    /// on the parse thread by [`block_iqmatrix`] and captured here so the
    /// transform scales identically without touching frame state.
    pub(crate) qm: Option<crate::qm::Iqm>,
    pub(crate) stride: usize,
    /// lane-lossless: this transform unit belongs to a lossless BLOCK (its
    /// segment's `xd->lossless[segment_id]` bit), so the residual comes off
    /// the 4x4 Walsh-Hadamard
    /// ([`crate::transform::dequant_and_inverse_wht4x4`]) instead of the
    /// DCT/ADST network (libaom `inv_txfm_add`, `av1/common/idct.c`).
    pub(crate) lossless: bool,
}

/// lane-av1mixloss: Walsh-Hadamard transform units actually reconstructed.
/// The mixed-lossless witness's proof the per-segment lossless path really
/// ran on a frame that is NOT `CodedLossless`.
static LOSSLESS_WHT_UNITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// [`LOSSLESS_WHT_UNITS`] -- total over the process, read as a before/after
/// delta around one decode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn lossless_wht_units() -> usize {
    LOSSLESS_WHT_UNITS.load(std::sync::atomic::Ordering::Relaxed)
}

/// lane-whtshape: 4x4 Walsh-Hadamard units reconstructed as part of a LOSSLESS
/// transform unit that was itself NOT 4x4 -- a shape libaom cannot code.
/// `read_tx_mode` (`av1/decoder/decodeframe.c:140`) returns `ONLY_4X4` for a
/// `coded_lossless` frame before it looks at the `tx_mode` symbol,
/// `read_tx_size` (`:1117`) returns `TX_4X4` before it looks at a tree,
/// `get_vartx_max_txsize` (`av1/common/blockd.h:1447`) returns `TX_4X4` for
/// EVERY plane of a lossless block, and of `av1/common/idct.c`'s
/// `highbd_inv_txfm_add_*_c` only `highbd_inv_txfm_add_4x4_c` carries a
/// `lossless` branch -- every other size falls through to the IDCT, because
/// there is no non-4x4 lossless inverse transform to dispatch to. So one of
/// these is a CALLER sizing a lossless unit wrongly, not a bitstream shape;
/// read as a before/after delta around one decode it names the caller. Must
/// read 0 on a stream whose lossless unit geometry is right.
static LOSSLESS_WHT_SPLIT_UNITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// [`LOSSLESS_WHT_SPLIT_UNITS`] -- total over the process, read as a
/// before/after delta around one decode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn lossless_wht_split_units() -> usize {
    LOSSLESS_WHT_SPLIT_UNITS.load(std::sync::atomic::Ordering::Relaxed)
}

impl TxParams {
    /// The one copy of the produce-the-residual code.
    fn run(self, grid: &[i32]) -> Vec<i32> {
        let residual = if self.lossless {
            LOSSLESS_WHT_UNITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // lane-whtshape: a non-4x4 unit here is this decoder's own
            // transform-unit geometry being wrong (see
            // [`LOSSLESS_WHT_SPLIT_UNITS`] for the libaom evidence). It used
            // to be stopped by a `debug_assert_eq!` that RELEASE never ran:
            // the 4x4 WHT returned 16 values for a `w * h` block and the
            // release build died one frame later in the strided copy with an
            // opaque `range end index 8 out of range for slice of length 0`
            // that never named lossless or the transform. Counted, and
            // reconstructed as the raster of 4x4 WHT units
            // ([`crate::transform::dequant_and_inverse_wht`]) instead: total,
            // the same in both profiles, and the 4x4 fast path is the same
            // single call it always was. The old
            // `debug_assert_eq!(self.tx_type, TxType::DctDct)` is gone with
            // it: the WHT takes no tx_type (libaom's `av1_iwht4x4_add` does
            // not either), and `av1_get_tx_type` already answers `DCT_DCT` on
            // every plane of a lossless block, so asserting an argument
            // nothing reads was a debug-only panic on a value release
            // ignores.
            if self.w != 4 || self.h != 4 {
                LOSSLESS_WHT_SPLIT_UNITS.fetch_add(
                    self.w.div_ceil(4) * self.h.div_ceil(4),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            crate::transform::dequant_and_inverse_wht(
                grid,
                self.plane,
                self.w,
                self.h,
                self.bit_depth,
                self.q_idx,
                self.dc_delta,
                self.ac_delta,
            )
        } else {
            dequant_and_inverse_typed_wh(
                grid,
                self.w,
                self.h,
                self.bit_depth,
                self.q_idx,
                self.dc_delta,
                self.ac_delta,
                self.tx_type,
                self.qm,
            )
        };
        // An all-zero grid transforms to the empty marker, and a strided copy
        // of zeros is still zeros: the marker passes straight through.
        if residual.is_empty() {
            return residual;
        }
        if self.stride == 0 {
            return residual;
        }
        // lane-av1-422sub8rect: `h` rows of `stride` -- a rect unit whose
        // height exceeds its (narrow) prediction stride needs the full
        // stride * h; every square caller has h == stride and is unchanged.
        let mut strided = vec![0i32; self.stride * self.h];
        for row in 0..self.h {
            strided[row * self.stride..][..self.w]
                .copy_from_slice(&residual[row * self.w..][..self.w]);
        }
        strided
    }
}

/// An all-zero residual, for the transform units that code no coefficient:
/// [`crate::transform::dequant_and_inverse_typed_wh`] returns those EMPTY,
/// and the reconstruction loops below read this instead of a per-unit
/// `vec![0; w * h]`. Sized for the largest stride a recorded unit can carry
/// (a 128-wide inter block's prediction stride).
pub(crate) static ZERO_RESIDUAL: [i32; 128 * 128] = [0; 128 * 128];

/// lane-blockscratch: one plane's coefficient levels as the block-tail
/// neighbour writes want them. A plane that coded nothing (a skipped block, a
/// strip with no chroma, luma under a var-tx split whose units already wrote
/// themselves) names the shared [`ZERO_RESIDUAL`] instead of allocating and
/// zeroing up to 64 KB of its own per block.
#[derive(Clone)]
enum Grid {
    Zero(usize),
    Own(Vec<i32>),
}

/// lane-blockscratch: the mu-chunk chroma collector, allocated by the first
/// unit that writes into it. Only a block wider or taller than 64 px codes its
/// chroma per 64x64 chunk; every smaller block used to allocate and zero both
/// collectors per block and then drop them untouched.
fn mu_units(v: &mut Vec<i32>, n: usize) -> &mut Vec<i32> {
    if v.is_empty() {
        *v = vec![0i32; n];
    }
    v
}

impl std::ops::Deref for Grid {
    type Target = [i32];
    fn deref(&self) -> &[i32] {
        match self {
            Grid::Zero(n) => &ZERO_RESIDUAL[..*n],
            Grid::Own(v) => v,
        }
    }
}

/// Whether a coefficient grid codes nothing: a [`Grid::Zero`] unit IS the
/// shared [`ZERO_RESIDUAL`], so one pointer compare answers the 60% of
/// transform units that code no coefficient without the scan (lane-coefgrid).
pub(crate) fn is_zero_grid(grid: &[i32]) -> bool {
    std::ptr::eq(grid.as_ptr(), ZERO_RESIDUAL.as_ptr()) || grid.iter().all(|&c| c == 0)
}

/// `residual`, or `n` zeros when it is the empty all-zero marker.
fn dense_residual(residual: &[i32], n: usize) -> &[i32] {
    if residual.is_empty() {
        &ZERO_RESIDUAL[..n]
    } else {
        residual
    }
}

/// Where a motion-compensated write reads its prediction from.
#[derive(Clone, Copy)]
pub(crate) enum Pred<'a> {
    /// A prediction the parse thread already built, at its own stride
    /// (`stride == 0` means "the reader's own `stride` argument").
    Inline(&'a [u16], usize),
    /// The current block's deferred prediction (see [`wave::ReconOp::Pred`]):
    /// plane, its stride, and this unit's offset into it.
    Cur {
        plane: usize,
        stride: usize,
        off: usize,
    },
}

impl<'a> Pred<'a> {
    /// The same prediction read at an explicit stride.
    fn view(self, stride: usize) -> Self {
        match self {
            Self::Inline(b, _) => Self::Inline(b, stride),
            Self::Cur { plane, off, .. } => Self::Cur { plane, stride, off },
        }
    }

    /// The buffer and the stride to read it at (`stride` is the reader's own
    /// when this prediction was built dense); `None` for a deferred
    /// prediction, which only a recorded op can carry.
    fn inline(self, stride: usize) -> Option<(&'a [u16], usize)> {
        match self {
            Self::Inline(b, s) => Some((b, if s == 0 { stride } else { s })),
            Self::Cur { .. } => None,
        }
    }

    /// This prediction's window starting `n` samples in.
    fn offset(self, n: usize) -> Self {
        match self {
            Self::Inline(b, s) => Self::Inline(&b[n..], s),
            Self::Cur { plane, stride, off } => Self::Cur {
                plane,
                stride,
                off: off + n,
            },
        }
    }
}

/// Whether a block's prediction may be built by a recon worker: a wavefront
/// is running and this frame's references can be handed to it.
fn pred_deferred(refpix: &RefPix<'_>) -> bool {
    wave::active() && refpix.is_shared()
}

/// Records one block's prediction build for the worker that reconstructs it.
fn push_pred<F>(f: F)
where
    F: FnOnce(
            &mut PlaneBuf<'static>,
            &mut PlaneBuf<'static>,
            &mut PlaneBuf<'static>,
            &RefPix<'_>,
            &crate::decode::FrameCtx,
            &mut [Vec<u16>; 3],
        ) + Send
        + 'static,
{
    wave::push(wave::ReconOp::Pred(Box::new(f)));
}

/// The previous block's prediction buffer, cleared and zero-filled to `n`
/// (lane-blockmem): the same bytes the `vec![0u16; n]` it replaces produced,
/// without the per-block allocation and free.
fn refill(mut v: Vec<u16>, n: usize) -> Vec<u16> {
    // A buffer that has to grow anyway is cheaper allocated zeroed (`calloc`)
    // than resized element by element -- which is every block on the inline
    // path, where there is no previous buffer to reuse.
    if v.capacity() < n {
        return vec![0u16; n];
    }
    v.clear();
    v.resize(n, 0);
    v
}

/// [`PlaneBuf::reconstruct`] at its push site: the intra transform block is
/// written into the tile's planes as soon as it is parsed.
#[allow(clippy::too_many_arguments)]
fn push_intra(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    plane: usize,
    x: usize,
    cy: usize,
    side: usize,
    mode: usize,
    angle_delta: i32,
    reach: Reach,
    residual: &[i32],
    cfl: Option<(i32, CflSrc)>,
    filter_intra: Option<usize>,
    smooth_neighbor: bool,
    fctx: &crate::decode::FrameCtx,
) {
    let pred = take_palette_pred(fctx);
    exec_intra(
        y,
        u,
        v,
        plane,
        x,
        cy,
        side,
        side,
        false,
        mode,
        angle_delta,
        reach,
        residual,
        cfl,
        filter_intra,
        smooth_neighbor,
        pred,
        fctx,
    );
}

/// [`PlaneBuf::reconstruct_rect`] at its push site.
#[allow(clippy::too_many_arguments)]
fn push_intra_rect(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    plane: usize,
    x: usize,
    cy: usize,
    bw: usize,
    bh: usize,
    mode: usize,
    angle_delta: i32,
    reach: Reach,
    residual: &[i32],
    cfl: Option<(i32, CflSrc)>,
    filter_intra: Option<usize>,
    smooth_neighbor: bool,
    fctx: &crate::decode::FrameCtx,
) {
    let pred = take_palette_pred(fctx);
    exec_intra(
        y,
        u,
        v,
        plane,
        x,
        cy,
        bw,
        bh,
        true,
        mode,
        angle_delta,
        reach,
        residual,
        cfl,
        filter_intra,
        smooth_neighbor,
        pred,
        fctx,
    );
}

/// [`PlaneBuf::reconstruct_mc_rect`] at `stride == w == h == side`.
#[allow(clippy::too_many_arguments)]
fn push_mc(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    plane: usize,
    x: usize,
    cy: usize,
    side: usize,
    prediction: Pred<'_>,
    residual: &[i32],
    fctx: &crate::decode::FrameCtx,
) {
    push_mc_rect(
        y, u, v, plane, x, cy, side, side, side, prediction, residual, fctx,
    );
}

/// [`PlaneBuf::reconstruct_mc_rect`] at its push site.
#[allow(clippy::too_many_arguments)]
#[track_caller]
fn push_mc_rect(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    plane: usize,
    x: usize,
    cy: usize,
    stride: usize,
    w: usize,
    h: usize,
    prediction: Pred<'_>,
    residual: &[i32],
    fctx: &crate::decode::FrameCtx,
) {
    // lane-av1ibcfork: the PUSH site of an MC/inter write. The executed rung
    // (`EC_MCWR`, in `reconstruct_mc_rect`) can only name the dispatch, so
    // this is what names the block decoder that asked for the write -- the
    // only thing that distinguishes two writes of the same shape. It is what
    // localized a 4:4:4 intra-BC SKIP block committing its chroma plane block
    // at `(px/2, py/2)` instead of the ss origin, clobbering a neighbouring
    // block's reconstruction (lanes/av1ibcfork.report.md).
    if crate::envflags::env_flag!("EC_MCPUSH") {
        eprintln!(
            "EC_MCPUSH plane={plane} x={x} y={cy} stride={stride} w={w} h={h} \
             res_len={} at {}",
            residual.len(),
            std::panic::Location::caller()
        );
    }
    let Some((src, stride)) = prediction.inline(stride) else {
        let Pred::Cur {
            plane: sp,
            stride: ss,
            off,
        } = prediction
        else {
            unreachable!("only a deferred prediction has no inline buffer")
        };
        wave::push(wave::ReconOp::Mc {
            plane,
            x,
            oy: cy,
            stride: ss,
            w,
            h,
            prediction: wave::PredSrc::Cur {
                plane: sp,
                off,
                src: ss,
            },
            residual: wave::Residual::ready(residual),
        });
        return;
    };
    exec_mc(y, u, v, plane, x, cy, stride, w, h, src, residual, fctx);
}

/// [`push_intra_rect`] carrying the transform's INPUTS: the dequantization
/// and inverse transform run here, at the push site.
#[allow(clippy::too_many_arguments)]
fn push_intra_tx(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    plane: usize,
    x: usize,
    cy: usize,
    bw: usize,
    bh: usize,
    rect: bool,
    mode: usize,
    angle_delta: i32,
    reach: Reach,
    tx: TxParams,
    grid: &[i32],
    cfl: Option<(i32, CflSrc)>,
    filter_intra: Option<usize>,
    smooth_neighbor: bool,
    fctx: &crate::decode::FrameCtx,
) {
    if wave::active() {
        let pred = take_palette_pred(fctx);
        wave::push(wave::ReconOp::Intra {
            plane,
            x,
            oy: cy,
            bw,
            bh,
            rect,
            mode,
            angle_delta,
            reach,
            residual: wave::Residual::tx(tx, grid),
            cfl,
            filter_intra,
            smooth_neighbor,
            pred,
        });
        return;
    }
    let residual = tx.run(grid);
    let pred = take_palette_pred(fctx);
    exec_intra(
        y,
        u,
        v,
        plane,
        x,
        cy,
        bw,
        bh,
        rect,
        mode,
        angle_delta,
        reach,
        &residual,
        cfl,
        filter_intra,
        smooth_neighbor,
        pred,
        fctx,
    );
}

/// [`push_mc_rect`] carrying the transform's INPUTS (see [`push_intra_tx`]).
#[allow(clippy::too_many_arguments)]
fn push_mc_rect_tx(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    plane: usize,
    x: usize,
    cy: usize,
    stride: usize,
    w: usize,
    h: usize,
    prediction: Pred<'_>,
    tx: TxParams,
    grid: &[i32],
    fctx: &crate::decode::FrameCtx,
) {
    let Some((prediction, stride)) = prediction.inline(stride) else {
        // A deferred prediction is not there to be windowed: the op carries its
        // offset into the block's prediction, and the residual is laid out dense.
        let Pred::Cur {
            plane: sp,
            stride: ss,
            off,
        } = prediction
        else {
            unreachable!("only a deferred prediction has no inline buffer")
        };
        let mut tx = tx;
        tx.stride = 0;
        wave::push(wave::ReconOp::Mc {
            plane,
            x,
            oy: cy,
            stride: w,
            w,
            h,
            prediction: wave::PredSrc::Cur {
                plane: sp,
                off,
                src: ss,
            },
            residual: wave::Residual::tx(tx, grid),
        });
        return;
    };
    let res_stride = if tx.stride == 0 { w } else { tx.stride };
    let window;
    let prediction = if stride == res_stride {
        prediction
    } else {
        debug_assert_eq!(res_stride, w);
        let mut win = Vec::with_capacity(w * h);
        for row in 0..h {
            win.extend_from_slice(&prediction[row * stride..][..w]);
        }
        window = win;
        &window[..]
    };
    if wave::active() {
        wave::push(wave::ReconOp::Mc {
            plane,
            x,
            oy: cy,
            stride: res_stride,
            w,
            h,
            prediction: wave::PredSrc::Owned(prediction.to_vec()),
            residual: wave::Residual::tx(tx, grid),
        });
        return;
    }
    let residual = tx.run(grid);
    exec_mc(
        y, u, v, plane, x, cy, res_stride, w, h, prediction, &residual, fctx,
    );
}

/// Performs one intra transform block's write into the tile's planes.
#[allow(clippy::too_many_arguments)]
fn exec_intra(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    plane: usize,
    x: usize,
    oy: usize,
    bw: usize,
    bh: usize,
    rect: bool,
    mode: usize,
    angle_delta: i32,
    reach: Reach,
    residual: &[i32],
    cfl: Option<(i32, CflSrc)>,
    filter_intra: Option<usize>,
    smooth_neighbor: bool,
    pred: Option<Vec<u16>>,
    fctx: &crate::decode::FrameCtx,
) {
    if wave::active() {
        wave::push(wave::ReconOp::Intra {
            plane,
            x,
            oy,
            bw,
            bh,
            rect,
            mode,
            angle_delta,
            reach,
            residual: wave::Residual::ready(residual),
            cfl,
            filter_intra,
            smooth_neighbor,
            pred,
        });
        return;
    }
    // The CfL AC signal is read HERE, off the luma that has already been
    // written for this block -- at parse time it would have been the previous
    // frame's samples.
    let ac = cfl.map(|(alpha, src)| {
        let sig = cfl_ac_ss(&*y, src.px, src.py, src.bw, src.bh, ss_x(fctx), ss_y(fctx));
        (alpha, sig)
    });
    let cfl = ac.as_ref().map(|(a, sig)| (*a, sig.as_slice()));
    // Both replays below index their inputs by the transform block's area. A
    // size mismatch is a corrupt stream (the override or the CfL source window
    // does not belong to this transform block): flag it, and let the
    // reconstruction fall back to edge prediction without indexing past it.
    let area = bw * bh;
    if pred.as_ref().is_some_and(|p| p.len() != area) {
        fctx.flag_corrupt(
            "palette or intrabc prediction override is not the transform block's size",
        );
    }
    if cfl.is_some_and(|(_, sig)| sig.len() != area) {
        fctx.flag_corrupt("CfL AC signal is not the transform block's size");
    }
    let target: &mut PlaneBuf<'static> = match plane {
        0 => &mut *y,
        1 => &mut *u,
        _ => &mut *v,
    };
    // lane-av1cmpaudit: label the in-flight reconstruction's plane for the
    // `EC_PRED` rung (aomdec's `EC_PREDOUT8`/`EC_PREDND` print `plane=`).
    // Saved and restored so the rung reports THIS call's plane even under
    // nesting, and so both of exec_intra's return paths need no cleanup.
    let prev_plane = RECON_PLANE.with(|c| c.replace(plane));
    let _plane_guard = PlaneGuard(prev_plane);
    if rect {
        target.reconstruct_rect(
            x,
            oy,
            bw,
            bh,
            mode,
            angle_delta,
            reach,
            residual,
            cfl,
            filter_intra,
            smooth_neighbor,
            pred,
            fctx,
        );
    } else {
        target.reconstruct(
            x,
            oy,
            bw,
            mode,
            angle_delta,
            reach,
            residual,
            cfl,
            filter_intra,
            smooth_neighbor,
            pred,
            fctx,
        );
    }
}

/// Performs one motion-compensated transform block's write (see [`exec_intra`]).
#[allow(clippy::too_many_arguments)]
fn exec_mc(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    plane: usize,
    x: usize,
    oy: usize,
    stride: usize,
    w: usize,
    h: usize,
    prediction: &[u16],
    residual: &[i32],
    fctx: &crate::decode::FrameCtx,
) {
    if wave::active() {
        wave::push(wave::ReconOp::Mc {
            plane,
            x,
            oy,
            stride,
            w,
            h,
            prediction: wave::PredSrc::Owned(prediction.to_vec()),
            residual: wave::Residual::ready(residual),
        });
        return;
    }
    let target: &mut PlaneBuf<'static> = match plane {
        0 => &mut *y,
        1 => &mut *u,
        _ => &mut *v,
    };
    target.reconstruct_mc_rect(x, oy, plane, stride, w, h, prediction, residual, fctx);
}

// The `tx_type` the last plane-0 [`read_plane`] actually coded, kept so
// an intrabc block's chroma reads can inherit it (see
// [`INTRABC_CHROMA_TX`]). Written on every luma read; only ever read
// through that one arming site.
// libaom `av1_get_tx_type` (`blockd.h:1350`): for `PLANE_TYPE_UV` it
// branches on `is_inter_block(mbmi)` -- which `is_intrabc_block` also
// satisfies (`blockd.h:373`) -- and takes the *colocated luma
// transform's coded* `tx_type` (`mbmi->txk_type[...]`), never
// `intra_mode_to_tx_type` of the chroma mode. `Some` only across an
// intrabc block's two chroma [`read_plane`] calls; the type decides the
// `TX_CLASS`, hence the scan order, `eob_multi_ctx` and every nz context
// -- getting it wrong desyncs the chroma coefficients, it is not a
// reconstruction-only detail.

/// Whether this frame's header set `allow_intrabc` -- [`INTRABC_MI_GRID`] is
/// `Some` exactly then (set once per frame in `decode_frame`), so the flag
/// needs no separate plumbing to reach the rect-strip mode reader.
fn allow_intrabc_frame(fctx: &crate::decode::FrameCtx) -> bool {
    fctx.intrabc_mi_grid.with(|g| g.borrow().is_some())
}

thread_local! {
    /// How many rect intra strips READ the `use_intrabc` symbol (i.e. how many
    /// times the fix below was exercised) -- the rect-strip gate's hit counter,
    /// distinct from [`INTRABC_HITS`] which counts blocks that USE intrabc.
    static RECT_INTRABC_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`RECT_INTRABC_READS`].
pub(crate) fn rect_intrabc_reads() -> usize {
    RECT_INTRABC_READS.with(|c| c.get())
}

thread_local! {
    /// lane-av1-ibc128chunk: the mu-chunk walk's per-axis chroma extents, as
    /// `(64 >> ss_x) << 8 | (64 >> ss_y)` on the LAST chunk -- the pair the
    /// per-axis walk used. A 4:2:2 run and a 4:2:0 run of the same block
    /// differ here (32|64 vs 32|32), which is what the de-square-cut fixes.
    static INTRABC_128RECT_CHUNK_SHAPE: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`INTRABC_128RECT_CHUNK_SHAPE`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub fn intrabc_128rect_chunk_shape() -> usize {
    INTRABC_128RECT_CHUNK_SHAPE.with(|c| c.get())
}

/// Records one coded block into [`INTRABC_MI_GRID`]: every block contributes
/// its size (libaom's `scan_row_mbmi` advances `processed_rows` off any
/// candidate's `bsize`, ref-match or not), an intrabc one also its DV as an
/// `INTRA_FRAME` candidate (libaom treats intrabc as `is_inter_block`).
fn record_intrabc_mi(
    mi_r: usize,
    mi_c: usize,
    n4: usize,
    dv: Option<(i32, i32)>,
    n: &mut Neighbours,
    fctx: &crate::decode::FrameCtx,
) {
    record_intrabc_mi_rect(mi_r, mi_c, n4, n4, dv, n, fctx);
}

/// [`record_intrabc_mi`] with a true rectangular footprint: `av1_find_mv_refs`
/// takes the block's real `bsize`, so a sub-8x8 `BLOCK_4X8`/`BLOCK_8X4`
/// intrabc leaf must publish a 1x2/2x1 mi span, not a square (lane-av1txr).
///
/// Also stamps libaom's `is_inter_block` verdict for this block into the
/// `above_inter`/`left_inter` bands ([`tx_size_context_txfm_rect`]'s override
/// term): `is_inter_block` is `is_intrabc_block(mbmi) || ref_frame[0] >
/// INTRA_FRAME` (`blockd.h:373`), so an intrabc block is "inter" there and
/// `get_tx_size_context` reads its BLOCK size rather than its transform band.
/// The `false` half of the stamp is [`Neighbours::fill_lf_grid_rect`]'s, which
/// every block runs; this runs after it (class leaked-band otherwise).
fn record_intrabc_mi_rect(
    mi_r: usize,
    mi_c: usize,
    n4_w: usize,
    n4_h: usize,
    dv: Option<(i32, i32)>,
    n: &mut Neighbours,
    fctx: &crate::decode::FrameCtx,
) {
    if dv.is_some() {
        for i in 0..n4_w {
            if let Some(cell) = n.above_inter.get_mut(mi_c + i) {
                *cell = true;
            }
        }
        for i in 0..n4_h {
            if let Some(cell) = n.left_inter.get_mut(mi_r + i) {
                *cell = true;
            }
        }
        // lane-av1txctxband: the per-CELL `is_inter_block` an INTRABC block
        // publishes (`blockd.h:372-374` counts it), at the granularity
        // `get_tx_size_context` reads it. The band above is one slot per
        // column and so survives every later block in that column.
        n.mark_inter_grid_rect((mi_r, mi_c), n4_w, n4_h, true);
    }
    fctx.intrabc_mi_grid.with(|g| {
        let mut g = g.borrow_mut();
        let Some((grid, _, _)) = g.as_mut() else {
            return;
        };
        let info = crate::mvstack::MiInfo {
            is_inter: dv.is_some(),
            ref_frame: 0,
            ref_frame1: NO_REF1,
            mv: mv16(dv.unwrap_or((0, 0))),
            mv1: (0, 0),
            is_new_mv: dv.is_some(),
            is_global_mv0: false,
            is_global_mv1: false,
            size: n4_w as u8,
            size_h: n4_h as u8,
        };
        for r in mi_r..mi_r + n4_h {
            for c in mi_c..mi_c + n4_w {
                grid.set(r, c, info);
            }
        }
    });
}

/// Bump [`FIMV_FRAME_HITS`] -- called once per decoded frame header whose
/// `force_integer_mv` bit is set.
pub fn note_force_integer_mv_frame() {
    hit!(FIMV_FRAME_HITS);
}

// lane-t900 r27: OBMC blends that actually READ a recorded switchable filter
// off the neighbour band -- the positive half of the proof for "an OBMC
// neighbour whose switchable interp filter was never recorded". A
// fixed-filter frame returns before this point (its neighbours legitimately
// carry the `[3, 3]` sentinel), so every bump is a neighbour whose own
// `interp_filter` symbols were published by `record_inter_rect_mi`.
thread_local! {
    static OBMC_FILTER_BAND_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`OBMC_FILTER_BAND_READS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn obmc_filter_band_reads() -> usize {
    OBMC_FILTER_BAND_READS.with(|c| c.get())
}

/// lane-inter16ab r5: the six RECTANGULAR wedge shapes, in this order --
/// the counter slots of [`RECT_WEDGE_HITS`]/[`RECT_WII_HITS`].
pub(crate) const RECT_WEDGE_SHAPES: [(usize, usize); 6] =
    [(8, 16), (16, 8), (16, 32), (32, 16), (8, 32), (32, 8)];

fn rect_wedge_slot(bw: usize, bh: usize) -> Option<usize> {
    RECT_WEDGE_SHAPES.iter().position(|&s| s == (bw, bh))
}

// lane-intra14 r1: set while an INTRA block of an INTER frame is decoded
// through one of the key-frame rect readers ([`decode_block_rect4`],
// [`decode_block_rect64`]). `Some((size_group, skip))` tells
// [`read_intra_mode_rect`] that (a) `skip`/segment id/cdef/delta-q were
// already read by [`decode_inter_block`] and (b) the luma mode comes off
// `y_mode_cdf[size_group_lookup[bsize]]` (libaom `read_intra_block_mode_info`,
// decodemv.c:1189), never the key frame's above/left `kf_y_mode` pair.

// lane-modectx r1: how many times a `kf_y_mode`/`y_mode` neighbour read at a
// TILE edge found its coarse fallback band holding a NON-DC mode -- i.e. a
// mode from the previous tile, where libaom's `up_available`/`left_available`
// make the neighbour DC_PRED. Measured 0 over 20 two-tile-column streams:
// `above_mode` is cleared per tile in `Neighbours::start_tile` and `left_mode`
// per superblock row in `start_row`, so the luma fallback cannot leak the way
// lane-mtfix's `above_uv_mode` one did. The gate asserts it stays 0 -- this is
// the invariant that makes the coarse fallback safe.
thread_local! {
    static MODE_TILE_EDGE_COARSE_LEAKS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Current value of [`MODE_TILE_EDGE_COARSE_LEAKS`].
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn mode_tile_edge_coarse_leaks() -> usize {
    MODE_TILE_EDGE_COARSE_LEAKS.with(|c| c.get())
}

thread_local! {
    static UV_LEFT_SS_Y_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`UV_LEFT_SS_Y_READS`].
pub fn uv_left_ss_y_reads() -> usize {
    UV_LEFT_SS_Y_READS.with(|c| c.get())
}

// lane-av1leftwit: the per-tile epoch `uv_owner` cells are stamped with.
// Bumped in `Neighbours::start_tile`; never read by decode logic.
thread_local! {
    static UV_OWNER_EPOCH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn bump_edge_part(slot: usize) {
    hit_do! {
        EDGE_PART_HITS.with(|c| {
            let mut h = c.get();
            h[slot] += 1;
            c.set(h);
        });
    }
}

/// [`bump_edge32`] of `base + bit`, returning the bit -- the gathered edge
/// reads are `if`-conditions, so the counter has to ride the expression.
fn bump_edge32_bit(base: usize, bit: usize) -> usize {
    bump_edge32(base + bit.min(1));
    bit
}

fn bump_edge32(slot: usize) {
    hit_do! {
        EDGE32_HITS.with(|c| {
            let mut h = c.get();
            h[slot] += 1;
            c.set(h)
        });
    }
}

/// One 16x4 / 4x16 inter strip's chroma situation, handed to
/// [`decode_inter_block`] out of band (the same one-shot thread-local idiom
/// `REDUCED_TX_SET_INTER` uses) rather than through twenty-three call sites
/// that all mean "an ordinary block, chroma of its own".
#[derive(Clone, Copy)]
pub(crate) struct InterStripChroma {
    /// `is_chroma_reference` for this strip: false on strips 0 and 2, which
    /// read and reconstruct NO chroma at all.
    has_chroma: bool,
    /// `PARTITION_HORZ_4` (16x4 strips) rather than `PARTITION_VERT_4`.
    horz: bool,
    /// The mi position of the EVEN strip of this pair -- `setup_pred_plane`'s
    /// `if (ss_y && (mi_row & 1) && mi_size_high[bsize] == 1) mi_row -= 1`,
    /// i.e. the origin of the 8x4 (4x8) chroma transform.
    pair_mi: (usize, usize),
    /// The previous (even) strip's single-reference MC parameters, `None`
    /// when it was intra or compound -- `is_sub8x8_inter` then returns false
    /// and the whole chroma block is predicted from THIS strip's mv.
    prev: Option<(i8, (i32, i32), mc::InterpFilterKind, mc::InterpFilterKind)>,
}

/// The single-reference MC parameters of the inter block that decoded
/// last -- read by the next 1:4 strip to build its pair's chroma.
fn bump_ab16_inter(arm: usize) {
    hit_do! {
        AB16_INTER_HITS.with(|c| {
            let mut v = c.get();
            v[arm] += 1;
            c.set(v);
        });
    }
}

/// Everything [`decode_inter_block`] needs to fold temporal MV candidates
/// (spec 7.10.2.8), threaded down from [`decode_inter_frame_tile_with_cdfs`]:
/// the current frame's own projected field ([`crate::motion_field::setup_motion_field`]'s
/// output, built by the caller when `header.use_ref_frame_mvs` is set) plus
/// the order-hint bookkeeping [`crate::motion_field::get_relative_dist`]
/// needs to turn a query block's own resolved `ref_frame` into
/// `TplArgs::cur_offset_0` (spec 7.9.3).
pub(crate) struct TplFrameArgs<'a> {
    pub field: &'a crate::motion_field::TplField,
    pub order_hint_bits: u32,
    pub order_hint: u32,
    /// `ref_order_hints[ref_frame - LAST_FRAME]` for `ref_frame` in
    /// `LAST_FRAME..=ALTREF_FRAME` — the OrderHint of the picture this
    /// frame's own `ref_frame_idx` names for each single reference.
    pub ref_order_hints: [u32; 7],
}

/// Builds this frame's own saved motion field (libaom `av1_copy_frame_mvs`,
/// spec 7.9's per-frame `MotionFieldMvs` storage step) from its fully
/// decoded `grid`: every 4x4 unit an inter block covers reads the same
/// `MiInfo` ([`decode_inter_block`]/`decode_inter_block8` write it
/// block-uniform), so sampling any one 4x4 unit inside each 8x8 cell is
/// exact — every block this crate ever codes is at least 8x8, so no 8x8
/// cell straddles two different blocks' values.
pub(crate) fn build_motion_field(
    grid: &MiGrid,
    mi_cols: usize,
    mi_rows: usize,
    order_hint: u32,
    order_hint_bits: u32,
    ref_order_hints: [u32; 7],
    fctx: &FrameCtx,
) -> crate::motion_field::MotionField {
    let mut field =
        crate::motion_field::MotionField::new(mi_cols, mi_rows, order_hint, ref_order_hints, false);
    // lane-interbis r1: libaom `av1_calculate_ref_frame_side` -- a reference
    // whose OrderHint is AHEAD of (or equal to) this frame's is "on the other
    // side", and `av1_copy_frame_mvs` skips it entirely, so a compound block
    // referencing one stores only its OTHER side's mv. With
    // `enable_order_hint` off (`order_hint_bits == 0`) every side is 0 and
    // nothing is skipped.
    // lane-mf: hoisted out of the per-4x4-unit loop -- the answer depends
    // only on the reference frame, so it is 7 table lookups per frame instead
    // of two per inter unit.
    let other_side: [bool; 8] = std::array::from_fn(|rf| {
        let i = rf.wrapping_sub(LAST_FRAME as usize);
        order_hint_bits != 0
            && i < 7
            && (crate::motion_field::get_relative_dist(
                order_hint_bits,
                ref_order_hints[i],
                order_hint,
            ) > 0
                || ref_order_hints[i] == order_hint)
    });
    // libaom `REFMVS_LIMIT` (`mvref_common.h`): a stored motion field mv is
    // capped at 12 bits per component; a longer one is not stored at all.
    const REFMVS_LIMIT: i32 = (1 << 12) - 1;
    let (cols, rows, cells) = field.cells_mut();
    // Fills cell rows `r0..r1`; `out` is exactly those rows' cells, so bands
    // never touch each other (an 8x8 cell lives in one cell row, and mi row
    // `row` writes cell row `row / 2`).
    let fill = |r0: usize, r1: usize, out: &mut [Option<crate::motion_field::SavedMv>]| {
        for row in (r0 * 2)..(r1 * 2).min(mi_rows) {
            let out = &mut out[(row / 2 - r0) * cols..][..cols];
            for col in 0..mi_cols {
                let Some(info) = grid.get_frame(row, col) else {
                    continue;
                };
                // `av1_copy_frame_mvs` runs for EVERY block of an inter frame and
                // CLEARS the cells it covers first (`mv->ref_frame = NONE_FRAME`),
                // so an intra block -- or an inter block whose every slot is on the
                // other side / over the limit -- wipes what an earlier block wrote
                // into the same 8x8 cell (only reachable from sub-8x8 leaves, which
                // share a cell).
                if !info.is_inter {
                    out[col / 2] = None;
                    continue;
                }
                // `av1_copy_frame_mvs` walks BOTH reference slots and the LAST
                // qualifying one wins -- a compound block stores its second
                // reference's mv, never its first (unless the second is on the
                // other side or over the limit). Storing slot 0 unconditionally
                // put the wrong (mv, ref_frame_offset) pair in every compound
                // block's cell, which a later frame's temporal candidate then
                // projected with the wrong reference distance.
                let mut saved: Option<crate::motion_field::SavedMv> = None;
                for (rf, mv) in [
                    (info.ref_frame, Some(mv32(info.mv))),
                    (info.ref_frame1.max(0), Some(mv32(info.mv1))),
                ] {
                    let (Some(mv), true) = (mv, rf > 0) else {
                        continue;
                    };
                    if other_side[rf as usize] {
                        continue;
                    }
                    if mv.0.abs() > REFMVS_LIMIT || mv.1.abs() > REFMVS_LIMIT {
                        continue;
                    }
                    saved = Some(crate::motion_field::SavedMv { mv, ref_frame: rf });
                }
                out[col / 2] = saved;
            }
        }
    };
    // lane-mf: the whole-frame pass split over the filter pool in cell-row
    // bands, each band owning its own disjoint slice of `cells`.
    let bands = crate::par::bands(rows, crate::par::filter_threads());
    crate::par::run_bands_mut(&bands, cols, fctx, cells, |r0, r1, mine, _| {
        fill(r0, r1, mine);
    });
    if crate::envflags::env_flag!("EC_TRACE_TPL") {
        eprintln!("EC_TPL_SAVED oh={order_hint}");
        for (r, row) in field.occupancy().iter().enumerate() {
            eprintln!("EC_TPL_SAVED r{r} {row}");
        }
    }
    field
}

/// The sequence header's `enable_intra_edge_filter`, set once per
/// [`decode_key_frame_tile_with_cdfs`] call: [`PlaneBuf::reconstruct`]'s
/// own read of it, kept off the call stack (see that function's own doc
/// comment for why) rather than threaded through every `read_plane`/
/// `decode_block`/`decode_leaf8` call between here and there.
/// Set [`ENABLE_EDGE_FILTER`] for this thread.
///
/// `decode_key_frame_tile_with_cdfs` sets it, but `decode_inter_frame_tile`
/// never did -- so an inter frame's intra blocks read whatever value the last
/// key frame decoded ON THIS THREAD left behind. Inside one stream that is
/// harmless (its own key frame sets it first), but a caller that decodes an
/// inter frame directly, or a test harness running several streams on one
/// thread, inherits a foreign sequence's bit: decode stops being a pure
/// function of the bitstream. `decode_stream` now calls this once per stream
/// from the sequence header, which covers every frame including inter ones.
pub(crate) fn set_enable_edge_filter(value: bool, fctx: &crate::decode::FrameCtx) {
    fctx.enable_edge_filter.with(|f| f.set(value));
}

thread_local! {
    static CDEF_SKIPPED_UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // lane-cdef r2: (c) 8x8 CDEF units that are FILTERED and whose extent
    // crosses the cropped frame edge -- the only units for which "clamp at
    // the crop" and "pad past the mi-rounded extent with CDEF_VERY_LARGE"
    // (libaom `cdef.c:249-256`, `cdef_prepare_fb`'s hsize/vsize are
    // `nhb << mi_wide_l2` off the MI grid, not the crop) differ. A frame
    // whose width or height is not a multiple of 8 is the only shape that
    // produces one; the straddling-band gate arms assert it grew.
    static CDEF_STRADDLE_UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // lane-cdefstrip r1: 8x8 CDEF units visited with an UNWRITTEN skip band
    // (see `Neighbours::skip_written`).
    static CDEF_UNWRITTEN_SKIP_UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CDEF_MIXED_SKIP_UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of `CDEF_MIXED_SKIP_UNITS`.
pub fn cdef_mixed_skip_units() -> usize {
    CDEF_MIXED_SKIP_UNITS.with(|c| c.get())
}

/// Current value of `CDEF_SKIPPED_UNITS`.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn cdef_skipped_units() -> usize {
    CDEF_SKIPPED_UNITS.with(|c| c.get())
}

/// How many 8x8 CDEF units [`apply_cdef`] visited whose skip band no coded
/// block wrote this frame -- a writer gap, always a defect (lane-cdefstrip r1).
pub fn cdef_unwritten_skip_units() -> usize {
    CDEF_UNWRITTEN_SKIP_UNITS.with(|c| c.get())
}

/// Current value of `CDEF_STRADDLE_UNITS`.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn cdef_straddle_units() -> usize {
    CDEF_STRADDLE_UNITS.with(|c| c.get())
}

fn note_sub8_chroma_tx(first_sub: TxType, chroma_ref: TxType, coeffs: bool) {
    if coeffs && first_sub != chroma_ref {
        hit!(SUB8_CHROMA_TX_FROM_REF_HITS);
    }
}

/// Counts one [`CHROMA_EOB_CLASS1_HITS`] when `plane_idx`'s inherited
/// `tx_type` put this block's `eob_pt` on the class-1 row.
fn note_chroma_class1(plane_idx: usize, tx_type: TxType) {
    if plane_idx > 0 && TxClass::of(tx_type) != TxClass::TwoD {
        hit!(CHROMA_EOB_CLASS1_HITS);
    }
}

// Which plane the in-flight INTRA reconstruction is writing (0=Y, 1=U, 2=V).
// lane-av1cmpaudit: aomdec's `EC_PREDOUT8`/`EC_PREDND` print `plane=`, and
// without the same field on our side an `EC_PREDOUT8` vs `OUR_PRED` diff of a
// per-plane prediction cannot be read off ours -- a luma/chroma disagreement
// looked identical to a same-plane disagreement. Set by `exec_intra` (the
// single place a `PlaneBuf::reconstruct{,_rect}` is reached from) and read only
// by the `EC_PRED` rung, so it cannot change a decode.
thread_local! {
    static RECON_PLANE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The plane index the `EC_PRED` rung prints as `plane=`.
fn recon_plane() -> usize {
    RECON_PLANE.with(std::cell::Cell::get)
}

/// Restores [`RECON_PLANE`] on drop, so `exec_intra` needs no cleanup path on
/// either of its two early returns.
struct PlaneGuard(usize);
impl Drop for PlaneGuard {
    fn drop(&mut self) {
        RECON_PLANE.with(|c| c.set(self.0));
    }
}

const SUB_MI: u32 = 4;
const MI: usize = 4;
const SB: usize = 64;
/// The `above_side`/`left_side` value a cell with no decoded neighbour holds
/// (tile start, superblock-row start).
///
/// [`Neighbours::partition_ctx_mi`] reads it as libaom's
/// `av1_get_partition_plane_context` reads its own zero-initialised
/// `above_partition_context` byte: bit `bsl` clear, i.e. "the neighbour is at
/// least as wide as this block". lane-sb128 r1: the old sentinel was `SB`
/// (64), which reads identically at every level up to 64x64 but sets the bit
/// at the `BLOCK_128X128` root (64 * 2 <= 128) -- the 128-level partition
/// symbol was read off CDF row 3 where libaom uses row 0 (aomdec
/// `EC_PART mi_row=0 mi_col=0 bsize=15 ctx=16`, i.e. `bsl*4 + 0`).
const NO_NEIGHBOUR_SIDE: usize = 128;
const BLOCK: usize = 32;
const SUB: usize = 16;
const TX32: usize = 32;
const TX16: usize = 16;
const TX8: usize = 8;
const TX4: usize = 4;
const DC_PRED: usize = 0;
/// `V_PRED` (spec 6.10.2): the first directional mode, and so the first that
/// carries an angle delta — [`crate::tile`]'s own private copy of the same
/// constant.
const V_PRED: usize = 1;
const H_PRED: usize = 2;
/// `D157_PRED`: `fimode_to_intradir`'s target for `FILTER_D157_PRED` (spec
/// `Fimode_To_Intradir`, libaom `blockd.h`).
const D157_PRED: usize = 6;
/// The last directional mode, `D67_PRED`.
const D67_PRED: usize = 8;
/// The symbol an angle delta of zero codes as (spec 9.3): the alphabet runs
/// from -3 to +3, so this is its middle — [`crate::tile`]'s writer never
/// codes anything else, so this decoder never reads anything else either.
const ANGLE_DELTA_ZERO: usize = 3;
const NUM_BASE_LEVELS: i32 = 2;
const COEFF_BASE_RANGE: i32 = 12;
const BR_STEP: i32 = 3;
const MAX_BR_LEVEL: i32 = NUM_BASE_LEVELS + COEFF_BASE_RANGE;

/// `partition_gather_vert_alike`/`_horz_alike` (spec 9.3): the partition types
/// whose probability mass a superblock or block half outside the frame
/// gathers into its one remaining flag — [`crate::tile`]'s own private copy,
/// duplicated here (that module is another lane's territory this round) and
/// kept in step with it by the byte-exact round-trip tests below rather than
/// by sharing code.
const VERT_ALIKE: [usize; 6] = [2, PARTITION_SPLIT, 4, 6, 7, 9];
const HORZ_ALIKE: [usize; 6] = [1, PARTITION_SPLIT, 4, 5, 6, 8];

fn trace_edge(mi_r: usize, mi_c: usize, bsize: u32, ctx: usize, rng: u32) {
    if crate::envflags::env_flag!("EC_AV1_TRACE") {
        eprintln!("OUR_PART mi_row={mi_r} mi_col={mi_c} bsize={bsize} ctx={ctx} rng={rng}");
    }
}

fn element_prob(cdf: &[u16], element: usize) -> u16 {
    cdf[element] - if element > 0 { cdf[element - 1] } else { 0 }
}

/// The two-symbol CDF a partial superblock/block's forced-split flag is read
/// with: the mass of `elements` becomes the probability of the one partition
/// the frame edge still allows, mirroring [`crate::tile`]'s own `gather`
/// (the encoder's non-adapting counterpart to this read).
fn gather(cdf: &[u16], elements: [usize; 6]) -> [u16; 3] {
    let split: u16 = elements.iter().map(|&e| element_prob(cdf, e)).sum();
    [32768 - split, 32768, 0]
}

fn unsupported(what: impl Into<String>) -> Error {
    Error::unsupported("AV1 tile", what.into())
}

/// The coefficient q-context a frame's `base_q_idx` picks its default CDFs
/// from (spec `Get_Qctx`), mirroring [`crate::tile`]'s private copy of the
/// same rule.
pub(crate) fn q_ctx_of(base_q_idx: u8) -> usize {
    match base_q_idx {
        0..=20 => 0,
        21..=60 => 1,
        61..=120 => 2,
        _ => 3,
    }
}

/// `Default_Scan_NxN` (spec 8.4.2), the same anti-diagonal walk
/// [`crate::tile`]'s writer generates its scan tables with.
fn default_scan(side: usize) -> &'static [u16] {
    // One table per transform side (4/8/16/32), built once: this used to be a
    // fresh heap allocation plus the whole anti-diagonal walk on every
    // transform block read.
    static SCANS: std::sync::LazyLock<[Vec<u16>; 4]> = std::sync::LazyLock::new(|| {
        [
            default_scan_gen(4),
            default_scan_gen(8),
            default_scan_gen(16),
            default_scan_gen(32),
        ]
    });
    assert!(
        matches!(side, 4 | 8 | 16 | 32),
        "no default scan for a {side}-point transform"
    );
    &SCANS[side.trailing_zeros() as usize - 2]
}

fn default_scan_gen(side: usize) -> Vec<u16> {
    let mut scan = Vec::with_capacity(side * side);
    for d in 0..(2 * side - 1) {
        let lo = d.saturating_sub(side - 1);
        let hi = d.min(side - 1);
        let diagonal = (lo..=hi).map(|row| (row * side + (d - row)) as u16);
        if d % 2 == 0 {
            scan.extend(diagonal.rev());
        } else {
            scan.extend(diagonal);
        }
    }
    scan
}

/// `Mrow_Scan`/`Mcol_Scan` (spec 8.4.2, libaom `scan.c`): the two
/// class-specific scans a `V_DCT`/`H_DCT` transform block reads coefficients
/// in, instead of the default zigzag. libaom's own tables are keyed by a
/// *column-major* raster (`get_txb_bhl`'s `col = pos / height`) over its
/// physically column-major level buffer, but they decompose to the exact
/// same (row, col) walk as a flat row-major traversal of *our* row-major
/// `grid`/`levels` (`pos = row * side + col`) once you read them that way:
/// `Mrow_Scan` visits row 0 across every column, then row 1, ... — precisely
/// `0..side*side` in our own indexing — and `Mcol_Scan` is that walk
/// transposed (column 0 down every row, then column 1, ...).
pub(crate) fn class_scan_table(side: usize, class: TxClass) -> Vec<u16> {
    class_scan_table_wh(side, side, class)
}

/// [`class_scan_table`] widened to `(w, h)` (lane-tx4x8). Checked against
/// libaom's own `mrow_scan_4x8`/`mcol_scan_4x8`/`mrow_scan_8x4`/
/// `mcol_scan_8x4` (`scan.c`) converted out of their column-major encoding
/// by `(p % h) * w + p / h`, the same conversion [`SCAN_16X8`] was
/// transcribed with -- `mrow` is the plain row-major raster in our indexing
/// and `mcol` its transpose, at rect sizes exactly as at square ones.
fn class_scan_table_wh(w: usize, h: usize, class: TxClass) -> Vec<u16> {
    match class {
        TxClass::Vert => (0..(w * h) as u16).collect(),
        TxClass::Horiz => (0..w)
            .flat_map(|col| (0..h).map(move |row| (row * w + col) as u16))
            .collect(),
        TxClass::TwoD => unreachable!("class_scan is only for V_DCT/H_DCT"),
    }
}

fn eob_coeff_ctx(scan_idx: usize, area: usize) -> usize {
    match scan_idx {
        0 => 0,
        i if i <= area / 8 => 1,
        i if i <= area / 4 => 2,
        _ => 3,
    }
}

/// One cell of [`read_coeffs`]'s level buffer.
///
/// lane-coef2: the buffer is now PADDED (libaom's `TX_PAD_HOR`/`TX_PAD_VER`,
/// `txb_common.h`) -- four zero columns to the right of `side` and four zero
/// rows below it, at a fixed `stride = side + 4` -- so the four-away reads
/// `base_ctx` makes (`(0, 4)`/`(4, 0)` are its widest offsets) land on real
/// zero cells instead of needing the `row >= side || col >= side` edge test
/// this used to run five times per coefficient. Nothing writes a padding
/// cell, so the values are the same zeros the test returned.
#[inline]
fn neighbour(grid: &[u8], stride: usize, row: usize, col: usize) -> i32 {
    grid[row * stride + col] as i32
}

/// spec 5.11.39's three `TxClass`es (`tx_type_to_class`, libaom
/// `txb_common.h`): everything but the two lone-axis-identity types this
/// decoder reads (`V_DCT`/`H_DCT`) is `TX_CLASS_2D`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TxClass {
    TwoD,
    Horiz,
    Vert,
}

impl TxClass {
    pub(crate) fn of(tx_type: TxType) -> Self {
        match tx_type {
            // `TX_CLASS_VERT`/`TX_CLASS_HORIZ` (`txb_common.h`'s `tx_type_to_class`)
            // key on axis alone -- FLIPADST is still ADST for this purpose.
            TxType::VDct | TxType::VAdst | TxType::VFlipAdst => Self::Vert,
            TxType::HDct | TxType::HAdst | TxType::HFlipAdst => Self::Horiz,
            _ => Self::TwoD,
        }
    }
}

/// `nz_map_ctx_offset_1d`: `TX_CLASS_HORIZ`/`TX_CLASS_VERT` share one 32-entry
/// table (indexed by the position along the transform's *coded* axis --
/// `col` for horiz, `row` for vert) that places their contexts past the 2D
/// table's own 26 rows (`SIG_COEF_CONTEXTS_2D`), 16 rows total
/// (`SIG_COEF_CONTEXTS_1D`) split 26/31/36.
pub(crate) fn nz_map_ctx_offset_1d(i: usize) -> usize {
    match i {
        0 => 26,
        1 => 31,
        _ => 36,
    }
}

fn base_ctx(
    grid: &[u8],
    stride: usize,
    row: usize,
    col: usize,
    class: TxClass,
    rect_shape: Option<(usize, usize)>,
) -> usize {
    // libaom's `get_nz_map_ctx_from_stats` only special-cases the DC
    // coefficient to ctx 0 for `TX_CLASS_2D` (`(tx_class | coeff_idx) == 0`,
    // txb_common.h) -- `V_DCT`/`H_DCT` (class `Horiz`/`Vert`) still run the DC
    // position through the full 1D neighbour sum and offset table. Skipping
    // that for every class desynced the last (DC) coefficient of a
    // `TX_CLASS_HORIZ` block (lane-av1tx4 final round: real ctx 28 read as
    // 0, which then read one extra `BR` symbol at the max and forced a
    // spurious third Golomb call).
    if class == TxClass::TwoD && row == 0 && col == 0 {
        return 0;
    }
    let offsets: [(usize, usize); 5] = match class {
        TxClass::TwoD => [(1, 0), (0, 1), (1, 1), (2, 0), (0, 2)],
        TxClass::Horiz => [(1, 0), (0, 1), (0, 2), (0, 3), (0, 4)],
        TxClass::Vert => [(1, 0), (0, 1), (2, 0), (3, 0), (4, 0)],
    };
    let mag: i32 = offsets
        .iter()
        .map(|&(dr, dc)| neighbour(grid, stride, row + dr, col + dc).min(3))
        .sum();
    let ctx = ((mag + 1) >> 1).min(4) as usize;
    match class {
        // libaom's CDF *set* for a truncated 64-wide/tall corner resolves
        // through `get_txsize_entropy_ctx` to the square `TX_64X64` (this
        // decoder's `TxbSet::Luma64`), but `get_nz_map_ctx_from_stats`
        // separately indexes `av1_nz_map_ctx_offset` by the raw, un-adjusted
        // `tx_size` -- `TX_32X64`/`TX_64X32` for a superblock-level HORZ/VERT
        // strip, which own a genuinely different table from the square one
        // (lane-sbpart r8 root cause: `(row=1, col=0)` reads ctx 2 under the
        // square table vs the real ctx 13 the rect table gives).
        TxClass::TwoD => {
            // lane-rectsplit r4: apply libaom's GENERATING RULE
            // (`txb_common.h:199-209`) directly instead of indexing the
            // transcribed 5x5 tables. `av1_nz_map_ctx_offset[tx_size]` is a
            // FLAT array read at `coeff_idx`, and `coeff_idx` is COLUMN-major
            // (libaom decomposes it as `col = coeff_idx >> bhl`, `bhl` the
            // ADJUSTED height's log2, 5 for both `TX_64X32` and `TX_32X64`),
            // so 32 consecutive entries are one COLUMN: the [`cdf`]
            // transcriptions are `[col][row]` and this read had them as
            // `[row][col]` -- the transpose, i.e. the other shape's rule with
            // 11 and 16 swapped. Same values as [`base_ctx_rect`] now, stated
            // once; `base_ctx_rect_offsets_match_the_transcribed_tables_over_the_whole_domain`
            // pins the rule against the tables over their whole 5x5 domain.
            if let Some((w, h)) = rect_shape {
                if crate::envflags::env_flag!("EC_NZOFF_DUMP") {
                    eprintln!("NZOFF stride={stride} shape={w}x{h} row={row} col={col}");
                }
                if w < h && row < 2 {
                    return ctx + 11;
                }
                if w > h && col < 2 {
                    return ctx + 16;
                }
            }
            ctx + cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize
        }
        TxClass::Horiz => ctx + nz_map_ctx_offset_1d(col.min(31)),
        TxClass::Vert => ctx + nz_map_ctx_offset_1d(row.min(31)),
    }
}

/// The five `base_ctx` neighbour taps and the three `br_ctx` ones as FLAT
/// offsets into the padded level frame (lane-coefread): the tap pattern is a
/// function of the tx class alone, which is fixed for a whole transform unit,
/// so resolving it once per unit takes the class match and the `(dr, dc)`
/// -> `dr * stride + dc` arithmetic out of the per-coefficient path. Same
/// taps as [`base_ctx`]/[`br_ctx`] read, stated once;
/// `flat_taps_match_the_row_col_neighbour_reads` pins them against the
/// original `(dr, dc)` form.
fn flat_taps(stride: usize, class: TxClass) -> ([usize; 5], [usize; 3]) {
    match class {
        TxClass::TwoD => (
            [stride, 1, stride + 1, 2 * stride, 2],
            [stride, 1, stride + 1],
        ),
        TxClass::Horiz => ([stride, 1, 2, 3, 4], [stride, 1, 2]),
        TxClass::Vert => (
            [stride, 1, 2 * stride, 3 * stride, 4 * stride],
            [stride, 1, 2 * stride],
        ),
    }
}

/// [`base_ctx`]'s neighbour sum, off flat taps: `((sum of five clamped
/// neighbours) + 1) >> 1`, clamped to 4.
#[inline(always)]
fn base_mag(grid: &[u8], idx: usize, taps: &[usize; 5]) -> usize {
    let mut mag = 0u32;
    for &t in taps {
        mag += u32::from(grid[idx + t]).min(3);
    }
    (((mag + 1) >> 1) as usize).min(4)
}

/// [`br_ctx`]'s neighbour sum, off flat taps.
#[inline(always)]
fn br_mag(grid: &[u8], idx: usize, taps: &[usize; 3]) -> usize {
    let mut mag = 0u32;
    for &t in taps {
        mag += u32::from(grid[idx + t]);
    }
    (((mag + 1) >> 1) as usize).min(6)
}

/// lane-coefread: superseded on the decode path by [`br_mag`] plus the
/// origin-offset tail the two coefficient readers state inline; kept as the
/// `(row, col)` form `flat_taps_match_the_row_col_neighbour_reads` pins the
/// flat one against.
#[cfg(test)]
fn br_ctx(grid: &[u8], stride: usize, row: usize, col: usize, class: TxClass) -> usize {
    let extra = match class {
        TxClass::TwoD => neighbour(grid, stride, row + 1, col + 1),
        TxClass::Horiz => neighbour(grid, stride, row, col + 2),
        TxClass::Vert => neighbour(grid, stride, row + 2, col),
    };
    let mag = neighbour(grid, stride, row + 1, col) + neighbour(grid, stride, row, col + 1) + extra;
    let mag = (((mag + 1) >> 1).min(6)) as usize;
    if row == 0 && col == 0 {
        return mag;
    }
    let near_origin = match class {
        TxClass::TwoD => row < 2 && col < 2,
        TxClass::Horiz => col == 0,
        TxClass::Vert => row == 0,
    };
    if near_origin { mag + 7 } else { mag + 14 }
}

/// `Default_Scan_32x16` (spec 8.4.2, libaom `scan.c`'s `default_scan_32x16`):
/// transposed from libaom's column-major buffer into this decoder's row-major
/// `pos = row * 32 + col` (lane-rectwire, verified as a bijection of `0..512`
/// against the real table). Not the square zigzag formula generalized to
/// unequal `w`/`h` -- checked and that does NOT reproduce this table.
const SCAN_32X16: [u16; 512] = [
    0, 32, 1, 64, 33, 2, 96, 65, 34, 3, 128, 97, 66, 35, 4, 160, 129, 98, 67, 36, 5, 192, 161, 130,
    99, 68, 37, 6, 224, 193, 162, 131, 100, 69, 38, 7, 256, 225, 194, 163, 132, 101, 70, 39, 8,
    288, 257, 226, 195, 164, 133, 102, 71, 40, 9, 320, 289, 258, 227, 196, 165, 134, 103, 72, 41,
    10, 352, 321, 290, 259, 228, 197, 166, 135, 104, 73, 42, 11, 384, 353, 322, 291, 260, 229, 198,
    167, 136, 105, 74, 43, 12, 416, 385, 354, 323, 292, 261, 230, 199, 168, 137, 106, 75, 44, 13,
    448, 417, 386, 355, 324, 293, 262, 231, 200, 169, 138, 107, 76, 45, 14, 480, 449, 418, 387,
    356, 325, 294, 263, 232, 201, 170, 139, 108, 77, 46, 15, 481, 450, 419, 388, 357, 326, 295,
    264, 233, 202, 171, 140, 109, 78, 47, 16, 482, 451, 420, 389, 358, 327, 296, 265, 234, 203,
    172, 141, 110, 79, 48, 17, 483, 452, 421, 390, 359, 328, 297, 266, 235, 204, 173, 142, 111, 80,
    49, 18, 484, 453, 422, 391, 360, 329, 298, 267, 236, 205, 174, 143, 112, 81, 50, 19, 485, 454,
    423, 392, 361, 330, 299, 268, 237, 206, 175, 144, 113, 82, 51, 20, 486, 455, 424, 393, 362,
    331, 300, 269, 238, 207, 176, 145, 114, 83, 52, 21, 487, 456, 425, 394, 363, 332, 301, 270,
    239, 208, 177, 146, 115, 84, 53, 22, 488, 457, 426, 395, 364, 333, 302, 271, 240, 209, 178,
    147, 116, 85, 54, 23, 489, 458, 427, 396, 365, 334, 303, 272, 241, 210, 179, 148, 117, 86, 55,
    24, 490, 459, 428, 397, 366, 335, 304, 273, 242, 211, 180, 149, 118, 87, 56, 25, 491, 460, 429,
    398, 367, 336, 305, 274, 243, 212, 181, 150, 119, 88, 57, 26, 492, 461, 430, 399, 368, 337,
    306, 275, 244, 213, 182, 151, 120, 89, 58, 27, 493, 462, 431, 400, 369, 338, 307, 276, 245,
    214, 183, 152, 121, 90, 59, 28, 494, 463, 432, 401, 370, 339, 308, 277, 246, 215, 184, 153,
    122, 91, 60, 29, 495, 464, 433, 402, 371, 340, 309, 278, 247, 216, 185, 154, 123, 92, 61, 30,
    496, 465, 434, 403, 372, 341, 310, 279, 248, 217, 186, 155, 124, 93, 62, 31, 497, 466, 435,
    404, 373, 342, 311, 280, 249, 218, 187, 156, 125, 94, 63, 498, 467, 436, 405, 374, 343, 312,
    281, 250, 219, 188, 157, 126, 95, 499, 468, 437, 406, 375, 344, 313, 282, 251, 220, 189, 158,
    127, 500, 469, 438, 407, 376, 345, 314, 283, 252, 221, 190, 159, 501, 470, 439, 408, 377, 346,
    315, 284, 253, 222, 191, 502, 471, 440, 409, 378, 347, 316, 285, 254, 223, 503, 472, 441, 410,
    379, 348, 317, 286, 255, 504, 473, 442, 411, 380, 349, 318, 287, 505, 474, 443, 412, 381, 350,
    319, 506, 475, 444, 413, 382, 351, 507, 476, 445, 414, 383, 508, 477, 446, 415, 509, 478, 447,
    510, 479, 511,
];

/// `Default_Scan_16x32`, this decoder's own row-major transcription (see
/// [`SCAN_32X16`]'s doc comment).
const SCAN_16X32: [u16; 512] = [
    0, 1, 16, 2, 17, 32, 3, 18, 33, 48, 4, 19, 34, 49, 64, 5, 20, 35, 50, 65, 80, 6, 21, 36, 51,
    66, 81, 96, 7, 22, 37, 52, 67, 82, 97, 112, 8, 23, 38, 53, 68, 83, 98, 113, 128, 9, 24, 39, 54,
    69, 84, 99, 114, 129, 144, 10, 25, 40, 55, 70, 85, 100, 115, 130, 145, 160, 11, 26, 41, 56, 71,
    86, 101, 116, 131, 146, 161, 176, 12, 27, 42, 57, 72, 87, 102, 117, 132, 147, 162, 177, 192,
    13, 28, 43, 58, 73, 88, 103, 118, 133, 148, 163, 178, 193, 208, 14, 29, 44, 59, 74, 89, 104,
    119, 134, 149, 164, 179, 194, 209, 224, 15, 30, 45, 60, 75, 90, 105, 120, 135, 150, 165, 180,
    195, 210, 225, 240, 31, 46, 61, 76, 91, 106, 121, 136, 151, 166, 181, 196, 211, 226, 241, 256,
    47, 62, 77, 92, 107, 122, 137, 152, 167, 182, 197, 212, 227, 242, 257, 272, 63, 78, 93, 108,
    123, 138, 153, 168, 183, 198, 213, 228, 243, 258, 273, 288, 79, 94, 109, 124, 139, 154, 169,
    184, 199, 214, 229, 244, 259, 274, 289, 304, 95, 110, 125, 140, 155, 170, 185, 200, 215, 230,
    245, 260, 275, 290, 305, 320, 111, 126, 141, 156, 171, 186, 201, 216, 231, 246, 261, 276, 291,
    306, 321, 336, 127, 142, 157, 172, 187, 202, 217, 232, 247, 262, 277, 292, 307, 322, 337, 352,
    143, 158, 173, 188, 203, 218, 233, 248, 263, 278, 293, 308, 323, 338, 353, 368, 159, 174, 189,
    204, 219, 234, 249, 264, 279, 294, 309, 324, 339, 354, 369, 384, 175, 190, 205, 220, 235, 250,
    265, 280, 295, 310, 325, 340, 355, 370, 385, 400, 191, 206, 221, 236, 251, 266, 281, 296, 311,
    326, 341, 356, 371, 386, 401, 416, 207, 222, 237, 252, 267, 282, 297, 312, 327, 342, 357, 372,
    387, 402, 417, 432, 223, 238, 253, 268, 283, 298, 313, 328, 343, 358, 373, 388, 403, 418, 433,
    448, 239, 254, 269, 284, 299, 314, 329, 344, 359, 374, 389, 404, 419, 434, 449, 464, 255, 270,
    285, 300, 315, 330, 345, 360, 375, 390, 405, 420, 435, 450, 465, 480, 271, 286, 301, 316, 331,
    346, 361, 376, 391, 406, 421, 436, 451, 466, 481, 496, 287, 302, 317, 332, 347, 362, 377, 392,
    407, 422, 437, 452, 467, 482, 497, 303, 318, 333, 348, 363, 378, 393, 408, 423, 438, 453, 468,
    483, 498, 319, 334, 349, 364, 379, 394, 409, 424, 439, 454, 469, 484, 499, 335, 350, 365, 380,
    395, 410, 425, 440, 455, 470, 485, 500, 351, 366, 381, 396, 411, 426, 441, 456, 471, 486, 501,
    367, 382, 397, 412, 427, 442, 457, 472, 487, 502, 383, 398, 413, 428, 443, 458, 473, 488, 503,
    399, 414, 429, 444, 459, 474, 489, 504, 415, 430, 445, 460, 475, 490, 505, 431, 446, 461, 476,
    491, 506, 447, 462, 477, 492, 507, 463, 478, 493, 508, 479, 494, 509, 495, 510, 511,
];

/// `Default_Scan_4x8` (libaom `scan.c`), this decoder's own row-major
/// transcription (see [`SCAN_32X16`]'s doc comment for the conversion, which
/// was re-verified this lane by regenerating [`SCAN_16X8`]/[`SCAN_8X16`] from
/// the same source).
const SCAN_4X8: [u16; 32] = [
    0, 1, 4, 2, 5, 8, 3, 6, 9, 12, 7, 10, 13, 16, 11, 14, 17, 20, 15, 18, 21, 24, 19, 22, 25, 28,
    23, 26, 29, 27, 30, 31,
];

/// `Default_Scan_8x4`, same source and conversion as [`SCAN_4X8`].
const SCAN_8X4: [u16; 32] = [
    0, 8, 1, 16, 9, 2, 24, 17, 10, 3, 25, 18, 11, 4, 26, 19, 12, 5, 27, 20, 13, 6, 28, 21, 14, 7,
    29, 22, 15, 30, 23, 31,
];

/// `Default_Scan_16x8`, this decoder's own row-major transcription (see
/// [`SCAN_32X16`]'s doc comment).
const SCAN_16X8: [u16; 128] = [
    0, 16, 1, 32, 17, 2, 48, 33, 18, 3, 64, 49, 34, 19, 4, 80, 65, 50, 35, 20, 5, 96, 81, 66, 51,
    36, 21, 6, 112, 97, 82, 67, 52, 37, 22, 7, 113, 98, 83, 68, 53, 38, 23, 8, 114, 99, 84, 69, 54,
    39, 24, 9, 115, 100, 85, 70, 55, 40, 25, 10, 116, 101, 86, 71, 56, 41, 26, 11, 117, 102, 87,
    72, 57, 42, 27, 12, 118, 103, 88, 73, 58, 43, 28, 13, 119, 104, 89, 74, 59, 44, 29, 14, 120,
    105, 90, 75, 60, 45, 30, 15, 121, 106, 91, 76, 61, 46, 31, 122, 107, 92, 77, 62, 47, 123, 108,
    93, 78, 63, 124, 109, 94, 79, 125, 110, 95, 126, 111, 127,
];

/// `Default_Scan_8x16`, this decoder's own row-major transcription (see
/// [`SCAN_32X16`]'s doc comment).
const SCAN_8X16: [u16; 128] = [
    0, 1, 8, 2, 9, 16, 3, 10, 17, 24, 4, 11, 18, 25, 32, 5, 12, 19, 26, 33, 40, 6, 13, 20, 27, 34,
    41, 48, 7, 14, 21, 28, 35, 42, 49, 56, 15, 22, 29, 36, 43, 50, 57, 64, 23, 30, 37, 44, 51, 58,
    65, 72, 31, 38, 45, 52, 59, 66, 73, 80, 39, 46, 53, 60, 67, 74, 81, 88, 47, 54, 61, 68, 75, 82,
    89, 96, 55, 62, 69, 76, 83, 90, 97, 104, 63, 70, 77, 84, 91, 98, 105, 112, 71, 78, 85, 92, 99,
    106, 113, 120, 79, 86, 93, 100, 107, 114, 121, 87, 94, 101, 108, 115, 122, 95, 102, 109, 116,
    123, 103, 110, 117, 124, 111, 118, 125, 119, 126, 127,
];

/// `Default_Scan_32x8`, this decoder's own row-major transcription (see
/// [`SCAN_32X16`]'s doc comment) -- the chroma plane of a 64x16
/// `PARTITION_HORZ_4` superblock strip (lane-tx64x16). libaom stores its
/// scans column-major (`p = col * height + row`), so a naive copy of
/// `default_scan_32x8` would silently install the TRANSPOSED 8x32 order
/// (class reference-layout-not-spec): `default_scan_16x8`'s literal bytes
/// are byte-identical to this decoder's [`SCAN_8X16`].
const SCAN_32X8: [u16; 256] = [
    0, 32, 1, 64, 33, 2, 96, 65, 34, 3, 128, 97, 66, 35, 4, 160, 129, 98, 67, 36, 5, 192, 161, 130,
    99, 68, 37, 6, 224, 193, 162, 131, 100, 69, 38, 7, 225, 194, 163, 132, 101, 70, 39, 8, 226,
    195, 164, 133, 102, 71, 40, 9, 227, 196, 165, 134, 103, 72, 41, 10, 228, 197, 166, 135, 104,
    73, 42, 11, 229, 198, 167, 136, 105, 74, 43, 12, 230, 199, 168, 137, 106, 75, 44, 13, 231, 200,
    169, 138, 107, 76, 45, 14, 232, 201, 170, 139, 108, 77, 46, 15, 233, 202, 171, 140, 109, 78,
    47, 16, 234, 203, 172, 141, 110, 79, 48, 17, 235, 204, 173, 142, 111, 80, 49, 18, 236, 205,
    174, 143, 112, 81, 50, 19, 237, 206, 175, 144, 113, 82, 51, 20, 238, 207, 176, 145, 114, 83,
    52, 21, 239, 208, 177, 146, 115, 84, 53, 22, 240, 209, 178, 147, 116, 85, 54, 23, 241, 210,
    179, 148, 117, 86, 55, 24, 242, 211, 180, 149, 118, 87, 56, 25, 243, 212, 181, 150, 119, 88,
    57, 26, 244, 213, 182, 151, 120, 89, 58, 27, 245, 214, 183, 152, 121, 90, 59, 28, 246, 215,
    184, 153, 122, 91, 60, 29, 247, 216, 185, 154, 123, 92, 61, 30, 248, 217, 186, 155, 124, 93,
    62, 31, 249, 218, 187, 156, 125, 94, 63, 250, 219, 188, 157, 126, 95, 251, 220, 189, 158, 127,
    252, 221, 190, 159, 253, 222, 191, 254, 223, 255,
];
/// `Default_Scan_8x32`, [`SCAN_32X8`]'s transpose (the chroma plane of a
/// 16x64 `PARTITION_VERT_4` strip).
const SCAN_8X32: [u16; 256] = [
    0, 1, 8, 2, 9, 16, 3, 10, 17, 24, 4, 11, 18, 25, 32, 5, 12, 19, 26, 33, 40, 6, 13, 20, 27, 34,
    41, 48, 7, 14, 21, 28, 35, 42, 49, 56, 15, 22, 29, 36, 43, 50, 57, 64, 23, 30, 37, 44, 51, 58,
    65, 72, 31, 38, 45, 52, 59, 66, 73, 80, 39, 46, 53, 60, 67, 74, 81, 88, 47, 54, 61, 68, 75, 82,
    89, 96, 55, 62, 69, 76, 83, 90, 97, 104, 63, 70, 77, 84, 91, 98, 105, 112, 71, 78, 85, 92, 99,
    106, 113, 120, 79, 86, 93, 100, 107, 114, 121, 128, 87, 94, 101, 108, 115, 122, 129, 136, 95,
    102, 109, 116, 123, 130, 137, 144, 103, 110, 117, 124, 131, 138, 145, 152, 111, 118, 125, 132,
    139, 146, 153, 160, 119, 126, 133, 140, 147, 154, 161, 168, 127, 134, 141, 148, 155, 162, 169,
    176, 135, 142, 149, 156, 163, 170, 177, 184, 143, 150, 157, 164, 171, 178, 185, 192, 151, 158,
    165, 172, 179, 186, 193, 200, 159, 166, 173, 180, 187, 194, 201, 208, 167, 174, 181, 188, 195,
    202, 209, 216, 175, 182, 189, 196, 203, 210, 217, 224, 183, 190, 197, 204, 211, 218, 225, 232,
    191, 198, 205, 212, 219, 226, 233, 240, 199, 206, 213, 220, 227, 234, 241, 248, 207, 214, 221,
    228, 235, 242, 249, 215, 222, 229, 236, 243, 250, 223, 230, 237, 244, 251, 231, 238, 245, 252,
    239, 246, 253, 247, 254, 255,
];
/// `Default_Scan_16x4`, this decoder's own row-major transcription of
/// libaom's column-major `default_scan_16x4` (`scan.c:66`) -- the CHROMA
/// plane of a 32x8 `PARTITION_HORZ_4` strip at the 32x32 level
/// (lane-tx64x16 r3). The transcription rule (`row * w + col` with
/// `row = p % h`, `col = p / h`) was validated by REGENERATING the existing
/// [`SCAN_16X8`] from `default_scan_16x8` byte for byte before these two
/// were emitted -- a naive copy installs the transpose (class
/// reference-layout-not-spec).
const SCAN_16X4: [u16; 64] = [
    0, 16, 1, 32, 17, 2, 48, 33, 18, 3, 49, 34, 19, 4, 50, 35, 20, 5, 51, 36, 21, 6, 52, 37, 22, 7,
    53, 38, 23, 8, 54, 39, 24, 9, 55, 40, 25, 10, 56, 41, 26, 11, 57, 42, 27, 12, 58, 43, 28, 13,
    59, 44, 29, 14, 60, 45, 30, 15, 61, 46, 31, 62, 47, 63,
];
/// `Default_Scan_4x16`, [`SCAN_16X4`]'s transpose (the chroma plane of an
/// 8x32 `PARTITION_VERT_4` strip).
const SCAN_4X16: [u16; 64] = [
    0, 1, 4, 2, 5, 8, 3, 6, 9, 12, 7, 10, 13, 16, 11, 14, 17, 20, 15, 18, 21, 24, 19, 22, 25, 28,
    23, 26, 29, 32, 27, 30, 33, 36, 31, 34, 37, 40, 35, 38, 41, 44, 39, 42, 45, 48, 43, 46, 49, 52,
    47, 50, 53, 56, 51, 54, 57, 60, 55, 58, 61, 59, 62, 63,
];

/// [`neighbour`]'s rect form (lane-rectwire): same lane-coef2 padding
/// (`stride = w + 4`, four spare rows), so the edge test is the zeros the
/// padding already holds.
#[inline]
fn neighbour_rect(grid: &[u8], stride: usize, row: usize, col: usize) -> i32 {
    grid[row * stride + col] as i32
}

/// [`base_ctx`]'s `TxClass::TwoD` arm widened to `(w, h)` (lane-rectwire):
/// the rect generalization is libaom's own comment in
/// `get_nz_map_ctx_from_stats` (`txb_common.h:189-224`), reduces to the exact
/// same [`cdf::NZ_MAP_CTX_OFFSET_32`] table when `w == h`. Never called for
/// `V_DCT`/`H_DCT` -- geometrically impossible at the two size pairs
/// [`decode_block_rect`] codes (see that function's own doc comment).
fn base_ctx_rect(
    grid: &[u8],
    stride: usize,
    w: usize,
    h: usize,
    row: usize,
    col: usize,
    class: TxClass,
) -> usize {
    // Same `(tx_class | coeff_idx) == 0` guard [`base_ctx`] documents: only
    // the 2D class short-circuits the DC position.
    if class == TxClass::TwoD && row == 0 && col == 0 {
        return 0;
    }
    let offsets: [(usize, usize); 5] = match class {
        TxClass::TwoD => [(1, 0), (0, 1), (1, 1), (2, 0), (0, 2)],
        TxClass::Horiz => [(1, 0), (0, 1), (0, 2), (0, 3), (0, 4)],
        TxClass::Vert => [(1, 0), (0, 1), (2, 0), (3, 0), (4, 0)],
    };
    let mag: i32 = offsets
        .iter()
        .map(|&(dr, dc)| neighbour_rect(grid, stride, row + dr, col + dc).min(3))
        .sum();
    let ctx = ((mag + 1) >> 1).min(4) as usize;
    match class {
        TxClass::TwoD => {
            if w < h && row < 2 {
                return ctx + 11;
            }
            if w > h && col < 2 {
                return ctx + 16;
            }
            ctx + cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize
        }
        // Shape-independent in libaom too (`get_nz_map_ctx_from_stats`'s
        // `TX_CLASS_HORIZ`/`VERT` arms index `nz_map_ctx_offset_1d` by the
        // position along one axis only).
        TxClass::Horiz => ctx + nz_map_ctx_offset_1d(col.min(31)),
        TxClass::Vert => ctx + nz_map_ctx_offset_1d(row.min(31)),
    }
}

/// [`br_ctx`]'s `TxClass::TwoD` arm widened to `(w, h)` (lane-rectwire): the
/// neighbour-offset math itself is shape-independent, only the boundary
/// clamp needs the real bound in each axis.
/// lane-coefread: superseded by [`br_mag`] on the rect decode path too --
/// its body was character-for-character [`br_ctx`]'s, which
/// `flat_taps_match_the_row_col_neighbour_reads` now asserts.
#[cfg(test)]
fn br_ctx_rect(grid: &[u8], stride: usize, row: usize, col: usize, class: TxClass) -> usize {
    let extra = match class {
        TxClass::TwoD => neighbour_rect(grid, stride, row + 1, col + 1),
        TxClass::Horiz => neighbour_rect(grid, stride, row, col + 2),
        TxClass::Vert => neighbour_rect(grid, stride, row + 2, col),
    };
    let mag = neighbour_rect(grid, stride, row + 1, col)
        + neighbour_rect(grid, stride, row, col + 1)
        + extra;
    let mag = (((mag + 1) >> 1).min(6)) as usize;
    if row == 0 && col == 0 {
        return mag;
    }
    let near_origin = match class {
        TxClass::TwoD => row < 2 && col < 2,
        TxClass::Horiz => col == 0,
        TxClass::Vert => row == 0,
    };
    if near_origin { mag + 7 } else { mag + 14 }
}

fn dc_vote(dc: Option<bool>) -> i32 {
    match dc {
        None => 0,
        Some(true) => -1,
        Some(false) => 1,
    }
}

fn dc_sign_ctx(vote: i32) -> usize {
    match vote.signum() {
        0 => 0,
        -1 => 1,
        _ => 2,
    }
}

/// The `tx_type` a CDF symbol names, given the width of the row it was read
/// from.
///
/// The row's own width names its set (spec `Tx_Type_Intra_Inv_Set*` /
/// `Tx_Type_Inter_Inv_Set*`): 17 slots (16 symbols) is `EXT_TX_SET_ALL16`, 13
/// (12) `EXT_TX_SET_DTT9_IDTX_1DDCT`, 8 (7) `TX_SET_INTRA_1`, and everything
/// narrower the reduced `TX_SET_INTRA_2`/`TX_SET_INTER_3` order, whose two
/// members are a prefix of the five.
///
/// One function rather than a `match` at each reader, so
/// [`crate::refusal_inventory`]'s enumeration of every (width, symbol) pair
/// tests the mapping the readers actually use.
pub(crate) fn tx_type_from_symbol(cdf_len: usize, t: usize) -> Option<TxType> {
    match cdf_len {
        17 => TxType::from_symbol_all16(t),
        13 => TxType::from_symbol_set2_12(t),
        8 => TxType::from_symbol_set1(t),
        _ => TxType::from_symbol(t),
    }
}

/// [`tx_type_from_symbol`] inverted: which symbol of a `cdf_len`-wide
/// `tx_type` CDF names `tx_type`, or `None` when that set does not hold it
/// (class `table-and-reader-move-together` -- the writer picks its symbol
/// through the reader's own map, so the two cannot drift).
pub(crate) fn tx_type_symbol(cdf_len: usize, tx_type: TxType) -> Option<usize> {
    (0..cdf_len.saturating_sub(1)).find(|&t| tx_type_from_symbol(cdf_len, t) == Some(tx_type))
}

/// The remainder of a level the base and base-range syntax could not reach
/// (spec 5.11.40): its bit length in unary, then that many of its own bits,
/// most significant first — the exact inverse of [`crate::tile::write_golomb`].
pub(crate) fn read_golomb(dec: &mut SymbolDecoder) -> Result<u32> {
    // THIS CAP IS libaom's OWN, TO THE BIT -- do not "widen" it (lane-golomb2
    // re-read the source: `av1/decoder/decodetxb.c:22-43` counts the same
    // prefix and calls the 21st zero bit `AOM_CODEC_CORRUPT_FRAME`, "Invalid
    // length in read_golomb"; spec 5.11.40 BREAKS at `length == 20` and 5.11.39
    // masks the level with `0xFFFFF`, so no conformant stream carries a longer
    // tail). `refusal_inventory::read_golomb_reads_every_value_a_conformant_
    // stream_can_carry` enumerates that whole value domain.
    //
    // So this refusal never names a reader gap: it is always the SYMPTOM of an
    // earlier desync (class `refusal-hides-a-defect`), and every historical
    // instance was closed by fixing that desync -- lane-dgolomb's
    // `force_integer_mv`, lane-dkey's palette, lane-t900 r12's `uv_mode_grid`.
    // Raising the cap only trades a clean refusal for silent corruption:
    // lane-scaledref r1 tried it (commit ee1f980, reverted in 314ee08) and
    // turned `a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`
    // from a refusal into a frame-0 LUMA MISMATCH.
    let mut length = 1u32;
    while dec.literal(1) == 0 {
        length += 1;
        if length > 20 {
            return Err(unsupported("a Golomb tail longer than this decoder reads"));
        }
    }
    let mut x = 1u32;
    for _ in 1..length {
        x = (x << 1) | dec.literal(1);
    }
    Ok(x - 1)
}

/// `decode_symbol`'s end-of-block position (spec 5.11.39): the inverse of
/// [`crate::tile`]'s `write_eob`.
fn read_eob(dec: &mut SymbolDecoder, coding: &mut TxbTables, class: TxClass) -> usize {
    const GROUP_START: [usize; 12] = [0, 1, 2, 3, 5, 9, 17, 33, 65, 129, 257, 513];
    const OFFSET_BITS: [u32; 12] = [0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

    let trace = crate::envflags::env_flag!("EC_AV1_TRACE");
    // libaom's `eob_flag_cdf*` tables carry a second dimension the 2D scan
    // never touches: `TX_CLASS_HORIZ`/`TX_CLASS_VERT` (`V_DCT`/`H_DCT`) adapt
    // a wholly separate CDF from every other tx_type (`decodetxb.c`'s
    // `eob_multi_ctx`). Missing that split desynced the very first
    // `V_DCT`/`H_DCT` TU this decoder ever produced (lane-av1tx4 r5, caught
    // against a real aomdec trace).
    let eob_pt: &mut [u16] = match (class, coding.eob_pt_class1.as_deref_mut()) {
        (TxClass::TwoD, _) | (_, None) => coding.eob_pt,
        (_, Some(class1)) => class1,
    };
    if crate::envflags::env_flag!("EC_AV1_EOBPT_CDF") {
        let (range, value) = dec.debug_state();
        eprintln!("EC_AV1_EOBPT_CDF {eob_pt:?} range={range} value={value}");
    }
    crate::msac::SymbolDecoder::set_symr_cdf("eob_pt");
    let group = dec.symbol(eob_pt) + 1;
    if trace {
        eprintln!("TRACE eob_pt value={group} rng={}", dec.debug_state().0);
    }
    let bits = OFFSET_BITS[group];
    let mut offset = 0u32;
    if bits > 0 {
        let top = dec.symbol(&mut coding.eob_extra[group - 3]) as u32;
        if trace {
            eprintln!("TRACE eob_extra ctx={} value={top}", group - 3);
        }
        offset = top << (bits - 1);
        if bits > 1 {
            offset |= dec.literal(bits - 1);
        }
    }
    GROUP_START[group] + offset as usize
}

/// One transform block's levels, the inverse of [`crate::tile`]'s
/// `write_coeffs`, returned as a `side * side` grid in raster order, along
/// with the transform type that grid was coded under (`DCT_DCT` when the
/// block's `TxbSet` carries no `tx_type` symbol at all, e.g. 32-point and
/// 64-point transforms, which never code anything else).
///
/// # Errors
/// Returns an error if the `tx_type` symbol decodes to a value outside its
/// CDF's own set (a corrupt or unsupported bitstream) — every value the
/// intra `Tx_Type_Intra_Inv_Set2` and inter `Tx_Type_Inter_Inv_Set3` CDFs can
/// produce dispatches to a real inverse transform.
/// `Intra_Mode_To_Tx_Type` (spec 9.3): chroma's transform type is never its
/// own coded symbol -- `TxbSet::Chroma*`'s `tx_type` slot is always `None`
/// (a chroma block shares no CDF row with `Luma16`'s mode-indexed one) -- so
/// libaom's own `intra_mode_to_tx_type` (`blockd.h`) derives it purely from
/// the plane's own predicted mode instead. Every entry this table can
/// produce is a member of both `TX_SET_INTRA_1` and `_2` (spec 9.3's two
/// intra sets differ only in `IDTX`/`V_DCT`/`H_DCT`, none of which this
/// table ever names), so unlike the CDF-coded luma path there is no
/// ext-tx-set membership check to fall back from here.
pub(crate) fn default_intra_tx_type(mode: u8) -> TxType {
    use crate::intra::{
        D45_PRED, D67_PRED, D113_PRED, D135_PRED, D157_PRED, D203_PRED, DC_PRED, H_PRED,
        PAETH_PRED, SMOOTH_H_PRED, SMOOTH_PRED, SMOOTH_V_PRED, V_PRED,
    };
    match mode {
        DC_PRED | D45_PRED => TxType::DctDct,
        V_PRED | D113_PRED | D67_PRED | SMOOTH_V_PRED => TxType::AdstDct,
        H_PRED | D157_PRED | D203_PRED | SMOOTH_H_PRED => TxType::DctAdst,
        D135_PRED | SMOOTH_PRED | PAETH_PRED => TxType::AdstAdst,
        other => panic!("intra mode {other} has no Intra_Mode_To_Tx_Type entry"),
    }
}

/// `is_smooth` (libaom `reconintra.c`): true for `SMOOTH_PRED..=SMOOTH_H_PRED`
/// (9..=11) — deliberately excludes `PAETH_PRED` (12), which the spec's own
/// `get_intra_edge_filter_type`/libaom `get_filt_type` never treat as smooth.
fn is_smooth_mode(mode: usize) -> bool {
    (crate::intra::SMOOTH_PRED as usize..=crate::intra::SMOOTH_H_PRED as usize).contains(&mode)
}

/// Decodes the payload [`crate::tile::sb_coeff_key_frame_tile`] writes,
/// returning the picture it reconstructs to.
///
/// `mi_cols`/`mi_rows` and `base_q_idx` are the frame header's, as parsed by
/// [`ec_av1_syntax`]. `frame_width`/`frame_height` are the header's own true
/// (render) size — a separate field from `mi_cols`/`mi_rows` (spec
/// `compute_image_size` derives one from the other, but not losslessly: a
/// width that is not a multiple of 8 samples leaves `mi_cols * 4` past it) —
/// what the reconstruction is cropped to on the way out, mirroring
/// [`crate::encode::crop_encoded`].
///
/// # Errors
/// Returns an error when a block's partition, mode, skip flag or transform
/// type is anything this decoder does not reconstruct (round 2: inter, tx
/// types other than `DCT_DCT`, non-CDF-gathered rectangular splits below
/// 16x16, a directional mode's angle delta other than zero, chroma-from-luma),
/// or when the tile payload runs out of the symbols this decode expects (a
/// genuinely foreign stream).
#[allow(dead_code)]
// read only from the `#[cfg(test)]` gates
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_key_frame_tile(
    data: &[u8],
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    frame_width: u32,
    frame_height: u32,
    enable_filter_intra: bool,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    tx_select: bool,
    reduced_tx_set: bool,
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<Picture> {
    decode_key_frame_tile_lr(
        data,
        mi_cols,
        mi_rows,
        base_q_idx,
        frame_width,
        frame_height,
        enable_filter_intra,
        cdef,
        loop_filter,
        tx_select,
        reduced_tx_set,
        allow_screen_content_tools,
        allow_intrabc,
        &LoopRestorationParams::default(),
        fctx,
    )
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tx4x8_table_tests;
