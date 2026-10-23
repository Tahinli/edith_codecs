//! Shared analysis helpers for the SBR real-library probes.

use super::*;

pub(super) use crate::common::have_ffmpeg;
/// Fixture-presence probe for the generated HE-AAC matrix under
/// `<repo>/.cache/heaac-fixtures` (`scripts/aac-tables/make-heaac-fixtures.sh`).
/// Returns whether the fixture is present, but never silently: under
/// `EC_REQUIRE_FIXTURES=1` an absent fixture is a hard failure naming the path
/// and the script that regenerates it, so a host whose fixture library drifted
/// reports RED instead of a green SKIP (class: gate-skips-on-its-own-failure;
/// model `have_ffmpeg` in crates/ec-av1/src/stream.rs, which was silently
/// short-circuited because the probe ran first in a compound `if`).
///
/// Order is load-bearing: probe, assert, return. Never merge the probe into
/// the same `if` as the escape. The `~/Music` / `~/Downloads` / `~/Videos`
/// scans in this file are host corpora, not the committed fixture library, and
/// keep their plain skip.
pub(super) fn require_fixture(path: &Path, generator: &str) -> bool {
    let present = path.exists();
    assert!(
        present || std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
        "EC_REQUIRE_FIXTURES=1 but fixture {} is absent -- regenerate with: {}",
        path.display(),
        generator
    );
    if !present {
        eprintln!("SKIP: fixture {} absent", path.display());
    }
    present
}

/// A film's audio track is hours long; the correlation only needs a bounded
/// window near the start, so both sides cap how much they ever decode.
pub(super) const MAX_SAMPLES: usize = 1_000_000;

/// ffmpeg's decode of a file's given absolute stream index, as
/// planar-by-channel `f32`.
pub(super) fn ffmpeg_decode(path: &Path, absolute_stream: usize, channels: usize) -> Vec<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            &format!("0:{absolute_stream}"),
            "-t",
            "30",
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
        "ffmpeg decode failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let samples: Vec<f32> = out
        .stdout
        .as_chunks::<4>().0.iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let mut planes = vec![Vec::with_capacity(samples.len() / channels.max(1)); channels];
    for (i, &v) in samples.iter().enumerate() {
        planes[i % channels].push(v);
    }
    planes
}

/// One AAC stream pulled out of its container: the AudioSpecificConfig bytes
/// exactly as the container carried them, and its access units in decode
/// order. Whichever of `ec-mp4`/`ec-matroska` the file's header names does the
/// actual box/EBML walk -- this is a thin `Demuxer`-trait harness over them,
/// not a parser of its own.
pub(super) fn extract_aac_track(path: &Path, stream_index: usize) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    let mut head = [0u8; 12];
    File::open(path).ok()?.read_exact(&mut head).ok()?;
    if ec_mp4::is_mp4(&head) {
        let f = File::open(path).ok()?;
        let mut d = Mp4Demuxer::new(BufReader::new(f)).ok()?;
        extract_from(&mut d, stream_index)
    } else if ec_matroska::is_matroska(&head) {
        let f = File::open(path).ok()?;
        let mut d = MatroskaDemuxer::new(BufReader::new(f)).ok()?;
        extract_from(&mut d, stream_index)
    } else {
        None
    }
}

/// `stream_index` picks the Nth AAC stream the container lists, in listed
/// order -- a file can carry more than one audio track (FMJ's mkv has both an
/// LC 5.1 track and an HE-AAC stereo commentary track).
pub(super) fn extract_from(d: &mut dyn Demuxer, stream_index: usize) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    let aac = d
        .streams()
        .iter()
        .filter(|s| s.params.codec == CodecId::Aac)
        .nth(stream_index)?;
    let idx = aac.index;
    let asc = aac.params.extradata.as_ref()?.to_vec();
    let mut aus = Vec::new();
    // A film's audio track is a couple thousand AAC access units for the
    // handful of seconds the correlation actually needs; capped here so the
    // packet walk does not have to read the whole multi-gigabyte video
    // stream past it just to reach `Eof`.
    while aus.len() < 1_500 {
        match d.next_packet() {
            Ok(Packet { stream, data, .. }) if stream == idx => aus.push(data.to_vec()),
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    Some((asc, aus))
}

/// Our decode of one container-native AAC stream, through its real
/// AudioSpecificConfig, as planar-by-channel `f32`.
pub(super) fn our_decode(
    path: &Path,
    stream_index: usize,
) -> Option<(Vec<Vec<f32>>, ec_aac::SbrSupport, u32)> {
    let Some((asc, aus)) = extract_aac_track(path, stream_index) else {
        eprintln!("  extract_aac_track returned None");
        return None;
    };
    eprintln!("  asc={asc:02x?} aus={}", aus.len());
    let mut decoder = match AacDecoder::with_config_bytes(&asc) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("  with_config_bytes failed: {e:?}");
            return None;
        }
    };
    let sbr = decoder.sbr_support();
    let rate = decoder.output_sample_rate().unwrap_or(0);
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut failed = 0;
    let debug_failures = std::env::var("EC_AAC_SBR_AU_FAIL_DEBUG").as_deref() == Ok("1");
    for (au_idx, au) in aus.iter().enumerate() {
        let frame = match decoder.decode(au, None) {
            Ok(f) => f,
            Err(e) => {
                failed += 1;
                if debug_failures {
                    let head: Vec<u8> = au.iter().take(16).copied().collect();
                    eprintln!(
                        "  AU {au_idx} FAILED len={} err={e:?} head={head:02x?}",
                        au.len()
                    );
                }
                continue;
            }
        };
        let ch = usize::from(frame.channels);
        if ch == 0 {
            continue;
        }
        if planes.is_empty() {
            planes = vec![Vec::new(); ch];
        }
        for (i, v) in frame.samples.iter().enumerate() {
            planes[i % ch].push(*v);
        }
        if planes[0].len() >= MAX_SAMPLES {
            break;
        }
    }
    if failed > 0 {
        eprintln!("  {failed}/{} access units failed to decode", aus.len());
    }
    if planes.is_empty() {
        eprintln!("  no channels decoded");
        return None;
    }
    Some((planes, sbr, rate))
}

/// Search window bound: keeps the correlation sum bounded, per the charter
/// -- an unbounded version over a whole film's worth of samples was too slow
/// to be an every-run test.
pub(super) const WINDOW: usize = 200_000;

/// Wide lag search bound for the specific gate proven to need it
/// (`sbr_real_library_matches_reference`, ours-vs-reference across a real
/// container's remux/priming delay). Measured (round-54 wide-lag probe,
/// worktree sbr-per-au) FMJ's true full-chain delay vs the reference is
/// ~22,913 samples @48kHz -- that gate's old `LAG_MAX=4_000` search clipped
/// at its own edge and silently reported the edge's noise as the answer for
/// 50+ rounds (class: "an alignment instrument whose result sits at its own
/// search edge is invalid"). 100_000 covers that with 4x headroom and is
/// already the bound round-53's own `WIDE_LAG_MAX=100_000` probe used to
/// find it. NOT used as the default everywhere: a wider bound also widens
/// the coarse pass's exposure to spurious short-slice noise peaks, so
/// call sites whose true delay is known-small (container/decoder priming,
/// ours-vs-own-core self-consistency) keep their original, narrower bound
/// -- round-54 first tried blanket-widening every call site to this value
/// and it flipped `full_chain_low_band_matches_own_core` and the LC control
/// row of `synthetic_heaac_matrix` from a correct small-lag answer to a
/// wrong, noise-driven distant one, still comfortably inside the bound
/// (not `at_edge`) but wrong regardless.
pub(super) const SEARCH_LAG_MAX: i64 = 100_000;
/// Original bound for the plain `best_lag_correlation` wrapper, used by the
/// file's other (ours-vs-reference small-priming-delay, or ours-vs-ours)
/// callers that were never shown to need more.
pub(super) const PLAIN_LAG_MAX: i64 = 4_000;
/// Odd stride `best_lag_correlation_ex`'s coarse pass steps by, so it
/// doesn't alias against frame-length periodicity.
pub(super) const SEARCH_STRIDE: i64 = 11;

pub(super) fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum()
}

pub(super) fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n < 1024 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let ma = a.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let mb = b.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let ac: Vec<f32> = a.iter().map(|v| (f64::from(*v) - ma) as f32).collect();
    let bc: Vec<f32> = b.iter().map(|v| (f64::from(*v) - mb) as f32).collect();
    let num = dot(&ac, &bc);
    let den = (dot(&ac, &ac) * dot(&bc, &bc)).sqrt();
    if den == 0.0 { 0.0 } else { num / den }
}

/// Best correlation over a bounded lag search, PLUS whether the winning lag
/// sits at the search's own edge -- the single instrument every asserting
/// test in this file now routes through (class fix, round-54: a search that
/// silently clips at its bound and reports the edge's noise as "the
/// answer" is not an instrument, it's a lie). Two passes keep the whole
/// search O(reasonable) even at `SEARCH_LAG_MAX`-sized bounds on a
/// film-length file: an odd-`SEARCH_STRIDE` coarse pass over a short slice
/// finds roughly where the alignment is (the stride, not full 1-by-1
/// stepping, is what keeps a `lag_max` in the tens of thousands cheap, and
/// also avoids aliasing against frame-length periodicity), then one
/// full-`WINDOW` correlation at that lag (and its immediate neighbours, in
/// case the coarse slice picked a noisy lag) gives the reported number.
pub(super) fn best_lag_correlation_ex(ours: &[f32], theirs: &[f32], lag_max: i64) -> (i64, f64, bool) {
    const COARSE: usize = 4_096;
    let start = ours
        .len()
        .min(theirs.len())
        .saturating_sub(WINDOW)
        .min(ours.len() / 4);
    let slice_at = |lag: i64, len: usize| -> Option<(usize, usize)> {
        let (oa, ob) = if lag >= 0 {
            (start, start + lag as usize)
        } else {
            (start + (-lag) as usize, start)
        };
        if oa + len > ours.len() || ob + len > theirs.len() {
            None
        } else {
            Some((oa, ob))
        }
    };
    // The winning lag is the one that maximizes |correlation|, not signed
    // correlation: a channel that is a pure phase/sign inversion of the
    // reference reads strongly NEGATIVE at its true alignment lag, and a
    // signed-max search rejects that lag for whatever unrelated lag gives a
    // merely positive value -- reporting a real inversion as near-zero
    // noise. `found` (not "best abs value >= 0", which every real
    // correlation already satisfies) is what actually distinguishes "no
    // in-bounds lag existed" from "the best in-bounds lag was negative" --
    // an earlier version used `best.1 < 0.0` for that check, which treats
    // every genuinely negative (inverted) result as "not found" and falls
    // back to the COARSE, `COARSE`-sample-window-only candidate instead;
    // that coarse window is short enough to spuriously correlate at 0.87 on
    // real audio, so the fallback was silently replacing an honest,
    // correctly-refined negative full-`WINDOW` result with a noise-driven
    // positive one from a 20x-shorter slice. Carry the signed value through
    // so callers can see the sign, not just |corr|.
    // Stride 1, not `SEARCH_STRIDE` (round-61): a real correlation peak
    // between two decodes of the same audio is often ONE sample wide (on
    // reeln's core-only PCM: 0.9998 at lag 481, 0.94 at 479), so a
    // strided grid samples only the noise floor around it and the "best
    // coarse lag" it hands the refine pass is arbitrary. That is how a
    // 0.9998 match printed as corr 0.408 at lag -52. Stride 1 over the
    // narrow bound is ~33M multiply-adds; the wide bound is 25x that and
    // only runs on escalation, so this is not worth a resolution trade.
    let mut coarse_best = (0i64, -1.0f64, 0.0f64);
    for lag in -lag_max..=lag_max {
        if let Some((oa, ob)) = slice_at(lag, COARSE) {
            let c = correlation(&ours[oa..oa + COARSE], &theirs[ob..ob + COARSE]);
            if c.abs() > coarse_best.1 {
                coarse_best = (lag, c.abs(), c);
            }
        }
    }
    let at_edge = coarse_best.0.abs() >= lag_max - SEARCH_STRIDE;
    let mut best = (coarse_best.0, -1.0f64, 0.0f64);
    let mut found = false;
    for lag in (coarse_best.0 - 2 * SEARCH_STRIDE)..=(coarse_best.0 + 2 * SEARCH_STRIDE) {
        let Some((oa, ob)) = slice_at(lag, WINDOW) else {
            continue;
        };
        let n = WINDOW.min(ours.len() - oa).min(theirs.len() - ob);
        if n < 1024 {
            continue;
        }
        let c = correlation(&ours[oa..oa + n], &theirs[ob..ob + n]);
        if c.abs() > best.1 {
            best = (lag, c.abs(), c);
        }
        found = true;
    }
    if !found {
        best = coarse_best;
    }
    // A winning lag at the search bound is not a measurement (the true
    // alignment may lie outside it): widen once to `SEARCH_LAG_MAX`, and if
    // it still sits at the edge fail here rather than hand any caller a
    // noise-driven number (instrument-at-bound class).
    if at_edge && lag_max < SEARCH_LAG_MAX {
        return best_lag_correlation_ex(ours, theirs, SEARCH_LAG_MAX);
    }
    assert!(
        !at_edge,
        "winning lag {} sits at the +/-{lag_max} search bound -- the true \
         alignment is outside the search, corr {:.6} is noise, not a measurement",
        best.0, best.2
    );
    (best.0, best.2, at_edge)
}

