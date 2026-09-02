# lane-golomb r1 — the Golomb-tail refusal is NOT a bound defect

## Verdict: the charter's premise is dead; no refusal lifted, no gate owed

The charter asked to raise our `read_golomb` bound because "our reader refuses beyond some
smaller bound" than libaom's. **It does not.** libaom's own `read_golomb`
(`~/.cache/aom-oracle/src/av1/decoder/decodetxb.c:22-42`):

```c
static int read_golomb(MACROBLOCKD *xd, aom_reader *r) {
  int x = 1; int length = 0; int i = 0;
  while (!i) {
    i = aom_read_bit(r, ACCT_STR);
    ++length;
    if (length > 20) {
      aom_internal_error(xd->error_info, AOM_CODEC_CORRUPT_FRAME,
                         "Invalid length in read_golomb");
      break;
    }
  }
  ...
```

`decode.rs:3871`'s loop starts at `length = 1` and increments per leading-zero bit, so both
sides read the same number of bits and both stop at `length > 20` — the bound is
**byte-identical**, and it fires on the identical bit pattern (20 zeros, then anything).
libaom calls that stream a CORRUPT FRAME. Raising our bound is therefore *not* "what libaom
does": it would convert a clean refusal into silent pixel corruption. The i32 accumulator and
the dequant clamp are moot — we never get a value to clamp.

The refusal comment at `decode.rs:3872` already said this ("this cap MASKS A REAL DEFECT");
lane-golomb r1 re-measured it and the comment was right. Class: `refusal-hides-a-defect`.

## Measurement (step 1 of the charter, done)

New env-gated rungs, behaviour otherwise unchanged (commit 58fad3e4):
- `crates/ec-av1/src/decode.rs:3871` — `read_golomb(dec, w, h, pos, base)`; under `EC_GOLOMB=1`
  it prints the block shape, coefficient position, base level, golomb length and msac range at
  the first firing coefficient.
- `crates/ec-av1/src/stream.rs` (`EC_FRAME_OK`) — under `EC_FRAMES=1`, one line per
  successfully decoded frame in DECODE order, so a mid-stream refusal names a frame index.

On the 10-bit 3840x1608 stream's 10 s head cut (`~/.cache/hg-0-10s.obu`):

```
EC_FRAME_OK decode_idx=79 show=false type=Inter order_hint=78
EC_GOLOMB_LONG w=32 h=32 pos=674 base=15 length=21 rng=58888 cnt=58887
REFUSED: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)
```

80 frames (decode idx 0..79) decode; the wall is **decode-order frame 80** (frame header
`HDR 118: type=Inter show=true order_hint=77 primary_ref=4 refresh=0x00 base_q=64`). The
requested level is above `15 + 2^20` at **high-frequency coefficient 674 of a 32x32
coefficient grid** — no real coefficient carries that. Symptom, not capability.

A header-field novelty scan over all 81 decoded headers found no coding tool switched on for
the first time at frame 80 (only new order-hint / reference-permutation values), so it is a
block-level defect.

## Where the desync is NOT (cross-decoder ladder)

Instrument: our `EC_TRACE_MODE_STEP` `EC_ISTEP ... name=tx_depth` vs the instrumented aomdec's
own print at the same site (`decodeframe.c:1130`). Calibration: line 0 of frame 0 is identical
on both sides (`mi_row=0 mi_col=0 name=tx_depth val=2 ctx=0 rng=51246`), so the two prints sit
at the same syntax position and `rng` is directly comparable. (The `EC_PART` rung is NOT
usable this way: on pixel-exact frame 0 its `rng` differs from aomdec's from the second
superblock on, so the two `EC_PART` prints sit at different syntax points.)

Two-pointer alignment over the whole cut (42576 ours / 38818 aomdec `tx_depth` lines; the two
have complementary *print* gaps — ours has no print for the bottom partial superblock row
`mi_row=400` nor for 64x16 edge strips, aomdec has none for some of ours):

- **Exactly two divergence points, both in decode frame 57** (ours `(96,568,val=0,ctx=2,rng=37256)`
  vs aomdec `(106,536,val=0,ctx=2,rng=59000)`, region mi_row 96..110 / mi_col 536..624).
  Frame 57 is `show=true` with **`refresh_frame_flags = 0x00`** — it refreshes no reference
  slot, so it cannot feed frame 80's CDFs, motion field or references.
