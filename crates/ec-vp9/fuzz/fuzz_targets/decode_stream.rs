#![no_main]

use libfuzzer_sys::fuzz_target;

/// Harness caps (see lanes/fuzz1hvideo.report.md): at most 8 IVF frames per input, and the
/// stream is cut before the first key frame with luma above 128x128.
const MAX_DIM: u32 = 128;
const MAX_FRAMES: usize = 8;
const IVF_HEADER: usize = 32;
const IVF_FRAME_HEADER: usize = 12;

fuzz_target!(|data: &[u8]| {
    // Whole IVF stream through the container demuxer and the frame decoder.
    let _ = ec_vp9::stream::decode_stream(capped(data));
});

/// The prefix of `data` that holds at most `MAX_FRAMES` IVF frames and no oversized key frame.
fn capped(data: &[u8]) -> &[u8] {
    let mut end = IVF_HEADER.min(data.len());
    for _ in 0..MAX_FRAMES {
        let Some(head) = data.get(end..end + IVF_FRAME_HEADER) else {
            break;
        };
        let size = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let start = end + IVF_FRAME_HEADER;
        let stop = start.saturating_add(size).min(data.len());
        if oversized_frame(&data[start..stop]) {
            break;
        }
        end = stop;
    }
    &data[..end]
}

fn oversized_frame(frame: &[u8]) -> bool {
    matches!(frame_dims(frame), Some((w, h)) if w > MAX_DIM || h > MAX_DIM)
}

/// Frame size of a key frame or an intra-only frame from its uncompressed header (VP9 spec 6.2),
/// or `None` for inter frames and headers too short to read. Both carry their own frame size.
fn frame_dims(frame: &[u8]) -> Option<(u32, u32)> {
    let mut r = Bits { data: frame, pos: 0 };
    if r.bits(2)? != 2 {
        return None;
    }
    let profile = (r.bits(1)? << 1) | r.bits(1)?;
    if profile == 3 {
        r.bits(1)?;
    }
    if r.bits(1)? == 1 {
        return None; // show_existing_frame
    }
    let key = r.bits(1)? == 0;
    let show_frame = r.bits(1)?;
    let error_resilient = r.bits(1)?;
    let intra_only = if key {
        false
    } else {
        let intra_only = if show_frame == 0 { r.bits(1)? == 1 } else { false };
        if error_resilient == 0 {
            r.bits(2)?; // reset_frame_context
        }
        intra_only
    };
    if !key && !intra_only {
        return None;
    }
    if r.bits(24)? != 0x49_83_42 {
        return None;
    }
    if key || profile > 0 {
        color_config(&mut r, profile)?;
    }
    if !key {
        r.bits(8)?; // refresh_frame_flags
    }
    let width = r.bits(16)? + 1;
    let height = r.bits(16)? + 1;
    Some((width, height))
}

fn color_config(r: &mut Bits, profile: u32) -> Option<()> {
    if profile >= 2 {
        r.bits(1)?; // ten_or_twelve_bit
    }
    let color_space = r.bits(3)?;
    let odd_profile = profile == 1 || profile == 3;
    if color_space != 7 {
        r.bits(1)?; // color_range
        if odd_profile {
            r.bits(3)?; // subsampling_x, subsampling_y, reserved_zero
        }
    } else if odd_profile {
        r.bits(1)?; // reserved_zero
    }
    Some(())
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut value = 0;
        for _ in 0..n {
            let byte = *self.data.get(self.pos >> 3)?;
            value = (value << 1) | u32::from((byte >> (7 - (self.pos & 7))) & 1);
            self.pos += 1;
        }
        Some(value)
    }
}
