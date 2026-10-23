//! Container round trips: ffmpeg writes AVI / MP4 / 3GP / MKV, our demuxers feed the registry
//! `Decoder`, and the frames must equal ffmpeg's own decode of the same file. Also packed
//! bitstream, refusals, robustness and the speed gate (`--ignored`).
mod common;
use common::*;

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use ec_core::registry::{CodecId, Decoder, Demuxer};
use ec_core::{Error, Packet};
use ec_mpeg4::Mpeg4Decoder;

fn vop_type(d: &[u8]) -> Option<u8> {
    let i = d.windows(4).position(|w| w == [0, 0, 1, 0xB6])?;
    Some(d.get(i + 4)? >> 6)
}

fn demux(path: &Path) -> Box<dyn Demuxer> {
    let ext = path.extension().unwrap().to_str().unwrap();
    let f = BufReader::new(File::open(path).unwrap());
    match ext {
        "avi" => Box::new(ec_riff::AviDemuxer::new(f).unwrap()),
        "mp4" | "3gp" => Box::new(ec_mp4::Mp4Demuxer::new(f).unwrap()),
        "mkv" => Box::new(ec_matroska::MatroskaDemuxer::new(f).unwrap()),
        e => panic!("ext {e}"),
    }
}

fn video_packets(path: &Path) -> (ec_core::registry::StreamInfo, Vec<Packet>) {
    let mut d = demux(path);
    let si = d
        .streams()
        .iter()
        .find(|s| s.params.codec == CodecId::Mpeg4Part2)
        .expect("an mpeg4 stream")
        .clone();
    let mut pk = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) if p.stream == si.index => pk.push(p),
            Ok(_) => {}
            Err(Error::Eof) => break,
            Err(e) => panic!("demux: {e}"),
        }
    }
    (si, pk)
}

