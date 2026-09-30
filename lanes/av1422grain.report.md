# lane-av1422grain — `film_grain.rs` is per-axis; 4:2:2 and 4:4:4 grain are byte-exact vs ffmpeg

**Target:** `crates/ec-av1/src/film_grain.rs`, the one module the lift study
(`lanes/av1422liftrisk.report.md` §1b/§2 item 1) found still 4:2:0-shaped.
**Branch:** `lane/av1422grain`, commit `64549b09`. Base `228a55f5`.
**Changed file:** `crates/ec-av1/src/film_grain.rs` only. Nothing else touched.

---

## 1. THE MEASUREMENT — aomenc DOES emit 4:2:2 film grain

The assignment's branch 3 (assertion + "cannot be produced") does **not** apply.
The cell is codable, so this is branch 2: port and prove.

**Encoder:** `/home/tahinli/.cache/aom-oracle/build/aomenc` (libaom, oracle
snapshot `9bb526a`).

```sh
# 4:2:2 source
ffmpeg -f lavfi -i testsrc2=size=320x240:rate=1:duration=1 \
       -pix_fmt yuv422p -f yuv4mpegpipe src422.y4m

# the encode under test
aomenc src422.y4m --profile=2 --film-grain-test=5 --obu -o g422_t5.obu
```

**Result: exit 0, 10124 bytes, no diagnostic of any kind.** The encoder said
nothing because nothing was wrong — it simply emitted the cell.

The proof that the frame header really carries 4:2:2 grain:

```sh
ffprobe -v error -export_side_data +film_grain -show_frames -of json g422_t5.obu
```
```json
"side_data_type": "Film grain parameters",
"seed": "7825", "width": 320, "height": 240,
"subsampling_x": 1, "subsampling_y": 0,
"chroma_scaling_from_luma": 0, "scaling_shift": 11,
"ar_coeff_lag": 3, "ar_coeff_shift": 7
```

`subsampling_x: 1, subsampling_y: 0` is 4:2:2, with a **full** point set
(`ar_coeff_lag 3` is the maximum, so every AR tap and both `avgLuma` paths are
exercised — the params are not a degenerate no-op). The identical encode
without `--film-grain-test` produces 9998 bytes and ffprobe reports **zero**
`Film grain parameters` side-data blocks, so the flag is what puts grain in.

**4:4:4 is also live**, which the assignment did not anticipate and which
changes the shape of the fix (see §3):

```sh
aomenc src444.y4m --profile=1 --film-grain-test=5 --obu -o g444_t5.obu
# 15541 bytes; ffprobe: profile "High", pix_fmt yuv444p, 1x Film grain parameters
```

So this module had **two** silently-wrong arms, not one, and a port that
special-cased only `ss_y` would have fixed 4:2:2 and left 4:4:4 broken. The
port is per-axis on both.

**Cells built** (all in `/home/tahinli/.cache/av1422grain/`):

| cell | encode | size |
|---|---|---|
| `g422_t5.obu` | `--profile=2 --film-grain-test=5` (8-bit 4:2:2) | 10124 |
| `g422_t11.obu` | `--profile=2 --film-grain-test=11` (8-bit 4:2:2) | 10124 |
| `g422_10_t5.obu` | `--profile=2 --input-bit-depth=10 --bit-depth=10 --film-grain-test=5` | 13792 |
| `g444_t5.obu` | `--profile=1 --film-grain-test=5` (8-bit 4:4:4) | 15541 |

Two grain vectors at 8-bit so the result is not one lucky seed; one at 10-bit so
the bit-depth-generic path (`grain_range`, `scale_lut`, the `<< (bit_depth - 8)`
offsets) is covered at a second depth.

---

## 2. WHAT WAS ACTUALLY WRONG (the study's list, confirmed and corrected)

The study listed 9 sites. All 9 were real. Two of its characterisations needed
correcting, and both corrections are load-bearing:

* **`:577 n = min(..., luma.len()/2)`** — the study calls this "halves the luma
  operand". True, and it is right at 4:2:2 (where the operand IS a horizontal
  pair) but wrong at **4:4:4**, where the operand is the bare luma sample. So
  the fix is not "divide by 2", it is a `pair` flag.
* **`:351 c_stride = width/2`** — the study says "right for WIDTH at 4:2:2". It
  is right at 4:2:2 and wrong at 4:4:4, where it must be `width`.

