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

**Cross-check from the parallel lane (lane-av1leftref, the ss_y left-chroma
reference fix; landed as `83b6af97`).** That lane reports main and its tip byte-identical on all 35
committed fixtures that decode at all (4:2:0, 4:4:4, 8/10/12-bit), with the
six 4:2:2 pins decoding zero frames in both trees because they need the
uncommitted bypass. That matches what this sweep measured independently: no
verdict in §1 moved between the two builds. For 4:2:2 the change is a no-op by
construction anyway — libaom's read offset is `ss_y`, and 4:2:2 has
`ss_y = 0`, so the cell read is the same one.

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
| **even, not a multiple of 8** (`w%8≠0` or `h%8≠0`, both even) | **y → gate A** | **y → gate A** | **y → gate A** |
| luma dimension itself ODD | not producible¹ | not producible¹ | not producible¹ |
| partial-superblock edge (72, 136, …) | Y (`tiny_frame_size_sweep`) | Y | – |
| tile columns / rows | Y (2 and 4, both axes) | Y (multi-tile family) | – |
| superres | Y | Y | – |
| film grain / warp / CDEF+LR | Y | Y | Y |

¹ **Neither gateable nor refutable from this recipe:** aomenc rounds a 4:2:0
frame's coded size DOWN to even, so no stream it produces from a lavfi source
codes an odd luma dimension at all. Verified: a `testsrc2 131x131` yuv420p
y4m (ffmpeg accepts it) encodes to a sequence header of `max_frame=130x130`,
which this decoder decodes correctly; the ffmpeg reference for the same file
comes back 131x131 because ffmpeg pads to the y4m header. Closing that cell
needs a hand-built stream (the `Av1Parser` header writer the sweep's own
`every_frame_size_a_header_can_code_has_a_mode_info_grid` gate already uses),
not an aomenc recipe.

Even non-multiple-of-8 dimensions had **no committed gate at any depth**:
every frame size in the crate (the tiny-size ladder included — 8, 16, 24, 32,
48, 64, 72, 136, 192, 200…248) is a multiple of 8, so the partial-chroma-plane
walk had no exactness evidence. Measured exact, and gated.

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

**Red-before proof for gate A (done).** The mutation that bites is the
mode-info grid's ceil: `ec-av1-syntax`'s
`h.mi_cols = 2 * ((h.frame_width + 7) >> 3)` floored to
`2 * (h.frame_width >> 3)` turns the gate red on the first arm — decode-order
frame 0, byte 128, ours 74 vs oracle 170, 11538 bytes differ — and green again
on revert.

