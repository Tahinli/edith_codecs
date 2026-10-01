# lane-av1422liftverify — refute-first pass over the 4:2:2 lift

**Subject**: `b7a36001` (code) + `dc07ddb3` / `eb808df0` / `2ebb86ba` (report), branch `lane/av1422lift`.
**Base**: `4f206ec3` (`main`). **Tip**: `2ebb86ba`.
**Question answered**: does the lift change any behaviour for the formats it is not about, and are its
new assertions / fixtures / gates / comments honest?

## Trees and instruments (all absolute, nothing in the primary checkout)

| role | path | head |
|---|---|---|
| review worktree (read-only on the branch) | `/home/tahinli/.cache/wt/av1422liftverify` | `2ebb86ba` |
| base worktree for the A/B | `/home/tahinli/.cache/wt/mainref_4f206ec3` | `4f206ec3` (detached) |
| mutation worktree (throwaway, restored) | `/home/tahinli/.cache/wt/liftmut` | `2ebb86ba` (detached) |

Three separate `CARGO_TARGET_DIR`s (`wt-target-liftverify`, `wt-target-mainref`, `wt-target-mut`) so no
binary is ever shared across trees. Oracle: ffmpeg 8.1.3 on this host. Every 4:2:2 gate was run with
`EC_AV1_REQUIRE_FFMPEG=1` so the `have_ffmpeg()` early return cannot turn into a silent green.

## Verdict per item

| # | item | verdict |
|---|---|---|
| 1 | 4:2:0 / 4:4:4 decode is byte-identical to `4f206ec3` | **CONFIRMED** — 45/45 cells, byte-for-byte |
| 2 | `!(ss_x == 0 && ss_y == 1)` holds for every shape the parser can produce; right place | **CONFIRMED** |
| 3 | REFUSALS row + PROVEN tuple removed together; inventory gates still bite | **CONFIRMED** — 3/3 mutations red |
| 4 | the two new fixtures: pins, genuineness, not the old header/tile pin, gates assert pixels | **CONFIRMED** |
| 5 | the six renamed `the_pinned_422_*` gates are real pixel claims and they bite | **CONFIRMED** — 6/6 red on a 1-LSB error |
| 6 | corrected comments do not claim more than the code does | **REFUTED** — 6 sites, one of them contradicted by measurement |

Nothing in items 1–5 is wrong. Item 6 is: the lift replaced four *true* "no committed test can read
this counter" statements with *false* "the corpus gate reads this counter" statements, and one of the
four is contradicted by the branch's own committed fixture.

---

## 1. Format-independence — CONFIRMED

**Method**: `examples/dump_yuv` on **both** trees (per-frame planar YUV, display order, one byte per
sample at 8-bit and little-endian `u16` at 10/12-bit, sample depth taken from the stream's own parsed
sequence header, never from the file name), then `sha256` over the concatenated frames of each side
and a string compare. 45 committed fixtures, 13.9 GiB-free, two `cargo build`s, no oracle involved —
this is our-decoder-vs-our-decoder, and `dump_yuv` writes `decode_stream`'s returned picture, which
is **post**-film-grain (unlike the pre-grain `EC_AV1_FINAL_DUMP` rung), so the grain row is a
full-strength identity claim and not a weakened one.

```
for c in <cell>; do
  $BDBIN $BR/$c $OUT/b/$n/p ; $MDBIN $MAIN/$c $OUT/m/$n/p
  bh=$(cat $OUT/b/$n/p.f*.yuv | sha256sum) ; mh=$(cat $OUT/m/$n/p.f*.yuv | sha256sum)
done
```

### Batch 1 (24 cells) — `identical=24 differing=0`

