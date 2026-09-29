# lane-av1recipehunt report

Three `#[ignore]`d ec-av1 gates whose RECIPE never provoked the feature. Two are
CLOSED with a pinned, red-proved fixture. One is UNPRODUCIBLE for a structural
reason that no recipe can move, proved by mutation.

Branch `lane-av1recipehunt`, base `a21f3680`. Oracle
`~/.cache/aom-oracle/build/aomenc` (aom `v3.13.3-7-g9bb526a`), ffmpeg
`/usr/bin/ffmpeg`. Every number below was measured in RELEASE on this tree; the
row format is `recipe | parsed feature evidence | counter before | counter after
| pixel verdict`.

Method: hunt deliberately from the code path out, not by random sweep. For each
gate, first read the DECODE-path counter that the gate's own precondition assert
consumes, then work out which encoder-side knob can move it, then sweep only
that knob. Reachability is asserted from the real read (`decode_probe` prints
each counter from the decode path; `enc_probe` prints the writer-side one), never
from a mirror call site, and every firing recipe is additionally pixel-compared.

---

## Gate 1 -- `a_128_superblock_clip_whose_drl_index_the_write_time_stack_cannot_carry_decodes_exact`

**Verdict: UNPRODUCIBLE. The gate stays `#[ignore]`d.**

### Baseline (run unmodified, RELEASE)

```
thread 'encode::tests::a_128_superblock_clip_whose_drl_index_the_write_time_stack_cannot_carry_decodes_exact'
panicked at crates/ec-av1/src/encode.rs:19484:9:
this clip never asked for a drl index the write-time stack could not carry, so it cannot witness the defect
test result: FAILED. 0 passed; 1 failed; ... finished in 32.25s
```

`assert!(clamps > 0)` with `clamps = crate::tile::take_drl_clamp_hits()` after
63.93 s (32.25 s here) of real encoding of mandelbrot 1280x768x12 at q 150 with
`force_sb128(true)`.

### Why no recipe exists

`DRL_CLAMP_HITS` is bumped by `tile::write_inter_mode` (and the two compound
walks) only when the SEARCH chose a `ref_mv_idx` the write-time stack could not
signal. On this tree the search's offer set is inside the signalable range in
every mode:

| mode | search's offer set | writer's signalable ceiling | gap |
|---|---|---|---|
| `NEWMV` | `best_new_mv_syntax`, `for idx in 0..3` -> {0,1,2} | `write_drl_idx(..., start=0, ...)`: `idx < start + 2` -> 2 | none |
| `NEARMV` | `for idx in 1..=2` (both search arms) | `start=1`, `idx < 3`, plus `entries.len() > idx + 1` | none: the search offers `idx` only when `entries.get(idx).is_some()`, i.e. `entries.len() > idx`, and the walk stops one entry later -- the bounds coincide at every stack size |
| `NEARESTMV` / `GLOBALMV` | no DRL | n/a | n/a |
| compound (`write_compound_block`) | every compound `InterInfo` the search builds hardcodes `ref_mv_idx: 0` | walks start at 0/1 | none |

This is not inference. The tree already ASSERTS it, over stack sizes 0..=4, in
`encode::tests::every_drl_index_the_new_mv_pricer_offers_is_one_the_writer_can_signal`:
for every index either pricer offers, `tile::signalled_drl_idx(entries, start, idx)
== idx`. That test and this gate are two readings of one invariant, and it is
still green on this tree (verified). The counter this gate waits on can never
leave zero.

### Attempt table -- 153 combinations, every one `drl_clamp = 0`

Driven by `enc_probe` (`tile::drl_clamp_hits()` printed per encode point).

