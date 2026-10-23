#![no_main]

use std::io::Cursor;

use ec_amr::container::AmrDemuxer;
use ec_core::registry::Demuxer;
use libfuzzer_sys::fuzz_target;

// Open arbitrary bytes as an .amr/.awb file and drain every packet. Errors end
// the walk; a reserved frame type is an error but later calls must still make
// progress, so a bounded number of them is skipped.
fuzz_target!(|data: &[u8]| {
    let Ok(mut demuxer) = AmrDemuxer::new(Cursor::new(data.to_vec())) else {
        return;
    };
    let mut errors = 0usize;
    loop {
        match demuxer.next_packet() {
            Ok(_) => {}
            Err(e) if e.is_eof() => break,
            Err(_) => {
                errors += 1;
                if errors > data.len() {
                    break;
                }
            }
        }
    }
});
