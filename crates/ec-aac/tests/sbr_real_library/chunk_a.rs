//! SBR real-library checks and probes, part A.

use super::*;

/// Root-cause triage (round-51): is a real file's SBR-chain break actually
/// downstream of a broken LC core, or does the core decode cleanly and the
/// break lives only in the SBR chain layered on top of it? Prints a
/// core-rate correlation per channel; not asserted on, this is a diagnostic,
/// not a regression gate (the asserted gate is
/// `sbr_real_library_matches_reference`, which already covers the full-band
/// SBR-reconstructed output).
///
/// Round-61: this printed corr 0.402349 for reeln ch0 while that file's
/// core is provably fine (the asserted reference gate scores its below-5kHz
/// band 0.999988, and `full_chain_low_band_matches_own_core` scores this very
/// core-only PCM 0.998 against our own full chain). The number was the
/// instrument, not the codec: `robust_lag_topk`'s stride-11 coarse grid
/// cannot see a 1-sample-wide correlation peak, so its top-K was ranked from
/// noise. At the true lag -- 481, ffmpeg's resampler delay, the same lag FMJ
/// reads -- these two signals correlate 0.999845. The search here is now
/// stride-1 (`exhaustive_lag_correlation`), and both sides are low-passed at
/// `0.9 * crossover` first, since only the reference side (ffmpeg's full SBR
/// reconstruction, resampled down to the core rate) can hold anything above
/// the crossover at all.
#[test]
pub(super) fn core_only_matches_reference() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    for c in &candidates() {
        let Some((ours, core_rate)) = our_decode_core_only(&c.path, c.aac_stream) else {
            eprintln!("SKIP {}: could not core-decode", c.path.display());
            continue;
        };
        let theirs = ffmpeg_decode_at_rate(&c.path, c.ffmpeg_stream, ours.len(), core_rate);
        println!(
            "{} core-only ({} ch, {} Hz):",
            c.path.display(),
            ours.len(),
            core_rate
        );
        let cutoff = c.crossover_hz * 0.9;
        for (ch, (o, t)) in ours.iter().zip(&theirs).enumerate() {
            let ol = lowpass(o, core_rate, cutoff);
            let tl = lowpass(t, core_rate, cutoff);
            let (mut lag, mut corr, mut at_edge) =
                exhaustive_lag_correlation(&ol, &tl, PLAIN_LAG_MAX);
            if at_edge {
                (lag, corr, at_edge) = exhaustive_lag_correlation(&ol, &tl, SEARCH_LAG_MAX);
            }
            let edge = if at_edge {
                " (AT SEARCH BOUND -- noise, not a measurement)"
            } else {
                ""
            };
            println!("  ch{ch}: below {cutoff:.0}Hz lag {lag}, corr {corr:.6}{edge}");
        }
    }
}

/// Round-51 triage: dumps `kx`/`k2` (the SBR crossover/top band) for the
/// first several frames of every candidate, unconditionally (no ffmpeg
/// needed) -- a wrong `kx` would corrupt bands BELOW the crossover too, not
/// just the HF-generated ones, since `adjust()` gain-scales everything from
/// `kx` up, and `kx` computed too low would pull real core content into
/// that scaling.
#[test]
pub(super) fn dump_sideinfo_kx() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _knob = ec_aac::knobs::set_sbr_sideinfo_debug(Some(true));
    for c in &candidates() {
        let Some((asc, aus)) = extract_aac_track(&c.path, c.aac_stream) else {
            continue;
        };
        let Ok(mut decoder) = AacDecoder::with_config_bytes(&asc) else {
            continue;
        };
        let before = ec_aac::sbr_sideinfo_log().len();
        for au in aus.iter().take(50) {
            let _ = decoder.decode(au, None);
        }
        let rows = ec_aac::sbr_sideinfo_log();
        println!("{}:", c.path.display());
        for row in rows[before..].iter().filter(|r| r.ch == 0).take(6) {
            println!(
                "  frame={} source={} kx={} k2={} t_env={:?} f_high={:?}",
                row.frame, row.source, row.kx, row.k2, row.t_env, row.f_high
            );
        }
    }
}

/// Round-51 triage: FMJ's full-chain best-lag search (`LAG_MAX` = 4000)
/// finds no usable alignment (|corr| ~0.02) even though the core-only
/// decode matches almost exactly -- this widens the search far past
/// `LAG_MAX` to see whether the true alignment just lies outside the normal
/// bound (a large, rate-family-specific extra delay) rather than the SBR
/// content itself being wrong.
#[test]
pub(super) fn wide_lag_search_full_chain() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    for c in &candidates() {
        let Some((ours, sbr, rate)) = our_decode(&c.path, c.aac_stream) else {
            continue;
        };
        if sbr != ec_aac::SbrSupport::V1 {
            continue;
        }
        let theirs = ffmpeg_decode(&c.path, c.ffmpeg_stream, ours.len());
        let o = &ours[0];
        let t = &theirs[0];
        println!(
            "{} ({} Hz): ours len={} theirs len={}",
            c.path.display(),
            rate,
            o.len(),
            t.len()
        );
        const COARSE: usize = 4096;
        const WIDE_LAG_MAX: i64 = 200_000;
        let mut best = (0i64, -1.0f64);
        let mut lag = -WIDE_LAG_MAX;
        while lag <= WIDE_LAG_MAX {
            let (oa, ob) = if lag >= 0 {
                (0usize, lag as usize)
            } else {
                ((-lag) as usize, 0usize)
            };
            if oa + COARSE <= o.len() && ob + COARSE <= t.len() {
                let c = correlation(&o[oa..oa + COARSE], &t[ob..ob + COARSE]).abs();
                if c > best.1 {
                    best = (lag, c);
                }
            }
            lag += 97; // odd stride so it doesn't alias against 2048-sample AU boundaries
        }
        println!("  widest coarse best: lag={} |corr|={:.4}", best.0, best.1);
    }
}

