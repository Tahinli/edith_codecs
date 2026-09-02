# lane-intersub8 r5 — GREEN: the sub-8x8 inter HORZ refusal is lifted

## What changed
* **Merged `lane-cdef` 8c6065e** (`5c9cccd`, merge commit, not on main at the time).
  Its `decode_inter_block8` skip-band write IS the root cause of this lane's r4 residue.
* `crates/ec-av1/src/decode.rs` (`decode_tile`'s 8x8 partition switch) — the
  `PARTITION_HORZ` refusal and its `EC_AV1_SUB8_HORZ` escape hatch are **deleted**; HORZ and
  VERT now both fall into `decode_inter_sub8_rect2`.
* `crates/ec-av1/src/decode.rs:1841+` — new `RECT_MASKED_COMPOUND_HITS` /
  `rect_masked_compound_hits()`, incremented at the `comp_group_idx == 1` site when
  `write_w != write_h`: the non-square half of `MASKED_COMPOUND_HITS`, i.e. exactly the shape
  whose `compound_type`/`wedge_idx` CDF row r4 fixed. Square blocks cannot show that defect, so
  a gate asserting `masked_compound_hits` alone stayed vacuous for it.
* `crates/ec-av1/src/refusal_inventory.rs` — the HORZ entry removed (47 -> 46 in this tree's list).
* `crates/ec-av1/src/stream.rs` — gate
  `a_real_aomenc_inter_sequence_with_a_sub8x8_inter_split_decodes_pixel_exact` grows attempts
  16..23: `--enable-rect-partitions=1` with the geq source **transposed** (the moving sine runs
  along Y), the schedule aomenc actually picks HORZ on. The two axes are counted apart
  (`horz_fired` / `vert_fired`, each `>= 2` asserted), `rect_masked >= 1` is asserted, and every
  counter is taken only across a **pixel-compared** attempt (class counter-from-refused-stream).
  `out_of_scope_mismatch == 0` unchanged. Both bit depths, 48 attempts.
* `bb59012` — `git merge main` (aa83400). Clean; `cdf.rs` byte-identical to main's; exactly one
  `bsize_index` and one `compound_type`/`wedge_idx` read site survive, so main's inter16ab copy
  of the rect CDF-row fix and this lane's r4 copy did not duplicate.

## Root cause of the r4 residue (charter hypothesis (a) was stale, (b) never applied)
The charter asked for the 8x4 pair to write the 8x8 CDEF skip band and the internal-edge lf
grid. Measured: `decode_inter_sub8_rect2` **already** wrote both per leaf
(`fill_skip_grid_rect(lmi, w_mi, h_mi, skip)` at `decode.rs:20071`, `fill_lf_grid_rect` at
`:19987`/`:20060`), and the skip grid is per-4x4-mi with `is_skip_txfm` gathering all four cells
— two 8x4 leaves cover all four. The missing writer was the **8x8 square leaf next door**
(`decode_inter_block8`), fixed by lane-cdef. Merging it alone, with no further edit, turned all
three red arms exact.

## Gate results
Command (worktree, `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-intersub8`):
`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j3 sub8x8_inter_split -- --nocapture`

EVIDENCE: gate stdout | 48 real-aomenc attempts (2 depths x 24), each decoded and Y/U/V-compared vs ffmpeg | `fired=10 horz=5 vert=3 rect_masked=2`, `test result: ok. 1 passed; 0 failed` in 42.90s
EVIDENCE: ~/.cache/intersub8-sweep-r5.log | 56-arm sweep (cq {8,12,14,16,20,26,32} x sp {3,6,9,12} x {8,10}-bit, transposed source, rect on) re-run after the cdef merge with the HORZ path live | **zero MISMATCH arms**; every `horz8x4>0` arm is EXACT (8-bit cq14 sp3 h=4, cq20 sp3 h=4, cq32 sp9 h=3; 10-bit cq12 sp3 h=5, cq16 sp3 h=2) or stops at another lane's named refusal (8-bit cq12 sp6 h=1 -> the rect COMPOUND_WEDGE refusal, 10-bit cq8 sp12 h=2 -> non-DC chroma on an 8x8 inter leaf). r4 had 3 mismatching arms.
EVIDENCE: same log, 3 targeted arms re-run individually | `gen_t.sh` + probe under `systemd-run --scope -p MemoryMax=6G`, compared to `ffmpeg -pix_fmt yuv420p{,10le}` | 8-bit cq32 sp9, 10-bit cq12 sp3, 10-bit cq16 sp3: MISMATCH (r4) -> EXACT, with the cdef merge as the only change

`refusal_inventory` (2 tests) and `gate_coverage` (10 tests) green after the lift:
`test result: ok. 12 passed; 0 failed`.

## Full suite
Unit `intersub8-suite-r5-1788332492.service` -> `$HOME/.cache/intersub8-suite-r5.log`.
See RESULT line appended below.

r4's unit (`intersub8-suite-r4-1788331918`) was stopped as charter-ordered: **187 ok, 0 FAILED,
no `test result:` line** at stop time — recorded, not a green claim.

## Film probe (charter premise corrected)
No `census4`/`hunger4.tsv` exists on this box (`find ~/.cache ~/Documents/Code/Rust -name
'hunger4.tsv'` -> empty), so the keyframe offsets were taken from `~/.cache/kf900/census_r3.tsv`,
which samples the same 3840x1608 stream every 300 s.

EVIDENCE: ~/.cache/intersub8-tmp/hg_{300,1200,1800}.obu | `ffmpeg -ss <s> -t 0.5 -c:v copy -an -f obu` then `decode_probe` under a 6G scope | ss=300 stops at "a split intra strip whose transform unit is 32x64"; ss=1200 and ss=1800 both stop at "a split (nonzero tx_depth) transform on an intra HORZ/VERT strip in an inter frame". `sub8_inter_split`/`sub8_inter_rect` counters are 0 at all three — **the below-8x8 refusal is no longer this film's frontier at these offsets**; two intra-strip transform refusals (other lanes) are hit first.

## Residue
* accepted: `COMPOUND_WEDGE` on a rect block still refused (rect wedge codebooks unimplemented);
  the 8-bit cq12 sp6 sweep arm is the witness that a real encoder reaches it.
* deferred: the two intra-strip transform refusals that now front the film — owned by the
  intra-strip lanes — unblocked by rect/split luma coefficient tables for 32x64 and for a
  nonzero-tx_depth intra strip in an inter frame.
