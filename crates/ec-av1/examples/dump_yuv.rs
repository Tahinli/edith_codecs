//! Decode one OBU stream and write every frame's planes as raw planar video
//! (`<out-prefix>.f<N>.yuv`), at the sample depth **the stream's own sequence
//! header carries**: 8-bit as one byte per sample, 10/12-bit as little-endian
//! `u16`. That is the layout `ffmpeg -pix_fmt yuv420p{,10,12}le -f rawvideo`
//! and `aomdec --rawvideo` write, so a byte-exact `cmp` against either is
//! meaningful.
//!
//! It is the high-bit-depth companion to `EC_AV1_PREFILT_DUMP`, whose `as u8`
//! narrowing throws away exactly the bits a 10-bit mismatch lives in
//! (lane-rect1d r1 found its defect by diffing this against
//! `ffmpeg -pix_fmt yuv420p10le -f rawvideo`).
//!
//! ```text
//! cargo run -p ec-av1 --example dump_yuv -- s.obu /tmp/ours
//! ffmpeg -v error -i s.obu -pix_fmt yuv420p10le -f rawvideo /tmp/ref.yuv
//! cmp /tmp/ours.f0.yuv /tmp/ref.yuv
//! ```
//!
//! The depth is NEVER taken from the file name, the output extension, or the
//! `-pix_fmt` of a reference tool: it is parsed out of the sequence header
//! (spec 5.5.2 `color_config.bit_depth`) before a single byte is written. The
//! old version packed every sample as `u16` unconditionally, so pointing it at
//! an 8-bit stream silently produced a file TWICE the size of the oracle's,
//! and the size mismatch read like a decoder bug. `--depth N` is accepted for
//! the case where you already know what the stream is, and is ASSERTED against
//! the parsed header: a mismatch names both depths and the consequence it
//! would have caused, and writes nothing.
//!
//! `i` in `<out-prefix>.f<i>.yuv` indexes `decode_stream`'s output, which is
//! DISPLAY order and SHOWN frames only -- hidden alt-refs are never emitted.
//! Every other `.f{N}` in this crate (EC_AV1_FINAL_DUMP,
//! EC_AV1_DECODE_ORDER_DUMP, EC_AV1_PREFILT_DUMP) uses a DECODE index, so this
//! file does NOT line up with an aomdec dump past frame 0 on a stream with
//! altref. Compare against ffmpeg rawvideo, or against aomdec via
//! `decode_all_frames_vs_oracle`.
fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (Some(path), Some(out)) = (argv.first().cloned(), argv.get(1).cloned()) else {
        eprintln!("usage: dump_yuv <stream.obu> <out-prefix> [--depth 8|10|12]");
        std::process::exit(2);
    };
    // An ASSERTED expectation, not a switch: the packing always follows the
    // parsed header, and a wrong `--depth` is a hard error before any file.
    let mut claimed_depth: Option<u8> = None;
    let mut rest = argv[2.min(argv.len())..].iter();
    while let Some(arg) = rest.next() {
        let value = arg
            .strip_prefix("--depth=")
            .map(str::to_string)
            .or_else(|| (arg == "--depth").then(|| rest.next().cloned()).flatten());
        let Some(value) = value else {
            eprintln!("dump_yuv: unknown argument `{arg}`");
            eprintln!("usage: dump_yuv <stream.obu> <out-prefix> [--depth 8|10|12]");
            std::process::exit(2);
        };
        let Ok(depth) = value.parse::<u8>() else {
            eprintln!("dump_yuv: --depth `{value}` is not a bit depth (8, 10 or 12)");
            std::process::exit(2);
        };
        if claimed_depth.is_some() {
            eprintln!("dump_yuv: --depth given twice; the depth is asserted, not a switch");
            std::process::exit(2);
        }
        claimed_depth = Some(depth);
    }
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(2);
        }
    };

    // The stream's OWN header, read before the decode so a depth mismatch is
    // reported instead of a file. Scans OBU by OBU because the sequence header
    // need not be the first OBU (decode_probe walks it the same way).
    let mut parser = ec_av1_syntax::Av1Parser::new();
    let mut pos = 0usize;
    while pos < data.len() && parser.sequence_header().is_none() {
        let Ok(obu) = parser.parse_obu(&data[pos..]) else {
            break;
        };
        pos += obu.total_size.max(1);
    }
    let Some(seq) = parser.sequence_header() else {
        eprintln!(
            "dump_yuv: {path}: no sequence header parsed, so the sample depth is unknown. \
             Refusing to guess: packing u16 blind is what wrote a 2x-size file for every 8-bit \
             stream. Re-checkout the stream (an IVF container parses as zero frames too), or pass \
             a raw OBU stream."
        );
        std::process::exit(1);
    };
    let cfg = &seq.color_config;
    let bit_depth = cfg.bit_depth;
    if let Some(claimed) = claimed_depth {
        if claimed != bit_depth {
            let theirs = if bit_depth == 8 {
                format!(
                    "aomdec --rawvideo / ffmpeg -pix_fmt yuv420p ({} bytes per sample, \
                     W*H*3/2 for 4:2:0)",
                    1
                )
            } else {
                format!(
                    "aomdec --rawvideo / ffmpeg -pix_fmt yuv420p{}le ({} bytes per sample, \
                     W*H*3 for 4:2:0)",
                    bit_depth, 2
                )
            };
            eprintln!(
                "dump_yuv: {path}: --depth {claimed} contradicts the stream's own sequence \
                 header, which carries bit_depth {bit_depth} (seq_profile {}, \
                 subsampling {}/{}, mono_chrome {}). Writing {claimed}-bit would emit \
                 {} bytes per sample against the oracle's {theirs}, so every `cmp` would fail on \
                 size alone and read like a decoder defect. Nothing was written.",
                seq.seq_profile,
                cfg.subsampling_x,
                cfg.subsampling_y,
                cfg.mono_chrome,
                if claimed == 8 { 1 } else { 2 },
            );
            std::process::exit(1);
        }
    }
    // 8 bit -> one byte per sample; anything above -> little-endian u16, which
    // is what both oracles emit for `*p10le` / `*p12le` (they never pack to
    // the bit depth itself, they use a full 16-bit container).
    let bytes_per_sample = if bit_depth == 8 { 1 } else { 2 };
    // Spec 5.5.3 `subsampling_{x,y}`: 1/1 IS 4:2:0 (measured on this crate's own
    // 8-bit 192x128 fixture, which parses 1/1 and yields 36864 samples for a
    // 192x128 frame = W*H*3/2), 0/0 is 4:4:4, 1/0 is 4:2:2, 0/1 is 4:4:0.
    let chroma = match (cfg.mono_chrome, cfg.subsampling_x, cfg.subsampling_y) {
        (true, _, _) => "gray",
        (false, 0, 0) => "yuv444p",
        (false, 0, 1) => "yuv440p",
        (false, 1, 0) => "yuv422p",
        _ => "yuv420p",
    };
    let pix_fmt = match (chroma, bit_depth) {
        (c, 8) => c.to_string(),
        ("gray", _) => "gray16le".to_string(),
        (c, 10) => format!("{c}10le"),
        (c, 12) => format!("{c}12le"),
        (c, d) => format!("{c}{d}le"),
    };
    let max = (1u32 << bit_depth) - 1;
    println!(
        "sequence header: bit_depth {bit_depth} -> {bytes_per_sample} byte(s) per sample, \
         seq_profile {}, subsampling {}/{}, mono_chrome {}; ffmpeg -pix_fmt {pix_fmt}",
        seq.seq_profile, cfg.subsampling_x, cfg.subsampling_y, cfg.mono_chrome,
    );

    let frames = match ec_av1::stream::decode_stream(&data) {
        Ok(frames) => frames,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    for (i, f) in frames.iter().enumerate() {
        let planes: &[&Vec<u16>] = if cfg.mono_chrome {
            &[&f.y]
        } else {
            &[&f.y, &f.u, &f.v]
        };
        // The OTHER half of the depth contract: samples that do not fit the
        // container we are about to write. A `s as u8` on a 10-bit plane (or a
        // u16 on a depth we packed narrow) is exactly the bit loss this tool
        // exists to rule out, so it is an error, not a truncation.
        if let Some((plane_name, sample)) = [("Y", &f.y), ("U", &f.u), ("V", &f.v)]
            .into_iter()
            .take(if cfg.mono_chrome { 1 } else { 3 })
            .find_map(|(name, plane)| plane.iter().max().map(|&s| (name, s)))
        {
            if u32::from(sample) > max {
                eprintln!(
                    "dump_yuv: {path}: frame {i} plane {plane_name} carries sample {sample} above \
                     the {bit_depth}-bit maximum {max} -- the decoded output does not match the \
                     sequence header it was decoded under, and packing it would silently drop the \
                     high bits. Nothing was written."
                );
                std::process::exit(1);
            }
        }
        let samples: usize = planes.iter().map(|p| p.len()).sum();
        let mut buf = Vec::with_capacity(samples * bytes_per_sample);
        for plane in planes {
            if bytes_per_sample == 1 {
                buf.extend(plane.iter().map(|&s| s as u8));
            } else {
                for &s in plane.iter() {
                    buf.extend_from_slice(&s.to_le_bytes());
                }
            }
        }
        let name = format!("{out}.f{i}.yuv");
        if let Err(e) = std::fs::write(&name, &buf) {
            eprintln!("{name}: {e}");
            std::process::exit(2);
        }
        println!(
            "frame {i}: {}x{} -> {name} ({samples} samples, {} bytes, {pix_fmt})",
            f.width,
            f.height,
            buf.len(),
        );
    }
}
