//! The committed tiny program-stream fixtures (`tests/data/ps_*`, made by
//! `data/gen_ps.sh` from synthetic sources) against the hashes of ffmpeg's own bytes in
//! `expected_ps.txt`. No ffmpeg needed.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;

use crate::common::*;
use ec_core::registry::Demuxer;
use ec_mpegts::{PsDemuxer, PsKind, is_mpeg_video_es, is_mpegps, is_mpegts};

#[test]
fn bytes_match_ffmpeg() {
    let text = std::fs::read_to_string(data().join("expected_ps.txt")).unwrap();
    let mut by_file: BTreeMap<&str, Vec<(usize, usize, usize, u64)>> = BTreeMap::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split(' ').collect();
        by_file.entry(f[0]).or_default().push((
            f[1].parse().unwrap(),
            f[2].parse().unwrap(),
            f[3].parse().unwrap(),
            u64::from_str_radix(f[4], 16).unwrap(),
        ));
    }
    assert_eq!(by_file.len(), 4);
    for (file, rows) in by_file {
        let mut d = PsDemuxer::new(BufReader::new(File::open(data().join(file)).unwrap())).unwrap();
        let mut per: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
        let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
        while let Ok(p) = d.next_packet() {
            per.entry(p.stream).or_default().extend_from_slice(&p.data);
            *counts.entry(p.stream).or_default() += 1;
        }
        // ffmpeg's index 0 is the video, 1 the audio.
        let video = d
            .streams()
            .iter()
            .find(|s| s.params.video().is_some())
            .unwrap()
            .index;
        let audio = d
            .streams()
            .iter()
            .find(|s| s.params.audio().is_some())
            .map(|s| s.index);
        assert_eq!(d.streams().len(), rows.len(), "{file}");
        assert_eq!(d.kind() == PsKind::VideoElementary, file.ends_with(".m2v"));
        for (idx, packets, bytes, hash) in rows {
            let si = if idx == 0 { video } else { audio.unwrap() };
            // DVD LPCM packets are one per PES; ffmpeg's count is its own framing.
            if !file.contains("pcm_dvd") || idx == 0 {
                assert_eq!(counts[&si], packets, "{file} stream {idx} packets");
            }
            let got = &per[&si];
            assert_eq!(got.len(), bytes, "{file} stream {idx} bytes");
            assert_eq!(fnv(got.iter().copied()), hash, "{file} stream {idx} hash");
        }
        assert!(d.duration_secs().unwrap() > 0.2, "{file}");
    }
}

#[test]
fn sniffing() {
    for f in ["ps_mpeg2_ac3.vob", "ps_mpeg1_mp2.mpg", "ps_pcm_dvd.vob"] {
        let b = std::fs::read(data().join(f)).unwrap();
        assert!(is_mpegps(&b[..4096]), "{f}");
        assert!(!is_mpeg_video_es(&b[..4096]), "{f}");
        assert!(!is_mpegts(&b[..4096]), "{f}");
    }
    let m2v = std::fs::read(data().join("ps_mpeg2.m2v")).unwrap();
    assert!(is_mpeg_video_es(&m2v[..4096]));
    assert!(!is_mpegps(&m2v[..4096]));
    let ts = std::fs::read(data().join("tiny_h264_aac.ts")).unwrap();
    assert!(!is_mpegps(&ts[..4096]) && !is_mpeg_video_es(&ts[..4096]));
    for junk in [&[][..], b"RIFFxxxxWAVEfmt ", &[0, 0, 1, 0xBA], &[0u8; 5000]] {
        assert!(!is_mpegps(junk) && !is_mpeg_video_es(junk));
    }
}
