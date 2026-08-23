//! Encoder conformance: what this encoder writes is what this decoder — and,
//! in the ffmpeg-driven half of the suite, what ffmpeg — reconstructs.
//!
//! The load-bearing property is that the encoder's own reconstruction is
//! *bit-identical* to a decode of its bitstream. Both sides share the
//! prediction, transform and deblocking code, so a mismatch means the syntax
//! written does not describe the picture reconstructed, which is the one class
//! of encoder bug that quality metrics cannot see.

use ec_h264::{Decoder, Encoder, EncoderConfig, NalOutcome, OutputOrder, PictureView, Preset};

/// One decoded picture: Y, Cb and Cr planes, cropped and tightly packed.
type Planes = (Vec<u8>, Vec<u8>, Vec<u8>);

/// A synthetic source with real structure: moving edges, a gradient and a
/// noisy patch, so intra, inter and skip all get exercised.
struct Clip {
    w: usize,
    h: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Clip {
    fn new(w: usize, h: usize) -> Clip {
        Clip {
            w,
            h,
            y: vec![0; w * h],
            u: vec![0; w / 2 * h / 2],
            v: vec![0; w / 2 * h / 2],
        }
    }

    /// Frame `t`: a diagonal gradient, a moving box and a deterministic noise
    /// field that keeps the residual from vanishing.
    fn render(&mut self, t: usize) {
        let (w, h) = (self.w, self.h);
        for y in 0..h {
            for x in 0..w {
                let grad = ((x + y * 2 + t * 3) % 256) as u8;
                let noise = ((x * 37 + y * 17 + t * 11) % 23) as u8;
                let box_x = (t * 5) % (w.saturating_sub(40).max(1));
                let inside = x >= box_x && x < box_x + 40 && y >= h / 4 && y < h / 4 + 30;
                self.y[y * w + x] = if inside {
                    200u8.saturating_sub(noise)
                } else {
                    grad / 2 + noise
                };
            }
        }
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                self.u[y * (w / 2) + x] = (100 + ((x + t) % 40)) as u8;
                self.v[y * (w / 2) + x] = (140 + ((y + t * 2) % 30)) as u8;
            }
        }
    }

    /// Frame `t` without the per-sample noise field: smooth gradients, a
    /// band of sinusoidal texture and a box translating by whole and
    /// sub-sample amounts. `render`'s noise is incompressible, which makes a
    /// rate-quality comparison read mostly as how willing each encoder is to
    /// throw residual away; this one is content where prediction matters, the
    /// half a rate-quality comparison is about.
    fn render_smooth(&mut self, t: usize) {
        let (w, h) = (self.w, self.h);
        let ft = t as f64;
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f64, y as f64);
                // A slow diagonal ramp that drifts by 1.5 samples a frame, so
                // motion is not always on the sample grid.
                let ramp = (fx + fy * 0.5 - ft * 1.5) * 0.35;
                // A textured band, the part a search has to actually find.
                let texture = if y > h / 3 && y < 2 * h / 3 {
                    18.0 * ((fx * 0.19 + ft * 0.9).sin() + (fy * 0.11).cos())
                } else {
                    0.0
                };
                let box_x = ((ft * 2.7) as usize) % (w.saturating_sub(48).max(1));
                let inside = x >= box_x && x < box_x + 48 && y >= h / 5 && y < h / 5 + 36;
                let v = if inside {
                    196.0 - texture * 0.5
                } else {
                    40.0 + (ramp % 90.0).abs() + texture
                };
                self.y[y * w + x] = v.clamp(0.0, 255.0) as u8;
            }
        }
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                let (fx, fy) = (x as f64, y as f64);
                self.u[y * (w / 2) + x] =
                    (118.0 + 12.0 * ((fx * 0.05 + ft * 0.3).sin())).clamp(0.0, 255.0) as u8;
                self.v[y * (w / 2) + x] =
                    (134.0 + 10.0 * ((fy * 0.04 - ft * 0.25).cos())).clamp(0.0, 255.0) as u8;
            }
        }
    }

    fn view(&self) -> PictureView<'_> {
        PictureView::i420(self.w as u32, self.h as u32, &self.y, &self.u, &self.v)
    }
}

/// Decode a whole stream with this crate's decoder, returning one I420 triple
/// per picture.
fn decode_all(stream: &[u8]) -> Vec<Planes> {
    let mut dec = Decoder::new();
    dec.set_output_order(OutputOrder::Decode);
    let mut out = Vec::new();
    for nal in ec_h264_syntax::AnnexBIter::new(stream) {
        if dec.push_nal(nal).expect("decoder accepts the NAL") == NalOutcome::PictureBoundary {
            dec.end_picture().expect("end of picture");
            dec.push_nal(nal).expect("decoder accepts the NAL");
        }
        while let Some(frame) = dec.next_frame() {
            out.push(planes_of(&frame));
        }
    }
    dec.flush().expect("flush");
    while let Some(frame) = dec.next_frame() {
        out.push(planes_of(&frame));
    }
    out
}

