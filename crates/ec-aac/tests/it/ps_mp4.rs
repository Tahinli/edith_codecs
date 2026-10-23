//! HE-AACv2 (Parametric Stereo) through the mp4 container path.
//!
//! The PS fixture is remuxed to mp4 with ffmpeg (stream copy), then its access
//! units are read back through `ec_mp4::Mp4Demuxer` and decoded with the
//! AudioSpecificConfig the `esds` box carries, as `AacDecoder::with_config_bytes`
//! does for any container. Nothing on this path may refuse PS: the decode must
//! produce two channels at the extension rate, matching FFmpeg's own decode of
//! the same mp4.

use crate::shared::have_ffmpeg;
use crate::common::fixture;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_aac::AacDecoder;
use ec_core::{CodecId, Demuxer, Packet};
use ec_mp4::Mp4Demuxer;

const FIXTURE: &str = "ps_48000_music.aac";
const RATE: u32 = 48_000;

/// FFmpeg's decode of `path` as interleaved f32, two channels at `RATE`.
fn ffmpeg_stereo(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-ac", "2", "-ar", "48000", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "ffmpeg decode failed");
    out.stdout
        .chunks(4)
        .map(|b| f32::from_le_bytes(b.try_into().expect("whole f32 samples")))
        .collect()
}

fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    let (mut num, mut da, mut db) = (0f64, 0f64, 0f64);
    for i in 0..n {
        let (x, y) = (f64::from(a[i]), f64::from(b[i]));
        num += x * y;
        da += x * x;
        db += y * y;
    }
    num / (da.sqrt() * db.sqrt()).max(1e-30)
}

#[test]
fn ps_in_mp4_decodes_to_stereo_like_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("SKIP: ffmpeg not installed");
        return;
    }
    let src = fixture(FIXTURE);
    assert!(src.exists(), "fixture {} is absent", src.display());
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let mp4 = dir.join("ps_48000_music.m4a");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&src)
        .args(["-c", "copy", "-f", "mp4"])
        .arg(&mp4)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg could not remux the PS fixture to mp4");

    let mut demux = Mp4Demuxer::new(BufReader::new(File::open(&mp4).expect("mp4 opens")))
        .expect("mp4 parses");
    let (idx, asc) = {
        let aac = demux
            .streams()
            .iter()
            .find(|s| s.params.codec == CodecId::Aac)
            .expect("mp4 has an AAC track");
        let asc = aac.params.extradata.as_ref().expect("esds carries the ASC").to_vec();
        (aac.index, asc)
    };
    let mut decoder = AacDecoder::with_config_bytes(&asc).expect("ASC parses");
    // The config is implicit HE-AACv2 (an LC config at the core rate), so the rate
    // is only known once the first frame has revealed the SBR/PS extension.

    let mut ours: Vec<f32> = Vec::new();
    let mut channels = 0u16;
    let mut rate = 0u32;
    while let Ok(Packet { stream, data, .. }) = demux.next_packet() {
        if stream != idx {
            continue;
        }
        let frame = decoder
            .decode(&data, None)
            .unwrap_or_else(|e| panic!("access unit failed to decode: {e:?}"));
        channels = frame.channels;
        rate = frame.sample_rate;
        ours.extend_from_slice(&frame.samples);
    }
    assert_eq!(channels, 2, "PS must decode to two channels");
    assert_eq!(rate, RATE, "PS must decode at the extension rate");

    let theirs = ffmpeg_stereo(&mp4);
    let n = ours.len().min(theirs.len());
    assert!(n > RATE as usize, "too few samples decoded: {n}");
    let corr = correlation(&ours[..n], &theirs[..n]);
    println!("ps mp4: ours {} samples, ffmpeg {} samples, corr {corr:.6}", ours.len(), theirs.len());
    assert!(corr >= 0.999, "PS in mp4 correlates {corr:.6} with ffmpeg, below 0.999");
}
