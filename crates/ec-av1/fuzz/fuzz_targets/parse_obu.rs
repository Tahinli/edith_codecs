#![no_main]

// Untrusted entry: a raw AV1 OBU stream (low-overhead bitstream format) through
// the ec-av1-syntax parsers: the OBU framing of a whole temporal unit, then the
// OBU header, leb128 size and sequence header read directly off the raw bytes.
// Errors are expected for malformed input; only panics, hangs and OOM count.

use ec_av1_syntax::obu::{self, ObuHeader};
use ec_av1_syntax::sequence::SequenceHeader;
use ec_av1_syntax::Av1Parser;
use ec_core::bitio::BitReader;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut parser = Av1Parser::new();
    if let Ok(obus) = parser.parse_temporal_unit(data) {
        std::hint::black_box(obus.len());
    }

    let mut r = BitReader::new(data);
    if let Ok(header) = ObuHeader::parse(&mut r) {
        std::hint::black_box(header);
    }
    let mut r = BitReader::new(data);
    if let Ok(size) = obu::read_leb128(&mut r) {
        std::hint::black_box(size);
    }
    let mut r = BitReader::new(data);
    if let Ok(seq) = SequenceHeader::parse(&mut r) {
        std::hint::black_box(seq.max_frame_width.wrapping_mul(seq.max_frame_height));
    }
});
