# lane-av1p1pin — pin recipe P1 and gate it byte-exact vs ffmpeg

Base: `9a535ccb` (main). Branch: `lane/av1p1pin`. No decode-path edit.

## r1 — The recipe did not drift; bytes are the report's bytes

Rebuilt from scratch in `/tmp/av1p1pin` with the report's exact lines
(`lanes/av1witness2recheck.report.md` r"Commands"), ffmpeg 8.1.2 on PATH and
`~/.cache/aom-oracle/build/aomenc`:

```
ffmpeg -y -f lavfi -i "nullsrc=s=320x256:rate=30:d=1,geq=lum='mod(floor((Y+T*160)/8)*67,255)':cb='128+20*sin(Y/17+T)':cr='128+20*sin(X/23+T*2)'" -pix_fmt yuv444p bands444.y4m
aomenc --codec=av1 --obu -o s1d.obu --passes=1 --threads=1 bands444.y4m --limit=3 --kf-max-dist=1 --cpu-used=2 --end-usage=q --cq-level=20 --sb-size=128 --enable-rect-partitions=1 --enable-1to4-partitions=1 --min-partition-size=4
```

| file | sha256 | size |
|---|---|---|
| `bands444.y4m` (input) | `fa6218399cce8dd9eaadb6e24642d31cb2d2bde505b39a8df237938492c642ee` | — |
| `s1d.obu` (stream) | `68773f2c6e01a2d211bf0213479accb4c2c360132c0e1bc3b326eab7861925b1` | **595 B** |

Both match the report verbatim, so the pin is the report's stream, not a
lookalike. Stop condition not hit.

## r2 — Pinned in-tree

`crates/ec-av1/fixtures/444_sb128rect_chroma_tile_witness.obu`, 595 bytes,
sha256 `68773f2c6e01a2d211bf0213479accb4c2c360132c0e1bc3b326eab7861925b1`
(FNV-1a `0xd4fa_ac7b_dccc_520b`, asserted in the gate). The name says the
shape: 4:4:4, 320x256, `--sb-size=128`, the 128-root rect chroma-tiling
witness. Provenance is in the gate's doc comment, not only here.

This is a NEW pin. The tree's only other 4:4:4 sb128 fixture is
`444_sb128rect_lr_witness.obu` (794 B, sha256 `27825e14…`), a different
stream; the report's r"Outcome" already established the P1 bytes lived only
under `/tmp`.

## r3 — Census on this tree, re-measured (not quoted from the report)

`EC_NOMEMGUARD=1 decode_probe s1d.obu` on `9a535ccb`:

```
OK: 3 frames decoded, 320x256
sb128_rect: edge_horz=0 edge_vert=6 inter_128x64=0 inter_64x128=0
part128: split=7 none=5 horz=0 vert=6 ab=[0,0,0,0] intra_horz=0 intra_vert=6
rect4_32: horz=0 vert=8 coded=8
```

Identical to the report's numbers. These are the values the gate pins.

## r4 — The gate

`stream::tests::a_444_sb128_rect_chroma_tile_witness_is_byte_exact`
(`crates/ec-av1/src/stream.rs`). Two arms, both load-bearing:

1. **Non-vacuity.** Deltas over the decode, under `lock_gate_counters()`:
   `sb128_rect_counters().1` (gathered edge-VERT 128 roots) `== 6`;
   `part128_census().3` (128-root `PARTITION_VERT`) `== 6`;
   `part128_census().6` (key-frame intra 128-axis) `== 6`;
   `rect4_32_counters().1/.2` (32x32 VERT strips, coded) `== (8, 8)`.
   Plus the two zeros that make the shape specific rather than incidental:
   `part128` HORZ `== 0`, gathered edge-HORZ `== 0`. A stream that stops coding
   the 128-root VERT shape reds here instead of passing on a decode that never
   reached the guarded walk (class `gate-blind-to-feature`).
2. **Pixel-exactness.** Every sample of all 3 frames, all 3 planes, vs
   `aomdec --codec=av1 --rawvideo` and vs `ffmpeg -f obu -pix_fmt yuv444p
   -f rawvideo`. Both oracles are real decoders, not shims. Each compare is
   guarded by a plane-length assert first, so a short plane cannot zip-truncate
   the comparison into a false zero (class `short-plane-hides-span`).
   `EC_AV1_REQUIRE_FFMPEG=1` turns a missing ffmpeg into a hard assert inside
   `have_ffmpeg()`, so the ffmpeg arm cannot silently skip.

Frame 0 is a key frame and the report's `intra_vert=6` is its census, so the
guarded walk is exercised on the key frame; frames 1/2 are inter.

## r5 — The gate is not vacuous: three mutations, three reds

Each mutation was applied to the gate only, run, then reverted. Nothing touched
`decode.rs`.

| # | mutation | result |
|---|---|---|
| A | `EDGE_VERT` expectation `6 → 7` (counter arm) | **RED** — `stream.rs:11385`, "gathered edge-VERT 128 roots moved (expected 7)" |
| B | `frames[1].u[7] += 1` before the oracle compares | **RED** — "aomdec frame 1 plane u: 1 samples differ" |
| C | perturb `frames[2].v[1234] += 1` AFTER the aomdec block, inside the ffmpeg arm only | **RED** — "ffmpeg frame 2 plane 2: 1 samples differ" |

A proves the non-vacuity assertion reads a real measured value, not a tautology
the decode cannot fail. B and C prove each oracle arm separately bites (C is the
one that matters for this task: it reds on the ffmpeg compare with aomdec
already green, so the ffmpeg arm is not riding on aomdec's verdict).

Post-revert: `EC_AV1_REQUIRE_FFMPEG=1 cargo test -p ec-av1 --lib
a_444_sb128_rect_chroma_tile_witness_is_byte_exact` → **1 passed, 0 failed**.

## r6 — What this does and does not claim

- Claims: these exact 595 bytes are permanently in-tree; on `9a535ccb` they
  decode to 3 frames that are byte-identical to ffmpeg and aomdec on all three
  planes; and that decode provably routes through the 128-root VERT chroma
  tiling path 6 times.
- Does not claim: the 4:4:4 class is exact. One stream, one shape. The
  report's own instrument limit ("one stream per predicate") carries over
  unchanged.
- Control inherited: the report shows these bytes panicked at `fe3e8418`
  (`decode.rs:17631`, the 128-rect chroma assert). So the pin is a live
  regression witness, not a fixture that always passed.
- Local scoped run only: one example build plus this one named gate test. No
  full suite, nothing pushed, nothing merged.

## Files

- `crates/ec-av1/fixtures/444_sb128rect_chroma_tile_witness.obu` (new, 595 B)
- `crates/ec-av1/src/stream.rs` (new gate at ~11310; one stale doc count
  "26 pinned 4:4:4 fixtures" → 27 in
  `the_pinned_444_quadrant_witness_...`'s comment, moved by this pin)
