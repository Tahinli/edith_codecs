use super::*;
use super::support::*;
use super::gates_a::*;
use super::gates_b::*;
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

    /// lane-tiles r10: non-uniform tile spacing (`uniform_spacing_flag == 0`,
    /// spec 5.9.15). aomenc never sets this from `--tile-columns`/
    /// `--tile-rows` (those always take the `uniform_spacing = 1` branch in
    /// `set_tile_info`, libaom `encoder.c`), and `--auto-tiles` only takes
    /// the non-uniform balancing path when `g_threads >= 2` -- this charter's
    /// `--threads=1` rules that out. The only CLI surface that reaches
    /// libaom's `else { tiles->uniform_spacing = 0; ... }` arm is the
    /// explicit per-tile size lists `--tile-width=<sb-list>`/
    /// `--tile-height=<sb-list>` (`av1_cx_iface.c` `set_tile_info`, argument
    /// parsing at `arg_defs.c`'s `.tile_width`/`.tile_height` -- present in
    /// the binary, just not printed by `--help`). Probed live with
    /// `aomenc --tile-width=1,3 --tile-height=1 --sb-size=64` on a 256x64
    /// source (4 SB columns, 1 SB row): confirmed by hand this round to
    /// produce a real OBU stream aomdec/ffmpeg both decode.
    ///
    /// `decode.rs`'s tile loop was already generic over spacing before this
    /// gate existed -- it walks `tile_info.mi_col_starts`/`mi_row_starts`
    /// (populated identically by `read_tile_info` for both the uniform and
    /// non-uniform branches, spec 5.9.15) and never recomputes tile bounds
    /// from `cols_log2`/`rows_log2`. The only place in this crate that ever
    /// refused non-uniform spacing is `frame.rs`'s OBU *writer*
    /// (`"non-uniform tile spacing is not written"`), an unrelated encode
    /// path this gate does not touch. So this is a staleness check, not new
    /// machinery: prove the already-generic decode path live and pin it.
    #[test]
    fn a_real_aomenc_stream_with_non_uniform_tile_spacing_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_non_uniform_tile_spacing_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (256usize, 64usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut saw_non_uniform_spacing = false;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let out = run_with_stdin(
                Command::new(aomenc_path()).args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--limit=1",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    "--tile-width=1,3",
                    "--tile-height=1",
                    "--loopfilter-control=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ]),
                &y4m.stdout,
            );
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            let mut probe = Av1Parser::new();
            let mut pos = 0usize;
            while pos < stream.len() {
                let obu = probe.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                let tile_info = match &obu.kind {
                    ObuKind::Frame(header, _) => Some(&header.tile_info),
                    ObuKind::FrameHeader(header) if !header.show_existing_frame => {
                        Some(&header.tile_info)
                    }
                    _ => None,
                };
                if let Some(info) = tile_info {
                    if !info.uniform_spacing {
                        saw_non_uniform_spacing = true;
                    }
                    assert!(
                        info.cols > 1 || info.rows > 1,
                        "{NAME}: --tile-width=1,3 --tile-height=1 produced only 1 tile \
                         (cols={} rows={}, seed {seed}) -- fixture stopped exercising the case",
                        info.cols,
                        info.rows
                    );
                }
            }

            let before = crate::decode::tile_hits();
            let pictures = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(pictures) => pictures,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 0,
                "{NAME}: tile_hits delta was {hits}, expected >0 (seed {seed})"
            );
            assert_eq!(
                pictures.len(),
                1,
                "{NAME}: expected exactly 1 picture (seed {seed})"
            );

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                pictures[0].y, ffmpeg_frames[0].y,
                "{NAME}: luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                pictures[0].u, ffmpeg_frames[0].u,
                "{NAME}: U vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                pictures[0].v, ffmpeg_frames[0].v,
                "{NAME}: V vs ffmpeg (seed {seed})"
            );
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); decode_stream never \
             decoded a non-uniform-tile-spacing stream"
        );
        assert!(
            saw_non_uniform_spacing,
            "{NAME}: no attempt actually set uniform_spacing_flag == 0 -- this sweep proves \
             nothing about the non-uniform case"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-sb128 r1 gate: a real `aomenc --sb-size=128` stream (the size his
    /// ReelC encode uses -- `EC_TRACE=1 aomdec reelc-head.obu` prints
    /// `EC_PART mi_row=0 mi_col=0 bsize=15` = `BLOCK_128X128`) must decode
    /// pixel-exact. `--max-partition-size=64` keeps the RD search from ever
    /// choosing a 128-level NONE/HORZ/VERT/AB arm, so every 128 root here is
    /// genuinely `PARTITION_SPLIT` -- the one arm
    /// `decode_key_frame_tile_with_cdfs` codes; the other seven refuse by
    /// name (`PART128_NON_SPLIT_REFUSAL`) and stay refused after this round.
    ///
    /// What the 128 root actually changes, and what this gate therefore
    /// proves: the 8-symbol `partition_w128` CDF and its forced-split gather
    /// at the frame edge, the quadrant VISIT ORDER (TL, TR, BL, BR inside one
    /// superblock, not raster over 64x64 superblocks), one loop-restoration
    /// read per 128x128 instead of per 64x64, `delta_q`/`delta_lf` once per
    /// 128x128, and the per-64x64 `cdef_idx` read inside the 128 superblock.
    /// 256x192 gives a 2x2 grid of 128 superblocks whose bottom row is a
    /// partial superblock (the gathered forced-split read); 384x384 gives a
    /// 3x3 grid of whole ones. 8-bit sweep plus one 10-bit stream (his films
    /// are `yuv420p10le`). Only a missing tool SKIPs.
    #[test]
    fn a_real_aomenc_stream_with_128x128_superblocks_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_128x128_superblocks_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let base: Vec<&str> = vec![
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=40",
            "--cpu-used=0",
            "--threads=1",
            "--row-mt=0",
            "--sb-size=128",
            "--max-partition-size=64",
            "--enable-rect-partitions=1",
            "--enable-ab-partitions=0",
            "--enable-1to4-partitions=0",
            // aomenc emits PARTITION_HORZ_4/VERT_4 below 16x16 even with the
            // flag off (ledger, lane-modectx r1), so bound the search instead.
            "--min-partition-size=16",
            "--enable-restoration=1",
            "--enable-palette=0",
            "--deltaq-mode=0",
            "--enable-intrabc=0",
            "--enable-tx-size-search=0",
            "--limit=1",
        ];
        let encode = |y4m: &[u8], extra: &[&str]| -> Vec<u8> {
            let mut args = base.clone();
            args.extend_from_slice(extra);
            args.extend_from_slice(&["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), y4m);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        let render = |seed: u32, width: usize, height: usize, ten_bit: bool| -> Vec<u8> {
            let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
            let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &gradients_source(seed, width, height, "duration=0.04:rate=25"),
                    "-pix_fmt",
                    pix,
                    "-strict",
                    "-1",
                    "-t",
                    "0.04",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                out.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        // (seed, width, height, 10-bit): 256x192 = 2x2 superblocks with a
        // partial bottom row; 384x384 = 3x3 whole ones.
        let arms: Vec<(u32, usize, usize, bool)> = vec![
            (42, 256, 192, false),
            (43, 256, 192, false),
            (44, 256, 192, false),
            (45, 384, 384, false),
            (46, 256, 192, true),
            (47, 384, 384, true),
        ];
        for (seed, width, height, ten_bit) in arms {
            let y4m = render(seed, width, height, ten_bit);
            let extra: Vec<&str> = if ten_bit {
                vec!["--input-bit-depth=10", "--bit-depth=10"]
            } else {
                vec![]
            };
            let stream = encode(&y4m, &extra);
            let parsed = {
                let mut parser = ec_av1_syntax::Av1Parser::new();
                let mut pos = 0usize;
                while let Ok(obu) = parser.parse_obu(&stream[pos..]) {
                    pos += obu.total_size;
                    if pos >= stream.len() {
                        break;
                    }
                }
                parser
                    .sequence_header()
                    .map(|q| (q.use_128x128_superblock, q.color_config.bit_depth))
            };
            assert_eq!(
                parsed,
                Some((true, if ten_bit { 10 } else { 8 })),
                "{NAME} (seed {seed}): the encoder did not write a 128x128-superblock \
                 sequence header at the requested bit depth -- the flag never arrived"
            );
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let want = if ten_bit {
                ffmpeg_decode_sequence_10bit(&stream, width, height, 1)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, 1)
            };
            assert_eq!(frames.len(), 1);
            for (i, (got, want)) in frames.iter().zip(&want).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} frame {i} luma vs ffmpeg (seed {seed})"
                );
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a stream"
        );
        // 256x192 codes 3 whole + 1 partial superblock, 384x384 codes 9 whole
        // ones, so a run that decoded anything at all must have read real 128
        // partition symbols and split every one of them.
        assert!(
            crate::decode::part128_symbols() >= 3 * matched as usize,
            "{NAME}: only {} 128-level partition symbols over {matched} decoded streams -- \
             the 128 root never fired",
            crate::decode::part128_symbols()
        );
        assert!(
            crate::decode::part128_split_hits() >= 4 * matched as usize,
            "{NAME}: only {} 128 superblocks split",
            crate::decode::part128_split_hits()
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact, {named_refusals} named refusals, \
             part128_symbols={} part128_split_hits={}",
            crate::decode::part128_symbols(),
            crate::decode::part128_split_hits()
        );
    }

    /// lane-sb128b r1 gate: a real `aomenc --sb-size=128` KEY frame whose 128
    /// roots resolve to `PARTITION_NONE` -- one whole 128x128 intra block,
    /// four TX_64X64 luma transform units and FOUR TX_32X32 chroma units per
    /// plane (`av1_get_max_uv_txsize(BLOCK_128X128)` is
    /// `av1_get_adjusted_tx_size` of its 64x64 uv plane block's TX_64X64, so
    /// TX_32X32) -- lane-sb128b r2's `chroma_split_tx_hits` asserts them. The r1 gate above pins `--max-partition-size=64`, which
    /// makes every 128 root a forced SPLIT and leaves this arm unexercised.
    /// Smooth gradients at a high `--cq-level` are what make the RD search
    /// keep a whole 128x128 block; `part128_none_hits` HARD-asserts it did.
    #[test]
    fn a_real_aomenc_key_frame_with_a_128x128_none_partition_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_key_frame_with_a_128x128_none_partition_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let render = |seed: u32, width: usize, height: usize, ten_bit: bool| -> Vec<u8> {
            let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
            let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &gradients_source(seed, width, height, "duration=0.04:rate=25"),
                    "-pix_fmt",
                    pix,
                    "-strict",
                    "-1",
                    "-t",
                    "0.04",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                out.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut none_arms = 0u32;
        // (seed, cq, width, height, 10-bit). 384x384 is a 3x3 grid of whole
        // 128 superblocks; 256x256 a 2x2.
        let arms: Vec<(u32, &str, usize, usize, bool)> = vec![
            (42, "--cq-level=63", 384, 384, false),
            (43, "--cq-level=55", 384, 384, false),
            (44, "--cq-level=63", 256, 256, false),
            (45, "--cq-level=63", 384, 384, true),
        ];
        for (seed, cq, width, height, ten_bit) in arms {
            let y4m = render(seed, width, height, ten_bit);
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                cq,
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=128",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=16",
                "--enable-restoration=1",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-intrabc=0",
                "--enable-tx-size-search=0",
                "--limit=1",
            ];
            if ten_bit {
                args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
            }
            args.extend_from_slice(&["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = crate::decode::part128_none_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let want = if ten_bit {
                ffmpeg_decode_sequence_10bit(&stream, width, height, 1)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, 1)
            };
            assert_eq!(frames.len(), 1);
            for (i, (got, want)) in frames.iter().zip(&want).enumerate() {
                for (name, a, b, w) in [
                    ("Y", &got.y, &want.y, width),
                    ("U", &got.u, &want.u, width / 2),
                    ("V", &got.v, &want.v, width / 2),
                ] {
                    let n = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
                    let first = a.iter().zip(b.iter()).position(|(x, y)| x != y);
                    assert_eq!(
                        n,
                        0,
                        "{NAME} frame {i} {name} vs ffmpeg (seed {seed}): {n} differ, first row {} col {} ours {:?} ffmpeg {:?}",
                        first.unwrap_or(0) / w,
                        first.unwrap_or(0) % w,
                        first.map(|k| a[k]),
                        first.map(|k| b[k]),
                    );
                    assert_eq!(
                        a.len(),
                        b.len(),
                        "{NAME} frame {i} {name} vs ffmpeg (seed {seed}): ours has {} samples, \
                         ffmpeg's has {} -- a short plane would zip-truncate the walk above \
                         and hide a chroma-span defect",
                        a.len(),
                        b.len()
                    );
                }
            }
            let hits = crate::decode::part128_none_hits() - before;
            eprintln!("seed {seed} cq {cq} {width}x{height}: part128_none_hits +{hits}");
            if hits > 0 {
                none_arms += 1;
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a stream"
        );
        assert!(
            none_arms > 0,
            "{NAME}: {matched} stream(s) decoded pixel-exact but NOT ONE 128 root was \
             PARTITION_NONE -- the arm this gate exists for never fired"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact ({none_arms} with a 128 NONE root), \
             {named_refusals} named refusals, part128_none_hits={}",
            crate::decode::part128_none_hits()
        );
    }

    /// lane-sb128c r7 GATE: the 128 root's GATHERED bottom/right-edge arm --
    /// the only 128-partition shape the user's 1080p AV1 film reaches
    /// (`mi_rows = 270`, so every bottom 128 row has `has_rows128 == false`,
    /// one gathered bit, and the film codes `PARTITION_HORZ`: a 128x64 block
    /// whose bottom half is outside the frame and never decoded). Frame sizes
    /// here are chosen so the LAST superblock row (or column, for the VERT
    /// twin at 192x256) is partial, which is what forces the gathered symbol.
    /// Both the key frame and the inter frames of each stream carry the arm;
    /// `part128_edge_*_hits` and `inter_sb128_*_hits` HARD-assert it fired,
    /// and `decode_all_frames_vs_oracle` compares EVERY decode-order frame
    /// (hidden alt-refs included) against the instrumented aomdec.
    #[test]
    fn a_real_aomenc_sb128_gathered_edge_horz_partition_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_sb128_gathered_edge_horz_partition_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // (seed, cq, width, height, 10-bit, vertical twin). Every size has a
        // PARTIAL last superblock row (or column, for the VERT twin): 320 px
        // = 80 mi rows, two full 128 rows and a 64-px remainder; 256 px = 64
        // mi = two full rows and none, so the VERT arm uses 192 px = 48 mi on
        // the column axis. A smooth `gradients` source makes the KEY frame
        // take the arm too, which is still refused by name (square-block
        // intra), so this gate's source is `mandelbrot`: its key frames split
        // and its inter frames keep the 128x64/64x128 edge block.
        // lane-vert46 r1: the last field picks a SMOOTH deterministic source
        // (`geq` sinusoids, the sb128c r8 intra arm's own fixture) instead of
        // `mandelbrot`. Arm 46 is the stream that exposed this round's two
        // defects: mandelbrot's inter frames never put a 64x128 edge block
        // next to a neighbour whose partition context or per-mu-chunk chroma
        // DC sign decides a symbol, so both were invisible to arms 42-45.
        let arms: Vec<(u32, &str, usize, usize, bool, bool, bool)> = vec![
            (42, "--cq-level=55", 384, 320, false, false, false),
            (43, "--cq-level=55", 384, 320, true, false, false),
            (44, "--cq-level=32", 192, 256, false, true, false),
            (45, "--cq-level=45", 192, 256, true, true, false),
            (46, "--cq-level=55", 192, 256, false, true, true),
        ];
        let (mut matched, mut named_refusals) = (0u32, 0u32);
        let (mut edge_arms, mut inter_arms) = (0u32, 0u32);
        for (seed, cq, width, height, ten_bit, vert, smooth) in arms {
            let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
            let source = if smooth {
                format!(
                    "nullsrc=size={width}x{height}:rate=25,format=yuv420p,\
                     geq=lum='128+90*sin((X+2*N)/37)+30*sin(Y/29)':\
                     cb='128+30*sin(X/23)':cr='128+30*sin((Y+N)/19)'"
                )
            } else {
                format!("mandelbrot=size={width}x{height}:rate=25:start_x=-0.6:start_y=-0.4")
            };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    pix,
                    "-strict",
                    "-1",
                    "-t",
                    "0.2",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "{NAME}: ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                cq,
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=128",
                "--min-partition-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-tx-size-search=0",
                "--deltaq-mode=0",
                "--limit=5",
            ];
            if ten_bit {
                args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
            }
            args.extend_from_slice(&["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let (before_edge, before_inter) = if vert {
                (
                    crate::decode::part128_edge_vert_hits(),
                    crate::decode::inter_sb128_vert_hits(),
                )
            } else {
                (
                    crate::decode::part128_edge_horz_hits(),
                    crate::decode::inter_sb128_horz_hits(),
                )
            };
            // A refusal is still a legal outcome for an arm whose stream hits
            // some OTHER lane's gap, but never for all of them (asserted
            // below), and never as a silent skip.
            if let Err(e) = decode_stream(&stream) {
                let msg = e.to_string();
                assert!(
                    msg.contains("unsupported"),
                    "{NAME} failed outright, not a named refusal \
                     (seed {seed} {width}x{height}): {msg}"
                );
                named_refusals += 1;
                eprintln!("{NAME}: seed {seed} {width}x{height} refusal: {msg}");
                continue;
            }
            let (frames, hidden) = decode_all_frames_vs_oracle(
                &stream,
                &format!("sb128c-edge-{seed}-{width}x{height}"),
            );
            assert!(frames > 0, "{NAME}: no frames decoded at {width}x{height}");
            let (edge, inter) = if vert {
                (
                    crate::decode::part128_edge_vert_hits() - before_edge,
                    crate::decode::inter_sb128_vert_hits() - before_inter,
                )
            } else {
                (
                    crate::decode::part128_edge_horz_hits() - before_edge,
                    crate::decode::inter_sb128_horz_hits() - before_inter,
                )
            };
            eprintln!(
                "{NAME}: seed {seed} cq {cq} {width}x{height} {}bit {}: \
                 {frames} frames pixel-exact ({hidden} hidden), \
                 edge_hits +{edge} inter_128rect_blocks +{inter}",
                if ten_bit { 10 } else { 8 },
                if vert { "VERT" } else { "HORZ" },
            );
            if edge > 0 {
                edge_arms += 1;
            }
            if inter > 0 {
                inter_arms += 1;
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals}); the gate never \
             decoded a stream"
        );
        assert!(
            edge_arms > 0,
            "{NAME}: {matched} stream(s) decoded pixel-exact but NOT ONE 128 root \
             took the gathered frame-edge HORZ/VERT arm this gate exists for"
        );
        assert!(
            inter_arms > 0,
            "{NAME}: the gathered arm fired only on key frames -- no INTER \
             128x64/64x128 block was decoded, which is the film's own shape"
        );
    }

    /// lane-sb128c r8: the 128 root's HORZ/VERT arm on a KEY frame -- one
    /// 128x64 (or 64x128) INTRA block, the shape r7 still refused by name.
    /// A smooth `gradients` source makes the key frame itself take the arm
    /// (`mandelbrot`'s key frames always split), at sizes whose last
    /// superblock row/column is a partial 64 so the gathered frame-edge bit
    /// decides the root. `--enable-tx-size-search=1` on half the arms is what
    /// makes the intra `tx_depth` symbol real (depth 1 = 32x32 units,
    /// depth 2 = 16x16). Every decode-order frame (hidden alt-refs included)
    /// is compared against the instrumented aomdec, and the per-depth
    /// counters are printed for every compared attempt.
    #[test]
    fn a_real_aomenc_sb128_intra_rect_block_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_sb128_intra_rect_block_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // (tag, cq, width, height, 10-bit, vertical twin, tx-size-search)
        let arms: Vec<(u32, &str, usize, usize, bool, bool, bool)> = vec![
            (42, "--cq-level=55", 384, 320, false, false, false),
            (43, "--cq-level=32", 384, 320, true, false, true),
            (44, "--cq-level=8", 256, 192, false, false, true),
            (46, "--cq-level=55", 192, 256, false, true, false),
            (45, "--cq-level=45", 192, 256, true, true, true),
            // lane-sb128c r10: a SECOND 128 superblock column is what the
            // arms above never had -- the right 64x128 half of a 128-root
            // VERT at mi_col 80 read its tx-depth symbol off the wrong CDF
            // row and every unit column of a 128x64 chunk predicted from the
            // not-yet-decoded chunk to its right.
            (47, "--cq-level=45", 512, 512, false, false, true),
        ];
        let (mut matched, mut named_refusals, mut out_of_scope) = (0u32, 0u32, 0u32);
        let (mut horz_arms, mut vert_arms) = (0u32, 0u32);
        let mut depth_seen = [0usize; 3];
        for (tag, cq, width, height, ten_bit, vert, tx_search) in arms {
            let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v", "error", "-f", "lavfi", "-i",
                    // DETERMINISTIC smooth source: lavfi `gradients` ignores
                    // its seed and gives a different picture every run (a
                    // whole round was spent on a "flake" that was really this),
                    // so the smoothness comes from long-period sinusoids
                    // through `geq` instead -- same effect on the encoder's
                    // partition choice, byte-identical across runs.
                    &format!(
                        "nullsrc=size={width}x{height}:rate=25,format=yuv420p,                         geq=lum='{lum}':                         cb='128+30*sin(X/23)':cr='128+30*sin((Y+N)/19)'",
                        // The 512x512 arm carries the r9 witness's own
                        // higher-frequency luma: the smooth two-term source
                        // gives every 128-rect block a whole-block TX_64X64
                        // and no VERT root at all, which is exactly the
                        // combination that hid the tx-depth CDF row defect.
                        lum = if width == 512 {
                            "128+80*sin((X+3*N)/17)+50*sin(Y/29)+30*sin((X+Y)/13)"
                        } else {
                            "128+90*sin((X+2*N)/37)+30*sin(Y/29)"
                        }
                    ),
                    "-pix_fmt", pix, "-strict", "-1", "-t", "0.2",
                    "-f", "yuv4mpegpipe", "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "{NAME}: ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                cq,
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=128",
                "--min-partition-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--deltaq-mode=0",
                "--limit=5",
                // EVERY frame a key frame: this gate exists for the INTRA
                // 128x64/64x128 block, and a mixed stream lands on other
                // lanes' inter gaps (sub-8 leaves, rect inter residual)
                // before it ever gets there. The inter twin of this same
                // 128-root arm has its own gate
                // (`..._sb128_gathered_edge_horz_partition_decodes_pixel_exact`).
                "--kf-max-dist=1",
            ];
            // AOMENC FLAG PRECEDENCE: the LAST occurrence wins, so this
            // per-arm override goes after the base recipe.
            args.push(if tx_search {
                "--enable-tx-size-search=1"
            } else {
                "--enable-tx-size-search=0"
            });
            if ten_bit {
                args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
            }
            args.extend_from_slice(&["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = intra_sb128_counters();
            if let Err(e) = decode_stream(&stream) {
                let msg = e.to_string();
                assert!(
                    msg.contains("unsupported"),
                    "{NAME} failed outright, not a named refusal \
                     (arm {tag} {width}x{height}): {msg}"
                );
                named_refusals += 1;
                eprintln!("{NAME}: arm {tag} {width}x{height} refusal: {msg}");
                continue;
            }
            let (frames, hidden) = decode_all_frames_vs_oracle(
                &stream,
                &format!("sb128c-intra-{tag}-{width}x{height}"),
            );
            assert!(frames > 0, "{NAME}: no frames decoded at {width}x{height}");
            let after = intra_sb128_counters();
            let (horz, vert_hits) = (after.0 - before.0, after.1 - before.1);
            let depth: Vec<usize> = (0..3).map(|i| after.2[i] - before.2[i]).collect();
            eprintln!(
                "{NAME}: arm {tag} cq {cq} {width}x{height} {}bit {} txsearch={}: \
                 {frames} frames pixel-exact ({hidden} hidden), \
                 intra_128x64 +{horz} intra_64x128 +{vert_hits} \
                 depth[64/32/16] {depth:?}",
                if ten_bit { 10 } else { 8 },
                if vert { "VERT" } else { "HORZ" },
                u8::from(tx_search),
            );
            if horz == 0 && vert_hits == 0 {
                out_of_scope += 1;
            }
            if horz > 0 {
                horz_arms += 1;
            }
            if vert_hits > 0 {
                vert_arms += 1;
            }
            for i in 0..3 {
                depth_seen[i] += depth[i];
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals}); the gate never \
             decoded a stream"
        );
        assert!(
            horz_arms > 0,
            "{NAME}: {matched} stream(s) decoded pixel-exact ({out_of_scope} carried \
             no 128-root rect intra block at all) but NOT ONE key-frame 128x64 \
             block fired -- the shape this gate exists for"
        );
        assert!(
            vert_arms > 0,
            "{NAME}: the VERT twin never produced a key-frame 64x128 block"
        );
        assert!(
            depth_seen[0] > 0,
            "{NAME}: no depth-0 (TX_64X64) intra 128-rect block: {depth_seen:?}"
        );
    }

    /// lane-sb128c r9 GATE: the four AB shapes at the 128 root
    /// (`PARTITION_HORZ_A`/`_HORZ_B`/`_VERT_A`/`_VERT_B`), each one
    /// 128x64/64x128 half plus two 64x64 quadrants in libaom
    /// `decode_partition`'s visit order. The recipe is r8's measured one
    /// (`--sb-size=128 --min-partition-size=64 --enable-ab-partitions=1` over
    /// a 512x512 `geq` sinusoid), which is what made aomenc pick the shapes at
    /// all; arms cover cq 18/45, 8 and 10 bit, and one INTER arm
    /// (`--kf-max-dist=100`) so the shapes are exercised on both tile paths.
    /// Continue-and-sweep: an arm that stops on ANOTHER lane's named refusal
    /// is counted and skipped, a decode failure or a pixel mismatch is a hard
    /// failure, and the end-of-sweep assert names which of the four fired.
    #[test]
    fn a_real_aomenc_sb128_ab_partition_at_the_128_root_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_sb128_ab_partition_at_the_128_root_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // (tag, cq, width, height, 10-bit, inter)
        let arms: Vec<(u32, &str, usize, usize, bool, bool)> = vec![
            (18, "--cq-level=18", 512, 512, false, false),
            (45, "--cq-level=45", 512, 512, false, false),
            (32, "--cq-level=32", 512, 512, true, false),
            (55, "--cq-level=55", 384, 320, false, false),
            (91, "--cq-level=45", 512, 512, false, true),
            (92, "--cq-level=18", 512, 512, false, true),
        ];
        let (mut matched, mut named_refusals, mut out_of_scope) = (0u32, 0u32, 0u32);
        let mut ab_seen = [0usize; 4];
        let mut ab_inter = [0usize; 4];
        for (tag, cq, width, height, ten_bit, inter) in arms {
            let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    // Same DETERMINISTIC sinusoid source that made aomenc pick
                    // the AB shapes in the r8 sweep (lavfi `gradients` ignores
                    // its seed and is not reproducible -- never use it here).
                    &format!(
                        "nullsrc=size={width}x{height}:rate=25,format=yuv420p,\
                         geq=lum='128+80*sin((X+3*N)/17)+50*sin(Y/29)+30*sin((X+Y)/13)':\
                         cb='128+30*sin(X/23)':cr='128+30*sin((Y+N)/19)'"
                    ),
                    "-pix_fmt",
                    pix,
                    "-strict",
                    "-1",
                    "-t",
                    "0.2",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "{NAME}: ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                cq,
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=128",
                "--min-partition-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=1",
                "--enable-1to4-partitions=0",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--deltaq-mode=0",
                "--limit=5",
            ];
            // AOMENC FLAG PRECEDENCE: last occurrence wins, so the per-arm
            // key-frame cadence goes AFTER the base recipe.
            args.push(if inter {
                "--kf-max-dist=100"
            } else {
                "--kf-max-dist=1"
            });
            if ten_bit {
                args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
            }
            args.extend_from_slice(&["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = sb128_ab_counters();
            if let Err(e) = decode_stream(&stream) {
                let msg = e.to_string();
                assert!(
                    msg.contains("unsupported"),
                    "{NAME} failed outright, not a named refusal \
                     (arm {tag} {width}x{height}): {msg}"
                );
                named_refusals += 1;
                eprintln!("{NAME}: arm {tag} {width}x{height} refusal: {msg}");
                continue;
            }
            let (frames, hidden) =
                decode_all_frames_vs_oracle(&stream, &format!("sb128c-ab-{tag}-{width}x{height}"));
            assert!(frames > 0, "{NAME}: no frames decoded at {width}x{height}");
            let after = sb128_ab_counters();
            let ab: Vec<usize> = (0..4).map(|i| after[i] - before[i]).collect();
            eprintln!(
                "{NAME}: arm {tag} cq {cq} {width}x{height} {}bit {}: {frames} frames \
                 pixel-exact ({hidden} hidden), AB[HORZ_A,HORZ_B,VERT_A,VERT_B] {ab:?}",
                if ten_bit { 10 } else { 8 },
                if inter { "INTER" } else { "KEY" },
            );
            if ab.iter().sum::<usize>() == 0 {
                out_of_scope += 1;
            }
            for i in 0..4 {
                ab_seen[i] += ab[i];
                if inter {
                    ab_inter[i] += ab[i];
                }
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals}); the gate never \
             decoded a stream"
        );
        assert!(
            ab_seen.iter().sum::<usize>() > 0,
            "{NAME}: {matched} stream(s) decoded pixel-exact ({out_of_scope} carried \
             no 128-root AB partition at all) but NOT ONE AB shape fired -- the \
             shapes this gate exists for"
        );
        // Which of the four this sweep actually reaches, hard-asserted so a
        // later change that stops reaching one of them fails here instead of
        // silently narrowing the gate (class `gate-blind-to-feature`).
        for (i, name) in ["HORZ_A", "HORZ_B", "VERT_A", "VERT_B"].iter().enumerate() {
            if AB_ARMS_REACHED[i] {
                assert!(
                    ab_seen[i] > 0,
                    "{NAME}: PARTITION_{name} at the 128 root fired in the round that \
                     pinned this gate but not now: {ab_seen:?}"
                );
            } else {
                eprintln!(
                    "{NAME}: PARTITION_{name} NOT reached by this sweep (aomenc never \
                     picked it): {ab_seen:?}"
                );
            }
        }
        assert!(
            ab_inter.iter().sum::<usize>() > 0,
            "{NAME}: the AB shapes fired only on KEY frames -- the inter tile path's \
             own AB arm was never exercised: key {ab_seen:?} inter {ab_inter:?}"
        );
    }

    #[test]
    #[ignore = "lane-sb128 r2 diagnostic: is the frame-6 mismatch sb128-specific?"]
    fn sb128_r2_control_sb64() {
        // lane-av1cmpaudit2: this control reaches the oracle, so it takes
        // the gate mutex like every other one.
        let _gate_lock = lock_gate_counters();
        let (width, height) = (256usize, 192usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!(
                    "{},noise=all_seed=53:alls=12:allf=t",
                    gradients_source(53, width, height, "duration=0.32:rate=25")
                ),
                "-pix_fmt",
                "yuv420p",
                "-t",
                "0.32",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg");
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=40",
                "--cpu-used=2",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--max-partition-size=64",
                "--min-partition-size=32",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-tx-size-search=0",
                "--deltaq-mode=0",
                "--limit=8",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m.stdout,
        );
        assert!(out.status.success());
        let (frames, hidden) = decode_all_frames_vs_oracle(&out.stdout, "sb128-control-64");
        eprintln!("control sb64: {frames} frames exact ({hidden} hidden)");
    }

    /// lane-sb128 r2: an INTER SEQUENCE under a 128x128 superblock -- the
    /// half ReelC is made of. r1 refused it by name (the inter tile loop
    /// recursed a 64x64 root only); r2 ported `sb_visit_order` and the 128
    /// root (per-superblock `read_lr`, the 8-symbol `partition_w128` read with
    /// its edge gather, `delta_q`/`delta_lf` at 128 granularity) into that
    /// loop, so this gate replaces the refusal test.
    ///
    /// Every frame in DECODE order, hidden alt-refs included
    /// ([`decode_all_frames_vs_oracle`], class gate-blind-to-hidden-frames):
    /// `--lag-in-frames` is left at aomenc's default so the encoder really
    /// emits them. Per-ATTEMPT counter deltas are hard asserts (class
    /// counter-from-refused-stream: the helper panics on any refusal, so every
    /// counted attempt is a compared one): 128 partition roots read on this
    /// stream, `cdef_idx` reads (one per 64x64 quadrant inside the 128
    /// superblock), and temporal MV candidates (the inter-only path). Both bit
    /// depths -- his films are `yuv420p10le`. Only a missing tool SKIPs.
    #[test]
    fn a_real_aomenc_inter_sequence_with_128x128_superblocks_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_128x128_superblocks_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc/aomdec");
            return;
        }
        let (width, height) = (256usize, 192usize);
        for ten_bit in [false, true] {
            let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!(
                        // Noise on top of the gradient is what makes aomenc
                        // signal a nonzero `cdef_bits` (a clean gradient codes
                        // with CDEF off, and the cdef_idx counter stays 0).
                        "{},noise=all_seed=53:alls=12:allf=t",
                        gradients_source(53, width, height, "duration=0.32:rate=25")
                    ),
                    "-pix_fmt",
                    pix,
                    "-strict",
                    "-1",
                    "-t",
                    "0.32",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "{NAME}: ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=40",
                "--cpu-used=2",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=128",
                "--max-partition-size=64",
                // aomenc emits PARTITION_HORZ_4/VERT_4 below 16x16 even with
                // the flag off (ledger, lane-modectx r1), so bound the search.
                "--min-partition-size=32",
                // The inter path still refuses rectangular strips ("a non-skip
                // rectangular (HORZ/VERT/HORZ_B) strip needs rectangular
                // residual coding") -- another lane's gap, off here so this
                // gate measures the 128 root and nothing else.
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-tx-size-search=0",
                "--deltaq-mode=0",
                "--limit=8",
            ];
            if ten_bit {
                args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
            }
            args.extend_from_slice(&["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let header = {
                let mut parser = ec_av1_syntax::Av1Parser::new();
                let mut pos = 0usize;
                while let Ok(obu) = parser.parse_obu(&stream[pos..]) {
                    pos += obu.total_size;
                    if pos >= stream.len() {
                        break;
                    }
                }
                parser
                    .sequence_header()
                    .map(|q| (q.use_128x128_superblock, q.color_config.bit_depth))
            };
            assert_eq!(
                header,
                Some((true, if ten_bit { 10 } else { 8 })),
                "{NAME}: the encoder did not write a 128x128-superblock sequence header at \
                 the requested bit depth -- the flag never arrived"
            );
            let before = (
                crate::decode::part128_symbols(),
                crate::decode::cdef_idx_hits(),
                crate::decode::tmv_hits(),
            );
            let (frames, hidden) = decode_all_frames_vs_oracle(
                &stream,
                &format!("sb128-inter-{}", if ten_bit { 10 } else { 8 }),
            );
            let (part128, cdef_idx, tmv) = (
                crate::decode::part128_symbols() - before.0,
                crate::decode::cdef_idx_hits() - before.1,
                crate::decode::tmv_hits() - before.2,
            );
            assert!(
                frames >= 6,
                "{NAME} ({pix}): only {frames} frames in decode order, the recipe asks for 8"
            );
            assert!(
                part128 >= 3,
                "{NAME} ({pix}): only {part128} 128-level partition symbols on this stream -- \
                 the 128 root never fired on the inter frames"
            );
            assert!(
                cdef_idx >= 1,
                "{NAME} ({pix}): no cdef_idx read -- the per-64x64-quadrant cdef read inside \
                 the 128 superblock never fired"
            );
            assert!(
                tmv >= 1,
                "{NAME} ({pix}): no temporal MV candidate -- the inter path never ran"
            );
            eprintln!(
                "{NAME} ({pix}): {frames} decode-order frames pixel-exact ({hidden} hidden), \
                 part128_symbols={part128} cdef_idx_hits={cdef_idx} tmv_hits={tmv}"
            );
        }
    }

    /// lane-sb128c r1: an INTER frame whose 128x128 superblock root really
    /// resolves `PARTITION_NONE` -- one whole 128x128 inter block, the shape
    /// ReelC's first inter frame stops this decoder on. r1 wired it: four
    /// TX_64X64 luma units (`max_txsize_rect_lookup[BLOCK_128X128]`), four
    /// TX_32X32 chroma units (`av1_get_max_uv_txsize`), interleaved Y,U,V per
    /// 64x64 "mu" chunk (libaom `decode_token_recon_block`) with the chroma
    /// `txb_skip_ctx` offset-10 rows, and OBMC's overlap capped at a 64x64
    /// block's half (`reconinter.c` 860/899).
    ///
    /// `--max-partition-size=128` is what lets the root be NONE at all (the
    /// sibling gate above pins 64, so its roots always SPLIT). Every arm hard
    /// asserts its own `inter_sb128_none_hits` delta, so a recipe that stops
    /// choosing the shape fails instead of passing vacuously; every decode
    /// order frame is compared by [`decode_all_frames_vs_oracle`], which
    /// panics on a refusal or any differing byte. Arms: 8-bit and 10-bit (his
    /// films are 10-bit) with `tx-size-search=0` (TX_MODE_LARGEST, the block's
    /// four TX_64X64 units come straight from `max_txsize_rect_lookup`) and an
    /// 8-bit `tx-size-search=1` arm (the var-tx tree at 128).
    // lane-sb128c r3: green. `--min-partition-size=32` let the RD search reach
    // 16x16-level AB / SB-level AB shapes that other lanes still refuse (two of
    // the three arms stopped there, measured); at 64 every arm decodes and the
    // 128 root still resolves NONE.
    #[test]
    fn a_real_aomenc_inter_128x128_none_root_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_inter_128x128_none_root_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc/aomdec");
            return;
        }
        let (width, height) = (256usize, 256usize);
        let mut intra128_total = 0usize;
        for (ten_bit, tx_search, seed) in
            [(false, false, 61u32), (true, false, 61), (false, true, 62)]
        {
            let pix = if ten_bit { "yuv420p10le" } else { "yuv420p" };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!(
                        "{},noise=all_seed={seed}:alls=6:allf=t",
                        gradients_source(seed, width, height, "duration=0.32:rate=25")
                    ),
                    "-pix_fmt",
                    pix,
                    "-strict",
                    "-1",
                    "-t",
                    "0.32",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "{NAME}: ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=62",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=128",
                "--max-partition-size=128",
                "--min-partition-size=64",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--deltaq-mode=0",
                "--limit=8",
            ];
            // Spelled ONCE per invocation (a repeated `--enable-*` keeps its
            // LAST occurrence, measured 2026-09-02).
            args.push(if tx_search {
                "--enable-tx-size-search=1"
            } else {
                "--enable-tx-size-search=0"
            });
            if ten_bit {
                args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
            }
            args.extend_from_slice(&["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let header = {
                let mut parser = ec_av1_syntax::Av1Parser::new();
                let mut pos = 0usize;
                while let Ok(obu) = parser.parse_obu(&stream[pos..]) {
                    pos += obu.total_size;
                    if pos >= stream.len() {
                        break;
                    }
                }
                parser
                    .sequence_header()
                    .map(|q| (q.use_128x128_superblock, q.color_config.bit_depth))
            };
            assert_eq!(
                header,
                Some((true, if ten_bit { 10 } else { 8 })),
                "{NAME}: the encoder did not write a 128x128-superblock sequence header at \
                 the requested bit depth -- the flag never arrived"
            );
            let before = (
                crate::decode::inter_sb128_none_hits(),
                crate::decode::chroma_split_tx_hits(),
                crate::decode::intra_128_in_inter_hits(),
            );
            let (frames, hidden) = decode_all_frames_vs_oracle(
                &stream,
                &format!(
                    "sb128c-inter-none-{}-{}",
                    if ten_bit { 10 } else { 8 },
                    tx_search
                ),
            );
            let (none128, chroma_units, intra128) = (
                crate::decode::inter_sb128_none_hits() - before.0,
                crate::decode::chroma_split_tx_hits() - before.1,
                crate::decode::intra_128_in_inter_hits() - before.2,
            );
            intra128_total += intra128;
            assert!(
                frames >= 6,
                "{NAME} ({pix} tx_search={tx_search}): only {frames} decode-order frames"
            );
            assert!(
                none128 >= 1,
                "{NAME} ({pix} tx_search={tx_search}): no 128x128 PARTITION_NONE root on an \
                 inter frame -- the recipe stopped exercising the shape this gate names"
            );
            eprintln!(
                "{NAME} ({pix} tx_search={tx_search}): {frames} decode-order frames pixel-exact \
                 ({hidden} hidden), inter_sb128_none_hits={none128} chroma_units={chroma_units} \
                 intra_128_in_inter={intra128}"
            );
        }
        assert!(
            intra128_total >= 1,
            "{NAME}: no INTRA-coded 128x128 block inside an inter frame across the three arms -- \
             the refusal this round lifted is no longer exercised"
        );
    }

    /// lane-av1-444-128: a real `aomenc` stream whose inter frames are ONE
    /// 128x128 PARTITION_NONE block each, non-skip, must walk that block's
    /// chroma through the mu-chunk loop -- one TX_32X32 unit per 64x64 mu
    /// chunk per plane at 4:2:0 (`decode_token_recon_block`'s mu walk with
    /// `av1_get_max_uv_txsize(BLOCK_128X128)` = `TX_32X32`), each unit read
    /// at its own origin against its own entropy-cell span and stamped
    /// immediately (`av1_set_entropy_contexts`). The census asserts the walk
    /// actually ran (four units per plane per coded 128-none block); the key
    /// frame splits to 64x64 (detailed testsrc2 content), so the whole
    /// stream stays inside the 4:2:0 paths this tree decodes. The 4:4:4 twin
    /// of this shape (FOUR TX_32X32 units per chunk per plane) is witnessed
    /// in `lanes/av1444128.report.md` and committed here as
    /// `a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact`.
    /// lane-av1422lift: the sequence-header refusal is gone, so this tree no
    /// longer refuses non-4:2:0 sequences by name.
    #[test]
    fn a_real_aomenc_128x128_none_inter_blocks_coded_chroma_per_mu_chunk_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_128x128_none_inter_blocks_coded_chroma_per_mu_chunk_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc/aomdec");
            return;
        }
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=128x128:rate=30",
                "-frames:v",
                "12",
                "-pix_fmt",
                "yuv420p",
                "-strict",
                "-1",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "{NAME}: ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let args: Vec<&str> = vec![
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=8",
            "--cpu-used=2",
            "--threads=1",
            "--row-mt=0",
            "--lag-in-frames=0",
            "--kf-max-dist=100",
            "--enable-fwd-kf=0",
            "--sb-size=128",
            "--max-partition-size=128",
            "--min-partition-size=64",
            "--enable-palette=0",
            "--enable-intrabc=0",
            "--enable-cdef=0",
            "--enable-restoration=0",
            "--loopfilter-control=0",
            "--enable-obmc=0",
            "--enable-warped-motion=0",
            "--enable-interintra-comp=0",
            "--enable-interintra-wedge=0",
            "--enable-smooth-interintra=0",
            "--enable-ref-frame-mvs=0",
            "--deltaq-mode=0",
            "--tile-columns=0",
            "--limit=12",
            "--obu",
            "-o",
            "-",
            "-",
        ];
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(
            out.status.success(),
            "{NAME}: aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let header = {
            let mut parser = ec_av1_syntax::Av1Parser::new();
            let mut pos = 0usize;
            while let Ok(obu) = parser.parse_obu(&stream[pos..]) {
                pos += obu.total_size;
                if pos >= stream.len() {
                    break;
                }
            }
            parser
                .sequence_header()
                .map(|q| (q.use_128x128_superblock, q.color_config.bit_depth))
        };
        assert_eq!(
            header,
            Some((true, 8)),
            "{NAME}: the encoder did not write a 128x128-superblock 8-bit sequence header -- \
             the flags never arrived"
        );
        let before = (
            crate::decode::inter_sb128_none_hits(),
            crate::decode::chroma_split_tx_hits(),
        );
        let (frames, hidden) = decode_all_frames_vs_oracle(&stream, "av1444128-128none-mu-chroma");
        let (none128, chroma_units) = (
            crate::decode::inter_sb128_none_hits() - before.0,
            crate::decode::chroma_split_tx_hits() - before.1,
        );
        assert!(frames >= 11, "{NAME}: only {frames} decode-order frames");
        assert!(
            none128 >= 8,
            "{NAME}: only {none128} inter 128x128 PARTITION_NONE roots -- the recipe stopped \
             exercising the shape this gate names"
        );
        assert!(
            chroma_units >= 8 * 8,
            "{NAME}: only {chroma_units} mu-chunk chroma units read -- the per-unit walk this \
             gate names never fired (four units per plane per coded 128-none inter block)"
        );
        eprintln!(
            "{NAME}: {frames} decode-order frames pixel-exact ({hidden} hidden), \
             inter_sb128_none_hits={none128} chroma_units={chroma_units}"
        );
    }

    /// lane-av1ibc128arm: the INTRA-in-inter 128x128 root's mu-chunk chroma
    /// arm (`decode_inter_block`'s `side > 64` arm, the intra half of the
    /// inter pair the sibling gate above names) must code the mu chunk's
    /// chroma as a PER-UNIT walk at every subsampling, and at 4:4:4 each
    /// 64x64 mu chunk is FOUR TX_32X32 units per plane, not one.
    ///
    /// **The defect.** The arm hardcoded `cu_tx = 32`, `luma_span = cu_tx *
    /// 2` and the chroma origin `cc * cu_tx` -- the 4:2:0 arithmetic, where a
    /// mu chunk's chroma plane block is `64 >> 1 == 32` square and the whole
    /// chunk IS one TX_32X32 unit. At 4:4:4 the chunk's chroma is 64x64, so
    /// the arm (a) read one 32x32 unit where libaom codes four, (b) stamped
    /// the unit's entropy context over the 4:2:0 64x64 luma span rather than
    /// the unit's own 32x32, and (c) placed chunk 1's unit at `cpx + 32`
    /// instead of `cpx + 64`, i.e. back inside chunk 0's samples.
    ///
    /// **`av1_get_max_uv_txsize` is NOT the source of a 64x64 chroma unit.**
    /// `blockd.h:1371` is `av1_get_adjusted_tx_size(max_txsize_rect_lookup[
    /// get_plane_block_size(bsize, ss_x, ss_y)])`, and `av1_get_adjusted_tx_size`
    /// (`blockd.h:1361`) takes NO subsampling argument -- it maps TX_64X64 /
    /// TX_64X32 / TX_32X64 to TX_32X32 UNCONDITIONALLY, so even at ss (0,0)
    /// (where `get_plane_block_size` returns BLOCK_128X128 and the lookup
    /// entry is TX_64X64) the answer is TX_32X32. Measured on this gate's own
    /// stream against the oracle's `EC_TRACE_COEFF`: every chroma read of the
    /// 128x128 intra-in-inter root is `tx_size=3` (TX_32X32), four per plane
    /// per mu chunk at (row,col) (0,0) (0,8) (8,0) (8,8).
    ///
    /// **The recipe** (128x256, 3 frames). `--sb-size=128` with
    /// `--min-partition-size=64 --max-partition-size=128` keeps the
    /// superblock root resolvable to `PARTITION_NONE`, and the noise-on-
    /// gradient content is what makes the encoder's RD pick INTRA over inter
    /// for a whole 128 root on an inter frame (a clean `testsrc2` stream
    /// never does -- measured over 24 recipe/cell combinations, three of
    /// which fired and none of them `testsrc2`):
    /// ```text
    /// ffmpeg -f lavfi -i "gradients=size=128x256:c0=..:c1=..:c2=..:c3=..:\
    /// seed=63:duration=0.12:rate=25,noise=all_seed=63:alls=6:allf=t" \
    ///        -pix_fmt yuv444p -t 0.12 -f yuv4mpegpipe - | \
    /// aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q --cq-level=62 \
    ///        --cpu-used=0 --threads=1 --row-mt=0 --sb-size=128 --limit=3 \
    ///        --max-partition-size=128 --min-partition-size=64 \
    ///        --enable-rect-partitions=0 --enable-ab-partitions=0 \
    ///        --enable-1to4-partitions=0 --enable-palette=0 \
    ///        --enable-intrabc=0 --deltaq-mode=0 --enable-tx-size-search=0 \
    ///        --obu -o out.obu -
    /// ```
    /// Both `rate=25` and the frame limit are load-bearing: `gradients`' and
    /// `noise`' patterns are time-parameterised, so a different rate or a
    /// different `--limit` emits a different stream (and at `--limit=2` this
    /// recipe stops choosing the shape at all -- measured, 0 fires).
    ///
    /// Non-vacuity, three ways: the sequence header is asserted really to be
    /// 4:4:4 (`subsampling_x == subsampling_y == 0`) and 128-superblock;
    /// `intra_128_in_inter_mu_chroma_hits()` must be non-zero, so the arm
    /// really ran; and every decode-order frame is compared byte-for-byte
    /// against the oracle by [`decode_all_frames_vs_oracle`], which panics on
    /// a refusal or any differing byte. Red before the fix (the ss-aware
    /// arithmetic and the per-unit walk reverted, the counter kept): decode-
    /// order frame 2 differs in 44649 of 98304 bytes while the counter still
    /// reads 4 -- the red is on PIXELS, not on the counter.
    #[test]
    fn a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc/aomdec");
            return;
        }
        let (width, height) = (128usize, 256usize);
        let source = format!(
            "{},noise=all_seed=63:alls=6:allf=t",
            gradients_source(63, width, height, "duration=0.12:rate=25")
        );
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-pix_fmt",
                "yuv444p",
                "-t",
                "0.12",
                "-f",
                "yuv4mpegpipe",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            y4m.status.success(),
            "{NAME}: ffmpeg fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let args: Vec<&str> = vec![
            "--codec=av1",
            "--profile=1",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=62",
            "--cpu-used=0",
            "--threads=1",
            "--row-mt=0",
            "--sb-size=128",
            "--limit=3",
            "--max-partition-size=128",
            "--min-partition-size=64",
            "--enable-rect-partitions=0",
            "--enable-ab-partitions=0",
            "--enable-1to4-partitions=0",
            "--enable-palette=0",
            "--enable-intrabc=0",
            "--deltaq-mode=0",
            "--enable-tx-size-search=0",
            "--obu",
            "-o",
            "-",
            "-",
        ];
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(
            out.status.success(),
            "{NAME}: aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        // The gate is only about 4:4:4 at a 128 superblock, so assert the
        // parsed sequence header really is that before anything else: a
        // stream that silently came back 4:2:0 would leave this arm
        // unexercised while every other assert below still passed.
        let header = {
            let mut parser = ec_av1_syntax::Av1Parser::new();
            let mut pos = 0usize;
            while let Ok(obu) = parser.parse_obu(&stream[pos..]) {
                pos += obu.total_size;
                if pos >= stream.len() {
                    break;
                }
            }
            parser.sequence_header().map(|q| {
                (
                    q.use_128x128_superblock,
                    q.color_config.subsampling_x,
                    q.color_config.subsampling_y,
                    q.color_config.mono_chrome,
                )
            })
        };
        assert_eq!(
            header,
            Some((true, 0, 0, false)),
            "{NAME}: the encoder did not write a 4:4:4 (ss 0/0, non-mono) 128x128-superblock \
             sequence header -- the flags never arrived, so the arm this gate names is not \
             the one under test"
        );
        let before = (
            crate::decode::intra_128_in_inter_hits(),
            crate::decode::intra_128_in_inter_mu_chroma_hits(),
        );
        let (frames, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
        let (roots, units) = (
            crate::decode::intra_128_in_inter_hits() - before.0,
            crate::decode::intra_128_in_inter_mu_chroma_hits() - before.1,
        );
        assert!(
            frames >= 3,
            "{NAME}: only {frames} decode-order frames ({hidden} hidden), the recipe asks for 3"
        );
        assert!(
            roots >= 1,
            "{NAME}: no INTRA-coded 128x128 root inside an inter frame -- the recipe stopped \
             exercising the shape this gate names"
        );
        // One mu chunk per arm firing, and at 4:4:4 FOUR units per plane per
        // chunk (two 64x64 mu chunks per 128 root, two planes each).
        assert!(
            units >= 4,
            "{NAME}: only {units} mu-chunk chroma reads across {roots} intra-in-inter 128 \
             root(s) -- the per-unit walk this gate names never fired (a 4:4:4 64x64 mu chunk \
             is FOUR TX_32X32 units per plane, not one)"
        );
        eprintln!(
            "{NAME}: {frames} decode-order frames pixel-exact ({hidden} hidden), \
             intra_128_in_inter={roots} mu_chunk_chroma_reads={units}"
        );
    }

    /// lane-av1sb128chroma: the PINNED half of
    /// [`a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact`].
    /// That gate re-encodes its fixture on every run, so the arm it names had
    /// NO committed 4:4:4 bytes at all: a host without ffmpeg/aomenc silently
    /// skips it, and the whole committed 4:4:4 base is lossless 8-bit, so
    /// nothing else in the crate decoded one of these streams. This gate is
    /// the pin, and it also carries the COUNTER-PROOF arm: the recipe a
    /// proposed 4:4:4 `--sb-size=128` chroma repair was argued from reads
    /// ZERO on both the arm's own counters, so its green is vacuous.
    ///
    /// **Witness** `fixtures/444_intra_in_inter_128root_mu_chroma.obu`,
    /// sha256 `f5999bd996fef820472dc7846e42cad3e62bd1f816fafd7f218e34b77786394a`,
    /// 3570 bytes, 3 decode-order frames. Recipe (verified byte-reproducible):
    /// ```text
    /// ffmpeg -v error -f lavfi -i "gradients=size=128x256:c0=0xa1128f:c1=0xd98c48:\
    /// c2=0x120601:c3=0x4abfba:seed=63:duration=0.12:rate=25,\
    /// noise=all_seed=63:alls=6:allf=t" -pix_fmt yuv444p -t 0.12 -f yuv4mpegpipe - > in.y4m
    /// aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q --cq-level=62 \
    ///        --cpu-used=0 --threads=1 --row-mt=0 --sb-size=128 --limit=3 \
    ///        --max-partition-size=128 --min-partition-size=64 \
    ///        --enable-rect-partitions=0 --enable-ab-partitions=0 \
    ///        --enable-1to4-partitions=0 --enable-palette=0 --enable-intrabc=0 \
    ///        --deltaq-mode=0 --enable-tx-size-search=0 --obu -o out.obu in.y4m
    /// ```
    /// `duration=0.12:rate=25` and `--limit=3` are load-bearing (the
    /// `gradients`/`noise` patterns are time-parameterised, and at `--limit=2`
    /// the encoder stops choosing INTRA over inter for a whole 128 root).
    ///
    /// **Measured on this witness** (unpatched `main` `57834ee2`): the arm
    /// fires `intra_128_in_inter_hits = 1` root and
    /// `intra_128_in_inter_mu_chroma_hits = 4` mu-chunk chroma reads (a 4:4:4
    /// 64x64 mu chunk is FOUR TX_32X32 units per plane, two chunks, one
    /// plane reached before the other), and all 3 frames are byte-exact
    /// against the oracle. The claimed `cu_tx = 64` repair is refuted by two
    /// independent measurements: (a) with `cu_tx` at 64 this arm PANICS --
    /// `no default scan for a 64-point transform` at `decode.rs:7197`, because
    /// libaom's `av1_get_adjusted_tx_size` (blockd.h:1361) maps TX_64X64 to
    /// TX_32X32 unconditionally, so a 4:4:4 chroma unit is never 64 and
    /// `av1_scan_orders` has no 64-point chroma table; (b) with only the
    /// luma-span and per-chunk-origin halves of that repair applied (`cu_tx`
    /// left at 32 so the mutation reaches the pixels), this same witness goes
    /// RED: Y 12537, U 16696, V 16667 wrong samples of 98304 per plane,
    /// first at Y(90,120) ours 142 vs oracle 141, U(65,120) 153 vs 152,
    /// V(64,120) 145 vs 146 -- all of it on decode-order frame 2, and the
    /// counters still read 1/4, so the damage is on PIXELS.
    #[test]
    fn a_444_intra_in_inter_128root_muchroma_pinned_stream_decodes_pixel_exact() {
        const NAME: &str =
            "a_444_intra_in_inter_128root_muchroma_pinned_stream_decodes_pixel_exact";
        const WITNESS_LEN: usize = 3570;
        const WITNESS_FNV: u64 = 0xe455_060d_f685_86dd;
        const CONTROL_LEN: usize = 31602;
        const CONTROL_FNV: u64 = 0xc542_f45f_f3a5_d494;
        const W: usize = 128;
        const H: usize = 256;
        const FRAMES: usize = 3;
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let read_pinned = |name: &str, len: usize, fnv: u64| -> Vec<u8> {
            let path = dir.join(name);
            let bytes = std::fs::read(&path).unwrap_or_else(|e| {
                panic!(
                    "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                     which is a failure, not a skip",
                    path.display()
                )
            });
            assert_eq!(bytes.len(), len, "{NAME}: {name} length moved");
            assert_eq!(fnv1a64(&bytes), fnv, "{NAME}: {name} bytes moved");
            bytes
        };
        let stream = read_pinned(
            "444_intra_in_inter_128root_mu_chroma.obu",
            WITNESS_LEN,
            WITNESS_FNV,
        );

        let _gate_lock = lock_gate_counters();
        // The shape claim first: a stream that silently came back 4:2:0 would
        // leave the arm unexercised while every other assert still passed.
        let header = {
            let mut parser = ec_av1_syntax::Av1Parser::new();
            let mut pos = 0usize;
            while let Ok(obu) = parser.parse_obu(&stream[pos..]) {
                pos += obu.total_size;
                if pos >= stream.len() {
                    break;
                }
            }
            parser.sequence_header().map(|q| {
                (
                    q.use_128x128_superblock,
                    q.color_config.subsampling_x,
                    q.color_config.subsampling_y,
                    q.color_config.mono_chrome,
                )
            })
        };
        assert_eq!(
            header,
            Some((true, 0, 0, false)),
            "{NAME}: the witness is not a 4:4:4 (ss 0/0, non-mono) 128x128-superblock stream \
             -- {header:?}, so the arm under test is not the one that ran"
        );
        let before = (
            crate::decode::intra_128_in_inter_hits(),
            crate::decode::intra_128_in_inter_mu_chroma_hits(),
        );
        let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
        let (roots, units) = (
            crate::decode::intra_128_in_inter_hits() - before.0,
            crate::decode::intra_128_in_inter_mu_chroma_hits() - before.1,
        );
        assert_eq!(decoded, FRAMES, "{NAME}: decode-order frame count");
        // At 4:4:4 the chroma planes are full resolution; a decoder that kept
        // the 4:2:0 extent would hand quarter-size chroma to the byte compare,
        // which cannot pass. Say it here rather than let the compare stand in
        // for the shape claim.
        for f in crate::stream::decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the witness no longer decodes cleanly: {e}"))
        {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: frame dimensions");
            assert_eq!(
                f.u.len(),
                W * H,
                "{NAME}: chroma plane is {} samples",
                f.u.len()
            );
        }
        assert!(
            roots >= 1,
            "{NAME}: no INTRA-coded 128x128 root inside an inter frame -- the arm under \
             test never ran"
        );
        assert!(
            units >= 4,
            "{NAME}: only {units} mu-chunk chroma reads across {roots} intra-in-inter 128 \
             root(s) -- a 4:4:4 64x64 mu chunk is FOUR TX_32X32 units per plane, so fewer \
             than four means the per-unit walk this gate names did not run"
        );
        eprintln!(
            "{NAME}: {decoded} decode-order frame(s) pixel-exact ({hidden} hidden), \
             intra_128_in_inter={roots} mu_chunk_chroma_reads={units}"
        );

        // The COUNTER-PROOF arm. The 4:4:4 `--sb-size=128 --cq-level=20`
        // `testsrc2` recipe a 64-point-chroma repair was argued from is
        // byte-exact on all 24 frames AND reads ZERO on both of the arm's
        // counters: on clean `testsrc2` the encoder's RD never picks INTRA
        // over inter for a whole 128 root, so that stream cannot witness this
        // arm at all. Pinning it keeps the refutation reproducible in-crate
        // with no encoder on the host.
        let control = read_pinned("444_sb128cq20_tsrc2_control.obu", CONTROL_LEN, CONTROL_FNV);
        let ctl_header = {
            let mut parser = ec_av1_syntax::Av1Parser::new();
            let mut pos = 0usize;
            while let Ok(obu) = parser.parse_obu(&control[pos..]) {
                pos += obu.total_size;
                if pos >= control.len() {
                    break;
                }
            }
            parser.sequence_header().map(|q| {
                (
                    q.use_128x128_superblock,
                    q.color_config.subsampling_x,
                    q.color_config.subsampling_y,
                )
            })
        };
        assert_eq!(
            ctl_header,
            Some((true, 0, 0)),
            "{NAME}: the control stream is not the 4:4:4 128-superblock stream its recipe \
             names -- {ctl_header:?}"
        );
        let ctl_before = (
            crate::decode::intra_128_in_inter_hits(),
            crate::decode::intra_128_in_inter_mu_chroma_hits(),
        );
        let ctl_frames = crate::stream::decode_stream(&control)
            .unwrap_or_else(|e| panic!("{NAME}: the control stream no longer decodes: {e}"))
            .len();
        let ctl = (
            crate::decode::intra_128_in_inter_hits() - ctl_before.0,
            crate::decode::intra_128_in_inter_mu_chroma_hits() - ctl_before.1,
        );
        assert_eq!(ctl_frames, 24, "{NAME}: control frame count");
        assert_eq!(
            ctl,
            (0, 0),
            "{NAME}: the control stream now reaches the arm ({ctl:?}) -- the witness's \
             non-vacuity argument rested on `testsrc2` never choosing an \
             intra-coded 128 root, and that is no longer true, so the two \
             fixtures no longer separate the cells"
        );
        if aomdec_available(NAME) {
            let (ctl_decoded, ctl_hidden) = decode_all_frames_vs_oracle(&control, NAME);
            assert_eq!(ctl_decoded, 24, "{NAME}: control decode-order frame count");
            eprintln!("{NAME}: control {ctl_decoded} frame(s) pixel-exact ({ctl_hidden} hidden)");
        }
    }

    /// lane-av1muchunk422b: the **compound** `side > 64` mu-chunk walk --
    /// per-unit chroma allocation of `decode_inter_block`'s compound arm, site
    /// 2 of `lanes/av1muchunk422.report.md`, which that lane's recipes all
    /// missed (`part128 none=0` on a 384x256 frame).
    ///
    /// Reachability, shared with the intra-in-inter twin below: the streams
    /// that reach this walk are 128-as-the-whole-frame (`--sb-size=128
    /// --min-partition-size=128 --max-partition-size=128`, PARTITION_NONE
    /// forced; here `part128 none=7`) and LOSSY, which is load-bearing: each arm
    /// has a lossless branch that returns before the unit loop, and
    /// `W_intrabc.obu` reaches both guards at ss (1,0) allocating nothing. That
    /// is why [`crate::decode::mu_chunk_compound_units`] counts at the
    /// ALLOCATION and not at the guard; the counter-proof at the end of the
    /// intra test pins that gap. The single-reference twin of site 2 (site 5)
    /// shares this exact source text and is routed out of the counter by
    /// `is_compound`.
    ///
    /// **7 of the 8 streams this lane generated DIVERGED from ffmpeg; only
    /// `i6_grad128_iis` (the intra-in-inter fixture, pinned in the twin test
    /// below) was byte-exact.** lane-av1muchunk422sym has since fixed this
    /// pin's stream: the compound mu-chunk walk gathered its CHROMA
    /// coefficient context with the plain rect rule, which at ss (1, 0) counts
    /// one chroma column twice in the above vote (see the walk's own comment).
    /// **The stream is now byte-exact against the instrumented aomdec over all
    /// 7 decode-order frames (6 shown + 1 hidden alt-ref)**, so this test
    /// carries both halves: the counter witness that the walk ran AND the
    /// exactness witness. The pinned V-plane hashes run without an oracle;
    /// `decode_all_frames_vs_oracle` adds the decode-order compare when one is
    /// present. The counter half still stops this fixture silently ceasing to
    /// reach the walk it was pinned for.
    ///
    /// Recipe (byte-reproducible; whole aomenc argv):
    /// ```text
    /// ffmpeg -v error -f lavfi -i "testsrc2=size=128x128:rate=25" -t 0.24 \
    ///   -pix_fmt yuv422p -f yuv4mpegpipe - > tsrc128.y4m
    /// aomenc --codec=av1 --profile=2 --input-chroma-subsampling-x=1 \
    ///   --input-chroma-subsampling-y=0 --passes=1 --end-usage=q --cq-level=62 \
    ///   --cpu-used=0 --threads=1 --row-mt=0 --sb-size=128 \
    ///   --min-partition-size=128 --max-partition-size=128 \
    ///   --enable-rect-partitions=0 --enable-ab-partitions=0 \
    ///   --enable-1to4-partitions=0 --enable-palette=0 --enable-intrabc=0 \
    ///   --deltaq-mode=0 --enable-tx-size-search=0 --enable-interintra-comp=1 \
    ///   --enable-interintra-wedge=1 --enable-smooth-interintra=1 --obu -o X
    /// ```
    #[test]
    fn a_422_muchunk_compound_128root_pinned_stream_reaches_its_unit_walk() {
        const NAME: &str = "a_422_muchunk_compound_128root_pinned_stream_reaches_its_unit_walk";
        const FILE: &str = "422_muchunk_compound_128root.obu";
        const LEN: usize = 895;
        const FNV: u64 = 0xfb28_f9a7_cf23_9897;
        const MIN_UNITS: usize = 32;
        // lane-av1muchunk422sym: shown frame 2's V plane byte-for-byte (it is
        // the first intrinsically wrong frame; the pre-fix square gather put
        // 2138 of these 8192 samples off by up to 4), and shown frame 0's as
        // the unaffected control. 0xa0e5_8bf4_207b_29ba is the ffmpeg/aomdec
        // exact plane.
        const SHOWN_F2_V_FNV: u64 = 0xa0e5_8bf4_207b_29ba;
        const SHOWN_F0_V_FNV: u64 = 0xcc5e_3291_1915_9024;
        /// Chroma (36, 60) of shown frame 2 -- the FIRST sample that moved,
        /// `60 * 64 + 36`. The pre-fix square gather read 113, the oracle 112.
        const FIRST_WRONG_V: usize = 3876;
        const SHOWN_F2_V_AT_FIRST_WRONG: u16 = 112;
        let _gate_lock = lock_gate_counters();
        let stream = read_pinned_muchunk_fixture(FILE, LEN, FNV, NAME);
        assert_eq!(
            obu_stream_shape(&stream),
            Some((true, 1, 0, false)),
            "{NAME}: {FILE} is not a 4:2:2 (ss 1/0, non-mono) 128x128-superblock stream"
        );
        let before = (
            crate::decode::mu_chunk_compound_units(),
            crate::decode::mu_chunk_intra_in_inter_units(),
        );
        let shown = crate::stream::decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: {FILE} no longer decodes cleanly: {e}"));
        let frames = shown.len();
        let (compound, intra) = (
            crate::decode::mu_chunk_compound_units() - before.0,
            crate::decode::mu_chunk_intra_in_inter_units() - before.1,
        );
        assert_eq!(frames, 6, "{NAME}: {FILE} shown-frame count");
        assert!(
            compound >= MIN_UNITS,
            "{NAME}: {FILE} allocated only {compound} compound `side > 64` mu-chunk units \
             (expected >= {MIN_UNITS}) -- the walk this gate names did not run"
        );
        assert_eq!(
            intra, 0,
            "{NAME}: {FILE} also ran the intra-in-inter walk ({intra} units) -- the two fixtures \
             no longer separate the cells"
        );
        // lane-av1muchunk422sym: the EXACTNESS half the diagnostic lane handed
        // over ("when a fix lane lands, add the oracle compare to this test").
        // The fix routes this walk's CHROMA context gather through the 4:2:2
        // pair rule (`around_mi_422_chroma`): at ss (1, 0) one chroma column
        // spans two luma mi columns, so the plain rect gather summed the unit's
        // above dc sign twice and `dc_sign_ctx`'s signum flipped whenever above
        // and left cancelled. The first wrong symbol was the DC SIGN of the V
        // unit at chunk (1, 1) of decode frame 2 -- ours read
        // `dc_sign_cdf[1]`, the oracle row 0.
        //
        // The pinned half below needs NO oracle and is therefore the one that
        // cannot silently skip: shown frame 2 (the first intrinsically wrong
        // frame) is asserted sample-for-sample on its V plane. Frame 0's is
        // asserted too, as the unaffected control that fails if the gather is
        // "fixed" past its own cell.
        assert_eq!(
            shown[0].v.len(),
            64 * 128,
            "{NAME}: {FILE} is no longer 4:2:2 -- the V plane shape moved"
        );
        assert_eq!(
            fnv1a64(&shown[2].v.iter().map(|&s| s as u8).collect::<Vec<u8>>()),
            SHOWN_F2_V_FNV,
            "{NAME}: {FILE} shown frame 2's V plane moved -- the compound mu-chunk walk's 4:2:2 \
             chroma context gather is wrong again (first sample to move is chroma (36, 60), index \
             3876, expected {})",
            SHOWN_F2_V_AT_FIRST_WRONG
        );
        assert_eq!(
            shown[2].v[FIRST_WRONG_V], SHOWN_F2_V_AT_FIRST_WRONG,
            "{NAME}: {FILE} shown frame 2 V[3876] (chroma (36, 60)) is {} -- the pre-fix square \
             gather read 113 here",
            shown[2].v[FIRST_WRONG_V]
        );
        assert_eq!(
            fnv1a64(&shown[0].v.iter().map(|&s| s as u8).collect::<Vec<u8>>()),
            SHOWN_F0_V_FNV,
            "{NAME}: {FILE} shown frame 0's V plane moved -- the 4:2:2 routing changed a frame it \
             must not touch"
        );
        if aomdec_available(NAME) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert_eq!(decoded, 7, "{NAME}: {FILE} decode-order frame count");
            eprintln!(
                "{NAME}: {FILE} byte-exact over {decoded} decode-order frame(s) ({hidden} hidden), \
                 compound_units={compound} intra_in_inter_units={intra}"
            );
        } else {
            eprintln!(
                "{NAME}: {FILE} compound_units={compound} intra_in_inter_units={intra} -- the \
                 decode-order oracle compare above did NOT run (no aomdec)"
            );
        }
    }

    /// lane-av1muchunk422b: the **intra-in-inter** `side > 64` mu-chunk walk --
    /// per-unit chroma allocation of the intra-in-inter arm (site 8 of
    /// `lanes/av1muchunk422.report.md`). This is the ONE stream of the eight
    /// this lane generated that is BYTE-EXACT against the ffmpeg/aom oracle
    /// (`i6_grad128_iis`), so unlike its compound twin this gate asserts
    /// pixel-exactness over every decode-order frame AND that the site counter
    /// moved. See the compound test above for the shared reachability and
    /// count-at-the-ALLOCATION argument.
    ///
    /// Recipe (byte-reproducible; same aomenc argv as the compound test except
    /// for the source):
    /// ```text
    /// ffmpeg -v error -f lavfi -i "gradients=size=128x256:c0=0xa1128f:\
    ///   c1=0xd98c48:c2=0x120601:c3=0x4abfba:seed=63:duration=0.24:rate=25,\
    ///   noise=all_seed=63:alls=6:allf=t" -t 0.24 -pix_fmt yuv422p \
    ///   -f yuv4mpegpipe - > grad256.y4m
    /// aomenc --codec=av1 --profile=2 --input-chroma-subsampling-x=1 \
    ///   --input-chroma-subsampling-y=0 --passes=1 --end-usage=q --cq-level=62 \
    ///   --cpu-used=0 --threads=1 --row-mt=0 --sb-size=128 \
    ///   --min-partition-size=128 --max-partition-size=128 \
    ///   --enable-rect-partitions=0 --enable-ab-partitions=0 \
    ///   --enable-1to4-partitions=0 --enable-palette=0 --enable-intrabc=0 \
    ///   --deltaq-mode=0 --enable-tx-size-search=0 --enable-interintra-comp=1 \
    ///   --enable-interintra-wedge=1 --enable-smooth-interintra=1 --obu -o X
    /// ```
    #[test]
    fn a_422_muchunk_intra_in_inter_128root_pinned_stream_is_exact_and_reaches_its_unit_walk() {
        const NAME: &str =
            "a_422_muchunk_intra_in_inter_128root_pinned_stream_is_exact_and_reaches_its_unit_walk";
        const FILE: &str = "422_muchunk_intra_in_inter_128root.obu";
        const LEN: usize = 391;
        const FNV: u64 = 0xe4d7_2c19_5a11_1f4f;
        const MIN_UNITS: usize = 16;
        // 6 shown frames + the 1 hidden alt-ref frame the oracle also emits.
        const DECODED: usize = 7;
        let _gate_lock = lock_gate_counters();
        let stream = read_pinned_muchunk_fixture(FILE, LEN, FNV, NAME);
        assert_eq!(
            obu_stream_shape(&stream),
            Some((true, 1, 0, false)),
            "{NAME}: {FILE} is not a 4:2:2 (ss 1/0, non-mono) 128x128-superblock stream"
        );
        let before = (
            crate::decode::mu_chunk_compound_units(),
            crate::decode::mu_chunk_intra_in_inter_units(),
        );
        let frames = crate::stream::decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: {FILE} no longer decodes cleanly: {e}"))
            .len();
        let (compound, intra) = (
            crate::decode::mu_chunk_compound_units() - before.0,
            crate::decode::mu_chunk_intra_in_inter_units() - before.1,
        );
        assert_eq!(frames, 6, "{NAME}: {FILE} shown-frame count");
        assert!(
            intra >= MIN_UNITS,
            "{NAME}: {FILE} allocated only {intra} intra-in-inter `side > 64` mu-chunk units \
             (expected >= {MIN_UNITS}) -- the site counter this gate names did not move"
        );
        assert_eq!(
            compound, 0,
            "{NAME}: {FILE} also ran the compound walk ({compound} units) -- the two fixtures no \
             longer separate the cells"
        );
        if aomdec_available(NAME) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert_eq!(decoded, DECODED, "{NAME}: {FILE} decode-order frame count");
            eprintln!(
                "{NAME}: {FILE} byte-exact over {decoded} decode-order frame(s) ({hidden} \
                 hidden), intra_in_inter_units={intra}"
            );
        }

        // The counter-proof: the LOSSLESS 4:2:2 stream that reaches both
        // `side > 64` guards at ss (1,0) and allocates nothing. `W_intrabc`
        // is the fixture the prior lane measured `site 2 = 0` on, and this
        // pins why that zero was real -- the arm's lossless branch returns
        // before the unit loop.
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let intrabc = std::fs::read(dir.join("W_intrabc.obu"))
            .expect("W_intrabc.obu is missing -- the counter-proof cannot run");
        let proof_before = (
            crate::decode::mu_chunk_compound_units(),
            crate::decode::mu_chunk_intra_in_inter_units(),
            crate::decode::intra_128_in_inter_mu_chroma_hits(),
        );
        let proof_frames = crate::stream::decode_stream(&intrabc)
            .expect("W_intrabc.obu no longer decodes")
            .len();
        let proof = (
            crate::decode::mu_chunk_compound_units() - proof_before.0,
            crate::decode::mu_chunk_intra_in_inter_units() - proof_before.1,
            crate::decode::intra_128_in_inter_mu_chroma_hits() - proof_before.2,
        );
        assert_eq!(proof_frames, 16, "{NAME}: W_intrabc shown-frame count");
        assert_eq!(
            (proof.0, proof.1),
            (0, 0),
            "{NAME}: W_intrabc now allocates mu-chunk units ({}, {}) -- it cannot be the \
             counter-proof any more",
            proof.0,
            proof.1
        );
        assert!(
            proof.2 > 0,
            "{NAME}: W_intrabc no longer reaches the intra-in-inter guard at all \
             (INTRA_128_IN_INTER_MU_CHROMA_HITS delta 0), so its zero allocations prove \
             nothing about the guard-to-allocation gap this gate rests on"
        );
        eprintln!(
            "{NAME}: counter-proof W_intrabc allocations=({}, {}) with the intra-in-inter guard \
             fired {} time(s)",
            proof.0, proof.1, proof.2
        );
    }

    /// lane-av1muchunk422iis: the **intra-in-inter** `side > 64` mu-chunk
    /// walk at 128x256 (TWO 128-superblock rows), site 8's second stream.
    /// lane-av1muchunk422class measured this stream diverging with the class
    /// signature -- V-plane-only diffs, first wrong sample V (row 190, col 36)
    /// of shown frame 2 (V index `190 * 64 + 36 = 12196`, ours 147 vs
    /// ffmpeg's 146), a byte-identical decode before and after the compound
    /// fix, and the SAME `dc_sign_ctx` fork the compound fix targeted, on the
    /// TX_32X32 chroma unit of THIS walk. The walk gathered its chroma
    /// context with the plain luma `around_mi` (decode.rs:46820), which at
    /// ss (1, 0) sums `unit_luma_w / MI` above cells where libaom sums
    /// `txb_w_unit` -- twice as many -- flipping the signum whenever the
    /// doubled above vote cancels the left one (see the walk's own comment).
    ///
    /// The fixture is the hunt's `i5_grad256_iis.obu` (sha256
    /// `64794f278eee738dcc59d90dc0c7c4ef2828c5b7c13724760a33be11f224908e`),
    /// pinned byte-for-byte. Recipe: same aomenc argv as the 128x128 twin
    /// below, source `gradients=size=128x256:...:seed=63:duration=0.24:rate=25,
    /// noise=all_seed=63:alls=6:allf=t` (NOT re-encoded by this lane; the
    /// bytes are the hunted bytes).
    ///
    /// Three halves, in order: (a) the site counter moved AND this stream
    /// never enters the compound walk; (b) an ORACLE-FREE V-plane pin of
    /// shown frame 2 plus the one sample that moved, so the exactness half
    /// cannot silently skip; (c) the required ffmpeg compare -- every plane
    /// of every shown frame byte-exact -- and the decode-order oracle compare
    /// when one is available.
    #[test]
    fn a_422_muchunk_intra_in_inter_128x256_stream_is_exact_and_reaches_its_unit_walk() {
        const NAME: &str =
            "a_422_muchunk_intra_in_inter_128x256_stream_is_exact_and_reaches_its_unit_walk";
        const FILE: &str = "422_muchunk_intra_in_inter_128x256.obu";
        const LEN: usize = 667;
        const FNV: u64 = 0x2bee_eba3_052b_f9a3;
        const MIN_UNITS: usize = 96;
        // 6 shown frames; the oracle also emits 1 hidden alt-ref frame.
        const SHOWN: usize = 6;
        // Shown frame 2's V plane, 4:2:2 at 128x256 -> 64 x 256 chroma.
        const F2_V_FNV: u64 = 0xa7ba_8711_9d13_8fe5;
        // V (row 190, col 36): the first sample that moved. The pre-fix
        // square gather read 147 here, ffmpeg 146.
        const FIRST_WRONG_V: usize = 190 * 64 + 36;
        const F2_V_AT_FIRST_WRONG: u16 = 146;
        let _gate_lock = lock_gate_counters();
        let stream = read_pinned_muchunk_fixture(FILE, LEN, FNV, NAME);
        assert_eq!(
            obu_stream_shape(&stream),
            Some((true, 1, 0, false)),
            "{NAME}: {FILE} is not a 4:2:2 (ss 1/0, non-mono) 128-superblock stream"
        );
        let before = (
            crate::decode::mu_chunk_compound_units(),
            crate::decode::mu_chunk_intra_in_inter_units(),
        );
        let shown = crate::stream::decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: {FILE} no longer decodes cleanly: {e}"));
        let frames = shown.len();
        let (compound, intra) = (
            crate::decode::mu_chunk_compound_units() - before.0,
            crate::decode::mu_chunk_intra_in_inter_units() - before.1,
        );
        assert_eq!(frames, SHOWN, "{NAME}: {FILE} shown-frame count");
        assert!(
            intra >= MIN_UNITS,
            "{NAME}: {FILE} allocated only {intra} intra-in-inter `side > 64` mu-chunk units \
             (expected >= {MIN_UNITS}) -- the site counter this gate names did not move"
        );
        assert_eq!(
            compound, 0,
            "{NAME}: {FILE} also ran the compound walk ({compound} units) -- the two cells are \
             no longer separated"
        );
        // (b) the oracle-free half: the V plane of the first intrinsically
        // wrong frame, sample-for-sample, plus the one sample the pre-fix
        // gather got wrong.
        assert_eq!(
            shown[2].v.len(),
            64 * 256,
            "{NAME}: {FILE} is no longer 4:2:2 at 128x256 -- the V plane shape moved"
        );
        assert_eq!(
            shown[2].v[FIRST_WRONG_V], F2_V_AT_FIRST_WRONG,
            "{NAME}: {FILE} shown frame 2 V[{FIRST_WRONG_V}] (chroma (190, 36)) is {} -- the \
             pre-fix square gather read 147 here",
            shown[2].v[FIRST_WRONG_V]
        );
        assert_eq!(
            fnv1a64(&shown[2].v.iter().map(|&s| s as u8).collect::<Vec<u8>>()),
            F2_V_FNV,
            "{NAME}: {FILE} shown frame 2's V plane moved -- the intra-in-inter mu-chunk walk's \
             4:2:2 chroma context gather is wrong again (first sample to move is chroma (190, \
             36), index {FIRST_WRONG_V})"
        );
        // (c) the required ffmpeg compare: every plane of every shown frame.
        if !have_ffmpeg() {
            eprintln!(
                "SKIP {NAME}: no ffmpeg -- the 4:2:2 pixel compare this gate exists for did NOT \
                 run (set EC_AV1_REQUIRE_FFMPEG=1 to make this a failure)"
            );
            return;
        }
        let ff = ffmpeg_decode_sequence_422(&stream, 128, 256, SHOWN);
        assert_eq!(ff.len(), SHOWN, "{NAME}: {FILE} ffmpeg shown-frame count");
        for (i, (got, want)) in shown.iter().zip(&ff).enumerate() {
            assert_eq!(
                got.y, want.y,
                "{NAME}: {FILE} shown frame {i} luma vs ffmpeg"
            );
            assert_eq!(got.u, want.u, "{NAME}: {FILE} shown frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME}: {FILE} shown frame {i} V vs ffmpeg");
        }
        if aomdec_available(NAME) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert_eq!(
                decoded,
                SHOWN + 1,
                "{NAME}: {FILE} decode-order frame count"
            );
            eprintln!(
                "{NAME}: {FILE} byte-exact vs ffmpeg over {SHOWN} shown frames and vs the \
                 decode-order oracle over {decoded} frame(s) ({hidden} hidden), \
                 intra_in_inter_units={intra}"
            );
        } else {
            eprintln!(
                "{NAME}: {FILE} byte-exact vs ffmpeg over {SHOWN} shown frames, \
                 intra_in_inter_units={intra} -- the decode-order oracle compare above did NOT \
                 run (no aomdec)"
            );
        }
    }

    /// lane-av1muchunkluma: the **compound-arm CONTENT** stream that reaches
    /// only the intra-in-inter `side > 64` mu-chunk walk. `aomenc` was driven
    /// with the compound arm's own flags (`--enable-masked-comp=1
    /// --enable-onesided-comp=1 --auto-alt-ref=1`), so it *is* the compound
    /// hunt's `c3_grad128_comp` -- but its partition search never enters the
    /// compound mu-chunk walk (0 `compound/singleref` probe lines, 16
    /// `intra_in_inter` ones), so its ONLY mu-chunk walk is site 8.
    ///
    /// lane-av1muchunk422class measured this stream diverging on the
    /// then-current tree: first wrong sample **Y (62, 6) of shown frame 1**
    /// (`62 * 128 + 6 = 7942`, ours 74, ffmpeg 75) and a byte-identical decode
    /// before and after the compound DC-sign fix. lane-av1muchunk422iis's
    /// intra-in-inter gather fix closed it: the pinned halves below are the
    /// exact plane and the exact sample the pre-fix square gather moved.
    ///
    /// Recipe (byte-reproducible; re-encoded by this lane from the frozen
    /// `grad128.y4m`, sha256 `ab8b23137c4efc818884efcf0bb6ccda363bae4b8360f2ca10ca68506cbb777d`,
    /// which itself regenerates byte-for-byte):
    /// ```text
    /// ffmpeg -v error -f lavfi -i "gradients=size=128x128:c0=0xa1128f:\
    ///   c1=0xd98c48:c2=0x120601:c3=0x4abfba:seed=63:duration=0.24:rate=25,\
    ///   noise=all_seed=63:alls=6:allf=t" -t 0.24 -pix_fmt yuv422p \
    ///   -f yuv4mpegpipe - > grad128.y4m
    /// aomenc --codec=av1 --profile=2 --input-chroma-subsampling-x=1 \
    ///   --input-chroma-subsampling-y=0 --passes=1 --end-usage=q --cq-level=40 \
    ///   --cpu-used=0 --threads=1 --row-mt=0 --sb-size=128 \
    ///   --min-partition-size=128 --max-partition-size=128 \
    ///   --enable-rect-partitions=0 --enable-ab-partitions=0 \
    ///   --enable-1to4-partitions=0 --enable-palette=0 --enable-intrabc=0 \
    ///   --deltaq-mode=0 --enable-tx-size-search=0 --limit=6 --lag-in-frames=25 \
    ///   --auto-alt-ref=1 --enable-global-motion=1 --enable-warped-motion=1 \
    ///   --enable-masked-comp=1 --enable-diff-wtd-comp=1 --enable-onesided-comp=1 \
    ///   --obu -o c3_grad128_comp.obu grad128.y4m
    /// ```
    #[test]
    fn a_422_muchunk_iis_128root_grad128_comp_stream_is_exact_and_reaches_its_unit_walk() {
        const NAME: &str =
            "a_422_muchunk_iis_128root_grad128_comp_stream_is_exact_and_reaches_its_unit_walk";
        const FILE: &str = "422_muchunk_iis_128root_grad128.obu";
        const LEN: usize = 34449;
        const FNV: u64 = 0xfc59_a295_38db_577e;
        const MIN_UNITS: usize = 16;
        const SHOWN: usize = 6;
        // The stream carried in frame 1, the first frame the pre-fix gather
        // moved; frame 0 is the unaffected control.
        const F1_Y_FNV: u64 = 0x1b17_b4c2_4bd3_58d9;
        const F0_Y_FNV: u64 = 0x7666_cd9b_dc47_be3f;
        // Y (62, 6) of shown frame 1: the first sample that moved. The
        // pre-fix square gather read 74 here, ffmpeg 75.
        const FIRST_WRONG_Y: usize = 62 * 128 + 6;
        const F1_Y_AT_FIRST_WRONG: u16 = 75;
        let _gate_lock = lock_gate_counters();
        let stream = read_pinned_muchunk_fixture(FILE, LEN, FNV, NAME);
        assert_eq!(
            obu_stream_shape(&stream),
            Some((true, 1, 0, false)),
            "{NAME}: {FILE} is not a 4:2:2 (ss 1/0, non-mono) 128x128-superblock stream"
        );
        let before = (
            crate::decode::mu_chunk_compound_units(),
            crate::decode::mu_chunk_intra_in_inter_units(),
        );
        let shown = crate::stream::decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: {FILE} no longer decodes cleanly: {e}"));
        let frames = shown.len();
        let (compound, intra) = (
            crate::decode::mu_chunk_compound_units() - before.0,
            crate::decode::mu_chunk_intra_in_inter_units() - before.1,
        );
        assert_eq!(frames, SHOWN, "{NAME}: {FILE} shown-frame count");
        assert!(
            intra >= MIN_UNITS,
            "{NAME}: {FILE} allocated only {intra} intra-in-inter `side > 64` mu-chunk units \
             (expected >= {MIN_UNITS}) -- the site counter this gate names did not move"
        );
        assert_eq!(
            compound, 0,
            "{NAME}: {FILE} moved the compound-named counter ({compound} units) -- this stream's \
             only mu-chunk walk is the intra-in-inter one and the cells are no longer separated"
        );
        // (b) the oracle-free half: shown frame 1 (the first frame the pre-fix
        // gather moved) is asserted sample-for-sample on its luma plane, plus
        // the single sample that moved first; shown frame 0's luma is the
        // unaffected control that fails if the gather is "fixed" past its own
        // cell.
        assert_eq!(
            shown[1].y.len(),
            128 * 128,
            "{NAME}: {FILE} is no longer 4:2:2 at 128x128 -- the luma plane shape moved"
        );
        assert_eq!(
            fnv1a64(&shown[1].y.iter().map(|&s| s as u8).collect::<Vec<u8>>()),
            F1_Y_FNV,
            "{NAME}: {FILE} shown frame 1's luma moved -- the intra-in-inter mu-chunk walk's 4:2:2 \
             chroma context gather is wrong again (first sample to move is Y (62, 6), index \
             {FIRST_WRONG_Y})"
        );
        assert_eq!(
            shown[1].y[FIRST_WRONG_Y], F1_Y_AT_FIRST_WRONG,
            "{NAME}: {FILE} shown frame 1 Y[{FIRST_WRONG_Y}] (luma (62, 6)) is {} -- the pre-fix \
             square gather read 74 here",
            shown[1].y[FIRST_WRONG_Y]
        );
        assert_eq!(
            fnv1a64(&shown[0].y.iter().map(|&s| s as u8).collect::<Vec<u8>>()),
            F0_Y_FNV,
            "{NAME}: {FILE} shown frame 0's luma moved -- the 4:2:2 routing changed a frame it \
             must not touch"
        );
        // (c) the required ffmpeg compare: every plane of every shown frame.
        if !have_ffmpeg() {
            eprintln!(
                "SKIP {NAME}: no ffmpeg -- the 4:2:2 pixel compare this gate exists for did NOT \
                 run (set EC_AV1_REQUIRE_FFMPEG=1 to make this a failure)"
            );
            return;
        }
        let ff = ffmpeg_decode_sequence_422(&stream, 128, 128, SHOWN);
        assert_eq!(ff.len(), SHOWN, "{NAME}: {FILE} ffmpeg shown-frame count");
        for (i, (got, want)) in shown.iter().zip(&ff).enumerate() {
            assert_eq!(
                got.y, want.y,
                "{NAME}: {FILE} shown frame {i} luma vs ffmpeg"
            );
            assert_eq!(got.u, want.u, "{NAME}: {FILE} shown frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME}: {FILE} shown frame {i} V vs ffmpeg");
        }
        if aomdec_available(NAME) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert_eq!(
                decoded,
                SHOWN + 1,
                "{NAME}: {FILE} decode-order frame count (6 shown + the hidden alt-ref)"
            );
            eprintln!(
                "{NAME}: {FILE} byte-exact vs ffmpeg over {SHOWN} shown frames and vs the \
                 decode-order oracle over {decoded} frame(s) ({hidden} hidden), \
                 compound_units={compound} intra_in_inter_units={intra}"
            );
        } else {
            eprintln!(
                "{NAME}: {FILE} byte-exact vs ffmpeg over {SHOWN} shown frames, \
                 compound_units={compound} intra_in_inter_units={intra} -- the decode-order \
                 oracle compare above did NOT run (no aomdec)"
            );
        }
    }

    /// lane-av1muchunkluma: the 128x256 twin of the gate above -- the compound
    /// hunt's `c4_grad256_comp`. Unlike c3 this stream reaches BOTH `side > 64`
    /// mu-chunk walks (16 `intra_in_inter` probe lines, 24 `compound/singleref`
    /// ones with `compound=false`, i.e. the single-reference twin), yet
    /// lane-av1muchunk422class measured it diverging luma-first (first wrong
    /// sample **Y (126, 3) of shown frame 1**, `126 * 128 + 3 = 16131`, ours
    /// 94, ffmpeg 93). This lane re-measured it: still diverging on the
    /// pre-iis-gather tree, byte-exact on current `main`, so the
    /// intra-in-inter gather is what closed it -- its `MU_CHUNK_COMPOUND_UNITS`
    /// delta stays 0 (the counter names the compound arm only), which is why
    /// the gate's site half asserts the intra-in-inter counter.
    ///
    /// Recipe: same `aomenc` argv as the 128x128 gate above with the 128x256
    /// source (`gradients=size=128x256`, the frozen `grad256.y4m`, sha256
    /// `03c7fab7a6abc4e6764836cd2d35e76489438ecdba4ca1167fcd868250c0464c`,
    /// which regenerates byte-for-byte).
    #[test]
    fn a_422_muchunk_iis_128x256_grad256_comp_stream_is_exact_and_reaches_its_unit_walk() {
        const NAME: &str =
            "a_422_muchunk_iis_128x256_grad256_comp_stream_is_exact_and_reaches_its_unit_walk";
        const FILE: &str = "422_muchunk_iis_128x256_grad256.obu";
        const LEN: usize = 68925;
        const FNV: u64 = 0x65b8_d4dd_3806_7e09;
        const MIN_UNITS: usize = 16;
        const SHOWN: usize = 6;
        const F1_Y_FNV: u64 = 0x75e6_e790_47a8_9882;
        const F0_Y_FNV: u64 = 0x1265_8460_c00e_a10f;
        // Y (126, 3) of shown frame 1: the first sample that moved. The
        // pre-fix square gather read 94 here, ffmpeg 93.
        const FIRST_WRONG_Y: usize = 126 * 128 + 3;
        const F1_Y_AT_FIRST_WRONG: u16 = 93;
        let _gate_lock = lock_gate_counters();
        let stream = read_pinned_muchunk_fixture(FILE, LEN, FNV, NAME);
        assert_eq!(
            obu_stream_shape(&stream),
            Some((true, 1, 0, false)),
            "{NAME}: {FILE} is not a 4:2:2 (ss 1/0, non-mono) 128x128-superblock stream"
        );
        let before = (
            crate::decode::mu_chunk_compound_units(),
            crate::decode::mu_chunk_intra_in_inter_units(),
        );
        let shown = crate::stream::decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: {FILE} no longer decodes cleanly: {e}"));
        let frames = shown.len();
        let (compound, intra) = (
            crate::decode::mu_chunk_compound_units() - before.0,
            crate::decode::mu_chunk_intra_in_inter_units() - before.1,
        );
        assert_eq!(frames, SHOWN, "{NAME}: {FILE} shown-frame count");
        assert!(
            intra >= MIN_UNITS,
            "{NAME}: {FILE} allocated only {intra} intra-in-inter `side > 64` mu-chunk units \
             (expected >= {MIN_UNITS}) -- the site counter this gate names did not move"
        );
        assert_eq!(
            compound, 0,
            "{NAME}: {FILE} moved the compound-named counter ({compound} units) -- the fixture's \
             `compound/singleref` hits are its single-reference twin, which must not be counted \
             here"
        );
        assert_eq!(
            shown[1].y.len(),
            128 * 256,
            "{NAME}: {FILE} is no longer 4:2:2 at 128x256 -- the luma plane shape moved"
        );
        assert_eq!(
            fnv1a64(&shown[1].y.iter().map(|&s| s as u8).collect::<Vec<u8>>()),
            F1_Y_FNV,
            "{NAME}: {FILE} shown frame 1's luma moved -- the intra-in-inter mu-chunk walk's 4:2:2 \
             chroma context gather is wrong again (first sample to move is Y (126, 3), index \
             {FIRST_WRONG_Y})"
        );
        assert_eq!(
            shown[1].y[FIRST_WRONG_Y], F1_Y_AT_FIRST_WRONG,
            "{NAME}: {FILE} shown frame 1 Y[{FIRST_WRONG_Y}] (luma (126, 3)) is {} -- the pre-fix \
             square gather read 94 here",
            shown[1].y[FIRST_WRONG_Y]
        );
        assert_eq!(
            fnv1a64(&shown[0].y.iter().map(|&s| s as u8).collect::<Vec<u8>>()),
            F0_Y_FNV,
            "{NAME}: {FILE} shown frame 0's luma moved -- the 4:2:2 routing changed a frame it \
             must not touch"
        );
        if !have_ffmpeg() {
            eprintln!(
                "SKIP {NAME}: no ffmpeg -- the 4:2:2 pixel compare this gate exists for did NOT \
                 run (set EC_AV1_REQUIRE_FFMPEG=1 to make this a failure)"
            );
            return;
        }
        let ff = ffmpeg_decode_sequence_422(&stream, 128, 256, SHOWN);
        assert_eq!(ff.len(), SHOWN, "{NAME}: {FILE} ffmpeg shown-frame count");
        for (i, (got, want)) in shown.iter().zip(&ff).enumerate() {
            assert_eq!(
                got.y, want.y,
                "{NAME}: {FILE} shown frame {i} luma vs ffmpeg"
            );
            assert_eq!(got.u, want.u, "{NAME}: {FILE} shown frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME}: {FILE} shown frame {i} V vs ffmpeg");
        }
        if aomdec_available(NAME) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert_eq!(
                decoded,
                SHOWN + 1,
                "{NAME}: {FILE} decode-order frame count (6 shown + the hidden alt-ref)"
            );
            eprintln!(
                "{NAME}: {FILE} byte-exact vs ffmpeg over {SHOWN} shown frames and vs the \
                 decode-order oracle over {decoded} frame(s) ({hidden} hidden), \
                 compound_units={compound} intra_in_inter_units={intra}"
            );
        } else {
            eprintln!(
                "{NAME}: {FILE} byte-exact vs ffmpeg over {SHOWN} shown frames, \
                 compound_units={compound} intra_in_inter_units={intra} -- the decode-order \
                 oracle compare above did NOT run (no aomdec)"
            );
        }
    }

    /// lane-sbpart r2: a real `aomenc` stream whose superblock-level
    /// partition decision is genuinely HORZ/VERT (not NONE/SPLIT) must
    /// decode pixel-exact through [`crate::decode::decode_block_rect64`] --
    /// `sb_rect_hits() > 0` is a HARD assert (this round's rule: a removed
    /// refusal needs a firing gate in the same commit, not a green suite
    /// that never took the new path). Recipe = the charter's own, verified
    /// live by r1: `--min/max-partition-size=32/64` keeps the RD search
    /// from ever recursing below 32x32, so every SB-level decision is
    /// genuinely NONE/SPLIT/HORZ/VERT; a `gradients` source blended with
    /// `testsrc2` (lavfi `blend=all_mode=average`) keeps the content from
    /// being flat enough that the search stops at NONE every time.
    #[test]
    fn a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (192usize, 128usize, 1usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_SBPART_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=32",
                "--max-partition-size=64",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
                "--enable-filter-intra=0",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--enable-tx-size-search=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    // r5: the four SB-level AB arms (`partition_w64` 4-7)
                    // decode since this round, so the only partition refusal
                    // left at 64x64 is the 1:4 pair (values 8/9, four
                    // 64x16/16x64 strips, whose luma TX_64X16 this decoder
                    // has no transform for); the refusals this loop actually
                    // counts on these fixtures are the pre-existing
                    // screen-content-palette strip one. Every refusal is
                    // named, so a genuine decode failure still fails the
                    // assert above rather than skipping.
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched
                && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} frame {i} luma vs ffmpeg (seed {seed})"
                );
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a stream"
        );
        assert!(
            crate::decode::sb_rect_hits() > 0,
            "{NAME}: zero superblock-level HORZ/VERT blocks fired ({matched} matches, \
             {named_refusals} refusals out of {n_attempts}) -- gate proved nothing this run"
        );
        eprintln!(
            "{NAME}: {named_refusals} named refusals, {matched} pixel-exact matches out of \
             {n_attempts}, sb_rect_hits={}",
            crate::decode::sb_rect_hits()
        );
    }

    /// lane-golomb r2 gate: a 32x32-level block that STRADDLES the frame edge
    /// and whose partition is the gathered edge bit's `PARTITION_HORZ` (bottom
    /// edge) / `PARTITION_VERT` (right edge). Since r1 that bit can name
    /// HORZ/VERT instead of always inferring SPLIT, so the second 32x16/16x32
    /// half is out of frame and must NOT be decoded (libaom `decode_partition`:
    /// `case PARTITION_HORZ: decode_block(top); if (has_rows) decode_block(bottom)`).
    /// The AB and 1:4 arms need no such guard: at an edge libaom's
    /// `ec_read_partition_impl` can only yield HORZ/VERT/SPLIT, so those arms
    /// are unreachable there.
    ///
    /// Geometry: 192x80 (mi_rows=20 -- the second SB row is 16 px tall, so its
    /// 32-level blocks at y=64 have `has_rows32 == false`) and 80x192 for the
    /// right edge. Natural content (`testsrc2`); `smptebars`-style screen
    /// content trips the palette/screen-content refusal before the edge is
    /// reached. cq sweep, 8-bit and 10-bit, plus a 5-frame inter arm.
    /// Counters are sampled per attempt and summed only over attempts that
    /// decoded AND pixel-compared (class `counter-from-refused-stream`).
    #[test]
    fn a_real_aomenc_stream_with_a_32x32_frame_edge_rect_partition_decodes_pixel_exact() {
        edge32_gate(
            "a_real_aomenc_stream_with_a_32x32_frame_edge_rect_partition_decodes_pixel_exact",
            false,
        );
    }

    /// lane-golomb r2 OPEN DEFECT, pinned as a runnable test: with the 16-px
    /// straddling band left FLAT the encoder answers the 32-level edge bit
    /// with HORZ (`edge32=[2, 0, 2, 0]` on 192x80 cq40 8-bit), the guard added
    /// this round keeps the stream in entropy sync -- every pixel outside the
    /// two edge strips matches ffmpeg -- but the strips themselves
    /// reconstruct wrong: `plane Y: 1081 pixels differ, first at row 62 col
    /// 128`, diffs confined to rows 62..79 over ~64 columns (exactly the two
    /// 32x16 strips plus the deblock bleed above them). That is a
    /// reconstruction defect inside the strip (prediction/availability), not
    /// the partition guard. Un-ignored 2026-09-04: that reconstruction defect is gone and the two edge strips decode exactly.
    #[test]
    fn a_32x32_frame_edge_rect_partition_with_a_flat_band_decodes_pixel_exact() {
        edge32_gate(
            "a_32x32_frame_edge_rect_partition_with_a_flat_band_decodes_pixel_exact",
            true,
        );
    }

    /// lane-golomb r9 OPEN DEFECT, pinned as a runnable test: a frame whose
    /// height (192x68) or width (68x192) is not a multiple of 8 leaves a 4-px
    /// straddling band inside the mi-rounded plane, and every INTER frame's
    /// output is +-1 wrong across that band and the columns/rows next to it
    /// (`68x192 cq35 8-bit: frame 1 plane Y: 145 pixels differ, first at row 0
    /// col 64`, mass in columns 64..67). r9 bisected it stage by stage against
    /// the instrumented aomdec on that exact stream:
    /// * `EC_AV1_PREFILT_DUMP` frame 1 is BYTE-IDENTICAL over the cropped
    ///   68 columns -- reconstruction and inter prediction are exact, so the
    ///   charter's "MC reads past the crop" hypothesis is refuted a second time.
    /// * `EC_AV1_POSTDEBLOCK_DUMP` with `EC_AV1_DEBUG_SKIP_CDEF=1` also matches
    ///   aomdec's own post-deblock dump (which is 72 columns wide, i.e. it
    ///   exposes the straddling band) -- the deblock, including its
    ///   `frame_width` clip, is correct.
    /// * The divergence therefore enters in CDEF, at the last 8x8 CDEF block
    ///   column/row, which is the one that straddles the crop edge.
    ///   lane-cdef r2: CLOSED, and un-ignored. r1 rooted the luma half in
    ///   `apply_cdef`'s direction search, which clamped at the CROPPED frame
    ///   edge while libaom runs `cdef_find_dir` over `cdef_prepare_fb`'s buffer
    ///   -- whose columns/rows past the MI-ROUNDED extent (`hsize = nhb <<
    /// mi_wide_l2`, `cdef.c:164-167`, and the `frame_boundary[RIGHT]`
    ///   CDEF_VERY_LARGE fill at `cdef.c:249-256`) are CDEF_VERY_LARGE, not the
    ///   last real sample. The chroma half was lane-golomb r8's loop-restoration
    ///   plane size (LR is the one post-decode filter sized by the CROP, not the
    ///   mi grid) plus its reference-size fix; merging both branches takes all
    ///   four arms to 0 differing pixels on Y, U and V.
    ///
    /// Arms: 192x68 and 68x192 (a 4-px straddling band on one axis), 5 frames,
    /// 8-bit and 10-bit, plus a `--tile-columns=1` twin of each (COMMON's
    /// neighbour-map rule -- the skip band CDEF reads is a per-mi side band).
    /// RE-MEASURED lane-t900 r15 on the merged tip: all 64 attempts (8 arms x
    /// 8 cq levels, single- and multi-tile alike) decode pixel-exact with ZERO
    /// named refusals -- on 2026-09-02 all 32 multi-tile attempts still refused
    /// by name, so that half compared nothing (class
    /// `counter-from-refused-stream`). `compared_multi_tile` is asserted below
    /// so the multi-tile twins can never silently go back to comparing
    /// nothing.
    /// Hard-asserts `decode::cdef_straddle_units()` grew: a FILTERED 8x8 CDEF
    /// unit whose extent crosses the crop edge is the only shape for which
    /// clamping and padding differ, so a zero delta would make these arms
    /// vacuous for this defect.
    #[test]
    fn a_frame_edge_straddling_band_decodes_pixel_exact() {
        edge32_gate("a_frame_edge_straddling_band_decodes_pixel_exact", false);
    }

    /// lane-sqchroma r1 gate: a plain SQUARE-partition intra key frame with
    /// REAL chroma (no `hue=s=0` guard) and aomenc's tx-size search left at
    /// its own DEFAULT (on) -- the combination the user's films hit
    /// everywhere and that no gate in this file covered: every older recipe
    /// pins `--enable-tx-size-search=0`, and the one that does not
    /// (`a_real_aomenc_stream_with_a_split_transform_horz_vert_strip_decodes_pixel_exact`)
    /// desaturates its fixture. With the search on, RD gives a square block a
    /// luma transform smaller than its own side while chroma keeps the
    /// block's single `side/2` transform and its own `uv_mode`-derived
    /// `tx_type` -- the pair [`crate::decode::sq_chroma_tx_hits`] counts and
    /// this gate hard-asserts, so a stream whose split-transform blocks all
    /// landed on `DC_PRED` chroma cannot pass for free.
    ///
    /// Sweeps seeds 8-bit and finishes with one 10-bit stream of the same
    /// recipe (his films are `yuv420p10le`), whose sequence header's
    /// `bit_depth` is asserted before any pixel is compared. Only a missing
    /// tool SKIPs: a decode error that is not a named refusal, and every
    /// pixel mismatch, FAIL.
    #[test]
    fn a_real_aomenc_intra_stream_with_tx_size_search_and_chroma_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_intra_stream_with_tx_size_search_and_chroma_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (192usize, 128usize);
        // NOTE: no `--enable-tx-size-search=0` (the point of this gate), and
        // no chroma filter on the fixture (the other point). Square
        // partitions only: rect/AB/1to4 all off.
        let base_args: Vec<&str> = vec![
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cpu-used=0",
            "--threads=1",
            "--row-mt=0",
            "--sb-size=64",
            "--enable-rect-partitions=0",
            "--enable-ab-partitions=0",
            "--enable-1to4-partitions=0",
            "--min-partition-size=16",
            "--max-partition-size=32",
            "--enable-restoration=0",
            "--enable-palette=0",
            "--deltaq-mode=0",
            "--enable-intrabc=0",
            "--limit=1",
        ];
        let encode = |y4m: &[u8], extra: &[&str]| -> Vec<u8> {
            let mut args = base_args.clone();
            args.extend_from_slice(extra);
            args.extend_from_slice(&["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), y4m);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                !out.stdout.is_empty(),
                "{NAME}: aomenc wrote an empty stream"
            );
            out.stdout
        };
        let render = |source: &str, pix_fmt: &str| -> Vec<u8> {
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    source,
                    "-t",
                    "0.04",
                    "-pix_fmt",
                    pix_fmt,
                    "-strict",
                    "-1",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            y4m.stdout
        };
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        // Hits counted ONLY over attempts that decoded AND pixel-compared:
        // a stream that later refuses must not lend its firings to the
        // assertion below (class refusal-hides-a-defect).
        let mut firing = 0usize;
        let n_attempts: u32 = std::env::var("EC_SQCHROMA_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            // Two quantisers per seed: the coarse one keeps most blocks on
            // one transform, the fine one is what actually splits them.
            let cq = if attempt % 2 == 0 {
                "--cq-level=20"
            } else {
                "--cq-level=40"
            };
            let y4m = render(
                &gradients_source(seed, width, height, "duration=0.04:rate=25"),
                "yuv420p",
            );
            let stream = encode(&y4m, &[cq]);
            let before = crate::decode::sq_chroma_tx_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(frames.len(), 1, "{NAME}: expected one frame (seed {seed})");
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "{NAME} luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                frames[0].u, ffmpeg_frames[0].u,
                "{NAME} U vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                frames[0].v, ffmpeg_frames[0].v,
                "{NAME} V vs ffmpeg (seed {seed})"
            );
            firing += crate::decode::sq_chroma_tx_hits() - before;
            matched += 1;
        }
        // Same recipe, 10-bit: his two films are both yuv420p10le, so the
        // square+tx-search+chroma claim is only worth anything if it holds
        // at their bit depth too.
        let y4m10 = render(
            &gradients_source(42, width, height, "duration=0.04:rate=25"),
            "yuv420p10le",
        );
        let stream10 = encode(
            &y4m10,
            &["--cq-level=20", "--input-bit-depth=10", "--bit-depth=10"],
        );
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        while pos < stream10.len() && probe.sequence_header().is_none() {
            let obu = probe.parse_obu(&stream10[pos..]).unwrap();
            pos += obu.total_size;
        }
        assert_eq!(
            probe
                .sequence_header()
                .expect("stream has a sequence header OBU")
                .color_config
                .bit_depth,
            10,
            "{NAME}: aomenc did not actually write a 10-bit sequence header"
        );
        let before10 = crate::decode::sq_chroma_tx_hits();
        let frames10 = match decode_stream(&stream10) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused the 10-bit stream: {e}"),
        };
        let ffmpeg10 = ffmpeg_decode_sequence_10bit(&stream10, width, height, 1);
        assert_eq!(
            frames10[0].y, ffmpeg10[0].y,
            "{NAME} luma vs ffmpeg (10-bit)"
        );
        assert_eq!(frames10[0].u, ffmpeg10[0].u, "{NAME} U vs ffmpeg (10-bit)");
        assert_eq!(frames10[0].v, ffmpeg10[0].v, "{NAME} V vs ffmpeg (10-bit)");
        let firing10 = crate::decode::sq_chroma_tx_hits() - before10;
        firing += firing10;
        matched += 1;
        assert!(
            matched > 1,
            "{NAME}: every 8-bit attempt refused; the gate decoded only the 10-bit stream"
        );
        assert!(
            firing > 0,
            "{NAME}: zero square blocks split their luma transform under a non-DC chroma mode \
             ({matched} matches, {named_refusals} refusals) -- gate proved nothing this run"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact decodes ({named_refusals} named refusals out of \
             {n_attempts} 8-bit attempts), split-tx-with-chroma blocks: {firing} \
             (10-bit stream: {firing10})"
        );
    }

    /// lane-rectsplit r1 gate (a): a real `aomenc` key frame whose 32x32
    /// quadrants split HORZ/VERT into true 32x16/16x32 strips, with the
    /// tx-size search left at aomenc's own default (ON) -- every sibling
    /// recipe in this file pins `--enable-tx-size-search=0`, which is exactly
    /// why the split-transform strip was never exercised. RD is then free to
    /// give a strip a transform smaller than its own size, the case
    /// [`crate::decode::decode_block_rect`] refused by name ("a HORZ/VERT
    /// intra strip with a split transform (per-unit rect prediction is not
    /// ported)") until this round ported the per-transform-unit predict-and-
    /// reconstruct loop. Hard-asserts a nonzero `rect_split_tx_hits()` delta:
    /// without it a stream that resolved every `tx_depth` to 0 would
    /// pixel-match by construction and prove nothing. A decode error that is
    /// not a named refusal, and any pixel mismatch, FAIL -- only a missing
    /// tool skips.
    #[test]
    fn a_real_aomenc_stream_with_a_split_transform_horz_vert_strip_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_split_transform_horz_vert_strip_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (192usize, 128usize);
        let before = crate::decode::rect_split_tx_hits();
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        // lane-rectsplit r2 (verifier finding): the feature counter is
        // sampled PER ATTEMPT and folded in only after that attempt was
        // decoded AND pixel-compared. One delta across the whole sweep
        // counted strips decoded by attempts that went on to hit a named
        // refusal, so the gate could pass with every COMPARED stream
        // containing none of the feature at all.
        let mut compared_hits = 0usize;
        let mut split_so_far = 0usize;
        // Bounded sweep: see this gate's own doc for the seed-77 residue and
        // why extending it is a diagnosis knob, not a pass/fail one.
        let n_attempts: u32 = std::env::var("EC_RECTSPLIT_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    // REAL chroma (lane-sqchroma r1 dropped the `hue=s=0`
                    // guard this recipe carried): the "PRE-EXISTING square-path
                    // chroma defect" that guard named does not reproduce at
                    // HEAD -- seeds 42..61, cq 12/20/32/45, plus testsrc2 and
                    // mandelbrot, all decode pixel-exact with
                    // `--enable-rect-partitions=0`, and this gate is green with
                    // the filter gone. Its own gate is
                    // `a_real_aomenc_intra_stream_with_tx_size_search_and_chroma_decodes_pixel_exact`.
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            // NOTE: no `--enable-tx-size-search=0` here -- leaving aomenc's
            // own default (search ON) is the entire point of this gate.
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=16",
                "--max-partition-size=32",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
                "--enable-filter-intra=0",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            assert!(
                !stream.is_empty(),
                "{NAME}: aomenc wrote an empty stream (seed {seed})"
            );
            let attempt_before = crate::decode::rect_split_tx_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    continue;
                }
                Ok(frames) => frames,
            };
            let after = crate::decode::rect_split_tx_hits();
            eprintln!(
                "{NAME}: seed {seed} decoded, split-tx strips this stream: {}",
                after - before - split_so_far
            );
            split_so_far = after - before;
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(frames.len(), 1, "{NAME}: expected one frame (seed {seed})");
            if let Some(i) = frames[0]
                .y
                .iter()
                .zip(&ffmpeg_frames[0].y)
                .position(|(a, b)| a != b)
            {
                let diffs: Vec<usize> = frames[0]
                    .y
                    .iter()
                    .zip(&ffmpeg_frames[0].y)
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(j, _)| j)
                    .collect();
                let (x0, x1) = (
                    diffs.iter().map(|j| j % width).min().unwrap(),
                    diffs.iter().map(|j| j % width).max().unwrap(),
                );
                let (y0, y1) = (diffs[0] / width, diffs[diffs.len() - 1] / width);
                eprintln!(
                    "{NAME}: seed {seed} first luma mismatch at ({}, {}) ours={} ffmpeg={} \
                     -- {} mismatching samples, bbox x {x0}..={x1} y {y0}..={y1}",
                    i % width,
                    i / width,
                    frames[0].y[i],
                    ffmpeg_frames[0].y[i],
                    diffs.len(),
                );
            }
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "{NAME}: luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                frames[0].u, ffmpeg_frames[0].u,
                "{NAME}: U vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                frames[0].v, ffmpeg_frames[0].v,
                "{NAME}: V vs ffmpeg (seed {seed})"
            );
            compared_hits += crate::decode::rect_split_tx_hits() - attempt_before;
            matched += 1;
        }
        let split_hits = compared_hits;
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} named refusals) -- gate decoded nothing"
        );
        assert!(
            split_hits > 0,
            "{NAME}: zero HORZ/VERT strips resolved a split transform ({matched} pixel-exact \
             matches, {named_refusals} refusals out of {n_attempts}) -- gate proved nothing \
             this run"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals, \
             rect_split_tx_hits delta={split_hits} out of {n_attempts} attempts"
        );
    }

    /// lane-rect1d r1 (copied from lane-rectclass r1): the class-correctness measurement for every
    /// RECTANGULAR transform unit this decoder reads. [`crate::decode::read_coeffs_rect`]
    /// handles `TxClass::TwoD` only -- it refuses by name both a coded
    /// `tx_type` symbol and a non-2D default -- so the open question a
    /// verifier raised was whether a real encoder ever puts a
    /// `V_DCT`/`H_DCT` (`TX_CLASS_HORIZ`/`VERT`) type on a rect TU that
    /// reaches this reader. The recipe deliberately maximises the chance:
    /// `--enable-flip-idtx=1` (the flip/identity/1D types are exactly what
    /// `EXT_TX_SET_DTT4_IDTX_1DDCT` and the wider inter sets add),
    /// `--enable-tx-size-search=1`, rect partitions on, small minimum
    /// partition size, both bit depths. Every successful decode is
    /// pixel-compared; the rect-TU counter is folded in PER ATTEMPT and only
    /// after that attempt compared (class `counter-from-refused-stream`), and
    /// the class-1 refusal counter is summed over EVERY attempt including the
    /// refused ones -- a refused attempt is exactly where a 1D class on a
    /// rect TU would show up.
    ///
    /// lane-rect1d: on THIS tree `read_coeffs_rect` no longer refuses a 1D
    /// class -- lane-tx4x8 gave it the mrow/mcol scan and the class-aware
    /// base/br contexts -- so the live pins are `compared_class1 > 0` (the
    /// flag arrived and 1D types were coded) plus the pixel compare itself.
    #[test]
    fn a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (192usize, 128usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let mut matched_10bit = 0u32;
        let mut compared_rect_tus = 0usize;
        let mut compared_rect_parts = 0usize;
        let mut compared_tx_depths = 0usize;
        let mut compared_class1 = 0usize;
        let mut compared_rect_class1 = 0usize;
        let n_attempts: u32 = std::env::var("EC_RECTCLASS_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(12);
        // Both bit depths in one sweep: his two films are yuv420p10le, and a
        // class claim that only holds at 8 bits is worth nothing to them.
        for attempt in 0..n_attempts {
            let seed = 42 + attempt / 2;
            let ten_bit = attempt % 2 == 1;
            let pix_fmt = if ten_bit { "yuv420p10le" } else { "yuv420p" };
            // Content + quantiser sweep: the 1D (`V_DCT`/`H_DCT`) types RD can
            // only pick on strongly one-directional detail, and only when the
            // quantiser leaves that detail in the residual -- smooth gradients
            // at cq 45 produced exactly zero of them (measured this round),
            // which would have made the `--enable-flip-idtx=1` arrival assert
            // fail rather than the gate pass vacuously.
            let period = [4u32, 6, 8, 12, 16][(attempt as usize / 2) % 5];
            let source = format!("testsrc2=size={width}x{height}:rate=25");
            let filter = format!(
                "geq=lum='128+80*sin(2*PI*Y/{period})':cb='128+60*sin(2*PI*X/{period})':cr=128,\
                 noise=alls=6:allf=t"
            );
            let cq = if attempt % 4 < 2 {
                "--cq-level=20"
            } else {
                "--cq-level=40"
            };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    // `-strict -1`: y4m calls 10-bit an unofficial pixel
                    // format and refuses to write the header otherwise (no-op
                    // at 8 bit).
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-vf",
                    &filter,
                    "-t",
                    "0.04",
                    "-pix_fmt",
                    pix_fmt,
                    "-strict",
                    "-1",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            // aomenc keeps the LAST occurrence of a repeated `--enable-*`
            // flag (measured 2026-09-02 by md5 of four orderings), so this
            // lane's overrides come AFTER the base recipe. They are also
            // proven to have ARRIVED rather than assumed: the asserts below
            // pin a nonzero rect-partition, tx-depth and 1D-tx-class counter
            // on pixel-compared attempts, one per override.
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                cq,
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
                "--enable-filter-intra=0",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
            ];
            if ten_bit {
                args.extend_from_slice(&["--input-bit-depth=10", "--bit-depth=10"]);
            }
            args.extend_from_slice(&[
                "--enable-rect-partitions=1",
                "--enable-tx-size-search=1",
                "--enable-flip-idtx=1",
                // Without this aomenc's reduced tx set narrows every block to
                // DCT_DCT/IDTX and no 1D type can be coded at all -- the flag
                // that actually gates the class, as the arrival assert below
                // proved by staying at 0 without it.
                "--reduced-tx-type-set=0",
                "--min-partition-size=8",
                "--max-partition-size=32",
                "--obu",
                "-o",
                "-",
                "-",
            ]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            assert!(
                !stream.is_empty(),
                "{NAME}: aomenc wrote an empty stream (seed {seed})"
            );
            if ten_bit {
                let mut probe = Av1Parser::new();
                let mut pos = 0usize;
                while pos < stream.len() && probe.sequence_header().is_none() {
                    let obu = probe.parse_obu(&stream[pos..]).unwrap();
                    pos += obu.total_size;
                }
                assert_eq!(
                    probe
                        .sequence_header()
                        .expect("stream has a sequence header OBU")
                        .color_config
                        .bit_depth,
                    10,
                    "{NAME}: aomenc did not actually write a 10-bit sequence header"
                );
            }
            let attempt_before = crate::decode::rect_coeff_tu_hits();
            let rect_before = crate::decode::rect_partition_hits();
            let depth_before = crate::decode::tx_depth_hits();
            let class1_before = crate::decode::tx_class1_hits();
            let rect_class1_before = crate::decode::rect_class1_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}, 10bit \
                         {ten_bit}): {msg}"
                    );
                    named_refusals += 1;
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = if ten_bit {
                ffmpeg_decode_sequence_10bit(&stream, width, height, 1)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, 1)
            };
            assert_eq!(frames.len(), 1, "{NAME}: expected one frame (seed {seed})");
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "{NAME}: luma vs ffmpeg (seed {seed}, 10bit {ten_bit})"
            );
            assert_eq!(
                frames[0].u, ffmpeg_frames[0].u,
                "{NAME}: U vs ffmpeg (seed {seed}, 10bit {ten_bit})"
            );
            assert_eq!(
                frames[0].v, ffmpeg_frames[0].v,
                "{NAME}: V vs ffmpeg (seed {seed}, 10bit {ten_bit})"
            );
            compared_rect_tus += crate::decode::rect_coeff_tu_hits() - attempt_before;
            compared_rect_parts += crate::decode::rect_partition_hits() - rect_before;
            compared_tx_depths += crate::decode::tx_depth_hits() - depth_before;
            compared_class1 += crate::decode::tx_class1_hits() - class1_before;
            compared_rect_class1 += crate::decode::rect_class1_hits() - rect_class1_before;
            matched += 1;
            matched_10bit += u32::from(ten_bit);
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} named refusals) -- gate decoded \
             nothing"
        );
        assert!(
            matched_10bit > 0,
            "{NAME}: every 10-bit attempt refused -- his films are yuv420p10le, so a gate that \
             only compared 8-bit streams proves nothing for them"
        );
        assert!(
            compared_rect_tus > 0,
            "{NAME}: no pixel-exact attempt coded a single rectangular transform unit \
             ({matched} matches, {named_refusals} refusals out of {n_attempts}) -- the gate \
             measured nothing about rect coefficient reading"
        );
        // FLAG ARRIVAL, one assert per override (a flag whose counter stays
        // at 0 is an inert flag, not a passing gate).
        assert!(
            compared_rect_parts > 0,
            "{NAME}: --enable-rect-partitions=1 never produced a rect partition on a compared \
             attempt ({matched} matches) -- the flag did not arrive"
        );
        assert!(
            compared_tx_depths > 0,
            "{NAME}: --enable-tx-size-search=1 never produced a nonzero tx depth on a compared \
             attempt ({matched} matches) -- the flag did not arrive"
        );
        assert!(
            compared_class1 > 0,
            "{NAME}: --enable-flip-idtx=1 never produced a V_DCT/H_DCT transform on a compared \
             attempt ({matched} matches) -- the 1D types live only in the extended ext-tx sets \
             this flag enables, so the flag did not arrive"
        );
        // lane-rect1d r1: the pin of the LANE's own capability -- a V_DCT/H_DCT
        // transform on a RECTANGULAR transform unit, read through
        // `read_coeffs_rect`'s own 1D scan and contexts. `compared_class1`
        // above counts square TUs too, so it can be nonzero while this path
        // never fires.
        assert!(
            compared_rect_class1 > 0,
            "{NAME}: no pixel-exact attempt put a 1D tx class on a RECTANGULAR transform \
             ({compared_class1} 1D transforms overall, {compared_rect_tus} rect coefficient \
             TUs) -- the gate proves nothing about the rect 1D path"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact decodes ({named_refusals} named refusals out of \
             {n_attempts} attempts, {matched_10bit} of the matches 10-bit), rect coefficient \
             TUs on compared attempts: {compared_rect_tus} ({compared_rect_class1} of them a \
             1D tx class); flag arrival: rect partitions {compared_rect_parts}, tx \
             depths {compared_tx_depths}, 1D-class (square) transforms {compared_class1}"
        );
    }

    /// lane-rectsplit r1 gate (c): gate (a)'s recipe with
    /// `--enable-filter-intra=1` (every sibling gate here pins it to 0), so
    /// aomenc's RD is free to pick `use_filter_intra` on a 32x16/16x32 strip
    /// -- the case [`crate::decode::decode_block_rect`] refused by name
    /// ("filter intra on a HORZ/VERT strip (this decoder predicts
    /// square-only)") until this round gave `predict_filter_intra` its own
    /// `bw`x`bh`. Hard-asserts a nonzero `filter_intra_rect_hits()` delta:
    /// a stream where RD never picked filter intra on a strip would
    /// pixel-match by construction and prove nothing.
    #[test]
    fn a_real_aomenc_stream_with_filter_intra_on_a_horz_vert_strip_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_filter_intra_on_a_horz_vert_strip_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (192usize, 128usize);
        let rect_before = crate::decode::rect_partition_hits();
        let fi_square_before = crate::decode::filter_intra_hits();
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        // lane-rectsplit r2 (verifier finding): the feature counter is
        // sampled PER ATTEMPT and folded in only after that attempt was
        // decoded AND pixel-compared. One delta across the whole sweep
        // counted strips decoded by attempts that went on to hit a named
        // refusal, so the gate could pass with every COMPARED stream
        // containing none of the feature at all.
        let mut compared_hits = 0usize;
        let n_attempts: u32 = std::env::var("EC_RECTSPLIT_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            // Diagnosis knob: which content makes RD pick a HORZ/VERT strip
            // AND filter intra on it is a search (see this round's report).
            let source = match std::env::var("EC_RECTSPLIT_SRC")
                .unwrap_or_default()
                .as_str()
            {
                "bars" => format!("smptebars=size={width}x{height}:duration=0.04:rate=25"),
                "src2" => format!("testsrc2=size={width}x{height}:duration=0.04:rate=25"),
                _ => gradients_source(seed, width, height, "duration=0.04:rate=25"),
            };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    // Real chroma: the `hue=s=0` guard gate (a) carried is
                    // gone (lane-sqchroma r1 -- the square-path chroma defect
                    // it named does not reproduce at HEAD).
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            // Diagnosis knob only (default is the gate's pinned recipe):
            // which `--cq-level` makes aomenc's RD reach for filter intra on
            // a strip is a search, and the sweep that found this one is in
            // lanes/rectsplit-r1.report.md.
            let cq = format!(
                "--cq-level={}",
                // Pinned by the sweep in lanes/rectsplit-r1.report.md: at
                // cq 45 aomenc's RD picks a strip 18 times in 100 seeds and
                // filter intra on none of them; at cq 25 strips are common
                // (130/100 seeds) and filter intra lands on one every ~13
                // seeds.
                std::env::var("EC_RECTSPLIT_CQ").unwrap_or_else(|_| "25".into())
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                &cq,
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=16",
                "--max-partition-size=32",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
                // The point of this gate.
                "--enable-filter-intra=1",
                // NOTE (lane-rectsplit r2, verifier finding): every intra
                // mode stays ON here. The square filter-intra gate reaches
                // for filter intra by switching the directional/smooth/paeth
                // competitors off, and this gate CANNOT: with them off
                // aomenc stops picking rect partitions at all (rect
                // strips 0/20 seeds, r1's table). What fires filter intra on
                // a strip here is the quantiser (`--cq-level=25`), not the
                // mode set -- `EC_RECTSPLIT_OFF` below is the diagnosis knob
                // that re-runs the subset search.
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            // Diagnosis knob (default: every intra mode on): turning the
            // directional/smooth/paeth competitors off is how the square
            // filter-intra gate makes RD reach for filter intra, but with all
            // four off aomenc stops picking rect partitions here entirely
            // (rect strips=0, filter-intra blocks=28, measured this round),
            // so the firing recipe is a search over subsets.
            let mut args = args;
            let off = std::env::var("EC_RECTSPLIT_OFF").unwrap_or_default();
            for tool in off.split(',').filter(|t| !t.is_empty()) {
                args.push(match tool {
                    "smooth" => "--enable-smooth-intra=0",
                    "paeth" => "--enable-paeth-intra=0",
                    "dir" => "--enable-directional-intra=0",
                    "angle" => "--enable-angle-delta=0",
                    other => panic!("unknown EC_RECTSPLIT_OFF entry {other}"),
                });
            }
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            assert!(
                !stream.is_empty(),
                "{NAME}: aomenc wrote an empty stream (seed {seed})"
            );
            let attempt_before = crate::decode::filter_intra_rect_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(frames.len(), 1, "{NAME}: expected one frame (seed {seed})");
            if let Some(i) = frames[0]
                .y
                .iter()
                .zip(&ffmpeg_frames[0].y)
                .position(|(a, b)| a != b)
            {
                let diffs: Vec<usize> = frames[0]
                    .y
                    .iter()
                    .zip(&ffmpeg_frames[0].y)
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(j, _)| j)
                    .collect();
                let (x0, x1) = (
                    diffs.iter().map(|j| j % width).min().unwrap(),
                    diffs.iter().map(|j| j % width).max().unwrap(),
                );
                let (y0, y1) = (diffs[0] / width, diffs[diffs.len() - 1] / width);
                eprintln!(
                    "{NAME}: seed {seed} first luma mismatch at ({}, {}) ours={} ffmpeg={} \
                     -- {} mismatching samples, bbox x {x0}..={x1} y {y0}..={y1}",
                    i % width,
                    i / width,
                    frames[0].y[i],
                    ffmpeg_frames[0].y[i],
                    diffs.len(),
                );
            }
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "{NAME}: luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                frames[0].u, ffmpeg_frames[0].u,
                "{NAME}: U vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                frames[0].v, ffmpeg_frames[0].v,
                "{NAME}: V vs ffmpeg (seed {seed})"
            );
            compared_hits += crate::decode::filter_intra_rect_hits() - attempt_before;
            matched += 1;
        }
        let fi_hits = compared_hits;
        eprintln!(
            "{NAME}: rect strips this run={}, filter-intra blocks (any shape)={}",
            crate::decode::rect_partition_hits() - rect_before,
            crate::decode::filter_intra_hits() - fi_square_before
        );
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} named refusals) -- gate decoded nothing"
        );
        assert!(
            fi_hits > 0,
            "{NAME}: no HORZ/VERT strip used filter intra ({matched} pixel-exact matches, \
             {named_refusals} refusals out of {n_attempts}) -- gate proved nothing this run"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals, \
             filter_intra_rect_hits delta={fi_hits} out of {n_attempts} attempts"
        );
    }

    /// lane-fistrip r1, the 10-bit arm of the strip gate (his films are
    /// yuv420p10le). Same ceiling as its 8-bit twin, lifted in lane-rectsplitx
    /// r3 (the split arm's `smooth_neighbor_uv`); before that, MEASURED over seeds
    /// 42..61 the 8x16/16x8 strips aomenc writes filter intra on are
    /// `skip=0`, which `decode_leaf_rect` refuses, so
    /// `filter_intra_rect_sub16_hits` never reaches a pixel compare.
    /// lane-fistrip r2 adds seeds 49 and 213 to the window: those are the two
    /// seeds measured to fire the feature on the 8-BIT twin's recipe (this
    /// arm's 10-bit encode is a different stream, so they are a widening, not
    /// a measured firing set for this recipe). Green since r3.
    #[test]
    fn a_real_aomenc_10bit_filter_intra_on_a_sub16_strip_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_filter_intra_on_a_sub16_strip_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let mut seeds: Vec<u32> = (42..62).collect();
        seeds.push(213);
        ten_bit_tool_gate(
            NAME,
            192,
            128,
            1,
            &[
                "--limit=1",
                "--cq-level=25",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=8",
                "--max-partition-size=32",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
                "--enable-filter-intra=1",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
            ],
            &seeds,
            &[(
                "filter_intra_rect_sub16_hits",
                crate::decode::filter_intra_rect_sub16_hits as fn() -> usize,
            )],
        );
    }

    /// lane-fistrip r1, the pinned half of this lane's proof: one real
    /// `aomenc` stream (recipe below, byte-reproducible -- md5
    /// ac3a64773e5454ee9b5d8650507063cd, hashed twice) whose key frame
    /// carries four `BLOCK_16X8` DC_PRED leaves, one of which sets
    /// `use_filter_intra`. `av1_filter_intra_allowed_bsize` allows 16x8, so
    /// aomenc writes that flag; before this round `filter_intra_size_class_rect`
    /// had no row for the shape, the symbol was never read, and the tile
    /// desynced ON THAT BLOCK -- observable as a DIFFERENT named refusal
    /// ("a HORZ/VERT intra strip below 16x16 with a split transform", a
    /// `tx_depth` read out of a desynced stream) instead of this one.
    ///
    /// The stream cannot be pixel-compared yet: the 16x8 strip that carries
    /// the flag is `skip=0`, and a coded rect strip below 16x16 has no
    /// residual path here (its own refusal, another lane's ceiling). What
    /// this test pins is the SYMBOL: the flag is read, the counter fires
    /// once, and the decode stops at the coded-strip ceiling rather than
    /// wandering off a desynced stream.
    ///
    /// Fixture recipe (`crates/ec-av1/fixtures/filter_intra_8x16_strip_seed49.obu`):
    /// `ffmpeg -f lavfi -i gradients=size=192x128:c0=0x986ae1:c1=0xd3e49a:
    /// c2=0x0b1e53:c3=0x42980c:seed=49:duration=0.04:rate=25 -pix_fmt yuv420p`
    /// into `aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=25
    /// --cpu-used=0 --threads=1 --row-mt=0 --sb-size=64
    /// --enable-rect-partitions=1 --enable-ab-partitions=0
    /// --enable-1to4-partitions=0 --min-partition-size=8
    /// --max-partition-size=32 --enable-restoration=0 --enable-palette=0
    /// --deltaq-mode=0 --enable-filter-intra=1 --enable-cfl-intra=0
    /// --enable-intrabc=0 --obu`.
    #[test]
    fn a_pinned_aomenc_16x8_strip_reads_its_use_filter_intra_flag() {
        const NAME: &str = "a_pinned_aomenc_16x8_strip_reads_its_use_filter_intra_flag";
        let _gate_lock = lock_gate_counters();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/filter_intra_8x16_strip_seed49.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let before = crate::decode::filter_intra_rect_sub16_hits();
        // lane-rectsplitx r1: the ceiling this fixture used to stop at (a
        // sub-16 strip with a SPLIT transform) is ported, so the pinned
        // stream is now a full pixel compare -- what the doc above said this
        // test should become the round that ceiling lifted.
        let frames =
            decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME}: decode refused: {e}"));
        let hits = crate::decode::filter_intra_rect_sub16_hits() - before;
        // TWO now, not the one the truncated decode used to see: the frame
        // no longer stops at the first filter-intra strip, so the second one
        // later in the tile is reached as well (and the pixel compare below
        // proves both reconstructed right).
        assert_eq!(
            hits, 2,
            "{NAME}: expected both 16x8 strips to read use_filter_intra=1; got {hits}"
        );
        // This is also the GATE for the sub-16 split-transform lift: both
        // strips split their transform (`tx_depth != 0` on a BLOCK_16X8 ->
        // two TX_8X8 units), which is the refusal this round removed.
        let splits = crate::decode::sub16_split_hits();
        assert!(
            splits > 0,
            "{NAME}: no sub-16 strip split its transform -- the fixture no longer proves the \
             per-unit path this test gates"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 192, 128, 1);
        assert_eq!(frames.len(), 1, "{NAME}: expected one frame");
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg");
    }

    /// lane-fistrip r1: the filter-intra strip gate above, one partition
    /// level DOWN (`--min-partition-size=8`, so a 16x16 HORZ/VERT splits
    /// into 8x16/16x8 leaves). `av1_filter_intra_allowed_bsize` is
    /// `wide <= 32 && high <= 32`, which both of those satisfy, so aomenc
    /// writes a `use_filter_intra` flag on every DC_PRED one of them --
    /// `filter_intra_size_class_rect` had no row for either shape and
    /// [`crate::decode::read_intra_mode_rect`] therefore never READ that
    /// symbol (class `symbol-consumption-gap`, a silent desync rather than
    /// a refusal). Hard-asserts
    /// [`crate::decode::filter_intra_rect_sub16_hits`], not the wider
    /// `filter_intra_rect_hits`: a 32x16 strip firing filter intra would
    /// satisfy that one and prove nothing about the new shapes.
    /// MEASURED (lane-fistrip r1, seeds 42..=241; r2 re-ran 50..=241), which
    /// is why the window carries the two firing seeds: over those 200 seeds at
    /// cq 25 aomenc put filter intra on an 8x16/16x8 strip exactly TWICE --
    /// **seed 49 and seed 213** -- and both times that strip was `skip=0`, so
    /// the attempt refused at the coded-rect-strip-below-16x16 ceiling before
    /// any pixel compare (r1: 177 pixel-exact matches, 23 refusals, compared
    /// sub16 delta 0, all-attempt delta 2; r2 seeds 50..=241: 172 matches, 20
    /// refusals, the one all-attempt hit at seed 213). BOTH firing seeds are
    /// appended to this gate's seed window below, so it compares two real
    /// hits. UN-IGNORED lane-rectsplitx r3: the ceiling lifted with the split
    /// refusal, and the residual chroma defect was the split arm of
    /// [`crate::decode::decode_leaf_rect`] passing `smooth_neighbor_uv: false`
    /// where the unsplit arm passes the neighbour's real answer. The symbol
    /// itself stays pinned by
    /// [`a_pinned_aomenc_16x8_strip_reads_its_use_filter_intra_flag`].
    #[test]
    fn a_real_aomenc_stream_with_filter_intra_on_a_sub16_horz_vert_strip_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_filter_intra_on_a_sub16_horz_vert_strip_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (192usize, 128usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let mut compared_hits = 0usize;
        let n_attempts: u32 = std::env::var("EC_FISTRIP_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        // lane-fistrip r2: the two seeds that MEASURABLY fire filter intra on
        // an 8x16/16x8 strip are 49 and 213 -- the only two in seeds 42..=241
        // (r1 measured 42..=241 and reported "all-attempt delta 2" with seed
        // 49; r2 re-ran seeds 50..=241 and found the second, log
        // `$HOME/.cache/fistrip-seedscan-r2.log`: "seed 213 refused with sub16
        // filter-intra delta 1"). Both are appended to whatever window
        // `EC_FISTRIP_GATE_ATTEMPTS` asks for, so the un-ignore after the
        // coded-rect-strip ceiling lifts compares TWO real hits instead of
        // depending on the default window happening to contain one.
        const FIRING_SEEDS: [u32; 2] = [49, 213];
        let mut seeds: Vec<u32> = (0..n_attempts).map(|a| 42 + a).collect();
        for s in FIRING_SEEDS {
            if !seeds.contains(&s) {
                seeds.push(s);
            }
        }
        let cq = format!(
            "--cq-level={}",
            std::env::var("EC_FISTRIP_CQ").unwrap_or_else(|_| "25".into())
        );
        // Diagnosis: the counter over EVERY attempt, refused ones included --
        // a filter-intra 8x16 strip inside an attempt that later refused
        // (non-skip, split transform) fires this but never reaches
        // `compared_hits`, and the two numbers differing is what says the
        // recipe reaches the feature but this decoder's other ceilings hide
        // it.
        let all_before = crate::decode::filter_intra_rect_sub16_hits();
        for &seed in &seeds {
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                &cq,
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                // The point of this gate: one level below the 32x16/16x32
                // strips the sibling gate covers.
                "--min-partition-size=8",
                "--max-partition-size=32",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
                "--enable-filter-intra=1",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            // Diagnosis knob only (default: every intra mode on) -- the same
            // subset search the 32x16/16x32 sibling gate carries.
            let mut args = args;
            let off = std::env::var("EC_FISTRIP_OFF").unwrap_or_default();
            for tool in off.split(',').filter(|t| !t.is_empty()) {
                args.push(match tool {
                    "smooth" => "--enable-smooth-intra=0",
                    "paeth" => "--enable-paeth-intra=0",
                    "dir" => "--enable-directional-intra=0",
                    "angle" => "--enable-angle-delta=0",
                    other => panic!("unknown EC_FISTRIP_OFF entry {other}"),
                });
            }
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            assert!(
                !stream.is_empty(),
                "{NAME}: aomenc wrote an empty stream (seed {seed})"
            );
            let attempt_before = crate::decode::filter_intra_rect_sub16_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    // Diagnosis: a refused attempt that DID fire the feature
                    // says which other ceiling of this decoder hides it.
                    eprintln!(
                        "{NAME}: seed {seed} refused with sub16 filter-intra delta {}: {msg}",
                        crate::decode::filter_intra_rect_sub16_hits() - attempt_before
                    );
                    named_refusals += 1;
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(frames.len(), 1, "{NAME}: expected one frame (seed {seed})");
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "{NAME}: luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                frames[0].u, ffmpeg_frames[0].u,
                "{NAME}: U vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                frames[0].v, ffmpeg_frames[0].v,
                "{NAME}: V vs ffmpeg (seed {seed})"
            );
            compared_hits += crate::decode::filter_intra_rect_sub16_hits() - attempt_before;
            matched += 1;
        }
        eprintln!(
            "{NAME}: filter_intra_rect_sub16_hits over ALL attempts (refused included) = {}",
            crate::decode::filter_intra_rect_sub16_hits() - all_before
        );
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} named refusals) -- gate decoded nothing"
        );
        assert!(
            compared_hits > 0,
            "{NAME}: no 8x16/16x8 strip used filter intra ({matched} pixel-exact matches, \
             {named_refusals} refusals out of {} attempts) -- gate proved nothing this run",
            seeds.len()
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals, \
             filter_intra_rect_sub16_hits delta={compared_hits} (all attempts including \
             refused: {}) out of {} attempts",
            crate::decode::filter_intra_rect_sub16_hits() - all_before,
            seeds.len()
        );
    }

    /// lane-rectsplit r1 gate (b): [`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`]'s
    /// own recipe (`--sb-size=64`, `--min/max-partition-size=32/64`, so every
    /// SB-level decision is genuinely NONE/SPLIT/HORZ/VERT) with its
    /// `--enable-tx-size-search=0` DROPPED: the 64x32/32x64 strips may then
    /// carry a split transform, which [`crate::decode::decode_block_rect64`]
    /// refused by name ("a superblock-level HORZ/VERT strip with a split
    /// transform") until lane-rectsplit r2 lifted it.
    ///
    /// The hard assert is on [`crate::decode::rect_split_sb_interior_tu_hits`],
    /// NOT the shared `rect_split_tx_hits()` gate (a) uses: that counter also
    /// counts 32x32-level and depth-1 strips, whose units are all in the
    /// first transform-unit row/column. Only a superblock-level strip split
    /// past depth 1 has an INTERIOR unit (`row_off > 0 && col_off > 0`),
    /// which is the per-unit availability rule this port exists for, and no
    /// gate fired one before this round.
    ///
    /// This gate was `#[ignore]`d RED through r3 (seed 50 luma (171,56)).
    /// Root cause, r4, TWO defects, neither of them availability -- the
    /// strip's own per-unit prediction was exact all along:
    /// 1. `av1_nz_map_ctx_offset` is indexed by libaom's COLUMN-major
    ///    `coeff_idx = col * 32 + row`, so [`crate::decode::base_ctx`] read
    ///    the `TX_32X64`/`TX_64X32` offset tables transposed (r2 saw this and
    ///    r3 reverted it, because "fixing" it turned the merged SB HORZ/VERT
    ///    gate RED at seed 43);
    /// 2. that seed-43 RED was a SECOND defect the first one had been hiding
    ///    behind a refusal: `EOB_PT_512_CHROMA` was transcribed from
    ///    `av1_default_eob_multi512_cdfs`'s `eob_multi_ctx = 1` row (the flat
    ///    `V_DCT`/`H_DCT` distribution), so the first real 32x16 chroma
    ///    transform read `eob_pt` 7 where aomdec read 2. With both fixed,
    ///    seeds 43 and 50 are pixel-exact.
    #[test]
    fn a_real_aomenc_stream_with_a_split_transform_superblock_strip_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_split_transform_superblock_strip_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (192usize, 128usize);
        let before = crate::decode::rect_split_tx_hits();
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        // lane-rectsplit r2 (verifier finding): the feature counter is
        // sampled PER ATTEMPT and folded in only after that attempt was
        // decoded AND pixel-compared. One delta across the whole sweep
        // counted strips decoded by attempts that went on to hit a named
        // refusal, so the gate could pass with every COMPARED stream
        // containing none of the feature at all.
        let mut compared_hits = 0usize;
        let mut split_so_far = 0usize;
        // Bounded sweep: see this gate's own doc for the seed-77 residue and
        // why extending it is a diagnosis knob, not a pass/fail one.
        let n_attempts: u32 = std::env::var("EC_RECTSPLIT_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.04:rate=25");
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    // REAL chroma (lane-sqchroma r1 dropped the `hue=s=0`
                    // guard this recipe carried): the "PRE-EXISTING square-path
                    // chroma defect" that guard named does not reproduce at
                    // HEAD -- seeds 42..61, cq 12/20/32/45, plus testsrc2 and
                    // mandelbrot, all decode pixel-exact with
                    // `--enable-rect-partitions=0`, and this gate is green with
                    // the filter gone. Its own gate is
                    // `a_real_aomenc_intra_stream_with_tx_size_search_and_chroma_decodes_pixel_exact`.
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            // NOTE: no `--enable-tx-size-search=0` here -- leaving aomenc's
            // own default (search ON) is the entire point of this gate.
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=32",
                "--max-partition-size=64",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
                "--enable-filter-intra=0",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            assert!(
                !stream.is_empty(),
                "{NAME}: aomenc wrote an empty stream (seed {seed})"
            );
            let attempt_before = crate::decode::rect_split_sb_interior_tu_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    named_refusals += 1;
                    continue;
                }
                Ok(frames) => frames,
            };
            let after = crate::decode::rect_split_tx_hits();
            eprintln!(
                "{NAME}: seed {seed} decoded, split-tx strips this stream: {}",
                after - before - split_so_far
            );
            split_so_far = after - before;
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(frames.len(), 1, "{NAME}: expected one frame (seed {seed})");
            if let Some(i) = frames[0]
                .y
                .iter()
                .zip(&ffmpeg_frames[0].y)
                .position(|(a, b)| a != b)
            {
                let diffs: Vec<usize> = frames[0]
                    .y
                    .iter()
                    .zip(&ffmpeg_frames[0].y)
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(j, _)| j)
                    .collect();
                let (x0, x1) = (
                    diffs.iter().map(|j| j % width).min().unwrap(),
                    diffs.iter().map(|j| j % width).max().unwrap(),
                );
                let (y0, y1) = (diffs[0] / width, diffs[diffs.len() - 1] / width);
                eprintln!(
                    "{NAME}: seed {seed} first luma mismatch at ({}, {}) ours={} ffmpeg={} \
                     -- {} mismatching samples, bbox x {x0}..={x1} y {y0}..={y1}",
                    i % width,
                    i / width,
                    frames[0].y[i],
                    ffmpeg_frames[0].y[i],
                    diffs.len(),
                );
            }
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "{NAME}: luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                frames[0].u, ffmpeg_frames[0].u,
                "{NAME}: U vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                frames[0].v, ffmpeg_frames[0].v,
                "{NAME}: V vs ffmpeg (seed {seed})"
            );
            compared_hits += crate::decode::rect_split_sb_interior_tu_hits() - attempt_before;
            matched += 1;
        }
        let split_hits = compared_hits;
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} named refusals) -- gate decoded nothing"
        );
        assert!(
            split_hits > 0,
            "{NAME}: zero HORZ/VERT strips resolved a split transform ({matched} pixel-exact \
             matches, {named_refusals} refusals out of {n_attempts}) -- gate proved nothing \
             this run"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals, \
             rect_split_sb_interior_tu_hits delta={split_hits} (over COMPARED streams only) \
             out of {n_attempts} attempts"
        );
    }

    /// lane-part32 r4: a real `aomenc` stream whose superblock-level
    /// partition decision is one of the four AB arms (`PARTITION_HORZ_A`/
    /// `_B`/`VERT_A`/`_B` at 64x64: two 32x32 squares plus one 64x32/32x64
    /// strip) must decode pixel-exact. Same recipe as the HORZ/VERT sibling
    /// above with `--enable-ab-partitions=1`; `sb_ab_hits() > 0` is a HARD
    /// assert, so a run where aomenc's RD never picked an AB arm fails
    /// rather than passing vacuously.
    #[test]
    fn a_real_aomenc_stream_with_a_superblock_level_ab_partition_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_superblock_level_ab_partition_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (192usize, 128usize, 1usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_SBAB_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source =
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"));
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-pix_fmt",
                    "yuv420p",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=1",
                "--enable-1to4-partitions=0",
                "--min-partition-size=32",
                "--max-partition-size=64",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
                "--enable-filter-intra=0",
                "--enable-cfl-intra=0",
                "--enable-intrabc=0",
                "--enable-tx-size-search=0",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    // r5: the four SB-level AB arms (`partition_w64` 4-7)
                    // decode since this round, so the only partition refusal
                    // left at 64x64 is the 1:4 pair (values 8/9, four
                    // 64x16/16x64 strips, whose luma TX_64X16 this decoder
                    // has no transform for); the refusals this loop actually
                    // counts on these fixtures are the pre-existing
                    // screen-content-palette strip one. Every refusal is
                    // named, so a genuine decode failure still fails the
                    // assert above rather than skipping.
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched
                && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} frame {i} luma vs ffmpeg (seed {seed})"
                );
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a stream"
        );
        let arms = crate::decode::sb_ab_hits_by_arm();
        for (i, name) in ["HORZ_A", "HORZ_B", "VERT_A", "VERT_B"].iter().enumerate() {
            assert!(
                arms[i] > 0,
                "{NAME}: zero superblock-level PARTITION_{name} blocks fired (arms={arms:?}, \
                 {matched} matches, {named_refusals} refusals out of {n_attempts}) -- that arm \
                 proved nothing this run"
            );
        }
        eprintln!(
            "{NAME}: {named_refusals} named refusals, {matched} pixel-exact matches out of \
             {n_attempts}, sb_ab_hits={} arms(HORZ_A,HORZ_B,VERT_A,VERT_B)={arms:?}",
            crate::decode::sb_ab_hits()
        );
    }
