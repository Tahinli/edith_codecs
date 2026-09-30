# lane-av1cmpframe — `count_rawvideo_diffs` attributed planes by the FIRST picture

Base `6c3d079c` (merge of `lane-av1gapremeasure`), branch `lane-av1cmpframe`.
**This is an INSTRUMENT fix.** No decoder behaviour is touched: the diff is one
test-module helper plus one new gate and one committed fixture.

## 1. The defect

`count_rawvideo_diffs` (`crates/ec-av1/src/stream.rs:10374`) packed our decoded
pictures frame by frame — `pack_rawvideo` walks `decoded` and lays out each
frame's own `y`/`u`/`v` lengths — and then attributed every differing byte with
**frame 0's thresholds, forever**:

```rust
let f0 = decoded.first().expect("a decoded picture");
let (ys, us) = (w * h * bpp, f0.u.len() * bpp);
let pi = if i < ys { 0 } else if i < ys + us { 1 } else { 2 };
```

On a multi-frame stream that is wrong for every byte after frame 0's luma: all of
it lands in plane V. The totals and `frames_exact` stayed sound (they were
computed from `per` = one frame's size applied uniformly, which is right for the
uniform-size streams in the corpus), so **every gate that only asserted zeros
could not see it** — and the per-plane split is the only reason a per-plane
report exists.

The symptom that exposed it: on the pinned 320x236 stream it reported
`wrongV = 330 673` when plane V holds **302 080** bytes across the 16 frames. A
count larger than its own plane is not a plausible measurement.

## 2. The fix

The plane table is now built **per frame from each decoded picture's own plane
lengths** — the same source `pack_rawvideo` lays the bytes out from, and not a
formula, not `width * height`, not the first picture:

```rust
let mut spans: Vec<[usize; 4]> = Vec::with_capacity(decoded.len());
let mut off = 0usize;
for f in &decoded {
    let (y, u, v) = (f.y.len() * bpp, f.u.len() * bpp, f.v.len() * bpp);
    spans.push([off, off + y, off + y + u, off + y + u + v]);
    off += y + u + v;
}
assert_eq!(off, ours.len(), ...);
```

then a monotone cursor walks `spans` as the byte index advances, and
`frames_exact` compares each frame's own span instead of a fixed stride. A
stream whose frames differ in size is now attributed correctly as well — the old
code could not express that case at all. The signature is unchanged, so no caller
changed.

## 3. The control, and the mutation that proves it bites

New gate `the_counting_oracle_diff_attributes_planes_per_frame` on a new
committed pin `420_oddheight_320x236_diverging.obu` (16562 B, sha256
`84e4d1ab56620af1c5b78e1aa3d2d67496a492f2e127198c82dd1d68b01c6200`, fnv1a64
`521889434652397870`; the recipe and the one-flag bisect that reaches it are in
`lanes/av1gapremeasure.report.md` §5).

It counts the truth **itself** — running `aomdec --rawvideo` again, packing our
pictures, and walking its own per-frame plane table — and then asserts the
helper agrees with it, on three axes:

1. **per-plane split** equals the independent per-frame count, and
2. `frames_exact` equals the independently counted exact frames, and
3. **a byte inside frame 5's U plane** (`spans[5][1]`, i.e. a byte ~5 frames in)
   moves **U** by exactly one and leaves Y and V at their baseline values. This
   is the arm the old code cannot pass: that byte is past every frame-0
   threshold, so the old attribution put it in V.

Measured on the fixed tree:

```
pinned 420_oddheight_320x236_diverging.obu as wrongY=234349 wrongU=56957
wrongV=52981 over 16 frames, 0 exact; this test's own count
[234349, 56957, 52981] with 0 exact frames; plane capacities
[1208320, 302080, 302080]
```

**Mutation (the fix reverted to the first-picture form, one hunk):**

```
left:  [11198, 2416, 330673]
right: [234349, 56957, 52981]
```

with the named message `the helper's per-plane split must equal an independent
per-frame count`. The old numbers are reproduced digit for digit, including the
impossible `V > capacity`.

The gate deliberately does **not** assert that the stream diverges: it is a
control for the instrument and must keep biting after the odd-height entropy fork
is fixed. The flip arm's expected `frames_exact` is written to accept either
case (`truth_exact` when the frame is already wrong, `frames - 1` when it was
exact).

## 4. Caller sweep — who else attributes planes, and are they frame-aware

| site | how it splits planes | verdict |
|---|---|---|
| `count_rawvideo_diffs` (`stream.rs:10374`) | was frame 0's `width*height` / `u.len()`, applied to every byte | **FIXED here** |
| `oracle_plane_diffs` (`stream.rs:49951`) | reads ONE `EC_AV1_FINAL_DUMP` file per decode-order frame and splits that frame with the caller's `plane_samples` | already frame-aware |
| the 10-bit film arm of `every_ffmpeg_comparator_reds_on_a_one_sample_wrong_ffmpeg` (`count10`, `stream.rs:11008`) | `for (a, b) in our_10bit.iter().zip(theirs)` then per-plane over each pair's own `y`/`u`/`v` | already frame-aware |
| the 8-bit arm of the same control (`count8`, `stream.rs:10975`) | `our_8bit[0]` / `theirs[0]` — **frame 0 only** | not a mis-attribution: the arm's subject is one 320x240 frame and it is documented as such |
| `assert_rawvideo_matches` / `decode_all_frames_vs_oracle` | byte-equality per decode-order frame, no plane attribution at all | not applicable |
| the `320x240` offset arithmetic in the ffmpeg shim control (`stream.rs:10805`) | asserts its own geometry (`ys == 76800`, `cs == 19200`) for a single even-sized frame | correct by assertion |

