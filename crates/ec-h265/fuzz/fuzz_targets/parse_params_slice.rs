#![no_main]

// Untrusted entry: a raw Annex B byte stream, parsed NAL by NAL with the
// ec-h265-syntax parsers. Every VPS, SPS and PPS is parsed, every slice segment
// header is parsed against the most recent SPS and PPS that parsed. Errors are
// expected for malformed input; only panics, hangs and OOM count.

use ec_h265_syntax::slice::SliceHeader;
use ec_h265_syntax::{NalUnitType, Pps, Sps, Vps, split_annex_b};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut sps: Option<Sps> = None;
    let mut pps: Option<Pps> = None;
    for nal in split_annex_b(data) {
        let rbsp = nal.rbsp();
        match nal.header.nal_type {
            NalUnitType::Vps => {
                if let Ok(vps) = Vps::parse(&rbsp) {
                    std::hint::black_box(vps);
                }
            }
            NalUnitType::Sps => {
                if let Ok(parsed) = Sps::parse(&rbsp) {
                    sps = Some(parsed);
                }
            }
            NalUnitType::Pps => {
                if let Ok(parsed) = Pps::parse(&rbsp) {
                    pps = Some(parsed);
                }
            }
            t if t.is_vcl() => {
                if let (Some(s), Some(p)) = (&sps, &pps) {
                    if let Ok((header, positions)) = SliceHeader::parse(&rbsp, s, p, t) {
                        std::hint::black_box((header, positions.slice_data_byte_offset(nal.payload)));
                    }
                }
            }
            _ => {}
        }
    }
});
