//! AV1 film grain on the hardware path.
//!
//! The grain fixture is an 8-bit 4:2:0 stream whose sequence header sets
//! `film_grain_params_present`. The driver applies the grain (spec 7.18.3) onto
//! a separate display surface, so the picture comes out grained while the
//! decoded reference stays clean. The oracles are two independent decoders
//! that both apply grain: `ec-av1` (this family's software decoder) and ffmpeg
//! (libdav1d). Per-frame deviations are reported, not just asserted.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

use ec_hw::{Codec, Decoder, I420};
use ec_va::Display;

/// Frames in the grain fixture (every one of them is shown).
const FRAMES: usize = 90;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bitstreams/av1-grain.ivf")
}

fn display() -> Option<Rc<Display>> {
    match ec_va::open() {
        Ok(display) => Some(display),
        Err(e) => {
            eprintln!("skipped: no VA display ({e})");
            None
        }
    }
}

/// Split an IVF container into its frames (temporal units).
fn split_ivf(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if data.len() < 32 || &data[..4] != b"DKIF" {
        return out;
    }
    let header = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut pos = header.max(32);
    while pos + 12 <= data.len() {
        let size =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        if pos + size > data.len() {
            break;
        }
        out.push(&data[pos..pos + size]);
        pos += size;
    }
    out
}

/// Three 8-bit planes, widened so every comparison is in one type.
#[derive(Clone)]
struct Planes {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Planes {
    fn from_i420(f: &I420) -> Planes {
        Planes {
            y: f.y.clone(),
            u: f.u.clone(),
            v: f.v.clone(),
        }
    }

    fn from_slices(y: &[u16], u: &[u16], v: &[u16]) -> Planes {
        let conv = |p: &[u16]| p.iter().map(|&s| s as u8).collect();
        Planes {
            y: conv(y),
            u: conv(u),
            v: conv(v),
        }
    }
}

/// Largest absolute sample difference over the three planes.
fn max_abs(a: &Planes, b: &Planes) -> u8 {
    [(&a.y, &b.y), (&a.u, &b.u), (&a.v, &b.v)]
        .into_iter()
        .flat_map(|(x, y)| x.iter().zip(y.iter()).map(|(p, q)| p.abs_diff(*q)))
        .max()
        .unwrap_or(0)
}

/// PSNR over the three planes in dB; infinite when the frames are identical.
fn psnr(a: &Planes, b: &Planes) -> f64 {
    let mut sum = 0f64;
    let mut n = 0usize;
    for (x, y) in [(&a.y, &b.y), (&a.u, &b.u), (&a.v, &b.v)] {
        for (p, q) in x.iter().zip(y.iter()) {
            let d = f64::from(*p) - f64::from(*q);
            sum += d * d;
        }
        n += x.len().min(y.len());
    }
    if n == 0 {
        return 0.0;
    }
    let mse = sum / n as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

/// Decode the fixture on the hardware, reading each shown frame back.
fn hardware_frames(display: &Rc<Display>, data: &[u8]) -> Vec<(Planes, (u32, u32))> {
    let mut decoder = Decoder::new(display, Codec::Av1).expect("AV1 decoder");
    let mut out = Vec::new();
    for (i, unit) in split_ivf(data).into_iter().enumerate() {
        decoder.decode(unit, i as i64).expect("hardware decode");
        while let Some(frame) = decoder.next_frame() {
            let size = frame.display_size;
            let i420 = frame.to_i420().expect("read back the frame");
            out.push((Planes::from_i420(&i420), size));
        }
    }
    decoder.flush().expect("flush");
    while let Some(frame) = decoder.next_frame() {
        let size = frame.display_size;
        let i420 = frame.to_i420().expect("read back the frame");
        out.push((Planes::from_i420(&i420), size));
    }
    out
}

/// ec-av1's software decode of the same bytes, grain applied.
fn software_frames(data: &[u8]) -> Vec<Planes> {
    let mut stream = Vec::new();
    for unit in split_ivf(data) {
        stream.extend_from_slice(unit);
    }
    ec_av1::stream::decode_stream(&stream)
        .expect("ec-av1 decodes the grain fixture")
        .iter()
        .map(|p| Planes::from_slices(&p.y, &p.u, &p.v))
        .collect()
}

/// ffmpeg's decode (libdav1d, grain applied by default) as raw 4:2:0 planes.
fn ffmpeg_frames(path: &Path, width: usize, height: usize) -> Option<Vec<Planes>> {
    let output = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "rawvideo",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let mut bytes = output.stdout.as_slice();
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let frame = width * height + 2 * cw * ch;
    let mut out = Vec::new();
    while bytes.len() >= frame {
        let (f, rest) = bytes.split_at(frame);
        let (y, uv) = f.split_at(width * height);
        let (u, v) = uv.split_at(cw * ch);
        out.push(Planes {
            y: y.to_vec(),
            u: u.to_vec(),
            v: v.to_vec(),
        });
        bytes = rest;
    }
    Some(out)
}

/// The grain fixture decodes on the hardware with grain applied, and matches
/// the two software decoders frame by frame.
#[test]
fn hardware_grain_matches_software_decoders() {
    let path = fixture();
    let Ok(data) = std::fs::read(&path) else {
        // `EC_REQUIRE_FIXTURES=1` turns an absent fixture into a failure, so a
        // checkout that never generated it cannot report this test green.
        assert!(
            std::env::var_os("EC_REQUIRE_FIXTURES").is_none(),
            "EC_REQUIRE_FIXTURES=1 but fixture {} is absent",
            path.display()
        );
        eprintln!(
            "skipped: {} is absent (extract it from test_av1_grain.mkv)",
            path.display()
        );
        return;
    };
    let Some(display) = display() else {
        return;
    };

    let hardware = hardware_frames(&display, &data);
    assert_eq!(
        hardware.len(),
        FRAMES,
        "every frame decoded on the hardware"
    );
    let (w, h) = hardware[0].1;

    let software = software_frames(&data);

    assert_eq!(software.len(), FRAMES, "ec-av1 decodes every frame");
    let ffmpeg = ffmpeg_frames(&path, w as usize, h as usize)
        .expect("ffmpeg is available to make the reference");
    assert_eq!(ffmpeg.len(), FRAMES, "ffmpeg decodes every frame");

    let mut worst_sw = 0u8;
    let mut worst_ff = 0u8;
    for (i, (hw, _)) in hardware.iter().enumerate() {
        let sw_diff = max_abs(hw, &software[i]);
        let ff_diff = max_abs(hw, &ffmpeg[i]);
        worst_sw = worst_sw.max(sw_diff);
        worst_ff = worst_ff.max(ff_diff);
        eprintln!(
            "frame {i:3}: hw vs ec-av1 max {sw_diff:3} psnr {:6.2} dB | hw vs ffmpeg max {ff_diff:3} psnr {:6.2} dB",
            psnr(hw, &software[i]),
            psnr(hw, &ffmpeg[i]),
        );
    }
    eprintln!("worst: hw vs ec-av1 max {worst_sw}, hw vs ffmpeg max {worst_ff}");

    // Bit-exact against both software decoders: the grain synthesis is a
    // pure function of the picture and its parameters (spec 7.18.3).
    assert_eq!(worst_sw, 0, "hardware grain deviates from ec-av1");
    assert_eq!(worst_ff, 0, "hardware grain deviates from ffmpeg");
}
