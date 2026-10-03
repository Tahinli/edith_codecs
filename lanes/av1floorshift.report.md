# lane/av1floorshift — the floor-`>>` chroma plane allocators

Base `main` = `81fc00bb` ("Merge lane/av1muchroma: the skip-arm chroma grid was
the one short square"). Worktree `~/.cache/wt/av1floorshift`, own
`CARGO_TARGET_DIR=~/.cache/tgt/floorshift`.

Target: the sites named at `lanes/av1subsizesweep.report.md:242` as
"`decode.rs:35708, 53419` — floor-`>>` chroma plane allocators — shape
assumption at frame edges; not probed", left open by `lane/av1muchroma`.

## The census: which sites shift by 1 instead of `>> ss_x` / `>> ss_y`

The instruction asked for "every chroma allocation that shifts a dimension by 1
instead of `>> ss_x` / `>> ss_y`". Swept the whole crate for that shape
(`>> 1`, `/ 2` on a chroma extent or a chroma allocation) and then read each hit
for whether it is a chroma **plane allocation** at all.

**The premise of the target did not survive the sweep: the two named sites were
already per-axis.** The report's line numbers had drifted *twice* — by ~570 lines
from `e45cc748` to `81fc00bb`, and the sites they point at moved again in this
lane. What they name now:

| site | expression | chroma plane allocation? |
|---|---|---|
| `decode.rs:36380` (`decode_key_frame_tile_with_cdfs`) | `(width >> ss_x(fctx), height >> ss_y(fctx))` | **yes** — the frame's U/V `PlaneBuf` |
| `decode.rs:54161` (`decode_inter_tile`) | same | **yes** — the frame's U/V `PlaneBuf` |

Both were `>> ss_x` / `>> ss_y` — already the per-axis form the charter asks
for. So the finding is not "they shift by 1"; it is **"floor vs. crop"**: `>>`
floors where libaom's own chroma producer (`av1_round_shift`, spelled `round_ss`
at `decode.rs:697`) crops. That is what the target row's "shape assumption at
frame edges" actually meant, and it is the only question left.

Every other `>> 1` / `/ 2` hit in the crate, with its verdict:

