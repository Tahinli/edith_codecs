# lane-screen r1 charter — consume screen-content-tools syntax (palette + intrabc)

Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-screen, branch lane-screen @ 00de8d3.
Build/test ONLY: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-screen CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture` (full lib ~70s, run it whole; EC_AV1_REQUIRE_AOMENC=1 on gate runs).
Never push, never merge, never touch sibling worktrees (lane-gm and lane-intradisp are live).
libaom oracle: ~/.cache/aom-oracle/src (v3.13.3); binaries in ~/.cache/aom-oracle/build/ (aomdec is instrumented: EC_TRACE=1 prints EC_PART with msac range; EC_AV1_PREFILT_DUMP=<prefix> dumps pre-filter recon).

## Why: this is the widest cheap win left
`stream.rs:169` refuses ANY frame with `allow_screen_content_tools` set. That is
14/40 attempts in the AB-partition gate, it blocks the 8x8-OBMC recipe search
(the content that fires 8x8 OBMC trips this bit), and it makes several gates
skip. Crucially the encoder in those gates runs `--enable-palette=0
--enable-intrabc=0`: the tools are SIGNALLED as allowed but never USED. So the
decoder does not need palette reconstruction — it needs to CONSUME the symbols
so the arithmetic decoder stays in sync (class: symbol-consumption-gap).

## Scope
1. `read_palette_mode_info` (~/.cache/aom-oracle/src/av1/decoder/decodemv.c:567,
   called at :840 and :1103): read it wherever libaom does, gated by
   `av1_allow_palette` (blockd.h:1498 — `allow_screen_content_tools &&
   block_size_wide <= 64 && block_size_high <= 64 && bsize >= BLOCK_8X8`;
   confirm the exact constants in the header, do not guess).
   - luma branch only when `mode == DC_PRED`; chroma branch only when
     `uv_mode == UV_DC_PRED` and the block has chroma.
   - You need `palette_y_mode_cdf[bsize_ctx][palette_mode_ctx]`,
     `palette_y_size_cdf[bsize_ctx]`, the uv equivalents, plus
     `av1_get_palette_bsize_ctx` and `av1_get_palette_mode_ctx` (blockd.h /
     reconintra) and the palette COLOR reads that follow a nonzero size.
   - If `palette_size != 0` for either plane, refuse BY NAME ("a block that
     actually uses a palette") — reconstruction is out of scope. The point of
     this lane is that a size of zero must decode, not desync.
2. `read_intrabc_info` (decodemv.c:693, called at :811): read `use_intrabc`
   where libaom does (intra frames with `allow_intrabc`); if it is 1, refuse by
   name. If 0, continue — note libaom's `use_intrabc` path also affects which
   later syntax is read, so mirror the structure exactly.
3. Remove the whole-frame refusal at stream.rs:169 once 1-2 land. Leave the
   `delta.q_present || delta.lf_present` refusal below it ALONE.
4. New CDF tables must be wired at all four sites (field, defaults, save/restore,
   and the per-frame counter reset) — a table missing from the counter reset
   adapts at the wrong RATE and desyncs later, which cost an earlier lane a full
   round (class cdf-counter-not-reset).

## Gate ladder (commit at each green step)
(a) full lib suite — every existing stream bit-identical;
(b) 14-pin default list (`pinned_warp_stream_decodes_pixel_exact -- --ignored`);
(c) `a_real_aomenc_stream_with_ab_partitions_decodes_pixel_exact` — report the
    `allow_screen_content_tools` refusal count before and after (that is this
    lane's score; it is ~14/40 today);
(d) `a_real_aomenc_filter_intra_stream_decodes_pixel_exact` currently SKIPS
    because this same refusal eats it — check whether it now runs.
Any pixel mismatch: pin with EC_AV1_GATE_DUMP=/tmp/claude-1000/screen-flake-N.obu,
then localize (our EC_AV1_PREFILT_DUMP vs the oracle aomdec's, then EC_TRACE=1
vs EC_AV1_TRACE range ladder). Never guess-fix.

## Done criteria
Frames that ALLOW screen content but do not USE it decode pixel-exact; actual
palette/intrabc use refuses by name; suite + pins green; report
lanes/screen-r1.report.md with VERDICT first line and the before/after count.