**Why the nearer mutation was the wrong one.** Flooring `decode::round_ss`
(libaom's `ROUND_POWER_OF_TWO`) leaves the gate GREEN, and that is not a gap
in the gate: aomenc rounds a 4:2:0 frame's coded size DOWN to even, so no
stream this recipe can produce codes an odd luma dimension, and the
chroma-extent ceil only differs from its floor when the luma dimension is
itself odd. Verified directly: a `testsrc2 131x131` yuv420p y4m (ffmpeg
accepts it) comes out of aomenc as a stream whose sequence header says
`max_frame=130x130` — and this decoder decodes that 130x130 frame correctly.
The ffmpeg reference for the same file comes back 131x131 (25873 samples
against our 25350), which is ffmpeg padding to the y4m header, not a decoder
defect. **Consequence for the matrix:** the "luma dimension itself odd" cell
is not producible through the aomenc + lavfi recipe, so it can be neither
gated nor refuted from here; the sweep's odd-dimension cell is "even, not a
multiple of 8".

**Declared debt:** gates B (4:4:4 lossless 10-bit) and C (4:4:4 12-bit)
carry NO targeted mutation proof. They inherit the `aomdec` byte-compare the
180 existing gates run on, plus their own header/counter non-vacuity asserts
(`rect_split_lossless_chroma444_hits()`, `mc_subpel_hits()`, the full-resolution
chroma extent). A future lane that touches the 4:4:4 unit geometry should
spend a mutation on them; the shape of the proof is the one in gate A's doc
comment — mutate the site the CELL depends on, not a nearby helper.

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
nothing.

**Round 2 (this lane, deeper).** The pre-filter dumps exonerate the whole
filter chain: `EC_AV1_PREFILT_DUMP` on both sides shows frame 2 already
carries 847 wrong CHROMA samples (luma 0 wrong) BEFORE deblock, CDEF and LR.
The oracle's per-unit census (`EC_COEFF plane=… row=… col=… tx_size=…`) shows
every chroma unit in this stream is `tx_size=3` (TX_32X32) at mi_row 0 / 0 and
mi_row 16, mi_col 0 and 8 — i.e. the `av1_get_adjusted_tx_size` clamp the
first suspect pointed at is ALREADY being applied correctly on both sides
(`decode_inter_block`'s 4:4:4 arm computes `if side >= 64 { 32 }`), and there
is no chroma unit at mi_row 8 at all. The wrong pixels are exactly the left
column of chroma units of the BOTTOM, frame-edge PARTIAL block row (frame is
96 high, blocks 64: the second block row has 32 real luma rows, so its chroma
band is 32 rows at `ss (0,0)` where 4:2:0 would give 16). Both sides walk the
same units with the same coefficients, so the defect is in what happens to
that partial block's chroma — its `unit_height` /
`ROUND_POWER_OF_TWO` clamp at `ss 0` (decodeframe.c:989) or its above/left
prediction reach at the frame edge — not in the transform-size rule.

**Round 3 (this lane) — THE ROUND-2 LOCALIZATION IS REFUTED, RETRACTED.**
Geometry controls kill it:

| stream | frame grid | verdict |
|---|---|---|
| 128x96 (the H2 stream) | 1.5 x 2 block rows — partial bottom row | divergent, U+V, from f2 |
| **128x128** | **2 x 2 whole 64 blocks — NO partial row, NO partial column** | **divergent, V only, 777 samples from f1** |
| 160x96 | 2.5 x 2 — partial bottom row only | divergent, V only, from f3 |
| 96x96 | partial row AND partial column | divergent, U+V, from f1 |
| any of the above with `--enable-tx-size-search=0` | — | **exact** |

Candidate (a), the partial block's `unit_height` / `ROUND_POWER_OF_TWO` clamp
at ss 0, is refuted: a frame with no partial block anywhere reproduces the
defect, and a wrong clamp would change the unit COUNT, which the
bit-identical 140820-symbol entropy trace already excludes. Candidate (b) is
also refuted *as an edge class* — the errors appear mid-block (first wrong
sample V(27,60) in the 128x128 control, y=60, inside the first block row) and
the affected extent tracks no frame edge. What survives is narrower and not
geometry at all: **4:4:4 lossy inter with tx size search ON, chroma only,
always ±1, luma always exact, entropy bit-identical, present before every
filter, and the plane that errs varies with content** (V only at 128x128, U
and V at 128x96/96x96).

Named next step (one run, and it is the measurement that finally splits
dequant from transform): take ONE affected 32x32 chroma unit and compare our
dequantized coefficients against the oracle's `EC_COEFF_VAL` (and ours via
`EC_DQCOEFF`). Values already different => the chroma dequant at `ss (0,0)`
is the fault. Values equal but pixels differ => the fault is in the 32x32
inverse transform / residual add.

### H3 — 4:4:4 lossless + 2 tile columns: 90 chroma samples wrong in the key frame

256x128 `testsrc2 yuv444p`, `--lossless=1 --tile-columns=1`, 6 frames,
aomdec-confirmed (not an ffmpeg artefact). Frame 0: exactly 90 wrong
samples, U and V, ±1, first at U(237,58). Frames 1–5: byte-exact.

**Round 2 (this lane).** Also pre-filter: the same `EC_AV1_PREFILT_DUMP`
pair shows all 90 samples wrong BEFORE deblock/CDEF/LR, luma 0 wrong. Frames
1–5 are byte-exact even though they predict from that same key frame — so the
wrong samples are never read as a prediction source.

**Round 3 (this lane) — the output/crop and reference-store candidates are
REFUTED.** The `EC_AV1_FINAL_DUMP` rung writes the picture exactly as it is
handed to the reference slots, and diffing it against the oracle's rung 12
gives the SAME 90 chroma samples wrong in frame 0 (first at U(237,58), luma 0)
with frames 1–2 byte-exact. The error is therefore already in the stored
reference picture: not introduced by the output crop, not introduced by the
reference-store path. It is the key frame's own chroma reconstruction, and the
fact that it never propagates is a property of which samples the inter frames
happen to predict from, not evidence of a healthy path.

**H2 and H3 still do not reduce to one defect.** Both are pre-filter,
chroma-only, ±1, and present in the stored picture — but they run different
unit paths: H2 is the LOSSY 32x32 chroma unit and needs tx size search ON,
H3 is the LOSSLESS per-4x4 chroma unit (the walk
`rect_split_lossless_chroma444_hits` counts) and fires with tx search
irrelevant. No single fix is claimed for them.

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
   entropy-clean, filter chain exonerated, tx-size clamp exonerated, and
   (round 3) the frame-edge geometry exonerated too: a 128x128 control with
   whole blocks reproduces it. What is left is the chroma unit's own
   arithmetic at `ss (0,0)`, gated on tx size search being ON. One
   `EC_DQCOEFF` vs `EC_COEFF_VAL` comparison on a single affected 32x32 unit
   splits dequant from inverse transform.
5. **H3** 4:4:4 lossless + tile columns, 90 chroma samples — smallest
   magnitude, likely the same root as H2.

For the **4:2:2 lift** the sweep's message is mostly good news: the odd-
dimension, 10-bit and multi-tile 4:2:2 paths are already pixel-exact under
the bypass. Start from the shapes `refusal_inventory.rs` still names, and use
the bypass recipe in `skill://ec-av1-422-probe-bypass` to widen the committed
422 pins from "refuses by name" to "refuses by name, and is exact when
admitted" for at least the lossless big-block case.

---

## 6. Round 4 (lane-av1444chr) — the "not measured" list, closed

**Tree.** `lane-av1formatsweep` (this branch), append-only, no code change.
Every verdict below is against the **durable instrumented aomdec**
(`EC_AV1_FINAL_DUMP`, decode order), **not** ffmpeg, and every compare
asserts (a) the two frame COUNTS are equal and (b) each frame's byte length
is equal, before any sample is looked at. That is not pedantry: two of the
twelve cells below first read DIVERGENT against an ffmpeg reference and
turned out EXACT against aomdec (round-4 finding F, below).

### 6.0 The two measurement traps this round hit (read before trusting any sweep number)

**Trap 1 — a compare that truncates is a PREFIX compare.** A per-frame
compare zipped against `min(len(ours), len(ref))` passes exactly when the
defect lies outside the prefix; the 4:4:4 lossless defect sits at x >= 108,
outside the 128x96 window a hardcoded geometry produces. This is how a
"byte-exact 6/6" was reported for a stream that is wrong in every frame. The
prefix compare must assert equal lengths first.

**Trap 2 — an aomenc flag is not evidence the geometry changed.**
`--tile-rows=1` at 256x128 is a **no-op**: with a 128px superblock the frame
is a 2x1 SB grid, so there is no second SB row to tile. Verified by bytes,
not by the flag:

| stream | untiled sha256 | with `--tile-rows=1` | verdict |
|---|---|---|---|
| 4:4:4 lossless 10-bit, 256x128 | `ec7a6a0ed9d7c2bada8816bb692de3379620856644a462f287a6cc3e6d9c5103` | **identical** | flag had NO effect |
| 4:4:4 lossless 12-bit, 256x128 | `ef7338136d560674677cb5fd903e60906ef4df7b2f4c93a990e65a5b5ed4402c` | **identical** | flag had NO effect |

So this round's 10- and 12-bit "tile rows" cells are NOT tile witnesses; what
they actually measure is the **untiled** 4:4:4 lossless stream at those depths,
which is itself divergent (6.1). A real 4:4:4 tile-rows witness needs a
geometry with two SB rows (>= 256 high at `--sb-size=128`).

**Trap 3 — the reference can be the outlier.** Against `ffmpeg
-pix_fmt yuv420p12le` the 12-bit 4:2:0 cells read as ~8175/8192 chroma
samples wrong in every frame. Against aomdec the same streams are **byte-exact
6/6**. ffmpeg's raw 12-bit 4:2:0 output is not the reference for this cell.

### 6.1 4:4:4 at 10 and 12 bit — LOSSY tile cells and the untiled control

Source `testsrc2=size=256x128:rate=25` yuv444p / yuv444p10le; the 12-bit runs
need a smooth source (`mandelbrot` + `gblur=sigma=6`) or
`--enable-palette=0 --enable-intrabc=0`, else the 12-bit screen-tools refusal
fires first (H6). Common flags: `--lossless=1 --input-bit-depth=N
--bit-depth=N --codec=av1 --passes=1 --end-usage=q --threads=1 --row-mt=0
--lag-in-frames=0 --kf-max-dist=100 --limit=6 --obu -o - -`.

| cell | stream | bytes | sha256 | verdict vs aomdec |
|---|---|---|---|---|
| 4:4:4 10-bit lossless, **no tiles** | `t444_10_notile` | 122730 | `ec7a6a0e…` | **DIVERGENT** from f1, 290506 samples, first (f1, s128) = Y(128,0) |
| 4:4:4 10-bit + **2 tile columns** | `t444_10_col` | 122937 | `2d295c2c…` | **DIVERGENT** 248394 samples; f0 first (f0, s47853) = U(229,58) |
| 4:4:4 12-bit lossless, **no tiles** | `t444_12_notile` | 171599 | `ef733813…` | **DIVERGENT** from f0, 264180 samples, first (f0, s32877) = U(109,0) |
| 4:4:4 12-bit + **2 tile columns** | `t444_12columns` | 172340 | `06fb4a8d…` | **DIVERGENT** from f0, 174949 samples, **same first sample** (f0, s32877) = U(109,0) |

Reading: the 10-bit tile-column stream's frame 0 carries the H3 signature
(7 U + 11 V wrong against ffmpeg, first wrong U(237,58) — the sweep's H3 first
divergence, reproduced at 10 bits), then diverges hard from frame 1. The 12-bit
cell diverges from **frame 0** with the first wrong sample at U(109,0) whether
or not tiles are on — i.e. at 12 bits the 4:4:4 lossless key-frame defect is
present **untiled**. So the class is **not** depth-independent and **not**
tile-specific: it is a 4:4:4 lossless chroma defect that appears at 8, 10 and
12 bits, with or without tiles. None of these reduce to H1 (entropy fork —
H2's stream is entropy-clean) or to H2 (lossy, tx-search-gated).

### 6.2 4:4:4 superres and odd coded dimensions (8-bit lossy, cq 20, cpu-used 2)

| cell | stream | bytes | sha256 | verdict vs aomdec |
|---|---|---|---|---|
| 4:4:4 **superres** 256x128 (`--superres-mode=1`) | `sr444` | 15239 | `06621606…` | **DIVERGENT** from f2, 76404 samples, first (f2, s192) = Y(192,0) |
| 4:4:4 **odd coded dims 66x66** | `odd444_66x66` | 5938 | `973eddf4…` | **EXACT 4/4** — new exact cell |
| 4:4:4 **odd coded dims 130x122** | `odd444_130x122` | 8945 | `c87ac65b…` | **DIVERGENT** from f3, 11521 samples, first (f3, s2208) = Y(128,16) |

Note the aomenc flag is `--superres-mode=1`; there is no `--enable-superres`
in this build (`aomenc --help | grep -i superres`).

The 130x122 stream is the interesting one: 66x66 is exact at the same
settings, so the 4:4:4 partial-frame walk is right at one odd geometry and
wrong at another, and the first divergence is at (128,16) — inside the frame,
not on its edge. Same shape of question as H2's retracted edge theory; **not
claimed to be the same defect** without a discriminator run.

### 6.3 12-bit 4:2:0 tiles and superres — the two cells the matrix left open

Source `mandelbrot=size=256x128:rate=25` yuv420p12le (smooth, or the 12-bit
screen-tools refusal fires), `--enable-palette=0 --enable-intrabc=0
--input-bit-depth=12 --bit-depth=12 --profile=0 --cq-level=20 --cpu-used=2`.

| cell | stream | bytes | sha256 | verdict vs aomdec |
|---|---|---|---|---|
| 4:2:0 12-bit + 2 tile columns | `t420_12_col` | 8713 | `c983a91c…` | **EXACT 6/6** |
| 4:2:0 12-bit + 2 tile rows | `t420_12_row` | 8760 | `0bf14e1e…` | **EXACT 6/6** (flag effect at this geometry NOT verified — see caveat) |
| 4:2:0 12-bit superres | `sr420_12` | 8494 | `9e8206bf…` | **EXACT 6/6** |

Caveat, stated so the next round does not inherit it: at 256x128 `--tile-rows`
was a proven no-op for 4:4:4, and I did **not** encode an untiled 4:2:0
control here, so `t420_12_row` may be measuring the same untiled stream as
`t420_12_col`. The EXACT verdict is safe either way (it is exact against the
oracle); what is unproven is that it exercised tiles.

### 6.4 4:4:0 (`ss 0,1`) — NOT PRODUCIBLE through aomenc

The y4m muxer refuses the format outright: `yuv4mpeg can only handle yuv444p,
yuv422p, yuv420p, yuv411p and gray8`. A raw `yuv440p` plane dump reaches
aomenc, which has no 4:4:0 input path and falls back to 4:2:0
(`Profile 1 requires 4:4:4 color format`; there is no `--input-format` flag
in `--help`). So the `4:4:0 (ss 0,1)` row stays **unmeasured and
unproducible by recipe**, for the same reason the 4:2:0 odd-luma-dimension row
does: closing it needs a hand-built sequence header, not an aomenc run. The
decoder's refusal (`subsampling_x != subsampling_y` refuses by name in
`decode_frame`) is unchanged and still the honest answer for any 4:4:0 stream
that is ever hand-built.

### 6.5 Matrix deltas and the honest "not measured" list

Matrix changes (§1): 4:4:4 row gains `odd dims 66x66 → y` (exact this lane),
`superres → D`, `10/12-bit tiles → D`, `12-bit lossless → D`; 4:2:0 row gains
`12-bit tiles + superres → y`. The tile-ROWS cells for 4:4:4 stay as they
are — the 8-bit one is now Mustafa's EXACT (tile rows genuinely enabled at
geometries with a second SB row), and the 10/12-bit ones are the untiled
control above.

