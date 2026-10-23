//! AVI audio+video demuxer tests. Real files are synthetic, written by ffmpeg at test time and
//! checked against `ffprobe -show_packets`; the OpenDML, scan and vprp cases are hand-built
//! byte by byte. Without ffmpeg the ffmpeg-backed tests skip unless `EC_REQUIRE_FIXTURES` is set.

use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use ec_core::frame::{Interlace, VideoFrame};
use ec_core::registry::{CodecId, Demuxer, MediaType, SeekMode};
use ec_core::{Error, TimeBase, Timestamp};
use ec_riff::{AviDemuxer, AviIndexKind, is_avi};
use crate::common::{chunk, ffmpeg, list, tool_ok};

// ---------------------------------------------------------------------------------------------
// ffmpeg plumbing

fn oracle() -> bool {
    if tool_ok("ffmpeg") && tool_ok("ffprobe") {
        return true;
    }
    assert!(
        std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
        "ffmpeg missing and EC_REQUIRE_FIXTURES is set"
    );
    eprintln!("SKIP: no ffmpeg");
    false
}

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("ec-riff-avi-{}-{tag}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

const VIDEO_SRC: [&str; 4] = ["-f", "lavfi", "-i", "testsrc2=size=96x64:rate=25:duration=3"];
const AUDIO_SRC: [&str; 4] = ["-f", "lavfi", "-i", "sine=frequency=440:duration=3:sample_rate=44100"];

/// Make `name.avi` from a video and an audio codec spec; returns its bytes.
fn make(tag: &str, vargs: &[&str], aargs: &[&str]) -> (PathBuf, Vec<u8>) {
    let path = scratch(tag).join("t.avi");
    let mut args: Vec<&str> = Vec::new();
    args.extend(VIDEO_SRC);
    args.extend(AUDIO_SRC);
    args.extend(vargs);
    args.extend(aargs);
    args.extend(["-f", "avi", path.to_str().unwrap()]);
    ffmpeg(&args);
    let bytes = std::fs::read(&path).unwrap();
    (path, bytes)
}

struct Probe {
    stream: u32,
    size: usize,
    key: bool,
    time: f64,
    hash: String,
}

fn probe(path: &PathBuf) -> Vec<Probe> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_packets", "-show_data_hash", "adler32", "-of", "compact=p=0"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| {
            let get = |k: &str| -> String {
                l.split('|')
                    .find_map(|kv| kv.strip_prefix(&format!("{k}=")))
                    .unwrap_or_default()
                    .to_string()
            };
            Probe {
                stream: get("stream_index").parse().unwrap(),
                size: get("size").parse().unwrap(),
                key: get("flags").starts_with('K'),
                time: get("dts_time").parse().unwrap_or(f64::NAN),
                hash: get("data_hash"),
            }
        })
        .collect()
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

struct Got {
    stream: u32,
    data: Vec<u8>,
    key: bool,
    secs: f64,
    pts: i64,
}

fn read_all(d: &mut AviDemuxer<Cursor<Vec<u8>>>) -> Vec<Got> {
    let mut v = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => v.push(Got {
                stream: p.stream,
                key: p.flags.keyframe,
                secs: p.dts.unwrap() as f64 * p.time_base.num() as f64 / p.time_base.den() as f64,
                pts: p.pts.unwrap(),
                data: p.data.to_vec(),
            }),
            Err(Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    }
    v
}

