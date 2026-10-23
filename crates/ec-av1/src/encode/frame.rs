use super::*;

/// lane-av1tpl: the strength `k` of the temporal lambda weighting
/// ([`tpl_lambda_factors`]). 0 keeps every block at the frame's own lambda, which
/// is what every measurement before this lane was taken on. Swept by
/// `EC_AV1_TPL=<k>`.
///
/// SWEPT on lane-av1tpl at the native gate (1920x1024, 12 frames, BD-rate vs
/// libaom `cpu-used 6` / rav1e `speed 6`); `k=0` reproduces the pre-lane
/// baseline exactly, which is the control the rest of the table is read
/// against:
///
/// | k | film 1080p | film 2160p | screen |
/// |---|---|---|---|
/// | 0 | +16.2/-1.1 | +46.6/+18.8 | +51.3/-14.4 |
/// | 0.25 | +16.4/-0.7 | +46.6/+18.8 | +51.2/-14.4 |
/// | **0.5** | **+15.9/-1.3** | **+46.5/+18.7** | **+51.2/-14.4** |
/// | 1 | +16.2/-0.9 | +46.6/+18.8 | +51.2/-14.4 |
/// | 2 | +16.5/-0.7 | +46.7/+19.0 | +51.1/-14.4 |
///
/// 0.5 is the only arm that is at least flat on every row and down on five of
/// the six columns, so it ships as the default. The lookahead pass that feeds
/// it costs 1.5 ms per 1920x1024 frame against a ~0.8 s encode (0.2%).
pub(super) const TPL_STRENGTH: f64 = 0.5;

/// lane-deltaq: the per-superblock quantizer, `delta_q_present` (spec
/// 5.9.17/5.11.10). `EC_AV1_DELTAQ=0` switches it off (the stream is then
/// byte-identical to the pre-lane one); anything else is the `delta_q_res`
/// LOG2, so `2` is a step of 4 and `3` a step of 8. `None` = off.
///
/// A frame with no tpl map (no lookahead picture, or `EC_AV1_TPL=0`) has
/// nothing to vary the quantizer BY and codes no delta syntax at all.
pub(super) fn deltaq_res_log2() -> Option<u8> {
    match DELTAQ_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        u8::MAX => {}
        4 => return None,
        r => return Some(r),
    }
    match crate::envflags::var("EC_AV1_DELTAQ")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("0") => None,
        Some(v) => v.parse::<u8>().ok().filter(|r| *r <= 3),
        None => Some(crate::speed::at(&crate::speed::DELTAQ_RES)).filter(|r| *r < 4),
    }
}

/// A PROCESS-GLOBAL [`deltaq_res_log2`] override that wins over both the
/// environment and the preset -- `u8::MAX` = unset, `4` = off. The gate that
/// sets it holds [`crate::speed::knob_write`], the same exclusive lock the
/// speed presets take, because the streams every other test pins move under
/// it (the shipped default is OFF, see [`crate::speed::DELTAQ_RES`]).
pub(super) static DELTAQ_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(u8::MAX);

/// Sets [`DELTAQ_OVERRIDE`]; `None` clears it.
#[allow(dead_code)] // set only from the `#[cfg(test)]` gates
pub(crate) fn set_deltaq_res(res_log2: Option<u8>) {
    DELTAQ_OVERRIDE.store(
        res_log2.unwrap_or(u8::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// How much of the derived qindex step is actually taken
/// (`EC_AV1_DELTAQ_K`, default 1.0 = the full derivation).
pub(super) fn deltaq_k() -> f64 {
    crate::envflags::var("EC_AV1_DELTAQ_K")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|k| k.is_finite() && *k >= 0.0)
        .unwrap_or(1.0)
}

/// The largest qindex the map may move a superblock by, either way
/// (`EC_AV1_DELTAQ_CLAMP`, default 16 -- libaom clamps its own objective
/// deltaq around a quarter of the base qindex).
pub(super) fn deltaq_clamp() -> i32 {
    crate::envflags::var("EC_AV1_DELTAQ_CLAMP")
        .ok()
        .and_then(|v| v.trim().parse::<i32>().ok())
        .filter(|c| (0..=63).contains(c))
        .unwrap_or(16)
}

/// The qindex a lambda factor `f` asks for, derived rather than fitted.
///
/// The tpl map hands out a factor on LAMBDA, and RD lambda goes as the
/// square of the quantizer step (`encode_inter_frame`'s own
/// `lambda = .. * step * step`), so the step this superblock wants is
/// `sqrt(f)` times the frame's. `ac_q` is not linear in the index, so the
/// step ratio is inverted through the table itself -- the index whose `ac_q`
/// is closest to `ac_q(base) * sqrt(f)` -- rather than through a fitted
/// constant. `k` scales the resulting step (1.0 takes all of it), `clamp`
/// bounds it, and the answer is snapped onto the `res` grid so that every
/// superblock's qindex is congruent to `base` and the coded delta is a whole
/// number of steps.
pub(super) fn deltaq_for_factor(base: u8, f: f64, k: f64, clamp: i32, res: i32) -> u8 {
    let base_ac = f64::from(ac_q(8, i32::from(base)));
    let target = base_ac * f.max(1e-6).sqrt();
    let exact = (1..=255)
        .min_by(|&a, &b| {
            let d = |q: i32| (f64::from(ac_q(8, q)) - target).abs();
            d(a).partial_cmp(&d(b)).expect("ac_q is finite")
        })
        .expect("the qindex range is not empty")
        - i32::from(base);
    let want = (exact as f64 * k).round() as i32;
    // Snapped to the `res` grid BEFORE the clamp: `clamp` is a qindex bound
    // and need not be a whole number of steps, and clamping afterwards would
    // hand back a qindex that is not congruent to `base`.
    let max_steps = clamp / res;
    let steps = (want as f64 / f64::from(res)).round() as i32;
    let delta = steps.clamp(-max_steps, max_steps) * res;
    (i32::from(base) + delta).clamp(1, 255) as u8
}

/// lane-dq: derive the per-superblock qindex libaom's `deltaq-mode 1`
/// objective would, from the SAME tpl dependence ratio our own map already
/// carries (`EC_AV1_DQ_TPL=1`).
///
/// libaom (`av1_get_q_for_deltaq_objective`, encodeframe_utils.c) forms a
/// superblock dependence ratio `rk = exp((intra_cost - mc_dep_cost)/w)` in the
/// LOG domain, divides the frame-level `r0` (`encoder_utils.c:627`, the very
/// same expression at frame scope) by it, and asks
/// `av1_get_deltaq_offset(bit_depth, base, beta)` for the qindex whose
/// **DC** step is `dc_q(base)/sqrt(beta)`. With per-cell costs `intra` and
/// `intra + mc_dep`, `rk` is `1/(1 + r)` for this crate's `r = mc_dep/intra`,
/// so `beta = (1 + r_sb) / (1 + r_frame)` with `r_frame` read the log-mean way
/// libaom reads `r0`. The offset is clamped to libaom's own
/// `+-(delta_q_res * 9 - 1)`, which at `res = 4` is +-8 steps -- more than
/// twice the +-16 qindex [`deltaq_for_factor`] allows.
///
/// Two things differ from [`deltaq_for_factor`] and both are the point of the
/// arm: the table inverted is `dc_q` not `ac_q`, and the ratio is read raw
/// (`sqrt(1 + r)`) rather than through the map's `log2`-compressed,
/// `TPL_POWER`-flattened, mean-normalised lambda factor -- so this mapping is
/// much stronger for the same map. `strength` scales the offset.
pub(super) fn deltaq_libaom(base: u8, r_sb: f64, r_frame: f64, strength: f64, res: i32) -> u8 {
    let beta = ((1.0 + r_sb.max(0.0)) / (1.0 + r_frame.max(0.0))).max(1e-6);
    let target = f64::from(crate::quant::dc_q(8, i32::from(base))) / beta.sqrt();
    let exact = (1..=255)
        .min_by(|&a, &b| {
            let d = |q: i32| (f64::from(crate::quant::dc_q(8, q)) - target).abs();
            d(a).partial_cmp(&d(b)).expect("dc_q is finite")
        })
        .expect("the qindex range is not empty")
        - i32::from(base);
    let want = exact as f64 * strength;
    // Snapped onto the `res` grid first, so the coded delta is a whole number
    // of steps, then bounded by libaom's own `res * 9 - 1`.
    let max_steps = (res * 9 - 1) / res;
    let steps = (want / f64::from(res)).round() as i32;
    let delta = steps.clamp(-max_steps, max_steps) * res;
    (i32::from(base) + delta).clamp(1, 255) as u8
}

/// The frame-level `r0` libaom's objective divides by: the log-mean of
/// `1 + r` over the frame's cells, minus 1.
pub(super) fn tpl_frame_ratio(ratio: &[f64]) -> f64 {
    if ratio.is_empty() {
        return 0.0;
    }
    let m = ratio.iter().map(|&r| (1.0 + r.max(0.0)).ln()).sum::<f64>() / ratio.len() as f64;
    m.exp() - 1.0
}

/// The [`deltaq_libaom`] strength, or `None` to keep the shipped
/// [`deltaq_for_factor`] derivation.
///
/// `EC_AV1_DQ_TPL=0`/`1` forces the mapping either way and `EC_AV1_DQ_TPL_K`
/// the strength (default 1.0 = libaom's own offset when the flag armed it);
/// with neither set the preset decides, see [`crate::speed::DQ_TPL_K`].
pub(super) fn dq_tpl_strength(level: DqLevel) -> Option<f64> {
    let m = dq_level_k()[level as usize];
    let env_k = crate::envflags::var("EC_AV1_DQ_TPL_K")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|k| k.is_finite() && *k >= 0.0);
    let base = match crate::envflags::var("EC_AV1_DQ_TPL")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("0") => return None,
        Some(_) => Some(env_k.unwrap_or(1.0)),
        None => env_k.or_else(|| Some(crate::speed::at(&crate::speed::DQ_TPL_K))),
    };
    base.map(|k| k * m).filter(|k| *k > 0.0)
}

/// lane-arfq: which pyramid level the frame asking for a
/// [`dq_tpl_strength`] sits at. The KEY frame is not here on purpose: it
/// codes no `delta_q` syntax at all (the whole lever lives in
/// [`encode_inter_frame`]), so a per-level strength cannot touch it -- the
/// key's allocation is [`crate::encoder::Pyramid::key_q_offset`]'s job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DqLevel {
    /// The mini-GOP's own hidden `ALTREF`, which every other frame of the
    /// group predicts from (directly or through the mid level).
    TopArf = 0,
    /// A hidden mid/quarter-point `ALTREF`, read by the leaves around it.
    MidArf = 1,
    /// A shown leaf -- and every frame of the flat, non-pyramid path.
    Leaf = 2,
}

/// The per-level multiplier on the [`dq_tpl_strength`] the preset carries:
/// `EC_AV1_DQ_K=<top arf>:<mid arf>:<leaf>`, else
/// [`crate::speed::DQ_LEVEL_K`]. Three fields, not four: the key frame codes
/// no `delta_q` (see [`DqLevel`]).
pub(super) fn dq_level_k() -> [f64; 3] {
    dq_level_k_of(crate::envflags::var("EC_AV1_DQ_K").ok().as_deref())
}

/// [`dq_level_k`]'s parse, pure: a missing spec, a missing field and an
/// unparseable one all keep [`crate::speed::DQ_LEVEL_K`]'s entry.
pub(super) fn dq_level_k_of(spec: Option<&str>) -> [f64; 3] {
    let mut k = crate::speed::DQ_LEVEL_K;
    if let Some(spec) = spec {
        for (slot, field) in k.iter_mut().zip(spec.split(':')) {
            if let Some(v) = field
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v: &f64| v.is_finite() && *v >= 0.0)
            {
                *slot = v;
            }
        }
    }
    k
}

/// `EC_AV1_DQ_CENSUS=1`: per inter frame, one line comparing the offsets the
/// shipped mapping ([`deltaq_for_factor`]) WOULD emit against the ones
/// libaom's objective ([`deltaq_libaom`]) gives for the same superblocks and
/// the same tpl map -- and which of the two the frame actually coded.
pub(super) fn dq_census() -> bool {
    crate::envflags::var("EC_AV1_DQ_CENSUS")
        .ok()
        .as_deref()
        .map(str::trim)
        == Some("1")
}

/// The histogram/extent summary [`dq_census`] prints for one set of offsets.
pub(super) fn dq_summary(offsets: &[i32]) -> String {
    let n = offsets.len().max(1);
    let (lo, hi) = offsets
        .iter()
        .fold((0i32, 0i32), |(l, h), &o| (l.min(o), h.max(o)));
    let nz = offsets.iter().filter(|&&o| o != 0).count();
    let mean = offsets.iter().map(|&o| f64::from(o)).sum::<f64>() / n as f64;
    let absmean = offsets.iter().map(|&o| f64::from(o.abs())).sum::<f64>() / n as f64;
    let mut levels: Vec<i32> = offsets.to_vec();
    levels.sort_unstable();
    levels.dedup();
    format!(
        "range {lo}..{hi} mean {mean:+.2} |mean| {absmean:.2} nonzero {nz}/{} levels {}",
        offsets.len(),
        levels.len()
    )
}

/// How many DISTINCT per-superblock quantizer levels the last inter frame's
/// [`sb_q`](encode_inter_frame) grid carried -- lane-deltaq's fire counter
/// (class `gate-blind-to-feature`: every exactness assert below passes on a
/// grid that is one flat value, i.e. on no delta_q at all). Process-global
/// like [`TX_DEPTH_HITS`], and read through [`take_deltaq_levels`], which
/// keeps the LARGEST count seen since the last read.
pub(super) static DELTAQ_LEVELS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// [`DELTAQ_LEVELS`], reset to 0.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn take_deltaq_levels() -> usize {
    DELTAQ_LEVELS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// This superblock's search state: the quantizer it is coded at, and the
/// lambda that quantizer implies. RD lambda goes as the square of the step,
/// which is exactly the relation [`deltaq_for_factor`] inverted, so with no
/// `sb_q` grid the caller keeps the tpl LAMBDA factor alone (the behaviour
/// before lane-deltaq).
pub(super) fn sb_q_search<'m>(
    sb_q: &[u8],
    sb_cols: usize,
    base_q_idx: u8,
    (sb_r, sb_c): (usize, usize),
    base: Search<'m>,
) -> Search<'m> {
    let q = sb_q[sb_r * sb_cols + sb_c];
    let ratio = f64::from(ac_q(8, i32::from(q))) / f64::from(ac_q(8, i32::from(base_q_idx)));
    Search {
        base_q_idx: q,
        lambda: base.lambda * ratio * ratio,
        ..base
    }
}

pub(super) fn tpl_strength() -> f64 {
    std::env::var("EC_AV1_TPL")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|k| k.is_finite() && *k >= 0.0)
        .unwrap_or(TPL_STRENGTH)
}

/// lane-av1tpl2: how many SOURCE pictures the lambda map looks at, counting
/// the frame being coded. 1 turns the weighting off; 2 is the shipped
/// one-frame area map ([`tpl_area_map`]); 3 and up only mean anything under
/// [`tpl_propagating`], which is the map that can actually use them and is
/// the default since lane-av1tpl3, so the default window is 8. Swept by
/// `EC_AV1_TPL_D=<d>`.
///
/// The streaming facade holds `depth - 1` pictures back so it feeds
/// `encode_inter_frame` the same window `encode_sequence` does; both read
/// THIS function, so the two paths cannot disagree about the window.
pub(crate) const TPL_DEPTH: usize = 8;