| site | what it sizes | verdict |
|---|---|---|
| `decode.rs:18374`, `18391` (`decode_rect4_16_intrabc` `else` arm) | `(pw / 2, ph / 2)` and a `MI / 2` origin | not a plane allocation; **already closed** by the named residue in `av1subsizesweep.report.md` §"The named residue" — dead because the `(16,16)` `refuse_invalid_subsize` one level up refuses `PARTITION_VERT_4` at 4:2:2, the only shape that could reach it |
| `decode.rs:21118` (`cfl_ac_q3_at`) | `bw / 2, bh / 2` | **not** a plane allocation; the 4:2:0 CfL AC signal, gated to 4:2:0-only reachability by `cfl_ac_q3_at_is_reached_only_through_the_420_fallthrough_of_cfl_ac_ss` (static + counter, both directions) |
| `decode.rs:19456` (`decode_intrabc_rect` ctx stamp) | `(oy / 2, ox / 2)` | **already per-axis** two lines below: the code uses `(oy << ss_y(fctx)) / MI`, `(ox << ss_x(fctx)) / MI`; the `/2` is in the explanatory comment naming the 4:2:0 form |
| `decode.rs:41040-41041` (OBMC `overlap_above`/`overlap_left`) | `write_h.min(64) / 2` | luma OBMC overlap in luma pixels (libaom `reconinter.c` 860/899 caps at a 64×64 **luma** block's half); not a chroma extent |
| `decode.rs:31544` | `(w / 2, h / 2)` in a `rect_inter_chroma_set` test helper | test-local; the two shapes it names above it are spelled out |
| `decode.rs:60430-60454`, `60702-60718`, `stream.rs:2334-2358`, `16622` | test-card / harness `PlaneBuf` and `Picture` chroma at `width / 2` | test surface, no decode path |
| `encode.rs` (many), `tile.rs:5005` (`palette_uv_side`) | encoder/test surface | **out of charter** and separately owned — `palette_uv_side` is closed by `lane/av1paletteuv`, the encoder's 4:2:0-only arithmetic is the `encode.rs` row already listed in `av1subsizesweep.report.md`'s wider sweep |

So the whole in-charter surface is **two sites**, and both are already per-axis.

## Per-site verdict

### Site A — key-frame frame chroma planes (`decode.rs:36380`) — **already correct**
### Site B — inter frame chroma planes (`decode.rs:54161`) — **already correct**

Not "correct by luck of the corpus" — **structurally, for every input**:

* `width` / `height` are `(cols, rows) * BLOCK` where `(cols, rows) =
  block_grid(mi_cols, mi_rows)` and `BLOCK == 32` (`decode.rs:7565`,
  `tile.rs:2141`). **Both axes are multiples of 32** before any shift.
* `ss_x` / `ss_y` are `fctx.subsampling_x` / `subsampling_y` — the sequence
  header's single-bit fields, so `ss ∈ {0, 1}` on every stream that exists.
* `32k >> ss == round_ss(32k, ss)` for every `k` and every `ss ≤ 1`. The floor
  **cannot** drop a sample. Frame edge or not.
* The same holds for `true_width` / `true_height` (`mi_cols * 4`, `mi_rows * 4`)
  — multiples of 4, so the true-extent shift is exact too.
* 4:4:0 (`ss (0,1)`) would be exact as well, and cannot exist anyway:
  libaom's `av1_read_color_config` asserts `subsampling_y == 0` when
  `subsampling_x == 0`, and no one of 256 header bytes reads back `(0,1)`
  (the reader census names this).

`the_frame_chroma_alloc_shift_argument_cannot_floor_below_round_ss` checks that
exhaustively over `mi ∈ 1..=512` and `ss ∈ {0, 1}`, on **both** the coded
(`mi.div_ceil(8) * 32`) and true (`mi * 4`) extents, plus an explicit 4:4:0
spelling-out.

### Reachability, measured

Both sites run on **every decoded frame**, so reachability is not the question;
what mattered was whether the corpus could ever *discriminate* floor from crop.
Three byte-pinned witnesses, one per selectable cell:

| file | cell | size | fnv1a64 | covers |
|---|---|---|---|---|
| `420_odd65x65_key.obu` | 4:2:0, **odd** 65×65 | 2305 | `0xf55645c4b139e8f7` | key allocator, on the odd-dimension cell where a floor *would* show (output crop is 33×33 = 1089, not a floored 32×32 = 1024) |
| `422_key_64x64.obu` | 4:2:2 (1,0) | 755 | `0x87fc569cf53bcbc6` | key allocator, half-width / full-height |
| `444_intrabc_rect4_witness.obu` | 4:4:4 (0,0) | 1016 | `0x27c2fad540472994` | key allocator, the `ss == 0` cell where `>> ss` is the identity |

Counted shortfall (`frame_chroma_alloc_short_samples`): **0** on all three.
Crop-disagreement frames (`frame_chroma_alloc_crop_shapes`): **0** on all three.
Allocator invocations: non-zero on all three, asserted `>= frames.len()`.

The inter site (B) is reached by every inter frame; the same arithmetic applies
and the gate's corpus half reads the shared counter, so a regression on either
site moves it.

## The gate

`stream.rs::the_frame_chroma_planes_are_allocated_on_the_per_axis_floor_that_never_rounds`

Three counters in `decode.rs`, deliberately **not** one:

* `frame_chroma_alloc_calls` — **non-vacuity**. One bump per allocator run. The
  other two are zero on every input that exists, so without this the gate would
  pass with the instrument removed entirely.
* `frame_chroma_alloc_crop_shapes` — **decidability**. Fires only where `>> ss`
  and `round_ss` disagree for that frame's own shift argument. Its zero is the
  finding, and it is also the guard that keeps the shortfall assertion from
  going vacuous: a non-zero here means the allocator's argument changed, and the
  report says so in the assertion message rather than passing quietly.
* `frame_chroma_alloc_short_samples` — **the claim**. `round_ss(w,ss_x) *
  round_ss(h,ss_y)` minus the allocation's **own** `(cw, ch)`, summed.

The shortfall reads the allocator's real `(cw, ch)`, passed in as an argument —
not a recomputation of `shape >> ss` beside it. An earlier draft recomputed it
and would have read the same value under either form, i.e. a gate that could not
bite; the sibling lane's `mutation-red-proof-names-its-assertion` caught the
class and this is the fix. `EC_AV1_CHROMA_ALLOC_SWEEP` traces a disagreement
loudly rather than leaving it to be inferred.

## Mutation proof, two ways

**Restoring the shift** — both sites `>> ss_x`/`>> ss_y` → `width / 2, height / 2`
(the 4:2:0-only hardcoded form this target class is made of):

```
panicked at crates/ec-av1/src/decode.rs:22442:
range start index 1256 out of range for slice of length 1024
test the_frame_chroma_planes_are_allocated_on_the_per_axis_floor... FAILED
```

It reds before the gate's assertion — the shortfall is a real out-of-bounds read
in the tile walk, which is the defect's actual consequence. Recorded as such,
not as the gate biting.

**The gate's own assertion**, isolated — `ch` made one sample short
(`.saturating_sub(1)`), too little to trip the bounds check:

```
panicked at crates/ec-av1/src/stream.rs:3308:
the_frame_chroma_planes_are_allocated_on_the_per_axis_floor_that_never_rounds:
420_odd65x65_key.obu (4:2:0) allocated 48 chroma samples SHORT of the `round_ss`
extent the tile's chroma walks may address
  left: 48
 right: 0
test result: FAILED. 0 passed; 1 failed
```

Both mutations reverted; the diff is the census, the wiring, and the two tests.

## Tests

| suite | result |
|---|---|
| the two new gates | 2/2 ok |
| 7 pinned `the_pinned_422_*` + `the_422_skip_arm_*` + `an_odd_luma_420_key_frame_*` | 9/9 ok, unchanged from base |
| 51 `444` gates | 51/51 ok, no regression |
| `cargo check -p ec-av1 --tests --features gate-counters` | clean, no warnings from this lane |

Counters are behind the existing `gate-counters` feature like every other
counter in this crate; run with `--features gate-counters`.

## Still open, not touched

1. **`palette_uv_side`** (`tile.rs:5005`) — closed by `lane/av1paletteuv`. Untouched.
2. **The mu-chroma squares** — closed by `lane/av1muchroma` (the one short square
   was site 1). Untouched.
3. **`encode.rs`'s 4:2:0-only chroma arithmetic** — the encoder emits 4:2:0
   exclusively, so every `width / 2` there is correct for the only cell it can
   emit. Out of charter; the `encode.rs` row in `av1subsizesweep.report.md`'s
   wider sweep already names it, including the `obmc_plan` (`7362`) convention
   split. **No fix is warranted** and none was written.
4. **`stream.rs`'s ffmpeg rawvideo oracle slices** (`width * height / 4`) — the
   row `av1subsizesweep.report.md` calls "the dangerous ones". Still unguarded
   against the wrong format; a future 4:2:2 gate that used them would compare
   half the oracle's bytes and pass green. The new gate here avoids the trap by
   asserting the plane's **shape** (`round_ss` crop of the frame's own
   dimensions) rather than comparing against a 4:2:0-sized oracle slice, which
   is the pattern any such gate should copy.
5. **`decode_rect4_16_intrabc`'s `else` arm** is still wrong in isolation —
   closed by the guard one level up, not by construction. Unchanged; see the
   named residue in `av1subsizesweep.report.md`.

## Reproduction

```
git worktree add ~/.cache/wt/av1floorshift -b lane/av1floorshift 81fc00bb
cd ~/.cache/wt/av1floorshift
CARGO_TARGET_DIR=~/.cache/tgt/floorshift \
  cargo test -p ec-av1 --lib --features gate-counters -- the_frame_chroma
```