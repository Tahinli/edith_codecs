//! Where does a real stream stop? Decode one AV1 file and print the answer.
//!
//! The gates in [`ec_av1::stream`] each pin one capability against one encoder
//! recipe. This asks the complementary question -- given a stream someone
//! actually produced, which refusal comes first -- and it is how the refusal
//! distribution in commit 2be815f was measured rather than reasoned about:
//!
//! ```text
//! aomenc --codec=av1 --obu -o s.obu --passes=1 --cpu-used=4 --limit=4 in.y4m
//! cargo run -p ec-av1 --example decode_probe -- s.obu
//! ```
//!
//! Note `--obu`: [`ec_av1::stream::decode_stream`] takes a raw OBU stream, and
//! an IVF file decodes as zero frames rather than as an error.
//!
//! That run is also what disproved three refusal strings claiming an encoder
//! never writes a case it demonstrably writes, so this is a first-class
//! instrument, not a scratch file.
fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: decode_probe <stream.obu>");
        std::process::exit(2);
    };
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(2);
        }
    };
    let report = || {
        let (sc, dp) = ec_av1::decode::troy_chroma_counters();
        println!("troy_chroma: skip_cfl={sc} dir_1to4_pairs={dp}");
        let (h, v, c) = ec_av1::stream::rect4_32_counters();
        println!("rect4_32: horz={h} vert={v} coded={c}");
        let (rtu, rsplit, robmc) = ec_av1::stream::rect_inter_tu_counters();
        println!("rect_inter: tu={rtu} txsplit={rsplit} obmc_leaf={robmc}");
        let i4 = ec_av1::stream::intra_rect4_in_inter_counters();
        println!(
            "intra_rect4_in_inter: 64x16={} 16x64={} 32x8={} 8x32={}",
            i4.0, i4.1, i4.2, i4.3
        );
        let ir = ec_av1::stream::inter_rect_counters();
        println!(
            "inter_rect: 32x8={} 8x32={} 64x32={} 32x64={} 64x16={} 16x64={}",
            ir.0, ir.1, ir.2, ir.3, ir.4, ir.5
        );
    };
    // lane-tiles: the tiling a real stream actually uses is a decision input
    // (every gate in `stream.rs` picks its own `--tile-columns`), so report it
    // from the frame headers before saying anything about pixels.
    let mut parser = ec_av1_syntax::Av1Parser::new();
    let mut seen: Vec<(u32, u32, bool, u32)> = Vec::new();
    let mut pos = 0usize;
    let mut frames_seen = 0usize;
    // OBU at a time, so one unparseable OBU late in the stream still leaves
    // every earlier frame header's tiling reported.
    while pos < data.len() {
        let Ok(obu) = parser.parse_obu(&data[pos..]) else {
            break;
        };
        pos += obu.total_size.max(1);
        let header = match &obu.kind {
            ec_av1_syntax::ObuKind::FrameHeader(h) => h,
            ec_av1_syntax::ObuKind::Frame(h, _) => h,
            _ => continue,
        };
        frames_seen += 1;
        let t = &header.tile_info;
        let entry = (t.cols, t.rows, t.uniform_spacing, t.context_update_tile_id);
        if !seen.contains(&entry) {
            seen.push(entry);
        }
    }
    println!("TILING: {frames_seen} frame headers parsed");
    // lane-sb128 r4: the superblock size decides the whole partition tree, so
    // report it next to the tiling -- it is the first thing a film blocker
    // triage needs.
    if let Some(seq) = parser.sequence_header() {
        println!(
            "SEQ: use_128x128_superblock={} bit_depth={} max_frame={}x{}",
            seq.use_128x128_superblock,
            seq.color_config.bit_depth,
            seq.max_frame_width,
            seq.max_frame_height
        );
    }
    for (cols, rows, uniform, ctx_id) in &seen {
        println!(
            "TILING: cols={cols} rows={rows} uniform_spacing={uniform} context_update_tile_id={ctx_id}"
        );
    }
    if seen.is_empty() {
        println!("TILING: no frame header parsed");
    }

    match ec_av1::stream::decode_stream(&data) {
        Ok(frames) if frames.is_empty() => {
            println!("OK but EMPTY: no frames -- is {path} an IVF rather than a raw OBU stream?");
        }
        Ok(frames) => {
            // Optional second arg, or EC_PROBE_OUT=<path>: dump the decoded
            // planes as raw yuv420p so a pixel diff against `ffmpeg -i s.obu
            // -f rawvideo` needs no test harness.
            // 10-bit streams: EC_PROBE_OUT16 dumps the planes as little-endian
            // u16 (yuv420p10le), the only form a pixel diff against
            // `ffmpeg -pix_fmt yuv420p10le` can use (lane-band63).
            if let Ok(out) = std::env::var("EC_PROBE_OUT16") {
                let mut buf: Vec<u8> = Vec::new();
                for f in &frames {
                    for p in [&f.y, &f.u, &f.v] {
                        buf.extend(p.iter().flat_map(|&s| s.to_le_bytes()));
                    }
                }
                std::fs::write(&out, &buf).expect("writing raw planes");
                println!("wrote {} bytes of yuv420p10le to {out}", buf.len());
            }
            let out = std::env::args().nth(2).or_else(|| std::env::var("EC_PROBE_OUT").ok());
            if let Some(out) = out {
                // 8-bit only: planes are u16, take the low byte.
                let mut buf: Vec<u8> = Vec::new();
                for f in &frames {
                    for p in [&f.y, &f.u, &f.v] {
                        buf.extend(p.iter().map(|&s| s as u8));
                    }
                }
                std::fs::write(&out, &buf).expect("writing raw planes");
                println!("wrote {} bytes of yuv420p to {out}", buf.len());
            }
            println!("OK: {} frames decoded, {}x{}", frames.len(), frames[0].width, frames[0].height);
        }
        Err(e) => println!("REFUSED: {e}"),
    }
    report();
}
