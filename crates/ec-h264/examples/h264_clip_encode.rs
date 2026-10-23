//! Encode a raw I420 clip and write the bitstream and the encoder's own
//! reconstruction, for the ffmpeg recon-match and rate-control checks in
//! `scripts/h264-lane-check.sh`. Not a product surface.
//!
//! Usage: h264_clip_encode <in.yuv|-> <width> <height> <fps> <bitrate|0> <qp> <threads> <out-prefix|none>
//!
//! `-` reads raw I420 from stdin, so a 60 s 1080p clip never has to exist on
//! disk. Writes `<out-prefix>.h264` (Annex B), and `<out-prefix>.recon.yuv`
//! (every reconstructed picture, display order, I420 at the coded size) unless
//! the prefix is `none`. Prints frames, bytes and the wall time spent inside
//! `encode` on stdout.
use ec_h264::{Encoder, EncoderConfig, PictureView};
use std::io::Write;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 9 {
        eprintln!(
            "usage: h264_clip_encode <in.yuv|-> <w> <h> <fps> <bitrate> <qp> <threads> <out-prefix|none>"
        );
        std::process::exit(2);
    }
    let parse = |i: usize| -> f64 { args[i].parse().expect("numeric argument") };
    let (w, h) = (parse(2) as u32, parse(3) as u32);
    let fps = parse(4) as f32;
    let bitrate = parse(5) as u32;
    let qp = parse(6) as i32;
    let threads = parse(7) as usize;
    let prefix = &args[8];

    let (wu, hu) = (w as usize, h as usize);
    let cw = wu.div_ceil(2);
    let ch = hu.div_ceil(2);
    let frame_len = wu * hu + 2 * cw * ch;
    let mut input: Box<dyn std::io::Read> = if args[1] == "-" {
        Box::new(std::io::stdin().lock())
    } else {
        Box::new(std::fs::File::open(&args[1]).expect("open raw i420 input"))
    };

    let mut cfg = EncoderConfig::new(w, h);
    cfg.framerate = fps;
    cfg.bitrate = bitrate;
    cfg.qp = qp;
    cfg.threads = threads;
    // H264_CAVLC=1 codes CAVLC instead of the default CABAC (identity checks).
    cfg.cabac = std::env::var_os("H264_CAVLC").is_none();
    cfg.gop_size = (fps.round() as u32).max(1) * 2;
    let mut enc = Encoder::new(cfg).expect("h264 encoder");

    let mut stream_out = (prefix != "none")
        .then(|| std::fs::File::create(format!("{prefix}.h264")).expect("create bitstream"));
    let mut recon_out = (prefix != "none")
        .then(|| std::fs::File::create(format!("{prefix}.recon.yuv")).expect("create recon"));
    // H264_TRACE=1 prints one line per picture: its QP and size.
    let trace = std::env::var_os("H264_TRACE").is_some();
    let mut frame = vec![0u8; frame_len];
    let mut frames = 0usize;
    let mut bytes = 0usize;
    let mut spent = std::time::Duration::ZERO;
    while input.read_exact(&mut frame).is_ok() {
        let (y, rest) = frame.split_at(wu * hu);
        let (u, v) = rest.split_at(cw * ch);
        let view = PictureView::i420(w, h, y, u, v);
        let t0 = Instant::now();
        let pic = enc.encode(&view).expect("h264 encode");
        spent += t0.elapsed();
        if trace {
            println!(
                "trace {frames} key {} qp {} bytes {}",
                u8::from(pic.key_frame),
                pic.qp,
                pic.au.len()
            );
        }
        frames += 1;
        bytes += pic.au.len();
        if let Some(out) = stream_out.as_mut() {
            out.write_all(&pic.au).expect("write bitstream");
        }
        if let Some(out) = recon_out.as_mut() {
            let (ry, ru, rv) = enc.reconstruction().expect("a reconstruction");
            out.write_all(&ry)
                .and_then(|_| out.write_all(&ru))
                .and_then(|_| out.write_all(&rv))
                .expect("write reconstruction");
        }
    }
    println!(
        "frames {frames} bytes {bytes} encode_secs {:.3} fps {:.2}",
        spent.as_secs_f64(),
        frames as f64 / spent.as_secs_f64()
    );
    let t = enc.timings();
    println!(
        "stages pad {:.3} bands_wall {:.3} bands_busy {:.3} bands_slowest {:.3} stitch {:.3} deblock {:.3} output {:.3}",
        t.pad, t.bands_wall, t.bands_busy, t.bands_slowest, t.stitch, t.deblock, t.output
    );
    #[cfg(feature = "enc-profile")]
    {
        let snap = ec_h264::mb_stages::snapshot();
        for (name, ns) in ec_h264::mb_stages::NAMES.iter().zip(snap) {
            println!("mb_stage {name} {:.3}", ns as f64 / 1e9);
        }
    }
}
