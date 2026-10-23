//! Encode raw planar video to a ProRes `.mov`.
//! `enc in.yuv WxH i210|i422|i410|i412|i444 proxy|lt|422|hq|4444|4444xq out.mov [fps_num]`
//! with env `THREADS`, `QSCALE`, `QUIET`; prints bytes per frame and frames per second.
use std::time::Instant;

use ec_core::frame::{ColorInfo, PixelFormat, Plane, VideoFrame};
use ec_core::registry::{Muxer, StreamInfo};
use ec_core::timebase::TimeBase;
use ec_core::Packet;
use ec_mp4::Mp4Muxer;
use ec_prores::{EncoderConfig, Profile, ProResEncoder};
use ec_core::registry::Encoder;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (w, h) = a[2].split_once('x').map(|(w, h)| (w.parse::<u32>().unwrap(), h.parse::<u32>().unwrap())).unwrap();
    let fmt = match a[3].as_str() {
        "i210" => PixelFormat::I210,
        "i422" => PixelFormat::I422,
        "i410" => PixelFormat::I410,
        "i412" => PixelFormat::I412,
        _ => PixelFormat::I444,
    };
    let profile = match a[4].as_str() {
        "proxy" => Profile::Proxy,
        "lt" => Profile::Lt,
        "422" => Profile::Standard,
        "hq" => Profile::Hq,
        "4444" => Profile::P4444,
        _ => Profile::P4444Xq,
    };
    let data = std::fs::read(&a[1]).unwrap();
    let geo = fmt.plane_geometry(w, h);
    let fsize: usize = geo.iter().map(|g| g.0 * g.1).sum();
    let mut cfg = EncoderConfig::new(profile, w, h);
    cfg.color = ColorInfo { primaries: 1, transfer: 1, matrix: 1, full_range: false };
    if let Some(t) = std::env::var("THREADS").ok().and_then(|v| v.parse().ok()) {
        cfg.threads = t;
    }
    cfg.qscale = std::env::var("QSCALE").ok().and_then(|v| v.parse().ok());
    let enc = ProResEncoder::new(cfg).unwrap();
    let tb = TimeBase::try_new(1001, 30000).unwrap();
    let mut mux = Mp4Muxer::with_brands(std::fs::File::create(&a[5]).unwrap(), *b"qt  ", &[*b"qt  "]).unwrap();
    let mut info = StreamInfo::new(0, tb, enc.codec_parameters().clone());
    info.default = true;
    let s = mux.add_stream(info).unwrap();
    let t0 = Instant::now();
    let mut total = 0usize;
    let n = data.len() / fsize;
    for i in 0..n {
        let mut off = i * fsize;
        let mut planes = Vec::new();
        for (rb, rows) in &geo {
            planes.push(Plane::new(data[off..off + rb * rows].to_vec(), *rb));
            off += rb * rows;
        }
        let v = VideoFrame::try_new(fmt, w, h, planes).unwrap();
        let bytes = enc.encode(&v).unwrap();
        total += bytes.len();
        let mut p = Packet::new(s, tb, bytes);
        p.pts = Some(i as i64);
        p.dts = Some(i as i64);
        p.duration = Some(1);
        p.flags.keyframe = true;
        mux.write_packet(&p).unwrap();
    }
    let el = t0.elapsed().as_secs_f64();
    mux.finish().unwrap();
    if std::env::var_os("QUIET").is_none() {
        println!("frames {n} avg_bytes {} fps {:.1}", total / n.max(1), n as f64 / el);
    }
}
