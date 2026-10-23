use super::*;

/// Publishes one coded inter-frame block into the `mi` grid the next block's
/// MV stack reads, over the `size` x `size` 4x4 units it covers. An intra
/// block casts no vote but is still a coded cell (see `code_square_inter`'s
/// note on `processed_rows`/`processed_cols`).
pub(super) fn record_mi(
    grid: &mut MiGrid,
    mi_row: usize,
    mi_col: usize,
    size: u8,
    inter: Option<InterInfo>,
    // lane-ctx: the block's own `skip`, so the next block's MV stack can hand
    // the RD pricer the two cells the writer's `skip_ctx` sums.
    skip: bool,
) {
    let info = match inter {
        Some(info) => MiInfo {
            is_inter: true,
            ref_frame: info.ref_frame,
            // A COMPOUND block votes with BOTH of its references and both of
            // its vectors, exactly as the tile writer publishes it: recording
            // a compound block as a single-reference one here left the
            // encoder's own grid disagreeing with the writer's from the first
            // compound block on, so every later stack it searched against was
            // not the stack the decoder derives.
            ref_frame1: info.ref1.unwrap_or(NO_REF1),
            mv1: mv16(info.mv1),
            mv: mv16(info.mv),
            // decode.rs' own `is_new_mv`: `NEWMV` on the single-reference
            // side, compound modes 2/3/7 on the other.
            is_new_mv: matches!(
                info.mode,
                InterMode::NewMv
                    | InterMode::NewNewMv
                    | InterMode::NearestNewMv
                    | InterMode::NewNearestMv
            ),
            size,
            size_h: size,
            is_global_mv0: false,
            is_global_mv1: false,
        },
        None => MiInfo {
            is_inter: false,
            ref_frame: -1,
            ref_frame1: NO_REF1,
            mv1: (0, 0),
            mv: (0, 0),
            is_new_mv: false,
            size,
            size_h: size,
            is_global_mv0: false,
            is_global_mv1: false,
        },
    };
    for dr in 0..usize::from(size) {
        for dc in 0..usize::from(size) {
            grid.set(mi_row + dr, mi_col + dc, info);
            grid.set_skip(mi_row + dr, mi_col + dc, skip);
        }
    }
}

/// lane-b128hv: [`record_mi`] for a RECTANGULAR block -- the mi stamp the
/// writer's own `write_inter_block_128_rect` makes, so the second half of a
/// HORZ/VERT 128 root searches against the stack the writer will rebuild.
pub(super) fn record_mi_rect(
    grid: &mut MiGrid,
    mi_row: usize,
    mi_col: usize,
    (w_mi, h_mi): (usize, usize),
    inter: Option<InterInfo>,
    skip: bool,
) {
    record_mi(grid, mi_row, mi_col, w_mi.min(h_mi) as u8, inter, skip);
    // `record_mi` stamped the square corner; widen it to the true footprint,
    // with the block's own (rect) size fields.
    let mut info = grid.get_frame(mi_row, mi_col).expect("just recorded");
    info.size = w_mi as u8;
    info.size_h = h_mi as u8;
    for dr in 0..h_mi {
        for dc in 0..w_mi {
            grid.set(mi_row + dr, mi_col + dc, info);
            grid.set_skip(mi_row + dr, mi_col + dc, skip);
        }
    }
}

/// The reconstructed samples one 32x32 square covers in all three planes, so
/// that a partition trial can be undone.
/// One square of a plane, row-major, the inverse of [`Plane::restore`].
///
/// Same bytes as the `flat_map(|row| ..to_vec()).collect()` this replaces,
/// which allocated and freed a `Vec` per row of every block it copied --
/// `Plane::snapshot` runs once per partition trial.
pub(super) fn rows_of(plane: &[u8], width: usize, x: usize, y: usize, side: usize) -> Vec<u8> {
    let mut out = vec![0u8; side * side];
    for row in 0..side {
        out[row * side..][..side].copy_from_slice(&plane[(y + row) * width + x..][..side]);
    }
    out
}

pub(super) fn snapshot(
    luma: &Plane,
    chroma: &[Plane; 2],
    (x, y): (usize, usize),
    side: usize,
) -> [(Vec<u8>, Vec<CoefCtx>); 3] {
    [
        luma.snapshot(x, y, side),
        chroma[0].snapshot(x / 2, y / 2, side / 2),
        chroma[1].snapshot(x / 2, y / 2, side / 2),
    ]
}

/// Puts such a snapshot back.
pub(super) fn restore(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    side: usize,
    saved: &[(Vec<u8>, Vec<CoefCtx>); 3],
) {
    luma.restore(x, y, side, &saved[0]);
    chroma[0].restore(x / 2, y / 2, side / 2, &saved[1]);
    chroma[1].restore(x / 2, y / 2, side / 2, &saved[2]);
}

/// Keeps the DC candidate plus the cheapest `top_k` (or fewer, once DC is set
/// aside) of `scored` (as [`Plane::intra_scores`] built it), in `modes`' own
/// order -- the same DC-always-kept, cheapest-by-SAD rule [`Plane::search_block`]
/// applies, so [`search_inter_block`]'s intra-candidate loop prunes by one
/// rule shared with the key-frame path, not a second one that could diverge.
pub(super) fn prune_by_sad(modes: &[u8], mut scored: Vec<(f64, u8)>, top_k: usize) -> Vec<u8> {
    let dc_pos = scored.iter().position(|&(_, mode)| mode == DC_PRED);
    let dc_entry = dc_pos.map(|i| scored.remove(i));
    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("scores are finite"));
    scored.truncate(if dc_entry.is_some() {
        top_k.saturating_sub(1)
    } else {
        top_k
    });
    if let Some(dc_entry) = dc_entry {
        scored.push(dc_entry);
    }
    let keep: Vec<u8> = scored.into_iter().map(|(_, mode)| mode).collect();
    modes.iter().copied().filter(|m| keep.contains(m)).collect()
}

pub(super) fn mode_bits(above_mode: u8, left_mode: u8) -> [f64; 13] {
    let luma = &cdf::KF_Y_MODE[INTRA_MODE_CTX[usize::from(above_mode)]]
        [INTRA_MODE_CTX[usize::from(left_mode)]];
    std::array::from_fn(|mode| {
        let angle = if (usize::from(V_PRED)..=usize::from(D67_PRED)).contains(&mode) {
            symbol_bits(&cdf::ANGLE_DELTA[mode - usize::from(V_PRED)], 3)
        } else {
            0.0
        };
        symbol_bits(luma, mode) + angle + symbol_bits(&cdf::UV_MODE_CFL[mode], usize::from(DC_PRED))
    })
}

/// What the luma mode search picks between, and under what terms.
#[derive(Clone, Copy)]
pub(super) struct Search<'a> {
    /// This frame's `allow_screen_content_tools` ([`screen_content`]): the
    /// luma search then offers a palette candidate beside the intra modes.
    pub(super) screen: bool,
    pub(super) base_q_idx: u8,
    pub(super) deadzone: f64,
    pub(super) lambda: f64,
    pub(super) modes: &'a [u8],
    /// How many of `modes` the full RD trial (forward transform, quantize,
    /// reconstruct, `coeff_bits`) actually runs on, ranked by a cheap SAD
    /// pre-pass on the prediction residual alone plus DC (always kept, since
    /// libaom's own intra pruning -- `av1/encoder/speed_features.c`'s
    /// `intra_pruning_with_hog` -- never prunes it away). `None` runs every
    /// mode through full RD, unchanged from before this lever.
    pub(super) top_k: Option<usize>,
    /// lane-arfmode: the top ARF's offer set ([`crate::speed::ARFMODE`])
    /// -- `false` cuts the named candidate group from the whole-32 search
    /// ([`search_inter_block`]), the 16x16/8x8 leaf search
    /// ([`code_square_inter`]) and the 8x8 split offer, at the top ARF
    /// ([`DqLevel::TopArf`] frames only). `true` everywhere else, which is
    /// the pre-lane behaviour: the mid and quarter hidden frames, the shown
    /// leaves and the flat path keep intra and compound, and the
    /// straddling-edge 8x8s are only mode-gated, never partition-gated.
    /// The probe's correction: with the leaf gate ALONE the film A stream
    /// was byte-identical to the control -- the ARF's intra area lives at
    /// the whole-32 level, so the intra arm must cut it there too.
    pub(super) arf_leaf_intra: bool,
    pub(super) arf_leaf8: bool,
    pub(super) arf_leaf_compound: bool,
}

/// [`Search`]'s ARF fields' source: [`crate::speed::ARFMODE`], or what
/// `EC_AV1_ARFMODE` names on one build (a missing or broken value keeps the
/// shipped table -- 0, bit-exact with the pre-lane encoder).
pub(super) fn arfmode() -> u8 {
    match ARFMODE_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        u8::MAX => {}
        n => return n,
    }
    static ENV: std::sync::LazyLock<Option<u8>> = std::sync::LazyLock::new(|| {
        crate::envflags::var("EC_AV1_ARFMODE")
            .ok()
            .and_then(|v| v.parse().ok())
    });
    ENV.unwrap_or_else(|| crate::speed::at(&crate::speed::ARFMODE))
}

/// [`ARFMODE_OVERRIDE`]: `u8::MAX` clears it.
pub(super) static ARFMODE_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// Sets [`ARFMODE_OVERRIDE`]; `None` clears it. Test-only: the env is a
/// process constant ([`crate::envflags`]), so an in-process witness needs
/// this to arm the lever.
#[allow(dead_code)] // set only from the `#[cfg(test)]` witness
pub(crate) fn set_arfmode(n: Option<u8>) {
    ARFMODE_OVERRIDE.store(n.unwrap_or(u8::MAX), std::sync::atomic::Ordering::Relaxed);
}

/// How often each armed [`crate::speed::ARFMODE`] arm actually fired since
/// the last take (class `gate-blind-to-feature`): 0 = a top-ARF leaf coded
/// with its intra candidate skipped, 1 = a top-ARF 16x16 leaf whose 8x8
/// split the search would have offered and the arm refused, 2 = a top-ARF
/// leaf whose compound set was dropped. A green gate over counts of zero
/// proves nothing about the lever.
pub(super) static ARFMODE_HITS: [std::sync::atomic::AtomicUsize; 3] = [
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
    std::sync::atomic::AtomicUsize::new(0),
];

/// Records one such hit.
pub(super) fn arfmode_hit(which: usize) {
    ARFMODE_HITS[which].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// [`arfmode_hit`]`(1)` and "no": the `||` arm of the top ARF's 8x8-offer
/// condition, so the counter only runs when every other prune let the split
/// through and the arm alone refuses it.
pub(super) fn arfmode_hit_and_no() -> bool {
    arfmode_hit(1);
    false
}

/// Takes (and clears) the counts. `pub` for `examples/enc_probe`, so a
/// probe point can print a non-zero engagement count under the env knob
/// itself -- the counter the gate BD numbers are read against.
pub fn take_arfmode_hits() -> [usize; 3] {
    [0, 1, 2].map(|i| ARFMODE_HITS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

pub(super) fn prune_top_k() -> Option<usize> {
    #[cfg(test)]
    {
        if let Some(k) = TEST_TOP_K_OVERRIDE.with(|cell| cell.get()) {
            return k;
        }
    }
    let swept = std::env::var("EC_AV1_PRUNE_K")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());
    match swept {
        Some(k) if cfg!(test) => Some(k),
        _ => crate::speed::at(&crate::speed::PRUNE_K),
    }
}

// A per-thread override [`prune_top_k`] checks first, so one test can sweep
// `top_k` in-process without mutating the environment (forbidden here --
// `#![forbid(unsafe_code)]` -- and racy across parallel tests besides).
#[cfg(test)]
thread_local! {
    static TEST_TOP_K_OVERRIDE: std::cell::Cell<Option<Option<usize>>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn set_test_top_k_override(value: Option<usize>) {
    TEST_TOP_K_OVERRIDE.with(|cell| cell.set(Some(value)));
}

/// The default: unpruned, thirteen full trials per block, same as before this
/// lever. Set from the sweep in the lane report if the quality gate clears it.
pub(crate) const PRUNE_TOP_K: Option<usize> = None;

/// [`Search::top_k`] for [`search_inter_block`]'s intra-candidate loop --
/// kept apart from [`prune_top_k`]/[`PRUNE_TOP_K`] because the two paths'
/// quality gates were measured separately and may land on different
/// defaults; `EC_AV1_PRUNE_K_INTER` sweeps it without touching
/// [`EC_AV1_PRUNE_K`] or the key-frame path.
pub(super) fn prune_top_k_inter() -> Option<usize> {
    #[cfg(test)]
    {
        if let Some(k) = TEST_TOP_K_OVERRIDE_INTER.with(|cell| cell.get()) {
            return k;
        }
    }
    let swept = std::env::var("EC_AV1_PRUNE_K_INTER")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());
    match swept {
        Some(k) if cfg!(test) => Some(k),
        _ => crate::speed::at(&crate::speed::PRUNE_K_INTER),
    }
}

// A per-thread override [`prune_top_k_inter`] checks first, same reason as
// [`TEST_TOP_K_OVERRIDE`].
#[cfg(test)]
thread_local! {
    static TEST_TOP_K_OVERRIDE_INTER: std::cell::Cell<Option<Option<usize>>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn set_test_top_k_override_inter(value: Option<usize>) {
    TEST_TOP_K_OVERRIDE_INTER.with(|cell| cell.set(Some(value)));
}

/// The inter path's default `top_k`: `Some(3)`, from `prune_k_quality_sweep_inter`
/// (three real clips, q 60 and 150) -- every `K` in {3, 4, 6} cleared the
/// same rule the key-frame lever uses (every PSNR delta <0.05dB, bytes
/// within +1%; worst seen was -0.011dB / +0.147% bytes), so the cheapest,
/// `K=3`, ships as default. Unlike the key-frame path (still `None`), an
/// inter block's intra candidates rarely win against NEWMV/NEARESTMV, so
/// pruning them costs less quality here.
pub(crate) const INTER_PRUNE_TOP_K: Option<usize> = Some(3);

// Per-thread nanosecond counters `stage_timing_breakdown_inter` (and
// [`crate::mc::predict`], through [`stage_add`]) accumulate into, so an
// inter frame's cost can be attributed to a bucket without changing what
// the search actually does. Index: 0 = whole-call time inside
// [`motion::search`] (includes bucket 1's time, run from inside it), 1 =
// [`crate::mc::predict`] (every call, from the motion search's own
// candidates and from a committed inter block's final prediction alike),
// 2 = `forward_and_quantize` + `dequant_and_inverse`, 3 = `coeff_bits`.
#[cfg(test)]
thread_local! {
    static STAGE_NS: [std::cell::Cell<u64>; 4] = const {
        [
            std::cell::Cell::new(0),
            std::cell::Cell::new(0),
            std::cell::Cell::new(0),
            std::cell::Cell::new(0),
        ]
    };
}

/// lane-av1cap: the trial timers are `#[cfg(test)]`, and the BD gate and
/// every encoder test are `cfg(test)` builds -- so two `Instant::now()` ran
/// per transform trial and per `mc::predict` call whether or not anyone was
/// reading the breakdown (the vdso clock was 1.7% of the encoder's profile).
/// The clock is now read only when [`crate::par::stage_times`] is on, which
/// is what prints the breakdown in the first place.
#[cfg(test)]
pub(crate) fn stage_start() -> Option<std::time::Instant> {
    crate::par::stage_times().then(std::time::Instant::now)
}

#[cfg(test)]
pub(crate) fn stage_since(bucket: usize, t: Option<std::time::Instant>) {
    if let Some(t) = t {
        stage_add(bucket, t.elapsed());
    }
}

#[cfg(test)]
pub(crate) fn stage_add(bucket: usize, dur: std::time::Duration) {
    STAGE_NS.with(|c| c[bucket].set(c[bucket].get() + dur.as_nanos() as u64));
}

#[cfg(test)]
pub(super) fn stage_reset() {
    crate::par::set_stage_times(true);
    STAGE_NS.with(|c| c.iter().for_each(|cell| cell.set(0)));
}

#[cfg(test)]
pub(super) fn stage_read() -> [std::time::Duration; 4] {
    crate::par::set_stage_times(false);
    STAGE_NS.with(|c| {
        c.each_ref()
            .map(|cell| std::time::Duration::from_nanos(cell.get()))
    })
}

/// One mode's coding of one block, before it is committed.
#[derive(Clone)]
pub(super) struct Trial {
    pub(super) levels: Vec<i32>,
    pub(super) reconstruction: Vec<u8>,
    pub(super) sse: f64,
    pub(super) bits: f64,
}

/// The top-left `to` x `to` corner of a `from`-sided level grid -- the part
/// of a 64-point transform that is actually coded (everything outside it is
/// zeroed by [`crate::transform::quantize`] already, so this only reshapes).
pub(super) fn coded_corner(levels: &[i32], from: usize, to: usize) -> Vec<i32> {
    let mut out = vec![0i32; to * to];
    for row in 0..to {
        out[row * to..][..to].copy_from_slice(&levels[row * from..][..to]);
    }
    out
}

/// The non-zero levels of a block, as the tile writer takes them.
pub(super) fn coeffs(levels: &[i32], side: usize) -> Vec<Coeff> {
    levels
        .iter()
        .enumerate()
        .filter(|&(_, &level)| level != 0)
        .map(|(i, &level)| Coeff {
            row: (i / side) as u8,
            col: (i % side) as u8,
            level,
        })
        .collect()
}

thread_local! {
    /// The tile grid the next frame is coded with, as `(TileColsLog2,
    /// TileRowsLog2)` -- armed by [`crate::encoder::Av1Encoder`] from its
    /// [`crate::encoder::EncoderConfig`], the same way `crate::tile`'s
    /// `arm_cdef_idx`/`arm_lr`/`arm_sign_bias` arm the writers. `(0, 0)` is
    /// one tile per frame, what every caller that never arms anything gets
    /// and what every stream this crate wrote before tiles existed carries.
    static TILE_LOG2: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, 0)) };
}

/// Arms this thread's next frame encodes with a tile grid (log2 counts).
pub(crate) fn arm_tiles(cols_log2: u32, rows_log2: u32) {
    TILE_LOG2.with(|c| c.set((cols_log2, rows_log2)));
}

pub(super) fn armed_tiles() -> (u32, u32) {
    let armed = TILE_LOG2.with(std::cell::Cell::get);
    if armed != (0, 0) {
        return armed;
    }
    // `EC_AV1_TILES=<cols_log2>[:<rows_log2>]` in a TEST build, so the BD gate
    // (which codes through `encode_sequence`, not the facade) can measure what
    // a tile grid costs without a knob of its own -- same shape as
    // `EC_AV1_LR`/`EC_AV1_PYRAMID`.
    match std::env::var("EC_AV1_TILES").ok() {
        Some(v) if cfg!(test) => {
            let mut f = v.split(':');
            let cols = f.next().and_then(|c| c.parse().ok()).unwrap_or(0);
            let rows = f.next().and_then(|r| r.parse().ok()).unwrap_or(0);
            (cols, rows)
        }
        _ => armed,
    }
}

/// Codes a frame's `tiles` tiles, each entirely on its own (see
/// [`crate::tile::TileRect`]), across [`crate::par::tile_threads`] workers of
/// the crate's own pool, and hands back their payloads in tile order plus
/// whatever `code` returned for tile `store` -- the CDF state
/// `context_update_tile_id` names. The result cannot depend on the thread
/// count: each job writes one tile from the frame's own starting tables into
/// its own slot.
pub(super) fn write_tiles<F>(
    tiles: usize,
    store: usize,
    code: F,
) -> Result<(Vec<Vec<u8>>, Option<crate::cdf_state::Cdfs>)>
where
    F: Fn(usize) -> Result<(Vec<u8>, Option<crate::cdf_state::Cdfs>)> + Sync + Send,
{
    let _t = crate::par::timer(crate::par::S_TILE_WRITE);
    type Slot = Option<Result<(Vec<u8>, Option<crate::cdf_state::Cdfs>)>>;
    let threads = crate::par::tile_threads().min(tiles);
    let mut slots: Vec<Slot> = Vec::new();
    if threads <= 1 {
        for index in 0..tiles {
            slots.push(Some(code(index)));
        }
    } else {
        let done: std::sync::Mutex<Vec<Slot>> =
            std::sync::Mutex::new((0..tiles).map(|_| None).collect());
        {
            crate::par::scope("ec-av1-tile", |batch| {
                for (from, to) in crate::par::bands(tiles, threads) {
                    let done = &done;
                    let code = &code;
                    batch.submit(move || {
                        for index in from..to {
                            let coded = code(index);
                            done.lock().expect("tile slots")[index] = Some(coded);
                        }
                    });
                }
            });
        }
        slots = done.into_inner().expect("tile slots");
    }
    let mut out = Vec::with_capacity(tiles);
    let mut stored = None;
    for (index, slot) in slots.into_iter().enumerate() {
        let (bytes, cdfs) = slot.expect("every tile was coded")?;
        if index == store {
            stored = cdfs;
        }
        out.push(bytes);
    }
    Ok((out, stored))
}

/// Runs `job(tile_index, ctx)` once per tile, on `EC_AV1_TILE_THREADS`
/// workers, and returns the results in tile order. The counterpart of
/// [`write_tiles`] for the RD/motion SEARCH: a tile's job owns every piece of
/// state the search mutates (its own reconstruction planes, its own `MiGrid`,
/// its own neighbour-mode bands and its own [`crate::decode::FrameCtx`]), and
/// availability is already tile-relative (`clip_planes_to_tile`,
/// `MiGrid::set_tile_bounds`), so what a tile decides cannot depend on which
/// other tiles have run -- gated by
/// `encoder::tests::tile_bytes_do_not_depend_on_the_thread_count`, which now
/// covers the search as well as the write.
///
/// The `FrameCtx` copy is made on the calling thread (`&FrameCtx` is `!Sync`
/// by design) and moved into the worker, exactly as the decoder's filter bands
/// do it through [`crate::par::scope`].
pub(super) fn search_tiles<T, F>(tiles: usize, fctx: &crate::decode::FrameCtx, job: F) -> Result<Vec<T>>
where
    T: Send,
    F: Fn(usize, &crate::decode::FrameCtx) -> Result<T> + Sync,
{
    let _t = crate::par::timer(crate::par::S_TILE_SEARCH);
    let threads = crate::par::tile_threads().min(tiles);
    if threads <= 1 {
        return (0..tiles).map(|index| job(index, fctx)).collect();
    }
    type Slot<T> = Option<Result<T>>;
    let done: std::sync::Mutex<Vec<Slot<T>>> =
        std::sync::Mutex::new((0..tiles).map(|_| None).collect());
    {
        crate::par::scope("ec-av1-tile-search", |batch| {
            for (from, to) in crate::par::bands(tiles, threads) {
                let done = &done;
                let job = &job;
                let ctx = crate::decode::filter_ctx_copy(fctx);
                batch.submit(move || {
                    for index in from..to {
                        let searched = job(index, &ctx);
                        done.lock().expect("tile slots")[index] = Some(searched);
                    }
                });
            }
        });
    }
    done.into_inner()
        .expect("tile slots")
        .into_iter()
        .map(|slot| slot.expect("every tile was searched"))
        .collect()
}

