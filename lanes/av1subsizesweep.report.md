# lane/av1subsizesweep — census of every chroma-plane walk a BLOCK_INVALID subsize can reach

Base `main` = `e45cc748` ("Merge lane/av1unwritten: refuse the subsize libaom calls corrupt").
Worktree `~/.cache/wt/av1subsizesweep`, own `CARGO_TARGET_DIR=~/.cache/tgt/subsizesweep`.

## What the merged lane already covered (not redone)

`refuse_invalid_subsize` (`decode.rs:21489`) with `PLANE_BLOCK_INVALID_422` /
`PLANE_BLOCK_INVALID_440` transcribed cell-for-cell from `av1_ss_size_lookup`
(`common_data.c`), `partition_subsize_dims` as libaom's `get_partition_subsize`
reduced to dimensions, **11 guard call sites**, the structural test
`every_partition_symbol_resolution_is_guarded_before_it_dispatches`, and the
`EC_AV1_SUBSIZE_GUARD_TRACE` / `EC_AV1_SUBSIZE_ARM_TRACE` coverage artefacts.

Its named residue: `decode_rect4_16_intrabc`.

## The hole this lane found: the key-frame 16-level FRAME-EDGE arm

`decode.rs:37034-37067` (pre-fix numbering) is the **one** partition read in the
file that dispatches on its own symbol instead of resolving it into a `partN` the
level's guard covers.

| | reads | resolves to | guarded? |
|---|---|---|---|
| 128-root edge (`35551`/`35559`) | `symbol_fixed(&gather(..))` | `PARTITION_SPLIT`/`HORZ`/`VERT` | yes — `refuse_invalid_subsize((128,128), part128)` at `35588` |
| 64-level edge (`36239`/`36256`) | same | same | yes — `(64,64)` guard at `36278` |
| 32-level edge (`36363`/`36383`) | same | same | yes — `(32,32)` guard at `36406` |
| **16-level edge (kf)** | same | **`edge_split`, straight into `decode_leaf_rect`** | **NO — nothing on the path** |
| 16-level edge (inter, `54767`/`54780`) | same | `part16` | yes — `(16,16)` guard at `54799` |

With `has_cols16 == false` the strip is `BLOCK_8X16`, whose `(1,0)` chroma plane
block is `BLOCK_INVALID` (`av1_ss_size_lookup`, `common_data.c:20`). At 4:2:2 that
is a subsize libaom refuses at `decodeframe.c:1456`, and this decoder walked it
into `decode_leaf_rect` anyway. With `has_cols16 == true` the strip is `16x8` —
`BLOCK_8X16`'s mirror, whose `(1,0)` cell is `BLOCK_8X8` — and **must** keep
decoding.

Why the merged lane's structural test could not see it: the scan keys on
`dec.symbol(` (line 56698), and this arm reads `dec.symbol_fixed(&gather(..))`.
Every other edge read is either invisible for the same reason or resolves into a
guarded `partN`; only this one both dispatches directly *and* carries an
INVALID shape.

**Fix** (`decode.rs:37063-37109`): the guard sits on the arm's own path, named by
LEVEL not by a hand-picked shape — the gathered bit resolves to
`PARTITION_HORZ` when the columns are full and `PARTITION_VERT` when they are
not, and `refuse_invalid_subsize((16,16), edge_part, …)` reduces that through the
same `partition_subsize_dims` every other site uses. A `note_subsize_arm` census
line was added alongside, per the merged lane's convention.

## The named residue: `decode_rect4_16_intrabc` — CLOSED, not guarded a third time

Its concrete code shape is `decode.rs:18336-18337`:

```rust
let (cw, ch) = if own_chroma { (bw >> ss_x(fctx), bh >> ss_y(fctx)) }
               else            { (pw / 2, ph / 2) };      // <- hardcoded both-axes halving
```

`own_chroma = own444 || own422`, and `own422 = ss_x == 1 && ss_y == 0 && horz`
(`18317`). A **VERT** strip at `ss (1,0)` is therefore neither `own444` nor
`own422`, falls to the `else`, and derives `pw,ph = (8,16)` → `cw,ch = (4,8)` —
a `BLOCK_4X8` plane block, itself `BLOCK_INVALID` at `(1,0)`. Same for the
`(pair_mi.1 * MI / 2, pair_mi.0 * MI / 2)` origin at `18354`.

It is dead, and the reason is derived rather than read:

1. `decode_rect4_16_intrabc` has exactly **one** caller (`decode_rect4_16_strip`,
   `18934`).
2. `decode_rect4_16_strip` has exactly **two** (`17595` from `decode_rect4_16`,
   `13810` from `decode_intra_rect_in_inter`).
