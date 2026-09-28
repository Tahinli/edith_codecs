# lane-av1formatsweep — chroma-format coverage sweep

**Scope.** Breadth leg for any future 4:2:2 refusal lift, and a general
capability audit of `crates/ec-av1`: which (chroma format x bit depth x
geometry) cells carry a committed exactness gate, which do not, and for the
uncovered ones that can be witnessed locally, whether the decoder is exact
or divergent.

**Tree.** `lane-av1formatsweep` off `4155c7c7`, worktree
`~/.cache/wt/av1fmt`. Measurements are against a CLEAN HEAD decoder
(`decode.rs` untouched); a parallel lane's in-progress `decode.rs` in
`~/.cache/wt/av1leftref` was measured first and gave identical verdicts, so
nothing here is an artifact of that lane's build.

**Method.** (1) Every `#[test]` in the crate was enumerated (693 total, 285
in `stream.rs`) and each `a_real_aomenc_*` / 4:4:4-named gate resolved to its
shape from the aomenc flag vector its body (or its helper) actually builds.
(2) Uncovered cells were encoded with the durable oracle
`~/.cache/aom-oracle/build/aomenc` and decoded two ways: against
`aomdec`'s `EC_AV1_FINAL_DUMP` (byte-for-byte, decode order) and against
`ffmpeg -f rawvideo`. A cell is called EXACT only when both agree.
(3) Control arms first, to prove the comparison itself is sound: 4:2:0 8-bit
`min-partition=64` and 4:4:4 8-bit lossless `min-partition=64` (the recipe of
the committed `a_lossless_444_min_partition64_inter_stream_decodes_pixel_exact`
gate) both came out exact, so a divergence below is the decoder's, not the
harness's.

## 1. The matrix

Format is the sequence header's `subsampling_x/y` (what the decoder branches
on), not aomenc's `--profile`: profile 2 at 12 bits still emits `ss (0,0)`
for a `yuv444p12le` input, and a true 4:4:0 needs a `yuv440p` input the sweep
never produced.

Legend: **Y** = committed exactness gate; **y** = no gate, measured EXACT this
lane; **D** = no gate, measured DIVERGENT (localized in §3); **R** = refused by
name (no exactness claim possible or wanted); **–** = not measured.

### 4:2:0 (`ss 1,1`)

| cell | 8-bit | 10-bit | 12-bit |
|---|---|---|---|
| even dims, key+inter | Y (≈120 gates) | Y (≈40 gates) | Y (4 gates) |
| **odd coded dims** (`w%8≠0` or `h%8≠0`) | **y → gate A** | **y → gate A** | **y → gate A** |
| partial-superblock edge (72, 136, …) | Y (`tiny_frame_size_sweep`) | Y | – |
| tile columns / rows | Y (2 and 4, both axes) | Y (multi-tile family) | – |
| superres | Y | Y | – |
| film grain / warp / CDEF+LR | Y | Y | Y |

Odd coded dimensions had **no committed gate at any depth**: every frame size
in the crate (the tiny-size ladder included — 8, 16, 24, 32, 48, 64, 72, 136,
192, 200…248) is a multiple of 8, so the partial-chroma-plane walk had no
exactness evidence. Measured exact, and gated.

### 4:4:4 (`ss 0,0`) — the whole committed base is LOSSLESS 8-bit

| cell | 8-bit | 10-bit | 12-bit |
|---|---|---|---|
| lossless | Y (8 gates, `ll444_*` pins) | **y → gate B** | R¹ / – |
| **lossy** | **D (H1, H2)** | **D (H1)** | **y → gate C** |
| tile columns (2) | **D (H3)** | – | – |
| tile rows (2) | **D (H4)** | – | – |
| superres | – | – | – |
| odd coded dims | – | – | – |

¹ 12-bit 4:4:4 is reachable only from a source smooth enough that aomenc
leaves `allow_screen_content_tools = 0`; otherwise the 12-bit screen-tools
refusal fires first (H6). The smooth-source 12-bit 4:4:4 path is exact
(gate C). The smooth-source 12-bit **lossless** 4:4:4 stream was not measured.

### 4:2:2 (`ss 1,0`) and 4:4:0 (`ss 0,1`)

