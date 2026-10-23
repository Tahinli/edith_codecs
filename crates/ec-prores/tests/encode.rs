//! The ProRes encoder: ffmpeg decodes what we write exactly as ec-prores does, frame sizes sit on
//! Apple's rates, quality matches or beats ffmpeg's `prores_ks`, the .mov probes right.
//! All media is synthetic; ffmpeg-dependent tests skip (or fail under `EC_REQUIRE_FIXTURES`)
//! when ffmpeg is missing.

mod common;
use std::io::Cursor;
use std::process::Command;

use common::*;
use ec_core::frame::{ColorInfo, PixelFormat, Plane, VideoFrame};
use ec_core::registry::{CodecId, Demuxer, Encoder, Muxer, StreamInfo};
use ec_core::timebase::TimeBase;
use ec_core::Packet;
use ec_mp4::{Mp4Demuxer, Mp4Muxer};
use ec_prores::{nominal_frame_bytes, EncoderConfig, Profile, ProResEncoder};

const ALL: [Profile; 6] =
    [Profile::Proxy, Profile::Lt, Profile::Standard, Profile::Hq, Profile::P4444, Profile::P4444Xq];

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// Synthetic picture: smooth gradients and rings plus `noise` (peak, 10-bit units), as planar
/// little-endian 16-bit samples in `bits` (10 or 12) with planes Y, U, V.
fn synth(w: usize, h: usize, is_444: bool, bits: u32, noise: i32, seed: u64) -> Vec<Vec<u16>> {
    let mut s = seed | 1;
    let cw = if is_444 { w } else { w.div_ceil(2) };
    let scale = 1i32 << (bits - 10);
    let mut planes = Vec::new();
    for p in 0..3 {
        let (pw, ph) = if p == 0 { (w, h) } else { (cw, h) };
        let mut v = Vec::with_capacity(pw * ph);
        for y in 0..ph {
            for x in 0..pw {
                let (fx, fy) = (x as f64 / pw as f64, y as f64 / ph as f64);
                let base = match p {
                    0 => 200.0 + 500.0 * fx + 150.0 * (fy * 9.0).sin() + 90.0 * (((fx - 0.5).powi(2) + (fy - 0.5).powi(2)) * 90.0).sin(),
                    1 => 512.0 + 150.0 * (fx * 4.0).cos() - 80.0 * fy,
                    _ => 512.0 - 120.0 * (fy * 5.0).sin() + 60.0 * fx,
                };
                let n = if noise > 0 { (xorshift(&mut s) % (2 * noise as u64 + 1)) as i32 - noise } else { 0 };
                v.push((((base as i32 + n).clamp(16, 1000)) * scale) as u16);
            }
        }
        planes.push(v);
    }
    planes
}

fn to_frame(planes: &[Vec<u16>], w: usize, h: usize, fmt: PixelFormat) -> VideoFrame {
    let geo = fmt.plane_geometry(w as u32, h as u32);
    let ps = planes
        .iter()
        .zip(&geo)
        .map(|(p, g)| {
            let bytes: Vec<u8> = if fmt.bytes_per_component() == 2 {
                p.iter().flat_map(|v| v.to_le_bytes()).collect()
            } else {
                p.iter().map(|&v| v as u8).collect()
            };
            Plane::new(bytes, g.0)
        })
        .collect();
    VideoFrame::try_new(fmt, w as u32, h as u32, ps).unwrap()
}

fn fmt_for(p: Profile) -> PixelFormat {
    if p.is_444() { PixelFormat::I412 } else { PixelFormat::I210 }
}

fn encoder(p: Profile, w: usize, h: usize) -> ProResEncoder {
    let mut cfg = EncoderConfig::new(p, w as u32, h as u32);
    cfg.color = ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false };
    ProResEncoder::new(cfg).unwrap()
}

