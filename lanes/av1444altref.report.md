# lane-av1444altref — 4:4:4 LOSSLESS + alt-ref chroma residual: FIXED, GATED

Base `3698787d` (main), worktree `~/.cache/wt/av1444altref`, branch `lane-av1444altref`.
Round 3 (Ege-3 → Mehmet-2). **The 317-byte chroma residual is gone: `ll444_c`
now decodes byte-exact against the oracle aomdec on all 18 decode-order pictures
(0 Y / 0 U / 0 V).** A permanent byte-exactness gate with an oracle-flip
control is in `crates/ec-av1/src/stream.rs:8395`, and it is proven to bite.

The root cause is **not** an arithmetic/rounding difference. It is a **missing
operation**: the 8x8 compound inter leaf warped its LUMA plane and left both
CHROMA planes translational. §4 names the exact C and Rust sites, §5 explains
why the signature is a bottom-right-biased ±1 gradient, §7 lists what the
predecessor's rounding hypothesis actually checked and why it is now provably
identical arithmetic.

## 0. Verdict table

| | |
|---|---|
| parent's 317 bytes reproduced | **YES, exactly** (0 Y / 147 U / 170 V, first diff decode picture 8) |
| pairing hole closed | **YES** — the oracle's rung now carries a decode-order picture counter + full block identity; pairing is by `(picture, plane, x, y, w, h)` and is unambiguous |
| entropy / residual / loop filter | **REFUTED** (inherited, re-checked: the pre-filter reconstruction already carried 100% of the divergence) |
| rounding/bias hypothesis | **REFUTED** — the CONV_BUF bias cancels exactly in every combine; see §7 |
| **root cause named** | **YES** — `decode_inter_block8`'s compound arm has no per-plane chroma warp |
| **fix** | **4 added `warp_affine_compound` calls (U/V × ref0/ref1) + a tightened per-plane bail** |
| **after the fix** | **0 differing bytes**, all 18 pictures, `cmp -l` empty |
| **gate** | `stream::tests::a_lossless_444_altref_leaf8_warp_chroma_is_byte_exact` — byte-exactness on all 3 planes × 16 displayed frames, plus an oracle-flip control and a reachability witness |
| **mutation proof** | gate reds **by name** with `0 Y / 147 U / 170 V` when the 4 chroma warp calls are removed (§6) |
| **regression** | `cargo test -p ec-av1 --lib -- lossless 444 422 --skip bitrate_...` → **71 passed, 0 failed**; `-- warp obmc` → **32 passed, 0 failed**; `-- inter` → see §8 |

## 1. The cell, and the measurement I re-took first

```
$ sha256sum /home/tahinli/.cache/cells/av1422lpf/regress_444/ll444_c.obu
999a41d88a092840456d5e9de83590ab819a8a14686a694889e63c2258ff2bb8   512220 bytes
```
4:4:4 (ss 0/0), 8-bit, 320×240, 18 decode-order pictures for 16 displayed
frames (2 hidden altrefs, decode idx 1, 2, 8, 9, 12 are ALTREF). Never
re-encoded; the bytes are now committed as
`crates/ec-av1/fixtures/ll444_c_altref_leaf8_warp.obu` (same sha256).

Baseline, `EC_AV1_PREFILT_WIDE_DUMP` (decode order, per picture):

```
f0..f7  0        f13  80
f8     80  <- first divergence (hidden altref)   f14  46
f9     25        f15  80
f10     5        f16  67
f11    50        f17  26
f12     3
TOTAL  317  (Y 0 / U 147 / V 170)
```
The 80 bytes of picture 8, mapped to plane coordinates (320-wide rows, Y then
U then V per picture):

```
U (288,32)   4 samples   U (112,80)  20   U (120,80)  2   U (184,208) 12
V (288,32)   4           V (112,80)  23                V (184,208) 15
```
Every one of the 80 is ±1.

## 2. Closing the pairing hole (charter step 1)

The predecessor's blocker was real: the oracle's rung identified only a
GEOMETRY, the same chroma geometry recurs across pictures, so its 12-of-256
prediction diff could not be attributed to a block. Two changes, both on the
oracle side (`~/.cache/aom-oracle/src/av1/decoder/decodeframe.c`, the tree is
not a git repo, so the patch is reproduced here verbatim in intent):

1. **A decode-order picture counter.** `int aom_ec_pict_idx = 0;` at
   `decodeframe.c:89`, incremented at `decodeframe.c:5493` — immediately after
   `decode_tiles*` returns in `av1_decode_tg_tiles_and_wrapup`, so during
   picture N's decode the counter already reads N (0-based), which is the same
   numbering the `EC_AV1_PREFILT_DUMP` `.fN` files use.
