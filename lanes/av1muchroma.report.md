# lane-av1muchroma — the `chroma_side * chroma_side` mu-chroma sites

Base: `main` @ `820eeb68` (post `lane/av1paletteuv`). Branch: `lane/av1muchroma`.
Scope: **only** the mu-chroma sites that size a chroma region as
`chroma_side * chroma_side`. `palette_uv_side` and the floor-`>>` chroma plane
allocators are **untouched** — named still-open at the end.

`lanes/av1subsizesweep.report.md:241` named this residue "not probed; needs an
owning lane with a 128-root 4:2:2 mu-chunk witness". This lane probed it. The
headline is that **the report's premise had gone stale**: line numbers drifted
AND `chroma_side` itself was redefined by `lane-av1-422j` after that row was
written, which changes most of the verdict.

## The two `chroma_side` bindings (this is the whole story)

There are two, and they are NOT the same expression:

| binding | line | expression | 4:2:2 value for a `side`-square block |
|---|---|---|---|
| `decode_block` (key frames) | `decode.rs:23261` | `side >> ss_x` | `side / 2` |
| `decode_inter_block` (inter) | `decode.rs:42130` | `(side >> ss_x).max(side >> ss_y)` | `side` |

`lane-av1-422j` changed the second to the **enclosing square** of the chroma
rect. So at 4:2:2 (ss `(1,0)`), where the true plane block is `(side/2, side)`:

- every site under `decode_inter_block` allocates `side * side` — the
  **enclosing** square, i.e. **2x the per-axis extent: oversized, never short**;
- site 1, under `decode_block`, allocates `(side/2) * (side/2)` — exactly
  **half** the plane block: **short**.

The `subsizesweep` row asserted "a square `side/2` is too short on rows
`>= side/2`". That is true of site 1 and false of the other eleven, and the
row did not distinguish them.

## Reachability, measured

Method: an env-gated `eprintln` probe at each of the 12 sites printing
`(site, ss_x, ss_y, side, chroma_side, plane_w, plane_h)`, run over every
pinned 4:2:2 gate (`the_pinned_422_*`, 7 tests) and the 4:4:4 sweep (51 tests).
The probe was temporary and is **not** in the commit. Counts are probe hits:

| site | arm | 4:2:2 hits | 4:4:4 hits | 4:2:0 hits |
|---|---|---|---|---|
| 1 | `decode_block` skip arm | **16** | 344 | 5064 |
| 2 | compound `side>64` mu-chunk | 0 | 0 | 2700 |
| 3 | compound lossless whole-block | **258** | 2559 | 89 |
| 4 | compound 4:4:4 quadrant | 0 | 24 | 0 |
| 5 | single-ref `side>64` mu-chunk | **16** | 32 | 2224 |
| 6 | single-ref lossless whole-block | **457** | 3892 | 3952 |
| 7 | single-ref 4:4:4 quadrant | 0 | 648 | 0 |
| 8 | intra-in-inter `side>64` vartx | 0 | 96 | 0 |
| 9 | intra lossless whole-block | **160** | 283 | 549 |
| 10 | intra 4:4:4 quadrant | 0 | 31 | 0 |
| 11 | intra 4:2:2 `side==64` U | **14** | 0 | 0 |
| 12 | intra 4:2:2 `side==64` V | **14** | 0 | 0 |

Corpus: **10 committed 4:2:2 fixtures** across the 7 pinned gates, plus 51
4:4:4 gates. 4:2:2 fixture inventory is 11 `.obu` files; `422_residual_compound_warp_nolr_16f.obu`
is the lr-off arm of another gate, so 10 distinct witnesses fed these numbers.

## Per-site verdict

**Sites 4, 7, 10 — unreachable at 4:2:2, structurally.** Each is guarded
`ss_x(fctx) == 0 && ss_y(fctx) == 0` (`decode.rs:43983`, `45806`, `46866`),
which is 4:4:4 exactly. Not merely unwitnessed: no 4:2:2 stream can enter them.
At 4:4:4 the two forms coincide anyway.

**Sites 2, 8 — unreachable at 4:2:2 by the committed corpus.** Both are
`side > 64` mu-chunk walks inside the compound and intra-in-inter paths. No
committed 4:2:2 fixture reaches them (2 is hit 2700x at 4:2:0, 8 is hit 96x at
4:4:4 — so both are live, just not at this subsampling). Their allocation is the
enclosing square, and their twin **site 5** *is* reached at 4:2:2 (side 128,
plane 64x128) with an in-bounds max index of 16319 against a 16384 allocation.
**No fix invented for the unwitnessed case**, per the standing rule.

**Sites 3, 5, 6, 9, 11, 12 — already correct.** All reached at 4:2:2. All sit
under the enclosing-square `chroma_side`, so each allocates 2.0x its per-axis
extent. Arithmetic check at every observed shape, e.g.:

```
3/6/9 lossless zero   side=16 cs=16 plane=(8,16)  alloc=256  need=128  maxidx=247  OK  2.0x
3/6/9 lossless zero   side=32 cs=32 plane=(16,32) alloc=1024 need=512  maxidx=1007 OK  2.0x
3/6/9 lossless zero   side=64 cs=64 plane=(32,64) alloc=4096 need=2048 maxidx=4063 OK  2.0x
5  single-ref mu-chunk  side=128 cs=128 plane=(64,128) alloc=16384 need=8192 maxidx=16319 OK 2.0x
11/12 422 side==64      side=64 cs=64 plane=(32,64) alloc=4096 need=2048 maxidx=4063 OK 2.0x
```