/// The exponent the dependence ratio is read through ([`tpl_lambda_factors`]).
/// 0.5 is libaom's own square-root compression of its dependence ratio.
pub(super) const TPL_POWER: f64 = 0.5;

/// lane-av1tpl2: the propagating map ([`tpl_lambda_factors`]) instead of the
/// one-frame area map ([`tpl_area_map`]), behind `EC_AV1_TPL_PROP=1`.
///
/// MEASURED at the native gate (1920x1024, 12 frames, BD-rate vs libaom
/// `cpu-used 6` / rav1e `speed 6`), against the shipped one-frame map
/// (+15.9/-1.3, +46.5/+18.7, +51.2/-14.4). `p` is [`TPL_POWER`]:
///
/// | k | D | p | film 1080p | film 2160p | screen |
/// |---|---|---|---|---|---|
/// | 0.0625 | 8 | 1 | +15.7/-1.6 | +46.6/+18.8 | +51.1/-14.5 |
/// | 0.125 | 4 | 1 | +15.7/-1.6 | +46.7/+18.9 | +51.2/-14.5 |
/// | 0.125 | 8 | 1 | +15.9/-1.3 | +46.8/+19.0 | +50.7/-14.7 |
/// | 0.25 | 4 | 1 | +15.5/-1.8 | +46.8/+19.1 | +51.2/-14.5 |
/// | 0.25 | 8 | 1 | +15.7/-1.6 | +46.9/+19.1 | +50.7/-14.6 |
/// | 0.5 | 4 | 1 | +16.1/-1.2 | +47.0/+19.2 | +51.1/-14.6 |
/// | 0.5 | 8 | 1 | +16.0/-1.4 | +47.1/+19.3 | +51.8/-13.9 |
/// | 1 | 4 | 1 | +16.2/-1.3 | +47.2/+19.4 | +51.9/-13.9 |
/// | 2 | 4 | 1 | +17.2/-0.5 | +47.2/+19.5 | +57.0/-10.7 |
/// | **0.5** | **8** | **0.5** | **+15.5/-1.7** | **+46.7/+19.0** | **+50.8/-14.6** |
/// | 0.5 | 4 | 0.5 | +15.6/-1.6 | +46.8/+19.0 | +51.0/-14.6 |
/// | 1 | 8 | 0.5 | +15.9/-1.5 | +47.0/+19.2 | +51.0/-14.5 |
///
/// The map is genuinely non-flat (0.25..1.12 across 7680 cells, only 58 of
/// them within 0.9..1.1, where the one-frame map had 7127 of 7680 inside
/// 1..1.5x its own mean), and its best arm was 0.4 down on both film 1080p
/// columns and 0.4/0.2 down on screen -- but EVERY arm of that lane was
/// 0.1..0.4 UP on film 2160p, so it shipped off.
///
/// lane-av1tpl3 found why and turned it on. The denominator is the cell's own
/// MAD from its DC, and on the gate's film clips (ffmpeg `testsrc2`, colour
/// bars) 89.4% (1080p) / 90.8% (2160p) of all 16x16 cells have a MAD of
/// EXACTLY ZERO -- so a per-16x16 ratio is a huge number divided by 1
/// wherever the window leans on a flat bar, which is exactly where coding is
/// cheap. Summing intra and mc_dep over the 64x64 superblock before the ratio
/// (libaom's own granularity) puts a busy cell's denominator next to the flat
/// one's, and reading the ratio through `log2(1 + r)` (libaom's compressed
/// beta) flattens what is left of the tail. Both on, at k=0.5 D=8 p=0.5, on
/// the same native gate (baseline = the shipped one-frame map):
///
/// | arm | film 1080p | film 2160p | screen |
/// |---|---|---|---|
/// | one-frame map (was default) | +15.9/-1.3 | +46.5/+18.7 | +51.2/-14.4 |
/// | propagating, per-16x16 | +15.5/-1.7 | +46.7/+19.0 | +50.8/-14.6 |
/// | + denominator floor 0.25x mean | +15.7/-1.5 | +46.9/+19.1 | +50.9/-14.6 |
/// | + floor 0.5x mean | +15.7/-1.5 | +46.8/+19.0 | +50.8/-14.5 |
/// | + floor 1.0x mean | +15.8/-1.4 | +46.9/+19.1 | +51.0/-14.5 |
/// | + 64x64 aggregation | +15.9/-1.3 | +46.6/+18.8 | +51.1/-14.6 |
/// | + log2 beta | +16.0/-1.3 | +46.9/+19.0 | +51.0/-14.6 |
/// | + aggregation + floor 0.25x | +15.7/-1.5 | +46.6/+18.9 | +50.9/-14.6 |
/// | + aggregation + floor 0.5x | +16.0/-1.3 | +46.9/+19.1 | +50.8/-14.7 |
/// | **+ aggregation + log2 beta** | **+15.6/-1.7** | **+46.5/+18.8** | **+51.0/-14.5** |
///
/// The last row is the only arm that is down on two rows (film 1080p 0.3/0.4,
/// screen 0.2/0.1) and flat on the third (film 2160p 0.0/+0.1), so it ships
/// as the default: `EC_AV1_TPL_PROP=0` goes back to the one-frame map. The
/// floor is a real effect on the ratio and never a win once the aggregation
/// is in -- the aggregation is the same fix, done where libaom does it.
pub(super) fn tpl_propagating() -> bool {
    std::env::var("EC_AV1_TPL_PROP").ok().as_deref() != Some("0")
}

pub(crate) fn tpl_depth() -> usize {
    std::env::var("EC_AV1_TPL_D")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|d| *d >= 1)
        .unwrap_or_else(|| crate::speed::at(&crate::speed::TPL_DEPTH))
        .max(1)
}

/// Nanoseconds the lookahead pass ([`tpl_lambda_factors`]) has spent this
/// process, so the gate can price it against the encode it rides on.
pub(super) static TPL_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One coarse full-pel 16x16 motion pass of `next` against `cur`: per cell,
/// the winning displacement and its SAD.
///
/// Deterministic: integer SADs, a fixed 3-step pattern seeded at zero and at
/// the left/above winners, first-best tie-break. Built once on the frame's
/// own thread and read-only afterwards, so it cannot make a tile's bytes
/// depend on the thread count.
pub(super) fn tpl_coarse_pass(cur: &[u8], next: &[u8], width: usize, height: usize) -> Vec<((i32, i32), u32)> {
    let (cells_x, cells_y) = (width / 16, height / 16);
    let sad = |bx: usize, by: usize, (mx, my): (i32, i32)| -> u32 {
        let (sx, sy) = (bx as i32 + mx, by as i32 + my);
        if sx < 0 || sy < 0 || sx as usize + 16 > width || sy as usize + 16 > height {
            return u32::MAX;
        }
        let (sx, sy) = (sx as usize, sy as usize);
        let mut sum = 0u32;
        for r in 0..16 {
            let a = &next[(by + r) * width + bx..][..16];
            let b = &cur[(sy + r) * width + sx..][..16];
            sum += a
                .iter()
                .zip(b)
                .map(|(&p, &q)| u32::from(p.abs_diff(q)))
                .sum::<u32>();
        }
        sum
    };
    let mut out = Vec::with_capacity(cells_x * cells_y);
    let mut above = vec![(0i32, 0i32); cells_x];
    for cy in 0..cells_y {
        let mut left = (0i32, 0i32);
        // Index loop: `above` is read per column beside the block origin; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for cx in 0..cells_x {
            let (bx, by) = (cx * 16, cy * 16);
            let mut best = (sad(bx, by, (0, 0)), (0i32, 0i32));
            for cand in [left, above[cx]] {
                if cand != (0, 0) {
                    let s = sad(bx, by, cand);
                    if s < best.0 {
                        best = (s, cand);
                    }
                }
            }
            for step in [8i32, 4, 2, 1] {
                let centre = best.1;
                for (dx, dy) in [(step, 0), (-step, 0), (0, step), (0, -step)] {
                    let mv = (centre.0 + dx, centre.1 + dy);
                    if mv.0.abs() > 32 || mv.1.abs() > 32 {
                        continue;
                    }
                    let s = sad(bx, by, mv);
                    if s < best.0 {
                        best = (s, mv);
                    }
                }
            }
            left = best.1;
            above[cx] = best.1;
            out.push((best.1, best.0));
        }
    }
    out
}

/// The intra-cost proxy libaom's tpl gets from a DC prediction plus a
/// transform: here, the mean absolute deviation of the cell from its own DC.
/// It is the denominator every propagated cost is read against, so only its
/// SHAPE across the frame matters, not its scale.
pub(super) fn tpl_intra_costs(frame: &[u8], width: usize, height: usize) -> Vec<u32> {
    let (cells_x, cells_y) = (width / 16, height / 16);
    let mut out = Vec::with_capacity(cells_x * cells_y);
    for cy in 0..cells_y {
        for cx in 0..cells_x {
            let (bx, by) = (cx * 16, cy * 16);
            let mut sum = 0u32;
            for r in 0..16 {
                sum += frame[(by + r) * width + bx..][..16]
                    .iter()
                    .map(|&p| u32::from(p))
                    .sum::<u32>();
            }
            let dc = ((sum + 128) / 256) as u8;
            let mut cost = 0u32;
            for r in 0..16 {
                cost += frame[(by + r) * width + bx..][..16]
                    .iter()
                    .map(|&p| u32::from(p.abs_diff(dc)))
                    .sum::<u32>();
            }
            out.push(cost);
        }
    }
    out
}

