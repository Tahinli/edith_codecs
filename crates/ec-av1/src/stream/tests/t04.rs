use super::*;
use super::support::*;
use super::gates_a::*;
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

    /// The 12-bit WARP witness (lane-av112bitw): the same source and recipe
    /// with the warp tools ON -- aomenc picks warp models on this content
    /// (measured pre-lift: the warp model fires at the third frame, and the
    /// decode refused by name at exactly that block) -- now decodes
    /// BYTE-EXACT against ffmpeg. The warp filter's shifts are the bumped
    /// `conv_params->round_0` (`warp::warp_round_0`, 5 at 12 bits, feeding
    /// `reduce_bits_horiz`, the derived non-compound
    /// `reduce_bits_vert = 2*FILTER_BITS - round_0` and both `offset_bits`),
    /// the whole libaom `get_conv_params_no_round` 12-bit bump.
    ///
    /// Non-vacuous: the same recipe refused by name before the lift, the
    /// gate hard-asserts a warp predictor was really resolved (a warpless
    /// stream would pass the pixel compare and prove nothing), and every
    /// plane of every frame compares against ffmpeg's decode.
    #[test]
    fn a_12bit_warped_motion_stream_decodes_pixel_exact() {
        const NAME: &str = "a_12bit_warped_motion_stream_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let y4m = y4m_12bit_subpel_source(6);
        // Exactly the recipe measured to refuse at warp pre-lift: warp ON,
        // global motion NOT spelled (spelling it ON drives a GLOBAL_GLOBALMV
        // compound block into the compound-warp arm -- see the compound gate
        // below), six frames -- the warp model fires at the third frame.
        let stream = encode_12bit(&y4m, 6, &["--enable-warped-motion=1"]);
        assert_12bit_sequence_header(&stream, NAME);
        let (before_selected, before_warp8, before_affine) = (
            decode::warp_selected_hits(),
            decode::warp_hits_8(),
            decode::affine_gm_hits(),
        );
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{NAME}: decode_stream refused a real 12-bit warp stream: {e}")
        });
        assert_eq!(frames.len(), 6);
        let (delta_selected, delta_warp8, delta_affine) = (
            decode::warp_selected_hits() - before_selected,
            decode::warp_hits_8() - before_warp8,
            decode::affine_gm_hits() - before_affine,
        );
        eprintln!(
            "{NAME}: warp_engagement selected(16x16+ symbol)={delta_selected} \
             warped8(usable 8x8 model)={delta_warp8} affine_gm(6-param model)={delta_affine}"
        );
        assert!(
            delta_selected + delta_warp8 + delta_affine > 0,
            "{NAME}: no warp predictor was resolved -- the stream carries no \
             warp model and the pixel compare would prove nothing"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence_12bit(&stream, 160, 128, 6);
        for (i, (ours, reference)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(
                ours.y, reference.y,
                "frame {i} luma vs ffmpeg (12-bit warp)"
            );
            assert_eq!(ours.u, reference.u, "frame {i} U vs ffmpeg (12-bit warp)");
            assert_eq!(ours.v, reference.v, "frame {i} V vs ffmpeg (12-bit warp)");
        }
    }

    /// The 12-bit COMPOUND witness (lane-av112bitc): the parent's source
    /// and recipe with warp pinned OFF -- with six frames LAST diverges
    /// from GOLDEN from frame 2 on and aomenc codes compound inter blocks
    /// (pre-lift measurement: the decode refused at the compound mode
    /// read). Two arms: the default recipe (dist-weighted average
    /// combines live) and `--enable-dist-wtd-comp=0` (pushes compound AND
    /// the masked choice harder: 6 diffwtd blends). Both byte-exact
    /// against ffmpeg; each hard-asserts compound mode symbols and
    /// masked-diffwtd blends really ran, so the parameterised combine
    /// rounds (`INTER_POST_ROUND - delta`, diffwtd `+ (bd-8) - delta` at
    /// 12 bits) and the ffmpeg-pinned pixel path genuinely executed.
    #[test]
    fn a_real_aomenc_12bit_compound_inter_sequence_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_12bit_compound_inter_sequence_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let y4m = y4m_12bit_subpel_source(6);
        for (arm, extra) in [
            ("dist-wtd", &["--enable-warped-motion=0"][..]),
            (
                "plain-masked",
                &["--enable-warped-motion=0", "--enable-dist-wtd-comp=0"][..],
            ),
        ] {
            let stream = encode_12bit(&y4m, 6, extra);
            assert_12bit_sequence_header(&stream, NAME);
            let before_compound = decode::compound_mode_hits();
            let before_masked = decode::masked_compound_hits();
            let before_diffwtd = decode::diffwtd_hits();
            let before_wedge = decode::wedge_hits();
            let frames = decode_stream(&stream).unwrap_or_else(|e| {
                panic!("{NAME} [{arm}]: decode_stream refused a real 12-bit compound stream: {e}")
            });
            assert_eq!(frames.len(), 6, "{NAME} [{arm}]: frame count");
            let (comp, masked, diffwtd, wedge) = (
                decode::compound_mode_hits() - before_compound,
                decode::masked_compound_hits() - before_masked,
                decode::diffwtd_hits() - before_diffwtd,
                decode::wedge_hits() - before_wedge,
            );
            eprintln!(
                "{NAME} [{arm}]: compound blocks={comp} masked={masked} diffwtd={diffwtd} wedge={wedge}"
            );
            assert!(
                comp > 0,
                "{NAME} [{arm}]: no compound mode symbol was read -- the compound path is unexercised"
            );
            assert!(
                masked > 0,
                "{NAME} [{arm}]: no masked compound block ran -- the masked combine is unexercised"
            );
            assert!(
                diffwtd > 0,
                "{NAME} [{arm}]: no diffwtd blend ran -- the diffwtd combine is unexercised"
            );
            let ffmpeg_frames = ffmpeg_decode_sequence_12bit(&stream, 160, 128, 6);
            for (i, (ours, reference)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(
                    ours.y, reference.y,
                    "{NAME} [{arm}] frame {i} luma vs ffmpeg"
                );
                assert_eq!(ours.u, reference.u, "{NAME} [{arm}] frame {i} U vs ffmpeg");
                assert_eq!(ours.v, reference.v, "{NAME} [{arm}] frame {i} V vs ffmpeg");
            }
        }
    }

    /// lane-av112bitg: the 12-bit film grain witness. A real
    /// `aomenc --bit-depth=12 --profile=2 --film-grain-test=5` stream -- grain
    /// params present AND applied (2 luma + 9 cb + 9 cr scaling points,
    /// overlap blending on, clip-to-restricted-range on; the second frame is
    /// an INTER frame that inherits the params via `update_grain = 0`) --
    /// decoded byte-exact against ffmpeg's own 12-bit decode of the identical
    /// bytes (ffmpeg synthesizes grain by default; no grain-disable flag is
    /// passed anywhere in this file). Proves the bit-depth-generic grain path
    /// at 12 bits: `grain_range`'s ±(128 << 4) AR clamp and overlap-blend
    /// clamp, the `gauss_sec_shift = 12 - bd` Gaussian shift, the 4096-entry
    /// expanded scaling-LUT interpolation, and the `<< (bit_depth - 8)`
    /// chroma offsets and legal-range clamps. HARD-asserts the parsed headers
    /// really carry that coverage and `film_grain::grain_hits()` fired, so
    /// neither a params-without-apply stream nor a silently-skipped synthesis
    /// can pass vacuously.
    #[test]
    fn a_real_aomenc_12bit_film_grain_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_12bit_film_grain_stream_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let y4m = y4m_12bit_subpel_source(2);
        // Warp stays refused at 12 bits (lane-av112bitg lifts GRAIN only), so
        // the arm pins both motion tools off exactly once each -- aomenc
        // otherwise picks warped motion on this fixture's sub-pel motion.
        let stream = encode_12bit(
            &y4m,
            2,
            &[
                "--film-grain-test=5",
                "--enable-warped-motion=0",
                "--enable-global-motion=0",
            ],
        );
        assert_12bit_sequence_header(&stream, NAME);
        // The arm is only honest if the stream itself carries the coverage:
        // grain applied on every frame, scaling points on all three planes,
        // overlap blending and restricted-range clipping on.
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        let mut grain_frames = 0usize;
        while pos < stream.len() {
            let obu = probe.parse_obu(&stream[pos..]).unwrap();
            pos += obu.total_size;
            if let ObuKind::Frame(header, _) = obu.kind {
                let fg = &header.film_grain;
                assert!(
                    fg.apply_grain,
                    "{NAME}: frame without apply_grain -- aomenc emitted params it does not apply"
                );
                assert!(
                    fg.num_y_points > 0 && fg.num_cb_points > 0 && fg.num_cr_points > 0,
                    "{NAME}: grain without all three planes' scaling points -- the bit-depth \
                     LUT math would not be exercised"
                );
                assert!(
                    fg.overlap_flag,
                    "{NAME}: overlap off -- the grain_min/grain_max blend clamp is not exercised"
                );
                assert!(
                    fg.clip_to_restricted_range,
                    "{NAME}: clip_to_restricted_range off -- the << (bd - 8) legal-range clamps \
                     are not exercised"
                );
                grain_frames += 1;
            }
        }
        assert_eq!(grain_frames, 2, "{NAME}: expected grain on both frames");
        let before = crate::film_grain::grain_hits();
        let frames = decode_stream(&stream).unwrap_or_else(|e| {
            panic!("{NAME}: decode_stream refused a real 12-bit grain stream: {e}")
        });
        assert_eq!(frames.len(), 2);
        assert!(
            crate::film_grain::grain_hits() > before,
            "{NAME}: matched but zero grain_hits -- grain synthesis never ran"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence_12bit(&stream, 160, 128, 2);
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(
                ours.y, theirs.y,
                "{NAME}: luma vs ffmpeg (12-bit grain, frame {i})"
            );
            assert_eq!(
                ours.u, theirs.u,
                "{NAME}: U vs ffmpeg (12-bit grain, frame {i})"
            );
            assert_eq!(
                ours.v, theirs.v,
                "{NAME}: V vs ffmpeg (12-bit grain, frame {i})"
            );
        }
        eprintln!(
            "{NAME}: byte-exact, grain_hits={}",
            crate::film_grain::grain_hits()
        );
    }

    /// lane-av1screen12: a 12-bit PALETTE screen-content stream, palette on
    /// and palette-UV on, `allow_intrabc` off.
    ///
    /// Recipe: `ffmpeg -f lavfi -i testsrc2=s=160x128:r=25 -pix_fmt
    /// yuv420p12le -strict -1` (2 frames, 160x128 4:2:0) piped to
    /// [`encode_12bit`] with `--tune-content=screen --enable-palette=1
    /// `--enable-intrabc=1`; pin `screen12_palette.obu` (3616 bytes, sha256
    /// `adfe26c67c08a7e986db2743b914ecc3e214c56dd2abbdfd17ebf99f06de64c8`).
    ///
    /// What it replaces: this is the arm the blanket refusal "a 12-bit frame
    /// with screen content tools" blocked while decoding exactly.
    #[test]
    fn a_real_aomenc_12bit_screen_content_palette_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_12bit_screen_content_palette_stream_decodes_pixel_exact";
        const PIN: &str = "screen12_palette.obu";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let y4m = y4m_12bit_screen_source("testsrc2=s=160x128:r=25", false);
        let live = if have_aomenc() {
            encode_12bit(
                &y4m,
                2,
                &[
                    "--tune-content=screen",
                    "--enable-palette=1",
                    "--enable-intrabc=1",
                ],
            )
        } else {
            Vec::new()
        };
        assert_12bit_screen_content_arm(
            NAME,
            PIN,
            live,
            PIN_LEN_PALETTE,
            PIN_FNV_PALETTE,
            2,
            160,
            128,
            false,
        );
    }

    /// lane-av1screen12: a 12-bit screen-content stream whose KEY frame sets
    /// `allow_intrabc = 1` and really codes `use_intrabc` blocks.
    ///
    /// Recipe: [`screen_intrabc_stream_at_depth`] at depth 12 -- the same
    /// shape the 8/10-bit intrabc census and the sb128 intrabc witness use:
    /// `-vf tile=2x2` over `smptebars=size=128x96:rate=25` (256x192),
    /// `--tune-content=screen --enable-intrabc=1 --enable-palette=1
    /// --cq-level=30 --enable-tx-size-search=0 --min-partition-size=8
    /// --max-partition-size=32 --sb-size=64 --kf-max-dist=1 --limit=1`, so
    /// ONE 12-bit key frame; pin `screen12_intrabc.obu` (482 bytes, sha256
    /// `a3b6b94cabdd09a12057c4688990d55495826b87a14332c386d319629070382e`).
    ///
    /// This arm is the one the old refusal's "nor intrabc" half named, and it
    /// is why the refusal could be LIFTED rather than narrowed: it decodes
    /// byte-exact at 12 bits and `intrabc_hits()` proves the blocks really
    /// reconstructed from the current frame.
    #[test]
    fn a_real_aomenc_12bit_intrabc_screen_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_12bit_intrabc_screen_stream_decodes_pixel_exact";
        const PIN: &str = "screen12_intrabc.obu";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let live = if have_aomenc() {
            // `--enable-palette=1` LAST (aomenc keeps the last occurrence), so
            // this 12-bit arm carries BOTH screen-content syntaxes rather than
            // intra-block copy alone.
            screen_intrabc_stream_at_depth(
                "smptebars=size=128x96:rate=25",
                "30",
                "0",
                true,
                false,
                12,
                &["--enable-palette=1"],
            )
        } else {
            Vec::new()
        };
        assert_12bit_screen_content_arm(
            NAME,
            PIN,
            live,
            PIN_LEN_INTRABC,
            PIN_FNV_INTRABC,
            1,
            256,
            192,
            true,
        );
    }

    /// lane-av1screen12: a 12-bit LOSSLESS screen-content stream with
    /// palette and palette-UV on.
    ///
    /// Recipe: the palette arm's source and flags plus `--lossless=1`, so the
    /// frame is `CodedLossless` (base_q_idx 0, WHT on every plane) AND
    /// screen-content -- two 12-bit tile-syntax interactions in one frame;
    /// pin `screen12_lossless_palette.obu` (11698 bytes, sha256
    /// `ec7942c96763593e6d8e4b3fb23c03930bba3bb743102d0cf552fe73ae11c203`).
    #[test]
    fn a_real_aomenc_12bit_lossless_screen_content_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_12bit_lossless_screen_content_stream_decodes_pixel_exact";
        const PIN: &str = "screen12_lossless_palette.obu";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let y4m = y4m_12bit_screen_source("testsrc2=s=160x128:r=25", false);
        let live = if have_aomenc() {
            encode_12bit(
                &y4m,
                2,
                &[
                    "--tune-content=screen",
                    "--enable-palette=1",
                    "--enable-intrabc=1",
                    "--lossless=1",
                ],
            )
        } else {
            Vec::new()
        };
        assert_12bit_screen_content_arm(
            NAME,
            PIN,
            live,
            PIN_LEN_LOSSLESS,
            PIN_FNV_LOSSLESS,
            2,
            160,
            128,
            false,
        );
    }

    #[test]
    fn a_real_aomenc_10bit_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_stream_decodes_pixel_exact";
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
                &gradients_source(42, width, height, "duration=0.04:rate=25"),
                "-pix_fmt",
                "yuv420p10le",
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
            "ffmpeg failed to render the 10-bit fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=30",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--input-bit-depth=10",
                "--bit-depth=10",
                "--limit=1",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m.stdout,
        );
        assert!(
            out.status.success(),
            "aomenc refused the 10-bit fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        // Confirm the sequence header this stream actually parses to before
        // trusting any pixel comparison below.
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        while pos < stream.len() && probe.sequence_header().is_none() {
            let obu = probe.parse_obu(&stream[pos..]).unwrap();
            pos += obu.total_size;
        }
        let seq = probe
            .sequence_header()
            .expect("stream has a sequence header OBU");
        assert_eq!(
            seq.color_config.bit_depth, 10,
            "{NAME}: aomenc did not actually write a 10-bit sequence header"
        );
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused a real 10-bit stream: {e}"),
        };
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "luma vs ffmpeg (10-bit)");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (10-bit)");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (10-bit)");
    }

    /// The 10-bit *inter* counterpart of the gate above: a real `aomenc`
    /// `yuv420p10le` sequence whose non-key frames are plain forward
    /// single-`LAST_FRAME` P frames (same `--lag-in-frames=0
    /// --auto-alt-ref=0 --error-resilient=1` shape as the 8-bit inter gate),
    /// decoded pixel-exact on every frame. 10-bit intra was proven exact by
    /// lane-hbd; inter reconstruction at `bit_depth == 10` was never gated.
    #[test]
    fn a_real_aomenc_10bit_inter_sequence_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_inter_sequence_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (128usize, 64usize, 16usize);
        let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &gradients_source(42, width, height, "duration=0.64:rate=25"),
                "-pix_fmt",
                "yuv420p10le",
                "-strict",
                "-1",
                "-t",
                "0.64",
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
            "ffmpeg failed to render the 10-bit fixture: {}",
            String::from_utf8_lossy(&y4m.stderr)
        );
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                // lane-defon r1: explicit on-value (aomenc keeps the FIRST
                // occurrence, so overrides go before the base list).
                "--loopfilter-control=1",
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=15",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--input-bit-depth=10",
                "--bit-depth=10",
                "--lag-in-frames=0",
                "--auto-alt-ref=0",
                "--kf-max-dist=1000",
                "--error-resilient=1",
                "--aq-mode=0",
                "--deltaq-mode=0",
                "--enable-cdef=0",
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
                "--enable-restoration=1",
                "--min-partition-size=16",
                "--max-partition-size=32",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m.stdout,
        );
        assert!(
            out.status.success(),
            "aomenc refused the 10-bit fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        if let Ok(dump) = std::env::var("EC_AV1_GATE_DUMP") {
            std::fs::write(dump, &stream).expect("dump stream");
        }
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
        let before_deblock = decode::deblock_hits();
        let before_inter = decode::inter_pred_hits();
        let before_lr = crate::restoration::wiener_hits()
            + crate::restoration::sgrproj_hits()
            + crate::restoration::switchable_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused a real 10-bit inter stream: {e}"),
        };
        assert!(
            decode::inter_pred_hits() > before_inter,
            "{NAME}: no inter-predicted block decoded -- the gate proves nothing"
        );
        assert!(
            decode::deblock_hits() > before_deblock,
            "{NAME}: no deblocking edge fired -- raise cq-level"
        );
        // Loop restoration is where the 10-bit inter/intra defect lived
        // (lane-hbdinter r1: the self-guided box sums were never brought back
        // to the 8-bit scale, `restoration.c:660`); a recipe whose RD happened
        // to pick RESTORE_NONE would pass while proving nothing.
        assert!(
            crate::restoration::wiener_hits()
                + crate::restoration::sgrproj_hits()
                + crate::restoration::switchable_hits()
                > before_lr,
            "{NAME}: no loop-restoration unit decoded a real filter -- lower cq-level"
        );
        eprintln!(
            "{NAME}: inter_pred {} deblock {} cdef_idx {} wiener {} sgrproj {}",
            decode::inter_pred_hits() - before_inter,
            decode::deblock_hits() - before_deblock,
            decode::cdef_idx_hits(),
            crate::restoration::wiener_hits(),
            crate::restoration::sgrproj_hits(),
        );
        assert_eq!(frames.len(), frame_count);
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            if got.y != want.y || got.u != want.u || got.v != want.v {
                let pin = std::env::temp_dir().join("ec-av1-10bit-inter-gate-fail.obu");
                crate::dumpio::pin("TEST_PIN", &pin, &stream);
                let first = |a: &Vec<u16>, b: &Vec<u16>| {
                    a.iter()
                        .zip(b)
                        .enumerate()
                        .find(|(_, (x, y))| x != y)
                        .map(|(k, (x, y))| (k, *x, *y))
                };
                panic!(
                    "{NAME}: frame {i} mismatch vs ffmpeg -- Y {:?} U {:?} V {:?}; stream pinned at {}",
                    first(&got.y, &want.y),
                    first(&got.u, &want.u),
                    first(&got.v, &want.v),
                    pin.display()
                );
            }
        }
    }

    /// lane-hbd10 r1: a real `aomenc --bit-depth=10 --film-grain-test=1`
    /// stream, decoded pixel-exact against ffmpeg's own 10-bit decode of the
    /// identical bytes (ffmpeg synthesizes grain by default -- no
    /// grain-disable flag is passed anywhere in this file). Proves the
    /// bit-depth-generic grain path (`film_grain.rs`'s `grain_range`,
    /// `scale_lut`, the `<< (bit_depth - 8)` offsets/legal ranges) rather
    /// than the old `bit_depth != 8` refusal. HARD-asserts both the parsed
    /// `bit_depth == 10` and `film_grain::grain_hits() > 0`, so neither a
    /// silent 8-bit fallback nor a stream aomenc emitted without
    /// `apply_grain` can make it pass vacuously.
    #[test]
    fn a_real_aomenc_10bit_film_grain_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_film_grain_stream_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 64usize);
        let stream = encode_10bit_gradients(
            NAME,
            width,
            height,
            1,
            &["--film-grain-test=1", "--limit=1"],
        );
        assert_eq!(
            parsed_bit_depth(&stream),
            10,
            "{NAME}: aomenc did not actually write a 10-bit sequence header"
        );
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        let mut saw_grain = false;
        while pos < stream.len() {
            let obu = probe.parse_obu(&stream[pos..]).unwrap();
            pos += obu.total_size;
            if let ObuKind::Frame(header, _) = obu.kind
                && header.film_grain.apply_grain
            {
                // A grain block whose scaling LUT is empty would blend a
                // zero delta into every pixel and let this gate pass without
                // the bit-depth math mattering at all.
                assert!(
                    header.film_grain.num_y_points > 0,
                    "{NAME}: apply_grain with no luma scaling points -- grain would be a no-op"
                );
                saw_grain = true;
            }
        }
        assert!(saw_grain, "{NAME}: aomenc did not turn on apply_grain");
        let before = crate::film_grain::grain_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        assert!(
            crate::film_grain::grain_hits() > before,
            "{NAME}: matched but zero grain_hits -- grain synthesis never ran"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg");
        eprintln!(
            "{NAME}: pixel-exact, grain_hits={}",
            crate::film_grain::grain_hits()
        );
    }

    /// lane-t900 r29: `fixtures/superres_kf_cdef_lr_64x64.obu` is a real
    /// aomenc key frame (64x64 upscaled from a coded 43x64,
    /// `--superres-mode=1 --superres-denominator=12 --cpu-used=0 --sb-size=64
    /// --enable-cdef=1 --enable-restoration=1 --cq-level=32`) -- the first
    /// stream in this repo that runs BOTH in-loop filters under superres.
    ///
    /// The defect it pins: spec 7.16's upscale runs between CDEF and loop
    /// restoration (libaom `superres_post_decode`, `decodeframe.c`, called
    /// between `av1_cdef_frame` and `av1_loop_restoration_filter_frame`),
    /// but this decoder ran the whole filter chain on the DOWNSCALED frame
    /// and upscaled last, in `stream.rs`. Loop restoration therefore filtered
    /// downscaled pixels with a unit grid sized from the downscaled width
    /// (1873 wrong samples, max 7, on this very stream). Class
    /// `wrong-stage-width`: every plane width in the filter chain is either
    /// the downscaled one (deblock, CDEF, and the mi grid all three walk) or
    /// the upscaled one (loop restoration's `av1_get_upsampled_plane_size`
    /// unit grid, `av1_loop_restoration_corners_in_sb`'s `mi_to_num_x`
    /// superres arm, and the pixel filters themselves); the sweep is every
    /// caller of `apply_deblock`/`apply_cdef`/`apply_loop_restoration` plus
    /// `RestorationGrid::new` and `read_lr`.
    #[test]
    fn a_superres_key_frame_with_cdef_and_loop_restoration_decodes_pixel_exact() {
        const NAME: &str =
            "a_superres_key_frame_with_cdef_and_loop_restoration_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/superres_kf_cdef_lr_64x64.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (64usize, 64usize);
        let before_lr = crate::restoration::wiener_hits() + crate::restoration::sgrproj_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        assert_eq!(frames.len(), 1, "{NAME}: one key frame");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        // class `gate-blind-to-feature`: the stream must actually upscale and
        // actually run a real restoration filter, or this gate proves nothing.
        assert!(
            crate::superres::superres_hits() > 0,
            "{NAME}: zero superres_hits -- the frame was never upscaled"
        );
        assert!(
            crate::restoration::wiener_hits() + crate::restoration::sgrproj_hits() > before_lr,
            "{NAME}: zero restoration units fired -- LR never ran under superres"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg");
    }

    /// lane-av1superhbd (1): 4:4:4 superres at **10 bits**, `--superres-mode=1
    /// --superres-denominator=12` on all four frames -- coded 171x128 upscaled
    /// to 256x128 at `denom = 12`.
    ///
    /// **What it closes.** Every superres fixture the crate carried parsed
    /// `bit_depth = 8` (4:2:0 `superres_kf_cdef_lr_64x64.obu`) or was 4:2:0
    /// (`superres_alltools_sb128_320x180.obu`), and the 4:4:4 superres cells
    /// were 8-bit. The chroma-format sweep's not-measured list kept "4:4:4
    /// superres at 10/12 bits" open on that basis. This arm is the 10-bit
    /// measurement: a 4:4:4 chroma plane upscaled at FULL resolution, where
    /// the upscaler and the scaled inter predictor both run with a
    /// 10-bit sample width and the 4:4:4 chroma gather has no subsampling
    /// halving to hide behind.
    ///
    /// **The recipe** (sha256
    /// `95b49d7aa81843399ee3aa64bdadfec8f4037bcc3f861923c9b2da3bc868b223`,
    /// 15803 bytes, 4 frames, `testsrc2 256x128 rate=25` yuv444p10le):
    /// ```text
    /// ffmpeg -f lavfi -i "testsrc2=size=256x128:rate=25" -frames:v 4 \
    ///        -pix_fmt yuv444p10le -strict -1 -f yuv4mpegpipe - | \
    /// aomenc --codec=av1 --profile=1 --bit-depth=10 --input-bit-depth=10 \
    ///        --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 --threads=1 \
    ///        --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 --limit=4 \
    ///        --superres-mode=1 --superres-denominator=12 \
    ///        --superres-kf-denominator=12 --obu -o - -
    /// ```
    /// `-strict -1` is load-bearing: without it ffmpeg refuses to write
    /// `yuv444p10le` into a y4m pipe at all (zero bytes out).
    #[test]
    fn a_444_lossy_superres_10bit_d12_stream_decodes_pixel_exact() {
        a_444_hbd_superres_arm(
            "a_444_lossy_superres_10bit_d12_stream_decodes_pixel_exact",
            "fixtures/444_lossy_superres_256x128_d12_10bit.obu",
            15803,
            0x9fad_3227_6e91_792c,
            "95b49d7aa81843399ee3aa64bdadfec8f4037bcc3f861923c9b2da3bc868b223",
            10,
            (256, 128),
            &[(12, 171); 4],
        );
    }

    /// lane-av1superhbd (2): 4:4:4 superres at **12 bits**,
    /// `--superres-mode=1 --superres-denominator=12`, coded 171x128 upscaled
    /// to 256x128.
    ///
    /// **Why the source is `mandelbrot` and not `testsrc2`.** The 12-bit
    /// frame gate refuses a frame carrying `allow_screen_content_tools = 1`
    /// by name (palette and intrabc have no 12-bit witness). libaom sets
    /// that bit from its own content detection
    /// (`av1_set_screen_content_options` -> `encoder_utils.c:1153`, keyed on
    /// the palette-pixel ratio of the FIRST pass), and `testsrc2` -- flat
    /// colour bars and hard edges -- trips it at 12 bits while the very same
    /// recipe at 10 bits does not. `--enable-palette=0 --enable-intrabc=0`
    /// and `--tune-content=film` do NOT suppress it (measured: all four
    /// still parse `allow_screen_content_tools = true`), because the
    /// detection is a property of the CONTENT, not of those flags. A
    /// photographic source (`mandelbrot`, same geometry and frame count)
    /// parses `allow_screen_content_tools = false` and decodes. This is a
    /// source choice, stated so the next lane does not re-derive it.
    ///
    /// **The recipe** (sha256
    /// `973bf374c0185c4e6301509ce834cef01318e6cdb9488fd258118d7ec89bdd04`,
    /// 11023 bytes, 4 frames, `mandelbrot 256x128 rate=25` yuv444p12le):
    /// ```text
    /// ffmpeg -f lavfi -i "mandelbrot=size=256x128:rate=25" -frames:v 4 \
    ///        -pix_fmt yuv444p12le -strict -1 -f yuv4mpegpipe - | \
    /// aomenc --codec=av1 --profile=1 --bit-depth=12 --input-bit-depth=12 \
    ///        --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 --threads=1 \
    ///        --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 --limit=4 \
    ///        --superres-mode=1 --superres-denominator=12 \
    ///        --superres-kf-denominator=12 --obu -o - -
    /// ```
    /// Byte-reproducible: two independent encodes produced the same 11023
    /// bytes.
    #[test]
    fn a_444_lossy_superres_12bit_d12_stream_decodes_pixel_exact() {
        a_444_hbd_superres_arm(
            "a_444_lossy_superres_12bit_d12_stream_decodes_pixel_exact",
            "fixtures/444_lossy_superres_256x128_d12_12bit.obu",
            11023,
            0xd6cf_bb83_2b42_8e2d,
            "973bf374c0185c4e6301509ce834cef01318e6cdb9488fd258118d7ec89bdd04",
            12,
            (256, 128),
            &[(12, 171); 4],
        );
    }

    /// lane-av1superhbd (3): 4:4:4 superres at **12 bits** at the WIDEST
    /// denominator this geometry allows, `--superres-denominator=9` -- coded
    /// 228x128 upscaled to 256x128, so 28 of the 256 columns are the frame's
    /// right-edge upscale margin, against 12's 85.
    ///
    /// It is a separate gate from the den=12 arm because the margin is
    /// where a round-trip is most likely to read past the coded picture: a
    /// decoder that sized the upscale source from `UpscaledWidth` instead of
    /// `FrameWidth` is exact at den=12 on this content and wrong here. Its
    /// recipe is the 12-bit one above with `--superres-denominator=9
    /// --superres-kf-denominator=9` (sha256
    /// `898c775108495b998cb5ec9b08e194d4656ed0140aa8e7e7236892d468bbad99`,
    /// 13015 bytes).
    #[test]
    fn a_444_lossy_superres_12bit_d9_stream_decodes_pixel_exact() {
        a_444_hbd_superres_arm(
            "a_444_lossy_superres_12bit_d9_stream_decodes_pixel_exact",
            "fixtures/444_lossy_superres_256x128_d9_12bit.obu",
            13015,
            0x4110_ce7b_cd85_901b,
            "898c775108495b998cb5ec9b08e194d4656ed0140aa8e7e7236892d468bbad99",
            12,
            (256, 128),
            &[(9, 228); 4],
        );
    }

    /// lane-av1superhbd (4): 4:4:4 superres at **10 bits** under
    /// `--superres-mode=2` -- the random-denominator arm, where libaom picks
    /// a DIFFERENT denominator for every frame and IGNORES
    /// `--superres-denominator` entirely.
    ///
    /// This is what keeps the high-bit-depth superres cell from silently
    /// degenerating into a single-denominator cell: the per-frame parsed
    /// denominators are 11, 14, 15, 9 -- three of them values no flag on the
    /// command line ever named -- so a gate that read the flag instead of
    /// the header would be wrong on three of four frames. The assertion
    /// below makes that structural rather than incidental, and it is the
    /// assertion that fails if a future libaom starts honouring the flag
    /// under mode 2.
    ///
    /// **The recipe** (sha256
    /// `699398aba58fb53b80e05d3d214eb746a3c91945e01da6b47488513f20b55668`,
    /// 17776 bytes, 4 frames, `testsrc2 256x128 rate=25` yuv444p10le):
    /// ```text
    /// ffmpeg -f lavfi -i "testsrc2=size=256x128:rate=25" -frames:v 4 \
    ///        -pix_fmt yuv444p10le -strict -1 -f yuv4mpegpipe - | \
    /// aomenc --codec=av1 --profile=1 --bit-depth=10 --input-bit-depth=10 \
    ///        --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 --threads=1 \
    ///        --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 --limit=4 \
    ///        --superres-mode=2 --obu -o - -
    /// ```
    #[test]
    fn a_444_lossy_superres_10bit_mode2_random_denom_stream_decodes_pixel_exact() {
        const NAME: &str =
            "a_444_lossy_superres_10bit_mode2_random_denom_stream_decodes_pixel_exact";
        let heads = a_444_hbd_superres_arm(
            NAME,
            "fixtures/444_lossy_superres_mode2_256x128_10bit.obu",
            17776,
            0x164f_4099_7576_7ae4,
            "699398aba58fb53b80e05d3d214eb746a3c91945e01da6b47488513f20b55668",
            10,
            (256, 128),
            &[(11, 186), (14, 146), (15, 137), (9, 228)],
        );
        // Distinct-denominator assert: the cell must stay a MULTI-denominator
        // one. A libaom that collapsed mode 2 onto a single denominator (or
        // stopped scaling) would still decode, and the gate above would still
        // hold only because the literals would have been re-pinned with it --
        // so the property is asserted from the parsed headers directly.
        let denoms: Vec<u8> = heads.iter().map(|h| h.superres_denom).collect();
        let mut distinct = denoms.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(
            distinct.len() >= 3,
            "{NAME}: the stream carries denominators {denoms:?} -- fewer than three distinct \
             values, so this is no longer the per-frame-denominator cell and the pin has to be \
             re-derived from the stream"
        );
        let flagged = [9u8, 12, 16];
        assert!(
            denoms.iter().any(|d| !flagged.contains(d)),
            "{NAME}: every parsed denominator is one of the flag values {flagged:?} -- mode 2 now \
             appears to honour --superres-denominator, and this cell stops being the \
             flag-ignoring one"
        );
    }

    /// lane-av1superhbd (5): 4:4:4 superres at **12 bits** under
    /// `--superres-mode=2` -- the high-bit-depth random-denominator arm, the
    /// 12-bit twin of the arm above (sha256
    /// `18aa8b3124ab10672f22577a8eea81e44141a3d0bb506104aee2f950254a2e9f`,
    /// 14656 bytes, same `mandelbrot` source as the 12-bit fixed-denominator
    /// arms). Its parsed denominators are again 11/14/15/9 at coded widths
    /// 186/146/137/228, so the upscaler is handed FOUR coded-to-upscaled
    /// ratios in one 12-bit decode -- the case a single-denominator gate
    /// cannot reach at any bit depth.
    #[test]
    fn a_444_lossy_superres_12bit_mode2_random_denom_stream_decodes_pixel_exact() {
        const NAME: &str =
            "a_444_lossy_superres_12bit_mode2_random_denom_stream_decodes_pixel_exact";
        let heads = a_444_hbd_superres_arm(
            NAME,
            "fixtures/444_lossy_superres_mode2_256x128_12bit.obu",
            14656,
            0xef38_3fb7_b284_2166,
            "18aa8b3124ab10672f22577a8eea81e44141a3d0bb506104aee2f950254a2e9f",
            12,
            (256, 128),
            &[(11, 186), (14, 146), (15, 137), (9, 228)],
        );
        let denoms: Vec<u8> = heads.iter().map(|h| h.superres_denom).collect();
        let mut distinct = denoms.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(
            distinct.len() >= 3,
            "{NAME}: the stream carries denominators {denoms:?} -- fewer than three distinct \
             values, so this is no longer the per-frame-denominator cell and the pin has to be \
             re-derived from the stream"
        );
        let flagged = [9u8, 12, 16];
        assert!(
            denoms.iter().any(|d| !flagged.contains(d)),
            "{NAME}: every parsed denominator is one of the flag values {flagged:?} -- mode 2 now \
             appears to honour --superres-denominator, and this cell stops being the \
             flag-ignoring one"
        );
    }

    /// lane-wave1 step (ii): the superblock-row wavefront
    /// (`EC_AV1_RECON_THREADS`) reconstructs every output frame byte for
    /// byte as the inline path does, on a hidden-ARF 10-bit stream (no-show
    /// frames, CfL, 128 superblocks) and on the two screen-content streams
    /// whose palette/intrabc blocks READ the current frame's pixels while
    /// parsing -- the sites that make the workers barrier.
    #[test]
    fn a_real_stream_reconstructs_identically_with_one_and_four_recon_threads() {
        const NAME: &str = "a_real_stream_reconstructs_identically_with_one_and_four_recon_threads";
        let _gate_lock = lock_gate_counters();
        for fixture in [
            "reelb_arf_witness.obu",
            "palette_screen_witness.obu",
            "palette_screen_witness_10bit.obu",
            // lane-av1wavefront: the 4:4:4 inter witness. The wavefront's
            // replay-side chroma (`mv_to_q4(.., ss_x(fctx))` in the deferred
            // builds, `exec_intra`'s CfL AC) used to read the worker's fresh
            // `FrameCtx` 4:2:0 DEFAULT instead of the frame's subsampling --
            // invisible on the three 4:2:0 streams above, chroma-only loss on
            // this one (f1 U 17504 / V 3, f2 U 25472 / V 1472 at PREFILT).
            "444_sb128rect_lr_witness.obu",
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(fixture);
            let stream = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
            crate::par::set_recon_threads(1);
            let one = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => panic!("{NAME}: {fixture} refused at 1 recon thread: {e}"),
            };
            crate::par::set_recon_threads(4);
            let four = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    crate::par::set_recon_threads(1);
                    panic!("{NAME}: {fixture} refused at 4 recon threads: {e}")
                }
            };
            crate::par::set_recon_threads(1);
            assert!(!one.is_empty(), "{NAME}: {fixture} decoded nothing");
            assert_eq!(one.len(), four.len(), "{NAME}: {fixture} frame count");
            for (i, (a, b)) in one.iter().zip(four.iter()).enumerate() {
                assert_eq!(
                    a.y, b.y,
                    "{NAME}: {fixture} frame {i} luma differs at 4 recon threads"
                );
                assert_eq!(
                    a.u, b.u,
                    "{NAME}: {fixture} frame {i} U differs at 4 recon threads"
                );
                assert_eq!(
                    a.v, b.v,
                    "{NAME}: {fixture} frame {i} V differs at 4 recon threads"
                );
            }
        }
    }

    /// lane-filt1: `EC_AV1_FILTER_THREADS` splits every in-frame filter stage
    /// -- deblock (row bands for the vertical-edge pass, column bands for the
    /// horizontal one), CDEF (mi row bands), loop restoration (RU row bands)
    /// and film grain (32-pixel block rows) -- across scoped worker threads.
    /// A split is only legal if it is BYTE-EXACT, so this gate decodes two
    /// real aomenc streams at 1 and at 4 filter threads and compares every
    /// output frame's three planes.
    ///
    /// Between them the fixtures fire all four stages, and the firing
    /// counters are asserted rather than assumed (class `gate-blind-to-feature`):
    /// `superres_alltools_sb128_320x180.obu` is the 10-bit CDEF + loop
    /// restoration stream, `grain_cdef_lr_128x128.obu` a 128x128
    /// `--denoise-noise-level=25` stream whose frames carry film grain
    /// parameters (small enough that 4 bands is one block row each, which is
    /// what exercises the grain band's one-row `prelude` replay).
    #[test]
    fn a_real_stream_filters_identically_with_one_and_four_filter_threads() {
        const NAME: &str = "a_real_stream_filters_identically_with_one_and_four_filter_threads";
        let _gate_lock = lock_gate_counters();
        for (fixture, wants_lr, wants_grain) in [
            ("superres_alltools_sb128_320x180.obu", true, false),
            ("grain_cdef_lr_128x128.obu", false, true),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(fixture);
            let stream = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
            let before = (
                crate::decode::deblock_hits(),
                crate::restoration::wiener_hits() + crate::restoration::sgrproj_hits(),
                crate::film_grain::grain_hits(),
            );
            crate::par::set_filter_threads(1);
            let one = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => panic!("{NAME}: {fixture} refused at 1 filter thread: {e}"),
            };
            let after = (
                crate::decode::deblock_hits(),
                crate::restoration::wiener_hits() + crate::restoration::sgrproj_hits(),
                crate::film_grain::grain_hits(),
            );
            crate::par::set_filter_threads(4);
            let four = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    crate::par::set_filter_threads(1);
                    panic!("{NAME}: {fixture} refused at 4 filter threads: {e}")
                }
            };
            crate::par::set_filter_threads(1);
            assert!(
                after.0 > before.0,
                "{NAME}: {fixture} deblocked no edge at all"
            );
            if wants_lr {
                assert!(
                    after.1 > before.1,
                    "{NAME}: {fixture} ran no restoration unit"
                );
            }
            if wants_grain {
                assert!(
                    after.2 > before.2,
                    "{NAME}: {fixture} applied no film grain"
                );
            }
            assert_eq!(one.len(), four.len(), "{NAME}: {fixture} frame count");
            assert!(!one.is_empty(), "{NAME}: {fixture} decoded nothing");
            for (i, (a, b)) in one.iter().zip(four.iter()).enumerate() {
                assert_eq!(
                    a.y, b.y,
                    "{NAME}: {fixture} frame {i} luma differs at 4 filter threads"
                );
                assert_eq!(
                    a.u, b.u,
                    "{NAME}: {fixture} frame {i} U differs at 4 filter threads"
                );
                assert_eq!(
                    a.v, b.v,
                    "{NAME}: {fixture} frame {i} V differs at 4 filter threads"
                );
            }
        }
    }

    /// lane-t900 r30b: `fixtures/superres_alltools_sb128_320x180.obu` is 10
    /// frames of `--cpu-used=4 --sb-size=128 --superres-mode=1
    /// --superres-denominator=12 --cq-level=32` with every tool aomenc turns
    /// on by default (CDEF, loop restoration, OBMC, compound, warp) -- the
    /// first stream here that deblocks CHROMA across a group of 4x4 luma
    /// blocks.
    ///
    /// The defect it pins (fixed at this commit, `decode.rs`'s
    /// `plane_to_mi`/`edge_params`): `set_lpf_parameters`
    /// (`av1_loopfilter.c:245`) indexes the mi grid for a CHROMA edge at
    /// `scale | ((coord << scale) >> MI_SIZE_LOG2)` -- the BOTTOM/RIGHT mi of
    /// the co-located 8x8 luma group, the same cell that donates the group's
    /// chroma prediction. This decoder dropped that `| 1` and read the
    /// group's top-left mi, stepped to the previous cell by one mi instead of
    /// `1 << ss`, and priced `pu_edge` by comparing mi origins rather than
    /// the edge coordinate against the block size in the FILTERED plane's own
    /// pixels (chroma bottoms out at 4). All three are invisible while every
    /// block is 8x8 or larger (all four mi of a group carry the same tx size,
    /// ref frame, skip flag and delta_lf) and wrong the moment cpu-used=4
    /// splits a group into 4x4 blocks: on this stream one U sample of frame 2
    /// took a filter level of 12 where libaom's neighbouring 4x4 block gives
    /// 8, and the error compounded through the reference chain to 280 U + 331
    /// V samples (max 9) by frame 9. Class `context-read-from-one-cell`.
    ///
    /// Post-deblock, this stream is now byte-exact against the instrumented
    /// libaom (`EC_AV1_POSTDEBLOCK_DUMP`) in all 10 frames, and the final
    /// output matches ffmpeg's.
    #[test]
    fn a_superres_all_tools_sb128_stream_decodes_pixel_exact() {
        const NAME: &str = "a_superres_all_tools_sb128_stream_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/superres_alltools_sb128_320x180.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height, count) = (320usize, 180usize, 10usize);
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        assert_eq!(frames.len(), count, "{NAME}: frame count");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, count);
        for (i, (ours, theirs)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(ours.y, theirs.y, "{NAME}: frame {i} luma vs ffmpeg");
            assert_eq!(ours.u, theirs.u, "{NAME}: frame {i} U vs ffmpeg");
            assert_eq!(ours.v, theirs.v, "{NAME}: frame {i} V vs ffmpeg");
        }
    }

    /// lane-golomb r1: THE FILM ITSELF. `crates/ec-av1/fixtures/reelb_head_key_frame.obu`
    /// is the first frame of the user's ReelB AV1 release (3840x1608
    /// yuv420p10le, `ffmpeg -i <mkv> -c copy -frames:v 1 -f obu`, 147 bytes) --
    /// a real 10-bit key frame whose height is NOT a multiple of the 64-pixel
    /// superblock, so its last superblock row (mi_row=400 of 402) carries the
    /// edge partition symbol this lane fixed: libaom's `ec_read_partition_impl`
    /// returns `PARTITION_HORZ` when the gathered `split_or_horz` bit is 0, and
    /// this decoder used to READ that bit and throw it away, forcing SPLIT
    /// (class `parsed-then-discarded`). The desync surfaced two rows later as
    /// "a Golomb tail longer than this decoder reads" -- a refusal naming the
    /// symptom, never the cause.
    #[test]
    fn the_reelb_head_key_frame_decodes_pixel_exact() {
        const NAME: &str = "the_reelb_head_key_frame_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/reelb_head_key_frame.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (3840usize, 1608usize);
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        assert_eq!(frames.len(), 1, "{NAME}: one key frame");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg");
    }

    /// lane-reelckf r2: `crates/ec-av1/fixtures/reelc_kf2700.obu` is a 1920x792
    /// 10-bit AV1 key frame taken out of one of the user's own films
    /// (`ffmpeg -ss 2700 -c:v copy -f obu`, then truncated to its first temporal
    /// unit, 36 kB) -- the ONLY stream in this repo that contains a SKIPPED
    /// `UV_CFL_PRED` block. 30 aomenc recipes (r1's 20 plus r2's chroma-correlated
    /// `geq` sweep, cq 40..63, cpu-used 0..2, min/max-partition 4..64, 8 and
    /// 10 bit) never made the encoder pick one, so the counter can only be
    /// hard-asserted against a real film frame. The instrumented oracle agrees:
    /// `EC_TRACE_MODE=1 aomdec` prints 5 `EC_IMODE_VAL ... uv_mode=13 skip=1`
    /// lines for this frame.
    ///
    /// The defect it pins (fixed at 29cd53f, `decode.rs:7719`/`:10528`/`:10891`):
    /// a skipped intra block must still PREDICT with CfL -- libaom's
    /// `predict_and_reconstruct_intra_block` predicts every plane and guards only
    /// the residual on `skip_txfm`. Before the fix this frame had 499 wrong
    /// chroma samples (U rows 48..79 cols 464..492); after it, zero.
    ///
    /// The ffmpeg reference is `ffmpeg -f obu -i <fixture> -f rawvideo
    /// -pix_fmt yuv420p10le -` (what `ffmpeg_decode_sequence_10bit` runs).
    #[test]
    fn a_real_film_key_frame_with_a_skipped_cfl_block_decodes_pixel_exact() {
        const NAME: &str = "a_real_film_key_frame_with_a_skipped_cfl_block_decodes_pixel_exact";
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/reelc_kf2700.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (1920usize, 792usize);
        let before = crate::decode::reelc_chroma_counters();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let after = crate::decode::reelc_chroma_counters();
        let skip_cfl = after.0 - before.0;
        assert_eq!(frames.len(), 1, "{NAME}: one key frame");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        // Asserted before the ffmpeg compare so an absent ffmpeg cannot make the
        // gate vacuous: the skipped-CfL path is exercised on every run.
        assert!(
            skip_cfl > 0,
            "{NAME}: zero skipped-CfL blocks -- the fixture no longer exercises defect 1"
        );
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME} pixel compare: no ffmpeg (skipped_cfl={skip_cfl})");
            return;
        }
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg");
        assert_eq!(
            frames[0].u, ffmpeg_frames[0].u,
            "{NAME}: U vs ffmpeg ({skip_cfl} skipped-CfL blocks)"
        );
        assert_eq!(
            frames[0].v, ffmpeg_frames[0].v,
            "{NAME}: V vs ffmpeg ({skip_cfl} skipped-CfL blocks)"
        );
        eprintln!("{NAME}: pixel-exact, skipped_cfl={skip_cfl}");
    }

    /// lane-kf1200 r2: the same film's `-ss 300` key frame, pinned because no
    /// aomenc recipe reaches its shape -- a SKIPPED sub-8x8 chroma block whose
    /// `uv_mode` is `UV_CFL_PRED`. (lane-reelckf r1 burned 20 recipes across cq
    /// 40..63 and nine lavfi sources hunting exactly this block and never got
    /// one; ledger dead-end "never produced a SKIPPED 1:4 or sub-8x8 CfL
    /// block".) Before the fix the two sub-8x8 skip arms passed `None` for the
    /// CfL argument, so the block reconstructed as flat DC and 463 bytes of
    /// this frame's chroma were wrong. HARD-asserts `skip_cfl_hits` moved, so
    /// the fixture cannot silently stop exercising the arm.
    #[test]
    fn the_reelb_ss300_key_frame_skipped_cfl_decodes_pixel_exact() {
        const NAME: &str = "the_reelb_ss300_key_frame_skipped_cfl_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/reelb_ss300_key_frame.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (3840usize, 1608usize);
        let before = crate::decode::skip_cfl_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let skipped_cfl = crate::decode::skip_cfl_hits() - before;
        assert!(
            skipped_cfl > 0,
            "{NAME}: zero skipped sub-8x8 CfL blocks -- the fixture no longer \
             exercises the arm this test exists for"
        );
        assert_eq!(frames.len(), 1, "{NAME}: one key frame");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg");
        eprintln!("{NAME}: {skipped_cfl} skipped sub-8x8 CfL chroma blocks, frame pixel-exact");
    }

    /// lane-kf900 r2: the film's key frame at -ss 900, one frame
    /// (`crates/ec-av1/fixtures/reelb_kf900.obu`, 484640 bytes, sha256
    /// `a273ae99328388be14a1ff8fa1c5a6d391a9948159eaef120e1b50e02015eb83`,
    /// extracted twice from independent
    /// `ffmpeg -ss 900 -t 2 -i <2160p AV1 HDR10 mkv> -c:v copy -f obu` runs and
    /// truncated to the first frame; both extracts hashed identically).
    ///
    /// This is the ONLY stream that reaches the r1 defect: a SKIPPED 8x8 intra
    /// leaf whose `tx_depth` symbol resolved to `TX_4X4` still publishes 4 --
    /// not its 8-pixel block side -- to the next block's `get_tx_size_context`
    /// (`set_txfm_ctxs(mbmi->tx_size, bw, bh, skip_txfm && is_inter_block(...))`,
    /// `decodemv.c`; the skip term is 0 for intra). It fires 15 times here and
    /// zero times across 108 measured aomenc recipes, so the counter is
    /// asserted against the film rather than against a synthesised stream.
    ///
    /// LUMA only. U/V still differ (open residue, lane-kf900 r2: 221 U and
    /// 47912 V samples, max |d| 23/2, first at chroma (195, 640) -- chroma
    /// RESIDUAL, the prediction there is bit-exact against aomdec's
    /// `EC_PREDOUT`), so this gate pins what is proven and no more.
    #[test]
    fn a_10bit_key_frame_with_skipped_8x8_intra_leaves_that_split_their_transform_decodes_luma_exact()
     {
        const NAME: &str = "a_10bit_key_frame_with_skipped_8x8_intra_leaves_that_split_their_transform_decodes_luma_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/reelb_kf900.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (3840usize, 1608usize);
        crate::decode::reset_skip_split_tx_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        assert_eq!(frames.len(), 1, "{NAME}: one key frame");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        assert!(
            crate::decode::skip_split_tx_hits() >= 15,
            "{NAME}: skip_split_tx_hits() == {} -- the stream that carries the tx-context \
             defect stopped carrying it, so this gate proves nothing",
            crate::decode::skip_split_tx_hits()
        );
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg");
    }

    /// lane-kf1200 r3: the same film's `-ss 600` key frame (3840x1608, one
    /// 8-px partial bottom superblock row), pinned because no aomenc recipe
    /// reaches its shape -- a SKIPPED 8x8 intra leaf whose `tx_depth` resolved
    /// to TX_4X4. (80 recipes swept this round: 4 frame shapes x 2 sources x
    /// cq 32..63 x cpu-used 0/3 x 8/10-bit, all with
    /// `--enable-tx-size-search=1 --enable-filter-intra=1`; every one came
    /// back with `skip_split_tx_hits == 0`, `lanes/kf1200-r3.report.md`.)
    /// An intra block reads `tx_depth` whether or not it is skipped, so the
    /// leaf must record its resolved 4 -- not its 8x8 side -- in the txfm
    /// context; recording 8 gave the NEXT block's `tx_depth` symbol the wrong
    /// `tx_size_cdf` row (ctx 2 where libaom reads 1) and desynced the
    /// entropy stream at mi (400,46). HARD-asserts `skip_split_tx_hits` moved.
    #[test]
    fn the_reelb_ss600_key_frame_skipped_split_tx_decodes_pixel_exact() {
        const NAME: &str = "the_reelb_ss600_key_frame_skipped_split_tx_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/reelb_ss600_key_frame.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let (width, height) = (3840usize, 1608usize);
        crate::decode::reset_skip_split_tx_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        let skipped_split_tx = crate::decode::skip_split_tx_hits();
        assert!(
            skipped_split_tx > 0,
            "{NAME}: zero skipped 8x8 leaves with a split transform -- the fixture no longer \
             exercises the arm this test exists for"
        );
        assert_eq!(frames.len(), 1, "{NAME}: one key frame");
        assert_eq!((frames[0].width, frames[0].height), (width, height));
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg");
        eprintln!("{NAME}: {skipped_split_tx} skipped 8x8 TX_4X4 leaves, frame pixel-exact");
    }

    /// lane-hbd10 r1: a real `aomenc --bit-depth=10 --superres-mode=1
    /// --superres-denominator=12` key frame, decoded pixel-exact against
    /// ffmpeg's own 10-bit decode. Proves `superres::upscale_row`'s
    /// `clip_pixel_highbd` clamp (`(1 << bit_depth) - 1`) on real 10-bit
    /// planes rather than the old `bit_depth != 8` refusal. HARD-asserts
    /// `bit_depth == 10` and `superres_hits > 0`.
    #[test]
    fn a_real_aomenc_10bit_superres_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_superres_stream_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height) = (64usize, 64usize);
        let stream = encode_10bit_gradients(
            NAME,
            width,
            height,
            1,
            &[
                "--limit=1",
                "--kf-max-dist=0",
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
            ],
        );
        assert_eq!(
            parsed_bit_depth(&stream),
            10,
            "{NAME}: aomenc did not actually write a 10-bit sequence header"
        );
        let before = crate::superres::superres_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        assert!(
            crate::superres::superres_hits() > before,
            "{NAME}: matched but zero superres_hits -- the upscaler never ran"
        );
        let ffmpeg_frames = ffmpeg_decode_sequence_10bit(&stream, width, height, 1);
        assert_eq!(frames[0].y, ffmpeg_frames[0].y, "{NAME}: luma vs ffmpeg");
        assert_eq!(frames[0].u, ffmpeg_frames[0].u, "{NAME}: U vs ffmpeg");
        assert_eq!(frames[0].v, ffmpeg_frames[0].v, "{NAME}: V vs ffmpeg");
        eprintln!(
            "{NAME}: pixel-exact, superres_hits={}",
            crate::superres::superres_hits()
        );
    }

    /// lane-hbdgates r1: the four intra tools that had an 8-bit gate and no
    /// 10-bit one (`filter-intra`, `smooth-intra`, `paeth-intra`,
    /// `intra-edge-filter`) on ONE real 10-bit `aomenc` key frame, each with
    /// its own hard-asserted counter. His two films are yuv420p10le, so an
    /// 8-bit-only gate for these tools proves nothing about the streams that
    /// matter (lane-covbd r1).
    #[test]
    fn a_real_aomenc_10bit_intra_tools_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_intra_tools_stream_decodes_pixel_exact";
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
            1,
            &[
                "--limit=1",
                "--kf-max-dist=0",
                "--enable-filter-intra=1",
                "--enable-smooth-intra=1",
                "--enable-paeth-intra=1",
                "--enable-intra-edge-filter=1",
                "--enable-directional-intra=1",
                "--enable-angle-delta=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=16",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
            ],
            &[42, 7, 11, 23, 31, 57],
            &[
                ("filter_intra_hits", decode::filter_intra_hits),
                ("smooth_pred_hits", crate::intra::smooth_pred_hits),
                ("paeth_pred_hits", crate::intra::paeth_pred_hits),
                (
                    "intra_edge_filter_hits",
                    crate::intra::intra_edge_filter_hits,
                ),
            ],
        );
    }

    /// lane-hbdgates r1: `--enable-restoration=1` at 10 bits. Every earlier
    /// bit-depth gate ran `--enable-restoration=0`, which is exactly why two
    /// high-bit-depth loop-restoration defects survived to 2026-09-01
    /// (SGR box sums unscaled, Wiener clamp at the 8-bit bound). Same recipe
    /// as the 8-bit LR gate, at 10 bits.
    // Written `#[ignore]`d on lane-hbdgates (the branch lacked lane-hbdinter's
    // 10-bit LR fix and failed at seed 42, frame 0, Y index 122: ours 351 vs
    // ffmpeg 350). Un-ignored on the merge into main 2026-09-01, where the
    // SGR box-sum scaling and the Wiener clamp are fixed: 16/16 attempts
    // pixel-exact, lr_hits=28.
    #[test]
    fn a_real_aomenc_10bit_restoration_stream_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_restoration_stream_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // 192x128 = 3x2 superblocks, so `read_lr` is re-entered per SB
        // (the 8-bit LR gate's own trap warning); cq-level=15 because
        // aomenc's RD only picks a non-RESTORE_NONE frame type at low cq.
        ten_bit_tool_gate(
            NAME,
            192,
            128,
            1,
            &[
                "--limit=1",
                "--kf-max-dist=0",
                "--cq-level=15",
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
            ],
            &LR_SEEDS,
            &[("lr_hits", ten_bit_lr_hits)],
        );
    }

    /// lane-reelckf2 r1: loop restoration on frame sizes whose LAST stripe is
    /// partial, with 128-sample superblocks -- the two stripe shapes whose
    /// boundary substitution is special (`filter_restoration_unit`: the first
    /// stripe is 8 rows short and takes its above boundary from the frame's
    /// own top rows, the last one is truncated and takes its below boundary
    /// from the frame's own bottom row) had no gate at all: every earlier LR
    /// size (192x128, 160x96) is a whole multiple of 64 rows and ran at
    /// `--sb-size=64`. 152 = 2*64+24 and 216 = 3*64+24 both leave a 32-row
    /// tail stripe, mirroring the 792-row 10-bit film that made this lane.
    /// `lr_stripe0_hits`/`lr_last_stripe_hits` are hard-asserted per attempt
    /// by [`ten_bit_tool_gate`], so an RD that picked RESTORE_NONE on either
    /// stripe cannot pass.
    #[test]
    fn a_real_aomenc_10bit_partial_bottom_stripe_restoration_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_10bit_partial_bottom_stripe_restoration_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // `--sb-size=128` goes in `extra`, which the helper appends AFTER its
        // own `--sb-size=64` (aomenc keeps the LAST occurrence of a repeated
        // flag, measured 2026-09-02); `--max-partition-size=32` keeps the 128
        // root on SPLIT, the only 128-root partition decoded today.
        for (width, height, cq) in [
            (256usize, 152usize, "15"),
            (384, 216, "15"),
            (256, 152, "25"),
        ] {
            ten_bit_tool_gate(
                &format!("{NAME} {width}x{height} cq{cq}"),
                width,
                height,
                1,
                &[
                    "--limit=1",
                    "--kf-max-dist=0",
                    &format!("--cq-level={cq}"),
                    "--sb-size=128",
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
                ],
                &LR_SEEDS,
                &[
                    ("lr_hits", ten_bit_lr_hits),
                    ("lr_stripe0_hits", crate::restoration::lr_stripe0_hits),
                    (
                        "lr_last_stripe_hits",
                        crate::restoration::lr_last_stripe_hits,
                    ),
                ],
            );
        }
    }

    /// lane-hbdgates r1: `--enable-rect-partitions=1 --enable-ab-partitions=1`
    /// at 10 bits, one intra stream, one counter per tool
    /// (`rect_partition_hits`, `partab_hits`) so a stream that only fired the
    /// rect arms cannot stand in for AB.
    #[test]
    fn a_real_aomenc_10bit_rect_and_ab_partitions_decode_pixel_exact() {
        const NAME: &str = "a_real_aomenc_10bit_rect_and_ab_partitions_decode_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        // The 8-bit free-partition/AB recipe, verbatim, at 10 bits: AB
        // splits are an INTER dispatch here (intra-frame AB is still a named
        // refusal), so the stream needs alt-refs and 24 frames, not one key
        // frame.
        ten_bit_tool_gate(
            NAME,
            64,
            64,
            24,
            &[
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
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
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
            ],
            &LR_SEEDS,
            &[
                ("rect_partition_hits", decode::rect_partition_hits),
                ("partab_hits", decode::partab_hits),
            ],
        );
    }

    /// lane-obmcrec r1: OBMC whose above/left neighbour is a 4-wide/4-tall
    /// 1:4-partition strip -- the shape the three 1080p 10-bit film cuts
    /// refused on ("an OBMC neighbour whose switchable interp filter was never
    /// recorded"). `overlappable_above`/`_left` snap such a neighbour to the
    /// chroma pair's SECOND mi but used to read its interp filter from the
    /// FIRST one, a different block whose filter record is the intra
    /// `[3, 3]` sentinel whenever that half is intra. `obmc_pair_filter_hits`
    /// counts exactly that neighbour shape, so a stream without 1:4 strips
    /// next to an OBMC block cannot stand in for it.
    #[test]
    fn a_real_aomenc_10bit_obmc_over_1to4_strip_neighbours_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_10bit_obmc_over_1to4_strip_neighbours_decodes_pixel_exact";
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
            128,
            128,
            24,
            &[
                "--ec-fixture-noise",
                "--cq-level=45",
                "--kf-max-dist=1000",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--enable-fwd-kf=0",
                "--enable-order-hint=1",
                "--enable-warped-motion=0",
                "--enable-obmc=1",
                "--tune-content=default",
                "--enable-masked-comp=0",
                "--enable-interintra-comp=0",
                "--enable-onesided-comp=0",
                "--enable-interintra-wedge=0",
                "--enable-smooth-interintra=0",
                "--enable-rect-partitions=1",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=1",
                "--enable-filter-intra=0",
                "--enable-smooth-intra=0",
                "--enable-paeth-intra=0",
                "--enable-directional-intra=0",
                "--enable-angle-delta=0",
                "--enable-tx-size-search=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--max-partition-size=32",
                "--min-partition-size=4",
                "--enable-palette=0",
                "--enable-intrabc=0",
                "--enable-cfl-intra=0",
                "--enable-ref-frame-mvs=0",
            ],
            &OBMC_PAIR_SEEDS,
            &[("obmc_pair_filter_hits", decode::obmc_pair_filter_hits)],
        );
    }

    /// A real `aomenc` palette-**UV** fixture (lane-palette2 r2): `testsrc2`
    /// (the multicoloured AV1 test pattern -- flat, few-colour, repetitive
    /// per-region colour *and* chroma, unlike `smptebars=hue=s=0` above,
    /// which flattened chroma to a single value specifically to avoid
    /// exercising this path). Same square-only recipe as the palette-Y gate
    /// (`--enable-rect-partitions=0`/`-ab-`/`-1to4-`, fixed 32x32 partition)
    /// so a UV palette block never lands inside the still-refused HORZ/VERT
    /// screen-content strip. HARD-asserts [`decode::palette_uv_hits`] moved
    /// -- a stream that never reads a real UV palette block would decode (or
    /// not) by construction without proving this milestone at all
    /// ([[gate-blind-to-feature]]).
    #[test]
    fn a_real_aomenc_stream_with_palette_uv_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_palette_uv_decodes_pixel_exact";
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let source = "testsrc2=size=64x64:rate=25";
        fn render(source: &str) -> Vec<u8> {
            crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    source,
                    "-pix_fmt",
                    "yuv420p",
                    "-t",
                    "0.16",
                    "-f",
                    "yuv4mpegpipe",
                    "-",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()), None, |c| c.output())
                .expect("ffmpeg failed to run")
                .stdout
        }
        let y4m_a = render(source);
        let y4m_b = render(source);
        assert_eq!(
            y4m_a, y4m_b,
            "testsrc2 must render byte-identical across two runs"
        );
        let y4m = y4m_a;
        let out = run_with_stdin(
            Command::new(aomenc_path()).args([
                "--codec=av1",
                "--passes=1",
                "--end-usage=q",
                "--cq-level=30",
                "--cpu-used=0",
                "--threads=1",
                "--row-mt=0",
                "--sb-size=64",
                "--tune-content=screen",
                "--enable-palette=1",
                "--enable-intrabc=0",
                "--enable-rect-partitions=0",
                "--enable-ab-partitions=0",
                "--enable-1to4-partitions=0",
                "--min-partition-size=32",
                "--max-partition-size=32",
                "--enable-filter-intra=0",
                "--enable-cdef=0",
                "--enable-restoration=0",
                "--enable-tx-size-search=0",
                "--loopfilter-control=0",
                "--obu",
                "-o",
                "-",
                "-",
            ]),
            &y4m,
        );
        assert!(
            out.status.success(),
            "aomenc refused the fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stream = out.stdout;
        let before = decode::palette_uv_hits();
        let frames = match decode_stream(&stream) {
            Ok(frames) => frames,
            Err(e) => panic!("{NAME}: decode_stream refused: {e}"),
        };
        assert!(
            decode::palette_uv_hits() > before,
            "palette_uv_mode never fired decoding this stream -- gate is vacuous"
        );
        let frame_count = frames.len();
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, frame_count);
        for i in 0..frame_count {
            assert_eq!(frames[i].y, ffmpeg_frames[i].y, "luma vs ffmpeg, frame {i}");
            assert_eq!(frames[i].u, ffmpeg_frames[i].u, "U vs ffmpeg, frame {i}");
            assert_eq!(frames[i].v, ffmpeg_frames[i].v, "V vs ffmpeg, frame {i}");
        }
    }

    /// A real `aomenc` fixture that lands palette syntax on a HORZ/VERT
    /// intra strip (lane-palette2 r5, gating r4's `read_intra_mode_rect`
    /// wiring). `smptebars`/`rgbtestsrc` at plain default settings both hit a
    /// SQUARE palette refusal or an unrelated 32x32 `HORZ_A`/`VERT_A`/etc
    /// partition FIRST on every size/cq combo probed -- `--enable-ab-
    /// partitions=0` measurably does not stop aomenc choosing one at this
    /// screen-content tune (byte-identical output with `=0` vs `=1`, same
    /// aomenc quirk as `[[lane-sbpart-r2]]`'s SB-level AB). Forcing
    /// `--min/max-partition-size=64` sidesteps that whole 32x32-level code
    /// path: the only choices left at the (single, key-frame) superblock are
    /// NONE/SPLIT/HORZ/VERT/AB/1to4 straight from [`decode_block_rect64`],
    /// and a size/cq sweep (measured live this round) reliably lands
    /// PARTITION_HORZ/VERT with a real (non-split-tx) palette block on both
    /// sources. Drives `decode_stream` directly (below any refusal) and
    /// HARD-asserts [`decode::palette_rect_hits`] moved on every attempt that
    /// didn't refuse, before comparing pixels ([[gate-blind-to-feature]]) --
    /// a decode that never lands a rect palette block is not a match.
    #[test]
    fn a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let mut matched = 0u32;
        let mut uncounted_exact = 0u32;
        let mut named_refusals = 0u32;
        let mut attempts = 0u32;
        let sizes = [(64, 128), (128, 64), (128, 128), (128, 192), (192, 192)];
        let cqs = [5u32, 15, 25, 35, 45, 55, 63];
        for src_name in ["smptebars", "rgbtestsrc"] {
            for (width, height) in sizes {
                for cq in cqs {
                    attempts += 1;
                    let source = format!("{src_name}=size={width}x{height}:rate=25");
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
                    let cq_arg = format!("--cq-level={cq}");
                    let out = run_with_stdin(
                        Command::new(aomenc_path()).args([
                            "--codec=av1",
                            "--passes=1",
                            "--end-usage=q",
                            &cq_arg,
                            "--cpu-used=4",
                            "--threads=1",
                            "--row-mt=0",
                            "--sb-size=64",
                            "--enable-palette=1",
                            "--min-partition-size=16",
                            "--max-partition-size=64",
                            "--obu",
                            "-o",
                            "-",
                            "-",
                        ]),
                        &y4m.stdout,
                    );
                    assert!(
                        out.status.success(),
                        "aomenc refused the fixture ({source} cq={cq}): {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                    let stream = out.stdout;
                    let before = crate::decode::palette_rect_hits();
                    let frames = match decode_stream(&stream) {
                        Err(e) => {
                            let msg = e.to_string();
                            assert!(
                                msg.contains("unsupported"),
                                "{NAME} failed outright, not a named refusal \
                                 ({source} cq={cq}): {msg}"
                            );
                            named_refusals += 1;
                            eprintln!("{source} cq={cq}: refusal: {msg}");
                            continue;
                        }
                        Ok(frames) => frames,
                    };
                    // A successful decode is ALWAYS pixel-compared; the hit
                    // counter only decides whether the attempt counts toward
                    // the hit-asserting total (class
                    // [[gate-skips-on-its-own-failure]]: the old `continue`
                    // here let 16/70 decoded attempts past without any pixel
                    // check at all).
                    let counted = crate::decode::palette_rect_hits() != before;
                    if !counted {
                        eprintln!(
                            "{NAME}: {source} cq={cq} decoded without ever landing a rect \
                             palette block (palette_rect_hits unchanged) -- pixel-compared, \
                             not counted toward the hit assertion"
                        );
                    }
                    let frame_count = frames.len();
                    let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
                    if frames
                        .iter()
                        .zip(&ffmpeg_frames)
                        .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v)
                        && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP")
                    {
                        std::fs::write(&path, &stream).expect("writing pinned stream");
                        eprintln!(
                            "EC_AV1_GATE_DUMP: wrote mismatching stream ({source} cq={cq}) to {path}"
                        );
                    }
                    for i in 0..frame_count {
                        assert_eq!(
                            frames[i].y, ffmpeg_frames[i].y,
                            "{NAME}: {source} cq={cq} frame {i} luma vs ffmpeg"
                        );
                        assert_eq!(
                            frames[i].u, ffmpeg_frames[i].u,
                            "{NAME}: {source} cq={cq} frame {i} U vs ffmpeg"
                        );
                        assert_eq!(
                            frames[i].v, ffmpeg_frames[i].v,
                            "{NAME}: {source} cq={cq} frame {i} V vs ffmpeg"
                        );
                    }
                    if counted {
                        matched += 1;
                    } else {
                        uncounted_exact += 1;
                    }
                }
            }
        }
        assert!(
            matched > 0,
            "{NAME}: no attempt out of {attempts} ever landed a matching rect palette \
             block ({named_refusals} named refusals) -- palette_rect_hits never proven \
             pixel-exact"
        );
        eprintln!(
            "{NAME}: {matched}/{attempts} attempts matched pixel-exact, \
             {uncounted_exact} decoded-and-pixel-exact but uncounted, {named_refusals} \
             named refusals, palette_rect_hits={}",
            crate::decode::palette_rect_hits()
        );
    }

    /// The BROAD case (lane-palette2 r6, per its charter): a HORZ/VERT intra
    /// strip in a screen-content-tools frame that does NOT necessarily use a
    /// palette. This is what the refusal r4 removed actually guarded --
    /// [`a_real_aomenc_stream_with_rect_palette_decodes_pixel_exact`] above
    /// gates the rarer "and picks a palette" case, which r5 measured RD
    /// almost never chooses -- 39/70 of that same sweep DID land here (a
    /// clean rect-strip decode with `palette_rect_hits` unchanged), just
    /// uncounted by r5's narrower assertion. Same `--min/max-partition-size=64`
    /// recipe r5 measured live to dodge the 32x32-level AB-partition quirk
    /// ([[lane-sbpart-r2]]), single key frame, size/cq sweep. HARD-asserts
    /// [`decode::rect_screen_content_hits`] moved -- proof this stream
    /// actually ran [`read_intra_mode_rect`]'s screen-content-tools branch --
    /// before comparing pixels.
    #[test]
    fn a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_rect_screen_content_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let mut matched = 0u32;
        // Blocks that reconstructed a palette-Y prediction through a SPLIT
        // luma transform (lane-palette2 r7's lifted refusal): the same
        // screen-content streams exercise it, so this gate proves both.
        let mut split_palette_matched = 0u32;
        let mut uncounted_exact = 0u32;
        let mut named_refusals = 0u32;
        let mut attempts = 0u32;
        let sizes = [(64, 128), (128, 64), (128, 128), (128, 192), (192, 192)];
        let cqs = [5u32, 15, 25, 35, 45, 55, 63];
        for src_name in ["smptebars", "rgbtestsrc"] {
            for (width, height) in sizes {
                for cq in cqs {
                    attempts += 1;
                    let source = format!("{src_name}=size={width}x{height}:rate=25");
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
                    let cq_arg = format!("--cq-level={cq}");
                    let out = run_with_stdin(
                        Command::new(aomenc_path()).args([
                            "--codec=av1",
                            "--passes=1",
                            "--end-usage=q",
                            &cq_arg,
                            "--cpu-used=4",
                            "--threads=1",
                            "--row-mt=0",
                            "--sb-size=64",
                            "--enable-palette=1",
                            "--min-partition-size=16",
                            "--max-partition-size=64",
                            "--obu",
                            "-o",
                            "-",
                            "-",
                        ]),
                        &y4m.stdout,
                    );
                    assert!(
                        out.status.success(),
                        "aomenc refused the fixture ({source} cq={cq}): {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                    let stream = out.stdout;
                    let before = crate::decode::rect_screen_content_hits();
                    let split_before = crate::decode::palette_split_tx_hits();
                    let frames = match decode_stream(&stream) {
                        Err(e) => {
                            let msg = e.to_string();
                            assert!(
                                msg.contains("unsupported"),
                                "{NAME} failed outright, not a named refusal \
                                 ({source} cq={cq}): {msg}"
                            );
                            named_refusals += 1;
                            eprintln!("{source} cq={cq}: refusal: {msg}");
                            continue;
                        }
                        Ok(frames) => frames,
                    };
                    // Same integrity rule as the palette gate above: decoded
                    // means pixel-compared, counter or no counter.
                    let counted = crate::decode::rect_screen_content_hits() != before;
                    if !counted {
                        eprintln!(
                            "{NAME}: {source} cq={cq} decoded without ever running the rect \
                             screen-content path (rect_screen_content_hits unchanged) -- \
                             pixel-compared, not counted toward the hit assertion"
                        );
                    }
                    let frame_count = frames.len();
                    let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
                    if frames
                        .iter()
                        .zip(&ffmpeg_frames)
                        .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v)
                        && let Ok(path) = std::env::var("EC_AV1_GATE_DUMP")
                    {
                        std::fs::write(&path, &stream).expect("writing pinned stream");
                        eprintln!(
                            "EC_AV1_GATE_DUMP: wrote mismatching stream ({source} cq={cq}) to {path}"
                        );
                    }
                    for i in 0..frame_count {
                        assert_eq!(
                            frames[i].y, ffmpeg_frames[i].y,
                            "{NAME}: {source} cq={cq} frame {i} luma vs ffmpeg"
                        );
                        assert_eq!(
                            frames[i].u, ffmpeg_frames[i].u,
                            "{NAME}: {source} cq={cq} frame {i} U vs ffmpeg"
                        );
                        assert_eq!(
                            frames[i].v, ffmpeg_frames[i].v,
                            "{NAME}: {source} cq={cq} frame {i} V vs ffmpeg"
                        );
                    }
                    if counted {
                        matched += 1;
                    } else {
                        uncounted_exact += 1;
                    }
                    if counted && crate::decode::palette_split_tx_hits() > split_before {
                        split_palette_matched += 1;
                    }
                }
            }
        }
        assert!(
            split_palette_matched > 0,
            "{NAME}: no pixel-exact attempt ever reconstructed a palette block with a \
             split luma transform -- the lifted refusal is ungated"
        );
        assert!(
            matched > 0,
            "{NAME}: no attempt out of {attempts} ever landed a matching rect \
             screen-content block ({named_refusals} named refusals) -- \
             rect_screen_content_hits never proven pixel-exact"
        );
        eprintln!(
            "{NAME}: {matched}/{attempts} attempts matched pixel-exact \
             ({split_palette_matched} of them through a split-transform palette block), \
             {uncounted_exact} decoded-and-pixel-exact but uncounted, \
             {named_refusals} named refusals, rect_screen_content_hits={}, \
             palette_split_tx_hits={}",
            crate::decode::rect_screen_content_hits(),
            crate::decode::palette_split_tx_hits()
        );
    }

    /// As [`a_real_aomenc_filter_intra_stream_decodes_pixel_exact`]'s
    /// harness, but with the deblocking filter left *on* (no
    /// `--loopfilter-control=0`) at a crf high enough that libaom actually
    /// picks a nonzero level -- proves `apply_deblock` itself, not just that
    /// this decoder still works when every edge lands on level 0. CDEF
    /// stays off so a pixel mismatch can only come from the loop filter.
    ///
    /// lane-av1skipfix: the attempt-loop shape. The sweep walks cq (which is
    /// what decides whether libaom picks a nonzero deblocking level at all)
    /// and seed, REQUIRES at least one attempt to decode, and hard-asserts
    /// `decode::deblock_hits` on a pixel-compared attempt -- a decode error on
    /// every attempt is a hard failure carrying the refusal texts, never a
    /// printed SKIP (class `gate-skips-on-its-own-failure`). MEASURED
    /// 2026-09-29 on `a21f3680`: cq 55/45/35/63 all decode pixel-exact with
    /// 48/64/64/64 deblock edges, so the old single recipe was one lucky
    /// point of a cq range that needed no fallback.
    #[test]
    fn a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact";
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
        // Two seeds x four quantisers.
        const CQS: [u32; 4] = [55, 45, 35, 63];
        let mut refusals = Vec::new();
        let mut counted_exact = 0u32;
        let mut uncounted_exact = 0u32;
        'attempts: for round in 0..2u32 {
            for cq in CQS {
                let source = gradients_source(42 + round, width, height, "duration=0.04:rate=25");
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
                    "{NAME}: ffmpeg fixture (seed {} cq {cq}): {}",
                    42 + round,
                    String::from_utf8_lossy(&y4m.stderr)
                );
                let cq_arg = format!("--cq-level={cq}");
                let out = run_with_stdin(
                    Command::new(aomenc_path()).args([
                        // lane-defon r1: explicit on-value (aomenc keeps the FIRST
                        // occurrence, so overrides go before the base list).
                        "--loopfilter-control=1",
                        "--codec=av1",
                        "--passes=1",
                        "--end-usage=q",
                        &cq_arg,
                        "--cpu-used=0",
                        "--enable-filter-intra=0",
                        "--enable-smooth-intra=0",
                        "--enable-paeth-intra=0",
                        "--enable-directional-intra=0",
                        "--enable-angle-delta=0",
                        "--enable-tx-size-search=0",
                        "--enable-cdef=0",
                        // Loop restoration is a named refusal (aomenc's RD picks
                        // Sgrproj on this kind of content) -- off in every recipe
                        // whose test targets a different surface.
                        "--enable-restoration=0",
                        "--max-partition-size=32",
                        "--enable-palette=0",
                        "--enable-intrabc=0",
                        "--enable-cfl-intra=0",
                        // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                        // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                        "--enable-ref-frame-mvs=0",
                        "--obu",
                        "-o",
                        "-",
                        "-",
                    ]),
                    &y4m.stdout,
                );
                assert!(
                    out.status.success(),
                    "{NAME}: aomenc refused the seed {} cq {cq} fixture: {}",
                    42 + round,
                    String::from_utf8_lossy(&out.stderr)
                );
                let stream = out.stdout;
                let before = decode::deblock_hits();
                let frames = match decode_stream(&stream) {
                    Ok(frames) => frames,
                    Err(e) => {
                        refusals.push(format!("seed {} cq {cq}: {e}", 42 + round));
                        continue;
                    }
                };
                // A decode that succeeded is ALWAYS pixel-compared; the hit
                // counter only decides whether the attempt counts as firing
                // (class `counter-from-refused-stream`).
                let counted = decode::deblock_hits() != before;
                if counted {
                    counted_exact += 1;
                } else {
                    uncounted_exact += 1;
                }
                let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
                assert_eq!(
                    frames[0].y,
                    ffmpeg_frames[0].y,
                    "luma vs ffmpeg (seed {} cq {cq}, deblock fired: {counted})",
                    42 + round
                );
                assert_eq!(
                    frames[0].u,
                    ffmpeg_frames[0].u,
                    "U vs ffmpeg (seed {} cq {cq}, deblock fired: {counted})",
                    42 + round
                );
                assert_eq!(
                    frames[0].v,
                    ffmpeg_frames[0].v,
                    "V vs ffmpeg (seed {} cq {cq}, deblock fired: {counted})",
                    42 + round
                );
                if counted_exact >= 2 {
                    break 'attempts;
                }
            }
        }
        gate_buckets(NAME, counted_exact, uncounted_exact, refusals.len());
        assert!(
            counted_exact > 0,
            "{NAME}: none of {} attempts decoded a stream with a deblocking edge -- \
             apply_deblock is unproven (class gate-blind-to-feature):\n{}",
            CQS.len() * 2,
            refusals.join("\n")
        );
    }

    /// As [`a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact`],
    /// but WITHOUT `--enable-tx-size-search=0` -- the one flag every other
    /// recipe in this file pins off specifically to keep the stream on
    /// `TxMode::Largest`. Dropped, aomenc's RD is free to pick
    /// `TxMode::Select` and split some blocks' luma transform below their own
    /// side (spec 5.11.16), which is what this test's `tx_depth_hits()`
    /// counter proves actually happened -- a stream that resolves every
    /// `tx_depth` to 0 would pixel-match by construction (identical to
    /// `TxMode::Largest`) without exercising the new split-TU decode path at
    /// all. Same multi-seed retry shape as
    /// [`a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`]:
    /// aomenc's own RD is free (with tx-size search on) to also occasionally
    /// pick `allow_screen_content_tools` on this content, a separate,
    /// already-named refusal -- try several seeds, and the first stream that
    /// both decodes AND actually split a transform must be pixel-exact.
    #[test]
    fn a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut uncounted_exact = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            // A flat gradient never has enough local detail for the RD
            // search to prefer a split transform over the block's own full
            // size -- `mandelbrot`'s fractal boundary gives every attempt
            // (a different `start_scale`) sharp, varied-frequency edges.
            // start_scale 5.0 is the proven first try (decodes pixel-exact
            // AND reads the seven-symbol set-1 tx_type rows on TX8 TUs);
            // later attempts walk down for variety when aomenc's RD flips
            // elsewhere.
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
                5.0 - f64::from(attempt) * 0.06
            );
            let y4m = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &source,
                    // Desaturated: chroma stays flat (constant 128) so
                    // `uv_mode` always resolves to `DC_PRED` and this
                    // decoder's still-unsupported directional-chroma round-2
                    // gap never fires, leaving the luma detail alone to
                    // drive whether `TxMode::Select`'s RD splits a block.
                    "-vf",
                    "hue=s=0",
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
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    // Every non-DC intra tool off: directional chroma and
                    // angle deltas are separate round-2 gaps, and DC-only
                    // residual is exactly what gives the RD search a reason
                    // to split a transform.
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
                    "--enable-cdef=0",
                    // Loop restoration is a named refusal (aomenc's RD picks
                    // Sgrproj on this kind of content) -- off in every recipe
                    // whose test targets a different surface.
                    "--enable-restoration=0",
                    "--max-partition-size=32",
                    // 16x16 blocks are what split into the TX8 TUs whose
                    // set-1 tx_type rows this gate exists to prove; some
                    // scales resolve depth 2 (TX4, a named refusal) and the
                    // attempt loop just moves on.
                    "--min-partition-size=16",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                    // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                    "--enable-ref-frame-mvs=0",
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
            let before = decode::tx_depth_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            let counted = decode::tx_depth_hits() != before;
            if !counted {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed}: decoded and pixel-compared, but no block resolved a split \
                     tx_depth"
                ));
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (seed {seed})");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (seed {seed})");
            if counted {
                gate_buckets(
                    "a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact",
                    1,
                    uncounted_exact,
                    refusals.len() - uncounted_exact as usize,
                );
                return;
            }
        }
        gate_buckets(
            "a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact",
            0,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        eprintln!(
            "SKIP a_real_aomenc_intra_stream_with_tx_select_decodes_pixel_exact: every attempt \
             hit a named refusal or never split a transform:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-txselect's decisive gate: a real aomenc INTER sequence WITHOUT
    /// `--enable-tx-size-search=0`, the flag every other inter recipe in this
    /// file pins off precisely to keep the stream on `TxMode::Largest`.
    /// Dropped, aomenc takes its own default (`TX_MODE_SELECT` -- which is
    /// what both of the user's real films use), and an inter block's
    /// transform size is then the recursive `txfm_split` tree of spec
    /// 5.11.17 rather than one whole-block transform. A stream that decoded
    /// pixel-exact while every `txfm_split` symbol resolved to "no split"
    /// would prove nothing (it is `TxMode::Largest` by another name), so this
    /// gate hard-asserts BOTH counters: at least one `txfm_split` symbol read
    /// AND at least one split actually taken. Exhausting every attempt is a
    /// FAILURE, not a SKIP -- an unfired gate is a vacuous gate.
    #[test]
    fn a_real_aomenc_inter_sequence_with_tx_select_decodes_pixel_exact() {
        tx_select_inter_gate(8);
    }

    /// The 10-bit arm of the gate above (lane-txselect r2): both of the
    /// user's films are `yuv420p10le` and var-tx is exactly the tool they
    /// use, so an 8-bit-only proof would not reach them. Same recipe plus
    /// `-b 10 --input-bit-depth=10`.
    #[test]
    fn a_real_aomenc_inter_sequence_with_tx_select_decodes_pixel_exact_10bit() {
        tx_select_inter_gate(10);
    }

    /// lane-intrarect r1's decisive gate: a real aomenc INTER sequence whose
    /// hard mid-GOP content cut makes aomenc code INTRA blocks inside inter
    /// frames, with rect partitions ON so those intra blocks land on
    /// HORZ/VERT strip shapes. Every such block was the named refusal "an
    /// intra-coded HORZ/VERT strip needs rectangular intra prediction this
    /// decoder does not code yet" until this round; they now decode through
    /// the same `decode_rect_split` machinery the key-frame rect paths use.
    ///
    /// Hard-asserts per-shape counters (`intra_rect_strip_in_inter_hits`:
    /// 0 = 64x32/32x64, 1 = 32x16/16x32, 2 = 16x8/8x16) summed over the
    /// attempts that were actually pixel-compared -- at least TWO of the
    /// three shape classes must fire, so a green run cannot come from one
    /// lucky size. A decode that succeeds is ALWAYS pixel-compared (class
    /// [[gate-skips-on-its-own-failure]]); exhausting every attempt without
    /// two shapes is a FAILURE, never a SKIP.
    #[test]
    fn a_real_aomenc_inter_sequence_with_an_intra_rect_strip_decodes_pixel_exact() {
        intra_rect_in_inter_gate(8);
    }

    /// The 10-bit arm (both of the user's films are `yuv420p10le`).
    #[test]
    fn a_real_aomenc_inter_sequence_with_an_intra_rect_strip_decodes_pixel_exact_10bit() {
        intra_rect_in_inter_gate(10);
    }

    /// lane-intrainter r1's decisive gate: a real aomenc INTER sequence in
    /// which aomenc's RD picks an INTRA block inside an inter frame AND
    /// splits that block's luma transform (`tx_depth >= 1`, the intra depth
    /// symbol -- not the inter var-tx tree). That block codes one intra
    /// prediction plus one coefficient set per transform unit, exactly like a
    /// key frame's split block but with the inter frame's CDF state and the
    /// inter-frame intra mode syntax; before this lane it was a named
    /// refusal. Hard-asserts [`decode::intra_in_inter_split_tx_hits`] per
    /// block size: an attempt that never produced such a block proves
    /// nothing, and exhausting every attempt is a FAILURE, not a SKIP.
    #[test]
    fn a_real_aomenc_inter_sequence_with_a_split_transform_intra_block_decodes_pixel_exact() {
        intra_in_inter_split_tx_gate(8);
    }

    /// The 10-bit arm (both of the user's films are `yuv420p10le`).
    #[test]
    fn a_real_aomenc_10bit_inter_sequence_with_a_split_transform_intra_block_decodes_pixel_exact() {
        intra_in_inter_split_tx_gate(10);
    }

    /// lane-fiinter r1 (class [[symbol-consumption-gap]] +
    /// [[tool-disabled-in-every-gate]]): a real aomenc inter sequence with
    /// `--enable-filter-intra=1` (aomenc's own default -- every inter gate
    /// recipe before this one pinned it OFF, so nothing ever exercised it) in
    /// which the RD picks an INTRA block inside an inter frame and gives it
    /// filter intra. `read_intra_block_mode_info` reads `use_filter_intra`
    /// there exactly as a key frame does; this decoder read no such symbol at
    /// all until this round, so such a stream desynced at that symbol and
    /// decoded silently wrong. Hard-asserts
    /// [`decode::intra_in_inter_filter_intra_hits`]: an attempt that never
    /// produced one proves nothing, and exhausting every attempt is a
    /// FAILURE, not a SKIP.
    #[test]
    fn a_real_aomenc_inter_sequence_with_filter_intra_on_an_intra_block_decodes_pixel_exact() {
        filter_intra_in_inter_gate(8);
    }

    /// The 10-bit arm (both of the user's films are `yuv420p10le`).
    #[test]
    fn a_real_aomenc_10bit_inter_sequence_with_filter_intra_on_an_intra_block_decodes_pixel_exact()
    {
        filter_intra_in_inter_gate(10);
    }

    /// lane-angleinter r1 (class [[refusal-strings-are-claims]] +
    /// [[tool-disabled-in-every-gate]]): a real aomenc inter sequence with
    /// `--enable-angle-delta=1` and `--enable-directional-intra=1` (aomenc's
    /// own defaults; every inter gate before this one pinned BOTH off) in
    /// which the RD gives an INTRA block inside an inter frame a directional
    /// luma mode with a nonzero `angle_delta_y`. Both intra-in-inter arms
    /// read that symbol and then REFUSED it as "this encoder never writes
    /// one" -- the claim is false, `read_intra_angle_info` writes it exactly
    /// as a key frame does. Hard-asserts
    /// [`decode::intra_in_inter_angle_delta_y_hits`]; an attempt that never
    /// produced one proves nothing, and exhausting every attempt is a
    /// FAILURE, not a SKIP.
    #[test]
    fn a_real_aomenc_inter_sequence_with_a_directional_intra_block_with_angle_delta_decodes_pixel_exact()
     {
        angle_delta_in_inter_gate(8, "--min-partition-size=16");
    }

    /// The 10-bit arm (both of the user's films are `yuv420p10le`).
    #[test]
    fn a_real_aomenc_10bit_inter_sequence_with_a_directional_intra_block_with_angle_delta_decodes_pixel_exact()
     {
        angle_delta_in_inter_gate(10, "--min-partition-size=16");
    }

    /// lane-uv8 r1 (classes [[refusal-strings-are-claims]] +
    /// [[tool-disabled-in-every-gate]]): a real aomenc inter sequence with
    /// 8x8 partitions and full chroma intra tooling
    /// (`--enable-cfl-intra=1 --enable-smooth-intra=1 --enable-paeth-intra=1
    /// --enable-directional-intra=1`), in which the RD gives an INTRA 8x8
    /// leaf inside an inter frame a NON-DC `uv_mode`. That leaf
    /// ([`decode::decode_inter_block8`]'s intra arm) read the `uv_mode`
    /// symbol and refused every value but `DC_PRED` -- "this encoder never
    /// writes one" was a claim, and this gate is what contradicts it. Hard-
    /// asserts `decode::intra_in_inter8_uv_hits` (directional, smooth/paeth,
    /// CfL, counted on THIS leaf only); an attempt that never produced one
    /// proves nothing, and exhausting every attempt is a FAILURE, not a SKIP.
    #[test]
    fn a_real_aomenc_inter_sequence_with_a_non_dc_chroma_8x8_intra_leaf_decodes_pixel_exact() {
        uv_mode_in_inter8_gate(8, 1);
    }

    /// The 10-bit arm (both of the user's films are `yuv420p10le`).
    #[test]
    fn a_real_aomenc_10bit_inter_sequence_with_a_non_dc_chroma_8x8_intra_leaf_decodes_pixel_exact()
    {
        uv_mode_in_inter8_gate(10, 1);
    }

    /// The multi-tile arm (class [[new-map-ignores-tile-edge]]): this round
    /// stamps a new `uv_mode` band from the 8x8 leaf, whose availability must
    /// be tile-relative.
    #[test]
    fn a_real_aomenc_two_tile_inter_sequence_with_a_non_dc_chroma_8x8_intra_leaf_decodes_pixel_exact()
     {
        uv_mode_in_inter8_gate(8, 2);
    }

    /// lane-leaf8tx r1: the refusal this round lifts on an INTRA 8x8 leaf
    /// inside an INTER frame -- a nonzero luma `angle_delta_y` on it
    /// (`--enable-angle-delta=1`,
    /// `av1_use_angle_delta(BLOCK_8X8)`). Both counters are per-attempt
    /// deltas on an attempt that decoded to the end and is pixel-compared
    /// (class [[counter-from-refused-stream]]); a decode error or a mismatch
    /// is a FAILURE, never a SKIP.
    #[test]
    fn a_real_aomenc_inter_sequence_with_an_angle_delta_8x8_intra_leaf_decodes_pixel_exact() {
        tx_split_angle_in_inter8_gate(8);
    }

    /// The 10-bit arm (both of the user's films are `yuv420p10le`).
    #[test]
    fn a_real_aomenc_10bit_inter_sequence_with_an_angle_delta_8x8_intra_leaf_decodes_pixel_exact() {
        tx_split_angle_in_inter8_gate(10);
    }

    /// lane-chroma r2 stage 1: a real aomenc key frame with
    /// `--enable-smooth-intra=1` (opposite of every sibling recipe's `=0`)
    /// and `--enable-paeth-intra=0` -- paeth chroma stays off so the still-
    /// refused smooth/paeth `uv_mode` (round-2, unrelated to this gate) never
    /// fires; smooth *luma* neighbours are exactly what this gate wants to
    /// exercise, proving [`decode::smooth_luma_hits`] actually landed a
    /// directional block next to a smooth-predicted one and decoded
    /// pixel-exact (the [`PlaneBuf::reconstruct`] `smooth_neighbor` fix).
    #[test]
    fn a_real_aomenc_stream_with_smooth_luma_neighbour_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_smooth_luma_neighbour_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_smooth_luma_neighbour_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut fired_runs = 0u32;
        let mut uncounted_exact = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
                5.0 - f64::from(attempt) * 0.06
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
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=1",
                    // Smooth/paeth chroma is a still-refused, separate
                    // round-2 gap -- off so no attempt trips it while this
                    // gate is purely about luma's edge-filter fix.
                    "--enable-paeth-intra=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--enable-intra-edge-filter=1",
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
                ]),
                &y4m.stdout,
            );
            assert!(
                out.status.success(),
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let smooth_before = decode::smooth_luma_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            let counted = decode::smooth_luma_hits() != smooth_before;
            if counted {
                fired_runs += 1;
            } else {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed}: decoded and pixel-compared, but no smooth-luma-neighbour block \
                     fired"
                ));
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (seed {seed})");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (seed {seed})");
            if fired_runs >= 4 {
                gate_buckets(
                    "a_real_aomenc_stream_with_smooth_luma_neighbour_decodes_pixel_exact",
                    fired_runs,
                    uncounted_exact,
                    refusals.len() - uncounted_exact as usize,
                );
                return;
            }
        }
        gate_buckets(
            "a_real_aomenc_stream_with_smooth_luma_neighbour_decodes_pixel_exact",
            fired_runs,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        assert!(
            fired_runs > 0,
            "every attempt hit a named refusal or never exercised a smooth luma neighbour:\n{}",
            refusals.join("\n")
        );
    }

    /// The smooth/paeth-chroma gate (lane-chroma r1/r3): a real aomenc key
    /// frame with `--enable-smooth-intra=1 --enable-paeth-intra=1` (the
    /// opposite of the directional-chroma gate below, which turns these OFF
    /// to keep this gap from firing while directional chroma is under test).
    /// Some seeds won't pick smooth/paeth for chroma at all -- retry like
    /// every other real-encoder gate here, but the first attempt that
    /// actually fires [`decode::smooth_uv_hits`] must decode pixel-exact.
    #[test]
    fn a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut fired_runs = 0u32;
        let mut uncounted_exact = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
                5.0 - f64::from(attempt) * 0.06
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
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=1",
                    "--enable-paeth-intra=1",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    "--enable-intra-edge-filter=1",
                    "--max-partition-size=32",
                    // 8x8/leaf8 splits are a separate, already-named refusal.
                    "--min-partition-size=16",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    "--enable-ref-frame-mvs=0",
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
            let smooth_before = decode::smooth_uv_hits();
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            let counted = decode::smooth_uv_hits() != smooth_before;
            if counted {
                fired_runs += 1;
            } else {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed}: decoded and pixel-compared, but no smooth/paeth uv_mode fired"
                ));
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (seed {seed})");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (seed {seed})");
            if fired_runs >= 4 {
                gate_buckets(
                    "a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact",
                    fired_runs,
                    uncounted_exact,
                    refusals.len() - uncounted_exact as usize,
                );
                return;
            }
        }
        gate_buckets(
            "a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact",
            fired_runs,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        assert!(
            fired_runs > 0,
            "every attempt hit a named refusal or never exercised smooth/paeth chroma:\n{}",
            refusals.join("\n")
        );
    }

    /// The directional-chroma/angle-delta gate: a real aomenc key frame with
    /// `--enable-directional-intra`/`--enable-angle-delta` left ON (the
    /// opposite of every sibling recipe's `=0`, which existed specifically to
    /// keep this round-2 gap from firing while a different surface was under
    /// test). A colourful, sharp-edged source gives the RD search a reason to
    /// pick a directional `uv_mode` and/or a nonzero angle delta -- some
    /// seeds still won't, so retry like every other real-encoder gate here,
    /// but the first attempt that actually fires either counter must decode
    /// pixel-exact.
    #[test]
    fn a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let mut refusals = Vec::new();
        let mut fired_runs = 0u32;
        let mut uncounted_exact = 0u32;
        for attempt in 0..40u32 {
            let seed = 42 + attempt;
            let source = format!(
                "mandelbrot=size=64x64:start_scale={}:rate=25",
                5.0 - f64::from(attempt) * 0.06
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
                    // lane-defon r1: explicit on-value (aomenc keeps the FIRST
                    // occurrence, so overrides go before the base list).
                    "--enable-directional-intra=1",
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=45",
                    "--cpu-used=0",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--enable-filter-intra=0",
                    // Smooth/Paeth chroma is a separate, still-refused round-2
                    // gap (an unrelated `Edges::build` no-neighbour bug this
                    // lane does not cover) -- off so the RD search's only
                    // non-DC/CFL chroma choice left is directional.
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-tx-size-search=0",
                    "--enable-cdef=0",
                    "--enable-restoration=0",
                    // The intra edge filter/upsample (spec 7.11.2.7-2.9) is
                    // now implemented (`directional()`) -- left ON (aomenc's
                    // own default) so this gate actually exercises it instead
                    // of the un-filtered fast path a disabled flag would keep
                    // testing.
                    "--enable-intra-edge-filter=1",
                    "--max-partition-size=32",
                    // Same reason as the tx-select gate above: 8x8 and other
                    // sub-16x16 splits are a separate, already-named refusal.
                    "--min-partition-size=16",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                    // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                    "--enable-ref-frame-mvs=0",
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
            let (uv_before, angle_before) =
                (decode::directional_uv_hits(), decode::angle_delta_hits());
            let frames = match decode_stream(&stream) {
                Ok(frames) => frames,
                Err(e) => {
                    refusals.push(format!("seed {seed}: {e}"));
                    continue;
                }
            };
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            let counted = decode::directional_uv_hits() != uv_before
                || decode::angle_delta_hits() != angle_before;
            if counted {
                fired_runs += 1;
            } else {
                uncounted_exact += 1;
                refusals.push(format!(
                    "seed {seed}: decoded and pixel-compared, but neither a directional uv_mode \
                     nor a nonzero angle delta fired"
                ));
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, 1);
            assert_eq!(
                frames[0].y, ffmpeg_frames[0].y,
                "luma vs ffmpeg (seed {seed})"
            );
            assert_eq!(frames[0].u, ffmpeg_frames[0].u, "U vs ffmpeg (seed {seed})");
            assert_eq!(frames[0].v, ffmpeg_frames[0].v, "V vs ffmpeg (seed {seed})");
            if fired_runs >= 4 {
                gate_buckets(
                    "a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact",
                    fired_runs,
                    uncounted_exact,
                    refusals.len() - uncounted_exact as usize,
                );
                return;
            }
        }
        gate_buckets(
            "a_real_aomenc_stream_with_directional_chroma_decodes_pixel_exact",
            fired_runs,
            uncounted_exact,
            refusals.len() - uncounted_exact as usize,
        );
        assert!(
            fired_runs > 0,
            "every attempt hit a named refusal or never exercised directional chroma:\n{}",
            refusals.join("\n")
        );
    }

    #[test]
    fn a_real_aomenc_inter_sequence_with_a_whole_superblock_block_decodes_pixel_exact() {
        inter_sb_none_gate(
            "a_real_aomenc_inter_sequence_with_a_whole_superblock_block_decodes_pixel_exact",
            false,
            "32",
            "64",
            decode::inter_sb_none_hits,
            "whole-superblock inter PARTITION_NONE",
            &[],
            64,
            4,
        );
    }

    #[test]
    fn a_real_aomenc_10bit_inter_sequence_with_a_whole_superblock_block_decodes_pixel_exact() {
        inter_sb_none_gate(
            "a_real_aomenc_10bit_inter_sequence_with_a_whole_superblock_block_decodes_pixel_exact",
            true,
            "32",
            "64",
            decode::inter_sb_none_hits,
            "whole-superblock inter PARTITION_NONE",
            &[],
            64,
            4,
        );
    }

    /// lane-inter8 r1: the same recipe with `--min-partition-size=8`, so
    /// aomenc's RD may split a 16x16 inter block into four 8x8 leaves. The
    /// interior (non-edge-straddling) `PARTITION_SPLIT` at the 16x16 level
    /// was refused by name until this round; the straddle path always ran the
    /// same loop, so only [`decode::inter_sub16_split_hits`] -- which counts
    /// interior splits only -- proves the newly-lifted alphabet value fired.
    // lane-inter8 r2: GREEN. The neighbour side bands are mi-granular, every
    // leaf stamps its own 2x2-mi span (including the compound-return path
    // that used to skip `record_mi` entirely) and the compound
    // group/index contexts read the LEAF's mi cell, not its 16x16's corner.
    /// lane-inter8 r3, from lane-sub8's verifier finding: the mi-granular
    /// inter neighbour bands added in r2 must reset per TILE (above) and per
    /// SB row (left) and guard availability tile-relative, so the same recipe
    /// runs again at 128x64 with two tile columns -- a leaf on the second
    /// tile's first column has no left neighbour and must not read the first
    /// tile's band.
    // Un-ignored 2026-09-04: the SB-level rect-partition refusal (lane-sbpart's arm at 128x64) that blocked this gate is gone and it passes. The tile-relative audit it was written for
    // was done by inspection instead: every reader of the mi bands guards on
    // `tile_row0_mi`/`tile_col0_mi` (no `> 0` guard exists in decode.rs),
    // `start_tile` resets the above mi bands over the tile's own mi column
    // span and `start_row` resets every left mi band.
    #[test]
    fn a_real_aomenc_inter_sequence_with_an_8x8_leaf_split_across_tile_columns_decodes_pixel_exact()
    {
        inter_sb_none_gate(
            "a_real_aomenc_inter_sequence_with_an_8x8_leaf_split_across_tile_columns_decodes_pixel_exact",
            false,
            "8",
            "16",
            decode::inter_sub16_split_hits,
            "interior 16x16 inter PARTITION_SPLIT into 8x8 leaves",
            &["--tile-columns=1"],
            128,
            4,
        );
    }

    /// lane-interp3 r1: the same 8x8-leaf recipe with DUAL FILTER, OBMC and
    /// warped motion on. `read_mb_interp_filter` runs for every inter block
    /// (libaom `decodemv.c` 1575), compound ones included; the compound 8x8
    /// leaf skipped it entirely, so a SWITCHABLE-filter stream lost a symbol
    /// per compound leaf and stamped the `[3, 3]` "no filter" sentinel, which
    /// made a neighbouring OBMC block's `neighbour_filter` PANIC (a crash on
    /// his data, not a named refusal). The three hard asserts prove the flags
    /// arrived: a compound 8x8 leaf that really read the filter, a block whose
    /// two dual-filter directions DIFFER (impossible with dual filter off),
    /// and an 8x8 OBMC blend.
    // lane-interp3 r2: the ENTROPY desync r1 measured is CLOSED -- the full
    // 16-frame stream now parses end to end and every `EC_MODE`/`EC_MODE_VAL`
    // element's msac RANGE matches the instrumented aomdec's line for line
    // (570/570 mode elements, 0 diff). Two twin-drift root causes, both in
    // `decode_inter_block8`'s compound arm: `read_compound_ref_frames` was
    // passed a hard-coded `LAST_FRAME` for every inter neighbour instead of
    // the real per-mi band, and the grid stamp cleared both per-slot
    // `is_global_mv_block` flags.
    // lane-interp3 r3: the luma reconstruction residue r2 left open is CLOSED
    // and this gate is GREEN. Third instance of the same twin-drift class:
    // libaom applies `allow_warp`'s GLOBAL branch to every
    // `is_global_mv_block` reference slot, compound included and independent
    // of `motion_mode` (`av1_init_warp_params` inside
    // `build_inter_predictors_8x8_and_bigger`'s per-`ref` loop) -- the 16x16+
    // compound arm has warped since lane-cwarp r1, the 8x8 leaf's compound arm
    // predicted BOTH taps translationally, so a GLOBAL_GLOBALMV 8x8 compound
    // leaf under a non-TRANSLATION model drifted a few luma levels, growing
    // away from the block origin. It was NOT OBMC: the `EC_OBMC` emitter this
    // round added to `decode.rs` diffs 325/325 lines against the instrumented
    // aomdec's own rung (position, span, neighbour mv/ref/bsize/filter) with
    // zero differences.
    #[test]
    fn a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let before_dual = decode::dual_filter_diff_hits();
        let before_obmc = decode::obmc_hits_8();
        let before_cwarp8 = decode::compound_warp_hits_8();
        inter_sb_none_gate(
            NAME,
            false,
            "8",
            "16",
            decode::compound8_filter_hits,
            "a compound 8x8 leaf reading its own switchable interp_filter",
            &[
                "--enable-dual-filter=1",
                "--enable-obmc=1",
                "--enable-warped-motion=1",
                "--enable-onesided-comp=1",
            ],
            64,
            // Recipe search hook (same shape as EC_OBMC8_ATTEMPTS): a compound
            // block needs several coded references, so the sequence is longer
            // than the other arms' 4 frames.
            std::env::var("EC_INTERP3_FRAMES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(16),
        );
        assert!(
            decode::dual_filter_diff_hits() > before_dual,
            "{NAME}: no block read two DIFFERENT dual-filter directions -- \
             --enable-dual-filter=1 did not arrive or aomenc declined it"
        );
        assert!(
            decode::obmc_hits_8() > before_obmc,
            "{NAME}: no 8x8 OBMC blend fired -- --enable-obmc=1 did not arrive"
        );
        assert!(
            decode::compound_warp_hits_8() > before_cwarp8,
            "{NAME}: no 8x8 COMPOUND leaf was predicted through the per-ref GLOBAL \
             warp -- the r3 root-cause path never fired, so this stream no longer \
             proves it"
        );
    }

    /// lane-av1dualfilter10: the 10-bit arm of the dual-filter witness.
    ///
    /// `NEVER_EXERCISED_10BIT` kept `enable-dual-filter` listed after the
    /// 8-bit entry was retired (lane-av1distwtd r3, gate_coverage.rs), and
    /// the reason was structural, not incidental: the only `=1` witness,
    /// `a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`,
    /// calls `inter_sb_none_gate(NAME, false, ...)` -- and that `false` IS the
    /// `ten_bit` parameter, so it built an 8-bit stream. Its `ten_bit = true`
    /// siblings build 10-bit streams but spell no dual-filter flag. This is
    /// that missing arm.
    ///
    /// What makes it a witness and not a spelling: the header is read back
    /// from the ENCODED stream (`enable_dual_filter` and `bit_depth` both come
    /// from `Av1Parser`, never from the flag or the fixture name), and the
    /// arrival assert is a DELTA on `decode::dual_filter_diff_hits()` taken
    /// around this arm alone. A stream that merely parsed would not move the
    /// counter; the counter moves only when `resolve_interp_filter` reads two
    /// DIFFERENT directions for one block, which is the property the entry
    /// was claiming no real stream carried.
    #[test]
    fn a_real_aomenc_10bit_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_10bit_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact";
        if !have_ffmpeg() || !have_aomenc() {
            eprintln!("SKIP {NAME}: no ffmpeg/aomenc");
            return;
        }
        let before_dual = decode::dual_filter_diff_hits();
        let stream = inter_sb_none_gate(
            NAME,
            // `ten_bit`: this is the whole difference from the 8-bit witness.
            true,
            "8",
            "16",
            decode::compound8_filter_hits,
            "a compound 8x8 leaf reading its own switchable interp_filter",
            &[
                "--enable-dual-filter=1",
                "--enable-obmc=1",
                "--enable-warped-motion=1",
                "--enable-onesided-comp=1",
            ],
            64,
            std::env::var("EC_INTERP3_FRAMES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(16),
        );
        // The ARRIVAL assert comes first on purpose. `dual_filter_diff_hits()`
        // moves only inside `resolve_interp_filter` when the header bit is set
        // AND the block's two directions differ, so pinning it =0 reds HERE,
        // on the tool failing to arrive -- not on some shape or pixel assert
        // downstream of it. The two header asserts below corroborate it from
        // the encoded bytes; they are not what carries the proof.
        assert!(
            decode::dual_filter_diff_hits() > before_dual,
            "{NAME}: no block read two DIFFERENT dual-filter directions in the 10-bit stream \
             -- --enable-dual-filter=1 did not arrive or aomenc declined it"
        );
        // Both facts from the PARSED header, not from what we asked for: a
        // gate that trusted `--bit-depth=10` would pass on a stream aomenc
        // silently coded at 8, which is the vacuity this asserts against.
        let mut probe = Av1Parser::new();
        let mut pos = 0usize;
        while pos < stream.len() && probe.sequence_header().is_none() {
            let obu = probe.parse_obu(&stream[pos..]).unwrap();
            pos += obu.total_size;
        }
        let seq = probe
            .sequence_header()
            .expect("stream has a sequence header OBU");
        assert_eq!(
            seq.color_config.bit_depth, 10,
            "{NAME}: aomenc did not write a 10-bit sequence header"
        );
        assert!(
            seq.enable_dual_filter,
            "{NAME}: the parsed sequence header says enable_dual_filter = false, so this \
             stream cannot carry per-direction interp filters however the pixels compare"
        );
    }

    #[test]
    fn a_real_aomenc_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact() {
        inter_sb_none_gate(
            "a_real_aomenc_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact",
            false,
            "8",
            "16",
            decode::inter_sub16_split_hits,
            "interior 16x16 inter PARTITION_SPLIT into 8x8 leaves",
            &[],
            64,
            4,
        );
    }

    #[test]
    // Un-ignored 2026-09-04: the desync behind the bogus 32x32 value=9 refusal (class refusal-from-own-desync) is gone and this 10-bit twin decodes pixel-exact.
    fn a_real_aomenc_10bit_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact() {
        inter_sb_none_gate(
            "a_real_aomenc_10bit_inter_sequence_with_an_8x8_leaf_split_decodes_pixel_exact",
            true,
            "8",
            "16",
            decode::inter_sub16_split_hits,
            "interior 16x16 inter PARTITION_SPLIT into 8x8 leaves",
            &[],
            64,
            4,
        );
    }

    #[test]
    fn a_real_aomenc_inter_sequence_with_a_class1_chroma_eob_decodes_pixel_exact() {
        chroma_class1_eob_gate(
            "a_real_aomenc_inter_sequence_with_a_class1_chroma_eob_decodes_pixel_exact",
            false,
        );
    }

    #[test]
    fn a_real_aomenc_10bit_inter_sequence_with_a_class1_chroma_eob_decodes_pixel_exact() {
        chroma_class1_eob_gate(
            "a_real_aomenc_10bit_inter_sequence_with_a_class1_chroma_eob_decodes_pixel_exact",
            true,
        );
    }

    /// lane-inter4 r1: the SUPERBLOCK-level inter `PARTITION_HORZ`/
    /// `PARTITION_VERT` pair -- two 64x32 / 32x64 inter strips, the arm the
    /// inter tile path refused outright before this lane ("an inter SB-level
    /// partition type other than NONE or SPLIT"). `--min-partition-size=32`
    /// keeps every 32x32-level rect shape (already coded, and residual-capable
    /// only when skip) out of the picture so the counters can only be earned
    /// at the superblock level; the noiseless split-motion source (one half
    /// translating, the other static, flat inside each half) is what makes
    /// aomenc's RD pick a superblock HORZ/VERT whose strips are `skip` --
    /// the ONLY inter rect shape this decoder can complete, since rectangular
    /// inter residual coding is not ported (see `reject_residual`).
    ///
    /// Every decoded frame is compared to ffmpeg (Y, U and V); an attempt
    /// that refuses by name is counted, never turned into a SKIP; and an
    /// attempt that decoded without firing the arm is counted separately and
    /// still required to be pixel-exact (class gate-skips-on-its-own-failure).
    #[test]
    fn a_real_aomenc_inter_sequence_with_a_superblock_level_rect_partition_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_inter_sequence_with_a_superblock_level_rect_partition_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (192usize, 128usize, 6usize);
        // lane-inter4 r1: 8-bit ONLY, and the reason is the lane's own open
        // residue, not a tolerance: across all 16 attempts x both depths, every
        // 10-bit attempt that reached a superblock-level rect strip found it
        // NON-skip, so it stops at "a non-skip rectangular (HORZ/VERT/HORZ_B)
        // strip needs rectangular residual coding". The 10-bit twin below is
        // `#[ignore]`d with that exact reason rather than deleted or weakened.
        // lane-r14 r2 re-measured the 8-bit-only note above: the residual
        // refusal it names is GONE (this round codes a 64-px transform axis as
        // its truncated low-32 corner), and a 10-bit sweep of this exact
        // recipe now reaches the arm -- 2 pixel-exact attempts, 2 64x32 and 2
        // 32x64 strips, 2 of them carrying a coded 64-axis residual.
        // lane-sbrect10 r1 closed the newly exposed 10-bit defect r14 r2
        // parked this arm on (10-bit attempt 14, cq 61, horizontal split,
        // motion step 12 -- it mismatched ffmpeg while decoding ZERO rect
        // strips): a 64x64 INTRA block inside an inter frame read `uv_mode`
        // off the 14-symbol CFL-allowed alphabet, which libaom's
        // `is_cfl_allowed` caps at 32x32 -- same DC_PRED value, wrong
        // interval narrowing, tile desynced from that block on. Both depths
        // run now.
        for bit_depth in [8u32, 10u32] {
            let (mut named_refusals, mut matched, mut out_of_scope, mut oos_mismatch) =
                (0u32, 0u32, 0u32, 0u32);
            let (mut horz_proved, mut vert_proved) = (0usize, 0usize);
            let mut rect64_tus = 0usize;
            for attempt in 0..16u32 {
                // Two orientations (the transposed pair a one-sided gate could
                // never separate) x four quantisers x two motion steps: which
                // superblock the RD hands a skip-able rect strip moves with
                // the quantiser, and the 10-bit arm needs a different corner
                // of the grid from the 8-bit one.
                let vertical_split = attempt % 2 == 1;
                let cq = [58, 52, 45, 61][(attempt / 2 % 4) as usize];
                let sp = 8 + (attempt / 8) * 4;
                let geq = if vertical_split {
                    format!("if(lt(Y,64), 40+mod(floor((X+N*{sp})/32)*90,200), 200)")
                } else {
                    format!("if(lt(X,64), 40+mod(floor((Y+N*{sp})/32)*90,200), 200)")
                };
                let duration = frame_count as f64 / 25.0;
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='{geq}',format=yuv420p"
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
                let cq_arg = format!("--cq-level={cq}");
                let depth_arg = format!("--bit-depth={bit_depth}");
                let input_depth_arg = format!("--input-bit-depth={bit_depth}");
                let args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_arg,
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    &depth_arg,
                    &input_depth_arg,
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--deltaq-mode=0",
                    "--enable-filter-intra=0",
                    "--enable-cfl-intra=0",
                    "--enable-intrabc=0",
                    "--enable-tx-size-search=0",
                    "--lag-in-frames=0",
                    // Per-arm overrides go LAST: aomenc keeps the last
                    // occurrence of a repeated --enable-* flag.
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=1",
                    "--min-partition-size=32",
                    "--max-partition-size=64",
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
                let before = crate::stream::inter_rect_counters();
                let rect64_before = crate::stream::rect64_inter_tu_hits();
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit, \
                             attempt {attempt}): {msg}"
                        );
                        named_refusals += 1;
                        eprintln!("{bit_depth}-bit attempt {attempt} refusal: {msg}");
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let after = crate::stream::inter_rect_counters();
                let (horz, vert) = (after.2 - before.2, after.3 - before.3);
                let rect64 = crate::stream::rect64_inter_tu_hits() - rect64_before;
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
                        oos_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit attempt {attempt} MISMATCHES with zero \
                             superblock-level inter rect strips -- another shape's defect"
                        );
                    }
                    continue;
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg (attempt {attempt}, {bit_depth}-bit)"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg (attempt {attempt}, {bit_depth}-bit)"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg (attempt {attempt}, {bit_depth}-bit)"
                    );
                }
                horz_proved += horz;
                vert_proved += vert;
                rect64_tus += rect64;
                matched += 1;
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {matched} pixel-exact \
                 attempts carrying the arm, 64x32 strips={horz_proved}, 32x64 strips={vert_proved}, \
                 64-axis residual TUs={rect64_tus}, \
                 {out_of_scope} attempts carried none ({oos_mismatch} of them mismatched)"
            );
            assert_eq!(
                oos_mismatch, 0,
                "{NAME}: {oos_mismatch} attempt(s) with no superblock-level inter rect strip \
                 decoded but mismatched ffmpeg -- a defect of another shape, not a pass"
            );
            // lane-r14 r2: the residual half of the arm -- a strip that is not
            // `skip` and codes a real 64-axis transform unit. Without this the
            // gate proves only the partition symbol.
            assert!(
                rect64_tus > 0,
                "{NAME} ({bit_depth}-bit): no superblock-level rect strip carried a coded \
                 64-axis residual transform unit inside a pixel-exact attempt \
                 (horz={horz_proved}, vert={vert_proved}) -- the residual arm proved nothing"
            );
            assert!(
                horz_proved + vert_proved > 0,
                "{NAME} ({bit_depth}-bit): the superblock-level inter rect arm never fired \
                 inside a pixel-exact attempt (horz={horz_proved}, vert={vert_proved}, \
                 {named_refusals} refusals, {out_of_scope} out of scope) -- gate proved nothing"
            );
        }
    }

    /// lane-r14 r3: a 1:4 inter strip with a 64-px axis (64x16 / 16x64) whose
    /// transform SPLITS -- the arm r2 left refused by name ("a split transform
    /// on a 1:4 inter strip with a 64-px axis"). libaom's
    /// `sub_tx_size_map[TX_64X16] == TX_32X16` (and `[TX_16X64] == TX_16X32`)
    /// keeps the sub-transform RECTANGULAR, so the var-tx tree's leaves are
    /// 32x16/16x32 units read through [`TxbSet::LumaRect32x16Inter`], each with
    /// its own `get_txb_ctx_general` neighbour context (the unit is smaller
    /// than its block, so `txb_skip_ctx` is NOT the lone-TU 0).
    ///
    /// `--enable-1to4-partitions=1 --min-partition-size=16
    /// --max-partition-size=64 --enable-tx64=1` (each spelled once, last
    /// occurrence wins) is what lets aomenc pick a superblock-level HORZ_4 /
    /// VERT_4; the textured translating source is what keeps the strips
    /// non-skip so the residual is actually coded. 8-bit AND 10-bit, every
    /// decoded frame compared to ffmpeg on Y/U/V, refusals counted (never
    /// SKIPped), attempts that carried no rectangular var-tx leaf counted
    /// separately and still required pixel-exact.
    #[test]
    fn real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx_leaves_fire_before_a_named_refusal()
     {
        const NAME: &str = "real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx_leaves_fire_before_a_named_refusal";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let frame_count = 6usize;
        for bit_depth in [8u32, 10u32] {
            let (mut named_refusals, mut compared, mut out_of_scope, mut oos_mismatch) =
                (0u32, 0u32, 0u32, 0u32);
            let (mut leaf_32x16, mut leaf_16x32) = (0usize, 0usize);
            // Leaves decoded on ANY attempt, including one whose stream hits a
            // later named refusal: the firing half of this gate, kept apart
            // from `leaf_32x16`/`leaf_16x32` (pixel-compared attempts only) so
            // a refused stream is never counted as a pixel proof
            // (class counter-from-refused-stream).
            let (mut fired_32x16, mut fired_16x32) = (0usize, 0usize);
            // lane-gaterecipe r1: the firing arms (16-17) are 8-BIT ONLY, and
            // this is measured, not a convenience. Their 10-bit twin fires the
            // leaves too (32x16=0 16x32=46 on attempt 16) and then MISMATCHES
            // ffmpeg at decode-order frame 2 of attempt 17 -- an open DECODE
            // defect handed off in lanes/gaterecipe-r1.report.md, never
            // silenced inside this loop.
            let attempts = if bit_depth == 8 { 18u32 } else { 16u32 };
            for attempt in 0..attempts {
                // Two orientations (the transposed pair a one-sided gate could
                // never separate) x four quantisers x two motion steps.
                // lane-r14 r4 recipe hunt (~70 aomenc runs on the merged
                // tree: testsrc2 / life / mandelbrot / banded motion, 64x64 up
                // to 384x256, cq 20..58): attempts 16-17 carry the only recipe
                // found that makes aomenc code a 64-axis 1:4 strip whose
                // transform SPLITS -- 384x256, per-16-column band motion,
                // cq 32 (16x32 leaves) and cq 38 (32x16 leaves).
                let firing = attempt >= 16;
                let (width, height) = if firing {
                    (384usize, 256usize)
                } else {
                    (192usize, 128usize)
                };
                let vertical = attempt % 2 == 1;
                // lane-vartxsplit r2, MEASURED against the oracle
                // (`EC_VARTX` histogram, ~40 aomenc runs): at cq 10 this
                // source splits NO rectangular transform at all -- aomdec
                // reads only TX_8X8/TX_16X16/TX_8X16/TX_16X8 split symbols,
                // so the leaves gaterecipe r1 counted here came out of a
                // stream our decoder was already desynced on (class
                // counter-from-refused-stream). cq 44 and 46 are the two
                // quantisers of that sweep whose stream really carries
                // `TX_32X8`/`TX_8X32` splits (16x8 and 8x16 rectangular
                // var-tx leaves) AND decodes pixel-exact on all six frames.
                let cq = if firing {
                    if attempt == 16 { 44 } else { 46 }
                } else {
                    [30, 36, 44, 52][(attempt / 2 % 4) as usize]
                };
                let sp = 3 + (attempt / 8) * 3;
                // 16-pixel bands, each band translating at its OWN speed
                // across the other axis: that is what makes aomenc's RD split
                // a 64x64 superblock into four 1:4 strips (`PARTITION_HORZ_4`
                // / `VERT_4`) with distinct motion vectors, and the texture
                // inside a band is what keeps the strip non-skip and its
                // transform split.
                let geq = if firing && !vertical {
                    // lane-gaterecipe r1, MEASURED: the banded translating
                    // source this arm used until now splits NO transform at
                    // all -- an aomdec EC_TRACE/EC_TRACE_COEFF histogram of its
                    // stream reads zero 1:4 partitions and zero rectangular
                    // sub-transforms, so the leaves the old assert counted came
                    // out of a stream that stopped at a refusal (class
                    // counter-from-refused-stream). What DOES split a
                    // rectangular transform is a high-frequency product texture
                    // at a fine quantiser with `--min-partition-size=8`: part
                    // of the block predicts well and part does not, which is
                    // the RD case for `txfm_partition`.
                    "128+90*sin((X+N*3)/6)*sin(Y/2)+50*sin((X*Y)/37)".to_string()
                } else if firing {
                    // lane-vartxsplit r2: BOTH leaf orientations come out of
                    // this one source (three 16x8 leaves and one 8x16 on
                    // attempt 16), so the firing pair is the same texture at
                    // two quantisers. Its transposed twin was measured over
                    // cq 40..56 and splits no rectangular transform at all;
                    // at cq 56 it also mismatches ffmpeg from decode-order
                    // frame 2 with ZERO rect var-tx leaves in it -- an open
                    // reconstruction defect (8-pixel-wide luma column, see
                    // lanes/vartxsplit-r2.report.md), not something to hide
                    // inside this gate's attempt list.
                    "128+90*sin((X+N*3)/6)*sin(Y/2)+50*sin((X*Y)/37)".to_string()
                } else if vertical {
                    format!("128+60*sin((Y+N*({sp}+9*mod(floor(X/16),2)))/7)+25*sin(X/11)")
                } else {
                    format!("128+60*sin((X+N*({sp}+9*mod(floor(Y/16),2)))/7)+25*sin(Y/11)")
                };
                let duration = frame_count as f64 / 25.0;
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='{geq}',format=yuv420p"
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
                let cq_arg = format!("--cq-level={cq}");
                let depth_arg = format!("--bit-depth={bit_depth}");
                let input_depth_arg = format!("--input-bit-depth={bit_depth}");
                // A rectangular var-tx LEAF needs a block whose max rectangular
                // transform can split: `--min-partition-size=8` is what lets
                // aomenc's RD reach one (measured); at 16 it never did.
                let min_part = if firing {
                    "--min-partition-size=8"
                } else {
                    "--min-partition-size=16"
                };
                let args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_arg,
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    &depth_arg,
                    &input_depth_arg,
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--deltaq-mode=0",
                    "--enable-filter-intra=0",
                    "--enable-cfl-intra=0",
                    "--enable-intrabc=0",
                    "--lag-in-frames=0",
                    // Per-arm overrides go LAST: aomenc keeps the last
                    // occurrence of a repeated --enable-* flag.
                    "--enable-tx64=1",
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=1",
                    "--enable-dual-filter=0",
                    "--enable-obmc=0",
                    "--enable-tx-size-search=1",
                    min_part,
                    "--max-partition-size=64",
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
                let before = crate::stream::vartx_rect_leaf_hits();
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit, \
                             attempt {attempt}): {msg}"
                        );
                        let after = crate::stream::vartx_rect_leaf_hits();
                        fired_32x16 += after[0] - before[0];
                        fired_16x32 += after[1] - before[1];
                        named_refusals += 1;
                        eprintln!(
                            "{bit_depth}-bit attempt {attempt} refusal (rect var-tx leaves \
                             decoded before it: 32x16={}, 16x32={}): {msg}",
                            after[0] - before[0],
                            after[1] - before[1]
                        );
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let after = crate::stream::vartx_rect_leaf_hits();
                let (h32, v32) = (after[0] - before[0], after[1] - before[1]);
                fired_32x16 += h32;
                fired_16x32 += v32;
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
                if h32 + v32 == 0 {
                    // Counted per ATTEMPT, on a decoded+compared attempt only
                    // (class counter-from-refused-stream).
                    out_of_scope += 1;
                    if mismatched {
                        oos_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit attempt {attempt} MISMATCHES with zero \
                             rectangular var-tx leaves -- another shape's defect"
                        );
                    }
                    continue;
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg (attempt {attempt}, {bit_depth}-bit)"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg (attempt {attempt}, {bit_depth}-bit)"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg (attempt {attempt}, {bit_depth}-bit)"
                    );
                }
                leaf_32x16 += h32;
                leaf_16x32 += v32;
                compared += 1;
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {compared} \
                 pixel-exact attempts carrying the arm, pixel-compared rect var-tx leaves \
                 32x16={leaf_32x16} 16x32={leaf_16x32}, leaves decoded anywhere in the sweep \
                 32x16={fired_32x16} 16x32={fired_16x32}, {out_of_scope} attempts carried none \
                 ({oos_mismatch} of them mismatched)"
            );
            assert_eq!(
                oos_mismatch, 0,
                "{NAME}: {oos_mismatch} attempt(s) with no rectangular var-tx leaf decoded \
                 but mismatched ffmpeg -- a defect of another shape, not a pass"
            );
            assert!(
                compared + out_of_scope > 0,
                "{NAME} ({bit_depth}-bit): every attempt refused -- the gate compared no pixels"
            );
            // The firing half. lane-vartxsplit r2 re-measured it against
            // the oracle: attempts 16/17 (384x256 product texture, cq 44/46,
            // `--min-partition-size=8 --enable-tx-size-search=1`) are the only
            // recipes of a ~40-run sweep whose stream carries `TX_32X8` /
            // `TX_8X32` split symbols, i.e. RECTANGULAR var-tx leaves (16x8
            // and 8x16, both orientations out of the same stream). They now
            // decode PIXEL-EXACT on all six frames, so the assert is on the
            // pixel-compared tally, not on the "decoded anywhere" one --
            // `fired_*` stays only to separate a refused stream's leaves from
            // a proved one (class counter-from-refused-stream). At 10 bits
            // the same sweep either fires nothing or mismatches on a
            // reconstruction defect of another shape (report), so the firing
            // assert stays 8-bit -- and it is a hard assert, never a print.
            if bit_depth == 8 {
                assert!(
                    leaf_32x16 > 0 && leaf_16x32 > 0,
                    "{NAME}: the rectangular var-tx leaf arm never fired on a pixel-exact \
                     attempt (pixel-compared 32x16={leaf_32x16}, 16x32={leaf_16x32}, decoded \
                     anywhere 32x16={fired_32x16} 16x32={fired_16x32}, {named_refusals} \
                     refusals, {compared} compared) -- the rectangular sub-transform path is \
                     unproved by this gate"
                );
            }
        }
    }

    /// lane-rectres r1: a 32x32-level 1:4 INTER strip (32x8 / 8x32) that codes
    /// a REAL rectangular residual. THIS is the shape both 10-bit 3840x1608
    /// film cuts and every measured 1080p offset stopped on -- six of six
    /// `decode_probe` runs refused at "a non-skip rectangular (HORZ/VERT/
    /// HORZ_B) strip needs rectangular residual coding" with `w=32 h=8` or
    /// `w=8 h=32`, nothing else.
    ///
    /// `max_txsize_rect_lookup[BLOCK_32X8] == TX_32X8` codes all 256 of its
    /// positions ([`TxbSet::LumaRect32x8Inter`]: `get_txsize_entropy_ctx` =
    /// TX_16X16 so the coefficient tables are the luma-16 ones,
    /// `txsize_log2_minus4` = 4 so `eob_pt` is the 256-position alphabet), and
    /// unlike the INTRA twin it DOES code a `tx_type`:
    /// `av1_get_ext_tx_set_type` at `tx_size_sqr_up == TX_32X32` is
    /// `EXT_TX_SET_DCT_IDTX` for inter with or without `reduced_tx_set`, read
    /// at the `txsize_sqr_map[TX_32X8] == TX_8X8` CDF row (class
    /// table-indexed-by-raw-size: the coefficient tables follow the ADJUSTED
    /// size, the `tx_type` row the SQUARE one -- they disagree here).
    /// Its chroma pair is one 16x4 / 4x16 unit (`ss_size_lookup[BLOCK_32X8]`
    /// = BLOCK_16X4, `Chroma8`'s tables, 64-position `eob_pt`), and a split
    /// transform gives 16x8 / 8x16 leaves (`sub_tx_size_map[TX_32X8] ==
    /// TX_16X8`) the rect var-tx path already reads.
    ///
    /// `--enable-1to4-partitions=1 --min-partition-size=8
    /// --max-partition-size=32 --enable-tx-size-search=1` (each spelled once,
    /// last occurrence wins -- aomenc keeps the LAST `--enable-*`) is what
    /// lets aomenc pick a 32x32-level HORZ_4 / VERT_4; the 8-pixel banded
    /// translating source is what makes those strips the RD winner and keeps
    /// them non-skip so the residual is really coded. Both orientations, 8-bit
    /// AND 10-bit, every decode-order frame compared to ffmpeg on Y/U/V,
    /// refusals counted (never SKIPped), attempts that carried no 32x8/8x32
    /// residual unit counted separately and still required pixel-exact.
    #[test]
    fn a_real_aomenc_inter_sequence_with_32x32_level_1to4_strips_codes_their_rect_residual() {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_32x32_level_1to4_strips_codes_their_rect_residual";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let frame_count = 6usize;
        let (mut proved_32x8, mut proved_8x32) = (0usize, 0usize);
        let (mut fired_32x8, mut fired_8x32) = (0usize, 0usize);
        for bit_depth in [8u32, 10u32] {
            let (mut named_refusals, mut compared, mut out_of_scope, mut oos_mismatch) =
                (0u32, 0u32, 0u32, 0u32);
            for attempt in 0..16u32 {
                // Two orientations (the transposed pair a one-sided gate could
                // never separate) x four quantisers x two motion steps.
                let vertical = attempt % 2 == 1;
                let (width, height) = (192usize, 128usize);
                let cq = [48, 52, 56, 60][(attempt / 2 % 4) as usize];
                let sp = 3 + (attempt / 8) * 4;
                // 32-pixel bands, each translating at its OWN speed across the
                // other axis: that is what makes aomenc's RD split a 32x32
                // block into four 32x8 (or 8x32) strips with distinct motion
                // vectors, and the texture inside a band is what keeps a strip
                // non-skip. MEASURED (72-run recipe sweep, 192x128..256x192,
                // cq 24..60, bands 8/16/32/64): only 32-pixel bands at cq >= 48
                // make aomenc code a NON-SKIP one -- attempt 0 (cq 48,
                // horizontal source) codes an 8x32 residual unit, attempt 7
                // (cq 60, the transposed source) five 32x8 ones. Narrower
                // bands give the same strips with `skip` set, which proves the
                // partition symbol and nothing about the residual.
                let geq = if vertical {
                    format!("128+62*sin((Y+N*({sp}+7*mod(floor(X/32),4)))/6)+28*sin(X/9)")
                } else {
                    format!("128+62*sin((X+N*({sp}+7*mod(floor(Y/32),4)))/6)+28*sin(Y/9)")
                };
                let duration = frame_count as f64 / 25.0;
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='{geq}',format=yuv420p"
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
                let cq_arg = format!("--cq-level={cq}");
                let depth_arg = format!("--bit-depth={bit_depth}");
                let input_depth_arg = format!("--input-bit-depth={bit_depth}");
                let args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_arg,
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    &depth_arg,
                    &input_depth_arg,
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--deltaq-mode=0",
                    "--enable-filter-intra=0",
                    "--enable-cfl-intra=0",
                    "--enable-intrabc=0",
                    "--lag-in-frames=0",
                    // Per-arm overrides go LAST: aomenc keeps the last
                    // occurrence of a repeated --enable-* flag.
                    "--enable-tx64=0",
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=1",
                    "--enable-tx-size-search=1",
                    "--enable-dual-filter=0",
                    "--enable-obmc=0",
                    // aomenc's min-partition-size bounds the SMALLER side of a
                    // 1:4 strip, so 32x8 needs 8 (memory
                    // aomenc-min-partition-bounds-smaller-side).
                    "--min-partition-size=8",
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
                let before = crate::stream::rect32x8_inter_tu_hits();
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit, \
                             attempt {attempt}): {msg}"
                        );
                        let after = crate::stream::rect32x8_inter_tu_hits();
                        // Firing counted on a refused stream is kept apart from
                        // the pixel-proved tally (class
                        // counter-from-refused-stream).
                        fired_32x8 += after[0] - before[0];
                        fired_8x32 += after[1] - before[1];
                        named_refusals += 1;
                        eprintln!(
                            "{NAME} {bit_depth}-bit attempt {attempt} refusal (32x8 residual \
                             units decoded before it: {}, 8x32: {}): {msg}",
                            after[0] - before[0],
                            after[1] - before[1]
                        );
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let after = crate::stream::rect32x8_inter_tu_hits();
                let (h, v) = (after[0] - before[0], after[1] - before[1]);
                fired_32x8 += h;
                fired_8x32 += v;
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
                if h + v == 0 {
                    out_of_scope += 1;
                    if mismatched {
                        oos_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit attempt {attempt} MISMATCHES with zero \
                             32x8/8x32 inter residual units -- another shape's defect"
                        );
                    }
                    continue;
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg (attempt {attempt}, {bit_depth}-bit)"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg (attempt {attempt}, {bit_depth}-bit)"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg (attempt {attempt}, {bit_depth}-bit)"
                    );
                }
                proved_32x8 += h;
                proved_8x32 += v;
                compared += 1;
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {compared} \
                 pixel-exact attempts carrying the arm, {out_of_scope} attempts carried none \
                 ({oos_mismatch} of them mismatched)"
            );
            assert_eq!(
                oos_mismatch, 0,
                "{NAME}: {oos_mismatch} attempt(s) with no 32x8/8x32 inter residual unit \
                 decoded but mismatched ffmpeg -- a defect of another shape, not a pass"
            );
            assert!(
                compared + out_of_scope > 0,
                "{NAME} ({bit_depth}-bit): every attempt refused -- the gate compared no pixels"
            );
        }
        eprintln!(
            "{NAME}: pixel-compared 32x32-level 1:4 inter residual units 32x8={proved_32x8} \
             8x32={proved_8x32} (decoded anywhere in the sweep 32x8={fired_32x8} \
             8x32={fired_8x32})"
        );
        // The firing half, one assert per lifted shape. Both orientations must
        // be PIXEL-PROVED: a transposed table passes one and fails the other
        // (class reference-layout-not-spec).
        assert!(
            proved_32x8 > 0,
            "{NAME}: no 32x8 inter residual transform unit was decoded inside a pixel-exact \
             attempt (decoded anywhere: {fired_32x8}) -- the lift is unproved"
        );
        assert!(
            proved_8x32 > 0,
            "{NAME}: no 8x32 inter residual transform unit was decoded inside a pixel-exact \
             attempt (decoded anywhere: {fired_8x32}) -- the lift is unproved"
        );
    }

    /// lane-sbrect10 r1: a 64x64 INTRA block inside an INTER frame, whose
    /// `uv_mode` alphabet libaom's `is_cfl_allowed` (`blockd.h`: `wide <= 32
    /// && high <= 32`) narrows to the 13-symbol NO-CFL table. Reading the
    /// 14-symbol CFL one produced the same `DC_PRED` value with the wrong
    /// interval narrowing, so the tile desynced from that block on and the
    /// frame decoded silently wrong (class `wrong-alphabet-same-value`; found
    /// as the "another shape" mismatch the superblock rect gate above had to
    /// park its 10-bit arm on).
    ///
    /// The counter `nocfl_uv_mode_hits` proves the shape fired in EVERY
    /// compared attempt, and every decode-order frame is compared against
    /// ffmpeg -- an attempt that mismatches fails, it never SKIPs.
    ///
    /// lane-sbrect10 r2: with the half-random source below this gate also
    /// covers a SECOND defect it exposed at 10-bit cq 58/61 -- the compound
    /// MV stack's extension pass zero-filled the `comp_list` tail slots
    /// libaom fills with each side's GLOBAL MOTION vector
    /// (`combine_compound_candidates`, mvref_common.c:723-729). A compound
    /// block with no usable neighbour pair (frame top row, intra block to
    /// its left, empty temporal field) then took a `(0, 0)` predictor
    /// instead of the frame's translation: 32890/39907 wrong luma samples
    /// on decode-order frame 2 with the entropy stream still in sync.
    #[test]
    fn a_real_aomenc_inter_frame_with_a_64x64_intra_block_reads_the_no_cfl_uv_alphabet() {
        const NAME: &str =
            "a_real_aomenc_inter_frame_with_a_64x64_intra_block_reads_the_no_cfl_uv_alphabet";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (192usize, 128usize, 6usize);
        for bit_depth in [8u32, 10u32] {
            let (mut named_refusals, mut matched, mut carried_none) = (0u32, 0u32, 0u32);
            let mut total_hits = 0usize;
            for attempt in 0..16u32 {
                // The sibling superblock-rect gate's own grid (its 10-bit
                // attempt 14 -- cq 61, horizontal split, motion step 12 -- is
                // the stream that exposed the defect); which superblock the RD
                // hands an intra 64x64 block moves with the quantiser and the
                // depth, so both orientations x four quantisers x two motion
                // steps run here too.
                let vertical_split = attempt % 2 == 1;
                let cq = [58, 52, 45, 61][(attempt / 2 % 4) as usize];
                let sp = 8 + (attempt / 8) * 4;
                // Half ramp, half deterministic pseudo-noise: the flat "200"
                // half of r1's source made aomenc code the right-hand
                // superblocks skip, so at 8 bit no >32 intra block ever
                // appeared (probed cq 45/52/58/61/63 = 0 hits). The textured
                // half forces intra above 32x32 in an inter frame at BOTH
                // depths, so both arms carry the shape.
                let geq = if vertical_split {
                    format!(
                        "if(lt(Y,64), 40+mod(floor((X+N*{sp})/32)*90,200),                          mod((X*7+Y*13+N*97)*31,256))"
                    )
                } else {
                    format!(
                        "if(lt(X,128), 40+mod(floor((Y+N*{sp})/32)*90,200),                          mod((X*7+Y*13+N*97)*31,256))"
                    )
                };
                let duration = frame_count as f64 / 25.0;
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='{geq}',format=yuv420p"
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
                let cq_arg = format!("--cq-level={cq}");
                let depth_arg = format!("--bit-depth={bit_depth}");
                let input_depth_arg = format!("--input-bit-depth={bit_depth}");
                let args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    &cq_arg,
                    "--cpu-used=0",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    &depth_arg,
                    &input_depth_arg,
                    "--enable-restoration=0",
                    "--enable-palette=0",
                    "--deltaq-mode=0",
                    "--enable-filter-intra=0",
                    "--enable-cfl-intra=0",
                    "--enable-intrabc=0",
                    "--enable-tx-size-search=0",
                    "--lag-in-frames=0",
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=1",
                    "--min-partition-size=32",
                    "--max-partition-size=64",
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
                let before = crate::decode::nocfl_uv_mode_hits();
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit, \
                             attempt {attempt}): {msg}"
                        );
                        named_refusals += 1;
                        eprintln!("{bit_depth}-bit attempt {attempt} refusal: {msg}");
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let hits = crate::decode::nocfl_uv_mode_hits() - before;
                let ffmpeg_frames = if bit_depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frame_count)
                };
                assert_eq!(frames.len(), frame_count);
                if hits == 0 {
                    // No 64-axis intra block in this stream: still compared
                    // (a mismatch here is a defect, never a pass), just not
                    // proof of this gate's shape.
                    carried_none += 1;
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg (attempt {attempt}, {bit_depth}-bit, hits={hits})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg (attempt {attempt}, {bit_depth}-bit, hits={hits})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg (attempt {attempt}, {bit_depth}-bit, hits={hits})"
                    );
                }
                if hits > 0 {
                    matched += 1;
                    total_hits += hits;
                }
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {matched} pixel-exact \
                 attempts carrying a >32 intra block ({total_hits} no-CFL uv_mode reads), \
                 {carried_none} attempts carried none"
            );
            assert!(
                matched > 0 && total_hits > 0,
                "{NAME} ({bit_depth}-bit): no attempt decoded an intra block above 32x32, so the \
                 no-CFL uv_mode alphabet was never exercised -- gate proved nothing \
                 ({named_refusals} refusals, {carried_none} attempts carried none)"
            );
        }
    }
