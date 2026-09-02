# lane-intra64split r1 -- the un-split TX_64X32/TX_32X64 intra strip in an inter frame

## Premise correction (MEASURED, not the charter's)

The charter chartered a *split* (tx_depth >= 1) 64-level intra strip. That is NOT
what the refusal fires on. Commands that produced this:

* `grep -rn "intra strip whose transform unit" crates/ec-av1/src/` -> one site,
  `decode.rs:5834`, inside `decode_rect_split`'s `luma_rect` shape match.
* Reading every `decode_rect_split` call site: the depth-derived `depth_to_tx_wh`
  can never return a 64-axis unit (`sub_tx_size_map[TX_64X32] == TX_32X32`).
  The only caller that passes a 64-axis `tx_w`/`tx_h` is
  `decode_intra_rect_in_inter` (decode.rs:6252) **at depth 0**, where
  `(tx_w, tx_h) == (bw, bh)`. Its depth != 0 arm refuses earlier by another
  name ("a split (nonzero tx_depth) transform on an intra HORZ/VERT strip in an
  inter frame" -- lane-intrasplit's surface, left refused and untouched here).
* Film probe confirms it: the refusal moved after the fix (below).

So the lifted shape is the 64-level intra HORZ/VERT strip **on the inter path**,
tx_depth 0, luma = one TX_64X32/TX_32X64.

## What changed

* `crates/ec-av1/src/decode.rs:1961..` -- new `RECT64_CORNER_TU_HITS` counter +
  `rect64_corner_tu_hits()`.
* `crates/ec-av1/src/decode.rs:5834..` -- `luma_rect` match gains
  `(64, 32) | (32, 64) => None` plus `luma_64corner`; the refusal's `_` catch-all
  stays as the shape guard for the next caller (the match must be exhaustive; the
  string is parametrised over shapes, so it stays in `refusal_inventory.rs` --
  DEVIATION from "drop the refusal + inventory line", reason: removing it would
  mean panicking on an unknown shape, exactly what the chroma twin above it
  documents).
* `crates/ec-av1/src/decode.rs:5900..` -- the new TU arm: `read_coeffs` on
  `TxbSet::Luma64` / `default_scan(TX32)`, txb_skip ctx 0 (the unit covers its
  whole block, `get_txb_ctx`), `TxType::DctDct` (`av1_get_ext_tx_set_type` ->
  EXT_TX_SET_DCTONLY at square-up TX_64X64), `Some((tx_w, tx_h))` so
  `av1_nz_map_ctx_offset` is indexed by the RAW rect shape, then the 32x32
  corner copied into the `tx_w * tx_h` grid before
  `dequant_and_inverse_typed_wh`. Byte for byte `decode_block_rect64`'s own
  depth-0 key-frame read (decode.rs:8069..).
* `crates/ec-av1/src/stream.rs:74..` -- `rect64_corner_tu_hits()` accessor.
* `crates/ec-av1/src/stream.rs:4275..` -- gate
  `a_real_aomenc_inter_sequence_with_a_64_level_intra_rect_strip_decodes_pixel_exact`
  (+ `_10bit`): 256x192 (four by three WHOLE superblocks -- the 96x96 sibling
  gate could never offer a 64-level partition), real aomenc,
  `--enable-rect-partitions=1 --min-partition-size=32 --max-partition-size=64
  --enable-tx-size-search=0`, 40 attempts sweeping cpu-used 0..4 and cq
  30/20/12/45, every decode-order frame compared against ffmpeg on all three
  planes, hard assert `rect64_corner_tu_hits > 0`, refusals collected and a
  decode error asserted to contain "unsupported" (never a SKIP).
  DEVIATION from the charter's `--enable-tx-size-search=1`: with it on, this
  path stops at lane-intrasplit's split refusal before reaching the shape, and
  the shape this lane lifts is the depth-0 one.

## EVIDENCE

EVIDENCE: /tmp/claude-1000/-home-tahinli-Documents-Code-Rust-edith-codecs/b6d8a07f-96a4-4bbb-b378-af9ae25cf7c9/scratchpad/i64/hg300.obu | `ffmpeg -ss 300 -t 2 -c:v copy -f obu` off the 4K 10-bit AV1 mkv, then `decode_probe` under `systemd-run --scope -p MemoryMax=6G` | stop refusal moved from "a split intra strip whose transform unit is 32x64" (census4 hunger4.tsv, start_s 300 and 3000) to "a split transform on a 1:4 inter strip with a 64-px axis"; 206 frame headers parsed, counters `rect4_32: horz=104 vert=172 coded=238`, `inter_rect: 32x64=2`

## Open residue

* The gate's own result: **pending**. fix-now for r2 -- the r1 run
  (both bit depths, 40 attempts) was killed by the harness at 65 minutes before
  printing anything (it was piped through `grep | tail`, so nothing was flushed).
  Re-run it detached into a log file. Whether aomenc will emit a 64-level intra
  strip in an inter frame at 256x192 is UNPROVEN; the sibling gate's r2 note says
  it could not at 96x96 (cropped superblocks) -- the film proves the shape is
  real, so if 40 attempts miss, widen the frame/content rather than weaken the
  assert.
* Next film gap on the same segment: "a split transform on a 1:4 inter strip
  with a 64-px axis" -- deferred(another lane's surface).
