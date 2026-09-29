# lane-av1tilemeasure — tile-rows / superres / odd-size cells, measured

**Tree.** `lane-av1tilemeasure` off `a21f3680`, worktree `~/.cache/wt/av1tilemeasure`.
`crates/ec-av1/src/decode.rs` is **untouched**; every verdict below is HEAD's decoder.

**Method.** Every cell was encoded live against the durable oracle
(`~/.cache/aom-oracle/build/aomenc`), its OBUs were **parsed** and the frame
headers' `tile_info` / `use_superres` / coded size read off the parse (never off
the aomenc flag), and the stream was decoded against the instrumented
`~/.cache/aom-oracle/build/aomdec` through the same
`decode_all_frames_vs_oracle` path the committed gates use: it asserts the
decode-order frame COUNT and each frame's byte LENGTH before a single sample is
compared. Control arms first: 4:2:0 12-bit untiled and the two existing 4:4:4
committed recipes both come out byte-exact, so the harness is sound.

---

## 0. Trap 3 (this lane's own): a SHARED `CARGO_TARGET_DIR` silently measures another lane's decoder

The first two sweeps of this lane were run with the house
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target`, which at that moment was being
written by at least one other lane building `ec-av1` from its own worktree. The
example binary linked **that** lane's in-flight `ec-av1` rlib, and the numbers
were not merely noisy — they were a different decoder:

| cell | shared target dir | lane-private target dir |
|---|---|---|
| 4:4:4 lossless 8-bit 2x2 tiles, 256x256 | DIVERGENT f1, 11913 samples, first Y(216,128) | **EXACT 6/6** |
| 4:4:4 lossless 8-bit 1 tile row, 128x256 | DIVERGENT f1, 8055 samples, first Y(108,128) | **EXACT 6/6** |
| 4:2:0 lossless 8-bit 2x2 tiles, 256x256 | DIVERGENT f1, 5545 samples, first Y(64,0) | **EXACT 6/6** |
| 4:2:0 lossless 8-bit 1 tile row, 256x256 | DIVERGENT f1, 39763 samples, first Y(128,0) | **EXACT 6/6** |
| 4:2:0 lossless 8-bit 128x256 untiled | DIVERGENT f1, 25334 samples, first Y(86,0) | **EXACT 6/6** |
| 4:4:4 lossless 10-bit 1 tile row, 128x256 | refused by name ("Golomb tail") | DIVERGENT f0, 74 samples, first U(82,188) |
| 4:4:4 lossless 10-bit 1 tile row, 256x256 | 1208 then 1083 samples (two binaries) | 1374 samples, one number |

A second symptom of the same collision, and the one that could have shipped a
false green: `cargo test -p ec-av1 --lib -- --exact <my new gate>` reported
`0 passed; 717 filtered out` **three times in a row** while the source file
contained the gate — cargo was running the other lane's test binary. It also
rebuilt "successfully" (`Compiling ec-av1`) without my tests in it.

**Rule this lane proposes:** any lane that measures *or gates* `ec-av1` while
other lanes are live must use a LANE-PRIVATE `CARGO_TARGET_DIR`
(`$HOME/.cache/cargo-target-<lane>`). The shared directory is fine for
`cargo check` on a path that no sibling is building, and useless as evidence.
Every number in this report was re-taken in
`$HOME/.cache/cargo-target-av1tilemeasure` and is stable across repeated runs
(three runs of the same cell, identical counts and identical first-fork values).

---

## 1. The `t420_12_row` correction (published verdict: "may be measuring the same stream")

**It was not measuring the same stream, and the flag was not a no-op at that
geometry.** The sweep's caveat assumed 128px superblocks; profile-0 4:2:0
defaults to `--sb-size=64`, so a 256x128 frame is a 4x2 SB grid and
`--tile-rows=1` really does split it. Encoded at the sweep's own flag set
(`--profile=0 --cq-level=20 --cpu-used=2 --enable-palette=0 --enable-intrabc=0
--threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 --limit=6`, source
`mandelbrot=size=256x128:rate=25` + `-vf gblur=sigma=6` yuv420p12le), all three
arms parse as three different grids and are three different streams:

| arm | parsed `tile_info` (all 6 frames) | bytes | sha256 | verdict vs aomdec |
|---|---|---|---|---|
| `t420_12_notile` (the control the sweep never encoded) | 1x1 | 1675 | `6cd14c13356a5d8e` | **EXACT 6/6** |
| `t420_12_col` (`--tile-columns=1`) | 2x1 | 1747 | `0069fa4b1a9f63bd` | **EXACT 6/6** |
| `t420_12_row` (`--tile-rows=1`) | **1x2** | 1785 | `85050393a76cafae` | **EXACT 6/6** |

Two independent ways to see the published cells were distinct streams: the
sweep's own table already lists different sha256 and different byte counts for
`t420_12_col` (`c983a91c`, 8713 B) and `t420_12_row` (`0bf14e1e`, 8760 B) — a
no-op flag would have produced identical bytes, which is exactly what the
sweep's own trap-2 table shows happening at `--sb-size=128`. So **the cell did
carry two tile rows, and the EXACT verdict stands on its own stream.**

Trap 2 is nevertheless real and now has its own reproduction, in the same
report's own words: at `--sb-size=128`, `--tile-rows=1` at 256x128 leaves the
stream byte-identical (`a628e835b8ca1002` for both arms, parsed 1x1 both times).
The flag is a no-op exactly when the frame is one SB row tall.

**Caveat on the published byte counts.** The sweep's 8713/8760-byte streams are
not reproducible from the recipe text in the report (a re-encode of that exact
flag set gives 1675/1747/1785 bytes). Dropping the `gblur` smoothing — the one
degree of freedom the report leaves open — gives 10830/11139/11147 bytes, and
all three of THOSE hit this decoder's named 12-bit screen-content-tools refusal
(`allow_screen_content_tools=1`). The published byte counts are therefore from a
third variant; the structural answer above does not depend on reproducing them.

---

## 2. Cells measured, with the recipe and the parsed geometry

Every row: parse asserted before pixels, decode compared against `aomdec` in
decode order with equal frame count and equal byte length asserted first.
Common aomenc tail: `--codec=av1 --passes=1 --end-usage=q --threads=1 --row-mt=0
--lag-in-frames=0 --kf-max-dist=100 --obu -o - -` (+ `--input-bit-depth=N
--bit-depth=N` above 8). Sources: `testsrc2=WxH:rate=25` unless marked
*smooth* = `mandelbrot=WxH:rate=25` + `-vf gblur=sigma=6` (mandatory at 12 bits,
H6: a sharp 12-bit source sets `allow_screen_content_tools` and the refusal
fires before anything else).

### 2.1 Tile ROWS genuinely enabled (the cell trap 2 left open)

| cell | recipe flags | parsed grid | bytes / sha256 | verdict |
|---|---|---|---|---|
| 4:2:0 12-bit 1x2 rows, 256x256 | `--cq-level=20 --cpu-used=2 --enable-palette=0 --enable-intrabc=0 --tile-rows=1`, *smooth*, 6f | 1x2 | 3725 / `40448841c229d18c` | **EXACT 6/6** |
| 4:2:0 12-bit 2x2 grid, 256x256 | same + `--tile-columns=1` | 2x2 | 3886 / `90fff0c014e954c4` | **EXACT 6/6** |
| 4:2:0 10-bit 1x2 rows, 256x256 | same, yuv420p10le | 1x2 | 16509 / `3f2041bb56f4b6c0` | **EXACT 6/6** |
| 4:2:0 8-bit 1x2 rows, 256x256 | same, yuv420p | 1x2 | 15705 / `7726da064e677fb7` | **EXACT 6/6** |
| 4:4:4 lossless 8-bit 1x2 rows, 256x256 | `--lossless=1 --sb-size=128 --tile-rows=1` + tools off | 1x2 | 111159 / `043e31cc555b5946` | DIVERGENT — §3 |
| 4:4:4 lossless 10-bit 1x2 rows, 256x256 | same, yuv444p10le | 1x2 | 189317 / `562ea414b515f715` | DIVERGENT — §3 |
| 4:4:4 lossless 12-bit 1x2 rows, 256x256 | same, yuv444p12le, *smooth* | 1x2 | 414712 / `63d3b7a9c80dc60b` | DIVERGENT — §3 |
| 4:4:4 lossless 10-bit 1x2 rows, 128x256, sb64 | `--lossless=1 --tile-rows=1` + tools off | 1x2 | 126594 / `491982ec18cc45b9` | DIVERGENT — §3 |
| 4:4:4 lossless 8-bit 2x2 grid, 256x256 | `--lossless=1 --sb-size=128 --tile-columns=1 --tile-rows=1` | 2x2 | 111264 / `d517d33ef232770b` | **EXACT 6/6** |
| 4:4:4 lossless 8-bit 1x2 rows, 128x256, sb128 | `--lossless=1 --sb-size=128 --tile-rows=1` | 1x2 | 68908 / `74c85b982ff75ff5` | **EXACT 6/6** |

`gate_coverage.rs` says the multi-tile 2D grid is covered "at both bit depths"
— meaning 8 and 10. **12-bit was the hole**, and it is now a gate.

### 2.2 4:4:4 superres, and the trap on the same axis as trap 2

`--superres-mode=1` alone does **not** make libaom signal superres: re-encoding
the sweep's own superres recipe (`--superres-mode=1 --cq-level=20
--cpu-used=2`, 256x128) and parsing every frame header gives `use_superres` on
**0 of 6 frames**, at 8, 10 and 12 bits. A denominator is required. With
`--superres-denominator=12 --superres-kf-denominator=12` every frame header
carries `use_superres=1` and the coded width drops (128x128 display -> 85x128
coded), i.e. the flag finally did something:

| cell | recipe | parsed | verdict |
|---|---|---|---|
| 4:4:4 superres 8-bit, 128x128, 4f | `--cq-level=20 --cpu-used=2 --enable-palette=0 --enable-intrabc=0 --superres-mode=1 --superres-denominator=12 --superres-kf-denominator=12` | 85x128 coded, use_superres 4/4 | **EXACT 4/4** |
| 4:4:4 superres 10-bit, same | yuv444p10le | 85x128, 4/4 | **EXACT 4/4** |
| 4:4:4 superres 12-bit, same | yuv444p12le, *smooth* | 85x128, 4/4 | **EXACT 4/4** |
| 4:2:0 superres 12-bit, 128x128, 2f | same, yuv420p12le, *smooth* | 85x128, 2/2 | **EXACT 2/2** |
| 4:4:4 superres 10-bit, 256x256, 4f | same at 256x256 | 171x256, 4/4 | **EXACT 4/4** |
| 4:4:4 superres 12-bit, 256x256, 4f | same at 256x256 | 171x256, 4/4 | **EXACT 4/4** |

The sweep's published 4:4:4 superres verdicts (`sr444` DIVERGENT from f2;
`sr444_12`/`sr420_12` EXACT) are all `--superres-mode=1`-only recipes, i.e.
streams with `use_superres = 0` on every frame: whatever they measured, it was
not superres. With superres genuinely on, all three depths are exact.

### 2.3 4:4:4 partial and odd coded dimensions

The sweep's "odd coded dims" cells (66x66, 130x122, 194x130) are **even in both
axes** — what they are is "not a multiple of 8". This gate said so out loud on
its first run: `the 66x66 arm is even in BOTH axes`. A genuinely odd 4:4:4
coded size is producible **only at 12 bits** (seq_profile 2): at 8 and 10 bits
aomenc rounds the size down to even — `67x67` -> coded 66x66 (byte-identical to
the 66x66 arm, sha `973eddf4aa6b0bf1`) and `131x131` -> coded 130x130.

| cell | coded size (parsed) | bytes / sha256 | verdict |
|---|---|---|---|
| 4:4:4 66x66 8-bit | 66x66 (even, not mult. of 8) | 5938 / `973eddf4aa6b0bf1` | **EXACT 4/4** |
| 4:4:4 98x66 8-bit | 98x66 | 6431 / `fc0e13060d1b7ca7` | **EXACT 4/4** |
| 4:4:4 130x122 8-bit | 130x122 | 9513 / `897791f360e3f3fb` | **EXACT 4/4** |
| 4:4:4 66x66 10-bit | 66x66 | 6006 / `995e6d45ca3e6983` | **EXACT 4/4** |
| 4:4:4 194x130 10-bit | 194x130 | 15751 / `cdb16f6a47306bbc` | **EXACT 4/4** |
| 4:4:4 130x122 10-bit | 130x122 | 10237 / `d34448dd0d44e4e9` | **EXACT 4/4** |
| 4:4:4 66x66 12-bit | 66x66 | 791 / `d266e4281c00eb4f` | **EXACT 4/4** |
| 4:4:4 130x122 12-bit | 130x122 | 1994 / `039d0a5aaabbe230` | **EXACT 4/4** |
| 4:4:4 **67x67 12-bit (odd)** | 67x67 | 844 / `da5d680389656d97` | **EXACT 4/4** |
| 4:4:4 **65x67 12-bit (odd)** | 65x67 | 791 / `471e4458babf7fa2` | **EXACT 4/4** |

Recipe per arm: `testsrc2=WxH:rate=25` yuv444p[10le|12le] (12-bit: *smooth*),
4 frames, `--cq-level=20 --cpu-used=2 --enable-palette=0 --enable-intrabc=0`.

**Correction to a published verdict.** The sweep records 4:4:4 `130x122` as
DIVERGENT from f3 (11521 samples, first f3 s2208 = Y(128,16)). This lane
re-encoded the sweep's own recipe (no `--enable-palette/intrabc`), reproduced its
stream byte-for-byte (8945 bytes, sha `c87ac65b77579afd`) and it is **byte-exact
4/4** against `aomdec` under a length-asserting compare. That published verdict
is the prefix compare the same report documents as its trap 1.

### 2.4 The H3 control: 4:4:4 lossless 8-bit WITHOUT tiles

| arm | parsed grid | bytes / sha256 | verdict |
|---|---|---|---|
| 256x128 untiled (the control the sweep never encoded) | 1x1 | 76468 / `c2cc3bd54d9127a4` | **EXACT 6/6** |
| 256x128 `--tile-columns=1` | 2x1 | 77905 / `1349984ef364c618` | **EXACT 6/6** |
| 256x128 untiled, screen tools left at their default | 1x1 | 74076 / `6c49502811b1a5d8` | **EXACT 6/6** |
| 256x128 `--tile-columns=1`, tools defaulted | 2x1 | 75446 / `65e94e91c56178d2` | **EXACT 6/6** |
| 256x256 untiled | 1x1 | 109215 / `4563a01f17786000` | DIVERGENT — §3 |
| 192x192 untiled | 1x1 | 82918 / `82c3b0a6db9dba2a` | **EXACT 6/6** |
| 192x192 1 tile row | 1x2 | 83105 / `359983962a2c06c3` | **EXACT 6/6** |

**H3 does not reproduce.** On both flag variants at 256x128, the lossless 4:4:4
tile-column stream decodes byte-exact 6/6 — no 90-chroma-sample fork, and the
untiled control is exact too. Whatever produced H3's numbers, this recipe is not
it, so H3's "tiles cause it" has no reproduction on this tree and the control
that was missing now exists as a gate.

---

## 3. Divergences (measured, NOT fixed — each is a charter, not this lane's work)

All of them are the same class: **4:4:4 LOSSLESS, key frame, chroma only, ±1,
luma exactly zero wrong**, at 256x256 or 128x256. Not one of them is
tile-specific.

| cell | untiled control at the same geometry | tiled arm | first fork (decode order) |
|---|---|---|---|
| 4:4:4 lossless 8-bit 256x256 | DIVERGENT f0, 49 samples | 1 tile row: DIVERGENT f0, 33 samples / 2 tile cols: DIVERGENT f0, 14 samples | control `U(201,200)` ours 160 ref 159; 1x2 `U(199,201)` ours 165 ref 164; 2x1 `U(191,178)` ours 171 ref 172 |
| 4:4:4 lossless 10-bit 256x256 | DIVERGENT f0, 7695 samples, first `U(163,36)` ours 209 ref 129 | 1 tile row: DIVERGENT f0, 1374 samples, **first `U(163,36)` — the same sample** | 10-bit |
| 4:4:4 lossless 12-bit 256x256 | DIVERGENT f0, 17336 samples, first `U(154,32)` ours 84 ref 85 | 1 tile row: DIVERGENT f0, 25152 samples, **first `U(154,32)` — the same sample** | 12-bit |
| 4:4:4 lossless 10-bit 128x256 (sb64) | (not encoded) | 1 tile row: DIVERGENT f0, 74 samples, first `U(82,188)` ours 182 ref 165 | 10-bit, sb64 |

**Attribution, stated as a measurement and not a theory.** At 10 and 12 bits the
tiled and untiled streams fork at the *same* first sample, so the tile row is
not the cause and the cell does not add a class; the fork belongs to the
4:4:4-lossless chroma defect the sweep already tracks as H3 / §6.1 and handed
to Kaan-2. **H4 ("4:4:4 lossless + 2 tile ROWS: hard divergence", "the whole
plane comes out flat from frame 3") is refuted on two counts:** its recipe was
256x128 with `--tile-rows=1`, which trap 2 shows is a no-op at that geometry
(so it was an untiled stream), and the genuine tile-row stream here diverges by
tens of samples in the key frame, not by 70-85% of the plane from frame 3. At
8 bits the tiled forks land at three nearby-but-different samples inside the
same bottom-right region, all ±1, all frame 0 — the same family, and a
discriminator run is owed before anyone claims one defect.

The `256x128` control being exact while `256x256` diverges at all three depths
is the sharpest thing this lane measured about the class: it needs a frame that
is 256 rows tall **and** 256 wide, tiled or not.

---

## 4. Gates added (all in `crates/ec-av1/src/stream.rs`)

| gate | arms | what it pins |
|---|---|---|
| `a_real_aomenc_12bit_stream_with_two_tile_rows_and_a_two_by_two_tile_grid_decodes_pixel_exact` | 4:2:0 12-bit 1x2 rows; 4:2:0 12-bit 2x2 grid | the 12-bit multi-tile hole `gate_coverage.rs` left open |
| `a_real_aomenc_444_superres_stream_at_8_10_and_12_bit_decodes_pixel_exact` | 4:4:4 8/10/12-bit, `use_superres` on every frame | 4:4:4 superres at all three depths, the first with superres actually signalled |
| `a_real_aomenc_444_partial_and_odd_coded_dimension_streams_decode_pixel_exact` | 8 arms: 66x66, 98x66, 130x122 (8-bit), 66x66, 194x130, 130x122 (10-bit), 66x66, 130x122, **67x67, 65x67** (12-bit) | 4:4:4 partial-size at 8/10/12 and the first genuinely ODD 4:4:4 coded sizes |
| `a_lossless_444_8bit_untiled_control_and_its_two_tile_column_sibling_decode_pixel_exact` | 256x128 untiled + 256x128 2 tile columns | the H3 control the sweep never encoded, beside the tile-column sibling it was missing |

Shared helper added: `frame_facts` (+ `FrameFacts`), which reads
`tile_cols/tile_rows/use_superres/coded_w/coded_h/upscaled_w/mi_cols/mi_rows`
off every frame header of a parsed stream.

**Non-vacuity.** Each gate asserts, before any pixel: the sequence header's
`ss` and bit depth; the PARSED per-frame `tile_info` equal to the grid the arm
claims (or `use_superres` with `coded_w < upscaled_w` for the superres arms, or
the exact coded size and a partial mode-info grid for the size arms); a
per-path counter (`decode::tile_hits()` >= frames x tiles,
`superres::superres_hits()` >= frames, `decode::rect_split_lossless_chroma444_hits()`
strictly up); the decoded plane extents (256x256, the UPSCALED 128x128, or
exactly `w*h` at `ss (0,0)`), which a truncated or quarter-extent decode fails;
and then `decode_all_frames_vs_oracle`, which asserts equal frame counts and
equal per-frame byte lengths before comparing samples.

**Red-before proofs (all reverted, all reproduced):**

1. tile grid — drop `--tile-rows=1` from the 1x2 arm: RED,
   `decode-order frame 0 parses tile_info 1x1, not 1x2 -- the aomenc tile flag
   was a NO-OP at this geometry`. That is trap 2 caught by the gate.
2. superres — drop both denominators: RED, `decode-order frame 0 has
   use_superres=0 -- the superres cell would be measuring an ordinary 4:4:4
   stream`.
3. coded size — expect `(w+1, h)` instead of `(w, h)` on the 66x66 arm: RED,
   `left: (66, 66) right: (67, 66)`.
4. arm shape — the first run of the size gate, with an "odd in at least one
   axis" assert written before the arm list was checked, went RED on its own
   first arm (`the 66x66 arm is even in BOTH axes`). That is where the
   "odd dims were not odd" correction came from.

**`gate_coverage.rs`.** No entry is closed by these gates and none is deleted:
they spell no `--enable-<tool>=1` (the multi-tile and superres tools carry no
`NEVER_EXERCISED` entry — TILING is deliberately outside that derivation, and
superres is already positively covered through the `--superres-mode` alias by
the three existing superres gates), and they add no new `--enable-*` flag name,
so no new hole can open. `gate_coverage`'s three tests are green on this tree.
Entries 17/18/19 of the census were not touched.

---

## 5. Dispositions (cells that cannot be produced, with the tooling path)

1. **4:4:4 at 8/10 bits with a genuinely ODD coded dimension** — not producible
   by recipe. The tooling path that refuses it is aomenc's own frame-size
   rounding: `67x67` yuv444p encodes to a stream whose frame header says
   `frame_width = 66`, and the bytes are identical to the 66x66 arm
   (`973eddf4aa6b0bf1`); `131x131` -> 130x130. At 12 bits (seq_profile 2) the
   rounding does not happen and 67x67/65x67 stay odd, which is why the odd arms
   are 12-bit. Closing the 8/10-bit odd cell needs a hand-built frame header,
   the same conclusion the sweep reached for 4:2:0 odd luma dimensions.
2. **12-bit 4:2:0 from a sharp source** — refused by name before any pixel:
   `allow_screen_content_tools=1` trips the 12-bit screen-tools refusal even
   with `--enable-palette=0 --enable-intrabc=0` (measured on the no-gblur
   variants, 10830/11139/11147 bytes). Every 12-bit arm in this report
   therefore carries the smoothed source.
3. **4:4:4 lossless 10-bit with tile rows at 128x256 on the default 64px SB** —
   measured, DIVERGENT (§3), not refused: 74 samples, first `U(82,188)`.
4. **4:4:0 (`ss 0,1`)** — carried forward unmeasured from the sweep's §6.4: the
   y4m muxer has no `yuv440p` and aomenc has no 4:4:0 input path. Not
   re-derived here.
5. **The sweep's exact published byte counts** for the 12-bit 4:2:0 cells
   (8713/8760 B) are not reproducible from its recipe text (§1). Disposition:
   recorded, not chased; the structural question is answered without them.

## 6. Handed on

- **Kaan-2 / the 4:4:4 lossless chroma class** gains the sharpest discriminator
  this lane produced: exact at 128x128, 192x192 and 256x128, divergent at
  256x256 at 8, 10 and 12 bits, untiled, key frame, chroma only, +-1 — and the
  tile-row stream forks at the *same* first sample as the untiled control at 10
  and 12 bits. H4 (the tile-rows divergence) should be closed as refuted.
- **The shared `CARGO_TARGET_DIR`** is a measurement hazard for every lane that
  measures or gates `ec-av1` while siblings build it (§0). Worth a house rule.
