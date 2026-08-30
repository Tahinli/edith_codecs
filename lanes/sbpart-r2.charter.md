# lane-sbpart r2 — build decode_block_rect64, wire HORZ/VERT at 64x64

At a2bc6dc. Read `lanes/sbpart.report.md` — it is a typing plan with the
formulas already worked out. Do not re-derive it.

**Correction to the r1 charter, on the record:** it said this lane was "mostly
extending an existing recursion, not new theory". That was wrong, and r1's
report says why: `decode_block_rect` (32x16 / 16x32) has no truncation logic
because 32 and 16 both fit inside the 32-coefficient cap. A 64x32 / 32x64 strip
does not — its luma plane needs the corner truncation `decode_block` does for a
plain 64x64 square, and `read_plane`'s `tx_side != side` branch is square-only.
That is genuinely new work for a rectangular block, and r1 was right to spend
its round establishing it rather than typing something that could not work.

## Already landed (0e04b79, green, inert by design)
- `cdf.rs`: `EOB_PT_512_CHROMA`, the true 512-position chroma end-of-block CDF
  from libaom's `av1_default_eob_multi512_cdfs[*][1][*]` (token_cdfs.h:874-901).
  libaom's default is the same flat distribution at all four q-contexts, so it
  is one constant with no `_Q0`/`_Q1`/`_Q3` siblings.
- `cdf_state.rs`: `TxbSet::ChromaRect32x16` with its field, reset, `pick()` /
  defaults wiring and `txb()` arm. Shares everything `Chroma32` does except
  `eob_pt`.

r1 also resolved which entropy context each axis lands on, via
`get_txsize_entropy_ctx` = `(txsize_sqr_map[tx] + txsize_sqr_up_map[tx] + 1) >> 1`
(common_data.h:302-341): LUMA TX_64X32/TX_32X64 resolves to **TX_64X64**, so the
already-landed `TxbSet::Luma64` + `SCAN_32` are correct and no new table is
needed; CHROMA TX_32X16/TX_16X32 resolves to **TX_32X32**, which is `Chroma32`'s
tables except the eob — hence this round's new variant.

## Build, in this order — COMMIT AT EVERY GREEN STEP
1. `decode_block_rect64(bw, bh)` (or generalize `decode_block_rect`):
   - mode via `read_intra_mode_rect`, which is already bw/bh-generic but
     unverified above 32x16 — check its angle-delta and filter-intra ranges do
     not assume `bw, bh <= 32`;
   - luma: read a real 32x32 corner with `read_coeffs` (not `_rect`) using
     `TxbSet::Luma64` / `SCAN_32`, embed it top-left of a `bw*bh` zero grid
     (mirror `read_plane`'s `tx_side != side` branch, generalized to
     non-square), then `dequant_and_inverse_typed_wh` over the whole grid;
   - chroma: read the real `bw/2 x bh/2` corner with `read_coeffs_rect` using
     `TxbSet::ChromaRect32x16` and the matching `SCAN_32X16` / `SCAN_16X32`
     (both already exist).
2. Wire `PARTITION_HORZ` / `PARTITION_VERT` into the `_ =>` arm of the `match`
   at decode.rs ~5292 (intra tile path, 64x64 level), exactly the way HORZ/VERT
   are already wired at the 32x32 level just above it — two calls to the new
   function at `at` and `(at.0 + 2, at.1)` or `(at.0, at.1 + 2)`, mi units,
   `SUB_MI`-scaled for the 64 level.
3. The gate, with r1's verified recipe (it reproduces the refusal live):
   `aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=45 --cpu-used=0
   --threads=1 --row-mt=0 --sb-size=64 --enable-rect-partitions=1
   --enable-ab-partitions=0 --enable-1to4-partitions=0 --min-partition-size=32
   --max-partition-size=64 --enable-restoration=0 --enable-palette=0
   --deltaq-mode=0 --enable-filter-intra=0 --enable-cfl-intra=0
   --enable-intrabc=0 --obu` on a 192x128 `gradients` source blended with
   `testsrc2` so it is not flat. `min/max-partition-size=32/64` is the key: it
   stops the RD search recursing below 32x32, so every SB-level decision is
   genuinely NONE/SPLIT/HORZ/VERT. `EC_AV1_REQUIRE_AOMENC=1`, attempt loop
   requiring at least one decode, HARD-asserted firing count.
4. Then the 32x32-level `part32` values, then the inter path's two refusals.

## The rule this batch has now paid for twice
Do NOT remove a refusal in a commit that does not also gate it. A green suite
proves the removal broke nothing that already ran — it cannot prove the
newly-allowed path, because until the gate exists nothing takes it. Two sibling
lanes lifted refusals on a green suite this batch; one of them turned out to
have a firing counter that was never incremented, so its own assert was blind.
If you cannot gate it this round, leave the refusal in place and say so.

Merge note: main is at 93d9510 (multi-tile decode landed, gated). Report every
refusal string you remove, verbatim, for `refusal_inventory.rs`.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms. Sibling
worktrees have live agents — never build in or edit them. Never push, never
merge into `main`. 75-turn cap, does not reset: COMMIT AT EVERY GREEN STEP.
End with `lanes/sbpart-r2.report.md`, VERDICT on line 1.
