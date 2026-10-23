//! The process-wide GPU reset epoch, end to end on a real VA display.
//!
//! One test in its own binary: the reset state is global to the process, so
//! sharing a binary with other GPU tests would condemn them.

use std::rc::Rc;
use std::time::Duration;

use ec_hw::{
    Codec, Decoder, EncCodec, Encoder, EncoderConfig, FrameMetadata, I420, RateControlMode,
};
use ec_va::Display;

const W: u32 = 128;
const H: u32 = 96;

fn picture(t: u32) -> I420 {
    let (w, h) = (W as usize, H as usize);
    let mut f = I420 {
        y: vec![0; w * h],
        u: vec![128; w * h / 4],
        v: vec![128; w * h / 4],
        width: W,
        height: H,
    };
    for y in 0..h {
        for x in 0..w {
            f.y[y * w + x] = ((x * 3 + y + t as usize * 7) % 250) as u8;
        }
    }
    f
}

fn units(display: &Rc<Display>, n: u32) -> Vec<Vec<u8>> {
    let mut config = EncoderConfig::new(EncCodec::H264, W, H);
    config.framerate = (30, 1);
    config.gop_size = 30;
    config.rate_control = RateControlMode::ConstantQp { qp: 30 };
    let mut encoder = Encoder::new(display, config).expect("encoder");
    (0..n)
        .map(|t| {
            encoder
                .encode(
                    &picture(t),
                    FrameMetadata {
                        timestamp: i64::from(t),
                        force_keyframe: false,
                    },
                )
                .expect("encode")
                .data
        })
        .collect()
}

#[test]
fn a_reset_condemns_live_sessions_and_new_ones_work_after_the_grace_period() {
    let display = match ec_va::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipped: no VA display ({e})");
            return;
        }
    };
    let units = units(&display, 6);

    let mut a = Decoder::new(&display, Codec::H264).expect("a");
    let mut b = Decoder::new(&display, Codec::H264).expect("b");
    for d in [&mut a, &mut b] {
        d.decode(&units[0], 0).expect("first picture");
        d.decode(&units[1], 1).expect("second picture");
    }
    a.flush().expect("flush");
    let early = a.next_frame().expect("a frame before the reset");
    assert!(early.to_i420().is_ok());
    assert_eq!(ec_hw::gpu_reset_epoch(), 0);
    assert!(!ec_hw::gpu_recently_reset());

    ec_hw::report_gpu_reset();
    assert_eq!(ec_hw::gpu_reset_epoch(), 1);
    assert!(ec_hw::gpu_recently_reset());

    // Both live sessions are dead, and say so distinctly.
    for d in [&mut a, &mut b] {
        let err = d.decode(&units[2], 2).expect_err("session must be dead");
        assert!(err.is_hw_reset(), "{err}");
        let err = d.decode(&units[3], 3).expect_err("and stay dead");
        assert!(err.is_hw_reset(), "{err}");
    }
    // A frame decoded before the reset is not read back from a lost context.
    let err = early.to_i420().expect_err("stale frame");
    assert!(err.is_hw_reset(), "{err}");

    // A session created inside the grace period is refused.
    let mut c = Decoder::new(&display, Codec::H264).expect("c");
    let err = c.decode(&units[0], 0).expect_err("grace period");
    assert!(err.is_hw_reset(), "{err}");

    // After the grace period a fresh session decodes normally.
    std::thread::sleep(Duration::from_millis(3200));
    assert!(!ec_hw::gpu_recently_reset());
    let mut d = Decoder::new(&display, Codec::H264).expect("d");
    d.decode(&units[0], 0).expect("fresh session");
    d.decode(&units[1], 1).expect("fresh session");
    d.flush().expect("flush");
    let frame = d.next_frame().expect("frame");
    assert!(frame.to_i420().is_ok());
}
