//! SBR real-library checks and probes, part B.

use super::*;

/// (Round-41, Task 1) Instrument anchor: before trusting `bin_level_conviction`'s
/// low (~0.15-0.25) HF direct-coherence numbers on reeln as evidence the
/// HF content is genuinely different, confirm the measurement itself reads
/// near 1.0 on pairs that are KNOWN to carry the same content. Three
/// controls:
///   A. ours vs ours, decoded independently twice -- a determinism check on
///      our own decode AND the coherence arithmetic's floor.
///   B. the reference's own PCM from the original file vs from a remux of
///      the SAME access units/ASC through a fresh container -- near-1.0
///      expected; anything else would implicate the remux path, not SBR.
///   C. the reference's own PCM vs itself read 64 samples later (one QMF
///      slot) -- sanity that `bin_level_conviction`'s STFT hop/window does
///      not itself destroy coherence for equivalent content at a fixed
///      sample offset, the same shape of offset the real `oa`/`ob` alignment
///      applies above.
/// If any control reads below ~0.9 on the HF bands, the instrument -- not
/// the SBR chain -- is the story; every prior round's coherence number would
/// need to be recalibrated against whatever this control actually measures.
#[test]
pub(super) fn sbr_instrument_anchor_controls() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if std::env::var("EC_AAC_SBR_INSTRUMENT_ANCHOR").is_err() {
        eprintln!(
            "SKIP (round-41 Task 1): EC_AAC_SBR_INSTRUMENT_ANCHOR=1 cargo test -p ec-aac \
             --release --test sbr_real_library sbr_instrument_anchor_controls -- --nocapture"
        );
        return;
    }
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let path = PathBuf::from(format!("{home}/Music/reeln.mp4"));
    if !path.exists() {
        eprintln!("SKIP: {} not found", path.display());
        return;
    }
    const MIN_BAND: usize = 14;
    const TOP_N: usize = 128; // larger than any real band count -- keeps every HF band

    // Control A: ours vs ours, decoded independently twice.
    let (ours_a, _, rate) = our_decode(&path, 0).expect("our_decode works");
    let (ours_b, _, _) = our_decode(&path, 0).expect("our_decode works (2nd pass)");
    let n = ours_a[0].len().min(ours_b[0].len()).min(MAX_SAMPLES);
    let rows = bin_level_conviction(&ours_a[0][..n], &ours_b[0][..n], rate, MIN_BAND, TOP_N);
    report_conviction("A: ours-vs-ours (determinism)", &rows);

    // Control B: reference PCM from the original file vs from a remux of the
    // SAME access units/ASC.
    let (asc, aus) = extract_aac_track(&path, 0).expect("track extracts");
    let remux_path = std::env::temp_dir().join("ec_aac_sbr_instrument_anchor_remux.mp4");
    write_remux(&remux_path, &asc, &aus);
    let theirs_orig = ffmpeg_decode(&path, 0, 2);
    let theirs_remux = ffmpeg_decode(&remux_path, 0, 2);
    let _ = std::fs::remove_file(&remux_path);
    let n = theirs_orig[0]
        .len()
        .min(theirs_remux[0].len())
        .min(MAX_SAMPLES);
    let (lag_b, _) = best_lag_correlation(&theirs_orig[0][..n], &theirs_remux[0][..n]);
    let (oa, ob) = if lag_b >= 0 {
        (0usize, lag_b as usize)
    } else {
        ((-lag_b) as usize, 0usize)
    };
    let win = n.saturating_sub(oa.max(ob));
    let rows = bin_level_conviction(
        &theirs_orig[0][oa..oa + win],
        &theirs_remux[0][ob..ob + win],
        rate,
        MIN_BAND,
        TOP_N,
    );
    report_conviction(
        &format!("B: reference-vs-reference-through-remux (lag {lag_b})"),
        &rows,
    );

    // Control C: reference PCM vs itself read 64 samples later (one QMF
    // slot) -- both slices are the SAME underlying samples, just offset.
    let t = &theirs_orig[0][..MAX_SAMPLES.min(theirs_orig[0].len())];
    if t.len() > 64 {
        let rows = bin_level_conviction(&t[64..], &t[..t.len() - 64], rate, MIN_BAND, TOP_N);
        report_conviction("C: reference vs reference, 64-sample offset", &rows);
    }
}

