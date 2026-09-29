# Branch census — every unmerged `lane-*` branch in `edith_codecs`

Census run: 2026-09-30, against `main@3aa16dd1` ("Merge lane-av1w3erefute: wave-3e
refutation (3 PASS by mutation) + the unexercised-matcher class closed").
92 of the 233 `lane-*` branches are unmerged (the other 141 are ancestors of `main`
and are excluded per the charter). No branch was deleted, merged, rebased or pushed;
this lane only adds this report.

`main` moved once during the run (`edffad9e` → `3aa16dd1`) and three branches were
rebased under it. See **"The census target moved under the run"** below the counts
before acting on a row.

## Method (reproduce with these commands, nothing else)

```bash
cd /home/tahinli/Documents/Code/Rust/edith_codecs

# 1. the universe + staleness
git for-each-ref --format='%(refname:short) %(committerdate:short) %(objectname:short)' \
  refs/heads | grep '^lane-'

# 2. already merged -> excluded
git merge-base --is-ancestor <branch> main && echo merged

# 3. the decisive test: patch-id against main. This repo rebases constantly, so
#    SHAs lie. Build main's patch-id set once:
git log main --no-merges -p --format='commit %H' | git patch-id --stable \
  | awk '{print $1}' | sort -u > /tmp/main_pids.txt

# 4. per branch, per commit since its merge-base
git merge-base main <branch>
git rev-list --no-merges $(git merge-base main <branch>)..<branch>
git show <commit> --format='' -p | git patch-id --stable     # 1st field
#    1st field present in /tmp/main_pids.txt  ->  the patch landed
#    absent                                   ->  it did not

# 5. secondary: does the branch's whole code diff already sit in main's tree?
git diff $(git merge-base main <branch>)..<branch> -- crates/ scripts/ tools/ .cargo/ \
  > /tmp/w.diff
git apply --reverse --check /tmp/w.diff && echo ALREADY-IN-MAIN   # landed, re-worded
git apply --check /tmp/w.diff          && echo APPLIES-CLEAN     # still landable

# 6. size + staleness for the table
git diff --stat $(git merge-base main <branch>)..<branch>
git log -1 --format=%cd --date=short <branch>
```

Two derived detectors, both needed because step 4 alone is not decisive:

- **`git apply --reverse --check` (step 5, per-commit).** A commit whose patch-id is
  NEW can still be *in* main when main later grew on top of it. Two commits landed
  this way and are marked `landed-elsewhere` with the main sha that carries the
  content: `lane-av1ibc128arm` and the last `lane-fxmerge` commit.
- **Superseded chains.** This repo accumulated multi-round branch families
  (`f9`, `ibc`, `12sc`, `ll`, `422`). Within a family every round is a strict
  ancestor of the next, proven by
  `git merge-base --is-ancestor <round-N> <round-M>`; only the family head is
  `outstanding`, the earlier rounds are `superseded`.

`ALREADY-IN-MAIN` / `APPLIES-CLEAN` at *whole-branch* scale was **not** used to
decide: the merge bases of these lanes are from 2026-09-25 and main has moved
~5 weeks of work past them, so almost every whole-branch diff reports
`CONFLICTS-WITH-MAIN` regardless of whether it landed. Per-commit patch-id is the
only sound signal at this scale.

## Disposition table

`files` = `git diff --stat $(git merge-base main B)..B | tail -1` (whole-branch delta).

### landed-elsewhere

| branch | tip | date | files | proof (command) |
|---|---|---|---|---|
| `lane-av1-leaf8oob` | `9dbeafab` | 2026-09-25 | 4 | `git show 9dbeafab --format='' -p \| git patch-id --stable` = `2e88f55b…`; same patch-id on main `b6d4b528` |
| `lane-av1ibc128arm` | `4bfe8d8e` | 2026-09-29 | 4 | patch-id NEW but `git apply --reverse --check` succeeds → content is in main, carried by `c0727d64` ("re-declare provenance of the foreign work") |
| `lane-fxmerge` | `48d168d4` | 2026-09-29 | 20 | `scripts/verify-fixture-library.sh` and `scripts/pin-gate-audit.py` exist in main and are *supersets* of the branch's (main 466/373 lines vs branch 334/274); evolved by main `116445f5`, `800509db`, `12cae635`, `2b4ecf71`, `f4e1ac6b`, `502c18ce` |
| `lane-palette-sn` | `f251440b` | 2026-08-30 | 360 | 611/614 patch-ids in main; the 3 remaining (`667f9265`, `b66e9600`, `f251440b`) are the palette CDF tables, carried by main `06136b18` "palette colour-index CDF tables + 4-site wiring" — `default_palette_y_color_index_cdf` / `default_palette_uv_color_index_cdf` are present at `crates/ec-av1/src/cdf.rs:546,606` |
| `lane-park-abl` | `06968d08` | 2026-09-28 | 5 | `git show 06968d08 … \| git patch-id --stable` = `750a6385…`; same patch-id on main `8a91ee14` |
| `lane-park-mineonly` | `3c4fbf2e` | 2026-09-28 | 4 | patch-id `5c19bac8…`; same patch-id on main `4e151813` |
| `lane-t900` | `196962d8` | 2026-09-05 | 773 | 1434/1434 branch commits have their patch-id in `/tmp/main_pids.txt`; `tools/ec-bench` is on main (`a3c7c122`) |

### superseded

Each row's tip is an ancestor of the named replacement: `git merge-base --is-ancestor <tip> <replacement>`.

| branch | tip | date | files | replaced by | command |
|---|---|---|---|---|---|
| `lane-av1-lr444` | `5c4af5b0` | 2026-09-25 | 3 | `lane-av1-f9` (the f9 stack's base; its 3 commits are `f9`'s first 3) | `git merge-base --is-ancestor 5c4af5b0 lane-av1-f9` |
| `lane-av1-f9` | `2cb380e0` | 2026-09-25 | 4 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9b` | `32ee79fe` | 2026-09-25 | 5 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9c` | `8082ad48` | 2026-09-25 | 6 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9d` | `f52c489a` | 2026-09-25 | 7 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9e` | `95f61097` | 2026-09-25 | 8 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9f` | `d34ea416` | 2026-09-25 | 9 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9g` | `5a587f39` | 2026-09-25 | 10 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9h` | `a18c6614` | 2026-09-25 | 11 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9i` | `ec4ba889` | 2026-09-25 | 12 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9j` | `aae2ad2f` | 2026-09-25 | 13 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9l` | `b8396ca8` | 2026-09-25 | 15 | `lane-av1-f9single` | `git merge-base --is-ancestor b8396ca8 lane-av1-f9single` |
| `lane-av1-f9k` | `1cf32888` | 2026-09-25 | 14 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9m` | `f737d94c` | 2026-09-25 | 16 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9n` | `c6c66d15` | 2026-09-25 | 17 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-f9gate` | `7d33c5e7` | 2026-09-25 | 19 | `lane-av1-f9single` | `… lane-av1-f9single` |
| `lane-av1-12screen` | `4d1a15d1` | 2026-09-25 | 3 | `lane-av1-12sc32` | `git merge-base --is-ancestor 4d1a15d1 lane-av1-12sc32` |
| `lane-av1-12sc8` | `b08d14e3` | 2026-09-25 | 4 | `lane-av1-12sc8d` | `git merge-base --is-ancestor b08d14e3 lane-av1-12sc8d` |
| `lane-av1-12sc8b` | `5d329760` | 2026-09-25 | 5 | `lane-av1-12sc8d` | `… lane-av1-12sc8d` |
| `lane-av1-12sc8c` | `9f748382` | 2026-09-25 | 6 | `lane-av1-12sc8d` | `… lane-av1-12sc8d` |
| `lane-av1-ibcpix` | `37a64d99` | 2026-09-25 | 1 | `lane-av1-ibcpalgate` (its commit is the chain's first) | `git merge-base --is-ancestor 37a64d99 lane-av1-ibcpalgate` |
| `lane-av1-ibcwrite` | `25f5c729` | 2026-09-25 | 3 | `lane-av1-ibcpalgate` | `… lane-av1-ibcpalgate` |
| `lane-av1-ibcskip` | `55201f65` | 2026-09-25 | 4 | `lane-av1-ibcpalgate` | `… lane-av1-ibcpalgate` |
| `lane-av1-ibcrecon` | `4049da21` | 2026-09-25 | 6 | `lane-av1-ibcpalgate` | `git merge-base --is-ancestor 4049da21 lane-av1-ibcpalgate` |
| `lane-av1-ibccfl` | `f6614d2e` | 2026-09-25 | 7 | `lane-av1-ibcpalgate` | `git merge-base --is-ancestor f6614d2e lane-av1-ibcpalgate` |
| `lane-av1-ibceob` | `28877a4d` | 2026-09-25 | 5 | `lane-av1-ibcpalgate` | code commit `c87f1442` is in the successor: `git merge-base --is-ancestor c87f1442 lane-av1-ibcpalgate`; residue is one report file, and `git diff lane-av1-ibceob lane-av1-ibcpalgate --stat -- crates/` shows the successor is a strict code superset |
| `lane-av1-ibcpal` | `de4b23bd` | 2026-09-25 | 8 | `lane-av1-ibcpalgate` | code commit `aaa7b195` is in the successor: `git merge-base --is-ancestor aaa7b195 lane-av1-ibcpalgate`; residue is the palette verdict writeup `de4b23bd` |
| `lane-av1-llstrip` | `f49390fe` | 2026-09-25 | 3 | `lane-av1-llsub8cgate` | `… lane-av1-llsub8cgate` |
| `lane-av1-llsub8c` | `970b1776` | 2026-09-25 | 4 | `lane-av1-llsub8cgate` | `… lane-av1-llsub8cgate` |
| `lane-av1-refscale` | `aad1da48` | 2026-09-25 | 8 | `lane-av1-refscaleb` | `git merge-base --is-ancestor aad1da48 lane-av1-refscaleb` |
| `lane-av1-422scratch` | `d8c2d5a3` | 2026-09-25 | 1 | `lane-av1-422tile` | `git merge-base --is-ancestor d8c2d5a3 lane-av1-422tile` |
| `lane-av1-llf1` | `85112e0a` | 2026-09-25 | 3 | `lane-av1-llf1b` | `git merge-base --is-ancestor 85112e0a lane-av1-llf1b` |
| `lane-av1txsizeaudit-r4` | `99f25b1f` | 2026-09-29 | 7 | `lane-av1txsizeaudit-r5` (`c0727d64`), **merged** | r5 rebased r4 onto a newer main, so the SHAs are unrelated — the proof is the report: main's `lanes/av1txsizeaudit.report.md` is r5's 75465-byte rewrite and `git log main --oneline -1 -- lanes/av1txsizeaudit.report.md` = `c0727d64` (branch copy is 68897 bytes) |
| `lane-chromahalvings` | `69e80133` | 2026-09-29 | 2 | `lane-chromahalvings-r5` (`4b8db898`), **merged** at `edffad9e` | r5 rebased and merged; `git log main --oneline -1 -- lanes/av1chromahalvings.report.md` = `4b8db898` (main 5190 bytes vs branch 14253), and r5's message records "item (3) already fixed by C1, item (2) confirmed still deferred" — the decisions r1's sweep left open |
| `lane-chromahalvings-r3` | `acca8e1c` | 2026-09-29 | 3 | `lane-chromahalvings-r5` (`4b8db898`), **merged** at `edffad9e` | same command; r3's `decode_intrabc_owned_rect` blocker is written up in the merged r5 report (main 5190 bytes vs branch 9713) |
| `lane-ibc444c` | `6b78f492` | 2026-09-29 | 3 | `lane-ibc444c-r4` (`fa70a68c`), **merged** at `edffad9e` | `git log main --oneline -1 -- lanes/ibc444c.report.md` = `fa70a68c` (main 34648 bytes vs branch 16575); r2's own commit retracts its "symbol-exact" claim |
| `lane-ibc444c-r3` | `17f03d90` | 2026-09-29 | 6 | `lane-ibc444c-r4` (`fa70a68c`), **merged** at `edffad9e` | same command (main 34648 bytes vs branch 28602) |

### outstanding — unlanded work a merge owner can charter

Ranked by size of genuinely-unlanded code first.

| branch | tip | date | files | what is still unlanded | files it touches |
|---|---|---|---|---|---|
| `lane-av1chromarect` | `58629693` | 2026-09-30 | 3 | **live lane, branched off `main@edffad9e` during the census** — 4:4:4 witness pin + ss-derived chroma extent at the 128-root intra-BC strip; the base is 1 commit behind today's main, so it applies nearly clean | `crates/ec-av1/src/decode.rs` (106), `stream.rs` (69), `fixtures/r512.obu` (6948 B) |
| `lane-av1pinspec` | `8a8a9717` | 2026-09-30 | 2 | **live lane, branched off `main@3b691e13`** — pins the encode's frame count above the oracle-call count across ~11 gate families (palette, rect-tx tool sweep, intrabc rect/census, restoration sweep, edge32, band/chroma-rect) and lowers the count-vacuity unpinned ceiling 43 → 2 | `crates/ec-av1/src/stream.rs` (277), `crates/ec-av1/src/gate_coverage.rs` (24) |
| `lane-av1-refscaleb` | `cff9cc04` | 2026-09-25 | 9 | vertical-axis reference MC scaling (resize) + `frame_size_with_refs` gated on `!ER && override`; the whole feature is absent from main | `crates/ec-av1/src/stream.rs` (255), 8 files, 846+/126- |
| ~~`lane-av1-f9single`~~ **WITHDRAWN 2026-09-30 — false positive, see "Corrections since the first pass" §C1** | `c7fd5afa` | 2026-09-25 | 20 | **NOT a clean claim — the fix is already on main.** `6c33f204` ("4:4:4 inter chroma units inherit their own quadrant's luma tx_type") is a twin-flow sweep: it collected `leaf_tx_types` at EVERY size and pointed **both** copies of the four-unit arm at the shared `covering_leaf_tx_type`, so the single-reference arm was fixed by the same commit the row calls "the compound twin". The row's two evidence commands are both unsound: the cited `if side > 64` at `decode.rs:42271` is a *different* guard (the 128×128 mu-chunk walk, correctly `side > 64`; the leaf push below it is unconditional), and `grep -c lane-av1f9single` tests for the lane's **tag comment**, not for the fix. `git cherry-pick --no-commit c7fd5afa` conflicts. The branch's own form is also worse: an inline 4-clause `find` closure where main has the shared helper. **Do not charter this row.** | residue only: `lanes/av1f9single.report.md` (measurements of an arm state that never existed on main; its "counter left compound-only" claim is false — the counters are in both arms). The 19 other commits are the f9 attribution stack + `stream.rs` rungs. |
| `lane-av1-llinter` | `b75a7cb5` | 2026-09-25 | 4 | 4:4:4 lossless inter chroma raster, all frames exact, plus a firing gate and the panic-name correction — the largest single decoder delta among the 09-25 lanes | `crates/ec-av1/src/decode.rs` (341), `stream.rs` (153), `fixtures/ll444_minp64_inter.obu`, `lanes/av1llinter.report.md` |
| `lane-av1-12sc8d` | `ec52c137` | 2026-09-25 | 8 | head of the 12-bit-screen family: a skipped INTRABC sub-8x8 chroma reference rerouted to the frame copy + an exclusive reroute counter; the cap-8 chroma miss is localized to the fenced `decode_leaf8` walk | `crates/ec-av1/src/decode.rs` (95), `stream.rs` (301), `refusal_inventory.rs` (12), 4 `lanes/av112sc*.report.md` |
| `lane-av1-12sc32` | `2b647f5f` | 2026-09-25 | 4 | the 12-bit screen cap-32 gate (sibling of the cap-8 chain, not in its history) | `stream.rs` (305), `refusal_inventory.rs` (12), 2 reports |
| `lane-av1-c10` | `a2806383` | 2026-09-25 | 2 | compound pipeline pinned against libaom transcriptions at 8/16 + disclosure of the byte-identical `diffwtd_mask` cast move; **applies clean to main** (`git apply --check` passes) | `crates/ec-av1/src/mc.rs` (215), `lanes/av1c10.report.md` |
| `lane-av1-ibcpalgate` | `6213ec45` | 2026-09-25 | 11 | head of the IBC family: makes the intrabc chroma palette override reach reconstruction and pins r5 behind a firing gate for the tx4-split arm | `crates/ec-av1/src/decode.rs` (123), `stream.rs` (130), `fixtures/r5.obu`, 7 reports |
| `lane-av1loss444mm` | `71ca43b1` | 2026-09-29 | 4 | 11 rounds on the 4:4:4-lossless intra-in-inter chroma walk: plane-stamped lossless `EC_DQCOEFF`, plane threaded onto `TxParams`, 925-line report | `crates/ec-av1/src/transform.rs` (38), 3 files, `lanes/av1loss444mm.report.md` |
| `lane-av1lm444loss` | `927dbf09` | 2026-09-29 | 4 | ss-aware chroma geometry + lossless `TX_4X4` raster in `decode_intrabc_owned_rect`, and the intrabc rect route's lossless 4:4:4 chroma walk gated | `crates/ec-av1/src/stream.rs` (168), 4 files, 513+/51- |
| `lane-park-av1444edge` | `a02a2511` | 2026-09-29 | 5 | the den=9 / mode=2 4:4:4 superres cells pinned, with mode 2 recorded as a per-frame cell, and the two mis-verdict rows in the format-sweep report corrected | `crates/ec-av1/src/stream.rs` (469), 2 fixtures, `lanes/av1444edge.report.md` (130), `lanes/av1formatsweep.report.md` (544) |
| `lane-av1-422tile` | `3bc8eaf3` | 2026-09-25 | 4 | removes the frame-9 `read_coeffs_rect` panic (the 4:2:2 tile route) | `crates/ec-av1/src/decode.rs` (130), `examples/decode_probe.rs`, 2 reports |
| `lane-av1-llsub8cgate` | `6be33e9d` | 2026-09-25 | 6 | head of the lossless-sub8 family: 4:4:4 lossless sub8 rect chroma units take leaf-granular extent, pinned behind a pixel-exact gate | `decode.rs` (83), `stream.rs` (108), `fixtures/ll444_min4_strip_witness.obu`, 3 reports |
| `lane-av1-llf1b` | `59482797` | 2026-09-25 | 6 | clip the intra-in-inter luma walks to `max_blocks_wide/high`; the inter half is in `llf1` | `decode.rs` (94), `stream.rs` (53), `examples/decode_probe.rs`, `fixtures/llf1_testsrc2_lossless.obu`, 2 reports |
| `lane-av1-444stamp` | `833a553e` | 2026-09-25 | 4 | the 444 inter pair-strip chroma stamp covers its own footprint | `decode.rs` (16), `stream.rs` (86), `fixtures/inter16x4_stamp_444_witness.obu`, report |
| `lane-av1-cdfc` | `e2d333a4` | 2026-09-24 | 4 | inherit the first luma unit's `tx_type` and the DV prediction override | `decode.rs` (62), `stream.rs` (56), `examples/decode_probe.rs`, report |
| `lane-av1-cfl` | `7c6dad53` | 2026-09-24 | 2 | the 4:2:0 sub8 group uv stamp no longer flattens an intra chroma block | `crates/ec-av1/src/decode.rs` (23), report |
| `lane-av1-422p` | `121db30c` | 2026-09-25 | 2 | the `chroma422_pair16` chunk's unit extent (4:2:2 probe continuation) | `crates/ec-av1/src/decode.rs` (14), report |
| `lane-av1-llband` | `01a34bc8` | 2026-09-25 | 4 | 444 lossless rect-strip chroma units step one mi per unit | `decode.rs` (45), `stream.rs` (77), `examples/decode_probe.rs`, report |
| `lane-av1-llcpred` | `3773b044` | 2026-09-25 | 2 | entropy-exact chroma-only pixel diffs (mandelbrot, minp) localised | `crates/ec-av1/src/decode.rs` (39), report |
| `lane-av1-128none` | `94091920` | 2026-09-25 | 2 | the 4:4:4 arm of the 128-none inter mu-chroma gate, ss-aware | `decode.rs` (16), `stream.rs` (148) |
| `lane-av1tilerows2` | `bf88d5e1` | 2026-09-28 | 4 | two mislabelled `EC_PART` rungs (**one sub-claim did not reproduce, see §C4** — main prints no `at16.0/at16.1` string and the branch's tip commit touches no such rung; `at16` exists in `main:decode.rs:16833` only as a *variable* name) + `set_symr_cdf("eob_pt"/"tx_type")` call sites absent from `stream.rs` (**verified**: `git grep -c set_symr_cdf main -- crates/ec-av1/src/stream.rs` = 0, while `msac.rs` has the fn) + 5 report rounds and a 909-line `lanes/av1tilerows.report.md` | `crates/ec-av1/src/stream.rs` (180), `lanes/av1tilerows.report.md` (909) |
| `lane-av1refusal` | `910dc296` | 2026-09-29 | 4 | r6/r7 of the decode-path refusal audit: the third weak-anchor finding, named | `stream.rs` (8), `lanes/av1refusalclaim.report.md` (698) |
| `lane-av1refusalfix` | `4741f0d4` | 2026-09-29 | 5 | the execution round: r3's base already closes items 1-2, so this records the remaining fixes with red/green proofs | 2 reports (386 + 287), 5 files, 1030+/13- |
| `lane-av1cmpaudit` | `3d0001bc` | 2026-09-28 | 2 | `OUR_PRED` gains `plane=`, `EC_PREDOUT8` gains `mode=`, plus rung-16 scripting in the oracle instrument | `crates/ec-av1/src/decode.rs` (36), `scripts/instrument-aom-oracle.sh` (99) |
| `lane-av1skipfix` | `b696d485` | 2026-09-29 | 3 | commits the 444 lossless key-frame pin the gate could never find, so the gate reads a COMMITTED pin | `crates/ec-av1/src/stream.rs` (86), report |
| `lane-av1oraclerungdepth` | `98d878d6` | 2026-09-29 | 2 | documents the 1-byte-per-sample depth assumption on oracle dumps; **applies clean to main** | `scripts/instrument-aom-oracle.sh` (63), report |
| ~~`lane-infra-gitdir`~~ **RECLASSIFIED 2026-09-30: `outstanding` → `landed-elsewhere`, see §C3** | `082ef153` | 2026-09-25 | 1 | the test runner ignoring a leaked `GIT_DIR` **landed** as main `71f1f09c` (same subject, same patch-id `2b7d379f…`), committer date `2026-09-30 01:30:50` — five minutes after this census's base `3aa16dd1` (`01:25:11`). The row was **correct when written** and went stale inside the run, which is the "target moved under the run" case the census already documents, but this row was not caught by it. The row's "**applies clean to main**" is now false: `git apply --check` fails (`patch does not apply`), `git apply --reverse --check` succeeds. **Nothing to charter.** | nothing — content is byte-identical in `main:.cargo/config.toml` |
| `lane-park-a1` | `24342d4d` | 2026-09-28 | 8 | parked aggregate of 4 lanes' work. Its headline fix (4:4:4 inter chroma units inherit their own quadrant's luma `tx_type`) **landed** as main `6c33f204` (identical subject, superset patch), but the residue did not: `EC_DBGCTX` rungs (`grep -c EC_DBGCTX crates/ec-av1/src/stream.rs` = 0), 2 reports and 3 fixtures | `decode.rs` (376), `stream.rs` (576), 3 `.obu`, `lanes/av1444chr.report.md`, `lanes/av1tilerows.report.md` |
| `lane-av1444chr` | `49e30db7` | 2026-09-28 | 2 | plane-tagged `OUR_MODE` rung (behaviour-free diagnostic) + 4 report rounds; `tu_reach` exonerated, the H3 unit named | `crates/ec-av1/src/decode.rs` (13), `lanes/av1444chr.report.md` (157) |
| `lane-av1-wedge12` | `94d9d753` | 2026-09-25 | 5 | the 12-bit COMPOUND_WEDGE witness (fixture + probe rung) and its review | `stream.rs` (159), `fixtures/av112bit-wedge.obu`, `examples/decode_probe.rs`, 2 reports |
| `lane-av1-16x4` | `9fd85862` | 2026-09-24 | 1 | report-only, but unlanded knowledge: the 444 16x4/inter-strip chroma wall's **next size class down** is localized (the full fixture is byte-exact) | `lanes/av116x4.stop.md` (102) |
| `lane-av1leafsize` | `571b4090` | 2026-09-28 | 1 | report-only: the 4:4:4 128-root leaf-size fork is gone | `lanes/av1tilerows.report.md` (+46 on top of main's copy) |
| `lane-av1444rect` | `0beaf6c5` | 2026-09-28 | 1 | report-only: round 6 names the fork's caller (`decode_intrabc…`) | `lanes/av1444rect.report.md` (+46/-4) |
| `lane-av1chrtx` | `6cc9ea6f` | 2026-09-29 | 1 | report-only: the read-census gate names the actual branch/worktree instead of a stale path | `lanes/av1readcensus.report.md` (+4/-3) |

### scratch — debug/probe branches, nothing to land

| branch | tip | date | files | why nothing to land (command) |
|---|---|---|---|---|
| `lane-av1testcount` | `f61ebe79` | 2026-09-29 | 13 | 10/12 commits' patch-ids in main; the 2 NEW touch only `lanes/*.report.md` (`git diff $(git merge-base main lane-av1testcount)..lane-av1testcount --name-only`) |
| `lane-av1oraclepost` | `6d167f73` | 2026-09-29 | 13 | 10/13 landed; the 3 NEW are `lanes/av1oraclepost.report.md` + `merge-wave-2.report.md` only |
| `lane-av1-leaf8tx` | `d0946460` | 2026-09-25 | 4 | pure instrumentation — the whole `crates/` delta is one `INTRABC_LEAF8_VARTX_CHROMA_ARM_HITS` counter and its `println!`; the fix it measures landed as main `b6d4b528` |
| `lane-av1-10film` | `e4c9ab4b` | 2026-09-24 | 1 | report only, and its own verdict is "DONE — premise falsified; defect is 444-lane-owned"; the 444 lane is now merged |
| `lane-av1-422c` | `c7735057` | 2026-09-25 | 1 | report only; "STOP … NO source change", the 4:2:2 header refusal stays in place |
| `lane-av1-cdefu` | `5604fc1b` | 2026-09-25 | 1 | report only; "s68 chroma residue NAMED … already closed at this HEAD" |
| `lane-av1-intra64` | `00a232a6` | 2026-09-25 | 1 | report only; "Verdict: CLOSED on fe3e8418", zero differing samples |
| `lane-av1-ll16x4` | `c3e794f2` | 2026-09-25 | 1 | report only; "already fixed on this branch … No code changed in this lane" (`4b73d07f`) |
| `lane-av1-ll444` | `01d05452` | 2026-09-25 | 1 | report only; "STOP — the function is unreachable on this tree" |
| `lane-av1-pareto` | `ec337a06` | 2026-09-25 | 1 | report only; "Disposition: accepted (nothing to fix on this head)" |
| `lane-av1-superres` | `ac7da199` | 2026-09-25 | 1 | report only; round-10 attribution of the inter-superres gate's red-on-main |
| `lane-av1-txroute` | `c042a68a` | 2026-09-25 | 1 | report only; "Verdict: REFUTED — already-exact stop at fe3e8418. No code edited." |

## Counts

| class | count |
|---|---|
| landed-elsewhere | **8** (was 7; +`lane-infra-gitdir`, §C3) |
| superseded | **37** |
| outstanding | **35** (was 36; −`lane-infra-gitdir` reclassified, §C3; −`lane-av1-f9single` **withdrawn** as a false positive, §C1) |
| scratch | **12** |
| **total unmerged `lane-*`** | **92** (unchanged — the two corrections reclassify rows, they do not add or remove branches) |

(141 further `lane-*` branches are ancestors of `main` and were excluded by
`git merge-base --is-ancestor <branch> main` before the census ran.
233 `lane-*` branches exist in total. Every unmerged branch appears in exactly one
section above — verified by replaying the Method block's steps 1-2 over
`refs/heads` and grepping the table for each name.)

### The census target moved under the run

`main` advanced `edffad9e` → `3aa16dd1` and three branches were rebased or
committed-to *while this census was running*. The table is a snapshot of
`main@3aa16dd1` with each branch's tip as of 2026-09-30; the two rows dated
`2026-09-30` (`lane-av1chromarect`, `lane-av1pinspec`) are live lanes, not
abandoned ones, and `lane-av1w3erefute` was merged as `3aa16dd1` and therefore
dropped out of the table mid-run. Re-run the Method block against a fresh `main`
before acting on any row.

## Corrections since the first pass (2026-09-30)

**The rule:** *a twin-flow sweep's subject names one arm, its diff touches both —
re-check every subject-sourced row with `git show --stat` before ranking it.*

This census ranked rows by reading commit **subjects** and spot-grepping main
for a lane's **tag comment**. Both are unsound when one commit fixes several
textually-parallel copies of the same arm: the subject names the arm the author
was thinking about, and a tag-comment grep proves only that the *marker* is
absent, never that the *fix* is. `6c33f204` is exactly that shape — it collected
`leaf_tx_types` at every size and pointed **both** copies of the four-unit 4:4:4
chroma arm at the shared `covering_leaf_tx_type`, while its subject says
"4:4:4 inter chroma units" and reads as compound-only.

### §C1 — `lane-av1-f9single`: FALSE POSITIVE, wrong at census time (highest severity)

The document's single most actionable row was wrong. It claimed the
single-reference covering-leaf fix was unlanded and that "only the single-ref
arm is missing".

```bash
# the fix is on main, and was ALREADY there when this census ran
git merge-base --is-ancestor 6c33f204 3aa16dd1     # YES  (the census base)
git show 3aa16dd1:crates/ec-av1/src/decode.rs | grep -c covering_leaf_tx_type   # 20
git show 3aa16dd1:crates/ec-av1/src/decode.rs | grep -c 'Some(cu_tx_type)'       # 2
git blame -L 42250,42262 -s 3aa16dd1 -- crates/ec-av1/src/decode.rs             # 6c33f2047

# the branch's own patch cannot be applied — it is a duplicate
git cherry-pick --no-commit c7fd5afa
#   CONFLICT (content): Merge conflict in crates/ec-av1/src/decode.rs

# and the row's two evidence commands are both unsound
git show 3aa16dd1:crates/ec-av1/src/decode.rs | sed -n '42271p'  # the 128 mu-chunk walk,
                                                                 # NOT the leaf-push guard
git grep -c lane-av1f9single main -- crates/ec-av1/src/decode.rs # 0 — but this greps a
                                                                 # TAG COMMENT, not the fix
```

`6c33f204`'s own message calls the single-reference side "the single-reference
twin" — the subject under-read its own diff. Main's form is also the better
one: the branch inlines a 4-clause `find` closure where main has the shared
`covering_leaf_tx_type` helper (`decode.rs:38785`) that the intra-128 path, the
rect sibling and both four-unit arms call. Taking the branch would have
reintroduced a fourth copy of a convention main had already deduplicated.

**Replaced by** `f4ac435c` — not a re-land of the fix (it is already there) but
the *attribution* gap the census's blindness hid: `CHROMA_QUAD_LEAF_TX_HITS` is
bumped from both arms, so the existing witness cannot say which arm it
witnessed. Measured on the pinned witness `444_quad_leaf_tx_type.obu`: **88 of
96** covering-leaf units and **all 8** of the DIFFERS units are the
single-reference arm's.

### §C2 — `c6c66d15` (f9n): the "comment-only" claim is false

Merge-owner note 2 called f9n's `decode.rs` content a *comment-only* diff. It is
not — it carries 27 lines of real code:

```bash
git show c6c66d15 --stat --format='%s'
#   lane-av1f9n: a6tools444 f11 U recon - 64x64 inter chroma units inherited ...
#   crates/ec-av1/src/decode.rs | 27 +++++++++++++--
git show c6c66d15 --format='' -p -- crates/ | grep '^+' | grep -v '^+++' \
  | sed 's/^+//' | sed 's/^[[:space:]]*//' | grep -v '^//' | grep -v '^$'
#   if side >= 64 {
#   let covering = leaf_tx_types
#   .iter().copied().find(|&(lr, lc, lw, lh, _)| { ...
```

Its *effect* is on main via `6c33f204` (same thing, via the shared helper), so
the note's bottom line — retire the 15 `f9` branches by reading one report —
still stands. Only the stated evidence changes.

### §C3 — `lane-infra-gitdir`: reclassified `outstanding` → `landed-elsewhere`

**Not** a false positive: the row was correct when written and went stale inside
the run. The fix landed as `71f1f09c` (same subject, same patch-id
`2b7d379f…`), committer date `2026-09-30 01:30:50` — **five minutes after** this
census's base `3aa16dd1` (`01:25:11`). The author date is `2026-09-25`, which is
what the table prints, so the staleness is invisible in the row.

```bash
git show 082ef153 --format='' -p | git patch-id --stable        # 2b7d379f…
git log 3aa16dd1 --no-merges -p --format='commit %H' | git patch-id --stable \
  | awk '{print $1}' | grep -c '^2b7d379f'                    # 0  -> absent at census time
git show main:.cargo/config.toml                                # byte-identical to the branch
git diff $(git merge-base main lane-infra-gitdir)..lane-infra-gitdir > /tmp/g.diff
git apply --check /tmp/g.diff            # FAILS: patch does not apply
git apply --reverse --check /tmp/g.diff  # SUCCEEDS -> already in main
```

The row's "**applies clean to main**" is now false in both directions, and note 3
listed it as a cheap win. This is the "target moved under the run" class the
census already documents — the row just was not caught by that note.

### §C4 — `lane-av1tilerows2`: one sub-claim did not reproduce

The row's `set_symr_cdf` half is **verified** (`git grep -c set_symr_cdf main --
crates/ec-av1/src/stream.rs` = 0, while `msac.rs` has the fn). Its `EC_PART`
half is not: main prints no `at16.0/at16.1` string, and the branch's tip commit
touches no such rung. `at16` occurs in `main:decode.rs:16833` only as a *local
variable* name. The row stands; that one sub-claim should not be cited as proof.

### §C5 — `lane-park-a1`: confirmed correct, and now has a mechanism

The twin-sweep detector independently re-found this row's headline commit
`d6fe0d8d` as touching **both** arms (4 hunks in the compound copy at 38704,
4 in the single-reference copy at 40982) — which is precisely why `6c33f204`
subsumes it. The row's "identical subject, superset patch" reading is right.
Its residue claim also holds: `git grep -c EC_DBGCTX main --
crates/ec-av1/src/stream.rs` = 0.

### What was re-checked, and what was not

**Re-checked — every outstanding row, mechanically.** Two detectors ran across
**all 36 outstanding branches as first listed** (35 after §C3's reclassification)
and every unlanded commit in them:

1. **Twin-sweep detector** — for each unlanded commit not landed by patch-id,
   locate the compound and single-reference copies of `decode_inter_block` *on
   that commit's own tree*, and flag any commit whose `decode.rs` hunks land in
   both. Result: 2 hits — `lane-park-a1 d6fe0d8d` (§C5, already correct) and
   `lane-av1-refscaleb aad1da48`, which is a *superseded intermediate* inside
   the refscaleb chain, not an outstanding tip. This is the filter the first
   pass lacked, and it is sound in the "false positive" direction: a row can
   only be a subject-vs-diff twin false positive if its diff lands in an arm its
   subject does not name, and every unlanded commit in every outstanding branch
   was tested.
2. **Reverse-apply scan** — `git apply --reverse --check` on each outstanding
   branch's code diff. A success is decisive in the *landed* direction (the
   content is in main). Result: 1 hit — `lane-infra-gitdir` (§C3). Run over the
   32 outstanding branches that carry a `crates/`/`scripts/`/`tools/`/`.cargo/`
   delta; the 4 report-only rows (`lane-av1-16x4`, `lane-av1leafsize`,
   `lane-av1444rect`, `lane-av1chrtx`) have no code diff to test, and the
   twin-sweep detector did cover them.

**Not re-checked, and why:** the 7 (now 8) `landed-elsewhere` rows, the 37
`superseded` rows and the 12 `scratch` rows were left as-is. Those classes are
defined by ancestry and patch-id, which are sound and were already applied
against every branch in the class — the subject-vs-diff blindness only affects
rows that *assert a fix is unlanded*, and no `superseded` or `scratch` row makes
that assertion. Their risk is staleness, not misreading, and staleness is
uniformly handled by the re-run instruction above.

Within the outstanding class, per-row **semantic** spot-checks (reading the
row's specific claim and grepping main for the actual code) were done for the
rows carrying a checkable main-content claim: `lane-av1-f9single` (§C1),
`lane-infra-gitdir` (§C3), `lane-av1tilerows2` (§C4), `lane-park-a1` (§C5),
`lane-av1-refscaleb` (claim **holds** — `git grep -n 'fn frame_size_with_refs'
main -- crates/ec-av1/src/` finds no such function, so the feature really is
absent) and the 444/chroma-tx family (`lane-av1-444stamp`, `lane-av1-cdfc`,
`lane-av1-128none`: no both-arm signature, no reverse-apply hit). The remaining
~28 outstanding rows were covered by the two detectors above but not read
one-by-one. A false positive of a *different* shape — a fix landed by a commit
that is neither a twin sweep nor byte-identical — would be caught by neither
detector and would need that read. That is the honest limit of this pass.

Reproduce the twin-sweep detector from the repo root:

```bash
git log main --no-merges -p --format='commit %H' | git patch-id --stable \
  | awk '{print $1}' | sort -u > /tmp/main_pids.txt
# per commit, locate both arm boundaries ON THAT COMMIT'S TREE (line numbers drift):
git show <commit>:crates/ec-av1/src/decode.rs \
  | grep -n 'let build = move |_y: &mut PlaneBuf'          # compound arm starts here
git show <commit>:crates/ec-av1/src/decode.rs \
  | awk -v c=<that line> 'NR>c+1500 && /let mut leaf_tx_types: Vec</ {print NR; exit}'
# then test each hunk start from:
git show <commit> --format='' -p -- crates/ec-av1/src/decode.rs | grep -o '^@@ -[0-9]*'
```

## Notes for the merge owner

1. ~~**`lane-av1-f9single` is the highest-value row.**~~ **WITHDRAWN 2026-09-30 —
   this was the document's worst row and it was wrong. See §C1.** The fix is
   already on main: `6c33f204` is a twin-flow sweep that pointed *both* copies
   of the four-unit arm at the covering lookup, not the compound arm alone. The
   row's "only the single-ref arm is missing" is false, and its two evidence
   commands are unsound (one cites a different guard, one greps for a tag
   comment rather than the fix). **Chartering this row would have merged a
   duplicate that also reintroduced an inline copy of a helper main had already
   extracted.** What replaced it is real but small: the *attribution* gap — the
   existing witness's counters are bumped from both arms, and measured on the
   pinned witness 88 of 96 covering-leaf units and **all 8** of the DIFFERS
   units are the single-reference arm's. That landed as `f4ac435c`.
2. **The `f9` stack is 15 branches and one fix — and the fix is already on main.**
   Everything from `lane-av1-lr444` to `lane-av1-f9gate` is one investigation,
   and a merge owner can retire 15 branches by reading one report. But note 2's
   original claim that `c6c66d15` (f9n) is a ***comment-only* diff** is **false**
   (§C2): it carries 27 lines of real `decode.rs` — `if side >= 64` and an inline
   4-clause `find` closure. Its *effect* is on main via `6c33f204`, which did the
   same thing through the shared `covering_leaf_tx_type` helper. So the
   bottom-line advice survives; the stated evidence does not.
3. **`APPLIES-CLEAN` is the cheap win list**: `lane-av1-c10` (233 lines of `mc.rs`),
   `lane-av1oraclerungdepth` (oracle script), `lane-av1444chr` (13 lines),
   `lane-av1-ibcwrite` (76 lines) — these apply to today's `main` without
   conflict. `lane-av1-ibcwrite` is `superseded` above only because its rounds
   sit inside the `ibc` family; its own delta is still landable.
   **`lane-infra-gitdir` is removed from this list (§C3): it landed as
   `71f1f09c` five minutes after the census's base, and its diff no longer
   applies forward** (`git apply --check` fails, `--reverse --check` succeeds).
4. **`undecidable` rows: none.** Every row carries a command. The two rows whose
   whole-branch diff conflicts with main (`lane-palette-sn`, `lane-fxmerge`) were
   resolved by per-commit patch-id plus the line-count superset check, not by
   guessing.
5. **The 09-24/09-25 families share a stale base** (`fe3e8418`). `git apply --check`
   on their full diff reports conflicts against today's main for all of them, so
   each needs a rebase, not a merge. Budget the rebase, not the review.
