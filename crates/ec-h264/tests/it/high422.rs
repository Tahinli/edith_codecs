//! High 4:2:2 decode, byte-exact against ffmpeg (`yuv422p` / `yuv422p10le`),
//! on streams libx264 writes from synthetic sources: intra-only and long-GOP
//! with B-frames, CABAC and CAVLC, the 8x8 transform, weighted prediction,
//! scaling matrices, chroma QP offsets, several slices, progressive and
//! MBAFF, at 8 and 10 bits.
//!
//! Every case is generated in the test (no committed binaries). A case skips,
//! saying so, when ffmpeg or the needed libx264 is absent, and fails instead
//! when `EC_REQUIRE_FIXTURES` is set. The 1920x1080 cases are slow enough to be
//! opt-in: set `EC_H264_HI422_HD=1` (the lane gate does).

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_h264::{Decoder, NalOutcome};

use super::common::{escape, ffmpeg_frames, frame_bytes};

fn decode_all(bytes: &[u8]) -> Result<Vec<Vec<u8>>, ec_h264::Error> {
    let mut dec = Decoder::new();
    let mut frames = Vec::new();
    for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
        if dec.push_nal(nal)? == NalOutcome::PictureBoundary {
            dec.end_picture()?;
            dec.push_nal(nal)?;
        }
        while let Some(f) = dec.next_frame() {
            frames.push(frame_bytes(&f));
        }
    }
    dec.flush()?;
    while let Some(f) = dec.next_frame() {
        frames.push(frame_bytes(&f));
    }
    Ok(frames)
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ec-h264-hi422-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Case {
    tag: &'static str,
    /// lavfi source.
    src: &'static str,
    size: &'static str,
    frames: u32,
    /// 10-bit High 10 instead of High 8-bit.
    ten_bit: bool,
    /// Macroblock-adaptive frame/field coding.
    mbaff: bool,
    /// `-x264-params`.
    params: &'static str,
}

fn encode(dir: &Path, c: &Case) -> Option<PathBuf> {
    let stream = dir.join(format!("{}.264", c.tag));
    let source = format!("{}=size={}:rate=25", c.src, c.size);
    let (pix, profile) = if c.ten_bit {
        ("yuv422p10le", "high422")
    } else {
        ("yuv422p", "high422")
    };
    let params = if c.mbaff {
        format!("interlaced=1:{}", c.params)
    } else {
        c.params.to_string()
    };
    let st = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(source)
        .args(["-pix_fmt", pix, "-c:v", "libx264", "-profile:v", profile])
        .args([
            "-flags",
            if c.mbaff { "+ildct" } else { "+loop" },
            "-frames:v",
        ])
        .arg(c.frames.to_string())
        .args(["-x264-params", &params, "-f", "h264"])
        .arg(&stream)
        .status()
        .ok()?;
    st.success().then_some(stream)
}

fn check(c: &Case) -> Result<usize, String> {
    let dir = scratch();
    let Some(stream) = encode(&dir, c) else {
        if std::env::var_os("EC_REQUIRE_FIXTURES").is_some() {
            panic!("{}: ffmpeg/libx264 cannot encode this case", c.tag);
        }
        eprintln!(
            "SKIP {}: ffmpeg with the needed libx264 is not available",
            c.tag
        );
        return Ok(0);
    };
    let bytes = std::fs::read(&stream).map_err(|e| e.to_string())?;
    let ours = decode_all(&bytes).map_err(|e| format!("{}: {e}", c.tag))?;
    let first = ours
        .first()
        .ok_or(format!("{}: no frames decoded", c.tag))?;
    let pix = if c.ten_bit { "yuv422p10le" } else { "yuv422p" };
    let theirs = ffmpeg_frames(&stream, pix, first.len())
        .ok_or(format!("{}: ffmpeg cannot decode", c.tag))?;
    let _ = std::fs::remove_file(&stream);
    if theirs.len() != first.len() * ours.len() {
        return Err(format!(
            "{}: {} frames, ffmpeg {}",
            c.tag,
            ours.len(),
            theirs.len() / first.len()
        ));
    }
    for (i, f) in ours.iter().enumerate() {
        let t = &theirs[i * f.len()..(i + 1) * f.len()];
        if let Some(pos) = f.iter().zip(t).position(|(a, b)| a != b) {
            return Err(format!(
                "{}: frame {i} differs at byte {pos}: ours {} ffmpeg {}",
                c.tag, f[pos], t[pos]
            ));
        }
    }
    Ok(ours.len())
}

fn sweep(cases: &[Case]) {
    let mut failed = Vec::new();
    for c in cases {
        match check(c) {
            Ok(n) => eprintln!("ok   {} ({n} frames)", c.tag),
            Err(e) => failed.push(e),
        }
    }
    let _ = std::fs::remove_dir_all(scratch());
    assert!(
        failed.is_empty(),
        "4:2:2 mismatches:\n{}",
        failed.join("\n")
    );
}