/// (Round-41, Task 2) Actual injected HF noise energy, measured as OUTPUT
/// energy rather than trusted from either side of round-24's contradiction:
/// `noise_fraction_table` bookkept a 0.37-0.39 noise share from the
/// transmitted envelope/noise split, while separately zeroing the injected
/// noise (`EC_AAC_SBR_NOISE_ZERO`, which only skips the PCM addition in
/// `sbr_env::adjust`, not the bookkeeping) was found to change full-band
/// correlation by <0.001 -- both cannot be true of the same signal. This
/// decodes reeln twice (noise on, then off) and reads the per-HF-band
/// `bin_level_conviction` energy delta between them as the fraction noise
/// ACTUALLY realizes in the output, independent of either prior claim.
#[test]
pub(super) fn sbr_actual_noise_fraction() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if std::env::var("EC_AAC_SBR_NOISE_ANCHOR").is_err() {
        eprintln!(
            "SKIP (round-41 Task 2): EC_AAC_SBR_NOISE_ANCHOR=1 cargo test -p ec-aac \
             --release --test sbr_real_library sbr_actual_noise_fraction -- \
             --nocapture --test-threads=1 (env-gated because it sets process \
             env vars mid-run, which is unsound alongside parallel tests)"
        );
        return;
    }
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let path = PathBuf::from(format!("{home}/Music/reeln.mp4"));
    if !path.exists() {
        eprintln!("SKIP: {} not found", path.display());
        return;
    }
    const MIN_BAND: usize = 14;
    const TOP_N: usize = 128;

    let (ours_on, _, rate) = {
        let _zero = ec_aac::knobs::set_sbr_noise_zero(Some(false));
        let _fraction = ec_aac::knobs::set_sbr_noise_fraction(Some(true));
        our_decode(&path, 0).expect("our_decode works (noise on)")
    };
    let bookkept = ec_aac::noise_fraction_table();
    let (ours_off, _, _) = {
        let _zero = ec_aac::knobs::set_sbr_noise_zero(Some(true));
        let _fraction = ec_aac::knobs::set_sbr_noise_fraction(Some(false));
        our_decode(&path, 0).expect("our_decode works (noise zeroed)")
    };

    let theirs = ffmpeg_decode(&path, 0, ours_on.len());
    let n = ours_on[0].len().min(ours_off[0].len()).min(MAX_SAMPLES);
    let on = &ours_on[0][..n];
    let off = &ours_off[0][..n];

    // Per-band actual energy delta: `energy` in `bin_level_conviction`'s row
    // is always the FIRST argument's own band energy, so swap argument order
    // between the two calls to read both sides' energies at the SAME band
    // set (TOP_N kept above any real band count so both calls select every
    // band, not just each side's own top few).
    let rows_on = bin_level_conviction(on, off, rate, MIN_BAND, TOP_N);
    let rows_off = bin_level_conviction(off, on, rate, MIN_BAND, TOP_N);
    // (Round-42, Task 2) This `band_hz` feeds the `band` used below to look
    // up `bookkept` (`noise_fraction_table()`'s QMF band index, `tables.kx`
    // .. `tables.k2`, width `rate/128` -- 64 QMF bands over the SBR
    // Nyquist, per the NOISE-FRACTION-DEBUG dump above). It is NOT
    // `bin_level_conviction`'s own internal FFT-band scale (`rate/256`,
    // used correctly elsewhere in this file for THAT function's own rows).
    // Using `rate/256` here silently doubled every looked-up `band` index,
    // so every real band above roughly half of `k2` missed the table and
    // read back a false 0.0 bookkept fraction -- a test-side lookup bug,
    // not a decoder coverage gap (verified: `noise_fraction_table()` itself
    // has nonzero entries for QMF bands up to `k2`).
    let band_hz = f64::from(rate) / 128.0;
    let kx_band = bookkept.iter().map(|(b, ..)| *b).min().unwrap_or(0);
    println!(
        "    (crossover: QMF band {kx_band}, {:.0} Hz -- rows below it are core-decoded and excluded from the total)",
        kx_band as f64 * band_hz
    );
    println!(
        "  ACTUAL noise fraction (band_hz, energy_on, energy_off, actual_fraction, bookkept_fraction):"
    );
    let mut sum_on = 0.0f64;
    let mut sum_delta = 0.0f64;
    for (hz, e_on, ..) in &rows_on {
        let e_off = rows_off
            .iter()
            .find(|(hz2, ..)| (hz2 - hz).abs() < 1.0)
            .map(|(_, e, ..)| *e)
            .unwrap_or(0.0);
        let actual = if *e_on > 0.0 {
            (e_on - e_off) / e_on
        } else {
            0.0
        };
        let band = (hz / band_hz).round() as usize;
        let book = bookkept
            .iter()
            .find(|(b, ..)| *b == band)
            .map(|(_, s, n, _)| if s + n > 0.0 { n / (s + n) } else { 0.0 })
            .unwrap_or(0.0);
        println!(
            "    {hz:>6.0}Hz: on {e_on:.3e} off {e_off:.3e} actual {actual:.4} bookkept {book:.4}"
        );
        // HF ONLY (round-61): `MIN_BAND` (14, ~2412 Hz at 44.1k) sits BELOW
        // this stream's crossover (~4500 Hz), so the row list opens with a
        // dozen core-decoded bands that carry an order of magnitude more
        // energy than the SBR-generated ones (6.4e4 vs ~5e3) and, by
        // definition, zero injected noise. Summing them into a "whole-HF"
        // ratio dilutes it toward zero -- that is how this metric read
        // 0.0397 while every band above the crossover was realizing
        // 0.15-0.27. The crossover comes from the bookkeeping table itself
        // (its lowest QMF band IS `kx`), so it tracks the stream instead of
        // a constant that a QMF-mapping change can invalidate.
        if band < kx_band {
            continue;
        }
        sum_on += e_on;
        sum_delta += e_on - e_off;
    }
    let actual_total = if sum_on > 0.0 {
        sum_delta / sum_on
    } else {
        0.0
    };
    println!("  ACTUAL whole-HF noise fraction: {actual_total:.4}");

    // Corr re-verification: does zeroing the injected noise move full-band
    // correlation against the reference, and by how much (round-24 claimed
    // <0.001).
    let (_, corr_on) = best_lag_correlation(on, &theirs[0][..theirs[0].len().min(MAX_SAMPLES)]);
    let (_, corr_off) = best_lag_correlation(off, &theirs[0][..theirs[0].len().min(MAX_SAMPLES)]);
    println!(
        "  full-band corr: noise-on {corr_on:.6} noise-off {corr_off:.6} delta {:.6}",
        corr_off - corr_on
    );

    // (Round-42, Task 1/3 acceptance) Two real accounting bugs fixed this
    // round pushed the realized whole-HF fraction 0.1325 -> 0.1787 (reeln,
    // this same measurement): the boost pass was conserving the cell's FULL
    // envelope target against a signal-only realized sum (now conserves
    // total-vs-total, matching the reference algorithm's known
    // E_orig/E_curr+Q_M boost ratio), and `noise_amps` divided an
    // already-per-sample `noise_here` by the cell's QMF-band width a SECOND
    // time (removed) -- worse at wide high-frequency sfb cells, which is
    // exactly where the pre-fix gap was largest.
    //
    // The charter's ±0.05-of-bookkept (~0.38) target is NOT met by this
    // fix alone (round-43 investigated why, with `EC_AAC_SBR_CELLDUMP` in
    // `sbr_env::adjust`): `sbr_env::adjust`'s per-cell signal/noise split
    // is NOT the culprit -- corrected-band dumps (the charter's quoted
    // "6718/6891/7063Hz" and "8.5-13kHz decaying" bands turned out to be
    // mislabeled by a factor of two, this file's true SBR band spacing is
    // `rate/128` = 344.5Hz not the 172.3Hz FFT-analysis spacing those Hz
    // labels come from) show `adjust` injecting 40-85% noise share in the
    // QMF domain, at or ABOVE the ~0.37 bookkept split, not starved.
    //
    // (Round-44) Round-43's "essentially 0% of injected energy lands back
    // in the source subband" reading of
    // `synthesis_energy_gain_for_white_noise_excitation` was itself a test
    // bug, not a `Synthesis` defect: that harness measured its in-band FFT
    // window at `k0/SYNTHESIS_BANDS`, but band k's passband is centred at
    // `(k+0.5)*omega_step` with `omega_step = pi/(2*SYNTHESIS_BANDS)` --
    // HALF that fraction of Nyquist -- so the window sat on the image
    // region, not the signal. Corrected, a raw i.i.d.-per-slot draw already
    // lands ~0.49 of its own energy in its home subband's own frequency
    // range (not 0%), and a short slot-domain boxcar (`sbr_env::NoiseGen`'s
    // new `LOWPASS_TAPS = 2`) raises that to ~0.61 while staying spread
    // across the band (not collapsing to a residual tone) -- see the
    // harness's `synthesis_energy_gain_for_lowpassed_noise_excitation`.
    // That shape fix is real (an i.i.d. draw genuinely was less
    // band-faithful than it needed to be, and now is not), but re-measuring
    // THIS whole-HF metric after shipping it moved `actual_total` by <0.002
    // (0.1787 -> 0.1770, i.e. flat) -- because this metric sums the noise
    // energy DELTA across the whole swept HF spectrum, and energy an
    // i.i.d. draw aliased to a NEIGHBOURING QMF band still lands somewhere
    // inside that same swept HF range, so it was never actually excluded
    // from this particular sum. The ~0.38 bookkept vs ~0.18 actual gap is
    // therefore NOT a spectral-shape/aliasing defect (refuting round-43's
    // framing) -- it is a genuine energy shortfall, and per-band coherence
    // sits well below even the ACTUAL-fraction ceiling
    // (`sqrt(1 - f)`; ~0.15-0.20 measured vs. ~0.90 predicted at
    // `f ~= 0.177`, see `EC_AAC_SBR_PARITY_SPLIT`), meaning most of the
    // remaining HF correlation shortfall is a content defect independent of
    // noise level. Deferred: locating that energy shortfall and the
    // content defect, budget did not reach either this round (open, see
    // ledger). Floor tightened from round-42's 0.15 to the level this
    // fix's shape change actually lands at (0.177, both before and after),
    // with margin for run-to-run FFT/decode jitter.
    assert!(
        actual_total > 0.17,
        "whole-HF realized noise fraction regressed to {actual_total:.4} \
         (round-44 fix floor is 0.17; measured 0.1770 post-shape-fix, \
         0.1787 pre-fix -- shape does not move this metric, see doc above)"
    );
}