/// Thin wrapper over `best_lag_correlation_ex` at `PLAIN_LAG_MAX`, for the
/// many non-asserting call sites that only want `(lag, corr)` and whose true
/// delay was never shown to exceed the original 4_000 bound. The one gate
/// proven to need the wide bound (`sbr_real_library_matches_reference`)
/// calls `_ex` directly with `SEARCH_LAG_MAX` instead of going through this
/// wrapper.
pub(super) fn best_lag_correlation(ours: &[f32], theirs: &[f32]) -> (i64, f64) {
    let (lag, corr, _) = best_lag_correlation_ex(ours, theirs, PLAIN_LAG_MAX);
    (lag, corr)
}

/// Thin wrapper over `best_lag_correlation_ex` at a caller-supplied bound,
/// for non-asserting call sites (asserting ones use `_ex` directly).
pub(super) fn best_lag_correlation_wide(ours: &[f32], theirs: &[f32], lag_max: i64) -> (i64, f64) {
    let (lag, corr, _) = best_lag_correlation_ex(ours, theirs, lag_max);
    (lag, corr)
}

/// The robust two-stage lag search `sbr441_family_sample_drift_probe` proved
/// out (round-55/56): a plain coarse-stride search picks the SINGLE best
/// `COARSE`-sample-window candidate, which on periodic/tonal real music
/// content routinely aliases onto a wrong lag (this file family's ref-L-vs-
/// ref-R self-check scored a coarse-search lag=-24 corr=0.213, worse than
/// the true fixed-lag comparison's 0.873). Instead of trusting the single
/// coarse winner, keep the top `K` coarse candidates and refine EACH with a
/// full-resolution, narrow (+/-128) search restricted to the file's first
/// 10 seconds (`sample_rate`-derived) -- periodic aliasing needs many
/// seconds of a repeating pattern to fool a full-resolution window, so the
/// candidate that is genuinely aligned wins there even when it wasn't the
/// coarse pass's top pick. Returns `(lag, corr, at_edge)` for the winning
/// refined candidate, `at_edge` inherited from that candidate's own coarse
/// Stride-1 lag search: correlates a `COARSE`-sized window at EVERY lag in
/// `+/-lag_max`, then refines the winner with a full `WINDOW`.
///
/// `robust_lag_topk`'s coarse pass steps by `SEARCH_STRIDE` (11) and then
/// refines only its top-K coarse scorers. That ranking is meaningless when
/// the true peak is narrower than the stride: on reeln's core-only PCM
/// the correlation is 0.9998 at lag 481 and already 0.94 at 479, so no
/// stride-11 grid point (the grid is 4 mod 11; 481 is 8 mod 11) scores
/// anything but noise, the top-K is drawn from noise, and the search
/// returned lag -52 corr 0.408 for a pair that is actually a 0.9998 match.
/// FMJ, same instrument, same true lag 481, happened to land a noise-ranked
/// candidate within the +/-128 refine window and read correctly -- the
/// failure is luck-dependent, not file-dependent. Stride 1 over a 4_096
/// window is ~33M multiply-adds at `PLAIN_LAG_MAX`, cheaper than the top-K
/// refinement it replaces, so the resolution is simply not worth trading.
pub(super) fn exhaustive_lag_correlation(ours: &[f32], theirs: &[f32], lag_max: i64) -> (i64, f64, bool) {
    const COARSE: usize = 4_096;
    let start = ours
        .len()
        .min(theirs.len())
        .saturating_sub(WINDOW)
        .min(ours.len() / 4);
    let slice_at = |lag: i64, len: usize| -> Option<(usize, usize)> {
        let (oa, ob) = if lag >= 0 {
            (start, start + lag as usize)
        } else {
            (start + (-lag) as usize, start)
        };
        if oa + len > ours.len() || ob + len > theirs.len() {
            None
        } else {
            Some((oa, ob))
        }
    };
    let mut best = (0i64, -1.0f64);
    for lag in -lag_max..=lag_max {
        if let Some((oa, ob)) = slice_at(lag, COARSE) {
            let c = correlation(&ours[oa..oa + COARSE], &theirs[ob..ob + COARSE]).abs();
            if c > best.1 {
                best = (lag, c);
            }
        }
    }
    let at_edge = best.0.abs() >= lag_max - 1;
    let mut refined = (best.0, -1.0f64);
    for lag in (best.0 - 2)..=(best.0 + 2) {
        let Some((oa, ob)) = slice_at(lag, WINDOW) else {
            continue;
        };
        let n = WINDOW.min(ours.len() - oa).min(theirs.len() - ob);
        if n < 1024 {
            continue;
        }
        let c = correlation(&ours[oa..oa + n], &theirs[ob..ob + n]).abs();
        if c > refined.1 {
            refined = (lag, c);
        }
    }
    if refined.1 < 0.0 {
        refined = best;
    }
    (refined.0, refined.1, at_edge)
}

/// bucket (true if ITS coarse lag sat at the +/-`lag_max` search bound).
pub(super) fn robust_lag_topk(
    ours: &[f32],
    theirs: &[f32],
    lag_max: i64,
    rate: u32,
    k: usize,
) -> (i64, f64, bool) {
    const COARSE: usize = 4_096;
    let start = ours
        .len()
        .min(theirs.len())
        .saturating_sub(WINDOW)
        .min(ours.len() / 4);
    let slice_at = |lag: i64, len: usize| -> Option<(usize, usize)> {
        let (oa, ob) = if lag >= 0 {
            (start, start + lag as usize)
        } else {
            (start + (-lag) as usize, start)
        };
        if oa + len > ours.len() || ob + len > theirs.len() {
            None
        } else {
            Some((oa, ob))
        }
    };
    // Stride 1 (round-61), for the reason spelled out on
    // `exhaustive_lag_correlation`: a strided grid can miss a 1-sample-wide
    // peak entirely, and then the top-K is a ranking of noise.
    let mut coarse: Vec<(i64, f64)> = Vec::new();
    for lag in -lag_max..=lag_max {
        if let Some((oa, ob)) = slice_at(lag, COARSE) {
            let c = correlation(&ours[oa..oa + COARSE], &theirs[ob..ob + COARSE]);
            coarse.push((lag, c.abs()));
        }
    }
    coarse.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let ten_s = 10 * rate.max(1) as usize;
    let mut best: (i64, f64, bool) = (0, -2.0, false);
    for &(cand_lag, _) in coarse.iter().take(k) {
        let at_edge = cand_lag.abs() >= lag_max - SEARCH_STRIDE;
        // `narrow_lag_at` computes `ob_i = oa0 + lag` directly (no sign
        // split the way `slice_at` above does), so a fixed `oa0 = 0` goes
        // negative -- and every lag in the whole +/-128 window skips --
        // whenever `cand_lag` is negative enough (this file family's true
        // lag is -4674). Shift the window's start so it stays anchored to
        // "the first 10s of real content" while guaranteeing `ob_i >= 0`
        // even at the most-negative lag in the +/-128 refine range.
        let oa0 = (128 - cand_lag).max(0) as usize;
        let refine_len = ten_s.min(ours.len().saturating_sub(oa0)).min(
            theirs
                .len()
                .saturating_sub((oa0 as i64 + cand_lag).max(0) as usize),
        );
        if refine_len < 1024 {
            continue;
        }
        let (rlag, rcorr) = narrow_lag_at(ours, theirs, oa0, refine_len, cand_lag, 128);
        if rcorr > best.1 {
            best = (rlag, rcorr, at_edge);
        }
    }
    best
}

/// Signed correlation of `ours` against `theirs` at one fixed `lag`, full
/// `WINDOW`-sized, anchored at the SAME `start` offset `best_lag_correlation`
/// uses internally -- used to test a hypothesized sign inversion by reusing
/// a KNOWN-good lag (and window) from a healthy channel instead of trusting
/// the (signed, positive-biased) search above to relocate both for a
/// channel that may be exactly inverted at that same lag. Anchoring must
/// match `best_lag_correlation`'s `start`, not sample 0 -- the two windows
/// otherwise cover different seconds of audio and are not comparable.
pub(super) fn correlation_at_lag(ours: &[f32], theirs: &[f32], lag: i64) -> f64 {
    let start = ours
        .len()
        .min(theirs.len())
        .saturating_sub(WINDOW)
        .min(ours.len() / 4);
    let (oa, ob) = if lag >= 0 {
        (start, start + lag as usize)
    } else {
        (start + (-lag) as usize, start)
    };
    let n = WINDOW
        .min(ours.len().saturating_sub(oa))
        .min(theirs.len().saturating_sub(ob));
    if n < 1024 {
        return 0.0;
    }
    correlation(&ours[oa..oa + n], &theirs[ob..ob + n])
}

/// A first-order RC low-pass, run forward then backward to cancel its own
/// phase shift, giving a clean low-band/high-band split without pulling in a
/// filter design dependency for a diagnostic.
pub(super) fn lowpass(samples: &[f32], rate: u32, cutoff_hz: f64) -> Vec<f32> {
    let dt = 1.0 / f64::from(rate);
    let rc = 1.0 / (2.0 * std::f64::consts::PI * cutoff_hz);
    let alpha = (dt / (rc + dt)) as f32;
    let mut fwd = Vec::with_capacity(samples.len());
    let mut y = 0.0f32;
    for &x in samples {
        y += alpha * (x - y);
        fwd.push(y);
    }
    let mut out = vec![0.0f32; fwd.len()];
    y = 0.0;
    for i in (0..fwd.len()).rev() {
        y += alpha * (fwd[i] - y);
        out[i] = y;
    }
    out
}

pub(super) fn highpass(samples: &[f32], rate: u32, cutoff_hz: f64) -> Vec<f32> {
    let low = lowpass(samples, rate, cutoff_hz);
    samples.iter().zip(&low).map(|(a, b)| a - b).collect()
}