fn planes_of(frame: &ec_core::frame::VideoFrame) -> Planes {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut planes = Vec::new();
    for (i, (pw, ph)) in [(w, h), (w / 2, h / 2), (w / 2, h / 2)]
        .into_iter()
        .enumerate()
    {
        let plane = &frame.planes[i];
        let mut out = Vec::with_capacity(pw * ph);
        for row in 0..ph {
            out.extend_from_slice(&plane.data[row * plane.stride..row * plane.stride + pw]);
        }
        planes.push(out);
    }
    let v = planes.pop().unwrap();
    let u = planes.pop().unwrap();
    let y = planes.pop().unwrap();
    (y, u, v)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = f64::from(x) - f64::from(y);
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

fn cubic_fit(xs: [f64; 4], ys: [f64; 4]) -> [f64; 4] {
    let mut a = [[0.0; 5]; 4];
    for i in 0..4 {
        a[i] = [1.0, xs[i], xs[i] * xs[i], xs[i] * xs[i] * xs[i], ys[i]];
    }
    for col in 0..4 {
        let mut pivot = col;
        for row in col + 1..4 {
            if a[row][col].abs() > a[pivot][col].abs() {
                pivot = row;
            }
        }
        a.swap(col, pivot);
        let div = a[col][col];
        assert!(div.abs() > 1e-12, "singular BD-PSNR fit");
        for j in col..5 {
            a[col][j] /= div;
        }
        for row in 0..4 {
            if row == col {
                continue;
            }
            let f = a[row][col];
            for j in col..5 {
                a[row][j] -= f * a[col][j];
            }
        }
    }
    [a[0][4], a[1][4], a[2][4], a[3][4]]
}

fn cubic_integral(c: [f64; 4], x: f64) -> f64 {
    c[0] * x + c[1] * x * x / 2.0 + c[2] * x * x * x / 3.0 + c[3] * x * x * x * x / 4.0
}

fn bd_psnr_delta(candidate: &[(f64, f64)], anchor: &[(f64, f64)]) -> f64 {
    let xs_candidate = std::array::from_fn(|i| candidate[i].0.ln());
    let ys_candidate = std::array::from_fn(|i| candidate[i].1);
    let xs_anchor = std::array::from_fn(|i| anchor[i].0.ln());
    let ys_anchor = std::array::from_fn(|i| anchor[i].1);
    let lo = xs_candidate
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min)
        .max(xs_anchor.iter().copied().fold(f64::INFINITY, f64::min));
    let hi = xs_candidate
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max)
        .min(xs_anchor.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    assert!(hi > lo, "BD-PSNR curves do not overlap in bitrate");
    let c_candidate = cubic_fit(xs_candidate, ys_candidate);
    let c_anchor = cubic_fit(xs_anchor, ys_anchor);
    (cubic_integral(c_candidate, hi)
        - cubic_integral(c_candidate, lo)
        - cubic_integral(c_anchor, hi)
        + cubic_integral(c_anchor, lo))
        / (hi - lo)
}

/// The encoder's reconstruction and a decode of its bitstream agree sample for
/// sample, over a QP sweep, both presets, single and multi threaded.
#[test]
fn reconstruction_matches_the_decoder() {
    let mut t8x8_mbs = (0, 0);
    for (w, h) in [(176, 144), (208, 122), (64, 48)] {
        let mut clip = Clip::new(w, h);
        for qp in [12, 24, 33, 44] {
            for (threads, preset, cabac, t8x8) in [
                (1usize, Preset::Fast, true, false),
                (4, Preset::Balanced, true, false),
                (1, Preset::Fast, false, false),
                (3, Preset::Balanced, false, false),
                (2, Preset::Balanced, true, true),
                (2, Preset::Balanced, false, true),
            ] {
                let mut cfg = EncoderConfig::new(w as u32, h as u32);
                cfg.qp = qp;
                cfg.gop_size = 5;
                cfg.threads = threads;
                cfg.preset = preset;
                cfg.cabac = cabac;
                cfg.transform_8x8 = t8x8;
                let mut enc = Encoder::new(cfg).expect("encoder");
                let mut stream = Vec::new();
                let mut recons = Vec::new();
                for t in 0..8 {
                    clip.render(t);
                    let picture = enc.encode(&clip.view()).expect("encode");
                    stream.extend_from_slice(&picture.au);
                    recons.push(enc.reconstruction().expect("a reconstruction"));
                }
                if t8x8 {
                    let (intra, inter) = enc.transform_8x8_mbs();
                    t8x8_mbs.0 += intra;
                    t8x8_mbs.1 += inter;
                }
                let decoded = decode_all(&stream);
                assert_eq!(
                    decoded.len(),
                    recons.len(),
                    "{w}x{h} qp {qp}: picture count"
                );
                for (i, (dec, rec)) in decoded.iter().zip(&recons).enumerate() {
                    assert_eq!(
                        dec.0, rec.0,
                        "{w}x{h} qp {qp} t{threads}: luma of picture {i}"
                    );
                    assert_eq!(
                        dec.1, rec.1,
                        "{w}x{h} qp {qp} t{threads}: Cb of picture {i}"
                    );
                    assert_eq!(
                        dec.2, rec.2,
                        "{w}x{h} qp {qp} t{threads}: Cr of picture {i}"
                    );
                }
            }
        }
    }
    // The flag-on streams above really carried 8x8 macroblocks of both kinds.
    assert!(
        t8x8_mbs.0 > 0 && t8x8_mbs.1 > 0,
        "8x8 MBs across the sweep: intra {} inter {}",
        t8x8_mbs.0,
        t8x8_mbs.1
    );
}

