#![no_main]

use ec_ac3::{Ac3Decoder, eac3, frame_size};
use libfuzzer_sys::fuzz_target;

// The whole input is an elementary stream: walk it syncframe by syncframe, the way a
// container would hand frames over, and decode each one. Bytes that do not start a
// syncframe are skipped one at a time. A dependent substream is handed over with the
// independent substream it extends, as a container packet carries them.
fuzz_target!(|data: &[u8]| {
    let mut dec = Ac3Decoder::new();
    let mut pos = 0;
    while pos + 2 <= data.len() {
        let rest = &data[pos..];
        if rest[0] != 0x0B || rest[1] != 0x77 {
            pos += 1;
            continue;
        }
        let size = match frame_size(rest) {
            Ok(size) => size,
            Err(ec_ac3::Error::NeedMore) => break,
            Err(_) => {
                pos += 1;
                continue;
            }
        };
        if size == 0 || size > rest.len() {
            break;
        }
        let mut end = pos + size;
        while let Some(tail) = data.get(end..) {
            let dependent = tail
                .get(2..)
                .is_some_and(|b| eac3::bsi::parse(b).is_ok_and(|h| h.strmtyp == eac3::StreamType::Dependent));
            match frame_size(tail) {
                Ok(next) if dependent && next <= tail.len() => end += next,
                _ => break,
            }
        }
        let _ = dec.decode_frame(&data[pos..end]);
        pos = end;
    }
});
