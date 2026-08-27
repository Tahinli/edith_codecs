use std::io::Write;
use std::process::{Command, Stdio};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/mb.obu".into());
    let stream = std::fs::read(&path).expect("read stream");
    let frames = match ec_av1::stream::decode_stream(&stream) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("decode error: {e}");
            std::process::exit(1);
        }
    };
    let (width, height) = (64usize, 64usize);
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ffmpeg spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&stream)
        .expect("write stream");
    let out = child.wait_with_output().expect("ffmpeg run");
    if !out.status.success() {
        eprintln!("ffmpeg refused: {}", String::from_utf8_lossy(&out.stderr));
        std::process::exit(1);
    }
    let luma = width * height;
    let chroma = luma / 4;
    let y_ref = &out.stdout[..luma];
    let u_ref = &out.stdout[luma..luma + chroma];
    let v_ref = &out.stdout[luma + chroma..luma + 2 * chroma];

    let pic = &frames[0];
    let y_match = pic.y == y_ref;
    let u_match = pic.u == u_ref;
    let v_match = pic.v == v_ref;
    println!("decoded ok, y_match={y_match} u_match={u_match} v_match={v_match}");
    if !y_match {
        for i in 0..luma {
            if pic.y[i] != y_ref[i] {
                println!(
                    "first luma mismatch at idx {i} (row {} col {}): ours={} ref={}",
                    i / width,
                    i % width,
                    pic.y[i],
                    y_ref[i]
                );
                break;
            }
        }
    }
    std::process::exit(if y_match && u_match && v_match { 0 } else { 2 });
}
