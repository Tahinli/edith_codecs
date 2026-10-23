use super::*;

/// lane-split census (`EC_AV1_SPLIT_CENSUS=1`): the split decision itself,
/// bucketed by site (0 = intra `tx_depth`, 1 = inter var-tx), frame type
/// (0 = key, 1 = inter) and block side (8/16/32/64). Ten counters per bucket:
/// trials offered, trials the split won, then the (rate, distortion) of the
/// split and flat arms summed apart for the split-won and flat-won trials --
/// rate scaled by 64 into an integer counter, as [`TXRD_STATS`] is.
pub(super) static SPLIT_CENSUS: [std::sync::atomic::AtomicU64; 160] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 160];

/// Whether [`SPLIT_CENSUS`] is armed, read once.
pub(crate) fn split_census_on() -> bool {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| crate::envflags::env_flag!("EC_AV1_SPLIT_CENSUS"));
    *ON
}

/// Where one (site, frame type, block side) bucket starts in [`SPLIT_CENSUS`].
/// Sides below 8 and above 64 fold into the end rows, which no caller offers.
pub(super) fn split_census_base(site: usize, key: bool, side: usize) -> usize {
    let side_idx = (side.trailing_zeros() as usize).saturating_sub(3).min(3);
    ((site * 2 + usize::from(!key)) * 4 + side_idx) * 10
}

/// Records one split trial into [`SPLIT_CENSUS`]. `split` and `flat` are each
/// `(rate in bits, distortion as squared error)`.
pub(super) fn split_census(
    site: usize,
    key: bool,
    side: usize,
    split_won: bool,
    split: (f64, f64),
    flat: (f64, f64),
) {
    use std::sync::atomic::Ordering::Relaxed;
    let base = split_census_base(site, key, side);
    let add = |i: usize, v: u64| {
        SPLIT_CENSUS[base + i].fetch_add(v, Relaxed);
    };
    add(0, 1);
    let off = if split_won {
        add(1, 1);
        2
    } else {
        6
    };
    let (win, lose) = if split_won {
        (split, flat)
    } else {
        (flat, split)
    };
    add(off, (win.0 * 64.0).max(0.0) as u64);
    add(off + 1, win.1.max(0.0) as u64);
    add(off + 2, (lose.0 * 64.0).max(0.0) as u64);
    add(off + 3, lose.1.max(0.0) as u64);
}

/// Prints [`SPLIT_CENSUS`] to stderr when `EC_AV1_SPLIT_CENSUS=1`. Called at
/// the end of every `Av1Encoder::encode_frames` call and NOT reset, so the
/// last table a run prints is the whole encode's total (there is no flush
/// hook the encoder API guarantees to reach).
pub(crate) fn dump_split_census() {
    use std::sync::atomic::Ordering::Relaxed;
    eprintln!(
        "SPLITCENSUS site frame side offered split share% \
         win_rate win_dist lose_rate lose_dist"
    );
    for site in 0..2 {
        for key in [true, false] {
            for side_idx in 0..4 {
                let base = split_census_base(site, key, 8 << side_idx);
                let v: [u64; 10] = std::array::from_fn(|i| SPLIT_CENSUS[base + i].load(Relaxed));
                if v[0] == 0 {
                    continue;
                }
                let (n, s) = (v[0] as f64, v[1] as f64);
                // Winner terms live at 2,3 (split won) and 6,7 (flat won),
                // loser terms at 4,5 and 8,9 -- so each mean is one sum over
                // all `n` trials, not a mean of two means.
                eprintln!(
                    "SPLITCENSUS {} {} {:>3} {:>7} {:>7} {:>5.1} \
                     {:>9.2} {:>9.0} {:>9.2} {:>9.0}",
                    if site == 0 { "intra" } else { "inter" },
                    if key { "key  " } else { "inter" },
                    8 << side_idx,
                    v[0],
                    v[1],
                    100.0 * s / n,
                    (v[2] + v[6]) as f64 / n / 64.0,
                    (v[3] + v[7]) as f64 / n,
                    (v[4] + v[8]) as f64 / n / 64.0,
                    (v[5] + v[9]) as f64 / n,
                );
            }
        }
    }
}

/// lane-newmv: a multiplier on the mv RATE term of the BLOCK-level inter
/// decision only -- the `NEWMV` candidate's own mv residual + DRL bits, not
/// its mode/reference syntax and not the search's own cost function
/// ([`crate::motion`]'s `EC_AV1_MV_SEARCH_LAMBDA` is that one). `1.0` is the
/// control; below 1 a coded vector is charged less and `NEWMV` wins more
/// blocks. `EC_AV1_MV_LAMBDA`, class `rd-rate-term-calibration`.
pub(crate) fn mv_lambda_mult() -> f64 {
    static M: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_MV_LAMBDA")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(MV_LAMBDA)
    });
    *M
}

/// The shipped value of [`mv_lambda_mult`]. See lanes/newmv.report.md.
pub(super) const MV_LAMBDA: f64 = 1.0;

/// What a `NEWMV` candidate is charged ON TOP of its own priced mv syntax
/// bits, `(EC_AV1_MV_LAMBDA - 1) * mv_bits` (added to the candidate's bits,
/// which the RD cost multiplies by lambda). Zero at the default weight.
pub(super) fn mv_rate_surcharge(mv_bits: f64) -> f64 {
    (mv_lambda_mult() - 1.0) * mv_bits
}

/// lane-newmv census (`EC_AV1_NEWMV_CENSUS=1`): the block-level `NEWMV`
/// decision, bucketed by block side (8/16/32/64). Eight counters per bucket:
/// 0 blocks that ran a `LAST` motion search, 1 of those whose vector the
/// writer could name (offered to RD), 2 blocks `NEWMV` won, 3 the summed RD
/// cost it won by, 4 the summed cost it lost by, 5 the summed
/// `|mv - nearest_mv|` (1/8-pel units, both axes), 6 blocks whose searched
/// vector IS the stack's `NEARESTMV`, 7 the summed mv syntax bits (x64, as
/// [`SPLIT_CENSUS`]'s rate terms are). Four globals follow at 32: extra-
/// reference searches offered/skipped by [`EXTRA_NEW_SKIP_MARGIN`], leaf
/// second-reference searches offered/skipped by [`LEAF_SECOND_NEW_MARGIN`].
pub(super) static NEWMV_CENSUS: [std::sync::atomic::AtomicU64; 36] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 36];

/// Whether [`NEWMV_CENSUS`] is armed, read once.
pub(crate) fn newmv_census_on() -> bool {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| crate::envflags::env_flag!("EC_AV1_NEWMV_CENSUS"));
    *ON
}

/// Bumps one of [`NEWMV_CENSUS`]'s four global counters (32..36).
pub(super) fn newmv_census_global(i: usize) {
    if newmv_census_on() {
        NEWMV_CENSUS[32 + i].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Records one block's `NEWMV` decision into [`NEWMV_CENSUS`]. `other_cost`
/// is the best cost the block had WITHOUT the `NEWMV` candidate (its
/// `NEAREST`/`NEAR`/`GLOBAL`/intra winner), `new_cost` the `NEWMV`
/// candidate's own; both `f64::INFINITY` is a block that offered neither.
pub(super) fn newmv_census_block(
    side: usize,
    offered: bool,
    new_cost: f64,
    other_cost: f64,
    mv: (i32, i32),
    nearest: (i32, i32),
    mv_bits: f64,
) {
    if !newmv_census_on() {
        return;
    }
    use std::sync::atomic::Ordering::Relaxed;
    let base = (side.trailing_zeros() as usize).saturating_sub(3).min(3) * 8;
    let add = |i: usize, v: u64| {
        NEWMV_CENSUS[base + i].fetch_add(v, Relaxed);
    };
    add(0, 1);
    if !offered {
        return;
    }
    add(1, 1);
    if new_cost < other_cost {
        add(2, 1);
        add(3, (other_cost - new_cost).max(0.0) as u64);
    } else {
        add(4, (new_cost - other_cost).max(0.0) as u64);
    }
    add(
        5,
        ((mv.0 - nearest.0).unsigned_abs() + (mv.1 - nearest.1).unsigned_abs()) as u64,
    );
    if mv == nearest {
        add(6, 1);
    }
    add(7, (mv_bits * 64.0).max(0.0) as u64);
}

/// Prints [`NEWMV_CENSUS`] to stderr when `EC_AV1_NEWMV_CENSUS=1`, on the
/// same guard [`dump_split_census`] rides (cumulative, so the last table a
/// run prints is the whole encode's).
pub(crate) fn dump_newmv_census() {
    use std::sync::atomic::Ordering::Relaxed;
    eprintln!(
        "NEWMVCENSUS side searched offered won won% win_margin lose_margin \
         mv_dist_q3 mv==nearest% mv_bits"
    );
    for side_idx in 0..4 {
        let base = side_idx * 8;
        let v: [u64; 8] = std::array::from_fn(|i| NEWMV_CENSUS[base + i].load(Relaxed));
        if v[0] == 0 {
            continue;
        }
        let off = v[1].max(1) as f64;
        eprintln!(
            "NEWMVCENSUS {:>3} {:>8} {:>8} {:>7} {:>5.1} {:>10.0} {:>11.0} {:>10.2} {:>12.1} {:>7.2}",
            8 << side_idx,
            v[0],
            v[1],
            v[2],
            100.0 * v[2] as f64 / off,
            v[3] as f64 / v[2].max(1) as f64,
            v[4] as f64 / (v[1] - v[2]).max(1) as f64,
            v[5] as f64 / off,
            100.0 * v[6] as f64 / off,
            v[7] as f64 / off / 64.0,
        );
    }
    // lane-hpmv: how many mv components this encode coded an eighth-pel bit
    // for, and how many of those named a vector no non-hp frame can code.
    let (hp_all, hp_eighth) = crate::tile::hp_symbols();
    eprintln!(
        "NEWMVCENSUS hp symbols {hp_all}, of them eighth-pel {hp_eighth} ({:.1}%)",
        100.0 * hp_eighth as f64 / hp_all.max(1) as f64,
    );
    let g: [u64; 4] = std::array::from_fn(|i| NEWMV_CENSUS[32 + i].load(Relaxed));
    eprintln!(
        "NEWMVCENSUS pre-RD margin skips: extra-reference {}/{} ({:.1}%), \
         leaf second reference {}/{} ({:.1}%)",
        g[1],
        g[0],
        100.0 * g[1] as f64 / g[0].max(1) as f64,
        g[3],
        g[2],
        100.0 * g[3] as f64 / g[2].max(1) as f64,
    );
    let [
        calls,
        evals,
        _subpel,
        rounds,
        near_pred,
        unmoved,
        widest,
        widest_moves,
    ] = crate::motion::take_census();
    let per = |n: u64| n as f64 / calls.max(1) as f64;
    eprintln!(
        "NEWMVCENSUS search: {calls} calls, {:.1} evals/call, {:.2} integer rounds/call; \
         winner within +-1 pel of pred_mv {:.1}%, winner = starting seed {:.1}%, \
         moved at the WIDEST step {:.1}% of calls ({:.2} such moves/call)",
        per(evals),
        per(rounds),
        100.0 * near_pred as f64 / calls.max(1) as f64,
        100.0 * unmoved as f64 / calls.max(1) as f64,
        100.0 * widest as f64 / calls.max(1) as f64,
        per(widest_moves),
    );
}

/// [`TX_TYPE_HITS`] for the INTER luma units (lane-txset2), counted apart so
/// the intra witness's fire count stays its own.
pub(crate) static INTER_TX_TYPE_HITS: [std::sync::atomic::AtomicUsize; 16] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 16];

/// lane-txrd: a multiplier on the rate term of the TRANSFORM-TYPE decision
/// only (`EC_AV1_TXRD_LAMBDA`, default 1.0 = the mode search's own weight).
/// `0` compares types on reconstruction error alone, which is what separates
/// a rate-term calibration (class `rd-rate-term-calibration`) from a
/// distortion the trial mis-measures.
pub(crate) fn tx_type_lambda_mult() -> f64 {
    static M: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_TXRD_LAMBDA")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0)
    });
    *M
}

/// lane-txi: what a non-`DCT_DCT` transform type is charged ON TOP of its own
/// priced bits -- `(EC_AV1_TXRD_LAMBDA - 1) * lambda * bits`, zero at the
/// default weight of 1 and for `DCT_DCT` itself. The instrument the film
/// question needs: at weight 1 the five-type intra search spends 1.2% more
/// bytes for 0.04 dB on both film rows, so what it under-charges is the rate
/// of the type it picks.
pub(crate) fn non_dct_surcharge(lambda: f64, tx_type: TxType, bits: f64) -> f64 {
    if tx_type == TxType::DctDct {
        0.0
    } else {
        lambda * (tx_type_lambda_mult() - 1.0) * bits
    }
}

/// lane-txrd instrument: over the FLAT inter luma decision, the summed
/// reconstruction error and priced bits of the type the search chose against
/// the `DCT_DCT` trial it was compared with. Both are `* 64` so they fit an
/// integer counter. `EC_AV1_TXRD_CENSUS`.
pub(crate) static TXRD_STATS: [std::sync::atomic::AtomicU64; 7] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Reads [`TXRD_STATS`] and zeroes it. Only the gate reads it, so the lib
/// build would warn `dead_code` (class from lane-txset2's `coeff_bits`).
#[cfg(test)]
pub(crate) fn take_txrd_stats() -> [u64; 7] {
    std::array::from_fn(|i| TXRD_STATS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Reads [`INTER_TX_TYPE_HITS`] and zeroes it.
#[cfg(test)]
pub(crate) fn take_inter_tx_type_hits() -> [usize; 16] {
    std::array::from_fn(|t| INTER_TX_TYPE_HITS[t].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// How many luma transform units resolved to each [`TxType`], indexed by the
/// enum's own discriminant (lane-txset): the fire count a witness asserts on,
/// and the instrument that says which types the search actually reaches.
pub(crate) static TX_TYPE_HITS: [std::sync::atomic::AtomicUsize; 16] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 16];

/// Reads [`TX_TYPE_HITS`] and zeroes it, so a gate or a test attributes the
/// counts to its own encode.
#[cfg(test)]
pub(crate) fn take_tx_type_hits() -> [usize; 16] {
    std::array::from_fn(|t| TX_TYPE_HITS[t].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// How many blocks resolved to each `tx_depth`, the fire count that says
/// whether the depth search below reaches its candidates at all.
pub(super) static TX_DEPTH_HITS: [std::sync::atomic::AtomicUsize; 3] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 3];

/// Reads [`TX_DEPTH_HITS`] and zeroes it, so a gate can attribute the counts
/// to its own encode.
#[cfg(test)]
pub(crate) fn take_tx_depth_hits() -> [usize; 3] {
    std::array::from_fn(|d| TX_DEPTH_HITS[d].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// How many inter blocks resolved to each var-tx depth (0 = one transform
/// over the whole block, 1 = its four halves), the fire count a gate prints
/// to see the split search reach its candidates at all. Indexed
/// `(block is 32x32) * 4 + (block is compound) * 2 + depth`, so one census
/// says which of the four block classes the split reaches -- the 32x32 root
/// and the compound blocks are this lane's two new ones.
pub(super) static INTER_TX_SPLIT_HITS: [std::sync::atomic::AtomicUsize; 8] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 8];

/// Whether a 32x32 inter winner searches its own var-tx depth (the four
/// 16x16 units against the flat transform) instead of coding depth 0 blind.
/// ON by default; `EC_AV1_TX32_DEPTH=0` turns it off.
///
/// Rejected three times on the bars rows of the native gate (flat to +0.3).
/// lane-av1resweep re-judged it on the two REAL film crops at
/// [`LAMBDA_SCALE`] 0.05, where it is the largest single lever in this file
/// (BD vs libaom / vs rav1e, shipped -> `EC_AV1_TX32_DEPTH=1`):
///
/// | clip | shipped | tx32 depth | + [`compound_var_tx`] |
/// |---|---|---|---|
/// | bars 1080p | +15.6 / -1.7 | +15.7 / -1.5 | +15.6 / -1.6 |
/// | bars 2160p | +46.5 / +18.8 | +46.5 / +18.8 | +46.6 / +18.8 |
/// | film A | +69.0 / +37.5 | +67.9 / +36.3 | +65.5 / +34.7 |
/// | film B | +100.0 / +61.5 | +96.6 / +58.4 | +94.5 / +56.9 |
/// | screen | +51.0 / -14.5 | +50.7 / -14.4 | +50.2 / -14.6 |
///
/// The bars rows are ~90% zero-intra-cost cells, so the split never had
/// residual to reach there -- the tool was being judged on content that
/// cannot pay for it (class `gate-blind-to-feature`). `EC_AV1_TX32_DEPTH`.
pub(super) fn tx32_depth_search() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_TX32_DEPTH")
            .ok()
            .map(|v| !matches!(v.as_str(), "0" | "off"))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::TX32_DEPTH))
}

/// Whether a COMPOUND inter winner is offered the var-tx split at all: its
/// trial units have to be predicted from BOTH references
/// ([`mc_trial_compound`]) or the split would re-predict them by single-
/// reference translation, which is not what the decoder reconstructs.
/// ON by default (the table on [`tx32_depth_search`]: on real film it is
/// worth a further -2.4/-1.6 on film A and -2.1/-1.5 on film B on top of the
/// 32x32 depth search); `EC_AV1_COMP_VARTX=0` turns it off.
/// `EC_AV1_COMP_VARTX`.
pub(super) fn compound_var_tx() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_COMP_VARTX")
            .ok()
            .map(|v| !matches!(v.as_str(), "0" | "off"))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::COMPOUND_VAR_TX))
}

