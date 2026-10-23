#![no_main]

// Drain every packet of every stream, then seek each stream under each mode and drain again.
//
// The input is first run through `fix_crcs`: a mutated page almost always fails its CRC and is
// dropped before any parsing past the checksum happens, so this target recomputes each page's
// checksum in place to get the reader past that gate. The byte-for-byte input still goes to
// `demux_raw`, which keeps the CRC rejection path under test.

use std::io::Cursor;

use ec_core::{Demuxer, SeekMode, Timestamp};
use ec_ogg::OggDemuxer;
use libfuzzer_sys::fuzz_target;

const MODES: [SeekMode; 3] = [SeekMode::SyncBefore, SeekMode::SyncAfter, SeekMode::Exact];

fuzz_target!(|data: &[u8]| {
    // The first eight bytes pick the seek target; the rest is the Ogg stream.
    let (head, rest) = data.split_at(data.len().min(8));
    let mut ticks = [0u8; 8];
    ticks[..head.len()].copy_from_slice(head);
    let ticks = i64::from_le_bytes(ticks);

    let mut bytes = rest.to_vec();
    fix_crcs(&mut bytes);

    let Ok(mut demux) = OggDemuxer::open(Cursor::new(bytes)) else {
        return;
    };
    drain(&mut demux);
    for stream in 0..demux.streams().len() as u32 {
        let base = demux.streams()[stream as usize].time_base;
        for mode in MODES {
            let to = Timestamp::new(ticks, base);
            let _ = demux.seek(stream, to, mode);
            drain(&mut demux);
        }
    }
});

fn drain(demux: &mut OggDemuxer<Cursor<Vec<u8>>>) {
    while demux.next_packet().is_ok() {}
}

/// Recompute the checksum of every structurally complete page, in place.
///
/// The walk is sequential: a page that fits is patched and skipped over whole, anything else
/// advances one byte. A page that does not fit is left alone, so the reader still sees the
/// damage it would see in the raw input.
fn fix_crcs(data: &mut [u8]) {
    let mut i = 0;
    while i + 27 <= data.len() {
        if &data[i..i + 4] != b"OggS" {
            i += 1;
            continue;
        }
        let nsegs = usize::from(data[i + 26]);
        let body_start = i + 27 + nsegs;
        if body_start > data.len() {
            i += 1;
            continue;
        }
        let body: usize = data[i + 27..body_start].iter().map(|&s| usize::from(s)).sum();
        let end = body_start + body;
        if end > data.len() {
            i += 1;
            continue;
        }
        data[i + 22..i + 26].fill(0);
        let crc = ogg_crc(&data[i..end]);
        data[i + 22..i + 26].copy_from_slice(&crc.to_le_bytes());
        i = end;
    }
}

/// The Ogg page checksum: CRC-32, polynomial 0x04c11db7, unreflected, zero initial value and no
/// final inversion. Written out here rather than imported, because the library keeps its copy
/// private.
fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &byte in bytes {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = match crc & 0x8000_0000 {
                0 => crc << 1,
                _ => (crc << 1) ^ 0x04c1_1db7,
            };
        }
    }
    crc
}