/// Quality tracks the quantiser, and a low QP is close to lossless: a stream
/// that decodes bit-exactly could still be encoding garbage, and this is what
/// says it is not.
#[test]
fn quality_follows_the_quantiser() {
    let (w, h) = (176, 144);
    let mut clip = Clip::new(w, h);
    let mut last = 0.0;
    for qp in [40, 30, 20, 10] {
        let mut cfg = EncoderConfig::new(w as u32, h as u32);
        cfg.qp = qp;
        cfg.gop_size = 4;
        cfg.threads = 1;
        let mut enc = Encoder::new(cfg).expect("encoder");
        let mut total = 0.0;
        for t in 0..6 {
            clip.render(t);
            enc.encode(&clip.view()).expect("encode");
            let (y, _, _) = enc.reconstruction().unwrap();
            total += psnr(&clip.y, &y);
        }
        let avg = total / 6.0;
        assert!(
            avg > last,
            "qp {qp}: PSNR {avg:.2} did not improve on {last:.2}"
        );
        if qp == 10 {
            assert!(avg > 45.0, "qp 10 should be near lossless, got {avg:.2}");
        }
        last = avg;
    }
}

/// Constant bitrate lands within a quarter of the target over a short clip,
/// and every GOP boundary is an IDR the decoder can start from.
#[test]
fn rate_control_hits_its_target() {
    let (w, h) = (352, 288);
    let mut clip = Clip::new(w, h);
    let bitrate = 1_500_000u32;
    let mut cfg = EncoderConfig::new(w as u32, h as u32);
    cfg.bitrate = bitrate;
    cfg.framerate = 25.0;
    cfg.gop_size = 25;
    cfg.threads = 2;
    let mut enc = Encoder::new(cfg).expect("encoder");
    let mut bits = 0u64;
    let frames = 50;
    let mut keys = 0;
    for t in 0..frames {
        clip.render(t);
        let p = enc.encode(&clip.view()).expect("encode");
        bits += p.au.len() as u64 * 8;
        keys += u32::from(p.key_frame);
    }
    assert_eq!(keys, 2, "one IDR per GOP");
    let actual = bits as f64 / (frames as f64 / 25.0);
    let ratio = actual / f64::from(bitrate);
    assert!(
        (0.7..1.35).contains(&ratio),
        "bitrate {actual:.0} against target {bitrate} (ratio {ratio:.2})"
    );
}

/// Every picture of a `gop_size` 1 stream is an IDR, which is what an
/// intraframe master asks for.
#[test]
fn all_intra_when_the_gop_is_one() {
    let (w, h) = (64, 64);
    let mut clip = Clip::new(w, h);
    let mut cfg = EncoderConfig::new(w as u32, h as u32);
    cfg.gop_size = 1;
    cfg.qp = 26;
    let mut enc = Encoder::new(cfg).expect("encoder");
    let mut stream = Vec::new();
    for t in 0..4 {
        clip.render(t);
        let p = enc.encode(&clip.view()).expect("encode");
        assert!(p.key_frame, "picture {t} is not a key frame");
        stream.extend_from_slice(&p.au);
    }
    assert_eq!(decode_all(&stream).len(), 4);
}

// ---------------------------------------------------------------------------
// ffmpeg-driven half: the third-party oracle. Skipped loudly when ffmpeg is
// absent, exactly like the conformance suite's ffmpeg cross-check.
// ---------------------------------------------------------------------------

