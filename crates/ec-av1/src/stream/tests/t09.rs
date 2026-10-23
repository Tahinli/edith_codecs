use super::*;
use super::support::*;
use super::gates_a::*;
use super::gates_b::*;
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

    /// lane-part32 r2: prints the exact mismatch geometry (block coords,
    /// got/want pixels) of the pinned seed-42 stream captured off
    /// `a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`
    /// via `EC_AV1_GATE_DUMP`. Debug-only, `#[ignore]`d.
    #[test]
    // The geometry is HARD-CODED to the seed-42 pin (192x128, one frame):
    // `ffmpeg_decode_sequence(&stream, 192, 128, 1)` plus the `width = 192` /
    // `0..128` scan bounds below. Point it at any other pin and it does not
    // say "wrong size" -- it dies inside the ffmpeg oracle with
    // `expected 1 4:2:0 frames ... left 196162560 right 36864`. Measured
    // against `fixtures/sbpart/seed42.obu` (the 192x128 pin it was written
    // for): `total luma mismatches: 0`.
    #[ignore = "debug harness: needs EC_AV1_GATE_DUMP_PIN at a 192x128 pin (the geometry is hard-coded; any other size dies in the ffmpeg oracle)"]
    fn debug_part32_r2_sbpart_pin_mismatch_geometry() {
        let path = std::env::var("EC_AV1_GATE_DUMP_PIN").expect("set EC_AV1_GATE_DUMP_PIN");
        let stream = std::fs::read(&path).expect("read pinned stream");
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        if let Ok(dump) = std::env::var("EC_DEBUG_DUMP_Y") {
            let bytes: Vec<u8> = frames[0].y.iter().map(|&v| v as u8).collect();
            std::fs::write(&dump, bytes).expect("write y dump");
        }
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 192, 128, 1);
        let got = &frames[0];
        let want = &ffmpeg_frames[0];
        let width = 192usize;
        let mut n = 0;
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (width, 0, 128, 0);
        for y in 0..128usize {
            for x in 0..width {
                let idx = y * width + x;
                if got.y[idx] != want.y[idx] {
                    n += 1;
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        eprintln!(
            "total luma mismatches: {n} bbox x=[{min_x},{max_x}] y=[{min_y},{max_y}] frame=192x128"
        );
        for y in 0..128usize {
            let mut row_first = None;
            let mut row_count = 0;
            for x in 0..width {
                let idx = y * width + x;
                if got.y[idx] != want.y[idx] {
                    if row_first.is_none() {
                        row_first = Some(x);
                    }
                    row_count += 1;
                }
            }
            if let Some(fx) = row_first {
                eprintln!("row {y}: first mismatch x={fx} count={row_count}");
            }
        }
    }

    /// lane-rect64q r1, recipe rewritten by lane-av1recipehunt: the same
    /// encoder-side situation as
    /// [`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`]
    /// -- a real superblock-level HORZ/VERT partition, so
    /// [`crate::decode::decode_block_rect64`] runs -- but with `delta_q` left
    /// ON, so that block's dequant sees a running `CURRENT_Q_IDX` that has
    /// drifted away from the frame-constant `base_q_idx`. Real aomenc's
    /// non-realtime default is `DELTA_Q_OBJECTIVE` (`av1_cx_iface.c:271`); the
    /// `--deltaq-mode=0` sibling is the same command line with that one flag
    /// added back, and this gate ASSERTS the pair differs (the control arm).
    ///
    /// **Why r1's recipe could not fire, measured (lane-av1recipehunt).** r1
    /// swept 40 attempts of `gradients` at 192x128 with `frame_count = 1` -- a
    /// single KEY frame, and that is the whole reason. A one-frame encode
    /// carries no inter frame, and `delta_q_present` needs the temporal (TPL)
    /// model to see a spatial complexity map at all. At `frame_count = 1` the
    /// sweep read ZERO `delta_q` symbol groups on every attempt: 40/40 streams
    /// had `delta_q_hits() == 0` AND `rect64_qidx_drift_hits() == 0` -- a
    /// different failure from "the drift never reached a rect64 block", and the
    /// reason r1 could not see the feature at all. The fix is a real inter
    /// sequence: 4 frames at 512x384 (`8x6` superblocks, so there IS a spatial
    /// map for the quantizer to vary over).
    ///
    /// **The recipe**, exactly (the recorded command line; `--deltaq-mode` is
    /// ABSENT, i.e. aomenc's default):
    /// ```text
    /// ffmpeg -v error -y -f lavfi -i testsrc2=size=512x384:rate=25 \
    ///        -pix_fmt yuv420p -frames:v 4 -f yuv4mpegpipe src512.y4m
    /// aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=45 --cpu-used=0 \
    ///        --threads=1 --row-mt=0 --sb-size=64 --enable-rect-partitions=1 \
    ///        --enable-ab-partitions=0 --enable-1to4-partitions=0 \
    ///        --min-partition-size=32 --max-partition-size=64 \
    ///        --enable-restoration=0 --enable-palette=0 --enable-cdef=0 \
    ///        --enable-filter-intra=0 --enable-cfl-intra=0 --enable-intrabc=0 \
    ///        --enable-tx-size-search=0 --obu -o out.obu src512.y4m
    /// ```
    /// Source `src512.y4m`: 1179730 bytes, sha256
    /// `f430d581497e4f6f7db07d0fc92476e92194e1f96c321988ec0739d475cbbe0c`.
    /// Output `crates/ec-av1/fixtures/rect64_dq_drift.obu`: 8384 bytes, sha256
    /// `094e51bb21b6caccc905417e95a03f9131b448071567d0b14ec8850799ea2501`
    /// (committed with `git add -f` past the `fixtures` gitignore, like every
    /// sibling fixture). The gate re-encodes live and byte-compares against
    /// that pin, so the pin is the expected output and not a second decode
    /// path. MEASURED on the feature arm: 48 `delta_q` symbol groups read, 17
    /// `rect64_qidx_drift_hits`, `sb_rect_hits` firing (the `inter_rect`
    /// census reads `64x32=2 32x64=2`), all 4 frames pixel-exact vs ffmpeg.
    /// On the `--deltaq-mode=0` control: 0 groups, 0 drift, also pixel-exact --
    /// so the drift is the delta_q syntax, not some unrelated arm.
    ///
    /// `--cpu-used=0` is load-bearing, measured: the same source at
    /// `--cpu-used=1` and `--cpu-used=3` reads the same 48 `delta_q` groups and
    /// drifts ZERO times (the faster presets' RD never leaves a rect64 root in
    /// a superblock whose quantizer moved). `--cq-level` is not: 45 and 55
    /// give the identical stream and the same drift count.
    #[test]
    fn a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_and_delta_q_decodes_pixel_exact()
     {
        const NAME: &str = "a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_and_delta_q_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        const FIXTURE_LEN: usize = 8384;
        const FIXTURE_FNV: u64 = 0x3d91_a216_8006_3ef6;
        let (width, height, frame_count) = (512usize, 384usize, 4usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=512x384:rate=25",
                "-pix_fmt",
                "yuv420p",
                "-frames:v",
                "4",
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
        assert_eq!(
            y4m.stdout.len(),
            1_179_730,
            "{NAME}: the testsrc2 source is not the pinned 4-frame 512x384 y4m -- `rate=` \
             and `-frames:v` are both part of this recipe's identity"
        );
        // Every flag spelled exactly once (class [[aomenc-last-flag-wins]]).
        // `delta_q` is absent ON PURPOSE: aomenc's non-realtime default is
        // DELTA_Q_OBJECTIVE, and the control arm below re-checks that by
        // ADDING `--deltaq-mode=0` back.
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
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: encoded size moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: encoded bytes moved");
        let pin = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rect64_dq_drift.obu");
        assert_eq!(
            std::fs::read(&pin).expect("fixtures/rect64_dq_drift.obu must be committed"),
            stream,
            "{NAME}: the live encode no longer matches the committed pin at {}",
            pin.display()
        );

        let before = crate::decode::rect64_qidx_drift_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the pinned stream must decode: {e}"));
        let drift = crate::decode::rect64_qidx_drift_hits() - before;
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
        assert_eq!(frames.len(), frame_count);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "{NAME} frame {i} luma vs ffmpeg");
            assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg");
            assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg");
        }
        assert!(
            crate::decode::sb_rect_hits() > 0,
            "{NAME}: zero superblock-level HORZ/VERT blocks fired -- the recipe stopped producing \
             the shape the delta_q group is read inside"
        );
        assert!(
            drift > 0,
            "{NAME}: {frame_count} frames pixel-exact but zero rect64 dequant calls ever saw \
             CURRENT_Q_IDX != base_q_idx -- delta_q was read and never moved the quantizer inside \
             a rect64 block, so this run proved nothing (class gate-blind-to-feature)"
        );

        // The control arm: the ONE flag separating this gate from the
        // always-vacuous `--deltaq-mode=0` sibling, reverted. If this ever
        // drifts too, delta_q is no longer what carries the coverage and the
        // feature arm above is not the witness it claims to be. It is also this
        // gate's own red-proof: dropping that one flag is exactly what turns
        // the drift assert above red on its own precondition.
        let mut control = args.clone();
        control.push("--deltaq-mode=0");
        let ctl = run_with_stdin(Command::new(aomenc_path()).args(&control), &y4m.stdout);
        assert!(
            ctl.status.success(),
            "aomenc refused the --deltaq-mode=0 control: {}",
            String::from_utf8_lossy(&ctl.stderr)
        );
        let before = crate::decode::rect64_qidx_drift_hits();
        decode_stream(&ctl.stdout)
            .unwrap_or_else(|e| panic!("{NAME}: the control stream must decode too: {e}"));
        let ctl_drift = crate::decode::rect64_qidx_drift_hits() - before;
        assert_eq!(
            ctl_drift, 0,
            "{NAME}: the --deltaq-mode=0 control drifted {ctl_drift} times -- delta_q is no \
             longer what moves CURRENT_Q_IDX inside a rect64 block, so the feature arm above is \
             not witnessing what it names"
        );
        eprintln!(
            "{NAME}: {frame_count} frames pixel-exact, sb_rect_hits={}, \
             rect64_qidx_drift_hits={drift} (--deltaq-mode=0 control: {ctl_drift}), stream \
             {FIXTURE_LEN} B fnv1a64={FIXTURE_FNV:016x}",
            crate::decode::sb_rect_hits()
        );
    }

    /// lane-sbpart r4: replays a pinned mismatch byte-for-byte off disk (no
    /// aomenc/ffmpeg re-encode), same pattern as `pinned_golden3/4_stream_
    /// decodes_pixel_exact` -- fast red/green loop for the bisect, and lets
    /// `EC_AV1_TRACE=1` be set for one run without re-driving the encoder.
    ///
    /// LIVE since lane-av1pinslive (it was `#[ignore]`d from lane-sbpart r4
    /// only because the bytes lived in a scratchpad). Bytes committed at
    /// `crates/ec-av1/fixtures/sbpart-pin.obu` (238 bytes; sha256
    /// `62238fc077f45d89745d4d254b9c0465464003529da3b42d0a65631fbb7e16e9`),
    /// re-encoded 2026-09-29 from the source gate's own recipe
    /// (`a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact`:
    /// smptebars 192x128, single frame, `--sb-size=64 --enable-palette=1
    /// --min-partition-size=16 --max-partition-size=64`, cq=55). The command
    /// is in `lanes/av1pins.report.md`. That gate's source list has no
    /// 192x128 cell, so this pin is a 192x128 re-run of its recipe, not a
    /// replay of the original mismatch bytes.
    #[test]
    fn pinned_sbpart_stream_decodes_pixel_exact() {
        let path = std::env::var("EC_AV1_GATE_DUMP_PIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| crate_pin("sbpart-pin.obu"));
        let Ok(stream) = std::fs::read(&path) else {
            eprintln!(
                "SKIP pinned_sbpart_stream_decodes_pixel_exact: no pinned bytes at {} \
                 -- the committed copy is crates/ec-av1/fixtures/sbpart-pin.obu",
                path.display()
            );
            return;
        };
        if !have_ffmpeg() {
            eprintln!("SKIP pinned_sbpart_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 192, 128, 1);
        assert_eq!(
            frames.len(),
            ffmpeg_frames.len(),
            "pinned stream decoded {} frames, ffmpeg {} -- a length mismatch would \
             make the zip below compare a mis-paired set",
            frames.len(),
            ffmpeg_frames.len()
        );
        let (width, height) = (192usize, 128usize);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            if std::env::var_os("EC_SBPART_DIAG").is_some() {
                let (mut min_r, mut max_r, mut min_c, mut max_c, mut n) =
                    (usize::MAX, 0usize, usize::MAX, 0usize, 0usize);
                for r in 0..height {
                    for c in 0..width {
                        let idx = r * width + c;
                        if got.y[idx] != want.y[idx] {
                            min_r = min_r.min(r);
                            max_r = max_r.max(r);
                            min_c = min_c.min(c);
                            max_c = max_c.max(c);
                            n += 1;
                        }
                    }
                }
                eprintln!(
                    "DIAG frame {i}: {n} luma mismatches, bbox rows [{min_r}..{max_r}] cols \
                     [{min_c}..{max_c}] of {width}x{height}"
                );
                // Per-64px-superblock-column mismatch counts, to see whether
                // corruption starts exactly at the rect64 SB or bleeds into
                // SBs decoded earlier (raster order).
                for sb_col in 0..width.div_ceil(64) {
                    let c0 = sb_col * 64;
                    let c1 = (c0 + 64).min(width);
                    let mut cnt = 0usize;
                    let mut first = None;
                    for r in 0..height {
                        for c in c0..c1 {
                            let idx = r * width + c;
                            if got.y[idx] != want.y[idx] {
                                cnt += 1;
                                if first.is_none() {
                                    first = Some((r, c));
                                }
                            }
                        }
                    }
                    eprintln!(
                        "DIAG frame {i} sb_col {sb_col} (cols {c0}..{c1}): {cnt} mismatches, \
                         first={first:?}"
                    );
                }
            }
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (pinned sbpart)");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (pinned sbpart)");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (pinned sbpart)");
        }
    }

    /// lane-tx64x16 r1: a real `aomenc` stream whose superblock-level
    /// partition decision is `PARTITION_HORZ_4`/`PARTITION_VERT_4` (four
    /// 64x16 / 16x64 strips) must decode pixel-exact. Both arms are HARD
    /// asserted, and only hits earned inside an attempt that then compared
    /// pixel-exact against ffmpeg are counted -- a strip decoded by an
    /// attempt that later refused proves nothing (class
    /// gate-skips-on-its-own-failure). `--enable-1to4-partitions=1` is the
    /// tool switch; `--min-partition-size=16` keeps the RD search from
    /// recursing below 16x16, where several other unlanded partition shapes
    /// live.
    #[test]
    fn a_real_aomenc_stream_with_a_superblock_level_1to4_partition_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_superblock_level_1to4_partition_decodes_pixel_exact";
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
        let (mut horz_proved, mut vert_proved) = (0usize, 0usize);
        let (mut out_of_scope, mut out_of_scope_mismatch) = (0u32, 0u32);
        let n_attempts: u32 = std::env::var("EC_TX64X16_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            // Odd attempts render the SAME generator rotated 90 degrees
            // (generated at height x width, then `transpose`), because
            // aomenc's RD picks `PARTITION_VERT_4` on this content and
            // essentially never `HORZ_4`: the two arms are a transposed
            // pair, and a gate that only ever sees one of them cannot catch
            // a transposed scan/context table (class scan-weights-cross-axis).
            let source = if attempt % 2 == 0 {
                gradients_source(seed, width, height, &format!("duration={duration}:rate=25"))
            } else {
                format!(
                    "{},transpose=1",
                    gradients_source(seed, height, width, &format!("duration={duration}:rate=25"))
                )
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
                "--enable-1to4-partitions=1",
                "--min-partition-size=16",
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
            let before = (
                crate::decode::sb_rect4_horz_hits(),
                crate::decode::sb_rect4_vert_hits(),
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
            let (horz, vert) = (
                crate::decode::sb_rect4_horz_hits() - before.0,
                crate::decode::sb_rect4_vert_hits() - before.1,
            );
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if horz + vert == 0 {
                // Out of this gate's territory: aomenc's RD picked no 1:4
                // partition at all for this seed, so the attempt can neither
                // prove nor disprove the arms under test. NOT silently
                // dropped -- counted and printed, and a mismatch here is
                // reported as a defect of another lane's shape (r1 found
                // one: seed 55 of this very recipe decodes 3944 wrong luma
                // samples with zero 1:4 blocks in the stream, first at
                // (154, 59), inside the 64x32 `PARTITION_HORZ` superblock at
                // (128, 64) -- see lanes/tx64x16-r1.report.md). Selecting on
                // "the stream contains the feature", never on "the stream
                // decoded correctly", is what keeps this from being survivor
                // bias.
                out_of_scope += 1;
                if mismatched {
                    out_of_scope_mismatch += 1;
                    eprintln!(
                        "{NAME}: seed {seed} MISMATCHES with zero 1:4 blocks -- a pre-existing \
                         defect of another shape, not this gate's arms"
                    );
                }
                continue;
            }
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
            horz_proved += horz;
            vert_proved += vert;
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a stream"
        );
        assert!(
            horz_proved > 0 && vert_proved > 0,
            "{NAME}: a 1:4 arm never fired inside a pixel-exact attempt \
             (horz={horz_proved}, vert={vert_proved}, {matched} matches, {named_refusals} \
             refusals out of {n_attempts}) -- gate proved nothing this run"
        );
        eprintln!(
            "{NAME}: {named_refusals} named refusals, {matched} pixel-exact matches out of \
             {n_attempts}, horz_4 strips={horz_proved}, vert_4 strips={vert_proved}, \
             {out_of_scope} attempts carried no 1:4 block ({out_of_scope_mismatch} of them \
             mismatched -- other lanes' shapes)"
        );
        // Class gate-skips-on-its-own-failure: an attempt that decoded but
        // does not carry this gate's shape is still a decode this crate got
        // wrong. Counted separately so the message names the other shape,
        // never tolerated.
        assert_eq!(
            out_of_scope_mismatch, 0,
            "{NAME}: {out_of_scope_mismatch} attempt(s) with no 1:4 block decoded but \
             mismatched ffmpeg -- a defect of another shape, not a pass"
        );
    }

    /// lane-tx64x16 r3: the 32x32-LEVEL 1:4 pair -- `PARTITION_HORZ_4` /
    /// `PARTITION_VERT_4` at 32x32, i.e. four 32x8 / 8x32 strips, the arm the
    /// ReelB mid-film extract stops at ("a 32x32 partition type this
    /// decoder does not code (value=8)"). `--min-partition-size=8` is what
    /// lets aomenc's RD reach the 32-level 1:4 shapes; `--max-partition-size=32`
    /// keeps the superblock-level pair (already gated by its own test above)
    /// out of the counters. Both orientations HARD-asserted per bit depth --
    /// the luma scans and the two new chroma scans (`SCAN_16X4`/`SCAN_4X16`)
    /// are transposed pairs, so a one-sided gate cannot catch a transposition
    /// (class scan-weights-cross-axis). `rect4_coeff_hits` is asserted too:
    /// an all-skip stream would exercise none of the new coefficient tables
    /// (class gate-blind-to-feature). Only hits earned inside an attempt that
    /// then compared pixel-exact are counted (class
    /// gate-skips-on-its-own-failure); the 10-bit arm is what retires
    /// `enable-1to4-partitions` from `NEVER_EXERCISED_10BIT`.
    /// lane-r14: the 16x16-level 1:4 pair -- four 16x4 (`PARTITION_HORZ_4`) or
    /// 4x16 (`PARTITION_VERT_4`) strips, the shape the wall
    /// "a 1:4 partition below 16x16" refused. Fixture: 4-PIXEL bands (the
    /// 8-pixel ones of the 32-level gate above, one size down -- a 16x4 strip
    /// is flat where a 16x8 one is not), transposed per attempt so a
    /// transposed scan or nz-map table cannot pass (class
    /// scan-weights-cross-axis), plus an explicit-seed noise layer that keeps
    /// aomenc out of screen-content mode. `--enable-filter-intra=1` and
    /// is ON: this shape reads `use_filter_intra` off its own CDF rows
    /// (classes 12/13). The `--enable-tx-size-search=1` half of the window
    /// splits the strip's transform (depth 1 = TX_8X4/TX_4X8, depth 2 =
    /// TX_4X4).
    ///
    /// Counters: 16x4 strips, 4x16 strips, chroma-reference strips (the odd
    /// strip of each 4:2:0 pair, the only one that codes chroma) and strips
    /// with a coded residual must all be nonzero over the compared attempts,
    /// and only attempts that DECODED and were compared pixel-exact count
    /// (class gate-skips-on-its-own-failure).
    #[test]
    fn a_real_aomenc_stream_with_a_16_level_1to4_partition_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_16_level_1to4_partition_decodes_pixel_exact";
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
        let n_attempts: u32 = std::env::var("EC_R14_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8);
        for bit_depth in [8u32, 10u32] {
            let mut named_refusals = 0u32;
            let mut matched = 0u32;
            let (mut horz_proved, mut vert_proved) = (0usize, 0usize);
            let (mut chroma_proved, mut coeff_proved) = (0usize, 0usize);
            let mut split_proved = 0usize;
            // lane-kf1200: chroma pairs whose intra-edge filter type comes out
            // different when read from the PAIR's base mi (libaom's
            // `chroma_above_mbmi`) rather than the odd strip mi -- the shape
            // that was silently wrong by 1 on the ReelB key frames.
            let mut pair_filt_proved = 0usize;
            let (mut out_of_scope, mut out_of_scope_mismatch) = (0u32, 0u32);
            // The second half of the window turns `--enable-tx-size-search` on:
            // that is what makes a 16x4 strip SPLIT its transform (depth 1 =
            // TX_8X4, depth 2 = TX_4X4), the arm the first half never reaches.
            for attempt in 0..n_attempts * 2 {
                let tx_search = attempt >= n_attempts;
                let attempt = attempt % n_attempts;
                let seed = 42 + attempt;
                let duration = frame_count as f64 / 25.0;
                let axis = if attempt.is_multiple_of(2) { "Y" } else { "X" };
                let step = 67 + (attempt / 2) * 12;
                let noise = 6 + (attempt % 4) * 3;
                // lane-kf1200: the odd attempts carry CHROMA content. The
                // original gray source leaves both chroma planes constant, so
                // every `uv_mode` is DC and a pair's edge-filter type can never
                // differ from its strip's -- the shape that shipped wrong for
                // months. A smooth chroma gradient makes aomenc pick SMOOTH /
                // directional `uv_mode`s next to each other.
                let source = if attempt.is_multiple_of(2) {
                    format!(
                        "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                         geq=lum='mod(floor({axis}/4)*{step},256)',\
                         noise=alls={noise}:all_seed={seed},format=yuv420p"
                    )
                } else {
                    format!(
                        "color=c=black:s={width}x{height}:d={duration}:r=25,format=yuv420p,\
                         geq=lum='mod(floor({axis}/4)*{step},256)'\
                         :cb='128+100*sin((X+Y)/23)':cr='128+100*cos(X/19)',\
                         noise=alls={noise}:all_seed={seed}"
                    )
                };
                let cq_level = format!("--cq-level={}", 24 + (attempt % 4) * 8);
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
                        "-",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()), None, |c| c.output())
                    .expect("ffmpeg failed to run");
                assert!(
                    y4m.status.success(),
                    "{NAME}: ffmpeg refused the {bit_depth}-bit fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let depth_arg = format!("--bit-depth={bit_depth}");
                let input_depth_arg = format!("--input-bit-depth={bit_depth}");
                let args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_level,
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    &depth_arg,
                    &input_depth_arg,
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=1",
                    "--min-partition-size=4",
                    "--max-partition-size=16",
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--deltaq-mode=0",
                    "--enable-filter-intra=1",
                    // lane-r14: CfL stays OFF here. This shape's CfL averages
                    // the luma of the whole PAIR and is written, but no gate
                    // asserts a CfL hit counter (there is none), and passing
                    // `=1` would retire `enable-cfl-intra` from
                    // `gate_coverage::NEVER_EXERCISED_*` on a flag alone --
                    // exactly the false coverage that list exists to prevent.
                    "--enable-cfl-intra=0",
                    "--enable-intrabc=0",
                    if tx_search {
                        "--enable-tx-size-search=1"
                    } else {
                        "--enable-tx-size-search=0"
                    },
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
                let before = crate::decode::rect4_16_counters();
                let pair_filt_before = crate::decode::rect4_16_uv_pair_filt_hits();
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit, seed \
                             {seed}): {msg}"
                        );
                        named_refusals += 1;
                        eprintln!("{bit_depth}-bit seed {seed} refusal: {msg}");
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let now = crate::decode::rect4_16_counters();
                let (horz, vert, chroma, coeff, split) = (
                    now.0 - before.0,
                    now.1 - before.1,
                    now.2 - before.2,
                    now.3 - before.3,
                    now.4 - before.4,
                );
                let ffmpeg_frames = if bit_depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frame_count)
                };
                assert_eq!(frames.len(), frame_count);
                let mismatched = frames
                    .iter()
                    .zip(&ffmpeg_frames)
                    .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                if horz + vert == 0 {
                    out_of_scope += 1;
                    if mismatched {
                        out_of_scope_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit seed {seed} MISMATCHES with zero 16-level \
                             1:4 strips -- a pre-existing defect of another shape"
                        );
                    }
                    continue;
                }
                if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!(
                        "EC_AV1_GATE_DUMP: wrote mismatching stream ({bit_depth}-bit, seed \
                         {seed}) to {path}"
                    );
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg ({bit_depth}-bit, seed {seed})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg ({bit_depth}-bit, seed {seed})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg ({bit_depth}-bit, seed {seed})"
                    );
                }
                horz_proved += horz;
                vert_proved += vert;
                chroma_proved += chroma;
                coeff_proved += coeff;
                pair_filt_proved += crate::decode::rect4_16_uv_pair_filt_hits() - pair_filt_before;
                split_proved += split;
                matched += 1;
            }
            assert!(
                matched > 0,
                "{NAME}: every {bit_depth}-bit attempt refused or fell out of scope ({named_refusals} \
                 refusals, {out_of_scope} out of scope); the gate proved nothing"
            );
            assert_eq!(
                out_of_scope_mismatch, 0,
                "{NAME}: {out_of_scope_mismatch} {bit_depth}-bit attempt(s) mismatch with no \
                 16-level 1:4 strip at all"
            );
            assert!(
                horz_proved > 0 && vert_proved > 0,
                "{NAME}: {bit_depth}-bit compared attempts decoded {horz_proved} 16x4 and \
                 {vert_proved} 4x16 strips -- both orientations must fire or a transposed table \
                 passes"
            );
            assert!(
                chroma_proved > 0,
                "{NAME}: {bit_depth}-bit compared attempts closed {chroma_proved} chroma pairs"
            );
            assert!(
                coeff_proved > 0,
                "{NAME}: {bit_depth}-bit compared attempts coded no residual on any strip"
            );
            assert!(
                split_proved > 0,
                "{NAME}: {bit_depth}-bit compared attempts never split a strip's transform -- \
                 the `--enable-tx-size-search=1` half of the window proved nothing"
            );
            assert!(
                pair_filt_proved > 0,
                "{NAME}: {bit_depth}-bit compared attempts never decoded a chroma pair whose \
                 edge-filter type differs between the pair's base mi and the strip's own mi -- \
                 the lane-kf1200 fix is unexercised, so this gate would pass with it reverted"
            );
            eprintln!(
                "{NAME}: {bit_depth}-bit {matched} compared attempts, {horz_proved} 16x4 / \
                 {vert_proved} 4x16 strips, {chroma_proved} chroma pairs, {coeff_proved} coded \
                 units, {split_proved} split-transform strips, {pair_filt_proved} pair-base \
                 filt-type flips, {named_refusals} refusals, {out_of_scope} out of scope"
            );
        }
    }

    #[test]
    // lane-tx64x16 r4: the fixture r3 could not find. `gradients` never wins
    // a 32-level 1:4 shape; 8-pixel BANDS do -- each 32x8 strip is flat, so
    // HORZ_4 beats both HORZ (two bands per 32x16) and SPLIT, while the noise
    // layer keeps aomenc out of screen-content mode. Measured 16 HORZ_4 / 24
    // VERT_4 strips per frame, both bit depths (lanes/tx64x16-r4.report.md).
    fn a_real_aomenc_stream_with_a_32x32_level_1to4_partition_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_32x32_level_1to4_partition_decodes_pixel_exact";
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
        let n_attempts: u32 = std::env::var("EC_TX64X16_GATE32_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8);
        for bit_depth in [8u32, 10u32] {
            let mut named_refusals = 0u32;
            let mut matched = 0u32;
            let (mut horz_proved, mut vert_proved, mut coeff_proved) = (0usize, 0usize, 0usize);
            let (mut depth1_proved, mut depth2_proved) = (0usize, 0usize);
            let (mut out_of_scope, mut out_of_scope_mismatch) = (0u32, 0u32);
            // lane-rectsplitx r1: the SECOND half of the attempt window turns
            // `--enable-tx-size-search` ON at low cq, which is what makes a
            // 32-level 1:4 strip SPLIT its transform -- depth 1 is the 2:1
            // rect unit TX_16X8/TX_8X16 (`sub_tx_size_map[TX_32X8]`), depth 2
            // the square TX_8X8, and the two interleave across attempts, so
            // both are asserted (a depth-2-only run leaves the rect-unit path
            // unproved).
            for attempt in 0..n_attempts * 2 {
                let tx_search = attempt >= n_attempts;
                let attempt = attempt % n_attempts;
                let seed = 42 + attempt;
                let duration = frame_count as f64 / 25.0;
                // 8-pixel bands: a 32x8 strip is flat, a 32x16 one is not, so
                // aomenc's RD picks HORZ_4 (bands across Y) / VERT_4 (bands
                // across X). Odd attempts are the transposed source -- the
                // transposed-pair discipline the superblock-level 1:4 gate
                // above documents, and the only way a transposed scan table is
                // caught (class scan-weights-cross-axis). The noise layer is
                // what keeps the flat bands out of screen-content mode; its
                // seed is explicit, never lavfi's ignored default.
                let axis = if attempt.is_multiple_of(2) { "Y" } else { "X" };
                let step = 67 + (attempt / 2) * 12;
                let noise = 6 + (attempt % 4) * 3;
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='mod(floor({axis}/8)*{step},256)',\
                     noise=alls={noise}:all_seed={seed},format=yuv420p"
                );
                // lane-band63: the second cq is 63, not 45 -- a near-max
                // quantiser is where deblock/CDEF do the most work and where a
                // 1:4 strip is most often skip; all 16 streams of this family at
                // cq 63 (8- and 10-bit) decode pixel-exact.
                let cq_level = if tx_search {
                    // 12..28: the window measured to fire both split depths on
                    // this fixture (lanes/tx64x16-r4.handoff.md).
                    format!("--cq-level={}", 12 + (attempt % 4) * 5)
                } else {
                    format!("--cq-level={}", if attempt % 4 < 2 { 32 } else { 63 })
                };
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
                        "-",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()), None, |c| c.output())
                    .expect("ffmpeg failed to run");
                assert!(
                    y4m.status.success(),
                    "{NAME}: ffmpeg refused the {bit_depth}-bit fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let depth_arg = format!("--bit-depth={bit_depth}");
                let input_depth_arg = format!("--input-bit-depth={bit_depth}");
                let args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_level,
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    &depth_arg,
                    &input_depth_arg,
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=1",
                    "--min-partition-size=8",
                    "--max-partition-size=32",
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--deltaq-mode=0",
                    "--enable-filter-intra=0",
                    "--enable-cfl-intra=0",
                    "--enable-intrabc=0",
                    if tx_search {
                        "--enable-tx-size-search=1"
                    } else {
                        "--enable-tx-size-search=0"
                    },
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
                let before = (
                    crate::decode::rect4_32_horz_hits(),
                    crate::decode::rect4_32_vert_hits(),
                    crate::decode::rect4_coeff_hits(),
                );
                let split_before = crate::decode::rect4_split_depth_hits();
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit, seed \
                             {seed}): {msg}"
                        );
                        named_refusals += 1;
                        eprintln!("{bit_depth}-bit seed {seed} refusal: {msg}");
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let (horz, vert, coeff) = (
                    crate::decode::rect4_32_horz_hits() - before.0,
                    crate::decode::rect4_32_vert_hits() - before.1,
                    crate::decode::rect4_coeff_hits() - before.2,
                );
                let split_now = crate::decode::rect4_split_depth_hits();
                let (d1, d2) = (split_now.0 - split_before.0, split_now.1 - split_before.1);
                let ffmpeg_frames = if bit_depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frame_count)
                };
                assert_eq!(frames.len(), frame_count);
                let mismatched = frames
                    .iter()
                    .zip(&ffmpeg_frames)
                    .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                if horz + vert == 0 {
                    // Out of this gate's territory (no 32-level 1:4 block in
                    // the stream): counted and printed, never silently
                    // dropped, and a mismatch here is another shape's defect.
                    out_of_scope += 1;
                    if mismatched {
                        out_of_scope_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit seed {seed} MISMATCHES with zero 32-level \
                             1:4 blocks -- a pre-existing defect of another shape"
                        );
                    }
                    continue;
                }
                if mismatched
                    && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                        std::fs::write(&path, &stream).expect("writing pinned stream");
                        eprintln!(
                            "EC_AV1_GATE_DUMP: wrote mismatching stream ({bit_depth}-bit, seed \
                             {seed}) to {path}"
                        );
                    }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg ({bit_depth}-bit, seed {seed})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg ({bit_depth}-bit, seed {seed})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg ({bit_depth}-bit, seed {seed})"
                    );
                }
                horz_proved += horz;
                vert_proved += vert;
                coeff_proved += coeff;
                depth1_proved += d1;
                depth2_proved += d2;
                matched += 1;
            }
            assert!(
                matched > 0,
                "{NAME}: every {bit_depth}-bit attempt refused; the gate never decoded a stream"
            );
            assert!(
                horz_proved > 0 && vert_proved > 0,
                "{NAME}: a 32-level 1:4 arm never fired inside a pixel-exact {bit_depth}-bit \
                 attempt (horz={horz_proved}, vert={vert_proved}, {matched} matches, \
                 {named_refusals} refusals out of {n_attempts}) -- gate proved nothing this run"
            );
            assert!(
                coeff_proved > 0,
                "{NAME}: every {bit_depth}-bit 32-level 1:4 strip was skip -- the 32x8/16x4 \
                 coefficient tables were never read"
            );
            eprintln!(
                "{NAME}: {bit_depth}-bit: {named_refusals} named refusals, {matched} pixel-exact \
                 matches out of {n_attempts}, horz_4 strips={horz_proved}, vert_4 \
                 strips={vert_proved}, coded strips={coeff_proved}, {out_of_scope} attempts \
                 carried no 32-level 1:4 block ({out_of_scope_mismatch} mismatched)"
            );
            // The split-transform halves of the window: both tx depths must
            // have fired inside an attempt that then compared pixel-exact
            // (class counter-from-refused-stream -- a global counter would
            // count strips of attempts that later refused).
            // lane-rectsplitx r2: BOTH depths are now decoded, so both are
            // HARD-asserted -- depth 1 is the 2:1 rect unit
            // (`sub_tx_size_map[TX_32X8] == TX_16X8`), depth 2 the square
            // TX_8X8, and they interleave across the window; a run that only
            // reached one of them proves half the walk.
            assert!(
                depth1_proved > 0 && depth2_proved > 0,
                "{NAME}: {bit_depth}-bit: a 1:4 split-transform depth never fired inside a \
                 pixel-exact attempt (depth1={depth1_proved}, depth2={depth2_proved}) -- the \
                 tx-size-search half of the window proved nothing"
            );
            eprintln!(
                "{NAME}: {bit_depth}-bit split-transform 1:4 strips: depth1={depth1_proved}, \
                 depth2={depth2_proved}"
            );
            assert_eq!(
                out_of_scope_mismatch, 0,
                "{NAME}: {bit_depth}-bit: {out_of_scope_mismatch} attempt(s) with zero 32-level \
                 1:4 blocks decoded but mismatched ffmpeg -- a defect of another shape, not a pass"
            );
        }
    }

    /// lane-fi32x8 r1 (class symbol-consumption-gap): the 32x32-level 1:4
    /// strips (32x8/8x32) are inside `av1_filter_intra_allowed_bsize`'s
    /// "both sides <= 32" bound, so libaom reads a `use_filter_intra` flag on
    /// every DC_PRED one of them -- and `--enable-filter-intra` is ON by
    /// aomenc default, so the sibling gate above (which spells it `=0`) could
    /// never see it. `filter_intra_size_class_rect` had no row for either
    /// shape, so the symbol was never read and the tile desynced silently
    /// from that strip on. Same band fixture as the sibling (8-px bands +
    /// seeded noise, both orientations), the ONLY recipe change being
    /// `--enable-filter-intra=1` appended AFTER the base recipe (aomenc keeps
    /// the LAST occurrence of a repeated `--enable-*` flag). The counter is
    /// this shape's OWN (`filter_intra_rect4_hits`), not the shared rect one,
    /// so a filter-intra 16x8 strip elsewhere in the frame cannot stand in
    /// for it; only hits earned inside an attempt that then compared
    /// pixel-exact are counted (class gate-skips-on-its-own-failure), and
    /// both bit depths are asserted independently.
    ///
    /// `--enable-tx-size-search=0` is kept from the sibling recipe: a 1:4
    /// strip with `tx_depth != 0` is still refused by name ("a 32x32-level
    /// 1:4 strip with a split transform"), so a search-on arm would only
    /// harvest refusals, not coverage -- filter intra is proven at depth 0
    /// here and the per-TU case belongs to the round that lifts that refusal.
    #[test]
    fn a_real_aomenc_stream_with_filter_intra_on_a_32_level_1to4_strip_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_filter_intra_on_a_32_level_1to4_strip_decodes_pixel_exact";
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
        let n_attempts: u32 = std::env::var("EC_FI32X8_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            // 16, not 12 (measured, r1): the first attempt whose VERT_4
            // (8x32) strips pick filter intra at 8 bit is attempt 13.
            .unwrap_or(16);
        for bit_depth in [8u32, 10u32] {
            let mut named_refusals = 0u32;
            let mut matched = 0u32;
            let (mut fi_horz_proved, mut fi_vert_proved, mut strips_proved) =
                (0usize, 0usize, 0usize);
            let (mut out_of_scope, mut out_of_scope_mismatch) = (0u32, 0u32);
            for attempt in 0..n_attempts {
                let seed = 42 + attempt;
                let duration = frame_count as f64 / 25.0;
                let axis = if attempt % 2 == 0 { "Y" } else { "X" };
                let step = 67 + (attempt / 2) * 12;
                // A HEAVIER noise layer than the sibling gate's 6..15 (lane-fi32x8 r1,
                // measured): filter intra is what aomenc reaches for when a flat
                // band is noisy, so at 6..15 the flag fired on a 1:4 strip in 3 of
                // 40 8-bit attempts and in NONE of 40 10-bit ones, while at 18..27
                // it fires in most attempts at BOTH depths.
                let noise = 18 + (attempt % 4) * 3;
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='mod(floor({axis}/8)*{step},256)',\
                     noise=alls={noise}:all_seed={seed},format=yuv420p"
                );
                // lane-band63: 63 joins the rotation -- filter intra ON at a
                // near-max quantiser is exactly the configuration the stale
                // cq-63 premise blamed, and it decodes exact.
                let cq_level = format!(
                    "--cq-level={}",
                    [32, 45, 55, 63][(attempt / 2 % 4) as usize]
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
                        "-",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()), None, |c| c.output())
                    .expect("ffmpeg failed to run");
                assert!(
                    y4m.status.success(),
                    "{NAME}: ffmpeg refused the {bit_depth}-bit fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let depth_arg = format!("--bit-depth={bit_depth}");
                let input_depth_arg = format!("--input-bit-depth={bit_depth}");
                let args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_level,
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    &depth_arg,
                    &input_depth_arg,
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=1",
                    "--min-partition-size=8",
                    "--max-partition-size=32",
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--deltaq-mode=0",
                    "--enable-cfl-intra=0",
                    "--enable-intrabc=0",
                    "--enable-tx-size-search=0",
                    // The competing intra modes off (lane-fi32x8 r1): with
                    // directional/paeth/smooth intra available, aomenc's RD
                    // picks filter intra on a 32x8 strip in only ~3 of 40
                    // attempts at 8 bit and NEVER at 10 bit (measured, see the
                    // r1 report), so this shape's own symbol would go
                    // unexercised at 10 bit. Leaving DC and filter intra as
                    // the block's luma intra choices makes the flag fire
                    // without weakening the gate: every decoded frame is
                    // still compared to ffmpeg in full.
                    "--enable-directional-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-smooth-intra=0",
                    // THE override, last (aomenc keeps the last occurrence).
                    "--enable-filter-intra=1",
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
                let before = (
                    crate::decode::filter_intra_rect4_horz_hits(),
                    crate::decode::filter_intra_rect4_vert_hits(),
                    crate::decode::rect4_32_horz_hits() + crate::decode::rect4_32_vert_hits(),
                );
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit, seed \
                             {seed}): {msg}"
                        );
                        named_refusals += 1;
                        eprintln!("{bit_depth}-bit seed {seed} refusal: {msg}");
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let (fi_horz, fi_vert, strips) = (
                    crate::decode::filter_intra_rect4_horz_hits() - before.0,
                    crate::decode::filter_intra_rect4_vert_hits() - before.1,
                    crate::decode::rect4_32_horz_hits() + crate::decode::rect4_32_vert_hits()
                        - before.2,
                );
                let fi = fi_horz + fi_vert;
                let ffmpeg_frames = if bit_depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frame_count)
                };
                assert_eq!(frames.len(), frame_count);
                let mismatched = frames
                    .iter()
                    .zip(&ffmpeg_frames)
                    .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                // Per-attempt line (opt-in): which (seed, cq, axis, noise)
                // actually made aomenc pick filter intra ON a 1:4 strip is
                // exactly what a re-tune of this fixture needs, and it is not
                // recoverable from the summary.
                if std::env::var_os("EC_FI32X8_VERBOSE").is_some() {
                    eprintln!(
                        "{NAME}: {bit_depth}-bit seed {seed} {cq_level} axis={axis} noise={noise} \
                         fi={fi_horz}+{fi_vert} strips={strips} mismatched={mismatched}"
                    );
                }
                if fi == 0 {
                    out_of_scope += 1;
                    if mismatched {
                        out_of_scope_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit seed {seed} MISMATCHES with zero \
                             filter-intra 1:4 strips ({strips} plain 1:4 strips) -- a defect of \
                             another shape"
                        );
                    }
                    continue;
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg ({bit_depth}-bit, seed {seed})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg ({bit_depth}-bit, seed {seed})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg ({bit_depth}-bit, seed {seed})"
                    );
                }
                fi_horz_proved += fi_horz;
                fi_vert_proved += fi_vert;
                strips_proved += strips;
                matched += 1;
            }
            // BOTH orientations, per bit depth: rows 8/9 of `cdf::FILTER_INTRA`
            // are asymmetric (18101 for 32x8, 20229 for 8x32), so a one-sided
            // assert would pass on a transposed pair.
            assert!(
                fi_horz_proved > 0 && fi_vert_proved > 0 && matched > 0,
                "{NAME}: a 32x8/8x32 strip orientation never read use_filter_intra=1 inside a \
                 pixel-exact {bit_depth}-bit attempt (32x8={fi_horz_proved}, \
                 8x32={fi_vert_proved}, {matched} matches, {named_refusals} refusals, \
                 {out_of_scope} attempts with no filter-intra 1:4 strip, out of {n_attempts}) \
                 -- gate proved nothing this run"
            );
            eprintln!(
                "{NAME}: {bit_depth}-bit: {named_refusals} named refusals, {matched} pixel-exact \
                 matches out of {n_attempts}, filter-intra 1:4 strips={fi_horz_proved}x(32x8)+{fi_vert_proved}x(8x32) of \
                 {strips_proved} 1:4 strips, {out_of_scope} attempts carried none \
                 ({out_of_scope_mismatch} mismatched)"
            );
            assert_eq!(
                out_of_scope_mismatch, 0,
                "{NAME}: {bit_depth}-bit: {out_of_scope_mismatch} attempt(s) with zero \
                 filter-intra 1:4 strips decoded but mismatched ffmpeg -- a defect of another \
                 shape, not a pass"
            );
        }
    }

    #[test]
    fn a_real_aomenc_multi_tile_intra_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_multi_tile_intra_stream_decodes_pixel_exact";
        let before = crate::decode::uv_tile_edge_suppressed_hits();
        run_multi_tile_gate(NAME, 1, 1, 8, 1, &[], false, None);
        // lane-mtfix r1: this is the only arm with a tile ROW boundary, so it
        // is the one that exercises the tile-relative guard on the chroma
        // edge-filter neighbour. Without the guard, seed 46's first block of
        // the bottom-right tile read the tile ABOVE it and mis-filtered its
        // chroma edge (U/V off by 1..2 over chroma x[120..127] y[32..37]).
        if have_ffmpeg() && have_aomenc() {
            assert!(
                crate::decode::uv_tile_edge_suppressed_hits() > before,
                "{NAME}: no chroma edge-filter neighbour read was suppressed at a tile edge -- \
                 the tile-relative guard is untested by this gate"
            );
        }
    }

    /// lane-kf900 r1: a SKIPPED 8x8 intra leaf whose `tx_depth` symbol
    /// resolved to TX_4X4 must publish 4 -- not its 8-px block side -- to the
    /// next block's `get_tx_size_context` (libaom `set_txfm_ctxs(mbmi->tx_size,
    /// ..., skip_txfm && is_inter_block(mbmi), xd)`; that skip term is always 0
    /// for an intra block). `decode_leaf8`'s tail wrote the literal 8, so the
    /// rect strip below such a leaf read `tx_size_cat1` row 2 where libaom
    /// reads row 1 -- same alphabet, wrong CDF row, the msac range diverged
    /// mid-frame with NO refusal. `skip_split_tx_hits` was `pub` and asserted
    /// by no gate at all before this one (class `tool-disabled-in-every-gate`).
    #[test]
    fn a_real_aomenc_stream_with_a_skipped_8x8_intra_leaf_whose_tx_split_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_a_skipped_8x8_intra_leaf_whose_tx_split_decodes_pixel_exact";
        crate::decode::reset_skip_split_tx_hits();
        run_multi_tile_gate(NAME, 0, 0, 8, 1, &[], false, None);
        // lane-kf900 r2: the counter assert that used to sit here is
        // UNREACHABLE from aomenc. 108 measured recipes (72 at 8-bit, 36 at
        // 10-bit: `geq` half-flat/half-noise, `mandelbrot`, `testsrc2`,
        // `noise=all_seed=49`, cq 55/59/63, `--cpu-used=0..2`,
        // `--enable-tx-size-search=1 --min-partition-size=8
        // --enable-rect-partitions=1`; log
        // `$HOME/.cache/kf900/sweep/results{,10}.tsv`) produced
        // `skip_split_tx_hits() == 0` on every stream that decoded, because
        // libaom's own encoder forces `tx_size` to the block maximum once it
        // decides `skip_txfm` -- so no aomenc stream carries a SKIPPED 8x8
        // intra leaf whose `tx_depth` resolved to `TX_4X4`. The film's
        // encoder does; the counter is hard-asserted on the real stream in
        // [`a_10bit_key_frame_with_skipped_8x8_intra_leaves_that_split_their_transform_decodes_luma_exact`].
        // These two arms stay as the pixel-exact multi-tile control.
    }

    /// [`a_real_aomenc_stream_with_a_skipped_8x8_intra_leaf_whose_tx_split_decodes_pixel_exact`]'s
    /// 10-bit arm -- the film that exposed the defect is yuv420p10le.
    #[test]
    fn a_real_aomenc_stream_with_a_skipped_8x8_intra_leaf_whose_tx_split_10bit_decodes_pixel_exact()
    {
        const NAME: &str = "a_real_aomenc_stream_with_a_skipped_8x8_intra_leaf_whose_tx_split_10bit_decodes_pixel_exact";
        crate::decode::reset_skip_split_tx_hits();
        run_multi_tile_gate(NAME, 0, 0, 10, 1, &[], false, None);
        // lane-kf900 r2: the counter assert that used to sit here is
        // UNREACHABLE from aomenc. 108 measured recipes (72 at 8-bit, 36 at
        // 10-bit: `geq` half-flat/half-noise, `mandelbrot`, `testsrc2`,
        // `noise=all_seed=49`, cq 55/59/63, `--cpu-used=0..2`,
        // `--enable-tx-size-search=1 --min-partition-size=8
        // --enable-rect-partitions=1`; log
        // `$HOME/.cache/kf900/sweep/results{,10}.tsv`) produced
        // `skip_split_tx_hits() == 0` on every stream that decoded, because
        // libaom's own encoder forces `tx_size` to the block maximum once it
        // decides `skip_txfm` -- so no aomenc stream carries a SKIPPED 8x8
        // intra leaf whose `tx_depth` resolved to `TX_4X4`. The film's
        // encoder does; the counter is hard-asserted on the real stream in
        // [`a_10bit_key_frame_with_skipped_8x8_intra_leaves_that_split_their_transform_decodes_luma_exact`].
        // These two arms stay as the pixel-exact multi-tile control.
    }

    #[test]
    fn a_real_aomenc_multi_tile_intra_10bit_stream_decodes_pixel_exact() {
        run_multi_tile_gate(
            "a_real_aomenc_multi_tile_intra_10bit_stream_decodes_pixel_exact",
            2,
            0,
            10,
            1,
            &[],
            false,
            None,
        );
    }

    /// lane-t900 r19, the defect this arm exists for, now FIXED: spec 5.11.2
    /// `decode_tile` resets `RefLrWiener`/`RefSgrXqd` (the running
    /// loop-restoration coefficient reference) to the midpoint filter at the
    /// top of EVERY tile; `decode.rs` declared that state once per frame, so
    /// every tile after the first decoded its Wiener/Sgrproj coefficients off
    /// the previous tile's reference. The symbol COUNT is unchanged, so
    /// entropy and reconstruction stayed byte-exact and only the filtered
    /// pixels moved -- at seed 44 frame 0 (the key frame) the whole right tile
    /// column, luma x[128..255] y[0..91], 1330 samples off by at most 2, with
    /// chroma exact. Ladder that named it: `EC_AV1_PREFILT_DUMP` exact,
    /// `EC_AV1_POSTDEBLOCK_DUMP` exact, `EC_AV1_POSTCDEF_DUMP` exact (that
    /// last rung added to the oracle's `decodeframe.c` in this round), so the
    /// only remaining stage was loop restoration.
    #[test]
    fn a_real_aomenc_multi_tile_inter_stream_decodes_pixel_exact() {
        run_multi_tile_gate(
            "a_real_aomenc_multi_tile_inter_stream_decodes_pixel_exact",
            1,
            1,
            8,
            16,
            // Pre-existing, non-tile decoder limits this lane does not own:
            // the inter path only recurses a superblock as SPLIT (so the SB
            // level must be forced to split) and never reads a `tx_depth`
            // symbol under `TxMode::Select`. Both are named refusals in
            // `refusal_inventory.rs`; without these two flags every inter
            // attempt refuses before a single tile edge is compared.
            &["--max-partition-size=32", "--enable-tx-size-search=0"],
            false,
            None,
        );
    }

    /// lane-tiles r11, the defect this arm exists for: `palette_ctx_and_cache`
    /// / `palette_uv_cache` (`decode.rs`) read the LEFT palette neighbour with
    /// no availability guard at all, where libaom's `av1_get_palette_cache`
    /// takes `xd->left_mbmi`, which is NULL at the block's own TILE left
    /// column. A second tile column therefore inherited the previous tile's
    /// palette colours and `palette_y_mode` ctx. Screen content is what makes
    /// aomenc pick palette at all, so this arm drives `--tune-content=screen
    /// --enable-palette=1` over a 2-column grid and asserts a real palette
    /// block actually fired.
    /// `#[ignore]`d at r11 and NOT yet proof of the fix: with the broad tool
    /// set every seed refuses before a palette block reconstructs
    /// ("a HORZ_A/HORZ_B/VERT_A partition below 16x16"), and the per-arm
    /// `extra` overrides do not switch the base recipe's tools off -- aomenc
    /// keeps the FIRST occurrence of a repeated `--enable-*` flag (measured:
    /// appending `--enable-rect-partitions=0` after the base `=1` changed the
    /// refusal not at all). Next round: put `extra` BEFORE the base list in
    /// `run_multi_tile_gate`'s args, then this arm should reach a palette
    /// block on smptebars with rect/ab off.
    #[test]
    fn a_real_aomenc_multi_tile_palette_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_multi_tile_palette_stream_decodes_pixel_exact";
        let before = crate::decode::palette_hits();
        run_multi_tile_gate(
            NAME,
            1,
            0,
            8,
            1,
            &[
                "--enable-palette=1",
                "--tune-content=screen",
                "--cq-level=30",
                "--enable-tx-size-search=0",
                "--max-partition-size=32",
                // Screen content + rect strips is a separate, pre-existing
                // refusal ("palette syntax is consumed for square blocks
                // only"); this arm owns the palette CACHE at a tile edge, so
                // it takes the square-only path the other palette gates take.
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
            ],
            true,
            Some("smptebars"),
        );
        if have_ffmpeg() && have_aomenc() {
            assert!(
                crate::decode::palette_hits() > before,
                "{NAME}: no palette block fired -- the gate proves nothing about the palette \
                 cache at a tile edge"
            );
        }
    }

    #[test]
    fn a_real_aomenc_multi_tile_inter_10bit_stream_decodes_pixel_exact() {
        run_multi_tile_gate(
            "a_real_aomenc_multi_tile_inter_10bit_stream_decodes_pixel_exact",
            2,
            0,
            10,
            16,
            // Pre-existing, non-tile decoder limits this lane does not own:
            // the inter path only recurses a superblock as SPLIT (so the SB
            // level must be forced to split) and never reads a `tx_depth`
            // symbol under `TxMode::Select`. Both are named refusals in
            // `refusal_inventory.rs`; without these two flags every inter
            // attempt refuses before a single tile edge is compared.
            &["--max-partition-size=32", "--enable-tx-size-search=0"],
            false,
            None,
        );
    }

    /// lane-rectchroma r1: a real `aomenc` stream in which a HORZ/VERT strip
    /// splits its transform (`--enable-tx-size-search` left ON) and whose
    /// chroma half is one of the four rect shapes that had NO coefficient
    /// tables in [`crate::decode`] before this round -- 32x8 / 8x32 (the 1:4
    /// superblock strips 64x16 / 16x64) and 8x4 / 4x8 (the 8x16 / 16x8 rect
    /// of a 16x16-level AB partition). Every such strip refused by name
    /// ("a coded HORZ/VERT strip whose chroma transform has no rect
    /// coefficient tables here"), which is where both ReelC extracts stopped.
    ///
    /// Per-shape counters, deltas taken only over attempts that then compared
    /// pixel-exact (class counter-from-refused-stream), on 8 AND 10 bit (his
    /// films are `yuv420p10le`). Odd seeds render the source transposed so
    /// the 32x8/8x32 and 8x4/4x8 pairs both fire -- a single-orientation
    /// sweep cannot catch a transposed scan table (class
    /// scan-weights-cross-axis).
    #[test]
    fn a_real_aomenc_stream_with_a_coded_strip_whose_chroma_is_a_4to1_or_sub8_rect_decodes_pixel_exact()
     {
        const NAME: &str = "a_real_aomenc_stream_with_a_coded_strip_whose_chroma_is_a_4to1_or_sub8_rect_decodes_pixel_exact";
        // No seed is excluded any more: seed 46's cq-32 mismatch was the
        // per-transform-unit reach (lane-band46 / palette2 r12), and the pin
        // below decodes it pixel-exact at both depths.
        const EXCLUDED_SEEDS: &[u32] = &[];
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
        let n_attempts: u32 = std::env::var("EC_RECTCHROMA_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);
        // 32x8, 8x32, 8x4, 4x8, summed over both bit depths.
        let mut proved = [0usize; 4];
        let mut matched = 0u32;
        let mut refusals = 0u32;
        // Diagnostics: how much of the territory the COMPARED streams carried
        // at all, so a zero above reads as "recipe picked nothing" rather
        // than "the shape never split" without another run.
        let (mut split_strips, mut rect4_strips) = (0usize, 0usize);
        let mut all_rect4 = 0usize;
        // Asserted separately: this is the gate that takes `enable-1to4-
        // partitions` off `gate_coverage::NEVER_EXERCISED_10BIT`, so the
        // 10-bit half has to carry the shape on its own.
        let mut rect4_strips_10bit = 0usize;
        let (mut out_of_scope, mut out_of_scope_mismatch) = (0u32, 0u32);
        // 1:4 strips whose transform splits ONCE (the unit is still a 2:1
        // rect) that decoded and compared pixel-exact. Until the rectsplitx
        // merge this shape was refused by name and the gate counted the
        // REFUSALS; counting the decoded hits instead keeps the same
        // territory claim now that the capability landed (class
        // counter-from-refused-stream: only attempts that compared exact
        // count).
        let mut depth1_decoded = 0usize;
        for (bit_depth, cq) in [
            (8u32, 45u32),
            (8, 32),
            (8, 60),
            (10, 45),
            (10, 32),
            (10, 60),
        ] {
            for attempt in 0..n_attempts {
                let seed = 42 + attempt;
                if EXCLUDED_SEEDS.contains(&seed) {
                    continue;
                }
                let stream = rectchroma_stream(bit_depth, cq, attempt, width, height);
                let before = crate::decode::rect_split_chroma_shape_hits();
                let split_before = crate::decode::rect_split_tx_hits();
                let depth1_before = crate::decode::rect4_split_depth_hits().0;
                let rect4_before = (
                    crate::decode::sb_rect4_horz_hits(),
                    crate::decode::sb_rect4_vert_hits(),
                );
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit seed \
                             {seed}): {msg}"
                        );
                        refusals += 1;
                        all_rect4 += (crate::decode::sb_rect4_horz_hits() - rect4_before.0)
                            + (crate::decode::sb_rect4_vert_hits() - rect4_before.1);
                        let r4 = (crate::decode::sb_rect4_horz_hits() - rect4_before.0)
                            + (crate::decode::sb_rect4_vert_hits() - rect4_before.1);
                        eprintln!(
                            "{NAME}: {bit_depth}-bit cq {cq} seed {seed} refusal (1:4 strips so \
                             far {r4}): {msg}"
                        );
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let after = crate::decode::rect_split_chroma_shape_hits();
                if (0..4).any(|k| after[k] > before[k]) {
                    eprintln!(
                        "{NAME}: {bit_depth}-bit cq {cq} seed {seed} carries chroma shapes \
                         32x8={} 8x32={} 8x4={} 4x8={}",
                        after[0] - before[0],
                        after[1] - before[1],
                        after[2] - before[2],
                        after[3] - before[3]
                    );
                }
                let rect4_now = (crate::decode::sb_rect4_horz_hits() - rect4_before.0)
                    + (crate::decode::sb_rect4_vert_hits() - rect4_before.1);
                // The ENCODE's frame count: `rectchroma_stream` (42059) renders
                // `duration=0.04:rate=25` (42082) with `-t 0.04` (42094) -- one
                // frame -- and its aomenc runs with no `--limit`, so every
                // attempt's stream codes exactly 1 frame at both depths.
                assert_eq!(
                    frames.len(),
                    1,
                    "{NAME}: {bit_depth}-bit cq {cq} seed {seed} key frame"
                );
                let reference = if bit_depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frames.len())
                };
                assert_eq!(frames.len(), reference.len(), "{NAME}: frame count");
                if rect4_now == 0 {
                    // Out of this gate's territory (same rule the sibling
                    // superblock-1:4 gate applies): aomenc picked no 1:4
                    // strip at all, so the attempt can neither prove nor
                    // disprove this round's shape. Counted and PRINTED, never
                    // silently dropped -- selection is on "the stream carries
                    // the feature", never on "the stream decoded correctly".
                    out_of_scope += 1;
                    if frames
                        .iter()
                        .zip(&reference)
                        .any(|(g, w)| g.y != w.y || g.u != w.u || g.v != w.v)
                    {
                        out_of_scope_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit cq {cq} seed {seed} MISMATCHES with zero \
                             1:4 strips -- a pre-existing defect of another shape, not this \
                             gate's arms"
                        );
                    }
                    continue;
                }
                for (i, (got, want)) in frames.iter().zip(&reference).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg ({bit_depth}-bit cq {cq} seed {seed})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg ({bit_depth}-bit cq {cq} seed {seed})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg ({bit_depth}-bit cq {cq} seed {seed})"
                    );
                }
                for k in 0..4 {
                    proved[k] += after[k] - before[k];
                }
                split_strips += crate::decode::rect_split_tx_hits() - split_before;
                depth1_decoded += crate::decode::rect4_split_depth_hits().0 - depth1_before;
                let rect4 = (crate::decode::sb_rect4_horz_hits() - rect4_before.0)
                    + (crate::decode::sb_rect4_vert_hits() - rect4_before.1);
                rect4_strips += rect4;
                if bit_depth == 10 {
                    rect4_strips_10bit += rect4;
                }
                matched += 1;
            }
        }
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {refusals} named refusals out of \
             {} attempts; chroma shapes proved 32x8={} 8x32={} 8x4={} 4x8={}; compared \
             streams carried {split_strips} split-transform strips and {rect4_strips} \
             superblock 1:4 strips ({all_rect4} more in refused streams); {out_of_scope} \
             attempts carried no 1:4 strip ({out_of_scope_mismatch} of them mismatched -- \
             other shapes' defects)",
            6 * n_attempts,
            proved[0],
            proved[1],
            proved[2],
            proved[3]
        );
        assert_eq!(
            out_of_scope_mismatch, 0,
            "{NAME}: {out_of_scope_mismatch} attempts carrying no 1:4 strip mismatched \
             ffmpeg -- a defect of another shape, not out of scope for the suite (the one \
             known case, seed 46, was the per-TU reach and is fixed)"
        );
        assert!(
            matched > 0,
            "{NAME}: every attempt refused; the gate never decoded a stream"
        );
        assert!(
            rect4_strips_10bit > 0,
            "{NAME}: no superblock 1:4 strip in any pixel-exact 10-BIT stream -- this gate is \
             what claims 10-bit coverage for enable-1to4-partitions"
        );
        assert!(
            rect4_strips > 0,
            "{NAME}: no superblock 1:4 strip in any pixel-exact stream -- recipe lost its \
             territory, gate proved nothing this run"
        );
        // r1's actual capability delta. Before this round a 1:4 strip whose
        // transform depth is 1 was decoded as if its units were SQUARE
        // (`bw.min(bh) >> (depth - 1)`), because `sub_tx_size_map[TX_64X16]`
        // is TX_32X16, a 2:1 RECT, not TX_16X16 -- 8-bit cq 45 seed 42 of
        // this very recipe decoded 2 such strips and MISMATCHED ffmpeg on
        // frame 0 luma. It is now refused by name (rect luma transform units
        // inside a split strip are unported; the chroma half's 32x8/8x32
        // tables this round wired are in place for it). This assert is what
        // keeps that from silently regressing to a wrong-pixel decode.
        assert!(
            depth1_decoded > 0,
            "{NAME}: no attempt decoded a depth-1 1:4 split strip -- the recipe no longer \
             covers the shape this round fixed"
        );
    }

    /// PIN for the stream the chroma-rect gate used to exclude: cq 32 seed 46
    /// mismatched ffmpeg at BOTH bit depths while carrying zero 1:4 strips.
    /// The defect was the per-transform-unit intra reach (lane-band46 /
    /// palette2 r12); un-ignored 2026-09-02 once it decoded pixel-exact.
    #[test]
    fn the_chroma_rect_gates_excluded_seed_46_decodes_pixel_exact() {
        const NAME: &str = "the_chroma_rect_gates_excluded_seed_46_decodes_pixel_exact";
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
        // seed 46 == attempt 4 of the gate's sweep, cq 32.
        for bit_depth in [8u32, 10] {
            let stream = rectchroma_stream(bit_depth, 32, 4, width, height);
            let frames = decode_stream(&stream)
                .unwrap_or_else(|e| panic!("{NAME}: {bit_depth}-bit seed 46 refused: {e}"));
            // The ENCODE's frame count: `rectchroma_stream` (42059) renders
            // `duration=0.04:rate=25` (42082) with `-t 0.04` (42094) -- one frame
            // -- and its aomenc runs with no `--limit`, so the stream codes
            // exactly 1 frame at both depths. Asserted ABOVE the oracle call.
            assert_eq!(frames.len(), 1, "{NAME}: {bit_depth}-bit seed 46 key frame");
            let reference = if bit_depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frames.len())
            };
            assert_eq!(frames.len(), reference.len(), "{NAME}: frame count");
            for (i, (got, want)) in frames.iter().zip(&reference).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME} frame {i} luma ({bit_depth}-bit seed 46)"
                );
                assert_eq!(
                    got.u, want.u,
                    "{NAME} frame {i} U ({bit_depth}-bit seed 46)"
                );
                assert_eq!(
                    got.v, want.v,
                    "{NAME} frame {i} V ({bit_depth}-bit seed 46)"
                );
            }
        }
    }

    /// lane-band46 r1: a real `aomenc` band stream whose 16x16 (and 8x8)
    /// intra blocks SPLIT their transform, so every unit but the first
    /// answers `has_top_right`/`has_bottom_left` from its `row_off`/`col_off`
    /// inside the parent block rather than from the standalone-block tables
    /// (libaom `reconintra.c`; class reach-is-per-transform-unit).
    ///
    /// Seed 46 of the sibling 1:4-strip recipe is the stream that found it:
    /// it carries ZERO 1:4 strips, so that gate skipped it as out of scope
    /// while frame 0 luma mismatched ffmpeg over 916 samples -- the first at
    /// (x 14, y 18), the second 8x8 transform unit of the D203_PRED block at
    /// mi (4, 0), whose bottom-left samples sit in a unit that is not
    /// reconstructed yet (`col_off > 0` => `has_bottom_left` is 0). The full
    /// mode-info range ladder matched aomdec element for element, so the
    /// defect was reconstruction, never entropy.
    ///
    /// The gate asserts the fix FIRED (`split_tu_reach_fix_hits`, transform
    /// units whose reach differs from the old standalone answer) on 8 AND 10
    /// bit, and that seed 46 itself decodes pixel-exact.
    #[test]
    fn a_real_aomenc_band_stream_seed46_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_band_stream_seed46_decodes_pixel_exact";
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
        let mut matched = 0u32;
        let mut refusals = 0u32;
        let mut fixed_tus = 0usize;
        let mut fixed_tus_10bit = 0usize;
        let mut seed46_compared = [false; 2];
        for (di, bit_depth) in [8u32, 10].into_iter().enumerate() {
            let pix_fmt = if bit_depth == 10 {
                "yuv420p10le"
            } else {
                "yuv420p"
            };
            for seed in 42u32..=51 {
                // Byte-identical recipe to the 1:4-strip sibling gate, whose
                // seed-46 attempt is what exposed this defect (that gate
                // classes a no-1:4 stream as out of scope; this one compares
                // it).
                let bands = format!(
                    "geq=lum='mod(floor(Y/16),2)*160+48':cb=128:cr=128,noise=alls=10:all_seed={seed}"
                );
                let source = if seed % 2 == 0 {
                    format!("color=c=black:size={width}x{height}:duration=0.04:rate=25,{bands}")
                } else {
                    format!(
                        "color=c=black:size={height}x{width}:duration=0.04:rate=25,{bands},transpose=1"
                    )
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
                    "{NAME}: ffmpeg refused the {bit_depth}-bit fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let depth_arg = format!("--bit-depth={bit_depth}");
                let input_depth_arg = format!("--input-bit-depth={bit_depth}");
                let out = run_with_stdin(
                    Command::new(aomenc_path()).args([
                        "--codec=av1",
                        "--passes=1",
                        "--end-usage=q",
                        "--cq-level=32",
                        "--cpu-used=0",
                        "--threads=1",
                        "--row-mt=0",
                        "--sb-size=64",
                        &depth_arg,
                        &input_depth_arg,
                        "--enable-rect-partitions=1",
                        "--enable-ab-partitions=0",
                        "--enable-1to4-partitions=1",
                        "--min-partition-size=16",
                        "--max-partition-size=64",
                        // Left ON: this is what makes a 16x16 block code four
                        // 8x8 transform units, the shape the fix is about.
                        "--enable-tx-size-search=1",
                        "--enable-restoration=0",
                        "--enable-palette=0",
                        "--deltaq-mode=0",
                        "--enable-filter-intra=0",
                        "--enable-cfl-intra=0",
                        "--enable-intrabc=0",
                        "--obu",
                        "-o",
                        "-",
                        "-",
                    ]),
                    &y4m.stdout,
                );
                assert!(
                    out.status.success(),
                    "{NAME}: aomenc refused the fixture: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let stream = out.stdout;
                let before = crate::decode::split_tu_reach_fix_hits();
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit seed \
                             {seed}): {msg}"
                        );
                        assert!(
                            seed != 46,
                            "{NAME}: seed 46 ({bit_depth}-bit) must decode -- it is this gate's \
                             pinned stream, and it refused: {msg}"
                        );
                        refusals += 1;
                        eprintln!("{NAME}: {bit_depth}-bit seed {seed} refusal: {msg}");
                        continue;
                    }
                    Ok(frames) => frames,
                };
                // The ENCODE's frame count: `duration=0.04:rate=25` (42263) is
                // one frame and `-t 0.04` (42277) caps the y4m there, and aomenc
                // runs with no `--limit` -- so every arm (both depths, all 10
                // seeds) codes exactly 1 frame. Asserted ABOVE the oracle call.
                assert_eq!(
                    frames.len(),
                    1,
                    "{NAME}: {bit_depth}-bit seed {seed} key frame"
                );
                let reference = if bit_depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frames.len())
                };
                assert_eq!(frames.len(), reference.len(), "{NAME}: frame count");
                for (i, (got, want)) in frames.iter().zip(&reference).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg ({bit_depth}-bit seed {seed})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg ({bit_depth}-bit seed {seed})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg ({bit_depth}-bit seed {seed})"
                    );
                }
                let fixed = crate::decode::split_tu_reach_fix_hits() - before;
                fixed_tus += fixed;
                if bit_depth == 10 {
                    fixed_tus_10bit += fixed;
                }
                if seed == 46 {
                    seed46_compared[di] = true;
                }
                matched += 1;
            }
        }
        eprintln!(
            "{NAME}: {matched} pixel-exact streams, {refusals} named refusals out of 20; \
             {fixed_tus} split transform units took the per-TU reach ({fixed_tus_10bit} of them \
             10-bit)"
        );
        assert!(
            seed46_compared[0] && seed46_compared[1],
            "{NAME}: seed 46 was not compared on both depths (8-bit {}, 10-bit {})",
            seed46_compared[0],
            seed46_compared[1]
        );
        assert!(
            fixed_tus > 0,
            "{NAME}: no split transform unit answered its reach from its offset inside the \
             parent block -- the recipe lost the shape this gate pins"
        );
        assert!(
            fixed_tus_10bit > 0,
            "{NAME}: the shape never appeared in a 10-BIT pixel-exact stream (his films are \
             yuv420p10le)"
        );
    }

    /// lane-hgkf r1: a frame whose height (or width) is not a multiple of the
    /// block size makes the last superblock row (column) read a GATHERED
    /// partition bit instead of a full alphabet symbol -- and libaom's
    /// `ec_read_partition_impl` answers that bit with `PARTITION_HORZ`
    /// (resp. `VERT`) when it is 0, only `SPLIT` when it is 1. All three
    /// key-frame edge readers (64-, 32- and 16-level) read the bit and threw
    /// it away, forcing SPLIT; every such frame desynced at the first block
    /// of its partial row. 192x136 / 136x192 is the smallest shape that hits
    /// all three levels at once (136 = 2*64 + 8, so mi_rows = 34: the 64-,
    /// 32- and 16-level halves all fall outside), and it is the shape of a
    /// real 3840x1608 10-bit film key frame (1608 mod 64 == 8) that decoded
    /// 51768 samples wrong before this round.
    ///
    /// The counter is sampled per attempt and read only on an attempt that
    /// decoded AND is pixel-compared; slot 0/2 (the bit taken as HORZ/VERT)
    /// must be nonzero or the gate proves nothing -- before this round that
    /// arm was unreachable by construction.
    #[test]
    fn a_real_aomenc_stream_whose_frame_edge_partition_bit_is_horz_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_whose_frame_edge_partition_bit_is_horz_decodes_pixel_exact";
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
        let mut edge_horz_total = 0usize;
        let mut edge_sub_total = 0usize;
        let mut skip_split_tx_total = 0usize;
        // The last two shapes are the ledger-measured 64-level pair: on a
        // flat band (192x80 / 80x192) real aomenc answers the bottom-edge
        // superblock with the 64-level HORZ/VERT, which the 136 shapes above
        // never produce (their SBs always split down to the 32/16 levels).
        // lane-kf1200 r3: the 2-/4-/6-mi partial bottom rows (h mod 64 == 8,
        // 16, 24) with partial RIGHT columns as well (w mod 64 == 8), the
        // geometry of the film key frames this lane chases.
        for (width, height, flat) in [
            (192usize, 136usize, false),
            (136usize, 192usize, false),
            (192usize, 80usize, true),
            (80usize, 192usize, true),
            (200usize, 72usize, false),
            (136usize, 88usize, false),
            (264usize, 104usize, false),
        ] {
            // lane-kf1200 r3 (class `tool-disabled-in-every-gate`): the whole
            // recipe ran with `--enable-tx-size-search=0`, so no block ever
            // carried a nonzero `tx_depth` and the SKIPPED 8x8 leaf that
            // resolves to TX_4X4 -- whose txfm context this round fixed --
            // was unreachable by construction. The second arm turns it on.
            for tx_search in [0usize, 1] {
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
                            &if flat {
                                format!("color=c=gray:size={width}x{height}")
                            } else {
                                format!("mandelbrot=size={width}x{height}:start_x=-0.6")
                            },
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
                        "{NAME}: ffmpeg fixture ({width}x{height}, depth={depth}): {}",
                        String::from_utf8_lossy(&y4m.stderr)
                    );
                    let encode = || {
                        let out = run_with_stdin(
                            Command::new(aomenc_path()).args([
                                "--codec=av1",
                                "--passes=1",
                                "--end-usage=q",
                                "--cq-level=32",
                                "--cpu-used=3",
                                "--threads=1",
                                "--row-mt=0",
                                "--sb-size=64",
                                "--kf-max-dist=0",
                                "--enable-rect-partitions=1",
                                "--enable-ab-partitions=0",
                                "--enable-1to4-partitions=0",
                                &format!("--enable-tx-size-search={tx_search}"),
                                "--reduced-tx-type-set=1",
                                "--min-partition-size=8",
                                "--max-partition-size=64",
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
                            "aomenc refused the fixture ({width}x{height}, depth={depth}): {}",
                            String::from_utf8_lossy(&out.stderr)
                        );
                        out.stdout
                    };
                    let stream = encode();
                    assert_eq!(
                        stream,
                        encode(),
                        "{NAME}: aomenc output is not reproducible ({width}x{height}, depth={depth})"
                    );
                    crate::decode::reset_edge_part_hits();
                    crate::decode::reset_skip_split_tx_hits();
                    let frames = match decode_stream(&stream) {
                        Ok(frames) => frames,
                        Err(e) => {
                            let msg = e.to_string();
                            assert!(
                                msg.contains("unsupported"),
                                "{NAME}: decode failed ({width}x{height}, depth={depth}): {msg}"
                            );
                            eprintln!(
                                "{NAME}: {width}x{height} depth={depth} arm refused (not compared): {msg}"
                            );
                            continue;
                        }
                    };
                    let hits = crate::decode::edge_part_hits();
                    let skip_split_tx = crate::decode::skip_split_tx_hits();
                    // The ENCODE's frame count: `-t 1` AND `-vframes 1` (42490)
                    // cap the y4m at one frame and aomenc is given that one frame
                    // with no `--limit`, so every arm's stream holds exactly 1
                    // coded frame. Asserted ABOVE the oracle call so a decode that
                    // dropped it reds naming the encode.
                    assert_eq!(
                        frames.len(),
                        1,
                        "{NAME}: {width}x{height} depth={depth} key frame"
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
                            "{NAME} frame {i} luma vs ffmpeg ({width}x{height}, depth={depth}, \
                         edge bits horz/split 64-level {}/{}, sub-level {}/{})",
                            hits[0], hits[1], hits[2], hits[3]
                        );
                        assert_eq!(
                            got.u, want.u,
                            "{NAME} frame {i} U vs ffmpeg ({width}x{height}, depth={depth})"
                        );
                        assert_eq!(
                            got.v, want.v,
                            "{NAME} frame {i} V vs ffmpeg ({width}x{height}, depth={depth})"
                        );
                    }
                    compared += 1;
                    edge_horz_total += hits[0];
                    edge_sub_total += hits[2];
                    skip_split_tx_total += skip_split_tx;
                    eprintln!(
                        "{NAME}: pixel-exact {width}x{height} at {depth}-bit tx_search={tx_search}, \
                     edge bits 64-level horz/vert={} split={} sub-level horz/vert={} split={}, \
                     skipped 8x8 leaves with TX_4X4 {skip_split_tx}",
                        hits[0], hits[1], hits[2], hits[3]
                    );
                }
            }
        }
        assert!(
            compared > 0,
            "{NAME}: every arm refused -- the gate compared no pixels at all"
        );
        assert!(
            edge_horz_total > 0,
            "{NAME}: no 64-level frame-edge bit was ever answered HORZ/VERT -- the arm this \
             round fixed never fired"
        );
        assert!(
            edge_sub_total > 0,
            "{NAME}: no 32/16-level frame-edge bit was ever answered HORZ/VERT"
        );
        // lane-kf1200 r3: the `--enable-tx-size-search=1` half of the window
        // exists so a partial-row block can carry a nonzero `tx_depth` at all
        // (the whole recipe ran with it OFF before -- class
        // `tool-disabled-in-every-gate`). The SKIPPED 8x8 leaf that resolves
        // to TX_4X4 -- whose txfm context this round fixed -- is NOT reachable
        // from any synthetic source measured here (80 recipes, all zero), so
        // that arm is asserted by the pinned film fixture
        // `the_reelb_ss600_key_frame_skipped_split_tx_decodes_pixel_exact`
        // instead; this counter is reported, not gated.
        eprintln!(
            "{NAME}: {skip_split_tx_total} skipped 8x8 TX_4X4 leaves across compared attempts"
        );
    }

    /// lane-rectchroma2 r1, the defect this gate exists for: our inter path
    /// predicts a RECTANGULAR block over its enclosing SQUARE buffer
    /// (`side`x`side`, `chroma_side`x`chroma_side` -- see
    /// [`crate::decode`]'s `write_w`/`write_h` corner-cut), and
    /// [`crate::mc::predict_with_filters`] took libaom's narrow-block 4-tap
    /// decision from that buffer's dimensions. libaom
    /// (`av1_get_interp_filter_params_with_block_size`, called per axis from
    /// `inter_predictor` with `w` then `h`) takes it from the PREDICTION
    /// block's own dimensions, so an 8x16 / 16x8 inter block -- whose chroma
    /// is 4x8 / 8x4 -- must use the 4-tap kernel on the short axis. Luma is
    /// never <= 4 wide here, which is exactly why every such stream decoded
    /// LUMA EXACT and differed by +-1 on scattered chroma samples of those
    /// blocks (lane-interedge r1's 192x128 control, a size with no frame-edge
    /// node at all).
    ///
    /// Full-plane (Y+U+V) compare of EVERY decode-order frame
    /// (`--lag-in-frames=0`), both orientations (192x128 and its transposed
    /// twin, so the 4x8 and 8x4 chroma shapes both fire -- class
    /// `scan-weights-cross-axis`), 8 AND 10 bit (his films are yuv420p10le),
    /// cq 20/32/45, CDEF and loop restoration left ON. The counter is reset
    /// per attempt and read only on an attempt that decoded and compared
    /// (class `counter-from-refused-stream`); it counts predictions whose
    /// kernel choice DIFFERS from what the square buffer would have given, so
    /// a zero means the recipe lost the shape this gate pins.
    #[test]
    fn a_real_aomenc_rect_inter_block_predicts_chroma_with_the_narrow_kernel_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_rect_inter_block_predicts_chroma_with_the_narrow_kernel_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        const FRAMES: usize = 6;
        let mut compared = 0usize;
        let mut narrow_by_depth = [0usize; 2];
        let mut narrow_by_shape = [0usize; 2];
        for (si, (width, height)) in [(192usize, 128usize), (128usize, 192usize)]
            .into_iter()
            .enumerate()
        {
            for (di, depth) in [8usize, 10].into_iter().enumerate() {
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
                        &format!("mandelbrot=size={width}x{height}:rate=25"),
                        "-pix_fmt",
                        pix,
                        "-strict",
                        "-1",
                        "-vframes",
                        &FRAMES.to_string(),
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
                    "{NAME}: ffmpeg fixture ({width}x{height}, depth={depth}): {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                for cq in [20usize, 32, 45] {
                    let encode = || {
                        let out = run_with_stdin(
                            Command::new(aomenc_path()).args([
                                "--codec=av1",
                                "--passes=1",
                                "--end-usage=q",
                                &format!("--cq-level={cq}"),
                                "--cpu-used=4",
                                "--threads=1",
                                "--row-mt=0",
                                "--kf-max-dist=9999",
                                &format!("--limit={FRAMES}"),
                                "--lag-in-frames=0",
                                &format!("--bit-depth={depth}"),
                                &format!("--input-bit-depth={depth}"),
                                // Rect partitions ON is the whole point; every
                                // other tool below is another lane's open gap
                                // that would refuse before a pixel compare.
                                "--enable-rect-partitions=1",
                                "--enable-ab-partitions=0",
                                "--enable-1to4-partitions=0",
                                "--min-partition-size=8",
                                "--max-partition-size=64",
                                "--enable-palette=0",
                                "--enable-intrabc=0",
                                "--enable-warped-motion=0",
                                "--enable-obmc=0",
                                "--enable-masked-comp=0",
                                "--enable-interintra-comp=0",
                                "--enable-onesided-comp=0",
                                "--enable-interintra-wedge=0",
                                "--enable-smooth-interintra=0",
                                "--enable-ref-frame-mvs=0",
                                "--enable-angle-delta=0",
                                "--enable-cfl-intra=0",
                                "--enable-directional-intra=0",
                                "--enable-smooth-intra=0",
                                "--enable-paeth-intra=0",
                                "--enable-filter-intra=0",
                                "--enable-tx-size-search=0",
                                "--obu",
                                "-o",
                                "-",
                                "-",
                            ]),
                            &y4m.stdout,
                        );
                        assert!(
                            out.status.success(),
                            "aomenc refused the fixture ({width}x{height}, depth={depth}, \
                             cq={cq}): {}",
                            String::from_utf8_lossy(&out.stderr)
                        );
                        out.stdout
                    };
                    let stream = encode();
                    assert_eq!(
                        stream,
                        encode(),
                        "{NAME}: aomenc output is not reproducible ({width}x{height}, \
                         depth={depth}, cq={cq})"
                    );
                    crate::mc::reset_rect_narrow_kern_hits();
                    let frames = match decode_stream(&stream) {
                        Ok(frames) => frames,
                        Err(e) => {
                            let msg = e.to_string();
                            assert!(
                                msg.contains("unsupported"),
                                "{NAME}: decode failed ({width}x{height}, depth={depth}, \
                                 cq={cq}): {msg}"
                            );
                            eprintln!(
                                "{NAME}: {width}x{height} depth={depth} cq={cq} arm refused \
                                 (not compared): {msg}"
                            );
                            continue;
                        }
                    };
                    let narrow = crate::mc::rect_narrow_kern_hits();
                    // The ENCODE's frame count, asserted ABOVE the oracle call, not
                    // our decode's length: `-vframes {FRAMES}` (42696) feeds aomenc
                    // exactly FRAMES frames and `--limit={FRAMES}` (42724) caps its
                    // output there. A decode that dropped a frame must red HERE,
                    // naming the encode, rather than hand the same short count to
                    // ffmpeg and let the helper's byte-length assert blame ffmpeg.
                    assert_eq!(
                        frames.len(),
                        FRAMES,
                        "{NAME}: not every coded frame was decoded ({width}x{height}, \
                         depth={depth}, cq={cq})"
                    );
                    let ffmpeg_frames = if depth == 10 {
                        ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
                    } else {
                        ffmpeg_decode_sequence(&stream, width, height, frames.len())
                    };
                    assert_eq!(
                        ffmpeg_frames.len(),
                        frames.len(),
                        "{NAME}: ffmpeg frame count ({width}x{height}, depth={depth}, cq={cq})"
                    );
                    for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                        assert_eq!(
                            got.y, want.y,
                            "{NAME} frame {i} luma vs ffmpeg ({width}x{height}, depth={depth}, \
                             cq={cq}, narrow-kernel hits {narrow})"
                        );
                        assert_eq!(
                            got.u, want.u,
                            "{NAME} frame {i} U vs ffmpeg ({width}x{height}, depth={depth}, \
                             cq={cq}, narrow-kernel hits {narrow})"
                        );
                        assert_eq!(
                            got.v, want.v,
                            "{NAME} frame {i} V vs ffmpeg ({width}x{height}, depth={depth}, \
                             cq={cq}, narrow-kernel hits {narrow})"
                        );
                    }
                    compared += 1;
                    narrow_by_depth[di] += narrow;
                    narrow_by_shape[si] += narrow;
                    eprintln!(
                        "{NAME}: {width}x{height} depth={depth} cq={cq} {} frames full-plane \
                         exact, narrow-kernel hits {narrow}",
                        frames.len()
                    );
                }
            }
        }
        assert_eq!(
            compared, 12,
            "{NAME}: only {compared} of 12 arms were pixel-compared -- a refusal makes this \
             gate vacuous"
        );
        assert!(
            narrow_by_depth[0] > 0 && narrow_by_depth[1] > 0,
            "{NAME}: no rect-chroma narrow kernel fired at 8-bit ({}) or 10-bit ({}) -- the \
             recipe lost the shape this gate pins",
            narrow_by_depth[0],
            narrow_by_depth[1]
        );
        assert!(
            narrow_by_shape[0] > 0 && narrow_by_shape[1] > 0,
            "{NAME}: the shape fired in only one orientation (192x128 {}, 128x192 {})",
            narrow_by_shape[0],
            narrow_by_shape[1]
        );
    }

    /// lane-intra14 r1's decisive gate: a real aomenc INTER sequence with
    /// `--enable-1to4-partitions=1`, whose fast-zooming content makes aomenc
    /// code INTRA blocks inside inter frames on 1:4 strip shapes (32x8/8x32 at
    /// the 32 level, 64x16/16x64 at the superblock level). Every such block
    /// was the named refusal "an intra-coded 1:4 (or other non-2:1) rect strip
    /// on the inter block path" until this round; they now decode through the
    /// same `decode_block_rect4` / `decode_block_rect64` readers the KEY FRAME
    /// 1:4 paths already prove, with only the mode-info read switched to
    /// `read_intra_block_mode_info` (libaom decodemv.c:1189).
    ///
    /// Hard-asserts `intra_rect4_strip_in_inter_hits` (0 = 64x16, 1 = 16x64,
    /// 2 = 32x8, 3 = 8x32) on the one pinned recipe below, and every
    /// decode-order frame is compared. A decode that succeeds is ALWAYS
    /// pixel-compared (class [[gate-skips-on-its-own-failure]]), and a recipe
    /// that decodes exact without a counted strip is a FAILURE, never a pass.
    /// The recipe, the pin and the `--enable-1to4-partitions=0` control arm
    /// are documented on [`intra_rect4_in_inter_gate`].
    #[test]
    // CLOSED by lane-av1recipehunt r1 (un-ignored). Two independent runs agree
    // on why this gate was blind, and they are worth keeping side by side.
    //
    // lane-av1pins RE-RAN this gate in RELEASE on a21f3680 (the seven-lane
    // merged tree) and found BOTH halves of the r2 record stale:
    //   * THE PIXEL BLOCKER IS GONE -- `counted-exact=0 uncounted-exact=40
    //     named-refusals=0 (attempts 40)` at both bit depths, and not one
    //     mismatch line anywhere in the run log. 40/40 pixel-exact.
    //   * THE RECIPE NEVER FIRES -- every counted attempt reads
    //     `64x16=0 16x64=0 32x8=0 8x32=0`, so the assert trips on its
    //     non-vacuity precondition, not on any pixel.
    //
    // lane-av1recipehunt then found WHY, and closed it. The blindness is not
    // the r2 "pre-existing INTER-frame pixel defect" (gone) and not the
    // mandelbrot source: the old recipe produced ZERO 1:4 strips of ANY shape
    // at ANY level, because `--min-partition-size=32` with an all-but-empty
    // intra mode set never puts a 1:4 partition on aomenc's RD menu.
    // MEASURED: the 32-level 1:4 counter is 0 in ALL 48 attempts at
    // `--min-partition-size=16` and fires in 40 of 48 at 8 with
    // `--enable-smooth-intra/paeth-intra/directional-intra/angle-delta=1`
    // (a 32x8/8x32 strip needs an intra mode WORTH CODING; with those four
    // off the only candidate is DC, and DC never wins a 1:4 strip against
    // inter). That also explains r2's "`--enable-1to4-partitions=0` is
    // byte-identical": with no 1:4 reachable at all, the flag had nothing to
    // turn off. The new recipe is 256x192x6, `--min-partition-size=8`, the
    // full intra mode set, `--cpu-used=2 --cq-level=63`; it fires 32x8=8
    // 8x32=11 and is pixel-exact on all 6 frames. See
    // [`intra_rect4_in_inter_gate`] for the pin and the flag control arm.
    fn a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact() {
        intra_rect4_in_inter_gate(8);
    }

    /// The 10-bit arm (both of the user's films are `yuv420p10le`).
    #[test]
    // CLOSED by lane-av1recipehunt r1 (un-ignored), same recipe and same two
    // reasons as the 8-bit arm above: lane-av1pins' RELEASE re-run showed the
    // pixel blocker gone and the recipe never firing, and the root cause is
    // `--min-partition-size=32` plus the pruned intra mode set, not the
    // mandelbrot source. The 10-bit cell fires 32x8=12 8x32=7 and is
    // pixel-exact on all 6 frames.
    fn a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact_10bit() {
        intra_rect4_in_inter_gate(10);
    }

    /// lane-intrasplit r1's decisive gate: a real aomenc INTER sequence in
    /// which aomenc's RD picks an intra-coded HORZ/VERT *strip* (a 2:1
    /// rectangular partition) inside an inter frame AND splits its luma
    /// transform (`tx_depth >= 1`). Until this round that block was the named
    /// refusal "a split (nonzero tx_depth) transform on an intra HORZ/VERT
    /// strip in an inter frame"; it now walks the key frame's own per-TU path
    /// ([`decode::decode_rect_split`]). Hard-asserts
    /// [`decode::intra_rect_in_inter_split_tx_hits`] per depth: an attempt
    /// that never produced such a block proves nothing, and exhausting every
    /// attempt is a FAILURE, not a SKIP.
    #[test]
    fn a_real_aomenc_inter_sequence_with_a_split_transform_intra_strip_decodes_pixel_exact() {
        intra_rect_in_inter_split_tx_gate(8);
    }

    /// The 10-bit arm (both of the user's films are `yuv420p10le`).
    #[test]
    fn a_real_aomenc_10bit_inter_sequence_with_a_split_transform_intra_strip_decodes_pixel_exact() {
        intra_rect_in_inter_split_tx_gate(10);
    }

    /// lane-t900 r21: the witness for the palette SYNTAX on a rect intra strip
    /// inside a screen-content INTER frame.
    ///
    /// `read_palette_mode_info` is gated by the frame's
    /// `allow_screen_content_tools` and by the block's own shape
    /// (`palette_bsize_ctx_wh`: `min(w,h) >= 8`, `max(w,h) <= 64`),
    /// with no square-only term -- so a `--tune-content=screen` inter frame
    /// writes `palette_y_mode` (and `palette_uv_mode`) for an 8x16 or 16x8
    /// intra strip exactly as for a square block. This decoder's inter-path
    /// intra arm skipped both symbols and refused the whole case by name ("a
    /// HORZ/VERT intra strip in a screen-content frame (palette syntax is
    /// consumed for square blocks only)"), which a real stream reached:
    /// measured with `decode_probe`, `testsrc2` 256x192 at
    /// `--tune-content=screen --enable-palette=0 --enable-intrabc=0` stops on
    /// that refusal at an 8x16 strip (8-bit) / a 16x8 one (10-bit).
    ///
    /// Both arms now decode all ten frames pixel-exact against ffmpeg, and
    /// [`decode::intra_rect_in_inter_screen_hits`] counts the strips that read
    /// the syntax -- hard-asserted, because an attempt with no such strip
    /// proves nothing about the case the refusal named.
    ///
    /// Note `--enable-palette=0`: the encoder's palette SEARCH is what this
    /// gate must switch off, not the syntax. With it on, the key frame codes
    /// a block that really uses a palette and stops on the separate,
    /// still-live "a block that actually uses a palette (Y)" refusal before
    /// any inter frame is reached (class `refusal-hides-a-defect`, measured on
    /// six such encodes).
    #[test]
    fn a_real_aomenc_screen_inter_sequence_codes_palette_syntax_on_rect_intra_strips() {
        screen_rect_intra_palette_syntax_gate(8);
    }

    /// The 10-bit arm (both of the user's films are `yuv420p10le`).
    #[test]
    fn a_real_aomenc_10bit_screen_inter_sequence_codes_palette_syntax_on_rect_intra_strips() {
        screen_rect_intra_palette_syntax_gate(10);
    }

    /// lane-intra14 r4: the FILM witness for an intra block on a 1:4 partition
    /// inside an INTER frame -- `crates/ec-av1/fixtures/reelb_intra14_witness.obu`,
    /// 163057 bytes, sha256
    /// `0eff603bf1608e47faf5e6729670c4c77cf5c674dbad3a1533ac6660151fd90e`,
    /// two decode-order frames (key + first inter) cut from two INDEPENDENT
    /// `ffmpeg -ss 90 -t 2 -i <3840x1608 10-bit AV1 HDR10 mkv> -c:v copy -f obu`
    /// runs that hashed identically.
    ///
    /// MEASURED (r4, 27 two-second segments of that film at ss 0..6000, probe
    /// logs in the report): no aomenc recipe reaches this shape and only ONE
    /// class of stream does -- the film's own inter frames. Decode-order frame
    /// 1 of this fixture codes 3 intra 16x64 strips (`intra_rect4_in_inter`
    /// 16x64=3, zero when the fixture is truncated to the key frame alone), so
    /// the counter attribution is pinned to that frame.
    ///
    /// It is `#[ignore]`d for exactly ONE reason, and not this lane's shape:
    /// that same frame stops at a SIBLING refusal, "a split intra strip whose
    /// transform unit is 64x32 (no luma coefficient tables for that shape
    /// here)". The moment TX_64X32/TX_32X64 luma coefficient tables land, drop
    /// the `#[ignore]` -- everything else this gate needs (the reader, the
    /// counter, the two-frame pixel compare) is in place and runs.
    ///
    /// lane-sub8x4 r3: un-`#[ignore]`d. The sibling refusal it waited on (the
    /// un-split TX_64X32/TX_32X64 corner unit) is lifted in this round, so the
    /// frame this gate exists for now finishes.
    ///
    /// lane-wit16x4 r1, MEASURED: this gate is blind to the frame it counts
    /// (class `gate-blind-to-hidden-frames`). The fixture has two frame OBUs
    /// and ONE shown frame, and `decode_probe` on the fixture truncated to its
    /// first frame OBU reads `intra_rect4_in_inter 0/0/0/0` and
    /// `intra16x4_in_inter 0/0/0` while matching ffmpeg exactly -- so all of
    /// 4/23/28/72 (and the 4 intra 16x4 strips with them) live in the SECOND,
    /// NO-SHOW frame, whose pixels ffmpeg never emits and this gate never
    /// compares. Since that hidden frame also carries the intra 16x4/4x16
    /// strip whose refusal this round re-adds (no stream anywhere decodes
    /// pixel-exact while that arm fires -- lanes/wit16x4-r1.report.md), the
    /// decode now stops by name here. Un-ignored 2026-09-04: that re-added intra 16x4/4x16-in-inter refusal is gone and the gate passes.
    #[test]
    fn a_10bit_film_inter_frame_with_intra_1to4_strips_decodes_pixel_exact() {
        const NAME: &str = "a_10bit_film_inter_frame_with_intra_1to4_strips_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/reelb_intra14_witness.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (3840usize, 1608usize);
        let before = intra_rect4_in_inter_counters();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let now = intra_rect4_in_inter_counters();
        let fired =
            (now.0 - before.0) + (now.1 - before.1) + (now.2 - before.2) + (now.3 - before.3);
        assert!(
            fired > 0,
            "{NAME}: zero intra 1:4 strips in an inter frame (64x16/16x64/32x8/8x32 = \
             {}/{}/{}/{}) -- the fixture stopped carrying the shape this gate exists for",
            now.0 - before.0,
            now.1 - before.1,
            now.2 - before.2,
            now.3 - before.3
        );
        // lane-sub8x4 r3, MEASURED when this gate first RAN: the fixture has
        // TWO frame OBUs but only ONE SHOWN frame -- a raw
        // `ffmpeg -i <fixture> -pix_fmt yuv420p10le -f rawvideo` writes exactly
        // 18524160 bytes, one 3840x1608 10-bit frame, so the second is a
        // no-show frame and `frames.len() == 2` was never true. The compare is
        // not vacuous: ffmpeg's own frame count is asserted equal to ours.
        assert_eq!(
            frames.len(),
            1,
            "{NAME}: one shown frame (the second frame OBU is a no-show)"
        );
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len());
        assert_eq!(
            ffmpeg_frames.len(),
            frames.len(),
            "{NAME}: ffmpeg returned {} frames, we decoded {}",
            ffmpeg_frames.len(),
            frames.len()
        );
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!(
            "{NAME}: {fired} intra 1:4 strips in an inter frame, the shown frame pixel-exact"
        );
    }

    /// lane-sub8x4 r3 / lane-wit16x4 r1: the FILM witness for the un-split
    /// TX_64X32/TX_32X64 corner unit of a split intra strip.
    ///
    /// r3 wrote this gate as a witness for TWO arms; its intra 16x4/4x16
    /// numbers were PHANTOMS (class `counter-from-refused-stream`), counted
    /// while decode-order frame 33 of this very prefix was desynced. On the
    /// merged tip (lane-frame36 r2 fixed that desync) the fixture decodes all
    /// 33 frames pixel-exact and `intra16x4_in_inter` reads 0/0/0, so the gate
    /// panicked "the intra 16x4/4x16 strip arm did not fire". lane-wit16x4 r1
    /// re-measured every counter, split the claim, and re-added the
    /// 16x4/4x16-in-inter refusal (no stream on this tree decodes exactly
    /// while that arm fires -- see the note at `decode.rs`'s `strip16`).
    ///
    /// `crates/ec-av1/fixtures/reelb_rect64_intra16x4_witness.obu`, 23472 bytes,
    /// sha256 `c9e721088766163b9dbeb9fa8dd8b257ff0ec7f9157fc350de4d3c33cd9ec4e4`:
    /// the first 50 frame-carrying OBUs of `ffmpeg -ss 0 -t 2 -i <3840x1608
    /// 10-bit AV1 HDR10 mkv> -c:v copy -f obu`, cut twice from independent
    /// ffmpeg runs that hashed identically. 33 decode-order frames.
    ///
    /// MEASURED on the merged tip (`decode_probe`, EC_PROBE_OUT16 vs
    /// `ffmpeg -pix_fmt yuv420p10le`): all 33 frames 0 differing bytes, and
    /// `rect64_corner_tu` fires 64x32=148 32x64=123 (r3's 90/5 came out of the
    /// desynced decode). `intra16x4_in_inter` reads 0/0/0 here and is asserted
    /// as such -- if a later change makes that arm fire on this stream, the
    /// refusal that now guards it moved and this gate says so.
    ///
    /// Refusal strings: "a split intra strip whose transform unit is
    /// {tx_w}x{tx_h}" stays NARROWED (every shape other than 64x32/32x64 --
    /// that is what this gate lifts), and "an intra 16x4/4x16 strip inside an
    /// inter 16x16-level 1:4 partition" is back to its FULL width.
    #[test]
    fn a_10bit_film_frames_with_rect64_corner_tus_decode_pixel_exact() {
        const NAME: &str = "a_10bit_film_frames_with_rect64_corner_tus_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/reelb_rect64_intra16x4_witness.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (3840usize, 1608usize);
        let before16 = crate::decode::intra16x4_in_inter_hits();
        let before64 = (
            crate::decode::rect64_corner_tu_hits(0),
            crate::decode::rect64_corner_tu_hits(1),
        );
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let now16 = crate::decode::intra16x4_in_inter_hits();
        let fired16 = (
            now16.0 - before16.0,
            now16.1 - before16.1,
            now16.2 - before16.2,
        );
        let fired64 = (
            crate::decode::rect64_corner_tu_hits(0) - before64.0,
            crate::decode::rect64_corner_tu_hits(1) - before64.1,
        );
        // Hard counter asserts: the lift may not rest on a stream that stopped
        // carrying its shape (class `tool-disabled-in-every-gate`).
        assert_eq!(
            fired16,
            (0, 0, 0),
            "{NAME}: this stream carries NO intra 16x4/4x16 strip in an inter 1:4 \
             partition (r3's 31/17/23 were phantoms of the frame-33 desync); a \
             non-zero count here means the refusal guarding that arm moved"
        );
        assert!(
            fired64.0 > 0 && fired64.1 > 0,
            "{NAME}: the un-split TX_64X32/TX_32X64 corner arm did not fire (64x32={} 32x64={})",
            fired64.0,
            fired64.1
        );
        assert_eq!(frames.len(), 33, "{NAME}: 33 decode-order frames");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len());
        assert_eq!(
            ffmpeg_frames.len(),
            frames.len(),
            "{NAME}: ffmpeg returned {} frames, we decoded {}",
            ffmpeg_frames.len(),
            frames.len()
        );
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!(
            "{NAME}: 33 frames pixel-exact, rect64_corner_tu 64x32={} 32x64={} \
             (intra16x4_in_inter {} {} {}, refused shape)",
            fired64.0, fired64.1, fired16.0, fired16.1, fired16.2
        );
    }

    /// lane-thread2 (iv): frame-parallel decoding changes nothing a caller
    /// can observe. Two real streams are decoded through
    /// [`decode_stream_with_threads`] at 1 and at 4 threads and every sink
    /// call is compared -- decode index, shown flag, and all three planes.
    ///
    /// `reelb_arf_witness.obu` is the one that proves the parallel path: it
    /// carries hidden alt-refs, which is exactly the case where "output in
    /// coded order" is not the same as "output in the order workers finish",
    /// and under lane-thread3's dependency-aware scheduling 39 of its 40
    /// coded frames (all but the key frame) are decoded off the main thread
    /// -- MORE than its 37 shown frames, which is the point: lane-thread2's
    /// leaf rule dispatched 18. `palette_screen_witness.obu` is the
    /// complement, an 8-bit screen stream whose every coded frame refreshes a
    /// slot: lane-thread2 dispatched NOTHING for it, lane-thread3 dispatches
    /// all 14 of its non-key frames. Both counts are asserted exactly, plus
    /// the "more than half the shown frames really ran on a worker" bound, so
    /// neither arm can pass vacuously.
    #[test]
    fn a_real_stream_decodes_identically_with_one_and_four_threads() {
        const NAME: &str = "a_real_stream_decodes_identically_with_one_and_four_threads";
        // (fixture, shown frames, frames a 4-thread run must dispatch)
        for (fixture, shown_frames, want_dispatched) in [
            ("fixtures/reelb_arf_witness.obu", 37usize, 39usize),
            ("fixtures/palette_screen_witness.obu", 15, 14),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture);
            let stream = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
            let run = |threads: usize| -> Vec<(usize, bool, Picture)> {
                let mut out = Vec::new();
                decode_stream_with_threads(&stream, threads, |p, idx, shown| {
                    out.push((idx, shown, p.clone()));
                    Ok(())
                })
                .unwrap_or_else(|e| panic!("{NAME}: {fixture} at {threads} threads refused: {e}"));
                out
            };
            let serial = run(1);
            let before = frames_dispatched();
            let parallel = run(4);
            let dispatched = frames_dispatched() - before;
            assert_eq!(
                dispatched, want_dispatched,
                "{NAME}: {fixture} dispatched {dispatched} frames to workers, expected {want_dispatched}"
            );
            assert!(
                dispatched * 2 > shown_frames,
                "{NAME}: {fixture} dispatched {dispatched} frames, not even half of its {shown_frames} shown frames -- the parallel path is barely exercised"
            );
            assert_eq!(
                serial.iter().filter(|f| f.1).count(),
                shown_frames,
                "{NAME}: {fixture} shown frames"
            );
            assert_eq!(
                serial.len(),
                parallel.len(),
                "{NAME}: {fixture} sink call count differs"
            );
            for (i, (a, b)) in serial.iter().zip(&parallel).enumerate() {
                assert_eq!(a.0, b.0, "{NAME}: {fixture} sink {i} decode index");
                assert_eq!(a.1, b.1, "{NAME}: {fixture} sink {i} shown flag");
                assert_eq!(a.2.y, b.2.y, "{NAME}: {fixture} sink {i} luma");
                assert_eq!(a.2.u, b.2.u, "{NAME}: {fixture} sink {i} U");
                assert_eq!(a.2.v, b.2.v, "{NAME}: {fixture} sink {i} V");
            }
            eprintln!(
                "{NAME}: {fixture} {} sink calls identical, {dispatched} decoded on workers",
                serial.len()
            );
        }
    }

    /// lane-frame36 r2: the hidden-ARF witness. `crates/ec-av1/fixtures/
    /// reelb_arf_witness.obu`, 32126 bytes, sha256
    /// `7f3b060da5aa9c537633e760c4af71316237db7692f6ae0d459bd2c2dd867d37`:
    /// the first 58 frame-carrying OBUs of a 2 s head cut of the 10-bit
    /// 3840x1608 AV1 HDR10 stream, 40 decode-order frames, 37 shown.
    ///
    /// What it covers that no earlier fixture did: decode-order frame 33 is a
    /// HIDDEN alt-ref (`show_frame=0`, order_hint 64, `cdef_bits=1`, loop
    /// restoration `[Switchable, Sgrproj, Switchable]`, `skip_mode_frame`
    /// [1,6]) -- the first frame of the stream carrying a SPLIT
    /// superblock-level intra strip in an inter frame. Its 16x64 strip at
    /// mi(192,140) reads `tx_depth=1` (TX_16X32), and until this round
    /// [`decode_block_rect64`]'s split arm returned past the tail that
    /// publishes the `TXFM_CONTEXT` bands, so the 32x64 block at mi(192,144)
    /// read the row's init value 64 as its left transform height and took
    /// `tx_size_cat3` row 2 where aomdec takes row 1 -- a desync no shown
    /// frame exposed until the ARF was displayed (class
    /// `gate-blind-to-hidden-frames`).
    ///
    /// The counter assert is on `rect64_split_txfm_publish_hits`, the split
    /// arm that was silent, so a stream that stopped carrying the shape fails
    /// the gate instead of passing it vacuously.
    #[test]
    fn a_10bit_film_hidden_arf_with_split_superblock_strips_decodes_pixel_exact() {
        const NAME: &str =
            "a_10bit_film_hidden_arf_with_split_superblock_strips_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/reelb_arf_witness.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (3840usize, 1608usize);
        let before = crate::decode::rect64_split_txfm_publish_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let fired = crate::decode::rect64_split_txfm_publish_hits() - before;
        assert!(
            fired > 0,
            "{NAME}: no split superblock-level strip in the stream -- the gate would be vacuous"
        );
        assert_eq!(frames.len(), 37, "{NAME}: 37 shown frames");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len());
        assert_eq!(
            ffmpeg_frames.len(),
            frames.len(),
            "{NAME}: ffmpeg returned {} frames, we decoded {}",
            ffmpeg_frames.len(),
            frames.len()
        );
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!(
            "{NAME}: 37 shown frames pixel-exact on every plane, \
             rect64_split_txfm_publish={fired}"
        );
    }

    /// lane-unwritten-dep: the UNWRITTEN-SAMPLE census at the point the output is
    /// taken, over the real film fixtures that carry hidden alt-refs.
    ///
    /// SCOPE, stated rather than implied: the census is taken where
    /// `EC_AV1_FINAL_DUMP` writes, which is BEFORE `apply_grain`. The claim is
    /// therefore over the PRE-GRAIN planes -- which are also the bytes the
    /// reference bank stores and every later frame predicts from, the dependency
    /// that matters. Every filter writes IN PLACE into the buffer this scans, so
    /// none of them can open a hole outside its extent. `apply_grain` is the one
    /// stage after this scan that ALLOCATES a new picture, and this census never
    /// sees it: the claim is NOT "the caller's final grained picture has no hole".
    ///
    /// lane-av1graincensus: that paragraph is still true of THIS gate and is not
    /// weakened by the pointer below -- what changed is that the gap it names is
    /// no longer UNMEASURED.
    /// [`the_grained_picture_the_caller_receives_carries_no_unwritten_plane_sample`]
    /// measures the grained picture `apply_grain` returns, over its own counters,
    /// and is the gate that carries the post-grain claim.
    ///
    /// NON-VACUITY, the part that makes this gate worth having: a census is only a
    /// measurement if it CAN report non-zero. The control is a SYNTHETIC positive
    /// -- the census is handed planes with a known number of sentinel samples and
    /// must report exactly that number. It deliberately does NOT use a fixture:
    /// the corpus's one witness with a real hole (`440_request_is_422`) is REFUSED
    /// by lane/av1unwritten, so a fixture-carried control goes red on merge for a
    /// reason unrelated to this gate. A synthetic control cannot be refused and
    /// cannot change behaviour when a lane lands. It is proven by mutation in the
    /// same run: neutering `census_unwritten_final` reds it (lane report §5).
    ///
    /// WHAT THIS IS NOT, corrected from an earlier revision of this comment: it
    /// does NOT rest on a claim that the pre-deblock census is blind. That claim
    /// was wrong. It came from `dump_stage16` cropping chroma as `(fw/2, fh/2)` --
    /// a 4:2:0 assumption -- so on a 64x64 4:2:2 frame 2048 chroma samples were in
    /// no stage dump at all, and an earlier revision read that blind spot as
    /// "identical pre-deblock, so deblock creates the dependence". With per-axis
    /// extents the corrected measurement is the opposite: the pre-deblock dump on
    /// `440_request_is_422` already DIFFERS by 2158 bytes and the exact u16 census
    /// finds 224 unwritten samples (U 112, V 112, rows 36..63, whole 4x4 groups) --
    /// the same count `lanes/av1unwritten.report.md` reports. The hole is present
    /// before any filter runs and the pre-deblock census SEES it. This census is
    /// here because it is the measurement taken where the OUTPUT is taken, which
    /// spans the whole route from the tile walk to the caller.
    ///
    /// The four film fixtures are the corpus's real 10-bit streams with hidden
    /// alt-refs -- the shapes where a hidden frame's unwritten region would feed
    /// a later DISPLAYED frame through the reference bank, which is the only way
    /// an unwritten sample could reach output on a stream that decodes correctly.
    /// All four measure 0 here AND 0 on the pre-deblock census.
    ///
    /// Measured on `8d6998d7` with a provenance-verified probe (`strings` on the
    /// binary names only this worktree; reelb_arf sha256 `7f3b060da5aa9c53`,
    /// reelb_rect64 `c9e721088766163b`):
    ///
    /// ```text
    /// pre-deblock census (u16, whole plane, 0xDEAD counted per frame)
    ///   440_request_is_422            224     <- also lane/av1unwritten's own number
    ///   reelb_arf_witness                0
    ///   reelb_rect64_intra16x4_witness   0
    /// this gate (pre-grain, at the output point)
    ///   reelb_arf_witness                0
    ///   reelb_rect64_intra16x4_witness   0
    ///   gm_small_side_witness         0
    ///   reelc_sb128_inter_witness      0
    /// ```
    #[test]
    fn the_frame_the_caller_receives_carries_no_unwritten_plane_sample() {
        const NAME: &str = "the_frame_the_caller_receives_carries_no_unwritten_plane_sample";
        let _gate_lock = lock_gate_counters();

        // The census needs the sentinel FILL, which is env-gated (see
        // `fresh_plane`), and this crate forbids `set_var` (see `envflags`), so
        // the fill cannot be switched from inside a test process. The child
        // process below is therefore how the measurement is taken; what the
        // parent asserts is the CHILD's reported count.
        let census = |fixture: &str| -> (usize, usize) {
            let exe = std::env::current_exe().expect("test binary path");
            let out = std::process::Command::new(exe)
                .args([
                    "--exact",
                    "stream::tests::t09::uwdep_child_unwritten_census",
                    "--nocapture",
                ])
                .env("UWDEP_CENSUS_FIXTURE", fixture)
                .env("EC_AV1_PLANE_SENTINEL", "1")
                .output()
                .expect("running the census child");
            // `--nocapture` puts the child's `println!` on its stderr, so both
            // streams are searched.
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
            text.lines()
                .find_map(|l| l.strip_prefix("UWDEP_CENSUS "))
                .and_then(parse_census_line)
                .unwrap_or_else(|| {
                    panic!(
                        "{NAME}: the census child reported no count for {fixture} \
                         (child said: {:?})",
                        text.trim()
                    )
                })
        };

        // Non-vacuity FIRST, and SYNTHETIC on purpose (see the doc comment): the
        // census is handed planes with a known number of sentinel samples and must
        // report exactly that number. No fixture is involved, so this cannot be
        // affected by lane/av1unwritten refusing `440_request_is_422` on merge.
        // The fill is env-gated, so even this runs in the child.
        let (synthetic, synthetic_scanned) = census_synthetic();
        assert_eq!(
            synthetic_scanned, 40,
            "{NAME}: the synthetic control scanned {synthetic_scanned} samples, expected 40 \
             (16 + 16 + 8) -- the census did not look at what it was handed"
        );
        assert_eq!(
            synthetic, 7,
            "{NAME}: the census reported {synthetic} for planes carrying exactly 7 sentinel \
             samples -- the census is not measuring what it claims, so every zero below is \
             meaningless (class: census-silently-blind)"
        );

        for fixture in [
            "reelb_arf_witness",
            "reelb_rect64_intra16x4_witness",
            "gm_small_side_witness",
            "reelc_sb128_inter_witness",
        ] {
            let (n, scanned) = census(fixture);
            assert!(
                scanned > 0,
                "{NAME}: the census scanned 0 samples on {fixture} -- the call at the output \
                 point is not running, so the {n} below proves nothing (class: \
                 census-never-ran). Deleting the call site reproduces this."
            );
            assert_eq!(
                n, 0,
                "{NAME}: {fixture} hands the caller {n} samples the tile walk never wrote"
            );
        }
    }

    /// lane-av1graincensus: the picture the caller RECEIVES -- including film
    /// grain -- carries no unwritten plane sample.
    ///
    /// # WHAT THIS GATE IS FOR, and the blind spot it closes
    ///
    /// [`the_frame_the_caller_receives_carries_no_unwritten_plane_sample`] scans
    /// at `stream.rs`'s `EC_AV1_FINAL_DUMP` site, which is BEFORE `apply_grain`.
    /// Every filter (deblock/CDEF/LR/superres) writes IN PLACE into the buffer it
    /// scans, so none of them can open a hole outside its extent -- but
    /// `apply_grain` is the one stage after that scan that ALLOCATES a fresh
    /// picture: `film_grain::uninit_plane`, `Vec::with_capacity` + `set_len`, no
    /// fill, with correctness resting on a band row partition rather than on the
    /// type system. `lanes/unwritten-dep.report.md` §6 and §8 declined to claim
    /// anything about that destination. This gate measures it.
    ///
    /// # WHY THE COUNT CAN BE NON-ZERO (the part that makes it a census)
    ///
    /// Counting `== plane_poison()` over an uninitialised buffer is a census that
    /// reports ~0 either way: a hole survives with probability ~1/65536, not
    /// always. `uninit_plane` therefore fills with the poison under the SAME
    /// predicate `fresh_plane` honours. With the sentinel off -- every shipped
    /// build -- `uninit_plane` is still `with_capacity` + `set_len`, so the
    /// corner-cut it documents is untouched.
    ///
    /// # SCOPE, stated not implied
    ///
    /// The claim is over the picture the caller receives ON A GRAINED FRAME, in
    /// its final form, after the noise. It is NOT a claim that the pre-grain
    /// census already covered this -- that census never sees this buffer -- and it
    /// is NOT a claim about a frame whose grain step does not run.
    ///
    /// # NON-VACUITY, two arms, both required
    ///
    /// 1. **synthetic, first, and with a DIFFERENT known count (5, not the
    ///    pre-grain arm's 7)** so a gate reading the wrong counter goes red. No
    ///    fixture involved, so no lane's refusal can redden it.
    /// 2. **per-fixture `scanned > 0`** -- `unwritten == 0` is also what a build
    ///    with the census call deleted reports. `scanned > 0` is reachable only by
    ///    `apply_grain` actually running, so it is at once the
    ///    proof-that-grain-fired and the deletion control.
    ///
    /// Both mutations are in `lanes/av1graincensus.report.md`: deleting the
    /// `census_unwritten_grained` call at `apply_grain`'s return reds arm 2, and
    /// forcing ONE poison sample into a grained picture reds arm 2's count.
    #[test]
    fn the_grained_picture_the_caller_receives_carries_no_unwritten_plane_sample() {
        const NAME: &str =
            "the_grained_picture_the_caller_receives_carries_no_unwritten_plane_sample";
        let _gate_lock = lock_gate_counters();

        let (syn, syn_scanned) = grain_census_synthetic();
        assert_eq!(
            syn_scanned, 40,
            "{NAME}: the synthetic control scanned {syn_scanned} samples, expected 40 \
             (16 + 16 + 8) -- the grained census did not look at what it was handed"
        );
        assert_eq!(
            syn, 5,
            "{NAME}: the grained census reported {syn} for planes carrying exactly 5 sentinel \
             samples -- it is not measuring what it claims, so every zero below is \
             meaningless (class: census-silently-blind). Note 5, not the pre-grain arm's 7: \
             reading the wrong counter is the failure this number catches."
        );

        // Small and cheap: a 4:2:2 grain stream, a 4:2:0 one that also runs
        // CDEF+LR, and the real 10-bit 3840x1608 hidden-alt-ref film whose height
        // is not a multiple of the 32-pixel band row -- the shape where the band
        // row partition has a tail.
        for fixture in [
            "s422_grain_160x128",
            "grain_cdef_lr_128x128",
            "reelb_arf_witness",
        ] {
            let (n, scanned) = grain_census(fixture);
            assert!(
                scanned > 0,
                "{NAME}: the grained census scanned 0 samples on {fixture} -- the call at \
                 `apply_grain`'s return is not running (or grain never fired), so the {n} \
                 below proves nothing (class: census-never-ran). Deleting the call in \
                 apply_grain reproduces this."
            );
            assert_eq!(
                n, 0,
                "{NAME}: {fixture} hands the caller a grained picture with {n} samples the \
                 grain destination's band copy never wrote"
            );
        }
    }

    /// The child half of
    /// [`the_frame_the_caller_receives_carries_no_unwritten_plane_sample`]: decodes
    /// the fixture named in `UWDEP_CENSUS_FIXTURE` with the sentinel fill live and
    /// prints `UWDEP_CENSUS <n> frames=<n>` plus a frame-count assertion.
    ///
    /// A `#[test]` rather than an example because it needs the crate's own
    /// in-process counter -- which no external binary could read.
    #[test]
    fn uwdep_child_unwritten_census() {
        // Reached with NEITHER variable set means a normal suite run: the parent
        // invokes this test BY NAME with the variable set, so there is nothing to
        // measure here. Asserting the fill instead made every full suite red
        // (`stream::tests::uwdep_child_unwritten_census` is in the default test
        // list), and the run it wanted to catch is already caught on the parent's
        // side, which panics when the child prints no census line. A standalone
        // run SAYS it skipped rather than passing silently.
        if std::env::var_os("UWDEP_CENSUS_SYNTHETIC").is_none()
            && std::env::var_os("UWDEP_CENSUS_FIXTURE").is_none()
        {
            println!("UWDEP_CENSUS skipped: not invoked as a census child");
            return;
        }
        assert!(
            std::env::var_os("EC_AV1_PLANE_SENTINEL").is_some(),
            "the census child ran without EC_AV1_PLANE_SENTINEL, so the fill was never live \
             and the count is meaningless"
        );
        // The non-vacuity control: no decode, no fixture, just the census over
        // planes with a KNOWN number of sentinel samples. 3 + 4 + 0 = 7.
        if std::env::var_os("UWDEP_CENSUS_SYNTHETIC").is_some() {
            const S: u16 = 0xDEAD;
            let mut a = vec![7u16; 16];
            let mut b = vec![7u16; 16];
            let c = vec![7u16; 8];
            for s in a.iter_mut().take(3) {
                *s = S;
            }
            for s in b.iter_mut().take(4) {
                *s = S;
            }
            let _ = decode::take_final_unwritten_samples();
            decode::census_unwritten_final(&a, &b, &c);
            let n = decode::take_final_unwritten_samples();
            let scanned = decode::take_final_census_scanned();
            println!("UWDEP_CENSUS {n} synthetic=1 scanned={scanned}");
            // lane-av1graincensus: the grained census over the SAME planes but a
            // DIFFERENT known count, 2 + 3 + 0 = 5. Deliberately not 7: a parent
            // that read the pre-grain counter for the grained arm would see 7 and
            // go red, so the two arms cannot be confused for one another.
            for s in a.iter_mut().take(3) {
                *s = 7;
            }
            for s in b.iter_mut().take(4) {
                *s = 7;
            }
            for s in a.iter_mut().take(2) {
                *s = S;
            }
            for s in b.iter_mut().take(3) {
                *s = S;
            }
            let _ = decode::take_grained_unwritten_samples();
            decode::census_unwritten_grained(&a, &b, &c);
            let gn = decode::take_grained_unwritten_samples();
            let gscanned = decode::take_grained_census_scanned();
            println!("GRAINCENSUS {gn} synthetic=1 scanned={gscanned}");
            return;
        }
        let Ok(fixture) = std::env::var("UWDEP_CENSUS_FIXTURE") else {
            // Not a census run: the parent invokes this test by name, so being
            // reached without the variable means a normal suite run.
            return;
        };
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("fixtures/{fixture}.obu"));
        let stream =
            std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let _ = decode::take_final_unwritten_samples();
        // Both counters are drained BEFORE the decode: they are process-global
        // and this child may inherit nothing, but a partial run must not be able
        // to add its numbers to another test's.
        let _ = decode::take_grained_unwritten_samples();
        let frames = decode_stream(&stream).unwrap_or_else(|e| panic!("{fixture} refused: {e}"));
        let n = decode::take_final_unwritten_samples();
        let scanned = decode::take_final_census_scanned();
        println!("UWDEP_CENSUS {n} frames={} scanned={scanned}", frames.len());
        // lane-av1graincensus: the post-grain census, over the picture
        // `apply_grain` returns. A different prefix from `UWDEP_CENSUS`, so
        // neither parent can pick up the other's line.
        let gn = decode::take_grained_unwritten_samples();
        let gscanned = decode::take_grained_census_scanned();
        println!(
            "GRAINCENSUS {gn} frames={} scanned={gscanned}",
            frames.len()
        );
    }

    /// lane-av1uballoc: the frame the caller receives does not depend on what
    /// the plane buffer held before the decode wrote it.
    ///
    /// Four runs of the same fixtures, each in its own process (the fill and
    /// the alloc closure are both process-constant env reads -- see
    /// [`crate::envflags`] -- and this crate forbids `set_var`), must agree on
    /// one hash per fixture:
    ///
    /// * `EC_AV1_PLANE_SENTINEL=0xDEAD` and `=0xBEEF` -- two DIFFERENT
    ///   impossible values for the same buffer. This is the arm that can
    ///   actually FAIL: if any reader's value reaches the picture, changing the
    ///   poison changes the output. Equal hashes are a statement about a READ,
    ///   which is what the poisoned-read check in [`decode::fresh_plane`] needs and what no
    ///   census in the crate measures.
    /// * the sentinel OFF (the shipped zeroed allocation), which must agree
    ///   with the poisoned runs. This is the "same with the sentinel off"
    ///   arm; on its own it is weak (the allocator may hand out the same
    ///   ambient bytes twice) and is here as the join between the two worlds.
    ///
    /// NON-VACUITY, first and synthetic: [`allocdet_synthetic`] hashes two
    /// pictures that differ in ONE sample and asserts the hashes DIFFER. A hash
    /// that cannot see a one-sample difference would make every equality below
    /// vacuous, and it is the failure mode a "0 errors" style result has.
    /// Second: every arm must report `frames>0`, so a child that decoded
    /// nothing cannot agree with another child that decoded nothing.
    #[test]
    fn the_frame_does_not_depend_on_the_plane_buffers_initial_content() {
        const NAME: &str = "the_frame_does_not_depend_on_the_plane_buffers_initial_content";
        let _gate_lock = lock_gate_counters();
        // Kept SMALL on purpose: the whole gate is 4 child processes x this
        // list, and the suite runs it in a DEBUG build. A first revision used
        // the 1920x792 film fixtures here and took 252 s, which is not a gate
        // anyone keeps. These ten cover every shape the region list names --
        // 4:2:2 (12-bit key, inter, sb128 intrabc, 17-frame odd-size), 4:4:4
        // (lossless sb64, intrabc rect4, 128-root mu), a padded odd size, an
        // off-tile chroma walk, and superres+CDEF+LR -- for ~1.3 s per arm.
        //
        // lane-av1graincensus adds FILM GRAIN. `apply_grain`'s `uninit_plane` is
        // the crate's only OTHER `Vec::with_capacity` + `set_len` allocation, and
        // before this row the only gate that can catch a READ of it had no grain
        // fixture at all, so that path had no coverage here. It matters: with
        // the grain band's clean-row copy neutered, the noise kernels read the
        // destination to build `avgLuma` and the picture's values then depend on
        // whatever the buffer held -- a dependence the 0xDEAD-vs-0xBEEF arm
        // exists to catch and the unwritten-sample census cannot (the read value
        // is overwritten before the census runs). Measured in
        // `lanes/av1graincensus.report.md` §5. `s422_grain_160x128` is the
        // smallest grain fixture, so the arm grows by ~0.05 s.
        const FIXTURES: &[&str] = &[
            "s422_12bit_160x128",
            "422_inter_160x128_3f",
            "422_intrabc_sb128_strip",
            "s422_352x242_10b",
            "ll444_sb64_1to4_lossless",
            "444_intrabc_rect4_witness",
            "444_intra_in_inter_128root_mu_chroma",
            "svt1-split-tx-pin",
            "mix176_offtile_chroma_clip",
            "superres_kf_cdef_lr_64x64",
            "s422_grain_160x128",
        ];

        let (a, b) = allocdet_synthetic();
        assert_ne!(
            a, b,
            "{NAME}: the content hash returned the same value for two pictures differing in one \
             sample, so every equality below is vacuous -- this gate would pass on any decoder, \
             including one that returns garbage (class: hash-cannot-see-content)"
        );

        let arms: [(&str, Option<&str>, &str); 3] = [
            ("poison-dead", Some("0xDEAD"), "zeroed"),
            ("poison-beef", Some("0xBEEF"), "zeroed"),
            ("alloc-zeroed", None, "zeroed"),
        ];
        let mut baseline: Option<Vec<(String, usize, u64)>> = None;
        for (label, poison, alloc) in arms {
            let rows = allocdet_child(FIXTURES, poison, alloc);
            assert_eq!(
                rows.len(),
                FIXTURES.len(),
                "{NAME}: the {label} arm reported {} of {} fixtures -- a run that decoded \
                 nothing can agree with another run that decoded nothing",
                rows.len(),
                FIXTURES.len()
            );
            for (f, frames, _) in &rows {
                assert!(
                    *frames > 0,
                    "{NAME}: the {label} arm decoded 0 frames of {f}; the hashes it compared are \
                     over nothing"
                );
            }
            match &baseline {
                None => baseline = Some(rows),
                Some(first) => {
                    for (i, (f, frames, h)) in rows.iter().enumerate() {
                        let (bf, bframes, bh) = &first[i];
                        assert_eq!(
                            bf, f,
                            "{NAME}: arm {label} reported fixture {f} where the \
                             baseline reported {bf} -- the arms are not comparable"
                        );
                        assert_eq!(
                            bframes, frames,
                            "{NAME}: {f} decoded {frames} frames under \
                             {label} and {bframes} under the baseline arm"
                        );
                        assert_eq!(
                            bh, h,
                            "{NAME}: {f} decoded to a DIFFERENT picture depending on what the \
                             plane buffer held before the decode wrote it -- {label} gave {h:016x}, \
                             the baseline arm gave {bh:016x}. A read of uninitialised memory \
                             reaches the caller's frame (see decode::fresh_plane)."
                        );
                    }
                }
            }
        }
    }

    /// The child half of
    /// [`the_frame_does_not_depend_on_the_plane_buffers_initial_content`]:
    /// decodes every fixture named in `ALLOCDET_FIXTURES` and prints one
    /// `ALLOCDET` line each.
    ///
    /// With `ALLOCDET_SYNTHETIC` set it instead prints the two hashes the
    /// non-vacuity control compares -- two pictures differing in ONE sample --
    /// and decodes nothing.
    #[test]
    fn allocdet_child_decode() {
        if std::env::var_os("ALLOCDET_SYNTHETIC").is_some() {
            let (a, b) = allocdet_synthetic();
            println!("ALLOCDET_SYNTHETIC {a:016x} {b:016x}");
            return;
        }
        let Ok(list) = std::env::var("ALLOCDET_FIXTURES") else {
            // Reached without either variable == a normal suite run: the parent
            // names this test with ALLOCDET_FIXTURES (or ALLOCDET_SYNTHETIC) set,
            // so there is nothing to measure here. Asserting the variable instead
            // made every full suite red (caught by the server suite at the merged
            // head); the parent already fails when the child prints no ALLOCDET
            // line, so a skip cannot hide a broken child. Print rather than
            // pass silently.
            println!("ALLOCDET skipped: not invoked as a census child");
            return;
        };
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for fixture in list.split(',').filter(|s| !s.is_empty()) {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("fixtures/{fixture}.obu"));
            let stream =
                std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            let mut frames = 0usize;
            decode_stream_with(&stream, |pic, _idx, shown| {
                if !shown {
                    return Ok(());
                }
                frames += 1;
                for p in [&pic.y, &pic.u, &pic.v] {
                    for &s in p.iter() {
                        for b in s.to_le_bytes() {
                            hash ^= u64::from(b);
                            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                        }
                    }
                }
                Ok(())
            })
            .unwrap_or_else(|e| panic!("{fixture} refused: {e}"));
            println!("ALLOCDET {fixture} frames={frames} hash={hash:016x}");
        }
    }

    /// lane-t900 r6: the 128-superblock INTER witness.
    /// `crates/ec-av1/fixtures/reelc_sb128_inter_witness.obu`, 177773 bytes,
    /// sha256 `09d995946d27695ef4b7b1a24a4d7ad85f8e2c8176d58ee54e825efc061f9cbc`:
    /// the first 23 frame-carrying OBUs of a 2 s cut taken 900 s into a 10-bit
    /// 1920x792 AV1 stream with `use_128x128_superblock=1`, 16 decode-order
    /// frames, 15 shown. Cut twice independently from the source (`-ss 900 -t 2
    /// -c:v copy` then the OBU truncation) -- both cuts hash identically.
    /// lane-t900 r7 extended it from 19 OBUs / 14 frames: decode-order frame 14
    /// is the ONLY frame in this repository's fixtures whose sub-8x8 inter
    /// chroma inherits a `tx_type` from a chroma-reference sub-block that
    /// differs from its group's first sub-block AND codes coefficients
    /// (`sub8_chroma_tx_from_ref_hits`), i.e. the only frame that can tell
    /// `av1_get_tx_type`'s two candidate rules apart.
    ///
    /// It is the first stream in this repository that decodes exactly while
    /// five separately-found defects' paths all fire, which is why the asserts
    /// below are on all of them:
    ///
    /// * `warp_plane_suppress_hits` (r5) -- `av1_init_warp_params` bails out at
    ///   `block_width < 8 || block_height < 8` on the PLANE's own block, so the
    ///   4x8 chroma of an 8x16 luma block is predicted translationally while its
    ///   luma warps. Gated on the square side before r5.
    /// * `inter_sb128_vert_hits` + `cdef_idx_hits` (r5) -- the second half of a
    ///   128-root HORZ/VERT inter block must read its OWN `cdef_idx` literal
    ///   (libaom `cdef_transmitted[4]`, one flag per 64x64 CDEF unit).
    /// * `interintra_rect_hits` (r4) -- interintra on a RECTANGULAR block built
    ///   a square intra predictor before.
    /// * `non_chroma_ref_ctx_skip_hits` (r3) -- a block that is not the chroma
    ///   reference of its 4:2:0 pair leaves its neighbours' chroma coefficient
    ///   contexts untouched (`nplanes = 1 + (num_planes - 1) * is_chroma_ref`).
    /// * `intra_in_inter_txctx_hits` (r2) -- an intra strip in an INTER frame
    ///   takes its `tx_depth` row from the real `TXFM_CONTEXT` bands, not the
    ///   key frame's deblock grid.
    /// * `edge_filter_mi_fix_hits` (r4) -- both halves of a split intra block
    ///   read their OWN mi-exact intra-edge-filter neighbours.
    ///
    /// It is ALSO the witness that lifted the intra-16x4-in-inter opt-in -- that path decodes by default now, gated here:
    /// `intra16x4_in_inter` fires 78x 16x4 + 149x 4x16 (108 chroma-paired)
    /// inside frames that are decoded AND compared exact -- not counted out of
    /// a refused or desynced stream the way lane-sub8x4 r3's phantom was
    /// (class `counter-from-refused-stream`). The 128x64/64x128 intra-in-inter
    /// arm fires 0 times here, so THAT refusal is untouched.
    ///
    /// All 14 decode-order frames (including the 2 no-show frames ffmpeg never
    /// hands back) were compared byte-exact against instrumented aomdec with
    /// `EC_AV1_FINAL_DUMP` -- see the lane report's EVIDENCE line; the gate
    /// itself compares the 12 shown frames on every plane.
    #[test]
    fn a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_decode_pixel_exact() {
        const NAME: &str =
            "a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/reelc_sb128_inter_witness.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (1920usize, 792usize);
        let before = (
            crate::decode::warp_plane_suppress_hits(),
            crate::decode::cdef_idx_hits(),
            crate::decode::inter_sb128_vert_hits(),
            crate::decode::interintra_rect_hits(),
            crate::decode::non_chroma_ref_ctx_skip_hits(),
            crate::decode::intra_in_inter_txctx_hits(),
            crate::decode::edge_filter_mi_fix_hits(),
        );
        let sub8tx_before = crate::decode::sub8_chroma_tx_from_ref_hits();
        // lane-t900 r8: the intra edge-filter type an INTER neighbour keeps
        // non-smooth (the coarse SUB band still holds an older SMOOTH mode).
        let notsmooth_before = crate::decode::inter_neighbour_not_smooth_hits();
        // lane-t900 r10: the rect warp top-right reach (600 blocks here).
        let tr_before = crate::decode::tr_reach_longer_side_hits();
        let i16_before = crate::decode::intra16x4_in_inter_hits();
        // lane-t900 r14: OBMC neighbour strips that run past the frame edge
        // (792 luma rows = 198 mi rows, so every bottom-row 32x32 block
        // overhangs) -- the shape whose prediction size used to be clamped.
        let edge_before = crate::decode::obmc_edge_span_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let obmc_edge_span = crate::decode::obmc_edge_span_hits() - edge_before;
        let fired = (
            crate::decode::warp_plane_suppress_hits() - before.0,
            crate::decode::cdef_idx_hits() - before.1,
            crate::decode::inter_sb128_vert_hits() - before.2,
            crate::decode::interintra_rect_hits() - before.3,
            crate::decode::non_chroma_ref_ctx_skip_hits() - before.4,
            crate::decode::intra_in_inter_txctx_hits() - before.5,
            crate::decode::edge_filter_mi_fix_hits() - before.6,
        );
        let sub8tx = crate::decode::sub8_chroma_tx_from_ref_hits() - sub8tx_before;
        let notsmooth = crate::decode::inter_neighbour_not_smooth_hits() - notsmooth_before;
        let tr = crate::decode::tr_reach_longer_side_hits() - tr_before;
        let i16_now = crate::decode::intra16x4_in_inter_hits();
        let i16 = (
            i16_now.0 - i16_before.0,
            i16_now.1 - i16_before.1,
            i16_now.2 - i16_before.2,
        );
        for (what, n) in [
            ("warp_plane_suppress", fired.0),
            ("cdef_idx", fired.1),
            ("inter_sb128_vert", fired.2),
            ("interintra_rect", fired.3),
            ("non_chroma_ref_ctx_skip", fired.4),
            ("intra_in_inter_txctx", fired.5),
            ("edge_filter_mi_fix", fired.6),
            ("intra16x4_in_inter 16x4", i16.0),
            ("intra16x4_in_inter 4x16", i16.1),
            ("intra16x4_in_inter chroma_ref", i16.2),
            ("sub8_chroma_tx_from_ref", sub8tx),
            ("inter_neighbour_not_smooth", notsmooth),
            ("tr_reach_longer_side", tr),
            ("obmc_edge_span", obmc_edge_span),
        ] {
            assert!(
                n > 0,
                "{NAME}: {what} never fired on this stream -- the gate would be vacuous for it"
            );
        }
        assert_eq!(frames.len(), 15, "{NAME}: 15 shown frames");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len());
        assert_eq!(
            ffmpeg_frames.len(),
            frames.len(),
            "{NAME}: ffmpeg returned {} frames, we decoded {}",
            ffmpeg_frames.len(),
            frames.len()
        );
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!(
            "{NAME}: 15 shown frames pixel-exact on every plane; warp_plane_suppress={} \
             cdef_idx={} inter_sb128_vert={} interintra_rect={} non_chroma_ref_ctx_skip={} \
             intra_in_inter_txctx={} edge_filter_mi_fix={} intra16x4_in_inter={:?} \
             sub8_chroma_tx_from_ref={sub8tx} inter_neighbour_not_smooth={notsmooth} \
             tr_reach_longer_side={tr} obmc_edge_span={obmc_edge_span}",
            fired.0, fired.1, fired.2, fired.3, fired.4, fired.5, fired.6, i16
        );
    }

    /// lane-t900 r16/r17: a 10-bit 512x512 `--sb-size=128` stream in which
    /// every 128 root is a var-tx block, and the gate for the SKIPPED-128-block
    /// luma entropy reset (r17). It was BUILT as an intra-128-in-inter witness
    /// and r16 measured three hits of that arm in it -- every one of them an
    /// artifact of the desync fixed here (class `counter-from-refused-stream`).
    /// Decoding exactly, the arm fires ZERO times, so this stream does not
    /// carry that shape; the intra-128-in-inter path decodes by default now, gated by `an_intra_coded_128_half_inside_an_inter_frame_decodes_pixel_exact`. Renamed
    /// out of `an_intra_coded_128x64_block_inside_an_inter_frame_...` for that
    /// reason -- the fixture keeps the original intent in its own name.
    ///
    /// `crates/ec-av1/fixtures/intra128_in_inter_witness.obu`, 13066 bytes,
    /// sha256 `dd41fc39367a3d38bf8a8a8106dc30d91b716b2a9123fdfa4716930f4f98caf9`,
    /// produced by this repository's aomenc oracle:
    ///
    /// ```text
    /// aomenc --codec=av1 -w 512 -h 512 --input-bit-depth=10 --bit-depth=10 \
    ///   --sb-size=128 --min-partition-size=64 --max-partition-size=128 \
    ///   --enable-rect-partitions=1 --kf-max-dist=9999 --lag-in-frames=0 \
    ///   --cpu-used=1 --end-usage=q --cq-level=24 --threads=1 --obu -o s.obu src.y4m
    /// ```
    ///
    /// The source is 10 frames of 10-bit 512x512 whose left two superblock
    /// columns carry 128-wide bands 64 rows apart (so the 128 root's cheapest
    /// split is `PARTITION_HORZ`) and whose right two carry 64-wide vertical
    /// bands (`PARTITION_VERT`), with two hard scene cuts at frames 4 and 7 and
    /// `--kf-max-dist=9999` so those cuts must be coded as INTER frames whose
    /// content no reference holds. It is the first stream found in this
    /// repository that makes the encoder code a 128-root HORZ/VERT half INTRA
    /// inside an inter frame at all: `intra128_in_inter` fires 1x 128x64 +
    /// 2x 64x128. Three recipes that do NOT fire it, so the shape matters: a
    /// small new patch on a static background (the frame costs 80 bytes, the
    /// encoder skips), unstructured texture (the 128 root always SPLITs), and
    /// cyclically repeating phases (GOLDEN holds an earlier phase, so the cut
    /// is predicted rather than coded intra).
    ///
    /// r16 could not lift the opt-in: the three hits landed in a frame this
    /// decoder did not decode exactly, and a counter read out of a desynced
    /// frame proves nothing (class `counter-from-refused-stream`). The blocker
    /// was a SEPARATE defect on the inter path, not this arm -- the same source
    /// re-encoded with every optional tool disabled fired the arm 0 times and
    /// desynced on exactly the same frames, and the same source at
    /// `--sb-size=64` decoded all 10 frames byte-exact.
    ///
    /// r17 root cause, and what this gate now pins (class
    /// `early-return-skips-tail`): a SKIPPED inter block
    /// whose `read_block_tx_size` still hands back var-tx leaves -- which every
    /// 128 root does under `TX_MODE_LARGEST` -- took `decode_inter_block`'s
    /// `if skip` arm, so the per-transform-unit luma publication loop never
    /// ran, and the block-level `record_split_luma_rect_mi` deliberately writes
    /// no plane 0. The skipped block therefore left the PREVIOUS block's luma
    /// coefficient levels standing where libaom's `av1_reset_entropy_context`
    /// zeroes all three planes over the block. First-bad before the fix
    /// (`EC_TRACE_COEFF` on both decoders, prefiltered to the
    /// `all_zero`/`eob`/`after_bases` tags, filtered index 426): frame 1, the
    /// 128x128 `is_inter=1 skip=0` block at mi(32,64), its first luma TU
    /// (`bc=0 br=0`, TX_64X64) -- aomdec `txb_skip` ctx 1 (`top == 0 &&
    /// left == 0`), ours ctx 2, entry range equal on both, because the skipped
    /// 128x128 block at mi(32,32) never cleared `left[32..48]` (ours 1,
    /// aomdec 0). With the publication restored the whole 10-frame filtered
    /// ladder matches 1397/1397 steps.
    ///
    #[test]
    fn a_128_superblock_stream_whose_skipped_blocks_are_var_tx_decodes_pixel_exact() {
        const NAME: &str =
            "a_128_superblock_stream_whose_skipped_blocks_are_var_tx_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/intra128_in_inter_witness.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (512usize, 512usize);
        let before = crate::decode::skip_vartx_luma_reset_hits();
        let intra128_before = intra128_in_inter_counters();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let fired = crate::decode::skip_vartx_luma_reset_hits() - before;
        assert!(
            fired > 0,
            "{NAME}: no skipped var-tx block cleared its own luma coefficient \
             contexts -- the gate would be vacuous for the defect it pins"
        );
        let intra128_now = intra128_in_inter_counters();
        assert_eq!(
            [
                intra128_now[0] - intra128_before[0],
                intra128_now[1] - intra128_before[1]
            ],
            [0, 0],
            "{NAME}: this stream does not carry a 128-root intra half -- the \
             witness for that arm is `intra128_in_inter_witness2.obu` (r18)"
        );
        assert_eq!(frames.len(), 10, "{NAME}: 10 shown frames");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len());
        assert_eq!(
            ffmpeg_frames.len(),
            frames.len(),
            "{NAME}: ffmpeg returned {} frames, we decoded {}",
            ffmpeg_frames.len(),
            frames.len()
        );
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!(
            "{NAME}: 10 shown frames pixel-exact on every plane; \
             skip_vartx_luma_reset={fired}"
        );
    }

    /// lane-t900 r18 GATE: a 128-root `PARTITION_HORZ`/`PARTITION_VERT` half
    /// coded INTRA inside an INTER frame -- the shape an opt-in guarded from
    /// lane-inter128intra r1 until this round; it decodes by default now, gated by this stream.
    ///
    /// What r16 and r17 could not find, and what this recipe exploits: libaom
    /// does not PICK a 128-root HORZ/VERT in the frame interior -- r16's
    /// 512x512 witness codes its 128 roots as NONE (96) or SPLIT (64) and
    /// nothing else, and a textured 640x384 sweep the same. The shape appears
    /// where the frame EDGE forces it: with a partial last superblock ROW
    /// `has_rows128 == false`, so `PARTITION_HORZ` is implied and only the
    /// 128x64 top half is decoded; a partial last COLUMN implies the 64x128
    /// left half the same way (the arm the
    /// `..._sb128_gathered_edge_horz_partition_...` gate pins for INTER
    /// blocks). Put content no reference holds in exactly those two bands and
    /// the encoder codes those forced halves INTRA.
    ///
    /// `crates/ec-av1/fixtures/intra128_in_inter_witness2.obu`, 86516 bytes,
    /// sha256 `cace4cc3a5ba4dc5f8c7c219e87c9052351e832cad3d7f4c8ffe09288a6d7ccf`,
    /// produced by this repository's aomenc oracle:
    ///
    /// ```text
    /// aomenc --codec=av1 -w 704 -h 320 --input-bit-depth=10 --bit-depth=10 \
    ///   --sb-size=128 --max-partition-size=128 --enable-rect-partitions=1 \
    ///   --enable-ab-partitions=0 --enable-1to4-partitions=0 \
    ///   --enable-intrabc=0 --enable-palette=0 --tune=psnr \
    ///   --kf-max-dist=9999 --lag-in-frames=0 --cpu-used=1 --end-usage=q \
    ///   --cq-level=20 --threads=1 --passes=1 --obu -o s.obu src.y4m
    /// ```
    ///
    /// The source is 15 frames of 10-bit 704x320 (704 = 5*128 + 64 and
    /// 320 = 2*128 + 64, so BOTH the last superblock column and the last
    /// superblock row are partial) whose interior is a low-passed noise
    /// texture panning 4 px per frame -- an exact integer translation, so
    /// inter predicts it and intra never would -- and whose bottom 64 rows and
    /// right 64 columns are replaced at frames 6, 9 and 12 by a fresh smooth
    /// diagonal ramp with a different angle and offset each time. No
    /// reference holds that content (`--lag-in-frames=0`, so there is no
    /// hidden alt-ref either), it is internally smooth, and it lands exactly
    /// in the edge-forced halves: `intra128_in_inter` fires 3x 128x64 and
    /// 6x 64x128, every one of them inside a frame this gate compares
    /// byte-exact against ffmpeg (class `counter-from-refused-stream`).
    #[test]
    fn an_intra_coded_128_half_inside_an_inter_frame_decodes_pixel_exact() {
        const NAME: &str = "an_intra_coded_128_half_inside_an_inter_frame_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/intra128_in_inter_witness2.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (704usize, 320usize);
        let before = intra128_in_inter_counters();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let after = intra128_in_inter_counters();
        let fired = [after[0] - before[0], after[1] - before[1]];
        assert!(
            fired[0] > 0 && fired[1] > 0,
            "{NAME}: intra-128-in-inter fired {fired:?} (128x64, 64x128) -- both \
             arms must fire or the gate is vacuous for the one that did not"
        );
        assert_eq!(frames.len(), 15, "{NAME}: 15 shown frames");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len());
        assert_eq!(
            ffmpeg_frames.len(),
            frames.len(),
            "{NAME}: ffmpeg returned {} frames, we decoded {}",
            ffmpeg_frames.len(),
            frames.len()
        );
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!(
            "{NAME}: 15 shown frames pixel-exact on every plane; \
             intra128_in_inter 128x64={} 64x128={}",
            fired[0], fired[1]
        );
    }

    /// lane-t900 r32 GATE for the LIFTED refusal "a HORZ/VERT intra strip in
    /// a screen-content frame (palette syntax is consumed for square blocks
    /// only)": the r18 edge-forced recipe re-encoded with
    /// `--tune-content=screen`, i.e. the exact case that refusal guarded.
    ///
    /// `crates/ec-av1/fixtures/intra128_in_inter_screen_witness.obu`, 119048
    /// bytes, sha256
    /// `c84a493f90327f5d2bea038a42e87cbfe56762b5c6a856d76b4bff9797c5ea09`,
    /// from this repository's aomenc oracle over r18's own 10-bit 704x320
    /// source (`gen3.py`): the r18 flag set plus `--tune-content=screen
    /// --enable-palette=0 --enable-intrabc=0 --cq-level=20`. Three cq levels
    /// were swept (20/35/50); all three fire, and the `--enable-rect-\
    /// partitions=0` twin of each codes ZERO 128-root halves, which is what
    /// says the shape here is the coded HORZ/VERT arm and not an artefact.
    ///
    /// The lift's proof is r31's enumeration
    /// (`no_block_footprint_with_a_128_pixel_side_can_carry_palette_syntax`):
    /// `av1_allow_palette`'s `bw > 64 || bh > 64` bound means a 128x64/64x128
    /// half codes no palette syntax at all, so the screen flag changes nothing
    /// about its syntax. This is the WITNESS that the lifted arm is reachable
    /// and decodes, which r23's census could not produce.
    #[test]
    fn an_intra_coded_128_half_in_a_screen_content_inter_frame_decodes_pixel_exact() {
        const NAME: &str =
            "an_intra_coded_128_half_in_a_screen_content_inter_frame_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/intra128_in_inter_screen_witness.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (704usize, 320usize);
        // The refusal named `allow_screen_content_tools`, so the gate reads
        // that bit out of the frame headers rather than trusting the encoder
        // flag (class `gate-blind-to-feature`).
        let mut screen_frames = 0u32;
        {
            let mut p = Av1Parser::new();
            let mut pos = 0usize;
            while pos < stream.len() {
                let obu = p.parse_obu(&stream[pos..]).expect("parse");
                pos += obu.total_size;
                let header = match &obu.kind {
                    ObuKind::Frame(h, _) => Some(&**h),
                    ObuKind::FrameHeader(h) => Some(&**h),
                    _ => None,
                };
                if header.is_some_and(|h| h.allow_screen_content_tools) {
                    screen_frames += 1;
                }
            }
        }
        assert!(
            screen_frames > 0,
            "{NAME}: no frame sets allow_screen_content_tools -- the gate no longer \
             exercises the lifted refusal's own condition"
        );
        let before = intra128_in_inter_counters();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let after = intra128_in_inter_counters();
        let fired = [after[0] - before[0], after[1] - before[1]];
        assert!(
            fired[0] > 0 && fired[1] > 0,
            "{NAME}: intra-128-in-inter fired {fired:?} (128x64, 64x128) -- both arms \
             must fire or the lift is unwitnessed at the one that did not"
        );
        assert_eq!(frames.len(), 15, "{NAME}: 15 shown frames");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len());
        assert_eq!(
            ffmpeg_frames.len(),
            frames.len(),
            "{NAME}: frame count vs ffmpeg"
        );
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!(
            "{NAME}: {screen_frames} screen-content frame(s), 15 shown frames pixel-exact \
             on every plane; intra128_in_inter 128x64={} 64x128={}",
            fired[0], fired[1]
        );
    }

    /// lane-t900 r22 GATE: a real `aomenc --tune-content=screen
    /// --enable-palette=1` stream whose INTER frames reconstruct palette
    /// blocks (Y and UV), at both bit depths.
    ///
    /// Recipe (both fixtures, 15 frames, key + 14 inter, 256x192, one tile):
    /// `aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=1
    /// --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=9999
    /// --sb-size=64 --tune-content=screen --enable-palette=1
    /// --enable-intrabc=0 --obu` (plus `--bit-depth=10 --profile=0` for the
    /// twin) over a synthetic screen-like source: two luma levels on a 16x16
    /// grid, four chroma levels on an 8x8 one, both re-keyed per frame so the
    /// encoder cannot predict the new blocks and codes them INTRA inside the
    /// inter frames. `--enable-palette=0` on the same source is 13% larger,
    /// which is what says the tool is really used.
    ///
    /// What this pins that no key-frame palette gate can: the palette
    /// colour-index CDFs are this decoder's only VARIABLE-ALPHABET tables (the
    /// reader takes `[..=n]` of a `[u16; 9]` row, so `update_cdf`'s counter
    /// lives at index `n`), and `Cdfs::reset_counts` zeroed slot 8 instead --
    /// so every colour-index row crossed a frame boundary with a SATURATED
    /// count and adapted at rate 6 where libaom, counting from 0, adapts at 4.
    /// The first inter-frame palette block's map desynced at its THIRD
    /// colour-index symbol (decode-order frame 3, mi(40,0), map cell (0,2):
    /// ours cdf0 = 32768-1071, aomdec's = 32768-971, same ctx, same value,
    /// different range).
    #[test]
    fn a_real_aomenc_screen_stream_with_palette_blocks_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_screen_stream_with_palette_blocks_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        for (fixture, ten_bit) in [
            ("fixtures/palette_screen_witness.obu", false),
            ("fixtures/palette_screen_witness_10bit.obu", true),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture);
            let stream = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
            let (width, height) = (256usize, 192usize);
            let before = intra_in_inter_palette_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => panic!("{NAME} [{fixture}]: decode_stream refused: {e}"),
            };
            let after = intra_in_inter_palette_hits();
            let (fired_y, fired_uv) = (after.0 - before.0, after.1 - before.1);
            assert!(
                fired_y > 0 && fired_uv > 0,
                "{NAME} [{fixture}]: inter-frame palette fired y={fired_y} uv={fired_uv} \
                 -- both planes must fire or the gate is vacuous for the one that did not"
            );
            assert_eq!(frames.len(), 15, "{NAME} [{fixture}]: 15 shown frames");
            let ffmpeg_frames = if ten_bit {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len())
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frames.len())
            };
            assert_eq!(
                ffmpeg_frames.len(),
                frames.len(),
                "{NAME} [{fixture}]: frame count"
            );
            for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    ours.y, theirs.y,
                    "{NAME} [{fixture}]: frame {i} luma vs ffmpeg"
                );
                assert_eq!(
                    ours.u, theirs.u,
                    "{NAME} [{fixture}]: frame {i} U vs ffmpeg"
                );
                assert_eq!(
                    ours.v, theirs.v,
                    "{NAME} [{fixture}]: frame {i} V vs ffmpeg"
                );
            }
            eprintln!(
                "{NAME} [{fixture}]: 15 frames pixel-exact on every plane; \
                 inter-frame palette y={fired_y} uv={fired_uv}"
            );
        }
    }

    /// lane-t900 r23 GATE for the two LIFTED refusals `"a block that actually
    /// uses a palette (Y)/(UV) -- reconstruction is out of scope"` at their
    /// 8x8-LEAF site ([`crate::decode`]'s `decode_inter_block8` intra arm):
    /// a palette block on an 8x8 intra leaf inside an inter frame.
    ///
    /// Recipe (`fixtures/palette_screen_leaf8_witness.obu`): 256x192, 15
    /// frames, a static two-level 16x16 screen background with a 96x96 patch
    /// of hashed 4x4 colour cells that moves non-periodically (so no
    /// reference holds it and the encoder codes it INTRA inside the inter
    /// frames), `aomenc --tune-content=screen --enable-palette=1
    /// --enable-intrabc=0 --sb-size=64 --cq-level=20 --cpu-used=1
    /// --lag-in-frames=0 --kf-max-dist=9999 --max-partition-size=8` -- the
    /// partition cap is what puts the palette on BLOCK_8X8 rather than on the
    /// 16x16/16x8 blocks r22's recipe produced. `EC_PALSYN=1` on aomdec
    /// counts 25 `bsize=BLOCK_8X8 palette_size>0` blocks past the key frame.
    ///
    /// What it caught beyond the refusal itself: a sub-8x8 group publishes no
    /// palette state, so it left the row's `left_palette_size` band holding an
    /// earlier 8x8 palette block's size and `av1_get_palette_mode_ctx` read 1
    /// where libaom gathers 0 (decode-order frame 1, mi(30,30), palette size
    /// decoded as 8 instead of 2) -- class `new-map-ignores-tile-edge`, fixed
    /// at all three sub-8x8 tails.
    #[test]
    fn a_screen_stream_with_palette_8x8_leaves_in_inter_frames_decodes_pixel_exact() {
        const NAME: &str =
            "a_screen_stream_with_palette_8x8_leaves_in_inter_frames_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/palette_screen_leaf8_witness.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let before = inter8_palette_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let after = inter8_palette_hits();
        let (fired_y, fired_uv) = (after.0 - before.0, after.1 - before.1);
        assert!(
            fired_y > 0,
            "{NAME}: no palette block on an 8x8 leaf inside an inter frame \
             -- the gate is vacuous for the site whose refusal it pins"
        );
        assert_eq!(frames.len(), 15, "{NAME}: 15 shown frames");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 256, 192, frames.len());
        assert_eq!(ffmpeg_frames.len(), frames.len(), "{NAME}: frame count");
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!("{NAME}: 15 frames pixel-exact; 8x8-leaf palette y={fired_y} uv={fired_uv}");
    }

    /// lane-t900 r22 GATE for the LIFTED refusal `"a HORZ/VERT intra strip in
    /// a screen-content frame (palette syntax is consumed for square blocks
    /// only)"` on its 16x4/4x16 arm: that arm routes through
    /// `decode_rect4_16_strip`, which takes `allow_screen_content_tools` and
    /// reads the whole palette syntax itself, so the refusal was a stale
    /// premise (the key-frame path proves that body). This 10-bit
    /// `--tune-content=screen` stream reached that exact refusal before the
    /// lift and now decodes all 15 frames pixel-exact while 8 inter-frame
    /// palette blocks reconstruct.
    #[test]
    fn a_screen_stream_with_16x4_intra_strips_in_inter_frames_decodes_pixel_exact() {
        const NAME: &str =
            "a_screen_stream_with_16x4_intra_strips_in_inter_frames_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/palette_screen_strip16_witness_10bit.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let before = intra_in_inter_palette_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let fired_y = intra_in_inter_palette_hits().0 - before.0;
        assert!(
            fired_y > 0,
            "{NAME}: no inter-frame palette block -- gate is vacuous"
        );
        assert_eq!(frames.len(), 15, "{NAME}: 15 shown frames");
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, 256, 192, frames.len());
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!("{NAME}: 15 frames pixel-exact; inter-frame palette y={fired_y}");
    }

    /// lane-t900 r20 GATE, refusal A -- `"a non-skip rectangular
    /// (HORZ/VERT/HORZ_B) strip needs rectangular residual coding"`.
    ///
    /// That string is a claim about a SHAPE, so what proves it is the set of
    /// shapes the inter block path presents. `inter_piece!` sets
    /// `reject_residual` to `$ww != $side || $wh != $side`, i.e. only for a
    /// block whose true footprint is not its square CDF context, and the
    /// refusal then needs `!rect_inter_residual_supported(write_w, write_h)`.
    /// So it can fire only for a footprint that is BOTH rectangular (or a
    /// square smaller than its own context) AND missing from that table.
    ///
    /// This gate measures the domain instead of asserting it:
    /// [`crate::decode::inter_block_shape_mask`] records every
    /// `(side, write_w, write_h)` triple decoded on this thread, with bit 31
    /// reserved for a triple [`crate::decode::INTER_BLOCK_SHAPES`] does not
    /// list. Three real streams -- a 10-bit 1920x792 128-superblock film cut,
    /// its 128-root intra witness and the 1920x792 small-side warp cut,
    /// together ~140k inter blocks -- decode without a refusal and present
    /// exactly 18 triples: the four square ones (`write == side`, where
    /// `reject_residual` is false) and the fourteen rectangular AV1 block
    /// shapes, every one of which `rect_inter_residual_supported` covers. No
    /// square-inside-a-larger-context triple appears at all.
    ///
    /// Measured 2026-09-04: mask covers 18/18 listed triples, bit 31 clear,
    /// 0 refusals.
    #[test]
    fn a_block_shape_census_over_three_real_streams_leaves_the_rect_residual_refusal_unreachable() {
        const NAME: &str = "a_block_shape_census_over_three_real_streams_leaves_the_rect_residual_refusal_unreachable";
        const REFUSAL: &str =
            "a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding";
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut blocks = 0usize;
        for name in [
            "reelc_sb128_inter_witness.obu",
            "intra128_in_inter_witness2.obu",
            "gm_small_side_witness.obu",
        ] {
            let path = dir.join(name);
            let stream = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
            match decode_stream(&stream) {
                Ok(frames) => {
                    assert!(!frames.is_empty(), "{NAME}: {name} decoded no frame");
                    blocks += frames.len();
                }
                Err(e) => {
                    assert!(
                        !format!("{e}").contains(REFUSAL),
                        "{NAME}: {name} hit the refusal this gate proves unreachable: {e}"
                    );
                    panic!("{NAME}: {name} refused: {e}");
                }
            }
        }
        assert!(blocks > 0, "{NAME}: no frames decoded");
        let mask = crate::decode::inter_block_shape_mask();
        assert_eq!(
            mask & crate::decode::INTER_BLOCK_SHAPE_UNLISTED,
            0,
            "{NAME}: a (side, write_w, write_h) triple outside INTER_BLOCK_SHAPES was decoded --              the census no longer describes the inter path's domain, so {REFUSAL:?} is not              proven by it"
        );
        for (i, &(side, w, h)) in crate::decode::INTER_BLOCK_SHAPES.iter().enumerate() {
            assert!(
                mask & (1u32 << i) != 0,
                "{NAME}: {side}: {w}x{h} never decoded -- the census would prove nothing about it"
            );
            // `reject_residual` is false for a square footprint (`write ==
            // side`), so only the rectangular ones can reach the refusal.
            if (w, h) != (side, side) {
                assert!(
                    crate::decode::rect_inter_residual_supported(w, h),
                    "{NAME}: {w}x{h} is decoded by real streams with reject_residual set but has                      no rectangular residual path -- {REFUSAL:?} is reachable"
                );
            }
        }
        eprintln!(
            "{NAME}: 3 streams, 0 refusals, {} of {} listed (side, write) triples decoded,              bit31 clear",
            crate::decode::INTER_BLOCK_SHAPES.len(),
            crate::decode::INTER_BLOCK_SHAPES.len()
        );
    }

    /// lane-t900 r20 GATE, refusal C -- `"an intra 16x4/4x16 strip inside an
    /// inter 16x16-level 1:4 partition (its 4:2:0 chroma pair is coded once
    /// for two strips; only the inter path implements that pairing)"`.
    ///
    /// The arm fires for an INTRA footprint with a side below 8 that carries
    /// no chroma-pair record (`strip16`). Two measurements make that a claim
    /// rather than a hope:
    ///
    /// * the block-shape census: the ONLY sub-8 footprints three real streams
    ///   present are 16x4 and 4x16 at a 16-px context, i.e. the strips of a
    ///   16x16-level 1:4 partition -- and that caller sets
    ///   `INTER_STRIP_CHROMA` for every one of its four strips before the
    ///   block is decoded, so the record is never missing there;
    /// * the sibling counter: the SUPPORTED path
    ///   ([`crate::decode::intra16x4_in_inter_hits`]) fires on both
    ///   orientations, chroma-paired, inside streams that decode with no
    ///   refusal at all.
    ///
    /// Measured 2026-09-04: 3 streams, 0 refusals, 78 16x4 + 149 4x16 (108
    /// chroma-paired), and 2 of 2 sub-8 census triples are the 1:4 strips.
    #[test]
    fn a_sub8_footprint_census_over_real_streams_leaves_the_intra_16x4_pairing_refusal_unreachable()
    {
        const NAME: &str = "a_sub8_footprint_census_over_real_streams_leaves_the_intra_16x4_pairing_refusal_unreachable";
        const REFUSAL: &str = "an intra 16x4/4x16 strip inside an inter 16x16-level 1:4 partition";
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let before = crate::decode::intra16x4_in_inter_hits();
        for name in [
            "reelc_sb128_inter_witness.obu",
            "intra128_in_inter_witness2.obu",
            "gm_small_side_witness.obu",
        ] {
            let path = dir.join(name);
            let stream = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
            if let Err(e) = decode_stream(&stream) {
                assert!(
                    !format!("{e}").contains(REFUSAL),
                    "{NAME}: {name} hit the refusal this gate proves unreachable: {e}"
                );
                panic!("{NAME}: {name} refused: {e}");
            }
        }
        let after = crate::decode::intra16x4_in_inter_hits();
        let fired = (after.0 - before.0, after.1 - before.1, after.2 - before.2);
        assert!(
            fired.0 > 0 && fired.1 > 0 && fired.2 > 0,
            "{NAME}: the SUPPORTED intra 16x4/4x16-in-inter path fired {fired:?} \
             (16x4, 4x16, chroma-paired) -- with a sibling at zero the refusal's claim is \
             untested from the other side"
        );
        let sub8: Vec<(usize, usize, usize)> = crate::decode::INTER_BLOCK_SHAPES
            .iter()
            .copied()
            .filter(|&(_, w, h)| w.min(h) < 8)
            .collect();
        assert_eq!(
            sub8,
            vec![(16, 4, 16), (16, 16, 4)],
            "{NAME}: the sub-8 footprints the inter path presents are no longer just the \
             16x16-level 1:4 strips, whose caller always records the chroma pair -- the \
             refusal's domain changed"
        );
        let mask = crate::decode::inter_block_shape_mask();
        for (i, &(side, w, h)) in crate::decode::INTER_BLOCK_SHAPES.iter().enumerate() {
            if w.min(h) < 8 {
                assert!(
                    mask & (1u32 << i) != 0,
                    "{NAME}: {side}: {w}x{h} never decoded -- the sub-8 census is vacuous"
                );
            }
        }
        eprintln!(
            "{NAME}: 3 streams, 0 refusals, intra16x4_in_inter 16x4={} 4x16={} chroma_ref={}, \
             sub-8 census triples {sub8:?}",
            fired.0, fired.1, fired.2
        );
    }

    /// lane-t900 r10: the small-side GLOBALMV / rect warp-reach witness.
    /// `crates/ec-av1/fixtures/gm_small_side_witness.obu`, 1158066 bytes,
    /// sha256 `f1cdbcbb8d6d469c7957d70252ab42b24ab75f48a717600ffa25991458775dca`:
    /// the first 50 frame-carrying OBUs of a 2 s cut taken 5400 s into a 10-bit
    /// 1920x792 AV1 stream with `use_128x128_superblock=1` -- 34 decode-order
    /// frames, 33 shown, every ARF in it coded with `allow_warped_motion=1`.
    /// Truncated twice independently from the same source, both cuts hash
    /// identically. All 34 DECODE-order frames (the hidden ARF included) were
    /// compared byte-exact against instrumented aomdec with `EC_AV1_FINAL_DUMP`
    /// -- see the lane report's EVIDENCE line; the gate itself compares the 33
    /// shown frames on every plane, which is all ffmpeg hands back.
    ///
    /// It is the only stream in this repository that fires BOTH r9/r10 paths:
    ///
    /// * `gm_nontrans_small_side_hits` -- libaom `is_nontrans_global_motion`
    ///   returns 0 at `AOMMIN(mi_size_wide, mi_size_high) < 2` BEFORE it looks
    ///   at any warp model, so a GLOBALMV block with a 4-px side still reads
    ///   its interp filter. We suppressed that symbol before r9.
    /// * `tr_reach_longer_side_hits` -- `av1_findSamples` probes the top-right
    ///   warp sample at `AOMMAX(xd->width, xd->height)` (mvref_common.c:1235)
    ///   with libaom's two rect shape refinements
    ///   (`is_last_vertical_rect` / `is_first_horizontal_rect`,
    ///   av1_common_int.h:1419-1428) applied; probing at the block's WIDTH
    ///   instead picks the 3-symbol `motion_mode` alphabet where libaom reads
    ///   the 2-symbol `obmc` one -- same value, different narrowing.
    #[test]
    fn a_10bit_film_frames_with_small_side_globalmv_and_rect_warp_reach_decode_pixel_exact() {
        const NAME: &str =
            "a_10bit_film_frames_with_small_side_globalmv_and_rect_warp_reach_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/gm_small_side_witness.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (1920usize, 792usize);
        let gm_before = crate::decode::gm_nontrans_small_side_hits();
        let tr_before = crate::decode::tr_reach_longer_side_hits();
        // lane-t900 r13: this stream is also the witness for r12's mi-granular
        // uv_mode grid -- the chroma edge-filter neighbour read that the coarse
        // one-slot-per-column map answered wrongly inside a sub-8x8 pair.
        let uv_before = crate::decode::uv_mode_grid_override_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let gm = crate::decode::gm_nontrans_small_side_hits() - gm_before;
        let tr = crate::decode::tr_reach_longer_side_hits() - tr_before;
        let uvg = crate::decode::uv_mode_grid_override_hits() - uv_before;
        assert!(
            uvg > 0,
            "{NAME}: uv_mode_grid_override never fired -- the chroma edge-filter \
             grid read would be untested"
        );
        assert!(
            gm > 0,
            "{NAME}: gm_nontrans_small_side never fired -- the gate would be vacuous for it"
        );
        assert!(
            tr > 0,
            "{NAME}: tr_reach_longer_side never fired -- the gate would be vacuous for it"
        );
        assert_eq!(frames.len(), 33, "{NAME}: 33 shown frames");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len());
        assert_eq!(
            ffmpeg_frames.len(),
            frames.len(),
            "{NAME}: ffmpeg returned {} frames, we decoded {}",
            ffmpeg_frames.len(),
            frames.len()
        );
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!(
            "{NAME}: 33 shown frames pixel-exact on every plane; \
             gm_nontrans_small_side={gm} tr_reach_longer_side={tr} \
             uv_mode_grid_override={uvg}"
        );
    }

    /// lane-film10fix: the DECODE-ORDER half of the two charter-named 10-bit
    /// film gates, against the instrumented `aomdec` rather than ffmpeg.
    ///
    /// `a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_...` (15 shown
    /// frames) and `a_10bit_film_frames_with_small_side_globalmv_and_rect_warp_...`
    /// (33 shown) each hold exactly ONE hidden alt-ref frame, and both
    /// compare through `ffmpeg_decode_sequence_10bit`, which emits SHOWN
    /// frames only. So one frame in 16 and one in 34 of each stream is
    /// outside both gates' reach -- class `gate-blind-to-hidden-frames`: a
    /// defect in a hidden frame surfaces only as drift on some later shown
    /// frame, if at all. Their doc comments claim the hidden frames were
    /// compared byte-exact against aomdec, but that measurement lived in a
    /// lane report, not in a test, so nothing re-ran it.
    ///
    /// This gate is that measurement, committed.
    /// [`decode_all_frames_vs_oracle`] writes `EC_AV1_FINAL_DUMP` on both
    /// sides (ours in `decode_stream`, the oracle's in rung 12 of
    /// `scripts/instrument-aom-oracle.sh`) and byte-compares EVERY decode-order
    /// frame, hidden included. Both fixtures are the same 1920x792 10-bit
    /// films their own gates pin, at the sha256 those gates record.
    ///
    /// The frame counts are asserted, not just reported: 16 = 15 shown + 1
    /// hidden, 34 = 33 shown + 1. A stream that quietly lost its hidden ARF
    /// would make this compare fewer frames and would otherwise read as a
    /// pass.
    #[test]
    fn the_two_10bit_film_pins_match_aomdec_on_every_decode_order_frame() {
        const NAME: &str = "the_two_10bit_film_pins_match_aomdec_on_every_decode_order_frame";
        let _gate_lock = lock_gate_counters();
        if !aomdec_available(NAME) {
            return;
        }
        // The pin paths are BOUND HERE, from literals, rather than built from
        // the loop variable: `crate_pin`'s argument has to be a literal the
        // fixture-library census can resolve, or invariant 4 reports this gate
        // as UNPROVEN ("name built at runtime -- this census cannot resolve
        // it") and a preflight that checks code shape goes RED.
        let pins: [(&str, std::path::PathBuf, usize, usize); 2] = [
            (
                "reelc_sb128_inter_witness.obu",
                crate_pin("reelc_sb128_inter_witness.obu"),
                16usize,
                15usize,
            ),
            (
                "gm_small_side_witness.obu",
                crate_pin("gm_small_side_witness.obu"),
                34usize,
                33usize,
            ),
        ];
        for (pin, path, frames, shown) in pins {
            let stream =
                std::fs::read(&path).unwrap_or_else(|e| panic!("{NAME}: reading {pin}: {e}"));
            let (decoded, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
            assert_eq!(
                (decoded, hidden),
                (frames, frames - shown),
                "{NAME}: {pin} must decode to {frames} decode-order frames of which {shown} are \
                 shown -- a stream that lost its hidden alt-ref would make the byte compare below \
                 cover less than it claims"
            );
            eprintln!(
                "{NAME}: {pin} {decoded} decode-order frames ({hidden} hidden) byte-exact vs \
                 aomdec EC_AV1_FINAL_DUMP on every plane"
            );
        }
    }

    /// lane-t900 r11: the frame-edge MV clamp witness.
    ///
    /// `crates/ec-av1/fixtures/reelb_head_mvclamp_witness.obu` (72158 bytes,
    /// sha256 451a9b138995e636f22308847a123b5c8621d4908cd93772cf5bbf2b37df39d7,
    /// truncated twice independently to identical hashes) is the first 80
    /// frame-carrying OBUs of a 10-bit 3840x1608 HDR film head -- the SMALLEST
    /// such prefix that fires the clamp (N=79 fires 0, N=80 fires 5). 55
    /// decode-order frames, 52 shown, every decode-order frame byte-identical
    /// to instrumented aomdec (`EC_AV1_FINAL_DUMP`, see the r11 report).
    ///
    /// The path it pins: libaom's `mb_to_right_edge` / `mb_to_bottom_edge`
    /// (`set_mi_row_col`, av1_common_int.h) are SIGNED, and a block that
    /// overhangs the frame's right/bottom edge keeps its full `bw4`/`bh4`, so
    /// `mi_cols - bw4 - mi_col` and `mi_rows - bh4 - mi_row` go NEGATIVE there
    /// and `clamp_mv_ref` (mvref_common.h) narrows the stack window by that
    /// much. This frame height is 1608 -> `mi_rows == 402`, so every 64x64
    /// block on the bottom row overhangs by 14 mi units. Computing that
    /// difference in `usize` saturated it to 0 and left the window 448 subpel
    /// too wide, so an edge block's predictor was never clamped -- one 64x64
    /// NEWMV block at mi(400,368) took `mv0=(224,568)` where libaom takes
    /// `(32,568)`, its MC then read the replicated bottom row instead of real
    /// reference rows, and the frame missed by 221 bytes.
    #[test]
    fn a_10bit_film_frames_with_frame_edge_mv_clamping_decode_pixel_exact() {
        const NAME: &str = "a_10bit_film_frames_with_frame_edge_mv_clamping_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/reelb_head_mvclamp_witness.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (3840usize, 1608usize);
        let clamp_before = crate::mvstack::mv_clamp_edge_overhang_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let clamped = crate::mvstack::mv_clamp_edge_overhang_hits() - clamp_before;
        assert!(
            clamped > 0,
            "{NAME}: mv_clamp_edge_overhang never fired -- the gate would be vacuous for it"
        );
        assert_eq!(frames.len(), 52, "{NAME}: 52 shown frames");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, frames.len());
        assert_eq!(
            ffmpeg_frames.len(),
            frames.len(),
            "{NAME}: ffmpeg returned {} frames, we decoded {}",
            ffmpeg_frames.len(),
            frames.len()
        );
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
        eprintln!(
            "{NAME}: 52 shown frames pixel-exact on every plane; \
             mv_clamp_edge_overhang={clamped}"
        );
    }
