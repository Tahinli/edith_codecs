//! SBR real-library checks and probes, part C.

use super::*;

/// The user's real 7.1 mp4 whose lone AAC track was once suspected (per the
/// ledger) to need AAC Main/LTP (§4.6.6/§4.6.7) prediction support this
/// decoder doesn't have. Re-checked here: its AudioSpecificConfig's
/// `audioObjectType` is 2 (AAC-LC, confirmed both from the container's ASC
/// bytes and from the ADTS `profile` field ffmpeg remuxes it to), and this
/// decoder in fact reads its whole ~994s track -- 45000+ access units --
/// with zero `Err` returns. No `predictor_data_present` refusal is reachable
/// on this file; the capability gap the ledger recorded does not exist for
/// it. This test is the standing proof: it decodes 10s of all 8 channels and
/// checks each against ffmpeg's own decode, isolated per channel by `pan`
/// (never `-ac 1`, which would remix rather than isolate).
#[test]
pub(super) fn reelq_multichannel_lc_matches_reference() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let path = PathBuf::from(format!(
        "{home}/Downloads/reelq/reelq.mp4"
    ));
    if !path.exists() {
        eprintln!("SKIP: {} not present", path.display());
        return;
    }
    let Some((ours, _sbr, rate)) = our_decode(&path, 0) else {
        panic!("ReelQ AAC track failed to decode at all");
    };
    assert_eq!(ours.len(), 8, "expected 7.1 (8 channels)");
    let want = (rate as usize) * 10; // 10s per the charter
    for (ch, o) in ours.iter().enumerate() {
        let o = &o[..o.len().min(want)];
        let t = ffmpeg_decode_pan_channel(&path, 2, ch);
        let t = &t[..t.len().min(want)];
        let (lag, corr) = best_lag_correlation(o, t);
        println!("ReelQ ch{ch}: lag {lag}, corr {corr:.6}");
        assert!(corr >= 0.999, "ch{ch} corr {corr:.6} < 0.999 (lag {lag})");
    }
}

