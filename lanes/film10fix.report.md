# lane-film10fix — the two "pre-existing 10-BIT FILM divergences": MEASURED, both already exact

Branch `lane-film10fix`, base `main` = `3f08ae3f`.

## 0. The charter's premise, measured first

The ticket asked me to fix or narrow two divergences recorded as deferred to a
"dedicated 10-bit-film round": (1) a film sample with warp + CDEF + inter-intra,
(2) small-side globalmv. Per the ticket's own instruction I reproduced both on
current `main` before believing the debt line. **Both are already exact.** The
debt line is stale.

There is no `lanes/` report that names a 10-bit-film deferral of these two
cases. The nearest match, `lanes/av1444.report.md:251-269`, records the
opposite: those two gates FAILED at `1ad7e0fc` and `b9530604` and were
attributed to that lane's own 4:4:4 `record_uv_mode_mi` change (class
`override-slot-on-one-arm`), then FIXED there. Six suite logs
(`lanes/dq-suite.log`, `fi-suite.log`, `libdet-suite.log`, `rdoq-suite.log`,
`sse-suite.log`, `tw-suite.log`, `tpl2/3-suite.log`, `i64/suite2.log`) carry
both gates as `... ok`.

### Case 1 — warp + CDEF + inter-intra
`crates/ec-av1/fixtures/troy_sb128_inter_witness.obu`, 177773 B,
sha256 `09d995946d27695ef4b7b1a24a4d7ad85f8e2c8176d58ee54e825efc061f9cbc`.

```
$ cargo test -p ec-av1 --lib -- a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_decode_pixel_exact --nocapture --test-threads=1
a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_decode_pixel_exact:
  15 shown frames pixel-exact on every plane; warp_plane_suppress=15
  cdef_idx=852 inter_sb128_vert=76 interintra_rect=118
  non_chroma_ref_ctx_skip=194 intra_in_inter_txctx=3787 edge_filter_mi_fix=206
  intra16x4_in_inter=(80, 163, 113) sub8_chroma_tx_from_ref=2
  inter_neighbour_not_smooth=370 tr_reach_longer_side=600 obmc_edge_span=33
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 780 filtered out; finished in 8.44s
```

All 13 path counters fire (every one `assert!(n > 0)` in the gate), so this is
not a gate that passes by not reaching the code.

### Case 2 — small-side globalmv
`crates/ec-av1/fixtures/gm_small_side_witness.obu`, 1158066 B,
sha256 `f1cdbcbb8d6d469c7957d70252ab42b24ab75f48a717600ffa25991458775dca`.

```
$ cargo test -p ec-av1 --lib -- a_10bit_film_frames_with_small_side_globalmv_and_rect_warp_reach_decode_pixel_exact --nocapture --test-threads=1
a_10bit_film_frames_with_small_side_globalmv_and_rect_warp_reach_decode_pixel_exact:
  33 shown frames pixel-exact on every plane; gm_nontrans_small_side=1
  tr_reach_longer_side=2295 uv_mode_grid_override=138
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 780 filtered out; finished in 21.33s
```

### Against `aomdec` too, on ALL decode-order frames
Both gates compare SHOWN frames only (ffmpeg emits shown frames only), so 1 of
16 and 1 of 34 decode-order frames per stream sit outside both gates — class
`gate-blind-to-hidden-frames`. Measured against the instrumented oracle's
`EC_AV1_FINAL_DUMP` (rung 12):

```
troy_sb128_inter_witness.obu:  16 decode-order frames (1 hidden) byte-exact vs aomdec
gm_small_side_witness.obu:     34 decode-order frames (1 hidden) byte-exact vs aomdec
```

**0 wrong on all three planes, all 50 decode-order frames, both cells.** No
PREFILT/POSTDEBLOCK/POSTCDEF/FINAL stage ladder was needed: there is no
divergence to localize, so no stage is suspect and none is exonerated by
comparison. Naming a mechanism and a libaom file:line here would be fabrication.

## 1. Non-vacuity control (the batch rule)

