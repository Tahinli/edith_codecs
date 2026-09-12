// Decode frame 0 of an IVF with decision tracing (EC_VP8_TRACE=1).
use ec_vp8::decode::Decoder;

fn main() {
    let path = std::env::args().nth(1).expect("ivf path");
    let bytes = std::fs::read(path).unwrap();
    let sz = u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]) as usize;
    let frame = &bytes[44..44 + sz];
    let mut dec = Decoder::new();
    match dec.decode(frame) {
        Ok(Some(pic)) => println!(
            "OUT {}x{} y[0..8]={:?}",
            pic.width, pic.height, &pic.y[..8]
        ),
        Ok(None) => println!("OUT hidden"),
        Err(e) => println!("OUT ERR {e}"),
    }
}
