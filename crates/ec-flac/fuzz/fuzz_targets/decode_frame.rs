#![no_main]

use ec_flac::decode::{Block, ChannelAssignment};
use ec_flac::{FlacReader, StreamInfo};
use libfuzzer_sys::fuzz_target;

// Byte 0 selects a fixed STREAMINFO (as a container would carry it); the rest is
// bare frames, handed to `next_block` one call at a time.
fuzz_target!(|data: &[u8]| {
    let Some((&sel, frames)) = data.split_first() else {
        return;
    };
    // Harness cap: at most 16 KiB of frame data per input. The seeds run to 950 KB, and
    // uncapped the target ran at about 31 exec/s.
    let frames = &frames[..frames.len().min(16 * 1024)];
    let (sample_rate, channels, bits_per_sample) = match sel % 5 {
        0 => (44_100, 2, 16),
        1 => (48_000, 1, 24),
        2 => (96_000, 2, 32),
        3 => (22_050, 8, 8),
        _ => (192_000, 6, 12),
    };
    let info = StreamInfo {
        min_block_size: 16,
        max_block_size: 4096,
        min_frame_size: 0,
        max_frame_size: 0,
        sample_rate,
        channels,
        bits_per_sample,
        total_samples: 0,
        md5: [0; 16],
    };
    let Ok(mut reader) = FlacReader::frames(frames, Some(info)) else {
        return;
    };
    let mut block = Block::default();
    // Bounded by the input: every decoded frame consumes at least its header.
    while let Ok(true) = reader.next_block(&mut block) {
        if let Some(h) = block.header {
            // Frame decode must agree with the header it was built from.
            assert_eq!(block.channels.len(), h.channels.channel_count());
            assert!(matches!(
                h.channels,
                ChannelAssignment::Independent(_)
                    | ChannelAssignment::LeftSide
                    | ChannelAssignment::RightSide
                    | ChannelAssignment::MidSide
            ));
            assert!(block.channels.iter().all(|c| c.len() == h.block_size));
        }
    }
});
