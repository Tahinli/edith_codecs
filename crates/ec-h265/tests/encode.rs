//! Encoder rate-quality gate: BD-PSNR of this encoder against x265 at matched
//! features over a QP sweep, modelled on ec-h264's `bd_psnr_vs_x264`.
//!
//! Both encoders code the same synthetic 352x288 24-picture clip, both are read
//! the same way — on ffmpeg's decode of the stream, not on either encoder's
//! own reconstruction — and x265's psychovisual and adaptive tools are off so
//! the comparison is at matched features, not at matched marketing.

use ec_core::frame::VideoFrame;
use ec_h265::encoder::{Encoder, EncoderConfig, RateControl};

/// One decoded picture: Y, Cb and Cr planes, cropped and tightly packed.
type Planes = (Vec<u8>, Vec<u8>, Vec<u8>);

/// Floor for [`bd_psnr_vs_x265`] on the synthetic clip. Calibrated on the
/// measured value with 0.15 dB of room for platform noise, so the distance can
/// shrink but not grow unnoticed; it is not a claim that the number is fine.
///
/// Measured -38.258 dB luma / -31.656 dB YUV on the first run (352x288, 24
/// pictures of `Clip::render_smooth`, QP 22/26/30/34, both sides decoded
/// through ffmpeg, union-range BD-PSNR). The gap is wide because this encoder
/// spends 40-70% fewer bits than x265 at every QP, so its curve sits below
/// x265's and the cubic fit extrapolates; the union-range integral is the
/// handoff's chosen convention for that case.
const BD_PSNR_VS_X265_FLOOR: f64 = -38.41;

/// A synthetic source with real structure: smooth gradients, a band of
/// sinusoidal texture and a translating box, so prediction matters — the half
/// a rate-quality comparison is about. Ported verbatim from ec-h264's `Clip`
/// (minus the `view()` method this encoder's `encode_idr_planes` does not
/// need).
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

    /// Frame `t` without a per-sample noise field: smooth gradients, a band of
    /// sinusoidal texture and a box translating by whole and sub-sample
    /// amounts. Content where prediction matters, not where residual is
    /// incompressible.
    fn render_smooth(&mut self, t: usize) {
        let (w, h) = (self.w, self.h);
        let ft = t as f64;
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f64, y as f64);
                let ramp = (fx + fy * 0.5 - ft * 1.5) * 0.35;
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
}

/// Crop and tightly pack the Y, Cb and Cr planes of a reconstruction.
fn planes_of(frame: &VideoFrame) -> Planes {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut out = Vec::new();
    for (i, (pw, ph)) in [(w, h), (w / 2, h / 2), (w / 2, h / 2)]
        .into_iter()
        .enumerate()
    {
        let plane = &frame.planes[i];
        let mut buf = Vec::with_capacity(pw * ph);
        for row in 0..ph {
            buf.extend_from_slice(&plane.data[row * plane.stride..row * plane.stride + pw]);
        }
        out.push(buf);
    }
    let v = out.pop().unwrap();
    let u = out.pop().unwrap();
    let y = out.pop().unwrap();
    (y, u, v)
}

/// Mean squared error between two equal-length planes.
fn plane_mse(a: &[u8], b: &[u8]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = f64::from(x) - f64::from(y);
            d * d
        })
        .sum::<f64>()
        / a.len() as f64
}

