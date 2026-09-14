use ec_vp9_syntax::Vp9Parser;
#[test]
fn synth() {
    let frame = std::fs::read("/tmp/synth-kf-frame.bin").unwrap();
    let mut p = Vp9Parser::new();
    match p.parse_frame(&frame) {
        Ok(h) => println!("w={} h={} uhs={} hsib={}", h.width, h.height, h.uncompressed_header_size, h.header_size_in_bytes),
        Err(e) => println!("ERR {e}"),
    }
}
