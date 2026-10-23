#![no_main]

use ec_vp9::decode::Decoder;
use libfuzzer_sys::fuzz_target;

/// Harness caps (see lanes/fuzz1hvideo.report.md): a key frame with luma above
/// 128x128 is not fed. Inter frames inherit the size of the key frame before them,
/// so a part that starts with an oversized key frame ends the input.
const MAX_DIM: u32 = 128;

fuzz_target!(|data: &[u8]| {
    if oversized_frame(data) {
        return;
    }
    // The whole input as one frame or superframe, on a fresh decoder.
    let _ = Decoder::new().decode(data);

    // The same bytes cut at 2 or 3 points and decoded in order, so that the
    // inter-frame state carried between calls is exercised too.
    if let Some(parts) = split(data) {
        let mut dec = Decoder::new();
        for part in parts {
            if oversized_frame(part) {
                break;
            }
            let _ = dec.decode(part);
        }
    }
});

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

/// Cut `data` into 2 or 3 non-empty parts. The first byte picks the number of
/// parts and the cut points; the rest is the material that gets cut.
fn split(data: &[u8]) -> Option<Vec<&[u8]>> {
    let (&ctl, body) = data.split_first()?;
    let n = body.len();
    if n < 2 {
        return None;
    }
    if ctl & 1 == 1 && n >= 3 {
        let a = 1 + ((ctl as usize) >> 1) % (n - 2);
        let b = a + 1 + (body[0] as usize) % (n - a - 1);
        Some(vec![&body[..a], &body[a..b], &body[b..]])
    } else {
        let a = 1 + ((ctl as usize) >> 1) % (n - 1);
        Some(vec![&body[..a], &body[a..]])
    }
}
