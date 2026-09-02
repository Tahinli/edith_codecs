//! Decode one OBU stream and write every frame's planes as raw little-endian
//! `u16` (`<out-prefix>.f<N>.yuv`) — the high-bit-depth companion to
//! `EC_AV1_PREFILT_DUMP`, whose `as u8` narrowing throws away exactly the bits
//! a 10-bit mismatch lives in (lane-rect1d r1 found its defect by diffing this
//! against `ffmpeg -pix_fmt yuv420p10le -f rawvideo`).
//!
//! ```text
//! cargo run -p ec-av1 --example dump_yuv -- s.obu /tmp/ours
//! ffmpeg -v error -i s.obu -pix_fmt yuv420p10le -f rawvideo /tmp/ref.yuv
//! cmp /tmp/ours.f0.yuv /tmp/ref.yuv
//! ```
fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: dump_yuv <stream.obu> <out-prefix>");
        std::process::exit(2);
    };
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(2);
        }
    };
    let frames = match ec_av1::stream::decode_stream(&data) {
        Ok(frames) => frames,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    for (i, f) in frames.iter().enumerate() {
        let mut buf = Vec::with_capacity((f.y.len() + f.u.len() + f.v.len()) * 2);
        for plane in [&f.y, &f.u, &f.v] {
            for &s in plane.iter() {
                buf.extend_from_slice(&s.to_le_bytes());
            }
        }
        let name = format!("{out}.f{i}.yuv");
        if let Err(e) = std::fs::write(&name, &buf) {
            eprintln!("{name}: {e}");
            std::process::exit(2);
        }
        println!("frame {i}: {}x{} -> {name}", f.width, f.height);
    }
}
