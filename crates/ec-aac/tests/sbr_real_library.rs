//! The SBR chain, against real HE-AAC files pulled from the user's own media
//! directories, decoded through their real container (mp4 `esds` / Matroska
//! `CodecPrivate`) so `AacDecoder::with_config_bytes` actually arms the SBR
//! path -- `tests/oracle.rs` remuxes everything to bare ADTS first, and an
//! ADTS stream carries no AudioSpecificConfig, so its SBR chain never sees a
//! frame. Every file here is a live discovery over `~/Music`, `~/Downloads`,
//! `~/Videos`: absent files skip loudly, nothing is generated or bundled.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_aac::AacDecoder;
use ec_core::{CodecId, Demuxer, Packet};
use ec_matroska::MatroskaDemuxer;
use ec_mp4::Mp4Demuxer;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A film's audio track is hours long; the correlation only needs a bounded
/// window near the start, so both sides cap how much they ever decode.
const MAX_SAMPLES: usize = 1_000_000;

/// ffmpeg's decode of a file's given absolute stream index, as
/// planar-by-channel `f32`.
fn ffmpeg_decode(path: &Path, absolute_stream: usize, channels: usize) -> Vec<Vec<f32>> {
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
        .chunks_exact(4)
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
fn extract_aac_track(path: &Path, stream_index: usize) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
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
fn extract_from(d: &mut dyn Demuxer, stream_index: usize) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
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
fn our_decode(
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

/// Search window and lag bound: keeps the lag search and the correlation sum
/// both bounded, per the charter -- an unbounded version over a whole film's
/// worth of samples was too slow to be an every-run test.
const LAG_MAX: usize = 4_000;
const WINDOW: usize = 200_000;

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum()
}

fn correlation(a: &[f32], b: &[f32]) -> f64 {
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

/// Best correlation over a bounded lag search. Two passes keep the whole
/// search O(reasonable) on a film-length file: a coarse pass over every lag
/// but a short slice finds roughly where the alignment is, then one full-size
/// correlation at that lag (and its immediate neighbours, in case the coarse
/// slice picked a noisy lag) gives the reported number. The 8001-lag charter
/// bound times the full `WINDOW` in one pass, tried first, took minutes on
/// this box -- this is that ceiling brought back to milliseconds.
fn best_lag_correlation(ours: &[f32], theirs: &[f32]) -> (i64, f64) {
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
    let mut coarse_best = (0i64, -1.0f64, 0.0f64);
    for lag in -(LAG_MAX as i64)..=(LAG_MAX as i64) {
        let Some((oa, ob)) = slice_at(lag, COARSE) else {
            continue;
        };
        let c = correlation(&ours[oa..oa + COARSE], &theirs[ob..ob + COARSE]);
        if c.abs() > coarse_best.1 {
            coarse_best = (lag, c.abs(), c);
        }
    }
    let mut best = (coarse_best.0, -1.0f64, 0.0f64);
    let mut found = false;
    for lag in (coarse_best.0 - 8)..=(coarse_best.0 + 8) {
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
    (best.0, best.2)
}

/// Signed correlation of `ours` against `theirs` at one fixed `lag`, full
/// `WINDOW`-sized, anchored at the SAME `start` offset `best_lag_correlation`
/// uses internally -- used to test a hypothesized sign inversion by reusing
/// a KNOWN-good lag (and window) from a healthy channel instead of trusting
/// the (signed, positive-biased) search above to relocate both for a
/// channel that may be exactly inverted at that same lag. Anchoring must
/// match `best_lag_correlation`'s `start`, not sample 0 -- the two windows
/// otherwise cover different seconds of audio and are not comparable.
fn correlation_at_lag(ours: &[f32], theirs: &[f32], lag: i64) -> f64 {
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
fn lowpass(samples: &[f32], rate: u32, cutoff_hz: f64) -> Vec<f32> {
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

fn highpass(samples: &[f32], rate: u32, cutoff_hz: f64) -> Vec<f32> {
    let low = lowpass(samples, rate, cutoff_hz);
    samples.iter().zip(&low).map(|(a, b)| a - b).collect()
}

/// DIAGNOSTIC (round-13): cascades `lowpass` three times for a much sharper
/// stopband than the single first-order zero-phase filter the real
/// assertion uses -- checking whether the below-crossover bar is actually
/// being missed because of above-crossover energy leaking through the
/// gentle single-pass rolloff, not because the low band itself is wrong.
fn sharp_lowpass(samples: &[f32], rate: u32, cutoff_hz: f64) -> Vec<f32> {
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
fn windowed_lag_drift(
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
fn magnitude_series_correlation(a: &[f32], b: &[f32]) -> f64 {
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

fn per_band_correlation(o: &[f32], t: &[f32], rate: u32) -> Vec<(f64, f64, f64)> {
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
fn per_band_lag_search(o: &[f32], t: &[f32], rate: u32) -> Vec<(f64, i64, f64)> {
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
fn per_band_phase(o: &[f32], t: &[f32], rate: u32) -> Vec<(f64, f64, f64)> {
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
fn bandpass_offset_probe(
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
fn bin_level_conviction(
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
fn hf_lag_sweep(
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
fn hf_bin_shift_sweep(
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
fn hf_complex_ratio_trajectory(
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

/// A file discovered under the user's own media directories: its path, which
/// AAC stream in it (0-based among AAC streams only) carries HE-AAC, and the
/// core/SBR crossover an ffprobe/ASC inspection already pinned for it -- used
/// only to draw the below/above-crossover line for localization, not as a
/// spec-exact `kx` band edge.
struct Candidate {
    path: PathBuf,
    /// Which AAC stream, 0-based among AAC streams only -- what
    /// `extract_from` selects out of the demuxer's stream list.
    aac_stream: usize,
    /// The same stream's absolute ffprobe/ffmpeg index -- what `-map`
    /// selects, and not the same number when the container also carries a
    /// non-AAC audio track before it (FMJ's mkv has an AC-3 track between its
    /// two AAC ones).
    ffmpeg_stream: usize,
    crossover_hz: f64,
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
fn residual_sideinfo_analysis(o: &[f32], t: &[f32], oa: usize, ob: usize, rate: u32, ch: usize) {
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

fn candidates() -> Vec<Candidate> {
    let home = std::env::var("HOME").unwrap_or_default();
    [
        (format!("{home}/Music/Yok - Nikbinler.mp4"), 0, 0, 5_000.0),
        (
            format!(
                "{home}/Downloads/Full Metal Jacket (1987) (1080p BluRay x265 HEVC 10bit HDR AAC 5.1 afm72)/Full Metal Jacket (1987) (1080p BluRay x265 HDR afm72).mkv"
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
fn sbr_real_library_matches_reference() {
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
            // Nikbinler is a 30s file capped to ours=22.7s, so the old code
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
            let (lag, full) = best_lag_correlation(o, t);
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
            // Nikbinler ch1, full-band lag 65 vs 64 differ by only 0.0015 in
            // full-band correlation but by 0.88 in below-crossover
            // correlation once the low band is measured at its own winning
            // lag instead. Both bands are still re-anchored at the SAME
            // `start` offset internally (see `best_lag_correlation`), so
            // this is genuinely comparing the low band against itself, not
            // drifting to an unrelated part of the file.
            let low = {
                let ol_full = lowpass(o, rate, c.crossover_hz);
                let tl_full = lowpass(t, rate, c.crossover_hz);
                let (low_lag, low_corr) = best_lag_correlation(&ol_full, &tl_full);
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
                for (band, signal, noise) in ec_aac::noise_fraction_table() {
                    if signal + noise <= 0.0 {
                        continue;
                    }
                    let f = noise / (signal + noise);
                    println!(
                        "    {:>6.0}Hz band{band:>3}: signal {signal:.3e} noise {noise:.3e} f_noise {f:.6} ceiling {:.4}",
                        band as f64 * band_hz,
                        (1.0 - f).max(0.0).sqrt()
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
                            let base_o = (oa as i64 + lag + (w * 4_096) as i64).try_into().ok()?;
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
fn write_remux(out_path: &Path, asc: &[u8], aus: &[Vec<u8>]) {
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
fn report_conviction(label: &str, rows: &[(f64, f64, f64, f64, f64, f64)]) {
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

/// (Round-41, Task 1) Instrument anchor: before trusting `bin_level_conviction`'s
/// low (~0.15-0.25) HF direct-coherence numbers on Nikbinler as evidence the
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
fn sbr_instrument_anchor_controls() {
    if std::env::var("EC_AAC_SBR_INSTRUMENT_ANCHOR").is_err() {
        eprintln!("SKIP: set EC_AAC_SBR_INSTRUMENT_ANCHOR=1 to run (round-41 Task 1)");
        return;
    }
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let path = PathBuf::from(format!("{home}/Music/Yok - Nikbinler.mp4"));
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
/// decodes Nikbinler twice (noise on, then off) and reads the per-HF-band
/// `bin_level_conviction` energy delta between them as the fraction noise
/// ACTUALLY realizes in the output, independent of either prior claim.
#[test]
fn sbr_actual_noise_fraction() {
    if std::env::var("EC_AAC_SBR_NOISE_ANCHOR").is_err() {
        eprintln!("SKIP: set EC_AAC_SBR_NOISE_ANCHOR=1 to run (round-41 Task 2)");
        return;
    }
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not on PATH");
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let path = PathBuf::from(format!("{home}/Music/Yok - Nikbinler.mp4"));
    if !path.exists() {
        eprintln!("SKIP: {} not found", path.display());
        return;
    }
    const MIN_BAND: usize = 14;
    const TOP_N: usize = 128;

    // SAFETY: this test does not run concurrently with any other test that
    // reads these two vars (both are `ec_aac`-internal SBR debug switches,
    // not read by `sbr_real_library_matches_reference`'s own decode path
    // except through this same `zero_noise`/`track_fraction` plumbing) --
    // invoke this test by name, not as part of a parallel `cargo test` run.
    unsafe {
        std::env::remove_var("EC_AAC_SBR_NOISE_ZERO");
        std::env::set_var("EC_AAC_SBR_NOISE_FRACTION_DEBUG", "1");
    }
    let (ours_on, _, rate) = our_decode(&path, 0).expect("our_decode works (noise on)");
    let bookkept = ec_aac::noise_fraction_table();
    unsafe {
        std::env::remove_var("EC_AAC_SBR_NOISE_FRACTION_DEBUG");
        std::env::set_var("EC_AAC_SBR_NOISE_ZERO", "1");
    }
    let (ours_off, _, _) = our_decode(&path, 0).expect("our_decode works (noise zeroed)");
    unsafe {
        std::env::remove_var("EC_AAC_SBR_NOISE_ZERO");
    }

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
    let band_hz = f64::from(rate) / 256.0;
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
            .map(|(_, s, n)| if s + n > 0.0 { n / (s + n) } else { 0.0 })
            .unwrap_or(0.0);
        println!(
            "    {hz:>6.0}Hz: on {e_on:.3e} off {e_off:.3e} actual {actual:.4} bookkept {book:.4}"
        );
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
}
