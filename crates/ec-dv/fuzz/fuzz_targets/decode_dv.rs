#![no_main]

// Untrusted entry: arbitrary bytes as a DV frame (video + audio extraction), as a frame whose
// DIF header is forced valid for a profile chosen by the first byte (so the entropy decoder is
// reached), and as a raw .dv stream through the demuxer. Errors are expected; only panics,
// hangs and OOM count.

use ec_core::registry::Demuxer;
use ec_dv::{DvDemuxer, DvVideoDecoder, PROFILES, extract_audio, is_dv};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let mut dec = DvVideoDecoder::new();
    let mut frame;
    let buf: &[u8] = if usize::from(sel) < PROFILES.len() * 4 {
        let p = &PROFILES[usize::from(sel) % PROFILES.len()];
        frame = vec![0u8; p.frame_size];
        let n = rest.len().min(frame.len());
        frame[..n].copy_from_slice(&rest[..n]);
        frame[3] = (frame[3] & 0x7f) | (p.dsf << 7);
        let v = 80 * 5 + 48 + 3;
        frame[v] = (frame[v] & 0x20) | p.video_stype;
        &frame
    } else {
        data
    };
    if let Ok(pic) = dec.decode(buf) {
        std::hint::black_box(pic);
    }
    std::hint::black_box(extract_audio(buf));
    let _ = is_dv(data);
    if let Ok(mut d) = DvDemuxer::new(std::io::Cursor::new(data.to_vec())) {
        for _ in 0..4 {
            if d.next_packet().is_err() {
                break;
            }
        }
    }
});
