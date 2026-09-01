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
    match ec_av1::stream::decode_stream(&data) {
        Ok(frames) if frames.is_empty() => {
            println!("OK but EMPTY: no frames -- is {path} an IVF rather than a raw OBU stream?");
        }
        Ok(frames) => {
            println!("OK: {} frames decoded", frames.len());
            // Optional second argument: write the decoded planes as raw
            // I420 (`ffmpeg -pix_fmt yuv420p -f rawvideo` order), so a
            // mismatch against ffmpeg's own decode can be located per pixel
            // without going through a gate.
            if let Some(out) = std::env::args().nth(2) {
                let mut raw = Vec::new();
                for f in &frames {
                    for plane in [&f.y, &f.u, &f.v] {
                        raw.extend(plane.iter().map(|&v| v as u8));
                    }
                }
                std::fs::write(&out, raw).expect("writing the raw dump");
                println!("wrote {out}");
            }
        }
        Err(e) => println!("REFUSED: {e}"),
    }
}
