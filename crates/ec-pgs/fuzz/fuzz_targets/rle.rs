#![no_main]

use ec_pgs::{PgsDecoder, SEG_END, SEG_ODS, SEG_PCS, SEG_PDS};
use libfuzzer_sys::fuzz_target;

// The RLE bitmap in isolation. A display set is built around the input: a canvas and one object
// whose declared width and height come from the first four bytes, its data is the rest. Then the
// decoder runs it. Bitmaps past the object limit are refused, the rest decode or are dropped.
fn segment(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = b"PG".to_vec();
    out.extend_from_slice(&[0; 8]);
    out.push(kind);
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let width = u16::from_be_bytes([data[0], data[1]]);
    let height = u16::from_be_bytes([data[2], data[3]]);
    let rle = &data[4..];
    // Palette: entry 0 clear, entry 1 opaque white. The composition is 1920x1080.
    let pcs = [0x07, 0x80, 0x04, 0x38, 0x10, 0, 0, 0x80, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let pds = [0, 0, 1, 235, 128, 128, 255];
    let mut ods = vec![0, 0, 0, 0xC0];
    ods.extend_from_slice(&((rle.len() + 4) as u32).to_be_bytes()[1..]);
    ods.extend_from_slice(&width.to_be_bytes());
    ods.extend_from_slice(&height.to_be_bytes());
    ods.extend_from_slice(rle);
    let stream = [
        segment(SEG_PCS, &pcs),
        segment(SEG_PDS, &pds),
        segment(SEG_ODS, &ods),
        segment(SEG_END, &[]),
    ]
    .concat();
    let mut decoder = PgsDecoder::new();
    let _ = decoder.push(&stream);
    while let Some(frame) = decoder.take_frame() {
        std::hint::black_box((frame.width, frame.height));
    }
});
