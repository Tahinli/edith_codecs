# lane-av1444palw — the 4:4:4 palette chroma walk's HORIZONTAL arm (nw >= 2) and a 12-bit cell

## Verdict

| claim | before this lane | after |
| --- | --- | --- |
| horizontal windowing (`cu_col * uw`, `nw >= 2`) on a palette chroma block | "correct by construction", reached by **nothing** in 106 committed fixtures | **MEASURED**: 2 committed fixtures, 8 windowed units on the `nw = 2` arm, byte-exact vs `aomdec`, reds without the window |
| 12-bit 4:4:4 palette chroma | never built | **MEASURED**: 1 committed 12-bit fixture, 4 windowed units on the `nw = 2` arm, byte-exact, reds without the window |
| coverage-debt counter floor | `MIN_WINDOWED_UNITS = 4`, a floor from ONE stream | re-measured per stream below; no single floor is a census |

No second defect was found. The horizontal window is byte-exact wherever it is reached.

## The mechanism, and why the arm was dead

`decode_block_rect64`'s 32-capped chroma walk (`decode.rs:20075`):

```rust
let (uw, uh) = (chroma_w.min(32), chroma_h.min(32));
let (nw, nh) = (chroma_w / uw, chroma_h / uh);
```

and both tiled arms hand each unit

```rust
palette_window(pbuf, chroma_w, cu_col * uw, cu_row * uh, uw, uh)
```

(`decode.rs:20132` square-unit arm, `decode.rs:20200` rect-unit arm). At 4:4:4 (ss
0,0) the chroma plane block equals the luma one, so a **64-wide** luma rect
block keeps `cw = 64` and the cap tiles it into `nw = 2`. That shape is only
reachable from `PARTITION_HORZ` / `HORZ_A` / `HORZ_B` (64x32) and `HORZ_4`
(64x16) — the horizontal siblings of the `PARTITION_VERT` 32x64 strip the
pinned gate already covered.

## Reach census (this lane's own measurement)

`EC_PALWALK=1` — a new env-gated rung in `decode_block_rect64`, printing
`bw/bh/cw/ch/uw/uh/nw/nh/ast/puv/py` per block. Diagnostic only; nothing in the
decode path depends on it.

**Census over every committed fixture** (108 `.obu` files, `decode_probe` per
file, `EC_PALWALK=1`):

| fixture | walks | geometry |
| --- | --- | --- |
| `420_intrabc_rect4_witness.obu` | 4 | `bw=64 bh=32 cw=32 ch=16` / `bw=64 bh=16 cw=32 ch=8` — 4:2:0, so `nw = 1` by subsampling |
| `444_lossy_palette_chroma_352x242_10b.obu` | 1 | `bw=32 bh=64 cw=32 ch=64 uw=32 uh=32 nw=1 nh=2` |
| `r512_rect2x1_1x2_444.obu` | 1 | `bw=32 bh=64 cw=32 ch=64 uw=32 uh=32 nw=1 nh=2` |

That reproduces the charter's census exactly: **every windowed unit in the
corpus has `nw = 1`**. The 4:2:0 fixture is the interesting near-miss — it owns
a genuine 64-WIDE luma palette rect block, and subsampling collapses its chroma
to 32, so the horizontal term stays dead there too.

## The search that reached `nw >= 2`

Dead ends, all measured (walks = `EC_PALWALK` lines, `nw2_puv` = lines with
`nw >= 2` **and** a chroma palette):