/// PSNR over all three planes, pooled by sample count the way x265 reports it:
/// for 4:2:0 that weights luma 6 and each chroma plane 1. A luma-only number
/// rewards an encoder for throwing chroma away, so any decision that can move
/// chroma bits is read here instead.
fn psnr_yuv(a: &Planes, b: &Planes) -> f64 {
    let (n_y, n_c) = (a.0.len() as f64, a.1.len() as f64);
    let sse =
        plane_mse(&a.0, &b.0) * n_y + plane_mse(&a.1, &b.1) * n_c + plane_mse(&a.2, &b.2) * n_c;
    let mse = sse / (n_y + 2.0 * n_c);
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
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

#[allow(clippy::needless_range_loop)]
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

/// BD-PSNR: average PSNR difference between two rate-quality curves over a
/// shared bitrate range. Modelled on ec-h264's `bd_psnr_delta`, with one
/// departure: the integration range is the *union* of the two curves' bitrate
/// extents, not their intersection.
///
/// The h264 gate's encoders land in the same bitrate band at the same QP, so
/// the intersection is non-empty and the standard Bjøntegaard cubic integral
/// over it is well defined. Our h265 encoder is leaner than x265 on the
/// synthetic clip — it spends fewer bits at every QP — so its entire curve
/// sits below x265's and the intersection is empty. Clipping to the
/// intersection would leave no range to integrate over; expanding to the
/// union lets the cubic fit extrapolate the curve that doesn't reach a given
/// bitrate, which is the same polynomial either way. The extrapolation is
/// reported alongside the number so a wild cubic can't pass silently.
fn bd_psnr_delta(candidate: &[(f64, f64)], anchor: &[(f64, f64)]) -> f64 {
    let xs_candidate = std::array::from_fn(|i| candidate[i].0.ln());
    let ys_candidate = std::array::from_fn(|i| candidate[i].1);
    let xs_anchor = std::array::from_fn(|i| anchor[i].0.ln());
    let ys_anchor = std::array::from_fn(|i| anchor[i].1);
    // Union, not intersection: see the doc comment above.
    let lo = xs_candidate
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min)
        .min(xs_anchor.iter().copied().fold(f64::INFINITY, f64::min));
    let hi = xs_candidate
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max)
        .max(xs_anchor.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    let c_candidate = cubic_fit(xs_candidate, ys_candidate);
    let c_anchor = cubic_fit(xs_anchor, ys_anchor);
    let (cand_lo, cand_hi) = (
        xs_candidate.iter().copied().fold(f64::INFINITY, f64::min),
        xs_candidate
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max),
    );
    let (anc_lo, anc_hi) = (
        xs_anchor.iter().copied().fold(f64::INFINITY, f64::min),
        xs_anchor.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    );
    let cand_extrap = ((lo - cand_lo).max(0.0) + (hi - cand_hi).max(0.0)) / (cand_hi - cand_lo);
    let anc_extrap = ((lo - anc_lo).max(0.0) + (hi - anc_hi).max(0.0)) / (anc_hi - anc_lo);
    eprintln!(
        "  cubic extrapolation: candidate {cand_extrap:.0}% anchor {anc_extrap:.0}% of measured range"
    );
    (cubic_integral(c_candidate, hi)
        - cubic_integral(c_candidate, lo)
        - cubic_integral(c_anchor, hi)
        + cubic_integral(c_anchor, lo))
        / (hi - lo)
}

fn have_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn have_x265() -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("libx265"))
}

