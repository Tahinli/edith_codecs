# lane-whtshape — a non-4x4 unit on the LOSSLESS WHT path

Branch `lane-whtshape`, base `a21f3680`, one commit
`lossless WHT: a non-4x4 unit is reconstructed as a 4x4 raster, not asserted away`.

Files touched: `crates/ec-av1/src/decode.rs`, `crates/ec-av1/src/transform.rs`,
`crates/ec-av1/src/stream.rs` (the new gate, appended at the end of the
`mod tests` block), and the new pin
`crates/ec-av1/fixtures/ll444_sb64_1to4_lossless.obu`.
Untouched as instructed: the `read_plane` coefficient-reader region (~19600) and
`decode_rect_split`.

---

## r1 — The charter's premise, measured and partly REFUTED

The ticket says a RELEASE build "silently runs a wrong-shape transform — silent
wrongness, not a panic". **That is wrong, and the measurement matters: RELEASE
panics.**

`TxParams::run`'s lossless arm was

```rust
LOSSLESS_WHT_UNITS.fetch_add(1, …);
debug_assert_eq!((self.w, self.h), (4, 4));
debug_assert_eq!(self.tx_type, TxType::DctDct);
crate::transform::dequant_and_inverse_wht4x4(grid, …)   // always 16 values
```

`dequant_and_inverse_wht4x4` dequantizes with `w = h = 4` and returns 16
samples. `TxParams::run` then re-lays them at `stride * h`, copying
`w` per row for `h` rows — so a `w * h > 16` unit indexes past the 16-long
buffer.

| profile | behaviour on the witness | where |
| --- | --- | --- |
| DEBUG | `assertion left == right failed / left: (8, 4) / right: (4, 4)` | `decode.rs:2787` |
| RELEASE | `panicked … range end index 8 out of range for slice of length 0` | `decode.rs:4034` (the strided copy in `push_mc_rect_tx`) |

