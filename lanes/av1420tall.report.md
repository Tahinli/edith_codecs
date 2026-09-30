# lane-av1420tall — site 1's 4:2:0 half: NOT REACHABLE, and not by corpus luck

Worktree `~/.cache/wt/av1420tall` (branch `lane-av1420tall`, base `bf5bbaec`),
instrument worktree `~/.cache/wt/av1420tall-pre` (branch `lane-av1420tall-pre`,
base **`18643bb5`** = the PRE-FIX state; `git merge-base --is-ancestor 18643bb5
6d816368` → **false**, and `18643bb5` is the FIRST PARENT of the fix merge
`3698787d`, so the pre-fix code is the tree the arithmetic must be read
against). **The instrument is NOT committed**; it lives only in the `-pre`
worktree, which is never pushed.

## 0. Verdict

| | |
|---|---|
| instrument fires on a 4:2:2 cell | **YES** — 4/4 cells, `totcalls=1 totover=1` each, and the panic follows |
| instrument fires on any 4:2:0 or 4:4:4 cell | **NO** — 0 overruns over **1475 calls** across 7 controls + the new cell |
| 2:1-tall luma inter block coded at 4:2:0 | **YES** — `side=32 w=16 h=32`, `side=16 w=8 h=16`, `side=32 w=8 h=32` |
| 2:1-tall CHROMA PLANE block at 4:2:0 | **YES** — `ss=11 blk=8x16 stride=16` on the new pinned cell |
| site 1 reachable at 4:2:0 | **NO — and it is not reachable IN THE FORMAT, not merely absent from the corpus** |
| the lpf report's "not 4:2:2-specific" claim | **REFUTED on its arithmetic**, not just on its unpinned fixture |

The decisive line is the last one. The lpf report argued *"a 64x128 4:2:0
block's chroma plane block is 32x64, and `stride * stride` allocates 1024 where
the walk addresses 2048."* The **shape** half of that is right and is now
measured (a 4:2:0 lossless cell really does code 2:1-tall plane blocks). The
**stride** half is wrong: at ss (1,1) the site passes the ENCLOSING SQUARE, so
`stride == blk_h` and the walk addresses exactly `stride * stride - 1`. The
4:2:2 case differs because `chroma_stride` there is `write_chroma_w` — the
NARROW axis — which is the whole reason `blk_h > stride` is possible at all.

## 1. The instrument

`read_inter_chroma_lossless` (`decode.rs`, pre-fix tree) gains one
env-gated call at the top of the body, before any read. The predicate reads
ONLY caller arguments, so it is valid at any decode state — it does not depend
on the fix being absent, and the same predicate is meaningful post-fix.

```rust
// lane-av1420tall (INSTRUMENT, not committed)
if crate::envflags::env_flag!("EC_AV1_TALLBLOCK") {
    tall_block_probe(ss_x(fctx), ss_y(fctx), blk_w, blk_h, stride,
                     (org_x, org_y), (reg_w, reg_h), u_out.len(), v_out.len(),
                     std::panic::Location::caller());
}
```

`tall_block_probe` computes, from the caller's own arguments:

```
writes_units = reg_w >= 4 && reg_h >= 4          // libaom's edge clip
max_addr     = (org_y + reg_h - 1) * stride + org_x + reg_w - 1
need         = max_addr + 1                      // last write is dst[last..last+4]
alloc        = if u_len == 0 { stride * stride } else { u_len }   // the PRE-FIX sizing
over         = writes_units && need > alloc
```

`#[track_caller]` names the presenting CALL SITE (`decode.rs:43666` etc.), so
a key identifies not just a shape but which of the nine call sites produced it
— without that, `blk=64x64 stride=64` is ambiguous between the `side<=64`
site and the 128-root mu-chunk site.

**One arithmetic correction, recorded because it nearly produced a false
positive.** The first version compared `max_addr + 4 > alloc`, i.e. treated
`max_addr` as the last *touched* index. It is the last *touched index + 1*;
`max_addr + 4` over-reports by 3 and fired **1969 times on `ll420_a`**, all of
them `blk=32x32 stride=32 maxaddr=1023 alloc=1024` — i.e. pure noise on a
perfectly square block. Fixed to `need = max_addr + 1`; the 4:2:0 corpus went
to zero and the 4:2:2 tooth survived. This is the `partial-fix-revert-discipline`
class in a new costume: an off-by-N in the *instrument* reads exactly like an
over-broad fix.