3. `decode_rect4_16` (`36793`) and `decode_intra_rect_in_inter` (`45447`, reached
   only when `fctx.inter_strip_chroma` is set — one writer, `55116`) are both
   **16x16-level 1:4 arms**, and both sit above a
   `refuse_invalid_subsize((16, 16), part16, …)`.
4. `partition_subsize_dims((16,16), PARTITION_VERT_4) == (4,16)`, and `(4,16)` is
   in `PLANE_BLOCK_INVALID_422`. So the guard refuses the **only** shape that
   could reach the bad arm.
5. `PARTITION_HORZ_4` gives `(16,4)`, whose `(1,0)` cell is `BLOCK_8X4` and is
   legal — so the strip that survives is the one `own422` is written for, and
   `refuse_invalid_subsize` does not over-refuse.

Items 1–5 are asserted in
`every_chroma_walking_leaf_dispatch_is_guarded_or_closed_with_a_reason` (4 and 5
derived from the real predicate; 1–3 from counted call sites plus the ordering of
guard < 1:4-arm test < dispatch on each of the two routes). It is closed as
**guarded-at-resolution**, not `unreachable`: the site *is* handable an INVALID
subsize in principle, and what makes it safe is a guard one level up.

## Census table

`guard` = the `refuse_invalid_subsize` that covers the walk. "shape@mode" lists the
footprints the walk is handed and the subsampling mode at which libaom marks them
`BLOCK_INVALID`. Offending shapes at 4:2:2 `(1,0)`: **4x8, 8x16, 4x16, 8x32,
16x32, 16x64, 32x64, 64x128**. 4:4:0 `(0,1)` is excluded throughout — libaom
asserts `subsampling_y == 0` when `subsampling_x == 0`, so no frame can select
it.

