mod ivf;
use ec_vp9::decode::Decoder;
#[test]
fn show_err() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes = std::fs::read(dir.join("fixtures/bitstreams/vp9-superframe-altref.ivf")).unwrap();
    let (_, _, _, frames) = ivf::parse_ivf(&bytes);
    let mut d = Decoder::new();
    let dir2 = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let b2 = std::fs::read(dir2.join("fixtures/bitstreams/vp9-superframe-altref.ivf")).unwrap();
    let (_, _, _, f2) = ivf::parse_ivf(&b2);
    println!("f0 bytes: {:02x?}", &f2[0].data[16..24]);
    match d.decode(&frames[0].data) {
        Ok(_) => println!("OK"),
        Err(e) => println!("ERR: {e}"),
    }
}
