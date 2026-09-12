//! Throwaway decode-throughput probe for lane-h264perf: raw demux+decode
//! Mpx/s, no render. One untimed warm pass, then best-of-N timed passes over a
//! decoder built fresh per pass. `--hash` FNV-1a-hashes every released frame
//! (display order, visible rows only) so a byte-exact A/B is a file diff.
//!
//! `.mp4` drives the registry surface (ec-mp4 demux, avcC extradata,
//! send_packet/receive_frame); `.264` drives the NAL surface (push_nal +
//! end_picture + flush), both the way the crate's own conformance suite does.
//!
//! usage: bench_decode <file.mp4|file.264> [reps] [--hash]
use std::time::Instant;

use ec_core::frame::{Frame, Plane};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, Decoder as _, Demuxer as _};
use ec_h264::{Decoder as H264NalDecoder, H264Decoder, NalOutcome};

/// FNV-1a 64 over the visible bytes of every plane (I420 geometry — the same
/// bytes conformance.rs's frame_bytes packs), plus the geometry.
fn hash_frame(h: &mut u64, w: u32, height: u32, planes: &[Plane]) {
    let mut fold = |bytes: &[u8]| {
        for &b in bytes {
            *h ^= u64::from(b);
            *h = h.wrapping_mul(0x100000001b3);
        }
    };
    fold(&w.to_le_bytes());
    fold(&height.to_le_bytes());
    let (w2, h2) = (w.div_ceil(2) as usize, height.div_ceil(2) as usize);
    let dims = [(w as usize, height as usize), (w2, h2), (w2, h2)];
    for (plane, (pw, ph)) in planes.iter().zip(dims) {
        for y in 0..ph {
            let row = plane.row(y, pw).expect("plane row");
            fold(row);
        }
    }
}

fn decode_mp4(
    packets: &[Packet],
    params: &ec_core::registry::CodecParameters,
    hash: bool,
) -> (usize, u64, u32, u32) {
    let mut dec = H264Decoder::new(params.clone()).expect("h264 decoder");
    let mut frames = 0usize;
    let mut h: u64 = 0xcbf29ce484222325;
    let mut dims = (0u32, 0u32);
    for p in packets {
        dec.send_packet(p).expect("send_packet");
        while let Ok(Frame::Video(f)) = dec.receive_frame() {
            if hash {
                hash_frame(&mut h, f.width, f.height, &f.planes);
            }
            dims = (f.width, f.height);
            frames += 1;
        }
    }
    dec.flush().expect("flush");
    while let Ok(Frame::Video(f)) = dec.receive_frame() {
        if hash {
            hash_frame(&mut h, f.width, f.height, &f.planes);
        }
        dims = (f.width, f.height);
        frames += 1;
    }
    (frames, h, dims.0, dims.1)
}

fn decode_annexb(bytes: &[u8], hash: bool) -> (usize, u64, u32, u32) {
    let mut dec = H264NalDecoder::new();
    let mut frames = 0usize;
    let mut h: u64 = 0xcbf29ce484222325;
    let mut dims = (0u32, 0u32);
    for nal in ec_h264_syntax::AnnexBIter::new(bytes) {
        if dec.push_nal(nal).expect("push_nal") == NalOutcome::PictureBoundary {
            dec.end_picture().expect("end_picture");
            dec.push_nal(nal).expect("re-push");
        }
        while let Some(f) = dec.next_frame() {
            if hash {
                hash_frame(&mut h, f.width, f.height, &f.planes);
            }
            dims = (f.width, f.height);
            frames += 1;
        }
    }
    dec.flush().expect("flush");
    while let Some(f) = dec.next_frame() {
        if hash {
            hash_frame(&mut h, f.width, f.height, &f.planes);
        }
        dims = (f.width, f.height);
        frames += 1;
    }
    (frames, h, dims.0, dims.1)
}

enum Source {
    Mp4(Vec<Packet>, ec_core::registry::CodecParameters),
    AnnexB(Vec<u8>),
}

fn decode(src: &Source, hash: bool) -> (usize, u64, u32, u32) {
    match src {
        Source::Mp4(p, par) => decode_mp4(p, par, hash),
        Source::AnnexB(b) => decode_annexb(b, hash),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: bench_decode <file.mp4|.264> [reps] [--hash]");
    let reps: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let hash = args.iter().any(|a| a == "--hash");

    let src = if path.ends_with(".mp4") {
        let file = std::fs::File::open(path).expect("open mp4");
        let mut demux =
            ec_mp4::Mp4Demuxer::new(std::io::BufReader::new(file)).expect("ec-mp4 open");
        let (idx, params) = demux
            .streams()
            .iter()
            .find(|s| s.params.codec == CodecId::H264)
            .map(|s| (s.index, s.params.clone()))
            .expect("no H.264 track");
        let mut packets = Vec::new();
        while let Ok(p) = demux.next_packet() {
            if p.stream == idx {
                packets.push(p);
            }
        }
        Source::Mp4(packets, params)
    } else {
        Source::AnnexB(std::fs::read(path).expect("read annexb"))
    };

    // Warm pass (untimed): page in, branch predictors, allocator pools.
    let (frames, h, w, height) = decode(&src, hash);
    assert!(frames > 0, "no frames decoded");
    if hash {
        println!("hash={h:016x} frames={frames} {w}x{height}");
    }

    let px = u64::from(w) * u64::from(height) * frames as u64;
    let mut best = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        let r = decode(&src, false);
        let e = t.elapsed().as_secs_f64();
        assert_eq!(r.0, frames, "frame count diverged between passes");
        best = best.min(e);
    }
    println!(
        "{path}: frames={frames} px={px} best={:.1}ms over {reps} -> {:.1} Mpx/s",
        best * 1e3,
        px as f64 / best / 1e6
    );
}
