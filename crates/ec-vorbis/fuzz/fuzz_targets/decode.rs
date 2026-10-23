#![no_main]

// Input layout:
//   byte 0      selects one of three fixed header sets (mod 3): mono, stereo, 5.1 at 48 kHz,
//               each embedded from fixtures/audio via include_bytes! (see fuzz/seed.sh)
//   then        audio packets, each prefixed by a u16 big-endian length (a length past the end
//               is clamped to what remains), decoded in sequence on one decoder.

use ec_vorbis::VorbisDecoder;
use libfuzzer_sys::fuzz_target;

// Each blob is three u16-BE-length-prefixed header packets: identification, comment, setup.
const MONO: &[u8] = include_bytes!("../headers/mono-48k.bin");
const STEREO: &[u8] = include_bytes!("../headers/stereo-48k.bin");
const SURROUND: &[u8] = include_bytes!("../headers/surround-48k.bin");

/// Split `data` into u16-BE-length-prefixed packets, at most `max` of them.
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
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    // Harness cap: at most 4 KiB of packets and 8 packets per input. Uncapped, the seeds (up to
    // 62 KB of packets) ran at about 16 exec/s. Capped: 66 exec/s in a 60 s probe, 33 exec/s
    // averaged over the hour-long run on a shared host.
    let rest = &rest[..rest.len().min(4096)];
    let blob = match sel % 3 {
        0 => MONO,
        1 => STEREO,
        _ => SURROUND,
    };
    // The blobs are constants, so a failure here is a harness bug, not an input finding.
    let headers = chunks(blob, 3);
    let mut dec = VorbisDecoder::new(&headers).expect("built-in Vorbis headers must parse");
    for packet in chunks(rest, 8) {
        let _ = dec.decode_audio(packet);
    }
});