/// One tile job's own copy of a plane: the frame's source, a blank
/// reconstruction of the frame's size, and the frame's own bounds -- narrowed
/// to the tile by `clip_planes_to_tile` right after. Blank rather than a copy
/// of the frame's reconstruction because a tile's prediction never reads a
/// sample outside its own rectangle, and every sample inside it is written
/// before it is read.
pub(super) fn fresh_plane(
    source: &[u8],
    width: usize,
    height: usize,
    true_width: usize,
    true_height: usize,
) -> Plane<'_> {
    Plane {
        source,
        reconstruction: vec![128; source.len()],
        width,
        height,
        true_width,
        true_height,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width,
        tile_y1: height,
        ctx: CoefCtxMap::default(),
    }
}

/// One tile job's reconstructed rectangles, handed back to the calling thread
/// (which copies them into the frame planes in tile order).
pub(super) struct TilePixels {
    pub(super) luma_rect: (usize, usize, usize, usize),
    pub(super) chroma_rect: (usize, usize, usize, usize),
    pub(super) luma: Vec<u8>,
    pub(super) chroma: [Vec<u8>; 2],
}

/// The rectangle `[x0, x1) x [y0, y1)` of a plane with `stride`, copied out as
/// its rows back to back.
pub(super) fn take_rect(plane: &[u8], stride: usize, (x0, y0, x1, y1): (usize, usize, usize, usize)) -> Vec<u8> {
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        out.extend_from_slice(&plane[y * stride + x0..y * stride + x1]);
    }
    out
}

/// The inverse of [`take_rect`]: writes `rows` (as `take_rect` produced them)
/// back into the rectangle of `plane`.
pub(super) fn put_rect(plane: &mut [u8], stride: usize, (x0, y0, x1, y1): (usize, usize, usize, usize), rows: &[u8]) {
    let w = x1 - x0;
    for (row, y) in (y0..y1).enumerate() {
        plane[y * stride + x0..y * stride + x1].copy_from_slice(&rows[row * w..(row + 1) * w]);
    }
}

/// The frame header's `tile_info` (spec 5.9.15) for `layout`:
/// `context_update_tile_id` names the largest tile, whose end-of-tile CDF
/// tables the frame stores (spec 7.20), which is what
/// [`crate::tile::TileLayout::largest_tile`] picks and what the tile loops
/// below publish.
pub(super) fn tile_info_of(layout: &crate::tile::TileLayout, tile_size_bytes: u32) -> ec_av1_syntax::TileInfo {
    ec_av1_syntax::TileInfo {
        uniform_spacing: true,
        cols: layout.cols(),
        rows: layout.rows(),
        cols_log2: layout.cols_log2,
        rows_log2: layout.rows_log2,
        mi_col_starts: layout.mi_col_starts(),
        mi_row_starts: layout.mi_row_starts(),
        context_update_tile_id: layout.largest_tile() as u32,
        tile_size_bytes,
    }
}

