# lane-av1chromadc — the 4:4:4 2x2 chroma override window (79969 → 0)

Worktree `~/.cache/wt/av1chromadc`, branch `lane-av1chromadc`, base `57834ee2`
("Merge lane-av1chromarect-r2"). All edits absolute-pathed inside this worktree;
`git -C ~/Documents/Code/Rust/edith_codecs status --porcelain` printed nothing
after every edit batch.

Fixes the residual `lanes/av1chromarect-r2.report.md` narrowed, in
`decode_block`'s SQUARE path, and closes the witness to **full byte-exactness on
every plane of every frame**. `decode_intrabc_owned_rect` (the strip arm) is not
touched.

## 1. Red-before, on the UNPATCHED tree

Gate `a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit`
(`crates/ec-av1/src/stream.rs`), added first, asserting full frame exactness on
`fixtures/r512.obu` against the instrumented aomdec:

    $ EC_AV1_REQUIRE_AOMDEC=1 cargo test -p ec-av1 --lib a_444_skipped_64x64 -- --nocapture
    thread 'stream::tests::a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit' panicked:
    a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit: decode-order frame 0
    plane U is byte-exact nowhere -- first divergence at sample 229728 of 262144 (x=352, y=448),
    7488 samples wrong. The seed is frame 0, mi=(112,80) px=(320,448) side=64 skip=1, whose
    chroma unit cu=(1,0) at px=(352,448) must be the SECOND 32x32 window of the block's chroma
    override and not a copy of cu=(0,0)'s.
    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 768 filtered out

That is the unpatched decode diverging at the seed's exact sample, not a mutation
red. The 79969 total from r2 is the same measurement: 7488 + 8128 on frame 0,
22571 + 41782 on frame 1, 0 on frame 2.

## 2. The defect

A skipped 64x64 square block at 4:4:4 has a **64x64 chroma plane block**, which
libaom walks as a 2x2 grid of `TX_32X32` units (`get_vartx_max_txsize` →
`av1_get_adjusted_tx_size(TX_64X64) = TX_32X32`, decodeframe.c:994-1004).