The study also says the 4:2:2 failure is "the top half of the chroma plane
grained with a 4:2:0 row map and a 2x2 `avgLuma` quad". Confirmed, and §5
quantifies it.

## 3. THE PORT — libaom source lines

This libaom snapshot has **no** `generate_grain_uv_422` and **no**
`av1_apply_grain_422`; the assignment's premise about those function names is
from an older libaom. What this version does is thread `chroma_subsamp_y` /
`chroma_subsamp_x` through one unified `add_film_grain_run`
(`av1/decoder/grain_synthesis.c:987`), set per pixel format in
`av1_add_film_grain` (`:1386-1420`, which lists `AOM_IMG_FMT_I422` and
`I42216` explicitly). **Read the C, not the function names** — every site below
is transcribed from this snapshot.

| ours | C | what |
|---|---|---|
| `Ss::csub_x/y` | `:1018-1019` | `chroma_subblock_size_x/y = 32 >> chroma_subsamp` |
| `Ss::cblock_w/h` | `:1031-1035` | template size, `(2 >> ss) * ar_padding` terms |
| `c_stride` | `:1461` | `width >> chroma_subsamp_x` (right for 4:4:4 only here) |
| `chroma_row(y)` | `:1129` | chroma row of block row `y` is `y*2 >> ss_y` |
| `chroma_off_y/x` | `:1091-1094` | `top_pad + (2>>ss_y)*ar_pad + offset_y*(2>>ss_y)` |
| `cy0/cx0` | `:1129-1130` | `py0 >> ss_y`, `px0 >> ss_x` (not `/2`) |
| `ch/chw` block | `:680-681` | `half_h << (1-ss_y)` by `half_w << (1-ss_x)` |
| `avgLuma` in template | `:554-565` | `(ss_y+1) x (ss_x+1)` samples, `((1<<(ss_y+ss_x))>>1)` rounding |
| `average_luma` in apply | `:683-691` | horizontal pair at `ss_x==1`, bare sample at `ss_x==0` |
| line/col buffers | `:392-414` | `chroma_stride*(2>>ss_y)`, `(csub_y+(2>>ss_y))*(2>>ss_x)` |
| `ver_boundary_overlap` chroma | `:1102-1120` | width `2>>ss_x`, height `AOMMIN(csub_y+(2>>ss_y), (height-2y)>>ss_y)` |
| `hor_boundary_overlap` chroma | `:1157-1200` | height `2>>ss_y` |
| four chroma `copy_area`s | `:1289-1296`, `:1309-1320`, `:1345-1352` | per-axis extents |

### Three traps the port walks into

Each of these cost a real debugging cycle and is recorded at the site.

**(a) The chroma BLOCK size is `16 << (1 - ss)`, not `chroma_subblock_size << (1 - ss)`.**
`add_noise_to_block` is handed `half_luma_height = AOMMIN(32 >> 1, ...)` and
`half_luma_width = AOMMIN(32 >> 1, ...)` (`:1254-1255`) — **luma** half-extents
— and scales them per axis inside (`:680-681`). So the block is
`16 << (1-ss)`: 16x16 at 4:2:0, **32x16 at 4:2:2**, **32x32 at 4:4:4**. The
tempting `chroma_subblock_size << (1 - ss)` agrees at 4:2:0 and diverges
wherever `chroma_subsamp == 0` (4:2:2 rows, 4:4:4 both axes) — the two differ
by a factor of 2. Getting this wrong is an out-of-bounds read, not a wrong
pixel.

**(b) The block-column walk is in LUMA units at every subsampling.** The C is
`for (x = 0; x < width / 2; x += luma_subblock_size_x >> 1)` (`:1075`) and
`for (y = 0; y < height / 2; y += luma_subblock_size_y >> 1)` (`:1069`).
`width/2` and `height/2` are correct as written and must NOT become
`c_stride`; only the chroma ORIGIN and EXTENT derived from them are per-axis.
Making the walk itself per-axis is the single easiest way to break 4:2:0.

**(c) `copy_area(cb_col_buf + (chroma_subblock_size_y << (1 - ss_x)), ...)`
(`:1289`) is a FLAT element offset, not a row index.** `copy_area` here
addresses `src_row0 * src_stride + src_col0`. Passing the C's flat value as the
row index multiplies it by the stride a second time and reads past the buffer
(observed: `len is 68 but the index is 128` at 4:4:4). The correct row index is
`csub_y`, because `src_stride == 2 >> ss_x` and the flat offset is
`csub_y << (1 - ss_x)`, which are the same number for every `ss_x`. Recorded
in a comment at the call site.

