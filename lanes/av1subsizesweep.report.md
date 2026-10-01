# lane/av1subsizesweep — census of every chroma-plane walk a BLOCK_INVALID subsize can reach

Base `main` = `e45cc748` ("Merge lane/av1unwritten: refuse the subsize libaom calls corrupt").
Worktree `~/.cache/wt/av1subsizesweep`, own `CARGO_TARGET_DIR=~/.cache/tgt/subsizesweep`.

## Headline: the find came from censusing WALK sites, not SYMBOL sites

The merged lane's gate enumerates partition-**symbol** sites. This lane enumerates
chroma-plane **walk** sites — every `decode_*` that reconstructs chroma from a
block's luma extent, with the footprints its dispatch can hand it — and that
reframing is what found the hole. A symbol-site census cannot see a subsize that
arrives without a partition symbol at all, and it cannot see a subsize that
arrives through a *differently spelled* symbol read.

Concretely: the key-frame 16-level **frame-edge** arm reads one gathered bit with
`dec.symbol_fixed(&gather(..))` and dispatches `decode_leaf_rect` **directly** on
it. The merged scan's read test was the literal `dec.symbol(`, and
`dec.symbol(` is not a substring of `dec.symbol_fixed(`. Every sibling edge arm
resolves into a `partN` the level's guard sees; this one did not.

## The hole, and the fix

| | reads | resolves to | guarded before? |
|---|---|---|---|
| 128-root edge (`35559`/`35567`) | `symbol_fixed(&gather(..))` | `SPLIT`/`HORZ`/`VERT` | yes — `refuse_invalid_subsize((128,128), part128)` at `35596` |
| 64-level edge (`36247`/`36264`) | same | same | yes — `(64,64)` at `36286` |
| 32-level edge (`36371`/`36391`) | same | same | yes — `(32,32)` at `36414` |
| **16-level edge (key frame, `37043`/`37048`)** | same | **`edge_split`, straight into `decode_leaf_rect`** | **NO — nothing on the path** |
| 16-level edge (inter, `54777`/`54790`) | same | `part16` | yes — `(16,16)` at `54809` |

With `has_cols16 == false` the strip is `BLOCK_8X16`, whose `(1,0)` chroma plane
block is `BLOCK_INVALID` (`av1_ss_size_lookup`, `common_data.c:20`). At 4:2:2 this
walked a subsize libaom refuses at `decodeframe.c:1456`. With `has_cols16 == true`
the strip is `16x8` — `BLOCK_8X16`'s mirror, whose `(1,0)` cell is `BLOCK_8X8` —
and **must** keep decoding.

**Fix** (`decode.rs:37101-37119`): the guard sits on the arm's own path and names
the LEVEL, not a shape — the gathered bit resolves to `PARTITION_HORZ` when the
columns are full and `PARTITION_VERT` when they are not, and
`refuse_invalid_subsize((16,16), edge_part, …)` reduces that through the same
`partition_subsize_dims` every other site uses. A `note_subsize_arm` census line
was added alongside, per the merged lane's convention.

### Reachability, stated separately from the guard's correctness

There **is** a witness, and it is the only offending subsize in its stream.
`crates/ec-av1/fixtures/422_header_edge16_walk.obu` (128 bytes, sha256
`e8ab2bb229896f796cdec4d2cb383b41f057ef8d364da30f160a3f55a452657f`, fnv1a64
`0xcfa2394ebe1f7764`) is a 4:2:2 sequence header over a 4:2:0 key frame's tile at
88x64. Width 88 is 8 mod 16 → the last 16x16 column block is column-short, so the
frame-edge arm fires; height 64 is 16-aligned, so the row is not short and the
both-cut arm is not taken. The gathered bit names `PARTITION_VERT`, so the strip
is `BLOCK_8X16`.

libaom's own verdict:

```text
$ aomdec --rawvideo -o /dev/null fixtures/422_header_edge16_walk.obu
Warning: Failed to decode frame 1: Corrupt frame detected
Warning: Additional information: Block size 8x16 invalid with this subsampling mode
```

The three-way measurement, which is the part that matters:

| build | this decoder |
|---|---|
| with the guard | REFUSED, naming libaom's rule — agrees with `aomdec` |
| **guard deleted** | **`OK: 1 frames decoded, 88x64`** — a full picture from a frame libaom calls corrupt |
| libaom | refuses |

