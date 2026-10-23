//! TrueHD (stream type 0x83) in a transport stream: every demuxed access unit decodes
//! through `ec-truehd` to the PCM ffmpeg decodes from the same file, sample for sample.
//!
//! The fixture is made at test time by ffmpeg's own TrueHD encoder (a 440 Hz sine,
//! lavfi, no external media). The test skips, saying so, when ffmpeg is absent and
//! fails instead when `EC_REQUIRE_FIXTURES` is set.

use std::path::Path;
use std::process::Command;

use crate::common::*;
use ec_core::registry::Demuxer;
use ec_core::{CodecId, MediaParameters};
use ec_truehd::{AccessUnitHeader, TrueHdDecoder};

/// Stereo 48 kHz TrueHD in M2TS-style TS, as ffmpeg writes it (stream type 0x83).
fn truehd_ts(name: &str, channels: &str) -> Option<std::path::PathBuf> {
    let a = [
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=2",
        "-ac",
        channels,
        "-ar",
        "48000",
        "-c:a",
        "truehd",
        "-strict",
        "-2",
        "-f",
        "mpegts",
    ];
    fixture(name, &a)
}

/// ffmpeg's decode of the first audio stream as interleaved S32 little-endian.
fn ffmpeg_s32le(path: &Path) -> Vec<i32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "s32le", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    s32le(&out.stdout)
}

fn s32le(bytes: &[u8]) -> Vec<i32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| i32::from_le_bytes(*c))
        .collect()
}

#[test]
fn truehd_in_ts_decodes_sample_exact_to_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    for (name, channels, want) in [("truehd_sine_2ch.ts", "2", 2usize)] {
        let Some(path) = truehd_ts(name, channels) else {
            return;
        };
        let mut d = open(&path);
        let all = read_all(&mut d);
        let stream = d
            .streams()
            .iter()
            .find(|s| s.params.codec == CodecId::TrueHd)
            .unwrap_or_else(|| panic!("{name}: no TrueHD stream exposed"))
            .clone();
        let pkts: Vec<Pkt> = all
            .into_iter()
            .filter(|p| p.stream == stream.index)
            .collect();
        assert!(!pkts.is_empty(), "{name}: no TrueHD access units");

        // Every packet is one whole access unit: its length word covers it exactly.
        for p in &pkts {
            let head = AccessUnitHeader::parse(&p.data)
                .unwrap_or_else(|e| panic!("{name}: AU at {:#x}: {e}", p.off));
            assert_eq!(head.length, p.data.len(), "{name}: AU length");
        }

        let mut dec = TrueHdDecoder::new();
        let mut ours = Vec::new();
        let mut channels_seen = 0;
        for p in &pkts {
            if let Some(f) = dec
                .decode_access_unit(&p.data)
                .unwrap_or_else(|e| panic!("{name}: decode: {e}"))
            {
                channels_seen = f.channels();
                ours.extend(s32le(&f.data[0]));
            }
        }
        let stats = dec.check_stats();
        assert_eq!(stats.lossless_check_failures, 0, "{name}: {stats:?}");
        assert_eq!(stats.parity_failures, 0, "{name}: {stats:?}");
        assert_eq!(stats.restart_crc_failures, 0, "{name}: {stats:?}");
        assert_eq!(channels_seen, want, "{name}: channels");
        let MediaParameters::Audio(a) = &stream.params.media else {
            panic!("{name}: TrueHD stream is not audio");
        };
        assert_eq!(a.sample_rate, 48_000, "{name}: rate");

        let reference = ffmpeg_s32le(&path);
        assert!(!ours.is_empty(), "{name}: no samples decoded");
        assert_eq!(ours.len(), reference.len(), "{name}: sample count");
        assert!(ours == reference, "{name}: PCM differs from ffmpeg");
    }
}

#[test]
fn truehd_packets_match_the_container_packet_form() {
    // The access units in the TS are the same bytes ffmpeg's stream copy writes out
    // (the MP4 and Matroska demuxers hand over the same form).
    if !have_ffmpeg() {
        return;
    }
    let Some(path) = truehd_ts("truehd_sine_2ch.ts", "2") else {
        return;
    };
    let mut d = open(&path);
    let all = read_all(&mut d);
    let stream = d
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::TrueHd)
        .expect("a TrueHD stream")
        .index;
    let ours: Vec<u8> = all
        .into_iter()
        .filter(|p| p.stream == stream)
        .flat_map(|p| p.data)
        .collect();
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&path)
        .args(["-map", "0:a:0", "-c", "copy", "-f", "truehd", "-"])
        .output()
        .unwrap();
    assert!(o.status.success());
    let theirs = o.stdout;
    assert!(!ours.is_empty() && !theirs.is_empty());
    // Our stream starts at the first complete access unit ffmpeg also starts at, so the
    // reference must contain ours as a suffix-aligned run.
    let first = ours[..8].to_vec();
    let at = theirs
        .windows(first.len())
        .position(|w| w == first.as_slice())
        .expect("ffmpeg's stream copy starts at one of our access units");
    assert_eq!(
        &theirs[at..at + ours.len().min(theirs.len() - at)],
        &ours[..ours.len().min(theirs.len() - at)],
        "access unit bytes differ from ffmpeg's stream copy"
    );
}

// ---------------------------------------------------------------------------
// AC-3 core interleaved in the TrueHD PID
// ---------------------------------------------------------------------------

/// Splits a raw TrueHD stream into its access units, each from its length word.
fn split_aus(raw: &[u8]) -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        let len = ec_truehd::frame_length(&raw[at..]).expect("an access-unit length word");
        v.push(raw[at..at + len].to_vec());
        at += len;
    }
    v
}