/// DIAGNOSTIC (round-13): cascades `lowpass` three times for a much sharper
/// stopband than the single first-order zero-phase filter the real
/// assertion uses -- checking whether the below-crossover bar is actually
/// being missed because of above-crossover energy leaking through the
/// gentle single-pass rolloff, not because the low band itself is wrong.
pub(super) fn sharp_lowpass(samples: &[f32], rate: u32, cutoff_hz: f64) -> Vec<f32> {
    let a = lowpass(samples, rate, cutoff_hz);
    let b = lowpass(&a, rate, cutoff_hz);
    lowpass(&b, rate, cutoff_hz)
}

/// Decisive-experiment diagnostic: correlates consecutive short (`WIN`)
/// windows of `o` against `t` (already coarsely aligned at `oa`/`ob`, e.g.
/// the winning full-file lag from `best_lag_correlation`), searching a small
/// local lag range per window. A per-frame continuity bug (dropped/duplicated
/// samples at access-unit boundaries) shows up as the winning local lag
/// DRIFTING linearly window to window; a bug that is not about concatenation
/// (wrong band mapping, spectral corruption) shows up as a roughly constant
/// lag with low/noisy correlation instead.
pub(super) fn windowed_lag_drift(
    o: &[f32],
    t: &[f32],
    oa: usize,
    ob: usize,
    windows: usize,
) -> Vec<(i64, f64)> {
    const WIN: usize = 4_096;
    const LOCAL_LAG: i64 = 2_000;
    let mut out = Vec::with_capacity(windows);
    for w in 0..windows {
        let base_o = oa + w * WIN;
        let base_t = ob + w * WIN;
        let mut best = (0i64, -1.0f64, 0.0f64);
        for lag in -LOCAL_LAG..=LOCAL_LAG {
            let ao = base_o as i64 + lag;
            if ao < 0 {
                continue;
            }
            let ao = ao as usize;
            if ao + WIN > o.len() || base_t + WIN > t.len() {
                continue;
            }
            let c = correlation(&o[ao..ao + WIN], &t[base_t..base_t + WIN]);
            if c.abs() > best.1 {
                best = (lag, c.abs(), c);
            }
        }
        out.push((best.0, best.2));
    }
    out
}

/// DIAGNOSTIC (round-14, Task 2): per-QMF-band correlation over the
/// above-crossover region. No 64-band QMF Analysis exists standalone in this
/// crate, so this substitutes an STFT: magnitude-per-bin time series (one
/// value per hop) for `ours` and `theirs`, correlated bin by bin, then
/// averaged over the bins a QMF band of width `rate/128` Hz covers. A patch
/// boundary in `build_patches`' target ranges landing on a low-correlation
/// block convicts patch construction; an even spread across all HF bands
/// with the map otherwise right points at gain mapping instead.
/// Pearson correlation with no minimum-length floor -- `correlation`'s
/// 1024-sample floor exists for raw audio windows, not the much shorter
/// per-bin magnitude series this diagnostic correlates.
pub(super) fn magnitude_series_correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let ma = a.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let mb = b.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let ac: Vec<f32> = a.iter().map(|v| (f64::from(*v) - ma) as f32).collect();
    let bc: Vec<f32> = b.iter().map(|v| (f64::from(*v) - mb) as f32).collect();
    let num = dot(&ac, &bc);
    let den = (dot(&ac, &ac) * dot(&bc, &bc)).sqrt();
    if den == 0.0 { 0.0 } else { num / den }
}

pub(super) fn per_band_correlation(o: &[f32], t: &[f32], rate: u32) -> Vec<(f64, f64, f64)> {
    const FFT_LEN: usize = 2048;
    const HOP: usize = 1024;
    let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
    let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
    let bins = rfft.spectrum_len();
    let n = o.len().min(t.len());
    let hops = if n >= FFT_LEN {
        (n - FFT_LEN) / HOP + 1
    } else {
        0
    };
    let mut o_mag = vec![Vec::with_capacity(hops); bins];
    let mut t_mag = vec![Vec::with_capacity(hops); bins];
    let mut spectrum = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
    for (src, dst) in [(o, &mut o_mag), (t, &mut t_mag)] {
        for h in 0..hops {
            let mut block = src[h * HOP..h * HOP + FFT_LEN].to_vec();
            win.apply(&mut block);
            rfft.forward(&block, &mut spectrum);
            for (b, c) in spectrum.iter().enumerate() {
                dst[b].push(c.norm_sqr().sqrt());
            }
        }
    }
    // QMF band width is `core_rate/128` where `core_rate` is the core
    // decoder's own sample rate -- half the SBR extension/output rate for a
    // plain (2x) HE-AAC stream -- not `output_rate/128`: measured against
    // this file's own kx=14 crossover, the correlation break lands exactly
    // at 14*(rate/256) Hz (2412 Hz), not at 14*(rate/128).
    let band_hz = f64::from(rate) / 256.0;
    let bin_hz = f64::from(rate) / FFT_LEN as f64;
    let num_bands = (f64::from(rate) / 2.0 / band_hz).ceil() as usize;
    let mut out = Vec::with_capacity(num_bands);
    for band in 0..num_bands {
        let lo = (band as f64 * band_hz / bin_hz).floor() as usize;
        let hi = (((band + 1) as f64 * band_hz / bin_hz).ceil() as usize).min(bins);
        if lo >= hi {
            continue;
        }
        // round-16: correlating bin-by-bin then averaging the |corr| values
        // was the bug -- a QMF band's bins share one slowly-varying envelope,
        // but a tiny (sub-Hz) frequency offset between our filterbank and the
        // reference's shifts energy between adjacent bins hop to hop, so each
        // bin's OWN magnitude series decorrelates even though the band's
        // total energy (what a QMF band's single subband signal actually
        // carries) tracks the reference closely. Summing the bins' magnitudes
        // into one band-energy series per side FIRST, then correlating that
        // single pair, is robust to that intra-band leakage the way the
        // per-bin version wasn't.
        let hops_here = o_mag[lo].len().min(t_mag[lo].len());
        let mut o_band = vec![0.0f32; hops_here];
        let mut t_band = vec![0.0f32; hops_here];
        for b in lo..hi {
            for h in 0..hops_here {
                o_band[h] += o_mag[b][h];
                t_band[h] += t_mag[b][h];
            }
        }
        let c = magnitude_series_correlation(&o_band, &t_band);
        if c.is_finite() {
            out.push((band as f64 * band_hz, c.abs(), band as f64));
        }
    }
    out
}

/// Per-band PCM-domain lag search (round-15 candidate-A discriminator):
/// `per_band_correlation`'s magnitude series has one sample per 1024-sample
/// STFT `HOP`, far coarser than a single QMF slot (64 samples) -- a stage
/// delay of one or a few slots between our HF path and the reference's would
/// vanish into that hop bucket instead of showing up as a lag. This instead
/// bandpasses `o`/`t` to each HF QMF band (the same zero-phase RC
/// `highpass`/`lowpass` primitives the crossover split already uses) and
/// searches PCM-sample lag directly, coarse-then-refine like
/// `best_lag_correlation`, so a consistent nonzero peak lag (especially a
/// multiple of 64 samples) convicts a per-stage time-alignment bug; lags
/// clustered at/near zero exonerate it in favour of a patch-map or
/// gain-mapping bug instead. Bands below kx (already known clean from
/// `per_band_correlation`) are skipped -- only HF bands (14..) are searched.
///
/// CONVICTED (round-26, Task 1): `bandpass_offset_probe` correlated this same
/// `sharp_lowpass`/`sharp_highpass` cascade's band-k output against the
/// REFERENCE's band-(k+/-10) output for several HF `k` -- and the DOWNWARD
/// offset (band k-10, closer to the strongly-correlated below-crossover
/// content) read a HIGHER correlation than the nominal same-band pairing in
/// every HF band tried (e.g. band 34: same 0.937 vs -10 0.966). A filter
/// with real ~172Hz selectivity cannot produce that; a "sharp" 3x-cascaded
/// single-pole RC (~18dB/oct) at a 9kHz center still passes a wide swath
/// below its corner, so each "band k" reading here is really "everything up
/// to and including band k" -- cumulative correlation, dominated by the
/// genuinely-matching low band, which manufactures the smooth monotonic
/// decline earlier rounds chased. The reported LAG values are still valid
/// (a filter with no stopband selectivity still has a passband delay that
/// tracks the true alignment), but the reported per-band |corr| AMPLITUDE
/// here is NOT a trustworthy per-band figure -- use `per_band_phase`'s
/// cross-spectrum magnitude ratio (or the bin-level `bin_level_conviction`)
/// for that instead.
pub(super) fn per_band_lag_search(o: &[f32], t: &[f32], rate: u32) -> Vec<(f64, i64, f64)> {
    const LAG_MAX: i64 = 1_024;
    const COARSE_STEP: i64 = 8;
    const COARSE: usize = 4_096;
    const WIN: usize = 65_536;
    let band_hz = f64::from(rate) / 256.0;
    let num_bands = (f64::from(rate) / 2.0 / band_hz).ceil() as usize;
    let n = o.len().min(t.len());
    let start = n.saturating_sub(WIN + LAG_MAX as usize).min(n / 4);
    let mut out = Vec::new();
    for band in 14..num_bands {
        let lo = (band as f64 * band_hz).max(1.0);
        let hi = ((band + 1) as f64 * band_hz).min(f64::from(rate) / 2.0 - 1.0);
        // A single-pole RC (the plain `highpass`/`lowpass` used for the
        // crossover split) has too gentle a rolloff to isolate a ~172Hz-wide
        // QMF band -- it leaks in the much stronger, well-correlated
        // below-crossover energy, which was confirmed here as a smooth,
        // frequency-monotonic false-high correlation with no matching cliff
        // in `per_band_correlation`'s FFT-bin version of the same bands.
        // `sharp_lowpass` (3x cascade, already used for the crossover-band
        // debug check) plus its highpass complement sharpens the stopband
        // enough that leakage from outside this one band stops dominating.
        let sharp_highpass = |s: &[f32], cutoff: f64| -> Vec<f32> {
            s.iter()
                .zip(&sharp_lowpass(s, rate, cutoff))
                .map(|(a, b)| a - b)
                .collect::<Vec<f32>>()
        };
        let bp = |s: &[f32]| -> Vec<f32> { sharp_lowpass(&sharp_highpass(s, lo), rate, hi) };
        let ob = bp(o);
        let tb = bp(t);
        let slice_at = |lag: i64, len: usize| -> Option<(usize, usize)> {
            let (oa, ta) = if lag >= 0 {
                (start, start + lag as usize)
            } else {
                (start + (-lag) as usize, start)
            };
            if oa + len > ob.len() || ta + len > tb.len() {
                None
            } else {
                Some((oa, ta))
            }
        };
        let mut coarse = (0i64, -1.0f64, 0.0f64);
        let mut lag = -LAG_MAX;
        while lag <= LAG_MAX {
            if let Some((oa, ta)) = slice_at(lag, COARSE) {
                let c = correlation(&ob[oa..oa + COARSE], &tb[ta..ta + COARSE]);
                if c.abs() > coarse.1 {
                    coarse = (lag, c.abs(), c);
                }
            }
            lag += COARSE_STEP;
        }
        let mut best = coarse;
        for lag in (coarse.0 - COARSE_STEP)..=(coarse.0 + COARSE_STEP) {
            if let Some((oa, ta)) = slice_at(lag, WIN) {
                let c = correlation(&ob[oa..oa + WIN], &tb[ta..ta + WIN]);
                if c.abs() > best.1 {
                    best = (lag, c.abs(), c);
                }
            }
        }
        out.push((band as f64 * band_hz, best.0, best.2));
    }
    out
}

