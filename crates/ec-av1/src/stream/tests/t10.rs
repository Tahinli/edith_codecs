use super::*;
use super::support::*;
use super::gates_a::*;
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

    /// lane-sub8x4 r2, the gate that lifts BOTH sub-8x8 intra refusals ("an
    /// intra 4x4 block inside an inter frame's sub-8x8 split" and "an intra
    /// 8x4/4x8 block inside an inter frame's sub-8x8 HORZ/VERT partition").
    ///
    /// Recipe (measured, `~/.cache/sub8intra-tmp/sweep3.sh`): a 24-row band of
    /// per-frame-moving noise over a smooth sinusoid, `--min-partition-size=4
    /// --max-partition-size=8 --enable-1to4-partitions=0` -- at that maximum a
    /// 16x4 shape cannot exist, so nothing here can be confused with the
    /// still-refused intra 1:4 strip, and the noise band is what makes aomenc's
    /// RD pick INTRA leaves inside an inter frame's 8x8 groups. 12 arms
    /// (8/10-bit x cq 48/55/63 x 128x128/192x128) plus one `--tile-columns=1`
    /// arm (COMMON's neighbour-map rule: these leaves publish per-mi side
    /// bands), 6 frames each, every decode-order frame and all three planes
    /// compared against ffmpeg. Continue-and-sweep: a NAMED refusal is
    /// recorded and the arm skipped, any other decode error or any pixel
    /// mismatch fails the gate on the spot.
    ///
    /// The end-of-sweep assert reads all five
    /// [`decode::sub8_intra_rect_hits`] cells -- 8x4 leaves, 4x8 leaves, the
    /// chroma-reference ones (which code the group's 4x4 chroma intra),
    /// `is_sub8x8_inter`-false MIXED groups, and the `PARTITION_SPLIT` 4x4
    /// leaves (the second refusal's own arm) -- so neither lifted refusal
    /// rests on a stream that never carried its shape.
    #[test]
    fn a_real_aomenc_inter_sequence_with_intra_sub8x8_leaves_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_intra_sub8x8_leaves_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            let msg = format!("no aomenc at {}", aomenc_path().display());
            assert!(
                std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none(),
                "EC_AV1_REQUIRE_AOMENC is set but {msg}"
            );
            eprintln!("SKIP {NAME}: {msg}");
            return;
        }
        const FRAMES: usize = 6;
        // One arm's stream, byte for byte: the lavfi source (a moving 24-row
        // random band over `128+50*sin(x/7)`) feeds aomenc through a pipe.
        let encode = |depth: u32, cq: u32, width: usize, height: usize, tiles: bool| -> Vec<u8> {
            let pix = if depth == 10 {
                "yuv420p10le"
            } else {
                "yuv420p"
            };
            let source = format!(
                "color=c=gray:s={width}x{height}:d=0.24:r=25,format=gray,\
                 geq=lum='if(lt(mod(Y+N*37\\,{height})\\,24)\\,random(N*100+X+Y)*255\\,128+50*sin(X/7))',\
                 format={pix}"
            );
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-t",
                    "0.24",
                    "-pix_fmt",
                    pix,
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
                "{NAME}: ffmpeg fixture (depth={depth} cq={cq} {width}x{height}): {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let cq_arg = format!("--cq-level={cq}");
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                &cq_arg,
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--enable-restoration=0",
                "--enable-palette=0",
                "--deltaq-mode=0",
                "--enable-intrabc=0",
                "--lag-in-frames=0",
                "--kf-max-dist=9999",
                "--enable-tx-size-search=1",
                "--enable-rect-partitions=1",
                // The 16x4/4x16 intra strip is a DIFFERENT (still refused)
                // shape; keeping 1:4 off is what makes a refusal here mean
                // this gate's own arm.
                "--enable-1to4-partitions=0",
                "--min-partition-size=4",
                "--max-partition-size=8",
            ];
            if depth == 10 {
                args.push("--bit-depth=10");
                args.push("--input-bit-depth=10");
            }
            // AOMENC FLAG PRECEDENCE (COMMON): the per-arm override goes last.
            args.push(if tiles {
                "--tile-columns=1"
            } else {
                "--tile-columns=0"
            });
            args.extend(["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused the fixture (depth={depth} cq={cq} {width}x{height}): {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };

        let mut refusals: Vec<String> = Vec::new();
        let mut exact_arms = 0u32;
        let mut fired = [0usize; 5];
        let mut checked_reproducible = false;
        let mut arms: Vec<(u32, u32, usize, usize, bool)> = Vec::new();
        for depth in [8u32, 10] {
            for cq in [48u32, 55, 63] {
                for (w, h) in [(128usize, 128usize), (192usize, 128usize)] {
                    arms.push((depth, cq, w, h, false));
                }
            }
        }
        // lane-sub8x4 r3: the `--tile-columns=1` arm COMMON's neighbour-map
        // rule asks for. It was excluded in r2 because it desynced at
        // decode-order frame 4 -- identically with `--min-partition-size=16
        // --max-partition-size=16`, i.e. with no sub-8x8 block at all. That was
        // main's `1d2259c` (the saved motion field kept only the LAST tile's
        // columns), merged into this lane in r3; the arm is restored here.
        arms.push((8, 63, 192, 128, true));
        for (depth, cq, width, height, tiles) in arms {
            let arm = format!("depth={depth} cq={cq} {width}x{height} tiles={tiles}");
            let stream = encode(depth, cq, width, height, tiles);
            if !checked_reproducible {
                // seeded-fixture-not-reproducible: prove the recipe names ONE
                // stream before any measurement rests on it.
                let again = encode(depth, cq, width, height, tiles);
                assert_eq!(
                    stream,
                    again,
                    "{NAME}: the fixture recipe is not reproducible ({arm}, {} vs {} bytes)",
                    stream.len(),
                    again.len()
                );
                checked_reproducible = true;
            }
            let before = decode::sub8_intra_rect_hits();
            let tiles_before = decode::tile_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.starts_with("unsupported: "),
                        "{NAME}: decode error that is not a named refusal ({arm}): {msg}"
                    );
                    refusals.push(format!("{arm}: {msg}"));
                    continue;
                }
            };
            assert_eq!(
                frames.len(),
                FRAMES,
                "{NAME}: {arm} decoded {} frames",
                frames.len()
            );
            assert!(
                !tiles || decode::tile_hits() - tiles_before >= 2,
                "{NAME}: {arm} decoded only {} tile(s)",
                decode::tile_hits() - tiles_before
            );
            let theirs = if depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, FRAMES)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, FRAMES)
            };
            for (i, (ours, theirs)) in frames.iter().zip(&theirs).enumerate() {
                assert_eq!(ours.y, theirs.y, "{NAME}: {arm} frame {i} luma vs ffmpeg");
                assert_eq!(ours.u, theirs.u, "{NAME}: {arm} frame {i} U vs ffmpeg");
                assert_eq!(ours.v, theirs.v, "{NAME}: {arm} frame {i} V vs ffmpeg");
            }
            let now = decode::sub8_intra_rect_hits();
            let delta = [
                now.0 - before.0,
                now.1 - before.1,
                now.2 - before.2,
                now.3 - before.3,
                now.4 - before.4,
            ];
            exact_arms += 1;
            for (slot, d) in fired.iter_mut().zip(delta) {
                *slot += d;
            }
            eprintln!(
                "{NAME}: {arm} EXACT 8x4={} 4x8={} chroma_ref={} mixed={} split4x4={}",
                delta[0], delta[1], delta[2], delta[3], delta[4]
            );
        }
        assert!(
            exact_arms >= 12,
            "{NAME}: only {exact_arms} arm(s) decoded and compared exact; refusals:\n{}",
            refusals.join("\n")
        );
        for (cell, what) in fired.iter().zip([
            "intra 8x4 leaves",
            "intra 4x8 leaves",
            "chroma-reference intra leaves",
            "is_sub8x8_inter-false MIXED groups",
            "intra 4x4 leaves of a sub-8x8 PARTITION_SPLIT",
        ]) {
            assert!(
                *cell > 0,
                "{NAME}: zero {what} over {exact_arms} pixel-exact arms -- the lifted refusal \
                 rests on nothing (counters {fired:?})"
            );
        }
        eprintln!(
            "{NAME}: {exact_arms} arms pixel-exact over {FRAMES} frames each, counters {fired:?}"
        );
    }

    /// lane-thread1: every per-frame/per-stream decode state now lives in
    /// [`crate::decode::FrameCtx`], not in a `thread_local!`. A static that
    /// comes back would silently re-break frame-parallel decoding (a worker
    /// thread sees an empty copy) without failing any pixel gate, so pin the
    /// 38 names by name against the sources themselves.
    #[test]
    fn no_per_frame_state_is_thread_local_any_more() {
        const MOVED: [&str; 38] = [
            "SB128",
            "CDEF_BITS",
            "BIT_DEPTH",
            "SUPERRES",
            "CDEF_TRANSMITTED",
            "CDEF_SB_COLS",
            "CDEF_IDX_GRID",
            "DELTA_Q_PRESENT",
            "DELTA_Q_RES",
            "CURRENT_Q_IDX",
            "QUANT_DELTAS",
            "DELTA_LF_PRESENT",
            "DELTA_LF_RES",
            "DELTA_LF_MULTI",
            "CURRENT_DELTA_LF",
            "SEG",
            "SEG_IDS",
            "PREV_SEG_IDS",
            "SEG_MI_DIMS",
            "ABOVE_SEG_PRED",
            "LEFT_SEG_PRED",
            "CUR_SEGMENT_ID",
            "SEG_TILE_ORIGIN",
            "TX_SELECT_INTER",
            "REDUCED_TX_SET_INTER",
            "ENABLE_FILTER_INTRA_INTER",
            "PALETTE_PRED",
            "INTRABC_MI_GRID",
            "INTRABC_DV",
            "LUMA_TX_TYPE",
            "INTRABC_CHROMA_TX",
            "INTRA_IN_INTER_MODE",
            "INTER_STRIP_CHROMA",
            "INTER_LAST_MC",
            "ENABLE_EDGE_FILTER",
            "LAST_FRAME_WIDE_MARGIN",
            "REACH_SB_PX",
            "GRAIN_BIT_DEPTH",
        ];
        let sources = [
            ("decode.rs", crate::test_source::module_source("decode")),
            ("encode.rs", crate::test_source::module_source("encode")),
            ("film_grain.rs", include_str!("../../film_grain.rs")),
        ];
        let mut back = Vec::new();
        for (name, src) in sources {
            for line in src.lines() {
                let t = line.trim_start().trim_start_matches("pub(crate) ");
                if let Some(rest) = t.strip_prefix("static ") {
                    let ident = rest.split(':').next().unwrap_or("").trim();
                    if MOVED.contains(&ident) {
                        back.push(format!("{name}: {ident}"));
                    }
                }
            }
        }
        assert!(
            back.is_empty(),
            "per-frame state is thread_local again (belongs in FrameCtx): {back:?}"
        );
        // ... and the scratch pools / gate counters DID stay thread-local:
        // this file's own counter reads run on the test thread.
        assert!(
            crate::test_source::module_source("decode").contains("thread_local!"),
            "the scratch pools and gate hit counters are still thread_local by design"
        );
    }

    /// lane-d792: a frame whose height the superblock grid CUTS, decoded
    /// against ffmpeg on every ladder point of BOTH reference encoders.
    ///
    /// WHY IT EXISTS: the reference encoders answer a straddling frame with
    /// rectangular partitions in the cut superblock row -- the only place
    /// these streams code a 16x8/8x4 transform unit at all -- and the rect
    /// intra dequant path was reading the FRAME's quantizer index where every
    /// square path reads the block's segment-adjusted one
    /// (`SEG_LVL_ALT_Q`, class `parsed-then-discarded`). Every gate size
    /// before this one was a whole number of superblocks, so no gate ever
    /// decoded such a unit: the 1920x792 native BD arm's reference-decode
    /// assertion was red on all four rav1e points and this is its small,
    /// fast, encoder-independent form.
    ///
    /// 384x152 is 2 superblocks + 24 px (the 1920x792 straddle, film A's own
    /// coded size) and 384x184 is 2 + 56 px (the 1920x1080 straddle), so both
    /// the "less than half inside" and the "more than half inside" edge
    /// superblock shapes are covered.
    #[test]
    fn a_straddling_frame_decodes_exactly_on_both_reference_encoders_ladders() {
        const NAME: &str = "a_straddling_frame_decodes_exactly_on_both_reference_encoders_ladders";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        const FRAMES: usize = 4;
        let mut checked = 0usize;
        // The SEGMENTED half of this gate, built BY RECIPE.
        //
        // The defect this gate exists for only shows on a stream whose
        // segments carry distinct `SEG_LVL_ALT_Q`, and neither `testsrc2` nor
        // the repo's own clip turns activity segmentation on at all (ledger
        // dead-end: 16 ladder points all pass with the defect still in). The
        // contrast is therefore built here -- half flat grey, half heavy
        // noise -- and libaom's `-aq-mode 1` answers it with segments whose
        // quantizer indices differ; `block_q_span()` asserts exactly that, so
        // the gate cannot go quietly blind (class `gate-blind-to-feature`).
        // This replaces two crops of a real film that could never be
        // committed and so SKIPped on every fresh clone.
        for (w, h) in [(384usize, 152usize), (384, 184)] {
            for crf in ["20", "35"] {
                let half = (w / 2).to_string();
                let flat = format!("color=c=gray:s={half}x{h}:r=25:d=1");
                let busy = format!("{flat},noise=alls=80:all_seed=7:allf=t+u");
                let args: Vec<String> = [
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &flat,
                    "-f",
                    "lavfi",
                    "-i",
                    &busy,
                    "-filter_complex",
                    "[0:v][1:v]hstack=inputs=2,format=yuv420p",
                    "-frames:v",
                    &FRAMES.to_string(),
                    "-pix_fmt",
                    "yuv420p",
                    "-an",
                    "-threads",
                    "1",
                    "-g",
                    &FRAMES.to_string(),
                    "-c:v",
                    "libaom-av1",
                    "-cpu-used",
                    "6",
                    "-b:v",
                    "0",
                    "-crf",
                    crf,
                    // libaom's activity segmentation, i.e. `SEG_LVL_ALT_Q`.
                    "-aq-mode",
                    "1",
                    "-f",
                    "obu",
                    "-",
                ]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
                let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args(&args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()), None, |c| c.output())
                    .expect("ffmpeg failed to run");
                if !out.status.success() {
                    eprintln!(
                        "SKIP {NAME}: ffmpeg cannot encode the segmented {w}x{h} recipe ({})",
                        String::from_utf8_lossy(&out.stderr)
                            .lines()
                            .last()
                            .unwrap_or("")
                    );
                    continue;
                }
                let stream = out.stdout;
                let want = ffmpeg_decode_sequence(&stream, w, h, FRAMES);
                crate::decode::reset_segment_hits();
                let got = decode_stream(&stream).unwrap_or_else(|e| {
                    panic!("{NAME}: the segmented {w}x{h} crf {crf} recipe refused: {e}")
                });
                let (lo, hi) = crate::decode::block_q_span();
                assert!(
                    hi > lo,
                    "{NAME}: the segmented {w}x{h} crf {crf} recipe dequantized every block \
                     with q {lo} -- no segment carried a distinct SEG_LVL_ALT_Q, so this row \
                     would pass with the rect dequant defect back in ({} segment ids seen)",
                    crate::decode::segment_ids_seen()
                );
                assert_eq!(
                    got.len(),
                    want.len(),
                    "{NAME}: segmented {w}x{h} frame count"
                );
                for (f, (o, r)) in got.iter().zip(&want).enumerate() {
                    for (plane, a, b) in [("Y", &o.y, &r.y), ("U", &o.u, &r.u), ("V", &o.v, &r.v)] {
                        if let Some(i) = a.iter().zip(b.iter()).position(|(x, y)| x != y) {
                            let stride = if plane == "Y" { w } else { w / 2 };
                            panic!(
                                "{NAME}: segmented {w}x{h} crf {crf}, frame {f} plane {plane} \
                                 sample ({}, {}): ours {} vs ffmpeg {} (block q span {lo}..{hi})",
                                i % stride,
                                i / stride,
                                a[i],
                                b[i],
                            );
                        }
                        assert_eq!(
                            a.len(),
                            b.len(),
                            "{NAME}: segmented {w}x{h} crf {crf}, frame {f} plane {plane}: ours \
                             has {} samples, ffmpeg's has {} -- a short plane would \
                             zip-truncate the walk above and hide a chroma-span defect",
                            a.len(),
                            b.len()
                        );
                    }
                }
                eprintln!("{NAME}: segmented {w}x{h} crf {crf} exact, block q span {lo}..{hi}");
                checked += 1;
            }
        }
        for (w, h) in [(384usize, 152usize), (384, 184)] {
            // The ENCODER-LADDER half: both reference encoders, every
            // ladder point, on real (non-bars) content. These rows code ONE
            // segment, so they do not gate `SEG_LVL_ALT_Q` -- that is the
            // recipe rows' job above -- but they are the rectangular
            // partition coverage rav1e's straddle answer adds.
            // lane-av1clipprobe: the one probe. This one is worse than a plain
            // skip -- it `return`s from the middle of the gate, so everything
            // asserted above it had already run and the encoder-ladder half
            // silently never did, leaving a half-executed gate reported green.
            let Some(clip) =
                crate::library_fixture::require("video/h264-1080p-23.976-8bit.mp4", NAME)
            else {
                return;
            };
            for (encoder, points) in [
                ("librav1e", ["50", "100", "150", "200"]),
                ("libaom-av1", ["5", "20", "35", "45"]),
            ] {
                for point in points {
                    let mut args: Vec<String> =
                        ["-v", "error", "-i", clip.to_str().expect("fixture path")]
                            .iter()
                            .map(|s| (*s).to_string())
                            .collect();
                    args.extend(
                        [
                            "-frames:v",
                            &FRAMES.to_string(),
                            "-vf",
                            &format!("crop={w}:{h}:768:400"),
                            "-pix_fmt",
                            "yuv420p",
                            "-an",
                        ]
                        .iter()
                        .map(|s| (*s).to_string()),
                    );
                    args.extend(
                        ["-threads", "1", "-g", &FRAMES.to_string(), "-c:v", encoder]
                            .iter()
                            .map(|s| (*s).to_string()),
                    );
                    if encoder == "librav1e" {
                        args.push("-rav1e-params".into());
                        args.push(format!(
                            "speed=6:quantizer={point}:tile_cols=1:tile_rows=1:threads=1"
                        ));
                    } else {
                        args.extend(
                            // `-aq-mode 1`: libaom's activity segmentation,
                            // i.e. `SEG_LVL_ALT_Q` -- the feature the rect
                            // dequant path used to drop.
                            [
                                "-cpu-used",
                                "6",
                                "-b:v",
                                "0",
                                "-crf",
                                point,
                                "-aq-mode",
                                "1",
                            ]
                            .iter()
                            .map(|s| (*s).to_string()),
                        );
                    }
                    args.extend(["-f", "obu", "-"].iter().map(|s| (*s).to_string()));
                    let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                        .args(&args)
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped()), None, |c| c.output())
                        .expect("ffmpeg failed to run");
                    if !out.status.success() {
                        eprintln!(
                            "SKIP {NAME}: ffmpeg cannot encode {w}x{h} with {encoder} ({})",
                            String::from_utf8_lossy(&out.stderr)
                                .lines()
                                .last()
                                .unwrap_or("")
                        );
                        continue;
                    }
                    let stream = out.stdout;
                    let want = ffmpeg_decode_sequence(&stream, w, h, FRAMES);
                    let got = decode_stream(&stream).unwrap_or_else(|e| {
                        panic!("{NAME}: {encoder} {point} at {w}x{h} refused: {e}")
                    });
                    assert_eq!(
                        got.len(),
                        want.len(),
                        "{NAME}: {encoder} {point} at {w}x{h} frame count"
                    );
                    for (f, (o, r)) in got.iter().zip(&want).enumerate() {
                        for (plane, a, b) in
                            [("Y", &o.y, &r.y), ("U", &o.u, &r.u), ("V", &o.v, &r.v)]
                        {
                            if let Some(i) = a.iter().zip(b.iter()).position(|(x, y)| x != y) {
                                let stride = if plane == "Y" { w } else { w / 2 };
                                panic!(
                                    "{NAME}: {encoder} {point} at {w}x{h}, frame {f} plane \
                                     {plane} sample ({}, {}): ours {} vs ffmpeg {}",
                                    i % stride,
                                    i / stride,
                                    a[i],
                                    b[i],
                                );
                            }
                            assert_eq!(
                                a.len(),
                                b.len(),
                                "{NAME}: {encoder} {point} at {w}x{h}, frame {f} plane {plane}: \
                                 ours has {} samples, ffmpeg's has {} -- a short plane would \
                                 zip-truncate the walk above and hide a chroma-span defect",
                                a.len(),
                                b.len()
                            );
                        }
                    }
                    checked += 1;
                }
            }
        }
        assert!(
            checked > 0,
            "{NAME}: neither reference encoder was available"
        );
        eprintln!("{NAME}: {checked} straddling reference points decoded sample-exact");
    }

    /// lane-seg2: a SEGMENTED key frame that also codes sub-8x8 INTRA leaves.
    ///
    /// WHY IT EXISTS: `read_intra_mode_sub8` -- the reader every `BLOCK_4X4`
    /// split leaf and every 8x4/4x8 leaf goes through -- never called spec
    /// 5.11.6's `intra_segment_id` at all. On a stream with segmentation on
    /// it therefore (a) left the block's `segment_id` symbol unread, so the
    /// tile desynced from the frame's first sub-8x8 partition on, and (b)
    /// dequantized the leaf with the PREVIOUS block's `SEG_LVL_ALT_Q`.
    /// Reading it with a hardcoded 1x1 mi footprint was the second half of
    /// the same defect: an 8x4 leaf left its second mi cell at segment 0,
    /// which the next block read back as its spatial prediction and CDF row.
    ///
    /// RECIPE: flat gray + heavy `noise` (the contrast is what turns libaom's
    /// activity segmentation on -- a `testsrc2`/bars source codes ONE segment
    /// and passes with the defect in, ledger dead-end `gate film rows are
    /// colour bars`), at `-crf 5 -cpu-used 2`, which is the measured corner
    /// where libaom answers with sub-8x8 intra partitions. Measured on this
    /// exact stream: red before either half of the fix (134k/134k samples
    /// off), green with both.
    #[test]
    fn a_segmented_key_frame_with_sub_8x8_intra_leaves_decodes_exactly() {
        const NAME: &str = "a_segmented_key_frame_with_sub_8x8_intra_leaves_decodes_exactly";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        const FRAMES: usize = 4;
        let (w, h) = (384usize, 152usize);
        let src = format!("color=c=gray:s={w}x{h}:r=25,noise=alls=80:all_seed=7:allf=t+u");
        let args: Vec<String> = [
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &src,
            "-frames:v",
            "4",
            "-pix_fmt",
            "yuv420p",
            "-an",
            "-threads",
            "1",
            "-g",
            "4",
            "-c:v",
            "libaom-av1",
            "-cpu-used",
            "2",
            "-b:v",
            "0",
            "-crf",
            "5",
            "-aq-mode",
            "1",
            "-f",
            "obu",
            "-",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("ffmpeg failed to run");
        if !out.status.success() {
            eprintln!(
                "SKIP {NAME}: ffmpeg cannot encode with libaom-av1 ({})",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .last()
                    .unwrap_or("")
            );
            return;
        }
        let stream = out.stdout;
        crate::decode::reset_segment_hits();
        let want = ffmpeg_decode_sequence(&stream, w, h, FRAMES);
        let got = decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME}: refused: {e}"));
        assert_eq!(got.len(), want.len(), "{NAME}: frame count");
        for (f, (o, r)) in got.iter().zip(&want).enumerate() {
            for (plane, a, b) in [("Y", &o.y, &r.y), ("U", &o.u, &r.u), ("V", &o.v, &r.v)] {
                if let Some(i) = a.iter().zip(b.iter()).position(|(x, y)| x != y) {
                    let stride = if plane == "Y" { w } else { w / 2 };
                    panic!(
                        "{NAME}: frame {f} plane {plane} sample ({}, {}): ours {} vs ffmpeg {}",
                        i % stride,
                        i / stride,
                        a[i],
                        b[i],
                    );
                }
                assert_eq!(
                    a.len(),
                    b.len(),
                    "{NAME}: frame {f} plane {plane}: ours has {} samples, ffmpeg's has {} -- a \
                     short plane would zip-truncate the walk above and hide a chroma-span \
                     defect",
                    a.len(),
                    b.len()
                );
            }
        }
        // The property is asserted, not hoped for: without more than one
        // segment this row would pass with the defect still in.
        assert!(
            crate::decode::segment_ids_seen() >= 2,
            "{NAME}: vacuous run -- {} distinct segment ids",
            crate::decode::segment_ids_seen()
        );
        eprintln!(
            "{NAME}: {FRAMES} frames sample-exact, {} segment_id symbols, {} distinct ids",
            crate::decode::segment_id_hits(),
            crate::decode::segment_ids_seen()
        );
    }

    /// lane-av1leftref: the left chroma reference's `+ss_y` ROW term
    /// (libaom `av1_common_int.h:1400-1401`, `chroma_left_mi =
    /// base_mi[ss_y * mi_stride - 1]` -- one mi row DOWN at 4:2:0 and 4:4:0,
    /// on its own row at 4:2:2 and 4:4:4). This decoder read its own row at
    /// every subsampling until this lane.
    ///
    /// The read is only observable when the two cells hold DIFFERENT blocks --
    /// a left neighbour at most 1 mi tall (1 mi = 4 luma px, so a 4x4/8x4/16x4
    /// /32x4/64x4 leaf). lane-av1leftwit measured that shape off a per-cell
    /// owner stamp instead of the mode value: `uv_left_1mi_split_hits()` counts
    /// decode-path reads whose two cells were written by DIFFERENT blocks, and
    /// it is NOT zero -- five committed fixtures carry it (15 reads in all,
    /// `lanes/av1leftwit.report.md`). What no committed fixture shows is the two
    /// blocks DISAGREEING on `uv_mode`, which is what `mode_diff`/`smooth_diff`
    /// below measure. So this gate pins both halves honestly: the arm runs
    /// thousands of times, the shape it needs is genuinely reached, and the
    /// term has not yet been seen to change an answer. Its exactness arms are
    /// what would decide the shipped read if a DIFF bucket ever went non-zero.
    ///
    /// A non-zero DIFF bucket is not a failure here -- it is the first stream
    /// that witnesses the term, and the exactness arms below are what then
    /// decide whether the shipped read is the one that matches the oracle.
    #[test]
    fn a_4to20_key_frame_takes_the_left_chroma_reference_read_its_libaom_row() {
        const NAME: &str = "a_4to20_key_frame_takes_the_left_chroma_reference_read_its_libaom_row";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        // 3840x1608 10-bit key frame, the densest 4:2:0 intra leaf mix in the
        // fixture set and the stream that carries the most `+ss_y` reads.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/reelb_kf900.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (3840usize, 1608usize);
        let before = crate::decode::uv_left_ss_y_reads();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let reads = crate::decode::uv_left_ss_y_reads() - before;
        let mode_diff = crate::decode::uv_left_ss_y_mode_diff_hits();
        let smooth_diff = crate::decode::uv_left_ss_y_smooth_diff_hits();
        assert!(
            reads > 1000,
            "{NAME}: only {reads} chroma edge-filter reads took the `+ss_y` left \
             reference -- the arm this test pins is barely exercised, so the gate \
             proves little"
        );
        assert_eq!(frames.len(), 1, "{NAME}: one key frame");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        // The term is unreachable on this stream, so luma and BOTH chroma
        // planes must come out byte-identical to ffmpeg's decode. Luma alone
        // (what the kf900 gate asserts) would not notice a chroma edge-filter
        // type moving.
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg");
        eprintln!(
            "{NAME}: {reads} left-reference reads, +ss_y changed the mode {mode_diff} \
             and the smooth boolean {smooth_diff} times, all three planes pixel-exact"
        );
    }

    /// lane-av1leftwit: the `+ss_y` term's SHAPE is reachable on the decode
    /// path, and this is the gate that says so.
    ///
    /// lane-av1leftref shipped the left read's `+ss_y` row term with the
    /// argument that the shape it needs -- a left neighbour at most 1 mi tall,
    /// so the cell this decoder read and the cell libaom reads belong to
    /// different blocks -- is "structurally absent, because aomenc never starts
    /// a block on an odd mi row". Both halves of that are wrong. libaom's
    /// `PARTITION_HORZ_4` puts its `i == 0` child on the PARENT's own mi row
    /// (`decode_partition`, `av1/decoder/decodeframe.c`: `mi_row + i * bw / 4`),
    /// and a 16x16 parent sits on a 4-mi grid, so a 1-mi-tall leaf at an EVEN
    /// row is what the syntax asks for, not forbids. And the shape is measured
    /// in the corpus: these two fixtures each present it to a real decode-path
    /// read.
    ///
    /// The counter is `decode::uv_left_1mi_split_hits()`, incremented only
    /// where `decode_path` is true -- the snapping form every decode call site
    /// uses -- so the 16x4/4x16 pair witness's counter-only mirror (which
    /// reads an odd mi row and feeds `RECT4_16_UV_PAIR_FILT_HITS`) cannot
    /// inflate it. `UV_LEFT_SS_Y_READS` alone would prove nothing: it counts
    /// every read with a left neighbour, whether or not the two cells differ.
    ///
    /// NON-VACUITY: the `uv_owner` stamp the counter turns on was removed for
    /// one build; both arms fell to 0 and this gate failed on the census
    /// (report §5), so a decoder that stops recording which block owns a cell
    /// fails here instead of passing vacuously. It is deliberately NOT a
    /// mutation of the `+ss_y` term itself: the term's own mutation leaves
    /// these reads untouched, which is the honest limit of what the fixture
    /// set can decide (the kf900 gate above owns the exactness side).
    #[test]
    fn a_committed_4to20_stream_presents_a_one_mi_left_neighbour_to_a_decode_path_read() {
        const NAME: &str =
            "a_committed_4to20_stream_presents_a_one_mi_left_neighbour_to_a_decode_path_read";
        // (fixture, frames, width, height, measured split-owner reads)
        let arms: [(&str, usize, usize, usize, usize); 2] = [
            ("gm_small_side_witness.obu", 33, 1920, 792, 5),
            ("reelc_sb128_inter_witness.obu", 15, 1920, 792, 7),
        ];
        for (fixture, want_frames, width, height, want_split) in arms {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(fixture);
            let stream = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
            let before_split = crate::decode::uv_left_1mi_split_hits();
            let before_reads = crate::decode::uv_left_ss_y_reads();
            let before_mode = crate::decode::uv_left_ss_y_mode_diff_hits();
            let before_smooth = crate::decode::uv_left_ss_y_smooth_diff_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => panic!("{NAME}: {fixture} refused: {e}"),
            };
            let split = crate::decode::uv_left_1mi_split_hits() - before_split;
            assert_eq!(frames.len(), want_frames, "{NAME}: {fixture} frame count");
            assert!(
                frames
                    .iter()
                    .all(|f| (f.width, f.height) == (width, height)),
                "{NAME}: {fixture} is not {width}x{height}"
            );
            assert_eq!(
                split, want_split,
                "{NAME}: {fixture} presented {split} 1-mi-tall left neighbours to \
                 a decode-path read, measured {want_split} -- the census this \
                 gate pins moved"
            );
            // Printed, not asserted: `== 0` would pin an accident, and a
            // non-zero value is the first WITNESS of the term rather than a
            // failure (that is what the kf900 gate above then decides against
            // the oracle).
            eprintln!(
                "{NAME}: {fixture}: {split} 1-mi-tall left neighbours in \
                 {} decode-path reads, +ss_y changed the mode {} and the smooth \
                 boolean {} times",
                crate::decode::uv_left_ss_y_reads() - before_reads,
                crate::decode::uv_left_ss_y_mode_diff_hits() - before_mode,
                crate::decode::uv_left_ss_y_smooth_diff_hits() - before_smooth,
            );
        }
    }

    /// lane-av1formatsweep: the ODD CODED DIMENSION cell -- a frame whose
    /// width or height is not a multiple of 8, so the last mode-info column
    /// and the last chroma plane row/column are PARTIAL (`mi_cols * 8 >
    /// width`, and at 4:2:0 the chroma plane is `ceil(w/2) x ceil(h/2)`).
    ///
    /// Nothing else in this file ever encoded one: every frame size in the
    /// suite (the `tiny_frame_size_sweep` ladder included -- 8,16,24,32,48,
    /// 64,72,136,192) is a multiple of 8, so the partial-chroma-plane walk
    /// had no committed exactness evidence at any depth. The class matters
    /// for a 4:2:2 lift too: at ss (1,0) the chroma plane is
    /// `ceil(w/2) x h`, so the partial extent lands on the OTHER axis.
    ///
    /// NON-VACUITY: `decode::inter_edge_strip_hits()` (the frame-edge strip
    /// walk) is asserted to fire, and the even-size control arm is asserted
    /// to leave it at zero -- measured: 130x122 -> `[0,2,0,4,0,6]`,
    /// 130x130 -> `[0,3,0,2,24,6]`, 128x96 -> all zero. Without that pair a
    /// decoder that dropped the partial column entirely would still pass the
    /// pixel compare (class `gate-blind-to-feature`).
    ///
    /// Recipe (deterministic: single pass, single thread, no row-mt):
    /// `ffmpeg -f lavfi -i testsrc2=size=<W>x<H>:rate=25 -frames:v 4
    /// -pix_fmt <pix> -f yuv4mpegpipe -` piped to
    /// `aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20
    /// --cpu-used=2 --threads=1 --row-mt=0 --lag-in-frames=0
    /// --kf-max-dist=100 --limit=4 --obu -o - -`, plus
    /// `--input-bit-depth=N --bit-depth=N` at 10 and 12 bits. The 12-bit arm
    /// uses the smoothed `mandelbrot` source because a sharp-edged source
    /// makes aomenc set `allow_screen_content_tools`, which this decoder
    /// refuses BY NAME at 12 bits (see `a_12bit_screen_content_stream_is_
    /// refused_by_name`).
    ///
    /// NON-VACUITY, RED-BEFORE PROVEN: the mode-info grid's ceil is the
    /// arithmetic this cell depends on, and mutating it turns every arm red
    /// -- `ec-av1-syntax`'s `h.mi_cols = 2 * ((h.frame_width + 7) >> 3)`
    /// floored to `2 * (h.frame_width >> 3)` makes the first arm fail at
    /// decode-order frame 0, byte 128 (ours 74 vs oracle 170, 11538 bytes
    /// differ); reverted, the gate is green again. A NEARER mutation,
    /// flooring `decode::round_ss` from libaom's `ROUND_POWER_OF_TWO`,
    /// leaves this gate GREEN and is not evidence either way: aomenc rounds
    /// a 4:2:0 frame's coded size DOWN to even, so no stream this recipe can
    /// produce codes an odd luma dimension, and the chroma-extent ceil only
    /// differs from its floor when the luma dimension is itself odd.
    ///
    /// Also asserted, so a recipe that silently became even-dimensioned can
    /// not pass as this cell: the header's `ss (1,1)` and bit depth, the
    /// partial mode-info grid (`mi*8 > dim`), every frame's chroma plane
    /// being `ceil(w/2)*ceil(h/2)`, and
    /// `decode::inter_edge_strip_hits()` non-zero after a per-arm reset --
    /// measured per arm: `[0,6,0,4,48,12]`, `[0,0,0,4,0,6]`,
    /// `[0,4,0,10,0,10]`, `[0,4,0,8,0,12]`, `[0,4,0,8,0,8]`,
    /// `[0,0,0,12,0,14]`, where the even-size control (128x96) reads all
    /// zeros. Without the counter a decoder that dropped the partial column
    /// entirely would still pass the pixel compare (class
    /// `gate-blind-to-feature`).
    #[test]
    fn a_real_aomenc_odd_coded_dimension_streams_decode_pixel_exact() {
        const NAME: &str = "a_real_aomenc_odd_coded_dimension_streams_decode_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        // (width, height, pix_fmt, bit_depth, smooth_source)
        let arms: &[(usize, usize, &str, usize, bool)] = &[
            (130, 130, "yuv420p", 8, false),
            (66, 66, "yuv420p", 8, false),
            (194, 122, "yuv420p", 8, false),
            (130, 122, "yuv420p", 8, false),
            (130, 122, "yuv420p10le", 10, false),
            (130, 122, "yuv420p12le", 12, true),
        ];
        let mut odd_arms = 0usize;
        for &(width, height, pix_fmt, bit_depth, smooth) in arms {
            let stream = odd_dim_stream(width, height, pix_fmt, bit_depth, smooth);
            // The header must really carry the odd geometry, or the arm
            // silently degrades to an even-dimension stream (class
            // `arrival-not-intent`).
            let (mi_cols, mi_rows) = odd_dim_header_mi_grid(&stream, NAME, bit_depth);
            assert!(
                mi_cols * 8 > width || mi_rows * 8 > height,
                "{NAME}: {width}x{height} is not an odd coded dimension \
                 (mi grid {mi_cols}x{mi_rows})"
            );
            // Reset per attempt, never a delta across arms (class
            // `counter-from-refused-stream`).
            crate::decode::reset_inter_edge_strip_hits();
            let (frames_decoded, _hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            let frames = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME}: refused {width}x{height}: {e}"));
            let chroma = width.div_ceil(2) * height.div_ceil(2);
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(f.y.len(), width * height, "{NAME}: frame {i} luma extent");
                assert_eq!(
                    f.u.len(),
                    chroma,
                    "{NAME}: frame {i} chroma extent -- a {width}x{height} frame's \
                     chroma plane is ceil(w/2)*ceil(h/2) = {chroma}, not w/2*h/2"
                );
            }
            let edge = crate::decode::inter_edge_strip_hits();
            assert!(
                edge.iter().any(|&n| n > 0),
                "{NAME}: {width}x{height} decoded byte-exact but walked NO \
                 frame-edge strip -- the partial-column chroma extent this \
                 cell exists for was never reached (class \
                 `gate-blind-to-feature`): {edge:?}"
            );
            eprintln!(
                "{NAME}: {width}x{height} {pix_fmt}: {frames_decoded} frames \
                 byte-exact vs aomdec, frame-edge walk {edge:?}"
            );
            odd_arms += 1;
        }
        assert!(odd_arms == arms.len(), "{NAME}: arm loop ended early");
    }

    /// lane-av1formatsweep: the 4:4:4 x 10-BIT cell. Every committed 4:4:4
    /// exactness gate in this file is `--lossless=1` at 8 bits
    /// (`a_real_aomenc_lossless_444_key_frame_decodes_sample_exact`,
    /// `a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact`,
    /// `a_lossless_444_defaultp_inter_strip_stream_decodes_byte_exact`, the
    /// `ll444_*` pins), so the whole committed 4:4:4 evidence base is LOSSLESS
    /// 8-bit: no 4:4:4 stream above 8 bits had a committed exactness gate.
    ///
    /// NON-VACUITY: `decode::rect_split_lossless_chroma444_hits()` -- the
    /// per-4x4 chroma unit walk a lossless HORZ/VERT strip takes at
    /// subsampling 0/0 -- is asserted to fire (measured 32 units on this
    /// recipe), and the sequence header is asserted to say ss (0,0) at
    /// 10-bit before any pixel is trusted. A 4:2:0 stream of the same
    /// content leaves that counter at zero, so the pixel compare alone would
    /// not prove the 4:4:4 chroma path ran (class `gate-blind-to-feature`).
    ///
    /// Recipe: `testsrc2 128x96` yuv444p10le 6 frames, `aomenc --codec=av1
    /// --profile=1 --input-bit-depth=10 --bit-depth=10 --lossless=1
    /// --enable-palette=0 --enable-intrabc=0 --cq-level=20 --cpu-used=2
    /// --passes=1 --end-usage=q --threads=1 --row-mt=0 --lag-in-frames=0
    /// --kf-max-dist=100 --limit=6 --obu -o - -`.
    #[test]
    fn a_lossless_444_10bit_inter_stream_decodes_pixel_exact() {
        const NAME: &str = "a_lossless_444_10bit_inter_stream_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let stream = chroma_format_stream(
            128,
            96,
            "yuv444p10le",
            10,
            6,
            &[
                "--lossless=1",
                "--enable-palette=0",
                "--enable-intrabc=0",
                // `--cpu-used=2`, NOT 0: at `--cpu-used=0` aomenc reaches
                // the 128-root partition (measured `part128: split=1
                // none=5` where this recipe reads `none=0`) and THAT stream
                // diverges at 10-bit 4:4:4 from frame 1 on -- 11152 of
                // 36864 samples, first at Y(8,4), with ffmpeg and aomdec
                // agreeing on the reference. Handoff, not this gate's
                // subject; see `lanes/av1formatsweep.report.md`.
                "--cpu-used=2",
                "--cq-level=20",
            ],
            0,
        );
        assert_444_header(&stream, NAME, 10);
        let before_444 = crate::decode::rect_split_lossless_chroma444_hits();
        let (frames_decoded, _hidden) = decode_all_frames_vs_oracle(&stream, NAME);
        assert!(
            crate::decode::rect_split_lossless_chroma444_hits() > before_444,
            "{NAME} went blind: no lossless strip walked its chroma as 4:4:4 \
             4x4 units"
        );
        assert!(
            frames_decoded >= 6,
            "{NAME}: expected the six-frame recipe to decode"
        );
        eprintln!("{NAME}: {frames_decoded} frames byte-exact vs aomdec at 10-bit 4:4:4");
    }

    /// lane-av1formatsweep: the 4:4:4 x 12-BIT cell -- the deepest chroma
    /// format the spec allows, and the one no committed gate reached. A
    /// sharp-edged source makes aomenc set `allow_screen_content_tools`,
    /// which this decoder refuses BY NAME at 12 bits (palette and intrabc
    /// have no 12-bit witness), so the arm encodes the SMOOTHED
    /// `mandelbrot` source -- the same trick `a_real_aomenc_12bit_stream_
    /// decodes_pixel_exact`'s in-tree source exists for.
    ///
    /// NON-VACUITY: the header is asserted to say ss (0,0) at 12 bits,
    /// `mc::mc_subpel_hits()` (the 12-bit-sensitive subpel round pair) must
    /// fire -- a key-frame-only stream would pass the pixel compare without
    /// touching it -- and every decoded frame's chroma plane is asserted to
    /// be FULL resolution, which is the 4:4:4 extent itself.
    ///
    /// Recipe: `mandelbrot 128x96` + `-vf gblur=sigma=6` yuv444p12le 6 frames,
    /// `aomenc --codec=av1 --input-bit-depth=12 --bit-depth=12 --passes=1
    /// --end-usage=q --cq-level=20 --cpu-used=2 --threads=1 --row-mt=0
    /// --lag-in-frames=0 --kf-max-dist=100 --limit=6 --obu -o - -`.
    /// aomenc picks `seq_profile=2` here (12-bit forces it) with
    /// `subsampling_x/y = 0,0` -- still 4:4:4 planes. lane-av1422lift: 4:2:2
    /// (1,0) is DECODED now, so the only subsampling the decoder still asserts
    /// against is the uncodable 4:4:0 (0,1) cell, closed by
    /// `the_440_cell_is_not_a_codable_chroma_shape`.
    #[test]
    fn a_444_12bit_inter_sequence_decodes_pixel_exact() {
        const NAME: &str = "a_444_12bit_inter_sequence_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let stream = chroma_format_stream(128, 96, "yuv444p12le", 12, 6, &["--cq-level=20"], 1);
        assert_444_header(&stream, NAME, 12);
        let before_subpel = crate::mc::mc_subpel_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: refused a real 12-bit 4:4:4 stream: {e}"));
        let (frames_decoded, _hidden) = decode_all_frames_vs_oracle(&stream, NAME);
        assert!(
            crate::mc::mc_subpel_hits() > before_subpel,
            "{NAME} went blind: no subpel inter prediction ran, so the \
             12-bit round pair is unexercised"
        );
        // The 4:4:4 chroma EXTENT is the cell's own claim: at ss (0,0) the
        // chroma planes are full resolution, so a decoder that kept the
        // 4:2:0 extent would produce quarter-size chroma planes and a
        // byte-length the oracle cannot even match. (CDEF is NOT asserted
        // here: on this smoothed source CDEF runs but skips every band --
        // measured `cdef_band: rect_skip_writes=11`, zero index literals --
        // so a CDEF assertion would be a claim the witness cannot carry.)
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(
                f.u.len(),
                128 * 96,
                "{NAME}: frame {i} chroma plane is {} samples -- at 4:4:4 it \
                 must be full resolution (128x96), not the 4:2:0 quarter",
                f.u.len()
            );
        }
        eprintln!("{NAME}: {frames_decoded} frames byte-exact vs aomdec at 12-bit 4:4:4");
    }

    /// lane-av1tilemeasure: 12-BIT 4:2:0 with tile ROWS genuinely enabled, and
    /// with a 2x2 tile grid. `gate_coverage.rs` records that the multi-tile
    /// 2D grid is covered "at both bit depths" -- meaning 8 and 10; 12 was the
    /// gap, and the sweep's 12-bit "tile rows" cell (t420_12_row) never
    /// encoded an untiled control, so nobody could say which stream it had
    /// measured.
    ///
    /// Recipe (per arm): `mandelbrot 256x256` + `-vf gblur=sigma=6`
    /// yuv420p12le 6 frames (the 12-bit screen-tools refusal fires on a sharp
    /// source -- H6), `aomenc --codec=av1 --passes=1 --end-usage=q
    /// --cq-level=20 --cpu-used=2 --enable-palette=0 --enable-intrabc=0
    /// --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 --limit=6
    /// --input-bit-depth=12 --bit-depth=12 --obu -o - -` plus `--tile-rows=1`
    /// (arm 1) or `--tile-columns=1 --tile-rows=1` (arm 2).
    ///
    /// NON-VACUITY, in the order a reader should check it:
    /// 1. the sequence header is `ss (1,1)` at 12 bits
    ///    (`odd_dim_header_mi_grid`);
    /// 2. EVERY frame header's parsed `tile_info` is 1x2 / 2x2 -- the trap-2
    ///    guard: an untiled stream fails here before a pixel is read;
    /// 3. `decode::tile_hits()` advances by at least one hit per tile per
    ///    frame, so tile 1 (and the second ROW) really decoded rather than the
    ///    first tile being decoded twice;
    /// 4. every decoded plane is 256*256 samples -- a truncated or
    ///    quarter-extent decode fails the shape assert;
    /// 5. `decode_all_frames_vs_oracle` asserts the decode-order frame COUNT
    ///    and each frame's byte LENGTH against the instrumented `aomdec`
    ///    before any sample is compared (a prefix compare is not a compare --
    ///    the same report's trap 1).
    #[test]
    fn a_real_aomenc_12bit_stream_with_two_tile_rows_and_a_two_by_two_tile_grid_decodes_pixel_exact()
     {
        const NAME: &str = "a_real_aomenc_12bit_stream_with_two_tile_rows_and_a_two_by_two_tile_grid_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let arms: [(&str, &[&str], u32, u32); 2] = [
            ("two tile rows", &["--tile-rows=1"], 1, 2),
            (
                "a 2x2 tile grid",
                &["--tile-columns=1", "--tile-rows=1"],
                2,
                2,
            ),
        ];
        for (label, tiles, want_cols, want_rows) in arms {
            let mut extra = vec![
                "--cq-level=20",
                "--cpu-used=2",
                "--enable-palette=0",
                "--enable-intrabc=0",
            ];
            extra.extend_from_slice(tiles);
            let stream = chroma_format_stream(256, 256, "yuv420p12le", 12, 6, &extra, 1);
            odd_dim_header_mi_grid(&stream, NAME, 12);
            let facts = frame_facts(&stream, NAME);
            assert_eq!(
                facts.len(),
                6,
                "{NAME} ({label}): expected six frame headers, parsed {}",
                facts.len()
            );
            for (i, f) in facts.iter().enumerate() {
                assert_eq!(
                    (f.tile_cols, f.tile_rows),
                    (want_cols, want_rows),
                    "{NAME} ({label}): decode-order frame {i} parses tile_info {}x{}, not \
                     {want_cols}x{want_rows} -- the aomenc tile flag was a NO-OP at this \
                     geometry, so this arm would be measuring an untiled stream",
                    f.tile_cols,
                    f.tile_rows
                );
            }
            let before_tiles = crate::decode::tile_hits();
            let frames = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME} ({label}): refused the stream: {e}"));
            let tile_hits = crate::decode::tile_hits() - before_tiles;
            assert!(
                tile_hits >= facts.len() * (want_cols * want_rows) as usize,
                "{NAME} ({label}): tile_hits advanced by {tile_hits} over {} frames of a \
                 {}x{} grid -- not every tile decoded",
                facts.len(),
                want_cols,
                want_rows
            );
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(
                    f.y.len(),
                    256 * 256,
                    "{NAME} ({label}): frame {i} luma plane is {} samples, not 256x256",
                    f.y.len()
                );
            }
            let (frames_decoded, _hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert!(
                frames_decoded >= 6,
                "{NAME} ({label}): {frames_decoded} decode-order frames"
            );
            eprintln!(
                "{NAME} ({label}): {frames_decoded} frames byte-exact vs aomdec, tiles \
                 {want_cols}x{want_rows} confirmed on every frame header, tile_hits {tile_hits}"
            );
        }
    }

    /// lane-av1tilemeasure: 4:4:4 SUPERRES at 8, 10 and 12 bits. The sweep's
    /// superres row was `--superres-mode=1` alone, and measured that recipe
    /// at 4:4:4 8 bits: this lane re-encoded it and read `use_superres` on
    /// every frame header -- ZERO frames carried it, so that cell never
    /// exercised the upscaler at all (the same "an aomenc flag is not
    /// evidence" trap as tile rows, one axis over). libaom only signals
    /// `use_superres` when a DENOMINATOR is given, so the arms here spell
    /// `--superres-denominator=12 --superres-kf-denominator=12` and the
    /// header says `use_superres=1` on all four frames.
    ///
    /// Recipe: `testsrc2 128x128` yuv444p10le (12-bit: the gblur'd
    /// `mandelbrot`, H6) 4 frames, `aomenc --codec=av1 --passes=1
    /// --end-usage=q --cq-level=20 --cpu-used=2 --enable-palette=0
    /// --enable-intrabc=0 --superres-mode=1 --superres-denominator=12
    /// --superres-kf-denominator=12 --threads=1 --row-mt=0 --lag-in-frames=0
    /// --kf-max-dist=100 --limit=4 --input-bit-depth=N --bit-depth=N --obu
    /// -o - -`. Coded width comes out 85px, upscaled to 128.
    ///
    /// NON-VACUITY: `ss (0,0)` at the claimed depth; EVERY frame header has
    /// `use_superres` with a coded width strictly below the upscaled one (a
    /// stream without it fails here, not at the pixel compare);
    /// `superres::superres_hits()` advances once per frame (the upscaler
    /// really ran); every decoded plane is the UPSCALED 128x128 -- a decoder
    /// that forgot the upsample would hand back 85x128 and fail the shape
    /// assert, and at `ss (0,0)` the chroma planes must be full resolution
    /// too; then the length-asserting `aomdec` compare.
    #[test]
    fn a_real_aomenc_444_superres_stream_at_8_10_and_12_bit_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_444_superres_stream_at_8_10_and_12_bit_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        for (depth, pix_fmt, smooth) in [
            (8usize, "yuv444p", 0u8),
            (10, "yuv444p10le", 0),
            (12, "yuv444p12le", 1),
        ] {
            let stream = chroma_format_stream(
                128,
                128,
                pix_fmt,
                depth,
                4,
                &[
                    "--cq-level=20",
                    "--cpu-used=2",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--superres-mode=1",
                    "--superres-denominator=12",
                    "--superres-kf-denominator=12",
                ],
                smooth,
            );
            assert_444_header(&stream, NAME, depth);
            let facts = frame_facts(&stream, NAME);
            for (i, f) in facts.iter().enumerate() {
                assert!(
                    f.use_superres,
                    "{NAME} ({depth}-bit): decode-order frame {i} has use_superres=0 -- the \
                     superres cell would be measuring an ordinary 4:4:4 stream"
                );
                assert!(
                    f.coded_w < f.upscaled_w,
                    "{NAME} ({depth}-bit): frame {i} codes {}px and upscaled_width says {}px",
                    f.coded_w,
                    f.upscaled_w
                );
                assert_eq!(f.upscaled_w, 128, "{NAME} ({depth}-bit): upscaled width");
            }
            let before_sr = crate::superres::superres_hits();
            let frames = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME} ({depth}-bit): refused the stream: {e}"));
            let superres_hits = crate::superres::superres_hits() - before_sr;
            assert!(
                superres_hits >= facts.len(),
                "{NAME} ({depth}-bit): superres_hits advanced by {superres_hits} over {} \
                 frames -- the upscaler never ran",
                facts.len()
            );
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(
                    f.y.len(),
                    128 * 128,
                    "{NAME} ({depth}-bit): frame {i} luma is {} samples -- a superres decode \
                     must hand back the UPSCALED 128x128, not the 85px coded width",
                    f.y.len()
                );
                assert_eq!(
                    f.u.len(),
                    128 * 128,
                    "{NAME} ({depth}-bit): frame {i} chroma is {} samples -- at 4:4:4 the chroma \
                     plane is full resolution at the upscaled size",
                    f.u.len()
                );
            }
            let (frames_decoded, _hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert!(
                frames_decoded >= 4,
                "{NAME} ({depth}-bit): {frames_decoded} decode-order frames"
            );
            eprintln!(
                "{NAME} ({depth}-bit): {frames_decoded} frames byte-exact vs aomdec, \
                 use_superres on every frame, superres_hits {superres_hits}"
            );
        }
    }

    /// lane-av1tilemeasure: 4:4:4 at PARTIAL and ODD coded dimensions,
    /// 8/10/12 bits -- and a correction to what the sweep's "odd dims" cell
    /// was measuring.
    ///
    /// **The sweep's odd-dimension 4:4:4 cells were not odd.** 66x66,
    /// 130x122 and 194x130 are even in BOTH axes; what they are is "not a
    /// multiple of 8", the same class as the 4:2:0 row the sweep measured as
    /// "even, not a multiple of 8". This gate's own assert said so out loud
    /// on its first run: `the 66x66 arm is even in BOTH axes`. The genuinely
    /// odd arms are the two 12-bit ones, and they are producible only at 12
    /// bits: at 8 and 10 bits aomenc rounds a 4:4:4 odd size DOWN to even
    /// (`67x67` -> coded 66x66, `131x131` -> coded 130x130, both measured),
    /// the same rounding the sweep proved for 4:2:0. 12-bit (seq_profile 2)
    /// keeps 67x67 and 65x67 odd, and both decode byte-exact.
    ///
    /// The sweep's 130x122 DIVERGENT verdict is also corrected here: this
    /// lane re-encoded the identical stream (the bytes reproduce the
    /// sweep's sha256 `c87ac65b…`) and it is byte-exact against `aomdec`.
    /// That published verdict came from the prefix compare the same report
    /// documents as its trap 1.
    ///
    /// Recipe per arm: `testsrc2 WxH` yuv444p[10le|12le] 4 frames (12-bit:
    /// the gblur'd `mandelbrot`, H6), `aomenc --codec=av1 --passes=1
    /// --end-usage=q --cq-level=20 --cpu-used=2 --enable-palette=0
    /// --enable-intrabc=0 --threads=1 --row-mt=0 --lag-in-frames=0
    /// --kf-max-dist=100 --limit=4 --obu -o - -` (plus `--input-bit-depth` /
    /// `--bit-depth` above 8).
    ///
    /// NON-VACUITY: `ss (0,0)` at the claimed depth; the arm is not a
    /// multiple of 8 in some axis, and the `odd` flag says whether an axis is
    /// odd, so an arm cannot quietly become the cell next door; EVERY frame
    /// header's coded width and height are exactly the claimed sizes -- a
    ///omenc rounding would make the arm measure a different geometry and fails
    /// here rather than at the pixel compare; the mode-info grid is PARTIAL
    /// (`mi_cols*8 > width` or `mi_rows*8 > height`), the partial-plane walk
    /// the cell exists for; every decoded plane is exactly `width*height` (at
    /// `ss (0,0)` the chroma planes are full resolution, so a 4:2:0 extent or
    /// a truncated decode fails the shape assert); then the
    /// length-asserting `aomdec` compare.
    #[test]
    fn a_real_aomenc_444_partial_and_odd_coded_dimension_streams_decode_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_444_partial_and_odd_coded_dimension_streams_decode_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        // (width, height, bit depth, smooth source, an axis is odd)
        let arms: [(usize, usize, usize, u8, bool); 10] = [
            (66, 66, 8, 0, false),
            (98, 66, 8, 0, false),
            (130, 122, 8, 0, false),
            (66, 66, 10, 0, false),
            (194, 130, 10, 0, false),
            (130, 122, 10, 0, false),
            (66, 66, 12, 1, false),
            (130, 122, 12, 1, false),
            (67, 67, 12, 1, true),
            (65, 67, 12, 1, true),
        ];
        for (w, h, depth, smooth, odd) in arms {
            assert!(
                w % 8 != 0 || h % 8 != 0,
                "{NAME}: the {w}x{h} arm is a multiple of 8 -- it is not a partial-size cell"
            );
            assert_eq!(
                w % 2 == 1 || h % 2 == 1,
                odd,
                "{NAME}: the {w}x{h} arm's `odd` flag is wrong"
            );
            let pix_fmt = match depth {
                8 => "yuv444p",
                10 => "yuv444p10le",
                _ => "yuv444p12le",
            };
            let stream = chroma_format_stream(
                w,
                h,
                pix_fmt,
                depth,
                4,
                &[
                    "--cq-level=20",
                    "--cpu-used=2",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                ],
                smooth,
            );
            assert_444_header(&stream, NAME, depth);
            let facts = frame_facts(&stream, NAME);
            for (i, f) in facts.iter().enumerate() {
                assert_eq!(
                    (f.coded_w as usize, f.coded_h as usize),
                    (w, h),
                    "{NAME} ({w}x{h} {depth}-bit): decode-order frame {i} codes {}x{} -- the arm \
                     is not measuring the geometry it names",
                    f.coded_w,
                    f.coded_h
                );
                assert!(
                    f.mi_cols as usize * 8 > w || f.mi_rows as usize * 8 > h,
                    "{NAME} ({w}x{h} {depth}-bit): frame {i} has a {}x{} mode-info grid, which \
                     covers the frame exactly -- the partial-plane walk never runs",
                    f.mi_cols,
                    f.mi_rows
                );
            }
            let frames = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME} ({w}x{h} {depth}-bit): refused: {e}"));
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(
                    f.y.len(),
                    w * h,
                    "{NAME} ({w}x{h} {depth}-bit): frame {i} luma is {} samples",
                    f.y.len()
                );
                assert_eq!(
                    f.u.len(),
                    w * h,
                    "{NAME} ({w}x{h} {depth}-bit): frame {i} chroma is {} samples -- at 4:4:4 \
                     the chroma plane is full resolution even at an odd coded size",
                    f.u.len()
                );
            }
            let (frames_decoded, _hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert!(
                frames_decoded >= 4,
                "{NAME} ({w}x{h} {depth}-bit): {frames_decoded} decode-order frames"
            );
            eprintln!(
                "{NAME}: {w}x{h} {depth}-bit codes exactly as named (odd axis: {odd}), \
                 {frames_decoded} frames byte-exact vs aomdec"
            );
        }
    }

    /// lane-av1tilemeasure: the H3 CONTROL -- 4:4:4 LOSSLESS 8-bit with NO
    /// tiles -- beside its two-tile-column sibling at the same geometry. The
    /// sweep's H3 named a 90-chroma-sample divergence on a lossless 4:4:4
    /// tile-column stream; the control it never encoded is the untiled stream
    /// at the same size, and without it "tiles cause it" and "4:4:4 lossless
    /// chroma causes it" are the same sentence. Measured here: BOTH arms are
    /// byte-exact against the oracle at 256x128, so at THIS geometry the
    /// defect does not reproduce with or without tiles; the first 4:4:4
    /// lossless fork measured in this lane is at 192x192 (34 chroma samples,
    /// first U(90,25)), where the tiled and untiled streams diverge at the
    /// very same sample -- see `lanes/av1tilemeasure.report.md`.
    ///
    /// Recipe per arm: `testsrc2 256x128` yuv444p 6 frames, `aomenc
    /// --codec=av1 --lossless=1 --passes=1 --end-usage=q --threads=1
    /// --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 --limit=6
    /// --enable-palette=0 --enable-intrabc=0 --obu -o - -`, plus
    /// `--tile-columns=1` on the sibling.
    ///
    /// NON-VACUITY: `ss (0,0)` at 8 bits; every frame header's parsed
    /// `tile_info` is 1x1 for the control and 2x1 for the sibling (so the
    /// control cannot silently become a tiled stream, which is the whole
    /// point of the arm); `decode::rect_split_lossless_chroma444_hits()`
    /// advances, i.e. a lossless strip really walked its chroma as 4:4:4 4x4
    /// units; then the length-asserting `aomdec` compare.
    #[test]
    fn a_lossless_444_8bit_untiled_control_and_its_two_tile_column_sibling_decode_pixel_exact() {
        const NAME: &str = "a_lossless_444_8bit_untiled_control_and_its_two_tile_column_sibling_decode_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let arms: [(&str, &[&str], u32, u32); 2] = [
            (
                "untiled control",
                &["--lossless=1", "--enable-palette=0", "--enable-intrabc=0"],
                1,
                1,
            ),
            (
                "two tile columns",
                &[
                    "--lossless=1",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--tile-columns=1",
                ],
                2,
                1,
            ),
        ];
        for (label, flags, want_cols, want_rows) in arms {
            let stream = chroma_format_stream(256, 128, "yuv444p", 8, 6, flags, 0);
            assert_444_header(&stream, NAME, 8);
            let facts = frame_facts(&stream, NAME);
            for (i, f) in facts.iter().enumerate() {
                assert_eq!(
                    (f.tile_cols, f.tile_rows),
                    (want_cols, want_rows),
                    "{NAME} ({label}): decode-order frame {i} parses tile_info {}x{}, not \
                     {want_cols}x{want_rows}",
                    f.tile_cols,
                    f.tile_rows
                );
            }
            let before_444 = crate::decode::rect_split_lossless_chroma444_hits();
            let (frames_decoded, _hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert!(
                crate::decode::rect_split_lossless_chroma444_hits() > before_444,
                "{NAME} ({label}) went blind: no lossless strip walked its chroma as 4:4:4 4x4 \
                 units, so the cell never reached the 4:4:4 chroma path"
            );
            assert!(
                frames_decoded >= 6,
                "{NAME} ({label}): {frames_decoded} decode-order frames"
            );
            eprintln!(
                "{NAME} ({label}): {frames_decoded} frames byte-exact vs aomdec, tiles \
                 {want_cols}x{want_rows} confirmed on every frame header"
            );
        }
    }

    /// lane-av1loss444kf: the 4:4:4 lossless 256x256 key frame's 49-sample
    /// chroma damage, and the `Reach` a transform unit inside a sub-8x8 RECT
    /// leaf must be given.
    ///
    /// The cell: `testsrc2 256x256 rate=25` yuv444p, 6 frames,
    /// `aomenc --lossless=1 --enable-palette=0 --enable-intrabc=0` -- **109215
    /// bytes, sha256 `4563a01f1778600020ffa9f15bc05f5559321cf5712b99a29ed7192675dee6a4`,
    /// fnv1a64 `0xbbb56442f393a9b2`**. Its key frame decodes with **21 U + 28 V
    /// samples wrong** (U over x 196..207 / y 200..203, V over x 196..206),
    /// luma exact, entropy bit-locked over all 81933 of its symbol reads and
    /// its 12288 transform units at identical absolute `(plane, x, y)`.
    ///
    /// The defect: libaom's `has_bottom_left` (`reconintra.c:381`) is asked
    /// with the **leaf's** `bsize`, the unit's `(row_off, col_off)` and
    /// `tx_size` -- and its first branch inside the left column is
    /// `if (row_off + bottom_left_count_unit < plane_bh_unit) return 1;`, the
    /// block's OWN next row, already reconstructed. A 4x8 leaf's first TX_4X4
    /// unit therefore HAS four bottom-left samples. `sub8_leaf_chroma444`
    /// instead looked a standalone `BLOCK_4X4` up at the unit's own position
    /// (`Reach::of(4, upx, upy, ..)`), which answers false there, so
    /// [`PlaneBuf::edges`] replicated the last left sample out to the edge's
    /// full `bw + bh` reach and the unit predicted D203/CFL from a fabricated
    /// edge. The 4:2:2 twin of the same function was fixed this way already
    /// (lane-av1-422kf2, whose comment names the old lookup); this is its
    /// 4:4:4 twin, still carrying the stale copy.
    ///
    /// Non-vacuity, in order:
    /// 1. the live re-encode's length AND fnv1a64 -- a recipe or encoder drift
    ///    fails here instead of passing as this cell;
    /// 2. [`assert_444_header`] -- at 4:2:0 the arm is unreachable (every
    ///    caller of `sub8_leaf_chroma444` sits behind `chroma_444`) and every
    ///    assert below would pass for free;
    /// 3. [`key_frame_diffs`] == 0 samples, with the first differing sample
    ///    NAMED in the failure so a regression is a location, not a count;
    /// 4. [`crate::decode::sub8_chroma444_lossless_tu_reach_hits`] rose over
    ///    the decode -- the units where the block-relative answer and the
    ///    standalone one disagree, which is what makes this a witness of the
    ///    FIXED path and not of a stream that merely has rect leaves.
    #[test]
    fn a_lossless_444_sub8_rect_leaf_chroma_reach_decodes_the_key_frame_pixel_exact() {
        const NAME: &str = "lossless-444-sub8-rect-leaf-chroma-reach";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let stream = chroma_format_stream(
            256,
            256,
            "yuv444p",
            8,
            6,
            &["--lossless=1", "--enable-palette=0", "--enable-intrabc=0"],
            0,
        );
        assert_eq!(
            (stream.len(), fnv1a64(&stream)),
            (109215, 0xbbb5_6442_f393_a9b2),
            "{NAME}: the pinned 4:4:4 lossless 256x256 cell is {} bytes / fnv1a64 {:#x}, \
             not 109215 / 0xbbb56442f393a9b2 -- the recipe or the encoder drifted, so a \
             result here is not a result about this cell",
            stream.len(),
            fnv1a64(&stream)
        );
        assert_444_header(&stream, NAME, 8);
        crate::decode::reset_sub8_chroma444_tu_reach_hits();
        let (differing, first) = key_frame_diffs(&stream, NAME);
        assert_eq!(
            differing, 0,
            "{NAME}: the 4:4:4 lossless key frame differs from aomdec in {differing} samples \
             (first {first:?} as (plane, x, y, ours, oracle)) -- a sub-8x8 RECT leaf's chroma \
             unit must be given the reach libaom's has_bottom_left computes for the LEAF, not a \
             standalone BLOCK_4X4 lookup at the unit's own position"
        );
        let reach_hits = crate::decode::sub8_chroma444_lossless_tu_reach_hits();
        assert!(
            reach_hits > 0,
            "{NAME}: went blind -- not one 4:4:4 lossless rect-leaf chroma unit had the \
             block-relative reach disagree with the standalone BLOCK_4X4 lookup, so the cell \
             never reached the fixed path and the byte-exactness above proves nothing about it"
        );
        eprintln!(
            "{NAME}: key frame byte-exact vs aomdec (0 of 196608 samples differ), {reach_hits} \
             rect-leaf chroma unit(s) took the block-relative reach"
        );
    }

    /// lane-av1distwtd: the gate that pays the `enable-dist-wtd-comp` entry in
    /// `gate_coverage.rs`'s [`NEVER_EXERCISED_8BIT`], and corrects its stated
    /// reason. The entry said "distance-weighted compound is unimplemented" --
    /// it is NOT: [`crate::compound::dist_wtd_comp_weight_assign`] (lane-av1comp)
    /// and `mc::combine_compound`'s weighted path have been in the tree since.
    /// What was missing is a stream that CARRIES the tool. Proved producible
    /// against aom 3.13.3: the flag maps onto the sequence header's
    /// `enable_jnt_comp` bit (`av1_cx_iface.c:1062` ->
    /// `order_hint_info.enable_dist_wtd_comp` -> `bitstream.c:2681` writes it
    /// under `enable_order_hint`), and this recipe's parsed header prints
    /// `jnt_comp=true` with `=1` and `jnt_comp=false` with `=0` -- the parsed
    /// value is asserted, never the flag spelling, per the class
    /// `knob-never-reached-the-tool` trap that mislabelled a cell once already.
    ///
    /// Three hard asserts, all load-bearing:
    /// 1. the PARSED sequence header carries `enable_jnt_comp == true` (the
    ///    off-arm asserts `false` from the same recipe, so the bit is proven to
    ///    MOVE with the flag rather than to be set by something else);
    /// 2. `dist_wtd_comp_hits() > 0` -- the `compound_idx == 0` arm, the only
    ///    branch whose blend weights are not the constant (8, 8). A stream
    ///    carrying the header bit while coding every compound block with
    ///    `compound_idx == 1` leaves this at 0 and decodes byte-identically
    ///    without the whole distance-weight path, so the counter -- not the
    ///    header bit -- is what makes this a witness of the TOOL;
    /// 3. every frame pixel-compared against the oracle through
    ///    [`decode_all_frames_vs_oracle`], which length-asserts each
    ///    decode-order frame before zipping.
    ///
    /// `--min-partition-size=32` is what makes the arm fire: at 16 the encoder
    /// emits 8x8 inter leaves this decoder refuses BY NAME, so a smaller
    /// min-partition refuses before the `compound_idx` read ever runs (measured:
    /// the `min=16` arm of the same recipe refuses with "an inter partition
    /// below 16x16" and dumps zero `compound_idx` lines). `--cq-level=30` is
    /// likewise load-bearing: at cq 45/55 the same arm encodes a single-ref
    /// stream (zero `compound_idx` reads) and the counter assert would be
    /// vacuous.
    #[test]
    fn a_distance_weighted_compound_stream_decodes_pixel_exact() {
        const NAME: &str = "distwtd-comp";
        let _gate_lock = lock_gate_counters();
        if !have_aomenc() {
            return;
        }
        assert!(have_ffmpeg(), "{NAME}: ffmpeg required");
        assert!(
            aomdec_path().is_file(),
            "{NAME}: aomenc resolved to {} but no aomdec sits beside it -- a \
             pixel-exactness gate that compares nothing is worse than no gate",
            aomenc_path().display()
        );
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        // mandelbrot + a slow rotation: two genuinely different references at
        // different temporal distances, which is what `dist_wtd_comp_weight_
        // assign` weights BY. A static or flat source has equal distances
        // everywhere and every block collapses onto one weight row.
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "mandelbrot=size=128x128:rate=25:start_x=-0.6:start_y=-0.4,\
                 rotate=a=0.01*n:ow=128:oh=128:c=black,scale=64:64",
                "-t",
                "0.96",
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
        // the shared recipe; the flag under test is appended per arm so the
        // two arms differ in exactly one argument.
        let recipe: Vec<&str> = vec![
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=30",
            "--cpu-used=0",
            "--kf-max-dist=1000",
            "--threads=1",
            "--row-mt=0",
            "--auto-alt-ref=1",
            "--lag-in-frames=25",
            "--enable-fwd-kf=0",
            // `enable_jnt_comp` is only READ when order hints are on
            // (`av1_cx_iface.c:1491` ANDs the two).
            "--enable-order-hint=1",
            "--enable-warped-motion=1",
            "--tune-content=default",
            "--min-partition-size=32",
            "--max-partition-size=64",
            // partition/intra tools this decoder does not code -- the same
            // exclusion list every compound gate in this file carries.
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
            "--enable-palette=0",
            "--enable-intrabc=0",
            "--enable-cfl-intra=0",
            "--enable-ref-frame-mvs=0",
        ];
        let encode = |flag: &str| -> Vec<u8> {
            let out = run_with_stdin(
                Command::new(aomenc_path()).args(
                    recipe
                        .iter()
                        .copied()
                        .chain([flag, "--obu", "-o", "-", "-"]),
                ),
                &y4m.stdout,
            );
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused the fixture with {flag}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        // The PARSED header, read back off the encoded bytes -- never the
        // command line. `Av1Parser` walks the real OBUs, so a flag the encoder
        // ignored is caught here rather than inherited into coverage.
        let parsed_jnt_comp = |stream: &[u8]| -> bool {
            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            while pos < stream.len() {
                let obu = parser
                    .parse_obu(&stream[pos..])
                    .expect("parsing our own OBUs");
                pos += obu.total_size;
            }
            parser
                .sequence_header()
                .expect("the stream carries a sequence header")
                .enable_jnt_comp
        };
        // NB: the OFF arm is spelled FIRST on purpose. `gate_coverage.rs`'s
        // `flags_in` keeps the LAST `--enable-<tool>=<value>` it finds in a gate
        // body, so a trailing `=0` would make the whole gate read as "tool
        // disabled" to the census and leave the entry unretired even though
        // this gate does exercise the tool. Both arms are still asserted below.
        let off = encode("--enable-dist-wtd-comp=0");
        let on = encode("--enable-dist-wtd-comp=1");
        // the bit MOVES with the flag, and it moves the BYTES: a flag the
        // encoder accepted but ignored would fail both of these.
        assert!(
            parsed_jnt_comp(&on),
            "{NAME}: the recipe spelled --enable-dist-wtd-comp=1 but the PARSED \
             sequence header carries enable_jnt_comp == false (class \
             knob-never-reached-the-tool) -- the gate would be vacuous"
        );
        assert!(
            !parsed_jnt_comp(&off),
            "{NAME}: the same recipe with --enable-dist-wtd-comp=0 still parses \
             enable_jnt_comp == true, so the ON arm proves nothing about the flag"
        );
        assert_ne!(
            on.len(),
            off.len(),
            "{NAME}: both arms encoded {} bytes -- the flag changed nothing on \
             the wire, so neither arm is evidence for anything",
            on.len()
        );
        // shape non-vacuity, before any compare: at 4:2:0 a decoder keeping a
        // subsampled extent would hand quarter-size planes to the byte compare.
        let frames = decode_stream(&on).unwrap_or_else(|e| panic!("{NAME}: refused: {e}"));
        assert_eq!(
            frames.len(),
            frame_count,
            "{NAME}: expected {frame_count} shown frames"
        );
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(
                (f.width, f.height),
                (width, height),
                "{NAME}: frame {i} is {}x{}",
                f.width,
                f.height
            );
            assert_eq!(
                f.u.len(),
                width * height / 4,
                "{NAME}: frame {i} U plane is {} samples, not 4:2:0",
                f.u.len()
            );
            assert_eq!(f.v.len(), f.u.len(), "{NAME}: frame {i} V plane extent");
            assert_eq!(f.y.len(), width * height, "{NAME}: frame {i} luma extent");
        }
        crate::decode::reset_dist_wtd_comp_hits();
        let hits_before = crate::decode::dist_wtd_comp_hits();
        let (oracle_frames, hidden) = decode_all_frames_vs_oracle(&on, NAME);
        let dist_wtd = crate::decode::dist_wtd_comp_hits() - hits_before;
        eprintln!(
            "{NAME}: {oracle_frames} oracle frames ({hidden} hidden), \
 dist_wtd_comp_hits={dist_wtd}, on={} B off={} B",
            on.len(),
            off.len()
        );
        assert!(
            dist_wtd > 0,
            "{NAME}: no block took the distance-weighted compound combine \
             (compound_idx == 0) across {oracle_frames} frames that carry \
             enable_jnt_comp = true -- every compound block used the simple \
             average, so this gate proves nothing about the tool"
        );
    }

    /// lane-whtshape: a LOSSLESS 4:4:4 frame whose rect intra-BC blocks used
    /// to code their chroma plane block as ONE rect transform unit -- a shape
    /// libaom cannot code. `read_tx_mode` (`av1/decoder/decodeframe.c:140`)
    /// returns `ONLY_4X4` for a `coded_lossless` frame BEFORE it looks at the
    /// `tx_mode` symbol, `read_tx_size` (`:1117`) returns `TX_4X4` before it
    /// looks at a tree, `get_vartx_max_txsize` (`av1/common/blockd.h:1447`)
    /// returns `TX_4X4` for EVERY plane of a lossless block, and of
    /// `av1/common/idct.c`'s `highbd_inv_txfm_add_*_c` only the 4x4 one carries
    /// a `lossless` branch -- the rest fall through to the IDCT, because there
    /// is no non-4x4 lossless inverse transform to dispatch to.
    ///
    /// The 8x4 unit that resulted reached the lossless Walsh-Hadamard path,
    /// where a `debug_assert_eq!` named it in DEBUG and the RELEASE build died
    /// one frame later with an opaque `range end index 8 out of range for slice
    /// of length 0` (measured, not assumed -- see
    /// lanes/whtshape.report.md r1).
    ///
    /// The witness is `fixtures/ll444_sb64_1to4_lossless.obu` (51107 bytes,
    /// sha256 `fdb6467987095b99c8eb51b48743691d906b2d99d069f71f1d51b1abb6d2056e`),
    /// encoded with the instrumented oracle
    /// (`scripts/build-aom-oracle.sh`'s `aomenc`, 4:4:4 10-bit, `--lossless=1
    /// --profile=1 --i444 --enable-rect-partitions=1
    /// --enable-1to4-partitions=1 --min-partition-size=4 --max-partition-size=64
    /// --sb-size=64 --tune-content=screen --enable-intrabc=1 --enable-palette=1
    /// --cq-level=60 --enable-tx-size-search=0 --cpu-used=0 --limit=2`) over
    /// `ffmpeg -f lavfi -i testsrc2=size=256x256:rate=25:duration=1 -pix_fmt
    /// yuv444p10le -f rawvideo`. 4:2:0 cannot produce the shape at all -- its
    /// smallest lossless chroma plane block is already 4x4 -- which is why the
    /// 4:4:4 10-bit header is asserted.
    ///
    /// Four assertions, each with its own failure:
    ///
    /// 1. the route fired: `INTRABC_RECT_LOSSLESS_CHROMA4_HITS` is EXCLUSIVE to
    ///    the per-4x4 chroma walk (no other caller walks a rect intra-BC
    ///    block's chroma), so `>= 1` cannot be satisfied by the old
    ///    single-unit read;
    /// 2. NO non-4x4 lossless transform unit was reconstructed
    ///    (`LOSSLESS_WHT_SPLIT_UNITS` delta 0) -- the shape-total WHT entry is
    ///    not quietly papering over a caller that is still wrong;
    /// 3. the stream DECODES both frames (before the fix the same bytes were a
    ///    RELEASE panic and a Golomb-tail refusal in DEBUG); and
    /// 4. a PIXEL comparison against the instrumented oracle's own
    ///    `EC_AV1_FINAL_DUMP`, in DECODE order.
    ///
    /// **(4) is a PREFIX floor, not an exactness claim, and it says so.**
    /// Measured against the oracle, decode-order frame 0 (the key frame) is
    /// byte-exact on all three planes for its first 32992 of 65536 samples --
    /// the whole frame above row 128, plus row 128 out to column 224. The
    /// first differing sample there is (x=224, y=128) on Y, U and V alike: the
    /// first column PAST the 16x8 lossless intra-BC block at mi (32, 52),
    /// whose own 16x8 luma and chroma are byte-exact -- so the fork is at the
    /// block boundary, in the block that FOLLOWS it. Decode-order frame 1 (the
    /// inter frame) is exact to 55008 on luma and to 9378 on chroma. Both forks
    /// are a SEPARATE defect this lane does not own; each is named with its
    /// measurement in lanes/whtshape.report.md r3. Asserting "our output is
    /// wrong by exactly N" would encode those defects as expected behaviour;
    /// the per-frame, per-plane floor is the honest form, and it only grows.
    #[test]
    fn a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units() {
        const NAME: &str = "a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units";
        const FIXTURE: &str = "ll444_sb64_1to4_lossless.obu";
        const FIXTURE_LEN: usize = 51107;
        const FIXTURE_FNV: u64 = 0x7664_6a62_ddd3_4df2;
        const FRAMES: usize = 2;
        const PLANE: usize = 256 * 256;
        /// Per decode-order frame, per plane (0=Y, 1=U, 2=V), the number of
        /// leading samples MEASURED byte-exact against the oracle's
        /// `EC_AV1_FINAL_DUMP`; the first differing sample is exactly this
        /// index on that plane. Key frame: the whole frame above row 128 plus
        /// row 128 out to column 224. Inter frame: luma exact to (224, 214),
        /// chroma diverging early at (162, 36).
        const EXACT_PREFIX: [[usize; 3]; 2] = [[32992, 32992, 32992], [55008, 9378, 9378]];
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("fixtures/{FIXTURE}"));
        let stream = read_pin(&obu, FIXTURE_LEN, FIXTURE_FNV, NAME);
        assert_444_header(&stream, NAME, 10);

        let _guard = lock_gate_counters();
        crate::decode::reset_intrabc_rect_lossless_chroma4_hits();
        let split_before = crate::decode::lossless_wht_split_units();
        let wht_before = crate::decode::lossless_wht_units();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned witness was refused: {e}"));
        let chroma4 = crate::decode::intrabc_rect_lossless_chroma4_hits();
        let split = crate::decode::lossless_wht_split_units() - split_before;
        let wht = crate::decode::lossless_wht_units() - wht_before;

        assert!(
            chroma4 > 0,
            "{NAME}: no LOSSLESS rect intra-BC block walked its chroma as 4x4 units \
             -- the corrected route never ran, so this gate is measuring the old \
             single rect unit (class gate-blind-to-feature)"
        );
        assert_eq!(
            split, 0,
            "{NAME}: {split} 4x4 WHT unit(s) were reconstructed inside a LOSSLESS \
             transform unit that was itself not 4x4 -- a caller is still sizing a \
             lossless unit wrongly and the shape-total WHT entry is hiding it"
        );
        assert!(
            wht > 0,
            "{NAME}: no lossless WHT unit ran at all -- vacuous"
        );
        assert_eq!(frames.len(), FRAMES, "{NAME}: frame count");

        // The probe decides; its own SKIP (and its assert under
        // EC_AV1_REQUIRE_AOMDEC / EC_AV1_REQUIRE_AOMENC) has already been said.
        if !aomdec_available(NAME) {
            eprintln!(
                "{NAME}: {chroma4} lossless rect intra-BC block(s) walked 4x4 chroma units, \
                 {wht} WHT units, 0 split"
            );
            return;
        }
        // The oracle's own final-frame dumps, in DECODE order, against ours.
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let in_obu = dir.join("in.obu");
        std::fs::write(&in_obu, &stream).expect("writing the stream");
        let aom_prefix = dir.join("aom");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "-o"])
            .arg(dir.join("out.y4m"))
            .arg(&in_obu)
            .env("EC_AV1_FINAL_DUMP", &aom_prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{NAME}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ours_prefix = dir.join("ours");
        set_final_dump_prefix(Some(ours_prefix.display().to_string()));
        let shown = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the second decode was refused: {e}"))
            .len();
        set_final_dump_prefix(None);
        let count = |prefix: &std::path::Path| -> usize {
            let mut n = 0usize;
            while std::path::PathBuf::from(format!("{}.f{n}", prefix.display())).is_file() {
                n += 1;
            }
            n
        };
        let oracle_frames = count(&aom_prefix);
        let our_frames = count(&ours_prefix);
        assert_eq!(
            our_frames, oracle_frames,
            "{NAME}: decoded {our_frames} frames in decode order, the oracle decoded \
             {oracle_frames}"
        );
        for i in 0..oracle_frames {
            let want =
                std::fs::read(format!("{}.f{i}", aom_prefix.display())).expect("oracle dump");
            let got = std::fs::read(format!("{}.f{i}", ours_prefix.display())).expect("our dump");
            assert_eq!(
                got.len(),
                want.len(),
                "{NAME}: decode-order frame {i} is {} bytes, the oracle's is {}",
                got.len(),
                want.len()
            );
            assert_eq!(
                got.len(),
                3 * PLANE * 2,
                "{NAME}: decode-order frame {i} is not three 256x256 16-bit planes"
            );
            for (plane, name) in [(0usize, "Y"), (1, "U"), (2, "V")] {
                let at = plane * PLANE * 2;
                let floor = EXACT_PREFIX[i.min(EXACT_PREFIX.len() - 1)][plane].min(PLANE);
                if got[at..at + floor * 2] == want[at..at + floor * 2] {
                    continue;
                }
                let first = (0..floor)
                    .find(|&s| got[at + s * 2..at + s * 2 + 2] != want[at + s * 2..at + s * 2 + 2])
                    .unwrap_or(floor);
                panic!(
                    "{NAME}: decode-order frame {i} plane {name} diverges from the oracle at \
                     sample {first} of {PLANE} (x={}, y={}) -- the exact prefix this gate pins \
                     is {floor} and it only grows; the rest is the OPEN defect named in \
                     lanes/whtshape.report.md r3",
                    first % 256,
                    first / 256
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!(
            "{NAME}: {chroma4} lossless rect intra-BC block(s) walked 4x4 chroma units, \
             {wht} WHT units, 0 split; {oracle_frames} decode-order frame(s) ({shown} shown) \
             byte-exact against the oracle up to the per-frame, per-plane exact prefix \
             -- the tail is the OPEN r3 defect, not an assertion"
        );
    }

    /// lane-av1444edge: the two 4:4:4 lossy cells the chroma-format sweep
    /// (§6.2) left as "unassigned, neither reduces to a known class". They
    /// are one class, and it is the sibling gate's: a 1:4 (HORZ_4 / VERT_4)
    /// inter strip's chroma coefficient context gathered at the 4:2:0 PAIR
    /// extent at ss (0,0), where `is_chroma_reference`
    /// (av1_common_int.h:1454) makes every block its own chroma reference so
    /// no pair exists. What these two add is the geometry and the depth the
    /// sibling's 128x96 gate does not reach.
    ///
    /// **Arm A -- partial-frame right edge, 130x122.** Neither dimension is a
    /// multiple of 8 (the sweep's own odd-dimension gate covers 4:2:0 only, so
    /// the 4:4:4 partial-column walk had no exactness evidence), and 66x66 is
    /// EXACT at the same settings, so the walk is right at one odd geometry
    /// and wrong at another. Arm B -- 256x128, 4 frames, with CDEF and loop
    /// restoration both live on the last two frames.
    ///
    /// **The recipes** (the sweep's, byte-reproduced -- the truncated sha256
    /// prefixes it recorded, `c87ac65b…` and `06621606…`, match):
    /// ```text
    /// ffmpeg -f lavfi -i "testsrc2=size=<W>x<H>:rate=25" -frames:v 4 \
    ///        -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe - | \
    /// aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 \
    ///        --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
    ///        --limit=4 --obu -o - -
    /// ```
    /// (`rate=25` is load-bearing for the same reason as the sibling gate:
    /// `testsrc2`'s pattern is time-parameterised.)
    ///
    /// Red before the fix, both arms: arm A frame 3, 11521 samples, first at
    /// Y(128,16) (ours 134, oracle 170); arm B frame 2, 25296 samples, first
    /// at Y(192,0) (ours 92, oracle 106), frame 3 51108 more. The
    /// pre-loop-filter reconstruction already carries the identical first
    /// wrong sample on both, so no filter is involved; in arm B the first
    /// wrong superblock in DECODE order is SBcol2/SBrow0 (first wrong pixel
    /// (128,48) = mi(12,32), whose mode/ref/mv are identical on both sides),
    /// and the arithmetic fork lands on the very next block, mi(13,32), where
    /// the oracle reads ref0 = LAST and we read GOLDEN.
    ///
    /// Non-vacuity, three ways per arm: the pinned bytes (length + FNV-1a);
    /// the decoded chroma planes are full resolution, which is the ss (0,0)
    /// shape claim; and `decode::rect4_inter_own_chroma444_hits()` must be
    /// non-zero AND `inter16_rect4_counters()` must report 1:4 inter strips,
    /// so the corrected route is known to have run rather than the gate
    /// passing on a stream that never reached it.
    #[test]
    fn a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries() {
        const NAME: &str =
            "a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries";
        // (fixture, len, fnv1a64, width, height, strips HORZ_4 + VERT_4)
        const ARMS: [(&str, usize, u64, usize, usize, usize); 2] = [
            (
                "fixtures/444_lossy_rect4_odd_130x122.obu",
                8945,
                0x5125_0554_751c_bf12,
                130,
                122,
                28, // measured 8 HORZ_4 + 20 VERT_4
            ),
            (
                "fixtures/444_lossy_rect4_wide_256x128.obu",
                15239,
                0xf94a_0218_bb34_4f9e,
                256,
                128,
                76, // measured 60 HORZ_4 + 16 VERT_4
            ),
        ];
        let _guard = lock_gate_counters();
        for (fixture, len, fnv, w, h, min_strips) in ARMS {
            let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(fixture);
            let stream = std::fs::read(&obu).unwrap_or_else(|e| {
                panic!(
                    "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot \
                     run, which is a failure, not a skip",
                    obu.display()
                )
            });
            assert_eq!(stream.len(), len, "{NAME}: {fixture} length moved");
            assert_eq!(fnv1a64(&stream), fnv, "{NAME}: {fixture} bytes moved");

            // The 1:4-strip counters are process-wide with no reset, so read
            // them as a DELTA: a full-suite run has already put 1:4 strips
            // through the decoder by the time this test starts, and an
            // absolute count would be a non-vacuity assertion that passes on
            // residue from another stream.
            let before_strips = {
                let (h, v, _, _) = crate::decode::inter16_rect4_counters();
                h + v
            };
            crate::decode::reset_rect4_inter_own_chroma444_hits();
            let frames = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME}: {fixture} no longer decodes cleanly: {e}"));
            let own_chroma = crate::decode::rect4_inter_own_chroma444_hits();
            let strips = {
                let (h, v, _, _) = crate::decode::inter16_rect4_counters();
                h + v - before_strips
            };
            assert!(
                strips >= min_strips,
                "{NAME}: {fixture} coded only {strips} 1:4 inter strips \
                 (HORZ_4 + VERT_4), expected about {min_strips} -- the stream \
                 moved, and the route this gate witnesses may no longer be on it"
            );
            assert!(
                own_chroma > 0,
                "{NAME}: {fixture} ran no 1:4 inter strip over its own chroma \
                 extent -- the corrected route never ran (class \
                 gate-blind-to-feature)"
            );
            assert_eq!(frames.len(), 4, "{NAME}: {fixture} frame count");
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(
                    (f.width, f.height),
                    (w, h),
                    "{NAME}: {fixture} frame {i} dimensions"
                );
                // At 4:4:4 the chroma planes are full resolution; a decoder
                // that kept the 4:2:0 extent would hand quarter-size chroma
                // planes to the byte compare, which cannot pass. Say it here
                // rather than let the compare stand in for the shape claim.
                assert_eq!(
                    f.u.len(),
                    w * h,
                    "{NAME}: {fixture} frame {i} chroma plane is {} samples; at \
                     4:4:4 it must be full resolution ({w}x{h})",
                    f.u.len()
                );
            }
            if aomdec_available(NAME) {
                let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
                eprintln!(
                    "{NAME}: {fixture} {decoded} decode-order frame(s) \
                     ({hidden} hidden) byte-exact vs aomdec, {own_chroma} own-extent \
                     1:4 chroma gather(s), {strips} 1:4 inter strip(s)"
                );
            }
        }
    }

    /// lane-av1444d10: the 10-bit 4:4:4 352x242 cell the 4:2:2 census
    /// (`lanes/av1422census.report.md` §4 class D) found diverging on chroma
    /// only -- 0 / 1166 / 1413 over 17 of 17 decode frames, first bad sample
    /// at decode frame 0 plane U row 224 col 64 -- and the fix for it, a byte
    /// exact gate. 4:4:4 is a format this decoder claims to support, so the
    /// cell is not probe-only evidence.
    ///
    /// **Why this cell and not its five same-depth controls.** `s444_*_10b`
    /// at 320x242, 320x250, 352x250, 416x242 and 416x250, same encoder recipe,
    /// same 10-bit depth, were all byte-exact before the fix and still are; only
    /// 352x242 diverged. What 352x242 has is the first block of the cell whose
    /// chroma is TALLER than the 32-pixel unit cap at ss (0,0): a 32x64 luma
    /// strip keeps its 32x64 chroma, and `decode_block_rect`'s tiled chroma
    /// walk splits it into `nh = 2` units of 32x32. Every other cell's chroma
    /// blocks are <= 32 on both axes, so `nw * nh == 1` and the whole-block
    /// buffer IS the unit's window -- the bug is invisible there. That is why
    /// the census could report "geometry-sensitive" without the mechanism:
    /// the sensitive axis is chroma UNIT COUNT, not frame width or height.
    ///
    /// **The defect.** Both tiled arms handed the unit the WHOLE block's
    /// palette prediction buffer, and `PlaneBuf::reconstruct` reads its first
    /// `side * side` entries, so unit (0,1) at pixel (64, 224) reconstructed
    /// the block's row-192 palette values (flat 209) where libaom writes
    /// row 224's (216). The neighbouring 32x32 `PAETH_PRED` block at
    /// (96, 224), whose left edge sample IS that pixel, then propagated 19
    /// more wrong columns along its own top row -- which is why the census saw
    /// 53 wrong columns rather than 32.
    ///
    /// **Red before the fix, by this gate's own name:** with the two
    /// `palette_window` calls reverted the first wrong sample is decode frame
    /// 0 plane U row 224 col 64 (ours 209, oracle 216) and the totals are
    /// 0 / 1166 / 1413 over 17 decode frames.
    ///
    /// **Non-vacuity, four ways.** (1) The pinned bytes: length 24875 and
    /// FNV-1a64 `0x7fda_dbb4_3bbf_9af3`, sha256
    /// `84df2f37cc7982f9a572443d85dfcc13bf366a64f3fb567391dcd5acd58a0ad2`.
    /// (2) 17 decode-order frames at 352x242 with FULL-RESOLUTION chroma, so
    /// a decoder that kept the 4:2:0 extent cannot pass the compare.
    /// (3) `decode::chroma_palette_window_hits()` read as a delta must be at
    /// least 1. After the refutation pass read the numbers back, that counter
    /// is guarded on `nw * nh > 1`, so it fires ONLY for units of a block the
    /// 32-cap actually tiled -- a hit is therefore proof that the tiled arm ran,
    /// not merely that some palette chroma block existed, and a gate that
    /// passed with the tiled arm removed would be gate-blind-to-feature. (4) The oracle comparator this gate leans on has
    /// its own per-plane one-byte-flip liveness control in the crate
    /// (`every_oracle_comparator_reds_on_a_one_byte_wrong_oracle`).
    #[test]
    fn a_444_lossy_palette_chroma_unit_window_is_byte_exact_at_352x242_10bit() {
        const NAME: &str = "a_444_lossy_palette_chroma_unit_window_is_byte_exact_at_352x242_10bit";
        const FIXTURE: &str = "fixtures/444_lossy_palette_chroma_352x242_10b.obu";
        const LEN: usize = 24875;
        const FNV: u64 = 0x7fda_dbb4_3bbf_9af3;
        const W: usize = 352;
        const H: usize = 242;
        // 16 SHOWN frames (`--limit=16`); the oracle compare below is the one
        // that walks all 17 DECODE-order pictures (16 shown + 1 hidden altref).
        const FRAMES: usize = 16;
        // measured 4 on this stream (two tiled palette chroma blocks x U and V)
        const MIN_WINDOWED_UNITS: usize = 4;
        let _guard = lock_gate_counters();
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), LEN, "{NAME}: {FIXTURE} length moved");
        assert_eq!(fnv1a64(&stream), FNV, "{NAME}: {FIXTURE} bytes moved");

        // Process-wide with no reset: read as a DELTA, or a full-suite run
        // leaves residue that would make the reachability assert vacuous.
        crate::decode::reset_chroma_palette_window_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: {FIXTURE} no longer decodes cleanly: {e}"));
        let windowed = crate::decode::chroma_palette_window_hits();
        assert!(
            windowed >= MIN_WINDOWED_UNITS,
            "{NAME}: {FIXTURE} ran {windowed} windowed palette chroma unit(s), \
             expected at least {MIN_WINDOWED_UNITS} -- the tiled chroma walk did \
             not reach a palette block, so the fixed lines never ran and this \
             gate is blind to the defect (class gate-blind-to-feature)"
        );
        assert_eq!(
            frames.len(),
            FRAMES,
            "{NAME}: {FIXTURE} decode-order frame count"
        );
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(
                (f.width, f.height),
                (W, H),
                "{NAME}: {FIXTURE} frame {i} dimensions"
            );
            assert_eq!(
                f.u.len(),
                W * H,
                "{NAME}: {FIXTURE} frame {i} chroma plane is {} samples; at 4:4:4 \
                 it must be full resolution ({W}x{H})",
                f.u.len()
            );
        }
        if aomdec_available(NAME) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            eprintln!(
                "{NAME}: {FIXTURE} {decoded} decode-order frame(s) ({hidden} hidden) \
                 byte-exact vs aomdec, {windowed} windowed palette chroma unit(s)"
            );
        }
    }

    /// lane-av1444palw: the gate for the arm
    /// `a_444_lossy_palette_chroma_unit_window_is_byte_exact_at_352x242_10bit`
    /// could NOT reach -- the HORIZONTAL half of the same 32-capped chroma
    /// walk in `decode_block_rect64` (`palette_window(pbuf, chroma_w,
    /// cu_col * uw, cu_row * uh, uw, uh)`, the arms at decode.rs:20131 and
    /// :20199). Its `cu_col` term is the axis the corpus census never
    /// exercised: a 106-fixture trace of that walk found 16 windowed units and
    /// every one had `nw == 1`, i.e. only the `cu_row` term (`nh = 2`) was
    /// ever load-bearing.
    ///
    /// **What makes `nw >= 2` reachable.** At ss (0,0) the chroma plane block
    /// equals the luma one, so a 64-WIDE luma rect block keeps `cw = 64` and
    /// the 32-cap tiles it into `nw = 2` units. `decode_block_rect64` is the
    /// only function owning this walk at a 64-wide block, reached from
    /// `PARTITION_HORZ`/`HORZ_A`/`HORZ_B` (64x32) and `HORZ_4` (64x16). The
    /// cells here came from sweeping `--min-partition-size` x `--cq-level` over
    /// a 4:4:4 testsrc2 640x480 cell encoded with the lane-av1rect14
    /// `ibc640` flag set (rect + 1:4 partitions, `--min-partition-size=8
    /// --sb-size=64 --tune-content=screen --enable-palette=1
    /// --enable-intrabc=1 --enable-tx-size-search=0 --cpu-used=0
    /// --lag-in-frames=0 --kf-max-dist=1 --limit=1 --threads=1
    /// --tile-columns=0 --passes=1 --end-usage=q --profile=1`):
    /// `--min-partition-size=8` is what yields the 64-wide strip, and
    /// `cq-level=45` is what makes the chroma palette win on it. Census read
    /// off `EC_PALWALK=1`, the diagnostic rung this lane added to
    /// `decode_block_rect64`:
    ///
    /// * 10-bit `cq 45`: `bw=64 bh=16 cw=64 ch=16 uw=32 uh=16 nw=2 nh=1 puv=1`
    ///   -- the RECT-unit arm (`uw != uh`) carrying a chroma palette, `nw = 2`.
    /// * 12-bit `cq 45`: `bw=64 bh=32 cw=64 ch=32 uw=32 uh=32 nw=2 nh=1 puv=1`
    ///   -- the SQUARE-unit arm carrying a chroma palette, `nw = 2`, on the
    ///   12-bit chroma plane no prior gate had built.
    ///
    /// **Both cells are byte-exact against the instrumented `aomdec`** per
    /// plane over the single decode-order frame (10-bit 0/0/0, 12-bit 0/0/0),
    /// and the comparator's liveness is proved by a one-byte oracle tamper
    /// moving the mismatch count 0 -> 1 at each depth.
    ///
    /// **Red without the per-unit window, by this gate's own numbers.**
    /// Reverting both `palette_window` calls to the whole-block buffer (the
    /// counter left armed) gives 0 / 25553 / 26139 on the 10-bit cell, first
    /// wrong sample decode frame 0 plane U row 96 col 448 (mutant 810, oracle
    /// 763; plane V the same sample 891 vs 767), and 0 / 6400 / 6400 on the
    /// 12-bit cell, first wrong sample plane U row 352 col 160 (866 vs 2004).
    /// Both are the `cu_col = 1` unit reading the block's column 0 -- this arm,
    /// not a cascade.
    ///
    /// **Non-vacuity.** (1) pinned bytes: 3301 / 2571 with FNV-1a64 and
    /// sha256 asserted. (2) the DERIVED condition: the counter is guarded on
    /// `nw * nh > 1`, so a hit proves the tiled arm ran, and the floors below
    /// are re-measured on these two streams (the pinned gate's floor of 4 was
    /// a floor from ONE stream, not a census). (3) 640x480 with
    /// FULL-RESOLUTION chroma, so a 4:2:0 extent cannot pass. (4) the crate's
    /// own one-byte oracle-comparator liveness control.
    #[test]
    fn a_444_palette_chroma_horizontal_unit_window_is_byte_exact_10bit_and_12bit() {
        const NAME: &str =
            "a_444_palette_chroma_horizontal_unit_window_is_byte_exact_10bit_and_12bit";
        // (fixture, len, fnv1a64, bit_depth, min_windowed_units)
        const ARMS: [(&str, usize, u64, usize, usize); 2] = [
            (
                "fixtures/444_palette_chroma_hstrip_640x480_10b.obu",
                3301,
                0x6f94_da75_95cd_9a17,
                10,
                8,
            ),
            (
                "fixtures/444_palette_chroma_hstrip_640x480_12b.obu",
                2571,
                0x65a5_8561_c1f3_538e,
                12,
                4,
            ),
        ];
        const W: usize = 640;
        const H: usize = 480;
        let _guard = lock_gate_counters();
        for (fixture, len, fnv, depth, min_units) in ARMS {
            let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(fixture);
            let stream = std::fs::read(&obu).unwrap_or_else(|e| {
                panic!(
                    "{NAME}: pinned fixture {fixture} is missing ({e}) -- the gate cannot \
                     run, which is a failure, not a skip"
                )
            });
            assert_eq!(stream.len(), len, "{NAME}: {fixture} length moved");
            assert_eq!(fnv1a64(&stream), fnv, "{NAME}: {fixture} bytes moved");

            crate::decode::reset_chroma_palette_window_hits();
            let frames = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME}: {fixture} no longer decodes cleanly: {e}"));
            let windowed = crate::decode::chroma_palette_window_hits();
            assert!(
                windowed >= min_units,
                "{NAME}: {fixture} ran {windowed} windowed palette chroma unit(s), expected \
                 at least {min_units} -- the 32-cap tiled chroma walk did not reach enough \
                 palette blocks, so the horizontal windowing under test never ran and this \
                 gate is blind to the defect (class gate-blind-to-feature)"
            );
            assert_eq!(
                frames.len(),
                1,
                "{NAME}: {fixture} decode-order frame count"
            );
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(
                    (f.width, f.height),
                    (W, H),
                    "{NAME}: {fixture} frame {i} dimensions"
                );
                assert_eq!(
                    f.u.len(),
                    W * H,
                    "{NAME}: {fixture} frame {i} chroma plane is {} samples; at 4:4:4 it must \
                     be full resolution ({W}x{H}) -- a {depth}-bit decoder that kept the \
                     4:2:0 extent cannot pass this compare",
                    f.u.len()
                );
            }
            if aomdec_available(NAME) {
                let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
                eprintln!(
                    "{NAME}: {fixture} ({depth}-bit) {decoded} decode-order frame(s) \
                     ({hidden} hidden) byte-exact vs aomdec, {windowed} windowed palette \
                     chroma unit(s)"
                );
            }
        }
    }

    /// lane-av1cfl: the 4:4:4 CfL AC body is a DIFFERENT function from the
    /// 4:2:0 one, and this gate is the proof that the format-blind
    /// `cfl_ac_q3_at` (`bw / 2, bh / 2`, a 2x2 luma average per chroma
    /// sample) is reached only at 4:2:0.
    ///
    /// **The claim this closes.** `cfl_ac_q3_at` has no `fctx`/`ss`
    /// parameter, so at 4:4:4 it would be wrong twice over: the chroma
    /// extent equals the luma extent when `ss_x == ss_y == 0`, and libaom
    /// averages 2x2 luma per chroma sample at 4:2:0 but 1x1 at 4:4:4
    /// (`cfl_luma_subsampling_420_lbd_c` vs `cfl_luma_subsampling_444_lbd_c`,
    /// `~/.cache/aom-oracle/src/av1/common/cfl.c:230` and `:257`). The
    /// inherited report claimed "188 rung hits on the 4:4:4 set"
    /// (`lanes/av1chromahalvings.report.md` r3.4). **It does not
    /// reproduce**: the env-gated rung `EC_HALVSWEEP` in `cfl_ac_q3_at`,
    /// swept over all 27 `*444*` pins, printed **0 hits on every one**,
    /// while the 4:4:4 arm of `cfl_ac_ss` fired 4..312 times on the same
    /// fixtures. The 4:4:4 CfL path is real and heavily exercised; it just
    /// does not go through this function. `cfl_ac_ss` dispatches 4:4:4 and
    /// 4:2:2 to their own transcriptions of libaom's 444/422 subsampling
    /// bodies BEFORE the 4:2:0 fallthrough, and the fallthrough is the only
    /// road to `cfl_ac_q3_at`.
    ///
    /// Two-sided on purpose. The 4:4:4 arm asserts `> 0` (the CfL path
    /// fired, so the `== 0` below is a routing fact and not an absence of
    /// CfL) and the 4:2:0 arm asserts `> 0` on the SAME counter (so the
    /// counter is live and the 4:4:4 zero is not a dead one). Both arms
    /// compare every decoded frame to the instrumented `aomdec`
    /// plane by plane, and the 4:2:0 arm is the "unchanged" quote: its
    /// per-plane mismatch counts must be 0/0/0.
    ///
    /// Provenance: both fixtures are aomenc-produced and already pinned
    /// with sha256 by the gates named beside them. 4:4:4 lossy
    /// `444_lossy_superres_mode2_256x128.obu` (16620 bytes, sha256
    /// `c18496cf3b7db0feca9aa904024cec2115be80b5b94f87c848ac52f9b32ffd97`,
    /// `testsrc2=size=256x128:rate=25 -pix_fmt yuv444p`, `--passes=1
    /// --end-usage=q --cq-level=20 --cpu-used=2 --superres-mode=2
    /// --limit=4 --obu`, 4 frames); 4:2:0 lossy
    /// `420_intrabc_rect4_witness.obu` (890 bytes, sha256
    /// `d01352af5059919cdce733abdcbae8e8240ac3c2a6190fa1bd96d3c6f945b979`).
    #[test]
    fn cfl_ac_444_routes_to_its_own_1x1_luma_body_and_420_to_the_2x2_average() {
        const NAME: &str = "cfl_ac_444_routes_to_its_own_1x1_luma_body_and_420_to_the_2x2_average";
        // (fixture, len, fnv1a64, sha256, (w, h), frames, cfL arm index, is_444)
        const ARMS: [(&str, usize, u64, &str, (usize, usize), usize, usize, bool); 2] = [
            (
                "fixtures/444_lossy_superres_mode2_256x128.obu",
                16620,
                0x8cbb_36b2_5e0f_07ab,
                "c18496cf3b7db0feca9aa904024cec2115be80b5b94f87c848ac52f9b32ffd97",
                (256, 128),
                4,
                0,
                true,
            ),
            (
                "fixtures/420_intrabc_rect4_witness.obu",
                890,
                0x57ec_3da2_a63b_403a,
                "d01352af5059919cdce733abdcbae8e8240ac3c2a6190fa1bd96d3c6f945b979",
                (640, 480),
                1,
                2,
                false,
            ),
        ];
        let _guard = lock_gate_counters();
        for (fixture, len, fnv, sha, (w, h), want_frames, arm_idx, is_444) in ARMS {
            let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(fixture);
            let stream = std::fs::read(&obu).unwrap_or_else(|e| {
                panic!(
                    "{NAME}: pinned fixture {fixture} is missing ({e}) -- the gate cannot run, \
                     which is a failure, not a skip"
                )
            });
            assert_eq!(stream.len(), len, "{NAME}: {fixture} length moved");
            assert_eq!(fnv1a64(&stream), fnv, "{NAME}: {fixture} bytes moved");

            // The arm counters are process-wide with no reset, so every read
            // is a DELTA: a full-suite run has already put other streams'
            // CfL blocks through this decoder, and an absolute count would be
            // a non-vacuity assertion satisfied by residue.
            let arms_before = crate::decode::cfl_ac_arm_hits();
            let q3_before = crate::decode::cfl_ac_q3_at_hits();
            let frames = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME}: {fixture} no longer decodes cleanly: {e}"));
            let arms = crate::decode::cfl_ac_arm_hits();
            let q3_delta = crate::decode::cfl_ac_q3_at_hits() - q3_before;
            let arm_delta: Vec<usize> = (0..4).map(|i| arms[i] - arms_before[i]).collect();

            // The arm this stream's format MUST take, and the count that
            // proves the CfL path is live on it. At 4:4:4 that is arm 0
            // (`cfl_luma_subsampling_444_lbd_c`'s 1x1 body); at 4:2:0 it is
            // arm 2 (the square fallthrough into `cfl_ac_q3`).
            assert!(
                arm_delta[arm_idx] > 0,
                "{NAME}: {fixture} ran {} 4:4:4 / {} 4:2:2 / {} 4:2:0-square / \
                 {} 4:2:0-rect CfL blocks; arm {arm_idx} is the one this format must \
                 take and it is ZERO -- the stream no longer exercises CfL, so the \
                 routing assertion below would be vacuous (class gate-blind-to-feature)",
                arm_delta[0],
                arm_delta[1],
                arm_delta[2],
                arm_delta[3],
            );
            if is_444 {
                assert_eq!(
                    q3_delta, 0,
                    "{NAME}: {fixture} is 4:4:4 and entered the format-blind cfl_ac_q3_at \
                     {q3_delta} time(s) -- its extent is bw/2, bh/2 and its footprint is a \
                     2x2 luma average, both wrong at ss (0,0) (libaom cfl.c:230 vs :257). \
                     Arm deltas {arm_delta:?}. Either cfl_ac_ss lost its 4:4:4 arm or a new \
                     caller reached cfl_ac_q3_at directly; both make this site LIVE"
                );
                assert_eq!(
                    (arm_delta[2], arm_delta[3]),
                    (0, 0),
                    "{NAME}: {fixture} is 4:4:4 but ran the 4:2:0 CfL arms -- the format test \
                     that keeps cfl_ac_q3_at format-blind has moved"
                );
            } else {
                assert!(
                    q3_delta > 0,
                    "{NAME}: {fixture} is 4:2:0 and cfl_ac_q3_at was entered ZERO times \
                     (arm deltas {arm_delta:?}) -- the counter this gate proves 4:4:4 never \
                     touches is dead on 4:2:0 too, so the 4:4:4 assertion is a dead zero"
                );
                assert_eq!(
                    (arm_delta[0], arm_delta[1]),
                    (0, 0),
                    "{NAME}: {fixture} is 4:2:0 but ran a 4:4:4 or 4:2:2 CfL arm"
                );
            }
            assert_eq!(frames.len(), want_frames, "{NAME}: {fixture} frame count");
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(
                    (f.width, f.height),
                    (w, h),
                    "{NAME}: {fixture} frame {i} dimensions"
                );
                if is_444 {
                    assert_eq!(
                        f.u.len(),
                        w * h,
                        "{NAME}: {fixture} frame {i} chroma plane is {} samples; at 4:4:4 it \
                         must be full resolution ({w}x{h})",
                        f.u.len()
                    );
                }
            }
            // Per-plane mismatch counts against the instrumented oracle, so
            // the "4:2:0 unchanged" claim is a number rather than a claim.
            if aomdec_available(NAME) {
                let [y, u, v] = oracle_plane_diffs(
                    &stream,
                    NAME,
                    [frames[0].y.len(), frames[0].u.len(), frames[0].v.len()],
                );
                assert!(
                    y == 0 && u == 0 && v == 0,
                    "{NAME}: {fixture} differs from the oracle on {y}/{u}/{v} of the \
                     {w}x{h} Y/U/V samples -- sha256 {sha}"
                );
                eprintln!(
                    "{NAME}: {fixture} arm{arm_idx} {arm_delta:?}, cfl_ac_q3_at {q3_delta}, \
                     per-plane Y/U/V mismatches vs aomdec {y}/{u}/{v} of {w}x{h}; \
                     sha256 {sha}"
                );
            }
        }
    }

    /// lane-av1cfl: the STATIC half of the same claim -- `cfl_ac_q3_at`'s
    /// caller set, read out of the sources themselves, is what makes its
    /// `bw / 2, bh / 2` extent and 2x2 luma footprint correct rather than
    /// merely untested. The counter gate above measures that on two pinned
    /// streams; this one survives a change no fixture would catch.
    ///
    /// **What is asserted, and why each part is load-bearing.**
    ///
    /// 1. `cfl_ac_q3_at` has exactly **three** call sites -- the two 4:2:0
    ///    wrappers `cfl_ac_q3` / `cfl_ac_q3_rect` in `decode.rs` and the
    ///    encoder's CfL candidate in `encode.rs`. A fourth would be a new
    ///    road to a format-blind body.
    /// 2. Both `decode.rs` wrappers are called from **exactly one** function,
    ///    `cfl_ac_ss`, and only from its LAST two arms -- the ones after the
    ///    `ss_x == 0 && ss_y == 0` (4:4:4) and `ss_x == 1 && ss_y == 0`
    ///    (4:2:2) tests. So every decode-side entry is 4:2:0 by
    ///    construction, whatever a stream's header says.
    /// 3. The encoder's call site is 4:2:0 because `Colour::color_config` and
    ///    `unspecified_color_config` both hardcode `subsampling_x: 1,
    ///    subsampling_y: 1` -- there is no 4:4:4 encoder colour config in the
    ///    crate. A future encoder that grows one must thread `ss` into this
    ///    call, and the `subsampling_x: 0` count going above zero is what
    ///    says so.
    ///
    /// (0, 1) needs no arm: `ec-av1-syntax` reads `subsampling_y` only when
    /// `subsampling_x == 1` (`crates/ec-av1-syntax/src/sequence.rs:483-485`),
    /// so the pair is structurally uncodable and the 4:2:0 fallthrough is the
    /// only residual. lane-av1422lift: this sentence used to end "4:2:2 is
    /// refused at the sequence header", which was the wrong reason -- the
    /// claim holds because `cfl_ac_ss` DISPATCHES `ss (1,0)` before the 4:2:0
    /// fallthrough, not because the format could not arrive. The gate's
    /// mechanism is unchanged; only the reason was stale.
    #[test]
    fn cfl_ac_q3_at_is_reached_only_through_the_420_fallthrough_of_cfl_ac_ss() {
        const NAME: &str = "cfl_ac_q3_at_is_reached_only_through_the_420_fallthrough_of_cfl_ac_ss";
        let decode = crate::test_source::module_source("decode");
        let encode = crate::test_source::module_source("encode");

        // (1) The call-site set, definition excluded. A declaration is
        // preceded by "pub(crate) fn " / "fn "; a call is not.
        let mut sites: Vec<&str> = Vec::new();
        for src in [decode, encode] {
            for (i, _) in src.match_indices("cfl_ac_q3_at(") {
                if src[..i].ends_with("fn ") {
                    continue;
                }
                sites.push(if src == decode {
                    "decode.rs"
                } else {
                    "encode.rs"
                });
            }
        }
        let mut per_file: std::collections::BTreeMap<&str, usize> =
            std::collections::BTreeMap::new();
        for s in &sites {
            *per_file.entry(s).or_default() += 1;
        }
        assert_eq!(
            per_file,
            std::collections::BTreeMap::from([("decode.rs", 2), ("encode.rs", 1)]),
            "{NAME}: cfl_ac_q3_at's call-site set changed -- {per_file:?} against an expected \
             2 in decode.rs (the cfl_ac_q3 / cfl_ac_q3_rect wrappers) and 1 in encode.rs (the \
             encoder's CfL candidate). A new call site must thread the frame's subsampling in; \
             the body is a 4:2:0 2x2 luma average over a bw/2 x bh/2 extent and is WRONG at \
             4:4:4 on both counts (libaom cfl_luma_subsampling_444_lbd_c is 1x1 over the full \
             extent, cfl.c:257)"
        );

        // (2) Both wrappers have exactly one caller, and it is cfl_ac_ss.
        let cfl_ac_ss_at = decode
            .find("fn cfl_ac_ss(")
            .expect("cfl_ac_ss is gone -- re-derive the fallthrough arms below");
        for wrapper in ["cfl_ac_q3(", "cfl_ac_q3_rect("] {
            let mut ats = Vec::new();
            for (i, _) in decode.match_indices(wrapper) {
                if decode[..i].ends_with("fn ") {
                    continue;
                }
                ats.push(i);
            }
            // The scan string carries its own open paren, so it cannot match
            // `cfl_ac_q3_rect(` or `cfl_ac_q3_at(` -- those are `cfl_ac_q3`
            // followed by `_`, not `(`. No prefix filter is needed.
            assert_eq!(
                ats.len(),
                1,
                "{NAME}: `{wrapper}` is called {} time(s) outside its own definition, expected \
                 exactly 1 (from cfl_ac_ss) -- a second caller is a new road to the 4:2:0 \
                 average that this gate's counter arm measures only on 4:2:0",
                ats.len()
            );
            assert!(
                ats[0] > cfl_ac_ss_at && ats[0] < cfl_ac_ss_at + 4000,
                "{NAME}: `{wrapper}`'s only call site is at byte {} and cfl_ac_ss is at {} -- \
                 the call has moved out of the 4:2:0 fallthrough into something that may be \
                 handed a 4:4:4 or 4:2:2 block",
                ats[0],
                cfl_ac_ss_at
            );
        }

        // Inside cfl_ac_ss, the two format tests must come BEFORE both
        // wrapper calls, and the wrappers must be the fallthrough -- a 4:4:4
        // block returns from the first arm and never reaches either.
        let body_end = decode[cfl_ac_ss_at..]
            .find("\nfn ")
            .expect("cfl_ac_ss must be a top-level fn")
            + cfl_ac_ss_at;
        let body = &decode[cfl_ac_ss_at..body_end];
        let t444 = body.find("if ss_x == 0 && ss_y == 0 {").expect(
            "cfl_ac_ss lost its 4:4:4 arm -- every 4:4:4 CfL block now falls into the \
2:0 average",
        );
        let t422 = body
            .find("} else if ss_x == 1 && ss_y == 0 {")
            .expect("cfl_ac_ss lost its 4:2:2 arm -- a 4:2:2 block would take the 4:2:0 average");
        let sq = body
            .find("cfl_ac_q3(y, px, py, bw)")
            .expect("cfl_ac_ss lost its 4:2:0 square fallthrough");
        let rect = body
            .find("cfl_ac_q3_rect(y, px, py, bw, bh)")
            .expect("cfl_ac_ss lost its 4:2:0 rect fallthrough");
        assert!(
            t444 < t422 && t422 < sq && sq < rect,
            "{NAME}: cfl_ac_ss's arm order moved (4:4:4 at {t444}, 4:2:2 at {t422}, 4:2:0 square \
             at {sq}, 4:2:0 rect at {rect}) -- the format-blind 2x2 average must stay the \
             FALLTHROUGH, after both format tests have had their chance to return"
        );

        // (3) The encoder's colour configs are 4:2:0 only.
        for (src, label) in [
            (encode, "encode.rs"),
            (crate::test_source::module_source("encoder"), "encoder.rs"),
        ] {
            let n = src.matches("subsampling_x: 0").count();
            assert_eq!(
                n, 0,
                "{NAME}: {label} now builds a 4:4:4 ColorConfig ({n} site(s)) -- the encoder's \
                 cfl_ac_q3_at call (encode.rs) is a 4:2:0 2x2 average over a half extent and \
                 must thread ss before any encoder can emit one"
            );
        }
    }

    /// lane-av1444edge r3: the FIRST 4:4:4 superres cell in the crate, and
    /// the cell the chroma-format sweep's "4:4:4 superres" row was supposed
    /// to be.
    ///
    /// **Why this gate exists at all.** §6.2 of
    /// `lanes/av1formatsweep.report.md` recorded that cell as DIVERGENT from
    /// frame 2 with a first wrong sample of Y(192,0). It was never a superres
    /// cell: the recipe passed `--superres-mode=1` and no denominator, so
    /// libaom's `get_superres_denom_for_qindex`
    /// (`av1/encoder/superres_scale.c:143`) chose 8, and all four frame
    /// headers of that stream read `use_superres=false denom=8`. Nothing was
    /// ever scaled; it was a plain 4:4:4 lossy cq-20 stream, and its
    /// divergence was the same H1 class as the 130x122 cell beside it (see
    /// `lanes/av1444edge.report.md` §2). That is the trap this gate is built
    /// to make impossible to repeat: it asserts the scaled geometry from the
    /// PARSED HEADERS, so a future flag or heuristic change that silently
    /// unsizes the stream makes the gate red on a header assertion instead of
    /// turning into a differently-shaped exactness claim.
    ///
    /// **This is the only 4:4:4 superres coverage here.** All eight
    /// pre-existing superres gates are 4:2:0, checked rather than assumed:
    /// the two pinned fixtures parse `ss (1,1)` from their own sequence
    /// headers (`superres_kf_cdef_lr_64x64.obu` 8-bit,
    /// `superres_alltools_sb128_320x180.obu` 10-bit) and the six live-aomenc
    /// gates all feed a `yuv420p` / `yuv420p10le` source. So this cell's
    /// claim is scoped honestly: it is the FIRST 4:4:4 superres stream, not
    /// a broadening of an existing family.
    ///
    /// **The recipe** (sha256
    /// `450caa3e31526a8973b9f64d5030a2f11b87bd60fac07aef121cb62af9bf85ff`,
    /// 14808 bytes, 4 frames, `testsrc2 256x128 rate=25` yuv444p):
    /// ```text
    /// ffmpeg -f lavfi -i "testsrc2=size=256x128:rate=25" -frames:v 4 \
    ///        -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe - | \
    /// aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 \
    ///        --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
    ///        --limit=4 --superres-mode=1 --superres-denominator=12 \
    ///        --superres-kf-denominator=12 --obu -o - -
    /// ```
    /// The two denominator flags are the whole point: without them the cell
    /// does not exist. Verified byte-reproducible — re-encoding this recipe
    /// reproduces the pinned 14808 bytes exactly.
    ///
    /// **What the cell exercises.** Coded 171x128 upscaled to 256x128 at
    /// `denom = 12`, so a 4:4:4 chroma plane is upscaled at FULL resolution
    /// (a 4:2:0 stream halves it first, and libaom's `av1_superres_upscale`
    /// takes a different path per chroma format). Measured `superres_hits` 4
    /// and `mc::predict_scaled_hits` 2130 — the upscaler and scaled inter
    /// prediction both run, so the cell is not passing on an unscaled
    /// stream.
    ///
    /// Non-vacuity, four ways: the pinned bytes (length + FNV-1a); the parsed
    /// headers really are `use_superres` at `denom 12` with the SCALED coded
    /// size, and the coded width is re-derived from `UpscaledWidth` and the
    /// denominator rather than only compared to a literal, so a re-pinned
    /// fixture cannot drift past the assertion; both counters are asserted as
    /// DELTAS; and every decode-order frame is compared byte-for-byte
    /// through `decode_all_frames_vs_oracle`, which is what makes the
    /// upscaled width on both sides a real check.
    #[test]
    fn a_444_lossy_superres_stream_decodes_pixel_exact() {
        const NAME: &str = "a_444_lossy_superres_stream_decodes_pixel_exact";
        const FIXTURE: &str = "fixtures/444_lossy_superres_256x128_d12.obu";
        const FIXTURE_LEN: usize = 14808;
        const FIXTURE_FNV: u64 = 0x44e0_d12f_1155_0e09;
        /// Quoted so a re-pin has to be deliberate; the FNV above is what the
        /// gate actually compares, this is what a human reads.
        const FIXTURE_SHA256: &str =
            "450caa3e31526a8973b9f64d5030a2f11b87bd60fac07aef121cb62af9bf85ff";
        const UPSCALED: (usize, usize) = (256, 128);
        const CODED: (usize, usize) = (171, 128);
        const DENOM: u8 = 12;
        const FRAMES: usize = 4;
        let obu = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
        let stream = std::fs::read(&obu).unwrap_or_else(|e| {
            panic!(
                "{NAME}: pinned fixture {} is missing ({e}) -- the gate cannot run, \
                 which is a failure, not a skip",
                obu.display()
            )
        });
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        // --- the header facts the CELL depends on, parsed not assumed ------
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut seq = None;
        let mut heads: Vec<FrameHeader> = Vec::new();
        while pos < stream.len() {
            let obu = parser
                .parse_obu(&stream[pos..])
                .unwrap_or_else(|e| panic!("{NAME}: aomenc OBU does not parse: {e:?}"));
            if obu.total_size == 0 {
                break;
            }
            pos += obu.total_size;
            match obu.kind {
                ObuKind::SequenceHeader(s) => seq = Some(s),
                ObuKind::FrameHeader(h) => heads.push(*h),
                ObuKind::Frame(h, _) => heads.push(*h),
                _ => {}
            }
        }
        let seq = seq.unwrap_or_else(|| panic!("{NAME}: the stream carries no sequence header"));
        assert_eq!(
            (
                seq.color_config.subsampling_x,
                seq.color_config.subsampling_y
            ),
            (0, 0),
            "{NAME}: the stream is not 4:4:4 (ss = ({}, {}))",
            seq.color_config.subsampling_x,
            seq.color_config.subsampling_y
        );
        assert_eq!(
            seq.color_config.bit_depth, 8,
            "{NAME}: not the 8-bit cell this gate names"
        );
        assert!(
            seq.enable_superres,
            "{NAME}: the sequence header does not enable superres at all"
        );
        assert_eq!(
            heads.len(),
            FRAMES,
            "{NAME}: {} frame headers, expected {FRAMES}",
            heads.len()
        );
        for (i, h) in heads.iter().enumerate() {
            // The assertion the phantom cell failed. Without the denominator
            // flags aomenc picks 8 and this is false while the stream still
            // decodes, which is how a whole sweep cell ends up mislabelled.
            assert!(
                h.use_superres,
                "{NAME}: frame {i} has use_superres = false -- this is an UNSCALED \
                 stream and the gate would be measuring something other than the \
                 cell it names (class gate-blind-to-feature)"
            );
            assert_eq!(
                h.superres_denom, DENOM,
                "{NAME}: frame {i} denominator moved from {DENOM}"
            );
            assert_eq!(
                (h.upscaled_width as usize, h.frame_height as usize),
                (UPSCALED.0, UPSCALED.1),
                "{NAME}: frame {i} upscaled size moved"
            );
            assert_eq!(
                (h.frame_width as usize, h.frame_height as usize),
                (CODED.0, CODED.1),
                "{NAME}: frame {i} CODED size moved -- this is the scaled half of \
                 the cell and the part an unsized stream gets wrong"
            );
            // Re-derive rather than only compare literals, so a re-pinned
            // fixture cannot satisfy the numbers above while the scaled-size
            // arithmetic has moved under them.
            assert_eq!(
                (h.upscaled_width * 8 + u32::from(DENOM) / 2) / u32::from(DENOM),
                h.frame_width,
                "{NAME}: frame {i}: FrameWidth is not (UpscaledWidth*8 + denom/2)/denom -- \
                 the scaled size the cell rests on is not the spec's"
            );
        }

        // --- the pixels -----------------------------------------------------
        let _guard = lock_gate_counters();
        let before_sr = crate::superres::superres_hits();
        let before_scaled = crate::mc::predict_scaled_hits();
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{NAME}: the pinned 4:4:4 superres stream was refused: {e}")
        });
        let scaled_up = crate::superres::superres_hits() - before_sr;
        let scaled_mc = crate::mc::predict_scaled_hits() - before_scaled;
        assert!(
            scaled_up > 0,
            "{NAME}: zero superres_hits -- the upscaler never ran (class \
             gate-blind-to-feature)"
        );
        assert!(
            scaled_mc > 0,
            "{NAME}: zero predict_scaled_hits -- no inter block read a scaled reference, \
             so the scaled-PREDICTION half of the cell is unexercised (class \
             gate-blind-to-feature)"
        );
        assert_eq!(frames.len(), FRAMES, "{NAME}: decoded frame count");
        for (i, f) in frames.iter().enumerate() {
            // The upscale must have REPLACED the frame: a decoder that
            // returned the 171-wide coded picture fails here, and without
            // this check the chroma-shape assertion below could be satisfied
            // by quarter-size planes.
            assert_eq!(
                (f.width, f.height),
                UPSCALED,
                "{NAME}: frame {i} came back {}x{}, not the UPSCALED {}x{}",
                f.width,
                f.height,
                UPSCALED.0,
                UPSCALED.1
            );
            assert_eq!(
                f.u.len(),
                UPSCALED.0 * UPSCALED.1,
                "{NAME}: frame {i} chroma plane is {} samples; at 4:4:4 upscaled it must \
                 be full resolution ({}x{})",
                f.u.len(),
                UPSCALED.0,
                UPSCALED.1
            );
            assert_eq!(
                f.v.len(),
                UPSCALED.0 * UPSCALED.1,
                "{NAME}: frame {i} V plane is {} samples, expected full resolution",
                f.v.len()
            );
        }
        if aomdec_available(NAME) {
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            eprintln!(
                "{NAME}: {decoded} decode-order frame(s) ({hidden} hidden) byte-exact vs \
                 aomdec at {UPSCALED:?} upscaled from {CODED:?} denom {DENOM}, \
                 {scaled_up} upscale(s), {scaled_mc} scaled MC block(s); fixture sha256 \
                 {FIXTURE_SHA256}"
            );
        }
    }

    /// lane-av1superpin (a): `--superres-mode=1 --superres-denominator=9`, the
    /// WIDEST frame-edge margin this 256x128 geometry allows -- coded 228x128
    /// upscaled to 256x128, `denom = 9`, on all four frames.
    ///
    /// **Why 9 and not 12.** The den=12 cell beside it (coded 171x128) is the
    /// one the chroma-format sweep's matrix row ended up pointing at, and
    /// denom 16 (coded 128x128) is the degenerate one where the coded width
    /// equals the superblock grid exactly. Nine is the largest denominator
    /// libaom's own `SUPERRES_NUM` ladder accepts here and still scales, and
    /// it is the member that leaves the most coded samples at the frame's
    /// right edge: `228 * 8 / 256` of the frame is a 28-column upscale margin,
    /// against 12's 85 columns. A wide margin is where a round-trip is most
    /// likely to read past the coded picture, so it is the member worth
    /// pinning next to the den=12 one.
    ///
    /// **The recipe** (sha256
    /// `3d619ee0fda3d9bb9cf8ff0541ba00ea5d83ae38cd8eec1322f8dbbbe6a5ec96`,
    /// 17437 bytes, 4 frames, `testsrc2 256x128 rate=25` yuv444p):
    /// ```text
    /// ffmpeg -f lavfi -i "testsrc2=size=256x128:rate=25" -frames:v 4 \
    ///        -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe - | \
    /// aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 \
    ///        --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
    ///        --limit=4 --superres-mode=1 --superres-denominator=9 \
    ///        --superres-kf-denominator=9 --obu -o - -
    /// ```
    /// Byte-reproducible: re-encoding this recipe reproduces the pinned 17437
    /// bytes exactly, and the PARSED headers of that re-encode were read
    /// (`frame=228x128 upscaled=256 use_superres=true denom=9` on all four) --
    /// which is where the numbers below come from, not from the flags.
    #[test]
    fn a_444_lossy_superres_mode1_den9_stream_decodes_pixel_exact() {
        a_444_superres_arm(
            "a_444_lossy_superres_mode1_den9_stream_decodes_pixel_exact",
            "fixtures/444_lossy_superres_256x128_d9.obu",
            17437,
            0x1ff2_fb84_528f_aa05,
            "3d619ee0fda3d9bb9cf8ff0541ba00ea5d83ae38cd8eec1322f8dbbbe6a5ec96",
            (256, 128),
            &[(9, 228); 4],
        );
    }

    /// lane-av1superpin (b): `--superres-mode=2` (random denominator), coded
    /// 186x128 on the first frame and a DIFFERENT denominator on each frame
    /// after it.
    ///
    /// **This is the cell that makes the flag-vs-header distinction
    /// unavoidable.** Measured on the pinned bytes by parsing every frame
    /// header, `--superres-mode=2` does not honour `--superres-denominator`
    /// at all -- encoding the recipe four times (no denominator, then 9, 12,
    /// 16) produced ONE stream, sha256
    /// `c18496cf3b7db0feca9aa904024cec2115be80b5b94f87c848ac52f9b32ffd97`
    /// (16620 bytes), byte-identical every time -- and the parsed headers of
    /// that stream are:
    ///
    /// ```text
    /// FH0 frame=186x128 upscaled=256 use_superres=true denom=11
    /// FH1 frame=146x128 upscaled=256 use_superres=true denom=14
    /// FH2 frame=137x128 upscaled=256 use_superres=true denom=15
    /// FH3 frame=228x128 upscaled=256 use_superres=true denom=9
    /// ```
    ///
    /// So a gate written against this cell that read the flag instead of the
    /// header would assert `denom == 9` (or 12, or 16) and be wrong on three
    /// of four frames -- or, worse, pass while printing numbers the stream does
    /// not carry. Three of the four parsed denominators (11, 14, 15) are values
    /// no flag on the command line ever named, which is the property the arm
    /// below asserts directly.
    ///
    /// **Why it is worth a gate of its own** beyond the flag-ignoring point:
    /// the den=12 and den=9 cells both scale every frame at one denominator,
    /// so `av1_superres_upscale` is only ever handed one coded-to-upscaled
    /// ratio. This stream hands it four (256->186, 256->146, 256->137,
    /// 256->228) in one decode, and libaom's upscaler picks its filter taps
    /// from `denom` per frame. That is the case a single-denominator gate
    /// cannot reach.
    ///
    /// **The recipe**:
    /// ```text
    /// ffmpeg -f lavfi -i "testsrc2=size=256x128:rate=25" -frames:v 4 \
    ///        -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe - | \
    /// aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 \
    ///        --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
    ///        --limit=4 --superres-mode=2 --obu -o - -
    /// ```
    #[test]
    fn a_444_lossy_superres_mode2_random_denom_stream_decodes_pixel_exact() {
        const NAME: &str = "a_444_lossy_superres_mode2_random_denom_stream_decodes_pixel_exact";
        let heads = a_444_superres_arm(
            NAME,
            "fixtures/444_lossy_superres_mode2_256x128.obu",
            16620,
            0x8cbb_36b2_5e0f_07ab,
            "c18496cf3b7db0feca9aa904024cec2115be80b5b94f87c848ac52f9b32ffd97",
            (256, 128),
            &[(11, 186), (14, 146), (15, 137), (9, 228)],
        );

        // The flag-ignoring arm. Every denominator libaom accepted on the
        // command line, plus the recipe's own "no denominator at all", must
        // land on the SAME bytes as the pinned fixture. If a future libaom
        // starts honouring the flag under mode 2 this goes red -- and that is
        // the point: the gate above is pinned to parsed denominators, so the
        // day the flag starts mattering the pin has to be re-derived from the
        // stream, not from the recipe.
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("{NAME}: flag-ignoring arm skipped (no ffmpeg/aomenc)");
            return;
        }
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=256x128:rate=25",
                "-frames:v",
                "4",
                "-pix_fmt",
                "yuv444p",
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
            "{NAME}: ffmpeg refused to generate the y4m source: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let pinned = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/444_lossy_superres_mode2_256x128.obu"),
        )
        .expect("pinned fixture disappeared between the two reads");
        for extra in [
            vec![],
            vec!["--superres-denominator=9"],
            vec!["--superres-denominator=12"],
            vec!["--superres-denominator=16"],
        ] {
            let mut args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=20",
                "--cpu-used=2",
                "--threads=1",
                "--row-mt=0",
                "--lag-in-frames=0",
                "--kf-max-dist=100",
                "--limit=4",
                "--superres-mode=2",
            ];
            args.extend(extra.iter().copied());
            args.extend(["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{NAME}: aomenc refused the recipe with {extra:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                out.stdout.len(),
                pinned.len(),
                "{NAME}: --superres-mode=2 {extra:?} produced {} bytes, pinned is {} -- \
                 mode 2 now honours --superres-denominator (or stopped scaling), and this \
                 gate's per-frame denominators are pinned to the PARSED headers, not to \
                 the recipe",
                out.stdout.len(),
                pinned.len()
            );
            assert_eq!(
                fnv1a64(&out.stdout),
                fnv1a64(&pinned),
                "{NAME}: --superres-mode=2 {extra:?} does NOT reproduce the pinned bytes"
            );
        }
        // And the denominators the stream carries must include values no flag
        // on the command line ever named -- the property that makes "read the
        // parsed header" the only correct source for the table above.
        let flagged = [9u8, 12, 16];
        assert!(
            heads
                .iter()
                .any(|h| h.superres_denom > 0 && !flagged.contains(&h.superres_denom)),
            "{NAME}: every parsed denominator is one of the flags {flagged:?} -- if libaom \
             ever starts honouring --superres-denominator under mode 2 this cell stops being \
             the flag-ignoring one and the pin has to be re-derived"
        );
        eprintln!(
            "{NAME}: --superres-mode=2 with no/9/12/16 denominator flags all reproduce the \
             pinned {} bytes; parsed denominators {:?} (flag values {flagged:?} ignored)",
            pinned.len(),
            heads.iter().map(|h| h.superres_denom).collect::<Vec<_>>()
        );
    }

    /// lane-av1lossy128x96: the 128x96 sibling of the pinned 128x128 witness
    /// `a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type`
    /// (commit 6c33f204). That fix closed the "4:4:4 lossy + TX size search"
    /// divergence (H2) for the 128x128 twin; the 128x96 geometry -- two whole
    /// 64x64 roots over a frame whose second block row is only 32 rows tall,
    /// which is why it exercises BOTH chroma planes and a partial block row --
    /// was left ungated, and it is the geometry the sweep measured the
    /// U+V form of the defect on (608 wrong samples from decode-order frame 2).
    ///
    /// Measured for this lane on the fix's parent `4155c7c7`: decode-order
    /// frames 2 and 3 differ from the oracle in 608 and 692 samples (luma
    /// exact, U and V wrong), entropy bit-identical; at HEAD and at `6c33f204`
    /// all four frames are byte-exact. So the defect is FIXED and this gate is
    /// the pin that keeps it fixed.
    ///
    /// Recipe (encoded LIVE, no fixture): `testsrc2 128x96:rate=25` yuv444p
    /// 4 frames, `aomenc --profile=1 --codec=av1 --passes=1 --end-usage=q
    /// --cq-level=20 --cpu-used=2 --sb-size=64 --min-partition-size=64
    /// --max-partition-size=64 --enable-tx-size-search=1 --threads=1 --row-mt=0
    /// --lag-in-frames=0 --kf-max-dist=100 --limit=4 --obu -o - -`.
    /// Search ON: 21830 B, sha256 `7380bcbbfb791c6a369850357211223e20fab959423135951f6eb43ffbfcfebd`,
    /// fnv1a64 `0x1ba3fe9a4335bdbb`. Control arm, same recipe with
    /// `--enable-tx-size-search=0`: 21296 B, sha256
    /// `347edcc9f4899fb05b15174b82d37f6562dcc6fe7691af0ed9ba4efd6cc878c6`,
    /// fnv1a64 `0x2f87d8e98c9db686`.
    ///
    /// NON-VACUITY, and the search flag is NOT the assert: the recipe SPELLS
    /// `--enable-tx-size-search=1` (aomenc keeps the FIRST occurrence, and a
    /// flag left defaulted is "unknown", not "on"), and then
    /// 1. every frame header's PARSED `tx_mode` is `TxMode::Select`
    ///    (`TX_MODE_SELECT`) -- the searched transform mode is really in the
    ///    bitstream, not merely requested. The control arm asserts the
    ///    opposite (`TxMode::Largest`), so neither arm can pass on the other's
    ///    stream;
    /// 2. the sequence header is `ss (0,0)` at 8 bits (`assert_444_header`),
    ///    and the stream is byte-identical to the recorded encode (length +
    ///    fnv1a64), so an aomenc that stopped producing this cell fails here
    ///    rather than silently measuring something else;
    /// 3. `decode::chroma_quad_leaf_tx_diff_hits()` strictly increases on the
    ///    search arm -- the number that says the per-quadrant resolve CHANGED
    ///    an answer, which is the defect itself. It must be exactly 0 on the
    ///    search-off control: with no var-tx there is no four-leaf luma tree,
    ///    so the arm is unreachable and a counter that fired there would be
    ///    counting something else;
    /// 4. every decoded plane is 128*96 (at `ss (0,0)` chroma is full
    ///    resolution, so a subsampled extent fails the shape assert);
    /// 5. `decode_all_frames_vs_oracle` asserts the decode-order frame COUNT
    ///    and each frame's byte LENGTH before any sample is compared.
    ///
    /// Red-before: reverting ONLY the per-quadrant resolve in
    /// `decode_inter_block`'s 64x64 four-unit chroma arm (the `covering_leaf_tx_type`
    /// call back to the block-level `first_tx_type`, counters and this gate
    /// untouched) turns this gate red on decode-order frame 2 with 608 bytes
    /// differing, and green again on restore.
    #[test]
    fn a_real_aomenc_444_whole_64_root_tx_size_search_stream_decodes_pixel_exact_at_128x96() {
        const NAME: &str =
            "a_real_aomenc_444_whole_64_root_tx_size_search_stream_decodes_pixel_exact_at_128x96";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        // (label, --enable-tx-size-search, parsed tx mode, bytes, fnv1a64)
        let arms: [(&str, &str, TxMode, usize, u64); 2] = [
            (
                "tx size search on",
                "1",
                TxMode::Select,
                21830,
                0x1ba3_fe9a_4335_bdbb,
            ),
            (
                "tx size search off (control)",
                "0",
                TxMode::Largest,
                21296,
                0x2f87_d8e9_8c9d_b686,
            ),
        ];
        for (label, search, want_mode, want_len, want_fnv) in arms {
            let search_flag = format!("--enable-tx-size-search={search}");
            let stream = chroma_format_stream(
                128,
                96,
                "yuv444p",
                8,
                4,
                &[
                    "--profile=1",
                    "--cq-level=20",
                    "--cpu-used=2",
                    "--sb-size=64",
                    "--min-partition-size=64",
                    "--max-partition-size=64",
                    &search_flag,
                ],
                0,
            );
            assert_444_header(&stream, NAME, 8);
            assert_eq!(
                stream.len(),
                want_len,
                "{NAME} ({label}): aomenc produced {} bytes, not the recorded {want_len} -- the \
                 recipe no longer makes this cell",
                stream.len()
            );
            assert_eq!(
                fnv1a64(&stream),
                want_fnv,
                "{NAME} ({label}): the encoded bytes moved"
            );
            // PARSED, not requested: the transform mode the bitstream carries.
            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut modes: Vec<TxMode> = Vec::new();
            while pos < stream.len() {
                let obu = parser
                    .parse_obu(&stream[pos..])
                    .unwrap_or_else(|e| panic!("{NAME}: aomenc OBU does not parse: {e:?}"));
                if obu.total_size == 0 {
                    break;
                }
                pos += obu.total_size;
                if let ObuKind::FrameHeader(h) | ObuKind::Frame(h, _) = obu.kind {
                    modes.push(h.tx_mode);
                }
            }
            assert_eq!(
                modes.len(),
                4,
                "{NAME} ({label}): four frame headers expected"
            );
            for (i, mode) in modes.iter().enumerate() {
                assert_eq!(
                    *mode, want_mode,
                    "{NAME} ({label}): decode-order frame {i} carries tx_mode {mode:?}, not \
                     {want_mode:?} -- the searched-transform claim is about the bitstream"
                );
            }
            let diff0 = crate::decode::chroma_quad_leaf_tx_diff_hits();
            let frames = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME} ({label}): refused the stream: {e}"));
            let diff = crate::decode::chroma_quad_leaf_tx_diff_hits() - diff0;
            for (i, f) in frames.iter().enumerate() {
                assert_eq!(
                    f.y.len(),
                    128 * 96,
                    "{NAME} ({label}): frame {i} luma is {} samples, not 128x96",
                    f.y.len()
                );
                assert_eq!(
                    f.u.len(),
                    128 * 96,
                    "{NAME} ({label}): frame {i} U is {} samples -- at 4:4:4 the chroma plane is \
                     full resolution",
                    f.u.len()
                );
            }
            // Pixel compare FIRST: it is the informative failure (it names the
            // decode-order frame and the sample), and the counter assert below
            // is the guard that says WHY the cell is not vacuous.
            let (frames_decoded, _hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert!(
                frames_decoded >= 4,
                "{NAME} ({label}): {frames_decoded} decode-order frames"
            );
            if label.starts_with("tx size search on") {
                assert!(
                    diff >= 2,
                    "{NAME}: the per-quadrant tx_type resolve never CHANGED an answer (diff \
                     {diff}) -- on this stream the four-leaf luma tree under a searched transform \
                     is exactly what makes the block-level value wrong"
                );
            } else {
                assert_eq!(
                    diff, 0,
                    "{NAME}: the four-unit 4:4:4 chroma arm fired with tx size search OFF \
                     ({diff} resolves) -- the counter is not measuring the searched path"
                );
            }
            eprintln!(
                "{NAME} ({label}): {frames_decoded} frames byte-exact vs aomdec, tx_mode \
                 {want_mode:?} on every frame, quad-tx resolves that differed {diff}"
            );
        }
    }

    /// lane-ibc444c: the 4:4:4 intra-BC chroma cell. `decode_intrabc_rect`
    /// sized a rect intra-BC block's chroma plane block with a hardcoded 4:2:0
    /// halving (`bw / 2`, `bh / 2`, origin `px / 2`).
    /// `av1_get_max_uv_txsize` (`av1/common/blockd.h`) is `ss_size_lookup[bsize]`
    /// and applies NO halving of its own, and `get_vartx_max_txsize` (`:1447`)
    /// then reads that plane block as `max_txsize_rect_lookup[plane_bsize]`. So
    /// at 4:4:4 an 8x16 luma block's chroma plane block is 8x16 and libaom
    /// codes it as ONE TX_8X16, while the halving read the same 8x16 footprint
    /// as ONE 4x8 rect unit.
    ///
    /// **Localised by trace, not by reading** (`EC_IBCTX`, this commit's rung
    /// on `read_coeffs_rect`, paired against the oracle's `EC_ECDUMP_IN` by
    /// STREAM BIT POSITION — never by the 16-bit `rng`, which collides at this
    /// scale, and never by a plane label, which is the ORACLE's side of the
    /// pairing):
    ///
    /// ```text
    /// oracle  bit 6870  plane=0 mi=(92,104) tx=7  (TX_8X16) all_zero=0
    /// ours    bit 6885  8x16 skip_ctx=0  decode.rs (read_inter_plane_rect)
    /// oracle  bit 6878  plane=1 mi=(92,104) tx=7  (TX_8X16) all_zero=1
    /// ours    bit 6893  4x8  skip_ctx=0  decode.rs            <-- FORK
    /// oracle  bit 6879  plane=2 mi=(92,104) tx=7  (TX_8X16) all_zero=1
    /// ours    bit 6902  4x8  skip_ctx=0  decode.rs
    /// ```
    ///
    /// The `txb_skip` VALUE first disagrees one unit before the bit position
    /// does: the oracle skips the U unit, we do not, we burn 14 extra bits, and
    /// every later symbol decodes from the wrong place. Reverting the two
    /// arithmetic lines reproduces luma sample **205264** as the first wrong
    /// sample — the exact ceiling `lanes/av1444rect.report.md` rounds 3-4
    /// documented — which is the red-before proof.
    ///
    /// **The 4:4:4 pixel arm is a PREFIX, not an exactness claim, and the
    /// residual is an ENTROPY fork — NOT a reconstruction one.** An earlier
    /// round of this lane claimed the coefficient ladder was "bit-exact for all
    /// 2359 oracle units" and inferred from that the tail was reconstruction.
    /// **That claim was wrong and is retracted** (see this branch's report r2):
    /// the walk compared entry bits at matching LIST INDICES across two traces
    /// of different lengths, and the square `read_coeffs` reader logs an
    /// `all_zero` step even for a read that consumes ZERO bits, so it paired
    /// 0-bit lines against real units. Measured on this branch, with the
    /// 0-bit filter applied to BOTH sides:
    ///
    /// ```text
    /// oracle all_zero reads               2395   (2359 with a real position)
    /// our   all_zero lines               3999
    /// our   lines that consumed bits      1326
    /// 1:1 entry-bit matches, monotone merge  71
    /// ```
    ///
    /// We perform ~1069 FEWER real symbol reads than the oracle. A
    /// reconstruction-only defect cannot make a symbol count come up short, so
    /// the tail is a second entropy fork, first visible in U and V at
    /// **(x=160, y=80) = mi (20, 40)**, spreading to the whole x[320,640)
    /// y[160,480) quadrant. (A figure of "1910 units matching 1:1 with 486 vs
    /// 486 non-zero-bit reads" was relayed to this lane and does NOT reproduce
    /// here — re-measure before quoting either number.)
    ///
    /// **The next reader must pair by BIT INTERVAL, not by index.** The oracle's
    /// `EC_ECDUMP_IN` prints only the ENTRY position; there is no post position,
    /// and entry bits alone drift the moment per-unit bit consumption differs.
    /// The unblock is one line of oracle instrumentation OUTSIDE the repo —
    /// print `aom_reader_tell(r)` after the `aom_read_symbol` at
    /// `~/.cache/aom-oracle/src/av1/decoder/decodetxb.c:158` — then pair on
    /// `(entry, post)`, which is exact and collision-free. Coordinate the
    /// rebuild with the main session; every lane's oracle binary moves.
    ///
    /// 4:2:0 identity is a first-class arm, not a claim: `ss_x = ss_y = 1`
    /// makes `bw >> ss_x` the same arithmetic as the halving, and the pinned
    /// 4:2:0 twin (`420_intrabc_rect4_witness.obu`) is asserted byte-exact
    /// against the oracle in the same run.
    /// lane-av1offtile: a lossless HORZ/VERT strip's PLANE-MAJOR 4x4 chroma
    /// walk is bounded by the CHROMA PLANE, not by the block's own footprint.
    ///
    /// libaom bounds every transform walk per PLANE -- `max_block_wide` /
    /// `max_block_high` (`av1_common_int.h:1565`) add `mb_to_right_edge >>
    /// (3 + pd->subsampling_x)` to the plane block's size before the
    /// `>> MI_SIZE_LOG2`, and `av1_foreach_transformed_block_in_plane`
    /// (`decodeframe.c:1124`) drops any unit whose `blk_col`/`blk_row` is past
    /// them. `decode_rect_split`'s luma walk carries that clip
    /// (`decode.rs`, the `tu_px >= y.true_width` guard lane-hgkf r1 added for
    /// the LUMA plane); its lossless chroma walk counted `chroma_w / 4` x
    /// `chroma_h / 4` off the block instead, so a block overhanging the bottom-
    /// right frame edge read chroma transform units that do not exist.
    ///
    /// MEASURED on `mix176_offtile_chroma_clip.obu`, whose bytes are frame 0
    /// of the 4:2:0 8-bit `mix_176x144_6.obu`
    /// (`ffmpeg -f lavfi -i testsrc2=size=176x144:rate=25 -frames:v 6
    /// -pix_fmt yuv420p`, then `aomenc --codec=av1 --obu --passes=1
    /// --cpu-used=4 --limit=6 --end-usage=q --cq-level=0 --aq-mode=1`; the
    /// whole 6-frame stream is 20 069 B, sha256
    /// `02b15d0d5b08aa5aebf139aee94c31b4f8363d14649c238cd7a19a9785ef124a`,
    /// and this fixture is its first 4 581 B -- the temporal delimiter,
    /// sequence header and frame-0 OBU, nothing else). The bottom-right
    /// superblock is the 48x16 sliver of a 176x144 frame, and its block at
    /// `mi=(32,32)` `px=(128,128)` is `bw=64 bh=32`, so the chroma plane
    /// block (`cpx=64 cpy=64`, plane 88x72) is 32x16 where only 24x8 is coded.
    /// That is `cw_n` 8 -> 6 and `ch_n` 4 -> 2: **20 phantom units per plane,
    /// 40 in total**, and 40 is exactly the extra-read count the paired
    /// `EC_SYMR` ladder measures (ours 25792..25831, the oracle doing a
    /// range-coder init for the next frame there).
    ///
    /// The unit count below is the load-bearing number, not the pixels: the
    /// 40 phantom reads all carry `all_zero` and consume no data (the bit
    /// position freezes at 36288), so frame 0's pixels are byte-exact EITHER
    /// WAY and a pixel compare alone would be blind to this defect. Frame 0's
    /// transform-unit count is 2413 without the clip and 2373 with it, and
    /// **2373 is the oracle's own count** -- the number of `EC_ECDUMP_IN`
    /// lines the shared `aomdec` prints for the same bytes. Reverting the
    /// clip restores 2413 and fails this assert; that is the mutation proof.
    ///
    /// What the 40 reads cost, on the FULL 6-frame stream rather than this
    /// prefix: the desync is 11 970 reads downstream of them, where the
    /// exhausted tile yields an unbounded `read_golomb` tail and the decoder
    /// refuses with `unsupported: AV1 tile (a Golomb tail longer than this
    /// decoder reads)` -- a supported 4:2:0 8-bit cell lost to a walk that ran
    /// off its own tile. Fixing the walk moves the first remaining divergence
    /// on the full stream from SYMR 25 792 to 29 411, a different block at a
    /// different geometry (the var-tx/`lossless` conjunct lane-av1mixllfork
    /// and lane-av1golomb320 own), and both this clip and that one applied
    /// together decode the cell and match the oracle byte for byte. That
    /// composition is measured, not claimed: this gate pins only the walk
    /// this commit fixes.
    #[test]
    fn a_420_lossless_rect_chroma_walk_is_bounded_by_the_chroma_plane() {
        const NAME: &str = "a_420_lossless_rect_chroma_walk_is_bounded_by_the_chroma_plane";
        const FIXTURE: &str = "mix176_offtile_chroma_clip.obu";
        const FIXTURE_LEN: usize = 4581;
        const FIXTURE_FNV: u64 = 0x5e31be5efc9c3c1d;
        /// The oracle's own `EC_ECDUMP_IN` count for these bytes.
        const ORACLE_UNITS: usize = 2373;
        const W: usize = 176;
        const H: usize = 144;

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("fixtures/{FIXTURE}"));
        let stream = read_pin(&path, FIXTURE_LEN, FIXTURE_FNV, NAME);
        assert_eq!(
            stream_bit_depth(&stream, NAME),
            8,
            "{NAME}: the pinned witness is not 8-bit"
        );

        let _guard = lock_gate_counters();
        crate::decode::reset_rect_split_lossless_chroma_offtile_hits();
        crate::decode::reset_census_unit_n();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned witness no longer decodes: {e}"));
        assert_eq!(frames.len(), 1, "{NAME}: frame count");
        assert_eq!(
            (frames[0].width, frames[0].height),
            (W, H),
            "{NAME}: dimensions"
        );

        // Non-vacuity: the clip must have run on this fixture, or the unit
        // count below is just "the walk happened to be short here" (class
        // gate-blind-to-feature).
        let clipped = crate::decode::rect_split_lossless_chroma_offtile_hits();
        assert!(
            clipped > 0,
            "{NAME}: no lossless rect chroma unit fell outside the chroma plane -- \
             the corrected walk never ran, so this gate is measuring the \
             unclipped one (class gate-blind-to-feature)"
        );

        // The oracle-anchored red-before/green-after: 2413 without the clip.
        let units = crate::decode::census_unit_n();
        assert_eq!(
            units, ORACLE_UNITS,
            "{NAME}: frame 0 walked {units} transform units, the oracle's own \
             EC_ECDUMP_IN census for these bytes is {ORACLE_UNITS}"
        );

        decode_all_frames_vs_oracle(&stream, NAME);
    }

    #[test]
    fn a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint() {
        const NAME: &str = "a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint";
        const F444: &str = "444_intrabc_rect4_witness.obu";
        const F444_LEN: usize = 1016;
        const F420: &str = "420_intrabc_rect4_witness.obu";
        const F420_LEN: usize = 890;
        let read = |name: &str, file: &str, len: usize| -> Vec<u8> {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("fixtures/{file}"));
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{name}: pinned fixture {file} missing: {e}"));
            assert_eq!(bytes.len(), len, "{name}: {file} length moved");
            bytes
        };
        let stream444 = read(NAME, F444, F444_LEN);
        let stream420 = read(NAME, F420, F420_LEN);
        // The defect only exists where `ss_size_lookup` != the 4:2:0 halving.
        assert_444_header(&stream444, NAME, 8);

        let _guard = lock_gate_counters();
        crate::decode::reset_ibc_rect_chroma_footprint_444_hits();
        let frames = decode_stream(&stream444)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned 4:4:4 witness was refused: {e}"));
        let hits = crate::decode::ibc_rect_chroma_footprint_444_hits();
        assert_eq!(frames.len(), 1, "{NAME}: frame count");
        assert!(
            hits > 0,
            "{NAME}: no rect intra-BC block was sized at 4:4:4 -- the corrected \
             footprint never ran, so this gate is measuring the hardcoded 4:2:0 \
             halving (class gate-blind-to-feature)"
        );

        // lane-av1oraclepost: main's shared probe. With
        // EC_AV1_REQUIRE_AOMDEC / EC_AV1_REQUIRE_AOMENC set it is a HARD
        // failure, so a batch run cannot report GREEN with the compare never
        // run -- ONE presence-check shape in the crate.
        if !aomdec_available(NAME) {
            eprintln!("{NAME}: {hits} rect intra-BC block(s) sized at 4:4:4; pixel arm SKIPPED");
            return;
        }

        // 4:2:0 identity FIRST and in full.
        decode_all_frames_vs_oracle(&stream420, &format!("{NAME}-420"));

        // 4:4:4 is a per-plane exact PREFIX, not an exactness claim, and the
        // doc above says why. Asserting "wrong by exactly N" would encode the
        // open entropy fork as expected behaviour; the floor is the honest form
        // and it only ever grows.
        const PLANE: usize = 640 * 480;
        const EXACT_PREFIX: [usize; 3] = [PLANE, 51360, 51360];
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let in_obu = dir.join("in.obu");
        std::fs::write(&in_obu, &stream444).expect("writing the stream");
        let aom_prefix = dir.join("aom");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "-o"])
            .arg(dir.join("out.y4m"))
            .arg(&in_obu)
            .env("EC_AV1_FINAL_DUMP", &aom_prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{NAME}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ours_prefix = dir.join("ours");
        set_final_dump_prefix(Some(ours_prefix.display().to_string()));
        let _ = decode_stream(&stream444).expect("second decode");
        set_final_dump_prefix(None);
        let aom_dump = std::fs::read(format!("{}.f0", aom_prefix.display())).expect("oracle dump");
        let our_dump = std::fs::read(format!("{}.f0", ours_prefix.display())).expect("our dump");
        assert_eq!(
            our_dump.len(),
            aom_dump.len(),
            "{NAME}: our frame is {} bytes, the oracle's is {}",
            our_dump.len(),
            aom_dump.len()
        );
        assert_eq!(aom_dump.len(), 3 * PLANE, "{NAME}: 8-bit 4:4:4 640x480");
        for (plane, name) in [(0usize, "Y"), (1, "U"), (2, "V")] {
            let at = plane * PLANE;
            let floor = EXACT_PREFIX[plane];
            if our_dump[at..at + floor] == aom_dump[at..at + floor] {
                continue;
            }
            let first = (0..floor)
                .find(|&s| our_dump[at + s] != aom_dump[at + s])
                .unwrap_or(floor);
            panic!(
                "{NAME}: plane {name} diverges from the oracle at sample {first} of {PLANE} \
                 (x={}, y={}) -- the exact prefix this gate pins is {floor} and it only \
                 grows; the chroma tail is the OPEN entropy fork in \
                 lanes/ibc444c.report.md r2",
                first % 640,
                first / 640
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!(
            "{NAME}: {hits} rect intra-BC block(s) sized at 4:4:4; 4:2:0 twin \
             byte-exact; 4:4:4 luma byte-exact (307200/307200), U and V byte-exact to \
             sample 51360 -- the chroma tail is the OPEN entropy fork \
             (lanes/ibc444c.report.md r2), NOT an assertion"
        );
    }

    /// lane-av1chromarect: the 128-root intra-BC HORZ/VERT strip's chroma
    /// plane block, on the 4:4:4 LOSSY witness `fixtures/r512.obu` that
    /// reaches `decode_intrabc_owned_rect` five times.
    ///
    /// The extent is the ss-derived one (`bw >> ss_x` by `bh >> ss_y`;
    /// `av1_get_max_uv_txsize` = `ss_size_lookup[bsize]`, blockd.h:1372), and
    /// the plane block is walked by `get_vartx_max_txsize` steps -- a 4:4:4
    /// 32x64 luma block's chroma plane block is 32x64, which libaom codes as
    /// TWO `TX_32X32` units, not as one 32x64 transform.
    ///
    /// This gate WAS the refusal assertion "a rectangular inter chroma
    /// transform unit whose shape has no coefficient table set here", put
    /// there one commit earlier so that the day the arm landed it would go
    /// RED and force this rewrite. The refusal string is GONE from the
    /// witness's decode because nothing reaches it any more, not because it
    /// was deleted: `rect_inter_chroma_set` still refuses (32, 64) and the
    /// string is still in the source, it is just not on this path any more.
    ///
    /// The floors were RAISED to full byte-exactness on all three planes of
    /// all three frames by lane-av1chromadc, which closed the "open chroma
    /// fork" this gate used to carry: a skipped 64x64 square block at 4:4:4
    /// has a 64x64 chroma plane block, libaom walks it as a 2x2 grid of
    /// `TX_32X32` units, and the intra-BC chroma override was handed to every
    /// unit whole instead of windowed per unit (see
    /// `a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit`
    /// and lanes/av1chromadc.report.md). The history of these floors:
    ///
    /// * lane-av1chromarect, after the ss-derived extent and the multi-unit
    ///   walk: `[[262144, 229728, 229728], [262144, 65704, 65704],
    ///   [262144, 262144, 262144]]` -- luma exact on all three frames, frame 2
    ///   exact on all three planes, 79969 chroma samples still wrong.
    /// * lane-av1chromadc: `[[262144; 3]; 3]` -- every plane of every frame
    ///   byte-exact against aomdec. Floors only ever grow.
    #[test]
    fn a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes() {
        const NAME: &str =
            "a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes";
        const FILE: &str = "r512.obu";
        const LEN: usize = 6948;
        /// 512x512 8-bit 4:4:4, one plane.
        const PLANE: usize = 512 * 512;
        /// Exact prefix per (frame, plane), decode order. FULL: every plane of
        /// every frame is byte-exact against the oracle.
        const EXACT_PREFIX: [[usize; 3]; 3] = [[PLANE; 3]; 3];
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/{FILE}"));
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: pinned fixture {FILE} missing: {e}"));
        assert_eq!(stream.len(), LEN, "{NAME}: {FILE} length moved");
        assert_444_header(&stream, NAME, 8);

        let _guard = lock_gate_counters();
        reset_ibc_owned_rect_chroma_footprint_444_hits();
        // The refusal this gate used to assert must NOT be what happens now.
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let obu = dir.join("in.obu");
        std::fs::write(&obu, &stream).expect("writing the stream");
        let aom_prefix = dir.join("aom");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "-o"])
            .arg(dir.join("out.y4m"))
            .arg(&obu)
            .env("EC_AV1_FINAL_DUMP", &aom_prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{NAME}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ours_prefix = dir.join("ours");
        set_final_dump_prefix(Some(ours_prefix.display().to_string()));
        let decoded = decode_stream(&stream);
        set_final_dump_prefix(None);
        if let Err(e) = decoded {
            panic!(
                "{NAME}: this decoder refused the witness: {e} -- the chroma plane block is \
                 sized ss-wide and walked in `get_vartx_max_txsize` units, so the 4:4:4 32x64 \
                 plane block is two TX_32X32 units, NOT the rect unit the refusal named"
            );
        }
        let hits = ibc_owned_rect_chroma_footprint_444_hits();
        assert_eq!(
            hits, 5,
            "{NAME}: {hits} 128-root intra-BC strip(s) sized at 4:4:4, not 5 -- the witness \
             stopped exercising the arm"
        );
        assert!(
            !std::fs::read(format!("{}.f0", ours_prefix.display()))
                .unwrap_or_default()
                .is_empty(),
            "{NAME}: our decoder wrote no frame"
        );
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for f in 0..3 {
            let aom_dump = std::fs::read(format!("{}.f{f}", aom_prefix.display()))
                .unwrap_or_else(|e| panic!("{NAME}: oracle dump f{f} missing: {e}"));
            let our_dump = std::fs::read(format!("{}.f{f}", ours_prefix.display()))
                .unwrap_or_else(|e| panic!("{NAME}: our dump f{f} missing: {e}"));
            assert_eq!(
                our_dump.len(),
                3 * PLANE,
                "{NAME}: frame {f} is not 8-bit 4:4:4 512x512"
            );
            assert_eq!(
                our_dump.len(),
                aom_dump.len(),
                "{NAME}: frame {f} length differs"
            );
            for (plane, name) in [(0usize, "Y"), (1, "U"), (2, "V")] {
                let at = plane * PLANE;
                let floor = EXACT_PREFIX[f][plane];
                if our_dump[at..at + floor] == aom_dump[at..at + floor] {
                    continue;
                }
                let first = (0..floor)
                    .find(|&s| our_dump[at + s] != aom_dump[at + s])
                    .unwrap_or(floor);
                panic!(
                    "{NAME}: frame {f} plane {name} diverges from the oracle at sample {first} \
                     of {PLANE} (x={}, y={}) -- the exact prefix this gate pins is {floor} and \
                     it only grows",
                    first % 512,
                    first / 512
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// lane-av1ibcfork: the SECOND 4:4:4 intra-BC witness, and the one the
    /// entropy investigation was chartered against. `444_intrabc_rect4_witness.obu`
    /// reaches `decode_intrabc_owned_rect`'s SKIP arm once -- the 64x32 block at
    /// mi (40,80) -- and is byte-exact on all three planes.
    ///
    /// Why a second witness and not a repeat of
    /// `a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes`
    /// (which pins `r512.obu`): the skip arm's defect was a wrong PLACE, not a
    /// wrong size, and the two shapes produce DIFFERENT symptoms. `r512.obu`'s
    /// 128-root strips fail as a sizing error inside the coefficient walk; this
    /// witness's 64x32 skip block read no coefficient at all, so the only
    /// symptom was pixels -- a 32x16 window committed at (px/2, py/2) =
    /// (160,80) over a region another block had already reconstructed, and its
    /// own 64x32 footprint at (320,160) left unwritten. Measured before the
    /// ss correction: U 72945 / V 64865 of 307200 wrong, first at (160,80) on
    /// both planes, luma byte-exact; the two 8x8-cell difference components
    /// were 8 cells at x[160,192) y[80,96) and 1182 cells over the whole
    /// x[320,640) y[160,480) quadrant -- one wrong-place write, not two.
    ///
    /// Full byte-exactness, not a prefix. That is possible only because
    /// `lane-av1chromarect` (the ss-derived extent and origins, this function
    /// included -- the skip arm was where the defect was first reached) and
    /// `lane-av1chromadc` (the per-unit windowed chroma override) both landed;
    /// before them this witness carried the open tail
    /// `lanes/ibc444c.report.md` r2 recorded. The 4:2:0 twin is asserted
    /// byte-exact in the same run, where `>> ss_x` IS `/ 2`.
    #[test]
    fn a_444_intrabc_rect4_witness_is_byte_exact_after_the_skip_arm_footprint() {
        const NAME: &str = "a_444_intrabc_rect4_witness_is_byte_exact_after_the_skip_arm_footprint";
        const F444: &str = "444_intrabc_rect4_witness.obu";
        const F444_LEN: usize = 1016;
        const F420: &str = "420_intrabc_rect4_witness.obu";
        const F420_LEN: usize = 890;
        const PLANE: usize = 640 * 480;
        let read = |file: &str, len: usize| -> Vec<u8> {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("fixtures/{file}"));
            let data =
                std::fs::read(&path).unwrap_or_else(|e| panic!("{NAME}: reading {file}: {e}"));
            assert_eq!(data.len(), len, "{NAME}: {file} is pinned at {len} bytes");
            data
        };
        let stream444 = read(F444, F444_LEN);
        let stream420 = read(F420, F420_LEN);
        assert_444_header(&stream444, NAME, 8);

        let _guard = lock_gate_counters();
        if !aomdec_available(NAME) {
            eprintln!("{NAME}: SKIP -- no aomdec; run scripts/build-aom-oracle.sh");
            return;
        }
        // 4:2:0 identity FIRST and in full: `>> ss_x` is `/ 2` at ss=1, so
        // this arm is the measurement, not a claim about it.
        decode_all_frames_vs_oracle(&stream420, &format!("{NAME}-420"));

        crate::decode::reset_ibc_owned_rect_chroma_footprint_444_hits();
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let in_obu = dir.join("in.obu");
        std::fs::write(&in_obu, &stream444).expect("writing the stream");
        let aom_prefix = dir.join("aom");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "-o"])
            .arg(dir.join("out.y4m"))
            .arg(&in_obu)
            .env("EC_AV1_FINAL_DUMP", &aom_prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{NAME}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ours_prefix = dir.join("ours");
        set_final_dump_prefix(Some(ours_prefix.display().to_string()));
        let _ = decode_stream(&stream444).expect("second decode");
        set_final_dump_prefix(None);
        let hits = crate::decode::ibc_owned_rect_chroma_footprint_444_hits();
        assert!(
            hits > 0,
            "{NAME}: no rect intra-BC block committed a 4:4:4 chroma plane block -- the \
             ss-derived footprint never ran, so this gate is measuring a hardcoded halving \
             (class gate-blind-to-feature)"
        );
        let aom_dump = std::fs::read(format!("{}.f0", aom_prefix.display())).expect("oracle dump");
        let our_dump = std::fs::read(format!("{}.f0", ours_prefix.display())).expect("our dump");
        assert_eq!(aom_dump.len(), 3 * PLANE, "{NAME}: 8-bit 4:4:4 640x480");
        assert_eq!(
            our_dump.len(),
            aom_dump.len(),
            "{NAME}: our frame is {} bytes, the oracle's is {}",
            our_dump.len(),
            aom_dump.len()
        );
        for (plane, name) in [(0usize, "Y"), (1, "U"), (2, "V")] {
            let at = plane * PLANE;
            if our_dump[at..at + PLANE] == aom_dump[at..at + PLANE] {
                continue;
            }
            let first = (0..PLANE)
                .find(|&s| our_dump[at + s] != aom_dump[at + s])
                .unwrap_or(PLANE);
            panic!(
                "{NAME}: plane {name} diverges from the oracle at sample {first} of {PLANE} \
                 (x={}, y={}) -- this gate pins FULL byte-exactness on all three planes",
                first % 640,
                first / 640
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!(
            "{NAME}: {hits} rect intra-BC block(s) committed a 4:4:4 chroma plane block; \
             4:2:0 twin byte-exact; 4:4:4 byte-exact on all three planes \
             (307200 samples each)"
        );
    }

    /// lane-av1chromadc: a SKIPPED 64x64 square block at 4:4:4 has a 64x64
    /// chroma plane block, which libaom walks as a 2x2 grid of `TX_32X32` units
    /// (`get_vartx_max_txsize`: `av1_get_adjusted_tx_size(TX_64X64) =
    /// TX_32X32`). The intra-BC / UV-palette chroma override
    /// ([`decode_block`]'s `intrabc_bufs` / `palette_uv_bufs`, the
    /// `PALETTE_PRED` slot) is a whole-`chroma_side x chroma_height` buffer and
    /// must be WINDOWED per unit: `PlaneBuf::reconstruct` uses the buffer it
    /// takes as the unit's own `side x side` prediction. At 4:2:0 the chroma
    /// plane block is one 32x32 unit, so the whole buffer IS that unit's
    /// window and the bug is invisible -- which is why it survived every 4:2:0
    /// gate. At 4:4:4 every unit was handed the buffer's FIRST 32x32, so the
    /// block's right and bottom chroma columns were a copy of its top-left
    /// unit.
    ///
    /// The witness: `fixtures/r512.obu`, 4:4:4 8-bit 512x512, three frames. The
    /// seed is frame 0, `mi=(112,80)`, `px=(320,448)`, `side=64`, `skip=1`,
    /// `mode=0`, `uv_mode=0` -- a skipped intra-BC block at the frame's bottom
    /// edge. Its chroma unit `cu=(1,0)` at `px=(352,448)` reconstructs a flat
    /// 241 (U) / 110 (V) where the oracle has 202 / 222, because it read the
    /// same override window as `cu=(0,0)`.
    ///
    /// This gate asserts FULL frame exactness on all three planes of all three
    /// frames. It is the red-before: on the unpatched tree it fails at
    /// `(352,448)` of frame 0, which is the seed's second chroma unit.
    #[test]
    fn a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit() {
        const NAME: &str = "a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit";
        const FILE: &str = "r512.obu";
        const LEN: usize = 6948;
        const PLANE: usize = 512 * 512;
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/{FILE}"));
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: pinned fixture {FILE} missing: {e}"));
        assert_eq!(stream.len(), LEN, "{NAME}: {FILE} length moved");
        assert_444_header(&stream, NAME, 8);

        let _guard = lock_gate_counters();
        reset_skip_chroma_override_window_hits();
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let obu = dir.join("in.obu");
        std::fs::write(&obu, &stream).expect("writing the stream");
        let aom_prefix = dir.join("aom");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "-o"])
            .arg(dir.join("out.y4m"))
            .arg(&obu)
            .env("EC_AV1_FINAL_DUMP", &aom_prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{NAME}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ours_prefix = dir.join("ours");
        set_final_dump_prefix(Some(ours_prefix.display().to_string()));
        if let Err(e) = decode_stream(&stream) {
            panic!("{NAME}: this decoder refused the witness: {e}");
        }
        set_final_dump_prefix(None);
        // Non-vacuity: the windowed arm must have run, or the exactness below
        // is measuring a witness that never reached the 2x2 chroma walk.
        let windows = skip_chroma_override_window_hits();
        assert!(
            windows >= 8,
            "{NAME}: {windows} windowed chroma override unit(s) on this witness, expected at \
             least 8 (two skipped 64x64 intra-BC blocks x 2x2 units x 2 planes) -- the witness \
             stopped exercising the arm, so the exactness below is vacuous"
        );
        for f in 0..3 {
            let aom_dump = std::fs::read(format!("{}.f{f}", aom_prefix.display()))
                .unwrap_or_else(|e| panic!("{NAME}: oracle dump f{f} missing: {e}"));
            let our_dump = std::fs::read(format!("{}.f{f}", ours_prefix.display()))
                .unwrap_or_else(|e| panic!("{NAME}: our dump f{f} missing: {e}"));
            assert_eq!(
                our_dump.len(),
                3 * PLANE,
                "{NAME}: frame {f} is not 8-bit 4:4:4 512x512"
            );
            for (plane, name) in [(0usize, "Y"), (1, "U"), (2, "V")] {
                let at = plane * PLANE;
                if our_dump[at..at + PLANE] == aom_dump[at..at + PLANE] {
                    continue;
                }
                let first = (0..PLANE)
                    .find(|&s| our_dump[at + s] != aom_dump[at + s])
                    .unwrap_or(PLANE);
                let wrong = (0..PLANE)
                    .filter(|&s| our_dump[at + s] != aom_dump[at + s])
                    .count();
                panic!(
                    "{NAME}: decode-order frame {f} plane {name} is byte-exact nowhere -- first \
                     divergence at sample {first} of {PLANE} (x={}, y={}), {wrong} samples \
                     wrong. The seed is frame 0, mi=(112,80) px=(320,448) side=64 skip=1, whose \
                     chroma unit cu=(1,0) at px=(352,448) must be the SECOND 32x32 window of \
                     the block's chroma override and not a copy of cu=(0,0)'s.",
                    first % 512,
                    first / 512
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// lane-av1rectreach, the 2x1 / 1x2 half of the class sweep
    /// `a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit`
    /// opened: `decode_rect_split`'s `chroma_tiled` walk is the THIRD site that
    /// walks a 4:4:4 chroma plane block as a grid of Chroma32 units, and the
    /// only one that handed every unit the BLOCK's reach.
    ///
    /// The other two are already per-unit and are not touched by this gate:
    /// `decode_block`'s skip arm (`chroma_window`, lane-av1chromadc) and
    /// `decode_block`'s coded arm (its own `tu_reach_rect`) both resolve
    /// `has_top_right` per transform unit, the way libaom's
    /// `av1_build_intra_predictors` does
    /// (`has_top_right`, defined reconintra.c:196, called from
    /// `av1_build_intra_predictors` at reconintra.c:1847),
    /// fed the chroma txb walk's own `blk_col`/`blk_row`).
    ///
    /// Witness `fixtures/r512_rect2x1_1x2_444.obu`, 2697 B,
    /// sha256 `a009ec580a1ff24250be53e11b8d09616da71e095cefd5eb621c941d40a8688b`:
    ///
    /// ```text
    /// ffmpeg -f lavfi -i testsrc2=size=256x256:rate=1:duration=1 -pix_fmt yuv444p \
    ///   -f rawvideo in.yuv
    /// aomenc --codec=av1 -w 256 -h 256 --i444 --sb-size=64 --enable-palette=1 \
    ///   --enable-rect-partitions=1 --enable-1to4-partitions=1 \
    ///   --min-partition-size=32 --cq-level=40 --profile=1 --obu --limit=1 \
    ///   -o r512_rect2x1_1x2_444.obu in.yuv
    /// ```
    ///
    /// (aom-affine aomenc, one key frame, 4:4:4 8-bit 256x256.) It reaches
    /// BOTH grid shapes: a 64x32 chroma plane block as `nw=2 nh=1` of 32x32
    /// and a 32x64 one as `nw=1 nh=2`.
    ///
    /// RED-BEFORE, on the unpatched tree, measured with `EC_AV1_FINAL_DUMP`
    /// (aomdec) against `set_final_dump_prefix` (ours), 786432 B per frame:
    /// frame 0 plane Y 0 wrong, plane U 130 wrong, plane V 30 wrong -- all of
    /// them in the right 6 columns of the `cu=(0,0)` unit at `mi=(48,48)`
    /// `px=(192,192)`, a 64x32 block whose right edge IS the frame's, so the
    /// block-level reach denied an above-right that libaom's per-unit
    /// `col_off + top_right_count_unit < plane_bw_unit` (0 + 8 < 16) grants.
    /// After: 0 wrong on all three planes.
    #[test]
    fn a_444_rect_strip_chroma_tiled_2x1_and_1x2_units_take_their_own_above_right_reach() {
        const NAME: &str =
            "a_444_rect_strip_chroma_tiled_2x1_and_1x2_units_take_their_own_above_right_reach";
        const FILE: &str = "r512_rect2x1_1x2_444.obu";
        const LEN: usize = 2697;
        const SHA: &str = "a009ec580a1ff24250be53e11b8d09616da71e095cefd5eb621c941d40a8688b";
        const W: usize = 256;
        const PLANE: usize = W * W;
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/{FILE}"));
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: pinned fixture {FILE} missing: {e}"));
        assert_eq!(stream.len(), LEN, "{NAME}: {FILE} length moved");
        assert_444_header(&stream, NAME, 8);
        // The byte pin is fnv1a64, the same load-bearing-identity hash every
        // other pinned gate here uses; the sha256 in this test's comment is
        // the provenance half. A silent stream swap under the same name
        // cannot stay green.
        assert_eq!(
            fnv1a64(&stream),
            0x2751ccaa079cc422,
            "{NAME}: {FILE} bytes drifted from the fnv1a64 pin above (provenance sha256 \
             {SHA}) -- regenerate the stream with the recipe in this test's comment and \
             re-pin the length, the fnv1a64 and the sha256 together"
        );

        let _guard = lock_gate_counters();
        reset_rect_tiled_chroma_grid_hits();
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let obu = dir.join("in.obu");
        std::fs::write(&obu, &stream).expect("writing the stream");
        let aom_prefix = dir.join("aom");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "-o"])
            .arg(dir.join("out.y4m"))
            .arg(&obu)
            .env("EC_AV1_FINAL_DUMP", &aom_prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{NAME}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ours_prefix = dir.join("ours");
        set_final_dump_prefix(Some(ours_prefix.display().to_string()));
        if let Err(e) = decode_stream(&stream) {
            panic!("{NAME}: this decoder refused the witness: {e}");
        }
        set_final_dump_prefix(None);

        // Non-vacuity, in the order the claim depends on it: the walk must
        // have reached a 2x1 AND a 1x2 grid, and at least one unit must have
        // a per-unit reach that DIFFERS from the block-level one. Without the
        // last one the two answers would be interchangeable everywhere and
        // the exactness below would be measuring a witness the fix cannot
        // touch. (It does not check the argument identity -- see
        // `RECT_TILED_CHROMA_UNIT_REACH_HITS`; the exactness assert below is
        // what reds when the substitution is deleted.)
        let (reach_hits, h2x1, h1x2, h_nxn) = rect_tiled_chroma_grid_hits();
        assert!(
            h2x1 >= 4 && h1x2 >= 4,
            "{NAME}: the walk reached {h2x1} unit(s) in a 2x1 grid and {h1x2} in a 1x2 grid \
             (other multi-unit shapes: {h_nxn}), expected at least 4 of each -- the witness \
             stopped exercising one of the two grid shapes, so half the claim is vacuous"
        );
        assert!(
            reach_hits >= 1,
            "{NAME}: {reach_hits} chroma unit(s) had a per-unit reach differing from the \
             block-level one, expected at least 1 -- on the unpatched tree every unit took \
             the block's reach, so the exactness below is vacuous"
        );

        let aom_dump = std::fs::read(format!("{}.f0", aom_prefix.display()))
            .unwrap_or_else(|e| panic!("{NAME}: oracle dump f0 missing: {e}"));
        let our_dump = std::fs::read(format!("{}.f0", ours_prefix.display()))
            .unwrap_or_else(|e| panic!("{NAME}: our dump f0 missing: {e}"));
        assert_eq!(
            our_dump.len(),
            3 * PLANE,
            "{NAME}: frame 0 is not 8-bit 4:4:4 256x256"
        );
        for (plane, pname) in [(0usize, "Y"), (1, "U"), (2, "V")] {
            let at = plane * PLANE;
            if our_dump[at..at + PLANE] == aom_dump[at..at + PLANE] {
                continue;
            }
            let first = (0..PLANE)
                .find(|&s| our_dump[at + s] != aom_dump[at + s])
                .unwrap_or(PLANE);
            let wrong = (0..PLANE)
                .filter(|&s| our_dump[at + s] != aom_dump[at + s])
                .count();
            panic!(
                "{NAME}: frame 0 plane {pname} is byte-exact nowhere -- first divergence at \
                 sample {first} of {PLANE} (x={}, y={}), {wrong} samples wrong. Each unit of \
                 this walk must resolve `has_top_right` over ITS OWN extent \
                 (reconintra.c:196/1847), not the block's: the seed unit is cu=(0,0) of \
                 64x32 chroma grid at mi=(48,48) px=(192,192), whose right 6 columns are the \
                 ones the truncated above row moved.",
                first % W,
                first / W
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The UV-PALETTE half of the same class, and the cell
    /// `lanes/av1chromadc.report.md` §5 left explicitly UNMEASURED ("every
    /// multi-unit walk on this witness is `src=intrabc`; the UV-palette source
    /// never reaches a multi-unit chroma walk here, so its arm of the sweep is
    /// argued from the shared code path, not witnessed"). This witness is the
    /// measurement: it puts a UV-palette block on a 1x2 16x64 chroma grid
    /// (`mi=(32,12)`) as well as plenty of non-palette ones.
    ///
    /// Witness `fixtures/r512_rect1x2_444_palette.obu`, 2250 B,
    /// sha256 `fad03c69087f316c1a117e8d8a70769d161b332bce7b2998e646312bb78fb067`:
    ///
    /// ```text
    /// ffmpeg -f lavfi -i testsrc2=size=256x256:rate=1:duration=1 -pix_fmt yuv444p \
    ///   -f rawvideo in.yuv
    /// aomenc --codec=av1 -w 256 -h 256 --i444 --sb-size=64 --enable-palette=1 \
    ///   --enable-rect-partitions=1 --enable-1to4-partitions=1 \
    ///   --min-partition-size=16 --cq-level=40 --profile=1 --obu --limit=1 \
    ///   -o r512_rect1x2_444_palette.obu in.yuv
    /// ```
    ///
    /// (`--min-partition-size=16` is what puts the 16x64 chroma grid on a
    /// strip at all; at 32 the encoder only ever picks 32x64 / 64x32.) The
    /// per-unit reach the fix installs is the same line for the palette and
    /// the non-palette unit -- the palette only supplies the PREDICTION
    /// (`palette_window`, decode.rs:12784), never the reach -- so this gate
    /// exists to pin the prediction source down, not to re-prove the reach.
    #[test]
    fn a_444_rect_strip_chroma_tiled_1x2_palette_grid_decodes_byte_exact() {
        const NAME: &str = "a_444_rect_strip_chroma_tiled_1x2_palette_grid_decodes_byte_exact";
        const FILE: &str = "r512_rect1x2_444_palette.obu";
        const LEN: usize = 2250;
        const SHA: &str = "fad03c69087f316c1a117e8d8a70769d161b332bce7b2998e646312bb78fb067";
        const W: usize = 256;
        const PLANE: usize = W * W;
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("fixtures/{FILE}"));
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: pinned fixture {FILE} missing: {e}"));
        assert_eq!(stream.len(), LEN, "{NAME}: {FILE} length moved");
        assert_444_header(&stream, NAME, 8);
        assert_eq!(
            fnv1a64(&stream),
            0x7759753a0d3f75a2,
            "{NAME}: {FILE} bytes drifted from the fnv1a64 pin above (provenance sha256 \
             {SHA}) -- regenerate the stream with the recipe in this test's comment and \
             re-pin the length, the fnv1a64 and the sha256 together"
        );

        let _guard = lock_gate_counters();
        reset_rect_tiled_chroma_grid_hits();
        let dir = std::env::temp_dir().join(format!("ec-av1-{NAME}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let obu = dir.join("in.obu");
        std::fs::write(&obu, &stream).expect("writing the stream");
        let aom_prefix = dir.join("aom");
        let out = crate::tool_cache::cached_output_io(Command::new(aomdec_path())
            .args(["--codec=av1", "-o"])
            .arg(dir.join("out.y4m"))
            .arg(&obu)
            .env("EC_AV1_FINAL_DUMP", &aom_prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()), None, |c| c.output())
            .expect("aomdec failed to run");
        assert!(
            out.status.success(),
            "{NAME}: the oracle aomdec refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ours_prefix = dir.join("ours");
        set_final_dump_prefix(Some(ours_prefix.display().to_string()));
        if let Err(e) = decode_stream(&stream) {
            panic!("{NAME}: this decoder refused the witness: {e}");
        }
        set_final_dump_prefix(None);

        let (_reach_hits, h2x1, h1x2, _h_nxn) = rect_tiled_chroma_grid_hits();
        assert!(
            h1x2 >= 4,
            "{NAME}: the walk reached {h1x2} unit(s) in a 1x2 grid ({h2x1} in a 2x1), expected \
             at least 4 of the 1x2 -- the witness stopped exercising the arm"
        );

        let aom_dump = std::fs::read(format!("{}.f0", aom_prefix.display()))
            .unwrap_or_else(|e| panic!("{NAME}: oracle dump f0 missing: {e}"));
        let our_dump = std::fs::read(format!("{}.f0", ours_prefix.display()))
            .unwrap_or_else(|e| panic!("{NAME}: our dump f0 missing: {e}"));
        assert_eq!(
            our_dump.len(),
            3 * PLANE,
            "{NAME}: frame 0 is not 8-bit 4:4:4 256x256"
        );
        for (plane, pname) in [(0usize, "Y"), (1, "U"), (2, "V")] {
            let at = plane * PLANE;
            if our_dump[at..at + PLANE] == aom_dump[at..at + PLANE] {
                continue;
            }
            let first = (0..PLANE)
                .find(|&s| our_dump[at + s] != aom_dump[at + s])
                .unwrap_or(PLANE);
            let wrong = (0..PLANE)
                .filter(|&s| our_dump[at + s] != aom_dump[at + s])
                .count();
            panic!(
                "{NAME}: frame 0 plane {pname} is byte-exact nowhere -- first divergence at \
                 sample {first} of {PLANE} (x={}, y={}), {wrong} samples wrong.",
                first % W,
                first / W
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
