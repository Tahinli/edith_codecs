# lane-av1muchunk422 — a witness hunt for the two `side > 64` mu-chunk walks

Base: `main` @ `6e58730b`. Branch: `lane/av1muchunk422`.

**Verdict: `12 attempts, 0 entries`.** No 4:2:2 stream reached site 2
(compound) or site 8 (intra-in-inter). Nothing is pinned, no gate was added,
and `decode.rs` is **unchanged** in this commit — the probes were temporary and
reverted (`git status` clean apart from this report).

## The two sites

Both live inside `decode_inter_block` (`crates/ec-av1/src/decode.rs:42122`) at
this base. Line numbers are this base, and the walks themselves are as
`lanes/av1muchroma.report.md` named them — I did not touch them.

| site | allocation site | enclosing guard | arm |
|---|---|---|---|
| **2** — compound `side>64` mu-chunk | `decode.rs:43954` (`chroma_side * chroma_side`) | `if side > 64` at `decode.rs:43767`, inside the vartx leaf loop of the `if is_compound` branch (`decode.rs:42541`) | compound inter |
| **8** — intra-in-inter `side>64` vartx | `decode.rs:46761` (`chroma_side * chroma_side`) | `if side > 64` at `decode.rs:46563`, in the intra-in-inter section | intra-in-inter inter block |

The twin **site 5** (single-ref `side>64` mu-chunk) is the same walk in the
`else` branch at `decode.rs:44239`, allocation at `45747`, guard at `45580`.

### The instrument

There is no committed counter that names these two walks. `CHROMA_SPLIT_TX_HITS`
fires at all 17 mu-chunk unit sites, so it cannot distinguish site 2 from site 5
— which is exactly why the prior lane had to write a throwaway probe.

So I did the same, with an env-gated `eprintln` behind `EC_MUCHUNK422` at
five places (two site probes, three guard probes):

```
MUCHUNK422 site=2 arm=compound   side={side} ss=({ss_x},{ss_y}) cs={chroma_side}
MUCHUNK422 site=8 arm=intra_in_inter side={side} ss=... cs=...
MUCHUNK422 guard=compound        side={side} ss=... nleaves=...
MUCHUNK422 guard=singleref       ...
MUCHUNK422 guard=intra_in_inter  ...
```

The three **guard** probes are what make the zero readable. A site probe that
stays 0 alone cannot say whether the walk was skipped because the arm never ran
at a 128 root or because the inner unit loop never ran; the guard probe sits on
the `side > 64` test itself and separates the two.

Instrument run via `cargo run -p ec-av1 --example decode_probe` (the existing
"where does a real stream stop" example). All five probes were compiled into
the binary — verified with `strings` on the built artifact before any decode,
so no zero below is a missing-string zero.

## Non-vacuity: the probes DO fire

A zero from an unverified instrument is worthless, so the positive controls come
first. Replaying every committed `.obu` in `crates/ec-av1/fixtures` through the
probe binary:

| fixture | site=2 | site=8 | guard=singleref | guard=compound | guard=intra_in_inter |
|---|---|---|---|---|---|
| `gm_small_side_witness.obu` | **272** | 0 | 140 | **136** | 0 |
| `444_intra_in_inter_128root_mu_chroma.obu` | 0 | **32** | 0 | 0 | **4** |
| `422_sb128_3f.obu` | 0 | 0 | **13** | 0 | 0 |
| `troy_sb128_inter_witness.obu` | **628** | 0 | 196 | **314** | 0 |
| `W_intrabc.obu` | 0 | 0 | 2048 | **896** | **1792** |
| `X_intrabc_tiled.obu` | 0 | 0 | 2048 | **1792** | 0 |
| `Y_intrabc_10b.obu` | 0 | 0 | 3712 | **3584** | 896 |

