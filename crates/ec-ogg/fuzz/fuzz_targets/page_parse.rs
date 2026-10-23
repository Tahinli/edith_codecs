#![no_main]

use ec_ogg::{PageHeader, xiph_unlace};
use libfuzzer_sys::fuzz_target;

/// Bytes before the segment table in a page (RFC 3533 section 6); the library keeps this private.
const HEADER_LEN: usize = 27;

// The fixed header, then the segment table it announces, then Xiph extradata.
fuzz_target!(|data: &[u8]| {
    if let Ok((mut header, nsegs)) = PageHeader::parse_fixed(data) {
        let Some(table) = data.get(HEADER_LEN..HEADER_LEN + nsegs) else {
            return;
        };
        header.segments = table.to_vec();
        let _ = header.body_len();
        let _ = PageHeader::stored_crc(data);
    }
    let _ = xiph_unlace(data);
});
