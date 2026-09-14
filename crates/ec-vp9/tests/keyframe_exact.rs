//! M1 witness: key-frame decode is byte-exact against ffmpeg's libvpx
//! (`ffmpeg -v error -i in.ivf -f rawvideo -pix_fmt yuv420p -`), plus
//! the lane's named refusals.

mod ivf;

use ec_vp9::decode::Decoder;

/// Generate the small all-keyframe fixture with libvpx if it is absent
/// (2 keyframes of 320x240 testsrc2, realtime cpu-used 4).
fn ensure_fixture(path: &std::path::Path) {
    if path.exists() {
        return;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    let st = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f", "lavfi",
            "-i", "testsrc2=size=320x240:rate=30:duration=1",
            "-pix_fmt", "yuv420p",
            "-c:v", "libvpx-vp9",
            "-g", "1",
            "-cpu-used", "4",
            "-deadline", "realtime",
            "-frames:v", "2",
        ])
        .arg(path)
        .output()
        .expect("ffmpeg with libvpx-vp9 must be on PATH for the witnesses");
    assert!(st.status.success(), "fixture generation failed: {st:?}");
}

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

/// Decode every frame of an IVF with our decoder and compare each SHOWN
/// frame's planes against ffmpeg's byte stream.
fn compare(path: &std::path::Path) {
    let bytes = std::fs::read(path).unwrap();
    let (_fourcc, _w, _h, frames) = ivf::parse_ivf(&bytes);
    let reference = ffmpeg_raw_yuv(path);

    let mut decoder = Decoder::new();
    let mut ref_pos = 0usize;
    let mut shown = 0usize;
    for frame in &frames {
        let pic = decoder.decode(&frame.data).expect("decode must succeed");
        if let Some(pic) = pic {
            let (w, h) = (pic.width as usize, pic.height as usize);
            assert_eq!(w, 320, "fixture width");
            assert_eq!(h, 240, "fixture height");
            let uv = (w / 2) * (h / 2);
            let fr = &reference[ref_pos..ref_pos + w * h + 2 * uv];
            ref_pos += w * h + 2 * uv;
            let want_y = &fr[..w * h];
            let want_u = &fr[w * h..w * h + uv];
            let want_v = &fr[w * h + uv..];
            assert_eq!(pic.y.len(), want_y.len());
            assert_eq!(pic.u.len(), want_u.len());
            assert_eq!(pic.v.len(), want_v.len());
            for (i, (&a, &b)) in pic.y.iter().zip(want_y).enumerate() {
                assert_eq!(a, b, "Y plane mismatch at pixel ({}, {}) frame {shown}", i % w, i / w);
            }
            for (i, (&a, &b)) in pic.u.iter().zip(want_u).enumerate() {
                assert_eq!(a, b, "U plane mismatch at ({}, {}) frame {shown}", i % (w / 2), i / (w / 2));
            }
            for (i, (&a, &b)) in pic.v.iter().zip(want_v).enumerate() {
                assert_eq!(a, b, "V plane mismatch at ({}, {}) frame {shown}", i % (w / 2), i / (w / 2));
            }
            shown += 1;
        }
    }
    assert_eq!(shown, 2, "both keyframes are shown");
}

#[test]
fn keyframes_match_ffmpeg() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vp9");
    let path = dir.join("key-320.ivf");
    ensure_fixture(&path);
    compare(&path);
}

#[test]
fn inter_is_named_unsupported() {
    let Some(dir) = ivf::fixture_dir() else { return };
    // The altref clip contains hidden (inter) frames.
    let path = dir.join("vp9-superframe-altref.ivf");
    let bytes = std::fs::read(&path).unwrap();
    let (_, _, _, frames) = ivf::parse_ivf(&bytes);
    let mut decoder = Decoder::new();
    let mut saw_inter = false;
    let mut saw_named = false;
    for frame in &frames {
        match decoder.decode(&frame.data) {
            Ok(_) => {}
            Err(e) => {
                saw_inter = true;
                let msg = format!("{e}");
                if msg.contains("vp9 inter") {
                    saw_named = true;
                }
                break;
            }
        }
    }
    assert!(saw_inter, "the altref fixture must contain a refused frame");
    assert!(saw_named, "the refusal must name 'vp9 inter'");
}

#[test]
fn profile1_444_is_named_unsupported() {
    let Some(dir) = ivf::fixture_dir() else { return };
    let path = dir.join("vp9-profile1-444.ivf");
    let bytes = std::fs::read(&path).unwrap();
    let (_, _, _, frames) = ivf::parse_ivf(&bytes);
    let mut decoder = Decoder::new();
    let err = decoder
        .decode(&frames[0].data)
        .expect_err("profile 1 4:4:4 must be refused");
    assert!(
        format!("{err}").contains("vp9 profile 1"),
        "the refusal must name the profile: {err}"
    );
}
