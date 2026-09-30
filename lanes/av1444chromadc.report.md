# lane-av1444chromadc — debt line 21 is ALREADY FIXED on main; this lane proves which fix, red-before and green-after

Worktree `~/.cache/wt/av1444chromadc`, branch `lane-av1444chromadc`, base `81a21c7d`
("Merge lane-av1422llinter2"). **No decode-path change is committed.** Every
number below is measured on this tip; the probe that produced them was reverted
and `git status` is clean, as is the primary checkout.

## 0. Disposition, in one line

**Debt line 21 does not reproduce. `(352,448)` is 202, matching the oracle, and
all three planes of all three frames are byte-exact. The defect was fixed by
`e05a6ceb` ("4:4:4 2x2 chroma walk: window the intra-BC/palette override per
unit"), merged into main as `2c0fd2b9` ("Merge lane-av1chromadc").** The
deliverable is therefore the red-before/green-after proof of that commit, the
class sweep, and this line retired.

`git merge-base --is-ancestor e05a6ceb main` succeeds: the fix is on main.

    $ sha256sum crates/ec-av1/fixtures/r512.obu
    f587f0f980f149abce905e3ef66f2f1634f050c85430626eafb9c30a1e25bc21

The committed fixture is byte-identical to the witness the debt line was filed
against (`~/.cache/whtshape/r512.obu`, same sha256), so this is the same stream,
not a lookalike.

## 1. RE-MEASURE on this tip (step 1 of the ticket)

A temporary probe decoded `fixtures/r512.obu` with both this decoder
(`set_final_dump_prefix`) and the instrumented oracle
(`/home/tahinli/.cache/aom-oracle/build/aomdec`, `EC_AV1_FINAL_DUMP`), 512x512
8-bit 4:4:4, in DECODE order, and read the seed's four chroma units directly.

    PROBE frame 0
    PROBE  f0 plane Y wrong=0
    PROBE  f0 plane U wrong=0
    PROBE  f0 plane V wrong=0
    PROBE  f0 seed mi=(112,80) skip=1 px=(320,448)
    PROBE   plane1 (0,0)@320,448 aom=241 our=241 (1,0)@352,448 aom=202 our=202 (0,1)@320,480 aom=241 our=241 (1,1)@352,480 aom=202 our=202
    PROBE   plane2 (0,0)@320,448 aom=110 our=110 (1,0)@352,448 aom=222 our=222 (0,1)@320,480 aom=110 our=110 (1,1)@352,480 aom=222 our=222
    PROBE frame 1
    PROBE  f1 plane Y wrong=0
    PROBE  f1 plane U wrong=0
    PROBE  f1 plane V wrong=0
    PROBE frame 2
    PROBE  f2 plane Y wrong=0
    PROBE  f2 plane U wrong=0
    PROBE  f2 plane V wrong=0

The debt line's exact claim — "its right chroma unit cu=(1,0) at (352,448) has
left=202 above=194 corner=118 yet reconstructs flat 241 (the sibling's value;
the ORACLE has 202)" — is **false on this tip**: ours is 202, the oracle's value.
79969 wrong samples (3.4% of 2359296) is now **0**. Per-frame, as filed by r2:

| frame | Y | U | V |
| --- | --- | --- | --- |
| 0 | 0 wrong | 7488 -> **0** | 8128 -> **0** |
| 1 | 0 wrong | 22571 -> **0** | 41782 -> **0** |
| 2 | 0 wrong | 0 | 0 |

## 2. The named expression (libaom file:line vs ours)

The ticket's hypothesis was that the second unit "reconstructs its SIBLING's
flat DC", i.e. that the DC predictor gathered its above row / left column over
the wrong extent. **That hypothesis is wrong, and the r2 report that filed it
said so.** The prediction of a skipped intra-BC block is not edge-derived at
all; it is the intra-BC / UV-palette colour-map override. `av1_build_dc_predictor_sb`
is never reached on this path.

libaom, `av1/common/reconintra.c:1717-1739`, inside `av1_predict_intra_block`:

    1717:   if (use_palette) {
    ...
    1721:     const uint16_t *const palette =
    1722:         mbmi->palette_mode_info.palette_colors + plane * PALETTE_MAX_SIZE;
    1723:     if (is_hbd) {
    1724:       uint16_t *dst16 = CONVERT_TO_SHORTPTR(dst);
    1725:       for (r = 0; r < txhpx; ++r) {
    1726:         for (c = 0; c < txwpx; ++c) {
    1727:           dst16[r * dst_stride + c] = palette[map[(r + y) * wpx + c + x]];
    1728:         }
    1729:       }
    1730:     } else {
    1731:       for (r = 0; r < txhpx; ++r) {
    1732:         for (c = 0; c < txwpx; ++c) {
    1733:           dst[r * dst_stride + c] =
    1734:               (uint8_t)palette[map[(r + y) * wpx + c + x]];

**The offending expression is `map[(r + y) * wpx + c + x]`, reconintra.c:1727 and
:1734.** `wpx` is `pd->width`, the WHOLE chroma plane block's width, and `map` is
the whole plane block's colour-index map
(`xd->plane[plane != 0].color_index_map + xd->color_index_map_offset[...]`,
line 1719). The unit's own position enters as `x` and `y`
(`1711-1712`: `x = col_off << MI_SIZE_LOG2`, `y = row_off << MI_SIZE_LOG2`),
where `col_off`/`row_off` are the `blk_col`/`blk_row` passed down from
`av1_predict_intra_block_facade(cm, xd, plane, col, row, tx_size)`
(`reconintra.c:1966-1969`) — which is called ONCE PER TRANSFORM UNIT from
`predict_and_reconstruct_intra_block` (`av1/decoder/decodeframe.c:232`), whose
`row`/`col` are the unit's own. So libaom reads the shared map **at the unit's
own offset**.

Ours, pre-fix, `crates/ec-av1/src/decode.rs`, `decode_block`'s skip arm — the
whole buffer for every unit, with no per-unit offset:

    set_palette_pred(ub.clone(), fctx);     // plane 1
    set_palette_pred(vb.clone(), fctx);     // plane 2

Ours, post-fix (`decode.rs:22274-22284`), the unit's own window:

    let chroma_window = |buf: &[u16]| -> Vec<u16> {
        if cn_cols == 1 && cn_rows == 1 {
            return buf.to_vec();
        }
        let mut window = Vec::with_capacity(chroma_tx * chroma_tx_h);
        for row in 0..chroma_tx_h {
            let at = (cu_row * chroma_tx_h + row) * chroma_side + cu_col * chroma_tx;
            window.extend_from_slice(&buf[at..at + chroma_tx]);
        }
        window
    };

`chroma_side` in the stride is libaom's `wpx`; `(cu_row * chroma_tx_h + row,
cu_col * chroma_tx)` is libaom's `(r + y, c + x)`. That is the whole fix, and it
is the same expression on both sides.

Why it was invisible at 4:2:0: a 64x64 block's 4:2:0 chroma plane block is a
single 32x32 unit, so the whole buffer IS that unit's window and
`cn_cols * cn_rows == 1` takes the early return. At 4:4:4 the plane block is
64x64 = a 2x2 grid of `TX_32X32` (`get_vartx_max_txsize` ->
`av1_get_adjusted_tx_size(TX_64X64)`), so `(1,0)`, `(0,1)` and `(1,1)` all read
offset 0.

## 3. The class sweep: 4 positions x {skipped, coded} x edge families

Step 3 of the ticket asked to sweep the class, because a unit whose data is
taken from the wrong unit can hide when a neighbour happens to supply the same
values. Measured with the pre-fix shape (mutation M1) against the oracle, all
four unit positions of every multi-unit override walk on the witness, plus the
coded arm:

    M1 = pre-fix whole buffer, decode.rs chroma_window -> buf.to_vec()

| frame | block | coding | (0,0) | (1,0) | (0,1) | (1,1) | buffer varies? |
| --- | --- | --- | --- | --- | --- | --- | --- |
| f0 | mi=(112,80) px=(320,448) | skipped (intrabc) | aom 241 / **our 241** OK | aom 202 / **our 241** WRONG | aom 241 / **our 241** OK | aom 202 / **our 241** WRONG | yes (241 vs 202) |
| f0 | mi=(112,96) px=(384,448) | skipped (intrabc) | aom 202 / our 202 OK | aom 203 / our 202 **off by 1** | aom 202 / our 202 OK | aom 203 / our 202 **off by 1** | barely (202/203) |
| f0 | mi=(112,112) px=(448,448) | **coded** skip=0 | aom 166 / our 175 WRONG | aom 166 / our 171 WRONG | aom 166 / our 179 WRONG | aom 166 / our 175 WRONG | n/a (victim) |
| f1 | mi=(32,32) px=(128,128) | skipped (intrabc) | aom 54 / our 54 OK | aom 54 / our 54 OK | aom 54 / our 54 OK | aom 54 / our 54 OK | no (constant 54) |
| f1 | mi=(112,80) px=(320,448) | skipped (intrabc) | aom 240 / our 240 OK | aom 202 / **our 240** WRONG | aom 240 / our 240 OK | aom 202 / **our 240** WRONG | yes (240 vs 202) |
| f1 | mi=(96,112) px=(448,384) | skipped (intrabc) | aom 166 / our 166 OK | aom 166 / our 166 OK | aom 166 / our 166 OK | aom 166 / our 166 OK | no (constant 166) |
| f2 | mi=(112,96) px=(384,448) | skipped (intrabc) | aom 202 / our 202 OK | aom 202 / our 202 OK | aom 202 / our 202 OK | aom 202 / our 202 OK | no |

The V plane mirrors U on every row of this table (f0 seed `(1,0)`: aom 222 /
our 110; f1 seed `(1,0)`: aom 222 / our 109), so the table holds for both
chroma planes.

Same table AFTER the fix — every cell reads `our == aom` at all four positions,
for every block, skipped and coded.

**The rule the sweep establishes, and it is the ticket's edge-family question
answered by measurement rather than argument:** a unit is wrong **iff** the
override buffer VARIES across the plane block, and it is then wrong at exactly
`(1,0)` and `(1,1)`. `(0,0)` and `(0,1)` are never wrong under M1 — they read
offset 0, which is their own window. This is precisely why r2's dirty map starts
at `x = 352 = 320 + 32` and not at `x = 320`. The three constant-buffer rows
(f1 mi=(32,32) at 54, f1 mi=(96,112) at 166, f2 mi=(112,96) at 202) are the
hiding case the ticket predicted: the defect exists there in the code and is
invisible in the pixels, so a single sample could never have found it.

The `mi=(112,96)` row is the subtler one: the buffer varies by **one** (202 vs
203), so the defect is a 1-LSB error at `(1,0)`/`(1,1)` that survives a
"looks about right" reading of the same cells.

Edge families: the override path is edge-free — `av1_build_dc_predictor_sb` and
`have_top`/`have_left` (`reconintra.c:1744-1747`) are on the other side of the
`use_palette` early `return` at line 1738. So "above-only / left-only / both /
neither" do not apply to this defect: there is no edge family. That is the
finding, and it corrects the ticket's premise.

**The coded arm was already correct and I verified it by reading, not by
assuming**: `decode_block`'s multi-unit coded chroma walk
(`decode.rs:22546-22559`) already calls `palette_window(pbuf, chroma_side,
cu_col * chroma_tx, cu_row * chroma_tx_h, chroma_tx, chroma_tx_h)` per unit —
the same per-unit offset the skip arm was missing. The `mi=(112,112)` row above
is that arm: under M1 it goes wrong only because it is downstream collateral of
the seed's corruption, and after the fix all four positions are exact.

The one cell of the class that is **argued, not measured**: the UV-palette
source (`palette_uv_bufs` from `PALETTE_PRED`, as opposed to the intra-BC
`intrabc_bufs`) shares the exact `chroma_window` line, so it is fixed by
construction, but no multi-unit UV-palette walk exists on this witness to prove
it. Every multi-unit override walk on `r512.obu` is `src=intrabc`.

## 4. The gate, and its mutation proof

The gate already exists on main and is the one this debt line needs:

* test `a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit`
  (`crates/ec-av1/src/stream.rs:52383`)
* fixture `crates/ec-av1/fixtures/r512.obu`, **6948 B**, sha256
  **`f587f0f980f149abce905e3ef66f2f1634f050c85430626eafb9c30a1e25bc21`**,
  4:4:4 8-bit 512x512, three frames
* non-vacuity: asserts `SKIP_CHROMA_OVERRIDE_WINDOW_HITS >= 8` before it
  compares a single sample, so the exactness cannot pass on a witness that
  stopped reaching the 2x2 walk
* asserts FULL three-plane, three-frame exactness (all 2359296 samples)

Mutation proof, run here, each mutation reverted and the file restored with
`git checkout --`:

| # | mutation | result |
| --- | --- | --- |
| M1 | `chroma_window` -> `buf.to_vec()` (the pre-fix whole-buffer shape) | **RED** — "decode-order frame 0 plane U ... first divergence at sample 229728 of 262144 (x=352, y=448), 7488 samples wrong" |
| M2 | window row stride `chroma_side` -> `chroma_tx` | **RED** — first divergence at sample **229824 (x=448, y=448)**, 7488 wrong |
| M3 | `cu_row`/`cu_col` transposed | **RED** — first divergence at sample 229728 (x=352, y=448) |
| restore | `git checkout -- crates/ec-av1/src/decode.rs` | **GREEN** — `1 passed; 0 failed` |

M2 diverging at a *different* sample (229824, not 229728) is the check that the
gate is not a single-point probe: it catches the stride and the origin
independently.

## 5. The f1 witness (step 5)

**Proven, not assumed.** f1's `mi=(32,32)` skipped 64x64 at `px=(128,128)` is
the same defect, and it is the only multi-unit override walk in r2's dirty
`x=128..192` column. Under M1 its four units are aom 54 / our 54 — correct *only
because its buffer is constant*, which is exactly why a sample-level
investigation could not have attributed f1's 22571 wrong U samples to it. What
attributes it is the per-frame plane count: f1 goes 22571 U + 41782 V wrong ->
0 with this fix and no other change, and the class sweep above shows the
constant-buffer rows become wrong the moment the buffer varies.

## 6. Regression

    cargo test -p ec-av1 --lib -- 444 420 422 lossless warp intra \
      --skip bitrate_target_lands_within_5_percent_over_48_frames

    test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 618 filtered out; finished in 266.84s

181 passed / 0 failed / 2 ignored on tip `81a21c7d`. The two ignored are
pre-existing `#[ignore]` gates unrelated to this lane; no floor was lowered and
the only test skipped is the one the ticket named
(`bitrate_target_lands_within_5_percent_over_48_frames`, the ~7 h debug test).

## 7. Retired

Debt line 21 is **retired, already closed on main** by `e05a6ceb` (merge
`2c0fd2b9`), with the gate and the mutation proof re-verified on this tip. No
new code is needed. The one open cell of the class is the UV-palette source at a
multi-unit 4:4:4 walk: it is fixed by the same line but has no witness, so it
stays an argued cell. Whoever wants it closed needs a 4:4:4 stream with a
skipped 64x64 block in `PALETTE_PRED` on chroma — `r512.obu` and
`r512_rect*_444_palette.obu` do not contain one.
