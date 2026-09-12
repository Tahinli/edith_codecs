//! M2 witness: key-frame decode is sample-exact against ffmpeg/libvpx
//! (`ffmpeg -v error -i in.ivf -f rawvideo -pix_fmt yuv420p -`).

mod ivf;

use ec_vp8::decode::Decoder;

/// ffmpeg's decoded rawvideo byte stream for the same file.
fn ffmpeg_raw_yuv(path: &std::path::Path) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg must be on PATH for the witnesses");
    assert!(out.status.success(), "ffmpeg failed on {path:?}");
    out.stdout
}

fn raw<'a>(pos: &mut usize, data: &'a [u8], len: usize) -> &'a [u8] {
    let s = &data[*pos..*pos + len];
    *pos += len;
    s
}

/// Decode every frame of an IVF with our decoder and compare each SHOWN
/// frame's planes against ffmpeg's byte stream (ffmpeg skips hidden
/// `show_frame == 0` frames exactly like a player).
fn compare(name: &str, skip_if_missing: bool) {
    let Some(dir) = ivf::fixture_dir() else {
        panic!("fixtures missing; run scripts/gen_vp8_fixtures.sh");
    };
    let path = dir.join(name);
    if skip_if_missing && !path.exists() {
        panic!("{name} missing (regenerate fixtures)");
    }
    let bytes = std::fs::read(&path).unwrap();
    let (_, w, h, frames) = ivf::parse_ivf(&bytes);
    let all = ffmpeg_raw_yuv(&path);
    let frame_len = usize::from(w) * usize::from(h) * 3 / 2;
    let shown: Vec<&ivf::IvfFrame> =
        frames.iter().filter(|f| ivf::show_frame(&f.data)).collect();
    assert_eq!(
        all.len(),
        shown.len() * frame_len,
        "{name}: ffmpeg output {}/{} not {} whole shown frames",
        all.len(),
        frame_len,
        shown.len()
    );

    let mut dec = Decoder::new();
    let (cw, ch) = (usize::from(w + 1) / 2, usize::from(h + 1) / 2);
    let mut shown_idx = 0;
    for (i, f) in frames.iter().enumerate() {
        match dec.decode(&f.data).expect("decode succeeds") {
            Some(pic) => {
                assert!(ivf::show_frame(&f.data), "{name} frame {i}: picture for hidden frame");
                assert_eq!(pic.width, w);
                assert_eq!(pic.height, h);
                let mut pos = shown_idx * frame_len;
                shown_idx += 1;
                let exp_y = raw(&mut pos, &all, usize::from(w) * usize::from(h));
                let exp_u = raw(&mut pos, &all, cw * ch);
                let exp_v = raw(&mut pos, &all, cw * ch);
                assert_eq!(pic.y, exp_y, "{name} frame {i}: luma mismatch");
                assert_eq!(pic.u, exp_u, "{name} frame {i}: U mismatch");
                assert_eq!(pic.v, exp_v, "{name} frame {i}: V mismatch");
            }
            None => assert!(
                !ivf::show_frame(&f.data),
                "{name} frame {i}: shown frame produced no picture"
            ),
        }
    }
    assert_eq!(shown_idx, shown.len(), "{name}: shown picture count");
}

#[test]
fn keyframes_match_ffmpeg() {
    for (name, _, _) in [
        ("kf-160x96-q20.ivf", 160, 96),
        ("kf-160x96-q40.ivf", 160, 96),
        ("kf-76x52-q30.ivf", 76, 52),
        ("kf-32x16-q35.ivf", 32, 16),
        ("kf-172x144-q25.ivf", 172, 144),
        ("kf-96x80-q63.ivf", 96, 80),
    ] {
        compare(name, false);
    }
}

#[test]
fn multi_partition_keyframe_matches_ffmpeg() {
    compare("mparts-160x96.ivf", false);
}

/// M3 witness: full inter-frame streams — simple-filter GOP, multi-token-
/// partition, hidden-altref and the OBS screen-content clip — decode
/// byte-exact against ffmpeg, every frame. The altref vector must
/// actually CONTAIN hidden frames (census the tags; a shown-only stream
/// would silently stop exercising the hidden-frame contract).
#[test]
fn interframe_streams_match_ffmpeg() {
    for name in [
        "gop-160x96.ivf",
        "clip-obs-320x192.ivf",
        "altref-160x96.ivf",
    ] {
        if name == "altref-160x96.ivf" {
            let dir = ivf::fixture_dir().expect("fixtures dir");
            let bytes = std::fs::read(dir.join(name)).unwrap();
            let hidden = census_hidden(&bytes);
            assert!(
                hidden >= 6,
                "{name}: only {hidden} hidden frames — regenerate with \
                 scripts/gen_vp8_fixtures.sh (--auto-alt-ref=1 --passes=2)"
            );
        }
        compare(name, false);
    }
}

/// Number of hidden (`show_frame == 0`) frames in an IVF stream.
fn census_hidden(bytes: &[u8]) -> usize {
    let (_, _, _, frames) = ivf::parse_ivf(bytes);
    frames.iter().filter(|f| !ivf::show_frame(&f.data)).count()
}

#[test]
fn vp8_in_webp_still_matches_ffmpeg() {
    let Some(dir) = ivf::fixture_dir() else {
        panic!("fixtures missing; run scripts/gen_vp8_fixtures.sh");
    };
    let bytes = std::fs::read(dir.join("still.webp")).unwrap();
    let vp8 = ivf::webp_vp8_chunk(&bytes);
    let reference = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(dir.join("still.webp"))
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .unwrap();
    assert!(reference.status.success());
    let mut dec = Decoder::new();
    let pic = dec.decode(&vp8).expect("decode").expect("shown");
    assert_eq!(
        pic.y.len() + pic.u.len() + pic.v.len(),
        reference.stdout.len()
    );
    let mut exp = &reference.stdout[..];
    assert_eq!(pic.y, &exp[..pic.y.len()]);
    exp = &exp[pic.y.len()..];
    assert_eq!(pic.u, &exp[..pic.u.len()]);
    exp = &exp[pic.u.len()..];
    assert_eq!(pic.v, exp);
}