/// lane-av1tpl's one-frame map, in terms of the pass above: how much of THIS
/// frame the next one predicts from, per 16x16 cell -- each cell of `next`
/// splats its own area, at its winning displacement, onto the (at most four)
/// cells of `cur` it lands on. 1.0 is "referenced exactly once".
pub(super) fn tpl_area_map(cur: &[u8], next: &[u8], width: usize, height: usize) -> Vec<f64> {
    let start = std::time::Instant::now();
    let (cells_x, cells_y) = (width / 16, height / 16);
    let pass = tpl_coarse_pass(cur, next, width, height);
    let mut area = vec![0.0f64; cells_x * cells_y];
    for cy in 0..cells_y {
        for cx in 0..cells_x {
            let (mv, _) = pass[cy * cells_x + cx];
            let (x0, y0) = ((cx * 16) as i32 + mv.0, (cy * 16) as i32 + mv.1);
            let (x0, y0) = (x0.max(0) as usize, y0.max(0) as usize);
            for ci in x0 / 16..=x0.div_ceil(16).min(cells_x - 1) {
                let ox = (x0 + 16).min(ci * 16 + 16) - x0.max(ci * 16);
                for ri in y0 / 16..=y0.div_ceil(16).min(cells_y - 1) {
                    let oy = (y0 + 16).min(ri * 16 + 16) - y0.max(ri * 16);
                    area[ri * cells_x + ci] += (ox * oy) as f64 / 256.0;
                }
            }
        }
    }
    TPL_NANOS.fetch_add(
        start.elapsed().as_nanos() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    area
}

/// [`tpl_area_map`]'s per-cell referenced-ness turned into one lambda
/// factor per 64x64 superblock: `f = (1 + k) / (1 + k * w_rel)`, where
/// `w_rel` is the superblock's mean referenced-ness over the FRAME's mean.
/// Decreasing in referenced-ness (a block the next frame leans on is coded
/// more distortion-averse) and 1.0 at the frame's own mean, so the frame's
/// total lambda scale is only redistributed, never moved -- a global move is
/// the thing lane-av1lambda already measured inert.
pub(super) fn tpl_sb_factors(map: &[f64], cells_x: usize, cells_y: usize, k: f64) -> Vec<f64> {
    let (sb_cols, sb_rows) = (cells_x / 4, cells_y / 4);
    let mean = (map.iter().sum::<f64>() / map.len().max(1) as f64).max(1e-6);
    let mut out = Vec::with_capacity(sb_cols * sb_rows);
    for sr in 0..sb_rows {
        for sc in 0..sb_cols {
            let mut sum = 0.0;
            for r in 0..4 {
                for c in 0..4 {
                    sum += map[(sr * 4 + r) * cells_x + sc * 4 + c];
                }
            }
            let w_rel = sum / 16.0 / mean;
            out.push(((1.0 + k) / (1.0 + k * w_rel)).clamp(0.25, 4.0));
        }
    }
    if std::env::var("EC_AV1_TPL_HIST").ok().as_deref() == Some("1") {
        let mut hist = [0usize; 8];
        for w in map {
            hist[((w / mean * 2.0) as usize).min(7)] += 1;
        }
        let (lo, hi) = out
            .iter()
            .fold((f64::MAX, 0.0f64), |(l, h), &f| (l.min(f), h.max(f)));
        eprintln!(
            "tpl k={k}: {} cells mean w={mean:.3}, w/mean hist (0-.5,.5-1,..,>3.5) {hist:?}, \
             sb factor {lo:.3}..{hi:.3}, lookahead wall {:.1} ms cumulative",
            map.len(),
            TPL_NANOS.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
        );
    }
    out
}

/// libaom's `tpl_model_update` in this crate's cheapest shape: one lambda
/// factor per 16x16 cell of `frames[0]` (the frame being coded), from how
/// much of the WHOLE lookahead window leans on that cell.
///
/// Per lookahead level `f` (source `frames[f]` against `frames[f-1]`) the
/// coarse pass gives a displacement and an inter cost; a cell's own
/// propagated cost is `max(intra - inter, 0) + mc_dep` -- what coding that
/// cell well saves the frames that predict from it -- and it is splatted,
/// area-weighted, onto the (at most four) cells of `frames[f-1]` its
/// displacement lands on. Walking `f` from the far end of the window back to
/// 1 accumulates the whole chain into level 0, which lane-av1tpl's one-frame
/// area map could not do at all.
///
/// The factor is `f = 1 / (1 + k * mc_dep / intra)`, normalised so the mean
/// factor over the frame is exactly 1: a cell the window leans on is coded
/// more distortion-averse (smaller lambda), and the frame's TOTAL lambda
/// scale only moves around, never up or down -- a global move is what
/// lane-av1lambda already measured inert.
/// Returns the per-cell lambda factors AND the per-cell dependence ratio
/// `mc_dep / intra` they were derived from -- the second half is what
/// [`deltaq_libaom`] needs, since the factor alone has been through a
/// mean normalisation the raw ratio cannot be recovered across.
pub(super) fn tpl_lambda_factors(
    frames: &[&[u8]],
    width: usize,
    height: usize,
    k: f64,
) -> (Vec<f64>, Vec<f64>) {
    let start = std::time::Instant::now();
    let (cells_x, cells_y) = (width / 16, height / 16);
    let cells = cells_x * cells_y;
    let levels = frames.len();
    let intra: Vec<Vec<u32>> = frames
        .iter()
        .map(|f| tpl_intra_costs(f, width, height))
        .collect();
    let passes: Vec<Vec<((i32, i32), u32)>> = (1..levels)
        .map(|f| tpl_coarse_pass(frames[f - 1], frames[f], width, height))
        .collect();
    let mut mc_dep = vec![vec![0.0f64; cells]; levels];
    for f in (1..levels).rev() {
        for cy in 0..cells_y {
            for cx in 0..cells_x {
                let ci = cy * cells_x + cx;
                let (mv, inter) = passes[f - 1][ci];
                let saved = f64::from(intra[f][ci].saturating_sub(inter));
                let propagate = saved + mc_dep[f][ci];
                if propagate <= 0.0 {
                    continue;
                }
                // The 16x16 window of the PREVIOUS frame this cell predicted
                // from, spread over the cells it overlaps.
                let (x0, y0) = ((cx * 16) as i32 + mv.0, (cy * 16) as i32 + mv.1);
                let (x0, y0) = (x0.max(0) as usize, y0.max(0) as usize);
                for rc in x0 / 16..=x0.div_ceil(16).min(cells_x - 1) {
                    let ox = (x0 + 16).min(rc * 16 + 16) - x0.max(rc * 16);
                    for rr in y0 / 16..=y0.div_ceil(16).min(cells_y - 1) {
                        let oy = (y0 + 16).min(rr * 16 + 16) - y0.max(rr * 16);
                        mc_dep[f - 1][rr * cells_x + rc] += propagate * (ox * oy) as f64 / 256.0;
                    }
                }
            }
        }
    }
    // The tail compression `p`: `mc_dep/intra` is a ratio with a very long
    // tail (a nearly flat cell the window leans on has a tiny denominator),
    // and at p = 1 that tail slams into the clamp. libaom reads its own
    // dependence ratio through a sqrt (`dr_from_beta`); `EC_AV1_TPL_P`
    // sweeps the exponent, 1 being the raw ratio.
    let power = std::env::var("EC_AV1_TPL_P")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|p| p.is_finite() && *p > 0.0)
        .unwrap_or(TPL_POWER);
    // lane-av1tpl3, the three shape fixes, each its own knob so the gate can
    // read them apart. `SB` and `LOG` are the shipped shape (set the knob to
    // `0` to take either back out); `FLOOR` defaults to off.
    //
    // * `EC_AV1_TPL_FLOOR=<f>` floors the denominator at `f` times the
    //   frame's own mean intra cost. A cell of flat content has a near-zero
    //   MAD, so its ratio explodes exactly where coding is cheap.
    // * `EC_AV1_TPL_SB=0` goes back to a per-16x16 ratio; by default intra
    //   and mc_dep are summed over the 64x64 superblock first -- libaom's
    //   granularity (`mc_dep_cost` and `intra_cost` are SB sums there, not
    //   per-16x16 numbers), which is also what makes the ratio survive one
    //   flat cell next to a busy one.
    // * `EC_AV1_TPL_LOG=0` reads the raw ratio; by default it goes through
    //   `log2(1 + r)`, libaom's log-compressed beta, which flattens the tail
    //   harder than the `p` exponent alone can.
    let floor_frac = std::env::var("EC_AV1_TPL_FLOOR")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|f| f.is_finite() && *f >= 0.0)
        .unwrap_or(0.0);
    let sb_agg = std::env::var("EC_AV1_TPL_SB").ok().as_deref() != Some("0");
    let log_beta = std::env::var("EC_AV1_TPL_LOG").ok().as_deref() != Some("0");
    let intra_mean = intra[0].iter().map(|&c| f64::from(c)).sum::<f64>() / cells.max(1) as f64;
    let floor = floor_frac * intra_mean;
    let denom = |c: usize| f64::from(intra[0][c].max(1)).max(floor).max(1.0);
    // Per-cell ratios, or one ratio per 64x64 superblock (4x4 cells) shared by
    // every cell under it. The edge groups are partial and summed as they are.
    let ratio: Vec<f64> = if sb_agg {
        let (sb_cols, sb_rows) = (cells_x.div_ceil(4), cells_y.div_ceil(4));
        let mut sb = vec![(0.0f64, 0.0f64); sb_cols * sb_rows];
        for cy in 0..cells_y {
            for cx in 0..cells_x {
                let e = &mut sb[(cy / 4) * sb_cols + cx / 4];
                e.0 += mc_dep[0][cy * cells_x + cx];
                e.1 += denom(cy * cells_x + cx);
            }
        }
        (0..cells)
            .map(|c| {
                let (dep, den) = sb[(c / cells_x / 4) * sb_cols + (c % cells_x) / 4];
                dep / den.max(1.0)
            })
            .collect()
    } else {
        (0..cells).map(|c| mc_dep[0][c] / denom(c)).collect()
    };
    let mut out: Vec<f64> = ratio
        .iter()
        .map(|&r| {
            let r = if log_beta { (1.0 + r).log2() } else { r };
            (1.0 + k * r).powf(-power)
        })
        .collect();
    let mean = (out.iter().sum::<f64>() / cells.max(1) as f64).max(1e-9);
    for f in &mut out {
        *f = (*f / mean).clamp(0.25, 4.0);
    }
    TPL_NANOS.fetch_add(
        start.elapsed().as_nanos() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    if std::env::var("EC_AV1_TPL_HIST").ok().as_deref() == Some("1") {
        let mut hist = [0usize; 8];
        for &f in &out {
            hist[((f * 4.0) as usize).min(7)] += 1;
        }
        let (lo, hi) = out
            .iter()
            .fold((f64::MAX, 0.0f64), |(l, h), &f| (l.min(f), h.max(f)));
        let flat = out.iter().filter(|&&f| (0.9..=1.1).contains(&f)).count();
        eprintln!(
            "tpl k={k} d={levels}: {cells} cells, factor hist (0-.25,.25-.5,..,>1.75) {hist:?}, \
             range {lo:.3}..{hi:.3}, {flat} within 0.9..1.1, lookahead wall {:.1} ms cumulative",
            TPL_NANOS.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
        );
    }
    (out, ratio)
}

/// Where one inter frame sits in a coding-order pyramid: which DPB slots its
/// references name, which slot it refreshes, whether it is shown when it is
/// coded, and the `ref_frame_sign_bias` that follows from all of it. `None`
/// keeps `encode_inter_frame`'s pre-pyramid flat behaviour verbatim (slot
/// 0/2 alternation, every frame shown, every reference in the past).
///
/// The sign bias is carried rather than derived here because only the caller
/// knows what order hint each slot holds; it is armed onto the tile writer
/// ([`crate::tile::arm_sign_bias`]) and set on the encoder's own MV grid, so
/// the writer scans the same stacks the decoder will.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PyramidFrame {
    /// The slot `LAST_FRAME` (and every reference this encoder does not name
    /// apart) reads.
    pub last_slot: u8,
    /// The single slot `refresh_frame_flags` sets.
    pub self_slot: u8,
    /// The slot `ALTREF_FRAME` reads -- a FUTURE frame for a leaf of a
    /// pyramid, which is what makes `sign_bias`'s last entry true.
    pub altref_slot: u8,
    /// lane-last2: the slot `LAST2_FRAME` reads -- a SECOND, older past
    /// picture (the leaf chain alternates its refresh between two slots, so
    /// the frame before `LAST` is still intact). `None` leaves
    /// `ref_frame_idx[1]` naming `last_slot`, as every frame this encoder
    /// wrote before, and the caller then passes no `last2` picture either.
    pub last2_slot: Option<u8>,
    /// `show_frame`: false for a hidden frame, output later by a
    /// `show_existing_frame` header.
    pub show_frame: bool,
    /// lane-arfq: which level's `delta_q` strength this frame reads
    /// ([`DqLevel`]) -- the caller knows it, since the top ARF and the mid
    /// one share `Level::Arf` and differ only by their q offset.
    pub dq_level: DqLevel,
    /// This frame's `ref_frame_sign_bias` (spec 5.9.2).
    pub sign_bias: crate::mvstack::SignBiasTable,
}

/// lane-arfpred: the temporal filter of a top ARF's SOURCE picture, libaom's
/// `temporal_filter` in the small: every 16x16 luma block of `src` is
/// motion-compensated from each picture of `window` (the group's own display
/// neighbours, nearest first) and averaged in with weight
/// `exp(-mse / strength)`, so a neighbour that matches badly contributes
/// nothing and one that matches well cancels the grain the block's residual
/// would otherwise have to code.
///
/// `strength` is [`crate::speed::ARF_TF`]; `0` never reaches here. Chroma
/// rides the luma block's own vector (halved), which is what libaom does at
/// this block size. The picture is the SUPERBLOCK-padded one, so the 16x16
/// grid tiles it exactly.
///
/// Priced before it was built by `encode::tests::arf_pred_census`: on film B
/// the filtered ARF's prediction SAD at lag 8 is 11.7% below the unfiltered
/// one's (film A 5.9%) with the two past neighbours at strength 30, and the
/// filtered picture is 50.1 dB from the source it replaces -- 4 dB above the
/// quality the ARF is coded at, so the substitution itself costs little.
// lane-lookahead: the CALLER sizes `window` (`ARF_TF_WIN` past neighbours and
// `ARF_TF_WIN_FUT` future ones); this takes every picture in it.
pub(crate) fn arf_temporal_filter(src: &Picture, window: &[Picture], strength: f64) -> Picture {
    const SIDE: usize = 16;
    let neighbours: Vec<&Picture> = window
        .iter()
        .filter(|p| p.width == src.width && p.height == src.height)
        .collect();
    if neighbours.is_empty() || strength <= 0.0 {
        return src.clone();
    }
    let mut out = src.clone();
    // SAD of `src`'s block at `(bx, by)` against `r` displaced by `mv`, or
    // `None` when the displaced block leaves the picture.
    let sad = |r: &Picture, bx: usize, by: usize, mv: (isize, isize)| {
        let (sy, sx) = (by as isize + mv.0, bx as isize + mv.1);
        if sy < 0 || sx < 0 || sy as usize + SIDE > r.height || sx as usize + SIDE > r.width {
            return None;
        }
        let (sy, sx) = (sy as usize, sx as usize);
        let mut acc = 0u64;
        for row in 0..SIDE {
            let c = &src.y[(by + row) * src.width + bx..][..SIDE];
            let p = &r.y[(sy + row) * r.width + sx..][..SIDE];
            acc += c
                .iter()
                .zip(p)
                .map(|(&a, &b)| u64::from(a.abs_diff(b)))
                .sum::<u64>();
        }
        Some((acc, (sy, sx)))
    };
    // The log-step diamond `arf_pred_census` priced this with.
    let search = |r: &Picture, bx: usize, by: usize| -> Option<(usize, usize)> {
        let mut best = sad(r, bx, by, (0, 0))?;
        let mut centre = (0isize, 0isize);
        let mut step = 16isize;
        while step >= 1 {
            let from = centre;
            for (dy, dx) in [
                (-1, -1),
                (-1, 0),
                (-1, 1),
                (0, -1),
                (0, 1),
                (1, -1),
                (1, 0),
                (1, 1),
            ] {
                let mv = (from.0 + dy * step, from.1 + dx * step);
                if let Some(s) = sad(r, bx, by, mv)
                    && s.0 < best.0 {
                        best = s;
                        centre = mv;
                    }
            }
            step /= 2;
        }
        Some(best.1)
    };
    let (cw, ch) = (src.width / 2, src.height / 2);
    for by in (0..src.height).step_by(SIDE) {
        for bx in (0..src.width).step_by(SIDE) {
            if by + SIDE > src.height || bx + SIDE > src.width {
                continue;
            }
            let mut y_acc = vec![0f64; SIDE * SIDE];
            let mut c_acc = [vec![0f64; SIDE * SIDE / 4], vec![0f64; SIDE * SIDE / 4]];
            let mut wsum = 1.0f64;
            // Chroma keeps its OWN weight sum: an edge block whose halved
            // vector leaves the chroma plane contributes to luma only, and
            // dividing chroma by the luma sum would darken it (class
            // `metric blind to a plane`, on the encoder side).
            let mut cwsum = 1.0f64;
            for row in 0..SIDE {
                for col in 0..SIDE {
                    y_acc[row * SIDE + col] = f64::from(src.y[(by + row) * src.width + bx + col]);
                }
            }
            for row in 0..SIDE / 2 {
                for col in 0..SIDE / 2 {
                    let i = (by / 2 + row) * cw + bx / 2 + col;
                    c_acc[0][row * SIDE / 2 + col] = f64::from(src.u[i]);
                    c_acc[1][row * SIDE / 2 + col] = f64::from(src.v[i]);
                }
            }
            for r in &neighbours {
                let Some((sy, sx)) = search(r, bx, by) else {
                    continue;
                };
                let mut sse = 0f64;
                for row in 0..SIDE {
                    for col in 0..SIDE {
                        let d = f64::from(src.y[(by + row) * src.width + bx + col])
                            - f64::from(r.y[(sy + row) * r.width + sx + col]);
                        sse += d * d;
                    }
                }
                let w = (-sse / (SIDE * SIDE) as f64 / strength).exp();
                for row in 0..SIDE {
                    for col in 0..SIDE {
                        y_acc[row * SIDE + col] +=
                            w * f64::from(r.y[(sy + row) * r.width + sx + col]);
                    }
                }
                // Chroma reads the luma vector halved; a block on the picture
                // edge whose halved vector would leave the chroma plane is
                // left to luma alone rather than clamped to a different
                // displacement than the luma it must stay registered with.
                let (cy, cx) = (sy / 2, sx / 2);
                if cy + SIDE / 2 <= ch && cx + SIDE / 2 <= cw {
                    for row in 0..SIDE / 2 {
                        for col in 0..SIDE / 2 {
                            let i = (cy + row) * cw + cx + col;
                            c_acc[0][row * SIDE / 2 + col] += w * f64::from(r.u[i]);
                            c_acc[1][row * SIDE / 2 + col] += w * f64::from(r.v[i]);
                        }
                    }
                    cwsum += w;
                }
                wsum += w;
            }
            for row in 0..SIDE {
                for col in 0..SIDE {
                    out.y[(by + row) * src.width + bx + col] =
                        (y_acc[row * SIDE + col] / wsum).round() as u16;
                }
            }
            for row in 0..SIDE / 2 {
                for col in 0..SIDE / 2 {
                    let i = (by / 2 + row) * cw + bx / 2 + col;
                    out.u[i] = (c_acc[0][row * SIDE / 2 + col] / cwsum).round() as u16;
                    out.v[i] = (c_acc[1][row * SIDE / 2 + col] / cwsum).round() as u16;
                }
            }
        }
    }
    out
}

