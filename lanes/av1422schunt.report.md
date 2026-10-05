# lane/av1422schunt — 4:2:2 screen-content encode hunt (8 cells)

Base `629ed5bd` (main, suite green). Bounded hunt: **8 new 4:2:2 encodes, axis
screen content + intrabc + palette + var-tx, 8-bit and 10-bit, 256x144 and
384x256, cq 20/40, `--threads=1 --row-mt=0`, `--tune-content=screen
--enable-intrabc=1 --enable-palette=1`**. No decoder edit. No 9th encode.

Host `tCloud@178.105.165.182` (fedora-8gb-nbg1-3) only. Clean build from
`git archive 629ed5bd` (full workspace tarball, sha256 of tar
`fd70fe4dfc41fb9e2f313465ad30864b30972a953ae352895463c1552cd056f4`, verified
matching on host), staged at `~/.cache/schunt-src`, git-inited so pin gates see
a repo. `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-hunt422sc`. aomenc
`~/.cache/aom-oracle/build/aomenc`. Oracle: ffmpeg rawvideo at the stream's own
pix_fmt (`yuv422p` / `yuv422p10le`), per-plane per-frame byte compare against
`dump_yuv` output (depth parsed from the sequence header, 8-bit u8 /
10-bit u16-LE). Off-limits hosts untouched (`2.28.124.204`, `51.195.223.40`,
`2.28.112.x`).

## Result table

| cell | geometry | depth | cq | sha256 | size | frames | verdict |
|---|---|---|---|---|---|---|---|
| sc_a | 256x144 | 8 | 20 | `367f3fae71b182342347556bae7a0caccf71cc12dbe7e21b622d3954374440ef` | 807 | 12 | **EXACT** |
| sc_b | 256x144 | 8 | 40 | `11de2e6859a54a631e3ab5ff791e10a950237cbbf8e28a55f8b1687b13e58b1b` | 711 | 12 | **EXACT** |
| sc_c | 384x256 | 8 | 20 | `7e82e8c374f829e289e00580ab7921a3eca45d17cde45c6dc6a1a41f90110f5e` | 818 | 12 | **EXACT** |
| sc_d | 384x256 | 8 | 40 | `1605106ad3aa91f8d2ce858a7f6513a4361a976574d81b0c3149cba59b1bf836` | 728 | 12 | **EXACT** |
| sc_e | 256x144 | 10 | 20 | `092ef2e80481bde605a95d9fb0e740d673fc1ef1a0a540f035f30d0441e14058` | 3378 | 6 | **EXACT** |
| sc_f | 256x144 | 10 | 40 | `acb82b4b838a972e321ae4a5ef97f0cf4416a0724b9706c2bbd1ece42666eadf` | 2178 | 6 | **EXACT** |
| sc_g | 384x256 | 10 | 20 | `8aff9ca67025dc4e13789dbf29019cf8c217a9441d03ae54ad72c706c739d839` | 3504 | 6 | **EXACT** |
| sc_h | 384x256 | 10 | 40 | `63da73d6ca82e1423d2060f148c73bc9dc79510d51eb1a098fe318ed8f4d2dbf` | 2580 | 6 | **EXACT** |

**8/8 EXACT — no divergence found. No decoder edit made.**

aomenc argv, identical for all 8 except the noted vars (no `--profile` for
10-bit, per charter; aomenc self-selects profile 2 — warning captured):

```
aomenc --codec=av1 --obu -o <cell>.obu --passes=1 --threads=1 --row-mt=0 \
  --cpu-used=3 --end-usage=q --cq-level=<20|40> --tune-content=screen \
  --enable-intrabc=1 --enable-palette=1 --limit=<12|6> -w <w> -h <h> \
  [--bit-depth=8 --input-bit-depth=8 | --bit-depth=10 --input-bit-depth=10] <src.y4m>
```

