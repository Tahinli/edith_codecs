# lane-av1superpin — the last two measured-exact 4:4:4 superres members, and the correction of a cell that measured a no-op

**Base** `4c973b94` (lane-av1444edge r3 tip). **Branch** `lane-av1superpin`,
worktree `~/.cache/wt/v444pin`. Two commits:

| commit | what |
|---|---|
| `e47d8f60` | the two gates + the two pinned fixtures |
| (this file) | the `lanes/av1formatsweep.report.md` corrections |

**Oracle** `~/.cache/aom-oracle/build/{aomenc,aomdec}`, built from the
`scripts/build-aom-oracle.sh` source tree at libaom
**`v3.13.3-7-g9bb526a`** (`git describe` in `~/.cache/aom-oracle/src`, HEAD
`9bb526a`). **Not** established via `aomenc --version`: that build does not
accept `--version` as a flag, so the revision is read off the source tree the
binary was compiled from. All measurements below: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target`,
`EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1`, `--test-threads=1`,
`--nocapture`.

**Inherited, not re-derived.** The flag sweep in
`lanes/av1444edge.report.md` §11 and the "byte-exact 4/4 on all four scaled
streams" claim are that lane's measurements and are cited as inherited. What
this lane re-verified itself is stated as such in §2: the two streams it
encodes were re-encoded from the recipes and their frame headers re-parsed
here.

---

## 1. The flag-vs-parsed-header table

Flags as issued on the command line vs what the stream's OWN parsed frame
header says. Every row below was produced by encoding the recipe on this
oracle and parsing the result; the verdict column is never the flag.

Source: `testsrc2=size=256x128:rate=25 -frames:v 4 -pix_fmt yuv444p`,
common encode flags `--codec=av1 --passes=1 --end-usage=q --cq-level=20
--cpu-used=2 --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100
--limit=4 --obu`.

| verdict | flags | parsed FH0 | sha256 | source |
|---|---|---|---|---|
| no-op | (none, control) | `frame=256x128 upscaled=256 use_superres=false denom=8` | `06621606…` | inherited |
| no-op | `--superres-mode=1` (no denom) | `frame=256x128 upscaled=256 use_superres=false denom=8` | `06621606…` | inherited — **this is the sweep's `sr444` cell** |
| SCALED | `mode=1 den=9` | `frame=228x128 upscaled=256 use_superres=true denom=9` | `3d619ee0…` | inherited; **re-verified here** (§2) |
| SCALED | `mode=1 den=12` | `frame=171x128 upscaled=256 use_superres=true denom=12` | `450caa3e…` | inherited (already gated by `a_444_lossy_superres_stream_decodes_pixel_exact`) |
| SCALED | `mode=1 den=16` | `frame=128x128 upscaled=256 use_superres=true denom=16` | `194450c4…` | inherited |
| SCALED | `mode=2` (random) | **`frame=186x128 denom=11`, then `146x128 denom=14`, `137x128 denom=15`, `228x128 denom=9`** | `c18496cf…` | inherited; **the per-frame denominators re-verified here** (§2) |
| SCALED | `mode=2` + `den=9` / `12` / `16` | identical to the row above, all three | `c18496cf…` (all three) | inherited; **re-verified here** (§3, in-gate) |
| no-op | `mode=3`, all denominators | `frame=256x128 use_superres=false denom=8` | `8dafaf38…` | inherited |
| no-op | `mode=4`, all denominators | `frame=256x128 use_superres=false denom=8` | `8dafaf38…` | inherited |
| no-op | `resize-mode=1,2,3`, any denominator | `frame=256x128 use_superres=false denom=8` | `06621606…` | inherited |

**7 of 29 attempted combinations produce a real scaled stream.**

Two facts the table exists to make impossible to get wrong again:

1. **`--superres-mode=1` alone is a no-op.** libaom picks the denominator
   itself (`get_superres_denom_for_qindex`, `av1/encoder/superres_scale.c:143`),
   which returns `SCALE_NUMERATOR` (8) unless the frame is a KF/ARF update AND
   the horizontal-energy test passes. On a smooth `testsrc2` source it returns
   8, and the result is byte-identical to the no-flag control. The chroma-format
   sweep's `sr444` cell is exactly this stream (15239 B, sha `06621606…`), which
   is why a whole sweep cell could sit in the matrix labelled "superres" while
   measuring nothing about superres.
2. **Mode 2 ignores `--superres-denominator` entirely**, and chooses a
   **different denominator per frame**. Three of its four parsed denominators
   (11, 14, 15) are values no flag on the command line ever named. A gate that
   read the flag instead of the header would be wrong on three of four frames
   here — which is why §3 makes the flag-ignoring property an assertion rather
   than a comment.

---

## 2. The two gates: fixtures, recipes, parsed headers

Both recipes are byte-reproducible on this oracle. The parsed headers below
were read from the streams **this lane encoded**, not copied from the previous
run.

### 2a. `a_444_lossy_superres_mode1_den9_stream_decodes_pixel_exact`

`fixtures/444_lossy_superres_256x128_d9.obu`, 17437 bytes,
sha256 `3d619ee0fda3d9bb9cf8ff0541ba00ea5d83ae38cd8eec1322f8dbbbe6a5ec96`,
FNV-1a-64 `0x1ff2_fb84_528f_aa05`.

```text
ffmpeg -f lavfi -i "testsrc2=size=256x128:rate=25" -frames:v 4 \
       -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe - | \
aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 \
       --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
       --limit=4 --superres-mode=1 --superres-denominator=9 \
       --superres-kf-denominator=9 --obu -o - -
```

Parsed, per frame (all four identical):

```text
SEQ ss=(0,0) bd=8 enable_superres=true profile=1
FH frame=228x128 upscaled=256 use_superres=true denom=9     (x4)
```

**Why this member.** 9 is the widest frame-edge margin this 256x128 geometry
allows among the denominators libaom's `SUPERRES_NUM` ladder accepts and still
scales: `228 * 8 / 256` leaves a 28-column upscale margin against den=12's 85
columns, and den=16 (coded 128x128) is degenerate — the coded width equals the
superblock grid exactly. A wide margin is where a round-trip is most likely to
read past the coded picture.

Measured on the gate: 4 upscales, **2568** scaled-MC blocks, 4 decode-order
frames byte-exact vs the instrumented aomdec.

### 2b. `a_444_lossy_superres_mode2_random_denom_stream_decodes_pixel_exact`

`fixtures/444_lossy_superres_mode2_256x128.obu`, 16620 bytes,
sha256 `c18496cf3b7db0feca9aa904024cec2115be80b5b94f87c848ac52f9b32ffd97`,
FNV-1a-64 `0x8cbb_36b2_5e0f_07ab`.

```text
ffmpeg -f lavfi -i "testsrc2=size=256x128:rate=25" -frames:v 4 \
       -pix_fmt yuv444p -strict -1 -f yuv4mpegpipe - | \
aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=20 --cpu-used=2 \
       --threads=1 --row-mt=0 --lag-in-frames=0 --kf-max-dist=100 \
       --limit=4 --superres-mode=2 --obu -o - -