Still not measured after this round: 4:4:4 at 10/12 bits with **tile rows
actually enabled** (needs a >= 256-high geometry — trap 2); 4:4:4 superres at
10/12 bits; 4:4:4 odd dims at 10/12 bits and at dimensions other than 66x66 /
130x122; 4:4:4 lossless at 8 bits **without** tiles as a control for the H3
witness; 4:4:0 at any depth (unproducible, §6.4); 4:2:2 (ss 1,0) beyond the
existing probe-bypass row.

### 6.6 Handed to other lanes

- The 4:4:4 **lossless chroma** divergences at 8/10/12 bits with and without
  tiles (first wrong sample U(109,0) at 12 bits, U(237,58) at 8 bits) go to
  **Kaan-2** with H3 — same format, same unit family, and this round shows the
  class is not tile-specific and not depth-specific, which narrows it.
- The 4:4:4 **130x122** odd-dimension divergence and the 4:4:4 **superres**
  divergence are unassigned; neither reduces to a known class on the evidence
  here, and neither has a discriminator run.
- Recorded from Mustafa-2 and folded into the matrix above: a hardcoded 4:2:0
  luma footprint at the lossless 16x4/4x16 chroma-pair walk
  (`Reach::of_tu` mis-answering at `ss 0,0`), same shape class as the wave's
  other 4:4:4 bugs; its stream family is exact after the fix.
