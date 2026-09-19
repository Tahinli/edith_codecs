//! Odd coded-extent witness: a stream whose VP9 keyframe signals an odd
//! width and/or height must decode byte-exactly against ffmpeg's libvpx
//! output, chroma plane included. libvpx stores and crops chroma at
//! `uv_crop_width = (w + 1) / 2` / `uv_crop_height = (h + 1) / 2`, i.e. it
//! CEILS an odd coded extent; a `>> 1` layout is one column/row short and
//! clips the last chroma sample.
//!
//! libvpx-vp9's encoder rounds an odd source through its scaler, so the
//! fixture is an even 322x242 encode whose keyframe's uncompressed-header
//! size fields are patched to the odd target. The change is mi-grid
//! invariant for 322<->321 and 242<->241 (both ceil to 41x31 mi units), so
//! the tile data stays valid; every later frame inherits the odd size through
//! `found_ref`. ffmpeg's rawvideo output is the oracle, exactly as in
//! `inter_pixels_exact.rs`.

mod ivf;

use ec_vp9::decode::Decoder;
use std::path::Path;

/// Generate the even 322x242 GOP with libvpx (frame 0 the only keyframe, so
/// the whole stream inherits the patched size).
fn ensure_even_fixture(path: &Path) {
    if path.exists() {
        return;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    let st = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=322x242:rate=24:duration=0.5",
            "-c:v",
            "libvpx-vp9",
            "-g",
            "999",
            "-auto-alt-ref",
            "0",
            "-crf",
            "30",
            "-deadline",
            "good",
            "-cpu-used",
            "4",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "ivf",
        ])
        .arg(path)
        .output()
        .expect("ffmpeg with libvpx-vp9 must be on PATH for the witnesses");
    assert!(st.status.success(), "fixture generation failed: {st:?}");
}

/// Patch the FIRST frame's keyframe size fields and rewrite the IVF container
/// size. Profile 0, 8-bit, 4:2:0, `error_resilient = 0`: 8 header bits +
/// 24-bit sync code + 3 `color_space` + 1 `color_range` = the 16-bit
/// `width-1` / `height-1` fields start at bit 36 (VP9 reads MSB-first).
fn patched(ivf: &[u8], nw: u16, nh: u16) -> Vec<u8> {
    let mut d = ivf.to_vec();
    let payload = 32 + 12; // IVF file header + first frame chunk header
    for (base, value) in [(36usize, nw - 1), (52, nh - 1)] {
        for i in 0..16 {
            let bit = (value >> (15 - i)) & 1;
            let pos = base + i;
            let byte = payload + (pos >> 3);
            let mask = 1u8 << (7 - (pos & 7));
            if bit == 1 {
                d[byte] |= mask;
            } else {
                d[byte] &= !mask;
            }
        }
    }
    d[12..14].copy_from_slice(&nw.to_le_bytes());
    d[14..16].copy_from_slice(&nh.to_le_bytes());
    d
}

/// ffmpeg's decoded rawvideo byte stream (frame-packed I420, ceil chroma).
fn ffmpeg_raw_yuv(path: &Path) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg on PATH");
    assert!(out.status.success(), "ffmpeg decode failed: {out:?}");
    out.stdout
}

/// Decode the patched odd stream and compare every shown frame byte-exactly;
/// every frame must carry the odd coded size (frame 0 sets it, the rest
/// inherit it), and there must be inter frames.
fn check(nw: u16, nh: u16) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/vp9-witness");
    let even = dir.join("odd-src-322x242.ivf");
    ensure_even_fixture(&even);
    let odd = dir.join(format!("odd-{nw}x{nh}.ivf"));
    let patched = patched(&std::fs::read(&even).unwrap(), nw, nh);
    std::fs::write(&odd, &patched).unwrap();

    let cw = (nw as usize + 1) / 2;
    let ch = (nh as usize + 1) / 2;
    let frame_bytes = nw as usize * nh as usize + 2 * cw * ch;
    let expected = ffmpeg_raw_yuv(&odd);
    assert_eq!(
        expected.len() % frame_bytes,
        0,
        "ffmpeg's odd-size output is not frame-aligned"
    );

    let (_, _, _, frames) = ivf::parse_ivf(&patched);
    let mut decoder = Decoder::new();
    let mut shown = 0usize;
    for (i, f) in frames.iter().enumerate() {
        let Some(pic) = decoder.decode(&f.data).expect("odd frame decodes") else {
            continue;
        };
        assert_eq!(
            (pic.width, pic.height),
            (nw, nh),
            "frame {i} must carry the odd coded size"
        );
        assert_eq!(
            (pic.u.len(), pic.v.len()),
            (cw * ch, cw * ch),
            "frame {i} chroma must ceil the odd extent"
        );
        assert_eq!(pic.uv_stride, cw, "frame {i} chroma stride");
        let off = shown * frame_bytes;
        assert!(
            off + frame_bytes <= expected.len(),
            "frame {i}: ffmpeg has fewer frames than we show"
        );
        let mut got = Vec::with_capacity(frame_bytes);
        got.extend(pic.y.iter().map(|&v| v as u8));
        got.extend(pic.u.iter().map(|&v| v as u8));
        got.extend(pic.v.iter().map(|&v| v as u8));
        assert_eq!(
            got,
            expected[off..off + frame_bytes],
            "frame {i}: pixel mismatch vs ffmpeg ({nw}x{nh})"
        );
        shown += 1;
    }
    assert!(shown >= 2, "the odd stream must show a keyframe and inter frames");
    assert_eq!(shown * frame_bytes, expected.len(), "frame count mismatch");
}

#[test]
fn odd_width_matches_ffmpeg() {
    check(321, 242);
}

#[test]
fn odd_height_matches_ffmpeg() {
    check(322, 241);
}

#[test]
fn odd_width_and_height_matches_ffmpeg() {
    check(321, 241);
}