### Two blend formulas, not one

`ver_boundary_overlap` (`:912-939`) and `hor_boundary_overlap` (`:941-970`)
each have a `width/height == 1` arm and a `== 2` arm with **different
coefficients**: 23/22 for one, 27/17 and 17/27 for two. At 4:2:0 the `2 >> ss`
terms are both 1, so only the first arm ever ran and the second was dead code
that had never been exercised. It is live at 4:2:2 (rows) and 4:4:4 (both
axes), and the 17/27 second row is a genuinely different value. `film_grain.rs`
already had both arms (`ver_overlap_inplace`, `hor_overlap_inplace`); the port
is what makes them reachable.

---

## 4. NON-VACUITY — the comparator was proven to bite

`stream.rs:16626-16629` warns that a params-without-apply stream is the known
false green. Three independent guards, all live:

1. **`grain_hits() > 0`** — asserted per cell, delta printed:
   `0->1`, `1->2`, `2->3`, `3->4`. A stream carrying `apply_grain` that never
   ran synthesis fails here.
2. **The header really carries grain** — the test re-parses the OBUs and
   asserts some frame header has `film_grain.apply_grain`.
3. **The comparator bites** — one flipped oracle sample per plane, then the
   expected plane-difference tuple is asserted to be exactly `(1,1,1)`:

```
TAMPER g422_t5.obu:     plane diffs must be exactly (1,1,1), got (1,1,1)
TAMPER g422_t11.obu:    plane diffs must be exactly (1,1,1), got (1,1,1)
TAMPER g422_10_t5.obu:  plane diffs must be exactly (1,1,1), got (1,1,1)
```

A zero here would have meant the plane comparison was not reading what it
claims to.

## 5. NEGATIVE CONTROL — the verdict is produced by this port

Forcing `Ss::read` back to the old hardcoded `Ss { x: 1, y: 1 }` (the pre-lane
geometry, everything else identical) makes the cell **fail** against ffmpeg:

```
g422_t5.obu frame 0: luma/u/v samples differing from ffmpeg
  left: (0, 23370, 22631)     right: (0, 0, 0)
```

Read that shape carefully, it is the whole defect in one line: **luma exact,
~30% of each chroma plane wrong**. No panic, no counter, no refusal — exactly
the silent-wrong-picture failure the study predicted. (30% rather than ~50%
because a wrong grain delta is frequently zero, so many mis-grained samples
coincide with the oracle.)

## 6. THE RESULT

Every plane of every frame, `decode_stream` vs ffmpeg's own decode of the same
bytes, 4:2:2 chroma planes split by sequence-header geometry
(`yuv422p` / `yuv422p10le`; 10-bit read as u16 little-endian, never
u8-narrowed):

```
g422_t5.obu    byte-exact, 1 frames, grain_hits 0->1
g422_t11.obu   byte-exact, 1 frames, grain_hits 1->2
g422_10_t5.obu byte-exact, 1 frames, grain_hits 2->3
g444_t5.obu    byte-exact, 1 frames, grain_hits 3->4
```

## 7. CONTROLS UNCHANGED

The existing 4:2:0 coverage was run and is untouched and green — 8-bit, 10-bit
and 12-bit, all byte-exact against ffmpeg:

| test | result |
|---|---|
| `a_real_aomenc_stream_with_film_grain_decodes_pixel_exact` (8-bit) | ok |
| `a_real_aomenc_10bit_film_grain_stream_decodes_pixel_exact` | ok |
| `a_real_aomenc_12bit_film_grain_stream_decodes_pixel_exact` | ok |
| `real_aomenc_film_grain_streams_decode_pixel_exact` (3 live aomenc draws) | ok |
| `untouched_planes_and_ragged_tail_come_back_clean` | ok |
| `simd_matches_scalar_luma_noise_row` | ok |
| `simd_matches_scalar_chroma_noise_row` | ok |
| `ragged_overlap_tail_matches_spec_reference` | ok (now 3 formats) |

`cargo test -p ec-av1 --lib film_grain` → **8 passed, 0 failed**.