/// How many 32x32-and-below inter blocks each reference frame won, indexed by
/// `ref_frame` (`crate::mvstack::LAST_FRAME` = 1 .. `ALTREF_FRAME` = 7), the
/// fire count a gate prints to see an extra reference reach a block at all
/// ([[gate-blind-to-feature]]: a pyramid whose backward `ALTREF` is never
/// chosen is a pyramid that costs bytes and buys nothing).
pub(super) static REF_FRAME_HITS: [std::sync::atomic::AtomicUsize; 8] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 8];

/// Reads [`REF_FRAME_HITS`] and zeroes it.
#[cfg(test)]
pub(crate) fn take_ref_frame_hits() -> [usize; 8] {
    std::array::from_fn(|r| REF_FRAME_HITS[r].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Reads [`INTER_TX_SPLIT_HITS`] and zeroes it.
#[cfg(test)]
pub(crate) fn take_inter_tx_split_hits() -> [usize; 8] {
    std::array::from_fn(|d| INTER_TX_SPLIT_HITS[d].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// What the `txfm_split` symbols of one inter block cost at `depth`, priced
/// off the neighbour-free context row the way [`tx_depth_bits`] is: depth 0
/// is one flag, depth 1 is a split root plus one unsplit flag per child
/// (decode.rs `read_var_tx_size` stops symbol-free only at `MAX_VARTX_DEPTH`
/// or a 4x4 sub-size).
pub(super) fn txfm_split_bits(side: usize, depth: usize) -> f64 {
    let max_tx = side.min(64);
    let ctx = |tx: usize| {
        crate::decode::txfm_partition_ctx_rect(
            crate::decode::TXFM_CTX_INIT,
            crate::decode::TXFM_CTX_INIT,
            side,
            tx,
            tx,
        )
    };
    if depth == 0 {
        return symbol_bits(&cdf::TXFM_PARTITION[ctx(max_tx)], 0);
    }
    symbol_bits(&cdf::TXFM_PARTITION[ctx(max_tx)], 1)
        + 4.0 * symbol_bits(&cdf::TXFM_PARTITION[ctx(max_tx / 2)], 0)
}

/// spec 5.11.40 / libaom `av1_get_tx_type`: an INTER block's chroma transform
/// type is never coded -- the decoder takes the LUMA type of the block's
/// chroma-reference position and reduces it to the set the chroma size allows
/// ([`crate::decode::reduce_inherited_chroma_tx_type`]). This encoder
/// quantised chroma under a `DCT_DCT` basis whatever luma chose, so every
/// non-`DCT_DCT` luma winner had its chroma levels inverse-transformed on a
/// basis they were never produced for: on the bars gate row the luma of the
/// unconditional tx-type arm came out BETTER on every frame (53.66 -> 53.70
/// dB) while V lost seven decibels (52.52 -> 45.53) -- the whole 20-point BD
/// collapse two lanes attributed to the type PRICE (class
/// `parsed then discarded` / `override slot on one arm`: a decoder-derived
/// field the encoder never mirrored).
///
/// Re-codes the two chroma planes of a committed inter block at the type the
/// decoder will use, `None` when the inherited type reduces to `DCT_DCT` and
/// the trials the caller already holds are right.
#[allow(clippy::too_many_arguments)]
pub(super) fn recode_inter_chroma(
    chroma: &[Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    mv: (i32, i32),
    refs: [(&[u16], usize, usize, usize); 2],
    second: Option<((i32, i32), [(&[u16], usize, usize, usize); 2])>,
    search: &Search,
    set: TxbSet,
    luma_tx_type: TxType,
    fctx: &crate::decode::FrameCtx,
) -> Option<[Trial; 2]> {
    let c = side / 2;
    let tx =
        crate::decode::reduce_inherited_chroma_tx_type(luma_tx_type, c.min(32), c.min(32), fctx);
    if tx == TxType::DctDct {
        return None;
    }
    Some([0usize, 1].map(|p| match second {
        Some((mv1, r1)) => mc_trial_compound_typed(
            &chroma[p],
            x / 2,
            y / 2,
            c,
            (mv, mv1),
            false,
            refs[p],
            r1[p],
            search.base_q_idx,
            search.deadzone,
            set,
            tx,
            fctx,
        ),
        None => mc_trial_typed(
            &chroma[p],
            x / 2,
            y / 2,
            c,
            mv,
            false,
            refs[p].0,
            refs[p].1,
            refs[p].2,
            refs[p].3,
            false,
            search.base_q_idx,
            search.deadzone,
            set,
            tx,
            fctx,
        ),
    }))
}

/// The inter block's own var-tx decision, once its motion vector is settled:
/// its residual as ONE transform over the whole block, or as the four
/// transforms of half the side the tree's first halving names. The prediction
/// is the same motion compensation either way, so the split trial
/// re-transforms the identical residual in four pieces -- it wins where four
/// small transforms carry a locally varying residual cheaper than one big
/// one. Commits the winner into `luma` and hands back its block-coordinate
/// levels, the depth the writer codes and the cost the winner adds to (or
/// takes off) the caller's already-computed flat cost.
///
/// Only for a block wholly inside the frame: past the true edge the reader's
/// var-tx recursion skips the units that fall outside
/// (decode.rs `read_var_tx_size`'s `max_w_mi`/`max_h_mi` guard), which would
/// leave the residual units this writes unpaired.
#[allow(clippy::too_many_arguments)]
pub(super) fn commit_inter_luma(
    luma: &mut Plane,
    (x, y): (usize, usize),
    side: usize,
    mv: (i32, i32),
    reference: (&[u16], usize, usize, usize),
    search: &Search,
    flat: &Trial,
    skip: bool,
    // A COMPOUND winner's second half: the vector and reference planes the
    // trial units are blended from ([`mc_trial_compound`], the decoder's own
    // `comp_group_idx == 0` average). `None` is the single-reference block.
    second: Option<((i32, i32), (&[u16], usize, usize, usize))>,
    fctx: &crate::decode::FrameCtx,
) -> (Vec<i32>, u8, f64, Vec<TxType>) {
    // lane-txset2 step 1: the whole-block (depth 0) residual picks its own
    // type out of the two-type inter set the writer already codes a symbol
    // for, priced the same way the intra search is (sse + lambda * bits, the
    // bits carrying the `tx_type` symbol itself through RDOQ). A skipped
    // block codes no residual and the 64-point transform names no type.
    let mut flat_typed: Option<Trial> = None;
    let mut flat_type = TxType::DctDct;
    if !skip {
        // lane-intertx: on a `reduced_tx_set = 0` frame the search names the
        // Set1 alphabet the writer codes into ([`luma_set_for`], the shape
        // the intra search already takes), which is what makes the wider
        // seven-type candidate list below reachable -- and prices every
        // candidate, `IDTX` included, in the alphabet its symbol is really
        // coded into.
        if let Some(fset) = inter_luma_set(side).map(|set| luma_set_for(set, search.screen)) {
            let lambda = search.lambda * tx_type_lambda_mult();
            let cost = |t: &Trial| t.sse + lambda * t.bits;
            for &tx_type in inter_tx_type_candidates(fset, search.screen) {
                if tx_type == TxType::DctDct {
                    continue;
                }
                let candidate = match second {
                    Some((mv1, ref1)) => mc_trial_compound_typed(
                        luma,
                        x,
                        y,
                        side,
                        (mv, mv1),
                        true,
                        reference,
                        ref1,
                        search.base_q_idx,
                        search.deadzone,
                        fset,
                        tx_type,
                        fctx,
                    ),
                    None => mc_trial_typed(
                        luma,
                        x,
                        y,
                        side,
                        mv,
                        true,
                        reference.0,
                        reference.1,
                        reference.2,
                        reference.3,
                        false,
                        search.base_q_idx,
                        search.deadzone,
                        fset,
                        tx_type,
                        fctx,
                    ),
                };
                if cost(&candidate) < cost(flat_typed.as_ref().unwrap_or(flat)) {
                    flat_type = tx_type;
                    flat_typed = Some(candidate);
                }
            }
        }
    }
    // Counted at the return points, so the census names the types the block
    // really CODES -- a split unit's winner that the flat cost then beats was
    // never written (class `counter from refused stream`).
    let count = |types: &[TxType]| {
        for &t in types {
            INTER_TX_TYPE_HITS[t as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    };
    let flat_ref: &Trial = flat_typed.as_ref().unwrap_or(flat);
    if crate::envflags::env_flag!("EC_AV1_TXRD_CENSUS") && !skip {
        use std::sync::atomic::Ordering::Relaxed;
        TXRD_STATS[0].fetch_add(1, Relaxed);
        TXRD_STATS[1].fetch_add(flat_ref.sse as u64, Relaxed);
        TXRD_STATS[2].fetch_add(flat.sse as u64, Relaxed);
        TXRD_STATS[3].fetch_add((flat_ref.bits * 64.0) as u64, Relaxed);
        TXRD_STATS[4].fetch_add((flat.bits * 64.0) as u64, Relaxed);
        // Is the `DCT_DCT` baseline the search compares against the trial
        // THIS function would build for the same block? A stale or
        // differently-predicted baseline is what would let a candidate win on
        // both terms at once (class `local-rd-on-references` vs a real
        // pricing defect); re-predicting it here is the only way to tell.
        if let Some(fset) = inter_luma_set(side) {
            let here = match second {
                Some((mv1, ref1)) => mc_trial_compound_typed(
                    luma,
                    x,
                    y,
                    side,
                    (mv, mv1),
                    true,
                    reference,
                    ref1,
                    search.base_q_idx,
                    search.deadzone,
                    fset,
                    TxType::DctDct,
                    fctx,
                ),
                None => mc_trial_typed(
                    luma,
                    x,
                    y,
                    side,
                    mv,
                    true,
                    reference.0,
                    reference.1,
                    reference.2,
                    reference.3,
                    false,
                    search.base_q_idx,
                    search.deadzone,
                    fset,
                    TxType::DctDct,
                    fctx,
                ),
            };
            TXRD_STATS[5].fetch_add(here.sse as u64, Relaxed);
            TXRD_STATS[6].fetch_add((here.bits * 64.0) as u64, Relaxed);
            static DUMPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = TXRD_STATS[0].load(Relaxed);
            if here.sse * 2.0 < flat.sse && DUMPED.fetch_add(1, Relaxed) < 30 {
                eprintln!(
                    "TXRD block {n} at ({x},{y}) side={side} compound={} mv={mv:?} \
                     flat sse={:.0} bits={:.1} | here sse={:.0} bits={:.1} | chosen {flat_type:?} \
                     sse={:.0} bits={:.1}",
                    second.is_some(),
                    flat.sse,
                    flat.bits,
                    here.sse,
                    here.bits,
                    flat_ref.sse,
                    flat_ref.bits,
                );
            }
        }
    }
    // What the caller's already-counted `flat` cost has to be corrected by
    // when a non-`DCT_DCT` type won it, the same way the split delta below is.
    let flat_gain =
        (flat_ref.sse + search.lambda * flat_ref.bits) - (flat.sse + search.lambda * flat.bits);
    let eligible = tx_select()
        && tx_select_inter()
        && !skip
        && side >= 16
        && (second.is_none() || compound_var_tx())
        && (side != BLOCK || tx32_depth_search())
        && x + side <= luma.true_width
        && y + side <= luma.true_height;
    if !eligible {
        luma.commit(x, y, side, flat_ref);
        count(&[flat_type]);
        return (flat_ref.levels.clone(), 0, flat_gain, vec![flat_type]);
    }
    let tx = side / 2;
    let set = luma_set_for(
        if tx >= 32 {
            // lane-b64b: the 64x64 root's halved transform is TX_32X32.
            TxbSet::Luma32Inter
        } else if tx >= 16 {
            TxbSet::Luma16Inter
        } else {
            TxbSet::Luma8Inter
        },
        search.screen,
    );
    let mut levels = vec![0i32; side * side];
    let (mut sse, mut bits) = (0.0, 0.0);
    let mut units = Vec::with_capacity(4);
    // The split units are smaller than their block, so their `txb_skip`
    // context is the neighbour magnitude table ([`Plane::coef_ctx`]) -- and
    // each unit's own commit publishes what the next one reads, coding order.
    luma.ctx.tu_split = true;
    // One type per unit in the writer's own raster order (tile.rs
    // `write_luma_tus`), all `DCT_DCT` when nothing else is offered.
    let mut split_types = vec![TxType::DctDct; 4];
    for tu_row in 0..2 {
        for tu_col in 0..2 {
            let (tu_x, tu_y) = (x + tu_col * tx, y + tu_row * tx);
            let unit = |tx_type: TxType| match second {
                Some((mv1, ref1)) => mc_trial_compound_typed(
                    luma,
                    tu_x,
                    tu_y,
                    tx,
                    (mv, mv1),
                    true,
                    reference,
                    ref1,
                    search.base_q_idx,
                    search.deadzone,
                    set,
                    tx_type,
                    fctx,
                ),
                None => mc_trial_typed(
                    luma,
                    tu_x,
                    tu_y,
                    tx,
                    mv,
                    true,
                    reference.0,
                    reference.1,
                    reference.2,
                    reference.3,
                    false,
                    search.base_q_idx,
                    search.deadzone,
                    set,
                    tx_type,
                    fctx,
                ),
            };
            let mut best: Option<(f64, TxType, Trial)> = None;
            for &tx_type in inter_tx_type_candidates(set, search.screen) {
                let candidate = unit(tx_type);
                let cost = candidate.sse + search.lambda * tx_type_lambda_mult() * candidate.bits;
                if best.as_ref().is_none_or(|(b, _, _)| cost < *b) {
                    best = Some((cost, tx_type, candidate));
                }
            }
            let (_, tx_type, trial) = best.expect("the candidate list is never empty");
            split_types[tu_row * 2 + tu_col] = tx_type;
            sse += trial.sse;
            bits += trial.bits;
            for row in 0..tx {
                levels[(tu_row * tx + row) * side + tu_col * tx..][..tx]
                    .copy_from_slice(&trial.levels[row * tx..][..tx]);
            }
            units.push((tu_x, tu_y, trial));
        }
    }
    luma.ctx.tu_split = false;
    let split_rate = bits + txfm_split_bits(side, 1);
    let flat_rate = flat_ref.bits + txfm_split_bits(side, 0);
    let split_cost = sse + search.lambda * split_lambda_mult() * split_rate;
    let flat_cost = flat_ref.sse + search.lambda * flat_rate;
    if split_census_on() {
        split_census(
            1,
            false,
            side,
            split_cost < flat_cost,
            (split_rate, sse),
            (flat_rate, flat_ref.sse),
        );
    }
    let class = usize::from(side == BLOCK) * 4 + usize::from(second.is_some()) * 2;
    // The depth census has a leaf row and a 32x32 row and no 64 one, so the
    // 64x64 root counts itself ([`take_b64_var_tx_hits`]) instead of landing
    // in the leaf row (class `gate-blind-to-feature`).
    let counted = side <= BLOCK;
    if split_cost < flat_cost {
        for (tu_x, tu_y, trial) in &units {
            luma.commit(*tu_x, *tu_y, tx, trial);
        }
        if counted {
            INTER_TX_SPLIT_HITS[class + 1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        count(&split_types);
        return (levels, 1, flat_gain + split_cost - flat_cost, split_types);
    }
    if counted {
        INTER_TX_SPLIT_HITS[class].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    luma.commit(x, y, side, flat_ref);
    count(&[flat_type]);
    (flat_ref.levels.clone(), 0, flat_gain, vec![flat_type])
}

/// The largest distinct-colour count a block may hold and still be offered a
/// palette candidate at all: past it the k-means quantisation costs more
/// error than the map saves rate, and every trial is wasted wall.
///
/// Swept on the gate's own screen capture (lane-av1pal2, the
/// `unswept-decision-constants` class): 32 (the value this lane inherited)
/// finds 515 palette blocks and scores +66.4%/+10.3% vs libaom/rav1e, 64
/// finds 1264 and scores +57.1%/+3.2%, 128 finds 1102 and scores
/// +56.9%/+2.3%, and 256 is byte-identical to 128 -- above 128 the bound
/// binds on nothing this content holds. 128 ships: best BD against both
/// references and no wall cost at all (6.1s, the same as at 32, because the
/// blocks it newly admits are ones the RD then keeps).
/// `EC_AV1_PAL_MAXCOLORS`.
pub(super) fn palette_max_colors() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("EC_AV1_PAL_MAXCOLORS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(128)
    })
}

/// The palettes worth trying for one block's luma, in the shape libaom's
/// `av1_k_means` picks them: a block holding at most `PALETTE_MAX_SIZE`
/// distinct colours takes them ALL (the prediction is then exact and the
/// residual empty), and a busier one is quantised to 8 and to 4 colours by a
/// one-dimensional Lloyd iteration seeded evenly across its range -- both
/// offered, because which wins is an RD question the caller answers.
/// `block` is `side*side` source samples, row-major.
pub(super) fn palette_candidates(block: &[u8]) -> Vec<crate::tile::PaletteY> {
    let mut histogram = [0u32; 256];
    for &v in block {
        histogram[usize::from(v)] += 1;
    }
    let present: Vec<u16> = (0..256u16)
        .filter(|&v| histogram[usize::from(v)] > 0)
        .collect();
    if present.len() < 2 {
        return Vec::new();
    }
    let exact = |colors: &[u16]| -> crate::tile::PaletteY {
        let mut base = [0u16; 8];
        base[..colors.len()].copy_from_slice(colors);
        let map = block
            .iter()
            .map(|&v| {
                colors
                    .iter()
                    .enumerate()
                    .min_by_key(|&(_, &c)| (i32::from(c) - i32::from(v)).abs())
                    .map(|(i, _)| i as u8)
                    .expect("a palette holds at least two colours")
            })
            .collect();
        crate::tile::PaletteY {
            size: colors.len() as u8,
            colors: base,
            map,
        }
    };
    if present.len() <= 8 {
        return vec![exact(&present)];
    }
    if present.len() > palette_max_colors() {
        return Vec::new();
    }
    let (lo, hi) = (i32::from(present[0]), i32::from(present[present.len() - 1]));
    let mut out = Vec::with_capacity(2);
    for n in [8usize, 4] {
        // Even seeding across the block's own range, then four Lloyd passes
        // over the histogram (256 bins, not `side*side` pixels).
        let mut centres: Vec<f64> = (0..n)
            .map(|i| f64::from(lo) + f64::from(hi - lo) * (i as f64 + 0.5) / n as f64)
            .collect();
        for _ in 0..4 {
            let mut sums = vec![0f64; n];
            let mut counts = vec![0u32; n];
            for (v, &count) in histogram.iter().enumerate() {
                if count == 0 {
                    continue;
                }
                let k = (0..n)
                    .min_by(|&a, &b| {
                        (centres[a] - v as f64)
                            .abs()
                            .total_cmp(&(centres[b] - v as f64).abs())
                    })
                    .expect("n >= 2");
                sums[k] += (v as u32 * count) as f64;
                counts[k] += count;
            }
            for k in 0..n {
                if counts[k] > 0 {
                    centres[k] = sums[k] / f64::from(counts[k]);
                }
            }
        }
        let mut colors: Vec<u16> = centres
            .iter()
            .map(|&c| c.round().clamp(0.0, 255.0) as u16)
            .collect();
        colors.sort_unstable();
        colors.dedup();
        if colors.len() >= 2 {
            out.push(exact(&colors));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// lane-av1ibc: intra block copy (spec 5.11.13 `use_intrabc`, 7.11.4 DV
// validity). A key frame that detected screen content and finds enough exact
// repeats in its own source sets `allow_intrabc`, which the spec forces every
// in-loop filter OFF for (frame.rs:194/210/252/272) -- so the bit only earns
// its place when the copied blocks pay back the deblock/CDEF/LR the frame
// gives up. `EC_AV1_INTRABC=0` turns the whole arm off.
// ---------------------------------------------------------------------------

/// Whether intra block copy is offered at all. **Off by default** --
/// `EC_AV1_INTRABC=1` turns it on. The syntax is complete and proven against
/// ffmpeg (`a_repeated_pattern_key_frame_codes_intrabc_blocks_ffmpeg_decodes_exactly`),
/// but on the gate's real screen capture it MEASURED WORSE: setting
/// `allow_intrabc` costs the key frame its deblocking, CDEF and loop
/// restoration (spec 5.9.11/5.9.19/5.9.20) and the copied blocks do not pay
/// that back -- screen BD +59.1/+5.1 against the +54.5/+1.1 base (lane-av1ibc,
/// `probe_intrabc_key_frame` has the per-key-frame table). Also off on a frame
/// that detected no screen content.
pub(super) fn intrabc_enabled() -> bool {
    if let Some(forced) = INTRABC_FORCE.with(std::cell::Cell::get) {
        return forced;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("EC_AV1_INTRABC").ok().as_deref() == Some("1"))
}

thread_local! {
    /// Overrides [`intrabc_enabled`] for the calling thread -- the ablation
    /// probe runs both arms in one process, and the suite is one process, so
    /// an environment variable cannot separate them (same reason
    /// [`SCREEN_FORCE`] exists).
    static INTRABC_FORCE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn force_intrabc(value: Option<bool>) {
    INTRABC_FORCE.with(|c| c.set(value));
}

/// How many of a screen frame's 16x16 blocks must have an exact repeat
/// somewhere else in the source before `allow_intrabc` is worth the in-loop
/// filters it costs, in percent (`EC_AV1_INTRABC_PCT`). Unswept-decision-
/// constant class: `probe_intrabc_threshold` measures the arm at several
/// values.
pub(super) fn intrabc_pct() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("EC_AV1_INTRABC_PCT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10)
    })
}

/// The pixel grid intrabc source positions are hashed on
/// (`EC_AV1_INTRABC_STEP`): every DV this encoder writes is a multiple of it,
/// which keeps the block-hash table (and so the search) linear in the frame
/// at a `step`-th of the density. It must stay EVEN -- a chroma plane copies
/// at `dv / 2` full pel, so an odd DV would land half-pel and need the
/// bilinear interpolation decode.rs applies but this encoder does not.
pub(super) fn intrabc_step() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let n: usize = std::env::var("EC_AV1_INTRABC_STEP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        if n.is_multiple_of(2) { n.max(2) } else { 4 }
    })
}

/// The side of the luma square the block-hash table is keyed on.
pub(super) const IBC_HASH: usize = 16;

/// libaom `INTRABC_DELAY_PIXELS / 64` -- how many 64x64 superblocks behind
/// the current one the source's bottom-right corner must already be. Under
/// 128 superblocks the same 256 pixels are two superblocks, which
/// [`Ibc::dv_valid`] derives from the size itself.
pub(super) const INTRABC_DELAY_SB64: i32 = 4;

/// FNV-1a over one `IBC_HASH`-square of `plane` at `(x, y)`.
pub(super) fn ibc_hash(plane: &[u8], width: usize, x: usize, y: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for row in 0..IBC_HASH {
        let base = (y + row) * width + x;
        for &v in &plane[base..base + IBC_HASH] {
            h ^= u64::from(v);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// Every `intrabc_step()`-aligned position of `plane` inside
/// `(x0, y0)..(x1, y1)`, keyed by [`ibc_hash`].
pub(super) fn ibc_index(
    table: &mut std::collections::HashMap<u64, Vec<(u32, u32)>>,
    plane: &[u8],
    width: usize,
    (x0, y0): (usize, usize),
    (x1, y1): (usize, usize),
) {
    let step = intrabc_step();
    if x1 < x0 + IBC_HASH || y1 < y0 + IBC_HASH {
        return;
    }
    let mut y = y0.next_multiple_of(step);
    while y + IBC_HASH <= y1 {
        let mut x = x0.next_multiple_of(step);
        while x + IBC_HASH <= x1 {
            table
                .entry(ibc_hash(plane, width, x, y))
                .or_default()
                .push((x as u32, y as u32));
            x += step;
        }
        y += step;
    }
}

/// Whether `screen`-detected source `y` repeats itself enough to be worth
/// `allow_intrabc` -- the share of its 16-aligned 16x16 blocks whose exact
/// content appears at some other position, against [`intrabc_pct`]. Returns
/// the share as well, for the gate's own print.
pub(super) fn intrabc_worth_it(y: &[u8], width: usize, true_width: usize, true_height: usize) -> (bool, f64) {
    if true_width < 4 * 64 || true_height < 2 * 64 {
        return (false, 0.0);
    }
    let mut table = std::collections::HashMap::new();
    ibc_index(&mut table, y, width, (0, 0), (true_width, true_height));
    let (mut total, mut matched) = (0usize, 0usize);
    let mut by = 0;
    while by + IBC_HASH <= true_height {
        let mut bx = 0;
        while bx + IBC_HASH <= true_width {
            total += 1;
            if let Some(hits) = table.get(&ibc_hash(y, width, bx, by))
                && hits
                    .iter()
                    .any(|&(hx, hy)| (hx as usize, hy as usize) != (bx, by))
                {
                    matched += 1;
                }
            bx += IBC_HASH;
        }
        by += IBC_HASH;
    }
    let share = if total == 0 {
        0.0
    } else {
        matched as f64 / total as f64
    };
    (share * 100.0 >= intrabc_pct() as f64, share)
}

/// One tile's intrabc search state: a block-hash table over the part of the
/// tile's own reconstruction that is already final (whole superblock rows --
/// the in-loop filters are off on this frame, so a coded superblock row never
/// changes again), plus the last DV chosen, which the RD price uses as its
/// predictor estimate.
pub(super) struct Ibc {
    /// Keyed on the SOURCE, exactly as libaom's `av1_intrabc_hash` is: the
    /// source repeats exactly where a lossily coded reconstruction only
    /// nearly does, so an exact-match table over the reconstruction finds
    /// almost nothing. The candidate is then SCORED against the
    /// reconstruction ([`ibc_sse`]), which is what a decoder will copy.
    pub(super) table: std::collections::HashMap<u64, Vec<(u32, u32)>>,
    /// The tile's own luma rect, in samples.
    pub(super) tile: (usize, usize, usize, usize),
    /// The DV the previous intrabc block of this tile took -- the *estimate*
    /// of decode.rs `read_intrabc_dv`'s predictor the RD price is taken
    /// against. The writer computes the true predictor off its own mi grid
    /// (`crate::tile::intrabc_dv_pred`); a wrong estimate here can only cost
    /// a slightly mispriced candidate, never a wrong bitstream.
    pub(super) last_dv: Option<(i32, i32)>,
    /// How many blocks took intrabc, and how many hash lookups found at least
    /// one valid candidate -- the gate's own fire counts.
    pub(super) hits: usize,
    pub(super) lookups: usize,
    pub(super) found: usize,
}

impl Ibc {
    pub(super) fn new(tile: (usize, usize, usize, usize), source: &[u8], width: usize) -> Self {
        let mut table = std::collections::HashMap::new();
        ibc_index(
            &mut table,
            source,
            width,
            (tile.0, tile.1),
            (tile.2, tile.3),
        );
        Self {
            table,
            tile,
            last_dv: None,
            hits: 0,
            lookups: 0,
            found: 0,
        }
    }

    /// libaom `av1_is_dv_valid` for a `w`x`h` block at `(x, y)` under `dv`
    /// (1/8 pel), with this lane's own extra restriction on top: the source
    /// must lie entirely in a COMPLETED superblock row, which is exactly the
    /// region [`Self::fill_to`] indexes and a strict subset of what the spec
    /// allows. corner-cut, ceiling named: same-superblock-row sources (the
    /// wavefront's own diagonal) are legal and not searched; the upgrade path
    /// is to index the current row's finished superblocks too.
    pub(super) fn dv_valid(&self, x: usize, y: usize, w: usize, h: usize, dv: (i32, i32)) -> bool {
        let (dr, dc) = (dv.0 / 8, dv.1 / 8);
        if dv.0 % 8 != 0 || dv.1 % 8 != 0 || dr % 2 != 0 || dc % 2 != 0 {
            return false;
        }
        let (tx0, ty0, tx1, ty1) = (
            self.tile.0 as i32,
            self.tile.1 as i32,
            self.tile.2 as i32,
            self.tile.3 as i32,
        );
        let (sx, sy) = (x as i32 + dc, y as i32 + dr);
        let (w, h) = (w as i32, h as i32);
        if sx < tx0 || sy < ty0 || sx + w > tx1 || sy + h > ty1 {
            return false;
        }
        // lane-b128m: libaom counts this wavefront in SUPERBLOCKS, and the
        // superblock is 128 wide under `EC_AV1_SB128` (`INTRABC_DELAY_SB =
        // INTRABC_DELAY_PIXELS >> sb_size_log2`, i.e. 2 there and 4 here).
        // Counting 64s under a 128 superblock let the search take a source
        // from a 64 row the 128 visit order had not reconstructed yet.
        let sb_log2 = if crate::encode::sb128_on() { 7 } else { 6 };
        let delay: i32 = if sb_log2 == 7 {
            INTRABC_DELAY_SB64 / 2
        } else {
            INTRABC_DELAY_SB64
        };
        let active_sb_row = (y as i32 - ty0) >> sb_log2;
        let active_sb_col = (x as i32 - tx0) >> sb_log2;
        let src_sb_row = (sy + h - 1 - ty0) >> sb_log2;
        let src_sb_col = (sx + w - 1 - tx0) >> sb_log2;
        // This lane's own restriction (see the doc comment).
        if src_sb_row >= active_sb_row {
            return false;
        }
        let total = ((tx1 - tx0 - 1) >> sb_log2) + 1;
        if src_sb_row * total + src_sb_col >= active_sb_row * total + active_sb_col - delay {
            return false;
        }
        // libaom's wavefront: only the top-left cone of the frame is legal.
        let wf = (1 + delay) * (active_sb_row - src_sb_row);
        src_sb_col < active_sb_col - delay + wf
    }
}

/// How many block searches ran, how many found at least one VALID candidate,
/// and how many won on rate-distortion -- the hash hit rate a gate prints
/// (class `gate-blind-to-feature`: "the search ran" and "the search found
/// something" and "the block took it" are three different claims).
pub(super) static IBC_SEARCH: [std::sync::atomic::AtomicUsize; 3] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 3];

impl Drop for Ibc {
    fn drop(&mut self) {
        for (slot, n) in [self.lookups, self.found, self.hits]
            .into_iter()
            .enumerate()
        {
            IBC_SEARCH[slot].fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Reads [`IBC_SEARCH`] and zeroes it, so a gate can attribute the counts to
/// its own encode.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn take_intrabc_search() -> [usize; 3] {
    std::array::from_fn(|i| IBC_SEARCH[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// [`crate::tile::write_dv_component`]'s static price -- the same shape as
/// [`mv_component_bits`] without the `mv_fr` symbol, which `force_integer_mv`
/// infers rather than codes.
pub(super) fn dv_component_bits(diff: i32) -> f64 {
    let z = diff.unsigned_abs() as i32 - 1;
    let mut bits = symbol_bits(&cdf::MV_SIGN, usize::from(diff < 0));
    let class = mv_class_of(z);
    bits += symbol_bits(&cdf::MV_CLASS, class);
    let local = z - mv_class_base(class);
    if class == 0 {
        bits += symbol_bits(&cdf::MV_CLASS0_BIT, ((local >> 3) & 1) as usize);
    } else {
        let d = local >> 3;
        for i in 0..class {
            bits += symbol_bits(&cdf::MV_BIT[i], ((d >> i) & 1) as usize);
        }
    }
    bits
}

/// What one block vector costs against `pred`, plus the `skip` and
/// `use_intrabc` flags the block carries and the ordinary intra block does
/// not pay in this currency.
pub(super) fn dv_bits(dv: (i32, i32), pred: (i32, i32)) -> f64 {
    let diff = (dv.0 - pred.0, dv.1 - pred.1);
    let joint = match (diff.0 != 0, diff.1 != 0) {
        (false, false) => 0,
        (false, true) => 1,
        (true, false) => 2,
        (true, true) => 3,
    };
    let mut bits = symbol_bits(&cdf::MV_JOINT, joint) + 2.0;
    if diff.0 != 0 {
        bits += dv_component_bits(diff.0);
    }
    if diff.1 != 0 {
        bits += dv_component_bits(diff.1);
    }
    bits
}

/// The squared error of copying the `side`-square block at `(sx, sy)` of each
/// plane's reconstruction onto the source block at `(x, y)`, luma and both
/// chroma planes together -- what an intrabc block's whole (residual-free)
/// distortion is.
pub(super) fn ibc_sse(
    luma: &Plane,
    chroma: &[Plane; 2],
    (x, y): (usize, usize),
    (sx, sy): (usize, usize),
    side: usize,
) -> f64 {
    // Inside the true frame only, for the reason [`Plane::block_sse`] is
    // (lane-sse): an intrabc block at the edge would otherwise be ranked by
    // how well it copies the padding.
    let mut sse = 0.0f64;
    let clip = |whole: usize, inside: usize| match edge_sse() {
        true => whole.min(inside),
        false => whole,
    };
    let rows = clip(side, luma.true_height.saturating_sub(y));
    let cols = clip(side, luma.true_width.saturating_sub(x));
    for row in 0..rows {
        let src = &luma.source[(y + row) * luma.width + x..][..cols];
        let rec = &luma.reconstruction[(sy + row) * luma.width + sx..][..cols];
        for (&s, &r) in src.iter().zip(rec) {
            let d = f64::from(i32::from(s) - i32::from(r));
            sse += d * d;
        }
    }
    let half = side / 2;
    for plane in chroma {
        let rows = clip(half, plane.true_height.saturating_sub(y / 2));
        let cols = clip(half, plane.true_width.saturating_sub(x / 2));
        for row in 0..rows {
            let src = &plane.source[(y / 2 + row) * plane.width + x / 2..][..cols];
            let rec = &plane.reconstruction[(sy / 2 + row) * plane.width + sx / 2..][..cols];
            for (&s, &r) in src.iter().zip(rec) {
                let d = f64::from(i32::from(s) - i32::from(r));
                sse += d * d;
            }
        }
    }
    sse
}

/// Copies the `side`-square block at `(sx, sy)` of each plane's
/// reconstruction onto `(x, y)` -- an intrabc block's whole reconstruction,
/// since it carries no residual (`skip`). Full-pel in both planes by
/// construction ([`intrabc_step`] keeps every DV even), which is what makes
/// this a plain copy where decode.rs runs the bilinear kernel over the same
/// samples.
pub(super) fn ibc_commit(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    (sx, sy): (usize, usize),
    side: usize,
) {
    for row in 0..side {
        let from = (sy + row) * luma.width + sx;
        let to = (y + row) * luma.width + x;
        let copied: Vec<u8> = luma.reconstruction[from..from + side].to_vec();
        luma.reconstruction[to..to + side].copy_from_slice(&copied);
    }
    let half = side / 2;
    for plane in chroma {
        for row in 0..half {
            let from = (sy / 2 + row) * plane.width + sx / 2;
            let to = (y / 2 + row) * plane.width + x / 2;
            let copied: Vec<u8> = plane.reconstruction[from..from + half].to_vec();
            plane.reconstruction[to..to + half].copy_from_slice(&copied);
        }
        plane.commit_ctx_zero(x / 2, y / 2, half);
    }
    luma.commit_ctx_zero(x, y, side);
}

/// Searches this block's own DV: every hash-table position whose 16x16 luma
/// square equals the block's source square, plus the previous block's DV as a
/// local fallback, scored by [`ibc_sse`] against the DV's own price. Returns
/// the winning `(dv, cost)`, or `None` when nothing valid was found.
pub(super) fn ibc_search(
    ibc: &mut Ibc,
    luma: &Plane,
    chroma: &[Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    lambda: f64,
) -> Option<((i32, i32), f64, (usize, usize))> {
    let pred = ibc.last_dv.unwrap_or((0, -(64 + 256) * 8));
    let mut best: Option<((i32, i32), f64, (usize, usize))> = None;
    let consider =
        |ibc: &Ibc, sx: usize, sy: usize, best: &mut Option<((i32, i32), f64, (usize, usize))>| {
            let dv = ((sy as i32 - y as i32) * 8, (sx as i32 - x as i32) * 8);
            if !ibc.dv_valid(x, y, side, side, dv) {
                return;
            }
            let cost = ibc_sse(luma, chroma, (x, y), (sx, sy), side) + lambda * dv_bits(dv, pred);
            if best.as_ref().is_none_or(|b| cost < b.1) {
                *best = Some((dv, cost, (sx, sy)));
            }
        };
    ibc.lookups += 1;
    let key = ibc_hash(luma.source, luma.width, x, y);
    // The table is filled in raster order, so the entries BEFORE this block's
    // own position are exactly the already-coded ones -- and the nearest of
    // them costs the fewest DV bits. Walking backwards from the block's own
    // slot is what makes the search look up-and-left; taking the newest
    // entries outright only ever finds candidates below and to the right,
    // every one of which `dv_valid` rejects.
    let hits: Vec<(u32, u32)> = ibc
        .table
        .get(&key)
        .map(|h| {
            let end = h.partition_point(|&(hx, hy)| (hy, hx) < (y as u32, x as u32));
            h[..end].iter().rev().take(32).copied().collect()
        })
        .unwrap_or_default();
    for (hx, hy) in hits {
        consider(&*ibc, hx as usize, hy as usize, &mut best);
    }
    if let Some(dv) = ibc.last_dv {
        let (sx, sy) = (x as i32 + dv.1 / 8, y as i32 + dv.0 / 8);
        if sx >= 0 && sy >= 0 {
            consider(&*ibc, sx as usize, sy as usize, &mut best);
        }
    }
    if best.is_some() {
        ibc.found += 1;
    }
    best
}

// Spec-shaped signature; bundling the operands would obscure the spec mapping.
#[allow(clippy::too_many_arguments)]
pub(super) fn code_square(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    search: &Search,
    mode_bits: &[f64; 13],
    // The frame header's `tx_mode == TxMode::Select`: the block then searches
    // its own transform depth ([`Plane::code_tx_depth`]) instead of coding one
    // transform over its whole side.
    tx_select: bool,
    // This tile's intra block-copy state ([`Ibc`]), `None` on a frame whose
    // header did not set `allow_intrabc`.
    ibc: Option<&mut Ibc>,
    // Whether this is a KEY frame's block, read only by the split census
    // ([`SPLIT_CENSUS`]): the two frame types split at very different rates,
    // which is this lane's whole finding.
    census_key: bool,
    fctx: &crate::decode::FrameCtx,
) -> (BlockCoeffs, f64) {
    let (luma_set, chroma_set) = if side == SUPERBLOCK {
        // A whole superblock coded as ONE intra block (lane-i64): one
        // TX_64X64 luma transform, whose coded quarter is the 32x32 corner
        // `code_from_prediction` hands back, and one TX_32X32 per chroma
        // plane -- the same three sets the key frame writer's
        // `Superblock::Whole` arm codes, and the same the inter 64x64 root
        // uses (`search_skip_64`).
        (TxbSet::Luma64, TxbSet::Chroma32)
    } else if side == BLOCK {
        (TxbSet::Luma32, TxbSet::Chroma16)
    } else if side == 8 {
        // An 8x8 leaf under a straddling 16x16 (lane-av1-rect). r5 correction:
        // `TxbSet::Chroma4`'s doc now has the checked spec fact -- chroma is
        // `is_chroma_reference` at every BLOCK_8X8, so it is coded per leaf at
        // 4x4, not once at the parent's 8x8. No caller passes `side == 8` yet
        // (this branch is still unreached), so leaving `Chroma8` here would
        // silently wire the wrong table the moment one does; `Chroma4` is
        // what a real caller needs.
        (TxbSet::Luma8, TxbSet::Chroma4)
    } else {
        (TxbSet::Luma16, TxbSet::Chroma8)
    };
    let at = At {
        x,
        y,
        side,
        reach: Reach::of(side, x, y, luma.true_width, luma.true_height, fctx),
        set: luma_set_for(luma_set, search.screen),
    };
    let (mut luma_coeffs, mode, angle_delta, mut cost, tx_type) =
        luma.search_block(at, search, mode_bits, fctx);
    let mut luma_tx_types = vec![tx_type];
    // The depth search runs on the mode the whole-block transform picked --
    // libaom's own ordering, and the reason it costs one extra pass per depth
    // rather than one per (mode, depth) pair.
    let mut tx_depth = 0u8;
    // A 64x64 block has no depth to search (`max_tx_depth`), but the writer
    // still codes its `tx_depth` symbol off `TX_SIZE_CAT3`, so the incumbent
    // pays that zero exactly as a 32x32's depth search makes it pay
    // `tx_depth_bits(32, 0)` -- otherwise the 64-versus-four-32 comparison
    // below hands the root a symbol for free.
    if tx_select && side == SUPERBLOCK {
        cost += search.lambda * tx_depth_bits(side, 0);
    }
    if tx_select && max_tx_depth(side) > 0 {
        let mut best = (
            cost + search.lambda * tx_depth_bits(side, 0),
            0usize,
            luma.snapshot(x, y, side),
            luma_coeffs,
        );
        let mut best_types = std::mem::take(&mut luma_tx_types);
        // The census wants the flat arm's own (rate, distortion), which
        // `search_block` only hands back folded into one cost -- so under the
        // env flag alone it is re-coded at depth 0 and the plane restored.
        let census_flat = split_census_on().then(|| {
            let kept = luma.snapshot(x, y, side);
            let (_, sse, bits, _) =
                luma.code_tx_depth(at, mode, angle_delta, 0, None, search, fctx);
            luma.restore(x, y, side, &kept);
            (bits + tx_depth_bits(side, 0), sse)
        });
        let mut census_split: Option<(f64, f64)> = None;
        for depth in 1..=max_tx_depth(side) {
            let (levels, sse, bits, types) =
                luma.code_tx_depth(at, mode, angle_delta, depth, None, search, fctx);
            let rate = bits + mode_bits[usize::from(mode)] + tx_depth_bits(side, depth);
            let cost_d = sse + search.lambda * split_lambda_mult() * rate;
            if census_flat.is_some() {
                let c = (bits + tx_depth_bits(side, depth), sse);
                if census_split
                    .is_none_or(|b| sse + search.lambda * rate < b.1 + search.lambda * b.0)
                {
                    census_split = Some(c);
                }
            }
            if cost_d < best.0 {
                best = (
                    cost_d,
                    depth,
                    luma.snapshot(x, y, side),
                    coeffs(&levels, side),
                );
                best_types = types;
            }
        }
        if let (Some(flat), Some(split)) = (census_flat, census_split) {
            split_census(0, census_key, side, best.1 > 0, split, flat);
        }
        luma_tx_types = best_types;
        luma.restore(x, y, side, &best.2);
        cost = best.0;
        tx_depth = best.1 as u8;
        luma_coeffs = best.3;
        TX_DEPTH_HITS[best.1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    // The palette candidate is priced against the mode the luma search (and
    // its transform-depth search) already settled on, so a block only takes
    // one when it beats the best ordinary intra coding of the same pixels.
    // A palette block is `DC_PRED` with `tx_depth` 0 by construction: its
    // prediction is the colour map, not the DC of its neighbours.
    let mut mode = mode;
    let mut angle_delta = angle_delta;
    let mut palette = None;
    if search.screen && crate::decode::palette_bsize_ctx(side).is_some() {
        let source: Vec<u8> = (y..y + side)
            .flat_map(|row| luma.source[row * luma.width + x..][..side].to_vec())
            .collect();
        let kept = luma.snapshot(x, y, side);
        // spec 5.11.50: a block cut by the frame's right/bottom edge codes
        // its map over the ON-SCREEN corner only and the decoder replicates
        // that outward, so the searched map is folded the same way here --
        // the prediction priced, the reconstruction committed and the symbols
        // `write_color_index_map` writes are then one and the same map
        // (lane-palw).
        let on = (
            side.min(luma.true_width.saturating_sub(x)),
            side.min(luma.true_height.saturating_sub(y)),
        );
        for mut pal in palette_candidates(&source) {
            let n = usize::from(pal.size);
            crate::decode::palette_replicate(&mut pal.map, side, side, on);
            let prediction: Vec<u8> = pal
                .map
                .iter()
                .map(|&i| pal.colors[usize::from(i).min(n - 1)] as u8)
                .collect();
            let trial = luma.code_from_prediction(
                x,
                y,
                side,
                &prediction,
                false,
                search.base_q_idx,
                search.deadzone,
                luma_set,
            );
            let cost_p = trial.sse
                + search.lambda
                    * (trial.bits
                        + mode_bits[DC_PRED as usize]
                        + crate::tile::palette_bits(&pal, side, on)
                        + if tx_select {
                            tx_depth_bits(side, 0)
                        } else {
                            0.0
                        });
            if cost_p < cost {
                cost = cost_p;
                mode = DC_PRED;
                angle_delta = 0;
                tx_depth = 0;
                luma_coeffs = coeffs(&trial.levels, side.min(BLOCK));
                luma.commit(x, y, side, &trial);
                palette = Some(pal);
            }
        }
        if palette.is_none() {
            luma.restore(x, y, side, &kept);
        }
    }

    // The five recursive filter-intra modes (spec 5.11.14), priced against
    // whatever the mode, transform-depth and palette searches settled on --
    // the same "one candidate loop after the incumbent is known" shape the
    // palette above uses. A block that takes one is `DC_PRED` with no palette
    // by construction (`av1_filter_intra_allowed`), and its luma transform
    // types are coded from `fimode_to_intradir`'s row (`crate::tile::tx_row`).
    //
    // lane-fitu: on a SCREEN frame the candidate is priced at every transform
    // depth the ordinary intra search runs, each unit predicting recursively
    // off the units before it ([`Plane::code_tx_depth`]'s `filter_intra`
    // arm), so a block whose ordinary winner wanted a split transform is
    // compared against a filter arm that may split too.
    //
    // Measured (native BD table, lane-fitu): searching the depths on EVERY
    // frame moves screen -1.0/-0.4 and the bars rows -0.4/-0.7 but leaves
    // film A flat and film B +0.1/+0.5 WORSE -- the extra filter blocks are a
    // locally cheaper reconstruction that the film rows go on to predict from
    // (class `local-rd-on-references`) -- at +30..35% encoder wall. Screen
    // content is where it pays, and a non-screen frame keeps the depth-0-only
    // arm, byte for byte.
    let mut filter_intra: Option<u8> = None;
    if filter_intra_on()
        && palette.is_none()
        && let Some(class) = crate::decode::filter_intra_size_class(side)
    {
        let row = &cdf::FILTER_INTRA[class];
        // With the sequence bit on, EVERY `DC_PRED` block without a luma
        // palette carries the flag, so the incumbent pays its zero before the
        // comparison -- otherwise the candidate is charged a bit the block
        // spends either way (class `price-the-narrowing-not-the-table`).
        if mode == DC_PRED {
            cost += search.lambda * symbol_bits(row, 0);
        }
        let one_bits = symbol_bits(row, 1);
        let kept = luma.snapshot(x, y, side);
        let max_depth = if tx_select && search.screen {
            max_tx_depth(side)
        } else {
            0
        };
        let mut best: Option<(f64, u8, usize, (Vec<u8>, Vec<CoefCtx>), Vec<Coeff>)> = None;
        for fi in 0..5u8 {
            for depth in 0..=max_depth {
                // A filter-intra block's units are coded through
                // `code_from_prediction`, which is `DCT_DCT` throughout, so
                // this arm keeps the empty (all-`DCT_DCT`) type list.
                let (levels, sse, bits, _) =
                    luma.code_tx_depth(at, DC_PRED, 0, depth, Some(fi), search, fctx);
                let cost_f = sse
                    + search.lambda
                        * (bits
                            + mode_bits[DC_PRED as usize]
                            + one_bits
                            + symbol_bits(&cdf::FILTER_INTRA_MODE, usize::from(fi))
                            + if tx_select {
                                tx_depth_bits(side, depth)
                            } else {
                                0.0
                            });
                if cost_f < cost && best.as_ref().is_none_or(|b| cost_f < b.0) {
                    best = Some((
                        cost_f,
                        fi,
                        depth,
                        luma.snapshot(x, y, side),
                        coeffs(&levels, side),
                    ));
                }
            }
        }
        if let Some((cost_f, fi, depth, snapshot, levels)) = best {
            cost = cost_f;
            mode = DC_PRED;
            angle_delta = 0;
            tx_depth = depth as u8;
            luma_coeffs = levels;
            luma.restore(x, y, side, &snapshot);
            filter_intra = Some(fi);
        } else {
            luma.restore(x, y, side, &kept);
        }
    }

    // Chroma searches its own mode over [`CHROMA_MODES`], one symbol for both
    // planes; what it costs still counts towards the partition decision.
    let ([u, v], uv_mode, chroma_cost, palette_uv, cfl_alphas) =
        search_chroma(chroma, luma, (x, y), side, chroma_set, search, mode, fctx);
    cost += chroma_cost;

    // The intrabc arm is priced against the WHOLE block -- luma and both
    // chroma planes, residual-free -- so it is compared after the chroma
    // search has added its own cost, not against the luma decision alone.
    if let Some(ibc) = ibc
        && let Some((dv, cost_ibc, src)) =
            ibc_search(ibc, luma, chroma, (x, y), side, search.lambda)
            && cost_ibc < cost {
                ibc_commit(luma, chroma, (x, y), src, side);
                ibc.last_dv = Some(dv);
                ibc.hits += 1;
                return (
                    BlockCoeffs {
                        skip: true,
                        dv: Some(dv),
                        ..BlockCoeffs::default()
                    },
                    cost_ibc,
                );
            }
    // A palette or filter-intra block's luma was recoded through `DCT_DCT`
    // (its levels came from `code_from_prediction`), so the searched types
    // belong to a block this one no longer is.
    if palette.is_some() || filter_intra.is_some() {
        luma_tx_types = Vec::new();
    }
    (
        BlockCoeffs {
            u,
            v,
            luma: luma_coeffs,
            mode,
            uv_mode,
            tx_depth,
            palette,
            palette_uv,
            filter_intra,
            cfl_alphas,
            angle_delta_y: angle_delta as i8,
            luma_tx_types,
            ..BlockCoeffs::default()
        },
        cost,
    )
}

/// The chroma modes the search offers, and the only ones it may offer: each
/// predicts from the row directly above and the column directly to the left
/// and nothing further, so none of them needs the above-right/below-left
/// `Reach` a chroma block is coded with `Reach::none()` for. The eight
/// directional modes do read past that, and `UV_CFL_PRED` needs the luma
/// reconstruction and its own alpha syntax; both are left out here.
pub(super) const CHROMA_MODES: [u8; 7] = [
    DC_PRED,
    V_PRED,
    H_PRED,
    SMOOTH_PRED,
    SMOOTH_V_PRED,
    SMOOTH_H_PRED,
    PAETH_PRED,
];

/// The thirteen intra mode names, for the chroma fire count's print.
#[cfg(test)]
pub(super) const UV_MODE_NAMES: [&str; 14] = [
    "DC", "V", "H", "D45", "D135", "D113", "D157", "D203", "D67", "SMOOTH", "SMOOTH_V", "SMOOTH_H",
    "PAETH", "CFL",
];

/// How many blocks each chroma mode has won, indexed by the mode itself --
/// the fire count that says whether the search below reaches its candidates
/// at all (the `gate-blind-to-feature` class), printed once per gate run.
pub(super) static UV_MODE_HITS: [std::sync::atomic::AtomicUsize; 14] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 14];

/// How many blocks each CfL alpha magnitude (`1..=16`, indexed `mag - 1`) has
/// won, summed over both chroma planes -- the alpha histogram the gate prints
/// beside the CfL share (`gate-blind-to-feature`: a tool that fires at one
/// magnitude only is a search that never left its first candidate).
pub(super) static CFL_ALPHA_HITS: [std::sync::atomic::AtomicUsize; 17] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 17];

/// Reads [`CFL_ALPHA_HITS`] and zeroes it. Index 0 is "this plane took alpha
/// zero on a CfL block", `1..=16` the magnitudes.
#[cfg(test)]
pub(crate) fn take_cfl_alpha_hits() -> [usize; 17] {
    std::array::from_fn(|m| CFL_ALPHA_HITS[m].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// How many directional luma blocks won at each `angle_delta_y`, indexed
/// `delta + 3` -- the histogram that says whether the refinement below ever
/// leaves zero (`gate-blind-to-feature`).
pub(super) static ANGLE_DELTA_HITS: [std::sync::atomic::AtomicUsize; 7] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 7];

/// Reads [`ANGLE_DELTA_HITS`] and zeroes it.
#[cfg(test)]
pub(crate) fn take_angle_delta_hits() -> [usize; 7] {
    std::array::from_fn(|d| ANGLE_DELTA_HITS[d].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Whether the luma search refines a directional winner's `angle_delta_y`
/// (`EC_AV1_ANGLE=0` switches it off, the lane's own on/off pair).
pub(super) fn angle_delta_on() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> =
        std::sync::LazyLock::new(|| crate::envflags::var("EC_AV1_ANGLE").ok().map(|v| v != "0"));
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::ANGLE))
}

/// Whether the chroma search offers `UV_CFL_PRED` at all (`EC_AV1_CFL=0`
/// switches it off, which is how the lane measured its own on/off pair).
pub(super) fn cfl_on() -> bool {
    static ENV: std::sync::LazyLock<Option<bool>> =
        std::sync::LazyLock::new(|| crate::envflags::var("EC_AV1_CFL").ok().map(|v| v != "0"));
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::CFL))
}

// Whether the intra search offers the five recursive filter-intra modes at
// all, and with them whether the sequence header sets `enable_filter_intra`
// (`EC_AV1_FILTER_INTRA=0` switches both off, which is how the lane measured
// its own on/off pair -- and how every byte pin written before it still
// reproduces). It is also a speed lever: on at preset 0, off above it.
// lane-sb128: `use_128x128_superblock` (spec 5.5.1). `EC_AV1_SB128=0` turns
// it off for the whole process (a sequence-header field, so it cannot change
// mid-stream anyway), the same env-flag shape `EC_AV1_FILTER_INTRA` uses.
// NOT an `EncoderConfig` field: that struct is built by 90 literal sites in
// this workspace.

/// Whether a sequence codes 128x128 superblocks when the environment says
/// nothing. ON since lane-b128 gave the 128 root a real `PARTITION_NONE`
/// candidate ([`search_root_128`]): with SPLIT-only roots the size was pure
/// cost (lane-sb128b measured film B +24.9 -> +37.5 vs libaom), and with the
/// 128x128 block in it every row of the native BD gate is down or flat on
/// both columns (BD-rate vs libaom cpu-6 / rav1e s6, 12 frames):
///
/// | row | 64 superblocks | 128 + the 128 root |
/// |---|---|---|
/// | bars 1080p | -3.1 / -18.7 | **-3.7 / -19.2** |
/// | bars 2160p | +9.1 / -13.3 | **+8.5 / -13.8** |
/// | film A | +20.3 / -5.0 | **+20.1 / -5.2** |
/// | film B | +24.6 / -2.0 | **+24.3 / -2.2** |
/// | screen capture | +14.6 / -33.2 | **+14.4 / -33.2** |
///
/// and the long-GOP gate agrees (film A +24.0/-7.6 -> +23.6/-7.9, film B
/// +85.8/+7.1 -> +85.1/+6.6). Both arms re-run on lane-b128m's own base.
///
/// ON since lane-b128m closed the eleven 128-superblock desyncs that held it
/// off (the delta_q unit at the 128 superblock, the superblock size travelling
/// with a `FrameCtx` copy into a tile search worker, the writer's intrabc DV
/// predictor and wavefront, and seven test harnesses that handed the encoder a
/// 64-superblock context). `EC_AV1_SB128=0` turns it off.
pub(crate) const SB128_DEFAULT: bool = true;

/// Whether this process's sequences code 128x128 superblocks. An ENCODER
/// knob (like `speed::SPEED`), never read by a decode: read through a
/// process-global (not a thread-local) because the tile writers run on
/// worker threads; `2` is "the environment has not been read yet".
pub(super) static ENC_SB128: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

/// Whether this process's sequences code 128x128 superblocks.
pub(crate) fn sb128_on() -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    match ENC_SB128.load(Relaxed) {
        2 => {
            let on = crate::envflags::var("EC_AV1_SB128")
                .ok()
                .map_or(SB128_DEFAULT, |v| v != "0");
            ENC_SB128.store(u8::from(on), Relaxed);
            on
        }
        v => v == 1,
    }
}

/// Forces [`sb128_on`] for the rest of the process -- the superblock size is
/// a SEQUENCE field, so a gate that flips it must run alone (`#[ignore]`),
/// exactly like the speed-preset gates.
#[cfg(test)]
pub(crate) fn force_sb128(on: bool) {
    ENC_SB128.store(u8::from(on), std::sync::atomic::Ordering::Relaxed);
}

/// The luma loop-restoration unit size this sequence codes: a 128 superblock
/// cannot signal the 64 unit at all (spec `lr_params` reads ONE bit there,
/// `RESTORATION_TILESIZE_MAX >> (2 - (shift + 1))`, so the smallest unit is
/// 128), which is the one filter parameter the superblock size moves.
pub(crate) fn lr_unit() -> u32 {
    if sb128_on() { 128 } else { 64 }
}

pub(crate) fn filter_intra_on() -> bool {
    #[cfg(test)]
    if let Some(forced) = FILTER_INTRA_FORCE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_FILTER_INTRA")
            .ok()
            .map(|v| v != "0")
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::FILTER_INTRA))
}

/// Reads [`UV_MODE_HITS`] and zeroes it, so a gate can attribute the counts
/// to its own encode.
#[cfg(test)]
pub(crate) fn take_uv_mode_hits() -> [usize; 14] {
    std::array::from_fn(|m| UV_MODE_HITS[m].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Searches both chroma planes over [`CHROMA_MODES`] under one shared mode --
/// `uv_mode` is a single symbol covering U and V -- and commits the cheapest.
/// Returns each plane's levels, the mode, and what the pair cost (squared
/// error plus lambda times bits, the same currency the luma search speaks).
///
/// The chroma symbol itself was already priced at `DC_PRED` by [`mode_bits`],
/// which is what the luma mode decision was taken against, so what a
/// candidate owes here is only the *difference* from that -- plus, for `V`/`H`
/// (directional, spec `read_intra_angle_info`), the zero `angle_delta_uv` the
/// writer then has to code.
#[allow(clippy::too_many_arguments)]
/// How many of [`CHROMA_MODES`] the chroma search runs a full trial pair on,
/// ranked by the SAD of their predictions summed over both planes (`DC_PRED`
/// always kept, the same rule [`prune_by_sad`] applies to luma). `None`
/// trials all seven, as before this lever.
///
/// The census (`EC_AV1_CENSUS=1`) is what justifies the lever: chroma was
/// the search's largest trial population by far -- seven modes times two
/// planes for every intra block, against 3.7 luma trials -- and the RD
/// winner sat at SAD rank 0 for 67-77% of blocks and inside the top three
/// for 88%. `EC_AV1_CHROMA_K` sweeps it in any build.
pub(super) fn chroma_top_k() -> Option<usize> {
    static ENV: std::sync::LazyLock<Option<Option<usize>>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_CHROMA_K")
            .ok()
            .map(|v| v.parse::<usize>().ok().filter(|&k| k > 0))
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::CHROMA_K))
}

/// [`chroma_top_k`]'s default. `None`: no `K` cleared the keep rule on the BD
/// gate. Swept over 2/3/4/5 (vs libaom, base +121.1/+146.1/+79.3):
/// K=2 is +122.3/+147.5/+79.8 for -7.0/-8.3/-7.9% wall, K=3 +121.4/+146.8/
/// +80.6 for -5.4/-5.2/-4.6%, K=4 +120.5/+145.2/+79.3 (BD-neutral or better)
/// for only -3.4% wall and -2.8% instructions, K=5 +122.0/+145.1/+80.0. The
/// BD swings by about a point in either direction as near-tie chroma modes
/// flip, which is the measurement's own sensitivity, not a trend: nothing
/// here buys >=5% wall at <=+0.3 BD, so the default stays unpruned.
/// RE-MEASURED on lane-av1rejudge at [`LAMBDA_SCALE`] 0.05 with warp on:
/// K=4 is +17.1/-0.1, +47.1/+19.2, +51.6/-13.8 against the base's
/// +16.7/-0.5, +47.0/+19.1, +51.3/-14.3 -- every row worse. Still `None`.
pub(crate) const CHROMA_TOP_K: Option<usize> = None;

// Spec-shaped signature; bundling the operands would obscure the spec mapping.
#[allow(clippy::too_many_arguments)]
pub(super) fn search_chroma(
    chroma: &mut [Plane; 2],
    // The luma plane's RECONSTRUCTION under this block, already committed by
    // [`Plane::search_block`] -- the signal a `UV_CFL_PRED` candidate scales.
    luma: &Plane,
    (x, y): (usize, usize),
    side: usize,
    set: TxbSet,
    search: &Search,
    luma_mode: u8,
    fctx: &crate::decode::FrameCtx,
) -> (
    [Vec<Coeff>; 2],
    u8,
    f64,
    Option<crate::tile::PaletteUv>,
    Option<(i32, i32)>,
) {
    let at = At {
        x: x / 2,
        y: y / 2,
        side: side / 2,
        reach: Reach::none(),
        set,
    };
    // The census's cheap proxy for a chroma mode: the SAD of its prediction
    // summed over both planes, ranked by the same rule the luma histogram
    // uses. Only built under `EC_AV1_CENSUS`.
    // The SAD of each mode's prediction, summed over both planes -- the
    // census's ranking rule, and what [`chroma_top_k`] prunes by.
    let sad_scores = |chroma: &[Plane; 2]| -> Vec<(f64, u8)> {
        let u = chroma[0].intra_scores(at, &CHROMA_MODES, &[0.0; 13], 0.0, fctx);
        let v = chroma[1].intra_scores(at, &CHROMA_MODES, &[0.0; 13], 0.0, fctx);
        u.iter()
            .zip(v.iter())
            .map(|(&(us, mode), &(vs, _))| (us + vs, mode))
            .collect()
    };
    let modes: Vec<u8> = match chroma_top_k() {
        Some(k) if k < CHROMA_MODES.len() => prune_by_sad(&CHROMA_MODES, sad_scores(chroma), k),
        _ => CHROMA_MODES.to_vec(),
    };
    let uv_cdf = &cdf::UV_MODE_CFL[usize::from(luma_mode)];
    let dc_bits = symbol_bits(uv_cdf, usize::from(DC_PRED));
    let ranking: Vec<(f64, u8)> = if census_on() {
        sad_scores(chroma)
    } else {
        Vec::new()
    };
    census_add(2, 1);
    census_add(3, modes.len());
    let mut best: Option<(f64, u8, Trial, Trial)> = None;
    for mode in modes {
        let mut bits = symbol_bits(uv_cdf, usize::from(mode)) - dc_bits;
        if (V_PRED..=D67_PRED).contains(&mode) {
            bits += symbol_bits(&cdf::ANGLE_DELTA[usize::from(mode - V_PRED)], 3);
        }
        // lane-i64: `av1_get_ext_tx_set_type` resolves a CHROMA plane at
        // `tx_size_sqr >= TX_32X32` to `EXT_TX_SET_DCTONLY`, so the 32x32
        // chroma of a 64x64 block is `DCT_DCT` whatever
        // `Intra_Mode_To_Tx_Type` says -- the decoder's own `side >= 32` arm
        // (decode.rs `read_chroma_plane`), and ADST is not even defined at 32
        // points.
        let tx_type = if at.side >= 32 {
            TxType::DctDct
        } else {
            crate::decode::default_intra_tx_type(mode)
        };
        let u = chroma[0].trial_typed(
            at,
            mode,
            0,
            search.base_q_idx,
            search.deadzone,
            tx_type,
            fctx,
        );
        let v = chroma[1].trial_typed(
            at,
            mode,
            0,
            search.base_q_idx,
            search.deadzone,
            tx_type,
            fctx,
        );
        let cost = u.sse + v.sse + search.lambda * (bits + u.bits + v.bits);
        if best.as_ref().is_none_or(|(b, ..)| cost < *b) {
            best = Some((cost, mode, u, v));
        }
    }
    let (mut cost, mut mode, mut u, mut v) = best.expect("CHROMA_MODES is not empty");
    // The chroma palette arm (spec 5.11.46's plane-1 half): one k-means over
    // U -- the same 1-D Lloyd [`palette_candidates`] runs for luma, which is
    // what keeps `u_colors` ascending and distinct the way
    // `read_palette_colors_uv`'s merge/delta reader needs -- and V taken as
    // each cluster's own mean, since the colour-index map is SHARED by the
    // two planes. Priced against the best ordinary chroma mode above, so a
    // block only takes one when it beats it; a palette block's chroma mode is
    // `UV_DC_PRED` by construction.
    let mut palette_uv = None;
    if search.screen && crate::decode::palette_bsize_ctx(side).is_some() {
        debug_assert_eq!(crate::tile::palette_uv_side(side), at.side);
        let plane_source = |p: &Plane| -> Vec<u8> {
            (at.y..at.y + at.side)
                .flat_map(|row| p.source[row * p.width + at.x..][..at.side].to_vec())
                .collect()
        };
        let (u_source, v_source) = (plane_source(&chroma[0]), plane_source(&chroma[1]));
        // The luma on-screen extent carried into this plane exactly as the
        // reader does it (`decode::palette_onscreen_uv`); see the luma arm.
        let on_uv = crate::decode::palette_onscreen_uv(
            (side, side),
            (
                side.min(luma.true_width.saturating_sub(x)),
                side.min(luma.true_height.saturating_sub(y)),
            ),
            (at.side, at.side),
            // The encoder writes 4:2:0 only (`palette_uv_side` above).
            (1, 1),
        );
        for mut pal in palette_candidates(&u_source) {
            let n = usize::from(pal.size);
            crate::decode::palette_replicate(&mut pal.map, at.side, at.side, on_uv);
            // V's base colour per cluster: the mean of the samples the shared
            // map assigns to it (an empty cluster cannot happen -- every
            // colour `palette_candidates` returns is some pixel's nearest).
            let mut sums = [0u32; 8];
            let mut counts = [0u32; 8];
            for (&i, &sample) in pal.map.iter().zip(v_source.iter()) {
                let k = usize::from(i).min(n - 1);
                sums[k] += u32::from(sample);
                counts[k] += 1;
            }
            let mut v_colors = [0u16; 8];
            for k in 0..n {
                v_colors[k] = (sums[k] + counts[k] / 2).checked_div(counts[k]).unwrap_or(0) as u16;
            }
            let candidate = crate::tile::PaletteUv {
                size: pal.size,
                u_colors: pal.colors,
                v_colors,
                map: pal.map,
            };
            let predict = |colors: &[u16; 8]| -> Vec<u8> {
                candidate
                    .map
                    .iter()
                    .map(|&i| colors[usize::from(i).min(n - 1)] as u8)
                    .collect()
            };
            let (pu, pv) = (predict(&candidate.u_colors), predict(&candidate.v_colors));
            let u_trial = chroma[0].code_from_prediction(
                at.x,
                at.y,
                at.side,
                &pu,
                false,
                search.base_q_idx,
                search.deadzone,
                set,
            );
            let v_trial = chroma[1].code_from_prediction(
                at.x,
                at.y,
                at.side,
                &pv,
                false,
                search.base_q_idx,
                search.deadzone,
                set,
            );
            let cost_p = u_trial.sse
                + v_trial.sse
                + search.lambda
                    * (u_trial.bits
                        + v_trial.bits
                        + crate::tile::palette_uv_bits(&candidate, side, on_uv));
            if cost_p < cost {
                cost = cost_p;
                mode = DC_PRED;
                u = u_trial;
                v = v_trial;
                palette_uv = Some(candidate);
            }
        }
    }
    // The chroma-from-luma arm (spec 5.11.45, libaom `cfl_rd_pick_alpha`):
    // `UV_CFL_PRED` predicts as `DC_PRED` (`get_uv_mode`, spec 9.3) and then
    // nudges every sample by `alpha_q3` times the block's subsampled,
    // average-subtracted luma reconstruction. Offered only where
    // `is_cfl_allowed` (spec 5.11.5) does -- a luma block at most 32x32 --
    // and priced against the best ordinary chroma mode above, palette
    // included, so a block only takes it when it beats both.
    let mut cfl_alphas = None;
    if side <= 32 && cfl_on() {
        // The AC signal, through the decoder's own
        // `cfl_luma_subsampling_420_lbd_c` + `subtract_average_c` body.
        let ac = crate::decode::cfl_ac_q3_at(x, y, side, side, |lx, ly| {
            i32::from(luma.reconstruction[ly * luma.width + lx])
        });
        let dc_prediction = |plane: &Plane| -> Vec<u8> {
            let (mut above_buf, mut left_buf) = ([0u8; 2 * SUPERBLOCK], [0u8; 2 * SUPERBLOCK]);
            let (above, left, corner) =
                plane.edges_into(at.x, at.y, at.side, at.reach, &mut above_buf, &mut left_buf);
            let mut prediction = vec![0u8; at.side * at.side];
            intra_predict_u8(
                DC_PRED,
                0,
                above,
                left,
                corner,
                at.side,
                at.side,
                false,
                false,
                &mut prediction,
                fctx,
            );
            prediction
        };
        let bases = [dc_prediction(&chroma[0]), dc_prediction(&chroma[1])];
        // `av1_cfl_predict_block` (cfl.c): clip the nudged prediction, then
        // add the residual -- which is what [`Plane::code_from_prediction`]
        // does with what this returns.
        let apply = |base: &[u8], alpha: i32| -> Vec<u8> {
            base.iter()
                .zip(ac.iter())
                .map(|(&b, &a)| {
                    (i32::from(b) + crate::decode::cfl_scaled(alpha, a)).clamp(0, 255) as u8
                })
                .collect()
        };
        // The least-squares alpha, then its two neighbours: minimising
        // `sum (source - base - alpha*ac/64)^2` over a continuous alpha is one
        // dot-product ratio, and the quantised optimum is one of the two
        // integers around it (the clamp and the Q6 rounding are what the SSE
        // check below re-decides). libaom sweeps all sixteen magnitudes under
        // full RD; three predictions per plane buy nearly the same alpha for a
        // thirtieth of the search.
        let pick_alpha = |plane: &Plane, base: &[u8]| -> i32 {
            let (mut num, mut den) = (0.0f64, 0.0f64);
            for row in 0..at.side {
                for col in 0..at.side {
                    let i = row * at.side + col;
                    let d = f64::from(plane.source[(at.y + row) * plane.width + at.x + col])
                        - f64::from(base[i]);
                    num += d * f64::from(ac[i]);
                    den += f64::from(ac[i]) * f64::from(ac[i]);
                }
            }
            if den == 0.0 {
                return 0;
            }
            let ideal = 64.0 * num / den;
            let mut best = (plane.block_sse(at.x, at.y, at.side, base), 0i32);
            for alpha in [ideal.floor() as i64, ideal.ceil() as i64] {
                let alpha = alpha.clamp(-16, 16) as i32;
                if alpha == 0 {
                    continue;
                }
                let sse = plane.block_sse(at.x, at.y, at.side, &apply(base, alpha));
                if sse < best.0 {
                    best = (sse, alpha);
                }
            }
            best.1
        };
        let picked = [
            pick_alpha(&chroma[0], &bases[0]),
            pick_alpha(&chroma[1], &bases[1]),
        ];
        if picked != [0, 0] {
            let cfl_bits = symbol_bits(uv_cdf, crate::tile::UV_CFL_PRED) - dc_bits;
            let trial = |plane_idx: usize, alpha: i32| -> Trial {
                let prediction = if alpha == 0 {
                    bases[plane_idx].clone()
                } else {
                    apply(&bases[plane_idx], alpha)
                };
                chroma[plane_idx].code_from_prediction(
                    at.x,
                    at.y,
                    at.side,
                    &prediction,
                    false,
                    search.base_q_idx,
                    search.deadzone,
                    set,
                )
            };
            // At least one alpha must be nonzero: `cfl_alpha_signs` has no
            // (ZERO, ZERO) joint value ([`crate::tile::cfl_joint_sign`]).
            let combos: [(i32, i32); 3] = [(picked[0], picked[1]), (picked[0], 0), (0, picked[1])];
            for &(alpha_u, alpha_v) in &combos {
                if (alpha_u, alpha_v) == (0, 0) {
                    continue;
                }
                let cu = trial(0, alpha_u);
                let cv = trial(1, alpha_v);
                let bits =
                    cfl_bits + crate::tile::cfl_alpha_bits(alpha_u, alpha_v) + cu.bits + cv.bits;
                let cost_cfl = cu.sse + cv.sse + search.lambda * bits;
                if cost_cfl < cost {
                    cost = cost_cfl;
                    mode = crate::tile::UV_CFL_PRED as u8;
                    u = cu;
                    v = cv;
                    palette_uv = None;
                    cfl_alphas = Some((alpha_u, alpha_v));
                }
            }
        }
    }
    if let Some((alpha_u, alpha_v)) = cfl_alphas {
        for alpha in [alpha_u, alpha_v] {
            CFL_ALPHA_HITS[alpha.unsigned_abs() as usize]
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if census_on() {
        CHROMA_RANK[sad_rank(&ranking, mode).min(6)]
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    UV_MODE_HITS[usize::from(mode)].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    chroma[0].commit(at.x, at.y, at.side, &u);
    chroma[1].commit(at.x, at.y, at.side, &v);
    (
        [coeffs(&u.levels, at.side), coeffs(&v.levels, at.side)],
        mode,
        cost,
        palette_uv,
        cfl_alphas,
    )
}

/// Codes one leaf of a straddling quadrant as whichever costs least of intra
/// ([`code_square`], unchanged) or `NEARESTMV` against `reference` at
/// `stack.nearest_mv` -- the same NEARESTMV candidate [`search_inter_block`]
/// prices for a whole 32x32 block, just at this leaf's own `side` (16, for a
/// 16x16 leaf under a straddling 32x32 quadrant, or 8, for an 8x8 leaf under
/// a straddling 16x16 -- lane-av1inter8) and through [`TxbSet::Luma16Inter`]
/// or [`TxbSet::Luma8Inter`]. `NEWMV` (a motion search of its own) is not
/// attempted here: these leaves only ever fill the sliver of real content a
/// true edge lands in the middle of a larger block, not a leaf worth a full
/// search, so `NEARESTMV`-or-intra is this function's deliberate first cut
/// (see `write_inter_frame_leaf`'s doc comment).
#[allow(clippy::too_many_arguments)]
/// The compound MV stack of each `LAST` + extra-reference pair at one leaf,
/// as [`code_square_inter`] wants them: empty on a frame that codes no
/// `reference_select`, or when the leaf compound candidates are switched off.
/// One helper because THREE leaf call sites need it -- the split quadrants
/// and, since lane-av1comp4, both straddling-edge sites, which used to pass
/// `&[]` and so coded no compound block anywhere along the frame's true edge.
pub(super) fn leaf_compound_stacks<'a>(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    mi_cols: usize,
    mi_rows: usize,
    reference_select: bool,
    golden: Option<&'a Picture>,
    altref: Option<&'a Picture>,
) -> Vec<(i8, &'a Picture, crate::mvstack::CompoundMvStack)> {
    if !(reference_select && leaf_compound()) {
        return Vec::new();
    }
    [
        (crate::mvstack::GOLDEN_FRAME, golden),
        (crate::mvstack::ALTREF_FRAME, altref),
    ]
    .into_iter()
    .filter_map(|(r, pic)| {
        pic.map(|p| {
            (
                r,
                p,
                crate::mvstack::find_mv_stack_compound(
                    grid,
                    mi_row,
                    mi_col,
                    bw4,
                    bh4,
                    (crate::mvstack::LAST_FRAME, r),
                    mi_cols,
                    mi_rows,
                    grid.sign_bias_table(),
                    &[(0, 0); 7],
                    None,
                ),
            )
        })
    })
    .collect()
}

/// The OBMC prediction of one square single-reference block: this block's own
/// translation prediction (`mc::predict`) with every bordering above/left
/// neighbour blended into the overlap strips by the DECODER's own plan and
/// blend ([`crate::decode::obmc_plan`] / [`crate::decode::obmc_run`]), so what
/// the search prices is what a decoder reconstructs -- never a second
/// implementation of the blend. `None` when the block has no overlappable
/// neighbour at all (the writer would then code no `motion_mode` symbol) or
/// when the plan cannot be built (a neighbour naming a reference this encode
/// does not hold).
#[allow(clippy::too_many_arguments)]
pub(super) fn obmc_prediction(
    grid: &MiGrid,
    (mi_row, mi_col): (usize, usize),
    (mi_rows, mi_cols): (usize, usize),
    (x, y): (usize, usize),
    side: usize,
    mv: (i32, i32),
    refs: &[Option<&Picture>; 8],
    // This block's OWN reference planes, luma then both chroma, each as
    // (samples, stride, true width, true height).
    ref_planes: [(&[u16], usize, usize, usize); 3],
    frame_size: (usize, usize),
    fctx: &crate::decode::FrameCtx,
) -> Option<[Vec<u8>; 3]> {
    let n4 = side / 4;
    if !crate::decode::has_overlappable_neighbour(grid, mi_row, mi_col, n4, n4, mi_cols, mi_rows) {
        return None;
    }
    let refpix = crate::decode::RefPix::ready(*refs);
    let plan = crate::decode::obmc_plan(
        grid,
        &[],
        &[],
        mi_row,
        mi_col,
        n4,
        n4,
        mi_rows,
        mi_cols,
        side,
        side,
        side,
        crate::decode::ss_x(fctx),
        crate::decode::ss_y(fctx),
        side / 2,
        x,
        y,
        x / 2,
        y / 2,
        &refpix,
        Some(fctx.interp_filter.get()),
        frame_size,
    )
    .ok()?;
    let mut pred = [
        vec![0u16; side * side],
        vec![0u16; side * side / 4],
        vec![0u16; side * side / 4],
    ];
    for (i, r) in ref_planes.into_iter().enumerate() {
        let luma_plane = i == 0;
        let (px, py) = if luma_plane { (x, y) } else { (x / 2, y / 2) };
        let s = if luma_plane { side } else { side / 2 };
        mc::predict(
            r.0,
            r.1,
            r.2,
            r.3,
            mv_to_q4(px, mv.1, luma_plane),
            mv_to_q4(py, mv.0, luma_plane),
            s,
            s,
            &mut pred[i],
            fctx,
        );
    }
    let [mut py_buf, mut pu_buf, mut pv_buf] = pred;
    crate::decode::obmc_run(&plan, &refpix, &mut py_buf, &mut pu_buf, &mut pv_buf, fctx);
    let u8s = |b: &[u16]| b.iter().map(|&v| v as u8).collect::<Vec<u8>>();
    Some([u8s(&py_buf), u8s(&pu_buf), u8s(&pv_buf)])
}

// Spec-shaped signature; bundling the operands would obscure the spec mapping.
#[allow(clippy::too_many_arguments)]
pub(super) fn code_square_inter(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    search: &Search,
    mode_bits: &[f64; 13],
    reference: &Picture,
    stack: &MvStack,
    // The COMPOUND stack of each `LAST` + extra-reference pair, built at THIS
    // leaf's own `bw4`/`bh4` (empty on a frame that does not code
    // `reference_select`, and at the straddling call sites).
    compound: &[(i8, &Picture, crate::mvstack::CompoundMvStack)],
    // lane-av1obmc2: the mi grid this leaf's neighbours were published into
    // (`record_mi`, in the writer's own coding order) and this leaf's place in
    // it -- what the OBMC candidate's eligibility walk and neighbour plan
    // read, the same state the tile writer reads when it codes the leaf's
    // `motion_mode` symbol.
    grid: &MiGrid,
    (mi_row, mi_col): (usize, usize),
    (mi_rows, mi_cols): (usize, usize),
    // Every picture an OBMC neighbour can name: a leaf only searches LAST,
    // but the neighbours it blends may be GOLDEN/ALTREF blocks.
    refs: &[Option<&Picture>; 8],
    fctx: &crate::decode::FrameCtx,
) -> (BlockCoeffs, f64) {
    // lane-arfmode: at the top ARF (arm 1) the leaf offers no intra
    // candidate at all -- `intra_cost` stays at infinity, so every
    // comparison below takes an inter candidate and the fallback arm at the
    // tail is unreachable. The planes hold what the caller snapshotted until
    // the winner commits: the intra trial used to commit here and be
    // overwritten by the inter commit, so skipping it changes no committed
    // sample, only work.
    let (intra_block, intra_cost) = if search.arf_leaf_intra {
        code_square(
            luma,
            chroma,
            (x, y),
            side,
            search,
            mode_bits,
            tx_select() && tx_select_inter(),
            None,
            false,
            fctx,
        )
    } else {
        arfmode_hit(0);
        (BlockCoeffs::default(), f64::INFINITY)
    };
    let luma_set = if side == 8 {
        TxbSet::Luma8Inter
    } else {
        TxbSet::Luma16Inter
    };
    let chroma_set = if side == 8 {
        TxbSet::Chroma4
    } else {
        TxbSet::Chroma8
    };

    let skip_bits = |skip: bool| skip_bits_at(stack, skip);
    let intra_inter_bits = |inter: bool| intra_inter_bits_at(stack, inter);
    let single_ref_bits = single_ref_bits(crate::mvstack::LAST_FRAME, stack);
    let mode_bits_inter = symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 1) // not NEWMV
        + symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 1) // not zero
        + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 0); // NEARESTMV

    let mv = stack.nearest_mv;
    let ref_luma = (
        &reference.y,
        reference.width,
        luma.true_width,
        luma.true_height,
    );
    let ref_u = (
        &reference.u,
        reference.width / 2,
        chroma[0].true_width,
        chroma[0].true_height,
    );
    let ref_v = (
        &reference.v,
        reference.width / 2,
        chroma[1].true_width,
        chroma[1].true_height,
    );
    let luma_trial = mc_trial(
        luma,
        x,
        y,
        side,
        mv,
        true,
        ref_luma.0,
        ref_luma.1,
        ref_luma.2,
        ref_luma.3,
        false,
        search.base_q_idx,
        search.deadzone,
        luma_set,
        fctx,
    );
    let u = mc_trial(
        &chroma[0],
        x / 2,
        y / 2,
        side / 2,
        mv,
        false,
        ref_u.0,
        ref_u.1,
        ref_u.2,
        ref_u.3,
        false,
        search.base_q_idx,
        search.deadzone,
        chroma_set,
        fctx,
    );
    let v = mc_trial(
        &chroma[1],
        x / 2,
        y / 2,
        side / 2,
        mv,
        false,
        ref_v.0,
        ref_v.1,
        ref_v.2,
        ref_v.3,
        false,
        search.base_q_idx,
        search.deadzone,
        chroma_set,
        fctx,
    );
    let skip = luma_trial.levels.iter().all(|&l| l == 0)
        && u.levels.iter().all(|&l| l == 0)
        && v.levels.iter().all(|&l| l == 0);
    let inter_cost = luma_trial.sse
        + u.sse
        + v.sse
        + search.lambda
            * (skip_bits(skip)
                + intra_inter_bits(true)
                + single_ref_bits
                + mode_bits_inter
                + if skip {
                    0.0
                } else {
                    luma_trial.bits + u.bits + v.bits
                });

    // NEARESTMV for 16x16 leaves, gated on real cost. r1/r2 found the gate's
    // desync in `mvstack.rs`'s `find_mv_stack`: it lacked spec 7.10.2.4's
    // extended row/col scan, invisible at 8-mi-only geometry (the immediate
    // scan's own coverage already reached as far) but a real gap at 4-mi
    // leaf geometry. r3 ported the extended scan and, while proving it a
    // no-op at 8-mi (`extended_row_scan_is_a_no_op_at_8mi_geometry_*`,
    // `mvstack.rs`), found and fixed a second bug the extended scan's
    // coverage bookkeeping (`processed_rows`/`processed_cols`) exposed: an
    // intra neighbour is a coded cell too (libaom's `scan_row_mbmi`/
    // `scan_col_mbmi` advance coverage from any candidate's `bsize`,
    // regardless of whether it also casts a ref-frame-matching vote) --
    // `MiGrid` now records intra cells (`tile.rs`/`encode.rs`'s `else`
    // branches), which is what made 8-mi geometry a true no-op instead of
    // one that only held when every neighbour happened to be inter.
    // A motion search of this leaf's own (lane-av1rd2): before it, a leaf
    // only ever took the stack's NEARESTMV candidate or went intra, so a
    // 32x32 block splitting because its four quarters MOVE differently could
    // not code the movement it split for. Same candidate `search_inter_block`
    // prices for a whole block -- `motion::search` from `pred_mv`, rounded to
    // a vector the residual syntax can name -- at this leaf's own side.
    let mut best: (f64, Option<(Trial, Trial, Trial, bool, InterInfo)>) = (inter_cost, None);
    // The `NEARESTMV` candidate `inter_cost` prices, named once so the OBMC
    // re-price below can hand it back as the winner's own mode info.
    let nearest_info = InterInfo {
        ref1: None,
        mv1: (0, 0),
        ref_frame: crate::mvstack::LAST_FRAME,
        mode: InterMode::NearestMv,
        mv,
        ref_mv_idx: 0,
    };
    // What the current SINGLE-REFERENCE winner pays in mode/reference/mv
    // syntax, apart from its residual: the OBMC candidate re-prices the same
    // syntax against a different prediction, so both sides of that comparison
    // carry it.
    let mut single_syntax = intra_inter_bits(true) + single_ref_bits + mode_bits_inter;
    // This leaf's own `NEWMV` vector, kept whether or not it won the
    // single-reference comparison: it seeds the compound half-new candidate
    // below, exactly as `search_inter_block` seeds its own from the searches
    // it already ran.
    let mut searched_new: Option<(i32, i32)> = None;
    // What that search cost, kept for the second reference's own early-out
    // below (`None` when this leaf ran no search at all).
    let mut searched_new_cost: Option<f64> = None;
    let source_block = luma.source_block(x, y, side);
    if leaf_new_mv() {
        let (seeds, seed_n) = mv_seeds(stack);
        let found = motion::search(
            ref_luma.0,
            ref_luma.1,
            ref_luma.2,
            ref_luma.3,
            &source_block,
            x,
            y,
            side,
            side,
            stack.pred_mv,
            &seeds[..seed_n],
            search.lambda,
            fctx,
            1,
        );
        let new_mv = round_to_valid_mv(found.mv, stack.pred_mv);
        searched_new_cost = Some(found.cost);
        let mv_residual = mv_residual_bits(new_mv, stack.pred_mv);
        if mv_residual.is_none() {
            // lane-newmv: a vector the writer cannot name never reaches RD.
            newmv_census_block(
                side,
                false,
                f64::INFINITY,
                best.0,
                new_mv,
                stack.nearest_mv,
                0.0,
            );
        }
        if let Some(mv_bits) = mv_residual {
            searched_new = Some(new_mv);
            // Same reuse as `search_inter_block`: a NEWMV equal to the
            // stack's NEARESTMV is the same three trials, already run above.
            let (luma_new, u_new, v_new) = if new_mv == mv {
                (luma_trial.clone(), u.clone(), v.clone())
            } else {
                (
                    mc_trial(
                        luma,
                        x,
                        y,
                        side,
                        new_mv,
                        true,
                        ref_luma.0,
                        ref_luma.1,
                        ref_luma.2,
                        ref_luma.3,
                        false,
                        search.base_q_idx,
                        search.deadzone,
                        luma_set,
                        fctx,
                    ),
                    mc_trial(
                        &chroma[0],
                        x / 2,
                        y / 2,
                        side / 2,
                        new_mv,
                        false,
                        ref_u.0,
                        ref_u.1,
                        ref_u.2,
                        ref_u.3,
                        false,
                        search.base_q_idx,
                        search.deadzone,
                        chroma_set,
                        fctx,
                    ),
                    mc_trial(
                        &chroma[1],
                        x / 2,
                        y / 2,
                        side / 2,
                        new_mv,
                        false,
                        ref_v.0,
                        ref_v.1,
                        ref_v.2,
                        ref_v.3,
                        false,
                        search.base_q_idx,
                        search.deadzone,
                        chroma_set,
                        fctx,
                    ),
                )
            };
            let skip_new = luma_new.levels.iter().all(|&l| l == 0)
                && u_new.levels.iter().all(|&l| l == 0)
                && v_new.levels.iter().all(|&l| l == 0);
            let drl_bits = if stack.entries.len() > 1 {
                symbol_bits(&cdf::DRL_MODE[stack.drl_ctx[0]], 0)
            } else {
                0.0
            };
            let cost_new = luma_new.sse
                + u_new.sse
                + v_new.sse
                + search.lambda
                    * (skip_bits(skip_new)
                        + intra_inter_bits(true)
                        + single_ref_bits
                        + symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 0)
                        + drl_bits
                        + mv_bits
                        + mv_rate_surcharge(mv_bits)
                        + if skip_new {
                            0.0
                        } else {
                            luma_new.bits + u_new.bits + v_new.bits
                        });
            newmv_census_block(
                side,
                true,
                cost_new,
                best.0,
                new_mv,
                stack.nearest_mv,
                mv_bits,
            );
            if cost_new < best.0 {
                single_syntax = intra_inter_bits(true)
                    + single_ref_bits
                    + symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 0)
                    + drl_bits
                    + mv_bits;
                best = (
                    cost_new,
                    Some((
                        luma_new,
                        u_new,
                        v_new,
                        skip_new,
                        InterInfo {
                            ref1: None,
                            mv1: (0, 0),
                            ref_frame: crate::mvstack::LAST_FRAME,
                            mode: InterMode::NewMv,
                            mv: new_mv,
                            ref_mv_idx: 0,
                        },
                    )),
                );
            }
        }
    }

    // The COMPOUND candidates at this leaf (lane-av1comp3), the same set
    // `search_inter_block` prices for a whole 32x32 block:
    // `NEAREST_NEARESTMV`, `GLOBAL_GLOBALMV`, `NEW_NEARESTMV` off the vector
    // this leaf already searched, and -- since lane-av1comp4 gave the leaf a
    // SECOND-reference search of its own -- `NEAREST_NEWMV` and `NEW_NEWMV`
    // too. Priced with the same static-CDF approximation, plus the
    // compound-only syntax the writer emits.
    if leaf_compound() && !search.arf_leaf_compound && !compound.is_empty() {
        // lane-arfmode: arm 4 drops this leaf's compound set (counted once
        // per leaf, not once per pair); the single-reference candidates
        // above stay.
        arfmode_hit(2);
    }
    for (ref1, g, cstack) in compound
        .iter()
        .filter(|_| leaf_compound() && search.arf_leaf_compound)
    {
        let (ref1, g) = (*ref1, *g);
        // The whole `comp_mode` + reference-pair tree at the writer's own
        // contexts, for THIS pair (lane-ctx).
        let pair_bits = compound_ref_bits((crate::mvstack::LAST_FRAME, ref1), stack);
        let mode_ctx = cdf::COMPOUND_MODE_CTX_MAP[cstack.ref_mv_ctx >> 1][cstack.new_mv_ctx.min(4)];
        let mode_bits_of =
            |mode: usize| pair_bits + symbol_bits(&cdf::INTER_COMPOUND_MODE[mode_ctx], mode);
        let mut ccands: Vec<(((i32, i32), (i32, i32)), f64, InterInfo)> = vec![
            (
                cstack.nearest_mv,
                mode_bits_of(0),
                InterInfo {
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::NearestNearestMv,
                    mv: cstack.nearest_mv.0,
                    mv1: cstack.nearest_mv.1,
                    ref1: Some(ref1),
                    ref_mv_idx: 0,
                },
            ),
            (
                ((0, 0), (0, 0)),
                mode_bits_of(6),
                InterInfo {
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::GlobalGlobalMv,
                    mv: (0, 0),
                    mv1: (0, 0),
                    ref1: Some(ref1),
                    ref_mv_idx: 0,
                },
            ),
        ];
        // Every compound MV residual here is priced against compound stack
        // entry 0, which is what `assign_compound_mv` predicts from at
        // `ref_mv_idx = 0` -- so it is also what the second-reference search
        // below seeds from and rounds to, and no candidate it finds can then
        // fail `mv_residual_bits`.
        let cbase = cstack
            .entries
            .first()
            .map_or(cstack.nearest_mv, |e| (e.mv0, e.mv1));
        // This leaf's SECOND-reference motion search (lane-av1comp4). Without
        // it a leaf could only name a stack vector for the second half, and
        // screen capture showed the consequence: every leaf compound block
        // there was `NEAREST_NEARESTMV`. Skipped, like `search_inter_block`'s
        // extra-reference `NEWMV`, when this reference's own
        // `NEAREST_NEARESTMV` vector already prices `margin` times better
        // than the `LAST` search of this same leaf did -- one candidate
        // evaluation standing in for a whole search.
        let margin = leaf_second_new_margin();
        let mut second_new: Option<(i32, i32)> = None;
        if leaf_second_new_mv() {
            newmv_census_global(2);
            let skip = margin > 0.0
                && searched_new_cost.is_some_and(|last| {
                    motion::cost_at(
                        &g.y,
                        g.width,
                        luma.true_width,
                        luma.true_height,
                        &source_block,
                        x,
                        y,
                        side,
                        side,
                        cstack.nearest_mv.1,
                        cbase.1,
                        search.lambda,
                        fctx,
                    ) * margin
                        <= last
                });
            if skip {
                newmv_census_global(3);
            }
            if !skip {
                let mut seeds = [(0, 0); 4];
                let mut seed_n = 1;
                for e in cstack.entries.iter().take(3) {
                    seeds[seed_n] = e.mv1;
                    seed_n += 1;
                }
                let found = motion::search(
                    &g.y,
                    g.width,
                    luma.true_width,
                    luma.true_height,
                    &source_block,
                    x,
                    y,
                    side,
                    side,
                    cbase.1,
                    &seeds[..seed_n],
                    search.lambda,
                    fctx,
                    ref_distance(ref1),
                );
                second_new = Some(round_to_valid_mv(found.mv, cbase.1));
            }
        }
        if let Some(new_mv) = searched_new
            && let Some(b0) = mv_residual_bits(new_mv, cbase.0) {
                ccands.push((
                    (new_mv, cstack.nearest_mv.1),
                    mode_bits_of(3) + b0,
                    InterInfo {
                        ref_frame: crate::mvstack::LAST_FRAME,
                        mode: InterMode::NewNearestMv,
                        mv: new_mv,
                        mv1: cstack.nearest_mv.1,
                        ref1: Some(ref1),
                        ref_mv_idx: 0,
                    },
                ));
            }
        // `NEAREST_NEWMV` and `NEW_NEWMV`: the NEW halves are the two
        // searches this leaf ran, each residual priced against `cbase`.
        if let Some(mv1) = second_new
            && let Some(b1) = mv_residual_bits(mv1, cbase.1) {
                ccands.push((
                    (cstack.nearest_mv.0, mv1),
                    mode_bits_of(2) + b1,
                    InterInfo {
                        ref_frame: crate::mvstack::LAST_FRAME,
                        mode: InterMode::NearestNewMv,
                        mv: cstack.nearest_mv.0,
                        mv1,
                        ref1: Some(ref1),
                        ref_mv_idx: 0,
                    },
                ));
                if let Some((new_mv, b0)) =
                    searched_new.and_then(|m| mv_residual_bits(m, cbase.0).map(|b| (m, b)))
                {
                    ccands.push((
                        (new_mv, mv1),
                        mode_bits_of(7) + b0 + b1,
                        InterInfo {
                            ref_frame: crate::mvstack::LAST_FRAME,
                            mode: InterMode::NewNewMv,
                            mv: new_mv,
                            mv1,
                            ref1: Some(ref1),
                            ref_mv_idx: 0,
                        },
                    ));
                }
            }
        ccands.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        ccands.dedup_by_key(|c| c.0);
        let g_luma = (g.y.as_slice(), g.width, luma.true_width, luma.true_height);
        let g_u = (
            g.u.as_slice(),
            g.width / 2,
            chroma[0].true_width,
            chroma[0].true_height,
        );
        let g_v = (
            g.v.as_slice(),
            g.width / 2,
            chroma[1].true_width,
            chroma[1].true_height,
        );
        for (mvs, bits, info) in ccands {
            let luma_c = mc_trial_compound(
                luma,
                x,
                y,
                side,
                mvs,
                true,
                (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
                g_luma,
                search.base_q_idx,
                search.deadzone,
                luma_set,
                fctx,
            );
            let u_c = mc_trial_compound(
                &chroma[0],
                x / 2,
                y / 2,
                side / 2,
                mvs,
                false,
                (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3),
                g_u,
                search.base_q_idx,
                search.deadzone,
                chroma_set,
                fctx,
            );
            let v_c = mc_trial_compound(
                &chroma[1],
                x / 2,
                y / 2,
                side / 2,
                mvs,
                false,
                (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3),
                g_v,
                search.base_q_idx,
                search.deadzone,
                chroma_set,
                fctx,
            );
            let skip_c = luma_c.levels.iter().all(|&l| l == 0)
                && u_c.levels.iter().all(|&l| l == 0)
                && v_c.levels.iter().all(|&l| l == 0);
            let cost_c = luma_c.sse
                + u_c.sse
                + v_c.sse
                + search.lambda
                    * (skip_bits(skip_c)
                        + intra_inter_bits(true)
                        + bits
                        + if skip_c {
                            0.0
                        } else {
                            luma_c.bits + u_c.bits + v_c.bits
                        });
            if cost_c < best.0 {
                best = (cost_c, Some((luma_c, u_c, v_c, skip_c, info)));
            }
        }
    }

    // lane-av1obmc2: the OBMC candidate at a 16x16 / 8x8 LEAF (spec
    // `motion_mode == OBMC_CAUSAL`), offered to this leaf's single-reference
    // winner -- libaom's `motion_mode_allowed` refuses a compound block, and
    // the writer codes no symbol for one. The prediction is the decoder's own
    // plan and blend over the neighbours this leaf's `grid` already carries
    // (published by `record_mi` in the writer's coding order), so the
    // reconstruction priced here is the one a decoder rebuilds. lane-av1obmc
    // offered this only at 32x32, which is not where libaom cashes its OBMC
    // gain.
    let mut motion_won = 0u8;
    let single = match &best.1 {
        None => Some(nearest_info),
        Some((_, _, _, _, info)) if info.ref1.is_none() => Some(*info),
        Some(_) => None,
    };
    if let Some(info) = single.filter(|i| {
        (crate::envflags::env_flag!("EC_AV1_OBMC") || warp_on())
            && i.ref_frame == crate::mvstack::LAST_FRAME
    }) {
        let planes = [
            (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
            (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3),
            (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3),
        ];
        // Which alphabet this block's writer will code the symbol against
        // (`tile::write_motion_mode`, libaom `motion_mode_allowed`): the
        // 3-symbol one only under `allow_warped_motion` and at least one warp
        // sample. Mirrored here so the candidates are priced against the
        // table the tile really narrows.
        let warp_alphabet = warp_on()
            && crate::decode::num_proj_ref(
                grid,
                mi_row,
                mi_col,
                side / 4,
                side / 4,
                mi_cols,
                mi_rows,
                info.ref_frame,
                fctx,
            ) >= 1;
        let mm = |m: u8| motion_mode_bits(side, side, m, warp_alphabet);
        let mut candidates: Vec<(u8, [Vec<u8>; 3])> = Vec::new();
        if crate::envflags::env_flag!("EC_AV1_OBMC") && side >= obmc_min_side()
            && let Some(pred) = obmc_prediction(
                grid,
                (mi_row, mi_col),
                (mi_rows, mi_cols),
                (x, y),
                side,
                info.mv,
                refs,
                planes,
                (reference.width, reference.height),
                fctx,
            ) {
                candidates.push((1, pred));
            }
        if warp_alphabet
            && let Some(pred) = warp_prediction(
                grid,
                (mi_row, mi_col),
                (mi_rows, mi_cols),
                (x, y),
                side,
                info.mv,
                info.ref_frame,
                planes,
                fctx,
            ) {
                candidates.push((2, pred));
            }
        for (motion, pred) in candidates {
            let luma_o = luma.code_from_prediction(
                x,
                y,
                side,
                &pred[0],
                false,
                search.base_q_idx,
                search.deadzone,
                luma_set,
            );
            let u_o = chroma[0].code_from_prediction(
                x / 2,
                y / 2,
                side / 2,
                &pred[1],
                false,
                search.base_q_idx,
                search.deadzone,
                chroma_set,
            );
            let v_o = chroma[1].code_from_prediction(
                x / 2,
                y / 2,
                side / 2,
                &pred[2],
                false,
                search.base_q_idx,
                search.deadzone,
                chroma_set,
            );
            let skip_o = luma_o.levels.iter().all(|&l| l == 0)
                && u_o.levels.iter().all(|&l| l == 0)
                && v_o.levels.iter().all(|&l| l == 0);
            let cost_o = luma_o.sse
                + u_o.sse
                + v_o.sse
                + search.lambda
                    * (skip_bits(skip_o)
                        + single_syntax
                        + mm(motion)
                        + if skip_o {
                            0.0
                        } else {
                            luma_o.bits + u_o.bits + v_o.bits
                        });
            // The incumbent pays the motion_mode symbol IT would code, so
            // both sides of the comparison carry that syntax.
            if cost_o < best.0 + search.lambda * mm(motion_won) {
                motion_won = motion;
                best = (cost_o, Some((luma_o, u_o, v_o, skip_o, info)));
            }
        }
    }

    if let (cost, Some((luma_new, u_new, v_new, skip_new, info))) = best
        && cost < intra_cost {
            // A compound winner commits the flat trial it was priced from:
            // `commit_inter_luma`'s var-tx trial set is single-reference
            // (`mc_trial`), so a compound leaf codes `tx_depth = 0`.
            // lane-av1comp4 BUILT that upgrade (a `commit_inter_luma`
            // predicting each half-side trial from BOTH references, verified
            // byte-identical to this when its split is disabled) and MEASURED
            // it a loss: vs libaom +83.5/+99.7/+54.9 at no margin against the
            // +83.0/+99.9/+54.5 here, and +83.3/+100.2/+54.9, +83.8/+99.5/
            // +54.9, +83.3/+100.0/+54.9 at split margins 0.02/0.05/0.15. A
            // compound block's residual is small enough that the four extra
            // per-unit txb_skip/eob symbols cost more ladder bytes than this
            // encoder's static-CDF estimate prices them at, so the local RD
            // win does not convert. lane-av1txbits REBUILT it on top of a
            // search that prices against the tables the frame is really
            // written with (`tile::arm_pricing_cdfs`, the estimate error on
            // these very blocks measured down from +31% to +3%) and measured
            // it neutral-to-worse again: +78.8/+95.0/+54.5 and +36.5/+56.3/
            // +1.4 against the +78.8/+95.1/+54.5 and +36.1/+56.4/+1.4 of the
            // same build without it. The static-CDF estimate was NOT what
            // held the compound var-tx trial back. The flat trial stays.
            // An OBMC winner commits the flat trial too: `commit_inter_luma`
            // re-predicts each var-tx unit by plain translation, which is not
            // the prediction this candidate was priced from.
            // lane-av1txdepth REBUILT the compound trial set at LAMBDA_SCALE
            // 0.05 (both rejections above were measured at 0.1, the weight
            // the lambda lane showed was 2x too heavy): a compound winner is
            // offered the same var-tx split, its units predicted from BOTH
            // references (`mc_trial_compound`). RE-MEASURED native: film
            // 1080p +16.0/-1.1, film 2160p +46.6/+18.8, screen +51.4/-14.4
            // against the +16.2/-1.1, +46.6/+18.8, +51.3/-14.4 with it off --
            // one row 0.2 down, two flat, short of the two-down-one-flat keep
            // rule, so it stays behind `EC_AV1_COMP_VARTX` (default OFF)
            // rather than change the default on a single 0.2 row. It fires:
            // 37.0% / 11.1% / 32.9% of the compound blocks split. 640x384 for
            // the record: +70.5/+84.0/+48.9 and +28.9/+46.2/-3.7 against
            // +70.4/+85.3/+48.5 and +28.4/+47.4/-4.0 -- the 4K film row is
            // 1.3/1.2 down there, which is where the upgrade path is if this
            // is picked up again.
            let second = info.ref1.filter(|_| motion_won == 0).and_then(|r| {
                refs[r as usize].map(|g| {
                    (
                        info.mv1,
                        (g.y.as_slice(), g.width, luma.true_width, luma.true_height),
                    )
                })
            });
            let (levels, tx_depth, dcost, luma_tx_types) =
                if motion_won != 0 || (info.ref1.is_some() && second.is_none()) {
                    luma.commit(x, y, side, &luma_new);
                    (luma_new.levels.clone(), 0, 0.0, Vec::new())
                } else {
                    commit_inter_luma(
                        luma,
                        (x, y),
                        side,
                        info.mv,
                        (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
                        search,
                        &luma_new,
                        skip_new,
                        second,
                        fctx,
                    )
                };
            // lane-txrd: chroma follows the luma type the search just chose.
            let [u_new, v_new] = match luma_tx_types.first().copied() {
                Some(t) => recode_inter_chroma(
                    chroma,
                    (x, y),
                    side,
                    info.mv,
                    [
                        (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3),
                        (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3),
                    ],
                    info.ref1.filter(|_| motion_won == 0).and_then(|r| {
                        refs[r as usize].map(|g| {
                            (
                                info.mv1,
                                [
                                    (
                                        g.u.as_slice(),
                                        g.width / 2,
                                        chroma[0].true_width,
                                        chroma[0].true_height,
                                    ),
                                    (
                                        g.v.as_slice(),
                                        g.width / 2,
                                        chroma[1].true_width,
                                        chroma[1].true_height,
                                    ),
                                ],
                            )
                        })
                    }),
                    search,
                    chroma_set,
                    t,
                    fctx,
                )
                .unwrap_or([u_new, v_new]),
                None => [u_new, v_new],
            };
            chroma[0].commit(x / 2, y / 2, side / 2, &u_new);
            chroma[1].commit(x / 2, y / 2, side / 2, &v_new);
            return (
                BlockCoeffs {
                    angle_delta_y: 0,
                    cfl_alphas: None,
                    filter_intra: None,
                    luma: coeffs(&levels, side),
                    u: coeffs(&u_new.levels, side / 2),
                    v: coeffs(&v_new.levels, side / 2),
                    mode: DC_PRED,
                    uv_mode: DC_PRED,
                    skip: skip_new,
                    eight: None,
                    dv: None,
                    palette: None,
                    palette_uv: None,
                    tx_depth,
                    luma_tx_types,
                    inter: Some(info),
                    motion_mode: motion_won,
                },
                cost + dcost,
            );
        }

    census_add(8, 1);
    if skip {
        census_add(10, 1);
    }
    if inter_cost >= intra_cost {
        census_add(9, 1);
    }
    if inter_cost < intra_cost {
        let (levels, tx_depth, dcost, luma_tx_types) = commit_inter_luma(
            luma,
            (x, y),
            side,
            mv,
            (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
            search,
            &luma_trial,
            skip,
            None,
            fctx,
        );
        // lane-txrd: chroma follows the luma type (this tail's block is
        // single-reference `NEARESTMV`, so no compound second half).
        let [u, v] = match luma_tx_types.first().copied() {
            Some(t) => recode_inter_chroma(
                chroma,
                (x, y),
                side,
                mv,
                [
                    (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3),
                    (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3),
                ],
                None,
                search,
                chroma_set,
                t,
                fctx,
            )
            .unwrap_or([u, v]),
            None => [u, v],
        };
        chroma[0].commit(x / 2, y / 2, side / 2, &u);
        chroma[1].commit(x / 2, y / 2, side / 2, &v);
        (
            BlockCoeffs {
                luma_tx_types,
                angle_delta_y: 0,
                cfl_alphas: None,
                filter_intra: None,
                luma: coeffs(&levels, side),
                u: coeffs(&u.levels, side / 2),
                v: coeffs(&v.levels, side / 2),
                mode: DC_PRED,
                uv_mode: DC_PRED,
                skip,
                eight: None,
                dv: None,
                palette: None,
                palette_uv: None,
                tx_depth,
                motion_mode: 0,
                inter: Some(InterInfo {
                    ref1: None,
                    mv1: (0, 0),
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::NearestMv,
                    mv,
                    ref_mv_idx: 0,
                }),
            },
            inter_cost + dcost,
        )
    } else {
        (intra_block, intra_cost)
    }
}
