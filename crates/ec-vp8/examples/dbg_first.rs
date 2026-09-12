// Decode one frame: print header/dequant summary + pixel stats.
use ec_vp8::decode::Decoder;
use ec_vp8::transform;
use ec_vp8::{PersistedState, frame, header::FrameHeader};

fn main() {
    let path = std::env::args().nth(1).expect("ivf path");
    let bytes = std::fs::read(path).unwrap();
    let sz = u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]) as usize;
    let frame = &bytes[44..44 + sz];
    let mut st = PersistedState::default();
    let (h, _) = FrameHeader::parse(frame, &mut st).unwrap();
    println!(
        "q={} y2dc_d={} y2ac_d={} skip={} lvl={}",
        h.quant.yac_qi,
        h.quant.y2dc_delta,
        h.quant.y2ac_delta,
        h.coeff_skip_enabled,
        h.filter_level
    );
    let dq = transform::dequant(
        i32::from(h.quant.yac_qi),
        h.quant.ydc_delta,
        h.quant.y2dc_delta,
        h.quant.y2ac_delta,
        h.quant.uvdc_delta,
        h.quant.uvac_delta,
    );
    println!(
        "y1dc={} y1ac={} y2dc={} y2ac={}",
        dq.y1_dc, dq.y1_ac, dq.y2_dc, dq.y2_ac
    );
    let mut dec = Decoder::new();
    match dec.decode(frame) {
        Ok(Some(pic)) => println!(
            "{}x{} y[0..8]={:?} min={} max={}",
            pic.width,
            pic.height,
            &pic.y[..8],
            pic.y.iter().min().unwrap(),
            pic.y.iter().max().unwrap()
        ),
        Ok(None) => println!("hidden"),
        Err(e) => println!("ERR {e}"),
    }
}
