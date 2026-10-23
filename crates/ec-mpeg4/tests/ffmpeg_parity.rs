//! Byte-exact parity with ffmpeg's MPEG-4 Part 2 / H.263 decoder on streams ffmpeg encodes at test
//! time (`ffmpeg -c:v mpeg4`, `libxvid`, `h263`). Skipped without ffmpeg unless `EC_REQUIRE_FIXTURES`
//! is set; the libxvid cases (GMC, N-VOPs, XviD IDCT) also skip when the encoder is not built in.

mod common;

use common::*;

macro_rules! parity {
    ($name:ident, $size:expr, $frames:expr, [$($arg:expr),* $(,)?]) => {
        #[test]
        fn $name() {
            if !gate() {
                return;
            }
            let n = parity_es(stringify!($name), $size, $frames, &[$($arg),*], "m4v").unwrap();
            assert!(n >= $frames - 2, "decoded only {n} of {} pictures", $frames);
        }
    };
}

// Simple profile: I/P, H.263 quantisation.
parity!(
    simple_q2,
    "176x144",
    30,
    ["-c:v", "mpeg4", "-qscale:v", "2", "-g", "12"]
);
parity!(
    simple_q8,
    "176x144",
    30,
    ["-c:v", "mpeg4", "-qscale:v", "8", "-g", "30"]
);
parity!(
    simple_q31,
    "176x144",
    30,
    ["-c:v", "mpeg4", "-qscale:v", "31", "-g", "30"]
);
parity!(
    mv4,
    "176x144",
    30,
    ["-c:v", "mpeg4", "-qscale:v", "4", "-flags", "+mv4"]
);
parity!(
    aic,
    "176x144",
    30,
    ["-c:v", "mpeg4", "-qscale:v", "4", "-flags", "+aic"]
);
parity!(
    cbr_cif,
    "352x288",
    60,
    ["-c:v", "mpeg4", "-b:v", "300k", "-g", "50"]
);
// Advanced Simple profile tools.
parity!(
    bframes,
    "176x144",
    30,
    ["-c:v", "mpeg4", "-qscale:v", "4", "-bf", "2", "-g", "12"]
);
parity!(
    bframes_1,
    "176x144",
    30,
    ["-c:v", "mpeg4", "-qscale:v", "4", "-bf", "1"]
);
parity!(
    qpel,
    "176x144",
    30,
    ["-c:v", "mpeg4", "-qscale:v", "4", "-flags", "+qpel"]
);
parity!(
    qpel_bf_mv4,
    "176x144",
    30,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "4",
        "-bf",
        "2",
        "-flags",
        "+qpel+mv4"
    ]
);
parity!(
    interlaced_dct,
    "176x144",
    30,
    ["-c:v", "mpeg4", "-qscale:v", "4", "-flags", "+ildct"]
);
parity!(
    interlaced_me_bf,
    "176x144",
    30,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "4",
        "-bf",
        "2",
        "-flags",
        "+ildct+ilme"
    ]
);
parity!(
    interlaced_direct_q31,
    "176x144",
    30,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "31",
        "-bf",
        "2",
        "-flags",
        "+ildct+ilme"
    ]
);
parity!(
    interlaced_everything,
    "176x144",
    30,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "3",
        "-bf",
        "2",
        "-flags",
        "+ildct+ilme+qpel+mv4"
    ]
);
parity!(
    mpeg_quant,
    "176x144",
    30,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "4",
        "-bf",
        "2",
        "-mpeg_quant",
        "1"
    ]
);
parity!(
    mpeg_quant_q31_qpel,
    "176x144",
    30,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "31",
        "-mpeg_quant",
        "1",
        "-flags",
        "+qpel"
    ]
);
parity!(
    resync_packets,
    "176x144",
    30,
    ["-c:v", "mpeg4", "-qscale:v", "4", "-bf", "2", "-ps", "400"]
);
parity!(
    resync_small_packets,
    "352x288",
    20,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "6",
        "-ps",
        "100",
        "-flags",
        "+mv4"
    ]
);
parity!(
    all_tools,
    "176x144",
    30,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "5",
        "-bf",
        "2",
        "-flags",
        "+ildct+ilme+qpel+mv4+aic",
        "-mpeg_quant",
        "1",
        "-ps",
        "300"
    ]
);
// Field-MV B-frames across video-packet boundaries: ffmpeg clears only last_mv[dir][0] at a resync.
parity!(
    interlaced_resync_bf,
    "176x144",
    30,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "5",
        "-bf",
        "2",
        "-flags",
        "+ildct+ilme+qpel+mv4",
        "-ps",
        "100"
    ]
);
parity!(
    interlaced_resync_bf_tiny_packets,
    "352x288",
    16,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "4",
        "-bf",
        "2",
        "-flags",
        "+ildct+ilme",
        "-ps",
        "60"
    ]
);
// Picture sizes that are not a macroblock multiple (cropped output).
parity!(
    odd_size,
    "170x130",
    20,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "4",
        "-bf",
        "2",
        "-flags",
        "+qpel+mv4"
    ]
);
parity!(
    tiny,
    "32x32",
    20,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "4",
        "-bf",
        "2",
        "-flags",
        "+qpel+mv4"
    ]
);
parity!(
    wide,
    "640x64",
    12,
    [
        "-c:v",
        "mpeg4",
        "-qscale:v",
        "4",
        "-bf",
        "2",
        "-flags",
        "+qpel+mv4+ildct+ilme"
    ]
);

