//! Byte-exactness against ffmpeg's `dvvideo` decoder (native pix_fmt) and its DV demuxer audio.
mod common;

use std::io::Cursor;

use common::*;
use ec_core::registry::{CodecId, Demuxer};
use ec_dv::{Chroma, DvDemuxer, DvVideoDecoder};

fn frames_of(path: &std::path::Path) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let data = std::fs::read(path).unwrap();
    let mut dm = DvDemuxer::new(Cursor::new(data)).unwrap();
    let n_streams = dm.streams().len();
    let mut video = Vec::new();
    let mut audio = vec![Vec::new(); n_streams - 1];
    while let Ok(p) = dm.next_packet() {
        if p.stream == 0 {
            video.push(p.data.to_vec());
        } else {
            audio[p.stream as usize - 1].extend_from_slice(&p.data);
        }
    }
    (video, audio)
}

fn native_bytes(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut dec = DvVideoDecoder::new();
    let mut out = Vec::new();
    for f in frames {
        let pic = dec.decode(f).expect("decode");
        for p in &pic.planes {
            out.extend_from_slice(p);
        }
    }
    out
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    if a.len() != b.len() {
        return Some(a.len().min(b.len()));
    }
    a.iter().zip(b).position(|(x, y)| x != y)
}

fn check_flavour(f: &Flavour, ilme: bool, noise: u32) {
    let tag = format!("p{}{}", u8::from(ilme), noise);
    let path = make(f, &tag, "dv", ilme, noise).unwrap_or_else(|| panic!("could not make {}", f.name));
    let (video, audio) = frames_of(&path);
    assert!(video.len() >= 2, "{}: frame count {}", f.name, video.len());
    let want = ffmpeg_raw_video(&path, &[]);
    let got = native_bytes(&video);
    if let Some(i) = first_diff(&got, &want) {
        panic!("{} ilme={ilme} noise={noise}: video differs at byte {i} (got {} want {} bytes)", f.name, got.len(), want.len());
    }
    if f.audio {
        assert!(!audio.is_empty(), "{}: no audio streams", f.name);
        for (n, a) in audio.iter().enumerate() {
            let w = ffmpeg_pcm(&path, n).expect("ffmpeg audio");
            let len = a.len().min(w.len());
            assert_eq!(&a[..len], &w[..len], "{}: audio stream {n} differs", f.name);
            assert!(len >= w.len().saturating_sub(8000), "{}: audio too short ({} vs {})", f.name, a.len(), w.len());
        }
    }
}

macro_rules! flavour_test {
    ($name:ident, $idx:expr) => {
        #[test]
        fn $name() {
            if !ffmpeg_or_skip() {
                return;
            }
            let f = &FLAVOURS[$idx];
            for (ilme, noise) in [(false, 0), (true, 6), (true, 25), (false, 40)] {
                check_flavour(f, ilme, noise);
            }
        }
    };
}

flavour_test!(ntsc_411, 0);
flavour_test!(pal_420, 1);
flavour_test!(pal_411, 2);
flavour_test!(ntsc_422_dvcpro50, 3);
flavour_test!(pal_422_dvcpro50, 4);
flavour_test!(hd_1080i50, 5);
flavour_test!(hd_1080i60, 6);
flavour_test!(hd_720p50, 7);
flavour_test!(hd_720p60, 8);

#[test]
fn native_formats() {
    if !ffmpeg_or_skip() {
        return;
    }
    for (i, want) in [Chroma::Yuv411, Chroma::Yuv420, Chroma::Yuv411, Chroma::Yuv422, Chroma::Yuv422].into_iter().enumerate() {
        let f = &FLAVOURS[i];
        let path = make(f, "fmt", "dv", false, 5).unwrap();
        let (video, _) = frames_of(&path);
        let pic = DvVideoDecoder::new().decode(&video[0]).unwrap();
        assert_eq!(pic.chroma, want, "{}", f.name);
    }
    let _ = CodecId::DvVideo;
}

/// Offsets (within a frame) of the 80-byte video DIF blocks.
fn video_blocks(frame_size: usize, difseg: usize) -> Vec<usize> {
    let seq_bytes = 150 * 80;
    let mut v = Vec::new();
    for s in 0..frame_size / seq_bytes {
        let _ = difseg;
        for b in 6..150 {
            if (b - 6) % 16 != 0 {
                v.push(s * seq_bytes + b * 80);
            }
        }
    }
    v
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Damaged and field-flagged streams: bit recovery across blocks, macroblocks and segments, the
/// retry/concealment path and the HD last-row field-mode placement all have to match ffmpeg.
fn check_mutated(f: &Flavour, mode: u32, seed: u64) {
    let path = make(f, &format!("m{mode}_{seed}"), "dv", true, 8).unwrap();
    let mut data = std::fs::read(&path).unwrap();
    let fs = ec_dv::frame_profile(None, &data).unwrap().frame_size;
    let blocks = video_blocks(fs, 0);
    let mut rng = Rng(seed);
    for frame in data.chunks_exact_mut(fs) {
        match mode {
            0 => {
                // set the DCT mode bit of every block (HD: block 0 selects field mode)
                for &b in &blocks {
                    frame[b + 5] |= 0x40;
                }
            }
            1 => {
                // sprinkle random bytes
                for _ in 0..40 {
                    let b = blocks[rng.next() as usize % blocks.len()];
                    frame[b + 4 + rng.next() as usize % 76] = rng.next() as u8;
                }
            }
            2 => {
                // flip single bits (usually just a changed coefficient, sometimes a lost EOB)
                for _ in 0..200 {
                    let b = blocks[rng.next() as usize % blocks.len()];
                    frame[b + 4 + rng.next() as usize % 76] ^= 1 << (rng.next() % 8);
                }
            }
            4 => {
                // random block status nibbles (error concealment path)
                for &b in &blocks {
                    if rng.next().is_multiple_of(11) {
                        frame[b + 3] = (frame[b + 3] & 0x0f) | ((rng.next() as u8 & 0x0f) << 4);
                    }
                }
            }
            _ => {
                // random QNO and class bits
                for &b in &blocks {
                    if rng.next().is_multiple_of(3) {
                        frame[b + 3] = (frame[b + 3] & 0xf0) | (rng.next() as u8 & 0x0f);
                    }
                }
            }
        }
    }
    std::fs::write(&path, &data).unwrap();
    let (video, _) = frames_of(&path);
    // `-cpuflags 0`: ffmpeg's C simple IDCT. Its SIMD versions agree on every sane coefficient
    // set but wrap differently on the absurd ones damaged streams produce.
    let want = ffmpeg_raw_video(&path, &["-cpuflags", "0"]);
    let got = native_bytes(&video);
    if let Some(i) = first_diff(&got, &want) {
        panic!("{} mode={mode} seed={seed}: differs at byte {i} (got {} want {})", f.name, got.len(), want.len());
    }
}

#[test]
fn mutated_streams_match_ffmpeg() {
    if !ffmpeg_or_skip() {
        return;
    }
    for f in FLAVOURS {
        for mode in 0..5 {
            for seed in [1u64, 2, 3] {
                check_mutated(f, mode, seed);
            }
        }
    }
}
