//! Where does a real stream stop? Decode one AV1 file and print the answer.
//!
//! The gates in [`ec_av1::stream`] each pin one capability against one encoder
//! recipe. This asks the complementary question -- given a stream someone
//! actually produced, which refusal comes first -- and it is how the refusal
//! distribution in commit 2be815f was measured rather than reasoned about:
//!
//! ```text
//! aomenc --codec=av1 --obu -o s.obu --passes=1 --cpu-used=4 --limit=4 in.y4m
//! cargo run -p ec-av1 --example decode_probe -- s.obu
//! ```
//!
//! Note `--obu`: [`ec_av1::stream::decode_stream`] takes a raw OBU stream, and
//! an IVF file decodes as zero frames rather than as an error.
//!
//! That run is also what disproved three refusal strings claiming an encoder
//! never writes a case it demonstrably writes, so this is a first-class
//! instrument, not a scratch file.
fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: decode_probe <stream.obu>");
        std::process::exit(2);
    };
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(2);
        }
    };
    let report = || {
        let (h, v, c) = ec_av1::stream::rect4_32_counters();
        println!("rect4_32: horz={h} vert={v} coded={c}");
        let ir = ec_av1::stream::inter_rect_counters();
        println!(
            "inter_rect: 32x8={} 8x32={} 64x32={} 32x64={} 64x16={} 16x64={}",
            ir.0, ir.1, ir.2, ir.3, ir.4, ir.5
        );
    };
    match ec_av1::stream::decode_stream(&data) {
        Ok(frames) if frames.is_empty() => {
            println!("OK but EMPTY: no frames -- is {path} an IVF rather than a raw OBU stream?");
        }
        Ok(frames) => {
            // Optional second arg, or EC_PROBE_OUT=<path>: dump the decoded
            // planes as raw yuv420p so a pixel diff against `ffmpeg -i s.obu
            // -f rawvideo` needs no test harness.
            let out = std::env::args().nth(2).or_else(|| std::env::var("EC_PROBE_OUT").ok());
            if let Some(out) = out {
                // 8-bit only: planes are u16, take the low byte.
                let mut buf: Vec<u8> = Vec::new();
                for f in &frames {
                    for p in [&f.y, &f.u, &f.v] {
                        buf.extend(p.iter().map(|&s| s as u8));
                    }
                }
                std::fs::write(&out, &buf).expect("writing raw planes");
                println!("wrote {} bytes of yuv420p to {out}", buf.len());
            }
            println!("OK: {} frames decoded, {}x{}", frames.len(), frames[0].width, frames[0].height);
        }
        Err(e) => println!("REFUSED: {e}"),
    }
    report();
}
