//! Synthetic range extension streams (4:0:0, 4:2:2 and 4:4:4 at 8 to 12 bits, transform skip,
//! lossless, wavefronts and slices, made by `scripts/gen-hevc-rext-fixtures.sh`) against the MD5
//! of ffmpeg's decode, stored beside each stream. No external tool is needed at test time.

use std::path::Path;

use ec_h265::Decoder;
use ec_h265_syntax::md5::Md5;

fn hex(d: [u8; 16]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The MD5 of all output pictures, cropped to the conformance window, as planar samples:
/// one byte per sample at 8 bits, two (little endian) above.
fn output_md5(data: &[u8]) -> Result<(String, usize), String> {
    let mut dec = Decoder::new();
    dec.decode_annex_b(data)
        .map_err(|e| format!("decode: {e}"))?;
    dec.flush().map_err(|e| format!("flush: {e}"))?;
    let mut md5 = Md5::new();
    let mut frames = 0;
    while let Some(f) = dec.next_frame() {
        let p = &*f.picture;
        let (sx, sy) = p.chroma_shifts();
        let (cl, cr, ct, cb) = (
            f.window.left as usize,
            f.window.right as usize,
            f.window.top as usize,
            f.window.bottom as usize,
        );
        let mono = p.chroma_format_idc == 0;
        let (wx, wy) = if mono {
            (1, 1)
        } else {
            (1usize << sx, 1usize << sy)
        };
        let (cw, _) = p.chroma_dims();
        let planes: [(&Vec<u16>, usize, [usize; 4]); 3] = [
            (
                &p.y,
                p.width,
                [wx * cl, p.width - wx * cr, wy * ct, p.height - wy * cb],
            ),
            (
                &p.cb,
                cw,
                [
                    cl,
                    cw.saturating_sub(cr),
                    ct,
                    p.chroma_dims().1.saturating_sub(cb),
                ],
            ),
            (
                &p.cr,
                cw,
                [
                    cl,
                    cw.saturating_sub(cr),
                    ct,
                    p.chroma_dims().1.saturating_sub(cb),
                ],
            ),
        ];
        for (c, (plane, stride, [x0, x1, y0, y1])) in planes.into_iter().enumerate() {
            if plane.is_empty() {
                continue;
            }
            let wide = p.bit_depth_of(c) > 8;
            let mut bytes = Vec::new();
            for y in y0..y1 {
                for x in x0..x1 {
                    let v = plane[y * stride + x];
                    if wide {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    } else {
                        bytes.push(v as u8);
                    }
                }
            }
            md5.update(&bytes);
        }
        frames += 1;
    }
    Ok((hex(md5.finish()), frames))
}

#[test]
fn generated_rext_streams_match_ffmpeg() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/rext");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/data/rext")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "265"))
        .collect();
    names.sort();
    assert!(
        names.len() >= 20,
        "expected the generated streams in {dir:?}"
    );
    let mut bad = Vec::new();
    for path in &names {
        let data = std::fs::read(path).unwrap();
        let want = std::fs::read_to_string(path.with_extension("md5")).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        match output_md5(&data) {
            Ok((got, frames)) if frames > 0 && got == want.trim() => {}
            Ok((_, frames)) => bad.push(format!("{name}: {frames} frames, md5 differs")),
            Err(e) => bad.push(format!("{name}: {e}")),
        }
    }
    assert!(bad.is_empty(), "streams that differ from ffmpeg: {bad:?}");
}