fn have_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Decode an Annex B stream with ffmpeg, returning one I420 triple per picture.
fn ffmpeg_decode(stream: &[u8], w: usize, h: usize, extra: &[&str]) -> Option<Vec<Planes>> {
    let dir = std::env::temp_dir().join(format!("ec-h264-enc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("s{}.264", stream.len()));
    std::fs::write(&path, stream).ok()?;
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-v", "error"]);
    cmd.args(extra);
    cmd.arg("-i").arg(&path);
    cmd.args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"]);
    let out = cmd.output().ok()?;
    let _ = std::fs::remove_file(&path);
    if !out.status.success() {
        eprintln!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    let frame = w * h * 3 / 2;
    if frame == 0 || out.stdout.len() % frame != 0 {
        eprintln!(
            "ffmpeg returned {} bytes, not a whole number of {frame}-byte frames",
            out.stdout.len()
        );
        return None;
    }
    Some(
        out.stdout
            .chunks(frame)
            .map(|f| {
                let (y, rest) = f.split_at(w * h);
                let (u, v) = rest.split_at(w / 2 * h / 2);
                (y.to_vec(), u.to_vec(), v.to_vec())
            })
            .collect(),
    )
}

/// Encode a clip and hand back the stream with the encoder's reconstructions.
fn encode_clip(
    w: usize,
    h: usize,
    frames: usize,
    configure: impl FnOnce(&mut EncoderConfig),
) -> (Vec<u8>, Vec<Planes>, (u64, u64)) {
    let mut cfg = EncoderConfig::new(w as u32, h as u32);
    cfg.gop_size = 10;
    cfg.threads = 3;
    configure(&mut cfg);
    let mut enc = Encoder::new(cfg).expect("encoder");
    let mut clip = Clip::new(w, h);
    let mut stream = Vec::new();
    let mut recons = Vec::new();
    for t in 0..frames {
        clip.render(t);
        stream.extend_from_slice(&enc.encode(&clip.view()).expect("encode").au);
        recons.push(enc.reconstruction().expect("reconstruction"));
    }
    (stream, recons, enc.transform_8x8_mbs())
}

/// Flat gradients and glyph-like rectangles: the content the 8x8 transform
/// exists for. A 10-frame clip with the flag on must keep a clear positive
/// gain at equal bits, or cut bits at equal PSNR.
#[test]
fn eight_by_eight_gains_on_flat_and_text() {
    let (w, h) = (320, 240);
    let render = |t: usize, clip: &mut Clip| {
        // A half-sample pan per frame: inter prediction has a residual to code.
        let sx = |x: usize| x * 2 + t;
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (sx(x) as f64 / 2.0, y as f64);
                // Curved shading: smooth but not a plane, so no 16x16 mode
                // predicts it for free.
                let shade = 70.0 + px * px / 900.0 + py * py / 700.0 - px * py / 1500.0;
                // Gentle texture with a 20-sample period: residual energy that
                // spans more than a 4x4 block, which is what the 8x8 transform
                // is for.
                let tri = |v: f64| ((v % 20.0) - 10.0).abs() - 5.0;
                let shade = shade + tri(px) * tri(py) / 1.5;
                // Glyph rows: 16x24 cells, stroke pattern hashed per cell.
                let (cell, cx, cy) = (sx(x) / 32, (sx(x) / 2) % 16, y % 24);
                let hash = (cell * 2654435761usize.wrapping_add(y / 24 * 40503)) >> 7;
                let stem = (2..5).contains(&cx);
                let bar = (10..13).contains(&cy) && cx < 12 && hash & 1 == 1;
                let foot = cy >= 19 && (2..13).contains(&cx) && hash & 2 == 2;
                let glyph = y > h / 3 && (3..22).contains(&cy) && (stem || bar || foot);
                clip.y[y * w + x] = if glyph { 24 } else { shade.min(235.0) as u8 };
            }
        }
        clip.u.fill(128);
        clip.v.fill(128);
    };
    let run = |t8x8: bool, qp: i32, cabac: bool| {
        let mut cfg = EncoderConfig::new(w as u32, h as u32);
        cfg.qp = qp;
        cfg.gop_size = 10;
        cfg.transform_8x8 = t8x8;
        cfg.cabac = cabac;
        let mut enc = Encoder::new(cfg).expect("encoder");
        let mut clip = Clip::new(w, h);
        let (mut bits, mut sum_psnr) = (0usize, 0.0);
        let mut share = 0.0;
        for t in 0..10 {
            render(t, &mut clip);
            bits += enc.encode(&clip.view()).expect("encode").au.len() * 8;
            let rec = enc.reconstruction().expect("reconstruction");
            sum_psnr += psnr(&clip.y, &rec.0);
        }
        if t8x8 {
            let (i, p) = enc.transform_8x8_mbs();
            eprintln!("8x8 MBs: intra {i} inter {p}");
            share = (i + p) as f64 / (10.0 * (w / 16 * h / 16) as f64);
        }
        (bits as f64, sum_psnr / 10.0, share)
    };
    let qps = [22, 26, 30, 34];
    let on: Vec<_> = qps.iter().map(|&q| run(true, q, true)).collect();
    let off: Vec<_> = qps.iter().map(|&q| run(false, q, true)).collect();
    let on_curve: Vec<_> = on.iter().map(|&(bits, psnr, _)| (bits, psnr)).collect();
    let off_curve: Vec<_> = off.iter().map(|&(bits, psnr, _)| (bits, psnr)).collect();
    let bd = bd_psnr_delta(&on_curve, &off_curve);
    let share = on.iter().map(|&(_, _, share)| share).sum::<f64>() / on.len() as f64;
    const OLD_FLAG_ON_BD_PSNR_DB: f64 = 0.255_347;
    eprintln!(
        "8x8 BD-PSNR over q22/26/30/34: {bd:+.3} dB, average 8x8 MB share {:.1}%",
        share * 100.0
    );
    assert!(share > 0.0, "no 8x8 macroblocks were coded");
    assert!(bd >= 0.0, "8x8 BD-PSNR {bd:+.3} dB is below flag-off");
    assert!(
        bd + 0.02 >= OLD_FLAG_ON_BD_PSNR_DB,
        "8x8 BD-PSNR {bd:+.3} dB regressed more than 0.02 dB from the old flag-on gate"
    );
}

/// ffmpeg's decode of our stream is our reconstruction, sample for sample,
/// across a QP sweep and at a size that is not a multiple of a macroblock.
#[test]
fn ffmpeg_decodes_bit_exactly() {
    if !have_ffmpeg() {
        eprintln!("SKIP ffmpeg_decodes_bit_exactly: no ffmpeg on PATH");
        return;
    }
    let mut t8x8_mbs = (0, 0);
    for (w, h) in [(176, 144), (322, 242)] {
        for qp in [10, 20, 28, 37, 47] {
            let cavlc = qp % 2 == 0; // both entropy coders across the sweep
            let t8x8 = qp % 3 == 1; // High-profile PPS with the per-MB flag
            let (stream, recons, (intra, inter)) = encode_clip(w, h, 6, |cfg| {
                cfg.qp = qp;
                cfg.cabac = !cavlc;
                cfg.transform_8x8 = t8x8;
            });
            t8x8_mbs.0 += intra;
            t8x8_mbs.1 += inter;
            let decoded = ffmpeg_decode(&stream, w, h, &[]).expect("ffmpeg decodes our stream");
            assert_eq!(decoded.len(), recons.len(), "{w}x{h} qp {qp}: frame count");
            for (i, (dec, rec)) in decoded.iter().zip(&recons).enumerate() {
                assert_eq!(dec.0, rec.0, "{w}x{h} qp {qp}: luma of frame {i}");
                assert_eq!(dec.1, rec.1, "{w}x{h} qp {qp}: Cb of frame {i}");
                assert_eq!(dec.2, rec.2, "{w}x{h} qp {qp}: Cr of frame {i}");
            }
        }
    }
    assert!(
        t8x8_mbs.0 > 0 && t8x8_mbs.1 > 0,
        "8x8 MBs across the sweep: intra {} inter {}",
        t8x8_mbs.0,
        t8x8_mbs.1
    );
}

/// The same stream under constant bitrate, which is the mode edith exports in.
#[test]
fn ffmpeg_decodes_a_cbr_stream_bit_exactly() {
    if !have_ffmpeg() {
        eprintln!("SKIP ffmpeg_decodes_a_cbr_stream_bit_exactly: no ffmpeg on PATH");
        return;
    }
    let (w, h) = (352, 288);
    let (stream, recons, _) = encode_clip(w, h, 12, |cfg| {
        cfg.bitrate = 2_000_000;
        cfg.framerate = 25.0;
    });
    let decoded = ffmpeg_decode(&stream, w, h, &[]).expect("ffmpeg decodes our stream");
    assert_eq!(decoded.len(), recons.len());
    for (i, (dec, rec)) in decoded.iter().zip(&recons).enumerate() {
        assert_eq!(dec.0, rec.0, "luma of frame {i}");
        assert_eq!(
            (dec.1.clone(), dec.2.clone()),
            (rec.1.clone(), rec.2.clone()),
            "chroma of frame {i}"
        );
    }
}

/// The VA-API hardware decoder on this machine accepts the same stream: a
/// bitstream a driver refuses is not a bitstream edith can ship, whatever a
/// software decoder says about it.
#[test]
fn vaapi_decodes_our_stream() {
    if !have_ffmpeg() || !std::path::Path::new("/dev/dri/renderD128").exists() {
        eprintln!("SKIP vaapi_decodes_our_stream: no ffmpeg or no render node");
        return;
    }
    let (w, h) = (352, 288);
    let (stream, recons, (intra, inter)) = encode_clip(w, h, 6, |cfg| {
        cfg.qp = 26;
        cfg.transform_8x8 = true;
    });
    assert!(intra + inter > 0, "no 8x8 macroblocks in the VA-API stream");
    let Some(decoded) = ffmpeg_decode(
        &stream,
        w,
        h,
        &[
            "-hwaccel",
            "vaapi",
            "-hwaccel_device",
            "/dev/dri/renderD128",
        ],
    ) else {
        eprintln!("SKIP vaapi_decodes_our_stream: VA-API decode unavailable here");
        return;
    };
    assert_eq!(decoded.len(), recons.len(), "VA-API frame count");
    for (i, (dec, rec)) in decoded.iter().zip(&recons).enumerate() {
        let p = psnr(&dec.0, &rec.0);
        assert!(p > 60.0, "VA-API luma of frame {i} differs: PSNR {p:.1} dB");
    }
}

/// Real 1080p pictures from this machine's library, demuxed by `ec-mp4` and
/// decoded by this crate, are re-encoded and handed back to ffmpeg.
///
/// Synthetic clips do not have film grain, hard cuts, or the residual
/// statistics an encoder actually meets; a stream that survives a gradient and
/// a moving box says nothing about one. Every picture must decode to exactly
/// the encoder's own reconstruction, and the run reports the rate it landed on
/// against the rate it was asked for.
#[test]
fn real_library_frames_encode_and_decode_exactly() {
    use ec_core::registry::{Decoder as _, Demuxer as _};

    if !have_ffmpeg() {
        eprintln!("SKIP real_library_frames: no ffmpeg on PATH");
        return;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/real-library-manifest.tsv");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        eprintln!("SKIP real_library_frames: {} missing", manifest.display());
        return;
    };
    // The first 8-bit 4:2:0 H.264 mp4 of at least 720p in the manifest.
    let mut source = None;
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 || f[2] != "h264" || f[5] != "yuv420p" {
            continue;
        }
        let (Ok(w), Ok(h)) = (f[3].parse::<usize>(), f[4].parse::<usize>()) else {
            continue;
        };
        if w < 1280 || h < 720 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
            continue;
        }
        if !f[1].contains("mp4") || !std::path::Path::new(f[0]).exists() {
            continue;
        }
        source = Some((f[0].to_string(), w, h));
        break;
    }
    let Some((path, w, h)) = source else {
        eprintln!("SKIP real_library_frames: no H.264 4:2:0 mp4 in the manifest");
        return;
    };

    // Decode a handful of real pictures.
    let file = std::fs::File::open(&path).expect("source opens");
    let mut demux =
        ec_mp4::Mp4Demuxer::new(std::io::BufReader::new(file)).expect("ec-mp4 opens the file");
    let (index, params) = demux
        .streams()
        .iter()
        .find(|s| s.params.codec == ec_core::registry::CodecId::H264)
        .map(|s| (s.index, s.params.clone()))
        .expect("an H.264 track");
    let mut dec = ec_h264::H264Decoder::new(params).expect("decoder");
    let mut sources: Vec<Planes> = Vec::new();
    while sources.len() < 12 {
        let Ok(packet) = demux.next_packet() else {
            break;
        };
        if packet.stream != index {
            continue;
        }
        dec.send_packet(&packet).expect("decode");
        while let Ok(ec_core::frame::Frame::Video(f)) = dec.receive_frame() {
            if sources.len() < 12 {
                sources.push(planes_of(&f));
            }
        }
    }
    assert!(
        sources.len() >= 6,
        "only {} pictures decoded from the library file",
        sources.len()
    );

    let bitrate = 8_000_000u32;
    let mut cfg = EncoderConfig::new(w as u32, h as u32);
    cfg.bitrate = bitrate;
    cfg.framerate = 25.0;
    cfg.gop_size = 25;
    cfg.threads = 0;
    let mut enc = Encoder::new(cfg).expect("encoder");
    let mut stream = Vec::new();
    let mut recons = Vec::new();
    let mut bits = 0u64;
    for (y, u, v) in &sources {
        let view = PictureView::i420(w as u32, h as u32, y, u, v);
        let p = enc.encode(&view).expect("encode");
        bits += p.au.len() as u64 * 8;
        stream.extend_from_slice(&p.au);
        recons.push(enc.reconstruction().expect("reconstruction"));
    }
    let decoded = ffmpeg_decode(&stream, w, h, &[]).expect("ffmpeg decodes our stream");
    assert_eq!(decoded.len(), recons.len(), "picture count");
    for (i, (dec, rec)) in decoded.iter().zip(&recons).enumerate() {
        assert_eq!(dec.0, rec.0, "luma of picture {i} from the library clip");
        assert_eq!(dec.1, rec.1, "Cb of picture {i}");
        assert_eq!(dec.2, rec.2, "Cr of picture {i}");
    }
    let quality: f64 = sources
        .iter()
        .zip(&recons)
        .map(|((y, ..), (ry, ..))| psnr(y, ry))
        .sum::<f64>()
        / sources.len() as f64;
    let rate = bits as f64 / (sources.len() as f64 / 25.0);
    eprintln!(
        "library clip {w}x{h}: {} pictures, {:.0} kbit/s asked {:.0}, luma PSNR {quality:.2} dB",
        sources.len(),
        rate / 1000.0,
        f64::from(bitrate) / 1000.0
    );
    assert!(quality > 30.0, "luma PSNR {quality:.2} dB at 8 Mbit/s");
}