/// Round-51 triage: compares OUR full-chain (SBR-reconstructed) output's
/// low band directly against OUR OWN core-only decode of the same stream --
/// no reference decoder involved. `v[0..kx]` in `sbr_chain.rs` is a literal
/// copy of the same core analysis QMF data the core-only path also
/// produces, so if the synthesis/gain-adjust stage is corrupting even that
/// untouched region, this shows a real self-inconsistency with no external
/// factor (alignment against a reference, different tool, etc.) to blame.
#[test]
pub(super) fn full_chain_low_band_matches_own_core() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for c in &candidates() {
        let Some((core, core_rate)) = our_decode_core_only(&c.path, c.aac_stream) else {
            continue;
        };
        let Some((full, sbr, full_rate)) = our_decode(&c.path, c.aac_stream) else {
            continue;
        };
        if sbr != ec_aac::SbrSupport::V1 {
            continue;
        }
        println!(
            "{} (core {} Hz, full {} Hz):",
            c.path.display(),
            core_rate,
            full_rate
        );
        for ch in 0..core.len().min(full.len()) {
            // Naive nearest-neighbor 2x upsample of the core so it lines up
            // sample-for-sample with the full-chain output's rate --
            // crude, but a low-pass well below the core Nyquist erases the
            // stairstep artifacts this introduces.
            let up: Vec<f32> = core[ch].iter().flat_map(|&s| [s, s]).collect();
            let cutoff = f64::from(core_rate) * 0.4;
            let ol = lowpass(&full[ch], full_rate, cutoff);
            let ul = lowpass(&up, full_rate, cutoff);
            // Escalating search: narrow (`WIDE_LAG_MAX`=20_000, the file's
            // original bound) first, widen to `SEARCH_LAG_MAX` only if that
            // narrow search hit its own edge (`at_edge`). A single shared
            // wide bound was tried first (round-54) and, unconditionally
            // widening this comparison, flipped reeln from its correct
            // small-lag answer (-576, corr 0.9967) to a wrong, noise-driven
            // distant one (-912, corr -0.295) -- a genuinely low-quality
            // low-passed signal's coarse correlation surface has enough
            // stray peaks over a 100_000-wide range to beat a real but
            // modest true peak. Escalating only when the narrow bound
            // proves insufficient (FMJ's own core-vs-full delay lands AT
            // the 20_000 edge, corr 0.217393 there, the same edge-noise
            // class the reference-vs-ours gate had) gets both files right
            // without exposing the well-behaved one to the wider bound's
            // noise floor.
            const WIDE_LAG_MAX: i64 = 20_000;
            // Top-K refinement, not the single coarse winner: FMJ's single
            // coarse pick landed on an in-bounds noise peak (lag 18995,
            // corr 0.10) while its true alignment scores >0.99 on refine.
            // A winner at the bound widens once, then is asserted off it.
            let (mut lag, mut corr, mut at_edge) =
                robust_lag_topk(&ol, &ul, WIDE_LAG_MAX, core_rate, 8);
            let mut bound = WIDE_LAG_MAX;
            // A sub-bar winner inside the narrow bound is the same symptom
            // (true peak outside the search), so it widens too.
            if at_edge || corr < 0.99 {
                bound = SEARCH_LAG_MAX;
                (lag, corr, at_edge) = robust_lag_topk(&ol, &ul, SEARCH_LAG_MAX, core_rate, 8);
            }
            println!("  ch{ch}: measured best lag {lag}, corr {corr:.6}");
            assert!(
                !at_edge,
                "{} ch{ch}: winning lag {lag} sits at the +/-{bound} \
                 search bound -- corr {corr:.6} is noise, not a measurement",
                c.path.display()
            );
            assert!(
                corr >= 0.99,
                "{} ch{ch}: full-chain low band vs our own core-only decode \
                 corr {corr:.6} at measured lag {lag} < 0.99 -- the \
                 synthesis/gain-adjust stage is corrupting even the untouched \
                 low-band region copied straight from core analysis",
                c.path.display()
            );
        }
    }
}