So `count_rawvideo_diffs` was the only site with the defect.

## 5. Re-measured numbers for the two open cells (now attributed)

With the fixed comparator, live `aomdec --rawvideo`, and a `flip` byte-0 control
on each cell:

| cell | bytes | wrong Y | wrong U | wrong V | frames exact | flip-0 control |
|---|---|---|---|---|---|---|
| `320x236` (the committed pin, sha256 `84e4d1ab…`) | 16562 | **234349** | **56957** | **52981** | 0/16 | Y 234349 → 234350 |
| `322x248` (sha256 `9e8c9c6c…`) | 16131 | **36286** | **13359** | **13001** | 0/16 | Y 36286 → 36287 |
| `320x232` (the committed pin, sha256 `6832c3cd…`) | 15773 | 0 | 0 | 0 | 16/16 | Y 0 → 1, exact 16 → 15 |
| `320x248` (sha256 `e4a4f71d…`) | 16440 | 0 | 0 | 0 | 16/16 | Y 0 → 1 |
| `324x236` (sha256 `11687cc7…`) | 16683 | 0 | 0 | 0 | 16/16 | Y 0 → 1 |

**These are the debt's own numbers, reproduced by the crate's own instrument.**
The debt line reads "`320x236` (Y 234349, U 56957, V 52981) and `322x248` (Y
36286, U 13359, V 13001)" — the fixed helper now emits exactly those figures.
The earlier lane that recorded them used a different, frame-aware comparator;
the helper was the broken one, and the debt was right.

## 6. Earlier reports whose headline number was a mis-attributed split

- **`lanes/av1422anom.report.md:90` and `:176`** — the `Y_420_320x232` row,
  "`0 | 200 | 17658`", and its total "17858". Produced by this helper on a
  16-frame stream, so the U and V columns are not that cell's U and V: only
  frame 0's chroma could reach U, and frames 1–15's chroma was all counted as V.
  **What it should now read:** the totals (17858 differing bytes, 0/16 frames
  exact) stand; the per-plane split does not, and no corrected split is
  recoverable because the cell is byte-exact on main today. The conclusion the
  row supports — a real 320x232 chroma divergence, since fixed by
  lane-av1422chrtx — is unaffected, because the divergence was non-zero either
  way.
- **`lanes/av1422h232.report.md:31-35`** — the red-before table
  (`420_oddheight_320x232.obu` "0 | 5772 | 12086", and the same from an
  independent re-encode). Same defect, same conclusion: the totals and 0/16 are
  sound, the U/V split is not.
- **`lanes/av1cmpaudit2.report.md:71`** — quotes "17658 wrong chroma of 593920"
  from the same measurement to argue `count_rawvideo_diffs` is a LIVE comparator.
  The liveness argument survives (the count is non-zero and comes from the
  oracle's real bytes); the "of 593920" denominator does not describe a plane.

No other report quotes a non-zero per-plane split from this helper. Every
zero-valued result in the corpus is unchanged by the fix, because a zero is a
zero under any attribution.

## 7. What was run

Scoped, on this branch, with `EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMDEC=1
EC_AV1_REQUIRE_AOMENC=1 EC_AV1_REQUIRE_FFMPEG=1`:

- `the_counting_oracle_diff_attributes_planes_per_frame` — new, green.
- `the_counting_oracle_diff_detects_one_flipped_oracle_byte` — the pre-existing
  non-vacuity control, green.
- `the_pinned_420_oddheight_witness_is_pinned_and_decodes_byte_exact` — the
  pre-existing caller that asserts `(0, 0, 0, exact == frames)`, green.
- `every_oracle_comparator_reds_on_a_one_byte_wrong_oracle` — the control for
  the whole comparator family, green.
- the mutation of §3, red with the named message.

Full-suite validation is the orchestrator's; the diff is one helper, one gate,
one fixture.

## 8. `not_done`

- The odd-height entropy fork itself (`320x236`, `322x248`) is **not** touched:
  no decoder behaviour changed here, and the fix is an instrument fix.
- No second pin was added for `322x248`; its numbers in §5 come from a live
  encode with the recipe in `lanes/av1gapremeasure.report.md` §5. Committing a
  fixture nothing gates would be dead weight; if the fork's owning lane wants a
  witness, it should commit the pin together with the ratchet gate it needs.
- The mis-attributed U/V splits in the three reports of §6 are **not**
  retro-corrected in those files. The debt is Main's to update and this lane was
  told not to touch it; the corrected figures for the still-diverging cells are
  in §5 for whoever writes the debt text.
- `count8` in the ffmpeg control still scores frame 0 only. That is the arm's
  declared scope, not a defect, so it was left alone.