A green comparator is a measurement only if it can go red. Both film gates
declare "pixel-exact on every plane" using nothing but
`ffmpeg_decode_sequence_10bit`'s output — a helper that stopped reading
ffmpeg's stdout would leave both green on a decoder that could be arbitrarily
wrong.

**Measured with a PATH-level shim that runs the REAL ffmpeg and rotates exactly
one byte of its rawvideo stdout** (per plane, sample 1000; 1920x792 10-bit
frame = luma 3041280 B + chroma 760320 B, so byte 2000 / 3043280 / 3803600):

| case | arm | result |
|---|---|---|
| warp+cdef+interintra | byte 2000 (Y) | `FAILED … frame 0 luma vs ffmpeg` |
| warp+cdef+interintra | byte 3043280 (U) | `FAILED … frame 0 U vs ffmpeg` |
| warp+cdef+interintra | byte 3803600 (V) | `FAILED … frame 0 V vs ffmpeg` |
| small-side globalmv | byte 2000 (Y) | `FAILED … frame 0 luma vs ffmpeg` |
| small-side globalmv | byte 3043280 (U) | `FAILED … frame 0 U vs ffmpeg` |
| small-side globalmv | byte 3803600 (V) | `FAILED … frame 0 V vs ffmpeg` |

Six of six red, each naming the plane that was tampered with. The comparators
read the oracle.

## 2. What I found instead, and committed

Two real gaps, both closed.

### 2a. The ffmpeg comparator family had NO committed control

