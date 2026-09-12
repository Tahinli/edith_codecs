//! M1 witness: the header parser handles real libvpx-produced key frames
//! (and a VP8-in-WebP still) with no desync and the right dimensions.

mod ivf;

use ec_vp8::PersistedState;
use ec_vp8::frame::{FrameTag, FrameType, mb_geometry, parse_keyframe_dims};
use ec_vp8::header::FrameHeader;

const FIXTURES: &[(&str, u16, u16)] = &[
    ("kf-160x96-q20.ivf", 160, 96),
    ("kf-160x96-q40.ivf", 160, 96),
    ("kf-76x52-q30.ivf", 76, 52),
    ("kf-32x16-q35.ivf", 32, 16),
    ("kf-172x144-q25.ivf", 172, 144),
    ("kf-96x80-q63.ivf", 96, 80),
    ("mparts-160x96.ivf", 160, 96),
];

#[test]
fn parses_real_key_frame_headers() {
    let Some(dir) = ivf::fixture_dir() else {
        panic!("fixtures missing; run scripts/gen_vp8_fixtures.sh");
    };
    for (name, width, height) in FIXTURES {
        let bytes = std::fs::read(dir.join(name)).unwrap();
        let (fourcc, w, h, frames) = ivf::parse_ivf(&bytes);
        assert_eq!(fourcc, "VP80", "{name}");
        assert_eq!((w, h), (*width, *height), "{name} IVF dimensions");
        assert!(!frames.is_empty(), "{name} has no frames");

        let first = &frames[0].data;
        let tag = FrameTag::parse(first).unwrap();
        assert_eq!(tag.frame_type, FrameType::Key, "{name} first frame");
        assert!(tag.show_frame, "{name} first frame must be shown");
        assert_eq!(tag.version, 0, "{name}: default profile");
        let kf = parse_keyframe_dims(first).unwrap();
        assert_eq!((kf.width, kf.height), (*width, *height));
        assert_eq!((kf.h_scale, kf.v_scale), (0, 0), "{name} unscaled");

        let mut state = PersistedState::default();
        let (header, part0) = FrameHeader::parse(first, &mut state).unwrap();
        assert_eq!(part0.len(), tag.first_part_size as usize);
        assert!(header.dims.is_some());
        assert_eq!(header.color_space, 0, "{name}: YUV colour space");
        assert_eq!(
            header.partition_sizes.len(),
            1usize << header.log2_partitions,
            "{name} partition count vs log2 field"
        );
        let n_parts = header.partition_sizes.len();
        assert_eq!(
            header.token_data_offset
                + 3 * (n_parts - 1)
                + header.partition_sizes.iter().sum::<u32>() as usize,
            first.len(),
            "{name}: partition sizes must tile the frame exactly"
        );
        // The header partition must decode without reading past its end.
        assert!(
            header.quant.yac_qi <= 127,
            "{name}: qindex sanity {}",
            header.quant.yac_qi
        );
        let (mb_cols, mb_rows) = mb_geometry(kf.width, kf.height);
        assert_eq!(
            (mb_cols, mb_rows),
            (
                (usize::from(*width) + 15) / 16,
                (usize::from(*height) + 15) / 16
            )
        );
    }
}

#[test]
fn parses_multi_partition_header() {
    let Some(dir) = ivf::fixture_dir() else {
        panic!("fixtures missing; run scripts/gen_vp8_fixtures.sh");
    };
    let bytes = std::fs::read(dir.join("mparts-160x96.ivf")).unwrap();
    let (_, _, _, frames) = ivf::parse_ivf(&bytes);
    let mut state = PersistedState::default();
    let (header, _) = FrameHeader::parse(&frames[0].data, &mut state).unwrap();
    // vpxenc was asked for --token-parts=3 => 8 token partitions.
    assert_eq!(header.log2_partitions, 3, "8 token partitions expected");
    assert_eq!(header.partition_sizes.len(), 8);
    assert!(header.partition_sizes.iter().all(|&s| s > 0));
}

#[test]
fn parses_vp8_in_webp_still() {
    let Some(dir) = ivf::fixture_dir() else {
        panic!("fixtures missing; run scripts/gen_vp8_fixtures.sh");
    };
    let Ok(bytes) = std::fs::read(dir.join("still.webp")) else {
        panic!("still.webp missing (libwebp encoder unavailable at generation time)");
    };
    let vp8 = ivf::webp_vp8_chunk(&bytes);
    let tag = FrameTag::parse(&vp8).unwrap();
    assert_eq!(tag.frame_type, FrameType::Key);
    let kf = parse_keyframe_dims(&vp8).unwrap();
    assert_eq!((kf.width, kf.height), (96, 80));
    let mut state = PersistedState::default();
    let (header, _) = FrameHeader::parse(&vp8, &mut state).unwrap();
    assert!(header.dims.is_some());
}

#[test]
fn gop_key_frame_parses_and_coexists_with_inter() {
    let Some(dir) = ivf::fixture_dir() else {
        panic!("fixtures missing; run scripts/gen_vp8_fixtures.sh");
    };
    let bytes = std::fs::read(dir.join("gop-160x96.ivf")).unwrap();
    let (_, _, _, frames) = ivf::parse_ivf(&bytes);
    assert!(frames.len() >= 30);

    let mut state = PersistedState::default();
    let (kf_header, _) = FrameHeader::parse(&frames[0].data, &mut state).unwrap();
    assert_eq!(kf_header.frame_type(), FrameType::Key);
    assert!(kf_header.refresh.refresh_last && kf_header.refresh.refresh_gf);

    // First inter frame: header fields come from the same first partition,
    // and the persisted entropy state has absorbed the key frame's updates.
    let mut inter_seen = false;
    for f in &frames[1..] {
        let tag = FrameTag::parse(&f.data).unwrap();
        if tag.frame_type == FrameType::Key {
            continue;
        }
        let (header, _) = FrameHeader::parse(&f.data, &mut state).unwrap();
        assert_eq!(header.frame_type(), FrameType::Inter);
        assert!(header.dims.is_none(), "inter frames carry no dimensions");
        inter_seen = true;
        break;
    }
    assert!(inter_seen, "gop fixture has no inter frame");
}
