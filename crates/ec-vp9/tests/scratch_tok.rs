//! [scratch] Single decode of key-320 frame 0 with EC_VP9_TRACE=1 for
//! oracle TOK/TXB/MODE/EOB diffing. stderr is the trace.

mod ivf;

use ec_vp9::decode::Decoder;

#[test]
fn dump_tok_trace() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vp9/key-320.ivf");
    let bytes = std::fs::read(&path).unwrap();
    let (_f, _w, _h, frames) = ivf::parse_ivf(&bytes);
    let mut dec = Decoder::new();
    let _pic = dec.decode(&frames[0].data).expect("decode").expect("shown");
}