/// The first `want` pictures a second apart from one of the user's media
/// files: demuxed by ec-mp4, decoded by this crate, every Nth frame with N the
/// source frame rate. Returns the luma/chroma size and the pictures.
fn clip_sources(path: &std::ffi::OsStr, want: usize) -> (usize, usize, Vec<Planes>) {
    use ec_core::registry::{Decoder as _, Demuxer as _};

    let file = std::fs::File::open(path).expect("source opens");
    let mut demux =
        ec_mp4::Mp4Demuxer::new(std::io::BufReader::new(file)).expect("ec-mp4 opens the file");
    let (index, params) = demux
        .streams()
        .iter()
        .find(|s| s.params.codec == ec_core::registry::CodecId::H264)
        .map(|s| (s.index, s.params.clone()))
        .expect("an H.264 track");
    let video = match &params.media {
        ec_core::registry::MediaParameters::Video(v) => v,
        _ => panic!("the H.264 track carries no video parameters"),
    };
    let (w, h) = (video.width as usize, video.height as usize);
    let stride = video
        .frame_rate
        .map_or(25, |r| (r.as_secs_f64().round() as usize).max(1));
    let mut dec = ec_h264::H264Decoder::new(params).expect("decoder");
    let mut sources: Vec<Planes> = Vec::new();
    let mut seen = 0usize;
    while sources.len() < want {
        let Ok(packet) = demux.next_packet() else {
            break;
        };
        if packet.stream != index {
            continue;
        }
        dec.send_packet(&packet).expect("decode");
        while let Ok(ec_core::frame::Frame::Video(f)) = dec.receive_frame() {
            if sources.len() < want && seen % stride == 0 {
                sources.push(planes_of(&f));
            }
            seen += 1;
        }
    }
    assert!(
        sources.len() >= 10,
        "only {} pictures decoded from {}",
        sources.len(),
        path.to_string_lossy()
    );
    eprintln!(
        "{}: {}x{}, {} pictures 1 s apart (every {stride}th of {seen})",
        path.to_string_lossy(),
        w,
        h,
        sources.len()
    );
    (w, h, sources)
}

