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
        println!("rect_intrabc_reads: {}", ec_av1::stream::rect_intrabc_reads());
        let (pal_y, pal_uv) = ec_av1::stream::intra_in_inter_palette_hits();
        println!("intra_in_inter_palette: y={pal_y} uv={pal_uv}");
        println!(
            "rect64_corner_tu: 64x32={} 32x64={}",
            ec_av1::stream::rect64_corner_tu_hits(0),
            ec_av1::stream::rect64_corner_tu_hits(1)
        );
        println!("leaf8_intrabc_hits: {}", ec_av1::stream::leaf8_intrabc_hits());
        let (rtu, rsplit, robmc) = ec_av1::stream::rect_inter_tu_counters();
        println!("rect_inter: tu={rtu} txsplit={rsplit} obmc_leaf={robmc}");
        println!("sub8_inter_split: groups={}", ec_av1::decode::sub8_inter_split_hits());
        println!("interintra_rect: {}", ec_av1::decode::interintra_rect_hits());
        println!("gm_nontrans_small_side: {}", ec_av1::decode::gm_nontrans_small_side_hits());
        println!("tr_reach_longer_side: {}", ec_av1::decode::tr_reach_longer_side_hits());
        println!(
            "mv_clamp_edge_overhang: {}",
            ec_av1::mvstack::mv_clamp_edge_overhang_hits()
        );
        let (h84, h48) = ec_av1::decode::sub8_inter_rect_hits();
        println!("sub8_inter_rect: horz8x4={h84} vert4x8={h48}");
        let si = ec_av1::stream::sub8_intra_rect_hits();
        println!(
            "sub8_intra_rect: horz8x4={} vert4x8={} chroma_ref={} mixed={} split4x4={}",
            si.0, si.1, si.2, si.3, si.4
        );
        let leaf = ec_av1::stream::vartx_rect_leaf_hits();
        println!("vartx_rect_leaf: 32x16={} 16x32={}", leaf[0], leaf[1]);
        // lane-inter16ab r1: the 16x16-level inter AB arms, so a recipe sweep
        // can tell "aomenc never picked one" from "it did and we decoded it".
        let ab = ec_av1::decode::ab16_inter_hits_by_arm();
        println!(
            "inter_ab16: horz_a={} horz_b={} vert_a={} vert_b={}",
            ab[0], ab[1], ab[2], ab[3]
        );
        // lane-kf900 r1: the counter the skipped-8x8-split-transform gate
        // asserts -- printed here so a recipe sweep can see it fire without a
        // test-binary rebuild.
        println!("skip_split_tx: {}", ec_av1::decode::skip_split_tx_hits());
        let es = ec_av1::decode::inter_edge_strip_hits();
        println!(
            "inter_edge_strip: h64={} v64={} h32={} v32={} h16={} v16={}",
            es[0], es[1], es[2], es[3], es[4], es[5]

        );
        let (h4, v4, pairs, sub8) = ec_av1::decode::inter16_rect4_counters();
        println!("inter16_1to4: horz4={h4} vert4={v4} chroma_pairs={pairs} sub8_pieces={sub8}");
        let leaf4 = ec_av1::stream::vartx_rect_leaf4_hits();
        println!("vartx_rect_leaf4: 8x4={} 4x8={}", leaf4[0], leaf4[1]);
        let rw = ec_av1::stream::rect_wedge_hits();
        let rwi = ec_av1::stream::rect_wii_hits();
        println!(
            "rect_wedge(8x16,16x8,16x32,32x16,8x32,32x8): compound={rw:?} interintra={rwi:?}"
        );
        // lane-inter128intra r1: the 128 root's HORZ/VERT half coded INTRA in
        // an INTER frame -- the arm this round implemented.
        let i128 = ec_av1::stream::intra128_in_inter_counters();
        println!("intra128_in_inter: 128x64={} 64x128={}", i128[0], i128[1]);
        let sb = ec_av1::stream::sb128_rect_counters();
        println!(
            "sb128_rect: edge_horz={} edge_vert={} inter_128x64={} inter_64x128={}",
            sb.0, sb.1, sb.2, sb.3
        );
        // lane-sbab r1: the superblock-level inter AB arms.
        let sbab = ec_av1::decode::sb_ab_inter_hits_by_arm();
        println!(
            "inter_ab64: horz_a={} horz_b={} vert_a={} vert_b={}",
            sbab[0], sbab[1], sbab[2], sbab[3]
        );
        let i4 = ec_av1::stream::intra_rect4_in_inter_counters();
        println!(
            "intra_rect4_in_inter: 64x16={} 16x64={} 32x8={} 8x32={}",
            i4.0, i4.1, i4.2, i4.3
        );
        // lane-cdefstrip r1: 8x8 CDEF units whose skip band no coded block
        // wrote this frame -- non-zero means a decode arm forgot
        // `fill_skip_grid_rect`, and CDEF filtered a unit libaom may skip.
        println!(
            "cdef_unwritten_skip_units: {}",
            ec_av1::decode::cdef_unwritten_skip_units()
        );
        println!(
            "cdef_band: rect_skip_writes={} mixed_skip_units={}",
            ec_av1::decode::rect_skip_band_hits(),
            ec_av1::decode::cdef_mixed_skip_units()
        );
        let ir = ec_av1::stream::inter_rect_counters();
        println!(
            "inter_rect: 32x8={} 8x32={} 64x32={} 32x64={} 64x16={} 16x64={}",
            ir.0, ir.1, ir.2, ir.3, ir.4, ir.5
        );
        // lane-rectres r1: how many of those 32x8/8x32 strips coded a real
        // rectangular residual transform unit (a skipped one codes none).
        println!(
            "sub8_chroma_tx_from_ref: {}",
            ec_av1::stream::sub8_chroma_tx_from_ref_hits()
        );
        let r328 = ec_av1::stream::rect32x8_inter_tu_hits();
        println!("rect32x8_inter_tu: 32x8={} 8x32={}", r328[0], r328[1]);
        // lane-intra16x4: INTRA strips of an inter 16x16-level 1:4 partition.
        let i164 = ec_av1::stream::intra16x4_in_inter_hits();
        println!(
            "intra16x4_in_inter: 16x4={} 4x16={} chroma_ref={}",
            i164.0, i164.1, i164.2
        );
        // lane-t900 r12: chroma edge-filter neighbour answered from the mi-granular
        // uv_mode grid instead of the coarse one-slot-per-column map.
        println!(
            "scaled: leaf8={} sub8={} warpfb={} predict_scaled={}",
            ec_av1::decode::scaled_leaf8_hits(),
            ec_av1::decode::scaled_sub8_hits(),
            ec_av1::decode::scaled_warp_fallback_hits(),
            ec_av1::mc::predict_scaled_hits()
        );
        println!(
            "uv_mode_grid_override: {}",
            ec_av1::decode::uv_mode_grid_override_hits()
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
        // lane-frame36: `EC_PROBE_HDR=1` prints one line per frame header, so a
        // silent wall can be triaged by "which header field is new at frame N"
        // before any pixel work (class: a header field parsed then never consumed).
        if std::env::var("EC_PROBE_HDR").is_ok() {
            let h = header;
            let gm: Vec<String> = h
                .global_motion
                .iter()
                .map(|w| format!("{:?}", w.model))
                .collect();
            println!(
                "HDR {}: type={:?} show={} showable={} show_existing={}({}) intra={} \
                 size={}x{} up={} superres={}/{} order_hint={} primary_ref={} refresh={:#04x} \
                 refs={:?} order_hints={:?} sign_bias={:?} err_res={} disable_cdf={} \
                 screen_tools={} int_mv={} intrabc={} hp_mv={} interp={:?} switchable_motion={} \
                 ref_mvs={} base_q={} seg={} deltaq={} deltalf={} lf_level={:?} lf_deltas={:?}/{:?} \
                 cdef_bits={} lr={:?} tx_mode={:?} ref_sel={} skip_mode={}({:?}) warp={} \
                 reduced_tx={} gm={:?} grain={}/seed={} lossless={}",
                frames_seen - 1,
                h.frame_type,
                h.show_frame,
                h.showable_frame,
                h.show_existing_frame,
                h.frame_to_show_map_idx,
                h.frame_is_intra,
                h.frame_width,
                h.frame_height,
                h.upscaled_width,
                h.use_superres,
                h.superres_denom,
                h.order_hint,
                h.primary_ref_frame,
                h.refresh_frame_flags,
                h.ref_frame_idx,
                h.order_hints,
                h.ref_frame_sign_bias,
                h.error_resilient_mode,
                h.disable_cdf_update,
                h.allow_screen_content_tools,
                h.force_integer_mv,
                h.allow_intrabc,
                h.allow_high_precision_mv,
                h.interpolation_filter,
                h.is_motion_mode_switchable,
                h.use_ref_frame_mvs,
                h.quantization.base_q_idx,
                h.segmentation.enabled,
                h.delta.q_present,
                h.delta.lf_present,
                h.loop_filter.level,
                h.loop_filter.ref_deltas,
                h.loop_filter.mode_deltas,
                h.cdef.bits,
                h.loop_restoration.frame_restoration_type,
                h.tx_mode,
                h.reference_select,
                h.skip_mode_present,
                h.skip_mode_frame,
                h.allow_warped_motion,
                h.reduced_tx_set,
                gm,
                h.film_grain.apply_grain,
                h.film_grain.grain_seed,
                h.coded_lossless,
            );
        }
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

    // lane-streamsink r1: decode through the streaming entry point and write
    // each shown frame out as it completes. The old path collected every
    // picture in a Vec and then built a second full copy of the raw dump
    // before one `fs::write`, so peak RSS was 2x the whole decoded segment (a
    // 4K 8 s window needed >20 GiB, a whole film terabytes). Here at most one
    // frame is held beyond the decoder's own DPB, and the printed lines are
    // byte-identical to the old ones.
    // `EC_PROBE_OUT16` is yuv420p10le (u16 LE); `EC_PROBE_OUT` / argv[2] is
    // 8-bit (planes are u16, low byte taken). Either target may be a FIFO.
    use std::io::Write;
    let open = |path: &str| {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .unwrap_or_else(|e| panic!("opening {path}: {e}"))
    };
    let out16 = std::env::var("EC_PROBE_OUT16").ok();
    let out8 = std::env::args().nth(2).or_else(|| std::env::var("EC_PROBE_OUT").ok());
    let mut f16 = out16.as_deref().map(open);
    let mut f8 = out8.as_deref().map(open);
    let mut written16 = 0usize;
    let mut written8 = 0usize;
    let mut shown = 0usize;
    let mut dims = (0usize, 0usize);
    let mut buf: Vec<u8> = Vec::new();
    let result = ec_av1::stream::decode_stream_with(&data, |f, _idx, is_shown| {
        if !is_shown {
            return Ok(());
        }
        shown += 1;
        if shown == 1 {
            dims = (f.width, f.height);
        }
        if let Some(w) = f16.as_mut() {
            buf.clear();
            for p in [&f.y, &f.u, &f.v] {
                buf.extend(p.iter().flat_map(|&s| s.to_le_bytes()));
            }
            w.write_all(&buf).expect("writing raw planes");
            written16 += buf.len();
        }
        if let Some(w) = f8.as_mut() {
            buf.clear();
            for p in [&f.y, &f.u, &f.v] {
                buf.extend(p.iter().map(|&s| s as u8));
            }
            w.write_all(&buf).expect("writing raw planes");
            written8 += buf.len();
        }
        Ok(())
    });
    match result {
        Ok(()) if shown == 0 => {
            println!("OK but EMPTY: no frames -- is {path} an IVF rather than a raw OBU stream?");
        }
        Ok(()) => {
            if let Some(out) = &out16 {
                println!("wrote {written16} bytes of yuv420p10le to {out}");
            }
            if let Some(out) = &out8 {
                println!("wrote {written8} bytes of yuv420p to {out}");
            }
            println!("OK: {shown} frames decoded, {}x{}", dims.0, dims.1);
        }
        Err(e) => println!("REFUSED: {e}"),
    }
    report();
}
