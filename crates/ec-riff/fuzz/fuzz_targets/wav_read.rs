#![no_main]

// The whole input is one WAVE stream. Header parsing runs first; if it succeeds, the
// sample reader for the matching sample type drains the `data` chunk.

use std::io::Cursor;

use ec_riff::{SampleType, WavReader};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(reader) = WavReader::new(Cursor::new(data)) else {
        return;
    };
    let spec = reader.spec();
    let _ = reader.duration();
    let _ = spec.sample_format();
    let _ = spec.block_align();
    let _ = spec.layout();
    match spec.sample_format {
        SampleType::Int => {
            let mut reader = WavReader::new(Cursor::new(data)).expect("header parsed once already");
            let _ = reader.read_all_i32();
        }
        SampleType::Float => {
            let mut reader = WavReader::new(Cursor::new(data)).expect("header parsed once already");
            let _ = reader.read_all_f32();
        }
    }
});
