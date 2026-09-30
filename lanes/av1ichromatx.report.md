# lane-av1ichromatx — the recorded 4:4:4 lossy divergence is ALREADY FIXED on main; the last block-level `tx_type` site is now closed with an assertion

Branch `lane-av1ichromatx` (worktree `~/.cache/wt/av1ichromatx`), base `8680e31d` (main).
Oracle `~/.cache/aom-oracle/build/{aomdec,aomenc}` (instrumented).
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1ichromatx`.

## 0. Headline: the ticket's premise does not reproduce

The ticket's recorded divergence (**777 / 640 / 554 wrong V samples on decode-order
frames 1/2/3** of `444_quad_leaf_tx_type.obu`) is **not present on current main**.
Measured on the unpatched tree, every frame is byte-identical to instrumented
`aomdec`'s `EC_AV1_FINAL_DUMP`:

```
$ EC_AV1_FINAL_DUMP=/tmp/cmpx/ours decode_probe crates/ec-av1/fixtures/444_quad_leaf_tx_type.obu
frame 0: len 49152 vs 49152 IDENTICAL
frame 1: len 49152 vs 49152 IDENTICAL
frame 2: len 49152 vs 49152 IDENTICAL
frame 3: len 49152 vs 49152 IDENTICAL
```

Per-frame wrong-sample counts, decode order: **f0 0, f1 0, f2 0, f3 0** (before AND
after this lane's change). Fixture confirmed first:

```
$ sha256sum crates/ec-av1/fixtures/444_quad_leaf_tx_type.obu
a06c9f7a0862252f6ab7ebdcec340d0602e730e38089365f14747db37f8e78d5   (27933 bytes)
```

It is a committed pin (`crates/ec-av1/src/stream.rs:21241`, fnv `0x85fa830b880090df`,
also `scripts/fixture-library.tsv:53`), so the ticket's "may already be a committed
pin — reuse it" is the case.

**Where 777/640/554 comes from:** it is the PRE-fix / mutation number recorded in
`lanes/av1444chr.report.md:77-80` ("reverting only the live arm's resolve … 777
bytes differ") and re-measured in `lanes/av1f9singleland.report.md`. The fix is
already an ancestor of main:

```
$ git merge-base --is-ancestor 6c33f204 HEAD && echo ANCESTOR   # four-unit arms, BOTH copies
ANCESTOR
$ git merge-base --is-ancestor 2142d2cf HEAD && echo ANCESTOR   # read_inter_rect_chroma sibling
ANCESTOR
```

## 1. Re-locating the three sites (by grep, not by line number)

`decode_inter_block` is `decode.rs:39235-44746` and carries TWO textually parallel
copies of the chroma arm chain (compound `40673-41297`, single-ref `42442-43105`).
Grep for every place a chroma read is handed the BLOCK-level `luma_tx_type`:

| # | site | line (single-ref / compound) | state on main |
|---|---|---|---|
| A | four-unit 4:4:4 arm, per-quadrant resolve | `41258` / `43006` | **already per-unit** (`covering_leaf_tx_type`, 6c33f204) |
| B | `read_inter_rect_chroma`, per-unit resolve | `29476` | **already per-unit** (2142d2cf) |
| B' | `read_inter_rect_chroma`, block-level FALLBACK | `29482` | **the one live block-level handoff** |
| C | whole-plane square chroma arm (`Some(luma_tx_type)`) | `41273`/`41292` / `43081`/`43100` | identity — see §3 |

So the ticket's "three sites" are the three site-KINDS (A, B/B', C); A and B were
fixed before this lane, B' and C are what remained.

## 2. Measuring each site against libaom's real cell

`av1_get_tx_type` (`blockd.h:1291-1296`) reads `xd->tx_type_map[blk_row << ss_y][blk_col << ss_x]`,
i.e. the unit's OWN cell. A luma leaf stamps its own top-left cell with its coded
type; a 64-class leaf additionally stamps every 16-px sub-cell
(`decodetxb.c:504-507`); an `all_zero` leaf stamps `DCT_DCT` at its own cell
(`decodetxb.c:199-203`). I modelled those three stamp rules in a temporary probe
and compared, per site, the block-level value against the cell the unit's own
position reads.

```
444_rect_strip_leaf_tx_type.obu   site1=8/2   site2=13/0  site3=20/0   leaf0=13/13  agree=24/0
w_sb64_1to4_cq24.obu (new)        site1=150/0 site2=1/0   site3=2/0    leaf0=1/1    agree=150/0
w_sb128_1to4_cq20.obu (new)       site1=150/0 site2=1/0   site3=4/0    leaf0=1/1    agree=150/0
w_sb64_1to4_cq30.obu (new)        site1=146/0 site2=1/0   site3=4/0    leaf0=1/1    agree=146/0
```

(read as `reads/differs-from-own-cell`)

* **A (four-unit arms)** — resolved per quadrant; `agree=0` disagreements between
  `covering_leaf_tx_type` and the true stamped cell across 470 resolves.
* **C (whole-plane square arm)** — 41 reads, **0 differ**. Its argument: that arm
  reads ONE chroma unit covering the whole plane, so its own cell is the block's
  MI (0,0), and `luma_tx_type` is `first_tx_type` = `leaves[0]`'s coded type.
  Measured `leaves[0]` at MI (0,0) in **15/15** vartx blocks, so C is an identity,
  not a defect. It is also vacuous to "fix": resolving `(0,0)` returns the same
  value.
* **B' (rect fallback)** — the only site that can still hand a block-level type to
  a chroma unit. It fires **4x** on the pinned rect witness, all on a MULTI-unit
  chroma plane. That is the shape where a block-level value could mistype a unit,
  so it needed closing rather than measuring.

## 3. Why B' is safe, and what closes it

Every observed B' firing has `leaves == 0`:

```
$ EC_TEMP_SITES=1 decode_probe crates/ec-av1/fixtures/444_rect_strip_leaf_tx_type.obu
TEMPFB multi=true leaves=0 rel_mi=(0, 0) luma=DctDct first=None   x2
TEMPFB multi=true leaves=0 rel_mi=(8, 0) luma=DctDct first=None   x2
TEMPNZ nz=0 / nz=1 / nz=13      (units that took the fallback DO carry coefficients)
```

`leaves == 0` means the block coded a SINGLE luma transform, whose own stamp
covers the whole plane, so `luma_tx_type` **is** that unit's cell — the fallback is
correct. The shape that would be wrong is a fallback on a plane while
`leaves` is NON-empty (some leaf exists, it just does not cover this unit's cell,
so the block-level value is a *different* leaf's type). Measured over the 87
committed fixtures plus 3 fresh streams (90 streams, decode_probe, debug build):
**0 occurrences**.

Closed with an assertion, not a comment (`decode.rs:29482`):

```rust
hit!(CHROMA_RECT_BLOCK_TX_FALLBACK);
if multi { hit!(CHROMA_RECT_BLOCK_TX_FALLBACK_MULTI); }
debug_assert!(
    leaf_tx_types.is_empty(),
    "lane-av1ichromatx: a multi-unit rect chroma plane fell back to the block-level \
     tx_type while {} luma leaves were coded -- the unit's own av1_get_tx_type cell \
     is then some other leaf's stamp, not luma_tx_type",
    leaf_tx_types.len()
);
```

## 4. Red-before (mutation) — the closure bites

Forcing the covering-leaf resolve to miss on a multi-unit plane
(`covering_leaf_tx_type(leaf_tx_types, unit_rel_mi).filter(|_| !multi)`):

```
$ cargo test -p ec-av1 --lib -- --exact stream::tests::a_multi_unit_rect_chroma_plane_...
lane-av1ichromatx: a multi-unit rect chroma plane fell back to the block-level tx_type
while 5 luma leaves were coded -- the unit's own av1_get_tx_type cell is then some
other leaf's stamp, not luma_tx_type
test result: FAILED. 0 passed; 1 failed
```

Reverted; green again: `4 block-level chroma fallbacks, 4 of them on a multi-unit plane`.

## 5. New gate (non-vacuous)

`stream::tests::a_multi_unit_rect_chroma_plane_only_falls_back_to_the_block_type_on_a_single_luma_unit`

Pins `fixtures/444_rect_strip_leaf_tx_type.obu` (26839 B, fnv `0xc77f310c036dff73`),
asserts `multi_fallback >= 2` — the number that says the guarded shape is REACHED,
so the assertion cannot go vacuous — and the same mutation above turns it red.
Measured: 4 fallbacks, 4 of them multi-unit.

## 6. Neighbours re-quoted (no SKIP, all compared against instrumented aomdec)

```
a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type   ok  (96 quad-resolved units, 8 differed)
the_pinned_444_quadrant_witness_is_the_single_reference_arms_...              ok  (88/96 single-ref, 8 changed an answer)
a_pinned_444_rect_inter_stream_resolves_each_chroma_unit_from_its_own_luma_leaf ok (4 resolved, 2 differed)
a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact              ok
a_lossless_444_defaultp_inter_strip_stream_decodes_byte_exact                ok
a_multi_unit_rect_chroma_plane_only_falls_back_... (new)                     ok  (4/4)
test result: ok. 6 passed; 0 failed; 0 ignored
```

Per-frame byte comparison against `EC_AV1_FINAL_DUMP`, before and after the change —
identical, all EXACT:

| stream | frames | result |
|---|---|---|
| `444_quad_leaf_tx_type.obu` | 4 | f0-f3 EXACT |
| `444_rect_strip_leaf_tx_type.obu` | 3 | f0-f2 EXACT |
| `ll444_defaultp_inter_strip.obu` | 7 | f0-f6 EXACT |
| `ll444_minp64_inter.obu` | 6 | f0-f5 EXACT |
| `av1_192x128_8bit_intra64_in_inter.obu` (4:2:0 control) | 6 | f0-f5 EXACT |
| `w_sb64_1to4_cq24.obu` (new, VPS) | 6 | f0-f5 EXACT |
| `w_sb128_1to4_cq20.obu` (new, VPS) | 6 | f0-f5 EXACT |
| `w_sb64_1to4_cq30.obu` (new, VPS) | 6 | f0-f5 EXACT |

## 7. tx_type evidence — the 2 `DCT_DCT` chroma units on V, frame 1

`EC_TRACE_TXTYPE=1`, per decode-order frame, chroma units only:

```
frame 1: 32 chroma units, tx_type=DctDct at U(0,32) V(0,32) U(64,96) V(64,96)
         inherited DctDct -> Some(DctDct) on all four
