# lane-chroma r4 — finish the chroma refusal removal

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-chroma`, branch
`lane-chroma`. Read `lanes/chroma-r3.charter.md` (still binding) and, briefly,
`lanes/chroma-r1.report.md` for the root cause. Do not re-derive them.

## State
- `8bca1d6` — the real luma fix: `smooth_neighbor` threaded into the intra edge
  filter strength instead of hardcoded `false`.
- `bafda07` — r2's smooth-intra gate, cap-rescued, compiles.
- HEAD — r3's work, cap-rescued by the orchestrator: `decode.rs` +99,
  `stream.rs` +126. r3's last words before the cap were "now let's add the r1
  gate for smooth/paeth chroma (charter step 2), then run the full lib test
  suite" — so the luma gate is believed to have gone green in r3's hands, but
  **no suite run has ever been seen on this tree**. Verify, do not assume.

## Order
1. `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-chroma` then
   `EC_AV1_REQUIRE_AOMENC=1 nice -n 19 cargo test -p ec-av1 --lib -j4`
   (timeout >= 600000 ms). Report the count against 232/0 for this tree.
   If red, fixing HEAD is the whole job until it is green. COMMIT once green.
2. Chroma: UV-mode neighbour tracking in `Neighbours`, remove the smooth/paeth
   chroma refusals (decode.rs uv_mode 9..=12, around 1970 and 2458), with r1's
   gate `a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact`
   (in `lanes/chroma-r1-attempt.diff`) green on luma AND chroma. COMMIT.
3. Note for the merge: main carries `crates/ec-av1/src/gate_coverage.rs`, whose
   `NEVER_EXERCISED` list still names `enable-smooth-intra` and
   `enable-paeth-intra` as tools no gate exercises. Once your gates enable them,
   those two entries must be deleted — that guard exists so closing the hole is
   noticed. If `gate_coverage.rs` is not in your tree, say so in the report so
   the merge does it.
4. If turns remain: the directional-chroma sibling at `decode.rs:7786` (the
   inter-frame intra-block reader refuses any non-DC/CFL uv_mode and never
   computes `angle_delta_uv`). COMMIT.

## Gate rules
`EC_AV1_REQUIRE_AOMENC=1` on every run; `-t <seconds>` on every ffmpeg generate;
fixtures through `gradients_source(seed, w, h, tail)`; aomenc
`--threads=1 --row-mt=0 --enable-smooth-intra=1 --enable-paeth-intra=1`;
firing counts are HARD asserts via thread-local `Cell<usize>` counters.

## Hard rules
Own `CARGO_TARGET_DIR` as above; foreground builds `nice -n 19 cargo ... -j4`;
every `cargo test` a timeout >= 600000 ms. Sibling worktrees (edith_codecs,
-realworld, -lr, -superres, -tiles, -palette) have live agents — never build in
or edit them. Never push, never merge, never touch `main`. 75-turn cap, does not
reset: COMMIT AT EVERY GREEN STEP — three rounds in a row on this lane have
ended with the work uncommitted. End with `lanes/chroma-r4.report.md`, VERDICT
on line 1.