fn mux(enc: &ProResEncoder, frames: &[Vec<u8>], w: usize, h: usize) -> Vec<u8> {
    let tb = TimeBase::try_new(1001, 30000).unwrap();
    let mut m = Mp4Muxer::with_brands(Cursor::new(Vec::new()), *b"qt  ", &[*b"qt  "]).unwrap();
    let mut params = enc.codec_parameters().clone();
    if let ec_core::registry::MediaParameters::Video(v) = &mut params.media {
        v.width = w as u32;
        v.height = h as u32;
    }
    let s = m.add_stream(StreamInfo::new(0, tb, params)).unwrap();
    for (i, f) in frames.iter().enumerate() {
        let mut p = Packet::new(s, tb, f.clone());
        p.pts = Some(i as i64);
        p.dts = Some(i as i64);
        p.duration = Some(1);
        p.flags.keyframe = true;
        m.write_packet(&p).unwrap();
    }
    m.finish().unwrap();
    m.into_inner().into_inner()
}

/// Tight little-endian planes of a decoded frame.
fn tight(f: &ec_core::frame::VideoFrame) -> Vec<u8> {
    let (w, h) = (f.width as usize, f.height as usize);
    let (sx, sy) = f.format.chroma_shift();
    let mut v = Vec::new();
    for (i, pl) in f.planes.iter().enumerate() {
        let (pw, ph) = if i == 0 { (w, h) } else { (w.div_ceil(1 << sx), h.div_ceil(1 << sy)) };
        for y in 0..ph {
            v.extend_from_slice(&pl.data[y * pl.stride..y * pl.stride + pw * 2]);
        }
    }
    v
}

fn psnr(a: &[u8], b: &[u8], peak: f64) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut se = 0f64;
    for (x, y) in a.as_chunks::<2>().0.iter().zip(b.as_chunks::<2>().0) {
        let d = f64::from(u16::from_le_bytes([x[0], x[1]])) - f64::from(u16::from_le_bytes([y[0], y[1]]));
        se += d * d;
    }
    let mse = se / (a.len() / 2) as f64;
    if mse == 0.0 { 99.0 } else { 10.0 * (peak * peak / mse).log10() }
}

fn raw_of(planes: &[Vec<u16>]) -> Vec<u8> {
    planes.iter().flatten().flat_map(|v| v.to_le_bytes()).collect()
}

fn pix(p: Profile) -> &'static str {
    if p.is_444() { "yuv444p12le" } else { "yuv422p10le" }
}

#[test]
fn ffmpeg_and_ec_prores_decode_every_profile_identically() {
    if !ffmpeg_or_skip() {
        return;
    }
    let dir = scratch("enc-rt");
    for (i, p) in ALL.into_iter().enumerate() {
        // Odd size: padding macroblocks on both axes, 3 slice widths in a row.
        let (w, h) = (331, 75);
        let planes = synth(w, h, p.is_444(), if p.is_444() { 12 } else { 10 }, 10, 7 + i as u64);
        let enc = encoder(p, w, h);
        let f = enc.encode(&to_frame(&planes, w, h, fmt_for(p))).unwrap();
        let mov = mux(&enc, &[f.clone(), f], w, h);
        let path = dir.join(format!("{}.mov", String::from_utf8_lossy(&p.fourcc())));
        std::fs::write(&path, &mov).unwrap();
        let ours = decode_ours(&mov, 1).unwrap();
        assert_eq!(ours.len(), 2);
        let mut ours_raw = tight(&ours[0].video);
        ours_raw.extend(tight(&ours[1].video));
        let theirs = raw_ffmpeg(&format!("enc-rt-{i}"), &path, pix(p)).unwrap();
        assert_eq!(first_diff(&ours_raw, &theirs), None, "{p:?}: ffmpeg and ec-prores disagree");
        let peak = if p.is_444() { 4095.0 } else { 1023.0 };
        let q = psnr(&raw_of(&planes), &theirs[..theirs.len() / 2], peak);
        assert!(q > 34.0, "{p:?}: PSNR {q:.1} dB against the source");
        // Same bytes with a different thread count.
        let mut cfg = enc.config().clone();
        cfg.threads = 1;
        let one = ProResEncoder::new(cfg).unwrap().encode(&to_frame(&planes, w, h, fmt_for(p))).unwrap();
        assert_eq!(one, packets(&mov)[0], "{p:?}: output depends on the thread count");
    }
}