- After resync the ladder matches element for element **all the way to the end of our trace**,
  including every traced symbol of frame 80. Frame 80's last traced symbol before the refusal
  is `mi_row=336 mi_col=912 name=tx_depth val=0 ctx=2 rng=52228`, and aomdec's value there is
  identical.

So frame 80 is **entropy-exact right up to the failing block**: the defect is inside the
coefficient read of the block at, or immediately after, the superblock at
`mi_row=336 mi_col=912` (pixel 1344, 3648) of decode frame 80. Inference (not yet proven): the
run of `tx_depth` prints stepping `mi_col` by 16 means 64x64 blocks, and `val=0` on a 64x64
block is `TX_64X64`, whose coefficients are coded in a 32x32 grid — which matches the refusal's
`w=32 h=32`.

## EVIDENCE

- `EVIDENCE: ~/.cache/golomb-tmp/hdrs.txt | EC_PROBE_HDR=1 decode_probe on the 10 s head cut, field-novelty scan over 81 decoded headers | no coding-tool field first-occurs at decode frame 80`
- `EVIDENCE: (stderr, quoted above) | EC_GOLOMB=1 EC_FRAMES=1 decode_probe ~/.cache/hg-0-10s.obu | 80 frames decoded; refusal at decode frame 80, w=32 h=32 pos=674 base=15 length=21`
- `EVIDENCE: ~/.cache/golomb-tmp/ours.istep2 + ~/.cache/golomb-tmp/aom.istep | ours EC_TRACE_MODE_STEP vs aomdec --limit=80 EC_TRACE_MODE_STEP, two-pointer alignment on (mi_row,mi_col,val,rng) | 2 divergence points, both decode frame 57 (refresh=0x00); frame 80 identical through mi_row=336 mi_col=912 rng=52228`

## Residue

- `deferred: the decode-frame-80 coefficient defect at mi_row=336 mi_col=912 — the tx_depth ladder is bit-exact up to it, so the next rung is a coefficient ladder (ours EC_TRACE_COEFF vs aomdec's EC_COEFF/EC_COEFF_STEP, tags line up) restricted to that frame — unblocked by a frame-gated coeff trace on our side (a full-stream EC_TRACE_COEFF over 80 4K frames in a debug build is too slow to pipe), which is a ~10-line change to the trace guard.`
- `deferred: the decode-frame-57 divergence (mi_row 96..110, mi_col 536..624) — separate defect, refresh=0x00 so it is a shown-frame pixel defect only, not the film wall — unblocked by a per-frame pixel compare of frame 57 vs ffmpeg.`
- `accepted: no refusal lifted, so refusal_inventory.rs and gate_coverage.rs are untouched and no new gate is owed. The charter's gate/fixture half is void with its premise: there is nothing to gate, because a >20-zero Golomb tail is a corrupt stream by libaom's own definition.`

## Suite

`systemd-run --user --unit=golomb-suite-r1-1788373224 -p MemoryMax=10G --same-dir bash -lc 'EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-golomb nice -n 10 cargo test -p ec-av1 --lib -j3'`
→ `test result: FAILED. 425 passed; 1 failed; 37 ignored; 0 measured; 0 filtered out; finished in 843.21s`

The one failure is
`stream::tests::a_10bit_film_frames_with_intra_16x4_strips_and_rect64_corner_tus_decode_pixel_exact`:
`the intra 16x4/4x16 strip arm did not fire (16x4=0 4x16=0 chroma_ref=0)` — a firing-counter
assert, reproduced in isolation (`--exact`, 54.89s, so it is not parallel-counter
contamination). **It is not attributable to this lane's diff**: this commit adds two
env-gated `eprintln!`s and threads four already-computed values into an error path; it
touches no partition, transform or counter code. Same shape as the ledger's recorded
`real_aomenc_1to4_streams_...decodes_pixel_exact` RED-ON-MAIN case.
`deferred: confirm this test on main 5d81a67 in a detached verify worktree — not done here (tool-call cap) — unblocked by one `git worktree add --detach` + its own CARGO_TARGET_DIR.`
`refusal_inventory` and `gate_coverage` are unmodified by this lane and pass in the suite run.
