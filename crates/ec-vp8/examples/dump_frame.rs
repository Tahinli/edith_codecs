//! Dump decoded frame N of an IVF as raw yuv420p (ffmpeg-comparable:
//! `ffmpeg -i in.ivf -f rawvideo -pix_fmt yuv420p -` streams the same
//! layout, frame after frame). Usage: `dump_frame <ivf> <frame-index>
//! <out.yuv>`. Decoding stops at the first frame the crate cannot yet
//! decode (e.g. inter frames before M3), reporting it on stderr.

use ec_vp8::decode::Decoder;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args.len(), 4, "usage: dump_frame <ivf> <frame-index> <out.yuv>");
    let want: usize = args[2].parse().expect("frame index");
    let bytes = std::fs::read(&args[1]).unwrap();

    // IVF: 32-byte file header, then (4-byte LE size, 8-byte pts, data).
    let mut frames = Vec::new();
    let mut pos = 32;
    while pos + 12 <= bytes.len() {
        let sz = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        frames.push(&bytes[pos + 12..pos + 12 + sz]);
        pos += 12 + sz;
    }

    let mut dec = Decoder::new();
    for (i, f) in frames.iter().enumerate() {
        match dec.decode(f) {
            Ok(Some(pic)) if i == want => {
                let mut out = Vec::with_capacity(
                    pic.y.len() + pic.u.len() + pic.v.len(),
                );
                for row in 0..usize::from(pic.height) {
                    let s = row * pic.stride;
                    out.extend_from_slice(&pic.y[s..s + usize::from(pic.width)]);
                }
                let (cw, ch) = (
                    usize::from(pic.width + 1) / 2,
                    usize::from(pic.height + 1) / 2,
                );
                for plane in [&pic.u, &pic.v] {
                    for row in 0..ch {
                        let s = row * pic.uv_stride;
                        out.extend_from_slice(&plane[s..s + cw]);
                    }
                }
                std::fs::write(&args[3], out).unwrap();
                return;
            }
            Ok(Some(_)) => {}
            Ok(None) => {}
            Err(e) => {
                eprintln!("frame {i}: {e}");
                std::process::exit(1);
            }
        }
    }
    eprintln!("no frame {want} decoded");
    std::process::exit(1);
}
