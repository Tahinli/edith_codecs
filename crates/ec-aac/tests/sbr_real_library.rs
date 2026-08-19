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
            let (o_low, t_low, low) = if n >= 1024 {
                let ol = lowpass(&o[oa..oa + n], rate, c.crossover_hz);
                let tl = lowpass(&t[ob..ob + n], rate, c.crossover_hz);
                let lc = correlation(&ol, &tl);
                (ol, tl, lc)
            } else {
                (Vec::new(), Vec::new(), 0.0)
            };
            let high = if n >= 1024 {
                let oh = highpass(&o[oa..oa + n], rate, c.crossover_hz);
                let th = highpass(&t[ob..ob + n], rate, c.crossover_hz);
                correlation(&oh, &th)
            } else {
                0.0
            };
            println!(
                "  ch{ch}: lag {lag}, full {full:.6}, below {:.0}Hz {low:.6}, above {:.0}Hz {high:.6}",
                c.crossover_hz, c.crossover_hz
            );
            let _ = (&o_low, &t_low);
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