So the guard is load-bearing, not a redundant second refusal, and the gate
`a_422_header_whose_only_invalid_subsize_is_the_16_level_frame_edge_walk_is_refused`
reds when the guard is deleted (verified).

**Severity: a false-accept, not a pixel divergence.** libaom refuses the whole
frame, so there is no "correct" pixel output to diverge from; the defect is that we
returned plausible-looking pixels for a corrupt frame. No legal 4:2:2 stream is
affected — the shape only reaches stream data through a hostile or mismatched
header, which is the same exposure the merged lane's guards already carry.

**The earlier "no witness" claim in this lane was wrong, and so was its
explanation.** The first sweep (45 aomenc-encoded 4:2:2 streams) never reached the
arm, and I attributed that to `partition_rect_allowed`. That mechanism governs the
encoder's partition *search*; this arm's partition comes from a single gathered
bit after a frame-edge geometry test, so `partition_rect_allowed` does not explain
it — the real reason aomenc never produced a witness is **not known**. The
witness exists, but it had to be built from this crate's own encoder with a
**declared** width (`encode_key_frame_at_size`, the `gen_coverage_cells.rs` cell-2
construction) and then re-headered, because aomenc rounds the coded size to even
and never declares 88.

## The named residue: `decode_rect4_16_intrabc` — CLOSED, not guarded a third time

Its concrete code shape is `decode.rs:18334-18337`:

```rust
let (cw, ch) = if own_chroma { (bw >> ss_x(fctx), bh >> ss_y(fctx)) }
               else            { (pw / 2, ph / 2) };      // both axes halved
```

`own_chroma = own444 || own422`, and `own422 = ss_x == 1 && ss_y == 0 && horz`
(`18317`). A **VERT** strip at `ss (1,0)` is therefore neither `own444` nor
`own422`, falls to the `else`, and derives `pw,ph = (8,16)` → `cw,ch = (4,8)` — a
`BLOCK_4X8` plane block, itself `BLOCK_INVALID` at `(1,0)`. Same for the
`(pair_mi.1 * MI / 2, pair_mi.0 * MI / 2)` origin at `18354`.

It is dead, and the reason is derived rather than read:

1. `decode_rect4_16_intrabc` has exactly **one** caller (`decode_rect4_16_strip`, `18934`).
2. `decode_rect4_16_strip` has exactly **two** (`17595` from `decode_rect4_16`, `13810` from `decode_intra_rect_in_inter`).
3. `decode_rect4_16` (`36801`) and `decode_intra_rect_in_inter` (`45457`, reached only when `fctx.inter_strip_chroma` is set — one writer, `55126`) are both **16x16-level 1:4 arms**, each above a `refuse_invalid_subsize((16, 16), part16, …)`.
4. `partition_subsize_dims((16,16), PARTITION_VERT_4) == (4,16)`, and `(4,16)` is in `PLANE_BLOCK_INVALID_422`. The guard refuses the **only** shape that could reach the bad arm.
5. `PARTITION_HORZ_4` gives `(16,4)`, whose `(1,0)` cell is `BLOCK_8X4` and is legal — so the strip that survives is the one `own422` is written for.

Classified **guarded-at-resolution**, not `unreachable`: the site *is* handable an
INVALID subsize in principle, and a guard one level up is what makes it safe.

## Census table

`guard` = the `refuse_invalid_subsize` that covers the walk. Offending shapes at
4:2:2 `(1,0)`: **4x8, 8x16, 4x16, 8x32, 16x32, 16x64, 32x64, 64x128**. 4:4:0
`(0,1)` is excluded throughout — libaom's `color_config` asserts
`subsampling_y == 0` when `subsampling_x == 0`, so no frame can select it.