```

Parsed, **per frame — and the four differ**:

```text
SEQ ss=(0,0) bd=8 enable_superres=true profile=1
FH0 frame=186x128 upscaled=256 use_superres=true denom=11
FH1 frame=146x128 upscaled=256 use_superres=true denom=14
FH2 frame=137x128 upscaled=256 use_superres=true denom=15
FH3 frame=228x128 upscaled=256 use_superres=true denom=9
```

**Why this member is not redundant with den=12 / den=9.** Both of those scale
every frame at a single denominator, so `av1_superres_upscale` is only ever
handed one coded-to-upscaled ratio. This stream hands it four (256→186,
256→146, 256→137, 256→228) in one decode, and libaom picks the filter taps
from `denom` per frame. That is a case a single-denominator gate cannot reach.

Measured on the gate: 4 upscales, **2265** scaled-MC blocks, 4 decode-order
frames byte-exact vs aomdec.

### 2c. Shape, and what is asserted from the PARSED header

Both gates call one shared body (`a_444_superres_arm`) — the body of the
existing den=12 gate with the single `(denominator, coded_width)` constant
generalised to a per-frame table, which is what mode 2 requires. Per arm:

- pinned bytes: length + FNV-1a-64;
- from the **sequence header**: `ss (0,0)` (4:4:4), `bit_depth 8`,
  `enable_superres`;
- from **every parsed frame header**: `use_superres` true, the per-frame
  `superres_denom`, `upscaled_width`/height, and the CODED `frame_width`/height;
- `FrameWidth == (UpscaledWidth * 8 + denom / 2) / denom`, re-derived from
  **that frame's own parsed denominator** rather than compared to a literal
  alone — so a re-pinned fixture cannot satisfy the literals while the scaled
  arithmetic moves under them;
- `superres_hits` and `predict_scaled_hits` as **deltas** (both counters are
  process-wide with no reset, so absolute values would pass on residue from
  another stream in a full-suite run);
- every decoded frame at the **UPSCALED** width with full-resolution 4:4:4 U and
  V planes;
- every decode-order frame byte-exact through `decode_all_frames_vs_oracle`.

My FNV-1a-64 implementation was validated against the sibling gate's published
pair before being used on new bytes: `444_lossy_rect4_inter_witness.obu`, 6441
bytes → `0x4368_8b2d_2e91_72b0`. Match.

---

## 3. The flag-ignoring assertion (mode-2 gate, second half)

The pin above is only trustworthy if mode 2 really does ignore the flag. That
is asserted, not assumed. The gate re-encodes the recipe four ways — no
denominator, then `--superres-denominator=9`, `=12`, `=16` — and requires each
to reproduce the pinned 16620 bytes exactly (length + FNV compared to the pin,
not to each other). It then asserts that at least one **parsed** denominator
is a value none of those flags named, so the per-frame table cannot be
satisfied by echoing the command line back.

Green output:

```text
a_444_lossy_superres_mode2_random_denom_stream_decodes_pixel_exact:
  --superres-mode=2 with no/9/12/16 denominator flags all reproduce the pinned
  16620 bytes; parsed denominators [11, 14, 15, 9] (flag values [9, 12, 16] ignored)
```

If a future libaom starts honouring `--superres-denominator` under mode 2, both
the byte compare and that last assertion go red — which is the intended
behaviour: the pin is to the **parsed** denominators, so the day the flag
starts mattering the cell has to be re-derived from the stream, not from the
recipe.

---

## 4. Green, and red-before

**Green** (`--nocapture`, `EC_AV1_REQUIRE_AOMENC=1`, `--test-threads=1`, no
SKIP line anywhere):

```text
a_444_lossy_superres_mode1_den9_stream_decodes_pixel_exact:
  4 decode-order frame(s) (0 hidden) byte-exact vs aomdec at (256, 128) upscaled
  from [(9, 228), (9, 228), (9, 228), (9, 228)] (denominator, coded width) per
  frame, 4 upscale(s), 2568 scaled MC block(s); fixture sha256 3d619ee0…
a_444_lossy_superres_mode2_random_denom_stream_decodes_pixel_exact:
  4 decode-order frame(s) (0 hidden) byte-exact vs aomdec at (256, 128) upscaled
  from [(11, 186), (14, 146), (15, 137), (9, 228)] (denominator, coded width) per
  frame, 4 upscale(s), 2265 scaled MC block(s); fixture sha256 c18496cf…
  --superres-mode=2 with no/9/12/16 denominator flags all reproduce the pinned
  16620 bytes; parsed denominators [11, 14, 15, 9] (flag values [9, 12, 16] ignored)
a_444_lossy_superres_stream_decodes_pixel_exact:            (pre-existing, den=12)
  4 decode-order frame(s) (0 hidden) byte-exact … 4 upscale(s), 2130 scaled MC block(s)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 705 filtered out
