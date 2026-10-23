//! The committed tiny fixtures (`tests/data`, made by `data/gen.sh` from synthetic
//! sources) against the hashes of ffmpeg's own bytes in `expected.txt`. No ffmpeg
//! needed.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;

use crate::common::*;
use ec_core::registry::Demuxer;
use ec_mpegts::TsDemuxer;

#[test]
fn bytes_match_ffmpeg() {
    let text = std::fs::read_to_string(data().join("expected.txt")).unwrap();
    let mut by_file: BTreeMap<&str, Vec<(u32, usize, usize, u64)>> = BTreeMap::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split(' ').collect();
        by_file.entry(f[0]).or_default().push((
            f[1].parse().unwrap(),
            f[2].parse().unwrap(),
            f[3].parse().unwrap(),
            u64::from_str_radix(f[4], 16).unwrap(),
        ));
    }
    assert_eq!(by_file.len(), 5);
    for (file, rows) in by_file {
        let mut d = TsDemuxer::new(BufReader::new(File::open(data().join(file)).unwrap())).unwrap();
        let mut per: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
        let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
        while let Ok(p) = d.next_packet() {
            per.entry(p.stream).or_default().extend_from_slice(&p.data);
            *counts.entry(p.stream).or_default() += 1;
        }
        assert_eq!(d.streams().len(), 2, "{file}");
        for (idx, packets, bytes, hash) in rows {
            // ffmpeg's second audio packet count for LPCM is its own framing; the
            // demuxer's is one packet per PES.
            if !file.contains("lpcm") || idx == 0 {
                assert_eq!(counts[&idx], packets, "{file} stream {idx} packets");
            }
            let got = &per[&idx];
            assert_eq!(got.len(), bytes, "{file} stream {idx} bytes");
            assert_eq!(fnv(got.iter().copied()), hash, "{file} stream {idx} hash");
        }
        assert!(d.duration_secs().unwrap() > 0.2, "{file}");
    }
}

#[test]
fn m2ts_is_detected_and_probed() {
    let head = std::fs::read(data().join("tiny_h264_ac3.m2ts")).unwrap();
    assert!(ec_mpegts::is_mpegts(&head[..4096]));
    let d = TsDemuxer::new(BufReader::new(
        File::open(data().join("tiny_h264_ac3.m2ts")).unwrap(),
    ))
    .unwrap();
    assert_eq!((d.packet_size(), d.is_bdav()), (192, true));
    assert!(!ec_mpegts::is_mpegts(b"RIFFxxxxWAVEfmt "));
    assert!(!ec_mpegts::is_mpegts(&[]));
}
