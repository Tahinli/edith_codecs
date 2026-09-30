# lane-av1tilerows — H4: 4:4:4 lossless + tile rows

**Verdict.** The H4 *label* is wrong; the defect it points at is real, and it is
**not** a tile defect. Fixed: a hardcoded 4:2:0-shaped chroma reach at 4:4:4 in
the lossless 16x4/4x16 chroma-pair walk (`decode.rs`). Red-before **63 samples
wrong per frame**; entropy **bit-identical** over 351,109 `EC_SYMR` reads.

**Tree.** `lane-av1tilerows` off `4155c7c7`, worktree `~/.cache/wt/av1tilerows`.
Not pushed (Main merges).

## 1. Two corrections to the sweep's H4 framing

Both measured, both reproducible.

### 1.1 `--tile-rows=1` at 256x128 is a no-op

aomenc's default superblock is 128px, so **256x128 is 2x1 superblocks** — there
is no second superblock *row* to split. aomenc silently emits a stream
**byte-identical** to `--tile-columns=0 --tile-rows=0`:

```
g_256x128_notile.obu vs g_256x128_rows1.obu  identical=True (64828/64828 bytes)
```

The sweep's "tiled" H4 fixture contained **no tile rows at all**. (Same trap at
512x128: `g_512x128_notile` == `g_512x128_rows1`, 90553 bytes both.)

Tile rows at 4:4:4 lossless are **exact**. Measured pixel-exact, 6-8 frames
each, against ffmpeg display order, on a recipe that is itself exact untiled:

| recipe | tile grid | wrong/frame |
|---|---|---|
| `clean_notile` | 1x1 | 0 |
| `clean_sb64_r1` | 1x2 | 0 |
| `clean_sb64_r2` | 1x2 | 0 |
| `clean_sb64_r1c1` | 2x2 | 0 |
| `clean_512_r1` | 1x2 | 0 |
| `clean_256x256_r2` | 1x4 | 0 |
| `clean_128x256_r1` | 1x2 | 0 |

(`clean` = `--enable-rect-partitions=0 --enable-palette=0 --cpu-used=2
--lag-in-frames=0 --lossless=1`, so the tile path is measured in isolation.)

### 1.2 The "flat plane from frame 3" is a harness frame-order mismatch