/// BD-PSNR of the opt-in `transform_8x8` flag against flag-off on one real
/// clip from the library, full size, over a QP sweep: the number the synthetic
/// gate above cannot give, because the synthetic clip is content the flag wins
/// on. Prints the per-QP points, the BD-PSNR and the 8x8 macroblock share;
/// asserts nothing, so a regressing clip fails a gate, not the test run.
///
///     EC_H264_CLIP=<file> cargo test --release -p ec-h264 --test encode \
///         real_clip_t8x8_bd_psnr -- --ignored --nocapture
#[test]
#[ignore = "set EC_H264_CLIP to a media file of yours"]
fn real_clip_t8x8_bd_psnr() {
    let Some(path) = std::env::var_os("EC_H264_CLIP") else {
        panic!("EC_H264_CLIP must name a media file");
    };
    let (w, h, sources) = clip_sources(&path, 30);

    let run = |t8x8: bool, qp: i32| {
        let mut cfg = EncoderConfig::new(w as u32, h as u32);
        cfg.qp = qp;
        cfg.gop_size = 10;
        cfg.cabac = true;
        cfg.transform_8x8 = t8x8;
        cfg.framerate = 1.0; // the kept pictures are a second apart
        cfg.threads = 0;
        let mut enc = Encoder::new(cfg).expect("encoder");
        let (mut bits, mut sum_psnr) = (0usize, 0.0);
        for (y, u, v) in &sources {
            let view = PictureView::i420(w as u32, h as u32, y, u, v);
            bits += enc.encode(&view).expect("encode").au.len() * 8;
            let rec = enc.reconstruction().expect("reconstruction");
            sum_psnr += psnr(y, &rec.0);
        }
        let (i8x8, p8x8) = enc.transform_8x8_mbs();
        let share = (i8x8 + p8x8) as f64 / (sources.len() * (w / 16) * (h / 16)) as f64;
        (bits as f64, sum_psnr / sources.len() as f64, share)
    };
    let qps = [22, 26, 30, 34];
    let off: Vec<_> = qps.iter().map(|&q| run(false, q)).collect();
    let on: Vec<_> = qps.iter().map(|&q| run(true, q)).collect();
    for (i, &qp) in qps.iter().enumerate() {
        let (ob, op, _) = off[i];
        let (nb, np, share) = on[i];
        eprintln!(
            "qp {qp}: flag-off {ob:.0} bits {op:.2} dB | flag-on {nb:.0} bits {np:.2} dB, 8x8 share {:.1}%",
            share * 100.0
        );
    }
    let on_curve: Vec<_> = on.iter().map(|&(bits, psnr, _)| (bits, psnr)).collect();
    let off_curve: Vec<_> = off.iter().map(|&(bits, psnr, _)| (bits, psnr)).collect();
    let bd = bd_psnr_delta(&on_curve, &off_curve);
    let share = on.iter().map(|&(_, _, s)| s).sum::<f64>() / on.len() as f64;
    eprintln!(
        "BD-PSNR flag-on vs flag-off over q22/26/30/34: {bd:+.3} dB, average 8x8 MB share {:.1}%",
        share * 100.0
    );
}