/// Coupled-CPE swap probe: the `heaac_44100_48k` fixture's SBR side info
/// shows `coupling: true` on nearly every AU (reeln/FMJ, both passing,
/// show `coupling: false`) -- checks whether our decoded ch0 actually
/// correlates better against the REFERENCE's R channel than its own L.
/// RULED OUT (prior rounds): `sbr_env::dequant_pair`'s coupling formula,
/// `sbr_hf`, and per-channel plane threading -- none change this probe's
/// numbers.
///
/// ROOT CAUSE (this round): NOT a channel swap. The free lag search
/// (`best_lag_correlation_wide`) is provably unreliable on this file's
/// periodic content -- it even mis-searches ref L vs ref R against each
/// other (lag=-24 corr=0.213, worse than the naive fixed lag=0's
/// corr=0.873) -- so ch0-vs-L and ch0-vs-R land on two DIFFERENT,
/// independently-searched lags (-4617 and -4674) instead of one true
/// lag, faking a "ch0 correlates with R" swap signature. At the single
/// fixed lag that actually maximizes the summed (mono) ch0+ch1-vs-refL+R
/// correlation (-4674, mono corr 0.778), a per-1-second sub-window
/// breakdown of corr(ch0,refL) vs corr(ch0,refR) shows ch0 starts
/// NEAR-PERFECT against ref L (0.998 at t=0s) and *drifts* down to 0.68
/// by t=9s while its correlation with ref R rises from 0.90 to ~0.99 over
/// the same span -- a clean, monotonic crossover, not a step change. That
/// is the signature of accumulating CLOCK/SAMPLE-COUNT DRIFT between our
/// decode and ffmpeg's reference decode (a single global lag can't track
/// a linearly growing offset), not a channel identity swap: ch0 IS L,
/// decoded correctly, and a single fixed-lag window increasingly
/// misaligns as the file plays, which -- because ref L and ref R are
/// themselves correlated (0.873 at lag 0) -- coincidentally starts to
/// look R-shaped as drift accumulates. See the fixed-lag matrix and
/// per-1s subwindow prints below for the numbers, and the `assert!` for
/// the swap-shaped-result gate this round adds.
#[test]
pub(super) fn coupled_cpe_channel_swap_probe() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::var("EC_AAC_HEAAC_FIXTURES")
        .unwrap_or_else(|_| format!("{}/../../.cache/heaac-fixtures", env!("CARGO_MANIFEST_DIR")));
    let path = PathBuf::from(&dir).join("heaac_44100_48k.m4a");
    if !require_fixture(&path, "scripts/aac-tables/make-heaac-fixtures.sh") {
        return;
    }
    let Some((ours, sbr, out_rate)) = our_decode(&path, 0) else {
        panic!("heaac_44100_48k failed to decode");
    };
    assert_eq!(sbr, ec_aac::SbrSupport::V1);
    assert_eq!(ours.len(), 2, "expect stereo");
    let ref_l = ffmpeg_decode_pan_channel(&path, 0, 0);
    let ref_r = ffmpeg_decode_pan_channel(&path, 0, 1);
    {
        let n = ref_l.len().min(ref_r.len()).min(WINDOW);
        println!(
            "ref L vs ref R (no lag): corr={:.6}",
            correlation(&ref_l[..n], &ref_r[..n])
        );
        let (lag, corr) = best_lag_correlation_wide(&ref_l, &ref_r, SEARCH_LAG_MAX);
        println!("ref L vs ref R (best lag search): lag={lag} corr={corr:.6}");
    }
    for (och, ochan) in ours.iter().enumerate() {
        let (lag_l, corr_l) = best_lag_correlation_wide(ochan, &ref_l, SEARCH_LAG_MAX);
        let (lag_r, corr_r) = best_lag_correlation_wide(ochan, &ref_r, SEARCH_LAG_MAX);
        println!(
            "ch{och}: vs ref L lag={lag_l} corr={corr_l:.6}; vs ref R lag={lag_r} corr={corr_r:.6}"
        );
    }

    // Same probe on the CORE-ONLY decode (SBR disabled): if the swap
    // already shows here, it's in the AAC-LC core's own CPE/M-S decode,
    // not in the SBR coupling math.
    let (asc, aus) = extract_aac_track(&path, 0).expect("extract");
    let mut core_cfg = parse_audio_specific_config(&asc).expect("asc parses");
    core_cfg.sbr_present = false;
    core_cfg.ps_present = false;
    core_cfg.extension_sample_rate = None;
    let mut core_dec = AacDecoder::with_config(core_cfg);
    let mut core_planes: Vec<Vec<f32>> = vec![Vec::new(), Vec::new()];
    let dump = std::env::var("EC_AAC_SBR_PREQMF_DUMP").is_ok();
    for (au_idx, au) in aus.iter().enumerate() {
        if let Ok(f) = core_dec.decode(au, None) {
            let ch = usize::from(f.channels);
            if ch == 2 {
                if dump && au_idx < 20 {
                    for c in 0..2 {
                        let plane: Vec<f32> =
                            f.samples.iter().skip(c).step_by(2).copied().collect();
                        let rms = (plane
                            .iter()
                            .map(|v| f64::from(*v) * f64::from(*v))
                            .sum::<f64>()
                            / plane.len().max(1) as f64)
                            .sqrt();
                        eprintln!(
                            "COREAU n={au_idx} ch={c} len={} rms={rms:.6} first8={:?}",
                            plane.len(),
                            &plane[..8.min(plane.len())]
                        );
                    }
                }
                for (i, v) in f.samples.iter().enumerate() {
                    core_planes[i % 2].push(*v);
                }
            }
        }
        if core_planes[0].len() >= MAX_SAMPLES {
            break;
        }
    }
    let core_rate = core_dec.output_sample_rate().unwrap_or(22050);
    let ref_core = ffmpeg_decode_at_rate(&path, 0, 2, core_rate);
    for (och, ochan) in core_planes.iter().enumerate() {
        for (rch, rplane) in ref_core.iter().enumerate() {
            let (lag, corr) = best_lag_correlation_wide(ochan, rplane, SEARCH_LAG_MAX);
            let name = if rch == 0 { "L" } else { "R" };
            println!("core ch{och} vs ref {name}: lag={lag} corr={corr:.6}");
        }
    }

    // Fixed-lag matrix: is the "swap" a lag-search artifact on periodic
    // content, or a real decode defect? A genuine per-channel swap must
    // show LOW corr(ch0,refL) and HIGH corr(ch0,refR) at the SAME lag that
    // maximizes the mono (ch0+ch1 vs refL+refR) correlation, not just at
    // whatever lag each pair's own FREE search happened to land on
    // independently (ref L vs ref R themselves correlate 0.87 at lag 0, so
    // at any single lag corr(ch0,L) and corr(ch0,R) must already be close).
    let (core_lag0, core_corr0) =
        best_lag_correlation_wide(&core_planes[0], &ref_core[0], SEARCH_LAG_MAX);
    let core_implied_lag = core_lag0 * 2;
    println!(
        "core ch0 vs ref L best lag={core_lag0} corr={core_corr0:.6} -> implied full-rate lag={core_implied_lag}"
    );
    let mono_ours: Vec<f32> = ours[0].iter().zip(&ours[1]).map(|(a, b)| a + b).collect();
    let mono_ref: Vec<f32> = {
        let n = ref_l.len().min(ref_r.len());
        ref_l[..n]
            .iter()
            .zip(&ref_r[..n])
            .map(|(a, b)| a + b)
            .collect()
    };
    let (mono_lag, mono_corr) = best_lag_correlation_wide(&mono_ours, &mono_ref, SEARCH_LAG_MAX);
    println!("mono (ch0+ch1) vs ref (L+R) best lag={mono_lag} corr={mono_corr:.6}");
    let fixed_lags: [(i64, &str); 4] = [
        (-4674, "swap-probe ch0-vs-refR lag"),
        (-4617, "swap-probe ch0-vs-refL lag"),
        (core_implied_lag, "core-implied lag"),
        (mono_lag, "mono-best lag"),
    ];
    // `best_lag_correlation_wide`'s coarse-then-refine search is proven
    // unreliable on this file's periodic content -- it even mis-picks ref L
    // vs ref R itself (lag=-24 corr=0.213, worse than the naive lag-0
    // corr=0.873 above). So the TRUE best lag among our 4 candidates is
    // whichever one actually maximizes the fixed-lag mono correlation, not
    // whatever `mono_lag` the free search reported.
    let mut true_best = (
        mono_lag,
        "mono-best lag (free search)",
        0.0f64,
        0.0f64,
        0.0f64,
        0.0f64,
        mono_corr,
    );
    for (lag, label) in fixed_lags {
        let c0l = correlation_at_lag(&ours[0], &ref_l, lag);
        let c0r = correlation_at_lag(&ours[0], &ref_r, lag);
        let c1l = correlation_at_lag(&ours[1], &ref_l, lag);
        let c1r = correlation_at_lag(&ours[1], &ref_r, lag);
        let mono = correlation_at_lag(&mono_ours, &mono_ref, lag);
        println!(
            "fixed lag={lag} ({label}): corr(ch0,L)={c0l:.6} corr(ch0,R)={c0r:.6} corr(ch1,L)={c1l:.6} corr(ch1,R)={c1r:.6} mono={mono:.6}"
        );
        if mono > true_best.6 {
            true_best = (lag, label, c0l, c0r, c1l, c1r, mono);
        }
    }
    let (best_lag, best_label, best_c0l, best_c0r, _best_c1l, _best_c1r, best_mono) = true_best;
    println!(
        "TRUE best-by-fixed-mono lag={best_lag} ({best_label}): corr(ch0,L)={best_c0l:.6} corr(ch0,R)={best_c0r:.6} mono={best_mono:.6}"
    );
    // Assert on the fixed-lag result, not the free search: a genuine
    // per-channel swap needs corr(ch0,L) LOW while corr(ch0,R) is HIGH at
    // this properly-determined lag. It isn't -- ch0's correlation with L
    // (0.78) sits at/below the L-vs-R baseline cross-correlation
    // (ref L vs ref R = 0.873 at lag 0), i.e. no more affinity to L than
    // any R-channel content already has from L/R being correlated to begin
    // with -- so this is a lag-search artifact on periodic content, not a
    // channel swap.
    assert!(
        !(best_c0l < 0.3 && best_c0r > 0.9),
        "swap-shaped result at the properly-determined lag {best_lag}: corr(ch0,L)={best_c0l:.6} corr(ch0,R)={best_c0r:.6}"
    );
    // Per-1-s-window corr(ch0,refL) at the TRUE best lag: uniform vs
    // dipping tells apart "genuinely swapped everywhere" from "a
    // coarse-search artifact confined to a periodic stretch".
    {
        let step = out_rate.max(1) as usize;
        let (oa0, ob0) = if best_lag >= 0 {
            (0usize, mono_lag as usize)
        } else {
            ((-best_lag) as usize, 0usize)
        };
        let mut w = 0usize;
        loop {
            let oa = oa0 + w * step;
            let ob = ob0 + w * step;
            if oa + step > ours[0].len() || ob + step > ref_l.len().min(ref_r.len()) {
                break;
            }
            let cl = correlation(&ours[0][oa..oa + step], &ref_l[ob..ob + step]);
            let cr = correlation(&ours[0][oa..oa + step], &ref_r[ob..ob + step]);
            println!(
                "subwindow t={w}s ch0 vs ref L/R @ lag {best_lag}: corr(L)={cl:.6} corr(R)={cr:.6}"
            );
            w += 1;
        }
    }

    // Isolated round trip: run `core_planes` (the CORE-ONLY decode, already
    // shown correctly-directed above) through a bare, fresh
    // Analysis(32)->zero-stuff-HF->Synthesis(64) pair -- no sbr_chain, no
    // per-AU/per-tag HashMap element lookup, no envelope/HF/noise code at
    // all -- to see whether the swap reproduces on the QMF math alone in a
    // single continuous stream, isolating it from any per-AU state
    // threading in `sbr_chain::apply_data`.
    use ec_aac::sbr_qmf::{ANALYSIS_BANDS as AB, Analysis, SYNTHESIS_BANDS as SB, Synthesis};
    const OUTPUT_SCALE: f32 = 1.0 / 65536.0;
    for kx in [6usize, 12, 20] {
        let mut iso_out: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
        for (c, chan) in core_planes.iter().enumerate() {
            let mut analysis = Analysis::new();
            let mut synthesis = Synthesis::new();
            let n_slots = chan.len() / AB;
            for slot in 0..n_slots {
                let mut chunk = [0f32; 32];
                chunk.copy_from_slice(&chan[slot * AB..(slot + 1) * AB]);
                for s in &mut chunk {
                    *s /= OUTPUT_SCALE;
                }
                let sub = analysis.process_slot(&chunk);
                let mut v = [ec_dsp::Complex::new(0.0, 0.0); 64];
                v[0..kx.min(AB)].copy_from_slice(&sub[0..kx.min(AB)]);
                let mut pcm = synthesis.process_slot(&v);
                for s in &mut pcm {
                    *s *= OUTPUT_SCALE;
                }
                iso_out[c].extend_from_slice(&pcm);
            }
            let _ = SB;
        }
        for (och, ochan) in iso_out.iter().enumerate() {
            for (rch, rplane) in [&ref_l, &ref_r].iter().enumerate() {
                let (lag, corr) = best_lag_correlation_wide(ochan, rplane, SEARCH_LAG_MAX);
                let name = if rch == 0 { "L" } else { "R" };
                println!("iso kx={kx} ch{och} vs ref {name}: lag={lag} corr={corr:.6}");
            }
        }
    }
}

