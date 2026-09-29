# lane-av1superhbd — 4:4:4 SUPERRES at 10-bit and 12-bit

**Verdict: EXACT at both depths. Five gates added, all green, all mutation-proved red-before.**

Branch `lane-av1superhbd` (base `a21f3680`); the gate+fixture commit is the
parent of this report's commit.
Oracle: libaom `v3.13.3-7-g9bb526a` (`~/.cache/aom-oracle/build/aomdec`).

---

## 1. Parsed headers — read from each stream's OWN sequence/frame header

Every number below comes from `ec_av1_syntax`'s `Av1Parser` walking the pinned
bytes, never from the encoder flags. A stream that parses `use_superres=false`
was discarded, not pinned.

### Accepted (all five)

| stream | bytes | sha256 | SEQ `bit_depth` | SEQ `ss` | SEQ `enable_superres` | per-frame `use_superres` | per-frame `superres_denom` | per-frame `upscaled_width` | per-frame coded `frame_width x frame_height` |
|---|---|---|---|---|---|---|---|---|---|
| `444_lossy_superres_256x128_d12_10bit.obu` | 15803 | `95b49d7aa81843399ee3aa64bdadfec8f4037bcc3f861923c9b2da3bc868b223` | 10 | (0,0) | true | true x4 | 12, 12, 12, 12 | 256, 256, 256, 256 | 171x128 x4 |
| `444_lossy_superres_mode2_256x128_10bit.obu` | 17776 | `699398aba58fb53b80e05d3d214eb746a3c91945e01da6b47488513f20b55668` | 10 | (0,0) | true | true x4 | 11, 14, 15, 9 | 256, 256, 256, 256 | 186x128, 146x128, 137x128, 228x128 |
| `444_lossy_superres_256x128_d12_12bit.obu` | 11023 | `973bf374c0185c4e6301509ce834cef01318e6cdb9488fd258118d7ec89bdd04` | 12 | (0,0) | true | true x4 | 12, 12, 12, 12 | 256, 256, 256, 256 | 171x128 x4 |
| `444_lossy_superres_256x128_d9_12bit.obu` | 13015 | `898c775108495b998cb5ec9b08e194d4656ed0140aa8e7e7236892d468bbad99` | 12 | (0,0) | true | true x4 | 9, 9, 9, 9 | 256, 256, 256, 256 | 228x128 x4 |
| `444_lossy_superres_mode2_256x128_12bit.obu` | 14656 | `18aa8b3124ab10672f22577a8eea81e44141a3d0bb506104aee2f950254a2e9f` | 12 | (0,0) | true | true x4 | 11, 14, 15, 9 | 256, 256, 256, 256 | 186x128, 146x128, 137x128, 228x128 |

Scaled-size arithmetic re-derived per frame and held on all 20 frame headers:
`(upscaled_width*8 + denom/2)/denom == frame_width` —
`(256*8+6)/12 = 171`, `(256*8+1)/9 = 228`, `(256*8+5)/11 = 186`,
`(256*8+7)/14 = 146`, `(256*8+7)/15 = 137`.

### Discarded

Nothing was discarded for `use_superres=false` — every stream above was built
with an explicit `--superres-denominator` / `--superres-kf-denominator` pair, so
the libaom no-op (denominator flag absent -> denom 8, nothing scaled) could not
occur. The two mode-2 arms carry denominators 11/14/15/9, three of which no
command-line flag ever named, which is itself the proof that the headers are
what the gates read.

---

## 2. Verdicts — exactness against the instrumented oracle

Frame counts and per-frame byte lengths are asserted by
`decode_all_frames_vs_oracle` BEFORE the pixel compare; it writes
`EC_AV1_FINAL_DUMP` frames on both sides and compares them byte-for-byte in
DECODE order, so a frame-length mismatch is a hard failure, not a short compare.

| gate | frames compared | hidden | divergent | `superres_hits` delta | `predict_scaled_hits` delta |
|---|---|---|---|---|---|
| `a_444_lossy_superres_10bit_d12_stream_decodes_pixel_exact` | 4/4 | 0 | **0** | 4 | 2097 |
| `a_444_lossy_superres_10bit_mode2_random_denom_stream_decodes_pixel_exact` | 4/4 | 0 | **0** | 4 | 2181 |
| `a_444_lossy_superres_12bit_d12_stream_decodes_pixel_exact` | 4/4 | 0 | **0** | 4 | 1311 |
| `a_444_lossy_superres_12bit_d9_stream_decodes_pixel_exact` | 4/4 | 0 | **0** | 4 | 1956 |
| `a_444_lossy_superres_12bit_mode2_random_denom_stream_decodes_pixel_exact` | 4/4 | 0 | **0** | 4 | 1737 |