/// Round-52: a controlled fixture matrix -- unlike reeln/FMJ (one real
/// file each, whose sample rate, bitrate and content all differ at once),
/// every row here decodes the SAME synthesized source (three summed sine
/// sweeps 100 Hz-18 kHz + seeded pink noise, per-channel phase offset),
/// varying only sample rate (44100/48000) and bitrate (32k/48k/64k/96k,
/// which sweeps the SBR crossover `kx`/`k2`), so a break tied to a rate
/// FAMILY or to `kx` GEOMETRY shows as a pattern across the table instead
/// of being confounded with one file's own content. `aac_low` control rows
/// (no SBR at all) prove the harness itself -- container extraction,
/// decode, correlation -- is sound before the HE-AAC rows next to them are
/// trusted; those are the only asserted numbers here, everything else is
/// instrumentation for the ledger, not a regression gate yet.
///
/// Env-gated on `EC_AAC_HEAAC_FIXTURES` (defaults to the same
/// `<repo>/.cache/heaac-fixtures` `scripts/aac-tables/make-heaac-fixtures.sh`
/// writes to) -- skips loudly, generates nothing itself, when that
/// directory is absent.
#[test]
pub(super) fn synthetic_heaac_matrix() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::var("EC_AAC_HEAAC_FIXTURES")
        .unwrap_or_else(|_| format!("{}/../../.cache/heaac-fixtures", env!("CARGO_MANIFEST_DIR")));
    let dir = PathBuf::from(dir);
    if !require_fixture(&dir, "scripts/aac-tables/make-heaac-fixtures.sh") {
        return;
    }
    let _knob = ec_aac::knobs::set_sbr_sideinfo_debug(Some(true));
    println!(
        "{:>5} {:>6} {:>4} {:>4} {:>4}  {:>8} {:>8} {:>8} {:>6}  {:>11} {:>8}  {:>10} {:>8}  {:>9} {:>9}",
        "rate",
        "kbps",
        "prof",
        "kx",
        "k2",
        "full",
        "below",
        "above",
        "lag",
        "ours_v_core",
        "ovc_lag",
        "core_v_ref",
        "cvr_lag",
        "core_rate",
        "full_rate"
    );
    // Wide enough that the WIDE_LAG_MAX==LC's own decoder priming delay
    // (2048) plus a full SBR-chain worth of extra delay (a few thousand
    // samples at the doubled/full output rate) both land well inside it,
    // not right at the edge the way the plain (LAG_MAX=4000) search did.
    const WIDE_LAG_MAX: i64 = 20_000;
    // Floor: the matrix is 2 rates x 4 bitrates x 2 profiles. Without a count
    // the whole test is green having probed nothing, which is the same
    // false-green this file's `require_fixture` exists to kill.
    let mut probed = 0usize;
    for rate in [48_000u32, 44_100] {
        for br in ["32k", "48k", "64k", "96k"] {
            // LC first: its winning lag is the pure container/decoder
            // priming delay with no SBR chain involved, and is reused below
            // as the anchor for the HE row's own delay-candidate probe.
            let mut lc_lag: Option<i64> = None;
            for (profile, prefix) in [("LC", "lc"), ("HE", "heaac")] {
                let path = dir.join(format!("{prefix}_{rate}_{br}.m4a"));
                if !require_fixture(&path, "scripts/aac-tables/make-heaac-fixtures.sh") {
                    continue;
                }
                probed += 1;
                let before = ec_aac::sbr_sideinfo_log().len();
                let Some((ours, _sbr, our_rate)) = our_decode(&path, 0) else {
                    eprintln!("SKIP {}: could not decode", path.display());
                    continue;
                };
                let rows = ec_aac::sbr_sideinfo_log();
                let (kx, k2) = rows[before..]
                    .iter()
                    .find(|r| r.ch == 0)
                    .map(|r| (r.kx, r.k2))
                    .unwrap_or((-1, -1));
                let theirs = ffmpeg_decode(&path, 0, ours.len());
                let o = &ours[0];
                let t = &theirs[0];
                const TOPK: usize = 5;
                let (mut lag, mut full, edge) = robust_lag_topk(o, t, WIDE_LAG_MAX, our_rate, TOPK);
                assert!(
                    !edge,
                    "{}: {profile} row's refined winning lag {lag} sits at the \
                     +/-{WIDE_LAG_MAX} search's own edge",
                    path.display()
                );
                if profile == "LC" {
                    lc_lag = Some(lag);
                }
                // HE rows: a per-channel free lag search on periodic/tonal
                // content is unreliable (coupled_cpe_channel_swap_probe
                // proved this file's ref L vs ref R itself mis-searches --
                // lag=-24 corr=0.213 vs the fixed lag=0 corr=0.873 -- and
                // the same coarse search can land two channels of the SAME
                // stream on DIFFERENT, individually-wrong lags, faking a
                // channel swap that a fixed-lag check rules out). Derive
                // ONE lag per stream from the summed (mono) signal instead,
                // and reuse it for both channels -- more robust than
                // trusting a single, possibly-periodic channel's own search
                // -- via the same top-K/refine-on-first-10s robust method.
                if profile == "HE" && ours.len() >= 2 && theirs.len() >= 2 {
                    let mono_o: Vec<f32> =
                        ours[0].iter().zip(&ours[1]).map(|(a, b)| a + b).collect();
                    let mono_t: Vec<f32> = theirs[0]
                        .iter()
                        .zip(&theirs[1])
                        .map(|(a, b)| a + b)
                        .collect();
                    let (mono_lag, _, mono_edge) =
                        robust_lag_topk(&mono_o, &mono_t, WIDE_LAG_MAX, our_rate, TOPK);
                    assert!(
                        !mono_edge,
                        "{}: HE row's mono-derived refined lag {mono_lag} sits at the \
                         +/-{WIDE_LAG_MAX} search's own edge",
                        path.display()
                    );
                    lag = mono_lag;
                    full = correlation_at_lag(o, t, lag);
                }
                // Crossover in Hz from the actual parsed kx (band width
                // rate/128, same domain `sbr_chain.rs`'s own doc comment
                // gives `kx`/`k2` in) when SBR engaged; a fixed quarter-Nyquist
                // split for the LC controls, which carry no kx at all.
                let crossover_hz = if kx > 0 {
                    f64::from(our_rate) * (kx as f64) / 128.0
                } else {
                    f64::from(our_rate) * 0.25
                };
                // Same single robust `lag` the full/above bands use, not an
                // independent search: a narrowband low-passed signal's
                // correlation surface is broad and flat across nearby lags,
                // which used to let an independent search here silently
                // report a different (and unvalidated) alignment than the
                // one everything else in the row is anchored at.
                let below = correlation_at_lag(
                    &lowpass(o, our_rate, crossover_hz),
                    &lowpass(t, our_rate, crossover_hz),
                    lag,
                );
                // `above` re-slices at the FULL-band winning `lag` before
                // splitting, same convention `sbr_real_library_matches_reference`
                // uses -- round-52's first cut of this row measured `o[..n]`
                // against `t[..n]` with no lag offset at all, which is only
                // valid by accident at lag 0 and was silently wrong for
                // every other winning lag in that table.
                let (oa, ob) = if lag >= 0 {
                    (0usize, lag as usize)
                } else {
                    ((-lag) as usize, 0usize)
                };
                let n = WINDOW
                    .min(o.len().saturating_sub(oa))
                    .min(t.len().saturating_sub(ob));
                let above = if n >= 1024 {
                    correlation(
                        &highpass(&o[oa..oa + n], our_rate, crossover_hz),
                        &highpass(&t[ob..ob + n], our_rate, crossover_hz),
                    )
                } else {
                    0.0
                };
                // Round-51's self-consistency probe (our full chain's low
                // band vs our own core-only decode), run per-row so a
                // rate/kx-specific self-inconsistency shows in the same
                // table instead of needing a second pass.
                //
                // Fixed (this round): the original convention naively
                // nearest-neighbor UPSAMPLED the core 2x and compared it
                // against the full-rate signal, low-passed at
                // `core_rate*0.4` -- that stairstep-and-lowpass round trip
                // is lossy enough on its own to sink a genuinely-correct
                // pair's correlation (measured flat ~-0.12 here even on
                // rows whose `full`/`below` columns read 0.999+ against the
                // real reference). `core_only_matches_reference` already
                // proved a clean pattern for this exact comparison: stay in
                // the CORE's own native rate (DECIMATE the full/reference
                // signal down instead of upsampling the core up) and use a
                // wide lag search there. `decimate` below is a lowpass
                // (well under the core Nyquist) followed by a plain
                // every-other-sample drop -- the inverse of the old
                // `[s, s]` upsample, at the same information cost but
                // without its stairstep artifact.
                let decimate = |s: &[f32], from_rate: u32, to_rate: u32| -> Vec<f32> {
                    let cutoff = f64::from(to_rate) * 0.45;
                    lowpass(s, from_rate, cutoff)
                        .iter()
                        .step_by((from_rate / to_rate.max(1)).max(1) as usize)
                        .copied()
                        .collect()
                };
                let core_lag_max = WIDE_LAG_MAX / 2;
                let ours_v_core = if kx > 0 {
                    our_decode_core_only(&path, 0).map(|(core, core_rate)| {
                        let full_at_core_rate = decimate(o, our_rate, core_rate);
                        best_lag_correlation_wide(&full_at_core_rate, &core[0], core_lag_max)
                    })
                } else {
                    None
                };
                // Coordinator follow-up 2: splits "our core decode of THIS
                // fixture is wrong" (core_v_ref ~0) from "the SBR chain
                // scrambles a good core" (core_v_ref >= ~0.99 while `below`/
                // `above` stays ~0/negative) -- OUR core-only decode, same
                // native-core-rate convention `ours_v_core` now uses, against
                // the REFERENCE decoder's own decode resampled to that same
                // core rate (`ffmpeg_decode_at_rate`, the same helper
                // `core_only_matches_reference` already trusts) -- wide lag
                // search since this is an independent alignment from
                // `full`/`below`'s (core-only carries no SBR delay).
                let (core_v_ref, core_rate_str) = if kx > 0 {
                    match our_decode_core_only(&path, 0) {
                        Some((core, core_rate)) => {
                            let ref_at_core_rate =
                                ffmpeg_decode_at_rate(&path, 0, core.len(), core_rate);
                            (
                                Some(best_lag_correlation_wide(
                                    &core[0],
                                    &ref_at_core_rate[0],
                                    core_lag_max,
                                )),
                                core_rate.to_string(),
                            )
                        }
                        None => (None, "n/a".into()),
                    }
                } else {
                    (None, "n/a".into())
                };
                println!(
                    "{rate:>5} {br:>6} {profile:>4} {kx:>4} {k2:>4}  {full:>8.4} {below:>8.4} {above:>8.4} {lag:>6}  {:>11} {:>8}  {:>10} {:>8}  {:>9} {:>9}",
                    ours_v_core
                        .map(|(_, c)| format!("{c:.4}"))
                        .unwrap_or_else(|| "n/a".into()),
                    ours_v_core
                        .map(|(l, _)| l.to_string())
                        .unwrap_or_else(|| "n/a".into()),
                    core_v_ref
                        .map(|(_, c)| format!("{c:.4}"))
                        .unwrap_or_else(|| "n/a".into()),
                    core_v_ref
                        .map(|(l, _)| l.to_string())
                        .unwrap_or_else(|| "n/a".into()),
                    core_rate_str,
                    our_rate,
                );
                // 44.1k-family HE rows: print per-second full-band corr at
                // the row's own robust `lag` over 30s, so a content-
                // dependent HF-reconstruction dip (real, not an alignment
                // artifact -- `sbr441_family_sample_drift_probe` isolated
                // one at t=13-27s on heaac_44100_48k.m4a) is visible per
                // second instead of averaged away by the single `full`
                // column above.
                if profile == "HE" && rate == 44_100 {
                    println!("  per-second full-band corr @ lag {lag} (t, corr):");
                    let step = our_rate.max(1) as usize;
                    for sec in 0..30usize {
                        let oa0 = sec * step;
                        if oa0 + step > o.len() {
                            break;
                        }
                        let ob0 = oa0 as i64 + lag;
                        if ob0 < 0 {
                            continue;
                        }
                        let ob0 = ob0 as usize;
                        if ob0 + step > t.len() {
                            continue;
                        }
                        let c = correlation(&o[oa0..oa0 + step], &t[ob0..ob0 + step]);
                        println!("    t={sec:>2}s corr={c:.6}");
                    }
                }
                // Coordinator follow-up (4): on the two named rows, probe
                // candidate total HE-vs-reference delays built from LC's own
                // priming lag plus a guessed SBR-chain contribution, to see
                // which (if any) actually lands near 1.0 -- and whether that
                // offset is constant across the two rows.
                if profile == "HE"
                    && ((rate == 44_100 && br == "48k") || (rate == 48_000 && br == "64k"))
                {
                    let lc = lc_lag.unwrap_or(-2048);
                    println!("  ch0 delay probe (LC lag {lc}):");
                    for (label, extra) in [
                        ("2990", 2990i64),
                        ("3010", 3010),
                        ("3020", 3020),
                        ("1505x2", 1505 * 2),
                        ("962x2", 962 * 2),
                    ] {
                        let candidate = (lc - 2048) + extra;
                        println!(
                            "    offset={label:>6} -> lag={candidate:>6}  corr={:.6}",
                            correlation_at_lag(o, t, candidate)
                        );
                    }
                }
                if profile == "LC" {
                    assert!(
                        full >= 0.999,
                        "{}: LC control full-band corr {full:.6} < 0.999 -- the \
                         harness itself (extraction/decode/correlation) is broken, \
                         not the SBR chain this fixture matrix is meant to isolate",
                        path.display()
                    );
                }
            }
        }
    }
    assert!(
        probed >= 16,
        "only {probed} of 16 HE-AAC matrix fixtures were present -- rerun \
         scripts/aac-tables/make-heaac-fixtures.sh"
    );
}