`every_oracle_comparator_reds_on_a_one_byte_wrong_oracle` (lane-av1cmpaudit2,
`3f08ae3f`) controls the five `aomdec` comparators. The `ffmpeg` half — the
half both charter-named film gates rest on — was audited **by reading only**
(`lanes/av1cmpaudit2.report.md:48-52`: "all reach ffmpeg through a real
`Command::output()` and compare the stdout — LIVE"). Reading proves it is live
today; only a deliberately wrong reference proves it can FAIL.

New gate **`every_ffmpeg_comparator_reds_on_a_one_sample_wrong_ffmpeg`**
(`crates/ec-av1/src/stream.rs`), 7 arms:

- `set_ffmpeg_path` / `ffmpeg_path()` — a THREAD-LOCAL ffmpeg override, same
  shape and same reason as the existing `set_oracle_path`: a process-global
  PATH edit would leak the tampered reference into every sibling test on
  another thread, and the crate denies `unsafe_code` so `set_var` is out.
- arm 1, real ffmpeg: 4:2:0 8-bit (320x240), monochrome (64x64 gray) and the
  10-bit film pin (1920x792) all count `[0, 0, 0]` / `0`. Without this arm,
  arm 2's red could just be a stream that was never exact.
- arm 2, one sample wrong **per plane**: `ffmpeg_decode_sequence` red on Y,
  U and V separately; `ffmpeg_decode_gray_sequence` red on one luma sample;
  `ffmpeg_decode_sequence_10bit` red on Y, U and V separately **of the 10-bit
  film pin** — the comparator both charter-named gates use. Per-plane arms, not
  one: a helper that dropped chroma would pass a luma-only arm and leave both
  film gates green with chroma unchecked for 15 and 33 frames.
- All three reference helpers now spawn through the override.

Mutation proofs (both reverted before commit):

1. Revert `ffmpeg_decode_sequence` to the bare `"ffmpeg"` (the override no
   longer reaches it) → `3 of 7 ffmpeg comparator arms stayed GREEN …
   counted [0, 0, 0], expected [1, 0, 0]`, `test result: FAILED`.
2. Make `ffmpeg_decode_sequence_10bit` return zeros for V instead of ffmpeg's
   samples → `arm 1's 10-bit film pin must be zero on every plane …
   left: [0, 0, 5702400], right: [0, 0, 0]`, `test result: FAILED`. That is
   the retracted `av1422remeasure` defect reproduced inside the 10-bit helper.

### 2b. The hidden alt-ref frame of each film stream was outside every gate

Both film gates' doc comments claim their hidden frames were compared
byte-exact against aomdec, but that measurement lived in a lane report
(`t900-r7`, `t900-r10`), not in a test, so nothing re-ran it. New gate
**`the_two_10bit_film_pins_match_aomdec_on_every_decode_order_frame`** runs
`decode_all_frames_vs_oracle` on both pins and asserts the frame COUNTS
(16 = 15 shown + 1 hidden, 34 = 33 + 1) before the byte compare, so a stream
that quietly lost its hidden ARF cannot make the compare cover less than it
claims.

Mutation proof (reverted before commit): one byte wrong in our own
`EC_AV1_FINAL_DUMP` write →

```
decode-order frame 0 of 16 (15 shown, 1 hidden) differs from the oracle at
byte 7 (ours 128 vs 0), 1 bytes differ
test result: FAILED
```

## 3. Sweep

Re-ran, on this branch, `EC_AV1_REQUIRE_AOMDEC=1 EC_AV1_REQUIRE_AOMENC=1
EC_AV1_REQUIRE_FIXTURES=1 EC_NOMEMGUARD=1`:

| cell | result |
|---|---|
| `a_10bit_128sb_film_frames_with_warp_cdef_and_interintra_...` | ok, 15 frames, 13/13 counters fire |
| `a_10bit_film_frames_with_small_side_globalmv_and_rect_warp_...` | ok, 33 frames, 3/3 counters fire |
| `a_10bit_film_frames_with_frame_edge_mv_clamping_...` | ok, 52 frames, mv_clamp_edge_overhang=5 |
| `a_10bit_film_frames_with_rect64_corner_tus_...` | ok, 33 frames, 64x32=148 32x64=123 |
| `a_10bit_film_hidden_arf_with_split_superblock_strips_...` | ok, 37 frames, rect64_split_txfm_publish=4 |
| `a_10bit_film_inter_frame_with_intra_1to4_strips_...` | ok, 127 strips, shown frame exact |
| `a_real_aomenc_10bit_film_grain_stream_...` | ok, grain_hits=1 |
| `the_two_10bit_film_pins_match_aomdec_on_every_decode_order_frame` (new) | ok, 16 + 34 decode-order frames |
| `every_ffmpeg_comparator_reds_on_a_one_sample_wrong_ffmpeg` (new) | ok, 7/7 arms red |
| `every_oracle_comparator_reds_on_a_one_byte_wrong_oracle` | ok, 5/5 comparators red |
| `a_real_libaom_monochrome_key_frame_...` (the gray arm's pin) | ok |

`cargo check -p ec-av1 --all-targets`: 0 errors, 0 warnings.

No 10-bit cell regressed. Nothing outside `crates/ec-av1/src/stream.rs` is
touched, so no other crate's gates are affected.

## 4. not_done

- **The 4:2:0 8-bit reference helper cannot be driven on an ODD-dimension
  stream.** `ffmpeg_decode_sequence` derives its frame size as `w*h + 2*(w*h/4)`,
  which is wrong for odd dimensions: 4:2:0 chroma on 65x65 is
  `ceil(65/2)^2 = 1089`, not `4225/4 = 1056`. Measured: it fails LOUDLY
  (`FRAME COUNT MISMATCH (4:2:0 65x65) … 6403 bytes, 66 byte(s) not a whole
  frame, at 6337 B/frame`), which is the designed behaviour of
  `frame_count_diagnosis` — not a silent wrong answer — so nothing is
  mis-gated. Left alone: out of charter, and the loud failure is the correct
  behaviour. Unblocked by a lane that teaches the helper
  `((w+1)/2)*((h+1)/2)`. My control uses 320x240 for this reason and says so.
- **No full suite run.** Per the batch rule, project-wide validation is Main's
  job. My sweep is the 11 cells above.
- **The 10-bit `aomdec` per-plane WRONG-SAMPLE COUNTS are 0 by whole-buffer
  byte equality**, not by a counting comparator: `decode_all_frames_vs_oracle`
  panics at the first differing byte. A per-plane count on a 10-bit cell would
  need a 10-bit arm of `count_rawvideo_diffs`; the zero here is "0 differing
  bytes across all 50 frames on all three planes", which is stronger than a
  count but is not a count.