### The instrument's tooth (required before any zero is trusted)

Every 4:2:2 cell fires, once, immediately before the pre-fix panic:

| cell | exit | `totcalls` | `totover` | firing key |
|---|---|---|---|---|
| `probe/ll422_noibc.obu` | 101 | 1 | **1** | `site=decode.rs:43666 ss=10 blk=32x64 stride=32 maxaddr=2047 need=2048 alloc=1024` |
| `W_intrabc.obu` | 101 | 1 | **1** | `site=decode.rs:43666 ss=10 blk=4x16 stride=4 maxaddr=63 need=64 alloc=16` |
| `X_intrabc_tiled.obu` | 101 | 1 | **1** | `ss=10 blk=4x16 stride=4 maxaddr=63 need=64 alloc=16` |
| `Y_intrabc_10b.obu` | 101 | 1 | **1** | `ss=10 blk=4x16 stride=4 maxaddr=63 need=64 alloc=16` |

and the panic is the pre-fix one, on the line the instrument just reported:
`panicked at crates/ec-av1/src/decode.rs:37701: range end index 1028 out of
range for slice of length 1024` (`ll422_noibc`) — `need 2048` truncating to
`1028` because the walk reaches `(oy+row)*stride + ox` one row at a time.

**All 4:2:2 firing is at ONE call site (`decode.rs:43666`, the `side <= 64`
lossless arm), and every firing key is `ss=10` (4:2:2) with a 2:1 plane
block.** The instrument is not dead and the zeros below are not vacuous.

