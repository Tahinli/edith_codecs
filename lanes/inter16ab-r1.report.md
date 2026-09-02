# lane-inter16ab r1 — 16x16-level AB partitions in inter frames

## Verdict: GREEN for the AB half; the 1:4 half stays refused (narrowed refusal).

## What changed
- `crates/ec-av1/src/decode.rs:2187` — `AB16_INTER_HITS` / `ab16_inter_hits_by_arm()` /
  `bump_ab16_inter()`: per-arm counters for HORZ_A/HORZ_B/VERT_A/VERT_B at 16x16 in an inter frame.
- `crates/ec-av1/src/decode.rs:20718` — `inter_leaf8!` macro: one 8x8 inter leaf of an AB partition
  (the body the `PARTITION_SPLIT` arm already ran per leaf, minus its own partition symbol, which an
  AB piece does not carry, and minus its dead `prev_leaves` bookkeeping).
- `crates/ec-av1/src/decode.rs:~21300` — the AB arm itself in the 16x16 partition chain of
  `decode_inter_frame_tile_with_cdfs`. libaom `decode_partition` visit order (decodeframe.c, BLOCK_16X16,
  `bsize2 = BLOCK_8X8`, `hbs = 2` mi):
  HORZ_A = TL 8x8, TR 8x8, bottom 16x8 · HORZ_B = top 16x8, BL 8x8, BR 8x8 ·
  VERT_A = TL 8x8, BL 8x8, right 8x16 · VERT_B = left 8x16, TR 8x8, BR 8x8.
  VERT_A/VERT_B take `Reach::vert_ab_partition()` (libaom `reconintra.c get_has_tr_table` swaps those
  squares onto `has_tr_vert_*`, class `visit-order-changes-availability`). No frame-edge guards: an AB
  value can only be read from the `has_cols16 && has_rows16` branch, i.e. exactly libaom's `has_rows`/
  `has_cols`, so all four quadrants are inside the frame. Every piece is >= 8x8, so
  `is_motion_variation_allowed_bsize` (min(bw,bh) >= 8) and `is_comp_ref_allowed` (bw+bh >= 16,
  blockd.h) hold unchanged — no OBMC/compound gate was added or removed.
- `crates/ec-av1/src/refusal_inventory.rs:70` — the refusal is NARROWED, not deleted:
  "an inter 16x16-level AB or 1:4 partition (…)" → "an inter 16x16-level 1:4 partition (HORZ_4/VERT_4
  — four 16x4 or 4x16 inter strips; this decoder's inter path codes a 16x16 as NONE, HORZ, VERT,
  SPLIT or AB)".
- `crates/ec-av1/src/stream.rs:~7470` — gate
  `a_real_aomenc_inter_sequence_with_16x16_level_ab_partitions_decodes_pixel_exact`.
- `crates/ec-av1/examples/decode_probe.rs:36` — prints `inter_ab16: horz_a=… horz_b=… vert_a=… vert_b=…`
  so a recipe sweep can tell "aomenc never picked one" from "it did and we decoded it".

## Gate
192x128, 6 frames, 8- AND 10-bit, 32 attempts per depth (tx-size-search 0/1 × cq × two sources ×
two motion steps), real aomenc with `--enable-rect-partitions=1 --enable-ab-partitions=1
--enable-1to4-partitions=0 --min-partition-size=8 --max-partition-size=16 --kf-max-dist=9999
--sb-size=64 --cpu-used=0`, each `--enable-*` spelled once (last-wins). Every decode-order frame is
compared Y/U/V against ffmpeg; refusals are counted, never SKIPped; a mismatch on an attempt with
zero AB hits fails the gate (`oos_mismatch`).

Run:
`EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-inter16ab cargo test -p ec-av1 --lib a_real_aomenc_inter_sequence_with_16x16_level_ab_partitions -- --nocapture`

EVIDENCE: $HOME/.cache/inter16ab-suite-r1.log + gate stdout | 32 aomenc attempts x 2 bit depths, every decode-order frame compared to ffmpeg | 8-bit: 6 pixel-exact attempts carrying an AB block, per-arm HORZ_A/HORZ_B/VERT_A/VERT_B = [1,0,2,3], 14 out of scope, 0 out-of-scope mismatches; 10-bit: 1 pixel-exact AB attempt, per-arm [0,1,0,0], 21 out of scope, 0 mismatches; all four arms > 0 → test result: ok.

## Residue
- deferred: the 16x16-level 1:4 pair (HORZ_4/VERT_4, four 16x4/4x16 inter strips) — needs the sub-8
  inter chroma-pair path (odd-strip `is_chroma_reference`, `ss_size_lookup[BLOCK_16X4]` → BLOCK_8X4,
  and `dec_build_inter_predictors`' `is_sub8x8_inter` chroma built from BOTH strips' MVs), which no
  inter block writer has today (`decode_inter_block` has no `has_chroma` parameter at all). Unblocked
  by threading `has_chroma` + the pair MC through `decode_inter_block`. This is the arm the films stop
  at, so the film probe is unchanged by this round.
- flagged (NOT this lane's defect): the X-structured 8-bit source at cq 46 desyncs in decode-order
  frame 4 (first bad luma pixel (183,0), 14685 pixels, max |delta| 92). Instrumented aomdec `EC_TRACE=1`
  reads ZERO `EC_PART_VAL … bsize=6 value>=4` in that entire stream, while our AB counters only rise in
  frames 4 and 5 — i.e. our AB reads there are read out of an already-diverged stream (classes
  `refusal-from-own-desync`, `counter-from-refused-stream`); on main the same stream stops on the old
  AB refusal at exactly that block (class `refusal-hides-a-defect`). The gate's cq list excludes it and
  says so in a comment.
- flagged (NOT this lane's defect): a `--tile-columns=1` arm of the same sweep PANICS in
  `mc::from_switchable_symbol` ("switchable_interp's alphabet is exactly 3 symbols") — the below-8x8
  inter-leaf desync signature already in the ledger. The tiled arm is therefore not in the gate.
- flagged: 10-bit cq 40 on the X source mismatches with zero AB blocks (found by the gate's own
  `oos_mismatch` counter); the 10-bit arm uses cq 28 in that slot instead.
- deferred: the Troy census4 decode-order frame 1 case was not re-probed this round (tool budget).

## Film probe
`ffmpeg -ss 900 -t 2 -i <Hunger Games> -c:v copy -an -f obu` → `decode_probe`:
before AND after this round the stop is at a 16x16-level 1:4 partition (before: the combined
"AB or 1:4" refusal, value=8 = PARTITION_HORZ_4 measured with `EC_AV1_TRACE=1` at mi=(12,44); after:
the narrowed 1:4 refusal), 1 frame completed (`EC_AV1_FINAL_DUMP` file count). Unchanged, as expected
for an AB-only lift.