2. **`EC_ZZCPRED` → `EC_CPRED`, a self-identifying block dump** at
   `decodeframe.c:691`, moved to the END of the plane loop (after
   `av1_build_interintra_predictor`, so it measures the same quantity on both
   sides: the finished prediction, pre-residual). It now prints
   `pict, mi_row, mi_col, bsize, plane, x, y, w, h, ss, ref0, ref1, mv0, mv1,
   cidx, type, mm, iit, gm0, gm1, sx/sy per ref, interp filters` plus every
   sample of the block. `EC_ZZCPRED="pict:x:y:plane"`, any field `< 0` = any.
   Rebuilt with `ninja -C ~/.cache/aom-oracle/build aomdec`.

The matching rung on our side is `EC_CPRED` in
`decode.rs:37184` (`PlaneBuf::reconstruct_mc_rect`) — the single site every
inter prediction passes through on its way into a plane, after the compound
blend and after the inter-intra blend and before the residual. It prints
`pict` (`PREFILT_PICTURE_IDX`, which is bumped once per frame just after that
frame's `flush_recon`, so it still reads the current picture), `plane, x, y, w,
h, side`. Both rungs are env-gated and change no behaviour; they are the
instrumentation this report's numbers come from and are left in place
deliberately.

**Pairing protocol that actually works** (the naive one does not): our side
writes per TRANSFORM UNIT, not per block — an 8×8 block with 4×4 units emits
four 4×4 writes — so a `(x, y, w, h)` key match is the wrong join. The correct
join is per PIXEL: for a pixel, take the oracle's block value (the block
ladder is per block and now picture-identified) and the set of values our side
wrote there. `decode_inter_block8` writes a block's chroma prediction through
the same `reconstruct_mc_rect` for every unit, so a wrong prediction is a value
that appears in NO write covering that pixel.

## 3. The block identity, now unambiguous

For each of the 7 diff cells, the oracle's picture-8 ladder names exactly ONE
owning block (`EC_CPRED="8:<x>:<y>:<plane>"`):

| cell | mi | bsize | refs | mv0 | mv1 | compound_idx | type | interintra | global mv |
|---|---|---|---|---|---|---|---|---|---|
| U/V (288,32) | (8,72) | 8×8 | 4,5 | (24,-35) | (44,-66) | 0 | **WEDGE** | no | **both** |
| U/V (112,80) | (20,28) | 8×8 | 4,5 | (10,12) | (20,23) | 0 | **WEDGE** | no | **both** |
| U (120,80) | (20,30) | 8×8 | 4,INTRA | (8,8) | — | 1 | AVERAGE | **yes** | no |
| U/V (184,208) | (52,46) | 8×8 | 4,5 | (-24,-8) | (-45,-14) | 0 | **WEDGE** | no | **both** |

So the class is **8×8 compound leaves whose two references are GLOBAL-motion
(affine) blocks**, plus one inter-intra leaf in the same area. The
predecessor's quoted `compound_idx=1` "(8,8) simple-average split" belongs to
a *different* block in f8 and is not the affected one; its
"chroma INTER PREDICTION ROUNDING" class is wrong.

Per-pixel prediction evidence at two of the diff pixels (residual = pre-filter
final − prediction, and the block is a SKIP block so the oracle's residual is 0):

```
pixel U (112,80): oracle pred 163, oracle final 163 | our pred 164, our final 164
pixel U (112,87): oracle pred 152, oracle final 152 | our pred 151, our final 151
pixel U (186,208): oracle pred 139, oracle final 138 | our pred 138, our final 137
```
— i.e. OUR PREDICTION is off by ±1 and the residual is not involved. The
LUMA of the very same three blocks is bit-exact (checked per pixel: 0 of 64
pixels at each block's luma origin where no value our side wrote equals the
oracle's).

## 4. The named defect

**libaom warps per PLANE; `decode_inter_block8`'s compound arm warped per
PLANE-LUMA-ONLY.**

* libaom: the plane loop in `dec_build_inter_predictor`
  (`decodeframe.c:701-768`, `for (int plane = 0; plane < num_planes; ++plane)`)
  calls `dec_build_inter_predictors` per plane, which for an 8×8-and-bigger
  block runs the per-ref loop of `build_inter_predictors_8x8_and_bigger`
  (`reconinter_template.inc:225`). Inside it, **per plane**:
  `av1_init_warp_params(&inter_pred_params, &warp_types, ref, xd, mi)`
  (`reconinter_template.inc:248`, C at `reconinter.c:58-75`, whose first line is
  the per-plane bail `if (inter_pred_params->block_height < 8 ||
  inter_pred_params->block_width < 8) return;`, `reconinter.c:61`) and then
  `build_one_inter_predictor(...)` (`reconinter_template.inc:262`) →
  `av1_make_inter_predictor` → `av1_warp_plane` (`reconinter.c:156`), with
  `pd->subsampling_x/y` and that plane's own origin and extent. The plane loop
  is outside the ref loop, so **every** plane of a global-motion compound
  block is warped.
* ours, before the fix: `decode_inter_block8` applied
  `crate::warp::warp_affine_compound` to the LUMA intermediates only
  (`warp0_c` at `decode.rs:49205`, `warp1_c` at `decode.rs:49239`) and the
  chroma arms (`inter0_u`/`inter1_u` at 49297/49313, `inter0_v`/`inter1_v` at
  49414/49445 before the fix) had **no warp call at all**. The 16×16+ compound
  arm already had all four chroma calls (`decode.rs:41137/41178/41249/41290`)
  and the 8×8 leaf's SINGLE-reference arm already had both chroma calls — which
  is why only this one arm diverged, and why the divergence is chroma-only.
* the fix: four `warp_affine_compound` calls added to the leaf's compound chroma
  arms — U ref0 `decode.rs:49333`, U ref1 `:49369`, V ref0 `:49432`, V ref1
  `:49468` — each guarded by `warp_plane_allowed(chroma_w, chroma_h)`
  (`decode.rs:762`) which is our transcription of the `reconinter.c:61` bail,
  and each passing the plane's own origin (`cpx`, `cpy`), extent
  (`chroma_w`, `chroma_h`) and subsampling (`ss_x(fctx)`, `ss_y(fctx)`).

### Same-class sweep (done in this batch)

The four inter arms' per-plane warp coverage after the fix:

| arm | luma | U | V | guard |
|---|---|---|---|---|
| 16×16+ single-ref (`decode_inter_block`) | ✅ | ✅ | ✅ | `warp_plane_allowed(write_chroma_w, write_chroma_h)` |
| 16×16+ compound | ✅ | ✅ | ✅ | `warp_plane_allowed(write_chroma_w, write_chroma_h)` |
| 8×8 leaf single-ref | ✅ | ✅ | ✅ | **tightened** (below) |
| 8×8 leaf compound | ✅ | **added** | **added** | `warp_plane_allowed(chroma_w, chroma_h)` |

The 8×8 leaf's single-ref arm tested only the WIDTH (`if chroma_w >= 8`), so a
rect chroma block 8 wide but under 8 high (4:2:2 `BLOCK_8X4`) would have warped
where libaom bails on `block_height < 8` (`reconinter.c:61`). Fixed in the same
hunk (`decode.rs:50379`). It cannot fire on this 4:4:4 cell (its chroma is
8×8), so it is a latent same-class correction, not part of the 317.

## 5. Why a missing warp is a bottom-right-biased ±1, not a wrong reference

A GLOBALMV block's MV *is* the local derivative of the frame's affine
transform, evaluated at the block's centre. A translation by that MV therefore
agrees with the affine map at the centre and diverges from it linearly with
distance from the centre: the per-sample error is ≈ (½·divergence)·(offset from
centre), i.e. **zero at the block's top-left, growing toward the bottom-right**
— which is the exact shape the predecessor measured. The magnitude is a couple
of LSBs here because the affine's residual over 8 px on this screen-content cell
is 1–2 samples, and the two blocks' contributions then pass through the wedge
mask blend. A wrong reference or a wrong weight would have produced a uniform
offset; a wrong rounding would have produced a ±1 scatter with no spatial
gradient. The gradient is the signature.

This also explains the class boundaries for free: the same cell's 4×4 leaves
(whose chroma is under 8 in a subsampled format) and the leaf's luma (warped
correctly) are exact, and the divergence only ever appeared in chroma.

## 6. Before / after, and the mutation proof

```
# parent state (reproduced, decode order, cmp -l against aomdec --rawvideo)
$ cmp -l ours.yuv oracle.yuv | wc -l
317                      # 0 Y / 147 U / 170 V, first diff decode picture 8

# with the four chroma warp calls
$ cmp -l ours.yuv oracle.yuv | wc -l
0
$ for i in $(seq 0 17); do cmp -l w3.f$i w2.f$i | wc -l; done   # pre-filter, per picture
0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
```

**Mutation proof** — the fix reverted in place (the four
`if let Some(wp) = &warpN_c { if warp_plane_allowed(chroma_w, chroma_h) { … } }`
blocks deleted from the leaf's compound chroma arms, and the single-ref guard put

```
test stream::tests::a_lossless_444_altref_leaf8_warp_chroma_is_byte_exact ... FAILED
panicked at crates/ec-av1/src/stream.rs:8437:
assertion `left == right` failed:
a_lossless_444_altref_leaf8_warp_chroma_is_byte_exact: 0 Y / 147 U / 170 V samples
differ from the oracle (class altref-444-leaf8-chroma-unwarped)
  left: (0, 147, 170)
 right: (0, 0, 0)
test result: FAILED. 0 passed; 1 failed
```
Red **by name**, with the parent's exact numbers, and green again after the
restore (`… ok`). The gate's oracle-flip control is inside the same test:

```
arm 2: count_rawvideo_diffs(&stream, NAME, Some(W * H))  →  (0, 1, 0)
```
one byte of the oracle's own rawvideo output (the first U sample of frame 0)
rotated in the comparator, and the count must move by exactly one in U only. A
comparator that ignored the reference, or zipped a truncated buffer, would still
report zero there. The gate also asserts the geometry (16 displayed frames,
320×240, 8-bit) and that the two sides are the same size
(`count_rawvideo_diffs` returning `None` is a failure, not a pass), and
`compound_warp_hits_8`'s delta proves the 8×8 compound warp-leaf arm — the body
the fix lives in — actually ran on this cell.

## 7. What the predecessor's rounding hypothesis actually checks out as

The open hypothesis was "the blend's rounding / the CONV_BUF gain". It is
**identical arithmetic**; the reason is that libaom's CONV_BUF bias cancels
exactly in every combine our code performs, so our unbiased `i32` domain and
libaom's biased one are the same function:

* libaom at 8-bit, compound: `round_0 = ROUND0_BITS = 3`,
  `round_1 = COMPOUND_ROUND1_BITS = 7` (`convolve.h:68-100`). The horizontal
  pass adds `1 << (bd + FILTER_BITS - 1)` = 2^14 **before**
  `ROUND_POWER_OF_TWO(sum, 3)`, so each `im_block` entry carries +2048; the
  vertical pass adds `1 << offset_bits` = 2^19 and the 2048 carried through the
  8-tap vertical filter (taps sum to 128) adds another 2^18. After
  `ROUND_POWER_OF_TWO(sum, 7)` the total bias is 2^12 + 2^11 = **6144**, which is
  exactly libaom's `round_offset = (1 << (offset_bits - round_1)) +
  (1 << (offset_bits - round_1 - 1))` (`convolve.c:964-965`, `:948`). Our
  `predict_compound_intermediate` keeps that bias out and
  `combine_compound`/`diffwtd_mask`/`blend_masked_compound` are written against
  the unbiased domain (`mc.rs:2077`, `:2116`, `:2153`).
* The bias is identical on both references, a whole multiple of 64 (mask
  weight sum), of 16 (`DIST_PRECISION_BITS`) and of 2 (the `>> 1` average), so
  it cancels **including under the truncating shifts**:
  `(b + a)>>1 - b == a>>1` for the simple average;
  `((b + a)*f + (b + c)*bk)>>4 - b == (a*f + c*bk)>>1` for the dist-weighted
  arm; `abs` for `diffwtd_mask`; `(m*(b+a) + (64-m)*(b+c))>>6 - b` for the
  masked arm. The final `ROUND_POWER_OF_TWO(·, round_bits)` then acts on the
  same integer in both.
* The only rounding difference that survives all of this would be a change in
  `round_0`/`round_1`/`round_bits`, and those come from one shared place
  (`round_pair`, `mc.rs`) plus `INTER_POST_ROUND - round_delta(bd)`, which is
  constant at 8-bit. The measurement agrees: with the warp restored, the same
  combine code produces **zero** differing bytes on this cell.
* The warp arm's own arithmetic (`warp.rs:507-548`, `warp_inner:579-663`)
  carries the same 2^11/2^12/2^13 constants and was already exercised, exactly,
  by the LUMA of the very blocks that diverged in chroma. That is the strongest
  single refutation of the rounding class: same `warp_affine_compound`, same
  reference, same MVs, same mask — luma exact, chroma not.

Also refuted or superseded:

* **entropy** — inherited and re-confirmed: msac `(value, range)` identical
  over 213474 units. (And with the fix, identical *pixels*.)
* **loop filter** — `EC_AV1_PREFILT_WIDE_DUMP` per picture: f0..f7 = 0,
  f8 = 80 = exactly the final diff. Deblock/CDEF/LR inert. The NARROW
  `EC_AV1_PREFILT_DUMP` must not be used at 4:4:4 (padded vs cropped) — the
  predecessor's trap, re-confirmed.
* **OBMC** — the kill-switch experiment (317 → 32325 with OBMC off) says OBMC
  is load-bearing and correct, and the per-pixel evidence in §3 pins the error
  to the pre-residual prediction of a non-OBMC block.
* **`record_mi_chroma`'s mid-cell align** — arithmetic no-op at ss 0/0
  (`step == 1`), inherited from round 2 and still true.
* **`compound_idx=1` simple average** — the affected blocks are
  `compound_idx=0` + `COMPOUND_WEDGE`; the simple-average combine is correct
  (it is the same function libaom computes, per the algebra above).

## 8. Regression

```
$ cargo test -p ec-av1 --lib -- lossless 444 422 \
      --skip bitrate_target_lands_within_5_percent_over_48_frames
test result: ok. 71 passed; 0 failed; 0 ignored; 0 measured; 719 filtered out

$ cargo test -p ec-av1 --lib -- warp obmc
test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 758 filtered out

$ cargo test -p ec-av1 --lib -- every_oracle_comparator_reds_on_a_one_byte_wrong_oracle
test result: ok. 1 passed; 0 failed
```
(the new gate is inside the 71 —
`a_lossless_444_altref_leaf8_warp_chroma_is_byte_exact ... ok`.) The two extra
filters cover the arms the hunk touches (warp, the per-plane bail, OBMC-skip
guards, inter leaves). `EC_AV1_REQUIRE_AOMDEC=1` was set for all three runs, so
no oracle arm silently skipped.

**INCOMPLETE, reported as such:** `cargo test -p ec-av1 --lib -- inter` (108
tests) does not fit one local run on this box — each test spawns `aomdec` and
many take 20-60 s. It was SIGTERM'd at ~350 s twice (once by the foreground-job
reaper, once by my own 340 s cap under `cargo nextest`, whose per-test
processes were in flight). Of the ~90 that finished, **0 failed**; the remaining
~18 never ran, so this is NOT a green verdict and must be finished on a fleet
host (standing remote-execution decision). Full-suite runs likewise stay on the
fleet.

## 9. Files

| file | change |
|---|---|
| `crates/ec-av1/src/decode.rs` | the fix: 4 chroma `warp_affine_compound` calls in the 8×8 leaf's compound arm (49333, 49369, 49432, 49468) + the single-ref per-plane bail tightened (50379) + the `EC_CPRED` rung (37173-37206, env-gated, no behaviour change) |
| `crates/ec-av1/src/stream.rs` | the gate `a_lossless_444_altref_leaf8_warp_chroma_is_byte_exact` (8395) |
| `crates/ec-av1/fixtures/ll444_c_altref_leaf8_warp.obu` | **new**, 512220 B, sha256 `999a41d8…ff2bb8`, the cell committed byte for byte (never re-encoded) |
| `scripts/fixture-library.tsv` | regenerated (`scripts/gen-fixture-library.sh`): adds the new pin's row and re-syncs every `required-by` line number shifted by this lane's insertion. The pre-existing file was already stale against main, so the diff is larger than this lane's own row. |
| `lanes/av1444altref.report.md` | this file |
| `~/.cache/aom-oracle/src/av1/decoder/decodeframe.c` | the `aom_ec_pict_idx` counter + the `EC_CPRED` rung (env-gated; the oracle tree is not a git repo, so this is the record of the patch) |

## 10. Open items (not fixed here)

* **The cell's `aomenc` recipe is still unrecorded.** The pin is exact
  (sha256 + length) and committed, but nobody can re-derive those bytes from a
  command; `lanes/av1422lpf.report.md` names the cell without a recipe. The
  manifest therefore carries provenance class `captured` with no recipe, which
  its own header calls a FINDING. Whoever has the av1422lpf shell history
  should record it.
* **The oracle tree is unversioned.** `aom_ec_pict_idx` and the `EC_CPRED` rung
  exist only in `~/.cache/aom-oracle/src`; a fresh `build-aom-oracle.sh` from
  the upstream tag will not have them. The two rungs this lane added are not in
  `scripts/instrument-aom-oracle.sh` (that script's decoder patch is guarded by
  an `EC_INSTRUMENTED` marker and no-ops on an already-patched tree, so adding
  them there needs a separate, idempotent-by-marker step).
* `MC_SIZE`-style ladder join between a per-block oracle dump and a per-unit
  decoder write is a trap worth remembering: compare per PIXEL, not per write.