(4:2:2 is reached with the standard patch-run-restore `EC_AV1_ALLOW_422_PROBE`
bypass on `stream.rs`'s `ss_x != ss_y` refusal. It is reverted in the `-pre`
worktree's committed state; nothing 4:2:2-related is lifted anywhere.)

## 2. The per-cell table at `18643bb5`

`cargo run --release --example decode_probe` under
`EC_AV1_TALLBLOCK=1` on the pre-fix tree.

| cell | chroma | exit | distinct keys | total calls | **overruns** |
|---|---|---|---|---|---|
| `regress_420/ll420_a.obu` | 4:2:0 | 0 | 6 | 44 | **0** |
| `regress_420/ll420_b.obu` | 4:2:0 | 0 | 6 | 44 | **0** |
| `regress_420/ll420_c.obu` | 4:2:0 | 0 | 7 | 43 | **0** |
| `regress_420/ll420_d.obu` | 4:2:0 | 0 | 4 | 86 | **0** |
| `regress_444/ll444_a.obu` | 4:4:4 | 0 | 10 | 145 | **0** |
| `regress_444/ll444_b.obu` | 4:4:4 | 0 | 11 | 153 | **0** |
| `regress_444/ll444_c.obu` | 4:4:4 | 0 | 17 | 1000 | **0** |
| `fixtures/420_lossless_tallinter_8x16.obu` (new) | 4:2:0 | 0 | 2 | 4 | **0** |
| `probe/ll422_noibc.obu` | 4:2:2 | 101 | 1 | 1 | **1** |
| `W_intrabc.obu` | 4:2:2 | 101 | 1 | 1 | **1** |
| `X_intrabc_tiled.obu` | 4:2:2 | 101 | 1 | 1 | **1** |
| `Y_intrabc_10b.obu` | 4:2:2 | 101 | 1 | 1 | **1** |

**1475 calls at 4:2:0/4:4:4, 0 overruns. 4 calls at 4:2:2, 4 overruns.**

The three `ll420_*`/`ll444_*` controls are the same unpinned-by-me cells the
lpf report used, so the "the zero is not a dead instrument" control is
like-for-like.

## 3. The cell: a 4:2:0 lossless stream that DOES code 2:1-tall luma inter

This is the deliverable the ticket asked for, and it was **not** in the corpus.
The blocker was that `--kf-max-dist=9999` (the recipe family the prior lanes
used) makes an all-intra stream, so no inter block exists at all; the first
~10 encodes I made were all-intra and correctly reported zero. Two more things
had to be fixed before the shape appeared:

1. **real inter frames** — `--kf-max-dist=3` and motion-rich content, and
2. **chroma with residual** — a flat-chroma block codes `skip`, and a `skip`
   block never reaches `read_inter_chroma_lossless` at all, so it reports
   zero for a reason that has nothing to do with the extent. The `TALLLUMA`
   companion counter (`census_inter_block_shape`, `h > w`) is what exposed
   this: a 4:2:0 cell was coding `side=32 w=16 h=32` while the walk census
   showed only `8x8` — the tall block was being skipped.

### Pinned cell

`crates/ec-av1/fixtures/420_lossless_tallinter_8x16.obu`

| | |
|---|---|
| sha256 | `76513799683e9b1b1bcd9c2d16ee7cfbc075a3d5e44a49058ec2b9e05d7c9c4e` |
| size | 52 138 B |
| encoder | `~/.cache/aom-oracle/build/aomenc` (durable oracle) |
| source | `cnoise.y4m`, 64x64, 24 frames, `yuv420p`, sha256 `946d7679d7890a8700b1fb736bf9264597b3dffbd3e42d90f3fb48653f6da430` |

Source (ffmpeg, reproducible — re-running it yields a byte-identical y4m):

```
ffmpeg -y -f lavfi -i "nullsrc=s=32x64:r=1:d=24" -f lavfi -i "nullsrc=s=32x64:r=1:d=24" \
 -filter_complex "[0:v]geq=lum='60+40*mod(X*3+T*2\,64)':cb='40+180*abs(sin(X*0.7+T*0.9)*cos(Y*0.5-T*1.3))':cr='200-150*abs(cos(X*0.6-T*0.7)*sin(Y*0.8+T*1.1))',format=yuv420p[a];\
 [1:v]geq=lum='200-40*mod(X*3-T*2\,64)':cb='210-170*abs(sin(X*0.8+T*0.6)*cos(Y*0.4+T*1.7))':cr='30+190*abs(cos(X*0.5+T*0.8)*sin(Y*0.7-T*0.9))',format=yuv420p[b];\
 [a][b]hstack=inputs=2,format=yuv420p" -frames:v 24 cnoise.y4m
```

The two halves drift in OPPOSITE horizontal directions, which is what makes a
tall (`w < h`) partition the rate-distortion optimum rather than a wide one;
the `cb`/`cr` expressions are high-frequency in both axes so the chroma
residual is non-trivial and the blocks do not code `skip`.

Encode (reproduces the pinned sha256 byte-for-byte):

```
aomenc --codec=av1 --obu -o out.obu --passes=1 --end-usage=q --cq-level=0 \
  --cpu-used=1 --threads=1 --row-mt=0 --lossless=1 --lag-in-frames=0 \
  --kf-max-dist=3 --min-partition-size=16 --max-partition-size=64 --sb-size=64 \
  --enable-rect-partitions=1 cnoise.y4m
```

`--enable-rect-partitions=1` is load-bearing: at `=0` the same source and
flags code no tall block at all (`TALLLUMA` = 0), because the rect codebooks
that serve a 1:2 partition are off. That is the `arrival-not-intent` census
rule from `skill://ec-av1-aomenc-oracle-gates` — the flag is not evidence, the
`TALLLUMA` count is.

### What the cell measures

`TALLLUMA` (2:1-tall LUMA inter blocks the stream really codes, from
`census_inter_block_shape(side, write_w, write_h)` with `h > w`):

```
TALLLUMA side=32 w=16 h=32     x12    <- the 2:1-tall luma inter block
```

`TALLKEY` (the walk census, pre-fix tree, `18643bb5`):

```
TALLKEY site=decode.rs:43666 ss=11 blk=8x16 stride=16 org=0,0 reg=8x16 \
        keycalls=1 keyover=0 totcalls=4 totover=0 maxaddr=247 alloc=256
TALLKEY site=decode.rs:43666 ss=11 blk=8x8  stride=8  org=0,0 reg=8x8 \
        keycalls=1 keyover=0 totcalls=4 totover=0 maxaddr=63 alloc=64
```

**This is the answer in one line.** A 4:2:0 lossless inter block whose luma
footprint is 2:1-tall presents the walk a chroma plane block of
`8x16` with **`stride = 16`** — the square — so `maxaddr = 247` against
`alloc = 256`: the walk addresses the last element of the square and not one
past it. `blk_h (16) == stride (16)`, so the ticket's `blk_h > stride`
predicate is **false by exactly the margin it needs**.

Sibling encodes from the same source with `--min-partition-size=8` code
`side=16 w=8 h=16` and `side=32 w=8 h=32` as well — more 2:1-tall luma inter
blocks, all zero (`sw8/c_0_8_1.obu`, sha256
`2f1baf19673b30573c1036c957c82929fa10bc6ae32498be79f116f8a5ff71d9`).

## 4. Why this is "not possible in the format", not "not in the corpus"

The corpus zero is a fact. The verdict needs the arithmetic, and the
arithmetic is exhaustive over the caller's whole contract — not a sample.

Every one of the nine call sites passes `stride` as **an enclosing square of
the block's own chroma plane block**, except the 4:2:2 branch. Exhaustive
enumeration (`side` ∈ {8,16,32,64,128}, every `(write_w, write_h)` in
`1..=side` on both axes, i.e. every shape the caller can present):

| site | `stride` argument | 4:2:0 overruns over the whole domain | 4:4:4 | 4:2:2 |
|---|---|---|---|---|
| `43666` / `41870` (`side <= 64` lossless) | `chroma_stride` | **0** | **0** | many |
| `41653` / `43440` (128-root mu chunk) | `chroma_side` | **0** | **0** | 0 |
| `14471` / `14933` / `18237` (rect/128 rect) | `cside = max(cw, ch)` | **0** | **0** | 0 |
| `37763` (`leaf8_inter_chroma_lossless`) | literal `8`, `blk` `8x8` | **0** | **0** | 0 |
| `47992` (sub-8 rect) | literal `SIDE = 8` | n/a | **0** | 0 |

The 4:2:0 row is the whole argument, and it is one line of arithmetic. At
ss (1,1):

```
chroma_side  = (side >> 1).max(side >> 1) = side / 2
chroma_stride = chroma_side                (the `else` arm of the chroma_422 test)
write_chroma  = (write_w >> 1, write_h >> 1)
```

so `stride = side/2` and `blk_h = write_h/2 <= side/2 = stride`, for **every**
`write_h <= side` the caller may pass. `blk_h > stride` requires
`write_h > side`, which the caller contract forbids (`write_w`/`write_h` are
documented at `decode.rs:40070` as "the strip's own true width/height", always
a subset of the square `side`). At 4:4:4 the same identity gives
`stride = side` and `blk_h = write_h <= side`. The bound is therefore not
statistical: it is the definition of the argument the site is handed.

The 128-root mu-chunk site is a second, independent instance of the same
identity, and its `org` offset makes it the only site where the *offset*
rather than the shape could overrun. Enumerated over all chunk origins:

| chroma | `chroma_side` (stride) | chunk | max `maxaddr` | alloc | overruns |
|---|---|---|---|---|---|
| 4:2:0 | 64 | 32x32, 2x2 grid | 4095 | 4096 | **0** (fits to the last element) |
| 4:4:4 | 128 | 64x64, 2x2 grid | 16383 | 16384 | **0** |
| 4:2:2 | 128 | 32x64, 4x2 grid | 16319 | 16384 | 0 |

**4:2:2 is the only format where `stride` is not the enclosing square**, and
the reason is one `if` in the source (`decode.rs:40161`, pre-fix numbering):

```rust
let chroma_422 = ss_x(fctx) == 1 && ss_y(fctx) == 0;
let (chroma_stride, chroma_buf_h) = if chroma_422 {
    (write_chroma_w, write_chroma_h)   // <-- the NARROW axis
} else {
    (chroma_side, chroma_side)         // <-- the square: 4:2:0 and 4:4:4
};
```

At 4:2:2 `chroma_stride` becomes `write_chroma_w = write_w/2` while
`blk_h = write_h/2`, so `blk_h > stride` becomes reachable the moment
`write_h > write_w` — which is exactly the `32x64` and `4x16` keys measured
above, and exactly why every firing key is `ss=10` with a 2:1 plane block. The
4:2:2-only-ness is not an accident of the corpus; it is one boolean in one
`if`.

### Reconciling with the lpf report and the refutation pass

* The **lpf report** (`lanes/av1422lpf.report.md` §1.1) claimed site 1 is not
  4:2:2-specific, from a 4:2:0 panic on an unpinned `ll420_a.obu`. The
  refutation pass (`lanes/refute-av1-w3a.report.md` §1.1) could not reproduce
  it and called the cell unpinned.
* **Both are now settled, and neither claim survives as stated.** The cell
  does not reproduce because the arithmetic cannot produce it at 4:2:0 — the
  lpf report's "1028 out of range for slice of length 1024" shape is the
  *4:2:2* `chroma_stride` bug, and its `ll420_a` was almost certainly a 4:2:2
  stream (the same class of mistake the report itself flags two paragraphs
  later, about `src_mandel_320x240.y4m` carrying `C422`).
* The lpf report's *diagnosis* of the 4:2:2 mechanism is correct and is what
  this lane's instrument confirms; its *generality claim* is refuted, with the
  refutation resting on an exhaustive source argument plus a new pinned cell
  rather than on the absence of a panic.

## 5. Attempted settings (so the zero is auditable)

Roughly 90 encodes across these axes, all measured:

| axis | values tried |
|---|---|
| geometry | 32x32, 32x64, 64x64, 64x128, 128x128, 128x256, 256x128 frames |
| `--sb-size` | 16, 32, 64, 128 |
| `--min-partition-size` | 4, 8, 16, 32, 64, 128 |
| `--max-partition-size` | 32, 64, 128 |
| `--enable-rect-partitions` | 0, 1 |
| `--cpu-used` | 0, 1, 2, 3, 4 |
| `--lag-in-frames` | 0, 8, 16 |
| `--kf-max-dist` | 2, 3, 4, 9999 |
| content | flat bars, `testsrc2`, two-speed opposite drift, and the chroma-noised two-speed source that worked |
| chroma | 4:2:0 throughout; 4:2:2 via the patch-run-restore probe bypass only |

**A 2:1-tall luma inter block WAS coded at 4:2:0** — in `sw8/c_1_16_1.obu`
(`side=32 w=16 h=32`, x12), `sw8/c_0_8_1.obu` (`side=16 w=8 h=16` x5 and
`side=32 w=8 h=32` x68), `sw8/c_0_16_1.obu` (`side=32 w=16 h=32` x4), and
`sw5/t_1_8_1.obu` / `sw5/t_1_16_1.obu` (x4 each). The cell therefore **does**
answer the question; the earlier "inconclusive" risk (no 2:1-tall luma block
coded at all) does not apply, and the pinned fixture is the proof.

## 6. Regression

`cargo test -p ec-av1 --lib -- 420 444 lossless intra --skip bitrate_target_lands_within_5_percent_over_48_frames`

```
test result: ok. 158 passed; 0 failed; 2 ignored; 0 measured; 641 filtered out; finished in 227.71s
```

**158 passed, 0 failed, 2 ignored, 641 filtered out** (227.71 s). Zero
regressions. This is a clean baseline by construction — the instrument is not
in this tree — which is the point: the pinned fixture is the only addition to
`main` and it changes no decode path.

The instrument is env-gated and lives only in the `-pre` worktree; `main` and
`lane-av1420tall` carry **no decoder change at all** — the only addition to
`lane-av1420tall` is the pinned fixture. `git status --porcelain` is EMPTY in
the primary checkout.

## 7. Non-goals honoured

No push, no merge, no rustfmt. The seven-site fix is not re-opened. The
instrument is **not committed** (it is in `lane-av1420tall-pre`, uncommitted,
which is a throwaway worktree). No 4:2:2 refusal is lifted in any committed
tree; the `EC_AV1_ALLOW_422_PROBE` bypass is patch-run-restore only and the
committed `-pre` state carries it uncommitted alongside the instrument.

## 8. One thing the next lane should know

`maxaddr == alloc - 1` on the 4:2:0 mu-chunk site is not slack, it is exact.
At 4:2:0 with a 128 root, the bottom-right mu chunk's last 4x4 unit writes
indices `4064..4067` of a 4096-element grid and not one element further. Any
future edit that makes the walk's last write even one element later — a
`blk_h` that stops being floored, an `org` that stops being chunk-aligned, a
`reg` that grows by one 4x4 row — turns the entire 4:2:0 128-root lossless
corpus into the 4:2:2 panic. There is no margin at this site; it is worth a
gate that asserts `maxaddr + 1 <= alloc` rather than relying on the identity
holding.