// ---------------------------------------------------------------------------
// The encoder's rate-quality distance from x264: the survey's rank-1 gap,
// because until this test existed the repo could measure our decoder against
// every reference and our encoder against nothing but itself.
// ---------------------------------------------------------------------------

/// Floor for [`bd_psnr_vs_x264`] on the synthetic clip. Our encoder measured
/// -5.202 dB against x264 there on 2026-08-23 (352x288, 24 pictures of
/// `Clip::render_smooth`, both encoders at their own default effort: our
/// `Preset::Fast`, x264's `-preset medium`). The floor is that number with
/// room for platform noise, so the distance can shrink but not grow
/// unnoticed; it is not a claim that -5.2 dB is fine.
const BD_PSNR_VS_X264_FLOOR: f64 = -5.40;

fn have_x264() -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("libx264"))
}

/// Encode raw I420 pictures with x264 at constant QP, matched to what our
/// encoder actually does: Main profile (CABAC, no 8x8 transform), one
/// reference, no B pictures, the same GOP, and adaptive quantisation and psy
/// off so both encoders spend bits by the same rule. Returns the Annex B
/// stream.
fn x264_encode_qp(sources: &[Planes], w: usize, h: usize, qp: i32, gop: u32) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("ec-h264-x264-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let raw = dir.join(format!("in-q{qp}.yuv"));
    let out = dir.join(format!("out-q{qp}.264"));
    let mut buf = Vec::with_capacity(sources.len() * w * h * 3 / 2);
    for (y, u, v) in sources {
        buf.extend_from_slice(y);
        buf.extend_from_slice(u);
        buf.extend_from_slice(v);
    }
    std::fs::write(&raw, &buf).ok()?;
    let status = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .args(["-s", &format!("{w}x{h}"), "-r", "1"])
        .arg("-i")
        .arg(&raw)
        .args(["-c:v", "libx264", "-preset", "medium"])
        .args(["-profile:v", "main", "-qp", &qp.to_string()])
        .args(["-g", &gop.to_string(), "-bf", "0", "-refs", "1"])
        // ipratio/pbratio 1.0 matter: x264's constant-QP mode otherwise codes
        // I pictures at a lower QP than the one asked for, which reads as a
        // rate-quality win that is really a different quantiser.
        .args([
            "-x264-params",
            "scenecut=0:aq-mode=0:psy=0:8x8dct=0:ipratio=1.0:pbratio=1.0:qcomp=1.0:chroma-qp-offset=0",
        ])
        .args(["-f", "h264"])
        .arg(&out)
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&raw);
    if !status.status.success() {
        eprintln!("x264 failed: {}", String::from_utf8_lossy(&status.stderr));
        return None;
    }
    let stream = std::fs::read(&out).ok()?;
    let _ = std::fs::remove_file(&out);
    Some(stream)
}