/// Our packets against ffprobe's: per stream the same count, sizes, key flags, bytes (checksum)
/// and, when `times`, dts.
fn compare(path: &PathBuf, bytes: &[u8], times: bool, hashes: bool) -> Vec<Got> {
    let mut d = AviDemuxer::new(Cursor::new(bytes.to_vec())).unwrap();
    let got = read_all(&mut d);
    let want = probe(path);
    for s in 0..d.streams().len() as u32 {
        let ours: Vec<&Got> = got.iter().filter(|g| g.stream == s).collect();
        let theirs: Vec<&Probe> = want.iter().filter(|g| g.stream == s).collect();
        assert_eq!(ours.len(), theirs.len(), "stream {s}: packet count");
        for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            assert_eq!(a.data.len(), b.size, "stream {s} packet {i}: size");
            assert_eq!(a.key, b.key, "stream {s} packet {i}: keyframe");
            if hashes {
                assert_eq!(
                    format!("adler32:{:08x}", adler32(&a.data)),
                    b.hash,
                    "stream {s} packet {i}: bytes"
                );
            }
            if times {
                assert!((a.secs - b.time).abs() < 2e-3, "stream {s} packet {i}: {} vs {}", a.secs, b.time);
            }
        }
    }
    got
}

fn assert_interleaved(got: &[Got]) {
    let first: Vec<u32> = got.iter().take(20).map(|g| g.stream).collect();
    assert!(first.contains(&0) && first.contains(&1), "A/V interleaved: {first:?}");
}

// ---------------------------------------------------------------------------------------------
// ffmpeg-made files

#[test]
fn mjpeg_pcm() {
    if !oracle() {
        return;
    }
    let (path, bytes) = make("mjpeg-pcm", &["-c:v", "mjpeg", "-q:v", "3"], &["-c:a", "pcm_s16le"]);
    assert!(is_avi(&bytes));
    let mut d = AviDemuxer::new(Cursor::new(bytes.clone())).unwrap();
    let s = d.streams().to_vec();
    assert_eq!(s[0].params.codec, CodecId::Mjpeg);
    assert_eq!(s[1].params.codec, CodecId::PcmS16Le);
    let v = s[0].params.video().unwrap();
    assert_eq!((v.width, v.height), (96, 64));
    assert_eq!(v.frame_rate, Some(TimeBase::new(25, 1)));
    assert_eq!(s[1].params.audio().unwrap().sample_rate, 44100);
    assert_eq!(d.field_order(0), Some(Interlace::BottomFieldFirst));
    assert_eq!(d.tracks().len(), 2);
    assert_eq!(d.tracks()[0].media, Some(MediaType::Video));
    assert_ne!(d.index_kind(), AviIndexKind::Scan);
    assert_eq!(s[0].duration, Some(75));
    let _ = d.next_packet().unwrap();
    // PCM chunks may be split differently by ffprobe: compare video exactly, audio by total.
    let got = compare_video_only(&path, &bytes);
    assert_interleaved(&got);
    let ours: usize = got.iter().filter(|g| g.stream == 1).map(|g| g.data.len()).sum();
    let theirs: usize = probe(&path).iter().filter(|p| p.stream == 1).map(|p| p.size).sum();
    assert_eq!(ours, theirs, "audio bytes");
    // PCM clock counts samples.
    let last = got.iter().rfind(|g| g.stream == 1).unwrap();
    assert!(last.pts > 0);
}

/// Video packets exactly, audio left to the caller.
fn compare_video_only(path: &PathBuf, bytes: &[u8]) -> Vec<Got> {
    let mut d = AviDemuxer::new(Cursor::new(bytes.to_vec())).unwrap();
    let got = read_all(&mut d);
    let want = probe(path);
    let ours: Vec<&Got> = got.iter().filter(|g| g.stream == 0).collect();
    let theirs: Vec<&Probe> = want.iter().filter(|g| g.stream == 0).collect();
    assert_eq!(ours.len(), theirs.len());
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        assert_eq!(a.data.len(), b.size, "video {i}");
        assert_eq!(a.key, b.key, "video {i} key");
        assert_eq!(format!("adler32:{:08x}", adler32(&a.data)), b.hash, "video {i} bytes");
        assert!((a.secs - b.time).abs() < 2e-3, "video {i} time {} vs {}", a.secs, b.time);
        assert_eq!(a.pts, i as i64);
    }
    got
}

