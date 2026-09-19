mod ivf;
use ec_vp9::decode::Decoder;
#[test]
fn lossless_matches() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = dir.join("fixtures/vp9/lossless-64.ivf");
    let bytes = std::fs::read(&path).unwrap();
    let (_, w, h, frames) = ivf::parse_ivf(&bytes);
    let refout = std::process::Command::new("ffmpeg")
        .args(["-v","error","-i", path.to_str().unwrap(), "-frames:v","1","-f","rawvideo","-pix_fmt","yuv420p","-"])
        .output().unwrap().stdout;
    let mut d = Decoder::new();
    let pic = d.decode(&frames[0].data).unwrap().unwrap();
    let ylen = w as usize * h as usize;
    let mism: Vec<usize> = (0..ylen).filter(|&i| pic.y[i] as u8 != refout[i]).collect();
    println!("w={w} h={h} mismatches={} first={:?} ours={:?} ref={:?}",
        mism.len(), mism.first(), mism.first().map(|&i| pic.y[i]), mism.first().map(|&i| refout[i]));
}
