use ec_vp9_syntax::Vp9Parser;
#[test]
fn hdr_probe() {
    for name in ["lossless-64.ivf", "key-320.ivf"] {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop(); p.pop();
        p = p.join("fixtures/vp9").join(name);
        let bytes = std::fs::read(&p).unwrap();
        let fsize = u32::from_le_bytes(bytes[32..36].try_into().unwrap()) as usize;
        let frame = &bytes[44..44 + fsize];
        let mut pp = Vp9Parser::new();
        match pp.parse_frame(frame) {
            Ok(h) => println!(
                "{name}: w={} h={} uhs={} hsib={} lf_level={} sharp={} delta={} base_q={} ydc={} uvdc={} uvac={} seg={} tile={:?} txmode_hdr_bytes={}",
                h.width, h.height, h.uncompressed_header_size, h.header_size_in_bytes,
                h.loop_filter.level, h.loop_filter.sharpness, h.loop_filter.delta_enabled,
                h.quantization.base_q_idx, h.quantization.delta_q_y_dc,
                h.quantization.delta_q_uv_dc, h.quantization.delta_q_uv_ac,
                h.segmentation.enabled, h.tile_info,
                h.header_size_in_bytes
            ),
            Err(e) => println!("{name}: ERR {e}"),
        }
    }
}
