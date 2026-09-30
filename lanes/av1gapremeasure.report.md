# lane-av1gapremeasure — the debt file's remaining ec-av1 gap list, re-measured

Base `main` **facdcb73** ("ec-av1: bind the 10-bit film gate's pin names as literals").
Worktree `~/.cache/wt/av1gapremeasure`, branch `lane-av1gapremeasure`. Report only:
no source change is committed (the measurement harness was a throwaway test
appended to `crates/ec-av1/src/stream.rs` and reverted with `git checkout --`
afterwards; `git status --porcelain` is empty and both behaviour gates
`a_12bit_screen_content_stream_is_refused_by_name` and
`the_counting_oracle_diff_detects_one_flipped_oracle_byte` were re-run green on
the restored tree).

## 0. Instrument and the control that makes every zero mean something

All pixel numbers come from the crate's own
`count_rawvideo_diffs(stream, name, flip)` (`crates/ec-av1/src/stream.rs:10374`),
which runs the instrumented oracle `~/.cache/aom-oracle/build/aomdec --codec=av1
--rawvideo`, reads its output bytes, packs OUR decoded pictures at the stream's
own bit depth, and returns `(wrongY, wrongU, wrongV, frames, frames_exact)`.
`flip: Some(i)` XORs one bit of the ORACLE's bytes before comparing.

**The control, run live in every measurement below:** flipping oracle byte 0 of
`av112bit-key.obu` moves `wrongY` 0 → **1** and `frames_exact` 1 → **0**
(flip byte 40000 likewise: `wrongY=1`, `exact=0`; both bytes are luma because the
witness is 160x128 at 12-bit, i.e. 40 960 luma bytes per frame). For the
odd-height cells the same flip on the very stream being reported moves
`wrongY 0 → 1` and `exact 16 → 15`. A zero that cannot move by +1 when the
oracle's own bytes change is not a measurement, and none of these are.

## 1. The table

