# lane-cpu2formatprobe — does the `--cpu-used 2` luma divergence bite a SUPPORTED format?

**Outcome in one line: no. At the census's exact `--cpu-used 2` recipe, 4:2:0 and 4:4:4 at
8 and 10 bit are byte-exact against `aomdec` in 16 of 16 cells (0/0/0 wrong samples on
every plane of every decode frame), while the same recipe's 4:2:2 cells still refuse by
name — the luma divergence is FORMAT-SPECIFIC to 4:2:2, which is the refused format, so
it is not a live defect in any supported cell at this encoder setting.**

Report-only. No decoder source was edited, nothing was committed, no fix is proposed.
Tree under test: `main` = **`e0818f17`** of `/home/tahinli/Documents/Code/Rust/edith_codecs`.

---

## 1. Provenance

| role | binary | path |
|---|---|---|
| our decoder | `decode_probe` example, built from `main` `e0818f17` | `/home/tahinli/.cache/tgt/cpu2probe/debug/examples/decode_probe` (`cargo build -p ec-av1 --example decode_probe`, `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/cpu2probe`) |
| oracle encoder | instrumented `aomenc` | `/home/tahinli/.cache/aom-oracle/build/aomenc` |
| oracle decoder | instrumented `aomdec` (carries the `EC_AV1_FINAL_DUMP` rung) | `/home/tahinli/.cache/aom-oracle/build/aomdec` |
| source video | system `ffmpeg` 8.1.3, `testsrc2` | `ffmpeg` |

Both sides dump **decode-order** frames as `EC_AV1_FINAL_DUMP=<prefix>` writing
`<prefix>.f<N>` by DECODE index, hidden alt-refs included. The comparator is the 4:2:2
census's own `cmp422.py`, **imported, not reimplemented**:
`/home/tahinli/.cache/census422b/cmp422.py` (plus its `run.py::y4m_geometry`). Driver
and controls: `/home/tahinli/.cache/cpu2probe/{run,liveness,cross}.py`, cells in
`/home/tahinli/.cache/cpu2probe/cells/`, dumps in `…/work/`, JSON in
`…/{results,liveness,cross}.json`.

The four disciplines that instrument carries, unchanged:

1. **Geometry is an explicit argument**, parsed from `aomdec`'s OWN y4m header
   (`W320 H242 … C422` / `C420p10`), never from a file size; a frame whose length
   disagrees with the geometry is a hard error.
2. **Plane attribution is per frame**, from that frame's own plane lengths.
3. **A wrong sample is one `bps`-wide unit** — at 10 bit the walk is over 16-bit LE
   samples, so the top bits cannot hide.
4. **A count over zero frames is a hard error** naming the vacuous `0/0/0`.

---

## 2. Inputs, built correctly

Source: `ffmpeg -f lavfi -i "testsrc2=size=WxH:rate=25" -frames:v 16 -pix_fmt <pix> -f rawvideo`.

The y4m is assembled by the census's own builder (`mksweep.sh`/`mkrecipe.sh`, copied
verbatim into `/home/tahinli/.cache/cpu2probe/mkencode.sh`): libaom's `y4m_input_fetch_frame`
requires the six bytes `FRAME\n` **before every frame**, which is the framing bug that
made the earlier "42 encode" sweep weak evidence. This builder writes a correct y4m (and
asserts the raw length is a whole number of frames before writing). All 20 encodes decode
to 16 shown frames each, so no input was the degenerate zero-frame case.

## 3. Encoder command lines

One command, five variables, everything else held fixed — so **format is the only
independent variable** and the encoder mode is identical to the census's `rc` recipe:

```bash
AOMENC=/home/tahinli/.cache/aom-oracle/build/aomenc
$AOMENC --codec=av1 --profile=<0:420|1:444|2:422> \
        --input-bit-depth=<8|10> --bit-depth=<8|10> \
        --limit=16 --lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=1 \
        --pass=1 --cq-level=24 --threads=4 --kf-min-dist=0 --kf-max-dist=999999 \
        --width=<W> --height=<H> --cpu-used=<2|6> --obu -o <cell>.obu <cell>.y4m
```

