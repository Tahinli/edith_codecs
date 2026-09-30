# lane-av1422remeasure — the 4:2:2 capability gap, re-measured on `6d564cd6`

Base `6d564cd6`, worktree `~/.cache/wt/av1422remeasure`, branch
`lane-av1422remeasure`. Report-only: **no source change is committed**, and the
`EC_AV1_ALLOW_422_PROBE` bypass was patch-run-restore and is **not** in any
commit. Every number below was measured on `6d564cd6`, as chartered.

**Main moved under the lane** (two commits, `lane-av1odd440` → `0ea32904`,
merged in after the measurements). It does not disturb any conclusion: the
4:2:2 guard is the same single `subsampling_x != subsampling_y` test on the
same line, with only the message extended to also name 4:4:0 as uncodable, and
`440_request_is_422.obu` is a hand-built header probe, not a decodable cell.
Re-verified on the merged tip: the four committed 4:2:2 refusal-by-name gates,
`a_non_420_subsampled_sequence_header_is_refused_by_name`,
`decode::tests::a_422_reference_claims_the_full_chroma_height` and
`decode::chroma422_pair_plane_matches_the_ss_size_lookup_cells` — **7 passed /
0 failed**.

## Verdict

**The debt's blocker line is stale twice over, and the refusal is no longer
earning its keep as a correctness guard.**

1. The named blocker — "the dc-sign vote on the rect path taking luma-unit
   neighbour counts" — **is fixed on main and is now non-load-bearing**. It is
   present, it is reached, and with it surgically reverted the pinned stream is
   *still* 16/16 pixel-exact. See "The dc-sign symptom".
2. **Every 4:2:2 cell I could reach decodes byte-exact against the instrumented
   `aomdec`**: 6 committed fixtures + 12 freshly encoded ones = **18 cells,
   18/18 exact, 0 wrong samples on Y, U and V, every frame, no panic, no
   desync.** That includes two axes the committed corpus had never touched —
   **10-bit 4:2:2** and **odd width (322x242) 4:2:2** — plus a second
   independent encoder recipe, which was the other standing lift objection.

Recommendation, at the end, is therefore **lift the blanket refusal and replace
it with a named-cell refusal for the one shape still unreached (12-bit 4:2:2,
which the local `aomenc` cannot even produce)** — with the one caveat and the
one named anomaly stated in full.

## Method

Two independent comparisons, both against
`$HOME/.cache/aom-oracle/build/aomdec` (the Sep 29 05:44 instrumented build):

