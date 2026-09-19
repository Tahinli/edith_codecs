mod ivf;
use ec_vp9::decode::Decoder;
#[test]
fn sweep() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); p.pop();
    p = p.join("fixtures/vp9/lossless-64.ivf");
    let bytes = std::fs::read(&p).unwrap();
    let (_, w, h, frames) = ivf::parse_ivf(&bytes);
    let refout = std::process::Command::new("ffmpeg")
        .args(["-v","error","-i", p.to_str().unwrap(), "-frames:v","1","-f","rawvideo","-pix_fmt","yuv420p","-"])
        .output().unwrap().stdout;
    let ylen = w as usize * h as usize;
    let mut best = (0usize, 0usize);
    for t in (100..260).step_by(1) {
        unsafe { std::env::set_var("EC_VP9_FORCE_TAIL", t.to_string()); }
        let mut d = Decoder::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.decode(&frames[0].data)));
        let Ok(Ok(Some(pic))) = r else { continue };
        let m = (0..ylen).filter(|&i| pic.y[i] as u8 == refout[i]).count();
        if m > best.0 { best = (m, t); }
    }
    unsafe { std::env::remove_var("EC_VP9_FORCE_TAIL"); }
    println!("BEST matches {} at tail_start {}", best.0, best.1);
}