| format / depth | cells | result |
|---|---|---|
| 4:2:0 8-bit | `av1_192x128_8bit_intra64_in_inter`, `palette_screen_witness`, `filter_intra_8x16_strip_seed49`, `420_odd65x65_key`, `420_oddheight_320x236` | IDENTICAL |
| 4:2:0 10/12-bit | `intra14_256x192_10bit` (10), `av112bit-key` (12, profile 2), `screen12_palette` (12, profile 2) | IDENTICAL |
| 4:2:0 **lossless** | `420_lossless_arf_1to4_320x240_5f`, `420_mixll_320x240_6f`, `lossless_sb128_rect_kf` | IDENTICAL |
| 4:2:0 superres | `superres_kf_cdef_lr_64x64`, `superres_alltools_sb128_320x180` | IDENTICAL |
| 4:2:0 grain | `grain_cdef_lr_128x128` | IDENTICAL |
| 4:4:4 8-bit | `444_lossy_rect4_wide_256x128`, `444_quad_leaf_tx_type`, `r512_rect1x2_444_palette`, `444_lossy_rect4_odd_130x122` | IDENTICAL |
| 4:4:4 10/12-bit | `444_lossy_palette_chroma_352x242_10b`, `444_palette_chroma_hstrip_640x480_12b`, `444_lossy_superres_256x128_d12_12bit` | IDENTICAL |
| 4:4:4 **lossless** | `ll444-lossless-key`, `ll444_128root_lossless` (10), `ll444_minp8_inter` | IDENTICAL |

### Batch 2 (21 cells) — `identical=21 differing=0`

`hg_ss300_key_frame`, `hg_ss600_key_frame`, `mix176_offtile_chroma_clip`, `ii-flake-{1,5,9}` (24f
each), `rect-flake-{1,3}` (24f each), `golden3-pin`, `golden6-mismatch`, `hg_kf900`, `troy_kf2700`,
`hg_arf_witness` (37f), `lr-sgr-r7`, `420_intrabc_rect4_witness`, `444_intrabc_rect4_witness`,
`444_intra_in_inter_128root_mu_chroma`, `intra128_in_inter_witness`,
`palette_screen_strip16_witness_10bit`, `r512_rect2x1_1x2_444`.

**Determinism control.** The largest cell (`hg_arf_witness.obu`, 37 frames, 18.5 MB/frame) was
re-decoded three times per tree to prove the A/B is not measuring scheduler noise:
`tip run1/2/3` and `base run1/2/3` all hash `0e712dfd822bb50d`, `cmp -l` differing bytes = 0.
(An earlier single-shot reading of that cell looked like a difference; it was a dump read while the
sweep was still writing it. The repeated runs are the number I stand behind.)

### Why this is not luck, and the one thing to keep in mind

The production-code delta is provably one statement. Filtering the whole `decode.rs` + `stream.rs`
diff to lines that are not comments, not blank and not inside `mod tests` yields exactly the old
refusal and the new `assert!` — nothing else:

```
-    if seq.subsampling_x != seq.subsampling_y {
-        return Err(Error::unsupported(
-            "AV1 decode_stream",
-            "a chroma format of 4:2:2 (subsampling_x != subsampling_y): ...",
-        ));
-    }
+    assert!(
+        !(seq.subsampling_x == 0 && seq.subsampling_y == 1),
+        "AV1 decode_stream: a parsed color_config carried 4:4:0 ..."
+    );
```

The old condition `ss_x != ss_y` is **false** for 4:2:0 (1,1) and 4:4:4 (0,0), so for every stream
outside the lift the two versions execute the same statement list. The 45 cells are the measurement
of that argument, not the argument itself.

*One caveat for whoever reads the dump numbers later*: none for grain (see above), but these are
identity claims between two builds, not correctness claims against ffmpeg. The ffmpeg comparisons
are items 4 and 5's job, and they are per-frame and post-grain there.

---

## 2. The assertion replacing the refusal — CONFIRMED

**Read the parser, not the comment.** `crates/ec-av1-syntax/src/sequence.rs:427-504`,
`read_color_config(r, seq_profile)`. Exhaustive over the three profiles:

| branch | condition | `(subsampling_x, subsampling_y)` produced |
|---|---|---|
| `:454` | `mono_chrome` (any profile except 1, where it is not coded) | `(1, 1)` — assigned, never read |
| `:463` | sRGB identity description (primaries 1, transfer 13, matrix 0) | `(0, 0)` — assigned, never read |
| `:470` | profile 0 | `(1, 1)` |
| `:474` | profile 1 | `(0, 0)` |
| `:479-485` | profile 2, `bit_depth == 12` | `sx = read_bits(1)`; `sy = read_bits(1)` **only if `sx == 1`**, else `0` |
| `:486-490` | profile 2, below 12 bits | `(1, 0)` |

So the produced set is exactly `{(1,1), (0,0), (1,0)}` and `(0,1)` is never produced: the only
`sites that can set `subsampling_y` to `1` are the two *assignments* `(1,1)`, and the only site that
reads a coded `subsampling_y` gates that read on `subsampling_x == 1`. `!(sx == 0 && sy == 1)`
therefore holds over the whole reachable domain, and the existing crate-side test
`the_440_cell_is_not_a_codable_chroma_shape` plus `read_color_config`'s own unit tests
(`profile_shapes_the_subsampling`) pin it from both ends.

**Placement.** `SeqFlags` has exactly one constructor, `SeqFlags::read(parser)`
(`stream.rs:1401-1420`), which reads `parser.sequence_header()` and `map_or(1, …)`s the subsampling
when no header has been seen. So the assert at `stream.rs:1808` is reachable only with a parsed
header, and with no header at all the pair defaults to `(1,1)`, which passes. The only other
`decode::set_subsampling` call site in the tree is `film_grain.rs:2591`, inside a `#[test]`
(`ragged_overlap_tail_matches_spec_reference`); the single production site is `stream.rs:1918`,
inside `decode_frame`, below the assert. Nothing feeds `FrameCtx` a shape the assert did not see.

`assert!` (not `debug_assert!`) is the right call here and is this repository's stated convention for
a proven-unreachable arm; a hostile 4:4:0 stream cannot exist, so this does not convert an `Err` into
a panic on any reachable input.

**4:2:0 / 4:4:4 still parse and decode unchanged** — item 1's 45 cells, all of which take this same
`assert!` on every frame.

---

## 3. Inventory consistency — CONFIRMED

Baseline on the clean tip, all five inventory gates green:

```
cargo test -p ec-av1 --lib -- the_decode_path_refuses_exactly_the_listed_cases \
  every_proven_refusal_names_a_test_that_exists every_named_gate_body_is_bounded_in_these_files \
  capability_claims_are_declared_not_scattered gates_that_swallow_a_decode_error_are_declared
=> 5 passed; 0 failed
```

`the_decode_path_refuses_exactly_the_listed_cases` is a bidirectional set equality between the
strings harvested from `Error::unsupported(...)` in `decode.rs` + `stream.rs` and
`CAPABILITY_CLAIMS ∪ REFUSALS`, so each of the three deletions below is independently load-bearing.
All three mutations were applied in `/home/tahinli/.cache/wt/liftmut` and reverted with
`git checkout --`; the branch tip was never edited.

**M5 — restore the guard in the decode path only** (row still deleted), `sed -i '1808i …' stream.rs`:

```
test the_decode_path_refuses_exactly_the_listed_cases ... FAILED
  new decode-path refusals are not in the inventory: [
```

**M3 — restore the `REFUSALS` row only** (decode path still lifted), `sed -i '137i …' refusal_inventory.rs`:

```
test the_decode_path_refuses_exactly_the_listed_cases ... FAILED
  these refusals are listed but no longer in the decode path: [
```

**M4 — restore the `PROVEN` tuple only**, `sed -i '519i …' refusal_inventory.rs`:

```
test every_proven_refusal_names_a_test_that_exists ... FAILED
  "a chroma format of 4:2:2 (...)" is listed as proven but is no longer a decode-path refusal
test every_named_gate_body_is_bounded_in_these_files ... FAILED
```