#[test]
fn hd_720p_with_everything() {
    if !gate() {
        return;
    }
    let n = parity_es(
        "hd720",
        "1280x720",
        8,
        &[
            "-c:v",
            "mpeg4",
            "-qscale:v",
            "4",
            "-bf",
            "2",
            "-flags",
            "+qpel+mv4+ildct+ilme",
        ],
        "m4v",
    )
    .unwrap();
    assert!(n >= 6);
}

// H.263 baseline (short video header).
#[test]
fn h263_formats() {
    if !gate() {
        return;
    }
    for (size, q, gop) in [
        ("128x96", "4", "8"),
        ("176x144", "4", "10"),
        ("352x288", "4", "10"),
        ("352x288", "12", "30"),
        ("704x576", "6", "6"),
    ] {
        let tag = format!("h263_{size}_{q}");
        let n = parity_es(
            &tag,
            size,
            20,
            &["-c:v", "h263", "-qscale:v", q, "-g", gop],
            "h263",
        )
        .unwrap_or_else(|e| panic!("{tag}: {e}"));
        assert_eq!(n, 20, "{tag}");
    }
}

#[test]
fn h263_with_gob_headers() {
    if !gate() {
        return;
    }
    // `-ps` makes ffmpeg's H.263 encoder start a GOB (with its header) every n macroblocks.
    let n = parity_es(
        "h263_gob",
        "352x288",
        20,
        &["-c:v", "h263", "-qscale:v", "6", "-ps", "300"],
        "h263",
    )
    .unwrap();
    assert_eq!(n, 20);
}

// libxvid: GMC with warp points, N-VOPs, the XviD IDCT and XviD's qpel/bframe habits.
fn libxvid(tag: &str, size: &str, frames: u32, extra: &[&str]) {
    if !gate() {
        return;
    }
    if !have_encoder("libxvid") {
        eprintln!("skipped {tag}: ffmpeg has no libxvid encoder");
        return;
    }
    let mut enc = vec!["-c:v", "libxvid"];
    enc.extend_from_slice(extra);
    let n = parity_es(tag, size, frames, &enc, "m4v").unwrap_or_else(|e| panic!("{tag}: {e}"));
    assert!(n >= frames as usize - 3);
}

#[test]
fn xvid_gmc() {
    libxvid(
        "xvid_gmc",
        "176x144",
        30,
        &["-qscale:v", "4", "-bf", "2", "-gmc", "1", "-g", "12"],
    );
}

#[test]
fn xvid_gmc_qpel_mv4() {
    libxvid(
        "xvid_gmc_qpel",
        "352x288",
        40,
        &[
            "-qscale:v",
            "4",
            "-bf",
            "2",
            "-gmc",
            "1",
            "-flags",
            "+qpel+mv4",
            "-g",
            "12",
        ],
    );
}

#[test]
fn xvid_gmc_no_bframes_mpeg_quant() {
    libxvid(
        "xvid_gmc_mq",
        "320x240",
        40,
        &[
            "-qscale:v",
            "3",
            "-bf",
            "0",
            "-gmc",
            "1",
            "-flags",
            "+qpel",
            "-mpeg_quant",
            "1",
        ],
    );
}

#[test]
fn xvid_interlaced() {
    libxvid(
        "xvid_ilace",
        "176x144",
        30,
        &["-qscale:v", "4", "-bf", "2", "-flags", "+ildct+ilme"],
    );
}

#[test]
fn xvid_mpeg_quant_qpel() {
    libxvid(
        "xvid_mq",
        "176x144",
        30,
        &[
            "-qscale:v",
            "4",
            "-bf",
            "2",
            "-flags",
            "+qpel",
            "-mpeg_quant",
            "1",
        ],
    );
}
