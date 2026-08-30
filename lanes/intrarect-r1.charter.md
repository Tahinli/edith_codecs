# lane-intrarect r1 charter — rectangular INTRA prediction (predictor + edges only)

Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-intrarect, branch lane-intrarect @ daa0e4b.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intrarect CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND. Never push; never touch other worktrees (lane-gm is live in a sibling dir); fixtures/ is a symlink.
BUDGET ~55 calls: WIP COMMIT after every green milestone; commit + report by call 45.
libaom oracle source: ~/.cache/aom-oracle/src (NOT /tmp).

## Why this lane
The free-partition and AB gates refuse 17/40 attempts on INTRA-frame rect/AB
partitions (`unsupported: AV1 tile (a partition type this encoder never writes
(value=1|2|4|5|6|7))` raised from the KEY-frame dispatch, decode.rs ~3624/~3841).
The inter side already decodes those partitions (HORZ/VERT/HORZ_A/VERT_A/VERT_B);
the intra side cannot, because the predictor itself is square-only.

## The blocker, precisely
`crates/ec-av1/src/intra.rs::predict` takes ONE `side: usize` and asserts
`dst.len() == side * side`; `Edges::build` extends each edge to `side * 2`.
A 32x16 intra block needs `bw != bh` everywhere: DC's average over
`bw + bh` samples, the directional walk's separate x/y extents, SMOOTH's two
different weight tables, PAETH, and the edge extension to `bw + bh` (not
`2*side`).

## Scope — THE PREDICTOR ONLY. Do not touch the tile dispatch this round.
1. Widen `Edges::build` and `predict` to `(bw, bh)`. Every existing caller
   (21 call sites across the crate) passes `(side, side)` — zero behavior
   change there, and that is the main thing the gates below prove.
2. Port each mode's rect behaviour from ~/.cache/aom-oracle/src:
   - `aom_dsp/intrapred.c`: dc_predictor / dc_left / dc_top / dc_128 variants
     (`dc_predictor_rect` has the `bw + bh` sum with its expN/multiplier
     rounding -- transcribe the exact table use, do not re-derive it),
     `smooth_predictor` (`sm_weight_arrays` indexed separately by bw and bh),
     `paeth_predictor`, `v_/h_predictor`.
   - `av1/common/reconintra.c`: `av1_dr_prediction_z1/z2/z3` (the three
     directional zones take `bw`/`bh` separately, `dx`/`dy` from
     `av1_get_dr_intra_derivative`), `av1_filter_intra_edge`,
     `av1_upsample_intra_edge`, and `get_filt_type`/`intra_edge_filter_strength`
     which are indexed by `bw + bh` -- the blkWh term is where a square-only
     port silently diverges first (class decision-at-wrong-granularity).
3. Unit tests: for a handful of (mode, angle_delta, bw, bh) combinations with
   bw != bh, assert the produced block against values you TRANSCRIBE from the
   C code by hand-running it in the test comment (show the arithmetic), OR
   better: build a tiny C harness under lanes/ that links the oracle's
   intrapred and dumps reference blocks, and check a checksum per case --
   that is what lane-wedge did for its codebook (lanes/wedge_dump.c is the
   pattern to copy; the independent-oracle proof is what makes it landable).

## Gate ladder
(a) full lib suite (`-- --skip a_real_aomenc_stream_with_interintra_wedge`) --
    every square path must be BIT-IDENTICAL; this is the real proof of item 1;
(b) 14-pin default list;
(c) the intra gates by name (filter_intra, directional_chroma, tx_select,
    intra_with_deblocking) -- run them, they exercise the predictor hardest.
NOTE: the aomenc oracle now lives at ~/.cache/aom-oracle/build/aomenc; export
`EC_AV1_REQUIRE_AOMENC=1` on gate runs so a missing oracle FAILS instead of
silently skipping.

## Done criteria
1. `predict`/`Edges` are rect-capable and C-verified for bw != bh; all square
   behaviour bit-identical (suite green); the tile dispatch still refuses
   (next lane flips it).
2. WIP commits; REPORT lanes/intrarect-r1.report.md, verdict FIRST line,
   naming exactly which modes are C-verified vs transcribed-only.
