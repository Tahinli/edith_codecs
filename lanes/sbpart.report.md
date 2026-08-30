VERDICT: PARTIAL -- one green, inert building-block commit landed; the four
named refusals this lane owns are all still refused. Recon-heavy round; ran
out of turn budget before the actual decode wiring.

## What landed (commit 0e04b79)
- `crates/ec-av1/src/cdf.rs`: `EOB_PT_512_CHROMA`, the true 512-position
  end-of-block CDF for a chroma transform, extracted from libaom's
  `av1_default_eob_multi512_cdfs[*][1][*]` (`token_cdfs.h:874-901`). Unlike
  every other `EOB_PT_*` table in this crate, libaom's default is the
  identical flat distribution at all four q-contexts, so this is one
  constant, no `_Q0`/`_Q1`/`_Q3` siblings.
- `crates/ec-av1/src/cdf_state.rs`: `TxbSet::ChromaRect32x16` (+ field
  `eob_pt_512_chroma`, reset wiring, `pick()`/defaults wiring, `txb()`
  resolve arm). Shares every table `Chroma32` does except `eob_pt`.
- `cargo check -p ec-av1`: clean. `cargo test -p ec-av1 --lib round_trip`:
  16/16 passed (no regression). Neither is wired into any decode path yet
  (dead code, by design -- next round's first move).

## The gate recipe that reproduces the SB-level refusal (verified live)
`~/.cache/aom-oracle/build/aomenc --codec=av1 --passes=1 --end-usage=q
--cq-level=45 --cpu-used=0 --threads=1 --row-mt=0 --sb-size=64
--enable-rect-partitions=1 --enable-ab-partitions=0
--enable-1to4-partitions=0 --min-partition-size=32 --max-partition-size=64
--enable-restoration=0 --enable-palette=0 --deltaq-mode=0
--enable-filter-intra=0 --enable-cfl-intra=0 --enable-intrabc=0 --obu` on a
`gradients` source (192x128, blended with `testsrc2` so it isn't flat) hits
`TRACE partition_w64 ... value=2` (VERT) and refuses by name
("a superblock-level partition type other than NONE or SPLIT"). `min`/`max`
-partition-size=32/64 is the key: it keeps the RD search from ever
recursing below 32x32, so every SB-level decision is genuinely NONE/SPLIT/
HORZ/VERT rather than diving into unrelated 16x16/8x8 territory (which is
what a plain default recipe hits first -- see dead ends below).

## Why this took the whole budget: the spec fact this lane's "mostly
extending an existing recursion" framing undersold
`decode_block_rect` (the existing HORZ/VERT strip decoder, 32x16/16x32) has
no truncation logic at all, because 32 and 16 both already fit inside the
32-coefficient cap. A 64x32/32x64 strip does NOT fit: its LUMA plane needs
the same corner-truncation `decode_block` already does for a plain 64x64
square (`logical_tx`/`coeff_tx_side` split, `read_plane`'s `tx_side != side`
branch) -- genuinely new for a *rectangular* block, `read_plane` is
square-only. Worked out from libaom's `get_txsize_entropy_ctx` formula
(`(txsize_sqr_map[tx] + txsize_sqr_up_map[tx] + 1) >> 1`,
`common_data.h:302-341`) which table each axis actually resolves to:
- LUMA (TX_64X32/TX_32X64): sqr_map=TX_32X32, sqr_up_map=TX_64X64 ->
  ctx resolves to **TX_64X64** = the already-landed `TxbSet::Luma64` +
  `SCAN_32` (used exactly as the plain-64x64-square case already does).
  No new table needed here.
- CHROMA (TX_32X16/TX_16X32, real untruncated transform, no corner-crop):
  sqr_map=TX_16X16, sqr_up_map=TX_32X32 -> ctx resolves to **TX_32X32** =
  `Chroma32`'s tables, except the true 512-position `eob_pt` -- this is what
  needed the new `ChromaRect32x16` variant landed this round.

## What the next round needs to build (all groundwork above is done)
1. A `decode_block_rect64(bw, bh)` (or generalize `decode_block_rect`) that:
   mode-reads via `read_intra_mode_rect` (already bw/bh-generic, unverified
   above 32x16 -- check its angle-delta/filter-intra ranges don't assume
   `bw,bh <= 32`), then for luma reads a real `32x32` corner via
   `read_coeffs` (not `_rect`) with `TxbSet::Luma64`/`SCAN_32`, embeds it
   top-left of a `bw*bh` zero grid (mirror `read_plane`'s `tx_side != side`
   branch, generalized to non-square), and `dequant_and_inverse_typed_wh`
   the whole `bw x bh` grid; for chroma reads the real `bw/2 x bh/2` corner
   via `read_coeffs_rect` with `TxbSet::ChromaRect32x16` (this round's new
   variant) and the right `SCAN_32X16`/`SCAN_16X32` table (already exist).
2. Wire `PARTITION_HORZ`/`PARTITION_VERT` into the `match part` at
   decode.rs:5292's `_ =>` arm (intra tile path, 64x64 level) the same way
   `PARTITION_HORZ`/`VERT` are already wired at the 32x32 level just above
   (lines 5217-5283) -- two calls to the new function at `at`/`(at.0+2,
   at.1)` or `(at.0, at.1+2)` (mi units, `SUB_MI`-scaled for the 64-level).
3. Gate it with the recipe above (`EC_AV1_REQUIRE_AOMENC=1`, attempt loop,
   hard-assert a new `SB_RECT_HITS` thread-local fired, matched > 0)
   before touching the inter path or the 32x32-level part of the charter.
4. Only once intra HORZ/VERT is pixel-exact does stage 2 (32x32 `part32`
   values beyond NONE/SPLIT/HORZ/VERT the gate turns up), stage 3 (inter
   path, same two levels), and stage 4 (AB/4-way at these levels) become
   reachable -- none of those were attempted this round.

## Refusal strings removed
None. All four refusals this lane owns are still in place, verbatim, in
`refusal_inventory.rs` and `decode.rs` -- no capability landed yet that
would make any of them stop firing.

## Dead ends (worth recording so the next round doesn't re-spend budget)
- A plain default-ish recipe (`--codec=av1 --cpu-used=0 --threads=1
  --row-mt=0 --sb-size=64` with nothing else off) hits loop restoration or
  palette or a 16x16-level AB partition first, all lanes this one doesn't
  own -- `--enable-restoration=0 --enable-palette=0 --deltaq-mode=0` plus
  `--min-partition-size=32 --max-partition-size=64` is what isolates the
  SB-level decision cleanly.
- `testsrc2` alone at 192x128/cpu-used=0 without the min/max-partition-size
  clamp recurses straight to 8x8 leaves (too much local detail for the RD
  search to stop at 32x32) -- blended with a `gradients` source it stays
  coarse enough to resolve at 64/32.

## Hard rules followed
Worked only in this worktree; `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`
every build; no push, no merge, no touch to `main`, no other worktree
touched. `EC_AV1_REQUIRE_AOMENC=1` on the test run above.