/// A 128-byte AC-3 syncframe (48 kHz, 64 words, bsid 8): a valid header, zero payload.
fn fake_ac3_frame() -> Vec<u8> {
    let mut f = vec![0u8; 128];
    f[0] = 0x0B;
    f[1] = 0x77;
    f[4] = 0x00; // fscod 0 (48 kHz), frmsizecod 0 (64 words)
    f[5] = 8 << 3; // bsid 8
    f
}

/// A minimal transport stream writer: one program, one PID per stream, PAT and PMT.
#[derive(Default)]
struct Ts {
    out: Vec<u8>,
    cc: std::collections::HashMap<u16, u8>,
}

impl Ts {
    /// One transport packet; a short payload is padded with adaptation-field stuffing.
    fn packet(&mut self, pid: u16, pusi: bool, payload: &[u8]) {
        let cc = self.cc.entry(pid).or_insert(0);
        let mut p = [0xFFu8; 188];
        p[0] = 0x47;
        p[1] = (u8::from(pusi) << 6) | ((pid >> 8) as u8 & 0x1F);
        p[2] = pid as u8;
        let n = payload.len();
        if n == 184 {
            p[3] = 0x10 | (*cc & 0x0F);
            p[4..].copy_from_slice(payload);
        } else {
            p[3] = 0x30 | (*cc & 0x0F);
            let af = 184 - n; // the adaptation field, length byte included
            p[4] = (af - 1) as u8;
            if af >= 2 {
                p[5] = 0;
                // p[6..4 + af] stay 0xFF stuffing
            }
            p[4 + af..].copy_from_slice(payload);
        }
        *cc = (*cc + 1) & 0x0F;
        self.out.extend_from_slice(&p);
    }

    /// A PSI section in one packet, with its CRC.
    fn psi(&mut self, pid: u16, table: u8, ext: u16, body: &[u8]) {
        let mut sec = vec![table, 0, 0];
        sec.extend_from_slice(&ext.to_be_bytes());
        sec.extend_from_slice(&[0xC1, 0, 0]);
        sec.extend_from_slice(body);
        let len = sec.len() - 3 + 4;
        sec[1] = 0xB0 | ((len >> 8) as u8 & 0x0F);
        sec[2] = len as u8;
        let crc = crc32(&sec);
        sec.extend_from_slice(&crc.to_be_bytes());
        let mut payload = vec![0u8];
        payload.extend_from_slice(&sec);
        self.packet(pid, true, &payload);
    }

    /// A PES packet with a PTS, unbounded length, split over transport packets.
    fn pes(&mut self, pid: u16, pts: u64, data: &[u8]) {
        let mut all = vec![0, 0, 1, 0xBD, 0, 0, 0x80, 0x80, 5];
        all.extend_from_slice(&ts5(2, pts));
        all.extend_from_slice(data);
        for (i, chunk) in all.chunks(184).enumerate() {
            self.packet(pid, i == 0, chunk);
        }
    }
}

fn ts5(prefix: u8, v: u64) -> [u8; 5] {
    [
        (prefix << 4) | ((v >> 29) & 0x0E) as u8 | 1,
        (v >> 22) as u8,
        ((v >> 14) & 0xFE) as u8 | 1,
        (v >> 7) as u8,
        ((v << 1) & 0xFE) as u8 | 1,
    ]
}

#[test]
fn ac3_core_interleaved_in_the_truehd_pid_is_dropped() {
    if !have_ffmpeg() {
        return;
    }
    let Some(raw) = fixture(
        "truehd_sine_2ch.thd",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-ac",
            "2",
            "-ar",
            "48000",
            "-c:a",
            "truehd",
            "-strict",
            "-2",
            "-f",
            "truehd",
        ],
    ) else {
        return;
    };
    let aus = split_aus(&std::fs::read(&raw).unwrap());
    assert!(aus.len() > 8, "enough access units");

    // PAT (program 1 at PMT PID 0x1000); PMT with TrueHD (0x83) on PID 0x1100.
    let mut ts = Ts::default();
    ts.psi(0, 0, 1, &[0x00, 0x01, 0xF0, 0x00]);
    let pmt = [
        0xF1, 0x00, // PCR PID 0x1100
        0xF0, 0x00, // program_info_length 0
        0x83, 0xF1, 0x00, // stream_type 0x83, elementary PID 0x1100
        0xF0, 0x00, // ES_info_length 0
    ];
    ts.psi(0x1000, 2, 1, &pmt);

    // Per PES: the AC-3 core and TrueHD access units alternate, four AUs per PES.
    let ac3 = fake_ac3_frame();
    for (i, chunk) in aus.chunks(4).enumerate() {
        let mut payload = Vec::new();
        for au in chunk {
            payload.extend_from_slice(&ac3);
            payload.extend_from_slice(au);
        }
        ts.pes(0x1100, 90_000 + (i * 4) as u64 * 75, &payload);
    }

    let path = fixture_dir().join("truehd_ac3_core.ts");
    std::fs::write(&path, &ts.out).unwrap();
    let mut d = open(&path);
    let all = read_all(&mut d);
    let stream = d
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::TrueHd)
        .expect("the TrueHD stream is exposed")
        .index;
    let pkts: Vec<&Pkt> = all.iter().filter(|p| p.stream == stream).collect();
    assert_eq!(
        pkts.len(),
        aus.len(),
        "one packet per access unit, no AC-3 frames"
    );
    for (k, (p, au)) in pkts.iter().zip(&aus).enumerate() {
        assert_eq!(p.data, *au, "access unit {k} bytes");
        // 40 samples at 48 kHz per access unit; the AC-3 core takes no time.
        assert_eq!(p.pts, Some(90_000 + k as i64 * 75), "access unit {k} pts");
    }
}
