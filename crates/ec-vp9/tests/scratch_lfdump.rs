//! [scratch] Loop-filter diff map: decode key-320 frame 0 with and without
//! the loop filter (EC_VP9_SKIP_LF=1), print where we disagree with ffmpeg.

mod ivf;

use ec_vp9::decode::Decoder;

fn ffmpeg_raw_yuv(path: &std::path::Path) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg on PATH");
    assert!(out.status.success());
    out.stdout
}

#[test]
fn dump_lf_diff_map() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vp9/key-320.ivf");
    let bytes = std::fs::read(&path).unwrap();
    let (_f, _w, _h, frames) = ivf::parse_ivf(&bytes);
    let reference = ffmpeg_raw_yuv(&path);

    let (w, h) = (320usize, 240usize);
    let ysz = w * h;

    // Pre-LF run.
    // SAFETY: single-threaded test process; no other env readers run here.
    unsafe { std::env::set_var("EC_VP9_SKIP_LF", "1") };
    let mut dec = Decoder::new();
    let pic_pre = dec.decode(&frames[0].data).expect("decode").expect("shown");
    std::fs::write("/tmp/y_pre_ours.raw", &pic_pre.y[..ysz]).unwrap();
    unsafe { std::env::remove_var("EC_VP9_SKIP_LF") };

    // Post-LF run (fresh decoder).
    let mut dec = Decoder::new();
    let pic_post = dec.decode(&frames[0].data).expect("decode").expect("shown");
    std::fs::write("/tmp/y_post_ours.raw", &pic_post.y[..ysz]).unwrap();

    let ff = &reference[..ysz];
    let pre = &pic_pre.y[..ysz];
    let post = &pic_post.y[..ysz];

    let pre_diffs: Vec<usize> =
        (0..ysz).filter(|&i| pre[i] != ff[i]).collect();
    let post_diffs: Vec<usize> =
        (0..ysz).filter(|&i| post[i] != ff[i]).collect();
    let lf_changed_ours: Vec<usize> =
        (0..ysz).filter(|&i| pre[i] != post[i]).collect();
    println!("pre-LF diffs vs ffmpeg : {}", pre_diffs.len());
    println!("post-LF diffs vs ffmpeg: {}", post_diffs.len());
    println!("pixels our LF changed  : {}", lf_changed_ours.len());
    for &i in post_diffs.iter().take(30) {
        println!(
            "  POST diff ({:3},{:3}) ours {:3} ff {:3}  (pre-ours {:3})",
            i % w,
            i / w,
            post[i],
            ff[i],
            pre[i]
        );
    }
    for &i in pre_diffs.iter().take(10) {
        println!(
            "  PRE  diff ({:3},{:3}) ours {:3} ff {:3}",
            i % w,
            i / w,
            pre[i],
            ff[i]
        );
    }

    // ASCII map of the top-left 48x32: mark post-LF diffs (#), pre-LF diffs
    // (?), pixels only our LF touched (o).
    println!("map rows 0..32, cols 0..48 (#=post diff, ?=pre-only, o=our-LF-changed)");
    for y in 0..32 {
        let mut line = String::new();
        for x in 0..48 {
            let i = y * w + x;
            if post_diffs.binary_search(&i).is_ok() {
                line.push('#');
            } else if pre_diffs.binary_search(&i).is_ok() {
                line.push('?');
            } else if lf_changed_ours.binary_search(&i).is_ok() {
                line.push('o');
            } else {
                line.push('.');
            }
        }
        println!("{line}");
    }
    // Column/row histograms of post-LF diffs (first 24 of each).
    let mut cols = vec![0usize; w];
    let mut rows = vec![0usize; h];
    for &i in &post_diffs {
        cols[i % w] += 1;
        rows[i / w] += 1;
    }
    println!(
        "diff columns>0: {:?}",
        (0..w).filter(|&c| cols[c] > 0).take(24).collect::<Vec<_>>()
    );
    println!(
        "diff rows>0   : {:?}",
        (0..h).filter(|&r| rows[r] > 0).take(24).collect::<Vec<_>>()
    );
}
