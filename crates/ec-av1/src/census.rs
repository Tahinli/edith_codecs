//! Syntax census: what a stream actually codes, per frame, read off the
//! decoder itself (`EC_AV1_BITCENSUS=1`).
//!
//! Every number here is a decode-side observation, so OUR stream and any
//! other encoder's stream are measured by the same reader -- which is the
//! whole point: a BD-rate gap says our stream is bigger, it never says which
//! syntax it is bigger in. The accountant is off by default and costs one
//! relaxed atomic load per symbol when off.
//!
//! Bits are charged per CDF TABLE, not per hand-labelled call site: a tile
//! registers the address range of every field of its `Cdfs` (see
//! [`register_cdfs`]), and [`charge`] maps the table a symbol read to that
//! field's name. So a new coding tool joins the census the day its CDF joins
//! the struct, with no call-site edits (class `gate-blind-to-feature`).
//!
//! Run it through `examples/syntax_census.rs`; `EC_AV1_THREADS` must be 1
//! (the default) so the frame the counters attribute to is the frame being
//! parsed.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static ON: AtomicBool = AtomicBool::new(false);

/// Whether the accountant is armed (`EC_AV1_BITCENSUS=1`, read once).
#[inline]
pub fn on() -> bool {
    static INIT: OnceLock<bool> = OnceLock::new();
    let armed = *INIT.get_or_init(|| {
        let armed = crate::envflags::var("EC_AV1_BITCENSUS").ok().as_deref() == Some("1");
        ON.store(armed, Ordering::Relaxed);
        armed
    });
    armed
}

/// The per-symbol fast path: one relaxed load, nothing else, when off.
#[inline]
pub(crate) fn armed() -> bool {
    ON.load(Ordering::Relaxed)
}

/// One coded frame's census.
#[derive(Default, Debug, Clone)]
pub struct Frame {
    /// Decode-order index.
    pub idx: usize,
    /// `key` (intra), `arf` (coded but not shown) or `leaf` (coded and shown).
    pub kind: &'static str,
    /// `base_q_idx`.
    pub qindex: u8,
    /// `OrderHint` of this frame, i.e. its display position (lane-arfcen).
    pub order_hint: u32,
    /// `OrderHints[LAST..=ALTREF]`, so a row can name its references' display
    /// distance rather than only which slot it read (lane-arfcen).
    pub ref_hints: [u32; 7],
    /// Tile payload bytes of this frame.
    pub bytes: usize,
    /// `delta_q_present`.
    pub delta_q: bool,
    /// `segmentation_enabled`.
    pub segmentation: bool,
    /// `loop_filter_level[0..2]`.
    pub lf: [u8; 2],
    /// `cdef_bits`, `cdef_y_pri_strength[0]`, `cdef_uv_pri_strength[0]`.
    pub cdef: [u8; 3],
    /// `FrameRestorationType` per plane.
    pub lr: [u8; 3],
    /// Coded block shapes: (width, height) in pixels -> block count.
    pub blocks: BTreeMap<(u16, u16), u64>,
    /// Luma area (in 4x4 units) coded intra / single-ref inter / compound.
    pub area: [u64; 3],
    /// Luma area per reference frame slot (index 0 = intra, 1..=7 =
    /// LAST..ALTREF as `ref_frame` names them).
    pub refs: [u64; 8],
    /// Luma area with `skip` set.
    pub skip_area: u64,
    /// NEWMV / GLOBALMV / NEARESTMV / NEARMV blocks (single-ref reads).
    pub modes: [u64; 4],
    /// Coded motion vectors bucketed by max(|x|,|y|) in eighth-pel:
    /// 0, <=8 (1px), <=32, <=128, <=512, >512.
    pub mv: [u64; 6],
    /// Published luma transform shapes: (width, height) in pixels -> count.
    pub tx: BTreeMap<(u16, u16), u64>,
    /// Coded `tx_type` symbols: (CDF row length, symbol) -> count. The row
    /// length names the transform SET (17 = all 16 types, 13, 8, 6 = the
    /// reduced sets); symbol 0 is that set's first inverse-table entry.
    pub tx_type: BTreeMap<(u8, u8), u64>,
    /// Per-CDF-table (symbol count, bits).
    pub families: BTreeMap<&'static str, (u64, f64)>,
}

