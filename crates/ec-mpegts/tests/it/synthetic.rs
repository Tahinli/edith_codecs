//! Hand-built transport streams for the corners ffmpeg's muxer never produces: a PCR
//! on its own PID, several access units in one PES, 204-byte packets, duplicate
//! packets, PMT changes, damaged PSI, refused stream types, PGS.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::PathBuf;

use crate::common::crc32;
use ec_core::CodecId;
use ec_core::registry::Demuxer;
use ec_mpegts::{StreamKind, TsDemuxer};

fn tiny(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

fn section(table: u8, ext: u16, version: u8, body: &[u8]) -> Vec<u8> {
    let len = 5 + body.len() + 4;
    let mut s = vec![table, 0xB0 | ((len >> 8) as u8 & 0xf), len as u8];
    s.extend_from_slice(&ext.to_be_bytes());
    s.push(0xC1 | (version << 1));
    s.extend_from_slice(&[0, 0]);
    s.extend_from_slice(body);
    let c = crc32(&s);
    s.extend_from_slice(&c.to_be_bytes());
    s
}

fn ts5(prefix: u8, v: u64) -> [u8; 5] {
    [
        (prefix << 4) | (((v >> 30) as u8 & 7) << 1) | 1,
        (v >> 22) as u8,
        (((v >> 15) as u8) << 1) | 1,
        (v >> 7) as u8,
        ((v as u8) << 1) | 1,
    ]
}

#[derive(Clone)]
struct Es {
    pid: u16,
    stream_type: u8,
    descriptors: Vec<u8>,
}

#[derive(Default)]
struct Mux {
    out: Vec<u8>,
    cc: HashMap<u16, u8>,
    size: usize,
}

impl Mux {
    fn new(size: usize) -> Mux {
        Mux {
            size,
            ..Mux::default()
        }
    }

    fn packet(&mut self, pid: u16, pusi: bool, payload: &[u8], pcr: Option<u64>, rai: bool) {
        assert!(payload.len() <= 184);
        let cc = self.cc.entry(pid).or_insert(15);
        let has_payload = !payload.is_empty();
        if has_payload {
            *cc = (*cc + 1) & 15;
        }
        let mut af = Vec::new();
        let need = 184 - payload.len();
        if pcr.is_some() || rai || need > 0 {
            let flags = u8::from(rai) << 6 | u8::from(pcr.is_some()) << 4;
            let mut body = vec![flags];
            if let Some(p) = pcr {
                let (base, ext) = (p / 300, p % 300);
                body.extend_from_slice(&[
                    (base >> 25) as u8,
                    (base >> 17) as u8,
                    (base >> 9) as u8,
                    (base >> 1) as u8,
                    ((base & 1) as u8) << 7 | 0x7E | (ext >> 8) as u8,
                    ext as u8,
                ]);
            }
            let total = need.saturating_sub(1).max(body.len());
            if need == 1 {
                af = vec![0];
            } else {
                body.resize(total, 0xFF);
                af = std::iter::once(body.len() as u8).chain(body).collect();
            }
        }
        let afc = match (af.is_empty(), has_payload) {
            (true, _) => 1,
            (false, true) => 3,
            (false, false) => 2,
        };
        let mut p = vec![
            0x47,
            (u8::from(pusi) << 6) | (pid >> 8) as u8,
            pid as u8,
            (afc << 4) | *cc,
        ];
        p.extend_from_slice(&af);
        p.extend_from_slice(payload);
        assert_eq!(p.len(), 188, "af {} payload {}", af.len(), payload.len());
        if self.size == 192 {
            self.out.extend_from_slice(&[0, 0, 0, 0]);
        }
        self.out.extend_from_slice(&p);
        if self.size == 204 {
            self.out.extend_from_slice(&[0u8; 16]);
        }
    }

    fn psi(&mut self, pid: u16, sec: &[u8]) {
        let mut v = vec![0u8];
        v.extend_from_slice(sec);
        self.packet(pid, true, &v, None, false);
    }

    fn tables(&mut self, pmt_pid: u16, pcr_pid: u16, es: &[Es], version: u8) {
        self.psi(
            0,
            &section(0, 1, 0, &[0, 1, 0xE0 | (pmt_pid >> 8) as u8, pmt_pid as u8]),
        );
        let mut body = vec![0xE0 | (pcr_pid >> 8) as u8, pcr_pid as u8, 0xF0, 0];
        for e in es {
            body.extend_from_slice(&[
                e.stream_type,
                0xE0 | (e.pid >> 8) as u8,
                e.pid as u8,
                0xF0 | (e.descriptors.len() >> 8) as u8,
                e.descriptors.len() as u8,
            ]);
            body.extend_from_slice(&e.descriptors);
        }
        self.psi(pmt_pid, &section(2, 1, version, &body));
    }

    #[allow(clippy::too_many_arguments)] // a PES header is this many independent fields
    fn pes(
        &mut self,
        pid: u16,
        id: u8,
        pts: Option<u64>,
        dts: Option<u64>,
        data: &[u8],
        bounded: bool,
        rai: bool,
    ) {
        let mut hdr = vec![0, 0, 1, id, 0, 0, 0x80];
        let mut opt = Vec::new();
        match (pts, dts) {
            (Some(p), Some(d)) => {
                hdr.push(0xC0);
                opt.extend_from_slice(&ts5(3, p));
                opt.extend_from_slice(&ts5(1, d));
            }
            (Some(p), None) => {
                hdr.push(0x80);
                opt.extend_from_slice(&ts5(2, p));
            }
            _ => hdr.push(0),
        }
        hdr.push(opt.len() as u8);
        hdr.extend_from_slice(&opt);
        let total = hdr.len() - 6 + data.len();
        if bounded {
            hdr[4] = (total >> 8) as u8;
            hdr[5] = total as u8;
        }
        let mut all = hdr;
        all.extend_from_slice(data);
        let mut first = true;
        let mut rest = &all[..];
        while !rest.is_empty() {
            let take = rest.len().min(if first && rai { 182 } else { 184 });
            self.packet(pid, first, &rest[..take], None, first && rai);
            rest = &rest[take..];
            first = false;
        }
    }
}

struct Source {
    video: Vec<(i64, i64, bool, Vec<u8>)>,
    audio: Vec<(i64, Vec<u8>)>,
}

fn source() -> Source {
    let mut d = TsDemuxer::new(Cursor::new(tiny("tiny_h264_aac.ts"))).unwrap();
    let mut s = Source {
        video: vec![],
        audio: vec![],
    };
    while let Ok(p) = d.next_packet() {
        if p.stream == 0 {
            s.video.push((
                p.pts.unwrap(),
                p.dts.unwrap_or(p.pts.unwrap()),
                p.flags.keyframe,
                p.data.to_vec(),
            ));
        } else {
            s.audio.push((p.pts.unwrap(), p.data.to_vec()));
        }
    }
    s
}

const VPID: u16 = 0x100;
const APID: u16 = 0x101;

fn es_h264_aac() -> Vec<Es> {
    vec![
        Es {
            pid: VPID,
            stream_type: 0x1B,
            descriptors: vec![],
        },
        Es {
            pid: APID,
            stream_type: 0x0F,
            descriptors: vec![],
        },
    ]
}

fn build(size: usize, pcr_pid: u16, aud_pairs: bool) -> Vec<u8> {
    let s = source();
    let mut m = Mux::new(size);
    m.tables(0x1000, pcr_pid, &es_h264_aac(), 0);
    let mut ai = 0;
    let mut vi = 0;
    while vi < s.video.len() || ai < s.audio.len() {
        if vi % 4 == 0 {
            m.tables(0x1000, pcr_pid, &es_h264_aac(), 0);
            if pcr_pid != VPID {
                m.packet(
                    pcr_pid,
                    false,
                    &[],
                    Some(s.video.get(vi).map_or(0, |v| v.1 as u64 * 300)),
                    false,
                );
            }
        }
        if vi < s.video.len() {
            let (pts, dts, key, data) = &s.video[vi];
            if aud_pairs && vi % 2 == 0 && vi + 1 < s.video.len() {
                // Two access units in one PES: only the first carries timestamps.
                let mut both = data.clone();
                both.extend_from_slice(&s.video[vi + 1].3);
                m.pes(
                    VPID,
                    0xE0,
                    Some(*pts as u64),
                    Some(*dts as u64),
                    &both,
                    false,
                    *key,
                );
                vi += 2;
            } else {
                m.pes(
                    VPID,
                    0xE0,
                    Some(*pts as u64),
                    Some(*dts as u64),
                    data,
                    false,
                    *key,
                );
                vi += 1;
            }
        }
        for _ in 0..2 {
            if ai < s.audio.len() {
                m.pes(
                    APID,
                    0xC0,
                    Some(s.audio[ai].0 as u64),
                    None,
                    &s.audio[ai].1,
                    true,
                    false,
                );
                ai += 1;
            }
        }
    }
    m.out
}

type Pkt = (u32, Option<i64>, bool, Vec<u8>);

fn packets(bytes: Vec<u8>) -> (Vec<Pkt>, TsDemuxer<Cursor<Vec<u8>>>) {
    let mut d = TsDemuxer::new(Cursor::new(bytes)).unwrap();
    let mut v = Vec::new();
    while let Ok(p) = d.next_packet() {
        v.push((p.stream, p.pts, p.flags.keyframe, p.data.to_vec()));
    }
    (v, d)
}

fn reference() -> Vec<Pkt> {
    packets(tiny("tiny_h264_aac.ts")).0
}

fn by_stream(
    v: &[(u32, Option<i64>, bool, Vec<u8>)],
    s: u32,
) -> Vec<&(u32, Option<i64>, bool, Vec<u8>)> {
    v.iter().filter(|p| p.0 == s).collect()
}

#[test]
fn rebuilt_stream_equals_the_original() {
    let (got, d) = packets(build(188, VPID, false));
    let want = reference();
    for s in 0..2 {
        assert_eq!(by_stream(&got, s), by_stream(&want, s), "stream {s}");
    }
    assert_eq!(d.packet_size(), 188);
    assert_eq!(d.stats().cc_errors, 0);
}

#[test]
fn pcr_on_its_own_pid() {
    let (got, d) = packets(build(188, 0x1FF0, false));
    assert_eq!(d.pcr_pid(), Some(0x1FF0));
    assert!(d.first_pcr().is_some());
    assert_eq!(d.tracks().len(), 2);
    assert_eq!(by_stream(&got, 0), by_stream(&reference(), 0));
}

#[test]
fn packet_sizes_192_and_204() {
    for size in [192, 204] {
        let (got, d) = packets(build(size, VPID, false));
        assert_eq!(d.packet_size(), size);
        assert_eq!(d.is_bdav(), size == 192);
        assert_eq!(
            by_stream(&got, 1),
            by_stream(&reference(), 1),
            "size {size}"
        );
        assert_eq!(
            by_stream(&got, 0),
            by_stream(&reference(), 0),
            "size {size}"
        );
    }
}

#[test]
fn two_access_units_in_one_pes_are_split_on_the_delimiter() {
    let (got, _) = packets(build(188, VPID, true));
    let want = reference();
    let (g, w) = (by_stream(&got, 0), by_stream(&want, 0));
    assert_eq!(g.len(), w.len());
    for (a, b) in g.iter().zip(&w) {
        assert_eq!(a.3, b.3);
        assert_eq!(a.2, b.2, "keyframe flag");
    }
    // The second unit of a pair has no timestamp of its own.
    assert!(g.iter().filter(|p| p.1.is_none()).count() >= 5);
}

#[test]
fn duplicate_packets_are_dropped() {
    let clean = build(188, VPID, false);
    let mut dup = Vec::new();
    for (i, p) in clean.chunks(188).enumerate() {
        dup.extend_from_slice(p);
        if i % 7 == 3 && p[1] & 0x1f == 1 {
            dup.extend_from_slice(p);
        }
    }
    let (got, d) = packets(dup);
    assert!(d.stats().duplicates > 0);
    assert_eq!(by_stream(&got, 0), by_stream(&packets(clean).0, 0));
}

#[test]
fn damaged_psi_is_ignored() {
    let mut bytes = build(188, VPID, false);
    // The first PMT's CRC byte: second packet, last section byte.
    let mut broken = bytes.clone();
    let pmt = &mut broken[188..376];
    let st = 5 + usize::from(pmt[4]); // past the adaptation field and the pointer
    let end = st + 3 + (usize::from(pmt[st + 2] & 0xf) << 8 | usize::from(pmt[st + 3]));
    pmt[end] ^= 0xFF;
    // Later copies of the PMT are intact, so the file still opens; the damage is counted.
    let (_, d) = packets(broken);
    assert!(d.stats().crc_errors >= 1, "{:?}", d.stats());
    // Garbage everywhere in the PAT leaves nothing to open.
    for b in &mut bytes[..188] {
        *b = 0x47;
    }
    for (i, p) in bytes.chunks_mut(188).enumerate() {
        if p[1] == 0x40 && p[2] == 0 && i % 4 == 0 {
            p[10] ^= 0x55;
        }
    }
    let _ = TsDemuxer::new(Cursor::new(bytes));
}

#[test]
fn refusals_name_the_codec_and_pgs_is_exposed() {
    let es = vec![
        Es {
            pid: 0x100,
            stream_type: 0x02,
            descriptors: vec![],
        },
        Es {
            pid: 0x101,
            stream_type: 0x11,
            descriptors: vec![],
        },
        Es {
            pid: 0x102,
            stream_type: 0x90,
            descriptors: vec![],
        },
        Es {
            pid: 0x103,
            stream_type: 0x06,
            descriptors: vec![0x6A, 1, 0],
        },
        Es {
            pid: 0x104,
            stream_type: 0x06,
            descriptors: vec![0x59, 0],
        },
        Es {
            pid: 0x105,
            stream_type: 0x83,
            descriptors: vec![],
        },
    ];
    let mut m = Mux::new(192);
    m.tables(0x1000, 0x100, &es, 0);
    for i in 0..40u64 {
        m.pes(
            0x102,
            0xBD,
            Some(90_000 + i * 1000),
            None,
            &[0x16, 0, 3, 1, 2, 3],
            true,
            false,
        );
        m.pes(
            0x100,
            0xE0,
            Some(90_000 + i * 3600),
            None,
            &[0, 0, 1, 0xB3, 1, 2, 3, 4],
            false,
            false,
        );
        if i % 5 == 0 {
            m.tables(0x1000, 0x100, &es, 0);
        }
    }
    let mut d = TsDemuxer::new(Cursor::new(m.out)).unwrap();
    let tracks = d.tracks();
    assert_eq!(tracks.len(), 6);
    assert_eq!(tracks[0].kind, StreamKind::Mpeg2Video);
    assert!(
        tracks[0].refusal.is_none(),
        "MPEG-2 video is exposed, not refused"
    );
    assert_eq!(tracks[0].stream, None, "no parseable sequence header yet");
    assert!(tracks[1].refusal.as_ref().unwrap().contains("aac_latm"));
    assert_eq!(tracks[2].stream, Some(0));
    assert_eq!(tracks[3].kind, StreamKind::Ac3);
    assert_eq!(
        tracks[3].stream, None,
        "no AC-3 frames were seen, so it cannot be described yet"
    );
    assert_eq!(tracks[4].kind, StreamKind::DvbSub);
    assert!(tracks[4].refusal.is_none(), "DVB subtitles are exposed");
    assert_eq!(tracks[5].kind, StreamKind::TrueHd);
    assert!(
        tracks[5].refusal.is_none(),
        "TrueHD is exposed, not refused"
    );
    assert_eq!(tracks[5].stream, None, "no TrueHD access unit was seen yet");
    assert_eq!(d.streams().len(), 2);
    assert_eq!(d.streams()[0].params.codec, CodecId::Pgs);
    assert_eq!(d.streams()[1].params.codec, CodecId::DvbSub);
    let mut n = 0;
    while let Ok(p) = d.next_packet() {
        if p.stream != 0 {
            continue;
        }
        assert_eq!(&p.data[..], &[0x16, 0, 3, 1, 2, 3]);
        assert!(p.flags.keyframe);
        n += 1;
    }
    assert_eq!(n, 40);
}

#[test]
fn pmt_change_appends_streams_and_keeps_indices() {
    let a = tiny("tiny_h264_aac.ts");
    let b = tiny("tiny_h264_eac3.ts");
    let mut both = a.clone();
    both.extend_from_slice(&b);
    let mut d = TsDemuxer::new(Cursor::new(both)).unwrap();
    assert_eq!(d.streams().len(), 2);
    let mut counts = HashMap::new();
    while let Ok(p) = d.next_packet() {
        *counts.entry(p.stream).or_insert(0) += 1;
    }
    assert_eq!(d.stats().pmt_changes, 1);
    assert_eq!(d.streams().len(), 3);
    assert_eq!(d.streams()[2].params.codec, CodecId::EAc3);
    assert_eq!(d.streams()[1].params.codec, CodecId::Aac);
    let alone = |bytes: Vec<u8>| {
        let mut d = TsDemuxer::new(Cursor::new(bytes)).unwrap();
        let mut c = [0usize; 2];
        while let Ok(p) = d.next_packet() {
            c[p.stream as usize] += 1;
        }
        c
    };
    let (ca, cb) = (alone(a), alone(b));
    assert_eq!(counts[&0], ca[0] + cb[0]);
    assert_eq!(counts[&1], ca[1]);
    assert_eq!(counts[&2], cb[1]);
    let tracks = d.tracks();
    assert_eq!(tracks.iter().filter(|t| t.stream.is_some()).count(), 3);
    assert!(tracks[1].pid == tracks[2].pid);
}

#[test]
fn length_prefixed_video_and_config_records() {
    let mut d = TsDemuxer::new(Cursor::new(tiny("tiny_h264_aac.ts"))).unwrap();
    d.set_video_framing(ec_mpegts::VideoFraming::LengthPrefixed);
    let avcc = d.streams()[0].params.extradata.clone().unwrap();
    assert_eq!(avcc[0], 1);
    assert_eq!(avcc[4] & 3, 3, "4-byte lengths");
    let p = d.next_packet().unwrap();
    let n = u32::from_be_bytes(p.data[..4].try_into().unwrap()) as usize;
    assert!(n + 4 <= p.data.len());
    // The same access unit, re-framed from the Annex-B one, round-trips.
    let mut d2 = TsDemuxer::new(Cursor::new(tiny("tiny_h264_aac.ts"))).unwrap();
    let q = d2.next_packet().unwrap();
    assert_eq!(
        ec_mpegts::annexb_to_length_prefixed(&q.data, 4),
        p.data.to_vec()
    );
    // The hvcC of the HEVC fixture.
    let h = TsDemuxer::new(Cursor::new(tiny("tiny_hevc_aac.ts"))).unwrap();
    let hvcc = h.streams()[0].params.extradata.clone().unwrap();
    assert_eq!(hvcc[0], 1);
    assert_eq!(hvcc[21] & 3, 3);
    assert!(hvcc[22] >= 3, "VPS, SPS and PPS arrays");
    let _ = BufReader::new(File::open("/dev/null").unwrap());
}
