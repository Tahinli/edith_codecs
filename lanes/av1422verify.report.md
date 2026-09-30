# lane-av1422verify — refutation pass over `19b16ffd` (grain) and `2677c97a` (census)

**Role:** refute-first. Every load-bearing figure below was re-derived with my own
instruments, not read off the lane report.
**Trees:** `main` = `2677c97a`. My worktree `lane/av1422verify` (clean, no decoder edits).
Scratch trees `vfy-after` (main) and `vfy-prefix` (`697b1308` = `19b16ffd~1`) were
patched, measured, restored, and **removed**.
**Instruments:** ffmpeg 8.1.3 (pixel oracle), the oracle `aomenc`
(`~/.cache/aom-oracle/build/aomenc`), the C at `~/.cache/aom-oracle/src/av1/decoder/grain_synthesis.c`.

## 0. Verdict summary

| # | Claim | Verdict |
|---|---|---|
| G-a | a real 4:2:2 grain stream decodes byte-exact | **CONFIRMED** |
| G-b | negative control: forcing ss back to (1,1) fails `0/23370/22631`, no panic, no counter | **CONFIRMED** (and strengthened — measured on the real pre-fix tree, not a mutation) |
| G-c | the pre-fix geometry was also wrong for 4:4:4 | **CONFIRMED** |
| G-d | the three libaom traps (block size, luma-unit walk, flat element offset) | **CONFIRMED**, lines quoted in §3 |
| G-e | `grain_hits()` rises per cell **and the gate re-parses OBUs to assert `apply_grain`** | **PARTIAL** — the counter claim is unmeasurable from outside the crate; **the committed gate does not exist** |
| G-f | controls unchanged: `--lib film_grain` 8 passed, blast sweep 95 passed | **CONFIRMED** |
| C-1 | arming at ss (0,0) alone made every unit counter read 0 on 4:2:2; widening makes them move | **CONFIRMED**, all four cells digit-for-digit |
| C-2 | 4:2:0 (405) and 4:4:4 (11292, 1373) control counts unchanged | **CONFIRMED** with my own before/after binaries |
| C-3 | strip enumeration walks `[(0,0),(1,0),(1,1)]`, `checked == 12`, no unit missing | **CONFIRMED**, but **one cell of the report's own derivation table is wrong** (see §7 F1) |
| C-4 | `chroma422_chunk` / `chroma422_pair_wide` read 0 | **CONFIRMED** and strengthened: 0 on **all 18** 4:2:2 cells, not just four |
| C-5 | `g422_t5` is walk-shape only, not comparator-clean through the dump path | **CONFIRMED**, and the dump is provably pre-grain |

**No code defect found in either patch.** The port is faithful to the C at every site I
checked, the census widening is a one-expression change with disjoint conditions, and
every counter's "every exit path" claim holds in the source. The two findings below are
**figure/claim defects in the committed documentation**, not decode defects.

---

## 1. Grain: the 4:2:2 / 4:4:4 exactness claim, re-derived

### 1.1 Method

`EC_AV1_FINAL_DUMP` is pre-grain (§5), so the grain comparator must go through the
in-memory sink. `decode_probe`'s `EC_PROBE_OUT` / `EC_PROBE_OUT16` are written from the
`decode_stream_with` sink closure, i.e. from the **post-grain** `output` picture
(`stream.rs:1326-1335` builds `output` by calling `apply_grain` and `reorder.push`es it;
the sink drains `reorder`). Oracle: `ffmpeg -i cell.obu -pix_fmt yuv422p -f rawvideo`.
8-bit cells use `EC_PROBE_OUT` (u16 planes narrowed to u8 — lossless at 8-bit); the
10/12-bit cells use `EC_PROBE_OUT16` (u16 LE), never a u8 narrowing.

### 1.2 AFTER (`main` = `2677c97a`), mine beside the lane's

