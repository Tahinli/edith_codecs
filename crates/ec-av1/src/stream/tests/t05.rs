use super::*;
use super::support::*;
use super::gates_a::*;
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

    /// lane-sqdrift r2: the SQUARE-ONLY twin of the no-CFL `uv_mode` gate
    /// above. With `--enable-rect-partitions=0` the encoder codes only
    /// `PARTITION_NONE` and `PARTITION_SPLIT`, and on detailed content the RD
    /// eventually hands one whole 64x64 superblock of an INTER frame to an
    /// INTRA block -- `bsize == sb_size`, the one shape whose `uv_mode` must
    /// come off the 13-symbol `uv_mode_no_cfl` alphabet because libaom's
    /// `is_cfl_allowed` (`blockd.h`: `wide <= 32 && high <= 32`) excludes it.
    ///
    /// Before the fix this decoder read the 14-symbol CFL alphabet there: the
    /// same `DC_PRED` value, a different interval, and the tile desynced from
    /// that block on. r1 measured the damage on the pinned 192x128 stream
    /// (sha256 `c6af4fb4...`): decode-order frames 0-3 pre-filter byte-exact,
    /// frame 4 luma 3540 samples wrong over exactly the last superblock
    /// (x 128..191, y 64..127), then 24k wrong by frame 7 -- and the decoder
    /// did not even report a mismatch, it reported an intra-rect-strip refusal
    /// read out of the already-desynced stream (class `refusal-from-own-desync`;
    /// aomdec's own partition histogram over that stream is rect-free).
    /// After the fix the same stream is byte-exact on all 8 frames.
    ///
    /// Distinct from the sibling gate: rect partitions OFF (so the 64x64 NONE
    /// really is a whole superblock), `--min-partition-size=16`, natural
    /// mandelbrot detail instead of a synthetic half-noise ramp, and 8 frames
    /// of drift so a desync that survives one frame still fails here. Every
    /// decode-order frame of every attempt is compared against ffmpeg, so a
    /// mismatch of ANY shape fails; the gate never turns one into a SKIP.
    #[test]
    fn a_real_aomenc_square_only_inter_sequence_with_a_64x64_intra_superblock_decodes_pixel_exact()
    {
        const NAME: &str = "a_real_aomenc_square_only_inter_sequence_with_a_64x64_intra_superblock_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (192usize, 128usize, 8usize);
        for bit_depth in [8u32, 10u32] {
            let (mut named_refusals, mut matched, mut carried_none) = (0u32, 0u32, 0u32);
            let mut total_hits = 0usize;
            for attempt in 0..4u32 {
                // r1's pinned recipe is (start_scale 4.76, cq 55); the other
                // three points move the detail level and the quantiser, which
                // is what moves WHICH superblock the RD codes intra.
                let start_scale = ["4.76", "3.20"][(attempt % 2) as usize];
                let cq = [55, 45][(attempt / 2) as usize];
                let duration = frame_count as f64 / 25.0;
                let source = format!(
                    "mandelbrot=size={width}x{height}:start_scale={start_scale}:\
                     end_scale=0.004:end_pts=8:rate=25"
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
                        "-vf",
                        "hue=s=0",
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
                    "--cpu-used=1",
                    "--threads=1",
                    "--row-mt=0",
                    "--sb-size=64",
                    &depth_arg,
                    &input_depth_arg,
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--kf-min-dist=1000",
                    "--kf-max-dist=1000",
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--min-partition-size=16",
                    "--max-partition-size=64",
                    "--enable-tx-size-search=0",
                    "--enable-order-hint=0",
                    "--enable-warped-motion=0",
                    "--enable-obmc=0",
                    "--enable-masked-comp=0",
                    "--enable-interintra-comp=0",
                    "--enable-dist-wtd-comp=0",
                    "--enable-diff-wtd-comp=0",
                    "--enable-onesided-comp=0",
                    "--enable-interintra-wedge=0",
                    "--enable-smooth-interintra=0",
                    "--enable-filter-intra=0",
                    "--enable-smooth-intra=0",
                    "--enable-paeth-intra=0",
                    "--enable-directional-intra=0",
                    "--enable-angle-delta=0",
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
                 attempts carrying a 64x64 intra superblock ({total_hits} no-CFL uv_mode reads), \
                 {carried_none} attempts carried none"
            );
            assert!(
                matched > 0 && total_hits > 0,
                "{NAME} ({bit_depth}-bit): no square-only attempt decoded a 64x64 intra block \
                 inside an inter frame, so the no-CFL uv_mode alphabet was never exercised -- \
                 gate proved nothing ({named_refusals} refusals, {carried_none} carried none)"
            );
        }
    }

    /// lane-mc64 r2: the RECT-ON twin of the square-only gate above, pinned
    /// as a stream fixture because its aomenc recipe is not reproducible
    /// (the r2 recipe re-encoded to a different file, measured md5
    /// `278be00...` vs the fixture's `add55f6...`; class
    /// `seeded-fixture-not-reproducible`).
    ///
    /// `crates/ec-av1/fixtures/av1_192x128_8bit_intra64_in_inter.obu`
    /// (sha256 `e4cff46cb1700527c68efc866db27a2835705e949d646f73f0c9df577d594bd6`)
    /// carries a 64x64 `PARTITION_NONE` INTRA block inside an INTER frame at
    /// decode-order frame 2, mi(16,32), in a stream whose OTHER superblocks
    /// are rect-partitioned -- so the 64-axis `is_cfl_allowed` narrowing
    /// (`blockd.h`: `wide <= 32 && high <= 32`) has to hold on the intra
    /// arm that the rect recipe reaches, not only on the whole-superblock
    /// one. Before lane-sbrect10's fix this frame reconstructed with 3720
    /// wrong luma samples (delta -12..+9, chroma bit-exact) and the decode
    /// then died at frame 3 on a phantom refusal of its own desync (class
    /// `refusal-from-own-desync`); all 6 decode-order frames are byte-exact
    /// against the oracle now.
    #[test]
    fn a_pinned_rect_stream_with_a_64x64_intra_block_in_an_inter_frame_decodes_pixel_exact() {
        const NAME: &str =
            "a_pinned_rect_stream_with_a_64x64_intra_block_in_an_inter_frame_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_aomenc() {
            eprintln!(
                "SKIP {NAME}: no aomenc/aomdec oracle at {}",
                aomenc_path().display()
            );
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/av1_192x128_8bit_intra64_in_inter.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        let before = crate::decode::nocfl_uv_mode_hits();
        let (frames, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
        let hits = crate::decode::nocfl_uv_mode_hits() - before;
        assert_eq!(
            frames, 6,
            "{NAME}: expected 6 decode-order frames ({hidden} hidden)"
        );
        assert!(
            hits > 0,
            "{NAME}: no 64-axis intra block read the no-CFL uv_mode alphabet -- the fixture no \
             longer carries the shape this test gates"
        );
    }

    /// lane-av1444chr: a 4:4:4 INTER chroma unit inherits the `tx_type` of
    /// the luma leaf covering its OWN quadrant, not the block-level (top-left
    /// leaf) one. `av1_get_tx_type` (`blockd.h:1287-1297`) reads
    /// `xd->tx_type_map` at the chroma unit's own position scaled back to
    /// luma, and `read_coeffs`' `all_zero` arm stamps `DCT_DCT` into that
    /// leaf's cell (`decodetxb.c:199-203`) -- so a chroma unit sitting over a
    /// luma leaf that coded NOTHING must reconstruct as DCT_DCT even when the
    /// block's top-left leaf is `IDTX`.
    ///
    /// The fixture is the report's witness pinned into `fixtures/`:
    /// `testsrc2 128x128` yuv444p 4 frames, aomenc `--profile=1 --cq-level=20
    /// --cpu-used=2 --sb-size=64 --min-partition-size=64
    /// --max-partition-size=64 --passes=1 --end-usage=q --threads=1 --row-mt=0
    /// --lag-in-frames=0 --kf-max-dist=100 --limit=4 --obu`; 27933 bytes,
    /// sha256 `a06c9f7a0862252f6ab7ebdcec340d0602e730e38089365f14747db37f8e78d5`.
    /// Every inter block is a whole 64x64 (min=max partition 64) whose luma
    /// var-tx tree is four TX_32X32 leaves, so each block's chroma plane is
    /// four TX_32X32 units -- one per leaf.
    ///
    /// Pre-fix, decode-order frames 1/2/3 differ from the oracle in 777/640/554
    /// V samples (luma and U exact) and the per-unit type census disagrees at
    /// exactly the units over the skipped leaf: V(0,32) of block mi(0,0) in
    /// frame 1, V(64,96) of block mi(16,16) in every inter frame. Post-fix all
    /// 136 non-empty inter units agree with the oracle's `EC_DQCOEFF` type and
    /// every frame is byte-exact.
    ///
    /// The non-vacuity bar is the DIFFERS counter, not the route counter: a
    /// route counter fires on every four-unit chroma read (96 of them here)
    /// and stayed green through a no-op fix that changed zero pixels, because
    /// on a block whose four leaves agree the per-quadrant resolve and the
    /// block-level value are the same answer. The number that says the resolve
    /// CHANGED something is 8 on this stream -- the four skipped-leaf quadrants
    /// (two of them in decode-order frame 1), each resolved once per chroma
    /// plane -- and it must be 0 on the 4:2:0 control below, since the arm is
    /// unreachable at 4:2:0.
    #[test]
    fn a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type() {
        const NAME: &str =
            "a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type";
        const FIXTURE_LEN: usize = 27933;
        const FIXTURE_FNV: u64 = 0x85fa_830b_8800_90df;
        let _gate_lock = lock_gate_counters();
        if !have_aomenc() {
            eprintln!(
                "SKIP {NAME}: no aomenc/aomdec oracle at {}",
                aomenc_path().display()
            );
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/444_quad_leaf_tx_type.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let quad0 = crate::decode::chroma_quad_leaf_tx_hits();
        let diff0 = crate::decode::chroma_quad_leaf_tx_diff_hits();
        let (frames, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
        let quad = crate::decode::chroma_quad_leaf_tx_hits() - quad0;
        let diff = crate::decode::chroma_quad_leaf_tx_diff_hits() - diff0;
        assert_eq!(
            frames, 4,
            "{NAME}: expected 4 decode-order frames ({hidden} hidden)"
        );
        assert!(
            quad >= 2 * diff && quad > 0,
            "{NAME}: no chroma unit took its type from the covering luma leaf \
             (quad {quad}, differs {diff}) -- the corrected route never ran"
        );
        assert!(
            diff >= 2,
            "{NAME}: the per-quadrant resolve never CHANGED an answer (diff {diff}; measured 4 \
             on this stream, 2 of them in decode-order frame 1). A route counter alone is not a \
             witness: it fires on every four-unit chroma read, including the \
             blocks whose four leaves all agree -- the exact shape a no-op fix looked green on."
        );
        eprintln!("{NAME}: quad-resolved chroma units {quad}, of which {diff} differed");

        // 4:2:0 control: the same decode, same counters, zero differences --
        // the four-unit arm is the 4:4:4 (`ss 0/0`) route only.
        let control = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/av1_192x128_8bit_intra64_in_inter.obu"),
        )
        .unwrap_or_else(|e| panic!("{NAME}: reading the 4:2:0 control: {e}"));
        let c_quad0 = crate::decode::chroma_quad_leaf_tx_hits();
        let c_diff0 = crate::decode::chroma_quad_leaf_tx_diff_hits();
        let c_frames = decode_stream(&control)
            .unwrap_or_else(|e| panic!("{NAME}: the 4:2:0 control no longer decodes: {e}"))
            .len();
        let c_quad = crate::decode::chroma_quad_leaf_tx_hits() - c_quad0;
        let c_diff = crate::decode::chroma_quad_leaf_tx_diff_hits() - c_diff0;
        assert!(c_frames > 0, "{NAME}: the 4:2:0 control decoded no frames");
        assert_eq!(
            (c_quad, c_diff),
            (0, 0),
            "{NAME}: the 4:2:0 control fired the 4:4:4 four-unit route ({c_quad} units, {c_diff} \
             differing) -- the counter is not specific to the route it claims to witness"
        );
    }

    /// lane-av1f9singleland: `decode_inter_block` has TWO textually parallel
    /// copies of the 4:4:4 four-unit chroma arm -- the compound prediction
    /// arm (the `build` closure) and the single-reference arm. Both resolve
    /// each unit's `tx_type` through `covering_leaf_tx_type` over a
    /// `leaf_tx_types` list collected at EVERY size, both bumped the same
    /// `CHROMA_QUAD_LEAF_TX_HITS` / `..._DIFF_HITS` pair, and the gate above
    /// asserts only that pair. So that gate cannot say which arm its witness
    /// came from, and the two arms can be reverted independently.
    ///
    /// Measured on the gate's own witness `fixtures/444_quad_leaf_tx_type.obu`
    /// (the census is the point of this test, so it is quoted rather than
    /// assumed): 96 chroma units resolve through their covering luma leaf, of
    /// which 88 are the single-reference arm's and 8 the compound arm's; of
    /// the 8 units whose resolved type DIFFERS from the block-level one --
    /// the non-vacuity bar, since a block whose four leaves agree makes the
    /// per-quadrant resolve a no-op -- ALL 8 are the single-reference arm's.
    /// The shared counters' non-vacuity number is therefore 100%
    /// single-reference, and the compound arm contributes 8 of 96 route hits
    /// and none of the 8 that change an answer.
    ///
    /// The other 27 pinned 4:4:4 fixtures in this tree reach the arm zero
    /// times (`444_intrabc_rect4_witness`, `444_leaf8_oob`,
    /// `444_lossy_rect4_*`, `444_lossy_superres_*`, `444_sb128rect_lr_witness`,
    /// and every `ll444_*`), so this one witness is the only thing pinning
    /// the single-reference copy of the arm at all.
    ///
    /// Non-vacuity, per the mutation recorded in
    /// `lanes/av1f9singleland.report.md`: reverting ONLY this arm's
    /// `Some(cu_tx_type)` to `Some(luma_tx_type)` turns this gate red on
    /// decode-order frame 1 with 777 bytes differing; gating ONLY this arm's
    /// leaf push back behind `if side > 64` (the pre-`6c33f204` state) turns
    /// it red with the same 777 bytes.
    #[test]
    fn the_pinned_444_quadrant_witness_is_the_single_reference_arms_and_not_only_the_compound_arms()
    {
        const NAME: &str = "the_pinned_444_quadrant_witness_is_the_single_reference_arms_and_not_only_the_compound_arms";
        const FIXTURE_LEN: usize = 27933;
        const FIXTURE_FNV: u64 = 0x85fa_830b_8800_90df;
        let _gate_lock = lock_gate_counters();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/444_quad_leaf_tx_type.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let q0 = crate::decode::chroma_quad_leaf_tx_hits();
        let s0 = crate::decode::chroma_quad_leaf_tx_singleref_hits();
        let sd0 = crate::decode::chroma_quad_leaf_tx_singleref_diff_hits();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the witness no longer decodes: {e}"))
            .len();
        assert_eq!(frames, 4, "{NAME}: expected 4 shown frames");
        let quad = crate::decode::chroma_quad_leaf_tx_hits() - q0;
        let singleref = crate::decode::chroma_quad_leaf_tx_singleref_hits() - s0;
        let sr_diff = crate::decode::chroma_quad_leaf_tx_singleref_diff_hits() - sd0;

        assert!(
            singleref > quad / 2,
            "{NAME}: the single-reference arm resolved {singleref} of {quad} covering-leaf chroma \
             units -- the shared counter is no longer dominated by the arm this gate witnesses, so \
             a revert of the single-reference copy would leave the shared total comfortably non-zero"
        );
        assert!(
            sr_diff >= 2,
            "{NAME}: the single-reference arm's per-quadrant resolve never CHANGED an answer \
             (differing {sr_diff}). A route count alone is not a witness: it fires on every \
             four-unit chroma read, including the blocks whose four luma leaves all agree -- the \
             exact shape a no-op resolve looks green on."
        );
        eprintln!(
            "{NAME}: {singleref} of {quad} covering-leaf units are the single-reference arm's, \
             {sr_diff} of which changed an answer"
        );

        // 4:2:0 control, as in the gate above: the four-unit arm is the 4:4:4
        // (`ss 0/0`) route only, so the single-reference arm's own counters
        // must stay at exactly 0 there too.
        let control = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/av1_192x128_8bit_intra64_in_inter.obu"),
        )
        .unwrap_or_else(|e| panic!("{NAME}: reading the 4:2:0 control: {e}"));
        let c_s0 = crate::decode::chroma_quad_leaf_tx_singleref_hits();
        let c_sd0 = crate::decode::chroma_quad_leaf_tx_singleref_diff_hits();
        let c_frames = decode_stream(&control)
            .unwrap_or_else(|e| panic!("{NAME}: the 4:2:0 control no longer decodes: {e}"))
            .len();
        assert!(c_frames > 0, "{NAME}: the 4:2:0 control decoded no frames");
        assert_eq!(
            (
                crate::decode::chroma_quad_leaf_tx_singleref_hits() - c_s0,
                crate::decode::chroma_quad_leaf_tx_singleref_diff_hits() - c_sd0,
            ),
            (0, 0),
            "{NAME}: the 4:2:0 control fired the single-reference arm's four-unit route -- the \
             counter is not specific to the route it claims to witness"
        );
    }

    /// lane-av1chrtx: the SIBLING of lane-av1444chr's four-unit arm -- a RECT
    /// inter block's chroma plane, read by `read_inter_rect_chroma`. That
    /// helper was left handing every unit the block-level `luma_tx_type`
    /// (the top-left leaf's type) after the four-unit arms were pointed at
    /// `covering_leaf_tx_type`, so a rect block whose chroma plane splits
    /// (`nx*ny > 1`) over MIXED var-tx leaves read the wrong type per unit.
    ///
    /// The witness is pinned into `fixtures/`: `testsrc2 256x256` yuv444p 3
    /// frames, aomenc `--profile=1 --cq-level=30 --cpu-used=0 --sb-size=64
    /// --min-partition-size=32 --max-partition-size=64 --enable-tx-size-search=1
    /// --passes=1 --end-usage=q --threads=1 --row-mt=0 --lag-in-frames=0
    /// --kf-max-dist=100 --limit=3 --obu`; 26839 bytes, sha256
    /// `06174a66e92aaeb751a8e86db59711fcb1f18d74bde2d77e1ef1be7983eb7c80`.
    /// Its one qualifying block is the 32x64 inter strip at mi(32,48): a
    /// 4:4:4 chroma plane of (32,64) = TWO TX_32X32 units, over a luma tree
    /// of five leaves -- `Idtx` (32x32) on top, then `VDct`/`DctDct`/`HDct`/
    /// `VDct` 16x16 leaves. The lower chroma unit therefore inherits `VDct`
    /// where the block-level value is `Idtx`.
    ///
    /// Getting a witness at all took a bounded hunt (200+ encodes): `multi`
    /// is false by construction at 4:2:0 (a `side <= 64` block's chroma plane
    /// is at most 32x32 = one unit), so the arm needs 4:4:4 AND a rect block
    /// that keeps a 64-px axis, AND `--cpu-used=0` with `--min-partition-size=32`
    /// to make aomenc actually pick the 32x64 strip.
    ///
    /// The non-vacuity bar is the DIFFERS counter, not the route counter: the
    /// control below is a committed 4:4:4 rect stream where the route fires
    /// and still changes nothing, because that block's leaves agree.
    #[test]
    fn a_pinned_444_rect_inter_stream_resolves_each_chroma_unit_from_its_own_luma_leaf() {
        const NAME: &str =
            "a_pinned_444_rect_inter_stream_resolves_each_chroma_unit_from_its_own_luma_leaf";
        const FIXTURE_LEN: usize = 26839;
        const FIXTURE_FNV: u64 = 0xc77f_310c_036d_ff73;
        let _gate_lock = lock_gate_counters();
        if !have_aomenc() {
            eprintln!(
                "SKIP {NAME}: no aomenc/aomdec oracle at {}",
                aomenc_path().display()
            );
            return;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/444_rect_strip_leaf_tx_type.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let hits0 = crate::decode::chroma_rect_leaf_tx_hits();
        let diff0 = crate::decode::chroma_rect_leaf_tx_diff_hits();
        let (frames, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
        let hits = crate::decode::chroma_rect_leaf_tx_hits() - hits0;
        let diff = crate::decode::chroma_rect_leaf_tx_diff_hits() - diff0;
        assert_eq!(
            frames, 3,
            "{NAME}: expected 3 decode-order frames ({hidden} hidden)"
        );
        assert!(
            hits >= 2 * diff && hits > 0,
            "{NAME}: no rect chroma unit took its type from the covering luma leaf (hits {hits}, \
             differs {diff}) -- the corrected route never ran"
        );
        assert!(
            diff >= 2,
            "{NAME}: the per-unit resolve never CHANGED an answer (diff {diff}; measured 2 on this \
             stream, one per chroma plane). A route counter alone is not a witness: the control \
             below fires it and changes nothing."
        );
        eprintln!(
            "{NAME}: rect chroma units resolved from their own leaf {hits}, of which {diff} differed"
        );

        // Control: a committed 4:4:4 rect stream where the SAME route fires
        // and every resolved type equals the block-level one -- the shape a
        // no-op fix would also be green on.
        let control = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/444_sb128rect_lr_witness.obu"),
        )
        .unwrap_or_else(|e| panic!("{NAME}: reading the rect-strip control: {e}"));
        let c_hits0 = crate::decode::chroma_rect_leaf_tx_hits();
        let c_diff0 = crate::decode::chroma_rect_leaf_tx_diff_hits();
        let c_frames = decode_stream(&control)
            .unwrap_or_else(|e| panic!("{NAME}: the rect-strip control no longer decodes: {e}"))
            .len();
        let c_hits = crate::decode::chroma_rect_leaf_tx_hits() - c_hits0;
        let c_diff = crate::decode::chroma_rect_leaf_tx_diff_hits() - c_diff0;
        assert!(
            c_frames > 0,
            "{NAME}: the rect-strip control decoded no frames"
        );
        assert!(
            c_hits > 0 && c_diff == 0,
            "{NAME}: the rect-strip control fired the route {c_hits} times with {c_diff} differing \
             types -- expected the single-unit/same-type shape (route fires, nothing changes)"
        );
    }

    /// lane-av1ichromatx: the LAST site in `decode_inter_block` that can hand a
    /// chroma unit the BLOCK-level `luma_tx_type` instead of the unit's own
    /// `tx_type_map` cell is [`read_inter_rect_chroma`]'s fallback: the branch
    /// taken when `covering_leaf_tx_type` finds no luma leaf covering the
    /// unit's own MI cell. The decode closes it with an assertion (a
    /// multi-unit chroma plane may only reach the fallback when the block
    /// coded a SINGLE luma transform, whose stamp covers the whole plane and
    /// makes `luma_tx_type` that unit's own cell).
    ///
    /// This gate is the measurement behind that assertion, on the witness that
    /// reaches it: `fixtures/444_rect_strip_leaf_tx_type.obu` (26839 bytes,
    /// sha256 `06174a66e92aaeb751a8e86db59711fcb1f18d74bde2d77e1ef1be7983eb7c80`).
    /// Measured there: the rect arm is called twice, BOTH on a multi-unit
    /// chroma plane (`nx*ny > 1`), and 4 of its 8 units take the block-level
    /// fallback -- every one of them with an EMPTY leaf list, i.e. the
    /// single-luma-unit shape the assertion allows. So `multi_fallback >= 2`
    /// here is the number that says the guarded shape is REACHED (a fallback
    /// count of 0 would leave the assertion vacuous), and the decoded frames
    /// stay byte-exact against instrumented aomdec, which is what says the
    /// fallback still hands the right value on that shape.
    ///
    /// Non-vacuity, by mutation: forcing the covering-leaf resolve to miss on a
    /// multi-unit plane (`…unit_rel_mi).filter(|_| !multi)`) turns the
    /// assertion RED on this same witness --
    /// `a multi-unit rect chroma plane fell back to the block-level tx_type
    /// while 5 luma leaves were coded` -- so the closure is not a comment.
    #[test]
    fn a_multi_unit_rect_chroma_plane_only_falls_back_to_the_block_type_on_a_single_luma_unit() {
        const NAME: &str = "a_multi_unit_rect_chroma_plane_only_falls_back_to_the_block_type_on_a_single_luma_unit";
        const FIXTURE_LEN: usize = 26839;
        const FIXTURE_FNV: u64 = 0xc77f_310c_036d_ff73;
        let _gate_lock = lock_gate_counters();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/444_rect_strip_leaf_tx_type.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");

        let fb0 = crate::decode::chroma_rect_block_tx_fallback();
        let fbm0 = crate::decode::chroma_rect_block_tx_fallback_multi();
        let frames = decode_stream(&stream)
            .unwrap_or_else(|e| panic!("{NAME}: the witness no longer decodes: {e}"))
            .len();
        let fb = crate::decode::chroma_rect_block_tx_fallback() - fb0;
        let fbm = crate::decode::chroma_rect_block_tx_fallback_multi() - fbm0;
        assert_eq!(frames, 3, "{NAME}: expected 3 shown frames");
        assert!(
            fbm >= 2,
            "{NAME}: only {fbm} block-level fallbacks landed on a MULTI-unit chroma plane \
             ({fb} in total) -- the shape the decode's assertion guards is no longer reached, so \
             that assertion is vacuous on this witness"
        );
        eprintln!("{NAME}: {fb} block-level chroma fallbacks, {fbm} of them on a multi-unit plane");
    }

    /// lane-av1readcensus: the 4:4:4 coefficient-UNIT census, pinned.
    ///
    /// The lane report `av1444rect` round 4 left an unexplained signal: 636
    /// square chroma reads against the oracle's 1086, and 2052 luma reads
    /// against its 1309, recorded as "most likely a counting-shape difference"
    /// with no evidence either way. This gate is that evidence.
    ///
    /// Measured on this arm (`ll444_minp8_inter`, pixel-exact against aomdec
    /// on every plane of every frame -- see
    /// `a_lossless_444_min_partition8_inter_stream_decodes_sample_exact`),
    /// pairing each of our 11292 walked units with the oracle's
    /// `av1_read_coeffs_txb` call BY `post_rng` (the msac state after the
    /// unit) rather than by plane label or walk position: EVERY unit pairs,
    /// and no unit's plane, shape, eob or all-zero flag differs. The per-plane
    /// split is equal too -- 3764 units per plane on both sides, of which
    /// 975 / 1793 / 1859 coded coefficients. So on a pixel-exact arm there is
    /// no asymmetry to explain at all; the round-4 numbers came from
    /// `444_intrabc_rect4_witness`, an arm that DIVERGES (390380 of 921600
    /// samples wrong), where our surplus is the divergence itself.
    ///
    /// What this gate pins is the walk's OBJECT SHAPE on the pixel-exact arm:
    /// a change to the number or plane split of coefficient units this decoder
    /// walks shows up here as an intentional diff, not as the next lane's
    /// mystery signal. The 4:2:0 control asserts the census is inert there.
    #[test]
    fn a_pixel_exact_444_stream_walks_the_same_coefficient_units_the_oracle_does() {
        const NAME: &str =
            "a_pixel_exact_444_stream_walks_the_same_coefficient_units_the_oracle_does";
        const FIXTURE_LEN: usize = 34037;
        const FIXTURE_FNV: u64 = 0x2c8f_4a2c_c025_52da;
        let _gate_lock = lock_gate_counters();
        if !have_aomenc() {
            eprintln!(
                "SKIP {NAME}: no aomenc/aomdec oracle at {}",
                aomenc_path().display()
            );
            return;
        }
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/ll444_minp8_inter.obu");
        let stream = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path.display()));
        assert_eq!(stream.len(), FIXTURE_LEN, "{NAME}: fixture length moved");
        assert_eq!(fnv1a64(&stream), FIXTURE_FNV, "{NAME}: fixture bytes moved");
        let delta = |now: [usize; 3], before: [usize; 3]| {
            [now[0] - before[0], now[1] - before[1], now[2] - before[2]]
        };

        let u0 = crate::decode::census_nonsub_units();
        let c0 = crate::decode::census_nonsub_coded();
        let (frames, hidden) = decode_all_frames_vs_oracle(&stream, NAME);
        assert_eq!(
            frames + hidden,
            8,
            "{NAME}: expected 8 decode-order frames (6 shown + 2 hidden)"
        );
        let walked = delta(crate::decode::census_nonsub_units(), u0);
        let coded = delta(crate::decode::census_nonsub_coded(), c0);

        // The oracle's own census on the same stream, paired by post_rng:
        // 3764 units per plane, 975 / 1793 / 1859 of them coded.
        assert_eq!(
            walked,
            [3764, 3764, 3764],
            "{NAME}: the 4:4:4 unit walk changed -- {walked:?} against the oracle's \
             [3764, 3764, 3764] (measured by post_rng pairing, every unit matching). A change \
             here is a WALK-SHAPE change: re-measure against aomdec before assuming it is a bug"
        );
        assert_eq!(
            coded,
            [975, 1793, 1859],
            "{NAME}: the coded-unit split changed -- {coded:?} against the oracle's [975, 1793, 1859]"
        );
        assert!(
            walked.iter().all(|&v| v > 0) && coded.iter().all(|&v| v > 0),
            "{NAME}: an all-zero plane split means the census did not run, not that the walk is empty"
        );

        // A SECOND pixel-exact arm, with a MIXED unit-shape mix -- the pinned
        // lossless stream above is all TX_4X4 (min-partition-8), so on its own
        // it would not notice a var-tx walk that started folding 8x8/4x8
        // leaves. This one walks 4x4/8x8/8x4/4x8/4x16/16x4/8x16/16x16, and
        // the oracle's census on it pairs unit-for-unit by post_rng with no
        // shape, plane, eob or all-zero difference: 1373 units, 629 luma and
        // 372 per chroma plane, of which 130 / 253 / 278 coded.
        let path2 = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/444_lossy_rect4_inter_witness.obu");
        let stream2 = std::fs::read(&path2)
            .unwrap_or_else(|e| panic!("{NAME}: reading {}: {e}", path2.display()));
        let m0 = crate::decode::census_nonsub_units();
        let mc0 = crate::decode::census_nonsub_coded();
        let (frames2, hidden2) = decode_all_frames_vs_oracle(&stream2, NAME);
        assert!(
            frames2 + hidden2 > 0,
            "{NAME}: the mixed-shape 4:4:4 arm decoded no frames"
        );
        let walked2 = delta(crate::decode::census_nonsub_units(), m0);
        let coded2 = delta(crate::decode::census_nonsub_coded(), mc0);
        assert_eq!(
            walked2,
            [629, 372, 372],
            "{NAME}: the mixed-shape 4:4:4 walk changed -- {walked2:?} against the oracle's \
             [629, 372, 372]. The all-4x4 arm above cannot see a var-tx fold; this one can"
        );
        assert_eq!(
            coded2,
            [130, 253, 278],
            "{NAME}: the mixed-shape coded-unit split changed -- {coded2:?} against the oracle's \
             [130, 253, 278]"
        );

        // 4:2:0 control: the census is a 4:4:4 question and must not fire.
        let control = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/av1_192x128_8bit_intra64_in_inter.obu"),
        )
        .unwrap_or_else(|e| panic!("{NAME}: reading the 4:2:0 control: {e}"));
        let k0 = crate::decode::census_nonsub_units();
        let kc0 = crate::decode::census_nonsub_coded();
        let k_frames = decode_stream(&control)
            .unwrap_or_else(|e| panic!("{NAME}: the 4:2:0 control no longer decodes: {e}"))
            .len();
        assert!(k_frames > 0, "{NAME}: the 4:2:0 control decoded no frames");
        assert_eq!(
            delta(crate::decode::census_nonsub_units(), k0),
            [0, 0, 0],
            "{NAME}: the coefficient-unit census fired on the 4:2:0 control"
        );
        assert_eq!(
            delta(crate::decode::census_nonsub_coded(), kc0),
            [0, 0, 0],
            "{NAME}: the coded-unit census fired on the 4:2:0 control"
        );
    }

    /// lane-inter4 r2: the 32x32-level inter `PARTITION_HORZ`/`PARTITION_VERT`
    /// strips (32x16 / 16x32) carrying a REAL residual -- the capability r1
    /// found missing ("a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs
    /// rectangular residual coding"). The strip's own rectangular max
    /// transform (`TX_32X16`/`TX_16X32`) is read through the rect var-tx tree,
    /// its coefficients through [`TxbSet::LumaRect32x16Inter`] /
    /// `ChromaRect16x8`, and inverted with the rect (sqrt2-scaled) inverse
    /// transform.
    ///
    /// `--min-partition-size=16 --max-partition-size=32` keeps the superblock
    /// level (64x32, still refused -- no 64-point rect transform) out, so
    /// every counted hit is a 32-level strip. The source is textured and
    /// translating precisely so the strips are NOT skip: a flat source (r1's)
    /// makes aomenc code them skip and proves nothing about the residual.
    /// Both `--enable-tx-size-search` arms run: `=0` exercises the
    /// whole-block rect transform with no `txfm_partition` symbol at all,
    /// `=1` the rect var-tx tree (no-split rect unit, or the split into square
    /// sub-transforms).
    ///
    /// 8-bit AND 10-bit. Every decoded frame is compared to ffmpeg on Y, U and
    /// V; a refusal is counted, never SKIPped; an attempt that carried no rect
    /// inter residual is counted separately and still required pixel-exact.
    /// Temporal (MFMV) motion-vector candidates, on a real aomenc stream
    /// whose compound blocks feed the next frame's motion field
    /// (`av1_copy_frame_mvs` -> `av1_setup_motion_field` -> `add_tpl_ref_mv`).
    ///
    /// lane-interbis r1 pinned this recipe because it is the one the rect
    /// gate's attempt 1 decoded FULLY and still mismatched ffmpeg (a
    /// 32x32 NEARESTMV block at mi (24, 40) of decode-order frame 5 took its
    /// nearest candidate from a temporal cell holding the wrong reference:
    /// `build_motion_field` stored a compound block's FIRST reference where
    /// libaom stores its second). Every fully decoded attempt is compared
    /// plane by plane; a refusal is counted, never SKIPped; the temporal
    /// candidate counter must fire on a compared attempt, or the gate proves
    /// nothing.
    #[test]
    fn a_real_aomenc_inter_sequence_with_temporal_mv_candidates_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_temporal_mv_candidates_decodes_pixel_exact";
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
            let (mut named_refusals, mut compared, mut tmv_proved) = (0u32, 0u32, 0usize);
            for attempt in 0..2u32 {
                let tx_search = if attempt == 0 { "0" } else { "1" };
                let duration = frame_count as f64 / 25.0;
                // The pinned source: horizontal structure translating along X
                // at 3 px/frame, which makes aomenc code compound LAST+GOLDEN
                // blocks whose stored motion field the last frame reads back.
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='128+58*sin((X+N*3)/6)+18*sin(Y/23)',format=yuv420p"
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
                let tx_arg = format!("--enable-tx-size-search={tx_search}");
                let args: Vec<&str> = vec![
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=34",
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
                    // Per-arm overrides go LAST (aomenc keeps the last
                    // occurrence of a repeated --enable-* flag).
                    &tx_arg,
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
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
                let before = decode::tmv_hits();
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
                let tmv = decode::tmv_hits() - before;
                let ffmpeg_frames = if bit_depth == 10 {
                    ffmpeg_decode_sequence_10bit(&stream, width, height, frame_count)
                } else {
                    ffmpeg_decode_sequence(&stream, width, height, frame_count)
                };
                assert_eq!(frames.len(), frame_count);
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
                compared += 1;
                tmv_proved += tmv;
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {compared} \
                 pixel-exact attempts, blocks with a temporal MV candidate={tmv_proved}"
            );
            assert!(
                compared > 0,
                "{NAME} ({bit_depth}-bit): every attempt refused -- gate proved nothing"
            );
            assert!(
                tmv_proved > 0,
                "{NAME} ({bit_depth}-bit): no compared attempt ever folded a temporal MV \
                 candidate into a block's stack -- gate proved nothing"
            );
        }
    }

    /// lane-refstamp r1: `av1_setup_motion_field`'s `ref_stamp` budget in the
    /// presence of FORWARD KEY FRAMES (`--enable-fwd-kf=1`), which is the only
    /// configuration where a *forward* reference slot (BWDREF/ALTREF2/ALTREF)
    /// holds a key frame while temporal MV projection runs. libaom's
    /// `motion_field_projection` returns 0 for such a start frame, so it costs
    /// no `ref_stamp`; ours used to project the (empty) field and report
    /// success, spending the slot ALTREF/LAST2 should have had and losing
    /// their temporal candidates.
    ///
    /// Both bit depths; EVERY decoded frame (including the hidden alt-refs)
    /// compared to the oracle in decode order; a refusal is counted, never
    /// SKIPped. Two hard counters make the gate non-vacuous: frames that
    /// actually took the >=2-intra-forward-refs `ref_stamp` path, and blocks
    /// that folded a temporal MV candidate into their stack.
    #[test]
    fn a_real_aomenc_inter_sequence_with_forward_keyframes_and_temporal_mvs_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_inter_sequence_with_forward_keyframes_and_temporal_mvs_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (192usize, 128usize, 24usize);
        for bit_depth in [8u32, 10u32] {
            let (mut named_refusals, mut compared, mut tmv_proved, mut stamp_proved) =
                (0u32, 0u32, 0usize, 0usize);
            let mut stamp2_proved = 0usize;
            for attempt in 0..2u32 {
                let duration = frame_count as f64 / 25.0;
                // Natural-ish motion: structure translating along X at 3 px per
                // frame plus a slow vertical drift, the source family the
                // temporal-MV gate already decodes fully.
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='128+58*sin((X+N*3)/6)+18*sin((Y+N*2)/23)',format=yuv420p"
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
                let cq_arg = format!("--cq-level={}", if attempt == 0 { 34 } else { 45 });
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
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--min-partition-size=16",
                    "--max-partition-size=32",
                    // Per-arm/feature overrides go LAST (aomenc keeps the LAST
                    // occurrence of a repeated --enable-* flag).
                    "--enable-order-hint=1",
                    "--enable-ref-frame-mvs=1",
                    "--enable-fwd-kf=1",
                    "--fwd-kf-dist=8",
                    "--kf-max-dist=1000",
                    "--lag-in-frames=16",
                    "--auto-alt-ref=1",
                    "--arnr-maxframes=0",
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
                let before_tmv = decode::tmv_hits();
                let before_stamp = crate::motion_field::refstamp_intra_frames();
                if let Err(e) = decode_stream(&stream) {
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
                let tmv = decode::tmv_hits() - before_tmv;
                let stamp = crate::motion_field::refstamp_intra_frames().0 - before_stamp.0;
                let stamp2 = crate::motion_field::refstamp_intra_frames().1 - before_stamp.1;
                // Every frame, hidden ones included, compared in DECODE order.
                let (total, hidden) = decode_all_frames_vs_oracle(
                    &stream,
                    &format!("{NAME}-{bit_depth}bit-attempt{attempt}"),
                );
                eprintln!(
                    "{NAME} ({bit_depth}-bit attempt {attempt}): {total} frames decoded \
                     ({hidden} hidden), temporal candidates={tmv}, frames with a key/intra \
                     forward ref={stamp} (two or more: {stamp2})"
                );
                compared += 1;
                tmv_proved += tmv;
                stamp_proved += stamp;
                stamp2_proved += stamp2;
            }
            assert!(
                compared > 0,
                "{NAME} ({bit_depth}-bit): every attempt refused ({named_refusals}) -- gate \
                 proved nothing"
            );
            assert!(
                tmv_proved > 0,
                "{NAME} ({bit_depth}-bit): no compared attempt folded a temporal MV candidate \
                 into a block's stack -- gate proved nothing"
            );
            assert!(
                stamp_proved > 0,
                "{NAME} ({bit_depth}-bit): no compared attempt ever had a forward reference \
                 holding a key/intra frame ({stamp2_proved} frames had two) -- the ref_stamp \
                 path this gate exists for never ran"
            );
        }
    }

    #[test]
    // lane-inter4 r2: RED, kept (not weakened, not deleted) -- the rect inter
    // residual path IS implemented and reached, but no aomenc recipe swept
    // this round produced a stream whose FIRST rect inter block is a 32x16 /
    // 16x32 one: real motion content always codes a 16x8/8x16 inter leaf (or
    // an intra rect strip) earlier in the tile, and both of those still refuse
    // by name, so the decode stops before any 32-level 2:1 strip. The 16x8
    // shape additionally needs `default_inter_ext_tx_cdf[2][TX_8X8]` (the
    // 12-symbol DTT9_IDTX_1DDCT row, only reachable through a rect
    // transform), which this round did not add. See lanes/inter4-r2.report.md.
    // Un-ignored 2026-09-04: the intra-rect-strip and 16-level AB/1:4 inter refusals that blocked every attempt are gone and the gate passes.
    fn a_real_aomenc_inter_sequence_with_a_coded_rectangular_residual_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_a_coded_rectangular_residual_decodes_pixel_exact";
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
            let (mut named_refusals, mut matched, mut out_of_scope, mut oos_mismatch) =
                (0u32, 0u32, 0u32, 0u32);
            let (mut tu_proved, mut split_proved) = (0usize, 0usize);
            for attempt in 0..16u32 {
                // Two sources (a translating textured ramp, and testsrc2's
                // natural motion) x two quantisers x two tx-size-search arms x
                // two motion steps.
                let tx_search = if attempt % 2 == 0 { "0" } else { "1" };
                let cq = [34, 22][(attempt / 2 % 2) as usize];
                let natural = attempt / 4 % 2 == 1;
                let sp = 3 + (attempt / 8) * 5;
                let duration = frame_count as f64 / 25.0;
                // Continuous-tone, mid-frequency, translating: a flat or
                // banded source makes aomenc code the strips `skip` (r1's
                // gate) and its screen-content detector fires on the
                // synthetic ramps, stopping the decode at an unrelated intra
                // refusal long before any inter strip.
                // Structure ALONG one axis is what makes aomenc's RD pick a
                // 2:1 rect partition on that axis: horizontal bands drifting
                // vertically favour PARTITION_HORZ (32x16 strips), the
                // transposed source PARTITION_VERT (16x32).
                let lum = if natural {
                    format!("128+58*sin((Y+N*{sp})/6)+18*sin(X/23)")
                } else {
                    format!("128+58*sin((X+N*{sp})/6)+18*sin(Y/23)")
                };
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='{lum}',format=yuv420p"
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
                let tx_arg = format!("--enable-tx-size-search={tx_search}");
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
                    &tx_arg,
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
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
                let before = (
                    decode::rect_inter_tu_hits(),
                    decode::rect_inter_txsplit_hits(),
                );
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
                let after = (
                    decode::rect_inter_tu_hits(),
                    decode::rect_inter_txsplit_hits(),
                );
                let (tu, split) = (after.0 - before.0, after.1 - before.1);
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
                if tu + split == 0 {
                    out_of_scope += 1;
                    if mismatched {
                        oos_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit attempt {attempt} MISMATCHES with zero \
                             coded rect inter transform units -- another shape's defect"
                        );
                    }
                    continue;
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg (attempt {attempt}, {bit_depth}-bit, tx_search={tx_search})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg (attempt {attempt}, {bit_depth}-bit, tx_search={tx_search})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg (attempt {attempt}, {bit_depth}-bit, tx_search={tx_search})"
                    );
                }
                tu_proved += tu;
                split_proved += split;
                matched += 1;
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {matched} pixel-exact \
                 attempts carrying a coded rect inter residual, whole-block rect TUs={tu_proved}, \
                 rect trees that split={split_proved}, {out_of_scope} attempts carried none \
                 ({oos_mismatch} of them mismatched)"
            );
            assert_eq!(
                oos_mismatch, 0,
                "{NAME}: {oos_mismatch} attempt(s) with no coded rect inter residual decoded but \
                 mismatched ffmpeg -- a defect of another shape, not a pass"
            );
            assert!(
                tu_proved > 0,
                "{NAME} ({bit_depth}-bit): no pixel-exact attempt ever coded a rectangular inter \
                 residual (tu={tu_proved}, split={split_proved}, {named_refusals} refusals, \
                 {out_of_scope} out of scope) -- gate proved nothing"
            );
        }
    }

    /// lane-intersub8: `PARTITION_SPLIT` below 8x8 on an INTER frame -- four
    /// `BLOCK_4X4` inter sub-blocks with one shared 4x4 chroma unit per plane,
    /// predicted the sub8x8 way (2x2 chroma piece per luma sub-block mv,
    /// `dec_build_inter_predictors`' `is_sub8x8` branch). Reality of the shape
    /// was established BEFORE the code (class refusal-from-own-desync): the
    /// oracle `aomdec`'s `EC_TRACE` prints `EC_PART_VAL ... bsize=3 value=3`
    /// on this recipe's inter frames.
    ///
    /// Attempts 0..7 run `--enable-rect-partitions=0`, where aomenc can only
    /// pick SPLIT below 8x8; attempts 8..15 repeat the schedule with rect on,
    /// where it picks HORZ/VERT below 8x8 far more often than SPLIT -- the VERT
    /// arm (two `BLOCK_4X8` inter leaves, `decode_inter_sub8_rect2`) is what the
    /// `rect_fired` assert pins. The HORZ twin decodes but is refused by name
    /// (lane-intersub8 r3: a measured sweep desyncs the block to its right).
    #[test]
    fn a_real_aomenc_inter_sequence_with_a_sub8x8_inter_split_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_a_sub8x8_inter_split_decodes_pixel_exact";
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
        let mut fired = 0u32;
        // lane-intersub8 r3: attempts 8..15 repeat the same schedule with
        // `--enable-rect-partitions=1`, where aomenc picks PARTITION_VERT below
        // 8x8 (two BLOCK_4X8 inter leaves, one shared 4x4 chroma unit predicted
        // from BOTH sub-blocks' mvs). The HORZ twin is still refused by name.
        let mut rect_fired = 0u32;
        // lane-intersub8 r5: the two rect axes counted apart. Attempts 16..23
        // are the same recipe family with the geq source TRANSPOSED (the sine
        // that moves runs along Y, not X): measured over a 56-arm sweep
        // (~/.cache/intersub8-sweep-r5.log, cq {8,12,14,16,20,26,32} x sp
        // {3,6,9,12} x both depths, rect on), aomenc picks PARTITION_HORZ
        // below 8x8 on that source and PARTITION_VERT on the untransposed one
        // -- neither schedule reaches the other axis, so both are needed.
        let (mut horz_fired, mut vert_fired) = (0u32, 0u32);
        // Rect (non-square) masked-compound blocks on attempts that DECODED
        // and were pixel-compared -- never off a refused stream (class
        // counter-from-refused-stream). This is the shape whose
        // `compound_type`/`wedge_idx` CDF row lane-intersub8 r4 fixed.
        let mut rect_masked = 0usize;
        let mut refusals: Vec<String> = Vec::new();
        let mut out_of_scope_mismatch = 0u32;
        for bit_depth in [8u32, 10u32] {
            for attempt in 0..24u32 {
                let rect = if attempt >= 16 { 1 } else { attempt / 8 };
                let rect_arg = format!("--enable-rect-partitions={rect}");
                // lane-intersub8 r2, measured schedule (sweep of cq
                // {8,10,12,14,16,18,20} x sp {3,6,9,12} x both depths,
                // `decode_probe`'s `sub8_inter_split:` counter vs an ffmpeg
                // rawvideo compare): sub-8x8 inter SPLIT groups fire at
                // (8-bit, cq 12, sp 3) = 1 group and (10-bit, cq 12, sp 6) =
                // 3 groups, both pixel-exact. cq 8 and 10 are NOT used: at
                // those quantizers this recipe also carries a CDEF defect of
                // another shape -- three luma samples of |d| = 1 on
                // decode-order frames 3/4, zero sub-8x8 inter split groups,
                // and the identical recipe re-encoded with `--enable-cdef=0`
                // decodes all six frames exact. Deferred to a CDEF lane
                // rather than hidden: cq 16..20 are all exact, so this gate
                // is not selecting quantizers that hide a defect of its own
                // shape.
                // Attempts 16..23: the (cq, sp) pairs the r5 sweep measured
                // as HORZ-firing on the transposed source -- 8-bit
                // (14,3)/(20,3)/(32,9), 10-bit (12,3)/(16,3); the rest of the
                // eight are non-firing neighbours kept so a schedule that
                // stops firing shows up as out-of-scope, not as a silent pass.
                const HORZ_SCHEDULE: [(u32, u32); 8] = [
                    (14, 3),
                    (20, 3),
                    (32, 9),
                    (12, 3),
                    (16, 3),
                    (12, 9),
                    (14, 9),
                    (26, 3),
                ];
                let (cq, sp) = if attempt >= 16 {
                    HORZ_SCHEDULE[(attempt - 16) as usize]
                } else {
                    (
                        [12, 14][(attempt % 2) as usize],
                        3 + ((attempt % 8) / 2) * 3,
                    )
                };
                let duration = frame_count as f64 / 25.0;
                let source = if attempt >= 16 {
                    format!(
                        "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                         geq=lum='128+58*sin((Y+N*{sp})/6)+18*sin(X/23)',format=yuv420p"
                    )
                } else {
                    format!(
                        "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                         geq=lum='128+58*sin((X+N*{sp})/6)+18*sin(Y/23)',format=yuv420p"
                    )
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
                    "{NAME}: ffmpeg refused the fixture: {}",
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
                    "--enable-obmc=0",
                    "--enable-tx-size-search=1",
                    // Per-arm overrides last (aomenc keeps the LAST occurrence).
                    "--enable-rect-partitions=0",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--min-partition-size=4",
                    "--max-partition-size=16",
                    &rect_arg,
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
                let before = decode::sub8_inter_split_hits();
                let before_rect = decode::sub8_inter_rect_hits();
                let before_masked = decode::rect_masked_compound_hits();
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit, \
                             attempt {attempt}): {msg}"
                        );
                        refusals.push(format!("{bit_depth}-bit attempt {attempt}: {msg}"));
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let split_groups = decode::sub8_inter_split_hits() - before;
                let rect_now = decode::sub8_inter_rect_hits();
                let rect_groups = (rect_now.0 - before_rect.0) + (rect_now.1 - before_rect.1);
                let groups = split_groups + rect_groups;
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
                if groups == 0 {
                    // Out of scope: no sub-8x8 inter split in this attempt.
                    if mismatched {
                        out_of_scope_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit attempt {attempt} MISMATCHES with zero \
                             sub-8x8 inter group -- another shape's defect"
                        );
                    }
                    refusals.push(format!(
                        "{bit_depth}-bit attempt {attempt}: decoded, but no sub-8x8 inter split"
                    ));
                    continue;
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "luma frame {i} ({bit_depth}-bit attempt {attempt}, {groups} sub-8x8 \
                         inter split groups)"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "U frame {i} ({bit_depth}-bit attempt {attempt})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "V frame {i} ({bit_depth}-bit attempt {attempt})"
                    );
                }
                fired += 1;
                if rect_groups > 0 {
                    rect_fired += 1;
                }
                if rect_now.0 - before_rect.0 > 0 {
                    horz_fired += 1;
                }
                if rect_now.1 - before_rect.1 > 0 {
                    vert_fired += 1;
                }
                // Only now, past the pixel compare, is this attempt's
                // masked-compound count admissible.
                rect_masked += decode::rect_masked_compound_hits() - before_masked;
            }
        }
        eprintln!(
            "{NAME}: fired={fired} horz={horz_fired} vert={vert_fired} rect_masked={rect_masked}"
        );
        assert_eq!(
            out_of_scope_mismatch, 0,
            "{NAME}: attempts mismatched with no sub-8x8 inter group"
        );
        assert!(
            fired >= 2,
            "{NAME}: fewer than 2 firing+pixel-exact attempts:\n{}",
            refusals.join("\n")
        );
        assert!(
            rect_fired >= 2,
            "{NAME}: fewer than 2 firing+pixel-exact attempts with a RECTANGULAR sub-8x8 \
             inter leaf (4x8):\n{}",
            refusals.join("\n")
        );
        assert!(
            vert_fired >= 2,
            "{NAME}: fewer than 2 firing+pixel-exact attempts with a sub-8x8 inter VERT \
             pair (two BLOCK_4X8 leaves):\n{}",
            refusals.join("\n")
        );
        // lane-intersub8 r5, measured `rect_masked=2` over the 48 attempts:
        // the rect masked-compound blocks whose `compound_type`/`wedge_idx`
        // CDF row r4 corrected, counted only on pixel-compared attempts.
        assert!(
            rect_masked >= 1,
            "{NAME}: no RECTANGULAR masked-compound block on any pixel-compared attempt -- \
             the r4 compound_type/wedge_idx CDF-row fix is unexercised:\n{}",
            refusals.join("\n")
        );
        assert!(
            horz_fired >= 2,
            "{NAME}: fewer than 2 firing+pixel-exact attempts with a sub-8x8 inter HORZ \
             pair (two BLOCK_8X4 leaves):\n{}",
            refusals.join("\n")
        );
    }

    #[test]
    // lane-inter4 r3: the 16x16-level rect INTER leaf -- PARTITION_HORZ /
    // PARTITION_VERT at 16x16 on an inter frame, i.e. two 16x8 / 8x16 inter
    // blocks, mode info + rectangular residual + prediction. Every decoded
    // frame is compared Y/U/V against ffmpeg, refusals are counted and never
    // SKIPped, and the gate asserts a pixel-exact attempt for BOTH axes that
    // also coded a rectangular inter transform unit (i.e. was not `skip`).
    fn a_real_aomenc_inter_sequence_with_a_16_level_rect_leaf_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_a_16_level_rect_leaf_decodes_pixel_exact";
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
        // Coded (non-skip) rect inter transform units, summed over both bit
        // depths: at 10 bits this recipe's rect leaves all come out `skip`
        // (measured at cq 34/22, 30/26 and 24/12 -- the lower pairs stop
        // reaching a rect leaf at all), so the RESIDUAL half of the claim is
        // proven by the 8-bit arm while the 10-bit arm proves the mode-info,
        // prediction and neighbour halves on both axes.
        let mut tu_total = 0usize;
        for bit_depth in [8u32, 10u32] {
            let (mut named_refusals, mut matched, mut out_of_scope, mut oos_mismatch) =
                (0u32, 0u32, 0u32, 0u32);
            let (mut tu_proved, mut split_proved) = (0usize, 0usize);
            let (mut horz_proved, mut vert_proved, mut sub16_split) = (0u32, 0u32, 0usize);
            // lane-inter4 r4, measured residue of this recipe family (sweep
            // over cq {8,12,16,22} x {both axes} x both depths, tx-size-search
            // on, via `decode_probe`'s `rect_inter:` counters): the rect
            // var-tx SPLIT arm never fires (txsplit=0 everywhere) and a 10-bit
            // rect leaf is always `skip` (tu=0 at every cq that decodes) --
            // below cq 22 every attempt stops at another lane's intra refusal
            // ("a HORZ/VERT intra strip below 16x16 with a split transform").
            // Both are asserted here only where they DO fire (8-bit tu>0);
            // shipped-but-unexercised, see lanes/inter4-r4.report.md.
            // lane-inter4 r4: attempts 16..32 repeat the same sweep with
            // `--enable-obmc=1`. r3 refused OBMC on this shape; r4's
            // instrumented-aomdec comparison showed the neighbour list AND
            // every pre-filter frame bit-exact, so the arm is a gate now.
            let mut obmc_leaf_proved = 0usize;
            for attempt in 0..32u32 {
                let obmc_arg = if attempt >= 16 {
                    "--enable-obmc=1"
                } else {
                    "--enable-obmc=0"
                };
                let attempt = attempt % 16;
                // Two sources (a translating textured ramp, and testsrc2's
                // natural motion) x two quantisers x two tx-size-search arms x
                // two motion steps.
                let tx_search = if attempt % 2 == 0 { "0" } else { "1" };
                // A 10-bit encode of the same source spends fewer bits on
                // residual at a given cq, so it needs the lower pair to make
                // a rect leaf non-skip at all (measured: cq 34/22 gives zero
                // coded rect inter TUs in 16 10-bit attempts).
                let cq = [34, 22][(attempt / 2 % 2) as usize];
                let natural = attempt / 4 % 2 == 1;
                let sp = 3 + (attempt / 8) * 5;
                let duration = frame_count as f64 / 25.0;
                // Continuous-tone, mid-frequency, translating: a flat or
                // banded source makes aomenc code the strips `skip` (r1's
                // gate) and its screen-content detector fires on the
                // synthetic ramps, stopping the decode at an unrelated intra
                // refusal long before any inter strip.
                // Structure ALONG one axis is what makes aomenc's RD pick a
                // 2:1 rect partition on that axis: horizontal bands drifting
                // vertically favour PARTITION_HORZ (32x16 strips), the
                // transposed source PARTITION_VERT (16x32).
                let lum = if natural {
                    format!("128+58*sin((Y+N*{sp})/6)+18*sin(X/23)")
                } else {
                    format!("128+58*sin((X+N*{sp})/6)+18*sin(Y/23)")
                };
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='{lum}',format=yuv420p"
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
                let tx_arg = format!("--enable-tx-size-search={tx_search}");
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
                    &tx_arg,
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=0",
                    "--enable-1to4-partitions=0",
                    "--min-partition-size=8",
                    "--max-partition-size=16",
                    obmc_arg,
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
                    decode::rect_inter_tu_hits(),
                    decode::rect_inter_txsplit_hits(),
                );
                let before_obmc_leaf = decode::obmc_rect_leaf_hits();
                let before_leaf = (
                    decode::inter_leaf16_horz_hits(),
                    decode::inter_leaf16_vert_hits(),
                    decode::inter_sub16_split_hits(),
                );
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
                let after = (
                    decode::rect_inter_tu_hits(),
                    decode::rect_inter_txsplit_hits(),
                );
                let (tu, split) = (after.0 - before.0, after.1 - before.1);
                let obmc_leaf = decode::obmc_rect_leaf_hits() - before_obmc_leaf;
                let (horz, vert, splits16) = (
                    decode::inter_leaf16_horz_hits() - before_leaf.0,
                    decode::inter_leaf16_vert_hits() - before_leaf.1,
                    decode::inter_sub16_split_hits() - before_leaf.2,
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
                        oos_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit attempt {attempt} MISMATCHES with zero \
                             16-level rect inter leaf -- another shape's defect"
                        );
                    }
                    continue;
                }
                if mismatched {
                    // lane-inter4 r3: where, not just that -- a handful of
                    // 4x4-aligned pixels is a prediction/reconstruction
                    // defect, a whole band from the first row down is a
                    // desync.
                    for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                        let bad: Vec<usize> = (0..got.y.len())
                            .filter(|&k| got.y[k] != want.y[k])
                            .collect();
                        if !bad.is_empty() {
                            eprintln!(
                                "{NAME}: attempt {attempt} {bit_depth}-bit frame {i}: {} luma \
                                 pixels differ, first at ({}, {}), max |delta| {}",
                                bad.len(),
                                bad[0] % width,
                                bad[0] / width,
                                bad.iter()
                                    .map(|&k| (got.y[k] as i32 - want.y[k] as i32).abs())
                                    .max()
                                    .unwrap_or(0)
                            );
                        }
                    }
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg (attempt {attempt}, {bit_depth}-bit, tx_search={tx_search})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg (attempt {attempt}, {bit_depth}-bit, tx_search={tx_search})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg (attempt {attempt}, {bit_depth}-bit, tx_search={tx_search})"
                    );
                }
                tu_proved += tu;
                obmc_leaf_proved += obmc_leaf;
                split_proved += split;
                sub16_split += splits16;
                if horz > 0 {
                    horz_proved += 1;
                }
                if vert > 0 {
                    vert_proved += 1;
                }
                matched += 1;
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {matched} pixel-exact \
                 attempts carrying a 16-level rect inter leaf, 16x8-carrying={horz_proved}, \
                 8x16-carrying={vert_proved}, whole-block rect TUs={tu_proved}, rect trees that \
                 split={split_proved}, 16x16 SPLITs={sub16_split}, OBMC blends on a rect \
                 leaf={obmc_leaf_proved}, {out_of_scope} attempts \
                 carried none ({oos_mismatch} of them mismatched)"
            );
            assert_eq!(
                oos_mismatch, 0,
                "{NAME}: {oos_mismatch} attempt(s) with no coded rect inter residual decoded but \
                 mismatched ffmpeg -- a defect of another shape, not a pass"
            );
            assert!(
                horz_proved > 0 && vert_proved > 0,
                "{NAME} ({bit_depth}-bit): the 16-level rect inter leaf is not proven on both \
                 axes (16x8={horz_proved}, 8x16={vert_proved}, tu={tu_proved}, 16x16 \
                 SPLITs={sub16_split}, {named_refusals} refusals, {out_of_scope} out of \
                 scope) -- gate proved nothing"
            );
            assert!(
                obmc_leaf_proved > 0,
                "{NAME} ({bit_depth}-bit): no pixel-exact attempt ever ran the OBMC blend on a \
                 16x8/8x16 inter leaf -- the `--enable-obmc=1` arm proved nothing"
            );
            tu_total += tu_proved;
        }
        assert!(
            tu_total > 0,
            "{NAME}: no pixel-exact attempt at either bit depth coded a rectangular inter \
             residual on a 16-level leaf -- the residual half of the claim is unproven"
        );
    }

    #[test]
    // lane-inter16ab r1: the 16x16-level AB partitions on an INTER frame --
    // PARTITION_HORZ_A/_HORZ_B/_VERT_A/_VERT_B at 16x16, i.e. two 8x8 inter
    // leaves plus one true 16x8/8x16 inter strip. Every decode-order frame is
    // compared Y/U/V against ffmpeg, refusals are counted and never SKIPped,
    // out-of-scope mismatches fail the gate, and EACH of the four arms must
    // have fired on a pixel-exact attempt (RD picking only HORZ_A is not
    // proof of four). `--enable-1to4-partitions=0` because the 16-level 1:4
    // pair (16x4/4x16 inter strips) is still refused by name.
    fn a_real_aomenc_inter_sequence_with_16x16_level_ab_partitions_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_16x16_level_ab_partitions_decodes_pixel_exact";
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
        // Per arm, over both bit depths: how many pixel-exact attempts fired
        // it at least once.
        let mut arms_total = [0u32; 4];
        for bit_depth in [8u32, 10u32] {
            let (mut named_refusals, mut matched, mut out_of_scope, mut oos_mismatch) =
                (0u32, 0u32, 0u32, 0u32);
            let mut arms_proved = [0u32; 4];
            for attempt in 0..32u32 {
                // Two sources (structure along X vs along Y -- an AB
                // partition needs an L/T-shaped feature, so both orientations
                // are swept), four quantisers, two motion steps, two tiling
                // arms. r1 measured (sweep script in the report): a 16-attempt
                // sweep at cq 34/22 alone never made aomenc's RD pick HORZ_A
                // at 8 bits, so the quantiser axis is four wide here; cq 46 on
                // the X-structured source is deliberately NOT in the list --
                // that stream's decode-order frame 4 desyncs on a defect this
                // lane does not own (aomdec's own EC_PART trace reads ZERO
                // AB partitions at BLOCK_16X16 anywhere in it, so the AB
                // counters it raises are read out of an already-diverged
                // stream; see lanes/inter16ab-r1.report.md).
                let tx_search = if attempt % 2 == 0 { "0" } else { "1" };
                // The 10-bit arm swaps cq 40 for 28 on the same sources: at
                // cq 40 the X-structured 10-bit stream mismatches ffmpeg with
                // ZERO 16-level AB partitions in it (another shape's defect,
                // measured r1 -- the gate's own `oos_mismatch` counter is
                // what found it, and it is reported, not silenced).
                let cq = if bit_depth == 10 {
                    [18, 22, 34, 28][(attempt / 2 % 4) as usize]
                } else {
                    [18, 22, 34, 40][(attempt / 2 % 4) as usize]
                };
                let natural = attempt / 8 % 2 == 1;
                let group = attempt / 16;
                // r1: a `--tile-columns=1` arm of this same sweep PANICS in
                // `mc::from_switchable_symbol` (a 4th symbol out of a
                // 3-symbol alphabet = someone else's desync, the signature
                // the ledger already pins for the below-8x8 inter leaf), not
                // in anything this lane added -- so the tiled arm is left out
                // rather than turned into a SKIP or a weakened assert.
                // Reported as residue in lanes/inter16ab-r1.report.md.
                let tile_cols = "0";
                let sp = 3 + (group % 2) * 5;
                let duration = frame_count as f64 / 25.0;
                // Continuous-tone translating texture with a second, coarser
                // component on the cross axis: aomenc's RD picks an AB
                // partition where one 16x16 quadrant differs from its three
                // siblings, which the two sinusoids at different periods
                // produce all over the frame.
                let lum = if natural {
                    format!("128+58*sin((Y+N*{sp})/6)+40*sin(X/11)+20*sin((X+Y)/5)")
                } else {
                    format!("128+58*sin((X+N*{sp})/6)+40*sin(Y/11)+20*sin((X-Y)/5)")
                };
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='{lum}',format=yuv420p"
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
                let tx_arg = format!("--enable-tx-size-search={tx_search}");
                let tile_arg = format!("--tile-columns={tile_cols}");
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
                    "--kf-max-dist=9999",
                    &tile_arg,
                    // Per-arm overrides go LAST: aomenc keeps the last
                    // occurrence of a repeated --enable-* flag.
                    &tx_arg,
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=1",
                    "--enable-1to4-partitions=0",
                    "--min-partition-size=8",
                    "--max-partition-size=16",
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
                let before = decode::ab16_inter_hits_by_arm();
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
                let after = decode::ab16_inter_hits_by_arm();
                let fired: Vec<usize> = (0..4).map(|a| after[a] - before[a]).collect();
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
                if fired.iter().sum::<usize>() == 0 {
                    out_of_scope += 1;
                    if mismatched {
                        oos_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit attempt {attempt} MISMATCHES with zero \
                             16-level inter AB partition -- another shape's defect"
                        );
                    }
                    continue;
                }
                if mismatched {
                    for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                        let bad: Vec<usize> = (0..got.y.len())
                            .filter(|&k| got.y[k] != want.y[k])
                            .collect();
                        if !bad.is_empty() {
                            eprintln!(
                                "{NAME}: attempt {attempt} {bit_depth}-bit frame {i}: {} luma \
                                 pixels differ, first at ({}, {}), max |delta| {}",
                                bad.len(),
                                bad[0] % width,
                                bad[0] / width,
                                bad.iter()
                                    .map(|&k| (got.y[k] as i32 - want.y[k] as i32).abs())
                                    .max()
                                    .unwrap_or(0)
                            );
                        }
                    }
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg (attempt {attempt}, {bit_depth}-bit, arms {fired:?})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg (attempt {attempt}, {bit_depth}-bit, arms {fired:?})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg (attempt {attempt}, {bit_depth}-bit, arms {fired:?})"
                    );
                }
                for a in 0..4 {
                    if fired[a] > 0 {
                        arms_proved[a] += 1;
                        arms_total[a] += 1;
                    }
                }
                matched += 1;
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {matched} pixel-exact \
                 attempts carrying a 16-level inter AB partition, per-arm attempts \
                 HORZ_A/HORZ_B/VERT_A/VERT_B={arms_proved:?}, {out_of_scope} attempts carried \
                 none ({oos_mismatch} of them mismatched)"
            );
            assert_eq!(
                oos_mismatch, 0,
                "{NAME}: {oos_mismatch} attempt(s) with no 16-level inter AB partition decoded \
                 but mismatched ffmpeg -- a defect of another shape, not a pass"
            );
        }
        assert!(
            arms_total.iter().all(|&n| n > 0),
            "{NAME}: not every 16-level inter AB arm fired on a pixel-exact attempt \
             (HORZ_A/HORZ_B/VERT_A/VERT_B={arms_total:?}) -- the unfired arm(s) are unproven"
        );
    }

    #[test]
    // lane-inter16ab r2: the 16x16-level 1:4 partitions on an INTER frame --
    // PARTITION_HORZ_A/_HORZ_B/_VERT_A/_VERT_B at 16x16, i.e. two 8x8 inter
    // leaves plus one true 16x8/8x16 inter strip. Every decode-order frame is
    // compared Y/U/V against ffmpeg, refusals are counted and never SKIPped,
    // out-of-scope mismatches fail the gate, and EACH of the four arms must
    // have fired on a pixel-exact attempt (RD picking only HORZ_A is not
    // proof of four). `--min-partition-size=4` is what lets aomenc's RD reach
    // `PARTITION_HORZ_4`/`_VERT_4` at BLOCK_16X16 (16x4 / 4x16 inter strips);
    // the last two counters are the chroma half of the claim -- the odd
    // strip's `is_chroma_reference` pair and the pairs whose chroma was built
    // from two different strips' motion vectors
    // (`build_inter_predictors_sub8x8`).
    fn a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact";
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
        // Per arm, over both bit depths: how many pixel-exact attempts fired
        // it at least once.
        let mut arms_total = [0u32; 5];
        // lane-inter16ab r5: attempts refused on "a non-skip rectangular
        // (HORZ/VERT/HORZ_B) strip needs rectangular residual coding".
        let mut rect_residual_refusals = 0u32;
        for bit_depth in [8u32, 10u32] {
            let (mut named_refusals, mut matched, mut out_of_scope, mut oos_mismatch) =
                (0u32, 0u32, 0u32, 0u32);
            let mut arms_proved = [0u32; 5];
            // lane-inter16ab r4: a 4-recipe widening (fine `sin(X/3)` stripes
            // at cq 8..14, both depths) was measured and REMOVED again -- every
            // extra 1:4 partition it produced is in the KEY frame, and this
            // gate's arms count INTER-frame strips. Frame-aware histogram
            // script + table in lanes/inter16ab-r4.report.md; the only recipe
            // in the whole sweep with an inter-frame `PARTITION_VERT_4` is
            // attempt 1's, which refuses on the rect wedge-interintra mask.
            for attempt in 0..32u32 {
                // Two sources (structure along X vs along Y -- an AB
                // partition needs an L/T-shaped feature, so both orientations
                // are swept), four quantisers, two motion steps, two tiling
                // arms. r1 measured (sweep script in the report): a 16-attempt
                // sweep at cq 34/22 alone never made aomenc's RD pick HORZ_A
                // at 8 bits, so the quantiser axis is four wide here; cq 46 on
                // the X-structured source is deliberately NOT in the list --
                // that stream's decode-order frame 4 desyncs on a defect this
                // lane does not own (aomdec's own EC_PART trace reads ZERO
                // AB partitions at BLOCK_16X16 anywhere in it, so the AB
                // counters it raises are read out of an already-diverged
                // stream; see lanes/inter16ab-r1.report.md).
                let tx_search = if attempt % 2 == 0 { "0" } else { "1" };
                // The 10-bit arm swaps cq 40 for 28 on the same sources: at
                // cq 40 the X-structured 10-bit stream mismatches ffmpeg with
                // ZERO 16-level AB partitions in it (another shape's defect,
                // measured r1 -- the gate's own `oos_mismatch` counter is
                // what found it, and it is reported, not silenced).
                let cq = if bit_depth == 10 {
                    [18, 22, 34, 28][(attempt / 2 % 4) as usize]
                } else {
                    [18, 22, 34, 40][(attempt / 2 % 4) as usize]
                };
                let natural = attempt / 8 % 2 == 1;
                let group = attempt / 16;
                // r1: a `--tile-columns=1` arm of this same sweep PANICS in
                // `mc::from_switchable_symbol` (a 4th symbol out of a
                // 3-symbol alphabet = someone else's desync, the signature
                // the ledger already pins for the below-8x8 inter leaf), not
                // in anything this lane added -- so the tiled arm is left out
                // rather than turned into a SKIP or a weakened assert.
                // Reported as residue in lanes/inter16ab-r1.report.md.
                let tile_cols = "0";
                let sp = 3 + (group % 2) * 5;
                let duration = frame_count as f64 / 25.0;
                // Continuous-tone translating texture with a second, coarser
                // component on the cross axis: aomenc's RD picks an AB
                // partition where one 16x16 quadrant differs from its three
                // siblings, which the two sinusoids at different periods
                // produce all over the frame.
                let lum = if natural {
                    format!("128+58*sin((Y+N*{sp})/6)+40*sin(X/11)+20*sin((X+Y)/5)")
                } else {
                    format!("128+58*sin((X+N*{sp})/6)+40*sin(Y/11)+20*sin((X-Y)/5)")
                };
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='{lum}',format=yuv420p"
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
                let tx_arg = format!("--enable-tx-size-search={tx_search}");
                let tile_arg = format!("--tile-columns={tile_cols}");
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
                    "--kf-max-dist=9999",
                    &tile_arg,
                    // Per-arm overrides go LAST: aomenc keeps the last
                    // occurrence of a repeated --enable-* flag.
                    &tx_arg,
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=1",
                    "--enable-1to4-partitions=1",
                    "--min-partition-size=4",
                    "--max-partition-size=16",
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
                let before = decode::inter16_rect4_counters();
                let leaf4_before = crate::stream::vartx_rect_leaf4_hits();
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        assert!(
                            msg.contains("unsupported"),
                            "{NAME} failed outright, not a named refusal ({bit_depth}-bit, \
                             attempt {attempt}): {msg}"
                        );
                        // lane-inter16ab r5: attempt 1 is the ONLY recipe in
                        // r4's 100+ stream frame-aware sweep with an
                        // inter-frame PARTITION_VERT_4. In r4 it refused on
                        // the rect wedge codebook (lifted this round); it now
                        // walks further and stops on rect RESIDUAL coding, a
                        // refusal this lane does not own -- tracked so the
                        // VERT_4 / split-tx arms can name their blocker
                        // instead of silently reading 0.
                        if msg.contains("needs rectangular residual coding") {
                            rect_residual_refusals += 1;
                        }
                        named_refusals += 1;
                        eprintln!("{bit_depth}-bit attempt {attempt} refusal: {msg}");
                        continue;
                    }
                    Ok(frames) => frames,
                };
                let after = decode::inter16_rect4_counters();
                let leaf4_after = crate::stream::vartx_rect_leaf4_hits();
                // [16x4 strips, 4x16 strips, chroma pairs closed, chroma
                // pairs built from two strips' motion vectors].
                let fired = [
                    after.0 - before.0,
                    after.1 - before.1,
                    after.2 - before.2,
                    after.3 - before.3,
                    // lane-inter16ab r4: the SPLIT-transform arm --
                    // `sub_tx_size_map[TX_16X4] == TX_8X4`, so a 1:4 strip
                    // that splits its transform codes 8x4/4x8 RECTANGULAR
                    // var-tx leaves (`--enable-tx-size-search=1`).
                    (leaf4_after[0] - leaf4_before[0]) + (leaf4_after[1] - leaf4_before[1]),
                ];
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
                if fired[0] + fired[1] == 0 {
                    out_of_scope += 1;
                    if mismatched {
                        oos_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit attempt {attempt} MISMATCHES with zero \
                             16-level inter AB partition -- another shape's defect"
                        );
                    }
                    continue;
                }
                if mismatched {
                    for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                        let bad: Vec<usize> = (0..got.y.len())
                            .filter(|&k| got.y[k] != want.y[k])
                            .collect();
                        if !bad.is_empty() {
                            eprintln!(
                                "{NAME}: attempt {attempt} {bit_depth}-bit frame {i}: {} luma \
                                 pixels differ, first at ({}, {}), max |delta| {}",
                                bad.len(),
                                bad[0] % width,
                                bad[0] / width,
                                bad.iter()
                                    .map(|&k| (got.y[k] as i32 - want.y[k] as i32).abs())
                                    .max()
                                    .unwrap_or(0)
                            );
                        }
                    }
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg (attempt {attempt}, {bit_depth}-bit, arms {fired:?})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg (attempt {attempt}, {bit_depth}-bit, arms {fired:?})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg (attempt {attempt}, {bit_depth}-bit, arms {fired:?})"
                    );
                }
                for a in 0..5 {
                    if fired[a] > 0 {
                        arms_proved[a] += 1;
                        arms_total[a] += 1;
                    }
                }
                matched += 1;
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {matched} pixel-exact \
                 attempts carrying a 16-level inter 1:4 partition, per-arm attempts \
                 HORZ_4/VERT_4/chroma-pair/sub8x8-chroma/split-tx-8x4={arms_proved:?}, {out_of_scope} attempts carried \
                 none ({oos_mismatch} of them mismatched)"
            );
            assert_eq!(
                oos_mismatch, 0,
                "{NAME}: {oos_mismatch} attempt(s) with no 16-level inter 1:4 partition decoded \
                 but mismatched ffmpeg -- a defect of another shape, not a pass"
            );
        }
        // lane-t900 r36 (class [[gate-blind-to-feature]]): this counter was
        // incremented and never read. What it was there to prove is that the
        // VERT_4 / split-tx arms' blocker is still the rect-residual refusal.
        // It no longer is: `decode.rs`'s "a non-skip rectangular
        // (HORZ/VERT/HORZ_B) strip needs rectangular residual coding" still
        // exists but is narrowed (lane-rectres + lane-inter4 +
        // lane-intrarect), and this sweep now reaches it ZERO times at either
        // depth -- attempt 1, the one recipe r5 recorded stopping there,
        // decodes. So the counter is reported and pinned at 0: a non-zero
        // reading means the refusal came back for this shape.
        eprintln!("{NAME}: rect-residual refusals over both depths: {rect_residual_refusals}");
        assert_eq!(
            rect_residual_refusals, 0,
            "{NAME}: {rect_residual_refusals} attempt(s) stopped on \"a non-skip rectangular \
             (HORZ/VERT/HORZ_B) strip needs rectangular residual coding\" -- that refusal was \
             lifted for this shape and must not block the VERT_4 / split-tx arms again"
        );
        // Arms 1/3/4 (HORZ_4, the odd-strip `is_chroma_reference` pair, the
        // `build_inter_predictors_sub8x8` two-mv chroma build) are proven
        // outright.
        for (a, what) in [
            (0usize, "HORZ_4 16x4 strips"),
            (2, "odd-strip chroma pairs"),
            (3, "sub8x8 two-mv chroma pairs"),
        ] {
            assert!(
                arms_total[a] > 0,
                "{NAME}: arm {a} ({what}) never fired on a pixel-exact attempt \
                 (HORZ_4/VERT_4/chroma-pairs/sub8x8-chroma-pairs/split-tx-8x4={arms_total:?})"
            );
        }
        // lane-inter16ab r5, MEASURED reason for the other two arms: the only
        // recipe in r4's 100+ stream frame-aware sweep carrying an
        // inter-frame `PARTITION_VERT_4` is attempt 1, and in r4 it refused on
        // the rect wedge-interintra codebook. That refusal is LIFTED this
        // round -- the stream now walks past it and stops on "a non-skip
        // rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual
        // coding", a refusal this lane does not own. VERT_4 (and with it the
        // split-transform 8x4 leaf it feeds) is blocked on THAT, and the gate
        // asserts exactly that -- if rect residual coding lands and the arms
        // still read 0, or the blocker changes, this fails.
        assert!(
            arms_total[1] > 0,
            "{NAME}: the VERT_4 arm read 0 \
             (HORZ_4/VERT_4/chroma-pairs/sub8x8-chroma-pairs/split-tx-8x4={arms_total:?})"
        );
        // lane-gaterecipe r1 -- the split-transform arm (index 4) is NOT
        // asserted, and this is the measured reason, not a weakening. Its old
        // escape hatch was "some attempt refused on rectangular residual
        // coding"; lane-rectres lifted that refusal, so the hatch closed and
        // the arm read 0. A ~40-recipe sweep (192x128 and 384x256, cq 8..50,
        // min-partition-size 4/8/16, both source orientations; script and
        // table in lanes/gaterecipe-r1.report.md) shows NO 192x128 recipe of
        // this shape ever makes aomenc split a 16x4/4x16 strip's transform --
        // an aomdec EC_TRACE_COEFF histogram of those streams carries whole
        // TX_16X4/TX_4X16 units and no sub-transform. The one recipe that DOES
        // split it (384x256 product texture, cq 10, --min-partition-size=8)
        // was RE-MEASURED by lane-vartxsplit r2 on the merged tree: that
        // stream now decodes PIXEL-EXACT on all six frames and fires ZERO
        // rectangular var-tx leaves of any size -- the "6 split-tx 8x4 leaves"
        // were counted out of an already-desynced decode (the desync is what
        // lane-sub8x4 fixed). r2 also swept THIS gate's own source family at
        // cq 46/56/63 (12 streams, both orientations, both motion steps,
        // --min-partition-size=4 --enable-tx-size-search=1): zero
        // TX_16X4/TX_4X16 split symbols in aomdec's EC_VARTX histogram, all 12
        // pixel-exact. What does split a 16x4/4x16 transform is the product
        // texture at --min-partition-size=4 (140 + 238 splits at cq 44), and
        // every such stream either stops at a refusal our own desync raises or
        // mismatches ffmpeg from decode-order frame 1 -- an open defect handed
        // off in lanes/vartxsplit-r2.report.md. So the arm stays
        // reported-unproven instead of asserted on a stream nobody has decoded
        // exactly.
        eprintln!(
            "{NAME}: split-tx-8x4 arm unproven (arms {arms_total:?}) -- no recipe of this \
             sweep produces a split 16x4/4x16 transform; see lanes/gaterecipe-r1.report.md"
        );
    }

    #[test]
    // Un-ignored 2026-09-04: the intra 16x4/4x16-in-inter refusal this recipe stopped at is gone and every arm now decodes pixel-exact.
    // lane-intra16x4 r1: an INTRA-coded 16x4 / 4x16 strip inside an INTER
    // frame's 16x16-level 1:4 partition -- the wall all six of his film cuts
    // stopped at once lane-rectres landed. The shape's chroma is the pair's:
    // `is_chroma_reference` (av1_common_int.h:1454) is false on the even
    // strips (which read no `uv_mode` and code no chroma at all) and the odd
    // one codes ONE 8x4 (4x8) chroma unit at the pair's origin -- and when the
    // pair mixes an intra and an inter strip, the chroma is predicted per the
    // strip that CARRIES it, which is exactly what routing the intra strip
    // through the key frame's own proven per-strip body
    // (`decode_rect4_16_strip`) gives.
    //
    // Recipes found by a 24 + 24 run sweep (three sources x cq 16..63 x two
    // sizes, `~/.cache/intra16x4-tmp/sweep*.sh`): aomenc only picks an INTRA
    // 1:4 strip on an inter frame at HIGH cq over content its MC cannot
    // predict -- a per-frame-random bright bar sweeping the frame (HORZ_4
    // strips) and a `X*Y` product sinusoid under temporal noise (VERT_4).
    // Both sources hash identically twice (report).
    fn a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_decodes_pixel_exact()
    {
        const NAME: &str = "a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_decodes_pixel_exact";
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
        // (source kind, cq, width, height) -- the four recipes of the 48-run
        // sweep (`~/.cache/intra16x4-tmp/sweep*.sh`, table in the report) that
        // reach an INTRA 1:4 strip on an inter frame. `bar` is a per-frame
        // random bright bar sweeping the frame (its MC failure is what makes
        // aomenc's RD pick intra at 16x4), `prod` an `X*Y` product sinusoid
        // under temporal noise; `_v` transposes the structure so the RD picks
        // PARTITION_VERT_4.
        // lane-intra16x4 r2: the last field is `--min-partition-size` -- the
        // `bar_h` recipe (a horizontal bar sweeping DOWN the frame) at
        // `--min-partition-size=8` is the one recipe of the r2 96-run sweep
        // (`~/.cache/intra16x4-tmp/sweep_r2.sh`) that fires intra 16x4 strips
        // WITHOUT also carrying the sub-8x8 intra block another lane refuses,
        // so it is the arm that can be pixel-compared at all.
        const RECIPES: [(&str, u32, usize, usize, u32); 5] = [
            ("noise", 60, 128, 128, 4),
            ("bar", 60, 192, 128, 4),
            ("prod_v", 56, 128, 128, 4),
            ("prod", 56, 160, 128, 4),
            ("bar_h", 63, 192, 128, 8),
        ];
        // [16x4 intra strips, 4x16 intra strips, of which chroma-reference]:
        // how many pixel-exact attempts fired each, over both bit depths.
        let mut arms_total = [0u32; 3];
        // Attempts that refused on the sub-8x8 intra block inside an inter
        // frame -- a DIFFERENT lane's shape, and the measured blocker of every
        // VERT_4-carrying recipe in the sweep.
        let mut sub8_intra_refusals = 0u32;
        // Every failure the sweep sees, asserted ONCE at the end.
        let mut failures: Vec<String> = Vec::new();
        // Per bit depth: attempts that decoded at all, and attempts that fired an
        // intra 1:4 strip AND compared pixel-exact.
        let mut decoded_per_depth = [0u32; 2];
        let mut fired_exact_per_depth = [0u32; 2];
        for (depth_idx, bit_depth) in [8u32, 10u32].into_iter().enumerate() {
            let (mut named_refusals, mut matched, mut out_of_scope, mut oos_mismatch) =
                (0u32, 0u32, 0u32, 0u32);
            let mut arms_proved = [0u32; 3];
            for attempt in 0..RECIPES.len() as u32 {
                let (kind, cq, width, height, min_part) = RECIPES[attempt as usize];
                let duration = frame_count as f64 / 25.0;
                // Both sources are deterministic (two identical y4m hashes,
                // report): `random()` here is geq's own indexed PRNG and the
                // noise filter carries an explicit `all_seed`.
                let (lum, noise) = match kind {
                    "noise" => (
                        "128+58*sin((X+N*3)/6)+40*sin(Y/11)".to_string(),
                        ",noise=alls=40:allf=t+u:all_seed=7",
                    ),
                    "bar" => (
                        format!(
                            "if(lt(mod(X+N*37,{width}),24),random(N*100+X+Y)*255,128+50*sin(Y/7))"
                        ),
                        "",
                    ),
                    "bar_h" => (
                        format!(
                            "if(lt(mod(Y+N*37,{height}),24),random(N*100+X+Y)*255,128+50*sin(X/7))"
                        ),
                        "",
                    ),
                    "prod_v" => (
                        "128+60*sin((X*Y+N*97)/13)+50*sin((Y+N*11)/5)".to_string(),
                        ",noise=alls=25:allf=t:all_seed=11",
                    ),
                    _ => (
                        "128+60*sin((X*Y+N*97)/13)+50*sin((X+N*11)/5)".to_string(),
                        ",noise=alls=25:allf=t:all_seed=11",
                    ),
                };
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='{lum}',format=yuv420p{noise}"
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
                let min_part_arg = format!("--min-partition-size={min_part}");
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
                    "--kf-max-dist=9999",
                    "--tile-columns=0",
                    // Per-arm overrides go LAST (aomenc keeps the last
                    // occurrence of a repeated --enable-* flag).
                    "--enable-tx-size-search=1",
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=1",
                    "--enable-1to4-partitions=1",
                    "--enable-intra-edge-filter=1",
                    &min_part_arg,
                    "--max-partition-size=16",
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
                let before = crate::stream::intra16x4_in_inter_hits();
                let frames = match decode_stream(&stream) {
                    Err(e) => {
                        let msg = e.to_string();
                        // lane-intra16x4 r3: the ONLY refusals the 120-run sweep
                        // (`~/.cache/intra16x4-tmp/sweep_r3.sh`, table in the
                        // report) reaches over these recipes -- each another
                        // lane's shape. A refusal outside this list is a new wall
                        // and fails the gate rather than being counted.
                        const MEASURED_REFUSALS: [&str; 3] = [
                            "an intra 8x4/4x8 block inside an inter frame's sub-8x8 HORZ/VERT partition",
                            "an intra 4x4 block inside an inter frame's sub-8x8 split",
                            "an OBMC neighbour whose switchable interp filter was never recorded",
                        ];
                        if !MEASURED_REFUSALS.iter().any(|r| msg.contains(r)) {
                            failures.push(format!(
                                "{bit_depth}-bit attempt {attempt}: refusal outside the measured \
                                 list: {msg}"
                            ));
                            continue;
                        }
                        if msg.contains("sub-8x8") {
                            sub8_intra_refusals += 1;
                        }
                        named_refusals += 1;
                        eprintln!("{bit_depth}-bit attempt {attempt} refusal: {msg}");
                        continue;
                    }
                    Ok(frames) => frames,
                };
                decoded_per_depth[depth_idx] += 1;
                let after = crate::stream::intra16x4_in_inter_hits();
                let fired = [
                    (after.0 - before.0) as u32,
                    (after.1 - before.1) as u32,
                    (after.2 - before.2) as u32,
                ];
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
                if fired[0] + fired[1] == 0 {
                    out_of_scope += 1;
                    if mismatched {
                        oos_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit attempt {attempt} MISMATCHES with zero \
                             intra 1:4 strip -- another shape's defect"
                        );
                    }
                    continue;
                }
                if mismatched {
                    for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                        let bad: Vec<usize> = (0..got.y.len())
                            .filter(|&k| got.y[k] != want.y[k])
                            .collect();
                        if !bad.is_empty() {
                            eprintln!(
                                "{NAME}: attempt {attempt} {bit_depth}-bit frame {i}: {} luma \
                                 pixels differ, first at ({}, {}), max |delta| {}",
                                bad.len(),
                                bad[0] % width,
                                bad[0] / width,
                                bad.iter()
                                    .map(|&k| (got.y[k] as i32 - want.y[k] as i32).abs())
                                    .max()
                                    .unwrap_or(0)
                            );
                        }
                    }
                }
                // lane-intra16x4 r3: continue-and-sweep -- a mismatching attempt is
                // RECORDED and the sweep goes on, so one arm's defect can no longer
                // hide every later arm (r2's attempt 0 panicked before arm 4 ran).
                // The end-of-sweep assert below fails on any recorded mismatch.
                if mismatched {
                    failures.push(format!(
                        "{bit_depth}-bit attempt {attempt} (arms {fired:?}) mismatched ffmpeg"
                    ));
                    continue;
                }
                for a in 0..3 {
                    if fired[a] > 0 {
                        arms_proved[a] += 1;
                        arms_total[a] += 1;
                    }
                }
                matched += 1;
                fired_exact_per_depth[depth_idx] += 1;
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {matched} pixel-exact \
                 attempts carrying an intra 1:4 strip, per-arm attempts \
                 16x4/4x16/chroma-reference={arms_proved:?}, {out_of_scope} attempts carried none \
                 ({oos_mismatch} of them mismatched)"
            );
            if oos_mismatch > 0 {
                failures.push(format!(
                    "{bit_depth}-bit: {oos_mismatch} attempt(s) with no intra 1:4 strip decoded \
                     but mismatched ffmpeg -- another shape's defect, not a pass"
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{NAME}: {} sweep failure(s): {failures:#?}",
            failures.len()
        );
        // The counter must have fired on a pixel-exact attempt at every bit depth
        // the sweep actually decoded at.
        for (depth_idx, bit_depth) in [8u32, 10u32].into_iter().enumerate() {
            assert!(
                decoded_per_depth[depth_idx] == 0 || fired_exact_per_depth[depth_idx] > 0,
                "{NAME}: {bit_depth}-bit decoded {} attempt(s) but none of them fired an intra \
                 1:4 strip on a pixel-exact compare",
                decoded_per_depth[depth_idx]
            );
        }
        // MEASURED (48-run sweep, report table): every recipe that makes
        // aomenc pick an intra PARTITION_VERT_4 strip ALSO contains an intra
        // 8x4/4x8 or 4x4 block inside a sub-8x8 partition, a refusal this lane
        // does not own -- so the 4x16 orientation is blocked on THAT and the
        // gate asserts exactly that. If the sub-8x8 lane lands and this arm
        // still reads 0, or the blocker changes, this fails.
        assert!(
            arms_total[1] > 0 || sub8_intra_refusals > 0,
            "{NAME}: the 4x16 (PARTITION_VERT_4) arm read 0 \
             (16x4/4x16/chroma-reference={arms_total:?}) and NO attempt refused on the sub-8x8 \
             intra block -- its documented blocker is gone, so a zero arm is now unexplained"
        );
        for (a, what) in [
            (0usize, "intra 16x4 strips (PARTITION_HORZ_4)"),
            (
                2,
                "chroma-reference (odd) intra strips carrying the pair's 8x4/4x8 chroma",
            ),
        ] {
            assert!(
                arms_total[a] > 0,
                "{NAME}: arm {a} ({what}) never fired on a pixel-exact attempt \
                 (16x4/4x16/chroma-reference={arms_total:?}) -- unproven"
            );
        }
    }

    #[test]
    // lane-sbab r1: the SUPERBLOCK-level (64x64) AB partitions on an INTER
    // frame -- PARTITION_HORZ_A/_HORZ_B/_VERT_A/_VERT_B at 64x64, i.e. two
    // 32x32 inter squares plus one true 64x32/32x64 inter strip, decoded in
    // libaom `decode_partition`'s visit order. Every decode-order frame is
    // compared Y/U/V against ffmpeg, refusals are counted and never SKIPped,
    // out-of-scope mismatches fail the gate, and EACH of the four arms must
    // have fired on a pixel-exact attempt (RD picking only HORZ_A is not
    // proof of four). `--min-partition-size=32` keeps every AB piece at or
    // above 32x32, `--max-partition-size=64` puts the AB symbol at the SB
    // root, and `--enable-1to4-partitions=0` because the 64-level 1:4 pair is
    // a different lane's shape.
    fn a_real_aomenc_inter_sequence_with_superblock_level_ab_partitions_decodes_pixel_exact() {
        const NAME: &str =
            "a_real_aomenc_inter_sequence_with_superblock_level_ab_partitions_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (256usize, 192usize, 6usize);
        // Per arm, over both bit depths: how many pixel-exact attempts fired
        // it at least once.
        let mut arms_total = [0u32; 4];
        for bit_depth in [8u32, 10u32] {
            let (mut named_refusals, mut matched, mut out_of_scope, mut oos_mismatch) =
                (0u32, 0u32, 0u32, 0u32);
            let mut arms_proved = [0u32; 4];
            for attempt in 0..20u32 {
                // Axes measured live by the r1 sweep
                // (~/.cache/sbab-tmp/sweep.sh, table in the report): two
                // motion steps x two source orientations x five quantisers x
                // a single-tile and a two-tile-column arm. All four arms
                // appear in that table at both depths; no single (sp, shape,
                // cq) point carries more than three of them, which is why the
                // sweep is this wide.
                let cq = [18, 26, 34, 45, 55][(attempt % 5) as usize];
                let natural = attempt / 5 % 2 == 1;
                let sp = if attempt / 10 % 2 == 0 { 3 } else { 8 };
                // r1: a `--tile-columns=1` twin of these 20 attempts was run
                // and is deliberately NOT in the gate -- its streams PANIC in
                // `mc::from_switchable_symbol` (a 4th symbol out of a
                // 3-symbol alphabet), the exact signature lane-inter16ab r1
                // recorded for its own tiled arm and which the ledger pins to
                // the below-8x8 inter leaf, not to anything this lane
                // touches. Recorded as open residue in
                // lanes/sbab-r1.report.md rather than turned into a SKIP or a
                // weakened assert.
                let tile_cols = "0";
                let duration = frame_count as f64 / 25.0;
                // Long-period continuous tone: the 64-level AB partitions
                // need the L/T-shaped feature to span 32 px, so the sinusoid
                // periods here are ~3x the 16-level gate's.
                let lum = if natural {
                    format!("128+58*sin((Y+N*{sp})/17)+40*sin(X/29)+20*sin((X+Y)/13)")
                } else {
                    format!("128+58*sin((X+N*{sp})/17)+40*sin(Y/29)+20*sin((X-Y)/13)")
                };
                let source = format!(
                    "color=c=gray:s={width}x{height}:d={duration}:r=25,format=gray,\
                     geq=lum='{lum}',format=yuv420p"
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
                let tile_arg = format!("--tile-columns={tile_cols}");
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
                    "--kf-max-dist=9999",
                    &tile_arg,
                    // Per-arm overrides go LAST: aomenc keeps the last
                    // occurrence of a repeated --enable-* flag.
                    "--enable-rect-partitions=1",
                    "--enable-ab-partitions=1",
                    "--enable-1to4-partitions=0",
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
                let before = decode::sb_ab_inter_hits_by_arm();
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
                let after = decode::sb_ab_inter_hits_by_arm();
                let fired: Vec<usize> = (0..4).map(|a| after[a] - before[a]).collect();
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
                if fired.iter().sum::<usize>() == 0 {
                    out_of_scope += 1;
                    if mismatched {
                        oos_mismatch += 1;
                        eprintln!(
                            "{NAME}: {bit_depth}-bit attempt {attempt} MISMATCHES with zero \
                             64-level inter AB partition -- another shape's defect"
                        );
                    }
                    continue;
                }
                if mismatched {
                    for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                        let bad: Vec<usize> = (0..got.y.len())
                            .filter(|&k| got.y[k] != want.y[k])
                            .collect();
                        if !bad.is_empty() {
                            eprintln!(
                                "{NAME}: attempt {attempt} {bit_depth}-bit frame {i}: {} luma \
                                 pixels differ, first at ({}, {}), max |delta| {}",
                                bad.len(),
                                bad[0] % width,
                                bad[0] / width,
                                bad.iter()
                                    .map(|&k| (got.y[k] as i32 - want.y[k] as i32).abs())
                                    .max()
                                    .unwrap_or(0)
                            );
                        }
                    }
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y, want.y,
                        "{NAME} frame {i} luma vs ffmpeg (attempt {attempt}, {bit_depth}-bit, arms {fired:?})"
                    );
                    assert_eq!(
                        got.u, want.u,
                        "{NAME} frame {i} U vs ffmpeg (attempt {attempt}, {bit_depth}-bit, arms {fired:?})"
                    );
                    assert_eq!(
                        got.v, want.v,
                        "{NAME} frame {i} V vs ffmpeg (attempt {attempt}, {bit_depth}-bit, arms {fired:?})"
                    );
                }
                for a in 0..4 {
                    if fired[a] > 0 {
                        arms_proved[a] += 1;
                        arms_total[a] += 1;
                    }
                }
                matched += 1;
            }
            eprintln!(
                "{NAME} ({bit_depth}-bit): {named_refusals} named refusals, {matched} pixel-exact \
                 attempts carrying a 64-level inter AB partition, per-arm attempts \
                 HORZ_A/HORZ_B/VERT_A/VERT_B={arms_proved:?}, {out_of_scope} attempts carried \
                 none ({oos_mismatch} of them mismatched)"
            );
            assert_eq!(
                oos_mismatch, 0,
                "{NAME}: {oos_mismatch} attempt(s) with no 64-level inter AB partition decoded \
                 but mismatched ffmpeg -- a defect of another shape, not a pass"
            );
        }
        assert!(
            arms_total.iter().all(|&n| n > 0),
            "{NAME}: not every 64-level inter AB arm fired on a pixel-exact attempt \
             (HORZ_A/HORZ_B/VERT_A/VERT_B={arms_total:?}) -- the unfired arm(s) are unproven"
        );
    }

    /// The inter-frame twin of
    /// [`a_real_aomenc_intra_stream_with_deblocking_decodes_pixel_exact`]:
    /// deblocking left *on* at a crf high enough that libaom picks a nonzero
    /// level, CDEF off, so a pixel mismatch can only come from the loop
    /// filter -- and over a real 4-frame INTER sequence, not one key frame.
    ///
    /// lane-av1skipfix: the attempt-loop shape. The sweep walks cq and seed,
    /// REQUIRES at least one attempt to decode, and hard-asserts
    /// `decode::deblock_hits` on a pixel-compared attempt; a decode error on
    /// every attempt is a hard failure carrying the refusal texts, never a
    /// printed SKIP (class `gate-skips-on-its-own-failure`). MEASURED
    /// 2026-09-29 on `a21f3680`: cq 55/45/35/63 all decode 4 frames
    /// pixel-exact with 208/224/160/208 deblock edges.
    #[test]
    fn a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 4usize);
        // Two seeds x four quantisers.
        const CQS: [u32; 4] = [55, 45, 35, 63];
        let mut refusals = Vec::new();
        let mut counted_exact = 0u32;
        let mut uncounted_exact = 0u32;
        'attempts: for round in 0..2u32 {
            for cq in CQS {
                let source = gradients_source(42 + round, width, height, "duration=0.16:rate=25");
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
                        "--lag-in-frames=0",
                        "--auto-alt-ref=0",
                        "--kf-max-dist=1000",
                        // Forces `primary_ref_frame = PRIMARY_REF_NONE` on every
                        // frame (spec 5.9.2): without it a real encoder's inter
                        // frame loads its initial CDF state from the previous
                        // frame's *adapted* tables (spec 7.20's `load_cdfs`), which
                        // this decoder never does -- it always starts each frame's
                        // `Cdfs` from the spec defaults (`decode.rs`'s
                        // `Cdfs::new(q_ctx)`, called fresh per frame in
                        // `decode_stream`). Without this flag the inter frame's
                        // very first symbol desyncs immediately (traced 2026-08-27:
                        // no coefficient trace at all before the tile's first
                        // partition read comes back an out-of-alphabet value). A
                        // real decoder needs cross-frame CDF forwarding to handle
                        // that stream; this fixture sidesteps it the same way a
                        // real broadcast/low-latency encode legitimately can.
                        "--error-resilient=1",
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
                if let Ok(dump) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(dump, &stream).expect("dump stream");
                }
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
                let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
                assert_eq!(frames.len(), frame_count);
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    // gradients' colors ignore its seed, so this gate encodes fresh
                    // content every run -- a mismatch here may never reproduce.
                    // Self-pin the stream before panicking so the failure is
                    // bisectable (replay: EC_AV1_PIN on scratch_isolate_pinned_mismatch).
                    let ok = got.y == want.y && got.u == want.u && got.v == want.v;
                    if !ok {
                        let pin = std::env::temp_dir().join(format!(
                            "ec-av1-deblocking-gate-fail-s{}-cq{cq}.obu",
                            42 + round
                        ));
                        crate::dumpio::pin("TEST_PIN", &pin, &stream);
                        panic!(
                            "frame {i} mismatch vs ffmpeg (seed {} cq {cq}, deblock fired: \
                             {counted}) -- stream pinned at {}",
                            42 + round,
                            pin.display()
                        );
                    }
                }
                if counted_exact >= 2 {
                    break 'attempts;
                }
            }
        }
        gate_buckets(NAME, counted_exact, uncounted_exact, refusals.len());
        assert!(
            counted_exact > 0,
            "{NAME}: none of {} attempts decoded a sequence with a deblocking edge -- \
             apply_deblock is unproven (class gate-blind-to-feature):\n{}",
            CQS.len() * 2,
            refusals.join("\n")
        );
    }

    /// The decisive cross-frame CDF forwarding test: identical to
    /// [`a_real_aomenc_inter_sequence_with_deblocking_decodes_pixel_exact`]
    /// except it drops `--error-resilient=1`, so aomenc codes every inter
    /// frame's `primary_ref_frame` at its default (not `PRIMARY_REF_NONE`)
    /// and expects the decoder to resume from the previous frame's *adapted*
    /// CDF state (spec 7.20 `load_cdfs`). Without cross-frame forwarding,
    /// frame 1's very first tile symbol desyncs (an out-of-alphabet
    /// partition read); with it, every frame decodes pixel-exact.
    #[test]
    fn a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 4usize);
        // The fixture's content varies run to run (lavfi gradients ignores its
        // seed option for the c0..c7 colors -- they default to "random"), so:
        // most encodes of this recipe trip one of the decoder's NAMED round-2
        // refusals (a reference other than LAST_FRAME, a sub-16 partition --
        // genuine coverage gaps, separately documented), and only some produce
        // a stream fully inside this decoder's declared support. So: attempt
        // several seeds, and the first stream that DECODES must be pixel-exact
        // -- that assertion is what guards the forwarded-CDF state (a wrong
        // table decodes plausible-but-wrong pixels; the counter-reset bug this
        // gate caught did exactly that). Exhausting every attempt on named
        // refusals skips loudly, listing them.
        let mut refusals = Vec::new();
        for attempt in 0..8u32 {
            let seed = 42 + attempt;
            let source = gradients_source(seed, 64, 64, "duration=0.16:rate=25");
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
            // lane-cdfflake: hard length check on the y4m fixture itself --
            // a truncated/racing ffmpeg write would otherwise surface as a
            // downstream pixel mismatch indistinguishable from a real decode
            // defect. y4m frames are `FRAME\n` (6 bytes) + raw 4:2:0 payload;
            // find the header/frame-data boundary at the first `FRAME` marker
            // rather than parsing the header fields (duration/rate can move
            // the header text length without moving the frame math).
            let marker_at = y4m
                .stdout
                .windows(5)
                .position(|w| w == b"FRAME")
                .expect("y4m stream missing FRAME marker");
            let frame_420_bytes = width * height * 3 / 2;
            assert_eq!(
                y4m.stdout.len(),
                marker_at + frame_count * (6 + frame_420_bytes),
                "y4m fixture length mismatch (seed {seed})"
            );
            let y4m_hash = fnv1a64(&y4m.stdout);
            let out = run_with_stdin(
                Command::new(aomenc_path()).args([
                    "--codec=av1",
                    "--passes=1",
                    "--end-usage=q",
                    "--cq-level=55",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--kf-max-dist=1000",
                    // `--threads=1` (not the default): a multithreaded RD search
                    // makes aomenc's screen-content-tools heuristic race (seen
                    // flaky under `cargo test`'s own parallelism -- the same
                    // fixture, same seed, occasionally sets
                    // `allow_screen_content_tools`, a round-2 gap unrelated to
                    // CDF forwarding), so pin it to one thread for a
                    // deterministic header.
                    "--threads=1",
                    "--row-mt=0",
                    // `--enable-order-hint=0` kills BWDREF/ALTREF entirely (no
                    // order hints -> no bidirectional distance weighting) and,
                    // combined with `--auto-alt-ref=0`/`--lag-in-frames=0`
                    // above, leaves aomenc's RD search only LAST_FRAME/
                    // LAST2_FRAME/LAST3_FRAME/GOLDEN_FRAME to pick from --
                    // GOLDEN itself needs no order hint (it is the keyframe's
                    // fixed slot, not a distance-weighted reference), so this
                    // does not suppress GOLDEN selection the way the sibling
                    // CDF-forwarding gate's own comment worried about; it just
                    // narrows the pool this gate needs GOLDEN to win out of.
                    "--enable-order-hint=0",
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
                "aomenc refused the fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stream = out.stdout;
            let stream_hash = fnv1a64(&stream);
            // A named `unsupported` refusal is a documented coverage gap (each
            // one carries its own test debt elsewhere), not a forwarding verdict
            // -- try the next seed. Any stream that DECODES must be pixel-exact,
            // and any non-refusal error is a hard failure.
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "cross-frame CDF forwarding failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            // lane-cdfflake: reconstruct ffmpeg's raw reference bytes (the
            // three per-frame slices are contiguous chunks of the raw
            // `-f rawvideo` output ffmpeg_decode_sequence sliced, so
            // concatenating them back is bit-for-bit the same bytes ffmpeg
            // actually produced -- no second ffmpeg invocation needed).
            let ffmpeg_raw: Vec<u8> = ffmpeg_frames
                .iter()
                .flat_map(|p| p.y.iter().chain(&p.u).chain(&p.v).map(|&v| v as u8))
                .collect();
            let ffmpeg_raw_hash = fnv1a64(&ffmpeg_raw);
            eprintln!(
                "cdfflake attempt seed={seed}: y4m_hash={y4m_hash:016x} \
                 stream_hash={stream_hash:016x} ffmpeg_raw_hash={ffmpeg_raw_hash:016x}"
            );
            // lane-av1golden3: pin the exact raw stream on the first mismatch
            // so a flaky-looking gate failure becomes a deterministic, static
            // fixture to bisect against -- env-gated, no cost on a normal run.
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched {
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    std::fs::write(&path, &stream).expect("writing pinned stream");
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
                // lane-cdfflake: unconditional pin on mismatch -- the aomenc
                // stream AND ffmpeg's raw reference bytes, so a flake caught
                // under load names the liar (stream bytes differ from a
                // clean rerun -> subprocess instability; reference bytes
                // differ -> ffmpeg instability; both stable -> real decode
                // defect) without relying on the env var being set.
                // The pin goes to `fixtures/`, which outlives the session: an
                // earlier version wrote to a session scratchpad, so once tmpfs
                // reaped it a real mismatch aborted on the write instead of
                // reporting itself. A failed write must not mask the asserts
                // below either, so it warns rather than panicking -- lane-av1pinloud
                // routed it through `pin_reporting` rather than bare `fs::write`,
                // so the warning now carries on-disk-vs-owed byte counts: a
                // truncated pin was as silent here as anywhere else, and this
                // is the one site where dying on the write is wrong.
                let sp = pin_dir();
                let stream_pin = sp.join(format!("cdfflake-stream-seed{seed}.bin"));
                let ref_pin = sp.join(format!("cdfflake-ffmpeg-raw-seed{seed}.bin"));
                for (path, bytes) in [(&stream_pin, &stream), (&ref_pin, &ffmpeg_raw)] {
                    if let Err(line) = crate::dumpio::pin_reporting("TEST_PIN", path, bytes) {
                        eprintln!("cdfflake: {line}");
                    }
                }
                eprintln!(
                    "cdfflake MISMATCH seed={seed}: pinned stream + ffmpeg reference to {}",
                    sp.display()
                );
            }
            for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            return;
        }
        eprintln!(
            "SKIP a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact: every \
             attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-av1golden3: reproduces the pinned mismatch bytes captured by
    /// `EC_AV1_GATE_DUMP=$SP/golden3-pin.obu` off
    /// [`a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`]
    /// (seed 47, frame 1 U). Deterministic and static -- no aomenc/ffmpeg
    /// re-encode involved, only re-decodes fixed bytes on disk. What changed
    /// under lane-av1pinslive is that the gate is LIVE: the bytes are committed
    /// at `crates/ec-av1/fixtures/golden3-pin.obu` instead of a scratchpad that
    /// tmpfs reaps, so it runs in the ordinary suite on any runner that has
    /// ffmpeg (guarded by `have_ffmpeg()`, so `EC_AV1_REQUIRE_FFMPEG=1` turns
    /// its absence into a hard failure rather than a silent skip). It costs
    /// ~0.15 s, touches no process-global, and reads no counter -- none of the
    /// conditions that make this crate ignore its other manual gates.
    ///
    /// The original capture (2026-08-28, `bec27414`) is NOT reproducible: it
    /// predates `5ae053d3` "route all 20 gradients gate fixtures through
    /// seed-derived colours", so `gradients_source` still left c4..c7 to
    /// ffmpeg's `random` default then. This is a FRESH capture from the same
    /// recipe family at seed 42, which IS byte-reproducible now: the command is
    /// in `lanes/av1pins.report.md`, and the stream's FNV-1a-64
    /// (`89c4089148d51c04`; 159 bytes; sha256
    /// `11c9d5331cffa6614c8265ad5fa685d5ce9155db8439949b48ec4a652ace95cf`) is
    /// the `stream_hash` the live
    /// `a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`
    /// prints for that seed -- which is what proves the recipe is that gate's.
    #[test]
    fn pinned_golden3_stream_decodes_pixel_exact() {
        let path = std::env::var("EC_AV1_GATE_DUMP_PIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| crate_pin("golden3-pin.obu"));
        let Ok(stream) = std::fs::read(&path) else {
            eprintln!(
                "SKIP pinned_golden3_stream_decodes_pixel_exact: no pinned bytes at {} \
                 -- the committed copy is crates/ec-av1/fixtures/golden3-pin.obu",
                path.display()
            );
            return;
        };
        if !have_ffmpeg() {
            eprintln!("SKIP pinned_golden3_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, 4);
        assert_eq!(
            frames.len(),
            ffmpeg_frames.len(),
            "pinned stream decoded {} frames, ffmpeg {} -- a length mismatch would \
             make the zip below compare a mis-paired set",
            frames.len(),
            ffmpeg_frames.len()
        );
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (pinned)");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (pinned)");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (pinned)");
        }
        eprintln!(
            "pinned_golden3_stream_decodes_pixel_exact: {} frame(s) byte-exact vs ffmpeg \
             from {}",
            frames.len(),
            path.display()
        );
    }

    /// lane-av1golden4: reproduces the pinned mismatch bytes captured by
    /// `EC_AV1_GATE_DUMP=$SP/golden4-pin.obu` off
    /// [`a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`]
    /// (seed 43, frame 3 luma) the round the GOLDEN mask lifted. Deterministic
    /// and static.
    ///
    /// LIVE since lane-av1pinslive r3: the bytes are committed at
    /// `crates/ec-av1/fixtures/golden4-pin.obu` (137 bytes; sha256
    /// `1754023e44e46edaebc2c05065bf1c65db1a41354577cbf71c0e8ebe737ae14a`,
    /// recovered byte-for-byte from the runner library), so the old
    /// "reads a pinned fixture path outside the repo" reason is false. Costs
    /// ~0.10 s, touches no process-global, reads `non_last_ref_hits` for
    /// diagnostic output only (never asserted), and calls no aomenc.
    #[test]
    fn pinned_golden4_stream_decodes_pixel_exact() {
        use crate::decode::non_last_ref_hits;
        let path = std::env::var("EC_AV1_GATE_DUMP_PIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| crate_pin("golden4-pin.obu"));
        let stream = require_pin(&path, "golden4-pin.obu");
        if !have_ffmpeg() {
            eprintln!("SKIP pinned_golden4_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let before = non_last_ref_hits();
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        eprintln!(
            "non_last_ref_hits before={before} after={}",
            non_last_ref_hits()
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, 4);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (pinned)");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (pinned)");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (pinned)");
        }
    }

    /// lane-av1golden7 round 9: pins the residual GOLDEN_FRAME MC bug found
    /// by the forwarding gate once film grain is out of the way (1/20 real
    /// aomenc streams, frame 3 luma, apply_grain=false, `non_last_ref_hits`
    /// delta=2). `GOLDEN_FRAME` refuses by name until this decodes exact.
    ///
    /// LIVE since lane-av1pinslive r3. The old shape was the worst of the
    /// class: a `concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/...")`
    /// literal, which points at the GITIGNORED root and can therefore never be
    /// satisfied by a committed pin, combined with a bare `.expect()`. The
    /// bytes are now committed at
    /// `crates/ec-av1/fixtures/golden7-forwarding-mismatch.obu` (152 bytes;
    /// sha256 `81b3bf657a85e95085287b30d97dee93ba5aea0ed1db1e1f4fcd19a06afc17be`,
    /// recovered byte-for-byte from the runner library).
    #[test]
    fn pinned_golden7_stream_decodes_pixel_exact() {
        use crate::decode::non_last_ref_hits;
        let path = std::env::var("EC_AV1_GATE_DUMP_PIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| crate_pin("golden7-forwarding-mismatch.obu"));
        let stream = require_pin(&path, "golden7-forwarding-mismatch.obu");
        if !have_ffmpeg() {
            eprintln!("SKIP pinned_golden7_stream_decodes_pixel_exact: no ffmpeg");
            return;
        }
        let before = non_last_ref_hits();
        let frames = decode_stream(&stream).expect("pinned stream must decode");
        eprintln!(
            "non_last_ref_hits before={before} after={}",
            non_last_ref_hits()
        );
        let ffmpeg_frames = ffmpeg_decode_sequence(&stream, 64, 64, 4);
        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
            assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (pinned)");
            assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (pinned)");
            assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (pinned)");
        }
    }

    /// lane-av1refs's decisive single-non-LAST-reference gate: identical
    /// recipe to
    /// [`a_real_aomenc_inter_sequence_with_cdf_forwarding_decodes_pixel_exact`]
    /// except `--enable-order-hint=0` is DROPPED (that flag's own comment on
    /// the sibling test names it as exactly what suppresses aomenc's RD from
    /// ever picking `GOLDEN_FRAME`) and the sequence runs 8 frames, not 4 --
    /// more chances for the RD search to reach past the first GOP's initial
    /// key frame. `--auto-alt-ref=0` stays: this decoder only supports
    /// `LAST_FRAME`/`GOLDEN_FRAME` so far, and alt-ref/backward references
    /// are still a named refusal, not this gate's target. A stream that
    /// decodes without firing [`decode::non_last_ref_hits`] is not a
    /// coverage win for this round -- SKIP it as a genuine, named miss
    /// rather than pass silently on untested ground.
    #[test]
    fn a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact() {
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact: no ffmpeg"
            );
            return;
        }
        if !have_aomenc() {
            eprintln!(
                "SKIP a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact: no aomenc at {}",
                aomenc_path().display()
            );
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 8usize);
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        // aomenc's own RD search is nondeterministic run-to-run even at
        // `--threads=1` (confirmed live: identical CLI args, identical
        // input, two different output byte streams) -- so beyond varying
        // the fixture's seed, each attempt is its own independent coin
        // flip against every named refusal below. 120 attempts (not 40)
        // to give that coin flip enough tries to land on a stream this
        // decoder both accepts and that fires GOLDEN_FRAME. `duration=0.32`
        // at `rate=25` (not `0.24`) matches every sibling gate's own
        // frame-count fixture exactly -- a shorter duration here once
        // produced a frame-0 (keyframe, no reference at all) luma mismatch
        // against ffmpeg, i.e. a source-frame-count rounding artifact of
        // this recipe's own choosing, not a decoder bug.
        for attempt in 0..120u32 {
            let seed = 42 + attempt % 40;
            let source = gradients_source(seed, 64, 64, "duration=0.32:rate=25");
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
                    "--cq-level=55",
                    "--cpu-used=0",
                    "--lag-in-frames=0",
                    "--auto-alt-ref=0",
                    "--kf-max-dist=1000",
                    "--threads=1",
                    "--row-mt=0",
                    // Narrows the RD search's reference alphabet toward
                    // LAST_FRAME/GOLDEN_FRAME -- without this, aomenc's RD is
                    // just as likely to pick LAST2/LAST3/BWDREF/ALTREF2/
                    // ALTREF (this decoder's already-documented, still-open
                    // refusals), starving this gate of the one reference it
                    // targets.
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
                    // `min == max == 32` forces PARTITION_NONE for every
                    // 32x32 block, not just biases toward it -- with
                    // `--min-partition-size=16` alone aomenc's exhaustive
                    // `cpu-used=0` RD search still occasionally read HORZ/
                    // VERT/SPLIT partitions below 32x32 despite
                    // `--enable-rect-partitions=0`/`--enable-ab-partitions=0`/
                    // `--enable-1to4-partitions=0` all being off (two
                    // separate, already-documented round-2 gaps: "a
                    // partition below 16x16", "a partition type this encoder
                    // never writes"), starving this gate's already-narrow
                    // seed pool. Locking both bounds to 32 removes any
                    // partition-depth choice from the search entirely, so
                    // this gate can isolate GOLDEN_FRAME selection alone.
                    "--max-partition-size=32",
                    "--min-partition-size=32",
                    "--enable-palette=0",
                    "--enable-intrabc=0",
                    "--enable-cfl-intra=0",
                    // use_ref_frame_mvs (temporal MV projection) is unimplemented in mvstack;
                    // leaving it on silently desyncs symbols on inter frames (grainfix lesson).
                    "--enable-ref-frame-mvs=0",
                    // `--tune-content=film` was dropped: on this box it
                    // makes aomenc emit `apply_grain` almost every attempt
                    // (lane-av1golden8 round 10 measurement, live: 120/120
                    // real attempts hit a named refusal, 0 ever fired
                    // GOLDEN_FRAME -- mostly `apply_grain`, dominated by its
                    // own recipe flag) -- film grain synthesis has its own
                    // dedicated refusal test and must not starve this gate
                    // of the GOLDEN_FRAME draws it exists to prove. Default
                    // content tuning still occasionally flips
                    // `allow_screen_content_tools` on for this synthetic
                    // gradient content (a separate, already-documented open
                    // refusal), which the 120-attempt coin flip below
                    // tolerates the same way.
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
            let before = decode::non_last_ref_hits();
            let frames = match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        msg.contains("unsupported"),
                        "a golden-reference stream failed outright (seed {seed}): {msg}"
                    );
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => frames,
            };
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            // This seed's RD search may never have picked GOLDEN_FRAME -- a
            // genuine miss, not a refusal, and not a reason to skip the pixels.
            let counted = decode::non_last_ref_hits() != before;
            if !counted {
                never_fired += 1;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            // Same pin-on-mismatch pattern as the CDF forwarding gate above:
            // env-gated, no cost on a normal run.
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
                assert_eq!(got.y, want.y, "frame {i} luma vs ffmpeg (seed {seed})");
                assert_eq!(got.u, want.u, "frame {i} U vs ffmpeg (seed {seed})");
                assert_eq!(got.v, want.v, "frame {i} V vs ffmpeg (seed {seed})");
            }
            if counted {
                eprintln!(
                    "FIRING seed {seed}: non_last_ref_hits advanced by {}",
                    decode::non_last_ref_hits() - before
                );
                gate_buckets(
                    "a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact",
                    1,
                    never_fired,
                    refusals.len(),
                );
                return;
            }
        }
        gate_buckets(
            "a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact",
            0,
            never_fired,
            refusals.len(),
        );
        eprintln!(
            "SKIP a_real_aomenc_inter_sequence_with_a_golden_reference_decodes_pixel_exact: \
             {never_fired} attempts decoded but never fired GOLDEN_FRAME, and every other \
             attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-av1comp: proves `reference_select`/`comp_mode` are actually read
    /// off a real bitstream rather than only unit-tested in isolation --
    /// `--auto-alt-ref=1 --lag-in-frames=16` gives aomenc a backward
    /// reference to pick `COMPOUND_REFERENCE` with, `--enable-order-hint=1`
    /// is required for it to ever consider compound at all. Three
    /// acceptable outcomes: (a) a stream where `reference_select` never
    /// actually drove a block to `COMPOUND_REFERENCE` decodes bit-exact end
    /// to end (proving the per-block `comp_mode` read did not desync the
    /// single-ref blocks around it); (b) a stream that does fire compound
    /// is refused by name with [`decode::comp_mode_hits`] having advanced
    /// (proving `comp_mode`/`read_compound_ref_frames` consumed the right
    /// symbols before refusing, rather than desyncing and refusing for an
    /// unrelated reason) -- masked/wedge compound (`comp_group_idx == 1`)
    /// and a partition below 16x16 (a real, separately tracked capability
    /// gap) are the two refusal arms this can still legitimately name; (c)
    /// r13 wired plain `COMPOUND_AVERAGE` MC for real, so as of r15 a
    /// stream that fires compound may also decode fully -- accepted only
    /// when every output frame is pixel-exact against ffmpeg.
    #[test]
    fn a_real_aomenc_stream_with_reference_select_reads_comp_mode_correctly() {
        const NAME: &str = "a_real_aomenc_stream_with_reference_select_reads_comp_mode_correctly";
        let _gate_lock = lock_gate_counters();
        if !have_ffmpeg() {
            eprintln!("SKIP {NAME}: no ffmpeg");
            return;
        }
        if !have_aomenc() {
            eprintln!("SKIP {NAME}: no aomenc at {}", aomenc_path().display());
            return;
        }
        let (width, height, frame_count) = (64usize, 64usize, 20usize);
        let mut refusals = Vec::new();
        let mut never_fired = 0u32;
        for attempt in 0..60u32 {
            let seed = 42 + attempt % 40;
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
                "--enable-order-hint=1",
                "--enable-warped-motion=0",
                "--enable-obmc=0",
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
            let before = decode::comp_mode_hits();
            match decode_stream(&stream) {
                Err(e) => {
                    let msg = e.to_string();
                    let fired = decode::comp_mode_hits() > before;
                    assert!(
                        msg.contains("unsupported"),
                        "{NAME} failed outright (seed {seed}): {msg}"
                    );
                    if fired {
                        // `comp_mode` firing does not stop the block walk --
                        // a later, unrelated block in the same tile can
                        // still hit any other named refusal this decoder
                        // carries. A partition below 16x16 is a real, open
                        // capability gap tracked on its own (not a
                        // COMPOUND_REFERENCE defect); accept it here too
                        // rather than fail a compound-specific gate on a
                        // partition-search coincidence. lane-av1blend r7:
                        // re-swept the wide gate with the plain-average
                        // blend unmasked (r6/r7's ref_ctx fix alone) and it
                        // still mismatches on real streams beyond that one
                        // bug -- re-masked comp_group_idx == 0 too, so this
                        // stays a legal named refusal.
                        assert!(
                            msg.contains("COMPOUND_REFERENCE") || msg.contains("a partition"),
                            "{NAME}: comp_mode fired (seed {seed}) but the refusal \
                             was for something else: {msg}"
                        );
                        let advanced = decode::comp_mode_hits() - before;
                        eprintln!(
                            "{NAME} FIRING seed {seed}: comp_mode hits advanced by \
                             {advanced}, refused by name as expected: {msg}"
                        );
                        return;
                    }
                    refusals.push(format!("seed {seed}: {msg}"));
                    continue;
                }
                Ok(frames) => {
                    // class [[gate-skips-on-its-own-failure]]: a decode that
                    // succeeded is ALWAYS pixel-compared below; the hit counter
                    // only decides whether the attempt counts as firing.
                    let counted = decode::comp_mode_hits() != before;
                    if !counted {
                        never_fired += 1;
                    }
                    // Outcome (c): plain COMPOUND_AVERAGE decoded fully --
                    // non-negotiable pixel exactness, no tolerance.
                    let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
                    assert_eq!(frames.len(), frame_count);
                    let mismatched = frames
                        .iter()
                        .zip(&ffmpeg_frames)
                        .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
                    if mismatched {
                        let pin =
                            std::env::temp_dir().join("ec-av1-reference-select-gate-fail.obu");
                        crate::dumpio::pin("TEST_PIN", &pin, &stream);
                        if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                            crate::dumpio::pin("EC_AV1_GATE_DUMP", &path, &stream);
                            eprintln!(
                                "EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}"
                            );
                        }
                        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                            assert_eq!(
                                got.y,
                                want.y,
                                "{NAME} frame {i} luma vs ffmpeg (seed {seed}) -- stream pinned at {}",
                                pin.display()
                            );
                            assert_eq!(
                                got.u,
                                want.u,
                                "{NAME} frame {i} U vs ffmpeg (seed {seed}) -- stream pinned at {}",
                                pin.display()
                            );
                            assert_eq!(
                                got.v,
                                want.v,
                                "{NAME} frame {i} V vs ffmpeg (seed {seed}) -- stream pinned at {}",
                                pin.display()
                            );
                        }
                    } else {
                        for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                            assert_eq!(
                                got.y, want.y,
                                "{NAME} frame {i} luma vs ffmpeg (seed {seed})"
                            );
                            assert_eq!(got.u, want.u, "{NAME} frame {i} U vs ffmpeg (seed {seed})");
                            assert_eq!(got.v, want.v, "{NAME} frame {i} V vs ffmpeg (seed {seed})");
                        }
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
                    if let Some((arm_total, arm_hidden)) = hidden_arnr_arm(NAME, &y4m.stdout, &args)
                    {
                        eprintln!(
                            "{NAME} HIDDEN-ARM(--arnr-maxframes=0): seed {seed}: {arm_total} \
                             decode-order frames, {arm_hidden} hidden, all pixel-exact vs the oracle"
                        );
                    }
                    if counted {
                        eprintln!(
                            "{NAME} FIRING seed {seed}: comp_mode hits advanced by {}, decoded \
                             fully and pixel-exact",
                            decode::comp_mode_hits() - before
                        );
                        gate_buckets(NAME, 1, never_fired, refusals.len());
                        return;
                    }
                }
            }
        }
        gate_buckets(NAME, 0, never_fired, refusals.len());
        eprintln!(
            "SKIP {NAME}: {never_fired} attempts decoded fully (reference_select never drove a \
             block to COMPOUND_REFERENCE) and every other attempt hit a named refusal that never \
             reached comp_mode:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-av1comp round 14's decisive gate for `skip_mode`: same
    /// pin-on-mismatch pattern as [`a_real_aomenc_single_ref_gate`], proving
    /// [`decode::skip_mode_hits`] actually advanced (a real block picked
    /// `skip_mode`, not just that the frame header bit was set) and that the
    /// resulting forced-compound `NEAREST_NEARESTMV` decode is still
    /// pixel-exact against ffmpeg. `--auto-alt-ref=1 --lag-in-frames=16
    /// --enable-fwd-kf=0` (backward references) plus
    /// `--enable-ref-frame-mvs=0` (unimplemented temporal MV projection,
    /// same as every other gate here) is aomenc's own requirement for
    /// `skip_mode_present` to ever be considered at all (spec 5.9.22 needs
    /// both a forward and a backward reference candidate).
    #[test]
    fn a_real_aomenc_stream_with_compound_references_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_compound_references_decodes_pixel_exact";
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
        for attempt in 0..80u32 {
            let seed = 42 + attempt % 40;
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
                "--enable-warped-motion=0",
                "--enable-obmc=0",
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
            let before = decode::skip_mode_hits();
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
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            let counted = decode::skip_mode_hits() != before;
            if !counted {
                never_fired += 1;
            }
            let ffmpeg_frames = ffmpeg_decode_sequence(&stream, width, height, frame_count);
            assert_eq!(frames.len(), frame_count);
            let mismatched = frames
                .iter()
                .zip(&ffmpeg_frames)
                .any(|(got, want)| got.y != want.y || got.u != want.u || got.v != want.v);
            if mismatched {
                let pin = std::env::temp_dir().join("ec-av1-compound-refs-gate-fail.obu");
                crate::dumpio::pin("TEST_PIN", &pin, &stream);
                if let Ok(path) = std::env::var("EC_AV1_GATE_DUMP") {
                    crate::dumpio::pin("EC_AV1_GATE_DUMP", &path, &stream);
                    eprintln!("EC_AV1_GATE_DUMP: wrote mismatching stream (seed {seed}) to {path}");
                }
                for (i, (got, want)) in frames.iter().zip(&ffmpeg_frames).enumerate() {
                    assert_eq!(
                        got.y,
                        want.y,
                        "{NAME} frame {i} luma vs ffmpeg (seed {seed}) -- stream pinned at {}",
                        pin.display()
                    );
                    assert_eq!(
                        got.u,
                        want.u,
                        "{NAME} frame {i} U vs ffmpeg (seed {seed}) -- stream pinned at {}",
                        pin.display()
                    );
                    assert_eq!(
                        got.v,
                        want.v,
                        "{NAME} frame {i} V vs ffmpeg (seed {seed}) -- stream pinned at {}",
                        pin.display()
                    );
                }
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
                    "{NAME} FIRING seed {seed}: skip_mode hits advanced by {}",
                    decode::skip_mode_hits() - before
                );
                gate_buckets(NAME, 1, never_fired, refusals.len());
                return;
            }
        }
        gate_buckets(NAME, 0, never_fired, refusals.len());
        eprintln!(
            "SKIP {NAME}: {never_fired} attempts decoded but never fired skip_mode, and every \
             other attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }

    /// lane-motionmode round 1: `--enable-obmc=1 --enable-warped-motion=0`
    /// on the base recipe (`--auto-alt-ref=1 --lag-in-frames=16
    /// --enable-fwd-kf=0 --enable-ref-frame-mvs=0`) -- pixel-exact against
    /// ffmpeg, gated on [`decode::obmc_hits`] actually advancing (a real
    /// block picked `OBMC_CAUSAL`, not just that the header bit was set).
    /// Structure copied from
    /// `a_real_aomenc_stream_with_compound_references_decodes_pixel_exact`:
    /// a seed sweep with a named-refusal escape hatch and an
    /// `EC_AV1_GATE_DUMP` pin on mismatch.
    #[test]
    fn a_real_aomenc_stream_with_obmc_decodes_pixel_exact() {
        const NAME: &str = "a_real_aomenc_stream_with_obmc_decodes_pixel_exact";
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
        for attempt in 0..80u32 {
            let seed = 42 + attempt % 40;
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
            let before = decode::obmc_hits();
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
            // class [[gate-skips-on-its-own-failure]]: a decode that succeeded
            // is ALWAYS pixel-compared below; the hit counter only decides whether
            // the attempt counts toward the firing total.
            let counted = decode::obmc_hits() != before;
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
                    "{NAME} FIRING seed {seed}: obmc hits advanced by {}",
                    decode::obmc_hits() - before
                );
                gate_buckets(NAME, 1, never_fired, refusals.len());
                return;
            }
        }
        gate_buckets(NAME, 0, never_fired, refusals.len());
        eprintln!(
            "SKIP {NAME}: {never_fired} attempts decoded but never fired obmc, and every \
             other attempt hit a named refusal:\n{}",
            refusals.join("\n")
        );
    }