| family | cells | result |
| --- | --- | --- |
| testsrc2 4:4:4, width sweep 256…640 x cq 16/24/36 (the pinned gate's recipe verbatim) | 26 | 3 cells with any walk, all `nw = 1` |
| the same cell with `--sb-size=64` | 4 | 309 PALSET but 0 walks — palette fires, never on a 64-wide rect64 block |
| the same cell, raw planes transposed (edges swap axis) | 6 | 0 walks |
| flat synthetic colour cells (64x64 / 64x32 / 128x64 / 64x128 grids, 2-colour halves, 4-colour quadrants, horizontal bands with a growing palette downwards) at cq 20/24/40 x screen-tune on/off | 120 | 0 walks — the encoder picks 32x16 / 16x8 strips there |
| **lane-av1rect14 `ibc640` flag set, 4:4:4 640x480, `--min-partition-size` x `cq` grid** | 12 | **2 cells with `nw = 2` AND `puv = 1`** |

The lever that worked is `--min-partition-size=8` (at 4 it stays `puv = 0`; at
16 the 12-bit cell stops firing) combined with the rect + 1:4 partition flags:
those keep the 64-wide strip in the partition search while a small minimum
partition size stops the encoder from subdividing it past 64.

## The two cells

Provenance, both `aomenc` from `~/.cache/aom-oracle/build/aomenc` on
`ffmpeg -f lavfi -i 'testsrc2=s=640x480:r=25' -frames:v 1 -pix_fmt yuv444p{10,12}le -f yuv4mpegpipe`:

```
aomenc --codec=av1 --profile=1 --input-bit-depth=$D --bit-depth=$D \
  --width=640 --height=480 --passes=1 --end-usage=q --threads=1 --tile-columns=0 \
  --enable-rect-partitions=1 --enable-1to4-partitions=1 --min-partition-size=8 \
  --sb-size=64 --tune-content=screen --enable-intrabc=1 --enable-palette=1 \
  --enable-tx-size-search=0 --cpu-used=0 --lag-in-frames=0 --kf-max-dist=1 \
  --limit=1 --cq-level=45 --obu -o <out>.obu <in>.y4m
```

| fixture | bytes | sha256 | FNV-1a64 | depth | `EC_PALWALK` on the reaching block |
| --- | --- | --- | --- | --- | --- |
| `444_palette_chroma_hstrip_640x480_10b.obu` | 3301 | `40cebc523b327ff41f00d1e813046976990f29fec7da40e730c981b489de6493` | `0x6f94da7595cd9a17` | 10 | `bw=64 bh=16 cw=64 ch=16 uw=32 uh=16 nw=2 nh=1 ast=true puv=1 py=1` — the **rect-unit** arm |
| `444_palette_chroma_hstrip_640x480_12b.obu` | 2571 | `2fb5ba97d7257756a0a2bf781d9f3300ff2d66009989d297758d848561daee9a` | `0x65a58561c1f3538e` | 12 | `bw=64 bh=32 cw=64 ch=32 uw=32 uh=32 nw=2 nh=1 ast=true puv=1 py=0` — the **square-unit** arm |

Both tiled arms of the walk are now covered, at two bit depths.

## Measurement vs the oracle (per plane, per decode-order frame)

Oracle: the instrumented `aomdec` at `~/.cache/aom-oracle/build/aomdec`,
`--rawvideo --output-bit-depth=$D`; ours: `EC_AV1_FINAL_DUMP` u16 LE. 640x480,
one decode-order frame, no hidden frames.

| cell | Y | U | V |
| --- | --- | --- | --- |
| 10-bit | 0 | 0 | 0 |
| 12-bit | 0 | 0 | 0 |

**Comparator liveness, proved per depth before trusting the zero.** Flipping
ONE byte of the oracle's own dump:

```
10-bit: clean diffs 0 -> tampered diffs 1
12-bit: clean diffs 0 -> tampered diffs 1
```

## Mutation proof (the gate reds without the per-unit window)

Reverting both `palette_window(...)` calls at `decode.rs:20132` / `decode.rs:20200`
to the whole-block buffer `pbuf.clone()`, leaving the counter armed, then
running the gate:

```
$ cargo test -p ec-av1 --lib a_444_palette_chroma_horizontal_unit_window_is_byte_exact_10bit_and_12bit -- --nocapture
thread 'stream::tests::a_444_palette_chroma_horizontal_unit_window_is_byte_exact_10bit_and_12bit' panicked at crates/ec-av1/src/stream.rs:10848:17:
a_444_palette_chroma_horizontal_unit_window_is_byte_exact_10bit_and_12bit: decode-order frame 0 of 1 (1 shown, 0 hidden) differs from the oracle at byte 738176 (ours 42 vs 251), 54278 bytes differ
test result: FAILED. 0 passed; 1 failed
```

(10-bit arm; the 12-bit arm alone, with the 10-bit row removed from `ARMS`,
fails the same way at byte 1065280, 13312 bytes differ.) The reachability assert
stays green through both mutations — the counter is untouched by the mutation —
so it is the PIXEL compare that reds, which is what a non-vacuous gate needs.

Per-plane counts of the same mutation, from the dumps:

| cell | Y | U | V | first wrong sample |
| --- | --- | --- | --- | --- |
| 10-bit | 0 | 25553 | 26139 | frame 0 plane U r96 c448, mutant 810 vs oracle 763 (plane V same sample 891 vs 767) |
| 12-bit | 0 | 6400 | 6400 | frame 0 plane U r352 c160, mutant 866 vs oracle 2004 |

Both are the `cu_col = 1` unit reading the block's column 0 — the horizontal arm
itself, not a downstream cascade (luma stays at 0 in both, so no luma-side
neighbour context is involved).

## The re-measured counter census

`decode::chroma_palette_window_hits()` is guarded on `nw * nh > 1`, so a hit
means "the 32-cap tiled this block", i.e. per unit per plane:

| stream | windowed units | of which `nw >= 2` |
| --- | --- | --- |
| `444_lossy_palette_chroma_352x242_10b.obu` (pinned, existing gate) | 4 | 0 |
| `444_palette_chroma_hstrip_640x480_10b.obu` (new) | 8 | 4 |
| `444_palette_chroma_hstrip_640x480_12b.obu` (new) | 4 | 4 |

The charter's "floor of 4" was one stream's number. Across the three committed
palette-chroma streams the total is **16 units, 8 of them on the horizontal
arm**, and the per-stream numbers are what the gate now asserts (8 and 4, both
exactly the measured values — the asserts are `>=` on those measurements, so a
future encoder-side change that drops a block still reds the gate).

## Regression gate

`stream.rs::a_444_palette_chroma_horizontal_unit_window_is_byte_exact_10bit_and_12bit`
— pinned length + FNV-1a64 per arm, the derived-condition counter as a floor, one
decode-order frame at 640x480 with **full-resolution** chroma asserted (so a
4:2:0 extent cannot pass the compare), and `decode_all_frames_vs_oracle` (which
carries the crate's own one-byte oracle-tamper liveness control).

Scoped runs on this branch:

```
$ CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1444palw \
  cargo test -p ec-av1 --lib a_444_palette_chroma_horizontal_unit_window_is_byte_exact_10bit_and_12bit -- --nocapture
a_444_palette_chroma_horizontal_unit_window_is_byte_exact_10bit_and_12bit: fixtures/444_palette_chroma_hstrip_640x480_10b.obu (10-bit) 1 decode-order frame(s) (0 hidden) byte-exact vs aomdec, 8 windowed palette chroma unit(s)
a_444_palette_chroma_horizontal_unit_window_is_byte_exact_10bit_and_12bit: fixtures/444_palette_chroma_hstrip_640x480_12b.obu (12-bit) 1 decode-order frame(s) (0 hidden) byte-exact vs aomdec, 4 windowed palette chroma unit(s)
test stream::tests::a_444_palette_chroma_horizontal_unit_window_is_byte_exact_10bit_and_12bit ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 803 filtered out; finished in 0.50s

$ cargo test -p ec-av1 --lib a_444_lossy_palette_chroma_unit_window_is_byte_exact_at_352x242_10bit -- --nocapture
a_444_lossy_palette_chroma_unit_window_is_byte_exact_at_352x242_10bit: fixtures/444_lossy_palette_chroma_352x242_10b.obu 17 decode-order frame(s) (1 hidden) byte-exact vs aomdec, 4 windowed palette chroma unit(s)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 803 filtered out; finished in 1.20s

$ cargo test -p ec-av1 --lib palette -- --nocapture
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 780 filtered out; finished in 27.07s
```

## Scope and honesty notes

* The only `decode.rs` change is the `EC_PALWALK` diagnostic rung inside
  `decode_block_rect64`; no decode logic changed. Announced over IRC to the
  three peer lanes before landing; `Deniz-4` and `Emre-5` confirmed no overlap.
* `EC_PALWALK` is an env-gated `eprintln` in the same style as the existing
  `EC_PALSYN` / `EC_DEBUG_PAL` rungs, so the census stays reproducible on any
  future fixture without re-deriving it.
* The 12-bit cell needed its own sweep (`--min-partition-size=16`, cq 45): at 8
  the 12-bit chroma never lands a palette on the 64-wide strip. Both settings
  are recorded above so either cell can be reproduced exactly.
* The `nw = 2` walk is still reachable only through the `decode_block_rect64`
  partition arms. A 128-wide chroma plane block (`nw = 4`) would need a
  `128x64` intra block, which this decoder routes elsewhere; that shape remains
  **unmeasured**, and no gate claims it.