const fn case(
    tag: &'static str,
    src: &'static str,
    size: &'static str,
    frames: u32,
    params: &'static str,
) -> Case {
    Case {
        tag,
        src,
        size,
        frames,
        ten_bit: false,
        mbaff: false,
        params,
    }
}

const fn ten(mut c: Case) -> Case {
    c.ten_bit = true;
    c
}

const fn mbaff(mut c: Case) -> Case {
    c.mbaff = true;
    c
}

const SMALL: &[Case] = &[
    case("i-cabac", "testsrc2", "192x128", 3, "keyint=1"),
    case("i-cavlc", "testsrc2", "192x128", 3, "keyint=1:cabac=0"),
    case(
        "i-cabac-no8x8",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:8x8dct=0",
    ),
    case(
        "i-cavlc-no8x8",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:cabac=0:8x8dct=0",
    ),
    case(
        "i-cabac-mandel",
        "mandelbrot",
        "176x144",
        3,
        "keyint=1:qp=18",
    ),
    case(
        "i-cavlc-mandel",
        "mandelbrot",
        "176x144",
        3,
        "keyint=1:cabac=0:qp=18",
    ),
    case("i-cabac-qp40", "testsrc2", "192x128", 3, "keyint=1:qp=40"),
    case(
        "i-cavlc-qp8",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:cabac=0:qp=8",
    ),
    case("i-cabac-cqm", "testsrc2", "192x128", 3, "keyint=1:cqm=jvt"),
    case(
        "i-cavlc-cqm",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:cabac=0:cqm=jvt",
    ),
    case(
        "i-cabac-cqo",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:chroma-qp-offset=-5",
    ),
    case(
        "i-cavlc-cqo",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:cabac=0:chroma-qp-offset=6",
    ),
    case(
        "i-cabac-slices",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:slices=3",
    ),
    case(
        "i-cabac-nodeblock",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:no-deblock=1",
    ),
    case(
        "i-cabac-cip",
        "testsrc2",
        "192x128",
        3,
        "keyint=1:constrained-intra=1",
    ),
    case("p-cabac", "testsrc2", "192x128", 8, "keyint=30:bframes=0"),
    case(
        "p-cavlc",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=0:cabac=0",
    ),
    case("b-cabac", "testsrc2", "192x128", 10, "keyint=30:bframes=3"),
    case(
        "b-cavlc",
        "testsrc2",
        "192x128",
        10,
        "keyint=30:bframes=3:cabac=0",
    ),
    case(
        "b-cabac-pyramid",
        "mandelbrot",
        "192x128",
        12,
        "keyint=30:bframes=3:b-pyramid=normal:ref=4",
    ),
    case(
        "b-cabac-temporal",
        "testsrc2",
        "192x128",
        10,
        "keyint=30:bframes=3:direct=temporal",
    ),
    case(
        "b-cabac-weight",
        "testsrc2",
        "192x128",
        10,
        "keyint=30:bframes=2:weightp=2:weightb=1",
    ),
    case(
        "b-cavlc-weight",
        "testsrc2",
        "192x128",
        10,
        "keyint=30:bframes=2:weightp=2:weightb=1:cabac=0",
    ),
    case(
        "b-cabac-cqm",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:cqm=jvt",
    ),
    case(
        "b-cabac-slices",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:slices=4",
    ),
    case(
        "b-cabac-qp40",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:qp=40",
    ),
    case(
        "b-cabac-subme9",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:subme=9:trellis=2",
    ),
    case(
        "b-cabac-noloop",
        "testsrc2",
        "192x128",
        8,
        "keyint=30:bframes=2:no-deblock=1",
    ),
];

/// Every case at 8 bits, progressive.
#[test]
fn x264_hi422_progressive_8bit() {
    sweep(SMALL);
}

/// The same at 10 bits.
#[test]
fn x264_hi422_progressive_10bit() {
    let cases: Vec<Case> = SMALL.iter().map(|c| ten(Case { ..*c })).collect();
    sweep(&cases);
}

/// MBAFF at 8 and 10 bits.
#[test]
fn x264_hi422_mbaff() {
    let mut cases: Vec<Case> = Vec::new();
    for (i, c) in SMALL.iter().enumerate() {
        // Every other case keeps the sweep quick; the rest run progressive.
        if i % 2 == 0 {
            cases.push(mbaff(Case { ..*c }));
            cases.push(ten(mbaff(Case { ..*c })));
        }
    }
    for c in &mut cases {
        c.tag = Box::leak(
            format!("mbaff-{}{}", c.tag, if c.ten_bit { "-10" } else { "" }).into_boxed_str(),
        );
    }
    sweep(&cases);
}

