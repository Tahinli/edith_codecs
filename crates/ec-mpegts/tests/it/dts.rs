//! DTS in MPEG-TS: stream type 0x82 (and the DTS-HD types) come out as
//! `CodecId::Dts`, with the core's rate and channels, one packet per core frame
//! and the payload byte-identical to what ffmpeg copies out.

use crate::common::*;
use ec_core::CodecId;
use ec_core::registry::{Demuxer, MediaParameters};
use ec_mpegts::StreamKind;

fn dts_ts(name: &str, channels: &str) -> Option<std::path::PathBuf> {
    fixture(
        name,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=0.5",
            "-ac",
            channels,
            "-c:a",
            "dca",
            "-strict",
            "-2",
            "-f",
            "mpegts",
        ],
    )
}

fn check(name: &str, channels: &str, want: usize) {
    let Some(p) = dts_ts(name, channels) else {
        return;
    };
    let mut d = open(&p);
    let streams = d.streams().to_vec();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].params.codec, CodecId::Dts);
    let MediaParameters::Audio(a) = &streams[0].params.media else {
        panic!("not audio");
    };
    assert_eq!(a.sample_rate, 48000);
    assert_eq!(a.layout.channel_count(), want);
    assert_eq!(d.tracks()[0].kind, StreamKind::Dts);
    let pkts = read_all(&mut d);
    assert!(pkts.len() > 20, "{} packets", pkts.len());
    let ours: Vec<u8> = pkts.iter().flat_map(|p| p.data.clone()).collect();
    let theirs = ffmpeg_bytes(&p, 0);
    assert_eq!(ours, theirs, "payload differs from ffmpeg's copy");
    // Timestamps advance by one core frame (512 samples at 48 kHz) each.
    let pts: Vec<i64> = pkts.iter().filter_map(|p| p.pts).collect();
    assert!(pts.windows(2).all(|w| w[1] - w[0] == 90_000 * 512 / 48_000));
}

#[test]
fn dts_stereo_in_ts() {
    check("dts_stereo.ts", "2", 2);
}

#[test]
fn dts_51_in_ts() {
    check("dts_51.ts", "6", 6);
}

/// Rewrites the (single-section) PMT of a one-stream transport stream so its
/// elementary stream has `stream_type` and `descriptors`.
fn rewrite_pmt(ts: &[u8], stream_type: u8, descriptors: &[u8]) -> Vec<u8> {
    let mut out = ts.to_vec();
    let pmt_pid = u16::from_be_bytes([ts[188 + 1] & 0x1F, ts[188 + 2]]);
    let _ = pmt_pid;
    // PAT is the first packet, the PMT the second.
    let pkt = &mut out[188..376];
    assert_eq!(pkt[1] & 0x40, 0x40, "PMT starts a payload");
    let body = &pkt[5..];
    let sec_len = (usize::from(body[1] & 0x0F) << 8) | usize::from(body[2]);
    let old = &body[..3 + sec_len];
    // table header(8) | pcr pid(2) | program info len(2) + info | ES loop | crc
    let pi = (usize::from(old[10] & 0x0F) << 8) | usize::from(old[11]);
    let es = 12 + pi;
    let es_pid = [old[es + 1], old[es + 2]];
    let mut sec = old[..es].to_vec();
    sec.extend_from_slice(&[stream_type, es_pid[0], es_pid[1]]);
    sec.extend_from_slice(&[
        0xF0 | (descriptors.len() >> 8) as u8,
        descriptors.len() as u8,
    ]);
    sec.extend_from_slice(descriptors);
    let len = sec.len() - 3 + 4;
    sec[1] = (sec[1] & 0xF0) | (len >> 8) as u8;
    sec[2] = len as u8;
    let crc = crc32(&sec);
    sec.extend_from_slice(&crc.to_be_bytes());
    assert!(sec.len() <= 183);
    let at = 5;
    pkt[at..at + sec.len()].copy_from_slice(&sec);
    pkt[at + sec.len()..].fill(0xFF);
    out
}

#[test]
fn dts_hd_stream_types_and_descriptors_map_to_dts() {
    let Some(p) = dts_ts("dts_retag.ts", "2") else {
        return;
    };
    let ts = std::fs::read(&p).unwrap();
    let cases: [(&str, u8, Vec<u8>); 5] = [
        ("0x82", 0x82, vec![]),
        ("0x85 DTS-HD", 0x85, vec![]),
        ("0x86 DTS-HD MA", 0x86, vec![]),
        (
            "private + DTS1 registration",
            0x06,
            vec![0x05, 4, b'D', b'T', b'S', b'1'],
        ),
        ("private + DTS descriptor 0x7B", 0x06, vec![0x7B, 2, 0, 0]),
    ];
    for (what, stream_type, desc) in cases {
        let bytes = rewrite_pmt(&ts, stream_type, &desc);
        let mut d = ec_mpegts::TsDemuxer::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(d.streams().len(), 1, "{what}");
        assert_eq!(d.streams()[0].params.codec, CodecId::Dts, "{what}");
        assert!(d.next_packet().is_ok(), "{what}");
    }
}
