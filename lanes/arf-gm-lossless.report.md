# lane-arf-gm-lossless — 4:2:0 LOSSLESS + alt-ref + global motion DIVERGES

Branch `lane-arf-gm-lossless`, base `aef4fa67`. FIXED (one line of arithmetic in
`record_mi_chroma`, plus a route counter and a pinned gate).

## 1. The four-configuration table, re-measured (320x240 testsrc2, 16 frames, lossless)

Recipe: `ffmpeg -f lavfi -i testsrc2=size=320x240:rate=25 -frames:v 16 -pix_fmt yuv420p y.yuv`
then `aomenc --codec=av1 --obu --lossless=1 -w 320 -h 240 <knobs> -o x.obu y.yuv`.

| lag | alt-ref | global-motion | sha256 (first 16) | bytes | pre-fix | post-fix |
|---|---|---|---|---|---|---|
| 0  | 0 | 0 | 796afea2597f7b86 | 107094 | EXACT 16/16 | EXACT 16/16 |
| 0  | 1 | 0 | 796afea2597f7b86 | 107094 | EXACT 16/16 | EXACT 16/16 |
| 25 | 0 | 0 | 8c7f117b51097df1 | 113465 | EXACT 16/16 | EXACT 16/16 |
| 25 | 1 | 0 | 065cdfb5d4212b19 | 87075  | **DIVERGES** (1242219 wrong) | EXACT 17/17 |
| 25 | 1 | 1 | b8eea5dc679f50df | 86649  | **DIVERGES** (1088573 wrong) | EXACT 17/17 |

Full shas: L0_A0_G0 = L0_A1_G0 = `796afea2597f7b8665b11ff16eb2b7a3fa7e7acd02fdfdcd1355bd8290edae8a`,
L25_A0_G0 = `8c7f117b51097df1ecf1d41660e1950bc6cbf9b19c41c4859ecd79794baeab0f`,
L25_A1_G0 = `065cdfb5d4212b195460709ade77f1ceb4caf3948777df81cd243a1afc688afc`,
L25_A1_G1 = `b8eea5dc679f50dfaae99da09011b123d143a7d9b60e14be74683f6c366f2cfb`.

**Two corrections to the ticket's premise, both measured:**
1. Global motion is NOT the trigger. `--auto-alt-ref=1 --enable-global-motion=0`
   diverges too (and by MORE: 1 242 219 wrong samples vs 1 088 573). Every GM model in
   the frame headers is `Identity`, so no global motion is applied at all.
2. The "ARF alone refuses" cell does not refuse at 16 frames. It DIVERGES, exactly
   like ARF+GM. The *refusal* appears in the neighbouring cells that a single encoder
   knob moves: `--sb-size=64`, `--enable-dual-filter=0`, `--enable-intra-edge-filter=0`,
   `--max-reference-frames=3`, and the same cell cut to 5/6/8/12 frames.
   Lossy is exact with alt-ref (`--cq-level=30`, 17/17), so the trigger is
   **lossless AND alt-ref AND a 1:4 rect strip**; `--enable-rect-partitions=0` and
   `--enable-1to4-partitions=0` both make the cell exact.

## 2. Decode order vs shown, per plane

Per-cell table from `EC_AV1_FINAL_DUMP` on BOTH sides (one file per decoded picture,
hidden alt-ref frames included), 320x240 4:2:0, 115200 B/frame:

L25_A1_G1 pre-fix, 17 decode-order frames (16 shown outputs, 1 show_existing header):

| frame | Y | U | V | note |
|---|---|---|---|---|
| 0 | 0 | 0 | 0 | key frame exact |
| 1 | 0 | 0 | 0 | hidden alt-ref #1 exact |
| 2 | 12559 | 4715 | 4761 | **first divergence**: the first frame that predicts from the ALTREF slot (`refs[ALTREF]=1`) |
| 3-16 | 34033-68857 | 9530-18514 | 9676-17607 | monotone growth (each frame predicts from the previous, already-wrong picture) |
| total | 686123 | 198962 | 203488 | 1 088 573 wrong, 2/17 frames exact |

So: **both** planes and **both** classes of frame — the first wrong frame is a HIDDEN
alt-ref frame (decode-order 2), and the damage reaches the shown frames by prediction
from it. Post-fix: 0/0/0 on all 17, per plane.

Spatial shape: not whole-frame garbage (only 299 of 1200 8x8 luma blocks in frame 2),
a wedge over the moving content — i.e. wrong PREDICTION downstream of one wrong context
read, not an entropy flood. 1st wrong sample luma (264,0).

Per-cell control (comparator proven to read the oracle): flipping ONE bit in
`aom.f2` byte 1000 moves the count by exactly +1 in Y and knocks exactly one frame out
of 17/17 → 16/17. A chroma-side flip behaves the same in U.