fn decode_registry(params: ec_core::registry::CodecParameters, pk: &[Packet]) -> Vec<Vec<u8>> {
    let mut dec: Box<dyn Decoder> = Box::new(Mpeg4Decoder::new(params).unwrap());
    let mut out = Vec::new();
    let drain = |dec: &mut Box<dyn Decoder>, out: &mut Vec<Vec<u8>>| loop {
        match dec.receive_frame() {
            Ok(ec_core::frame::Frame::Video(v)) => out.push(frame_bytes(&v)),
            Ok(_) => panic!("audio"),
            Err(Error::NeedMore) | Err(Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    };
    for p in pk {
        dec.send_packet(p).unwrap();
        drain(&mut dec, &mut out);
    }
    dec.flush().unwrap();
    drain(&mut dec, &mut out);
    out
}

fn check(tag: &str, file: &str, enc: &[&str]) {
    if !gate() {
        return;
    }
    let dir = scratch(tag);
    let path = dir.join(file);
    assert!(encode("176x144", 30, enc, &path), "encode");
    let refb = reference(&path, &[], &dir.join("ref.yuv")).unwrap();
    let (si, pk) = video_packets(&path);
    let frames = decode_registry(si.params.clone(), &pk);
    let fsz = frames[0].len();
    assert_eq!(refb.len() / fsz, frames.len(), "frame count");
    for (i, f) in frames.iter().enumerate() {
        assert!(
            f[..] == refb[i * fsz..(i + 1) * fsz],
            "{tag}: frame {i} differs"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

const BF: [&str; 8] = ["-c:v", "mpeg4", "-qscale:v", "4", "-bf", "2", "-g", "12"];

#[test]
fn avi_dx50_bframes() {
    check(
        "avi_dx50",
        "a.avi",
        &[
            BF[0], BF[1], BF[2], BF[3], BF[4], BF[5], BF[6], BF[7], "-vtag", "DX50",
        ],
    );
}
#[test]
fn avi_xvid_qpel() {
    check(
        "avi_xvid",
        "a.avi",
        &[
            "-c:v",
            "mpeg4",
            "-qscale:v",
            "5",
            "-flags",
            "+qpel+mv4",
            "-bf",
            "2",
            "-vtag",
            "XVID",
        ],
    );
}
#[test]
fn avi_fmp4() {
    check(
        "avi_fmp4",
        "a.avi",
        &["-c:v", "mpeg4", "-qscale:v", "6", "-vtag", "FMP4"],
    );
}
#[test]
fn mp4_mp4v() {
    check(
        "mp4v",
        "a.mp4",
        &[
            "-c:v",
            "mpeg4",
            "-qscale:v",
            "4",
            "-bf",
            "2",
            "-flags",
            "+qpel",
        ],
    );
}
#[test]
fn mp4_mp4v_global_header() {
    check(
        "mp4v_gh",
        "a.mp4",
        &[
            "-c:v",
            "mpeg4",
            "-qscale:v",
            "4",
            "-flags",
            "+global_header",
        ],
    );
}
#[test]
fn mkv_asp() {
    check(
        "mkv",
        "a.mkv",
        &[
            "-c:v",
            "mpeg4",
            "-qscale:v",
            "4",
            "-bf",
            "2",
            "-flags",
            "+mv4+qpel",
        ],
    );
}
#[test]
fn gp3_h263() {
    if !gate() {
        return;
    }
    let dir = scratch("3gp");
    let path = dir.join("a.3gp");
    assert!(encode(
        "176x144",
        20,
        &["-c:v", "h263", "-qscale:v", "5"],
        &path
    ));
    let refb = reference(&path, &[], &dir.join("ref.yuv")).unwrap();
    let (si, pk) = video_packets(&path);
    let frames = decode_registry(si.params.clone(), &pk);
    let fsz = frames[0].len();
    assert_eq!(refb.len() / fsz, frames.len());
    for (i, f) in frames.iter().enumerate() {
        assert!(f[..] == refb[i * fsz..(i + 1) * fsz], "frame {i}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// DivX "packed B-frames": fold each B packet into the preceding packet and leave a 7-byte N-VOP
/// where it was.
#[test]
fn packed_bitstream_matches_unpacked() {
    if !gate() {
        return;
    }
    let dir = scratch("packed");
    let path = dir.join("a.avi");
    assert!(encode(
        "176x144",
        40,
        &["-c:v", "mpeg4", "-qscale:v", "4", "-bf", "1", "-g", "12"],
        &path
    ));
    let refb = reference(&path, &[], &dir.join("ref.yuv")).unwrap();
    let (si, pk) = video_packets(&path);
    let mut packed: Vec<Packet> = Vec::new();
    let mut nb = 0;
    for p in &pk {
        if vop_type(&p.data) == Some(2) && !packed.is_empty() {
            let last = packed.last_mut().unwrap();
            let mut v = last.data.to_vec();
            v.extend_from_slice(&p.data);
            last.data = v.into();
            let mut n = p.clone();
            n.data = vec![0, 0, 1, 0xB6, 0x50, 0, 0].into();
            packed.push(n);
            nb += 1;
        } else {
            packed.push(p.clone());
        }
    }
    assert!(nb > 4, "stream has B-frames");
    let frames = decode_registry(si.params.clone(), &packed);
    let fsz = frames[0].len();
    assert_eq!(refb.len() / fsz, frames.len(), "frame count");
    for (i, f) in frames.iter().enumerate() {
        assert!(f[..] == refb[i * fsz..(i + 1) * fsz], "packed: frame {i}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn data_partitioning_is_refused_by_name() {
    if !gate() {
        return;
    }
    let dir = scratch("dp");
    let path = dir.join("a.m4v");
    if !encode(
        "176x144",
        4,
        &[
            "-c:v",
            "mpeg4",
            "-data_partitioning",
            "1",
            "-flags",
            "+aic",
            "-f",
            "m4v",
        ],
        &path,
    ) {
        eprintln!("SKIP: encoder rejects data partitioning");
        return;
    }
    let es = std::fs::read(&path).unwrap();
    let mut dec = new_decoder();
    let mut err = None;
    for p in split_m4v(&es) {
        if let Err(e) = dec.send_bytes(p, None) {
            err = Some(e.to_string());
            break;
        }
    }
    let e = err.expect("must refuse");
    assert!(
        e.contains("data partitioning") || e.contains("partition"),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_input_does_not_panic() {
    if !gate() {
        return;
    }
    let dir = scratch("corrupt");
    let path = dir.join("a.m4v");
    assert!(encode(
        "176x144",
        20,
        &[
            "-c:v",
            "mpeg4",
            "-qscale:v",
            "5",
            "-bf",
            "2",
            "-flags",
            "+qpel+mv4",
            "-f",
            "m4v"
        ],
        &path
    ));
    let es = std::fs::read(&path).unwrap();
    let mut seed = 0x1234_5678u32;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for round in 0..40 {
        let mut b = es.clone();
        match round % 3 {
            0 => {
                for _ in 0..20 {
                    let i = rnd() as usize % b.len();
                    b[i] ^= 1 << (rnd() % 8);
                }
            }
            1 => b.truncate(rnd() as usize % b.len()),
            _ => {
                let i = rnd() as usize % b.len();
                let n = (rnd() as usize % 200).min(b.len() - i);
                b.drain(i..i + n);
            }
        }
        let mut dec = new_decoder();
        for p in split_m4v(&b) {
            let _ = dec.send_bytes(p, None);
            while dec.pop_picture().is_some() {}
        }
        let _ = dec.flush_stream();
        while dec.pop_picture().is_some() {}
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Run on a quiet machine: `cargo test -p ec-mpeg4 --release -- --ignored speed`.
#[test]
#[ignore]
fn speed_720p_at_least_120_fps() {
    if !gate() {
        return;
    }
    let dir = scratch("speed");
    let path = dir.join("hd.m4v");
    assert!(encode(
        "1280x720",
        100,
        &[
            "-c:v",
            "mpeg4",
            "-qscale:v",
            "4",
            "-bf",
            "2",
            "-flags",
            "+qpel+mv4",
            "-g",
            "25",
            "-f",
            "m4v"
        ],
        &path
    ));
    let es = std::fs::read(&path).unwrap();
    let pk = split_m4v(&es);
    let mut best = f64::MAX;
    for _ in 0..3 {
        let mut dec = new_decoder();
        let t = std::time::Instant::now();
        let out = decode_packets(&mut dec, pk.iter().copied()).unwrap();
        let dt = t.elapsed().as_secs_f64();
        assert_eq!(out.len(), 100);
        best = best.min(dt);
    }
    let fps = 100.0 / best;
    eprintln!("720p decode: {fps:.0} fps");
    assert!(fps >= 120.0, "{fps:.0} fps");
    let _ = std::fs::remove_dir_all(&dir);
}
