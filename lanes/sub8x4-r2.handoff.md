# lane-sub8x4 r2 HANDOFF (tip = `2515eb1`, branch lane-sub8x4)

Two commits on top of `94bda2f`:

* `f0bd09d` fix: compound 8x8 leaf published transform width 8 to the deblock grid even
  when its var-tx split it into 4x4 units
* `2515eb1` feat: both sub-8x8 intra refusals lifted behind the new gate

## STEP A -- DONE, gate GREEN, both refusals LIFTED

### The last defect (root cause, class `early-return-skips-tail`, 3rd instance in that arm)

`decode_inter_block8`'s COMPOUND early return called
`neighbours.fill_lf_grid(leaf_mi, 2, 8, ..)` with a LITERAL 8 where the fall-through
single-ref tail writes `if split8 { 4 } else { 8 }` (decode.rs, ~line 24990 vs ~25980).
libaom `get_transform_size` (av1_loopfilter.c:207) reads the per-TU `inter_tx_size` for a
non-skip inter LUMA block, so a var-tx-split 8x8's interior edges at `x % 8 == 4` are real
filter edges; ours never filtered them and widened the neighbouring x=64 edge from filter4
to filter8.

Ladder that named it, on `~/.cache/sub8intra-tmp/g_8_63_128x128.obu`:
* `EC_OBMC` trace ours vs instrumented aomdec: **404 = 404 lines, byte-identical** (OBMC
  neighbour lists are exact -- do not re-audit them).
* `EC_AV1_PREFILT_DUMP` (ours, no `EC_AV1_DEBUG_SKIP_CDEF`) vs aomdec's: decode frames
  0,1,2 **SAME**; only f3+ differ (propagation).
* `EC_AV1_POSTDEBLOCK_DUMP` both sides: frame 2 differs, 12 luma px at rows 78..80 cols
  58..63, max delta 3 -> the deblocker is the stage.
* `EC_AV1_DEBLOCK_TRACE_V`: no `VEDGE x0=60` in frame 2 where frames 0/1/3 have one;
  `EC_TXGRID_TRACE` (new `EC_LFLEAF` line added this round in `fill_lf_grid_leaf_luma`)
  showed the 8x8 at mi(20,14) publishing `tx_px=8` with no leaf fill.

Probe sweep `~/.cache/sub8intra-tmp/sweep3.sh` (12 streams): **10/12 -> 12/12 pixel-EXACT**.

### The gate

`a_real_aomenc_inter_sequence_with_intra_sub8x8_leaves_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs`, end of the test module).

* 12 arms: 8/10-bit x cq 48/55/63 x 128x128/192x128, recipe copied from sweep3.sh
  (`--min-partition-size=4 --max-partition-size=8 --enable-1to4-partitions=0
  --enable-rect-partitions=1 --cpu-used=0 --sb-size=64 --lag-in-frames=0`), streams
  re-encoded BY the gate, first arm encoded twice and compared byte for byte.
* 6 decode-order frames per arm, all three planes, vs `ffmpeg_decode_sequence` /
  `_10bit`. Continue-and-sweep: only `unsupported: ` is recorded and skipped; any other
  error or ANY pixel mismatch fails.
* End-of-sweep asserts `exact_arms >= 12` and all five `sub8_intra_rect_hits` cells > 0.
* **Result: 12/12 arms EXACT, counters `[8x4=362, 4x8=80, chroma_ref=156, mixed=153,
  split4x4=98]`, 7.42 s.**
* `SUB8_INTRA_RECT_HITS` grew a 5th cell (split 4x4 leaves); `decode_probe` prints
  `split4x4=`.

Lifted: `EC_SUB8INTRA_DECODE` (both sites) and the two refusal strings, removed from
`refusal_inventory.rs` in the same commit. `gate_coverage.rs` needed no edit (this gate
spells no tool flag that was previously never-on); `refusal_inventory` + all 12
`gate_coverage` tests pass.

Siblings re-run (`-- sub8 obmc mv_stack 1to4 refusal_inventory gate_coverage`):
**33 passed, 2 failed, 5 ignored**. The 2 failures are the pre-existing zero-COUNTER
recipe defects this branch already carried (`..._16x16_level_1to4_partitions...`
stream.rs:10994, `real_aomenc_1to4_streams_..._rect_vartx_leaves_fire` stream.rs:8876);
neither is a pixel mismatch and neither moved this round.

### NOT in the gate: the `--tile-columns=1` arm (measured, not assumed)

The arm was written, run, and **failed** at decode-order frame 4 (12056 luma px). It then
failed IDENTICALLY with `--min-partition-size=16 --max-partition-size=16` (frame 3, 13256
luma px) -- a stream with no sub-8x8 block of any kind -- so this is a PRE-EXISTING
multi-tile defect, not this lane's bands. Streams pinned:
`~/.cache/sub8x4-tmp/tiles.obu` (sub-8x8) and `~/.cache/sub8x4-tmp/t16.obu` (none);
`diff.py` in the same dir prints per-frame/plane deltas. The gate carries the repro in a
comment with the one line to restore. This is the single most valuable open item.

## STEP B -- NOT DONE, its premise is STALE

`~/.cache/intra16x4-tmp/g_8.obu` is NOT an intra-16x4 witness: re-measured this round with
`EC_INTRA16X4_DECODE=1`, it reports `intra16x4_in_inter: 16x4=0 4x16=0 chroma_ref=0` and
still mismatches at byte 53825. Its mismatch is another shape's defect (the ledger already
records the same stream as never-exact on unmodified HEAD). There is no firing witness at
all: the r3 49-row sweep found 0 EXACT rows and every row had `16x4=0`. Next step for B is
therefore NOT the prediction/OBMC/deblock comparison in the charter -- it is finding a
stream where the counter fires AND the rest of the frame is clean (r1's dead-end: every
firing film frame also carries a sub-8x8 intra block -- which is now DECODABLE, so re-run
`~/.cache/intra16x4-tmp/films.sh` and the hg0-5x prefixes on this tip: that dead-end may
have expired with this commit).

## STEP C -- suite armed, not read

Unit `sub8x4-suite-r2-1788363069.service`, log **`$HOME/.cache/sub8x4-suite.log`**.
Check with a single `grep -E "^test result|FAILED" $HOME/.cache/sub8x4-suite.log`.

## STEP D -- NOT RUN (turn cap). No film probe, no `EC_RECT64_SPLIT` measurement, no
fixture pinned. Inputs are in place: `~/.cache/hg-0.obu`, `~/.cache/hg-300.obu`,
`~/.cache/intra16x4-tmp/hg0-{50,52,55}.obu`.

## Next step, in order

1. Read the suite log; if a gate outside this lane's files is red, bisect against `94bda2f`.
2. Re-run the film/hg0 probes on this tip (STEP D) -- the sub-8x8 intra lift removes one of
   the two walls the r1 film sweep hit, so the refusal STRING each cut stops at has moved.
3. The multi-tile defect above (own lane; it is not sub-8x8 specific and it blocks the
   neighbour-map arm COMMON asks every lane for).
