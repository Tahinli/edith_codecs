mod ivf;
use ec_vp9::bool::BoolDecoder;

#[test]
fn probe_first_bools() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes = std::fs::read(dir.join("fixtures/vp9/lossless-64.ivf")).unwrap();
    let (_, _, _, frames) = ivf::parse_ivf(&bytes);
    let frame = &frames[0].data;
    println!("slice bytes: {:02x?}", &frame[18..26]);
    let mut r = BoolDecoder::new(&frame[18..18 + 113]).unwrap();
    let bits: Vec<u8> = (0..8).map(|_| u8::from(r.read_bool(128))).collect();
    println!("first 8 bools(128): {bits:?}");
}
