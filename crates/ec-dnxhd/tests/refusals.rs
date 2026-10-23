#![allow(clippy::too_many_arguments, clippy::needless_range_loop, clippy::len_zero)]
//! Streams the decoder refuses by name, and damaged input that must never panic.

mod common;
use common::*;
use ec_core::error::Error;
use ec_core::registry::{CodecId, CodecParameters};
use ec_dnxhd::DnxhdDecoder;

fn decoder() -> DnxhdDecoder {
    DnxhdDecoder::new(CodecParameters::new(CodecId::Dnxhd)).unwrap()
}

fn encode_one(tag: &str, pix: &str, enc: &[&str]) -> Option<Vec<u8>> {
    if !can_run() {
        return None;
    }
    let dir = scratch(tag);
    let out = dir.join("a.mov");
    let lavfi = format!("testsrc2=size=320x176:rate=25,format={pix}");
    let mut args = vec!["-f", "lavfi", "-i", &lavfi, "-frames:v", "1", "-c:v", "dnxhd"];
    args.extend_from_slice(enc);
    args.push(out.to_str().unwrap());
    assert!(ffmpeg(&args));
    let f = mov_frames(&out).remove(0);
    let _ = std::fs::remove_dir_all(&dir);
    Some(f)
}

fn refused(r: Result<ec_core::frame::VideoFrame, Error>, what: &str) {
    match r {
        Err(Error::Unsupported { what: w, .. }) => {
            assert!(w.contains(what), "refusal names {w:?}, wanted {what:?}")
        }
        other => panic!("expected an Unsupported({what}), got {:?}", other.map(|f| f.format)),
    }
}

#[test]
fn alpha_rgb_and_act_variants_are_refused_by_name() {
    let Some(sq) = encode_one("ref-sq", "yuv422p", &["-profile:v", "dnxhr_sq"]) else { return };
    assert!(decoder().decode_bytes(&sq).is_ok());

    let mut alpha = sq.clone();
    alpha[7] |= 1;
    refused(decoder().decode_bytes(&alpha), "alpha");

    let mut act = sq.clone();
    act[0x2c] |= 1;
    refused(decoder().decode_bytes(&act), "adaptive colour transform");

    // GBR (RGB) 4:4:4: ffmpeg writes it with the ACT flag clear.
    let rgb = encode_one("ref-rgb", "gbrp10le", &["-profile:v", "dnxhr_444"]).unwrap();
    refused(decoder().decode_bytes(&rgb), "RGB");
    // The same encoder's YCbCr 4:4:4 is accepted.
    let yuv = encode_one("ref-444", "yuv444p10le", &["-profile:v", "dnxhr_444"]).unwrap();
    assert!(decoder().decode_bytes(&yuv).is_ok());
}

#[test]
fn unknown_cids_and_bad_headers_are_errors() {
    let Some(sq) = encode_one("ref-bad", "yuv422p", &["-profile:v", "dnxhr_sq"]) else { return };
    let mut bad = sq.clone();
    bad[0x28..0x2c].copy_from_slice(&1239u32.to_be_bytes());
    assert!(matches!(decoder().decode_bytes(&bad), Err(Error::Unsupported { .. })));
    let mut bad = sq.clone();
    bad[2] = 0x55;
    assert!(decoder().decode_bytes(&bad).is_err());
    assert!(decoder().decode_bytes(&sq[..100]).is_err());
    assert!(decoder().decode_bytes(&[]).is_err());
    // Depth indicator that contradicts the CID.
    let mut bad = sq.clone();
    bad[0x28..0x2c].copy_from_slice(&1235u32.to_be_bytes());
    assert!(decoder().decode_bytes(&bad).is_err());
}

#[test]
fn damaged_frames_never_panic() {
    let Some(hq) = encode_one("ref-dmg", "yuv422p10le", &["-profile:v", "dnxhr_hqx"]) else { return };
    let mut dec = decoder();
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for round in 0..400 {
        let mut f = hq.clone();
        match round % 4 {
            0 => {
                for _ in 0..1 + next() % 8 {
                    let i = (next() as usize) % f.len();
                    f[i] ^= 1 << (next() % 8);
                }
            }
            1 => {
                // Headers only.
                for _ in 0..1 + next() % 4 {
                    let i = (next() as usize) % 0x300.min(f.len());
                    f[i] = next() as u8;
                }
            }
            2 => f.truncate((next() as usize) % f.len()),
            _ => {
                let i = (next() as usize) % f.len();
                for b in &mut f[i..] {
                    *b = next() as u8;
                }
            }
        }
        let _ = dec.decode_bytes(&f);
        let _ = dec.decode_native(&f);
    }
    // The decoder is still usable afterwards.
    assert!(dec.decode_bytes(&hq).is_ok());
}

#[test]
fn registry_decoder_path_returns_a_frame() {
    use ec_core::registry::Decoder;
    let Some(hq) = encode_one("ref-reg", "yuv422p", &["-profile:v", "dnxhr_hq"]) else { return };
    let mut d = decoder();
    let pkt = ec_core::packet::Packet::new(0, ec_core::timebase::TimeBase::new(1, 25), hq);
    d.send_packet(&pkt).unwrap();
    let frame = d.receive_frame().unwrap();
    assert!(matches!(frame, ec_core::frame::Frame::Video(_)));
    assert!(d.receive_frame().unwrap_err().is_need_more());
    d.flush().unwrap();
    assert!(d.receive_frame().unwrap_err().is_eof());
    let params = d.codec_parameters();
    assert_eq!(params.video().unwrap().width, 320);
    assert!(DnxhdDecoder::new(CodecParameters::new(CodecId::H264)).is_err());
}