/// DIAGNOSTIC (round-16, Task 2): per-band phase offset and magnitude
/// transfer between `o` and `t`, over the SAME aligned window
/// `per_band_correlation` uses. Reuses the STFT-and-coherent-band-sum
/// machinery `per_band_correlation` was just fixed to use (sum the complex
/// spectrum, not just its magnitude, across a band's bins first, so a
/// per-bin sub-band redistribution doesn't wash out a genuine per-band
/// phase relationship) but keeps the complex value: the cross-spectrum
/// `sum_h O_band(h) * conj(T_band(h))` accumulated over every hop gives one
/// phasor per band whose angle is the (hop-weighted) phase offset between
/// `o` and `t` in that band and whose radius, once normalized by each side's
/// own energy, gives the magnitude transfer ratio.
pub(super) fn per_band_phase(o: &[f32], t: &[f32], rate: u32) -> Vec<(f64, f64, f64)> {
    const FFT_LEN: usize = 2048;
    const HOP: usize = 1024;
    let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
    let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
    let bins = rfft.spectrum_len();
    let n = o.len().min(t.len());
    let hops = if n >= FFT_LEN {
        (n - FFT_LEN) / HOP + 1
    } else {
        0
    };
    let mut o_spec = vec![Vec::with_capacity(hops); bins];
    let mut t_spec = vec![Vec::with_capacity(hops); bins];
    let mut spectrum = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
    for (src, dst) in [(o, &mut o_spec), (t, &mut t_spec)] {
        for h in 0..hops {
            let mut block = src[h * HOP..h * HOP + FFT_LEN].to_vec();
            win.apply(&mut block);
            rfft.forward(&block, &mut spectrum);
            for (b, c) in spectrum.iter().enumerate() {
                dst[b].push(*c);
            }
        }
    }
    let band_hz = f64::from(rate) / 256.0;
    let bin_hz = f64::from(rate) / FFT_LEN as f64;
    let num_bands = (f64::from(rate) / 2.0 / band_hz).ceil() as usize;
    let mut out = Vec::with_capacity(num_bands);
    for band in 0..num_bands {
        let lo = (band as f64 * band_hz / bin_hz).floor() as usize;
        let hi = (((band + 1) as f64 * band_hz / bin_hz).ceil() as usize).min(bins);
        if lo >= hi || hops == 0 {
            continue;
        }
        // Cross-spectrum accumulated PER BIN over hops, THEN summed across
        // the band's bins -- not the reverse. Complex-summing the band's
        // bins together at each hop first (as an earlier version of this
        // function did) mixes in `O_b * conj(T_b')` cross terms for every
        // b != b' pair once the per-hop products are formed; two adjacent
        // FFT bins carry unrelated instantaneous phase (they are different
        // frequencies), so those cross terms are pure noise that swamped the
        // real per-bin coherence and produced an incoherent, near-random
        // phase reading across exactly the HF bands `per_band_lag_search`'s
        // direct PCM correlation shows are highly correlated at lag 0/-1.
        let mut cross_re = 0.0f64;
        let mut cross_im = 0.0f64;
        let mut o_energy = 0.0f64;
        let mut t_energy = 0.0f64;
        for b in lo..hi {
            for h in 0..hops {
                let ob = o_spec[b][h];
                let tb = t_spec[b][h];
                let cross = ob * tb.conj();
                cross_re += f64::from(cross.re);
                cross_im += f64::from(cross.im);
                o_energy +=
                    f64::from(ob.re) * f64::from(ob.re) + f64::from(ob.im) * f64::from(ob.im);
                t_energy +=
                    f64::from(tb.re) * f64::from(tb.re) + f64::from(tb.im) * f64::from(tb.im);
            }
        }
        let phase = cross_im.atan2(cross_re);
        let mag_ratio = if t_energy > 0.0 {
            (o_energy / t_energy).sqrt()
        } else {
            0.0
        };
        out.push((band as f64 * band_hz, phase, mag_ratio));
    }
    out
}

/// Round-26, Task 1: convicts (or acquits) `per_band_lag_search`'s bandpass.
/// Applies the SAME 3x-cascaded RC bandpass that diagnostic uses to `o` at
/// band `band` and to `t` at band `band` (same-band control), `band+offset`
/// and `band-offset`. A single-pole cascade's stopband is gentle enough that
/// "band k" may really pass "everything below f_k" -- if so, `o`-band-k and
/// `t`-band-(k+offset) still both carry the same dominant low-band leakage
/// and read highly correlated even though their nominal passbands don't
/// overlap at all, which convicts the filter, not the content. Below the SBR
/// crossover, genuinely narrowband-matching content should decorrelate
/// quickly as `offset` grows; a HF band that does NOT decorrelate at
/// `offset` bands away is evidence the filter has no real selectivity there.
pub(super) fn bandpass_offset_probe(
    o: &[f32],
    t: &[f32],
    rate: u32,
    band: usize,
    offset: i64,
) -> (f64, f64, f64) {
    let band_hz = f64::from(rate) / 256.0;
    let bp_at = |s: &[f32], b: usize| -> Vec<f32> {
        let lo = (b as f64 * band_hz).max(1.0);
        let hi = ((b + 1) as f64 * band_hz).min(f64::from(rate) / 2.0 - 1.0);
        let sharp_highpass = |s: &[f32], cutoff: f64| -> Vec<f32> {
            s.iter()
                .zip(&sharp_lowpass(s, rate, cutoff))
                .map(|(a, b)| a - b)
                .collect::<Vec<f32>>()
        };
        sharp_lowpass(&sharp_highpass(s, lo), rate, hi)
    };
    let ob = bp_at(o, band);
    let same_corr = correlation(&ob, &bp_at(t, band));
    let hi_band = band as i64 + offset;
    let hi_corr = if hi_band >= 0 {
        correlation(&ob, &bp_at(t, hi_band as usize))
    } else {
        0.0
    };
    let lo_band = band as i64 - offset;
    let lo_corr = if lo_band >= 0 {
        correlation(&ob, &bp_at(t, lo_band as usize))
    } else {
        0.0
    };
    (same_corr, hi_corr, lo_corr)
}