#[test]
fn eight_bit_input_is_widened() {
    let (w, h) = (96, 48);
    let planes = synth(w, h, false, 10, 8, 3);
    let p8: Vec<Vec<u16>> = planes.iter().map(|p| p.iter().map(|&v| v >> 2).collect()).collect();
    let enc = encoder(Profile::Hq, w, h);
    let f = enc.encode(&to_frame(&p8, w, h, PixelFormat::I422)).unwrap();
    let dec = decoder(1).decode(&f).unwrap();
    let want: Vec<u8> = p8.iter().flatten().flat_map(|&v| (u16::from(v as u8) * 4 + u16::from(v as u8 >> 6)).to_le_bytes()).collect();
    assert!(psnr(&tight(&dec), &want, 1023.0) > 45.0);
    // A 4:4:4 picture into a 4:2:2 profile is refused, not guessed at.
    let p444 = synth(w, h, true, 10, 0, 1);
    assert!(enc.encode(&to_frame(&p444, w, h, PixelFormat::I410)).is_err());
}

#[test]
fn frame_sizes_sit_on_apples_rates() {
    // Dense noise saturates the budget; HD: 8160 macroblocks, UHD: 32400.
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        for p in ALL {
            if w == 3840 && !matches!(p, Profile::Hq | Profile::Lt | Profile::P4444) {
                continue;
            }
            let planes = synth(w, h, p.is_444(), if p.is_444() { 12 } else { 10 }, 70, 11);
            let enc = encoder(p, w, h);
            let n = enc.encode(&to_frame(&planes, w, h, fmt_for(p))).unwrap().len() as f64;
            let nominal = nominal_frame_bytes(p, w.div_ceil(16) * h.div_ceil(16)) as f64;
            let ratio = n / nominal;
            eprintln!("{p:?} {w}x{h}: {n} bytes, nominal {nominal}, ratio {ratio:.3}");
            assert!((0.9..=1.1).contains(&ratio), "{p:?} {w}x{h}: ratio {ratio:.3}");
            if w == 1920 {
                // Apple's 1080p29.97 rates, Mbit/s.
                let mbps = match p {
                    Profile::Proxy => 45.0,
                    Profile::Lt => 102.0,
                    Profile::Standard => 147.0,
                    Profile::Hq => 220.0,
                    Profile::P4444 => 330.0,
                    Profile::P4444Xq => 500.0,
                };
                let apple = mbps * 1e6 / 29.97 / 8.0;
                assert!((0.9..=1.1).contains(&(n / apple)), "{p:?}: {:.3} of Apple's 1080p rate", n / apple);
            } else {
                // Apple's UHD 29.97 rates.
                let mbps = match p {
                    Profile::Lt => 328.0,
                    Profile::Hq => 707.0,
                    _ => 1060.0,
                };
                let apple = mbps * 1e6 / 29.97 / 8.0;
                assert!((0.9..=1.1).contains(&(n / apple)), "{p:?}: {:.3} of Apple's 2160p rate", n / apple);
            }
        }
    }
}