| # | site | shapes | offending @mode | guard | mechanism | evidence |
|---|---|---|---|---|---|---|
| 1 | `decode_leaf_rect` **kf 16-edge** (`37067`) | 16x8, **8x16** | 8x16@(1,0) | `refuse_invalid_subsize((16,16), edge_part)` **added this lane** | AtDispatch | census row + ordering assert; mutations A/B red |
| 2 | `decode_rect4_16_intrabc` (`18232`) | 16x4, **4x16** | 4x16@(1,0) | `(16,16)` @ `36472` (kf) / `54799` (inter) | AtResolution | derived VERT_4→(4,16) + caller count, census part (d)/(f) |
| 3 | `decode_rect4_16` (`17552`) | 16x4, 4x16 | 4x16@(1,0) | same `(16,16)` guards | AtResolution | single caller `36793`, under the 1:4 arm |
| 4 | `decode_rect4_16_strip` (`18862`) | 16x4, 4x16 | 4x16@(1,0) | same | AtResolution | callers `17595`, `13810`, both 1:4 |
| 5 | `decode_block_rect` (`15596`) | 32x16, **16x32** | 16x32@(1,0) | `(32,32)` @ `36406` / `54653` | AtResolution | 32-level HORZ/VERT arms |
| 6 | `decode_block_rect4` (`16764`) | 32x8, **8x32** | 8x32@(1,0) | `(32,32)` @ `36406` / `54653` | AtResolution | 32-level HORZ_4/VERT_4 arms |
| 7 | `decode_block_rect64` (`19851`) | 64x16, **16x64** | 16x64@(1,0) | `(64,64)` @ `36278` / `54269` | AtResolution | 64-level 1:4 arms |
| 8 | `decode_block_128rect` (`23921`) | 128x64, **64x128** | 64x128@(1,0) | `(128,128)` @ `35588` / `54032` | AtResolution | 128-root HORZ/VERT + all four AB arms |
| 9 | `decode_rect_split` (`12420`) | 32x16, 16x32, 64x16, 16x64 | all four | inherited from callers 5/7 | AtResolution | 4 call sites, all under 5–8 |
| 10 | `decode_leaf_rect` **AB arms** (`36599`, `36688`, `36737`, `36756`) | 16x8, **8x16** | 8x16@(1,0) | `(16,16)` @ `36472` — `partition_subsize_dims((16,16), VERT_A/B) == (8,16)` | AtResolution | guard is above the AB range, never inside it (merged lane's AB invariant) |
| 11 | `decode_leaf_rect8` (`27733`) | 8x4, **4x8** | 4x8@(1,0) | `(8,8)` @ `36882` / `37179`, plus the redundant inline check at `36904` | AtResolution | both 8x8 resolutions |
| 12 | `decode_inter_sub8_rect2` (`48863`) | 8x4, **4x8** | 4x8@(1,0) | `(8,8)` @ `55211` | AtResolution | inter 8x8 level |
| 13 | `decode_leaf_split4` (`27044`) | 4x4 | none | none needed | unreachable | 4x4's plane block is 4x4 in **every** cell |
| 14 | `decode_inter_sub8_split4` (`46999`) | 4x4 | none | none needed | unreachable | same |
| 15 | `decode_leaf8` (`24413`) | 8x8 | none | none needed | unreachable | 8x8 → 8x8/4x8/4x4, never INVALID |
| 16 | `decode_inter_block8` (`50045`) | 8x8 | none | none needed | unreachable | same |
| 17 | `decode_block` (`22475`) | squares only | none | none needed | unreachable | every square's plane block is valid at **every** selectable mode; the whole BLOCK_INVALID column is rect |
| 18 | `decode_inter_block` (`41372`) | squares only | none | none needed | unreachable | same |
| 19 | `read_sb128_root` (`35493`) | 128x128 root | none itself | own `(128,128)` guard at `35588` | AtResolution | covers rows 5–8 upstream |

**Rows not closed: none.** Every row is either guarded (1–12, 19) or needs no
guard because the shape is codable at every selectable subsampling mode (13–18).

## Evidence

**Guard fires on a live path, and does not over-refuse.** `EC_AV1_SUBSIZE_GUARD_TRACE`
on real `aomenc` 4:2:0 streams of exactly the shape that reaches the arm
(`testsrc2`, widths 40/72/104/136, `h=120`, `--min-partition-size=4
--max-partition-size=64 --sb-size=64`):

```
SUBSIZE_GUARD site=.../decode.rs:37098 bsize=16x16 part=2 codable=true reached=2
SUBSIZE_ARM   site=.../decode.rs:37101 part=2 codable=true reached=2
```

`part=2` is `PARTITION_VERT` — the 8x16 strip, the shape that is INVALID at
4:2:2. At 4:2:0 the guard reports `codable=true`, the frame decodes OK. The guard
is on a live path and refuses nothing it should not.

**A real aomenc witness for the guard FIRING does not exist, and that is a
property of libaom, not a gap in the sweep.** 45 real 4:2:2 encodes were built
(`--i422`, `testsrc2`, 11 widths × 3 heights at cq 45, plus 12
`--tune-content=screen --enable-intrabc=1 --enable-palette=1` encodes at cq 20;
widths chosen so the 16-level column is short, heights so it is the *only* short
axis). None reached `37098` at all. The reason is that aomenc's own
`partition_rect_allowed` is gated on the same `av1_ss_size_lookup` table (the
merged lane cites `partition_search.c:3383-3389`), so a conformant encoder
cannot emit a partition whose plane block is `BLOCK_INVALID`. The guard is
therefore a **hostile-stream** guard, in exactly the same class as the merged
lane's own `37171-37177` note ("Not corpus-reachable at ss(1,0) today, so it is
residue rather than live pixels, and it is guarded anyway"). No witness is
claimed.

**Red-before-green on the new gate** (both mutations against
`every_chroma_walking_leaf_dispatch_is_guarded_or_closed_with_a_reason`):

* delete `refuse_invalid_subsize((16,16), edge_part, …)` → RED, "dispatches
  `decode_leaf_rect` on an 8x16 strip … with NO `refuse_invalid_subsize` on its path".
* move it **below** the `decode_leaf_rect` call (r5's defect in a new place) →
  RED, "sits AFTER the `decode_leaf_rect` it protects -- it can never observe it".

Restored; both green.

**The gate caught two errors in my own census before it shipped** — recorded
because they are the class this lane exists to close:

* it counted 4:4:0 `(0,1)` as a reachable offending mode and flagged `16x4` as
  needing a guard. That is the merged lane's r2 mistake reproduced: refusing a
  legal 4:2:2 shape by testing a cell no `color_config` can request. Fixed by
  restricting the offending set to the selectable modes.
* it flagged `decode_block`/`decode_inter_block` as `AtResolution` when no shape
  they walk is ever INVALID (squares only). Reclassified `unreachable`.

**Named tests.** `cargo check --tests` clean. `cargo test -p ec-av1 --lib -- 422
sub8 subsize chroma_422` → **27 passed, 0 failed** (52 s), including
`a_real_422_key_frame_and_inter_sequence_decode_pixel_exact`,
`the_pinned_422_corpus_cells_decode_pixel_exact`,
`the_pinned_422_bigblock_witnesses_decode_pixel_exact`,
`the_pinned_422_lossless_inter_witnesses_decode_pixel_exact`,
`the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_exact`.
The three structural gates pass together.

**Non-regression on the committed tree.** `cargo test -p ec-av1 --lib -- pixel_exact
byte_exact` → **270 passed, 0 failed** (818 s), on this lane's commit. This is the
over-refusal control at scale: the new guard sits on a live path (measured
reached with `part=2` at 4:2:0 above), and if it refused anything legal these
would go red.

## Wider sweep — hardcoded chroma halvings, NAMED NOT FIXED (out of charter)

One wider finding **is inside** the residue and is covered by row 2:
`decode_rect4_16_intrabc`'s `else` arm at `18336-18337` / `18354` — the
`(pw / 2, ph / 2)` halving and the `(pair_mi.1 * MI / 2, pair_mi.0 * MI / 2)`
origin — is a both-axes halving whose predicate (`own444 || own422`) does not
enumerate every reachable `(ss, shape)` cell. It is dead only because row 2's
guard refuses VERT_4 at 4:2:2. If that guard ever moves, this arm becomes the
live defect.

Outside the charter, named for the owning lanes. Provenance: every row below is
either read by me in the decode spine (rows 1, and the residue's `else` arm
above) or comes from a read-only source sweep run as part of this lane — the
line numbers were confirmed against the worktree, but nothing outside the
decode.rs partition/chroma walk was **reproduced by a probe here**. Rows marked
"not probed" should be re-measured by the lane that takes them.

| site | what | consequence at 4:2:2 |
|---|---|---|
| `decode.rs:21081` `cfl_ac_q3_at` | `let (cw, ch) = (bw / 2, bh / 2)` + 2×2 luma average | correct today only because `cfl_ac_ss` routes 4:2:2 elsewhere; held by the `cfl_ac_q3_at_hits` 4:2:0-only gate |
| `decode.rs:47516, 47908, 49484, 49930` | `mv_to_q4(<y>, <row_mv>, ss_x(fctx))` — the **Y** axis shifted by `ss_x` | each currently masked by a guard pinning the format to one where `ss_x == ss_y` |
| `decode.rs:43251/43262, 43366` | `chroma_side * chroma_side` sizing in the mu-chroma composed grid, where at `ss (1,0)` the plane rect is `(side/2, side)` and a square `side/2` is too short on rows `>= side/2` | a scout finding from a source read; **not reproduced by a probe in this lane** — the lossless sibling of this path was already repaired to `chroma_stride`, these were not. Needs an owning lane with a 128-root 4:2:2 mu-chunk witness |
| `decode.rs:35700, 53411` | floor-`>>` chroma plane allocators | shape assumption at frame edges; scout finding, not probed here |
| `encode.rs:1674-1687, 1862-1863, 7477/7483, 7928/7934, 8215/8221, 9295-9315` | every chroma plane/stride/crop at `width / 2` with no ss term | correct only because the encoder emits 4:2:0 exclusively; `obmc_plan` (`7362`) passes real `ss_x`/`ss_y` next to a `side / 2` stride, so two conventions meet there |
| `tile.rs:5004-5006` (`fn palette_uv_side`) | `palette_uv_side = (side / 2).max(4)` | 4:2:2/4:4:0 palette map mis-sized |
| `stream.rs:6672, 6773, 17075` + `probe.rs:81` | ffmpeg rawvideo oracle slices chroma at `width * height / 4` | **the dangerous ones**: a future 4:2:2 gate using them compares half the oracle's bytes and passes green. The `_422_depth`/`_444` siblings exist and are correct; the 4:2:0 trio is unguarded against the wrong format |
| `encode.rs:1595-1616` (`Picture::grey`/`check`), `1605`, `1658`, `1852` | every test card's chroma allocated and asserted at `width * height / 4` | the crate's test surface is 4:2:0-sized before any test code runs |
| `decode.rs:59012, 59031, 59284-59304` | test-card chroma + `PlaneBuf` test harness at `width/2`, `height/2` | same |
| `examples/gen_coverage_cells.rs:94-96`, `examples/syntax_census.rs` | quarter-plane chroma arithmetic | same |

`mc.rs`, `intra.rs`, `transform.rs`, `compound.rs` are clean — plane dims arrive as
parameters. `restoration.rs` and `film_grain.rs` are per-axis and correct.
`motion_field.rs:74`'s `/ 2` is the 8×8 MV-cell grid, not a chroma extent.

## Not closed, stated plainly

1. **No aomenc witness proves the new guard FIRES**, because libaom's encoder
   cannot emit the symbol (§ Evidence). The red-before-green proof for the guard
   is mutation-based, not corpus-based. This is the same limitation the merged
   lane's guards carry and is not fixable from this side.
2. **The structural gate is a derived site list, not a proof.** It pins the
   mechanism for 19 walks and the guard ordering for the hole, but it cannot see
   a walk that is neither named in the census nor reachable through a
   `partition_w<N>` read — e.g. a future site fed a block shape from a
   non-partition source. The census is a ratchet, not a proof.
3. **`decode_rect4_16_intrabc`'s `else` arm is still wrong in isolation.** It is
   closed by reachability, not by construction. Closing it by construction would
   mean making the arm `unreachable!("...")` at `ss (1,0) && !horz` — deliberately
   not done here, because that would assert a property of the callers from inside
   the callee and break the moment the arm gains a legal 4:2:2 VERT_4 use.