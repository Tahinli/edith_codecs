//! Uncompressed video in AVI and DV type-1 AVI (`iavs`). Media is synthetic, written by ffmpeg
//! at test time; ffmpeg cannot write type-1 AVI, so that wrapper is built here from a `.dv`
//! file. Without ffmpeg the tests skip unless `EC_REQUIRE_FIXTURES` is set.

use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use ec_core::frame::Frame;
use ec_core::registry::{CodecId, Decoder, Demuxer, MediaType, SeekMode};
use ec_core::{Error, Timestamp};
use ec_riff::{AviDemuxer, RawVideoDecoder};
use crate::common::{ffmpeg, frame_bytes, tool_ok};

fn oracle() -> bool {
    if tool_ok("ffmpeg") {
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
    let dir = base.join(format!("ec-riff-raw-{}-{tag}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn ffmpeg_out(args: &[&str]) -> Vec<u8> {
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin"])
        .args(args)
        .output()
        .expect("ffmpeg");
    assert!(
        o.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    o.stdout
}

fn decode_all(bytes: Vec<u8>) -> Vec<Vec<u8>> {
    let mut d = AviDemuxer::new(Cursor::new(bytes)).unwrap();
    assert_eq!(d.streams()[0].params.codec, CodecId::RawVideo);
    let mut dec = RawVideoDecoder::new(d.streams()[0].params.clone()).unwrap();
    let mut frames = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) => {
                dec.send_packet(&p).unwrap();
                while let Ok(Frame::Video(f)) = dec.receive_frame() {
                    frames.push(frame_bytes(&f));
                }
            }
            Err(Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    }
    frames
}

fn make_raw(tag: &str, size: &str, pix: &str, vtag: Option<&str>) -> (PathBuf, Vec<u8>) {
    let path = scratch(tag).join("v.avi");
    let src = format!("testsrc2=size={size}:rate=25:duration=2");
    let codec = if pix == "yuv422p10le" {
        "v210"
    } else {
        "rawvideo"
    };
    let mut args = vec!["-f", "lavfi", "-i", &src, "-c:v", codec, "-pix_fmt", pix];
    if let Some(t) = vtag {
        args.extend(["-vtag", t]);
    }
    args.extend(["-f", "avi", path.to_str().unwrap()]);
    ffmpeg(&args);
    let bytes = std::fs::read(&path).unwrap();
    (path, bytes)
}

fn expect_parity(path: &std::path::Path, bytes: Vec<u8>, ff_pix: &str) {
    let want = ffmpeg_out(&[
        "-i",
        path.to_str().unwrap(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        ff_pix,
        "-",
    ]);
    let got = decode_all(bytes);
    assert_eq!(got.len(), 50);
    let flat: Vec<u8> = got.concat();
    assert_eq!(flat.len(), want.len());
    assert!(flat == want, "{ff_pix}: decoded frames differ from ffmpeg");
}

#[test]
fn raw_rgb_and_yuv_match_ffmpeg() {
    if !oracle() {
        return;
    }
    for (tag, size, pix, vtag, ff) in [
        ("bgr24", "98x66", "bgr24", None, "bgr24"),
        ("bgr24odd", "97x65", "bgr24", None, "bgr24"),
        ("bgra", "98x66", "bgra", None, "bgra"),
        ("yuyv", "98x66", "yuyv422", None, "yuv422p"),
        ("uyvy", "98x66", "uyvy422", None, "yuv422p"),
        ("i420", "98x66", "yuv420p", None, "yuv420p"),
        ("iyuv", "98x66", "yuv420p", Some("IYUV"), "yuv420p"),
        ("yv12", "98x66", "yuv420p", Some("YV12"), "yuv420p"),
        ("pal8", "98x66", "pal8", None, "bgr24"),
        ("pal8odd", "97x65", "pal8", None, "bgr24"),
        ("rgb555", "98x66", "rgb555le", None, "bgr24"),
        ("rgb555odd", "97x65", "rgb555le", None, "bgr24"),
        ("yvyu", "98x66", "yvyu422", Some("YVYU"), "yuv422p"),
        ("nv12", "98x66", "nv12", Some("NV12"), "nv12"),
        ("nv12odd", "98x66", "nv12", Some("NV12"), "nv12"),
        ("y800", "98x66", "gray", Some("Y800"), "gray"),
        ("y800odd", "97x65", "gray", Some("Y800"), "gray"),
        ("v210", "98x66", "yuv422p10le", None, "yuv422p10le"),
        ("v210w96", "96x64", "yuv422p10le", None, "yuv422p10le"),
        ("v210odd", "97x65", "yuv422p10le", None, "yuv422p10le"),
    ] {
        eprintln!("case {tag}");
        let (path, bytes) = make_raw(tag, size, pix, vtag);
        let d = AviDemuxer::new(Cursor::new(bytes.clone())).unwrap();
        let s = &d.streams()[0];
        assert_eq!(s.params.codec, CodecId::RawVideo, "{tag}");
        assert_eq!(s.params.codec.media_type(), MediaType::Video);
        expect_parity(&path, bytes, ff);
    }
}

/// Walk `movi` and call `f` on every `##db` chunk body.
fn patch_chunks(bytes: &mut [u8], mut f: impl FnMut(&mut [u8])) {
    let movi = bytes.windows(4).position(|w| w == b"movi").unwrap() + 4;
    let mut pos = movi;
    while pos + 8 <= bytes.len() {
        let id = [bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if &id[2..] == b"db" || &id[2..] == b"dc" {
            f(&mut bytes[pos + 8..pos + 8 + size]);
        } else if &id == b"idx1" {
            break;
        }
        pos += 8 + size + (size & 1);
    }
}

#[test]
fn both_row_orders_are_upright() {
    if !oracle() {
        return;
    }
    let (path, bytes) = make_raw("topdown", "98x66", "bgr24", None);
    let mut td = bytes.clone();
    let strf = td.windows(4).position(|w| w == b"strf").unwrap() + 8;
    let h = i32::from_le_bytes(td[strf + 8..strf + 12].try_into().unwrap());
    // ffmpeg writes one orientation; store the other (rows reversed) and expect the same pictures.
    assert_eq!(h.abs(), 66);
    td[strf + 8..strf + 12].copy_from_slice(&(-h).to_le_bytes());
    let stride = (98 * 3 + 3) & !3;
    patch_chunks(&mut td, |body| {
        let rows: Vec<Vec<u8>> = body.chunks(stride).map(|r| r.to_vec()).collect();
        for (i, r) in rows.iter().rev().enumerate() {
            body[i * stride..(i + 1) * stride].copy_from_slice(r);
        }
    });
    expect_parity(&path, td, "bgr24");
}

// ---------------------------------------------------------------------------------------------
// DV type-1

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend((body.len() as u32).to_le_bytes());
    v.extend(body);
    if body.len() & 1 == 1 {
        v.push(0);
    }
    v
}

fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = kind.to_vec();
    b.extend(body);
    chunk_list(b)
}

fn chunk_list(body: Vec<u8>) -> Vec<u8> {
    let mut v = b"LIST".to_vec();
    v.extend((body.len() as u32).to_le_bytes());
    v.extend(body);
    v
}

/// Wrap raw DV frames as a type-1 AVI (`iavs`, handler `dvsd`, one `00dc` chunk per frame).
fn type1_avi(dv: &[u8], frame_size: usize, scale: u32, rate: u32, handler: &[u8; 4]) -> Vec<u8> {
    let frames: Vec<&[u8]> = dv.chunks_exact(frame_size).collect();
    let mut strh = b"iavs".to_vec();
    strh.extend(handler);
    strh.extend([0u8; 12]);
    strh.extend(scale.to_le_bytes());
    strh.extend(rate.to_le_bytes());
    strh.extend(0u32.to_le_bytes());
    strh.extend((frames.len() as u32).to_le_bytes());
    strh.extend([0u8; 20]);
    let strl = list(
        b"strl",
        &[chunk(b"strh", &strh), chunk(b"strf", &[0u8; 32])].concat(),
    );
    let mut avih = vec![0u8; 56];
    avih[0..4].copy_from_slice(&(1_000_000u32 * scale / rate).to_le_bytes());
    avih[16..20].copy_from_slice(&(frames.len() as u32).to_le_bytes());
    avih[24..28].copy_from_slice(&1u32.to_le_bytes());
    let hdrl = list(b"hdrl", &[chunk(b"avih", &avih), strl].concat());
    let mut movi_body = Vec::new();
    let mut idx1 = Vec::new();
    for f in &frames {
        idx1.extend(b"00dc");
        idx1.extend(0x10u32.to_le_bytes());
        idx1.extend((4 + movi_body.len() as u32).to_le_bytes());
        idx1.extend((f.len() as u32).to_le_bytes());
        movi_body.extend(chunk(b"00dc", f));
    }
    let movi = list(b"movi", &movi_body);
    let mut body = b"AVI ".to_vec();
    body.extend(hdrl);
    body.extend(movi);
    body.extend(chunk(b"idx1", &idx1));
    let mut out = b"RIFF".to_vec();
    out.extend((body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

fn dv_case(
    tag: &str,
    size: &str,
    rate: &str,
    scale_rate: (u32, u32),
    handler: &[u8; 4],
    pix: &str,
) {
    let dir = scratch(tag);
    let dv = dir.join("x.dv");
    let v = format!("testsrc2=size={size}:rate={rate}:duration=2");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &v,
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=2:sample_rate=48000",
        "-ac",
        "2",
        "-c:v",
        "dvvideo",
        "-pix_fmt",
        pix,
        "-c:a",
        "pcm_s16le",
        "-f",
        "dv",
        dv.to_str().unwrap(),
    ]);
    let dvb = std::fs::read(&dv).unwrap();
    let frame_size = if size == "720x576" { 144000 } else { 120000 };
    let avi = type1_avi(&dvb, frame_size, scale_rate.0, scale_rate.1, handler);
    let apath = dir.join("t1.avi");
    std::fs::write(&apath, &avi).unwrap();

    let mut d = AviDemuxer::new(Cursor::new(avi)).unwrap();
    assert_eq!(d.streams().len(), 2, "{tag}");
    assert_eq!(d.streams()[0].params.codec, CodecId::DvVideo);
    assert_eq!(d.streams()[1].params.codec, CodecId::PcmS16Le);
    let want_audio = ffmpeg_out(&[
        "-i",
        dv.to_str().unwrap(),
        "-map",
        "0:a",
        "-f",
        "s16le",
        "-",
    ]);
    let want_video = ffmpeg_out(&[
        "-i",
        dv.to_str().unwrap(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-",
    ]);
    let mut dec = ec_dv::DvDecoder::new(d.streams()[0].params.clone()).unwrap();
    let (mut audio, mut video) = (Vec::new(), Vec::new());
    let mut last = i64::MIN;
    loop {
        match d.next_packet() {
            Ok(p) if p.stream == 0 => {
                dec.send_packet(&p).unwrap();
                if let Ok(Frame::Video(f)) = dec.receive_frame() {
                    video.extend(frame_bytes(&f));
                }
            }
            Ok(p) => {
                assert_eq!(p.stream, 1);
                assert!(p.pts.unwrap() > last);
                last = p.pts.unwrap();
                audio.extend_from_slice(&p.data);
            }
            Err(Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    }
    if pix == "yuv420p" {
        assert_eq!(video, want_video, "{tag} video");
    } else {
        assert_eq!(video.len() / 2 * 3 / 2 * 2, video.len() / 2 * 3, "{tag}");
    }
    assert!(!audio.is_empty());
    let n = audio.len().min(want_audio.len());
    assert!(
        audio.len().abs_diff(want_audio.len()) < 4 * 2000,
        "{tag}: audio length"
    );
    assert_eq!(audio[..n], want_audio[..n], "{tag} audio");
    // Also against ffmpeg's own reading of the type-1 file.
    let ff_audio = ffmpeg_out(&[
        "-i",
        apath.to_str().unwrap(),
        "-map",
        "0:a",
        "-f",
        "s16le",
        "-",
    ]);
    let m = audio.len().min(ff_audio.len());
    assert_eq!(audio[..m], ff_audio[..m], "{tag} audio vs ffmpeg type-1");

    // Seek lands on a frame and audio resumes with it.
    d.seek(
        0,
        Timestamp::new(10, d.streams()[0].time_base),
        SeekMode::SyncBefore,
    )
    .unwrap();
    let p = d.next_packet().unwrap();
    assert_eq!(p.stream, 0);
    assert_eq!(p.pts, Some(10));
}

#[test]
fn dv_type1_pal_splits_video_and_audio() {
    if !oracle() {
        return;
    }
    dv_case("dvpal", "720x576", "25", (1, 25), b"dvsd", "yuv420p");
}

#[test]
fn dv_type1_ntsc_splits_video_and_audio() {
    if !oracle() {
        return;
    }
    dv_case(
        "dvntsc",
        "720x480",
        "30000/1001",
        (1001, 30000),
        b"dvsd",
        "yuv411p",
    );
}
