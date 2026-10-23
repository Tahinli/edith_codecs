//! High 10 (profile_idc 110) decode, byte-exact against ffmpeg's
//! `-pix_fmt yuv420p10le` output.
//!
//! Two layers, following the other ec-h264 tests:
//! 1. Committed fixtures in `tests/data/high10/`: tiny libx264 streams with the
//!    MD5 of ffmpeg's decode beside them, checked on every run with no ffmpeg.
//! 2. A sweep that encodes synthetic sources with libx264 (`-profile:v high10`)
//!    and compares every frame with ffmpeg's decode. It skips, saying so, when
//!    ffmpeg or a 10-bit libx264 is absent, and fails instead when
//!    `EC_REQUIRE_FIXTURES` is set.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_core::frame::{PixelFormat, VideoFrame};
use ec_h264::{Decoder, NalOutcome};

/// One decoded frame as I010 bytes (little-endian 16-bit samples, planes
/// back to back), checking the format and plane geometry on the way.
fn frame_bytes(frame: &VideoFrame) -> Vec<u8> {
    assert_eq!(frame.format, PixelFormat::I010, "High 10 comes out as I010");
    let (w, h) = (frame.width as usize, frame.height as usize);
    let dims = [
        (w, h),
        (w.div_ceil(2), h.div_ceil(2)),
        (w.div_ceil(2), h.div_ceil(2)),
    ];
    let mut out = Vec::new();
    for (plane, (pw, ph)) in frame.planes.iter().zip(dims) {
        for y in 0..ph {
            out.extend_from_slice(plane.row(y, pw * 2).expect("plane row"));
        }
    }
    out
}

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

fn ffmpeg_frames(stream: &Path, frame_len: usize) -> Option<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(stream)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output()
        .ok()?;
    (out.status.success() && !out.stdout.is_empty() && out.stdout.len() % frame_len == 0)
        .then_some(out.stdout)
}

// ---- MD5 (RFC 1321), so the committed fixtures need no ffmpeg ----------------

#[allow(clippy::chunks_exact_to_as_chunks)]
fn md5(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_le_bytes());
    for chunk in msg.chunks_exact(64) {
        let m: Vec<u32> = chunk
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/high10")
}

/// Every committed stream decodes to the MD5 ffmpeg produced for it.
#[test]
fn committed_streams_match_ffmpeg_md5() {
    let mut names: Vec<_> = std::fs::read_dir(data_dir())
        .expect("tests/data/high10 exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "264"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no committed High 10 streams");
    for stream in names {
        let want = std::fs::read_to_string(stream.with_extension("md5")).expect("md5 file");
        let want = want.split_whitespace().next().expect("md5 text");
        let frames = decode_all(&std::fs::read(&stream).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", stream.display()));
        let all: Vec<u8> = frames.concat();
        assert_eq!(
            md5(&all),
            want,
            "{}: decode differs from ffmpeg",
            stream.display()
        );
    }
}

// ---- the libx264 sweep --------------------------------------------------------

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ec-h264-high10-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Case {
    tag: &'static str,
    /// lavfi source and size.
    src: &'static str,
    size: &'static str,
    frames: u32,
    /// Extra ffmpeg output arguments.
    extra: &'static [&'static str],
}

