# lane-av1-decode (pilot resume) — the parked first wall is already closed; today's first stop is monochrome

Base: `main` **79984549** (2026-09-19). Worktree `edith_codecs-av1dec`, branch
`lane-av1-decode`, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1dec` (private).
Gate: `cargo test -p ec-av1` (lib). No push.

## TL;DR

The charter's first wall — **64x64 superblock-level partition arms beyond
NONE/SPLIT in the intra tile path** — is **ALREADY CLOSED on `main`**. It was
built by lanes `sbpart`/`part32`/`sb128*`/`t900` in the three weeks the parked
numbers describe, and it is not only un-refused: it is **witness-gated by seven
real-aomenc pixel-exact gates**, and both user films decode **byte-exact** past
every offset where the parked lane stopped.

So the honest pilot result is a **re-measurement that redirects the resume**, not
a code un-refusal of an arm that no longer exists. This report gives:

1. the baseline re-measurement (parked vs today, both films),
2. decisive proof the named wall is down,
3. the walls that a real stream **does** stop on TODAY, root-caused,
4. the ordered terrain map for the next wave, and
5. one committed regression gate pinning today's measured first stop.

**Distance advanced (parked → today):** `main` today decodes **100% of both films
byte-exact at every probed offset**; at the parked tip (`3808cf8`, 2026-08-30)
both films stopped early (Troy at a superblock-level HORZ/VERT strip with a split
transform; Hunger Games at a superblock-level partition). The refusal inventory
shrank from **47** (41 REFUSALS + 6 CAPABILITY_CLAIMS) to **37**
(36 REFUSALS + 1 CAPABILITY_CLAIM). The refusal-scan parser reproduces the parked
**47** exactly, so the earlier "35 (34 + 1)" figure was a miscount.

## 1. Baseline re-measurement (main 79984549)

Harness: `decode_probe` (release) over OBU windows extracted with
`ffmpeg -ss <s> -t <d> -i <film> -c:v copy -an -f obu`, run under
`systemd-run --user --scope -p MemoryMax=8G`. Byte-exactness compares the probe's
`EC_PROBE_OUT16` stream against `ffmpeg -pix_fmt yuv420p10le -f rawvideo` over the
same OBU (`sha256` + per-frame numpy diff). Film paths come from the environment
only; never committed.

### Film A — 1920x792 10-bit, 128x128 superblocks (the parked lane's "Troy")

Refusal scan: **21 windows** (ss 300, 900, …, 12300; 2 s each) → **every one
`OK`, 0 refusals** (parked tip: stopped at the SB-level HORZ/VERT split
transform).

Byte-exact windows (probe `EC_PROBE_OUT16` sha256 vs ffmpeg `yuv420p10le`):

| ss | frames | probe hw | ffmpeg hw | differing |
|---|---|---|---|---|
| 0 | 72 | c0b84add535d4016 | c0b84add535d4016 | 0 |
| 2700 | 290 | b788fdf9a0429a68 | b788fdf9a0429a68 | 0 |
| 5400 | 273 | 19ea55ea5cde528e | 19ea55ea5cde528e | 0 |
| 8100 | 95 | c6e9aeead5fbc701 | c6e9aeead5fbc701 | 0 |
| 9000 | 261 | e7859bba900fb35e | e7859bba900fb35e | 0 |
| 9900 | 175 | c852aaa1cd2ab28f | c852aaa1cd2ab28f | 0 |
| 11700 | 169 | 8d1a6aaa64d2fd2f | 8d1a6aaa64d2fd2f | 0 |

(ss 8100 and 9000 refused outright on the parked lane's descendant `lane-t900` —
they are byte-exact here.)

### Film B — 3840x1608 10-bit HDR (the parked lane's "Hunger Games")

Refusal scan: **30 windows** (ss 300, 900, …, 17700; 2 s each) → **every one
`OK`, 0 refusals**.

Byte-exact windows (13 offsets: 300, 900, 1500, 2100, 2700, 3300, 3900, 4500,
5100, 5700, 6300, 6900, 7500) — **sha256 identical, 0 differing frames each**,
e.g. ss 300 = `79fdd1eb4085bbde` (136 fr), ss 2700 = `773ffa148582d9df` (104 fr),
ss 5700 = `1faf056f15d159fb` (247 fr).

### Fixture corpus (9 `fixtures/bitstreams/av1-*.ivf` → OBU via ffmpeg)

| fixture | today |
|---|---|
| av1-1080p-23.976-10bit / -8bit | OK 48 fr |
| av1-1080p-60-10bit / -8bit | OK 120 fr |
| av1-2160p-23.976-8bit | OK 48 fr |
| av1-altref | OK 60 fr |
| av1-profile1-444 | OK 60 fr |
| av1-tiles-1280 | OK 30 fr |
| **av1-monochrome** | **REFUSED** (see §3) |

## 2. The named wall is down (proof)

**Code.** `decode_key_frame_tile_with_cdfs` (`crates/ec-av1/src/decode.rs`,
the SB-level `match part` at ~:23978/:24050) has an arm for **all ten**
`partition_w64` values: NONE, SPLIT, HORZ, VERT, HORZ_A|HORZ_B|VERT_A|VERT_B
(`decode.rs:23978`), HORZ_4|VERT_4 (`decode.rs:24073`). The `_ =>` fallback at
`decode.rs:24113` (the string *"a superblock-level partition value outside
PARTITION_NONE..PARTITION_VERT_4"*) is therefore unreachable — pinned by
`every_partition_value_of_an_enumerated_alphabet_has_an_arm`. The inter tile
path carries the same arms (SB-level rect `decode.rs:4709`, AB, 1:4).

**Witness gates** (real aomenc streams, pixel-exact, in `stream.rs`):

| gate | line |
|---|---|
| `a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact` | 28356 |
| `a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_and_delta_q_decodes_pixel_exact` | 30569 |
| `a_real_aomenc_stream_with_a_superblock_level_ab_partition_decodes_pixel_exact` | 30341 |
| `a_real_aomenc_stream_with_a_superblock_level_1to4_partition_decodes_pixel_exact` | 30800 |
| `a_real_aomenc_inter_sequence_with_a_superblock_level_rect_partition_decodes_pixel_exact` | 12111 |
| `a_real_aomenc_sb128_gathered_edge_horz_partition_decodes_pixel_exact` | 27435 |
| `a_real_aomenc_sb128_ab_partition_at_the_128_root_decodes_pixel_exact` | 27803 |

**Independent re-derivation (this lane).** The aomenc recipe the parked lane
recorded (`--sb-size=64 --min-partition-size=32 --max-partition-size=64
--enable-rect-partitions=1 --enable-ab-partitions=0 --enable-1to4-partitions=0`
on a `gradients`+`testsrc2` blend) reproduces `TRACE partition_w64` at the SB
root with values **2 (VERT), 3 (SPLIT), 4 (HORZ_A), 7 (VERT_B)**; the stream
decodes 16/16 frames and the probe's low byte equals `aomdec --rawvideo`
**byte-identical** (`cmp` IDENTICAL, 589824 B). Repro:

```
ffmpeg -f lavfi -i gradients=size=192x128:rate=4:duration=4 \
       -f lavfi -i testsrc2=size=192x128:rate=4:duration=4 \
       -filter_complex "[0:v][1:v]blend=all_mode=overlay" -pix_fmt yuv420p -f yuv4mpegpipe in.y4m
