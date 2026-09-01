# lane-rectsplit r2 (gates run and reported by r3) — SB-level split-transform strips: r2's root cause DISPROVED

Branch `lane-rectsplit` off main `3808cf8` (r1 `3627fe9` already merged). r2 was capped
before running any gate; its work is WIP `bf53652`. This round ran the gates, found that
r2's fix regresses a merged gate, and reverted the unsafe half.

## Headline

r2 claimed the root cause of r1's RED gate (b) was a transposed `av1_nz_map_ctx_offset`
read (`base_ctx`, `[row][col]` -> `[col][row]`). **Measured: that change turns gate (b)
green (20/20) and turns the MERGED gate
`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact` RED
(seed 43 luma mismatch).** All four index/shape combinations were run:

| base_ctx TwoD rect read | SB HORZ/VERT partition gate (merged) | split-tx SB gate (b) |
| --- | --- | --- |
| `table(shape)[row][col]` (r1, shipped) | ok | FAILED (seed 50) |
| `table(shape)[col][row]` (r2's "fix") | FAILED (seed 43) | ok |
| `table(swap)[row][col]` | FAILED | FAILED |
| `table(swap)[col][row]` | FAILED | FAILED |

So the offset read is NOT the defect: each variant only moves which rare corner position
desyncs. What is genuinely established (checked against the oracle source, not another
transcription): `av1_nz_map_ctx_offset_32x64` is a FLAT 1024-entry array whose `coeff_idx`
is COLUMN-major with stride 32 — entries 0..4 = 0,11,6,6,21 and 32..34 = 11,11,6 match
`txb_common.h:199-209`'s generating rule only as `flat[col * 32 + row]` — so the `cdf.rs`
5x5 transcriptions are `[col][row]`, while `base_ctx` indexes them `[row][col]` and is
green that way. The unproven quantity is therefore the ORIENTATION of `(row, col)` coming
out of the square 32x32-corner scan (`pos / side`, `pos % side` at `decode.rs:1583`)
relative to libaom's column-major `coeff_idx` — the `scan-weights-cross-axis` /
`reference-layout-not-spec` class. That is the next round's bisection.

## State shipped on this branch (`89d4d59` + this commit)

KEPT from r2's WIP:
- `stream.rs` gates (a)/(b)/(c): per-attempt counter sampling — a feature-hit delta is
  folded in only after that attempt decoded AND pixel-compared, so hits from a stream that
  later refuses can no longer make a gate pass vacuously (ledger dead-end). Gate (b) also
  asserts the new `decode::rect_split_sb_interior_tu_hits` (`bw.max(bh)==64 && bw/tx>1 &&
  bh/tx>1` = SB-level AND tx depth >= 2), which is what the verifier asked for.
- Mismatch reporting: mismatching-sample count + bbox.
- `decode.rs:3248`: the chroma-shape refusal is documented UNREACHABLE with both callers
  wired and KEPT as the shape guard (without it a new strip size panics on `expect`
  instead of refusing by name); it stays in `refusal_inventory::REFUSALS`.
- Gate (c)'s stale "competitors disabled" comment replaced by the measured reason they
  cannot be (competitors off -> 0 rect strips; `--cq-level=25` is what fires filter intra).
- `decode::tests::nz_map_ctx_offset_tables_match_the_rect_rule` — pins the `[col][row]`
  packing of both rect tables against `base_ctx_rect`'s independently-written rule. It is
  TRUE about the transcription and deliberately contradicts `base_ctx`'s read; that
  contradiction is the open defect, spelled out in `base_ctx`'s comment.
- `examples/decode_probe.rs` raw-I420 dump (2nd arg); `EC_NZOFF_DUMP=1` prints every
  rect-shaped nz_map read; `EC_SBPART_DUMP64=1` prints split-tx strips.

REVERTED by r3 (unsafe):
- `base_ctx` back to the r1 `[row][col]` read (`decode.rs:1162` comment now records the
  four-way measurement instead of r2's claim).
- `decode_block_rect64` refusal "a superblock-level HORZ/VERT strip with a split transform
  (per-unit rect prediction is not ported)" RESTORED, re-added to `refusal_inventory.rs`.
- Gate (b) `#[ignore]`d again with the r3 reason.

## Refusals lifted this round: NONE.

## Evidence

EVIDENCE: `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j3 -- ...split_transform... ...superblock_level_horz_vert_partition... filter_intra_on_a_horz refusal_inventory gate_coverage nz_map_ctx_offset` on lane-rectsplit @ r3 final | 3 aomenc gates + the merged SB partition gate + inventory/coverage/table tests | 9 passed, 0 failed, 1 ignored (gate b), 53.77s
EVIDENCE: same command with `table[col][row]` (r2's WIP `bf53652`) | same gates | gate (b) ok (20/20 pixel-exact, SB-interior-TU delta=7), merged SB HORZ/VERT partition gate FAILED seed 43 — the regression that forced the revert
EVIDENCE: full `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j3` at r2's WIP state | whole crate lib | **270 passed, 1 failed, 23 ignored**, 1566.70s — the 1 failure is that merged gate. NOT re-run after the revert (turn cap): the reverted state restores r1's behaviour for every path plus one new unit test and one re-ignored gate, so 270 passed / 0 failed / 24 ignored is EXPECTED, not measured.

## Film check (both films, 0.4s extracts)

```
ffmpeg -v error -t 0.4 -i "<Troy ...DECK.mkv>" -c:v copy -f obu troy.obu    # 418 bytes
ffmpeg -v error -t 0.4 -i "<Hunger Games ...UH.mkv>" -c:v copy -f obu hg.obu # 1817 bytes
decode_probe troy.obu -> REFUSED: AV1 tile (a 32x32 partition type this decoder does not code (value=4))
decode_probe hg.obu   -> REFUSED: AV1 tile (a partition below 8x8 (this decoder codes no leaf smaller than 8x8))
```

EVIDENCE: scratchpad `troy.obu` (418 B) / `hg.obu` (1817 B) | `ffmpeg -t 0.4 -c:v copy -f obu` then `cargo run -p ec-av1 --example decode_probe` on this branch | neither film stops at a refusal this lane owns

## Residue

- fix-now(next round): the (row, col) orientation of the square 32x32-corner coefficient
  reader vs libaom's column-major `coeff_idx`. Repro both directions: seed 50 of gate (b)
  (`--ignored`) and seed 43 of the SB HORZ/VERT partition gate. Decisive instrument: msac
  RANGE ladder vs instrumented aomdec at the first differing coefficient of the 64x32 strip
  (r1 located it at luma (171,56), mi=(8,32)); `EC_NZOFF_DUMP=1` lists every candidate read.
- deferred(a below-16x16 strip gate — lane-sub8's territory): the `decode_leaf_rect` strip
  refusals.
- deferred(lane-sub8 / the 32x32 AB-partition arm): both films' first 0.4s.