The reported signature — *frames 0-2 byte-exact, frame 3 onward the whole plane
comes out flat, Y(0,0) ours 128 vs ref 81* — is **not a decode defect**. With
altref on (the recipe's default), aomdec's `EC_AV1_FINAL_DUMP` writes **7**
frames in **decode** order while `ffmpeg -f rawvideo` writes **6** in **display**
order. Indexing one against the other:

```
aomdec_decode[i] vs ffmpeg_display[i]: f0 EXACT, f1 29109 (30%), f2 EXACT,
                                        f3 24412 (25%), f4 20046, f5 19906
```

**aomdec vs ffmpeg alone reproduces the identical counts with no edith decoder
in the loop.** Our decode-order output is byte-exact against the oracle on all 7
frames of that exact stream. Any H4-shaped verdict obtained by pairing a
decode-order dump against a display-order reference is a harness artefact.

## 2. The real defect (found at the same recipe, tile-independent)

The stream at the sweep's recipe **does** diverge, and it does so **with zero
tiles** — so it was never a tile-row bug:

```
notile   256x128  wrong=[63, 63, 63, 63, 63, 63]
rows1    256x128  wrong=[63, 63, 63, 63, 63, 63]     (same counts)
```

**First wrong sample: U(110,26)** (ours 8, ref 7). V first at (108,26). 63
samples/frame, U 29 + V 34, deltas **-10..+13**, region x108..125 y26..29.
Frame 0 (the **key** frame) is already wrong, and all five inter frames inherit
it identically.

**Entropy: bit-identical.** `EC_SYMR` both sides, 351,109 reads compared on
`(value, range, symbol, post_rng)` with the CDF convention reconciled
(`32768 - ours_cdf0 == oracle_cdf0`): **zero divergence**. So this is
reconstruction arithmetic, not an entropy fork — which is exactly what
separates it from H1.

**Bisect** (all at 256x128, `--lossless=1`):

| toggle | wrong/frame |
|---|---|
| baseline (default partitions) | 63 |
| `--enable-rect-partitions=0` | **0** |
| `--enable-palette=0` | **0** |
| `--min-partition-size=32` | **0** |
| `--min-partition-size=8` | **0** |
| `--max-partition-size=64` | 63 |
| `--enable-ab-partitions=0` | 63 |
| `--enable-tx-size-search=0` | 63 |
| `--enable-cfl-intra=0` | 63 |

So the site is a **rect-partitioned 16x4/4x16 luma block** whose **lossless**
4:4:4 chroma pair walk mis-answers `Reach::of_tu`.

## 3. Class and fix

**Class: hardcoded 4:2:0-shaped walk at 4:4:4** (`reach-is-4:2:0-shaped`), the
same class the wave fixed in the 422 monochrome-table and chroma-block-size bugs.

`decode.rs`, the lossless 16x4/4x16 chroma-pair walk (both the skip and the
coded arm) passed `tu_reach` a hardcoded **4:2:0 luma footprint**:

```rust
tu_reach(pw, ph, ox * 2, oy * 2, 8, pair_reach, ...)
```

A 4x4 chroma unit is 8x8 **luma** at 4:2:0 — so `ox * 2 / 8` is right there and
**only** there. At 4:4:4 the same unit is 4x4 luma, so the footprint is doubled:
`Reach::of_tu`'s `col_off + tx_w < bw` test then answers against a block twice
its real width, and every unit past the midpoint wrongly reports
`above_right = false`. A directional chroma strip then predicts from a
neighbour libaom never reads.

The wrong rows are **not block-aligned** (y26 is not a multiple of 4) — the
signature of a REACH answer, not of a coefficient or transform error.

Fix: the ss-aware form, identical to the block-walk sibling at
`decode.rs:35342` (`<< ss` at 4:2:0, identity at 4:4:4):

```rust
tu_reach(pw, ph, ox << ss_x(fctx), oy << ss_y(fctx), 4 << ss_x(fctx), ...)
```

Both copies patched (the class sweep found exactly two). The 4:2:0 answer is
unchanged by construction — `<< 1` is `* 2` and `4 << 1` is `8` — which is why
every 4:2:0 gate stayed green.

## 4. Gate

`a_lossless_444_rect16x4_chroma_reach_is_ss_aware` (`stream.rs`), fixture
`fixtures/ll444_rect16x4_chroma_reach.obu`:

- **77918 bytes**, sha256 `382e67c0cc0988d14c500a62f5c3457a3f3b5f76d05acce2f655b1089bb8e7e3`,
  FNV-1a `0xe2a290acf2494bf7`. Recipe: `testsrc2 256x128 yuv444p`, aomenc
  `--profile=1 --lossless=1 --passes=1 --end-usage=q --threads=1 --row-mt=0
  --limit=6 --cpu-used=2 --lag-in-frames=0 --kf-max-dist=100`.
- **Non-vacuity**: header asserted `ss (0,0)` / 8-bit / 256x128 from the
  *parsed* sequence header; `rect4_16_lossless_chroma_hits()` must fire
  (measured **80** on this fixture; a 4:2:0 stream of the same content takes the
  pair-merge and leaves it at 0); every frame byte-exact through the oracle
  `aomdec --rawvideo` in **decode** order (all 7) and ffmpeg in **display**
  order with full-resolution chroma.
- **Mutation proof (non-vacuous, run)**: reverting the two calls to `ox * 2 /
  oy * 2 / 8` turns the gate **RED** —
  `decode-order frame 0 ... differs from the oracle at byte 39534 (ours 8 vs 7),
  63 bytes differ` — byte 39534 is exactly U(110,26), the measured site.

## 5. Identity (all green locally, this tree)

| group | result |
|---|---|
| new gate `a_lossless_444_rect16x4_chroma_reach_is_ss_aware` | ok (mutation red) |
| all `a_lossless` (10, incl. 4:2:0 `a_lossless_16x4_chroma_pair_repairs_the_measured_site`, `a_lossless_sb128_rect_intra`, `a_lossless_libaom_key_frame`, `a_lossless_libaom_inter_frame`) | 10 passed / 0 failed |
| all `444` (9, incl. the 4 committed `ll444_*` pins and `a_444_sb128_root_rect_stream_with_restoration`) | 9 passed / 0 failed |
| tile gates (3: two_tile_rows, …_through_decode_stream, four_tile_rows) | 3 passed / 0 failed |
| sweep's 3 new gates, run in `~/.cache/wt/av1fmt` with this `decode.rs` applied, then restored | 3 passed / 0 failed |

## 6. Handed back to the H1 lane (NOT fixed here)

**512x128, `--cpu-used=2`, `--lossless=1`**: 18779 wrong in the key frame,
growing to **130708** by frame 5. Same *shape* of damage, but it **also forks
entropy** — 459,866 oracle `EC_SYMR` reads vs 369,136 ours — so it is not this
defect and not claimable here. The 512x128 damage is **unchanged by this fix**
(measured before and after: `[18779, 84910, 93811, 104654, 82976, 130708]` both
times) — it forks entropy before any reach is consulted, so
**the residual is an entropy fork in the H1 rect-partition class**. Recommended
for whoever takes H1: re-run the `EC_SYMR` ladder on
`testsrc2 512x128 yuv444p --lossless=1 --cpu-used=2 --lag-in-frames=0
--kf-max-dist=100 --limit=6` and pair the first fork against
`decodetxb.c:158` (`txb_skip`) exactly as the sweep's H1 section did.

Two other pre-existing items found but **not** fixed (out of scope, not
introduced here, and neither is a tile-row defect):

1. **256x256 4:4:4 lossless, zero tiles, `--sb-size=64`**: panics at
   `decode.rs:2787` — `debug_assert_eq!((self.w, self.h), (4, 4))` in
   `TxParams::run`, i.e. a non-4x4 unit reached the lossless WHT path. Release
   builds miss the assert, so this is a latent wrong-shape reconstruction.
2. **256x256 4:4:4 lossless with default (128px) superblocks**: exact. Only the
   64px-SB variant panics.

## 7. Hygiene

- The H4 diagnosis added no instrumentation: it used existing env-gated rungs
  only (`EC_SYMR`, `EC_TRACE` on the oracle).
- The §8 class sweep DID add one temporary env-gated counter
  (`EC_TMP_SB128CHROMA`, at the `decode.rs:41930` arm) to prove 4:4:4
  reachability. It is **removed** — `grep -c EC_TMP_SB128CHROMA
  crates/ec-av1/src/decode.rs` is 0 at HEAD and `git status` is clean against
  the lane commit.
- `git diff` for the fix itself touches `decode.rs` (the two reach calls +
  comments), `stream.rs` (the gate) and the pinned fixture. Nothing else.
- No formatters run.
- Local runs used `EC_NOMEMGUARD=1`: the repo's `scripts/memguard-runner.sh`
  transient-scope wrapper collided with stale `run-p*.scope` units on this box
  ("Unit run-p…scope was already loaded"), which aborts `cargo test` before any
  test runs. That is an environment artefact, not a test failure — every gate
  above was run with the wrapper bypassed.

## 8. Class sweep — is the class closed repo-wide?

Enumerated **every** production call site of `tu_reach`, `tu_reach_rect` and
`Reach::of_tu` (excluding the two wrapper bodies at `decode.rs:1593`/`1622` and
`encode.rs:16024`/`16031`/`16180`, which are inside `mod tests`). Result:
**32 production sites — 27 ss-aware, 4 4:2:2-only, 1 remaining hardcoded
4:2:0-shaped site.** `Reach` is referenced by no crate outside
`crates/ec-av1/src/{decode,encode}.rs`, and no site sits behind a feature or
`cfg` other than `#[cfg(test)]`.

Three correct conventions exist, and no fourth:

- **A — canonical ss shift** `ox << ss_x(fctx), oy << ss_y(fctx), 4 << ss_x(fctx)`:
  17401, 17488 (fixed here), 35359 (the reference form).
- **B — hoisted per-axis luma-span locals** (`span_x = 4 << ss_x`,
  `luma_span_x = chroma_tx << ss_x`, …): 11610, 20401, 20660, 21021, 21618,
  42370, 42497. Semantically identical to A.
- **C — 4:4:4-gated identity literal** (a bare `4` / `chroma_tx`, no shift):
  22235, 22288, 42249, 44605. Each sits under a guard that *names*
  `ss_x == 0 && ss_y == 0` (or `chroma_444`), so the literal IS the identity
  and the guard is load-bearing. Correct as written, but these are the four
  sites that would break first if a 4:2:0/4:2:2 sibling arm were ever added
  beside them — flagged, not changed.

All remaining luma sites pass pure luma pixel offsets and are SS_AWARE by
construction. The 4 HARDCODED_422_ONLY sites (15514, 15782, 17567, 17650) are
guarded by `chroma422_rect32 = … && ss_x == 1 && ss_y == 0` (15417) and
`chroma422_pair16 = … && ss_x == 1` (17341) — 4:2:2, refused by name at
`stream.rs:1749`, and unreachable at 4:4:4. Their `(0, unit_row*8, 8, 8)` is
exactly the ss(1,0) luma footprint of a 4x8 chroma unit.

### The one remaining member: `decode.rs:41934` — reachable at 4:4:4, NOT fixed here

```rust
if side > 64 {                       // plain geometry, not a subsampling test
    let cu_tx = 32usize;             // a CHROMA extent, the 4:2:0 answer
    let luma_span = cu_tx * 2;       // 4:2:0 luma scale -- THE CLASS
    let (cu_x, cu_y) = (cpx + cc * cu_tx, cpy + cr * cu_tx);
    let cu_reach = crate::decode::tu_reach(side, side, cc * luma_span, cr * luma_span, luma_span, …);
```

**Reachability PROVED by counter, not by argument.** A temporary env-gated
counter (added, measured, removed — `grep EC_TMP_SB128CHROMA` is 0 at HEAD)
fired **32 times at `ss=(0,0) side=128`** on a 4:4:4 lossy
`--sb-size=128 --cq-level=20` stream. 4:4:4 is admitted; only
`subsampling_x != subsampling_y` is refused. No committed 4:4:4 fixture fires
it (`444_sb128rect_lr_witness`, `444_leaf8_oob`, and all three `ll444_*` pins
each fire **0**), so this arm has **no 4:4:4 coverage at all**.

**Why it is not patched.** I could not build a witness, and I will not ship an
unvalidated change into a path with zero coverage:

1. The only recipes that fire the arm are also broken by the **pre-existing H1
   entropy fork** — 31272 wrong from frame 3, and byte-identical between base
   `4155c7c7` and this lane's tip, so it is not mine. A reach delta is invisible
   under that much damage: A/B-ing `luma_span = cu_tx << ss_x(fctx)` against
   `cu_tx * 2` gave **byte-identical output on every frame**.
2. Every recipe that IS pixel-exact at 4:4:4 refuses to fire the arm
   (`--enable-rect-partitions=0` and `--enable-1to4-partitions=0` both give 0
   fires across six cq/geometry variants, all exact).
3. `luma_span` is not the whole defect. Per `blockd.h:1372`,
   `av1_get_max_uv_txsize(BLOCK_128X128, 0, 0)` is
   `max_txsize_rect_lookup[BLOCK_128X128]` = `TX_64X64` (adjusted → itself),
   **not** the `TX_32X32` the comment assumes: at 4:2:0 the plane block is
   halved to `BLOCK_64X64` first. So at 4:4:4 `cu_tx` should be 64 and
   `cu_x = cpx + cc * cu_tx` should be `cc * 64`, not `cc * 32` — the arm
   addresses the wrong chroma region entirely, independently of the reach
   argument. Patching only `luma_span` would be a **half fix**: it would change
   the reach answer in an arm whose unit geometry is still 4:2:0-shaped, with
   no gate able to catch the difference.

   > **RETRACTED by `lanes/av1txsizeaudit.report.md` §3.1. DO NOT ACT ON
   > ITEM 3 OR THE RECOMMENDATION BELOW.** `blockd.h:1371` is the *body* of
   > `av1_get_max_uv_txsize`, and its last line is
   > `return av1_get_adjusted_tx_size(uv_tx);`.
   > `av1_get_adjusted_tx_size` (`blockd.h:1361`) takes **no subsampling
   > argument** and maps `TX_64X64`/`TX_64X32`/`TX_32X64` to `TX_32X32`
   > **unconditionally** — the parenthetical "(adjusted → itself)" above is
   > the error: it stopped one line short of the adjustment it annotated.
   > Measured against libaom's own code (a C probe linked against
   > `~/.cache/aom-oracle/build/libaom.a`),
   > `av1_get_max_uv_txsize(BLOCK_128X128, 0, 0) == TX_32X32`, and the
   > oracle's `EC_TRACE_COEFF` on a 4:4:4 `--sb-size=128` stream shows every
   > TX_64X64 luma unit followed by **four** TX_32X32 chroma units per plane
   > per 64x64 mu chunk (`cu_tx` stays 32; the unit *count* is what changes,
   > 1 → 4 at 4:4:4). The site was repaired correctly by `4bfe8d8e`.
   > The generalisation: `max_txsize_rect_lookup` is only half the rule; the
   > `av1_get_adjusted_tx_size` call on its result is the other half and it is
   > unconditional.

