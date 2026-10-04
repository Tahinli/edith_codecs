# lane-av1422mismatch9 — the 9 on `SB128RECT_REPLAY_SPAN_MISMATCH_HITS`

Base `main` @ `42b230c4`. Branch `lane/av1422mismatch9`.

**Verdict: not a pixel miss, asserted.**

`lanes/av1422gather3.report.md` recorded that
`SB128RECT_REPLAY_SPAN_MISMATCH_HITS` reads **9** on each of the two pinned
`422_intrabc_sb128_strip*.obu` cells while those cells are byte-exact against
ffmpeg, and that nothing asserted the 9 and nothing explained it. Both are now
settled.

## 1. What the increment compares

`decode_block_128rect`'s end-of-block chroma replay
(`crates/ec-av1/src/decode.rs`, the `for (plane, cu_mi, grid) in &units` loop at
the tail of the `else` branch):

```rust
let luma_span   = chroma_tx << ss_x(fctx);   // 64 at ss (1,0), chroma_tx 32
let luma_span_h = chroma_tx << ss_y(fctx);   // 32 at ss (1,0)
...
if luma_span != luma_span_h { hit!(SB128RECT_REPLAY_SPAN_MISMATCH_HITS); }
for (plane, cu_mi, grid) in &units {
    neighbours.record_mi_chroma(*cu_mi, luma_span, luma_span_h, *plane, grid);
    hit!(SB128RECT_CHROMA_REPLAY_HITS);
}
```

The two spans are the unit's LUMA extent on each axis: **width**
`chroma_tx << ss_x` and **height** `chroma_tx << ss_y`, both in luma pixels.
The counter fires when they differ.

## 2. The measured count

Ran `the_422_block_128rect_chroma_chunk_gather_stays_per_axis` on both pins
(build `42b230c4` + this change, `CARGO_TARGET_DIR` own dir):

| pin | size | fnv1a64 | replay (`.0`) | mismatch (`.1`) |
|---|---|---|---|---|
| `422_intrabc_sb128_strip.obu` | 1672 | `0x50f5cfc576e4cd00` | **72** | **9** |
| `422_intrabc_sb128_strip_notxsearch.obu` | 1675 | `0xd4936f252ff8cff0` | **72** | **9** |

Confirmed: **9 per pin**, and `72 = 9 x 8` — each of the nine blocks replayed
eight chroma units. The counting reader now asserts `72` and `9` per pin.

## 3. Not a pixel miss — one dumped unit

A temporary env-gated trace at the replay site (removed before commit) dumped
every replayed unit's `(plane, cu_mi, span_w, span_h)` on both pins:
**144 lines, all `wh=(64,32)`**, distinct coords `plane ∈ {1,2}` x
`cu_mi.1 ∈ {0,16,32,48,64,80}` (step `luma_span/4 = 16` mi) x
`cu_mi.0 ∈ {64,72}` (step `luma_span_h/4 = 8` mi). The nine are the same
shape; one unit is representative:

```
EC_IBC128_SPAN_TMP replay plane=1 cu_mi=(64,32) wh=(64,32)
```

Three facts make that write **clipped to the same cells the walk already
stamped**, not a sample the oracle lacks:

1. **The predicate is a shape test, not a divergence test.** `luma_span !=
   luma_span_h` is `chroma_tx<<ss_x != chroma_tx<<ss_y`, which at ss (1,0) is
   true **by construction for every 128-root rect block**. So 9 is simply the
   number of such blocks the five frames code; the counter cannot distinguish
   a correct 4:2:2 replay from a wrong one.
2. **The replay re-stamps with the identical per-axis pair the read walk
   used.** The in-loop stamp is
   `record_mi_chroma(cu_mi, luma_span, luma_span_h, plane_pass, &grid)`; the
   replay is `record_mi_chroma(*cu_mi, luma_span, luma_span_h, *plane, grid)`
   — same receiver, same `cu_mi`, same `(luma_span, luma_span_h)`, same grid.
   It rewrites the **same** `left[]`/`above[]` cells, so it cannot introduce a
   state the walk did not already publish. (The lane-av1-128rectspan defect —
   width passed on both axes — is pinned by the source-scan arm of the same
   gate, which counts the per-axis spellings, not by this counter.)
3. **`record_mi_chroma` writes no pixel.** Its body
   (`decode.rs:~11379-11384`) writes only `Neighbours::left[..][plane]` and
   `Neighbours::above[..][plane]` — entropy-context arrays, both clipped by
   `min(bound_h.saturating_sub(mi_r))` / `min(bound_w.saturating_sub(mi_c))`
   against the frame's mi bounds. Zero plane samples are touched, so the 9
   events physically cannot write a sample the oracle lacks. The pixel proof
   is the sibling pin `the_pinned_422_intrabc_sb128_strip_witnesses_decode_
   pixel_exact` (all five frames, every plane, byte-exact vs ffmpeg
   `yuv422p`), which passes on the same bytes.

## 4. What changed

| file | change |
|---|---|
| `stream.rs` | `the_422_block_128rect_chroma_chunk_gather_stays_per_axis` now reads the full `(replay, mismatch)` pair and `assert_eq!`s the measured **9** per pin, with the three-fact reason in the message; the gate doc gains the same note. |
| `stream.rs` | `sb128rect_chroma_replay_counters` doc: corrected from "No 4:2:2 gate reads this pair" to name the gate that now asserts `(72, 9)`. |
| `decode.rs` | `SB128RECT_REPLAY_SPAN_MISMATCH_HITS` doc: the predicate restated as the per-axis span **difference** (a 4:2:2 shape test, not the divergence test it was named for and not a pixel-miss signature); the `record_mi_chroma`-writes-no-pixel and byte-exact-pin facts recorded; "nothing asserts zero" now says the measured NINE is asserted instead. |

## 5. Red-before proof of the new assert

Flipped the expected count `9 -> 10`, reran:

```
assertion `left == right` failed: ...: 422_intrabc_sb128_strip.obu fired the 4:2:2
span-shape arm 9 time(s); NINE were measured and are NOT a pixel miss ...
  left: 9
 right: 10
test result: FAILED. 0 passed; 1 failed
```

Reverted to 9; the gate is green. The assert bites.

## 6. Green after

```
test stream::tests::the_422_block_128rect_chroma_chunk_gather_stays_per_axis ... ok
test stream::tests::the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_exact ... ok
test stream::tests::the_intrabc_128rect_chroma_chunk_walk_stays_per_axis ... ok
test result: ok. 3 passed; 0 failed
```

No decoder behaviour changed: comments/doc plus one `assert_eq!` in a test.

## 7. Left open (disposition)

- **The other 19 committed 4:2:2 cells' `mismatch = 0`** rests on the existing
  doc measurement, not on an assert added here — `accepted`: the pinned pair
  is the only cell class this counter is non-zero on, and the gate that would
  cover the rest is the corpus gate, out of this ticket's scope.
- **The counter is not a divergence detector.** Renaming/repurposing it to
  actually witness the lane-av1-128rectspan class is a separate change —
  `deferred(<unblock: a lane that owns the counter's semantics>)`. The
  source-scan arm already covers the defect.