pub(super) fn candidates() -> Vec<Candidate> {
    let home = std::env::var("HOME").unwrap_or_default();
    [
        (format!("{home}/Music/reeln.mp4"), 0, 0, 5_000.0),
        (
            format!(
                "{home}/Downloads/reelp/reelp.mkv"
            ),
            1,
            3,
            5_000.0,
        ),
    ]
    .into_iter()
    .map(|(p, aac_stream, ffmpeg_stream, crossover_hz)| Candidate {
        path: PathBuf::from(p),
        aac_stream,
        ffmpeg_stream,
        crossover_hz,
    })
    .filter(|c| c.path.exists())
    .collect()
}

/// The SBR chain against the reference decoder on every real HE-AAC file this
/// checkout can find. Skips loudly (never silently) when ffmpeg or the files
/// are absent. Per channel: full-band correlation must clear 0.999 and the
/// below-crossover (core-decoded) band must clear 0.9999, so a real SBR
/// regression cannot hide behind an average across bands the core never
/// touches.
#[test]
pub(super) fn sbr_real_library_matches_reference() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let list = candidates();
    if list.is_empty() {
        eprintln!("SKIP: no HE-AAC files found under ~/Music, ~/Downloads, ~/Videos");
        return;
    }
    let mut checked = 0;
    let mut worst_full = 1.0f64;
    let mut worst_low = 1.0f64;
    let mut any_at_edge: Option<String> = None;
    for c in &list {
        let Some((ours, sbr, rate)) = our_decode(&c.path, c.aac_stream) else {
            eprintln!("SKIP {}: could not decode", c.path.display());
            continue;
        };
        if sbr != ec_aac::SbrSupport::V1 {
            eprintln!(
                "SKIP {}: sbr_support() is {sbr:?}, not a plain-SBR (v1) stream this chain reconstructs",
                c.path.display()
            );
            continue;
        }
        let theirs = ffmpeg_decode(&c.path, c.ffmpeg_stream, ours.len());
        println!("{} ({} ch, {} Hz):", c.path.display(), ours.len(), rate);
        // Full cross-channel triangle at one fixed lag/window: both channels
        // of one CPE are decoded frame-locked and so share the same
        // start-of-stream offset, so ch0's own alignment lag is a valid
        // anchor for every pair here, letting all five numbers be compared
        // directly instead of each being independently (and, before the
        // |corr|-max fix above, unreliably) re-searched.
        if ours.len() >= 2 && theirs.len() >= 2 {
            let (lag0, _) = best_lag_correlation(&ours[0], &theirs[0]);
            println!(
                "  TRIANGLE @ ch0's lag {lag0}: ours0*ref0={:.6} ours0*ref1={:.6} ours1*ref0={:.6} ours1*ref1={:.6} ours0*ours1={:.6}",
                correlation_at_lag(&ours[0], &theirs[0], lag0),
                correlation_at_lag(&ours[0], &theirs[1], lag0),
                correlation_at_lag(&ours[1], &theirs[0], lag0),
                correlation_at_lag(&ours[1], &theirs[1], lag0),
                correlation_at_lag(&ours[0], &ours[1], 0),
            );
        }
        for (ch, (o_full, t_full)) in ours.iter().zip(&theirs).enumerate() {
            // (Round-22 stability check) EC_AAC_SBR_SWEEP_SEGMENT=1 measures
            // the second half of the file instead of the first, so a
            // calibration candidate can be checked on two disjoint segments
            // rather than just the file's start.
            //
            // (Round-23) The split point MUST be the same absolute sample
            // index on both sides, not each side's own `len() / 2`: `ours`
            // is capped at MAX_SAMPLES while `theirs` (the reference) is
            // decoded in full, so on a file longer than the cap `o_full` and
            // `t_full` have different total lengths and an independent
            // `len() / 2` on each picks DIFFERENT seconds of the file --
            // reeln is a 30s file capped to ours=22.7s, so the old code
            // compared ours' 11.35s-22.7s against theirs' 15.0s-30.0s, a
            // 3.6s-misaligned pair of unrelated passages that reads as a
            // catastrophic collapse (~-0.01 corr) with no decode defect
            // behind it. Splitting both sides at the shorter side's midpoint
            // keeps the two halves pointed at the same audio.
            let (o, t): (&[f32], &[f32]) =
                if std::env::var("EC_AAC_SBR_SWEEP_SEGMENT").as_deref() == Ok("1") {
                    let half = o_full.len().min(t_full.len()) / 2;
                    (&o_full[half..], &t_full[half..])
                } else {
                    (&o_full[..], &t_full[..])
                };
            let rms = |s: &[f32]| {
                (s.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>()
                    / s.len().max(1) as f64)
                    .sqrt()
            };
            eprintln!(
                "  ch{ch} debug: ours len={} rms={:.4} first10={:?}; theirs len={} rms={:.4} first10={:?}",
                o.len(),
                rms(o),
                &o[..10.min(o.len())],
                t.len(),
                rms(t),
                &t[..10.min(t.len())],
            );
            let (lag, full, full_at_edge) = best_lag_correlation_ex(o, t, SEARCH_LAG_MAX);
            if full_at_edge {
                any_at_edge.get_or_insert_with(|| {
                    format!(
                        "{} ch{ch} full-band: winning lag {lag} at +/-{SEARCH_LAG_MAX} search edge",
                        c.path.display()
                    )
                });
            }
            // Re-slice both signals at the winning lag before splitting into
            // bands, so the band correlations are aligned too.
            let (oa, ob) = if lag >= 0 {
                (0usize, lag as usize)
            } else {
                ((-lag) as usize, 0usize)
            };
            let n = WINDOW
                .min(o.len().saturating_sub(oa))
                .min(t.len().saturating_sub(ob));
            // The below-crossover band gets its OWN lag search rather than
            // reusing the full-band winning lag: a narrowband, slowly-varying
            // low-passed signal has a correlation surface that stays broad
            // and flat across many nearby lags, so a full-band search (whose
            // winner is decided mostly by the higher-energy/higher-bandwidth
            // content) can land on a lag that is a genuine local optimum for
            // the whole signal yet a bad one for the low band alone -- on
            // reeln ch1, full-band lag 65 vs 64 differ by only 0.0015 in
            // full-band correlation but by 0.88 in below-crossover
            // correlation once the low band is measured at its own winning
            // lag instead. Both bands are still re-anchored at the SAME
            // `start` offset internally (see `best_lag_correlation`), so
            // this is genuinely comparing the low band against itself, not
            // drifting to an unrelated part of the file.
            let low = {
                let ol_full = lowpass(o, rate, c.crossover_hz);
                let tl_full = lowpass(t, rate, c.crossover_hz);
                let (low_lag, low_corr, low_at_edge) =
                    best_lag_correlation_ex(&ol_full, &tl_full, SEARCH_LAG_MAX);
                if low_at_edge {
                    any_at_edge.get_or_insert_with(|| {
                        format!(
                            "{} ch{ch} below-crossover: winning lag {low_lag} at +/-{SEARCH_LAG_MAX} search edge",
                            c.path.display()
                        )
                    });
                }
                if low_lag != lag {
                    eprintln!(
                        "  ch{ch} below-crossover band's own best lag {low_lag} differs from full-band lag {lag}"
                    );
                }
                if std::env::var("EC_AAC_SBR_DRIFT").is_ok() {
                    let ol_sharp = sharp_lowpass(o, rate, c.crossover_hz);
                    let tl_sharp = sharp_lowpass(t, rate, c.crossover_hz);
                    let (_, sharp_corr) = best_lag_correlation(&ol_sharp, &tl_sharp);
                    println!(
                        "  ch{ch} below-crossover SHARP (3x cascaded) filter: {sharp_corr:.6}"
                    );
                }
                low_corr
            };
            let high = if n >= 1024 {
                let oh = highpass(&o[oa..oa + n], rate, c.crossover_hz);
                let th = highpass(&t[ob..ob + n], rate, c.crossover_hz);
                let rms = |v: &[f32]| {
                    (v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / v.len().max(1) as f64)
                        .sqrt()
                };
                println!(
                    "  ch{ch} above-crossover RMS ours/ref = {:.4}",
                    rms(&oh) / rms(&th).max(1e-12)
                );
                correlation(&oh, &th)
            } else {
                0.0
            };
            if ch == 0 && std::env::var("EC_AAC_SBR_NOISE_FRACTION_DEBUG").is_ok() {
                // (Round-17, Task 1) the noise-energy-fraction ceiling
                // hypothesis: predicted per-band correlation ceiling
                // sqrt(1 - noise_energy/(signal_energy+noise_energy)),
                // from OUR OWN transmitted (dequantized) envelope/noise
                // split -- printed once per file so it can be eyeballed
                // against per_band_lag_search's measured |corr| curve
                // below. Real QMF band width in output Hz is
                // `rate/128` (64-band synthesis over the output Nyquist).
                let band_hz = f64::from(rate) / 128.0;
                println!(
                    "  NOISE-FRACTION ceiling prediction (band_hz, f_noise, predicted_ceiling):"
                );
                for (band, signal, noise, out) in ec_aac::noise_fraction_table() {
                    if signal + noise <= 0.0 {
                        continue;
                    }
                    let f = noise / (signal + noise);
                    println!(
                        "    {:>6.0}Hz band{band:>3}: signal {signal:.3e} noise {noise:.3e} f_noise {f:.6} ceiling {:.4} realised/target {:.4}",
                        band as f64 * band_hz,
                        (1.0 - f).max(0.0).sqrt(),
                        out / (signal + noise)
                    );
                }
            }
            if std::env::var("EC_AAC_SBR_BANDS").is_ok() && n >= 4_096 {
                let spectrum = per_band_correlation(&o[oa..oa + n], &t[ob..ob + n], rate);
                println!("  ch{ch} per-QMF-band |corr| (band_hz, |corr|, band_index):");
                for (hz, corr, band) in &spectrum {
                    println!("    {hz:>6.0}Hz band{band:>3.0}: {corr:.4}");
                }
                let lags = per_band_lag_search(&o[oa..oa + n], &t[ob..ob + n], rate);
                println!("  ch{ch} per-HF-band PCM lag search (band_hz, best_lag, |corr|):");
                for (hz, lag, corr) in &lags {
                    println!("    {hz:>6.0}Hz: lag {lag:>5} corr {corr:.4}");
                }
                let phases = per_band_phase(&o[oa..oa + n], &t[ob..ob + n], rate);
                println!(
                    "  ch{ch} per-band cross-spectrum (band_hz, phase_deg, mag_ratio ours/theirs):"
                );
                for (hz, phase, ratio) in &phases {
                    println!(
                        "    {hz:>6.0}Hz: phase {:>7.2}deg ratio {ratio:.4}",
                        phase.to_degrees()
                    );
                }
            }
            if std::env::var("EC_AAC_SBR_HF_CONVICTION").is_ok() && n >= 4_096 {
                let band_hz = f64::from(rate) / 256.0;
                let kx = (c.crossover_hz / band_hz).round() as usize;
                let win = &o[oa..oa + n];
                let twin = &t[ob..ob + n];
                println!(
                    "  ch{ch} TASK1 bandpass-selectivity probe (band_hz, same-band corr, +offset corr, -offset corr):"
                );
                for hf_band in [kx + 5, kx + 15, kx + 25] {
                    let (same, hi_c, lo_c) = bandpass_offset_probe(win, twin, rate, hf_band, 10);
                    println!(
                        "    HF band{hf_band:>3} ({:.0}Hz) offset=10: same {same:.4} +10band {hi_c:.4} -10band {lo_c:.4}",
                        hf_band as f64 * band_hz
                    );
                }
                if kx > 6 {
                    let lf_band = kx.saturating_sub(6);
                    let (same, hi_c, lo_c) = bandpass_offset_probe(win, twin, rate, lf_band, 3);
                    println!(
                        "    LF CONTROL band{lf_band:>3} ({:.0}Hz) offset=3: same {same:.4} +3band {hi_c:.4} -3band {lo_c:.4}",
                        lf_band as f64 * band_hz
                    );
                }
                println!(
                    "  ch{ch} TASK2 bin-level conviction (band_hz, ours-energy, direct, mirror, shift+1band, shift-1band):"
                );
                let min_band = (2_500.0 / band_hz).ceil() as usize;
                for (hz, energy, direct, mirror, shift_up, shift_down) in
                    bin_level_conviction(win, twin, rate, min_band, 6)
                {
                    println!(
                        "    {hz:>6.0}Hz: energy {energy:.3e} direct {direct:.4} mirror {mirror:.4} shift+1 {shift_up:.4} shift-1 {shift_down:.4}"
                    );
                }
            }
            // (Round-36, Task 2) parity-split coherence: round-35 fixed the
            // Synthesis phase-intercept bug only for ODD (target-source)
            // patch gaps, at the patch-copy site. On header A (the real
            // file's own header) that fix can only be SEEN in the odd-gap
            // territory those patches cover (bands 28..42, from patches
            // (3,28,11)/(0,39,3)/(13,42,1)); the even-gap patch (0,14,14)
            // covers bands 14..27 and was already exact before round-35, so
            // it is the control range the fix must NOT move. Reuses
            // `bin_level_conviction`'s per-bin direct-coherence measurement
            // over every HF band in [14, 43) (min_band=14, top_n wide enough
            // to keep every band, not just the top-energy handful), then
            // averages `direct` within each region separately.
            if std::env::var("EC_AAC_SBR_PARITY_SPLIT").is_ok() && n >= 4_096 {
                let band_hz = f64::from(rate) / 256.0;
                let win = &o[oa..oa + n];
                let twin = &t[ob..ob + n];
                let rows = bin_level_conviction(win, twin, rate, 14, 29);
                let mean_in = |lo: usize, hi: usize| {
                    let vals: Vec<f64> = rows
                        .iter()
                        .filter(|(hz, ..)| {
                            let band = (hz / band_hz).round() as usize;
                            band >= lo && band < hi
                        })
                        .map(|(_, _, direct, ..)| *direct)
                        .collect();
                    if vals.is_empty() {
                        (0.0, 0)
                    } else {
                        (vals.iter().sum::<f64>() / vals.len() as f64, vals.len())
                    }
                };
                let (even_mean, even_n) = mean_in(14, 28);
                let (odd_mean, odd_n) = mean_in(28, 43);
                println!(
                    "  ch{ch} PARITY-SPLIT direct-coherence mean: even-gap[14,28)={even_mean:.4} (n={even_n}) odd-gap[28,43)={odd_mean:.4} (n={odd_n})"
                );
            }
            // (Round-38, Task 1) HF-only constant-lag sweep: see
            // `hf_lag_sweep`'s doc comment. Reuses this window's already
            // globally-aligned `oa`/`ob`/`n`; only `o`'s STFT read position is
            // shifted, `n_slot`*64 samples at a time.
            if std::env::var("EC_AAC_SBR_HF_LAG_SWEEP").is_ok() && n >= 4_096 {
                let band_hz = f64::from(rate) / 256.0;
                let min_band = (2_500.0 / band_hz).ceil() as usize;
                let sweep = hf_lag_sweep(o, t, oa, ob, n, min_band);
                println!(
                    "  ch{ch} TASK1 HF-only lag sweep (k = QMF slots of 64 samples, mean direct HF-bin coherence):"
                );
                for (k, coh) in &sweep {
                    println!("    k={k:>3}: coherence {coh:.4}");
                }
                if let Some((peak_k, peak_c)) =
                    sweep.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                {
                    println!("  ch{ch} HF lag sweep peak: k={peak_k} coherence {peak_c:.4}");
                }
            }
            // (Round-40, Task 1) fine spectral-bin shift scan -- see
            // `hf_bin_shift_sweep`'s doc comment.
            if std::env::var("EC_AAC_SBR_BIN_SHIFT").is_ok() && n >= 4_096 {
                let win = &o[oa..oa + n];
                let twin = &t[ob..ob + n];
                for (region_name, lo, hi) in
                    [("even-gap[14,28)", 14, 28), ("odd-gap[28,43)", 28, 43)]
                {
                    let sweep = hf_bin_shift_sweep(win, twin, rate, lo, hi);
                    println!(
                        "  ch{ch} TASK1 BIN-SHIFT {region_name} (b = FFT bins, mean direct HF-bin coherence):"
                    );
                    for (b, coh) in &sweep {
                        println!("    b={b:>3}: coherence {coh:.4}");
                    }
                    if let Some((peak_b, peak_c)) =
                        sweep.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                    {
                        println!(
                            "  ch{ch} {region_name} bin-shift peak: b={peak_b} coherence {peak_c:.4}"
                        );
                    }
                }
            }
            // (Round-40, Task 2) complex-gain fingerprint -- see
            // `hf_complex_ratio_trajectory`'s doc comment.
            if std::env::var("EC_AAC_SBR_COMPLEX_GAIN").is_ok() && n >= 4_096 {
                let band_hz = f64::from(rate) / 256.0;
                let min_band = (2_500.0 / band_hz).ceil() as usize;
                let win = &o[oa..oa + n];
                let twin = &t[ob..ob + n];
                for (hz, traj) in hf_complex_ratio_trajectory(win, twin, rate, min_band, 4) {
                    println!(
                        "  ch{ch} TASK2 COMPLEX-GAIN {hz:.0}Hz dominant-bin O/T trajectory (frame: mag, phase_deg, dphase_deg):"
                    );
                    let mut prev_phase: Option<f64> = None;
                    for (h, (mag, phase)) in traj.iter().enumerate() {
                        let dphase = prev_phase.map(|p| {
                            let mut d = phase.to_degrees() - p;
                            while d > 180.0 {
                                d -= 360.0;
                            }
                            while d < -180.0 {
                                d += 360.0;
                            }
                            d
                        });
                        prev_phase = Some(phase.to_degrees());
                        match dphase {
                            Some(d) => println!(
                                "    frame{h:>3}: mag {mag:.4} phase {:>7.2}deg dphase {d:>7.2}deg",
                                phase.to_degrees()
                            ),
                            None => println!(
                                "    frame{h:>3}: mag {mag:.4} phase {:>7.2}deg dphase    n/a",
                                phase.to_degrees()
                            ),
                        }
                    }
                }
            }
            // (Round-45, Task 3) THIRD WITNESS: a reference-free plain
            // patch+gain simulator built from the reference's own low band --
            // see `hf_patch_simulator`'s doc comment.
            if std::env::var("EC_AAC_SBR_SIMULATOR").is_ok() && n >= 4_096 {
                let win = &o[oa..oa + n];
                let twin = &t[ob..ob + n];
                let (vs_ref, vs_ours) = hf_patch_simulator(win, twin, rate);
                let mean_in = |rows: &[(usize, f64)], lo: usize, hi: usize| {
                    let vals: Vec<f64> = rows
                        .iter()
                        .filter(|(b, _)| *b >= lo && *b < hi)
                        .map(|(_, c)| *c)
                        .collect();
                    if vals.is_empty() {
                        0.0
                    } else {
                        vals.iter().sum::<f64>() / vals.len() as f64
                    }
                };
                println!("  ch{ch} SIMULATOR sim-vs-REFERENCE per-band coherence (band, coh):");
                for (b, coh) in &vs_ref {
                    println!("    band{b:>3}: {coh:.4}");
                }
                println!(
                    "  ch{ch} SIMULATOR sim-vs-REFERENCE mean: even-gap[14,28)={:.4} odd-gap[28,43)={:.4}",
                    mean_in(&vs_ref, 14, 28),
                    mean_in(&vs_ref, 28, 43)
                );
                println!("  ch{ch} SIMULATOR sim-vs-OURS per-band coherence (band, coh):");
                for (b, coh) in &vs_ours {
                    println!("    band{b:>3}: {coh:.4}");
                }
                println!(
                    "  ch{ch} SIMULATOR sim-vs-OURS mean: even-gap[14,28)={:.4} odd-gap[28,43)={:.4}",
                    mean_in(&vs_ours, 14, 28),
                    mean_in(&vs_ours, 28, 43)
                );
            }
            // (Round-46, Task 1) QMF-domain exact third witness -- see
            // `hf_patch_simulator_qmf`'s doc comment.
            if std::env::var("EC_AAC_SBR_QMF_WITNESS").is_ok() && n >= 4_096 {
                let win = &o[oa..oa + n];
                let twin = &t[ob..ob + n];
                let (vs_ref, vs_ours, ours_vs_ref) = hf_patch_simulator_qmf(win, twin);
                // Per-QMF-band energy ratio and coherence, ours vs reference
                // (the envelope-adjuster instrument: a level offset shows as
                // a constant dB ratio, a gain-shape defect as per-band dB).
                {
                    let (o_spec, t_spec, _) = qmf_domain_spec(win, twin);
                    println!("  ch{ch} QMF-BAND ours-vs-REFERENCE (band, ratio_dB, coherence):");
                    for band in 0..o_spec.len().min(t_spec.len()).min(48) {
                        let (mut eo, mut et, mut cr, mut ci) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                        for (a, b) in o_spec[band].iter().zip(&t_spec[band]) {
                            eo += a.norm_sqr();
                            et += b.norm_sqr();
                            let c = *a * b.conj();
                            cr += c.re;
                            ci += c.im;
                        }
                        let coh = (cr * cr + ci * ci).sqrt() / (eo * et).sqrt().max(1e-30);
                        println!(
                            "    band{band:>3}: {:>7.2} dB  coh {coh:.4}",
                            10.0 * (eo / et.max(1e-30)).log10()
                        );
                    }
                }
                let mean_in = |rows: &[(usize, f64)], lo: usize, hi: usize| {
                    let vals: Vec<f64> = rows
                        .iter()
                        .filter(|(b, _)| *b >= lo && *b < hi)
                        .map(|(_, c)| *c)
                        .collect();
                    if vals.is_empty() {
                        0.0
                    } else {
                        vals.iter().sum::<f64>() / vals.len() as f64
                    }
                };
                println!("  ch{ch} QMF-WITNESS sim-vs-REFERENCE per-band coherence (band, coh):");
                for (b, coh) in &vs_ref {
                    println!("    band{b:>3}: {coh:.4}");
                }
                println!(
                    "  ch{ch} QMF-WITNESS sim-vs-REFERENCE mean: even-gap[14,28)={:.4} odd-gap[28,43)={:.4}",
                    mean_in(&vs_ref, 14, 28),
                    mean_in(&vs_ref, 28, 43)
                );
                println!("  ch{ch} QMF-WITNESS sim-vs-OURS per-band coherence (band, coh):");
                for (b, coh) in &vs_ours {
                    println!("    band{b:>3}: {coh:.4}");
                }
                println!(
                    "  ch{ch} QMF-WITNESS sim-vs-OURS mean: even-gap[14,28)={:.4} odd-gap[28,43)={:.4}",
                    mean_in(&vs_ours, 14, 28),
                    mean_in(&vs_ours, 28, 43)
                );
                // (Round-48, Task 4a) the ultimate content-match witness:
                // our HF QMF content directly against the reference's, no
                // simulator step in between.
                println!("  ch{ch} QMF-WITNESS ours-vs-REFERENCE per-band coherence (band, coh):");
                for (b, coh) in &ours_vs_ref {
                    println!("    band{b:>3}: {coh:.4}");
                }
                println!(
                    "  ch{ch} QMF-WITNESS ours-vs-REFERENCE mean: even-gap[14,28)={:.4} odd-gap[28,43)={:.4}",
                    mean_in(&ours_vs_ref, 14, 28),
                    mean_in(&ours_vs_ref, 28, 43)
                );
                // (Round-46, Task 2) fitted sim-to-reference transfer, one
                // representative band per patch plus a spread across the
                // strong odd-gap patch (3,28,11) where the QMF witness shows
                // sim-vs-reference clearing 0.5.
                let fit_bands = [14, 18, 22, 26, 28, 30, 32, 34, 36, 38, 39, 42];
                let fits_ref = hf_patch_transfer_fit(win, twin, &fit_bands, false);
                println!("  ch{ch} TASK2 FIT ref[l]~=a0*sim[l]+a1*sim[l-1]+a2*sim[l-2]:");
                for (b, a) in &fits_ref {
                    println!(
                        "    band{b:>3}: a0=({:>7.3},{:>7.3}) a1=({:>7.3},{:>7.3}) a2=({:>7.3},{:>7.3})",
                        a[0].re, a[0].im, a[1].re, a[1].im, a[2].re, a[2].im
                    );
                }
                // (Round-48, Task 2) same fit, target = ours: side by side
                // with the above shows exactly what our transform does to
                // the plain copy vs what the reference's does.
                let fits_ours = hf_patch_transfer_fit(win, twin, &fit_bands, true);
                println!("  ch{ch} TASK2 FIT ours[l]~=a0*sim[l]+a1*sim[l-1]+a2*sim[l-2]:");
                for (b, a) in &fits_ours {
                    println!(
                        "    band{b:>3}: a0=({:>7.3},{:>7.3}) a1=({:>7.3},{:>7.3}) a2=({:>7.3},{:>7.3})",
                        a[0].re, a[0].im, a[1].re, a[1].im, a[2].re, a[2].im
                    );
                }
            }
            println!(
                "  ch{ch}: lag {lag}, full {full:.6}, below {:.0}Hz {low:.6}, above {:.0}Hz {high:.6}",
                c.crossover_hz, c.crossover_hz
            );
            if std::env::var("EC_AAC_SBR_DRIFT").is_ok() && n >= 4_096 {
                let windows = (n / 4_096).min(20);
                let drift = windowed_lag_drift(o, t, oa, ob, windows);
                println!("  ch{ch} DRIFT (window_idx, local_lag, |corr|):");
                for (w, (l, cr)) in drift.iter().enumerate() {
                    println!("    w{w}: lag {l} corr {cr:.4}");
                }
            }
            // (Round-23, Task 1/3) whole-file corr-vs-time timeline: the
            // DRIFT diagnostic above caps at 20 windows (~1.85s) starting at
            // the full-band lag offset near sample 0, so it never sees a
            // collapse that only appears later in the file. This walks
            // every 4096-sample window from that same start to EOF on BOTH
            // sides, with its own local lag search per window, so a
            // mid-file onset shows up as a specific window index/timestamp
            // instead of being averaged away inside one giant WINDOW-sized
            // correlation. The mean/min across every window is also the
            // TRUE whole-file correlation number (`best_lag_correlation`
            // above only ever measures one WINDOW=200_000-sample slice
            // starting near the file's first quarter): unlike that one
            // fixed-lag figure, this one is immune to a single global lag
            // being slightly wrong for a later part of the file.
            {
                let avail = o.len().saturating_sub(oa).min(t.len().saturating_sub(ob));
                let windows = avail / 4_096;
                // The full per-window LOCAL_LAG=+-2000 search inside
                // `windowed_lag_drift` costs O(windows * 4001 * 4096) and runs
                // ~4min/channel over a whole real-library file -- that walk
                // is only needed when actually hunting for a drifting lag
                // (EC_AAC_SBR_TIMELINE). By default this block instead reuses
                // the already-established global `lag` with NO per-window
                // search (O(windows * 4096)), which is cheap and still gives
                // an honest whole-file windowed mean/min at that fixed lag.
                let timeline: Vec<(i64, f64)> = if std::env::var("EC_AAC_SBR_TIMELINE").is_ok() {
                    windowed_lag_drift(o, t, oa, ob, windows)
                } else {
                    (0..windows)
                        .filter_map(|w| {
                            // `oa`/`ob` (computed above from `lag`) already encode the
                            // alignment shift via their differing base offsets -- do NOT
                            // add `lag` again here, that double-applies it and shifts
                            // every window an extra `lag` samples out of alignment
                            // (this collapsed the whole-file mean from ~0.96 to ~0.14
                            // while leaving the single-WINDOW `full`/`low` numbers,
                            // which don't go through this path, unaffected).
                            let base_o = oa + w * 4_096;
                            let base_t = ob + w * 4_096;
                            if base_o + 4_096 > o.len() || base_t + 4_096 > t.len() {
                                return None;
                            }
                            Some((
                                lag,
                                correlation(&o[base_o..base_o + 4_096], &t[base_t..base_t + 4_096]),
                            ))
                        })
                        .collect()
                };
                if std::env::var("EC_AAC_SBR_TIMELINE").is_ok() {
                    println!(
                        "  ch{ch} TIMELINE ({windows} windows, {rate} Hz, window_idx@sec, local_lag, corr):"
                    );
                    for (w, (l, cr)) in timeline.iter().enumerate() {
                        let t_sec = (oa + w * 4_096) as f64 / f64::from(rate);
                        println!("    w{w}@{t_sec:.2}s: lag {l} corr {cr:.4}");
                    }
                }
                if !timeline.is_empty() {
                    let mean = timeline.iter().map(|(_, c)| c).sum::<f64>() / timeline.len() as f64;
                    let min = timeline.iter().map(|(_, c)| *c).fold(f64::MAX, f64::min);
                    println!(
                        "  ch{ch} WHOLE-FILE windowed corr: mean {mean:.6} min {min:.6} over {} windows ({:.2}s)",
                        timeline.len(),
                        windows as f64 * 4_096.0 / f64::from(rate)
                    );
                }
            }
            if std::env::var("EC_AAC_SBR_RESIDUAL_DEBUG").is_ok() {
                residual_sideinfo_analysis(o, t, oa, ob, rate, ch);
            }
            worst_full = worst_full.min(full);
            worst_low = worst_low.min(low);
        }
        checked += 1;
    }
    if checked == 0 {
        eprintln!("SKIP: no plain-SBR real files decoded");
        return;
    }
    assert!(
        any_at_edge.is_none(),
        "lag search hit its bound: {} -- the reported correlations are noise \
         at the search edge, not a measurement",
        any_at_edge.unwrap_or_default()
    );
    assert!(
        worst_full >= 0.999,
        "worst full-band correlation {worst_full:.6} < 0.999 bar"
    );
    assert!(
        worst_low >= 0.9999,
        "worst below-crossover correlation {worst_low:.6} < 0.9999 -- the core decode itself regressed"
    );
}

