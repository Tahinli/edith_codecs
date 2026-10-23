//! MS-MPEG4 (DivX 3 / MPEG-4 variant v2, v3) byte-exact parity with ffmpeg: ffmpeg's `msmpeg4`
//! and `msmpeg4v2` encoders write AVI files at test time, our AVI demuxer feeds the registry
//! `Decoder`, and every frame must equal `ffmpeg -c:v rawvideo` of the same file.
mod common;
use common::*;

use std::fs::File;
use std::io::BufReader;
use ec_core::registry::{CodecId, Decoder, Demuxer};
use ec_core::{Error, Packet};
use ec_mpeg4::Mpeg4Decoder;

#[allow(clippy::too_many_arguments)]
fn check(tag: &str, vtag: &str, codec: &str, id: CodecId, lavfi: &str, size: &str, frames: u32, enc: &[&str]) {
    if !gate() {
        return;
    }
    if !have_encoder(codec) {
        assert!(!require(), "ffmpeg encoder {codec} missing");
        eprintln!("skipped: no {codec} encoder");
        return;
    }
    let dir = scratch(tag);
    let path = dir.join(format!("{tag}.avi"));
    let input = format!("{lavfi}=size={size}:rate=25");
    let fr = frames.to_string();
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", &input, "-frames:v", &fr, "-c:v", codec, "-vtag", vtag];
    args.extend_from_slice(enc);
    args.push(path.to_str().unwrap());
    assert!(run(&args), "encode failed");
    let refb = reference(&path, &[], &dir.join("ref.yuv")).unwrap();

    let mut d = ec_riff::AviDemuxer::new(BufReader::new(File::open(&path).unwrap())).unwrap();
    let si = d.streams().iter().find(|s| s.params.codec == id).expect("stream").clone();
    let mut pk: Vec<Packet> = Vec::new();
    loop {
        match d.next_packet() {
            Ok(p) if p.stream == si.index => pk.push(p),
            Ok(_) => {}
            Err(Error::Eof) => break,
            Err(e) => panic!("demux: {e}"),
        }
    }
    let mut dec: Box<dyn Decoder> = Box::new(Mpeg4Decoder::new(si.params.clone()).unwrap());
    let mut out: Vec<Vec<u8>> = Vec::new();
    let drain = |dec: &mut Box<dyn Decoder>, out: &mut Vec<Vec<u8>>| loop {
        match dec.receive_frame() {
            Ok(ec_core::frame::Frame::Video(v)) => out.push(frame_bytes(&v)),
            Ok(_) => panic!("audio"),
            Err(Error::NeedMore) | Err(Error::Eof) => break,
            Err(e) => panic!("{e}"),
        }
    };
    for p in &pk {
        dec.send_packet(p).unwrap();
        drain(&mut dec, &mut out);
    }
    dec.flush().unwrap();
    drain(&mut dec, &mut out);
    let fsz = out[0].len();
    assert_eq!(refb.len() / fsz, out.len(), "{tag}: frame count");
    let mut bad = 0;
    for (i, f) in out.iter().enumerate() {
        if f[..] != refb[i * fsz..(i + 1) * fsz] {
            bad += 1;
            eprintln!("{tag}: frame {i} differs");
        }
    }
    eprintln!("{tag}: {} frames compared, {bad} mismatches", out.len());
    assert_eq!(bad, 0, "{tag}: mismatching frames");
    assert!(out.len() as u32 >= frames - 1);
    let _ = std::fs::remove_dir_all(&dir);
}

macro_rules! v3 {
    ($name:ident, $lavfi:expr, $size:expr, $frames:expr, [$($arg:expr),* $(,)?]) => {
        #[test]
        fn $name() {
            check(stringify!($name), "DIV3", "msmpeg4", CodecId::MsMpeg4V3, $lavfi, $size, $frames, &[$($arg),*]);
        }
    };
}
macro_rules! v2 {
    ($name:ident, $lavfi:expr, $size:expr, $frames:expr, [$($arg:expr),* $(,)?]) => {
        #[test]
        fn $name() {
            check(stringify!($name), "MP42", "msmpeg4v2", CodecId::MsMpeg4V2, $lavfi, $size, $frames, &[$($arg),*]);
        }
    };
}