/// 1920x1080 intra and long-GOP, progressive and MBAFF (opt-in, slow).
#[test]
fn x264_hi422_hd() {
    if std::env::var_os("EC_H264_HI422_HD").is_none() {
        eprintln!("SKIP x264_hi422_hd: set EC_H264_HI422_HD=1");
        return;
    }
    sweep(&[
        case("hd-i-cabac", "testsrc2", "1920x1080", 2, "keyint=1"),
        case(
            "hd-b-cabac",
            "testsrc2",
            "1920x1080",
            5,
            "keyint=30:bframes=2",
        ),
        case(
            "hd-b-cavlc",
            "mandelbrot",
            "1920x1080",
            5,
            "keyint=30:bframes=2:cabac=0",
        ),
        ten(case(
            "hd-i-cabac-10",
            "testsrc2",
            "1920x1080",
            2,
            "keyint=1",
        )),
        mbaff(case(
            "hd-mbaff-b-cabac",
            "testsrc2",
            "1920x1080",
            5,
            "keyint=30:bframes=2",
        )),
        ten(mbaff(case(
            "hd-mbaff-i-cavlc-10",
            "testsrc2",
            "1920x1080",
            2,
            "keyint=1:cabac=0",
        ))),
    ]);
}

// ---- I_PCM in 4:2:2 (x264 never writes one) ----------------------------

fn push_nal(out: &mut Vec<u8>, kind: u8, rbsp: &[u8]) {
    out.extend_from_slice(&[0, 0, 0, 1, (3 << 5) | kind]);
    out.extend_from_slice(&escape(rbsp));
}

/// Two I_PCM macroblocks side by side (32x16) in a High 4:2:2 stream at
/// `bd` bits: the decoder must read 256 luma and 2 x 8 x 16 chroma samples
/// per macroblock and hand the planes back untouched.
fn pcm_stream(bd: u32) -> (Vec<u8>, Vec<u16>, Vec<u16>, Vec<u16>) {
    use ec_core::BitWriter;
    let mut w = BitWriter::new();
    w.write_bits(122, 8);
    w.write_bits(0, 8);
    w.write_bits(30, 8);
    w.write_ue(0);
    w.write_ue(2); // chroma_format_idc 2
    w.write_ue(bd - 8);
    w.write_ue(bd - 8);
    w.write_bit(false);
    w.write_bit(false);
    w.write_ue(0);
    w.write_ue(2);
    w.write_ue(1);
    w.write_bit(false);
    w.write_ue(1); // 2 macroblocks wide
    w.write_ue(0); // 1 high
    w.write_bit(true);
    w.write_bit(true);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(true);
    w.align_to_byte();
    let sps = w.as_bytes().to_vec();
    let mut w = BitWriter::new();
    w.write_ue(0);
    w.write_ue(0);
    w.write_bit(false);
    w.write_bit(false);
    w.write_ue(0);
    w.write_ue(0);
    w.write_ue(0);
    w.write_bit(false);
    w.write_bits(0, 2);
    w.write_se(0);
    w.write_se(0);
    w.write_se(0);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(true);
    w.align_to_byte();
    let pps = w.as_bytes().to_vec();

    let max = (1u32 << bd) - 1;
    let val =
        |plane: u32, x: u32, y: u32| ((plane * 97 + x * 13 + y * 29 + x * y) % (max + 1)) as u16;
    let (mut yp, mut cb, mut cr) = (Vec::new(), Vec::new(), Vec::new());
    for y in 0..16 {
        for x in 0..32 {
            yp.push(val(0, x, y));
        }
    }
    for y in 0..16 {
        for x in 0..16 {
            cb.push(val(1, x, y));
            cr.push(val(2, x, y));
        }
    }
    let mut w = BitWriter::new();
    w.write_ue(0);
    w.write_ue(7);
    w.write_ue(0);
    w.write_bits(0, 4);
    w.write_ue(0);
    w.write_bit(false);
    w.write_bit(false);
    w.write_se(0);
    for mb in 0..2usize {
        w.write_ue(25);
        w.align_to_byte();
        for y in 0..16usize {
            for x in 0..16usize {
                w.write_bits(u32::from(yp[y * 32 + mb * 16 + x]), bd);
            }
        }
        for plane in [&cb, &cr] {
            for y in 0..16usize {
                for x in 0..8usize {
                    w.write_bits(u32::from(plane[y * 16 + mb * 8 + x]), bd);
                }
            }
        }
    }
    w.write_bit(true);
    w.align_to_byte();
    let mut out = Vec::new();
    push_nal(&mut out, 7, &sps);
    push_nal(&mut out, 8, &pps);
    push_nal(&mut out, 5, w.as_bytes());
    (out, yp, cb, cr)
}

#[test]
fn hi422_pcm_macroblocks_round_trip() {
    for bd in [8u32, 10] {
        let (stream, yp, cb, cr) = pcm_stream(bd);
        let frames = decode_all(&stream).expect("decode");
        assert_eq!(frames.len(), 1);
        let bytes = &frames[0];
        let mut expected = Vec::new();
        for plane in [&yp, &cb, &cr] {
            for &v in plane {
                if bd == 8 {
                    expected.push(v as u8);
                } else {
                    expected.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        assert_eq!(bytes, &expected, "{bd}-bit I_PCM 4:2:2");
    }
}
