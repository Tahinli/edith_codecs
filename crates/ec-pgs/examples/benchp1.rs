//! benchp1: PGS (.sup) decode throughput for lanes/bench.md ("lane benchp1").
//!
//! usage:
//!   benchp1 gen <out.sup> <seconds>   write a synthetic .sup: every second one display set
//!                                     shows a 64x32 object, 0.8 s later a clear set follows
//!                                     (2 display sets per second, 90 kHz pts).
//!   benchp1 decode <file.sup> [reps]  decode the whole file `reps` times (default 5) with a
//!                                     fresh decoder each time; print the best time, MB/s, the
//!                                     rendered frame count and the stream duration from the pts.
//!
//! The generator uses the same segment layout as the crate's tests (`tests/fuzz_regressions.rs`).
//! ffmpeg has no PGS encoder, so this is the input source for the row.
use std::hint::black_box;
use std::time::Instant;

use ec_pgs::{PgsDecoder, SEG_END, SEG_ODS, SEG_PCS, SEG_PDS};

fn segment(pts: u32, kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = b"PG".to_vec();
    out.extend_from_slice(&pts.to_be_bytes());
    out.extend_from_slice(&pts.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// A 1920x1080 epoch-start composition with `objects` placed objects (0 or 1).
fn pcs(pts: u32, x: u16, y: u16, objects: u8) -> Vec<u8> {
    let mut body = vec![0x07, 0x80, 0x04, 0x38, 0x10, 0, 0, 0x80, 0, 0, objects];
    if objects == 1 {
        body.extend_from_slice(&[0, 0, 0, 0]);
        body.extend_from_slice(&x.to_be_bytes());
        body.extend_from_slice(&y.to_be_bytes());
    }
    segment(pts, SEG_PCS, &body)
}

/// Palette entry 1 opaque white.
fn pds(pts: u32) -> Vec<u8> {
    segment(pts, SEG_PDS, &[0, 0, 1, 235, 128, 128, 255])
}

/// One 64x32 object, RLE: each row is 64 pixels of colour 1 then an end-of-line code.
fn ods(pts: u32) -> Vec<u8> {
    let (w, h) = (64u16, 32u16);
    let mut rle = Vec::new();
    for _ in 0..h {
        rle.extend(std::iter::repeat_n(1u8, w as usize));
        rle.extend_from_slice(&[0, 0]);
    }
    let mut body = vec![0, 0, 0, 0xC0];
    body.extend_from_slice(&((rle.len() + 4) as u32).to_be_bytes()[1..]);
    body.extend_from_slice(&w.to_be_bytes());
    body.extend_from_slice(&h.to_be_bytes());
    body.extend_from_slice(&rle);
    segment(pts, SEG_ODS, &body)
}

fn generate(path: &str, seconds: u32) {
    let mut out = Vec::new();
    for s in 0..seconds {
        let t = s * 90_000;
        let x = ((s * 37) % 1800) as u16;
        let y = ((s * 19) % 1000) as u16;
        out.extend(pcs(t, x, y, 1));
        out.extend(pds(t));
        out.extend(ods(t));
        out.extend(segment(t, SEG_END, &[]));
        let c = t + 72_000;
        out.extend(pcs(c, 0, 0, 0));
        out.extend(segment(c, SEG_END, &[]));
    }
    std::fs::write(path, &out).expect("write sup");
    println!("wrote {path} bytes={} display_sets={}", out.len(), 2 * seconds);
}

/// (segment count, the last segment's pts in seconds), walking the segment headers.
fn scan(bytes: &[u8]) -> (usize, f64) {
    let mut pos = 0;
    let mut last = 0u32;
    let mut segments = 0;
    while pos + 13 <= bytes.len() {
        last = u32::from_be_bytes(bytes[pos + 2..pos + 6].try_into().unwrap());
        let size = u16::from_be_bytes([bytes[pos + 11], bytes[pos + 12]]) as usize;
        pos += 13 + size;
        segments += 1;
    }
    (segments, f64::from(last) / 90_000.0)
}

/// One full decode with a fresh decoder: (rendered frames, push errors). The file is fed one
/// segment at a time and the finished frames are dropped after each push, as a streaming reader
/// would. A full 1920x1080 canvas is about 8 MB, so draining per segment keeps the decoder's
/// output queue at most one frame deep.
fn decode_once(bytes: &[u8]) -> (usize, usize) {
    let mut d = PgsDecoder::new();
    let (mut frames, mut errors) = (0, 0);
    let mut pos = 0;
    while pos + 13 <= bytes.len() {
        let size = u16::from_be_bytes([bytes[pos + 11], bytes[pos + 12]]) as usize;
        let end = (pos + 13 + size).min(bytes.len());
        if d.push(black_box(&bytes[pos..end])).is_err() {
            errors += 1;
        }
        pos = end;
        while let Some(f) = d.take_frame() {
            frames += 1;
            black_box(f);
        }
    }
    (frames, errors)
}

fn decode(path: &str, reps: usize) {
    let bytes = std::fs::read(path).expect("read sup");
    let (frames, errors) = decode_once(&bytes);
    let mut best = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        black_box(decode_once(black_box(&bytes)));
        best = best.min(t.elapsed().as_secs_f64());
    }
    let (segments, dur) = scan(&bytes);
    println!(
        "bytes={} segments={segments} frames={frames} push_errors={errors} duration_s={dur:.1} best_decode_s={best:.4} mb_s={:.2} reps={reps}",
        bytes.len(),
        bytes.len() as f64 / best / 1e6
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen") => generate(&args[2], args[3].parse().expect("seconds")),
        Some("decode") => decode(&args[2], args.get(3).map_or(5, |s| s.parse().expect("reps"))),
        _ => panic!("usage: benchp1 gen <out.sup> <seconds> | decode <file.sup> [reps]"),
    }
}
