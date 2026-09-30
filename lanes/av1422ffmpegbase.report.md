# lane-av1422ffmpegbase — the 4:2:2 measurement BASE, re-taken against ffmpeg, and the lift checklist

**Status: measurement only.** No decoder source was edited, and the 4:2:2 sequence-header
refusal is still in the tree at `crates/ec-av1/src/stream.rs:1803`. Tree measured: `main` =
`affe70dc`. Oracle: **ffmpeg 8.1.3** (`/usr/bin/ffmpeg`, `libdav1d`/`libaom-av1`),
`/home/tahinli/.cache/tgt/probe422base/release/examples/dump_yuv` on our side.

**Headline: the ffmpeg-oracle census and the aomdec-oracle census AGREE on every one of
the 51 4:2:2 cells — 44 byte-exact, 7 diverging, the same 7 cells — and they disagree on
exactly one cell in the whole 95-cell set, `s444_352x242_10b`, where the aomdec row is a
stale-tree artefact, not an oracle disagreement.** The per-cell wrong counts differ on all
7 diverging cells, and that difference is *exactly* the hidden alt-ref frame the ffmpeg
path cannot see (§3.2), which makes the two oracles a cross-check on each other rather
than two opinions.

The census of record for the lift decision is `lanes/av1422census2.report.md` §3.2
(44 exact / 7 diverging). This report re-measures the same 95 cells, same tree, with the
other oracle, and confirms that number.

---

## 0. The instrument, and its one non-obvious choice

`/home/tahinli/.cache/census422b/ffcensus.py` (driver + comparator),
`/home/tahinli/.cache/census422b/ffliveness.py` (liveness control).

**Pairing basis: DISPLAY order, SHOWN frames only, on both sides.**

| side | producer | what it emits |
|---|---|---|
| ours | `dump_yuv` (`crates/ec-av1/examples/dump_yuv.rs`) | `<prefix>.f<N>.yuv`, indexed by `decode_stream`'s output — display order, hidden alt-refs never emitted, at the depth the stream's own sequence header carries (8-bit u8, 10/12-bit u16 LE) |
| theirs | `ffmpeg -i cell.obu -pix_fmt <fmt> -f rawvideo -` | exactly the shown frames, display order, u8 or u16 LE native |

The aomdec census paired **decode** order on both sides (`EC_AV1_FINAL_DUMP`, hidden
alt-refs included). ffmpeg does not expose hidden alt-refs at all — `-flags2 +showall`
emits the identical byte count on `s422_320x242` with `libaom-av1`, `libdav1d` and the
native `av1` decoder — so display order is the only basis on which an ffmpeg comparison is
well posed. Both bases are reported, because a verdict that depends on the basis is itself
a finding; §3.2 shows it does not.

**Geometry is an explicit argument, and it comes from the oracle.** `ffprobe -show_streams`
reports `width`/`height`/`pix_fmt` from the same libavcodec decoder; the frame length is
derived from that (`plane_spans`), and a frame whose byte length disagrees is a hard error.
A count over zero frames is a hard error, never a vacuous `0/0/0`.

**One measured instrument trap, and it would have faked 18 red cells.**
`ffmpeg -f yuv4mpegpipe` is the natural geometry source (it is what the aomdec census
parsed out of aomdec's y4m header), and it **cannot carry any 10-bit cell in ffmpeg 8.1.3**:

```
$ ffmpeg -loglevel error -i s422_320x242_10b.obu -frames:v 1 -f yuv4mpegpipe -
Conversion failed!            # vf#0:0 Task finished with error code: -22
```

The same stream's `-f rawvideo -pix_fmt yuv422p10le` path decodes it fine (4 956 160 B =
16 × 309 760). My first full run therefore recorded 18 of the 18 ten-bit cells as
`ORACLE-REFUSED` — a false red produced by the muxer's format list, not by any decode.
The census now takes geometry from `ffprobe` and packs `-pix_fmt` explicitly. Anyone
re-deriving this census from a y4m header will hit the same 18 false reds.

**Our side is the 4:2:2 bypass build.** `decode_stream` refuses 4:2:2 at the sequence
header, so the 51 4:2:2 cells cannot be decoded by a build of `affe70dc` at all. The
measurement used a scratch, detached, never-committed worktree
`/home/tahinli/.cache/wt/probe422base` at `affe70dc` with the guard condition at
`stream.rs:1803` neutered (`if false && seq.subsampling_x != seq.subsampling_y`), marked
`// TEMP-PROBE-BYPASS: EC_AV1_ALLOW_422_PROBE`. Note that `EC_AV1_ALLOW_422_PROBE=1` on its
own is inert — the shipped code never reads it; the bypass is patch-run-restore. The 4:2:0
and 4:4:4 control rows in §2 were produced by the same build and are therefore unaffected
by the patch (the patched branch is unreachable for `subsampling_x == subsampling_y`).

---

## 1. Cell list and encoder provenance

95 cells: the census's own list (`cells_full.json`), with the nine committed pins repathed
onto this worktree. **sha256 of all 95 files verified against `cells_full.json`: 0
mismatches** (`cells_ff.json` is the repathed copy).

| origin | n | where | encoder recipe |
|---|---|---|---|
| `COMMITTED-PIN` | 9 | `crates/ec-av1/fixtures/*.obu` | committed fixtures; recipes recorded per cell in `lanes/av1422lpf.report.md:58-64` (`testsrc2` 320x240 4:2:2 + `--lossless=1`; `+ --tile-columns=1 --tile-rows=1`; the same at 10-bit) |
| `probe-cache` | 23 | `/home/tahinli/.cache/cells/av1422lpf/{lossy_all,probe}/*.obu` | the `--lossless=1` / odd-geometry / tiled corpora of `lane-av1422lpf`; per-cell recipe column at `lanes/av1422lpf.report.md:58` |
| `probe-cache-control` | 8 | `/home/tahinli/.cache/cells/av1422lpf/{regress_420,regress_444,probe}/*.obu` | `ll420_a/c/d`, `ll444_a/b/c`, `ll420_allintra`, `ll444_allintra` — the same recipe at 4:2:0 / 4:4:4, kept as the format controls |
| `fresh-reproducer` | 1 | `/home/tahinli/.cache/lane-av1422ctintrabc/cells/R422_320x242.obu` | `lane-av1422ctintrabc`'s own reproducer |
| `fresh-sweep` | 54 | `/home/tahinli/.cache/census422b/sweep/*.obu` | the census's `mksweep.sh` recipe — quoted in full in §5 |