/// Decode an Annex-B stream with ffmpeg, returning one I420 triple per picture.
fn ffmpeg_decode(stream: &[u8], w: usize, h: usize) -> Option<Vec<Planes>> {
    let dir = std::env::temp_dir().join(format!("ec-h265-enc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("s{}.265", stream.len()));
    std::fs::write(&path, stream).ok()?;
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error"])
        .arg("-i")
        .arg(&path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"])
        .output()
        .ok()?;
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

/// Encode raw I420 pictures with x265 at constant QP, matched to what this
/// encoder actually does: intra-only (keyint 1), no B pictures, no scene cut
/// or lookahead, in-loop filters (SAO, deblock) off, adaptive quantisation off,
/// psychovisual RD and RDOQ off, RDOQ itself off, the shallowest intra TU
/// depth x265 allows (1, the closest it has to our
/// `max_transform_hierarchy_depth_intra=0`), and ipratio/pbratio/qcomp 1.0 so
/// constant-QP codes I pictures at the QP asked for. Returns the Annex-B
/// stream.
fn x265_encode_qp(sources: &[Planes], w: usize, h: usize, qp: i32) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("ec-h265-x265-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let raw = dir.join(format!("in-q{qp}.yuv"));
    let out = dir.join(format!("out-q{qp}.265"));
    let mut buf = Vec::with_capacity(sources.len() * w * h * 3 / 2);
    for (y, u, v) in sources {
        buf.extend_from_slice(y);
        buf.extend_from_slice(u);
        buf.extend_from_slice(v);
    }
    std::fs::write(&raw, &buf).ok()?;
    // The param list is the h264 gate's x264 list translated to x265's option
    // names: psychovisual (psy-rd, psy-rdoq) and adaptive (aq-mode) tools off,
    // plus the in-loop filters x265 adds and x264 does not have (no-sao,
    // no-deblock), plus RDOQ off (rdoq-level=0) so the quantiser spends bits
    // the same way ours does. EC_H265_X265_PARAMS appends to attribute a single
    // feature the same way EC_H264_X264_PARAMS does.
    let params = format!(
        "keyint=1:bframes=0:scenecut=0:rc-lookahead=0:no-sao:no-deblock:aq-mode=0:\
         psy-rd=0:psy-rdoq=0:rdoq-level=0:tu-intra-depth=1:ipratio=1.0:pbratio=1.0:\
         qcomp=1.0:chroma-qp-offset=0{}",
        match std::env::var("EC_H265_X265_PARAMS") {
            Ok(extra) if !extra.trim().trim_matches(':').is_empty() =>
                format!(":{}", extra.trim().trim_matches(':')),
            _ => String::new(),
        }
    );
    let status = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .args(["-s", &format!("{w}x{h}"), "-r", "1"])
        .arg("-i")
        .arg(&raw)
        .args(["-c:v", "libx265", "-preset", "medium"])
        .args(["-x265-params", &params])
        .args(["-qp", &qp.to_string()])
        .args(["-f", "hevc"])
        .arg(&out)
        .status()
        .ok()?;
    let _ = std::fs::remove_file(&raw);
    if !status.success() {
        return None;
    }
    std::fs::read(&out).ok()
}

/// BD-PSNR of this encoder against x265 at matched features over QP
/// 22/26/30/34. Runs on the synthetic clip by default, so the number cannot
/// rot behind an `#[ignore]`; point `EC_H265_CLIP` at a media file to measure
/// the same thing on real content.
///
/// Both curves are (bits, luma PSNR against the same source pictures), and
/// both are measured on ffmpeg's decode of the stream rather than on an
/// encoder's own reconstruction, so the two halves are read the same way.
/// BD-PSNR-YUV pools all three planes by sample count (luma weighted 6, each
/// chroma 1 for 4:2:0), the way x265 reports it.
///
///     cargo test --release -p ec-h265 --test encode bd_psnr_vs_x265 -- --nocapture
///     EC_H265_CLIP=<file> cargo test --release -p ec-h265 --test encode \
///         bd_psnr_vs_x265 -- --nocapture
#[test]
fn bd_psnr_vs_x265() {
    if !have_ffmpeg() || !have_x265() {
        eprintln!(
            "SKIP bd_psnr_vs_x265: no ffmpeg with libx265 on PATH. \
             Install one, then: cargo test --release -p ec-h265 --test encode \
             bd_psnr_vs_x265 -- --nocapture"
        );
        return;
    }
    let (w, h, sources) = match std::env::var_os("EC_H265_CLIP") {
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

    let ours = |qp: i32| {
        let mut cfg = EncoderConfig::new(w as u32, h as u32);
        cfg.rate_control = RateControl::ConstantQp(qp);
        cfg.keep_recon = true;
        cfg.threads = 0;
        let enc = Encoder::new(cfg).expect("encoder");
        let (mut stream, mut sum_recon) = (Vec::new(), 0.0);
        for (y, u, v) in &sources {
            let coded = enc
                .encode_idr_planes(y, w, u, w / 2, v, w / 2)
                .expect("encode");
            stream.extend_from_slice(&coded.au);
            let recon = coded.recon.as_ref().expect("reconstruction");
            let (ry, _ru, _rv) = planes_of(recon);
            sum_recon += psnr(y, &ry);
        }
        // Measured the same way x265 is: on what a decoder shows, not on the
        // encoder's own reconstruction. The two are printed together because a
        // gap between them is a bug in one of the halves, not a rate-quality
        // result.
        let decoded = ffmpeg_decode(&stream, w, h).expect("ffmpeg decodes our stream");
        assert_eq!(decoded.len(), sources.len(), "our encoder dropped pictures");
        let n = sources.len() as f64;
        let sum: f64 = sources
            .iter()
            .zip(&decoded)
            .map(|((y, _, _), (dy, _, _))| psnr(y, dy))
            .sum();
        let sum_yuv: f64 = sources
            .iter()
            .zip(&decoded)
            .map(|(s, d)| psnr_yuv(s, d))
            .sum();
        let (sum_u, sum_v) = (
            sources
                .iter()
                .zip(&decoded)
                .map(|(s, d)| psnr(&s.1, &d.1))
                .sum::<f64>(),
            sources
                .iter()
                .zip(&decoded)
                .map(|(s, d)| psnr(&s.2, &d.2))
                .sum::<f64>(),
        );
        eprintln!(
            "    ours qp {qp}: u {:.2} dB v {:.2} dB",
            sum_u / n,
            sum_v / n
        );
        let (decoded_psnr, recon_psnr) = (sum / n, sum_recon / n);
        assert!(
            (decoded_psnr - recon_psnr).abs() < 0.05,
            "qp {qp}: our reconstruction reads {recon_psnr:.2} dB but our own \
             stream decodes to {decoded_psnr:.2} dB"
        );
        ((stream.len() * 8) as f64, decoded_psnr, sum_yuv / n)
    };
    let x265 = |qp: i32| {
        let stream = x265_encode_qp(&sources, w, h, qp).expect("x265 encodes");
        let decoded = ffmpeg_decode(&stream, w, h).expect("ffmpeg decodes x265's own stream");
        assert_eq!(
            decoded.len(),
            sources.len(),
            "x265 returned {} pictures for {} sources",
            decoded.len(),
            sources.len()
        );
        let n = sources.len() as f64;
        let sum: f64 = sources
            .iter()
            .zip(&decoded)
            .map(|((y, _, _), (dy, _, _))| psnr(y, dy))
            .sum();
        let sum_yuv: f64 = sources
            .iter()
            .zip(&decoded)
            .map(|(s, d)| psnr_yuv(s, d))
            .sum();
        eprintln!(
            "    x265 qp {qp}: u {:.2} dB v {:.2} dB",
            sources
                .iter()
                .zip(&decoded)
                .map(|(s, d)| psnr(&s.1, &d.1))
                .sum::<f64>()
                / n,
            sources
                .iter()
                .zip(&decoded)
                .map(|(s, d)| psnr(&s.2, &d.2))
                .sum::<f64>()
                / n
        );
        ((stream.len() * 8) as f64, sum / n, sum_yuv / n)
    };

    let qps = [22, 26, 30, 34];
    let mine: Vec<_> = qps.iter().map(|&q| ours(q)).collect();
    let theirs: Vec<_> = qps.iter().map(|&q| x265(q)).collect();
    for (i, &qp) in qps.iter().enumerate() {
        let (ob, op, oy) = mine[i];
        let (xb, xp, xy) = theirs[i];
        eprintln!(
            "qp {qp}: ours {ob:.0} bits {op:.2} dB ({oy:.2} yuv) | x265 {xb:.0} bits {xp:.2} dB \
             ({xy:.2} yuv) ({:+.1}% bits, {:+.2} dB, {:+.2} yuv)",
            (ob / xb - 1.0) * 100.0,
            op - xp,
            oy - xy
        );
    }
    let curve = |v: &[(f64, f64, f64)], luma: bool| -> Vec<(f64, f64)> {
        v.iter()
            .map(|&(b, p, y)| (b, if luma { p } else { y }))
            .collect()
    };
    let bd = bd_psnr_delta(&curve(&mine, true), &curve(&theirs, true));
    let bd_yuv = bd_psnr_delta(&curve(&mine, false), &curve(&theirs, false));
    eprintln!("BD-PSNR ours vs x265 over q22/26/30/34: {bd:+.3} dB");
    eprintln!("BD-PSNR-YUV ours vs x265 over q22/26/30/34: {bd_yuv:+.3} dB");

    // The floor is calibrated on the synthetic clip; a clip of the user's is a
    // measurement, not a gate, because its content is not pinned.
    if std::env::var_os("EC_H265_CLIP").is_none() {
        assert!(
            bd > BD_PSNR_VS_X265_FLOOR,
            "BD-PSNR against x265 fell to {bd:+.3} dB, past the \
             {BD_PSNR_VS_X265_FLOOR:+.3} dB floor"
        );
    }
}

/// Decode `want` pictures from a media file as tightly packed I420 triples.
/// `EC_H265_CLIP_SKIP` passes over that many pictures before sampling (film
/// heads are titles and fades).
fn clip_sources(path: &std::ffi::OsStr, want: usize) -> (usize, usize, Vec<Planes>) {
    let dims = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe");
    assert!(
        dims.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&dims.stderr)
    );
    let csv = String::from_utf8_lossy(&dims.stdout);
    let (w, h) = csv
        .trim()
        .split_once(',')
        .and_then(|(w, h)| Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?)))
        .expect("ffprobe returned dimensions");
    let raw = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"])
        .output()
        .expect("ffmpeg raw extract");
    assert!(
        raw.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&raw.stderr)
    );
    let frame = w * h * 3 / 2;
    assert!(
        raw.stdout.len().is_multiple_of(frame) && raw.stdout.len() / frame >= want,
        "clip {} yielded {} bytes, need {want}x{frame}",
        path.to_string_lossy(),
        raw.stdout.len()
    );
    let skip: usize = std::env::var("EC_H265_CLIP_SKIP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let sources: Vec<Planes> = raw
        .stdout
        .chunks(frame)
        .skip(skip)
        .take(want)
        .map(|f| {
            let (y, rest) = f.split_at(w * h);
            let (u, v) = rest.split_at(w / 2 * h / 2);
            (y.to_vec(), u.to_vec(), v.to_vec())
        })
        .collect();
    eprintln!(
        "{}: {}x{}, {} pictures (skipped {skip})",
        path.to_string_lossy(),
        w,
        h,
        sources.len()
    );
    (w, h, sources)
}
