//! Decode a raw DTS stream (or the STRMDATA of a `.dtshd` file) to
//! interleaved `f32le` on stdout. Dev tool for comparing against ffmpeg.
//!
//! `dec <file> [out.f32]`; frames are split with `ec_dts::frame_size`.

use ec_dts::DtsDecoder;
use std::io::Write;

fn strmdata(buf: &[u8]) -> &[u8] {
    if !buf.starts_with(b"DTSHDHDR") {
        return buf;
    }
    let mut p = 0;
    while p + 16 <= buf.len() {
        let tag = &buf[p..p + 8];
        let size = u64::from_be_bytes(buf[p + 8..p + 16].try_into().unwrap()) as usize;
        if tag == b"STRMDATA" {
            return &buf[p + 16..(p + 16 + size).min(buf.len())];
        }
        p += 16 + size;
    }
    buf
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).unwrap();
    let data = strmdata(&data);
    let mut dec = DtsDecoder::new();
    let mut out: Box<dyn Write> = match args.get(2) {
        Some(p) => Box::new(std::io::BufWriter::new(std::fs::File::create(p).unwrap())),
        None => Box::new(std::io::sink()),
    };
    let mut pos = 0;
    let mut frames = 0;
    let mut errors = 0;
    while pos + 4 < data.len() {
        let Some((s, _)) = ec_dts::find_sync(&data[pos..]) else { break };
        pos += s;
        let hdr = match ec_dts::parse_frame_header(&data[pos..]) {
            Ok(h) => h,
            Err(_) => {
                pos += 1;
                continue;
            }
        };
        // The frame plus whatever follows up to the next core sync word.
        let end = ec_dts::find_sync(&data[pos + hdr.stream_frame_size.min(data.len() - pos)..])
            .map(|(o, _)| pos + hdr.stream_frame_size + o)
            .unwrap_or(data.len());
        match dec.decode_frame(&data[pos..end]) {
            Ok(f) => {
                frames += 1;
                out.write_all(&f.data[0]).unwrap();
            }
            Err(e) => {
                errors += 1;
                eprintln!("frame at {pos}: {e}");
            }
        }
        pos = end;
    }
    eprintln!("frames={frames} errors={errors} info={:?}", dec.frame_info().map(|i| (i.sample_rate, i.header.channels, i.extensions, i.xll)));
}
