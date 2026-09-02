# lane-rectsplitx r2 report

Branch `lane-rectsplitx`, rebased onto main `ce05d5f` (clean), commit `ec0b3b5`.

## Root cause (the r1 premise was wrong)

r1 refused the depth-1 RECT transform unit (`TX_16X8`/`TX_32X16`) as "measured
wrong pixels". It is not the transform unit. Range ladder, band fixture 8-bit
cq 12 seed 42 (`EC_TRACE_MODE_STEP=1` ours vs instrumented aomdec):

```
ours: EC_ISTEP mi_row=4 mi_col=8 name=mode val=11 rng=52576
ref : EC_ISTEP mi_row=4 mi_col=8 name=mode val=12 rng=34624
      (entry rng identical, 38331, on the preceding dq line)
```

Same entry range, different value => a different `kf_y_mode` CDF row => the
neighbour MODE, not the transform. Three defects, all in the neighbour bands:

1. `decode.rs` `decode_block_rect4` unsplit arm never wrote the mi-exact
   mode/uv maps (`record_mode_mi`/`record_uv_mode_mi`); only the split arm did
   (via `record_mi_luma_rect`). So `mode_left_mi` returned `None` and the
   coarse 16x16 cell answered -- holding the LAST of the four 32x8 strips.
2. New `Neighbours::modes_above_left_mi` (decode.rs, above `modes_above_left`):
   mi-exact AND availability-correct. Four 32x8 strips at tile column 0 share
   one `left_mode` cell, so strip 2 read strip 1's mode as its "left" where
   libaom has no left neighbour at all (measured at mi(10,0): ours left=2,
   libaom DC). Class swept into `decode_leaf_rect` and `decode_leaf8`.
3. `decode_block_rect4` asked `smooth_uv_neighbour(r*(SUB/MI), c*(SUB/MI), ..)`
   -- the SUB cell's top-left mi, not the strip's real mi (a 1:4 strip starts
   8 px into a 16-px cell). Chroma-only defect: seed 43 VERT_4 had luma
   bit-exact with U/V wrong.

## Evidence

EVIDENCE: scratchpad/s42.obu (sha256 7d578ff5deb0c6ec1ac7142ac0b73dcbdf702417da7868448bada17b4cb07e18) | aomenc band fixture 192x128 8-bit cq12 seed42 --enable-tx-size-search=1, EC_AV1_PREFILT_DUMP ours vs aomdec | 22986 luma + 5293 U diffs -> 0 (byte-identical 36864-byte dumps), rect4_32 horz=92 coded=92
EVIDENCE: scratchpad/s43_17.obu | same recipe, axis X (VERT_4), seed43 cq17 | luma 0 diffs / U 39 / V 150 -> byte-identical after the `smooth_uv_neighbour` mi fix, rect4_32 vert=88 coded=88

## Gate

`a_real_aomenc_stream_with_a_32x32_level_1to4_partition_decodes_pixel_exact`
now HARD-asserts `depth1_proved > 0 && depth2_proved > 0` (was
`depth1+depth2 == 0`, the refusal-honest form), and both `tx_w != tx_h`
refusals are deleted from `decode.rs` + `refusal_inventory.rs`.

**This gate is RED as committed.** 8-bit seed 46 (cq 12, tx-size-search on)
mismatches CHROMA ONLY: luma bit-exact, U 32 samples and V 173 samples off by
+-1..2, bounding box U x16..23 y8..15 / V x16..35 y8..24, first wrong sample
chroma (16,8) = the 16x4 chroma of the 32x8 strip at luma (32,16). The same
seed at cq 32 (tx-size-search OFF) is byte-exact, so it is specific to a
strip whose luma transform is SPLIT. Reproducer:

```
ffmpeg -v error -f lavfi -i "color=c=gray:s=192x128:d=0.04:r=25,format=gray,\
geq=lum='mod(floor(Y/8)*91,256)',noise=alls=6:all_seed=46,format=yuv420p" \
  -t 0.04 -pix_fmt yuv420p -strict -1 -f yuv4mpegpipe - > src46.y4m
aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=12 --cpu-used=0 \
  --threads=1 --row-mt=0 --sb-size=64 --bit-depth=8 --input-bit-depth=8 \
  --enable-rect-partitions=1 --enable-ab-partitions=0 --enable-1to4-partitions=1 \
  --min-partition-size=8 --max-partition-size=32 --enable-restoration=0 \
  --enable-palette=0 --deltaq-mode=0 --enable-filter-intra=0 --enable-cfl-intra=0 \
  --enable-intrabc=0 --enable-tx-size-search=1 --obu -o s46.obu src46.y4m
EC_AV1_PREFILT_DUMP=o cargo run -p ec-av1 --example decode_probe -- s46.obu
EC_AV1_PREFILT_DUMP=r aomdec --rawvideo -o /dev/null s46.obu   # o.f0 vs r.f0
```

Entropy is exact through the whole frame (all luma bit-exact), so the residue
is chroma RECONSTRUCTION inside `decode_rect_split`'s chroma arm
(decode.rs ~5060-5130) -- prediction inputs or the 16x4 inverse, not a symbol.

FALLBACK TO GREEN (one edit, if the batch needs a merge before that is found):
re-insert `if tx_w != tx_h { return Err(unsupported(..)) }` in
`decode_rect_split` (before `let luma_set =`) and in `decode_block_rect4`
(before `let which = if depth == 1`), re-add the two strings to
`refusal_inventory::REFUSALS`, and flip the gate's depth assert back to
`assert_eq!(depth1_proved + depth2_proved, 0, ..)`. The three neighbour-map
fixes stay and remain gated by the unsplit half of the same gate.

## r1's two ignored gates -- re-measured, both still ignored

- `a_real_aomenc_10bit_filter_intra_on_a_sub16_strip_decodes_pixel_exact`
  (and its 8-bit twin): still CHROMA-only. 10-bit seed 49 frame 0 plane V,
  first diff row 4 col 60, ours 510 vs ffmpeg 509,
  `filter_intra_rect_sub16_hits=2`. Ignore text updated with that measurement.
  deferred: sub-16 filter-intra chroma prediction -- unblocked by the same
  chroma-reconstruction hunt as the seed-46 residue.
- `a_directional_16x8_strip_reads_the_right_above_right_samples`: NOT this
  lane's defect any more -- every seed 700..739 attempt now stops at another
  lane's refusal "a HORZ_A/HORZ_B/VERT_A partition below 16x16", so the gate
  reaches no strip at all. deferred: blocked on AB partitions below 16x16.

## Films

Not re-measured this round (turn cap). r1 measured hg5.obu moving to
"a HORZ_A/HORZ_B/VERT_A partition below 16x16"; the ledger records that no
`troy-head.obu` exists on the box.

## Suite

`systemd-run --user --unit=rectsplitx-suite-1788313999` ->
`$HOME/.cache/rectsplitx-suite-r2.log`. Expected: 1 failure, the 1:4 gate
above (the only gate whose assertion this round strengthened).