/// Clips the three planes' intra edge reads to `rect`, the tile the
/// superblock about to be searched belongs to (the encoder side of the
/// decoder's `PlaneBuf::set_tile_origin`). The search itself stays in frame
/// raster order -- which is a linear extension of every tile's own coding
/// order, so a block still only ever predicts from samples already
/// reconstructed -- and only availability is tile-relative.
pub(super) fn clip_planes_to_tile(
    luma: &mut Plane<'_>,
    chroma: &mut [Plane<'_>; 2],
    rect: crate::tile::TileRect,
) {
    let (x0, y0) = (rect.mi_col0 as usize * 4, rect.mi_row0 as usize * 4);
    let (x1, y1) = (rect.mi_col1 as usize * 4, rect.mi_row1 as usize * 4);
    luma.set_tile(x0, y0, x1, y1);
    for plane in chroma.iter_mut() {
        plane.set_tile(x0 / 2, y0 / 2, x1.div_ceil(2), y1.div_ceil(2));
    }
}

/// Encodes one picture as a key frame.
///
/// `base_q_idx` is the frame's quantizer index (0..=255); the tile writer
/// picks its coefficient CDFs from one of four q contexts by that index.
/// `deadzone` is the quantizer's rounding offset: 0.5 rounds to nearest, and
/// smaller values trade fidelity for rate.
///
/// The picture may be any even width and height: the encoder pads it by edge
/// replication to a whole number of 32x32 blocks internally, and the frame it
/// writes crops back to the picture's own size (`render_width`/
/// `render_height`), so [`Encoded::reconstruction`] is always the picture's
/// own size, never the padded one.
///
/// # Errors
/// Returns an error when the picture's width or height is zero or odd, or
/// when its planes are not 4:2:0 of that size.
/// Codes one picture as a whole key frame, at `base_q_idx`, and returns its
/// stream. The per-frame decode state this needs lives for exactly this call.
///
/// # Errors
/// Returns an error under the same conditions
/// [`encode_key_frame_with_modes`] does.
pub fn encode_key_frame(picture: &Picture, base_q_idx: u8, deadzone: f64) -> Result<Encoded> {
    encode_key_frame_with_ctx(
        picture,
        base_q_idx,
        deadzone,
        &crate::decode::FrameCtx::for_encoder(),
    )
}

/// [`encode_key_frame`] with the per-block intra modes given rather than
/// searched.
///
/// # Errors
/// Returns an error when `picture`'s dimensions are odd or zero, when `modes`
/// is empty, or when a block's coefficients do not fit the coded frame.
pub fn encode_key_frame_with_modes(
    picture: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    modes: &[u8],
) -> Result<Encoded> {
    encode_key_frame_with_modes_with_ctx(
        picture,
        base_q_idx,
        deadzone,
        modes,
        &crate::decode::FrameCtx::for_encoder(),
    )
}

/// Codes `pictures` as one key frame followed by inter frames.
///
/// Camera material is coded under [`crate::encoder::Pyramid::default`] since
/// lane-av1pyrdef -- mini-GOPs of 4 with a hidden `ALTREF` and a
/// `show_existing_frame` header, so `stream` is in CODING order while
/// `frames` stays in display order (`EncodedSequence::coding_order` is the
/// map). Screen content codes flat, decided once per sequence by the same
/// detector the streaming facade's content gate uses, and `EC_AV1_PYRAMID=0`
/// puts every stream back on the flat path for an A/B.
///
/// # Errors
/// Returns an error under the same conditions [`encode_key_frame`] and the
/// inter frame path do.
pub fn encode_sequence(
    pictures: &[Picture],
    base_q_idx: u8,
    deadzone: f64,
) -> Result<EncodedSequence> {
    encode_sequence_with_ctx(
        pictures,
        base_q_idx,
        deadzone,
        &crate::decode::FrameCtx::for_encoder(),
    )
}

/// Codes one padded picture as a key frame whose HEADER declares
/// `frame_width` x `frame_height` -- a frame size the picture's own
/// `width`/`height` need not be, and in particular need not be even.
///
/// [`encode_key_frame`] derives the declared size from the picture and
/// refuses an odd one, which is a property of a 4:2:0 SOURCE (an odd luma
/// column has no chroma sample of its own to be fed), not of the format: a
/// 65-wide 4:2:0 frame is a legal AV1 frame, with
/// `ROUND_POWER_OF_TWO(65, 1) = 33` chroma columns (libaom
/// `av1_common_plane_width` / `ROUND_POWER_OF_TWO`, `av1/common/
/// av1_common_int.h`), and aomenc never emits one only because it rounds the
/// coded size down to even before it looks at the picture
/// (`av1enc_set_frame_size`). This is the entry that can produce one, and it
/// is what the odd-luma coverage fixture is built with: the caller hands
/// over a picture already padded to the block grid and names the size to
/// declare, so the block walk codes exactly the `mi_cols`/`mi_rows` a
/// decoder derives from the declared size (spec `compute_image_size`).
///
/// # Errors
/// Returns an error when `picture` is not a whole number of 32x32 blocks
/// ([`Picture::check`]'s own rule), when either declared size is zero or
/// larger than the padded picture's, or when the mode search fails -- the
/// same conditions [`encode_key_frame`] reports for a malformed picture.
pub fn encode_key_frame_at_size(
    picture: &Picture,
    frame_width: usize,
    frame_height: usize,
    base_q_idx: u8,
    deadzone: f64,
) -> Result<Encoded> {
    picture.check()?;
    if frame_width == 0
        || frame_height == 0
        || frame_width > picture.width
        || frame_height > picture.height
    {
        return Err(Error::unsupported(
            "AV1 encode",
            "the declared frame size must be nonzero and fit inside the padded picture",
        ));
    }
    let encoded = encode_key_frame_inner(
        picture,
        base_q_idx,
        deadzone,
        &KEY_FRAME_MODES,
        split_blocks(),
        (frame_width, frame_height),
        unspecified_color_config(),
        &crate::decode::FrameCtx::for_encoder(),
    )?;
    Ok(crop_encoded(&encoded, frame_width, frame_height))
}

pub(crate) fn encode_key_frame_with_ctx(
    picture: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    fctx: &crate::decode::FrameCtx,
) -> Result<Encoded> {
    encode_key_frame_with_modes_with_ctx(picture, base_q_idx, deadzone, &KEY_FRAME_MODES, fctx)
}

/// Encodes one picture as a key frame, choosing each block's luma mode from
/// `modes` alone.
///
/// This is what an ablation measures against: `&[DC_PRED]` is the encoder
/// before the mode search, [`crate::intra::NON_DIRECTIONAL`] is it before the
/// directional modes, and [`KEY_FRAME_MODES`] is what [`encode_key_frame`]
/// uses.
///
/// # Errors
/// The same as [`encode_key_frame`], and additionally when `modes` is empty or
/// names a mode [`crate::intra::predict`] does not predict.
pub(crate) fn encode_key_frame_with_modes_with_ctx(
    picture: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    modes: &[u8],
    fctx: &crate::decode::FrameCtx,
) -> Result<Encoded> {
    picture.check_even()?;
    let padded = picture.padded_to(BLOCK);
    let encoded = encode_key_frame_inner(
        &padded,
        base_q_idx,
        deadzone,
        modes,
        split_blocks(),
        (picture.width, picture.height),
        unspecified_color_config(),
        fctx,
    )?;
    Ok(crop_encoded(&encoded, picture.width, picture.height))
}

/// [`encode_key_frame_with_modes`] with the partition decision forced, which is
/// what the sweep that sets [`SPLIT_BLOCKS`] measures both ways. `picture` is
/// already padded to a whole number of 32x32 blocks; `render` is the real
/// (pre-pad) size the frame header tells a decoder to crop back to.
// Spec-shaped signature; bundling the operands would obscure the spec mapping.
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_key_frame_inner(
    picture: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    modes: &[u8],
    split_blocks: bool,
    render: (usize, usize),
    color_config: ColorConfig,
    fctx: &crate::decode::FrameCtx,
) -> Result<Encoded> {
    // lane-refs: an intra frame runs no motion compensation, and the
    // context is shared across the sequence -- put the kernel back to the
    // default so nothing reads the previous inter frame's.
    fctx.interp_filter.set(mc::InterpFilterKind::Regular);
    picture.check()?;
    if modes.is_empty() {
        return Err(Error::unsupported(
            "AV1 encode",
            "a mode search needs at least one mode to choose from",
        ));
    }
    if let Some(bad) = modes.iter().find(|m| !KEY_FRAME_MODES.contains(m)) {
        return Err(Error::unsupported(
            "AV1 encode",
            format!("intra mode {bad} is not one the encoder predicts"),
        ));
    }
    // The header carries the frame's true (unpadded) size as `frame_width`/
    // `frame_height` -- what a decoder actually crops to -- with `mi_cols`/
    // `mi_rows` derived from that same true size (spec `compute_image_size`).
    // `render_width`/`render_height` come out equal to it too, so
    // `render_and_frame_size_different` is false and no render_size bits are
    // written, mirroring libaom/rav1e: the padded `picture` below is only the
    // internal coding surface, never what the header names.
    // Screen-content detection runs on the SOURCE luma, before any header is
    // built: it decides this sequence's `seq_force_screen_content_tools` (and
    // so the layout of every frame header after it, [`SEQ_SCREEN`]) as well as
    // this frame's own `allow_screen_content_tools`.
    let screen = screen_content(
        &picture.y.iter().map(|&v| v as u8).collect::<Vec<u8>>(),
        picture.width,
        (picture.width).min(render.0),
        (picture.height).min(render.1),
    );
    arm_seq_screen(screen);
    // A key frame's writer really does start from the defaults, and a search
    // worker may be carrying an inter frame's armed tables (lane-av1txbits):
    // disarm, or a key frame's bits would depend on which thread searched it.
    crate::tile::arm_pricing_cdfs(None, false);
    // spec 5.9.2 `allow_intrabc`, screen-detected key frames only: the bit
    // forces deblocking, CDEF and loop restoration OFF for this frame
    // (frame.rs:194/210/252/272), so it is only set when the source repeats
    // itself enough for the copied blocks to pay that back
    // ([`intrabc_worth_it`]).
    // Single-tile frames only: decode.rs keeps ONE frame-scoped intrabc mi
    // grid across every tile while this writer builds one per tile, so a
    // multi-tile allow_intrabc frame would predict its DVs off a different
    // grid than the decoder does. corner-cut, ceiling named: the upgrade
    // path is a frame-scoped grid shared by the tile writers.
    let (allow_intrabc, ibc_share) = if screen && intrabc_enabled() && armed_tiles() == (0, 0) {
        intrabc_worth_it(
            &picture.y.iter().map(|&v| v as u8).collect::<Vec<u8>>(),
            picture.width,
            picture.width.min(render.0),
            picture.height.min(render.1),
        )
    } else {
        (false, 0.0)
    };
    IBC_SHARE.with(|c| c.set(ibc_share));
    let (seq, mut header) = key_frame_headers_colour(render.0, render.1, base_q_idx, color_config)?;
    header.allow_screen_content_tools = screen;
    // lane-txi: the wider `tx_type` alphabets, gated by the same content
    // rule the type search itself stands behind ([`wide_tx_set`]).
    header.reduced_tx_set = !wide_tx_set(screen);
    header.allow_intrabc = allow_intrabc;
    header.render_width = render.0 as u32;
    header.render_height = render.1 as u32;
    // `TxMode::Select`: every block codes a `tx_depth` symbol and may split
    // its luma transform below its own side (`crate::tile::write_luma_select`).
    // Set here rather than in `key_frame_headers_colour` so the header
    // builders' other callers -- the hand-built tile writers, which code no
    // depth symbol -- keep the `TxMode::Largest` stream they are written for.
    let tx_select = tx_select();
    if tx_select {
        header.tx_mode = TxMode::Select;
    }
    let (tile_cols_log2, tile_rows_log2) = armed_tiles();

    // Prediction reads no further than this, in each plane's own units --
    // see `Plane::true_width`/`true_height`.
    let (true_width, true_height) = (header.mi_cols as usize * 4, header.mi_rows as usize * 4);
    // lane-palw: the encoder's own in-process reconstruct reads its palette
    // colour-index maps back through `decode::palette_onscreen`, which takes
    // the frame's mi dims off the `FrameCtx` -- a real decode gets them from
    // `set_segmentation`, which never runs on `FrameCtx::for_encoder`. Publish
    // them here so the reconstruct clamps a block cut by the frame edge
    // exactly as the writer now does.
    let frame_mi_dims = (header.mi_rows as usize, header.mi_cols as usize);
    fctx.seg_mi_dims.set(frame_mi_dims);
    // lane-hbd r4: `Picture.y/u/v` widened to `Vec<u16>` for the decoder's
    // sake (DPB reference slots, 10-bit output); the encoder stays 8-bit by
    // design (see `intra_predict_u8`'s doc comment), so narrow the source
    // once here.
    let picture_y8: Vec<u8> = picture.y.iter().map(|&v| v as u8).collect();
    let picture_u8: Vec<u8> = picture.u.iter().map(|&v| v as u8).collect();
    let picture_v8: Vec<u8> = picture.v.iter().map(|&v| v as u8).collect();
    let mut luma = Plane {
        source: &picture_y8,
        reconstruction: vec![128; picture_y8.len()],
        width: picture.width,
        height: picture.height,
        true_width,
        true_height,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: picture.width,
        tile_y1: picture.height,
        ctx: CoefCtxMap::default(),
    };
    let mut chroma = [
        Plane {
            source: &picture_u8,
            reconstruction: vec![128; picture_u8.len()],
            width: picture.width / 2,
            height: picture.height / 2,
            true_width: true_width / 2,
            true_height: true_height / 2,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: picture.width / 2,
            tile_y1: picture.height / 2,
            ctx: CoefCtxMap::default(),
        },
        Plane {
            source: &picture_v8,
            reconstruction: vec![128; picture_v8.len()],
            width: picture.width / 2,
            height: picture.height / 2,
            true_width: true_width / 2,
            true_height: true_height / 2,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: picture.width / 2,
            tile_y1: picture.height / 2,
            ctx: CoefCtxMap::default(),
        },
    ];

    // The search trades a bit for the squared error it saves, in the units the
    // reconstruction is measured in: one step of the quantizer, squared.
    let step = f64::from(ac_q(8, i32::from(base_q_idx))) / 8.0;
    let search = Search {
        base_q_idx,
        deadzone,
        lambda: lambda_scale() * key_lambda_factor() * step * step,
        modes,
        top_k: prune_top_k(),
        screen,
        arf_leaf_intra: true,
        arf_leaf8: true,
        arf_leaf_compound: true,
    };

    // The block grid this frame is coded over: [`crate::tile::block_grid`]'s
    // ceiling of the header's own (true-size-derived) `mi_cols`/`mi_rows`,
    // which may be fewer columns/rows than the padded picture has room for
    // -- a block whose origin sits past the true bound is not coded at all,
    // matching what a decoder derives from `frame_width`/`frame_height` and
    // so never visits either.
    let (cols, rows) = crate::tile::block_grid(header.mi_cols, header.mi_rows);
    let (cols, rows) = (cols as usize, rows as usize);
    // The luma mode of the block above and of the block to the left, which is
    // what picks the CDF the next block's mode is coded against -- the same
    // bookkeeping the tile writer keeps, so that the search is costing the
    // symbol the writer will actually write.
    // The bookkeeping is kept on the 16x16 grid the tile writer keeps it on,
    // because a 32x32 block may be split into four 16x16 ones.
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    // This frame's tile grid (one tile unless the facade armed more).
    let layout = crate::tile::TileLayout::new(
        header.mi_cols,
        header.mi_rows,
        tile_cols_log2,
        tile_rows_log2,
    );
    header.tile_info = tile_info_of(&layout, 1);
    // The filter search re-decodes this frame's tiles; the decoder derives
    // every tile's own rect from this same `tile_info` (its `mi_col_starts`/
    // `mi_row_starts`), which is what makes the writer's rects and the
    // reader's the same rects.
    let header_tile_info = header.tile_info.clone();
    // lane-av1tsearch: the search runs once per TILE (`search_tiles`), not
    // once over the whole frame. Each tile job builds its own reconstruction
    // planes and its own `above_mode`/`left_mode` bands, which is also what
    // the tile writer reads -- a tile resets its neighbour contexts (spec
    // 5.11.1), so a block at a tile's left edge must cost its mode against
    // the default context, not against whatever the tile to its west left in
    // that row. At one tile the bands are the frame's, exactly as before.
    let (frame_width, frame_height) = (picture.width, picture.height);
    let search_tile = |index: usize,
                       fctx: &crate::decode::FrameCtx|
     -> Result<(Vec<(usize, Superblock, Vec<u8>)>, TilePixels)> {
        crate::tile::arm_pricing_cdfs(None, false);
        let rect = layout.rect(index);
        let mut luma = fresh_plane(
            &picture_y8,
            frame_width,
            frame_height,
            true_width,
            true_height,
        );
        let mut chroma = [
            fresh_plane(
                &picture_u8,
                frame_width / 2,
                frame_height / 2,
                true_width / 2,
                true_height / 2,
            ),
            fresh_plane(
                &picture_v8,
                frame_width / 2,
                frame_height / 2,
                true_width / 2,
                true_height / 2,
            ),
        ];
        clip_planes_to_tile(&mut luma, &mut chroma, rect);
        let mut above_mode = vec![DC_PRED; cols * 2];
        let mut left_mode = vec![DC_PRED; rows * 2];
        let mut ibc = allow_intrabc.then(|| {
            Ibc::new(
                (
                    rect.mi_col0 as usize * 4,
                    rect.mi_row0 as usize * 4,
                    (rect.mi_col1 as usize * 4).min(true_width),
                    (rect.mi_row1 as usize * 4).min(true_height),
                ),
                &picture_y8,
                frame_width,
            )
        });
        let mut coded: Vec<(usize, Superblock, Vec<u8>)> = Vec::new();
        for sb_row in rect.sb_row0 as usize..rect.sb_row1 as usize {
            for sb_col in rect.sb_col0 as usize..rect.sb_col1 as usize {
                let mut modes = Vec::with_capacity(4);
                // The quadrants of a superblock are coded in the order the decoder
                // walks them, which for a 64x64 split into 32x32 blocks is raster
                // order among the quadrants that are inside the frame.
                let mut blocks = Vec::with_capacity(4);
                // lane-i64: the whole superblock as ONE 64x64 intra block, tried
                // BEFORE its quadrants (so both start from the same
                // reconstruction) and compared against the four of them below --
                // the shape the inter frame's own 64 root takes
                // (`search_skip_64`). A frame that allows intrabc is refused
                // outright ([`I64_ROOT`]) because the quadrant searches below
                // carry DV state a discarded trial would desync.
                let (x64, y64) = (sb_col * SUPERBLOCK, sb_row * SUPERBLOCK);
                // lane-corner LIFTED the two guards this root shipped with: the
                // frame no longer has to be a whole number of superblocks, and
                // the superblock no longer has to be wholly inside the frame.
                // The desync the guards were written for (a 248x152 corner's
                // chroma disagreeing three ways) does NOT reproduce at this
                // revision -- see
                // `stream::tests::a_doubly_cut_superblock_root_round_trips_in_every_plane`,
                // which pins all three planes of the encoder's reconstruction,
                // our decoder and ffmpeg on four straddling sizes, three of them
                // cut on BOTH axes, with the edge fire count asserted. What the
                // guards cost was every key-frame root on an arbitrary export
                // size (the `aligned` line switched the whole feature off on, for
                // instance, a 1920x792 frame).
                let whole_inside =
                    x64 + SUPERBLOCK <= luma.true_width && y64 + SUPERBLOCK <= luma.true_height;
                // lane-corner: and the superblock the true frame edge CUTS
                // THROUGH with more than half of each axis still inside, which is
                // all AV1 asks of a `PARTITION_NONE` root (spec
                // `decode_partition`'s `has_cols`/`has_rows`, the same pair the
                // key writer's `Superblock::Whole` arm checks) -- the shape the
                // inter root already takes ([`b64_edge`]).
                let edge_inside = edge_root_enabled()
                    && crate::tile::has_half(
                        sb_col as u32 * crate::tile::SB_MI,
                        crate::tile::SB_MI,
                        header.mi_cols,
                    )
                    && crate::tile::has_half(
                        sb_row as u32 * crate::tile::SB_MI,
                        crate::tile::SB_MI,
                        header.mi_rows,
                    );
                let sb64_legal = i64_root()
                    && ibc.is_none()
                    && sb_row * 2 + 1 < rows
                    && sb_col * 2 + 1 < cols
                    && (whole_inside || edge_inside);
                let mut sb64: Option<(f64, BlockCoeffs, [(Vec<u8>, Vec<CoefCtx>); 3])> = None;
                if sb64_legal {
                    I64_HITS[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let base = snapshot(&luma, &chroma, (x64, y64), SUPERBLOCK);
                    let (c64, r64) = (sb_col * 4, sb_row * 4);
                    let (block, cost) = code_square(
                        &mut luma,
                        &mut chroma,
                        (x64, y64),
                        SUPERBLOCK,
                        &search,
                        &mode_bits(above_mode[c64], left_mode[r64]),
                        tx_select,
                        None,
                        true,
                        fctx,
                    );
                    let cost =
                        cost + search.lambda * crate::tile::partition_bits(SUPERBLOCK, false);
                    let after = snapshot(&luma, &chroma, (x64, y64), SUPERBLOCK);
                    restore(&mut luma, &mut chroma, (x64, y64), SUPERBLOCK, &base);
                    sb64 = Some((cost, block, after));
                }
                // A superblock whose 64x64 trial already codes almost nothing per
                // pixel is taken outright and its quadrants are never searched --
                // `split_rd_breakout` one size up, at the threshold the inter
                // root clamps to ([`b64_breakout_threshold`]), where this lever
                // BUYS wall instead of spending it.
                let early64 = sb64.as_ref().is_some_and(|(c, _, _)| {
                    split_rd_breakout_at(b64_breakout_threshold(), *c, SUPERBLOCK, search.lambda)
                });
                // What the four quadrants cost together, including the split
                // partition symbol at the superblock level, so the two arms are
                // one comparable number.
                let mut sb_cost = match sb64 {
                    Some(_) => search.lambda * crate::tile::partition_bits(SUPERBLOCK, true),
                    None => 0.0,
                };
                for quadrant in 0..if early64 { 0 } else { 4 } {
                    let (col, row) = (sb_col * 2 + quadrant % 2, sb_row * 2 + quadrant / 2);
                    if col >= cols || row >= rows {
                        continue;
                    }
                    let (x, y) = (col * BLOCK, row * BLOCK);
                    let (c0, r0) = (col * 2, row * 2);
                    let base = snapshot(&luma, &chroma, (x, y), BLOCK);

                    // spec `decode_partition`'s hasRows/hasCols recomputed at this
                    // 32x32 block's own half (see `crate::tile::has_half`): the
                    // true frame edge can fall inside a quadrant a superblock-level
                    // check already let through. A quadrant that fails either may
                    // not be left whole; a 16x16 sub-block that fails either (once
                    // split) is a leaf this writer has no rectangular transform
                    // for, so the search must not pick a split that would need one.
                    let (has_cols32, has_rows32) = (
                        crate::tile::has_half(
                            col as u32 * crate::tile::BLOCK_MI,
                            crate::tile::BLOCK_MI,
                            header.mi_cols,
                        ),
                        crate::tile::has_half(
                            row as u32 * crate::tile::BLOCK_MI,
                            crate::tile::BLOCK_MI,
                            header.mi_rows,
                        ),
                    );
                    let whole_legal = has_cols32 && has_rows32;
                    // A 16x16 sub-block's own hasCols/hasRows (spec
                    // `decode_partition`, recomputed at this leaf's own half):
                    // either axis false is a straddling leaf this writer codes as
                    // the 8x8 leaves that are inside it
                    // (`crate::tile::write_leaf8`, lane-av1-rect r7). Both axes
                    // false is the same split with the partition symbol inferred
                    // rather than coded -- no rectangular transform is needed
                    // (lane-av1rect).
                    let sub_half = |sr: usize, sc: usize| {
                        (
                            crate::tile::has_half(
                                sc as u32 * crate::tile::SUB_MI,
                                crate::tile::SUB_MI,
                                header.mi_cols,
                            ),
                            crate::tile::has_half(
                                sr as u32 * crate::tile::SUB_MI,
                                crate::tile::SUB_MI,
                                header.mi_rows,
                            ),
                        )
                    };
                    // What the whole 32x32 costs, including the partition symbol
                    // that says it is not split.
                    let (whole, mut cost_whole) = code_square(
                        &mut luma,
                        &mut chroma,
                        (x, y),
                        BLOCK,
                        &search,
                        &mode_bits(above_mode[c0], left_mode[r0]),
                        tx_select,
                        ibc.as_mut(),
                        true,
                        fctx,
                    );
                    cost_whole += search.lambda * partition_bits(BLOCK, false);
                    let after_whole = snapshot(&luma, &chroma, (x, y), BLOCK);

                    // What four 16x16 blocks cost instead, each searched against
                    // the reconstruction the ones before it left.
                    restore(&mut luma, &mut chroma, (x, y), BLOCK, &base);
                    let mut split = Vec::with_capacity(4);
                    let mut cost_split = search.lambda
                        * (partition_bits(BLOCK, true) + 4.0 * partition_bits(SUB, false));
                    let mut split_modes = Vec::with_capacity(4);
                    for sub in 0..4 {
                        let (sc, sr) = (c0 + sub % 2, r0 + sub / 2);
                        // Same filter as the writer's `sub_positions` (spec
                        // `decode_partition`'s `r >= MiRows || c >= MiCols` early
                        // return): a sub-block whose own origin sits past the
                        // true frame is never coded, so it must not be searched
                        // or pushed onto `split` either, or the writer's block
                        // count will not match what it is prepared to name.
                        if (sr as u32) * crate::tile::SUB_MI >= header.mi_rows
                            || (sc as u32) * crate::tile::SUB_MI >= header.mi_cols
                        {
                            continue;
                        }
                        let (has_cols16, has_rows16) = sub_half(sr, sc);
                        if has_cols16 && has_rows16 {
                            let (block, cost) = code_square(
                                &mut luma,
                                &mut chroma,
                                (x + (sub % 2) * SUB, y + (sub / 2) * SUB),
                                SUB,
                                &search,
                                &mode_bits(above_mode[sc], left_mode[sr]),
                                tx_select,
                                ibc.as_mut(),
                                true,
                                fctx,
                            );
                            cost_split += cost;
                            split_modes.push(block.mode);
                            above_mode[sc] = block.mode;
                            left_mode[sr] = block.mode;
                            split.push(block);
                        } else {
                            // A straddling 16x16: two (or, at a true corner, one)
                            // 8x8 leaves, in the raster order `write_leaf8`'s
                            // caller expects. Both leaves cost against the SAME
                            // enclosing-slot mode context (see
                            // `crate::tile::write_leaf8`'s doc), and neither
                            // updates `above_mode`/`left_mode` at this SUB slot --
                            // the writer never does either, so the search must
                            // not diverge from what it will actually read next.
                            let leaf_mode_bits = mode_bits(above_mode[sc], left_mode[sr]);
                            let (x_sub, y_sub) = (x + (sub % 2) * SUB, y + (sub / 2) * SUB);
                            let mut leaves = Vec::with_capacity(2);
                            for i in 0..4 {
                                let leaf_x = x_sub + (i % 2) * 8;
                                let leaf_y = y_sub + (i / 2) * 8;
                                if leaf_x >= luma.true_width || leaf_y >= luma.true_height {
                                    continue;
                                }
                                let (leaf, cost) = code_square(
                                    &mut luma,
                                    &mut chroma,
                                    (leaf_x, leaf_y),
                                    8,
                                    &search,
                                    &leaf_mode_bits,
                                    tx_select,
                                    None,
                                    true,
                                    fctx,
                                );
                                cost_split += cost;
                                split_modes.push(leaf.mode);
                                leaves.push(leaf);
                            }
                            split.push(BlockCoeffs {
                                eight: Some(leaves),
                                ..BlockCoeffs::default()
                            });
                        }
                    }

                    // A split whose subs are all whole 16x16 is a real quality
                    // alternative to `whole`, exactly as before. Through r14, a
                    // split carrying an 8x8-leaf sub was only ever forced
                    // (`!whole_legal`), never chosen on cost, because the leaf
                    // path was not yet proven against a real decoder; r15 proved
                    // it against ffmpeg, and lane-av1dec r5 taught crate::decode
                    // the read path (`decode_leaf8`), so leaf8 can now win on
                    // cost anywhere, same as any other split.
                    if !whole_legal || (split_blocks && cost_split < cost_whole) {
                        sb_cost += cost_split;
                        modes.extend_from_slice(&split_modes);
                        blocks.push(Quadrant::Split(split));
                    } else {
                        sb_cost += cost_whole;
                        restore(&mut luma, &mut chroma, (x, y), BLOCK, &after_whole);
                        for cell in 0..2 {
                            above_mode[c0 + cell] = whole.mode;
                            left_mode[r0 + cell] = whole.mode;
                        }
                        modes.push(whole.mode);
                        blocks.push(Quadrant::Whole(whole));
                    }
                }
                let index = sb_row * sb_cols + sb_col;
                match sb64 {
                    Some((cost64, block, after)) if early64 || cost64 < sb_cost => {
                        restore(&mut luma, &mut chroma, (x64, y64), SUPERBLOCK, &after);
                        for cell in 0..4 {
                            above_mode[sb_col * 4 + cell] = block.mode;
                            left_mode[sb_row * 4 + cell] = block.mode;
                        }
                        I64_HITS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if !whole_inside {
                            I64_HITS[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        let mode = block.mode;
                        coded.push((index, Superblock::Whole(block), vec![mode]));
                    }
                    _ => coded.push((index, Superblock::Split(blocks), modes)),
                }
            }
        }
        let (x0, y0) = (rect.mi_col0 as usize * 4, rect.mi_row0 as usize * 4);
        let (x1, y1) = (rect.mi_col1 as usize * 4, rect.mi_row1 as usize * 4);
        let luma_rect = (x0, y0, x1, y1);
        let chroma_rect = (x0 / 2, y0 / 2, x1.div_ceil(2), y1.div_ceil(2));
        let pixels = TilePixels {
            luma_rect,
            chroma_rect,
            luma: take_rect(&luma.reconstruction, frame_width, luma_rect),
            chroma: [
                take_rect(&chroma[0].reconstruction, frame_width / 2, chroma_rect),
                take_rect(&chroma[1].reconstruction, frame_width / 2, chroma_rect),
            ],
        };
        Ok((coded, pixels))
    };
    // Each tile's reconstructed rectangles are copied back here, on this
    // thread. The rectangles partition the frame, so the copy order does not
    // change the bytes.
    let searched = search_tiles(layout.count(), fctx, search_tile)?;
    let mut searched_coded = Vec::with_capacity(searched.len());
    for (coded, pixels) in searched {
        put_rect(&mut luma.reconstruction, frame_width, pixels.luma_rect, &pixels.luma);
        for (plane, bytes) in chroma.iter_mut().zip(&pixels.chroma) {
            put_rect(&mut plane.reconstruction, frame_width / 2, pixels.chroma_rect, bytes);
        }
        searched_coded.push(coded);
    }
    let searched = searched_coded;
    // Back into frame raster order: the tile writer indexes `superblocks` by
    // the frame's own superblock index, and `modes` is the coding order the
    // ablation drivers read.
    let mut in_order: Vec<Option<(Superblock, Vec<u8>)>> =
        (0..sb_cols * sb_rows).map(|_| None).collect();
    for (at, superblock, block_modes) in searched.into_iter().flatten() {
        in_order[at] = Some((superblock, block_modes));
    }
    let mut superblocks = Vec::with_capacity(sb_cols * sb_rows);
    let mut modes = Vec::with_capacity(cols * rows);
    for slot in in_order {
        let (superblock, block_modes) = slot.expect("every superblock belongs to a tile");
        superblocks.push(superblock);
        modes.extend_from_slice(&block_modes);
    }

    #[cfg(test)]
    record_predicted_bits(crate::tile::predicted_coeff_bits_sb(
        &superblocks,
        base_q_idx,
    ));
    // A key frame always starts from the defaults (`primary_ref_frame` is
    // `PRIMARY_REF_NONE`), and its header keeps `disable_frame_end_update_cdf`
    // set, so what it stores into the slots it refreshes is exactly those
    // defaults again (spec 7.20's `started_from` arm) -- the first inter
    // frame after it therefore starts from the defaults too.
    let (mi_cols, mi_rows) = (header.mi_cols, header.mi_rows);
    let mut cdfs = crate::cdf_state::Cdfs::new(crate::tile::q_ctx_of(base_q_idx));
    // lane-txi: the ONE place the writer reads the frame's `reduced_tx_set`
    // from -- `Cdfs::txb` widens every luma set under it.
    cdfs.reduced_tx_set = header.reduced_tx_set;
    let start_cdfs = CdfSnapshot(cdfs);
    // The luma restoration-unit grid this frame's `lr_params` implies
    // (`av1_lr_count_units` rounds to nearest, so a short last unit is
    // swallowed by the one before it), for the tile writer's own per-unit
    // walk.
    let lr_horz = crate::restoration::count_units(header.frame_width, lr_unit()) as u32;
    let lr_vert = crate::restoration::count_units(header.frame_height, lr_unit()) as u32;
    // Every tile is coded from this frame's OWN starting tables and leaves
    // its own end state behind (spec 5.11.1: each tile resets the CDFs, the
    // loop-restoration reference and its neighbour contexts), so the tiles
    // are independent of each other and of the order they are written in.
    let code_tiles = |bits: u8,
                      sb_cols: usize,
                      grid: &[u8],
                      units: &[Option<crate::restoration::WienerInfo>]|
     -> Result<Vec<Vec<u8>>> {
        let (out, _) = write_tiles(layout.count(), usize::MAX, |index| {
            crate::tile::arm_cdef_idx(bits, sb_cols, grid.to_vec());
            crate::tile::arm_lr(lr_unit(), lr_horz, lr_vert, units.to_vec());
            crate::tile::arm_sb128(sb128_on());
            let mut cdfs = start_cdfs.0.clone();
            crate::tile::arm_screen(screen);
            crate::tile::arm_frame_mi_dims(frame_mi_dims);
            crate::tile::arm_filter_intra(filter_intra_on());
            crate::tile::arm_intrabc(allow_intrabc);
            crate::tile::arm_pricing_cdfs(None, false);
            let bytes = crate::tile::sb_coeff_key_frame_tile_cdfs(
                mi_cols,
                mi_rows,
                base_q_idx,
                &superblocks,
                tx_select,
                &mut cdfs,
                layout.rect(index),
            )?;
            Ok((bytes, None))
        })?;
        Ok(out)
    };
    let mut tiles = code_tiles(0, sb_cols, &[], &[])?;
    // spec 7.14 deblocking: pick this frame's `loop_filter_level` by handing
    // the tile just coded back to the decoder under each candidate (see
    // `crate::filter_search`), and keep the filtered picture that wins as
    // the reconstruction -- what a decoder outputs, and what the next
    // frame predicts from.
    pick_and_apply_filters(
        &mut header,
        &mut luma,
        &mut chroma,
        [&picture_y8, &picture_u8, &picture_v8],
        // spec 5.9.11/5.9.19/5.9.20: `allow_intrabc` forces deblocking, CDEF
        // and loop restoration off and codes none of their parameters
        // (frame.rs:194/210/252/272) -- there is no filter to search for, and
        // the encoder's reconstruction is already what a decoder produces.
        !allow_intrabc,
        &mut tiles,
        search.lambda,
        fctx,
        // A key frame codes no 128x128 root block (the writer has no intra
        // path at that size), so every unit carries its own literal.
        &[],
        // Every re-code starts from this frame's OWN starting tables, never
        // from where the first write left them: the second tile is the same
        // symbol sequence plus new ones, so it must adapt from the same
        // point the decoder will.
        |bits, sb_cols, grid, units| code_tiles(bits, sb_cols, grid, units),
        |lf, cdef, h, tiles: &[Vec<u8>], lr| {
            let payloads: Vec<&[u8]> = tiles.iter().map(Vec::as_slice).collect();
            crate::decode::decode_key_frame_tiles_lr(
                &payloads,
                &header_tile_info,
                h.mi_cols,
                h.mi_rows,
                base_q_idx,
                h.frame_width,
                h.frame_height,
                seq.enable_filter_intra,
                cdef,
                lf,
                tx_select,
                h.reduced_tx_set,
                h.allow_screen_content_tools,
                h.allow_intrabc,
                lr,
                fctx,
            )
        },
    )?;
    let tile_size_bytes = crate::frame::tile_size_bytes_for(&tiles);
    header.tile_info.tile_size_bytes = tile_size_bytes;
    let tile = crate::frame::tile_group_payload(&tiles, tile_size_bytes);
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&sequence_header_obu(&seq)?);
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile)?);

    let [u, v] = chroma;
    Ok(Encoded {
        stream,
        modes,
        inter_block_share: 0.0,
        reconstruction: Picture {
            width: luma.width,
            height: luma.height,
            y: luma.reconstruction.iter().map(|&v| u16::from(v)).collect(),
            u: u.reconstruction.iter().map(|&v| u16::from(v)).collect(),
            v: v.reconstruction.iter().map(|&v| u16::from(v)).collect(),
        },
        tile,
        mi_cols: header.mi_cols,
        mi_rows: header.mi_rows,
        base_q_idx,
        tx_select,
        // A key frame codes no inter block, so no motion_mode symbol.
        switchable_motion_mode: false,
        screen,
        reduced_tx_set: header.reduced_tx_set,
        next_cdfs: Box::new(start_cdfs.clone()),
        start_cdfs: Box::new(start_cdfs),
        loop_filter: header.loop_filter,
        cdef: header.cdef,
        loop_restoration: header.loop_restoration,
        allow_intrabc,
        delta: header.delta,
    })
}

/// Chooses this frame's deblocking levels and CDEF strengths, writes them
/// into `header` and
/// replaces the frame's own region of the encoder's reconstruction with the
/// filtered picture the decoder produced under them
/// ([`crate::filter_search::pick_deblock`]).
///
/// `search == false` leaves the header at level 0 (deblocking off) and the
/// reconstruction untouched: the caller's stream is one this crate's own
/// decoder cannot read back, so there is nothing to search against.
///
/// # Errors
/// The decoder's own error, when it refuses the tile the encoder just wrote
/// -- that is an encoder defect, not a filter one, so it is propagated
/// rather than swallowed into "no deblocking".
// Spec-shaped signature; bundling the operands would obscure the spec mapping.
#[allow(clippy::too_many_arguments)]
pub(super) fn pick_and_apply_filters(
    header: &mut FrameHeader,
    luma: &mut Plane<'_>,
    chroma: &mut [Plane<'_>; 2],
    source: [&[u8]; 3],
    search: bool,
    // The coded tiles, one payload per tile of the frame's grid, replaced in
    // place when the search chooses a per-64x64 `cdef_idx` list
    // (`cdef_bits > 0`): those literals live in the tile payloads, so every
    // tile is written a second time under `recode` and the winner
    // re-decoded from them.
    tiles: &mut Vec<Vec<u8>>,
    lambda: f64,
    fctx: &crate::decode::FrameCtx,
    // lane-edge128: `crate::tile::cdef_unit_owner` for the blocks these tiles
    // carry -- empty (the identity map) for a frame whose every block covers
    // one 64x64 CDEF unit.
    cdef_owner: &[u32],
    recode: impl Fn(u8, usize, &[u8], &[Option<crate::restoration::WienerInfo>]) -> Result<Vec<Vec<u8>>>,
    decode: impl Fn(
        &LoopFilterParams,
        &CdefParams,
        &FrameHeader,
        &[Vec<u8>],
        &LoopRestorationParams,
    ) -> Result<Picture>,
) -> Result<()> {
    if !search {
        return Ok(());
    }
    let (fw, fh) = (header.frame_width as usize, header.frame_height as usize);
    let hdr = header.clone();
    // libaom `av1_pick_filter_level`'s sibling `av1_cdef_search`:
    // `cdef_damping = 3 + (base_qindex >> 6)`, never searched.
    let damping = 3 + (header.quantization.base_q_idx >> 6);
    let (cols32, rows32) = crate::tile::block_grid(header.mi_cols, header.mi_rows);
    let (sb_cols, sb_rows) = (cols32.div_ceil(2) as usize, rows32.div_ceil(2) as usize);
    let none_lr = LoopRestorationParams::default();
    // Every candidate scores the same coded tile: decode it once and re-run
    // the filters alone for the rest (`crate::decode::FilterReplay`, 24% of
    // the encoder's profile before this). The per-64x64 error the CDEF
    // preset search reads comes off the REPLAYED picture, which is the same
    // picture a decode produces -- that is what the replay is pinned on.
    crate::decode::clear_filter_replay();
    // lane-av1fpar: the per-tile search is over, so every core is idle from
    // here to the end of the frame -- band each candidate's deblock/CDEF
    // replay, the capture decode's own filter chain and the loop-restoration
    // passes across the same workers the search used.
    let _bands = crate::par::override_filter_threads(crate::par::tile_threads());
    let ft = crate::par::timer(crate::par::S_FILTER);
    let search_result = crate::filter_search::pick_filters(
        |lf, cdef| {
            if let Some(picture) = crate::decode::replay_filters(lf, cdef, fctx) {
                return Ok(picture);
            }
            crate::decode::arm_filter_replay();
            let _t = crate::par::timer(crate::par::S_FIRST);
            decode(lf, cdef, &hdr, tiles, &none_lr)
        },
        source,
        luma.width,
        (fw, fh),
        damping,
        (sb_cols, sb_rows),
        lambda,
        cdef_owner,
    );
    drop(ft);
    let (lf, cdef, idx_grid, picture) = search_result?;
    header.loop_filter = lf;
    header.cdef = cdef;
    // spec 7.17: loop restoration. Its per-unit filters live in the TILE, not
    // in the frame header, so a re-decode under candidate parameters cannot
    // search them the way deblocking and CDEF are searched above. What the
    // decoder hands back instead is its own filter chain's intermediates
    // (`crate::decode::FrameCtx::capture_stages`), and the search runs on
    // those, through the decoder's own Wiener kernel.
    //
    // The capture rides the decode this stage was already going to do: the
    // re-decode of the `cdef_idx` tile when there is one, and otherwise one
    // decode of the tile as it stands. That decode's header still says
    // `RESTORE_NONE`, so the decoder takes no pre-CDEF snapshot and the
    // post-CDEF picture stands in for it at LR's 3-row stripe borders --
    // an approximation inside the SEARCH only; the reconstruction this
    // function ends up splicing is always a real decode of the real tile.
    let lr_on = restoration_enabled();
    // The `cdef_idx` literals live in the tile payload, so a frame that
    // chose a per-64x64 preset list codes its tiles a second time.
    if !idx_grid.is_empty() {
        *tiles = recode(cdef.bits, sb_cols, &idx_grid, &[])?;
    }
    // lane-av1cap: that re-coded tile used to be DECODED here as well --
    // 10% of a 4K frame -- purely to reach the post-CDEF plane the loop
    // restoration search reads (`crate::decode::FrameCtx::capture_stages`)
    // and the picture this function splices. Both come out of ONE more
    // replay of the winning parameters on the same captured pre-filter
    // planes every candidate was scored on: the same two filter kernels, on
    // the same input, in the same order, so the same bytes. Re-coding the
    // tile does not move the reconstruction the replay holds -- `cdef_idx`
    // is an equiprobable literal, so it adapts no CDF and changes no
    // residual.
    let replayed = crate::decode::replay_final(&lf, &cdef, &idx_grid, lr_on, fctx);
    #[cfg(test)]
    let replay_was_used = replayed.is_some();
    let (mut filtered, mut stages) = match replayed {
        Some((picture, stages)) => (picture, stages),
        // Nothing captured (the shapes [`crate::decode::FilterReplay`]
        // refuses): every candidate was a full decode, so the winner's own
        // picture stands, and the stages still need a capturing decode.
        None => {
            fctx.capture_stages.set(lr_on);
            let picture = if idx_grid.is_empty() && !lr_on {
                picture
            } else {
                let _t = crate::par::timer(crate::par::S_CAPTURE);
                decode(&lf, &cdef, &hdr, tiles, &none_lr)?
            };
            fctx.capture_stages.set(false);
            (picture, crate::decode::take_filter_stages(fctx))
        }
    };
    #[cfg(test)]
    if crate::decode::verify_final_replay() && replay_was_used {
        fctx.capture_stages.set(lr_on);
        let want = decode(&lf, &cdef, &hdr, tiles, &none_lr)?;
        fctx.capture_stages.set(false);
        let want_stages = crate::decode::take_filter_stages(fctx);
        assert!(
            (want.width, want.height) == (filtered.width, filtered.height)
                && want.y == filtered.y
                && want.u == filtered.u
                && want.v == filtered.v,
            "the final filter replay is not the capture decode it stands in for",
        );
        // Only the frame's own cropped region: the plane is padded to the
        // superblock grid and the columns past the mi extent are written by
        // nothing (the decoder's plane pool leaves whatever the last frame
        // put there), while the restoration search reads strictly inside
        // `fw x fh` -- `restoration::lr_sample` clamps its column to
        // `plane_w - 1` and its row to the frame's, and
        // `filter_search::pick_restoration`'s own SSE walk never leaves the
        // crop either.
        let region = |a: &[u16], b: &[u16], stride: usize, w: usize, h: usize, what: &str| {
            for row in 0..h {
                let (x, y) = (&a[row * stride..][..w], &b[row * stride..][..w]);
                assert!(
                    x == y,
                    "{what}: the replayed filter stages are not the ones the capture \
                     decode returns (row {row} of {h}, width {w}, stride {stride})",
                );
            }
        };
        match (&want_stages, &stages) {
            (Some(a), Some(b)) => {
                assert_eq!(a.stride, b.stride, "the captured plane strides moved");
                let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
                for (i, (w, h)) in [(fw, fh), (cw, ch), (cw, ch)].into_iter().enumerate() {
                    region(
                        &a.deblocked[i],
                        &b.deblocked[i],
                        a.stride[i],
                        w,
                        h,
                        "deblocked",
                    );
                    region(&a.cdefed[i], &b.cdefed[i], a.stride[i], w, h, "cdefed");
                }
            }
            (None, None) => {}
            _ => panic!("one path captured filter stages and the other did not"),
        }
    }
    // lane-av1cap2: the capture stays live past the restoration search --
    // `crate::decode::replay_restoration` filters the same post-CDEF plane
    // rather than decoding the re-coded tile. Cleared at the end instead.
    let mut lr = LoopRestorationParams::default();
    if lr_on {
        let unit = lr_unit();
        let mut want = LoopRestorationParams {
            loop_restoration_size: [unit, unit, unit],
            uses_lr: true,
            // spec `lr_params`: `RESTORATION_TILESIZE_MAX >> (2 - shift)`, so
            // 0 is the 64 unit and 1 the 128 one a 128 superblock forces.
            lr_unit_shift: u8::from(unit == 128),
            ..LoopRestorationParams::default()
        };
        want.frame_restoration_type[0] = ec_av1_syntax::RestorationType::Wiener;
        let lrt = crate::par::timer(crate::par::S_LR);
        let units = match stages.take() {
            Some(stages) => crate::filter_search::pick_restoration(
                &stages,
                source[0],
                luma.width,
                (fw, fh),
                &want,
                lambda,
                fctx,
            ),
            None => Vec::new(),
        };
        // A frame no unit restores keeps `RESTORE_NONE` on every plane
        // rather than paying ~1 bit per 64x64 for a tile full of
        // `restore_wiener = 0` symbols (the BD gate saw exactly that on the
        // clip whose units never take a filter).
        drop(lrt);
        if units.iter().any(Option::is_some) {
            let coded = recode(cdef.bits, sb_cols, &idx_grid, &units)?;
            let _t = crate::par::timer(crate::par::S_CAPTURE);
            // lane-av1cap2: that re-coded tile used to be DECODED here to
            // reach the restored reference frame -- 5.6% of a 4K frame,
            // for a filter that reads nothing of the tile but the per-unit
            // Wiener filters this stage just chose. Run the decoder's own
            // restoration kernel on the replay's post-CDEF plane instead
            // (`crate::decode::replay_restoration`), with the real
            // post-deblock stripe borders.
            match crate::decode::replay_restoration(&want, &units, fctx) {
                Some(y) => {
                    #[cfg(test)]
                    let margin = fctx
                        .last_frame_wide_margin
                        .with(|m| m.borrow().as_ref().map(|p| p.y.clone()));
                    #[cfg(test)]
                    if crate::decode::verify_final_replay() {
                        let want_pic = decode(&lf, &cdef, &hdr, &coded, &want)?;
                        assert!(
                            (want_pic.width, want_pic.height) == (filtered.width, filtered.height)
                                && want_pic.u == filtered.u
                                && want_pic.v == filtered.v,
                            "the restoration replay moved a chroma plane or the picture size",
                        );
                        assert!(
                            want_pic.y == y,
                            "the restoration replay is not the decode of the re-coded tile",
                        );
                        assert!(
                            fctx.last_frame_wide_margin
                                .with(|m| m.borrow().as_ref().map(|p| p.y.clone()))
                                == margin,
                            "the restoration replay's wide-margin luma is not the decode's",
                        );
                    }
                    filtered.y = y;
                }
                // Nothing captured: the winner came out of a full decode,
                // so this one does too.
                None => filtered = decode(&lf, &cdef, &hdr, &coded, &want)?,
            }
            *tiles = coded;
            lr = want;
        }
    }
    crate::decode::clear_filter_replay();
    header.loop_restoration = lr;
    let dec_cw = filtered.width.div_ceil(2);
    let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
    crate::filter_search::splice(
        &mut luma.reconstruction,
        luma.width,
        &filtered.y,
        filtered.width,
        fw,
        fh,
    );
    crate::filter_search::splice(
        &mut chroma[0].reconstruction,
        chroma[0].width,
        &filtered.u,
        dec_cw,
        cw,
        ch,
    );
    crate::filter_search::splice(
        &mut chroma[1].reconstruction,
        chroma[1].width,
        &filtered.v,
        dec_cw,
        cw,
        ch,
    );
    Ok(())
}

/// `Y_MODE`'s size group (spec `Size_Group`) for a 32x32 block: the only
/// group an inter frame's intra branch codes (spec `inter_frame_mode_info`),
/// since every block this crate's inter tile writer codes is 32x32.
pub(super) const SIZE_GROUP_32: usize = 3;

/// `Ref_Frame_List`'s `LAST_FRAME` (spec 3): the only reference
/// [`sb_coeff_inter_frame_tile`] ever names, and so the only one the MV
/// stack this encoder builds is ever asked to predict against.
pub(super) const LAST_FRAME: i8 = 1;

/// What an inter frame's intra branch spends to name each of the thirteen
/// modes: `Y_MODE` by size group rather than `KF_Y_MODE` by neighbour
/// context, since an inter frame's intra blocks do not read their
/// neighbours' modes (spec `inter_frame_mode_info`, `sb_coeff_inter_frame_tile`'s
/// own doc comment). Unlike [`mode_bits`] this needs no per-block neighbour
/// state, so it is built once per frame.
pub(super) fn inter_mode_bits() -> [f64; 13] {
    std::array::from_fn(|mode| {
        let angle = if (usize::from(V_PRED)..=usize::from(D67_PRED)).contains(&mode) {
            symbol_bits(&cdf::ANGLE_DELTA[mode - usize::from(V_PRED)], 3)
        } else {
            0.0
        };
        symbol_bits(&cdf::Y_MODE[SIZE_GROUP_32], mode)
            + angle
            + symbol_bits(&cdf::UV_MODE_CFL[mode], usize::from(DC_PRED))
    })
}

/// A 1/8-pel motion vector component, converted to the 1/16-pel offset
/// [`mc::predict`] takes, in the units of the plane it predicts into.
///
/// A luma sample of displacement is `mv/8`; in the 1/16-pel domain that is
/// `mv*2`. A 4:2:0 chroma sample is half a luma sample wide, so the same
/// physical displacement is `mv/16` chroma samples — `mv*1` in the 1/16-pel
/// domain. `luma` picks which.
pub(super) fn mv_to_q4(pos: usize, mv_component: i32, luma: bool) -> i32 {
    (pos as i32) * 16 + mv_component * if luma { 2 } else { 1 }
}

/// `CLASS0_SIZE << (class + 2)`, mirroring `tile::mv_class_base` (private to
/// that module): the magnitude an `MV_CLASS_n` component's own bits start
/// counting from.
pub(super) fn mv_class_base(class: usize) -> i32 {
    if class == 0 { 0 } else { 2i32 << (class + 2) }
}

/// The class a pre-offset magnitude `z` (`|diff| - 1`) falls in, mirroring
/// `tile::mv_class_of`.
pub(super) fn mv_class_of(z: i32) -> usize {
    let mut class = 0;
    while class < 10 && mv_class_base(class + 1) <= z {
        class += 1;
    }
    class
}

/// What [`crate::tile::write_mv_component`] (private to that module) would
/// spend coding one non-zero motion vector component, against the same
/// static default CDFs the writer starts a frame from — the same
/// static-table approximation [`mode_bits`] costs the luma mode symbol
/// through, not the tile writer's own adapting state, which this encoder has
/// no way to read without duplicating it block for block.
///
/// Returns `None` when `diff` needs eighth-pel precision in a frame coded
/// without `allow_high_precision_mv`, which is what [`round_to_valid_mv`]
/// exists to avoid ever happening.
pub(super) fn mv_component_bits(diff: i32) -> Option<f64> {
    let hp_frame = crate::tile::high_precision_mv();
    let mag = diff.unsigned_abs() as i32;
    let z = mag - 1;
    if !hp_frame && z & 1 == 0 {
        return None;
    }
    let mut bits = symbol_bits(&cdf::MV_SIGN, usize::from(diff < 0));
    let class = mv_class_of(z);
    bits += symbol_bits(&cdf::MV_CLASS, class);
    let local = z - mv_class_base(class);
    let hp = (local & 1) as usize;
    if class == 0 {
        let bit = (local >> 3) & 1;
        let fr = (local >> 1) & 3;
        bits += symbol_bits(&cdf::MV_CLASS0_BIT, bit as usize);
        bits += symbol_bits(&cdf::MV_CLASS0_FR[bit as usize], fr as usize);
        // lane-hpmv: the writer codes the eighth-pel bit in a high-precision
        // frame, so the pricer charges for it there and only there.
        if hp_frame {
            bits += symbol_bits(&cdf::MV_CLASS0_HP, hp);
        }
    } else {
        let d = local >> 3;
        let fr = (local >> 1) & 3;
        for i in 0..class {
            bits += symbol_bits(&cdf::MV_BIT[i], ((d >> i) & 1) as usize);
        }
        bits += symbol_bits(&cdf::MV_FR, fr as usize);
        if hp_frame {
            bits += symbol_bits(&cdf::MV_HP, hp);
        }
    }
    Some(bits)
}

/// Whether `LAST2_FRAME` gets a `NEWMV` search of its own beside the
/// neighbour-derived modes ([`extra_ref_new_mv`]'s per-reference switch,
/// `EC_AV1_LAST2_NEW=0`): the charter's "free arm", i.e. the reference's
/// value without the second motion search's wall.
pub(super) fn last2_new_mv() -> bool {
    match std::env::var("EC_AV1_LAST2_NEW").ok() {
        Some(v) => v != "0",
        None => true,
    }
}

/// Whether an extra reference (GOLDEN/ALTREF) is offered a `NEWMV` of its
/// own -- a second motion search per block against that reference's picture,
/// priced off that reference's own stack. `EC_AV1_MV_EXTRA_NEW=0` turns it
/// off in a test build (what the lane measured both arms with).
pub(super) fn extra_ref_new_mv() -> bool {
    match std::env::var("EC_AV1_MV_EXTRA_NEW").ok() {
        Some(v) if cfg!(test) => v != "0",
        _ => crate::speed::at(&crate::speed::EXTRA_REF_NEW),
    }
}

/// The seven `ref_frame_idx` order hints of a frame coded on this crate's own
/// flat slot plan (`inter_frame_headers_slots`): every reference but
/// `GOLDEN_FRAME` and `ALTREF_FRAME` reads the frame just coded,
/// `GOLDEN_FRAME` reads the GOP's key frame, and `ALTREF_FRAME` reads the
/// frame two back when there is one (otherwise it, too, names the key frame's
/// slot). Hints are 7-bit, the only `order_hint_bits` this encoder writes.
pub(crate) fn flat_order_hints(order_hint: u32, key_hint: u32, has_altref: bool) -> [u32; 7] {
    let last = order_hint.wrapping_sub(1) & 0x7f;
    let alt = if has_altref {
        order_hint.wrapping_sub(2) & 0x7f
    } else {
        key_hint
    };
    [last, last, last, key_hint, last, last, alt]
}

/// Whether an inter frame carries `reference_select` (spec 5.9.22): the
/// header bit that lets each block choose between a single and a compound
/// reference. Off by default until compound prediction is measured to pay;
/// `EC_AV1_COMPOUND=1` arms it in a test build (which is what the BD gate
/// runs as), and every block still codes SINGLE, so the only cost is one
/// `comp_mode` symbol per inter block.
pub(crate) const REFERENCE_SELECT: bool = true;

/// [`REFERENCE_SELECT`], with the test-build environment override.
pub(crate) fn reference_select() -> bool {
    match std::env::var("EC_AV1_COMPOUND").ok() {
        Some(v) if cfg!(test) => v != "0",
        _ => crate::speed::at(&crate::speed::COMPOUND),
    }
}

/// [`tile::write_drl_idx`]'s cost against one stack: one `drl_mode` symbol
/// per entry past `start`, `1` to advance and `0` to stop, at most two.
pub(super) fn drl_bits_for(stack: &MvStack, start: usize, target: usize) -> f64 {
    let mut bits = 0.0;
    let mut idx = start;
    while idx < start + 2 && stack.entries.len() > idx + 1 {
        let advance = idx < target;
        bits += symbol_bits(&cdf::DRL_MODE[stack.drl_ctx[idx]], usize::from(advance));
        if !advance {
            break;
        }
        idx += 1;
    }
    bits
}

/// The cheapest `(bits, drl index)` the `NEWMV` syntax can name `mv` with off
/// `stack`: each DRL entry is a different `PredMv` (spec 7.10.2.10
/// `assign_mv`), so the same vector costs a different residual from each.
pub(super) fn best_new_mv_syntax(stack: &MvStack, mv: (i32, i32)) -> Option<(f64, u8)> {
    let mut best: Option<(f64, u8)> = None;
    for idx in 0..3usize {
        if idx > 0 && stack.entries.len() <= idx {
            break;
        }
        let base = stack.entries.get(idx).map_or(stack.pred_mv, |e| e.mv);
        if let Some(b) = mv_residual_bits(mv, base) {
            let total = b + drl_bits_for(stack, 0, idx);
            if best.is_none_or(|(t, _)| total < t) {
                best = Some((total, idx as u8));
            }
        }
    }
    best
}

/// The starting points [`motion::search`] gets besides `stack.pred_mv`: the
/// zero vector and the stack's own candidates. A neighbour block's vector is
/// where this block's motion most often already is, which is what pays for
/// the extra starting points (measured on the BD gate: -0.9/-0.8/-0.0 points
/// vs libaom for 0.2 s off a 6 s ladder).
pub(super) fn mv_seeds(stack: &MvStack) -> ([(i32, i32); 4], usize) {
    let mut seeds = [(0, 0); 4];
    let mut n = 1;
    for e in stack.entries.iter().take(3) {
        seeds[n] = e.mv;
        n += 1;
    }
    (seeds, n)
}

/// What [`crate::tile::write_mv`] (private to that module) would spend
/// coding `mv` as a residual against `pred`: the joint symbol naming which
/// components differ, then each differing component. `None` under the same
/// condition [`mv_component_bits`] returns `None`.
/// How many frames back `ref_frame` sits from the frame being coded, off the
/// order hints [`crate::tile::arm_order_hints`] armed for it. `LAST_FRAME` is
/// 1 in this encoder's GOP; `GOLDEN`/`ALTREF` can be much further, which is
/// what [`crate::motion::search`] scales its starting step by.
pub(super) fn ref_distance(ref_frame: i8) -> u32 {
    let (bits, order_hint, hints) = crate::tile::order_hints();
    let i = (ref_frame - crate::mvstack::LAST_FRAME).clamp(0, 6) as usize;
    let d = crate::motion_field::get_relative_dist(bits, order_hint, hints[i])
        .unsigned_abs()
        .max(1);
    if crate::envflags::env_flag!("EC_TRACE_DIST") {
        eprintln!(
            "EC_DIST ref={ref_frame} bits={bits} oh={order_hint} hint={} d={d}",
            hints[i]
        );
    }
    d
}

pub(super) fn mv_residual_bits(mv: (i32, i32), pred: (i32, i32)) -> Option<f64> {
    let diff = (mv.0 - pred.0, mv.1 - pred.1);
    let joint = match (diff.0 != 0, diff.1 != 0) {
        (false, false) => 0,
        (false, true) => 1,
        (true, false) => 2,
        (true, true) => 3,
    };
    let mut bits = symbol_bits(&cdf::MV_JOINT, joint);
    for d in [diff.0, diff.1] {
        if d != 0 {
            bits += mv_component_bits(d)?;
        }
    }
    Some(bits)
}

/// Rounds `mv` so that its residual against `pred` is one
/// [`crate::tile::write_mv_component`] can actually code: each component's
/// difference is either zero or has an even magnitude (the eighth-pel bit
/// `allow_high_precision_mv` off always infers as one forces the coded
/// magnitude odd one step further in, which comes out even here since this
/// crate's motion search works in whole units of that same step). Rounding
/// down keeps every coded MV — and so every `nearest_mv` a later block's
/// stack votes with — built from an even displacement from `(0, 0)`, so the
/// invariant holds without this function seeing the whole stack.
/// A PROCESS-GLOBAL [`high_precision_mv_at`] override that wins over both
/// the threshold and the environment (`u8::MAX` = unset, `0` = never, `1` =
/// always), the shape [`set_frame_interp`] has: the witness that has to see
/// `mv_hp` symbols coded turns it on through this while holding
/// [`crate::speed::knob_write`].
pub(super) static HP_MV_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// Sets [`HP_MV_OVERRIDE`]; `None` clears it.
#[allow(dead_code)] // set only from the `#[cfg(test)]` gates
pub(crate) fn set_high_precision_mv(on: Option<bool>) {
    HP_MV_OVERRIDE.store(
        on.map_or(u8::MAX, u8::from),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The qindex below which an inter frame codes `allow_high_precision_mv`
/// (lane-hpmv). libaom's `HIGH_PRECISION_MV_QTHRESH` is 128; `0` is this
/// crate's shipped value, i.e. high-precision motion vectors off in every
/// frame -- see `lanes/hpmv.report.md` for the sweep that chose it.
pub(crate) const HP_MV_QTHRESH: u8 = 0;

/// Whether an inter frame at `base_q_idx` carries `allow_high_precision_mv`:
/// [`HP_MV_QTHRESH`], overridable by `EC_AV1_HP_MV` (`0`/`off` never, `all`
/// always, any number as the threshold) so the BD gate can sweep the policy
/// without a rebuild.
pub(crate) fn high_precision_mv_at(base_q_idx: u8) -> bool {
    match HP_MV_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => return false,
        1 => return true,
        _ => {}
    }
    static T: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
    let threshold = *T.get_or_init(|| match std::env::var("EC_AV1_HP_MV").ok().as_deref() {
        Some("all" | "always") => 256,
        Some("0" | "off") => 0,
        Some(v) => v.parse().unwrap_or(u16::from(HP_MV_QTHRESH)),
        None => u16::from(HP_MV_QTHRESH),
    });
    u16::from(base_q_idx) < threshold
}

pub(super) fn round_to_valid_mv(mv: (i32, i32), pred: (i32, i32)) -> (i32, i32) {
    // lane-hpmv: with `allow_high_precision_mv` on, every eighth-pel
    // residual is codable and there is nothing to round away.
    if crate::tile::high_precision_mv() {
        return mv;
    }
    let round = |m: i32, p: i32| {
        let diff = m - p;
        let mag = diff.unsigned_abs() as i32;
        let even = mag - mag % 2;
        if even == 0 {
            p
        } else {
            p + even * diff.signum()
        }
    };
    (round(mv.0, pred.0), round(mv.1, pred.1))
}

/// Predicts one `side`-square block by motion compensation from `reference`
/// at `mv` (1/8-pel), then codes it through [`Plane::code_from_prediction`].
#[allow(clippy::too_many_arguments)]
pub(super) fn mc_trial(
    plane: &Plane,
    x: usize,
    y: usize,
    side: usize,
    mv: (i32, i32),
    luma: bool,
    reference: &[u16],
    stride: usize,
    ref_width: usize,
    ref_height: usize,
    skip: bool,
    base_q_idx: u8,
    deadzone: f64,
    set: TxbSet,
    fctx: &crate::decode::FrameCtx,
) -> Trial {
    mc_trial_typed(
        plane,
        x,
        y,
        side,
        mv,
        luma,
        reference,
        stride,
        ref_width,
        ref_height,
        skip,
        base_q_idx,
        deadzone,
        set,
        TxType::DctDct,
        fctx,
    )
}

/// [`mc_trial`] coding its residual with a named transform type (lane-txset2).
#[allow(clippy::too_many_arguments)]
pub(super) fn mc_trial_typed(
    plane: &Plane,
    x: usize,
    y: usize,
    side: usize,
    mv: (i32, i32),
    luma: bool,
    reference: &[u16],
    stride: usize,
    ref_width: usize,
    ref_height: usize,
    skip: bool,
    base_q_idx: u8,
    deadzone: f64,
    set: TxbSet,
    tx_type: TxType,
    fctx: &crate::decode::FrameCtx,
) -> Trial {
    let x_q4 = mv_to_q4(x, mv.1, luma);
    let y_q4 = mv_to_q4(y, mv.0, luma);
    // lane-hbd r4: `reference` (a DPB `Picture`'s plane) is `u16` now that
    // `Picture` is widened; `prediction`/the encoder stay `u8` (encoder is
    // 8-bit only this round, see `intra_predict_u8`'s doc comment).
    // lane-av1speed: both buffers are per-candidate scratch on the hottest
    // path in the encoder, so they live on the stack -- `side` is at most
    // `BLOCK` (32) for luma and half that for chroma. Two heap allocations
    // per motion-compensated trial otherwise.
    let mut prediction16 = [0u16; BLOCK * BLOCK];
    let prediction16 = &mut prediction16[..side * side];
    mc::predict(
        reference,
        stride,
        ref_width,
        ref_height,
        x_q4,
        y_q4,
        side,
        side,
        prediction16,
        fctx,
    );
    let mut prediction = [0u8; BLOCK * BLOCK];
    let prediction = &mut prediction[..side * side];
    for (dst, &src) in prediction.iter_mut().zip(prediction16.iter()) {
        *dst = src as u8;
    }
    plane.code_from_prediction_typed(
        x, y, side, prediction, skip, base_q_idx, deadzone, set, tx_type,
    )
}

/// [`mc_trial`]'s compound counterpart (spec 7.11.3.1's compound path): both
/// references predict into the `CONV_BUF` intermediate domain
/// ([`mc::predict_compound_intermediate`]) and are blended by
/// [`mc::combine_compound`] at the simple-average split `(8, 8)` -- exactly
/// what a decoder computes for a `comp_group_idx == 0`, `compound_idx == 1`
/// block, so the encoder's reconstruction is the decoder's.
#[allow(clippy::too_many_arguments)]
pub(super) fn mc_trial_compound(
    plane: &Plane,
    x: usize,
    y: usize,
    side: usize,
    mvs: ((i32, i32), (i32, i32)),
    luma: bool,
    ref0: (&[u16], usize, usize, usize),
    ref1: (&[u16], usize, usize, usize),
    base_q_idx: u8,
    deadzone: f64,
    set: TxbSet,
    fctx: &crate::decode::FrameCtx,
) -> Trial {
    mc_trial_compound_typed(
        plane,
        x,
        y,
        side,
        mvs,
        luma,
        ref0,
        ref1,
        base_q_idx,
        deadzone,
        set,
        TxType::DctDct,
        fctx,
    )
}

/// [`mc_trial_compound`] coding its residual with a named transform type
/// (lane-txset2).
#[allow(clippy::too_many_arguments)]
pub(super) fn mc_trial_compound_typed(
    plane: &Plane,
    x: usize,
    y: usize,
    side: usize,
    (mv0, mv1): ((i32, i32), (i32, i32)),
    luma: bool,
    ref0: (&[u16], usize, usize, usize),
    ref1: (&[u16], usize, usize, usize),
    base_q_idx: u8,
    deadzone: f64,
    set: TxbSet,
    tx_type: TxType,
    fctx: &crate::decode::FrameCtx,
) -> Trial {
    let mut inter0 = [0i32; BLOCK * BLOCK];
    let mut inter1 = [0i32; BLOCK * BLOCK];
    let (inter0, inter1) = (&mut inter0[..side * side], &mut inter1[..side * side]);
    for (mv, r, dst) in [(mv0, ref0, &mut *inter0), (mv1, ref1, &mut *inter1)] {
        mc::predict_compound_intermediate(
            r.0,
            r.1,
            r.2,
            r.3,
            mv_to_q4(x, mv.1, luma),
            mv_to_q4(y, mv.0, luma),
            mc::RefScale::UNSCALED,
            side,
            side,
            fctx.interp_filter.get(),
            fctx.interp_filter.get(),
            dst,
            fctx,
        );
    }
    let mut blended16 = [0u16; BLOCK * BLOCK];
    let blended16 = &mut blended16[..side * side];
    mc::combine_compound(inter0, inter1, 8, 8, blended16, fctx);
    let mut prediction = [0u8; BLOCK * BLOCK];
    let prediction = &mut prediction[..side * side];
    for (dst, &src) in prediction.iter_mut().zip(blended16.iter()) {
        *dst = src as u8;
    }
    plane.code_from_prediction_typed(
        x, y, side, prediction, false, base_q_idx, deadzone, set, tx_type,
    )
}

/// Codes one 32x32 block of an inter frame as whichever costs least of: each
/// intra mode [`Search::modes`] offers (predicted exactly as a key frame's
/// block is), `NEARESTMV`, and `NEWMV` searched from `reference` by
/// [`motion::search`] and seeded by `stack.pred_mv` — both inter candidates
/// coding a real residual (`skip: false`) whenever one prices out cheaper
/// than the all-skip candidate, now that `sb_coeff_inter_frame_tile`'s
/// missing inter `tx_type` symbol (the desync this function's candidates
/// used to route around; see `crate::cdf_state::TxbSet::Luma32Inter`) is
/// fixed.
///
/// The symbol costs this ranks candidates by use fixed contexts (0) for
/// `skip`, `intra_inter` and `single_ref` — the tile writer's actual
/// contexts for those come from its own neighbour bookkeeping
/// (`tile::Neighbours`), which is private to that module and not worth
/// duplicating for three one-or-two-bit symbols; `new_mv`/`ref_mv`/`zero_mv`/
/// `drl_mode` contexts come straight from `stack`, which is exact, because
/// [`find_mv_stack`] is public and this function is handed the same `MvStack`
/// the tile writer derives from the same grid state.
#[allow(clippy::too_many_arguments)]
/// One prediction of `side` x `side` samples from `reference`, as `u8` --
/// [`mc_trial`]'s prediction half, on the heap because a 64x64 luma block is
/// four times the stack scratch that function sizes for [`BLOCK`].
pub(super) fn predict_u8(
    reference: &[u16],
    stride: usize,
    ref_width: usize,
    ref_height: usize,
    (x, y): (usize, usize),
    side: usize,
    mv: (i32, i32),
    luma: bool,
    fctx: &crate::decode::FrameCtx,
) -> Vec<u8> {
    let mut prediction = vec![0u16; side * side];
    mc::predict(
        reference,
        stride,
        ref_width,
        ref_height,
        mv_to_q4(x, mv.1, luma),
        mv_to_q4(y, mv.0, luma),
        side,
        side,
        &mut prediction,
        fctx,
    );
    prediction.iter().map(|&v| v as u8).collect()
}

/// [`predict_u8`]'s compound counterpart, on the heap because a 64x64 root's
/// side is twice what [`mc_trial_compound`]'s fixed trial buffers hold: both
/// references predict into the `CONV_BUF` intermediate domain and are blended
/// at the simple-average split `(8, 8)`, the same path (and so the same
/// samples) a `comp_group_idx == 0`, `compound_idx == 1` decoder computes.
pub(super) fn predict_compound_u8(
    ref0: (&[u16], usize, usize, usize),
    ref1: (&[u16], usize, usize, usize),
    (x, y): (usize, usize),
    side: usize,
    (mv0, mv1): ((i32, i32), (i32, i32)),
    luma: bool,
    fctx: &crate::decode::FrameCtx,
) -> Vec<u8> {
    let mut inter = [vec![0i32; side * side], vec![0i32; side * side]];
    for ((mv, r), dst) in [(mv0, ref0), (mv1, ref1)].into_iter().zip(inter.iter_mut()) {
        mc::predict_compound_intermediate(
            r.0,
            r.1,
            r.2,
            r.3,
            mv_to_q4(x, mv.1, luma),
            mv_to_q4(y, mv.0, luma),
            mc::RefScale::UNSCALED,
            side,
            side,
            fctx.interp_filter.get(),
            fctx.interp_filter.get(),
            dst,
            fctx,
        );
    }
    let mut blended = vec![0u16; side * side];
    mc::combine_compound(&inter[0], &inter[1], 8, 8, &mut blended, fctx);
    blended.iter().map(|&v| v as u8).collect()
}

/// lane-b128: the 128x128 `PARTITION_NONE` candidate for a whole 128
/// superblock root, the lever cen3's L1 asks for -- one block, one mode and
/// one motion vector where the four 64x64 roots below spend four of each.
///
/// SKIP only (`residual: false`): a 128x128 block's residual is four
/// TX_64X64 luma units interleaved with a TX_32X32 chroma pair per 64x64 mu
/// chunk, which neither the writer nor [`Plane::code_from_prediction`] has a
/// path for -- offering one here would price a block
/// [`crate::tile::write_inter_block_128`] refuses. Single reference only for
/// the same reason: the compound arm at this size is untested against the
/// writer's own `write_compound_block` window.
///
/// Only a root whose whole 128x128 lies inside the true frame AND inside
/// this tile is offered: the writer's own root codes `PARTITION_NONE` only
/// where the spec's `has_rows`/`has_cols` are both set, and a straddling
/// block would have to be ranked over its inside part alone.
///
/// Returns `(cost, lambda, block, reconstruction)` -- the cost already
/// carries the root's own `PARTITION_NONE` symbol, so the caller weighs it
/// against the four superblocks plus the SPLIT symbol.
#[allow(clippy::too_many_arguments)]
pub(super) fn search_root_128(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    grid: &crate::mvstack::MiGrid,
    (root_r, root_c): (usize, usize),
    // The 32x32-block grid this tile's `blocks` vector is indexed on.
    (rows, cols): (usize, usize),
    // The frame's own mode-info grid, which bounds the root.
    (mi_cols, mi_rows): (u32, u32),
    search: Search<'_>,
    tpl_factors: &Option<Vec<f64>>,
    sb_q: Option<&Vec<u8>>,
    sb_cols: usize,
    tpl_cells_x: usize,
    base_q_idx: u8,
    // lane-rectdq: `1 << delta_q_res` when this frame codes a delta_q plan,
    // zero when it does not -- what [`delta_q_bits_at_root`] prices with.
    dq_res: i32,
    reference: &Picture,
    // The `LAST` + extra-reference pairs at THIS ROOT's own 32x32-mi window,
    // built by the caller off the same grid the tile writer rebuilds its
    // stack from (empty unless the frame carries `reference_select` and
    // [`b128_compound`] is on).
    compound: &[(i8, &Picture, crate::mvstack::CompoundMvStack)],
    fctx: &crate::decode::FrameCtx,
) -> Option<(f64, f64, BlockCoeffs, [(Vec<u8>, Vec<CoefCtx>); 3])> {
    const SIDE: usize = SUPERBLOCK * 2;
    const SIDE_MI: usize = SIDE / 4;
    let (x128, y128) = (root_c * SUPERBLOCK, root_r * SUPERBLOCK);
    let (mi_row, mi_col) = (root_r * 16, root_c * 16);
    if root_r * 2 + 3 >= rows
        || root_c * 2 + 3 >= cols
        || (mi_row + SIDE_MI) as u32 > mi_rows
        || (mi_col + SIDE_MI) as u32 > mi_cols
        || x128 + SIDE > luma.true_width
        || y128 + SIDE > luma.true_height
    {
        return None;
    }
    // One lambda for the whole root, the mean of what its four superblocks
    // would each be searched at -- the 128-vs-four comparison weighs one cost
    // against four, so both sides have to be priced at the same lambda (the
    // same rule the 64 root's own mean-of-sixteen tpl cells follows).
    let cell_lambda = |sb_r: usize, sb_c: usize| match (sb_q, tpl_factors) {
        (Some(q), _) => sb_q_search(q, sb_cols, base_q_idx, (sb_r, sb_c), search).lambda,
        (None, Some(f)) => {
            let mean = (0..16)
                .map(|i| f[(sb_r * 4 + i / 4) * tpl_cells_x + sb_c * 4 + i % 4])
                .sum::<f64>()
                / 16.0;
            search.lambda * mean
        }
        (None, None) => search.lambda,
    };
    let lambda = [(0, 0), (0, 1), (1, 0), (1, 1)]
        .iter()
        .map(|&(dr, dc)| cell_lambda(root_r + dr, root_c + dc))
        .sum::<f64>()
        / 4.0;
    // The qindex this root is really quantized at: the delta_q unit is the
    // 128 superblock, so the root's own top-left cell is the one coded
    // (lane-b128m), and the residual arm below has to quantize at exactly it.
    let root_search = Search {
        lambda,
        ..match sb_q {
            Some(q) => sb_q_search(q, sb_cols, base_q_idx, (root_r, root_c), search),
            None => search,
        }
    };
    let stack = find_mv_stack(
        grid,
        mi_row,
        mi_col,
        SIDE_MI,
        SIDE_MI,
        LAST_FRAME,
        mi_cols as usize,
        mi_rows as usize,
    );
    let base = snapshot(luma, chroma, (x128, y128), SIDE);
    let found = search_skip_64(
        luma,
        chroma,
        (x128, y128),
        (SIDE, SIDE),
        &root_search,
        false,
        reference,
        &stack,
        compound,
        fctx,
    );
    let (mut cost, mut block) = found?;
    if block.inter.is_some_and(|i| i.ref1.is_some()) {
        B128_COMPOUND_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    // lane-b128r: the same prediction with a REAL residual, priced against
    // the skip arm the search just committed to the planes (which IS the
    // winner's prediction, sample for sample). The block's residual is four
    // TX_64X64 luma units, one per 64x64 mu chunk, each interleaved with its
    // own TX_32X32 chroma pair -- so it is coded here as twelve trials at
    // sides the forward network already reaches, never as one 128-point
    // transform. Var-tx depth 0 only: the four units ARE the tree's leaves.
    //
    // The four `txfm_partition` symbols the writer codes for them are not
    // priced, exactly as the 64 root does not price its own (their contexts
    // are only known at write time); at four symbols a root this is under a
    // bit against residuals of hundreds.
    if b128_residual() && block.skip {
        // Every unit's coded level grid is 32x32 (a 64-point transform codes
        // only its top-left corner), packed per plane into one 64-wide grid
        // with chunk (cr, cc) at (cr * 32, cc * 32) -- the layout
        // `write_inter_block_128` slices back out in the same chunk order.
        let mut packed = [
            vec![0i32; 64 * 64],
            vec![0i32; 64 * 64],
            vec![0i32; 64 * 64],
        ];
        let mut chunks: Vec<((usize, usize), [Trial; 3])> = Vec::new();
        let (mut sse_skip, mut sse_res, mut coeff_bits) = (0.0f64, 0.0f64, 0.0f64);
        for (cr, cc) in [(0usize, 0usize), (0, 1), (1, 0), (1, 1)] {
            let (px, py) = (x128 + cc * SUPERBLOCK, y128 + cr * SUPERBLOCK);
            let (cx, cy) = (px / 2, py / 2);
            let half = SUPERBLOCK / 2;
            let pred_y = rows_of(&luma.reconstruction, luma.width, px, py, SUPERBLOCK);
            let pred_u = rows_of(&chroma[0].reconstruction, chroma[0].width, cx, cy, half);
            let pred_v = rows_of(&chroma[1].reconstruction, chroma[1].width, cx, cy, half);
            sse_skip += luma.block_sse(px, py, SUPERBLOCK, &pred_y)
                + chroma[0].block_sse(cx, cy, half, &pred_u)
                + chroma[1].block_sse(cx, cy, half, &pred_v);
            let trials = [
                luma.code_from_prediction(
                    px,
                    py,
                    SUPERBLOCK,
                    &pred_y,
                    false,
                    root_search.base_q_idx,
                    root_search.deadzone,
                    TxbSet::Luma64,
                ),
                chroma[0].code_from_prediction(
                    cx,
                    cy,
                    half,
                    &pred_u,
                    false,
                    root_search.base_q_idx,
                    root_search.deadzone,
                    TxbSet::Chroma32,
                ),
                chroma[1].code_from_prediction(
                    cx,
                    cy,
                    half,
                    &pred_v,
                    false,
                    root_search.base_q_idx,
                    root_search.deadzone,
                    TxbSet::Chroma32,
                ),
            ];
            for (plane, trial) in trials.iter().enumerate() {
                sse_res += trial.sse;
                coeff_bits += trial.bits;
                for row in 0..32 {
                    packed[plane][(cr * 32 + row) * 64 + cc * 32..][..32]
                        .copy_from_slice(&trial.levels[row * 32..][..32]);
                }
            }
            chunks.push(((cr, cc), trials));
        }
        // `cost` is the skip arm's own `sse + lambda * syntax`; the residual
        // arm pays the same syntax with `skip` coded 0 instead of 1, plus its
        // coefficients.
        let unskip = skip_bits_at(&stack, false) - skip_bits_at(&stack, true);
        let res_cost = cost - sse_skip + sse_res + lambda * (unskip + coeff_bits);
        if res_cost < cost || FORCE_B128_RESIDUAL.load(std::sync::atomic::Ordering::Relaxed) {
            B128_RESIDUAL_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            for ((cr, cc), trials) in &chunks {
                let (px, py) = (x128 + cc * SUPERBLOCK, y128 + cr * SUPERBLOCK);
                let half = SUPERBLOCK / 2;
                luma.commit(px, py, SUPERBLOCK, &trials[0]);
                chroma[0].commit(px / 2, py / 2, half, &trials[1]);
                chroma[1].commit(px / 2, py / 2, half, &trials[2]);
            }
            cost = res_cost;
            block.skip = false;
            block.tx_depth = 0;
            block.luma_tx_types = Vec::new();
            block.luma = coeffs(&packed[0], 64);
            block.u = coeffs(&packed[1], 64);
            block.v = coeffs(&packed[2], 64);
        }
    }
    let after = snapshot(luma, chroma, (x128, y128), SIDE);
    restore(luma, chroma, (x128, y128), SIDE, &base);
    // lane-rectdq: a SKIPPED whole-superblock root is the one shape that
    // codes no delta_q group (`write_delta_q`'s `is_whole_sb && skip`); the
    // residual arm above codes it like every other block does.
    let dq = if block.skip {
        0.0
    } else {
        delta_q_bits_at_root(sb_q, sb_cols, base_q_idx, dq_res, (root_r, root_c))
    };
    Some((
        cost + lambda * (crate::tile::partition_bits(SIDE, false) + dq),
        lambda,
        block,
        after,
    ))
}

/// lane-rectres: one rect/AB piece's REAL residual, priced against the skip
/// arm the caller's [`search_skip_64`] just committed to the planes (which
/// IS the winner's prediction, sample for sample -- the same rule
/// [`search_root_128`]'s residual arm runs at the NONE root). The piece's
/// residual is one TX_64X64 luma unit plus a TX_32X32 chroma pair per 64x64
/// mu chunk (two chunks for a 128x64 / 64x128 half, one for a 64x64 square),
/// each trial at sides the forward network already reaches, never a
/// rectangular transform; var-tx depth 0 only -- the units ARE the tree's
/// leaves, exactly what `decode_inter_block`'s mu-chunk loop reads.
///
/// The depth-0 `txfm_partition` symbols the writer codes for the units are
/// not priced, exactly as [`search_root_128`] does not price its four (their
/// contexts are only known at write time); at two symbols a half this is
/// under a bit against residuals of hundreds. On a win (or under the witness
/// force) the trials are committed to the planes, `block` is flipped to
/// carry the packed level grids [`crate::tile::write_inter_block_128_rect`]
/// slices back out, and the new cost is returned; otherwise `cost` passes
/// through untouched. Counts [`take_b128_rectres_hits`] on a win.
#[allow(clippy::too_many_arguments)]
pub(super) fn search_rect_residual(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    // The piece's own top-left, in luma samples.
    (px, py): (usize, usize),
    // Its true footprint: (128, 64) HORZ, (64, 128) VERT, (64, 64) square.
    (w, h): (usize, usize),
    cost: f64,
    lambda: f64,
    search: &Search<'_>,
    // The piece's own mv stack, for the skip symbol's cost either way.
    stack: &crate::mvstack::MvStack,
    block: &mut BlockCoeffs,
) -> f64 {
    /// The 64x64 "mu" chunk side, luma; chroma halves it.
    const MU: usize = SUPERBLOCK;
    let chunks: [(usize, usize); 2] = match (w, h) {
        (128, 64) => [(0, 0), (0, 1)],
        (64, 128) => [(0, 0), (1, 0)],
        _ => [(0, 0), (0, 0)],
    };
    let n = if w.max(h) > MU { 2 } else { 1 };
    // Every unit's coded level grid is 32x32 (a 64-point transform codes
    // only its top-left corner), packed per plane into one 64-wide grid with
    // chunk (cr, cc) at (cr * 32, cc * 32) -- the layout the writer slices
    // back out in the same chunk order.
    let mut packed = [
        vec![0i32; 64 * 64],
        vec![0i32; 64 * 64],
        vec![0i32; 64 * 64],
    ];
    let mut sse_skip = 0.0f64;
    let mut sse_res = 0.0f64;
    let mut coeff_bits = 0.0f64;
    let mut trials_per_chunk: Vec<[Trial; 3]> = Vec::with_capacity(n);
    for &(cr, cc) in &chunks[..n] {
        let (cxx, cyy) = (px + cc * MU, py + cr * MU);
        let (cx, cy) = (cxx / 2, cyy / 2);
        let half = MU / 2;
        let pred_y = rows_of(&luma.reconstruction, luma.width, cxx, cyy, MU);
        let pred_u = rows_of(&chroma[0].reconstruction, chroma[0].width, cx, cy, half);
        let pred_v = rows_of(&chroma[1].reconstruction, chroma[1].width, cx, cy, half);
        sse_skip += luma.block_sse(cxx, cyy, MU, &pred_y)
            + chroma[0].block_sse(cx, cy, half, &pred_u)
            + chroma[1].block_sse(cx, cy, half, &pred_v);
        let trials = [
            luma.code_from_prediction(
                cxx,
                cyy,
                MU,
                &pred_y,
                false,
                search.base_q_idx,
                search.deadzone,
                TxbSet::Luma64,
            ),
            chroma[0].code_from_prediction(
                cx,
                cy,
                half,
                &pred_u,
                false,
                search.base_q_idx,
                search.deadzone,
                TxbSet::Chroma32,
            ),
            chroma[1].code_from_prediction(
                cx,
                cy,
                half,
                &pred_v,
                false,
                search.base_q_idx,
                search.deadzone,
                TxbSet::Chroma32,
            ),
        ];
        for (plane, trial) in trials.iter().enumerate() {
            sse_res += trial.sse;
            coeff_bits += trial.bits;
            for row in 0..32 {
                packed[plane][(cr * 32 + row) * 64 + cc * 32..][..32]
                    .copy_from_slice(&trial.levels[row * 32..][..32]);
            }
        }
        trials_per_chunk.push(trials);
    }
    // `cost` is the skip arm's own `sse + lambda * syntax`; the residual arm
    // pays the same syntax with `skip` coded 0 instead of 1, plus its
    // coefficients.
    let unskip = skip_bits_at(stack, false) - skip_bits_at(stack, true);
    let res_cost = cost - sse_skip + sse_res + lambda * (unskip + coeff_bits);
    if res_cost < cost || FORCE_B128_RECTRES.load(std::sync::atomic::Ordering::Relaxed) {
        for (trials, &(cr, cc)) in trials_per_chunk.iter().zip(&chunks[..n]) {
            let (cxx, cyy) = (px + cc * MU, py + cr * MU);
            let (cx, cy) = (cxx / 2, cyy / 2);
            let half = MU / 2;
            luma.commit(cxx, cyy, MU, &trials[0]);
            chroma[0].commit(cx, cy, half, &trials[1]);
            chroma[1].commit(cx, cy, half, &trials[2]);
        }
        block.skip = false;
        block.tx_depth = 0;
        block.luma_tx_types = Vec::new();
        block.luma = coeffs(&packed[0], 64);
        block.u = coeffs(&packed[1], 64);
        block.v = coeffs(&packed[2], 64);
        B128_RECTRES_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        res_cost
    } else {
        cost
    }
}

/// lane-b128hv: the `PARTITION_HORZ` / `PARTITION_VERT` candidate for a whole
/// 128 superblock root -- two 128x64 (`horz`) or 64x128 inter blocks, the
/// shipped NONE machinery halved. Skip-only by default, single reference or
/// compound; under `EC_AV1_RECTRES` each half also prices a REAL residual
/// ([`search_rect_residual`]) -- exactly what
/// `crate::tile::write_inter_block_128_rect` codes and `decode_inter_block`
/// reads at `sb128_rect`.
///
/// The FIRST half is published into `grid` before the second is searched --
/// the writer publishes it too, so a second half searched against the stale
/// grid would be priced (and its mv predicted) off a stack the decoder never
/// derives (class `encoder-grid-drift-invisible-to-pixel-gate`). The grid and
/// both planes are put back before returning; the caller re-applies the
/// winner.
///
/// Returns `(cost, lambda, the two halves, reconstruction)`, the cost already
/// carrying the root's own partition symbol so it ranks against
/// [`search_root_128`] and against the four superblocks below.
#[allow(clippy::too_many_arguments)]
pub(super) fn search_root_128_rect(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    grid: &mut crate::mvstack::MiGrid,
    (root_r, root_c): (usize, usize),
    (rows, cols): (usize, usize),
    (mi_cols, mi_rows): (u32, u32),
    search: Search<'_>,
    tpl_factors: &Option<Vec<f64>>,
    sb_q: Option<&Vec<u8>>,
    sb_cols: usize,
    tpl_cells_x: usize,
    base_q_idx: u8,
    // lane-rectdq: see [`search_root_128`]'s own `dq_res`.
    dq_res: i32,
    reference: &Picture,
    // The `LAST` + extra-reference pairs offered at this root; only the
    // reference and its picture are read here, since each HALF derives its
    // own compound stack at its own rectangular window.
    compound: &[(i8, &Picture, crate::mvstack::CompoundMvStack)],
    horz: bool,
    fctx: &crate::decode::FrameCtx,
) -> Option<(f64, f64, [BlockCoeffs; 2], [(Vec<u8>, Vec<CoefCtx>); 3])> {
    const SIDE: usize = SUPERBLOCK * 2;
    const SIDE_MI: usize = SIDE / 4;
    let (x128, y128) = (root_c * SUPERBLOCK, root_r * SUPERBLOCK);
    let (mi_row, mi_col) = (root_r * 16, root_c * 16);
    if root_r * 2 + 3 >= rows
        || root_c * 2 + 3 >= cols
        || (mi_row + SIDE_MI) as u32 > mi_rows
        || (mi_col + SIDE_MI) as u32 > mi_cols
        || x128 + SIDE > luma.true_width
        || y128 + SIDE > luma.true_height
    {
        return None;
    }
    // The root's own lambda, the mean of its four superblocks' -- the same
    // one [`search_root_128`] prices with, so the three root candidates and
    // the four-superblock arm are all comparable.
    let cell_lambda = |sb_r: usize, sb_c: usize| match (sb_q, tpl_factors) {
        (Some(q), _) => sb_q_search(q, sb_cols, base_q_idx, (sb_r, sb_c), search).lambda,
        (None, Some(f)) => {
            let mean = (0..16)
                .map(|i| f[(sb_r * 4 + i / 4) * tpl_cells_x + sb_c * 4 + i % 4])
                .sum::<f64>()
                / 16.0;
            search.lambda * mean
        }
        (None, None) => search.lambda,
    };
    let lambda = [(0, 0), (0, 1), (1, 0), (1, 1)]
        .iter()
        .map(|&(dr, dc)| cell_lambda(root_r + dr, root_c + dc))
        .sum::<f64>()
        / 4.0;
    let root_search = Search {
        lambda,
        ..match sb_q {
            Some(q) => sb_q_search(q, sb_cols, base_q_idx, (root_r, root_c), search),
            None => search,
        }
    };
    let (w, h) = if horz {
        (SIDE, SUPERBLOCK)
    } else {
        (SUPERBLOCK, SIDE)
    };
    let (w_mi, h_mi) = (w / 4, h / 4);
    let base = snapshot(luma, chroma, (x128, y128), SIDE);
    let grid_base = grid.snapshot_rect(mi_row, mi_col, SIDE_MI, SIDE_MI);
    let mut cost = 0.0f64;
    let mut halves: Vec<BlockCoeffs> = Vec::with_capacity(2);
    for i in 0..2usize {
        let (mr, mc) = if horz {
            (mi_row + i * 16, mi_col)
        } else {
            (mi_row, mi_col + i * 16)
        };
        let stack = find_mv_stack(
            grid,
            mr,
            mc,
            w_mi,
            h_mi,
            LAST_FRAME,
            mi_cols as usize,
            mi_rows as usize,
        );
        let half_compound: Vec<(i8, &Picture, crate::mvstack::CompoundMvStack)> = compound
            .iter()
            .map(|&(r, pic, _)| {
                (
                    r,
                    pic,
                    crate::mvstack::find_mv_stack_compound(
                        grid,
                        mr,
                        mc,
                        w_mi,
                        h_mi,
                        (LAST_FRAME, r),
                        mi_cols as usize,
                        mi_rows as usize,
                        grid.sign_bias_table(),
                        &[(0, 0); 7],
                        None,
                    ),
                )
            })
            .collect();
        let found = search_skip_64(
            luma,
            chroma,
            (mc * 4, mr * 4),
            (w, h),
            &root_search,
            false,
            reference,
            &stack,
            &half_compound,
            fctx,
        );
        match found {
            Some((mut c, mut block)) => {
                // lane-rectres: the same prediction with a REAL residual,
                // priced against the skip arm the search just committed to
                // the planes -- two 64x64 mu chunks for a half.
                if rectres() && block.skip {
                    c = search_rect_residual(
                        luma,
                        chroma,
                        (mc * 4, mr * 4),
                        (w, h),
                        c,
                        root_search.lambda,
                        &root_search,
                        &stack,
                        &mut block,
                    );
                }
                cost += c;
                record_mi_rect(grid, mr, mc, (w_mi, h_mi), block.inter, block.skip);
                halves.push(block);
            }
            None => {
                restore(luma, chroma, (x128, y128), SIDE, &base);
                grid.restore_rect(mi_row, mi_col, SIDE_MI, SIDE_MI, &grid_base);
                return None;
            }
        }
    }
    let after = snapshot(luma, chroma, (x128, y128), SIDE);
    restore(luma, chroma, (x128, y128), SIDE, &base);
    grid.restore_rect(mi_row, mi_col, SIDE_MI, SIDE_MI, &grid_base);
    let symbol = if horz { 1 } else { 2 };
    let [b0, b1] = <[BlockCoeffs; 2]>::try_from(halves).ok()?;
    // lane-rectdq: the FIRST half sits at the root's own mode-info origin and
    // codes the delta_q group whether it is skipped or not
    // (`write_inter_block_128_rect` passes `is_whole_sb = false`); the second
    // half is at a non-origin mi and codes none.
    let dq = delta_q_bits_at_root(sb_q, sb_cols, base_q_idx, dq_res, (root_r, root_c));
    Some((
        cost + lambda * (crate::tile::partition_bits_sym(SIDE, symbol) + dq),
        lambda,
        [b0, b1],
        after,
    ))
}

/// lane-ab128: one AB shape at a 128 superblock root -- two 64x64 blocks and
/// one 128x64 / 64x128 half, in the order [`ab128_pieces`] gives (the
/// decoder's own). Each piece is priced through the same `search_skip_64`
/// the NONE and HORZ/VERT arms use, at its own footprint, and is published
/// into `grid` before the next is searched -- the writer publishes it too, so
/// a later piece searched against a stale grid would be priced (and its mv
/// predicted) off a stack the decoder never derives (class
/// `encoder-grid-drift-invisible-to-pixel-gate`). The grid and both planes
/// are put back before returning; the caller re-applies the winner.
///
/// Skip-only by default; under `EC_AV1_RECTRES` each piece also prices a
/// REAL residual ([`search_rect_residual`]) -- the 64x64 squares are where
/// libaom's own AB shapes earn their keep.
///
/// Returns `(cost, lambda, the three pieces, reconstruction)`, the cost
/// already carrying the root's own partition symbol so it ranks against the
/// other root candidates and against the four superblocks below.
#[allow(clippy::too_many_arguments)]
pub(super) fn search_root_128_ab(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    grid: &mut crate::mvstack::MiGrid,
    (root_r, root_c): (usize, usize),
    (rows, cols): (usize, usize),
    (mi_cols, mi_rows): (u32, u32),
    search: Search<'_>,
    tpl_factors: &Option<Vec<f64>>,
    sb_q: Option<&Vec<u8>>,
    sb_cols: usize,
    tpl_cells_x: usize,
    base_q_idx: u8,
    reference: &Picture,
    compound: &[(i8, &Picture, crate::mvstack::CompoundMvStack)],
    // The partition symbol, 4..=7.
    symbol: usize,
    fctx: &crate::decode::FrameCtx,
) -> Option<(f64, f64, [BlockCoeffs; 3], [(Vec<u8>, Vec<CoefCtx>); 3])> {
    const SIDE: usize = SUPERBLOCK * 2;
    const SIDE_MI: usize = SIDE / 4;
    let (x128, y128) = (root_c * SUPERBLOCK, root_r * SUPERBLOCK);
    let (mi_row, mi_col) = (root_r * 16, root_c * 16);
    if root_r * 2 + 3 >= rows
        || root_c * 2 + 3 >= cols
        || (mi_row + SIDE_MI) as u32 > mi_rows
        || (mi_col + SIDE_MI) as u32 > mi_cols
        || x128 + SIDE > luma.true_width
        || y128 + SIDE > luma.true_height
    {
        return None;
    }
    // The root's own lambda, the mean of its four superblocks' -- the same one
    // [`search_root_128`] prices with, so every root candidate and the
    // four-superblock arm are comparable.
    let cell_lambda = |sb_r: usize, sb_c: usize| match (sb_q, tpl_factors) {
        (Some(q), _) => sb_q_search(q, sb_cols, base_q_idx, (sb_r, sb_c), search).lambda,
        (None, Some(f)) => {
            let mean = (0..16)
                .map(|i| f[(sb_r * 4 + i / 4) * tpl_cells_x + sb_c * 4 + i % 4])
                .sum::<f64>()
                / 16.0;
            search.lambda * mean
        }
        (None, None) => search.lambda,
    };
    let lambda = [(0, 0), (0, 1), (1, 0), (1, 1)]
        .iter()
        .map(|&(dr, dc)| cell_lambda(root_r + dr, root_c + dc))
        .sum::<f64>()
        / 4.0;
    let root_search = Search {
        lambda,
        ..match sb_q {
            Some(q) => sb_q_search(q, sb_cols, base_q_idx, (root_r, root_c), search),
            None => search,
        }
    };
    let base = snapshot(luma, chroma, (x128, y128), SIDE);
    let grid_base = grid.snapshot_rect(mi_row, mi_col, SIDE_MI, SIDE_MI);
    let mut cost = 0.0f64;
    let mut pieces: Vec<BlockCoeffs> = Vec::with_capacity(3);
    for ((mr, mc), (w, h)) in ab128_pieces(symbol, (mi_row, mi_col)) {
        let (w_mi, h_mi) = (w / 4, h / 4);
        let stack = find_mv_stack(
            grid,
            mr,
            mc,
            w_mi,
            h_mi,
            LAST_FRAME,
            mi_cols as usize,
            mi_rows as usize,
        );
        let piece_compound: Vec<(i8, &Picture, crate::mvstack::CompoundMvStack)> = compound
            .iter()
            .map(|&(r, pic, _)| {
                (
                    r,
                    pic,
                    crate::mvstack::find_mv_stack_compound(
                        grid,
                        mr,
                        mc,
                        w_mi,
                        h_mi,
                        (LAST_FRAME, r),
                        mi_cols as usize,
                        mi_rows as usize,
                        grid.sign_bias_table(),
                        &[(0, 0); 7],
                        None,
                    ),
                )
            })
            .collect();
        let found = search_skip_64(
            luma,
            chroma,
            (mc * 4, mr * 4),
            (w, h),
            &root_search,
            false,
            reference,
            &stack,
            &piece_compound,
            fctx,
        );
        match found {
            Some((mut c, mut block)) => {
                // lane-rectres: the same prediction with a REAL residual,
                // priced against the skip arm the search just committed to
                // the planes -- one mu chunk for a 64x64 square, two for a
                // half. The 64x64 squares are where libaom's own AB shapes
                // earn their keep.
                if rectres() && block.skip {
                    c = search_rect_residual(
                        luma,
                        chroma,
                        (mc * 4, mr * 4),
                        (w, h),
                        c,
                        root_search.lambda,
                        &root_search,
                        &stack,
                        &mut block,
                    );
                }
                cost += c;
                record_mi_rect(grid, mr, mc, (w_mi, h_mi), block.inter, block.skip);
                pieces.push(block);
            }
            None => {
                restore(luma, chroma, (x128, y128), SIDE, &base);
                grid.restore_rect(mi_row, mi_col, SIDE_MI, SIDE_MI, &grid_base);
                return None;
            }
        }
    }
    let after = snapshot(luma, chroma, (x128, y128), SIDE);
    restore(luma, chroma, (x128, y128), SIDE, &base);
    grid.restore_rect(mi_row, mi_col, SIDE_MI, SIDE_MI, &grid_base);
    let [b0, b1, b2] = <[BlockCoeffs; 3]>::try_from(pieces).ok()?;
    Some((
        cost + lambda * crate::tile::partition_bits_sym(SIDE, symbol),
        lambda,
        [b0, b1, b2],
        after,
    ))
}

/// The 64x64 `PARTITION_NONE` candidate for a whole superblock ([`B64_ROOT`]):
/// single-reference LAST at `NEARESTMV`/`GLOBALMV`/`NEARMV`/`NEWMV`, coded
/// SKIP, priced through the same `symbol_bits` path every other candidate
/// uses. The winner's prediction is committed to the planes (it IS the
/// reconstruction -- a skip block has no residual); the caller restores them
/// if the four quadrants come out cheaper.
///
/// `None` when no candidate could be coded at all (a `NEWMV` residual the
/// writer cannot name is simply not offered, and the other modes always are,
/// so this only happens if every candidate is refused).
// Spec-shaped signature; bundling the operands would obscure the spec mapping.
#[allow(clippy::too_many_arguments)]
pub(super) fn search_skip_64(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    // The root's footprint in samples: [`SUPERBLOCK`] square at a 64 root,
    // twice that at the 128 one [`search_root_128`] drives (lane-b128), and
    // 128x64 / 64x128 at the rect halves of [`search_root_128_rect`]
    // (lane-b128hv). Every trial below tiles it into squares of its SHORTER
    // side (one square when it is square, two otherwise), and the residual
    // arm is only ever asked for at 64 -- a 128 block's residual is four
    // TX_64X64 units the writer has no path for.
    (bw, bh): (usize, usize),
    search: &Search,
    // Whether the non-skip arm is priced at all ([`b64_residual`], and off on
    // screen content -- see that function's doc).
    residual: bool,
    reference: &Picture,
    stack: &MvStack,
    // The COMPOUND stack of each `LAST` + extra-reference pair at THIS
    // superblock's 16x16-mi window, built off the same grid the tile writer
    // rebuilds it from (empty unless the frame carries `reference_select`).
    compound: &[(i8, &Picture, crate::mvstack::CompoundMvStack)],
    fctx: &crate::decode::FrameCtx,
) -> Option<(f64, BlockCoeffs)> {
    debug_assert!(
        (bw, bh) == (SUPERBLOCK, SUPERBLOCK) || !residual,
        "only a 64 root has a residual arm"
    );
    // The square units every trial below is taken on: the block itself when
    // it is square, its two halves otherwise. Luma first (in raster order),
    // then each chroma plane over the same tiling at half the side -- which is
    // exactly `[(0, x, y, side), (1, ..), (2, ..)]` at bw == bh.
    let sq = bw.min(bh);
    let units: Vec<(usize, usize, usize, usize, bool)> = (0..3usize)
        .flat_map(|plane| {
            let (ox, oy, s) = if plane == 0 {
                (x, y, sq)
            } else {
                (x / 2, y / 2, sq / 2)
            };
            (0..bh / sq).flat_map(move |j| {
                (0..bw / sq).map(move |i| (plane, ox + i * s, oy + j * s, s, plane == 0))
            })
        })
        .collect();
    let not_new = symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 1);
    let not_zero = symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 1);
    let ref_bits = single_ref_bits(crate::mvstack::LAST_FRAME, stack);
    // What every candidate pays whatever its vector: `skip` coded 1, then
    // `is_inter` coded 1, then the LAST chain above.
    let fixed = skip_bits_at(stack, true) + intra_inter_bits_at(stack, true) + ref_bits;
    let info_of = |mode: InterMode, mv: (i32, i32), ref_mv_idx: u8| InterInfo {
        ref1: None,
        mv1: (0, 0),
        ref_frame: crate::mvstack::LAST_FRAME,
        mode,
        mv,
        ref_mv_idx,
    };
    let mut cands: Vec<(f64, InterInfo)> = vec![
        (
            not_new + not_zero + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 0),
            info_of(InterMode::NearestMv, stack.nearest_mv, 0),
        ),
        (
            not_new + symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 0),
            info_of(InterMode::GlobalMv, (0, 0), 0),
        ),
    ];
    let near_bits = not_new + not_zero + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 1);
    for idx in 1..=2usize {
        if let Some(e) = stack.entries.get(idx) {
            cands.push((
                near_bits + drl_bits_for(stack, 1, idx),
                info_of(InterMode::NearMv, e.mv, idx as u8),
            ));
        }
    }
    // `LAST`'s own `NEWMV` vector, kept for the compound `NEW_NEARESTMV`
    // candidate below to pair with the second reference's NEAREST half.
    let mut last_new_mv: Option<(i32, i32)> = None;
    if leaf_new_mv() {
        let source_block = luma.source_block_rect(x, y, bw, bh);
        let (seeds, seed_n) = mv_seeds(stack);
        let found = motion::search(
            &reference.y,
            reference.width,
            luma.true_width,
            luma.true_height,
            &source_block,
            x,
            y,
            bw,
            bh,
            stack.pred_mv,
            &seeds[..seed_n],
            search.lambda,
            fctx,
            1,
        );
        let mv = round_to_valid_mv(found.mv, stack.pred_mv);
        if let Some((bits, ref_mv_idx)) = best_new_mv_syntax(stack, mv) {
            last_new_mv = Some(mv);
            cands.push((
                symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 0) + bits + mv_rate_surcharge(bits),
                info_of(InterMode::NewMv, mv, ref_mv_idx),
            ));
        }
    }

    let mut best: Option<(f64, f64, InterInfo, Vec<Trial>)> = None;
    for (bits, info) in cands {
        let trials: Vec<Trial> = units
            .iter()
            .map(|&(plane, px, py, s, is_luma)| {
                let (samples, stride, w, h) = match plane {
                    0 => (
                        &reference.y,
                        reference.width,
                        luma.true_width,
                        luma.true_height,
                    ),
                    1 => (
                        &reference.u,
                        reference.width / 2,
                        chroma[0].true_width,
                        chroma[0].true_height,
                    ),
                    _ => (
                        &reference.v,
                        reference.width / 2,
                        chroma[1].true_width,
                        chroma[1].true_height,
                    ),
                };
                let prediction =
                    predict_u8(samples, stride, w, h, (px, py), s, info.mv, is_luma, fctx);
                let target: &Plane = if plane == 0 { luma } else { &chroma[plane - 1] };
                // `skip` true: the trial is the prediction itself plus its SSE,
                // so no transform of any size is involved (the reason a 64x64
                // root can be coded at all before a forward TX_64X64 exists).
                target.code_from_prediction(
                    px,
                    py,
                    s,
                    &prediction,
                    true,
                    search.base_q_idx,
                    search.deadzone,
                    TxbSet::Luma32Inter,
                )
            })
            .collect();
        let cost = trials.iter().map(|t| t.sse).sum::<f64>() + search.lambda * (fixed + bits);
        if best.as_ref().is_none_or(|b| cost < b.0) {
            best = Some((cost, fixed + bits, info, trials));
        }
    }

    // lane-b64b: the COMPOUND candidates at the 64 root, the same three a
    // superblock can form without a second motion search ([`b64_compound`]),
    // priced through the same `symbol_bits` path -- `fixed` minus the single
    // reference's tree, plus the pair tree and the compound mode.
    for (ref1, g, cstack) in compound.iter().filter(|_| b64_compound()) {
        let (ref1, g) = (*ref1, *g);
        // The whole `comp_mode` + reference-pair tree at the writer's own
        // contexts, for THIS pair (lane-ctx).
        let pair_bits = compound_ref_bits((crate::mvstack::LAST_FRAME, ref1), stack);
        let mode_ctx = cdf::COMPOUND_MODE_CTX_MAP[cstack.ref_mv_ctx >> 1][cstack.new_mv_ctx.min(4)];
        let mode_bits_of =
            |mode: usize| pair_bits + symbol_bits(&cdf::INTER_COMPOUND_MODE[mode_ctx], mode);
        let compound_info = |mode: InterMode, mvs: ((i32, i32), (i32, i32))| InterInfo {
            ref1: Some(ref1),
            mv1: mvs.1,
            ref_frame: crate::mvstack::LAST_FRAME,
            mode,
            mv: mvs.0,
            ref_mv_idx: 0,
        };
        let mut ccands: Vec<(((i32, i32), (i32, i32)), f64, InterInfo)> = vec![
            (
                cstack.nearest_mv,
                mode_bits_of(0),
                compound_info(InterMode::NearestNearestMv, cstack.nearest_mv),
            ),
            (
                ((0, 0), (0, 0)),
                mode_bits_of(6),
                compound_info(InterMode::GlobalGlobalMv, ((0, 0), (0, 0))),
            ),
        ];
        // `NEW_NEARESTMV`: the NEW half is `LAST`'s vector from the search
        // above, its residual priced against compound stack entry 0 -- what
        // `assign_compound_mv` predicts from at `ref_mv_idx = 0`.
        let cbase = cstack
            .entries
            .first()
            .map_or(cstack.nearest_mv, |e| (e.mv0, e.mv1));
        if let Some(new_mv) = last_new_mv
            && let Some(b0) = mv_residual_bits(new_mv, cbase.0)
        {
            let mvs = (new_mv, cstack.nearest_mv.1);
            ccands.push((
                mvs,
                mode_bits_of(3) + b0,
                compound_info(InterMode::NewNearestMv, mvs),
            ));
        }
        ccands.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        ccands.dedup_by_key(|c| c.0);
        for (mvs, bits, info) in ccands {
            let trials: Vec<Trial> = units
                .iter()
                .map(|&(plane, px, py, s, is_luma)| {
                    let (r0, r1) = match plane {
                        0 => (
                            (
                                reference.y.as_slice(),
                                reference.width,
                                luma.true_width,
                                luma.true_height,
                            ),
                            (g.y.as_slice(), g.width, luma.true_width, luma.true_height),
                        ),
                        1 => (
                            (
                                reference.u.as_slice(),
                                reference.width / 2,
                                chroma[0].true_width,
                                chroma[0].true_height,
                            ),
                            (
                                g.u.as_slice(),
                                g.width / 2,
                                chroma[0].true_width,
                                chroma[0].true_height,
                            ),
                        ),
                        _ => (
                            (
                                reference.v.as_slice(),
                                reference.width / 2,
                                chroma[1].true_width,
                                chroma[1].true_height,
                            ),
                            (
                                g.v.as_slice(),
                                g.width / 2,
                                chroma[1].true_width,
                                chroma[1].true_height,
                            ),
                        ),
                    };
                    let prediction = predict_compound_u8(r0, r1, (px, py), s, mvs, is_luma, fctx);
                    let target: &Plane = if plane == 0 { luma } else { &chroma[plane - 1] };
                    target.code_from_prediction(
                        px,
                        py,
                        s,
                        &prediction,
                        true,
                        search.base_q_idx,
                        search.deadzone,
                        TxbSet::Luma32Inter,
                    )
                })
                .collect();
            let syntax = fixed - ref_bits + bits;
            let cost = trials.iter().map(|t| t.sse).sum::<f64>() + search.lambda * syntax;
            if best.as_ref().is_none_or(|b| cost < b.0) {
                best = Some((cost, syntax, info, trials));
            }
        }
    }

    let (skip_cost, syntax, info, skip_trials) = best?;

    // The winner's prediction coded with a REAL residual (lane-tx64): one
    // TX_64X64 luma transform -- `TxbSet::Luma64`, whose coded quarter is the
    // 32x32 corner `code_from_prediction` hands back -- and one TX_32X32 per
    // chroma plane, since a 64x64 root's chroma block IS 32x32. Only the mv
    // the skip arm already chose is re-coded: the prediction with the lowest
    // SSE is also the cheapest residual to code, and a second 64x64 motion
    // search per superblock is the wall this root was taken to SAVE.
    //
    // Priced against the skip arm through the same `symbol_bits` path, so the
    // two numbers are comparable: `fixed` charged `skip` coded 1, and this
    // arm charges 0 instead and adds its three planes' coefficient bits.
    let mut chosen = (skip_cost, true, skip_trials);
    // Set when [`commit_inter_luma`] already wrote the residual arm's luma
    // into the plane, so the tail below does not commit over its units.
    let mut luma_committed = false;
    // The residual arm's luma levels and the `txfm_split` depth the writer
    // codes: depth 0 is the TX_64X64 corner in 32x32 coordinates, depth 1 is
    // four TX_32X32 units in 64x64 ones.
    let mut luma_levels: Option<(Vec<i32>, u8, Vec<TxType>)> = None;
    if residual {
        let sets = [TxbSet::Luma64, TxbSet::Chroma32, TxbSet::Chroma32];
        // Square by construction (the `debug_assert` above), so `units` is
        // one entry per plane and `chosen.2[plane]` is that plane's trial.
        let residual_trials = [0usize, 1, 2].map(|plane| {
            let (target, px, py, s) = if plane == 0 {
                (&*luma, x, y, bw)
            } else {
                (&chroma[plane - 1], x / 2, y / 2, bw / 2)
            };
            target.code_from_prediction(
                px,
                py,
                s,
                &chosen.2[plane].reconstruction,
                false,
                search.base_q_idx,
                search.deadzone,
                sets[plane],
            )
        });
        let unskip = skip_bits_at(stack, false) - skip_bits_at(stack, true);
        let coeff_bits: f64 = residual_trials.iter().map(|t| t.bits).sum();
        // lane-b64b: the root's own var-tx decision -- one TX_64X64 or the
        // four TX_32X32 units of `txfm_split = 1`, taken by the same
        // `commit_inter_luma` every other inter block's is (it commits the
        // winner and hands back the cost it took off the flat arm).
        let (levels, depth, gain, tx_types) = if b64_var_tx() {
            let second = info.ref1.and_then(|r1| {
                compound
                    .iter()
                    .find(|(r, _, _)| *r == r1)
                    .map(|&(_, g, _)| {
                        (
                            info.mv1,
                            (g.y.as_slice(), g.width, luma.true_width, luma.true_height),
                        )
                    })
            });
            luma_committed = true;
            commit_inter_luma(
                luma,
                (x, y),
                bw,
                info.mv,
                (
                    &reference.y,
                    reference.width,
                    luma.true_width,
                    luma.true_height,
                ),
                search,
                &residual_trials[0],
                false,
                second,
                fctx,
            )
        } else {
            (residual_trials[0].levels.clone(), 0, 0.0, Vec::new())
        };
        let cost = residual_trials.iter().map(|t| t.sse).sum::<f64>()
            + search.lambda * (syntax + unskip + coeff_bits)
            + gain;
        if cost < skip_cost {
            B64_RESIDUAL_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if depth == 1 {
                B64_VAR_TX_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            luma_levels = Some((levels, depth, tx_types));
            chosen = (cost, false, residual_trials.to_vec());
        }
    }

    let (cost, skip, trials) = chosen;
    // `commit_inter_luma` already wrote the residual arm's luma (its four
    // split units are not one 64-sided trial); a skip winner overwrites it.
    for (&(plane, px, py, s, _), trial) in units.iter().zip(&trials) {
        match plane {
            0 if skip || !luma_committed => luma.commit(px, py, s, trial),
            0 => {}
            _ => chroma[plane - 1].commit(px, py, s, trial),
        }
    }
    Some((
        cost,
        BlockCoeffs {
            skip,
            inter: Some(info),
            // The chroma grids are whole TX_32X32; the luma one is the
            // 64-point transform's coded 32x32 corner at depth 0 and the
            // whole 64-sided block at depth 1 (lane-b64b).
            tx_depth: luma_levels.as_ref().map_or(0, |(_, d, _)| *d),
            luma_tx_types: match (skip, &luma_levels) {
                (false, Some((_, _, types))) => types.clone(),
                _ => Vec::new(),
            },
            luma: match (skip, &luma_levels) {
                (true, _) => Vec::new(),
                (false, Some((levels, 1, _))) => coeffs(levels, SUPERBLOCK),
                (false, Some((levels, _, _))) => coeffs(levels, BLOCK),
                (false, None) => coeffs(&trials[0].levels, BLOCK),
            },
            u: if skip {
                Vec::new()
            } else {
                coeffs(&trials[1].levels, bw / 2)
            },
            v: if skip {
                Vec::new()
            } else {
                coeffs(&trials[2].levels, bw / 2)
            },
            ..BlockCoeffs::default()
        },
    ))
}