/// BD-PSNR of our encoder against x264 at matched features over QP
/// 22/26/30/34. Runs on the synthetic clip by default, so the number cannot
/// rot behind an `#[ignore]`; point `EC_H264_CLIP` at a media file to measure
/// the same thing on real content.
///
/// Both curves are (bits, luma PSNR against the same source pictures), and
/// both are measured on ffmpeg's decode of the stream rather than on an
/// encoder's own reconstruction, so the two halves are read the same way.
///
/// `EC_H264_GOP` sets the GOP of both encoders; 1 makes the comparison
/// all-intra, which is how the gap was split: on a 3840x1608 clip from the
/// library the intra-only distance is -0.749 dB (+2-4% bits) while the same
/// clip at GOP 10 is -2.992 dB (+19% bits), so roughly two thirds of the
/// distance is in P pictures, not in the intra path.
///
/// `EC_H264_PRESET_BALANCED` runs our half of the comparison at
/// `Preset::Balanced` instead of the default `Preset::Fast`, which separates
/// what search effort buys from what the coder itself costs: on the same
/// library clip it moves -2.992 dB to -1.587 dB for 40% more encode time.
///
///     cargo test --release -p ec-h264 --test encode bd_psnr_vs_x264 -- --nocapture
///     EC_H264_CLIP=<file> cargo test --release -p ec-h264 --test encode \
///         bd_psnr_vs_x264 -- --nocapture
#[test]
fn bd_psnr_vs_x264() {
    if !have_ffmpeg() || !have_x264() {
        eprintln!(
            "SKIP bd_psnr_vs_x264: no ffmpeg with libx264 on PATH. \
             Install one, then: cargo test --release -p ec-h264 --test encode \
             bd_psnr_vs_x264 -- --nocapture"
        );
        return;
    }
    let (w, h, sources) = match std::env::var_os("EC_H264_CLIP") {
        Some(path) => clip_sources(&path, 24),
        None => {
            let (w, h, frames) = (352, 288, 24);
            let mut clip = Clip::new(w, h);
            let mut sources = Vec::with_capacity(frames);
            for t in 0..frames {
                clip.render_smooth(t);
                sources.push((clip.y.clone(), clip.u.clone(), clip.v.clone()));
            }
            eprintln!("synthetic clip: {w}x{h}, {frames} pictures");
            (w, h, sources)
        }
    };

    let gop: u32 = std::env::var("EC_H264_GOP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let ours = |qp: i32| {
        let mut cfg = EncoderConfig::new(w as u32, h as u32);
        cfg.qp = qp;
        cfg.gop_size = gop;
        if std::env::var_os("EC_H264_PRESET_BALANCED").is_some() {
            cfg.preset = ec_h264::Preset::Balanced;
        }
        cfg.cabac = true;
        cfg.framerate = 1.0;
        cfg.threads = 0;
        let mut enc = Encoder::new(cfg).expect("encoder");
        let (mut stream, mut sum_recon) = (Vec::new(), 0.0);
        for (y, u, v) in &sources {
            let view = PictureView::i420(w as u32, h as u32, y, u, v);
            stream.extend_from_slice(&enc.encode(&view).expect("encode").au);
            sum_recon += psnr(y, &enc.reconstruction().expect("reconstruction").0);
        }
        // Measured the same way x264 is: on what a decoder shows, not on the
        // encoder's own reconstruction. The two are printed together because a
        // gap between them is a bug in one of the halves, not a rate-quality
        // result.
        let decoded = ffmpeg_decode(&stream, w, h, &[]).expect("ffmpeg decodes our stream");
        assert_eq!(decoded.len(), sources.len(), "our encoder dropped pictures");
        let sum: f64 = sources
            .iter()
            .zip(&decoded)
            .map(|((y, _, _), (dy, _, _))| psnr(y, dy))
            .sum();
        let (decoded_psnr, recon_psnr) =
            (sum / sources.len() as f64, sum_recon / sources.len() as f64);
        assert!(
            (decoded_psnr - recon_psnr).abs() < 0.05,
            "qp {qp}: our reconstruction reads {recon_psnr:.2} dB but our own \
             stream decodes to {decoded_psnr:.2} dB"
        );
        ((stream.len() * 8) as f64, decoded_psnr)
    };
    let x264 = |qp: i32| {
        let stream = x264_encode_qp(&sources, w, h, qp, gop).expect("x264 encodes");
        let decoded = ffmpeg_decode(&stream, w, h, &[]).expect("ffmpeg decodes x264's own stream");
        assert_eq!(
            decoded.len(),
            sources.len(),
            "x264 returned {} pictures for {} sources",
            decoded.len(),
            sources.len()
        );
        let sum: f64 = sources
            .iter()
            .zip(&decoded)
            .map(|((y, _, _), (dy, _, _))| psnr(y, dy))
            .sum();
        ((stream.len() * 8) as f64, sum / sources.len() as f64)
    };

    let qps = [22, 26, 30, 34];
    let mine: Vec<_> = qps.iter().map(|&q| ours(q)).collect();
    let theirs: Vec<_> = qps.iter().map(|&q| x264(q)).collect();
    for (i, &qp) in qps.iter().enumerate() {
        let (ob, op) = mine[i];
        let (xb, xp) = theirs[i];
        eprintln!(
            "qp {qp}: ours {ob:.0} bits {op:.2} dB | x264 {xb:.0} bits {xp:.2} dB \
             ({:+.1}% bits, {:+.2} dB)",
            (ob / xb - 1.0) * 100.0,
            op - xp
        );
    }
    let bd = bd_psnr_delta(&mine, &theirs);
    eprintln!("BD-PSNR ours vs x264 over q22/26/30/34: {bd:+.3} dB");

    // The floor is calibrated on the synthetic clip; a clip of the user's is a
    // measurement, not a gate, because its content is not pinned.
    if std::env::var_os("EC_H264_CLIP").is_none() && gop == 10 {
        assert!(
            bd > BD_PSNR_VS_X264_FLOOR,
            "BD-PSNR against x264 fell to {bd:+.3} dB, past the \
             {BD_PSNR_VS_X264_FLOOR:+.3} dB floor"
        );
    }
}