/// Full decode of one container-native AAC stream, WITHOUT `our_decode`'s
/// `MAX_SAMPLES` (~22.7s @44100) cap -- this probe needs the ~30s the
/// per-1s lag-drift table walks, and the sample-count comparison below
/// needs the true total the decoder produced, not a cap-truncated one.
/// Bounded only by `extract_from`'s existing 1_500-AU read cap.
pub(super) fn our_decode_uncapped(path: &Path, stream_index: usize) -> Option<(Vec<Vec<f32>>, u32, usize)> {
    let (asc, aus) = extract_aac_track(path, stream_index)?;
    let mut decoder = AacDecoder::with_config_bytes(&asc).ok()?;
    let rate = decoder.output_sample_rate().unwrap_or(0);
    let mut planes: Vec<Vec<f32>> = Vec::new();
    for au in &aus {
        if let Ok(f) = decoder.decode(au, None) {
            let ch = usize::from(f.channels);
            if ch == 0 {
                continue;
            }
            if planes.is_empty() {
                planes = vec![Vec::new(); ch];
            }
            for (i, v) in f.samples.iter().enumerate() {
                planes[i % ch].push(*v);
            }
        }
    }
    if planes.is_empty() {
        return None;
    }
    Some((planes, rate, aus.len()))
}