`cargo check -p ec-av1 --lib --tests` → clean, no warnings.

**Coverage that did not exist before this lane:** 4:2:2 and 4:4:4 film grain
had **no** test of any kind — not a byte-exactness gate, not a reference
comparison. `ragged_overlap_tail_matches_spec_reference` was a 4:2:0-only
transcription; it is now driven over all three formats, because a second
4:2:0-shaped copy of the reference would have been a new way to be wrong. It
also gained the assertion the old geometry **could not make**:

```rust
assert_ne!(&got.u[last_chroma_row], &src.u[last_chroma_row],
    "the LAST chroma row came back clean -- the walk did not reach it");
```

At 4:2:2 the chroma plane is `height` rows tall and the old `nrows = height/2`
walk left the bottom half raw, so this is the assertion that would have caught
the original bug directly.

## 8. PROCEDURE — probe bypass, patch-run-restore

`EC_AV1_ALLOW_422_PROBE` does nothing; the bypass is patch-run-restore.

1. `git worktree add --detach ~/.cache/wt/av1422grain-probe HEAD`
2. copy this lane's `film_grain.rs` into it
3. neutralise the guard at `stream.rs:1803` with
   `if false && seq.subsampling_x != seq.subsampling_y`, with a
   `// TEMP-PROBE-BYPASS lane-av1422grain: patch-run-restore, never committed.`
   marker on the line above
4. run, then `git checkout -- <both files>`, verify `git status --porcelain`
   is **empty**, verify `grep -c 'TEMP-PROBE-BYPASS\|if false &&'` is **0**
5. `git worktree remove --force`

Verified: the probe worktree is removed, and the guard is intact and
un-bypassed. **The primary checkout is clean** (`git status --porcelain`
empty) with no bypass markers.

**Incident, reported rather than buried:** my *first* edit of this lane used a
RELATIVE path (`crates/ec-av1/src/film_grain.rs`) while the shell CWD was the
primary checkout, so it landed in `/home/tahinli/Documents/Code/Rust/edith_codecs`
instead of the lane worktree. The tool reported success and I read the
worktree, saw no change, and wrongly concluded the edit had been rolled back —
so I reissued everything against absolute paths and the worktree ended up
complete. The leak sat unnoticed in the primary until the post-probe
`git status` check caught it. I verified all 61 added lines were mine (the `Ss`
fields and brace lines; every other line carries a `lane-av1422grain` or
`chroma`/`luma`/`subsamp` marker) and reverted with
`git checkout -- crates/ec-av1/src/film_grain.rs`. No peer work was lost. The
lesson is the one already in the skill library: **absolute paths only, and
check `git status` in the primary before yielding** — the check is what found
it, and it should have been run after the first edit, not at the end.

## 9. STATUS AND WHAT IS NOT DONE

Delivered: branch 2 in full — the cell exists (measured), the geometry is
per-axis, `grain_hits` fires, all four cells are byte-exact per plane per
frame, the comparator is proven to bite, the negative control fails without the
port, and the 4:2:0 controls are unchanged.

**The end-to-end gate is NOT committed, and cannot be until the guard is
lifted.** `decode_stream` still returns
`"a chroma format of 4:2:2 (subsampling_x != subsampling_y)"` on `main`
(`697b1308`) — the guard at `stream.rs:1803` is untouched by this lane, by
design; lifting it is not this arm. A committed 4:2:2 decode gate would be red
today for a reason that has nothing to do with film grain.

**Deferred, with its unblock:** commit the four-cell byte-exactness gate
immediately after the lift lands. It needs no further film-grain work — the
bytes and the assertions are settled. Handing it over as a follow-up is the one
piece of scope I am explicitly not closing, and it is blocked on a decision
that is not mine.

## 10. A NOTE FOR THE LIFT

Two things this lane learned that the lift should not have to rediscover:

* **The 4:2:2 film-grain cell is real and now byte-exact.** Whoever lifts the
  guard should expect it to pass, and should not read a film-grain failure at
  4:2:2 as a lift regression — it would be a film-grain regression, and this
  lane's commit is the fix.
* **`grain_hits()` is the counter to gate on** for the film-grain arm of a
  lifted 4:2:2 stream, and the four cells in §1 are ready-made fixtures for
  it. The study's §2 item 3 ("no 4:2:2 unit census exists") is a separate
  matter and does not affect film grain.
