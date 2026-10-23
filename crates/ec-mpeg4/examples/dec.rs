//! Decode an MPEG-4 Part 2 / H.263 elementary stream to raw I420 on stdout.
use std::io::Write;

use ec_core::registry::{CodecId, CodecParameters};
use ec_mpeg4::Mpeg4Decoder;

fn split(es: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < es.len() {
        if es[i] == 0 && es[i + 1] == 0 && es[i + 2] == 1 {
            starts.push((i, es[i + 3]));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut cuts = vec![0usize];
    let mut have_vop = false;
    for &(p, c) in &starts {
        match c {
            0xB6 => {
                if have_vop {
                    cuts.push(p);
                }
                have_vop = true;
            }
            0xB0 | 0xB3 if have_vop => {
                cuts.push(p);
                have_vop = false;
            }
            _ => {}
        }
    }
    cuts.push(es.len());
    cuts.windows(2).map(|w| &es[w[0]..w[1]]).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let es = std::fs::read(&args[1]).unwrap();
    let mut dec = Mpeg4Decoder::new(CodecParameters::new(CodecId::Mpeg4Part2)).unwrap();
    if args.len() > 2 {
        dec.set_xvid_idct(Some(args[2] == "xvid"));
    }
    let mut out = std::io::stdout().lock();
    let put = |dec: &mut Mpeg4Decoder, out: &mut dyn Write| {
        while let Some((f, _)) = dec.pop_picture() {
            for p in &f.planes {
                out.write_all(&p.data[..]).unwrap();
            }
        }
    };
    let h263 = es.len() > 3 && es[0] == 0 && es[1] == 0 && es[2] & 0xFC == 0x80;
    let pkts: Vec<&[u8]> = if h263 { split_h263(&es) } else { split(&es) };
    for p in pkts {
        if let Err(e) = dec.send_bytes(p, None) {
            eprintln!("error: {e}");
        }
        put(&mut dec, &mut out);
    }
    dec.flush_stream().unwrap();
    put(&mut dec, &mut out);
}

fn split_h263(es: &[u8]) -> Vec<&[u8]> {
    // picture start code 00 00 80..83 (22 bits)
    let mut cuts = vec![];
    let mut i = 0;
    while i + 3 < es.len() {
        if es[i] == 0 && es[i + 1] == 0 && es[i + 2] & 0xFC == 0x80 {
            cuts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    cuts.push(es.len());
    cuts.windows(2).map(|w| &es[w[0]..w[1]]).collect()
}
