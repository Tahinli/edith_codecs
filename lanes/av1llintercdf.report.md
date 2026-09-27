# lane-av1llintercdf: the t6 frame-1 INTER all_zero divergence named (and the one-site fix)

## Ticket

Deferred item 1 of `lanes/av1llkf.report.md`: the 6-frame default-partition
lossless 4:4:4 testsrc2 stream (t6) reads frame 0 fully exact, then diverges at
frame 1 (INTER), luma `all_zero` read #2317 — "same ctx row (1) and same decoded
value as the oracle but different CDF contents (ours `cdf0=5892` vs aom
`27244`)", suspected adaptation-history/row-selection desync. Name the divergent
symbol and its reader, decide missed-update vs applied-twice, fix only if one
site.

## The llkf reading is refuted — the CDF rows were EQUAL

Both trace printers were misread as the same quantity:

- ours (`decode.rs:6811-6823`): `cdf=[a, 32768, n]` is the row snapshot taken
  BEFORE `dec.symbol` — the PRE-update row, in OUR storage, where
  `ours_v = 32768 - aom_v` (complement).
- oracle (`decodetxb.c:161`): `cdf0=` is printed AFTER `aom_read_symbol` — the
  POST-update value in aom storage.

Replaying frame 0 read-by-read (rate-4 adaptation, val=0 up / val=1 down in our
convention) matches every printed pair: at the divergent read aom's pre-state
was `26876` (aom storage) = `5892` (ours) — the rows were IDENTICAL, and both
sides adapted that read symmetrically. So the answer to "missed update or
applied twice" is **neither**: there was no adaptation desync at all; the rng
mismatch at read #2316 (0-indexed; the report's #2317) came from symbols read
EARLIER in the same block's mode info, which the all_zero-only ladder cannot
see.

## The divergent symbol and reader (measured, t6, EC_ECDUMP_IN + EC_TRACE_COEFF
+ EC_TRACE_MODE_STEP on both sides)

Between the last coefficient read of frame 1's first superblock row and read
#2316, both sides decode the INTRA 8x4 leaf at mi=(0,2) — an intra leaf inside
an INTER frame, through `read_intra_block_mode_info` (decodemv.c:1065). The
states part inside its `uv_mode` read:

- ours pre-`use_filter_intra` rng 60904, oracle 58668; ours consumed 100 bits
  across the block's mode info, the oracle 98 (+2).
- ours read `uv_mode` off the **14-symbol `uv_mode_cfl`** row
  (`decode.rs:35703`, then unconditional); the oracle reads it off the
  **13-symbol `uv_mode_no_cfl`** row, because `av1_is_cfl_allowed` (blockd.h)
  returns 0 for a lossless block whose chroma plane block is not a single
  TX_4X4 — an 8x4 leaf at 4:4:4 has an 8x4 chroma plane block.

Reader: `decode_intra_sub8_leaf`'s uv read (`decode.rs:35703`), the one
`uv_mode` site that bypassed the canonical `cfl_allowed_px` predicate every
other uv read site uses (`decode.rs:9791, 11206, 15610, 18853, 34062, 38675`).
The block comment there stated only the non-lossless term.

The neighbouring 4x4 leaf (mi=(0,1)) kept matching by construction: at 4:4:4
lossless a 4x4 leaf's chroma block IS a single TX_4X4, so CFL stays allowed and
the 14-symbol row is correct there — which is why frame 1 synced for 2316 reads
and 5 blocks before diverging on the first 8x4.

## Change (decode.rs, one site)

`decode_intra_sub8_leaf`'s uv read now goes through
`cfl_allowed_px(bw, bh, fctx)` — the canonical predicate, which already
implements the lossless term (chroma plane block must be a single TX_4X4).
4:2:0 is arithmetically identical: every sub-8 shape's chroma block is
4x4-or-smaller under 4:2:0, so the predicate stays true and the row choice is
unchanged; the 4:2:0 lossless gates (below) are green.

## Verification (measured, this tree @ HEAD + this change)

- Entropy: pairs #2316 and #2317 (the whole 8x4 leaf's luma) now match the
  oracle's (value, rng) exactly, and the ECIN bits delta returns to the
  constant +15 header offset through the block.
- Byte witness: t6 frame 0 is byte-exact vs fresh `aomdec --rawvideo` (36864
  bytes; sha256 of the full 6-frame decode differs — see the deferred item).
- Gates: `cargo test -p ec-av1 --lib a_lossless` → 7 passed, 0 failed (both
  4:2:0 lossless libaom gates, the 444 min-partition-64 inter gate, the sb128
  pair, the 16x4 pair).
- `cargo check -p ec-av1 --all-targets` (target dir
  `$HOME/.cache/cargo-target-av1llintercdf`): 0 warnings, 0 errors.

## Named, not chased (spoken)

1. t6 frames 1..5 still differ (first diff frame-1 byte 65): a SECOND site in
   the same leaf, independent of the alphabet fix. At the still-shared state
   rng=51429 after the 8x4 leaf's luma, the oracle reads the leaf's chroma as
   FOUR TX_4X4 units (plane 1 bc=0,1 ctx=11 then plane 2 bc=0,1 — lossless
   forces per-unit TX_4X4), while `decode_intra_sub8_leaf`'s
   `chroma_444 && bw != bh` arm (`decode.rs:36104-36139`) reads the chroma
   plane as ONE `TX_8X4`/`TX_4X8` rect unit per plane (`read_coeffs_rect`,
   skip ctx fixed at above+left=1 where the oracle derives 11≡4). The fix is
   the b3f815c8-style per-unit lossless 444 chroma walk (unit decomposition +
   per-unit `get_txb_ctx` + dropped tx_type), i.e. a shape port of its own —
   not a one-site fix, so per the ticket it is stopped-with-site, deferred(
   the sub-8 4:4:4 lossless chroma walk; unblock: port the
   `decode_leaf_rect8`/8x8-leaf per-unit walk into the `chroma_444 && bw != bh`
   arm).
2. Instrument gap, no decode impact: the rect coefficient reader
   (`read_coeffs_rect`, `decode.rs:7056`) prints `EC_COEFF_STEP` with a
   hardcoded `plane=0` and no `EC_ECDUMP_IN` line, so rect-path reads are
   invisible to the (value, rng, bitpos) pairing used here. Worth one rung of
   parity next time that path is debugged.
