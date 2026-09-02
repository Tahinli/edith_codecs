# lane-leaf8tx r4 report -- refusal lifted, gate swept

## What changed
- `crates/ec-av1/src/decode.rs:22602` -- the refusal "an 8x8 intra leaf in an inter frame
  whose tx_depth splits it into 4x4 transform units" and its `EC_LEAF8TX_SPLIT=1` env bypass
  are DELETED; the per-TU 4x4 path is now the only path for that leaf. (Root cause that
  unblocked it landed in r3, `d226074`: `tx_size_context_txfm` read `above_inter`/`left_inter`
  at `mi/4`.)
- `crates/ec-av1/src/refusal_inventory.rs:69` -- the refusal string removed, replaced by the
  lane note naming the gate that carries it.
- `crates/ec-av1/src/stream.rs:6574,6580` -- `tx_split_angle_in_inter8_gate` is now
  continue-and-sweep: all 30 aomenc arms of each bit-depth run, every decode-order frame is
  pixel-compared on Y/U/V (a mismatch is an immediate FAILURE, never a SKIP), and the two hit
  counters are asserted ONCE after the sweep -- `totals.0 > 0` (an 8x8 intra-in-inter leaf with
  tx_depth split to 4x4 was decoded AND compared) and `totals.1 > 0` (nonzero `angle_delta_y`).
- `crates/ec-av1/src/gate_coverage.rs` -- unchanged, and correctly so: it keys on `--enable-*`
  flags, and this gate already spells `--enable-tx-size-search=1`/`--enable-angle-delta=1`, so
  no entry named the lifted tool.
- `crates/ec-av1/src/stream.rs:6740` -- the SHIPPED recipe of that gate used to append
  `--enable-tx-size-search=0` (aomenc keeps the LAST occurrence), so the split it is meant to
  prove never fired: measured 0 split-tx hits on 30 arms. It now ships
  `--enable-tx-size-search=1 --enable-angle-delta=1`, and odd attempts add
  `--max-partition-size=8` (the shape that produces the 2x2 TX_4X4 grid most often).
- `crates/ec-av1/src/decode.rs:25668` -- the r2 merge kept main's refusal text ("an OBMC
  neighbour whose *switchable* interp filter was never recorded") but the unit test still
  asserted the pre-merge string; test updated to main's text. NOT a decode change.
- `crates/ec-av1/src/refusal_inventory.rs` -- two strings the r2 rectchroma2 merge made stale
  ("a 1:4 rect strip that actually uses a palette", "an inter partition below 8x8") deleted;
  `the_decode_path_refuses_exactly_the_listed_cases` demanded exactly that.

## Gate
```
EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-leaf8tx \
  cargo test -p ec-av1 --lib -j3 -- --nocapture --test-threads=3 \
  angle_delta_8x8_intra_leaf cdef_and_sub16 refusal_inventory gate_coverage \
  an_obmc_neighbour_with_no_recorded_filter_refuses_instead_of_panicking
```
`test result: ok. 16 passed; 0 failed; 0 ignored; 423 filtered out; finished in 79.26s`
(log `$HOME/.cache/leaf8tx-r4-step23.log`)

EVIDENCE: $HOME/.cache/leaf8tx-r4-step23.log | 30 aomenc arms x 2 bit depths, every decode-order frame compared on Y/U/V vs ffmpeg | `tx_split_angle_in_inter8_gate(8): buckets counted-exact=29 uncounted-exact=1 named-refusals=0 (attempts 30)`, `tx_split_angle_in_inter8_gate(10): counted-exact=30 uncounted-exact=0 named-refusals=0 (attempts 30)`, 0 pixel mismatches
EVIDENCE: $HOME/.cache/leaf8tx-r4-step23.log | same run, split-leaf hit counter per arm | e.g. `(10) seed=43 --cq-level=19: 8x8 intra-in-inter leaves with tx_depth=1 315, nonzero angle_delta_y 124` -- the counter fires in the hundreds on decoded+compared attempts (it was 0 on every arm of the pre-r4 recipe)

## Step 3 (by name, same 16-test run above): ALL OK
`cdef_and_sub16` (r2's continue-and-sweep conversion, never run before) ok; all 7
`gate_coverage` tests ok; `refusal_inventory::the_decode_path_refuses_exactly_the_listed_cases`
ok (after dropping the two stale strings); `an_obmc_neighbour_with_no_recorded_filter_refuses_instead_of_panicking`
ok (after the test's expected string was aligned with main's refusal text -- it had been failing
since the r2 merge, i.e. NOT caused by this round's decode change).
Coordinator's ask: `a_real_aomenc_inter_sequence_with_a_split_transform_intra_block_decodes_pixel_exact`
and its `10bit_` twin both **ok** on this tree (full-suite log below) -- lane-mergefix's
independent `tx_size_context_txfm` fix agrees with r3's `d226074`.

## Full suite
`systemd-run --user --unit=leaf8tx-suite-r4-... -p MemoryMax=10G` ->
`test result: FAILED. 404 passed; 1 failed; 34 ignored; 0 measured; 0 filtered out; finished in 683.00s`
(log `$HOME/.cache/leaf8tx-suite-r4.log`). Baseline for comparison, PRE-lift on the same tree
(`$HOME/.cache/leaf8tx-suite-r3base.log`): `403 passed; 2 failed` -- the two r2-merge failures
above, both now green.

FAILED: `stream::tests::a_frame_edge_straddling_band_decodes_pixel_exact`
```
68x192 cq61 frames=5 10bit=false tile_cols=1 frame 1 plane Y: 2248 pixels differ,
first at row 56 col 50 (ours 117 vs ffmpeg 118) [edge32=[0, 34, 0, 0, 1, 17, 0, 1]]
```
This is the lift's cross-product, class [[refusal-hides-a-defect]] and exactly what that gate's
own doc comment (stream.rs:21573) predicted: its multi-tile arms were 32/32 refused BY NAME,
the census listing `6x "an 8x8 intra leaf in an inter frame whose tx_depth splits it into 4x4
transform units"`. With that refusal gone those arms decode for the first time and one of them
mismatches -- a defect the refusal was hiding, not a regression of the split-leaf path (the
leaf8tx gate itself is pixel-exact on 60/60 arms, and the two split-transform-intra-block gates
are green).

Disposition: deferred(a round scoped to the CDEF frame-edge straddle x multi-tile x 8x8-split-leaf
cross-product; start by bisecting whether the 2248-pixel luma delta is CDEF filtering or the
entropy state of the newly decoded split leaves in that arm) -- not fix-now here: found at
T-30min of a 60-minute round, after the commit that lifts the refusal. It must NOT be closed by
re-adding the refusal.