/// Discriminator instrument: one row per stream of the SBR header/frame
/// features actually parsed from its first ~20 `sbr_data()` frames, next to
/// that stream's own pass/fail correlation vs the reference decoder --
/// looking for which feature(s) separate the two known-PASS streams
/// (reeln, synthetic 48k HE) from the two known-FAIL families (FMJ,
/// synthetic 44.1k HE). Gated on the real files / fixture directory being
/// present, same as the tests it borrows machinery from; skips loudly,
/// generates nothing.
#[test]
pub(super) fn sbr_header_feature_table() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    struct Row {
        label: String,
        path: PathBuf,
        aac_stream: usize,
        ffmpeg_stream: usize,
    }
    let mut rows: Vec<Row> = candidates()
        .into_iter()
        .map(|c| Row {
            label: c
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: c.path,
            aac_stream: c.aac_stream,
            ffmpeg_stream: c.ffmpeg_stream,
        })
        .collect();
    let fixtures = std::env::var("EC_AAC_HEAAC_FIXTURES")
        .unwrap_or_else(|_| format!("{}/../../.cache/heaac-fixtures", env!("CARGO_MANIFEST_DIR")));
    let fixtures = PathBuf::from(fixtures);
    if require_fixture(&fixtures, "scripts/aac-tables/make-heaac-fixtures.sh") {
        for rate in [48_000u32, 44_100] {
            for br in ["32k", "48k", "64k", "96k"] {
                let p = fixtures.join(format!("heaac_{rate}_{br}.m4a"));
                if require_fixture(&p, "scripts/aac-tables/make-heaac-fixtures.sh") {
                    rows.push(Row {
                        label: format!("synth-{rate}-{br}"),
                        path: p,
                        aac_stream: 0,
                        ffmpeg_stream: 0,
                    });
                }
            }
        }
    }
    if rows.is_empty() {
        eprintln!("SKIP: no real HE-AAC files and no synthetic fixtures found");
        return;
    }
    let _knob = ec_aac::knobs::set_sbr_sideinfo_debug(Some(true));
    const WIDE_LAG_MAX: i64 = 20_000;
    println!(
        "{:<20} {:>6} {:>4} {:>4} {:>4} {:>4} {:>4}  {:>4} {:>4} {:>4} {:>4} {:>4}  {:>5} {:>10} {:>10}  {:>8} {:>8}  {:>10} {:>6}",
        "stream",
        "rate",
        "kx",
        "k2",
        "xovr",
        "sfrq",
        "efrq",
        "fscl",
        "ascl",
        "nbnd",
        "lbnd",
        "lgn",
        "ifrq",
        "smooth",
        "envs",
        "invf",
        "harm",
        "df%",
        "corr"
    );
    for r in &rows {
        let before = ec_aac::sbr_sideinfo_log().len();
        let Some((ours, sbr, our_rate)) = our_decode(&r.path, r.aac_stream) else {
            eprintln!("SKIP {}: could not decode", r.label);
            continue;
        };
        if sbr != ec_aac::SbrSupport::V1 {
            eprintln!("SKIP {}: not SBR v1", r.label);
            continue;
        }
        let log = ec_aac::sbr_sideinfo_log();
        let frames: Vec<_> = log[before..]
            .iter()
            .filter(|row| row.ch == 0)
            .take(20)
            .collect();
        if frames.is_empty() {
            eprintln!("SKIP {}: no sideinfo rows captured", r.label);
            continue;
        }
        let f0 = &frames[0];
        let mut env_counts: std::collections::BTreeMap<usize, usize> = Default::default();
        let mut invf_seen: std::collections::BTreeSet<u8> = Default::default();
        let mut any_harmonic = false;
        let mut df_total = 0usize;
        let mut df_ones = 0usize;
        for row in &frames {
            *env_counts
                .entry(row.t_env.len().saturating_sub(1))
                .or_default() += 1;
            invf_seen.extend(&row.invf_mode);
            if row
                .add_harmonic
                .as_ref()
                .is_some_and(|h| h.iter().any(|&v| v != 0))
            {
                any_harmonic = true;
            }
            df_total += row.df_env.len() + row.df_noise.len();
            df_ones += row
                .df_env
                .iter()
                .chain(&row.df_noise)
                .filter(|&&b| b != 0)
                .count();
        }
        let df_pct = if df_total > 0 {
            100.0 * df_ones as f64 / df_total as f64
        } else {
            0.0
        };
        let envs_hist: String = env_counts
            .iter()
            .map(|(k, v)| format!("{k}x{v}"))
            .collect::<Vec<_>>()
            .join(",");
        let invf_str: String = invf_seen
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("");
        let theirs = ffmpeg_decode(&r.path, r.ffmpeg_stream, ours.len());
        let (_, corr) = best_lag_correlation_wide(&ours[0], &theirs[0], WIDE_LAG_MAX);
        println!(
            "{:<20} {:>6} {:>4} {:>4} {:>4} {:>4} {:>4}  {:>4} {:>4} {:>4} {:>4} {:>4}  {:>5} {:>10} {:>10}  {:>8} {:>8}  {:>10.1} {:>6.3}",
            r.label,
            our_rate,
            f0.kx,
            f0.k2,
            f0.xover_band,
            f0.start_freq,
            f0.stop_freq,
            f0.freq_scale,
            f0.alter_scale,
            f0.noise_bands,
            f0.limiter_bands,
            f0.limiter_gains,
            f0.interpol_freq,
            f0.smoothing_mode,
            envs_hist,
            invf_str,
            any_harmonic,
            df_pct,
            corr
        );
        println!("  patches (frame0): {:?}", f0.patch_lengths);
    }
}