```

### Red-before

Committed **before** mutating (`e47d8f60`), so no `git checkout --` could
restore the gate away. Three mutations, each reverted immediately and each
verified reverted (`git diff --stat` empty, tree clean) before the next.

| # | mutation | site | `mode1_den9` | `mode2` | `…_stream_…` (pre-existing) |
|---|---|---|---|---|---|
| 1 | upscale-filter tap alignment: `base = pad - 4` → `pad - 4 + 1` | `superres.rs:133` | **RED, pixel** — `decode-order frame 0 of 4 differs from the oracle at byte 5 (ours 73 vs 74), 33668 bytes differ` | **RED, pixel** — `frame 0 … byte 6 (ours 76 vs 74), 36196 bytes differ` | **RED, pixel** — `frame 0 … byte 5 (ours 76 vs 74), 37726 bytes differ` |
| 2 | `set_superres` handed the **coded** width (`upscaled_width` → `frame_width`) | `stream.rs:1851` | **RED, counter** — `zero predict_scaled_hits -- no inter block read a scaled reference …` | **RED, shape** — `frame 0 came back 186x128, not the UPSCALED 256x128` | **RED, counter** — `zero predict_scaled_hits` |
| 3 | upscale step derived from `out_width`: `upscale_convolve_step(in_width, out_width)` → `(out_width, in_width)` | `superres.rs:106` | **RED, panic** — index out of bounds at `superres.rs:142` | **RED, panic** — index out of bounds at `superres.rs:142` | **RED, panic** — index out of bounds at `superres.rs:142` |

Every mutation run reported `test result: FAILED. 0 passed; 3 failed; 0
ignored; 0 measured; 705 filtered out`. **That is the fake-green signature to
watch for and it did not appear**: a line reading `0 passed … 705 filtered out`
with no `3 failed` is what a `git checkout --`-eaten gate looks like. Here the
filter always selected 3 tests and all 3 went red.

Mutation 1 is the one this lane prefers — a pixel diff is a stronger red than
an index panic, because it names the wrong value rather than only the wrong
address, and it cannot be mistaken for a bounds bug in the mutation itself.
Both gates go red on it independently.

Mutation 3 is the weakest of the three as a gate discriminator (it panics
inside the upscaler before any gate assertion runs) but it is retained because
it is the mutation that proves the per-frame coded width actually feeds the
upscaler's step, rather than being validated at the header assert alone.

---

## 5. Report corrections (diff-shaped)

All in `lanes/av1formatsweep.report.md` — that lane merged, so this is a
follow-up commit editing its file rather than a note in my own report.

| line (pre-edit) | before | after |
|---|---|---|
| 83 | `\| superres \| – \| – \| – \|` (4:4:4 matrix, all depths unmeasured) | `\| superres \| Y (\`a_444_lossy_superres_stream_decodes_pixel_exact\`, plus \`…_mode1_den9_…\` and \`…_mode2_random_denom_…\`) \| – \| – \|` — `Y` at 8-bit only |
| 438 | `\| 4:4:4 **superres** 256x128 (\`--superres-mode=1\`) \| \`sr444\` \| 15239 \| \`06621606…\` \| **DIVERGENT** from f2, 76404 samples, first (f2, s192) = Y(192,0) \|` | Row retitled `(\`--superres-mode=1\`, **no denominator flag**)`. Verdict cell now: **NOT A SUPERRES CELL** — the bytes are the sweep's own (15239 B, sha `06621606…`), all four parsed headers read `use_superres=false denom=8`. The DIVERGENT numbers are **kept verbatim** but re-attributed: **H1, the 1:4 inter-strip own-extent class**, already gated by `a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries` (second arm is these exact 15239 bytes). Real 4:4:4 superres is byte-exact and gated — cross-ref to this report. |
| 442 | `Note the aomenc flag is \`--superres-mode=1\`; there is no \`--enable-superres\` in this build.` | Same fact kept, plus **"but that flag alone does nothing, which is what made the row above a phantom cell"**, followed by a 3-route correction: (1) mode 1 needs an explicit `--superres-denominator`; (2) **mode 2 scales with no denominator flag and ignores `--superres-denominator` entirely** — 9/12/16 all give the same stream — and picks its denominator **per frame** (11, 14, 15, 9 at 256x128), so a single-denominator reading is wrong by construction; (3) modes 3 and 4 are no-ops at every denominator. Cites libaom `v3.13.3-7-g9bb526a` and states how it was established (source tree of `scripts/build-aom-oracle.sh`, **not** `--version`). Ends with "always read the verdict out of the stream's own parsed frame header, never out of the flag", and that `--resize-mode` is a different feature, never a superres witness. |
| 485 | ``Matrix changes (§1): 4:4:4 row gains … `superres → D`, `10/12-bit tiles → D` …`` | `superres → D` **removed** from the list, followed by a correction paragraph: the 4:4:4 row's superres cell reads **`Y` at 8-bit, not `D`**, because this lane's `D` was written from a stream that never scaled; with an explicit denominator or mode 2 the class is byte-exact 4/4 at coded widths 128/171/186/228, gated at denom 12, denom 9 and mode 2. 10/12-bit stays `–`. |
| 492 | `Still not measured after this round: … 4:4:4 superres at 10/12 bits; …` | **Left as written, checked.** A paragraph is added immediately after stating that lane-av1superpin re-checked it and it is **still open**: every fixture this lane pinned parses `bit_depth = 8` from its own sequence header, so nothing here measures 10/12-bit. |

Two further lines in the same file were corrected because leaving them would
have contradicted the five above:

| line (pre-edit) | before | after |
|---|---|---|
| 338 (§4 "Not measured") | `…; 12-bit superres; 4:4:4 superres; 4:4:4 odd coded dimensions; …` | `…; 12-bit superres; 4:4:4 superres **(closed since, at 8-bit only — lane-av1superpin, see the correction in §6.5; 10/12-bit still open)**; …` — the list is stated so nobody reads silence as coverage, so a stale entry there is itself a defect |
| 504 (§6.6 handoff) | `The 4:4:4 **130x122** odd-dimension divergence and the 4:4:4 **superres** divergence are unassigned; neither reduces to a known class on the evidence here, and neither has a discriminator run.` | Both are **closed by lane-av1444edge**: one class (H1, the 1:4 inter-strip own-chroma-extent gather at `ss (0,0)`), discriminator-run and fixed by `f92776ba` (lane-av1444rect), gated by `a_444_lossy_rect4_strip_stream_decodes_pixel_exact_at_odd_and_wide_geometries`. Adds that the "superres" name in the row was a mislabel — the stream never scaled — and that real 4:4:4 superres coverage is separate and lives here. |

---

## 6. Scope, and what this lane did not do

- **No `decode.rs` edit.** This lane is gates plus one markdown file; the six
  other lanes' regions of `decode.rs` are untouched. `git diff 4c973b94..HEAD`
  touches only `crates/ec-av1/src/stream.rs` (additive, two new tests and one
  shared helper), two new fixtures, and `lanes/`.
- **The pre-existing `a_444_lossy_superres_stream_decodes_pixel_exact` (den=12)
  is unchanged** and was only used as the shape to copy. All three superres
  gates went red under all three mutations and green after revert, which is
  also how I know my two new gates are not duplicating an existing claim in a
  way that only one of them would notice.
- **10/12-bit 4:4:4 superres remains unmeasured** (§5, line 492). Encoding it
  needs a 12-bit 4:4:4 source smooth enough to avoid the screen-tools refusal
  (H6) and a denominator that survives it — a separate lane.
- **den=16 (coded 128x128) remains ungated.** It is measured byte-exact
  (inherited) but degenerate — coded width equals the SB grid — so it is the
  weakest of the four as an exactness claim. Recorded, not dropped.
- No suite run beyond the three named gates. Full validation is Main's, once,
  after all subagents land.

## 7. Handed on

- **Main** — the five line-level corrections in §5 plus the two consistency
  fixes; `e47d8f60` is additive and merges independently of any decode-path
  work. Merge-order note: this branch is cut from `4c973b94`, which does **not**
  contain the `lanes/av1formatsweep.report.md` that landed on `main` via
  `f566862a`, so the report file was taken from `main` into this branch and
  edited here. Expect a content conflict on that one file at merge time; the
  intent is that this branch's version wins for the corrected lines.