**Recommendation (RETRACTED — see the note above).** Fix H1 first; this site
then becomes witnessable and should be repaired as a unit (`cu_tx = 32 <<
ss_x(fctx)`, `luma_span = cu_tx << ss_x(fctx)`, and the per-chunk chroma origin
with it), with the same `<< 1 == * 2` 4:2:0 identity argument. The 4:2:0
identity is not in question — `32 << 1 == 64` — so the repair is provably a
no-op for the only format with committed coverage on this path.

> **The 4:2:0 identity argument does not licence the repair.** `32 << 1 == 64`
> is true, and the 4:2:0 adjusted step is inert — but that is an argument about
> the one format where the change does nothing, and says nothing about 4:4:4,
> where it is wrong. **A subsampling-parameterised expression can only be
> certified by the measured value at every subsampling, never by an identity
> at one of them.**

**Net class status: the class is closed except for this one site, which is
proven reachable and documented rather than silently changed.**

## 17. Round 9 — on merged main (`e719050d`): the fork is GONE

Main absorbed seven lanes at `e719050d`, including a 128-root token-walk
change (`mu_chunk_order`) that sits squarely in the area rounds 3-8 were
chasing. Re-measured on a clean detached worktree at `e719050d` (`main` is
already checked out elsewhere); my fix `4e151813` is an ancestor there and
the reach call is ss-aware, so the base is sound.