// Spec-shaped signature; bundling the operands would obscure the spec mapping.
#[allow(clippy::too_many_arguments)]
pub(super) fn search_inter_block(
    luma: &mut Plane,
    chroma: &mut [Plane; 2],
    (x, y): (usize, usize),
    search: &Search,
    mode_bits: &[f64; 13],
    reference: &Picture,
    stack: &MvStack,
    // `GOLDEN_FRAME`: the key frame's own reconstruction, held in DPB slot 1
    // for the whole GOP, with the stack this block derives against it. Only
    // its two search-free modes are priced (`NEARESTMV`, `GLOBALMV`) -- a
    // second motion search would double the wall of the stage that already
    // owns most of it.
    extra: &[(i8, &Picture, &MvStack)],
    // The COMPOUND stack of each `LAST_FRAME` + extra-reference pair, built by
    // the caller off the same MI grid the tile writer will (empty unless the
    // frame carries `reference_select`).
    compound: &[(i8, &Picture, crate::mvstack::CompoundMvStack)],
    // lane-av1obmc: the mi grid this search's neighbours were published into
    // (`record_mi`) and this block's own place in it -- what the OBMC
    // candidate's eligibility walk and neighbour plan read, the same state
    // the tile writer will read when it codes the `motion_mode` symbol.
    grid: &MiGrid,
    (mi_row, mi_col): (usize, usize),
    (mi_rows, mi_cols): (usize, usize),
    fctx: &crate::decode::FrameCtx,
) -> (BlockCoeffs, f64) {
    // `luma_set` is the INTRA candidates' table; the two inter candidates
    // below price their coefficients through `TxbSet::Luma32Inter`, which is
    // what `sb_coeff_inter_frame_tile` codes an inter block with (it carries
    // the inter `tx_type` symbol `Luma32` has no table for -- lane-av1rd1).
    let (luma_set, chroma_set) = (TxbSet::Luma32, TxbSet::Chroma16);
    let reach = Reach::of(BLOCK, x, y, luma.true_width, luma.true_height, fctx);

    // skip / intra_inter symbol costs at a fixed context -- see this
    // function's doc comment.
    let skip_bits = |skip: bool| skip_bits_at(stack, skip);
    let intra_inter_bits = |inter: bool| intra_inter_bits_at(stack, inter);

    struct Candidate {
        cost: f64,
        luma: Trial,
        u: Trial,
        v: Trial,
        mode: u8,
        skip: bool,
        inter: Option<InterInfo>,
        /// The mode/mv syntax this candidate pays for, apart from its
        /// residual -- what the OBMC candidate below has to re-pay when it
        /// re-prices the winner's prediction.
        mode_bits: f64,
    }
    let mut best: Option<Candidate> = None;
    let mut consider = |candidate: Candidate| {
        if best.as_ref().is_none_or(|b| candidate.cost < b.cost) {
            best = Some(candidate);
        }
    };

    // The chroma trial an intra candidate prices does not depend on `mode`
    // at all -- both planes always code `DC_PRED` here (a coding choice this
    // search inherited, not this loop's to second-guess) at the same `reach:
    // Reach::none()` -- so it is the same call with the same inputs on every
    // one of the thirteen iterations below; run it once and reuse the result
    // rather than repeat an already-priced transform/quantize/entropy call
    // twelve times for nothing (bit-identical: the value each iteration
    // reads back is exactly what it used to recompute).
    let u_trial = chroma[0].trial(
        At {
            x: x / 2,
            y: y / 2,
            side: BLOCK / 2,
            reach: Reach::none(),
            set: chroma_set,
        },
        DC_PRED,
        0,
        search.base_q_idx,
        search.deadzone,
        fctx,
    );
    let v_trial = chroma[1].trial(
        At {
            x: x / 2,
            y: y / 2,
            side: BLOCK / 2,
            reach: Reach::none(),
            set: chroma_set,
        },
        DC_PRED,
        0,
        search.base_q_idx,
        search.deadzone,
        fctx,
    );

    let intra_modes: Vec<u8> = match search.top_k {
        Some(k) if k < search.modes.len() => {
            let scored = luma.intra_scores(
                At {
                    x,
                    y,
                    side: BLOCK,
                    reach,
                    set: luma_set,
                },
                search.modes,
                mode_bits,
                search.lambda,
                fctx,
            );
            prune_by_sad(search.modes, scored, k)
        }
        _ => search.modes.to_vec(),
    };
    census_add(5, 1);
    census_add(
        6,
        if search.arf_leaf_intra {
            intra_modes.len()
        } else {
            0
        },
    );
    // lane-arfmode: arm 1 drops the whole-32 block's intra candidates at the
    // top ARF too -- the probe proved the ARF's intra AREA lives HERE, not at
    // the leaves (film A arm 1 vs control byte-identical with the leaf gate
    // alone), so "the 32x32 whole keeps intra as the scene-cut hatch" was a
    // hatch nothing else was using. Counted once per block.
    if search.arf_leaf_intra {
        for &mode in &intra_modes {
            let luma_trial = luma.trial(
                At {
                    x,
                    y,
                    side: BLOCK,
                    reach,
                    set: luma_set,
                },
                mode,
                0,
                search.base_q_idx,
                search.deadzone,
                fctx,
            );
            let u = u_trial.clone();
            let v = v_trial.clone();
            let cost = luma_trial.sse
                + u.sse
                + v.sse
                + search.lambda
                    * (luma_trial.bits
                        + u.bits
                        + v.bits
                        + mode_bits[usize::from(mode)]
                        + skip_bits(false)
                        + intra_inter_bits(false));
            consider(Candidate {
                cost,
                luma: luma_trial,
                u,
                v,
                mode,
                skip: false,
                inter: None,
                mode_bits: mode_bits[usize::from(mode)],
            });
        }
    } else {
        arfmode_hit(0);
    }

    // The reference frame buffer a spec decoder holds is exactly the true
    // (unpadded) coded extent -- a superblock whose partition stops early at
    // `has_cols`/`has_rows` never codes syntax for the padding columns/rows
    // beyond it, so those samples are never part of the decoded picture.
    // Motion compensation clamps reads to that true extent (spec 7.11.3.4's
    // reference sample position clamp), not to this crate's own
    // padded-to-`SUPERBLOCK` coding surface -- passing the padded width/height
    // here would let a fractional-pel read past the true edge pick up this
    // encoder's own uncoded padding instead of the true edge's replicated
    // value a decoder computes.
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
    // Every single-reference mode the writer can code (spec 5.11.24's symbol
    // chain, mirrored by `tile::write_inter_mode`), priced with the static
    // CDFs at this block's own stack contexts. A candidate is a motion
    // vector plus the cheapest syntax that reaches it, so two modes landing
    // on the same vector share one set of trials: the added modes cost
    // mode-bit arithmetic, not another motion compensation.
    let not_new = symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 1);
    let not_zero = symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 1);
    // `tile::write_drl_idx`'s cost: one `drl_mode` symbol per stack entry
    // past `start`, `1` to advance and `0` to stop, at most two. A `target`
    // the loop cannot reach is never offered below -- every candidate's
    // index is one this same loop walks to.
    let drl_bits = |start: usize, target: usize| drl_bits_for(stack, start, target);
    let last_ref_bits = single_ref_bits(crate::mvstack::LAST_FRAME, stack);
    let mut cands: Vec<((i32, i32), f64, InterInfo)> = vec![
        (
            stack.nearest_mv,
            last_ref_bits + not_new + not_zero + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 0),
            InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::NearestMv,
                mv: stack.nearest_mv,
                ref_mv_idx: 0,
            },
        ),
        // GLOBALMV is the identity model's zero vector -- this encoder writes
        // no `gm_params` -- and the cheapest syntax of the four on the static
        // content where that vector is also the right one.
        (
            (0, 0),
            last_ref_bits + not_new + symbol_bits(&cdf::ZERO_MV[stack.zero_mv_ctx], 0),
            InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::GlobalMv,
                mv: (0, 0),
                ref_mv_idx: 0,
            },
        ),
    ];
    let near_bits =
        last_ref_bits + not_new + not_zero + symbol_bits(&cdf::REF_MV[stack.ref_mv_ctx], 1);
    for idx in 1..=2usize {
        if let Some(e) = stack.entries.get(idx) {
            cands.push((
                e.mv,
                near_bits + drl_bits(1, idx),
                InterInfo {
                    ref1: None,
                    mv1: (0, 0),
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::NearMv,
                    mv: e.mv,
                    ref_mv_idx: idx as u8,
                },
            ));
        }
    }

    let source_block = luma.source_block(x, y, BLOCK);
    let (seeds, seed_n) = mv_seeds(stack);
    #[cfg(test)]
    let t = stage_start();
    let found = motion::search(
        ref_luma.0,
        ref_luma.1,
        ref_luma.2,
        ref_luma.3,
        &source_block,
        x,
        y,
        BLOCK,
        BLOCK,
        stack.pred_mv,
        &seeds[..seed_n],
        search.lambda,
        fctx,
        1,
    );
    #[cfg(test)]
    stage_since(0, t);
    let mv = round_to_valid_mv(found.mv, stack.pred_mv);
    // lane-newmv: this block's own `NEWMV` vector and what its mv syntax
    // costs, for the census below (`None` when the writer cannot name it).
    let mut newmv_syntax: Option<((i32, i32), f64)> = None;
    // Each DRL entry is a different `PredMv` (spec 7.10.2.10 `assign_mv`), so
    // the same vector costs a different residual from each: take the index
    // whose residual plus DRL bits is cheapest, not entry 0 by default.
    if let Some((bits, ref_mv_idx)) = best_new_mv_syntax(stack, mv) {
        newmv_syntax = Some((mv, bits));
        cands.push((
            mv,
            last_ref_bits
                + symbol_bits(&cdf::NEW_MV[stack.new_mv_ctx], 0)
                + bits
                + mv_rate_surcharge(bits),
            InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::NewMv,
                mv,
                ref_mv_idx,
            },
        ));
    }

    // Each extra reference's own `NEWMV` vector, for the compound
    // `NEW_NEWMV` candidate below to pair with `LAST`'s.
    let mut extra_new_mvs: Vec<(i8, (i32, i32))> = Vec::new();
    for &(ref_frame, g, gstack) in extra {
        let g_ref_bits = single_ref_bits(ref_frame, gstack);
        let g_not_new = symbol_bits(&cdf::NEW_MV[gstack.new_mv_ctx], 1);
        cands.push((
            gstack.nearest_mv,
            g_ref_bits
                + g_not_new
                + symbol_bits(&cdf::ZERO_MV[gstack.zero_mv_ctx], 1)
                + symbol_bits(&cdf::REF_MV[gstack.ref_mv_ctx], 0),
            InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame,
                mode: InterMode::NearestMv,
                mv: gstack.nearest_mv,
                ref_mv_idx: 0,
            },
        ));
        cands.push((
            (0, 0),
            g_ref_bits + g_not_new + symbol_bits(&cdf::ZERO_MV[gstack.zero_mv_ctx], 0),
            InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame,
                mode: InterMode::GlobalMv,
                mv: (0, 0),
                ref_mv_idx: 0,
            },
        ));
        // A motion search of this reference's own: without it, GOLDEN/ALTREF
        // can only be coded at a vector some neighbour already found, which
        // is why they carried a third of the inter blocks at their two
        // search-free modes alone.
        // The search is skipped when this reference's own NEARESTMV already
        // prices out `margin` times better than LAST's best vector does --
        // one candidate evaluation (`motion::cost_at`) standing in for a
        // whole search of ~50.
        // lane-last2: the SEARCH-FREE arm for the second past leaf picture
        // (`EC_AV1_LAST2_NEW=0`) -- LAST2 is then coded only at a vector some
        // neighbour already found, which is how GOLDEN/ALTREF were offered
        // before `EXTRA_REF_NEW_MV`, and it is what separates the reference's
        // own value from the second motion search's wall.
        let searched =
            extra_ref_new_mv() && (ref_frame != crate::mvstack::LAST2_FRAME || last2_new_mv());
        let margin = extra_new_skip_margin();
        if searched {
            newmv_census_global(0);
        }
        let skip_search = searched
            && margin > 0.0
            && motion::cost_at(
                &g.y,
                g.width,
                luma.true_width,
                luma.true_height,
                &source_block,
                x,
                y,
                BLOCK,
                BLOCK,
                gstack.nearest_mv,
                gstack.pred_mv,
                search.lambda,
                fctx,
            ) * margin
                <= found.cost;
        if skip_search {
            newmv_census_global(1);
        }
        if searched && !skip_search {
            let (gseeds, gn) = mv_seeds(gstack);
            #[cfg(test)]
            let t = stage_start();
            let gfound = motion::search(
                &g.y,
                g.width,
                luma.true_width,
                luma.true_height,
                &source_block,
                x,
                y,
                BLOCK,
                BLOCK,
                gstack.pred_mv,
                &gseeds[..gn],
                search.lambda,
                fctx,
                ref_distance(ref_frame),
            );
            #[cfg(test)]
            stage_since(0, t);
            let gmv = round_to_valid_mv(gfound.mv, gstack.pred_mv);
            extra_new_mvs.push((ref_frame, gmv));
            if let Some((bits, ref_mv_idx)) = best_new_mv_syntax(gstack, gmv) {
                cands.push((
                    gmv,
                    g_ref_bits + symbol_bits(&cdf::NEW_MV[gstack.new_mv_ctx], 0) + bits,
                    InterInfo {
                        ref1: None,
                        mv1: (0, 0),
                        ref_frame,
                        mode: InterMode::NewMv,
                        mv: gmv,
                        ref_mv_idx,
                    },
                ));
            }
        }
    }

    // The compound candidates (spec 5.11.24's `INTER_COMPOUND_MODES`), one
    // trial set each: both references predict the block and the two
    // predictions are averaged, which is what pays on content where neither
    // reference alone is right. Priced with the same static-CDF approximation
    // the single-reference candidates use, plus the compound-only symbols
    // (`comp_mode`, the reference-pair tree, `comp_group_idx`/`compound_idx`).
    // The two compound-mode arms, each measured alone on the BD gate before
    // it could default on. `NEAREST_NEWMV`/`NEW_NEARESTMV` keep (-1.0/-0.8
    // vs libaom, screen byte-identical) and ship on; `NEAR_NEARMV` does not
    // (+0.5/-0.2/flat) and stays behind `EC_AV1_COMP_NEARNEAR`.
    // RE-MEASURED on lane-av1rejudge at LAMBDA_SCALE 0.05 with warp on:
    // +16.6/-0.7, +47.0/+19.1, +51.3/-14.3 against the base's +16.7/-0.5,
    // +47.0/+19.1, +51.3/-14.3 -- one row 0.1/0.2 down, two flat, short of
    // the keep rule. Still behind the knob.
    let near_near = crate::envflags::env_flag!("EC_AV1_COMP_NEARNEAR");
    let half_new = true;
    for (ref1, g, cstack) in compound {
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
        // `NEAR_NEARMV`: both halves off compound stack entry 1 (the
        // decoder's `ref_mv_idx = 0`), which costs one DRL symbol whenever
        // the stack is deep enough for that walk to read one.
        if let Some(e) = cstack.entries.get(1).filter(|_| near_near) {
            let drl = if cstack.entries.len() > 2 {
                symbol_bits(&cdf::DRL_MODE[cstack.drl_ctx[1]], 0)
            } else {
                0.0
            };
            ccands.push((
                (e.mv0, e.mv1),
                mode_bits_of(1) + drl,
                InterInfo {
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::NearNearMv,
                    mv: e.mv0,
                    mv1: e.mv1,
                    ref1: Some(ref1),
                    ref_mv_idx: 0,
                },
            ));
        }
        // The half-new modes and `NEW_NEWMV`, all seeded from the two
        // single-reference searches this block already ran -- no joint search
        // of its own. Every residual is priced against stack entry 0, which
        // is what `assign_compound_mv` predicts from at `ref_mv_idx = 0`.
        let cbase = cstack
            .entries
            .first()
            .map_or(cstack.nearest_mv, |e| (e.mv0, e.mv1));
        if let Some(b0) = mv_residual_bits(mv, cbase.0).filter(|_| half_new) {
            ccands.push((
                (mv, cstack.nearest_mv.1),
                mode_bits_of(3) + b0,
                InterInfo {
                    ref_frame: crate::mvstack::LAST_FRAME,
                    mode: InterMode::NewNearestMv,
                    mv,
                    mv1: cstack.nearest_mv.1,
                    ref1: Some(ref1),
                    ref_mv_idx: 0,
                },
            ));
        }
        if let Some(&(_, mv1)) = extra_new_mvs.iter().find(|(r, _)| *r == ref1) {
            if let Some(b1) = mv_residual_bits(mv1, cbase.1).filter(|_| half_new) {
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
            }
            let base = cbase;
            if let (Some(b0), Some(b1)) =
                (mv_residual_bits(mv, base.0), mv_residual_bits(mv1, base.1))
            {
                ccands.push((
                    (mv, mv1),
                    mode_bits_of(7) + b0 + b1,
                    InterInfo {
                        ref_frame: crate::mvstack::LAST_FRAME,
                        mode: InterMode::NewNewMv,
                        mv,
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
            let luma_trial = mc_trial_compound(
                luma,
                x,
                y,
                BLOCK,
                mvs,
                true,
                (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
                g_luma,
                search.base_q_idx,
                search.deadzone,
                TxbSet::Luma32Inter,
                fctx,
            );
            let u = mc_trial_compound(
                &chroma[0],
                x / 2,
                y / 2,
                BLOCK / 2,
                mvs,
                false,
                (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3),
                g_u,
                search.base_q_idx,
                search.deadzone,
                chroma_set,
                fctx,
            );
            let v = mc_trial_compound(
                &chroma[1],
                x / 2,
                y / 2,
                BLOCK / 2,
                mvs,
                false,
                (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3),
                g_v,
                search.base_q_idx,
                search.deadzone,
                chroma_set,
                fctx,
            );
            let skip = luma_trial.levels.iter().all(|&l| l == 0)
                && u.levels.iter().all(|&l| l == 0)
                && v.levels.iter().all(|&l| l == 0);
            let cost = luma_trial.sse
                + u.sse
                + v.sse
                + search.lambda
                    * (skip_bits(skip)
                        + intra_inter_bits(true)
                        + bits
                        + if skip {
                            0.0
                        } else {
                            luma_trial.bits + u.bits + v.bits
                        });
            consider(Candidate {
                cost,
                luma: luma_trial,
                u,
                v,
                mode: DC_PRED,
                skip,
                mode_bits: bits,
                inter: Some(info),
            });
        }
    }

    // One trial set per distinct (reference, vector) pair: sort, cheapest
    // syntax first, and drop the rest of each pair's run.
    cands.sort_by(|a, b| {
        (a.2.ref_frame, a.0)
            .cmp(&(b.2.ref_frame, b.0))
            .then(a.1.total_cmp(&b.1))
    });
    cands.dedup_by_key(|c| (c.2.ref_frame, c.0));
    census_add(7, cands.len());
    // lane-newmv: the NEWMV candidate's own RD cost against the best cost
    // this block reaches WITHOUT it, taken inside the one loop that prices
    // them all (a second pricing pass would be a second oracle).
    let (mut new_cost, mut other_cost) = (f64::INFINITY, f64::INFINITY);
    {
        for (mv, mode_bits_inter, info) in cands {
            let (ref_luma, ref_u, ref_v) = match extra.iter().find(|(r, _, _)| *r == info.ref_frame)
            {
                Some(&(_, g, _)) => (
                    (&g.y, g.width, luma.true_width, luma.true_height),
                    (
                        &g.u,
                        g.width / 2,
                        chroma[0].true_width,
                        chroma[0].true_height,
                    ),
                    (
                        &g.v,
                        g.width / 2,
                        chroma[1].true_width,
                        chroma[1].true_height,
                    ),
                ),
                None => (ref_luma, ref_u, ref_v),
            };
            let luma_trial = mc_trial(
                luma,
                x,
                y,
                BLOCK,
                mv,
                true,
                ref_luma.0,
                ref_luma.1,
                ref_luma.2,
                ref_luma.3,
                false,
                search.base_q_idx,
                search.deadzone,
                TxbSet::Luma32Inter,
                fctx,
            );
            let u = mc_trial(
                &chroma[0],
                x / 2,
                y / 2,
                BLOCK / 2,
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
                BLOCK / 2,
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
            let cost = luma_trial.sse
                + u.sse
                + v.sse
                + search.lambda
                    * (skip_bits(skip)
                        + intra_inter_bits(true)
                        + mode_bits_inter
                        + if skip {
                            0.0
                        } else {
                            luma_trial.bits + u.bits + v.bits
                        });
            consider(Candidate {
                cost,
                luma: luma_trial,
                u,
                v,
                mode: DC_PRED,
                skip,
                mode_bits: mode_bits_inter,
                inter: Some(info),
            });
            if newmv_census_on() {
                if info.mode == InterMode::NewMv
                    && info.ref1.is_none()
                    && info.ref_frame == crate::mvstack::LAST_FRAME
                {
                    new_cost = cost;
                } else {
                    other_cost = other_cost.min(cost);
                }
            }
        }
    }
    if let Some((new_mv, bits)) = newmv_syntax {
        newmv_census_block(
            BLOCK,
            true,
            new_cost,
            other_cost,
            new_mv,
            stack.nearest_mv,
            bits,
        );
    } else {
        newmv_census_block(
            BLOCK,
            false,
            f64::INFINITY,
            other_cost,
            mv,
            stack.nearest_mv,
            0.0,
        );
    }

    let best = best.expect("the search offers at least the intra modes");
    // lane-av1obmc: the OBMC candidate (spec `motion_mode == OBMC_CAUSAL`),
    // offered to the SINGLE-REFERENCE winner only -- libaom's
    // `motion_mode_allowed` fails `is_motion_variation_allowed_compound` on a
    // compound block, so the writer codes no symbol for one. The prediction
    // is built by the DECODER's own plan and blend
    // ([`crate::decode::obmc_plan`]/[`crate::decode::obmc_run`],
    // `av1_build_obmc_inter_prediction`) off the committed neighbours' mvs
    // this search's `grid` already carries, so what is priced here is what a
    // decoder reconstructs -- never a second implementation of the blend.
    //
    // MEASURED 2026-09-06 and NOT KEPT ON BY DEFAULT (`EC_AV1_OBMC=1` turns it
    // on). Native crop BD, the standing recipe, with it on against the same
    // build with it off: film 1080p +18.2/+0.9 against +18.0/+0.7, film 2160p
    // +47.3/+19.2 against +47.3/+19.2, screen +59.1/-10.0 against
    // +59.4/-10.0. That is one row down (screen -0.3 vs libaom), one flat and
    // one UP -- the keep rule wants two down and one flat, so it stays off.
    // The 640x384 gate reads the same shape louder: +79.7/+95.5/+54.2 and
    // +37.2/+56.8/+0.8 against +78.8/+95.1/+54.5 and +36.1/+56.4/+1.1. Two
    // things bound it and are the upgrade path: only the 32x32 winner is
    // offered OBMC (the 16x16/8x8 leaves in `code_square_inter` are not, and
    // that is where libaom cashes most of its OBMC gain), and the symbol/
    // residual price here is the static-CDF estimate.
    let mut best = best;
    let mut motion_won = 0u8;
    if let Some(info) = best
        .inter
        .filter(|i| i.ref1.is_none() && (crate::envflags::env_flag!("EC_AV1_OBMC") || warp_on()))
    {
        let mut refs: [Option<&Picture>; 8] = [None; 8];
        refs[LAST_FRAME as usize] = Some(reference);
        for &(r, p, _) in extra {
            refs[r as usize] = Some(p);
        }
        // This block's OWN reference planes: the single-reference winner may
        // be one of the extra references, not LAST.
        let planes: [(&[u16], usize, usize, usize); 3] =
            match extra.iter().find(|(r, _, _)| *r == info.ref_frame) {
                Some(&(_, g, _)) => [
                    (g.y.as_slice(), g.width, luma.true_width, luma.true_height),
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
                None => [
                    (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
                    (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3),
                    (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3),
                ],
            };
        // The alphabet `tile::write_motion_mode` will code this block's
        // symbol against, mirrored term for term (libaom
        // `motion_mode_allowed`).
        let warp_alphabet = warp_on()
            && crate::decode::num_proj_ref(
                grid,
                mi_row,
                mi_col,
                8,
                8,
                mi_cols,
                mi_rows,
                info.ref_frame,
                fctx,
            ) >= 1;
        let mm = |m: u8| motion_mode_bits(BLOCK, BLOCK, m, warp_alphabet);
        let mut candidates: Vec<(u8, [Vec<u8>; 3])> = Vec::new();
        if crate::envflags::env_flag!("EC_AV1_OBMC")
            && let Some(pred) = obmc_prediction(
                grid,
                (mi_row, mi_col),
                (mi_rows, mi_cols),
                (x, y),
                BLOCK,
                info.mv,
                &refs,
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
                BLOCK,
                info.mv,
                info.ref_frame,
                planes,
                fctx,
            ) {
                candidates.push((2, pred));
            }
        for (motion, pred) in candidates {
            let luma_trial = luma.code_from_prediction(
                x,
                y,
                BLOCK,
                &pred[0],
                false,
                search.base_q_idx,
                search.deadzone,
                TxbSet::Luma32Inter,
            );
            let u = chroma[0].code_from_prediction(
                x / 2,
                y / 2,
                BLOCK / 2,
                &pred[1],
                false,
                search.base_q_idx,
                search.deadzone,
                chroma_set,
            );
            let v = chroma[1].code_from_prediction(
                x / 2,
                y / 2,
                BLOCK / 2,
                &pred[2],
                false,
                search.base_q_idx,
                search.deadzone,
                chroma_set,
            );
            let skip = luma_trial.levels.iter().all(|&l| l == 0)
                && u.levels.iter().all(|&l| l == 0)
                && v.levels.iter().all(|&l| l == 0);
            let cost = luma_trial.sse
                + u.sse
                + v.sse
                + search.lambda
                    * (skip_bits(skip)
                        + intra_inter_bits(true)
                        + best.mode_bits
                        + mm(motion)
                        + if skip {
                            0.0
                        } else {
                            luma_trial.bits + u.bits + v.bits
                        });
            if cost < best.cost + search.lambda * mm(motion_won) {
                motion_won = motion;
                best = Candidate {
                    cost,
                    luma: luma_trial,
                    u,
                    v,
                    mode: DC_PRED,
                    skip,
                    mode_bits: best.mode_bits,
                    inter: best.inter,
                };
            }
        }
    }
    // The transform-depth search of a 32x32 winner (`EC_AV1_TX32_DEPTH`,
    // default OFF): the four 16x16 units of `commit_inter_luma` against the
    // flat trial, exactly as a 16x16 leaf is offered. lane-av1txbits measured
    // it flat-to-worse at LAMBDA_SCALE 0.1; lane-av1txdepth REBUILT it and
    // RE-MEASURED it at 0.05, where the rate weight is half of what rejected
    // it, and it is worse again on the deciding native table: film 1080p
    // +16.6/-0.5, film 2160p +46.6/+18.8, screen +50.9/-14.3 against the
    // +16.2/-1.1, +46.6/+18.8, +51.3/-14.4 of the same build with it off --
    // 1080p up 0.4/0.6, one row flat, screen 0.4 down against libaom and 0.1
    // up against rav1e. (640x384 reads it slightly the other way, +70.0/
    // +85.0/+48.4 and +28.3/+47.2/-3.6 against +70.4/+85.3/+48.5 and
    // +28.4/+47.4/-4.0 -- the native table decides.)
    //
    // NOT blindness: the split wins 62.3% of the 1080p film's 32x32 blocks,
    // 70.9% of the 4K film's and 53.5% of the screen's. NOT mis-pricing
    // either, and the census names the row: the split units' own set
    // `Luma16Inter` is priced within 1.4% of what it is written at in every
    // bucket from two non-zeros up, and its ZERO bucket (335 units) is priced
    // +30.1% ABOVE the written bits -- an error that biases AGAINST splitting,
    // so a perfect pricer would split even more, and more splitting is what
    // measured worse. The four extra txb_skip/eob symbols simply cost more
    // ladder bytes than the local RD saves. The flat 32x32 transform stays.
    // lane-av1txdepth REBUILT that search at LAMBDA_SCALE 0.05 (the rejection
    // above was measured at 0.1), behind `EC_AV1_TX32_DEPTH`, and gave the
    // compound winner the same offer behind `EC_AV1_COMP_VARTX`. An OBMC/warp
    // winner still commits flat: `commit_inter_luma` re-predicts each unit by
    // plain translation, which is not the prediction it was priced from.
    let (levels, tx_depth, dcost, luma_tx_types) = match best.inter.filter(|_| motion_won == 0) {
        Some(info) => {
            let own = match extra.iter().find(|(r, _, _)| *r == info.ref_frame) {
                Some(&(_, g, _)) => (g.y.as_slice(), g.width, luma.true_width, luma.true_height),
                None => (ref_luma.0.as_slice(), ref_luma.1, ref_luma.2, ref_luma.3),
            };
            let second = info.ref1.and_then(|r| {
                compound.iter().find(|(c, _, _)| *c == r).map(|&(_, g, _)| {
                    (
                        info.mv1,
                        (g.y.as_slice(), g.width, luma.true_width, luma.true_height),
                    )
                })
            });
            match info.ref1.is_some() && second.is_none() {
                true => {
                    luma.commit(x, y, BLOCK, &best.luma);
                    (best.luma.levels.clone(), 0, 0.0, Vec::new())
                }
                false => commit_inter_luma(
                    luma,
                    (x, y),
                    BLOCK,
                    info.mv,
                    own,
                    search,
                    &best.luma,
                    best.skip,
                    second,
                    fctx,
                ),
            }
        }
        None => {
            luma.commit(x, y, BLOCK, &best.luma);
            (best.luma.levels.clone(), 0, 0.0, Vec::new())
        }
    };
    // lane-txrd: chroma follows the luma type the var-tx/type search chose.
    let [bu, bv] = match (
        best.inter.filter(|_| motion_won == 0),
        luma_tx_types.first().copied(),
    ) {
        (Some(info), Some(t)) => {
            let (ou, ov) = match extra.iter().find(|(r, _, _)| *r == info.ref_frame) {
                Some(&(_, g, _)) => (
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
                ),
                None => (
                    (ref_u.0.as_slice(), ref_u.1, ref_u.2, ref_u.3),
                    (ref_v.0.as_slice(), ref_v.1, ref_v.2, ref_v.3),
                ),
            };
            let sec = info.ref1.and_then(|r| {
                compound.iter().find(|(c, _, _)| *c == r).map(|&(_, g, _)| {
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
            });
            recode_inter_chroma(
                chroma,
                (x, y),
                BLOCK,
                info.mv,
                [ou, ov],
                sec,
                search,
                chroma_set,
                t,
                fctx,
            )
            .unwrap_or([best.u, best.v])
        }
        _ => [best.u, best.v],
    };
    let best = Candidate {
        u: bu,
        v: bv,
        ..best
    };
    chroma[0].commit(x / 2, y / 2, BLOCK / 2, &best.u);
    chroma[1].commit(x / 2, y / 2, BLOCK / 2, &best.v);
    (
        BlockCoeffs {
            luma_tx_types,
            angle_delta_y: 0,
            cfl_alphas: None,
            filter_intra: None,
            luma: coeffs(&levels, BLOCK),
            u: coeffs(&best.u.levels, BLOCK / 2),
            v: coeffs(&best.v.levels, BLOCK / 2),
            mode: best.mode,
            uv_mode: DC_PRED,
            skip: best.skip,
            inter: best.inter,
            motion_mode: motion_won,
            eight: None,
            dv: None,
            palette: None,
            palette_uv: None,
            tx_depth,
        },
        best.cost + dcost,
    )
}