## 3. Mechanism (paired rungs, one read followed to the source)

Scoped to the 5-frame variant (28394 B) so the fork is early. Rungs: oracle
`EC_TRACE_MODE=1` (`EC_MODE`/`EC_MODE_MV`/`EC_MODE_VAL`) + `EC_ECDUMP=1` +
`EC_TRACE_COEFF=1`; ours the same tags plus `EC_SYMR=1` and `EC_CHROMA_PUB=1`.

1. The `EC_MODE` PRE-read ladder (mi, rng) is in lockstep for **359 blocks** and both
   sides read block mi (4,0) with the same pre-state `rng=38812`; the next block
   differs (aom 62752 / ours 60484). The fork is inside mi (4,0) of **frame 2**
   (`EC_COEFF_FRAME decode_idx=2`).
2. mi (4,0) is a 16x16 partition split to an 8x8 leaf: `mode=16` (NEWMV), `ref0=7`
   (ALTREF), `mv0=(36,0)`, `warp=false`, `stack=1` — identical on both sides.
3. Its coefficient ladder matches read for read through all **four luma 4x4 TUs**
   (`all_zero ctx=3,6,3,6`, eob 7, base_eob 2, base c=5..0, br k=3,0 — every post-rng
   equal: 42119, 53648, 63600, 51536, 35042, 48436, 53568, 38352, 49028, 51204, 43384).
4. The 5th read is the chroma U `txb_skip`, and here the pre-state is **identical**
   (`pre rng=35592` both sides) while the context differs: **ours 2, libaom 8**
   (`EC_ECDUMP plane=1 mi=(4,0) bc=0 br=0 ctx=8 above=[23,] left=[0,]`).
   libaom 8 = `7 + get_entropy_context` with above≠0, left==0
   (`av1/common/txb_common.h:429-435`, `av1/common/entropy.h:87-97`); ours 2 = both
   neighbours coded.
5. `EC_ECDUMP` names the disagreement: libaom's chroma LEFT neighbour is level 0 —
   there is none — and ours is the 16x4 (1:4) strip above, whose chroma cell is one
   mi row up. Both sides agree on chroma ABOVE (23).

**Root cause.** `record_mi_chroma` (decode.rs:10932) published a chroma transform
unit's coefficient context at the **block's** mi origin. A 4x4 chroma unit is `4 << ss`
luma px = `1 << ss` **luma mi** per axis, so a block that starts mid-cell — a 16x4 /
4x16 1:4 strip, whose mi row (or column) is ODD at 4:2:0 — stamped its own origin plus
the mi row/col BELOW it, one row/col past the cell it owns. libaom's per-plane
`left/above_context_map` is indexed in the **plane's own 4x4 units**
(`entropy.h:87`, `txb_common.h:313-366`), so the cell's origin is the step-aligned mi.
`EC_CHROMA_PUB` pre-fix prints the strip's units at `mi=(3,0)/(3,2)` and `mi=(1,0)/(1,2)`;
post-fix at `mi=(2,0)/(2,2)` and `(0,0)/(0,2)`.

**The fix** (one line): align the publication origin DOWN to the cell,
`mi_r & !(step_y - 1)`, `mi_c & !(step_x - 1)`, with the step derived from the unit's
own luma span (`w_px / MI`), so 4:2:0 aligns by 2, 4:2:2 by 2/1, 4:4:4 by 1. One point,
all six callers.

**Class sweep.** `record_mi_chroma` is the only per-transform-unit chroma publisher;
the luma publishers (`record_mi_luma`, `record_mi_luma_rect`) sit on the unsubsampled
mi grid where no alignment exists, and the whole-block records
(`record_mi_rect`/`record_split_luma_rect_mi`) write at most the block's own rows —
they UNDER-publish a cell, which no raster-order reader can observe, and never
over-publish into the next cell. So the over-publication defect is unique to the site
fixed. The mode/partition/skip/ref/filter context arrays are all luma-mi keyed and
unaffected.

## 4. Is the 'Golomb tail' refusal this divergence?

**Yes — proven by construction, not inferred.** Mutating the one alignment line back to
the identity (`let (mi_r, mi_c) = (mi_r, mi_c);`) makes the new gate fail with exactly
`unsupported: AV1 tile (a Golomb tail longer than this decoder reads)` on this cell
(stream.rs:10410). The guard is honest about what it sees (a long Golomb tail really is
what a wrong MV-derived read produces) but it was reporting THIS defect on a
user-reachable cell; with the alignment in place the cell decodes byte-exact and the
refusal is left to mean what it says.

## 5. Witness, pin, gate, mutation proof

