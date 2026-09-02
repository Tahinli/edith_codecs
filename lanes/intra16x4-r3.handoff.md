# lane-intra16x4 r3 HANDOFF

## 1. Attempt 0 verdict: OURS, root cause found and fixed (mv stack, 4-wide / 4-high blocks)

Stream reproduced exactly as gate attempt 0 (`~/.cache/intra16x4-tmp/gen_a0.sh 8` ->
`a0_8.obu`, md5 `0c9c588bb3d19f2feb2cecd31aa829fe`; 128x128 cq60 `noise`, 8-bit).
Before the fix: 6 frames decoded, `cmp` vs ffmpeg differs at byte 26363 = frame 1 (122,13).

Ladder (ours `EC_TRACE_MODE`/`EC_TRACE_MODE_STEP`/`EC_TRACE_COEFF` vs instrumented aomdec,
same rungs) -- first diverging element is NOT a coefficient: the `tag=all_zero` ladder
diverges at #515, but the mode ladder diverges earlier, at frame 1 block **mi(24,15)**, the
last 4x16 strip of a 16x16-level `PARTITION_VERT_4`:

| | stack |
|---|---|
| ours | (12,28) w648, (-4,26) w648, (-4,18) w8 -> mv0 (-4,18) |
| aomdec | (12,28) w**650**, (-4,18) w648, (-4,26) w8, (6,332) w4 -> mv0 (-4,26) |

`crates/ec-av1/src/mvstack.rs` `scan_row` / `scan_col` (and their `_compound` twins) were
missing two clauses of libaom `scan_row_mbmi` / `scan_col_mbmi`
(`~/.cache/aom-oracle/src/av1/common/mvref_common.c:143-236`):

1. the weight / `processed_rows` branch is `if (xd->width >= mi_size_wide[BLOCK_8X8] &&
   xd->width <= n4_w)` -- we had only `bw4 <= n4`, so a **4-wide / 4-high** block (exactly a
   1:4 strip or a sub-8x8 leaf) got a raised weight AND set `processed_rows`, which also
   moves the later outer scans (that is the missing 4th candidate);
2. `if (abs(row_offset) > 1) { col_offset = 1; if ((mi_col & 0x01) && xd->width < 2)
   --col_offset; }` -- the step-back on an odd mi_col under a sub-8px block was absent.

Both clauses ported at all four sites (row, col, row-compound, col-compound).
AFTER: `a0_8.obu` is entropy-exact -- all **125** aomdec `EC_MODE` entries match ours as an
ordered subsequence (ours prints 5 extra `EC_MODE` lines for intra strips aomdec does not
print) -- and the decode now stops on ANOTHER lane's named refusal,
`an intra 8x4/4x8 block inside an inter frame's sub-8x8 HORZ/VERT partition`, instead of
producing wrong pixels. Aligner: `~/.cache/intra16x4-tmp/align.py o_m2.log r_m.log`.

## 2. r2's `g_8.obu` "6 frames bit-exact" claim is FALSE (measured, class stale-measurement)

`~/.cache/intra16x4-tmp/g_8.obu` (md5 `2eaafd9eb56fbf9d597820ad74c9c4e3`, the r2 witness)
does NOT decode pixel-exact, and did not before this round's fix either: a build of the
**unmodified HEAD f403337** mvstack.rs mismatches at the same byte 53825 = frame 2, luma
(64,88). aomdec and ffmpeg agree with each other, so the reference is not in doubt.

Its first diverging element (`EC_TRACE_COEFF`, all_zero #913) is a DIFFERENT shape, not this
lane's: frame 2 block **mi(14,12)**, an 8x8 compound leaf (`ref0=1 ref1=4`, mode 17).
Mode info is exact through the interp read (both at rng 34722); then aomdec reads var-tx
`txfm_split` symbols (rng 34722 -> 62656) and its first luma TU has `txb_skip ctx=6`, while
we read NO tx-split symbol at all and use `ctx=3`. Owner: the sub-8x8 / var-tx inter lane,
not intra16x4.

## 3. Gate state

`a_real_aomenc_inter_sequence_with_intra_16x4_strips_in_1to4_partitions_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs:11023`) rewritten to **continue-and-sweep**:
* named refusals accepted ONLY from a measured 3-string list (sub-8x8 HORZ/VERT intra,
  sub-8x8 split intra 4x4, OBMC-neighbour interp) -- anything else is a failure;
* a mismatching attempt is RECORDED and the sweep continues (r2's attempt 0 panicked before
  arm 4 ran); every decoding attempt compares every decode-order frame Y/U/V;
* ONE end-of-sweep assert over all recorded failures (mismatches + out-of-scope mismatches +
  unmeasured refusals), then a per-bit-depth assert that the counter fired on >= 1
  pixel-exact attempt at every depth that decoded anything.
Still `#[ignore]`d and the refusal is **NOT lifted**: `EC_INTRA16X4_DECODE=1` remains the
bypass, `refusal_inventory.rs` / `gate_coverage.rs` untouched -- because no recipe found so
far gives a pixel-exact attempt with the counter > 0 (see 4).

Sibling gates run (`cargo test -p ec-av1 --lib -- 1to4 obmc mv_stack refusal_inventory
gate_coverage vartx_rect`): **26 passed, 2 failed, 5 ignored**. The two failures are the two
gates the ledger already records as known-red firing-assert/recipe defects on main
(`..._16x16_level_1to4_partitions...`: "VERT_4/split-tx-8x4 arms read 0";
`real_aomenc_1to4_streams_..._rect_vartx_leaves_fire`: "rect var-tx leaf arm never fired").
Neither is a pixel mismatch, and both fail on a 0-count arm, i.e. recipe, not decode.

## 4. Sweep (PARTIAL -- stopped at the turn cap, 49 of 120 rows)

`~/.cache/intra16x4-tmp/sweep_r3.sh` -> `sweep_r3_partial.txt`: 5 sources x cq 55/60/63 x
192x128|176x144 x --min-partition-size 8|4 x 8/10-bit. 49 rows done, ALL 8-bit.
**0 EXACT.** Every row is either a named refusal (sub-8x8 intra, overwhelmingly) or a
MISMATCH; the mismatching rows all have `16x4=0 4x16=0`, i.e. out-of-scope shapes.
`--min-partition-size=4` is what makes strips fire (up to `16x4=3 4x16=10 chroma_ref=7`) and
is also what brings the sub-8x8 intra refusal. 10-bit rows were not reached -- rerun the
script (it is idempotent; it appends) to finish rows 50..120.

## 5. Films (with the fix, `EC_RECT64_SPLIT=1 EC_INTRA16X4_DECODE=1`)

`hg-0` 16x4=14, `hg-300` 4x16=8, `t900` 4x16=4, `t5400` 0/0, `t8100` 0/0 -> all stop at
`an intra 8x4/4x8 block inside an inter frame's sub-8x8 HORZ/VERT partition`; `t6300`
(16x4=1 4x16=1) stops at `an OBMC neighbour whose switchable interp filter was never
recorded`. Same wall as r1 reported; no before/after frame counts measured this round.

## Next step

1. Finish the sweep (rows 50..120, the 10-bit half) looking for one EXACT row with a
   counter > 0. If none exists, this lane cannot lift its refusal until the sub-8x8 intra
   lane lands -- every strip-firing recipe found so far also carries that block.
2. The gate's arm list still contains the r2 `bar_h` arm 4 whose stream (`g_8.obu`) carries
   the frame-2 8x8-compound var-tx defect of section 2; it will be recorded as a mismatch
   until that lane's defect is fixed.
