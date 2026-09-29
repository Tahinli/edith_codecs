# lane-av1lossy128x96 — the 128x96 4:4:4 lossy tx-size-search cell

**Tree.** `lane-av1lossy128x96` off `a21f3680`, worktree `~/.cache/wt/av1lossy128x96`.
Builds in a LANE-PRIVATE `CARGO_TARGET_DIR` (`~/.cache/cargo-target-av1lossy128x96`) —
see `skill://counterfactual-ab-target-dir-provenance` variant 5 for why a shared
one silently measures a sibling lane's decoder. `decode.rs` is untouched in the
final commit; the only change is a new gate in `crates/ec-av1/src/stream.rs`.

**Headline: the cell is already FIXED, and the measurement that says so is the
gate this lane adds.** The charter assumed the 128x96 U+V variant was unowned
and open. It is neither: commit `6c33f204` ("4:4:4 inter chroma units inherit
their own quadrant's luma tx_type") closes it, exactly as it closed the 128x128
twin. What was missing is the GATE — the fix's own witness is pinned at
128x128, so nothing was watching the 128x96 geometry.

---

## 1. Measurement: the cell, before and after the fix

Stream (encoded live, identical bytes on both trees):

```
ffmpeg -f lavfi -i "testsrc2=size=128x96:rate=25" -frames:v 4 -pix_fmt yuv444p -f yuv4mpegpipe -
aomenc --profile=1 --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=2
       --sb-size=64 --min-partition-size=64 --max-partition-size=64
       --enable-tx-size-search=1 --threads=1 --row-mt=0 --lag-in-frames=0
       --kf-max-dist=100 --limit=4 --obu -o out.obu -
```

21830 bytes, sha256 `7380bcbbfb791c6a369850357211223e20fab959423135951f6eb43ffbfcbebd`
-> fnv1a64 `0x1ba3fe9a4335bdbb`. Reference: the instrumented `aomdec`
(`EC_AV1_FINAL_DUMP`, decode order).

| tree | decode-order frame 0 / 1 / 2 / 3, bytes differing from aomdec |
|---|---|
| `4155c7c7` (the report's base, the fix's parent) | 0 / 0 / **608** / **692** |
| `6c33f204` (the fix) | 0 / 0 / 0 / 0 |
| `a21f3680` (HEAD) | 0 / 0 / 0 / 0 |

The 608 is the sweep's number, reproduced exactly. First wrong sample in
decode-order frame 2: byte 16258 of the dump, i.e. `U(2,31)`, ours 111 vs the
oracle's 112 — U and V only, +-1, luma exact, as the sweep recorded.

`--enable-tx-size-search=0` on the same recipe: 21296 bytes, sha256
`347edcc9f4899fb05b15174b82d37f6562dcc6fe7691af0ed9ba4efd6cc878c6`, byte-exact
4/4 at every tree. So the cell is gated on the SEARCH, not on the geometry.

## 2. Discriminator: dequant or inverse transform?

The charter's method, run before reading any code. `EC_DQCOEFF` on the oracle
(`plane= txsize= txtype= eob= nz=`) against ours (`OUR_DQ w= h= q= dcd= acd= tx=
nz=`), both narrowed to decode-order frame 2 with `EC_TRACE_COEFF_FRAME=2`: 134
units on each side.

**Oracle-vs-ours by INDEX is not a valid pairing** (80 of 134 "type
disagreements" on a byte-exact decode) — the two decoders walk units in
different orders, exactly the trap `skill://av1-bitexact-debugging` names. The
valid comparison is our own decode before and after the fix, which has a fixed
unit order:

| comparison | units whose `tx_type` changed | units whose dequantized coefficient map changed |
|---|---|---|
| ours pre-fix vs ours post-fix (frame 2, 134 units) | **4** (units 72, 76, 107, 111) | **0** |

**Verdict: dequant is exonerated, the transform-TYPE selection was the defect.**
The same coefficients were read and dequantized identically before and after;
four chroma units were handed the block-level luma `tx_type` instead of the
covering leaf's, and 6c33f204 changed exactly those four answers. Post-fix the
frame is byte-exact against the oracle, so the oracle's types are the post-fix
ones.

Elimination list, each row a measurement rather than an argument:

1. **Entropy** — bit-identical (sweep: 140820 `EC_SYMR` reads, zero divergence).
2. **Coefficients / dequant** — 0 of 134 units differ pre→post (this lane).
3. **Inverse-transform type selection** — 4 of 134 units differ pre→post (this
   lane); that is the whole difference.
4. **Filters (deblock / CDEF / LR) and prediction input** — exonerated upstream
   by the sweep's pre-filter dumps (chroma already wrong before deblock, luma
   exact), and by the fact that a type change alone reproduces the diff with
   search OFF making it disappear.

Nothing here needed a new fix, so this lane ships no decoder change.

## 3. Gate added

`a_real_aomenc_444_whole_64_root_tx_size_search_stream_decodes_pixel_exact_at_128x96`
in `crates/ec-av1/src/stream.rs`. Live aomenc (no fixture pin: the recipe is
deterministic), two arms:

| arm | flags | parsed `tx_mode` | bytes | verdict |
|---|---|---|---|---|
| tx size search on | `--enable-tx-size-search=1` | `TxMode::Select` on all 4 frames | 21830 | EXACT 4/4, `chroma_quad_leaf_tx_diff_hits() == 6` |
| control | `--enable-tx-size-search=0` | `TxMode::Largest` on all 4 frames | 21296 | EXACT 4/4, `chroma_quad_leaf_tx_diff_hits() == 0` |

**The search flag is spelled AND the parsed bitstream is asserted** — the
charter's requirement, and the same rule the tilemeasure lane applied to tiles:
aomenc keeps the first occurrence of a flag and an unspecified flag is
"unknown", not "on", so the gate asserts `tx_mode == TxMode::Select` per frame
on the search arm and `TxMode::Largest` on the control, which also stops either
arm from silently measuring the other's stream. Recipe identity is pinned by
length + fnv1a64 (sha256 in the doc comment), so an aomenc that stops producing
this cell fails before the pixel compare.

Non-vacuity chain, in order: `ss (0,0)` at 8 bits; bytes + fnv match; parsed
`tx_mode` per frame; every decoded plane 128*96 (full-resolution chroma at
`ss (0,0)`); `decode_all_frames_vs_oracle` (frame count and per-frame byte
length asserted before any sample); then the load-bearing counter
`chroma_quad_leaf_tx_diff_hits() >= 2` on the search arm and exactly 0 on the
control — the route counter alone is not a witness, it fires on blocks whose
four leaves agree, which is the shape the original no-op port looked green on.

**Red-before (reverted, reproduced).** Reverting ONLY the per-quadrant resolve
in the two copies of `decode_inter_block`'s 64x64 four-unit chroma arm — the
`covering_leaf_tx_type(...)` call back to the block-level `luma_tx_type`,
counters and this gate untouched — turns the gate red exactly as the sweep
described it:

```
decode-order frame 2 of 4 (4 shown, 0 hidden) differs from the oracle at byte
16258 (ours 111 vs 112), 608 bytes differ
```

Green again on restore (`git diff crates/ec-av1/src/decode.rs` empty).

**Regressions checked on this tree.** The 128x128 twin's own gate
`a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type`
(96 quad-resolved units, 8 of them differing, plus its 4:2:0 control) is green.
A 4:2:0 identity run of this lane's own recipe (same flags, `yuv420p` input)
decodes byte-exact 4/4 with the 4:4:4-only counters at 0.

## 4. What this lane does NOT claim

- It does not claim a new fix. The fix is `6c33f204`, already on main.
- It does not claim the 128x96 cell was ever distinct from the 128x128 twin:
  the two are the same defect (per-quadrant `tx_type` inheritance), and the
  128x96 form is the U+V variant of it on a frame whose second block row is
  half-height.
- It does not touch the 4:4:4 LOSSLESS chroma class (256x256 / 512x128), which
  is owned elsewhere and which the tilemeasure lane bounded with untiled
  controls.