| cell | verdict |
|---|---|
| any depth, any geometry, committed | **R** — `subsampling_x != subsampling_y` refuses by name in `decode_frame` before any tile dispatch |
| committed 4:2:2 fixtures | 7 pins (`422_allskip_2f`, `422_sb128_3f`, 2x `422_intrabc_sb128_strip*`, 2x `422_residual_compound_warp*`, `422_residual_compound_warp_nolr_16f`); every committed gate asserts **refusal**, not pixels |
| odd coded dims, 8-bit, lossy inter | **y, probe-bypass only** (130x122, 66x66, 194x130 — 4 frames each, all three planes byte-exact vs ffmpeg) |
| odd coded dims, 10-bit | **y, probe-bypass only** (130x122) |
| 2x2 tiles, 8-bit | **y, probe-bypass only** (130x122) |
| 4:4:0 | – (no witness produced; refused by the same clause anyway) |

The bypass is a local patch (`EC_AV1_ALLOW_422_PROBE` added to the refusal
condition), applied, measured, and reverted in the same session;
`grep EC_AV1_ALLOW_422_PROBE crates/ --include=*.rs` at HEAD still hits only
doc comments. The refusal stays.

**This is the single most useful line in the report for a 4:2:2 lift:** the
lossy 4:2:2 inter path — odd extents, 10-bit, multi-tile — decodes
**pixel-exact** wherever the bypass admits it. The lift's risk is not in the
odd-dimension or tile geometry; it is in the shapes already named in
`refusal_inventory.rs` (rect chroma units, 64-side mu-chunk walks, sub-8
pairs), which the committed 4:2:2 pins exist to witness.

## 2. Gates added (all in `crates/ec-av1/src/stream.rs`)

Each is live-aomenc (no new fixture pin needed: the recipe is deterministic —
single pass, single thread, no row-mt — and the aomenc recipe is quoted in
the doc comment), compares byte-for-byte against the instrumented `aomdec`
through `decode_all_frames_vs_oracle`, and hard-asserts the shape it claims.

1. **`a_real_aomenc_odd_coded_dimension_streams_decode_pixel_exact`** — six
   arms: 130x130, 66x66, 194x122, 130x122 at 8-bit; 130x122 at 10-bit;
   130x122 at 12-bit (smoothed `mandelbrot` source). Non-vacuity: the header
   must really be `ss (1,1)` at the claimed depth, the mode-info grid must
   be partial (`mi*8 > dim`), every decoded frame's chroma plane must be
   `ceil(w/2)*ceil(h/2)`, and `decode::inter_edge_strip_hits()` must be
   non-zero after a per-arm reset. Measured per-arm edge walks:
   `[0,6,0,4,48,12]`, `[0,0,0,4,0,6]`, `[0,4,0,10,0,10]`, `[0,4,0,8,0,12]`,
   `[0,4,0,8,0,8]`, `[0,0,0,12,0,14]` — the even-size control (128x96) reads
   all zeros, so the counter is specific to the partial-column walk.
2. **`a_lossless_444_10bit_inter_stream_decodes_pixel_exact`** — 4:4:4
   lossless at 10-bit, 6 frames, byte-exact vs aomdec. Non-vacuity: the
   header must say `ss (0,0)` at 10-bit and
   `decode::rect_split_lossless_chroma444_hits()` must fire (measured 32
   units; a 4:2:0 stream of the same content leaves it at 0). The recipe is
   pinned to `--cpu-used=2`; see H5 for the `--cpu-used=0` sibling.
3. **`a_444_12bit_inter_sequence_decodes_pixel_exact`** — 4:4:4 at 12-bit,
   6 frames, byte-exact vs aomdec. Non-vacuity: header `ss (0,0)` at 12-bit,
   `mc::mc_subpel_hits()` must fire (the 12-bit round pair), and every
   decoded frame's chroma plane must be full resolution. CDEF is
   deliberately NOT asserted: on this smoothed source CDEF runs but skips
   every band (`cdef_band: rect_skip_writes=11`, zero index literals), so
   such an assertion would be a claim the witness cannot carry.

