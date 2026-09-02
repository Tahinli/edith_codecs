# lane-intrasplit r3 report (branch lane-intrasplit, tip 50c43cd + this doc)

## Verdict: GREEN — both gate arms pass, zero named refusals, both tx depths seen.

## What changed (r3, commit 50c43cd)
- `crates/ec-av1/src/decode.rs:11587,11602` (`tx_size_context_txfm_rect`, `tx_size_context_txfm`) --
  ROOT CAUSE. These implement libaom `get_tx_size_context`
  (`~/.cache/aom-oracle/src/av1/common/pred_common.h:342-361`): when the above/left neighbour is an
  INTER block its BLOCK size, not its transform-context band, decides the `tx_depth` CDF row
  (`if (has_above) if (is_inter_block(above_mbmi)) above = block_size_wide[above_mbmi->bsize] >= max_tx_wide;`,
  pred_common.h:355-357, and the `left`/`block_size_high` twin at :359-361). Both of our copies
  indexed the `above_inter` / `left_inter` predicate bands as `mi_c / (SUB / MI)` / `mi_r / (SUB / MI)`,
  but those bands are mi-granular everywhere else they are written (`record_inter_rect_mi`) and read
  (`intra_inter_ctx`). The override therefore consulted a neighbour four mi columns/rows away, and at
  the first intra block to the right of an inter one it read `tx_depth` off ctx 0 where libaom has
  ctx 1 -- same decoded VALUE, different interval narrowing, so the tile desynced from there
  (class [[wrong-alphabet-same-value]] shape, via a mis-scaled index).
- `crates/ec-av1/src/decode.rs:6558` -- `EC_SPLITSTRIP` now prints EVERY intra strip with its depth,
  not only split ones: a source that codes no strip at all and one that codes only unsplit strips were
  indistinguishable through the old print.
- `crates/ec-av1/src/stream.rs:5536,5559,5648` -- gate geometry moved from 192x128 / min=max=32 to
  96x96 / min 16 / max 64, cq floor 63 -> 4. With a CORRECT decode the old geometry contains ZERO intra
  strips in an inter frame; r1/r2's "firing" counters had been read off our own desynced decode
  (class [[counter-from-refused-stream]]).

## Gates (this tree, EC_AV1_REQUIRE_AOMENC=1)
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intrasplit cargo test -p ec-av1 --lib -- --nocapture --test-threads=1 split_transform_intra_strip`
-> `test result: ok. 2 passed; 0 failed; 0 ignored`.

EVIDENCE: $HOME/.cache/intrasplit-r3-gates.log:884-885 | 10-bit arm, 3 aomenc attempts (seeds 42-44, cq 30, cpu 0-2), each decoded and pixel-compared vs ffmpeg | buckets counted-exact=2 uncounted-exact=1 named-refusals=0; split strips seen depth1=1 depth2=3; 0 pixel diffs in any plane of any of 8 frames per attempt
EVIDENCE: $HOME/.cache/intrasplit-r3-gates.log:908-909 | 8-bit arm, 21 attempts (seeds 42-62, cq 30/20/12/4, cpu 0-4) | buckets counted-exact=6 uncounted-exact=15 named-refusals=0; split strips seen depth1=9 depth2=1; 0 pixel diffs
EVIDENCE: commit 50c43cd message | aomdec `EC_ISTEP name=tx_depth` ladder on the r2 pinned stream (10-bit 192x128, md5 50ea2b42423f1c8b4eed9fa48c4775a6) before vs after | 85/147 elements identical (first divergence mi(24,24) ctx 0 vs libaom 1) -> 147/147 identical, stream decodes pixel-exact

The 15 "uncounted-exact" 8-bit attempts are decoded-and-pixel-compared attempts in which aomenc's RD
simply coded no split strip; they are never SKIPs and never mask a mismatch (the pixel asserts run on
every successful decode, class [[gate-skips-on-its-own-failure]]). Named refusals: 0 in both arms.

## Sibling gate: inter16ab 16x16-level 1:4 -- RED, inherited, NOT this lane's
`a_real_aomenc_inter_sequence_with_16x16_level_1to4_partitions_decodes_pixel_exact` (stream.rs:8986)
fails on this tree exactly as r2 reported: attempt 1, 8-bit, arms [0,4,2,2], frame 4 = 25 luma pixels
differ first at (6,49) max |delta| 1; frame 5 = 14 pixels first at (3,48) max |delta| 1.

EVIDENCE: $HOME/.cache/intrasplit-r3-gates.log:1795-1799 (tip 50c43cd) vs $HOME/.cache/intrasplit-r3-r2chk.log (detached worktree at r2 tip 5691e9a, own CARGO_TARGET_DIR) | ran the same gate on both commits | BIT-IDENTICAL failure: same frames, same 25/14 pixel counts, same (6,49)/(3,48) first-diff coordinates, same max |delta| 1 -- r3's fix neither caused nor moved it
EVIDENCE: $HOME/.cache/intrasplit-r3-mainchk.log (detached worktree at main cc323d0) | same filter | `0 passed; 0 failed; 418 filtered out` -- the gate does not exist on main: main merged lane-inter16ab at af7de9f, this gate arrived with that lane's LATER tip 4811488, which only this branch carries

No msac RANGE ladder was run on it, deliberately: the entropy stream is provably in sync (frames 0-3
are bit-exact in all three planes and the decode completes) and a max |delta| of 1 over 39 pixels is a
RECONSTRUCTION-stage difference, not a desync -- a range ladder is the wrong instrument. The rows
(48, 49) sit on a 16-row boundary at low columns, so the first hypothesis for whoever owns it is the
lf-grid fill for a 16x16-level 1:4 partition's internal horizontal edge (r14's `fill_lf_grid_rect` /
(row,col,tw,th) var-tx leaves crossed with inter16ab's 1:4 arms) -- hypothesis, not a measurement.

Disposition: deferred(the merge that lands lane-inter16ab tip 4811488 alongside lane-r14 b86eb38 --
class [[merge-cross-product-defect]]; neither parent alone is red, and neither the gate nor its
failure exists on main today).

## Suite
`EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intrasplit cargo test -p ec-av1 --lib -j3`
-> **387 passed / 1 failed / 35 ignored**, 1460 s ($HOME/.cache/intrasplit-suite-r3.log). r2 was 385/3/35;
the two that flipped green are this lane's own gate arms. The single remaining failure is the inherited
1:4 cross-product above -- no other regression.

## Residue
- The 1:4 cross-product above: deferred, owner = the merge, as stated.
- accepted: the 8-bit arm needs 21 attempts to see both depths; depth2 is rare at 96x96. Both depths
  ARE seen in pixel-exact attempts, which is what the gate asserts.