#[test]
fn encoder_trait_and_mov_probe() {
    if !ffmpeg_or_skip() {
        return;
    }
    let dir = scratch("enc-probe");
    let (w, h) = (192, 108);
    for p in ALL {
        let planes = synth(w, h, p.is_444(), if p.is_444() { 12 } else { 10 }, 10, 5);
        let mut enc = encoder(p, w, h);
        assert_eq!(enc.codec_parameters().codec, CodecId::ProRes);
        let mut frames = Vec::new();
        for i in 0..3 {
            let mut v = to_frame(&planes, w, h, fmt_for(p));
            v.pts = Some(ec_core::timebase::Timestamp::new(i, TimeBase::try_new(1001, 30000).unwrap()));
            enc.send_frame(&ec_core::frame::Frame::Video(v)).unwrap();
            let pk = enc.receive_packet().unwrap();
            assert!(pk.flags.keyframe);
            assert_eq!(pk.pts, Some(i));
            frames.push(pk.data.to_vec());
        }
        let mov = mux(&enc, &frames, w, h);
        for needle in [&p.fourcc()[..], b"fiel", b"colr", b"pasp"] {
            assert!(mov.windows(4).any(|x| x == needle), "{p:?}: missing {:?}", String::from_utf8_lossy(needle));
        }
        assert!(!mov.windows(4).any(|x| x == b"avcC" || x == b"esds"), "{p:?}: a config box");
        let path = dir.join(format!("{}.mov", String::from_utf8_lossy(&p.fourcc())));
        std::fs::write(&path, &mov).unwrap();
        let out = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
            .arg("stream=codec_name,profile,pix_fmt,field_order,color_space,color_transfer,color_primaries,codec_tag_string,width,height,nb_frames")
            .args(["-of", "default=nw=1"])
            .arg(&path)
            .output()
            .unwrap();
        let t = String::from_utf8_lossy(&out.stdout).to_string();
        let want_profile = match p {
            Profile::Proxy => "Proxy",
            Profile::Lt => "LT",
            Profile::Standard => "Standard",
            Profile::Hq => "HQ",
            Profile::P4444 => "4444",
            Profile::P4444Xq => "XQ",
        };
        for line in [
            "codec_name=prores".to_string(),
            format!("profile={want_profile}"),
            format!("pix_fmt={}", if p.is_444() { "yuv444p12le" } else { "yuv422p10le" }),
            "field_order=progressive".into(),
            "color_space=bt709".into(),
            "color_transfer=bt709".into(),
            "color_primaries=bt709".into(),
            format!("codec_tag_string={}", String::from_utf8_lossy(&p.fourcc())),
            format!("width={w}"),
            format!("height={h}"),
            "nb_frames=3".into(),
        ] {
            assert!(t.lines().any(|l| l == line), "{p:?}: ffprobe lacks `{line}` in:\n{t}");
        }
        // Our demuxer reads it back.
        let dm = Mp4Demuxer::new(Cursor::new(mov)).unwrap();
        assert_eq!(dm.streams()[0].params.codec, CodecId::ProRes);
    }
}

/// Mean PSNR of ours and prores_ks on a lavfi source: ours must be within 0.5 dB (or better).
fn psnr_vs_prores_ks(generator: &str, post: &str, tag: &str) {
    let dir = scratch(tag);
    let (w, h) = (1920usize, 1080usize);
    for (p, num, pf) in [
        (Profile::Proxy, "0", "yuv422p10le"),
        (Profile::Lt, "1", "yuv422p10le"),
        (Profile::Standard, "2", "yuv422p10le"),
        (Profile::Hq, "3", "yuv422p10le"),
        (Profile::P4444, "4", "yuv444p10le"),
    ] {
        let raw = dir.join("src.raw");
        let fmt = if p.is_444() { "yuv444p10le" } else { pf };
        assert!(run(&["-f", "lavfi", "-i", &format!("{generator}=size={w}x{h}:rate=25{post}"), "-frames:v", "2", "-pix_fmt", fmt, "-f", "rawvideo", raw.to_str().unwrap()]));
        let data = std::fs::read(&raw).unwrap();
        let fsize = data.len() / 2;
        // ffmpeg's encoder.
        let refmov = dir.join("ref.mov");
        assert!(run(&["-f", "rawvideo", "-pix_fmt", fmt, "-s", &format!("{w}x{h}"), "-i", raw.to_str().unwrap(), "-c:v", "prores_ks", "-profile", num, "-pix_fmt", fmt, refmov.to_str().unwrap()]));
        let ref_dec = raw_ffmpeg(&format!("{tag}-ref"), &refmov, fmt).unwrap();
        // Ours (10-bit 4:4:4 input is I410: a 4444 stream decodes at 12 bits, so compare at 12).
        let enc = encoder(p, w, h);
        let infmt = if p.is_444() { PixelFormat::I410 } else { PixelFormat::I210 };
        let mut frames = Vec::new();
        for f in 0..2 {
            let chunk = &data[f * fsize..(f + 1) * fsize];
            let geo = infmt.plane_geometry(w as u32, h as u32);
            let mut off = 0;
            let mut planes = Vec::new();
            for g in &geo {
                planes.push(Plane::new(chunk[off..off + g.0 * g.1].to_vec(), g.0));
                off += g.0 * g.1;
            }
            frames.push(enc.encode(&VideoFrame::try_new(infmt, w as u32, h as u32, planes).unwrap()).unwrap());
        }
        let ours_mov = dir.join("ours.mov");
        std::fs::write(&ours_mov, mux(&enc, &frames, w, h)).unwrap();
        let ours_dec = raw_ffmpeg(&format!("{tag}-ours"), &ours_mov, fmt).unwrap();
        let ref_bytes = std::fs::metadata(&refmov).unwrap().len() as f64 / 2.0;
        let ours_bytes = frames[0].len() as f64;
        // 4444 decodes in ffmpeg as 10-bit when asked for yuv444p10le: same scale as the source.
        let a = psnr(&data, &ours_dec, 1023.0);
        let b = psnr(&data, &ref_dec, 1023.0);
        eprintln!("{tag} {p:?}: ours {a:.2} dB ({ours_bytes:.0} B)  prores_ks {b:.2} dB ({ref_bytes:.0} B)");
        assert!(a >= b - 0.5, "{tag} {p:?}: ours {a:.2} dB, prores_ks {b:.2} dB");
    }
}

