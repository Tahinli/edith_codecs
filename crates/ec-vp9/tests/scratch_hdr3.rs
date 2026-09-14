mod ivf;
use ec_vp9_syntax::Vp9Parser;
#[test]
fn dump_hdr() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes = std::fs::read(dir.join("fixtures/bitstreams/vp9-superframe-altref.ivf")).unwrap();
    let (_, _, _, frames) = ivf::parse_ivf(&bytes);
    let subs = ec_vp9_syntax::superframe::split(&frames[0].data).unwrap();
    println!("subframes: {}", subs.len());
    let mut p = Vp9Parser::new();
    let h = p.parse_frame(subs[0]).unwrap();
    println!("q={:#?} lf={:#?} seg={:#?}", h.quantization, h.loop_filter, h.segmentation);
    println!("uh={} hsz={} tile={:#?} w={} h={}", h.uncompressed_header_size, h.header_size_in_bytes, h.tile_info, h.width, h.height);
}