**The sweep recipe** (all 54 sweep cells; `--cpu-used` 0, `--cq-level` 24, 16 frames):

```
ffmpeg -v error -f lavfi -i "testsrc2=size=${w}x${h}:rate=25" -frames:v 16 \
       -pix_fmt yuv422p|yuv420p|yuv444p{,10le} -f rawvideo cell.raw
# y4m built BY HAND: header line, then b"FRAME\n" + one frame of planes per frame.
# libaom's y4minput.c:1163-1171 requires the six bytes "FRAME\n" before EVERY frame;
# a y4m that repeats the full header per frame encodes ZERO frames.
aomenc --codec=av1 --profile=$prof --input-bit-depth=$d --bit-depth=$d --limit=16 \
       --lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=1 --pass=1 \
       --cq-level=24 --threads=4 --kf-min-dist=0 --kf-max-dist=999999 \
       --width=$w --height=$h --cpu-used=0 --obu -o cell.obu cell.y4m
```

(`--auto-alt-ref=1` is why every sweep cell has 17 decode frames and 16 shown — see §3.2.)

**`cells_recipe.json` provenance, stated exactly:** it holds 16 entries, all
`origin: "recipe-sweep"`, `committed: false`, all `--cpu-used ∈ {0,2,4,6}` ×
{320x242, 320x246, 322x240, 322x242} 4:2:2 encodes used to test whether a verdict depends
on the recipe. **None of the 95 census cells is one of those 16** — they are a separate
probe set. Their sha256, e.g. `rc0_s422_320x242` = `909e58db18fd4b75…`, are recorded in
`cells_recipe.json` and in `recipe_pre.json` / `recipe_post.json`.

---

## 2. THE CENSUS — 95 cells, ffmpeg 8.1.3 oracle, tree `affe70dc`