/// Writes `aus` (raw access units, no ADTS framing) plus `asc` (their own
/// AudioSpecificConfig bytes, unmodified) into a fresh mp4 at `out_path`, so
/// the reference decoder can be pointed at container-native SBR content that
/// is byte-identical to the original stream's access units -- the same
/// `esds`-carries-an-explicit-SBR-config shape `examples/wrap_sbr.rs` proved
/// on synthetic content in round-31, reused here on a REAL file's own AUs as
/// the round-41 Task 1 remux control.
pub(super) fn write_remux(out_path: &Path, asc: &[u8], aus: &[Vec<u8>]) {
    let cfg = ec_aac::parse_audio_specific_config(asc).expect("real file's own ASC parses");
    let core_rate = cfg.sample_rate;
    let time_base = ec_core::TimeBase::from_rate(core_rate);
    let layout = if cfg.channels == 1 {
        ec_core::ChannelLayout::Mono
    } else {
        ec_core::ChannelLayout::Stereo
    };
    let mut params = ec_core::CodecParameters::new(CodecId::Aac);
    params.extradata = Some(asc.to_vec().into());
    params.media = ec_core::MediaParameters::Audio(ec_core::AudioParameters {
        sample_rate: core_rate,
        layout,
        format: None,
        bits_per_sample: None,
    });
    let mut info = ec_core::StreamInfo::new(0, time_base, params);
    info.default = true;
    let out = File::create(out_path).expect("remux output creatable");
    let mut muxer = ec_mp4::Mp4Muxer::new(out).expect("mp4 muxer opens");
    use ec_core::Muxer as _;
    muxer.add_stream(info).expect("stream declared");
    for (i, au) in aus.iter().enumerate() {
        let pts = i as i64 * 1024;
        let packet = Packet::new(0, time_base, au.as_slice())
            .with_pts(pts)
            .with_duration(1024);
        muxer.write_packet(&packet).expect("packet written");
    }
    muxer.finish().expect("finished");
}

/// Mean/min of `bin_level_conviction`'s `direct` column, printed as one
/// summary line per control pair.
pub(super) fn report_conviction(label: &str, rows: &[(f64, f64, f64, f64, f64, f64)]) {
    if rows.is_empty() {
        println!("  {label}: no HF bands measured (window too short)");
        return;
    }
    let vals: Vec<f64> = rows.iter().map(|(_, _, direct, ..)| *direct).collect();
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    let min = vals.iter().cloned().fold(f64::MAX, f64::min);
    println!(
        "  {label}: direct-coherence mean {mean:.4} min {min:.4} over {} HF bands",
        vals.len()
    );
    for (hz, _, direct, ..) in rows {
        println!("    {hz:>6.0}Hz: direct {direct:.4}");
    }
}
