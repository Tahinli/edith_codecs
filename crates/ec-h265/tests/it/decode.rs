//! Round-trip gate for `ec_h265::decode`: our own encoder's IDR stream,
//! decoded back, must equal the encoder's own reconstruction byte-for-byte —
//! and, separately, ffmpeg's decode of the same stream (the two-oracle rule).

use crate::common::test_frame;
use ec_h265::decode::decode_idr_au;
use ec_h265::encoder::{Encoder, EncoderConfig, RateControl};

fn roundtrip_at(width: u32, height: u32) {
    let frame = test_frame(width, height, 7);
    let cfg = EncoderConfig {
        rate_control: RateControl::ConstantQp(27),
        threads: 1,
        keep_recon: true,
        ..EncoderConfig::new(width, height)
    };
    let enc = Encoder::new(cfg).expect("encoder config");
    let picture = enc.encode_idr(&frame).expect("encode");
    let recon = picture.recon.expect("keep_recon was on");

    let decoded = decode_idr_au(&picture.au).unwrap_or_else(|e| {
        panic!("decode {width}x{height}: {e}");
    });

    let sps = enc.sps();
    assert_eq!(decoded.width, sps.pic_width_in_luma_samples as usize);

    // Compare over the displayed region only: `recon` is cropped by the
    // conformance window, `decoded` is the full coded (padded) picture.
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    for row in 0..h {
        let want = recon.planes[0].row(row, w).unwrap();
        let got: Vec<u8> = decoded.y[row * decoded.width..row * decoded.width + w]
            .iter()
            .map(|&v| v as u8)
            .collect();
        assert_eq!(&got[..], want, "{width}x{height} luma row {row} diverges");
    }
    for row in 0..ch {
        let want_cb = recon.planes[1].row(row, cw).unwrap();
        let got_cb: Vec<u8> = decoded.cb[row * decoded.width / 2..row * decoded.width / 2 + cw]
            .iter()
            .map(|&v| v as u8)
            .collect();
        assert_eq!(
            &got_cb[..],
            want_cb,
            "{width}x{height} Cb row {row} diverges"
        );
        let want_cr = recon.planes[2].row(row, cw).unwrap();
        let got_cr: Vec<u8> = decoded.cr[row * decoded.width / 2..row * decoded.width / 2 + cw]
            .iter()
            .map(|&v| v as u8)
            .collect();
        assert_eq!(
            &got_cr[..],
            want_cr,
            "{width}x{height} Cr row {row} diverges"
        );
    }
}

/// The sizes the round trip runs at, with the old test name of each: one CTB, several
/// CTBs, and odd sizes that need padding.
const CASES: &[(&str, u32, u32)] = &[
    (
        "decode_matches_the_encoders_own_reconstruction_at_one_ctb",
        32,
        32,
    ),
    (
        "decode_matches_the_encoders_own_reconstruction_over_several_ctbs",
        96,
        64,
    ),
    (
        "decode_matches_the_encoders_own_reconstruction_at_an_odd_padded_size",
        70,
        50,
    ),
    (
        "decode_matches_the_encoders_own_reconstruction_at_another_odd_size",
        150,
        94,
    ),
    (
        "decode_matches_the_encoders_own_reconstruction_at_an_extreme_odd_size",
        41,
        33,
    ),
];

#[test]
fn decode_matches_the_encoders_own_reconstruction() {
    let mut failures = Vec::new();
    for &(name, width, height) in CASES {
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| roundtrip_at(width, height)));
        if outcome.is_err() {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}