#[derive(Default)]
struct State {
    frames: Vec<Frame>,
    /// Sorted (start address, end address, field name) of the live tile's
    /// `Cdfs`.
    tables: Vec<(usize, usize, &'static str)>,
    /// Area of the block most recently published, so `compound` can move it.
    last_block_area: u64,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State::default()))
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(&mut state().lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

fn cur(s: &mut State) -> &mut Frame {
    if s.frames.is_empty() {
        s.frames.push(Frame::default());
    }
    s.frames.last_mut().expect("pushed above")
}

/// Every frame censused since the last call, and clears the record.
pub fn take() -> Vec<Frame> {
    with(|s| std::mem::take(&mut s.frames))
}

/// Opens a new frame's row -- called once per coded frame, before its tiles.
pub(crate) fn frame_start(header: &ec_av1_syntax::FrameHeader, bytes: usize, idx: usize) {
    if !armed() {
        return;
    }
    let kind = match (header.frame_is_intra, header.show_frame) {
        (true, _) => "key",
        (false, false) => "arf",
        (false, true) => "leaf",
    };
    with(|s| {
        s.frames.push(Frame {
            idx,
            kind,
            qindex: header.quantization.base_q_idx,
            order_hint: header.order_hint,
            ref_hints: header.order_hints,
            bytes,
            delta_q: header.delta.q_present,
            segmentation: header.segmentation.enabled,
            lf: [header.loop_filter.level[0], header.loop_filter.level[1]],
            cdef: [
                header.cdef.bits,
                header.cdef.y_pri_strength[0],
                header.cdef.uv_pri_strength[0],
            ],
            lr: [
                header.loop_restoration.frame_restoration_type[0] as u8,
                header.loop_restoration.frame_restoration_type[1] as u8,
                header.loop_restoration.frame_restoration_type[2] as u8,
            ],
            ..Frame::default()
        });
    });
}

/// Registers the address range of every CDF table of the tile's own `Cdfs`,
/// so [`charge`] can name the family a symbol was read with. Called once per
/// tile, right where the tile's symbol decoder is built.
pub(crate) fn register_cdfs(c: &crate::cdf_state::Cdfs) {
    if !armed() {
        return;
    }
    macro_rules! ranges {
        ($($name:ident),* $(,)?) => {
            vec![$({
                let start = std::ptr::addr_of!(c.$name) as usize;
                (start, start + std::mem::size_of_val(&c.$name), stringify!($name))
            }),*]
        };
    }
    let mut tables: Vec<(usize, usize, &'static str)> = ranges![
        partition_w128, partition_w64, partition_w32, txfm_partition, skip, kf_y_mode, 
        uv_mode_no_cfl, uv_mode_cfl, cfl_sign, cfl_alpha, angle_delta, txb_skip_luma_16, 
        txb_skip_luma_8, txb_skip_luma_4, txb_skip_chroma_8, txb_skip_chroma_4, eob_pt_256_luma, eob_pt_64_luma, 
        eob_pt_64_chroma, eob_pt_16_chroma, eob_pt_16_luma, eob_pt_16_luma_class1, eob_pt_64_luma_class1, eob_pt_16_chroma_class1, 
        eob_pt_64_chroma_class1, eob_pt_128_chroma_class1, eob_pt_128_luma_class1, eob_pt_32_chroma_class1, eob_pt_256_luma_class1, eob_pt_256_chroma_class1, 
        eob_pt_512_luma_class1, eob_pt_512_chroma_class1, eob_pt_1024_luma_class1, eob_pt_1024_chroma_class1, eob_pt_32_luma, eob_pt_32_luma_class1, 
        eob_extra_luma_16, eob_extra_luma_8, eob_extra_luma_4, eob_extra_chroma_8, eob_extra_chroma_4, base_luma_16, 
        base_luma_8, base_luma_4, base_chroma_8, base_chroma_4, base_eob_luma_16, base_eob_luma_8, 
        base_eob_luma_4, base_eob_chroma_8, base_eob_chroma_4, br_luma_16, br_luma_8, br_luma_4, 
        br_chroma_8, br_chroma_4, partition_w16, partition_w8, txb_skip_luma_32, txb_skip_luma_64, 
        txb_skip_chroma_16, txb_skip_chroma_32, eob_pt_1024_luma, eob_pt_1024_chroma, eob_pt_256_chroma, eob_pt_512_luma, 
        eob_pt_128_chroma, eob_pt_512_chroma, eob_pt_128_luma, eob_pt_32_chroma, eob_extra_luma_32, eob_extra_luma_64, 
        eob_extra_chroma_16, eob_extra_chroma_32, base_luma_32, base_luma_64, base_chroma_16, base_chroma_32, 
        base_eob_luma_32, base_eob_luma_64, base_eob_chroma_16, base_eob_chroma_32, br_luma_32, br_chroma_16, 
        br_chroma_32, dc_sign_luma, intra_tx_type_16, intra_tx_type_8, intra_tx_type_8_set1, intra_tx_type_4, 
        intra_tx_type_4_set1, inter_tx_type_32, inter_tx_type_16, inter_tx_type_8, inter_tx_type_16_set2, inter_tx_type_8_set1, 
        inter_tx_type_4, inter_tx_type_4_set1, dc_sign_chroma, intra_inter, single_ref, comp_mode, 
        skip_mode, obmc, motion_mode, interintra, interintra_mode, wedge_interintra, 
        compound_type, wedge_idx, inter_compound_mode, comp_ref_type, uni_comp_ref, comp_ref, 
        comp_bwdref, comp_group_idx, compound_idx, y_mode, new_mv, zero_mv, 
        ref_mv, drl_mode, mv_joint, mv_comp, dv_joint, dv_comp, 
        filter_intra, filter_intra_mode, tx_size_cat0, tx_size_cat1, tx_size_cat2, tx_size_cat3, 
        switchable_interp, palette_y_mode, palette_y_size, palette_uv_mode, palette_uv_size, palette_y_color_index, 
        palette_uv_color_index, intrabc, restore_wiener, restore_sgrproj, restore_switchable, segment_id, 
        segment_pred, delta_q, delta_lf, delta_lf_multi
    ];
    tables.sort_unstable();
    with(|s| s.tables = tables);
}

/// Charges `bits` to whichever CDF table `addr` falls inside.
pub(crate) fn charge(addr: usize, bits: f64) {
    with(|s| {
        let name = match s.tables.partition_point(|t| t.0 <= addr) {
            0 => "unregistered",
            i if s.tables[i - 1].1 > addr => s.tables[i - 1].2,
            _ => "unregistered",
        };
        let e = cur(s).families.entry(name).or_default();
        e.0 += 1;
        e.1 += bits;
    });
}

/// Charges raw (equiprobable) bits, which no CDF names.
pub(crate) fn charge_literal(bits: u32) {
    with(|s| {
        let e = cur(s).families.entry("literal").or_default();
        e.0 += u64::from(bits);
        e.1 += f64::from(bits);
    });
}

/// One coded block, from the neighbour publisher every block routes through.
pub(crate) fn block(w_mi: usize, h_mi: usize, is_inter: bool, ref_frame: i8, skip: bool) {
    if !armed() {
        return;
    }
    let area = (w_mi * h_mi) as u64;
    with(|s| {
        s.last_block_area = area;
        let f = cur(s);
        *f.blocks.entry(((w_mi * 4) as u16, (h_mi * 4) as u16)).or_default() += 1;
        f.area[usize::from(is_inter)] += area;
        f.refs[(ref_frame.max(0) as usize).min(7)] += area;
        if skip {
            f.skip_area += area;
        }
    });
}

/// The block just published is compound (two references) -- called from the
/// compound-context publisher, which runs right after [`block`].
pub(crate) fn compound() {
    if !armed() {
        return;
    }
    with(|s| {
        let area = s.last_block_area;
        let f = cur(s);
        f.area[1] = f.area[1].saturating_sub(area);
        f.area[2] += area;
    });
}

/// One block's published luma transform shape.
pub(crate) fn tx(w_px: u8, h_px: u8) {
    if !armed() {
        return;
    }
    with(|s| *cur(s).tx.entry((u16::from(w_px), u16::from(h_px))).or_default() += 1);
}

/// One coded motion vector (a NEWMV read).
pub(crate) fn mv(x: i32, y: i32) {
    if !armed() {
        return;
    }
    let m = x.abs().max(y.abs());
    let bucket = match m {
        0 => 0,
        1..=8 => 1,
        9..=32 => 2,
        33..=128 => 3,
        129..=512 => 4,
        _ => 5,
    };
    with(|s| cur(s).mv[bucket] += 1);
}

/// One single-ref inter mode decision: 0 NEWMV, 1 GLOBALMV, 2 NEARESTMV,
/// 3 NEARMV.
pub(crate) fn mode(which: usize) {
    if !armed() {
        return;
    }
    with(|s| cur(s).modes[which] += 1);
}

/// One coded `tx_type` symbol, with the CDF row length that names its set.
pub(crate) fn tx_type(len: usize, symbol: usize) {
    if !armed() {
        return;
    }
    with(|s| *cur(s).tx_type.entry((len as u8, symbol as u8)).or_default() += 1);
}

/// One coded block of an INTRA frame, from the `blk_grid` publisher every
/// block writes -- a no-op on an inter frame, where [`block`] (the inter
/// neighbour publisher, which knows the reference) counts instead.
pub(crate) fn block_intra_frame(w_mi: usize, h_mi: usize, skip: bool) {
    if !armed() {
        return;
    }
    if with(|s| cur(s).kind == "key") {
        block(w_mi, h_mi, false, 0, skip);
    }
}