/// Encodes one picture as an inter frame predicting from `reference`'s
/// reconstruction, which the caller decoded (or, for the frame right after
/// the key frame, is the key frame's own reconstruction).
// Spec-shaped signature; bundling the operands would obscure the spec mapping.
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_inter_frame(
    picture: &Picture,
    reference: &Picture,
    base_q_idx: u8,
    deadzone: f64,
    order_hint: u32,
    // The order hint each of the seven `ref_frame_idx` entries names, which a
    // decoder reads out of its own reference state -- only spec 5.9.22's
    // `skipModeAllowed` (in `frame::write_frame_header`) reads it, and only
    // on a `reference_select` frame. [`flat_order_hints`] builds it for the
    // callers that follow this crate's own flat slot plan.
    order_hints: [u32; 7],
    render: (usize, usize),
    // The tables this frame's writer starts from: what the previous frame
    // stored into the slot this one's `primary_ref_frame` names. `None`
    // starts from the defaults, which is right only for the frame straight
    // after a key frame.
    start_cdfs: Option<&crate::cdf_state::Cdfs>,
    // The key frame's own (padded) reconstruction, which every frame of the
    // GOP still holds in DPB slot 1 -- `GOLDEN_FRAME`. `None` codes the frame
    // with `LAST_FRAME` alone, exactly as before.
    golden: Option<&Picture>,
    // The frame two back, held in the slot this frame is about to refresh
    // (the encoder alternates 0/2): `ALTREF_FRAME`. `None` for the first two
    // inter frames of a GOP, where the frame two back IS the key frame.
    altref: Option<&Picture>,
    // lane-last2: the picture in [`PyramidFrame::last2_slot`] -- the shown
    // frame BEFORE `reference` on the leaf chain, offered to the 32x32
    // search as `LAST2_FRAME` through the same search path `GOLDEN`/`ALTREF`
    // take. `None` (every caller but the pyramid leaves) codes the frame
    // with the reference set unchanged.
    last2: Option<&Picture>,
    fctx: &crate::decode::FrameCtx,
    // Where this frame sits in the coding-order pyramid; `None` is the flat
    // one-shown-frame-per-picture stream this encoder wrote before.
    pyramid: Option<PyramidFrame>,
    // lane-av1tpl2: the NEXT pictures' sources (up to [`tpl_depth`] - 1 of
    // them), padded like this one -- the lookahead window the per-block
    // lambda weighting propagates back through ([`tpl_lambda_factors`]). An
    // EMPTY slice codes the frame at one lambda, as before.
    lookahead: &[Picture],
) -> Result<Encoded> {
    picture.check()?;
    if !picture.width.is_multiple_of(SUPERBLOCK) || !picture.height.is_multiple_of(SUPERBLOCK) {
        return Err(Error::unsupported(
            "AV1 encode",
            "an inter frame is coded only for a picture that is a whole number \
             of 64x64 superblocks -- the inter tile writer never splits a \
             superblock's partition below that",
        ));
    }
    if reference.width != picture.width || reference.height != picture.height {
        return Err(Error::unsupported(
            "AV1 encode",
            "the reference picture must be the same size as the one being coded",
        ));
    }

    // The single slot this crate's inter frames ever predict from or
    // refresh (`inter_frame_headers`'s contract): every frame both reads and
    // overwrites slot 0, so the frame just coded is always what the next one
    // predicts against.
    const LAST_SLOT: u8 = 0;
    // Slot 0 and slot 2 alternate so the frame two back survives one more
    // frame: frame n writes A(n) = 0 for odd n, 2 for even, so A(n) = A(n-2)
    // and the picture this frame names as `ALTREF_FRAME` is the one sitting
    // in the very slot it will overwrite.
    let self_slot = pyramid.map_or(if order_hint % 2 == 1 { 0 } else { 2 }, |p| p.self_slot);
    let last_slot = pyramid.map_or(
        if order_hint <= 1 {
            LAST_SLOT
        } else if (order_hint - 1) % 2 == 1 {
            0
        } else {
            2
        },
        |p| p.last_slot,
    );
    // `render`, not `picture.width`/`picture.height`: `picture` here is
    // already padded to a whole number of superblocks (`encode_sequence`'s
    // `padded_to(SUPERBLOCK)`), and the header's `mi_cols`/`mi_rows` (and so
    // every true-edge clamp downstream) must come from the frame's real,
    // unpadded size, same as `key_frame_headers_colour` takes `render` and
    // not the padded picture in `encode_key_frame_inner`.
    let (seq, mut header) = inter_frame_headers_slots(
        render.0,
        render.1,
        base_q_idx,
        order_hint,
        last_slot,
        self_slot,
        match pyramid {
            Some(p) => p.altref_slot,
            None if altref.is_some() => self_slot,
            None => GOLDEN_SLOT,
        },
    )?;
    header.render_width = render.0 as u32;
    header.render_height = render.1 as u32;
    // lane-refs: this frame's motion-compensation kernel, written into the
    // header AND armed on the context every prediction reads, in one place so
    // the two cannot disagree (a header saying SMOOTH over a REGULAR-searched
    // tile decodes to different pixels than the encoder scored).
    header.interpolation_filter = frame_interp_filter();
    fctx.interp_filter.set(mc::InterpFilterKind::from_header(
        header.interpolation_filter,
    ));
    let sign_bias = pyramid.map_or(crate::mvstack::NO_SIGN_BIAS, |p| p.sign_bias);
    // The order hint behind each `ref_frame_idx` entry. The flat path's own
    // slot plan (`inter_frame_headers_slots`) is: every reference but
    // `GOLDEN_FRAME` reads `last_slot` (the frame just coded) except
    // `ALTREF_FRAME`, which reads the frame two back when there is one;
    // `GOLDEN_FRAME` is the key frame, order hint 0. Only
    // `frame::write_frame_header`'s `skipModeAllowed` reads this.
    header.order_hints = order_hints;
    // lane-last2: `inter_frame_headers_slots` points every un-named
    // reference at `last_slot`; the pyramid's leaf chain retains a second,
    // older picture and names it here. The caller's `sign_bias` /
    // `order_hints` entry 1 is derived from this same slot, so the stack
    // scans and `skipModeAllowed` read what the decoder will.
    if let Some(slot) = pyramid.and_then(|p| p.last2_slot) {
        header.ref_frame_idx[1] = slot;
    }
    header.reference_select = reference_select();
    // Armed HERE, not only beside the tile write below: `ref_distance`'s own
    // per-reference order-hint distance is read by the block SEARCH, which
    // runs long before the writer arms these (the distance-scaled search step
    // read `(0, 0, [0; 7])` and came out 1 for every reference -- the knob
    // never reached the tool).
    crate::tile::arm_order_hints(seq.order_hint_bits, order_hint, order_hints);
    // lane-hpmv: armed here for the same reason the order hints are -- the
    // block SEARCH reads it (its eighth-pel stage) long before the writer.
    let hp_mv = header.allow_high_precision_mv;
    crate::tile::arm_high_precision_mv(hp_mv);
    if let Some(p) = pyramid {
        header.show_frame = p.show_frame;
        header.showable_frame = !p.show_frame;
        header.ref_frame_sign_bias = p.sign_bias;
    }
    // `TxMode::Select` here too: an inter block then codes one `txfm_split`
    // flag (kept at zero -- its var-tx tree is not searched yet) and an INTRA
    // block inside this frame codes and searches its own `tx_depth`, the same
    // as in a key frame (`crate::tile::write_tx_syntax_inter`).
    let tx_select = tx_select() && tx_select_inter();
    if tx_select {
        header.tx_mode = TxMode::Select;
    }

    let (true_width, true_height) = (header.mi_cols as usize * 4, header.mi_rows as usize * 4);
    // lane-palw: the encoder's own in-process reconstruct reads its palette
    // colour-index maps back through `decode::palette_onscreen`, which takes
    // the frame's mi dims off the `FrameCtx` -- a real decode gets them from
    // `set_segmentation`, which never runs on `FrameCtx::for_encoder`. Publish
    // them here so the reconstruct clamps a block cut by the frame edge
    // exactly as the writer now does.
    let frame_mi_dims = (header.mi_rows as usize, header.mi_cols as usize);
    fctx.seg_mi_dims.set(frame_mi_dims);
    // lane-hbd r4: encoder stays 8-bit by design; narrow the source once
    // here (see `encode_key_frame_inner`).
    let picture_y8: Vec<u8> = picture.y.iter().map(|&v| v as u8).collect();
    let picture_u8: Vec<u8> = picture.u.iter().map(|&v| v as u8).collect();
    let picture_v8: Vec<u8> = picture.v.iter().map(|&v| v as u8).collect();
    // This frame's own `allow_screen_content_tools`. It can only be set when
    // the SEQUENCE offers the bit at all, which is the key frame's decision
    // ([`SEQ_SCREEN`]): a frame header that contradicts
    // `seq_force_screen_content_tools` is a corrupt stream, not a tighter
    // encode.
    let screen =
        seq_screen() && screen_content(&picture_y8, picture.width, true_width, true_height);
    header.allow_screen_content_tools = screen;
    // lane-txi, as on the key frame: the header bit the writer's own
    // `Cdfs` then reads for every luma `tx_type` symbol.
    header.reduced_tx_set = !wide_tx_set(screen);
    // lane-intertx, desync (a): the frame context this value is read back
    // through ([`decode::reduce_inherited_chroma_tx_type`] inside
    // `recode_inter_chroma`, and every inter decode helper the search's own
    // trial decode shares) must carry THIS frame's bit, not whatever the
    // previous frame's trial decode last left in the shared cell -- a stale
    // `true` here narrows the chroma allowance the search reconstructs
    // against under the one the header's bit actually allows.
    fctx.reduced_tx_set_inter.set(header.reduced_tx_set);
    // The search prices coefficients against the tables this frame's writer
    // starts from, not against the defaults (lane-av1txbits) -- but only on a
    // frame the screen detector said no to (lane-av1price2): armed HERE, after
    // `screen` exists, and again on every search/tile worker below.
    crate::tile::arm_pricing_cdfs(start_cdfs, screen);
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

    let step = f64::from(ac_q(8, i32::from(base_q_idx))) / 8.0;
    let top_arf = matches!(
        pyramid,
        Some(p) if p.dq_level == DqLevel::TopArf
    );
    let arfmode_v = arfmode();
    let search = Search {
        base_q_idx,
        deadzone,
        lambda: lambda_scale()
            * match pyramid {
                Some(p) if !p.show_frame => hidden_lambda_factor(),
                _ => 1.0,
            }
            * step
            * step,
        modes: &KEY_FRAME_MODES,
        top_k: prune_top_k_inter(),
        screen,
        // lane-arfmode: the group's own anchor ([`DqLevel::TopArf`], whose
        // offset is the pyramid's own `arf_q_offset`) reads its leaf offer
        // set from [`crate::speed::ARFMODE`]; every other frame -- the mid
        // and quarter hidden frames, the leaves, the flat path -- keeps the
        // full set.
        arf_leaf_intra: !top_arf || arfmode_v & 1 == 0,
        arf_leaf8: !top_arf || arfmode_v & 2 == 0,
        arf_leaf_compound: !top_arf || arfmode_v & 4 == 0,
    };
    let mode_bits_table = inter_mode_bits();
    // lane-av1tpl: one lambda factor per 64x64 superblock, from how much of
    // this frame the NEXT one predicts from. Off (`None`) without a
    // lookahead picture or at strength 0.
    let tpl_k = tpl_strength();
    let window: Vec<&Picture> = lookahead
        .iter()
        .take_while(|n| n.width == picture.width && n.height == picture.height)
        .collect();
    let tpl_sources: Vec<Vec<u8>> = window
        .iter()
        .map(|n| n.y.iter().map(|&v| v as u8).collect())
        .collect();
    let tpl_cells_x = picture.width / 16;
    // `.1` is the propagating map's raw per-cell dependence ratio, which only
    // [`deltaq_libaom`] reads; the one-frame area map has no such ratio.
    let tpl_both: Option<(Vec<f64>, Option<Vec<f64>>)> = (tpl_k > 0.0 && !tpl_sources.is_empty())
        .then(|| {
            if tpl_propagating() {
                let mut frames: Vec<&[u8]> = vec![&picture_y8];
                frames.extend(tpl_sources.iter().map(Vec::as_slice));
                let (f, r) = tpl_lambda_factors(&frames, picture.width, picture.height, tpl_k);
                (f, Some(r))
            } else {
                // The shipped map is one factor per 64x64; it is spread over that
                // superblock's own 16 cells so the application site below (which
                // averages the four cells of a 32x32 block) reads exactly the
                // superblock factor, whichever map fed it.
                let map = tpl_area_map(&picture_y8, &tpl_sources[0], picture.width, picture.height);
                let cells_y = picture.height / 16;
                let sb = tpl_sb_factors(&map, tpl_cells_x, cells_y, tpl_k);
                let f: Vec<f64> = (0..tpl_cells_x * cells_y)
                    .map(|c| sb[(c / tpl_cells_x / 4) * (tpl_cells_x / 4) + (c % tpl_cells_x) / 4])
                    .collect();
                (f, None)
            }
        });
    let (tpl_factors, tpl_ratio) = match tpl_both {
        Some((f, r)) => (Some(f), r),
        None => (None, None),
    };

    // The true grid ([`crate::tile::block_grid`]'s ceiling), not the padded
    // coding surface's: `blocks` carries one entry per 32x32 block the tile
    // writer will actually visit, same count it checks `blocks.len()`
    // against, which is fewer than the padded surface's own block count
    // whenever the true size is not a 64x64 multiple.
    let (cols, rows) = crate::tile::block_grid(header.mi_cols, header.mi_rows);
    let (cols, rows) = (cols as usize, rows as usize);
    let (sb_cols, _sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    // lane-deltaq: one absolute qindex per 64x64 superblock, from the same
    // tpl map the lambda factor comes from ([`deltaq_for_factor`]). Present
    // only when the map is, and `delta_q_res` follows straight into the frame
    // header (`write_delta_q_params`) and into the tile writer's own plan.
    // Not on a screen frame ([`b64_residual`]'s content gate): lane-dq3
    // measured the per-superblock quantizer as a win on both films and a
    // 0.5 loss vs libaom on the desktop capture, so a frame that set
    // `allow_screen_content_tools` codes no delta syntax at all and stays
    // byte-identical to the pre-lane encoder.
    // lane-arfq: the level whose `delta_q` strength this frame reads; the
    // flat path (no pyramid) is a leaf.
    let dq_level = pyramid.map_or(DqLevel::Leaf, |p| p.dq_level);
    let deltaq_res: Option<i32> = tpl_factors
        .as_ref()
        .filter(|_| !screen)
        .and(deltaq_res_log2().map(|r| 1i32 << r));
    // The same 16-cell mean the superblock's own lambda takes, clamped to the
    // map the frame actually has -- read over the factors or over the raw
    // dependence ratios, whichever mapping is asking.
    let sb_mean = |m: &[f64], sb: usize| {
        let (sb_r, sb_c) = (sb / sb_cols, sb % sb_cols);
        (0..16)
            .map(|i| {
                let (cy, cx) = (sb_r * 4 + i / 4, sb_c * 4 + i % 4);
                m[(cy.min(m.len() / tpl_cells_x - 1)) * tpl_cells_x + cx.min(tpl_cells_x - 1)]
            })
            .sum::<f64>()
            / 16.0
    };
    // lane-dq: `EC_AV1_DQ_TPL=1` takes libaom's own objective mapping off the
    // raw dependence ratio; without it (and always without a ratio map, i.e.
    // on the one-frame area map) the shipped derivation stands.
    let r_frame = tpl_ratio.as_deref().map(tpl_frame_ratio);
    let sb_qindex = |f: &[f64], res: i32, sb: usize, libaom: Option<f64>| -> u8 {
        match (libaom, tpl_ratio.as_deref(), r_frame) {
            (Some(k), Some(r), Some(rf)) => deltaq_libaom(base_q_idx, sb_mean(r, sb), rf, k, res),
            _ => deltaq_for_factor(base_q_idx, sb_mean(f, sb), deltaq_k(), deltaq_clamp(), res),
        }
    };
    let sb_q: Option<Vec<u8>> = match (&tpl_factors, deltaq_res) {
        (Some(f), Some(res)) => {
            let libaom = dq_tpl_strength(dq_level);
            let mut g: Vec<u8> = (0..sb_cols * _sb_rows)
                .map(|sb| sb_qindex(f, res, sb, libaom))
                .collect();
            // lane-b128m: under 128 superblocks the delta_q syntax is coded
            // ONCE per 128 root (the reader's own unit), so all four 64x64
            // cells of a root have to be searched -- and quantized -- at the
            // one qindex the root's top-left cell codes.
            if sb128_on() {
                for sb_r in 0.._sb_rows {
                    for sb_c in 0..sb_cols {
                        let q = g[(sb_r & !1) * sb_cols + (sb_c & !1)];
                        g[sb_r * sb_cols + sb_c] = q;
                    }
                }
            }
            Some(g)
        }
        _ => None,
    };
    // lane-dq census: both mappings on the SAME superblocks, whether or not
    // the frame codes any delta syntax (`deltaq_res` is `None` with the lever
    // off, and the census then prices the default `res = 4` grid).
    if dq_census()
        && let Some(f) = &tpl_factors {
            let res = deltaq_res.unwrap_or(4);
            let off = |libaom: Option<f64>| -> Vec<i32> {
                (0..sb_cols * _sb_rows)
                    .map(|sb| i32::from(sb_qindex(f, res, sb, libaom)) - i32::from(base_q_idx))
                    .collect()
            };
            let strength = dq_tpl_strength(dq_level);
            let (ours, aom) = (off(None), off(Some(strength.unwrap_or(1.0))));
            let coded = match (deltaq_res.is_some(), strength) {
                (false, _) => "none".to_string(),
                (true, None) => "ours".to_string(),
                (true, Some(k)) => format!("libaom k={k}"),
            };
            eprintln!(
                "dq census: level={dq_level:?} base={base_q_idx} res={res} sbs={} coded={coded} r_frame={:.4}\n                   ours   {}\n  libaom {}",
                sb_cols * _sb_rows,
                r_frame.unwrap_or(f64::NAN),
                dq_summary(&ours),
                dq_summary(&aom),
            );
        }
    if let Some(res) = deltaq_res.filter(|_| sb_q.is_some()) {
        header.delta.q_present = true;
        header.delta.q_res = res.trailing_zeros() as u8;
    }
    // lane-rectdq: what the tile writer's own `arm_delta_q` is armed with
    // below, so the root search prices the group the writer really codes.
    let dq_res: i32 = deltaq_res.filter(|_| sb_q.is_some()).unwrap_or(0);
    if let Some(g) = &sb_q {
        let mut levels: Vec<u8> = g.clone();
        levels.sort_unstable();
        levels.dedup();
        DELTAQ_LEVELS.fetch_max(levels.len(), std::sync::atomic::Ordering::Relaxed);
    }
    let (mi_cols, mi_rows) = (header.mi_cols as usize, header.mi_rows as usize);
    // This frame's tile grid, as in `encode_key_frame_inner`.
    let (tile_cols_log2, tile_rows_log2) = armed_tiles();
    let layout = crate::tile::TileLayout::new(
        header.mi_cols,
        header.mi_rows,
        tile_cols_log2,
        tile_rows_log2,
    );
    header.tile_info = tile_info_of(&layout, 1);
    let header_tile_info = header.tile_info.clone();
    // lane-hbd r4: `motion::search` is 8-bit only (its SAD/cost math is
    // integer-`u8`-scale), so the DPB reference plane is narrowed for it --
    // once per frame (lane-av1rd2), not once per block, which is a whole
    // plane's map per 32x32 block and now per leaf too.
    let mut blocks = vec![Quadrant::Whole(BlockCoeffs::default()); cols * rows];
    // lane-av1tsearch: one search job per TILE (`search_tiles`). Neighbour
    // availability -- intra edges and the MV stack's scans alike -- already
    // stopped at the superblock's own tile (`PlaneBuf::set_tile_origin`,
    // `MiGrid::set_tile_bounds`), so a tile reads nothing another tile wrote;
    // giving each job its own planes and its own `MiGrid` therefore leaves
    // every decision, and so every coded bit, exactly where the whole-frame
    // loop left it, at any tile count and any thread count.
    let (frame_width, frame_height) = (picture.width, picture.height);
    let search_tile = |index: usize,
                       fctx: &crate::decode::FrameCtx|
     -> Result<(Vec<(usize, Quadrant)>, TilePixels)> {
        // Thread-local, and this job may be running on a `search_tiles`
        // worker: `ref_distance`'s order-hint distance (the scaled motion
        // search step) is read by the SEARCH, so every worker arms it or the
        // tile's bits depend on the thread count.
        crate::tile::arm_order_hints(seq.order_hint_bits, order_hint, order_hints);
        crate::tile::arm_high_precision_mv(hp_mv);
        crate::tile::arm_pricing_cdfs(start_cdfs, screen);
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
        let mut grid = MiGrid::new(mi_cols, mi_rows);
        grid.set_sign_bias(sign_bias);
        // lane-av1obmc2: every picture an OBMC neighbour of a LEAF can name,
        // built once per tile job (`obmc_prediction`).
        let mut obmc_refs: [Option<&Picture>; 8] = [None; 8];
        obmc_refs[crate::mvstack::LAST_FRAME as usize] = Some(reference);
        obmc_refs[crate::mvstack::LAST2_FRAME as usize] = last2;
        obmc_refs[crate::mvstack::GOLDEN_FRAME as usize] = golden;
        obmc_refs[crate::mvstack::ALTREF_FRAME as usize] = altref;
        grid.set_tile_bounds(
            rect.mi_row0 as usize,
            rect.mi_col0 as usize,
            rect.mi_row1 as usize,
            rect.mi_col1 as usize,
        );
        let mut blocks: Vec<(usize, Quadrant)> = Vec::new();
        // lane-b128: under a 128x128 superblock the search walks the roots the
        // WRITER walks (`tile::sb_write_order`: TL, TR, BL, BR inside each root),
        // not plain raster -- a search that visits BL before TR builds every mv
        // stack and neighbour context off blocks the writer has not coded yet,
        // which is what made the write-time drl clamp fire at all (lane-sb128b).
        // At a 64 superblock the two orders are identical and this is the same
        // raster loop as before.
        let sb128_search = sb128_on();
        let root_step = if sb128_search { 2 } else { 1 };
        let mut roots: Vec<(usize, usize)> = Vec::new();
        {
            let (r0, r1) = (rect.sb_row0 as usize, rect.sb_row1 as usize);
            let (c0, c1) = (rect.sb_col0 as usize, rect.sb_col1 as usize);
            let mut r = r0;
            while r < r1 {
                let mut c = c0;
                while c < c1 {
                    roots.push((r, c));
                    c += root_step;
                }
                r += root_step;
            }
        }
        for (root_r, root_c) in roots {
            // The 64x64 cells of this root, in the writer's own order.
            let cells: Vec<(usize, usize)> = if sb128_search {
                [(0, 0), (0, 1), (1, 0), (1, 1)]
                    .into_iter()
                    .map(|(dr, dc)| (root_r + dr, root_c + dc))
                    .filter(|&(r, c)| r < rect.sb_row1 as usize && c < rect.sb_col1 as usize)
                    .collect()
            } else {
                vec![(root_r, root_c)]
            };
            // What the four cells below really cost, against which the whole
            // 128x128 candidate is weighed.
            let mark128 = blocks.len();
            let mut root_cost = 0.0f64;
            // The 128 root's own compound pairs, at its 32x32-mi window (the 64
            // root builds the same list one size down, per superblock).
            let root_compound: Vec<(i8, &Picture, crate::mvstack::CompoundMvStack)> =
                if header.reference_select && b128_compound() && !screen && sb128_search {
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
                                    &grid,
                                    root_r * 16,
                                    root_c * 16,
                                    32,
                                    32,
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
                } else {
                    Vec::new()
                };
            let root128 = (sb128_search && b128_root() && cells.len() == 4)
                .then(|| {
                    search_root_128(
                        &mut luma,
                        &mut chroma,
                        &grid,
                        (root_r, root_c),
                        (rows, cols),
                        (header.mi_cols, header.mi_rows),
                        search,
                        &tpl_factors,
                        sb_q.as_ref(),
                        sb_cols,
                        tpl_cells_x,
                        base_q_idx,
                        dq_res,
                        reference,
                        &root_compound,
                        fctx,
                    )
                })
                .flatten();
            // lane-b128hv: the same root as two 128x64 / 64x128 halves. Both cuts
            // are priced; the cheaper one competes with the NONE block and with
            // the four superblocks below.
            let forced_rect = FORCE_B128_RECT.load(std::sync::atomic::Ordering::Relaxed);
            let root_rect = (sb128_search && b128_rect() && cells.len() == 4)
                .then(|| {
                    let mut best: Option<(bool, _)> = None;
                    for horz in [true, false] {
                        if forced_rect != 0 && forced_rect != u8::from(horz) + u8::from(!horz) * 2 {
                            continue;
                        }
                        let Some(found) = search_root_128_rect(
                            &mut luma,
                            &mut chroma,
                            &mut grid,
                            (root_r, root_c),
                            (rows, cols),
                            (header.mi_cols, header.mi_rows),
                            search,
                            &tpl_factors,
                            sb_q.as_ref(),
                            sb_cols,
                            tpl_cells_x,
                            base_q_idx,
                            dq_res,
                            reference,
                            &root_compound,
                            horz,
                            fctx,
                        ) else {
                            continue;
                        };
                        if best
                            .as_ref()
                            .is_none_or(|b: &(bool, (f64, f64, _, _))| found.0 < b.1.0)
                        {
                            best = Some((horz, found));
                        }
                    }
                    best
                })
                .flatten();
            // lane-ab128: and the same root as one of the four AB shapes -- two
            // 64x64 pieces plus one 128x64 / 64x128 half. All four are priced;
            // the cheapest joins the candidate list below.
            let forced_ab = FORCE_B128_AB.load(std::sync::atomic::Ordering::Relaxed);
            let root_ab = (sb128_search
                && (b128_ab() || forced_ab != 0)
                && cells.len() == 4
                && forced_rect == 0)
                .then(|| {
                    let mut best: Option<(usize, _)> = None;
                    for symbol in 4..=7usize {
                        if forced_ab != 0 && usize::from(forced_ab) != symbol {
                            continue;
                        }
                        let Some(found) = search_root_128_ab(
                            &mut luma,
                            &mut chroma,
                            &mut grid,
                            (root_r, root_c),
                            (rows, cols),
                            (header.mi_cols, header.mi_rows),
                            search,
                            &tpl_factors,
                            sb_q.as_ref(),
                            sb_cols,
                            tpl_cells_x,
                            base_q_idx,
                            reference,
                            &root_compound,
                            symbol,
                            fctx,
                        ) else {
                            continue;
                        };
                        if best
                            .as_ref()
                            .is_none_or(|b: &(usize, (f64, f64, _, _))| found.0 < b.1.0)
                        {
                            best = Some((symbol, found));
                        }
                    }
                    best
                })
                .flatten();
            for (sb_r, sb_c) in cells {
                // lane-b64: the whole superblock as ONE 64x64 block, tried before
                // its quadrants are (`search_skip_64`) and compared against the
                // four of them below. Only a superblock wholly inside the true
                // frame is offered it, and (lane-b64b, `EC_AV1_B64EDGE`) one the
                // edge CUTS THROUGH: the spec's own `has_cols`/`has_rows` ask
                // only that the block's half is inside, and the trial is ranked
                // over the inside part alone ([`Plane::block_sse`]).
                let (x64, y64) = (sb_c * SUPERBLOCK, sb_r * SUPERBLOCK);
                let whole_inside = (sb_c as u32 + 1) * crate::tile::SB_MI <= header.mi_cols
                    && (sb_r as u32 + 1) * crate::tile::SB_MI <= header.mi_rows
                    && x64 + SUPERBLOCK <= luma.true_width
                    && y64 + SUPERBLOCK <= luma.true_height;
                let edge_inside = b64_edge()
                    && crate::tile::has_half(
                        sb_c as u32 * crate::tile::SB_MI,
                        crate::tile::SB_MI,
                        header.mi_cols,
                    )
                    && crate::tile::has_half(
                        sb_r as u32 * crate::tile::SB_MI,
                        crate::tile::SB_MI,
                        header.mi_rows,
                    );
                let sb64_legal = b64_root()
                    && sb_r * 2 + 1 < rows
                    && sb_c * 2 + 1 < cols
                    && (whole_inside || edge_inside);
                // One lambda for the whole superblock, the same mean the 32x32
                // level takes over its own four tpl cells: the 64-vs-four-32
                // comparison below weighs one cost against four, so both sides
                // have to be priced at the same lambda.
                let sb_search = match &tpl_factors {
                    // lane-deltaq: with a per-superblock quantizer the lambda
                    // comes from that quantizer, not from the raw map factor --
                    // the two agree exactly at `EC_AV1_DELTAQ_K=1` unclamped.
                    Some(_) if sb_q.is_some() => sb_q_search(
                        sb_q.as_ref().expect("armed"),
                        sb_cols,
                        base_q_idx,
                        (sb_r, sb_c),
                        search,
                    ),
                    Some(f) if sb64_legal => {
                        let mean = (0..16)
                            .map(|i| f[(sb_r * 4 + i / 4) * tpl_cells_x + sb_c * 4 + i % 4])
                            .sum::<f64>()
                            / 16.0;
                        Search {
                            lambda: search.lambda * mean,
                            ..search
                        }
                    }
                    _ => search,
                };
                let mark = blocks.len();
                let mut sb64: Option<(f64, BlockCoeffs, [(Vec<u8>, Vec<CoefCtx>); 3])> = None;
                let mut sb_cost = 0.0f64;
                if sb64_legal {
                    let base = snapshot(&luma, &chroma, (x64, y64), SUPERBLOCK);
                    let (mi_row, mi_col) = (sb_r * 16, sb_c * 16);
                    let stack =
                        find_mv_stack(&grid, mi_row, mi_col, 16, 16, LAST_FRAME, mi_cols, mi_rows);
                    // lane-b64b: the `LAST` + extra-reference pairs at this
                    // superblock's own 16x16-mi window, the same stacks the tile
                    // writer's `Whole64` arm rebuilds. Empty unless the frame
                    // codes `reference_select`, which is what gates the syntax.
                    // `!screen` for the same reason the residual arm is gated
                    // there ([`b64_residual`]): a desktop capture's superblocks
                    // are flat runs the skip arm already codes for nothing, and
                    // MEASURED, compound at the root cost it +33.4/-23.8 ->
                    // +33.7/-23.6 while both film rows gained (step 1's table).
                    let sb_compound: Vec<(i8, &Picture, crate::mvstack::CompoundMvStack)> =
                        if header.reference_select && b64_compound() && !screen {
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
                                            &grid,
                                            mi_row,
                                            mi_col,
                                            16,
                                            16,
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
                        } else {
                            Vec::new()
                        };
                    if let Some((cost, block)) = search_skip_64(
                        &mut luma,
                        &mut chroma,
                        (x64, y64),
                        (SUPERBLOCK, SUPERBLOCK),
                        &sb_search,
                        b64_residual() && !screen,
                        reference,
                        &stack,
                        &sb_compound,
                        fctx,
                    ) {
                        let after = snapshot(&luma, &chroma, (x64, y64), SUPERBLOCK);
                        restore(&mut luma, &mut chroma, (x64, y64), SUPERBLOCK, &base);
                        let cost = cost
                            + sb_search.lambda * crate::tile::partition_bits(SUPERBLOCK, false);
                        sb64 = Some((cost, block, after));
                    }
                }
                // A superblock whose 64x64 trial already codes almost nothing per
                // pixel is taken outright and its quadrants are never searched --
                // the same `split_rd_breakout` shape the 32x32 level uses one
                // size up, and where this lever BUYS wall instead of spending it.
                let early64 = sb64.as_ref().is_some_and(|(c, _, _)| {
                    split_rd_breakout_at(b64_breakout_threshold(), *c, SUPERBLOCK, sb_search.lambda)
                });
                for quadrant in 0..if early64 { 0 } else { 4 } {
                    let (r32, c32) = (sb_r * 2 + quadrant / 2, sb_c * 2 + quadrant % 2);
                    // lane-av1tpl2: this 32x32 block's own lambda -- every RD
                    // decision under it (partition, mode, tx, motion) reads
                    // `search.lambda`, so scaling the one field here scales all
                    // of them. 32x32 and not the 16x16 the map is built at
                    // because the whole-vs-split comparison right below weighs
                    // one 32x32 cost against four 16x16 ones: a per-leaf lambda
                    // would make those two numbers incomparable.
                    let search = match &tpl_factors {
                        // lane-deltaq: the quantizer is a SUPERBLOCK-level
                        // symbol, so all four quadrants take their superblock's
                        // own qindex and the lambda it implies. No granularity is
                        // lost: the shipped map (`EC_AV1_TPL_SB`) is already one
                        // factor per superblock, shared by its sixteen cells.
                        Some(_) if sb_q.is_some() => sb_q_search(
                            sb_q.as_ref().expect("armed"),
                            sb_cols,
                            base_q_idx,
                            (sb_r, sb_c),
                            search,
                        ),
                        Some(f) => {
                            let mean = (0..4)
                                .map(|i| f[(r32 * 2 + i / 2) * tpl_cells_x + c32 * 2 + i % 2])
                                .sum::<f64>()
                                / 4.0;
                            Search {
                                lambda: search.lambda * mean,
                                ..search
                            }
                        }
                        None => search,
                    };
                    // Same filter as the tile writer's: a quadrant whose own mi
                    // origin is not inside the true frame is never coded.
                    if r32 >= rows || c32 >= cols {
                        continue;
                    }
                    // spec `decode_partition`'s hasRows/hasCols recomputed at this
                    // 32x32 block's own half (`crate::tile::has_half`), same as
                    // the key frame search: the true frame edge can fall inside a
                    // quadrant a superblock-level check already let through, and
                    // such a quadrant cannot be left whole -- it must split into
                    // the 16x16 leaves that are actually inside the true frame
                    // (`crate::tile::sb_coeff_inter_frame_tile`'s `Quadrant::Split`
                    // arm).
                    let (has_cols32, has_rows32) = (
                        crate::tile::has_half(
                            c32 as u32 * crate::tile::BLOCK_MI,
                            crate::tile::BLOCK_MI,
                            header.mi_cols,
                        ),
                        crate::tile::has_half(
                            r32 as u32 * crate::tile::BLOCK_MI,
                            crate::tile::BLOCK_MI,
                            header.mi_rows,
                        ),
                    );
                    if has_cols32 && has_rows32 {
                        let (x, y) = (c32 * BLOCK, r32 * BLOCK);
                        let (mi_row, mi_col) = (r32 * 8, c32 * 8);
                        let stack = find_mv_stack(
                            &grid, mi_row, mi_col, 8, 8, LAST_FRAME, mi_cols, mi_rows,
                        );
                        let extra_stacks: Vec<(i8, &Picture, MvStack)> = [
                            (crate::mvstack::LAST2_FRAME, last2),
                            (crate::mvstack::GOLDEN_FRAME, golden),
                            (crate::mvstack::ALTREF_FRAME, altref),
                        ]
                        .into_iter()
                        .filter_map(|(r, pic)| {
                            pic.map(|p| {
                                (
                                    r,
                                    p,
                                    find_mv_stack(&grid, mi_row, mi_col, 8, 8, r, mi_cols, mi_rows),
                                )
                            })
                        })
                        .collect();
                        let extra: Vec<(i8, &Picture, &MvStack)> =
                            extra_stacks.iter().map(|(r, p, st)| (*r, *p, st)).collect();
                        // The COMPOUND stack of each `LAST` + extra pair, off the
                        // same grid (and the same armed sign bias) the tile writer
                        // rebuilds it from. Empty unless this frame codes
                        // `reference_select`, which is what gates the syntax.
                        let compound: Vec<(i8, &Picture, crate::mvstack::CompoundMvStack)> =
                            if header.reference_select {
                                extra_stacks
                                    .iter()
                                    .map(|&(r, p, _)| {
                                        (
                                            r,
                                            p,
                                            crate::mvstack::find_mv_stack_compound(
                                                &grid,
                                                mi_row,
                                                mi_col,
                                                8,
                                                8,
                                                (crate::mvstack::LAST_FRAME, r),
                                                mi_cols,
                                                mi_rows,
                                                grid.sign_bias_table(),
                                                &[(0, 0); 7],
                                                None,
                                            ),
                                        )
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            };

                        let base = snapshot(&luma, &chroma, (x, y), BLOCK);
                        let (block, mut cost_whole) = search_inter_block(
                            &mut luma,
                            &mut chroma,
                            (x, y),
                            &search,
                            &mode_bits_table,
                            reference,
                            &stack,
                            &extra,
                            &compound,
                            &grid,
                            (mi_row, mi_col),
                            (mi_rows, mi_cols),
                            fctx,
                        );
                        cost_whole += search.lambda * partition_bits(BLOCK, false);
                        let after_whole = snapshot(&luma, &chroma, (x, y), BLOCK);

                        // What four 16x16 leaves cost instead (spec
                        // PARTITION_SPLIT at BLOCK_32X32, which
                        // `sb_coeff_inter_frame_tile`'s `Quadrant::Split` arm
                        // already writes for a straddling quadrant). Each leaf is
                        // searched against the reconstruction -- and the `mi`
                        // grid -- the ones before it left, exactly as the writer
                        // and the decoder will read them.
                        //
                        // A block the whole-32 search left skipped (no residual
                        // at all) is not offered the split: libaom prunes the
                        // same way at cpu-used 6, and it is where the wall would
                        // otherwise go on static content.
                        let leaf_positions: Vec<(usize, usize)> =
                            (0..4).map(|i| (r32 * 2 + i / 2, c32 * 2 + i % 2)).collect();
                        let leaves_legal = leaf_positions.iter().all(|&(sr, sc)| {
                            crate::tile::has_half(
                                sc as u32 * crate::tile::SUB_MI,
                                crate::tile::SUB_MI,
                                header.mi_cols,
                            ) && crate::tile::has_half(
                                sr as u32 * crate::tile::SUB_MI,
                                crate::tile::SUB_MI,
                                header.mi_rows,
                            )
                        });
                        // The breakout above: a 32x32 block that codes almost
                        // nothing is not offered the split at all.
                        let breakout = split_breakout_coeffs();
                        let split = (!block.skip
                            && !split_rd_breakout(cost_whole, BLOCK, search.lambda)
                            && (breakout == 0 || block.luma.len() > 4 * breakout)
                            && leaves_legal
                            && split_inter_blocks())
                        .then(|| {
                            restore(&mut luma, &mut chroma, (x, y), BLOCK, &base);
                            let mut cost = search.lambda * partition_bits(BLOCK, true);
                            let mut leaves = Vec::with_capacity(4);
                            for &(sr, sc) in &leaf_positions {
                                let (mi_row, mi_col) = (sr * 4, sc * 4);
                                let (lx, ly) = (sc * SUB, sr * SUB);
                                let leaf_base = snapshot(&luma, &chroma, (lx, ly), SUB);
                                let stack = find_mv_stack(
                                    &grid, mi_row, mi_col, 4, 4, LAST_FRAME, mi_cols, mi_rows,
                                );
                                let cstacks = leaf_compound_stacks(
                                    &grid,
                                    mi_row,
                                    mi_col,
                                    4,
                                    4,
                                    mi_cols,
                                    mi_rows,
                                    header.reference_select,
                                    golden,
                                    altref,
                                );
                                let (leaf, leaf_cost) = code_square_inter(
                                    &mut luma,
                                    &mut chroma,
                                    (lx, ly),
                                    SUB,
                                    &search,
                                    &mode_bits_table,
                                    reference,
                                    &stack,
                                    &cstacks,
                                    &grid,
                                    (mi_row, mi_col),
                                    (mi_rows, mi_cols),
                                    &obmc_refs,
                                    fctx,
                                );
                                let leaf_cost =
                                    leaf_cost + search.lambda * partition_bits(SUB, false);
                                let after_leaf = snapshot(&luma, &chroma, (lx, ly), SUB);

                                // And what four 8x8 leaves cost instead --
                                // the same tree one step further down, which
                                // the writer codes as a real PARTITION_SPLIT
                                // at BLOCK_16X16 (lane-av1rd2). Same
                                // skipped-block prune as the level above.
                                let eight = (!leaf.skip
                                    && !split_rd_breakout(leaf_cost, SUB, search.lambda)
                                    && (breakout == 0 || leaf.luma.len() > breakout)
                                    && split_inter_8()
                                    // lane-arfmode: arm 2 makes 16x16 the top
                                    // ARF's leaf floor -- the RD-optional 8x8
                                    // comparison is not offered. Counted only
                                    // when every other prune let it through,
                                    // so the counter means "refused", not
                                    // "never considered". The spec-forced 8x8s
                                    // at a straddling 16x16 (the
                                    // `sub_positions` path below) are a
                                    // different site and stay.
                                    && (search.arf_leaf8 || arfmode_hit_and_no()))
                                .then(|| {
                                    restore(&mut luma, &mut chroma, (lx, ly), SUB, &leaf_base);
                                    let mut cost8 = search.lambda
                                        * (partition_bits(SUB, true)
                                            + 4.0 * partition_bits(8, false));
                                    let mut eight = Vec::with_capacity(4);
                                    for i in 0..4 {
                                        let (mr, mc) = (mi_row + (i / 2) * 2, mi_col + (i % 2) * 2);
                                        let stack8 = find_mv_stack(
                                            &grid, mr, mc, 2, 2, LAST_FRAME, mi_cols, mi_rows,
                                        );
                                        let cstacks8: Vec<(
                                            i8,
                                            &Picture,
                                            crate::mvstack::CompoundMvStack,
                                        )> = if header.reference_select && leaf_compound() {
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
                                                            &grid,
                                                            mr,
                                                            mc,
                                                            2,
                                                            2,
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
                                        } else {
                                            Vec::new()
                                        };
                                        let (leaf8, cost) = code_square_inter(
                                            &mut luma,
                                            &mut chroma,
                                            (lx + (i % 2) * 8, ly + (i / 2) * 8),
                                            8,
                                            &search,
                                            &mode_bits_table,
                                            reference,
                                            &stack8,
                                            &cstacks8,
                                            &grid,
                                            (mr, mc),
                                            (mi_rows, mi_cols),
                                            &obmc_refs,
                                            fctx,
                                        );
                                        cost8 += cost;
                                        record_mi(&mut grid, mr, mc, 2, leaf8.inter, leaf8.skip);
                                        eight.push(leaf8);
                                    }
                                    (eight, cost8)
                                });
                                match eight {
                                    Some((eight, cost8)) if cost8 < leaf_cost => {
                                        partition_hit(3);
                                        cost += cost8;
                                        leaves.push(BlockCoeffs {
                                            eight: Some(eight),
                                            ..BlockCoeffs::default()
                                        });
                                        continue;
                                    }
                                    Some(_) => {
                                        restore(&mut luma, &mut chroma, (lx, ly), SUB, &after_leaf)
                                    }
                                    None => {}
                                }
                                partition_hit(2);
                                cost += leaf_cost;
                                record_mi(&mut grid, mi_row, mi_col, 4, leaf.inter, leaf.skip);
                                leaves.push(leaf);
                            }
                            (leaves, cost)
                        });
                        if let Some((leaves, cost)) = split {
                            if cost < cost_whole {
                                // The leaves' own `mi` cells are already in the
                                // grid, and their reconstruction is what the
                                // planes hold.
                                partition_hit(1);
                                sb_cost += cost;
                                blocks.push((r32 * cols + c32, Quadrant::Split(leaves)));
                                continue;
                            }
                            restore(&mut luma, &mut chroma, (x, y), BLOCK, &after_whole);
                        }
                        partition_hit(0);
                        sb_cost += cost_whole;
                        record_mi(&mut grid, mi_row, mi_col, 8, block.inter, block.skip);
                        blocks.push((r32 * cols + c32, Quadrant::Whole(block)));
                    } else {
                        // The sub-positions actually inside the true frame, same
                        // filter as `sb_coeff_inter_frame_tile`'s `sub_positions`.
                        let sub_positions: Vec<(usize, usize)> = (0..4)
                            .map(|i| (r32 * 2 + i / 2, c32 * 2 + i % 2))
                            .filter(|&(sr, sc)| {
                                (sr as u32) * crate::tile::SUB_MI < header.mi_rows
                                    && (sc as u32) * crate::tile::SUB_MI < header.mi_cols
                            })
                            .collect();
                        // Each leaf searches intra against its own `NEARESTMV`
                        // candidate (`code_square_inter`), same real-inter choice
                        // `search_inter_block` makes for a whole 32x32 block, at
                        // this leaf's own 4x4-mi mv-stack window -- unless the
                        // leaf's own half itself straddles the true edge, which
                        // it codes as two (or one, at a true corner) 8x8 leaves
                        // (`crate::tile::write_leaf8`'s inter counterpart),
                        // mirroring the key frame search's straddling-16x16
                        // handling above (lane-av1inter8).
                        let mut leaves = Vec::with_capacity(sub_positions.len());
                        for (sr, sc) in sub_positions {
                            let (has_cols16, has_rows16) = (
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
                            );
                            if has_cols16 && has_rows16 {
                                let (x, y) = (sc * SUB, sr * SUB);
                                let (mi_row, mi_col) = (sr * 4, sc * 4);
                                let stack = find_mv_stack(
                                    &grid, mi_row, mi_col, 4, 4, LAST_FRAME, mi_cols, mi_rows,
                                );
                                let cstacks = leaf_compound_stacks(
                                    &grid,
                                    mi_row,
                                    mi_col,
                                    4,
                                    4,
                                    mi_cols,
                                    mi_rows,
                                    header.reference_select,
                                    golden,
                                    altref,
                                );
                                let (block, _) = code_square_inter(
                                    &mut luma,
                                    &mut chroma,
                                    (x, y),
                                    SUB,
                                    &search,
                                    &mode_bits_table,
                                    reference,
                                    &stack,
                                    &cstacks,
                                    &grid,
                                    (mi_row, mi_col),
                                    (mi_rows, mi_cols),
                                    &obmc_refs,
                                    fctx,
                                );
                                // One publication point for every coded leaf
                                // (`record_mi`): this site used to spell the vote
                                // out with `ref_frame` pinned to LAST and `mv1`
                                // to (0,0), which is the encoder-grid-drift class
                                // -- correct only for as long as a leaf can never
                                // be compound.
                                record_mi(&mut grid, mi_row, mi_col, 4, block.inter, block.skip);
                                leaves.push(block);
                            } else {
                                let (x_sub, y_sub) = (sc * SUB, sr * SUB);
                                let (mi_row0, mi_col0) = (sr * 4, sc * 4);
                                let mut eight = Vec::with_capacity(2);
                                for i in 0..4 {
                                    let leaf_x = x_sub + (i % 2) * 8;
                                    let leaf_y = y_sub + (i / 2) * 8;
                                    if leaf_x >= luma.true_width || leaf_y >= luma.true_height {
                                        continue;
                                    }
                                    let (mi_row, mi_col) =
                                        (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2);
                                    let stack = find_mv_stack(
                                        &grid, mi_row, mi_col, 2, 2, LAST_FRAME, mi_cols, mi_rows,
                                    );
                                    let cstacks = leaf_compound_stacks(
                                        &grid,
                                        mi_row,
                                        mi_col,
                                        2,
                                        2,
                                        mi_cols,
                                        mi_rows,
                                        header.reference_select,
                                        golden,
                                        altref,
                                    );
                                    let (leaf, _) = code_square_inter(
                                        &mut luma,
                                        &mut chroma,
                                        (leaf_x, leaf_y),
                                        8,
                                        &search,
                                        &mode_bits_table,
                                        reference,
                                        &stack,
                                        &cstacks,
                                        &grid,
                                        (mi_row, mi_col),
                                        (mi_rows, mi_cols),
                                        &obmc_refs,
                                        fctx,
                                    );
                                    // Same publication point as the 16x16
                                    // straddling leaf above (`record_mi`).
                                    record_mi(&mut grid, mi_row, mi_col, 2, leaf.inter, leaf.skip);
                                    eight.push(leaf);
                                }
                                leaves.push(BlockCoeffs {
                                    eight: Some(eight),
                                    ..BlockCoeffs::default()
                                });
                            }
                        }
                        blocks.push((r32 * cols + c32, Quadrant::Split(leaves)));
                    }
                }
                // The 64x64 root against what its four quadrants really cost.
                if let Some((cost64, block, after)) = sb64
                    && (early64
                        || cost64
                            < sb_cost
                                + sb_search.lambda * crate::tile::partition_bits(SUPERBLOCK, true))
                {
                    B64_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if block.inter.is_some_and(|i| i.ref1.is_some()) {
                        B64_COMPOUND_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    if !whole_inside {
                        B64_EDGE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    blocks.truncate(mark);
                    restore(&mut luma, &mut chroma, (x64, y64), SUPERBLOCK, &after);
                    // Overwrites every 16x16 mi cell the quadrant searches
                    // published under this superblock, so the next block's stack
                    // is built off the block that was really coded.
                    record_mi(&mut grid, sb_r * 16, sb_c * 16, 16, block.inter, block.skip);
                    blocks.push(((sb_r * 2) * cols + sb_c * 2, Quadrant::Whole64(block)));
                    for q in 1..4 {
                        let (r32, c32) = (sb_r * 2 + q / 2, sb_c * 2 + q % 2);
                        blocks.push((r32 * cols + c32, Quadrant::Covered));
                    }
                    root_cost += cost64;
                } else {
                    root_cost += sb_cost;
                }
            }
            // lane-b128: the whole 128x128 root (`PARTITION_NONE` at the root's
            // `partition_w128` symbol) against what its four superblocks really
            // cost, the same weighing the 64 root does one size down. Both sides
            // carry their own partition symbol, so the comparison is complete.
            // The root candidates, each tagged with the PARTITION symbol its
            // writer codes: 0 NONE, 1 HORZ, 2 VERT (lane-b128hv), 4..=7 the AB
            // shapes (lane-ab128).
            let mut cands: Vec<(f64, f64, usize, Vec<BlockCoeffs>, _)> = Vec::new();
            if let Some((c, l, b, a)) = root128 {
                cands.push((c, l, 0, vec![b], a));
            }
            if let Some((horz, (c, l, halves, a))) = root_rect {
                cands.push((c, l, usize::from(!horz) + 1, halves.to_vec(), a));
            }
            if let Some((symbol, (c, l, pieces, a))) = root_ab {
                cands.push((c, l, symbol, pieces.to_vec(), a));
            }
            // The witness knobs drop every other arm outright: on synthetic
            // content the whole block always wins its own comparison.
            if forced_rect != 0 {
                cands.retain(|c| c.2 == 1 || c.2 == 2);
            }
            if forced_ab != 0 {
                cands.retain(|c| c.2 == usize::from(forced_ab));
            }
            cands.sort_by(|a, b| a.0.total_cmp(&b.0));
            // lane-rectdq class sweep: under a 128 superblock grid the FOUR-
            // superblock arm codes the delta_q group too -- the Whole64 writer
            // (tile.rs `sb_coeff_inter_frame_tile_cdfs`, the `write_delta_q` call
            // with `!sb128_armed()`) and every smaller shape below it pass
            // `is_whole_sb = false` under a 128 grid, so the arm's first
            // (root-origin) superblock codes the group even when skipped. Only the skipped 128
            // NONE root escapes it, so the split side is charged here at the same
            // root lambda the candidates are priced with.
            let split_dq =
                delta_q_bits_at_root(sb_q.as_ref(), sb_cols, base_q_idx, dq_res, (root_r, root_c));
            if let Some((cost128, lambda128, shape, halves, after)) = cands.into_iter().next()
                && (cost128
                < root_cost + lambda128 * (crate::tile::partition_bits(128, true) + split_dq)
                // The witness knobs take the root whatever it costs, so the
                // forced block really reaches the writer.
                || FORCE_B128_ROOT.load(std::sync::atomic::Ordering::Relaxed)
                || forced_rect != 0
                || forced_ab != 0)
            {
                let (x128, y128) = (root_c * SUPERBLOCK, root_r * SUPERBLOCK);
                blocks.truncate(mark128);
                restore(&mut luma, &mut chroma, (x128, y128), SUPERBLOCK * 2, &after);
                let at = (root_r * 2) * cols + root_c * 2;
                match shape {
                    0 => {
                        B128_NONE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let block = halves.into_iter().next().expect("the NONE arm's block");
                        record_mi(
                            &mut grid,
                            root_r * 16,
                            root_c * 16,
                            32,
                            block.inter,
                            block.skip,
                        );
                        blocks.push((at, Quadrant::Whole128(block)));
                    }
                    // lane-ab128: three pieces, published at their own footprints
                    // in the writer's own order ([`ab128_pieces`]).
                    symbol @ 4..=7 => {
                        B128_AB_HITS[symbol - 4].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        for (((mr, mc), (w, h)), piece) in
                            ab128_pieces(symbol, (root_r * 16, root_c * 16))
                                .iter()
                                .zip(&halves)
                        {
                            record_mi_rect(
                                &mut grid,
                                *mr,
                                *mc,
                                (w / 4, h / 4),
                                piece.inter,
                                piece.skip,
                            );
                        }
                        blocks.push((at, Quadrant::Ab128(symbol as u8, halves)));
                    }
                    symbol => {
                        let horz = symbol == 1;
                        if horz {
                            B128_HORZ_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        } else {
                            B128_VERT_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        let (w_mi, h_mi) = if horz { (32usize, 16usize) } else { (16, 32) };
                        for (i, half) in halves.iter().enumerate() {
                            let (mr, mc) = if horz {
                                (root_r * 16 + i * 16, root_c * 16)
                            } else {
                                (root_r * 16, root_c * 16 + i * 16)
                            };
                            record_mi_rect(&mut grid, mr, mc, (w_mi, h_mi), half.inter, half.skip);
                        }
                        blocks.push((at, Quadrant::Rect128(horz, halves)));
                    }
                }
                for q in 1..16 {
                    let (r32, c32) = (root_r * 2 + q / 4, root_c * 2 + q % 4);
                    blocks.push((r32 * cols + c32, Quadrant::Covered));
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
        Ok((blocks, pixels))
    };
    // Each tile's rectangles are copied back on this thread, in tile order (the
    // rectangles partition the frame, so the order does not change the bytes).
    let mut searched_blocks = Vec::new();
    for (tile_blocks, pixels) in search_tiles(layout.count(), fctx, search_tile)? {
        put_rect(&mut luma.reconstruction, frame_width, pixels.luma_rect, &pixels.luma);
        for (plane, bytes) in chroma.iter_mut().zip(&pixels.chroma) {
            put_rect(&mut plane.reconstruction, frame_width / 2, pixels.chroma_rect, bytes);
        }
        searched_blocks.push(tile_blocks);
    }
    for (at, quadrant) in searched_blocks.into_iter().flatten() {
        blocks[at] = quadrant;
    }

    // lane-b128, `EC_AV1_B128_CENSUS=1`: how many of this INTER frame's 128
    // superblock roots the search left whole, so a flat BD result is
    // attributable (class `gate-blind-to-feature`). A key frame codes none:
    // only the inter writer has a 128x128 block.
    if crate::envflags::env_flag!("EC_AV1_B128_CENSUS") {
        let whole = blocks
            .iter()
            .filter(|q| matches!(q, Quadrant::Whole128(_)))
            .count();
        let roots = blocks
            .iter()
            .filter(|q| !matches!(q, Quadrant::Covered))
            .count();
        // lane-b128r: of those roots, how many code a REAL residual rather
        // than the skip-only block lane-b128 shipped -- the fire count this
        // arm's own gate row is read against.
        let residual = blocks
            .iter()
            .filter(|q| matches!(q, Quadrant::Whole128(b) if !b.skip))
            .count();
        // lane-b128r: and how many named a second reference (the compound
        // arm at the 128 root).
        let compound = blocks
            .iter()
            .filter(
                |q| matches!(q, Quadrant::Whole128(b) if b.inter.is_some_and(|i| i.ref1.is_some())),
            )
            .count();
        // lane-b128hv: and how many roots took a rect cut instead, per shape
        // -- the HORZ/VERT fire count the gate rows are read against.
        let horz = blocks
            .iter()
            .filter(|q| matches!(q, Quadrant::Rect128(true, _)))
            .count();
        let vert = blocks
            .iter()
            .filter(|q| matches!(q, Quadrant::Rect128(false, _)))
            .count();
        let rect_compound = blocks
            .iter()
            .filter(|q| {
                matches!(q, Quadrant::Rect128(_, hs)
                    if hs.iter().any(|b| b.inter.is_some_and(|i| i.ref1.is_some())))
            })
            .count();
        // lane-ab128: and how many took each AB shape, in PARTITION_HORZ_A
        // .. _VERT_B order -- the per-shape fire count its gate rows are read
        // against.
        let ab: [usize; 4] = std::array::from_fn(|i| {
            blocks
                .iter()
                .filter(|q| matches!(q, Quadrant::Ab128(s, _) if *s as usize == i + 4))
                .count()
        });
        eprintln!(
            "B128 census: inter frame, {whole} whole 128x128 roots ({residual} with a residual, \
             {compound} compound), {horz} HORZ + {vert} VERT roots ({rect_compound} with a \
             compound half), AB roots {}/{}/{}/{} (HORZ_A/HORZ_B/VERT_A/VERT_B), \
             {roots} coded blocks ({:.1}% of the frame's area at 128)",
            ab[0],
            ab[1],
            ab[2],
            ab[3],
            100.0 * ((whole + horz + vert + ab.iter().sum::<usize>()) * 16) as f64
                / blocks.len().max(1) as f64,
        );
    }
    let modes = blocks
        .iter()
        .flat_map(Quadrant::blocks)
        .map(|b| b.mode)
        .collect::<Vec<_>>();
    let inter_block_share = blocks
        .iter()
        .flat_map(Quadrant::blocks)
        .filter(|b| b.inter.is_some())
        .count() as f64
        / modes.len() as f64;
    for block in blocks.iter().flat_map(Quadrant::blocks) {
        if let Some(info) = block.inter {
            let r = (info.ref_frame as usize).min(7);
            REF_FRAME_HITS[r].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    #[cfg(test)]
    record_predicted_bits(crate::tile::predicted_coeff_bits(&blocks, base_q_idx));
    // This frame's header leaves `disable_frame_end_update_cdf` off, so it
    // stores its end-of-tile tables with the counts reset (spec 7.20) and the
    // next frame's writer starts from them -- exactly what
    // `crate::stream::stored_cdfs_for` hands the decoder.
    let (mi_cols, mi_rows) = (header.mi_cols, header.mi_rows);
    let mut cdfs = start_cdfs
        .cloned()
        .unwrap_or_else(|| crate::cdf_state::Cdfs::new(crate::tile::q_ctx_of(base_q_idx)));
    // lane-txi: set per frame, never inherited from the snapshot the
    // previous frame stored -- the content gate can flip the bit between
    // frames (class `per-tile-state-not-reset`).
    cdfs.reduced_tx_set = header.reduced_tx_set;
    let start_cdfs = CdfSnapshot(cdfs);
    let reference_select_bit = header.reference_select;
    let switchable_motion_mode = header.is_motion_mode_switchable;
    let warped_motion = header.allow_warped_motion;
    let order_hint_bits = seq.order_hint_bits;
    let lr_horz = crate::restoration::count_units(header.frame_width, lr_unit()) as u32;
    let lr_vert = crate::restoration::count_units(header.frame_height, lr_unit()) as u32;
    // What this frame stores is the LARGEST tile's end-of-tile tables --
    // its own header's `context_update_tile_id` (spec 7.20), which
    // `tile_info_of` set to exactly that tile.
    let store_tile = layout.largest_tile();
    let end_cdfs = std::cell::RefCell::new(None);
    // As in `encode_key_frame_inner`: every tile starts from this frame's
    // own tables, so no tile depends on any other.
    let code_tiles = |bits: u8,
                      sb_cols: usize,
                      cdef_grid: &[u8],
                      units: &[Option<crate::restoration::WienerInfo>]|
     -> Result<Vec<Vec<u8>>> {
        let (out, stored) = write_tiles(layout.count(), store_tile, |index| {
            crate::tile::arm_cdef_idx(bits, sb_cols, cdef_grid.to_vec());
            crate::tile::arm_delta_q(
                deltaq_res.filter(|_| sb_q.is_some()).unwrap_or(0),
                sb_cols,
                sb_q.clone().unwrap_or_default(),
                base_q_idx,
            );
            crate::tile::arm_lr(lr_unit(), lr_horz, lr_vert, units.to_vec());
            crate::tile::arm_sb128(sb128_on());
            crate::tile::arm_sign_bias(sign_bias);
            // Thread-locals: every tile job arms its own worker.
            crate::tile::arm_reference_select(reference_select_bit);
            crate::tile::arm_motion_mode(switchable_motion_mode);
            crate::tile::arm_warped_motion(warped_motion);
            crate::tile::arm_order_hints(order_hint_bits, order_hint, order_hints);
            crate::tile::arm_high_precision_mv(hp_mv);
            crate::tile::arm_screen(screen);
            crate::tile::arm_frame_mi_dims(frame_mi_dims);
            crate::tile::arm_filter_intra(filter_intra_on());
            let mut cdfs = start_cdfs.0.clone();
            crate::tile::arm_pricing_cdfs(Some(&cdfs), screen);
            // Each tile's own MV grid: a candidate scan never reaches past
            // the tile's bounds, so a grid holding only this tile's own
            // blocks reads exactly as the decoder's frame-wide one does
            // under `MiGrid::set_tile_bounds`.
            let mut mv_grid = MiGrid::new(mi_cols as usize, mi_rows as usize);
            mv_grid.set_sign_bias(sign_bias);
            let bytes = crate::tile::sb_coeff_inter_frame_tile_cdfs(
                mi_cols,
                mi_rows,
                base_q_idx,
                &blocks,
                tx_select,
                &mut cdfs,
                layout.rect(index),
                &mut mv_grid,
            )?;
            Ok((bytes, Some(cdfs)))
        })?;
        if let Some(cdfs) = stored {
            *end_cdfs.borrow_mut() = Some(cdfs);
        }
        Ok(out)
    };
    let mut tiles = code_tiles(0, sb_cols, &[], &[])?;
    // Deblocking, as in `encode_key_frame_inner`: the decoder reconstructs
    // this tile against the same single-slot reference this encoder predicted
    // from, under each candidate level.
    // The reference a DECODER holds is this frame's cropped size, not the
    // encoder's padded coding surface -- `decode_inter_frame_tile` refuses a
    // reference whose height is not the header's own `frame_height` (spec
    // 7.9's compatible-size rule), and the chained-decode round trips prove
    // the two predict identically.
    let (fw, fh) = (header.frame_width as usize, header.frame_height as usize);
    let trial_ref = if (reference.width, reference.height) == (fw, fh) {
        reference.clone()
    } else {
        Picture {
            width: fw,
            height: fh,
            y: crop_plane(&reference.y, reference.width, fw, fh),
            u: crop_plane(&reference.u, reference.width / 2, fw / 2, fh / 2),
            v: crop_plane(&reference.v, reference.width / 2, fw / 2, fh / 2),
        }
    };
    // `RefPix` is indexed by the reference NAME (`LAST_FRAME` = 1,
    // `GOLDEN_FRAME` = 4), not by DPB slot: the trial decode of a frame whose
    // blocks may name GOLDEN needs that plane too, cropped the same way.
    let trial_golden = golden.map(|g| {
        if (g.width, g.height) == (fw, fh) {
            g.clone()
        } else {
            Picture {
                width: fw,
                height: fh,
                y: crop_plane(&g.y, g.width, fw, fh),
                u: crop_plane(&g.u, g.width / 2, fw / 2, fh / 2),
                v: crop_plane(&g.v, g.width / 2, fw / 2, fh / 2),
            }
        }
    });
    let trial_altref = altref.map(|a| {
        if (a.width, a.height) == (fw, fh) {
            a.clone()
        } else {
            Picture {
                width: fw,
                height: fh,
                y: crop_plane(&a.y, a.width, fw, fh),
                u: crop_plane(&a.u, a.width / 2, fw / 2, fh / 2),
                v: crop_plane(&a.v, a.width / 2, fw / 2, fh / 2),
            }
        }
    });
    let trial_last2 = last2.map(|l| {
        if (l.width, l.height) == (fw, fh) {
            l.clone()
        } else {
            Picture {
                width: fw,
                height: fh,
                y: crop_plane(&l.y, l.width, fw, fh),
                u: crop_plane(&l.u, l.width / 2, fw / 2, fh / 2),
                v: crop_plane(&l.v, l.width / 2, fw / 2, fh / 2),
            }
        }
    });
    let mut refs: [Option<&Picture>; 8] = [None; 8];
    refs[1] = Some(&trial_ref);
    refs[2] = trial_last2.as_ref();
    refs[4] = trial_golden.as_ref();
    refs[7] = trial_altref.as_ref();
    let refpix = crate::decode::RefPix::ready(refs);
    // lane-txrd: the frame's luma error BEFORE the loop filters, i.e. the
    // sum the block search believed it was minimising. Against the
    // post-filter per-frame PSNR the gate prints, this separates a trial
    // whose reconstruction is a lie from a filter stage that undoes the win.
    if crate::envflags::env_flag!("EC_AV1_FRAME_PSNR") {
        let sse: u64 = luma
            .reconstruction
            .chunks(luma.width)
            .take(luma.true_height)
            .zip(picture_y8.chunks(luma.width))
            .flat_map(|(r, s)| r[..luma.true_width].iter().zip(&s[..luma.true_width]))
            .map(|(&r, &s)| {
                let d = i64::from(r) - i64::from(s);
                (d * d) as u64
            })
            .sum();
        eprintln!("EC_PREFILTER q={base_q_idx} luma sse={sse}");
    }
    let post = |luma: &Plane<'_>, tag: &str| {
        if !crate::envflags::env_flag!("EC_AV1_FRAME_PSNR") {
            return;
        }
        let sse: u64 = luma
            .reconstruction
            .chunks(luma.width)
            .take(luma.true_height)
            .zip(picture_y8.chunks(luma.width))
            .flat_map(|(r, s)| r[..luma.true_width].iter().zip(&s[..luma.true_width]))
            .map(|(&r, &s)| {
                let d = i64::from(r) - i64::from(s);
                (d * d) as u64
            })
            .sum();
        eprintln!("{tag} q={base_q_idx} luma sse={sse}");
    };
    pick_and_apply_filters(
        &mut header,
        &mut luma,
        &mut chroma,
        [&picture_y8, &picture_u8, &picture_v8],
        // Merge of lane-av1filt with lane-av1tx2: the trial decoder takes
        // this frame's own `tx_select`, so the search runs under inter
        // `TxMode::Select` too (it used to skip and leave every inter frame
        // unfiltered once Select became the default).
        true,
        &mut tiles,
        search.lambda,
        fctx,
        &crate::tile::cdef_unit_owner(&blocks, mi_cols, mi_rows),
        // As in `encode_key_frame_inner`, but this frame's end-of-tile tables
        // are what the NEXT frame writes from, so the winning re-code
        // publishes its own into `end_cdfs` -- the first write's are stale
        // the moment a second tile is coded.
        |bits, sb_cols, grid, units| code_tiles(bits, sb_cols, grid, units),
        |lf, cdef, h, tiles: &[Vec<u8>], lr| {
            let payloads: Vec<&[u8]> = tiles.iter().map(Vec::as_slice).collect();
            crate::decode::decode_inter_frame_tiles_lr(
                &payloads,
                &header_tile_info,
                h.mi_cols,
                h.mi_rows,
                base_q_idx,
                h.frame_width,
                h.frame_height,
                &refpix,
                cdef,
                lf,
                h.allow_high_precision_mv,
                h.force_integer_mv,
                Some(fctx.interp_filter.get()),
                seq.enable_dual_filter,
                h.reference_select,
                // lane-txi: this frame's own bit, not the hardcoded `true`
                // the wrapper used to pass.
                h.reduced_tx_set,
                tx_select,
                lr,
                // The trial decode re-reads the tile the writer just wrote,
                // so it starts from the same tables the writer did (this
                // frame's `start_cdfs`), not from the defaults.
                Some(start_cdfs.0.clone()),
                sign_bias,
                h.allow_screen_content_tools,
                switchable_motion_mode,
                h.allow_warped_motion,
                seq.enable_filter_intra,
                // lane-deltaq: this frame's own delta_q/delta_lf params, so
                // the trial decode reads the per-superblock `delta_qindex`
                // syntax the writer just wrote.
                h.delta,
                fctx,
            )
        },
    )?;
    post(&luma, "EC_POSTFILTER");
    let mut cdfs = end_cdfs
        .into_inner()
        .unwrap_or_else(|| start_cdfs.0.clone());
    cdfs.reset_counts();
    let next_cdfs = CdfSnapshot(cdfs);
    let tile_size_bytes = crate::frame::tile_size_bytes_for(&tiles);
    header.tile_info.tile_size_bytes = tile_size_bytes;
    let tile = crate::frame::tile_group_payload(&tiles, tile_size_bytes);
    let mut stream = temporal_delimiter();
    stream.extend_from_slice(&frame_obu(&seq, &header, &tile)?);

    let [u, v] = chroma;
    Ok(Encoded {
        stream,
        modes,
        inter_block_share,
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
        switchable_motion_mode,
        screen,
        reduced_tx_set: header.reduced_tx_set,
        start_cdfs: Box::new(start_cdfs),
        next_cdfs: Box::new(next_cdfs),
        loop_filter: header.loop_filter,
        cdef: header.cdef,
        loop_restoration: header.loop_restoration,
        // An inter frame never allows intrabc (spec 5.9.2 codes the bit on
        // intra frames only).
        allow_intrabc: false,
        delta: header.delta,
    })
}

/// What [`encode_sequence`] produced: the concatenated AV1 stream -- a key
/// frame's temporal unit followed by one inter frame's temporal unit per
/// remaining picture, all sharing the key frame's sequence header -- and
/// each frame's own [`Encoded`], in coding order.
#[derive(Clone, Debug)]
pub struct EncodedSequence {
    /// The whole stream: `frames[0].stream` (a temporal delimiter, the
    /// sequence header and the key frame) followed by each later frame's
    /// `stream` (a temporal delimiter and that frame's OBU alone; it reuses
    /// the sequence header the key frame carried).
    pub stream: Vec<u8>,
    /// One entry per picture, in DISPLAY order — the order a decoder outputs
    /// them and the order `pictures` came in. Without a pyramid that is also
    /// the coding order; with one the two differ, and `coding_order` is the
    /// map between them.
    pub frames: Vec<Encoded>,
    /// The index into `frames` of each coded frame, in CODING order — what a
    /// per-coded-frame census (`take_predicted_bits`) must be zipped against.
    /// `0..frames.len()` on the flat path.
    pub coding_order: Vec<usize>,
}

/// Encodes a sequence of pictures as a key frame followed by one inter frame
/// per remaining picture, each predicting from the previous frame's own
/// decoded reconstruction — never from the source, which is what a decoder
/// cannot see. Every picture must be the same even size (any even width and
/// height — each is padded to a whole number of 64x64 superblocks
/// internally, which is what lets an inter frame's reference always match
/// the frame being coded against it), and `pictures` must be non-empty.
///
/// # Errors
/// Returns an error when `pictures` is empty, when any picture besides the
/// first is not the same size as the first, or under the same conditions
/// [`encode_key_frame`] and the inter frame path do.
pub(crate) fn encode_sequence_with_ctx(
    pictures: &[Picture],
    base_q_idx: u8,
    deadzone: f64,
    fctx: &crate::decode::FrameCtx,
) -> Result<EncodedSequence> {
    let Some((first, rest)) = pictures.split_first() else {
        return Err(Error::unsupported(
            "AV1 encode",
            "a sequence needs at least one picture",
        ));
    };
    first.check_even()?;
    let render = (first.width, first.height);
    // THE CODING PYRAMID (lane-av1pyrdef): a sequence of camera material is
    // coded as mini-GOPs -- a hidden ALTREF ahead of its display position,
    // the leaves that predict backward off it, and a `show_existing_frame`
    // header that re-outputs it -- unless `EC_AV1_PYRAMID` turns it off for
    // an A/B. Screen content stays flat (the content gate in
    // `Av1Encoder::encode_frames`, whose detector this asks first so the
    // whole sequence path is decided in one place), and so does a run that
    // asks for a quantizer rounding offset the facade does not carry: the
    // facade codes at `encoder::DEADZONE` (0.5), so any other deadzone --
    // the ablations' -- must keep the flat path rather than be silently
    // ignored.
    //
    // The reordering itself is NOT reimplemented here: it is the facade's
    // own driver ([`crate::encoder::Av1Encoder::encode_sequence_pyramid`]),
    // called with this sequence's shape, so the two paths cannot drift.
    let pyramid = crate::encoder::Pyramid::from_env()
        .filter(|_| !rest.is_empty() && (deadzone - 0.5).abs() < f64::EPSILON)
        .filter(|_| !picture_is_screen(first));
    LAST_SEQ_PYRAMID.with(|c| c.set(pyramid));
    if let Some(pyramid) = pyramid {
        for picture in rest {
            picture.check_even()?;
            if (picture.width, picture.height) != render {
                return Err(Error::unsupported(
                    "AV1 encode",
                    "every picture in a sequence must be the same size as the first",
                ));
            }
        }
        return crate::encoder::Av1Encoder::encode_sequence_pyramid(
            pictures,
            base_q_idx,
            pyramid,
            armed_tiles(),
        );
    }
    // The key frame and every inter frame after it are coded at the same
    // padded size, so the reference each inter frame predicts from -- the
    // previous frame's own uncropped reconstruction, at the size a decoder's
    // reference buffer actually holds -- always matches the frame being
    // coded against it.
    let key = encode_key_frame_inner(
        &first.padded_to(SUPERBLOCK),
        base_q_idx,
        deadzone,
        &KEY_FRAME_MODES,
        split_blocks(),
        render,
        unspecified_color_config(),
        fctx,
    )?;
    let mut stream = key.stream.clone();
    // Slot 1 keeps the key frame's own reconstruction for the whole GOP (the
    // key frame refreshes every slot, each inter frame only slot 0), which is
    // what `GOLDEN_FRAME` names.
    let golden = key.reconstruction.clone();
    // The frame two back, once there is one that is not the key frame
    // itself (which `GOLDEN_FRAME` already names).
    let mut prev2: Option<Picture> = None;
    let mut carried = key.next_cdfs.clone();
    let mut reference = key.reconstruction.clone();
    let mut frames = vec![crop_encoded(&key, render.0, render.1)];
    for (i, picture) in rest.iter().enumerate() {
        picture.check_even()?;
        // lane-av1tpl2: the lookahead window the per-block lambda weighting
        // propagates back through -- the next [`tpl_depth`] - 1 pictures'
        // sources at this frame's coded (padded) size. The tail of the
        // sequence gets a shorter window, and the last frame none at all.
        let lookahead: Vec<Picture> = rest[(i + 1).min(rest.len())..]
            .iter()
            .take(tpl_depth() - 1)
            .map(|n| n.padded_to(SUPERBLOCK))
            .collect();
        if (picture.width, picture.height) != render {
            return Err(Error::unsupported(
                "AV1 encode",
                "every picture in a sequence must be the same size as the first",
            ));
        }
        let order_hint = (i + 1) as u32;
        let inter = encode_inter_frame(
            &picture.padded_to(SUPERBLOCK),
            &reference,
            base_q_idx,
            deadzone,
            order_hint,
            flat_order_hints(order_hint, 0, prev2.is_some()),
            render,
            Some(&carried.0),
            Some(&golden),
            prev2.as_ref(),
            None,
            fctx,
            None,
            &lookahead,
        )?;
        stream.extend_from_slice(&inter.stream);
        carried = inter.next_cdfs.clone();
        prev2 = Some(std::mem::replace(
            &mut reference,
            inter.reconstruction.clone(),
        ));
        frames.push(crop_encoded(&inter, render.0, render.1));
    }
    Ok(EncodedSequence {
        stream,
        coding_order: (0..frames.len()).collect(),
        frames,
    })
}