3/3. The row and the tuple could not have been deleted independently without a red, and neither
deletion is currently hiding anything.

---

## 4. The two new fixtures — CONFIRMED

**Pins reproduced independently** (`sha256sum` + `stat`, not the report's own table):

| pin | bytes | sha256 measured | matches the pin in the test | fnv1a64 checked in-process by `read_pin` |
|---|---|---|---|---|
| `422_key_64x64.obu` | 755 | `3c9396c9e42701d720419ec2cb5577387e305482df0cb150f7f0b965edeaaffc` | yes | `0x87fc569cf53bcbc6` |
| `422_inter_160x128_3f.obu` | 6522 | `7b54834c18f5fdc0e8606a6dc5bf4ff368402417d87fcfc2ab9758e4e9164902` | yes | `0xf87eba3bf4fc2bb8` |

(also reproduced: `s422_12bit_160x128.obu` 9214 / `eacc70f7…`, `s422_superres_160x128.obu` 5304 /
`5852ff2c…`, `s422_grain_160x128.obu` 5433 / `a4a6957b…`, and the six corpus cells' sha256 columns
as printed by the gate at run time.)

**Genuinely 4:2:2.** `dump_yuv` prints each stream's own parsed header: both new pins are
`seq_profile 2, subsampling 1/0, mono_chrome false`, `yuv422p`. And they round-trip:

```
$ ffmpeg -v error -f obu -i 422_key_64x64.obu        -f rawvideo -pix_fmt yuv422p ff.yuv
$ cat p.f*.yuv > ours.all ; cmp ours.all ff.yuv
  422_key_64x64        ours=8192B    ffmpeg=8192B   -> BYTE-EXACT over all frames
  422_inter_160x128_3f ours=122880B  ffmpeg=122880B -> BYTE-EXACT over all frames
```

**Not the old 4:2:0-tile-behind-a-4:2:2-header construction.** The construction the lifted gate used
to be built in memory (`encode_key_frame_with_ctx(test_card(64,64))` + a hand-set profile-2 header);
the one committed pin of that shape is `440_request_is_422.obu`. I decoded it through the same
comparator to make the distinction measured rather than asserted:

```
440_request_is_422     ours=8192B    ffmpeg=0B   -> ffmpeg produced NO output
```

Same header shape, and ffmpeg cannot walk the tile at all — there is no oracle for those bytes. So
the branch's decision to keep that pin as a *shape* witness (assert `32x64` chroma geometry, assert
the chroma walk ran, claim no pixels) and to move the pixel claim onto real 4:2:2 bytes is the only
honest option available, and the report says so in the same words. The new pins are not that
construction: they are the first 4:2:2 bytes in the tree that an oracle can read.

**Each gate asserts byte-exactness, not a header.** See item 5 — all six renamed gates plus the four
new ones red on a single-LSB decode error, so none of them can be a header-only gate that passes on a
broken decoder. The census arm is real too, measured on the tip: `422_key_64x64` walks
`units=[54, 22, 22] coded=[21, 21, 20]`, `422_inter_160x128_3f` `[497, 277, 277] / [123, 256, 202]`,
`s422_12bit_160x128` `[1037, 529, 529] / [281, 371, 384]`, `s422_superres_160x128`
`[797, 325, 325] / [168, 202, 216]`, and the six corpus rows `[2118,1218,1218]` … `[2927,1426,1426]`
with `coded` non-zero on all three planes of every row. Nothing narrows at HBD: the 12-bit row reds
with `3542 vs 3541`, i.e. 16-bit containers on both sides.

*Disclosed, not a defect*: the six corpus rows print their sha256 with `eprintln!` instead of
asserting it (the in-process pin is size + fnv1a64), and the test says so in a comment.

---

## 5. The six renamed gates — CONFIRMED, 6/6 bite

Mutation **M1b**, applied in the scratch worktree at `stream.rs:2236` (right where the picture is
handed to the reference store and to the caller) — a one-LSB chroma decode error with the plane
**shape untouched**, so the shape guard cannot be what reds:

```rust
// MUTATION M1b: a single-LSB chroma decode error; plane SHAPE untouched.
let mut picture = picture;
if let Some(s) = picture.u.first_mut() { *s = s.saturating_add(1); }
```

```
test the_pinned_422_bigblock_witnesses_decode_pixel_exact ... FAILED
  U differs from ffmpeg at frame 0 sample 0 (129 vs 128; 1 of 8192 samples differ)
test the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_exact ... FAILED
  U differs from ffmpeg at frame 0 sample 0 (130 vs 129; 1 of 61440 samples differ)
test the_pinned_422_lossless_inter_witnesses_decode_pixel_exact ... FAILED
  U differs from ffmpeg at frame 0 sample 0 (222 vs 221; 1 of 38400 samples differ)
test the_pinned_422_residual_compound_warp_witness_decodes_pixel_exact ... FAILED
  U differs from ffmpeg at frame 0 sample 0 (141 vs 140; 1 of 36864 samples differ)
test the_pinned_422_lr_off_witness_decodes_pixel_exact ... FAILED
  U differs from ffmpeg at frame 0 sample 0 (141 vs 140; 1 of 36864 samples differ)
test the_pinned_422_corpus_cells_decode_pixel_exact ... FAILED
  U differs from ffmpeg at frame 0 sample 0 (223 vs 222; 1 of 39360 samples differ)
=> test result: FAILED. 1 passed; 9 failed
```

The seventh 4:2:2 gate that stays green, `the_440_cell_is_not_a_codable_chroma_shape`, is supposed
to: it makes a geometry claim and says in its own comment that its tile has no oracle. A green there
under M1b is the correct outcome, not a hole.

A second mutation **M1** — `set_subsampling` hardcoding the 4:2:0 chroma *height* on a `(1,0)` stream
— reds all 10 of the 4:2:2 gates on the shape assertion, which is the complementary proof that the
geometry half of the claim is load-bearing too.

---

## 6. Prose — REFUTED at six sites

Instrument: a throwaway `examples/liftprobe.rs` in the scratch worktree (removed afterwards) that
decodes one fixture and prints `sb128rect_chroma_replay_counters()`,
`rect_tiled_chroma_grid_hits()` and the census, plus `grep` for the readers of each named counter.

### 6.1 `decode.rs:23415-23425` — the one that measurement contradicts

The rewritten doc for `SB128RECT_REPLAY_SPAN_MISMATCH_HITS` says the counter is "a REAL detector, not
a defensive arm … the 4:2:2 row of `the_pinned_422_corpus_cells_decode_pixel_exact` asserts it stays
zero there too, which is the claim that replaces *it cannot fire*". Both halves are false:

* the corpus gate never reads it. Its only counter reads are `census_nonsub_units` /
  `census_nonsub_coded` inside the shared `assert_422_stream_pixel_exact` body; the only two gates that
  do read `sb128rect_chroma_replay_hits()` are `a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact`
  (`stream.rs:9188/9211`, 4:4:4) and `a_lossless_sb128_rect_intra_block_decodes_sample_exact`
  (`10753/10772`, 4:2:0 lossless) — neither is a 4:2:2 stream.
* it does not stay zero. On the branch tip, over all 21 committed 4:2:2 cells:

```
422_intrabc_sb128_strip.obu          sb128rect_replay=72  sb128rect_mismatch=9   frames=5
422_intrabc_sb128_strip_notxsearch.obu sb128rect_replay=72 sb128rect_mismatch=9   frames=5
(all 19 other 4:2:2 cells)                                                 mismatch=0
```

`422_intrabc_sb128_strip.obu` is one of the pins
`the_pinned_422_intrabc_sb128_strip_witnesses_decode_pixel_exact` asserts **byte-exact against
ffmpeg**, and it is green. So the counter documented as "must be zero, and a gate asserts it" reads
9 on a byte-exact cell, and the gate named as the asserting one never looks. Adding the read the
comment promises would turn a green gate red on a cell that is provably correct.