/// Round-26, Task 2: bin-level (no filter, no cross-band summing) comparison
/// of `o` against `t` in the most energetic HF SBR bands at/above `min_band`,
/// three ways: DIRECT (bin i vs bin i), MIRROR (bin i vs the band's bins in
/// REVERSED order -- the signature of a QMF band-orientation/parity flip
/// between our own-design synthesis and the reference's convention: energies
/// right, direct corr ~0, phase random, exactly what round-14/16 observed),
/// and SHIFT (the whole `BINS_PER_SBR_BAND`-bin band matched one band up or
/// down -- the signature of a `build_patches` source-mapping offset).
/// Coherence per bin pair is
/// `|sum_h O(h)*conj(T(h))| / sqrt(sum_h|O(h)|^2 * sum_h|T(h)|^2)` (in
/// [0,1]; 1 = perfectly phase+magnitude locked, ~0 = unrelated), accumulated
/// over the SAME aligned hops `per_band_phase` uses, then averaged over the
/// band's 8 bins. Bands are picked by OUR OWN energy (the "real" HF content),
/// not the reference's, so a genuinely-silent HF band can't crowd out an
/// energetic one just because it happens to sort first.
pub(super) fn bin_level_conviction(
    o: &[f32],
    t: &[f32],
    rate: u32,
    min_band: usize,
    top_n: usize,
) -> Vec<(f64, f64, f64, f64, f64, f64)> {
    const FFT_LEN: usize = 2048;
    const HOP: usize = 1024;
    const BINS_PER_SBR_BAND: usize = FFT_LEN / 256;
    let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
    let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
    let bins = rfft.spectrum_len();
    let n = o.len().min(t.len());
    let hops = if n >= FFT_LEN {
        (n - FFT_LEN) / HOP + 1
    } else {
        0
    };
    let mut o_spec = vec![Vec::with_capacity(hops); bins];
    let mut t_spec = vec![Vec::with_capacity(hops); bins];
    let mut spectrum = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
    for (src, dst) in [(o, &mut o_spec), (t, &mut t_spec)] {
        for h in 0..hops {
            let mut block = src[h * HOP..h * HOP + FFT_LEN].to_vec();
            win.apply(&mut block);
            rfft.forward(&block, &mut spectrum);
            for (b, c) in spectrum.iter().enumerate() {
                dst[b].push(*c);
            }
        }
    }
    if hops == 0 {
        return Vec::new();
    }
    let band_hz = f64::from(rate) / 256.0;
    let num_bands = bins / BINS_PER_SBR_BAND;
    let coh = |ob: usize, tb: usize| -> f64 {
        if ob >= bins || tb >= bins {
            return 0.0;
        }
        let mut cross_re = 0.0f64;
        let mut cross_im = 0.0f64;
        let mut o_e = 0.0f64;
        let mut t_e = 0.0f64;
        for h in 0..hops {
            let oc = o_spec[ob][h];
            let tc = t_spec[tb][h];
            let cross = oc * tc.conj();
            cross_re += f64::from(cross.re);
            cross_im += f64::from(cross.im);
            o_e += f64::from(oc.norm_sqr());
            t_e += f64::from(tc.norm_sqr());
        }
        let den = (o_e * t_e).sqrt();
        if den > 0.0 {
            (cross_re * cross_re + cross_im * cross_im).sqrt() / den
        } else {
            0.0
        }
    };
    // Rank HF bands by OUR OWN energy, so the energetic-band handful is
    // chosen by real content, not by whatever the reference happens to carry.
    let mut ranked: Vec<(usize, f64)> = (min_band..num_bands)
        .map(|band| {
            let lo = band * BINS_PER_SBR_BAND;
            let hi = (lo + BINS_PER_SBR_BAND).min(bins);
            let e: f64 = (lo..hi)
                .flat_map(|b| o_spec[b].iter())
                .map(|c| f64::from(c.norm_sqr()))
                .sum();
            (band, e)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let mut selected: Vec<usize> = ranked.into_iter().take(top_n).map(|(b, _)| b).collect();
    selected.sort_unstable();

    let mut out = Vec::with_capacity(selected.len());
    for band in selected {
        let lo = band * BINS_PER_SBR_BAND;
        let hi = (lo + BINS_PER_SBR_BAND).min(bins);
        if lo >= hi {
            continue;
        }
        let mut energy = 0.0f64;
        let mut direct = 0.0f64;
        let mut mirror = 0.0f64;
        let mut shift_up = 0.0f64;
        let mut shift_down = 0.0f64;
        let count = (hi - lo) as f64;
        // `i` feeds several derived indices (mirror/shift math below), not a
        // plain element access -- an iterator/enumerate rewrite would be
        // less readable here, not more.
        #[allow(clippy::needless_range_loop)]
        for i in lo..hi {
            energy += o_spec[i]
                .iter()
                .map(|c| f64::from(c.norm_sqr()))
                .sum::<f64>();
            direct += coh(i, i);
            mirror += coh(i, hi + lo - 1 - i);
            shift_up += coh(i, i + BINS_PER_SBR_BAND);
            shift_down += if i >= BINS_PER_SBR_BAND {
                coh(i, i - BINS_PER_SBR_BAND)
            } else {
                0.0
            };
        }
        out.push((
            band as f64 * band_hz,
            energy,
            direct / count,
            mirror / count,
            shift_up / count,
            shift_down / count,
        ));
    }
    out
}

/// Round-38, Task 1: sweeps a constant HF-only sample-domain lag `k*64`
/// (`k` in -8..=8; one QMF synthesis slot = 64 output samples) applied ONLY
/// to `o`'s STFT read position, holding `t` fixed at the globally-aligned
/// `ob` offset -- the low band (which drives the global `lag` this window is
/// already re-sliced to via `oa`/`ob`) is never touched by this probe. Per
/// bin, coherence is the SAME "direct" measure `bin_level_conviction` uses
/// (`|sum_h O(h)*conj(T(h))| / sqrt(sum_h|O(h)|^2 * sum_h|T(h)|^2)`), then
/// averaged uniformly over every bin in every HF SBR band from `min_band`
/// upward (both parity regions together, not just the top-energy handful) --
/// a clear peak at k != 0 convicts a constant reference-vs-ours HF alignment
/// offset of that many QMF slots; a flat or zero-peaked curve refutes it.
pub(super) fn hf_lag_sweep(
    o: &[f32],
    t: &[f32],
    oa: usize,
    ob: usize,
    n: usize,
    min_band: usize,
) -> Vec<(i64, f64)> {
    const FFT_LEN: usize = 2048;
    const HOP: usize = 1024;
    const BINS_PER_SBR_BAND: usize = FFT_LEN / 256;
    const MAX_K: i64 = 8;
    const SLOT: i64 = 64;
    let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
    let probe_bins = ec_dsp::RealFft::<f32>::new(FFT_LEN).spectrum_len();
    let num_bands = probe_bins / BINS_PER_SBR_BAND;
    let lo_bin = min_band * BINS_PER_SBR_BAND;
    let hi_bin = (num_bands * BINS_PER_SBR_BAND).min(probe_bins);
    if lo_bin >= hi_bin {
        return Vec::new();
    }
    let hops = if n >= FFT_LEN {
        (n - FFT_LEN) / HOP + 1
    } else {
        0
    };
    if hops == 0 {
        return Vec::new();
    }
    let spec_of = |s: &[f32], start: usize| -> Vec<Vec<ec_dsp::Complex<f32>>> {
        let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
        let mut spec = vec![Vec::with_capacity(hops); probe_bins];
        let mut spectrum = vec![ec_dsp::Complex::new(0.0f32, 0.0); probe_bins];
        for h in 0..hops {
            let mut block = s[start + h * HOP..start + h * HOP + FFT_LEN].to_vec();
            win.apply(&mut block);
            rfft.forward(&block, &mut spectrum);
            for (b, c) in spectrum.iter().enumerate() {
                spec[b].push(*c);
            }
        }
        spec
    };
    let mut out = Vec::with_capacity((2 * MAX_K + 1) as usize);
    for k in -MAX_K..=MAX_K {
        let shift = k * SLOT;
        // Prefer shifting `o`'s read start (the literal ask), but `o` has no
        // samples before its own decode origin -- when the global alignment
        // already pins `oa` at (or near) 0, negative `k` has nowhere to read
        // from on that side. `coh(o[x], t[y])` only depends on `x - y`, so
        // shifting `t`'s read start the OPPOSITE direction instead
        // (`ob - shift`) tests the exact same relative offset and `t` (the
        // reference, decoded in full) always has the room. Only actually
        // falls back for `o`-boundary-starved `k`; the un-shifted `t` path
        // above is untouched for every `k` that fits within `o`.
        let (o_start, t_start) = {
            let os = oa as i64 + shift;
            if os >= 0 && os as usize + n <= o.len() {
                (os as usize, ob)
            } else {
                let ts = ob as i64 - shift;
                if ts >= 0 && ts as usize + n <= t.len() {
                    (oa, ts as usize)
                } else {
                    continue;
                }
            }
        };
        let o_spec = spec_of(o, o_start);
        let t_spec = spec_of(t, t_start);
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for b in lo_bin..hi_bin {
            let mut cross_re = 0.0f64;
            let mut cross_im = 0.0f64;
            let mut o_e = 0.0f64;
            let mut t_e = 0.0f64;
            for h in 0..hops {
                let oc = o_spec[b][h];
                let tc = t_spec[b][h];
                let cross = oc * tc.conj();
                cross_re += f64::from(cross.re);
                cross_im += f64::from(cross.im);
                o_e += f64::from(oc.norm_sqr());
                t_e += f64::from(tc.norm_sqr());
            }
            let den = (o_e * t_e).sqrt();
            if den > 0.0 {
                sum += (cross_re * cross_re + cross_im * cross_im).sqrt() / den;
                count += 1;
            }
        }
        out.push((k, if count > 0 { sum / count as f64 } else { 0.0 }));
    }
    out
}

/// Round-40, Task 1: fine spectral-BIN shift scan on patched HF content.
/// `hf_lag_sweep` (round-38) only tested whole-QMF-slot SAMPLE-domain lags and
/// round-26's `bin_level_conviction` shift columns only tested whole
/// `BINS_PER_SBR_BAND` (8-bin) band shifts; neither ever tried a FEW-bin
/// spectral offset, which is what a half-QMF-band (~4-bin, ~86Hz at this
/// 2048-window scale) carrier convention mismatch on patched content would
/// look like -- it would leave envelope/magnitude tracking (and therefore
/// `hf_lag_sweep`'s k=0 peak) intact while destroying bin-level phase
/// coherence via linear phase drift across the band. The audio itself is
/// never shifted; only the bin INDEX used to pair `o` against `t` moves, by
/// `b` in -7..=+7 (deliberately short of the 8-bin whole-band-shift case
/// round-26 already ruled out). Reports mean direct coherence
/// (`bin_level_conviction`'s same `|sum_h O(h)*conj(T(h))| /
/// sqrt(sum|O|^2*sum|T|^2)` measure) separately over the even-gap [14,28) and
/// odd-gap [28,43) SBR-band regions -- the offset, if real, could differ by
/// patch parity given round-35/36's convention-fix history. A clear peak at
/// `b != 0` (region-dependent sign allowed) convicts a constant sub-band
/// frequency offset; a flat/0-peaked curve refutes it.
pub(super) fn hf_bin_shift_sweep(
    o: &[f32],
    t: &[f32],
    _rate: u32,
    region_lo_band: usize,
    region_hi_band: usize,
) -> Vec<(i64, f64)> {
    const FFT_LEN: usize = 2048;
    const HOP: usize = 1024;
    const BINS_PER_SBR_BAND: usize = FFT_LEN / 256;
    const MAX_B: i64 = 7;
    let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
    let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
    let bins = rfft.spectrum_len();
    let n = o.len().min(t.len());
    let hops = if n >= FFT_LEN {
        (n - FFT_LEN) / HOP + 1
    } else {
        0
    };
    if hops == 0 {
        return Vec::new();
    }
    let mut o_spec = vec![Vec::with_capacity(hops); bins];
    let mut t_spec = vec![Vec::with_capacity(hops); bins];
    let mut spectrum = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
    for (src, dst) in [(o, &mut o_spec), (t, &mut t_spec)] {
        for h in 0..hops {
            let mut block = src[h * HOP..h * HOP + FFT_LEN].to_vec();
            win.apply(&mut block);
            rfft.forward(&block, &mut spectrum);
            for (b, c) in spectrum.iter().enumerate() {
                dst[b].push(*c);
            }
        }
    }
    let lo_bin = region_lo_band * BINS_PER_SBR_BAND;
    let hi_bin = (region_hi_band * BINS_PER_SBR_BAND).min(bins);
    let coh = |ob: usize, tb: usize| -> f64 {
        if ob >= bins || tb >= bins {
            return 0.0;
        }
        let mut cross_re = 0.0f64;
        let mut cross_im = 0.0f64;
        let mut o_e = 0.0f64;
        let mut t_e = 0.0f64;
        for h in 0..hops {
            let oc = o_spec[ob][h];
            let tc = t_spec[tb][h];
            let cross = oc * tc.conj();
            cross_re += f64::from(cross.re);
            cross_im += f64::from(cross.im);
            o_e += f64::from(oc.norm_sqr());
            t_e += f64::from(tc.norm_sqr());
        }
        let den = (o_e * t_e).sqrt();
        if den > 0.0 {
            (cross_re * cross_re + cross_im * cross_im).sqrt() / den
        } else {
            0.0
        }
    };
    let mut out = Vec::with_capacity((2 * MAX_B + 1) as usize);
    if lo_bin >= hi_bin {
        return out;
    }
    for b in -MAX_B..=MAX_B {
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for i in lo_bin..hi_bin {
            let oi = i as i64 + b;
            if oi < 0 || oi as usize >= bins {
                continue;
            }
            sum += coh(oi as usize, i);
            count += 1;
        }
        out.push((b, if count > 0 { sum / count as f64 } else { 0.0 }));
    }
    out
}

/// Round-40, Task 2: per-STFT-frame complex-gain fingerprint. For the same
/// top-energy HF bands `bin_level_conviction` picks (ranked by `o`'s own
/// energy), takes each band's single dominant bin (highest total `|O(h)|^2`
/// across hops) and reports the per-hop complex ratio `O(h)/T(h)` trajectory
/// -- `(magnitude, phase_radians)` per STFT frame -- for that bin across
/// every frame. Read afterward, not here: constant magnitude+phase is a pure
/// gain (would predict HIGH coherence, contradicting the low bin-coherence
/// measured elsewhere -- flags a measurement issue, not a real
/// transformation); phase advancing at a roughly fixed per-frame rate is a
/// genuine carrier frequency offset (the rate converts directly to Hz and
/// cross-checks `hf_bin_shift_sweep`'s peak); phase jumping specifically at
/// hop indices lining up with SBR envelope borders is a per-envelope
/// gain/phase re-seed; anything else (magnitude and phase both wandering with
/// no fixed rate) is genuinely uncorrelated content frame to frame.
pub(super) fn hf_complex_ratio_trajectory(
    o: &[f32],
    t: &[f32],
    rate: u32,
    min_band: usize,
    top_n: usize,
) -> Vec<(f64, Vec<(f64, f64)>)> {
    const FFT_LEN: usize = 2048;
    const HOP: usize = 1024;
    const BINS_PER_SBR_BAND: usize = FFT_LEN / 256;
    let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
    let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
    let bins = rfft.spectrum_len();
    let n = o.len().min(t.len());
    let hops = if n >= FFT_LEN {
        (n - FFT_LEN) / HOP + 1
    } else {
        0
    };
    if hops == 0 {
        return Vec::new();
    }
    let mut o_spec = vec![Vec::with_capacity(hops); bins];
    let mut t_spec = vec![Vec::with_capacity(hops); bins];
    let mut spectrum = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
    for (src, dst) in [(o, &mut o_spec), (t, &mut t_spec)] {
        for h in 0..hops {
            let mut block = src[h * HOP..h * HOP + FFT_LEN].to_vec();
            win.apply(&mut block);
            rfft.forward(&block, &mut spectrum);
            for (b, c) in spectrum.iter().enumerate() {
                dst[b].push(*c);
            }
        }
    }
    let band_hz = f64::from(rate) / 256.0;
    let num_bands = bins / BINS_PER_SBR_BAND;
    if min_band >= num_bands {
        return Vec::new();
    }
    let mut ranked: Vec<(usize, f64)> = (min_band..num_bands)
        .map(|band| {
            let lo = band * BINS_PER_SBR_BAND;
            let hi = (lo + BINS_PER_SBR_BAND).min(bins);
            let e: f64 = (lo..hi)
                .flat_map(|b| o_spec[b].iter())
                .map(|c| f64::from(c.norm_sqr()))
                .sum();
            (band, e)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let mut selected: Vec<usize> = ranked.into_iter().take(top_n).map(|(b, _)| b).collect();
    selected.sort_unstable();

    let mut out = Vec::with_capacity(selected.len());
    for band in selected {
        let lo = band * BINS_PER_SBR_BAND;
        let hi = (lo + BINS_PER_SBR_BAND).min(bins);
        if lo >= hi {
            continue;
        }
        let dom = (lo..hi)
            .max_by(|&a, &b| {
                let ea: f64 = o_spec[a].iter().map(|c| f64::from(c.norm_sqr())).sum();
                let eb: f64 = o_spec[b].iter().map(|c| f64::from(c.norm_sqr())).sum();
                ea.partial_cmp(&eb).unwrap()
            })
            .unwrap();
        // Complex division `O/T` done by hand (`ec_dsp::Complex` carries no
        // `Div` impl -- it is a from-scratch minimal type, see fft.rs): `O *
        // conj(T) / |T|^2`.
        let traj: Vec<(f64, f64)> = (0..hops)
            .map(|h| {
                let oc = o_spec[dom][h];
                let tc = t_spec[dom][h];
                let t_e = f64::from(tc.norm_sqr());
                if t_e > 1e-12 {
                    let (or, oi) = (f64::from(oc.re), f64::from(oc.im));
                    let (tr, ti) = (f64::from(tc.re), f64::from(tc.im));
                    let rr = (or * tr + oi * ti) / t_e;
                    let ri = (oi * tr - or * ti) / t_e;
                    ((rr * rr + ri * ri).sqrt(), ri.atan2(rr))
                } else {
                    (0.0, 0.0)
                }
            })
            .collect();
        out.push((band as f64 * band_hz, traj));
    }
    out
}

/// Round-45, Task 3 THIRD WITNESS. Every prior round compared OURS against
/// the REFERENCE directly -- a pairwise measurement that can never say which
/// side is wrong when they disagree. This builds a reference-free model: a
/// pure "plain patch-copy + per-band energy match" reconstruction of HF
/// content from the REFERENCE's own LOW band (the one piece of ground truth
/// both decoders start from and our own low-band round-trip already measures
/// as near-identity), using the verified patch map
/// `[(0,14,14),(3,28,11),(0,39,3),(13,42,1)]` and this file's own established
/// `band_hz = rate/256`, `BINS_PER_SBR_BAND = FFT_LEN/256 = 8` convention
/// (round-14/16; NOT the `rate/128` scale the naive spec reading suggests --
/// that value indexes the QMF/hybrid analysis grid, not this STFT-bin grid,
/// and using it here would be the "trap" that bit round-42 elsewhere in this
/// file. The patch map's own band numbers (14/28/39/42/43) only make sense
/// under the `/256` convention already used everywhere else in this file that
/// touches the patch map -- confirmed by construction, not re-derived here).
///
/// For each HF band in `[14, 43)` and each STFT hop, the simulated bins are
/// the REFERENCE's low-band bins at the patch-mapped source position, then
/// RESCALED per (band, hop) to match the comparison side's own energy in
/// that band/hop -- two separate rescalings, "sim-normalized-to-ours" and
/// "sim-normalized-to-reference" -- so the only thing left for the
/// coherence measure to see is phase/content structure, not a gain
/// difference (the gain question is already closed by prior rounds). Returns
/// `(band, coherence)` pairs for sim-vs-reference and sim-vs-ours.
type BandCoherenceTable = Vec<(usize, f64)>;

pub(super) fn hf_patch_simulator(o: &[f32], t: &[f32], rate: u32) -> (BandCoherenceTable, BandCoherenceTable) {
    const FFT_LEN: usize = 2048;
    const HOP: usize = 1024;
    const BINS_PER_SBR_BAND: usize = FFT_LEN / 256;
    const PATCHES: [(usize, usize, usize); 4] = [(0, 14, 14), (3, 28, 11), (0, 39, 3), (13, 42, 1)];
    let _ = rate; // band Hz not needed here; caller converts band index itself.
    let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
    let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
    let bins = rfft.spectrum_len();
    let n = o.len().min(t.len());
    let hops = if n >= FFT_LEN {
        (n - FFT_LEN) / HOP + 1
    } else {
        0
    };
    if hops == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut o_spec = vec![Vec::with_capacity(hops); bins];
    let mut t_spec = vec![Vec::with_capacity(hops); bins];
    let mut spectrum = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
    for (src, dst) in [(o, &mut o_spec), (t, &mut t_spec)] {
        for h in 0..hops {
            let mut block = src[h * HOP..h * HOP + FFT_LEN].to_vec();
            win.apply(&mut block);
            rfft.forward(&block, &mut spectrum);
            for (b, c) in spectrum.iter().enumerate() {
                dst[b].push(*c);
            }
        }
    }
    // Plain patch copy: sim's target bins, per hop, are the reference's
    // source bins, unmodified (no phase adjustment) -- modelling exactly the
    // "plain patch+gain" hypothesis the charter names.
    let mut sim_spec = vec![vec![ec_dsp::Complex::new(0.0f32, 0.0); hops]; bins];
    for (src, tgt, width) in PATCHES {
        let src_lo = src * BINS_PER_SBR_BAND;
        let tgt_lo = tgt * BINS_PER_SBR_BAND;
        let len = width * BINS_PER_SBR_BAND;
        for i in 0..len {
            if src_lo + i >= bins || tgt_lo + i >= bins {
                continue;
            }
            for h in 0..hops {
                sim_spec[tgt_lo + i][h] = t_spec[src_lo + i][h];
            }
        }
    }
    let num_bands = bins / BINS_PER_SBR_BAND;
    let top_band = 43.min(num_bands);
    // Per (band, hop) rescale of `sim` to match `target`'s energy in that
    // band/hop, then the same magnitude-of-summed-cross-spectrum coherence
    // measure every prior round's `coh` helper uses (already scale-invariant
    // to a CONSTANT gain across hops; this rescale additionally removes a
    // gain that varies hop-to-hop, isolating phase/content agreement).
    let band_coherence = |target: &[Vec<ec_dsp::Complex<f32>>]| -> Vec<(usize, f64)> {
        (14..top_band)
            .map(|band| {
                let lo = band * BINS_PER_SBR_BAND;
                let hi = (lo + BINS_PER_SBR_BAND).min(bins);
                let mut cross_re = 0.0f64;
                let mut cross_im = 0.0f64;
                let mut sim_e = 0.0f64;
                let mut tgt_e = 0.0f64;
                for h in 0..hops {
                    let se: f64 = (lo..hi).map(|b| f64::from(sim_spec[b][h].norm_sqr())).sum();
                    let te: f64 = (lo..hi).map(|b| f64::from(target[b][h].norm_sqr())).sum();
                    if se <= 1e-20 || te <= 0.0 {
                        continue;
                    }
                    let scale = (te / se).sqrt() as f32;
                    for b in lo..hi {
                        let sc = sim_spec[b][h].scale(scale);
                        let tc = target[b][h];
                        let cross = sc * tc.conj();
                        cross_re += f64::from(cross.re);
                        cross_im += f64::from(cross.im);
                        sim_e += f64::from(sc.norm_sqr());
                        tgt_e += f64::from(tc.norm_sqr());
                    }
                }
                let den = (sim_e * tgt_e).sqrt();
                let coh = if den > 0.0 {
                    (cross_re * cross_re + cross_im * cross_im).sqrt() / den
                } else {
                    0.0
                };
                (band, coh)
            })
            .collect()
    };
    (band_coherence(&t_spec), band_coherence(&o_spec))
}

/// (Round-46) The QMF-domain exact version of [`hf_patch_simulator`]: same
/// plain-patch+gain model, same verified patch map, but read through the
/// crate's own [`ec_aac::sbr_qmf::HfAnalysis`] (64-band, `rate/128`-wide
/// bands -- exactly the patch map's own band unit) instead of an 8-bin-wide
/// STFT approximation, closing the "too crude to reach a verdict" gap
/// round-45's STFT simulator hit. Validated by
/// `hf_analysis_self_consistency_control` in `sbr_qmf.rs` before use here
/// (own-band plurality + self-coherence + round-trip control). Same
/// per-(band, window) energy-rescale-then-coherence method as the STFT
/// version, windowed over `WIN` consecutive QMF slots (`WIN * SYNTHESIS_BANDS
/// = 1024` samples, matching the STFT version's hop) so the rescale has
/// enough samples to estimate energy from, not so many that within-window
/// phase drift is averaged away.
pub(super) const QMF_PATCHES: [(usize, usize, usize); 4] = [(0, 14, 14), (3, 28, 11), (0, 39, 3), (13, 42, 1)];

/// Shared build step for the QMF-domain third-witness instruments: both
/// PCMs read through [`ec_aac::sbr_qmf::HfAnalysis`], plus the plain
/// patch-copy simulation of the reference's own low band, all in the
/// `(band, slot)` domain. Returns `(ours, reference, sim)`.
/// `[band][slot]` complex QMF-domain series.
type QmfBandSlots = Vec<Vec<ec_dsp::Complex<f64>>>;

pub(super) fn qmf_domain_spec(o: &[f32], t: &[f32]) -> (QmfBandSlots, QmfBandSlots, QmfBandSlots) {
    use ec_aac::sbr_qmf::{HfAnalysis, SYNTHESIS_BANDS};
    let n = o.len().min(t.len());
    let slots = n / SYNTHESIS_BANDS;
    let analyze = |pcm: &[f32]| -> QmfBandSlots {
        let mut ana = HfAnalysis::new();
        let mut out: QmfBandSlots = (0..SYNTHESIS_BANDS)
            .map(|_| Vec::with_capacity(slots))
            .collect();
        for s in 0..slots {
            let mut chunk = [0.0f32; SYNTHESIS_BANDS];
            chunk.copy_from_slice(&pcm[s * SYNTHESIS_BANDS..(s + 1) * SYNTHESIS_BANDS]);
            let sub = ana.process_slot(&chunk);
            for (b, c) in sub.iter().enumerate() {
                out[b].push(*c);
            }
        }
        out
    };
    let o_spec = analyze(o);
    let t_spec = analyze(t);
    // Plain patch copy: sim's target-band slot series is the reference's
    // source-band slot series, unmodified.
    let mut sim_spec = vec![vec![ec_dsp::Complex::new(0.0f64, 0.0); slots]; SYNTHESIS_BANDS];
    for (src, tgt, width) in QMF_PATCHES {
        for i in 0..width {
            if src + i >= SYNTHESIS_BANDS || tgt + i >= SYNTHESIS_BANDS {
                continue;
            }
            sim_spec[tgt + i] = t_spec[src + i].clone();
        }
    }
    (o_spec, t_spec, sim_spec)
}

/// Returns `(sim-vs-reference, sim-vs-ours, ours-vs-reference)` -- the last
/// pairing (round-48, Task 4a) is the ultimate content-match witness: how
/// well our actual HF QMF content agrees with the reference's, with no
/// simulator in between.
pub(super) fn hf_patch_simulator_qmf(
    o: &[f32],
    t: &[f32],
) -> (BandCoherenceTable, BandCoherenceTable, BandCoherenceTable) {
    use ec_aac::sbr_qmf::SYNTHESIS_BANDS;
    const WIN: usize = 16;
    let (o_spec, t_spec, sim_spec) = qmf_domain_spec(o, t);
    let slots = sim_spec.first().map(Vec::len).unwrap_or(0);
    if slots < WIN {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let top_band = 43.min(SYNTHESIS_BANDS);
    let windows = slots / WIN;
    let band_coherence = |source: &[Vec<ec_dsp::Complex<f64>>],
                          target: &[Vec<ec_dsp::Complex<f64>>]|
     -> Vec<(usize, f64)> {
        (14..top_band)
            .map(|band| {
                let mut cross_re = 0.0f64;
                let mut cross_im = 0.0f64;
                let mut sim_e = 0.0f64;
                let mut tgt_e = 0.0f64;
                for w in 0..windows {
                    let lo = w * WIN;
                    let hi = lo + WIN;
                    let se: f64 = source[band][lo..hi]
                        .iter()
                        .map(|c| c.re * c.re + c.im * c.im)
                        .sum();
                    let te: f64 = target[band][lo..hi]
                        .iter()
                        .map(|c| c.re * c.re + c.im * c.im)
                        .sum();
                    if se <= 1e-20 || te <= 0.0 {
                        continue;
                    }
                    let scale = (te / se).sqrt();
                    for i in lo..hi {
                        let sc = source[band][i].scale(scale);
                        let tc = target[band][i];
                        let cross = sc * tc.conj();
                        cross_re += cross.re;
                        cross_im += cross.im;
                        sim_e += sc.re * sc.re + sc.im * sc.im;
                        tgt_e += tc.re * tc.re + tc.im * tc.im;
                    }
                }
                let den = (sim_e * tgt_e).sqrt();
                let coh = if den > 0.0 {
                    (cross_re * cross_re + cross_im * cross_im).sqrt() / den
                } else {
                    0.0
                };
                (band, coh)
            })
            .collect()
    };
    (
        band_coherence(&sim_spec, &t_spec),
        band_coherence(&sim_spec, &o_spec),
        band_coherence(&o_spec, &t_spec),
    )
}

/// (Round-46, Task 2) Per-band 2nd-order complex prediction transfer fit
/// between the plain-copy sim series and the REFERENCE's own HF series for
/// that band: `ref[l] ~= a0*sim[l] + a1*sim[l-1] + a2*sim[l-2]`, least
/// squares over the whole file -- the same shape `sbr_hf::generate`'s own
/// chirp filter has (`y = residual + ca1*y[n-1] + ca2*y[n-2]`, an IIR rather
/// than this FIR, but both are order-2 linear reshapings of the same copied
/// content), so the fitted taps are directly comparable in spirit to our own
/// bandwidth-expanded LPC coefficients: near `(1,0,0)` means "reference is
/// also just a plain copy here", taps with real weight on lag 1/2 mean a
/// genuine order-2 transformation is present.
/// `target_is_ours`: fit against our own decode (`o_spec`) instead of the
/// reference's (`t_spec`) -- same `sim` (built from the reference's own low
/// band either way), so the two calls' fitted taps are directly comparable:
/// what transform sim would need to become ours vs. to become the reference.
#[allow(clippy::needless_range_loop)] // fixed 3x3 complex Gaussian elimination, index math throughout
pub(super) fn hf_patch_transfer_fit(
    o: &[f32],
    t: &[f32],
    bands: &[usize],
    target_is_ours: bool,
) -> Vec<(usize, [ec_dsp::Complex<f64>; 3])> {
    let (o_spec, t_spec, sim_spec) = qmf_domain_spec(o, t);
    let target_spec = if target_is_ours { &o_spec } else { &t_spec };
    let mut results = Vec::new();
    for &band in bands {
        if band >= sim_spec.len() {
            continue;
        }
        let sim_raw = &sim_spec[band];
        let ref_raw = &target_spec[band];
        let len = sim_raw.len().min(ref_raw.len());
        if len < 8 {
            continue;
        }
        // Normalize each series to unit RMS first: the raw QMF-domain
        // amplitudes differ by orders of magnitude (sim carries no envelope
        // gain, `ref` does), which would otherwise make every fitted
        // coefficient collapse toward the gain ratio rather than the
        // fraction-of-`ref`-explained-by-which-lag this fit is meant to
        // read off.
        let rms = |s: &[ec_dsp::Complex<f64>]| -> f64 {
            (s[..len]
                .iter()
                .map(|c| c.re * c.re + c.im * c.im)
                .sum::<f64>()
                / len as f64)
                .sqrt()
                .max(1e-300)
        };
        let sim_scale = 1.0 / rms(sim_raw);
        let ref_scale = 1.0 / rms(ref_raw);
        let sim: Vec<ec_dsp::Complex<f64>> =
            sim_raw[..len].iter().map(|c| c.scale(sim_scale)).collect();
        let refb: Vec<ec_dsp::Complex<f64>> =
            ref_raw[..len].iter().map(|c| c.scale(ref_scale)).collect();
        let sim = &sim[..];
        let refb = &refb[..];
        // Complex 3x3 Hermitian normal equations: r[i][j] = sum sim[l-i] conj(sim[l-j]),
        // p[i] = sum sim[l-i] conj(ref[l])... solved as R^T a = p with R real basis in
        // complex arithmetic (Gaussian elimination with partial pivoting by magnitude).
        let mut r = [[ec_dsp::Complex::new(0.0f64, 0.0); 3]; 3];
        let mut p = [ec_dsp::Complex::new(0.0f64, 0.0); 3];
        for l in 2..len {
            let s = [sim[l], sim[l - 1], sim[l - 2]];
            let y = refb[l];
            for i in 0..3 {
                p[i] = p[i] + s[i].conj() * y;
                for j in 0..3 {
                    r[i][j] = r[i][j] + s[i].conj() * s[j];
                }
            }
        }
        // Gaussian elimination on the augmented [r | p] system.
        let mut aug = r;
        let mut rhs = p;
        for col in 0..3 {
            let mut piv = col;
            let mut best = aug[col][col].norm_sqr();
            for row in (col + 1)..3 {
                let m = aug[row][col].norm_sqr();
                if m > best {
                    best = m;
                    piv = row;
                }
            }
            if best < 1e-24 {
                continue;
            }
            aug.swap(col, piv);
            rhs.swap(col, piv);
            let inv = aug[col][col].conj().scale(1.0 / best);
            for row in (col + 1)..3 {
                let f = aug[row][col] * inv;
                for k in 0..3 {
                    aug[row][k] = aug[row][k] - f * aug[col][k];
                }
                rhs[row] = rhs[row] - f * rhs[col];
            }
        }
        let mut a = [ec_dsp::Complex::new(0.0f64, 0.0); 3];
        for row in (0..3).rev() {
            let mut acc = rhs[row];
            for k in (row + 1)..3 {
                acc = acc - aug[row][k] * a[k];
            }
            let d = aug[row][row];
            a[row] = if d.norm_sqr() > 1e-24 {
                acc * d.conj().scale(1.0 / d.norm_sqr())
            } else {
                ec_dsp::Complex::new(0.0, 0.0)
            };
        }
        results.push((band, a));
    }
    results
}

/// A file discovered under the user's own media directories: its path, which
/// AAC stream in it (0-based among AAC streams only) carries HE-AAC, and the
/// core/SBR crossover an ffprobe/ASC inspection already pinned for it -- used
/// only to draw the below/above-crossover line for localization, not as a
/// spec-exact `kx` band edge.
pub(super) struct Candidate {
    pub(super) path: PathBuf,
    /// Which AAC stream, 0-based among AAC streams only -- what
    /// `extract_from` selects out of the demuxer's stream list.
    pub(super) aac_stream: usize,
    /// The same stream's absolute ffprobe/ffmpeg index -- what `-map`
    /// selects, and not the same number when the container also carries a
    /// non-AAC audio track before it (FMJ's mkv has an AC-3 track between its
    /// two AAC ones).
    pub(super) ffmpeg_stream: usize,
    pub(super) crossover_hz: f64,
}

/// Round-20 residual/side-info correlation. Builds a (frame x band) residual
/// ENERGY grid at AU-aligned 2048-output-sample blocks -- a plain (2x) SBR
/// stream reconstructs exactly one 2048-sample block per core access unit,
/// so `n`-th block lines up 1:1 with `ec_aac::sbr_sideinfo_log()`'s `frame`
/// counter -- then cross-tabulates that grid against every side-info feature
/// the log carries. FFT is linear, so `FFT(o_block) - FFT(t_block) ==
/// FFT(o_block - t_block)`: the residual's own per-bin energy is obtained
/// directly from each side's spectrum without ever materializing a
/// time-domain residual array. `band` is the same `rate/256`-wide index
/// domain `per_band_correlation`/`tables.kx`/`f_high`/`f_noise` all already
/// share (measured in round-14/round-16), so a side-info row's `f_high`/
/// `f_noise` boundaries index straight into this grid's bins with no
/// rescaling.
pub(super) fn residual_sideinfo_analysis(o: &[f32], t: &[f32], oa: usize, ob: usize, rate: u32, ch: usize) {
    let rows: Vec<_> = ec_aac::sbr_sideinfo_log()
        .into_iter()
        .filter(|r| r.ch == ch)
        .collect();
    if rows.is_empty() {
        eprintln!("  ch{ch} SIDEINFO: no rows logged (set EC_AAC_SBR_SIDEINFO_DEBUG too)");
        return;
    }
    const FFT_LEN: usize = 2048;
    let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
    let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
    let bins = rfft.spectrum_len();
    let mut o_spec = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
    let mut t_spec = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];

    // Per-frame residual bin-energy vector (index = `band`, per-bin, not yet
    // grouped) and, alongside it, a per-bin peakiness ratio (own-bin energy
    // vs its 3-bin neighbourhood mean) used by the whiteness check.
    let mut energy: std::collections::HashMap<usize, Vec<f64>> = std::collections::HashMap::new();
    for row in &rows {
        if energy.contains_key(&row.frame) {
            continue;
        }
        let base_o = oa + row.frame * FFT_LEN;
        let base_t = ob + row.frame * FFT_LEN;
        if base_o + FFT_LEN > o.len() || base_t + FFT_LEN > t.len() {
            continue;
        }
        let mut ob_win = o[base_o..base_o + FFT_LEN].to_vec();
        let mut tb_win = t[base_t..base_t + FFT_LEN].to_vec();
        win.apply(&mut ob_win);
        win.apply(&mut tb_win);
        rfft.forward(&ob_win, &mut o_spec);
        rfft.forward(&tb_win, &mut t_spec);
        let e: Vec<f64> = o_spec
            .iter()
            .zip(&t_spec)
            .map(|(a, b)| f64::from((*a - *b).norm_sqr()))
            .collect();
        energy.insert(row.frame, e);
    }
    if energy.is_empty() {
        eprintln!("  ch{ch} SIDEINFO: no frame overlapped the aligned window");
        return;
    }
    // SBR-band-index units (`kx`/`k2`/`f_high`/`f_noise`) are `band_hz =
    // rate/256` wide; FFT bins here are `bin_hz = rate/FFT_LEN` wide. The
    // ratio `band_hz/bin_hz = FFT_LEN/256 = 8` is rate-independent, matching
    // `per_band_correlation`'s own `lo = band*band_hz/bin_hz` conversion --
    // one SBR band is exactly 8 of these FFT bins, never 1.
    const BINS_PER_SBR_BAND: usize = FFT_LEN / 256;
    let band_range = |lo: i64, hi: i64| -> (usize, usize) {
        let lo = ((lo.max(0) as usize) * BINS_PER_SBR_BAND).min(bins);
        let hi = (((hi.max(0) as usize) * BINS_PER_SBR_BAND).max(lo + 1)).min(bins);
        (lo, hi)
    };
    let sum_range = |e: &[f64], lo: usize, hi: usize| -> f64 { e[lo..hi].iter().sum() };

    // --- predictor 1: add_harmonic bands (and their immediate neighbours) ---
    let (mut harm_sum, mut harm_n) = (0.0f64, 0usize);
    let (mut noharm_sum, mut noharm_n) = (0.0f64, 0usize);
    let (mut adj_sum, mut adj_n) = (0.0f64, 0usize);
    for row in &rows {
        let Some(e) = energy.get(&row.frame) else {
            continue;
        };
        let Some(harm) = &row.add_harmonic else {
            continue;
        };
        for (i, &flag) in harm.iter().enumerate() {
            let (lo, hi) = match (row.f_high.get(i), row.f_high.get(i + 1)) {
                (Some(&a), Some(&b)) => band_range(a, b),
                _ => continue,
            };
            let s = sum_range(e, lo, hi);
            let cells = (hi - lo).max(1);
            if flag != 0 {
                harm_sum += s / cells as f64;
                harm_n += 1;
            } else {
                noharm_sum += s / cells as f64;
                noharm_n += 1;
            }
            // adjacent band (off-by-one probe): the band one slot up.
            if flag != 0
                && let (Some(&a2), Some(&b2)) = (row.f_high.get(i + 1), row.f_high.get(i + 2))
            {
                let (lo2, hi2) = band_range(a2, b2);
                adj_sum += sum_range(e, lo2, hi2) / (hi2 - lo2).max(1) as f64;
                adj_n += 1;
            }
        }
    }

    // --- predictor 2: envelope time borders vs envelope interior ---
    // Envelope borders are in `t_env` (pre-`RATE`-scale envelope time-slot
    // units); one unit is `RATE(2) * SYNTHESIS_BANDS(64) = 128` output
    // samples. Time-domain residual energy (broadband, not per-band) near
    // (+/-64 samples) each INTERIOR border vs the envelope's own midpoint.
    let (mut border_sum, mut border_n) = (0.0f64, 0usize);
    let (mut mid_sum, mut mid_n) = (0.0f64, 0usize);
    let sample_energy = |global: usize| -> Option<f64> {
        let oi = oa + global;
        let ti = ob + global;
        if oi >= o.len() || ti >= t.len() {
            return None;
        }
        let d = f64::from(o[oi]) - f64::from(t[ti]);
        Some(d * d)
    };
    for row in &rows {
        let frame_base = row.frame * FFT_LEN;
        for w in row.t_env.windows(2) {
            let (b0, b1) = (w[0] * 128, w[1] * 128);
            if b1 <= b0 {
                continue;
            }
            let mid = (b0 + b1) / 2;
            for d in -64i64..=64 {
                let s0 = b0 + d;
                if s0 >= 0
                    && let Some(v) = sample_energy(frame_base + s0 as usize)
                {
                    border_sum += v;
                    border_n += 1;
                }
            }
            for off in 0..(b1 - b0).min(256) {
                let s = mid - (b1 - b0).min(256) / 2 + off;
                if s >= 0
                    && (s - mid).abs() > 64
                    && let Some(v) = sample_energy(frame_base + s as usize)
                {
                    mid_sum += v;
                    mid_n += 1;
                }
            }
        }
    }

    // --- predictor 3: hold-state (apply_last) vs fresh-payload frames ---
    let (mut hold_sum, mut hold_n) = (0.0f64, 0usize);
    let (mut fresh_sum, mut fresh_n) = (0.0f64, 0usize);
    for row in &rows {
        let Some(e) = energy.get(&row.frame) else {
            continue;
        };
        let total: f64 = e.iter().sum();
        if row.source == "hold" {
            hold_sum += total;
            hold_n += 1;
        } else {
            fresh_sum += total;
            fresh_n += 1;
        }
    }

    // --- predictor 4: frame immediately after a coupling flip ---
    let (mut flip_sum, mut flip_n) = (0.0f64, 0usize);
    let (mut noflip_sum, mut noflip_n) = (0.0f64, 0usize);
    let mut sorted = rows.clone();
    sorted.sort_by_key(|r| r.frame);
    for pair in sorted.windows(2) {
        let (prev, cur) = (&pair[0], &pair[1]);
        let Some(e) = energy.get(&cur.frame) else {
            continue;
        };
        let total: f64 = e.iter().sum();
        if cur.coupling != prev.coupling {
            flip_sum += total;
            flip_n += 1;
        } else {
            noflip_sum += total;
            noflip_n += 1;
        }
    }

    // --- predictor 5: residual scaling per invf_mode level, per noise band ---
    let mut per_level = [(0.0f64, 0usize); 4];
    for row in &rows {
        let Some(e) = energy.get(&row.frame) else {
            continue;
        };
        for (i, &mode) in row.invf_mode.iter().enumerate() {
            let (lo, hi) = match (row.f_noise.get(i), row.f_noise.get(i + 1)) {
                (Some(&a), Some(&b)) => band_range(a, b),
                _ => continue,
            };
            let cells = (hi - lo).max(1);
            let level = (mode as usize).min(3);
            per_level[level].0 += sum_range(e, lo, hi) / cells as f64;
            per_level[level].1 += 1;
        }
    }

    // --- predictor 6: residual whiteness within HF bands (peakiness) ---
    // mean(max-bin / mean-bin) over each HF band each frame -- near 1 reads
    // noise-like, large reads tonal (one bin dominating the band).
    let (mut peak_sum, mut peak_n) = (0.0f64, 0usize);
    for row in &rows {
        let Some(e) = energy.get(&row.frame) else {
            continue;
        };
        let (lo, hi) = band_range(row.kx, row.k2);
        for group_lo in (lo..hi.min(e.len())).step_by(4) {
            let group_hi = (group_lo + 4).min(hi).min(e.len());
            if group_hi <= group_lo {
                continue;
            }
            let slice = &e[group_lo..group_hi];
            let mean = slice.iter().sum::<f64>() / slice.len() as f64;
            let max = slice.iter().cloned().fold(0.0f64, f64::max);
            if mean > 0.0 {
                peak_sum += max / mean;
                peak_n += 1;
            }
        }
    }

    // Heat summary: the full (frame x band) grid collapsed to two profiles
    // (mean over bands per 50-frame group; mean over frames per 8-band
    // group) -- the full grid itself is `energy`, ~489 x ~90 cells, too
    // large to print usefully every run.
    let mut frame_keys: Vec<usize> = energy.keys().cloned().collect();
    frame_keys.sort_unstable();
    println!(
        "  ch{ch} RESIDUAL HEAT temporal profile (50-frame groups, mean energy over all bands):"
    );
    for chunk in frame_keys.chunks(50) {
        let (mut s, mut n) = (0.0f64, 0usize);
        for &f in chunk {
            if let Some(e) = energy.get(&f) {
                s += e.iter().sum::<f64>();
                n += e.len();
            }
        }
        if n > 0 {
            println!(
                "    frames {}..{}: {:.6e}",
                chunk[0],
                chunk[chunk.len() - 1],
                s / n as f64
            );
        }
    }
    // Nyquist bin: FFT_LEN/2 (the real bin at the Nyquist frequency, always
    // present in an RFFT of an even-length transform) -- bins past it are
    // this RFFT's own redundant/unused tail, never real spectrum.
    let bin_hz = f64::from(rate) / FFT_LEN as f64;
    let nyquist_bin = (FFT_LEN / 2 + 1).min(bins);
    println!(
        "  ch{ch} RESIDUAL HEAT spectral profile ({BINS_PER_SBR_BAND}-bin == 1 SBR-band groups, mean energy over all frames):"
    );
    for group_lo in (0..nyquist_bin).step_by(BINS_PER_SBR_BAND) {
        let group_hi = (group_lo + BINS_PER_SBR_BAND).min(nyquist_bin);
        let (mut s, mut n) = (0.0f64, 0usize);
        for e in energy.values() {
            for &v in &e[group_lo..group_hi] {
                s += v;
                n += 1;
            }
        }
        if n > 0 {
            println!(
                "    {:>6.0}Hz..{:>6.0}Hz (band {}): {:.6e}",
                group_lo as f64 * bin_hz,
                group_hi as f64 * bin_hz,
                group_lo / BINS_PER_SBR_BAND,
                s / n as f64
            );
        }
    }

    let avg = |s: f64, n: usize| if n > 0 { s / n as f64 } else { f64::NAN };
    println!(
        "  ch{ch} RESIDUAL/SIDEINFO ranked predictors ({} frames, {} rows):",
        energy.len(),
        rows.len()
    );
    println!(
        "    1. add_harmonic band mean energy: flagged={:.6e} (n={harm_n}) unflagged={:.6e} (n={noharm_n}) ratio={:.3} | adjacent(+1)-band-of-flagged={:.6e} (n={adj_n})",
        avg(harm_sum, harm_n),
        avg(noharm_sum, noharm_n),
        avg(harm_sum, harm_n) / avg(noharm_sum, noharm_n).max(1e-300),
        avg(adj_sum, adj_n)
    );
    println!(
        "    2. envelope time-border (+/-64 smp) mean energy={:.6e} (n={border_n}) vs interior={:.6e} (n={mid_n}) ratio={:.3}",
        avg(border_sum, border_n),
        avg(mid_sum, mid_n),
        avg(border_sum, border_n) / avg(mid_sum, mid_n).max(1e-300)
    );
    println!(
        "    3. hold-state(apply_last) frame total energy={:.6e} (n={hold_n}) vs fresh-payload={:.6e} (n={fresh_n}) ratio={:.3}",
        avg(hold_sum, hold_n),
        avg(fresh_sum, fresh_n),
        avg(hold_sum, hold_n) / avg(fresh_sum, fresh_n).max(1e-300)
    );
    println!(
        "    4. frame-after-coupling-flip total energy={:.6e} (n={flip_n}) vs no-flip={:.6e} (n={noflip_n}) ratio={:.3}",
        avg(flip_sum, flip_n),
        avg(noflip_sum, noflip_n),
        avg(flip_sum, flip_n) / avg(noflip_sum, noflip_n).max(1e-300)
    );
    println!("    5. invf_mode level -> mean per-band residual energy:");
    for (level, (s, n)) in per_level.iter().enumerate() {
        println!("       level {level}: {:.6e} (n={n})", avg(*s, *n));
    }
    println!(
        "    6. HF residual peakiness (max/mean over 4-bin groups): {:.3} (n={peak_n}) [near 1 = noise-like, larger = tonal]",
        avg(peak_sum, peak_n)
    );
}

/// Decodes one stream's core (AAC-LC) layer only, with SBR forced off at the
/// `AudioSpecificConfig` level -- `sbr_present: false` never arms
/// `BlockDecoder::sbr`, so `decode.rs`'s FIL-element branch never calls into
/// `sbr_chain` at all and `out` (the raw core `channel_stream` decode) is
/// handed back untouched, at the core rate. This isolates whether a
/// real-file break lives in the LC core or in the SBR chain bolted onto it.
pub(super) fn our_decode_core_only(path: &Path, stream_index: usize) -> Option<(Vec<Vec<f32>>, u32)> {
    let (asc, aus) = extract_aac_track(path, stream_index)?;
    let mut cfg = parse_audio_specific_config(&asc).ok()?;
    cfg.sbr_present = false;
    cfg.ps_present = false;
    cfg.extension_sample_rate = None;
    let core_rate = cfg.sample_rate;
    // `core_only` also stops implicit SBR: a bare `with_config` re-arms it for the
    // SBR payload this stream carries, and the "core" output would be the full chain.
    let mut decoder = AacDecoder::with_config(cfg).core_only();
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let dump = std::env::var("EC_AAC_SBR_PREQMF_DUMP").is_ok();
    for (au_idx, au) in aus.iter().enumerate() {
        let Ok(frame) = decoder.decode(au, None) else {
            continue;
        };
        let ch = usize::from(frame.channels);
        if ch == 0 {
            continue;
        }
        if dump && au_idx < 40 {
            for c in 0..ch {
                let plane: Vec<f32> = frame.samples.iter().skip(c).step_by(ch).copied().collect();
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
        if planes.is_empty() {
            planes = vec![Vec::new(); ch];
        }
        for (i, v) in frame.samples.iter().enumerate() {
            planes[i % ch].push(*v);
        }
        if planes[0].len() >= MAX_SAMPLES {
            break;
        }
    }
    if planes.is_empty() {
        return None;
    }
    Some((planes, core_rate))
}

/// The reference decoder's decode of `absolute_stream`, resampled to
/// `target_rate` -- used to compare against `our_decode_core_only`'s
/// core-rate PCM directly instead of at the doubled SBR rate.
pub(super) fn ffmpeg_decode_at_rate(
    path: &Path,
    absolute_stream: usize,
    channels: usize,
    target_rate: u32,
) -> Vec<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            &format!("0:{absolute_stream}"),
            "-t",
            "30",
            "-ar",
            &target_rate.to_string(),
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
        "ffmpeg decode failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let samples: Vec<f32> = out
        .stdout
        .as_chunks::<4>().0.iter()
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let mut planes = vec![Vec::with_capacity(samples.len() / channels.max(1)); channels];
    for (i, &v) in samples.iter().enumerate() {
        planes[i % channels].push(v);
    }
    planes
}
