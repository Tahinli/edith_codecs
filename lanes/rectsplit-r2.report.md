# lane-rectsplit r2 — superblock-level split-transform strips, and the transposed nz_map offset

Branch `lane-rectsplit` off main `3808cf8` (r1 `3627fe9` already merged into main).
r2's build work was capped mid-round and preserved as WIP `bf53652`; this round ran the
gates it never reached, fixed one stale print label, and reports the result.

## What changed (r2's WIP `bf53652`, verified this round)

- `crates/ec-av1/src/decode.rs:4054` `decode_block_rect64` — the refusal
  "a superblock-level HORZ/VERT strip with a split transform (per-unit rect prediction is
  not ported)" REMOVED; the SB-level strip now routes through the same per-transform-unit
  path as its 32x32-level sibling (`decode_rect_split`, `sub_tx_size_map[TX_64X32] ==
  TX_32X32`, so `bw.min(bh) >> (depth - 1)` names the square unit).
  `refusal_inventory.rs:46` drops that string.
- **ROOT CAUSE of r1's RED gate (b)** — `crates/ec-av1/src/decode.rs:1159` `base_ctx`,
  `TxClass::TwoD`: libaom's `av1_nz_map_ctx_offset[tx_size]` is a FLAT array indexed by a
  COLUMN-major `coeff_idx` (`col * height + row`), so a transcription holding 32 consecutive
  entries per outer index is `[col][row]`, not `[row][col]`. The square table is symmetric
  and never noticed; the two rect tables are not. Read `table[col.min(4)][row.min(4)]`.
  This is defect class `reference-layout-not-spec` / `scan-weights-cross-axis`, NOT the
  per-unit edge-availability rule r1 suspected.
- `crates/ec-av1/src/decode.rs` (tests) `nz_map_ctx_offset_tables_match_the_rect_rule` —
  pins both rect tables against `base_ctx_rect`'s independently-written generating rule
  (`w<h && row<2 -> +11`, `w>h && col<2 -> +16`, else the square table) in display
  coordinates. A transposed read breaks it.
- `crates/ec-av1/src/decode.rs:3248` — the chroma-shape refusal
  "a coded HORZ/VERT strip whose chroma transform has no rect coefficient tables here" is
  documented as UNREACHABLE with both callers wired, and KEPT as the shape guard for the
  next caller (without it a new strip size panics on the `expect` instead of refusing by
  name). It stays in `refusal_inventory::REFUSALS`.
- `crates/ec-av1/src/stream.rs` gates (a) `:9363`, (c) `:9554`, (b) `:9790` — per-attempt
  counter sampling: a feature-hit delta is folded in ONLY after that attempt decoded AND
  pixel-compared, so hits from a stream that later hits a named refusal can no longer make
  a gate pass vacuously (ledger dead-end "counter-delta firing asserts ... count hits from
  streams that LATER refuse"). Gate (b) asserts on the NEW
  `decode::rect_split_sb_interior_tu_hits` (`bw.max(bh)==64 && bw/tx>1 && bh/tx>1`), i.e.
  a superblock-level strip split past depth 1 with a genuinely interior transform unit —
  the shared `rect_split_tx_hits` also counts 32x32-level and depth-1 strips and could not
  prove this case. Mismatch reporting gained the mismatching-sample count + bbox.
- Gate (c)'s stale comment claiming the directional/smooth/paeth competitors are disabled
  is replaced by the measured reason they cannot be (r1's table: competitors off -> 0 rect
  strips); the quantiser `--cq-level=25` is what fires filter intra on a strip.
- `crates/ec-av1/examples/decode_probe.rs` — optional 2nd arg dumps decoded planes as raw
  I420 for per-pixel diffing outside a gate.
- THIS ROUND: `stream.rs:9953` gate (b)'s summary line said `rect_split_tx_hits delta=` while
  asserting the SB-interior counter; relabelled `rect_split_sb_interior_tu_hits delta=N
  (over COMPARED streams only)`.

## Refusals lifted

| refusal | gate | result |
| --- | --- | --- |
| "a superblock-level HORZ/VERT strip with a split transform (per-unit rect prediction is not ported)" | `a_real_aomenc_stream_with_a_split_transform_superblock_strip_decodes_pixel_exact` | GREEN (was `#[ignore]`d RED in r1): 20/20 pixel-exact, 0 named refusals, SB-interior-TU delta 7 over compared streams |

Still refused (unchanged, not this round's): the below-16x16 `decode_leaf_rect` strip arms
(deferred(a below-16x16 strip gate — lane-sub8's territory)); the unreachable chroma shape
guard above (accepted, justified in place).

## Evidence

EVIDENCE: `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j3 -- split_transform filter_intra_on_a_horz refusal_inventory gate_coverage nz_map_ctx_offset --nocapture` | 3 aomenc gates + inventory/coverage/table tests on lane-rectsplit @ bf53652 | 9 passed, 0 failed, 0 ignored, 47.68s — gate (b) 20 pixel-exact / 0 refusals / SB-interior delta=7 (nonzero on seeds 57,60,61); gate (a) 20/20 delta=2; gate (c) 40/40 filter_intra_rect delta=3

## Film check (both films, 0.4s extracts)

```
ffmpeg -v error -t 0.4 -i "<Troy ...DECK.mkv>" -c:v copy -f obu troy.obu   # 418 bytes
ffmpeg -v error -t 0.4 -i "<Hunger Games ...UH.mkv>" -c:v copy -f obu hg.obu  # 1817 bytes
cargo run -p ec-av1 --example decode_probe -- troy.obu
REFUSED: unsupported: AV1 tile (a 32x32 partition type this decoder does not code (value=4))
cargo run -p ec-av1 --example decode_probe -- hg.obu
REFUSED: unsupported: AV1 tile (a partition below 8x8 (this decoder codes no leaf smaller than 8x8))
```

EVIDENCE: scratchpad `troy.obu` (418 B) / `hg.obu` (1817 B) | `ffmpeg -t 0.4 -c:v copy -f obu` then `decode_probe` on lane-rectsplit | Troy stops at "a 32x32 partition type ... (value=4)", Hunger Games at "a partition below 8x8" — neither is a refusal this lane owns, both upstream of every line touched here

disposition: deferred(lane-sub8's below-8x8 leaf; the 32x32 value=4 AB-partition arm) — outside this lane.