Its chroma prediction is not edge-derived: it is the intra-BC / UV-palette
**override**, a whole-`chroma_side x chroma_height` buffer carried on the
`PALETTE_PRED` slot (`decode_block`'s `intrabc_bufs` / `palette_uv_bufs` →
`set_palette_pred` → `PlaneBuf::reconstruct`'s `pred`). `reconstruct` uses the
buffer it takes as the **unit's own** `chroma_tx x chroma_tx_h` prediction --
`let prediction = if let Some(buf) = pred { buf } else { … }` -- so a multi-unit
plane block must hand each unit its own WINDOW of that buffer.

The pre-fix call was `set_palette_pred(ub.clone(), fctx)`: the whole buffer, for
every unit. At 4:2:0 the chroma plane block of a 64x64 block is a single 32x32
unit, so the whole buffer IS that unit's window and the bug is **invisible** --
which is why every 4:2:0 gate stayed green. At 4:4:4 every unit took the buffer's
first 32x32.

The code already said so, at the site, for LUMA: *"its per-TU windowing is the
non-skip loop's own `palette_y_buf` slice, untested for skip and unreachable in
these streams"*, and `lane-svt1` had armed the whole-block luma buffer on the
skip arm for a related reason. The chroma half never got the same treatment, and
`r512.obu` is the stream that makes it reachable.

**Measured proof, before the fix** (`EC_PRED`, the per-unit prediction trace;
identical sums and identical `row0`/`col0` are the signature of a copied window):

    OUR_PRED x=320 y=448 plane=1 side=32 mode=0 sum=882176 row0=[241,241,241,241,241,241,241,241] col0=[241,202,241,202,241,202,241,202]
    OUR_PRED x=352 y=448 plane=1 side=32 mode=0 sum=882176 row0=[241,241,241,241,241,241,241,241] col0=[241,202,241,202,241,202,241,202]
    OUR_PRED x=320 y=448 plane=2 side=32 mode=0 sum=751936 row0=[110,110,110,110,110,110,110,110] col0=[110,222,110,222,110,222,110,222]
    OUR_PRED x=352 y=448 plane=2 side=32 mode=0 sum=751936 row0=[110,110,110,110,110,110,110,110] col0=[110,222,110,222,110,222,110,222]

and `EC_DEBUG_EDGES` printed **nothing** for x=320/352 at y=448, i.e. `edges()`
was never called -- the prediction came from the override, not from the edges.
(`EDGES_SQ` DOES print for the coded sibling at x=448, whose units therefore went
through the per-unit DC path and were already right.)

## 3. The fix

`decode_block`'s skip-arm chroma unit loop, both planes: one `chroma_window`
closure that returns the buffer whole when the plane block is a single unit and
otherwise slices rows `cu_row*chroma_tx_h .. +chroma_tx_h` × cols
`cu_col*chroma_tx .. +chroma_tx` at the buffer's own `chroma_side` stride.

Nothing else changes: the single-unit case (every 4:2:0 shape, every 4:4:4 shape
below 64) takes the identical path it took before, and the strip arm is
untouched.

## 4. Result

    $ EC_AV1_REQUIRE_AOMDEC=1 cargo test -p ec-av1 --lib -- a_444_intrabc_owned_rect a_444_skipped_64x64
    test stream::tests::a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit ... ok
    test stream::tests::a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes ... ok
    test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 767 filtered out

Per (frame, plane), wrong samples, before → after:

| frame | Y | U | V |
| --- | --- | --- | --- |
| 0 | 0 → 0 | 7488 → **0** | 8128 → **0** |
| 1 | 0 → 0 | 22571 → **0** | 41782 → **0** |
| 2 | 0 → 0 | 0 → 0 | 0 → 0 |

**79969 → 0.** Every plane of every frame is byte-exact against the aomdec
oracle, 2359296/2359296 samples. The floors in
`a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes` were
RAISED from `[[262144, 229728, 229728], [262144, 65704, 65704], [262144, 262144,
262144]]` to `[[262144; 3]; 3]`; both floors' history is recorded in the test's
doc comment. No floor was lowered.

## 5. Class sweep — all four unit positions, and which were already right

Every multi-unit chroma-override walk on the witness, with the window head the fix
now hands each unit (temporary `EC_ZZSRC` probe, removed before the commit;
`win_head` = the unit window's first eight samples):

| frame | block | skip | source | cu(0,0) | cu(1,0) | cu(0,1) | cu(1,1) | pre-fix state |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 0 | px=(320,448) side=64 | true | intrabc | 241 | 202 | 241 | 202 | **(1,0) and (1,1) WRONG** (both got 241) |
| 0 | px=(384,448) side=64 | true | intrabc | 202 | 203 | 202 | 203 | all four right -- the buffer is flat, so the copy was indistinguishable |
| 1 | px=(128,128) side=64 | true | intrabc | 54 | 54 | 54 | 54 | (1,0)/(1,1) wrong further into the window -- see §6 |
| 1 | px=(320,448) side=64 | true | intrabc | 240 | 202 | 240 | 202 | **(1,0) and (1,1) WRONG** |
| 1 | px=(448,384) side=64 | true | intrabc | 166 | 166 | 166 | 166 | all four right -- flat buffer |

The rule the sweep establishes: the bug is invisible when the override buffer is
constant across the plane block (rows 2 and 5 above, and both `cu_col == 0`
positions always), and wrong at exactly the `(1,0)` and `(1,1)` positions
whenever the buffer varies. `(0,0)` and `(0,1)` were never wrong -- they read
offset 0, which is their own window. That is why the dirty map in r2 §1 starts at
x=352 = 320 + 32 and not at x=320.

**Both edge families:** the intra-BC and UV-palette sources share one line
(`palette_uv_bufs` is `intrabc_bufs`' chroma half when intrabc is present, else
the palette map), so the fix is structurally shared. MEASURED, though: every
multi-unit walk on this witness is `src=intrabc`; the UV-palette source never
reaches a multi-unit chroma walk here, so its arm of the sweep is argued from
the shared code path, not witnessed. That is the one cell of the class this lane
does not have a measurement for.

Non-vacuity: new counter `SKIP_CHROMA_OVERRIDE_WINDOW_HITS` counts chroma units
whose override had to be windowed (`cn_cols * cn_rows > 1`), exclusive to this
walk. The gate asserts `>= 8` (two skipped 64x64 intra-BC blocks × 2×2 units × 2
planes) before it compares a single sample, so the exactness cannot be vacuous.

## 6. The f1 hypothesis: CONFIRMED

r2 §5 predicted f1's dirty column `x=128..192` (first dirty cell `(168,128)`,
56 of 64 samples wrong) was the skipped 64x64 at `px=(128,128)`. It is. That is
the only multi-unit override walk in that column, it is `skip=1 side=64 src=intrabc`,
and frame 1 goes from 22571 U + 41782 V wrong to **0** with this fix and no other
change. Its four windows' first eight samples are all `54`, so the difference is
further into the unit window than the head I printed — the block is flat near its
top-left and not flat at `(168,128)`, which is exactly the invisible-until-it-isn't
shape the sweep above describes.

## 7. Gates

`cargo check -p ec-av1 --all-targets` clean (0 errors, 0 warnings) before every
run, on a private `CARGO_TARGET_DIR`; a green gate next to a build error would
have been a stale binary.

    $ EC_AV1_REQUIRE_AOMDEC=1 EC_AV1_REQUIRE_AOMENC=1 \
      cargo test -p ec-av1 --lib -- a_444 lossless_444 intrabc_rect 444_lossless -- --nocapture
    test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 735 filtered out

34 = the 33 of lane-av1chromarect's r5 quote plus this lane's new gate. The set
carries the 4:4:4-lossless family (`a_lossless_444_*`,
`a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units`,
`a_real_aomenc_lossless_444_key_frame_decodes_sample_exact`), the intrabc_rect
family, and the 4:2:0 identity arm
(`a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint`, whose
`420_intrabc_rect4_witness.obu` twin goes through `decode_all_frames_vs_oracle`).

One filtered run per tree, same filter string, same env, private
`CARGO_TARGET_DIR` each:

    $ EC_AV1_REQUIRE_AOMDEC=1 EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib \
        -- rect intrabc ibc chroma 444 420 subsampl -- --nocapture

| tree | result |
| --- | --- |
| base `57834ee2` (`~/.cache/wt/av1chromadc-base`, detached) | `ok. 140 passed; 0 failed; 5 ignored; 0 measured; 623 filtered out; finished in 284.66s` |
| tip `lane-av1chromadc` | `ok. 141 passed; 0 failed; 5 ignored; 0 measured; 623 filtered out; finished in 288.94s` |

The set difference of the two passing-test lists is exactly this lane's new
gate and nothing else:

```
> test stream::tests::a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit
```

The same 5 tests are `#[ignore]`d in both trees.

### Mutation proofs, each reverted

| # | mutation | result |
| --- | --- | --- |
| M1 | `chroma_window` returns the whole buffer unconditionally (the pre-fix shape) | **RED** — `frame 0 plane U … first divergence at sample 229728 (x=352, y=448), 7488 samples wrong` |
| M2 | the window's row stride `chroma_side` → `chroma_tx` | **RED** — `frame 0 plane U … first divergence at sample 229824 (x=448, y=448), 7488 samples wrong` |
| M3 | `cu_row`/`cu_col` transposed in the window offset | **RED** — `frame 0 plane U … first divergence at sample 229728 (x=352, y=448), 7488 samples wrong` |

## 8. Not done

1. The UV-palette arm of the class sweep has no witness here (§5). It shares the
   fixed line, but a 4:4:4 `--enable-palette=1` stream with a skipped block of 64
   and above would be the measurement, and I did not encode one.
2. `decode_intrabc_rect` (decode.rs:13646) and the other rect paths read their
   chroma through `read_inter_rect_chroma` / the intrabc split walk, which
   already window per unit; I did not re-verify each of them against a
   multi-unit override, only that the wide family is unchanged.
3. No heavy encodes: the witness came from the pin and the gate families are
   fixture-driven. No VPS fleet run was requested or consumed.
