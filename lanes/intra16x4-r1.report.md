# lane-intra16x4 r1 — an INTRA 16x4/4x16 strip inside an inter 16x16-level 1:4 partition

## Step 1 MEASURE — what the six film probes actually stop on

Env-gated print at the refusal site (`crates/ec-av1/src/decode.rs`, `EC_INTRA16X4=1`),
each probe under `systemd-run --scope -p MemoryMax=6G`, `EC_RECT64_SPLIT=1`:

| probe | shape | pair parity | has_chroma | prev strip single-ref inter | skip | tx_select | screen tools |
|---|---|---|---|---|---|---|---|
| 10-bit 3840x1608 cut 0   | 16x4 | 0 (EVEN) | false | – | 0 | 1 | 0 |
| 10-bit 3840x1608 cut 300 | 4x16 | 0 | false | – | 0 | 1 | 0 |
| 10-bit 1920x792 @900     | 4x16 | 0 | false | – | 0 | 1 | 0 |
| 10-bit 1920x792 @5400    | 16x4 | 0 | false | – | 0 | 1 | 0 |
| 10-bit 1920x792 @6300    | 16x4 | 0 | false | – | 0 | 1 | 0 |
| 10-bit 1920x792 @8100    | 4x16 | 0 | false | – | 0 | 1 | 0 |

6 of 6 first fire on an EVEN strip: `is_chroma_reference` false, so that strip reads no
`uv_mode` and codes no chroma at all. All non-skip, all with `--enable-tx-size-search`
on, none in a screen-content frame.

EVIDENCE: shell probe, 6 streams | `EC_RECT64_SPLIT=1 EC_INTRA16X4=1 decode_probe <cut>` | 3x 16x4 + 3x 4x16, all pair-parity 0 / has_chroma=false / skip=0

## Step 2 IMPLEMENT

* `decode.rs` `decode_rect4_16_strip` (new) — the per-strip body of the KEY FRAME's
  `decode_rect4_16`, lifted verbatim into its own function (mode info, luma TU or split
  walk, the pair's 8x4/4x8 chroma when `has_chroma`, palette bands, skip/lf/partition
  bands). `decode_rect4_16` now calls it four times; no behaviour change on the key-frame
  path (the whole suite is the check).
* `decode.rs` `decode_intra_rect_in_inter` — new `strip16: Option<(horz, has_chroma)>`
  parameter and a 16x4/4x16 arm that runs that body with `INTRA_IN_INTER_MODE = Some((0,
  skip))`: `size_group_lookup[BLOCK_16X4] == size_group_lookup[BLOCK_4X16] == 0`
  (oracle `common_data.h:61`, indices 16/17), so the mode is `y_mode[0]` and `skip` is
  the one `decode_inter_block` already read. Chroma needs NO new code: the lifted body
  already codes one 8x4 (4x8) unit at the PAIR's origin on the odd strip only, which is
  exactly "predicted per the carrying strip's own type" for a mixed intra/inter pair;
  the inter side's `InterStripChroma::prev == None` arm already covers intra-then-inter.
* Same arm publishes what the key-frame body does not and an INTER frame needs
  (`set_txfm_ctxs` + `txfm_partition_update`, libaom `decodeframe.c:1078`): the strip's
  tx size into `above_txfm`/`left_txfm` and the var-tx partition context — the 2:1
  intra-in-inter strip already does this. On the inter path the tx-depth symbol also
  reads `tx_size_context_txfm_rect` (the real TXFM_CONTEXT bands) instead of the key
  frame's deblock-grid approximation.
* Counter `INTRA16X4_IN_INTER_HITS` `[16x4, 4x16, chroma-reference]`,
  `stream::intra16x4_in_inter_hits()`, printed by `decode_probe` as `intra16x4_in_inter:`.

**The path is behind `EC_INTRA16X4_DECODE=1`; the refusal is still the default.** It is
not pixel-proven (below), and a refusal is lifted only with a green gate.

## Step 3 GATE — RED, and what it measured

