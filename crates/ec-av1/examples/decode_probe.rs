//! Temporary probe: decode an IVF and print what the decoder says.
fn main() {
    let path = std::env::args().nth(1).expect("usage: decode_probe <file.ivf>");
    let data = std::fs::read(&path).expect("read");
    match ec_av1::stream::decode_stream(&data) {
        Ok(frames) => println!("OK: {} frames decoded", frames.len()),
        Err(e) => println!("REFUSED: {e}"),
    }
}