- **presented output** — `decode_stream` → `pack_rawvideo` (the committed
  `assert_rawvideo_matches` helper's own packing) vs `aomdec --rawvideo`,
  counting wrong samples **per plane** rather than panicking on the first
  mismatch, so one run classifies every cell.
- **decode-order output** — `EC_AV1_FINAL_DUMP` on both sides (ours inside
  `decode_frame`, the oracle's rung 12), compared frame by frame. This is the
  only view that sees hidden alt-ref frames, which `decode_stream` never
  returns.

**Oracle soundness established first**, per the standing rule, on the clean
tree with no 4:2:2 bypass in play:
`a_real_aomenc_quantisation_matrix_stream_decodes_pixel_exact`,
`the_rawvideo_helper_compares_real_samples_at_the_streams_own_bit_depth` and
`a_non_420_subsampled_sequence_header_is_refused_by_name` — **3 passed**.

## The per-cell table

Classification is four-way: **REFUSED** (sequence header), **PANIC**,
**DIVERGES**, **EXACT**. "wrong" is per-plane differing samples over all
decoded frames. Every cell below REFUSES on committed `6d564cd6`; the decode
columns are measured under the local bypass.

### Committed fixtures (pinned on main)

| cell | bytes | sha256 (first 16) | fnv1a64 | class | wrong Y | wrong U | wrong V | frames exact |
|---|---|---|---|---|---|---|---|---|
| `422_allskip_2f.obu` | 62 | `ba4932c14f959 71b` | `0x7e4d69d3c728c55f` | REFUSED → EXACT | 0/32768 | 0/16384 | 0/16384 | 2/2 |
| `422_sb128_3f.obu` | 12543 | `0b851799d99cb4d1` | `0x5171e0aab8000da7` | REFUSED → EXACT | 0/49152 | 0/24576 | 0/24576 | 3/3 |
| `422_intrabc_sb128_strip.obu` | 1672 | `f92000db86df7acc` | `0x50f5cfc576e4cd00` | REFUSED → EXACT | 0/614400 | 0/307200 | 0/307200 | 5/5 |
| `422_intrabc_sb128_strip_notxsearch.obu` | 1675 | `80dd0d4e93fd6c3d` | `0xd4936f252ff8cff0` | REFUSED → EXACT | 0/614400 | 0/307200 | 0/307200 | 5/5 |
| `422_residual_compound_warp_16f.obu` | 38845 | `d78e2afb43ce311d` | `0x0e73a51e2cc0c424` | REFUSED → EXACT | 0/1179648 | 0/589824 | 0/589824 | 16/16 |
| `422_residual_compound_warp_nolr_16f.obu` | 38538 | `8bed368f7ec4bca7` | `0x4b8ff761701e2bef` | REFUSED → EXACT | 0/1179648 | 0/589824 | 0/589824 | 16/16 |

Refusal string, identical for all six and for all twelve fresh cells:

```
unsupported: AV1 decode_stream (a chroma format of 4:2:2 (subsampling_x != subsampling_y): this decoder decodes 4:2:0 and 4:4:4; 4:2:2 is not ported)
```

Emitted at `crates/ec-av1/src/stream.rs:1780`, a single `subsampling_x !=
subsampling_y` test on the parsed sequence header. The four committed
refusal-by-name gates all pass on the clean tree:
`the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name`,
`the_pinned_422_intrabc_sb128_strip_witnesses_refuse_by_name`,
`the_pinned_422_lr_off_witness_is_present_and_refuses_by_name`,
`the_pinned_422_residual_compound_warp_witness_is_present_and_refuses_by_name`.

### Freshly encoded cells (this lane; NOT committed — see "not_done")

All from `aomenc --codec=av1 --profile=2 --input-bit-depth=<bd> --bit-depth=<bd>
--limit=16 --lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=1
--pass=1 --cq-level=<45> --threads=4 --kf-min-dist=0 --kf-max-dist=999999
--width=W --height=H --cpu-used=0 <src>.y4m -o <n>.webm`, then
`ffmpeg -i <n>.webm -c copy -f obu <n>.obu`. Wave 1 used `--cq-level=24`; the
per-cell deltas are named. Sources are `yuv422p` / `yuv422p10le` y4m from
ffmpeg 8.1.2 (`testsrc2`, `mandelbrot`, `smptebars`); the HBD and odd-size
y4m files are hand-built with per-frame `FRAME` headers, because ffmpeg's
y4m muxer will not write `C422p10` and libaom's reader rejects a single
leading `FRAME` header ("Loss of framing in Y4M input data").

| cell | recipe delta | bytes | sha256 (first 16) | class | wrong Y | wrong U | wrong V | frames exact | decode-order frames (dump-vs-dump) |
|---|---|---|---|---|---|---|---|---|---|
| `A_testsrc2_cpu0` | testsrc2 320x240, cq 24 | 21584 | `2e03b3fe92b17c35` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 17/17 exact |
| `B_testsrc2_cpu6` | A + `--cpu-used=6` | 22570 | `7da006204c9f990c` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 17/17 exact |
| `C_mandel320` | mandelbrot 320x240, cq 24 | 30327 | `945510d22b5da687` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 16/16 exact |
| `D_bars` | smptebars 320x240, cq 24 | 994 | `6d3e3638c2f8cbfe` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 16/16 exact |
| `E_noglobal` | A + `--enable-global-motion=0` | 21545 | `efee4a80a745ee5f` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 17/17 exact |
| `F_allintra` | A + `--lag-in-frames=0 --auto-alt-ref=0` | 19937 | `a96a52339196b4a4` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 16/16 exact |
| `H_10bit_testsrc2` | testsrc2 10-bit 4:2:2 | 20628 | `f983ec21f7040cce` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 17/17 exact |
| `I_10bit_mandel` | mandelbrot 10-bit 4:2:2 | 28770 | `ecf4ffc5ac3ed11b` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 16/16 exact |
| `J_10bit_lr0` | H + `--enable-restoration=0` | 20605 | `fcfb47ce74d9c41c` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 17/17 exact |
| `K_sct` | A + `--cq-level=18 --tune=ssim` | 18792 | `580bf8d7d459156c` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 17/17 exact |
| `L_tiled` | A + `--tile-columns=1 --tile-rows=1` | 22553 | `50c22276a6ad4864` | REFUSED → EXACT | 0/1228800 | 0/614400 | 0/614400 | 16/16 | 17/17 exact |
| `O_odd322x242` | testsrc2 **322x242**, odd width | 21490 | `5af747bdb597a7a2` | REFUSED → EXACT | 0/1246784 | 0/623392 | 0/623392 | 16/16 | **17/17 DIFFER — see anomaly** |

`A`–`L` carry one hidden alt-ref frame each (17 decode-order vs 16 shown), and
the hidden frame is byte-identical on both sides. `O` also has one.

**Totals: 18 cells, 18 EXACT, 0 PANIC, 0 DIVERGE on presented output, 0 wrong
samples on any plane of any frame; 17 of 18 exact in decode order.**

## The dc-sign symptom

**Counter named: the `EC_DCDUMP` / `EC_DCDUMP422` rungs** in
`crates/ec-av1/src/decode.rs` (printed at `decode.rs:10123` and
`decode.rs:10188`, both under `EC_DCDUMP=1`). `EC_DCDUMP` is the luma per-mi
rect gather (`around_mi_rect`); `EC_DCDUMP422` is the 4:2:2 chroma gather
(`around_mi_422_chroma`) that samples every second above cell. The debt's
symptom is the *disagreement between them on the same block*.

**The fix is on main.** `decode_leaf_rect` (`decode.rs:16084-16089`) routes
both chroma planes through `around_mi_422_chroma` when `chroma_422`, with the
gate closed at `ss_x == 1 && ss_y == 0`.

Measured on current main, `422_residual_compound_warp_16f.obu`, `EC_DCDUMP=1`,
at the exact block the debt names (mi(48,28), V plane, `wh=(16,8)`):

```
EC_DCDUMP    mi=(48,28) plane=2 wh=(16,8) vote=0   above=[None/6,None/6,Some(false)/7,Some(false)/7]  left=[Some(true)/7,Some(true)/7]
EC_DCDUMP422 mi=(48,28) plane=2 wh=(16,8) vote=-1  above=[None/6,Some(false)/7]                           left=[Some(true)/7,Some(true)/7]
```

That is the symptom, gone: the luma-unit walk sums **four** above cells to a
vote of **0**; the chroma-unit walk the leaf actually uses sums **two** and gets
**-1**. It matches `lanes/av1422warp.report.md`'s recorded "after" exactly.

Rung reach on that one stream: `EC_DCDUMP` 2091 lines, `EC_DCDUMP422` 9468
(6312 of them chroma-plane).

### Before / after, with the fix surgically reverted

I gated the `if chroma_422` block off (`decode.rs:16085` → `if false &&
chroma_422`), re-ran, then restored it — `git diff` on `decode.rs` is empty
again.

| measurement | reverted | main (fix present) |
|---|---|---|
| `EC_DCDUMP` lines (stream) | 2079 | 2091 |
| `EC_DCDUMP422` lines (stream) | 9174 | 9468 |
| `422_residual_compound_warp_16f` presented vs `aomdec` | **16/16 exact, 0 wrong** | **16/16 exact, 0 wrong** |
| `422_residual_compound_warp_nolr_16f` presented vs `aomdec` | 16/16 exact, 0 wrong | 16/16 exact, 0 wrong |

**The before/after that matters: there isn't one.** The reverted build is still
byte-exact on the stream that originally exposed the defect. The fix is real
and reached (the rung counts move, so the traversal genuinely changes), but on
current main it is **no longer load-bearing for any cell in the corpus**. The
later work in `lane-av1422warp` closed the surrounding desynces, and the
luma-unit vote now coincidentally agrees with the chroma-unit vote on this
content. That is the honest reading, and it is why the debt line is stale
rather than merely "fixed": there is no longer a single desync to point at.

## The one anomaly: odd-width 4:2:2, stored picture vs presented picture

`O_odd322x242` (322x242, so chroma is **161x242** — an odd, non-MI-aligned
chroma width) is the only cell that is exact on presented output but **differs
from the oracle in decode order on all 17 frames**.

| frame | differing bytes | planes | chroma cols | chroma rows |
|---|---|---|---|---|
| 0 | 48 | U 32, V 16 | 108–131 | 117–138 |
| 1 | 95 | U 59, V 36 | 107–133 | 116–139 |
| 2 | 91 | U 59, V 32 | 108–132 | 106–139 |

Both dumps are the same size (155848 B = the cropped 322x242 4:2:2 extent), so
this is not a shape or crop artefact, and the region is interior, not an edge.

The discriminating control: for the same source at 4:2:0 (322x242 →
chroma 161x121) both the decode-order dumps and the presented output are
**byte-identical** (`P_odd420_322x242`: 0 differing frames), and so is
320x242 4:2:0. So the anomaly is **4:2:2-specific and odd-chroma-width
specific**, not an odd-dimension problem in general.

What I can and cannot say:

- **CAN say**: the picture `decode_stream` hands the caller is correct — 0 wrong
  samples against `aomdec --rawvideo` on all 16 shown frames.
- **CAN say**: the `EC_AV1_FINAL_DUMP` rung, which documents itself as "the
  frame exactly as it is about to be stored into the reference slots", does
  **not** agree with that picture on this shape.
- **CANNOT say** whether that is a genuine stored-reference defect or a
  rung-timing artefact. The rung and the returned picture are the same object
  inside `decode_frame`, so the two readings cannot both be right, and I did not
  build the instrument to decide which. I am not going to guess: this goes in
  `not_done` with the exact next step.

It is a small, localized, chroma-only effect on a shape the committed corpus
has never contained, and it is invisible in user-visible output — but it is
the one measurement that argues against lifting the refusal outright, and it
should be settled **before** the refusal comes down, not after.

## What still blocks the lift

**The recorded entropy blocker: nothing.** It is fixed, and it is no longer
load-bearing.

The three grounds `lanes/av1422warp.report.md` gave for keeping the refusal
now stand as follows:

1. **Thin top-half engagement** — *not addressed, and now much weaker an
   objection.* That argument was about one stream's block census. I did not
   re-run the census; the 12 fresh cells simply do not make it worse, and the
   LR-off ground the same report listed (ground 3) is already discharged by the
   committed `422_residual_compound_warp_nolr_16f.obu` and re-confirmed here.
2. **One stream, one recipe** — **discharged.** Six independent encoder
   recipes across four different sources, two partitioning speeds, three
   encoder feature sets and two bit depths, all byte-exact.
3. **LR-off untested** — **discharged**, and re-confirmed independently here
   with `J_10bit_lr0` (10-bit *and* LR off) as well as the committed
   LR-off witness.

So the correctness case for the refusal is gone. What remains is: a coverage
argument about how much engagement a future stream could carry, one unattributed
odd-width anomaly, and one shape I could not produce at all.

## Recommendation

**Keep a refusal, but make it a named-cell refusal and narrow it — do not keep
the blanket `subsampling_x != subsampling_y` guard.**

1. **Replace the blanket guard** with a refusal conditioned on the shape that
   is actually unmeasured. The measured evidence is 18/18 byte-exact across
   8-bit and 10-bit 4:2:2, even and odd width, one and many encoder recipes,
   LR on and off, single-tile and 2x2-tile, global-motion on and off, alt-ref
   on and off.
2. **Keep 4:2:2 refused for 12-bit specifically**, by name, until a 12-bit
   4:2:2 stream exists. I could not produce one — this `aomenc` refuses the
   encode outright (`Failed to set chroma subsampling x: Unspecified internal
   error` at `--profile=2 --bit-depth=12` on a `C422p12` y4m), so 12-bit 4:2:2
   is genuinely unwitnessed rather than merely untried. This is the same
   pattern the tree already uses for 12-bit screen-content tools.
3. **Settle the odd-width anomaly first**, and make its outcome a gate either
   way. If it is a stored-reference defect, odd-width 4:2:2 joins the
   named-refusal list and the rest lifts. If it is a rung-timing artefact, the
   refusal lifts outright.
4. **Do not lift on this round's evidence alone**, for the reason the previous
   lane gave and that still holds: the lift decision should not be made in the
   same round that re-measured the thing it is deciding about. What this
   round does is retire the stale blocker line and hand the next lane a
   two-cell question with numbers attached, instead of an open-ended one.

The evidence that decides it, in one line: **18/18 cells byte-exact, 0 wrong
samples on any plane, versus one 48–95-byte-per-frame chroma-only divergence
on odd-width 4:2:2 in the stored-picture view that is absent from the
presented view.**

## Gates run

On the clean tree at `6d564cd6` (no source diff — the lane commits only this
report), scoped:

- `the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name` — pass
- `the_pinned_422_intrabc_sb128_strip_witnesses_refuse_by_name` — pass
- `the_pinned_422_lr_off_witness_is_present_and_refuses_by_name` — pass
- `the_pinned_422_residual_compound_warp_witness_is_present_and_refuses_by_name` — pass
- `a_real_aomenc_quantisation_matrix_stream_decodes_pixel_exact` — pass
- `the_rawvideo_helper_compares_real_samples_at_the_streams_own_bit_depth` — pass
- `a_non_420_subsampled_sequence_header_is_refused_by_name` — pass
- scoped battery `422 420 chroma chroma_422 rawvideo subsampling aomenc_444 hidden`
  — **61 passed / 0 failed / 0 ignored**

Re-run after merging main's `0ea32904`: the four refusal-by-name gates plus
`a_non_420_subsampled_sequence_header_is_refused_by_name`,
`decode::tests::a_422_reference_claims_the_full_chroma_height` and
`decode::chroma422_pair_plane_matches_the_ss_size_lookup_cells` —
**7 passed / 0 failed**.

## Instrumentation disclosure

Two temporary artefacts, both removed and neither committed:

- the `EC_AV1_ALLOW_422_PROBE` env bypass at `stream.rs:1780` (patch-run-restore, the
  established 4:2:2 probe mechanism);
- two `#[test]`s (`zz_temp_422_remeasure_cells`, `zz_temp_422_probe_one`) that
  count per-plane wrong samples instead of panicking, and drive
  `EC_AV1_FINAL_DUMP` from an external cell directory.

**No new instrumentation was added to the decoder or the oracle.** The dc-sign
counter used is the pre-existing `EC_DCDUMP`/`EC_DCDUMP422` pair, quoted rather
than built.

## `not_done`

- **The odd-width 4:2:2 stored-vs-presented anomaly is unattributed.** Next step:
  in `decode_frame`, write the `EC_AV1_FINAL_DUMP` bytes and a hash of the
  returned `Picture` from the *same* frame in one process, so the two views are
  provably the same object; if they still disagree, the defect is in the
  reference store, and the region (chroma cols ~107–133) is small enough to
  bisect against `EC_PRED`/`EC_MCB`. I did not build this.
- **No new fixture committed.** The 12 fresh cells are recorded above with
  full recipes and sha256 but are **not** pinned into
  `crates/ec-av1/fixtures/`, deliberately: pinning them would imply a defect or
  a lift decision this report does not establish, and the tree's convention is a
  pin per witnessed gate. If the next lane takes the lift, these cells are the
  witnesses to pin.
- **The top-half engagement census was not re-run** — libaom's
  `is_global_mv_block` gating is oracle-side instrumentation I did not rebuild.
  Ground 1 of the old refusal argument is therefore untested, not disproven.
- **12-bit 4:2:2 unwitnessed and unreachable** with the local `aomenc`; it needs
  an encoder that can emit `--profile=2 --bit-depth=12` with 4:2:2, or a
  hand-built stream.
- **Superres 4:2:2 unwitnessed** — `--superres-mode=2` never produced an
  encode with this build; no recipe found.
- **Full suite not run** (per lane rules: scoped only; project-wide validation is
  Main's).