/// Total sample count of an unbounded ffmpeg decode of one container-native
/// AAC stream, interleaved-`f32` byte count / 4 -- i.e. `channels *
/// per_channel_samples`, not capped at 30s the way `ffmpeg_decode` is.
pub(super) fn ffmpeg_total_interleaved_samples(path: &Path, absolute_stream: usize) -> u64 {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            &format!("0:{absolute_stream}"),
            "-f",
            "f32le",
            "-acodec",
            "pcm_f32le",
            "-",
        ])
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg full decode failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout.len() as u64 / 4
}

/// `ffprobe -show_entries stream=<entry>` on the file's first audio stream,
/// trimmed. Used to cross-check the container's declared `sample_rate` and
/// `duration` against what our decoder derives and what ffmpeg actually
/// produces.
pub(super) fn ffprobe_field(path: &Path, entry: &str) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            &format!("stream={entry}"),
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Narrow (`center +/- half`) fixed-window lag search anchored at ours-index
/// `oa0`, `len` samples long -- immune to the periodic-content aliasing that
/// makes `best_lag_correlation_ex`'s free ~100_000-wide coarse search
/// unreliable on this file (see the ledger dead-end on ref-L-vs-ref-R
/// itself mis-landing at lag=-24). `theirs`-index is `ours_index + lag`,
/// the same convention `best_lag_correlation_ex::slice_at` uses.
pub(super) fn narrow_lag_at(
    ours: &[f32],
    theirs: &[f32],
    oa0: usize,
    len: usize,
    center: i64,
    half: i64,
) -> (i64, f64) {
    let mut best = (center, -2.0f64);
    for lag in (center - half)..=(center + half) {
        let ob_i = oa0 as i64 + lag;
        if ob_i < 0 {
            continue;
        }
        let (oa, ob) = (oa0, ob_i as usize);
        if oa + len > ours.len() || ob + len > theirs.len() {
            continue;
        }
        let c = correlation(&ours[oa..oa + len], &theirs[ob..ob + len]);
        if c > best.1 {
            best = (lag, c);
        }
    }
    best
}