#[test]
fn quality_matches_prores_ks_testsrc2() {
    if !ffmpeg_or_skip() {
        return;
    }
    psnr_vs_prores_ks("testsrc2", "", "q-testsrc2");
}

#[test]
fn quality_matches_prores_ks_mandelbrot() {
    if !ffmpeg_or_skip() {
        return;
    }
    psnr_vs_prores_ks("mandelbrot", "", "q-mandel");
}

#[test]
fn quality_matches_prores_ks_noisy() {
    if !ffmpeg_or_skip() {
        return;
    }
    psnr_vs_prores_ks("testsrc2", ",noise=alls=12:allf=t", "q-noisy");
}

#[test]
fn random_small_sizes_round_trip() {
    let mut s = 0x9E3779B97F4A7C15u64;
    let mut dec = decoder(1);
    for case in 0..120 {
        let p = ALL[(xorshift(&mut s) % 6) as usize];
        let (w, h) = match case % 6 {
            0 => (1 + (xorshift(&mut s) % 16) as usize, 1 + (xorshift(&mut s) % 16) as usize), // one macroblock
            1 => (16, 16),
            _ => (1 + (xorshift(&mut s) % 150) as usize, 1 + (xorshift(&mut s) % 70) as usize),
        };
        let bits = if p.is_444() { 12 } else { 10 };
        let noise = (xorshift(&mut s) % 20) as i32;
        let planes = synth(w, h, p.is_444(), bits, noise, xorshift(&mut s));
        let mut cfg = EncoderConfig::new(p, w as u32, h as u32);
        cfg.threads = 1 + (xorshift(&mut s) % 6) as usize;
        let enc = ProResEncoder::new(cfg).unwrap();
        let f = enc.encode(&to_frame(&planes, w, h, fmt_for(p))).unwrap();
        let d = dec.decode_full(&f).unwrap_or_else(|e| panic!("case {case} {p:?} {w}x{h}: {e}"));
        assert_eq!((d.video.width as usize, d.video.height as usize), (w, h), "case {case}");
        assert_eq!(d.video.format, fmt_for(p));
        let q = psnr(&tight(&d.video), &raw_of(&planes), if p.is_444() { 4095.0 } else { 1023.0 });
        assert!(q > 40.0, "case {case} {p:?} {w}x{h} noise {noise}: PSNR {q:.1}");
    }
}

#[test]
fn encode_speed_1080p_hq() {
    let (w, h) = (1920, 1080);
    let planes = synth(w, h, false, 10, 12, 21);
    let frame = to_frame(&planes, w, h, PixelFormat::I210);
    let enc = encoder(Profile::Hq, w, h);
    enc.encode(&frame).unwrap();
    let t0 = std::time::Instant::now();
    let n = 20;
    for _ in 0..n {
        std::hint::black_box(enc.encode(&frame).unwrap());
    }
    let fps = n as f64 / t0.elapsed().as_secs_f64();
    eprintln!("1080p HQ encode, {} threads: {fps:.1} fps", enc.config().threads);
    if let Some(min) = std::env::var("EC_PRORES_ENC_MIN_FPS").ok().and_then(|v| v.parse::<f64>().ok()) {
        assert!(fps >= min, "{fps:.1} fps < {min}");
    }
}

