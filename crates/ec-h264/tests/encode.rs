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

/// The encoder's reconstruction and a decode of its bitstream agree sample for
/// sample, over a QP sweep, both presets, single and multi threaded.
#[test]
fn reconstruction_matches_the_decoder() {
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
                    assert!(
                        intra > 0 && inter > 0,
                        "{w}x{h} qp {qp} cabac {cabac}: 8x8 MBs intra {intra} inter {inter}"
                    );
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
) -> (Vec<u8>, Vec<Planes>) {
    let mut cfg = EncoderConfig::new(w as u32, h as u32);
    cfg.gop_size = 10;
    cfg.threads = 3;
    configure(&mut cfg);
    let t8x8 = cfg.transform_8x8;
    let mut enc = Encoder::new(cfg).expect("encoder");
    let mut clip = Clip::new(w, h);
    let mut stream = Vec::new();
    let mut recons = Vec::new();
    for t in 0..frames {
        clip.render(t);
        stream.extend_from_slice(&enc.encode(&clip.view()).expect("encode").au);
        recons.push(enc.reconstruction().expect("reconstruction"));
    }
    if t8x8 {
        let (intra, inter) = enc.transform_8x8_mbs();
        assert!(intra > 0 && inter > 0, "8x8 MBs intra {intra} inter {inter}");
    }
    (stream, recons)
}

/// Flat gradients and glyph-like rectangles: the content the 8x8 transform
/// exists for. A 10-frame clip with the flag on must beat the flag off by at
/// least 0.3 dB at equal bits, or by 5% fewer bits at equal PSNR.
#[test]
fn eight_by_eight_gains_on_flat_and_text() {
    let (w, h) = (320, 240);
    let render = |t: usize, clip: &mut Clip| {
        for y in 0..h {
            for x in 0..w {
                let grad = (x * 3 / 4 + y / 2 + t) as u8 / 2 + 40;
                // Glyph rows: 12x16 cells, a 2-sample stem and a bar.
                let (cx, cy) = ((x + t) % 12, y % 16);
                let glyph = y > h / 3 && cy >= 3 && cy < 13 && (cx < 2 || (cy == 8 && cx < 8));
                clip.y[y * w + x] = if glyph { 20 } else { grad };
            }
        }
        clip.u.fill(128);
        clip.v.fill(128);
    };
    let run = |t8x8: bool, qp: i32| {
        let mut cfg = EncoderConfig::new(w as u32, h as u32);
        cfg.qp = qp;
        cfg.gop_size = 10;
        cfg.transform_8x8 = t8x8;
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
            share = (i + p) as f64 / (10.0 * (w / 16 * h / 16) as f64);
        }
        (bits as f64, sum_psnr / 10.0, share)
    };
    // Interpolate the off curve at the on point's bits / PSNR.
    let qp = 30;
    let (bits_on, psnr_on, share) = run(true, qp);
    let off: Vec<_> = [qp - 2, qp, qp + 2].iter().map(|&q| run(false, q)).collect();
    let (bits_off, psnr_off, _) = off[1];
    // Local slopes of the off curve, from the neighbouring QPs.
    let dpsnr_dbits = (off[0].1 - off[2].1) / (off[0].0 - off[2].0);
    let psnr_off_at_on_bits = psnr_off + (bits_on - bits_off) * dpsnr_dbits;
    let bits_off_at_on_psnr = bits_off + (psnr_on - psnr_off) / dpsnr_dbits;
    let gain_db = psnr_on - psnr_off_at_on_bits;
    let saving = 1.0 - bits_on / bits_off_at_on_psnr;
    eprintln!(
        "8x8 qp {qp}: on {bits_on:.0} bits {psnr_on:.2} dB, off {bits_off:.0} bits {psnr_off:.2} dB; \
         gain at equal bits {gain_db:+.2} dB, saving at equal PSNR {:.1}%, 8x8 MB share {:.1}%",
        saving * 100.0,
        share * 100.0
    );
    assert!(share > 0.0, "no 8x8 macroblocks were coded");
    assert!(
        gain_db >= 0.3 || saving >= 0.05,
        "8x8 gain {gain_db:+.2} dB / {:.1}% is below the bar",
        saving * 100.0
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
    for (w, h) in [(176, 144), (322, 242)] {
        for qp in [10, 20, 28, 37, 47] {
            let cavlc = qp % 2 == 0; // both entropy coders across the sweep
            let t8x8 = qp % 3 == 1; // High-profile PPS with the per-MB flag
            let (stream, recons) = encode_clip(w, h, 6, |cfg| {
                cfg.qp = qp;
                cfg.cabac = !cavlc;
                cfg.transform_8x8 = t8x8;
            });
            let decoded = ffmpeg_decode(&stream, w, h, &[]).expect("ffmpeg decodes our stream");
            assert_eq!(decoded.len(), recons.len(), "{w}x{h} qp {qp}: frame count");
            for (i, (dec, rec)) in decoded.iter().zip(&recons).enumerate() {
                assert_eq!(dec.0, rec.0, "{w}x{h} qp {qp}: luma of frame {i}");
                assert_eq!(dec.1, rec.1, "{w}x{h} qp {qp}: Cb of frame {i}");
                assert_eq!(dec.2, rec.2, "{w}x{h} qp {qp}: Cr of frame {i}");
            }
        }
    }
}

/// The same stream under constant bitrate, which is the mode edith exports in.
#[test]
fn ffmpeg_decodes_a_cbr_stream_bit_exactly() {
    if !have_ffmpeg() {
        eprintln!("SKIP ffmpeg_decodes_a_cbr_stream_bit_exactly: no ffmpeg on PATH");
        return;
    }
    let (w, h) = (352, 288);
    let (stream, recons) = encode_clip(w, h, 12, |cfg| {
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
    let (stream, recons) = encode_clip(w, h, 6, |cfg| {
        cfg.qp = 26;
        cfg.transform_8x8 = true;
    });
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