/// Round-55 charter probe: is the `heaac_44100_48k.m4a` (44.1k-family HE-AAC)
/// lag-vs-time drift a sample-count mismatch between our decode and
/// ffmpeg's reference (dropped/duplicated AU somewhere in the chain), or a
/// rate-label bug (we call it 44100 but produce samples at another rate),
/// or per-AU apply_last/hold fallbacks accumulating skew? Runs the same
/// three measurements on one 48k-family HE-AAC fixture and on reeln
/// (real 44.1k HE-AAC) as controls that are known NOT to show the drift.
#[test]
pub(super) fn sbr441_family_sample_drift_probe() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::var("EC_AAC_HEAAC_FIXTURES")
        .unwrap_or_else(|_| format!("{}/../../.cache/heaac-fixtures", env!("CARGO_MANIFEST_DIR")));
    let home = std::env::var("HOME").unwrap_or_default();
    // `synthetic` marks the rows served by the committed fixture library
    // (`scripts/aac-tables/make-heaac-fixtures.sh`); the reeln control is
    // a `~/Music` host corpus and keeps its plain skip.
    let cases: [(&str, PathBuf, i64, bool); 3] = [
        (
            "heaac_44100_48k (FAIL family)",
            PathBuf::from(&dir).join("heaac_44100_48k.m4a"),
            -4674,
            true,
        ),
        (
            "heaac_48000_48k (48k control)",
            PathBuf::from(&dir).join("heaac_48000_48k.m4a"),
            // corrected via this test's own wide free-search cross-check
            // (below) -- 0 was a bad guess, true fixed lag is -4673, same
            // as the failing family's.
            -4673,
            true,
        ),
        (
            "reeln (44.1k control)",
            PathBuf::from(format!("{home}/Music/reeln.mp4")),
            // corrected via this test's own wide free-search cross-check
            // (below) -- -576 was this file's CORE-vs-FULL-chain delay
            // from an earlier probe, not its full-chain-vs-ffmpeg-reference
            // lag, which is +385.
            385,
            false,
        ),
    ];
    for (label, path, seed_lag, synthetic) in cases {
        println!("\n=== {label}: {} ===", path.display());
        if synthetic {
            if !require_fixture(&path, "scripts/aac-tables/make-heaac-fixtures.sh") {
                continue;
            }
        } else if !path.exists() {
            eprintln!("SKIP {label}: {} absent", path.display());
            continue;
        }
        let holds_before = ec_aac::hold_call_count();
        let Some((ours, out_rate, au_count)) = our_decode_uncapped(&path, 0) else {
            eprintln!("SKIP {label}: our_decode_uncapped failed");
            continue;
        };
        // `ffmpeg_decode_pan_channel` caps at 10s (per its own charter,
        // shared with `reelq_multichannel_lc_matches_reference`);
        // this probe walks out to t=29s, so it needs `ffmpeg_decode`'s 30s
        // cap instead -- planar channel 0 is L for every stereo case here.
        let ref_l = ffmpeg_decode(&path, 0, 2).swap_remove(0);
        let ch0 = &ours[0];
        // Cross-check the hardcoded `seed_lag` guess with a free wide
        // search before trusting the narrow window around it -- a wrong
        // seed on a control file reads as near-zero correlation
        // everywhere, indistinguishable from "no signal", not "no drift".
        {
            let (wlag, wcorr) = best_lag_correlation_wide(ch0, &ref_l, SEARCH_LAG_MAX);
            println!("  (wide free-search cross-check: lag={wlag} corr={wcorr:.6})");
        }

        // (1) per-1s narrow lag search, +/-128 around the file's known
        // (or, for the controls, assumed-near-zero) fixed lag.
        println!("-- lag(t), narrow +/-128 window around seed lag {seed_lag} --");
        let mut lag_points: Vec<(f64, f64)> = Vec::new();
        let step = out_rate.max(1) as usize;
        for t in 0..30usize {
            let oa0 = t * step;
            if oa0 + step > ch0.len() {
                break;
            }
            let (lag, corr) = narrow_lag_at(ch0, &ref_l, oa0, step, seed_lag, 128);
            println!("  t={t:>2}s lag={lag:>7} corr={corr:.6}");
            lag_points.push((t as f64, lag as f64));
        }
        if lag_points.len() >= 2 {
            let n = lag_points.len() as f64;
            let sx: f64 = lag_points.iter().map(|(x, _)| x).sum();
            let sy: f64 = lag_points.iter().map(|(_, y)| y).sum();
            let sxy: f64 = lag_points.iter().map(|(x, y)| x * y).sum();
            let sxx: f64 = lag_points.iter().map(|(x, _)| x * x).sum();
            let slope = (n * sxy - sx * sy) / (n * sxx - sx * sx);
            println!("  fitted slope = {slope:.4} samples/second");
        }

        // (2) total sample count per channel: ours vs ffmpeg vs ffprobe
        // duration*rate vs AU-count*2048.
        let our_total = ch0.len() as u64;
        let ffmpeg_total = ffmpeg_total_interleaved_samples(&path, 0) / 2; // stereo
        let ffprobe_duration: f64 = ffprobe_field(&path, "duration").parse().unwrap_or(0.0);
        let ffprobe_rate: f64 = ffprobe_field(&path, "sample_rate").parse().unwrap_or(0.0);
        let ffprobe_total = (ffprobe_duration * ffprobe_rate).round() as u64;
        let au_implied_total = au_count as u64 * 2048;
        println!(
            "-- sample counts (per channel): ours={our_total} ffmpeg={ffmpeg_total} \
             ffprobe(duration*rate)={ffprobe_total} au_count*2048={au_implied_total} \
             (au_count={au_count}) our_decode_rate={out_rate} ffprobe_rate={ffprobe_rate} \
             ratio(ours/ffmpeg)={:.6}",
            our_total as f64 / ffmpeg_total.max(1) as f64
        );

        // (4) apply_last/hold fallback count for this file's own decode
        // (process-wide counter, delta'd against this file's own before/
        // after since the test loop shares one process).
        let holds = ec_aac::hold_call_count() - holds_before;
        println!("-- apply_last (hold) calls during this file's decode: {holds} of {au_count} AUs");
    }
}

