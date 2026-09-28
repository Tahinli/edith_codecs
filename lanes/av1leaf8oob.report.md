# lane-av1leaf8oob: fix the 4:4:4 intrabc 8x8-leaf chroma OOB panic

Tree: lane-av1-leaf8oob @ 9e77c16b (base = lane-av1llpredgate2's pin).
Reproducer pinned at `crates/ec-av1/fixtures/444_leaf8_oob.obu`.

## Defect

`cargo run -p ec-av1 --example decode_probe` on the fixture died with

```
thread 'main' panicked at crates/ec-av1/src/decode.rs:3774:16:
index out of bounds: the len is 16 but the index is 16
```

(`PlaneBuf::reconstruct` via `exec_intra` via `decode_leaf8`, reached through
the `#[track_caller]` reconstruct calls at 4:4:4, 8x8 leaf.)

Root cause, ONE plane-math site as the ticket guessed: `decode_leaf8`'s
intrabc frame-copy closure sized the leaf's chroma prediction buffers with
the literal 4:2:0 halving of an 8x8 luma leaf — `vec![0u16; 4 * 4]` and
`predict_with_filter(.., 4, 4, ..)` for BOTH U and V — while the leaf's own
chroma reconstruct at 4:4:4 runs `csz == 8`. The 16-sample override rode the
`PALETTE_PRED` slot into `reconstruct` as an 8x8 prediction, which indexed
`prediction[row * 8 + col]` = 16 at row 2, col 0: `len 16, index 16`.

## Fix (source, not a clamp)

`crates/ec-av1/src/decode.rs`, `decode_leaf8` intrabc closure: the chroma
extent is now derived from the frame's subsampling,
`(cw, ch) = (8 >> ss_x(fctx), 8 >> ss_y(fctx))` for both the buffer size and
the `predict_with_filter` geometry, and the row subpel arg uses `ss_y` like
every sibling intrabc site (rect intrabc at 11570/11910/11920, the
`luma: bool` helper at 13899; bit-identical at every reachable ss since
4:2:2 refuses at the sequence header, so ss_x == ss_y in all decodable
streams). 4:2:0 still computes 4x4 — byte-identical there.

The one sizing heals all three consumers of the buffers: the TX8 and skip
arms consume them whole at `csz`, and the TX4 4:4:4-lossless arm windows
them at stride 8 inside `read_intra_chroma_lossless`
(`palette_window(buf, stride, ox, oy, 4, 4)` reads up to index 63), which
had the same OOB latent.

## Class sweep (hardcoded 4:2:0 chroma extent, decode.rs)

Swept every intrabc chroma prediction site in decode.rs:

| site | extent | verdict |
|---|---|---|
| `decode_leaf8` intrabc closure (18128/18134 pre-fix) | literal `4*4` | **THE BUG** — fixed |
| `decode_block` square intrabc (16735+) | `chroma_side = side >> ss_x` | ss-derived, clean |
| rect intrabc owned/rect4 (11557+, 11897+) | `cw * ch`, `cside` | ss-derived, clean |
| `predict_intrabc` helper (13899) | `w * h` params, `ss_x`/`ss_y` split | clean |
| `sub8_leaf_chroma444` (18927+) | `bw * bh` (already chroma units) | clean by construction |
| `decode_leaf_split4` 4x4 arm (19423+) | literal `4*4`/`4,4` | 4:2:0-only arm (chroma_444 returns earlier) — literal correct there |
| `decode_leaf_rect8` clamp arm (20106+) | literal `4*4`/`4,4` | same 4:2:0-only arm — literal correct there |

No second 4:4:4-reachable instance exists in decode.rs. Signature-class
note: lane-lossless128 hit the identical panic text (`len 16, index 16`)
from a 128-axis lossless extent — recurring class, both now pinned by
gates.

## Gate

`stream::tests::an_intrabc_8x8_leaf_chroma_frame_copy_at_444_decodes_without_the_oob`
(crates/ec-av1/src/stream.rs), modelled on the av1llpredgate2 pattern:

- Fixture provenance: the lane ticket's witness-hunt reproducer (s1a/s2e);
  3577 bytes, sha256
  `72bed2791d1ceba8be7926a052c7838c5d96d4f9d56d28fb69789a753d3593af`,
  FNV-1a64 `0x86b93a51fe16353c`; 320x256, 3 frames, 8-bit 4:4:4
  screen-content keyframes (the oracle's MB dump opens on unbroken
  inter-classified intra = intrabc blocks). The aomenc recipe is not
  independently recoverable in this lane — bytes pinned by length + FNV +
  oracle raw fingerprint. Force-added past the `fixtures` gitignore like
  every sibling fixture; missing file panics, no skip path.
- Assertions: fixture bytes (len + FNV), `decode_stream` succeeds (3
  frames, 320x256 — the pre-fix run died here), `leaf8_intrabc_hits`
  0 -> 4 non-vacuity, and the oracle aomdec still decodes the pinned bytes
  (raw 737280 bytes, FNV `0x38210bcbf572973c`).

### Red-before (mutation verification)

`git checkout --` of the fix (patch snapshotted to /tmp, restored
byte-identical, `diff`-verified): gate RED at
`decode.rs:3774:16 index out of bounds: the len is 16 but the index is 16`
— the exact ticketed panic. With the fix: green, standalone AND in the
full `intrabc lossless` filter batch (26 passed, 3 consecutive runs).

### Why the gate pins NO pixel baseline (measured, then scoped out)

Against the oracle aomdec (`--rawvideo`; comparison method ground-truthed:
`a_lossless_sb128_rect_intra_block_decodes_sample_exact` and
`a_lossless_444_min_partition8_inter_stream_decodes_sample_exact` are green
on this same tree), this stream DIVERGES, and not deterministically:

- per-frame differing (y, u, v) of 81920 samples/plane:
  (20811, 35467, 32320), (20308, 39177, 35994), (20131, 39588, 36601);
  frame 0's first differing LUMA sample at (275,128);
- under `EC_AV1_PLANE_SENTINEL=1` chroma counts move (u f0 35467 -> 35630)
  while luma stays put; sentinel-vs-uninit output diffing localises the
  dependence to 458 WHOLE 8x8 chroma blocks (frame 0: chroma x >= 96,
  y >= 64) reading samples this decode never wrote — ambient memory, so
  any pixel-exact assert drifts run to run (observed: u f1 39177 vs
  39126 vs 39138 across batch runs).

Attribution to this lane's diff: none. The fix touches only leaf8's chroma
prediction buffers; intra/intrabc luma reads only luma edges and the luma
plane, so a luma-first divergence cannot come from it, and the pre-fix run
never produced output (panic at the first intrabc 8x8 leaf).

Unified reading: ONE pre-existing decode desync on this stream around
frame-0 SB (3..4, 2) — after it, samples diverge from the oracle AND the
misread layout leaves whole chroma blocks unreconstructed (which is where
the never-written reads live). An initial-bounds witness would not have
seen this: the stream never decoded before this lane lifted the panic.

Disposition: `deferred(unblock: divergence lane — EC trace pairing against
the oracle on this fixture; the oracle's AOMMB dump covers only frame 0's
first MB rows, so pairing needs the env-gated rungs)`.

## Non-regression

- `cargo test -p ec-av1 --lib -- intrabc lossless`: 26 passed, 0 failed
  (3 consecutive runs, including the 4:2:0 intrabc gates
  `an_intrabc_tx4_leaf_chroma_inherits_the_luma_type_and_predicts_from_the_frame_copy`,
  the sb128 intrabc pixel-exact gates, and the 4:4:4 lossless
  minp64/minp8/sb128 sample-exact gates).
- `cargo check -p ec-av1`: 0 warnings.

## Hygiene

- Target dir `$HOME/.cache/cargo-target-av1leaf8oob` throughout.
- Throwaway instruments (`examples/oob_diff.rs`, `examples/oob_dump.rs`)
  deleted before commit; no other tree touched; nothing pushed.