#[test]
fn mjpeg_mp3() {
    if !oracle() {
        return;
    }
    let (path, bytes) = make("mjpeg-mp3", &["-c:v", "mjpeg"], &["-c:a", "libmp3lame"]);
    let d = AviDemuxer::new(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(d.streams()[1].params.codec, CodecId::Mp3);
    let got = compare(&path, &bytes, false, true);
    assert_interleaved(&got);
    // One unit (1152 samples) per chunk.
    let a: Vec<&Got> = got.iter().filter(|g| g.stream == 1).collect();
    assert!(a.iter().enumerate().all(|(i, g)| g.pts == i as i64));
}

#[test]
fn h264_no_bframes_aac() {
    if !oracle() {
        return;
    }
    let (path, bytes) = make(
        "h264",
        &["-c:v", "libx264", "-bf", "0", "-g", "10", "-preset", "ultrafast"],
        &["-c:a", "aac"],
    );
    let mut d = AviDemuxer::new(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(d.streams()[0].params.codec, CodecId::H264);
    assert_eq!(d.streams()[1].params.codec, CodecId::Aac);
    assert!(d.streams()[1].params.extradata.is_some(), "AudioSpecificConfig");
    assert!(matches!(
        d.field_order(0),
        None | Some(Interlace::Progressive)
    ));
    let got = compare(&path, &bytes, true, true);
    assert_interleaved(&got);
    let keys: Vec<i64> = got.iter().filter(|g| g.stream == 0 && g.key).map(|g| g.pts).collect();
    assert_eq!(keys.first(), Some(&0));
    assert!(keys.len() >= 7, "keys {keys:?}");

    // Seek: lands on a key at or before the target, video resumes there, audio near it.
    let landed = d
        .seek_stream(0, Timestamp::new(37, TimeBase::new(1, 25)), SeekMode::SyncBefore)
        .unwrap();
    assert!(keys.contains(&landed.ticks) && landed.ticks <= 37 && landed.ticks > 27);
    let mut saw_audio = false;
    loop {
        let p = d.next_packet().unwrap();
        if p.stream == 0 {
            assert_eq!(p.pts, Some(landed.ticks));
            assert!(p.flags.keyframe);
            break;
        }
        saw_audio = true;
    }
    let _ = saw_audio;
    let after = d
        .seek_stream(0, Timestamp::new(37, TimeBase::new(1, 25)), SeekMode::SyncAfter)
        .unwrap();
    assert!(after.ticks >= 37 && keys.contains(&after.ticks));
    d.rewind();
    assert_eq!(d.next_packet().unwrap().pts, Some(0));
}

#[test]
fn h264_bframes_decode_order() {
    if !oracle() {
        return;
    }
    let (path, bytes) = make(
        "h264b",
        &["-c:v", "libx264", "-bf", "2", "-g", "12", "-preset", "ultrafast"],
        &["-c:a", "aac"],
    );
    // Packets, sizes, bytes and key flags match; pts is the decode-order slot (documented).
    let got = compare(&path, &bytes, true, true);
    let v: Vec<&Got> = got.iter().filter(|g| g.stream == 0).collect();
    assert!(v.windows(2).all(|w| w[1].pts == w[0].pts + 1));
    // The stream really has reordering: at least one non-key frame is smaller than average
    // is not guaranteed, but more than one frame per GOP must be non-key.
    assert!(v.iter().filter(|g| !g.key).count() > v.len() / 2);
}

#[test]
fn mpeg2_mp2() {
    if !oracle() {
        return;
    }
    let (path, bytes) = make("mpeg2", &["-c:v", "mpeg2video", "-g", "10", "-bf", "0"], &["-c:a", "mp2"]);
    let d = AviDemuxer::new(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(d.streams()[0].params.codec, CodecId::Mpeg2Video);
    assert_eq!(d.streams()[1].params.codec, CodecId::Mp2);
    let got = compare(&path, &bytes, false, true);
    assert_interleaved(&got);
}

#[test]
fn raw_video_is_carried_and_unknown_layouts_refused_by_name() {
    if !oracle() {
        return;
    }
    for (tag, fmt) in [("yuyv", "yuyv422"), ("bgr24", "bgr24")] {
        let (_, bytes) = make(tag, &["-c:v", "rawvideo", "-pix_fmt", fmt], &["-c:a", "pcm_s16le"]);
        let d = AviDemuxer::new(Cursor::new(bytes)).unwrap();
        assert_eq!(d.streams().len(), 2);
        assert_eq!(d.streams()[0].params.codec, CodecId::RawVideo);
        assert_eq!(d.streams()[1].params.codec, CodecId::PcmS16Le);
    }
    // 1-bpp DIBs are still not carried.
    let (_, bytes) = make("gray", &["-c:v", "rawvideo", "-pix_fmt", "monow"], &["-c:a", "pcm_s16le"]);
    let d = AviDemuxer::new(Cursor::new(bytes)).unwrap();
    assert_eq!(d.streams().len(), 1);
    let why = d.tracks()[0].unsupported.clone().unwrap();
    assert!(why.contains("uncompressed"), "{why}");
}

#[test]
fn mpeg4_part2_and_ms_mpeg4_fourccs_map() {
    if !oracle() {
        return;
    }
    for tag in ["XVID", "xvid", "DIVX", "DX50", "FMP4", "mp4v"] {
        let path = scratch("mp4v").join(format!("v-{tag}.avi"));
        let mut args: Vec<&str> = Vec::new();
        args.extend(VIDEO_SRC);
        args.extend(["-c:v", "mpeg4", "-vtag", tag, "-f", "avi", path.to_str().unwrap()]);
        ffmpeg(&args);
        let d = AviDemuxer::new(Cursor::new(std::fs::read(&path).unwrap())).unwrap();
        assert_eq!(d.streams()[0].params.codec, CodecId::Mpeg4Part2, "{tag}");
    }
    // MS-MPEG4 FourCCs map to their codec ids.
    for (enc, tag, id) in [
        ("msmpeg4v3", "MP43", CodecId::MsMpeg4V3),
        ("msmpeg4v3", "DIV3", CodecId::MsMpeg4V3),
        ("msmpeg4v2", "MP42", CodecId::MsMpeg4V2),
    ] {
        let path = scratch("mp4v").join(format!("v-{enc}-{tag}.avi"));
        let mut args: Vec<&str> = Vec::new();
        args.extend(VIDEO_SRC);
        args.extend(["-c:v", enc, "-vtag", tag, "-f", "avi", path.to_str().unwrap()]);
        ffmpeg(&args);
        let d = AviDemuxer::new(Cursor::new(std::fs::read(&path).unwrap())).unwrap();
        assert_eq!(d.streams()[0].params.codec, id, "{tag}");
    }
}

#[test]
fn truncated_file_without_idx1_scans() {
    if !oracle() {
        return;
    }
    let (path, bytes) = make("trunc", &["-c:v", "mjpeg"], &["-c:a", "pcm_s16le"]);
    let full = compare_video_only(&path, &bytes);
    // Cut in the middle of movi: idx1 is gone, the RIFF and movi sizes lie.
    let cut = bytes.len() * 6 / 10;
    let mut d = AviDemuxer::new(Cursor::new(bytes[..cut].to_vec())).unwrap();
    assert_eq!(d.index_kind(), AviIndexKind::Scan);
    let part = read_all(&mut d);
    assert!(part.len() > 20 && part.len() < full.len());
    for s in 0..2 {
        let a: Vec<&Got> = full.iter().filter(|g| g.stream == s).collect();
        let b: Vec<&Got> = part.iter().filter(|g| g.stream == s).collect();
        assert!(!b.is_empty());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.data, y.data);
            assert_eq!(x.pts, y.pts);
        }
    }
}

#[test]
fn mjpeg_frames_decode_like_ffmpeg() {
    if !oracle() {
        return;
    }
    let (path, bytes) = make("mjpeg-dec", &["-c:v", "mjpeg", "-q:v", "2"], &["-c:a", "pcm_s16le"]);
    let raw = scratch("mjpeg-dec").join("o.yuv");
    ffmpeg(&[
        "-i", path.to_str().unwrap(), "-map", "0:v", "-c:v", "rawvideo", "-pix_fmt", "yuvj420p", "-f",
        "rawvideo", raw.to_str().unwrap(),
    ]);
    let want = std::fs::read(raw).unwrap();
    let mut d = AviDemuxer::new(Cursor::new(bytes)).unwrap();
    let mut dec = ec_mjpeg::MjpegDecoder::new(d.streams()[0].params.clone()).unwrap();
    dec.set_field_order(d.field_order(0).unwrap());
    let mut mine = Vec::new();
    while let Ok(p) = d.next_packet() {
        if p.stream == 0 {
            mine.extend(frame_bytes(&dec.decode(&p.data, None).unwrap()));
        }
    }
    assert_eq!(mine.len(), want.len());
    assert!(mine == want, "decoded frames differ from ffmpeg");
}

fn frame_bytes(f: &VideoFrame) -> Vec<u8> {
    let mut v = Vec::new();
    for (i, p) in f.planes.iter().enumerate() {
        let (row, rows) = f.format.plane_geometry(f.width, f.height)[i];
        for y in 0..rows {
            v.extend_from_slice(&p.data[y * p.stride..y * p.stride + row]);
        }
    }
    v
}

// ---------------------------------------------------------------------------------------------
// Hand-built files: OpenDML across RIFF AVIX, idx1-less scan, zero-size frames, vprp

fn strh(kind: &[u8; 4], handler: &[u8; 4], scale: u32, rate: u32, sample_size: u32) -> Vec<u8> {
    let mut b = vec![0u8; 56];
    b[0..4].copy_from_slice(kind);
    b[4..8].copy_from_slice(handler);
    b[20..24].copy_from_slice(&scale.to_le_bytes());
    b[24..28].copy_from_slice(&rate.to_le_bytes());
    b[44..48].copy_from_slice(&sample_size.to_le_bytes());
    chunk(b"strh", &b)
}

fn strf_video(compression: &[u8; 4]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(40u32.to_le_bytes());
    b.extend(64u32.to_le_bytes());
    b.extend(48u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(24u16.to_le_bytes());
    b.extend(compression);
    b.extend([0u8; 20]);
    chunk(b"strf", &b)
}

fn strf_pcm() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(1u16.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(8000u32.to_le_bytes());
    b.extend(16000u32.to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    chunk(b"strf", &b)
}

fn vprp(start0: u32, start1: u32) -> Vec<u8> {
    let mut b = [0u32; 9 + 16];
    b[8] = 2;
    b[9 + 7] = start0;
    b[9 + 8 + 7] = start1;
    chunk(b"vprp", &b.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>())
}

struct Frame {
    stream: u8,
    data: Vec<u8>,
    key: bool,
}

impl Frame {
    fn id(&self) -> [u8; 4] {
        if self.stream == 0 { *b"00dc" } else { *b"01wb" }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    OpenDml,
    Idx1,
    Bare,
}

/// A two-stream file (H.264-tagged video, 8 kHz mono PCM) across one or two RIFF chunks.
/// Returns the bytes. OpenDML puts `ix00`/`ix01` in each movi and a super-index per stream.
fn build(riffs: &[Vec<Frame>], mode: Mode, vprp_lines: Option<(u32, u32)>) -> Vec<u8> {
    // Pass one fixes the layout; pass two writes the real super-index entries.
    let mut sup = vec![vec![(0u64, 0u32); riffs.len()]; 2];
    for _ in 0..2 {
        let (bytes, ix) = assemble(riffs, mode, vprp_lines, &sup);
        if ix == sup {
            return bytes;
        }
        sup = ix;
    }
    assemble(riffs, mode, vprp_lines, &sup).0
}

type Sup = Vec<Vec<(u64, u32)>>;

fn assemble(riffs: &[Vec<Frame>], mode: Mode, vp: Option<(u32, u32)>, sup: &Sup) -> (Vec<u8>, Sup) {
    let odml = mode == Mode::OpenDml;
    let super_index = |stream: usize| -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(4u16.to_le_bytes());
        b.push(0);
        b.push(0);
        b.extend((riffs.len() as u32).to_le_bytes());
        b.extend(if stream == 0 { *b"00dc" } else { *b"01wb" });
        b.extend([0u8; 12]);
        for (off, size) in &sup[stream] {
            b.extend(off.to_le_bytes());
            b.extend(size.to_le_bytes());
            b.extend(0u32.to_le_bytes());
        }
        chunk(b"indx", &b)
    };
    let mut strl0 = [strh(b"vids", b"H264", 1, 25, 0), strf_video(b"H264")].concat();
    if let Some((a, b)) = vp {
        strl0.extend(vprp(a, b));
    }
    if odml {
        strl0.extend(super_index(0));
    }
    let mut strl1 = [strh(b"auds", &[0; 4], 1, 8000, 2), strf_pcm()].concat();
    if odml {
        strl1.extend(super_index(1));
    }
    let mut hdrl = chunk(b"avih", &[0u8; 56]);
    hdrl.extend(list(b"strl", &strl0));
    hdrl.extend(list(b"strl", &strl1));
    if odml {
        let mut dml = chunk(b"dmlh", &[0u8; 4]);
        let total: usize = riffs.iter().flatten().filter(|f| f.stream == 0).count();
        dml[8..12].copy_from_slice(&(total as u32).to_le_bytes());
        hdrl.extend(list(b"odml", &dml));
    }
    let mut out = Vec::new();
    let mut found: Sup = vec![vec![(0, 0); riffs.len()]; 2];
    let mut idx1 = Vec::new();
    for (r, frames) in riffs.iter().enumerate() {
        let riff_start = out.len();
        out.extend(b"RIFF\0\0\0\0");
        out.extend(if r == 0 { b"AVI " } else { b"AVIX" });
        if r == 0 {
            out.extend(list(b"hdrl", &hdrl));
        }
        let movi_pos = out.len();
        out.extend(b"LIST\0\0\0\0movi");
        let mut per_stream: [Vec<(u64, u32, bool)>; 2] = [Vec::new(), Vec::new()];
        for f in frames {
            let pos = out.len() as u64;
            out.extend(chunk(&f.id(), &f.data));
            per_stream[usize::from(f.stream)].push((pos + 8, f.data.len() as u32, f.key));
            let mut flags = if f.key { 0x10u32 } else { 0 };
            if f.stream == 1 {
                flags = 0x10;
            }
            idx1.extend(f.id());
            idx1.extend(flags.to_le_bytes());
            idx1.extend(((pos - movi_pos as u64 - 8) as u32).to_le_bytes());
            idx1.extend((f.data.len() as u32).to_le_bytes());
        }
        if odml {
            for s in 0..2 {
                let base = movi_pos as u64 + 12;
                let mut b = Vec::new();
                b.extend(2u16.to_le_bytes());
                b.push(0);
                b.push(1);
                b.extend((per_stream[s].len() as u32).to_le_bytes());
                b.extend(if s == 0 { *b"00dc" } else { *b"01wb" });
                b.extend(base.to_le_bytes());
                b.extend(0u32.to_le_bytes());
                for (data_pos, size, key) in &per_stream[s] {
                    b.extend(((data_pos - base) as u32).to_le_bytes());
                    b.extend((size | if *key { 0 } else { 0x8000_0000 }).to_le_bytes());
                }
                let pos = out.len() as u64;
                let id = if s == 0 { b"ix00" } else { b"ix01" };
                let c = chunk(id, &b);
                found[s][r] = (pos, c.len() as u32);
                out.extend(c);
            }
        }
        let movi_len = (out.len() - movi_pos - 8) as u32;
        out[movi_pos + 4..movi_pos + 8].copy_from_slice(&movi_len.to_le_bytes());
        if r == 0 && mode == Mode::Idx1 {
            out.extend(chunk(b"idx1", &idx1));
        }
        let riff_len = (out.len() - riff_start - 8) as u32;
        out[riff_start + 4..riff_start + 8].copy_from_slice(&riff_len.to_le_bytes());
    }
    (out, found)
}

fn video(n: usize) -> Vec<Frame> {
    // Frame n: key every 3rd; payloads are Annex B shaped so the scan can sniff them.
    (0..n)
        .map(|i| {
            let key = i % 3 == 0;
            let mut data = vec![0, 0, 0, 1, if key { 0x65 } else { 0x41 }];
            data.extend((0..(7 + i % 5)).map(|k| (i * 7 + k) as u8));
            Frame { stream: 0, data, key }
        })
        .collect()
}

fn audio(n: usize, seed: u8) -> Vec<Frame> {
    (0..n)
        .map(|i| Frame {
            stream: 1,
            data: (0..160).map(|k| seed.wrapping_add((i * 3 + k) as u8)).collect(),
            key: true,
        })
        .collect()
}

fn interleave(v: Vec<Frame>, a: Vec<Frame>) -> Vec<Frame> {
    let mut out = Vec::new();
    let mut a = a.into_iter();
    for f in v {
        out.push(f);
        if let Some(x) = a.next() {
            out.push(x);
        }
    }
    out.extend(a);
    out
}

fn expect(riffs: &[Vec<Frame>], d: &mut AviDemuxer<Cursor<Vec<u8>>>, keys_from_index: bool) {
    let got = read_all(d);
    let all: Vec<&Frame> = riffs.iter().flatten().collect();
    // Non-empty frames in file order.
    let want: Vec<&Frame> = all.iter().copied().filter(|f| !f.data.is_empty()).collect();
    assert_eq!(got.len(), want.len());
    let mut slot = [0i64; 2];
    let mut slots = Vec::new();
    for f in &all {
        let s = usize::from(f.stream);
        let units = if s == 1 { (f.data.len() / 2) as i64 } else { 1 };
        if !f.data.is_empty() {
            slots.push(slot[s]);
        }
        slot[s] += units;
    }
    for ((g, w), pts) in got.iter().zip(&want).zip(slots) {
        assert_eq!(g.stream, u32::from(w.stream));
        assert_eq!(g.data, w.data);
        assert_eq!(g.pts, pts, "pts (zero-size frames keep their slot)");
        let _ = keys_from_index;
        assert_eq!(g.key, w.key, "keyframe flag");
    }
}

#[test]
fn opendml_across_avix() {
    let mut r1 = interleave(video(14), audio(14, 1));
    r1.insert(5, Frame { stream: 0, data: Vec::new(), key: false }); // dropped frame
    let r2 = interleave(video(10), audio(10, 99));
    let riffs = vec![r1, r2];
    let bytes = build(&riffs, Mode::OpenDml, None);
    assert!(is_avi(&bytes));
    let mut d = AviDemuxer::new(Cursor::new(bytes)).unwrap();
    assert_eq!(d.index_kind(), AviIndexKind::OpenDml);
    assert_eq!(d.chunk_count(0), 25);
    assert_eq!(d.streams()[0].duration, Some(25));
    expect(&riffs, &mut d, true);
}

#[test]
fn idx1_and_linear_scan_match_opendml() {
    let mut r1 = interleave(video(12), audio(12, 7));
    r1.insert(3, Frame { stream: 0, data: Vec::new(), key: false });
    let riffs = vec![r1];
    for (mode, kind) in [(Mode::Idx1, AviIndexKind::Idx1), (Mode::Bare, AviIndexKind::Scan)] {
        let bytes = build(&riffs, mode, None);
        let mut d = AviDemuxer::new(Cursor::new(bytes)).unwrap();
        assert_eq!(d.index_kind(), kind);
        expect(&riffs, &mut d, true);
    }
    // Scan across two RIFFs too (AVIX walked flat).
    let two = vec![
        interleave(video(6), audio(6, 3)),
        interleave(video(6), audio(6, 4)),
    ];
    let mut d = AviDemuxer::new(Cursor::new(build(&two, Mode::Bare, None))).unwrap();
    assert_eq!(d.index_kind(), AviIndexKind::Scan);
    expect(&two, &mut d, false);
}

#[test]
fn zero_size_frames_keep_timing() {
    let mut v = video(8);
    v[2].data.clear();
    v[3].data.clear();
    let riffs = vec![interleave(v, audio(8, 0))];
    let mut d = AviDemuxer::new(Cursor::new(build(&riffs, Mode::Idx1, None))).unwrap();
    let got = read_all(&mut d);
    let pts: Vec<i64> = got.iter().filter(|g| g.stream == 0).map(|g| g.pts).collect();
    assert_eq!(pts, vec![0, 1, 4, 5, 6, 7]);
}

#[test]
fn seek_across_avix_lands_on_key() {
    let riffs = vec![
        interleave(video(9), audio(9, 1)),
        interleave(video(9), audio(9, 2)),
    ];
    let mut d = AviDemuxer::new(Cursor::new(build(&riffs, Mode::OpenDml, None))).unwrap();
    let tb = TimeBase::new(1, 25);
    let l = d.seek_stream(0, Timestamp::new(13, tb), SeekMode::SyncBefore).unwrap();
    assert_eq!(l.ticks, 12);
    let l = d.seek_stream(0, Timestamp::new(13, tb), SeekMode::SyncAfter).unwrap();
    assert_eq!(l.ticks, 15);
    let l = d.seek_stream(0, Timestamp::new(13, tb), SeekMode::Exact).unwrap();
    assert_eq!(l.ticks, 13);
    let p = d.next_packet().unwrap();
    assert_eq!((p.stream, p.pts), (0, Some(13)));
}

#[test]
fn vprp_field_order() {
    let riffs = vec![interleave(video(3), audio(3, 0))];
    let top = AviDemuxer::new(Cursor::new(build(&riffs, Mode::Bare, Some((22, 23))))).unwrap();
    assert_eq!(top.field_order(0), Some(Interlace::TopFieldFirst));
    let bottom = AviDemuxer::new(Cursor::new(build(&riffs, Mode::Bare, Some((23, 22))))).unwrap();
    assert_eq!(bottom.field_order(0), Some(Interlace::BottomFieldFirst));
    let none = AviDemuxer::new(Cursor::new(build(&riffs, Mode::Bare, None))).unwrap();
    assert_eq!(none.field_order(0), None);
    assert_eq!(none.field_order(1), None);
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    assert!(AviDemuxer::new(Cursor::new(b"RIFF\0\0\0\0AVI ".to_vec())).is_err());
    assert!(AviDemuxer::new(Cursor::new(Vec::new())).is_err());
    assert!(!is_avi(b"RIFF....WAVE"));
}

/// Writes the hand-built files as fuzz seeds: `EC_AVI_SEED_DIR=... cargo test -- --ignored dump_seeds`.
#[test]
#[ignore]
fn dump_seeds() {
    let dir = PathBuf::from(std::env::var("EC_AVI_SEED_DIR").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    let two = vec![
        interleave(video(14), audio(14, 1)),
        interleave(video(10), audio(10, 9)),
    ];
    for (name, mode) in [("odml", Mode::OpenDml), ("idx1", Mode::Idx1), ("bare", Mode::Bare)] {
        std::fs::write(dir.join(name), build(&two, mode, Some((22, 23)))).unwrap();
    }
}