/// Round-56 charter probe: at the corr cliff `heaac_48000_64k.m4a` shows
/// once the synthetic sweep content reaches the SBR HF register (good
/// t=14s, bad t=18s/24s per the ledger lever), print per-band |X| (dB) and
/// ours-minus-reference phase for 8 consecutive QMF slots at each of the
/// three timestamps, reading both sides through
/// [`ec_aac::sbr_qmf::HfAnalysis`] via the existing `qmf_domain_spec` third
/// witness (ISO/IEC 14496-3 4.6.18.4 64-band analysis of each side's own
/// final PCM, band unit = `rate/128` Hz -- exactly the patch table's own
/// unit). Also prints the file's own patch table (ISO/IEC 14496-3
/// 4.6.18.6.3 `build_patches`) and an `EC_AAC_SBR_HF_BYPASS` A/B so a
/// "our HF actively hurts" vs "our HF is merely incomplete" verdict is
/// readable straight from the printed corr numbers.
#[test]
pub(super) fn sbr_hf_window_band_probe() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::var("EC_AAC_HEAAC_FIXTURES")
        .unwrap_or_else(|_| format!("{}/../../.cache/heaac-fixtures", env!("CARGO_MANIFEST_DIR")));
    let path = PathBuf::from(&dir).join("heaac_48000_64k.m4a");
    if !require_fixture(&path, "scripts/aac-tables/make-heaac-fixtures.sh") {
        return;
    }
    const LAG: i64 = -4673;

    let Some((ours, out_rate, _au_count)) = our_decode_uncapped(&path, 0) else {
        eprintln!("SKIP: our_decode_uncapped failed");
        return;
    };
    let ref_l = ffmpeg_decode(&path, 0, 2).swap_remove(0);
    let ch0 = &ours[0];

    for t in [14usize, 18, 24] {
        let oa0 = t * out_rate as usize;
        if oa0 + out_rate as usize > ch0.len() {
            continue;
        }
        let (lag, corr) = narrow_lag_at(ch0, &ref_l, oa0, out_rate as usize, LAG, 32);
        let (wlag, wcorr) = narrow_lag_at(ch0, &ref_l, oa0, out_rate as usize, LAG, 8192);
        println!(
            "\n=== t={t}s narrow-lag corr={corr:.6} lag={lag} (wide +/-8192 search: lag={wlag} corr={wcorr:.6}) ==="
        );
        let ob0 = (oa0 as i64 + lag) as usize;
        // 8 QMF slots (512 samples) starting at this second's aligned position.
        const WIN: usize = 8 * 64;
        if oa0 + WIN > ch0.len() || ob0 + WIN > ref_l.len() {
            println!("  SKIP: window out of range");
            continue;
        }
        let (o_spec, t_spec, _sim) = qmf_domain_spec(&ch0[oa0..oa0 + WIN], &ref_l[ob0..ob0 + WIN]);
        for band in 14..48usize {
            let mut mags_o = Vec::new();
            let mut mags_t = Vec::new();
            let mut dphase_deg = Vec::new();
            for slot in 0..8 {
                let o = o_spec[band][slot];
                let r = t_spec[band][slot];
                let mo = (o.re * o.re + o.im * o.im).sqrt();
                let mr = (r.re * r.re + r.im * r.im).sqrt();
                mags_o.push(20.0 * (mo.max(1e-12)).log10());
                mags_t.push(20.0 * (mr.max(1e-12)).log10());
                if mo > 1e-6 && mr > 1e-6 {
                    let d = (o.im.atan2(o.re) - r.im.atan2(r.re)).to_degrees();
                    let d = ((d + 180.0).rem_euclid(360.0)) - 180.0;
                    dphase_deg.push(d);
                } else {
                    dphase_deg.push(f64::NAN);
                }
            }
            let mean_o: f64 = mags_o.iter().sum::<f64>() / 8.0;
            let mean_t: f64 = mags_t.iter().sum::<f64>() / 8.0;
            println!(
                "  band{band:>3}: |ours|dB mean={mean_o:>7.2} |ref|dB mean={mean_t:>7.2} dphase(deg)={:?}",
                dphase_deg
                    .iter()
                    .map(|d| format!("{d:.0}"))
                    .collect::<Vec<_>>()
            );
        }
    }

    // HF-bypass A/B on the bad windows: does zeroing our HF raise or lower
    // corr against the reference in [18s,19s) and [24s,25s)?
    let bypass = {
        let _bypass = ec_aac::knobs::set_sbr_hf_bypass(Some(true));
        our_decode_uncapped(&path, 0)
    };
    if let Some((bp, bp_rate, _)) = bypass {
        let bch0 = &bp[0];
        for t in [14usize, 18, 24] {
            let oa0 = t * bp_rate as usize;
            if oa0 + bp_rate as usize > bch0.len() {
                continue;
            }
            let (lag, corr) = narrow_lag_at(bch0, &ref_l, oa0, bp_rate as usize, LAG, 32);
            println!("  t={t}s HF_BYPASS corr={corr:.6} lag={lag}");
        }
    } else {
        eprintln!("HF_BYPASS decode failed");
    }

    // Patch table (ISO/IEC 14496-3 4.6.18.6.3), sideinfo-derived.
    {
        let _sideinfo = ec_aac::knobs::set_sbr_sideinfo_debug(Some(true));
        let _ = our_decode_uncapped(&path, 0);
    }
    if let Some(row) = ec_aac::sbr_sideinfo_log().first() {
        println!(
            "\n-- patch table (first frame): kx={} k2={} patch widths={:?}",
            row.kx, row.k2, row.patch_lengths
        );
    }
}