So: no silent wrong pixels, but a *worse* bug than silence — the only thing
naming the invariant was compiled out of the profile that ships, and release's
failure message named neither lossless nor the transform. **No reachable
non-4x4 lossless shape is silent**: every such shape has `w * h > 16`, so the
strided copy always overruns. (An `8 x 2`-shaped unit would have been silent,
but AV1's minimum transform side is 4, so it is not constructible.)

### Reproduction

Neither witness named in the ticket exists as a pin; both had to be encoded.
Probe over all 48 pinned fixtures in a DEBUG build: **zero** trip the assert.
Two independent encodes, both 4:4:4 10-bit `--lossless=1 --profile=1 --i444`:

```text
ffmpeg -f lavfi -i testsrc2=size=<N>x<N>:rate=25:duration=1 -pix_fmt yuv444p10le -f rawvideo src.yuv
aomenc --codec=av1 -w N -h N --i444 --bit-depth=10 --input-bit-depth=10 --passes=1
       --end-usage=q --cpu-used=0 --lag-in-frames=0 --kf-max-dist=1 --limit=2
       --threads=1 --enable-rect-partitions=1 --enable-1to4-partitions=1
       --min-partition-size=4 --max-partition-size=64 --sb-size=64
       --tune-content=screen --enable-intrabc=1 --enable-palette=1
       --cq-level=60 --enable-tx-size-search=0 --lossless=1 --profile=1 --obu -o out.obu src.yuv
```

| witness | size | sha256 (16) | DEBUG | RELEASE (pre-fix) |
| --- | --- | --- | --- | --- |
| 256x256 | 51107 B | `fdb6467987095b99` | assert `(8, 4)` | `range end index 8 … length 0` |
| 640x480 | 186912 B | `5e647e3afe60b5f6` | assert `(8, 4)` | `range end index 8 … length 0` |

The ticket's `left: (4, 8)` did not reproduce; `left: (8, 4)` did, on both
witnesses. Same defect, transposed unit. 4:2:0 cannot produce the shape at all
(its smallest lossless chroma plane block is already 4x4) — which is why the
gate asserts a 4:4:4 10-bit header.

### Localisation

A temporary `#[track_caller]` rung on `TxParams::run` and on
`read_inter_plane_rect` (both removed before the commit) put every non-4x4
lossless unit on one pair of call sites, in `decode_intrabc_rect`'s var-tx arm:

```text
EC_RECTSITE plane=1 x=104 y=64 w=8 h=4 stride=8 at decode.rs:13392   (U)
EC_RECTSITE plane=2 x=104 y=64 w=8 h=4 stride=8 at decode.rs:13410   (V)
```

A census of `decode_intrabc_rect` over every pinned fixture shows its chroma
plane block is `bw / 2` by `bh / 2` — a hardcoded 4:2:0 halving — so a 16x8
luma block at 4:4:4 got an 8x4 chroma plane block, and the walk read it as ONE
rect transform unit.

---

## r2 — What libaom actually does, and the decision

Four independent places in the reference say a non-4x4 lossless transform unit
cannot exist:

1. `av1/decoder/decodeframe.c:140` — `read_tx_mode(rb, coded_lossless)`:
   `if (coded_lossless) return ONLY_4X4;` before the `tx_mode` symbol is even
   considered.
2. `av1/decoder/decodeframe.c:1117` — `read_tx_size`:
   `if (xd->lossless[…]) return TX_4X4;` before any tree.
3. `av1/common/blockd.h:1447` — `get_vartx_max_txsize`:
   `if (xd->lossless[…]) return TX_4X4;` for EVERY plane, which is what sizes
   the units `decode_token_recon_block` actually reads.
4. `av1/common/idct.c:50` — the WHT dispatch. Of
   `highbd_inv_txfm_add_{4x4,4x8,8x4,16x4,…}_c`, **only** the 4x4 one carries a
   `lossless` branch (`if (lossless) { av1_highbd_iwht4x4_add(…); return; }`).
   Every other size falls straight through to the IDCT network.

Point 4 is the decisive one for the "refuse or split?" question: libaom has no
non-4x4 lossless inverse transform to dispatch *to*, because points 1–3 make
the unit unreachable. So a non-4x4 lossless unit here is not a bitstream shape
to be decoded — it is this decoder's own unit geometry being wrong.

**Decision: split into 4x4 WHT calls, counted.** Not a refusal: refusing would
turn a geometry bug into a decode failure with no way to see the pixels, and
`TxParams::run` has no `Result` channel to the point where a refusal could be
surfaced (it is called from the recorded-replay worker as well as inline).
Splitting makes the function *total*: no shape panics, no shape returns a
buffer its caller cannot index, release and debug agree, and the numbers are
the ones a correct per-4x4 caller would have written for the same footprint.
`LOSSLESS_WHT_SPLIT_UNITS` counts every 4x4 tile reconstructed this way, so a
caller that regresses is *named* rather than absorbed.

The 4x4 fast path is the same single `dequant_and_inverse_wht4x4` call it
always was — `the_4x4_lossless_fast_path_is_unchanged` asserts byte-identity
with and without the shape dispatch.

The `debug_assert_eq!(self.tx_type, TxType::DctDct)` went with it: the WHT
takes no `tx_type` (libaom's `av1_iwht4x4_add` does not either) and
`av1_get_tx_type` already answers `DCT_DCT` on every plane of a lossless block,
so it asserted an argument nothing reads — a debug-only panic on a value
release ignores, i.e. the same divergence class.

---

## r3 — The call site, and the defect this lane does NOT own

`decode_intrabc_rect`, lossless arm:

* chroma plane block = `bw >> ss_x` by `bh >> ss_y` at `(px >> ss_x,
  py >> ss_y)` — `av1_get_max_uv_txsize` is `ss_size_lookup[bsize]`, no halving
  of its own, so at 4:4:4 it is the block's own footprint;
* walked as a `(cw/4) * (ch/4)` PLANE-MAJOR raster of TX_4X4 units through the
  existing `read_inter_chroma_lossless` (the same helper the inter 8x8 and 128
  paths use), matching `decode_token_recon_block`'s
  `for plane { for blk_row { for blk_col } }`;
* each unit's own coefficient context replayed **over** the whole-block
  `record_split_luma_rect_mi` from the composed grid — class
  `override-slot-on-one-arm`, the same one `decode_rect_split`'s
  `ll_chroma_units` replay exists for. (Getting this order wrong first cost
  the exact prefix below: with the replay *before* the record, frame 0's first
  divergence sat at x=226 and 18376 luma samples were wrong; after it, x=224
  and 16022.)

**Scoped to `lossless` on purpose.** The 4:2:0 halving is correct at 4:2:0.
Every pinned 4:4:4 stream that reaches `decode_intrabc_rect` does so LOSSY
(measured: the `lossless`-gated census over all 48 pins returns nothing), and
that 4:4:4 lossy footprint is a separate, already-named open cell —
`lanes/av1444rect.report.md`'s 4:4:4 intra-BC chroma. This lane does not move
it. A lossless-only split changes no green pin either way.

### OPEN after this lane (measured, not guessed)

Pinned witness, 256x256 4:4:4 10-bit, 2 frames, 196608 samples per frame:

| frame | plane | differing | first differing sample |
| --- | --- | --- | --- |
| 0 (key) | Y | 16022 / 65536 | 32992 → (x=224, y=128) |
| 0 (key) | U | 16985 / 65536 | 32992 → (x=224, y=128) |
| 0 (key) | V | 17047 / 65536 | 32992 → (x=224, y=128) |
| 1 (inter) | Y | 1681 / 65536 | 55008 → (x=224, y=214) |
| 1 (inter) | U | 4071 / 65536 | 9378 → (x=162, y=36) |
| 1 (inter) | V | 4078 / 65536 | 9378 → (x=162, y=36) |

On the key frame the fork is at the first column PAST the 16x8 lossless
intra-BC block at mi (32, 52) = px (208, 128) — that block's own 16x8 luma and
chroma are byte-exact, so the fork is at the block boundary, in the block that
follows it. That is an entropy desync in a *different* block's residual walk,
not the WHT shape: `LOSSLESS_WHT_SPLIT_UNITS` is 0 and no non-4x4 lossless unit
exists anywhere in the decode. Not chased here — it is a distinct defect in a
distinct path.

The gate therefore asserts a measured per-frame, per-plane exact **prefix**, and
says so in its own doc comment. Asserting "our output is wrong by exactly N"
would encode the defect as expected behaviour; the prefix is the honest form
and it only ever grows.

---

## r4 — Gate and mutation proof

`stream::tests::a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units`
(pinned fixture, length + FNV asserted first, 4:4:4 10-bit header asserted):

1. `intrabc_rect_lossless_chroma4_hits() > 0` — `INTRABC_RECT_LOSSLESS_CHROMA4_HITS`
   is EXCLUSIVE to the per-4x4 chroma walk (no other caller walks a rect
   intra-BC block's chroma), so `>= 1` cannot be satisfied by the old
   single-unit read.
2. `lossless_wht_split_units()` delta `== 0` — the shape-total entry is not
   quietly papering over a caller that is still wrong.
3. `lossless_wht_units()` delta `> 0` — not vacuous.
4. Both frames decode, and every decode-order frame's per-plane exact prefix
   matches the oracle's own `EC_AV1_FINAL_DUMP`.

Plus four unit tests in `transform::lossless_tx_tests` covering the total entry
directly (raster equality against independently-composed per-unit 4x4 calls for
8x4 / 4x8 / 8x8 / 16x4 / 4x16 / 16x16, the 4x4 fast-path identity, the
empty-grid marker, and the partially-zero grid whose other tiles hit the empty
marker — that last one caught a real index panic in my first draft).

### Mutation results (RELEASE profile, where the debug-only assert never ran)

```
MUT1  decode_intrabc_rect reverted to the single rect unit + the 4:2:0 halving
      -> RED  panicked at stream.rs:43682
         "the pinned witness was refused: unsupported: AV1 tile
          (a Golomb tail longer than this decoder reads)"
         test result: FAILED. 0 passed; 1 failed

MUT2  dequant_and_inverse_wht forced back to the 4x4-only path
      -> stream gate: GREEN (survives -- see finding below)
      -> lossless_tx_tests: RED
         a_non_4x4_lossless_unit_is_the_raster_of_4x4_wht_units  FAILED
           assertion left == right failed: 8x4: residual length  left: 16  right: 32
         a_lossless_split_whose_other_tiles_are_all_zero_still_reconstructs FAILED
           assertion left == right failed: 8x4 residual length  left: 16  right: 32
         test result: FAILED. 3 passed; 2 failed

MUT3  hit!(INTRABC_RECT_LOSSLESS_CHROMA4_HITS) removed
      -> RED  panicked at stream.rs:43687
         "no LOSSLESS rect intra-BC block walked its chroma as 4x4 units --
          the corrected route never ran, so this gate is measuring the old
          single rect unit (class gate-blind-to-feature)"
         test result: FAILED. 0 passed; 1 failed

LIVE  restored tree: stream gate ok, lossless_tx_tests 5 passed
```

**MUT2 survives the stream gate, and that is reported rather than papered over.**
Assertion (2) is a *negative* assertion: after the call-site fix no non-4x4
lossless unit exists on this stream, so `split == 0` holds whether or not the
total entry works. The stream gate therefore cannot be the positive coverage of
`dequant_and_inverse_wht` for a non-4x4 shape — the four unit tests are, and
they do bite. Do not read the green stream gate as "the split is exercised
here".

### Regression scope (release, named tests only — no full suite on this box)

```
lossless                 21 passed; 0 failed
444                      16 passed; 0 failed
intrabc_rect              5 passed; 0 failed
rect16x4_chroma_reach     1 passed; 0 failed
a_444_lossless_sb64       1 passed; 0 failed     (the new gate)
a_444_intrabc_rect4       1 passed; 0 failed
a_444_sb128_witness       1 passed; 0 failed
real_aomenc_mixed_lossless 1 passed; 0 failed
transform lossless_tx_tests 5 passed; 0 failed
```

43 named gates across the lossless / 4:4:4 / intrabc-rect surface, green in
release. `cargo check -p ec-av1` is warning-free.

## r5 — Follow-ups this lane deliberately did not take

1. **`read_block_tx_size_rect`'s lossless arm returns before writing the TXFM
   bands.** libaom's `parse_decode_block` (`decodeframe.c:1163`) calls
   `set_txfm_ctxs(mbmi->tx_size, xd->width, xd->height, …)` on the lossless
   path too; our early return skips it. Unreachable on a fully lossless frame
   (no `txfm_partition` symbol is read when `tx_mode == ONLY_4X4`), so it is
   latent, not measured-wrong. Not fixed here — it is a neighbour-band concern,
   not the WHT shape.
2. **The 4:4:4 lossy chroma footprint of `decode_intrabc_rect`** (`bw / 2`,
   `px / 2`): wrong at 4:4:4, out of this lane's scope, already named in
   `lanes/av1444rect.report.md`.
3. **The r3 entropy fork**, localised to the block following the first lossless
   16x8 intra-BC rect block on the key frame and to (162, 36) on the inter
   frame's chroma.