| item | recorded claim (source) | measured on `facdcb73` | class | evidence |
|---|---|---|---|---|
| **12-bit AV1 — decode support** | "`12-bit AV1`" left on the unchanged list; the blanket refusal lifted by `lanes/av112bit{,c,g,w}.report.md`, "the witnessed paths (intra stack, dequant/transforms at the 12-bit tables, deblock, CDEF, Wiener+SGR restoration, translational subpel MC …) are lifted" | 7 committed 12-bit pins, **27 frame-dumps, 0 wrong on Y/U/V, every frame exact** | **CLOSED** | `av112bit-key.obu` 923 B 0/0/0 1/1 · `-inter` 4533 B 0/0/0 2/2 · `-compound` 7486 B 0/0/0 6/6 · `-compound-masked` 7470 B 0/0/0 6/6 · `444_lossy_superres_256x128_d12_12bit.obu` 11023 B 0/0/0 4/4 · `…_d9_12bit.obu` 13015 B 0/0/0 4/4 · `…_mode2_256x128_12bit.obu` 14656 B 0/0/0 4/4. Control: flip byte 0 → wrongY 1, exact 0 |
| **12-bit screen-content refusal** (the ONE surviving 12-bit refusal) | `refusal_inventory.rs:113` "a 12-bit frame with screen content tools (allow_screen_content_tools=1: neither palette nor intrabc has a 12-bit witness)"; gate `a_12bit_screen_content_stream_is_refused_by_name` | Refusal fires on 3 live aomenc 12-bit encodes whose headers are `[screen=true intrabc=false]`. **With the refusal lifted (local probe, reverted): all 6 12-bit arms decode byte-exact vs aomdec — 0/0/0, 2/2 frames each** (testsrc2 + smptebars × screen-only / palette+intrabc / lossless+palette+intrabc), each with flip control +1 / exact−1. No arm ever set `allow_intrabc`; the 8-bit controls with the same recipes also set `allow_intrabc=false` | **OPEN, but the guard is BROADER than its own string** | see §2.1 |
| **12-bit palette / intrabc** | "neither palette nor intrabc has a 12-bit witness" | Not producible here: 6 recipes × 2 sources, `--enable-palette=1 --enable-intrabc=1` and `--lossless=1` included, produce `allow_intrabc=false` at 12-bit AND at 8-bit | **UNREACHABLE with this aomenc** | what is missing: an encoder that selects the tools (the recipe, not the depth, is what failed — the 8-bit control is the proof) |
| **SEG_LVL gaps** (`SEG_LVL_REF_FRAME`/`SKIP`/`GLOBALMV`) | `lanes/av1seglvl.report.md:16-19`: "every frame header parsed: aomenc enables `SEG_LVL_ALT_Q` and nothing else. 162 seg-enabled frames … zero instances of any other `SEG_LVL` feature. The refusal **stays, by name, unchanged**" | Census gate `a_segmentation_census_over_real_aq_streams_finds_no_mode_overriding_feature` **green on main, reproducing the recorded numbers digit for digit**: 4 lag/two-pass arms → 162 seg-enabled frames, 134 inherited-map, 38 with ALT_Q, per-feature `[96,0,0,0,0,0,0,0]` / `[32,…]` / `[96,…]` / `[80,…]`. Refusal gate green: 3 features × segments 0/3/7 all refuse, 0 pictures | **UNREACHABLE (guarded) — the record is CORRECT** | control: the same census run on a hand-built header enabling features 5/6/7 reports `per_feature[5]=1`, `[6]=1`, `[7]=1` (and `alt_q=0`) — the instrument reads non-ALT_Q bits, so the corpus zeros are a measurement |
| **rect residual tables** ("census film-unreachable, no witness") | `lanes/av1chromarect.report.md:67-68` named `ChromaRect32x64`/`ChromaRect64x32` missing, then **refuted itself** at `:69` ("That conclusion is wrong, and the wrongness is the finding") — libaom never forms a 64-point chroma transform; the missing thing was the WALK. The witness recorded as REFUSING (`fixtures/r512.obu`, 6948 B) is the one the debt calls unwitnessed | **`r512.obu` DECODES 3/3 frames byte-exact vs aomdec, 0 wrong on Y/U/V.** `r512_rect1x2_444_palette.obu` 2250 B 1/1 · `r512_rect2x1_1x2_444.obu` 2697 B 1/1 · `420_intrabc_rect4_witness.obu` 890 B 1/1 · `444_intrabc_rect4_witness.obu` 1016 B 1/1 · `444_lossy_rect4_inter_witness.obu` 6441 B 2/2 — 9 frame-dumps, 0 wrong | **CLOSED** (and the premise was refuted in the record itself) | control: same `flip` mechanism, +1 in Y |
| **odd-height 4:2:0 cells** (debt's own "STILL OPEN" row) | "`320x236` (Y 234349, U 56957, V 52981) and `322x248` (Y 36286, U 13359, V 13001)" — recipe `aomenc --profile=0 --cq-level=45 --cpu-used=0 --limit=16` of testsrc2 | Measured with **that exact recipe**: `320x236` **0/0/0, 16/16 exact** · `322x248` **0/0/0, 16/16 exact** · `320x232` 0/0/0 16/16 · `320x248` 0/0/0 16/16 · `324x236` 0/0/0 16/16 | **CLOSED** | per-cell control: flip oracle byte 0 → `wrongY=1`, `exact=15` on the same stream |
| **4:4:4 SQUARE 64x64 sibling-DC** (debt line 21) | "the SECOND unit reconstructs its SIBLING's flat DC instead of its own … seed: f0 r512.obu mi=(112,80) px=(320,448) side=64 … reconstructs flat 241 (the oracle has 202)" | The seed stream itself measures **0 wrong on all three planes across all 3 frames** vs aomdec — a flat-241-vs-202 signature at one unit would be at least 1 wrong sample | **CLOSED** | same `r512.obu` dump as the row above |
| `cfl_ac_q3_at` (debt line 19) | recorded REFUTED and guarded by two gates on main | not re-run this round | closed **by record** (see `not_done`) | — |
| 4:2:2 anomaly on 322x242 + the refusal-lift decision (debt line 8) | "ONE NAMED ANOMALY, unattributed … being settled on lane-av1422anom" | not measured (owned by that lane). My fresh 322x242 encode is 0/0/0 vs aomdec on 4 frames, but that is PRESENTED output and does not touch the rung-vs-returned-picture question | **not measured** | see `not_done` |
| encoder-suite cost (debt line 15) | ~3h10m of a debug suite | not an ec-av1 correctness row; out of scope | untouched | — |

## 2. Charter-ready text for what is still open

### 2.1 The 12-bit screen-content refusal is wider than the gap it names — OPEN

**Witness:** real `aomenc --bit-depth=12 --profile=2` output over
`testsrc2=s=128x128:r=25` and `smptebars=s=128x128:r=25`, recipes
`--tune-content=screen` (273 B and 3828 B) and
`--tune-content=screen --lossless=1 --enable-palette=1 --enable-intrabc=1`
(262 B and 11 500 B) — four to six committed pins, all with parsed
`allow_screen_content_tools = true`, `allow_intrabc = false`.
**Measurement:** `decode_all_frames_vs_oracle` / `count_rawvideo_diffs` against
the instrumented aomdec. On `facdcb73` all six arms are **refused by name**; with
the single `if bit_depth == 12 && header.allow_screen_content_tools` arm
(`stream.rs:1842`) deleted and nothing else changed, the same six arms are
**0 wrong Y / 0 wrong U / 0 wrong V, 2/2 frames exact each**, with the +1 flip
control on every arm. The 8-bit control arms prove the recipe is the reason no
screen tool is selected: at 8 bits the same recipes also report
`allow_intrabc = false`, so the 12-bit absence is an encoder-side selection
question, not a depth question.
**Unblock / decision:** either (a) narrow the refusal to fire only when a
palette or intrabc symbol is actually READ (the flag alone is measurably safe),
keeping the string for the genuinely unwitnessed cell, or (b) keep it as is and
amend the string, which currently claims a gap ("neither palette nor intrabc has
a 12-bit witness") that the flag-only cell does not have. Producing a real 12-bit
palette/intrabc stream is a separate unblock and needs an encoder that selects
those tools — this `aomenc` does not, at either depth, for the six recipes
tried.

## 3. Verbatim corrections to the debt text

Debt file `~/.omp/agent/debts/DEBTS-home-tahinli-Documents-Code-Rust-edith_codecs.md`.

**(a) line 8, the unchanged-list sentence.** Current text:

> `Rest of the recorded list unchanged: rect residual tables (census film-unreachable, no witness); 12-bit AV1; SEG_LVL gaps; two pre-existing 10-bit film failures `

Replace the three named items with:

> `Re-measured 2026-09-30 on main facdcb73 (lane-av1gapremeasure): 12-bit AV1 is CLOSED for decode -- 7 committed 12-bit pins, 27 frame-dumps, 0 wrong samples on any plane vs the instrumented aomdec (control: one flipped oracle byte moves the count by exactly +1); the only surviving 12-bit refusal (allow_screen_content_tools) is BROADER than its gap -- lifted locally, 6 real aomenc 12-bit arms decode byte-exact 0/0/0 2/2, so the flag-only cell is safe and only a real 12-bit palette/intrabc stream is unwitnessed (this aomenc emits none, at 12 or 8 bits). rect residual tables: CLOSED and the premise refuted in the record itself -- r512.obu (6948 B, the stream the debt calls unwitnessed) decodes 3/3 frames byte-exact vs aomdec 0/0/0 on Y/U/V, plus 5 sibling rect pins 6/6. SEG_LVL gaps: the record is CORRECT -- the census reproduces exactly (162 seg-enabled frames over 4 arms, per-feature [96,0,0,0,0,0,0,0]/[32,..]/[96,..]/[80,..], all non-ALT_Q features 0) and the REF_FRAME/SKIP/GLOBALMV refusal fires on every hand-built header; census control: the same instrument reports per_feature[5]=1/[6]=1/[7]=1 on headers that enable them. `

**(b) line 8, the odd-height "STILL OPEN" row.** Current text:

> `STILL OPEN, separate mechanism (LUMA errors, unlocalised): 320x236 (Y 234349, U 56957, V 52981) and 322x248 (Y 36286, U 13359, V 13001). `

Replace with:

> `CLOSED 2026-09-30 (lane-av1gapremeasure, measured on main facdcb73 with the recorded recipe aomenc --profile=0 --cq-level=45 --cpu-used=0 --limit=16 of testsrc2, live aomdec comparator): 320x236 is 0/0/0 with 16/16 frames exact and 322x248 is 0/0/0 with 16/16 frames exact; 320x232 / 320x248 / 324x236 also 0/0/0 16/16. Per-cell non-vacuity control: flipping one byte of the real oracle output moves the count by exactly +1 in Y and exact frames 16 -> 15. `

**(c) line 21, the 4:4:4 SQUARE 64x64 row.** Its opening claim is:

> `4:4:4 SQUARE 64x64 block whose 64x64 chroma plane block is walked as a 2x2 grid of Chroma32 units: the SECOND unit reconstructs its SIBLING's flat DC instead of its own.`

Its own seed (`f0 r512.obu mi=(112,80)`) measures 0 wrong on all three planes over
all 3 frames on current main, so the row's claim and its numbers (flat 241 vs
oracle 202) are stale. Prefix the row with:

> `STALE 2026-09-30 (lane-av1gapremeasure): the seed stream r512.obu is byte-exact against the instrumented aomdec on current main -- 0 wrong Y/U/V over all 3 frames (control: flipped oracle byte -> +1 in Y). The flat-241-vs-202 signature at mi=(112,80) cannot be present. `

## 4. `not_done`

- The 4:2:2 items on debt line 8 (the 322x242 `EC_AV1_FINAL_DUMP`-vs-returned-picture
  anomaly and the sequence-header refusal lift decision) were NOT measured; they
  belong to `lane-av1422anom`. My 322x242 encode being 0/0/0 vs aomdec is
  presented-output only and does not address the rung disagreement.
- `cfl_ac_q3_at` (debt line 19) was not re-run; it is recorded refuted with two
  guards already on main, and the debt's own text says so.
- No 12-bit palette/intrabc stream was produced; 6 recipes × 2 sources were
  tried at 12-bit and the same recipes at 8-bit as a control. No encoder capable
  of selecting those tools at 12-bit is present on this box.
- No 12-bit AFFINE global motion witness (recorded open in
  `lanes/av112bitw.report.md:123-128` and not named in the debt list) was
  produced; `av1gwarp12` covers ROTZOOM at 12-bit.
- Nothing was left in the tree: the throwaway harness and the temporary refusal
  lift were reverted; `git status --porcelain` is empty at this commit.