- Pin: `crates/ec-av1/fixtures/420_lossless_arf_1to4_320x240_5f.obu`, 28394 bytes,
  sha256 `c5507ed97f74ca495b71449fda24f5fe2fcfa34eed38988a69141d45b126627c`,
  fnv1a64 `16971188390893372935` (asserted by the gate; the tree pins by fnv, a
  self-compared `const SHA256` would be the tautology lane-av1422anom exists to kill).
- Recipe in the gate doc comment: `ffmpeg testsrc2 320x240 r=25 -frames:v 5
  -pix_fmt yuv420p` + `aomenc --codec=av1 --obu --lossless=1 -w 320 -h 240
  --lag-in-frames=25 --auto-alt-ref=1 --limit=5`.
- Gate: `a_420_lossless_alt_ref_1to4_strip_witness_is_byte_exact_in_decode_order`
  (stream.rs:8786). Asserts, in order: pin identity → **6 decode-order frames
  byte-exact vs aomdec (1 hidden)** → the new `chroma_midcell_pub_hits()` route counter
  is non-zero (a mid-cell chroma publication really happened, so the gate cannot pass
  on a stream whose chroma units all start on a cell boundary) → 5 shown frames exact
  per plane via `count_rawvideo_diffs`.
- New counter: `chroma_midcell_pub_hits() -> [row, col, both]` / reset, incremented in
  `record_mi_chroma` from the CALLER's (pre-alignment) origin.
- Mutation proof: alignment → identity ⇒ `FAILED ... a Golomb tail longer than this
  decoder reads`; alignment restored ⇒ `ok`. GREEN measured after restore.
- The 16-frame cell (the ticket's own) was NOT pinned: at 86649 B it is 3x the pin for
  the same code path; its recipe + sha are in §1 and both are exact.

## 6. Regression

`cargo test -p ec-av1 --lib --features gate-counters -- chroma`: **52 passed, 0 failed**
(includes the 4:2:2/4:4:4/intra-BC/sb128 chroma gates and the 1:1-to-4 chroma-pair
gate). A second scoped run (`lossless 1to4 rect_`) was launched; see the yield for its
result.

## not_done

- No full-suite run (main's job by the lane rules); only the two scoped filters above.
- `scripts/fixture-library.tsv` is GENERATED and every fixture reference is recorded
  with a `file:line`; the new pin adds a row that belongs to the post-wave merge
  (`scripts/gen-fixture-library.sh`). Not hand-edited here on purpose.
- The 4:4:4 / 4:2:2 alignment paths (step 1 and 2/1) are exercised by the existing
  chroma gates, not by a NEW 4:4:4 mid-cell witness: at 4:4:4 a chroma cell is one mi,
  so no block can start mid-cell on the row/col axis, and 4:2:2's odd axis is the same
  arithmetic as 4:2:0's.
- The broader knob matrix (`--cpu-used`, `--enable-order-hint=0`, `--enable-obmc=0`, …)
  was measured for narrowing only; each "exact" result there is a re-partitioning
  coincidence, NOT mechanism evidence, and none of them is claimed as such.

## 7. Two more resolutions / contents (same recipe, lag 25 / alt-ref 1 / GM 1 / lossless)

| content | size | sha256 | pre-fix | post-fix |
|---|---|---|---|---|
| `testsrc2` 416x240, 16f | 24430 | `d87a13d84edd427e96bc195f5e90b8be6c4937b0a42fa5f26829622aa5da9dd1` | DIVERGES 655 402 wrong (Y 424601 / U 113556 / V 117245), 3/17 frames exact | EXACT 17/17 (0/0/0) |
| `mandelbrot` 320x240, 16f | 332523 | `cc82abd16949e3449d2e63f93a646e030f869bbc1091794af987028b767190bc` | DIVERGES 1 474 781 wrong (Y 981821 / U 243154 / V 249806), 1/16 frames exact | EXACT 16/16 (0/0/0) |

Pre-fix numbers measured by rebuilding the parent commit's `decode.rs`
(`git checkout aef4fa67 -- crates/ec-av1/src/decode.rs`) into the same target dir and
re-running the same comparator on the same bytes; the tree was restored and rebuilt
afterwards (`git status` clean).

## 8. Regression, second filter

`cargo test -p ec-av1 --lib --features gate-counters -- lossless 1to4 rect_` hit my
300 s cap with several LIVE-aomenc tests still encoding; every test that completed was
green (no `FAILED` line), among them
`a_real_aomenc_stream_with_a_superblock_level_1to4_partition_decodes_pixel_exact`,
`..._filter_intra_on_a_32_level_1to4_strip_...`, `..._rect_palette_...`,
`..._rect_screen_content_...`, `an_sb128_rect_strip_with_intrabc_...`. The filter needs a
longer cap (or a VPS) to finish; the `chroma` filter (52 tests, the ones this change can
plausibly move) completed fully green.