fn encode(dir: &Path, c: &Case) -> Option<PathBuf> {
    let stream = dir.join(format!("{}.264", c.tag));
    let source = format!("{}=size={}:rate=25", c.src, c.size);
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(source);
    args.extend(
        [
            "-pix_fmt",
            "yuv420p10le",
            "-c:v",
            "libx264",
            "-profile:v",
            "high10",
            "-frames:v",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.push(c.frames.to_string());
    args.extend(c.extra.iter().map(|s| s.to_string()));
    args.extend([
        "-f".into(),
        "h264".into(),
        stream.to_string_lossy().into_owned(),
    ]);
    let st = Command::new("ffmpeg").args(&args).status().ok()?;
    st.success().then_some(stream)
}

fn check(c: &Case) -> Result<usize, String> {
    let dir = scratch();
    let Some(stream) = encode(&dir, c) else {
        if std::env::var_os("EC_REQUIRE_FIXTURES").is_some() {
            panic!("{}: ffmpeg/libx264 10-bit cannot encode", c.tag);
        }
        eprintln!(
            "SKIP {}: ffmpeg with 10-bit libx264 is not available",
            c.tag
        );
        return Ok(0);
    };
    let bytes = std::fs::read(&stream).map_err(|e| e.to_string())?;
    let ours = decode_all(&bytes).map_err(|e| format!("{}: {e}", c.tag))?;
    let first = ours.first().ok_or("no frames decoded")?;
    let theirs = ffmpeg_frames(&stream, first.len()).ok_or("ffmpeg cannot decode")?;
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
                "{}: frame {i} differs at byte {pos} (sample {}): ours {} ffmpeg {}",
                c.tag,
                pos / 2,
                f[pos & !1] as u16 | (f[pos | 1] as u16) << 8,
                t[pos & !1] as u16 | (t[pos | 1] as u16) << 8
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
        "High 10 mismatches:\n{}",
        failed.join("\n")
    );
}

#[test]
fn intra_cavlc_and_cabac() {
    sweep(&[
        Case {
            tag: "i-cavlc-q20",
            src: "testsrc2",
            size: "176x144",
            frames: 3,
            extra: &["-qp", "20", "-x264-params", "cabac=0:keyint=1"],
        },
        Case {
            tag: "i-cabac-q10",
            src: "testsrc2",
            size: "176x144",
            frames: 3,
            extra: &["-qp", "10", "-x264-params", "keyint=1"],
        },
        Case {
            tag: "i-cavlc-no8x8",
            src: "mandelbrot",
            size: "176x144",
            frames: 2,
            extra: &["-qp", "26", "-x264-params", "cabac=0:keyint=1:8x8dct=0"],
        },
        Case {
            tag: "i-cabac-no8x8",
            src: "mandelbrot",
            size: "176x144",
            frames: 2,
            extra: &["-qp", "26", "-x264-params", "keyint=1:8x8dct=0"],
        },
        Case {
            tag: "i-qp1-cavlc",
            src: "testsrc2",
            size: "128x96",
            frames: 2,
            extra: &["-qp", "1", "-x264-params", "cabac=0:keyint=1"],
        },
        Case {
            tag: "i-qp1-cabac",
            src: "testsrc2",
            size: "128x96",
            frames: 2,
            extra: &["-qp", "1", "-x264-params", "keyint=1"],
        },
        Case {
            tag: "i-qp51",
            src: "testsrc2",
            size: "128x96",
            frames: 2,
            extra: &["-qp", "51", "-x264-params", "keyint=1"],
        },
        Case {
            tag: "i-noise-q4",
            src: "testsrc2",
            size: "128x96",
            frames: 2,
            extra: &[
                "-vf",
                "noise=alls=60:allf=t",
                "-qp",
                "4",
                "-x264-params",
                "keyint=1",
            ],
        },
        Case {
            tag: "i-noise-q4-cavlc",
            src: "testsrc2",
            size: "128x96",
            frames: 2,
            extra: &[
                "-vf",
                "noise=alls=60:allf=t",
                "-qp",
                "4",
                "-x264-params",
                "cabac=0:keyint=1",
            ],
        },
        // QP below zero: QPY runs down to -QpBdOffsetY (-12) at 10 bits.
        Case {
            tag: "i-negqp-cabac",
            src: "testsrc2",
            size: "128x96",
            frames: 2,
            extra: &["-x264-params", "qp=-6:crf=-1:keyint=1"],
        },
        Case {
            tag: "i-negqp-cavlc",
            src: "mandelbrot",
            size: "128x96",
            frames: 2,
            extra: &["-x264-params", "qp=-11:crf=-1:keyint=1:cabac=0"],
        },
        Case {
            tag: "i-slices",
            src: "testsrc2",
            size: "176x144",
            frames: 2,
            extra: &["-qp", "22", "-x264-params", "keyint=1:slices=4"],
        },
        Case {
            tag: "i-odd-crop",
            src: "testsrc2",
            size: "162x98",
            frames: 2,
            extra: &["-qp", "24", "-x264-params", "keyint=1"],
        },
    ]);
}

#[test]
fn inter_cavlc_and_cabac() {
    sweep(&[
        Case {
            tag: "p-cavlc",
            src: "testsrc2",
            size: "176x144",
            frames: 8,
            extra: &["-qp", "24", "-x264-params", "cabac=0:bframes=0"],
        },
        Case {
            tag: "b-cavlc",
            src: "testsrc2",
            size: "176x144",
            frames: 10,
            extra: &["-qp", "24", "-x264-params", "cabac=0:bframes=3"],
        },
        Case {
            tag: "b-cabac",
            src: "mandelbrot",
            size: "176x144",
            frames: 10,
            extra: &["-qp", "22", "-x264-params", "bframes=3:b-pyramid=normal"],
        },
        Case {
            tag: "b-cabac-direct-temporal",
            src: "testsrc2",
            size: "176x144",
            frames: 10,
            extra: &["-qp", "26", "-x264-params", "bframes=3:direct=temporal"],
        },
        Case {
            tag: "wp-explicit",
            src: "testsrc2",
            size: "176x144",
            frames: 10,
            extra: &[
                "-vf",
                "fade=in:0:10",
                "-qp",
                "24",
                "-x264-params",
                "weightp=2:weightb=1:bframes=2",
            ],
        },
        Case {
            tag: "wp-cavlc",
            src: "testsrc2",
            size: "176x144",
            frames: 10,
            extra: &[
                "-vf",
                "fade=in:0:10",
                "-qp",
                "24",
                "-x264-params",
                "cabac=0:weightp=2:weightb=1:bframes=2",
            ],
        },
        Case {
            tag: "wp-implicit",
            src: "testsrc2",
            size: "176x144",
            frames: 10,
            extra: &["-qp", "24", "-x264-params", "weightb=1:bframes=3:weightp=0"],
        },
        Case {
            tag: "p-qp1",
            src: "testsrc2",
            size: "128x96",
            frames: 5,
            extra: &["-qp", "1", "-x264-params", "bframes=2"],
        },
        Case {
            tag: "p-noise",
            src: "testsrc2",
            size: "128x96",
            frames: 5,
            extra: &[
                "-vf",
                "noise=alls=40:allf=t",
                "-qp",
                "12",
                "-x264-params",
                "cabac=0:bframes=2",
            ],
        },
        Case {
            tag: "p-negqp-cabac",
            src: "testsrc2",
            size: "128x96",
            frames: 6,
            extra: &["-x264-params", "qp=-6:crf=-1:bframes=2"],
        },
        Case {
            tag: "p-negqp-cavlc",
            src: "testsrc2",
            size: "128x96",
            frames: 6,
            extra: &["-x264-params", "qp=-9:crf=-1:bframes=2:cabac=0:weightp=2"],
        },
        Case {
            tag: "p-refs",
            src: "testsrc2",
            size: "176x144",
            frames: 12,
            extra: &[
                "-qp",
                "26",
                "-x264-params",
                "ref=5:bframes=3:subme=9:me=umh",
            ],
        },
        Case {
            tag: "p-no-deblock",
            src: "testsrc2",
            size: "176x144",
            frames: 6,
            extra: &["-qp", "26", "-x264-params", "no-deblock=1:bframes=2"],
        },
        Case {
            tag: "p-deblock-offsets",
            src: "testsrc2",
            size: "176x144",
            frames: 6,
            extra: &["-qp", "30", "-x264-params", "deblock=3,-2:bframes=2"],
        },
        Case {
            tag: "p-no8x8",
            src: "testsrc2",
            size: "176x144",
            frames: 6,
            extra: &["-qp", "26", "-x264-params", "8x8dct=0:bframes=2"],
        },
    ]);
}

#[test]
fn larger_pictures() {
    sweep(&[
        Case {
            tag: "cif",
            src: "testsrc2",
            size: "352x288",
            frames: 4,
            extra: &["-qp", "24", "-x264-params", "bframes=2"],
        },
        Case {
            tag: "720p",
            src: "testsrc2",
            size: "1280x720",
            frames: 2,
            extra: &["-qp", "26", "-x264-params", "bframes=1"],
        },
        Case {
            tag: "1080p-crop",
            src: "testsrc2",
            size: "1920x1080",
            frames: 2,
            extra: &["-qp", "28", "-x264-params", "bframes=1:cabac=0"],
        },
    ]);
}

/// The registry surface hands the same pictures out as `I010` frames.
#[test]
fn registry_decoder_emits_i010() {
    use ec_core::registry::{CodecId, CodecParameters, Decoder as _};
    use ec_core::{Frame, Packet, TimeBase};
    let stream = std::fs::read(data_dir().join("inter-cavlc-b.264")).unwrap();
    let want = std::fs::read_to_string(data_dir().join("inter-cavlc-b.md5")).unwrap();
    let mut dec = ec_h264::H264Decoder::new(CodecParameters::new(CodecId::H264)).unwrap();
    dec.send_packet(&Packet::new(0, TimeBase::new(1, 25), stream))
        .unwrap();
    dec.flush().unwrap();
    let mut all = Vec::new();
    while let Ok(Frame::Video(f)) = dec.receive_frame() {
        all.extend(frame_bytes(&f));
    }
    assert_eq!(md5(&all), want.split_whitespace().next().unwrap());
}
