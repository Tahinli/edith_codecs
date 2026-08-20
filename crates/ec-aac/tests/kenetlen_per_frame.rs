//! Per-frame corr scan for the coordinator's second repro: a real download
//! whose AAC-LC decode reads clean for the first several seconds then
//! collapses mid-stream, sequentially, with no seek/reset. Finds the first
//! bad frame and its side info, to compare with the frame before.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;

use ec_aac::AacDecoder;
use ec_core::{CodecId, Demuxer, Packet};
use ec_mp4::Mp4Demuxer;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn kenetlen_per_frame_scan() {
    if !have_ffmpeg() {
        eprintln!("skip: no ffmpeg");
        return;
    }
    let path = Path::new("/home/tahinli/Downloads/Kenetlenmişsin Kalbime.mp4");
    if !path.exists() {
        eprintln!("skip: file not present");
        return;
    }
    unsafe {
        std::env::set_var("EC_AAC_TOOL_SIDEINFO_DEBUG", "1");
    }

    // ffmpeg's channel-0-only decode (pan, not -ac, per the repro).
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-af", "pan=1c|c0=c0", "-f", "f32le", "-acodec", "pcm_f32le", "-",
        ])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let theirs: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    // Our sequential decode: one decoder, no reset, packet 0 onward.
    let f = File::open(path).unwrap();
    let mut d = Mp4Demuxer::new(BufReader::new(f)).unwrap();
    let aac = d
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::Aac)
        .unwrap();
    let idx = aac.index;
    let asc = aac.params.extradata.as_ref().unwrap().to_vec();
    let mut decoder = AacDecoder::with_config_bytes(&asc).unwrap();

    let mut ours: Vec<f32> = Vec::new();
    let mut frame_starts: Vec<usize> = Vec::new(); // sample offset each AU's ch0 output starts at
    let mut au_idx = 0usize;
    loop {
        let pkt = match d.next_packet() {
            Ok(Packet { stream, data, .. }) if stream == idx => data.to_vec(),
            Ok(_) => continue,
            Err(_) => break,
        };
        frame_starts.push(ours.len());
        match decoder.decode(&pkt, None) {
            Ok(frame) => {
                let ch = usize::from(frame.channels);
                if ch > 0 {
                    for chunk in frame.samples.chunks_exact(ch) {
                        ours.push(chunk[0]);
                    }
                }
            }
            Err(e) => eprintln!("AU {au_idx} decode error: {e:?}"),
        }
        au_idx += 1;
    }
    eprintln!("decoded {au_idx} AUs, {} ch0 samples ours, {} ffmpeg", ours.len(), theirs.len());

    let rows = ec_aac::tool_sideinfo_log();

    // Per-AU corr at zero lag (both start from packet 0 in decode order --
    // no lag search needed unless the two genuinely desync).
    let win = 1024usize;
    let mut first_bad: Option<usize> = None;
    let mut bad_count = 0usize;
    let mut pns_frames = 0usize;
    for r in &rows {
        if r.pns_bands > 0 {
            pns_frames += 1;
        }
    }
    eprintln!("frames with pns_bands>0: {pns_frames} / {}", rows.len());
    let full_rms_o: f64 = (ours.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / ours.len() as f64).sqrt();
    let full_rms_t: f64 = (theirs.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / theirs.len() as f64).sqrt();
    eprintln!("full-file rms ours={full_rms_o:.6} ffmpeg={full_rms_t:.6}");
    for (i, &start) in frame_starts.iter().enumerate() {
        if start + win > ours.len() || start + win > theirs.len() {
            break;
        }
        let a = &ours[start..start + win];
        let b = &theirs[start..start + win];
        let ma = a.iter().sum::<f32>() / win as f32;
        let mb = b.iter().sum::<f32>() / win as f32;
        let num: f64 = a.iter().zip(b).map(|(x, y)| f64::from(*x - ma) * f64::from(*y - mb)).sum();
        let da: f64 = a.iter().map(|x| f64::from(*x - ma).powi(2)).sum();
        let db: f64 = b.iter().map(|y| f64::from(*y - mb).powi(2)).sum();
        let corr = if da * db == 0.0 { 1.0 } else { num / (da * db).sqrt() };
        if corr.abs() < 0.9 {
            bad_count += 1;
            if first_bad.is_none() {
                first_bad = Some(i);
            }
            let rms_o: f64 = (a.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / win as f64).sqrt();
            let rms_t: f64 = (b.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / win as f64).sqrt();
            let pns = rows.get(i).map(|r| r.pns_bands).unwrap_or(0);
            eprintln!("AU {i}: sample_start={start} corr={corr:.4} rms_o={rms_o:.6} rms_t={rms_t:.6} pns_bands={pns} BAD");
        } else if i % 50 == 0 {
            eprintln!("AU {i}: sample_start={start} corr={corr:.4}");
        }
    }

    // Dump raw samples around the bad AU for inspection.
    if let Some(&start) = frame_starts.get(4) {
        eprintln!("--- AU4 samples (start={start}) ---");
        for k in 0..16 {
            eprintln!("  k={k} ours={:.6} ffmpeg={:.6}", ours[start + k], theirs[start + k]);
        }
        let rms_o: f64 = (ours[start..start + win].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / win as f64).sqrt();
        let rms_t: f64 = (theirs[start..start + win].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / win as f64).sqrt();
        eprintln!("  rms ours={rms_o:.6} ffmpeg={rms_t:.6}");
    }

    eprintln!("total BAD (|corr|<0.9) AUs: {bad_count} / {}", frame_starts.len());
    // Whole-window (first 10s @ 44.1k = 441000 samples) zero-lag Pearson,
    // ours vs ffmpeg -- reproduces (or not) the engine sweep's 0.9840 figure
    // from this decoder-level data alone.
    let n = 441_000usize.min(ours.len()).min(theirs.len());
    let (a, b) = (&ours[..n], &theirs[..n]);
    let ma = a.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let mb = b.iter().map(|v| f64::from(*v)).sum::<f64>() / n as f64;
    let (mut num, mut da, mut db) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x) - ma, f64::from(*y) - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    eprintln!("whole-10s zero-lag Pearson (n={n}): {:.6}", num / (da * db).sqrt());
    if let Some(bad) = first_bad {
        eprintln!("=== first bad AU: {bad} ===");
        for i in bad.saturating_sub(2)..=bad + 2 {
            if let Some(row) = rows.get(i) {
                eprintln!("  AU {i}: {row:?}");
            }
        }
    } else {
        eprintln!("no AU dropped below 0.9 corr");
    }
}
