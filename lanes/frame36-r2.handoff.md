# lane-frame36 r2 HANDOFF (branch `lane-frame36`, tip c9e15d7)

## Root causes this round
1. FIXED (decode.rs:9594) -- `decode_block_rect64`'s `depth != 0` (split) arm returns
   early past the tail `txfm_partition_update_rect` r1 added, so a SPLIT superblock-level
   strip published NO `TXFM_CONTEXT` band. libaom's `set_txfm_ctxs` runs for every block
   with the SPLIT unit's size. Frame 33: the 16x64 strip at mi(192,140) reads `tx_depth=1`
   (TX_16X32), so libaom has `left_txfm=32`; ours kept 64 and the 32x64 intra block at
   mi(192,144) read `tx_size_cat3` row 2 where aomdec reads row 1 (same value, different
   range).
2. FIXED by class sweep, latent (decode.rs:8625/8776/8839) -- `decode_block_rect4` (32-level
   1:4 strips, also the inter frame's 32x8/8x32 intra reader) published the bands on none of
   its three exits. This stream carries no such block once frame 33 is correct (the 48/40
   counters before the fix were phantoms of the desync).

## Current divergence in decode frame 33
NONE. Ladder identical: every superblock ENTRY range of every traced frame equals aomdec's
(ours[i] = aomdec[i+1]; our SB-root print skips the last partial SB row, and our group 0
holds frames 0+1). Pixel state: all 37 shown frames of the 58-OBU prefix are byte-exact vs
ffmpeg, and all 48 frames of the whole 2 s head cut are byte-exact.
Frame 36 differing bytes: 0 (1992768 before r1, 765464 at r1's tip fa20907).

## Instrumentation added
`EC_TRACE_MODE_STEP` now prints the oracle-format
`EC_ISTEP mi_row=.. mi_col=.. name=tx_depth val=.. ctx=.. cat=.. rng=..` in
`decode_intra_rect_in_inter` (decode.rs:7541) -- that line named root cause 1 in one run.
Counter `decode::rect64_split_txfm_publish_hits()`.

## Gate + fixture
`crates/ec-av1/fixtures/hg_arf_witness.obu` (32126 B, sha256
7f3b060da5aa9c537633e760c4af71316237db7692f6ae0d459bd2c2dd867d37, `git add -f`),
gated by `a_10bit_film_hidden_arf_with_split_superblock_strips_decodes_pixel_exact`
(stream.rs): every plane of all 37 shown frames vs ffmpeg + hard assert on the counter
(4 on this stream). Ran alone: `ok. 1 passed`, 94.77 s.
No refusal lifted, so refusal_inventory/gate_coverage are unchanged.

## EXACT NEXT STEP
1. Full suite (never run this round -- the r2 unit was cancelled at the turn cap):
   `systemd-run --user --unit=frame36-suite-$(date +%s) -p MemoryMax=10G --same-dir bash -lc
   'EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-frame36
   nice -n 10 cargo test -p ec-av1 --lib -j3 > $HOME/.cache/frame36-suite.log 2>&1'`.
   Known RED on main beecb64 (not this lane):
   `real_aomenc_1to4_streams_decode_pixel_exact_and_rect_vartx_leaves_fire_before_a_named_refusal`.
   169 tests had passed, 0 failed, when the unit was cancelled.
2. Then the next wall: cut a LONGER segment of the same stream (`ffmpeg -ss <t> -t 2 ... -f obu`)
   and re-run `decode_probe` + `diff16.py`; the 2 s head is now fully exact.

## Artifacts (~/.cache/frame36-tmp)
`n58.obu`, `r.raw`/`o2.raw` (prefix), `full_r.raw`/`full_o2.raw` (48-frame cut),
`r2/omode5.txt` (ours ladder, post-fix), `r2/amode.txt` (aomdec ladder),
`r2/split.py` + `r2/sb.py` (frame split / per-SB dump), `probe_r2.log` (post-fix counters).