Source: ffmpeg `smptebars` (flat synthetic graphics; chosen over testsrc2 so
none of the 8 can collide with the 51-cell census, whose recipe cells are all
testsrc2 at 320–416px — verified against `~/.cache/census422b/*.json` shas and
committed fixtures; no sha overlap). 4:2:2 y4m hand-wrapped with explicit
`C422` / `C422p10` tag (ffmpeg's muxer will not write it).

## Liveness (the zeros are claims)

1-sample flip control on `ours.f0`, mid-luma, count-then-restore:

| cell | base | flipped | restored | moves by exactly 1 |
|---|---|---|---|---|
| sc_a (8-bit) | 0 | 1 | 0 | yes |
| sc_e (10-bit, 1-LSB u16 flip) | 0 | 1 | 0 | yes |

## Engagement (what the streams really cover)

`decode_probe` counters (probe on host, same clean build):

| counter | sc_a | sc_b | sc_c | sc_d | sc_e | sc_f | sc_g | sc_h |
|---|---|---|---|---|---|---|---|---|
| chroma422_square | 11 | 11 | 42 | 46 | 24 | 126 | — | — |
| chroma422_rect | 124 | 106 | 202 | 156 | 612 | 456 | — | — |
| chroma422_sub8 | 10 | 6 | 2 | 0 | 48 | 12 | — | — |
| intrabc_hits / intrabc_vartx_trees | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| intra_in_inter_palette / palette_422_unit_window | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

(sc_g/sc_h rows omitted from the table above for space: same zeros for
intrabc/palette; square/rect/sub8 counters in the same pattern.)

EC_PROBE_HDR: `screen_tools=true` is PARSED on every frame (flag arrival, not
intent — the flag is in the header and reaches decode), but the encoder chose
`intrabc=false` per frame and no palette / var-tx leaf. So the honest cell
names are: **4:2:2 screen-tools inter coverage (8-bit and 10-bit, two
geometries, two cq points), byte-exact vs ffmpeg** — they are NOT intrabc /
palette / var-tx cells. smptebars under cq 20/40 gives the encoder nothing
worth copying or palette-izing.

## Corrections during the run (recorded, not hidden)

1. **Comparator 10-bit bug, mine**: the first comparison pass ran all 10-bit
   cells with an 8-bit frame-size constant and reported DIVERGE by size
   (`147456 != 73728`). Fixed `cmp.py` to scale per-frame bytes by
   `bytes-per-sample` (u16-LE at 10-bit) before any divergence was quoted; all
   four 10-bit cells re-compared EXACT at correct sizes. The first-divergence
   fields in the stale `chain.log` are comparator artefacts, not decoder
   defects.
2. **10-bit frames = 6, not 12, mine**: the 10-bit y4m wrapper wrote
   sample-count bytes where ffmpeg rawvideo emits 2 bytes/sample, so aomenc
   read 6 full frames then "Loss of framing in Y4M input data" (enc.err
   captured). The .obu is a valid 6-frame encode of source frames 0–5; ff.yuv
   is also 6 frames; cmp is per-frame consistent. Frame-count divergence
   (12 vs 6) is a harness defect, not a decoder one.

## Deferred

- **intrabc / palette / var-tx screen-content cells did not fire** —
  deferred(content choice): smptebars at cq 20/40 never motivates intrabc or
  palette; a next hunt should use repeated-block / screen-capture-like content
  (e.g. scrolling text or tiled UI patterns) at higher cq, or intrabc-forcing
  `--enable-intrabc=1` with strong copy opportunities, before those three axes
  can be claimed covered at 4:2:2.
- No TIMEOUT, no REFUSED. All 8 encodes under 2 min each.

## Artifacts

- Host `~/.cache/schunt/`: `sc_*/` (obu, src.y4m, ours.f*.yuv, ff.yuv,
  cmp.json, liveness.json for sc_a/sc_e, enc.err, sha.txt).
- Build: `~/.cache/cargo-target-hunt422sc` (dump_yuv + decode_probe from
  `629ed5bd` archive).
- Report worktree: `~/.cache/wt/av1422schunt`, branch `lane/av1422schunt`.
