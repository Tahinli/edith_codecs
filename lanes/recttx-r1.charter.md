# lane-recttx r1 charter — rectangular inverse transforms in transform.rs

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-recttx, branch lane-recttx @ 836a002.
Build/test ONLY:
  `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-recttx CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND. Never push. Never touch the main checkout or the sibling
-gm / -intradisp / -screen worktrees (three other lanes are live in them).
`fixtures` is a symlink — leave it alone.
WIP COMMIT after every green milestone. Budget your turns; commit compiling
state before you run low.

## Why this lane exists (measured)
`crates/ec-av1/src/transform.rs` is entirely square: every entry point takes a
single `side: usize` and asserts `dequant.len() == side * side`. That single
fact is the ceiling under two separate capability gaps found this batch:
- intra HORZ/VERT strips (32x16 / 16x32) can read every symbol but can only
  reconstruct SKIP blocks, because `av1_get_max_uv_txsize` hands chroma a
  genuinely rectangular TX_16X8 / TX_8X16 with no square escape;
- `decode.rs:8172` refuses every inter partition below 16x16 outright, so the
  8x8-OBMC gate has never fired a single 8x8 OBMC block in 80 attempts.

This lane does NOT wire decode.rs. It makes transform.rs able to do the
rectangular transform correctly, with its own unit tests, so the follow-up
lanes have something to call.

## Scope
Generalize the inverse path from `side` to `(w, h)`, keeping the CURRENT raster
coefficient convention (`dequant[i * w + j]`, row-major) — libaom's own buffer
is column-major (`input[c * txfm_size_row + r]`), so do NOT copy its indexing;
copy its ARITHMETIC.

1. `inverse_transform_2d_typed(dequant, w, h, bit_depth, tx_type)`; keep a
   `side`-taking wrapper so no existing call site changes in this lane.
   Same for `dequant_and_inverse_typed`. `crate::quant::dequant` also takes
   `side` — widen it the same way (check what its `side` is actually used for
   before assuming it is only a length).
2. The rect scale, exactly: when `abs(log2(w) - log2(h)) == 1`, each row's
   input is scaled BEFORE the row transform by
   `round2(coeff * 2896, 12)` (libaom `NewInvSqrt2 = 2896`,
   `NewSqrt2Bits = 12`, av1_inv_txfm2d.c:272-276; spec 7.13.3 says the same
   with the literal 2896). When the log2 ratio is 0 or 2, NO scale.
3. Row shift by tx size, from libaom `av1_inv_txfm_shift_ls`
   (av1_inv_txfm2d.c:132-158). `shift[0]` is the row shift, `shift[1]` is
   always -4 (our existing `round2(t[i], 4)` column shift is already right).
   The square values our `row_shift(log2)` already returns are 4x4:0, 8x8:1,
   16x16:2, 32x32:2, 64x64:2 — confirm that, then extend with the rect rows:
     4x8:0  8x4:0  8x16:1  16x8:1  16x32:1  32x16:1  32x64:1  64x32:1
     4x16:1 16x4:1 8x32:2  32x8:2  16x64:2  64x16:2
   Key it on `(w, h)`, not on a single log2.
4. Clamp ranges are unchanged in shape: row clamp `bit_depth + 8`, column
   clamp `max(bit_depth + 6, 16)`; the row pass clamps over `w` entries and
   the column pass over `h`.
5. The coded-coefficient region: the existing `if i < 32 && j < 32` zeroing
   generalizes to `min(w,32) x min(h,32)`. Get the axes the right way round.
6. `inverse_1d` is already per-axis (`log2`, kind) — the row pass uses
   `log2(w)` and the column pass `log2(h)`. `inverse_identity` keys its scale
   on the axis LENGTH, so make sure each axis passes its OWN length; an
   identity row on a 32x8 block is a 32-point identity, not an 8-point one.
   This is the single easiest place to swap the axes; test it explicitly.
7. FLIPADST: the existing two-buffer read/write with `lr_flip` / `ud_flip` is
   correct — just re-index it for `(w, h)`. `lr_flip` mirrors over `w`,
   `ud_flip` over `h`.

## Tests you must write (this is most of the lane's value)
The oracle binaries are at ~/.cache/aom-oracle/build/ and the pinned libaom
source at ~/.cache/aom-oracle/src.
- A per-size pin: for each of the 14 rect sizes, one fixed input coefficient
  block through our `inverse_transform_2d_typed` compared against values
  TRANSCRIBED from libaom. Get those values by writing a tiny C harness that
  links or copies `av1_inv_txfm2d.c`'s `av1_inv_txfm2d_add_<w>x<h>_c` (or
  drives the facade) — put the harness in `lanes/recttx_dump.c` with its
  expected output checked in, the same shape as the already-landed
  `lanes/intrarect_dump.c` + `.expected.txt` pair on main; read that pair
  first and copy its structure.
  IMPORTANT (class `reference-layout-not-spec`): libaom's input buffer is
  column-major. When you feed the harness, feed the SAME logical coefficients
  our function gets and transpose at the boundary — then a transposed-axis bug
  in our code still shows up, instead of being cancelled by a transposed
  harness. Use an ASYMMETRIC input (different values per row AND column) for
  every case; a symmetric probe cannot see an axis swap.
- A transposed-pair sweep (class `scan-weights-cross-axis`): every WxH case is
  run alongside its HxW twin in the SAME test, so an axis swap fails loudly.
- Keep the existing square tests passing byte-identically — the square path
  must not move at all.

## Done criteria
Square behaviour bit-identical (full lib suite `-p ec-av1 --release --lib`
must stay 224 passed / 0 failed); all 14 rect sizes pinned against libaom;
WIP commits; REPORT `lanes/recttx-r1.report.md` with VERDICT on the FIRST line,
the per-size pass table, and an explicit statement of what a decode.rs lane
still has to do to USE this (rect scan order, rect eob context, rect
`max_txsize_rect_lookup` threading) — that wiring is OUT of scope here.