`a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_decodes_pixel_exact`
(`stream.rs`, `#[ignore]`d with its own measurement in the reason string): real aomenc,
`--enable-1to4-partitions=1 --enable-intra-edge-filter=1 --min-partition-size=4
--max-partition-size=16` (per-arm overrides last), 4 recipes x {8, 10} bit, every
decode-order frame Y/U/V vs ffmpeg, refusals counted never SKIPped, `oos_mismatch == 0`.

Recipe search: 48 + 48 runs (`~/.cache/intra16x4-tmp/sweep.sh`, `sweep2.sh`, logs
`sweep.log`/`sweep2.log`/`sweep3.log`). aomenc picks an INTRA 1:4 strip on an inter frame
only at HIGH cq (>= 56) over content its MC cannot predict. Both gate sources hash
identically twice.

RED, with the strips themselves exact:

* 192x128 cq60 "bar" source, 8-bit, 4 intra 16x4 strips (2 chroma-reference): frame 0
  exact, frame 1 luma differs from (175, 0), 8400 px. The strips are at mi(1,40)/(3,40)
  = px x 160..175, y 4..15 — the first differing pixel is at y=0, i.e. in the EVEN
  (inter) strip ABOVE them, and chroma of the pair is exact.
* 128x128 cq60 "noise" source, 8-bit, 2 skipped intra 16x4 strips at px(80,112)/(80,120):
  frame 1 differs from (122, 13) — 99 rows ABOVE the first strip, so that arm's mismatch
  is not this shape at all (an unrelated defect this recipe exposes; the gate's
  `oos_mismatch` cannot separate it because the stream does carry strips).
* The txfm-context publication above was found the same way and DID fix a first
  measurement: before it, a 128x128 stream decoded its two intra strips at mi(23,16)/
  (23,20) exactly and broke on the very next 16x16 row (rows 96+, x>=62), chroma exact.

EVIDENCE: /home/tahinli/.cache/intra16x4-gate2.log + ~/.cache/intra16x4-tmp/{s4,s5}.obu | aomenc -> our decode -> ffmpeg raw compare, per frame | frame 1 first luma diff (175, 0) 8400 px with both intra strips' own pixels exact

## Film frontier — before / after (with `EC_INTRA16X4_DECODE=1`)

All six probes leave the 16x4/4x16 wall. Before: every one stopped on
`an intra 16x4/4x16 strip inside an inter 16x16-level 1:4 partition ...`.
After (counters are strips decoded):

| probe | new refusal | intra16x4_in_inter |
|---|---|---|
| hg cut 0    | an intra 8x4/4x8 block inside an inter frame's sub-8x8 HORZ/VERT partition | 16x4=14 |
| hg cut 300  | same | 4x16=8 |
| 1920x792 @900  | same | 4x16=4 |
| 1920x792 @5400 | same | 16x4=1 4x16=2 |
| 1920x792 @6300 | same | 16x4=1 4x16=1 |
| 1920x792 @8100 | an intra-coded 16x4/4x16 strip on the inter block path (its `_` arm: a CLIPPED shape, not a 1:4 strip) | 4x16=4 |

Since the walls are unproven pixel-wise, this is a REACH measurement, not a decode claim.

EVIDENCE: 6 probes | `EC_RECT64_SPLIT=1 EC_INTRA16X4_DECODE=1 decode_probe <cut>` | refusal string moves to the sub-8x8 intra wall on 5 of 6, counters 1..14 strips each

## Residue

* fix-now (next round): the r1 gate is RED. The defect is NOT in the strip's own pixels
  (they match) but in what the strip leaves behind for its neighbours — after the
  txfm-context fix, the surviving mismatch starts in the pair's EVEN (inter) strip above
  the intra one, which points at the pair's chroma/mv bookkeeping or the deblock grid
  the intra strip publishes over the pair, not at its luma.
* deferred: `EC_RECT64_SPLIT` bypass removal — no witness; the rect64 hits still live
  inside frames that refuse (confirms lane-rect64port r1 and lane-rectres r1).
* deferred: the 4x16 (VERT_4) orientation is unprovable by aomenc today — every
  VERT_4-carrying recipe in the 96-run sweep also contains an intra 8x4/4x8 or 4x4
  sub-8x8 block, another lane's refusal. The gate asserts exactly that blocker.
* accepted: `refusal_inventory.rs` / `gate_coverage.rs` unchanged — no refusal was lifted.
