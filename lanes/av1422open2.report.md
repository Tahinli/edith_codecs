# lane-av1422open2 — the two cells `av1422census2` left open are CLOSED at current main

**Result in one line: `s422_320x246` and `s422_322x246` are both BYTE-EXACT against
ffmpeg at `main` = `2be65e1b` — 0/0/0 wrong Y/U/V samples on all 16 displayed frames of
each, first divergence `none` — so there is no site to name and nothing to fix.**

Tip: `main` = `2be65e1b`. Worktree `/home/tahinli/.cache/wt/av1422open2`, branch
`lane/av1422open2`, `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422open2`.
No source edit is carried by this lane: the tree is a clean worktree of `2be65e1b`
plus this report. No push, no merge, no rustfmt.

---

## 1. The fixtures

Both are **committed**, and both are byte-identical to the census sweep artifact the
old report measured. No re-encode was needed.

| cell | committed path | bytes | sha256 |
|---|---|---|---|
| `s422_320x246` | `crates/ec-av1/fixtures/s422_320x246.obu` | 20692 | `aebd3a10d46e617b803579819f2502798076384be52e032b11b46f09eeb09a79` |
| `s422_322x246` | `crates/ec-av1/fixtures/s422_322x246.obu` | 20528 | `ee96617fe45ba963d3628f37086077a94806ef0465a74cb538a8e57d2ed900b3` |

Both hashes reproduce `sha256sum` of the census's own
`~/.cache/census422b/sweep/<name>.obu` exactly, so the old report and this one are
measuring the same bitstreams. They were committed by the 4:2:2 lift
(`8d6998d7`, "lane-av1422lift"), which copied the first six census sweep cells in and
pinned them by size + fnv1a64 + sha256.

Geometry, from the sequence header via `ffprobe` (`cmpff.probe_geometry`, never a file
size): both `ss=10` (4:2:2), 8-bit, `-pix_fmt yuv422p`, 17 decode frames / 16 displayed,
one hidden alt-ref picture (decode frame 1) in each.

## 2. The measurement at current main

Instrument: the committed `scripts/cmpff.py` + `scripts/run422-ffmpeg.py` module,
unmodified, driven by a local `/tmp` wrapper that only sets `PROBE` to this lane's
build. Probe: `cargo build -p ec-av1 --release --example decode_probe` at `2be65e1b`,
**no bypass patch** — the 4:2:2 sequence-header refusal is lifted on current main, so
both cells decode through the shipped path. Oracle: ffmpeg 8.1.3, per plane per
display frame, wrong SAMPLE = one byte at 8-bit.

| cell | verdict | Y | U | V | decode/display | hidden | first divergence |
|---|---|---|---|---|---|---|---|
| `s422_320x246` | **BYTE-EXACT** | 0 | 0 | 0 | 17 / 16 | `[1]` | none |
| `s422_322x246` | **BYTE-EXACT** | 0 | 0 | 0 | 17 / 16 | `[1]` | none |

Control in the same run: `s420_320x246` (4:2:0, same encoder recipe, same geometry
family) BYTE-EXACT 0/0/0, 17 decode / 16 displayed, hidden `[16]`. The control proves
the comparator is reading real planes and not returning a vacuous zero.

Non-vacuity census read off the same run (`census_units` from the probe):
`s422_320x246` luma=2118 u=1218 v=1218 units, coded 923/819/874;
`s422_322x246` luma=2381 u=1315 v=1315, coded 986/851/920. Both cells really do walk
chroma coefficient units; the zero is not a zero-work frame.

### 2.1 Flip control — 30 arms, 30 PASS, 0 FAIL

A 0/0/0 is only a claim once the comparator has been shown to move. One oracle sample's
low byte flipped per plane, per cell, at five display frames (0, 3, 7, 11, 15) — the
count must move by **exactly +1 in that plane** with the other two planes unchanged.
2 cells x 5 frames x 3 planes = **30 arms, 30 PASS**.

The mapping is **pinned from the unflipped run** before any flip (`map_decode_to_display`,
1 solution per cell, then held fixed). This matters: re-deriving the mapping after a flip
cannot work for a luma-plane flip, because the flipped luma matches nothing and
`cmpff` correctly raises `luma identity never covers the display side`. That error is the
comparator being honest about a broken identity, not a defect in the flip method — so
the pinning is a method requirement, and it is stated here rather than papered over.

## 3. Which commit closed them — measured, not inferred