| recipe | parsed feature evidence | clamps before | clamps after | pixel verdict |
|---|---|---|---|---|
| mandelbrot 320x192x8 q150 (the gate's own clip class, small) | search picks DRL idx 2 (35x, traced) | 0 | 0 | n/a (encode-side) |
| mandelbrot 640x384x8 q150 | search picks DRL idx 2 (163x, traced) | 0 | 0 | n/a |
| mandelbrot 640x384x12 q110 / q150 / q200 | idx 2 chosen | 0 | 0 | n/a |
| mandelbrot 1280x768x8 q110 / q150 / q200 (the gate's EXACT clip) | idx 2 chosen | 0 | 0 | n/a |
| mandelbrot 1280x768x12 q150 (the gate's exact point) | idx 2 chosen | 0 | 0 | n/a |
| testsrc2 320x192 / 640x384 / 1280x768 x 8/12, q 110/150/200 | idx 2 chosen (27x on 640x384) | 0 | 0 | n/a |
| white noise 320x192 / 640x384 x 8/12, q 60/110/150/200 | idx 0 only (drl_index histogram 108/0/0/0 -- the write-time stack never reaches 3 entries) | 0 | 0 | n/a |
| scrolling hard bands 320x192x8 | idx 0 only | 0 | 0 | n/a |
| 4x4 mandelbrot mosaic, per-tile independent drift, 320x192 / 640x384 x 8/12 | idx 2 chosen (35x) | 0 | 0 | n/a |
| 4-px luminance bands, both axes, 192x128, mps 8/16, cpu 0..6 | idx 2 chosen | 0 | 0 | n/a |
| panning field + a faster band inside it, 640x384x10/16, b=32/64, q 100/150/200 | idx 2 chosen | 0 | 0 | n/a |
| diagonal band on a pan, 640x384x10/16, b=32/64, q 100/150/200 | idx 2 chosen | 0 | 0 | n/a |
| three independently moving bands, 320x192x10/16, b=32/64, q 100/150/200 | idx 2 chosen | 0 | 0 | n/a |
| alternating hard 32x32 tiles (whole tiles re-predict from wherever the identical tile last was), 640x384x10, q 150 | idx 2 chosen | 0 | 0 | n/a |
| q ladder 60 / 150 / 220 on one clip (three encode points in a run) | idx 2 chosen | 0 | 0 / 0 / 0 | n/a |

A trace on `best_new_mv_syntax` is the decisive negative: the search DOES choose
DRL index 2, often (163 times on mandelbrot 640x384x8). Index 2 is exactly what
the writer signals, so it is a legal pick, not a clamp. The instrument I built
for the hunt (`tile::drl_index_histogram`, reading `DRL_HITS`) was REMOVED before
commit: it records the SIGNALLED index, which is downstream of the clamp and
structurally cannot show 3+, so it cannot separate "the search never asked" from
"it asked and the stack could not carry it". Shipping it would have been a
misleading instrument.

### Mutation proof -- the recipe was never the problem

`best_new_mv_syntax`'s offer set widened `for idx in 0..3` -> `0..5`, i.e.
letting the search ask for index 3/4 that the 2-step walk cannot signal:

| recipe (identical command line, only the offer set differs) | clamps |
|---|---|
| mandelbrot 640x384x8 q150 | **9** |
| mandelbrot 320x192x8 q150 | **2** |
| mandelbrot mosaic 320x192x8 q150 | **2** |
| testsrc2 640x384x8 q150 | **1** |
| white noise 320x192x8 q150 | 0 |
| alternating hard tiles 640x384x10 q150 | **1** |
| all of the above, mutation reverted | 0 |

Same clips, same presets, only the pricer's offer ceiling moved. The binding
constraint is the ceiling, which no clip can move. Mutation reverted and
`git diff` on `encode.rs` is empty apart from this lane's doc comment.

### Disposition

`#[ignore]` kept -- un-ignoring it would ship a gate that can never pass. The
gate's doc comment now carries the whole finding (the per-mode table, the
existing invariant test that proves it, the 153-row measurement, the mutation
proof, and the owning fix) so the next lane does not repeat the sweep.

**Owning fix, not this lane's to make** (the DRL code is another lane's surface,
and the DRL index range is the AV1 spec's, not ours): either widen the search's
offer set to the spec's own DRL range so the clamp becomes reachable and this
gate can witness it, or delete the three dead `note_drl_clamp` sites and this
gate with them. `deferred(unblock: DRL owner widens `best_new_mv_syntax`'s offer
set past the writer's `start + 2` ceiling, or retires the counter)`.

---

## Gate 2 -- `a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_and_delta_q_decodes_pixel_exact`

**Verdict: CLOSED. Un-ignored, pinned, red-proved.**

### Baseline (run unmodified, RELEASE)

```
a_real_aomenc_stream_..._and_delta_q_decodes_pixel_exact: zero rect64 dequant calls
ever observed CURRENT_Q_IDX != base_q_idx (40 matches, 0 refusals out of 40) --
delta_q was never actually exercised through decode_block_rect64 this run
```

### Root cause of the blindness

r1 swept 40 attempts of `gradients` at 192x128 with `frame_count = 1`. That is a
single KEY frame, and it is the whole reason. `delta_q_present` needs the
temporal (TPL) model to see a spatial complexity map at all; a one-frame encode
has no inter frame to carry it. Measured over 120 sweep arms at 192x128 /
256x192 / 320x256 / 512x384, 4 and 10 frames, cpu 0/1/3, cq 45/55, five source
classes: at `frame_count = 1` EVERY arm read `delta_q_hits() == 0` AND
`rect64_qidx_drift_hits() == 0`. A different failure from "the drift never
reached a rect64 block", and the reason r1 could not see the feature at all.

### The recipe that fires

`testsrc2` 512x384, **4 frames**, `--cpu-used=0`, `delta_q` left at aomenc's
default. Exactly:

```
ffmpeg -v error -y -f lavfi -i testsrc2=size=512x384:rate=25 \
       -pix_fmt yuv420p -frames:v 4 -f yuv4mpegpipe src512.y4m
aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=45 --cpu-used=0 \
       --threads=1 --row-mt=0 --sb-size=64 --enable-rect-partitions=1 \
       --enable-ab-partitions=0 --enable-1to4-partitions=0 \
       --min-partition-size=32 --max-partition-size=64 \
       --enable-restoration=0 --enable-palette=0 --enable-cdef=0 \
       --enable-filter-intra=0 --enable-cfl-intra=0 --enable-intrabc=0 \
       --enable-tx-size-search=0 --obu -o out.obu src512.y4m
```

* source `src512.y4m`: 1179730 bytes, sha256
  `f430d581497e4f6f7db07d0fc92476e92194e1f96c321988ec0739d475cbbe0c`
* output `crates/ec-av1/fixtures/rect64_dq_drift.obu`: **8384 bytes**, sha256
  `094e51bb21b6caccc905417e95a03f9131b448071567d0b14ec8850799ea2501`, fnv1a64
  `0x3d91a21680063ef6` (committed with `git add -f` past the `fixtures`
  gitignore, like every sibling fixture)

### Attempt table (representative; the sweep was 120 arms)

| recipe | parsed feature evidence | drift before | drift after | pixel verdict |
|---|---|---|---|---|
| testsrc2 512x384 **1 frame** cpu0 q45 (r1's shape) | `delta_q_present` absent, 0 groups | 0 | 0 | n/a (no feature) |
| testsrc2 512x384 4 frames cpu0 q45 | 48 `delta_q` groups; `inter_rect: 64x32=2 32x64=2` | 0 | **17** | **EXACT** all 4 frames |
| testsrc2 512x384 10 frames cpu0 q45 | 134 groups | 0 | 16 | EXACT |
| testsrc2 256x192 4 frames cpu0 q45 | 12 groups | 0 | 6 | EXACT |
| testsrc2 256x192 10 frames cpu0 q45 | 55 groups | 0 | 6 | EXACT |
| testsrc2 192x128 4 frames cpu0 q45 | 6 groups | 0 | 2 | EXACT |
| testsrc2 192x128 10 frames cpu0 q45 | 30 groups | 0 | 2 | EXACT |
| grad42 512x384 4 / 10 frames cpu0 q45 | 48 / 68 groups | 0 | 8 / 15 | EXACT |
| grad43 512x384 4 / 10 frames cpu0 q45 | 48 / 76 groups | 0 | 4 / 17 | EXACT |
| smptebars 512x384 4 / 10 frames cpu0 q45 | 48 groups | 0 | 6 / 5 | EXACT |
| **same source, `--cpu-used=1`** | 48-134 groups | 0 | **0** | EXACT |
| **same source, `--cpu-used=3`** | 48-138 groups | 0 | **0** | EXACT |
| **same source, `--deltaq-mode=0` (control)** | **0 groups** | 0 | **0** | EXACT |
| same source, `--cq-level=55` | 48 groups | 0 | 17 | EXACT (byte-identical to cq 45) |
| a "zone"/checkerboard source, every size and preset | 0 groups | 0 | 0 | EXACT (no delta_q at all) |
| smptebars / zone at `--cq-level=30` and `55` | 0 groups | 0 | 0 | EXACT |

`--cpu-used=0` is load-bearing and measured: at `--cpu-used=1` and `3` the same
source reads the same 48 `delta_q` groups and drifts ZERO times -- the faster
presets' RD never leaves a rect64 root in a superblock whose quantizer moved.
`--cq-level` is not load-bearing.

Note for whoever reads the census: a 192x128 SINGLE-frame testsrc2 at cpu 0 is
NOT pixel-exact on this tree (2177 differing bytes), and so is the 25-frame
512x384 one (2190). That is a pre-existing intra-frame defect, unrelated to
`delta_q` (the `--deltaq-mode=0` arm at the same size shows the same count), and
it is why the recipe uses a 4-frame 512x384 cell rather than either of those.

### Red-proof

The gate's own control arm re-runs the same command line with `--deltaq-mode=0`
and requires the drift to stay at 0. Run explicitly, by reverting the
feature-bearing flag INTO the feature arm (and relaxing the byte pin so the
precondition is what fires, not the byte identity):

```
thread 'stream::tests::a_real_aomenc_stream_..._and_delta_q_decodes_pixel_exact'
panicked at crates/ec-av1/src/stream.rs:37752:9:
...: 4 frames pixel-exact but zero rect64 dequant calls ever saw CURRENT_Q_IDX !=
base_q_idx -- delta_q was read and never moved the quantizer inside a rect64 block,
so this run proved nothing (class gate-blind-to-feature)
test result: FAILED. 0 passed; 1 failed
```

The pixel compare passed in that run; only the precondition went red. Both
temporary edits reverted; green again immediately after.

### Green

```
...: 4 frames pixel-exact, sb_rect_hits=40, rect64_qidx_drift_hits=17
(--deltaq-mode=0 control: 0), stream 8384 B fnv1a64=3d91a21680063ef6
test result: ok. 1 passed
```

Runtime 1.7 s (r1's 40-arm sweep was the 60 s+ version).

---

## Gate 3 -- `a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact` (+ `_10bit`)

**Verdict: CLOSED, both arms. Un-ignored, pinned, red-proved.**

### Baseline (run unmodified, RELEASE, both arms)

```
a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact: no 32-level
intra 1:4 strip (32x8/8x32) fired over 40 compared streams (64x16=0 16x64=0 32x8=0 8x32=0)
```
Same text for the `_10bit` arm. 40/40 streams pixel-compared, zero mismatches.

### Root cause of the blindness, and the stale comment

The 22-line comment above the `#[ignore]` blamed a pre-existing inter-frame
pixel defect (frames 3-7 drifting to ~24k samples, max |d| ~220). **That is
stale: the pixel blocker cleared.** What actually kept the gate blind is a
second, purely mechanical fact the comment never mentions -- and its own
conclusion ("the 1:4 shape is not the discriminator either") was measuring the
wrong arm:

* `--min-partition-size=32` with an all-but-empty intra mode set
  (`--enable-smooth-intra/paeth-intra/directional-intra/angle-delta` all `0`)
  never puts a 1:4 partition on aomenc's RD menu at all. Measured: at
  `--min-partition-size=16` the 32-level 1:4 counter is **0 in all 48** attempts
  (10-bit, 192x128 and 256x192, `--cpu-used` 0..5, cq 63/55); at
  `--min-partition-size=8` with the intra mode tools on it fires in **40 of 48**.
* A 32x8/8x32 strip needs an intra mode worth coding. With the four tools off
  the only candidate is DC, and DC never wins a 1:4 strip against inter.

So both the old `--min-partition-size=32` (protecting against a 16x4/4x16
refusal that no longer fires -- at `mps=8` the RD takes neither the 8- nor the
16-level 1:4 on this content, confirmed by the `inter16_1to4` and `rect4_32`
inter counters staying at 0) and the four intra-mode flags were holding the
shape off the menu. r2's `--enable-1to4-partitions=0` "byte-identical stream"
observation is explained by the same thing: with no 1:4 partitions reachable at
all, the flag had nothing to turn off.

### The recipe that fires

mandelbrot zoom, **256x192, 6 frames**, `--cpu-used=2 --cq-level=63`,
`--min-partition-size=8`, the full intra mode set. Exactly:

```
ffmpeg -v error -y -f lavfi \
  -i mandelbrot=size=256x192:start_scale=5.0:end_scale=0.004:end_pts=8:rate=25 \
  -pix_fmt yuv420p -frames:v 6 -f yuv4mpegpipe -      # 10-bit: yuv420p10le + -strict -1
aomenc [--cpu-used=2 --input-bit-depth=10 -b 10] --codec=av1 --passes=1 --end-usage=q \
  --cpu-used=2 --cq-level=63 --threads=1 --row-mt=0 \
  --enable-rect-partitions=1 --enable-1to4-partitions=1 \
  --min-partition-size=8 --max-partition-size=64 --enable-tx-size-search=0 \
  --enable-filter-intra=1 --enable-intra-edge-filter=1 --enable-smooth-intra=1 \
  --enable-paeth-intra=1 --enable-directional-intra=1 --enable-angle-delta=1 \
  --lag-in-frames=0 --auto-alt-ref=0 --kf-min-dist=1000 --kf-max-dist=1000 \
  --enable-order-hint=0 --enable-warped-motion=0 --enable-obmc=0 \
  --enable-masked-comp=0 --enable-interintra-comp=0 --enable-dist-wtd-comp=0 \
  --enable-diff-wtd-comp=0 --enable-onesided-comp=0 --enable-interintra-wedge=0 \
  --enable-smooth-interintra=0 --enable-ab-partitions=0 --enable-cdef=0 \
  --enable-restoration=0 --enable-palette=0 --enable-intrabc=0 \
  --enable-cfl-intra=0 --enable-ref-frame-mvs=0 --obu -o - -
```

* 8-bit source: 442462 bytes, sha256 `e4b46799b180a0d3e138caa18b2738c035e349219490313db664814a3eae9ec2`
* 10-bit source: 884828 bytes, sha256 `b7e0602e0bfc7a7f6813f40bb3989ec7574e5fbbfbf190298b9ab5b625156030`
* `crates/ec-av1/fixtures/intra14_256x192_8bit.obu`: **35179 bytes**, sha256
  `5e50eb6d4492e806a2803988d92bd47f069ca54e02c06344d5500c6b72deb861`, fnv1a64
  `0x86f8d1e5a221162a`
* `crates/ec-av1/fixtures/intra14_256x192_10bit.obu`: **34727 bytes**, sha256
  `ab6fae0a256af6410e8200f10827a3e965dee7b2d7ab63710e94968bddab23a2`, fnv1a64
  `0xecf9455bc20d60f0`

### Attempt table (grid v2: 576 arms, 0 hits; grid v3: 2520 streams carrying SOME 1:4, 54 hits)

| recipe | parsed feature evidence | 64x16 / 16x64 / 32x8 / 8x32 | pixel verdict |
|---|---|---|---|
| **grid v2, the gate's own recipe** (mandelbrot 192x128x8, zoom `start_scale` 3.0/4.4/5.0, cq 63/55/45, cpu 1-4, flagsets `bare`/`intratools`/`screen`/`screen16`, 6 sources x 2 sizes) | NO 1:4 partition of any level in any stream -- `rect4_32`, `inter16_1to4` and `intra_rect4_in_inter` all 0 | 0/0/0/0 | EXACT on all 64 arms |
| banded 4-px content, STATIC in T, mps 8, cpu 0..6 | 241 B stream -- every inter frame SKIPs, no partitions at all | 0/0/0/0 | EXACT |
| 4-px bands that MOVE, mps 8, cpu 0 | `rect32x8_inter_tu: 32x8=1` -- a 32-level 1:4 strip, but INTER | 0/0/0/0 | EXACT |
| 4-px bands that move, mps 8, cpu 4 | `rect4_32: horz=4` / `vert=8` -- 1:4 again, INTER | 0/0/0/0 | EXACT |
| 4-px bands jumping 37/97/173 px per frame, mps 8/32, cq 63, cpu 0/2/4 | 1:4 present, INTER | 0/0/0/0 | EXACT |
| grid v2 with `--min-partition-size=8` and the intra tools ON, plain mandelbrot 192x128x6, cpu 2, cq 63 | **`intra_rect4_in_inter: 32x8=4 8x32=8`** | 0/0/**4**/**8** | **EXACT** |
| same, cpu 3, cq 63 | 32x8=4 8x32=8 | 0/0/4/8 | EXACT |
| same, cpu 2, cq 45 | 8x32=7 | 0/0/0/7 | EXACT |
| same, cpu 3, cq 45 | 8x32=10 | 0/0/0/10 | EXACT |
| 10-bit, 192x128, `--min-partition-size=8`, cpu 0, cq 63 | 32x8=4 8x32=11 | 0/0/4/11 | EXACT |
| **10-bit, 192x128, `--min-partition-size=16`, cpu 0..5, cq 63/55** (24 arms) | **ZERO 1:4 at any level** | 0/0/0/0 | EXACT |
| **10-bit, 256x192, `--min-partition-size=8`, cpu 0..5, cq 63/55** (24 arms) | fires at every preset 0-4, never at 5 | varies | EXACT |
| **`--enable-1to4-partitions=0` control, 8-bit** | `intra_rect4_in_inter` all 0, 20205 B vs 35179 B | 0/0/0/0 | EXACT |
| **CHOSEN: 8-bit 256x192x6 cpu2 q63 mps8** | 32x8=8 8x32=11 | 0/0/**8**/**11** | **EXACT** all 6 frames |
| **CHOSEN: 10-bit 256x192x6 cpu2 q63 mps8** | 32x8=12 8x32=7 | 0/0/**12**/**7** | **EXACT** all 6 frames |

`--min-partition-size=8` vs `16` is the single load-bearing flag: 0/48 vs 40/48.

One methodology note, recorded because it nearly cost the hunt: grid v3's
`scene-cut` source builder reused `[0:v]` for BOTH concat halves, so the
"second scene" argument had no effect and six nominally different rows produced
byte-identical results. The real driver was `mps=8` + the intra mode set, not the
scene cut. The chosen recipe is the plain source with no concat at all.

### Red-proof

The gate's own control arm re-runs the same command line with
`--enable-1to4-partitions=0` and requires all four counters to stay at 0. Run
explicitly, by reverting that flag INTO the feature arm (and relaxing the byte
pin so the precondition is what fires, not the byte identity):

```
thread 'stream::tests::a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact'
panicked at crates/ec-av1/src/stream.rs:40677:9:
... (8-bit): 6 frames pixel-exact but no 32-level intra 1:4 strip (32x8/8x32) fired
(64x16=0 16x64=0 32x8=0 8x32=0) -- the recipe stopped producing the shape this gate names
(class gate-blind-to-feature)
test result: FAILED. 0 passed; 1 failed
```

All 6 frames still compared pixel-exact in that run; only the precondition went
red. Both temporary edits reverted.

### Green

```
...(8-bit):  6 frames pixel-exact, intra 1:4 strips in inter frames 64x16=0 16x64=0 32x8=8 8x32=11 (--enable-1to4-partitions=0 control: 0/0/0/0), stream 35179 B fnv1a64=86f8d1e5a221162a
...(10-bit): 6 frames pixel-exact, intra 1:4 strips in inter frames 64x16=0 16x64=0 32x8=12 8x32=7 (--enable-1to4-partitions=0 control: 0/0/0/0), stream 34727 B fnv1a64=ecf9455bc20d60f0
test result: ok. 3 passed; 0 failed
```

2.3 s and 2.1 s per arm (r1's 40-arm sweep was 60 s+ each).

---

## What changed in the tree

* `stream.rs`: gate 2 and the two gate-3 arms rewritten around a pinned recipe
  with a byte pin (len + fnv1a64 + equality against the committed fixture), a
  hard precondition assert on the decode-path counter, a full pixel compare, and
  a feature-flag control arm. All three un-ignored. The stale 22-line comment on
  the 1:4 pair is replaced by the measurement that actually explains them.
* `encode.rs`: gate 1's doc comment now carries the structural finding, the
  existing invariant test that proves it, the 153-row measurement, the mutation
  proof and the owning fix. The gate and its `#[ignore]` are unchanged.
* `tile.rs`: `pub fn drl_clamp_hits()` -- a non-destructive read of
  `DRL_CLAMP_HITS` (the `#[cfg(test)]` `take_drl_clamp_hits` clears), so a
  recipe sweep can read the gate's own precondition from outside the test
  binary.
* `stream.rs`: `pub fn rect64_qidx_drift_counters() -> (usize, usize)` --
  `(delta_q groups read, rect64 dequant calls that saw CURRENT_Q_IDX !=
  base_q_idx)`. The pair is the point: a recipe that reads no `delta_q` at all
  and a recipe that reads it but never moves the quantizer inside a rect64 block
  are different failures, and gate 2 was the first of those.
* `examples/decode_probe.rs`: prints that pair.
* `examples/enc_probe.rs`: prints `tile::drl_clamp_hits()`.
* Three fixtures committed under `crates/ec-av1/fixtures/` with `git add -f`.

## Hazards hit, worth the next lane's time

* **A shared `CARGO_TARGET_DIR` is not safe for a probe-driven hunt.**
  `~/.cache/cargo-target` was overwritten mid-sweep by a sibling lane's build
  and the probe binary silently lost its new counter lines -- a sweep that reads
  counters can be measuring an instrument that no longer exists. Give each lane
  its own target dir (`~/.cache/cargo-target-<lane>`).
* **A counter placed downstream of the event it names is not an instrument.**
  The first thing built for gate 1 was a histogram of the DRL index each block
  *coded*, read off `DRL_HITS`. It records the SIGNALLED index, so it is
  structurally incapable of showing the 3+ bucket and cannot distinguish "the
  search never asked" from "it asked and could not be carried". It was removed
  rather than shipped. The trace that did work was on the SEARCH's choice
  (`best_new_mv_syntax`), upstream of the writer.
* **`cargo test` under `scripts/memguard-runner.sh` collides on transient
  systemd scopes** ("Unit run-pNNNN.scope was already loaded or has a fragment
  file") when runs overlap. It is a runner failure, not a test failure; retry.
* **A 10-bit pixel compare needs `EC_PROBE_OUT16`, not `EC_PROBE_OUT`.**
  `EC_PROBE_OUT` writes u8 and every sample then differs from ffmpeg's
  `yuv420p10le` raw -- a 220438-byte "defect" that is purely a dump-format
  mismatch.

---

# r2 -- gate 1 disposed of, and the three pins re-verified on merged main

Base moved: rebased onto `aa0ac8c2` ("lanes: merge-wave-2 clean-checkout
census"), 58 commits past this lane's original `a21f3680`. One conflict, in the
two 1:4 gates' comment slot, and it is worth recording because **two lanes
reached the same two facts independently**: `lane-av1pins` had already re-run
both 1:4 gates in RELEASE on the seven-lane tree and recorded that the pixel
blocker was gone and the recipe never fired; this lane then found why and closed
them. Both records are kept side by side in the resolved comment rather than
either being dropped -- `lane-av1pins`' measurement is the merge-main evidence
that the stale mandelbrot-defect claim was dead, and this lane's is the
mechanism.

## Gate 1: DELETED, and the coverage claim moved to where it is asserted

**Decision: delete.** The evidence that decided it, in the order it was
gathered:

1. **The precondition is unreachable**, not merely unprovoked -- established in
   r1 (per-mode offer-set table, the existing invariant test, 153 attempts, and
   the `0..3 -> 0..5` mutation proof).
2. **The pixel half is not unique to the gate.** The deleted test's second half
   -- decode the encoded stream and compare every frame/plane against the
   encoder's own reconstruction, on a 1280x768 `force_sb128(true)` sequence --
   is already made **four times over** at the same geometry by
   `a_128_superblock_clip_whose_root_search_codes_128x128_blocks_decodes_exact`,
   `a_128_root_block_with_a_real_residual_decodes_exact_through_both_decoders`,
   `a_128_root_residual_block_under_a_per_unit_cdef_list_decodes_exact` and
   `a_rect128_half_with_a_real_residual_decodes_exact_through_both_decoders`
   (all four verified: same `force_sb128(true)`, same own-reconstruction
   compare, same ffmpeg compare).

So deletion loses no coverage on **either** half. That is the point on which
"delete vs keep" turned: had the pixel compare been unique, keeping the test
with a fact-stating ignore string would have been the honest call, because
`#[ignore]` is cheap and a deleted unique assertion is not recoverable from the
report. It was not unique, so the gate was pure dead weight in `--list`.

**Why the surviving invariant test is strictly better coverage, not a
substitute.** It enumerates the pricer's whole offer set against
`signalled_drl_idx` for stack sizes 0..=4, and 0..=4 bounds the DOMAIN rather
than sampling it: `best_new_mv_syntax` breaks at `entries.len() <= idx` for
`idx > 0`, so indices 3 and 4 are unreachable at *any* stack size, and a stack
larger than 4 only adds entries neither pricer offers. A clip gate could only
ever have sampled that space; this test closes it.

**Where the disposition is visible.** The cell is not tracked in
`gate_coverage.rs` or `refusal_inventory.rs` (grepped: no `drl_clamp`,
`sb128b` or `write_time_stack` entry in either), so the claim lives on the test
that now carries the coverage, at
**`crates/ec-av1/src/encode.rs:15689`**, in
`every_drl_index_the_new_mv_pricer_offers_is_one_the_writer_can_signal`'s doc
comment. It names the deleted gate, both substitutes, the measurement and the
mutation proof. A second pointer was added at
**`crates/ec-av1/src/encode.rs:18618`** on the sb128 gate whose own doc had a
`cargo test` run-line still naming the deleted test -- a stale instruction left
by the deletion, caught by grepping for the name after the cut.

### GENERAL RULE -- when a dead gate may be deleted, and when it may not

> A gate whose precondition is unreachable is dead weight ONLY when BOTH of its
> halves are covered elsewhere. Here the pixel half is covered four times at
> the same geometry with the same own-reconstruction compare, and the
> precondition is covered by an invariant test that BOUNDS THE DOMAIN (stack
> sizes 0..=4) rather than sampling it -- `best_new_mv_syntax` breaks at
> `entries.len() <= idx`, so indices 3/4 are unreachable at any stack size.
> Had the pixel compare been unique, keeping the test with a fact-stating
> ignore string would have been the honest call; a deleted unique assertion is
> not recoverable from a report.

The asymmetry is the whole rule. `#[ignore]` is nearly free -- a gate that
cannot fire is inert, and its doc comment still tells the next lane what was
tried. Deletion is not free and is not reversible from prose: once the function
is gone, the only surviving record of what it asserted is this report, and
nobody re-derives a coverage claim from a report. So the bar for deletion is
not "the assert cannot fire"; it is "the assert cannot fire AND everything
else the test did is provably done elsewhere". Check every other assertion in
the doomed test before cutting it, not just the one that is broken.

### GENERAL RULE -- a deletion leaves references behind

> Grep the name you removed, after you remove it.

Cutting `a_128_superblock_clip_whose_drl_index_the_write_time_stack_cannot_carry_decodes_exact`
left a live `cargo test ... --nocapture` run-line in a sibling gate's doc
comment (`encode.rs:18619`) pointing at a test that no longer exists -- a
reviewer following it would get an empty run and a filter matching nothing,
with nothing in the output to say the gate had been retired on purpose. The
deletion looked complete because the symbol search came back with only the two
doc mentions I had written myself. It took grepping the bare name (not the
`fn` signature) to find the stale instruction. Same class as the other
doc-drift traps in this repo: a rename or removal is not done until the
prose points at what now exists.

**What was deliberately NOT removed.** `note_drl_clamp` and `DRL_CLAMP_HITS`
stay. They are unreachable today, but they are the tripwire for the owning fix
(`deferred(unblock: the DRL owner widens `best_new_mv_syntax`'s offer set past
the writer's `start + 2` ceiling, or retires the counter)`): the invariant test
goes red the instant the offer set is widened, and `tile::drl_clamp_hits()`
(printed by `enc_probe`) then shows the clamp on real content. Deleting the
counter would have made the fix silent. The `#[cfg(test)]`
`take_drl_clamp_hits()` had no reader left and WAS removed -- the
non-destructive `drl_clamp_hits()` is the accessor that survives.

## Re-verification of the three pins on merged main

Run on the rebased tree at `aa0ac8c2` + this lane, RELEASE, **plain pass (no
`--ignored`)**, with a private `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-rh`
and `touch` on the three edited sources first (the census/source-scan trap).

```
test encode::tests::every_drl_index_the_new_mv_pricer_offers_is_one_the_writer_can_signal ... ok
a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact (8-bit): 6 frames
  pixel-exact, intra 1:4 strips in inter frames 64x16=0 16x64=0 32x8=8 8x32=11
  (--enable-1to4-partitions=0 control: 0/0/0/0), stream 35179 B fnv1a64=86f8d1e5a221162a
test stream::tests::a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact ... ok
a_real_aomenc_stream_..._horz_vert_partition_and_delta_q_decodes_pixel_exact: 4 frames
  pixel-exact, sb_rect_hits=40, rect64_qidx_drift_hits=17 (--deltaq-mode=0 control: 0),
  stream 8384 B fnv1a64=3d91a21680063ef6
test stream::tests::..._and_delta_q_decodes_pixel_exact ... ok
a_real_aomenc_inter_sequence_with_an_intra_1to4_strip_decodes_pixel_exact_10bit (10-bit):
  6 frames pixel-exact, intra 1:4 strips in inter frames 64x16=0 16x64=0 32x8=12 8x32=7
  (--enable-1to4-partitions=0 control: 0/0/0/0), stream 34727 B fnv1a64=ecf9455bc20d60f0
test stream::tests::..._1to4_strip_decodes_pixel_exact_10bit ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 737 filtered out; finished in 4.71s
```

* **`0 ignored`** -- the three gates now run in a plain pass, as they must once
  the recipes carry the coverage.
* Every pin byte-identical to r1 after the rebase onto 58 new commits:
  8384 B / `3d91a21680063ef6`, 35179 B / `86f8d1e5a221162a`,
  34727 B / `ecf9455bc20d60f0`. Wave 2 did not perturb aomenc's output on any
  of these three cells, which is the point of pinning.
* Each gate printed its self-evidencing line with the counter AND its control
  arm in the same line, so a reader can see from one line that the precondition
  fires and that the feature flag is what fires it.
* `cargo check --release -p ec-av1 --all-targets` clean on the rebased tree, no
  warnings.