`--pass=1` is libaom's `--end-usage=q` (constant quality), `--cq-level=24` is the census's
value, and `--cpu-used` is the only flag varied along the recipe axis. This is the census's
`mkrecipe.sh` flag set verbatim; **it reproduces the census's own 4:2:2 bytes exactly**,
which is the provenance proof that the recipe is the same recipe:

| cell | mine | census (`census422b/recipe.log`) |
|---|---|---|
| `rc2_s422_320x242` | 20600 B, `3bd62e422dfb7911` | 20600 B, `3bd62e42…` |
| `rc6_s422_320x242` | 21310 B, `462f39ec882b9425` | 21310 B, `462f39ec…` |

Cells: geometries 320x242 (odd height) and 256x128; formats 4:2:0, 4:4:4, 4:2:2; depths
8 and 10 (4:2:2 at 8 bit only — the format the census swept); `--cpu-used` 2 and 6.
20 cells.

---

## 4. Results — per cell, per plane, per decode frame

Wrong **sample** counts, decode-order basis, geometry taken from `aomdec`'s own y4m
header. "First" is `(decode frame, plane, row, col, ours, oracle)`.

| cell | chroma | size | depth | cpu-used | header ss / depth (parsed) | our status | oracle | frames | wrong Y / U / V | first divergence |
|---|---|---|---|---|---|---|---|---|---|---|
| `rc2_s420_320x242_8b` | 4:2:0 | 320x242 | 8 | 2 | `11` / 8 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc6_s420_320x242_8b` | 4:2:0 | 320x242 | 8 | 6 | `11` / 8 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc2_s420_320x242_10b` | 4:2:0 | 320x242 | 10 | 2 | `11` / 10 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc6_s420_320x242_10b` | 4:2:0 | 320x242 | 10 | 6 | `11` / 10 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc2_s420_256x128_8b` | 4:2:0 | 256x128 | 8 | 2 | `11` / 8 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc6_s420_256x128_8b` | 4:2:0 | 256x128 | 8 | 6 | `11` / 8 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc2_s420_256x128_10b` | 4:2:0 | 256x128 | 10 | 2 | `11` / 10 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc6_s420_256x128_10b` | 4:2:0 | 256x128 | 10 | 6 | `11` / 10 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc2_s444_320x242_8b` | 4:4:4 | 320x242 | 8 | 2 | `00` / 8 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc6_s444_320x242_8b` | 4:4:4 | 320x242 | 8 | 6 | `00` / 8 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc2_s444_320x242_10b` | 4:4:4 | 320x242 | 10 | 2 | `00` / 10 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc6_s444_320x242_10b` | 4:4:4 | 320x242 | 10 | 6 | `00` / 10 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc2_s444_256x128_8b` | 4:4:4 | 256x128 | 8 | 2 | `00` / 8 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc6_s444_256x128_8b` | 4:4:4 | 256x128 | 8 | 6 | `00` / 8 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc2_s444_256x128_10b` | 4:4:4 | 256x128 | 10 | 2 | `00` / 10 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc6_s444_256x128_10b` | 4:4:4 | 256x128 | 10 | 6 | `00` / 10 | decodes | aomdec | 17 | **0 / 0 / 0** | none |
| `rc2_s422_320x242_8b` | 4:2:2 | 320x242 | 8 | 2 | `10` / 8 | **REFUSES** | aomdec | — | — | — |
| `rc6_s422_320x242_8b` | 4:2:2 | 320x242 | 8 | 6 | `10` / 8 | **REFUSES** | aomdec | — | — | — |
| `rc2_s422_256x128_8b` | 4:2:2 | 256x128 | 8 | 2 | `10` / 8 | **REFUSES** | aomdec | — | — | — |
| `rc6_s422_256x128_8b` | 4:2:2 | 256x128 | 8 | 6 | `10` / 8 | **REFUSES** | aomdec | — | — | — |

Totals: **16 supported cells decoded and byte-exact, 0 diverging; 4 of 4 4:2:2 cells
refused by name.** Not one supported cell has a single wrong sample on any plane in any
of its 17 decode frames.

The refusal, verbatim and identical on all four 4:2:2 cells (a refusal is a result, not
an error — it is the sequence header's own statement of the cell):

```
unsupported: AV1 decode_stream (a chroma format of 4:2:2 (subsampling_x != subsampling_y):
this decoder decodes 4:2:0 and 4:4:4; 4:2:2 is not ported, and 4:4:0 (0,1) is not a
codable cell)
```

Note on the 4:2:2 arm: the census's `436945`-wrong-luma count for `rc2_s422_320x242`
**cannot be re-measured on the shipped tree** — main has no `EC_AV1_ALLOW_422_PROBE` env
gate (it is a patch-run-restore source hack, deliberately never committed; grep finds only
doc comments referring to it). Reproducing that number would require editing the refusal,
which this lane is not permitted to do. The census's number is taken as given and is
untouched by anything measured here.

### 4.1 The exact cells are not vacuous streams

A byte-exact verdict is only worth what the stream exercised. These `--cpu-used 2`
streams hit non-trivial decoder arms, read from the run's own counters (each decoded 16
shown frames; `TILING: 23 frame headers parsed`; 17 decode-order dumps, so hidden alt-refs
are inside the compared set):

| counter (`decode_probe` stdout) | `rc2_s420_320x242_8b` | `rc2_s444_320x242_10b` |
|---|---|---|
| `cfl_ac` | `420sq=708 420rect=50` | `444=60` |
| `troy_chroma: dir_1to4_pairs` | 0 | 22 |
| `rect_intrabc_reads` | 87 | 100 |
| `intra_in_inter_palette` | `y=25 uv=23` | `y=6 uv=5` |
| `intrabc_hits` | 14 | 58 |
| `leaf8_intrabc_hits` | 9 | 9 |
| `skipped_intrabc_chroma_arm_hits` | 0 | 14 |
| `intrabc_rect` | 1 | 5 |
| `uv_left_ss_y: reads` | 608 | 0 |

CfL, palette-in-inter, intra-BC (including the skipped-chroma arm and rect reads),
leaf-8 intra-BC and rect transforms all fire on the cpu-2 streams, and all of them are
byte-exact.

---

## 5. Non-vacuity: what the comparator control was

Two independent controls, both run **before** any "exact" verdict is read off.

### 5.1 Per-cell, per-plane oracle flip — 54 arms, 54 PASS, 0 FAIL

One oracle **sample** flipped (bit 0 of the sample's low byte; at 10 bit the sample sits in
a 16-bit LE container and `2**10 < 2**16`, so the change is exactly +1 by construction),
in the middle of the named plane, in a named decode frame; each arm restored before the
next so the planes are independent. Requirement: the SAME comparator's counts move by
**exactly +1 in that plane**, every other plane at +0, and every other frame's three
counts bit-identical.

* 16 cells × 3 planes at decode frame 16 (last): 48/48 PASS, all `other_frames_unchanged`.
* 2 cells × 3 planes at decode frame **0**: 6/6 PASS (`rc2_s420_320x242_8b`,
  `rc6_s444_320x242_10b`) — e.g. Y `51`→`50`, U `f0`→`f1`, V `6e`→`6f` on baseline
  `0/0/0` → `1/0/0`, `1/1/0`, `1/1/1`, with the other 16 frames untouched.

At 10 bit the arms show the 16-bit container behaviour, e.g. `4401`→`4501` (+1 Y),
`6801`→`6901` (+1 U), `c003`→`c103` (+1 V).

Log: `/home/tahinli/.cache/cpu2probe/liveness.json` (last frame) and
`cross.json::flip_at_frame0` (frame 0).

### 5.2 Cross-stream control — 8 same-geometry pairs, 8 DIVERGE

The flip control cannot rule out "the comparator compared our stream with itself". So:
cell A's **ours** dumps against cell B's **oracle** dumps, where A and B share the exact
geometry, chroma format and bit depth but are **different streams** (same content,
different `--cpu-used`). All 8 pairs must diverge hard:

| ours | vs oracle of | geometry | Y / U / V | first |
|---|---|---|---|---|
| `rc2_s420_320x242_8b` | `rc6_s420_320x242_8b` | 320x242 `11` d8 | 125476 / 42343 / 41721 | f0 Y(0,53) ours 81 oracle 80 |
| `rc2_s420_320x242_10b` | `rc6_s420_320x242_10b` | 320x242 `11` d10 | 217724 / 76139 / 70941 | f0 Y(0,4) 317/318 |
| `rc2_s420_256x128_8b` | `rc6_s420_256x128_8b` | 256x128 `11` d8 | 73754 / 26788 / 24808 | f0 Y(0,0) 78/79 |
| `rc2_s420_256x128_10b` | `rc6_s420_256x128_10b` | 256x128 `11` d10 | 137616 / 44779 / 47012 | f0 Y(0,0) 312/313 |
| `rc2_s444_320x242_8b` | `rc6_s444_320x242_8b` | 320x242 `00` d8 | 146010 / 245465 / 255451 | f0 Y(0,54) 146/145 |
| `rc2_s444_320x242_10b` | `rc6_s444_320x242_10b` | 320x242 `00` d10 | 336685 / 457332 / 455053 | f0 Y(0,0) 319/318 |
| `rc2_s444_256x128_8b` | `rc6_s444_256x128_8b` | 256x128 `00` d8 | 81622 / 143560 / 152723 | f0 Y(0,0) 78/79 |
| `rc2_s444_256x128_10b` | `rc6_s444_256x128_10b` | 256x128 `00` d10 | 172711 / 255186 / 260196 | f0 Y(0,0) 312/313 |

A comparator that silently returned 0 on any of these would have failed. It returned
hundreds of thousands of wrong luma samples on every one.

*(A first pass of this control paired cells at random and mostly crossed geometries, so
the comparator hard-errored on the byte length instead — that is the geometry-explicit
discipline working, but it is not a tautology test. The table above is the re-run, paired
by geometry. A related first-pass bug: `sys.path.insert(0, census422b)` shadowed the local
`liveness.py` with the census's, so `WORK` pointed at a stale root and the first re-run
read "no frame dumps"; fixed by path order, recorded here because the same shadowing will
bite any future lane that imports a helper from the census instrument directory.)*

### 5.3 What makes the exact verdicts non-vacuous, in one line

16 supported cells × 17 decode frames × 3 planes, every frame's byte length checked
against the explicit geometry (a length disagreement is a hard error, and none fired),
zero wrong samples — measured by a comparator proven live by 54 flip arms and 8
cross-stream divergences on the very same dumps.

---

## 6. Verdict

**FORMAT-SPECIFIC.** At the census's `--cpu-used 2` recipe the luma divergence does
**not** bite 4:2:0 or 4:4:4 at 8 or 10 bit: 16 of 16 supported cells are byte-exact vs
`aomdec` (0/0/0 on every plane of every decode frame), while all four 4:2:2 cells from the
same recipe refuse by name. The `--cpu-used 2` mode itself reaches serious decoder arms in
the supported formats (CfL, palette-in-inter, intra-BC incl. skipped-chroma and rect,
leaf-8, rect TU) and is exact on all of them.

Consistent with, and sharpening, the census's own §5 reading: `--cpu-used` was the only
flag varied there too, its 4:2:0 controls were 22/22 exact and its 4:4:4 controls 21/22.
This lane adds the missing arm — the *same* `rc` recipe (byte-reproduced against the
census's own 4:2:2 output) at `--cpu-used 2` specifically, across both supported formats,
both depths, and two geometries including an odd height.

**No supported-cell defect is localisable, so no owning surface is named.** The only
open item is the census's 4:2:2 class E, which lives inside a refused format and is
therefore not reachable on a shipping path; nothing here changes its status, and no fix is
proposed.

## 7. What this does NOT establish

* One content source (`testsrc2`), one CQ (24), one frame count (16), one alt-ref /
  global-motion configuration. The census's own recipe-dependence class shows verdicts
  are recipe-sensitive in 4:2:2; a supported-format divergence could in principle need a
  different recipe to reach. What is established is that this recipe — the one that broke
  luma in 4:2:2 — does not break any supported cell.
* 12-bit is not covered here (not requested; supported, and a separate sweep's business).
* The census's `436945`-luma number was not re-measured (shipped tree refuses 4:2:2 and
  the bypass is deliberately not committed). It is quoted, not re-derived.

## 8. Reproduce

```bash
D=/home/tahinli/.cache/cpu2probe
$D/mkencode.sh                       # 20 encodes -> $D/cells/*.obu
python3 $D/run.py $D/results.json   # per plane per decode frame vs aomdec
python3 $D/liveness.py $D/results.json $D/liveness.json   # 48 flip arms
python3 $D/cross.py                                      # 8 cross-stream + 6 flip-f0 arms
```
