//! Table over every screen recording under `~/Videos/OBS`, diagnosing the
//! PNS/M/S decorrelation the consumer sweep found in that encoder's output
//! (see repo ledger). Real files, read-only, absent directory skips loudly.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_aac::AacDecoder;
use ec_core::{CodecId, Demuxer, Packet};
use ec_mp4::Mp4Demuxer;
use std::fs::File;
use std::io::BufReader;

const MAX_SAMPLES: usize = 480_000; // 10s @ 48kHz
const LAG_MAX: usize = 4_000;
const WINDOW: usize = 200_000;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ffmpeg_decode(path: &Path, channels: usize) -> Vec<Vec<f32>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-t", "10", "-f", "f32le", "-acodec", "pcm_f32le", "-",
        ])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
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

fn extract_aac_track(path: &Path) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    let f = File::open(path).ok()?;
    let mut d = Mp4Demuxer::new(BufReader::new(f)).ok()?;
    let aac = d.streams().iter().find(|s| s.params.codec == CodecId::Aac)?;
    let idx = aac.index;
    let asc = aac.params.extradata.as_ref()?.to_vec();
    let mut aus = Vec::new();
    while aus.len() < 1_500 {
        match d.next_packet() {
            Ok(Packet { stream, data, .. }) if stream == idx => aus.push(data.to_vec()),
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    Some((asc, aus))
}

fn our_decode(path: &Path) -> Option<Vec<Vec<f32>>> {
    let (asc, aus) = extract_aac_track(path)?;
    let mut decoder = AacDecoder::with_config_bytes(&asc).ok()?;
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut failed = 0usize;
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
    eprintln!("  {failed}/{} AUs failed", aus.len());
    if planes.is_empty() { None } else { Some(planes) }
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum()
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

fn best_lag_correlation(ours: &[f32], theirs: &[f32]) -> (i64, f64) {
    const COARSE: usize = 4_096;
    let start = ours.len().min(theirs.len()).saturating_sub(WINDOW).min(ours.len() / 4);
    let slice_at = |lag: i64, len: usize| -> Option<(usize, usize)> {
        let (oa, ob) = if lag >= 0 { (start, start + lag as usize) } else { (start + (-lag) as usize, start) };
        if oa + len > ours.len() || ob + len > theirs.len() { None } else { Some((oa, ob)) }
    };
    let mut coarse_best = (0i64, -1.0f64, 0.0f64);
    for lag in -(LAG_MAX as i64)..=(LAG_MAX as i64) {
        let Some((oa, ob)) = slice_at(lag, COARSE) else { continue };
        let c = correlation(&ours[oa..oa + COARSE], &theirs[ob..ob + COARSE]);
        if c.abs() > coarse_best.1 {
            coarse_best = (lag, c.abs(), c);
        }
    }
    let mut best = (coarse_best.0, -1.0f64, 0.0f64);
    for lag in (coarse_best.0 - 8)..=(coarse_best.0 + 8) {
        let Some((oa, ob)) = slice_at(lag, WINDOW) else { continue };
        let c = correlation(&ours[oa..oa + WINDOW], &theirs[ob..ob + WINDOW]);
        if c.abs() > best.1 {
            best = (lag, c.abs(), c);
        }
    }
    (best.0, best.2)
}

fn rms(a: &[f32]) -> f64 {
    (a.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / a.len().max(1) as f64).sqrt()
}

#[test]
fn obs_content_table() {
    if !have_ffmpeg() {
        eprintln!("skip: no ffmpeg");
        return;
    }
    let dir = PathBuf::from(std::env::var("HOME").unwrap()).join("Videos/OBS");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!("skip: no {}", dir.display());
        return;
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("mp4"))
        .collect();
    files.sort();
    unsafe {
        std::env::set_var("EC_AAC_TOOL_SIDEINFO_DEBUG", "1");
    }
    for path in files {
        eprintln!("== {}", path.display());
        let before = ec_aac::tool_sideinfo_log().len();
        let Some(ours) = our_decode(&path) else {
            eprintln!("  our_decode: None");
            continue;
        };
        let theirs = ffmpeg_decode(&path, ours.len());
        for (ch, (o, t)) in ours.iter().zip(theirs.iter()).enumerate() {
            let (lag, corr) = best_lag_correlation(o, t);
            // Also the direct, un-windowed corr from t=0 (no lag search) --
            // this is what the consumer sweep that found this regression
            // measured, and can disagree with the windowed/lag-searched
            // number above if only part of the file is corrupted.
            let corr0 = correlation(o, t);
            let non_finite = o.iter().filter(|v| !v.is_finite()).count();
            eprintln!(
                "  ch{ch}: corr={corr:.4} corr@lag0={corr0:.4} lag={lag} rms_ours={:.5} rms_ffmpeg={:.5} non_finite={non_finite}",
                rms(o),
                rms(t)
            );
        }
        let all_rows = ec_aac::tool_sideinfo_log();
        let rows = &all_rows[before..];
        let (mut pns, mut is_, mut ms, mut tns, mut cpe) = (0usize, 0usize, 0usize, 0usize, 0usize);
        for r in rows {
            if r.pns_bands > 0 { pns += 1; }
            if r.is_bands > 0 { is_ += 1; }
            if r.ms_bands > 0 { ms += 1; }
            if r.tns_present { tns += 1; }
            if r.is_cpe { cpe += 1; }
        }
        eprintln!(
            "  tools over {} AUs: cpe={cpe} pns_aus={pns} is_aus={is_} ms_aus={ms} tns_aus={tns}",
            rows.len()
        );
    }
}