Mutation-verified in the other direction too: **shrinking sites 11/12 to
`chroma_w * chroma_h` reds** `the_pinned_422_corpus_cells` and
`the_pinned_422_palette_intra_in_inter`. The block-tail `mu_chroma_units`
replay (`decode.rs:47387`) strides the assembled grid by `chroma_side`, so the
square **is** the writer/reader contract here. These are not defects and must
not be "fixed" per-axis.

**Site 1 — FIXED and gated.** The only genuine under-allocation.
`decode.rs:23587`:

```rust
// before
let u_grid = vec![0i32; chroma_side * chroma_side];   // (side/2)^2 at 4:2:2
// after
let u_grid = vec![0i32; chroma_side * chroma_height]; // the plane block
```

**Was it live?** No — the shape was latent, and this lane says so rather than
overclaiming. The sole consumer is `record_rect` → `record_mi_rect`, which
reduces each plane through `neighbour_state` (`decode.rs:9032`): it sums the
slice and reads `grid[0]`, nothing per-row, and on a skip block every sample is
zero. Mutation-confirmed both ways: sizing site 1 per-axis leaves all 7 pinned
4:2:2 gates green (no live pixel path), so this is a correctness fix for a
consumer that does not exist yet, not a red-to-green rescue. Fixed anyway: an
allocation half the block it names is a trap for the next per-row consumer.

## The gate

`stream.rs::the_422_skip_arm_chroma_grid_is_sized_per_axis_not_square`, witness
`s422_322x240.obu` (21096 B, fnv1a64 `0x9a96dda2a19d47a6`, 322x240, 8-bit,
16 decode frames) — chosen because it is the smallest 4:2:2 fixture that
actually reaches site 1 (six do: `s422_320x246` +4, `s422_322x240` +1,
`s422_322x246` +3, `s422_352x242_10b` +2, `s422_416x242_10b` +3,
`s422_416x250_10b` +3).

Two paired counters in `decode.rs`, deliberately **not** one:

- `skip_arm_chroma_rect_shapes` — non-vacuity. Fires only where the per-axis and
  square forms differ (4:2:2). The gate asserts it moved, so a witness that
  stopped reaching the site cannot fake a green.
- `skip_arm_chroma_short_samples` — the claim. Reads
  `chroma_side * chroma_height - u_grid.len()`, i.e. the allocation's **real
  length**, not a proxy condition. The gate asserts it stayed 0.

### Mutation proof (restore the square)

```
let u_grid = vec![0i32; chroma_side * chroma_side];   // restored
```
→
```
panicked at stream.rs:3204:
decode_block's SKIP arm chroma grids fell 64 samples SHORT of their own
plane block -- the square `side >> ss_x` form this lane removed allocates
exactly half the plane block at 4:2:2 (ss (1,0))
test result: FAILED. 0 passed; 1 failed
```

The first gate draft used a `chroma_side != chroma_height` proxy counter and did
**not** bite on that mutation — a vacuous gate, caught and replaced with the
length-measuring counter above.

## Tests

| suite | result |
|---|---|
| 7 pinned `the_pinned_422_*` byte-exact gates | 7/7 ok (unchanged from base) |
| new `the_422_skip_arm_chroma_grid_...` | ok |
| 51 `444` gates | 51/51 ok, no regression |
| full `cargo test -p ec-av1 --lib` | timed out at 3600 s on live `aom`-encode tests (`a_real_aomenc_inter_128x128_none_root_...`); not a failure, an unfinished sweep. All 4:2:2 and 4:4:4 subsets ran green. |

Feature note: the gate's counters are behind the existing `gate-counters`
feature like every other counter in this crate; it was run with
`--features gate-counters`.

## Still open, not touched by this lane

1. **Floor-`>>` chroma plane allocators** — `decode.rs` frame-edge allocators
   named at `av1subsizesweep.report.md:242`. Out of charter; untouched.
2. **`palette_uv_side`** (`tile.rs`) — gated by `lane/av1paletteuv`, untouched.
3. **Sites 2 and 8 at 4:2:2** — allocation is sound (enclosing square), but no
   committed 4:2:2 fixture reaches them. A 128-root 4:2:2 compound or
   intra-in-inter witness would close the reachability question; **no fix is
   warranted** and none was written.
4. The two stride conventions in the block-tail replay — `chroma_stride` at
   `decode.rs:47347` (lossless) vs `chroma_side` at `47387` (non-lossless).
   Both correct for their own writer; noted as a convention split, not a defect.

## Reproduction

```
git worktree add ~/.cache/wt/av1muchroma -b lane/av1muchroma 820eeb68
cd ~/.cache/wt/av1muchroma
CARGO_TARGET_DIR=~/.cache/muchroma-target \
  cargo test -p ec-av1 --lib --features gate-counters -- \
  the_pinned_422 the_422_skip
```

`ffmpeg` is on `PATH`; the byte-exact gates run live against it.