aomenc --codec=av1 --passes=1 --end-usage=q --cq-level=45 --cpu-used=0 --threads=1 --row-mt=0 \
       --sb-size=64 --enable-rect-partitions=1 --enable-ab-partitions=0 --enable-1to4-partitions=0 \
       --min-partition-size=32 --max-partition-size=64 --enable-restoration=0 --enable-palette=0 \
       --deltaq-mode=0 --enable-filter-intra=0 --enable-cfl-intra=0 --enable-intrabc=0 --obu -o sb64.obu in.y4m
EC_AV1_TRACE=1 decode_probe sb64.obu     # TRACE partition_w64 ... value=2/3/4/7
```

**Films.** Both films decode byte-exact at every probed offset with the SB-level
arms firing — the strongest possible "un-refused with byte-exact proof".

Conclusion: there is **nothing to un-refuse** here and no honest "distance" to
add — the resume must redirect.

## 3. Walls a real stream stops on TODAY (root-caused)

Beyond the fixture corpus I swept a broad aomenc/ffmpeg-libaom recipe battery
(`~/.cache/av1dec-recon/recipes.sh`, 22 recipes: sb64/128, 4:4:4, 4:2:2,
lossless, screen, max-partition 4/16, tool toggles, aq/deltaq, cdef/lr off,
intrabc/palette/cfl off, warped, all-intra). Every 4:2:0 8/10-bit film-like
recipe decodes. Four real streams stop:

### 3.1 Monochrome key frame → `a Golomb tail longer than this decoder reads`

`ffmpeg -f lavfi -i testsrc2=size=320x240:rate=30:duration=2 -pix_fmt gray
-c:v libaom-av1 -cpu-used 8 -b:v 300k -f obu` → REFUSED. The **4:2:0 twin of the
same source+recipe decodes 60/60 frames**, so this is monochrome, not the recipe.
(The committed io fixture `av1-monochrome.ivf` is a **different, earlier** stop:
it refuses at `intra block copy on a HORZ/VERT/1:4 rect intra strip` — the
intrabc-rect wall — before reaching this desync. The pinning gate in §5
therefore generates its stream from this ffmpeg recipe, not from that fixture.)

**Root cause (localised).** Run the instrumented aomdec oracle and our probe with
`EC_TRACE_MODE_STEP=1` over the same OBU. On the **mono** stream **ours reads
`angle_uv` at `rng=50996`**, while aomdec reads **no** `uv_mode`/`angle_uv` at
all (the guard below): the monochrome frame codes neither symbol. The first
divergence at `(0,0)` is therefore aomdec reading `tx_depth ctx=0 cat=1`
(`rng=43616`) where **ours reads a `uv_mode` (val=13)** and desyncs. (The
`rng=56072` "agree through `uv_mode`/`angle_uv`" agreement belongs to the 4:2:0
twin, whose chroma symbols both decoders read; `56072` never appears in the mono
trace.) libaom `av1/decoder/decodemv.c:933`:
`if (!cm->seq_params->monochrome && xd->is_chroma_ref) { mbmi->uv_mode = ... }` —
a monochrome frame codes **no** `uv_mode`/`cfl`/`angle_delta_uv`/`palette_uv`.
This decoder's block layer has **no `num_planes`/monochrome notion at all**
(`grep -n num_planes crates/ec-av1/src/decode.rs` finds only comments; the encoder
`frame.rs` has the guards, the decoder does not), so it reads those symbols for
every block and desyncs. The `Golomb` refusal is the **symptom** of being left
mid-symbol, not the defect.

**Disposition: `deferred(needs a monochrome/num_planes threading lane)`.** The
fix is a feature, not a one-liner: thread `num_planes` into `FrameCtx`, gate the
chroma **symbol** reads (`decode.rs:9153-9212` and the rect/sub-8/`_in_inter`
twins), skip the chroma **coefficient** reads (≈12 `read_plane` u/v call sites)
and chroma reconstruction, and emit a 1-plane picture. Gated today by the
committed `a_real_libaom_monochrome_key_frame_is_refused_by_name` (flip it to a
witness when this lands).

### 3.2 4:4:4 (profile High) at high speed → same `Golomb` string

`-pix_fmt yuv444p -c:v libaom-av1`: **ffmpeg-libaom cpu-used 8 refuses, 5/6/7
decode; aomenc cpu-used 6/7 refuse, 4/5/8 decode** — non-monotonic, so it is a
coding-tool decision, not "4:4:4 is unsupported" (the fixture
`av1-profile1-444.ivf` decodes fine). Localised first divergence at block
`(0,0)` of the key frame: aomdec reads `tx_depth ctx=0 cat=1`, ours reads
`txfm_split ctx=18` (`read_var_tx_size`, `decode.rs:17814`) — our tx-size read
routes this block down the wrong path.

**Disposition: `deferred`.** Not gate-able stably (the offending recipe depends on
the encoder build/cpu-used).

### 3.3 `--allintra` and 3.4 `--enable-warped-motion=1 --cpu-used=0`
### → `a sub-8x8 leaf that uses intrabc (... no block-vector path ...)`

Both stop on the sub-8 intrabc refusal. First divergence (allintra, block
`(4,0)`): aomdec reads `tx_depth`, ours reads `intrabc`. The refusal is the
symptom of the same class as 3.2 — a block-routing desync, not an encoder that
truly wrote sub-8 intrabc.

**Disposition: `deferred`.**

### Inventory consequence

Two refusals carry **PROVEN** entries claiming real streams never reach them
(by census/enumeration): *"a Golomb tail longer than this decoder reads"* and
*"a sub-8x8 leaf that uses intrabc"*. §3.1-3.4 **refute** those proofs: real
libaom streams reach both. The census domain was too narrow (never covered
monochrome / 4:4:4-fast / all-intra / warped-at-cpu0). They remain **inventory
debt** until the root causes above land; the PROVEN tests must then be revisited.
`each is a reachable refusal whose "unreachable" proof is now known false`.

## 4. Terrain map for the next wave

Remaining refusals: **36 REFUSALS + 1 CAPABILITY_CLAIM** (37 total). Ordered by dependency
(what unblocks what):

**Wave 1 — the only real-stream-reachable cluster (this lane's finding):**
1. **Monochrome / `num_planes` threading** (3.1). Blocks: av1-monochrome fixture,
   the `-pix_fmt gray` family. Touches the chroma-symbol gate sites
   (`decode.rs:9153,9161,9166,9169,9205` + rect/sub8 twins) and every u/v
   `read_plane` site. **Unblocks nothing else directly**, but is the cleanest
   feature to build first (needed for a "all AV1 profiles" claim).
2. **Block-routing / tx-size desync** (3.2, 3.3, 3.4): all-intra, 4:4:4-fast,
   warped-cpu0. Localised to the tx-size read (`decode.rs:17814`) choosing the
   inter var-tx reader where libaom reads `tx_depth`. One root cause may cover all
   three.

**Wave 2 — census/enumeration-proven, no film/stream hits them yet**
(do NOT spend a lane until a witness stream exists; each has a PROVEN test):
- `an intra-coded {bw}x{bh} block on the inter block path ...` (decode.rs backtick
  unlocated; census `every_intra_in_inter_shape_the_census_lists_has_a_size_group_row`)
- `a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding` (decode.rs:26910)
- `a coded HORZ/VERT strip whose chroma transform has no rect coefficient tables here` (decode.rs:9531)
- `a split intra strip whose transform unit is {tx_w}x{tx_h} ...` (decode.rs:9574)
- the rect-transform-table/scan trio (decode.rs:18160/18187/18240)
- `a 32x32 partition type this decoder does not code` (decode.rs:23885) and the
  INTER twin (decode.rs:37355); `an inter 16x16-level partition value outside
  NONE/HORZ/VERT/SPLIT/AB/1:4` (decode.rs:36280)
- `a 128x128 superblock partition value outside the 8-symbol alphabet` (decode.rs:22144)
- `an inter var-tx tree with a leaf transform larger than 32x32/64x64` (decode.rs:17955/18402)
- `CfL, filter intra or a palette on a 128-root HORZ/VERT intra block` (decode.rs:14993)
- `an intra mode this decoder does not code (round 2)` (decode.rs:10442/29717)
- `a motion_mode symbol for a block shape with no CDF row here` (decode.rs:28572)
- `an OBMC neighbour whose switchable interp filter was never recorded` (decode.rs:25649)
- `an intra 16x4/4x16 strip inside an inter 16x16-level 1:4 partition` (decode.rs:29646)
- `an intrabc block whose var-tx tree resolved to mixed leaf transform sizes` (decode.rs:14287)
- `intra block copy on a HORZ/VERT/1:4 rect intra strip` (decode.rs:9122)
- `a sub-8x8 leaf that uses intrabc` (decode.rs:15959) — see §3.3, proof refuted
- `a reference frame selected with no picture ...` / `a reference picture whose height does not match ...`
- `a frame with no mode-info grid`; `a frame naming primary_ref_frame at an empty slot`
- `a frame whose segmentation enables SEG_LVL_REF_FRAME/SKIP/GLOBALMV`
- `a frame mixing lossless and lossy segments` (the only refusal with NO proving test)

**Wave 3 — capability claims (not refusals):**
- `filter intra on a superblock-level HORZ/VERT strip (... av1_filter_intra_allowed_bsize caps at 32x32)`

**Do not re-charter:** the 64x64 SB-level partition arms (this report, §2) and
anything already covered by the seven witness gates above.

## 5. Gates / regression coverage added

- **`a_real_libaom_monochrome_key_frame_is_refused_by_name`** (`stream.rs`):
  generates the monochrome stream and its 4:2:0 twin with ffmpeg-libaom, asserts
  the twin decodes and the monochrome stream stops on the exact measured refusal
  string. It is a tripwire for today's first real-stream stop: it fails if the
  refusal moves (re-measure) and must be flipped to a pixel-exact witness when
  monochrome decode lands.

## 6. Gates run / suite

- `cargo test -p ec-av1 --lib a_real_libaom_monochrome_key_frame_is_refused_by_name -- --nocapture`
  → `ok. 1 passed` (`4:2:0 twin 60 frames, monochrome refused by name`).
- `cargo check -p ec-av1 --all-targets` → rc 0, **0 warnings** (parity with
  79984549).
- Full lib suite (completed on the lane by the verifier): **592 passed, 0 failed,
  60 ignored** (6716 s). (An earlier in-lane run was cut off by a budget cap while
  showing "652 passed"; that partial figure is not a completed suite.)

## 7. Repro commands (all re-derivable)

```
# build the probe
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1dec cargo build --release -p ec-av1 --example decode_probe

# film refusal scan (paths via env only)
ffmpeg -loglevel error -ss <s> -t 2 -i "$FILM" -c:v copy -an -f obu /tmp/w.obu
EC_PROBE_OUT16=$HOME/w.raw decode_probe /tmp/w.obu        # OK: N frames | REFUSED: <string>
ffmpeg -loglevel error -i /tmp/w.obu -pix_fmt yuv420p10le -f rawvideo $HOME/ref.raw
sha256sum $HOME/w.raw $HOME/ref.raw                       # byte-exactness

# first-divergence between aomdec and our decoder on any OBU
EC_TRACE_MODE_STEP=1 ~/.cache/aom-oracle/build/aomdec --rawvideo -o /dev/null x.obu 2>a.step
EC_TRACE_MODE_STEP=1 decode_probe x.obu 2>o.step; diff a.step o.step
```

(Scripts kept at `~/.cache/av1dec-recon/`: `sweep.sh`, `pixcmp.sh`, `scan.sh`,
`recipes.sh`, `diverge.sh`.)

## 8. Git log

- `lane-av1-decode` off `79984549`: report + `a_real_libaom_monochrome_key_frame_is_refused_by_name`.
