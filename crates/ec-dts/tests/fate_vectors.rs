#![allow(clippy::chunks_exact_to_as_chunks, clippy::manual_is_multiple_of)]
//! The public FFmpeg FATE DTS samples (the dcadec conformance suite) against
//! ffmpeg's integer decoder, sample for sample: core 5.1, XCH (6.1), X96 (96
//! kHz, alone and with XCH), the core inside XBR and XLL (DTS-HD) streams and
//! a DTS-ES elementary stream.
//!
//! The samples are not committed. Fetch them with
//! `devscripts/fate.sh <dir>` and point `EC_DTS_FATE` at the directory
//! (default `fixtures/audio/dts-fate` at the repository root). A missing
//! directory or ffmpeg skips loudly; the committed fixtures in
//! `ffmpeg_exact.rs` are the gate that fails on absence.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_dts::DtsDecoder;

fn dir() -> PathBuf {
    match std::env::var("EC_DTS_FATE") {
        Ok(p) => PathBuf::from(p),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio/dts-fate"),
    }
}

/// The STRMDATA chunk of a `.dtshd` file, or the whole buffer.
fn strmdata(buf: &[u8]) -> &[u8] {
    if !buf.starts_with(b"DTSHDHDR") {
        return buf;
    }
    let mut p = 0;
    while p + 16 <= buf.len() {
        let size = u64::from_be_bytes(buf[p + 8..p + 16].try_into().unwrap()) as usize;
        if &buf[p..p + 8] == b"STRMDATA" {
            return &buf[p + 16..(p + 16 + size).min(buf.len())];
        }
        p += 16 + size;
    }
    buf
}

fn decode_ours(data: &[u8]) -> (Vec<i32>, usize, u32) {
    let mut dec = DtsDecoder::new();
    let (mut out, mut channels, mut rate) = (Vec::new(), 0, 0);
    let mut pos = 0;
    while pos + 4 < data.len() {
        let Some((s, _)) = ec_dts::find_sync(&data[pos..]) else {
            break;
        };
        pos += s;
        let Ok(h) = ec_dts::parse_frame_header(&data[pos..]) else {
            pos += 1;
            continue;
        };
        let end = ec_dts::find_sync(&data[pos + h.stream_frame_size.min(data.len() - pos)..])
            .map(|(o, _)| pos + h.stream_frame_size + o)
            .unwrap_or(data.len());
        let a = dec.decode_frame(&data[pos..end]).expect("decode");
        channels = a.layout.channel_count();
        rate = a.rate;
        for c in a.data[0].chunks_exact(4) {
            let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            out.push((v as f64 * 8388608.0).round() as i32);
        }
        pos = end;
    }
    (out, channels, rate)
}

fn decode_ffmpeg(path: &Path, core_only: bool) -> Vec<i32> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-flags", "+bitexact"]);
    if core_only {
        cmd.args(["-core_only", "1"]);
    }
    let out = cmd.arg("-i").arg(path).args(["-f", "s32le", "-"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) >> 8)
        .collect()
}

#[test]
fn fate_vectors_are_bit_exact_against_ffmpeg() {
    let d = dir();
    let ffmpeg = Command::new("ffmpeg").arg("-version").output().is_ok();
    if !d.is_dir() || !ffmpeg {
        eprintln!("skipping: FATE samples ({}) or ffmpeg missing", d.display());
        return;
    }
    // (file, ffmpeg -core_only, leading samples ffmpeg trims from .dtshd)
    let cases = [
        ("core_51_24_48_768_0.dtshd", true, 1024),
        ("core_51_24_48_768_1.dtshd", true, 1024),
        ("xch_61_24_48_768.dtshd", false, 1024),
        ("x96_51_24_96_1509.dtshd", false, 2048),
        ("x96_xch_61_24_96_3840.dtshd", false, 2048),
        ("xbr_51_24_48_3840.dtshd", true, 1024),
        ("xll_51_24_48_768.dtshd", true, 1024),
        ("dts_es.dts", false, 0),
    ];
    let mut ran = 0;
    for (name, core_only, skip) in cases {
        let path = d.join(name);
        let Ok(raw) = std::fs::read(&path) else {
            eprintln!("skipping {name}: not present");
            continue;
        };
        let (ours, ch, rate) = decode_ours(strmdata(&raw));
        let reference = decode_ffmpeg(&path, core_only);
        // ours is front-to-back; ffmpeg puts the back centre of a 6.1 stream
        // before the side surrounds.
        let perm: Vec<usize> = if ch == 7 { vec![0, 1, 2, 3, 6, 4, 5] } else { (0..ch).collect() };
        let ours: Vec<i32> = ours
            .chunks_exact(ch)
            .skip(skip)
            .flat_map(|f| perm.iter().map(|&p| f[p]).collect::<Vec<_>>())
            .collect();
        assert_eq!(ours.len(), reference.len(), "{name}: sample count");
        let bad = ours.iter().zip(&reference).position(|(a, b)| a != b);
        assert_eq!(bad, None, "{name}: first mismatch at {bad:?}");
        eprintln!("{name}: {ch} ch {rate} Hz {} samples/ch bit-exact", ours.len() / ch);
        ran += 1;
    }
    eprintln!("{ran} FATE vectors compared");
}
