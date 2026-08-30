VERDICT: PARTIAL -- decode_block_rect64 built and wired (green, committed,
c34be7c); the gate reaches it and fires `sb_rect_hits() > 0` live against a
real aomenc stream, but the decoded pixels do NOT yet match ffmpeg's own
decode -- a real mismatch, not a refusal. Gate test is uncommitted (red);
next round's first move is to bisect it.

## What landed (commit c34be7c, compiles clean, no regression)
- `decode_block_rect64(bw, bh)` in `crates/ec-av1/src/decode.rs`, right after
  `decode_block_rect`: mode via `read_intra_mode_rect` (unchanged, its
  `filter_intra_size_class_rect` already returns `None` for `(64,32)`/
  `(32,64)` since `av1_filter_intra_allowed_bsize` caps at 32 on both axes,
  so no extra work was needed there); `tx_size_cat2` depth read gated on
  `tx_select`, `depth != 0` refused by name (per-unit rect prediction not
  ported, matching `decode_block_rect`'s own scope limit); LUMA reads a
  real 32x32 corner via `read_coeffs` with `TxbSet::Luma64` + a locally
  computed `default_scan(TX32)` (matching what `decode_block`'s own 64x64
  corner case uses), embeds it top-left of a zeroed `bw*bh` grid (mirrors
  `read_plane`'s `tx_side != side` branch, generalized to non-square);
  CHROMA reads a real, untruncated `bw/2 x bh/2` transform via
  `read_coeffs_rect` with `TxbSet::ChromaRect32x16` (the r1-landed table)
  and `SCAN_32X16`/`SCAN_16X32` depending on orientation, with
  `default_tx_type = TxType::DctDct` forced (per r1's derivation: chroma at
  this size resolves to TX_32X32, `av1_get_ext_tx_set_type`'s `>= TX_32X32`
  rule forces DCT_DCT regardless of mode -- the same rule `read_plane`
  already applies via its own `side >= 32` check).
- `PARTITION_HORZ`/`PARTITION_VERT` wired into the SB-level `match part` at
  decode.rs (was the `_ =>` catchall): two `decode_block_rect64` calls at
  `at`/`(at.0+2, at.1)` (HORZ, two 64x32 strips) or `at`/`(at.0, at.1+2)`
  (VERT, two 32x64 strips), mirroring the already-landed 32x32-level
  pattern exactly.
- New `SB_RECT_HITS` thread-local + `sb_rect_hits()` accessor
  (decode.rs), same pattern as `RECT_PARTITION_HITS`/`RECT_COEFF_HITS`.
- `cargo check -p ec-av1`: clean. `cargo test -p ec-av1 --lib round_trip`:
  16/16 passed, no regression (`EC_AV1_REQUIRE_AOMENC=1`).

## The gate (uncommitted, RED -- pixel mismatch, not a refusal)
`a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact`
in `crates/ec-av1/src/stream.rs` (end of `mod tests`). Recipe, tuned live
this round from the charter's starting point:
- charter's own flags (`--min/max-partition-size=32/64`, rect on/AB off/
  1to4 off, restoration/palette/deltaq/filter-intra/cfl-intra/intrabc off)
  PLUS `--enable-tx-size-search=0` (missing from the charter's recipe --
  without it every HORZ/VERT strip picks a split transform and hits this
  function's own `depth != 0` refusal, never reaching the coefficient
  reader at all; same fix `decode_block_rect`'s own gate needed, r1's
  report just didn't carry it forward to the 64-level recipe).
- **Dead end, recorded so the next round doesn't re-spend budget**: the
  charter's "gradients blended with testsrc2" source (`blend=all_mode=
  average` lavfi filter) made `allow_screen_content_tools` fire on nearly
  every attempt (screen-content strips are refused by
  `read_intra_mode_rect` unconditionally) and, worse, made aomenc pick an
  SB-level AB partition (`partition_w64` value 7, VERT_B) on almost every
  remaining attempt *despite* `--enable-ab-partitions=0` -- a real,
  separate aomenc quirk (AB-at-64 is not gated by that flag the way AB
  below 64 is; unconfirmed root cause, not investigated further this
  round) -- 40/40 attempts refused, zero decodes. Plain `gradients_source`
  alone (no blend) reliably avoids both and reaches real HORZ/VERT single-
  TU blocks; the gate now uses that.
- With both fixes the gate reaches `decode_block_rect64` and increments
  `sb_rect_hits`, but the one attempt run this round (`EC_SBPART_GATE_
  ATTEMPTS=1`, seed 42, 192x128 single frame) MISMATCHES ffmpeg's own
  decode on luma starting partway through row 0 -- not a desync (the
  stream still parses/finishes, `matched` would be the only counter that
  stays 0), a real wrong-pixel bug in the new function. Not bisected this
  round (turn budget) -- **next round's first move**: dump the mismatching
  stream (`EC_AV1_GATE_DUMP`, same mechanism the masked-compound gate
  uses) and range-ladder the luma corner read / chroma read / the
  `dequant_and_inverse_typed_wh` call against a real aomdec EC_TRACE,
  the same method that closed `decode_block_rect`'s own r3-r5 desync
  (lane-rectwire history, memory `[[Interintra decode landed]]`'s sibling
  entries) -- suspects in order of likelihood: (a) the corner-embed loop's
  stride (`luma_levels[row*bw..][..32]`) vs whatever `dequant_and_
  inverse_typed_wh`'s `w,h` ordering actually expects for a `64x32` vs
  `32x64` grid (an axis swap here would look exactly like this symptom --
  right at first, wrong once the strip's true width diverges from 32); (b)
  `default_scan(TX32)` vs the packed/stored `scan32` other call sites
  reuse -- should be identical (pure function of `TX32`) but not directly
  diffed against the working square-64x64 path's own copy this round.

## Refusal strings removed
None removed outright -- the SB-level catchall
("a superblock-level partition type other than NONE or SPLIT...") still
fires for AB/4-way/split-tx cases, verbatim, unchanged in
`decode.rs`/`refusal_inventory.rs`. For a genuine single-TU HORZ/VERT it no
longer fires (replaced by the new function), but that path is proven WRONG
(pixel mismatch) by the gate above, not proven right -- so functionally no
capability should be considered landed yet; the gate is red and
uncommitted for exactly that reason.

## What the next round needs to do
1. Bisect the mismatch (see suspects above) -- likely a small, mechanical
   bug given `decode_block_rect`'s own precedent (the 32x16/16x32 sibling
   needed 5 rounds for something this shape: a spurious/missing symbol
   read, not deep coefficient-context math).
2. Once pixel-exact, re-run the gate with the full `n_attempts=40` default
   and hard-assert `sb_rect_hits() > 0` (already written that way, just
   currently failing before reaching that assert).
3. Only then move to stage 2 (32x32-level `part32` values beyond NONE/
   SPLIT/HORZ/VERT) and the inter path's two refusals, per the charter.

## Hard rules followed
Worked only in this worktree; `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`
every build; foreground `nice -n 19 cargo ... -j4`; `EC_AV1_REQUIRE_AOMENC=1`
on every test run; no push, no merge, no touch to `main`, no other worktree
touched. The one green step (`decode_block_rect64` + wiring, no regression)
is committed; the gate test is left uncommitted because it is red.