/// Round (sbr-per-au): per-AU localization. Decodes the same AU list through
/// (a) the core-only decoder and (b) the full SBR chain, tracks each AU's
/// sample range in both output streams as it goes (so a per-AU decode
/// failure on either side does not desync the mapping), aligns the two
/// streams ONCE in the core's own native rate domain (same decimate
/// convention `synthetic_heaac_matrix`'s `ours_v_core` row already trusts),
/// then re-uses that single lag for every AU's own low-band correlation.
/// Prints the first 5 AUs below 0.9 with that AU's SBR side-info (source/
/// envelope count/freq_res/kx/k2) so a per-frame trigger (e.g. `hold`
/// frames, a specific `freq_res`) shows up directly instead of needing a
/// second pass. If the first divergent AU is 0, that is reported explicitly
/// -- divergence from the very first AU means a per-stream setup mismatch,
/// not a per-frame trigger.
pub(super) fn per_au_low_band_probe(label: &str, path: &Path, aac_stream: usize) {
    let Some((asc, aus)) = extract_aac_track(path, aac_stream) else {
        eprintln!("SKIP {label}: extract_aac_track failed");
        return;
    };
    let Ok(full_cfg) = parse_audio_specific_config(&asc) else {
        eprintln!("SKIP {label}: asc parse failed");
        return;
    };
    if !full_cfg.sbr_present {
        eprintln!("SKIP {label}: no SBR");
        return;
    }
    let mut core_cfg = full_cfg.clone();
    core_cfg.sbr_present = false;
    core_cfg.ps_present = false;
    core_cfg.extension_sample_rate = None;
    let core_rate = core_cfg.sample_rate;
    let mut core_dec = AacDecoder::with_config(core_cfg);
    let mut full_dec = AacDecoder::with_config(full_cfg);
    let full_rate = full_dec.output_sample_rate().unwrap_or(core_rate * 2);

    struct AuInfo {
        core_range: (usize, usize),
        side: Option<ec_aac::SbrSideInfoRow>,
    }
    let mut core_ch0: Vec<f32> = Vec::new();
    let mut full_ch0: Vec<f32> = Vec::new();
    let mut aus_info: Vec<AuInfo> = Vec::new();
    for au in aus.iter().take(1_500) {
        let side_before = ec_aac::sbr_sideinfo_log().len();
        let cs = core_ch0.len();
        if let Ok(f) = core_dec.decode(au, None) {
            let ch = usize::from(f.channels).max(1);
            core_ch0.extend(f.samples.iter().step_by(ch).copied());
        }
        let ce = core_ch0.len();
        if let Ok(f) = full_dec.decode(au, None) {
            let ch = usize::from(f.channels).max(1);
            full_ch0.extend(f.samples.iter().step_by(ch).copied());
        }
        let side = ec_aac::sbr_sideinfo_log()[side_before..]
            .iter()
            .find(|r| r.ch == 0)
            .cloned();
        aus_info.push(AuInfo {
            core_range: (cs, ce),
            side,
        });
        if full_ch0.len() > MAX_SAMPLES {
            break;
        }
    }
    if core_ch0.is_empty() || full_ch0.is_empty() {
        eprintln!(
            "SKIP {label}: empty decode (core {} full {})",
            core_ch0.len(),
            full_ch0.len()
        );
        return;
    }

    // Same convention `full_chain_low_band_matches_own_core` already
    // validated (0.9967 on reeln): naive nearest-neighbor 2x-upsample
    // the core into the full-rate domain, low-pass both well under the core
    // Nyquist. The decimate-into-core-rate convention `synthetic_heaac_matrix`
    // uses for its own `ours_v_core` diagnostic column was tried first here
    // and reads badly (~-0.19) even on reeln, which is known-good --
    // that column is evidently not a reliable self-consistency probe on its
    // own, so this per-AU tool anchors on the convention independently
    // proven correct instead.
    let up: Vec<f32> = core_ch0.iter().flat_map(|&s| [s, s]).collect();
    let cutoff = f64::from(core_rate) * 0.4;
    let ol = lowpass(&full_ch0, full_rate, cutoff);
    let ul = lowpass(&up, full_rate, cutoff);
    const WIDE_LAG_MAX: i64 = 20_000;
    let (lag, align_corr) = best_lag_correlation_wide(&ol, &ul, WIDE_LAG_MAX);
    println!(
        "{label}: core={core_rate}Hz full={full_rate}Hz global align (upsampled full-rate domain) lag={lag} corr={align_corr:.6}"
    );

    let mut shown = 0usize;
    let mut first_divergent: Option<usize> = None;
    for (i, info) in aus_info.iter().enumerate() {
        let (cs, ce) = info.core_range;
        if ce <= cs {
            continue;
        }
        // core_range is in core-domain sample counts; the upsampled/lowpassed
        // arrays are at full rate, so the AU's window there is 2x as wide.
        let fs = cs * 2;
        let fe = ce * 2;
        let ds = match usize::try_from(fs as i64 + lag) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let de = ds + (fe - fs);
        if de > ul.len() || fe > ol.len() {
            break;
        }
        let c = correlation(&ol[fs..fe], &ul[ds..de]);
        if c < 0.9 {
            if first_divergent.is_none() {
                first_divergent = Some(i);
            }
            if shown < 5 {
                println!("  AU {i}: low-band corr={c:.6} side={:?}", info.side);
                shown += 1;
            }
        }
    }
    match first_divergent {
        Some(0) => println!(
            "{label}: divergence starts at AU 0 -- per-STREAM setup mismatch, not a per-frame trigger"
        ),
        Some(i) => println!("{label}: first divergent AU = {i}"),
        None => println!("{label}: no AU below 0.9 corr in the probed window"),
    }
}