`test result: ok. 5 passed; 0 failed; 0 ignored` (712 filtered out).

---

## 3. Gates

`crates/ec-av1/src/stream.rs`, one shared body `a_444_hbd_superres_arm` plus
five `#[test]` arms. Every arm asserts:

- the pinned bytes (length + FNV-1a), read at full decode order;
- `ss (0,0)` from the sequence header, and **`bit_depth` equal to the depth the
  gate names** — the high-bit-depth claim is otherwise unprovable, because a
  mistyped `--bit-depth` still yields a decodable scaled 4:4:4 stream, just at
  a different cell (the same phantom-cell trap as the `use_superres=false`
  case lane-av1444edge r3 recorded);
- `enable_superres` on the sequence header and `use_superres` on every parsed
  frame header;
- the per-frame PARSED `superres_denom` against a `(denominator, coded width)`
  table in decode order, the upscaled size, and the coded size;
- `FrameWidth == (UpscaledWidth*8 + denom/2)/denom` re-derived from that frame's
  own parsed denominator — not a literal comparison alone, so a re-pinned
  fixture cannot satisfy the numbers while the scaled-size arithmetic moves
  under them;
- `superres_hits` and `predict_scaled_hits` as **DELTAS** (both are
  process-wide counters with no reset, so absolute values would pass on residue
  from another stream);
- every returned frame at the UPSCALED size with full-resolution 4:4:4 chroma
  plane lengths, and every sample bounded by `(1 << bit_depth) - 1` — a decoder
  that produced 8-bit values padded into a `u16` plane would satisfy every
  geometry assert and only be caught here;
- all decode-order frames byte-exact against the instrumented `aomdec`.

The two mode-2 arms add the **distinct-denominator assert** the charter asked
for: `distinct.len() >= 3` across the parsed denominators, and at least one
denominator not in `{9, 12, 16}`. So neither arm can silently stop being a
scaled, multi-ratio cell — a libaom that collapsed mode 2 onto one denominator,
or started honouring `--superres-denominator` under mode 2, turns the arm red
instead of quietly re-pinning it into a different claim.

### Red-before (both mutations on the committed tree, then reverted)

| mutation | site | result |
|---|---|---|
| tap alignment in the upscale filter: `let idx = base + int_pel + k as i64 + 1;` | `superres.rs:141` (`upscale_row`) | **5/5 RED at pixel level.** e.g. 10-bit d12: `decode-order frame 0 of 4 differs from the oracle at byte 0 (ours 40 vs 38), 54916 bytes differ`; 10-bit mode-2: `byte 0 (ours 41 vs 40), 53206 bytes differ` |
| counter: `note_upscaled_picture` body emptied (`hit!(SUPERRES_HITS)` removed) | `superres.rs:188` | **5/5 RED** on the non-vacuity assert: `zero superres_hits -- the upscaler never ran (class gate-blind-to-feature)` |

Both reverted; `git status --porcelain` clean; the five gates re-run green on
the committed tree afterwards.

---

## 4. Recipes

`-strict -1` is load-bearing on the y4m pipe: without it ffmpeg refuses to
write `yuv444p10le` / `yuv444p12le` at all (zero bytes out).

10-bit (`testsrc2`, both arms):

```text
ffmpeg -f lavfi -i "testsrc2=size=256x128:rate=25" -frames:v 4 \
       -pix_fmt yuv444p10le -strict -1 -f yuv4mpegpipe - | \
aomenc --codec=av1 --profile=1 --bit-depth=10 --input-bit-depth=10 \
       --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 --threads=1 \
       --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 --limit=4 \
       --superres-mode=1 --superres-denominator=12 \
       --superres-kf-denominator=12 --obu -o - -
```

Swap the last three lines for `--superres-mode=2 --obu -o - -` for the mode-2 arm.

12-bit (`mandelbrot`, see §5):

```text
ffmpeg -f lavfi -i "mandelbrot=size=256x128:rate=25" -frames:v 4 \
       -pix_fmt yuv444p12le -strict -1 -f yuv4mpegpipe - | \
aomenc --codec=av1 --profile=1 --bit-depth=12 --input-bit-depth=12 \
       --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 --threads=1 \
       --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 --limit=4 \
       --superres-mode=1 --superres-denominator=<9|12> \
       --superres-kf-denominator=<9|12> --obu -o - -
```

