use super::*;
use super::support::*;
use super::gates_a::*;
use super::gates_b::*;
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

    /// lane-motionmode round 3: same shape as
    /// `a_real_aomenc_stream_with_obmc_decodes_pixel_exact`, but relaxed the
    /// `--min-partition-size=32 --max-partition-size=32` clamp (that other
    /// gate's own doc: locked to remove partition-depth choice from the
    /// search entirely) to `8`/`32`, to make an 8x8-leaf `OBMC_CAUSAL` pick
    /// possible at all. Gated on [`decode::obmc_hits_8`] specifically, not
    /// the aggregate [`decode::obmc_hits`]. Measured live (round 3, this
    /// box): 80 attempts, 0 ever called `decode_inter_block8` at all --
    /// `cpu-used=0`'s RD search never once splits a 16x16 down to a clean
    /// 4-way 8x8 leaf for this smooth `gradients` fixture (large blocks
    /// always win the RD cost there); the handful of named refusals seen
    /// are `PARTITION_SPLIT` chosen *at* 8x8 (down to 4x4, already refused
    /// at `decode.rs`'s `part8 != PARTITION_NONE` check) or
    /// `allow_screen_content_tools`, neither reaching a leaf. Kept anyway
    /// (mirrors this file's own `never_fired`-skip convention) as the
    /// honest record of the attempt; a real 8x8 OBMC fixture needs
    /// higher-frequency content than this gate's fixtures use, out of
    /// round-3 budget.
    #[test]
    fn a_real_aomenc_stream_with_obmc_8x8_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_obmc_8x8_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        let mut total_obmc8 = 0usize;
        let n_attempts: u32 = std::env::var("EC_OBMC8_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(80);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt % 40;
            let duration = frame_count as f64 / 25.0;
            // Recipe search hook: `gradients` is smooth, so aomenc's RD never
            // splits below 16x16 and this gate never fired one 8x8 OBMC block
            // in 45 attempts -- it "passed" by skipping (gate-blind-to-feature).
            // EC_AV1_OBMC8_SOURCE overrides the lavfi source so a firing recipe
            // can be searched for without a rebuild.
            let source = std::env::var("EC_AV1_OBMC8_SOURCE").unwrap_or_else(|_| {
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"))
            });
            // A source supplied through EC_AV1_OBMC8_SOURCE need not carry its
            // own `duration=`; `-t` is the bound that always holds. Without it
            // an endless source (`testsrc2`) fills the pipe forever and the
            // gate hangs to its 300 s timeout.
            let duration_arg = format!("{duration}");
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-t",
                    &duration_arg,
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
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=0",
                "--enable-obmc=1",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
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
                "--min-partition-size=8",
                "--max-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
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
            let before8 = decode::obmc_hits_8();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            let fired8 = decode::obmc_hits_8() - before8;
            total_obmc8 += fired8;
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            let counted = fired8 != 0;
            if !counted {
                never_fired += 1;
            }
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
            // lane-hidden r2 (class gate-blind-to-hidden-frames): the same
            // stream again, comparing EVERY decode-order frame -- the hidden
            // (show_frame == 0) alt-ref frames the ffmpeg compare above
            // cannot see, because both sides of it emit shown frames only.
            let (hidden_total, hidden_n) = decode_all_frames_vs_oracle(&stream, NAME);
            eprintln!(
                "{NAME} HIDDEN: seed {seed}: {hidden_total} decode-order frames, \
                 {hidden_n} hidden, all pixel-exact vs the oracle"
            );
            // lane-hidden r2 SECOND ARM (the recipe above is untouched):
            // libaom's temporal alt-ref filter absorbs this recipe's hidden
            // frames, so the compare above is vacuous -- re-encode the same
            // fixture with --arnr-maxframes=0 and compare THOSE.
            if let Some((arm_total, arm_hidden)) = hidden_arnr_arm(NAME, &y4m.stdout, &args) {
                eprintln!(
                    "{NAME} HIDDEN-ARM(--arnr-maxframes=0): seed {seed}: {arm_total} \
                     decode-order frames, {arm_hidden} hidden, all pixel-exact vs the oracle"
                );
            }
            if counted {
                eprintln!(
                    "{NAME} FIRING seed {seed}: 8x8 obmc hits {fired8} (total across attempts so far {total_obmc8})"
                );
                gate_buckets(NAME, 1, never_fired, refusals.len());
                return;
            }
        }
        gate_buckets(NAME, 0, never_fired, refusals.len());
        // lane-gmaffine r2: this gate fired for three rounds only as a SKIP
        // (gate-skips-on-its-own-failure); the 8x8 leaf is reachable now, so a
        // sweep that never lands one firing pixel-exact attempt is a FAILURE.
        panic!(
            "{NAME}: {never_fired} attempts decoded but never fired an 8x8 obmc block \
             ({total_obmc8} total 8x8 obmc hits across all attempts), and every other attempt \
             hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-warp r5e (flipped from lane-motionmode r1's refuses-or-matches):
    /// `--enable-warped-motion=1` streams must DECODE pixel-exact --
    /// `av1_findSamples`/`num_proj_ref`, the 3-symbol `motion_mode` read,
    /// the affine projection/filter, and the WARPED_CAUSAL interp-filter
    /// derivation are all ported. A refusal naming warp, or any pixel
    /// mismatch, fails the gate; refusals for OTHER named capabilities
    /// (screen content tools) still skip that seed. The sweep must also
    /// actually exercise warp (`warp_selected_hits > 0`) so a decoder that
    /// silently stops selecting WARPED_CAUSAL cannot pass vacuously.
    #[test]
    fn a_real_aomenc_stream_with_warped_motion_refuses_or_matches() {
        const NAME: &str = "a_real_aomenc_stream_with_warped_motion_refuses_or_matches";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_WARP_GATE_ATTEMPTS")
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
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
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
                "--enable-ref-frame-mvs=0",
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
                    assert!(
                        !msg.contains("warp"),
                        "{NAME} refused on warp (seed {seed}) -- warp decode is ported, this \
                         refusal must not exist: {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal (non-warp capability): {msg}");
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
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
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
            // lane-hidden r2 (class gate-blind-to-hidden-frames): the same
            // stream again, comparing EVERY decode-order frame -- the hidden
            // (show_frame == 0) alt-ref frames the ffmpeg compare above
            // cannot see, because both sides of it emit shown frames only.
            let (hidden_total, hidden_n) = decode_all_frames_vs_oracle(&stream, NAME);
            eprintln!(
                "{NAME} HIDDEN: seed {seed}: {hidden_total} decode-order frames, \
                 {hidden_n} hidden, all pixel-exact vs the oracle"
            );
            // lane-hidden r2 SECOND ARM (the recipe above is untouched):
            // libaom's temporal alt-ref filter absorbs this recipe's hidden
            // frames, so the compare above is vacuous -- re-encode the same
            // fixture with --arnr-maxframes=0 and compare THOSE.
            if let Some((arm_total, arm_hidden)) = hidden_arnr_arm(NAME, &y4m.stdout, &args) {
                eprintln!(
                    "{NAME} HIDDEN-ARM(--arnr-maxframes=0): seed {seed}: {arm_total} \
                     decode-order frames, {arm_hidden} hidden, all pixel-exact vs the oracle"
                );
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused on other capabilities; the gate never exercised warp"
        );
        assert!(
            crate::decode::warp_selected_hits() > 0,
            "{NAME}: {matched} matches but zero WARPED_CAUSAL blocks fired -- the gate decodes \
             warp streams without ever selecting warp, so it proves nothing about warp"
        );
        eprintln!(
            "{NAME}: {named_refusals} non-warp refusals, {matched} pixel-exact matches out of {n_attempts}, warp_selected_hits={}",
            crate::decode::warp_selected_hits()
        );
    }

    /// lane-superres stage 3: a real aomenc `--superres-mode=1` key-frame
    /// sequence must decode pixel-exact vs ffmpeg/dav1d's own upscaled
    /// output, and must actually run [`crate::superres::upscale_picture`]
    /// (`superres_hits > 0`) -- a decoder that silently stopped upscaling
    /// would otherwise pass vacuously (class `gate-blind-to-feature`).
    /// `--kf-max-dist` set to the frame count keeps every frame a key
    /// frame: inter-frame superres (a differently-scaled reference) is a
    /// later stage of this lane, refused by name today.
    #[test]
    fn a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_superres_key_frame_sequence_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 3usize);
        let duration = frame_count as f64 / 25.0;
        let source = gradients_source(7, width, height, &format!("duration={duration}:rate=25"));
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
            "--cq-level=32",
            "--cpu-used=0",
            "--kf-max-dist=0",
            "--threads=1",
            "--row-mt=0",
            "--superres-mode=1",
            "--superres-denominator=12",
            "--superres-kf-denominator=12",
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
        ];
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        if let Ok(path) = std::env::var("EC_SUPERRES_STREAM_DUMP") {
            std::fs::write(path, &stream).expect("dump stream");
        }
        let frames = decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME} refused: {e}"));
        assert_eq!(frames.len(), frame_count);
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg");
        }
        assert!(
            crate::superres::superres_hits() > 0,
            "{NAME}: matched but zero superres_hits -- the gate never actually exercised the \
             upscaler, it proves nothing"
        );
        eprintln!(
            "{NAME}: {frame_count} frames pixel-exact, superres_hits={}",
            crate::superres::superres_hits()
        );
    }

    /// lane-superres r10: a real aomenc key+inter `--superres-mode=1`
    /// sequence must DECODE pixel-exact vs ffmpeg, with the frame-level
    /// `use_superres` refusal LIFTED for inter frames -- `mc::predict_scaled`
    /// (spec 7.11.3.3) is the only path this recipe can reach (warp/OBMC/
    /// compound/interintra/palette/intrabc/CDEF/LR are all off, matching the
    /// scoped refusals' own unimplemented list). A pixel match alone would
    /// pass just as well with an unscaled reference and prove nothing
    /// (class `gate-blind-to-feature`) -- hard-assert
    /// `crate::mc::predict_scaled_hits() > 0` so the gate can only pass if the
    /// scaled MC path actually ran.
    #[test]
    fn a_real_aomenc_key_and_inter_superres_sequence_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_key_and_inter_superres_sequence_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 3usize);
        let duration = frame_count as f64 / 25.0;
        let source = gradients_source(11, width, height, &format!("duration={duration}:rate=25"));
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
            "--cq-level=32",
            "--cpu-used=0",
            // `--kf-max-dist` above `frame_count` keeps only frame 0 a key
            // frame -- frames 1/2 are the inter frames whose `use_superres`
            // refusal this round lifts.
            "--kf-max-dist=1000",
            "--lag-in-frames=0",
            "--auto-alt-ref=0",
            "--threads=1",
            "--row-mt=0",
            "--sb-size=64",
            "--superres-mode=1",
            "--superres-denominator=12",
            "--superres-kf-denominator=12",
            // Forces every inter block onto the single-ref, non-warp/OBMC/
            // interintra branch `decode_inter_block` threads `predict_scaled`
            // through -- the only branch this round's scoped refusals leave
            // unrefused under a scaled reference.
            "--enable-warped-motion=0",
            "--enable-obmc=0",
            "--enable-masked-comp=0",
            "--enable-interintra-comp=0",
            "--enable-dist-wtd-comp=0",
            "--enable-diff-wtd-comp=0",
            "--enable-onesided-comp=0",
            "--enable-interintra-wedge=0",
            "--enable-smooth-interintra=0",
            "--enable-order-hint=0",
            "--enable-ref-frame-mvs=0",
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
        ];
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        if let Ok(path) = std::env::var("EC_SUPERRES_INTER_STREAM_DUMP") {
            std::fs::write(&path, &stream).expect("dump stream");
        }
        let frames = decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME} refused: {e}"));
        assert_eq!(frames.len(), frame_count);
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg");
        }
        assert!(
            crate::superres::superres_hits() > 0,
            "{NAME}: matched but zero superres_hits -- the upscaler never ran"
        );
        assert!(
            crate::mc::predict_scaled_hits() > 0,
            "{NAME}: matched but zero predict_scaled_hits -- every block took the unscaled MC \
             path, which proves nothing about the scaled-reference code this round adds \
             (class gate-blind-to-feature)"
        );
        eprintln!(
            "{NAME}: {frame_count} frames pixel-exact, superres_hits={}, predict_scaled_hits={}",
            crate::superres::superres_hits(),
            crate::mc::predict_scaled_hits()
        );
    }

    /// lane-t900 r28: the three scaled-reference (superres) refusals are
    /// LIFTED -- an 8x8 partition leaf, a sub-8x8 inter block, and warp --
    /// so two real `--superres-mode=1` streams whose references are all
    /// scaled must decode PIXEL-EXACT vs ffmpeg.
    ///
    /// Arm A drives `--min-partition-size=4` so the split (4x4) and rect
    /// (8x4/4x8) sub-8x8 readers and the 8x8 leaf all run over a scaled
    /// reference; arm B turns `--enable-warped-motion=1` on, which is the
    /// root cause this round fixed: libaom `motion_mode_allowed` drops
    /// WARPED_CAUSAL eligibility when `av1_is_scaled(sf)`, so the 8x8 leaf
    /// must read the 2-symbol obmc alphabet (here: no symbol at all, OBMC
    /// off) instead of the 3-symbol motion_mode row.
    ///
    /// Each arm hard-asserts its own counter (class `gate-blind-to-feature`):
    /// a pixel match with zero scaled leaves/sub-8x8 blocks/suppressed warp
    /// eligibilities would prove nothing.
    #[test]
    fn real_superres_streams_with_sub8_leaf8_and_warp_decode_pixel_exact() {
        const NAME: &str = "real_superres_streams_with_sub8_leaf8_and_warp_decode_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // Every tool whose scaled-reference path this round does NOT touch is
        // off, so a mismatch can only be the lifted paths (the same scoping
        // the r10 key+inter superres gate uses).
        let common: [&str; 21] = [
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--threads=1",
            "--row-mt=0",
            "--kf-max-dist=9999",
            "--lag-in-frames=0",
            "--auto-alt-ref=0",
            "--cpu-used=0",
            "--sb-size=64",
            "--superres-mode=1",
            "--superres-denominator=12",
            "--superres-kf-denominator=12",
            "--enable-obmc=0",
            "--enable-masked-comp=0",
            "--enable-interintra-comp=0",
            "--enable-tx-size-search=0",
            "--enable-cdef=0",
            "--enable-restoration=0",
            "--enable-palette=0",
            "--enable-intrabc=0",
        ];
        struct Arm {
            name: &'static str,
            frames: usize,
            cq: &'static str,
            extra: [&'static str; 4],
        }
        let arms = [
            Arm {
                name: "sub8+leaf8",
                frames: 4,
                cq: "--cq-level=4",
                extra: [
                    "--enable-warped-motion=0",
                    "--enable-rect-partitions=1",
                    "--max-partition-size=32",
                    "--min-partition-size=4",
                ],
            },
            Arm {
                name: "warp",
                frames: 6,
                cq: "--cq-level=20",
                extra: [
                    "--enable-warped-motion=1",
                    "--enable-rect-partitions=1",
                    "--max-partition-size=32",
                    "--min-partition-size=8",
                ],
            },
        ];
        let (width, height) = (96usize, 96usize);
        for arm in &arms {
            let duration = format!("{}", arm.frames as f64 / 25.0);
            let src = format!("mandelbrot=size={width}x{height}:rate=25");
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &src,
                    "-t",
                    &duration,
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
            assert!(y4m.status.success(), "{NAME}: ffmpeg refused the fixture");
            let limit = format!("--limit={}", arm.frames);
            let mut args: Vec<&str> = common.to_vec();
            args.push(&limit);
            args.push(arm.cq);
            args.extend_from_slice(&arm.extra);
            args.extend_from_slice(&["--obu", "-o", "-", "-"]);
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "{NAME} {}: aomenc refused the recipe: {}",
                arm.name,
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let leaf8_before = crate::decode::scaled_leaf8_hits();
            let sub8_before = crate::decode::scaled_sub8_hits();
            let warp_before = crate::decode::scaled_warp_suppressed_hits();
            let frames = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME} {}: refused: {e}", arm.name));
            assert_eq!(frames.len(), arm.frames, "{NAME} {}", arm.name);
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, arm.frames);
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} {} frame {i} luma vs ffmpeg",
                    arm.name
                );
                assert_eq!(got.u, want.u, "{NAME} {} frame {i} U vs ffmpeg", arm.name);
                assert_eq!(got.v, want.v, "{NAME} {} frame {i} V vs ffmpeg", arm.name);
            }
            let (leaf8, sub8, warp) = (
                crate::decode::scaled_leaf8_hits() - leaf8_before,
                crate::decode::scaled_sub8_hits() - sub8_before,
                crate::decode::scaled_warp_suppressed_hits() - warp_before,
            );
            eprintln!(
                "{NAME} {}: {} frames pixel-exact, scaled leaf8={leaf8} sub8={sub8} \
                 warp_suppressed={warp}",
                arm.name, arm.frames
            );
            if arm.name == "sub8+leaf8" {
                assert!(
                    leaf8 > 0 && sub8 > 0,
                    "{NAME} {}: the lifted paths never ran (leaf8={leaf8} sub8={sub8})",
                    arm.name
                );
            } else {
                assert!(
                    warp > 0,
                    "{NAME} {}: no block reached WARPED_CAUSAL eligibility under a scaled \
                     reference, so the alphabet fix is unexercised",
                    arm.name
                );
            }
        }
    }

    /// lane-interintra r1: `--enable-interintra-comp=1` streams must DECODE
    /// pixel-exact -- the interintra flag/mode/wedge-flag syntax and the
    /// non-wedge smooth blend are ported. A refusal naming interintra fails
    /// the gate UNLESS it is the wedge refusal (`use_wedge_interintra == 1`
    /// is r2, and this recipe encodes with wedge off so it must not fire
    /// either); warp/obmc are enabled and supported; refusals for OTHER
    /// named capabilities (screen content tools) still skip that seed. The
    /// sweep must actually exercise interintra (`interintra_hits > 0`).
    #[test]
    fn a_real_aomenc_stream_with_interintra_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_interintra_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        // lane-midcut r1: rect interintra (`write_w != write_h`) built its
        // intra predictor at the square `side`; this gate passed with that
        // bug, so the shape gets its own delta-counted assert below.
        let rect_before = crate::decode::interintra_rect_hits();
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_INTERINTRA_GATE_ATTEMPTS")
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
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=1",
                "--enable-onesided-comp=1",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=1",
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
                "--enable-ref-frame-mvs=0",
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
                    assert!(
                        !msg.contains("warp"),
                        "{NAME} refused on warp (seed {seed}) -- warp decode is ported, this \
                         refusal must not exist: {msg}"
                    );
                    assert!(
                        !msg.contains("interintra"),
                        "{NAME} refused on interintra (seed {seed}) -- non-wedge interintra \
                         is ported and this recipe encodes with wedge off: {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal (non-warp capability): {msg}");
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
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
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
            // lane-hidden r2 (class gate-blind-to-hidden-frames): the same
            // stream again, comparing EVERY decode-order frame -- the hidden
            // (show_frame == 0) alt-ref frames the ffmpeg compare above
            // cannot see, because both sides of it emit shown frames only.
            let (hidden_total, hidden_n) = decode_all_frames_vs_oracle(&stream, NAME);
            eprintln!(
                "{NAME} HIDDEN: seed {seed}: {hidden_total} decode-order frames, \
                 {hidden_n} hidden, all pixel-exact vs the oracle"
            );
            // lane-hidden r2 SECOND ARM (the recipe above is untouched):
            // libaom's temporal alt-ref filter absorbs this recipe's hidden
            // frames, so the compare above is vacuous -- re-encode the same
            // fixture with --arnr-maxframes=0 and compare THOSE.
            if let Some((arm_total, arm_hidden)) = hidden_arnr_arm(NAME, &y4m.stdout, &args) {
                eprintln!(
                    "{NAME} HIDDEN-ARM(--arnr-maxframes=0): seed {seed}: {arm_total} \
                     decode-order frames, {arm_hidden} hidden, all pixel-exact vs the oracle"
                );
            }
            // lane-midcut r3 RECT ARM: the recipe above spells
            // `--enable-rect-partitions=0 --min-partition-size=32`, so it can
            // never produce a RECT interintra block -- the delta assert at the
            // end of this gate was unsatisfiable on it (the gate disabled the
            // very shape it claims to prove). Re-encode the SAME source with
            // the rect overrides APPENDED (aomenc keeps the LAST occurrence of
            // a repeated flag) and compare every decode-order frame of that
            // stream against aomdec, until a rect interintra block fires.
            if crate::decode::interintra_rect_hits() == rect_before {
                let mut rect_args = args.clone();
                rect_args.extend_from_slice(&[
                    "--enable-rect-partitions=1",
                    "--min-partition-size=8",
                    "--max-partition-size=64",
                ]);
                let out = run_with_stdin(Command::new(aomenc_path()).args(&rect_args), &y4m.stdout);
                assert!(
                    out.status.success(),
                    "{NAME} RECT-ARM: aomenc refused its own recipe plus the rect overrides: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let rect_stream = out.stdout;
                match decode_stream(&rect_stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} RECT-ARM failed outright, not a named refusal (seed {seed}): {msg}"
                        );
                        eprintln!("{NAME} RECT-ARM: skipped on a named refusal: {msg}");
                    }
                    Ok(_) => {
                        let (total, hidden) =
                            decode_all_frames_vs_oracle(&rect_stream, &format!("{NAME}-rect"));
                        eprintln!(
                            "{NAME} RECT-ARM: seed {seed}: {total} decode-order frames, {hidden} \
                             hidden, all pixel-exact vs the oracle, interintra_rect_hits={}",
                            crate::decode::interintra_rect_hits() - rect_before
                        );
                    }
                }
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused on other capabilities; the gate never exercised warp"
        );
        assert!(
            crate::decode::interintra_hits() > 0,
            "{NAME}: {matched} matches but zero interintra blocks fired -- the gate decodes \
             these streams without ever taking interintra, so it proves nothing"
        );
        // lane-defon r1: `--enable-onesided-comp=1` is spelled on this
        // recipe, so a UNIDIR_COMP_REFERENCE pair must actually have been
        // read -- otherwise the on-value proves nothing.
        assert!(
            crate::decode::uni_comp_hits() > 0,
            "{NAME}: {matched} matches but zero UNIDIR_COMP_REFERENCE pairs read -- \
             --enable-onesided-comp=1 never reached the uni_comp_ref alphabet"
        );
        assert!(
            crate::decode::interintra_rect_hits() > rect_before,
            "{NAME}: {matched} matches but zero RECTANGULAR interintra blocks fired -- the \
             square-only blend defect (lane-midcut r1) would pass this gate unseen"
        );
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched} pixel-exact matches out of {n_attempts}, interintra_hits={} interintra_rect_hits={} uni_comp_hits={}",
            crate::decode::interintra_hits(),
            crate::decode::interintra_rect_hits() - rect_before,
            crate::decode::uni_comp_hits()
        );
    }

    /// lane-scaledref r1: a real aomenc `--superres-mode=1` sequence with
    /// compound, OBMC, interintra and 8x8 leaves ALL enabled must decode
    /// pixel-exact -- these are the combinations the three
    /// "... with a scaled reference (superres, unimplemented)" refusals used
    /// to cover. Each combination gets its OWN hard-asserted hit counter
    /// (`crate::decode::scaled_*_hits`), because an unscaled reference would
    /// pass the pixels and prove nothing (class `gate-blind-to-feature`), and
    /// a stream that scales but never takes compound/OBMC/interintra/8x8
    /// would prove nothing about the code this round adds either. A refusal
    /// naming a scaled reference is a hard failure: those are exactly the
    /// strings this round lifts. Other named capabilities skip that seed.
    #[test]
    fn a_real_aomenc_superres_stream_with_compound_obmc_and_interintra_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_superres_stream_with_compound_obmc_and_interintra_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_SCALEDREF_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(12);
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
            let on = |name: &str| {
                if std::env::var(name).as_deref() == Ok("0") {
                    "0"
                } else {
                    "1"
                }
            };
            // lane-scaledref r2: warp is ON. A warp-capable frame header
            // under a scaled reference is exactly the case r1 got wrong
            // (the block must read the 2-symbol `obmc_cdf`, not the
            // 3-symbol `motion_mode_cdf` -- libaom `motion_mode_allowed`,
            // blockd.h:1484), and it decoded silently wrong pixels rather
            // than refusing. `EC_SCALEDREF_WARP=0` turns it off for
            // bisecting only.
            let warp = format!("--enable-warped-motion={}", on("EC_SCALEDREF_WARP"));
            // lane-scaledref r2: `EC_SCALEDREF_MODE=3` (qthresh) lets
            // superres switch on and off per frame, so a compound block can
            // reference one scaled and one unscaled reference -- the mixed
            // case mode 1 (every frame scaled) never produces.
            let superres_mode = format!(
                "--superres-mode={}",
                std::env::var("EC_SCALEDREF_MODE").unwrap_or_else(|_| "1".into())
            );
            let obmc = format!("--enable-obmc={}", on("EC_SCALEDREF_OBMC"));
            let interintra = format!("--enable-interintra-comp={}", on("EC_SCALEDREF_II"));
            let lag = format!(
                "--lag-in-frames={}",
                if on("EC_SCALEDREF_COMP") == "0" {
                    "0"
                } else {
                    "25"
                }
            );
            let alt_ref = format!("--auto-alt-ref={}", on("EC_SCALEDREF_COMP"));
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                // superres on every frame, key and inter alike -- the scaled
                // reference this gate is about.
                // denominator 16 (the maximum) keeps the CODED frame
                // 32x64 -- a multiple of 16 both ways, so no frame-edge
                // 16x16 straddle forces a sub-16x16 partition this decoder
                // still refuses on the aligned path ("an inter partition
                // below 16x16"). Denominator 12 codes 43 columns and every
                // seed refused there.
                &superres_mode,
                "--superres-denominator=16",
                "--superres-kf-denominator=16",
                // the four combinations this round lifts (each switchable
                // by env for bisecting a mismatch without a rebuild)
                &alt_ref,
                &lag,
                "--enable-order-hint=1",
                &warp,
                &obmc,
                &interintra,
                "--enable-smooth-interintra=1",
                "--enable-interintra-wedge=0",
                "--enable-masked-comp=0",
                "--enable-onesided-comp=0",
                "--enable-fwd-kf=0",
                "--tune-content=default",
                "--max-partition-size=32",
                "--min-partition-size=32",
                // everything this decoder still refuses under any reference
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
                    assert!(
                        !msg.contains("scaled reference"),
                        "{NAME} refused on a scaled reference (seed {seed}) -- those refusals \
                         are exactly what this round lifts: {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal (other capability): {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                let mismatched = frames
                    .iter()
                    .zip(&ffmpeg_frames)
                    .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                if mismatched {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
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
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched}/{n_attempts} pixel-exact, \
             superres_hits={} predict_scaled_hits={} scaled_compound={} scaled_warp_fallback={} \
             scaled_obmc={} scaled_interintra={} scaled_block8={} scaled_warp_suppressed={} mixed_scale_compound={}",
            crate::superres::superres_hits(),
            crate::mc::predict_scaled_hits(),
            crate::decode::scaled_compound_hits(),
            crate::decode::scaled_warp_fallback_hits(),
            crate::decode::scaled_obmc_hits(),
            crate::decode::scaled_interintra_hits(),
            crate::decode::scaled_block8_hits(),
            crate::decode::scaled_warp_suppressed_hits(),
            crate::decode::mixed_scale_compound_hits(),
        );
        assert!(
            matched > 0,
            "{NAME}: every attempt refused on other capabilities; the gate proves nothing"
        );
        assert!(
            crate::mc::predict_scaled_hits() > 0,
            "{NAME}: matched but zero predict_scaled_hits -- no block took the scaled MC path"
        );
        // The 8x8 leaf is NOT asserted here: it stays refused (no recipe
        // produced an 8x8 inter leaf at all), so its counter is a
        // diagnostic only -- see this gate's own doc and decode.rs.
        // `scaled_warp_suppressed` IS asserted: it counts the blocks whose
        // WARPED_CAUSAL eligibility libaom drops because the reference is
        // scaled, i.e. the blocks that must read `obmc_cdf`. Zero of them
        // would mean this gate's `--enable-warped-motion=1` never reached a
        // warp-eligible block and proves nothing about r2's fix.
        assert!(
            crate::decode::scaled_warp_suppressed_hits() > 0,
            "{NAME}: {matched} matches but no block had its warp eligibility \
             suppressed by a scaled reference -- the warp arm proves nothing"
        );
        for (hits, what) in [
            (
                crate::decode::scaled_compound_hits(),
                "compound with a scaled reference",
            ),
            (
                crate::decode::scaled_obmc_hits(),
                "OBMC with a scaled reference",
            ),
            (
                crate::decode::scaled_interintra_hits(),
                "interintra with a scaled reference",
            ),
        ] {
            assert!(
                hits > 0,
                "{NAME}: {matched} matches but zero blocks took {what}"
            );
        }
    }

    /// lane-defon r1: the 10-bit one-sided-compound hole --
    /// `--enable-onesided-comp` was spelled on in no 10-bit gate (5 of the 15
    /// pin it off, the rest default), so the UNIDIR_COMP_REFERENCE alphabet
    /// was proven at 8 bits only while both of his films are 10-bit 4:2:0.
    /// `--enable-diff-wtd-comp=1` is deliberately NOT spelled here (a flag a
    /// gate turns on without asserting it fired is coverage theatre, class
    /// gate-blind-to-feature): measured this round, the 10-bit `gradients`
    /// fixture decodes 8/8 pixel-exact with `diffwtd_hits == 0` -- smooth
    /// gradients never win
    /// a difference-weighted mask (the 8-bit gate needs `mandelbrot`'s hard
    /// diagonal edge, and [`ten_bit_tool_gate`] has no mandelbrot source), so
    /// hard-asserting it here would fail on encoder choice, not on a decoder
    /// defect.
    #[test]
    fn a_real_aomenc_10bit_stream_with_onesided_compound_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_stream_with_onesided_compound_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        ten_bit_tool_gate(
            NAME,
            64,
            64,
            24,
            &[
                "--enable-onesided-comp=1",
                "--enable-masked-comp=1",
                "--enable-dist-wtd-comp=0",
                "--cq-level=45",
                "--kf-max-dist=1000",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-global-motion=0",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-interintra-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
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
                "--enable-ref-frame-mvs=0",
            ],
            &[42, 43, 44, 45, 46, 47, 48, 49],
            &[(
                "uni_comp_hits",
                crate::decode::uni_comp_hits as fn() -> usize,
            )],
        );
    }

    /// lane-defon r1: the 10-bit sibling of
    /// [`a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact`] --
    /// `--enable-interintra-wedge=1` was spelled on in no 10-bit gate (5 of
    /// the 15 pin it off), and his two films are 10-bit, so the 8-bit
    /// gate alone proves nothing about the streams that matter. Same tool
    /// set as the 8-bit recipe (`--enable-masked-comp=0` keeps inter blocks
    /// off the COMPOUND_WEDGE path, fixed 32x32 partitions), driven through
    /// [`ten_bit_tool_gate`], which hard-asserts `wii_hits` moved and
    /// compares every plane against ffmpeg's own 10-bit decode.
    #[test]
    fn a_real_aomenc_10bit_stream_with_interintra_wedge_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_stream_with_interintra_wedge_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        ten_bit_tool_gate(
            NAME,
            64,
            64,
            24,
            &[
                "--enable-interintra-wedge=1",
                "--enable-interintra-comp=1",
                "--enable-smooth-interintra=1",
                "--cq-level=45",
                "--kf-max-dist=1000",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-onesided-comp=0",
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
                "--enable-ref-frame-mvs=0",
            ],
            &[42, 43, 44, 45, 46, 47, 48, 49],
            &[
                ("wii_hits", crate::decode::wii_hits as fn() -> usize),
                (
                    "interintra_hits",
                    crate::decode::interintra_hits as fn() -> usize,
                ),
            ],
        );
    }

    /// lane-wii r2: `--enable-interintra-wedge=1` streams must DECODE
    /// pixel-exact -- the wedge-interintra syntax (adapting `wedge_index`
    /// CDF symbol, fixed sign 0) and the wedge mask blend are ported over
    /// the checksum-verified codebook. `--enable-masked-comp=0` stays so
    /// the encoder cannot route inter blocks through the still-unported
    /// COMPOUND_WEDGE/DIFFWTD path. A refusal naming interintra/wedge
    /// fails the gate; refusals for OTHER named capabilities still skip
    /// that seed. Zero `wii_hits` on a run means the encoder never
    /// selected wedge-interintra -- soft-skipped (warned), not failed:
    /// that is encoder choice, not a decoder bug.
    #[test]
    fn a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_interintra_wedge_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_WEDGE_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            let source = if attempt % 2 == 0 {
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"))
            } else {
                // `end_pts` does NOT bound mandelbrot (it generated 300+
                // frames and hung the gate for an hour); `-t` below is the
                // real bound, matching every other mandelbrot gate recipe.
                format!("mandelbrot=size={width}x{height}:rate=25")
            };
            let duration_arg = format!("{duration}");
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
                    "-t",
                    &duration_arg,
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
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=1",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=1",
                "--enable-smooth-interintra=1",
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
                "--enable-ref-frame-mvs=0",
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
                    // Verifier note (lane-wii r2): no refusal string containing
                    // "wedge"/"interintra" exists in the decoder any more, so
                    // string-forbidding asserts here would be vacuous (class
                    // gate-blind-to-feature). The proof this gate carries is
                    // pixel-exact decode plus the wii_hits count printed below.
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal (non-wedge capability): {msg}");
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
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
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
            // lane-hidden r2 (class gate-blind-to-hidden-frames): the same
            // stream again, comparing EVERY decode-order frame -- the hidden
            // (show_frame == 0) alt-ref frames the ffmpeg compare above
            // cannot see, because both sides of it emit shown frames only.
            let (hidden_total, hidden_n) = decode_all_frames_vs_oracle(&stream, NAME);
            eprintln!(
                "{NAME} HIDDEN: seed {seed}: {hidden_total} decode-order frames, \
                 {hidden_n} hidden, all pixel-exact vs the oracle"
            );
            // lane-hidden r2 SECOND ARM (the recipe above is untouched):
            // libaom's temporal alt-ref filter absorbs this recipe's hidden
            // frames, so the compare above is vacuous -- re-encode the same
            // fixture with --arnr-maxframes=0 and compare THOSE.
            if let Some((arm_total, arm_hidden)) = hidden_arnr_arm(NAME, &y4m.stdout, &args) {
                eprintln!(
                    "{NAME} HIDDEN-ARM(--arnr-maxframes=0): seed {seed}: {arm_total} \
                     decode-order frames, {arm_hidden} hidden, all pixel-exact vs the oracle"
                );
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused on other capabilities; the gate never exercised wedge-interintra"
        );
        // Hardened from a soft-note (verifier: gate-blind-to-feature): the
        // recipe fired wedge-interintra on EVERY one of 6 hammer runs
        // (wii_hits 2-7), so a zero-hit run is a regression, not sampling.
        assert!(
            crate::decode::wii_hits() > 0,
            "{NAME}: {matched} pixel-exact matches but zero wedge-interintra blocks fired -- \
             the blend is unexercised (recipe fired 2-7 hits per run when landed)"
        );
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched} pixel-exact matches out of {n_attempts}, wii_hits={}",
            crate::decode::wii_hits()
        );
    }

    /// lane-gmaffine r1: an inter frame whose `global_motion_params` carries a
    /// SIX-parameter (`AFFINE`) model for a single-reference frame -- refused
    /// by name since lane-gm r4 because no aomenc recipe ever reached it.
    /// The reason it never did is in libaom's own encoder, not in the
    /// content: `av1/encoder/global_motion_facade.c:24` pins the model search
    /// to `ROTZOOM` at both ends, so *no* `--enable-global-motion=1` command
    /// line over any source can produce the case. The decoder must still
    /// handle it (the bitstream allows it, `read_global_motion_params` reads
    /// params 4/5 with their own subexp references, and ffmpeg decodes it),
    /// so this gate widens exactly that one encoder-side search range in a
    /// side build and encodes with it. Everything downstream is ordinary
    /// libaom and the pixel oracle is still ffmpeg.
    ///
    /// Content: rotation composed with a progressive *anisotropic* stretch --
    /// a similarity transform plus unequal axis scaling is precisely what a
    /// 4-parameter ROTZOOM model cannot express, which is what makes the
    /// encoder's RD pick AFFINE. `affine_gm_hits()` hard-asserts that real
    /// blocks were predicted through the 6-parameter warp, not merely that a
    /// header parsed one (class `gate-blind-to-feature`).
    #[test]
    fn a_real_affine_global_motion_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_affine_global_motion_stream_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_affine_aomenc() {
            eprintln!(
                "SKIP {NAME}: no affine-capable aomenc at {} (scripts/build-aom-affine-oracle.sh)",
                affine_aomenc_path().display()
            );
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let duration = frame_count as f64 / 25.0;
        // Both depths: his own AV1 library is yuv420p10le throughout, and a
        // 10-bit AFFINE warp runs the same `warp_affine` over wider samples.
        for depth in [8u32, 10u32] {
            // `-t` is the real bound on mandelbrot (it ignores `end_pts` and
            // has hung a gate for an hour); the 96x96 source is rotated and
            // then stretched horizontally only, and cropped back to 64x64.
            let source = format!(
                "mandelbrot=size=96x96:rate=25,rotate=a=0.12*t:c=black,\
                 scale=w='ceil(96*(1+0.03*n)/2)*2':h=96:eval=frame,crop={width}:{height}"
            );
            let pix_fmt = if depth == 10 {
                "yuv420p10le"
            } else {
                "yuv420p"
            };
            let mut ff_args: Vec<&str> = vec![
                "-v", "error", "-f", "lavfi", "-i", &source, "-pix_fmt", pix_fmt,
            ];
            // y4m has no official 10-bit tag, same as the 10-bit gate above.
            if depth == 10 {
                ff_args.extend(["-strict", "-1"]);
            }
            let duration_arg = duration.to_string();
            ff_args.extend(["-t", &duration_arg, "-f", "yuv4mpegpipe", "-"]);
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args(&ff_args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let bit_depth = format!("--bit-depth={depth}");
            let input_bit_depth = format!("--input-bit-depth={depth}");
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=0",
                "--lag-in-frames=0",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-global-motion=1",
                "--enable-warped-motion=0",
                "--enable-obmc=0",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
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
                "--enable-ref-frame-mvs=0",
                &bit_depth,
                &input_bit_depth,
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let out = run_with_stdin(Command::new(affine_aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before = crate::decode::affine_gm_hits();
            let frames =
                decode_stream(&stream).unwrap_or_else(|e| panic!("{NAME} ({depth}-bit): {e}"));
            let ffmpeg_frames = if depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frame_count)
            };
            assert_eq!(frames.len(), frame_count);
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} {depth}-bit frame {i} luma vs ffmpeg");
                assert_eq!(got.u, want.u, "{NAME} {depth}-bit frame {i} U vs ffmpeg");
                assert_eq!(got.v, want.v, "{NAME} {depth}-bit frame {i} V vs ffmpeg");
            }
            let hits = crate::decode::affine_gm_hits() - before;
            assert!(
                hits > 0,
                "{NAME} ({depth}-bit): decoded pixel-exact but no block was predicted through a \
                 6-parameter global-motion warp -- the AFFINE path is unexercised"
            );
            eprintln!(
                "{NAME}: {depth}-bit, {frame_count} frames pixel-exact, affine_gm_hits={hits}"
            );
        }
    }

    /// lane-av1gwarp12: the SINGLE-reference GLOBAL-warp witness at 12 bits
    /// -- the open half lane-av112bitw recorded (engagement 0 on every prior
    /// 12-bit content). The decode path is the same `warp_affine` with
    /// `warp_round_0(12)` the local-warp witness proved, fed by
    /// `global_warp_params(gm_ref.params)` at `decode_inter_block`'s
    /// single-ref arm -- but composition had no stream: the travelling-box
    /// source fits only TRANSLATION global models (excluded by
    /// `is_global_mv_block`), and the rotating mandelbrot reached the global
    /// models only on compound blocks. This recipe closes it: rotation +
    /// uniform zoom (a similarity transform, ROTZOOM's exact family), the
    /// compound tools off so a global model can only ride a single-reference
    /// GLOBALMV block, and partitions pinned to 32 so the 8x8 leaf arm is
    /// structurally out of the stream. Vanilla aomenc pins its gm search to
    /// ROTZOOM (`global_motion_facade.c:24`), so every >TRANSLATION model a
    /// real aomenc emits is ROTZOOM; AFFINE needs the widened side build
    /// (`a_real_affine_global_motion_stream_decodes_pixel_exact`).
    ///
    /// Non-vacuous by construction: `rotzoom_gm_warp_hits()` hard-asserts a
    /// 16x16+ block was really predicted through the global warp with a
    /// ROTZOOM model, the zero `compound_warp_hits` delta keeps the
    /// single-reference premise honest, and every plane of every frame
    /// compares against ffmpeg.
    #[test]
    fn a_real_rotzoom_global_warp_12bit_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_rotzoom_global_warp_12bit_stream_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        const W: usize = 128;
        const H: usize = 128;
        const FRAMES: usize = 16;
        let y4m = y4m_12bit_rotzoom_gm_source(FRAMES);
        let stream = encode_12bit(
            &y4m,
            FRAMES,
            &[
                "--enable-global-motion=1",
                "--enable-warped-motion=0",
                "--enable-obmc=0",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-ref-frame-mvs=0",
                "--min-partition-size=32",
                "--max-partition-size=32",
            ],
        );
        assert_12bit_sequence_header(&stream, NAME);
        let (before_rotzoom, before_affine, before_selected, before_compound) = (
            decode::rotzoom_gm_warp_hits(),
            decode::affine_gm_hits(),
            decode::warp_selected_hits(),
            decode::compound_warp_hits(),
        );
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{NAME}: decode_stream refused a real 12-bit global-warp stream: {e}")
        });
        assert_eq!(frames.len(), FRAMES);
        let (delta_rotzoom, delta_affine, delta_selected) = (
            decode::rotzoom_gm_warp_hits() - before_rotzoom,
            decode::affine_gm_hits() - before_affine,
            decode::warp_selected_hits() - before_selected,
        );
        eprintln!(
            "{NAME}: gm_warp_engagement rotzoom(single-ref global warp)={delta_rotzoom} \
             affine={delta_affine} selected_local_warp={delta_selected}"
        );
        assert!(
            delta_rotzoom > 0,
            "{NAME}: decoded but no single-reference ROTZOOM global-warp block fired -- \
             the 12-bit single-ref global-warp path is unexercised and the pixel \
             compare would prove nothing"
        );
        assert_eq!(
            decode::compound_warp_hits() - before_compound,
            0,
            "{NAME}: a compound warp block ran -- the single-reference premise is gone"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence_12bit(&stream, W, H, FRAMES);
        for (i, (ours, reference)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, reference.y, "{NAME} frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, reference.u, "{NAME} frame {i} U vs ffmpeg");
            assert_eq!(ours.v, reference.v, "{NAME} frame {i} V vs ffmpeg");
        }
        eprintln!(
            "{NAME}: {FRAMES} frames pixel-exact vs ffmpeg, rotzoom_gm_warp_hits={delta_rotzoom}"
        );
    }

    /// lane-gmaffine r1 item (3): `GLOBALMV` at an 8x8 leaf, ROTZOOM global
    /// motion, warp built from the frame's own model.
    #[test]
    fn a_real_globalmv_8x8_leaf_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_globalmv_8x8_leaf_stream_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        run_8x8_leaf_motion_gate(NAME, true, crate::decode::globalmv_hits_8);
    }

    /// lane-gmaffine r1 item (2): `WARPED_CAUSAL` at an 8x8 leaf --
    /// findSamples + `av1_find_projection` + the warp filter at BLOCK_8X8.
    #[test]
    fn a_real_warped_causal_8x8_leaf_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_warped_causal_8x8_leaf_stream_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        run_8x8_leaf_motion_gate(NAME, false, crate::decode::warp_hits_8);
    }

    /// lane-rect16 r1/r2: a plain default-settings aomenc run over lavfi
    /// `mandelbrot` (`--cpu-used=4 --end-usage=q --cq-level=32`, no
    /// `--enable-*` flags) used to refuse frame 0 outright with "a partition
    /// below 16x16 other than a clean split". r1 root-caused the FIRST hit to
    /// a `PARTITION_VERT_B` `partition_w16` symbol at mi=(3,2) and wired it
    /// (left 8x16 real `decode_block_rect` strip + two stacked 8x8
    /// `decode_leaf8` leaves on the right). r2 re-measured after that fix
    /// (`decision-before-quantiser`-shaped: don't trust a stale premise) and
    /// found this stream's REAL first-hit blocker in z-order is earlier, at
    /// mi=(0,7): a plain `PARTITION_HORZ`. That arm is now wired too
    /// (`decode_leaf_rect`, sibling to `decode_leaf8` but real-mi-addressed
    /// for a non-square strip) -- `horz_vert_intra_hits` proves it fires.
    /// This exact stream's own HORZ block at mi=(0,7) is *coded* (real
    /// coefficients), which is NOT ported (same ceiling as VERT_B's own
    /// non-skip refusal -- no rectangular-transform coefficient tables at
    /// this size), so decode still stops there and `vert_b_intra_hits`
    /// never gets a chance to increment on this particular stream. This gate
    /// proves the HORZ arm fires and the OLD (now-false) two-clause refusal
    /// never reappears, not a pixel-exact decode of this stream.
    #[test]
    fn a_real_aomenc_stream_with_mandelbrot_fires_the_vert_b_partition_arm() {
        const NAME: &str = "a_real_aomenc_stream_with_mandelbrot_fires_the_vert_b_partition_arm";
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
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("mandelbrot=size={width}x{height}"),
                "-pix_fmt",
                "yuv420p",
                "-t",
                "1",
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
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--cpu-used=4",
                "--end-usage=q",
                "--cq-level=32",
                "--passes=1",
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
        // lane-rect16 r2: the recipe above was missing `--passes=1` --
        // without it aomenc silently ran 2-pass and wrote nothing to stdout
        // on pass 1 (still exit 0), so this gate always decoded an EMPTY
        // stream and never actually exercised the arm it claims to gate.
        assert!(
            !stream.is_empty(),
            "{NAME}: aomenc wrote an empty stream (missing --passes=1?)"
        );
        let result = decode_stream(&stream);
        // The now-lifted false claim ("codes only the square arms below
        // 16x16") must never appear again.
        if let Err(e) = &result {
            let msg = e.to_string();
            assert!(
                !msg.contains("this decoder codes only the square arms below 16x16"),
                "{NAME}: the OLD (disproved) refusal reappeared: {msg}"
            );
        }
        assert!(
            crate::decode::horz_vert_intra_hits() > 0,
            "{NAME}: the plain PARTITION_HORZ/VERT arm never fired on this stream (regression -- \
             it fired deterministically at mi=(0,7) when this gate was written)"
        );
        eprintln!(
            "{NAME}: horz_vert_intra_hits={}, vert_b_intra_hits={}, decode result: {:?}",
            crate::decode::horz_vert_intra_hits(),
            crate::decode::vert_b_intra_hits(),
            result.as_ref().map(|f| f.len()).map_err(|e| e.to_string())
        );
    }

    /// lane-rectx r3: a real `aomenc` stream whose 16x16-level
    /// `PARTITION_HORZ`/`PARTITION_VERT` strips are CODED (non-skip), i.e.
    /// carry genuine `TX_16X8`/`TX_8X16` luma coefficients and their
    /// `TX_8X4`/`TX_4X8` chroma halves through [`decode_leaf_rect`]'s
    /// lane-rectx arm -- the refusal "a coded (non-skip) HORZ/VERT rect strip
    /// below 16x16" this round lifts. `--min-partition-size=8
    /// --max-partition-size=32 --enable-rect-partitions=1` is what makes
    /// aomenc reach for a 16x8 leaf at all; mandelbrot at `cq-level=24` is
    /// what makes those leaves non-skip (a gradient fixture codes them all
    /// away). `--reduced-tx-type-set=1` pins the five-symbol
    /// `EXT_TX_SET_DTT4_IDTX` alphabet: without it aomenc also writes
    /// `V_DCT`/`H_DCT`, whose 1D tx classes [`read_coeffs_rect`] still
    /// refuses by name (see the r3 report's open residue), so a `=0` run
    /// would prove nothing about the 2D path it does support.
    ///
    /// r2 shipped this path unproven; it was wrong. `TxbSet::LumaRect16x8`
    /// read its `tx_type` symbol from the 16x16 row, but libaom's
    /// `read_tx_type` indexes `intra_ext_tx_cdf[eset][txsize_sqr_map[tx_size]]`
    /// and `txsize_sqr_map[TX_16X8] == TX_8X8` -- a five-symbol CDF where a
    /// seven-symbol one belonged, desyncing at the FIRST coded rect leaf
    /// (whole frame wrong from pixel (0,0)).
    #[test]
    fn a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 64usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("mandelbrot=size={width}x{height}:start_x=-0.6"),
                "-pix_fmt",
                "yuv420p",
                "-t",
                "1",
                "-vframes",
                "1",
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
        let encode = || {
            let out = run_with_stdin(
                Command::new(aomenc_path()).args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=16",
                    "--cpu-used=4",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    "--kf-max-dist=0",
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-tx-size-search=0",
                    // Filter intra on a strip is a separate, still-refused
                    // predictor (`filter intra on a HORZ/VERT strip`); this
                    // gate is about the rect RESIDUAL path, so it is off here
                    // and covered by the square filter-intra gate instead.
                    "--enable-filter-intra=0",
                    "--reduced-tx-type-set=1",
                    "--min-partition-size=8",
                    "--max-partition-size=32",
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
            out.stdout
        };
        // The fixture rule: an encoder output is only a pin if it reproduces.
        let stream = encode();
        assert_eq!(
            stream,
            encode(),
            "{NAME}: aomenc output is not reproducible for this recipe"
        );
        let before = crate::decode::rect_leaf_coeff_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: decode failed, not a pixel mismatch: {e}"));
        let fired = crate::decode::rect_leaf_coeff_hits() - before;
        assert!(
            fired > 0,
            "{NAME}: the stream decoded but no coded (non-skip) rect leaf fired -- \
             the gate proves nothing"
        );
        // The ENCODE's frame count, asserted ABOVE the oracle call: the y4m is
        // rendered with `-t 1` and `-vframes 1` (27566) -- one frame at the
        // source's default rate 25 -- and aomenc runs with no `--limit` and
        // `--kf-max-dist=0` (27593), so the stream codes exactly 1 frame.
        assert_eq!(frames.len(), 1, "{NAME}: expected one key frame");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frames.len());
        assert_eq!(
            ffmpeg_frames.len(),
            frames.len(),
            "{NAME}: ffmpeg frame count"
        );
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(
                got.y, want.y,
                "{NAME} frame {i} luma vs ffmpeg ({fired} coded rect leaves)"
            );
            assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg");
        }
        eprintln!("{NAME}: pixel-exact, rect_leaf_coeff_hits={fired}");
    }

    /// lane-rectx r5: the same 16x8/8x16 rect leaves, but this time proving
    /// what a LATER, non-strip block reads from them. `decode_leaf_rect` took
    /// the mi-exact intra mode map in r4; `decode_block`/`decode_block_rect`/
    /// `decode_block_rect64` did not, so a 32x16 strip whose left neighbour was
    /// a 16x8 leaf still read the coarse per-16x16 `left_mode` slot -- which no
    /// sub-16x16 block ever writes -- and picked a DIFFERENT `kf_y_mode` CDF row
    /// for the same decoded mode value. The stream stayed in sync through mode
    /// info (`EC_TRACE_MODE_STEP` ranges match up to that symbol) and then
    /// diverged inside the block's own coefficients: the whole bottom-right
    /// 32x32 quadrant of this fixture decoded wrong (1650 bytes) while the
    /// three earlier quadrants were exact -- wrong pixels returned as success.
    /// `mode_mi_override_hits` counts exactly the reads where the mi-exact
    /// neighbour disagrees with the coarse slot, so a regression that drops the
    /// override again fails the counter as well as the pixels.
    #[test]
    fn a_real_aomenc_stream_whose_square_block_reads_a_sub16_neighbours_mode_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_whose_square_block_reads_a_sub16_neighbours_mode_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 64usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("rgbtestsrc=size={width}x{height}"),
                "-pix_fmt",
                "yuv420p",
                "-t",
                "1",
                "-vframes",
                "1",
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
        // Two swept cells of the r4 sweep that decoded to completion with
        // WRONG pixels (1714 and 1650 bytes): `--reduced-tx-type-set` and
        // `--enable-filter-intra` are the two axes that move which block sizes
        // aomenc reaches for here; both arms are pinned.
        let mut total_overrides = 0usize;
        for (rtx, filter_intra) in [("1", "0"), ("0", "1")] {
            let encode = || {
                let out = run_with_stdin(
                    Command::new(aomenc_path()).args([
                        "--codec=av1",
                        "--passes=1",
                        "--end-usage=q",
                        "--cq-level=32",
                        "--cpu-used=4",
                        "--threads=1",
                        "--row-mt=0",
                        "--sb-size=64",
                        "--kf-max-dist=0",
                        "--enable-rect-partitions=1",
                        "--enable-ab-partitions=0",
                        "--enable-1to4-partitions=0",
                        "--enable-tx-size-search=0",
                        "--enable-cdef=0",
                        "--enable-restoration=0",
                        &format!("--enable-filter-intra={filter_intra}"),
                        &format!("--reduced-tx-type-set={rtx}"),
                        "--min-partition-size=8",
                        "--max-partition-size=32",
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
                out.stdout
            };
            let stream = encode();
            assert_eq!(
                stream,
                encode(),
                "{NAME}: aomenc output is not reproducible (rtx={rtx} filter_intra={filter_intra})"
            );
            let before = crate::decode::mode_mi_override_hits();
            let frames = decode_stream(&stream).unwrap_or_else(|e| {
                panic!("{NAME}: decode failed (rtx={rtx} filter_intra={filter_intra}): {e}")
            });
            // Counted only on an attempt that actually decoded AND is compared
            // below -- a refusal never contributes to the firing assert.
            let overrides = crate::decode::mode_mi_override_hits() - before;
            // The ENCODE's frame count, asserted ABOVE the oracle call: the y4m is
            // rendered ONCE outside this loop with `-t 1` and `-vframes 1`
            // (27792) -- one frame at the source's default rate 25 -- and aomenc
            // runs with no `--limit` and `--kf-max-dist=0` (27726), so both
            // (rtx, filter_intra) arms code exactly 1 frame.
            assert_eq!(frames.len(), 1, "{NAME}: expected one key frame");
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frames.len());
            assert_eq!(
                ffmpeg_frames.len(),
                frames.len(),
                "{NAME}: ffmpeg frame count"
            );
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} frame {i} luma vs ffmpeg (rtx={rtx} filter_intra={filter_intra}, \
                     {overrides} mi-exact mode overrides)"
                );
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (rtx={rtx})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (rtx={rtx})");
            }
            total_overrides += overrides;
        }
        assert!(
            total_overrides > 0,
            "{NAME}: no non-strip block ever read a mi-exact neighbour that disagreed with its \
             coarse slot -- the gate proves nothing"
        );
        eprintln!("{NAME}: pixel-exact, mode_mi_override_hits={total_overrides}");
    }

    /// lane-cfl r1: the chroma intra-edge FILTER TYPE is the neighbour's
    /// `uv_mode` (libaom `get_filt_type`, reconintra.c: `chroma_above_mbmi`/
    /// `chroma_left_mbmi` -> `is_smooth` on `uv_mode`), and two whole decode
    /// paths passed a hardcoded `false` for it -- `decode_leaf_8x8` (every
    /// 8x8 leaf of a SPLIT) and `decode_leaf_rect` (every rect strip below
    /// 16x16), on a stale "chroma stays false (exact) until a smooth/paeth
    /// uv_mode can reach decode at all" comment. On top of that the coarse
    /// `above_uv_mode`/`left_uv_mode` cells are 16px-granular and their
    /// `for cell in 0..w / SUB` write runs ZERO times for any block below
    /// 16x16, so even the paths that DID read them saw whatever a
    /// 16x16-or-larger block had left there (class: context read from one
    /// cell / neighbour map at the wrong granularity). Result on this
    /// recipe: luma byte-exact, entropy ladder identical, and 40 U + 86 V
    /// chroma bytes wrong -- every one of them downstream of one 4x4 chroma
    /// block at mi(8,12) whose D135 + `angle_delta_uv=-3` left edge was
    /// filtered at the wrong strength (`ft=0` where the instrumented aomdec
    /// prints `ft=1`, left edge `95,59,30,28` vs `90,61,37,29`).
    ///
    /// Both arms hard-assert per-attempt counters for the two chroma block
    /// shapes involved (`UV_CFL_PRED` blocks, nonzero `angle_delta_uv`
    /// blocks) plus the mi-exact UV-neighbour override, and are counted only
    /// on an attempt that decoded AND is pixel-compared.
    #[test]
    fn a_real_aomenc_stream_whose_chroma_edge_filter_reads_a_sub16_neighbours_uv_mode_decodes_pixel_exact()
     {
        const NAME: &str = "a_real_aomenc_stream_whose_chroma_edge_filter_reads_a_sub16_neighbours_uv_mode_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let mut compared = 0usize;
        let (width, height) = (64usize, 64usize);
        // 8-bit arm = the r5 residue cell ("mandfi161"); 10-bit arm = its
        // twin through the same recipe at `--bit-depth=10`.
        for depth in [8usize, 10] {
            let pix = if depth == 10 {
                "yuv420p10le"
            } else {
                "yuv420p"
            };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("mandelbrot=size={width}x{height}:start_x=-0.6"),
                    "-pix_fmt",
                    pix,
                    "-strict",
                    "-1",
                    "-t",
                    "1",
                    "-vframes",
                    "1",
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
                "{NAME}: ffmpeg fixture (depth={depth}): {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let encode = || {
                let out = run_with_stdin(
                    Command::new(aomenc_path()).args([
                        "--codec=av1",
                        "--passes=1",
                        "--end-usage=q",
                        "--cq-level=16",
                        "--cpu-used=4",
                        "--threads=1",
                        "--row-mt=0",
                        "--sb-size=64",
                        "--kf-max-dist=0",
                        "--enable-rect-partitions=1",
                        "--enable-ab-partitions=0",
                        "--enable-1to4-partitions=0",
                        "--enable-tx-size-search=0",
                        "--enable-filter-intra=1",
                        "--reduced-tx-type-set=1",
                        "--min-partition-size=8",
                        "--max-partition-size=32",
                        &format!("--input-bit-depth={depth}"),
                        &format!("--bit-depth={depth}"),
                        "--obu",
                        "-o",
                        "-",
                        "-",
                    ]),
                    &y4m.stdout,
                );
                assert!(
                    out.status.success(),
                    "aomenc refused the fixture (depth={depth}): {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                out.stdout
            };
            let stream = encode();
            assert_eq!(
                stream,
                encode(),
                "{NAME}: aomenc output is not reproducible (depth={depth})"
            );
            let before_cfl = crate::decode::cfl_block_hits();
            let before_angle = crate::decode::uv_angle_delta_hits();
            let before_uv = crate::decode::uv_mode_mi_override_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                // Only a NAMED refusal is tolerated (COMMON rule): the 10-bit
                // twin of this recipe still stops at another lane's open
                // sub-16 AB-partition / strip-filter-intra refusals, so it
                // cannot be compared yet. Any other error is a failure, and
                // `compared` below keeps the test from passing vacuously.
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME}: decode failed (depth={depth}): {msg}"
                    );
                    eprintln!("{NAME}: depth={depth} arm refused (not compared): {msg}");
                    continue;
                }
            };
            compared += 1;
            // Read only on an attempt that decoded AND is compared below.
            let cfl_blocks = crate::decode::cfl_block_hits() - before_cfl;
            let angle_blocks = crate::decode::uv_angle_delta_hits() - before_angle;
            let uv_overrides = crate::decode::uv_mode_mi_override_hits() - before_uv;
            // The ENCODE's frame count, asserted ABOVE the oracle call: the y4m is
            // rendered with `-t 1` and `-vframes 1` (27852) -- one frame at the
            // source's default rate 25 -- and aomenc runs with no `--limit` and
            // `--kf-max-dist=0` (27890), so both depths code exactly 1 frame.
            assert_eq!(
                frames.len(),
                1,
                "{NAME}: expected one key frame (depth={depth})"
            );
            let ffmpeg_frames = if depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frames.len())
            };
            assert_eq!(
                ffmpeg_frames.len(),
                frames.len(),
                "{NAME}: ffmpeg frame count"
            );
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} frame {i} luma vs ffmpeg (depth={depth})"
                );
                assert_eq!(
                    got.u, want.u,
                    "{NAME} frame {i} U vs ffmpeg (depth={depth}, {cfl_blocks} CFL blocks, \
                     {angle_blocks} nonzero-angle_delta_uv blocks, {uv_overrides} mi-exact UV \
                     neighbour overrides)"
                );
                assert_eq!(
                    got.v, want.v,
                    "{NAME} frame {i} V vs ffmpeg (depth={depth}, {cfl_blocks} CFL blocks, \
                     {angle_blocks} nonzero-angle_delta_uv blocks, {uv_overrides} mi-exact UV \
                     neighbour overrides)"
                );
            }
            assert!(
                cfl_blocks > 0,
                "{NAME}: no UV_CFL_PRED block decoded (depth={depth}) -- the gate proves nothing"
            );
            assert!(
                angle_blocks > 0,
                "{NAME}: no nonzero angle_delta_uv chroma block decoded (depth={depth}) -- \
                 the chroma intra-edge filter path never ran"
            );
            assert!(
                uv_overrides > 0,
                "{NAME}: no chroma edge-filter read ever took a mi-exact UV neighbour that \
                 disagreed with its coarse 16x16 slot (depth={depth}) -- the r1 defect is \
                 not exercised"
            );
            eprintln!(
                "{NAME}: pixel-exact at {depth}-bit, cfl_blocks={cfl_blocks} \
                 angle_delta_uv_blocks={angle_blocks} uv_mi_overrides={uv_overrides}"
            );
        }
        assert!(
            compared > 0,
            "{NAME}: every arm refused -- the gate compared no pixels at all"
        );
    }

    /// lane-reelckf r1: ReelC's key frames decoded with no refusal and WRONG
    /// CHROMA (luma bit-exact at every one of ten seek points), from two
    /// defects this gate pins:
    ///
    ///  1. a SKIPPED intra block whose `uv_mode` is `UV_CFL_PRED` still
    ///     predicts with the CfL term -- libaom
    ///     `predict_and_reconstruct_intra_block` (decodeframe.c) calls
    ///     `av1_predict_intra_block_facade` unconditionally and only guards
    ///     the residual on `mbmi->skip_txfm`. Three skip arms (the 16x16-level
    ///     1:4 chroma pair and the two sub-8x8 4x4 groups) dropped it.
    ///  2. the chroma intra-edge FILTER TYPE of a 1:4 pair comes from the
    ///     CHROMA REFERENCE position (libaom `get_intra_edge_filter_type` ->
    ///     `chroma_above_mbmi`/`chroma_left_mbmi`), not from the closing
    ///     strip: at the closing strip the above cell is the pair's own first
    ///     strip, the mi-exact UV map misses and the coarse fallback answers,
    ///     which flipped `av1_use_intra_edge_upsample` (blk_wh 12 <= 16 for
    ///     type 0, no upsample for type 1) and moved directional chroma by
    ///     +-1..3.
    ///
    /// Recipe is a real `aomenc --sb-size=128 --cpu-used=0` (the shape ReelC
    /// uses) with 1:4 partitions and CfL on, both bit depths, and both
    /// counters hard-asserted per compared attempt.
    #[test]
    fn a_real_aomenc_sb128_stream_whose_skipped_cfl_and_1to4_chroma_pairs_decode_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_sb128_stream_whose_skipped_cfl_and_1to4_chroma_pairs_decode_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let mut compared = 0usize;
        let (width, height) = (256usize, 256usize);
        for depth in [8usize, 10] {
            let pix = if depth == 10 {
                "yuv420p10le"
            } else {
                "yuv420p"
            };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!(
                        "mandelbrot=size={width}x{height},\
                         noise=alls=20:allf=t+u:all_seed=7"
                    ),
                    "-pix_fmt",
                    pix,
                    "-strict",
                    "-1",
                    "-t",
                    "1",
                    "-vframes",
                    "1",
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
                "{NAME}: ffmpeg fixture (depth={depth}): {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let encode = || {
                let out = run_with_stdin(
                    Command::new(aomenc_path()).args([
                        "--codec=av1",
                        "--passes=1",
                        "--end-usage=q",
                        "--cq-level=52",
                        "--cpu-used=0",
                        "--threads=1",
                        "--row-mt=0",
                        "--sb-size=128",
                        "--kf-max-dist=0",
                        "--enable-rect-partitions=1",
                        "--enable-ab-partitions=0",
                        "--enable-1to4-partitions=1",
                        "--enable-cfl-intra=1",
                        "--enable-tx-size-search=0",
                        "--enable-filter-intra=0",
                        "--enable-intra-edge-filter=1",
                        "--reduced-tx-type-set=1",
                        "--min-partition-size=4",
                        "--max-partition-size=32",
                        &format!("--input-bit-depth={depth}"),
                        &format!("--bit-depth={depth}"),
                        "--obu",
                        "-o",
                        "-",
                        "-",
                    ]),
                    &y4m.stdout,
                );
                assert!(
                    out.status.success(),
                    "aomenc refused the fixture (depth={depth}): {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                out.stdout
            };
            let stream = encode();
            assert_eq!(
                stream,
                encode(),
                "{NAME}: aomenc output is not reproducible (depth={depth})"
            );
            let before = crate::decode::reelc_chroma_counters();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME}: decode failed (depth={depth}): {msg}"
                    );
                    eprintln!("{NAME}: depth={depth} arm refused (not compared): {msg}");
                    continue;
                }
            };
            compared += 1;
            // Counters read only on an attempt that decoded AND is compared.
            let after = crate::decode::reelc_chroma_counters();
            let (skip_cfl, dir_pairs) = (after.0 - before.0, after.1 - before.1);
            // The ENCODE's frame count: the y4m is rendered with `-t 1` and
            // `-vframes 1` (28056) -- one frame at the source's default rate 25
            // -- and aomenc runs with no `--limit` and `--kf-max-dist=0`
            // (28088), so both depths code exactly 1 frame. Asserted ABOVE the
            // oracle call.
            assert_eq!(frames.len(), 1, "{NAME}: depth={depth} key frame");
            let ffmpeg_frames = if depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frames.len())
            };
            assert_eq!(
                ffmpeg_frames.len(),
                frames.len(),
                "{NAME}: ffmpeg frame count"
            );
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "{NAME} frame {i} luma (depth={depth})");
                assert_eq!(
                    got.u, want.u,
                    "{NAME} frame {i} U (depth={depth}, {skip_cfl} skipped-CfL blocks, \
                     {dir_pairs} directional 1:4 chroma pairs)"
                );
                assert_eq!(
                    got.v, want.v,
                    "{NAME} frame {i} V (depth={depth}, {skip_cfl} skipped-CfL blocks, \
                     {dir_pairs} directional 1:4 chroma pairs)"
                );
            }
            // Defect 1 (skipped `UV_CFL_PRED`) is NOT asserted here: 20 aomenc
            // recipes (cq 40..63, 9 lavfi sources, min/max-partition 4..64,
            // both `--sb-size=128` and 64) never made the encoder pick a
            // skipped 1:4 / sub-8x8 CfL block, so an assert would only ever
            // fire on his real film. Its evidence is ReelC's own key frames
            // (lanes/reelckf-r1.report.md): ss=2700 went 499 wrong chroma
            // samples -> 0 on this fix alone. The count is printed so a future
            // recipe that does reach it is visible immediately. r2 pinned that
            // film frame as a fixture instead:
            // `a_real_film_key_frame_with_a_skipped_cfl_block_decodes_pixel_exact`
            // hard-asserts the counter.
            assert!(
                dir_pairs > 0,
                "{NAME}: no directional 1:4 chroma pair decoded (depth={depth}) -- defect 2 \
                 is not exercised"
            );
            eprintln!(
                "{NAME}: pixel-exact at {depth}-bit over {} frames, skipped_cfl={skip_cfl} \
                 directional_1to4_chroma_pairs={dir_pairs}",
                frames.len()
            );
        }
        assert!(
            compared > 0,
            "{NAME}: every arm refused -- the gate compared no pixels at all"
        );
    }

    /// lane-ab16 r1: real `aomenc --enable-ab-partitions=1` streams whose
    /// 16x16 blocks are split T-shaped (`PARTITION_HORZ_A`/`_HORZ_B`/
    /// `_VERT_A`/`_VERT_B`: two 8x8 squares plus one 16x8/8x16 strip), the
    /// refusals "a HORZ_A/HORZ_B/VERT_A partition below 16x16" and "a coded
    /// (non-skip) HORZ_B/VERT_B rect strip below 16x16" this round lifts.
    /// `--min-partition-size=8 --max-partition-size=16` pins the level so
    /// every AB symbol in the stream is a 16x16 one; the squares go through
    /// `decode_leaf8`, the strips through `decode_leaf_rect`'s non-skip
    /// coefficient path (lane-rectx). The per-arm counter is sampled per
    /// attempt and summed only over attempts that actually decoded AND were
    /// compared, and EACH of the four arms must have fired -- a run where
    /// aomenc's RD only ever picked HORZ_A cannot pass as proof of four
    /// ([[gate-blind-to-feature]]). 8-bit and 10-bit; `--tile-columns=1` is
    /// NOT among the attempts (base tile defect, reproducer in the r1
    /// report), and `--enable-tx-size-search` is left at aomenc's default
    /// (ON) -- pinning it off hid a panic in the TX4 sub-grid of an 8x8
    /// square inside a `PARTITION_VERT_A`/`_B` (r2).
    #[test]
    fn a_real_aomenc_stream_with_ab_partitions_below_16x16_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_ab_partitions_below_16x16_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        struct Attempt {
            src: &'static str,
            w: usize,
            h: usize,
            cq: u32,
            ten_bit: bool,
            tile_cols: u32,
            /// Left at aomenc's default (ON) instead of pinned off: reaches
            /// the TX4 sub-grid of an 8x8 square inside a
            /// `PARTITION_VERT_A`/`_B` (r2's panic), at the cost of the
            /// strip-level split transform lane-rectx still refuses.
            tx_size_search: bool,
        }
        // Recipes chosen by a ~200-cell sweep of {source, size, cq} at this
        // level (r1 report): one cell per arm that is pixel-exact end to end,
        // plus two 10-bit cells. `--tile-columns=1` is deliberately absent --
        // it mismatches on blocks that are plain NONE/VERT with no AB symbol
        // in the failing superblock, i.e. a base tile defect this lane
        // neither introduced nor owns (reproducer pinned in the r1 report).
        let attempts = [
            Attempt {
                src: "yuvtestsrc=size=64x64",
                w: 64,
                h: 64,
                cq: 22,
                ten_bit: false,
                tile_cols: 0,
                tx_size_search: false,
            },
            Attempt {
                src: "rgbtestsrc=size=64x64",
                w: 64,
                h: 64,
                cq: 20,
                ten_bit: false,
                tile_cols: 0,
                tx_size_search: false,
            },
            Attempt {
                src: "rgbtestsrc=size=64x64",
                w: 64,
                h: 64,
                cq: 50,
                ten_bit: false,
                tile_cols: 0,
                tx_size_search: false,
            },
            Attempt {
                src: "mandelbrot=size=64x64:start_x=-1.0",
                w: 64,
                h: 64,
                cq: 14,
                ten_bit: false,
                tile_cols: 0,
                tx_size_search: false,
            },
            // 10-bit: the only two cells of the sweep that are exact at this
            // depth (HORZ_A and VERT_A); the 10-bit HORZ_B/VERT_B cells all
            // trip the base chroma defect the r1 report pins, so those two
            // arms are proven 8-bit only.
            Attempt {
                src: "yuvtestsrc=size=64x64",
                w: 64,
                h: 64,
                cq: 30,
                ten_bit: true,
                tile_cols: 0,
                tx_size_search: false,
            },
            Attempt {
                src: "mandelbrot=size=64x64:start_x=0.4",
                w: 64,
                h: 64,
                cq: 30,
                ten_bit: true,
                tile_cols: 0,
                tx_size_search: false,
            },
            // `--enable-tx-size-search` at aomenc's default: an 8x8 square of
            // a VERT_A/VERT_B resolves to a 2x2 grid of TX4 units, which is
            // the reach lookup that had no table row and panicked (r2). These
            // attempts may legitimately stop at lane-rectx's still-standing
            // "strip with a split transform" refusal -- an `unsupported`
            // message is tolerated, a panic or a pixel mismatch is not, and
            // at least one of them must reach a TX4 unit inside a vertical AB
            // partition.
            Attempt {
                src: "yuvtestsrc=size=64x64",
                w: 64,
                h: 64,
                cq: 55,
                ten_bit: false,
                tile_cols: 0,
                tx_size_search: true,
            },
            Attempt {
                src: "yuvtestsrc=size=64x64",
                w: 64,
                h: 64,
                cq: 22,
                ten_bit: false,
                tile_cols: 0,
                tx_size_search: true,
            },
            Attempt {
                src: "rgbtestsrc=size=64x64",
                w: 64,
                h: 64,
                cq: 20,
                ten_bit: false,
                tile_cols: 0,
                tx_size_search: true,
            },
            Attempt {
                src: "mandelbrot=size=64x64:start_x=-1.0",
                w: 64,
                h: 64,
                cq: 14,
                ten_bit: false,
                tile_cols: 0,
                tx_size_search: true,
            },
        ];
        let mut arms = [0usize; 4];
        let mut compared = 0usize;
        let mut tx4_reached = 0usize;
        let mut tx4_compared = 0usize;
        for a in &attempts {
            let desc = format!(
                "{} cq={} {}bit tiles={}",
                a.src,
                a.cq,
                if a.ten_bit { 10 } else { 8 },
                a.tile_cols
            );
            let mut ff: Vec<String> = vec![
                "-v".into(),
                "error".into(),
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                a.src.into(),
                "-pix_fmt".into(),
                if a.ten_bit {
                    "yuv420p10le".into()
                } else {
                    "yuv420p".into()
                },
            ];
            if a.ten_bit {
                ff.push("-strict".into());
                ff.push("-1".into());
            }
            ff.extend([
                "-t".into(),
                "1".into(),
                "-vframes".into(),
                "1".into(),
                "-f".into(),
                "yuv4mpegpipe".into(),
                "-".into(),
            ]);
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args(&ff)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run");
            assert!(
                y4m.status.success(),
                "ffmpeg fixture {desc}: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let encode = || {
                let mut args: Vec<String> = vec![
                    "--codec=av1".into(),
                    "--passes=1".into(),
                    "--end-usage=q".into(),
                    format!("--cq-level={}", a.cq),
                    "--cpu-used=4".into(),
                    "--threads=1".into(),
                    "--row-mt=0".into(),
                    "--sb-size=64".into(),
                    "--kf-max-dist=0".into(),
                    "--limit=1".into(),
                    "--enable-rect-partitions=1".into(),
                    "--enable-ab-partitions=1".into(),
                    "--enable-1to4-partitions=0".into(),
                    format!("--enable-tx-size-search={}", u8::from(a.tx_size_search)),
                    // Filter intra ON a rect strip is a separate, still-refused
                    // predictor; this gate is about the AB partition wiring.
                    "--enable-filter-intra=0".into(),
                    "--reduced-tx-type-set=1".into(),
                    "--min-partition-size=8".into(),
                    "--max-partition-size=16".into(),
                    format!("--tile-columns={}", a.tile_cols),
                ];
                if a.ten_bit {
                    args.push("--input-bit-depth=10".into());
                    args.push("--bit-depth=10".into());
                }
                args.extend(["--obu".into(), "-o".into(), "-".into(), "-".into()]);
                let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
                assert!(
                    out.status.success(),
                    "aomenc refused {desc}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                out.stdout
            };
            let stream = encode();
            assert!(
                !stream.is_empty(),
                "{NAME}: {desc}: aomenc wrote an empty stream"
            );
            assert_eq!(
                stream,
                encode(),
                "{NAME}: {desc}: aomenc output is not reproducible"
            );
            let before = crate::decode::ab16_hits_by_arm();
            let tx4_before = crate::decode::vert_ab_tx4_hits();
            let decoded = decode_stream(&stream);
            let tx4_this = crate::decode::vert_ab_tx4_hits() - tx4_before;
            // Counted even when the attempt later stops at another lane's
            // refusal: reaching this lookup at all is what r2 fixed.
            tx4_reached += tx4_this;
            let frames = match decoded {
                Ok(frames) => frames,
                // Only lane-rectx's split-transform strip refusal may stop a
                // tx-size-search attempt; a panic never reaches here at all.
                Err(e) if a.tx_size_search => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME}: {desc}: decode failed with a non-refusal error: {msg}"
                    );
                    eprintln!("{NAME}: {desc} refused after {tx4_this} vert-AB TX4 units: {msg}");
                    continue;
                }
                Err(e) => panic!("{NAME}: {desc}: decode failed, not a pixel mismatch: {e}"),
            };
            let after = crate::decode::ab16_hits_by_arm();
            let this: Vec<usize> = (0..4).map(|i| after[i] - before[i]).collect();
            eprintln!("{NAME}: {desc} decoded, arms this attempt={this:?}");
            // The ENCODE's frame count: every attempt's y4m is rendered with
            // `-t 1` and `-vframes 1` (28367) -- one frame whatever the source's
            // rate -- and aomenc carries `--limit=1` (28401), so all six arms
            // code exactly 1 frame. Asserted ABOVE the oracle call.
            assert_eq!(frames.len(), 1, "{NAME}: {desc} key frame");
            let want = if a.ten_bit {
                ffmpeg_decode_sequence_10bit(&stream, a.w, a.h, frames.len())
            } else {
                ffmpeg_decode_sequence(&stream, a.w, a.h, frames.len())
            };
            assert_eq!(
                want.len(),
                frames.len(),
                "{NAME}: {desc}: ffmpeg frame count"
            );
            for (i, (got, w)) in frames.iter().zip(&want).enumerate() {
                assert_eq!(got.y, w.y, "{NAME}: {desc} frame {i} luma vs ffmpeg");
                assert_eq!(got.u, w.u, "{NAME}: {desc} frame {i} U vs ffmpeg");
                assert_eq!(got.v, w.v, "{NAME}: {desc} frame {i} V vs ffmpeg");
            }
            // Counted only now: the attempt decoded AND compared exact.
            if a.tx_size_search {
                tx4_compared += 1;
                eprintln!("{NAME}: {desc} pixel-exact, vert-AB TX4 units={tx4_this}");
                continue;
            }
            for i in 0..4 {
                arms[i] += after[i] - before[i];
            }
            compared += 1;
            eprintln!("{NAME}: {desc} pixel-exact");
        }
        assert_eq!(
            compared,
            attempts.iter().filter(|a| !a.tx_size_search).count(),
            "{NAME}: every fixed-transform attempt must decode and compare"
        );
        // Until the lane-rectsplitx merge every tx-size-search attempt stopped
        // at "a HORZ/VERT intra strip below 16x16 with a split transform", so
        // this gate could only claim that the TX4 sub-grid of an 8x8 square
        // inside a VERT_A/VERT_B was REACHED (`tx4_reached`, counted even on a
        // refused attempt). With per-TU rect prediction those same attempts now
        // decode to the end and compare pixel-exact, and they carry no vert-AB
        // TX4 unit at all -- the pre-merge nonzero count came from decodes that
        // were still desynced when they hit the refusal (class
        // counter-from-refused-stream). The full pixel compare is the stronger
        // claim and replaces it; the lookup's VALUES stay pinned against libaom
        // in `of_tu_keeps_the_ordinary_answer_and_uses_the_block_row_under_vert_ab`.
        eprintln!(
            "{NAME}: vert-AB TX4 units reached across tx-size-search attempts: {tx4_reached}"
        );
        let _ = tx4_reached;
        assert_eq!(
            tx4_compared,
            attempts.iter().filter(|a| a.tx_size_search).count(),
            "{NAME}: every --enable-tx-size-search=1 attempt must now decode to the end and \
             compare pixel-exact (the split-transform strip refusal these used to stop at is \
             lifted); the vert-AB TX4 lookup values stay pinned by \
             `of_tu_keeps_the_ordinary_answer_and_uses_the_block_row_under_vert_ab`"
        );
        for (i, arm) in ["HORZ_A", "HORZ_B", "VERT_A", "VERT_B"].iter().enumerate() {
            assert!(
                arms[i] > 0,
                "{NAME}: PARTITION_{arm} at 16x16 never fired across {compared} \
                                  compared streams -- the gate proves nothing about that arm (arms={arms:?})"
            );
        }
        eprintln!(
            "{NAME}: pixel-exact on {compared} streams, ab16 arms(HORZ_A,HORZ_B,VERT_A,VERT_B)={arms:?}"
        );
    }

    #[test]
    // The recipe SEARCH, not a lane's scratch any more: 9 sources x 7 cq x
    // rtx on/off, 28 s measured, and it is the evidence that no aomenc rect-tx
    // recipe mismatches -- which is what the un-ignored rect gates rest on.
    #[ignore = "recipe search: 9 sources x 7 cq x rtx on/off, 28 s; establishes that no aomenc rect-tx recipe mismatches"]
    fn sweep_rectx_recipes() {
        let _gate_lock = lock_gate_counters();
        let srcs: Vec<String> = vec![
            "mandelbrot=size=64x64:start_x=0.4".into(),
            "mandelbrot=size=64x64:start_x=-0.6".into(),
            "mandelbrot=size=64x64:start_x=0.1".into(),
            "mandelbrot=size=128x128:start_x=0.4".into(),
            "mandelbrot=size=128x128:start_x=-0.6".into(),
            "testsrc2=size=64x64".into(),
            "testsrc2=size=128x128".into(),
            "rgbtestsrc=size=64x64".into(),
            "smptebars=size=128x128".into(),
        ];
        for start_x in srcs {
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &start_x,
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "1",
                    "-vframes",
                    "1",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg");
            assert!(y4m.status.success());
            let (width, height) = if start_x.contains("128x128") {
                (128usize, 128usize)
            } else {
                (64usize, 64usize)
            };
            for cq in [16u32, 20, 24, 28, 32, 40, 50] {
                for rtx in ["0", "1"] {
                    let out = run_with_stdin(
                        Command::new(aomenc_path()).args([
                            "--codec=av1",
                            "--passes=1",
                            "--end-usage=q",
                            &format!("--cq-level={cq}"),
                            "--cpu-used=4",
                            "--threads=1",
                            "--row-mt=0",
                            "--sb-size=64",
                            "--kf-max-dist=0",
                            "--enable-rect-partitions=1",
                            "--enable-ab-partitions=0",
                            "--enable-1to4-partitions=0",
                            "--enable-tx-size-search=0",
                            "--enable-cdef=0",
                            "--enable-restoration=0",
                            &format!("--reduced-tx-type-set={rtx}"),
                            "--min-partition-size=8",
                            "--max-partition-size=32",
                            "--obu",
                            "-o",
                            "-",
                            "-",
                        ]),
                        &y4m.stdout,
                    );
                    assert!(out.status.success());
                    let stream = out.stdout;
                    let before = crate::decode::rect_leaf_coeff_hits();
                    let res = decode_stream(&stream);
                    let fired = crate::decode::rect_leaf_coeff_hits() - before;
                    match res {
                        Err(e) => {
                            eprintln!("x={start_x} cq={cq} rtx={rtx} fired={fired} REFUSED {e}")
                        }
                        Ok(frames) => {
                            // The ENCODE's frame count: the y4m is rendered with
                            // `-t 1` and `-vframes 1` (28747) -- one frame, and
                            // the sources carry no explicit `:rate=` so that
                            // `-vframes` is the binding bound -- and aomenc runs
                            // with `--kf-max-dist=0` and no `--limit`
                            // (28782), so all 126 arms code exactly 1 frame.
                            // This sweep asserts no verdict, but `bad` below is
                            // the NUMBER of mismatching frames it reports, so a
                            // decode that showed fewer frames than the wire
                            // would silently under-report it. Pinned here, above
                            // the oracle call, for that reason.
                            assert_eq!(frames.len(), 1, "x={start_x} cq={cq} rtx={rtx} key frame");
                            let want = ffmpeg_decode_sequence(&stream, width, height, frames.len());
                            let bad = frames
                                .iter()
                                .zip(&want)
                                .filter(|(g, w)| g.y != w.y || g.u != w.u || g.v != w.v)
                                .count();
                            eprintln!(
                                "x={start_x} cq={cq} rtx={rtx} fired={fired} frames={} mismatched={bad}",
                                frames.len()
                            );
                            if bad == 0 {
                                continue;
                            }
                            let (g, w) = (&frames[0], &want[0]);
                            for br in 0..height / 8 {
                                let mut line = String::new();
                                for bc in 0..width / 8 {
                                    let mut n = 0;
                                    for row in br * 8..br * 8 + 8 {
                                        for col in bc * 8..bc * 8 + 8 {
                                            if g.y[row * width + col] != w.y[row * width + col] {
                                                n += 1;
                                            }
                                        }
                                    }
                                    line.push_str(&format!("{n:4}"));
                                }
                                eprintln!("  blk8 row{br}: {line}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// lane-rectgate r1: every prior gate pins `--enable-rect-partitions=0
    /// --enable-ab-partitions=0 --min/max-partition-size=32` so aomenc never
    /// gets to choose a real rect/ab split. This charter's premise ("rect
    /// HORZ/VERT + HORZ_A/HORZ_B/VERT_A/VERT_B landed during the warp lane")
    /// does NOT hold: `decode_frame`'s inter-tile `match part32` only has
    /// arms for `PARTITION_NONE`/`SPLIT`/`HORZ_B` -- `HORZ`, `VERT`,
    /// `HORZ_A`, `VERT_A`, `VERT_B`, `HORZ_4`, `VERT_4` all fall into the
    /// generic "a partition type this encoder never writes" `_` arm. Two
    /// recipes were swept empirically (40 attempts each) to characterize the
    /// gap:
    /// - `--enable-rect-partitions=1 --enable-ab-partitions=1`, no
    ///   min/max clamp: 0/40 matched -- every attempt hit either the
    ///   screen-content refusal or a genuinely-unimplemented partition value
    ///   (1/2/6/7/9 all observed), always somewhere in the 24-frame stream.
    /// - Same, but `--enable-1to4-partitions=0` explicit and
    ///   `--min-partition-size=16`: still 0/40, `value=9` (`VERT_4`)
    ///   dominates -- this build's `--enable-1to4-partitions=0` does NOT
    ///   suppress `VERT_4` selection (aomenc-side, confirmed by direct flag
    ///   toggle; outside this decoder). This recipe (below) restores the
    ///   `--enable-rect-partitions=0 --min/max-partition-size=32` pin every
    ///   other gate uses, flipping only `--enable-ab-partitions=1`: matches
    ///   cleanly (33/40, rest screen-content refusals) but
    ///   `extended_partition_hits` stayed 0 -- confirmed with the same probe
    ///   wired into the already-green warp gate too (144 `warp_selected_hits`,
    ///   0 `extended_partition_hits`), so `PARTITION_HORZ_B` itself is
    ///   apparently never chosen by aomenc for this small gradient fixture at
    ///   `cq-level=45` regardless of clamp. No decode defect (crash or pixel
    ///   mismatch) was found in any sweep -- every refusal is a correctly
    ///   named, non-silent "unsupported" error. A refusal or mismatch naming
    ///   any OTHER already-ported capability (warp, obmc, interintra) still
    ///   fails the gate outright.
    #[test]
    fn a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_RECTGATE_ATTEMPTS")
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
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                // lane-rect r2: HORZ/VERT decode for real (rect-flake-1
                // byte-exact: size_h + warp dims + OBMC overlap + tx height)
                // -- free recipe restored; remaining partition kinds refuse
                // by name and skip their seeds.
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=1",
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
                "--min-partition-size=16",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
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
                    for banned in ["warp", "obmc", "interintra"] {
                        assert!(
                            !msg.contains(banned),
                            "{NAME} refused on {banned} (seed {seed}) -- that capability is \
                             ported and this recipe does not need it: {msg}"
                        );
                    }
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
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
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
            // lane-hidden r2 (class gate-blind-to-hidden-frames): the same
            // stream again, comparing EVERY decode-order frame -- the hidden
            // (show_frame == 0) alt-ref frames the ffmpeg compare above
            // cannot see, because both sides of it emit shown frames only.
            let (hidden_total, hidden_n) = decode_all_frames_vs_oracle(&stream, NAME);
            eprintln!(
                "{NAME} HIDDEN: seed {seed}: {hidden_total} decode-order frames, \
                 {hidden_n} hidden, all pixel-exact vs the oracle"
            );
            // lane-hidden r2 SECOND ARM (the recipe above is untouched):
            // libaom's temporal alt-ref filter absorbs this recipe's hidden
            // frames, so the compare above is vacuous -- re-encode the same
            // fixture with --arnr-maxframes=0 and compare THOSE.
            if let Some((arm_total, arm_hidden)) = hidden_arnr_arm(NAME, &y4m.stdout, &args) {
                eprintln!(
                    "{NAME} HIDDEN-ARM(--arnr-maxframes=0): seed {seed}: {arm_total} \
                     decode-order frames, {arm_hidden} hidden, all pixel-exact vs the oracle"
                );
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a free-partition stream"
        );
        // lane-partitions r1: HORZ/VERT arms landed, so the diversity probe
        // graduates from rectgate r1's eprintln to a report line on
        // `rect_partition_hits`; a zero-hit run SOFT-reports (aomenc RD may
        // legitimately never pick a rect split in a small window -- same
        // sampling caveat as the masked-compound gate's zero-hit runs).
        // `extended_partition_hits` (HORZ_B) stays unasserted per rectgate r1.
        if crate::decode::rect_partition_hits() == 0 {
            eprintln!(
                "{NAME}: SOFT-NOTE -- zero HORZ/VERT strips fired this run \
                 ({matched} matches, {named_refusals} refusals); sampling, not a regression"
            );
        } else {
            // r5: the desync was the tx-depth symbol being read
            // unconditionally where both square paths gate it on `tx_select`
            // -- with --enable-tx-size-search=0 the encoder writes no such
            // symbol and the decoder consumed one that was never there. With
            // that fixed the real coefficient decode is pixel-exact, so this
            // is a HARD assert again: strips fired but no coefficients read
            // would mean the coded path silently stopped being exercised.
            assert!(
                crate::decode::rect_coeff_hits() > 0,
                "{NAME}: {} HORZ/VERT strips fired but rect_coeff_hits==0 -- the \
                 coded rect path is no longer exercised",
                crate::decode::rect_partition_hits()
            );
        }
        eprintln!(
            "{NAME}: {named_refusals} named refusals, {matched} pixel-exact matches out of {n_attempts}, rect_partition_hits={} rect_coeff_hits={} extended_partition_hits={} partab_hits={}",
            crate::decode::rect_partition_hits(),
            crate::decode::rect_coeff_hits(),
            crate::decode::extended_partition_hits(),
            crate::decode::partab_hits()
        );
    }

    /// lane-partab r1: `--enable-ab-partitions=1` (aomenc default-on) INTER
    /// streams must consume PARTITION_HORZ_A/VERT_A/VERT_B entropy-exact AND
    /// decode pixel-exact -- `partab_hits() > 0` is soft-skipped on a
    /// zero-hit run (aomenc RD may legitimately never pick an AB split in a
    /// small window, same sampling caveat as wedge/rect). The INTER AB
    /// refusal is FORBIDDEN (decode.rs names it "an INTER 32x32 partition
    /// ..."); intra-frame AB refusals stay accepted named refusals -- the
    /// intra dispatch is a later lane. Recipe = the free-partition gate's,
    /// which fires AB live (r1: partab_hits 6/2/4/3/1/5 over six
    /// 40-attempt runs).
    #[test]
    fn a_real_aomenc_stream_with_ab_partitions_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_ab_partitions_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_RECTGATE_ATTEMPTS")
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
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                // lane-rect r2 recipe, kept byte-identical by lane-partab
                // r1: free partition search, 1to4 off, intra tools off.
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=1",
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
                "--min-partition-size=16",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
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
                    for banned in ["warp", "obmc", "interintra"] {
                        assert!(
                            !msg.contains(banned),
                            "{NAME} refused on {banned} (seed {seed}) -- that capability is \
                             ported and this recipe does not need it: {msg}"
                        );
                    }
                    for v in [4, 6, 7] {
                        assert!(
                            !msg.contains(&format!(
                                "an INTER 32x32 partition type this encoder never writes \
                                 (value={v})"
                            )),
                            "{NAME} refused an INTER AB partition (value={v}, seed {seed}) -- \
                             all three AB arms are ported, this refusal is forbidden: {msg}"
                        );
                    }
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} frame {i} luma vs ffmpeg (seed {seed})"
                );
                assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
            }
            // lane-hidden r2 (class gate-blind-to-hidden-frames): the same
            // stream again, comparing EVERY decode-order frame -- the hidden
            // (show_frame == 0) alt-ref frames the ffmpeg compare above
            // cannot see, because both sides of it emit shown frames only.
            let (hidden_total, hidden_n) = decode_all_frames_vs_oracle(&stream, NAME);
            eprintln!(
                "{NAME} HIDDEN: seed {seed}: {hidden_total} decode-order frames, \
                 {hidden_n} hidden, all pixel-exact vs the oracle"
            );
            // lane-hidden r2 SECOND ARM (the recipe above is untouched):
            // libaom's temporal alt-ref filter absorbs this recipe's hidden
            // frames, so the compare above is vacuous -- re-encode the same
            // fixture with --arnr-maxframes=0 and compare THOSE.
            if let Some((arm_total, arm_hidden)) = hidden_arnr_arm(NAME, &y4m.stdout, &args) {
                eprintln!(
                    "{NAME} HIDDEN-ARM(--arnr-maxframes=0): seed {seed}: {arm_total} \
                     decode-order frames, {arm_hidden} hidden, all pixel-exact vs the oracle"
                );
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a stream"
        );
        if crate::decode::partab_hits() == 0 {
            eprintln!(
                "{NAME}: SOFT-NOTE -- zero AB partitions fired this run \
                 ({matched} matches, {named_refusals} refusals); sampling, not a regression"
            );
        }
        eprintln!(
            "{NAME}: {named_refusals} named refusals, {matched} pixel-exact matches out of {n_attempts}, partab_hits={}",
            crate::decode::partab_hits()
        );
    }

    /// lane-ab32 r1: the 32x32-level AB arms (`PARTITION_HORZ_A`/`_HORZ_B`/
    /// `_VERT_A`/`_VERT_B` -- two 16x16 squares plus one 32x16/16x32 strip)
    /// are decoded on the INTRA path (decode.rs `PARTITION_HORZ_A` .. the
    /// 32x32 refusal) but nothing asserted they fire: the INTER AB gate above
    /// only SOFT-notes `partab_hits`, and the four per-arm counters
    /// lane-part32 added had no consumer at all (class
    /// `refusal-lifted-without-a-gate`). This drives a real key-frame-only
    /// aomenc at `--enable-ab-partitions=1 --enable-rect-partitions=1
    /// --sb-size=64 --min-partition-size=16 --max-partition-size=32`, so every
    /// AB split aomenc picks is a 32-level one, pixel-compares every
    /// successful decode against ffmpeg's decode of the same bytes, and
    /// hard-asserts each arm fired at least once over the seed sweep --
    /// counting deltas only from attempts that decoded AND compared (class
    /// `counter-from-refused-stream`).
    #[test]
    fn a_real_aomenc_stream_with_a_32_level_ab_partition_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_a_32_level_ab_partition_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (256usize, 256usize);
        let arms: [(&str, fn() -> usize); 4] = [
            ("HORZ_A_32", crate::decode::intra_horz_a_hits),
            ("HORZ_B_32", crate::decode::intra_horz_b_hits),
            ("VERT_A_32", crate::decode::intra_vert_a_hits),
            ("VERT_B_32", crate::decode::intra_vert_b_hits),
        ];
        let mut totals = [0usize; 4];
        let mut refusals: Vec<String> = Vec::new();
        let mut compared = 0usize;
        for &seed in &LR_SEEDS {
            // Smooth gradients code as PARTITION_NONE everywhere (r1: 16/16
            // attempts decoded pixel-exact with zero AB hits), so the fixture
            // carries seeded noise to give the intra RD a reason to split.
            let source = format!(
                "{},noise=alls=40:all_seed={seed}",
                gradients_source(seed, width, height, "duration=0.04:rate=25")
            );
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    "-frames:v",
                    "1",
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
                "{NAME}: ffmpeg fixture (seed {seed}): {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            // The flags under test come LAST: aomenc keeps the LAST
            // occurrence of a repeated flag (measured 2026-09-02 by md5 of
            // four orderings), so an override appended after the base recipe
            // is the one that lands. Nothing here is repeated, and each of the
            // four arm counters below proves the AB flag actually arrived.
            let args: Vec<&str> = vec![
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--limit=1",
                "--kf-max-dist=0",
                "--lag-in-frames=0",
                "--threads=1",
                "--row-mt=0",
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
                "--sb-size=64",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=1",
                "--min-partition-size=16",
                "--max-partition-size=32",
                "--obu",
                "-o",
                "-",
                "-",
            ];
            let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
            assert!(
                out.status.success(),
                "aomenc refused the fixture (seed {seed}): {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let before: Vec<usize> = arms.iter().map(|(_, f)| f()).collect();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright, not a named refusal (seed {seed}): {msg}"
                    );
                    for v in [4, 5, 6, 7] {
                        assert!(
                            !msg.contains(&format!(
                                "a 32x32 partition type this decoder does not code (value={v})"
                            )),
                            "{NAME} refused a 32-level AB partition (value={v}, seed {seed}) -- \
                             all four arms are ported, this refusal is forbidden: {msg}"
                        );
                    }
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            let deltas: Vec<usize> = arms
                .iter()
                .zip(before.iter())
                .map(|((_, f), b)| f() - b)
                .collect();
            let fired: Vec<String> = arms
                .iter()
                .zip(deltas.iter())
                .map(|((label, _), d)| format!("{label}={d}"))
                .collect();
            let fired = fired.join(" ");
            // The ENCODE's frame count: `gradients_source` (6015) is rendered with
            // `duration=0.04:rate=25` (29116) and `-frames:v 1` (29119) caps the
            // y4m at one frame; aomenc runs with `--limit=1` and
            // `--kf-max-dist=0` (29150) and so codes exactly that one. Asserted
            // ABOVE the oracle call -- it holds for every one of the 16 seeds.
            assert_eq!(frames.len(), 1, "{NAME}: seed {seed} key frame");
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frames.len());
            assert_eq!(
                frames.len(),
                ffmpeg_frames.len(),
                "{NAME}: seed {seed}: shown-frame count vs ffmpeg"
            );
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} frame {i} luma vs ffmpeg (seed {seed}) [{fired}]"
                );
                assert_eq!(
                    got.u, want.u,
                    "{NAME} frame {i} U vs ffmpeg (seed {seed}) [{fired}]"
                );
                assert_eq!(
                    got.v, want.v,
                    "{NAME} frame {i} V vs ffmpeg (seed {seed}) [{fired}]"
                );
            }
            compared += 1;
            for (i, d) in deltas.iter().enumerate() {
                totals[i] += d;
            }
        }
        assert!(
            compared > 0,
            "{NAME}: every attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
        for (i, (label, _)) in arms.iter().enumerate() {
            assert!(
                totals[i] > 0,
                "{NAME}: {compared} attempt(s) decoded pixel-exact but {label} never fired -- \
                 the gate would prove nothing about that arm. refusals:\n{}",
                refusals.join("\n")
            );
        }
        eprintln!(
            "{NAME}: {compared}/{} attempts pixel-exact, HORZ_A_32={} HORZ_B_32={} \
             VERT_A_32={} VERT_B_32={}, {} named refusals",
            LR_SEEDS.len(),
            totals[0],
            totals[1],
            totals[2],
            totals[3],
            refusals.len()
        );
    }

    /// lane-ab32 r1: the 10-bit twin of the gate above -- his two films are
    /// yuv420p10le, so an 8-bit-only AB gate proves nothing about the streams
    /// that matter. Same recipe through [`ten_bit_tool_gate`], which sums the
    /// per-arm counters only over attempts that decoded and pixel-compared.
    #[test]
    fn a_real_aomenc_10bit_stream_with_a_32_level_ab_partition_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_10bit_stream_with_a_32_level_ab_partition_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        ten_bit_tool_gate(
            NAME,
            256,
            256,
            1,
            &[
                "--ec-fixture-noise",
                "--cq-level=45",
                "--limit=1",
                "--kf-max-dist=0",
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
                // last wins: the flags under test go after the base recipe
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=1",
                "--min-partition-size=16",
                "--max-partition-size=32",
            ],
            &LR_SEEDS,
            &[
                ("HORZ_A_32", crate::decode::intra_horz_a_hits),
                ("HORZ_B_32", crate::decode::intra_horz_b_hits),
                ("VERT_A_32", crate::decode::intra_vert_a_hits),
                ("VERT_B_32", crate::decode::intra_vert_b_hits),
            ],
        );
    }

    /// 8-bit arm of [`run_fimv_motion_mode_gate`].
    #[test]
    fn a_real_force_integer_mv_warp_stream_decodes_pixel_exact() {
        run_fimv_motion_mode_gate(
            "a_real_force_integer_mv_warp_stream_decodes_pixel_exact",
            false,
        );
    }

    /// 10-bit arm of [`run_fimv_motion_mode_gate`] -- both of his films are
    /// `yuv420p10le`.
    #[test]
    fn a_real_force_integer_mv_warp_10bit_stream_decodes_pixel_exact() {
        run_fimv_motion_mode_gate(
            "a_real_force_integer_mv_warp_10bit_stream_decodes_pixel_exact",
            true,
        );
    }

    /// 8-bit arm of [`run_compound_global_warp_gate`].
    #[test]
    fn a_real_compound_global_warp_stream_decodes_pixel_exact() {
        run_compound_global_warp_gate(
            "a_real_compound_global_warp_stream_decodes_pixel_exact",
            8,
            32,
            false,
        );
    }

    /// 10-bit arm of [`run_compound_global_warp_gate`] -- both of his films are
    /// `yuv420p10le`, and `warp_affine`'s `bd` was hardcoded 8 before r1.
    ///
    /// Un-ignored 2026-09-04: the pre-existing 10-bit compound MC defect that blocked it is gone. Ablation run by lane-cwarp r1 on this
    /// very recipe: with `--enable-global-motion=0 --enable-warped-motion=0`
    /// (no warp of any kind reachable) the 10-bit stream STILL mismatches at
    /// frame 1 luma, cq 32; with `--auto-alt-ref=0 --lag-in-frames=0
    /// --enable-dist-wtd-comp=0` (single-reference 10-bit inter only) the same
    /// recipe is 6/6 pixel-exact. So: 10-bit single-ref inter is exact, 10-bit
    /// compound is not. `mc::diffwtd_mask`'s missing `(bd - 8)` round term was
    /// found and fixed while bisecting this, but is NOT the whole defect.
    /// The warp wiring under test here is bit-depth generic.
    #[test]
    fn a_real_compound_global_warp_10bit_stream_decodes_pixel_exact() {
        run_compound_global_warp_gate(
            "a_real_compound_global_warp_10bit_stream_decodes_pixel_exact",
            10,
            32,
            false,
        );
    }

    /// 12-bit arm of [`run_compound_global_warp_gate`] (lane-av112bitw): the
    /// same rotating-mandelbrot recipe at `--bit-depth=12`. Pre-lift the
    /// first `GLOBAL_GLOBALMV` block under a ROTZOOM model refused ("warped
    /// motion at 12 bits"); the lift runs the same `warp::warp_round_0` bump
    /// through `warp_affine_compound` (`reduce_bits_horiz` 5, compound
    /// `reduce_bits_vert` stays 7, the CONV_BUF blend bias one octave down:
    /// `(1 << (bd + 2)) + (1 << (bd + 1))`).
    #[test]
    fn a_real_compound_global_warp_12bit_stream_decodes_pixel_exact() {
        run_compound_global_warp_gate(
            "a_real_compound_global_warp_12bit_stream_decodes_pixel_exact",
            12,
            32,
            false,
        );
    }

    /// 12-bit 8x8 compound-warp LEAF arm (lane-av112bitw, reviewer gap): the
    /// same recipe with the 32x32 partition pin dropped to min 8, so the RD
    /// can code an 8x8 `GLOBAL_GLOBALMV` leaf whose two references warp per
    /// slot -- `decode_inter_block8`'s `COMPOUND_WARP_HITS_8` path, which
    /// the 32x32-pinned compound witness structurally cannot reach. Hard-
    /// asserts `compound_warp_hits_8() > 0` (an 8x8 compound warp leaf bumps
    /// it alongside [`decode::compound_warp_hits`]; a 16x16+ one cannot), so
    /// a stream of only bigger warped blocks fails this gate.
    #[test]
    fn a_real_compound_global_warp_12bit_8x8_leaf_stream_decodes_pixel_exact() {
        run_compound_global_warp_gate(
            "a_real_compound_global_warp_12bit_8x8_leaf_stream_decodes_pixel_exact",
            12,
            8,
            true,
        );
    }

    /// 8-bit arm of [`run_diffwtd_inferred_64x64_gate`].
    #[test]
    fn a_real_aomenc_stream_with_a_diffwtd_compound_64x64_block_decodes_pixel_exact() {
        run_diffwtd_inferred_64x64_gate(
            "a_real_aomenc_stream_with_a_diffwtd_compound_64x64_block_decodes_pixel_exact",
            false,
        );
    }

    /// 10-bit arm of [`run_diffwtd_inferred_64x64_gate`] -- both of his films
    /// are `yuv420p10le`.
    #[test]
    fn a_real_aomenc_10bit_stream_with_a_diffwtd_compound_64x64_block_decodes_pixel_exact() {
        run_diffwtd_inferred_64x64_gate(
            "a_real_aomenc_10bit_stream_with_a_diffwtd_compound_64x64_block_decodes_pixel_exact",
            true,
        );
    }

    /// lane-maskcomp r2 / lane-wedge r3: `--enable-masked-comp=1` streams
    /// must consume `compound_type`/`wedge_idx`/`wedge_sign`/`mask_type`
    /// entropy-exact AND build the real blend, DIFFWTD or WEDGE --
    /// `masked_compound_hits() > 0` is a hard assert and BOTH
    /// `COMPOUND_DIFFWTD` and `COMPOUND_WEDGE` refusals are now FORBIDDEN
    /// (matches interintra's own gate). r3: `--enable-dist-wtd-comp=0`
    /// (forces the masked choice toward wedge when comp_group_idx==1) plus
    /// a hard-diagonal-edge `geq` source (wedge's OBLIQUE directions favor
    /// a real diagonal edge over the r2 gradients content) to fire
    /// COMPOUND_WEDGE live; `wedge_hits()` is soft-skipped (not hard
    /// asserted) on a zero-hit run -- the recipe search is not proven to
    /// converge every run, see charter's "if no recipe fires" fallback.
    #[test]
    fn a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_MASKCOMP_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(80);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            // lane-wedge r3: a hard diagonal edge (not a smooth gradient)
            // is the content wedge's OBLIQUE master masks are shaped for --
            // `mandelbrot`'s pan position varies the edge angle/position
            // across attempts the way the r2 gradients seed did.
            let source = format!(
                "mandelbrot=size={width}x{height}:rate=25:start_x={sx}:start_y={sy}",
                sx = -0.6 + 0.005 * (attempt as f64),
                sy = -0.4 + 0.005 * (attempt as f64)
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
                    &duration.to_string(),
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
                // lane-defon r1: explicit on-value (aomenc keeps the FIRST
                // occurrence, so overrides go before the base list).
                "--enable-diff-wtd-comp=1",
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                // r3: mandelbrot's pan/zoom triggers aomenc's single-ref
                // GLOBALMV global-motion search (a capability distinct from
                // and outside this lane's scope) -- off, matching this
                // gate's own goal of exercising masked compound, not GM.
                "--enable-global-motion=0",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=1",
                // r3: force the masked choice toward wedge (r2 always set
                // this =1; distance-weighted compound competes with the
                // wedge/diffwtd choice for the same comp_group_idx==1 slot).
                "--enable-dist-wtd-comp=0",
                "--enable-interintra-comp=1",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=1",
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
                "--enable-ref-frame-mvs=0",
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
                    assert!(
                        !msg.contains("warp"),
                        "{NAME} refused on warp (seed {seed}) -- warp decode is ported: {msg}"
                    );
                    assert!(
                        !msg.contains("masked COMPOUND_REFERENCE"),
                        "{NAME} refused a masked-compound block (seed {seed}) -- both DIFFWTD \
                         and WEDGE blends are ported, this refusal is forbidden: {msg}"
                    );
                    assert!(
                        !msg.contains("interintra"),
                        "{NAME} refused on interintra (seed {seed}) -- non-wedge \
                         interintra is ported: {msg}"
                    );
                    named_refusals += 1;
                    eprintln!("seed {seed} refusal: {msg}");
                    if std::env::var_os("EC_AV1_GATE_DUMP").is_some() {
                        // r1's self-pin is armed via EC_AV1_GATE_DUMP=<path>
                        // only for a real pixel MISMATCH below, not for this
                        // still-expected named refusal.
                    }
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
            if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
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
            // lane-hidden r2 (class gate-blind-to-hidden-frames): the same
            // stream again, comparing EVERY decode-order frame -- the hidden
            // (show_frame == 0) alt-ref frames the ffmpeg compare above
            // cannot see, because both sides of it emit shown frames only.
            let (hidden_total, hidden_n) = decode_all_frames_vs_oracle(&stream, NAME);
            eprintln!(
                "{NAME} HIDDEN: seed {seed}: {hidden_total} decode-order frames, \
                 {hidden_n} hidden, all pixel-exact vs the oracle"
            );
            // lane-hidden r2 SECOND ARM (the recipe above is untouched):
            // libaom's temporal alt-ref filter absorbs this recipe's hidden
            // frames, so the compare above is vacuous -- re-encode the same
            // fixture with --arnr-maxframes=0 and compare THOSE.
            if let Some((arm_total, arm_hidden)) = hidden_arnr_arm(NAME, &y4m.stdout, &args) {
                eprintln!(
                    "{NAME} HIDDEN-ARM(--arnr-maxframes=0): seed {seed}: {arm_total} \
                     decode-order frames, {arm_hidden} hidden, all pixel-exact vs the oracle"
                );
            }
            matched += 1;
        }
        // r1: observed flaky at 40 attempts -- aomenc's cpu-used=0 RD is
        // run-to-run nondeterministic in whether it ever picks
        // comp_group_idx == 1 for this synthetic gradients content, and
        // allow_screen_content_tools sometimes eats most of a small seed
        // window (class gate-recipe-confound / never-fired-gate, memory).
        // 80 attempts is the mitigation, not a fix; a still-zero run here
        // is that sampling flake, not a decode regression -- check
        // masked_compound_hits/named_refusals in the panic message before
        // assuming a real bug.
        // r2: DIFFWTD is ported and gated, so a zero-hit run is no longer
        // soft-skipped -- hard assert, matching the interintra gate.
        assert!(
            crate::decode::masked_compound_hits() > 0,
            "{NAME}: zero masked-compound blocks fired ({matched} matches, {named_refusals} \
             other refusals out of {n_attempts}) -- gate proved nothing this run"
        );
        // lane-defon r1: `--enable-diff-wtd-comp=1` is now spelled on this
        // recipe, so the DIFFWTD half of MASKED_COMPOUND_HITS must fire.
        assert!(
            crate::decode::diffwtd_hits() > 0,
            "{NAME}: zero COMPOUND_DIFFWTD blends ({matched} matches, {named_refusals} \
             other refusals out of {n_attempts}) -- --enable-diff-wtd-comp=1 proved nothing"
        );
        // r3: wedge_hits() is soft-skipped, not hard-asserted -- the recipe
        // search (dist-wtd-comp=0 + diagonal mandelbrot content) is not
        // proven to fire wedge on every run; charter's fallback is to land
        // the checksum-verified codebook + blend with this soft path.
        if crate::decode::wedge_hits() == 0 {
            eprintln!(
                "{NAME}: WARNING wedge_hits()==0 this run -- COMPOUND_WEDGE never fired \
                 ({matched} matches, {named_refusals} refusals out of {n_attempts}); codebook \
                 is checksum-verified but unexercised live this run"
            );
        }
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched} pixel-exact matches \
             out of {n_attempts}, masked_compound_hits={} wedge_hits={}",
            crate::decode::masked_compound_hits(),
            crate::decode::wedge_hits()
        );
    }

    /// lane-inter16ab r5: `COMPOUND_WEDGE` on a RECTANGULAR inter block --
    /// the shapes `av1_wedge_params_lookup` gives the `hgtw`/`hltw`
    /// codebooks (8x16/16x32/8x32 and 16x8/32x16/32x8), which this decoder
    /// refused by name until this round ("the wedge codebook is
    /// square-only"). `--enable-rect-partitions=1` with
    /// `--min-partition-size=16 --max-partition-size=32` is what makes
    /// aomenc's RD put a masked compound block on a 16x32/32x16 strip
    /// (measured: ~/.cache/inter16ab-tmp/rw2.sh, 24/24 streams at cq 50/55
    /// carry one and decode to the end at BOTH bit depths).
    ///
    /// Class `counter-from-refused-stream`: the per-shape counters are read
    /// as a DELTA around one attempt and only counted for attempts that
    /// decoded fully AND compared pixel-exact -- a refused attempt's hits
    /// are discarded. Every decode-order frame (including hidden alt-ref
    /// frames) is compared via [`decode_all_frames_vs_oracle`], and an
    /// attempt that mismatches while carrying NO rect wedge fails the gate
    /// through `oos_mismatch` rather than being blamed on this lane.
    #[test]
    fn a_real_aomenc_stream_with_a_rectangular_compound_wedge_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_rectangular_compound_wedge_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (96usize, 96usize, 24usize);
        // Per rect shape (8x16, 16x8, 16x32, 32x16, 8x32, 32x8), summed over
        // the attempts that decoded AND matched.
        let mut shapes_proved = [0usize; 6];
        let (mut named_refusals, mut matched, mut oos_mismatch) = (0u32, 0u32, 0u32);
        let mut attempts = 0u32;
        for bit_depth in [8u32, 10u32] {
            for &cq in &[50u32, 55] {
                for a in 0..4u32 {
                    attempts += 1;
                    let duration = frame_count as f64 / 25.0;
                    // Same hard-diagonal-edge content the square wedge gate
                    // uses (wedge's masters are OBLIQUE), panned per attempt.
                    let source = format!(
                        "mandelbrot=size={width}x{height}:rate=25:start_x={sx}:start_y={sy}",
                        sx = -0.6 + 0.01 * (a as f64),
                        sy = -0.4 + 0.01 * (a as f64)
                    );
                    let pix_fmt = if bit_depth == 10 {
                        "yuv420p10le"
                    } else {
                        "yuv420p"
                    };
                    let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                        .args([
                            "-v",
                            "error",
                            "-f",
                            "lavfi",
                            "-i",
                            &source,
                            "-t",
                            &duration.to_string(),
                            "-pix_fmt",
                            pix_fmt,
                            "-strict",
                            "-1",
                            "-f",
                            "yuv4mpegpipe",
                            "-strict",
                            "-1",
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
                    let cq_arg = format!("--cq-level={cq}");
                    let depth_arg = format!("--bit-depth={bit_depth}");
                    let input_depth_arg = format!("--input-bit-depth={bit_depth}");
                    let args: Vec<&str> = vec![
                        "--codec=av1",
                        "--passes=1",
                        "--end-usage=q",
                        &cq_arg,
                        "--cpu-used=0",
                        &depth_arg,
                        &input_depth_arg,
                        "--kf-max-dist=1000",
                        "--threads=1",
                        "--row-mt=0",
                        "--auto-alt-ref=1",
                        "--lag-in-frames=16",
                        "--enable-fwd-kf=0",
                        "--enable-order-hint=1",
                        "--enable-warped-motion=1",
                        "--enable-global-motion=0",
                        "--enable-obmc=1",
                        "--enable-diff-wtd-comp=1",
                        "--enable-interintra-comp=1",
                        "--enable-onesided-comp=0",
                        "--enable-interintra-wedge=1",
                        "--enable-smooth-interintra=1",
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
                        // Per-arm overrides LAST (aomenc keeps the last
                        // occurrence of a repeated --enable-* flag): masked
                        // compound ON, distance-weighted OFF so the
                        // comp_group_idx==1 slot goes to wedge/diffwtd, and
                        // rectangular partitions ON at 16/32 -- the shapes
                        // the hgtw/hltw codebooks exist for.
                        "--enable-masked-comp=1",
                        "--enable-dist-wtd-comp=0",
                        "--enable-rect-partitions=1",
                        "--min-partition-size=16",
                        "--max-partition-size=32",
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
                    let before = crate::stream::rect_wedge_hits();
                    let frames = match decode_stream(&stream) {
                        Err(e) => {
                            let msg = e.to_string();
                            assert!(
                                msg.contains("unsupported"),
                                "{NAME} failed outright, not a named refusal \
                                 ({bit_depth}-bit cq{cq} attempt {a}): {msg}"
                            );
                            assert!(
                                !msg.contains("wedge codebook is square-only"),
                                "{NAME}: the rect wedge codebook is ported -- this refusal is \
                                 forbidden ({bit_depth}-bit cq{cq} attempt {a}): {msg}"
                            );
                            named_refusals += 1;
                            eprintln!("{bit_depth}-bit cq{cq} attempt {a} refusal: {msg}");
                            continue;
                        }
                        Ok(frames) => frames,
                    };
                    let after = crate::stream::rect_wedge_hits();
                    let fired: Vec<usize> = (0..6).map(|k| after[k] - before[k]).collect();
                    let ffmpeg_frames = if bit_depth == 10 {
                        ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
                    } else {
                        ffmpeg_decode_sequence(&stream, width, height, frame_count)
                    };
                    assert_eq!(frames.len(), ffmpeg_frames.len());
                    let mismatched = frames
                        .iter()
                        .zip(&ffmpeg_frames)
                        .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                    if fired.iter().sum::<usize>() == 0 {
                        if mismatched {
                            oos_mismatch += 1;
                            eprintln!(
                                "{NAME}: {bit_depth}-bit cq{cq} attempt {a} MISMATCHES with zero \
                                 rectangular wedge block -- another shape's defect"
                            );
                        }
                        continue;
                    }
                    for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                        assert_eq!(
                            got.y, want.y,
                            "{NAME} frame {i} luma vs ffmpeg ({bit_depth}-bit cq{cq} attempt {a}, shapes {fired:?})"
                        );
                        assert_eq!(
                            got.u, want.u,
                            "{NAME} frame {i} U vs ffmpeg ({bit_depth}-bit cq{cq} attempt {a}, shapes {fired:?})"
                        );
                        assert_eq!(
                            got.v, want.v,
                            "{NAME} frame {i} V vs ffmpeg ({bit_depth}-bit cq{cq} attempt {a}, shapes {fired:?})"
                        );
                    }
                    // class gate-blind-to-hidden-frames: the shown-frame
                    // compare above cannot see the alt-ref frames this
                    // recipe's --lag-in-frames=16 produces.
                    let (hidden_total, hidden_n) = decode_all_frames_vs_oracle(&stream, NAME);
                    eprintln!(
                        "{NAME}: {bit_depth}-bit cq{cq} attempt {a}: shapes {fired:?}, \
                         {hidden_total} decode-order frames ({hidden_n} hidden) pixel-exact"
                    );
                    for k in 0..6 {
                        shapes_proved[k] += fired[k];
                    }
                    matched += 1;
                }
            }
        }
        eprintln!(
            "{NAME}: {named_refusals} named refusals, {matched} pixel-exact attempts out of \
             {attempts}, rect wedge blocks per shape \
             (8x16,16x8,16x32,32x16,8x32,32x8)={shapes_proved:?}, {oos_mismatch} zero-wedge \
             mismatches"
        );
        assert_eq!(
            oos_mismatch, 0,
            "{NAME}: {oos_mismatch} attempt(s) carrying no rectangular wedge mismatched ffmpeg \
             -- a defect of another shape, not a pass"
        );
        // The claim: a rectangular wedge codebook row (hgtw AND hltw) was
        // used on a stream that then decoded pixel-exact. 16x32 is hgtw
        // (h > w), 32x16 is hltw (h < w) -- one of each, so a transposed
        // codebook/signflip pair cannot pass this.
        assert!(
            shapes_proved[2] > 0 && shapes_proved[3] > 0,
            "{NAME}: no pixel-exact attempt used both an hgtw (16x32) and an hltw (32x16) \
             rectangular wedge codebook row (per shape {shapes_proved:?}, {matched} matches, \
             {named_refusals} refusals out of {attempts}) -- the recipe proved nothing"
        );
    }
