use super::*;
use super::support::*;
use super::gates_a::*;
use super::gates_b::*;
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

    /// lane-lr r3: proves stage 1/2's exit criterion -- a real
    /// `--enable-restoration=1` aomenc stream must survive the whole
    /// partition walk (every symbol read entropy-exact) and land on the
    /// NEW, narrowed refusal ("read but not applied"), never desync into
    /// some other named refusal or an outright decode panic. Multi-superblock
    /// fixture (192x128 = 3x2 SBs of 64px) per the charter's own trap
    /// warning -- a single-SB fixture can never re-enter `read_lr` a second
    /// time and would leave `SWITCHABLE_HITS`/etc. proving only one call
    /// site, not the per-SB loop.
    #[test]
    fn a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly() {
        const NAME: &str = "a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // lane-reelckf2 r1: 152 = 2*64+24, not 128 -- the 8-bit twin of the
        // partial-bottom-stripe LR gate below. Stripe shapes at 128 rows are
        // a strict subset of those at 152 (56, 64, then the 32-row tail whose
        // below boundary is the frame's own last row).
        let (width, height) = (192usize, 152usize);
        let n_attempts: u32 = std::env::var("EC_LR_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        let mut lr_refusals = 0u32;
        let mut other_refusals = Vec::new();
        // lane-reelckf2 r1: the two special stripe shapes this size now has.
        let stripe0_before = crate::restoration::lr_stripe0_hits();
        let last_stripe_before = crate::restoration::lr_last_stripe_hits();
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            // Diagnosis knob: which content makes RD pick an 8x16/16x8 strip
            // AND filter intra on it is a search (see lanes/fistrip-r1.report.md).
            let source = match std::env::var("EC_FISTRIP_SRC").unwrap_or_default().as_str() {
                "bars" => format!("smptebars=size={width}x{height}:duration=0.04:rate=25"),
                "src2" => format!("testsrc2=size={width}x{height}:duration=0.04:rate=25"),
                "noise" => format!(
                    "nullsrc=size={width}x{height}:duration=0.04:rate=25,geq=random(1)*255:128:128"
                ),
                _ => gradients_source(seed, width, height, "duration=0.04:rate=25"),
            };
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args(["-v", "error", "-f", "lavfi", "-i"])
                .arg(&source)
                .args(["-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", "-"])
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
                    // r3: a low cq-level (high quality/bitrate) is load-bearing --
                    // sampled live (2026-08-30) that aomenc's RD only ever picks a
                    // non-`RESTORE_NONE` frame_restoration_type at cq-level<=20 for
                    // this fixture; cq-level=45 (this gate's sibling gates' usual
                    // value) landed `uses_lr=false` on every attempt.
                    "--cq-level=15",
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    // r3: this decoder always assumes 64px superblocks
                    // (`decode.rs` hardcodes `SB_MI=16`, ledger `lane-sb128`
                    // dead-ends) -- force aomenc to match rather than risk
                    // the unrelated sb128 gap eating this gate's own window.
                    "--sb-size=64",
                    "--enable-restoration=1",
                    "--enable-cdef=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-tx-size-search=0",
                    "--max-partition-size=32",
                    "--min-partition-size=16",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
                    "--enable-warped-motion=0",
                    "--enable-global-motion=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-obmc=0",
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
            match decode_stream(&stream) {
                Ok(pics) => {
                    // r4: the pixel filters are wired -- every `uses_lr`
                    // frame must now decode Ok AND match an independent
                    // decoder pixel-exact, not just refuse cleanly.
                    // The ENCODE's frame count: every `EC_FISTRIP_SRC` branch
                    // and the `gradients_source` default (30568-30573) render at
                    // `rate=25` with a 1-frame `duration=0.04`, and aomenc runs
                    // with no `--limit` (30590) -- so each attempt's stream codes
                    // exactly 1 frame. Asserted ABOVE the oracle call.
                    assert_eq!(pics.len(), 1, "{NAME}: seed {seed} key frame");
                    let reference = ffmpeg_decode_sequence(&stream, width, height, pics.len());
                    let mismatched = pics
                        .iter()
                        .zip(&reference)
                        .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                    // r7: pin a mismatching stream the same way the
                    // masked-compound gate does -- lets a follow-up round
                    // decode this exact seed's bytes directly instead of
                    // re-deriving them from a live 40-attempt sweep.
                    if mismatched && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                        std::fs::write(&path, &stream).expect("writing pinned stream");
                        eprintln!(
                            "EC_AV1_GATE_DUMP: wrote mismatching LR stream (seed {seed}) to {path}"
                        );
                    }
                    for (i, (pic, refpic)) in pics.iter().zip(reference.iter()).enumerate() {
                        assert_eq!(
                            pic.y, refpic.y,
                            "{NAME}: luma mismatch (seed {seed} frame {i})"
                        );
                        assert_eq!(
                            pic.u, refpic.u,
                            "{NAME}: U mismatch (seed {seed} frame {i})"
                        );
                        assert_eq!(
                            pic.v, refpic.v,
                            "{NAME}: V mismatch (seed {seed} frame {i})"
                        );
                    }
                    lr_refusals += 1; // reused below as "attempts that actually exercised LR pixels"
                }
                Err(e) => {
                    let msg = e.to_string();
                    other_refusals.push(format!("seed {seed}: {msg}"));
                }
            }
        }
        assert!(
            other_refusals.is_empty(),
            "{NAME}: {} attempts failed outright instead of decoding:\n{}",
            other_refusals.len(),
            other_refusals.join("\n")
        );
        assert!(
            lr_refusals > 0,
            "{NAME}: no attempt ever decoded pixel-exact ({n_attempts} attempts)"
        );
        // Any real (non-`RESTORE_NONE`) filter kind is acceptable proof --
        // aomenc's RD picks the frame-level `restoration_type` itself (not
        // always `Switchable`; observed `Sgrproj` live at cq-level=15 for
        // this fixture), so hard-requiring one specific arm would be the
        // gate-recipe-confound class (ledger).
        let total_hits = crate::restoration::wiener_hits()
            + crate::restoration::sgrproj_hits()
            + crate::restoration::switchable_hits();
        assert!(
            total_hits > 0,
            "{NAME}: {lr_refusals} LR refusals fired but read_lr_unit never decoded a real \
             (non-None) filter -- gate proved nothing about the symbol path"
        );
        // lane-reelckf2 r1: a size with a partial bottom stripe proves nothing
        // about the two frame-edge stripe shapes unless a real filter actually
        // ran on them (`filter_restoration_unit`).
        assert!(
            crate::restoration::lr_stripe0_hits() > stripe0_before
                && crate::restoration::lr_last_stripe_hits() > last_stripe_before,
            "{NAME}: no filtered stripe took the frame's own top/bottom row as its boundary \
             (stripe0 +{}, last +{})",
            crate::restoration::lr_stripe0_hits() - stripe0_before,
            crate::restoration::lr_last_stripe_hits() - last_stripe_before
        );
        eprintln!(
            "{NAME}: {lr_refusals} LR refusals, {} other refusals out of {n_attempts}, \
             wiener_hits={} sgrproj_hits={} switchable_hits={}",
            other_refusals.len(),
            crate::restoration::wiener_hits(),
            crate::restoration::sgrproj_hits(),
            crate::restoration::switchable_hits()
        );
    }

    /// r8: decode the ONE pinned stream `EC_AV1_GATE_DUMP` captured in r7
    /// (`fixtures/lr-sgr-r7.obu`, seed 46) directly -- not the live
    /// 40-attempt sweep, which reuses `v_start` coordinates across unrelated
    /// seeds/frames and made r6's captured 9 bytes unprovable. Run with
    /// `EC_LR_CALL_DUMP=1` set (see `restoration.rs`'s `apply_sgrproj_stripe`)
    /// to get the real window on stderr, call-uniquely keyed to `xqd ==
    /// [-16,-32]` rather than any coordinate.
    /// lane-av1pinslive r4: this gate asserted NOTHING. It `eprintln!`d per-frame
    /// mismatch flags and returned, so it could not go red on a decode regression
    /// -- which is why r3 left it `#[ignore]`d on that fact rather than un-ignoring
    /// it. It now carries the same three things its three siblings have: a pinned
    /// FRAME COUNT, a length assert before the zip, and per-plane per-frame pixel
    /// asserts. The diagnostic print is kept, and now runs BEFORE the asserts so a
    /// failure still names the shape of the divergence.
    ///
    /// The frame count is the substantive fix. The old body passed `pics.len()` as
    /// ffmpeg's EXPECTED count:
    ///     let reference = ffmpeg_decode_sequence(&stream, 192, 128, pics.len());
    /// so a decoder that emitted ZERO frames asked ffmpeg for zero frames, the zip
    /// compared no pairs, and the gate passed having proved nothing -- the exact
    /// vacuous-pass shape the `cmpaudit` lane swept elsewhere in this crate. The
    /// count now comes from the FIXTURE (`FRAMES`), so losing the frame reds on the
    /// count instead of comparing nothing.
    ///
    /// On "does it compare only frame 0": the loop always visited every frame; this
    /// pin simply holds one (`frame 0` was the only line the pre-r4 run printed).
    /// The loop is unchanged in that respect and still visits every frame it gets.
    ///
    /// The `EC_LR_CALL_DUMP=1` window is unchanged and still needs that env -- but
    /// it is a debugging session layered on a real gate now, not the gate itself.
    #[test]
    fn pinned_lr_sgr_stream_call_unique_dump() {
        const NAME: &str = "pinned_lr_sgr_stream_call_unique_dump";
        const W: usize = 192;
        const H: usize = 128;
        /// Measured: the pre-r4 run printed `frame 0` and no other frame line.
        const FRAMES: usize = 1;
        let path = std::env::var("EC_AV1_GATE_DUMP_PIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| crate_pin("lr-sgr-r7.obu"));
        let stream = require_pin(&path, "lr-sgr-r7.obu");
        let pics = decode_stream(&stream).expect("pinned lr-sgr-r7.obu must decode");
        assert_eq!(
            pics.len(),
            FRAMES,
            "{NAME}: decoded {} frame(s), the pinned stream holds {FRAMES} -- a short \
             decode would otherwise ask ffmpeg for the same short count and compare \
             nothing",
            pics.len()
        );
        for f in &pics {
            assert_eq!((f.width, f.height), (W, H), "{NAME}: dimensions");
        }
        if !have_ffmpeg() {
            eprintln!(
                "SKIP {NAME}: no ffmpeg -- the pixel compare IS this gate's claim, so it \
                 proves nothing here; set EC_AV1_REQUIRE_FFMPEG=1 to make that a hard \
                 failure instead"
            );
            return;
        }
        let reference = ffmpeg_decode_sequence(&stream, W, H, FRAMES);
        assert_eq!(
            reference.len(),
            FRAMES,
            "{NAME}: ffmpeg returned {} frame(s), expected {FRAMES}",
            reference.len()
        );
        for (i, (got, want)) in pics.iter().zip(&reference).enumerate() {
            let y_mismatch = got.y != want.y;
            let u_mismatch = got.u != want.u;
            let v_mismatch = got.v != want.v;
            eprintln!(
                "frame {i}: y_mismatch={y_mismatch} u_mismatch={u_mismatch} v_mismatch={v_mismatch}"
            );
            if y_mismatch {
                let n = got.y.iter().zip(&want.y).filter(|(a, b)| a != b).count();
                eprintln!("  {n} luma bytes differ / {}", got.y.len());
            }
            if v_mismatch {
                let w = 96usize; // chroma plane width (192/2)
                for (idx, (a, b)) in got.v.iter().zip(&want.v).enumerate() {
                    if a != b {
                        eprintln!("  V[{},{}] got={a} want={b}", idx / w, idx % w);
                    }
                }
            }
            assert_eq!(
                got.y, want.y,
                "{NAME} frame {i} luma vs ffmpeg (pinned lr-sgr)"
            );
            assert_eq!(
                got.u, want.u,
                "{NAME} frame {i} U vs ffmpeg (pinned lr-sgr)"
            );
            assert_eq!(
                got.v, want.v,
                "{NAME} frame {i} V vs ffmpeg (pinned lr-sgr)"
            );
        }
        eprintln!(
            "{NAME}: {FRAMES} frame(s) byte-exact vs ffmpeg from {}",
            path.display()
        );
    }

    /// lane-realworld r2: `read_cdef` (spec 5.11.56) ported -- per-superblock
    /// `cdef_idx`, wired at all five `skip`-decode sites, `apply_cdef` looks
    /// up strength through the grid instead of a hardcoded `[0]`. Structure
    /// copied from `a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact`
    /// but `--enable-cdef=1` (the one thing that gate turns off) and every
    /// *other* filter/mode feature this lane doesn't touch disabled, so a
    /// mismatch here can only be attributed to CDEF. `cdef_idx_hits()` is a
    /// hard assert -- a run that never reads a real cdef_idx symbol proves
    /// nothing (class `gate-blind-to-feature`).
    #[test]
    fn a_real_aomenc_stream_with_cdef_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_cdef_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // cdef_bits selects among PER-SUPERBLOCK strengths -- a single-SB
        // 64x64 frame (the other gates' size) gives aomenc nothing to
        // differentiate, so it always writes bits=0 and cdef_idx is never
        // read. 128x64 = 2 SBs is the minimum that lets aomenc's RD ever
        // pick bits > 0. Multi-SB content at --cpu-used=0 (every other
        // gate's setting) makes aomenc pick PARTITION_HORZ_4/VERT_B etc.
        // at the top SB level regardless of the ab/1to4-partitions=0
        // flags below (a real gap: this decoder's intra part64 match only
        // covers NONE/SPLIT, and the inter path blindly assumes SPLIT --
        // both pre-existing, out of this lane's scope); --cpu-used=4
        // avoids that RD path entirely and needs --aq-mode=0
        // --deltaq-mode=0 to keep delta_q/delta_lf (next lane step) off.
        let (width, height, frame_count) = (128usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_CDEF_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            // mandelbrot's ringing-prone hard edges are what CDEF's RD
            // search targets; flat gradients (charter's first guess) may
            // never make aomenc choose cdef_bits > 0.
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
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=4",
                "--aq-mode=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                // r2: a starved inter toolset (everything below off) pushes
                // aomenc's RD toward exotic partition shapes (HORZ_4/VERT_B
                // etc.) at the SB(64) level even with those partition
                // flags off -- this decoder's part64 8/9/10-way arms and
                // the inter path's blind SPLIT assumption don't cover that;
                // keeping the same rich toolset the (working, ab/1to4-off)
                // masked-compound gate uses avoids it.
                "--enable-warped-motion=1",
                "--enable-global-motion=0",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=1",
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
                "--enable-cdef=1",
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
                        !msg.contains("cdef"),
                        "{NAME} refused on cdef (seed {seed}) -- cdef_idx decode is ported: {msg}"
                    );
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
            crate::decode::cdef_idx_hits() > 0,
            "{NAME}: zero cdef_idx blocks fired ({matched} matches, {named_refusals} other \
             refusals out of {n_attempts}) -- gate proved nothing this run"
        );
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched} pixel-exact matches \
             out of {n_attempts}, cdef_idx_hits={}",
            crate::decode::cdef_idx_hits()
        );
    }

    /// lane-edgeboth r1 GATE: frame sizes whose width AND height are both
    /// 8 mod 16, so the bottom-right corner of the frame is a partition node
    /// with `has_rows == false && has_cols == false` at every level it
    /// reaches -- libaom `decode_partition` forces PARTITION_SPLIT there and
    /// reads NO symbol (`else if (!has_rows && !has_cols) partition =
    /// PARTITION_SPLIT;`), then drops the out-of-frame children on its
    /// `if (mi_row >= mi_rows || mi_col >= mi_cols) return;` early return.
    /// The 64 and 32 levels already inferred that split; the 16 level refused
    /// by name until r1, on both the intra (key) and the inter path -- so both
    /// arms are swept here. Every compared attempt must show a 16-level
    /// both-cut hit, and the sweep must show 32- and 64-level hits too (200x72
    /// puts all three levels in that state: mi_cols 50, mi_rows 18, so the
    /// nodes at mi (16,48) of size 64, 32 and 16 are all cut on both axes).
    #[test]
    fn a_real_aomenc_both_axes_cut_corner_block_decodes_pixel_exact() {
        both_axes_cut_sweep(
            "a_real_aomenc_both_axes_cut_corner_block_decodes_pixel_exact",
            ("key", 1, "--kf-max-dist=0", "--limit=1"),
        );
    }

    /// lane-edgeboth r1, INTER half of the same lift. Un-ignored 2026-09-04: the inter frame-edge half-strip band defect that blocked it is gone. As measured then: the inter path's both-cut corner block
    /// itself decodes exactly (200x72 inter cq 32, luma diff bounding box rows
    /// 64..=71 cols 15..=186 -- the corner at cols 192..=199 is clean), but the
    /// bottom 8-row frame-edge BAND of an inter frame is wrong, and the
    /// control 192x72 (width a multiple of 16, so zero both-cut nodes: hits
    /// [0,0,0,0]) fails identically at rows 64..=71 cols 8..=188. That band is
    /// the forced one-axis HORZ half strip on the inter path -- lane-oddh's
    /// own inter gate is `#[ignore]`d for the same arm -- so this gate turns
    /// green with that defect, not with anything this lane changed.
    #[test]
    fn a_real_aomenc_both_axes_cut_corner_inter_frames_decode_pixel_exact() {
        both_axes_cut_sweep(
            "a_real_aomenc_both_axes_cut_corner_inter_frames_decode_pixel_exact",
            ("inter", 6, "--kf-max-dist=9999", "--limit=6"),
        );
    }

    /// lane-tiny r4 GATE (was r1's `#[ignore]`d probe): every tiny key-frame
    /// size real aomenc will encode, decoded pixel-exact against ffmpeg.
    /// Tiny frames are where every edge/partial-superblock corner case is
    /// forced, and this sweep is what found both r3's filter-intra tx_type
    /// CDF row and r4's chroma-transform-size deblock defect -- so it
    /// hard-asserts now: a decode error, a refusal, or one differing sample
    /// at any size/seed fails. Only a genuinely absent oracle skips, and
    /// `EC_AV1_REQUIRE_AOMENC=1` turns that into a failure too.
    #[test]
    fn a_real_aomenc_tiny_frame_size_sweep_decodes_pixel_exact() {
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP: no ffmpeg/aomenc");
            return;
        }
        // lane-oddh r2: the second half of this list is the odd-height /
        // odd-width ladder -- frame sizes whose last superblock row or column
        // is a partial 64 (72 = one full SB plus 8 rows, 136 = two plus 8),
        // which is where the frame-edge half-strip path r1 added is the only
        // way to code the edge. 192x136 is the inter-sized member.
        let sizes: &[(usize, usize)] = &[
            (8, 8),
            (16, 16),
            (32, 32),
            (16, 32),
            (32, 16),
            (64, 64),
            (48, 48),
            (24, 24),
            (64, 72),
            (72, 64),
            (64, 136),
            (136, 64),
            (192, 136),
        ];
        let mut total_exact = 0u32;
        for &(width, height) in sizes {
            let mut ok = 0u32;
            let mut bad = 0u32;
            let mut refused = 0u32;
            let n = 10u32;
            for attempt in 0..n {
                let seed = 42 + attempt;
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
                        "0.04",
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
                let max_part = width.max(height).min(64).next_power_of_two();
                let min_part = width.min(height).next_power_of_two().max(8).min(max_part);
                let max_part_arg = format!("--max-partition-size={max_part}");
                let min_part_arg = format!("--min-partition-size={min_part}");
                let args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=32",
                    "--cpu-used=4",
                    "--kf-max-dist=0",
                    "--limit=1",
                    "--threads=1",
                    "--row-mt=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    &max_part_arg,
                    &min_part_arg,
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ];
                let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
                assert!(
                    out.status.success(),
                    "aomenc refused: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let stream = out.stdout;
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        // A NAMED refusal is the only non-exact outcome this
                        // gate tolerates (8x8 still needs a rectangular
                        // transform); anything else is a hard failure, and
                        // the floor assert below keeps that from going
                        // vacuous.
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{width}x{height} seed {seed}: failed outright, not a named \
                             refusal: {msg}"
                        );
                        eprintln!("  {width}x{height} seed {seed}: REFUSED {msg}");
                        refused += 1;
                        continue;
                    }
                    Ok(f) => f,
                };
                let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
                if frames.len() == 1
                    && frames[0].y == ffmpeg_frames[0].y
                    && frames[0].u == ffmpeg_frames[0].u
                    && frames[0].v == ffmpeg_frames[0].v
                {
                    ok += 1;
                } else {
                    bad += 1;
                    let nd = |a: &[u16], b: &[u16]| a.iter().zip(b).filter(|(x, y)| x != y).count();
                    let (ndy, ndu, ndv) = (
                        nd(&frames[0].y, &ffmpeg_frames[0].y),
                        nd(&frames[0].u, &ffmpeg_frames[0].u),
                        nd(&frames[0].v, &ffmpeg_frames[0].v),
                    );
                    let mut first = None;
                    for row in 0..height {
                        for col in 0..width {
                            let i = row * width + col;
                            if frames[0].y[i] != ffmpeg_frames[0].y[i] {
                                first = Some((row, col, frames[0].y[i], ffmpeg_frames[0].y[i]));
                                break;
                            }
                        }
                        if first.is_some() {
                            break;
                        }
                    }
                    let ndiff = frames[0]
                        .y
                        .iter()
                        .zip(&ffmpeg_frames[0].y)
                        .filter(|(a, b)| a != b)
                        .count();
                    eprintln!(
                        "  {width}x{height} seed {seed}: MISMATCH first_diff={first:?} \
                         ndiff_luma={ndiff}/{} ndiff y/u/v={ndy}/{ndu}/{ndv}",
                        width * height
                    );
                }
            }
            assert_eq!(
                bad, 0,
                "{width}x{height}: {bad} of {n} seeds decoded to WRONG PIXELS ({ok} exact, \
                 {refused} named refusals)"
            );
            eprintln!("{width}x{height}: {ok}/{n} pixel-exact, {refused} named refusals");
            total_exact += ok;
        }
        // Hit counter: the sweep proved nothing unless every size x seed
        // attempt really ran end to end (aomenc -> us -> ffmpeg compare).
        // Floor (hit counter): 12 of the 13 sizes must decode all 10 seeds
        // pixel-exact -- only 8x8 is allowed to refuse outright today. If a
        // future change makes sizes refuse instead of decode, this fails
        // rather than passing on refusals alone.
        assert!(
            total_exact >= 120,
            "tiny sweep only reached {total_exact} pixel-exact attempts (floor 70) -- \
             refusals cannot substitute for decodes"
        );
        eprintln!("tiny frame sweep: {total_exact} pixel-exact attempts");
    }

    /// lane-tiny r1: re-decodes the known-failing 32x32 seed 45 fixture with
    /// `EC_AV1_TRACE` on to inspect what mode/tx this single-block frame
    /// picked. Diagnostic only, `#[ignore]`d, run manually with --nocapture.
    #[test]
    #[ignore = "diagnostic, run manually with --nocapture"]
    fn probe_tiny_32x32_trace() {
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP: no ffmpeg/aomenc");
            return;
        }
        let (width, height) = (32usize, 32usize);
        let attempt = 3u32; // seed 45
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
                "0.04",
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
        assert!(y4m.status.success());
        let args: Vec<&str> = vec![
            "--codec=av1",
            "--passes=1",
            "--end-usage=q",
            "--cq-level=32",
            "--cpu-used=4",
            "--kf-max-dist=0",
            "--limit=1",
            "--threads=1",
            "--row-mt=0",
            "--enable-rect-partitions=0",
            "--enable-ab-partitions=0",
            "--enable-1to4-partitions=0",
            "--max-partition-size=32",
            "--min-partition-size=32",
            "--enable-filter-intra=0",
            "--obu",
            "-o",
            "-",
            "-",
        ];
        let out = run_with_stdin(Command::new(aomenc_path()).args(&args), &y4m.stdout);
        assert!(out.status.success());
        let stream = out.stdout;
        let frames = decode_stream(&stream).expect("decode");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        let mismatched = frames[0].y != ffmpeg_frames[0].y;
        eprintln!("mismatched={mismatched}");
    }

    /// lane-tiny r2: decodes a raw OBU stream at `EC_TINY_FIXTURE_PATH` --
    /// no ffmpeg/aomenc invocation -- so a range ladder can be run
    /// (`EC_TRACE_MODE_STEP=1`) against the exact same bytes fed to the
    /// oracle aomdec, byte for byte. `#[ignore]`d diagnostic, no assertion.
    #[test]
    // Panics with `set EC_TINY_FIXTURE_PATH to an .obu file: NotPresent`
    // without the var -- it has no default and cannot run unattended. Prints
    // `decoded N frame(s)` or `REFUSED: <err>`; asserts nothing.
    #[ignore = "diagnostic: needs EC_TINY_FIXTURE_PATH naming an .obu; prints decode/REFUSED, asserts nothing"]
    fn probe_tiny_fixture_trace() {
        let path = std::env::var("EC_TINY_FIXTURE_PATH")
            .expect("set EC_TINY_FIXTURE_PATH to an .obu file");
        let stream = std::fs::read(&path).expect("reading fixture");
        match decode_stream(&stream) {
            Ok(frames) => {
                eprintln!("decoded {} frame(s)", frames.len());
                if let Ok(dump) = std::env::var("EC_TINY_FIXTURE_Y_DUMP") {
                    let bytes: Vec<u8> = frames[0].y.iter().map(|&v| v as u8).collect();
                    std::fs::write(&dump, &bytes).expect("dump y plane");
                }
            }
            Err(e) => eprintln!("REFUSED: {e}"),
        }
    }

    /// Pinned streams captured by `EC_AV1_GATE_DUMP` off
    /// `a_real_aomenc_stream_with_warped_motion_refuses_or_matches` -- each
    /// one is a former mismatch, kept as a regression pin (warp-mismatch:
    /// the WARPED_CAUSAL interp-read suppression; warp-flake-7: the same
    /// suppression for skip_mode blocks; warp-flake-5: mvstack entry
    /// clamping; ii-flake-1..8: interintra neighbours excluded from
    /// warp-sample gathering, ref_frame[1] == INTRA_FRAME in the mi grid).
    /// `EC_AV1_GATE_DUMP_PIN` overrides to a single stream.
    ///
    /// Un-`#[ignore]`d with the pin shape fixed: the fourteen pins are COMMITTED
    /// under `crates/ec-av1/fixtures/`, so the gate runs on a clean checkout
    /// and carries `warp_selected_hits` 0 -> 113 over the set instead of
    /// skipping GREEN having read nothing.
    #[test]
    fn pinned_warp_stream_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP pinned_warp_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        // Every pin is COMMITTED under `crates/ec-av1/fixtures/` and named by a
        // string literal, which is the convention every other pinned gate here
        // uses and the only shape the pin census can resolve name-by-name. The
        // old shape -- one GITIGNORED root directory literal plus a runtime
        // name list formatted against it -- can never be satisfied by a
        // committed tree, so this gate skipped GREEN having tested nothing.
        let pins = match std::env::var("EC_AV1_GATE_DUMP_PIN") {
            Ok(p) => vec![std::path::PathBuf::from(p)],
            Err(_) => vec![
                crate_pin("warp-mismatch.obu"),
                crate_pin("warp-flake-5.obu"),
                crate_pin("warp-flake-7.obu"),
                crate_pin("ii-flake-1.obu"),
                crate_pin("ii-flake-2.obu"),
                crate_pin("ii-flake-3.obu"),
                crate_pin("ii-flake-5.obu"),
                crate_pin("ii-flake-6.obu"),
                crate_pin("ii-flake-7.obu"),
                crate_pin("ii-flake-8.obu"),
                // switchable_interp missing from reset_counts (counter
                // saturation slowed the adaptation rate) -- unrelated to
                // interintra, caught by the same gate.
                crate_pin("ii-flake-9.obu"),
                // lane-rect r2: HORZ strip rect defects (mvstack size_h,
                // warp sample/projection dims, OBMC overlap, deblock tx-h).
                crate_pin("rect-flake-1.obu"),
                // scan_row/scan_col weight `inc` min'd by the wrong candidate
                // axis (width vs height) -- ties reordered DRL entry 1 for a
                // block under a 32x16 strip, and suppressed the -5 extended
                // row scan.
                crate_pin("rect-flake-2.obu"),
                // overlappable_left stepped the vertical walk by the
                // neighbour's WIDTH: a 32x16 left strip swallowed the strip
                // below it, blending the wrong OBMC prediction there.
                crate_pin("rect-flake-3.obu"),
            ],
        };
        for path in pins {
            eprintln!("pin: {}", path.display());
            check_pinned_warp_stream(&path);
        }
    }

    /// `LAST2_FRAME`/`LAST3_FRAME` are just older slots in the same forward
    /// reference-buffer ring `LAST_FRAME`/`GOLDEN_FRAME` already prove --
    /// dropping the golden gate's `--max-reference-frames=3
    /// --reduced-reference-set=1` narrowing (which suppresses exactly these)
    /// and running long enough for aomenc's own buffer rotation to reach
    /// past the first 2 frames is the whole difference.
    #[test]
    fn a_real_aomenc_stream_with_a_last2_reference_decodes_pixel_exact() {
        a_real_aomenc_single_ref_gate(
            "a_real_aomenc_stream_with_a_last2_reference_decodes_pixel_exact",
            crate::mvstack::LAST2_FRAME,
            "LAST2_FRAME",
            &["--auto-alt-ref=0", "--lag-in-frames=0"],
            64,
            64,
            8,
            120,
        );
    }

    #[test]
    fn a_real_aomenc_stream_with_a_last3_reference_decodes_pixel_exact() {
        a_real_aomenc_single_ref_gate(
            "a_real_aomenc_stream_with_a_last3_reference_decodes_pixel_exact",
            crate::mvstack::LAST3_FRAME,
            "LAST3_FRAME",
            &["--auto-alt-ref=0", "--lag-in-frames=0"],
            64,
            64,
            8,
            120,
        );
    }

    /// `BWDREF`/`ALTREF2`/`ALTREF` are all backward (display-order-forward)
    /// references that only exist once aomenc builds a hierarchical GOP --
    /// `--auto-alt-ref=1 --lag-in-frames` (need at least one full mini-GOP
    /// of lookahead) is what makes aomenc code them at all; without it
    /// every attempt would hit this decoder's own real, still-open
    /// per-block sign-bias gap by never drawing the reference in the first
    /// place, not by proving it correct.
    #[test]
    fn a_real_aomenc_stream_with_a_bwdref_reference_decodes_pixel_exact() {
        a_real_aomenc_single_ref_gate(
            "a_real_aomenc_stream_with_a_bwdref_reference_decodes_pixel_exact",
            crate::mvstack::BWDREF_FRAME,
            "BWDREF_FRAME",
            &[
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
            ],
            64,
            64,
            16,
            120,
        );
    }

    /// lane-altref2 r1: 16 frames was too short for aomenc to ever build a
    /// second-level alt-ref pyramid, so this gate ran 86 attempts and fired
    /// ALTREF2 exactly zero times (class `gate-blind-to-feature`) -- the
    /// flags were already right, only the clip length was wrong. A 64-frame
    /// clip at the same `--lag-in-frames=16` gives aomenc enough lookahead
    /// to schedule a 2-level GF pyramid and reliably codes ALTREF2 (4/4
    /// hits on a plain seed-42 direct-aomenc probe; see
    /// lanes/altref2-r1.report.md for the recipe search table). Firing is
    /// now HARD-asserted below: the gate fails rather than skips if
    /// `ref_hits(ALTREF2_FRAME)` does not advance.
    #[test]
    fn a_real_aomenc_stream_with_an_altref2_reference_decodes_pixel_exact() {
        let before = decode::ref_hits(crate::mvstack::ALTREF2_FRAME);
        a_real_aomenc_single_ref_gate(
            "a_real_aomenc_stream_with_an_altref2_reference_decodes_pixel_exact",
            crate::mvstack::ALTREF2_FRAME,
            "ALTREF2_FRAME",
            &[
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
            ],
            64,
            64,
            64,
            120,
        );
        let after = decode::ref_hits(crate::mvstack::ALTREF2_FRAME);
        assert!(
            after > before,
            "a_real_aomenc_stream_with_an_altref2_reference_decodes_pixel_exact: \
             ALTREF2_FRAME never fired across 120 attempts -- the gate is vacuous \
             again (class gate-blind-to-feature); see lanes/altref2-r1.report.md \
             for the recipe that is supposed to make this fire"
        );
    }

    #[test]
    fn a_real_aomenc_stream_with_an_altref_reference_decodes_pixel_exact() {
        a_real_aomenc_single_ref_gate(
            "a_real_aomenc_stream_with_an_altref_reference_decodes_pixel_exact",
            crate::mvstack::ALTREF_FRAME,
            "ALTREF_FRAME",
            &[
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
            ],
            64,
            64,
            16,
            120,
        );
    }

    #[test]
    fn a_real_aomenc_stream_with_temporal_mvs_decodes_pixel_exact() {
        a_real_aomenc_temporal_mv_gate(42, 40);
    }

    #[test]
    fn a_real_aomenc_compound_mv_stack_with_temporal_mvs_decodes_pixel_exact() {
        a_real_aomenc_compound_mv_stack_gate(
            "a_real_aomenc_compound_mv_stack_with_temporal_mvs_decodes_pixel_exact",
            42,
            30,
            8,
            64,
            0,
            64,
            0,
        );
    }

    #[test]
    fn a_real_aomenc_compound_mv_stack_with_global_motion_decodes_pixel_exact() {
        a_real_aomenc_compound_mv_stack_gate(
            "a_real_aomenc_compound_mv_stack_with_global_motion_decodes_pixel_exact",
            80,
            30,
            8,
            64,
            1,
            64,
            0,
        );
    }

    #[test]
    fn a_real_aomenc_10bit_compound_mv_stack_at_128_superblocks_decodes_pixel_exact() {
        a_real_aomenc_compound_mv_stack_gate(
            "a_real_aomenc_10bit_compound_mv_stack_at_128_superblocks_decodes_pixel_exact",
            120,
            30,
            10,
            128,
            1,
            64,
            0,
        );
    }

    /// Was a KNOWN OPEN DEFECT (found by lane-mvtwin r1, `#[ignore]`d for it):
    /// a real aomenc stream 128 px wide with `--tile-columns=1` (two 64-px
    /// tile columns) mis-decoded frame 1's luma against ffmpeg at seed 200
    /// with this exact recipe. lane-t900 r15 RE-MEASURED it on the merged tip
    /// (after the motion-field tile clamp, the per-tile txfm/mode band resets
    /// and the 8x8-compound interp-filter read landed): the arm decodes
    /// pixel-exact, so the `#[ignore]` is removed and the multi-tile mv-stack
    /// coverage is live.
    ///
    /// It is also the ONLY arm that reaches r1 defect 3 (the col reach is
    /// clamped to `tile_col0`, not the frame edge): the counter
    /// `mvstack::tile_reach_clips` advanced to 60 on this stream. Defect 3
    /// itself is proven deterministically by
    /// `mvstack::tests::the_row_reach_is_clamped_to_the_tile_origin_not_the_frame_edge`.
    #[test]
    fn a_real_aomenc_compound_mv_stack_across_two_tile_columns_decodes_pixel_exact() {
        let name = "a_real_aomenc_compound_mv_stack_across_two_tile_columns_decodes_pixel_exact";
        let before = crate::mvstack::tile_reach_clips();
        a_real_aomenc_compound_mv_stack_gate(name, 200, 30, 8, 64, 0, 128, 1);
        assert!(
            crate::mvstack::tile_reach_clips() > before,
            "{name}: no block was ever built inside a tile past mi 0 -- \
             --tile-columns=1 did not produce a second tile column"
        );
    }

    /// bisect scratch: decode a pinned stream exactly once -- generic
    /// pin-repro tool, kept for manual use.
    #[test]
    // Panics with `set EC_AV1_PIN to the .obu path: NotPresent` without the
    // var. Prints `OK: N frames` or `ERR: <err>` -- a one-line "does this pin
    // decode at all" check, so it asserts nothing by design.
    #[ignore = "scratch: decodes the .obu at EC_AV1_PIN and prints OK/ERR; asserts nothing"]
    fn scratch_decode_pinned_stream_once() {
        let path = std::env::var("EC_AV1_PIN").expect("set EC_AV1_PIN to the .obu path");
        let stream = std::fs::read(&path).expect("read pinned stream");
        match decode_stream(&stream) {
            Ok(frames) => eprintln!("OK: {} frames", frames.len()),
            Err(e) => eprintln!("ERR: {e}"),
        }
    }

    /// scratch: isolate a pinned mismatching stream's first divergent pixel.
    #[test]
    // Panics with `set EC_AV1_PIN to the .obu path: NotPresent` without the
    // var, AND the built-in geometry defaults (W=64, H=64, N=1) are wrong for
    // every real pin: they are the 64x64 seed-42 pin's own numbers, so on any
    // other pin the run dies inside the ffmpeg oracle with a frame-size
    // assert (`expected 1 4:2:0 frames ... left 24576 right 6144`) instead of
    // saying "you forgot the geometry". Set all four.
    #[ignore = "scratch: needs EC_AV1_PIN plus EC_AV1_PIN_W/H/N at the pin's REAL geometry (defaults 64/64/1 are wrong for every other pin); prints the first divergent pixel per plane"]
    fn scratch_isolate_pinned_mismatch() {
        let path = std::env::var("EC_AV1_PIN").expect("set EC_AV1_PIN to the .obu path");
        let stream = std::fs::read(&path).expect("read pinned stream");
        {
            let mut p = Av1Parser::new();
            let mut pos = 0usize;
            while pos < stream.len() {
                let obu = p.parse_obu(&stream[pos..]).expect("parse");
                pos += obu.total_size;
                if let ObuKind::Frame(header, _) = obu.kind {
                    eprintln!("interpolation_filter: {:?}", header.interpolation_filter);
                    eprintln!("cdef: {:?}", header.cdef);
                    eprintln!("quantization: {:?}", header.quantization);
                    eprintln!(
                        "coded_lossless: {:?} tx_mode: {:?}",
                        header.coded_lossless, header.tx_mode
                    );
                    eprintln!("delta: {:?}", header.delta);
                    eprintln!("segmentation: {:?}", header.segmentation);
                    eprintln!("loop_restoration: {:?}", header.loop_restoration);
                    eprintln!(
                        "use_128x128_superblock: {:?} disable_cdf_update: {:?} disable_frame_end_update_cdf: {:?} primary_ref_frame: {:?} allow_high_precision_mv: {:?} force_integer_mv: {:?}",
                        p.sequence_header().map(|s| s.use_128x128_superblock),
                        header.disable_cdf_update,
                        header.disable_frame_end_update_cdf,
                        header.primary_ref_frame,
                        header.allow_high_precision_mv,
                        header.force_integer_mv,
                    );
                }
            }
        }
        let width = std::env::var("EC_AV1_PIN_W")
            .map(|s| s.parse().unwrap())
            .unwrap_or(64);
        let height = std::env::var("EC_AV1_PIN_H")
            .map(|s| s.parse().unwrap())
            .unwrap_or(64);
        let frame_count = std::env::var("EC_AV1_PIN_N")
            .map(|s| s.parse().unwrap())
            .unwrap_or(4);
        let frames = decode_stream(&stream).expect("our decode");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
        // The `.min()` chain below is a scratch diagnostic that tolerates a
        // pinned stream whose counts disagree, but a SILENT shortfall reads as
        // "these frames matched" for frames nobody compared. Say how many are
        // actually walked.
        let walked = frame_count.min(frames.len()).min(ffmpeg_frames.len());
        if walked < frame_count {
            eprintln!(
                "pinned stream: asked for {frame_count} frames (EC_AV1_PIN_N), we show {}, ffmpeg \
                 shows {} -- comparing the first {walked} only",
                frames.len(),
                ffmpeg_frames.len()
            );
        }
        for f in 0..walked {
            let ours = &frames[f];
            let theirs = &ffmpeg_frames[f];
            for (plane_name, a, b, w) in [
                ("y", &ours.y, &theirs.y, width),
                ("u", &ours.u, &theirs.u, width / 2),
                ("v", &ours.v, &theirs.v, width / 2),
            ] {
                // A short plane would zip-truncate the walk below and report
                // MATCH over the prefix the two share.
                assert_eq!(
                    a.len(),
                    b.len(),
                    "frame {f} plane {plane_name}: ours has {} samples, ffmpeg's has {}",
                    a.len(),
                    b.len()
                );
                let mut first = None;
                let mut count = 0;
                let mut worst = 0i32;
                for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                    if x != y {
                        count += 1;
                        if first.is_none() {
                            first = Some(i);
                        }
                        worst = worst.max((*x as i32 - *y as i32).abs());
                    }
                }
                if let Some(i) = first {
                    eprintln!(
                        "frame {f} plane {plane_name}: {count} mismatches (worst delta {worst}), first at offset {i} (row {}, col {}) ours={} theirs={}",
                        i / w,
                        i % w,
                        a[i],
                        b[i]
                    );
                } else {
                    eprintln!("frame {f} plane {plane_name}: MATCH");
                }
            }
        }
    }

    /// Final round of the TX4/tx-class lane's own gate: `--min-partition-size=8
    /// --max-partition-size=8` pins every leaf to exactly 8x8 (spec
    /// 5.11.4's own boundary carve-out aside, which a multiple-of-8 frame
    /// never triggers), so a real `aomenc` stream from this recipe can only
    /// legitimately read `PARTITION_NONE` at every `bsize=3` node -- any
    /// other value this decoder sees there is a genuine desync, not a
    /// "partition below 8x8" the encoder actually wrote (traced live against
    /// a debug `aomdec` 2026-08-27: the true bitstream's own 8x8 partition
    /// reads are all `NONE`; the mismatch was `base_ctx`'s DC-position
    /// short-circuit firing for `TX_CLASS_HORIZ`/`VERT` too, off by one
    /// `BR` read into a spurious extra Golomb call). The sinusoidal-stripe
    /// `geq` source (a smooth luma ramp, not the sharp two-tone stripes that
    /// tripped `aomenc`'s screen-content-tools auto-detect) is what actually
    /// resolves `V_DCT`/`H_DCT` (`TxClass::of`) on some 8x8 TUs at this
    /// content's frequency; the attempt loop moves on from named refusals
    /// (`allow_screen_content_tools`, angle-delta) same as every other
    /// multi-seed recipe in this file, and only counts a run as a genuine
    /// firing when [`decode::tx_class1_hits`] actually moved.
    #[test]
    fn a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut fired_runs = 0u32;
        let mut uncounted_exact = 0u32;
        for attempt in 0..60u32 {
            let cq = 20 + (attempt % 5) * 10;
            let period = [4u32, 6, 8, 12, 16][(attempt as usize / 5) % 5];
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x64:rate=25",
                    "-vf",
                    &format!(
                        "geq=lum='128+80*sin(2*PI*Y/{period})':cb=128:cr=128,noise=alls=6:allf=t"
                    ),
                    "-pix_fmt",
                    "yuv420p",
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
                y4m.status.success(),
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let out = run_with_stdin(
                Command::new(aomenc_path()).args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &format!("--cq-level={cq}"),
                    "--cpu-used=0",
                    "--enable-directional-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-rect-partitions=0",
                    "--enable-angle-delta=0",
                    "--enable-palette=0",
                    "--reduced-tx-type-set=0",
                    "--enable-restoration=0",
                    "--enable-cdef=0",
                    "--loopfilter-control=0",
                    "--min-partition-size=8",
                    "--max-partition-size=8",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                    // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                    "--enable-ref-frame-mvs=0",
                    "--tune-content=default",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ]),
                &y4m.stdout,
            );
            if !out.status.success() {
                refusals.push(format!(
                    "cq={cq} period={period}: aomenc itself refused the fixture"
                ));
                continue;
            }
            let stream = out.stdout;
            let before = decode::tx_class1_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("cq={cq} period={period}: {e}"));
                    continue;
                }
            };
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            let counted = decode::tx_class1_hits() != before;
            if !counted {
                uncounted_exact += 1;
                refusals.push(format!(
                    "cq={cq} period={period}: decoded and pixel-compared, but no block read a \
                     V_DCT/H_DCT tx_type"
                ));
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (cq={cq} period={period})"
            );
            assert_eq!(
                frames[0].u, ffmpeg_frames[0].u,
                "U vs ffmpeg (cq={cq} period={period})"
            );
            assert_eq!(
                frames[0].v, ffmpeg_frames[0].v,
                "V vs ffmpeg (cq={cq} period={period})"
            );
            if counted {
                fired_runs += 1;
                if fired_runs >= 4 {
                    gate_buckets(
                        "a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact",
                        fired_runs,
                        uncounted_exact,
                        refusals.len() - uncounted_exact as usize,
                    );
                    return;
                }
            }
        }
        gate_buckets(
            "a_real_aomenc_min8_stream_with_tx_class1_decodes_pixel_exact",
            fired_runs,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        panic!(
            "fewer than 4 firing+pixel-exact runs out of 60 attempts:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-cdef r1: CDEF's per-8x8 skip band under sub-16x16 inter leaves.
    ///
    /// `apply_cdef` excludes an 8x8 luma unit whose four mi cells are all
    /// `skip_txfm` (libaom `cdef.c:29-38 is_8x8_block_skip`, called from
    /// `av1_cdef_compute_sb_list`'s dlist loop). `decode_inter_block8` -- the
    /// leaf path `--min-partition-size=4` makes aomenc pick for a 16x16 group
    /// -- wrote the loop-filter grid but never that skip band, so an all-skip
    /// group kept the PREVIOUS frame's flags and got filtered where libaom
    /// skips it (3 luma samples, decode-order frames 3 and 4 of the 192x128
    /// cq-8 stream below, at the frame's last 8x8 column).
    ///
    /// Both counters are hard asserts per compared attempt: `cdef_skipped_units`
    /// proves the all-skip dlist exclusion was actually reached, and
    /// `inter8_skip_band_hits` proves the 8x8 leaf path (the writer that used
    /// to be missing) ran on the same stream.
    #[test]
    fn a_real_aomenc_stream_with_cdef_and_sub16_inter_leaves_decodes_pixel_exact() {
        if !have_aomenc() {
            return;
        }
        let (width, height, frames) = (192usize, 128usize, 6usize);
        let mut refusals: Vec<String> = Vec::new();
        let mut fired = 0u32;
        let mut cdef_fired = 0u32;
        let mut leaf_fired = 0u32;
        for (depth, cq, sp) in [
            (8u32, 8u32, 3u32),
            (8, 10, 3),
            (8, 12, 6),
            (10, 8, 3),
            (10, 12, 6),
        ] {
            let pix = if depth == 10 {
                "yuv420p10le"
            } else {
                "yuv420p"
            };
            let src = format!(
                "color=c=gray:s={width}x{height}:d=0.24:r=25,format=gray,\
                 geq=lum='128+58*sin((X+N*{sp})/6)+18*sin(Y/23)',format=yuv420p"
            );
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &src,
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
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let out = run_with_stdin(
                Command::new(aomenc_path()).args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &format!("--cq-level={cq}"),
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    &format!("--bit-depth={depth}"),
                    &format!("--input-bit-depth={depth}"),
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--deltaq-mode=0",
                    "--enable-filter-intra=0",
                    "--enable-cfl-intra=0",
                    "--enable-intrabc=0",
                    "--lag-in-frames=0",
                    "--enable-obmc=0",
                    "--enable-tx-size-search=1",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--min-partition-size=4",
                    "--max-partition-size=16",
                    // Last occurrence of a repeated --enable-* flag wins
                    // (aomenc-last-flag-wins): CDEF stays ON, and the two
                    // counters below prove the filter really ran.
                    "--enable-cdef=1",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ]),
                &y4m.stdout,
            );
            assert!(
                out.status.success(),
                "aomenc refused the fixture (depth={depth} cq={cq})"
            );
            let stream = out.stdout;
            let skipped_before = decode::cdef_skipped_units();
            let leaves_before = decode::inter8_skip_band_hits();
            let decoded = match decode_stream(&stream) {
                Ok(f) => f,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.starts_with("unsupported: "),
                        "decode error that is not a named refusal \
                         (depth={depth} cq={cq}): {msg}"
                    );
                    refusals.push(format!("depth={depth} cq={cq}: {msg}"));
                    continue;
                }
            };
            assert_eq!(decoded.len(), frames, "frame count (depth={depth} cq={cq})");
            // lane-leaf8tx r2: continue-and-sweep, not a per-attempt hard
            // assert. Lifting a refusal reshuffles WHICH attempt this gate
            // lands on (class [[parallel-flake-is-attempt-selection]]): the
            // first decoded attempt need not carry a sub-16 leaf, and
            // aborting there skipped the pixel compare entirely. Every
            // decoded attempt is compared; the counters are asserted once,
            // at the end, over the compared attempts.
            if decode::cdef_skipped_units() > skipped_before {
                cdef_fired += 1;
            }
            if decode::inter8_skip_band_hits() > leaves_before {
                leaf_fired += 1;
            }
            let reference = if depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frames)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frames)
            };
            for (i, (ours, theirs)) in decoded.iter().zip(reference.iter()).enumerate() {
                assert_eq!(ours.y, theirs.y, "luma frame {i} (depth={depth} cq={cq})");
                assert_eq!(ours.u, theirs.u, "U frame {i} (depth={depth} cq={cq})");
                assert_eq!(ours.v, theirs.v, "V frame {i} (depth={depth} cq={cq})");
            }
            fired += 1;
        }
        // Three of the five arms land on OTHER open refusals (a non-DC chroma
        // mode on an 8x8 inter leaf; inter partitions below 8x8) -- the two
        // 8-bit cq 8/10 arms ARE the streams the defect was found on, and each
        // hard-asserts both counters plus all six frames on all three planes.
        assert!(
            fired >= 2,
            "fewer than 2 decoded+pixel-exact attempts:\n{}",
            refusals.join("\n")
        );
        assert!(
            cdef_fired > 0,
            "no compared attempt excluded an 8x8 CDEF unit by the all-skip rule \
             ({fired} compared):\n{}",
            refusals.join("\n")
        );
        assert!(
            leaf_fired > 0,
            "no compared attempt had a sub-16x16 inter leaf write the CDEF skip band \
             ({fired} compared):\n{}",
            refusals.join("\n")
        );
    }

    /// lane-cdefstrip r1: the CDEF skip band under SKIPPED rectangular inter
    /// strips (32x8 / 8x32 / 64x16 / 16x64 / 32x64 / 64x32, the rect and 1:4
    /// arms), the sibling of the 8x8-leaf case lane-cdef fixed.
    ///
    /// A fresh `Neighbours` seeds `skip_grid` all-`false` ("not skip"), so any
    /// decode arm that resolves `skip` and forgets `fill_skip_grid_rect` makes
    /// `apply_cdef` FILTER an 8x8 unit libaom's `is_8x8_block_skip`
    /// (`cdef.c:29-38`, `av1_cdef_compute_sb_list`'s dlist test) excludes --
    /// a +-1 band that no entropy or prediction check can see.
    /// `decode::cdef_unwritten_skip_units` is the direct instrument for that
    /// class: it counts 8x8 CDEF units none of whose four mi cells any coded
    /// block wrote this frame, and this gate hard-asserts it stays 0 while
    /// `decode::rect_skip_band_hits` proves skipped rect strips really ran.
    ///
    /// MEASURED (lane-cdefstrip r1, 177 real aomenc streams incl. every arm
    /// below): the count is already 0 everywhere -- the charter's premise that
    /// the rect strip arms never write the band is FALSE. This gate is the
    /// regression fence for it, not a fix.
    #[test]
    fn a_real_aomenc_stream_with_skipped_rect_inter_strips_keeps_the_cdef_skip_band() {
        if !have_aomenc() {
            return;
        }
        let (width, height, frames) = (192usize, 128usize, 6usize);
        let mut refusals: Vec<String> = Vec::new();
        let mut fired = 0u32;
        // (depth, cq, source): the arms measured pixel-exact end to end in the
        // r1 sweep (~/.cache/cdefstrip-tmp/sw.sh). Source `b` is the two-axis
        // moving sinusoid that makes aomenc pick 16x64/32x64/8x32 strips at
        // low cq and 32x8/64x16 at high cq; `a` is lane-inter16ab's source.
        let src_b = "120+55*sin((X+N*4)/7)+40*sin((Y-N*2)/11)";
        let src_a = "128+60*sin((Y+N*3)/5)+30*sin(X/23)";
        for (depth, cq, lum) in [
            (8u32, 18u32, src_b),
            (8, 45, src_b),
            (10, 18, src_b),
            (10, 24, src_b),
            (10, 24, src_a),
            (8, 24, src_a),
        ] {
            let pix = if depth == 10 {
                "yuv420p10le"
            } else {
                "yuv420p"
            };
            let src = format!(
                "color=c=gray:s={width}x{height}:d=0.24:r=25,format=gray,\
                 geq=lum='{lum}',format=yuv420p"
            );
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &src,
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
                "ffmpeg fixture: {}",
                String::from_utf8_lossy(&y4m.stderr)
            );
            let out = run_with_stdin(
                Command::new(aomenc_path()).args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &format!("--cq-level={cq}"),
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    &format!("--bit-depth={depth}"),
                    &format!("--input-bit-depth={depth}"),
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--deltaq-mode=0",
                    "--enable-filter-intra=0",
                    "--enable-cfl-intra=0",
                    "--enable-intrabc=0",
                    "--lag-in-frames=0",
                    "--kf-max-dist=9999",
                    "--tile-columns=0",
                    "--enable-tx-size-search=0",
                    "--enable-ab-partitions=0",
                    "--min-partition-size=8",
                    "--max-partition-size=64",
                    // Last occurrence wins (aomenc-last-flag-wins): the three
                    // flags this gate is about stay last.
                    "--enable-rect-partitions=1",
                    "--enable-1to4-partitions=1",
                    "--enable-cdef=1",
                    "--obu",
                    "-o",
                    "-",
                    "-",
                ]),
                &y4m.stdout,
            );
            assert!(
                out.status.success(),
                "aomenc refused the fixture (depth={depth} cq={cq})"
            );
            let stream = out.stdout;
            let unwritten_before = decode::cdef_unwritten_skip_units();
            let rect_skip_before = decode::rect_skip_band_hits();
            let skipped_before = decode::cdef_skipped_units();
            let decoded = match decode_stream(&stream) {
                Ok(f) => f,
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.starts_with("unsupported: "),
                        "decode error that is not a named refusal \
                         (depth={depth} cq={cq}): {msg}"
                    );
                    refusals.push(format!("depth={depth} cq={cq}: {msg}"));
                    continue;
                }
            };
            assert_eq!(decoded.len(), frames, "frame count (depth={depth} cq={cq})");
            assert_eq!(
                decode::cdef_unwritten_skip_units(),
                unwritten_before,
                "an 8x8 CDEF unit was judged from an UNWRITTEN skip band \
                 (a decode arm resolved `skip` without `fill_skip_grid_rect`) \
                 (depth={depth} cq={cq})"
            );
            let reference = if depth == 10 {
                ffmpeg_decode_sequence_10bit(&stream, width, height, frames)
            } else {
                ffmpeg_decode_sequence(&stream, width, height, frames)
            };
            for (i, (ours, theirs)) in decoded.iter().zip(reference.iter()).enumerate() {
                assert_eq!(ours.y, theirs.y, "luma frame {i} (depth={depth} cq={cq})");
                assert_eq!(ours.u, theirs.u, "U frame {i} (depth={depth} cq={cq})");
                assert_eq!(ours.v, theirs.v, "V frame {i} (depth={depth} cq={cq})");
            }
            // Pixels are asserted for EVERY decoded attempt above; an attempt
            // counts as FIRING only if this stream really exercised the class
            // (a skipped rect strip wrote the band AND CDEF consulted it via
            // the all-skip exclusion). aomenc picking no skipped strip at a
            // given cq is a non-firing attempt, never a weakened assert.
            let rect_skips = decode::rect_skip_band_hits() - rect_skip_before;
            let excluded = decode::cdef_skipped_units() - skipped_before;
            if rect_skips == 0 || excluded == 0 {
                refusals.push(format!(
                    "depth={depth} cq={cq}: exact but did not fire \
                     (skipped rect strips {rect_skips}, excluded 8x8 units {excluded})"
                ));
                continue;
            }
            fired += 1;
        }
        eprintln!(
            "a_real_aomenc_stream_with_skipped_rect_inter_strips_keeps_the_cdef_skip_band: \
             {fired} firing+exact attempts, unwritten skip-band units {}, \
             skipped rect strips {}, 8x8 units excluded {}\n{}",
            decode::cdef_unwritten_skip_units(),
            decode::rect_skip_band_hits(),
            decode::cdef_skipped_units(),
            refusals.join("\n")
        );
        assert!(
            fired >= 3,
            "fewer than 3 firing+pixel-exact attempts:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-sub8 r1: `--min-partition-size=4` on fine-detail noisy gradients
    /// at low cq reliably makes aomenc split an 8x8 block all the way to
    /// four `BLOCK_4X4` leaves (`PARTITION_SPLIT` below 8x8, spec
    /// `decode_partition`'s recursion bottom) -- confirmed with
    /// `EC_AV1_TRACE=1` against `examples/decode_probe` before this gate was
    /// written (`partition_w8 ... value=3` at mi=(0,0) on the very first
    /// block). [`decode::sub8_split_hits`] hard-asserts the path actually
    /// ran rather than every attempt landing on `PARTITION_NONE`.
    #[test]
    fn a_real_aomenc_stream_with_a_sub8_split_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_aomenc_stream_with_a_sub8_split_decodes_pixel_exact: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            let msg = format!("no aomenc at {}", aomenc_path().display());
            assert!(
                std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none(),
                "EC_AV1_REQUIRE_AOMENC is set but {msg}"
            );
            eprintln!("SKIP a_real_aomenc_stream_with_a_sub8_split_decodes_pixel_exact: {msg}");
            return;
        }
        // round 2: `--enable-rect-partitions=0` was added below to keep
        // aomenc off HORZ/VERT-below-8x8 elsewhere in the same tile, but it
        // is NOT sufficient alone -- see the round-2 report
        // (lanes/sub8-r1.report.md) for the live finding: 0/160 attempts
        // across both 64x64 and a spatially-isolated single-16x16-leaf
        // fixture passed, and every SPLIT that fires is immediately
        // followed by a spurious HORZ/VERT read on the very next sibling
        // 8x8 leaf, which reads as a real desync left behind by
        // `decode_leaf_split4`/`read_intra_mode_sub8`, not an encoder
        // choice this recipe can dodge. Tried and rejected: shrinking the
        // frame to 16x16 (single 16x16 leaf, at most 4 candidate 8x8s) --
        // that surfaces an UNRELATED pre-existing tiny-frame edge-straddle
        // defect (reproduces with zero sub8 code involved, `--min-partition
        // -size`/`--max-partition-size` absent entirely), so it is not a
        // safe way to isolate the leaf count. Left red; do not "fix" this
        // gate by picking fixture parameters that dodge the desync without
        // finding it.
        // lane-sub8 r6: three arms. The mi-granular mode maps
        // (`sub8_mode_col`/`uv_mode_col` and their row twins) are per TILE --
        // the verifier's repro was a 128x64 `--tile-columns=1` stream (seeds
        // 207/228) whose every mismatching luma pixel sat at x>=64, the tile
        // boundary -- so one arm per tile axis is part of this gate.
        for (width, height, tile_args) in [
            (64usize, 64usize, &[][..]),
            (128usize, 64usize, &["--tile-columns=1"][..]),
            (64usize, 128usize, &["--tile-rows=1"][..]),
        ] {
            let mut refusals = Vec::new();
            let mut fired_runs = 0u32;
            for attempt in 0..40u32 {
                let seed = 100 + attempt;
                let cq = 6 + (attempt % 4) * 2;
                let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        &gradients_source(seed, width, height, "rate=25"),
                        "-vf",
                        // `noise` without `all_seed` re-renders differently on
                        // every run (seeded-fixture-not-reproducible); pinning it
                        // makes (seed, cq) name one exact stream.
                        &format!("noise=alls=40:allf=t:all_seed={seed},format=yuv420p"),
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
                    y4m.status.success(),
                    "ffmpeg fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let out = run_with_stdin(
                    Command::new(aomenc_path())
                        .args([
                            "--codec=av1",
                            "--passes=1",
                            "--end-usage=q",
                            &format!("--cq-level={cq}"),
                            "--cpu-used=0",
                            "--threads=1",
                            "--row-mt=0",
                            "--sb-size=64",
                            "--min-partition-size=4",
                            "--max-partition-size=8",
                            // Below 8x8 only PARTITION_NONE/SPLIT are decodable (HORZ/VERT
                            // need a real rectangular transform, see the module doc above);
                            // disabling rect partitions entirely -- not just AB -- keeps
                            // aomenc from picking the still-refused HORZ/VERT arms almost
                            // every attempt (round 2 measured ~90% HORZ/VERT-below-8x8
                            // refusals with --enable-ab-partitions=0 alone).
                            "--enable-rect-partitions=0",
                            "--enable-ab-partitions=0",
                            "--enable-1to4-partitions=1",
                            "--enable-palette=0",
                            "--enable-intrabc=0",
                            "--enable-restoration=0",
                            "--enable-cdef=0",
                            "--loopfilter-control=0",
                            // use_ref_frame_mvs (temporal MV projection) is unimplemented in
                            // mvstack; this is a key-frame-only fixture, but keep it off for
                            // safety against a run that emits a second frame.
                            "--enable-ref-frame-mvs=0",
                            "--limit=1",
                            "--obu",
                            "-o",
                            "-",
                            "-",
                        ])
                        .args(tile_args),
                    &y4m.stdout,
                );
                if !out.status.success() {
                    refusals.push(format!(
                        "seed={seed} cq={cq}: aomenc itself refused the fixture"
                    ));
                    continue;
                }
                let stream = out.stdout;
                let before = decode::sub8_split_hits();
                let tiles_before = decode::tile_hits();
                let frames = match decode_stream(&stream) {
                    Ok(frames) => frames,
                    Err(e) => {
                        // Only a named refusal ("unsupported: ...") is a skip
                        // reason; any other decode error is this decoder failing.
                        let msg = e.to_string();
                        assert!(
                            msg.starts_with("unsupported: "),
                            "decode error that is not a named refusal \
                         (seed={seed} cq={cq} tiles={tile_args:?}): {msg}"
                        );
                        refusals.push(format!("seed={seed} cq={cq}: {msg}"));
                        continue;
                    }
                };
                // gate-blind-to-feature: a tile arm must actually decode more
                // than one tile, not merely carry `tile_info.cols > 1`.
                assert!(
                    tile_args.is_empty() || decode::tile_hits() - tiles_before >= 2,
                    "tiles={tile_args:?} decoded only {} tile(s) (seed={seed} cq={cq})",
                    decode::tile_hits() - tiles_before
                );
                if decode::sub8_split_hits() == before {
                    refusals.push(format!(
                    "seed={seed} cq={cq}: decoded, but no block read a real PARTITION_SPLIT below 8x8"
                ));
                    continue;
                }
                let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
                assert_eq!(
                    frames[0].y, ffmpeg_frames[0].y,
                    "luma vs ffmpeg (seed={seed} cq={cq})"
                );
                assert_eq!(
                    frames[0].u, ffmpeg_frames[0].u,
                    "U vs ffmpeg (seed={seed} cq={cq})"
                );
                assert_eq!(
                    frames[0].v, ffmpeg_frames[0].v,
                    "V vs ffmpeg (seed={seed} cq={cq})"
                );
                fired_runs += 1;
                if fired_runs >= 4 {
                    break;
                }
            }
            assert!(
                fired_runs >= 4,
                "fewer than 4 firing+pixel-exact runs out of 40 attempts \
             ({width}x{height} tiles={tile_args:?}):\n{}",
                refusals.join("\n")
            );
        }
    }

    /// lane-tx4x8: `--enable-rect-partitions=1` with `--min-partition-size=4
    /// --max-partition-size=8` on fine-detail noisy gradients makes aomenc
    /// pick `PARTITION_VERT`/`PARTITION_HORZ` at the 8x8 level -- two
    /// `BLOCK_4X8`/`BLOCK_8X4` leaves, the last sub-8x8 shape this decoder
    /// refused (it needed a real `TX_4X8`/`TX_8X4`: rect scan, 32-position
    /// `eob_pt`, `TX_4X4` `tx_type` row over `TX_8X8` coefficient tables).
    /// Both [`decode::tx4x8_coded_hits`] and [`decode::tx8x4_coded_hits`] are
    /// hard-asserted per firing attempt so neither orientation can ride along
    /// on the other ([[gate-blind-to-feature]]), and only leaves that coded
    /// REAL coefficients count -- a skipped leaf never runs the transform.
    /// Arms: 8-bit 16x16, an 8-bit `--tile-columns=1` 128x16 (the mi-granular
    /// mode maps are per tile), and a 10-bit 16x16 -- 16-pixel fixtures, for
    /// the reason the comment inside the loop gives.
    #[test]
    fn a_real_aomenc_stream_with_a_sub8_rect_leaf_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_a_sub8_rect_leaf_decodes_pixel_exact";
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
        // 16-pixel-tall/wide fixtures on purpose: at 64x64 EVERY attempt
        // dies on a 16x16-LEVEL rect refusal belonging to other lanes
        // ("a HORZ/VERT intra strip with a split transform", "a coded
        // (non-skip) HORZ/VERT rect strip below 16x16", "a HORZ_A/HORZ_B/
        // VERT_A partition below 16x16") -- `--max-partition-size=8` does
        // NOT stop aomenc coding a 16x16 as two 16x8 strips, measured
        // 39/40 attempts refused. A 16x16 frame is one 16x16 block, so the
        // encoder either splits it to 8x8s (this lane's path) or codes it
        // whole; measured 12/12 attempts decoding, 3/12 with BOTH
        // orientations coded.
        for (width, height, tile_args, bit_depth) in [
            (16usize, 16usize, &[][..], 8u8),
            (128usize, 16usize, &["--tile-columns=1"][..], 8u8),
            (16usize, 16usize, &[][..], 10u8),
        ] {
            let mut refusals = Vec::new();
            let mut fired_runs = 0u32;
            let mut split_tx = 0usize;
            for attempt in 0..40u32 {
                let seed = 300 + attempt;
                let cq = 6 + (attempt % 4) * 2;
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
                        &format!("testsrc2=size={width}x{height}:rate=25"),
                        "-vf",
                        &format!("noise=alls=40:allf=t:all_seed={seed},format={pix_fmt}"),
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
                    y4m.status.success(),
                    "ffmpeg fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let out = run_with_stdin(
                    Command::new(aomenc_path())
                        .args([
                            "--codec=av1",
                            "--passes=1",
                            "--end-usage=q",
                            &format!("--cq-level={cq}"),
                            "--cpu-used=0",
                            "--threads=1",
                            "--row-mt=0",
                            "--sb-size=64",
                            "--min-partition-size=4",
                            "--max-partition-size=8",
                            // The tool under test: rect partitions ON, at a size
                            // where the only rect shapes are 4x8 and 8x4.
                            "--enable-rect-partitions=1",
                            "--enable-ab-partitions=0",
                            "--enable-1to4-partitions=0",
                            "--enable-palette=0",
                            "--enable-intrabc=0",
                            "--enable-restoration=0",
                            "--enable-cdef=0",
                            "--loopfilter-control=0",
                            "--enable-ref-frame-mvs=0",
                            // Filter intra on a 4x8/8x4 leaf is refused by name
                            // (`predict_filter_intra` is square-only in this
                            // decoder, the same refusal the 16x16-level strips
                            // carry); with it on, that refusal -- not this
                            // lane's path -- is what most attempts hit.
                            "--enable-filter-intra=0",
                            "--limit=1",
                            "--obu",
                            "-o",
                            "-",
                            "-",
                        ])
                        .args(if bit_depth == 10 {
                            &["--input-bit-depth=10", "--bit-depth=10"][..]
                        } else {
                            &[][..]
                        })
                        .args(tile_args),
                    &y4m.stdout,
                );
                if !out.status.success() {
                    refusals.push(format!(
                        "seed={seed} cq={cq}: aomenc itself refused the fixture"
                    ));
                    continue;
                }
                let stream = out.stdout;
                let vert_before = decode::tx4x8_coded_hits();
                let horz_before = decode::tx8x4_coded_hits();
                let split_before = decode::rect8_split_tx_hits();
                let tiles_before = decode::tile_hits();
                let frames = match decode_stream(&stream) {
                    Ok(frames) => frames,
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.starts_with("unsupported: "),
                            "decode error that is not a named refusal \
                             (seed={seed} cq={cq} depth={bit_depth} tiles={tile_args:?}): {msg}"
                        );
                        refusals.push(format!("seed={seed} cq={cq}: {msg}"));
                        continue;
                    }
                };
                assert!(
                    tile_args.is_empty() || decode::tile_hits() - tiles_before >= 2,
                    "tiles={tile_args:?} decoded only {} tile(s) (seed={seed} cq={cq})",
                    decode::tile_hits() - tiles_before
                );
                let vert = decode::tx4x8_coded_hits() - vert_before;
                let horz = decode::tx8x4_coded_hits() - horz_before;
                if vert == 0 || horz == 0 {
                    refusals.push(format!(
                        "seed={seed} cq={cq}: decoded, but coded 4x8={vert} 8x4={horz} leaves"
                    ));
                    continue;
                }
                let ffmpeg_frames = if bit_depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, 1)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, 1)
                };
                assert_eq!(
                    frames[0].y, ffmpeg_frames[0].y,
                    "luma vs ffmpeg (seed={seed} cq={cq} depth={bit_depth} tiles={tile_args:?})"
                );
                assert_eq!(
                    frames[0].u, ffmpeg_frames[0].u,
                    "U vs ffmpeg (seed={seed} cq={cq} depth={bit_depth})"
                );
                assert_eq!(
                    frames[0].v, ffmpeg_frames[0].v,
                    "V vs ffmpeg (seed={seed} cq={cq} depth={bit_depth})"
                );
                fired_runs += 1;
                split_tx += decode::rect8_split_tx_hits() - split_before;
                if fired_runs >= 4 {
                    break;
                }
            }
            assert!(
                fired_runs >= 4,
                "fewer than 4 firing+pixel-exact runs out of 40 attempts \
                 ({width}x{height} depth={bit_depth} tiles={tile_args:?}):\n{}",
                refusals.join("\n")
            );
            // aomenc's default `--enable-tx-size-search=1` makes TX_MODE_SELECT
            // the frame's tx mode, so every sub-8x8 rect leaf carries a
            // `tx_depth` symbol; depth 1 (two 4x4 transform units predicted one
            // after the other) is the half of that path a whole-leaf transform
            // would silently get wrong, so the gate proves it fired.
            assert!(
                split_tx > 0,
                "no sub-8x8 rect leaf used a split (depth-1) transform in the \
                 {fired_runs} pixel-exact runs ({width}x{height} depth={bit_depth})"
            );
        }
    }

    /// lane-fi8 r1: the sub8 rect-leaf gate above with the tool under test
    /// turned ON (`--enable-filter-intra=1`, appended AFTER the base recipe
    /// -- aomenc takes the FIRST occurrence of a repeated flag, so the base
    /// recipe below carries no `--enable-filter-intra` at all). A DC_PRED
    /// `BLOCK_4X8`/`BLOCK_8X4` leaf carries a `use_filter_intra` flag
    /// (`av1_filter_intra_allowed_bsize` is "both sides <= 32"), which this
    /// decoder read from `BLOCK_4X4`'s CDF row until this round -- the same
    /// VALUE, a different interval, so it desynced a block or two later
    /// ([[wrong-alphabet-same-value]]) -- and then refused the prediction by
    /// name. Per attempt, only runs whose [`decode::filter_intra_rect_sub8_hits`]
    /// delta is nonzero AND whose three planes match ffmpeg count; the wider
    /// `filter_intra_rect_hits` would be satisfied by a 16x8 strip and prove
    /// nothing about these shapes. Arms cover both transform depths
    /// (`--enable-tx-size-search=1`, where a leaf may split into two 4x4
    /// units each predicted from the previous one's reconstruction, and `=0`,
    /// one whole-leaf transform) and both bit depths.
    #[test]
    fn a_real_aomenc_stream_with_filter_intra_on_a_sub8_rect_leaf_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_filter_intra_on_a_sub8_rect_leaf_decodes_pixel_exact";
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
        let (width, height) = (16usize, 16usize);
        for (tx_search, bit_depth) in [("1", 8u8), ("0", 8u8), ("1", 10u8)] {
            let mut refusals = Vec::new();
            let mut fired_runs = 0u32;
            let mut fi_leaves = 0usize;
            let mut split_tx = 0usize;
            for attempt in 0..80u32 {
                let seed = 300 + attempt;
                let cq = 6 + (attempt % 8) * 6;
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
                        &format!("testsrc2=size={width}x{height}:rate=25"),
                        "-vf",
                        &format!("noise=alls=40:allf=t:all_seed={seed},format={pix_fmt}"),
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
                    y4m.status.success(),
                    "ffmpeg fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let out = run_with_stdin(
                    Command::new(aomenc_path())
                        .args([
                            "--codec=av1",
                            "--passes=1",
                            "--end-usage=q",
                            &format!("--cq-level={cq}"),
                            "--cpu-used=0",
                            "--threads=1",
                            "--row-mt=0",
                            "--sb-size=64",
                            "--min-partition-size=4",
                            "--max-partition-size=8",
                            "--enable-rect-partitions=1",
                            "--enable-ab-partitions=0",
                            "--enable-1to4-partitions=0",
                            "--enable-palette=0",
                            "--enable-intrabc=0",
                            "--enable-restoration=0",
                            "--enable-cdef=0",
                            "--loopfilter-control=0",
                            "--enable-ref-frame-mvs=0",
                            // The tools under test, first occurrence in the
                            // command line (aomenc keeps the first).
                            "--enable-filter-intra=1",
                            &format!("--enable-tx-size-search={tx_search}"),
                            "--limit=1",
                            "--obu",
                            "-o",
                            "-",
                            "-",
                        ])
                        .args(if bit_depth == 10 {
                            &["--input-bit-depth=10", "--bit-depth=10"][..]
                        } else {
                            &[][..]
                        }),
                    &y4m.stdout,
                );
                if !out.status.success() {
                    refusals.push(format!("seed={seed} cq={cq}: aomenc itself refused"));
                    continue;
                }
                let stream = out.stdout;
                let fi_before = decode::filter_intra_rect_sub8_hits();
                let split_before = decode::rect8_split_tx_hits();
                let frames = match decode_stream(&stream) {
                    Ok(frames) => frames,
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.starts_with("unsupported: "),
                            "decode error that is not a named refusal \
                             (seed={seed} cq={cq} depth={bit_depth} txs={tx_search}): {msg}"
                        );
                        refusals.push(format!("seed={seed} cq={cq}: {msg}"));
                        continue;
                    }
                };
                let fi = decode::filter_intra_rect_sub8_hits() - fi_before;
                if fi == 0 {
                    refusals.push(format!(
                        "seed={seed} cq={cq}: decoded, but no 4x8/8x4 leaf used filter intra"
                    ));
                    continue;
                }
                let ffmpeg_frames = if bit_depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, 1)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, 1)
                };
                assert_eq!(
                    frames[0].y, ffmpeg_frames[0].y,
                    "luma vs ffmpeg (seed={seed} cq={cq} depth={bit_depth} txs={tx_search})"
                );
                assert_eq!(
                    frames[0].u, ffmpeg_frames[0].u,
                    "U vs ffmpeg (seed={seed} cq={cq} depth={bit_depth} txs={tx_search})"
                );
                assert_eq!(
                    frames[0].v, ffmpeg_frames[0].v,
                    "V vs ffmpeg (seed={seed} cq={cq} depth={bit_depth} txs={tx_search})"
                );
                fired_runs += 1;
                fi_leaves += fi;
                split_tx += decode::rect8_split_tx_hits() - split_before;
                if fired_runs >= 3 {
                    break;
                }
            }
            assert!(
                fired_runs >= 3,
                "{NAME}: fewer than 3 firing+pixel-exact runs out of 80 attempts \
                 (depth={bit_depth} txs={tx_search}), {fi_leaves} filter-intra sub8 leaves \
                 seen:\n{}",
                refusals.join("\n")
            );
            if tx_search == "1" {
                assert!(
                    split_tx > 0,
                    "{NAME}: no sub-8x8 rect leaf used a split (depth-1) transform in the \
                     {fired_runs} pixel-exact runs (depth={bit_depth})"
                );
            }
            eprintln!(
                "{NAME}: depth={bit_depth} txs={tx_search} {fired_runs} pixel-exact runs, \
                 {fi_leaves} filter-intra 4x8/8x4 leaves, split_tx={split_tx}"
            );
        }
    }

    /// lane-tx4x8 r3 ([[enumerate-table-domain]], [[tool-disabled-in-every-gate]]):
    /// a 16x8/8x16 intra strip whose DIRECTIONAL mode reads above-right or
    /// below-left samples. Which of those the decoder may read is
    /// `Reach::of_rect`'s answer, and until this round `rect_reach_tables`
    /// had no 16x8 row at all -- every 16x8 strip read libaom's `has_tr_32x16`
    /// row instead of `has_tr_16x8`, wrong at 10 of the 21 reachable
    /// superblock positions for above-right and 7 for below-left. No gate saw
    /// it: the sibling strip gate (`coded_rect_strip_below_16x16`) runs with
    /// smooth/paeth/DC modes, which never read past the block, so the wrong
    /// bit changed no pixel there.
    ///
    /// So this gate turns every non-directional intra tool OFF
    /// (`--enable-smooth-intra=0 --enable-paeth-intra=0
    /// --enable-filter-intra=0`) at `--min-partition-size=8
    /// --max-partition-size=16 --enable-rect-partitions=1`, and counts, per
    /// attempt, the 16x8/8x16 strips whose prediction actually reached past
    /// their own width or height ([`decode::rect_strip_reach_hits`], recorded
    /// in the predictor's edge fetch). An attempt only counts when that
    /// counter moved AND the frame is pixel-exact against ffmpeg.
    ///
    /// Un-ignored 2026-09-04: the sub-16x16 coded-strip and AB-partition refusals that blocked it are gone and the gate passes. When it was blocked: the 16x16-level strip decoder
    /// refuses every NON-SKIP 16x8/8x16 strip ("a coded (non-skip) HORZ/VERT
    /// rect strip below 16x16", `decode.rs`), and that is what real streams
    /// contain -- measured on this exact recipe, 38 of 40 attempts died on
    /// that refusal and 2 on a sub-16x16 AB partition, at cq 30/45/58 and at
    /// `--min-partition-size=16` (which yields no 16x8 at all, aomenc bounds a
    /// rect shape by its smaller side). The table routing is also pinned by the domain test
    /// `encode::tests::every_rect_shape_reaches_what_libaom_says_over_the_whole_superblock`
    /// against libaom's own `has_tr_*`/`has_bl_*` arrays.
    #[test]
    fn a_directional_16x8_strip_reads_the_right_above_right_samples() {
        const NAME: &str = "a_directional_16x8_strip_reads_the_right_above_right_samples";
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
        // 64x16, not 64x64: a 16-tall frame is a single row of 16x16 blocks,
        // so a HORZ arm is a 16x8 strip at column 0..3 of the superblock --
        // exactly the positions whose `has_tr_16x8`/`has_bl_16x8` bit the old
        // routing got wrong -- while the frame is too short for aomenc to
        // build the deep sub-16x16 partitions other lanes still refuse. It
        // must also be WIDER than one block: at width 16 the block is the
        // frame, and `x + bw < width` kills above-right before any table.
        for (width, height, bit_depth) in [(64usize, 16usize, 8u8), (64, 16, 10u8)] {
            let mut refusals = Vec::new();
            let mut fired_runs = 0u32;
            let mut reach_hits = 0usize;
            for attempt in 0..40u32 {
                let seed = 700 + attempt;
                // Coarse quantisers on lightly-dithered content: fine detail
                // at low cq makes aomenc build the sub-16x16 partitions other
                // lanes refuse, and 40/40 attempts died there.
                let cq = 45 + (attempt % 4);
                let half = height / 2;
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
                        &format!("testsrc2=size={width}x{half}:rate=25"),
                        "-f",
                        "lavfi",
                        "-i",
                        &format!("color=c=gray:s={width}x{half}:rate=25"),
                        "-filter_complex",
                        &format!(
                            "[0]noise=alls=8:allf=t:all_seed={seed}[t];[t][1]vstack,\
                             format={pix_fmt}"
                        ),
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
                    y4m.status.success(),
                    "ffmpeg fixture: {}",
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let out = run_with_stdin(
                    Command::new(aomenc_path())
                        .args([
                            "--codec=av1",
                            "--passes=1",
                            "--end-usage=q",
                            &format!("--cq-level={cq}"),
                            "--cpu-used=0",
                            "--threads=1",
                            "--row-mt=0",
                            "--sb-size=64",
                            // The tool under test: 16x8/8x16 strips, predicted by
                            // a mode that reads beyond the block.
                            // aomenc bounds a RECT shape by its smaller side, so
                            // `--min-partition-size=16` yields no 16x8 at all
                            // (measured: 0 strips in 40 attempts); 8 is the
                            // smallest value that lets a 16x16 split HORZ/VERT.
                            "--min-partition-size=8",
                            "--max-partition-size=16",
                            // Largest transform per block: a split (depth-1) rect
                            // strip is a named refusal of another lane, and it
                            // ate every attempt while it was on.
                            "--enable-tx-size-search=0",
                            "--enable-rect-partitions=1",
                            "--enable-directional-intra=1",
                            "--enable-smooth-intra=0",
                            "--enable-paeth-intra=0",
                            "--enable-filter-intra=0",
                            "--enable-ab-partitions=0",
                            "--enable-1to4-partitions=0",
                            "--enable-palette=0",
                            "--enable-intrabc=0",
                            "--enable-restoration=0",
                            "--enable-cdef=0",
                            "--loopfilter-control=0",
                            "--enable-ref-frame-mvs=0",
                            "--limit=1",
                            "--obu",
                            "-o",
                            "-",
                            "-",
                        ])
                        .args(if bit_depth == 10 {
                            &["--input-bit-depth=10", "--bit-depth=10"][..]
                        } else {
                            &[][..]
                        }),
                    &y4m.stdout,
                );
                if !out.status.success() {
                    refusals.push(format!(
                        "seed={seed} cq={cq}: aomenc itself refused the fixture"
                    ));
                    continue;
                }
                let stream = out.stdout;
                let reach_before = decode::rect_strip_reach_hits();
                let pred_before = decode::rect_strip_pred_hits();
                let frames = match decode_stream(&stream) {
                    Ok(frames) => frames,
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.starts_with("unsupported: "),
                            "decode error that is not a named refusal \
                             (seed={seed} cq={cq} depth={bit_depth}): {msg}"
                        );
                        refusals.push(format!("seed={seed} cq={cq}: {msg}"));
                        continue;
                    }
                };
                let reached = decode::rect_strip_reach_hits() - reach_before;
                let predicted = decode::rect_strip_pred_hits() - pred_before;
                if predicted == 0 {
                    refusals.push(format!(
                        "seed={seed} cq={cq}: decoded, but no 16x8/8x16 strip in the frame"
                    ));
                    continue;
                }
                let ffmpeg_frames = if bit_depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, 1)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, 1)
                };
                assert_eq!(
                    frames[0].y, ffmpeg_frames[0].y,
                    "luma vs ffmpeg (seed={seed} cq={cq} depth={bit_depth}, {predicted} strips, \
                     {reached} of them reaching)"
                );
                assert_eq!(
                    frames[0].u, ffmpeg_frames[0].u,
                    "U vs ffmpeg (seed={seed} cq={cq} depth={bit_depth})"
                );
                assert_eq!(
                    frames[0].v, ffmpeg_frames[0].v,
                    "V vs ffmpeg (seed={seed} cq={cq} depth={bit_depth})"
                );
                fired_runs += 1;
                reach_hits += reached;
                if fired_runs >= 4 {
                    break;
                }
            }
            assert!(
                fired_runs >= 4,
                "fewer than 4 firing+pixel-exact runs out of 40 attempts \
                 ({width}x{height} depth={bit_depth}, {reach_hits} reaching strips):\n{}",
                refusals.join("\n")
            );
        }
    }

    /// lane-av1golden7 r9's decisive fixture: a real aomenc stream (seed 59,
    /// `--tune-content=film`, GOLDEN_FRAME firing downstream) whose frame-0
    /// *intra keyframe* mismatched ffmpeg on 3855/4096 luma pixels -- traced
    /// to `apply_grain=true` (spec 7.18.3 grain synthesis, now implemented in
    /// `crate::film_grain`), entirely unrelated to GOLDEN_FRAME or frame
    /// ordering. Frames beyond 0 in this fixture hit an unrelated, pre-existing
    /// gap (`decode_inter_block`'s GOLDEN_FRAME MC path, lane-av1golden7's own
    /// `golden7-forwarding-mismatch.obu`), so this test truncates the stream to
    /// the sequence header + frame-0 OBUs only -- exactly the span the
    /// mismatch was traced to -- rather than reaching into `decode.rs`/`mc.rs`
    /// to widen MC support, which is out of this lane's scope.
    #[test]
    fn a_real_aomenc_stream_with_film_grain_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP a_real_aomenc_stream_with_film_grain_decodes_pixel_exact: no ffmpeg");
            return;
        }
        // lane-av1pinslive r3: this gate was LIVE while reading a GITIGNORED
        // pin through a `concat!` literal and `.unwrap()` -- a clean checkout
        // with no runner library PANICKED here rather than skipping. The bytes
        // are now committed at `crates/ec-av1/fixtures/golden6-mismatch.obu`
        // (452 bytes; sha256
        // `c56909b98542192b03f3a945e153475aa57a58a1586bf166513297296cb225b3`,
        // recovered byte-for-byte from the runner library).
        let data = require_pin(
            &std::env::var("EC_AV1_GATE_DUMP_PIN")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| crate_pin("golden6-mismatch.obu")),
            "golden6-mismatch.obu",
        );
        // Truncate right after the first `Frame` OBU (the key frame): every
        // byte up to there is sequence header + frame-0, which is a complete,
        // independently decodable single-frame stream.
        let mut parser = Av1Parser::new();
        let mut pos = 0usize;
        let mut frame0_end = None;
        while pos < data.len() {
            let obu = parser.parse_obu(&data[pos..]).unwrap();
            pos += obu.total_size;
            if matches!(obu.kind, ObuKind::Frame(..)) {
                frame0_end = Some(pos);
                break;
            }
        }
        let data = &data[..frame0_end.expect("fixture has at least one frame OBU")];
        let (width, height) = (64usize, 64usize);
        let ffmpeg_frames = ffmpeg_decode_sequence(data, width, height, 1);
        let decoded = decode_stream(data).unwrap();
        assert_eq!(decoded.len(), 1);
        let (got, want) = (&decoded[0], &ffmpeg_frames[0]);
        assert_eq!(got.y, want.y, "frame 0 luma vs ffmpeg (film grain)");
        assert_eq!(got.u, want.u, "frame 0 U vs ffmpeg (film grain)");
        assert_eq!(got.v, want.v, "frame 0 V vs ffmpeg (film grain)");
    }

    /// A real-encoder sweep, not just the one pinned fixture above
    /// (fixture-proves-symbol-not-signal class): several fresh
    /// `--tune-content=film` draws at different seeds/content, each decoded
    /// pixel-exact against ffmpeg's own AV1 decoder (which synthesizes grain
    /// by default -- confirmed via `ffmpeg -h decoder=av1`, no
    /// `-export_side_data`/grain-disable flag is passed anywhere in this
    /// file).
    #[test]
    fn real_aomenc_film_grain_streams_decode_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!("SKIP real_aomenc_film_grain_streams_decode_pixel_exact: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP real_aomenc_film_grain_streams_decode_pixel_exact: no aomenc");
            return;
        }
        let (width, height) = (64usize, 64usize);
        for seed in [1u64, 2, 3] {
            let dir = std::env::temp_dir().join(format!(
                "ec-av1-filmgrain-gate-{}-{seed}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let yuv_path = dir.join("in.yuv");
            let obu_path = dir.join("out.obu");
            let frames = 3usize;
            let luma = width * height;
            let chroma = luma / 4;
            let mut yuv = Vec::with_capacity((luma + 2 * chroma) * frames);
            for f in 0..frames {
                let card = panned_test_card(width, height, ((f as i32 + seed as i32) * 5) as i64);
                yuv.extend(card.y.iter().map(|&v| v as u8));
                yuv.extend(card.u.iter().map(|&v| v as u8));
                yuv.extend(card.v.iter().map(|&v| v as u8));
            }
            std::fs::write(&yuv_path, &yuv).unwrap();
            let status = Command::new(aomenc_path())
                .args([
                    &format!("--width={width}"),
                    &format!("--height={height}"),
                    "--input-bit-depth=8",
                    "--bit-depth=8",
                    "--fps=25/1",
                    &format!("--limit={frames}"),
                    // The full constraint set of the forwarding/golden gates,
                    // so the streams stay inside this decoder's supported
                    // envelope and the gate isolates grain synthesis alone;
                    // `--tune-content=film` is the one flag under test (it
                    // enables apply_grain).
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=32",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    "--max-reference-frames=3",
                    "--reduced-reference-set=1",
                    "--enable-warped-motion=0",
                    "--enable-obmc=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-dist-wtd-comp=0",
                    "--enable-diff-wtd-comp=0",
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
                    // Temporal MV projection (`use_ref_frame_mvs`) has no
                    // decode path here (`mvstack.rs`'s documented corner-cut:
                    // `zero_mv_ctx` and the mode-context GLOBALMV_OFFSET bit
                    // are always computed as if it were off) -- this gate
                    // isolates grain synthesis, so it must stay inside that
                    // envelope like every other feature disabled above, not
                    // silently decode a wrong mode context. Root cause of
                    // this test's frame-1 mismatch: aomenc turns
                    // `use_ref_frame_mvs` on by default once order hints are
                    // enabled, unrelated to `--tune-content=film`.
                    "--enable-ref-frame-mvs=0",
                    "--tune-content=film",
                    "--obu",
                    "-o",
                ])
                .arg(&obu_path)
                .arg(&yuv_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("aomenc failed to start");
            assert!(status.success(), "aomenc failed for seed {seed}");
            let encoded = std::fs::read(&obu_path).unwrap();
            let _ = std::fs::remove_dir_all(&dir);

            let mut parser = ec_av1_syntax::Av1Parser::new();
            let mut pos = 0usize;
            let mut saw_grain = false;
            while pos < encoded.len() {
                let obu = parser.parse_obu(&encoded[pos..]).unwrap();
                pos += obu.total_size;
                if let ec_av1_syntax::ObuKind::Frame(header, _) = obu.kind
                    && header.film_grain.apply_grain
                {
                    saw_grain = true;
                }
            }
            assert!(saw_grain, "seed {seed}: aomenc did not turn on apply_grain");

            let ffmpeg_frames = ffmpeg_decode_sequence(&encoded, width, height, frames);
            let decoded = decode_stream(&encoded).unwrap();
            assert_eq!(decoded.len(), frames, "seed {seed}");
            for (i, (got, want)) in decoded.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "seed {seed} frame {i} luma vs ffmpeg");
                assert_eq!(got.u, want.u, "seed {seed} frame {i} U vs ffmpeg");
                assert_eq!(got.v, want.v, "seed {seed} frame {i} V vs ffmpeg");
            }
        }
    }

    /// lane-tiles r3/r7: a real two-tile-column aomenc key frame, decoded
    /// through [`decode_key_frame_tile_with_cdfs`] directly with the whole
    /// `tiles: &[&[u8]]` list -- `decode_stream`'s `cols > 1 || rows > 1`
    /// refusal (this file, ~line 292) still fires for any stream this wide,
    /// so this gate bypasses it on purpose to prove the per-tile loop r2
    /// landed (fresh `SymbolDecoder`/CDF copy per tile, `Neighbours::start_tile`,
    /// `PlaneBuf::set_tile_origin`) actually decodes both tiles pixel-exact
    /// before that refusal is lifted. 128x64 is exactly two 64x64
    /// superblocks wide with `--tile-columns=1` (`TileCols=2`), so tile 1 is
    /// also the frame's own right edge -- the known gap (`PlaneBuf`'s
    /// tile_x0/tile_y0 only clip left/top reach, not a non-last column's
    /// right-edge reach) does not apply to this fixture; it would to a
    /// 3+-column one, proven separately by the four-tile-column gate below.
    /// r7: `--loopfilter-control=0` REMOVED per the charter -- `PlaneBuf`'s
    /// `tile_x1`/`tile_y1` clamps now receive real values at every call site
    /// (r6's `set_tile_origin` sweep), so this gate now proves real
    /// deblocking across the tile boundary too, not just decode with it off.
    /// Tile count itself is a structural aomenc param, not
    /// RD-dependent content, but this decoder's chroma smooth/paeth gap
    /// (`decode.rs` "a smooth or paeth chroma mode (round 2)", unrelated to
    /// tiles, pre-dating this lane) is content-dependent -- no aomenc flag
    /// disables UV-only smooth/paeth search, so a small seed sweep is
    /// needed the same way the sibling `a_real_aomenc_stream_with_*` gates
    /// retry past unrelated named refusals (every refusal in this crate is
    /// prefixed `"AV1 tile"` regardless of category, so unlike the sibling
    /// gates' `warp`/`obmc`/`interintra` banned lists there is no
    /// tile-specific substring to forbid here -- `matched > 0` plus the
    /// hard `tile_hits > 1`/pixel-exact asserts on any actual decode are
    /// the real proof).
    #[test]
    fn a_real_aomenc_stream_with_two_tile_columns_decodes_pixel_exact() {
        let fctx = &crate::decode::FrameCtx::new();
        const NAME: &str = "a_real_aomenc_stream_with_two_tile_columns_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (128usize, 64usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        // lane-modectx r1: luma `kf_y_mode` neighbour reads whose coarse
        // fallback band held a previous tile's mode at a tile edge. Measured 0
        // -- `above_mode` is cleared per tile and `left_mode` per superblock
        // row -- and that is exactly why the coarse fallback is safe where
        // lane-mtfix's chroma one was not. Accumulated only for attempts that
        // then compared pixel-exact (class counter-from-refused-stream).
        let mut mode_edge_leaks = 0usize;
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
                    "--tile-columns=1",
                    "--tile-rows=0",
                    // This decoder hardcodes SB_MI=16 (64px) superblocks
                    // everywhere (`decode.rs`'s sb128 dead-end, ledger
                    // `av1-superblock-contexts`) -- without this, aomenc's
                    // own default 128x128 superblock makes 128x64 exactly
                    // ONE superblock wide, so `--tile-columns=1` has nothing
                    // to split and `cols` comes back 1.
                    "--sb-size=64",
                    // Deblocking crosses tile boundaries by default (spec
                    // `loop_filter_across_tiles_enabled`); `PlaneBuf`'s
                    // tile_x0/tile_y0 origin only ever clips a tile's own
                    // left/top reach (r2/r3's documented known gap), so a
                    // non-last tile column's right-edge deblock would read
                    // into the next tile's still-undecoded/differently-
                    // origined samples. Off here since deblocking itself is
                    // not what this gate is proving.
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

            // Parse the stream ourselves (rather than `decode_stream`, whose
            // multi-tile refusal is exactly what this gate exists to run
            // ahead of) to reach the real `Frame` OBU's `tile_info` and its
            // full `Vec<Tile>` -- `Av1Parser::parse_obu` already locates
            // every tile in a multi-tile tile group (`parse_tile_group`), it
            // is only `decode_stream` that ever narrows this to
            // `tiles.first()`.
            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut frame = None;
            while pos < stream.len() {
                let obu_offset = pos;
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, tiles) = obu.kind {
                    frame = Some((header, tiles, obu_offset));
                    break;
                }
            }
            let (header, tiles, obu_offset) = frame.expect("stream has a Frame OBU");
            assert!(
                header.tile_info.cols > 1,
                "{NAME}: aomenc did not actually split into >1 tile column (cols={}, seed {seed})",
                header.tile_info.cols
            );
            assert_eq!(
                tiles.len(),
                header.tile_info.cols as usize,
                "{NAME}: expected one tile per column (seed {seed})"
            );
            // `Tile::offset` is relative to the buffer `parse_obu` was
            // handed (`&stream[pos..]` at the time this OBU was parsed), so
            // it needs `obu_offset` added back -- the same adjustment
            // `decode_stream` makes for its own single-tile slice above.
            let tile_bufs: Vec<&[u8]> = tiles
                .iter()
                .map(|t| &stream[obu_offset + t.offset..obu_offset + t.offset + t.size])
                .collect();

            let enable_filter_intra = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_filter_intra);
            let enable_edge_filter = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_intra_edge_filter);

            let before = crate::decode::tile_hits();
            let before_edge = crate::decode::mode_tile_edge_coarse_leaks();
            let picture = match decode_key_frame_tile_with_cdfs(
                &tile_bufs,
                &header.tile_info,
                header.mi_cols,
                header.mi_rows,
                header.quantization.base_q_idx,
                crate::quant::QuantDeltas {
                    y_dc: i32::from(header.quantization.delta_q_y_dc),
                    u_dc: i32::from(header.quantization.delta_q_u_dc),
                    u_ac: i32::from(header.quantization.delta_q_u_ac),
                    v_dc: i32::from(header.quantization.delta_q_v_dc),
                    v_ac: i32::from(header.quantization.delta_q_v_ac),
                },
                header.frame_width,
                header.frame_height,
                enable_filter_intra,
                enable_edge_filter,
                &header.cdef,
                &header.loop_filter,
                &header.loop_restoration,
                None,
                header.tx_mode == TxMode::Select,
                header.reduced_tx_set,
                header.allow_screen_content_tools,
                header.allow_intrabc,
                header.delta,
                fctx,
            ) {
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
                Ok((picture, _end_cdfs)) => picture,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 1,
                "{NAME}: tile_hits delta was {hits}, expected >1 -- both tiles must actually \
                 decode, not just tile 0 (seed {seed})"
            );

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                picture.y, ffmpeg_frames[0].y,
                "{NAME}: luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                picture.u, ffmpeg_frames[0].u,
                "{NAME}: U vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                picture.v, ffmpeg_frames[0].v,
                "{NAME}: V vs ffmpeg (seed {seed})"
            );
            mode_edge_leaks += crate::decode::mode_tile_edge_coarse_leaks() - before_edge;
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); the gate never decoded \
             a two-tile-column stream"
        );
        assert_eq!(
            mode_edge_leaks, 0,
            "{NAME}: a luma mode neighbour read at a tile edge saw a NON-DC coarse band -- the \
             per-tile `above_mode` / per-superblock-row `left_mode` reset that makes the coarse \
             fallback safe has regressed"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20, \
             {mode_edge_leaks} tile-edge coarse leaks"
        );
    }

    /// lane-tiles r8 stage 1: the same bypass-the-refusal pattern as the
    /// two-tile-column gate above, this time for tile ROWS -- `decode_stream`
    /// still refuses any `tile_info.rows > 1` stream, so this gate calls
    /// [`decode_key_frame_tile_with_cdfs`] directly to prove the underlying
    /// per-tile loop (already row/col-generic: `tile_num / tile_info.cols`
    /// gives the row, `mi_row_starts`/`set_tile_origin`'s `y0`/`y1` are
    /// exactly symmetric with the column gate's `x0`/`x1`) actually decodes
    /// both tile rows pixel-exact before the refusal is lifted. 64x128 is
    /// one 64px superblock wide and two tall with `--tile-rows=1`
    /// (`TileRows=2`, `--tile-columns=0` keeps `cols=1` so only the row axis
    /// is under test). Loop filter is left ON (no
    /// `--loopfilter-control=0`) per the charter -- deblocking crosses tile
    /// row boundaries by spec default exactly as it crosses column ones, and
    /// `deblock_plane` runs once over the whole decoded picture after every
    /// tile is in, with no tile-axis distinction, so there is no reason to
    /// expect the row boundary to behave differently from the column one r7
    /// already proved -- this gate is what actually proves it either way.
    #[test]
    fn a_real_aomenc_stream_with_two_tile_rows_decodes_pixel_exact() {
        let fctx = &crate::decode::FrameCtx::new();
        const NAME: &str = "a_real_aomenc_stream_with_two_tile_rows_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 128usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
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
                    "--tile-columns=0",
                    "--tile-rows=1",
                    // This decoder hardcodes SB_MI=16 (64px) superblocks
                    // everywhere; without this aomenc's own default 128x128
                    // superblock makes 64x128 exactly ONE superblock tall,
                    // so `--tile-rows=1` has nothing to split.
                    "--sb-size=64",
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

            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut frame = None;
            while pos < stream.len() {
                let obu_offset = pos;
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, tiles) = obu.kind {
                    frame = Some((header, tiles, obu_offset));
                    break;
                }
            }
            let (header, tiles, obu_offset) = frame.expect("stream has a Frame OBU");
            assert!(
                header.tile_info.rows > 1,
                "{NAME}: aomenc did not actually split into >1 tile row (rows={}, seed {seed})",
                header.tile_info.rows
            );
            assert_eq!(
                tiles.len(),
                header.tile_info.rows as usize,
                "{NAME}: expected one tile per row (cols=1, seed {seed})"
            );
            let tile_bufs: Vec<&[u8]> = tiles
                .iter()
                .map(|t| &stream[obu_offset + t.offset..obu_offset + t.offset + t.size])
                .collect();

            let enable_filter_intra = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_filter_intra);
            let enable_edge_filter = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_intra_edge_filter);

            let before = crate::decode::tile_hits();
            let picture = match decode_key_frame_tile_with_cdfs(
                &tile_bufs,
                &header.tile_info,
                header.mi_cols,
                header.mi_rows,
                header.quantization.base_q_idx,
                crate::quant::QuantDeltas {
                    y_dc: i32::from(header.quantization.delta_q_y_dc),
                    u_dc: i32::from(header.quantization.delta_q_u_dc),
                    u_ac: i32::from(header.quantization.delta_q_u_ac),
                    v_dc: i32::from(header.quantization.delta_q_v_dc),
                    v_ac: i32::from(header.quantization.delta_q_v_ac),
                },
                header.frame_width,
                header.frame_height,
                enable_filter_intra,
                enable_edge_filter,
                &header.cdef,
                &header.loop_filter,
                &header.loop_restoration,
                None,
                header.tx_mode == TxMode::Select,
                header.reduced_tx_set,
                header.allow_screen_content_tools,
                header.allow_intrabc,
                header.delta,
                fctx,
            ) {
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
                Ok((picture, _end_cdfs)) => picture,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 1,
                "{NAME}: tile_hits delta was {hits}, expected >1 -- both tile rows must actually \
                 decode, not just tile 0 (seed {seed})"
            );

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                picture.y, ffmpeg_frames[0].y,
                "{NAME}: luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                picture.u, ffmpeg_frames[0].u,
                "{NAME}: U vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                picture.v, ffmpeg_frames[0].v,
                "{NAME}: V vs ffmpeg (seed {seed})"
            );
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); the gate never decoded \
             a two-tile-row stream"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r8 stage 2/3: the same two-tile-row stream as the bypass
    /// gate above, this time through `decode_stream` itself -- the entry
    /// point real callers use, with the `rows > 1` refusal lifted (same
    /// commit) so this actually reaches the tile decode instead of
    /// refusing by name. Loop filter ON, no `--loopfilter-control=0`.
    #[test]
    fn a_real_aomenc_stream_with_two_tile_rows_decodes_through_decode_stream() {
        const NAME: &str = "a_real_aomenc_stream_with_two_tile_rows_decodes_through_decode_stream";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 128usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
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
                    "--tile-columns=0",
                    "--tile-rows=1",
                    "--sb-size=64",
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
                hits > 1,
                "{NAME}: tile_hits delta was {hits}, expected >1 -- both tile rows must actually \
                 decode, not just tile 0 (seed {seed})"
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
             decoded a two-tile-row stream"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r7: the `>2 tile columns` refusal's own capability, proven
    /// separately from the 2-column gate above -- `--tile-columns=2` gives
    /// aomenc `TileCols=4` (log2), a real non-last-column right-edge case the
    /// 2-column fixture cannot exercise (there tile 1 IS the frame's right
    /// edge). 256x64 is four 64x64 superblocks wide with `--sb-size=64`, so
    /// each tile column is exactly one superblock. Loop filtering stays ON
    /// (no `--loopfilter-control=0`) to prove the same reach-bound fix also
    /// covers deblocking across an *interior* tile boundary, not just the
    /// 2-column case's boundary-that-is-also-the-frame-edge.
    #[test]
    fn a_real_aomenc_stream_with_four_tile_columns_decodes_pixel_exact() {
        let fctx = &crate::decode::FrameCtx::new();
        const NAME: &str = "a_real_aomenc_stream_with_four_tile_columns_decodes_pixel_exact";
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
                    "--tile-columns=2",
                    "--tile-rows=0",
                    "--sb-size=64",
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

            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut frame = None;
            while pos < stream.len() {
                let obu_offset = pos;
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, tiles) = obu.kind {
                    frame = Some((header, tiles, obu_offset));
                    break;
                }
            }
            let (header, tiles, obu_offset) = frame.expect("stream has a Frame OBU");
            assert!(
                header.tile_info.cols > 2,
                "{NAME}: aomenc did not actually split into >2 tile columns (cols={}, seed {seed})",
                header.tile_info.cols
            );
            assert_eq!(
                tiles.len(),
                header.tile_info.cols as usize,
                "{NAME}: expected one tile per column (seed {seed})"
            );
            let tile_bufs: Vec<&[u8]> = tiles
                .iter()
                .map(|t| &stream[obu_offset + t.offset..obu_offset + t.offset + t.size])
                .collect();

            let enable_filter_intra = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_filter_intra);
            let enable_edge_filter = parser
                .sequence_header()
                .is_some_and(|seq| seq.enable_intra_edge_filter);

            let before = crate::decode::tile_hits();
            let picture = match decode_key_frame_tile_with_cdfs(
                &tile_bufs,
                &header.tile_info,
                header.mi_cols,
                header.mi_rows,
                header.quantization.base_q_idx,
                crate::quant::QuantDeltas {
                    y_dc: i32::from(header.quantization.delta_q_y_dc),
                    u_dc: i32::from(header.quantization.delta_q_u_dc),
                    u_ac: i32::from(header.quantization.delta_q_u_ac),
                    v_dc: i32::from(header.quantization.delta_q_v_dc),
                    v_ac: i32::from(header.quantization.delta_q_v_ac),
                },
                header.frame_width,
                header.frame_height,
                enable_filter_intra,
                enable_edge_filter,
                &header.cdef,
                &header.loop_filter,
                &header.loop_restoration,
                None,
                header.tx_mode == TxMode::Select,
                header.reduced_tx_set,
                header.allow_screen_content_tools,
                header.allow_intrabc,
                header.delta,
                fctx,
            ) {
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
                Ok((picture, _end_cdfs)) => picture,
            };
            let hits = crate::decode::tile_hits() - before;
            assert!(
                hits > 2,
                "{NAME}: tile_hits delta was {hits}, expected >2 -- all four tiles must actually \
                 decode, not just a subset (seed {seed})"
            );

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                picture.y, ffmpeg_frames[0].y,
                "{NAME}: luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                picture.u, ffmpeg_frames[0].u,
                "{NAME}: U vs ffmpeg (seed {seed})"
            );
            assert_eq!(
                picture.v, ffmpeg_frames[0].v,
                "{NAME}: V vs ffmpeg (seed {seed})"
            );
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); the gate never decoded \
             a four-tile-column stream"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r8: the tile-*row* analogue of the four-tile-column gate
    /// above -- `--tile-rows=2` gives aomenc `TileRows=4` (log2; uniform
    /// tile spacing is always a power of two, so this is the smallest count
    /// that has an interior row neither the top nor bottom frame edge,
    /// mirroring why the column gate needed 4 columns rather than 3). 64x256
    /// is four 64x64 superblocks tall with `--sb-size=64`, one per tile row.
    /// Loop filter ON, run through `decode_stream` itself since the
    /// `rows > 1` refusal is already lifted by this commit. Also settles a
    /// stale documentation claim found while writing this gate: the old
    /// comment above `decode_stream`'s multi-tile block said
    /// `context_update_tile_id != 0` was refused "further down" -- no such
    /// refusal exists (`decode.rs` reads `tile_info.context_update_tile_id`
    /// generically, never hardcoded to tile 0), and this fixture's own
    /// aomenc runs pick 1/2/3 as often as 0 (RD-driven tile-size heuristic),
    /// all 20/20 pixel-exact -- so the capability was already there,
    /// undocumented and unproven; hard-asserted here so the proof can't go
    /// unnoticed if it stops firing.
    #[test]
    fn a_real_aomenc_stream_with_four_tile_rows_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_four_tile_rows_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 256usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut saw_nonzero_context_update_tile_id = false;
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
                    "--tile-columns=0",
                    "--tile-rows=2",
                    "--sb-size=64",
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

            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut rows_seen = 0u32;
            while pos < stream.len() {
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, _) = &obu.kind {
                    rows_seen = header.tile_info.rows;
                    if header.tile_info.context_update_tile_id != 0 {
                        saw_nonzero_context_update_tile_id = true;
                    }
                }
            }
            assert!(
                rows_seen > 2,
                "{NAME}: aomenc did not actually split into >2 tile rows (rows={rows_seen}, seed {seed})"
            );

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
                hits > 2,
                "{NAME}: tile_hits delta was {hits}, expected >2 -- all four tile rows must \
                 actually decode, not just a subset (seed {seed})"
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
            "{NAME}: every attempt refused ({named_refusals} refusals); the gate never decoded \
             a four-tile-row stream"
        );
        assert!(
            saw_nonzero_context_update_tile_id,
            "{NAME}: every attempt named context_update_tile_id=0 -- this sweep proves nothing \
             about the != 0 case (see the stale-refusal note above `decode_stream`'s multi-tile \
             comment: aomenc's own tile-size heuristic picks it, seen 1/2/3 live in a prior probe \
             run of this exact fixture, so a 20-attempt window not seeing it is the gate's own bug)"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r4/r7: the same real two-tile-column stream as the
    /// bypass gate above, this time run through `decode_stream` itself --
    /// the entry point real callers use. Proves the `cols > 1` refusal is
    /// actually lifted (not just that the underlying per-tile loop works)
    /// for a key frame; `--loopfilter-control=0` stays here for its own
    /// sake (isolating the entry-point wiring from deblocking), but r7's
    /// `a_real_aomenc_stream_with_four_tile_columns_decodes_pixel_exact`
    /// bypass gate now proves loop filtering ON across a tile boundary too,
    /// and the `cols > 1 && loop_filter_on` / `cols > 2` refusals this
    /// comment used to describe are gone from `decode_stream`.
    #[test]
    fn a_real_aomenc_stream_with_two_tile_columns_decodes_through_decode_stream() {
        const NAME: &str =
            "a_real_aomenc_stream_with_two_tile_columns_decodes_through_decode_stream";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (128usize, 64usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
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
                    "--tile-columns=1",
                    "--tile-rows=0",
                    "--sb-size=64",
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
                hits > 1,
                "{NAME}: tile_hits delta was {hits}, expected >1 -- both tiles must actually \
                 decode, not just tile 0 (seed {seed})"
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
             decoded a two-tile-column stream"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-realworld r6: r5 ported `maybe_read_delta_q`/`maybe_read_delta_lf`
    /// and removed the whole-frame refusal, but no fixture anywhere set
    /// `delta_lf_present` -- the removal was unproven. `--deltaq-mode=2`
    /// (`DELTA_Q_PERCEPTUAL`, checked against libaom's own
    /// `encodeframe.c:2192-2225`) sets `delta_q_present_flag` unconditionally
    /// once `base_qindex > 0` (unlike the default `--deltaq-mode=1`
    /// `DELTA_Q_OBJECTIVE`, which additionally requires an alt-ref-eligible
    /// frame and `allow_deltaq_mode`'s own RD search); `--delta-lf-mode=1`
    /// then ANDs `delta_lf_present_flag` on top
    /// (`tool_cfg->enable_deltalf_mode`, `av1_cx_iface.c:1269-1270`). 128x64
    /// (2 SBs; this decoder hardcodes 64px SBs) is the charter's minimum for
    /// a per-superblock symbol to have somewhere to differ; `--cpu-used=4`
    /// (not the other gates' `=0`) sidesteps the multi-SB
    /// HORZ_4/VERT_B-at-part64 gap the cdef gate's own comment documents
    /// (lane-realworld r2 dead-end, part64 only covers NONE/SPLIT). Note:
    /// libaom hardcodes `delta_lf_multi = DEFAULT_DELTA_LF_MULTI == 0`
    /// (`enums.h:73`) with no CLI flag to set it -- this gate cannot and
    /// does not claim to exercise this decoder's `delta_lf_multi` branch;
    /// only the single-plane path is gate-proven.
    #[test]
    fn a_real_aomenc_stream_with_delta_q_and_delta_lf_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_delta_q_and_delta_lf_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (128usize, 64usize, 24usize);
        let mut named_refusals = 0u32;
        let mut matched = 0u32;
        let n_attempts: u32 = std::env::var("EC_DELTAQ_GATE_ATTEMPTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(40);
        for attempt in 0..n_attempts {
            let seed = 42 + attempt;
            let duration = frame_count as f64 / 25.0;
            // Mandelbrot's varying local contrast is what gives
            // DELTA_Q_PERCEPTUAL's per-superblock RD something to
            // differentiate; flat gradients risk every superblock
            // resolving the same delta.
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
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=45",
                "--cpu-used=4",
                "--aq-mode=0",
                "--deltaq-mode=2",
                "--delta-lf-mode=1",
                "--kf-max-dist=1000",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=1",
                "--enable-global-motion=0",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=1",
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
                "--enable-cdef=1",
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
                        !msg.contains("delta_q") && !msg.contains("delta_lf"),
                        "{NAME} refused on delta_q/delta_lf (seed {seed}) -- that read is ported: {msg}"
                    );
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
            "{NAME}: every attempt refused on other capabilities ({named_refusals} refusals); \
             the gate never exercised delta_q/delta_lf"
        );
        assert!(
            crate::decode::delta_q_hits() > 0,
            "{NAME}: {matched} pixel-exact matches but zero delta_q symbol groups fired -- \
             the reader is unexercised"
        );
        assert!(
            crate::decode::delta_lf_hits() > 0,
            "{NAME}: {matched} pixel-exact matches but zero delta_lf symbol groups fired -- \
             the reader is unexercised"
        );
        eprintln!(
            "{NAME}: {named_refusals} other-capability refusals, {matched} pixel-exact matches \
             out of {n_attempts}, delta_q_hits={}, delta_lf_hits={}",
            crate::decode::delta_q_hits(),
            crate::decode::delta_lf_hits()
        );
    }

    #[test]
    fn a_real_aomenc_stream_with_variance_aq_segmentation_decodes_pixel_exact() {
        run_segmentation_gate(
            "a_real_aomenc_stream_with_variance_aq_segmentation_decodes_pixel_exact",
            "1",
            8,
            16,
            &["--end-usage=q", "--cq-level=45"],
        );
    }

    // lane-seg r1 DEFERRED: an `--aq-mode=2` (complexity AQ) twin of the gate
    // above. libaom only switches segmentation on for complexity AQ once the
    // target bits per 64x64 superblock clear `sb64_target_rate >= 256`
    // (`aq_complexity.c:is_sb_aq_enabled`), which needs `--end-usage=vbr`;
    // every VBR recipe tried (cpu-used 0 and 4, 32x32 and 64x64 fixed
    // partitions) makes aomenc pick partition shapes OTHER lanes still refuse
    // (AB below 16x16, SB-level HORZ/VERT, part32 values 4/6), so 10/10
    // attempts refuse before a pixel is compared. The syntax exercised would
    // be identical to variance AQ's (both features only ever set
    // `SEG_LVL_ALT_Q` via `update_map`), so this is coverage, not capability.
    // Unblocked by the sub-16x16 rect/AB partition lanes.

    /// His two AV1 films are both `yuv420p10le`, so every capability lands
    /// with a 10-bit twin (memory `his-av1-library-is-10-bit`). KEY FRAME
    /// ONLY (`frame_count = 1`): a 10-bit INTER frame does not decode
    /// pixel-exact in this crate yet, segmentation or not -- lane-seg r1 ran
    /// this same recipe with `--aq-mode=0` (segmentation off entirely) and it
    /// still mismatched at frame 2, so the 16-frame 10-bit variant would be
    /// gating an unrelated pre-existing gap. Variance AQ segments the key
    /// frame too (`aq_variance.c`, `is_frame_aq_enabled` includes
    /// `frame_is_intra_only`), so this still proves the 10-bit segment-id
    /// read and the per-segment 10-bit dequant.
    #[test]
    fn a_real_aomenc_10bit_stream_with_aq_segmentation_decodes_pixel_exact() {
        run_segmentation_gate(
            "a_real_aomenc_10bit_stream_with_aq_segmentation_decodes_pixel_exact",
            "1",
            10,
            1,
            &["--end-usage=q", "--cq-level=45"],
        );
    }

    /// lane-tiles r7: the gate stage 2 (r6, commit 8899fc1) shipped without
    /// -- a real >1-tile-column stream with a genuine INTER frame (not just
    /// the key-frame-only two-column gate above), decoded through
    /// `decode_stream` itself. Same recipe as the key-frame gate
    /// (`--sb-size=64`, `--tile-columns=1`, `--loopfilter-control=0`, the
    /// same feature-disable envelope so the gate isolates the tile loop, not
    /// an unrelated unimplemented mode) plus `--limit=2 --kf-max-dist=1000`
    /// so frame 1 is a real inter frame, and `gradients_source`'s default
    /// `speed=0.01` rotation (undisturbed -- not overridden by the tail)
    /// gives it genuine motion against frame 0, so `decode_inter_frame_tile_
    /// with_cdfs`'s new per-tile loop and `MiGrid`'s per-tile bound actually
    /// have to walk a live MV stack across the tile-column boundary, not
    /// just replay skip/zero-mv blocks. Bounded with `duration=0.08` (2
    /// frames at rate=25) per the class in ledger
    /// `refusal-claim-disproved-by-its-own-gate`/`gate-loader-slurps-whole-
    /// file` (never leave an ffmpeg/lavfi generate unbounded).
    #[test]
    fn a_real_aomenc_stream_with_two_tile_columns_and_an_inter_frame_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_stream_with_two_tile_columns_and_an_inter_frame_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (128usize, 64usize);
        let frames = 2usize;
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        for attempt in 0..20u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, width, height, "duration=0.08:rate=25");
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
                    &format!("--limit={frames}"),
                    "--kf-max-dist=1000",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--tile-columns=1",
                    "--tile-rows=0",
                    "--sb-size=64",
                    "--loopfilter-control=0",
                    "--max-reference-frames=3",
                    "--reduced-reference-set=1",
                    "--enable-warped-motion=0",
                    "--enable-obmc=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-dist-wtd-comp=0",
                    "--enable-diff-wtd-comp=0",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-wedge=0",
                    "--enable-smooth-interintra=0",
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
                ]),
                &y4m.stdout,
            );
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;

            // Confirm aomenc actually split into >1 tile column and that
            // frame 1 is a real inter frame before trusting a pixel match --
            // otherwise a match could mean "tiles collapsed to 1" or "frame
            // 1 fell back to key", not "the inter tile loop ran".
            let mut parser = Av1Parser::new();
            let mut pos = 0usize;
            let mut saw_multi_tile_inter = false;
            while pos < stream.len() {
                let obu = parser.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if let ObuKind::Frame(header, _) = obu.kind
                    && header.frame_type != FrameType::Key
                    && header.tile_info.cols > 1
                {
                    saw_multi_tile_inter = true;
                }
            }
            assert!(
                saw_multi_tile_inter,
                "{NAME}: seed {seed} never produced a >1-tile-column inter frame (aomenc \
                 collapsed tiling or coded frame 1 as key)"
            );

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
                hits > frames,
                "{NAME}: tile_hits delta was {hits}, expected > {frames} -- both tiles of both \
                 frames must actually decode (seed {seed})"
            );
            assert_eq!(
                pictures.len(),
                frames,
                "{NAME}: expected {frames} pictures (seed {seed})"
            );

            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frames);
            for (i, (got, want)) in pictures.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    got.y, want.y,
                    "{NAME}: seed {seed} frame {i} luma vs ffmpeg"
                );
                assert_eq!(got.u, want.u, "{NAME}: seed {seed} frame {i} U vs ffmpeg");
                assert_eq!(got.v, want.v, "{NAME}: seed {seed} frame {i} V vs ffmpeg");
            }
            matched += 1;
        }
        assert!(
            matched > 0,
            "{NAME}: every attempt refused ({named_refusals} refusals); decode_stream never \
             decoded a two-tile-column inter frame"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }

    /// lane-tiles r9: `--num-tile-groups` makes aomenc split one frame's
    /// tiles across several `OBU_TILE_GROUP`s instead of folding them into
    /// the single `OBU_FRAME` every other gate in this file relies on --
    /// verified live (`scratch_probe_num_tile_groups`, this round) to emit a
    /// standalone `OBU_FRAME_HEADER` (`show_existing_frame == false`)
    /// followed by two `OBU_TILE_GROUP` OBUs, one tile each. Before this
    /// round `decode_stream` had no arm for `ObuKind::TileGroup` at all (it
    /// fell through the catch-all `continue`, silently dropping the frame),
    /// and its `ObuKind::FrameHeader` arm matched *every* frame header
    /// unconditionally, so a standalone header was misrouted into the
    /// `show_existing_frame` slot lookup and returned the wrong named error
    /// ("a show_existing_frame header naming an empty reference slot") for a
    /// frame that was never a show_existing_frame header. Both are fixed
    /// above: the `FrameHeader` arm now checks `show_existing_frame`, and a
    /// `pending_header`/`pending_tiles` accumulator collects tile groups
    /// until the frame's `cols * rows` tiles have all arrived.
    #[test]
    fn a_real_aomenc_stream_with_several_tile_group_obus_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_several_tile_group_obus_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 128usize);
        let mut matched = 0u32;
        let mut named_refusals = 0u32;
        let mut saw_multiple_tile_group_obus = false;
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
                    "--tile-columns=0",
                    "--tile-rows=1",
                    "--sb-size=64",
                    "--num-tile-groups=4",
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
            let mut tile_group_obus = 0u32;
            while pos < stream.len() {
                let obu = probe.parse_obu(&stream[pos..]).unwrap();
                pos += obu.total_size;
                if matches!(obu.kind, ObuKind::TileGroup(_)) {
                    tile_group_obus += 1;
                }
            }
            if tile_group_obus > 1 {
                saw_multiple_tile_group_obus = true;
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
             decoded a several-tile-group-OBUs stream"
        );
        assert!(
            saw_multiple_tile_group_obus,
            "{NAME}: no attempt actually split into more than one OBU_TILE_GROUP -- this sweep \
             proves nothing about the multi-tile-group case (aomenc's --num-tile-groups is \
             RD/heuristic-driven, same as tile sizing)"
        );
        eprintln!(
            "{NAME}: {matched} pixel-exact matches, {named_refusals} named refusals out of 20"
        );
    }