Both target sites and both target arms are live instruments. Site 2's own
report row (2700 hits at 4:2:0) and site 8's (96 at 4:4:4) are consistent with
`gm_small_side_witness` and `444_intra_in_inter_128root_mu_chroma` respectively;
`422_sb128_3f` reproduces the report's site-5 control (`side=128 ss=(1,0)
nleaves=13`, its single `guard=singleref` line).

One process note, because it nearly produced a fake zero: my first probe edit
went to the **primary checkout**, not the worktree, so the first decode pass
reported `site2=0 site8=0` for all 12 streams — from a binary that contained no
site probe at all. Caught by `strings` on the artifact, primary reverted
(`git checkout -- decode.rs`), probes re-applied inside the worktree, and the
whole decode pass redone. Every number below is from the second, verified pass.

## The 12 recipes

Source is `C422` y4m from ffmpeg (`-pix_fmt yuv422p -f yuv4mpegpipe`), three
variants over 384x256 so a 128x128 SB root exists in every superblock column:

| source | generator |
|---|---|
| `src_384x256_8f.y4m` | `testsrc2=size=384x256:rate=1:duration=8` |
| `src_384x256_noisy_8f.y4m` | `testsrc2=...,noise=alls=24:allf=t` — var-tx pressure |
| `src_384x256_120f.y4m` | `testsrc2=size=384x256:rate=30:duration=4` — real motion |

Every encode: `aomenc --codec=av1 --profile=2 --input-chroma-subsampling-x=1
--input-chroma-subsampling-y=0 --obu`, each under `timeout 180`. All 12 encoded
`rc=0`, none timed out. Driver: `run422.sh` (kept out of the commit with `att/`).

| # | name | recipe beyond the base | out B | site2 | site8 | guard=compound | guard=intra_in_inter |
|---|---|---|---|---|---|---|---|
| 1 | `a1_compound_altref` | `--limit=6 --lag-in-frames=16 --auto-alt-ref=1 --cq-level=18 --cpu-used=0 --sb-size=128` | 16855 | 0 | 0 | 0 | 0 |
| 2 | `a2_compound_gm` | + `--enable-global-motion=1 --enable-warped-motion=1 --lag-in-frames=25` | 16855 | 0 | 0 | 0 | 0 |
| 3 | `a3_compound_masked` | + `--enable-masked-comp=1 --enable-dist-wtd-comp=1 --enable-onesided-comp=1` | 16855 | 0 | 0 | 0 | 0 |
| 4 | `a4_compound_noisy` | gm+warp+masked over the **noisy** source | 327113 | 0 | 0 | 0 | 0 |
| 5 | `a5_compound_slowest` | `--passes=2 --cpu-used=0` + gm/warp/masked/dist-wtd/1to4/rect/ab | 232769 | 0 | 0 | 0 | 0 |
| 6 | `a6_compound_highq` | `--cq-level=0` + gm/warp/masked/dist-wtd/onesided/rect-tx/tx64 | 1209632 | 0 | 0 | 0 | 0 |
| 7 | `b1_iis` | `--enable-interintra-comp=1 --enable-interintra-wedge=1 --enable-smooth-interintra=1` | 16855 | 0 | 0 | 0 | 0 |
| 8 | `b2_iis_noisy` | the same iis trio over the **noisy** source, lag 25 | 327113 | 0 | 0 | 0 | 0 |
| 9 | `b3_iis_both` | iis trio + masked + gm + warp, `--cq-level=10` | 433607 | 0 | 0 | 0 | 0 |
| 10 | `b4_iis_highq` | iis trio, `--cq-level=0`, `--cpu-used=0` | 898199 | 0 | 0 | 0 | 0 |
| 11 | `b5_iis_compound_iis` | iis trio + masked + gm + warp + rect-tx + 1to4, cq 10 | 577010 | 0 | 0 | 0 | 0 |
| 12 | `b6_iis_realtime_but_slow` | iis trio, `--cpu-used=2` | 22383 | 0 | 0 | 0 | 0 |

All 12 decoded clean: `OK: N frames decoded, 384x256`, zero refusal/panic hits,
every one genuinely 4:2:2 (`cfl_ac: 422=<non-zero>`, `420sq=0`).

sha256 (first 16 hex): `acc04d638f01ec4a` (1,2,3,7 — byte-identical, see
below), `d5b448a7dca0d246` (4,8), `87baa3d815ab2528` (5), `af19d053ef9c66bd`
(6), `3490b9ab9a42c9af` (9), `14b847d6ba6d37ec` (10), `226758998d8fda08` (11),
`723f55daf5a24487` (12).

## Why the zero happened — measured, not guessed

The guard probes give the cause, and it is not what the recipes were aiming at.

**No 4:2:2 stream I produced ever coded a 128x128 inter block at all.**
`decode_probe`'s `part128` census reads `none=0` on every one of the 12, while
the committed 4:2:2 and 4:4:4 controls that DO reach these walks read:

```
12 attempts (all):            part128: split=54 none=0
422_sb128_3f.obu:             part128: split=1  none=2      <- reaches site 5
444_intra_in_inter_128...obu: part128: split=3  none=3      <- reaches site 8
```

`part128: none=0` means aomenc always chose to **split** its 128 superblocks, so
no 128-root inter block exists and `side > 64` is never true in the compound or
intra-in-inter arm. The site is not unreachable; the *encoder* never emitted the
shape.

Second, subtler: recipes 1, 2, 3 and 7 produced **byte-identical streams**
(`acc04d638f01ec4a`, 16855 B). `--enable-global-motion=1`,
`--enable-warped-motion=1`, `--enable-masked-comp=1`,
`--enable-dist-wtd-comp=1`, `--enable-onesided-comp=1` and the whole iis trio
changed nothing on the `rate=1` source — that source is 8 near-identical
stills, so every inter frame is a trivial skip and the mode search has no
reason to pick anything. Recipe 12 (`--cpu-used=2`) is likewise a different,
near-degenerate stream. So of 12 attempts, **8 distinct bitstreams** were
actually exercised; four were duplicates that bought no coverage. That is on my
recipe design, and it is the clearest thing to fix with another attempt budget.

## Disposition

**`12 attempts, 0 entries`.** No witness, no pin, no gate, `decode.rs`
untouched.

What a next lane with budget should change, since these are the two measured
blockers and neither is a decode-side question:

1. **Force a 128 inter ROOT, not a split.** `part128: none=0` everywhere.
   The committed controls that reach these walks are small frames
   (`128x128`, `128x256`) where a 128 SB is the whole frame. A 384x256 frame
   gives the encoder room to split, and it always does. Try 128x128 and
   128x256 sources, or content flat enough at cq-level 0 that `NONE` wins the
   RD comparison.
2. **Do not reuse the `rate=1` source for the mode flags.** Four of the twelve
   were byte-identical. A source with real frame-to-frame motion is a
   precondition for compound and interintra to be selected at all; the
   `rate=30` variant is the one to keep, and `src_384x256_120f` needs the
   higher `--lag-in-frames` recipes applied to it directly rather than a
   cq-level/rate-1 variant.

Both are encoder-recipe facts. Nothing here says the walks are wrong, and per
the standing rule no fix is warranted for an unwitnessed case.

## Reproduction

```text
git worktree add ~/.cache/wt/av1muchunk422 -b lane/av1muchunk422 6e58730b
cd ~/.cache/wt/av1muchunk422
# temporary EC_MUCHUNK422 probes at decode.rs 43767/43946/45580/46563/46746
CARGO_TARGET_DIR=~/.cache/muchunk422-target cargo build -p ec-av1 --example decode_probe
strings ~/.cache/muchunk422-target/debug/examples/decode_probe | grep 'MUCHUNK422'   # 5 probes
bash run422.sh            # 12 encodes, timeout 180 each, then decode_probe per stream
```

The probes are reverted in this commit, so a fresh checkout reproduces the
encodes but not the counters; re-add the five `eprintln` sites before rerunning
the decode pass. `aomenc` was `~/.cache/aom-oracle/build/aomenc`; ffmpeg is
8.1.3 and, notably, its y4m muxer is spelled `yuv4mpegpipe` (`-f y4m` fails
with "Requested output format 'y4m' is not known").
