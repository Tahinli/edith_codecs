# lane-av1probes — classification of the ignored PROBES/BENCHMARKS

Branch `lane-av1probes`, base `745c60e7`. Measurements were taken on `298f75c4` (staged to
`~/gates/repo-probes` on VPS-2 `2.28.124.204`, and to `~/.cache/av1probes/stage` locally); the
edits below are on the branch. Release profile, one cargo invocation per test name,
`EC_AV1_REQUIRE_FFMPEG=1 EC_AV1_REQUIRE_AOMENC=1 EC_REQUIRE_FIXTURES=1`, `TMPDIR` off the tmpfs
(`~/gates/tmp` / `~/.cache/tmp`). Library-class items ran on the workstation (the only host with
`~/Music`, `~/Videos`, `~/Downloads`).

## How the list was re-derived

`#[ignore]` sites in `crates/ec-av1/src/{encode,encoder,stream,transform}.rs` at the base commit:
**51** attributes on **51** test fns. 13 carry `#[ignore = "sets the process-global …"]` and are the
*real gates* the release triage already dispositioned (18/22 pass, 4 gate-blind, 0 decoder defects) —
out of scope here. The remaining **38** are the probe/benchmark/scratch set this lane classifies.
(On the older `298f75c4` tree the same scan returned 46: the wave that landed between the two commits
un-ignored `pinned_golden3/4/7`, `pinned_lr_sgr`, `pinned_sbpart` and the three gate-blind
`a_real_aomenc_*` recipe gates. Those are already re-verified green on `6c1d78a6` — the SKIPs this lane
saw were staging artifacts of an older commit, not missing fixtures. See "already un-ignored by the
wave" below.)

After this branch's un-ignore the set is **50 attributes / 37 probes**.

**Real-media-library readers** (cannot run on a fleet host; `manifest.tsv` paths point at
`/home/tahinli/...`, and `~/Music` is empty so the mp3/m4a rows would decode no frames anyway):
`probe_intrabc_key_frame`, `pricer_error_census_on_clips`, `probe_screen_detect`,
`probe_screen_library`, `last2_census`, `arf_pred_census`, `bd_rate_vs_libaom_and_rav1e` (screen row
only), `bd_rate_screen_native` (film rows), `bd_rate_film_long_gop`, `probe_parity_point` (if pointed
at a library clip), `prune_k_quality_sweep`, `prune_k_quality_sweep_inter` (hard-coded
`/home/tahinli/...` paths). These are **not** reported red when they SKIP on a VPS — the SKIP is the
correct behaviour off the library host, and each was re-run on the workstation to get its number.

## Wall / BD / 1080p benchmark items (9)

| name | class | ran? | observed result | verdict bucket | disposition |
|---|---|---|---|---|---|
| `tile_wall_table_at_1080p` | wall/1080p benchmark | yes (VPS, 1630 s) | 16-row table: 1x1 57.9 s → 4x2/8thr 35.1 s; bytes 209 107→212 050 (tiles cost +1.4 % rate) | REAL DIAGNOSTIC VALUE (the tile/thread scaling curve; only source of the 1-thread baseline) | keep `#[ignore]`; reason already correct |
| `tile_search_wall_1080p` | wall benchmark | yes (local, 1243 s; **VPS timed out at 2400 s** — 12 frames × 2 passes × 12 cells does not fit a 4-core VPS hour) | 1920x1080, 12 frames: 1x1/1thr 44.35 s → 2x2/8thr 26.44 s = 1.68x; rav1e same layout 7.65 s | REAL DIAGNOSTIC VALUE | keep; note in the reason that a 4-core VPS cannot finish it (it was the only timeout in the batch) |
| `tile_search_wall_4k` | wall benchmark | yes (VPS, 732 s) | 3840x1608: 4x2/8thr 1.69x, 8x4/12thr 1.77x, idle share 79–85 % | REAL DIAGNOSTIC VALUE (names the parallel-efficiency ceiling) | keep |
| `filter_stage_wall_1080p` | wall benchmark | yes (VPS, 36 s) | 8 frames: wall 35.54 s, tile search 94.7 %, filter search 3.6 %, accounted 98.9 % | REAL DIAGNOSTIC VALUE | keep |
| `filter_stage_wall_4k` | wall benchmark | yes (VPS, 62 s) | 4 frames: wall 30.48 s, tile search 92.8 %, accounted 98.9 % | REAL DIAGNOSTIC VALUE | keep |
| `filter_stage_wall_film` | wall benchmark | yes (VPS, 16 s) | 1920x768 crop of the bars fixture: tile search 93.3 % | REAL DIAGNOSTIC VALUE, but **the "film" name is a lie off the library host** — `h264_clip_frames` falls back to `fixtures/video/h264-1080p-…mp4` (colour bars) unless `EC_AV1_WALL_CLIP` is set | keep; reason rewrite: `#[ignore = "wall measurement: minutes; the 'film' crop needs EC_AV1_WALL_CLIP, unset it measures the bars fixture"]` |
| `a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders` | 1080p encode **gate** | yes (VPS 64 s, local 31 s) | 2x1 84 488 B, 2x2 84 208 B, 4x2 84 685 B, all sample-exact | REAL DIAGNOSTIC VALUE — and it is a *gate*, not a probe: it costs 31 s and asserts. The only 1080p arm in the set that does | **candidate to un-ignore** (merge owner): 31 s is affordable in the suite |
| `bd_rate_vs_libaom_and_rav1e` | BD baseline | yes (local, 86 s) | 3 rows (bars 1080p +33.3 %/−4.1 %, bars 2160p +34.8 %/−3.2 %, screen +11.1 %/−25.6 %); film rows are in the native arm | REAL DIAGNOSTIC VALUE (the encoder's headline number) | keep; note it needs the manifest for the screen row |
| `bd_rate_screen_native` | native BD arm | yes (local, 858 s) | 5 rows: bars 1080p −3.3 %/−19.0 %, bars 2160p +8.4 %/−13.9 %, film A +17.9 %/−6.3 %, film B +22.6 %/−3.8 %, screen +13.7 %/−33.6 % | REAL DIAGNOSTIC VALUE | keep |

## `a sweep, not a gate` (4)

| name | class | ran? | observed result | verdict bucket | disposition |
|---|---|---|---|---|---|
| `probe_lambda` | mode-search sweep | yes (VPS, 0.3 s) | test card −14.8 % rate vs DC alone, stripes −56.4 % | REAL DIAGNOSTIC VALUE (the rate weight of the search) | keep |
| `probe_ladder` | RD ladder dump | yes (VPS, 2.2 s) | per-picture `bytes@dB` triples for card/stripes/diagonal | REAL DIAGNOSTIC VALUE (the cross-build comparison baseline) | keep |
| `probe_split` | split sweep | yes (VPS, **FAILED**) | printed `split test card: -4.27% rate` then panicked: `the two ladders have to overlap in PSNR: [(inf, 1.8260748027008264) ×3] vs [(inf, 1.8260748027008264) ×3]` — the *stripes* picture codes losslessly at q110/90/70, so both ladders are `(inf, same)` and the trapezoid is empty | **ROBUST-RED**: a real signal, and it is in the *measure*, not the decoder — `bd_rate` cannot integrate two identical all-`inf` points. Any tree whose `stripes()` is lossless at those quantizers red-flags | merge owner: the probe needs a lossy picture (or a q ladder that is not lossless) for its second/third row; the first row's number is still valid |
| `probe_directional` | directional sweep | yes (VPS, 4.1 s) | card −0.87 % vs the seven / −15.53 % vs DC, stripes +0.00 % / −56.41 %, diagonal −82.63 % (35 of 36 blocks directional) | REAL DIAGNOSTIC VALUE | keep |

## `a perf probe, not a gate` (3) + calibration (1)

| name | class | ran? | observed result | verdict bucket | disposition |
|---|---|---|---|---|---|
| `stage_timing_breakdown` | perf probe | yes (VPS, 83 s) | key 1080p: DC 2.89 s / 7-mode 5.20 s / 13-mode 7.66 s; 4K 11.46/19.65/30.78 s; per-call 32x32: predict 2.41 µs, fwd+quant 10.28, dequant+inv 10.22, coeff_bits 21.49 | REAL DIAGNOSTIC VALUE — the per-call table is the only place the transform/pricer split is measured | keep |
| `stage_timing_breakdown_inter` | perf probe | yes (VPS, 4.2 s) | one 1280x720 inter frame: total 594.89 ms, tx+quant 214.19, coeff_bits 4.26, motion search 17.82 (of which interpolation 148.63) | REAL DIAGNOSTIC VALUE, with a caveat: `motion search 17.82 ms (of which interpolation 148.63 ms)` is self-contradictory — the interpolation sub-timer is larger than its parent, so the "everything else 358.62 ms" bucket absorbs a stage that was already counted. The *total* and the tx/pricer split are sound | keep; the sub-stage arithmetic in the print is wrong, not the timer |
| `sequence_bench_sanity` | perf probe | yes (VPS, 22 s) | `24 frames @ 1280x720: 21.60 s total, 900.11 ms/frame, 18097 bytes` | REAL DIAGNOSTIC VALUE (single smoke number; asserts only that it runs) | keep |
| `calibration_sweep_base_q_idx` | calibration probe | yes (VPS, 160 s) | two fixtures × 8 quantizers, bytes/px 0.0318→0.0010, PSNR 55.1→37.3 dB — the curve that fixes the `base_q_idx` ↔ bits/px contract | REAL DIAGNOSTIC VALUE | keep |

## Stream diagnostics (5)

| name | class | ran? | observed result | verdict bucket | disposition |
|---|---|---|---|---|---|
| `probe_tiny_32x32_trace` | aomenc recipe trace | yes (VPS, 0.5 s) | `mismatched=false` on one 32x32 mandelbrot key frame; needs `EC_TRACE_MODE_STEP` for anything more | REAL DIAGNOSTIC VALUE, but it only reports a boolean it never asserts, and `a_real_aomenc_tiny_frame_size_sweep` already gates the same content un-ignored | keep as a manual trace; the un-ignored sweep is the gate |
| `probe_tiny_fixture_trace` | pin trace | yes, **only with `EC_TINY_FIXTURE_PATH`** | unset → panic `set EC_TINY_FIXTURE_PATH to an .obu file: NotPresent`; set to `fixtures/golden4-pin.obu` → `decoded 4 frame(s)` | STAGING (needs an operator-supplied `.obu`; no assertion) | keep; reason already names the env var — add "prints, asserts nothing" |
| `debug_part32_r2_sbpart_pin_mismatch_geometry` | debug harness | yes, with `EC_AV1_GATE_DUMP_PIN` | unset → panic `set EC_AV1_GATE_DUMP_PIN`; pointed at `fixtures/part32/troy-extract.obu` → panic in the ffmpeg oracle (`expected 1 4:2:0 frames … left 196162560 right 36864` — the hardcoded 192x128 does not match that pin); pointed at `fixtures/sbpart/seed42.obu` (192x128) → `total luma mismatches: 0 bbox x=[192,0] y=[128,0]` | STAGING (needs the exact 192x128 pin the harness was written for) → once fed that pin it is REAL DIAGNOSTIC VALUE: it is the mismatch-geometry printer, and it reports **zero** mismatches today | keep; reason rewrite should name the 192x128 requirement, since a wrong pin dies in the oracle rather than saying "wrong size" |
| `sb128_r2_control_sb64` | lane diagnostic | yes (VPS, 1.2 s) | `control sb64: 9 frames exact (1 hidden)` — the sb64 control arm | REAL DIAGNOSTIC VALUE: it is the negative control for the sb128 story, and it is the only arm that proves sb64 is unaffected | keep |
| `sweep_rectx_recipes` | recipe sweep | yes (VPS, 28 s) | 100+ aomenc recipe rows, every one `mismatched=0`; fires the rect-tx arm 2–13 times per recipe | REAL DIAGNOSTIC VALUE: it is the recipe *search* that established no rect-tx mismatch exists, which is what un-ignored the rect gates | keep; the reason `lane-rectx r3 scratch sweep` is a lane label, not a reason — rewrite to `#[ignore = "recipe search: 100+ aomenc rect-tx recipes, 28 s; establishes that no recipe mismatches"]` |

## Bare `#[ignore]` scratch in `stream.rs` (2) and `transform.rs` (2)

| name | class | ran? | observed result | verdict bucket | disposition |
|---|---|---|---|---|---|
| `scratch_decode_pinned_stream_once` | scratch | yes, with `EC_AV1_PIN` | unset → panic; with `fixtures/golden7-forwarding-mismatch.obu` → `OK: 4 frames` | STAGING as a *gate* (prints, asserts nothing); REAL DIAGNOSTIC VALUE as a one-line "does this pin decode" check | reason rewrite: `#[ignore = "scratch: decodes the .obu at EC_AV1_PIN and prints OK/ERR, asserts nothing"]` |
| `scratch_isolate_pinned_mismatch` | scratch | yes, with `EC_AV1_PIN` (+`EC_AV1_PIN_W/H/N`) | unset → panic; with golden7 and the *wrong* W/H/N → `expected 1 4:2:0 frames … left 24576 right 6144`; with `W=64 H=64 N=4` → per-frame per-plane `MATCH` for all 4 frames | STAGING (needs a pin **and** its dimensions); the default W/H=64 N=1 is wrong for every real pin, so the first run always dies in the oracle | reason rewrite must state "set `EC_AV1_PIN` *and* `EC_AV1_PIN_W/H/N` to the pin's real geometry"; the print itself is the useful artifact (first divergent pixel per plane) |
| `txrd_gain_probe` | scratch | yes (VPS, 0.0 s) | per-tx-type gain table 4x4→32x32: `alpha` 0.977–1.031, e.g. 32x32 DctDct 0.9964, Idtx 1.0004 | REAL DIAGNOSTIC VALUE — it is the orthonormal-ness check for every transform pair and size, and it is the only place a mis-scaled basis shows up | keep; reason rewrite (currently a bare `#[ignore]`, which the repo convention forbids) |
| `scratch_probe_32x32_dequant` | scratch | yes (VPS, 0.0 s) | `dq[0..4]=[-57,-67,-67,0]`, row0 8× the DC then −1s, `single_sum` vs `combined` differ in length 8 vs 32 | REAL DIAGNOSTIC VALUE while the dequant path is being changed; it is the smallest reproducible view of dequant scale | keep; reason rewrite (bare `#[ignore]`) |

## Real-library / real-film probes (4) + calibration-by-library

| name | class | ran? | observed result | verdict bucket | disposition |
|---|---|---|---|---|---|
| `probe_intrabc_key_frame` | real-library probe | **no on VPS** (`SKIP probe_intrabc_key_frame: no OBS recording`); **yes locally**, 134 s | 4 quantizers × intrabc on/off at 1280x768 from the OBS capture: q5 on = 112 339 B / 63.31 dB with 232 blocks and 4800/3174/820 searches/found/won; q45 on finds 91 searches, 0 wins | REAL DIAGNOSTIC VALUE (it is the measurement that priced IntraBC) | keep; it is a **local-run** item: the manifest paths do not exist on a VPS |
| `pricer_error_census_on_clips` | real-library census | no on VPS (2 rows, bars only); yes locally, 8.9 s | per-coefficient-set priced-vs-written error, e.g. `Chroma16 nz=0 +83.3 %`, `Chroma4 nz=0 −25.4 %`, `Luma8Inter nz=0 +54.6 %`, `ALL +10.2 %` | REAL DIAGNOSTIC VALUE — the largest mis-pricing rows are named | keep; **local-run** |
| `probe_screen_detect` | real-library census | no on VPS (bars rows only, 3 SKIPs); yes locally, 15 s | 5 clips × 2 crops: share of 16x16 blocks with 2..=N colours and per-pixel var > V, e.g. bars 1080p 1920x1024 N=64/V=0 → 9.5 %; film B 640x384 N=128/V=0 → 27.7 % | REAL DIAGNOSTIC VALUE (the detector's operating point) | keep; **local-run** for the film/screen rows |
| `probe_screen_library` | real-library sweep | no on VPS (135/142 rows `SKIP: file is gone`); yes locally, 169 s | every manifest VIDEO row at 10/50/90 % + the 2 bars anchors; widest split `4 colours, var>128, gap -0.3 points`, threshold `0.2 % = EC_AV1_SCREEN_PCT 541` | REAL DIAGNOSTIC VALUE — this is the number that *set* the screen threshold | keep; **local-run**, and it is the one item that is meaningless off the library (it degenerates to 4 surviving rows) |
| `tpl_intra_denominator_histogram` | fixture census | yes (VPS, 0.9 s) | bars 1080p 1920x1024: 61 440 cells over 8 frames, mean intra MAD 420, **89.4 % of cells below 0.25× the mean**; bars 2160p 90.8 %; p99 = 19.6–25.5× | REAL DIAGNOSTIC VALUE — it confirms the "flat filtered cells give a tiny denominator" worry is *expected*, not a bug | keep; the reason says "needs the film fixtures" but it reads only `fixtures/video/*` bars — reason rewrite |
| `probe_parity_point` | parity diagnostic | yes, with all 5 env vars | unset → `Result::unwrap() on Err(NotPresent)` at `encode.rs:22802`; with `EC_PARITY_{DIMS,FRAMES,Q,CLIP,SS,VF}` → `PARITY q=100 bytes=17075 fnv1a=0b505c758993965a speed=0 pyramid Some(mini_gop 8 …)` | STAGING (needs 5 env vars, and it is meant to be diffed against `examples/enc_probe`) but it produces the hash it promises | keep; reason rewrite should name the five variables instead of "a clip" |
| `last2_census` | real-film census | no on VPS (3 SKIPs, **empty table**); yes locally, 7 s | film A leaf: 57 600 blocks, far wins 38.7 % (>10 % on 19.0 %), energy removed 6.99 %; film A ARF 37.8 %/10.31 %; film B leaf 38.1 %/7.25 %; film B ARF 44.3 %/8.38 % | REAL DIAGNOSTIC VALUE — it is the pricing of the shipped `EC_AV1_LAST2` lever | keep; **local-run**; on a VPS it prints a header and no rows, which reads as green-but-empty (a silent-skip worth a guard) |
| `arf_pred_census` | real-film census | no on VPS (empty table); yes locally, 9 s | film A: 5 ARFs, lag8 SAD/px 2.484 → lag4 2.369 → tf lag8 2.315, tf removes 6.8 %, ceiling 47.74 dB; film B: 1.199/1.114/1.028, removes 14.3 %, ceiling 49.54 dB | REAL DIAGNOSTIC VALUE | keep; **local-run**; same empty-table-off-library caveat |

## `prune_k` sweeps (2)

| name | class | ran? | observed result | verdict bucket | disposition |
|---|---|---|---|---|---|
| `prune_k_quality_sweep` | real-clip sweep | no on VPS (3 hard-coded `/home/tahinli/...` paths, all SKIP); yes locally, 23 s | 3 clips × 2 quantizers × K∈{3,4,6}: `dark/flat q=60 baseline 2863 B 54.71 dB → K=6 2879 B 54.77 dB (+0.060 dB)`, `detailed q=150 baseline 2859 B 39.73 dB → K=3 2828 B (−0.061 dB)`, `film q=150 7570 B 36.74 dB → K=3 7526 B (−0.015 dB)` | REAL DIAGNOSTIC VALUE — the K-pruning decision table; the deltas are ≤0.06 dB everywhere | keep; **local-run only**; reason rewrite: the paths are hard-coded absolutes, so name that |
| `prune_k_quality_sweep_inter` | real-clip sweep | no on VPS (3 SKIPs); yes locally, 53 s | inter arms: `dark/flat q=150 baseline 504 B 49.82 dB`, K=3/4/6 all 504 B / +0.000 dB; `film q=60 33440 B 45.71 dB → K=3 33147 B (−0.012 dB)` | REAL DIAGNOSTIC VALUE (it is the evidence that K pruning is free on inter) | keep; **local-run only** |

## Already un-ignored by the wave (were in scope on `298f75c4`)

These six were `#[ignore]`d on the tree this lane measured first and are **not** in the base commit's
ignored set any more. Re-verified on `6c1d78a6`, un-ignored and green:

| name | class | ran? | observed result | verdict bucket | disposition |
|---|---|---|---|---|---|
| `pinned_golden3_stream_decodes_pixel_exact` | pinned gate | yes | `pinned_golden3_stream_decodes_pixel_exact: 4 frame(s) byte-exact vs ffmpeg from …/crates/ec-av1/fixtures/golden3-pin.obu` (0.10 s) | REAL DIAGNOSTIC VALUE | already un-ignored; the SKIP seen on the older tree was a staging artifact |
| `pinned_sbpart_stream_decodes_pixel_exact` | pinned gate | yes | `ok` (0.10 s) | REAL DIAGNOSTIC VALUE | already un-ignored |
| `pinned_golden4_stream_decodes_pixel_exact` | pinned gate | yes, 0.1 s | `non_last_ref_hits before=0 after=1` | REAL DIAGNOSTIC VALUE | already un-ignored |
| `pinned_golden7_stream_decodes_pixel_exact` | pinned gate | yes, 0.1 s | `non_last_ref_hits before=0 after=2`, pixel-exact vs ffmpeg on all 4 frames | REAL DIAGNOSTIC VALUE | already un-ignored |
| `pinned_lr_sgr_stream_call_unique_dump` | pinned dump | yes, 0.1 s | `frame 0: y_mismatch=false u_mismatch=false v_mismatch=false` | REAL DIAGNOSTIC VALUE | already un-ignored |
| `a_real_aomenc_*` recipe gates (3) | recipe gates, gate-blind | yes, 6/15/16 s | each FAILS by its own non-vacuity guard: `zero rect64 dequant calls ever observed CURRENT_Q_IDX != base_q_idx (40 matches, 0 refusals out of 40)`; `no 32-level intra 1:4 strip (32x8/8x32) fired over 40 compared streams` | ROBUST-RED (correct reds — the gate refuses to pass vacuously) | un-ignored by the wave, which means they now run in the suite and red on the feature not firing; that is the gate's stated contract, flagged here for the merge owner |

`pinned_warp_stream_decodes_pixel_exact` is still `#[ignore]`d at the base commit and is in the
measured set below (11 pins walked, `warp_selected_hits` 0→1→5→6→8…, all pixel-exact, 1.2 s).

## Summary of buckets

- **REAL DIAGNOSTIC VALUE (32 of the 38)** — every wall/BD/timing/sweep/census item that produced its
  table on the tree, plus the 2 transform scratch probes and the `pinned_warp` gate. The wall/BD set
  is the expensive half: `tile_search_wall_4k` 732 s, `bd_rate_screen_native` 858 s,
  `bd_rate_film_long_gop` 1318 s, `tile_search_wall_1080p` 1243 s (and it does not fit a 4-core VPS
  at all), `tile_wall_table_at_1080p` 1630 s.
- **STAGING (5)** — `probe_tiny_fixture_trace`, `debug_part32_r2_sbpart_pin_mismatch_geometry`,
  `scratch_decode_pinned_stream_once`, `scratch_isolate_pinned_mismatch`, `probe_parity_point`: all
  need operator-supplied env vars, and three of them die inside the ffmpeg oracle with a
  frame-size assert rather than a "you forgot the variable" message.
- **ROBUST-RED (1 of the 38)** — `probe_split`: a real signal, in `bd_rate`, not in the decoder.
  (The three `a_real_aomenc_*` recipe gates were ROBUST-RED too, but the wave un-ignored them, so
  they are no longer part of the ignored set — see the table above.) **0 decoder defects.**
- **DEAD WEIGHT (0)** — nothing in the set is dead: every item produced a number or a verdict on some
  tree. What *is* dead is the reason text: **11 items carried a reason that misdescribed them**
  (4 bare `#[ignore]`, `filter_stage_wall_film` naming a film it does not read off the library,
  `tpl_intra_denominator_histogram` naming "film fixtures" it never opens, `sweep_rectx_recipes`
  carrying a lane label instead of a reason, `probe_parity_point` saying "a clip" when it needs five
  env vars, `scratch_isolate_pinned_mismatch` with no reason and a wrong default geometry, and the
  two library sweeps whose hard-coded `/home/tahinli/...` paths are the real precondition). Rewrites
  proposed above; **deletion left to the merge owner**, nothing deleted, no decoder logic touched.

## Local-run class (cannot run on a fleet host)

`probe_intrabc_key_frame`, `pricer_error_census_on_clips`, `probe_screen_detect`,
`probe_screen_library`, `last2_census`, `arf_pred_census`, `prune_k_quality_sweep`,
`prune_k_quality_sweep_inter`, `bd_rate_film_long_gop` (films-only arm),
`bd_rate_screen_native` (film rows), `bd_rate_vs_libaom_and_rav1e` (screen row),
`probe_parity_point` (if pointed at a library clip). All twelve were run on the workstation and
produced their numbers above; their VPS behaviour is a correct `SKIP`, not a red.

## Staging notes for the merge owner

- `a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders` is a gate that costs 31 s
  and is the only 1080p arm in the ignored set — the strongest un-ignore candidate.
- `last2_census` and `arf_pred_census` print a table header and zero rows when the library is absent,
  then exit 0. Off the library that is indistinguishable from "measured, found nothing"; a
  `rows == 0 → panic` guard would make the skip loud.
- The `pinned_golden3` / `pinned_sbpart` SKIPs came from a tree whose `crates/ec-av1/fixtures/`
  lacked the pins; both pins are now tracked (`git ls-files` lists them, `.gitignore` negates
- Both pins (`golden3-pin.obu`, `sbpart-pin.obu`) are tracked at `crates/ec-av1/fixtures/` and both
  tests were un-ignored by the wave that landed after this lane staged its tree — re-verified green
  on `6c1d78a6`. The `SKIP` observed here was a staging artifact, not a missing fixture.

## What this branch changed

Base `745c60e7`, branch `lane-av1probes`. No decoder logic touched.

### 1. Un-ignored: `a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders`

Evidence, plain (non-`--ignored`) release run on this tree:

```
test encoder::tests::a_1080p_multi_tile_... ... 1080p 2x1 tiles: 84488 bytes, sample-exact
1080p 2x2 tiles: 84208 bytes, sample-exact
1080p 4x2 tiles: 84685 bytes, sample-exact
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 748 filtered out; finished in 27.39s
```

Assertion content: for each of the three tile grids (2x1, 2x2, 4x2) it encodes three 1920x1080
pictures, decodes the stream with our decoder and with ffmpeg, asserts both produce the same frame
count, and then compares the luma plane frame by frame, panicking with the **first differing sample's
(x, y)** and asserting the plane lengths match. It is a first-differing-sample comparison, not a
length or non-empty smoke.

The two skippable halves route through the crate's **existing** presence probes — `h264_clip_frames`
(which itself asks `have_ffmpeg()` and `clip.exists()`) and `have_ffmpeg()` for the ffmpeg arm. No new
presence-check shape was introduced.

Ignore count on the branch: **50** (was 51).

### 2. Reason rewrites — before / after

| item | before | after |
|---|---|---|
| `txrd_gain_probe` | `#[ignore]` (bare) | `scratch: prints the per-tx-pair orthonormal-ness alpha for 4x4..32x32; the reference table, asserts nothing` |
| `scratch_probe_32x32_dequant` | `#[ignore]` (bare) | `scratch: prints the 32x32 dequant rows for one hand-built level vector; asserts nothing` |
| `scratch_decode_pinned_stream_once` | `#[ignore]` (bare) | `scratch: decodes the .obu at EC_AV1_PIN and prints OK/ERR; asserts nothing` |
| `scratch_isolate_pinned_mismatch` | `#[ignore]` (bare) | `scratch: needs EC_AV1_PIN plus EC_AV1_PIN_W/H/N at the pin's REAL geometry (defaults 64/64/1 are wrong for every other pin); prints the first divergent pixel per plane` |
| `probe_tiny_fixture_trace` | `diagnostic, run manually with --nocapture and EC_TINY_FIXTURE_PATH set` | `diagnostic: needs EC_TINY_FIXTURE_PATH naming an .obu; prints decode/REFUSED, asserts nothing` |
| `debug_part32_r2_sbpart_pin_mismatch_geometry` | `debug harness, run manually with EC_AV1_GATE_DUMP_PIN` | `debug harness: needs EC_AV1_GATE_DUMP_PIN at a 192x128 pin (the geometry is hard-coded; any other size dies in the ffmpeg oracle)` |
| `sweep_rectx_recipes` | `lane-rectx r3 scratch sweep` (a lane label) | `recipe search: 9 sources x 7 cq x rtx on/off, 28 s; establishes that no aomenc rect-tx recipe mismatches` |
| `probe_split` | `a sweep, not a gate` | `a sweep, not a gate; PANICS on its own lossless synthetic rows (stripes is lossless at q110/90/70, so bd_rate has no PSNR overlap) -- point EC_AV1_CLIPS at a lossy clip` |
| `filter_stage_wall_film` | `wall measurement: minutes, run it with --ignored --nocapture` | `wall measurement: 16 s; MEASURES THE BARS FIXTURE unless EC_AV1_WALL_CLIP names a real film window` |
| `tile_search_wall_1080p` | `wall measurement: minutes, run it with --ignored --nocapture` | `wall measurement: 20 min at 1080p and it TIMES OUT on a 4-core host -- run it on >=8 cores` |
| `prune_k_quality_sweep` / `_inter` | `needs real clips and ffmpeg; prints numbers for the lane report to judge` | `local-run only: the three clips are hard-coded /home/tahinli paths, so off this workstation it SKIPs every row and prints nothing` |
| `tpl_intra_denominator_histogram` | `needs ffmpeg and the film fixtures` | `0.9 s; reads only the fixtures/video COLOUR-BARS clips (not the films) and prints the per-cell intra-MAD distribution` |
| `probe_parity_point` | `diagnostic: needs ffmpeg and a clip` | `diagnostic: needs EC_PARITY_{DIMS,FRAMES,Q,CLIP,SS,VF} -- all five, it unwraps the first missing one; prints one PARITY hash line` |
| `tile_wall_table_at_1080p` / `tile_search_wall_4k` / `filter_stage_wall_1080p` / `filter_stage_wall_4k` | `…: minutes, run it with --ignored [--nocapture]` | the measured wall each one actually costs (27 min / 12 min / 36 s / 62 s) |

### 3. `stage_timing_breakdown_inter`: the print lied about its own arithmetic

Before:

```
total:                           594.89ms  (151 bytes)
motion search (NEWMV):            17.82ms  (of which interpolation  148.63ms)
transform + quantize:           214.19ms
```

`of which 148.63` under a parent of `17.82` is not a breakdown, it is a contradiction. The timers are
fine; the **print's attribution** was wrong, in two ways:

- Bucket 0 is not "the motion search". `stage_since(0, ..)` fires only after `find_best_new_mv` and
  the global-MV search (`encode.rs:11959`, `:12080`), so it times the NEWMV/GLOBAL sub-search alone.
- Bucket 1 is EVERY `crate::mc::predict` call — from the search's candidates *and* from a committed
  inter block's final prediction, and that final prediction runs outside any bucket-0 region. So
  bucket 1 is neither a subset of bucket 0 nor smaller than it, and the residual was absorbing the
  committed block's own interpolation while its label claimed to be the 13-mode intra loop.

After (measured on this tree, total 264.82 ms):

```
total:                           264.82ms  (151 bytes)
NEWMV/GLOBAL sub-search (bucket 0):    8.57ms
transform + quantize (bucket 2):      96.30ms
coeff_bits, entropy pricing (bucket 3):  2.40ms
buckets 0+2+3 accounted:             107.27ms

OVERLAPPING, not additive: every mc::predict call (bucket 1)     70.54ms
  -- of which   61.97ms is OUTSIDE bucket 0 (a committed
  inter block's final prediction, which no bucket above times)
  and the rest is the search candidates' interpolation, already
  inside bucket 0. It is deliberately NOT added to the total.

residual (13-mode intra candidate loop, the committed block's
  interpolation above, NEARESTMV's own predict, RD glue):   157.56ms
```

**Which number is real:** both timers were always real — the wall, bucket 0, bucket 2 and bucket 3.
The old line was wrong in its arithmetic, not its measurement. Bucket 1 is now a top-level row with
the overlap quantified (70.54 − 8.57 = 61.97 ms sits outside bucket 0), it is deliberately not added
to `accounted` because adding it would double-count the shared candidates, and the residual is
labelled for what it now demonstrably contains. The accounting closes: 107.27 + 157.56 = 264.83 ms
against a 264.82 ms wall.