v3!(v3_q2, "testsrc2", "176x144", 30, ["-qscale:v", "2", "-g", "12"]);
v3!(v3_q5, "testsrc2", "176x144", 30, ["-qscale:v", "5", "-g", "30"]);
v3!(v3_q12, "testsrc2", "176x144", 30, ["-qscale:v", "12", "-g", "30"]);
v3!(v3_q28, "testsrc2", "176x144", 30, ["-qscale:v", "28", "-g", "30"]);
v3!(v3_q31, "testsrc2", "176x144", 30, ["-qscale:v", "31", "-g", "30"]);
v3!(v3_all_intra, "testsrc2", "176x144", 12, ["-qscale:v", "6", "-g", "1"]);
v3!(v3_odd_size, "testsrc2", "100x76", 30, ["-qscale:v", "4", "-g", "10"]);
v3!(v3_odd_size2, "mandelbrot", "98x50", 30, ["-qscale:v", "3", "-g", "100"]);
v3!(v3_cif_bitrate, "testsrc2", "352x288", 40, ["-b:v", "300k", "-g", "25"]);
v3!(v3_low_bitrate, "testsrc2", "320x240", 40, ["-b:v", "60k", "-g", "50"]);
v3!(v3_high_bitrate, "testsrc2", "320x240", 30, ["-b:v", "2500k", "-g", "50"]);
v3!(v3_mandelbrot, "mandelbrot", "176x144", 40, ["-qscale:v", "3", "-g", "20"]);
v3!(v3_mandelbrot_hi, "mandelbrot", "320x240", 30, ["-qscale:v", "1", "-g", "40"]);
v3!(v3_g_variation, "testsrc2", "176x144", 40, ["-qscale:v", "7", "-g", "3"]);
v3!(v3_mbd_rd, "testsrc2", "176x144", 20, ["-qscale:v", "6", "-mbd", "rd", "-g", "10"]);
v3!(v3_me_range, "testsrc2", "176x144", 30, ["-qscale:v", "5", "-me_method", "full", "-g", "30"]);
v3!(v3_hd_ish, "testsrc2", "640x360", 12, ["-qscale:v", "6", "-g", "6"]);
v3!(v3_rate_ctl, "testsrc2", "176x144", 60, ["-b:v", "120k", "-maxrate", "150k", "-bufsize", "50k", "-g", "20"]);

v2!(v2_q2, "testsrc2", "176x144", 30, ["-qscale:v", "2", "-g", "12"]);
v2!(v2_q6, "testsrc2", "176x144", 30, ["-qscale:v", "6", "-g", "30"]);
v2!(v2_q28, "testsrc2", "176x144", 30, ["-qscale:v", "28", "-g", "30"]);
v2!(v2_all_intra, "testsrc2", "176x144", 12, ["-qscale:v", "6", "-g", "1"]);
v2!(v2_odd_size, "testsrc2", "100x76", 30, ["-qscale:v", "4", "-g", "10"]);
v2!(v2_mandelbrot, "mandelbrot", "176x144", 40, ["-qscale:v", "3", "-g", "20"]);
v2!(v2_cif_bitrate, "testsrc2", "352x288", 40, ["-b:v", "300k", "-g", "25"]);

/// Version 1 has no encoder to generate fixtures with: it must be refused, not mis-decoded.
#[test]
fn v1_is_refused() {
    use ec_core::registry::CodecParameters;
    assert!(Mpeg4Decoder::new(CodecParameters::new(CodecId::MsMpeg4V1)).is_err());
}