Byte-reproducible: two independent encodes of the 12-bit den=12 recipe produced
the same 11023 bytes, sha256 `973bf374…`.

---

## 5. The 12-bit source: `mandelbrot`, not `testsrc2`

`testsrc2` at 12 bits parses `allow_screen_content_tools = true` on every
frame, which the crate refuses by name
(`a 12-bit frame with screen content tools (allow_screen_content_tools=1: …)`)
— palette and intrabc have no 12-bit witness. The identical recipe at **10 bits**
does not trip it, so the flag set is not the variable; the source content is.

Flag-combination attempt table, all read from the pinned-then-parsed headers
(same geometry, same 12-bit depth, `--superres-mode=1 --superres-denominator=12`):

| attempt | `allow_screen_content_tools` (parsed) | our decoder |
|---|---|---|
| `testsrc2` yuv444p12le, base flags | true | REFUSED by name |
| + `--enable-palette=0 --enable-intrabc=0` | true | REFUSED by name |
| + `--tune-content=film` | true | REFUSED by name |
| + both, `--tune-content=film` | true | REFUSED by name |
| `--cpu-used=8` | true | REFUSED by name |
| `--cq-level=40` | true | REFUSED by name |
| `testsrc` yuv444p12le | true | REFUSED by name |
| `mandelbrot` yuv444p12le | **false** | **decodes, 4 frames 256x128** |
| `nullsrc` yuv444p12le (135-byte stream) | false | decodes — but vacuous, no content |

Root cause in the oracle source, so the next lane does not re-derive it:
`av1_set_screen_content_options` (`av1/encoder/encoder.c:2427`) defers to
`encoder_utils.c:1153`, which keys the decision on the **first pass's
palette-pixel ratio**, not on `--enable-palette` / `--enable-intrabc` /
`--tune-content`. A photographic source keeps the ratio below threshold.
Recorded in the gate doc comment as well.

---

## 6. Correction to `lanes/av1formatsweep.report.md`

The not-measured list is closed for this cell. Proposed replacement text only —
this lane does not edit that file (another lane corrected parts of it today and
the merge must apply cleanly).

**Line 83** (the 4:4:4 matrix row) — current:

```markdown
| superres | – | – | – |
```

proposed:

```markdown
| superres | Y (5 gates, `444_lossy_superres_*` pins) | **Y (gates, this lane)** | **Y (gates, this lane)** |
```

**Line 493** (§6.5 "Still not measured after this round") — current fragment:

```markdown
Still not measured after this round: 4:4:4 at 10/12 bits with **tile rows
actually enabled** (needs a >= 256-high geometry — trap 2); 4:4:4 superres at
10/12 bits; 4:4:4 odd dims at 10/12 bits and at dimensions other than 66x66 /
```

proposed:

```markdown
Still not measured after this round: 4:4:4 at 10/12 bits with **tile rows
actually enabled** (needs a >= 256-high geometry — trap 2); 4:4:4 odd dims at 10/12 bits and at dimensions other than 66x66 /
```

**Line 438** (§6.2 table) — the `sr444` DIVERGENT row predates lane-av1444edge
r3, which showed that stream passes `--superres-mode=1` with no denominator and
therefore parses `use_superres=false denom=8` on all four frames: it was never a
scaled cell. The 8-bit 4:4:4 superres coverage is `lane-av1superpin`'s
(`444_lossy_superres_256x128_d12.obu`, `_d9`, `_mode2`). I did not touch this
row — that correction belongs to the lane that made the measurement, and it is
already landed on `lane-av1superpin` (`9646f74d` "correct the published cells
the superres no-op got wrong"). Flagged here only so the merge does not
reintroduce the stale `D` next to the new `Y`s.

---

## 7. What is NOT claimed

- Only 4:4:4 (`ss 0,0`) at 10 and 12 bits, 256x128 upscaled geometry, lossy
  cq-20 cpu-used-2, and denominators 9/11/12/14/15. Not claimed: 4:2:0 HBD
  superres (the sweep's `sr420_12` already covers 4:2:0 12-bit), lossless HBD
  superres, tile-geometry interaction, or any other source.
- The 12-bit arms run on `mandelbrot` content only. `testsrc2`-class content at
  12 bits is still refused by the screen-content gate — that refusal is
  unchanged and this lane did not touch it.
- Frame counts are 4 per stream, all shown, zero hidden; these are
  `--lag-in-frames=0 --auto-alt-ref` default single-layer streams, so the
  hidden-frame arm of the oracle compare is exercised but has nothing to find.