#[test]
pub(super) fn per_au_low_band_divergence() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let _knob = ec_aac::knobs::set_sbr_sideinfo_debug(Some(true));
    for c in &candidates() {
        per_au_low_band_probe(&c.path.display().to_string(), &c.path, c.aac_stream);
    }
    let dir = std::env::var("EC_AAC_HEAAC_FIXTURES")
        .unwrap_or_else(|_| format!("{}/../../.cache/heaac-fixtures", env!("CARGO_MANIFEST_DIR")));
    let failing = PathBuf::from(&dir).join("heaac_44100_48k.m4a");
    if require_fixture(&failing, "scripts/aac-tables/make-heaac-fixtures.sh") {
        per_au_low_band_probe("synthetic 44100 48k (failing)", &failing, 0);
    }
}

/// Coordinator follow-up: prints what `parse_audio_specific_config` actually
/// returns for a candidate's real ASC bytes (object/rate/sf_index/
/// sbr_present/extension_sample_rate), what `AacDecoder` reports back
/// (`SbrSupport`, `output_sample_rate`) from those parsed fields, then
/// re-decodes the SAME AU list through a HAND-BUILT explicit
/// `AudioSpecificConfig` (object_type=5, known core/ext rates) instead of
/// the parsed one -- if that jumps the correlations to ~1, the parsed
/// config (or something object_type-conditioned at runtime) is the defect,
/// not the SBR DSP chain itself. Also re-runs `full_chain_low_band_matches_
/// own_core`'s FMJ probe with `WIDE_LAG_MAX` raised to 100_000.
#[test]
pub(super) fn probe_explicit_config_and_wide_lag() {
    let _guard = DECODE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    fn decode_with(cfg: ec_aac::AudioSpecificConfig, aus: &[Vec<u8>]) -> (Vec<Vec<f32>>, u32) {
        let rate = match (cfg.sbr_present, cfg.ps_present) {
            (true, false) => cfg.extension_sample_rate.unwrap_or(cfg.sample_rate * 2),
            _ => cfg.sample_rate,
        };
        let mut d = AacDecoder::with_config(cfg);
        let mut planes: Vec<Vec<f32>> = Vec::new();
        for au in aus {
            let Ok(f) = d.decode(au, None) else { continue };
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
            if planes[0].len() >= MAX_SAMPLES {
                break;
            }
        }
        (planes, rate)
    }

    // `synthetic` marks the case as coming from the committed fixture library
    // (`scripts/aac-tables/make-heaac-fixtures.sh`); the FMJ row is a
    // `~/Downloads` host corpus and keeps its plain skip.
    for (
        label,
        path,
        aac_stream,
        ffmpeg_stream,
        core_sf_index,
        core_rate,
        ext_rate,
        channels,
        synthetic,
    ) in [
        (
            "FMJ",
            format!(
                "{}/Downloads/reelp/reelp.mkv",
                std::env::var("HOME").unwrap_or_default()
            ),
            1usize,
            3usize,
            6u8,
            24_000u32,
            48_000u32,
            2u16,
            false,
        ),
        (
            "heaac_44100_48k",
            format!(
                "{}/heaac_44100_48k.m4a",
                std::env::var("EC_AAC_HEAAC_FIXTURES").unwrap_or_else(|_| format!(
                    "{}/../../.cache/heaac-fixtures",
                    env!("CARGO_MANIFEST_DIR")
                ))
            ),
            0,
            0,
            4,
            44_100,
            0, // filled below from parsed cfg's own extension_sample_rate
            2,
            true,
        ),
    ] {
        let path = PathBuf::from(path);
        if synthetic {
            if !require_fixture(&path, "scripts/aac-tables/make-heaac-fixtures.sh") {
                continue;
            }
        } else if !path.exists() {
            eprintln!("SKIP {label}: {} absent", path.display());
            continue;
        }
        let Some((asc, aus)) = extract_aac_track(&path, aac_stream) else {
            eprintln!("SKIP {label}: extract failed");
            continue;
        };
        let parsed = parse_audio_specific_config(&asc);
        println!("{label}: asc={asc:02x?} parsed={parsed:?}");
        let Ok(parsed_cfg) = parsed else {
            eprintln!("SKIP {label}: parse failed");
            continue;
        };
        let d = AacDecoder::with_config(parsed_cfg.clone());
        println!(
            "  runtime: sbr_support={:?} output_sample_rate={:?}",
            d.sbr_support(),
            d.output_sample_rate()
        );

        // Hand-built EXPLICIT config: object_type=5 (SBR), known core/ext
        // rates, sbr_present forced true regardless of what the real ASC
        // parsed to.
        let ext = if ext_rate == 0 {
            parsed_cfg.extension_sample_rate.unwrap_or(core_rate * 2)
        } else {
            ext_rate
        };
        let hand_cfg = ec_aac::AudioSpecificConfig {
            object_type: ec_aac::AOT_SBR,
            sample_rate: core_rate,
            sf_index: core_sf_index,
            channels,
            channel_config: channels as u8,
            sbr_present: true,
            ps_present: false,
            extension_sample_rate: Some(ext),
            pce: Vec::new(),
        };
        let (hand_planes, hand_rate) = decode_with(hand_cfg.clone(), &aus);
        if hand_planes.is_empty() {
            eprintln!("  hand-built config: no channels decoded");
            continue;
        }
        // Own-core self-consistency (upsample+lowpass convention).
        let core_cfg = ec_aac::AudioSpecificConfig {
            sbr_present: false,
            ps_present: false,
            extension_sample_rate: None,
            pce: Vec::new(),
            ..hand_cfg.clone()
        };
        let (core_planes, _) = decode_with(core_cfg, &aus);
        if !core_planes.is_empty() {
            let up: Vec<f32> = core_planes[0].iter().flat_map(|&s| [s, s]).collect();
            let cutoff = f64::from(core_rate) * 0.4;
            let ol = lowpass(&hand_planes[0], hand_rate, cutoff);
            let ul = lowpass(&up, hand_rate, cutoff);
            let (lag, corr) = best_lag_correlation_wide(&ol, &ul, 100_000);
            println!(
                "  hand-built explicit config: low-band vs own core: lag={lag} corr={corr:.6}"
            );
        }
        let theirs = ffmpeg_decode(&path, ffmpeg_stream, hand_planes.len());
        if let (Some(o), Some(t)) = (hand_planes.first(), theirs.first()) {
            let (lag, corr) = best_lag_correlation_wide(o, t, 100_000);
            println!(
                "  hand-built explicit config: full-band vs reference: lag={lag} corr={corr:.6}"
            );
        }
    }

    // Probe B: FMJ's own (parsed) full-chain-vs-own-core lag search, widened
    // to 100_000 (5x the existing WIDE_LAG_MAX=20_000 this file's other
    // tests use) -- the coarse search in `full_chain_low_band_matches_own_
    // core` landed right at that bound's edge (20005), so this checks
    // whether FMJ's true delay simply lies further out.
    let home = std::env::var("HOME").unwrap_or_default();
    let fmj = PathBuf::from(format!(
        "{home}/Downloads/reelp/reelp.mkv"
    ));
    if fmj.exists()
        && let (Some((core, core_rate)), Some((full, sbr, full_rate))) =
            (our_decode_core_only(&fmj, 1), our_decode(&fmj, 1))
        && sbr == ec_aac::SbrSupport::V1
    {
        let up: Vec<f32> = core[0].iter().flat_map(|&s| [s, s]).collect();
        let cutoff = f64::from(core_rate) * 0.4;
        let ol = lowpass(&full[0], full_rate, cutoff);
        let ul = lowpass(&up, full_rate, cutoff);
        let (lag, corr) = best_lag_correlation_wide(&ol, &ul, 100_000);
        println!("FMJ WIDE_LAG_MAX=100_000: ch0 best lag={lag} corr={corr:.6}");
    }
}

/// One channel of `path`'s given absolute stream, isolated with an ffmpeg
/// `pan` filter rather than raw de-interleaving -- the charter's own witness
/// rule (see `ffmpeg_decode`'s doc and the ledger's "never use -ac 1", which
/// applies just as much to picking one of N channels as to downmixing).
pub(super) fn ffmpeg_decode_pan_channel(path: &Path, absolute_stream: usize, channel: usize) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            &format!("0:{absolute_stream}"),
            "-t",
            "10",
            "-af",
            &format!("pan=1c|c0=c{channel}"),
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
        "ffmpeg pan decode failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .as_chunks::<4>().0.iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