| cell | format | mine (Y / U / V differing) | lane §6 |
|---|---|---|---|
| `g422_t5.obu` | 4:2:2 8b | **0/76800, 0/38400, 0/38400** | byte-exact |
| `g422_t11.obu` | 4:2:2 8b | **0/76800, 0/38400, 0/38400** | byte-exact |
| `g444_t5.obu` | 4:4:4 8b | **0/76800, 0/76800, 0/76800** | byte-exact |
| `g422_10_t5.obu` | 4:2:2 10b | **0/153600B, 0/76800B, 0/76800B** | byte-exact |
| `g422_12b.obu` (**mine, not the lane's**) | 4:2:2 **12-bit** | **0/153600B, 0/76800B, 0/76800B** | — |

The lane tested 8-bit and 10-bit at 4:2:2 and 8-bit at 4:4:4. I added a **12-bit 4:2:2**
cell (`aomenc --profile=2 --input-bit-depth=12 --bit-depth=12 --film-grain-test=5`;
`ffprobe`: `subsampling_x: 1, subsampling_y: 0`, `seed 17968`) to hit the
`grain_range`/`scale_lut` bit-depth-generic path at a third depth. It is byte-exact too.

### 1.3 PRE-FIX (`697b1308` = `19b16ffd~1`) — the negative control, and claim G-c

The lane produced its negative control by mutating `Ss::read` in a scratch tree. I did
not need to: I built the **actual pre-fix tree** with only the probe bypass, so the
negative control measures the defect rather than a reconstruction of it.

| cell | `697b1308` (mine) | lane §5 (their mutation) |
|---|---|---|
| `g422_t5.obu` | Y=**0**/76800, U=**23370**/38400 (max 8), V=**22631**/38400 (max 16) | `(0, 23370, 22631)` |
| `g422_t11.obu` | Y=0/76800, U=23788/38400 (max 7), V=22884/38400 (max 16) | — |
| `g444_t5.obu` | Y=0/76800, U=**45612**/76800 (max 16), V=**43755**/76800 (max 16) | — |

**G-b CONFIRMED, digit-for-digit**, and on a stronger instrument than the lane used. The
shape is exactly as described — luma exact, chroma wrong, no panic (exit 0), no refusal.

**G-c CONFIRMED**: `g444_t5.obu` is RED at `19b16ffd~1` (59% / 57% of each chroma plane
wrong) and GREEN at `19b16ffd`. The pre-fix geometry really was silently wrong for 4:4:4.

### 1.4 Grain synthesis provably ran (the params-without-apply false green)

`grain_hits()` is `pub(crate)` and **not reachable from `decode_probe`** — the only
instrument that can read a 4:2:2 stream while the header refusal stands. I substituted
a stronger differential: decode the same bytes with `EC_AV1_NO_GRAIN=1` and compare to
the normal sink output.

```
g422_t5:  sink differs from EC_AV1_NO_GRAIN=1 in 100396 samples
g444_t5:  sink differs from EC_AV1_NO_GRAIN=1 in 144020 samples
```

Grain synthesis ran and changed the picture. (Incidentally 100396 is exactly the figure
the census lane independently measured for this cell — see §5.)

### 1.5 Comparator liveness — my own, on all four cells

One oracle sample flipped per plane, then the plane-diff tuple must be exactly `(1,1,1)`:

```
TAMPER g422_t5:     (1, 1, 1)
TAMPER g422_t11:    (1, 1, 1)
TAMPER g444_t5:     (1, 1, 1)
TAMPER g422_10_t5:  (1, 1, 1)
```

The comparator reads both sides. A zero above would have been meaningless.

### 1.6 Ragged-geometry sweep (mine, not the lane's) — 11/11 byte-exact

The four committed cells are all 320x240-ish. The port's per-axis clipping is where a
ragged tail would break, so I encoded and compared eleven more geometries, including
tall-thin (`34x130`), narrow-short (`100x50`, `130x34`) and sub-block (`66x66`):

```
g422_320x240 (0,0,0)  g422_322x242 (0,0,0)  g422_66x66  (0,0,0)
g422_64x98   (0,0,0)  g422_100x50  (0,0,0)  g422_130x34 (0,0,0)
g444_320x240 (0,0,0)  g444_66x66   (0,0,0)  g444_100x50 (0,0,0)
g444_34x130  (0,0,0)  g444_162x98  (0,0,0)
```

All BYTE-EXACT. `g444_34x130` in particular forces `ver_boundary_overlap`'s `width == 2`
arm (the `27/17` blend, dead at 4:2:0) over a ragged tail — the claim that the port makes
those arms live holds against the oracle, not just against the in-crate reference.

---

## 2. Grain controls (claim G-f)

```
$ cargo test -p ec-av1 --lib film_grain
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 800 filtered out
$ cargo test -p ec-av1 --lib -- 422 444 chroma
test result: ok. 95 passed; 0 failed; 0 ignored; 0 measured; 713 filtered out
$ EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib \
    a_pixel_exact_444_stream_walks_the_same_coefficient_units
test result: ok. 1 passed; 0 failed
```

**G-f CONFIRMED.** The 8 and the 95 reproduce exactly.

---

## 3. The three libaom traps, against the C

Read from `~/.cache/aom-oracle/src/av1/decoder/grain_synthesis.c` (oracle snapshot).

### Trap (a) — the chroma BLOCK is `16 << (1 - ss)`, not `chroma_subblock_size << (1 - ss)`

`add_noise_to_block` sizes its loop from **luma half-extents** scaled per axis:

```c
680|  for (int i = 0; i < (half_luma_height << (1 - chroma_subsamp_y)); i++) {
681|    for (int j = 0; j < (half_luma_width << (1 - chroma_subsamp_x)); j++) {
```

and every caller passes the *luma* half-extent, capped at `luma_subblock_size >> 1 = 16`:

```c
1285|            AOMMIN(luma_subblock_size_y >> 1, height / 2 - y) - i,
1286|            AOMMIN(luma_subblock_size_x >> 1, width / 2 - x) - j, bit_depth,
```

So the block is `16 << (1 - ss_y)` by `16 << (1 - ss_x)`: 16x16 at 4:2:0, **32x16 at
4:2:2**, **32x32 at 4:4:4**. `chroma_subblock_size << (1 - ss)` (where
`chroma_subblock_size = 32 >> ss`) would give 64 at `ss == 0` — 2x. **Trap confirmed.**

Rust, `film_grain.rs:1494-1502` (and the twin at `:2285-2287`):

```rust
// the block size is `16 << (1 - ss)`, NOT `chroma_subblock_size << (1 - ss)`: the two differ wherever
let cblk_stride = ((LUMA_SUBBLOCK >> 1) << (1 - ss.x)) as usize;
let cblk_rows = ((LUMA_SUBBLOCK >> 1) << (1 - ss.y)) as usize;
```

`LUMA_SUBBLOCK >> 1 == 16`. Matches the C.

### Trap (b) — the block walk is in LUMA units at every subsampling

```c
1080|  for (int y = 0; y < height / 2; y += (luma_subblock_size_y >> 1)) {
1083|    for (int x = 0; x < width / 2; x += (luma_subblock_size_x >> 1)) {
```

`height / 2` and `width / 2` are **luma** and must not become the chroma stride. Only the
chroma ORIGIN and EXTENT derived from them are per-axis — the origin is
`(y + i) << (1 - chroma_subsamp_y)` (`:1129-1130`, `:1143-1144`, `:1269-1270`).
**Trap confirmed.**

Rust, `film_grain.rs:1363` and `:1418-1421`:

```rust
let chroma_row = |y: usize| (2 * y) >> ss.y;
let nrows = ((height / 2).max(0) as usize).div_ceil(row_step as usize);
```

`(2*y) >> ss.y` is `(y + i) << (1 - ss_y)`. Matches.

### Trap (c) — `copy_area`'s `cb_col_buf + (chroma_subblock_size_y << (1 - ss_x))` is a FLAT ELEMENT offset

The C's `copy_area` takes a **pointer**, and advances it by the stride itself:

```c
873| static void copy_area(int *src, int src_stride, int *dst, int dst_stride,
874|                       int width, int height) {
875|   while (height) {
876|     memcpy(dst, src, width * sizeof(*src));
877|     src += src_stride;
```

so `src_row0 * src_stride + src_col0` is how a row index reaches an offset. The call:

```c
1296|          copy_area(
1297|              cb_col_buf + (chroma_subblock_size_y << (1 - chroma_subsamp_x)),
1298|              2 >> chroma_subsamp_x,
1299|              cb_line_buf + (x << (1 - chroma_subsamp_x)), chroma_stride,
1300|              2 >> chroma_subsamp_x, 2 >> chroma_subsamp_y);
```

The first argument is a flat element offset into `cb_col_buf`; the *second* is the stride.
Rust's `copy_area` (`film_grain.rs:1217-1226`) takes an explicit `src_row0`, so the flat
value had to be folded back into a row index. The port passes `csub_y`:

```rust
copy_area(&mut cb_line_buf, c_stride, 0, x << (1 - ss.x),
          &cb_col_buf, ss.two_x(), csub_y, 0, ss.two_x(), ss.two_y(), fctx);
```

**I re-derived the arithmetic independently:** row `csub_y` at stride `2 >> ss_x` is
`csub_y * (2 >> ss_x)`, and `2 >> ss_x == 1 << (1 - ss_x)`, so this equals
`csub_y << (1 - ss_x)` = `chroma_subblock_size_y << (1 - ss_x)` — the C's flat offset, for
every `ss_x`. And the lane's "len is 68 but the index is 128 at 4:4:4" checks out against
the C's allocation (`(csub_y + (2 >> ss_y)) * (2 >> ss_x)` = `(32 + 2) * 2 = 68`):
passing the flat `64` as a row index addresses `64 * 2 = 128`, past the end.
**Trap confirmed, and the port's row index is the only correct one.**

### Other sites I checked and found faithful

`ver_boundary_overlap` chroma width `2 >> ss_x` and height
`AOMMIN(csub_y + (2 >> ss_y), (height - 2y) >> ss_y)` (`:1104-1120`) →
`film_grain.rs:1558` `hc_count`; `hor_boundary_overlap` chroma width
`AOMMIN(csub_x - ((x?1:0) << (1-ss_x)), (width - ((x?x+1:0) << 1)) >> ss_x)` (`:1183-1194`)
→ `film_grain.rs:1658-1659` `cskip` / `cw`; the two blend coefficient sets
`23/22` vs `27/17` and `17/27` (`:912-939`, `:941-970`) → `overlap_inplace`'s
`skip == 1` / `j == 0` / else arms and the reference's `(2 >> ss_y) == 1` / `i == 0` / else
arms; `average_luma` horizontal pair at `ss_x == 1` and bare sample at `ss_x == 0`
(`:683-691`) → `film_grain.rs:797-810` `if pair { ... } else { ... }`, and the SIMD
dispatch guard `if pair && n >= 4 && Avx2` (`film_grain.rs:756`) which correctly never
sends a 4:4:4 row to the pair-shaped AVX2 kernel.

Two arm-counter facts, because they bear on G-c: `chroma422_square/rect/sub8` and
`ibc128` are all **0** on a 4:4:4 cell (`s444_320x240`) — the 4:2:2 arms are correctly keyed
to `ss (1,0)` and did not start firing at 4:4:4.

---

## 4. Census: the widening, re-derived

### 4.1 Method

Two `decode_probe` builds from **the same tree**, differing in exactly one expression —
`set_census_nonsub`'s `ss_x == 0 || ss_y == 0` vs `ss_x == 0 && ss_y == 0`:

```
before = main + probe print, arming reverted to (ss_x == 0 && ss_y == 0)
after  = main
```

Both carry the probe bypass so both can decode 4:2:2 at all.

### 4.2 C-1 — the 4:2:2 delta (mine vs the lane's §3.2)

| cell | mine BEFORE | mine AFTER | lane AFTER |
|---|---|---|---|
| `s422_320x240` | `[0,0,0]` / coded `[0,0,0]`, total **4994** | `[2338,1328,1328]` / coded `[1020,843,949]` | `[2338,1328,1328]` / `[1020,843,949]` / 4994 |
| `s422_320x242_10b` | `[0,0,0]`, total **4344** | `[2046,1149,1149]` / coded `[860,799,825]` | identical |
| `s422_326x240` | `[0,0,0]`, total **5664** | `[2798,1433,1433]` / coded `[1200,876,1029]` | identical |
| `s422_416x250_10b` | `[0,0,0]`, total **5779** | `[2927,1426,1426]` / coded `[1068,837,996]` | identical |
| `g422_t5` (mine) | `[0,0,0]`, total **2501** | `[1443,529,529]` / coded `[238,268,269]` | identical |

**C-1 CONFIRMED, digit-for-digit on all five cells.** `units_total` is unchanged across
the widening, which is what makes the `[0,0,0]` column a vacuity proof rather than an
empty walk: the decode walked 4994 units while the census read zero.

### 4.3 C-2 — the controls, byte-for-byte against `19b16ffd`

```
av1_192x128_8bit_intra64_in_inter  4:2:0  IDENTICAL  [0,0,0]/[0,0,0] total 405
s420_320x240                       4:2:0  IDENTICAL  [0,0,0]/[0,0,0] total 3514
ll444_minp8_inter                  4:4:4  IDENTICAL  [3764,3764,3764]/[975,1793,1859] total 11292
444_lossy_rect4_inter_witness      4:4:4  IDENTICAL  [629,372,372]/[130,253,278] total 1373
s444_320x240                       4:4:4  IDENTICAL  [2920,2104,2104]/[1178,1367,1561] total 7128
```

**C-2 CONFIRMED** with my own binaries (not the lane's). The two conditions are
disjoint, so the 4:2:0 zero control and the 4:4:4 exact counts are structurally unable to
move — and they did not. The existing 4:4:4 gate that pins `[3764,3764,3764]` /
`[975,1793,1859]` against libaom's `EC_ECDUMP_IN` still passes.

Whole-sweep check over all 54 cells (18 per format):

```
4:2:2 cells with a NON-ZERO per-plane census: 18/18
4:2:0 cells reading exactly [0,0,0]:           18/18
4:4:4 cells with a NON-ZERO per-plane census: 18/18
```

### 4.4 C-3 — the strip enumeration: 12 distinct units, no double-count

I re-derived the domain independently of the Rust loop (same `wl/hl` bounds, same
`max == 64` and `ratio in {2,4}` filters, the same three formats, the same
`min(_, 32)` cap) and printed every row:

```
16x64 ss(0,0) chroma 16x64 unit (16,32) IN      16x64 ss(1,0) chroma 8x64  unit (8,32)  IN
16x64 ss(1,1) chroma  8x32 unit  (8,32) IN      32x64 ss(0,0) chroma 32x64 unit (32,32) IN
32x64 ss(1,0) chroma 16x64 unit (16,32) IN      32x64 ss(1,1) chroma 16x32 unit (16,32) IN
64x16 ss(0,0) chroma 64x16 unit (32,16) IN      64x16 ss(1,0) chroma 32x16 unit (32,16) IN
64x16 ss(1,1) chroma 32x8  unit (32,8)  IN      64x32 ss(0,0) chroma 64x32 unit (32,32) IN
64x32 ss(1,0) chroma 32x32 unit (32,32) IN      64x32 ss(1,1) chroma 32x16 unit (32,16) IN

checked = 12   distinct (strip, format) pairs = 12   distinct units = 5
hit == handled: True   hit = [(8,32),(16,32),(32,8),(32,16),(32,32)]
```

- **12 iterations, 12 distinct `(strip, format)` pairs — nothing is counted twice.**
- Every resolved unit is a member of the `handled` set, so the
  `decode_block_rect64` `_` arm stays dead for 4:2:2: **C-3's "no unit missing" claim holds.**
- `hit == handled` holds, so the non-vacuity assert (`the enumeration stopped exercising
  every arm`) is not vacuous either — all five arms are still hit.

Mutation proof, re-run by me (removing `(1, 0)` from the loop):

```
$ cargo test -p ec-av1 --lib every_chroma_unit_a_64_axis_strip
assertion `left == right` failed: the strip domain is not the four 64-axis strips
  x three codable chroma formats (4:4:4, 4:2:2, 4:2:0)
  left: 8
 right: 12
```

Restored (`git checkout --`); the test passes again. Reverted and re-verified.

**The `(0,1)` exclusion is sound.** `sequence.rs:481` reads `subsampling_y` only when
`subsampling_x == 1` (and the 12-bit branch at `:479-483` does the same), so the reachable
set is exactly `{(1,1), (1,0), (0,0)}` and 4:4:0 cannot be handed to the decoder. The
existing gate `the_440_cell_is_not_a_codable_chroma_shape` passes.

### 4.5 What a 4:2:2 gate can now assert (the census lane's §5)

Demonstrated: `census_nonsub_units()` and `census_nonsub_coded()` read `[0,0,0]` on a
4:2:2 cell **by construction** under the old arming, and non-zero per plane under the new
one, on the same bytes. A gate could therefore assert non-zero on every plane and have it
be a real statement about the stream. Before the widening that assertion was a literal.

**One caveat for whoever writes that gate**, which I measured and which neither report
states: the counters are `thread_local!`, and so is `census_unit_n()`. With
`EC_AV1_THREADS=4` (default is 1, `stream.rs:410-421`):

```
EC_AV1_THREADS=1  4:4:4 [3764,3764,3764] total 11292   4:2:2 [2338,1328,1328] total 4994
EC_AV1_THREADS=4  4:4:4 [ 768, 768, 768] total  2304   4:2:2 [1081, 427, 427] total 1935
```

They read **partial**, not zero, so the failure mode is a plausible wrong number rather
than an obvious one. This is a pre-existing property of every counter in the crate and is
why `decode_threads()` defaults to 1 and documents that choice; it is not introduced here.
But §3.5 of the census report hands these numbers to a future gate as "the numbers a
committed gate should pin", and a gate must pin them under `EC_AV1_THREADS=1` (or assert
`units_total` consistency) or it is thread-count dependent.

---

## 5. The self-flagged gaps, checked rather than accepted

### C-4 — `chroma422_chunk` / `chroma422_pair_wide`: really 0, on all 18 cells

```
for all 18 sweep/s422_*.obu:
  chroma422_chunk: 0     chroma422_pair_wide: 0     intrabc_128rect_chroma_chunk: units=0 shape=0x0
```

The report says 0 "on all four cells". It is 0 on **every** 4:2:2 cell in the 54-cell
sweep — stronger than claimed, and the conclusion is unchanged: no 4:2:2 gate can claim
128-root chunked chroma or the 4-wide strip pair on this evidence.

Reading the two arm sites confirms what would reach them and that the census would count
their units if they ran:

- `decode.rs:30892` `hit!(CHROMA422_CHUNK_HITS)` is keyed to `invalid_ss_plane` — the
  128-root chunked chroma walk — reached through `read_inter_plane_rect`, so its units
  **would** be inside a census total. The blocker is a recipe, not the census.
- `decode.rs:13084` `hit!(CHROMA422_PAIR_WIDE_HITS)` is keyed to `chroma_422_pair_wide`,
  documented at `decode.rs:2211-2216` as "keyed to a 2-px-wide chroma block, which only a
  1-mi-wide strip at ss (1, 0) produces". Same conclusion.

So the census's structural reach does cover those arms; it is the fixture set that does
not. The report's framing is accurate.

### C-5 — `g422_t5` really is unusable through the dump path

Proved, not taken on faith. `EC_AV1_FINAL_DUMP` writes the pre-grain picture
(`stream.rs:2216-2233`, the `final_dump` block inside the function that returns
`FrameOutput { picture, .. }`, before `apply_grain` runs at `:1326`):

```
$ EC_AV1_FINAL_DUMP=dump_grain    decode_probe g422_t5.obu
$ EC_AV1_FINAL_DUMP=dump_nograin EC_AV1_NO_GRAIN=1 decode_probe g422_t5.obu
$ cmp dump_grain.f0 dump_nograin.f0   -> IDENTICAL
```

Byte-identical with and without grain synthesis: the dump is pre-grain. Against ffmpeg's
post-grain rawvideo:

```
DUMP-PATH Y: 58877/76800 differ (76.7%), max delta  4
DUMP-PATH U: 21325/38400 differ (55.5%), max delta 12
DUMP-PATH V: 20194/38400 differ (52.6%), max delta 16
```

**Exactly** the census report's §7.2 figures (76.7% / 55.5% / 52.6%, max 16). The
self-flag is accurate and correctly attributed: the cause is the dump's placement, not the
decode. The grain lane's `g422_t5` byte-exactness claim (§6 of its report) is reproducible
only through the in-memory sink, exactly as the census lane says.

This matters for sequencing: a grain-cell gate written on the `EC_AV1_FINAL_DUMP` path will
be red for a reason unrelated to film grain. That is a real trap and the census lane caught
it before someone else did.

---

## 6. Non-vacuity summary

| requirement | evidence | bites? |
|---|---|---|
| grain comparator reads both sides | flipped oracle sample per plane → exactly `(1,1,1)` on 4 cells | yes |
| grain synthesis actually ran at 4:2:2 | `EC_AV1_NO_GRAIN=1` differential, 100396 samples | yes |
| the grain pass comes from this port | real pre-fix tree `697b1308`, `0/23370/22631` | yes |
| strip enumeration is load-bearing | `(1, 0)` removed → `left: 8, right: 12` | yes |
| census arming is load-bearing | `(ss_x==0 && ss_y==0)` → `[0,0,0]` on a 4994-unit walk | yes |
| 4:2:0 / 4:4:4 controls unmoved | before/after binaries IDENTICAL on 5 controls | yes |
| census counts every reading unit | `read_coeffs` has one `return` (`decode.rs:8403`, `all_zero`) and calls `census_unit` at `:8393` **before** it; `read_coeffs_rect` likewise (`:8690` before `:8692`). No other `return` in either function. | yes |

---

## 7. Findings

### F1 — the census report's strip-derivation table has a wrong cell

`lanes/av1422census422.report.md:46` lists the 32x64 strip at 4:4:4 as resolving to chroma
unit **`16x32`**. The loop it is supposed to be transcribing computes
`(bw >> ss_x, bh >> ss_y).min(32)`; at `ss (0,0)` a 32x64 strip is a **32x64** chroma
block, so the unit is **`(32, 32)`**. My independent enumeration of the domain prints
`32x64 ss(0,0) chroma 32x64 unit (32,32) IN`.

The code is right — the test's own enumeration and its `arms == handled` pin both use the
correct value, and the test passes. Only the report's table is wrong, and it is wrong in
the one direction that would matter if someone re-derived the domain from the prose: it
makes the 4:4:4 32x64 row look like it lands on the same arm as the 16x64 row, which is
not true. Fix the cell to `(32, 32)`.

The remaining three rows and the 4:2:2 column are correct, and the report's prose
("the cap resolves both to `(16, 32)`" for the 32x64 pair) is correct.

### F2 — the merge message on `19b16ffd` claims a committed gate that is not in the tree

The `19b16ffd` commit message asserts three things about instrumentation:

> "A real 4:2:2 film-grain stream decodes byte-exact with `grain_hits()` rising per cell
> (3 -> 4), **the gate re-parses the OBUs and asserts a frame header carries
> `film_grain.apply_grain`** ... **Coverage closed**: 4:2:2 and 4:4:4 film grain had NO
> test before this lane"

`crates/ec-av1/src/film_grain.rs` contains exactly four `#[test]`s
(`simd_matches_scalar_luma_noise_row`, `simd_matches_scalar_chroma_noise_row`,
`untouched_planes_and_ragged_tail_come_back_clean`,
`ragged_overlap_tail_matches_spec_reference`), and
`crates/ec-av1/fixtures/` contains no grain fixture. There is **no committed 4:2:2 or
4:4:4 byte-exactness gate** — the measurements live in a scratch tree that was deleted.

Two of the three assertions are still true. `ragged_overlap_tail_matches_spec_reference`
*is* now driven over all three formats from one per-axis reference with the
last-chroma-row assertion, and I ran it (part of the 8). But the *oracle* comparison — the
thing that catches a regression against libaom rather than against our own transcription —
is not in the tree. `grain_hits()` is `pub(crate)` and unreachable from `decode_probe`, the
only instrument that can read a 4:2:2 stream while the refusal stands, so no external
check can substitute for it either.

The lane's own report is honest about this (§9: "The end-to-end gate is NOT committed, and
cannot be until the guard is lifted"), so this is a merge-message overclaim, not a hidden
gap. It still matters: the commit message is what a reviewer reads, and it says the coverage
is closed when a whole-stream regression would pass unnoticed. I reproduced the four cells
byte-exact independently (§1.2), so the fix is right — the risk is purely that nothing
guards it.

### Non-findings (checked, clean)

- No decode defect found in `film_grain.rs` or `decode.rs`. Every geometry expression I
  traced back to the C matches, including the three traps and the two `== 2` blend arms.
- The census widening is a one-expression change; `census_unit` is called on every exit path
  of both readers, before the return, so the "counts every reading unit" claim holds.
- `chroma422_*` arms correctly do not fire at 4:4:4, so the widening did not silently
  re-key an arm.
- `census_nonsub_units` / `census_nonsub_coded` being made `pub` while `grain_hits` stayed
  `pub(crate)` is an asymmetry, not a defect: an in-crate `#[cfg(test)]` gate can read
  `grain_hits` fine.

## 8. Discipline

- Primary checkout `/home/tahinli/Documents/Code/Rust/edith_codecs`: `git status --porcelain`
  **empty**; `TEMP-PROBE-BYPASS` count **0** in `stream.rs`, `decode.rs`,
  `refusal_inventory.rs`, `film_grain.rs`. Nothing was edited there.
- This worktree: no decoder source modified; only this report is added.
- `vfy-after` and `vfy-prefix` were the only bypassed trees; both were `git checkout --`-restored
  and then `git worktree remove --force`'d. `git worktree list` no longer lists either.
- Two mutations were run (census arming revert, strip-format narrowing). Both were reverted in
  the same tree and the tree verified clean before removal.
- `EC_AV1_ALLOW_422_PROBE` was never used — the bypass was patch-run-restore, as the env var
  is inert.