fn probe(path: &std::path::Path, entry: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", &format!("stream={entry}"), "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .expect("ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn probe_frame(path: &std::path::Path, entry: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_frames", "-read_intervals", "%+#1", "-show_entries", &format!("frame={entry}"), "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .expect("ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn interlaced_every_profile_ffmpeg_and_ours_agree() {
    use ec_core::frame::Interlace;
    if !ffmpeg_or_skip() {
        return;
    }
    let dir = scratch("enc-field");
    for (i, p) in ALL.into_iter().enumerate() {
        for (order, field_order) in [(Interlace::TopFieldFirst, "tt"), (Interlace::BottomFieldFirst, "bb")] {
            // Odd height: the two fields differ in height; odd width: a ragged macroblock column.
            let (w, h) = (331, 75);
            let planes = synth(w, h, p.is_444(), if p.is_444() { 12 } else { 10 }, 10, 21 + i as u64);
            let enc = encoder(p, w, h);
            let mut frame = to_frame(&planes, w, h, fmt_for(p));
            frame.interlace = order;
            let f = enc.encode(&frame).unwrap();
            let mov = mux(&enc, std::slice::from_ref(&f), w, h);
            let path = dir.join(format!("{}-{field_order}.mov", String::from_utf8_lossy(&p.fourcc())));
            std::fs::write(&path, &mov).unwrap();
            // The stream-level field_order comes from the container (a `fiel` atom the mov muxer
            // does not write); the decoded frames carry the order from the ProRes frame header.
            let fo = probe_frame(&path, "interlaced_frame,top_field_first");
            let want_tff = if field_order == "tt" { "1" } else { "0" };
            assert_eq!(fo.replace('\n', " "), format!("1 {want_tff}"), "{p:?}: ffprobe frame field order");
            let ours = decode_ours(&mov, 1).unwrap();
            assert_eq!(ours[0].video.interlace, order);
            let theirs = raw_ffmpeg(&format!("enc-field-{i}"), &path, pix(p)).unwrap();
            assert_eq!(first_diff(&tight(&ours[0].video), &theirs), None, "{p:?} {order:?}: ffmpeg and ec-prores disagree");
            let peak = if p.is_444() { 4095.0 } else { 1023.0 };
            let q = psnr(&raw_of(&planes), &theirs, peak);
            assert!(q > 34.0, "{p:?} {order:?}: PSNR {q:.1} dB against the source");
            // Thread count does not change the bytes.
            let mut cfg = enc.config().clone();
            cfg.threads = 1;
            assert_eq!(ProResEncoder::new(cfg).unwrap().encode(&frame).unwrap(), f);
        }
    }
}

#[test]
fn interlaced_fields_are_coded_separately() {
    use ec_core::frame::Interlace;
    // Two very different fields: a progressive coding would smear them, field coding keeps both.
    let (w, h) = (128, 64);
    let mut planes = synth(w, h, false, 10, 0, 5);
    for y in 0..h {
        if y % 2 == 1 {
            for x in 0..w {
                planes[0][y * w + x] = if (x / 8) % 2 == 0 { 900 } else { 150 };
            }
        }
    }
    let enc = encoder(Profile::Hq, w, h);
    let mut frame = to_frame(&planes, w, h, PixelFormat::I210);
    let prog = enc.encode(&frame).unwrap();
    frame.interlace = Interlace::TopFieldFirst;
    let field = enc.encode(&frame).unwrap();
    let want = raw_of(&planes);
    let qp = psnr(&tight(&decoder(1).decode(&prog).unwrap()), &want, 1023.0);
    let qf = psnr(&tight(&decoder(1).decode(&field).unwrap()), &want, 1023.0);
    eprintln!("progressive {qp:.1} dB, field {qf:.1} dB");
    assert!(qf > qp, "field coding should help on field-different content");
}

/// Alpha test pattern: smooth ramps, flat runs, hard edges and noise, 16-bit full scale.
fn alpha_pattern(w: usize, h: usize, seed: u64) -> Vec<u16> {
    let mut s = seed | 1;
    let mut v = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let a = match (y / 16 + x / 40) % 4 {
                0 => 0xFFFFu32,
                1 => (x * 65535 / w) as u32,
                2 => 0,
                _ => (xorshift(&mut s) & 0xFFFF) as u32,
            };
            let edge = if (x + y) % 97 < 3 { 0x8000 } else { a };
            v.push(edge as u16);
        }
    }
    v
}

