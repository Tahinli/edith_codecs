//! Scratch: dump every shown frame's planes for the pixel comparator.
//!
//! `INTER_IVF=<path> EC_VP9_PIXDUMP=<out> cargo test -p ec-vp9 --test scratch_pixdump`
//!
//! Layout matches the oracle: per shown frame, Y (w*h), U ((w/2)*(h/2)),
//! V ((w/2)*(h/2)), rows packed to the visible width.

use ec_vp9::decode::Decoder;

#[test]
fn pixdump() {
    let (Ok(ivf), Ok(out)) = (std::env::var("INTER_IVF"), std::env::var("EC_VP9_PIXDUMP")) else {
        eprintln!("SKIP scratch_pixdump: set INTER_IVF and EC_VP9_PIXDUMP");
        return;
    };
    let bytes = std::fs::read(&ivf).expect("read ivf");
    let (_, w, h, frames) = parse_ivf(&bytes);
    let mut decoder = Decoder::new();
    let mut dump = Vec::new();
    let mut shown = 0usize;
    for (i, f) in frames.iter().enumerate() {
        match decoder.decode(&f.data) {
            Ok(Some(pic)) => {
                dump.extend_from_slice(&pic.y);
                dump.extend_from_slice(&pic.u);
                dump.extend_from_slice(&pic.v);
                eprintln!(
                    "PIXFRAME input={i} shown={shown} {w}x{h} {}x{}",
                    pic.width, pic.height
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

fn parse_ivf(bytes: &[u8]) -> (&str, u16, u16, Vec<IvfFrame>) {
    assert_eq!(&bytes[0..4], b"DKIF", "not an IVF file");
    let hdr_len = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    let fourcc = std::str::from_utf8(&bytes[8..12]).unwrap();
    let width = u16::from_le_bytes([bytes[12], bytes[13]]);
    let height = u16::from_le_bytes([bytes[14], bytes[15]]);
    let mut pos = hdr_len;
    let mut frames = Vec::new();
    while pos + 12 <= bytes.len() {
        let sz = u32::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
        ]) as usize;
        let pts = u64::from_le_bytes(bytes[pos + 4..pos + 12].try_into().unwrap());
        pos += 12;
        if pos + sz > bytes.len() {
            break;
        }
        frames.push(IvfFrame {
            pts,
            data: bytes[pos..pos + sz].to_vec(),
        });
        pos += sz;
    }
    (fourcc, width, height, frames)
}

struct IvfFrame {
    #[allow(dead_code)]
    pts: u64,
    data: Vec<u8>,
}