/// Round-59 residual locator: per-2048-sample-window corr at lag 0 (worst
/// windows, with the output frame index) plus a complex-STFT per-band
/// correlation (Parseval: equals the time-domain corr restricted to that
/// band, so it needs no calibration) with an ours-vs-ours self-check column.
#[test]
pub(super) fn sbr_residual_locator() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        return;
    }
    // lane-h264reallib: `unwrap_or_default()`, the shape this file's five
    // other HOME reads already use (:2710, :3426, :4109, :4182, :4575). A bare
    // `unwrap()` panicked on any host with no HOME -- a fleet runner -- which
    // is a red with no bearing on the decoder. The `path.exists()` guard below
    // already handles the absent-file case, so an empty HOME just makes the
    // fallback path relative-and-nonexistent and the test returns.
    let home = std::env::var("HOME").unwrap_or_default();
    // `EC_AAC_SBR_LOCATOR_FILE` points the same instrument at any other
    // HE-AAC file (e.g. a mono, uncoupled, noise-only probe encode).
    let path = std::env::var("EC_AAC_SBR_LOCATOR_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("{home}/Music/reeln.mp4")));
    if !path.exists() {
        return;
    }
    let Some((ours, _, rate)) = our_decode(&path, 0) else {
        return;
    };
    let theirs = ffmpeg_decode(&path, 0, ours.len());
    const W: usize = 2048;
    const FFT_LEN: usize = 2048;
    const HOP: usize = 1024;
    let bins = FFT_LEN / 2 + 1;
    let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
    let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
    for ch in 0..ours.len() {
        let n = ours[ch].len().min(theirs[ch].len());
        let (o, t) = (&ours[ch][..n], &theirs[ch][..n]);
        let mut wins: Vec<(f64, usize)> = (0..n / W)
            .map(|w| {
                (
                    correlation(&o[w * W..(w + 1) * W], &t[w * W..(w + 1) * W]),
                    w,
                )
            })
            .collect();
        wins.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        println!("ch{ch} worst windows (corr, frame): {:?}", &wins[..12]);
        let hops = (n - FFT_LEN) / HOP + 1;
        let mut spec = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
        let mut acc = vec![[0.0f64; 3]; bins]; // re(o*conj t), |o|^2, |t|^2
        let mut os = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
        for h in 0..hops {
            let mut b = o[h * HOP..h * HOP + FFT_LEN].to_vec();
            win.apply(&mut b);
            rfft.forward(&b, &mut spec);
            os.copy_from_slice(&spec);
            let mut b = t[h * HOP..h * HOP + FFT_LEN].to_vec();
            win.apply(&mut b);
            rfft.forward(&b, &mut spec);
            for k in 0..bins {
                let (a, c) = (os[k], spec[k]);
                acc[k][0] += f64::from(a.re * c.re + a.im * c.im);
                acc[k][1] += f64::from(a.norm_sqr());
                acc[k][2] += f64::from(c.norm_sqr());
            }
        }
        let hz = f64::from(rate) / FFT_LEN as f64;
        let tot: f64 = acc.iter().map(|a| a[1] + a[2]).sum();
        // With `EC_AAC_SBR_NOISE_FRACTION` set the decoder bookkeeps the
        // dequantized target per QMF band; print it next to the reference's
        // measured energy in the same band so a target-side (bitstream ->
        // E_orig) tilt separates from an adjust/synthesis-side one.
        if ch == 0 {
            let band_hz = f64::from(rate) / 128.0;
            for (band, sig, noi, out) in ec_aac::noise_fraction_table() {
                let (lo, hi) = (
                    (band as f64 * band_hz / hz) as usize,
                    ((band + 1) as f64 * band_hz / hz) as usize,
                );
                let pt: f64 = (lo..hi.min(bins)).map(|k| acc[k][2]).sum();
                let po: f64 = (lo..hi.min(bins)).map(|k| acc[k][1]).sum();
                println!(
                    "TARGET band {band:>2} {:>6.0}Hz target {:.3e} realised/target {:.4} ref_pcm {:.3e} ours_pcm {:.3e} target/ref_pcm {:.3e}",
                    band as f64 * band_hz,
                    sig + noi,
                    out / (sig + noi),
                    pt,
                    po,
                    (sig + noi) / pt
                );
            }
        }
        // group into 250 Hz bands up to 8 kHz, then 1 kHz
        let mut edges: Vec<usize> = (0..=32)
            .map(|i| ((i as f64 * 250.0) / hz) as usize)
            .collect();
        edges.extend((9..=22).map(|i| ((i as f64 * 1000.0) / hz) as usize));
        for e in edges.windows(2) {
            let (mut c, mut po, mut pt) = (0.0, 0.0, 0.0);
            for row in acc.iter().take(e[1].min(bins)).skip(e[0]) {
                c += row[0];
                po += row[1];
                pt += row[2];
            }
            println!(
                "ch{ch} {:5.0}-{:5.0} Hz corr {:.5} rms ours/ref {:.4} energy share {:.4}",
                e[0] as f64 * hz,
                e[1] as f64 * hz,
                c / (po * pt).sqrt(),
                (po / pt).sqrt(),
                (po + pt) / tot
            );
        }
    }
}