#[test]
fn alpha_4444_round_trips_and_ffmpeg_agrees() {
    use ec_core::frame::Interlace;
    use ec_prores::AlphaPlane;
    if !ffmpeg_or_skip() {
        return;
    }
    let dir = scratch("enc-alpha");
    let mut n = 0;
    for p in [Profile::P4444, Profile::P4444Xq] {
        for bits in [8u8, 16] {
            for order in [Interlace::Progressive, Interlace::BottomFieldFirst] {
                n += 1;
                let (w, h) = (203, 67);
                let planes = synth(w, h, true, 12, 10, 40 + n);
                let a16 = alpha_pattern(w, h, 9 + n);
                let mut cfg = EncoderConfig::new(p, w as u32, h as u32);
                cfg.alpha_bits = bits;
                let enc = ProResEncoder::new(cfg).unwrap();
                let mut frame = to_frame(&planes, w, h, PixelFormat::I412);
                frame.interlace = order;
                let ap = AlphaPlane { width: w as u32, height: h as u32, stride: w, data: a16.clone() };
                let f = enc.encode_with_alpha(&frame, Some(&ap)).unwrap();
                let mov = mux(&enc, std::slice::from_ref(&f), w, h);
                let path = dir.join(format!("a{n}.mov"));
                std::fs::write(&path, &mov).unwrap();
                let pf = probe(&path, "pix_fmt");
                assert!(pf.starts_with("yuva444p1"), "{p:?} alpha {bits}: ffprobe pix_fmt {pf}");
                // Our decode: alpha is exact (16-bit) or the 8-bit value replicated.
                let ours = decode_ours(&mov, 1).unwrap();
                let got = ours[0].alpha.as_ref().expect("alpha plane");
                for y in 0..h {
                    for x in 0..w {
                        let a = a16[y * w + x];
                        let want = if bits == 16 { a } else { (a & 0xFF00) | (a >> 8) };
                        assert_eq!(got.data[y * got.stride + x], want, "{p:?} alpha {bits} {order:?} at {x},{y}");
                    }
                }
                // ffmpeg decodes the same planes and alpha as we do.
                let theirs = raw_ffmpeg(&format!("enc-alpha-{n}"), &path, "yuva444p12le").unwrap();
                assert_eq!(first_diff(&raw_ours(&ours), &theirs), None, "{p:?} alpha {bits} {order:?}: ffmpeg and ec-prores disagree");
                let ysz = w * h * 2 * 3;
                let q = psnr(&raw_of(&planes), &theirs[..ysz], 4095.0);
                assert!(q > 34.0, "{p:?} alpha {bits} {order:?}: PSNR {q:.1} dB");
                // Without an alpha plane the coded alpha is opaque.
                let opaque = enc.encode(&frame).unwrap();
                let d = decoder(1).decode_full(&opaque).unwrap();
                assert!(d.alpha.unwrap().data.iter().all(|&a| a == 0xFFFF));
            }
        }
    }
}

#[test]
fn alpha_config_is_validated() {
    use ec_prores::AlphaPlane;
    let mut cfg = EncoderConfig::new(Profile::Hq, 64, 32);
    cfg.alpha_bits = 8;
    assert!(ProResEncoder::new(cfg).is_err(), "alpha on a 4:2:2 profile");
    let mut cfg = EncoderConfig::new(Profile::P4444, 64, 32);
    cfg.alpha_bits = 12;
    assert!(ProResEncoder::new(cfg).is_err(), "12-bit alpha");
    let enc = encoder(Profile::P4444, 64, 32);
    let planes = synth(64, 32, true, 12, 0, 1);
    let ap = AlphaPlane { width: 64, height: 32, stride: 64, data: vec![0; 64 * 32] };
    assert!(enc.encode_with_alpha(&to_frame(&planes, 64, 32, PixelFormat::I412), Some(&ap)).is_err(), "alpha without alpha_bits");
}
