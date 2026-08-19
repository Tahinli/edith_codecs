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
    for au in &aus {
        let frame = match decoder.decode(au, None) {
            Ok(f) => f,
            Err(_) => {
                failed += 1;
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
        for (ch, (o, t)) in ours.iter().zip(&theirs).enumerate() {
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