Shared helpers added: `odd_dim_stream` / `chroma_format_stream` (ffmpeg y4m →
aomenc pipe, deadline from `run_with_stdin`), `odd_dim_header_mi_grid`,
`assert_444_header`.

**Honest gap in the proof:** the odd-dimensions gate's counter and geometry
assertions are in place, but its red-before (mutation) proof is NOT
complete. Forcing `decode::round_ss` from libaom's `ROUND_POWER_OF_TWO` to a
floor division left the gate GREEN — the odd-dimension extents are computed
from the mode-info grid (`mi_cols*4 >> ss`), not through `round_ss`. A
mutation inside that extent computation is the outstanding proof. Gates B and
C inherit the same `aomdec` byte-compare proof as the 180 existing gates, and
no mutation proof either.

## 3. Divergences, localized

### H1 — 4:4:4 lossy + a RECT partition: entropy fork (the big one)

Minimal repro: `aomenc --codec=av1 --profile=1 --passes=1 --end-usage=q
--cq-level=20 --cpu-used=2 --threads=1 --row-mt=0 --lag-in-frames=0
--kf-max-dist=100 --limit=2` over `testsrc2 128x96 yuv444p`, 6441 bytes.
Frame 0 (key) is exact; frame 1's first wrong sample is **Y(91,32)**
(ours 35, ref 38) and 8884 of 36864 samples are wrong by frame 1.

Bisect: `--enable-rect-partitions=0` → **exact**; `--enable-ab-partitions=0`
→ still divergent (AB partitions are innocent); `--min-partition-size=32`
and `=64` → exact; CDEF / loop restoration / deblock / OBMC / warp toggles
change nothing; the same recipe at 4:2:0 is exact.

First divergence, `EC_SYMR` on both sides (ours: `msac.rs` `SymbolDecoder::
symbol`; oracle: `aom_dsp/bitreader.h` `aom_read_symbol_`), one line per
symbol read in decode order, aligned on `(value, range, symbol, post_rng)`
with the CDF-row convention reconciled (`32768 - ours_icdf0 == oracle_cdf0`;
the `bit` field runs a constant +15 offset, 26219 of the first 26263 reads):

```
index 26262  both  mi=(8,16)  decodetxb.c:368 (dc_sign)   s=1  pre=(13550,51360) post=58008
index 26263  ORACLE mi=(8,16) decodetxb.c:158 txb_skip     s=1  cdf0=16519  n=2  pre=(27101,58008) post=58316
index 26263  OURS   mi=(8,16) (read_coeffs_rect)          s=0  icdf0=22556 n=2  pre=(27101,58008) post=40037
```

`decodetxb.c:158` is the `txb_skip` read. `EC_COEFF_STEP` names the unit:

```
[1252] OURS   plane=0 ctx=0 entry=61254 all_zero=0
[1253] ORACLE plane=1 bc=0 br=0 ctx=7 txs=1 pt=1 all_zero=1   <- the U plane
[1253] OURS   plane=0 ctx=1 entry=58008 all_zero=0            <- a second LUMA rect unit
```

(The `plane=0` label on our side is hardcoded in `read_coeffs_rect`'s trace
line — a label bug, not a fact; the read site is the rect walk.) So at
4:4:4, where the chroma plane block is 1:1 with luma, the rect unit walk
emits a LUMA unit where the oracle moves to the chroma plane, and the fork
propagates from there. The first *pixel* divergence is a block later
(the block at mi(8,16) is a skip/zero-residual block, so its own pixels
survive the wrong read). `decode.rs:11602` (`span_x = 4 << ss_x`) is the
4:4:4 unit geometry to look at first, and the txb_ctx chroma base
(7 vs 10, `txb_common.h` ~352, already flagged in
`skill://ec-av1-chroma-arm-oracle-verify`) is the second candidate.

### H2 — 4:4:4 lossy + TX size search: chroma-only ±1, NO entropy divergence

Recipe: profile 1 lossy cq 20, `--sb-size=64 --min-partition-size=64
--max-partition-size=64` (whole 64x64 roots, no rect splits), 4 frames.
Frame 0 and 1 exact; from frame 2, 608 of 36864 samples wrong — **U and V
only, luma exactly zero wrong**, magnitude ±1 (up to ±20 at cq 40).

