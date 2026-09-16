//! [scratch] Parse a synthetic keyframe header dump.
//!
//! The dump was produced ad hoc in an earlier session and lives in tmpfs
//! (`/tmp/synth-kf-frame.bin`), so the test skips - loudly - when it is gone:
//! a missing tmpfs fixture is not a regression.
use ec_vp9_syntax::Vp9Parser;
#[test]
fn synth() {
    let Ok(frame) = std::fs::read("/tmp/synth-kf-frame.bin") else {
        println!("SKIP: /tmp/synth-kf-frame.bin absent (tmpfs fixture)");
        return;
    };
    let mut p = Vp9Parser::new();
    match p.parse_frame(&frame) {
        Ok(h) => println!(
            "w={} h={} uhs={} hsib={}",
            h.width, h.height, h.uncompressed_header_size, h.header_size_in_bytes
        ),
        Err(e) => println!("ERR {e}"),
    }
}
