use std::io::Write;
use std::process::{Command, Stdio};

fn main() {
    let path = std::env::args().nth(1).expect("obu path");
    let stream = std::fs::read(&path).expect("read");
    let frames = match ec_av1::stream::decode_stream(&stream) {
        Ok(f) => f,
        Err(e) => {
            println!("REFUSED: {e}");
            std::process::exit(1);
        }
    };
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("ffmpeg");
    child.stdin.take().unwrap().write_all(&stream).unwrap();
    let out = child.wait_with_output().unwrap().stdout;
    let f = &frames[0];
    let mut ours = f.y.clone();
    ours.extend_from_slice(&f.u);
    ours.extend_from_slice(&f.v);
    println!("match={}", ours == out[..ours.len()]);
}