| # | site | shapes | offending @mode | guard | mechanism | checked by |
|---|---|---|---|---|---|---|
| 1 | `decode_leaf_rect` kf 16-edge (`37124`) | 16x8, **8x16** | 8x16@(1,0) | `(16,16)` `edge_part` **added this lane**, `37106` | AtDispatch | census ordering + the pinned fixture |
| 2 | `decode_rect4_16_intrabc` (`18232`) | 16x4, **4x16** | 4x16@(1,0) | `(16,16)` `36480` kf / `54809` inter | AtResolutionVia | derived VERT_4→(4,16), per-route ordering loop |
| 3 | `decode_rect4_16` (`17552`) | 16x4, 4x16 | 4x16@(1,0) | `(16,16)` `36480` | AtResolution | row-scoped guard + ordering |
| 4 | `decode_rect4_16_strip` (`18862`) | 16x4, 4x16 | 4x16@(1,0) | `(16,16)` `36480` | AtResolutionVia | guard-in-scope + per-route loop |
| 5 | `decode_block_rect` (`15596`) | 32x16, **16x32** | 16x32@(1,0) | `(32,32)` `36414` / `54663` | AtResolution | row-scoped guard + ordering |
| 6 | `decode_block_rect4` (`16764`) | 32x8, **8x32** | 8x32@(1,0) | `(32,32)` `36414` | AtResolution | row-scoped guard + ordering |
| 7 | `decode_block_rect4`[inter] | 32x8, **8x32** | 8x32@(1,0) | `(32,32)` `54663` | AtResolutionVia | guard-in-scope in the inter decoder |
| 8 | `decode_block_rect64` (`19851`) | 64x16, **16x64** | 16x64@(1,0) | `(64,64)` `36286` | AtResolution | row-scoped guard + ordering |
| 9 | `decode_block_rect64`[inter] | 64x16, **16x64** | 16x64@(1,0) | `(64,64)` `54279` | AtResolutionVia | guard-in-scope in the inter decoder |
| 10 | `decode_intra_rect_in_inter` (`13761`) | 16x8, **8x16**, 16x4, **4x16** | all four | `(16,16)` `54809` | AtResolutionVia | guard-in-scope + per-route loop |
| 11 | `decode_block_128rect` (`23929`) | 128x64, **64x128** | 64x128@(1,0) | `(128,128)` `35596` in `read_sb128_root` | AtResolutionVia | guard-in-scope |
| 12 | `decode_rect_split` (`12420`) | 32x16, 16x32, 64x16, 16x64 | all four | inherits from rows 5/7/8/9 | AtResolutionVia | guard-in-scope |
| 13 | `decode_leaf_rect` AB arms (`36599`, `36688`, `36737`, `36756`) | 16x8, **8x16** | 8x16@(1,0) | `(16,16)` `36480` — `partition_subsize_dims((16,16), VERT_A/B) == (8,16)` | AtResolution | guard is above the AB range, never inside it (merged lane's AB invariant) |
| 14 | `decode_leaf_rect8` (`27741`) | 8x4, **4x8** | 4x8@(1,0) | `(8,8)` `36890` / `37189`, plus the inline check at `36912` | AtResolution | row-scoped guard + ordering |
| 15 | `decode_inter_sub8_rect2` (`48920`) | 8x4, **4x8** | 4x8@(1,0) | `(8,8)` `55221` | AtResolution | row-scoped guard + ordering |
| 16 | `decode_leaf_split4` (`27052`) | 4x4 | none | none needed | unreachable | offending set asserted empty |
| 17 | `decode_inter_sub8_split4` (`47056`) | 4x4 | none | none needed | unreachable | offending set asserted empty |
| 18 | `decode_leaf8` (`24421`), `decode_inter_block8` (`50102`) | 8x8 | none | none needed | unreachable | offending set asserted empty |
| 19 | `decode_block` (`22483`), `decode_inter_block` (`41429`) | squares only | none | none needed | unreachable | every square's plane block is valid at every selectable mode; the whole BLOCK_INVALID column is rect |

**Rows not closed: none.** Every row is guarded (1–15) or needs no guard because
the shape is codable at every selectable subsampling mode (16–19).

## The merged lane's matcher gap, widened (r11)

Its site list was incomplete **for a spelling, not for a shape**, so it would miss
the next differently-spelled read the same way. The read test is now derived:

> a partition-symbol READ is a `dec.<name>(..)` call whose **own argument list**
> names a `partition_w<N>[` CDF.

That is the whole definition and it needs no hand-list. `dec.symbol(..partition_w..)`
qualifies; the frame-edge gather's `dec.symbol_fixed(&gather(..partition_w..))`
qualifies; `dec.debug_state()` does not — it never names a partition CDF. A third
spelling is covered the day somebody writes it. The `var == "b"` skip is gone too:
edge reads are **sites** now, and instead of being skipped they must carry a guard
for their own level before the first chroma-walking dispatch that follows them.

**A limitation, stated rather than hidden:** the definition is syntactic. A read
that reaches a `partition_w` CDF through a helper whose argument list does not
mention the CDF name would not be enumerated. There is no such call in this file
today, and the census's `AtDispatch` row covers the arm this gap actually hid.

## Evidence

### The widening bites, and so does every guard the census claims

Per-guard deletion, one at a time, by line number, against the census alone:

| deleted | census |
|---|---|
| 35596 — 128-root (`read_sb128_root`) | RED — "NO such guard there" |
| 36286 — kf 64x64 | RED — "NO such guard in `decode_key_frame_tile_with_cdfs`" |
| 36414 — kf 32x32 | RED — same, naming `decode_block_rect` |
| 36472 — kf 16x16 | RED — ordering ("guard BELOW the dispatch") |
| 36890 — kf 8x8 (first) | RED — ordering, naming `decode_leaf_rect8` |
| 37106 — kf 16-level frame-edge arm | RED — "sits AFTER the `decode_leaf_rect` it protects" |
| 37106 — same guard, moved below the dispatch | RED — the r5 defect in a new place |
| 54279 — inter 64x64 | RED — names `decode_block_rect64[inter]` |
| 54663 — inter 32x32 | RED — names `decode_block_rect4[inter]` |
| 54809 — inter 16x16 | RED — names `decode_intra_rect_in_inter` |
| 55221 — inter 8x8 | RED — names `decode_inter_sub8_rect2` |

**Two guards are claimed by LEVEL, not by site**, and the census stays green when
they are deleted — stated here rather than papered over:

* **37189** — the key-frame 8x8 **second** resolution (the frame-edge straddle loop).
* **54042** — the inter decoder's own 128-root guard (the row claims the shared one in `read_sb128_root`).

Both are caught by the merged scan's resolution-count cross-check
(`the decode body has 12 partition resolutions but only 11 guard sites`). A
per-site row for each would pin them individually; that is the obvious next
ratchet step and it is not in this lane.

### Over-refusal control

`EC_AV1_SUBSIZE_GUARD_TRACE` on real `aomenc` 4:2:0 streams of exactly the shape
that reaches the arm (`testsrc2`, widths 40/72/104/136, `h=120`,
`--min-partition-size=4 --max-partition-size=64 --sb-size=64`):

```
SUBSIZE_GUARD site=.../decode.rs:37106 bsize=16x16 part=2 codable=true reached=2
SUBSIZE_ARM   site=.../decode.rs:37109 part=2 codable=true reached=2
```

`part=2` is `PARTITION_VERT` — the 8x16 strip, INVALID at 4:2:2. At 4:2:0 the
guard reports `codable=true` and the frame decodes: the guard is on a live path and
refuses nothing it should not.

### Tests

* `cargo check --tests` clean.
* `cargo test -p ec-av1 --lib -- 422 sub8 subsize chroma_422` — **27 passed, 0 failed** (52 s), including the pinned 4:2:2 corpus, bigblock, lossless-inter and intrabc-sb128 pixel-exactness gates.
* `cargo test -p ec-av1 --lib -- pixel_exact byte_exact` — **270 passed, 0 failed** (818 s). The over-refusal control at scale: if the new guard refused anything legal these would go red.
* The three structural gates pass together, plus the two hostile-pin refusal gates.

## Wider sweep — hardcoded chroma halvings, NAMED NOT FIXED (out of charter)

One finding **is** inside the residue and is covered by row 2:
`decode_rect4_16_intrabc`'s `else` arm at `18334-18337` / `18354` — the
`(pw / 2, ph / 2)` halving and the `(pair_mi.1 * MI / 2, pair_mi.0 * MI / 2)`
origin — is a both-axes halving whose predicate (`own444 || own422`) does not
enumerate every reachable `(ss, shape)` cell. Dead only because row 2's guard
refuses VERT_4 at 4:2:2. If that guard moves, this arm becomes the live defect.

Outside the charter, named for the owning lanes. Provenance: line numbers
confirmed against the worktree, but nothing outside the decode.rs
partition/chroma walk was **reproduced by a probe here**; rows marked "not probed"
should be re-measured by the lane that takes them.

| site | what | consequence at 4:2:2 |
|---|---|---|
| `decode.rs:21081` `cfl_ac_q3_at` | `(bw / 2, bh / 2)` + 2×2 luma average | correct only because `cfl_ac_ss` routes 4:2:2 elsewhere; held by the `cfl_ac_q3_at_hits` 4:2:0-only gate |
| `decode.rs:47516, 47908, 49484, 49930` | `mv_to_q4(<y>, <row_mv>, ss_x(fctx))` — the **Y** axis shifted by `ss_x` | each masked by a guard pinning the format to one where `ss_x == ss_y` |
| `decode.rs:43251/43262, 43366` | `chroma_side * chroma_side` sizing in the mu-chroma composed grid; at `ss (1,0)` the plane rect is `(side/2, side)` and a square `side/2` is too short on rows `>= side/2` | not probed; the lossless sibling was repaired to `chroma_stride`, these were not. Needs an owning lane with a 128-root 4:2:2 mu-chunk witness |
| `decode.rs:35708, 53419` | floor-`>>` chroma plane allocators | shape assumption at frame edges; not probed |
| `encode.rs:1674-1687, 1862-1863, 7477/7483, 7928/7934, 8215/8221, 9295-9315` | every chroma plane/stride/crop at `width / 2` with no ss term | correct only because the encoder emits 4:2:0 exclusively; `obmc_plan` (`7362`) passes real `ss_x`/`ss_y` next to a `side / 2` stride, so two conventions meet there |
| `tile.rs:5004-5006` (`fn palette_uv_side`) | `(side / 2).max(4)` | 4:2:2/4:4:0 palette map mis-sized |
| `stream.rs:6672, 6773, 17075` + `probe.rs:81` | ffmpeg rawvideo oracle slices chroma at `width * height / 4` | **the dangerous ones**: a future 4:2:2 gate using them compares half the oracle's bytes and passes green. The `_422_depth`/`_444` siblings exist and are correct; the 4:2:0 trio is unguarded against the wrong format |
| `encode.rs:1595-1616` (`Picture::grey`/`check`), `1605`, `1658`, `1852` | every test card's chroma allocated and asserted at `width * height / 4` | the crate's test surface is 4:2:0-sized before any test code runs |
| `decode.rs:58989, 59008, 59261-59281` | test-card chroma + `PlaneBuf` test harness at `width/2`, `height/2` | same |
| `examples/gen_coverage_cells.rs:94-96`, `examples/syntax_census.rs` | quarter-plane chroma arithmetic | same |

`mc.rs`, `intra.rs`, `transform.rs`, `compound.rs` are clean — plane dims arrive as
parameters. `restoration.rs` and `film_grain.rs` are per-axis and correct.
`motion_field.rs:74`'s `/ 2` is the 8×8 MV-cell grid, not a chroma extent.

## Housekeeping

An early edit of this lane leaked into the primary checkout through a relative
path and was reverted before any commit (`git checkout --` on the one file it
touched); a second leak, the `stream.rs` gate, was caught the same way. The
primary checkout is clean at every commit on this branch, and no commit here
contains either hunk.

## Not closed, stated plainly

1. **The structural gate is a derived site list, not a proof.** It pins the
   mechanism for 19 walks and guard ordering for the hole, but it cannot see a
   walk that is neither named in the census nor reachable through a
   `partition_w<N>` read — e.g. a future site fed a block shape from a
   non-partition source. The census is a ratchet.
2. **Two guards are level-claimed, not site-claimed** (kf 8x8 second resolution,
   inter 128-root). The merged scan's count cross-check is what reds them.
3. **The read-spelling derivation is syntactic.** A read reaching a `partition_w`
   CDF through a helper that does not mention the CDF name would not be enumerated.
4. **`decode_rect4_16_intrabc`'s `else` arm is still wrong in isolation** — closed
   by reachability, not construction. Closing it by construction would mean
   asserting a caller property from inside the callee, and breaking the moment the
   arm gains a legal 4:2:2 VERT_4 use.
5. **Why aomenc never produces this shape is unknown.** The witness had to be
   built from this crate's own encoder with a declared width. See the reachability
   section — the earlier `partition_rect_allowed` explanation was wrong.