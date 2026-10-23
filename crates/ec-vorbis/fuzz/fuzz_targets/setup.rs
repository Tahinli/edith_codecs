#![no_main]

// Input layout:
//   three packets, each prefixed by a u16 big-endian length: identification, comment, setup.
//   A length past the end of the input is clamped to what remains. Extra bytes after the third
//   packet are ignored.

use ec_vorbis::{Comments, Identification, VorbisDecoder};
use libfuzzer_sys::fuzz_target;

/// Split `data` into at most `max` u16-BE-length-prefixed packets.
fn chunks(mut data: &[u8], max: usize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while out.len() < max && data.len() >= 2 {
        let len = u16::from_be_bytes([data[0], data[1]]) as usize;
        let (packet, tail) = data[2..].split_at(len.min(data.len() - 2));
        out.push(packet);
        data = tail;
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let parts = chunks(data, 3);
    // Each parser on its own first, then all three together through the public decoder
    // constructor, which runs Setup::parse against the parsed identification header.
    if let Some(ident) = parts.first() {
        let _ = Identification::parse(ident);
    }
    if let Some(comment) = parts.get(1) {
        let _ = Comments::parse(comment);
    }
    if let [ident, comment, setup] = parts.as_slice() {
        let _ = VorbisDecoder::new(&[*ident, *comment, *setup]);
    }
});
