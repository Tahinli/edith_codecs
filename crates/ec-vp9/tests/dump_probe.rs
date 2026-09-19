mod ivf;
use ec_vp9::decode::Decoder;
#[test]
fn diff_stats() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes = std::fs::read(dir.join("fixtures/vp9/key-320.ivf")).unwrap();
    let (_, _, _, frames) = ivf::parse_ivf(&bytes);
    let mut d = Decoder::new();
    let pic = d.decode(&frames[0].data).unwrap().unwrap();
    let refout = std::process::Command::new("ffmpeg")
        .args(["-v","error","-i",dir.join("fixtures/vp9/key-320.ivf").to_str().unwrap(),
               "-frames:v","1","-f","rawvideo","-pix_fmt","yuv420p","-"])
        .output().unwrap().stdout;
    let w = 320usize;
    let mut sb_match = [[0usize; 5]; 4];
    let mut sb_total = [[0usize; 5]; 4];
    let mut first: Option<(usize, usize)> = None;
    for y in 0..240 {
        for x in 0..320 {
            let a = pic.y[y * pic.stride + x] as u8;
            let b = refout[y * w + x];
            let (sy, sx) = (y / 64, x / 64);
            sb_total[sy][sx] += 1;
            if a == b { sb_match[sy][sx] += 1; } else if first.is_none() { first = Some((x, y)); }
        }
    }
    println!("first diff at {:?}", first);
    for (i, row) in sb_match.iter().enumerate() {
        println!("sb row {i}: {:?}", row.iter().zip(sb_total[i].iter()).map(|(m, t)| format!("{}/{}", m, t)).collect::<Vec<_>>());
    }
}