**Step 1 — does the fork still exist? NO.**

Same recipe and stream (`v2_c40.obu`, 4:4:4 lossy cq-40, `--sb-size=128`,
256x256, 36 shown frames):

| tree | wrong per frame |
|---|---|
| my base (rounds 3-8) | 1415 ... 62614 ... diverges throughout |
| **merged main `e719050d`** | **0 on all 36 frames** |

And the entropy, which is the authoritative measure:

    oracle reads 217570   merged-main reads 217570
    ENTROPY LOCKSTEP over 217570 reads

Identical read COUNTS and no value/range/symbol/post_rng divergence over the
entire stream. The read-41976 fork, the 32-vs-64 transform-class
disagreement, and every downstream desync are absent on main.

**Which merged change owns it — partially answered, and I will not guess past
what I measured.** The obvious candidate in my named area is av1h5's
`mu_chunk_order` ("read a 128 root's tokens CHUNK-major, not block-raster",
absent from my base). Ablated it: `4155c7c7` + my fix + `8a91ee14` alone
**still diverges** (1415, 1582, 478, 62614, 60028, ...). So that commit is
NOT sufficient on its own. I could not build the all-three-av1h5 arm — the
cherry-pick of `34b56884`/`f6fbad26` onto my base **conflicts** — so this
round reports one negative result and stops, rather than attributing.

**What this means for rounds 3-8.** The 128-root leaf-size disagreement those
rounds named is real, was correctly localised to the 128-root token walk, and
is fixed on main by one of the seven merged lanes. What produced it: the
`EC_SYMR` read-class tags (which proved both decoders were in the SAME read
function and that the difference was table class, not symbol class), the
`n=7` disambiguation against libaom's `eob_flag_cdf64`, and the four
retractions that kept a wrong fix from shipping. The parked inter-64x64
`EC_PART_VAL` alignment fix stays parked — main now answers the question
outright, so it is no longer needed.
