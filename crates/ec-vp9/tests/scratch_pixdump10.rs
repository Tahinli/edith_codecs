//! Scratch: dump every shown frame's planes as little-endian u16 for the
//! high-bit-depth pixel comparator.
//!
//! `INTER_IVF=<path> EC_VP9_PIXDUMP=<out> cargo test -p ec-vp9 --test scratch_pixdump10`
//!
//! Layout matches drv_pix10: per shown frame, Y (w*h), U and V each
//! (((w+1)/2)*((h+1)/2)) (chroma ceils), rows packed to the visible width,
//! each sample written as two little-endian bytes.

use ec_vp9::decode::Decoder;

#[test]
fn pixdump10() {
    let (Ok(ivf), Ok(out)) = (std::env::var("INTER_IVF"), std::env::var("EC_VP9_PIXDUMP")) else {
        eprintln!("SKIP scratch_pixdump10: set INTER_IVF and EC_VP9_PIXDUMP");
        return;
    };
    let bytes = std::fs::read(&ivf).expect("read ivf");
    let (_, w, h, frames) = parse_ivf(&bytes);
    let mut decoder = Decoder::new();
    let mut dump = Vec::new();
    let mut shown = 0usize;
    for (i, f) in frames.iter().enumerate() {
        match decoder.decode(&f) {
            Ok(Some(pic)) => {
                for &v in pic.y.iter().chain(pic.u.iter()).chain(pic.v.iter()) {
                    dump.extend_from_slice(&v.to_le_bytes());
                }
                eprintln!(
                    "PIXFRAME input={i} shown={shown} {w}x{h} {}x{} bd={}",
                    pic.width, pic.height, pic.bit_depth
                );
                shown += 1;
            }
            Ok(None) => eprintln!("PIXFRAME input={i} hidden"),
            Err(e) => {
                eprintln!("PIXERR input={i}: {e}");
                break;
            }
        }
    }
    std::fs::write(&out, &dump).expect("write dump");
    eprintln!("wrote {} bytes ({shown} shown frames)", dump.len());
}

fn parse_ivf(bytes: &[u8]) -> (&str, u16, u16, Vec<Vec<u8>>) {
    assert_eq!(&bytes[0..4], b"DKIF", "not an IVF file");
    let hdr_len = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    let fourcc = std::str::from_utf8(&bytes[8..12]).unwrap();
    let width = u16::from_le_bytes([bytes[12], bytes[13]]);
    let height = u16::from_le_bytes([bytes[14], bytes[15]]);
    let mut pos = hdr_len;
    let mut frames = Vec::new();
    while pos + 12 <= bytes.len() {
        let sz = u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let data = bytes[pos + 12..pos + 12 + sz].to_vec();
        frames.push(data);
        pos += 12 + sz;
    }
    (fourcc, width, height, frames)
}
