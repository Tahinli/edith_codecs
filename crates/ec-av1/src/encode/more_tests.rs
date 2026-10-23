use super::*;
use super::tests::*;
use crate::intra::{D45_PRED, NON_DIRECTIONAL};
use std::process::Command;


    /// lane-ab128: the 128 root's four AB shapes -- `PARTITION_HORZ_A` (two
    /// 64x64 on top, one 128x64 below), `_HORZ_B`, `_VERT_A`, `_VERT_B`.
    /// Five passes over the same `testsrc2` clip
    /// `a_128_root_horz_and_vert_halves_decode_exact_through_both_decoders`
    /// uses: each shape forced at every root ([`force_b128_ab`]), so every
    /// context and every visit order the shape reads is exercised rather than
    /// the handful the RD would pick, and then an UNFORCED pass whose
    /// per-shape count is REPORTED (it is the number the gate rows are read
    /// against).
    ///
    /// Each pass is sample-exact against the encoder's own reconstruction
    /// through `decode_stream` AND through ffmpeg; a writer/reader mismatch
    /// surfaces here as a refusal or a pixel miss, never in the BD gate. RED
    /// before this lane by construction: `write_sb128_root` could only name
    /// NONE / HORZ / VERT / SPLIT, so all four counts were 0.
    ///
    /// Runs only inside `superblock_size_global_suite`.
    pub(super) fn a_128_root_ab_shapes_decode_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP: no ffmpeg");
            return;
        }
        force_sb128(true);
        let dir = std::env::temp_dir().join(format!("ec-av1-ab128-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("testsrc2-1280x768.y4m");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=1280x768:rate=24"])
            .args(["-frames:v", "12", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("ffmpeg failed to run")
            .success();
        assert!(ok, "ffmpeg could not write the synthetic clip");
        let (width, height) = (1280usize, 768usize);
        let pictures = crate::probe::source(clip.to_str().unwrap(), "0", "null", width, height, 12);
        let names = ["HORZ_A", "HORZ_B", "VERT_A", "VERT_B"];
        let mut unforced = [0usize; 4];
        for force in [4u8, 5, 6, 7, 0] {
            force_b128_ab(force);
            let label = if force == 0 {
                "unforced"
            } else {
                names[usize::from(force) - 4]
            };
            let _ = take_b128_ab_hits();
            let _ = crate::tile::take_sb128_ab_hits();
            let encoded = encode_sequence(&pictures, 150, 0.5).unwrap();
            let won = take_b128_ab_hits();
            let coded = crate::tile::take_sb128_ab_hits();
            eprintln!(
                "{label}: search won {won:?} AB roots, writer coded {coded:?} \
                 (HORZ_A/HORZ_B/VERT_A/VERT_B)"
            );
            if force == 0 {
                // The unforced counts are REPORTED, not asserted: a zero here
                // would be an RD result, not a defect (the four forced passes
                // already prove the writer and the reader agree on the shape).
                unforced = won;
            } else {
                let i = usize::from(force) - 4;
                assert!(
                    coded[i] > 0,
                    "no {label} root was coded with {label} forced"
                );
            }
            let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
            assert_eq!(decoded.len(), encoded.frames.len(), "{label}: frame count");
            for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
                for (plane, got, want) in [
                    ("luma", &dec.y, &frame.reconstruction.y),
                    ("U", &dec.u, &frame.reconstruction.u),
                    ("V", &dec.v, &frame.reconstruction.v),
                ] {
                    let differ = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
                    assert_eq!(
                        differ, 0,
                        "{label} frame {i}: {plane} -- {differ} samples differ"
                    );
                    assert_eq!(
                        got.len(),
                        want.len(),
                        "{label} frame {i}: {plane} is {} samples, the reconstruction's is {}",
                        got.len(),
                        want.len()
                    );
                }
            }
            let ff = ffmpeg_decode_sequence(&encoded.stream, width, height, pictures.len());
            assert_eq!(
                encoded.frames.len(),
                ff.len(),
                "{label}: frame count: ours vs ffmpeg"
            );
            for (i, (frame, dec)) in encoded.frames.iter().zip(&ff).enumerate() {
                assert_eq!(
                    dec.y, frame.reconstruction.y,
                    "{label} frame {i}: ffmpeg luma"
                );
                assert_eq!(dec.u, frame.reconstruction.u, "{label} frame {i}: ffmpeg U");
                assert_eq!(dec.v, frame.reconstruction.v, "{label} frame {i}: ffmpeg V");
            }
        }
        force_b128_ab(0);
        eprintln!(
            "unforced on testsrc2 over 12 frames: {} HORZ_A + {} HORZ_B + {} VERT_A + {} VERT_B",
            unforced[0], unforced[1], unforced[2], unforced[3],
        );
    }

    #[test]
    fn every_frame_of_a_sequence_decodes_through_our_own_decoder() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        // 256x128, not the 128x64 this started at: once a leaf could search
        // its second reference (lane-av1comp4) the small card's every
        // eligible block went to a compound winner, which commits its flat
        // transform, and the split below stopped firing on content that
        // still splits at every larger size ([[gate-blind-to-feature]] --
        // the guard is right, the fixture had shrunk under it).
        let (width, height) = (256usize, 128usize);
        let pictures: Vec<Picture> = (0..5)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let _ = take_inter_tx_split_hits();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        let raw = take_inter_tx_split_hits();
        // Summed over the four block classes the census keys on (leaf/32x32 x
        // single/compound): this assert is about the split firing at all.
        let split = [
            raw.iter().step_by(2).sum::<usize>(),
            raw.iter().skip(1).step_by(2).sum::<usize>(),
        ];
        eprintln!("inter var-tx: {} blocks flat, {} split", split[0], split[1]);
        // [[gate-blind-to-feature]]: with inter `TxMode::Select` on, this
        // content splits -- a green round trip in which the split never fires
        // would prove only the flat path.
        if tx_select() && tx_select_inter() {
            assert!(split[1] > 0, "the inter var-tx split never fired");
        }
        let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
        assert_eq!(decoded.len(), encoded.frames.len(), "frame count");
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            let recon = &frame.reconstruction;
            for (plane, got, want, stride) in [
                ("luma", &dec.y, &recon.y, width),
                ("U", &dec.u, &recon.u, width / 2),
                ("V", &dec.v, &recon.v, width / 2),
            ] {
                assert_eq!(got.len(), want.len(), "frame {i}: {plane} size");
                if let Some(at) = got.iter().zip(want).position(|(a, b)| a != b) {
                    panic!(
                        "frame {i}: {plane} differs first at ({}, {}): decoded {} vs \
                         reconstruction {} ({} of {} samples differ)",
                        at % stride,
                        at / stride,
                        got[at],
                        want[at],
                        got.iter().zip(want).filter(|(a, b)| a != b).count(),
                        got.len(),
                    );
                }
            }
        }
    }

    /// The rate term the mode search ranks every decision by has to be the
    /// rate the writer then spends. The search prices a block's coefficients
    /// through `tile::coeff_bits` -- the writer's own symbol chain, but
    /// against the default CDFs a tile starts from and the contexts of a
    /// block whose neighbours coded nothing -- while the tile writer codes
    /// them against a state that has been adapting all frame (the
    /// price-the-narrowing class). This measures that drift: the sum of what
    /// the search paid for the coefficients it kept, against the bytes the
    /// writer actually emitted for the whole tile (which also carries the
    /// partition/mode/skip/mv syntax the sum leaves out, so the writer is
    /// expected to be the larger of the two).
    #[test]
    fn predicted_coeff_bits_track_the_tile_the_writer_wrote() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let (width, height) = (256usize, 128usize);
        let pictures: Vec<Picture> = (0..5)
            .map(|i| panned_test_card(width, height, i * 2))
            .collect();
        let _ = take_predicted_bits();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        let predicted = take_predicted_bits();
        assert_eq!(predicted.len(), encoded.frames.len());

        eprintln!("frame  predicted bits  written bits  drift");
        // Split by sign: a NEGATIVE drift is the search OVER-pricing (the
        // defect this gate exists to catch), a positive one is the written
        // side carrying syntax the coefficient sum leaves out -- and since
        // lane-av1mv an extra reference's `NEWMV` puts a whole MV residual
        // on that side, so the two directions no longer share a bound.
        let mut worst_over: f64 = 0.0;
        let mut worst_under: f64 = 0.0;
        // The census is per CODED frame, so it is zipped in coding order --
        // which is not display order under a pyramid.
        let coded: Vec<&Encoded> = encoded
            .coding_order
            .iter()
            .map(|&i| &encoded.frames[i])
            .collect();
        for (i, (frame, &bits)) in coded.into_iter().zip(&predicted).enumerate() {
            let written = frame.tile.len() as f64 * 8.0;
            let drift = (written - bits) / written;
            worst_over = worst_over.max(-drift);
            // lane-av1pyrdef: a pyramid LEAF codes almost no coefficients at
            // all (150-250 bits of tile, nearly all of it partition/mode/mv
            // syntax the coefficient sum never counts), so its ratio measures
            // that floor rather than any drift. Frames under 512 written bits
            // are left out of the under-price bound for that reason; the
            // over-price bound -- the defect direction, where the search pays
            // MORE than the writer -- still covers every frame.
            if written >= 512.0 {
                worst_under = worst_under.max(drift);
            }
            eprintln!(
                "{i:5}  {bits:14.0}  {written:12.0}  {:+6.2}%",
                drift * 100.0
            );
        }
        // Measured 2026-09-06 on this sequence: -13.5% at the key frame,
        // -8.1%, -3.8%, -2.1%, +1.8% down the inter frames. The search
        // over-prices every frame but the last (its sum EXCLUDES the
        // partition/mode/skip/mv syntax the written side carries, so the true
        // coefficient over-price is larger still): the writer's CDFs adapt to
        // the content while the search's stay at the defaults. The bound is
        // set just above the worst measured point -- a widening drift is the
        // thing this gate exists to catch.
        assert!(
            worst_over <= 0.16,
            "the search's rate term OVER-priced by {:.2}% against the bits the writer spent",
            worst_over * 100.0
        );
        // Measured 2026-09-06 with the extra-reference `NEWMV` in: +18.9% at
        // the worst inter frame, where nearly every block now codes an MV
        // residual and a reference the coefficient sum never counts. Re-measured
        // at +29.6% once the encoder's grid published compound blocks properly
        // (compound share 8.8% -> 11.5%): a compound block carries a second
        // reference tree, a compound mode symbol and -- for the half-new modes
        // -- an MV residual, none of which the coefficient sum counts.
        // lane-av1txbits: pricing against the tables the frame's writer really
        // starts from collapses the over-price above to nothing, so the whole
        // remaining gap is the syntax the coefficient sum leaves out. That is
        // the default on a non-screen frame since lane-av1price2 (this
        // fixture is one), and it reads +32.6% here.
        // lane-av1obmc: every eligible single-reference inter block now also
        // carries a `motion_mode` symbol the coefficient sum never counts,
        // and an OBMC block carries the more expensive of the two values --
        // re-measured at +35.0% here with `EC_AV1_OBMC=1`. Like
        // `EC_AV1_PRICE_FRAME_CDFS=1` before it, that knob changes encoder
        // decisions, so this bound and the byte pins above FAIL BY DESIGN
        // under it; the default path (no motion_mode syntax at all) is the
        // one they are measured on.
        // lane-av1pyrdef: `encode_sequence` codes this fixture under the
        // coding pyramid now, so the five frames are a key frame, a hidden
        // ALTREF at q-16 and three leaves at q+8: +2.6% / +6.9% (the two
        // frames over the 512-bit floor) and +44.7 / +74.4 / +74.4 on the
        // near-empty leaves below it. Both bounds stand where they were.
        // lane-av1rejudge: local warp is ON by default now, so every eligible
        // single-reference block carries that same `motion_mode` symbol (a
        // 3-value alphabet where a warp sample exists) on the DEFAULT path --
        // +34.9% here, one point past the OBMC reading above. The bound moves
        // with it; `EC_AV1_WARP=0` restores the +32.6% no-motion_mode number.
        // lane-av1cfl: a `UV_CFL_PRED` block carries a `cfl_alpha_signs`
        // symbol and one or two magnitude symbols, none of which the
        // coefficient sum counts either -- +36.9% here against the same
        // fixture's +34.9% with `EC_AV1_CFL=0`.
        // ... and a directional block whose `angle_delta_y` is no longer
        // always ZERO codes a costlier symbol off the same CDF -- +39.1%.
        assert!(
            worst_under <= 0.40,
            "the writer spent {:.2}% more than the search priced -- more than \
             the mode/mv syntax outside the coefficient sum explains",
            worst_under * 100.0
        );
    }

    /// The census behind step (1) of the pricer lane: for every transform
    /// block the writer really codes, what the search priced those levels at
    /// against what the writer spent on them, split by table set and non-zero
    /// count. One average hides the interesting part -- the classes the
    /// estimate is systematically wrong about (all-zero blocks, the
    /// one-coefficient blocks the inter search lives on) -- so the split is
    /// printed and only the total is asserted on.
    #[test]
    fn pricer_error_census_by_block_class() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let (width, height) = (256usize, 128usize);
        let pictures: Vec<Picture> = (0..5)
            .map(|i| panned_test_card(width, height, i * 2))
            .collect();
        crate::tile::pricer_census_on();
        let _ = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        let rows = crate::tile::take_pricer_census();
        assert!(!rows.is_empty(), "the census saw no transform block");

        println!("| set | nz | blocks | priced bits | written bits | error |");
        println!("|---|---|---|---|---|---|");
        let (mut priced_all, mut written_all) = (0.0, 0.0);
        for ((set, bucket), (blocks, priced, written)) in &rows {
            priced_all += priced;
            written_all += written;
            println!(
                "| {set} | {} | {blocks} | {priced:.0} | {written:.0} | {:+.1}% |",
                crate::tile::census_bucket_label(*bucket),
                if *written > 0.0 {
                    (priced - written) / written * 100.0
                } else {
                    0.0
                }
            );
        }
        println!(
            "| ALL | | {} | {priced_all:.0} | {written_all:.0} | {:+.1}% |",
            rows.values().map(|r| r.0).sum::<u64>(),
            (priced_all - written_all) / written_all * 100.0
        );
        assert!(written_all > 0.0);
    }

    /// Step (2) of the pricer lane: the same census, but on the REAL clips
    /// the BD gate scores -- one film clip and the screen capture -- so the
    /// question the film/screen split raises can be answered from data. Real
    /// tables (`EC_AV1_PRICE_FRAME_CDFS=1`) make both film clips smaller and
    /// the screen capture larger, and the census says why: on a screen
    /// capture the frame's own starting tables are the tables of a frame that
    /// coded PALETTE and long runs of zero blocks, so they are far from the
    /// tables the writer ends the frame with; the pricer error does not fall
    /// the way it does on film.
    ///
    /// Not a gate -- it prints. Run it once per mode:
    ///     EC_AV1_PRICE_FRAME_CDFS=0 cargo test -p ec-av1 --release --lib -- \
    ///         --ignored pricer_error_census_on_clips --nocapture
    #[test]
    #[ignore = "reads the real library, needs ffmpeg"]
    fn pricer_error_census_on_clips() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP pricer_error_census_on_clips: no ffmpeg");
            return;
        }
        // lane-av1clipprobe: the crate's one fixture root (EC_FIXTURES first),
        // so this probe reads the same tree the preflight validates. Its
        // `assert!(!clips.is_empty())` below was already a hard failure, so no
        // skip shape changes here — only the root does.
        let fixtures = crate::library_fixture::root();
        let mut clips: Vec<(String, std::path::PathBuf)> = Vec::new();
        let film = fixtures.join("video/h264-1080p-23.976-8bit.mp4");
        if film.exists() {
            clips.push(("film 1080p".to_string(), film));
        }
        if let Ok(manifest) = std::fs::read_to_string(fixtures.join("real-library-manifest.tsv"))
            && let Some(p) = manifest
                .lines()
                .skip(1)
                .filter_map(|l| l.split('\t').next())
                .find(|p| {
                    p.contains("/OBS/") && p.ends_with(".mkv") && std::path::Path::new(p).exists()
                })
        {
            clips.push(("screen".to_string(), std::path::PathBuf::from(p)));
        }
        assert!(!clips.is_empty(), "no source clip available");
        let (width, height, frames) = (640usize, 384usize, 6usize);
        for (name, path) in &clips {
            let fctx = &crate::decode::FrameCtx::for_encoder();
            let source = clip_frames(path.to_str().unwrap(), "0", width, height, frames);
            SCREEN_FRAMES.iter().for_each(|c| {
                c.store(0, std::sync::atomic::Ordering::Relaxed);
            });
            let _ = crate::tile::take_pricing_hits();
            crate::tile::pricer_census_on();
            let _ = encode_sequence_with_ctx(&source, 100, 0.5, fctx).unwrap();
            let rows = crate::tile::take_pricer_census();
            let (price_default, price_real) = crate::tile::take_pricing_hits();
            let screen_on = SCREEN_FRAMES[1].load(std::sync::atomic::Ordering::Relaxed);
            println!(
                "\n## {name} ({frames} frames at {width}x{height}, q=100): screen frames \
                 {screen_on}, pricer armings default={price_default} real={price_real}"
            );
            println!("| set | nz | blocks | priced bits | written bits | error |");
            println!("|---|---|---|---|---|---|");
            let (mut priced_all, mut written_all) = (0.0, 0.0);
            for ((set, bucket), (blocks, priced, written)) in &rows {
                priced_all += priced;
                written_all += written;
                println!(
                    "| {set} | {} | {blocks} | {priced:.0} | {written:.0} | {:+.1}% |",
                    crate::tile::census_bucket_label(*bucket),
                    if *written > 0.0 {
                        (priced - written) / written * 100.0
                    } else {
                        0.0
                    }
                );
            }
            println!(
                "| ALL | | {} | {priced_all:.0} | {written_all:.0} | {:+.1}% |",
                rows.values().map(|r| r.0).sum::<u64>(),
                (priced_all - written_all) / written_all * 100.0
            );
            assert!(
                written_all > 0.0,
                "{name}: the census saw no transform block"
            );
        }
    }

    /// Low motion has to actually buy something: at least one inter frame of
    /// a panning sequence has to cost fewer bytes than the key frame that
    /// starts it, and the inter-block share it prints has to be non-zero --
    /// a search that never picks an inter mode would still pass a rate gate
    /// on a picture with no motion in it (the class this repo calls
    /// gate-blind-to-feature), so the share is printed even though it is not
    /// asserted on beyond being reachable at all.
    #[test]
    fn low_motion_makes_an_inter_frame_smaller_than_the_key_frame() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let (width, height) = (256usize, 128usize);
        let pictures: Vec<Picture> = (0..5)
            .map(|i| panned_test_card(width, height, i * 2))
            .collect();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();

        let key_bytes = encoded.frames[0].stream.len();
        eprintln!("frame  bytes  inter share");
        for (i, frame) in encoded.frames.iter().enumerate() {
            eprintln!(
                "{i:5}  {:5}  {:11.2}",
                frame.stream.len(),
                frame.inter_block_share
            );
        }
        assert!(
            encoded.frames[1..]
                .iter()
                .any(|f| f.stream.len() < key_bytes),
            "no inter frame of a panning sequence beat the key frame's {key_bytes} bytes"
        );
        assert!(
            encoded.frames[1..]
                .iter()
                .any(|f| f.inter_block_share > 0.0),
            "no inter frame coded a single inter block -- the search never fired"
        );
    }

    /// A manual perf probe, not a gate: prints where a key frame's and an
    /// inter frame's time goes, coarse stage by coarse stage, so a perf lane
    /// measures before it optimizes rather than guessing. Run with
    /// `cargo test -p ec-av1 --release stage_timing_breakdown -- --ignored --nocapture`.
    #[test]
    #[ignore = "a perf probe, not a gate"]
    fn stage_timing_breakdown() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        use std::time::Instant;

        for &(width, height) in &[(1920usize, 1080usize), (3840, 2160)] {
            let picture = test_card(width, height);

            let t = Instant::now();
            let dc_only =
                encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &[DC_PRED], fctx).unwrap();
            let dc_only_t = t.elapsed();

            let t = Instant::now();
            let non_directional =
                encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &NON_DIRECTIONAL, fctx)
                    .unwrap();
            let non_directional_t = t.elapsed();

            let t = Instant::now();
            let all_modes = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
            let all_modes_t = t.elapsed();

            eprintln!(
                "\n=== key frame {width}x{height} ===\n\
                 1 mode  (DC only):        {dc_only_t:>9.2?}  ({} bytes)\n\
                 7 modes (non-directional): {non_directional_t:>9.2?}  ({} bytes)\n\
                 13 modes (default search):  {all_modes_t:>9.2?}  ({} bytes)",
                dc_only.stream.len(),
                non_directional.stream.len(),
                all_modes.stream.len(),
            );

            // One inter frame predicting off the key frame's own
            // reconstruction, the (256, 256)-padded surface encode_sequence
            // itself would hand encode_inter_frame.
            let padded_ref = all_modes.reconstruction.padded_to(SUPERBLOCK);
            let padded_pic = picture.padded_to(SUPERBLOCK);
            let t = Instant::now();
            let inter = encode_inter_frame(
                &padded_pic,
                &padded_ref,
                100,
                0.5,
                1,
                flat_order_hints(1, 0, false),
                (width, height),
                None,
                None,
                None,
                None,
                fctx,
                None,
                &[],
            )
            .unwrap();
            let inter_t = t.elapsed();
            eprintln!(
                "inter frame vs its own key frame: {inter_t:>9.2?}  ({} bytes, inter share \
                 {:.2})",
                inter.stream.len(),
                inter.inter_block_share
            );
        }

        // Isolated per-call cost of the trial pipeline's own stages, at both
        // transform sizes the search runs, averaged over enough calls to
        // rise above `Instant::now`'s own noise.
        for side in [16usize, 32] {
            const N: u32 = 20_000;
            let residual: Vec<i32> = (0..side * side)
                .map(|i| ((i * 37 % 61) as i32) - 30)
                .collect();

            let t = Instant::now();
            let mut levels = Vec::new();
            for _ in 0..N {
                levels = forward_and_quantize(&residual, side, 8, 100, 0.5);
            }
            let forward_t = t.elapsed() / N;

            let t = Instant::now();
            for _ in 0..N {
                std::hint::black_box(crate::transform::dequant_and_inverse(&levels, side, 8, 100));
            }
            let inverse_t = t.elapsed() / N;

            let set = if side == 32 {
                crate::cdf_state::TxbSet::Luma32
            } else {
                crate::cdf_state::TxbSet::Luma16
            };
            let t = Instant::now();
            for _ in 0..N {
                std::hint::black_box(crate::tile::coeff_bits(&levels, set, 2, 0, 0));
            }
            let entropy_t = t.elapsed() / N;

            let above = vec![128u8; side * 2];
            let left = vec![128u8; side * 2];
            let mut prediction = vec![0u8; side * side];
            let t = Instant::now();
            for _ in 0..N {
                intra_predict_u8(
                    D45_PRED,
                    0,
                    Some(&above),
                    Some(&left),
                    Some(128),
                    side,
                    side,
                    false,
                    false,
                    &mut prediction,
                    fctx,
                );
            }
            let predict_t = t.elapsed() / N;

            eprintln!(
                "\n=== per-call cost at {side}x{side}, averaged over {N} calls ===\n\
                 predict (1 directional mode): {predict_t:>9.2?}\n\
                 forward_and_quantize:         {forward_t:>9.2?}\n\
                 dequant_and_inverse:          {inverse_t:>9.2?}\n\
                 coeff_bits:                   {entropy_t:>9.2?}"
            );
        }
    }

    /// Breaks one inter frame's own cost into the four [`STAGE_NS`] buckets
    /// plus what is left over once buckets 0/2/3 are subtracted.
    ///
    /// lane-av1probes: the two search-shaped buckets OVERLAP and the old
    /// print denied it. Bucket 0 is not "the motion search": `stage_since(0,
    /// ..)` fires only after `find_best_new_mv` and the global-MV search
    /// (`encode.rs:11959`, `:12080`), so it is the NEWMV/GLOBAL sub-search
    /// alone. Bucket 1 is EVERY `crate::mc::predict` call, from the search's
    /// candidates *and* from a committed inter block's final prediction --
    /// and that final prediction runs OUTSIDE any bucket-0 region. So bucket 1
    /// is neither inside bucket 0 nor smaller than it, and a print that shows
    /// it as "motion search (of which interpolation ...)" necessarily shows
    /// a sub-number bigger than its parent. Measured: bucket 0 = 17.82 ms,
    /// bucket 1 = 148.63 ms.
    ///
    /// Which number is real: BOTH timers are right; only the old ARITHMETIC
    /// was wrong. Bucket 1 is now printed as its own top-level row, the
    /// overlap is stated, and the residual is labelled for what it is -- it
    /// absorbs the committed block's own interpolation, so it is not a pure
    /// "13-mode intra candidate loop" figure either.
    ///
    /// `cargo test -p ec-av1 --release stage_timing_breakdown_inter -- --ignored --nocapture`.
    #[test]
    #[ignore = "a perf probe, not a gate"]
    fn stage_timing_breakdown_inter() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        use std::time::Instant;

        let (width, height) = (1280usize, 720);
        let picture = test_card(width, height);
        let key = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let padded_ref = key.reconstruction.padded_to(SUPERBLOCK);
        let padded_pic = picture.padded_to(SUPERBLOCK);

        stage_reset();
        let t = Instant::now();
        let inter = encode_inter_frame(
            &padded_pic,
            &padded_ref,
            100,
            0.5,
            1,
            flat_order_hints(1, 0, false),
            (width, height),
            None,
            None,
            None,
            None,
            fctx,
            None,
            &[],
        )
        .unwrap();
        let total_t = t.elapsed();
        let [motion_search, interpolation, transform_quant, entropy] = stage_read();
        // Buckets 0 and 2 and 3 are disjoint and additive. Bucket 1 (every
        // `mc::predict` call) is NOT added: it overlaps bucket 0 wherever a
        // search candidate calls predict, and it also covers the committed
        // block's final prediction, which no other bucket times. Adding it
        // would double-count the overlap; leaving it out puts the committed
        // prediction (and the intra candidate loop, RD glue, and the symbol
        // lookups outside `coeff_bits`) into the residual, which is what the
        // residual has always been.
        let accounted = motion_search + transform_quant + entropy;
        let other = total_t.saturating_sub(accounted);
        let overlap = interpolation.saturating_sub(motion_search);

        eprintln!(
            "\n=== inter frame {width}x{height}, one frame ===\n\
             total:                          {total_t:>9.2?}  ({} bytes)\n\
             NEWMV/GLOBAL sub-search (bucket 0): {motion_search:>9.2?}\n\
             transform + quantize (bucket 2):     {transform_quant:>9.2?}\n\
             coeff_bits, entropy pricing (bucket 3): {entropy:>9.2?}\n\
             buckets 0+2+3 accounted:              {accounted:>9.2?}\n\
             \n\
             OVERLAPPING, not additive: every mc::predict call (bucket 1)  {interpolation:>9.2?}\n\
             \x20 -- of which {overlap:>9.2?} is OUTSIDE bucket 0 (a committed\n\
             \x20 inter block's final prediction, which no bucket above times)\n\
             \x20 and the rest is the search candidates' interpolation, already\n\
             \x20 inside bucket 0. It is deliberately NOT added to the total.\n\
             \n\
             residual (13-mode intra candidate loop, the committed block's\n\
             \x20 interpolation above, NEARESTMV's own predict, RD glue):  {other:>9.2?}",
            inter.stream.len(),
        );
    }

    /// The quality gate [`Search::top_k`]'s lever lives or dies by: three real
    /// frames (a dark/flat one, a detailed one, a normal film one) at two
    /// `base_q_idx` values, encoded with the mode search unpruned (`top_k =
    /// None`, the baseline this same build produces without
    /// `EC_AV1_PRUNE_K` set) and with it pruned to each of a few `K`, bytes
    /// and luma PSNR compared side by side. Not a gate itself -- the numbers
    /// go in the lane report, which is where "ship as default" is decided --
    /// so it stays `#[ignore]`, run by hand with real clips on this machine.
    /// `cargo test -p ec-av1 --release prune_k_quality_sweep -- --ignored --nocapture`.
    #[test]
    // The three clip paths are HARD-CODED ABSOLUTES under /home/user, not
    // env-driven: off this workstation every row prints `SKIP <label>: <path>
    // not present on this machine` and the run exits 0 with no numbers. It is
    // a local-run probe; there is no fleet-host configuration that makes it
    // measure anything.
    #[ignore = "local-run only: the three clips are hard-coded /home/user paths, so off this workstation it SKIPs every row and prints nothing"]
    pub(super) fn prune_k_quality_sweep() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP prune_k_quality_sweep: no ffmpeg");
            return;
        }
        let (width, height) = (640usize, 352usize);
        let frames: &[(&str, &str, &str)] = &[
            // (label, clip, seek) -- a near-black scene, a busy one, and an
            // ordinary film frame, each from a different real file.
            (
                "dark/flat",
                "/home/user/Videos/reela.mp4",
                "3",
            ),
            (
                "detailed",
                "/home/user/Downloads/reelb.mkv",
                "1200",
            ),
            (
                "film",
                "/home/user/Videos/Films/reelc.mkv",
                "600",
            ),
        ];
        for &(label, clip, skip) in frames {
            if !std::path::Path::new(clip).exists() {
                eprintln!("SKIP {label}: {clip} not present on this machine");
                continue;
            }
            let picture = clip_frame(clip, skip, width, height);
            for &q in &[60u8, 150] {
                // Note (process-global, not a memory question): this is
                // a single-threaded #[ignore] probe, run by hand, never
                // alongside other tests that read Search::top_k.
                set_test_top_k_override(None);
                let baseline = encode_key_frame_with_ctx(&picture, q, 0.5, fctx).unwrap();
                let baseline_psnr = psnr(&baseline.reconstruction.y, &picture.y);
                eprint!(
                    "{label:9} q={q:3}  baseline {:6} bytes  {:6.2} dB",
                    baseline.stream.len(),
                    baseline_psnr
                );
                for &k in &[3usize, 4, 6] {
                    set_test_top_k_override(Some(k));
                    let pruned = encode_key_frame_with_ctx(&picture, q, 0.5, fctx).unwrap();
                    let pruned_psnr = psnr(&pruned.reconstruction.y, &picture.y);
                    eprint!(
                        "   K={k} {:6} bytes  {:6.2} dB ({:+.3} dB)",
                        pruned.stream.len(),
                        pruned_psnr,
                        pruned_psnr - baseline_psnr
                    );
                }
                eprintln!();
            }
        }
        set_test_top_k_override(None);
    }

    /// Two consecutive decoded frames from one clip, for an inter-frame
    /// sweep -- [`clip_frame`] alone only ever gives one.
    pub(super) fn clip_frame_pair(clip: &str, skip: &str, width: usize, height: usize) -> [Picture; 2] {
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(["-v", "error", "-ss", skip, "-i", clip, "-frames:v", "2"])
            .args(["-vf", &format!("scale={width}:{height}")])
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"]), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_len = luma + 2 * chroma;
        assert_eq!(out.stdout.len(), frame_len * 2, "expected two 4:2:0 frames");
        std::array::from_fn(|i| {
            let bytes = &out.stdout[i * frame_len..][..frame_len];
            Picture {
                width,
                height,
                y: bytes[..luma].iter().map(|&v| u16::from(v)).collect(),
                u: bytes[luma..luma + chroma]
                    .iter()
                    .map(|&v| u16::from(v))
                    .collect(),
                v: bytes[luma + chroma..]
                    .iter()
                    .map(|&v| u16::from(v))
                    .collect(),
            }
        })
    }

    /// The quality gate [`INTER_PRUNE_TOP_K`] lives or dies by: the same
    /// three real clips as [`prune_k_quality_sweep`], but coding a key frame
    /// then one inter frame off it (`search_inter_block`'s own intra
    /// candidates are what [`prune_by_sad`] prunes here, not the key-frame
    /// loop), at 1280x704 -- a whole number of 64x64 superblocks, so no
    /// padding step complicates the comparison. Prints bytes and luma PSNR
    /// for the inter frame alone, unpruned vs each `K`.
    /// `cargo test -p ec-av1 --release prune_k_quality_sweep_inter -- --ignored --nocapture`.
    #[test]
    // Same hard-coded /home/user clip paths as `prune_k_quality_sweep`;
    // off this workstation it SKIPs all three rows and prints nothing.
    #[ignore = "local-run only: the three clips are hard-coded /home/user paths, so off this workstation it SKIPs every row and prints nothing"]
    pub(super) fn prune_k_quality_sweep_inter() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP prune_k_quality_sweep_inter: no ffmpeg");
            return;
        }
        let (width, height) = (1280usize, 704usize);
        let frames: &[(&str, &str, &str)] = &[
            (
                "dark/flat",
                "/home/user/Videos/reela.mp4",
                "3",
            ),
            (
                "detailed",
                "/home/user/Downloads/reelb.mkv",
                "1200",
            ),
            (
                "film",
                "/home/user/Videos/Films/reelc.mkv",
                "600",
            ),
        ];
        for &(label, clip, skip) in frames {
            if !std::path::Path::new(clip).exists() {
                eprintln!("SKIP {label}: {clip} not present on this machine");
                continue;
            }
            let [key_source, inter_source] = clip_frame_pair(clip, skip, width, height);
            for &q in &[60u8, 150] {
                // Note (process-global, not a memory question): a
                // single-threaded #[ignore] probe, run by hand, never
                // alongside other tests that read Search::top_k.
                set_test_top_k_override_inter(None);
                let key = encode_key_frame_with_ctx(&key_source, q, 0.5, fctx).unwrap();
                let baseline = encode_inter_frame(
                    &inter_source,
                    &key.reconstruction,
                    q,
                    0.5,
                    1,
                    flat_order_hints(1, 0, false),
                    (width, height),
                    None,
                    None,
                    None,
                    None,
                    fctx,
                    None,
                    &[],
                )
                .unwrap();
                let baseline_psnr = psnr(&baseline.reconstruction.y, &inter_source.y);
                eprint!(
                    "{label:9} q={q:3}  baseline {:6} bytes  {:6.2} dB",
                    baseline.stream.len(),
                    baseline_psnr
                );
                for &k in &[3usize, 4, 6] {
                    set_test_top_k_override_inter(Some(k));
                    let pruned = encode_inter_frame(
                        &inter_source,
                        &key.reconstruction,
                        q,
                        0.5,
                        1,
                        flat_order_hints(1, 0, false),
                        (width, height),
                        None,
                        None,
                        None,
                        None,
                        fctx,
                        None,
                        &[],
                    )
                    .unwrap();
                    let pruned_psnr = psnr(&pruned.reconstruction.y, &inter_source.y);
                    eprint!(
                        "   K={k} {:6} bytes  {:6.2} dB ({:+.3} dB)",
                        pruned.stream.len(),
                        pruned_psnr,
                        pruned_psnr - baseline_psnr
                    );
                }
                eprintln!();
            }
        }
        set_test_top_k_override_inter(None);
    }

    /// `frames` consecutive pictures out of a real clip, scaled to a whole
    /// number of 32x32 blocks. Generalizes [`clip_frame`]/[`clip_frame_pair`]
    /// to an arbitrary count for the fixture regression gate below.
    pub(super) fn clip_frames(
        clip: &str,
        skip: &str,
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Picture> {
        clip_frames_vf(
            clip,
            skip,
            &format!("scale={width}:{height}"),
            width,
            height,
            frames,
        )
    }

    /// [`clip_frames`] with the filter chain spelled out, so a caller can ask
    /// for a native-resolution CROP instead of a downscale (the downscale is
    /// what destroys a screen capture's exact repeats -- class
    /// `gate-recipe-confound`). `width`/`height` must be what `vf` produces.
    pub(super) fn clip_frames_vf(
        clip: &str,
        skip: &str,
        vf: &str,
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Picture> {
        // ONE definition of the gate's loader recipe, shared with
        // `examples/enc_probe` so the instrument cannot arm a different source
        // than the gate it reports on (class `instrument disagrees with the
        // gate`; lane-probe).
        crate::probe::source(clip, skip, vf, width, height, frames)
    }

    /// The standing regression gate on real content: 12 frames of a real
    /// clip (`fixtures/video/h264-1080p-23.976-8bit.mp4`, decoded down to
    /// this crate's own input regardless of its own source codec), encoded
    /// as an AV1 GOP and checked against three numeric floors so a
    /// compatibility or quality regression fails the suite instead of
    /// waiting for a downstream repo's gate to catch it (as happened for a
    /// padded-size inter `FrameHeader` dav1d rejected, and a 1280x720 export
    /// refusal, both caught only that way).
    ///
    /// 640x384 is a whole number of 32x32 blocks (no straddle edge case;
    /// that geometry is already covered elsewhere), chosen only so encode
    /// time and gate numbers are not muddied by an edge-block class this
    /// test is not about.
    ///
    /// Measured 2026-08-27 at q=100: stream 69861 bytes, average luma PSNR
    /// 45.41 dB across the 12 decoded frames vs the source. Floor/ceiling
    /// below carry a margin off those measured numbers (PSNR floor =
    /// measured - 0.5 dB; byte ceiling = measured * 1.15).
    #[test]
    fn real_clip_encodes_within_its_quality_and_size_budget() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP real_clip_encodes_within_its_quality_and_size_budget: no ffmpeg");
            return;
        }
        // lane-av1clipprobe: the clip's absence was a bare `clip.exists()`
        // -> SKIP with no env escape, so this gate reported green on a tree
        // with no root `fixtures/`. The probe prints that one line (naming the
        // resolved path) and fails under the require envs.
        let Some(clip) = crate::library_fixture::require(
            "video/h264-1080p-23.976-8bit.mp4",
            "real_clip_encodes_within_its_quality_and_size_budget",
        ) else {
            return;
        };
        let (width, height, frame_count) = (640usize, 384usize, 12usize);
        let source = clip_frames(clip.to_str().unwrap(), "0", width, height, frame_count);
        let encoded = encode_sequence_with_ctx(&source, 100, 0.5, fctx).unwrap();
        assert_eq!(encoded.frames.len(), frame_count);

        // Gate 1: a real AV1 decoder (dav1d, via ffmpeg) accepts the whole
        // stream and hands back every frame -- a stream a downstream
        // decoder refuses is the exact class of regression this gate
        // exists for.
        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, frame_count);
        assert_eq!(decoded.len(), frame_count, "dav1d decoded every frame");

        // Gate 2: quality vs the source did not silently collapse.
        let mean_psnr: f64 = decoded
            .iter()
            .zip(&source)
            .map(|(d, s)| psnr(&d.y, &s.y))
            .sum::<f64>()
            / frame_count as f64;
        assert!(
            mean_psnr >= 44.91,
            "mean luma PSNR {mean_psnr:.2} dB fell below the 44.91 dB floor"
        );

        // Gate 3: the stream did not silently bloat.
        assert!(
            encoded.stream.len() <= 80_340,
            "stream grew to {} bytes, over the 80340-byte ceiling",
            encoded.stream.len()
        );
    }

    /// Sibling of `real_clip_encodes_within_its_quality_and_size_budget` at a
    /// straddle size: 640x384 is exact on both axes (32-aligned), so
    /// `Quadrant::Split` never fires there and the 16x16 `NEARESTMV` leaf
    /// path landed in e19bb47 goes unexercised by real, mixed intra/inter
    /// content. 704x400 has 400 mod 32 == 16 -- the same "exactly half
    /// straddle" class as `a_sequence_round_trips_at_the_exactly_half_
    /// straddle_size`'s 1280x720, but that test is a synthetic panned test
    /// card; this one is the real-clip gate, same three floors.
    ///
    /// Measured 2026-08-27 at q=100, 8 frames: stream 54525 bytes, average
    /// luma PSNR 45.48 dB across the 8 decoded frames vs the source.
    /// Floor/ceiling below carry the same margin as the sibling test (PSNR
    /// floor = measured - 0.5 dB; byte ceiling = measured * 1.15).
    #[test]
    fn real_clip_encodes_within_its_quality_and_size_budget_at_a_straddle_size() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!(
                "SKIP real_clip_encodes_within_its_quality_and_size_budget_at_a_straddle_size: \
                 no ffmpeg"
            );
            return;
        }
        // lane-av1clipprobe: see the sibling gate above -- the bare
        // `clip.exists()` -> SKIP had no env escape.
        let Some(clip) = crate::library_fixture::require(
            "video/h264-1080p-23.976-8bit.mp4",
            "real_clip_encodes_within_its_quality_and_size_budget_at_a_straddle_size",
        ) else {
            return;
        };
        let (width, height, frame_count) = (704usize, 400usize, 8usize);
        let source = clip_frames(clip.to_str().unwrap(), "0", width, height, frame_count);
        let encoded = encode_sequence_with_ctx(&source, 100, 0.5, fctx).unwrap();
        assert_eq!(encoded.frames.len(), frame_count);

        // Gate 1: a real AV1 decoder (dav1d, via ffmpeg) accepts the whole
        // stream and hands back every frame.
        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, frame_count);
        assert_eq!(decoded.len(), frame_count, "dav1d decoded every frame");

        // Gate 2: quality vs the source did not silently collapse.
        let mean_psnr: f64 = decoded
            .iter()
            .zip(&source)
            .map(|(d, s)| psnr(&d.y, &s.y))
            .sum::<f64>()
            / frame_count as f64;
        assert!(
            mean_psnr >= 44.98,
            "mean luma PSNR {mean_psnr:.2} dB fell below the 44.98 dB floor"
        );

        // Gate 3: the stream did not silently bloat.
        assert!(
            encoded.stream.len() <= 62_703,
            "stream grew to {} bytes, over the 62703-byte ceiling",
            encoded.stream.len()
        );
    }

    /// The replica-shaped path: 24 frames, 1280x720, one key frame followed
    /// by inter frames -- the same GOP shape [`encode_sequence`] gives the
    /// facade -- timed end to end, so the edith export-time projection is a
    /// real number rather than a guess from the isolated key/inter numbers
    /// above. Run with and without `EC_AV1_PRUNE_K` set to compare.
    /// `cargo test -p ec-av1 --release sequence_bench_sanity -- --ignored --nocapture`.
    #[test]
    #[ignore = "a perf probe, not a gate"]
    fn sequence_bench_sanity() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        use std::time::Instant;
        let (width, height) = (1280usize, 720usize);
        let pictures: Vec<Picture> = (0..24)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let t = Instant::now();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        let elapsed = t.elapsed();
        let total_bytes: usize = encoded.frames.iter().map(|f| f.stream.len()).sum();
        eprintln!(
            "24 frames @ 1280x720: {elapsed:>9.2?} total, {:>9.2?}/frame, {total_bytes} bytes",
            elapsed / 24
        );
    }

    /// The `base_q_idx` calibration sweep for `lane-av1-ratectl` -- feeds the
    /// `Quality`/`BytesPerFrame` rate-control surface in `encoder.rs`, not a
    /// gate itself. `cargo test -p ec-av1 --release calibration_sweep_base_q_idx
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore = "a calibration probe, not a gate"]
    fn calibration_sweep_base_q_idx() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP calibration_sweep_base_q_idx: no ffmpeg");
            return;
        }
        let (width, height, frames) = (640usize, 384usize, 12usize);
        for clip in [
            "h264-1080p-23.976-8bit.mp4", // film-ish, 23.976fps
            "h264-1080p-60-8bit.mp4",     // higher motion, 60fps
        ] {
            // lane-av1clipprobe: the crate's one fixture root and its one
            // presence probe -- the bare `path.exists()` -> SKIP -> `continue`
            // had no env escape, and with every clip absent the sweep looped
            // zero times and passed.
            let Some(path) = crate::library_fixture::require(
                &format!("video/{clip}"),
                &format!("calibration_sweep_base_q_idx: {clip}"),
            ) else {
                continue;
            };
            let source = clip_frames(path.to_str().unwrap(), "0", width, height, frames);
            eprintln!("--- {clip} ---");
            for q in [40u8, 70, 100, 130, 160, 190, 220, 240] {
                let encoded = encode_sequence_with_ctx(&source, q, 0.5, fctx).unwrap();
                let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, frames);
                let mean_psnr: f64 = decoded
                    .iter()
                    .zip(&source)
                    .map(|(d, s)| psnr(&d.y, &s.y))
                    .sum::<f64>()
                    / frames as f64;
                let bytes_per_px = encoded.stream.len() as f64 / (width * height * frames) as f64;
                eprintln!(
                    "q={q:3}  bytes={:7}  bytes/px={bytes_per_px:.5}  psnr={mean_psnr:.2} dB",
                    encoded.stream.len()
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // The encoder's standing BD-rate baseline against libaom and rav1e.
    // Every PSNR below is measured on FFMPEG-DECODED pixels, never on an
    // encoder's own reconstruction ([[shared-oracle-blindness]]): our decoder
    // agreeing with our encoder proves nothing about the bitstream.
    // -----------------------------------------------------------------------

    /// All-plane PSNR of a decoded picture against its source, on the same
    /// 8-bit scale [`psnr`] uses.
    pub(super) fn psnr_all(decoded: &Picture, source: &Picture) -> f64 {
        let cat = |p: &Picture| {
            let mut v = Vec::with_capacity(p.y.len() + p.u.len() + p.v.len());
            v.extend_from_slice(&p.y);
            v.extend_from_slice(&p.u);
            v.extend_from_slice(&p.v);
            v
        };
        psnr(&cat(decoded), &cat(source))
    }

    /// The pictures back as the planar 8-bit bytes an encoder reads.
    pub(super) fn raw_yuv420p(source: &[Picture]) -> Vec<u8> {
        let mut raw = Vec::new();
        for p in source {
            for plane in [&p.y, &p.u, &p.v] {
                raw.extend(plane.iter().map(|&s| s as u8));
            }
        }
        raw
    }

    /// A (all-plane PSNR, log10 bytes) ladder from another AV1 encoder driven
    /// through ffmpeg: one point per entry of `points`, each a quality flag
    /// set. Returns the ladder ordered by fidelity and the total encode wall
    /// clock. The stream is decoded back through [`ffmpeg_decode_sequence`],
    /// so both curves are measured by the same decoder.
    pub(super) fn external_ladder(
        source: &[Picture],
        width: usize,
        height: usize,
        encoder: &str,
        points: &[Vec<String>],
    ) -> (Vec<(f64, f64)>, f64) {
        // [[pid-keyed-temp-path]]: parallel test binaries must not share a name.
        let dir = std::env::temp_dir();
        let raw_path = dir.join(format!("ec-av1-bd-{}-{encoder}.yuv", std::process::id()));
        std::fs::write(&raw_path, raw_yuv420p(source)).expect("raw source");
        let mut ladder = Vec::new();
        let mut wall = 0.0;
        for (i, params) in points.iter().enumerate() {
            let obu = dir.join(format!(
                "ec-av1-bd-{}-{encoder}-{i}.obu",
                std::process::id()
            ));
            let start = std::time::Instant::now();
            let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p"])
                .args(["-s", &format!("{width}x{height}"), "-r", "24", "-i"])
                .arg(&raw_path)
                .args(["-an", "-threads", "1"])
                .args(["-g", &source.len().to_string(), "-c:v", encoder])
                .args(params)
                .args(["-f", "obu"])
                .arg(&obu), None, |c| c.output())
                .expect("ffmpeg failed to run");
            wall += start.elapsed().as_secs_f64();
            assert!(
                out.status.success(),
                "{encoder} {params:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = std::fs::read(&obu).expect("reference stream");
            let decoded = ffmpeg_decode_sequence(&stream, width, height, source.len());
            // lane-dkey: every reference point must also decode through OUR
            // decoder, sample-exact against ffmpeg's decode of the same bytes.
            // A BD table computed off streams we cannot read is a table about
            // an encoder we cannot check; this is the assertion that would
            // have caught the `force_integer_mv` defect (its stream decoded to
            // 25 dB garbage while this gate stayed green) and the crf-5 key
            // frame desync. A failure names the encoder, the point's flags and
            // the first differing frame/plane/sample.
            // lane-dpm1: a failing reference point is COLLECTED, not panicked
            // on, so one run still prints the whole BD table and names every
            // bad point instead of stopping at the first. The collection is
            // asserted empty by [`assert_ladder_decodes`] at the end of each
            // gate.
            match crate::stream::decode_stream(&stream) {
                Ok(ours) if ours.len() != decoded.len() => note_ladder_failure(format!(
                    "{encoder} {params:?}: our decoder returned {} shown frames, ffmpeg {}",
                    ours.len(),
                    decoded.len(),
                )),
                Ok(ours) => {
                    for (f, (o, d)) in ours.iter().zip(&decoded).enumerate() {
                        if let Some((plane, s, got, want)) = first_plane_mismatch(o, d) {
                            note_ladder_failure(format!(
                                "{encoder} {params:?} frame {f} plane {plane} sample {s}: our \
                                 decoder decoded {got}, ffmpeg {want}"
                            ));
                            break;
                        }
                    }
                }
                Err(e) => note_ladder_failure(format!(
                    "{encoder} {params:?}: our decoder refused the stream ({e})"
                )),
            }
            let mean: f64 = decoded
                .iter()
                .zip(source)
                .map(|(d, s)| psnr_all(d, s))
                .sum::<f64>()
                / source.len() as f64;
            ladder.push((mean, (stream.len() as f64).log10()));
            let _ = std::fs::remove_file(&obu);
        }
        let _ = std::fs::remove_file(&raw_path);
        ladder.sort_by(|a, b| a.0.total_cmp(&b.0));
        (ladder, wall)
    }

    /// Every reference ladder point that did not decode sample-exactly
    /// through our own decoder, collected across a whole gate run so the BD
    /// table still prints and EVERY bad point is named
    /// (`encoder params frame plane sample`) instead of only the first.
    static LADDER_FAILURES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    pub(super) fn note_ladder_failure(msg: String) {
        eprintln!("LADDER DECODE FAILURE: {msg}");
        LADDER_FAILURES.lock().expect("ladder failures").push(msg);
    }

    /// Fail the gate at its END on everything [`external_ladder`] collected.
    pub(super) fn assert_ladder_decodes() {
        let failures = std::mem::take(&mut *LADDER_FAILURES.lock().expect("ladder failures"));
        assert!(
            failures.is_empty(),
            "{} reference ladder point(s) did not decode exactly through our decoder:\n{}",
            failures.len(),
            failures.join("\n"),
        );
    }

    /// First differing sample between two pictures, as
    /// `(plane, index, left, right)` -- so a gate failure names the plane and
    /// the sample instead of just "differs". A length difference counts as a
    /// mismatch of that plane at the first index past the shorter one.
    pub(super) fn first_plane_mismatch(a: &Picture, b: &Picture) -> Option<(&'static str, usize, i64, i64)> {
        for (plane, l, r) in [("Y", &a.y, &b.y), ("U", &a.u, &b.u), ("V", &a.v, &b.v)] {
            if let Some(i) = l.iter().zip(r.iter()).position(|(x, y)| x != y) {
                return Some((plane, i, i64::from(l[i]), i64::from(r[i])));
            }
            if l.len() != r.len() {
                return Some((plane, l.len().min(r.len()), l.len() as i64, r.len() as i64));
            }
        }
        None
    }

    /// The same ladder for this encoder, at four `base_q_idx` points. The PSNR
    /// comes from ffmpeg's decode of our own stream, and at EVERY point the
    /// stream is decoded twice -- by ffmpeg and by our own
    /// [`crate::stream::decode_stream`] -- and all three pictures (encoder
    /// reconstruction, ffmpeg's decode, our decode) are asserted sample-exact
    /// in Y, U and V over every shown frame in display order. That is itself
    /// a gate: it proves the bitstream says what the encoder thinks, in the
    /// only two decoders that read it.
    pub(super) fn our_ladder(
        name: &str,
        source: &[Picture],
        width: usize,
        height: usize,
        fctx: &crate::decode::FrameCtx,
    ) -> (Vec<(f64, f64)>, f64) {
        let mut ladder = Vec::new();
        let mut wall = 0.0;
        for &q in &[150u8, 120, 90, 60] {
            let start = std::time::Instant::now();
            // lane-rdoq: the deadzone was fitted WITHOUT the RDOQ pass, so
            // the gate has to be able to re-sweep it. 0.5 is the shipped one
            // (`crate::encoder::DEADZONE`).
            let deadzone = std::env::var("EC_AV1_DEADZONE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.5);
            let encoded = encode_sequence_with_ctx(source, q, deadzone, fctx).unwrap();
            wall += start.elapsed().as_secs_f64();
            let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, source.len());
            let ours = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
            assert_eq!(
                ours.len(),
                source.len(),
                "{name} q={q}: our decoder's display-order frame count"
            );
            for (i, (d, e)) in decoded.iter().zip(&encoded.frames).enumerate() {
                if let Some((plane, s, got, want)) = first_plane_mismatch(d, &e.reconstruction) {
                    panic!(
                        "{name} q={q} frame {i} plane {plane} sample {s}: ffmpeg decoded \
                         {got}, the encoder reconstructed {want}"
                    );
                }
            }
            for (i, (o, e)) in ours.iter().zip(&encoded.frames).enumerate() {
                if let Some((plane, s, got, want)) = first_plane_mismatch(o, &e.reconstruction) {
                    panic!(
                        "{name} q={q} frame {i} plane {plane} sample {s}: our decoder decoded \
                         {got}, the encoder reconstructed {want}"
                    );
                }
            }
            let per_frame: Vec<f64> = decoded
                .iter()
                .zip(source)
                .map(|(d, s)| psnr_all(d, s))
                .collect();
            // lane-txrd: does a tool's loss start at the FIRST inter frame
            // (a local mispricing) or grow down the GOP (propagation into
            // the reference chain)? One line per q point answers it.
            if crate::envflags::env_flag!("EC_AV1_FRAME_PSNR") {
                let row: Vec<String> = per_frame.iter().map(|p| format!("{p:.2}")).collect();
                eprintln!("{name}: q={q} per-frame PSNR {}", row.join(" "));
                // lane-txrd: per PLANE, because an all-plane mean hides a
                // collapse in one of them (class `metric blind to a plane`).
                let plane = |pick: fn(&Picture) -> &Vec<u16>| -> String {
                    let v: Vec<String> = decoded
                        .iter()
                        .zip(source)
                        .map(|(d, s)| format!("{:.2}", psnr(pick(d), pick(s))))
                        .collect();
                    v.join(" ")
                };
                eprintln!("{name}: q={q} Y {}", plane(|p| &p.y));
                eprintln!("{name}: q={q} U {}", plane(|p| &p.u));
                eprintln!("{name}: q={q} V {}", plane(|p| &p.v));
            }
            let mean: f64 = per_frame.iter().sum::<f64>() / source.len() as f64;
            ladder.push((mean, (encoded.stream.len() as f64).log10()));
        }
        // What this clip asked for and what the content gate left it coding
        // under: a screen clip requests a pyramid and codes flat, and that
        // difference is what keeps its row identical to the flat baseline
        // (class `gate-blind-to-feature`).
        eprintln!(
            "{name}: sequence, pyramid requested {:?} effective {:?}",
            crate::encoder::Pyramid::from_env(),
            last_sequence_pyramid(),
        );
        ladder.sort_by(|a, b| a.0.total_cmp(&b.0));
        (ladder, wall)
    }

    /// [`our_ladder`] through the streaming facade — the surface the editor's
    /// export drives (`EC_AV1_GATE_FACADE=1`). With a [`crate::encoder::Pyramid`]
    /// each stream is also reordered (hidden `ALTREF` first,
    /// `show_existing_frame` last per group) and each level is coded at its
    /// own quantizer offset; without one it is the flat ladder, which
    /// `encoder::tests::the_facade_codes_the_same_bytes_as_encode_sequence`
    /// pins byte-identical to [`our_ladder`]'s, so this arm must print the
    /// same BD numbers. Returns the ladder, the wall, per pyramid level over
    /// the whole ladder how many frames and how many bytes it spent, and the
    /// pyramid the streams were EFFECTIVELY coded under — the content gate in
    /// `Av1Encoder::encode_frames` drops a screen-content clip back to the
    /// flat path, so a requested pyramid can come back `None` here.
    pub(super) fn our_ladder_facade(
        name: &str,
        source: &[Picture],
        width: usize,
        height: usize,
        pyramid: Option<crate::encoder::Pyramid>,
    ) -> (
        Vec<(f64, f64)>,
        f64,
        [usize; 4],
        [usize; 4],
        Option<crate::encoder::Pyramid>,
    ) {
        use crate::encoder::{Av1Encoder, Colour, EncoderConfig, Level};
        let slot = |l: Level| match l {
            Level::Key => 0,
            Level::Arf => 1,
            Level::Leaf => 2,
            Level::ShowExisting => 3,
        };
        let mut ladder = Vec::new();
        let mut wall = 0.0;
        let mut effective = None;
        let (mut counts, mut bytes) = ([0usize; 4], [0usize; 4]);
        for &q in &[150u8, 120, 90, 60] {
            let config = EncoderConfig {
                width,
                height,
                base_q_idx: q,
                gop: source.len(),
                colour: Colour::Unspecified,
                tile_cols_log2: 0,
                tile_rows_log2: 0,
            };
            let start = std::time::Instant::now();
            let mut enc = match pyramid {
                None => Av1Encoder::new(config).unwrap(),
                Some(p) => Av1Encoder::with_pyramid(config, p).unwrap(),
            };
            let mut packets = Vec::new();
            for picture in source {
                packets.extend(enc.encode_frames(picture).unwrap());
            }
            packets.extend(enc.flush().unwrap());
            wall += start.elapsed().as_secs_f64();
            effective = enc.pyramid();
            let mut stream = Vec::new();
            for p in &packets {
                counts[slot(p.level)] += 1;
                bytes[slot(p.level)] += p.data.len();
                stream.extend_from_slice(&p.data);
            }
            // [[gate-blind-to-hidden-frames]]: a reordered stream is only
            // correct if the DISPLAY-order output still has one frame per
            // source picture, in the right order -- checked against our own
            // decoder as well as ffmpeg's, both below.
            let ours = crate::stream::decode_stream(&stream).expect("our decoder");
            assert_eq!(
                ours.len(),
                source.len(),
                "{name} q={q}: our display-order count"
            );
            let decoded = ffmpeg_decode_sequence(&stream, width, height, source.len());
            for (i, (d, o)) in decoded.iter().zip(&ours).enumerate() {
                if let Some((plane, s, got, want)) = first_plane_mismatch(d, o) {
                    panic!(
                        "{name} q={q} display frame {i} plane {plane} sample {s}: ffmpeg \
                         decoded {got}, our decoder decoded {want}"
                    );
                }
            }
            let mean: f64 = decoded
                .iter()
                .zip(source)
                .map(|(d, s)| psnr_all(d, s))
                .sum::<f64>()
                / source.len() as f64;
            ladder.push((mean, (stream.len() as f64).log10()));
        }
        ladder.sort_by(|a, b| a.0.total_cmp(&b.0));
        (ladder, wall, counts, bytes, effective)
    }

    /// Byte-exactness gate for encoder work that is meant to change only
    /// how long the encoder takes.
    ///
    /// The BD gate above measures quality, which a speed lane can move by a
    /// hair without failing anything; this pins the streams themselves.
    /// Re-measured on the lane-av1lr merge with main d5fc99e0 (per-64x64
    /// `cdef_idx` literals and per-unit loop restoration syntax are new bits
    /// in the tile) and again on lane-av1mv's merge of it: an extra
    /// reference's `NEWMV` is a coded-bits change too, judged by the BD gate
    /// above, not by this pin. Re-pinned on lane-av1price2: the search now
    /// prices coefficients against the tables the frame's writer really
    /// starts from on every non-screen frame (`tile::arm_pricing_cdfs`), a
    /// decision change the BD gate judges. Re-pinned again on
    /// lane-av1skipctx: the search now prices `txb_skip`/`dc_sign` at the
    /// real neighbour contexts ([`Plane::coef_ctx`]) instead of zero, which
    /// is the same kind of decision change. Re-pinned on lane-av1lambda:
    /// [`LAMBDA_SCALE`] moved 0.1 -> 0.05, so every RD decision in the
    /// search moved with it. Re-pinned on lane-av1rejudge: local warp is on
    /// by default ([`warp_on`], a sequence/frame header bit and a new
    /// motion_mode symbol on eligible blocks) and
    /// [`LEAF_SECOND_NEW_MARGIN`] moved 0.8 -> 1.2. Re-pinned on lane-av1tpl:
    /// every inter frame's superblocks now code at their own lambda
    /// ([`TPL_STRENGTH`]), so every RD decision in an inter frame moved.
    /// Re-pinned on lane-av1resweep: [`LAMBDA_SCALE`] moved 0.05 -> 0.0275
    /// and the two var-tx depth searches ([`tx32_depth_search`],
    /// [`compound_var_tx`]) are on by default, all three re-judged on the
    /// native gate's real film rows.
    /// Re-pinned on lane-av1cfl: the chroma search offers `UV_CFL_PRED`, so
    /// every block whose chroma takes it codes a different mode symbol, two
    /// alpha symbols and different chroma coefficients. `EC_AV1_CFL=0`
    /// restores the previous streams. Re-pinned again in the same lane once
    /// directional luma winners searched their `angle_delta_y`
    /// (`EC_AV1_ANGLE=0` restores that half).
    #[test]
    fn the_encoders_own_streams_are_byte_identical_to_their_pins() {
        // These are DEFAULT-PRESET pins: a speed preset switches search levers
        // off, so it legitimately moves every byte. Encode at speed 0 whatever
        // `EC_AV1_SPEED` says (write guard: the preset is process-global).
        let _knobs = crate::speed::knob_write();
        let was = crate::speed::speed();
        crate::speed::set_speed(0);
        if !have_ffmpeg() {
            eprintln!("SKIP the_encoders_own_streams_are_byte_identical_to_their_pins: no ffmpeg");
            return;
        }
        // lane-av1clipprobe: the bare `clip.exists()` -> SKIP had no env escape,
        // so a tree with no root `fixtures/` reported this gate green having
        // compared nothing at all.
        let Some(clip) = crate::library_fixture::require(
            "video/h264-1080p-23.976-8bit.mp4",
            "the_encoders_own_streams_are_byte_identical_to_their_pins",
        ) else {
            return;
        };
        let source = clip_frames(clip.to_str().unwrap(), "0", 640, 384, 4);
        // FNV-1a over the stream: a witness that every coded bit is where it
        // was, which a byte count alone is not.
        let fnv = |b: &[u8]| {
            b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &v| {
                (h ^ u64::from(v)).wrapping_mul(0x0000_0100_0000_01b3)
            })
        };
        // Re-taken on lane-av1pyrdef: `encode_sequence` codes this (non-screen)
        // clip under the coding pyramid now -- one hidden ALTREF per mini-GOP
        // of 4, its leaves at q+8 and a `show_existing_frame` header per
        // group. 7299 -> 8194 bytes at q=150 and 26804 -> 28285 at q=60 over
        // four frames; four pictures is one short mini-GOP, which is the
        // shape that pays least (the ARF's own quality has no later group to
        // carry it), so this is not the BD gate's number.
        // Re-taken again on lane-av1cfl: the chroma search offers
        // `UV_CFL_PRED` and directional luma winners search `angle_delta_y`,
        // so every block taking either codes different syntax and different
        // coefficients -- 8194 -> 7321 at q=150 and 28285 -> 27285 at q=60.
        // `EC_AV1_CFL=0` / `EC_AV1_ANGLE=0` restore each half.
        // Re-taken on lane-pyr3: the mini-GOP of the default pyramid is 16
        // pictures at `q-24` / `q+16` now (the sweep in [`Pyramid::default`]),
        // so these four pictures code as one truncated group -- a hidden
        // ALTREF at `q-24` and three leaves at `q+16` -- instead of one
        // mini-GOP of 4 at `q-16` / `q+8`: 7373 -> 8076 at q=150 and
        // 27074 -> 27585 at q=60. `EC_AV1_PYRAMID=4:-16:8` restores these.
        // Re-taken on lane-fintra: the sequence header sets
        // `enable_filter_intra`, so every DC_PRED intra block of at most
        // 32x32 without a luma palette carries a `use_filter_intra` flag and
        // the blocks that take one code a filter-intra mode, a different
        // prediction and different coefficients -- 7321 -> 7373 at q=150 and
        // 27285 -> 27074 at q=60. `EC_AV1_FILTER_INTRA=0` restores these.
        // Re-taken on lane-b64 (64x64 skip root), at the lane-pyr4 merge
        // (default pyramid `8:-32:16`, one truncated group of 8 with its
        // hidden ALTREF at `q-32`), and at the lane-tx64 merge: the 64x64
        // root can code a REAL residual now (one TX_64X64 luma transform and
        // two TX_32X32 chroma ones). `EC_AV1_B64RES=0` restores 8446 / 28548;
        // `EC_AV1_B64=0` gives the pre-root bytes under this pyramid.
        // Re-taken on lane-keyq: the pyramid codes its KEY FRAME at
        // `base_q - 48` now ([`crate::encoder::Pyramid::key_q_offset`]), and
        // one of these four pictures is the key -- 8590 -> 9778 at q=150 and
        // 28535 -> 35450 at q=60. `EC_AV1_PYRAMID=8:-32:16:-8:0` restores these.
        // Re-taken on lane-arfq: the leaves are coded 12 quantizer steps above
        // base now, not 16 ([`crate::encoder::Pyramid::leaf_q_offset`]) -- 9778
        // -> 9835 at q=150 and 35450 -> 35798 at q=60.
        // `EC_AV1_PYRAMID=8:-32:16:-8:-48` restores these.
        // Re-taken at the lane-b64b merge, the lane-rdoq merge (`EC_AV1_RDOQ=0`
        // gave 9808 / 35791) and the lane-i64 merge: the KEY frame -- one of
        // these four pictures -- offers every superblock a 64x64 intra root
        // now. `EC_AV1_I64=0` restores 8557 / 33345.
        // Re-taken on lane-pricer: the inter RD search prices a single
        // reference through the tree and the neighbour contexts the writer
        // really codes it with, so every inter picture's reference decisions
        // move -- 8562 -> 8535 at q=150 and 33357 -> 33221 at q=60.
        // Re-taken on lane-ctx: the RD pricer reads the writer's own contexts
        // for `skip`/`is_inter` and for the whole compound reference tree, so
        // every inter block's cost -- and with it the skip and reference
        // decisions -- moved: 8535 -> 8311 bytes at q=150 and 33221 -> 33087
        // at q=60. Re-taken on lane-ctx2: a single-reference candidate now
        // pays the `comp_mode = 0` symbol the writer codes ahead of it, so
        // single and compound are priced against the same syntax -- 8311 ->
        // 8325 bytes at q=150 and 33087 -> 33014 at q=60.
        // Re-taken on lane-newmv: the motion search charges its own rate term
        // at half the block RD lambda ([`crate::motion::MV_SEARCH_LAMBDA`])
        // and each subpel stage re-runs from its own winner
        // ([`crate::motion::MV_SUBPEL_ITERS`]), so every inter block's vector
        // -- and with it its residual -- moved: 8325 -> 8288 bytes at q=150
        // and 33014 -> 33090 at q=60.
        // Re-taken on lane-dq: preset 0 codes a per-superblock `delta_qindex`
        // from libaom's objective tpl mapping, so every inter picture's
        // quantizer grid -- and every decision priced against it -- moved
        // (8325 -> 8419 / 33014 -> 33029 on its own base); re-taken once more
        // at the merge of both lanes.
        // Re-taken on lane-arfq: the top ARF of a mini-GOP codes its
        // per-superblock `delta_qindex` at 1.5x the preset's strength
        // ([`crate::speed::DQ_LEVEL_K`]), so the hidden frames' quantizer
        // grids -- and every decision priced against them -- moved:
        // 8252 -> 8269 bytes at q=150 and 33053 -> 33044 at q=60.
        // Re-taken on lane-arfpred: the top ARF is coded from a temporally
        // FILTERED source ([`crate::speed::ARF_TF`]), so every hidden frame's
        // pixels -- and the leaves that predict from them -- moved:
        // 8269 -> 8218 bytes at q=150 and 33044 -> 33017 at q=60.
        // NOT re-taken on lane-b128: [`SB128_DEFAULT`] stayed off there, so
        // the shipped stream did not move.
        // Re-taken on lane-b128m, which flips [`SB128_DEFAULT`] on: every
        // picture is coded in 128x128 superblocks now, the inter ones
        // offering the 128 root's own `PARTITION_NONE` block -- 8327 bytes at
        // q=150 and 33194 at q=60. `EC_AV1_SB128=0` restores 8252 / 33053.
        // Re-taken at the merge of lane-b128m onto main (128 superblocks on
        // by default, on top of the ARF temporal filter).
        // Re-taken on lane-b128r: the 128 root now offers the COMPOUND
        // candidates the 64 root already did ([`b128_compound`]), so every
        // inter frame that carries `reference_select` codes different blocks
        // -- 8354 -> 8364 bytes at q=150 and 33222 -> 33257 at q=60.
        // `EC_AV1_B128COMP=0` restores 8354 / 33222 exactly.
        // Re-taken on lane-arftf: that filter's strength swept on the deciding
        // gate lands at 2, not 3 ([`crate::speed::ARF_TF`]), so the filtered
        // anchors' pixels moved once more: 8218 -> 8307 bytes at q=150 and
        // 33017 at q=60 (the same LENGTH, different bytes).
        // Re-taken at the merge of lane-arftf (ARF filter strength 2) and
        // lane-rlclamp onto the 128 compound default.
        // Re-taken on lane-b128hv: the 128 root now also offers PARTITION_HORZ
        // and PARTITION_VERT ([`b128_rect`]), two 128x64 / 64x128 skip blocks,
        // so every inter frame whose RD takes one codes a different partition
        // -- 8290 -> 8291 bytes at q=150. The q=60 pin does NOT move: at that
        // quantizer the rect cut never wins its own comparison on this clip.
        // `EC_AV1_B128HV=0` restores 8290 exactly.
        let pins: [(u8, usize, u64); 2] = [
            (150, 8291, 0x1f00bb0eb099a27f),
            (60, 33227, 0x57ee6b1f8eacd881),
        ];
        let coded: Vec<(u8, usize, u64)> = pins
            .iter()
            .map(|&(q, _, _)| {
                let e = encode_sequence(&source, q, 0.5).unwrap();
                (q, e.stream.len(), fnv(&e.stream))
            })
            .collect();
        crate::speed::set_speed(was);
        for (&(q, bytes, hash), &got) in pins.iter().zip(&coded) {
            assert_eq!(got, (q, bytes, hash), "q={q}: the encoder's stream moved");
        }
    }

    /// A ladder is only a ladder if both axes rise together.
    pub(super) fn assert_monotone(name: &str, ladder: &[(f64, f64)]) {
        assert_eq!(ladder.len(), 4, "{name}: wanted four quality points");
        for w in ladder.windows(2) {
            assert!(
                w[1].0 > w[0].0 && w[1].1 > w[0].1,
                "{name}: not monotone, {ladder:?}"
            );
        }
    }

    /// The sweep behind [`screen_content`]'s constants
    /// (`unswept-decision-constants`), on the SAME five clips
    /// [`bd_rate_screen_native`] keeps off (`native_gate_clips`) and at the
    /// same native crop: for each colour bound N the share of 16x16 luma
    /// blocks with 2..=N distinct colours, and the share of those whose
    /// per-pixel variance also clears a floor V -- libaom's
    /// `av1_set_screen_content_options` counts the variance-cleared blocks
    /// too (its `counts_2`, which gates intrabc), and without that term a
    /// smooth dark film frame reads as screen content.
    /// `cargo test -p ec-av1 --release --lib -- --ignored probe_screen_detect --nocapture`
    #[test]
    #[ignore = "reads the real library"]
    fn probe_screen_detect() {
        let _knobs = crate::speed::knob_read();
        if !have_ffmpeg() {
            eprintln!("SKIP probe_screen_detect: no ffmpeg");
            return;
        }
        let limits = [4usize, 8, 16, 32, 64, 128];
        let floors = [0u64, 4, 16, 64, 256];
        // The census of one clip at one size: mean share, over the frames, of
        // 16x16 blocks with 2..=N colours whose per-pixel variance is > V.
        let census = |label: &str, frames: &[Picture], cw: usize, ch: usize| {
            let mut acc = [[0f64; 5]; 6];
            for picture in frames {
                let y: Vec<u8> = picture.y.iter().map(|&v| v as u8).collect();
                let mut hit = [[0usize; 5]; 6];
                let mut blocks = 0usize;
                for by in (0..ch).step_by(16) {
                    for bx in (0..cw).step_by(16) {
                        let mut seen = [false; 256];
                        let (mut colors, mut sum, mut sq) = (0usize, 0u64, 0u64);
                        for row in 0..16 {
                            for col in 0..16 {
                                let v = y[(by + row) * cw + bx + col];
                                sum += u64::from(v);
                                sq += u64::from(v) * u64::from(v);
                                if !seen[usize::from(v)] {
                                    seen[usize::from(v)] = true;
                                    colors += 1;
                                }
                            }
                        }
                        let var = (sq - sum * sum / 256) / 256;
                        blocks += 1;
                        for (li, &limit) in limits.iter().enumerate() {
                            if colors > 1 && colors <= limit {
                                for (fi, &floor) in floors.iter().enumerate() {
                                    hit[li][fi] += usize::from(var > floor);
                                }
                            }
                        }
                    }
                }
                for li in 0..6 {
                    for fi in 0..5 {
                        acc[li][fi] += 100.0 * hit[li][fi] as f64 / blocks.max(1) as f64;
                    }
                }
            }
            let n = frames.len().max(1) as f64;
            eprintln!(
                "{label} {cw}x{ch}, {} frames -- share of 16x16 blocks with 2..=N colours and per-pixel var > V:",
                frames.len()
            );
            eprintln!(
                "  N \\ V |{}",
                floors.iter().map(|f| format!("{f:>8}")).collect::<String>()
            );
            for (li, &limit) in limits.iter().enumerate() {
                eprintln!(
                    "  {limit:>5} |{}",
                    (0..5)
                        .map(|fi| format!("{:>7.1}%", acc[li][fi] / n))
                        .collect::<String>()
                );
            }
        };
        for (name, path, seek) in native_gate_clips() {
            let Some((nw, nh)) = probe_dims(&path) else {
                eprintln!("SKIP {name}: ffprobe gave no size");
                continue;
            };
            let cw = nw.min(1920) / 128 * 128;
            let ch = nh.min(1024) / 128 * 128;
            let (x, y0) = (((nw - cw) / 2) & !1, ((nh - ch) / 2) & !1);
            let frames = clip_frames_vf(
                &path,
                &seek,
                &format!("crop={cw}:{ch}:{x}:{y0}"),
                cw,
                ch,
                12,
            );
            census(&name, &frames, cw, ch);
            // The 640x384 gate scales instead of cropping, and the scaling
            // changes the census (bars smooth out): the constants have to
            // separate at BOTH sizes.
            let small = clip_frames(&path, &seek, 640, 384, 12);
            census(&name, &small, 640, 384);
        }
    }

    /// The detector against the WHOLE real library (class
    /// `unswept-decision-constants`, user rule "his data is the gate"):
    /// every VIDEO row of `fixtures/real-library-manifest.tsv` -- plus the
    /// two colour-bar fixtures as anchors -- decoded at 10/50/90% of its
    /// duration, four frames each, 8-bit 4:2:0, at native size or a
    /// 1920x1080 centred crop when larger (the crop keeps a capture's exact
    /// repeats, which a downscale destroys -- class `gate-recipe-confound`).
    /// Prints per file and offset the share of qualifying 16x16 blocks, the
    /// [`screen_content`] verdict and a content hint (mean luma, luma
    /// standard deviation, inter MAD), then sweeps the colour bound against
    /// the variance floor over the whole library: for each (N, V) the worst
    /// (highest) share among the camera/film rows against the best (lowest)
    /// among the screen-recording rows, so the rule with the widest gap can
    /// be read off. Rows carry the manifest index, codec and size only --
    /// no titles, no home paths.
    /// `cargo test -p ec-av1 --release --lib -- --ignored probe_screen_library --nocapture`
    #[test]
    #[ignore = "reads the real library"]
    fn probe_screen_library() {
        let _knobs = crate::speed::knob_read();
        if !have_ffmpeg() {
            eprintln!("SKIP probe_screen_library: no ffmpeg");
            return;
        }
        // lane-av1clipprobe: the crate's one fixture root (EC_FIXTURES first).
        let fixtures = crate::library_fixture::root();
        let Ok(manifest) = std::fs::read_to_string(fixtures.join("real-library-manifest.tsv"))
        else {
            eprintln!("SKIP probe_screen_library: no real-library manifest");
            return;
        };
        let limits = [4usize, 8, 16, 32, 64];
        let floors = [0u64, 4, 8, 16, 32, 64, 128];
        // Whatever ffmpeg gave us, without the exact-count assertion the
        // gate loaders make: a seek near the end of a clip returns fewer
        // frames, and that is a row to print, not a panic.
        let decode =
            |clip: &str, seek: &str, vf: &str, w: usize, h: usize, n: usize| -> Vec<Picture> {
                let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
                    .args(["-v", "error", "-ss", seek, "-i", clip])
                    .args(["-frames:v", &n.to_string()])
                    .args(["-vf", vf])
                    .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"]), None, |c| c.output());
                let Ok(out) = out else { return Vec::new() };
                if !out.status.success() {
                    return Vec::new();
                }
                let (luma, chroma) = (w * h, w * h / 4);
                let frame_len = luma + 2 * chroma;
                (0..out.stdout.len() / frame_len)
                    .map(|i| {
                        let b = &out.stdout[i * frame_len..][..frame_len];
                        Picture {
                            width: w,
                            height: h,
                            y: b[..luma].iter().map(|&v| u16::from(v)).collect(),
                            u: b[luma..luma + chroma]
                                .iter()
                                .map(|&v| u16::from(v))
                                .collect(),
                            v: b[luma + chroma..].iter().map(|&v| u16::from(v)).collect(),
                        }
                    })
                    .collect()
            };
        // Share (%) of 16x16 blocks with 2..=N colours and per-pixel
        // variance > V, for every (N, V) of the sweep grid, over one frame.
        let census = |y: &[u8], w: usize, h: usize| -> [[f64; 7]; 5] {
            let mut hit = [[0usize; 7]; 5];
            let mut blocks = 0usize;
            for by in (0..h).step_by(16) {
                for bx in (0..w).step_by(16) {
                    let (bh, bw) = ((h - by).min(16), (w - bx).min(16));
                    let mut seen = [false; 256];
                    let (mut colors, mut sum, mut sq) = (0usize, 0u64, 0u64);
                    for row in 0..bh {
                        for col in 0..bw {
                            let v = y[(by + row) * w + bx + col];
                            sum += u64::from(v);
                            sq += u64::from(v) * u64::from(v);
                            if !seen[usize::from(v)] {
                                seen[usize::from(v)] = true;
                                colors += 1;
                            }
                        }
                    }
                    let n = (bh * bw) as u64;
                    let var = (sq - sum * sum / n) / n;
                    blocks += 1;
                    for (li, &limit) in limits.iter().enumerate() {
                        if colors > 1 && colors <= limit {
                            for (fi, &floor) in floors.iter().enumerate() {
                                hit[li][fi] += usize::from(var > floor);
                            }
                        }
                    }
                }
            }
            let mut out = [[0f64; 7]; 5];
            for li in 0..5 {
                for fi in 0..7 {
                    out[li][fi] = 100.0 * hit[li][fi] as f64 / blocks.max(1) as f64;
                }
            }
            out
        };
        // (index, label, path, duration, screen-by-its-own-content). The
        // OBS directory and the OBS file-name shape ("YYYY-MM-DD HH-MM-SS")
        // are desktop recordings; everything else in this library is camera
        // or film material.
        let mut rows: Vec<(String, String, String, f64, bool)> = Vec::new();
        for (label, file) in [
            ("bars 1080p", "h264-1080p-23.976-8bit.mp4"),
            ("bars 2160p", "h264-2160p-23.976-8bit.mp4"),
        ] {
            let path = fixtures.join("video").join(file);
            if path.exists() {
                rows.push((
                    "fx".into(),
                    label.into(),
                    path.to_str().unwrap().into(),
                    10.0,
                    false,
                ));
            }
        }
        for (i, line) in manifest.lines().skip(1).enumerate() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 10 {
                continue;
            }
            let (path, codec) = (f[0], f[2]);
            if codec == "-" || codec.is_empty() {
                continue;
            }
            let (Ok(w), Ok(h)) = (f[3].parse::<usize>(), f[4].parse::<usize>()) else {
                eprintln!("SKIP {}: manifest carries no coded size", i + 1);
                continue;
            };
            if !std::path::Path::new(path).exists() {
                eprintln!("SKIP {}: file is gone", i + 1);
                continue;
            }
            let name = path.rsplit('/').next().unwrap_or(path);
            let is_capture = path.contains("/OBS/")
                || (name.len() > 19
                    && name.as_bytes()[4] == b'-'
                    && name.as_bytes()[7] == b'-'
                    && name.as_bytes()[10] == b' '
                    && name.as_bytes()[13] == b'-');
            rows.push((
                format!("{}", i + 1),
                format!("{codec} {w}x{h}"),
                path.to_string(),
                f[8].parse::<f64>().unwrap_or(0.0),
                is_capture,
            ));
        }
        // Per (N, V): the worst camera/film row and the best screen row, so
        // the separation of the whole library is one subtraction.
        let (mut cam_max, mut scr_min) = ([[0f64; 7]; 5], [[100f64; 7]; 5]);
        eprintln!(
            "| idx | codec size | class | offset | qualifying blocks | verdict | mean/sd/MAD |"
        );
        for (idx, label, path, dur, is_capture) in &rows {
            let Some((nw, nh)) = probe_dims(path) else {
                eprintln!("SKIP {idx}: ffprobe gave no size");
                continue;
            };
            let (cw, ch) = (nw.min(1920) & !1, nh.min(1080) & !1);
            let (x, y0) = (((nw - cw) / 2) & !1, ((nh - ch) / 2) & !1);
            let vf = format!("crop={cw}:{ch}:{x}:{y0}");
            let mut file_grid = [[0f64; 7]; 5];
            let mut file_frames = 0usize;
            for pct in [10u32, 50, 90] {
                let seek = format!("{:.3}", dur * f64::from(pct) / 100.0);
                let frames = decode(path, &seek, &vf, cw, ch, 4);
                if frames.is_empty() {
                    eprintln!(
                        "| {idx} | {label} | {} | {pct}% | SKIP: ffmpeg decoded no frame here |",
                        if *is_capture { "screen" } else { "camera" }
                    );
                    continue;
                }
                let (mut share, mut yes, mut mean, mut sd, mut mad) =
                    (0f64, 0usize, 0f64, 0f64, 0f64);
                let mut prev: Option<Vec<u8>> = None;
                for picture in &frames {
                    let y: Vec<u8> = picture.y.iter().map(|&v| v as u8).collect();
                    let grid = census(&y, cw, ch);
                    for li in 0..5 {
                        for fi in 0..7 {
                            file_grid[li][fi] += grid[li][fi];
                        }
                    }
                    file_frames += 1;
                    // The shipped rule is (16 colours, var > 16): grid row 2,
                    // floor 16 is column 3.
                    share += grid[2][3];
                    yes += usize::from(screen_content(&y, cw, cw, ch));
                    let n = y.len() as f64;
                    let sum: f64 = y.iter().map(|&v| f64::from(v)).sum();
                    let sq: f64 = y.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
                    mean += sum / n;
                    sd += (sq / n - (sum / n) * (sum / n)).max(0.0).sqrt();
                    if let Some(p) = &prev {
                        mad += p
                            .iter()
                            .zip(&y)
                            .map(|(&a, &b)| f64::from(a.abs_diff(b)))
                            .sum::<f64>()
                            / n;
                    }
                    prev = Some(y);
                }
                let f = frames.len() as f64;
                eprintln!(
                    "| {idx} | {label} | {} | {pct}% | {:.1}% | {} ({yes}/{} frames) | {:.0}/{:.1}/{:.2} |",
                    if *is_capture { "screen" } else { "camera" },
                    share / f,
                    if yes * 2 > frames.len() {
                        "SCREEN"
                    } else {
                        "camera"
                    },
                    frames.len(),
                    mean / f,
                    sd / f,
                    mad / (f - 1.0).max(1.0),
                );
            }
            if file_frames == 0 {
                continue;
            }
            for li in 0..5 {
                for fi in 0..7 {
                    let s = file_grid[li][fi] / file_frames as f64;
                    match is_capture {
                        true => scr_min[li][fi] = scr_min[li][fi].min(s),
                        false => cam_max[li][fi] = cam_max[li][fi].max(s),
                    }
                }
            }
        }
        eprintln!(
            "sweep over the whole library -- worst camera/film share vs best screen share, per (N colours, var > V):"
        );
        eprintln!(
            "  N \\ V |{}",
            floors
                .iter()
                .map(|f| format!("{:>18}", format!("V>{f}")))
                .collect::<String>()
        );
        let (mut best, mut best_rule) = (f64::MIN, (16usize, 16u64));
        for (li, &limit) in limits.iter().enumerate() {
            let mut row = String::new();
            for fi in 0..7 {
                let (c, s) = (cam_max[li][fi], scr_min[li][fi]);
                row.push_str(&format!("{:>18}", format!("{c:.1}/{s:.1} {:+.1}", s - c)));
                if s - c > best {
                    best = s - c;
                    best_rule = (limit, floors[fi]);
                }
            }
            eprintln!("  {limit:>5} |{row}");
        }
        // The threshold sits at the midpoint of the widest gap; the shipped
        // constant is its reciprocal in whole parts of the frame.
        let (li, fi) = (
            limits.iter().position(|&l| l == best_rule.0).unwrap(),
            floors.iter().position(|&f| f == best_rule.1).unwrap(),
        );
        let mid = (cam_max[li][fi] + scr_min[li][fi]) / 2.0;
        eprintln!(
            "widest split: {} colours, var > {}, gap {:+.1} points (camera <= {:.1}%, screen >= {:.1}%); threshold {:.1}% = EC_AV1_SCREEN_PCT {}",
            best_rule.0,
            best_rule.1,
            best,
            cam_max[li][fi],
            scr_min[li][fi],
            mid,
            (100.0 / mid.max(0.1)).round() as usize,
        );
    }

    /// The missing instrument: what this encoder costs against libaom and
    /// rav1e at matched fidelity, and how long it takes to get there.
    ///
    /// Recipe, identical on all three sides: 12 consecutive frames of each
    /// clip scaled to 640x384 (a whole number of superblocks), one key frame
    /// then 11 inter frames (`-g 12`, matching our own sequence shape), one
    /// tile, `-threads 1`, no film grain synthesis (neither reference enables
    /// it by default), 4:2:0 8-bit in and out. Reference points:
    ///   libaom-av1: `-cpu-used 6 -b:v 0 -crf {5,20,35,45}`
    ///   librav1e:   `-rav1e-params speed=6:quantizer={50,100,150,200}:
    ///                tile_cols=1:tile_rows=1:threads=1`
    /// ours: `base_q_idx {60,90,120,150}`. Every PSNR is all-plane, on
    /// ffmpeg-decoded frames of each encoder's own stream.
    ///
    /// Measured 2026-09-06 after the in-loop filters (lane-av1filt: per-frame
    /// deblocking levels and CDEF strengths, both chosen by re-decoding the
    /// coded tile -- `crate::filter_search`), 12 frames, this box (wall = the
    /// four encodes of that ladder together):
    ///
    /// | clip | ours (q 150/120/90/60) | BD-rate vs libaom | vs rav1e | wall ours:aom:rav1e |
    /// |---|---|---|---|---|
    /// | 1080p fixture | 42.11 dB/17774 B .. 50.99 dB/76838 B | +143.6% | +94.0% | 9.2s:1.1s:2.6s |
    /// | 2160p fixture | 42.84 dB/12480 B .. 51.43 dB/46557 B | +179.5% | +128.8% | 8.6s:1.0s:2.2s |
    /// | screen capture | 39.76 dB/9536 B .. 48.87 dB/29189 B | +88.0% | +21.5% | 7.2s:1.0s:2.1s |
    ///
    /// Re-measured 2026-09-06 after lane-av1fwd, which changed only how long
    /// the encoder takes -- every stream it writes is byte-identical, so the
    /// BD columns are this box's own re-run of the same recipe rather than a
    /// quality move (+141.3/+174.2/+88.9 vs libaom, +91.9/+124.5/+21.6 vs
    /// rav1e). What moved is the wall of the four encodes:
    ///
    /// | clip | wall ours before | after | instructions:u before -> after |
    /// |---|---|---|---|
    /// | 1080p fixture | 9.2s | 7.2s | 191.5G -> 139.8G (-27.0%) |
    /// | 2160p fixture | 8.6s | 6.9s | 183.5G -> 136.4G (-25.6%) |
    /// | screen capture | 7.2s | 6.7s | 157.2G -> 122.5G (-22.1%) |
    ///
    /// Three exact steps, none of which touches a coded bit: the coefficient
    /// rate pricer stopped rebuilding every CDF table per candidate
    /// (`tile::coeff_bits`), the filter search stopped re-decoding its own
    /// tile once per candidate (`decode::FilterReplay`), and the forward
    /// transform stopped transforming the all-zero residual that 70% of its
    /// calls carry.
    ///
    /// The three steps of that lane, same recipe (vs libaom, then vs rav1e):
    ///
    /// | step | 1080p | 2160p | screen |
    /// |---|---|---|---|
    /// | no in-loop filter | +234.0 / +171.9 | +260.3 / +203.2 | +318.5 / +155.3 |
    /// | + deblocking | +198.2 / +143.1 | +218.1 / +162.4 | +136.9 / +52.7 |
    /// | + CDEF | +143.6 / +94.0 | +179.5 / +128.8 | +88.0 / +21.5 |
    ///
    /// Re-measured for the record 2026-09-07 (lane-av1resweep), after
    /// [`LAMBDA_SCALE`] moved 0.05 -> 0.0275 and the two var-tx depth
    /// searches ([`tx32_depth_search`], [`compound_var_tx`]) became defaults
    /// -- all three decided on the REAL film rows of
    /// [`bd_rate_screen_native`], which this downscaled recipe does not
    /// carry:
    ///
    /// | clip | BD vs libaom | BD vs rav1e | wall ours:libaom:rav1e |
    /// |---|---|---|---|
    /// | bars 1080p | +68.8% | +26.2% | 5.8s:1.2s:2.7s |
    /// | bars 2160p | +81.6% | +41.5% | 4.8s:1.0s:2.2s |
    /// | screen capture | +49.1% | -4.3% | 5.5s:1.0s:2.1s |
    ///
    /// RE-RECORDED 2026-09-07 (lane-av1pyrdef), with the coding pyramid the
    /// DEFAULT of the sequence path this gate codes through -- the bars are
    /// non-screen so they take it, the capture is gated flat:
    ///
    /// | clip | effective pyramid | BD vs libaom | BD vs rav1e | wall |
    /// |---|---|---|---|---|
    /// | bars 1080p | 4:-16:8 | +84.7% | +39.9% | 5.1s:1.1s:2.6s |
    /// | bars 2160p | 4:-16:8 | +124.4% | +75.2% | 4.4s:1.0s:2.8s |
    /// | screen capture | none (gated) | +49.1% | -4.3% | 6.5s:1.2s:2.3s |
    ///
    /// Those are, to the byte, the numbers lane-av1pyrgate recorded for the
    /// same clips under `EC_AV1_PYRAMID=4:-16:8 EC_AV1_GATE_FACADE=1` -- the
    /// facade arm -- which is what "one mini-GOP driver, two entry points"
    /// means here. This downscaled recipe HATES the pyramid (as it hates
    /// every knob the real film rows of `bd_rate_screen_native` keep, which
    /// is why it decides nothing); `EC_AV1_PYRAMID=0` restores the flat rows
    /// above. The capture's row is the flat row to the byte in both runs --
    /// that equality IS the content gate.
    ///
    /// Loop restoration is NOT in yet: it is the one filter whose parameters
    /// are per restoration UNIT inside the tile payload, so it needs both
    /// tile writers to code `lr` syntax and an encoder-visible post-CDEF
    /// buffer to solve against -- neither of which this frame-header-level
    /// search reaches.
    ///
    /// The baseline it replaced, before an inter frame's blocks chose their
    /// own partition, was +501.4/+504.4/+440.9 against libaom and
    /// +364.4/+401.0/+206.0 against rav1e at 5.1/4.6/5.9 s.
    ///
    /// So: roughly 2.5-3x libaom's rate and 1.5-2x rav1e's at matched
    /// fidelity, while spending far more time than either (the partition
    /// search's own cost, uncut: see the two intra-pruning variants measured
    /// and rejected in the lane report). No
    /// threshold is asserted yet -- this run sets the baseline; the gate
    /// asserts four monotone points per ladder, and at EVERY ladder point of
    /// EVERY clip it decodes our stream twice -- with ffmpeg and with our own
    /// `crate::stream::decode_stream` -- and asserts the encoder's
    /// reconstruction, ffmpeg's decode and our decode sample-exact in Y, U
    /// and V over every shown frame in display order. A failure names the
    /// clip, the q, the frame, the plane and the first differing sample.
    /// (Under `EC_AV1_PYRAMID` the frames come out of the streaming facade,
    /// which exposes no per-packet reconstruction, so that path asserts the
    /// two decoders against each other on all three planes.)
    ///
    /// Run:
    ///     cargo test -p ec-av1 --release --lib -- --ignored \
    ///         bd_rate_vs_libaom_and_rav1e --nocapture
    #[test]
    #[ignore = "the encoder BD-rate baseline: minutes per clip, needs ffmpeg"]
    fn bd_rate_vs_libaom_and_rav1e() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP bd_rate_vs_libaom_and_rav1e: no ffmpeg");
            return;
        }
        // lane-av1clipprobe: the crate's one fixture root (EC_FIXTURES first).
        let fixtures = crate::library_fixture::root();
        let mut clips: Vec<(String, std::path::PathBuf)> = Vec::new();
        // testsrc2 COLOUR BARS, not film -- labelled so no reader takes these
        // rows for real content (the real rows live in the native gate).
        for (label, name) in [
            ("bars 1080p", "h264-1080p-23.976-8bit.mp4"),
            ("bars 2160p", "h264-2160p-23.976-8bit.mp4"),
        ] {
            // lane-av1clipprobe: the one probe, so a missing clip is RED under
            // the require envs instead of a drop-with-no-escape.
            if let Some(path) = crate::library_fixture::require(
                &format!("video/{name}"),
                &format!("bd_rate_vs_libaom_and_rav1e: {label}"),
            ) {
                clips.push((label.to_string(), path));
            }
        }
        // Screen capture: the repo carries no screen-capture fixture, so take
        // the first OBS recording the real-library manifest names.
        match std::fs::read_to_string(fixtures.join("real-library-manifest.tsv")) {
            Ok(manifest) => {
                let screen = manifest
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split('\t').next())
                    .find(|p| {
                        p.contains("/OBS/")
                            && p.ends_with(".mkv")
                            && std::path::Path::new(p).exists()
                    });
                match screen {
                    Some(p) => clips.push((
                        format!("screen capture ({})", p.rsplit('/').next().unwrap_or(p)),
                        std::path::PathBuf::from(p),
                    )),
                    None => {
                        eprintln!("SKIP screen capture: no OBS recording in the manifest exists")
                    }
                }
            }
            Err(e) => eprintln!("SKIP screen capture: no real-library manifest ({e})"),
        }
        assert!(!clips.is_empty(), "no source clip available");

        let (width, height, frames) = (640usize, 384usize, 12usize);
        let aom_points: Vec<Vec<String>> = [5, 20, 35, 45]
            .iter()
            .map(|q| {
                ["-cpu-used", "6", "-b:v", "0", "-crf", &q.to_string()]
                    .map(String::from)
                    .to_vec()
            })
            .collect();
        let rav1e_points: Vec<Vec<String>> = [50, 100, 150, 200]
            .iter()
            .map(|q| {
                vec![
                    "-rav1e-params".to_string(),
                    format!("speed=6:quantizer={q}:tile_cols=1:tile_rows=1:threads=1"),
                ]
            })
            .collect();
        println!(
            "\n{frames} frames of each clip at {width}x{height}, gop={frames}, 1 tile, 1 thread"
        );
        println!(
            "| clip | ours PSNR/bytes per point | BD-rate vs libaom | BD-rate vs rav1e | wall ours:libaom:rav1e |"
        );
        println!("|---|---|---|---|---|");
        for (name, path) in &clips {
            let fctx = &crate::decode::FrameCtx::for_encoder();
            let source = clip_frames(path.to_str().unwrap(), "0", width, height, frames);
            let _ = take_partition_hits();
            let _ = crate::tile::take_palette_hits();
            SCREEN_FRAMES.iter().for_each(|c| {
                c.store(0, std::sync::atomic::Ordering::Relaxed);
            });
            let _ = take_uv_mode_hits();
            let _ = crate::tile::take_inter_mode_hits();
            let _ = crate::tile::take_drl_hits();
            let _ = crate::tile::take_ref_hits();
            let _ = crate::tile::take_compound_mode_hits();
            let _ = crate::tile::take_compound_pair_hits();
            let _ = crate::tile::take_compound_size_hits();
            let _ = crate::tile::take_compound_leaf_mode_hits();
            let _ = take_b64_root_hits();
            let _ = take_b64_residual_hits();
            let _ = take_b64_compound_hits();
            let _ = crate::motion::take_census();
            // The entry-surface arm: run the SAME ladder through the
            // streaming facade the editor's export calls, which must print
            // the same BD numbers as the sequence path -- byte for byte the
            // same stream now that both drive one mini-GOP driver
            // (lane-av1pyrdef).
            let facade = std::env::var_os("EC_AV1_GATE_FACADE").is_some();
            let (ours, ours_wall) = match facade {
                false => our_ladder(name, &source, width, height, fctx),
                true => {
                    let pyramid = crate::encoder::Pyramid::from_env();
                    let (ladder, wall, counts, bytes, effective) =
                        our_ladder_facade(name, &source, width, height, pyramid);
                    eprintln!(
                        "{name}: facade, pyramid requested {pyramid:?} effective \
                         {effective:?} -- frames key {} arf {} leaf {} \
                         show_existing {}; bytes key {} arf {} leaf {} show_existing {}",
                        counts[0],
                        counts[1],
                        counts[2],
                        counts[3],
                        bytes[0],
                        bytes[1],
                        bytes[2],
                        bytes[3],
                    );
                    (ladder, wall)
                }
            };
            // How often the inter split actually fired over this clip's four
            // encodes, against the whole 32x32 that was the only outcome
            // before (gate-blind-to-feature).
            let [whole32, split32, whole16, split16] = take_partition_hits();
            // Which inter modes actually fired over this clip's four encodes,
            // and at which DRL index -- a mode nobody picks is a mode the BD
            // number cannot be crediting (gate-blind-to-feature).
            let [nearest, near, global, new] = crate::tile::take_inter_mode_hits();
            let [drl0, drl1, drl2, drl3] = crate::tile::take_drl_hits();
            let refs = crate::tile::take_ref_hits();
            let modes_total = (nearest + near + global + new).max(1);
            eprintln!(
                "{name}: inter modes NEAREST {nearest} ({:.1}%) NEAR {near} ({:.1}%) \
                 GLOBAL {global} ({:.1}%) NEW {new} ({:.1}%); drl idx 0/1/2/3+ \
                 {drl0}/{drl1}/{drl2}/{drl3}",
                100.0 * nearest as f64 / modes_total as f64,
                100.0 * near as f64 / modes_total as f64,
                100.0 * global as f64 / modes_total as f64,
                100.0 * new as f64 / modes_total as f64,
            );
            // The compound census: what share of the coded inter blocks took
            // a compound reference at all, which compound mode they took, and
            // which pair (gate-blind-to-feature -- a compound mode nobody
            // picks cannot be what a BD number credits).
            // The 64x64 root's own fire shares (lane-b64/tx64/b64b): how many
            // superblocks were left whole at all, and how many of those coded
            // a residual or took a compound reference.
            let (roots, root_res, root_comp) = (
                take_b64_root_hits(),
                take_b64_residual_hits(),
                take_b64_compound_hits(),
            );
            eprintln!(
                "{name}: 64x64 roots {roots} (with a residual {root_res}, compound {root_comp})"
            );
            let comp_modes = crate::tile::take_compound_mode_hits();
            let comp_pairs = crate::tile::take_compound_pair_hits();
            let comp_sizes = crate::tile::take_compound_size_hits();
            let comp_leaf = crate::tile::take_compound_leaf_mode_hits();
            let comp_total: usize = comp_modes.iter().sum();
            eprintln!(
                "{name}: compound {comp_total} of {} inter blocks ({:.1}%); modes \
                 NEAREST_NEAREST {} NEAR_NEAR {} NEAREST_NEW {} NEW_NEAREST {} \
                 GLOBAL_GLOBAL {} NEW_NEW {}; pairs LAST+GOLDEN {} LAST+ALTREF {}",
                modes_total + comp_total,
                100.0 * comp_total as f64 / (modes_total + comp_total) as f64,
                comp_modes[0],
                comp_modes[1],
                comp_modes[2],
                comp_modes[3],
                comp_modes[6],
                comp_modes[7],
                comp_pairs[3],
                comp_pairs[6],
            );
            eprintln!(
                "{name}: leaf (16x16 and below) compound modes NEAREST_NEAREST {} \
                 NEAR_NEAR {} NEAREST_NEW {} NEW_NEAREST {} GLOBAL_GLOBAL {} NEW_NEW {}",
                comp_leaf[0], comp_leaf[1], comp_leaf[2], comp_leaf[3], comp_leaf[6], comp_leaf[7],
            );
            eprintln!(
                "{name}: compound by size 8x8 {} 16x16 {} 32x32 {}",
                comp_sizes[1], comp_sizes[2], comp_sizes[3],
            );
            // Where the wall-owning stage actually goes: how many candidate
            // evaluations (one `mc::predict` + SAD each) a search spends, how
            // many of them are subpel, and how often the winner is the seed
            // it started from -- the census the seeded search is judged by.
            let [
                calls,
                evals,
                subpel,
                rounds,
                near_pred,
                unmoved,
                widest,
                widest_moves,
            ] = crate::motion::take_census();
            let per = |n: u64| n as f64 / calls.max(1) as f64;
            eprintln!(
                "{name}: motion search {calls} calls, {evals} candidate evals ({:.1}/call, \
                 {:.1}% of them subpel), {rounds} integer rounds ({:.2}/call); winner within \
                 +-1 pel of pred_mv {:.1}%, winner = starting seed {:.1}%",
                per(evals),
                100.0 * subpel as f64 / evals.max(1) as f64,
                per(rounds),
                100.0 * near_pred as f64 / calls.max(1) as f64,
                100.0 * unmoved as f64 / calls.max(1) as f64,
            );
            // lane-newmv, class `instrument at bound`: how often the winner
            // was walked to at the log stage's own WIDEST step -- a high
            // share means the search stops where its step lets it, not where
            // the content is.
            eprintln!(
                "{name}: motion search moved at the widest step in {:.1}% of calls                  ({:.2} such moves/call)",
                100.0 * widest as f64 / calls.max(1) as f64,
                per(widest_moves),
            );
            eprintln!(
                "{name}: references LAST {} GOLDEN {} (LAST2 {} LAST3 {} BWD {} ALT2 {} ALT {})",
                refs[0], refs[3], refs[1], refs[2], refs[4], refs[5], refs[6]
            );
            eprintln!(
                "{name}: inter 32x32 split into 16s {split32} of {} blocks ({:.1}%); \
                 16x16 leaves split into 8s {split16} of {} ({:.1}%)",
                whole32 + split32,
                100.0 * split32 as f64 / (whole32 + split32).max(1) as f64,
                whole16 + split16,
                100.0 * split16 as f64 / (whole16 + split16).max(1) as f64
            );
            // The deblocking level each of this clip's frames chose
            // (lane-av1filt): 0 everywhere would mean the filter never fires.
            let levels = crate::filter_search::take_chosen_levels();
            type Chosen = ((u8, u8), (u8, u8), (u8, u8));
            let mean = |f: fn(&Chosen) -> u8| {
                levels.iter().map(|l| f(l) as f64).sum::<f64>() / levels.len().max(1) as f64
            };
            eprintln!(
                "{name}: over {} frames -- deblock luma mean {:.1} chroma {:.1}; \
                 cdef luma pri {:.1}/sec {:.1}, chroma pri {:.1}/sec {:.1}",
                levels.len(),
                mean(|l| l.0.0),
                mean(|l| l.0.1),
                mean(|l| l.1.0),
                mean(|l| l.1.1),
                mean(|l| l.2.0),
                mean(|l| l.2.1),
            );
            // How many strength presets each frame's CDEF list carried and
            // how the 64x64 units spread over them (lane-av1lr): `bits 0`
            // everywhere would mean the per-unit choice never fires.
            let presets = crate::filter_search::take_cdef_presets();
            let mut by_bits = [0usize; 4];
            for (b, _) in &presets {
                by_bits[usize::from(*b)] += 1;
            }
            let units: usize = presets.iter().flat_map(|(_, c)| c).sum();
            let on_extra: usize = presets.iter().flat_map(|(_, c)| c.iter().skip(1)).sum();
            eprintln!(
                "{name}: cdef_bits frames {}/{}/{}/{} (0/1/2/3); {} of {} 64x64 units \
                 on a non-default preset ({:.1}%)",
                by_bits[0],
                by_bits[1],
                by_bits[2],
                by_bits[3],
                on_extra,
                units,
                100.0 * on_extra as f64 / units.max(1) as f64
            );
            // How the 64x64 luma restoration units split between
            // RESTORE_NONE and Wiener (lane-av1lr).
            let (lr_none, lr_wiener) = crate::filter_search::take_lr_histogram();
            eprintln!(
                "{name}: lr units none={lr_none} wiener={lr_wiener} ({:.1}% restored)",
                100.0 * lr_wiener as f64 / (lr_none + lr_wiener).max(1) as f64
            );
            // Which chroma modes the search actually wins with, against the
            // unconditional DC_PRED that was the only outcome before
            // (gate-blind-to-feature).
            // Screen-content detection and the palette it turns on
            // (gate-blind-to-feature): how many frames set
            // `allow_screen_content_tools`, how many blocks took a palette,
            // and of what size. A film clip must print `screen frames on=0`.
            let screen_on = SCREEN_FRAMES[1].load(std::sync::atomic::Ordering::Relaxed);
            let screen_off = SCREEN_FRAMES[0].load(std::sync::atomic::Ordering::Relaxed);
            // Which tables the SEARCH priced against, per frame arming
            // (`tile::arm_pricing_cdfs`): the default tables or the frame's
            // real starting ones. A film clip must show every inter arming on
            // the real side, a screen clip none of them.
            let (price_default, price_real) = crate::tile::take_pricing_hits();
            eprintln!("{name}: pricer armings default={price_default} real={price_real}");
            let palette = crate::tile::take_palette_hits();
            eprintln!(
                "{name}: screen frames on={screen_on} off={screen_off}; palette blocks {} sizes {}",
                palette[0],
                (2..=8)
                    .filter(|&n| palette[n] > 0)
                    .map(|n| format!("{n}={}", palette[n]))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            // The CfL alpha histogram (lane-av1cfl): one entry per chroma
            // plane of every `UV_CFL_PRED` block, by |alpha_q3|. A tool that
            // only ever fires at one magnitude is a search that never left
            // its first candidate (gate-blind-to-feature).
            // The luma angle-delta histogram (lane-av1cfl): how many
            // directional blocks won at each delta. All-zero would mean the
            // refinement never beat the base angle.
            let deltas = take_angle_delta_hits();
            if deltas.iter().sum::<usize>() > 0 {
                eprintln!(
                    "{name}: angle_delta_y {}",
                    deltas
                        .iter()
                        .enumerate()
                        .map(|(d, &n)| format!("{:+}={n}", d as i32 - 3))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            let alphas = take_cfl_alpha_hits();
            if alphas.iter().sum::<usize>() > 0 {
                eprintln!(
                    "{name}: cfl |alpha| {}",
                    alphas
                        .iter()
                        .enumerate()
                        .filter(|&(_, &n)| n > 0)
                        .map(|(a, &n)| format!("{a}={n}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            let palette_uv = crate::tile::take_palette_uv_hits();
            eprintln!(
                "{name}: chroma palette blocks {} sizes {}",
                palette_uv[0],
                (2..=8)
                    .filter(|&n| palette_uv[n] > 0)
                    .map(|n| format!("{n}={}", palette_uv[n]))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let uv = take_uv_mode_hits();
            let uv_total: usize = uv.iter().sum();
            eprintln!(
                "{name}: uv modes {}",
                uv.iter()
                    .enumerate()
                    .filter(|&(_, &n)| n > 0)
                    .map(|(m, &n)| format!(
                        "{}={n} ({:.1}%)",
                        UV_MODE_NAMES[m],
                        100.0 * n as f64 / uv_total.max(1) as f64
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            // Which transform depth the intra blocks of this clip's key
            // frames resolved to (lane-av1tx): depth 0 is one transform over
            // the whole block, 1 and 2 its halvings.
            let depths = take_tx_depth_hits();
            let depth_total: usize = depths.iter().sum();
            eprintln!(
                "{name}: intra tx depths {}",
                depths
                    .iter()
                    .enumerate()
                    .map(|(d, &n)| format!(
                        "{d}={n} ({:.1}%)",
                        100.0 * n as f64 / depth_total.max(1) as f64
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            print_inter_tx_census(name);
            // lane-av1obmc: how many blocks actually CODED a motion_mode
            // symbol and which value they took, by footprint -- "OBMC is on"
            // as a measurement rather than a claim (class
            // `gate-blind-to-feature`).
            let mm = crate::tile::take_motion_mode_hits();
            eprintln!(
                "{name}: motion_mode 32x32 SIMPLE={} OBMC={} WARP={} | 16x16 SIMPLE={} OBMC={} \
                 WARP={} | 8x8 SIMPLE={} OBMC={} WARP={} ({:.1}% OBMC / {:.1}% WARP of {} \
                 eligible)",
                mm[0],
                mm[1],
                mm[2],
                mm[3],
                mm[4],
                mm[5],
                mm[6],
                mm[7],
                mm[8],
                100.0 * (mm[1] + mm[4] + mm[7]) as f64 / mm.iter().sum::<usize>().max(1) as f64,
                100.0 * (mm[2] + mm[5] + mm[8]) as f64 / mm.iter().sum::<usize>().max(1) as f64,
                mm.iter().sum::<usize>(),
            );
            let (aom, aom_wall) =
                external_ladder(&source, width, height, "libaom-av1", &aom_points);
            let (rav1e, rav1e_wall) =
                external_ladder(&source, width, height, "librav1e", &rav1e_points);
            assert_monotone(&format!("{name}: ours"), &ours);
            assert_monotone(&format!("{name}: libaom"), &aom);
            assert_monotone(&format!("{name}: rav1e"), &rav1e);
            let points = ours
                .iter()
                .map(|(p, b)| format!("{p:.2} dB/{:.0} B", 10f64.powf(*b)))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "| {name} | {points} | {:+.1}% | {:+.1}% | {ours_wall:.1}s:{aom_wall:.1}s:{rav1e_wall:.1}s |",
                bd_rate(&aom, &ours) * 100.0,
                bd_rate(&rav1e, &ours) * 100.0,
            );
            println!(
                "|   references | libaom {} | rav1e {} | | |",
                aom.iter()
                    .map(|(p, b)| format!("{p:.2} dB/{:.0} B", 10f64.powf(*b)))
                    .collect::<Vec<_>>()
                    .join(", "),
                rav1e
                    .iter()
                    .map(|(p, b)| format!("{p:.2} dB/{:.0} B", 10f64.powf(*b)))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        assert_ladder_decodes();
    }
    /// Which var-tx depth this clip's INTER blocks resolved to (lane-av1tx2,
    /// split by block class in lane-av1txdepth): depth 0 is one transform over
    /// the whole block, 1 its four halves; the classes are the 16x16/8x8
    /// leaves and the 32x32 root, each single-reference or compound. A class
    /// with no counts at all never reached `commit_inter_luma` -- which is
    /// what its knob being off looks like (class `gate-blind-to-feature`).
    pub(super) fn print_inter_tx_census(name: &str) {
        let splits = take_inter_tx_split_hits();
        let total: usize = splits.iter().sum();
        let row = |label: &str, base: usize| {
            let (n0, n1) = (splits[base], splits[base + 1]);
            format!(
                "{label} 0={n0} 1={n1} ({:.1}% split)",
                100.0 * n1 as f64 / (n0 + n1).max(1) as f64
            )
        };
        eprintln!(
            "{name}: inter var-tx depths of {total} blocks -- {} | {} | {} | {}",
            row("leaf single", 0),
            row("leaf compound", 2),
            row("32x32 single", 4),
            row("32x32 compound", 6),
        );
        // lane-txrd: which types the two searches actually COMMITTED over
        // this row's four encodes -- the gate could not say whether a type
        // search fired at all (class `gate-blind-to-feature`).
        const NAMES: [&str; 16] = [
            "IDTX",
            "DCT_DCT",
            "ADST_ADST",
            "ADST_DCT",
            "DCT_ADST",
            "V_DCT",
            "H_DCT",
            "FLIPADST_DCT",
            "DCT_FLIPADST",
            "FLIPADST_FLIPADST",
            "ADST_FLIPADST",
            "FLIPADST_ADST",
            "V_ADST",
            "H_ADST",
            "V_FLIPADST",
            "H_FLIPADST",
        ];
        let name_of = |t: usize| NAMES[t];
        for (label, hits) in [
            ("inter", take_inter_tx_type_hits()),
            ("intra", take_tx_type_hits()),
        ] {
            let total: usize = hits.iter().sum();
            let live: Vec<String> = hits
                .iter()
                .enumerate()
                .filter(|&(_, &n)| n > 0)
                .map(|(t, &n)| {
                    format!(
                        "{}={n} ({:.1}%)",
                        name_of(t),
                        100.0 * n as f64 / total.max(1) as f64
                    )
                })
                .collect();
            eprintln!(
                "{name}: {label} tx_type of {total} units -- {}",
                live.join(" ")
            );
        }
        let t = take_txrd_stats();
        if t[0] > 0 {
            eprintln!(
                "{name}: txrd flat decisions={} chosen sse={} bits={:.0} | DCT_DCT sse={} \
                 bits={:.0} (dsse {:+.2}%, dbits {:+.2}%)",
                t[0],
                t[1],
                t[3] as f64 / 64.0,
                t[2],
                t[4] as f64 / 64.0,
                100.0 * (t[1] as f64 - t[2] as f64) / t[2].max(1) as f64,
                100.0 * (t[3] as f64 - t[4] as f64) / t[4].max(1) as f64,
            );
            eprintln!(
                "{name}: txrd DCT_DCT re-predicted HERE sse={} bits={:.0} (vs the passed-in \
                 flat: dsse {:+.2}%, dbits {:+.2}%)",
                t[5],
                t[6] as f64 / 64.0,
                100.0 * (t[5] as f64 - t[2] as f64) / t[2].max(1) as f64,
                100.0 * (t[6] as f64 - t[4] as f64) / t[4].max(1) as f64,
            );
        }
    }

    /// The video stream's coded size, from ffprobe.
    pub(super) fn probe_dims(clip: &str) -> Option<(usize, usize)> {
        crate::probe::dims(clip)
    }

    /// `EC_AV1_NATIVE_CROP=<width>x<height>`: the native gate's window at a
    /// size of the reader's choosing instead of [`crate::probe::gate_crop`]'s
    /// superblock-aligned one, still centred and still even for 4:2:0.
    ///
    /// WHY IT EXISTS (lane-sse): `gate_crop` rounds DOWN to whole 128-wide
    /// superblocks by design, so no default gate row has a block the frame
    /// edge cuts through -- while every arbitrary export size does (3840x1608
    /// is 25.1 superblock rows, 1920x1080 is 8.4). `EC_AV1_NATIVE_CROP=
    /// 1920x1080` is the straddling arm the edge-clipped distortion score is
    /// read on.
    pub(super) fn native_crop_override(clip: &str) -> Option<(String, usize, usize)> {
        let spec = std::env::var("EC_AV1_NATIVE_CROP").ok()?;
        let (w, h) = spec
            .split_once('x')
            .expect("EC_AV1_NATIVE_CROP is <width>x<height>");
        let (cw, ch): (usize, usize) = (
            w.parse().expect("crop width"),
            h.parse().expect("crop height"),
        );
        let (nw, nh) = crate::probe::dims(clip)?;
        assert!(cw <= nw && ch <= nh, "{cw}x{ch} does not fit in {nw}x{nh}");
        let (x, y) = (((nw - cw) / 2) & !1, ((nh - ch) / 2) & !1);
        Some((format!("crop={cw}:{ch}:{x}:{y}"), cw, ch))
    }

    /// The rows of [`bd_rate_screen_native`] -- (label, path, seek) -- shared
    /// with `probe_screen_detect` so the detector sweep measures exactly the
    /// clips the keep table is read off. Selector env vars as documented on
    /// the gate; a missing clip prints a SKIP and drops its row.
    pub(super) fn native_gate_clips() -> Vec<(String, String, String)> {
        // lane-av1clipprobe: the crate's one fixture root (EC_FIXTURES first).
        let fixtures = crate::library_fixture::root();
        let on = |k: &str| std::env::var(k).ok().as_deref() == Some("1");
        let (film, film4k, screen) = (
            on("EC_AV1_NATIVE_FILM"),
            on("EC_AV1_NATIVE_FILM4K"),
            on("EC_AV1_NATIVE_SCREEN"),
        );
        // No selector at all means every clip; any selector means that subset.
        let all = !(film || film4k || screen);
        // (label, path, seek). The two `testsrc2` fixtures are COLOUR BARS,
        // not film -- they print as "bars" so no reader takes them for real
        // content -- and the two rows below them are the real AV1 films.
        let mut clips: Vec<(String, String, String)> = Vec::new();
        for (want, label, file) in [
            (all || film, "bars 1080p", "h264-1080p-23.976-8bit.mp4"),
            (all || film4k, "bars 2160p", "h264-2160p-23.976-8bit.mp4"),
        ] {
            if !want {
                continue;
            }
            // lane-av1clipprobe: the one probe, so a missing clip is RED under
            // the require envs instead of a dropped row with no escape.
            if let Some(path) = crate::library_fixture::require(
                &format!("video/{file}"),
                &format!("the native BD-rate gate: {label}"),
            ) {
                clips.push((
                    label.to_string(),
                    path.to_str().unwrap().to_string(),
                    "0".into(),
                ));
            }
        }
        let manifest = std::fs::read_to_string(fixtures.join("real-library-manifest.tsv"));
        // The real films, taken out of the real-library manifest by codec and
        // coded width so no title and no home path lives in this file. Each
        // seek is PINNED (class `seeded fixture not reproducible`) at a
        // moving, non-black window -- a black leader would make every arm
        // read +0.000 (class `flat sample window`). Both sources are 10-bit
        // (one HDR10/PQ); the loader's plain `-pix_fmt yuv420p` conversion is
        // what all three encoders see, so the comparison stays
        // encoder-vs-encoder.
        let pick = |width: &str| -> Option<String> {
            let m = manifest.as_ref().ok()?;
            m.lines()
                .skip(1)
                .map(|l| l.split('\t').collect::<Vec<_>>())
                .find(|f| {
                    f.len() > 4
                        && f[2] == "av1"
                        && f[3] == width
                        && std::path::Path::new(f[0]).exists()
                })
                .map(|f| f[0].to_string())
        };
        for (want, label, width, seek) in [
            (all || film, "film A (1080p source)", "1920", "00:35:00"),
            (
                all || film4k,
                "film B (2160p HDR source)",
                "3840",
                "00:40:00",
            ),
        ] {
            if !want {
                continue;
            }
            match pick(width) {
                Some(path) => clips.push((label.to_string(), path, seek.to_string())),
                None => eprintln!("SKIP {label}: no {width}-wide AV1 source in the manifest"),
            }
        }
        if all || screen {
            match &manifest {
                Ok(manifest) => match manifest
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split('\t').next())
                    .find(|p| {
                        p.contains("/OBS/")
                            && p.ends_with(".mkv")
                            && std::path::Path::new(p).exists()
                    }) {
                    Some(p) => clips.push((
                        format!("screen capture ({})", p.rsplit('/').next().unwrap_or(p)),
                        p.to_string(),
                        "0".into(),
                    )),
                    None => {
                        eprintln!("SKIP screen capture: no OBS recording in the manifest exists")
                    }
                },
                Err(e) => eprintln!("SKIP screen capture: no real-library manifest ({e})"),
            }
        }
        clips
    }

    /// The NATIVE-resolution arm of the BD gate (all three clips). The default gate
    /// downscales the OBS capture to 640x384, which destroys exactly the
    /// pixel-identical 16x16 repeats intrabc and (part of) the palette exist
    /// for (class `gate-recipe-confound`), so every screen-tool decision taken
    /// on that recipe is taken on content the tool cannot serve. This arm
    /// crops -- never scales -- a superblock-aligned window out of the middle
    /// of the capture at its own resolution (at most 1920x1024, so the wall
    /// stays in the minutes) and runs the same 12-frame, four-quantizer
    /// ladder against libaom `cpu-used 6` and rav1e `speed=6`, one tile, one
    /// thread, with the same three-way (encoder / ffmpeg / our decoder)
    /// sample-exactness assertion `our_ladder` makes.
    ///
    /// It is its own `--ignored` test so the default gate's wall does not
    /// grow. Every variant is an environment knob, and each of those is a
    /// `OnceLock` read once per process, so a variant is a separate run:
    ///
    ///     cargo test -p ec-av1 --release --lib -- --ignored \
    ///         bd_rate_screen_native --nocapture           # baseline
    ///     EC_AV1_INTRABC=1 ... same command                # intra block copy
    ///     EC_AV1_PAL_MAXCOLORS=256 ... same command        # palette bound
    ///     EC_AV1_TILES=1:1 EC_AV1_TILE_THREADS=4 ...       # 2x2 tiles
    ///     EC_AV1_PYRAMID=0 ... same command                # no coding pyramid
    ///     EC_AV1_GOP_LAYOUT=fixed ... same command          # no tail absorbing
    ///     EC_AV1_GATE_FACADE=1 ... same command            # the facade arm
    ///
    /// The coding pyramid is the DEFAULT of the sequence path this arm codes
    /// through (lane-av1pyrdef); the run prints the requested and the
    /// effective pyramid per clip, and a screen clip's effective one is
    /// `None`. `EC_AV1_GATE_FACADE=1` runs the same ladder through the
    /// streaming facade instead, which prints the per-level frame and byte
    /// census and asserts ffmpeg's decode against our own decoder on all
    /// three planes in display order; both arms code the same bytes
    /// (`encoder::tests::the_facade_codes_the_same_bytes_as_encode_sequence`).
    ///
    /// WHICH ROWS ARE REAL CONTENT: the `bars 1080p` / `bars 2160p` rows are
    /// the repo fixtures `scripts/gen-fixtures.sh` builds with ffmpeg
    /// `testsrc2` -- COLOUR BARS, ~90% of whose 16x16 cells have no intra
    /// cost -- so every "film" column in the historical tables of this file
    /// is that generator, not film. The real content is `film A`, `film B`
    /// (native crops out of the two AV1 films the real-library manifest
    /// names, at a pinned seek; added 2026-09-07) and the screen capture. A
    /// decision only the bars rows support is not supported.
    ///
    /// Measured 2026-09-07, the five rows before and after the screen
    /// detector grew libaom's variance term ([`screen_var`]) -- before it,
    /// BOTH real films were classified as screen content on 48/48 frames, so
    /// their coefficients were priced against the DEFAULT CDFs and the
    /// palette search ran over film; after, both are non-screen (0/48) and
    /// the capture is unchanged at 48/48:
    ///
    /// | clip | vs libaom before -> after | vs rav1e before -> after | wall ours |
    /// |---|---|---|---|
    /// | bars 1080p | +15.6% -> +15.6% | -1.7% -> -1.7% | 37.9s |
    /// | bars 2160p | +46.5% -> +46.5% | +18.8% -> +18.8% | 33.9s |
    /// | film A | +69.5% -> +69.0% | +37.7% -> +37.5% | 72s -> 64.4s |
    /// | film B | +102.4% -> +100.0% | +63.4% -> +61.5% | 72s -> 67.9s |
    /// | screen capture | +51.0% -> +51.0% | -14.5% -> -14.5% | 27.5s |
    ///
    /// Both bars rows and the capture are byte-identical across that change
    /// (their classification never moved); the palette search over film was
    /// costing roughly a tenth of the film rows' wall.
    ///
    /// CONFIRM RUN 2026-09-07 (lane-av1pyrdef), with the pyramid now the
    /// sequence path's default -- the four non-screen rows take `4:-16:8`,
    /// the capture is gated flat and does not move a byte:
    ///
    /// | clip | effective | flat -> default | vs rav1e | wall ours:libaom:rav1e |
    /// |---|---|---|---|---|
    /// | bars 1080p | 4:-16:8 | +16.4% -> +21.5% | -1.1% -> +3.7% | 37.8s:8.4s:16.4s |
    /// | bars 2160p | 4:-16:8 | +48.2% -> +48.3% | +20.4% -> +20.4% | 32.2s:5.6s:16.2s |
    /// | film A | 4:-16:8 | +62.8% -> +54.1% | +32.5% -> +24.2% | 58.9s:9.5s:13.6s |
    /// | film B | 4:-16:8 | +86.3% -> +82.6% | +50.5% -> +47.4% | 63.2s:18.7s:16.4s |
    /// | screen capture | none (gated) | +50.1% -> +50.1% | -15.4% -> -15.4% | 31.1s:8.4s:11.3s |
    ///
    /// Every row lands on lane-av1pyrgate's facade-arm number to the digit,
    /// which is the point of the shared driver: the two entry points code one
    /// stream. The bars rows are fixtures (recorded, never a decision); the
    /// two real films are what the pyramid ships for.
    ///
    /// All five gate clips are rows by default. `EC_AV1_NATIVE_FILM=1`,
    /// `EC_AV1_NATIVE_FILM4K=1` and `EC_AV1_NATIVE_SCREEN=1` select a subset:
    /// setting any one of them keeps only the selected rows (the two 1080p
    /// rows, the two 2160p rows, the capture row respectively). A missing
    /// real film SKIPs its row; it never fails the gate.
    #[test]
    #[ignore = "the native-resolution BD arm: minutes per row, needs ffmpeg"]
    fn bd_rate_screen_native() {
        let _knobs = crate::speed::knob_read();
        native_bd_arm("bd_rate_screen_native", 12, false);
    }

    /// The LONG-GOP arm of the native BD gate: the same recipe as
    /// [`bd_rate_screen_native`] over the two real film rows only, but 48
    /// pictures with `gop = 48` (`encode_sequence` sets `gop =
    /// pictures.len()`, and `external_ladder` passes ffmpeg `-g 48`, which
    /// reaches libaom as `--kf-max-dist=48` and rav1e as
    /// `key_frame_interval=48`), so every encoder sees one key frame and 47
    /// inter pictures.
    ///
    /// WHY IT EXISTS: `bd_rate_screen_native` codes 12 frames with `gop =
    /// 12`, and `Av1Encoder::encode_frames` cuts a mini-GOP at every key
    /// frame (`encoder.rs`), so ANY [`crate::encoder::Pyramid::mini_gop`]
    /// above 12 codes the same stream there -- the shipped `16` was measured
    /// as "one hidden ARF per 12-picture GOP", never as a real 16-picture
    /// group (class `instrument at bound` / `gate blind to feature`). The
    /// user's exports are long GOPs, so the pyramid shape is decided here.
    ///
    ///     cargo test -p ec-av1 --release --lib -- --ignored \
    ///         bd_rate_film_long_gop --nocapture
    ///
    /// `EC_AV1_NATIVE_FILM=1` keeps only film A, `EC_AV1_NATIVE_FILM4K=1`
    /// only film B; `EC_AV1_PYRAMID=<mini_gop>:<arf>:<leaf>` picks the shape.
    /// Read every row off the log's own "pyramid requested/effective" print,
    /// never off the argument order of the runs (lane-pyr3 misattributed
    /// three rows that way).
    ///
    /// Table: `lanes/pyr4.sweep.txt`.
    #[test]
    #[ignore = "the long-GOP native BD arm: ~7 minutes per film row, needs ffmpeg"]
    fn bd_rate_film_long_gop() {
        let _knobs = crate::speed::knob_write();
        native_bd_arm("bd_rate_film_long_gop", 48, true);
    }

    /// The shared driver of the two native BD arms above: `frames` pictures
    /// per row (the sequence's `gop` is the same number, so one key frame),
    /// four quantizers, `films_only` dropping every row but the two real
    /// films.
    pub(super) fn native_bd_arm(label: &str, frames: usize, films_only: bool) {
        if !have_ffmpeg() {
            eprintln!("SKIP {label}: no ffmpeg");
            return;
        }
        let mut clips = native_gate_clips();
        if films_only {
            clips.retain(|(name, _, _)| name.starts_with("film "));
        }
        if clips.is_empty() {
            eprintln!("SKIP {label}: no clip");
            return;
        }

        let aom_points: Vec<Vec<String>> = [5, 20, 35, 45]
            .iter()
            .map(|q| {
                ["-cpu-used", "6", "-b:v", "0", "-crf", &q.to_string()]
                    .map(String::from)
                    .to_vec()
            })
            .collect();
        let rav1e_points: Vec<Vec<String>> = [50, 100, 150, 200]
            .iter()
            .map(|q| {
                vec![
                    "-rav1e-params".to_string(),
                    format!("speed=6:quantizer={q}:tile_cols=1:tile_rows=1:threads=1"),
                ]
            })
            .collect();
        println!(
            "\nnative crop, {frames} frames, gop={frames}; tiles {:?}, tile threads {}, \
             intrabc {}, palette maxcolors {}",
            armed_tiles(),
            crate::par::tile_threads(),
            intrabc_enabled(),
            palette_max_colors(),
        );
        println!(
            "| clip | ours PSNR/bytes per point | BD-rate vs libaom | BD-rate vs rav1e | wall ours:libaom:rav1e (noisy) |"
        );
        println!("|---|---|---|---|---|");
        for (name, path, seek) in &clips {
            let fctx = &crate::decode::FrameCtx::for_encoder();
            if probe_dims(path).is_none() {
                eprintln!("SKIP {name}: ffprobe gave no size");
                continue;
            }
            // A whole number of 128-wide superblocks, at most 1920x1024, out
            // of the middle of the frame; the offsets stay even for 4:2:0.
            // ONE definition, shared with `examples/enc_probe` so the probe
            // cannot measure a different window than this gate (lane-probe).
            let (vf, cw, ch) = match native_crop_override(path) {
                Some(over) => over,
                None => crate::probe::gate_crop(path),
            };
            let source = clip_frames_vf(path, seek, &vf, cw, ch, frames);
            SCREEN_FRAMES.iter().for_each(|c| {
                c.store(0, std::sync::atomic::Ordering::Relaxed);
            });
            let _ = crate::tile::take_palette_hits();
            let _ = crate::tile::take_palette_uv_hits();
            let _ = crate::tile::take_intrabc_hits();
            let _ = take_intrabc_search();
            let _ = crate::tile::take_filter_intra_hits();
            let _ = take_edge_census();
            // The sequence path itself codes the pyramid now
            // (lane-av1pyrdef), and prints what it requested and what the
            // content gate left effective.
            let (ours, ours_wall) = our_ladder(name, &source, cw, ch, fctx);
            // lane-sse: the blocks the true frame edge cuts through, and how
            // much of their RAW squared error sat in the padding the encoder
            // invented past it -- what the inside-only score stopped ranking
            // on. Empty on a superblock-aligned crop, which is every default
            // gate row (class `gate-blind-to-feature`).
            if edge_census() {
                for (side, n, inside, padding) in take_edge_census() {
                    eprintln!(
                        "{name}: edge {side}x{side} scores={n} inside_sse={inside} \
                         padding_sse={padding} ({:.1}% of raw)",
                        100.0 * padding as f64 / (inside + padding).max(1) as f64,
                    );
                }
            }
            // gate-blind-to-feature: what the screen tools actually did at
            // native resolution, over this clip's four encodes.
            let screen_on = SCREEN_FRAMES[1].load(std::sync::atomic::Ordering::Relaxed);
            let screen_off = SCREEN_FRAMES[0].load(std::sync::atomic::Ordering::Relaxed);
            // Which tables the SEARCH priced against, per frame arming
            // (`tile::arm_pricing_cdfs`): the default tables or the frame's
            // real starting ones. A film clip must show every inter arming on
            // the real side, a screen clip none of them.
            let (price_default, price_real) = crate::tile::take_pricing_hits();
            eprintln!("{name}: pricer armings default={price_default} real={price_real}");
            // The 64x64 root's own fire shares (lane-b64/tx64/b64b): how many
            // superblocks stayed whole, and how many of those coded a
            // residual, split that residual's luma, or took a compound
            // reference (class `gate-blind-to-feature`).
            // lane-i64: the KEY frame's own 64x64 intra root -- how many of
            // the superblocks offered the trial were coded whole.
            let [i64_whole, i64_offered, i64_edge] = take_i64_root_hits();
            eprintln!(
                "{name}: key 64x64 intra roots {i64_whole} of {i64_offered} offered ({:.1}%), \
                 {i64_edge} on a superblock the frame edge cuts",
                100.0 * i64_whole as f64 / i64_offered.max(1) as f64,
            );
            eprintln!(
                "{name}: 128x128 roots {} (search) / {} (writer), with a residual \
                 {} (search) / {} (writer); rect/AB pieces with a residual \
                 {} (search) / {} (writer)",
                take_b128_none_hits(),
                crate::tile::take_sb128_none_hits(),
                take_b128_residual_hits(),
                crate::tile::take_sb128_residual_hits(),
                take_b128_rectres_hits(),
                crate::tile::take_sb128_rectres_hits(),
            );
            eprintln!(
                "{name}: 64x64 roots {} (with a residual {}, split luma {}, compound {})",
                take_b64_root_hits(),
                take_b64_residual_hits(),
                take_b64_var_tx_hits(),
                take_b64_compound_hits(),
            );
            // lane-rdoq, under `EC_AV1_RDOQ_CENSUS=1` only (it costs a scan
            // of every priced block): what the deadzone quantiser handed the
            // RDOQ pass and what the pass changed, over every TRIAL block of
            // this row's four encodes -- not over the coded stream, so it is
            // the bits AT STAKE in the search, not in the bitstream.
            let rq = crate::tile::take_rdoq_stats();
            if rq[0] > 0 {
                eprintln!(
                    "{name}: rdoq blocks={} coeffs={} (+-1 {:.1}%, trailing +-1 {:.1}%) \
                     zeroed={} lowered={} trial bits {:.0} -> {:.0} ({:+.2}%)",
                    rq[0],
                    rq[1],
                    100.0 * rq[2] as f64 / rq[1].max(1) as f64,
                    100.0 * rq[3] as f64 / rq[1].max(1) as f64,
                    rq[4],
                    rq[5],
                    rq[7] as f64 / 64.0,
                    rq[6] as f64 / 64.0,
                    100.0 * (rq[6] as f64 - rq[7] as f64) / rq[7].max(1) as f64,
                );
            }
            // lane-av1obmc2: the same motion_mode census the 640x384 gate
            // prints (class `gate-blind-to-feature`) -- how many blocks coded
            // the symbol at each footprint and how many took OBMC. Without it
            // the native table, which is the standing keep table, could not
            // say whether the tool fired at all.
            let mm = crate::tile::take_motion_mode_hits();
            eprintln!(
                "{name}: motion_mode 32x32 SIMPLE={} OBMC={} WARP={} | 16x16 SIMPLE={} OBMC={} \
                 WARP={} | 8x8 SIMPLE={} OBMC={} WARP={} ({:.1}% OBMC / {:.1}% WARP of {} \
                 eligible)",
                mm[0],
                mm[1],
                mm[2],
                mm[3],
                mm[4],
                mm[5],
                mm[6],
                mm[7],
                mm[8],
                100.0 * (mm[1] + mm[4] + mm[7]) as f64 / mm.iter().sum::<usize>().max(1) as f64,
                100.0 * (mm[2] + mm[5] + mm[8]) as f64 / mm.iter().sum::<usize>().max(1) as f64,
                mm.iter().sum::<usize>(),
            );
            print_inter_tx_census(name);
            // lane-last2 (class `gate-blind-to-feature`): which reference the
            // 32x32-and-below inter blocks of this clip's four encodes chose.
            // The native table is the standing keep table, so a second past
            // reference that never won a block has to be visible HERE and not
            // only in a synthetic witness.
            let rh = crate::encode::take_ref_frame_hits();
            let rtot: usize = rh.iter().sum();
            eprintln!(
                "{name}: references LAST {} LAST2 {} GOLDEN {} ALTREF {} of {} inter blocks \
                 (LAST2 {:.1}%)",
                rh[1],
                rh[2],
                rh[4],
                rh[7],
                rtot,
                100.0 * rh[2] as f64 / rtot.max(1) as f64,
            );
            let palette = crate::tile::take_palette_hits();
            let palette_uv = crate::tile::take_palette_uv_hits();
            let ibc = crate::tile::take_intrabc_hits();
            let search = take_intrabc_search();
            eprintln!(
                "{name}: screen frames on={screen_on} off={screen_off} ({:.1}% on); \
                 palette blocks luma {} chroma {}; intrabc blocks {} (searched {} / valid {} / \
                 won {})",
                100.0 * screen_on as f64 / (screen_on + screen_off).max(1) as f64,
                palette[0],
                palette_uv[0],
                ibc[0],
                search[0],
                search[1],
                search[2],
            );
            // gate-blind-to-feature: how often the five recursive filter-intra
            // modes actually won a block over this clip's four encodes -- a
            // tool that never fires is not measured by the row above it.
            let fi = crate::tile::take_filter_intra_hits();
            eprintln!(
                "{name}: filter intra on={} blocks {} (modes DC={} V={} H={} D157={} PAETH={})",
                filter_intra_on(),
                fi[0],
                fi[1],
                fi[2],
                fi[3],
                fi[4],
                fi[5],
            );
            let (aom, aom_wall) = external_ladder(&source, cw, ch, "libaom-av1", &aom_points);
            let (rav1e, rav1e_wall) = external_ladder(&source, cw, ch, "librav1e", &rav1e_points);
            assert_monotone(&format!("{name}: ours"), &ours);
            assert_monotone(&format!("{name}: libaom"), &aom);
            assert_monotone(&format!("{name}: rav1e"), &rav1e);
            let points = ours
                .iter()
                .map(|(p, b)| format!("{p:.2} dB/{:.0} B", 10f64.powf(*b)))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "| {name} {cw}x{ch} | {points} | {:+.1}% | {:+.1}% | {ours_wall:.1}s:{aom_wall:.1}s:{rav1e_wall:.1}s |",
                bd_rate(&aom, &ours) * 100.0,
                bd_rate(&rav1e, &ours) * 100.0,
            );
            // lane-pareto2: the two references at THEIR fast presets, plus
            // SVT-AV1, measured in the SAME arm as the row above -- the only
            // way the wall column of a Pareto table means anything (class:
            // wall tables are comparable only inside one interleaved batch).
            // `EC_AV1_PARETO_REFS=1` only: it roughly doubles the arm.
            if std::env::var("EC_AV1_PARETO_REFS").ok().as_deref() == Some("1") {
                let rav1e_at = |sp: u32| -> Vec<Vec<String>> {
                    [50, 100, 150, 200]
                        .iter()
                        .map(|q| {
                            vec![
                                "-rav1e-params".to_string(),
                                format!(
                                    "speed={sp}:quantizer={q}:tile_cols=1:tile_rows=1:threads=1"
                                ),
                            ]
                        })
                        .collect()
                };
                let aom_at = |cu: u32| -> Vec<Vec<String>> {
                    [5, 20, 35, 45]
                        .iter()
                        .map(|q| {
                            vec![
                                "-cpu-used".to_string(),
                                cu.to_string(),
                                "-b:v".to_string(),
                                "0".to_string(),
                                "-crf".to_string(),
                                q.to_string(),
                            ]
                        })
                        .collect()
                };
                let svt_at = |pr: u32| -> Vec<Vec<String>> {
                    [20, 35, 45, 55]
                        .iter()
                        .map(|q| {
                            vec![
                                "-preset".to_string(),
                                pr.to_string(),
                                "-crf".to_string(),
                                q.to_string(),
                                "-svtav1-params".to_string(),
                                "lp=1".to_string(),
                            ]
                        })
                        .collect()
                };
                let mut arms: Vec<(String, &str, Vec<Vec<String>>)> = vec![
                    ("rav1e speed 8".into(), "librav1e", rav1e_at(8)),
                    ("rav1e speed 10".into(), "librav1e", rav1e_at(10)),
                    ("libaom cpu-used 8".into(), "libaom-av1", aom_at(8)),
                ];
                if have_encoder("libsvtav1") {
                    arms.push(("svt-av1 preset 8".into(), "libsvtav1", svt_at(8)));
                    arms.push(("svt-av1 preset 10".into(), "libsvtav1", svt_at(10)));
                } else {
                    eprintln!("SKIP the SVT-AV1 rows: this ffmpeg has no libsvtav1");
                }
                for (label, encoder, points) in arms {
                    let (lad, wall) = external_ladder(&source, cw, ch, encoder, &points);
                    // A REFERENCE stream our decoder cannot read is a note in
                    // this table, not a gate failure: these rows exist to
                    // price the references, and `assert_ladder_decodes` keeps
                    // its teeth for the two ladders the keep table is read off.
                    let notes =
                        std::mem::take(&mut *LADDER_FAILURES.lock().expect("ladder failures"));
                    if !notes.is_empty() {
                        eprintln!(
                            "{name} {label}: {} point(s) our decoder could not read (reference row, not a gate failure)",
                            notes.len()
                        );
                    }
                    assert_monotone(&format!("{name}: {label}"), &lad);
                    println!(
                        "| {name} {cw}x{ch} / {label} | -- | {:+.1}% | {:+.1}% | {wall:.1}s (ours {ours_wall:.1}s, rav1e s6 {rav1e_wall:.1}s) |",
                        bd_rate(&aom, &lad) * 100.0,
                        bd_rate(&rav1e, &lad) * 100.0,
                    );
                }
            }
        }
        assert_ladder_decodes();
    }

    /// lane-av1tpl3 step 1: the SHAPE of the propagating map's denominator
    /// ([`super::tpl_intra_costs`], the per-16x16 MAD from the cell's own DC)
    /// on the two film clips at their native gate crop. The 2160p clip loses
    /// at every arm of the propagating map and the named suspect was that its
    /// heavily filtered flat cells give a tiny denominator, so the dependence
    /// ratio explodes exactly where coding is cheap; this prints the numbers
    /// that confirm or refute it.
    #[test]
    // Reads ONLY `fixtures/video/h264-{1080p,2160p}-23.976-8bit.mp4` -- the
    // COLOUR-BARS fixtures, in a centred crop. It never opens a film, so the
    // old reason ("the film fixtures") sent readers looking for content the
    // test cannot read. 0.9 s; prints the per-cell intra-MAD distribution.
    #[ignore = "0.9 s; reads only the fixtures/video COLOUR-BARS clips (not the films) and prints the per-cell intra-MAD distribution"]
    pub(super) fn tpl_intra_denominator_histogram() {
        let _knobs = crate::speed::knob_read();
        if !have_ffmpeg() {
            eprintln!("SKIP tpl_intra_denominator_histogram: no ffmpeg");
            return;
        }
        for (label, file) in [
            ("bars 1080p", "h264-1080p-23.976-8bit.mp4"),
            ("bars 2160p", "h264-2160p-23.976-8bit.mp4"),
        ] {
            // lane-av1clipprobe: the one probe, so a missing clip is RED under
            // the require envs. With both clips absent this histogram used to
            // loop zero times and pass having printed nothing.
            let Some(path) = crate::library_fixture::require(
                &format!("video/{file}"),
                &format!("tpl_intra_denominator_histogram: {label}"),
            ) else {
                continue;
            };
            let path = path.to_str().unwrap();
            let Some((nw, nh)) = probe_dims(path) else {
                eprintln!("SKIP {label}: ffprobe gave no size");
                continue;
            };
            let (cw, ch) = (nw.min(1920) / 128 * 128, nh.min(1024) / 128 * 128);
            let (x, y) = (((nw - cw) / 2) & !1, ((nh - ch) / 2) & !1);
            let source = clip_frames_vf(path, "0", &format!("crop={cw}:{ch}:{x}:{y}"), cw, ch, 8);
            let mut costs: Vec<f64> = Vec::new();
            for pic in &source {
                let y8: Vec<u8> = pic.y.iter().map(|&v| v as u8).collect();
                costs.extend(
                    super::tpl_intra_costs(&y8, cw, ch)
                        .iter()
                        .map(|&c| f64::from(c)),
                );
            }
            let mean = costs.iter().sum::<f64>() / costs.len() as f64;
            let mut sorted = costs.clone();
            sorted.sort_by(f64::total_cmp);
            let q = |f: f64| sorted[((sorted.len() - 1) as f64 * f) as usize] / mean;
            let below = |f: f64| costs.iter().filter(|&&c| c < f * mean).count();
            eprintln!(
                "{label} {cw}x{ch}: {} cells over 8 frames, mean intra MAD {mean:.0}; \
                 as a fraction of the mean p0={:.3} p1={:.3} p5={:.3} p25={:.3} p50={:.3} \
                 p75={:.3} p99={:.3} max={:.3}; below 0.25x {} ({:.1}%), below 0.5x {} ({:.1}%), \
                 below 1.0x {} ({:.1}%)",
                costs.len(),
                q(0.0),
                q(0.01),
                q(0.05),
                q(0.25),
                q(0.50),
                q(0.75),
                q(0.99),
                q(1.0),
                below(0.25),
                100.0 * below(0.25) as f64 / costs.len() as f64,
                below(0.5),
                100.0 * below(0.5) as f64 / costs.len() as f64,
                below(1.0),
                100.0 * below(1.0) as f64 / costs.len() as f64,
            );
        }
    }

    /// The probe arms the gate's source (lane-probe, class `instrument
    /// disagrees with the gate`). `examples/enc_probe` built its own ffmpeg
    /// command line and had no `-ss`, so it read film B from frame 0 -- a
    /// black leader -- and coded 59 kB where the gate's own ladder point
    /// coded 89 kB of the same title; every census taken through it was
    /// probe-relative. Both now call [`crate::probe::source`]; this pins that
    /// the shared loader honours the seek (arming the probe's way and the
    /// gate's way gives byte-identical streams, and dropping the seek does
    /// not), on a synthetic clip whose content moves over time.
    #[test]
    fn the_probe_arms_the_gates_source() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP the_probe_arms_the_gates_source: no ffmpeg");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ec-av1-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("testsrc2.y4m");
        // 384x202: neither side is a multiple of 128, so the gate's centre
        // window is a real derivation and not the frame itself -- exactly the
        // shape film B has (3840x1608, never 3840x2160).
        let ok = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=384x202:rate=10",
            ])
            .args(["-frames:v", "20", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("ffmpeg failed to run")
            .success();
        assert!(ok, "ffmpeg could not write the synthetic clip");
        let path = clip.to_str().unwrap();

        // The window: superblock-aligned, centred, even offsets.
        let (vf, w, h) = crate::probe::gate_crop(path);
        assert_eq!((vf.as_str(), w, h), ("crop=384:128:0:36", 384, 128));

        let (seek, q) = ("00:00:01", 150u8);
        let gate = clip_frames_vf(path, seek, &vf, w, h, 6);
        let probe = crate::probe::source(path, seek, &vf, w, h, 6);
        assert!(
            gate.iter()
                .zip(&probe)
                .all(|(a, b)| a.y == b.y && a.u == b.u && a.v == b.v),
            "the probe's source differs from the gate's at the same window"
        );
        let bytes = |src: &[Picture]| {
            encode_sequence_with_ctx(src, q, 0.5, &crate::decode::FrameCtx::for_encoder())
                .unwrap()
                .stream
        };
        assert_eq!(
            bytes(&gate),
            bytes(&probe),
            "probe and gate code different bytes"
        );
        // The two witnesses that both halves of the arming reach ffmpeg: the
        // seek, and the crop's own offset (the film-B defect in miniature --
        // the probe was reading `crop=..:568` of a 1608-high frame).
        let unseeked = crate::probe::source(path, "0", &vf, w, h, 6);
        assert_ne!(
            bytes(&gate),
            bytes(&unseeked),
            "seeked and unseeked arming coded the same bytes -- the seek is inert"
        );
        let offset = crate::probe::source(path, seek, "crop=384:128:0:0", w, h, 6);
        assert_ne!(
            bytes(&gate),
            bytes(&offset),
            "centred and top-aligned windows coded the same bytes -- the crop is inert"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// lane-probe diagnostic: the gate's own loader + ladder call for ONE
    /// point inside the test binary, to compare against `examples/enc_probe`.
    #[test]
    // Panics on the first `env::var(...).unwrap()` when unset -- it needs FIVE
    // variables, not "a clip": `EC_PARITY_DIMS`, `EC_PARITY_FRAMES`,
    // `EC_PARITY_Q`, `EC_PARITY_CLIP`, `EC_PARITY_SS`, `EC_PARITY_VF`. Prints
    // one PARITY line (bytes + fnv1a of the stream) to diff against
    // `examples/enc_probe`.
    #[ignore = "diagnostic: needs EC_PARITY_{DIMS,FRAMES,Q,CLIP,SS,VF} -- all five, it unwraps the first missing one; prints one PARITY hash line"]
    pub(super) fn probe_parity_point() {
        let _knobs = crate::speed::knob_write();
        let g = |k: &str| std::env::var(k).unwrap();
        let (w, h) = g("EC_PARITY_DIMS")
            .split_once(':')
            .map(|(a, b)| (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()))
            .unwrap();
        let frames: usize = g("EC_PARITY_FRAMES").parse().unwrap();
        let q: u8 = g("EC_PARITY_Q").parse().unwrap();
        let source = clip_frames_vf(
            &g("EC_PARITY_CLIP"),
            &g("EC_PARITY_SS"),
            &g("EC_PARITY_VF"),
            w,
            h,
            frames,
        );
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let e = encode_sequence_with_ctx(&source, q, 0.5, fctx).unwrap();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in &e.stream {
            hash = (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        eprintln!(
            "PARITY q={q} bytes={} fnv1a={hash:016x} speed={} pyramid {:?} effective {:?}",
            e.stream.len(),
            crate::speed::speed(),
            crate::encoder::Pyramid::from_env(),
            last_sequence_pyramid()
        );
    }
    /// lane-refs/lane-last2: IS A SECOND PAST REFERENCE WORTH RETAINING? The
    /// lever this census priced SHIPS since lane-last2 (merge `321b2dc1`):
    /// `EC_AV1_LAST2` (default OFF) retains a second past leaf picture in
    /// `LEAF_SLOTS`, names it `ref_frame_idx[1]`, and offers it to the 32x32
    /// search with its own `NEWMV` (`EC_AV1_LAST2_NEW`); the ARF-level lags
    /// the census's far row measures were already banked by `arf_altref`.
    /// This census stays the PRICING arm (class `gate-blind-to-feature`): its
    /// best-of-two SAD shares are read against the fire share the gate census
    /// prints (`references ... LAST2 n (n%)`) and the refutation in
    /// `lanes/last2.report.md` §4 -- a pure-SAD best-of-two over-counts the
    /// lever by more than an order of magnitude once NEAR/NEAREST/DRL off
    /// `LAST`, the backward `ALTREF`, and the rate term price in.
    ///
    /// The measurement is on the gate's own window and loader (`gate_crop`,
    /// 12 pictures) of the two real films: every 16x16 luma block of every
    /// picture gets one full-pel diamond search against the NEAR reference
    /// and the same search against the FAR one, at the two lags the pyramid
    /// actually offers -- (1, 2) for the leaf chain and (4, 8) for the ARF
    /// level, rav1e's own pair. Reported per lag: the share of blocks the far
    /// reference wins at all and by more than 10%, and -- the number the
    /// decision is taken on -- the total SAD of best-of-{near, far} against
    /// the total SAD of near alone, which is the prediction energy a second
    /// reference could remove before any rate is paid for naming it.
    ///
    ///     cargo test -p ec-av1 --release --lib -- --ignored last2_census --nocapture
    #[test]
    #[ignore = "reads the real films"]
    fn last2_census() {
        if !have_ffmpeg() {
            eprintln!("SKIP last2_census: no ffmpeg");
            return;
        }
        const SIDE: usize = 16;
        // SAD of one 16x16 block against `r` at full-pel `(dy, dx)`, or
        // `None` when the displaced block leaves the picture.
        let sad = |cur: &Picture, r: &Picture, bx: usize, by: usize, mv: (isize, isize)| {
            let (sy, sx) = (by as isize + mv.0, bx as isize + mv.1);
            if sy < 0 || sx < 0 {
                return None;
            }
            let (sy, sx) = (sy as usize, sx as usize);
            if sy + SIDE > r.height || sx + SIDE > r.width {
                return None;
            }
            let mut acc = 0u64;
            for row in 0..SIDE {
                let c = &cur.y[(by + row) * cur.width + bx..][..SIDE];
                let p = &r.y[(sy + row) * r.width + sx..][..SIDE];
                acc += c
                    .iter()
                    .zip(p)
                    .map(|(&a, &b)| u64::from(a.abs_diff(b)))
                    .sum::<u64>();
            }
            Some(acc)
        };
        // A log-step diamond from the zero motion vector: the cheapest search
        // that still finds real pans. Both references get exactly this one,
        // so the comparison is search-fair.
        let search = |cur: &Picture, r: &Picture, bx: usize, by: usize| -> u64 {
            let mut best = (
                sad(cur, r, bx, by, (0, 0)).unwrap_or(u64::MAX),
                (0isize, 0isize),
            );
            let mut step = 16isize;
            while step >= 1 {
                let centre = best.1;
                for (dy, dx) in [
                    (-1, -1),
                    (-1, 0),
                    (-1, 1),
                    (0, -1),
                    (0, 1),
                    (1, -1),
                    (1, 0),
                    (1, 1),
                ] {
                    let mv = (centre.0 + dy * step, centre.1 + dx * step);
                    if let Some(s) = sad(cur, r, bx, by, mv)
                        && s < best.0
                    {
                        best = (s, mv);
                    }
                }
                step /= 2;
            }
            best.0
        };
        println!(
            "| clip | level (near/far lag) | blocks | far wins | far wins >10% | SAD near | SAD best-of | energy removed |"
        );
        println!("|---|---|---|---|---|---|---|---|");
        for (name, path, seek) in native_gate_clips() {
            if !name.starts_with("film ") {
                continue;
            }
            let (vf, cw, ch) = crate::probe::gate_crop(&path);
            let pictures = clip_frames_vf(&path, &seek, &vf, cw, ch, 12);
            for (near, far, level) in [(1usize, 2usize, "leaf"), (4usize, 8usize, "ARF")] {
                let (mut blocks, mut wins, mut big) = (0u64, 0u64, 0u64);
                let (mut near_sad, mut best_sad) = (0u64, 0u64);
                for i in far..pictures.len() {
                    let cur = &pictures[i];
                    let (rn, rf) = (&pictures[i - near], &pictures[i - far]);
                    for by in (0..ch.saturating_sub(SIDE - 1)).step_by(SIDE) {
                        for bx in (0..cw.saturating_sub(SIDE - 1)).step_by(SIDE) {
                            let (n, f) = (search(cur, rn, bx, by), search(cur, rf, bx, by));
                            blocks += 1;
                            wins += u64::from(f < n);
                            big += u64::from(f as f64 * 1.1 < n as f64);
                            near_sad += n;
                            best_sad += n.min(f);
                        }
                    }
                }
                let pct = |a: u64| 100.0 * a as f64 / blocks.max(1) as f64;
                println!(
                    "| {name} | {level} ({near}/{far}) | {blocks} | {:.1}% | {:.1}% | {near_sad} | {best_sad} | {:.2}% |",
                    pct(wins),
                    pct(big),
                    100.0 * (near_sad - best_sad) as f64 / near_sad.max(1) as f64,
                );
            }
        }
    }

    /// lane-arfpred: WHAT THE TOP ARF'S PREDICTION IS WORTH, and what a
    /// temporal filter of its SOURCE would take off it, priced before any
    /// encoder change (class `gate-blind-to-feature`).
    ///
    /// The stream census says our top ARF costs 6992 B a frame against
    /// rav1e's 3872 at a COARSER quantizer (118 vs 104) for the same picture
    /// quality, and that 61% of the gap is residual, not syntax. Two
    /// explanations are separable here without an encoder: our anchor
    /// predicts across 8 pictures where rav1e's predicts across 4 (lag), and
    /// the residual it codes at that lag is dominated by film grain, which no
    /// motion search can predict (source noise). So, per ARF position of the
    /// gate's own 48-picture window:
    ///
    /// * mean full-pel SAD per pixel at lag 8 (ours) and lag 4 (rav1e's),
    /// * the same at lag 8 with the CURRENT picture temporally filtered --
    ///   a libaom-shaped motion-compensated average of the +-2 display
    ///   neighbours, weights `exp(-mse / strength)` -- which is the residual
    ///   energy an ARF temporal filter would remove, and
    /// * PSNR of that filtered picture against the unfiltered one, the
    ///   CEILING the filtered ARF's own PSNR is then measured under (the
    ///   BD gate scores against the unfiltered source).
    ///
    ///     EC_ARF_TF_S=<strength> cargo test -p ec-av1 --release --lib -- \
    ///   --ignored arf_pred_census --nocapture
    #[test]
    #[ignore = "reads the real films"]
    fn arf_pred_census() {
        if !have_ffmpeg() {
            eprintln!("SKIP arf_pred_census: no ffmpeg");
            return;
        }
        const SIDE: usize = 16;
        let strength: f64 = std::env::var("EC_ARF_TF_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30.0);
        let past = std::env::var("EC_ARF_TF_PAST").as_deref() == Ok("1");
        // SAD and the matching block of `r` at full-pel `(dy, dx)`, or `None`
        // when the displaced block leaves the picture.
        let at = |cur: &[u16], cw: usize, r: &Picture, bx: usize, by: usize, mv: (isize, isize)| {
            let (sy, sx) = (by as isize + mv.0, bx as isize + mv.1);
            if sy < 0 || sx < 0 {
                return None;
            }
            let (sy, sx) = (sy as usize, sx as usize);
            if sy + SIDE > r.height || sx + SIDE > r.width {
                return None;
            }
            let mut acc = 0u64;
            for row in 0..SIDE {
                let c = &cur[row * cw..][..SIDE];
                let p = &r.y[(sy + row) * r.width + sx..][..SIDE];
                acc += c
                    .iter()
                    .zip(p)
                    .map(|(&a, &b)| u64::from(a.abs_diff(b)))
                    .sum::<u64>();
            }
            Some((acc, (sy, sx)))
        };
        // The same log-step diamond `last2_census` uses, so the two censuses'
        // SADs are comparable.
        let search = |cur: &[u16], cw: usize, r: &Picture, bx: usize, by: usize| {
            let mut best = match at(cur, cw, r, bx, by, (0, 0)) {
                Some(v) => v,
                None => return (u64::MAX, (by, bx)),
            };
            let mut centre = (0isize, 0isize);
            let mut step = 16isize;
            while step >= 1 {
                let from = centre;
                for (dy, dx) in [
                    (-1, -1),
                    (-1, 0),
                    (-1, 1),
                    (0, -1),
                    (0, 1),
                    (1, -1),
                    (1, 0),
                    (1, 1),
                ] {
                    let mv = (from.0 + dy * step, from.1 + dx * step);
                    if let Some(s) = at(cur, cw, r, bx, by, mv)
                        && s.0 < best.0
                    {
                        best = s;
                        centre = mv;
                    }
                }
                step /= 2;
            }
            best
        };
        println!(
            "| clip | ARFs | strength {strength} past {past} | lag8 SAD/px | lag4 SAD/px | tf lag8 SAD/px | tf removes | tf PSNR ceiling |"
        );
        println!("|---|---|---|---|---|---|---|");
        for (name, path, seek) in native_gate_clips() {
            if !name.starts_with("film ") {
                continue;
            }
            let (vf, cw, ch) = crate::probe::gate_crop(&path);
            let pictures = clip_frames_vf(&path, &seek, &vf, cw, ch, 48);
            let (mut n8, mut n4, mut ntf, mut px, mut arfs) = (0u64, 0u64, 0u64, 0u64, 0usize);
            let (mut tf_sse, mut tf_px) = (0f64, 0u64);
            for i in (8..pictures.len()).step_by(8) {
                arfs += 1;
                let cur = &pictures[i];
                for by in (0..ch.saturating_sub(SIDE - 1)).step_by(SIDE) {
                    for bx in (0..cw.saturating_sub(SIDE - 1)).step_by(SIDE) {
                        let block: Vec<u16> = (0..SIDE)
                            .flat_map(|row| cur.y[(by + row) * cur.width + bx..][..SIDE].to_vec())
                            .collect();
                        n8 += search(&block, SIDE, &pictures[i - 8], bx, by).0;
                        n4 += search(&block, SIDE, &pictures[i - 4], bx, by).0;
                        // The temporal filter: each +-2 display neighbour is
                        // motion-compensated onto this block and averaged in
                        // with libaom's shape, a weight that falls with the
                        // match's own mean squared error.
                        let mut acc: Vec<f64> = block.iter().map(|&v| f64::from(v)).collect();
                        let mut wsum = 1.0f64;
                        // `EC_ARF_TF_PAST=1` restricts the filter to the
                        // DISPLAY-PAST neighbours -- the only ones the
                        // encoder's own group buffer holds when it codes its
                        // top ARF (the group's future is not buffered yet).
                        let offs: &[isize] = match past {
                            true => &[-2, -1],
                            false => &[-2, -1, 1, 2],
                        };
                        for &d in offs {
                            let j = i as isize + d;
                            let Some(nb) = usize::try_from(j).ok().and_then(|j| pictures.get(j))
                            else {
                                continue;
                            };
                            let (_, (sy, sx)) = search(&block, SIDE, nb, bx, by);
                            let mut sse = 0f64;
                            for row in 0..SIDE {
                                for col in 0..SIDE {
                                    let d = f64::from(block[row * SIDE + col])
                                        - f64::from(nb.y[(sy + row) * nb.width + sx + col]);
                                    sse += d * d;
                                }
                            }
                            let w = (-sse / ((SIDE * SIDE) as f64) / strength).exp();
                            for row in 0..SIDE {
                                for col in 0..SIDE {
                                    acc[row * SIDE + col] +=
                                        w * f64::from(nb.y[(sy + row) * nb.width + sx + col]);
                                }
                            }
                            wsum += w;
                        }
                        let filtered: Vec<u16> = acc
                            .iter()
                            .map(|v| (v / wsum).round().clamp(0.0, 1023.0) as u16)
                            .collect();
                        for (a, b) in filtered.iter().zip(&block) {
                            let d = f64::from(*a) - f64::from(*b);
                            tf_sse += d * d;
                        }
                        tf_px += (SIDE * SIDE) as u64;
                        ntf += search(&filtered, SIDE, &pictures[i - 8], bx, by).0;
                        px += (SIDE * SIDE) as u64;
                    }
                }
            }
            let per = |v: u64| v as f64 / px.max(1) as f64;
            let mse = tf_sse / tf_px.max(1) as f64;
            println!(
                "| {name} | {arfs} | s{strength} p{past} | {:.3} | {:.3} | {:.3} | {:.1}% | {:.2} dB |",
                per(n8),
                per(n4),
                per(ntf),
                100.0 * (n8 - ntf) as f64 / n8.max(1) as f64,
                10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10(),
            );
        }
    }

    /// lane-refs: THE FRAME FILTER REACHES THE PIXELS. `mc::predict` used to
    /// hardwire the REGULAR kernel, so the encoder could not have coded a
    /// SMOOTH or SHARP frame even if the header had said so -- the search,
    /// the trials, the recon and the frame header now all read
    /// `FrameCtx::interp_filter`. Each of the three kernels codes its own
    /// distinct stream (a filter that never reached the prediction would code
    /// the same bytes as REGULAR, class `symbol consumption gap`), and ffmpeg
    /// reconstructs every frame of each of them exactly, which is what proves
    /// the header and the prediction agree.
    #[test]
    fn each_frame_interpolation_filter_codes_its_own_stream_ffmpeg_decodes_exactly() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP each_frame_interpolation_filter_codes_its_own_stream: no ffmpeg");
            return;
        }
        let (width, height) = (256usize, 128usize);
        // A HALF-PEL pan: an integer-pel one reads no filter tap at all, so
        // all three kernels would code the same bytes and the test would pass
        // on a wiring that does nothing (class `fixture proves the symbol, not
        // the signal`). `panned_test_card` shifts by whole pixels, so the
        // search finds the sub-pel positions off the reconstruction instead:
        // three frames of a slow diagonal pan.
        let pictures: Vec<Picture> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let mut streams = Vec::new();
        for filter in [
            ec_av1_syntax::InterpolationFilter::Eighttap,
            ec_av1_syntax::InterpolationFilter::EighttapSmooth,
            ec_av1_syntax::InterpolationFilter::EighttapSharp,
        ] {
            set_frame_interp(Some(filter));
            let fctx = &crate::decode::FrameCtx::for_encoder();
            let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx)
                .unwrap_or_else(|e| panic!("{filter:?}: {e}"));
            let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, 3);
            assert_eq!(decoded.len(), 3, "{filter:?}: decoded frame count");
            for (i, (frame, decoded)) in encoded.frames.iter().zip(&decoded).enumerate() {
                assert_eq!(
                    decoded.y, frame.reconstruction.y,
                    "{filter:?} frame {i}: luma"
                );
                assert_eq!(decoded.u, frame.reconstruction.u, "{filter:?} frame {i}: U");
                assert_eq!(decoded.v, frame.reconstruction.v, "{filter:?} frame {i}: V");
            }
            streams.push((filter, encoded.stream));
        }
        set_frame_interp(None);
        for (i, (fa, a)) in streams.iter().enumerate() {
            for (fb, b) in &streams[i + 1..] {
                assert_ne!(a, b, "{fa:?} and {fb:?} coded the same bytes");
            }
        }
    }

    /// lane-hpmv: THE EIGHTH-PEL VECTOR REACHES THE STREAM. With
    /// `allow_high_precision_mv` off the writer refuses an odd eighth-pel
    /// residual outright (`round_to_valid_mv` rounds it away first), so the
    /// finest vector the encoder could commit to was a quarter pel. With the
    /// header bit on, the writer codes an `mv_hp`/`mv_class0_hp` symbol per
    /// non-zero component and the search's fourth stage refines to the
    /// eighth: this asserts ZERO hp symbols in the off arm, non-zero in the
    /// on arm, and -- the part that makes it a signal rather than a symbol
    /// count (class `fixture proves the symbol, not the signal`) -- that some
    /// of them come out ZERO, i.e. name a vector the off arm cannot code at
    /// all. Both streams are decoded by ffmpeg AND by this crate's own
    /// decoder and asserted sample-exact against the encoder's own
    /// reconstruction, which is what proves the header bit and the symbols
    /// agree.
    #[test]
    fn a_high_precision_frame_codes_eighth_pel_motion_vectors_both_decoders_read() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP a_high_precision_frame_codes_eighth_pel_motion_vectors: no ffmpeg");
            return;
        }
        let (width, height) = (256usize, 128usize);
        let pictures: Vec<Picture> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let mut arms = Vec::new();
        for hp in [false, true] {
            set_high_precision_mv(Some(hp));
            let _ = crate::tile::take_hp_symbols();
            let fctx = &crate::decode::FrameCtx::for_encoder();
            let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx)
                .unwrap_or_else(|e| panic!("hp={hp}: {e}"));
            let (symbols, eighth) = crate::tile::take_hp_symbols();
            let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, pictures.len());
            let ours = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
            assert_eq!(decoded.len(), pictures.len(), "hp={hp}: ffmpeg frame count");
            assert_eq!(
                ours.len(),
                pictures.len(),
                "hp={hp}: our decoder frame count"
            );
            for (i, e) in encoded.frames.iter().enumerate() {
                for (who, got) in [("ffmpeg", &decoded[i]), ("our decoder", &ours[i])] {
                    if let Some((plane, s, got, want)) =
                        first_plane_mismatch(got, &e.reconstruction)
                    {
                        panic!(
                            "hp={hp} frame {i} plane {plane} sample {s}: {who} decoded {got}, \
                             the encoder reconstructed {want}"
                        );
                    }
                }
            }
            arms.push((symbols, eighth, encoded.stream));
        }
        set_high_precision_mv(None);
        let (off, on) = (&arms[0], &arms[1]);
        assert_eq!(
            off.0, 0,
            "a frame without the header bit codes no hp symbol"
        );
        assert!(
            on.0 > 0,
            "a high-precision frame codes hp symbols (got {})",
            on.0
        );
        assert!(
            on.1 > 0,
            "a high-precision frame codes eighth-pel vectors ({} hp symbols, none of them zero)",
            on.0
        );
        assert_ne!(off.2, on.2, "the two arms coded the same bytes");
        println!(
            "hp witness: off {} hp symbols / {} B, on {} hp symbols ({} eighth-pel) / {} B",
            off.0,
            off.2.len(),
            on.0,
            on.1,
            on.2.len()
        );
    }