`EC_SYMR` on both sides: **140820 reads, zero divergence** — the entropy
decode is bit-identical, so this is pure reconstruction arithmetic, not a
fork. `--enable-tx-size-search=0` → exact; every filter toggle changes
nothing. Prime suspect is the chroma transform-unit shape at `ss (0,0)`: at
4:2:0 the chroma rect is halved and a `TX_32X64` chroma unit cannot arise,
while at 4:4:4 it can, and libaom clamps it —
`max_txsize_rect_lookup[BLOCK_32X64] = TX_32X64` then
`av1_get_adjusted_tx_size` (`blockd.h` ~1363) reduces TX_32X64 to TX_32X32.

### H3 — 4:4:4 lossless + 2 tile columns: 90 chroma samples wrong in the key frame

256x128 `testsrc2 yuv444p`, `--lossless=1 --tile-columns=1`, 6 frames,
aomdec-confirmed (not an ffmpeg artefact). Frame 0: exactly 90 wrong
samples, U and V, ±1, first at U(237,58). Frames 1–5: byte-exact.

### H4 — 4:4:4 lossless + 2 tile ROWS: hard divergence

Same recipe with `--tile-rows=1`. Frames 0–2 exact; frame 3 onwards the whole
plane comes out flat (Y(0,0) ours 128 vs ref 81, then ~70% of all samples
wrong, growing to 85% by frame 5). Worst verdict in the sweep.

### H5 — 4:4:4 lossless at 10-bit, `--cpu-used=0`: 128-root partition diverges

The gate-B recipe at `--cpu-used=0` makes aomenc reach the 128-root
partition (`part128: split=1 none=5`, where `--cpu-used=2` reads
`none=0`). That stream diverges from frame 1: 11152 of 36864 samples wrong,
first at Y(8,4), with **ffmpeg and aomdec agreeing** on the reference. The
committed 8-bit 4:4:4 gates cover 128 roots (`a_444_sb128_root_rect_stream_
with_restoration_decodes_pixel_exact`), but not at 10 bits.

### H6 — coverage caveat, not a defect: 12-bit 4:4:4 needs a smooth source

`aomenc --input-bit-depth=12` over `testsrc2 yuv444p12le` sets
`allow_screen_content_tools = 1` even with `--enable-palette=0
--enable-intrabc=0`, and the 12-bit screen-tools refusal fires by name. With
a `-vf gblur=sigma=6` source the same recipe decodes and is exact. Any
future 12-bit 4:4:4 witness has to carry that source, or it will measure the
screen-tools refusal and call it a 4:4:4 result.

## 4. Not measured (budget), stated so nobody reads silence as coverage

12-bit tiles at 4:2:0; 12-bit superres; 4:4:4 superres; 4:4:4 odd coded
dimensions; 4:4:4 at 10/12-bit with tiles; 12-bit 4:4:4 lossless; 4:4:0
(`ss 0,1`) at any depth. Suite staging and the full-suite run are Main's.

## 5. Handoffs, in the order a follow-up lane should take them

1. **H1** rect partition at 4:4:4 lossy — the largest correctness gap in the
   sweep; entropy fork at mi(8,16), localized to the chroma/luma unit walk.
2. **H4** 4:4:4 lossless + tile rows — hard failure, cheapest to bisect
   (single-tile-row frame-edge walk).
3. **H5** 4:4:4 10-bit lossless 128-root — a cell the 8-bit 444 gates
   suggest is safe and is not.
4. **H2** 4:4:4 lossy + tx size search chroma ±1 — reconstruction-only,
   entropy-clean, so the `av1_get_adjusted_tx_size` clamp is the first thing
   to check.
5. **H3** 4:4:4 lossless + tile columns, 90 chroma samples — smallest
   magnitude, likely the same root as H2.

For the **4:2:2 lift** the sweep's message is mostly good news: the odd-
dimension, 10-bit and multi-tile 4:2:2 paths are already pixel-exact under
the bypass. Start from the shapes `refusal_inventory.rs` still names, and use
the bypass recipe in `skill://ec-av1-422-probe-bypass` to widen the committed
422 pins from "refuses by name" to "refuses by name, and is exact when
admitted" for at least the lossless big-block case.