Counts are wrong **samples** per plane, over shown/display frames.
`frames` = shown frames (ffmpeg's count, and ours).

#### 4:2:2

| cell | geometry | ss | depth | disp frames | ffmpeg verdict | ffmpeg Y/U/V | aomdec verdict |
|---|---|---|---|---|---|---|---|
| `W_intrabc` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `X_intrabc_tiled` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `Y_intrabc_10b` | 320x240 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `422_sb128_3f` | 128x128 | 10 | 8 | 3 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `422_residual_compound_warp_nolr_16f` | 256x288 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `422_residual_compound_warp_16f` | 256x288 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `422_intrabc_sb128_strip_notxsearch` | 384x320 | 10 | 8 | 5 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `422_intrabc_sb128_strip` | 384x320 | 10 | 8 | 5 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `422_allskip_2f` | 128x128 | 10 | 8 | 2 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `V_tile2x2_odd` | 322x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `U_tilerows1` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `T_tilecols2` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `S_odd326x242_10b` | 326x242 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `R_odd322x240` | 322x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `Q_odd320x242` | 320x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `O_odd322x242` | 322x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `L_tiled` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `K_sct` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `J_10bit_lr0` | 320x240 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `I_10bit_mandel` | 320x240 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `H_10bit_testsrc2` | 320x240 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `F_allintra` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `E_noglobal` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `D_bars` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `C_mandel320` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `B_testsrc2_cpu6` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `A_testsrc2_cpu0` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `AD_inter_nogm` | 320x240 | 10 | 8 | 40 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `AB_inter_warp_odd` | 322x242 | 10 | 8 | 40 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `AA_inter_compound` | 320x240 | 10 | 8 | 40 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `ll422_noibc` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `ll422_allintra` | 320x240 | 10 | 8 | 1 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `R422_320x242` | 320x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_320x240` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_320x242` | 320x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_320x242_10b` | 320x242 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_320x246` | 320x246 | 10 | 8 | 16 | DIVERGES | 0/7810/5531 | DIVERGES |
| `s422_320x250_10b` | 320x250 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_322x240` | 322x240 | 10 | 8 | 16 | DIVERGES | 0/22003/21625 | DIVERGES |
| `s422_322x242` | 322x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_322x246` | 322x246 | 10 | 8 | 16 | DIVERGES | 0/3437/2222 | DIVERGES |
| `s422_326x240` | 326x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_326x242` | 326x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_326x246` | 326x246 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_352x242_10b` | 352x242 | 10 | 10 | 16 | DIVERGES | 0/4653/3777 | DIVERGES |
| `s422_352x250_10b` | 352x250 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_384x240` | 384x240 | 10 | 8 | 16 | DIVERGES | 0/1780/1871 | DIVERGES |
| `s422_384x242` | 384x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_384x246` | 384x246 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s422_416x242_10b` | 416x242 | 10 | 10 | 16 | DIVERGES | 0/5740/4348 | DIVERGES |
| `s422_416x250_10b` | 416x250 | 10 | 10 | 16 | DIVERGES | 99011/61486/60106 | DIVERGES |

#### 4:2:0 controls

| cell | geometry | ss | depth | disp frames | ffmpeg verdict | ffmpeg Y/U/V | aomdec verdict |
|---|---|---|---|---|---|---|---|
| `ll420_a` | 320x240 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `ll420_c` | 320x240 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `ll420_d` | 320x240 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `ll420_allintra` | 320x240 | 11 | 8 | 1 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_320x240` | 320x240 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_320x242` | 320x242 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_320x242_10b` | 320x242 | 11 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_320x246` | 320x246 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_320x250_10b` | 320x250 | 11 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_322x240` | 322x240 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_322x242` | 322x242 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_322x246` | 322x246 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_326x240` | 326x240 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_326x242` | 326x242 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_326x246` | 326x246 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_352x242_10b` | 352x242 | 11 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_352x250_10b` | 352x250 | 11 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_384x240` | 384x240 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_384x242` | 384x242 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_384x246` | 384x246 | 11 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_416x242_10b` | 416x242 | 11 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s420_416x250_10b` | 416x250 | 11 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |

#### 4:4:4 controls

| cell | geometry | ss | depth | disp frames | ffmpeg verdict | ffmpeg Y/U/V | aomdec verdict |
|---|---|---|---|---|---|---|---|
| `ll444_a` | 320x240 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `ll444_b` | 320x240 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `ll444_c` | 320x240 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `ll444_allintra` | 320x240 | 00 | 8 | 1 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_320x240` | 320x240 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_320x242` | 320x242 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_320x242_10b` | 320x242 | 00 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_320x246` | 320x246 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_320x250_10b` | 320x250 | 00 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_322x240` | 322x240 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_322x242` | 322x242 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_322x246` | 322x246 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_326x240` | 326x240 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_326x242` | 326x242 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_326x246` | 326x246 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_352x242_10b` | 352x242 | 00 | 10 | 16 | BYTE-EXACT | 0/0/0 | DIVERGES |
| `s444_352x250_10b` | 352x250 | 00 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_384x240` | 384x240 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_384x242` | 384x242 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_384x246` | 384x246 | 00 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_416x242_10b` | 416x242 | 00 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
| `s444_416x250_10b` | 416x250 | 00 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT |
Totals: **4:2:2 — 44 exact / 7 diverging of 51. 4:2:0 — 22/22 exact. 4:4:4 — 22/22 exact.**
Whole set: 88 byte-exact, 7 diverging, 0 refuses, 0 comparator errors, 0 frame-count
mismatches.

### 2.1 The 7 diverging 4:2:2 cells, with the first divergence

| cell | format | geometry | first bad frame | plane | first bad (row, col) | ours vs ffmpeg | wrong-sample bbox | frames with any wrong |
|---|---|---|---|---|---|---|---|---|
| `s422_320x246` | 422 | 320x246 d8 | 0 | U | (62, 144) | 161 vs 166 | U rows 46–201, cols 134–157 | 16/16 |
| `s422_322x240` | 422 | 322x240 d8 | 0 | U | (62, 149) | 160 vs 166 | U rows 61–239, cols 134–160 | 16/16 |
| `s422_322x246` | 422 | 322x246 d8 | 0 | U | (62, 144) | 169 vs 166 | U rows 60–203, cols 134–160 | 16/16 |
| `s422_352x242_10b` | 422 | 352x242 d10 | 0 | U | (62, 160) | 667 vs 664 | U rows 47–215, cols 150–175 | 16/16 |
| `s422_384x240` | 422 | 384x240 d8 | **6** | U | (174, 32) | 133 vs 132 | U rows 167–190, cols 19–34 | **10/16** |
| `s422_416x242_10b` | 422 | 416x242 d10 | 0 | U | (62, 192) | 640 vs 664 | U rows 22–203, cols 175–207 | 16/16 |
| `s422_416x250_10b` | 422 | 416x250 d10 | 0 | Y | (128, 384) | 370 vs 371 | U rows 99–249, cols 139–207 | 16/16 |

Six of the seven are **chroma-only** (Y is 0); `s422_416x250_10b` is the one cell where
luma is wrong too (99011/61486/60106), which is also the only one the census's own
ordering flagged as "luma also wrong". `s422_384x240` is the only cell that starts late —
its first bad display frame is 6 and only 10 of 16 frames carry a wrong sample — the same
late start the aomdec census recorded (first bad decode frame 1 of 17).

Per-frame per-plane counts for all seven are in `full_ff.json` (`per_frame`). The Y plane
is 0 for six of the seven in every frame; the V plane tracks the U plane's shape throughout.

---

## 3. Delta against the aomdec census (`lanes/av1422census2.report.md` §3.2)

### 3.1 Verdicts: 94 of 95 agree, and the one disagreement is not an oracle disagreement

| | aomdec census (`combined.json` post tree `ed7c99bc`) | this census (ffmpeg, `affe70dc`) |
|---|---|---|
| 4:2:2 (51) | 44 exact / 7 diverging | **44 exact / 7 diverging** — same 7 cells |
| 4:2:0 (22) | 22 exact | 22 exact |
| 4:4:4 (22) | 21 exact / 1 diverging | **22 exact** |

**The one disagreement is `s444_352x242_10b` (352x242 10-bit 4:4:4).** The aomdec census
reports `0/1166/1413`; ffmpeg reports `0/0/0`; and on **this tree against aomdec itself**
the cell is byte-exact too — I re-ran the aomdec comparator on it:

```
compare('s444_352x242_10b', ours=EC_AV1_FINAL_DUMP, oracle=aomdec EC_AV1_FINAL_DUMP,
        352, 242, ss=00, depth=10)  ->  BYTE-EXACT  17 frames  0/0/0
```

So the aomdec row is stale, not wrong about an oracle. `lane-av1444d10` fixed this exact
cell — "the 10-bit 4:4:4 352x242 chroma divergence is a chroma PALETTE PREDICTION window"
(`lanes/av1444d10.report.md:1-7`, commits `f3afa5b7` the fix and `eb73ef89` the fixture +
gate; both verified ancestors of `affe70dc`). The aomdec census's `post` tree `ed7c99bc`
is **36 commits behind `main`**, and that lane is in those 36. The census2 report's own
§3.3 line for this cell ("DIVERGES 0/1166/1413, identical counts on both trees") is a
correct record of `ed7c99bc` and is superseded.

**Lesson for the lift:** the census-of-record's numbers are only valid for the tree they
were measured on. Re-measure before lifting, and never lift a guard on the strength of a
report's table alone.

### 3.2 Counts differ on all 7 diverging cells, and the difference IS the hidden alt-ref

| cell | aomdec, decode order (17 frames) | hidden frame's own counts | shown-only total | ffmpeg, display order (16 frames) | |
|---|---|---|---|---|---|
| `s422_320x246` | 0/8339/5903 | 0/529/372 | 0/7810/5531 | 0/7810/5531 | MATCH |
| `s422_322x240` | 0/22969/22534 | 0/966/909 | 0/22003/21625 | 0/22003/21625 | MATCH |
| `s422_322x246` | 0/3603/2324 | 0/166/102 | 0/3437/2222 | 0/3437/2222 | MATCH |
| `s422_352x242_10b` | 0/4858/3949 | 0/205/172 | 0/4653/3777 | 0/4653/3777 | MATCH |
| `s422_384x240` | 0/1982/2081 | 0/202/210 | 0/1780/1871 | 0/1780/1871 | MATCH |
| `s422_416x242_10b` | 0/5968/4524 | 0/228/176 | 0/5740/4348 | 0/5740/4348 | MATCH |
| `s422_416x250_10b` | 103250/64191/62834 | 4239/2705/2728 | 99011/61486/60106 | 99011/61486/60106 | MATCH |

Every sweep cell is `--auto-alt-ref=1`, so every sweep cell has **17 decode frames of which
16 are shown** — one hidden alt-ref, coded and reconstructed, never emitted by ffmpeg.
Removing exactly that frame's own counts from the aomdec total reproduces the ffmpeg total
to the sample on all 7 cells and all 3 planes. The two oracles are not merely agreeing on
a verdict; on the diverging cells they agree on the exact per-plane wrong-sample count, once
the frame basis is stated. **A divergence count published without its frame basis is
therefore ambiguous by exactly one frame's worth of samples.**

(Frame identity, not just totals: the display-order per-frame vectors are a permutation of
the shown decode-order frames. For `s422_320x246` the display vector is
`f0, f4, f5, f3, f7, f6, f8, f2, f10, f9, f11, f12, f13, f15, f14, f16` — the ARF
reorder — and the hidden frame is `f1`. Pairing decode-order dumps to ffmpeg output
index-by-index would be wrong on these cells; pairing must be by display order.)

---

## 4. The lift checklist (plan only — no edits made)

### 4.1 The guard to lift

**`crates/ec-av1/src/stream.rs:1803-1808`**, in `decode_frame`:

```rust
1803    if seq.subsampling_x != seq.subsampling_y {
1804        return Err(Error::unsupported(
1805            "AV1 decode_stream",
1806            "a chroma format of 4:2:2 (subsampling_x != subsampling_y): this decoder decodes 4:2:0 and 4:4:4; 4:2:2 is not ported, and 4:4:0 (0,1) is not a codable cell",
1807        ));
1808    }
```

Lifting it changes the condition to the shape that remains uncodable, `subsampling_x == 0
&& subsampling_y == 1` (4:4:0), which the header parser cannot produce
(`ec-av1-syntax` `sequence.rs:481` reads `subsampling_y` only when `subsampling_x == 1`).
Per the repo's unreachable-code rule, that residual guard is closed with an **assertion,
not a comment**.

### 4.2 The refusal-inventory entries to retire

| site | what | action |
|---|---|---|
| `crates/ec-av1/src/refusal_inventory.rs:150` | the refusal string in `REFUSALS` | delete |
| `crates/ec-av1/src/refusal_inventory.rs:529-533` | the `PROVEN` tuple pairing that string with `a_non_420_subsampled_sequence_header_is_refused_by_name` (`Proof::NegativeGate`) | delete the tuple; the string is gone from the decode path so the pair goes with it |

`the_decode_path_refuses_exactly_the_listed_cases`
(`refusal_inventory.rs:854`) fails on **both** directions, so both rows must go in the same
commit: it asserts `listed - found` is empty ("Delete them from the inventory — the
capability landed, which is the good case") and `found - listed` is empty.

### 4.3 The tests that assert the refusal and must be inverted

| test | file:line | what it asserts today | what the lift needs |
|---|---|---|---|
| `a_non_420_subsampled_sequence_header_is_refused_by_name` | `stream.rs:2314` (string at `:2316`) | a hand-built profile-1 and profile-2 header each refuse by name, with a profile-0 control still decoding | invert: 4:2:2 decodes pixel-exact; keep the 4:4:0 arm as an assertion |
| `the_440_cell_is_not_a_codable_chroma_shape` | `stream.rs:2492` (assertion at `:2727-2733`) | arm 3 decodes `440_request_is_422.obu` and it refuses by name | keep arms 1–2 (the enumeration) unchanged; arm 3 must now assert the pinned bytes are `(1,0)` and **decode**, with a 4:4:0 assert standing where the refusal was |
| `the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name` | `stream.rs:2764` | pins `422_allskip_2f.obu` (62 B) + `422_sb128_3f.obu` (12543 B), then asserts each refuses | keep the size + `fnv1a64` pins; replace the refusal assert with an ffmpeg byte-exact decode |
| `the_pinned_422_intrabc_sb128_strip_witnesses_refuse_by_name` | `stream.rs:2820` | same shape, `422_intrabc_sb128_strip{,_notxsearch}.obu` | same inversion |
| `the_pinned_422_lossless_inter_witnesses_are_present_and_refuse_by_name` | `stream.rs:3010` | same shape, `422_residual_compound_warp{,_nolr}_16f.obu` | same inversion |
| `the_pinned_422_residual_compound_warp_witness_is_present_and_refuses_by_name` | `stream.rs:3326` | same shape | same inversion |
| `the_pinned_422_lr_off_witness_is_present_and_refuses_by_name` | `stream.rs:3406` | same shape | same inversion |

Six of these seven are the class the bypass skill warns about: their only load-bearing
content today is the fixture pins, because a gate that asserts a refusal string passes on a
PRE-fix tree too. **Inverting them is the point** — that is what makes them witnesses.

Two further tests must be re-checked, not edited, at lift time:

- `every_chroma_unit_decode_block_rect_can_present_has_a_coefficient_table`
  (`refusal_inventory.rs:1739`) already walks `(1,0)` "so the proof does not depend on that
  guard staying in place" — it should not change.
- `the_440_cell_is_not_a_codable_chroma_shape`'s own doc at `:1731-1736` names
  `a_non_420_subsampled_sequence_header_is_refused_by_name` as the thing that refuses
  `(1,0)`; that sentence has to be corrected in place or the file lies.

### 4.4 The fixture-pinning convention this corpus uses

- **Pins live in the crate:** `crates/ec-av1/fixtures/*.obu`, committed (`git add -f`;
  `fixtures` is gitignored and `.gitignore:11-12` re-negates `crates/*/fixtures/**`).
- **Read through `crate_pin()`** (`stream.rs:10909-10918`): crate dir first, `pin_dir()`
  (`EC_AV1_PIN_DIR`, else the gitignored root `fixtures/`) as an override only. A pin that
  lives only in the root `fixtures/` is lost when a runner's scratchpad is reaped, and
  three gates once sat green having tested nothing for exactly that reason.
- **Identity is size + `fnv1a64`**, asserted by `read_pin()` (`stream.rs:5768-5779`), never
  a bare existence check. This census additionally records sha256 per cell
  (`cells_full.json`), and verified 0 drift.
- **Missing pin is a FAILURE, not a skip** (`read_pin` panics).
- **Oracle availability is probed, then asserted last**: `have_ffmpeg()` /
  `have_aomenc()` (`stream.rs:6027`, `:10920`) with `EC_AV1_REQUIRE_FFMPEG` /
  `EC_AV1_REQUIRE_AOMENC` as the CI escape, plus a printed `SKIP` line so no gate can skip
  silently.
- **Depth is read from the stream, never from the gate's name**: `stream_bit_depth()`
  (`stream.rs:5787`) and `dump_yuv`'s `--depth` assert.

### 4.5 What the 51/51 gate has to look like

The gate cannot be "the corpus is green" — the corpus does not run in CI. It has to carry
the census's load-bearing properties, or it is a gate that cannot fail:

1. **One table of 51 pinned 4:2:2 cells**, each row: committed pin name, sha256/size/
   `fnv1a64`, geometry, depth, shown-frame count, and the decode arm it witnesses. The 51
   are the §2 table; 9 are already committed, **42 are not** and must be captured
   (`git add -f`) before the lift can be gated at all.
2. **Per-plane, per-frame byte-exactness against ffmpeg**, not a per-file hash: the
   comparator in `ffcensus.py` — explicit geometry from the oracle, per-frame plane spans,
   one `bps`-wide unit = one wrong sample, hard error on zero frames, hard error on a
   length disagreement.
3. **Depth-correct comparison on both sides**: `dump_yuv`'s u16-LE packing vs
   `ffmpeg -pix_fmt yuv422p10le`. Comparing the u8-narrowing `EC_AV1_FINAL_DUMP` sibling
   dump against a depth-correct oracle makes every HBD cell red by construction — the exact
   mistake `main`'s `5400db61` annotated.
4. **A liveness arm inside the gate**, following
   `every_oracle_comparator_reds_on_a_one_byte_wrong_oracle` (`stream.rs:11508`): one
   flipped oracle sample must turn every comparator red. §6's 23-arm table is the model.
5. **A non-vacuity assertion per row**: the reachability counter for the arm each cell
   witnesses (this crate's `chroma422_square_hits` / `chroma422_rect_hits` /
   `chroma422_sub8_hits` / `chroma422_chunk_hits` / `chroma422_pair_wide_hits` /
   `intra_128_in_inter_mu_chroma_hits` / `intrabc_128rect_chroma_chunk` families, all
   printed by `decode_probe`) must be `> 0` on that cell, or the byte-exact green is
   vacuous with respect to 4:2:2.
6. **The format controls stay in the same gate**: 22 4:2:0 + 22 4:4:4 rows must remain
   byte-exact, so a 4:2:2 fix that moves a supported format is caught by the gate rather
   than by a later report. §2 measures them all exact today; that is the baseline.
7. **The gate needs ffmpeg.** Under `have_ffmpeg()`'s current shape it would silently skip
   on a runner without it. For a 51/51 claim that must be `EC_AV1_REQUIRE_FFMPEG` in CI.

---

## 5. Reproducing the census — one command

```bash
# 1. bypass worktree (scratch, detached, never committed)
cd /home/tahinli/Documents/Code/Rust/edith_codecs
git worktree add -f --detach /home/tahinli/.cache/wt/probe422base affe70dc
#    edit crates/ec-av1/src/stream.rs:1803 -> `if false && seq.subsampling_x != ...`
CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/probe422base \
  cargo build --manifest-path /home/tahinli/.cache/wt/probe422base/Cargo.toml \
  -p ec-av1 --release --example decode_probe --example dump_yuv

# 2. the census (95 cells) and its liveness control (23 arms)
cd /home/tahinli/.cache/census422b
python3 ffcensus.py cells_ff.json full_ff.json          # ~82 s, writes work-ffmpeg/<cell>/
python3 ffliveness.py cells_ff.json liveness_arms.json liveness_ff.json
```

Restore afterwards, inside the worktree only:

```bash
git -C /home/tahinli/.cache/wt/probe422base checkout -- crates/ec-av1/src/stream.rs
git -C /home/tahinli/.cache/wt/probe422base status --porcelain   # must be empty
git worktree remove --force /home/tahinli/.cache/wt/probe422base
```

Inputs: `cells_ff.json` (this lane's repathed copy of `cells_full.json` — the only edit is
the nine committed pins' path prefix), `cells_recipe.json` (the 16 recipe-probe encodes,
provenance only), `combined.json` (the aomdec census's own per-cell results, for the §3
delta).

---

## 6. Liveness control — 23 arms, 23 PASS, 0 FAIL

**Before reading any verdict above, the comparator was shown to bite.** One oracle sample's
bit 0 is flipped, in one named plane of one named frame, per arm; the count must move by
exactly **+1 in that plane** with every other plane and every other frame unchanged, and
the file is restored before the next arm. +1 is exact by construction, not hoped for: at
8 bit bit 0 of the low byte is the sample's own LSB, and at 10/12 bit the sample sits in a
16-bit LE container whose low byte's bit 0 is still the sample LSB with `2**bitdepth <
2**16`, so nothing carries out of the container.

| cell | geometry | frame | plane | baseline Y/U/V | after flip | Δ | moved |
|---|---|---|---|---|---|---|---|
| `s422_320x242` | 320x242 d8 | 0 | Y | 0/0/0 | 1/0/0 | +1 Y | PASS |
| `s422_320x242` | 320x242 d8 | 0 | U | 0/0/0 | 0/1/0 | +1 U | PASS |
| `s422_320x242` | 320x242 d8 | 15 | V | 0/0/0 | 0/0/1 | +1 V | PASS |
| `W_intrabc` | 320x240 d8 | 0 | Y | 0/0/0 | 1/0/0 | +1 Y | PASS |
| `W_intrabc` | 320x240 d8 | 7 | U | 0/0/0 | 0/1/0 | +1 U | PASS |
| `W_intrabc` | 320x240 d8 | 15 | V | 0/0/0 | 0/0/1 | +1 V | PASS |
| `422_allskip_2f` | 128x128 d8 | 0 | U | 0/0/0 | 0/1/0 | +1 U | PASS |
| `Y_intrabc_10b` | 320x240 d10 | 0 | Y | 0/0/0 | 1/0/0 | +1 Y | PASS |
| `Y_intrabc_10b` | 320x240 d10 | 3 | U | 0/0/0 | 0/1/0 | +1 U | PASS |
| `Y_intrabc_10b` | 320x240 d10 | 15 | V | 0/0/0 | 0/0/1 | +1 V | PASS |
| `AB_inter_warp_odd` | 322x242 d8 | 39 | U | 0/0/0 | 0/1/0 | +1 U | PASS |
| `s422_320x246` | 320x246 d8 | 0 | Y | **0/7810/5531** | 1/7810/5531 | +1 Y | PASS |
| `s422_320x246` | 320x246 d8 | 0 | U | **0/7810/5531** | 0/7811/5531 | +1 U | PASS |
| `s422_320x246` | 320x246 d8 | 11 | V | **0/7810/5531** | 0/7810/5532 | +1 V | PASS |
| `s422_322x240` | 322x240 d8 | 5 | U | **0/22003/21625** | 0/22004/21625 | +1 U | PASS |
| `s422_384x240` | 384x240 d8 | 14 | V | **0/1780/1871** | 0/1780/1872 | +1 V | PASS |
| `s422_352x242_10b` | 352x242 d10 | 0 | U | **0/4653/3777** | 0/4654/3777 | +1 U | PASS |
| `s422_416x250_10b` | 416x250 d10 | 0 | Y | **99011/61486/60106** | 99012/61486/60106 | +1 Y | PASS |
| `s422_416x250_10b` | 416x250 d10 | 5 | U | **99011/61486/60106** | 99011/61487/60106 | +1 U | PASS |
| `s444_352x242_10b` | 352x242 d10 | 4 | U | 0/0/0 | 0/1/0 | +1 U | PASS |
| `s420_320x240` | 320x240 d8 | 0 | U | 0/0/0 | 0/1/0 | +1 U | PASS |
| `ll420_a` | 4:2:0 d8 | 0 | Y | 0/0/0 | 1/0/0 | +1 Y | PASS |
| `ll444_a` | 4:4:4 d8 | 0 | V | 0/0/0 | 0/0/1 | +1 V | PASS |

Sample values, as the raw arm records them: `s422_320x242` U f0 oracle byte 77440
`de`→`df`; `Y_intrabc_10b` (10-bit LE u16) U f3 byte 537600 `6801`→`6901`, V f15
`c003`→`c103`; `s422_352x242_10b` U f0 `6e03`→`6f03`; `s422_416x250_10b` Y f0
`3f01`→`3e01`.

The load-bearing arms are the seven **non-zero baselines**: a comparator that attributed a
plane to the wrong frame, dropped chroma, or compared our samples with themselves cannot
move a plane that is already wrong by thousands of samples by exactly one more while
leaving the other two planes untouched. The zero-baseline arms are in the table
deliberately — they are the false-green case.

Two frame-index notes, both caught by this control and both real: the arms address a
**display** frame, so `AB_inter_warp_odd`'s last arm is frame 39 of 40 (the aomdec census's
"43 decode frames" is not a display index), and an out-of-range frame is a hard error rather
than a silently-missing arm.

---

## 7. What this lane does NOT establish

- **It does not attribute any of the 7 divergences.** No arm was localised, no
  decode-path file was touched. The seven are measured, bounded (plane, first frame,
  coordinates, bbox, per-frame counts) and reproducible by the one command in §5.
- **It does not lift the guard.** `stream.rs:1803` still refuses 4:2:2 by name on
  `affe70dc`, and the refusal string is still in `refusal_inventory.rs:150` and `:530`.
- **The 42 uncommitted 4:2:2 cells are a gate-construction task, not a measurement task.**
  §2 measures them from `/home/tahinli/.cache/cells` and
  `/home/tahinli/.cache/census422b/sweep`; a 51/51 gate needs them committed under
  `crates/ec-av1/fixtures/` with the §4.4 pins.
- **Six of the seven diverging cells are chroma-only and share a first-divergence row
  band (row 62, U columns 134–207).** That is a shape observation for whoever owns the
  chroma arm, not an attribution: no trace was taken here.

---

# ADDENDUM (2026-10-01) — re-measured on `3d56fb60`: **94 of 95 byte-exact, 50 of 51 4:2:2**

**No cell moved the wrong way. Not one 4:2:0 cell and not one 4:4:4 cell changed
verdict: 22/22 and 22/22 byte-exact before and after, every one of them produced by a
CLEAN build of the tree, none touched by the 4:2:2 change.** The 4:2:2 delta is six cells
closing, one cell unmoved, and the 4:2:0/4:4:4 controls are bit-identical across the
change.

| | `affe70dc` (this report's §2) | `3d56fb60` (this addendum) |
|---|---|---|
| 4:2:2 | 44 exact / 7 diverging | **50 exact / 1 diverging** |
| 4:2:0 | 22 exact | 22 exact (0 moved) |
| 4:4:4 | 22 exact | 22 exact (0 moved) |
| whole set | 88 exact / 7 diverging | **94 exact / 1 diverging** |

`3d56fb60` = `298be661` (lane-av1422luma: `decode_rect4_16_intrabc` gives ss (1,0) its own
chroma) plus `b8eed69f`; both verified ancestors of `3d56fb60`.

## A1. The delta, per cell

| cell | format | `affe70dc` | `3d56fb60` | |
|---|---|---|---|---|
| `s422_320x246` | 4:2:2 | DIVERGES 0/7810/5531 | **BYTE-EXACT 0/0/0** | closed |
| `s422_322x240` | 4:2:2 | DIVERGES 0/22003/21625 | **BYTE-EXACT 0/0/0** | closed |
| `s422_322x246` | 4:2:2 | DIVERGES 0/3437/2222 | **BYTE-EXACT 0/0/0** | closed |
| `s422_352x242_10b` | 4:2:2 | DIVERGES 0/4653/3777 | **BYTE-EXACT 0/0/0** | closed |
| `s422_416x242_10b` | 4:2:2 | DIVERGES 0/5740/4348 | **BYTE-EXACT 0/0/0** | closed |
| `s422_416x250_10b` | 4:2:2 | DIVERGES 99011/61486/60106 | **BYTE-EXACT 0/0/0** | closed — **the luma-wrong cell** |
| `s422_384x240` | 4:2:2 | DIVERGES 0/1780/1871 | **DIVERGES 0/1780/1871** | **unmoved, to the sample** |
| all other 45 4:2:2 cells | 4:2:2 | BYTE-EXACT | BYTE-EXACT | unchanged |
| all 44 4:2:0 / 4:4:4 controls | | BYTE-EXACT | BYTE-EXACT | unchanged |

**These are this lane's own numbers, from my instrument, not lane av1422luma's.** The six
closures match the six the lane reported; `s422_384x240` was predicted to stay and does,
with a count that is *byte-for-byte the same* as on `affe70dc` — every plane, every frame,
first bad sample still display frame 6, U (row 174, col 32), ours 133 vs ffmpeg 132, bbox
U rows 167–190 cols 19–34 / V rows 169–192 cols 19–35, still only 10 of 16 frames carrying
a wrong sample. The per-frame vectors of the two trees are **identical lists**. Whatever
`298be661` changed, it changed nothing in that cell, which is the expected shape for a
residual whose owner is the separate group-tail chroma SKIP arm.

## A2. Full 4:2:2 table on `3d56fb60`
| cell | geometry | ss | depth | disp frames | verdict @3d56fb60 | Y/U/V @3d56fb60 | verdict @affe70dc | Y/U/V @affe70dc |
|---|---|---|---|---|---|---|---|---|
| `W_intrabc` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `X_intrabc_tiled` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `Y_intrabc_10b` | 320x240 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `422_sb128_3f` | 128x128 | 10 | 8 | 3 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `422_residual_compound_warp_nolr_16f` | 256x288 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `422_residual_compound_warp_16f` | 256x288 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `422_intrabc_sb128_strip_notxsearch` | 384x320 | 10 | 8 | 5 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `422_intrabc_sb128_strip` | 384x320 | 10 | 8 | 5 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `422_allskip_2f` | 128x128 | 10 | 8 | 2 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `V_tile2x2_odd` | 322x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `U_tilerows1` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `T_tilecols2` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `S_odd326x242_10b` | 326x242 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `R_odd322x240` | 322x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `Q_odd320x242` | 320x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `O_odd322x242` | 322x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `L_tiled` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `K_sct` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `J_10bit_lr0` | 320x240 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `I_10bit_mandel` | 320x240 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `H_10bit_testsrc2` | 320x240 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `F_allintra` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `E_noglobal` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `D_bars` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `C_mandel320` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `B_testsrc2_cpu6` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `A_testsrc2_cpu0` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `AD_inter_nogm` | 320x240 | 10 | 8 | 40 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `AB_inter_warp_odd` | 322x242 | 10 | 8 | 40 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `AA_inter_compound` | 320x240 | 10 | 8 | 40 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `ll422_noibc` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `ll422_allintra` | 320x240 | 10 | 8 | 1 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `R422_320x242` | 320x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_320x240` | 320x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_320x242` | 320x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_320x242_10b` | 320x242 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_320x246` | 320x246 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | DIVERGES | 0/7810/5531 |
| `s422_320x250_10b` | 320x250 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_322x240` | 322x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | DIVERGES | 0/22003/21625 |
| `s422_322x242` | 322x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_322x246` | 322x246 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | DIVERGES | 0/3437/2222 |
| `s422_326x240` | 326x240 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_326x242` | 326x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_326x246` | 326x246 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_352x242_10b` | 352x242 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | DIVERGES | 0/4653/3777 |
| `s422_352x250_10b` | 352x250 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_384x240` | 384x240 | 10 | 8 | 16 | DIVERGES | 0/1780/1871 | DIVERGES | 0/1780/1871 |
| `s422_384x242` | 384x242 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_384x246` | 384x246 | 10 | 8 | 16 | BYTE-EXACT | 0/0/0 | BYTE-EXACT | 0/0/0 |
| `s422_416x242_10b` | 416x242 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | DIVERGES | 0/5740/4348 |
| `s422_416x250_10b` | 416x250 | 10 | 10 | 16 | BYTE-EXACT | 0/0/0 | DIVERGES | 99011/61486/60106 |

## A3. Controls, on `3d56fb60` — 44/44 exact

All 44 produced by the **clean** build
(`/home/tahinli/.cache/tgt/cleanmain_v2/release/examples/dump_yuv`, from a detached
worktree at `3d56fb60` with `git status --porcelain` empty), 22 4:2:0 + 22 4:4:4,
**44 BYTE-EXACT, 0 diverging, 0 errors, 0 frame-count mismatches** — the same verdict each
one had on `affe70dc`. Per-cell rows and the 22+22 breakdown are unchanged from §2 and are
not re-pasted here; the machine record is `full_ff_controls_3d56.json`.

## A4. Instrument discipline for this run

1. **sha256 of all 95 cells re-verified** against `cells_full.json`: 0 drift, 0 missing.
   The nine committed pins were re-pathed to the `3d56fb60` worktree before hashing, so the
   bytes hashed are the bytes that tree ships.
2. **Geometry from `ffprobe`, never a y4m header and never a file size**; frame length
   derived from it; a length disagreement is a hard error; a count over zero frames is a
   hard error.
3. **Display order on both sides**, hidden alt-refs excluded — the mapping rule is §0/§3.2
   of this report, unchanged: `dump_yuv` is display-order+shown+depth-from-header,
   `ffmpeg -f rawvideo` is display-order+shown, and a decode-order count is not comparable
   without subtracting the hidden frame's own counts.
4. **Depth-correct both sides**; the u8-narrowing dump was not used anywhere.
5. **The probe/bypass split is now an ASSERTED ARGUMENT, not a constant.** The driver takes
   `FF_DUMP_YUV` and records it on every result row as `probe`. The 4:2:2 rows were produced
   by a patch-run-restore build in a scratch detached worktree; the 4:2:0/4:4:4 control
   rows by a **separate clean build of the same tree**, so a bypassed binary cannot serve
   the controls. `--probe-records` proves the split on every run and fails if it is wrong:

   ```
   {"probe_split_control": "PASS", "cell": "W_intrabc",
    "clean_build": "REFUSES", "bypass_build": "BYTE-EXACT"}
   ```

   The clean build was re-checked after the bypass was torn down: it still prints
   `REFUSED: … a chroma format of 4:2:2 (subsampling_x != subsampling_y) …`.
6. **Bypass hygiene.** Scratch worktree `/home/tahinli/.cache/wt/probe422_v2` at `3d56fb60`,
   the guard at `stream.rs:1803` patched to `if false && seq.subsampling_x != …` under the
   `TEMP-PROBE-BYPASS: EC_AV1_ALLOW_422_PROBE` marker. The diff was **2 lines**
   (`git diff --stat`: 1 file, 2 insertions, 1 deletion — verified before building, because
   a two-line edit tool silently dropped a neighbouring comment line on the first attempt in
   this lane and had to be repaired). Restored with
   `git checkout -- crates/ec-av1/src/stream.rs`, `git status --porcelain` **empty**,
   worktree removed, and **the bypassed target directory
   `/home/tahinli/.cache/tgt/probe422_v2bypass` deleted** together with the previous run's
   `tgt/probe422base`, so no bypassed binary survives on disk to be picked up by a later
   run of this driver. The path the driver is documented to use is the clean build.

## A5. Liveness re-run on the `3d56fb60` dumps — 23 arms, 23 PASS, 0 FAIL

Re-run before these verdicts were read. One oracle sample's bit 0 flipped, one named plane
of one named display frame; the count must move exactly +1 in that plane with every other
plane and frame unchanged; the file is restored before the next arm.

| cell | frame | plane | baseline Y/U/V @3d56fb60 | Δ | verdict |
|---|---|---|---|---|---|
| `s422_320x242` | 0 / 0 / 15 | Y / U / V | 0/0/0 | +1 Y, +1 U, +1 V | PASS |
| `W_intrabc` | 0 / 7 / 15 | Y / U / V | 0/0/0 | +1 Y, +1 U, +1 V | PASS |
| `422_allskip_2f` | 0 | U | 0/0/0 | +1 U | PASS |
| `Y_intrabc_10b` | 0 / 3 / 15 | Y / U / V | 0/0/0 | +1 Y, +1 U, +1 V | PASS |
| `AB_inter_warp_odd` | 39 | U | 0/0/0 | +1 U | PASS |
| `s422_320x246` | 0 / 0 / 11 | Y / U / V | **0/0/0** (was 0/7810/5531) | +1 Y, +1 U, +1 V | PASS |
| `s422_322x240` | 5 | U | **0/0/0** (was 0/22003/21625) | +1 U | PASS |
| `s422_352x242_10b` | 0 | U | **0/0/0** (was 0/4653/3777) | +1 U | PASS |
| `s422_416x250_10b` | 0 / 5 | Y / U | **0/0/0** (was 99011/61486/60106) | +1 Y, +1 U | PASS |
| **`s422_384x240`** | 14 | V | **0/1780/1871** | **+1 V → 0/1780/1872** | **PASS** |
| `s444_352x242_10b` | 4 | U | 0/0/0 | +1 U | PASS |
| `s420_320x240` | 0 | U | 0/0/0 | +1 U | PASS |
| `ll420_a` (4:2:0) | 0 | Y | 0/0/0 | +1 Y | PASS |
| `ll444_a` (4:4:4) | 0 | V | 0/0/0 | +1 V | PASS |

The one arm that now carries the load is `s422_384x240`: it is the only cell still red, its
baseline is non-zero, and the flip moves **that** plane by exactly one while Y stays 0 and
every other frame is unmoved. A comparator that had quietly stopped reading the oracle
would report 0/0/0 here too; it reports 1780/1871 and moves by one. Sample values:
`s422_384x240` V f14 oracle byte 2718720 `eb`→`ea`; `s422_416x250_10b` Y f0 `3f01`→`3e01`;
`Y_intrabc_10b` U f3 `6801`→`6901`.

**23 arms, 23 PASS, 0 FAIL.** Six of the seven non-zero baselines from §6 are now exact, so
that class of arm is currently carried by one cell — re-run this control whenever the
remaining divergence closes, or the liveness proof loses its non-zero arm.

## A6. What this addendum does and does not change in §4

§4's lift checklist is unaffected as a plan, and its gate requirement is now one step
closer: the base it will be judged on is **50/51 4:2:2 with one known residual**, and that
residual is a single named cell (`s422_384x240`, 384x240 8-bit, chroma-only, 10 of 16 frames,
identical before and after `298be661`) with an owner lane already working it. Everything
§4 says about the guard site, the two `refusal_inventory.rs` rows, the seven tests to
invert, the pin convention and the 51/51 gate stands as written, and the 42-uncommitted-cells
capture task is now the same task — it is a prerequisite to the gate, not to the fix.

Not established by this addendum: no attribution of the `s422_384x240` residual (no trace
was taken; it is measured, bounded and unchanged), and no claim about 4:2:2 shapes the 95
cells do not contain.

## A7. Note on the instrument that landed on main while this ran

`3d56fb60` also carries `scripts/run422-ffmpeg.py` + `scripts/cmp422-ffmpeg.py` +
`scripts/flipctl-422.py` (lane av1422seed). It solves the decode→display mapping
explicitly and reports `mapping_solutions`, so it additionally **proves the mapping is
unique** for a cell — a property my instrument does not assert, because it never mixes the
two bases: it pairs `dump_yuv` (display order, shown only) against `ffmpeg -f rawvideo`
(display order, shown only) and therefore has no mapping to be ambiguous about. The two
instruments are independent and they agree on this tree: both put 50 of 51 4:2:2 exact with
only `s422_384x240` diverging. Where they differ in *form*: that one decodes with
`EC_AV1_FINAL_DUMP` (decode order) and maps forward; this one decodes with `dump_yuv`
(display order) and maps nothing. Two instruments that reach the same verdict by different
routes is worth more than either alone, and this report's numbers are its own, not that
lane's.