frame 2: 32 chroma units, tx_type=DctDct at U(64,96) V(64,96)
frame 3: 32 chroma units, tx_type=DctDct at U(64,96) V(64,96)
```

So frame 1 has **exactly 2 `DCT_DCT` chroma units on V** — `V(0,32)` and `V(64,96)` —
and they are exactly the quadrants that sit over a luma leaf which coded NOTHING:
the value comes from `covering_leaf_tx_type`, i.e. from that leaf's `all_zero`
`DCT_DCT` stamp (`decodetxb.c:199-203`), while the block-level type on those blocks
is `Idtx`. That is the ticket's H2 mechanism, already landed; 4 + 2 + 2 = 8 units,
which is precisely the `diff = 8` the existing gate asserts.

## 8. What remains

* Nothing in this class is open in `decode_inter_block`: A and B resolve per unit,
  C is an identity by construction (measured 15/15), B' is closed by assertion.
* **4:2:2 is out of scope and provably so**: `stream.rs:1766` refuses
  `subsampling_x != subsampling_y` by name at the sequence header, so the
  `invalid_ss_plane` (8x32 luma → 4x8 chroma) path inside
  `read_inter_rect_chroma`, whose units sit at luma MI cells a single 8x32 leaf
  stamps only at (0,0), can never be reached.
* `covering_leaf_tx_type` models the leaf's own stamp, not libaom's
  never-cleared map. For every shape reachable today (leaf tops align with the
  chroma-unit grid, or a 64-class leaf stamps the whole area) the two coincide —
  measured 0 disagreements in 470 resolves. A stream where a chroma unit's cell
  lands strictly INSIDE a sub-64 luma leaf would expose the difference; no such
  shape is reachable with the current arm conditions.
