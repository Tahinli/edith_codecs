//! M2 witness: key-frame decode is sample-exact against ffmpeg/libvpx
//! (`ffmpeg -v error -i in.ivf -f rawvideo -pix_fmt yuv420p -`).

mod ivf;

use ec_vp8::decode::{Decoder, Picture};

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

/// Decode every frame of an IVF with our decoder and compare each
/// frame's planes against ffmpeg's byte stream.
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
    assert_eq!(
        all.len() % frame_len,
        0,
        "{name}: ffmpeg output {}/{} not whole frames",
        all.len(),
        frame_len
    );
    let count = all.len() / frame_len;
    assert_eq!(frames.len(), count, "{name}: frame count vs ffmpeg");

    let mut dec = Decoder::new();
    let (cw, ch) = (usize::from(w + 1) / 2, usize::from(h + 1) / 2);
    for (i, f) in frames.iter().enumerate() {
        let pic: Picture = dec
            .decode(&f.data)
            .expect("decode succeeds")
            .expect("shown");
        assert_eq!(pic.width, w);
        assert_eq!(pic.height, h);
        let mut pos = i * frame_len;
        let exp_y = raw(&mut pos, &all, usize::from(w) * usize::from(h));
        let exp_u = raw(&mut pos, &all, cw * ch);
        let exp_v = raw(&mut pos, &all, cw * ch);
        assert_eq!(pic.y, exp_y, "{name} frame {i}: luma mismatch");
        assert_eq!(pic.u, exp_u, "{name} frame {i}: U mismatch");
        assert_eq!(pic.v, exp_v, "{name} frame {i}: V mismatch");
    }
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