This is also the one place where the lift destroyed a *true* statement: pre-lift the doc said "Zero on
every stream this decoder admits … 4:2:2 … is refused by name at the sequence header", and that was
correct. Admission of 4:2:2 genuinely made the counter able to fire — the lift just did not notice,
and wrote down the opposite.

### 6.2 `decode.rs:2859-2864` — `RECT_TILED_CHROMA_NXN_HITS`

"The corpus gate … reads this counter as a non-vacuity arm", and "a 4:2:2 block whose chroma plane
is wider than tall relative to its luma span lands here". The corpus gate does not read it. The only
two readers of `rect_tiled_chroma_grid_hits()` are `a_444_rect_strip_chroma_tiled_*` (4:4:4,
`r512_rect1x2_444_palette.obu`), and there `h_nxn` is bound and interpolated into a failure message,
never asserted. Measured `nxn` on all 21 committed 4:2:2 cells and on the 4:2:0 / 4:4:4 cells
probed: **0 everywhere**. The reachability claim is aspirational — no committed 4:2:2 stream lands
in that arm.

### 6.3 `decode.rs:6139-6145` — `INTRABC_RECT4_OWN_CHROMA422_HITS`

"committed tests CAN read this counter now; `the_pinned_422_corpus_cells_decode_pixel_exact` reads
its delta on every corpus row". Its accessor `intrabc_rect4_own_chroma422_hits()`
(`decode.rs:6152`) has **zero callers** in the crate — the 4:4:4 twin is read at `stream.rs:10260`
and `10279`, this one nowhere. The corpus gate reads no delta of it. Pre-lift wording ("no committed
test can read it") was true; the replacement is false.

### 6.4 `stream.rs:319-327` — public API doc, same false claim

`sb128rect_chroma_replay_counters` is `pub`, and its doc ends "the 4:2:2 rows of
`the_pinned_422_corpus_cells_decode_pixel_exact` assert that [the mismatch stays zero]". Same
misattribution, same 9. Repeated inside the two tripwire gates' own comments at
`stream.rs:9201-9207` and `10764-10771` ("read the mismatch half on streams where the two spans
actually differ", "the 4:2:2 rows … assert the same zero there") — which is worse there, because
those are the gates that *do* read the counter, and a reader would reasonably add the missing 4:2:2
assert to one of them and get a red on a correct cell.

### 6.5 `refusal_inventory.rs:515` — names a gate that does not exist

The replacement note says the old refusal's replacement is `a_real_422_stream_decodes_pixel_exact`.
There is no such test. The gate is `a_real_422_key_frame_and_inter_sequence_decode_pixel_exact`
(`stream.rs:2354`). In the file whose entire job is naming gates, and in the paragraph a future
reader follows to find the evidence for the lift.

### 6.6 `refusal_inventory.rs:88` — dangling reference to the deleted gate

`a_non_420_subsampled_sequence_header_is_refused_by_name`'s sibling witnesses — the patch deleted
that test, so the sibling it names no longer exists. Not in the diff's hunks, but the staleness is
introduced by this patch, which is what removed the symbol.

### 6.7 Checked and sound

| site | claim | verdict |
|---|---|---|
| `stream.rs:1388-1396` (`SeqFlags` doc) | subsampling is "load-bearing on the DECODE path", published through `decode::set_subsampling` and read per axis | **CONFIRMED** — `set_subsampling` has exactly one production call site (`stream.rs:1918`) |
| `stream.rs:17009-17018` | "the header admits ss (1,1), (1,0) and (0,0), and the (1,0) 4:2:2 arm is dispatched a few lines below (`chroma422_rect32`)" | **CONFIRMED** (dispatch is real, at `decode.rs:17060`; "a few lines" is 42, a nit) |
| `stream.rs:52058-52065` (`cfl_ac_ss`) | "`cfl_ac_ss` DISPATCHES ss (1,0) before the 4:2:0 fallthrough" | **CONFIRMED** — `decode.rs:21007` `else if ss_x == 1 && ss_y == 0`, fallthrough at `21031`/`21034` |
| `lanes/av1422lift.report.md:89-93` | the non-vacuity arm is the census, "not a 4:2:2 arm counter" | **CONFIRMED** — and notably the report does *not* repeat the false claim its own in-code comments make |
| the all-skip row's negative measurement (`coded_delta == (0,0)`) | the cell's identity is that it codes nothing | **CONFIRMED** — measured `units=[4,8,8] coded=[1,0,0]` |

---

## Findings, ranked

| # | site | what is wrong | fix |
|---|---|---|---|
| 1 | `decode.rs:23415-23425` | claims a 4:2:2 gate asserts `SB128RECT_REPLAY_SPAN_MISMATCH_HITS` stays zero; no gate reads it and it reads **9** on a byte-exact committed 4:2:2 pin | either measure and document the 9, or state the predicate is not a 4:2:2 defect signature; drop the gate attribution |
| 2 | `decode.rs:2859-2864` | claims the corpus gate reads `RECT_TILED_CHROMA_NXN_HITS` as a non-vacuity arm; nothing does, and the arm reads 0 on all 21 committed 4:2:2 cells | say the arm is unwitnessed, not that a gate reads it |
| 3 | `decode.rs:6139-6145` | claims committed tests now read `INTRABC_RECT4_OWN_CHROMA422_HITS` and that the corpus gate reads its delta per row; its accessor has zero callers | restore "no committed test reads it", or add the read |
| 4 | `stream.rs:319-327` (+ `9201-9207`, `10764-10771`) | public API doc and two gate comments repeat finding 1's false attribution, and the two gate comments invite adding a read that would red on a correct cell | same as finding 1 |
| 5 | `refusal_inventory.rs:515` | names `a_real_422_stream_decodes_pixel_exact`; the gate is `a_real_422_key_frame_and_inter_sequence_decode_pixel_exact` | fix the name |
| 6 | `refusal_inventory.rs:88` | still names the deleted `a_non_420_subsampled_sequence_header_is_refused_by_name` as a live sibling gate | drop or re-point the reference |

Findings 1-4 are one class: the lift changed a true statement about a counter ("no committed test can
read it", "zero on every admitted stream") into a false one about a gate ("the corpus gate reads
it"). Nothing in items 1-5 of this review is wrong because of them — no decode result, no gate
outcome and no pin depends on any of these six sentences — but the sentences are the load-bearing
part of how this tree documents reachability, and a reader who acts on finding 1 or 4 lands a red on
a cell that is byte-exact against ffmpeg.

## What was not run

* The three VPS suites and the branch's own full lib suite were already in flight elsewhere; I ran
  scoped named tests only, as assigned.
* The corpus (`63`-cell) re-measurement, `s422_384x240`, and the 51/51 landing condition the corpus
  gate's doc states are the owning lane's/main's business; I did not re-measure the corpus. The
  branch declares that hold itself, in the gate's doc and in `av1422lift.report.md:226`, and that
  declaration is honest.
* I did not re-encode any fixture; "genuine aomenc bytes" is established indirectly but decisively,
  by ffmpeg decoding all three frames of each new pin and our output matching byte for byte.
* `a real aomenc 12-bit 4:2:2 stream is unreachable through aomenc on this libaom` (the provenance
  note for `s422_12bit_160x128.obu`) is the report's claim; I verified the pin's identity and its
  byte-exactness but did not re-run aomenc's `AV1E_SET_CHROMA_SUBSAMPLING_X` experiment.

## Cleanliness

`/home/tahinli/.cache/wt/liftmut` restored (`git status --porcelain` empty, `liftprobe.rs` deleted);
`/home/tahinli/.cache/wt/av1422liftverify` carries no decoder change — this report is the only file
added. Nothing pushed, `main` untouched, no `git add`/`git commit` from the primary checkout.