The old report is not wrong; it is superseded. Bisected by building a probe per commit
and running the same comparator, every row on its own tree, with the local
patch-run-restore 4:2:2 bypass wherever the tree still refuses:

| commit | `s422_320x246` | `s422_322x246` | note |
|---|---|---|---|
| `1bc6f543` (parent of the fix) | DIVERGES 0/**7810**/**5531** | DIVERGES 0/**3437**/**2222** | bypass applied |
| **`459e425d`** | **0/0/0** | **0/0/0** | `lane-av1422luma` |
| `7d3bdfe9` (tailskip, other branch) | DIVERGES 0/7810/5531 | DIVERGES 0/3437/2222 | bypass applied |
| `5dbca30d`, `b873bf2b`, `d16af6c3`, `41037463` | 0/0/0 | 0/0/0 | after the fix |
| `8d6998d7` (the lift) | 0/0/0 | 0/0/0 | |
| `820eeb68`, `2be65e1b` (current main) | 0/0/0 | 0/0/0 | |

The closing commit is **`459e425d` "lane-av1422luma: 4:2:2 IntraBC 1:4 strips code their
OWN chroma"**. Its own published pre/post table names these two cells at exactly the
numbers reproduced above (`0/7810/5531`, `0/3437/2222` -> `0/0/0`), so the census's
residue and this lane's red-before are the same defect, and the fix is the one that
closes it.

Mechanism, in one sentence: an intra-BC 16x4 HORZ 1:4 strip's chroma plane block is its
OWN 8x4 at 4:2:2 (which subsamples x only), not 4:2:0's pair, and
`decode_rect4_16_intrabc` had gated that pair geometry on ss `(0,0)`, so at `(1,0)` the
chroma unit was read one mi row high and the frame desynced from there — which is why
both cells were chroma-only and why the first divergence sat at the same U(62,144) in
both.

Note the census's "shrink-only" reading (§3.1 of `av1422census2`) is the right
observation of the wrong tree and the wrong attribution of the future: it saw a region
shrink without closing and inferred a second writer in the same rectangle. There was no
second writer; the same writer had a second half.

## 4. The gate that already holds these bytes

`the_pinned_422_corpus_cells_decode_pixel_exact` (`crates/ec-av1/src/stream.rs:4308`),
added by the lift, pins both cells by size + fnv1a64 + sha256 and asserts byte-exact
planes against ffmpeg with a per-plane coefficient-unit census as its non-vacuity arm.
Run here at `2be65e1b`:

```
the_pinned_422_corpus_cells_decode_pixel_exact[s422_320x246.obu]: byte-exact vs ffmpeg (yuv422p, 16 frames); census delta units=[2118, 1218, 1218] coded=[923, 819, 874]
the_pinned_422_corpus_cells_decode_pixel_exact[s422_322x246.obu]: byte-exact vs ffmpeg (yuv422p, 16 frames); census delta units=[2381, 1315, 1315] coded=[986, 851, 920]
test result: ok. 1 passed; 0 failed; 0 ignored; 842 filtered out
```

Both cells are inside a **green** gate, so nothing here can drift back silently. No new
gate is warranted: the charter's condition ("fix only if the site is one function and a
one-fixture gate can prove it") does not arise, because there is no defect.

## 5. Housekeeping

The 4:2:2 probe bypass was applied in scratch worktrees only, and every one was
restored: `grep -rn 'TEMP-PROBE-BYPASS\|if false && seq.subsampling_x'` over the primary
checkout, this lane's worktree and all scratch trees = **0 hits**; `git status --porcelain`
in each patched tree after `git checkout --` = empty; the primary checkout's
`crates/`+`scripts/` = clean. Ten scratch worktrees removed; the two remaining `wt-*`
entries (`wt-knobshold`, `wt-av1paletteuv`) belong to other lanes and were not touched.

## 6. What is NOT measured

1. **12-bit and superres 4:2:2** are not re-measured here — out of the two named cells'
   scope, and separately gated by the lift.
2. **No whole-suite run.** Only the one named gate plus the two probe measurements.
3. **No grain claim.** `EC_AV1_FINAL_DUMP` is pre-grain and these two cells are
   un-grained sweep encodes; the lift gates grained 4:2:2 separately, through its
   post-grain in-memory comparison.
4. **The decode-order basis was not compared against instrumented aomdec**; the basis
   here is ffmpeg display order with a content-derived decode->display mapping, which
   leaves exactly one hidden picture in each cell as expected.
5. **Nothing was fixed.** The fix is `459e425d`, landed before this lane started.
